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
                            "INSERT INTO files(root_id,relpath,name,ext,kind,size,mtime,chunked,seen)
                             VALUES(?1,?2,?3,?4,?5,?6,?7,0,?8)
                             ON CONFLICT(root_id,relpath) DO UPDATE SET
                               name=excluded.name, ext=excluded.ext, kind=excluded.kind,
                               chunked=CASE WHEN files.mtime=excluded.mtime AND files.size=excluded.size
                                            THEN files.chunked ELSE 0 END,
                               ftsed=CASE WHEN files.mtime=excluded.mtime AND files.size=excluded.size
                                          THEN files.ftsed ELSE 0 END,
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
    roots.sort();
    roots.dedup();
    roots
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
    // 去掉「嵌套根」:若 B 在 A 之内,盘 A 已覆盖 B,留 A 去 B(否则同一文件挂两个 root 重复)。
    // 排序后父目录必排在子目录前,顺序扫描即可。
    {
        let mut kept: Vec<String> = Vec::new();
        for r in roots.into_iter() {
            let inside = kept.iter().any(|k| {
                r == *k || r.starts_with(&format!("{k}/")) || r.starts_with(&format!("{k}\\"))
            });
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
    if SCANNING.swap(true, Ordering::SeqCst) {
        return Err("盘点已在进行中".into());
    }
    CANCEL.store(false, Ordering::SeqCst);
    std::thread::spawn(move || {
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
        SCANNING.store(false, Ordering::SeqCst);
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
    // 默认勾:知识库根 + NAS 挂载点(沿用盘点默认根集合)。
    for r in inventory_roots(None) {
        if !Path::new(&r).is_dir() || !seen.insert(r.clone()) {
            continue;
        }
        let label = if r == kb {
            "知识库".to_string()
        } else {
            Path::new(&r)
                .file_name()
                .map(super::decode_fs)
                .unwrap_or_else(|| r.clone())
        };
        out.push(ScanRootInfo { path: r, label, default_on: true });
    }
    // 默认不勾:本机盘符 / 桌面 / 外置卷 / 挂载点(复用全盘资源归集的跨平台根)。
    for sr in crate::scan::scan_roots() {
        if !Path::new(&sr.path).is_dir() || !seen.insert(sr.path.clone()) {
            continue;
        }
        out.push(ScanRootInfo { path: sr.path, label: sr.label, default_on: false });
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
                *bytes += m.len();
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
