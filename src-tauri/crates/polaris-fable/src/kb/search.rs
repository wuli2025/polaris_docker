//! 加权评分搜索 kb_search + 概览 kb_overview + 增量索引 + 上下文渲染 —— 自原 kb.rs 纯移动, 逻辑零改动。

use super::*;

#[derive(Serialize)]
pub struct KbHit {
    pub path: String,
    pub title: String,
    pub snippet: String,
    pub score: f64,
}

/// 单次搜索最多读多少篇正文。10k 文档的 NAS 库上,旧版对**每篇**都
/// `fs::read_to_string` + `to_lowercase` → 主线程冻 30-60s,且是 DoS 面。
/// 先按廉价的标题/category 命中排序,只对最有希望的前 MAX_SEARCH_DOCS 篇读全文打分;
/// 正常小库(篇数 ≤ 2000)行为与旧版完全一致。
pub(crate) const MAX_SEARCH_DOCS: usize = 2000;

/// PRD §8.8 关键词加权评分: 标题 +10 / category +8 / 正文 +1
///
/// 桌面端 async + spawn_blocking:大库(2000 篇上限内仍可能逐篇读全文打分)时这一下可达
/// 数百毫秒~数秒,直接当同步命令跑在 WebView 主线程会卡界面。server flavor 无 UI 主线程、
/// dispatch 本就在 spawn_blocking 中,保持同步直调。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn kb_search(query: String, top_k: Option<usize>) -> Vec<KbHit> {
    tauri::async_runtime::spawn_blocking(move || kb_search_sync(query, top_k))
        .await
        .unwrap_or_default()
}
#[cfg(not(feature = "desktop"))]
pub fn kb_search(query: String, top_k: Option<usize>) -> Vec<KbHit> {
    kb_search_sync(query, top_k)
}

/// 同步核(server flavor 直调,desktop 由上面的 #[tauri::command] 薄包装转发)。
pub fn kb_search_sync(query: String, top_k: Option<usize>) -> Vec<KbHit> {
    // CJK 感知切词(与寓言检索同口径):中文无空格,旧版 split_whitespace 把整句当一个词,
    // 「公司的退款政策是怎样的」永远 contains 不到 → 妈妈库自然语言提问零召回。现在切成
    // 概念词(退款/政策…)+ 拉丁词,逐词 contains 算分,自然句也能命中权威 wiki。
    let (_full, owned) = crate::fable::retrieve::split_query(&query);
    if owned.is_empty() {
        return vec![];
    }
    let terms: Vec<&str> = owned.iter().map(String::as_str).collect();
    let topk = top_k.unwrap_or(8);
    let idx = INDEX.read();

    // ── Pass 1: 廉价打分(只看内存里的标题/category,零磁盘 IO)。
    // 记录每篇的廉价分,据此挑出最值得读全文的候选,避免对全库每篇都读盘。
    // (cheap_score, index)
    let mut cheap: Vec<(f64, usize)> = Vec::with_capacity(idx.len());
    for (i, d) in idx.iter().enumerate() {
        let title_lc = d.title.to_lowercase();
        let cat_lc = d.category.to_lowercase();
        let mut score = 0.0;
        for t in &terms {
            if title_lc.contains(t) {
                score += 10.0;
            }
            if !cat_lc.is_empty() && cat_lc.contains(t) {
                score += 8.0;
            }
        }
        cheap.push((score, i));
    }
    // 廉价命中的优先(标题/category 命中的排前面),其余按 INDEX 原序紧随。
    // 这样在受限读盘预算下,优先把全文 IO 花在最相关的文档上。
    cheap.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    // ── Pass 2: 只对前 MAX_SEARCH_DOCS 篇候选读全文(有界字节)打正文分。
    let mut scored: Vec<(f64, String, String, String)> = Vec::new(); // score, path, title, snippet
    let mut bodies_read = 0usize;
    for (cheap_score, i) in cheap.into_iter() {
        let d = &idx[i];
        let mut score = cheap_score;
        // 受限读盘预算:超过上限就不再读正文,仅凭廉价分参与排序
        // (廉价分为 0 的尾部文档此时已读不到正文,自然被 score<1 过滤掉)。
        let body_opt = if bodies_read < MAX_SEARCH_DOCS {
            bodies_read += 1;
            read_doc_body(&d.rel_path) // 内部已对单篇做 8 MiB 上限截读
        } else {
            None
        };
        if let Some(ref body) = body_opt {
            let body_lc = body.to_lowercase();
            for t in &terms {
                let body_count = body_lc.matches(t).count() as f64;
                score += body_count;
            }
        }
        if score < 1.0 {
            continue;
        }
        let snippet = body_opt
            .as_deref()
            .map(|b| first_snippet(b, &terms, 160))
            .unwrap_or_default();
        scored.push((score, d.rel_path.clone(), d.title.clone(), snippet));
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored
        .into_iter()
        .take(topk)
        .map(|(score, path, title, snippet)| KbHit {
            path,
            title,
            snippet,
            score,
        })
        .collect()
}

/// 知识库「家底」总览:四车道各有多少。用于对话框始终注入的概览块,
/// 让模型一开口就答得清「你的库在哪 / 有什么」,而不是只会复述 wiki 结构。
/// 全部从内存 INDEX(scan_all 已索引全库 markdown)派生,零盘点依赖、O(n) 极快。
#[derive(Serialize, Clone, Default)]
pub struct KbOverview {
    pub root: String,
    /// 妈妈库 wiki 知识页数(排除 index/_index 导航页)
    pub wiki: usize,
    /// 原始资料层 raw 的 markdown 篇数(非 md 资料只在盘点库里计数)
    pub raw_md: usize,
    /// 成品层 output 篇数
    pub output: usize,
    /// 记忆层 memory 条数(排除 index.md)
    pub memory: usize,
    /// 全部已索引 markdown 文档数
    pub total_docs: usize,
}

pub fn kb_overview() -> KbOverview {
    let idx = INDEX.read();
    let mut ov = KbOverview {
        root: kb_root(),
        ..Default::default()
    };
    for d in idx.iter() {
        ov.total_docs += 1;
        let p = d.rel_path.as_str();
        let seg = p.split('/').next().unwrap_or("");
        let fname = p.rsplit('/').next().unwrap_or("");
        let is_nav = fname == "index.md" || fname.starts_with("_index");
        match seg {
            "wiki" => {
                if !is_nav {
                    ov.wiki += 1;
                }
            }
            "raw" => ov.raw_md += 1,
            "output" => ov.output += 1,
            "memory" => {
                if fname != "index.md" {
                    ov.memory += 1;
                }
            }
            _ => {}
        }
    }
    ov
}

/// 对话命令:供前端/调试查看家底(纯读,便宜)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn kb_overview_get() -> KbOverview {
    kb_overview()
}

pub(crate) fn first_snippet(body: &str, terms: &[&str], max_len: usize) -> String {
    let lower = body.to_lowercase();
    let mut best = 0usize;
    for t in terms {
        if let Some(p) = lower.find(t) {
            best = p;
            break;
        }
    }
    let start = best.saturating_sub(40);
    let end = (start + max_len).min(body.len());
    let raw = &body[clamp_char_boundary(body, start)..clamp_char_boundary(body, end)];
    raw.replace('\n', " ").trim().to_string()
}

pub(crate) fn clamp_char_boundary(s: &str, mut idx: usize) -> usize {
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx.min(s.len())
}

// ── 增量索引辅助 ─────────────────────────

/// 把单个新文档增量加入 INDEX(同 rel_path 已存在则覆盖)。
pub(crate) fn index_add_doc(doc: KbDoc) {
    let mut idx = INDEX.write();
    if let Some(pos) = idx.iter().position(|d| d.rel_path == doc.rel_path) {
        idx[pos] = doc;
    } else {
        idx.push(doc);
    }
}

/// 从 INDEX 中移除指定 rel_path 的文档。
pub(crate) fn index_remove(rel_path: &str) {
    let mut idx = INDEX.write();
    idx.retain(|d| d.rel_path != rel_path);
}

/// 用于 chat_send: 把 search hits 渲染成 system prompt KB 块
pub fn render_kb_context(query: &str, top_k: usize) -> String {
    let hits = kb_search_sync(query.to_string(), Some(top_k));
    if hits.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n\n## 维基库召回 (KB-first)\n\n");
    out.push_str("以下文件由 Polaris 在你的本地知识库中按关键词加权评分召回,优先以此回答:\n\n");
    let root = KB_ROOT.read().clone();
    for (i, h) in hits.iter().enumerate() {
        let full = root.join(&h.path);
        let body = fs::read_to_string(&full).unwrap_or_default();
        let trimmed: String = body.chars().take(4000).collect();
        out.push_str(&format!(
            "### [{}] {}\n来源: `{}`\n\n{}\n\n---\n\n",
            i + 1,
            h.title,
            h.path,
            trimmed
        ));
    }
    out
}
