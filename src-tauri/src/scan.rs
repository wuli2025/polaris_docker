//! 板块 · 全盘资源归集 — 跨平台扫描 + 启发式预览 + 价值评分
//!
//! 设计依据: 桌面 PRD「全盘资源归集 v4」。
//! - 扫描全程**只读**: 只 list + 读文件头, 绝不删改源文件。
//! - 跨平台扫描根 (Win 盘符 / mac 家目录+Volumes / Docker 挂载卷)。
//! - 黑名单剪枝(系统/缓存/依赖/敏感目录) + 白名单后缀 + 价值评分 + 启发式「大概内容」。
//! - 归档不在本模块: 复用 kb::kb_upload_files 把选中文件复制入资源库 raw/;
//!   「摄入核心层」= 归档后再跑 kb::kb_compile(构建知识网)。
//!
//! 本模块零外部新依赖(std + walkdir,后者 kb.rs 已在用)。

use serde::Serialize;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

// ───────────────────────── 数据结构 ─────────────────────────

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ScanRoot {
    /// 稳定标识(也是扫描时传回的 path 来源)
    pub id: String,
    /// 显示名,如「桌面」「C: 盘」
    pub label: String,
    /// 绝对路径
    pub path: String,
    /// desktop | drive | home | volume | mounted
    pub kind: String,
    /// 默认是否勾选
    pub default_on: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ScanRow {
    /// 稳定 id(路径哈希)
    pub id: String,
    pub path: String,
    pub name: String,
    pub ext: String,
    /// doc | sheet | slide | data | image | audio | video | archive | code | text | other
    pub kind: String,
    /// 大概内容(启发式;binary 类先给占位,待「智能摘要」增强)
    pub preview: String,
    pub size: u64,
    pub size_h: String,
    /// 修改时间(unix 秒)
    pub mtime: i64,
    /// 价值评分 1-5
    pub score: u8,
    /// 建议去向: resource | resource+core | skip
    pub suggest: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub rows: Vec<ScanRow>,
    /// 遍历过的文件总数(含被跳过的)
    pub total_seen: u64,
    /// 命中(进表)的资源数
    pub hit: usize,
    /// 因不在白名单/太小等被跳过的数
    pub skipped: u64,
    /// 是否因达到上限被截断
    pub truncated: bool,
}

// ───────────────────────── 扫描根(跨平台) ─────────────────────────

fn home_dir() -> Option<std::path::PathBuf> {
    directories::UserDirs::new().map(|u| u.home_dir().to_path_buf())
}

fn roots_impl() -> Vec<ScanRoot> {
    let mut out = Vec::new();

    #[cfg(target_os = "windows")]
    {
        // 桌面
        if let Some(home) = home_dir() {
            let desk = home.join("Desktop");
            if desk.exists() {
                out.push(ScanRoot {
                    id: desk.to_string_lossy().to_string(),
                    label: "桌面".into(),
                    path: desk.to_string_lossy().to_string(),
                    kind: "desktop".into(),
                    default_on: true,
                });
            }
        }
        // 盘符 A-Z
        for c in b'A'..=b'Z' {
            let drive = format!("{}:\\", c as char);
            if Path::new(&drive).exists() {
                out.push(ScanRoot {
                    id: drive.clone(),
                    label: format!("{}: 盘", c as char),
                    path: drive.clone(),
                    kind: "drive".into(),
                    // C/D 默认勾,其余(可能是 U 盘/光驱)默认不勾
                    default_on: c == b'C' || c == b'D',
                });
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = home_dir() {
            for (sub, label, on) in [
                ("Desktop", "桌面", true),
                ("Documents", "文稿", true),
                ("Downloads", "下载", true),
            ] {
                let p = home.join(sub);
                if p.exists() {
                    out.push(ScanRoot {
                        id: p.to_string_lossy().to_string(),
                        label: label.into(),
                        path: p.to_string_lossy().to_string(),
                        kind: "home".into(),
                        default_on: on,
                    });
                }
            }
        }
        // 外置卷
        if let Ok(rd) = fs::read_dir("/Volumes") {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    let name = e.file_name().to_string_lossy().to_string();
                    out.push(ScanRoot {
                        id: p.to_string_lossy().to_string(),
                        label: format!("卷 · {name}"),
                        path: p.to_string_lossy().to_string(),
                        kind: "volume".into(),
                        default_on: false,
                    });
                }
            }
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Linux / Docker: 扫挂载进来的目录。容器看不到宿主盘,
        // 只能扫被 bind mount 进来的卷(见 NAS 部署: /root/Polaris/nas 等)。
        if let Some(home) = home_dir() {
            out.push(ScanRoot {
                id: home.to_string_lossy().to_string(),
                label: "工作区(HOME)".into(),
                path: home.to_string_lossy().to_string(),
                kind: "home".into(),
                default_on: true,
            });
        }
        for cand in ["/root/Polaris/nas", "/data", "/mnt", "/volume1", "/host"] {
            if Path::new(cand).is_dir() {
                out.push(ScanRoot {
                    id: cand.into(),
                    label: format!("挂载 · {cand}"),
                    path: cand.into(),
                    kind: "mounted".into(),
                    default_on: cand == "/root/Polaris/nas",
                });
            }
        }
    }

    out
}

// ───────────────────────── 黑/白名单 ─────────────────────────

/// 目录名(小写)命中即整棵剪掉。系统 / 缓存 / 依赖 / 敏感。
fn is_pruned_dir(name: &str) -> bool {
    // 以 . 或 @ 开头的目录一律跳过(配置/缓存/群晖系统目录,如 .git .ssh @appdata)
    if name.starts_with('.') || name.starts_with('@') || name.starts_with('$') {
        return true;
    }
    let n = name.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "windows"
            | "program files"
            | "program files (x86)"
            | "programdata"
            | "system volume information"
            | "recovery"
            | "appdata"
            | "node_modules"
            | "target"
            | "dist"
            | "build"
            | "__pycache__"
            | "venv"
            | "site-packages"
            | "vendor"
            | "obj"
            | "bin"
            | "anaconda3"
            | "miniconda3"
            | "library"        // mac ~/Library
            | "applications"
            | "polariskb"      // 别把知识库自己扫进来
    )
}

/// 后缀 → 类型;不在表内返回 None(跳过)。
fn classify_ext(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "pdf" | "doc" | "docx" | "rtf" | "odt" | "pages" => "doc",
        "md" | "markdown" | "txt" => "text",
        "xls" | "xlsx" | "csv" | "tsv" | "ods" | "numbers" => "sheet",
        "ppt" | "pptx" | "key" | "odp" => "slide",
        "json" | "xml" | "yaml" | "yml" | "parquet" | "sqlite" | "db" | "ndjson" => "data",
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "tiff" | "tif" | "bmp" | "svg" => "image",
        "mp3" | "wav" | "flac" | "m4a" | "aac" | "ogg" => "audio",
        "mp4" | "mov" | "mkv" | "avi" | "webm" | "m4v" => "video",
        "zip" | "7z" | "rar" | "tar" | "gz" | "bz2" | "xz" => "archive",
        "py" | "js" | "ts" | "tsx" | "jsx" | "rs" | "go" | "java" | "c" | "cpp" | "h" | "hpp"
        | "sh" | "html" | "css" | "vue" | "sql" | "ipynb" | "toml" | "ini" => "code",
        _ => return None,
    })
}

/// 文本类(可直接读头部当预览)。
fn is_textual(kind: &str) -> bool {
    matches!(kind, "text" | "code" | "sheet" | "data")
        // 仅当真是文本(下方按扩展再判 csv/json/txt/md/code)
}

// ───────────────────────── 预览 / 评分 ─────────────────────────

fn human_size(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// 读文件头部若干字节(只读,不整文件入内存)。
fn read_head(path: &Path, max: usize) -> Option<String> {
    let f = fs::File::open(path).ok()?;
    let mut buf = vec![0u8; max];
    let mut h = f.take(max as u64);
    let n = h.read(&mut buf).ok()?;
    buf.truncate(n);
    Some(String::from_utf8_lossy(&buf).to_string())
}

/// 启发式「大概内容」。文本类读头部取前几行;其它给类型占位(待 AI 摘要)。
fn make_preview(path: &Path, ext: &str, kind: &str, size: u64) -> String {
    let textual = matches!(ext, "txt" | "md" | "markdown" | "csv" | "tsv" | "json" | "ndjson")
        || (kind == "code");
    if textual {
        if let Some(head) = read_head(path, 4096) {
            if ext == "csv" || ext == "tsv" {
                // 表头一行 + 列数
                if let Some(first) = head.lines().find(|l| !l.trim().is_empty()) {
                    let sep = if ext == "tsv" { '\t' } else { ',' };
                    let cols = first.split(sep).count();
                    let h: String = first.chars().take(80).collect();
                    return format!("{cols} 列:{h}…");
                }
            }
            let snippet: String = head
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .take(3)
                .collect::<Vec<_>>()
                .join(" · ");
            let snippet = snippet.replace(['\u{0}', '\r'], "");
            let snippet: String = snippet.chars().take(140).collect();
            if !snippet.trim().is_empty() {
                return snippet;
            }
        }
    }
    // binary / 不可直读 → 类型占位
    let label = match kind {
        "doc" => "文档",
        "slide" => "演示文稿",
        "image" => "图片",
        "audio" => "音频",
        "video" => "视频",
        "archive" => "压缩包",
        "data" => "数据文件",
        _ => "文件",
    };
    format!("{label} · {}（点「智能摘要」识别内容）", human_size(size))
}

/// 价值评分 1-5(位置 / 时间 / 命名 / 体积)。
fn score_row(path: &Path, name: &str, kind: &str, size: u64, mtime: i64, now: i64) -> u8 {
    let mut s: i32 = 3;
    let lower = path.to_string_lossy().to_ascii_lowercase();
    // 位置:常见有用目录加分
    if ["desktop", "documents", "downloads", "文档", "桌面", "工作", "项目", "report", "report"]
        .iter()
        .any(|k| lower.contains(k))
    {
        s += 1;
    }
    // 时效:近半年 +1,超三年 -1
    let age = now - mtime;
    if age >= 0 && age < 60 * 60 * 24 * 180 {
        s += 1;
    } else if age > 60 * 60 * 24 * 365 * 3 {
        s -= 1;
    }
    // 命名噪音
    let nl = name.to_ascii_lowercase();
    if ["新建", "未命名", "untitled", "tmp", "temp", "copy", "副本", "~$", "新建文本"]
        .iter()
        .any(|k| nl.contains(k))
    {
        s -= 2;
    }
    // 体积
    if size == 0 {
        s -= 2;
    }
    // 文档/演示天然偏有用
    if matches!(kind, "doc" | "slide") {
        s += 1;
    }
    s.clamp(1, 5) as u8
}

fn suggest_for(score: u8, kind: &str) -> &'static str {
    if score <= 2 {
        "skip"
    } else if score >= 4 && matches!(kind, "doc" | "slide" | "text") {
        "resource+core"
    } else {
        "resource"
    }
}

/// 路径稳定 id(简单哈希)。
fn path_id(path: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    format!("{:x}", h.finish())
}

// ───────────────────────── 命令 ─────────────────────────

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn scan_roots() -> Vec<ScanRoot> {
    roots_impl()
}

/// 扫描给定根下的有用资源。只读;返回多维表格行。
/// max: 命中上限(默认 20000),达到即截断,防止极端目录拖死。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn scan_resources(roots: Vec<String>, max: Option<usize>) -> Result<ScanReport, String> {
    if roots.is_empty() {
        return Err("未选择扫描范围".into());
    }
    let cap = max.unwrap_or(20_000).clamp(100, 200_000);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mut rows: Vec<ScanRow> = Vec::new();
    let mut total_seen: u64 = 0;
    let mut skipped: u64 = 0;
    let mut truncated = false;

    'outer: for root in &roots {
        let rp = Path::new(root);
        if !rp.exists() {
            continue;
        }
        let walker = WalkDir::new(rp)
            .max_depth(14)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                // 目录:命中黑名单则整棵剪掉
                if e.file_type().is_dir() && e.depth() > 0 {
                    let name = e.file_name().to_string_lossy();
                    return !is_pruned_dir(&name);
                }
                true
            });

        for entry in walker.filter_map(|r| r.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            total_seen += 1;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            let kind = match classify_ext(&ext) {
                Some(k) => k,
                None => {
                    skipped += 1;
                    continue;
                }
            };
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            let size = meta.len();
            // 过滤明显的图标/缩略图碎图
            if kind == "image" && size < 20 * 1024 {
                skipped += 1;
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);

            let preview = make_preview(path, &ext, kind, size);
            let score = score_row(path, &name, kind, size, mtime, now);
            let suggest = suggest_for(score, kind);
            let p = path.to_string_lossy().to_string();

            rows.push(ScanRow {
                id: path_id(&p),
                path: p,
                name,
                ext,
                kind: kind.to_string(),
                preview,
                size,
                size_h: human_size(size),
                mtime,
                score,
                suggest: suggest.to_string(),
            });

            if rows.len() >= cap {
                truncated = true;
                break 'outer;
            }
        }
    }

    // 默认按价值降序、再按修改时间降序
    rows.sort_by(|a, b| b.score.cmp(&a.score).then(b.mtime.cmp(&a.mtime)));
    let hit = rows.len();
    let _ = is_textual; // 预留:后续真正分流文本/二进制预览

    Ok(ScanReport {
        rows,
        total_seen,
        hit,
        skipped,
        truncated,
    })
}
