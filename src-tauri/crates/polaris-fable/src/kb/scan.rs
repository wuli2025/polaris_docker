//! 索引状态 (INDEX/KB_ROOT)、初始化、设置持久化、目录扫描与 frontmatter 解析 —— 自原 kb.rs 纯移动, 逻辑零改动。

use super::*;

// ───────────────────────── State ─────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct KbDoc {
    pub rel_path: String,
    pub title: String,
    pub category: String,
    /// frontmatter 的 `type` (entity/concept/source/synthesis), 缺省空串。供 kb_lint 校验。
    pub doc_type: String,
    pub wikilinks: Vec<String>,
}

/// 单篇正文按需读的字节上限(与 kb_read 同口径)。GB 级日志/数据集若被无上限
/// 直读会 OOM(弱 NAS 尤甚),搜索打分对超大文件也只需前若干字节即可命中。
pub(crate) const MAX_BODY_BYTES: u64 = 8 * 1024 * 1024;

/// 按需读取 KB 中某篇 wiki 的完整正文(不走 INDEX, 直接读磁盘)。
pub(crate) fn read_doc_body(rel_path: &str) -> Option<String> {
    let root = KB_ROOT.read();
    let full = root.join(rel_path);
    if !full.starts_with(&*root) || !full.is_file() {
        return None;
    }
    // 去除 frontmatter, 只返回正文(与 parse_doc 保持一致)
    let raw = read_text_capped(&full)?;
    match RE_FRONTMATTER.captures(&raw) {
        Some(c) => Some(raw[c.get(0)?.end()..].to_string()),
        None => Some(raw),
    }
}

/// 有界读文本: 超过 MAX_BODY_BYTES 只按 UTF-8 边界安全截读前 8 MiB,
/// 避免巨型文件一次性撑爆内存(与 kb_read 的护栏同口径)。
pub(crate) fn read_text_capped(full: &Path) -> Option<String> {
    let meta = fs::metadata(full).ok()?;
    if meta.len() > MAX_BODY_BYTES {
        use std::io::Read;
        let mut f = fs::File::open(full).ok()?;
        let mut buf = vec![0u8; MAX_BODY_BYTES as usize];
        let n = f.read(&mut buf).ok()?;
        buf.truncate(n);
        return Some(String::from_utf8_lossy(&buf).into_owned());
    }
    fs::read_to_string(full).ok()
}

pub(crate) static INDEX: Lazy<RwLock<Vec<KbDoc>>> = Lazy::new(|| RwLock::new(Vec::new()));
pub(crate) static KB_ROOT: Lazy<RwLock<PathBuf>> = Lazy::new(|| RwLock::new(PathBuf::new()));

/// KB 根目录(PathBuf 原值, 未设置为空)。`pub`: wiki 构建管线(3→2)跨 crate 取用,
/// 不直接暴露 KB_ROOT 内部锁。
pub fn kb_root_pathbuf() -> PathBuf {
    KB_ROOT.read().clone()
}

/// 重扫全库并替换内存索引, 返回文档数。`pub`: wiki 的 kb_compile 完成后刷新用。
pub fn kb_reindex() -> usize {
    let root = KB_ROOT.read().clone();
    let docs = scan_all(&root);
    let n = docs.len();
    *INDEX.write() = docs;
    n
}

// ───────────────────────── Init ──────────────────────────

pub fn init(_app: &AppHandle) -> Result<()> {
    let settings = load_settings();
    let root = settings
        .kb_root
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| default_kb_root().unwrap_or_else(|_| PathBuf::from(".")));
    ensure_skeleton(&root)?;
    *KB_ROOT.write() = root.clone();
    // 把「全量扫描解析」挪到后台线程，别拖住窗口出现。
    // scan_all 会 WalkDir 递归读+解析每篇 .md（KB 越大越慢）。而 INDEX 只被 KB 视图/命令
    // 按需用，首屏根本不读它，所以启动即设好 KB_ROOT（其它板块要它、且很轻），
    // 重活丢后台几百 ms 内填好 INDEX。
    // 注: 此前这里还有「首启播种毛主席资料库」(seed_default_kb)——已改成「名人资料包」
    // 按需安装(kb_pack_install)，不再初始自带。
    std::thread::spawn(move || {
        let docs = scan_all(&root);
        *INDEX.write() = docs;
    });
    Ok(())
}

pub(crate) fn default_kb_root() -> Result<PathBuf> {
    let user = UserDirs::new().ok_or_else(|| anyhow::anyhow!("no user dir"))?;
    let home = user.home_dir();
    Ok(home.join("Polaris").join("PolarisKB"))
}

// ───────────────────────── 名人资料包 (KB Packs) ─────────────────────────
//
// 随安装包打进来的名人资料(`resources/seed-kb/<名人>/`)**不再首启自动播种**，
// 改为「名人知识库」里的可安装资料包：点「下载到我的资料库」才拷到 `<KB>/raw/<名人>/`，
// 并顺带把配套 skill(内含该资料库的使用方法)装到用户技能目录 —— 资料和用法一起到手。
// 移除资料包时配套 skill 一并移除。

// ───────────────────────── Settings ──────────────────────

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct AppSettings {
    kb_root: Option<String>,
}

pub(crate) fn settings_path() -> Result<PathBuf> {
    let pd = ProjectDirs::from("com", "polaris", "polaris-app")
        .ok_or_else(|| anyhow::anyhow!("no config dir"))?;
    let dir = pd.config_dir().to_path_buf();
    fs::create_dir_all(&dir)?;
    Ok(dir.join("settings.json"))
}

pub(crate) fn load_settings() -> AppSettings {
    settings_path()
        .ok()
        .and_then(|p| fs::read_to_string(&p).ok())
        .and_then(|s| serde_json::from_str::<AppSettings>(&s).ok())
        .unwrap_or_default()
}

pub(crate) fn save_settings(s: &AppSettings) -> Result<()> {
    let p = settings_path()?;
    fs::write(p, serde_json::to_string_pretty(s)?)?;
    Ok(())
}

/// 三层目录铁律 (PRD §8.3) + 回声层第四车道 memory/(寓言计划 v5 §6:
/// 对话沉淀落这里,享受图谱可视化但注入只给地图,不进 wiki/ 全文区防臃肿)。
pub(crate) fn ensure_skeleton(root: &Path) -> Result<()> {
    for sub in ["raw", "output", "wiki", "memory"] {
        fs::create_dir_all(root.join(sub))?;
    }
    let claude_md = root.join("CLAUDE.md");
    if !claude_md.exists() {
        fs::write(&claude_md, include_str!("../../../../src/templates/kb_claude.md"))?;
    }
    let index_md = root.join("wiki").join("index.md");
    if !index_md.exists() {
        fs::write(&index_md, include_str!("../../../../src/templates/wiki_index.md"))?;
    }
    Ok(())
}

// ───────────────────────── Scan + Parse ──────────────────

pub(crate) fn scan_all(root: &Path) -> Vec<KbDoc> {
    if !root.exists() {
        return Vec::new();
    }
    // ① 廉价遍历目录项,收集待解析的 .md 路径(只读目录、不读内容,快)。
    //    conversations/ 不纳入知识库索引/图谱(保护板块②不被对话产物污染);
    //    这些文件改由 chat::artifact_search 单独检索。
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(root).into_iter().flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
        if ext != "md" && ext != "markdown" {
            continue;
        }
        if let Ok(rel) = p.strip_prefix(root) {
            if rel.components().next().and_then(|c| c.as_os_str().to_str()) == Some("conversations")
            {
                continue;
            }
            paths.push(p.to_path_buf());
        }
    }
    if paths.is_empty() {
        return Vec::new();
    }
    // 路径排序 → 解析结果顺序确定(与多线程分片无关),便于复现。
    paths.sort();

    // ② 多线程并行解析:parse_doc 要读全文 + 跑多条正则,是真瓶颈(KB 几百篇时
    //    旧的单线程顺序解析会卡主线程数百 ms~数秒)。parse_doc 是纯函数、共享的只有
    //    Lazy 正则(Sync),把路径切成 N 份各线程处理一段、主线程按序合并 → 近线性加速。
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8)
        .min(paths.len());
    if workers <= 1 {
        return paths
            .iter()
            .filter_map(|p| p.strip_prefix(root).ok().and_then(|rel| parse_doc(p, rel)))
            .collect();
    }
    let chunk_size = paths.len().div_ceil(workers);
    let mut docs: Vec<KbDoc> = Vec::with_capacity(paths.len());
    std::thread::scope(|s| {
        let mut handles = Vec::new();
        for chunk in paths.chunks(chunk_size) {
            handles.push(s.spawn(move || {
                let mut local = Vec::with_capacity(chunk.len());
                for p in chunk {
                    if let Ok(rel) = p.strip_prefix(root) {
                        if let Some(d) = parse_doc(p, rel) {
                            local.push(d);
                        }
                    }
                }
                local
            }));
        }
        for h in handles {
            if let Ok(mut local) = h.join() {
                docs.append(&mut local);
            }
        }
    });
    docs
}

pub(crate) static RE_FRONTMATTER: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?s)^---\r?\n(.*?)\r?\n---\r?\n").unwrap());
pub(crate) static RE_TITLE_H1: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^#\s+(.+)$").unwrap());
pub(crate) static RE_WIKILINK: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[\[([^\]\|#]+)(?:[#\|][^\]]*)?\]\]").unwrap());
/// 标准 Markdown 链接 [文字](目标) — 用于从 README/目录页派生边
pub(crate) static RE_MDLINK: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[[^\]]*\]\(([^)]+)\)").unwrap());
pub(crate) static RE_YAML_KV: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^(\w+)\s*:\s*(.+)$").unwrap());

pub(crate) fn parse_doc(abs_path: &Path, rel: &Path) -> Option<KbDoc> {
    let body = fs::read_to_string(abs_path).ok()?;

    // 提取 frontmatter
    let (fm, body_only) = match RE_FRONTMATTER.captures(&body) {
        Some(c) => (
            c.get(1).map(|m| m.as_str().to_string()).unwrap_or_default(),
            body[c.get(0).unwrap().end()..].to_string(),
        ),
        None => (String::new(), body.clone()),
    };

    // category / type
    let mut category = String::new();
    let mut doc_type = String::new();
    let mut fm_title: Option<String> = None;
    for cap in RE_YAML_KV.captures_iter(&fm) {
        let k = cap.get(1).map(|m| m.as_str()).unwrap_or("").to_lowercase();
        let v = cap
            .get(2)
            .map(|m| m.as_str().trim().trim_matches('"'))
            .unwrap_or("");
        match k.as_str() {
            "category" => category = v.to_string(),
            "type" => doc_type = v.to_string(),
            "title" => fm_title = Some(v.to_string()),
            _ => {}
        }
    }

    // title: frontmatter > # H1 > 文件名
    let title = fm_title
        .or_else(|| {
            RE_TITLE_H1
                .captures(&body_only)
                .and_then(|c| c.get(1).map(|m| m.as_str().trim().to_string()))
        })
        .unwrap_or_else(|| {
            abs_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("untitled")
                .to_string()
        });

    // [[wikilinks]]
    let wikilinks: Vec<String> = RE_WIKILINK
        .captures_iter(&body_only)
        .filter_map(|c| c.get(1).map(|m| m.as_str().trim().to_string()))
        .collect();

    Some(KbDoc {
        rel_path: rel.to_string_lossy().replace('\\', "/"),
        title,
        category,
        doc_type,
        wikilinks,
    })
}

// ───────────────────────── Tauri commands ────────────────

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn kb_root() -> String {
    KB_ROOT.read().to_string_lossy().to_string()
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn kb_default_root() -> String {
    default_kb_root()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn kb_set_root(new_path: String) -> Result<usize, String> {
    let trimmed = new_path.trim().to_string();
    if trimmed.is_empty() {
        return Err("路径不能为空".into());
    }
    let new_root = PathBuf::from(&trimmed);
    ensure_skeleton(&new_root).map_err(|e| format!("无法创建目录骨架: {e}"))?;
    let mut s = load_settings();
    s.kb_root = Some(trimmed);
    save_settings(&s).map_err(|e| format!("写入设置失败: {e}"))?;
    *KB_ROOT.write() = new_root.clone();
    let docs = scan_all(&new_root);
    let n = docs.len();
    *INDEX.write() = docs;
    Ok(n)
}

/// 同步核: 全量重扫 KB 并刷新 INDEX。
/// `scan_all` 走 `thread::scope` 并阻塞到所有解析 join(10k+ 文档的大 NAS 库要 10-60s)。
/// desktop 端由下面的 async 包装挪到 spawn_blocking,别冻 UI 主线程;
/// server flavor 由 server.rs 直调本同步核。
pub fn kb_scan_sync() -> Result<usize, String> {
    let root = KB_ROOT.read().clone();
    let docs = scan_all(&root);
    let n = docs.len();
    *INDEX.write() = docs;
    Ok(n)
}

/// desktop 端命令: 把阻塞的全量重扫挪到 blocking 线程池,避免冻结窗口主线程。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn kb_scan() -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(kb_scan_sync)
        .await
        .map_err(|e| format!("扫描任务异常退出: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 并行 scan_all 正确性:多线程分片解析不丢、不重、不串,合并后按 rel_path 稳定有序,
    /// 且 conversations/ 被排除、非 md 不收。文件数远大于 worker 上限,确保真的走多线程那条路
    /// (这也间接验证它不会死锁/卡住 —— thread::scope 必等所有子线程 join 才返回)。
    #[test]
    fn parallel_scan_all_complete_ordered_no_dupes() {
        let base = std::env::temp_dir().join(format!("polaris_scanall_{}", std::process::id()));
        let root = base.join("kb");
        let wiki = root.join("wiki");
        let conv = root.join("conversations");
        let _ = fs::create_dir_all(&wiki);
        let _ = fs::create_dir_all(&conv);

        const N: usize = 64; // ≫ worker 上限(8),逼出多线程分片合并路径
        for i in 0..N {
            let body = format!(
                "---\ntitle: 标题{i}\ntype: concept\n---\n# H{i}\n见 [[标题{}]]\n",
                (i + 1) % N
            );
            fs::write(wiki.join(format!("p{i:03}.md")), body).unwrap();
        }
        fs::write(wiki.join("note.txt"), "ignore").unwrap(); // 非 md:不收
        fs::write(conv.join("c.md"), "# 对话产物\n").unwrap(); // conversations/:排除

        let docs = scan_all(&root);
        assert_eq!(
            docs.len(),
            N,
            "应收全部 {N} 篇 wiki(不含 txt / conversations)"
        );

        let paths: Vec<&str> = docs.iter().map(|d| d.rel_path.as_str()).collect();
        let uniq: std::collections::HashSet<&str> = paths.iter().copied().collect();
        assert_eq!(uniq.len(), N, "并行分片合并不应产生重复");
        assert!(
            !docs.iter().any(|d| d.rel_path.contains("conversations")),
            "对话产物目录必须被排除"
        );
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(
            paths, sorted,
            "结果应按 rel_path 稳定有序(分片合并不打乱顺序)"
        );

        // 抽查解析正确性(frontmatter 标题 + 双链)
        let p0 = docs
            .iter()
            .find(|d| d.rel_path.ends_with("p000.md"))
            .unwrap();
        assert_eq!(p0.title, "标题0");
        assert!(p0.wikilinks.iter().any(|w| w == "标题1"), "双链应被解析出");

        let _ = fs::remove_dir_all(&base);
    }
}
