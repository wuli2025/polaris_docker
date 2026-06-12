//! 盘点引擎(L1a)—— 多线程并行全盘扫描 → SQLite。
//!
//! PRD v5 §7「P0.5 盘点+L1a:首小时全盘可搜」。设计:
//! - N 个 walker 线程(共享目录栈,work-stealing)只做 read_dir + stat,吃满多核;
//! - 1 个 writer 线程独占写连接,2000 行一个事务批量落库(SQLite 写入瓶颈在事务数);
//! - 「seen 代际」机制:全量重扫后自动清掉已消失文件(及其 chunks),幂等可重入;
//! - mtime/size 没变的文件保留 chunked 标记 → 重扫不会废掉已建好的向量索引。

use super::{cancelled, open_db, worker_count, CANCEL, SCANNING};
use serde::Serialize;
use serde_json::{json, Value};
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

/// 扫描时跳过的目录名(系统/缓存/版本仓;@eaDir、#recycle 是群晖特产)。
const SKIP_DIRS: &[&str] = &[
    ".git", ".svn", "node_modules", "target", ".fable", ".history", ".quarantine", "__pycache__",
    ".venv", "venv", "$RECYCLE.BIN", "System Volume Information", ".Trash", ".Trashes",
    "@eaDir", "#recycle", "#snapshot", ".DocumentRevisions-V100", ".Spotlight-V100",
];

fn skip_dir(name: &str) -> bool {
    SKIP_DIRS.iter().any(|s| s.eq_ignore_ascii_case(name))
}

// ───────────────────────── 扫描核心(三壳共用)─────────────────────────

struct FileRow {
    relpath: String,
    name: String,
    ext: String,
    kind: &'static str,
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
pub fn scan_root(root: &str, progress: &(dyn Fn(u64, u64) + Sync)) -> Result<ScanSummary, String> {
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
                            "INSERT INTO files(root_id,relpath,name,ext,kind,size,mtime,chunked,seen)
                             VALUES(?1,?2,?3,?4,?5,?6,?7,0,?8)
                             ON CONFLICT(root_id,relpath) DO UPDATE SET
                               name=excluded.name, ext=excluded.ext, kind=excluded.kind,
                               chunked=CASE WHEN files.mtime=excluded.mtime AND files.size=excluded.size
                                            THEN files.chunked ELSE 0 END,
                               size=excluded.size, mtime=excluded.mtime, seen=excluded.seen",
                        )
                        .map_err(|e| e.to_string())?;
                    for row in batch.drain(..) {
                        stmt.execute(rusqlite::params![
                            root_id, row.relpath, row.name, row.ext, row.kind,
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
                            let name = entry.file_name().to_string_lossy().into_owned();
                            if ft.is_dir() {
                                if skip_dir(&name) {
                                    continue;
                                }
                                pending.fetch_add(1, Ordering::SeqCst);
                                stack.lock().unwrap().push(entry.path());
                            } else if ft.is_file() {
                                let Ok(meta) = entry.metadata() else { continue };
                                let p = entry.path();
                                let rel = p
                                    .strip_prefix(&root_path)
                                    .map(|r| r.to_string_lossy().replace('\\', "/"))
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
                                let size = meta.len();
                                let total = n_files.fetch_add(1, Ordering::Relaxed) + 1;
                                let bytes = n_bytes.fetch_add(size, Ordering::Relaxed) + size;
                                let _ = tx.send(FileRow {
                                    relpath: rel,
                                    name,
                                    kind: classify(&ext),
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

    // 代际清理:本轮没见到的文件 = 已消失,连同 chunks 一起删
    let conn = open_db()?;
    let removed = {
        conn.execute(
            "DELETE FROM chunks WHERE file_id IN (SELECT id FROM files WHERE root_id=?1 AND seen<>?2)",
            rusqlite::params![root_id, gen],
        )
        .map_err(|e| e.to_string())?;
        conn.execute(
            "DELETE FROM files WHERE root_id=?1 AND seen<>?2",
            rusqlite::params![root_id, gen],
        )
        .map_err(|e| e.to_string())? as u64
    };
    let files = n_files.load(Ordering::Relaxed);
    let bytes = n_bytes.load(Ordering::Relaxed);
    conn.execute(
        "UPDATE roots SET scanned_at=?2, files=?3, bytes=?4 WHERE id=?1",
        rusqlite::params![root_id, gen, files as i64, bytes as i64],
    )
    .map_err(|e| e.to_string())?;

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

// ───────────────────────── 命令(后台线程 + 事件)─────────────────────────

fn emit(app: &AppHandle, payload: Value) {
    let _ = app.emit("fable:inventory", payload);
}

/// 开始盘点(root 缺省 = 知识库根)。立即返回,进度走 `fable:inventory` 事件。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fable_inventory_start(app: AppHandle, root: Option<String>) -> Result<(), String> {
    let root = root
        .filter(|r| !r.trim().is_empty())
        .unwrap_or_else(crate::kb::kb_root);
    if root.trim().is_empty() {
        return Err("没有可盘点的根目录(知识库未初始化,也未指定 root)".into());
    }
    if SCANNING.swap(true, Ordering::SeqCst) {
        return Err("盘点已在进行中".into());
    }
    CANCEL.store(false, Ordering::SeqCst);
    std::thread::spawn(move || {
        let app2 = app.clone();
        let result = scan_root(&root, &move |files, bytes| {
            emit(&app2, json!({ "kind": "progress", "files": files, "bytes": bytes }));
        });
        SCANNING.store(false, Ordering::SeqCst);
        match result {
            Ok(s) => emit(
                &app,
                json!({
                    "kind": "done", "files": s.files, "bytes": s.bytes,
                    "removed": s.removed, "seconds": s.seconds, "workers": s.workers,
                    "root": s.root,
                }),
            ),
            Err(e) => emit(&app, json!({ "kind": "error", "message": e })),
        }
    });
    Ok(())
}
