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
// 本地开源嵌入/重排(fastembed/ONNX),仅 local-embed feature 编译。
#[cfg(feature = "local-embed")]
pub mod embed_local;
pub mod eval;
pub mod files;
pub mod index;
pub mod inventory;
pub mod ontology;
pub mod retrieve;
pub mod sched;

use directories::UserDirs;
use rusqlite::Connection;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Mutex;

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

/// 任务闸 RAII 守卫:`acquire` 成功后,持有它即代表「已上闸」;drop 时(含工作线程 panic
/// 的栈展开)自动复位闸位 —— 根治「工作线程 panic → 手动 `store(false)` 被跳过 →
/// 闸永久停在 true、不重启进程就再也无法盘点/索引」这一类死锁。
/// 用法:`acquire` 后把守卫 move 进工作线程,线程无论正常结束还是 panic 都会释放闸。
pub(crate) struct FlagGuard(&'static AtomicBool);

impl FlagGuard {
    /// 尝试上闸:已被占用返回 `None`;成功返回守卫(离开作用域即释放)。
    pub(crate) fn acquire(flag: &'static AtomicBool) -> Option<Self> {
        if flag.swap(true, Ordering::SeqCst) {
            None
        } else {
            Some(FlagGuard(flag))
        }
    }
}

impl Drop for FlagGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

// ───────────────────────── SQLite 地基 ─────────────────────────

pub fn db_path() -> Option<PathBuf> {
    // 测试/高级用法可经 `POLARIS_FABLE_DB` 把库指到别处(默认 `~/Polaris/data/fable.db`),
    // 让增量盘点等端到端测试用临时库验证,绝不碰用户真实知识库。
    if let Ok(p) = std::env::var("POLARIS_FABLE_DB") {
        let p = p.trim();
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
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
    // ── 内存预算(随平台缩放)──
    // 关键:mmap_size / cache_size 是「每连接」状态,而一次 hybrid 检索的向量粗筛会按
    // worker 数(最高 12)同时开十余个连接 —— 旧值 mmap=2GiB、cache=64MiB「每连接」在桌面
    // 被乘成 ~24GiB 映射 + ~768MiB 堆,16/32GB 的 Mac/Win 直接被撑爆。
    // 现按外壳取预算:server(Docker/大内存)沿用大值吃满吞吐;桌面用克制值(总量受控),
    // 都可经 POLARIS_FABLE_MMAP_MB / POLARIS_FABLE_CACHE_MB 覆写(单位 MiB,mmap=0 关映射)。
    let (mmap_bytes, cache_kib) = db_mem_budget();
    conn.pragma_update(None, "temp_store", "MEMORY").ok();
    conn.pragma_update(None, "mmap_size", mmap_bytes).ok();
    conn.pragma_update(None, "cache_size", -cache_kib).ok(); // 负值 = KiB
    conn.pragma_update(None, "wal_autocheckpoint", 20_000i64).ok();
    // 模式迁移每进程只跑一次。migrate 含十余条 CREATE TABLE/INDEX、6 次列探测 SELECT、
    // 以及 FTS5 建虚表 —— 而 open_db 在检索热路径被高频调用(单次 hybrid 查询光向量粗筛就按
    // worker 数开十余个连接,每个旧实现都重跑整套迁移,纯浪费)。双检锁:快路径一次原子读
    // (绝大多数连接走这里,零迁移开销);仅进程内首个连接进锁内做一次迁移 —— 既免重复,
    // 又避免并发首开时两连接同时 `ALTER TABLE ADD COLUMN` 互撞(后者会报 duplicate column)。
    // PRAGMA 仍每连接设(mmap/cache/busy_timeout 等是连接级状态,必须每次)。
    if !MIGRATED.load(Ordering::Acquire) {
        let _g = MIGRATE_LOCK.lock().unwrap();
        if !MIGRATED.load(Ordering::Relaxed) {
            migrate(&conn)?;
            MIGRATED.store(true, Ordering::Release);
        }
    }
    Ok(conn)
}

// ──────────────────── 并发连接计量(纯观测,绝不阻塞)────────────────────

/// 进程内「此刻热路径打开的 fable.db 连接数」。仅观测用 —— 向量粗筛按 worker 数(≤12)
/// 并发开连接,每连接吃一份 cache 堆;若前台检索与后台索引/另一次检索重叠,连接数可叠加。
/// 本计数器让我们在连接数异常偏高时给用户一个信号(降 `POLARIS_FABLE_WORKERS`),
/// 而**绝不**节流/等待 —— 单纯 `eprintln!` 警告,热路径零阻塞。
pub(crate) static OPEN_DB_CONNS: AtomicI64 = AtomicI64::new(0);

/// 软上限:超过即告警(不阻塞)。默认 24(≈ 两轮 12 路粗筛重叠)。可经
/// `POLARIS_FABLE_MAX_CONNECTIONS` 覆写。仅用于 `eprintln!` 阈值,从不据此拒绝/等待。
fn max_connections_soft_cap() -> i64 {
    std::env::var("POLARIS_FABLE_MAX_CONNECTIONS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(24)
}

/// 已计量的连接 RAII 守卫:`Deref` 到内部 `Connection`(对调用方透明,当普通连接用),
/// drop 时(含线程 panic 栈展开)自减计数器 —— 计数永不泄漏。
/// 这是纯算术 + 一次条件 `eprintln!`,**不含任何锁/信号量/Condvar**,因此绝不会卡住查询线程。
pub(crate) struct ConnGuard(Connection);

impl std::ops::Deref for ConnGuard {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.0
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        OPEN_DB_CONNS.fetch_sub(1, Ordering::Relaxed);
    }
}

/// 与 [`open_db`] 等价,但把本连接计入 [`OPEN_DB_CONNS`] 并在 drop 时自减。
/// 在并发粗筛等「同时开多个连接」的热点用它,即可观测最坏并发度。
/// 超软上限只 `eprintln!` 一行告警(每进程最多打一次,免刷屏),**不阻塞不节流**。
pub(crate) fn open_db_gauged() -> Result<ConnGuard, String> {
    let conn = open_db()?;
    let now = OPEN_DB_CONNS.fetch_add(1, Ordering::Relaxed) + 1;
    let cap = max_connections_soft_cap();
    if now > cap && !OVER_CAP_WARNED.swap(true, Ordering::Relaxed) {
        eprintln!(
            "[fable] 警告:fable.db 并发连接数达 {now}(软上限 {cap})。\
             每连接占一份 SQLite cache 堆,过高会推高内存。\
             如内存吃紧可调小 POLARIS_FABLE_WORKERS(或 POLARIS_FABLE_CACHE_MB)。\
             这是纯告警,不会限速。"
        );
    }
    Ok(ConnGuard(conn))
}

/// 过上限告警只打一次(避免热路径刷屏);纯标志位,不阻塞。
static OVER_CAP_WARNED: AtomicBool = AtomicBool::new(false);

/// 每连接 SQLite 内存预算 → `(mmap_size 字节, cache_size KiB)`。
///
/// 这两个 PRAGMA 是连接级状态,而向量检索按 [`worker_count`](最高 12)并发开连接,
/// 故必须按「单连接 × 连接数」估总量。默认值:
/// - **server(Docker / 大内存服务器)**:mmap 2GiB、cache 64MiB —— 吃满大库顺序扫吞吐;
/// - **桌面(Mac/Win,通常 16/32GB)**:mmap 256MiB、cache 8MiB —— 12 连接累加仍仅
///   ~3GiB 映射(且映射只在真正触碰页时驻留)+ ~96MiB 堆,远低于撑爆线。
///
/// 覆写:`POLARIS_FABLE_MMAP_MB`(MiB,0=关 mmap)、`POLARIS_FABLE_CACHE_MB`(MiB,下限 1)。
/// 小内存桌面想再省 → 设 `POLARIS_FABLE_MMAP_MB=0 POLARIS_FABLE_CACHE_MB=4`;
/// Docker 大库想更激进 → 调高二者。
fn db_mem_budget() -> (i64, i64) {
    // 平台默认:server feature 给大值,其余(桌面)给克制值。
    #[cfg(feature = "server")]
    let (def_mmap_mb, def_cache_mb): (i64, i64) = (2048, 64);
    // 桌面:cache 从 16MiB 降到 8MiB —— 向量粗筛按 worker 数(≤12)并发开连接,cache 是
    // 「每连接」真实堆,16MiB 时 12 连接最坏 ~192MiB;降到 8MiB 后 12×8=96MiB 最坏(对半砍),
    // 命中缓存仍绰绰有余(单查询工作集远小于 8MiB)。env 覆写不变。
    #[cfg(not(feature = "server"))]
    let (def_mmap_mb, def_cache_mb): (i64, i64) = (256, 8);

    let env_mb = |k: &str| -> Option<i64> {
        std::env::var(k).ok().and_then(|v| v.trim().parse::<i64>().ok()).filter(|n| *n >= 0)
    };
    let mmap_mb = env_mb("POLARIS_FABLE_MMAP_MB").unwrap_or(def_mmap_mb).max(0);
    let cache_mb = env_mb("POLARIS_FABLE_CACHE_MB").unwrap_or(def_cache_mb).max(1);
    (mmap_mb * 1024 * 1024, cache_mb * 1024)
}

/// 进程级「fable.db 模式已就绪」闸 + 串行化锁(配合 [`open_db`] 的双检锁,见其注释)。
static MIGRATED: AtomicBool = AtomicBool::new(false);
static MIGRATE_LOCK: Mutex<()> = Mutex::new(());

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
        -- 增量盘点缓存:每个目录(相对根路径)的 mtime + 直属文件数/字节 + 见过代际。
        -- 重扫时先比对目录 mtime —— 没变 → 整个目录跳过 read_dir(及里面所有文件的逐个 stat),
        -- 直接把直属文件标记「还在」、只递归进子目录;改过的目录才真正 read_dir。NAS 上把
        -- 「每个目录一次 read_dir + 每个文件一次往返」降到「每个目录一次 stat」,重扫快一个数量级。
        -- fcount/fbytes 让跳过的目录仍能把直属文件计入进度/总量(报数不缩水)。seen 同 files 代际,
        -- 本轮没确认的(目录消失 / 没扫到)随对账清出。第一次盘点无缓存 → 全量;之后只摸动过的子树。
        CREATE TABLE IF NOT EXISTS dirs(
            root_id INTEGER NOT NULL,
            relpath TEXT NOT NULL,
            mtime   INTEGER NOT NULL DEFAULT 0,
            fcount  INTEGER NOT NULL DEFAULT 0,
            fbytes  INTEGER NOT NULL DEFAULT 0,
            seen    INTEGER NOT NULL DEFAULT 0,
            UNIQUE(root_id, relpath)
        );
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
        -- 文件中心 v3 · 簇间语义关系边(大模型在「读懂资料」后推断:同源 / 进阶 / 方法论 / 印证…)。
        -- 把星图从「从属树」升级成真·关系图谱。每次 AI 归类先清本范围旧边再重建,幂等。
        CREATE TABLE IF NOT EXISTS cluster_edges(
            id       INTEGER PRIMARY KEY,
            root_id  INTEGER NOT NULL DEFAULT 0,
            src      INTEGER NOT NULL,
            dst      INTEGER NOT NULL,
            label    TEXT NOT NULL DEFAULT '',
            built_at INTEGER NOT NULL DEFAULT 0
        );
        -- 文件中心 · 智能显示标题(覆盖原始乱/杂文件名;仅显示,不改磁盘)。
        -- 本地启发式不入库(grid 里现算);此表只存 AI 生成的标题(source='llm')。
        CREATE TABLE IF NOT EXISTS titles(
            file_id INTEGER PRIMARY KEY,
            title   TEXT NOT NULL DEFAULT '',
            source  TEXT NOT NULL DEFAULT '',
            made_at INTEGER NOT NULL DEFAULT 0
        );
        -- 框架派(Schema-Guided / D 方案,见 fable/ontology.rs)本体落地表。
        -- onto_types:某行业 schema 定义的实体 / 关系类型清单(单一事实源)。
        CREATE TABLE IF NOT EXISTS onto_types(
            id        INTEGER PRIMARY KEY,
            schema_id TEXT NOT NULL,
            type_id   TEXT NOT NULL,
            name      TEXT NOT NULL,
            kind      TEXT NOT NULL,
            hint      TEXT NOT NULL DEFAULT '',
            UNIQUE(schema_id, type_id, kind)
        );
        -- triples:Schema-Guided 抽出的显式三元组(主-谓-宾 + 置信 + 来源,可审计)。
        CREATE TABLE IF NOT EXISTS triples(
            id           INTEGER PRIMARY KEY,
            schema_id    TEXT NOT NULL,
            subject      TEXT NOT NULL,
            subject_type TEXT NOT NULL DEFAULT '',
            predicate    TEXT NOT NULL,
            object       TEXT NOT NULL,
            object_type  TEXT NOT NULL DEFAULT '',
            confidence   REAL NOT NULL DEFAULT 0,
            source_file  TEXT NOT NULL DEFAULT '',
            made_at      INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_triples_schema  ON triples(schema_id);
        CREATE INDEX IF NOT EXISTS idx_triples_subject ON triples(subject);
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
    // 文件中心 v3:簇「一句话画像」(大模型起的亲切口吻概括,如「你 2023-2024 的报税材料都在这」)。
    // 渲染到星图选中卡 + 报告,强化「它很懂我」的感觉。旧库 '' = 尚未 AI 命名。
    if conn.prepare("SELECT summary FROM clusters LIMIT 1").is_err() {
        conn.execute("ALTER TABLE clusters ADD COLUMN summary TEXT NOT NULL DEFAULT ''", [])
            .map_err(|e| format!("fable.db 加 clusters.summary 列失败: {e}"))?;
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
    // ── 20TB 整改 · 向量 IVF 倒排单元(ANN)──
    // chunks.cell:该向量被分配到的倒排单元(二值质心)id;-1=未分配(新入库、尚未优化)。
    // 向量车道查询只扫「最近 nprobe 个 cell + cell=-1 的新数据」,把每查询全表 O(N) 扫降到
    // ~O(N·nprobe/cells)。cell=-1 的全扫回退保证刚入库还没优化的数据不漏召回。
    if conn.prepare("SELECT cell FROM chunks LIMIT 1").is_err() {
        conn.execute("ALTER TABLE chunks ADD COLUMN cell INTEGER NOT NULL DEFAULT -1", [])
            .map_err(|e| format!("fable.db 加 chunks.cell 列失败: {e}"))?;
    }
    // ── 文件中心 · 按「语言」归类 ──
    // files.lang:文件的语言维度 —— 代码按编程语言(Python/Rust/JavaScript…,由扩展名精确判定)、
    // 文稿按自然语言(中文/英文/其他,读文件头按 CJK 占比嗅探)、媒体按大类(图片/视频/音频…)。
    // 比粗粒度 kind(text/image/…)细、比文件名(应用名)更稳。盘点时写入;旧库 '' = 待回填。
    if conn.prepare("SELECT lang FROM files LIMIT 1").is_err() {
        conn.execute("ALTER TABLE files ADD COLUMN lang TEXT NOT NULL DEFAULT ''", [])
            .map_err(|e| format!("fable.db 加 files.lang 列失败: {e}"))?;
    }
    // 20TB 热点查询复合索引 + IVF 质心表(均在所需列 ALTER 之后建,故放此处)。
    conn.execute_batch(
        r#"
        -- 嵌入待办/计数:WHERE kind='text' AND chunked=0 AND size<=? 直接走索引,免 files 大表全扫。
        CREATE INDEX IF NOT EXISTS idx_files_embed_pending ON files(kind, chunked, size);
        -- 倒排待办/计数:WHERE kind='text' AND ftsed=0 AND size<=? 同理。
        CREATE INDEX IF NOT EXISTS idx_files_lex_pending   ON files(kind, ftsed, size);
        -- 最近活动序:文件中心默认网格、晨报「最近在动的文件」、recent digest 均 ORDER BY mtime DESC
        -- LIMIT n。无此索引时大库(几十万行)要全表排序;有了它走反向索引扫描,读 ~n 行即停。
        CREATE INDEX IF NOT EXISTS idx_files_mtime         ON files(mtime);
        -- 文件中心首屏 overview:`GROUP BY kind` 取 COUNT/SUM(size)。把 size 也纳入索引 →
        -- 覆盖索引,纯走索引算聚合,免在百万行 files 表上整表扫(冷启动后台索引满负荷时,整表
        -- GROUP BY 单次可达数秒,会把 spawn_blocking 池占满拖垮 UI 取数)。
        CREATE INDEX IF NOT EXISTS idx_files_kind_size     ON files(kind, size);
        -- overview「按语言」:`GROUP BY lang, ext, kind` 取 COUNT/SUM(size)。同为覆盖索引,
        -- 把这条百万行三列聚合从全表扫降为索引扫。
        CREATE INDEX IF NOT EXISTS idx_files_lang          ON files(lang, ext, kind, size);
        -- 向量车道按 (model, cell) 取候选:IVF 探针命中的 cell 直接走索引。
        CREATE INDEX IF NOT EXISTS idx_chunks_cell         ON chunks(model, cell);
        -- IVF 二值质心:每模型 K 个 cell,bits=按位多数表决得到的二值码,分配/找最近 cell 都用汉明。
        CREATE TABLE IF NOT EXISTS vec_cells(
            id    INTEGER PRIMARY KEY,
            model TEXT NOT NULL,
            dim   INTEGER NOT NULL,
            bits  BLOB NOT NULL,
            n     INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_vec_cells_model ON vec_cells(model, dim);
        "#,
    )
    .map_err(|e| format!("fable.db 20TB 索引/IVF 迁移失败: {e}"))?;
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

/// 启动时调用(桌面 setup / server main):**后台**确保库可开、表/索引就位,绝不阻塞启动。
///
/// open_db 首调会跑一次 migrate,其中含在大库上 `CREATE INDEX idx_files_mtime` —— 几十万行的库
/// 这一下可达数秒。此前 init 在桌面 setup 主线程里同步调它,首启时窗口要等索引建完才出现 ——
/// 用户感知就是「一点开就卡死几分钟」。故改丢后台线程预热:启动瞬时返回,索引在后台默默备好;
/// 期间偶有检索/晨报命令撞上,会在 open_db 的 MIGRATE_LOCK 上短暂等一下(那些命令走 Tauri worker
/// 线程,不冻 UI 主线程),索引建好后全程走快路。server main 亦同 —— 请求路径自身也会兜底建表。
pub fn init() {
    std::thread::spawn(|| {
        if let Err(e) = open_db() {
            eprintln!("[fable] init: {e}");
        }
    });
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
///
/// 限速旋钮:`POLARIS_FABLE_WORKERS` 可显式压低盘点/语言回填/检索粗筛的并发线程数
/// (clamp 到 [1,64])。百万级大库冷启动时后台盘点/索引会占满全部核心,把 UI 线程的
/// CPU 时间片挤到几乎为零 → 主线程消息泵错过 5s 窗口被判无响应。想让后台「轻一点、
/// 别和 UI 抢核」就设小一点(例如 `POLARIS_FABLE_WORKERS=2`),重启 app 生效。
pub(crate) fn worker_count() -> usize {
    if let Ok(v) = std::env::var("POLARIS_FABLE_WORKERS") {
        if let Ok(n) = v.trim().parse::<usize>() {
            return n.clamp(1, 64);
        }
    }
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
    /// 已建的向量 IVF 倒排单元数(0=未优化,向量车道走全表扫;>0=已 ANN 加速)
    pub ivf_cells: u64,
    /// 已建 cell 后仍未分配(cell=-1)的向量数:新入库增量,巡夜再跑「优化」即可纳入 ANN
    pub ivf_unassigned: u64,
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
    // IVF 优化状态:已建 cell 数;以及已建 cell 后新入库、尚未分配进 ANN 的向量数。
    let (ivf_cells, ivf_unassigned) = match &active_model {
        Some(m) => {
            let cells = conn
                .query_row("SELECT COUNT(*) FROM vec_cells WHERE model=?1", [m], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap_or(0) as u64;
            let un = if cells > 0 {
                conn.query_row(
                    "SELECT COUNT(*) FROM chunks WHERE model=?1 AND bits IS NOT NULL AND cell=-1",
                    [m],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0) as u64
            } else {
                0
            };
            (cells, un)
        }
        None => (0, 0),
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
        ivf_cells,
        ivf_unassigned,
        scanning: SCANNING.load(Ordering::Relaxed),
        indexing: INDEXING.load(Ordering::Relaxed),
        embed_provider: crate::sense::active_provider("embed").map(|p| p.name),
        cli_path: agent::resolve_cli(),
    })
}

// ───────────────────────── 命令(薄包装)─────────────────────────

// 桌面端一律 async + spawn_blocking:status() 在百万行 files 表上连打 8 条 COUNT(*)
// (多为全表/无覆盖索引扫描),后台索引器持写锁或满负荷跑时这一下可达数秒。直接当同步
// Tauri 命令会在 WebView 主线程上跑 → 阻塞 >5s 被 Windows 判「无响应」强杀(AppHangB1)。
// server flavor 无 UI 主线程、且 dispatch 本就在 spawn_blocking 中,保持同步直调即可。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn fable_status() -> Result<FableStatus, String> {
    tauri::async_runtime::spawn_blocking(status)
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
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
    use super::FlagGuard;
    use rusqlite::Connection;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// 守卫正常生命周期:上闸 → 重复上闸被拒 → drop 释放 → 可再次上闸。
    #[test]
    fn flag_guard_acquire_block_and_release() {
        static FLAG: AtomicBool = AtomicBool::new(false);
        {
            let g = FlagGuard::acquire(&FLAG);
            assert!(g.is_some(), "首次上闸应成功");
            assert!(FLAG.load(Ordering::SeqCst), "持有守卫期间闸应为 true");
            assert!(FlagGuard::acquire(&FLAG).is_none(), "已占用时重复上闸应被拒");
        }
        assert!(!FLAG.load(Ordering::SeqCst), "守卫 drop 后闸应释放");
        assert!(FlagGuard::acquire(&FLAG).is_some(), "释放后应能再次上闸");
    }

    /// 本次修复的核心:工作线程 panic 时,栈展开也会 drop 守卫 → 闸必被释放,
    /// 不会停在 true 把功能永久锁死(回归此前「panic → store(false) 被跳过」的死锁)。
    #[test]
    fn flag_guard_releases_on_panic() {
        static FLAG: AtomicBool = AtomicBool::new(false);
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {})); // 静音预期内的 panic 输出
        let r = std::panic::catch_unwind(|| {
            let _g = FlagGuard::acquire(&FLAG).expect("上闸成功");
            assert!(FLAG.load(Ordering::SeqCst));
            panic!("模拟工作线程 panic");
        });
        std::panic::set_hook(prev);
        assert!(r.is_err(), "应捕获到 panic");
        assert!(!FLAG.load(Ordering::SeqCst), "panic 栈展开后闸必须已释放");
    }

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

    /// 并行度封顶,防多后台任务一起跑时 CPU 超订把界面拖卡(留一核给 UI,封顶 12)。
    #[test]
    fn worker_count_bounded_against_oversubscription() {
        let w = super::worker_count();
        assert!(
            (2..=12).contains(&w),
            "worker_count 须封顶在 [2,12] 防 CPU 超订(留核给 UI/主循环),实测 {w}"
        );
    }

    /// 「多后台进程也不卡顿」的数据层硬证据:盘点 / 建索引 / 智能归类等后台任务都各开
    /// 自己的连接并发读写同一个 `fable.db`。本测试照搬 [`open_db`] 的 WAL + busy_timeout
    /// 配置,起多写多读线程狂打同一库,断言:① 一次锁错误都不出(WAL 单写者 + 20s 退避
    /// 把并发写串行化、不报 "database is locked")② 不丢行 ③ 远快于 busy_timeout(无长期
    /// 阻塞)。这正是多任务并发的真正争用点 —— UI 本就跑在独立线程不受影响。
    #[test]
    fn concurrent_background_writers_no_lock_errors() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::Arc;

        // 与 open_db() 同款连接配置(WAL + NORMAL + 20s busy_timeout)。
        fn open_like_fable(path: &std::path::Path) -> Connection {
            let c = Connection::open(path).unwrap();
            c.pragma_update(None, "journal_mode", "WAL").ok();
            c.pragma_update(None, "synchronous", "NORMAL").ok();
            c.busy_timeout(std::time::Duration::from_secs(20)).unwrap();
            c
        }

        let dir = std::env::temp_dir().join(format!("polaris_fable_conc_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("conc.db");
        let _ = std::fs::remove_file(&db);
        {
            let c = open_like_fable(&db);
            c.execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY, who INTEGER, v TEXT);")
                .unwrap();
        }

        let errors = Arc::new(AtomicUsize::new(0));
        const WRITERS: usize = 4; // 模拟盘点 + 建索引 + 归类 + 第四个后台写并发
        const ROWS: usize = 400;
        const BATCH: usize = 50; // 与 inventory 写线程同构:小批量 BEGIN/COMMIT

        let start = std::time::Instant::now();
        std::thread::scope(|s| {
            for w in 0..WRITERS {
                let db = db.clone();
                let errors = Arc::clone(&errors);
                s.spawn(move || {
                    let c = open_like_fable(&db);
                    let mut written = 0usize;
                    while written < ROWS {
                        let n = BATCH.min(ROWS - written);
                        // 事务里只写、不先读 → 不会触发 WAL 的 snapshot 升级冲突;
                        // 与另一写者竞争时由 busy_timeout 退避重试,不报错。
                        let r: rusqlite::Result<()> = (|| {
                            c.execute_batch("BEGIN")?;
                            {
                                let mut stmt =
                                    c.prepare_cached("INSERT INTO t(who, v) VALUES(?1, ?2)")?;
                                for i in 0..n {
                                    stmt.execute(rusqlite::params![
                                        w as i64,
                                        format!("w{w}-{}", written + i)
                                    ])?;
                                }
                            }
                            c.execute_batch("COMMIT")?;
                            Ok(())
                        })();
                        if r.is_err() {
                            errors.fetch_add(1, Ordering::Relaxed);
                            let _ = c.execute_batch("ROLLBACK");
                        }
                        written += n;
                    }
                });
            }
            // 并发读者:模拟 overview/grid/检索在后台任务跑时照常查询(读不应被写阻死)。
            for _ in 0..2 {
                let db = db.clone();
                let errors = Arc::clone(&errors);
                s.spawn(move || {
                    let c = open_like_fable(&db);
                    for _ in 0..300 {
                        if c.query_row("SELECT COUNT(*) FROM t", [], |r| r.get::<_, i64>(0)).is_err()
                        {
                            errors.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
        });
        let elapsed = start.elapsed();

        assert_eq!(
            errors.load(Ordering::Relaxed),
            0,
            "WAL + busy_timeout 下并发后台读写不应出现任何锁错误"
        );
        let c = open_like_fable(&db);
        let total: i64 = c.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(total as usize, WRITERS * ROWS, "并发写入不应丢行");
        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            elapsed.as_secs() < 15,
            "并发写入应远快于 20s busy_timeout(无长期阻塞),实测 {elapsed:?}"
        );
    }
}
