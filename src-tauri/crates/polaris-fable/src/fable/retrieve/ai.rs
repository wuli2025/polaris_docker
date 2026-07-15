//! AI 辅助检索:多查询扩写融合 —— 起 headless claude 把查询多路扩写,各变体并行召回,
//! 按 anchored-to-原查询 的文件级 RRF + 覆盖/整句加权融合。仅深度档触发,失败优雅退回混检。
//
// 实测动机(2026-07-01,_kbeval/eval_ai_expand.py,本机库 182,718 chunk / 265 题):
// 让 app 自带的 claude 把查询**多路扩写**(同义/相关说法/英文对应词),各变体并行召回,
// 真答案因「多变体一起命中」上浮 —— 候选**池 recall 0.581→0.638**(+8.4%)、关键词查询
// recall@10 0.365→0.409;融合时排序仍 anchored 到**原查询**(变体只补召回不抢主),叠加覆盖/
// 整句加权后聚合 nDCG 0.433→0.492(+13.6%)。**仅深度档触发**(扩写要起一次 headless claude,
// 数秒级);短关键词/语义查询最受益,长精确短语本就强、自动跳过不打扰。失败一律优雅退回普通混检。

use super::*;

/// 是否值得为这条查询动用 AI 扩写:长 CJK 短语(≥4 字)或长拉丁词(≥6)= 精确匹配已很强,跳过;
/// 其余(短关键词组、模糊语义)才扩写。避免「本就 top-1 命中」的查询被变体稀释 + 省一次模型调用。
pub(crate) fn worth_ai_expand(query: &str) -> bool {
    let (latin, runs) = atoms(&query.trim().to_lowercase());
    let has_long_phrase =
        runs.iter().any(|r| r.chars().count() >= 4) || latin.iter().any(|w| w.chars().count() >= 6);
    !has_long_phrase
}

/// 把 claude 扩写返回的文本解析成变体列表:优先认 JSON 数组;退而认逐行。去重、去空、截到 6 条、
/// 各 ≤120 字符;剔除与原查询同形的。纯函数,便于单测。
pub(crate) fn parse_expansions(raw: &str, original: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let orig_norm = original.trim().to_lowercase();
    let push = |s: &str, out: &mut Vec<String>| {
        let t: String = s
            .trim()
            .trim_matches('"')
            .trim()
            .chars()
            .take(120)
            .collect();
        if t.chars().count() >= 2
            && t.to_lowercase() != orig_norm
            && !out.iter().any(|x: &String| x.eq_ignore_ascii_case(&t))
        {
            out.push(t);
        }
    };
    // 尝试 JSON 数组(从第一个 [ 到最后一个 ])
    if let (Some(s), Some(e)) = (raw.find('['), raw.rfind(']')) {
        if e > s {
            if let Ok(serde_json::Value::Array(arr)) =
                serde_json::from_str::<serde_json::Value>(&raw[s..=e])
            {
                for v in arr {
                    if let Some(t) = v.as_str() {
                        push(t, &mut out);
                    }
                }
            }
        }
    }
    // JSON 没解出来 → 逐行兜底(剥列表符号/序号)
    if out.is_empty() {
        for line in raw.lines() {
            let l = line.trim().trim_start_matches(|c: char| {
                c == '-' || c == '*' || c.is_ascii_digit() || c == '.' || c == ')' || c == ' '
            });
            if !l.is_empty() {
                push(l, &mut out);
            }
        }
    }
    out.truncate(6);
    out
}

/// 多查询融合(纯函数,可单测):把「原查询结果 + 各变体结果」按**文件级 RRF**(原查询权重最高)
/// 融合,再对每个候选叠加 anchored-to-原查询 的覆盖/整句加权(用候选的 snippet 当文本),取 top_k。
/// `results[0]` 必须是原查询的结果(权重 1.0),其余是变体(权重 `var_w`)。
fn fuse_multi_query(
    original: &str,
    results: &[FableSearchResult],
    var_w: f32,
    top_k: usize,
) -> Vec<FableHit> {
    let (q_full, terms) = split_query(original);
    let (rrf_k, _, _) = rrf_params();
    let cov_w = coverage_boost_w();
    let phrase_w = phrase_boost_w();
    struct Acc {
        hit: FableHit,
        score: f32,
        doc: String,
    }
    let mut acc: HashMap<String, Acc> = HashMap::new();
    for (li, r) in results.iter().enumerate() {
        let w = if li == 0 { 1.0 } else { var_w };
        for (rank, h) in r.hits.iter().enumerate() {
            let contrib = w / (rrf_k + rank as f32);
            acc.entry(h.path.clone())
                .and_modify(|a| {
                    a.score += contrib;
                    if h.snippet.len() > a.doc.len() {
                        a.doc = h.snippet.clone();
                    }
                })
                .or_insert(Acc {
                    hit: h.clone(),
                    score: contrib,
                    doc: h.snippet.clone(),
                });
        }
    }
    // 覆盖/整句加权(anchored 到原查询;用 snippet 当文本,够区分头部排序)
    for a in acc.values_mut() {
        let doc_lower = a.doc.to_lowercase();
        a.score += coverage_phrase_boost(&doc_lower, &q_full, &terms, cov_w, phrase_w);
    }
    let mut merged: Vec<Acc> = acc.into_values().collect();
    merged.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    merged
        .into_iter()
        .take(top_k)
        .map(|mut a| {
            a.hit.score = a.score;
            a.hit
        })
        .collect()
}

/// AI 辅助检索同步实现:原查询走完整 hybrid(含重排闸)→ 若该查询值得扩写且能起 claude,
/// 取变体各跑一遍快召回(grep_vec,不重排)→ 多查询融合。任何环节失败都优雅退回原查询结果。
pub(crate) fn search_ai_sync(
    query: &str,
    top_k: usize,
    scope: Option<&str>,
) -> Result<FableSearchResult, String> {
    let started = std::time::Instant::now();
    // top_k 是外部可传的裸值:先钳到 [1,50]。否则下面 (top_k*4).clamp(top_k,50) 在 top_k>50
    // 时会 min>max 触发 clamp 断言 panic;top_k=0 也一并被抬到 1(避免 want=0 返回空结果)。
    let top_k = top_k.clamp(1, 50);
    let base = search(query, top_k, "hybrid", scope)?;
    // 不值得扩写(长精确短语)→ 直接返回原结果(标注 mode 便于前端识别)
    if !worth_ai_expand(query) {
        return Ok(FableSearchResult {
            mode: "ai(skip)".into(),
            ..base
        });
    }
    // 起 headless claude 要扩写;失败/超时一律退回 base
    let variants = ai_expand_query(query).unwrap_or_default();
    if variants.is_empty() {
        return Ok(FableSearchResult {
            mode: "ai(noexp)".into(),
            ..base
        });
    }
    // 宽召回:每路多取候选给融合(top_k*4,clamp≤50)。变体走快档 grep_vec(不重排)。
    // 原查询 + 各变体并行(thread::scope):每路 ~250ms,串行 4 路要 ~1s,并行归到最慢一路。
    // 每路内部本就是双车道双线程,这里再并一层「路」;路数 ≤4,线程开销可忽略。
    let want = (top_k * 4).min(50);
    let mut slots: Vec<Option<FableSearchResult>> = Vec::new();
    std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(variants.len() + 1);
        // slot 0 = 原查询的宽召回(非重排序,保证融合 anchored 到原查询的纯召回序)
        handles.push(s.spawn(|| search(query, want, "grep_vec", scope)));
        for v in &variants {
            handles.push(s.spawn(move || search(v, want, "grep_vec", scope)));
        }
        for h in handles {
            slots.push(h.join().ok().and_then(|r| r.ok()));
        }
    });
    let mut results: Vec<FableSearchResult> = Vec::with_capacity(slots.len());
    match slots.remove(0) {
        Some(r) => results.push(r),
        None => results.push(base.clone()),
    }
    results.extend(slots.into_iter().flatten());
    let hits = fuse_multi_query(query, &results, 0.6, top_k);
    Ok(FableSearchResult {
        query: query.to_string(),
        mode: format!("ai({} variants)", variants.len()),
        hits,
        grep_hits: base.grep_hits,
        vector_hits: base.vector_hits,
        reranked: false,
        grep_truncated: base.grep_truncated,
        ms: started.elapsed().as_millis() as u64,
    })
}

/// 调起只读 headless claude 把查询扩写成检索变体(JSON 数组)。在 kb_root 下运行;静默失败上抛。
fn ai_expand_query(query: &str) -> Result<Vec<String>, String> {
    let root = crate::kb::kb_root();
    let root_path = std::path::Path::new(&root);
    if !root_path.exists() {
        return Err("kb_root 不存在".into());
    }
    let prompt = format!(
        "你是检索查询扩写器。把下面这条搜索查询扩写成 3 个**用于全文检索**的等价说法\
        (中文同义词/相关术语/对应英文词,保留关键专名)。只输出一个 JSON 字符串数组,不要任何解释、\
        不要读文件、不要调用工具。查询:{query}"
    );
    // 墙钟超时 10 分钟:claude 卡住也能放手(上层 unwrap_or_default 退回 base),
    // 不把 server 有限的阻塞线程池永久钉死、不泄漏子进程(长任务铁律:整树回收)。
    let raw = crate::kb::run_claude_readonly_timeout(
        root_path,
        &prompt,
        |_, _| {},
        std::time::Duration::from_secs(600),
    )?;
    Ok(parse_expansions(&raw, query))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worth_ai_expand_skips_long_exact_phrases() {
        // 短关键词组 / 模糊语义 → 值得扩写
        assert!(worth_ai_expand("退款 政策"));
        assert!(worth_ai_expand("rag eval"));
        // 长 CJK 短语(≥4)精确匹配已强 → 跳过
        assert!(!worth_ai_expand("知识库混合检索"));
        // 长拉丁词(≥6)→ 跳过
        assert!(!worth_ai_expand("retrieval pipeline"));
    }

    #[test]
    fn parse_expansions_handles_json_and_lines() {
        // JSON 数组(主路):去重、剔除与原查询同形
        let v = parse_expansions(
            r#"前言 ["知识库检索","召回准确率","retrieval accuracy","知识库检索"]"#,
            "原查询",
        );
        assert_eq!(v, vec!["知识库检索", "召回准确率", "retrieval accuracy"]);
        // 逐行兜底(JSON 解不出时):剥列表符号/序号
        let v2 = parse_expansions("1. 向量重排\n- 精排序\n* embedding rerank", "x");
        assert_eq!(v2, vec!["向量重排", "精排序", "embedding rerank"]);
        // 与原查询同形被剔除
        let v3 = parse_expansions(r#"["abc","ABC","def"]"#, "abc");
        assert_eq!(v3, vec!["def"]); // "abc"/"ABC" 都判同形剔除
                                     // 截到 6 条
        let many: Vec<String> = (0..10).map(|i| format!("\"t{i}\"")).collect();
        let v4 = parse_expansions(&format!("[{}]", many.join(",")), "q");
        assert_eq!(v4.len(), 6);
    }

    #[test]
    fn fuse_multi_query_anchors_to_original_and_fuses() {
        let mk = |path: &str, snippet: &str, rank_score: f32| FableHit {
            path: path.into(),
            abspath: format!("/root/{path}"),
            location: "C1".into(),
            snippet: snippet.into(),
            score: rank_score,
            lanes: vec!["vector".into()],
            superseded_by_path: None,
        };
        // 原查询命中 a(rank0) b(rank1);变体命中 b(rank0) c(rank1) —— b 两路同中应上浮。
        let orig = FableSearchResult {
            query: "退款 政策".into(),
            mode: "grep_vec".into(),
            hits: vec![mk("a.md", "退款说明", 0.0), mk("b.md", "退款政策细则", 0.0)],
            grep_hits: 2,
            vector_hits: 2,
            reranked: false,
            grep_truncated: false,
            ms: 1,
        };
        let var = FableSearchResult {
            query: "refund policy".into(),
            mode: "grep_vec".into(),
            hits: vec![
                mk("b.md", "退款政策细则", 0.0),
                mk("c.md", "policy doc", 0.0),
            ],
            grep_hits: 2,
            vector_hits: 2,
            reranked: false,
            grep_truncated: false,
            ms: 1,
        };
        let fused = fuse_multi_query("退款 政策", &[orig, var], 0.6, 3);
        // b 被两路命中 + 含全部内容词(退款/政策)→ 覆盖加权 → 应排第一
        assert_eq!(fused[0].path, "b.md", "两路同中 + 全词覆盖应居首");
        assert_eq!(fused.len(), 3);
        // 分数严格递减(已排序)
        assert!(fused[0].score >= fused[1].score && fused[1].score >= fused[2].score);
    }
}
