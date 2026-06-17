//! 盘点引擎(L1a)—— 多线程并行全盘扫描 → SQLite。
//!
//! PRD v5 §7「P0.5 盘点+L1a:首小时全盘可搜」。设计:
//! - N 个 walker 线程(共享目录栈,work-stealing)只做 read_dir + stat,吃满多核;
//! - 1 个 writer 线程独占写连接,2000 行一个事务批量落库(SQLite 写入瓶颈在事务数);
//! - 「seen 代际」机制:全量重扫后自动清掉已消失文件(及其 chunks),幂等可重入;
//! - mtime/size 没变的文件保留 chunked 标记 → 重扫不会废掉已建好的向量索引。

use super::{cancelled, open_db, worker_count, FlagGuard, CANCEL, SCANNING};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};

#[cfg(feature = "desktop")]
use tauri::{AppHandle, Emitter};
#[cfg(not(feature = "desktop"))]
use crate::host::AppHandle;

// ───────────────────────── 文件分类 ─────────────────────────

const TEXT_EXTS: &[&str] = &[
    "md", "txt", "rs", "py", "js", "ts", "tsx", "jsx", "mjs", "json", "jsonl", "yaml", "yml",
    "toml", "html", "htm", "css", "csv", "tsv", "log", "xml", "ini", "cfg", "conf", "sh", "ps1",
    "bat", "cmd", "sql", "vue", "go", "java", "c", "cpp", "h", "hpp", "rb", "php", "srt", "vtt",
    "tex", "rst", "org",
];
const DOC_EXTS: &[&str] = &["pdf", "docx", "doc", "pptx", "ppt", "xlsx", "xls", "epub", "mobi"];
const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "heic", "svg", "tif", "tiff", "raw", "cr2", "nef",
];
const AUDIO_EXTS: &[&str] = &["mp3", "wav", "flac", "m4a", "aac", "ogg", "wma", "opus", "amr"];
const VIDEO_EXTS: &[&str] = &["mp4", "mkv", "mov", "avi", "wmv", "flv", "webm", "m4v", "mpg", "mpeg"];
const ARCHIVE_EXTS: &[&str] = &["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "iso", "dmg"];

pub(crate) fn classify(ext: &str) -> &'static str {
    let e = ext.to_ascii_lowercase();
    let e = e.as_str();
    if TEXT_EXTS.contains(&e) {
        "text"
    } else if DOC_EXTS.contains(&e) {
        "doc"
    } else if IMAGE_EXTS.contains(&e) {
        "image"
    } else if AUDIO_EXTS.contains(&e) {
        "audio"
    } else if VIDEO_EXTS.contains(&e) {
        "video"
    } else if ARCHIVE_EXTS.contains(&e) {
        "archive"
    } else {
        "other"
    }
}

// ───────────────────────── 按「语言」归类 ─────────────────────────
//
// 用户诉求:文件归类要「按语言」(编程语言 / 自然语言),不要按应用名或粗粒度类型。
// 三层判定:① 代码/标记类 → 编程语言(扩展名精确判定,零 IO);② 媒体/压缩 → 大类;
// ③ 文稿(md/txt/doc…)→ 自然语言(读文件头按 CJK 占比嗅探,放在回填里做,避免拖慢盘点)。

/// 扩展名 → 编程语言/标记语言(「按语言归类」的精确信号,零 IO)。None = 非代码类。
pub(crate) fn prog_lang(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "py" | "pyw" | "pyi" | "ipynb" => "Python",
        "rs" => "Rust",
        "js" | "mjs" | "cjs" => "JavaScript",
        "ts" => "TypeScript",
        "tsx" | "jsx" => "React/JSX",
        "vue" => "Vue",
        "go" => "Go",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "c" | "h" => "C",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "C++",
        "cs" => "C#",
        "rb" => "Ruby",
        "php" => "PHP",
        "swift" => "Swift",
        "scala" => "Scala",
        "sh" | "bash" | "zsh" => "Shell",
        "ps1" | "psm1" | "psd1" => "PowerShell",
        "bat" | "cmd" => "Batch",
        "sql" => "SQL",
        "r" => "R",
        "lua" => "Lua",
        "dart" => "Dart",
        "pl" | "pm" => "Perl",
        "html" | "htm" => "HTML",
        "css" | "scss" | "sass" | "less" => "CSS/样式",
        "json" | "jsonl" | "ndjson" => "JSON",
        "yaml" | "yml" => "YAML",
        "toml" | "ini" | "cfg" | "conf" => "配置",
        "xml" => "XML",
        _ => return None,
    })
}

/// kind → 媒体/压缩大类(非代码、非文稿的语言归类兜底)。None = 文稿类(交自然语言嗅探)。
fn media_lang(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "image" => "图片",
        "video" => "视频",
        "audio" => "音频",
        "archive" => "压缩包",
        "other" => "其他文件",
        _ => return None, // text / doc → 文稿,按自然语言归
    })
}

/// 全部受支持的代码/标记扩展名(grid 按语言反查用)。与 [`prog_lang`] 的 match 同源。
pub(crate) const CODE_EXTS: &[&str] = &[
    "py", "pyw", "pyi", "ipynb", "rs", "js", "mjs", "cjs", "ts", "tsx", "jsx", "vue", "go", "java",
    "kt", "kts", "c", "h", "cpp", "cc", "cxx", "hpp", "hh", "hxx", "cs", "rb", "php", "swift",
    "scala", "sh", "bash", "zsh", "ps1", "psm1", "psd1", "bat", "cmd", "sql", "r", "lua", "dart",
    "pl", "pm", "html", "htm", "css", "scss", "sass", "less", "json", "jsonl", "ndjson", "yaml",
    "yml", "toml", "ini", "cfg", "conf", "xml",
];

/// 某编程/标记语言 → 对应扩展名集合(grid 按语言过滤;代码语言由扩展名确定,不依赖回填)。
/// 空 = 该标签不是代码语言(改按 lang 列 / kind 过滤)。
pub(crate) fn exts_for_lang(label: &str) -> Vec<&'static str> {
    CODE_EXTS.iter().copied().filter(|e| prog_lang(e) == Some(label)).collect()
}

/// 媒体/压缩语言标签 → 对应 kind(grid 过滤用)。None = 非媒体标签。
pub(crate) fn kind_for_media_lang(label: &str) -> Option<&'static str> {
    Some(match label {
        "图片" => "image",
        "视频" => "video",
        "音频" => "audio",
        "压缩包" => "archive",
        "其他文件" => "other",
        _ => return None,
    })
}

/// 盘点时即可定的语言(零 IO):代码看扩展名、媒体看 kind;文稿返回 ""(留待回填读头嗅探)。
pub(crate) fn quick_lang(ext: &str, kind: &str) -> String {
    if let Some(l) = prog_lang(ext) {
        return l.to_string();
    }
    media_lang(kind).unwrap_or("").to_string()
}

/// 读文件头嗅探自然语言:CJK 占比 ≥10% → 中文;拉丁字母为主 → 英文;否则其他语言。
pub(crate) fn natural_lang(sample: &str) -> &'static str {
    let (mut cjk, mut latin, mut letters) = (0usize, 0usize, 0usize);
    for c in sample.chars().take(8000) {
        if ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{3400}'..='\u{4dbf}').contains(&c) {
            cjk += 1;
            letters += 1;
        } else if c.is_ascii_alphabetic() {
            latin += 1;
            letters += 1;
        } else if c.is_alphabetic() {
            letters += 1;
        }
    }
    if letters < 8 {
        return "其他语种";
    }
    if cjk as f32 / letters as f32 >= 0.10 {
        "中文"
    } else if latin as f32 / letters as f32 >= 0.6 {
        "英文"
    } else {
        "其他语种"
    }
}

/// 读文件头(≤16KB)做文本采样;二进制(含 NUL)或不可读返回 None。
pub(crate) fn read_head_sample(abs: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(abs).ok()?;
    let mut buf = vec![0u8; 16 * 1024];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    if buf.iter().take(1024).any(|&b| b == 0) {
        return None; // 二进制(含改名的伪文本 / docx/pdf 等容器)
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// 扫描时跳过的目录名(系统/缓存/版本仓;@eaDir、#recycle 是群晖特产)。
const SKIP_DIRS: &[&str] = &[
    ".git", ".svn", "node_modules", "target", ".fable", ".history", ".quarantine", "__pycache__",
    ".venv", "venv", "$RECYCLE.BIN", "System Volume Information", ".Trash", ".Trashes",
    "@eaDir", "#recycle", "#snapshot", ".DocumentRevisions-V100", ".Spotlight-V100",
];

fn skip_dir(name: &str) -> bool {
    // 群晖/NAS 系统目录一律以 `@` 或 `#` 打头(@eaDir 缩略图、@docker 层、@database、
    // @appstore、#recycle、#snapshot…),用户数据从不放这里 → 整盘盘点时跳过,免噪音免爆量。
    if name.starts_with('@') || name.starts_with('#') {
        return true;
    }
    SKIP_DIRS.iter().any(|s| s.eq_ignore_ascii_case(name))
}

/// 盘点支持「不只盘知识库,也能盘整盘/桌面/其它文件夹」后,扫描会触达 C:/D: 这类系统盘。
/// 这些操作系统/缓存/依赖目录用户数据从不放、且体量巨大 → 扫文件夹和盘点时都整棵跳过,
/// 避免把 Windows、Program Files 卷进文件库。在 [`skip_dir`] 基础上再加一层系统目录黑名单。
const SCAN_EXTRA_SKIP: &[&str] = &[
    "windows", "program files", "program files (x86)", "programdata", "perflogs", "msocache",
    "$recycle.bin", "system volume information", "recovery", "appdata", "$windows.~bs",
    "$windows.~ws", "intel", "amd", "nvidia", "site-packages", "anaconda3", "miniconda3",
    "library", "applications", "boot", "proc", "sys", "dev",
];

fn skip_dir_scan(name: &str) -> bool {
    if skip_dir(name) {
        return true;
    }
    if name.starts_with('$') {
        return true;
    }
    let low = name.to_ascii_lowercase();
    SCAN_EXTRA_SKIP.iter().any(|s| *s == low)
}

/// 盘 `parent` 时是否真能扫到 `child`:child 在 parent 之内,且从 parent 到 child 的
/// 每一段目录名都不会被 [`skip_dir_scan`] 剪掉(否则 walker 会在中途剪枝、到不了 child)。
/// 嵌套根去重用它:被剪枝挡住的子根不算「已覆盖」,须独立保留(典型=appdata 内的下载目录)。
fn covered_by(parent: &str, child: &str) -> bool {
    if child == parent {
        return true;
    }
    let pn = parent.trim_end_matches(['/', '\\']);
    let rest = match child
        .strip_prefix(&format!("{pn}/"))
        .or_else(|| child.strip_prefix(&format!("{pn}\\")))
    {
        Some(s) => s,
        None => return false,
    };
    !rest
        .split(['/', '\\'])
        .any(|seg| !seg.is_empty() && skip_dir_scan(seg))
}

// ───────────────────────── 扫描核心(三壳共用)─────────────────────────

struct FileRow {
    relpath: String,
    name: String,
    ext: String,
    kind: &'static str,
    /// 「按语言归类」标签:代码=编程语言、媒体=大类;文稿盘点时为 ""(回填读头嗅探自然语言)。
    lang: String,
    size: u64,
    mtime: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanSummary {
    pub root: String,
    pub files: u64,
    pub bytes: u64,
    pub removed: u64,
    pub seconds: f64,
    pub workers: usize,
}

/// 同步全量扫描一个根(CLI 直接调;桌面/Docker 由 `fable_inventory_start` 包后台线程)。
/// `progress(files, bytes)` 每 ~5000 个文件回调一次。
/// `exclude` = 用户在「扫描」步骤里取消勾选的文件夹绝对路径集合,整棵跳过(空集=全盘点)。
pub fn scan_root(
    root: &str,
    exclude: &HashSet<String>,
    progress: &(dyn Fn(u64, u64) + Sync),
) -> Result<ScanSummary, String> {
    let root_path = PathBuf::from(root);
    if !root_path.is_dir() {
        return Err(format!("根目录不存在或不是目录: {root}"));
    }
    let root_canon = dunce_canonical(&root_path);
    let started = std::time::Instant::now();
    let gen = chrono::Local::now().timestamp_millis();

    // root 行就位
    let conn = open_db()?;
    conn.execute(
        "INSERT INTO roots(path) VALUES(?1) ON CONFLICT(path) DO NOTHING",
        [&root_canon],
    )
    .map_err(|e| e.to_string())?;
    let root_id: i64 = conn
        .query_row("SELECT id FROM roots WHERE path=?1", [&root_canon], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    drop(conn);

    // walker 线程池:共享目录栈 + pending 计数(栈空且 pending=0 才算扫完)
    let (tx, rx) = mpsc::channel::<FileRow>();
    let stack = Mutex::new(vec![root_path.clone()]);
    let pending = AtomicUsize::new(1);
    let n_files = AtomicU64::new(0);
    let n_bytes = AtomicU64::new(0);
    let workers = worker_count();

    // writer 线程:独占连接,批量事务
    let writer = {
        std::thread::spawn(move || -> Result<(), String> {
            let conn = open_db()?;
            let mut batch: Vec<FileRow> = Vec::with_capacity(2048);
            let flush = |conn: &rusqlite::Connection, batch: &mut Vec<FileRow>| -> Result<(), String> {
                if batch.is_empty() {
                    return Ok(());
                }
                conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
                {
                    let mut stmt = conn
                        .prepare_cached(
                            "INSERT INTO files(root_id,relpath,name,ext,kind,lang,size,mtime,chunked,seen)
                             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,0,?9)
                             ON CONFLICT(root_id,relpath) DO UPDATE SET
                               name=excluded.name, ext=excluded.ext, kind=excluded.kind,
                               -- 文稿回填得到的自然语言(中文/英文)别被重扫的 '' 覆盖:仅当新值非空才更新。
                               lang=CASE WHEN excluded.lang!='' THEN excluded.lang ELSE files.lang END,
                               chunked=CASE WHEN files.mtime=excluded.mtime AND files.size=excluded.size
                                            THEN files.chunked ELSE 0 END,
                               ftsed=CASE WHEN files.mtime=excluded.mtime AND files.size=excluded.size
                                          THEN files.ftsed ELSE 0 END,
                               size=excluded.size, mtime=excluded.mtime, seen=excluded.seen",
                        )
                        .map_err(|e| e.to_string())?;
                    for row in batch.drain(..) {
                        stmt.execute(rusqlite::params![
                            root_id, row.relpath, row.name, row.ext, row.kind, row.lang,
                            row.size as i64, row.mtime, gen
                        ])
                        .map_err(|e| e.to_string())?;
                    }
                }
                conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
                Ok(())
            };
            while let Ok(row) = rx.recv() {
                batch.push(row);
                if batch.len() >= 2048 {
                    flush(&conn, &mut batch)?;
                }
            }
            flush(&conn, &mut batch)?;
            Ok(())
        })
    };

    std::thread::scope(|s| {
        for _ in 0..workers {
            let tx = tx.clone();
            let (stack, pending, n_files, n_bytes) = (&stack, &pending, &n_files, &n_bytes);
            let root_path = &root_path;
            let exclude = &exclude;
            s.spawn(move || {
                loop {
                    if cancelled() {
                        break;
                    }
                    let dir = { stack.lock().unwrap().pop() };
                    let Some(dir) = dir else {
                        if pending.load(Ordering::SeqCst) == 0 {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        continue;
                    };
                    if let Ok(rd) = std::fs::read_dir(&dir) {
                        for entry in rd.flatten() {
                            let Ok(ft) = entry.file_type() else { continue };
                            if ft.is_symlink() {
                                continue;
                            }
                            // 非 UTF-8 名(Linux/Docker 上的 GBK 中文名)解回中文,避免乱码 �。
                            let name = super::decode_fs(&entry.file_name());
                            if ft.is_dir() {
                                if skip_dir_scan(&name) {
                                    continue;
                                }
                                let child = entry.path();
                                // 用户在「扫描」步骤取消勾选的文件夹 → 整棵跳过。
                                if !exclude.is_empty()
                                    && exclude.contains(child.to_string_lossy().as_ref())
                                {
                                    continue;
                                }
                                pending.fetch_add(1, Ordering::SeqCst);
                                stack.lock().unwrap().push(child);
                            } else if ft.is_file() {
                                let Ok(meta) = entry.metadata() else { continue };
                                let p = entry.path();
                                let rel = p
                                    .strip_prefix(&root_path)
                                    .map(|r| super::decode_fs(r.as_os_str()).replace('\\', "/"))
                                    .unwrap_or_else(|_| name.clone());
                                let ext = p
                                    .extension()
                                    .map(|e| e.to_string_lossy().to_ascii_lowercase())
                                    .unwrap_or_default();
                                let mtime = meta
                                    .modified()
                                    .ok()
                                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                    .map(|d| d.as_secs() as i64)
                                    .unwrap_or(0);
                                let size = on_disk_size(&p, &meta);
                                let total = n_files.fetch_add(1, Ordering::Relaxed) + 1;
                                let bytes = n_bytes.fetch_add(size, Ordering::Relaxed) + size;
                                let kind = classify(&ext);
                                let _ = tx.send(FileRow {
                                    relpath: rel,
                                    name,
                                    kind,
                                    lang: quick_lang(&ext, kind), // 代码/媒体当场定;文稿留 "" 待回填
                                    ext,
                                    size,
                                    mtime,
                                });
                                if total % 5000 == 0 {
                                    progress(total, bytes);
                                }
                            }
                        }
                    }
                    pending.fetch_sub(1, Ordering::SeqCst);
                }
            });
        }
    });
    drop(tx); // 关闭通道 → writer 落完尾批退出
    writer
        .join()
        .map_err(|_| "writer 线程 panic".to_string())??;

    if cancelled() {
        return Err("已取消".into());
    }

    let files = n_files.load(Ordering::Relaxed);
    let bytes = n_bytes.load(Ordering::Relaxed);

    // 护栏:本轮扫到 0 文件 → 几乎一定是「根临时读不到」(NAS 挂载掉线 / 权限抖动 / 路径没挂上),
    // 而非用户真把整个根清空了。此时若照常跑 seen 代际删除,会把该根上一轮的全部记录连同
    // 已建好的向量一并抹掉 → 文件中心「盘点完右边一下子全没了」。故扫到 0 文件就**跳过删除、
    // 也不刷新 roots 计数**,保留上一轮已知状态;待挂载恢复后重扫自然对账。
    // (真要清空一个根:删到只剩 1 个占位文件即可触发正常代际清理。)
    let conn = open_db()?;
    let removed = if files == 0 {
        0
    } else {
        conn.execute(
            "DELETE FROM chunks WHERE file_id IN (SELECT id FROM files WHERE root_id=?1 AND seen<>?2)",
            rusqlite::params![root_id, gen],
        )
        .map_err(|e| e.to_string())?;
        // P1-2:消失文件同步清出 FTS 倒排(lex 未编入时跳过)。rowid=file_id。
        if super::lex_available(&conn) {
            conn.execute(
                "DELETE FROM lex WHERE rowid IN (SELECT id FROM files WHERE root_id=?1 AND seen<>?2)",
                rusqlite::params![root_id, gen],
            )
            .map_err(|e| e.to_string())?;
        }
        let n = conn
            .execute(
                "DELETE FROM files WHERE root_id=?1 AND seen<>?2",
                rusqlite::params![root_id, gen],
            )
            .map_err(|e| e.to_string())? as u64;
        conn.execute(
            "UPDATE roots SET scanned_at=?2, files=?3, bytes=?4 WHERE id=?1",
            rusqlite::params![root_id, gen, files as i64, bytes as i64],
        )
        .map_err(|e| e.to_string())?;
        n
    };

    Ok(ScanSummary {
        root: root_canon,
        files,
        bytes,
        removed,
        seconds: started.elapsed().as_secs_f64(),
        workers,
    })
}

/// Windows 的 canonicalize 会出 `\\?\` 前缀(已在 PPTX 审计里踩过坑),手工剥掉。
fn dunce_canonical(p: &Path) -> String {
    let c = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let s = c.to_string_lossy().into_owned();
    s.strip_prefix(r"\\?\").map(|x| x.to_string()).unwrap_or(s)
}

/// 文件「磁盘实占」字节数,而非 `metadata().len()` 报的逻辑大小。
///
/// 为什么必须用实占:稀疏文件(WSL/Docker 的 `*.vhdx`、虚拟机盘、测试用占位大文件)
/// 的逻辑大小可达声称的几十 GB,但磁盘上几乎不占空间。照逻辑大小累加会让「总量」
/// 虚高好几倍(实测一台机 D:\ 真实 371 GB 被算成 2.8 TB,光 60 个稀疏 .mkv 就虚报 2.3 TB)。
/// NTFS 压缩卷同理——实占小于逻辑。这里统一取磁盘实占,口径才与资源管理器的「占用」一致。
fn on_disk_size(path: &Path, meta: &std::fs::Metadata) -> u64 {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::GetLastError;
        use windows_sys::Win32::Storage::FileSystem::{GetCompressedFileSizeW, INVALID_FILE_SIZE};
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let mut high: u32 = 0;
        // SAFETY: wide 是以 NUL 结尾的合法宽字符串;high 是有效可写指针。
        let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
        // INVALID_FILE_SIZE(0xFFFFFFFF)既可能是出错,也可能是合法低位 → 需查 GetLastError 区分。
        if low == INVALID_FILE_SIZE {
            let err = unsafe { GetLastError() };
            if err != 0 {
                return meta.len(); // 取不到实占(网络盘/权限)→ 保守回退逻辑大小
            }
        }
        return ((high as u64) << 32) | (low as u64);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = path;
        return meta.blocks().saturating_mul(512);
    }
    #[allow(unreachable_code)]
    meta.len()
}

// ───────────────────────── 命令(后台线程 + 事件)─────────────────────────

fn emit(app: &AppHandle, payload: Value) {
    let _ = app.emit("fable:inventory", payload);
}

/// 解析「盘点哪些根」。
/// - 显式传 root → 只盘这一个;
/// - 否则:`POLARIS_INVENTORY_ROOTS`(PATH 分隔:Win 用 `;`、Unix 用 `:`)+ 约定挂载点
///   `<KB父目录>/nas`(群晖 Docker 把各 NAS 共享 bind 到这里)+ 知识库根。
///
/// 桌面版没有 nas 挂载点、也不设环境变量 → 退化成单根 = 知识库根(行为不变)。
/// 容器版能据此把 `/root/Polaris/nas/<share>` 整个挂载点一并盘点,文件中心遂能看到全 NAS。
fn inventory_roots(explicit: Option<String>) -> Vec<String> {
    if let Some(r) = explicit
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
    {
        return vec![r];
    }
    let mut roots: Vec<String> = Vec::new();
    if let Ok(v) = std::env::var("POLARIS_INVENTORY_ROOTS") {
        for p in std::env::split_paths(&v) {
            let s = p.to_string_lossy().trim().to_string();
            if !s.is_empty() {
                roots.push(s);
            }
        }
    }
    let kb = crate::kb::kb_root();
    // 约定:NAS 各共享 bind-mount 到 <KB父目录>/nas/<share>(见 docker-compose.synology)。
    if let Some(parent) = std::path::Path::new(&kb).parent() {
        let nas = parent.join("nas");
        if nas.is_dir() {
            roots.push(nas.to_string_lossy().to_string());
        }
    }
    // 始终把知识库根纳入盘点。
    if !kb.trim().is_empty() {
        roots.push(kb);
    }
    // App 数据下载/接收目录(微信/QQ/浏览器…),默认纳入盘点 —— 用户最关心的「收到的文件」
    // 大多在这里,且常埋在 Documents 深处或 appdata(整盘扫会被剪掉),故单列为默认根。
    for r in app_data_roots() {
        roots.push(r.path);
    }
    roots.sort();
    roots.dedup();
    roots
}

/// 「App 数据」下载 / 接收目录预设(浏览器下载、微信、QQ/TIM、企业微信…)。
/// 这些目录里全是用户真实下载 / 收到的文件,但常埋在 `Documents` 深处、甚至 `AppData`
/// (被 [`skip_dir_scan`] 整棵剪掉)里 → 整盘盘点极易漏。故把它们提成**默认勾选的独立根**:
/// 既「一键收下载」,又因为是**显式根**(walker 从这里起步、永不途经名为 appdata 的目录),
/// 天然绕过 appdata 黑名单 —— 这正是「对 appdata 只放行这几个已知子路径」的白名单例外。
/// 只返回真实存在的目录;同一类(版本/路径不同)给多个候选,命中即收、按路径去重。
fn app_data_roots() -> Vec<ScanRootInfo> {
    let mut out: Vec<ScanRootInfo> = Vec::new();
    let Some(home) = directories::UserDirs::new().map(|u| u.home_dir().to_path_buf()) else {
        return out;
    };
    // (相对 home 的子路径段, 显示名)。
    let candidates: &[(&[&str], &str)] = &[
        (&["Downloads"], "下载"),
        (&["Documents", "WeChat Files"], "微信文件"), // 微信 3.x
        (&["Documents", "xwechat_files"], "微信文件"), // 微信 4.x
        (&["Documents", "WeChatFiles"], "微信文件"),
        (&["Documents", "Tencent Files"], "QQ/TIM 文件"), // 含各账号的 FileRecv
        (&["Documents", "WXWork"], "企业微信文件"),
    ];
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (segs, label) in candidates {
        let mut p = home.clone();
        for s in *segs {
            p = p.join(s);
        }
        if p.is_dir() {
            let path = p.to_string_lossy().to_string();
            if seen.insert(path.clone()) {
                out.push(ScanRootInfo { path, label: (*label).to_string(), default_on: true });
            }
        }
    }
    out
}

/// 开始盘点。立即返回,进度走 `fable:inventory` 事件。
/// - `roots` = 用户在选择器里勾选的要盘点的文件夹/盘符(可以是知识库之外的任意目录);
///   缺省/空 → 退回默认(知识库根 + 约定的 NAS 挂载点)。
/// - `exclude` = 勾选范围内又被取消的子文件夹绝对路径(整棵跳过)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fable_inventory_start(
    app: AppHandle,
    roots: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
) -> Result<(), String> {
    // 显式勾选优先;没传则退回默认根集合。去重 + 只留真实目录。
    let picked: Vec<String> = roots
        .unwrap_or_default()
        .into_iter()
        .map(|r| r.trim_end_matches(['/', '\\']).to_string())
        .filter(|r| !r.is_empty())
        .collect();
    let mut roots: Vec<String> = if picked.is_empty() {
        inventory_roots(None)
    } else {
        picked
    }
    .into_iter()
    .filter(|r| std::path::Path::new(r).is_dir())
    .collect();
    roots.sort();
    roots.dedup();
    // 去掉「嵌套根」:若 B 在 A 之内**且 A 扫得到 B**,盘 A 已覆盖 B,留 A 去 B(免重复)。
    // 排序后父目录必排在子目录前,顺序扫描即可。注意「扫得到」要排除中途被剪枝的情况:
    // 例如 B = …/AppData/…/Downloads 在 A = C:\ 之内,但扫 C:\ 时 `appdata` 整棵被
    // [`skip_dir_scan`] 剪掉、根本到不了 B → 此时必须保留 B(否则下载目录又被吞没)。
    {
        let mut kept: Vec<String> = Vec::new();
        for r in roots.into_iter() {
            let inside = kept.iter().any(|k| covered_by(k, &r));
            if !inside {
                kept.push(r);
            }
        }
        roots = kept;
    }
    if roots.is_empty() {
        return Err("没有可盘点的根目录(知识库未初始化,也无可访问的挂载点)".into());
    }
    let exclude: HashSet<String> = exclude
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.trim_end_matches(['/', '\\']).to_string())
        .filter(|p| !p.is_empty())
        .collect();
    let Some(scan_guard) = FlagGuard::acquire(&SCANNING) else {
        return Err("盘点已在进行中".into());
    };
    CANCEL.store(false, Ordering::SeqCst);
    std::thread::spawn(move || {
        // 守卫 move 进线程:正常结束或 panic 栈展开都会释放 SCANNING 闸(防永久锁死)。
        let _scan_guard = scan_guard;
        // 多根串行盘点;进度按「已盘过的根」累加,前端看到的是全量计数。
        let mut acc_files = 0u64;
        let mut acc_bytes = 0u64;
        let mut acc_removed = 0u64;
        let mut acc_secs = 0.0f64;
        let mut workers = 0usize;
        let mut last_err: Option<String> = None;
        for r in &roots {
            if cancelled() {
                break;
            }
            let app_p = app.clone();
            let base_f = acc_files;
            let base_b = acc_bytes;
            match scan_root(r, &exclude, &move |files, bytes| {
                emit(
                    &app_p,
                    json!({ "kind": "progress", "files": base_f + files, "bytes": base_b + bytes }),
                );
            }) {
                Ok(s) => {
                    acc_files += s.files;
                    acc_bytes += s.bytes;
                    acc_removed += s.removed;
                    acc_secs += s.seconds;
                    workers = s.workers;
                }
                Err(e) => last_err = Some(e),
            }
        }
        if cancelled() {
            emit(&app, json!({ "kind": "error", "message": "已取消" }));
        } else if acc_files == 0 {
            emit(
                &app,
                json!({ "kind": "error", "message": last_err.unwrap_or_else(|| "未扫描到任何文件".into()) }),
            );
        } else {
            emit(
                &app,
                json!({
                    "kind": "done", "files": acc_files, "bytes": acc_bytes,
                    "removed": acc_removed, "seconds": acc_secs, "workers": workers,
                    "roots": roots.len(),
                }),
            );
        }
    });
    Ok(())
}

// ───────────────────────── 扫描文件夹(盘点 = 扫描 + 选目录)─────────────────────────
//
// 「盘点」点开后先扫一眼文件夹结构(只读目录项、不读内容,秒级),让用户勾选要盘点的文件夹
// 再开始建库。范围**不局限于知识库**:除了知识库根 + NAS 挂载点(默认勾上),还会列出本机的
// 盘符 / 桌面 / 外置卷(默认不勾,用户按需勾选),于是知识库之外的任意文件夹也能盘进文件库。
// 设计:初次只列「根 + 第一层子目录」,更深层在用户点开时按需懒加载(fable_scan_folder_children),
// 于是 C/D 盘也能一层层点进任意深度而不必一次扫全盘。每个文件夹给直属文件数 + 是否还有更深子目录
// (前端据此显示展开箭头);各文件夹的「递归总大小」由 fable_folder_size 限并发地按需算。
// 系统/缓存目录(Windows、Program Files…)用 [`skip_dir_scan`] 整棵跳过。

/// 第一层文件夹总数上限(超出截断,前端提示「列表已截断」)。更深层按需懒加载,不受此限。
const FOLDER_SCAN_CAP: usize = 5000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanRootInfo {
    /// 根绝对路径(也是 path / parent / root 字段的同源串)。
    pub path: String,
    /// 显示名(知识库 / C: 盘 / 桌面 / 挂载点…)。
    pub label: String,
    /// 默认是否勾选(知识库 + NAS = true;盘符/桌面 = false,用户按需勾)。
    pub default_on: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderNode {
    /// 绝对路径(与盘点 walker 看到的 `entry.path()` 同源 → 可直接当 root/exclude 用)。
    pub path: String,
    /// 父目录绝对路径(顶层文件夹的父 = 所属根)。
    pub parent: String,
    /// 显示名(末段)。
    pub name: String,
    /// 所属根的绝对路径。
    pub root: String,
    /// 相对根的深度(1=顶层)。
    pub depth: usize,
    /// 该文件夹直属文件数(不含子目录)。
    pub files: u64,
    /// 是否还有更深的(未被跳过的)子目录 → 前端可展开。
    pub has_children: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderScan {
    pub roots: Vec<ScanRootInfo>,
    pub folders: Vec<FolderNode>,
    pub truncated: bool,
}

/// 盘点可选的全部根:知识库根 + NAS 挂载点(默认勾)+ 本机盘符/桌面/外置卷(默认不勾)。
fn scan_root_candidates(explicit: Option<String>) -> Vec<ScanRootInfo> {
    // 显式指定一个根 → 只扫它(默认勾上)。
    if let Some(r) = explicit.map(|r| r.trim().to_string()).filter(|r| !r.is_empty()) {
        let label = Path::new(&r)
            .file_name()
            .map(super::decode_fs)
            .unwrap_or_else(|| r.clone());
        return vec![ScanRootInfo { path: r, label, default_on: true }];
    }
    let mut out: Vec<ScanRootInfo> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let kb = crate::kb::kb_root();
    // App 数据根的友好名(下载/微信/QQ…),给下方贴标签用(inventory_roots 已含这些路径)。
    let app_labels: std::collections::HashMap<String, String> =
        app_data_roots().into_iter().map(|r| (r.path, r.label)).collect();
    // 默认勾:知识库根 + NAS 挂载点 + App 数据下载目录(沿用盘点默认根集合)。
    for r in inventory_roots(None) {
        if !Path::new(&r).is_dir() || !seen.insert(r.clone()) {
            continue;
        }
        let label = if r == kb {
            "知识库".to_string()
        } else if let Some(l) = app_labels.get(&r) {
            l.clone()
        } else {
            Path::new(&r)
                .file_name()
                .map(super::decode_fs)
                .unwrap_or_else(|| r.clone())
        };
        out.push(ScanRootInfo { path: r, label, default_on: true });
    }
    // 本机盘符 / 桌面 / 外置卷 / 挂载点(复用全盘资源归集的跨平台根)。
    // default_on 直接沿用 scan_roots 的判断(现在「一个不落」——所有真实存在的盘符/卷默认都勾),
    // 这样首次盘点就能把整机所有可达的盘都纳入,用户想缩小范围再手动取消。
    for sr in crate::scan::scan_roots() {
        if !Path::new(&sr.path).is_dir() || !seen.insert(sr.path.clone()) {
            continue;
        }
        out.push(ScanRootInfo { path: sr.path, label: sr.label, default_on: sr.default_on });
    }
    out
}

/// 列出某目录的直属子文件夹(只读一层目录项;每个子目录再读一层估直属文件数 + 是否可展开)。
/// `root` = 所属盘点根(用于算 depth 并回填 FolderNode.root)。
fn list_child_folders(dir: &Path, root: &str) -> Vec<FolderNode> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() || !ft.is_dir() {
            continue;
        }
        let name = super::decode_fs(&entry.file_name());
        if skip_dir_scan(&name) {
            continue;
        }
        subdirs.push(entry.path());
    }
    let root_path = Path::new(root);
    let mut out: Vec<FolderNode> = Vec::with_capacity(subdirs.len());
    for sub in subdirs {
        // 直属文件数 + 是否还有更深(未被跳过的)子目录 → 前端据此显示「可展开」。
        let mut files = 0u64;
        let mut has_children = false;
        if let Ok(rd2) = std::fs::read_dir(&sub) {
            for e2 in rd2.flatten() {
                let Ok(ft2) = e2.file_type() else { continue };
                if ft2.is_symlink() {
                    continue;
                }
                if ft2.is_dir() {
                    let n2 = super::decode_fs(&e2.file_name());
                    if !skip_dir_scan(&n2) {
                        has_children = true;
                    }
                } else if ft2.is_file() {
                    files += 1;
                }
            }
        }
        let name = sub
            .file_name()
            .map(super::decode_fs)
            .unwrap_or_else(|| sub.to_string_lossy().into_owned());
        let depth = sub.strip_prefix(root_path).map(|r| r.components().count()).unwrap_or(1).max(1);
        out.push(FolderNode {
            path: sub.to_string_lossy().into_owned(),
            parent: dir.to_string_lossy().into_owned(),
            name,
            root: root.to_string(),
            depth,
            files,
            has_children,
        });
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// 列出可盘点根 + 各根的「第一层」子文件夹;更深的层级由前端展开时按需懒加载
/// (见 [`fable_scan_folder_children`]),这样 C/D 盘等也能一层层点开,不必一次扫全盘。
pub fn scan_folders(explicit: Option<String>) -> Result<FolderScan, String> {
    let roots = scan_root_candidates(explicit);
    if roots.is_empty() {
        return Err("没有可扫描的根目录(知识库未初始化,也无可访问的盘符/挂载点)".into());
    }
    let mut folders: Vec<FolderNode> = Vec::new();
    let mut truncated = false;
    for root in &roots {
        for node in list_child_folders(Path::new(&root.path), &root.path) {
            folders.push(node);
            if folders.len() >= FOLDER_SCAN_CAP {
                truncated = true;
                break;
            }
        }
        if truncated {
            break;
        }
    }
    Ok(FolderScan { roots, folders, truncated })
}

/// 盘点前先扫一眼文件夹结构(根 + 第一层)。同步返回。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fable_scan_folders(root: Option<String>) -> Result<FolderScan, String> {
    scan_folders(root)
}

/// 懒加载:点开某个文件夹时才扫它的直属子文件夹(支持一层层往下钻到任意深度)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fable_scan_folder_children(root: String, path: String) -> Result<Vec<FolderNode>, String> {
    let p = Path::new(&path);
    if !p.is_dir() {
        return Ok(Vec::new());
    }
    Ok(list_child_folders(p, &root))
}

/// 文件夹递归总量(总文件数 + 总字节数),给选择器里显示「这个文件夹有多大」。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderSize {
    pub files: u64,
    pub bytes: u64,
}

fn folder_size_rec(dir: &Path, files: &mut u64, bytes: &mut u64) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            let name = super::decode_fs(&entry.file_name());
            if skip_dir_scan(&name) {
                continue;
            }
            folder_size_rec(&entry.path(), files, bytes);
        } else if ft.is_file() {
            if let Ok(m) = entry.metadata() {
                *bytes += on_disk_size(&entry.path(), &m);
                *files += 1;
            }
        }
    }
}

/// 递归统计一个文件夹的总文件数与总字节数(skip_dir_scan 剪枝;符号链接跳过)。
/// 前端在选择器里按需、限并发地逐个文件夹调用,把大小填进对应行。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fable_folder_size(path: String) -> Result<FolderSize, String> {
    let p = Path::new(&path);
    if !p.is_dir() {
        return Ok(FolderSize { files: 0, bytes: 0 });
    }
    let mut files = 0u64;
    let mut bytes = 0u64;
    folder_size_rec(p, &mut files, &mut bytes);
    Ok(FolderSize { files, bytes })
}

/// 「按语言归类」回填:给所有还没定语言(lang='')的文件补上语言标签。
/// 代码/媒体零 IO 当场定;文稿读文件头嗅探自然语言(中文/英文/其他)。多核并行、幂等续跑。
/// 旧库(刚加 lang 列,全为 '')或新盘点后的文稿都靠它补齐。返回本轮回填条数。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fable_backfill_lang() -> Result<u64, String> {
    let conn = open_db()?;
    let mut done = 0u64;
    loop {
        if cancelled() {
            break;
        }
        // 取一批未定语言的文件(连 root 路径,文稿要据此读头)。
        let batch: Vec<(i64, String, String, String, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT f.id, f.ext, f.kind, r.path, f.relpath FROM files f
                     JOIN roots r ON r.id=f.root_id WHERE f.lang='' LIMIT 4096",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            rows.flatten().collect()
        };
        if batch.is_empty() {
            break;
        }
        // 多核算语言:代码/媒体 quick_lang 零 IO;文稿读头嗅探(work-stealing 栈)。
        let stack = Mutex::new(batch);
        let out: Mutex<Vec<(i64, String)>> = Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for _ in 0..worker_count() {
                let (stack, out) = (&stack, &out);
                s.spawn(move || loop {
                    let item = { stack.lock().unwrap().pop() };
                    let Some((id, ext, kind, root, rel)) = item else { break };
                    let mut lang = quick_lang(&ext, &kind);
                    if lang.is_empty() {
                        // 文稿:读头嗅探自然语言;不可读/二进制 → 其他。
                        let abs = Path::new(&root).join(&rel);
                        lang = read_head_sample(&abs)
                            .map(|sample| natural_lang(&sample))
                            .unwrap_or("未识别")
                            .to_string();
                    }
                    out.lock().unwrap().push((id, lang));
                });
            }
        });
        // 单事务写回这一批。
        let updates = out.into_inner().unwrap();
        conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
        {
            let mut stmt = conn
                .prepare_cached("UPDATE files SET lang=?1 WHERE id=?2")
                .map_err(|e| e.to_string())?;
            for (id, lang) in &updates {
                // 给个非空哨兵避免再次入选(理论上 lang 已非空)。
                let v = if lang.is_empty() { "未识别" } else { lang.as_str() };
                stmt.execute(rusqlite::params![v, id]).map_err(|e| e.to_string())?;
            }
        }
        conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
        done += updates.len() as u64;
        // 单次调用封顶 ~16K 文件:桌面前端循环调用,每次都短(不冻界面),返回 0 即收工。
        if done >= 16_384 {
            break;
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prog_lang_maps_extensions() {
        assert_eq!(prog_lang("py"), Some("Python"));
        assert_eq!(prog_lang("rs"), Some("Rust"));
        assert_eq!(prog_lang("tsx"), Some("React/JSX"));
        assert_eq!(prog_lang("md"), None); // 文稿交自然语言
        assert_eq!(prog_lang("png"), None);
    }

    #[test]
    fn natural_lang_detects_script() {
        assert_eq!(natural_lang("这是一段中文文本,讲的是知识库检索系统"), "中文");
        assert_eq!(natural_lang("This is an English document about retrieval."), "英文");
        assert_eq!(natural_lang("123 456 !!! ==="), "其他语种"); // 字母太少
    }

    #[test]
    fn quick_lang_code_and_media() {
        assert_eq!(quick_lang("py", "text"), "Python");
        assert_eq!(quick_lang("png", "image"), "图片");
        assert_eq!(quick_lang("md", "text"), ""); // 文稿留空待回填
    }

    #[test]
    fn skip_dir_scan_prunes_system_and_appdata() {
        for n in ["Windows", "Program Files", "AppData", "node_modules", "$Recycle.Bin", "@eaDir"] {
            assert!(skip_dir_scan(n), "{n} 应被剪掉");
        }
        for n in ["Downloads", "Documents", "WeChat Files", "datasets"] {
            assert!(!skip_dir_scan(n), "{n} 不应被剪掉");
        }
    }

    #[test]
    fn covered_by_respects_pruned_path() {
        // 普通嵌套:扫父能到子 → 视为已覆盖,子根可去重。
        assert!(covered_by(r"C:\data", r"C:\data\sub\deep"));
        assert!(covered_by("/mnt/a", "/mnt/a/b/c"));
        // 相同路径。
        assert!(covered_by(r"C:\data", r"C:\data"));
        // 不在父之内。
        assert!(!covered_by(r"C:\data", r"D:\data\x"));
        // 关键:子根埋在 appdata 内,扫父会在 appdata 处剪枝、到不了 → 不算覆盖,须保留。
        assert!(!covered_by(
            r"C:\Users\me",
            r"C:\Users\me\AppData\Roaming\app\Downloads"
        ));
        // 前缀像但非真子目录(data2 不是 data 的子目录)→ 不覆盖。
        assert!(!covered_by(r"C:\data", r"C:\data2\x"));
    }
}
