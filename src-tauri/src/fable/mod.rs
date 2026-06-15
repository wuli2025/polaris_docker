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
pub mod eval;
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
        -- 文件中心 · 智能显示标题(覆盖原始乱/杂文件名;仅显示,不改磁盘)。
        -- 本地启发式不入库(grid 里现算);此表只存 AI 生成的标题(source='llm')。
        CREATE TABLE IF NOT EXISTS titles(
            file_id INTEGER PRIMARY KEY,
            title   TEXT NOT NULL DEFAULT '',
            source  TEXT NOT NULL DEFAULT '',
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
    // 文件中心:簇层级列(parent=0 顶层主题;parent=父簇 id 子主题)。语义两级归类写入。
    if conn.prepare("SELECT parent FROM clusters LIMIT 1").is_err() {
        conn.execute("ALTER TABLE clusters ADD COLUMN parent INTEGER NOT NULL DEFAULT 0", [])
            .map_err(|e| format!("fable.db 加 clusters.parent 列失败: {e}"))?;
    }
    // ── 20TB 整改 · P2-2 嵌入模型版本隔离 ──
    // chunks.model:写入该 chunk 时生效的嵌入模型标识(provider.default_model)。
    // 换模型后旧向量 model 不匹配 → 检索时直接被 SQL 过滤,不再「静默混入异源向量」,
    // 并据此在 status 里报「需重建的陈旧向量数」。旧库 model='' 视为陈旧。
    if conn.prepare("SELECT model FROM chunks LIMIT 1").is_err() {
        conn.execute("ALTER TABLE chunks ADD COLUMN model TEXT NOT NULL DEFAULT ''", [])
            .map_err(|e| format!("fable.db 加 chunks.model 列失败: {e}"))?;
    }
    // ── 20TB 整改 · P1-1/P1-3 二值量化粗筛位 ──
    // chunks.bits:入库时按符号位打包的二值码(dim/8 字节)。向量车道两段式 ANN:
    // 第一段只读 bits 算汉明距离(读量 1/32),粗筛出候选;第二段对候选读 f32 原始向量精排。
    // 旧库 bits=NULL → 该 chunk 退回暴力精排(不丢召回,只是慢)。
    if conn.prepare("SELECT bits FROM chunks LIMIT 1").is_err() {
        conn.execute("ALTER TABLE chunks ADD COLUMN bits BLOB", [])
            .map_err(|e| format!("fable.db 加 chunks.bits 列失败: {e}"))?;
    }
    // ── 20TB 整改 · P1-2 全文倒排(FTS5)就绪标记 ──
    // files.ftsed:该文件正文是否已写入 lex 倒排索引(类 chunked,幂等续跑;mtime 变即重置)。
    if conn.prepare("SELECT ftsed FROM files LIMIT 1").is_err() {
        conn.execute("ALTER TABLE files ADD COLUMN ftsed INTEGER NOT NULL DEFAULT 0", [])
            .map_err(|e| format!("fable.db 加 files.ftsed 列失败: {e}"))?;
    }
    // ── 20TB 整改 · P1-2 全文倒排表(FTS5 trigram)──
    // lex(rowid=file_id, body=正文):提前建好的倒排索引,查词秒回、覆盖全部文本文件,
    // 取代 grep 车道「查询时临时打开几万个文件当场读」的硬上限漏检。trigram 分词支持
    // 中文/代码的 ≥3 字符子串匹配。FTS5 未编入(理论上不会)时静默跳过 → 退回实时扫描。
    let _ = conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS lex USING fts5(body, tokenize='trigram');",
    );
    Ok(())
}

/// lex 倒排表是否就绪(FTS5 编入且建表成功)。检索/构建据此在「倒排」与「实时扫描」间择路。
pub(crate) fn lex_available(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='lex'",
        [],
        |_| Ok(()),
    )
    .is_ok()
}

/// 启动时调用(桌面 setup / server main):只确保库可开、表就位,不做扫描。
pub fn init() {
    if let Err(e) = open_db() {
        eprintln!("[fable] init: {e}");
    }
}

/// 文件系统名/路径段 → 显示用 String。修「Docker/NAS 上非 UTF-8 文件名(多为 Windows/
/// 网盘下载来的 GBK 中文名)经 to_string_lossy 变成乱码 �」:
///   ① 本就是合法 UTF-8(含纯 ASCII 与正常中文)→ 原样返回(零成本,绝大多数);
///   ② 否则(Unix 上拿到原始字节)→ 按 GBK 解码,无错且无替换符才采信(恢复真中文);
///   ③ 仍不行 → 退回 lossy(至少不崩)。
/// 注:这是「显示」用的解码;真要对该文件做 IO 时用 [`reencode_fs_path`] 把 UTF-8 编回字节命中磁盘。
pub(crate) fn decode_fs(os: &std::ffi::OsStr) -> String {
    if let Some(s) = os.to_str() {
        return s.to_string();
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let (cow, _enc, had_err) = encoding_rs::GBK.decode(os.as_bytes());
        if !had_err && !cow.contains('\u{fffd}') {
            return cow.into_owned();
        }
    }
    os.to_string_lossy().into_owned()
}

/// 把 [`decode_fs`] 解出的显示路径还原成磁盘上真实路径:UTF-8 路径若已存在直接用;
/// 否则(Unix 上原本是 GBK 名)把字符串按 GBK 编回字节、用原始字节构路径再试。
/// 让 GBK 命名的图片/文档仍能出缩略图/速览,而存进 DB 的是好看的 UTF-8 路径。
pub(crate) fn reencode_fs_path(display_abspath: &str) -> std::path::PathBuf {
    let p = std::path::PathBuf::from(display_abspath);
    if p.exists() {
        return p;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let (bytes, _enc, had_err) = encoding_rs::GBK.encode(display_abspath);
        if !had_err {
            let alt = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&bytes));
            if alt.exists() {
                return alt;
            }
        }
    }
    p
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
    /// 已写入全文倒排(lex)的文本文件数(P1-2)
    pub lex_files: u64,
    /// 还没进倒排的文本文件数(P1-2)
    pub pending_lex: u64,
    /// 与当前嵌入模型不一致、需重建的陈旧向量数(P2-2;model='' 的旧向量也计入)
    pub stale_chunks: u64,
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
    // 当前生效嵌入模型(用于算「陈旧向量」);无服务商时陈旧数报 0(向量车道本就停摆)。
    let active_model = crate::sense::active_provider("embed").map(|p| p.default_model);
    let stale_chunks = match &active_model {
        Some(m) => conn
            .query_row("SELECT COUNT(*) FROM chunks WHERE model<>?1", [m], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as u64,
        None => 0,
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
        lex_files: one("SELECT COUNT(*) FROM files WHERE kind='text' AND ftsed=1"),
        pending_lex: one(
            "SELECT COUNT(*) FROM files WHERE kind='text' AND ftsed=0 AND size<=4000000",
        ),
        stale_chunks,
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

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    /// 实测 bundled SQLite 编入了 FTS5 + trigram 分词器,且 trigram 的子串 MATCH 对
    /// 中文/代码都能命中(P1-2 全文倒排的硬前提;否则 lex 建表失败 → 静默退回实时扫描)。
    #[test]
    fn bundled_sqlite_has_fts5_trigram_substring_match() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE VIRTUAL TABLE lex USING fts5(body, tokenize='trigram');")
            .expect("bundled SQLite 缺 FTS5/trigram —— P1-2 倒排会退回实时扫描");
        conn.execute(
            "INSERT INTO lex(rowid, body) VALUES(?1, ?2)",
            rusqlite::params![1i64, "营业时间是早上九点到下午五点 open_hours=9to5"],
        )
        .unwrap();
        // 中文子串(≥3 字符)命中
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM lex WHERE body MATCH '\"营业时间\"'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "trigram 中文子串 MATCH 应命中");
        // 代码标识符子串命中
        let m: i64 = conn
            .query_row("SELECT COUNT(*) FROM lex WHERE body MATCH '\"open_hours\"'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(m, 1, "trigram ASCII 子串 MATCH 应命中");
        // bm25 排序可用(检索路用它取候选);term 取 ≥3 字符(trigram 索引不了 1~2 字符)
        let ordered: i64 = conn
            .query_row(
                "SELECT rowid FROM lex WHERE body MATCH '\"下午五点\"' ORDER BY bm25(lex) LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ordered, 1);
        // 反证 trigram 的 ≥3 字符下限:2 字符 term 不该命中(检索代码据此回退实时扫描)
        let two: i64 = conn
            .query_row("SELECT COUNT(*) FROM lex WHERE body MATCH '\"九点\"'", [], |r| r.get(0))
            .unwrap_or(0);
        assert_eq!(two, 0, "2 字符 term 在 trigram 下不命中(已知下限)");
    }
}
