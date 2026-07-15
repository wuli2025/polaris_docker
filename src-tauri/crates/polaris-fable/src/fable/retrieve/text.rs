//! 查询分词/切词与 FTS 表达式构造(grep 车道与融合层共用的纯文本处理)。

/// CJK(中日韩)表意文字判断 —— 这些字之间没有空格,必须自行切词。
fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{3400}'..='\u{4DBF}'   // 扩展 A
        | '\u{4E00}'..='\u{9FFF}' // 基本汉字
        | '\u{F900}'..='\u{FAFF}' // 兼容表意
        | '\u{3040}'..='\u{30FF}' // 日文假名
    )
}

/// CJK 功能词/填充词(2 字),作检索词无区分度 —— 从二元组里剔除以降噪(自然句里满是这种)。
const CJK_STOP: &[&str] = &[
    "我想", "想了", "了解", "怎么", "么做", "是怎", "做的", "一下", "知道", "什么", "这个", "那个",
    "可以", "因为", "所以", "但是", "如果", "就是", "没有", "已经", "这样", "一个", "一些", "现在",
    "时候", "出来", "起来", "相关", "资料", "的话", "进行", "通过", "对于", "以及", "或者", "还是",
    "为了", "需要", "应该", "如何", "请问", "帮我", "告诉",
];

/// 把查询切成原子:`(拉丁/数字词, CJK 连续段)`。空白与标点都作分隔符。
pub(crate) fn atoms(query: &str) -> (Vec<String>, Vec<String>) {
    let mut latin = Vec::new();
    let mut runs = Vec::new();
    let mut cur_latin = String::new();
    let mut cur_cjk = String::new();
    for ch in query.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            if !cur_cjk.is_empty() {
                runs.push(std::mem::take(&mut cur_cjk));
            }
            cur_latin.push(ch);
        } else if is_cjk(ch) {
            if !cur_latin.is_empty() {
                latin.push(std::mem::take(&mut cur_latin));
            }
            cur_cjk.push(ch);
        } else {
            if !cur_latin.is_empty() {
                latin.push(std::mem::take(&mut cur_latin));
            }
            if !cur_cjk.is_empty() {
                runs.push(std::mem::take(&mut cur_cjk));
            }
        }
    }
    if !cur_latin.is_empty() {
        latin.push(cur_latin);
    }
    if !cur_cjk.is_empty() {
        runs.push(cur_cjk);
    }
    (latin, runs)
}

/// 把查询拆成「全句 + 内容词」。**内容词 = 拉丁词(≥2)+ CJK 重叠二元组(滤功能词)+ 单字 CJK**。
/// 关键改动:CJK 自然句不再当一个大短语 —— 「我想了解模型索引」会切出 `模型`/`索引` 等概念词,
/// 这样子串算分(scan_and_score)能逐概念命中,自然句不再零召回。
pub(crate) fn split_query(query: &str) -> (String, Vec<String>) {
    let q_full = query.trim().to_lowercase();
    let (latin, runs) = atoms(&q_full);
    let mut terms: Vec<String> = Vec::new();
    for w in latin {
        if w.chars().count() >= 2 {
            terms.push(w);
        }
    }
    for run in runs {
        let chars: Vec<char> = run.chars().collect();
        if chars.len() == 1 {
            terms.push(run);
        } else {
            for w in chars.windows(2) {
                let bg: String = w.iter().collect();
                if !CJK_STOP.contains(&bg.as_str()) {
                    terms.push(bg);
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    terms.retain(|t| seen.insert(t.clone()));
    terms.truncate(40);
    (q_full, terms)
}

/// FTS5(trigram)只能命中 ≥3 个码点的项。从查询里取**可被 trigram 服务**的检索词:
/// 拉丁词(≥3)+ 每个 CJK 段(≥3 字)的重叠三元组。OR 拼接(非整句短语,故自然句也有候选)。
/// 返回 None 表示没有 ≥3 项 → 调用方靠实时子串扫描兜底。
pub(crate) fn fts_query_expr(query: &str) -> Option<String> {
    let esc = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let (latin, runs) = atoms(query);
    let mut terms: Vec<String> = Vec::new();
    for w in latin {
        if w.chars().count() >= 3 {
            terms.push(w);
        }
    }
    for run in runs {
        let chars: Vec<char> = run.chars().collect();
        if chars.len() >= 3 {
            for w in chars.windows(3) {
                terms.push(w.iter().collect());
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    terms.retain(|t| seen.insert(t.clone()));
    terms.truncate(60);
    if terms.is_empty() {
        None
    } else {
        Some(
            terms
                .iter()
                .map(|t| esc(t))
                .collect::<Vec<_>>()
                .join(" OR "),
        )
    }
}

/// 查询里是否含 trigram **无法**索引的短概念词(独立 1~2 字 CJK 段、或 2 字拉丁词)。
/// 有则补一趟实时子串扫描(覆盖 2 字中文关键词 + 未进倒排的文件);没有则纯走快的倒排路。
pub(crate) fn has_short_terms(query: &str) -> bool {
    let (latin, runs) = atoms(query);
    latin.iter().any(|w| w.chars().count() == 2)
        || runs.iter().any(|r| {
            let n = r.chars().count();
            n == 1 || n == 2
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_query_segments_cjk_and_latin() {
        let (full, terms) = split_query("  Open Hours 营业时间 ");
        assert_eq!(full, "open hours 营业时间");
        // 拉丁词原样保留
        assert!(terms.contains(&"open".to_string()));
        assert!(terms.contains(&"hours".to_string()));
        // CJK 段切成重叠二元组(概念词),而非整段一个 token
        assert!(terms.contains(&"营业".to_string()));
        assert!(terms.contains(&"时间".to_string()));
    }

    #[test]
    fn split_query_drops_cjk_stopword_bigrams() {
        // 自然句:功能词二元组(我想/想了/了解/怎么…)应被滤掉,内容词(模型/索引)保留
        let (_full, terms) = split_query("我想了解模型索引是怎么做的");
        assert!(terms.contains(&"模型".to_string()));
        assert!(terms.contains(&"索引".to_string()));
        assert!(!terms.contains(&"我想".to_string()));
        assert!(!terms.contains(&"怎么".to_string()));
    }

    #[test]
    fn fts_expr_uses_trigrams_not_whole_phrase() {
        // CJK ≥3 段 → 出三元组 OR(自然句/拼接词也有候选),不再是整句一个短语
        let expr = fts_query_expr("知识库检索").unwrap();
        assert!(expr.contains("\"知识库\""));
        assert!(expr.contains("\"识库检\""));
        assert!(expr.contains(" OR "));
        // 纯 2 字 CJK(独立概念)→ trigram 索引不了 → None(靠实时扫描兜底)
        assert!(fts_query_expr("模型").is_none());
        assert!(fts_query_expr("模型 索引").is_none());
        // 拉丁 ≥3 词原样成短语;<3 拉丁(a/bc)被丢,故标点切词后无 ≥3 项 → None
        assert_eq!(fts_query_expr("config").unwrap(), "\"config\"");
        assert!(fts_query_expr("a\"bc").is_none());
    }

    #[test]
    fn has_short_terms_detects_bare_cjk_concepts() {
        // 含独立 2 字 CJK 概念 → 需补实时扫描(trigram 服务不了)
        assert!(has_short_terms("模型"));
        assert!(has_short_terms("模型 索引"));
        // 长 CJK 段(自然句/拼接词)→ trigram 三元组够用,不必慢扫
        assert!(!has_short_terms("知识库检索系统"));
        assert!(!has_short_terms("我想了解模型索引是怎么做的"));
        // 2 字拉丁词也算短词
        assert!(has_short_terms("ab"));
        assert!(!has_short_terms("abcd"));
    }
}
