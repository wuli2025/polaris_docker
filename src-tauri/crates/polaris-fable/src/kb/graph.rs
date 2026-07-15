//! 双链图谱 kb_graph + 体检 kb_lint —— 自原 kb.rs 纯移动, 逻辑零改动。

use super::*;

// ───────────────────────── Graph ─────────────────────────

#[derive(Serialize)]
pub struct KbNode {
    pub id: String,
    pub title: String,
    pub category: String,
    /// 节点类型: "doc" 文档 | "folder" 目录中枢 | "root" 知识库根
    pub kind: String,
    /// 文件中心星图:簇的「一句话画像」(AI 命名时给的温暖概括),选中卡片展示。其余场景为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Serialize)]
pub struct KbEdge {
    pub source: String,
    pub target: String,
    /// 文件中心星图:簇间**语义关系**标签(如「方法论 / 进阶 / 同源」)。普通层级/双链边为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rel: Option<String>,
}

#[derive(Serialize)]
pub struct KbGraph {
    pub nodes: Vec<KbNode>,
    pub edges: Vec<KbEdge>,
}

/// 知识库根中枢节点 id (合成节点, 不对应真实文件)
pub(crate) const ROOT_ID: &str = "__kb_root__";

/// 目录中枢节点 id 前缀。Windows/真实文件名不含冒号, 故不会与 rel_path 冲突。
pub(crate) fn folder_id(rel: &str) -> String {
    format!("dir:{rel}")
}

/// 把 Markdown 链接目标 (可能含 ./ ../) 解析回知识库内的 rel_path。
/// base_dir 为发出链接的文档所在目录 (rel)。返回规范化的正斜杠 rel_path。
pub(crate) fn resolve_rel(base_dir: Option<&Path>, link: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(b) = base_dir {
        for s in b.to_string_lossy().replace('\\', "/").split('/') {
            if !s.is_empty() {
                parts.push(s.to_string());
            }
        }
    }
    for seg in link.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other.to_string()),
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// 知识图谱: 文档节点 + 目录层级派生的中枢结构 + 双链/Markdown 链接关系边。
///
/// 散点根因 (PRD §8 设计回顾): 原实现只认 `[[wikilink]]`, 未链接的文档=孤点。
/// 现按真实目录层级 (raw/X/卷/篇) 自动生成"目录中枢节点"和树状边, 使任意
/// 知识库无需手工双链即可呈现连通图谱; 双链与 Markdown 链接作为额外关系叠加。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn kb_graph() -> KbGraph {
    use std::collections::HashSet;
    let idx = INDEX.read();

    // 标题/文件名 -> rel_path (用于 [[wikilink]] 解析)
    let mut title_to_path: HashMap<String, String> = HashMap::new();
    let mut path_set: HashSet<String> = HashSet::new();
    for d in idx.iter() {
        title_to_path.insert(d.title.to_lowercase(), d.rel_path.clone());
        let stem = Path::new(&d.rel_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        title_to_path
            .entry(stem)
            .or_insert_with(|| d.rel_path.clone());
        path_set.insert(d.rel_path.clone());
    }

    let mut nodes: Vec<KbNode> = Vec::new();
    let mut edge_set: HashSet<(String, String)> = HashSet::new();
    let mut folder_set: HashSet<String> = HashSet::new();

    // ① 文档节点。memory/ 第四车道(回声层沉淀的记忆)单独标 kind=feedback,
    //    在星河图谱里显示为玫红「回声展区」(PRD v5 §6.3④);其余皆 doc。
    for d in idx.iter() {
        let is_memory = d.rel_path.replace('\\', "/").starts_with("memory/");
        nodes.push(KbNode {
            id: d.rel_path.clone(),
            title: d.title.clone(),
            category: d.category.clone(),
            kind: if is_memory {
                "feedback".into()
            } else {
                "doc".into()
            },
            summary: None,
        });
    }

    // ② 目录层级 -> 中枢节点 + 树状边
    for d in idx.iter() {
        let segs: Vec<&str> = d.rel_path.split('/').filter(|s| !s.is_empty()).collect();
        if segs.len() < 2 {
            // 根目录下的散文件: 直接挂到知识库根
            edge_set.insert((d.rel_path.clone(), ROOT_ID.to_string()));
            continue;
        }
        // 累积每一层文件夹路径 (不含文件名)
        let mut acc = String::new();
        let mut folders: Vec<String> = Vec::new();
        for s in &segs[..segs.len() - 1] {
            if acc.is_empty() {
                acc = (*s).to_string();
            } else {
                acc = format!("{acc}/{s}");
            }
            folders.push(acc.clone());
        }
        // 文档 -> 最深一层目录
        edge_set.insert((d.rel_path.clone(), folder_id(folders.last().unwrap())));
        // 目录 -> 上级目录 逐层
        for w in folders.windows(2) {
            edge_set.insert((folder_id(&w[1]), folder_id(&w[0])));
        }
        // 顶层目录 -> 知识库根
        edge_set.insert((folder_id(&folders[0]), ROOT_ID.to_string()));
        for f in folders {
            folder_set.insert(f);
        }
    }

    // ③ 目录中枢节点
    for f in &folder_set {
        let title = f.rsplit('/').next().unwrap_or(f).to_string();
        nodes.push(KbNode {
            id: folder_id(f),
            title,
            category: String::new(),
            kind: "folder".into(),
            summary: None,
        });
    }
    // ④ 知识库根节点 (有内容时)
    if !nodes.is_empty() {
        nodes.push(KbNode {
            id: ROOT_ID.to_string(),
            title: "知识库".into(),
            category: String::new(),
            kind: "root".into(),
            summary: None,
        });
    }

    // ⑤ [[wikilink]] 关系边
    for d in idx.iter() {
        for link in &d.wikilinks {
            let key = link.to_lowercase();
            if let Some(target) = title_to_path.get(&key) {
                if target != &d.rel_path {
                    edge_set.insert((d.rel_path.clone(), target.clone()));
                }
            }
        }
    }

    // ⑥ Markdown 链接 [文](relpath.md) 关系边
    for d in idx.iter() {
        let base_dir = Path::new(&d.rel_path).parent();
        if let Some(body) = read_doc_body(&d.rel_path) {
            for cap in RE_MDLINK.captures_iter(&body) {
                let raw = cap.get(1).map(|m| m.as_str().trim()).unwrap_or("");
                if raw.is_empty()
                    || raw.starts_with("http")
                    || raw.starts_with('#')
                    || raw.starts_with("mailto:")
                {
                    continue;
                }
                let target_raw = raw.split(['#', '?']).next().unwrap_or(raw);
                if !(target_raw.ends_with(".md") || target_raw.ends_with(".markdown")) {
                    continue;
                }
                if let Some(t) = resolve_rel(base_dir, target_raw) {
                    if t != d.rel_path && path_set.contains(&t) {
                        edge_set.insert((d.rel_path.clone(), t));
                    }
                }
            }
        }
    }

    let edges = edge_set
        .into_iter()
        .map(|(source, target)| KbEdge {
            source,
            target,
            rel: None,
        })
        .collect();

    KbGraph { nodes, edges }
}

// ───────────────────────── wiki 质量检查 (借鉴 llm_wiki lint + sweep) ─────────────────────────
//
// 知识库会「自己越长越乱」: claude 编译时可能写出指向不存在页的死双链、漏写 frontmatter 的 type、
// 留下没人链接也不链接别人的孤儿页。借鉴 llm_wiki 的 lint: 纯规则扫一遍 INDEX, 把问题列清楚,
// 作为「后台巡检 (sweep)」的眼睛 —— 先看见问题, 才能交给 kb_dedup / kb_enrich_links 去修。

#[derive(Serialize)]
pub struct KbLintIssue {
    /// dead-link | missing-type | orphan | unsafe-path
    pub kind: String,
    pub path: String,
    pub detail: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KbLintReport {
    pub total_pages: usize,
    pub dead_links: usize,
    pub missing_type: usize,
    pub orphans: usize,
    pub unsafe_paths: usize,
    pub issues: Vec<KbLintIssue>,
}

/// 导航/元页 (index / _index / log / 方法论): 不参与「缺 type」「孤儿」判定。
pub(crate) fn is_wiki_meta_page(rp: &str) -> bool {
    rp == "wiki/index.md"
        || rp.ends_with("/_index.md")
        || rp == "wiki/log.md"
        || rp.ends_with("/log.md")
        || rp == "wiki/karpathy-wiki方法论.md"
        || rp == "wiki/wiki-knowledge-base.md"
}

/// wiki 质量检查: 死双链 / 缺 frontmatter type / 孤儿页 / 不安全路径。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn kb_lint() -> KbLintReport {
    use std::collections::HashSet;
    let idx = INDEX.read();
    let norm = |s: &str| s.replace('\\', "/");

    // 双链解析表: 小写标题 + 文件名 stem → rel_path (与 kb_graph 一致的解析口径)
    let mut title_to_path: HashMap<String, String> = HashMap::new();
    for d in idx.iter() {
        title_to_path.insert(d.title.to_lowercase(), d.rel_path.clone());
        let stem = Path::new(&d.rel_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        title_to_path
            .entry(stem)
            .or_insert_with(|| d.rel_path.clone());
    }

    // 被任意页面双链指向的目标 (用于孤儿判定)
    let mut referenced: HashSet<String> = HashSet::new();
    for d in idx.iter() {
        for link in &d.wikilinks {
            if let Some(t) = title_to_path.get(&link.to_lowercase()) {
                referenced.insert(t.clone());
            }
        }
    }

    const MAX_ISSUES: usize = 300;
    let mut issues: Vec<KbLintIssue> = Vec::new();
    let (mut dead_links, mut missing_type, mut orphans, mut unsafe_paths) = (0, 0, 0, 0);
    let mut wiki_pages = 0usize;
    let push = |issues: &mut Vec<KbLintIssue>, kind: &str, path: &str, detail: String| {
        if issues.len() < MAX_ISSUES {
            issues.push(KbLintIssue {
                kind: kind.into(),
                path: path.into(),
                detail,
            });
        }
    };

    for d in idx.iter() {
        let rp = norm(&d.rel_path);
        if !rp.starts_with("wiki/") {
            continue; // 只检查知识层
        }
        wiki_pages += 1;

        // ① 死双链: 指向不存在页面的 [[X]]
        for link in &d.wikilinks {
            if !title_to_path.contains_key(&link.to_lowercase()) {
                dead_links += 1;
                push(
                    &mut issues,
                    "dead-link",
                    &rp,
                    format!("[[{}]] 无对应页面", link),
                );
            }
        }

        // ② 不安全路径 (理论上扫到的文件都存在, 但路径形态可能不规范)
        if is_safe_wiki_relpath(&rp).is_err() {
            unsafe_paths += 1;
            if let Err(why) = is_safe_wiki_relpath(&rp) {
                push(&mut issues, "unsafe-path", &rp, why);
            }
        }

        if is_wiki_meta_page(&rp) {
            continue; // 元页不查 type / 孤儿
        }

        // ③ 缺 frontmatter type
        if d.doc_type.trim().is_empty() {
            missing_type += 1;
            push(
                &mut issues,
                "missing-type",
                &rp,
                "frontmatter 缺 type 字段".into(),
            );
        }

        // ④ 孤儿页: 既不链接别人, 也没人链接它
        let links_out = !d.wikilinks.is_empty();
        let linked_in = referenced.contains(&d.rel_path);
        if !links_out && !linked_in {
            orphans += 1;
            push(
                &mut issues,
                "orphan",
                &rp,
                "无入链也无出链, 未接入知识网".into(),
            );
        }
    }

    KbLintReport {
        total_pages: wiki_pages,
        dead_links,
        missing_type,
        orphans,
        unsafe_paths,
        issues,
    }
}
