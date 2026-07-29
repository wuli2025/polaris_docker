//! collab.db —— 多人协作的权威数据地基（本地=权威，云端=兜底镜像）。
//!
//! 设计对齐 v8 方案第六节「collab 模块族」与铁律「主 Agent 裁决内容，永不裁决权限」：
//! 权限判断全部落在这张 SQLite 库的确定性授权表上，大模型的输出永远进不了权限通路。
//!
//! 连接策略沿用 fable::open_db（WAL + busy_timeout + 每线程一连接）。
use once_cell::sync::Lazy;
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

use directories::UserDirs;

// 按库路径记录已迁移——测试用 POLARIS_COLLAB_DB 切换多个临时库时,每个库都要建表。
static MIGRATED: Lazy<Mutex<HashSet<PathBuf>>> = Lazy::new(|| Mutex::new(HashSet::new()));

thread_local! {
    // 每线程缓存一条已设好 PRAGMA 的连接(rusqlite::Connection 非 Send,thread_local 正合适;
    // axum 的 spawn_blocking 线程池会复用线程,同线程二次调用零建连/零 PRAGMA 开销)。
    // 与 MIGRATED 同一手法**按 db_path 记账**:测试用 POLARIS_COLLAB_DB 切临时库时
    // 路径不同即弃旧重开,不串库。
    static TL_CONN: std::cell::RefCell<Option<(PathBuf, Connection)>> =
        const { std::cell::RefCell::new(None) };
}

/// 库位置：默认 `~/Polaris/data/collab.db`，可经 `POLARIS_COLLAB_DB` 覆写（测试用临时库）。
pub fn db_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("POLARIS_COLLAB_DB") {
        let p = p.trim();
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    UserDirs::new().map(|u| u.home_dir().join("Polaris").join("data").join("collab.db"))
}

/// 打开（或建）collab.db，跑一次迁移。
pub fn open_db() -> Result<Connection, String> {
    let path = db_path().ok_or("无法定位用户目录")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建数据目录失败: {e}"))?;
    }
    let conn = Connection::open(&path).map_err(|e| format!("打开 collab.db 失败: {e}"))?;
    conn.pragma_update(None, "journal_mode", "WAL").ok();
    conn.pragma_update(None, "synchronous", "NORMAL").ok();
    conn.pragma_update(None, "foreign_keys", "ON").ok();
    conn.busy_timeout(std::time::Duration::from_secs(20)).ok();
    {
        let mut done = MIGRATED.lock().unwrap();
        if !done.contains(&path) {
            migrate(&conn)?;
            done.insert(path.clone());
        }
    }
    Ok(conn)
}

/// 每线程复用连接执行闭包(热路径鉴权等高频只读查询用):首次调用经 `open_db()` 建连
/// (PRAGMA/迁移只跑这一次),之后同线程直接复用。`open_db()` 签名与 ~100 处调用方不动。
/// 嵌套调用(闭包内再 `with_conn`)不会 panic:检测到 RefCell 已借出时退回一个临时连接执行,
/// 语义正确(只是不复用),而非 BorrowMutError 崩溃。写事务请仍用 `open_db()`。
pub fn with_conn<T>(f: impl FnOnce(&Connection) -> Result<T, String>) -> Result<T, String> {
    let path = db_path().ok_or("无法定位用户目录")?;
    TL_CONN.with(|cell| {
        // try_borrow_mut:嵌套调用时外层已持借用 → 走临时连接兜底,不 panic。
        let Ok(mut slot) = cell.try_borrow_mut() else {
            let conn = open_db()?;
            return f(&conn);
        };
        // 路径变了(测试切库)→ 弃旧连接重开;否则复用。
        if !matches!(&*slot, Some((p, _)) if *p == path) {
            *slot = Some((path.clone(), open_db()?));
        }
        let (_, conn) = slot.as_ref().expect("上面刚填充");
        f(conn)
    })
}

/// 全部建表。只加不改（对齐兜底 7「API/schema 只加不改」），迁移幂等。
fn migrate(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        -- 账号（本地权威）。role: owner|collaborator|visitor|lead。
        CREATE TABLE IF NOT EXISTS users(
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            username     TEXT NOT NULL UNIQUE,
            pass_hash    TEXT NOT NULL,          -- argon2id PHC 串
            role         TEXT NOT NULL DEFAULT 'collaborator',
            display_name TEXT NOT NULL DEFAULT '',
            created_at   INTEGER NOT NULL,
            disabled     INTEGER NOT NULL DEFAULT 0
        );

        -- 会话票据（隧道内登录后签发）。
        CREATE TABLE IF NOT EXISTS sessions(
            token      TEXT PRIMARY KEY,
            user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            device_id  TEXT NOT NULL DEFAULT '',
            created_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL
        );

        -- 设备白名单（隧道层双因子之一）。pubkey_fp = iroh NodeId 指纹。
        CREATE TABLE IF NOT EXISTS devices(
            id        TEXT PRIMARY KEY,          -- 随机设备 id
            user_id   INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            name      TEXT NOT NULL DEFAULT '',
            node_id   TEXT NOT NULL DEFAULT '',  -- iroh NodeId（准入白名单键）
            pubkey_fp TEXT NOT NULL DEFAULT '',
            added_at  INTEGER NOT NULL,
            revoked   INTEGER NOT NULL DEFAULT 0
        );

        -- 一次性邀请票据（配对码），24h 有效、用后即废。
        CREATE TABLE IF NOT EXISTS tickets(
            code       TEXT PRIMARY KEY,
            role       TEXT NOT NULL DEFAULT 'collaborator',
            created_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL,
            used_at    INTEGER,                  -- NULL=未用
            note       TEXT NOT NULL DEFAULT ''
        );

        -- 项目。lead_expert_id=主 Agent 人格模板 id（取自 expert 花名册），可空=纯人工。
        CREATE TABLE IF NOT EXISTS projects(
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            name           TEXT NOT NULL,
            repo           TEXT NOT NULL DEFAULT '',
            lead_expert_id TEXT,
            charter_path   TEXT NOT NULL DEFAULT '',
            created_at     INTEGER NOT NULL,
            archived       INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS project_members(
            project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            role       TEXT NOT NULL DEFAULT 'collaborator',
            PRIMARY KEY(project_id, user_id)
        );

        -- 团队(GitHub org 式):一人可在多个团队,团队下挂项目,团队成员自动可见团队项目。
        CREATE TABLE IF NOT EXISTS teams(
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            name       TEXT NOT NULL UNIQUE,
            created_at INTEGER NOT NULL,
            archived   INTEGER NOT NULL DEFAULT 0
        );

        -- 团队成员。role: owner(团队管理者,可拉人/建项目)|member。
        CREATE TABLE IF NOT EXISTS team_members(
            team_id INTEGER NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            role    TEXT NOT NULL DEFAULT 'member',
            PRIMARY KEY(team_id, user_id)
        );

        -- 任务卡（六态状态机）。state: pending|in_progress|review|merged|archived|cancelled（打回=review→in_progress,round+1）。
        CREATE TABLE IF NOT EXISTS tasks(
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            title      TEXT NOT NULL,
            body       TEXT NOT NULL DEFAULT '',
            scope      TEXT NOT NULL DEFAULT '',   -- 目录/文件模式（稀疏检出+冲突预警）
            criteria   TEXT NOT NULL DEFAULT '',   -- 验收标准（逐条可判定）
            assignee   INTEGER REFERENCES users(id),
            state      TEXT NOT NULL DEFAULT 'pending',
            round      INTEGER NOT NULL DEFAULT 0, -- 当前打回轮次
            branch     TEXT NOT NULL DEFAULT '',
            pr_id      INTEGER,
            issue_no   INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );

        -- 每轮验收/打回留痕（第 N 轮能看到前 N-1 轮的完整脉络）。
        CREATE TABLE IF NOT EXISTS review_rounds(
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id    INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            round      INTEGER NOT NULL,
            reviewer   TEXT NOT NULL DEFAULT '',   -- 用户名或 lead:<expert_id>
            verdict    TEXT NOT NULL,              -- pass|reject
            comments   TEXT NOT NULL DEFAULT '',   -- JSON：挂在验收标准条目上的逐条意见
            created_at INTEGER NOT NULL
        );

        -- 审计（越权双闸的第二道：全程留痕，出错精确回溯到块级）。
        CREATE TABLE IF NOT EXISTS audit(
            id     INTEGER PRIMARY KEY AUTOINCREMENT,
            actor  TEXT NOT NULL,
            action TEXT NOT NULL,
            target TEXT NOT NULL DEFAULT '',
            detail TEXT NOT NULL DEFAULT '',
            at     INTEGER NOT NULL
        );

        -- 云端账号镜像的本地副本（握手失败兜底用，见 account_store.rs）。
        CREATE TABLE IF NOT EXISTS cloud_mirror(
            id         INTEGER PRIMARY KEY CHECK(id=1),
            version    INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL DEFAULT 0,
            blob       BLOB                      -- 加密后的镜像（云端存的就是这份密文）
        );

        -- 主 Agent 授权位(v8 3.3):默认全保守。权限判断只认这张表,不认模型输出。
        CREATE TABLE IF NOT EXISTS lead_grants(
            project_id   INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
            can_merge    INTEGER NOT NULL DEFAULT 0,  -- 能否自动放行合并
            can_reassign INTEGER NOT NULL DEFAULT 0,  -- 能否改派任务
            auto_dispatch INTEGER NOT NULL DEFAULT 0, -- 晨会分派是否免 owner 复核
            token_budget INTEGER NOT NULL DEFAULT 200000 -- 每日 token 预算上限
        );

        -- 主 Agent 每日用量(烧穿预算即暂停指挥,看板照常)。
        CREATE TABLE IF NOT EXISTS lead_usage(
            project_id INTEGER NOT NULL,
            day        TEXT NOT NULL,               -- YYYY-MM-DD(本地时区)
            tokens     INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY(project_id, day)
        );

        -- 库级小状态键值(主机标识等)。host_node_id = 主机自己那台设备的 node_id,
        -- 设备管理页据此点亮「主机」徽标。
        CREATE TABLE IF NOT EXISTS meta(
            k TEXT PRIMARY KEY,
            v TEXT NOT NULL
        );

        -- 邮箱验证码(注册/找回密码)。code_hash = argon2id PHC 串,明文码只进邮件不落库;
        -- 同一邮箱同一用途只留最新一枚(upsert),10 分钟过期,attempts 防在线穷举。
        CREATE TABLE IF NOT EXISTS email_codes(
            email      TEXT NOT NULL,
            purpose    TEXT NOT NULL,             -- signup|reset
            code_hash  TEXT NOT NULL,
            attempts   INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL,
            PRIMARY KEY(email, purpose)
        );

        -- 任务卡检查工作流(GitHub status checks 式):每轮提交跑一组检查。
        -- status: pass|fail|skipped|running。output 只留尾部(防爆库)。
        -- sha = 本轮检查针对的分支头提交(合并闸对比它防「检查后又推新提交」的陈旧窗口)。
        CREATE TABLE IF NOT EXISTS check_runs(
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id    INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            round      INTEGER NOT NULL,
            name       TEXT NOT NULL,
            status     TEXT NOT NULL,
            output     TEXT NOT NULL DEFAULT '',
            sha        TEXT NOT NULL DEFAULT '',
            started_at INTEGER NOT NULL,
            ended_at   INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_checks_task ON check_runs(task_id, round);

        -- 任务级对话(协作者↔负责人↔主Agent 的多轮微调通道,区别于 review_rounds 工单轮次)。
        -- author_user_id=0 且 role='ai' 表示主 Agent;idem_key 供 outbox 断线补传去重。
        CREATE TABLE IF NOT EXISTS task_messages(
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id        INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            round          INTEGER NOT NULL DEFAULT 0,
            author_user_id INTEGER NOT NULL,
            author_name    TEXT NOT NULL DEFAULT '',
            role           TEXT NOT NULL,
            body           TEXT NOT NULL,
            idem_key       TEXT UNIQUE,
            created_at     INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_task_messages_task ON task_messages(task_id, id);

        -- 云机中继网关挂牌(账号绑定 + 重启可恢复)。node_id = 桌面主机 iroh NodeId。
        -- 内存 REGISTRY 只是运行态(端口/监听);这张表才是「谁的主机挂了牌」的权威,
        -- 云机重启后按需懒恢复,踢人(吊销设备/停用账号)时同步删行。
        CREATE TABLE IF NOT EXISTS gw_hosts(
            node_id       TEXT PRIMARY KEY,
            user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            name          TEXT NOT NULL DEFAULT '',
            registered_at INTEGER NOT NULL
        );

        -- 同账号设备网(Tailscale 式「登录即成网」)的设备目录。**只有账号权威(云机)用这张表**:
        -- 每台装了 Polaris 的机器登录后把自己的 iroh NodeId 挂上来,同 uid 的其它设备据此
        -- 自动建隧道、自动挂盘,用户再不必手工粘连接码。
        -- key_hash = 设备密钥的 sha256(明文只在颁发那一次回给设备,库里不留)——
        -- 这把密钥能换取身份断言,等同该账号的长期凭据,所以按设备独立颁发、可独立吊销。
        CREATE TABLE IF NOT EXISTS mesh_nodes(
            node_id    TEXT PRIMARY KEY,
            uid        TEXT NOT NULL,
            name       TEXT NOT NULL DEFAULT '',
            os         TEXT NOT NULL DEFAULT '',
            ver        TEXT NOT NULL DEFAULT '',
            key_hash   TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            last_seen  INTEGER NOT NULL DEFAULT 0,
            revoked    INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_mesh_uid ON mesh_nodes(uid, revoked);

        CREATE INDEX IF NOT EXISTS idx_tasks_project ON tasks(project_id, state);
        CREATE INDEX IF NOT EXISTS idx_sessions_user ON sessions(user_id);
        CREATE INDEX IF NOT EXISTS idx_devices_node ON devices(node_id);
        CREATE INDEX IF NOT EXISTS idx_rounds_task ON review_rounds(task_id, round);
        CREATE INDEX IF NOT EXISTS idx_audit_at ON audit(at DESC, id DESC);
        "#,
    )
    .map_err(|e| format!("collab.db 迁移失败: {e}"))?;

    // 增量列(只加不改):projects.team_id —— 项目挂团队(GitHub org→repo 式)。
    // CREATE TABLE IF NOT EXISTS 对已有表不加新列,这里用 PRAGMA 探测后 ALTER 补齐。
    let has_team_id: bool = conn
        .prepare("PRAGMA table_info(projects)")
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(1))
                .map(|rows| rows.flatten().any(|c| c == "team_id"))
        })
        .unwrap_or(false);
    if !has_team_id {
        conn.execute("ALTER TABLE projects ADD COLUMN team_id INTEGER", [])
            .map_err(|e| format!("补 team_id 列失败: {e}"))?;
    }

    // 增量列:projects.check_profile —— 检查档位 code(全套)/creative(视频游戏放宽)/off。
    let has_profile: bool = conn
        .prepare("PRAGMA table_info(projects)")
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(1))
                .map(|rows| rows.flatten().any(|c| c == "check_profile"))
        })
        .unwrap_or(false);
    if !has_profile {
        conn.execute(
            "ALTER TABLE projects ADD COLUMN check_profile TEXT NOT NULL DEFAULT 'code'",
            [],
        )
        .map_err(|e| format!("补 check_profile 列失败: {e}"))?;
    }

    // 增量列:projects.shared_scope —— 管理者放行的全项目共享可见路径(CSV),
    // 协作者开工时并入稀疏集(scope_csv ∪ shared_scope)。
    let has_shared: bool = conn
        .prepare("PRAGMA table_info(projects)")
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(1))
                .map(|rows| rows.flatten().any(|c| c == "shared_scope"))
        })
        .unwrap_or(false);
    if !has_shared {
        conn.execute(
            "ALTER TABLE projects ADD COLUMN shared_scope TEXT NOT NULL DEFAULT ''",
            [],
        )
        .map_err(|e| format!("补 shared_scope 列失败: {e}"))?;
    }

    // 增量列:projects.check_skill —— 项目检查用的技能 id;空 = 内置 project-check-default。
    let has_check_skill: bool = conn
        .prepare("PRAGMA table_info(projects)")
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(1))
                .map(|rows| rows.flatten().any(|c| c == "check_skill"))
        })
        .unwrap_or(false);
    if !has_check_skill {
        conn.execute(
            "ALTER TABLE projects ADD COLUMN check_skill TEXT NOT NULL DEFAULT ''",
            [],
        )
        .map_err(|e| format!("补 check_skill 列失败: {e}"))?;
    }

    // 增量列:tickets.used_by —— 邀请码被**哪个账号**兑换(账号绑定审计:管理面能看
    // 「这张码是谁用的」,免鉴/匿名兑换从此无处遁形)。
    let has_used_by: bool = conn
        .prepare("PRAGMA table_info(tickets)")
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(1))
                .map(|rows| rows.flatten().any(|c| c == "used_by"))
        })
        .unwrap_or(false);
    if !has_used_by {
        conn.execute(
            "ALTER TABLE tickets ADD COLUMN used_by TEXT NOT NULL DEFAULT ''",
            [],
        )
        .map_err(|e| format!("补 tickets.used_by 列失败: {e}"))?;
    }

    // 增量列:check_runs.sha —— 今日早版建过无 sha 的表(未发版但开发库存在),探测补齐。
    let has_sha: bool = conn
        .prepare("PRAGMA table_info(check_runs)")
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(1))
                .map(|rows| rows.flatten().any(|c| c == "sha"))
        })
        .unwrap_or(false);
    if !has_sha {
        conn.execute(
            "ALTER TABLE check_runs ADD COLUMN sha TEXT NOT NULL DEFAULT ''",
            [],
        )
        .map_err(|e| format!("补 check_runs.sha 列失败: {e}"))?;
    }

    // 增量列:users.email —— 邮箱注册/找回密码的绑定邮箱(空串 = 老账号未绑)。
    // 唯一索引只约束非空邮箱:一个邮箱最多绑一个账号,老账号的空串互不冲突。
    let has_email: bool = conn
        .prepare("PRAGMA table_info(users)")
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(1))
                .map(|rows| rows.flatten().any(|c| c == "email"))
        })
        .unwrap_or(false);
    if !has_email {
        conn.execute(
            "ALTER TABLE users ADD COLUMN email TEXT NOT NULL DEFAULT ''",
            [],
        )
        .map_err(|e| format!("补 users.email 列失败: {e}"))?;
    }
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_users_email ON users(email) WHERE email<>''",
        [],
    )
    .map_err(|e| format!("建 users.email 唯一索引失败: {e}"))?;

    // 增量列:users.uid —— 账号权威签发的**全局账号 id**。跨主机认人只认它,
    // 用户名会改、各主机的 users.id 各不相同,uid 不变。
    // 空串 = 本机本地账号(老账号、应急 owner),不参与联邦;唯一索引因此只约束非空。
    let has_uid: bool = conn
        .prepare("PRAGMA table_info(users)")
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(1))
                .map(|rows| rows.flatten().any(|c| c == "uid"))
        })
        .unwrap_or(false);
    if !has_uid {
        conn.execute("ALTER TABLE users ADD COLUMN uid TEXT NOT NULL DEFAULT ''", [])
            .map_err(|e| format!("补 users.uid 列失败: {e}"))?;
    }
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_users_uid ON users(uid) WHERE uid<>''",
        [],
    )
    .map_err(|e| format!("建 users.uid 唯一索引失败: {e}"))?;
    // 用户名唯一升级为不区分大小写。历史库若已存在仅大小写不同的双账号,建索引会失败——
    // 容忍(.ok()):注册边界 insert_user_tx 的 NOCASE 查重照样拦新增,老双胞胎由 owner 手工清理。
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_users_username_nocase ON users(username COLLATE NOCASE)",
        [],
    )
    .ok();
    Ok(())
}

/// 测试串行锁:POLARIS_COLLAB_DB 是进程级环境变量,并行测试互设会串库。
/// 各测试第一行拿这把锁再 set_var。**不设 cfg(test)**:壳仓(hosting)的集成测试也要跨 crate 用它,
/// 依赖方 test 构建看不到本 crate 的 cfg(test) 项;常驻只是一把惰性空锁,零成本。
pub static TEST_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

/// 当前 Unix 秒。
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 记一条审计。失败不影响主流程（尽力而为）。
pub fn audit(actor: &str, action: &str, target: &str, detail: &str) {
    if let Ok(conn) = open_db() {
        let _ = conn.execute(
            "INSERT INTO audit(actor,action,target,detail,at) VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![actor, action, target, detail, now()],
        );
    }
}

/// 一条审计记录(「正在发生」活动流按新→旧读取)。
#[derive(serde::Serialize, Clone, Debug)]
pub struct AuditRow {
    pub actor: String,
    pub action: String,
    pub target: String,
    pub detail: String,
    /// Unix 秒(与 audit() 写入口径一致)。
    pub at: i64,
}

/// 读最近 N 条审计(新→旧)。「正在发生」活动流数据源。
pub fn audit_recent(limit: i64) -> Result<Vec<AuditRow>, String> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare("SELECT actor,action,target,detail,at FROM audit ORDER BY at DESC, rowid DESC LIMIT ?1")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([limit], |r| {
            Ok(AuditRow {
                actor: r.get(0)?,
                action: r.get(1)?,
                target: r.get(2)?,
                detail: r.get(3)?,
                at: r.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// meta 键值读(主机标识等库级小状态)。
pub fn meta_get(k: &str) -> Option<String> {
    let conn = open_db().ok()?;
    conn.query_row("SELECT v FROM meta WHERE k=?1", [k], |r| {
        r.get::<_, String>(0)
    })
    .ok()
}

/// meta 键值写(upsert)。
pub fn meta_set(k: &str, v: &str) -> Result<(), String> {
    let conn = open_db()?;
    conn.execute(
        "INSERT INTO meta(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        [k, v],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_roundtrip() {
        let _g = TEST_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("collab-meta-{}.db", std::process::id()));
        std::env::set_var("POLARIS_COLLAB_DB", &tmp);
        assert_eq!(meta_get("host_node_id"), None);
        meta_set("host_node_id", "node-abc").unwrap();
        assert_eq!(meta_get("host_node_id").as_deref(), Some("node-abc"));
        meta_set("host_node_id", "node-xyz").unwrap(); // upsert 覆盖
        assert_eq!(meta_get("host_node_id").as_deref(), Some("node-xyz"));
        std::env::remove_var("POLARIS_COLLAB_DB");
        let _ = std::fs::remove_file(&tmp);
    }
}
