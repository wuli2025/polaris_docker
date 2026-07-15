//! 产物(成品)读写/列举/搜索、产物目录定位、marker 解析与访问护栏。
//! (从 chat.rs 纯移动拆出, 逻辑零变化)

use crate::conv;
use directories::UserDirs;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;
use walkdir::WalkDir;

// ───────────────────────── Artifacts (产物预览) ─────────────────────────

/// assistant 正文里夹带的产物清单 marker 前缀; 完整形如
/// `<!--POLARIS_ARTIFACTS:["C:/a/b.html"]-->`, 重载历史时由前端解析并隐藏。
pub const ARTIFACT_MARKER_PREFIX: &str = "<!--POLARIS_ARTIFACTS:";

/// artifact_search 全量遍历的墙钟预算(秒):超时以已收集的部分命中返回,防产物量大时拖垮调用方。
const ARTIFACT_SEARCH_BUDGET_SECS: u64 = 8;

/// KB 根目录 —— 经内核桥取(引擎未拼装时空串, 语义同 `kb::kb_root` 未配置)。
fn kb_root_str() -> String {
    super::bridges::kb_bridge()
        .map(|b| b.root())
        .unwrap_or_default()
}

/// 对话框文件 chip 只展示用户能直接打开的常见成品格式。
/// 脚本 / 源码 / 配置 / 锁文件等中间产物一律不进对话框(应用类成品整体归并成
/// 一个「应用文件夹」chip, 见 packaged_project_root), 免得干扰用户。
const DISPLAY_EXTS: &[&str] = &[
    // 文档
    "md", "markdown", "txt", "pdf", "doc", "docx", "ppt", "pptx", "xls", "xlsx", "csv",
    // 网页成品
    "html", "htm", // 图片
    "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "avif", "ico", // 视频 / 音频
    "mp4", "mov", "webm", "mkv", "avi", "mp3", "wav", "m4a", "aac", "flac", "ogg",
    // 打包交付
    "zip",
];

/// 扩展名白名单之外按文件名特判放行的「源稿清单」: 传统 PPT 的 spec 是 DeckStudio
/// 预览/兜底转换(ensureSpecConverted)的唯一输入,滤掉它整条路线 B 就瘫——
/// 这是 v1.0.2 白名单与 PPT 可编辑化两个并行改动撞出的集成回归。
const DISPLAY_NAMES: &[&str] = &["polaris.slides.json"];

/// 该路径是否属于「值得在对话框里展示」的常见成品文件 (按扩展名白名单 + 文件名特判)
pub(crate) fn is_displayable_artifact(path: &str) -> bool {
    let p = Path::new(path);
    if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
        if DISPLAY_NAMES.contains(&name.to_ascii_lowercase().as_str()) {
            return true;
        }
    }
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| DISPLAY_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// 若 path 落在某个「打包应用文件夹」(任一祖先目录含 polaris.project.json) 内,
/// 返回该应用文件夹根。应用内部文件不单独展示, 整个应用以文件夹为单位呈现一个 chip
/// (路径带尾随 `/` 标记是目录), 点击直接在文件管理器打开。
pub(crate) fn packaged_project_root(path: &Path) -> Option<PathBuf> {
    let mut cur = path.parent();
    while let Some(d) = cur {
        if d.join("polaris.project.json").is_file() {
            return Some(d.to_path_buf());
        }
        cur = d.parent();
    }
    None
}

/// 目录型产物的统一表示: 正斜杠 + 尾随 `/`(前端据此识别为文件夹 chip)
pub(crate) fn folder_artifact_repr(dir: &Path) -> String {
    let mut s = dir.to_string_lossy().replace('\\', "/");
    if !s.ends_with('/') {
        s.push('/');
    }
    s
}

/// 每个会话一个目录。优先落到「工作文件夹」(KB root) 下，让产物与用户的知识库
/// 同处一地、可见可备份：`<kb_root>/conversations/<id>/`。
/// KB root 不可用时回退到 `~/Polaris/data/artifacts/<id>`。
pub(crate) fn conversation_dir(conv_id: Option<&str>) -> PathBuf {
    // conversation_id 来自 IPC/HTTP，绝不能原样 join；`../../...` 会把附件/产物写出
    // conversations 根。命令边界会返回明确错误，这里再做最后一道 fail-closed 兜底。
    let id = match conv_id {
        Some(id) if conv::is_safe_conversation_id(id) => id,
        Some(_) => "invalid-conversation-id",
        None => "scratch",
    };
    let kb_root = PathBuf::from(kb_root_str());
    if !kb_root.as_os_str().is_empty() && kb_root.exists() {
        kb_root.join("conversations").join(id)
    } else {
        UserDirs::new()
            .map(|u| u.home_dir().join("Polaris").join("data").join("artifacts"))
            .unwrap_or_else(|| PathBuf::from("artifacts"))
            .join(id)
    }
}

/// 产物(成品)目录: 会话目录下的 `outputs/`。claude 把成品写到这里 → 侧边栏可预览。
/// `pub`: 板块⑮「可运行项目」(project.rs, 壳仓)也要按同一规则定位产物目录, 去扫项目清单。
pub fn artifacts_dir(conv_id: Option<&str>) -> PathBuf {
    conversation_dir(conv_id).join("outputs")
}

/// 递归快照目录里的文件 → mtime, 用于前后 diff 找新增/改动文件
pub(crate) fn dir_snapshot(dir: &Path) -> HashMap<PathBuf, SystemTime> {
    let mut m = HashMap::new();
    if !dir.exists() {
        return m;
    }
    for entry in WalkDir::new(dir).into_iter().flatten() {
        if entry.file_type().is_file() {
            if let Ok(meta) = entry.metadata() {
                if let Ok(mt) = meta.modified() {
                    m.insert(entry.path().to_path_buf(), mt);
                }
            }
        }
    }
    m
}

/// 从 assistant 正文里剥出产物清单 marker: 返回 (去掉 marker 的正文, 产物绝对路径列表)。
/// marker 形如 `<!--POLARIS_ARTIFACTS:["C:/a.html","C:/b.md"]-->`(见 ARTIFACT_MARKER_PREFIX)。
pub(crate) fn split_artifacts(content: &str) -> (String, Vec<String>) {
    if let Some(idx) = content.find(ARTIFACT_MARKER_PREFIX) {
        let after = &content[idx + ARTIFACT_MARKER_PREFIX.len()..];
        if let Some(end) = after.find("-->") {
            let paths: Vec<String> = serde_json::from_str(&after[..end]).unwrap_or_default();
            let clean = content[..idx].trim_end().to_string();
            return (clean, paths);
        }
    }
    (content.trim().to_string(), Vec::new())
}

/// 标准 Base64 编码 (无外部依赖) — 给图片产物拼 data URL 用
fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn classify_ext(ext: &str) -> &'static str {
    match ext {
        "html" | "htm" => "html",
        "svg" => "svg",
        "md" | "markdown" => "markdown",
        "png" | "apng" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "avif" => "image",
        "txt" | "json" | "csv" | "tsv" | "js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx" | "css"
        | "scss" | "less" | "py" | "rs" | "go" | "java" | "c" | "cpp" | "h" | "hpp" | "toml"
        | "yaml" | "yml" | "xml" | "log" | "sh" | "bat" | "ps1" | "sql" | "ini" | "conf"
        | "env" | "vue" | "php" | "rb" | "kt" | "swift" | "" => "text",
        _ => "binary",
    }
}

fn mime_for(ext: &str) -> &'static str {
    match ext {
        "png" | "apng" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactPayload {
    pub path: String,
    pub name: String,
    pub ext: String,
    /// html | svg | image | markdown | text | binary
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_url: Option<String>,
    pub size: u64,
}

// command(async): 读 25MB 图 + base64 编码是重 IO/CPU, 同步命令会钉住 UI 主线程
// (v1.5.2 同族问题)。属性形式让 tauri 把它挪到工作线程跑, fn 本身仍是同步签名 ——
// server flavor 的 apihub dispatch 直接调 fn, 签名不能变。
#[cfg_attr(feature = "desktop", tauri::command(async))]
pub fn artifact_read(path: String) -> Result<ArtifactPayload, String> {
    let p = ensure_artifact_path(&path)?;
    let meta = std::fs::metadata(&p).map_err(|_| format!("文件不存在或无法访问: {}", path))?;
    if !meta.is_file() {
        return Err("目标不是文件".into());
    }
    let size = meta.len();
    let name = p
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());
    let ext = p
        .extension()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let kind = classify_ext(&ext);

    match kind {
        "image" => {
            const MAX: u64 = 25 * 1024 * 1024;
            if size > MAX {
                return Err("图片过大, 无法预览 (>25MB)".into());
            }
            let bytes = std::fs::read(&p).map_err(|e| e.to_string())?;
            let data_url = format!("data:{};base64,{}", mime_for(&ext), base64_encode(&bytes));
            Ok(ArtifactPayload {
                path,
                name,
                ext,
                kind: kind.into(),
                text: None,
                data_url: Some(data_url),
                size,
            })
        }
        "binary" => Ok(ArtifactPayload {
            path,
            name,
            ext,
            kind: kind.into(),
            text: None,
            data_url: None,
            size,
        }),
        _ => {
            // html / svg / markdown / text
            const MAX: u64 = 8 * 1024 * 1024;
            if size > MAX {
                return Err("文件过大, 无法预览 (>8MB)".into());
            }
            let text = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
            Ok(ArtifactPayload {
                path,
                name,
                ext,
                kind: kind.into(),
                text: Some(text),
                data_url: None,
                size,
            })
        }
    }
}

/// 用系统默认程序打开产物文件 (浏览器开 HTML / 看图器开图片等)
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn artifact_open_external(path: String) -> Result<(), String> {
    // 护栏 + 规范化: 只允许打开 App 管理目录内的文件, 且用解析后的绝对路径喂给系统命令
    let path = ensure_artifact_path(&path)?.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    {
        // rundll32 对本地文件同样走系统默认关联,且不解析 &/^ —— cmd start 会在 & 处
        // 截断并把后半段当命令执行(产物文件名来自模型生成内容,完全可能含 &)。
        Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", &path])
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 在系统文件管理器中定位并选中该产物文件 (Windows 资源管理器 / macOS Finder)。
/// Linux 无统一「选中文件」语义, 退化为打开其所在目录。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn artifact_reveal(path: String) -> Result<(), String> {
    // 护栏 + 规范化: 只允许定位 App 管理目录内的文件
    let path = ensure_artifact_path(&path)?.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // explorer /select 需要反斜杠路径; 用 raw_arg 让路径被正确引号包裹
        let win_path = path.replace('/', "\\");
        Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", win_path))
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .args(["-R", &path])
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let parent = std::path::Path::new(&path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());
        Command::new("xdg-open")
            .arg(&parent)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 把编辑后的文本写回一个**已存在**的产物文件 (供「成品编辑器」保存 HTML / 网页 deck)。
/// 护栏: 复用 ensure_artifact_path —— 路径必须已存在且落在 App 管理目录内, 防越界写入。
/// 仅允许文本类后缀, 防止误把二进制 / 可执行覆盖掉。原子写 (先写临时文件再 rename)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn artifact_write(path: String, content: String) -> Result<(), String> {
    let p = ensure_artifact_path(&path)?;
    if !p.is_file() {
        return Err("目标不是文件".into());
    }
    let ext = p
        .extension()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let editable = matches!(
        ext.as_str(),
        "html" | "htm" | "svg" | "md" | "markdown" | "txt" | "json" | "csv" | "css" | "js"
    );
    if !editable {
        return Err(format!("该文件类型不支持编辑保存: .{ext}"));
    }
    const MAX: usize = 16 * 1024 * 1024;
    if content.len() > MAX {
        return Err("内容过大, 拒绝保存 (>16MB)".into());
    }
    // 原子写: 同目录临时文件 → rename, 避免写一半损坏原文件。
    let parent = p.parent().ok_or("无法定位父目录")?;
    let tmp = parent.join(format!(
        ".{}.polaris-tmp",
        p.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    ));
    std::fs::write(&tmp, content.as_bytes()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &p).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })?;
    Ok(())
}

/// 「参考资料」文件夹视图的一条文件记录。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactEntry {
    /// 绝对路径 (正斜杠), 供 artifact_read / openExternal 用
    pub path: String,
    pub name: String,
    pub ext: String,
    /// html | svg | image | markdown | text | binary —— 前端选图标 / 预览方式
    pub kind: String,
    pub size: u64,
    /// 修改时间 (Unix 秒), 前端按此倒序 + 显示
    pub modified: u64,
}

/// 列出某会话产物目录下的全部成品文件, 按修改时间倒序 (最新在前)。
/// 供右侧抽屉「参考资料」以文件夹视图按时间排列、点开即预览。
///
/// 桌面 async + spawn_blocking:WalkDir 遍历产物目录(单会话可积攒大量文件)是真实磁盘
/// IO,直接同步跑在主线程会卡。server flavor 保持同步直调。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn artifact_list(conversation_id: Option<String>) -> Vec<ArtifactEntry> {
    tauri::async_runtime::spawn_blocking(move || artifact_list_sync(conversation_id))
        .await
        .unwrap_or_default()
}
#[cfg(not(feature = "desktop"))]
pub fn artifact_list(conversation_id: Option<String>) -> Vec<ArtifactEntry> {
    artifact_list_sync(conversation_id)
}

fn artifact_list_sync(conversation_id: Option<String>) -> Vec<ArtifactEntry> {
    let dir = artifacts_dir(conversation_id.as_deref());
    let mut entries: Vec<ArtifactEntry> = Vec::new();
    if !dir.exists() {
        return entries;
    }
    for w in WalkDir::new(&dir).into_iter().flatten() {
        if !w.file_type().is_file() {
            continue;
        }
        let p = w.path();
        let meta = match w.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        // 跳过隐藏 / 临时文件
        if name.starts_with('.') {
            continue;
        }
        // 与对话框 chip 同一策略: 只列常见成品格式, 不列脚本/配置等中间产物;
        // 打包应用内部文件不逐个列出(右抽屉「项目」tab 以应用为单位呈现)
        let p_norm = p.to_string_lossy().replace('\\', "/");
        if !is_displayable_artifact(&p_norm) || packaged_project_root(p).is_some() {
            continue;
        }
        let ext = p
            .extension()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        entries.push(ArtifactEntry {
            path: p_norm,
            name,
            ext: ext.clone(),
            kind: classify_ext(&ext).to_string(),
            size: meta.len(),
            modified,
        });
    }
    entries.sort_by(|a, b| b.modified.cmp(&a.modified));
    entries
}

/// 跨「所有对话」产物的搜索命中。供历史对话记忆检索把过往输出文件也算入。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactSearchHit {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub conversation_id: String,
    pub snippet: String,
    pub modified: u64,
    pub score: i32,
}

/// 产物命令 (read/open/reveal) 允许访问的根目录集合 (已规范化)。
/// = `~/Polaris` (含 data/artifacts、projects) + KB root (含 conversations 与 KB 资料)。
/// 这些是 App 自己产出/管理文件的地方; 命令传入的路径 canonicalize 后必须落在其一之内。
fn allowed_open_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(u) = UserDirs::new() {
        roots.push(u.home_dir().join("Polaris"));
    }
    let kb_root = PathBuf::from(kb_root_str());
    if !kb_root.as_os_str().is_empty() {
        roots.push(kb_root);
    }
    roots
        .into_iter()
        .filter_map(|r| r.canonicalize().ok())
        .collect()
}

/// 产物访问护栏: 把前端传入的路径 canonicalize 后, 校验其落在某个允许根之内。
/// 挡前端 (或被构造的会话内容) 用任意系统路径去读取 / 用默认程序打开 / 资源管理器
/// 定位库外文件 (e.g. `C:\Windows\...`、`../../` 穿越)。返回规范化后的绝对路径。
fn ensure_artifact_path(path: &str) -> Result<PathBuf, String> {
    let canon = PathBuf::from(path)
        .canonicalize()
        .map_err(|_| format!("文件不存在或无法访问: {path}"))?;
    let roots = allowed_open_roots();
    if roots.iter().any(|r| crate::runtime::paths::path_contains(r, &canon)) {
        Ok(canon)
    } else {
        Err("路径越界, 拒绝访问".into())
    }
}

/// 所有「会话根目录」候选: 工作文件夹(KB root)/conversations 与回退目录。
fn conversation_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let kb_root = PathBuf::from(kb_root_str());
    if !kb_root.as_os_str().is_empty() && kb_root.exists() {
        roots.push(kb_root.join("conversations"));
    }
    if let Some(u) = UserDirs::new() {
        roots.push(u.home_dir().join("Polaris").join("data").join("artifacts"));
    }
    roots
}

/// 在所有对话的 outputs 里检索: 文件名命中 +10, 正文命中 +2/次(上限), 按分数+时间排序。
/// 让「搜索以前的对话记忆」把之前输出的文件也算入。
///
/// 桌面 async + spawn_blocking:跨所有会话 WalkDir 遍历 + 逐个文本文件读正文匹配,是重
/// 磁盘 IO(此前 v1.6.2 实测的「挂死」同族),直接同步跑在主线程会卡。server flavor 保持同步。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn artifact_search(query: String) -> Vec<ArtifactSearchHit> {
    tauri::async_runtime::spawn_blocking(move || artifact_search_sync(query))
        .await
        .unwrap_or_default()
}
#[cfg(not(feature = "desktop"))]
pub fn artifact_search(query: String) -> Vec<ArtifactSearchHit> {
    artifact_search_sync(query)
}

fn artifact_search_sync(query: String) -> Vec<ArtifactSearchHit> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<ArtifactSearchHit> = Vec::new();
    // 全量遍历所有 conversations/<id>/outputs 并逐个读文本文件:产物攒多后可拖到数十秒,
    // 逼近/超过命令超时上限(历史「artifact_search 挂死」同族隐患)。给一个墙钟预算,超时即以
    // 已收集的部分命中收工,把最坏代价压成有界——宁可少召回,不可拖垮调用方。
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(ARTIFACT_SEARCH_BUDGET_SECS);
    let mut checked: u32 = 0;
    'roots: for root in conversation_roots() {
        if !root.exists() {
            continue;
        }
        for w in WalkDir::new(&root).into_iter().flatten() {
            // 每扫 128 个条目查一次预算(Instant 很廉价,分批只为省到极致);超时跳出所有根。
            checked = checked.wrapping_add(1);
            if checked % 128 == 0 && std::time::Instant::now() >= deadline {
                break 'roots;
            }
            if !w.file_type().is_file() {
                continue;
            }
            let p = w.path();
            // 仅 conversations/<id>/outputs/** 下的文件
            let rel = match p.strip_prefix(&root) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let comps: Vec<String> = rel
                .components()
                .filter_map(|c| c.as_os_str().to_str().map(|s| s.to_string()))
                .collect();
            // 期望 [<id>, "outputs", ...]
            if comps.len() < 3 || comps[1] != "outputs" {
                continue;
            }
            let conversation_id = comps[0].clone();
            let name = p
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if name.starts_with('.') {
                continue;
            }
            let ext = p
                .extension()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let kind = classify_ext(&ext);
            let meta = match w.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);

            let mut score = 0;
            let mut snippet = String::new();
            if name.to_lowercase().contains(&q) {
                score += 10;
            }
            // 文本类才读正文匹配 (限大小, 防卡)
            if matches!(kind, "text" | "markdown" | "html" | "svg") && meta.len() < 512 * 1024 {
                if let Ok(body) = std::fs::read_to_string(p) {
                    let lower = body.to_lowercase();
                    if let Some(pos) = lower.find(&q) {
                        score += 2;
                        let start = body[..pos]
                            .char_indices()
                            .rev()
                            .take(40)
                            .last()
                            .map(|(i, _)| i)
                            .unwrap_or(0);
                        let end = (pos + q.len() + 60).min(body.len());
                        let mut e = end;
                        while e < body.len() && !body.is_char_boundary(e) {
                            e += 1;
                        }
                        snippet = body[start..e].replace('\n', " ").trim().to_string();
                    }
                }
            }
            if score > 0 {
                hits.push(ArtifactSearchHit {
                    path: p.to_string_lossy().replace('\\', "/"),
                    name,
                    kind: kind.to_string(),
                    conversation_id,
                    snippet,
                    modified,
                    score,
                });
            }
        }
    }
    hits.sort_by(|a, b| b.score.cmp(&a.score).then(b.modified.cmp(&a.modified)));
    hits.truncate(50);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_artifacts_parses_marker_and_strips_body() {
        let content = "已生成报告。\n\n<!--POLARIS_ARTIFACTS:[\"D:/a/r.html\",\"D:/a/r.md\"]-->";
        let (clean, paths) = split_artifacts(content);
        assert_eq!(clean, "已生成报告。");
        assert_eq!(
            paths,
            vec!["D:/a/r.html".to_string(), "D:/a/r.md".to_string()]
        );
    }

    #[test]
    fn displayable_artifact_whitelists_common_formats_only() {
        // 常见成品: 进对话框
        for p in [
            "D:/a/report.html",
            "D:/a/读书笔记.MD",
            "D:/a/v.mp4",
            "D:/a/讲解.mp3",
            "D:/a/图.png",
            "D:/a/слайды.pptx",
            "D:/a/简历.docx",
            "D:/a/r.pdf",
        ] {
            assert!(is_displayable_artifact(p), "{p} 应展示");
        }
        // 脚本 / 配置 / 无后缀等中间产物: 不进对话框
        for p in [
            "D:/a/build.py",
            "D:/a/index.js",
            "D:/a/package.json",
            "D:/a/run.sh",
            "D:/a/Makefile",
            "D:/a/data.sqlite",
            "D:/a/启动应用.bat",
        ] {
            assert!(!is_displayable_artifact(p), "{p} 不应展示");
        }
    }

    #[test]
    fn folder_artifact_repr_appends_single_trailing_slash() {
        assert_eq!(
            folder_artifact_repr(Path::new("D:\\a\\myapp")),
            "D:/a/myapp/"
        );
    }

    #[test]
    fn split_artifacts_no_marker_returns_trimmed_body() {
        let (clean, paths) = split_artifacts("  普通回答  ");
        assert_eq!(clean, "普通回答");
        assert!(paths.is_empty());
    }

    #[test]
    fn split_artifacts_malformed_marker_is_safe() {
        // 有前缀但没有闭合 --> : 不应 panic, 当作无产物处理
        let (clean, paths) = split_artifacts("x<!--POLARIS_ARTIFACTS:[\"a\"");
        assert!(paths.is_empty());
        assert!(clean.contains("POLARIS_ARTIFACTS"));
    }
}
