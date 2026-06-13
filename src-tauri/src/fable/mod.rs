//! 寓言计划 · 检索枢纽(Fable Hub)—— 神经层框架
//!
//! 出处:桌面《寓言计划-PRD-v5.html》§1「神经」+ §7 路线图 P0/P0.5。
//! 定位:为 1TB~10TB 级「躯体」(用户数据盘)提供工业级检索地基,四个分离子模块:
//!
//! - [`inventory`] 盘点引擎(L1a):多线程并行全盘扫描 → SQLite 落盘,首小时全盘可搜;
//! - [`index`]     向量车道(RAG):文本 chunk → 嵌入(硅基 BGE-M3,钥匙②)→ 向量落库;
//! - [`retrieve`]  塌平混检:grep 车道(多核并行扫文本)+ 向量车道 并行 → RRF 融合 → 重排;
//! - [`agent`]     编排层:以 claude code agent 为根基 —— 所有检索方式都是它的工具
//!                 (Grep/Glob/Read 内置工具 + `polaris-forge fable search` CLI),
//!                 注入指令让模型自主多路并行取证。
//!
//! 设计铁律(与 kb.rs/echo.rs 同构):
//! - 「AI 出决策,代码执行」:模型只发查询,扫盘/算分/写库全在 Rust;
//! - 单一事实源 = `~/Polaris/data/fable.db`(SQLite WAL,多根支持,与数据盘解耦);
//! - 所有长活儿后台线程 + 事件上报 + 可取消 + 幂等续跑(chunked 标记位);
//! - 桌面 / Docker / CLI 三壳共用本文件全部核心函数(命令只是薄包装)。
//!
//! 升级路径(接口稳定,内部可换):向量检索当前为流式暴力余弦(十万级 chunk 亚秒),
//! 千万级时在 `index::vector_topk` 内换 ANN/量化,签名不变。

pub mod agent;
pub mod files;
pub mod index;
pub mod inventory;
pub mod retrieve;

use directories::UserDirs;
use rusqlite::Connection;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

// ───────────────────────── 全局任务闸 ─────────────────────────

/// 盘点进行中(防双发)
pub(crate) static SCANNING: AtomicBool = AtomicBool::new(false);
/// 索引构建进行中(防双发)
pub(crate) static INDEXING: AtomicBool = AtomicBool::new(false);
/// 协作式取消:盘点与索引循环里轮询
pub(crate) static CANCEL: AtomicBool = AtomicBool::new(false);

pub(crate) fn cancelled() -> bool {
    CANCEL.load(Ordering::Relaxed)
}

// ───────────────────────── SQLite 地基 ─────────────────────────

pub fn db_path() -> Option<PathBuf> {
    UserDirs::new().map(|u| u.home_dir().join("Polaris").join("data").join("fable.db"))
}

/// 打开(或建)fable.db:WAL + busy_timeout,每个线程开自己的连接
/// (WAL 天然支持多读一写,免全局锁)。
pub(crate) fn open_db() -> Result<Connection, String> {
    let path = db_path().ok_or("无法定位用户目录")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建数据目录失败: {e}"))?;
    }
    let conn = Connection::open(&path).map_err(|e| format!("打开 fable.db 失败: {e}"))?;
    conn.pragma_update(None, "journal_mode", "WAL").ok();
    conn.pragma_update(None, "synchronous", "NORMAL").ok();
    conn.busy_timeout(std::time::Duration::from_secs(20)).ok();
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS roots(
            id         INTEGER PRIMARY KEY,
            path       TEXT NOT NULL UNIQUE,
            scanned_at INTEGER NOT NULL DEFAULT 0,
            files      INTEGER NOT NULL DEFAULT 0,
            bytes      INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS files(
            id      INTEGER PRIMARY KEY,
            root_id INTEGER NOT NULL,
            relpath TEXT NOT NULL,
            name    TEXT NOT NULL,
            ext     TEXT NOT NULL DEFAULT '',
            kind    TEXT NOT NULL DEFAULT 'other',
            size    INTEGER NOT NULL DEFAULT 0,
            mtime   INTEGER NOT NULL DEFAULT 0,
            chunked INTEGER NOT NULL DEFAULT 0,
            seen    INTEGER NOT NULL DEFAULT 0,
            UNIQUE(root_id, relpath)
        );
        CREATE INDEX IF NOT EXISTS idx_files_kind ON files(kind);
        CREATE INDEX IF NOT EXISTS idx_files_name ON files(name);
        CREATE TABLE IF NOT EXISTS chunks(
            id      INTEGER PRIMARY KEY,
            file_id INTEGER NOT NULL,
            seq     INTEGER NOT NULL,
            text    TEXT NOT NULL,
            dim     INTEGER NOT NULL,
            vec     BLOB NOT NULL,
            UNIQUE(file_id, seq)
        );
        CREATE INDEX IF NOT EXISTS idx_chunks_file ON chunks(file_id);
        CREATE TABLE IF NOT EXISTS clusters(
            id       INTEGER PRIMARY KEY,
            root_id  INTEGER NOT NULL,
            label    TEXT NOT NULL DEFAULT '',
            color    TEXT NOT NULL DEFAULT '',
            keywords TEXT NOT NULL DEFAULT '',
            size     INTEGER NOT NULL DEFAULT 0,
            built_at INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS gists(
            key     TEXT PRIMARY KEY,
            text    TEXT NOT NULL DEFAULT '',
            made_at INTEGER NOT NULL DEFAULT 0
        );
        "#,
    )
    .map_err(|e| format!("fable.db 迁移失败: {e}"))?;
    // 文件中心:文件归簇列(语义聚类写入)。ALTER 无 IF NOT EXISTS → 先探列是否已在。
    if conn.prepare("SELECT cluster_id FROM files LIMIT 1").is_err() {
        conn.execute("ALTER TABLE files ADD COLUMN cluster_id INTEGER NOT NULL DEFAULT 0", [])
            .map_err(|e| format!("fable.db 加 cluster_id 列失败: {e}"))?;
    }
    Ok(())
}

/// 启动时调用(桌面 setup / server main):只确保库可开、表就位,不做扫描。
pub fn init() {
    if let Err(e) = open_db() {
        eprintln!("[fable] init: {e}");
    }
}

/// 并行度:留一个核给 UI/主循环,封顶 12(NAS 盘 IO 先饱和)。
pub(crate) fn worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .saturating_sub(1)
        .clamp(2, 12)
}

// ───────────────────────── 状态总览 ─────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct FableRootView {
    pub path: String,
    pub files: u64,
    pub bytes: u64,
    pub scanned_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FableStatus {
    pub db_path: String,
    pub roots: Vec<FableRootView>,
    pub files_total: u64,
    pub text_files: u64,
    pub chunks_total: u64,
    /// 已完成 chunk+嵌入的文本文件数
    pub embedded_files: u64,
    /// 还在排队等嵌入的文本文件数
    pub pending_files: u64,
    pub scanning: bool,
    pub indexing: bool,
    /// 当前生效的嵌入服务商(无则向量车道不可用,grep 车道照常)
    pub embed_provider: Option<String>,
    /// agent 可调的 CLI 路径(polaris-forge),未找到则只用内置工具
    pub cli_path: Option<String>,
}

pub fn status() -> Result<FableStatus, String> {
    let conn = open_db()?;
    let mut roots = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT path, files, bytes, scanned_at FROM roots ORDER BY id")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(FableRootView {
                    path: r.get(0)?,
                    files: r.get::<_, i64>(1)? as u64,
                    bytes: r.get::<_, i64>(2)? as u64,
                    scanned_at: r.get(3)?,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows.flatten() {
            roots.push(r);
        }
    }
    let one = |sql: &str| -> u64 {
        conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0) as u64
    };
    Ok(FableStatus {
        db_path: db_path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        roots,
        files_total: one("SELECT COUNT(*) FROM files"),
        text_files: one("SELECT COUNT(*) FROM files WHERE kind='text'"),
        chunks_total: one("SELECT COUNT(*) FROM chunks"),
        embedded_files: one("SELECT COUNT(*) FROM files WHERE kind='text' AND chunked=1"),
        pending_files: one(
            "SELECT COUNT(*) FROM files WHERE kind='text' AND chunked=0 AND size<=2000000",
        ),
        scanning: SCANNING.load(Ordering::Relaxed),
        indexing: INDEXING.load(Ordering::Relaxed),
        embed_provider: crate::sense::active_provider("embed").map(|p| p.name),
        cli_path: agent::resolve_cli(),
    })
}

// ───────────────────────── 命令(薄包装)─────────────────────────

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fable_status() -> Result<FableStatus, String> {
    status()
}

/// 取消当前盘点/索引任务(协作式,几百毫秒内停)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fable_cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}
