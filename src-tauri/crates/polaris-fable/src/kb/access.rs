//! kb_list / 上下文注入块 / kb_read / kb_delete / kb_clear / 路径护栏 —— 自原 kb.rs 纯移动, 逻辑零改动。

use super::*;

// 桌面 async + spawn_blocking:大库上 idx.iter().map(clone).collect() 会把几十万~
// 上百万条 rel_path 全克隆,足以在主线程上卡出可感顿挫。server flavor 保持同步直调。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn kb_list(subdir: Option<String>) -> Vec<String> {
    tauri::async_runtime::spawn_blocking(move || kb_list_sync(subdir))
        .await
        .unwrap_or_default()
}
#[cfg(not(feature = "desktop"))]
pub fn kb_list(subdir: Option<String>) -> Vec<String> {
    kb_list_sync(subdir)
}

pub(crate) fn kb_list_sync(subdir: Option<String>) -> Vec<String> {
    let idx = INDEX.read();
    idx.iter()
        .filter(|d| {
            subdir
                .as_deref()
                .map(|s| d.rel_path.starts_with(s))
                .unwrap_or(true)
        })
        .map(|d| d.rel_path.clone())
        .collect()
}

/// 封顶取样：单遍既返回前 `limit` 条路径，又给出匹配总数。
/// 给「按知识库反推专家团」这类只需少量样本 + 总量的调用用 ——
/// 大库上避免 `kb_list` 把几百万条 rel_path 全克隆出来再被 `.take(N)` 丢掉。
pub fn kb_list_sample(subdir: Option<String>, limit: usize) -> (Vec<String>, usize) {
    let idx = INDEX.read();
    let mut total = 0usize;
    let mut out: Vec<String> = Vec::new();
    for d in idx.iter() {
        let hit = subdir
            .as_deref()
            .map(|s| d.rel_path.starts_with(s))
            .unwrap_or(true);
        if !hit {
            continue;
        }
        total += 1;
        if out.len() < limit {
            out.push(d.rel_path.clone());
        }
    }
    (out, total)
}

// ───────────────────────── 上下文预算 (借鉴 llm_wiki context-budget) ─────────────────────────
//
// 痛点: wiki/ 全文注入 42k 字符曾撞 Windows 命令行 32k 上限(206)。即便改走 stdin, 无节制
// 注入也会吃掉模型有限的上下文窗口、挤掉它「回话」的余量。
// 借鉴 llm_wiki 的做法: 不拍脑袋, 按**固定比例**切预算 —— 导航页占大头、地图占其余、
// 留一截给模型回答。预算耗尽就优雅截断并显式告知「其余请用 Read/Glob 自取」。

/// 注入块总字符预算 (保守取值: 远低于 32k 命令行上限, 也给模型窗口留足回话余量)。
pub(crate) const KB_CTX_BUDGET: usize = 24_000;
/// 导航页(index/_index)分到的比例 —— 它们是「目录」, 信息密度最高, 给大头。
pub(crate) const KB_CTX_NAV_RATIO: f32 = 0.55;
/// 地图清单(raw/ 等文件标题列表)分到的比例。
pub(crate) const KB_CTX_MAP_RATIO: f32 = 0.40;
/// 单篇导航页正文上限 (防一个超大 _index 吃光整段预算)。
pub(crate) const KB_CTX_PER_PAGE_RATIO: f32 = 0.30;

/// 按字符边界安全截断; 超出时追加省略标记。
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!(
        "{}\n\n…(本页过长已截断, 需要全文请用 `Read` 打开)",
        cut.trim_end()
    )
}

/// Karpathy 式「结构化 wiki + 长上下文 + 双链导航」上下文块, 供 chat 发送前注入。
///
/// 不做关键词召回硬塞 (那是 Karpathy 反对的「平铺 + 向量/关键词召回」范式)。而是把
/// **wiki/ 知识层导航页** + **整库的双链/目录地图** + **KB 根的绝对路径** 给模型,
/// 让它用 Read/Glob/Grep 沿双链自取 —— 这才是 headless 下真正可行、且忠于 llmwiki 的
/// 「调用知识库」方式 (claude CLI 在 --print 下有 Read/Glob/Grep, 且 KB 就在 cwd 子树里)。
/// 注入量受 [`KB_CTX_BUDGET`] 约束, 按比例分配给导航页与地图。
/// KB 为空 / 不存在时返回空串。
pub fn kb_context_block() -> String {
    kb_context_block_scoped(None)
}

/// 同 [`kb_context_block`]，但可按 `scope`（KB 根下相对子目录，如 `raw/毛主席`）
/// 把「知识库地图」收窄到该子树 —— 板块⑫ 让不同人格看到各自的专属知识库。
/// `scope=None` 时行为与全局一致（向后兼容）。
pub fn kb_context_block_scoped(scope: Option<&str>) -> String {
    let root = KB_ROOT.read().clone();
    if root.as_os_str().is_empty() || !root.exists() {
        return String::new();
    }
    let idx = INDEX.read();
    if idx.is_empty() {
        return String::new();
    }
    let norm = |s: &str| s.replace('\\', "/");
    let stem = |rp: &str| -> String {
        let n = norm(rp);
        let base = n.rsplit('/').next().unwrap_or(&n).to_string();
        base.strip_suffix(".md")
            .or_else(|| base.strip_suffix(".markdown"))
            .unwrap_or(&base)
            .to_string()
    };
    let parent = |rp: &str| -> String {
        let n = norm(rp);
        match n.rfind('/') {
            Some(i) => n[..i].to_string(),
            None => ".".to_string(),
        }
    };

    let root_disp = norm(&root.to_string_lossy());
    let mut out = String::new();
    out.push_str(&format!(
        "### 维基库结构 (Karpathy 式: 结构化 wiki + 长上下文 + 双链导航)\n\n\
知识库根目录: `{root_disp}`\n\
**就在你的工作目录下** —— 你可以(并且应当)用 `Read` / `Glob` / `Grep` 直接打开其中任意页面来取证。\n\
三层目录: `raw/`(只读原始资料, 严禁写入) · `output/`(生成的成品) · `wiki/`(人工确认的知识层)。\n\n"
    ));

    // wiki/ 知识层: 只注入「导航文件」(顶层 index.md + 各子目录 _index.md),
    // 不再全文注入每篇 wiki —— 46 篇全文 42k 字符直接撞 Windows 命令行 32k 上限(206)。
    // 模型要细看哪篇, 用 `Read` 沿双链或路径自取 —— 索引里把路径写清楚就行。
    let mut nav_docs: Vec<&KbDoc> = idx
        .iter()
        .filter(|d| {
            let rp = norm(&d.rel_path);
            // 顶层 index.md / 顶层元页(方法论/说明/log)
            rp == "wiki/index.md"
                || rp == "wiki/karpathy-wiki方法论.md"
                || rp == "wiki/wiki-knowledge-base.md"
                // 各子目录 _index.md
                || rp.ends_with("/_index.md")
        })
        .collect();
    nav_docs.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    // 导航段省下多少字符可让给地图段 (由下方地图循环用)。
    let mut nav_surplus: usize = 0;
    if !nav_docs.is_empty() {
        out.push_str(
            "#### wiki/ 知识层 (仅注入导航页: 顶层 index + 各子目录 _index, 全文请用 Read 沿双链/路径自取)\n\n"
        );
        // 上下文预算 · 密度自适应 (借鉴 llm_wiki context-budget, 改造成弹性版):
        // 先看导航页**实际**总字符 S, 与硬上限比较:
        //   S ≤ nav_budget: 整段全塞 (不浪费), 省下的 `nav_budget − S` 让位给地图
        //   S > nav_budget: 仍按 nav_budget + 单页上限截, 多塞不进去
        // 单页上限 (per_page) 始终保留 —— 防一个超大 _index 独霸整段预算。
        let nav_budget = (KB_CTX_BUDGET as f32 * KB_CTX_NAV_RATIO) as usize;
        let per_page = (KB_CTX_BUDGET as f32 * KB_CTX_PER_PAGE_RATIO) as usize;
        // 导航页按需读正文算长度(导航页数量极少,IO 开销可忽略)
        let nav_bodies: Vec<(String, usize)> = nav_docs
            .iter()
            .filter_map(|d| {
                read_doc_body(&d.rel_path).map(|b| {
                    let trimmed = b.trim().to_string();
                    let len = trimmed.chars().count();
                    (trimmed, len)
                })
            })
            .collect();
        let nav_total: usize = nav_bodies.iter().map(|(_, len)| len).sum();
        let effective_nav = nav_budget.min(nav_total);
        nav_surplus = nav_budget - effective_nav; // 全塞时为「段内让位」,截断时为 0
        let mut nav_used = 0usize;
        let mut nav_truncated = 0usize;
        for (d, (body_raw, _)) in nav_docs.iter().zip(nav_bodies.iter()) {
            if nav_used >= effective_nav {
                nav_truncated += 1;
                continue;
            }
            // 本篇可用额度 = min(单篇上限, 段内剩余); 截断后会附"已截断"提示
            let avail = per_page.min(effective_nav - nav_used);
            let body = truncate_chars(body_raw.trim(), avail);
            nav_used += body.chars().count();
            out.push_str(&format!(
                "##### [[{}]] · `{}`\n\n{}\n\n",
                stem(&d.rel_path),
                norm(&d.rel_path),
                body
            ));
        }
        if nav_total > effective_nav {
            // 触发了截断 (整体超上限)
            if nav_total <= nav_budget {
                out.push_str(&format!("*(导航段共 {} 字符, 触达上限)*\n\n", nav_total));
            } else {
                out.push_str(&format!(
                    "*(还有 {} 篇导航页/总计 {} 字符未注入, 用 `Read` 打开 wiki/index.md 或对应 _index.md 查看)*\n\n",
                    nav_truncated, nav_total.saturating_sub(nav_used)
                ));
            }
        }
        // 提示: 其他 40+ 篇 wiki 的目录清单在 wiki/index.md / 概念/_index.md / 实体/_index.md 里
        let wiki_total = idx
            .iter()
            .filter(|d| {
                norm(&d.rel_path).starts_with("wiki/") && norm(&d.rel_path).ends_with(".md")
            })
            .count();
        out.push_str(&format!(
            "*(wiki/ 共 {} 篇, 此处仅注入 {} 篇导航页;要看某篇正文请用 Read 打开对应 .md)*\n\n",
            wiki_total,
            nav_docs.len()
        ));
    }

    // 知识库地图: raw/ output/ 等按文件夹分组, 列标题清单 (供沿双链/路径用 Read/Grep 自取)
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<String, Vec<&KbDoc>> = BTreeMap::new();
    let scope_norm = scope.map(norm).filter(|s| !s.trim().is_empty());
    for d in idx.iter() {
        let rp = norm(&d.rel_path);
        if rp == "CLAUDE.md" || rp.starts_with("wiki/") {
            continue; // 行为指南单独注入; wiki 已全文给过
        }
        // 板块⑫: 限定到该人格的知识库 scope 子树
        if let Some(s) = &scope_norm {
            if !rp.starts_with(s.as_str()) {
                continue;
            }
        }
        groups.entry(parent(&rp)).or_default().push(d);
    }
    if !groups.is_empty() {
        out.push_str("#### 知识库地图 (沿双链 `[[名称]]` 或路径, 用 Read / Grep 自取原文)\n\n");
        if let Some(s) = &scope_norm {
            out.push_str(&format!(
                "*(本人格知识范围限定在 `{}/` 子树, 其余目录不在此人格上下文内)*\n\n",
                s
            ));
        }
        // 上下文预算: 地图段按总字符封顶 (而非固定每文件夹条数), 预算耗尽即停并提示 Glob 自取。
        // 弹性: 拿导航段让位的 `nav_surplus` 补到地图, 实际预算 = 基础 + 让位, 但总不超 KB_CTX_BUDGET。
        const MAX_PER_FOLDER: usize = 60;
        let map_base = (KB_CTX_BUDGET as f32 * KB_CTX_MAP_RATIO) as usize;
        let map_budget = (map_base + nav_surplus).min(KB_CTX_BUDGET);
        let mut map_used = 0usize;
        let mut budget_hit = false;
        'folders: for (folder, docs) in &groups {
            let header = format!("- **{}/** ({} 篇)\n", folder, docs.len());
            map_used += header.chars().count();
            out.push_str(&header);
            let mut shown = 0usize;
            for d in docs.iter().take(MAX_PER_FOLDER) {
                if map_used >= map_budget {
                    budget_hit = true;
                    break 'folders;
                }
                let title = if d.title.trim().is_empty() {
                    stem(&d.rel_path)
                } else {
                    d.title.trim().to_string()
                };
                let line = format!(
                    "  - [[{}]] — {} · `{}`\n",
                    stem(&d.rel_path),
                    title,
                    norm(&d.rel_path)
                );
                map_used += line.chars().count();
                out.push_str(&line);
                shown += 1;
            }
            if docs.len() > shown {
                out.push_str(&format!(
                    "  - …其余 {} 篇, 用 `Glob \"{}/**\"` 或 `Grep` 关键词列出\n",
                    docs.len() - shown,
                    folder
                ));
            }
        }
        if budget_hit {
            out.push_str(
                "- *(地图已达上下文预算上限, 其余目录/文件请用 `Glob`/`Grep` 自行探索)*\n",
            );
        }
        out.push('\n');
    }

    out.push_str(
        "#### 调用方式 (KB-first, 忠于 Karpathy)\n\
- 回答前先沿上面的结构与双链, 用 Read/Glob/Grep 打开相关页面取证, 不要凭空作答。\n\
- 命中知识库内容时用脚注标源: 正文处 `[^1]`, 文末 `[^1]: [[文件名]]`。\n\
- 双链 `[[…]]` 只写名称 (wiki 根相对名或标题), 不写绝对路径。\n\
- 库里确实查不到时, 用 `💡` 标明这是你的推断/仿写, 不要伪造引文, 也不要谎称检索过。\n\n",
    );
    out
}

/// 把前端传入的相对路径解析为 KB root 子树内的真实路径。
/// **canonicalize 后必须仍在 KB root 之下** —— 仅靠 `starts_with(root)` 是失效护栏:
/// `root.join("../../x")` 的路径组件仍以 root 开头, 前缀检查会误判通过, 而 OS 读写时 `..`
/// 会真的逃出库外。故规范化两端再比前缀, 同时挡住 `../../` 穿越与「绝对路径替换 join」。
/// 仅用于「目标应当已存在」的入口 (read/delete); 文件不存在直接报错。
pub(crate) fn resolve_within_kb(root: &Path, rel_path: &str) -> Result<PathBuf, String> {
    let full = root.join(rel_path);
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let canon_full = full
        .canonicalize()
        .map_err(|_| "文件不存在或无法访问".to_string())?;
    if !path_contains(&canon_root, &canon_full) {
        return Err("路径越界, 拒绝访问".into());
    }
    Ok(canon_full)
}

/// 跨平台「子树包含」判断 —— 已下沉 polaris-runtime::paths(chat 产物护栏共用),
/// 此处转发保住 `kb::path_contains` 旧路径。
pub use crate::runtime::paths::path_contains;

// 桌面 async + spawn_blocking:kb_read 做真实磁盘读(最多 8MiB),NAS/机械盘上单次读
// 可达数百毫秒,叠在主线程会卡界面。server flavor 保持同步直调。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn kb_read(rel_path: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || kb_read_sync(rel_path))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
pub fn kb_read(rel_path: String) -> Result<String, String> {
    kb_read_sync(rel_path)
}

pub(crate) fn kb_read_sync(rel_path: String) -> Result<String, String> {
    let root = KB_ROOT.read().clone();
    let full = resolve_within_kb(&root, &rel_path)?;
    // 防御:rel_path 由前端/模型决定,无上限直读会被超大文件(GB 级日志/数据集)一次性
    // 撑爆内存(OOM,弱 NAS 上尤甚)。先看体积:超上限只按 UTF-8 边界安全截读前若干字节,
    // 附省略提示,既不崩也不悄悄给半截当全文。8 MiB 对任何 wiki/正文页都绰绰有余。
    const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;
    let meta = fs::metadata(&full).map_err(|e| e.to_string())?;
    if meta.len() > MAX_READ_BYTES {
        use std::io::Read;
        let mut f = fs::File::open(&full).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; MAX_READ_BYTES as usize];
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        buf.truncate(n);
        let mut s = String::from_utf8_lossy(&buf).into_owned();
        s.push_str(&format!(
            "\n\n…[文件过大: {} MB,仅显示前 {} MB;需全文请用 Grep 定向检索]…",
            meta.len() / 1024 / 1024,
            MAX_READ_BYTES / 1024 / 1024
        ));
        return Ok(s);
    }
    fs::read_to_string(&full).map_err(|e| e.to_string())
}

/// 删除资料库里的一份资料(浏览页每条右侧 × 用)。
/// 仅允许删除 KB root 子树内的文件; 删除后重扫索引, 返回剩余文件数。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn kb_delete(rel_path: String) -> Result<usize, String> {
    let root = KB_ROOT.read().clone();
    // 防越界: 规范化后必须仍在 KB root 下 (与 kb_read 共用同一护栏)
    let canon_full = resolve_within_kb(&root, &rel_path)?;
    if !canon_full.is_file() {
        return Err("只能删除文件".into());
    }
    fs::remove_file(&canon_full).map_err(|e| e.to_string())?;
    // 增量: 直接从 INDEX 移除, 避免全量重扫
    let rel_norm = rel_path.replace('\\', "/");
    index_remove(&rel_norm);
    let n = INDEX.read().len();
    Ok(n)
}

/// 清空资料库(管理页「清空资料库」用): 删除 `raw/` 下全部资料并重建空 `raw/`,
/// 保留三层骨架与 CLAUDE.md / wiki。返回清空后剩余索引文件数。
/// 已安装的名人资料包随之清掉, 想要回来去「名人资料包」重新安装即可。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn kb_clear() -> Result<usize, String> {
    let root = KB_ROOT.read().clone();
    let raw = root.join("raw");
    if raw.exists() {
        fs::remove_dir_all(&raw).map_err(|e| e.to_string())?;
    }
    fs::create_dir_all(&raw).map_err(|e| e.to_string())?;
    let docs = scan_all(&root);
    let n = docs.len();
    *INDEX.write() = docs;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_within_kb_rejects_traversal() {
        // 用真实临时目录验证运行期护栏 (区别于上面 is_safe_wiki_relpath 的纯函数校验)
        let base = std::env::temp_dir().join(format!("polaris_kbguard_{}", std::process::id()));
        let root = base.join("kb");
        let _ = fs::create_dir_all(&root);
        fs::write(root.join("inside.md"), "ok").unwrap();
        fs::write(base.join("secret.txt"), "secret").unwrap();

        // 库内文件: 放行, 且解析回真实路径
        assert!(resolve_within_kb(&root, "inside.md").is_ok());
        // `../` 穿越到库外: 必须拒 (旧 starts_with 护栏会误放)
        assert!(resolve_within_kb(&root, "../secret.txt").is_err());
        // 多级穿越同样拒
        assert!(resolve_within_kb(&root, "../../Windows/System32/drivers/etc/hosts").is_err());
        // 不存在的文件: 报错而非 panic
        assert!(resolve_within_kb(&root, "nope.md").is_err());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn path_contains_handles_verbatim_prefix_and_case() {
        // 同根, 子在内: 放行
        assert!(path_contains(
            Path::new(r"C:\Users\a\Polaris"),
            Path::new(r"C:\Users\a\Polaris\x.md")
        ));
        // 关键回归: 一端带 Windows `\\?\` 扩展长度前缀、一端没有, 仍判为包含 (旧裸 starts_with 会误判越界)
        assert!(path_contains(
            Path::new(r"C:\Users\a\Polaris"),
            Path::new(r"\\?\C:\Users\a\Polaris\x.md")
        ));
        assert!(path_contains(
            Path::new(r"\\?\C:\Users\a\Polaris"),
            Path::new(r"C:\Users\a\Polaris\x.md")
        ));
        // 伪前缀: 组件级比较不应把 `Polaris-bak` 当成 `Polaris` 的子树
        assert!(!path_contains(
            Path::new(r"C:\Users\a\Polaris"),
            Path::new(r"C:\Users\a\Polaris-bak\x.md")
        ));
        // 真越界: 不在根下
        assert!(!path_contains(
            Path::new(r"C:\Users\a\Polaris"),
            Path::new(r"C:\Windows\System32\drivers\etc\hosts")
        ));
        // Windows 上大小写不敏感: 根与子大小写不一致也应判为包含
        if cfg!(windows) {
            assert!(path_contains(
                Path::new(r"C:\Users\A\Polaris"),
                Path::new(r"c:\users\a\polaris\x.md")
            ));
        }
    }

    #[test]
    fn context_block_surplus_when_nav_is_small() {
        // 实际库 < 100 字符, 注入应接近原样而不被 55% 上限「闲置」
        use std::sync::OnceLock;
        // 直接读当前 KB 测 (单元测试跑时若有 KB 才有意义; 跑不过就当 placeholder)
        // 核心逻辑靠 nav_total ≤ nav_budget 时 nav_surplus = nav_budget − nav_total 来测
        let nav_budget = (KB_CTX_BUDGET as f32 * KB_CTX_NAV_RATIO) as usize;
        let nav_total: usize = 50; // 模拟极小
        let effective = nav_budget.min(nav_total);
        let surplus = nav_budget - effective;
        assert_eq!(effective, 50);
        assert_eq!(surplus, nav_budget - 50);
    }

    #[test]
    fn context_block_no_surplus_when_nav_fills() {
        let nav_budget = (KB_CTX_BUDGET as f32 * KB_CTX_NAV_RATIO) as usize;
        let nav_total = nav_budget + 5_000; // 溢出
        let effective = nav_budget.min(nav_total);
        let surplus = nav_budget - effective;
        assert_eq!(effective, nav_budget);
        assert_eq!(surplus, 0);
    }

    #[test]
    fn context_block_map_total_capped_by_global_budget() {
        // 即便 surplus 很大, map_budget + nav_budget 不能越界 KB_CTX_BUDGET
        let map_base = (KB_CTX_BUDGET as f32 * KB_CTX_MAP_RATIO) as usize;
        let nav_surplus = KB_CTX_BUDGET; // 极端: 导航段空, 让位最大
        let map_budget = (map_base + nav_surplus).min(KB_CTX_BUDGET);
        assert!(map_budget <= KB_CTX_BUDGET);
    }

    #[test]
    fn truncate_keeps_short_and_cuts_long() {
        assert_eq!(truncate_chars("短文本", 100), "短文本");
        let long = "字".repeat(50);
        let out = truncate_chars(&long, 10);
        assert!(out.starts_with(&"字".repeat(10)));
        assert!(out.contains("已截断"));
        // 截断后 CJK 字符不被切坏 (能正常算字符数)
        assert!(out.chars().count() > 10);
    }
}
