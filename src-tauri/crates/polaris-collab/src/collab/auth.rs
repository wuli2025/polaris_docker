//! collab/auth.rs —— 账号与会话（应用层密码，隧道层设备白名单构成双因子）。
//!
//! 密码用 argon2id 存 PHC 串（内含盐与参数），永不落明文。会话 token 随机 32 字节 base64url。
use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use base64::Engine;
use once_cell::sync::Lazy;
use rusqlite::{params, Transaction, TransactionBehavior};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use super::db::{self, now, open_db};

/// 会话有效期（秒）。默认 30 天。
const SESSION_TTL: i64 = 30 * 24 * 3600;

const USERNAME_MIN_CHARS: usize = 3;
const USERNAME_MAX_CHARS: usize = 32;
const PASSWORD_MIN_CHARS: usize = 8;
const PASSWORD_MAX_CHARS: usize = 128;

// ── 在线暴破节流(内存态,按用户名)──────────────────────────────────────────
// 连续登录失败达阈值后进冷却窗口拒绝;冷却随失败升级(30s 起、每次翻倍、封顶 300s)。成功即清零。
// 只做「冷却」不做「永久锁定」——永久锁定会被人拿去 DoS 锁别人账号;冷却已足以把在线暴破打到无意义。
static LOGIN_GATE: Lazy<Mutex<HashMap<String, (u32, i64)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
const LOGIN_FAIL_THRESHOLD: u32 = 5;

fn login_cooldown_check(username: &str) -> Result<(), String> {
    let g = LOGIN_GATE.lock().unwrap();
    if let Some((fails, until)) = g.get(username) {
        if *fails >= LOGIN_FAIL_THRESHOLD {
            let left = *until - now();
            if left > 0 {
                return Err(format!("登录尝试过于频繁,请 {left} 秒后再试"));
            }
        }
    }
    Ok(())
}

fn login_record_fail(username: &str) {
    let mut g = LOGIN_GATE.lock().unwrap();
    // 简单封顶防内存膨胀:表过大时清掉已过冷却的陈旧条目。
    if g.len() > 5000 {
        let t = now();
        g.retain(|_, (f, until)| *f >= LOGIN_FAIL_THRESHOLD && *until > t);
    }
    let e = g.entry(username.to_string()).or_insert((0, 0));
    e.0 = e.0.saturating_add(1);
    if e.0 >= LOGIN_FAIL_THRESHOLD {
        let over = (e.0 - LOGIN_FAIL_THRESHOLD).min(4); // 0..=4
        let secs = (30i64 << over).min(300); // 30,60,120,240,300
        e.1 = now() + secs;
    }
}

fn login_clear(username: &str) {
    LOGIN_GATE.lock().unwrap().remove(username);
}

// ── 会话短缓存(token → 用户,TTL 内免开库)──────────────────────────────────
// check_session 是每条 HTTP/WS 请求的热路径,此前每次都 open_db + JOIN 查询;手机一次
// 请求常触发「鉴权 + 业务」两次开库。这里加进程内短缓存,命中直接返回。
// 只缓存**成功**结果;失败(无效/过期)永不进缓存。
const SESSION_CACHE_TTL_SECS: i64 = 10;
/// 会话缓存硬上限:到顶先剔陈旧,仍满则清空(纯性能缓存,清空只回落查库)。
const SESSION_CACHE_CAP: usize = 4096;

/// 全局吊销版本号:登出/停用账号等任何「会话失效性」mutation 后自增。
/// 缓存项记录写入时的版本号,版本不符即视为陈旧强制重查 —— 停用账号**即时生效**,
/// 不用等 TTL 走完。
static SESSION_REVOKE_GEN: AtomicU64 = AtomicU64::new(0);

struct CachedSession {
    user: User,
    /// 会话自身到期时刻(来自 sessions.expires_at):命中也要查,否则过期会话被 TTL 延长最多 TTL 秒。
    expires_at: i64,
    cached_at: i64,
    gen: u64,
    /// 缓存所属库路径:测试用 POLARIS_COLLAB_DB 切库时,只按 token 命中会串库,故按库隔离。
    db_path: String,
}

static SESSION_CACHE: Lazy<parking_lot::Mutex<HashMap<String, CachedSession>>> =
    Lazy::new(|| parking_lot::Mutex::new(HashMap::new()));

/// 吊销版本 +1:整个会话缓存立即视为陈旧(下次请求落库重查一次即回暖)。
pub(crate) fn bump_session_revocation() {
    SESSION_REVOKE_GEN.fetch_add(1, Ordering::SeqCst);
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub role: String,
    pub display_name: String,
    pub disabled: bool,
}

pub(crate) fn hash_password(pw: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| format!("密码哈希失败: {e}"))
}

pub(crate) fn verify_password(pw: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(pw.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// Validate credentials once at the account-creation boundary. Bootstrap,
/// invite signup, and owner-created accounts all eventually pass through this
/// function, so their accepted credential formats cannot drift apart.
fn validate_new_account_credentials(username: &str, password: &str) -> Result<(), String> {
    let username_len = username.chars().count();
    if !(USERNAME_MIN_CHARS..=USERNAME_MAX_CHARS).contains(&username_len) {
        return Err(format!(
            "用户名长度须为 {USERNAME_MIN_CHARS}–{USERNAME_MAX_CHARS} 个字符"
        ));
    }

    let bytes = username.as_bytes();
    if !bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
    {
        return Err("用户名只能包含 ASCII 字母、数字、下划线、点和连字符".into());
    }
    if !bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        || !bytes.last().is_some_and(u8::is_ascii_alphanumeric)
    {
        return Err("用户名首尾必须是 ASCII 字母或数字".into());
    }

    let password_len = password.chars().count();
    if !(PASSWORD_MIN_CHARS..=PASSWORD_MAX_CHARS).contains(&password_len) {
        return Err(format!(
            "密码长度须为 {PASSWORD_MIN_CHARS}–{PASSWORD_MAX_CHARS} 个字符"
        ));
    }
    Ok(())
}

fn validate_display_name(display_name: &str) -> Result<(), String> {
    let name = display_name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err("显示昵称长度须为 1–80 个字符".into());
    }
    if name.chars().any(char::is_control) {
        return Err("显示昵称不能包含控制字符".into());
    }
    Ok(())
}

/// 耗时的 Argon2 在事务外完成；票据兑换随后把所有 SQLite 写入放进同一事务。
pub(crate) fn prepare_new_account(
    username: &str,
    password: &str,
    display_name: &str,
) -> Result<String, String> {
    validate_new_account_input(username, password, display_name)?;
    hash_password(password)
}

pub(crate) fn validate_new_account_input(
    username: &str,
    password: &str,
    display_name: &str,
) -> Result<(), String> {
    validate_new_account_credentials(username, password)?;
    validate_display_name(display_name)?;
    Ok(())
}

pub(crate) fn insert_user_tx(
    tx: &Transaction<'_>,
    username: &str,
    pass_hash: &str,
    role: &str,
    display_name: &str,
) -> Result<User, String> {
    tx.execute(
        "INSERT INTO users(username,pass_hash,role,display_name,created_at) VALUES(?1,?2,?3,?4,?5)",
        params![username, pass_hash, role, display_name.trim(), now()],
    )
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            "用户名已存在".to_string()
        } else {
            format!("建账号失败: {e}")
        }
    })?;
    Ok(User {
        id: tx.last_insert_rowid(),
        username: username.into(),
        role: role.into(),
        display_name: display_name.trim().into(),
        disabled: false,
    })
}

pub(crate) fn insert_session_tx(
    tx: &Transaction<'_>,
    token: &str,
    user_id: i64,
    device_id: &str,
) -> Result<(), String> {
    tx.execute(
        "INSERT INTO sessions(token,user_id,device_id,created_at,expires_at) VALUES(?1,?2,?3,?4,?5)",
        params![token, user_id, device_id, now(), now() + SESSION_TTL],
    )
    .map_err(|e| format!("创建会话失败: {e}"))?;
    Ok(())
}

/// 32 字节 CSPRNG → base64url。会话 token 与 server 壳自动口令共用。
pub fn random_token() -> String {
    let mut buf = [0u8; 32];
    getrandom::getrandom(&mut buf).expect("getrandom");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// 建账号。用户名唯一。owner 通常是第一个账号；其余由票据兑换而来。
pub fn create_user(
    username: &str,
    password: &str,
    role: &str,
    display_name: &str,
) -> Result<User, String> {
    let phc = prepare_new_account(username, password, display_name)?;
    let mut conn = open_db()?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| format!("建账号事务失败: {e}"))?;
    let user = insert_user_tx(&tx, username, &phc, role, display_name)?;
    tx.commit().map_err(|e| format!("提交账号失败: {e}"))?;
    db::audit(username, "user.create", role, "");
    Ok(user)
}

/// 密码单独校验(找回密码改密用:不新建账号,只换口令)。
pub(crate) fn validate_password(password: &str) -> Result<(), String> {
    let n = password.chars().count();
    if !(PASSWORD_MIN_CHARS..=PASSWORD_MAX_CHARS).contains(&n) {
        return Err(format!(
            "密码长度须为 {PASSWORD_MIN_CHARS}–{PASSWORD_MAX_CHARS} 个字符"
        ));
    }
    Ok(())
}

/// 邮箱格式浅校验(真正的所有权证明是「收到验证码」,这里只挡明显垃圾输入)。
pub(crate) fn validate_email(email: &str) -> Result<String, String> {
    let e = email.trim().to_ascii_lowercase();
    let ok = e.len() >= 6
        && e.len() <= 254
        && e.matches('@').count() == 1
        && !e.starts_with('@')
        && !e.ends_with('@')
        && e.split('@').nth(1).is_some_and(|d| d.contains('.'))
        && !e.chars().any(|c| c.is_whitespace() || c.is_control());
    if !ok {
        return Err("邮箱格式不对".into());
    }
    Ok(e)
}

/// 该邮箱是否已绑定账号(注册前查重;返回命中的用户,找回密码也用它)。
pub fn find_user_by_email(email: &str) -> Result<Option<User>, String> {
    let e = validate_email(email)?;
    let conn = open_db()?;
    let row = conn.query_row(
        "SELECT id,username,role,display_name,disabled FROM users WHERE email=?1",
        params![e],
        |r| {
            Ok(User {
                id: r.get(0)?,
                username: r.get(1)?,
                role: r.get(2)?,
                display_name: r.get(3)?,
                disabled: r.get::<_, i64>(4)? != 0,
            })
        },
    );
    match row {
        Ok(u) => Ok(Some(u)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// 邮箱验证注册:建账号并绑定已验证的邮箱(同一事务)。邮箱唯一索引兜底防并发重复绑定。
pub fn create_user_with_email(
    username: &str,
    password: &str,
    role: &str,
    display_name: &str,
    email: &str,
) -> Result<User, String> {
    let e = validate_email(email)?;
    let phc = prepare_new_account(username, password, display_name)?;
    let mut conn = open_db()?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| format!("建账号事务失败: {e}"))?;
    let user = insert_user_tx(&tx, username, &phc, role, display_name)?;
    tx.execute(
        "UPDATE users SET email=?1 WHERE id=?2",
        params![e, user.id],
    )
    .map_err(|err| {
        if err.to_string().contains("UNIQUE") {
            "该邮箱已被其他账号绑定".to_string()
        } else {
            format!("绑定邮箱失败: {err}")
        }
    })?;
    tx.commit().map_err(|e| format!("提交账号失败: {e}"))?;
    db::audit(username, "user.create", role, "email-signup");
    Ok(user)
}

/// 找回密码:换口令 + 踢掉该账号全部会话(旧 token 立刻作废)。
pub fn set_password(user_id: i64, new_password: &str) -> Result<(), String> {
    validate_password(new_password)?;
    let phc = hash_password(new_password)?;
    let conn = open_db()?;
    let n = conn
        .execute(
            "UPDATE users SET pass_hash=?1 WHERE id=?2",
            params![phc, user_id],
        )
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("账号不存在".into());
    }
    conn.execute("DELETE FROM sessions WHERE user_id=?1", params![user_id])
        .ok();
    bump_session_revocation();
    db::audit("system", "user.reset_password", &user_id.to_string(), "");
    Ok(())
}

/// 首次 owner 原子创建。`is_bootstrap()` 再 `create_user()` 是 TOCTOU：两个并发请求都能
/// 看到 COUNT=0 并各建一个 owner。BEGIN IMMEDIATE 把「确认空库 + 插入」锁在同一事务。
pub fn create_initial_owner(
    username: &str,
    password: &str,
    display_name: &str,
) -> Result<User, String> {
    let phc = prepare_new_account(username, password, display_name)?;
    let mut conn = open_db()?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| format!("初始化事务失败: {e}"))?;
    let n: i64 = tx
        .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if n != 0 {
        return Err("已初始化,不能重复建 owner".into());
    }
    let user = insert_user_tx(&tx, username, &phc, "owner", display_name)
        .map_err(|e| format!("建 owner 失败: {e}"))?;
    tx.commit().map_err(|e| format!("提交初始化失败: {e}"))?;
    db::audit(username, "user.create", "owner", "bootstrap");
    Ok(user)
}

/// 是否还没有任何账号（首启引导建 owner 用）。
pub fn is_bootstrap() -> Result<bool, String> {
    let conn = open_db()?;
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    Ok(n == 0)
}

/// 登录：校验密码 → 签发会话 token。device_id 关联到会话（供 /ws 与命令做设备核对）。
pub fn login(username: &str, password: &str, device_id: &str) -> Result<(User, String), String> {
    let uname = username.trim().to_string();
    // 暴破节流:同一账号连续失败达阈值后进冷却窗口,冷却期内直接拒(不查库、不比对哈希)。
    login_cooldown_check(&uname)?;
    let conn = open_db()?;
    let row = conn.query_row(
        "SELECT id,pass_hash,role,display_name,disabled FROM users WHERE username=?1",
        params![uname],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        },
    );
    let (id, phc, role, display_name, disabled) = match row {
        Ok(v) => v,
        Err(_) => {
            login_record_fail(&uname);
            return Err("用户名或密码错误".into());
        }
    };
    if disabled != 0 {
        return Err("账号已停用".into());
    }
    if !verify_password(password, &phc) {
        login_record_fail(&uname);
        return Err("用户名或密码错误".into());
    }
    login_clear(&uname); // 登录成功清零失败计数
    let token = random_token();
    let t = now();
    conn.execute(
        "INSERT INTO sessions(token,user_id,device_id,created_at,expires_at) VALUES(?1,?2,?3,?4,?5)",
        params![token, id, device_id, t, t + SESSION_TTL],
    )
    .map_err(|e| format!("签发会话失败: {e}"))?;
    db::audit(username, "auth.login", device_id, "");
    Ok((
        User {
            id,
            username: username.into(),
            role,
            display_name,
            disabled: false,
        },
        token,
    ))
}

/// 校验会话 token → 返回用户（check_auth 的核心）。过期或吊销即失败。
/// 短缓存命中(TTL 内且吊销版本未变)直接返回,免开库;未命中走每线程复用连接查库。
pub fn check_session(token: &str) -> Result<User, String> {
    let t = now();
    // 版本号必须在查缓存/查库**之前**读:若查询期间发生吊销,写入缓存的条目带旧版本号,
    // 下次请求即判陈旧 —— 堵住「查到旧数据 + 吊销后才写缓存」的竞态窗口。
    let gen = SESSION_REVOKE_GEN.load(Ordering::SeqCst);
    // 当前库路径:缓存按库隔离(切库不串库)。取不到路径就用固定串,行为退化为不按库隔离但不 panic。
    let cur_db = db::db_path()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    {
        let mut cache = SESSION_CACHE.lock();
        if let Some(e) = cache.get(token) {
            // 命中四道闸:版本未变 + 同库 + TTL 内 + 会话自身未到期(缺一即落库重查)。
            if e.gen == gen
                && e.db_path == cur_db
                && t - e.cached_at < SESSION_CACHE_TTL_SECS
                && t < e.expires_at
            {
                return Ok(e.user.clone());
            }
            cache.remove(token); // 陈旧 → 惰性剔除,落库重查
        }
    }
    let (user, expires_at) = check_session_db(token, t)?;
    let mut cache = SESSION_CACHE.lock();
    // 硬上限防 token 洪泛撑爆内存:先剔陈旧;仍超限就整体清空(纯性能缓存,清空只是回落到查库)。
    if cache.len() >= SESSION_CACHE_CAP {
        cache.retain(|_, e| {
            e.gen == gen && t - e.cached_at < SESSION_CACHE_TTL_SECS && t < e.expires_at
        });
        if cache.len() >= SESSION_CACHE_CAP {
            cache.clear();
        }
    }
    cache.insert(
        token.to_string(),
        CachedSession {
            user: user.clone(),
            expires_at,
            cached_at: t,
            gen,
            db_path: cur_db,
        },
    );
    Ok(user)
}

/// check_session 的落库部分(原逻辑不变,连接改走每线程复用的 with_conn)。返回 (用户, 会话到期时刻)。
fn check_session_db(token: &str, t: i64) -> Result<(User, i64), String> {
    db::with_conn(|conn| {
        let row = conn.query_row(
            "SELECT u.id,u.username,u.role,u.display_name,u.disabled,s.expires_at \
             FROM sessions s JOIN users u ON u.id=s.user_id WHERE s.token=?1",
            params![token],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            },
        );
        let (id, username, role, display_name, disabled, expires_at) =
            row.map_err(|_| "会话无效，请重新登录".to_string())?;
        if disabled != 0 {
            return Err("账号已停用".into());
        }
        if expires_at < t {
            let _ = conn.execute("DELETE FROM sessions WHERE token=?1", params![token]);
            return Err("会话已过期，请重新登录".into());
        }
        Ok((
            User {
                id,
                username,
                role,
                display_name,
                disabled: false,
            },
            expires_at,
        ))
    })
}

/// 登出：删会话。吊销版本 +1,短缓存里的该会话即刻失效(不等 TTL)。
pub fn logout(token: &str) -> Result<(), String> {
    let conn = open_db()?;
    conn.execute("DELETE FROM sessions WHERE token=?1", params![token])
        .map_err(|e| e.to_string())?;
    bump_session_revocation();
    Ok(())
}

/// owner 管理面的用户行:比会话 User 多一列邮箱(找回密码的绑定情况一目了然)。
#[derive(serde::Serialize)]
pub struct AdminUserRow {
    pub id: i64,
    pub username: String,
    pub role: String,
    pub display_name: String,
    pub disabled: bool,
    pub email: String,
}

/// 列所有账号（owner 管理面用）。
pub fn list_users() -> Result<Vec<AdminUserRow>, String> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare("SELECT id,username,role,display_name,disabled,email FROM users ORDER BY id")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(AdminUserRow {
                id: r.get(0)?,
                username: r.get(1)?,
                role: r.get(2)?,
                display_name: r.get(3)?,
                disabled: r.get::<_, i64>(4)? != 0,
                email: r.get(5)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// 停用/启用账号（owner 一键止血）。停用即删其所有会话。
pub fn set_user_disabled(user_id: i64, disabled: bool) -> Result<(), String> {
    let conn = open_db()?;
    conn.execute(
        "UPDATE users SET disabled=?1 WHERE id=?2",
        params![disabled as i64, user_id],
    )
    .map_err(|e| e.to_string())?;
    if disabled {
        conn.execute("DELETE FROM sessions WHERE user_id=?1", params![user_id])
            .ok();
    }
    // 停用/启用都要吊销缓存:停用须即时踢下线;重新启用后角色等也应立即回到真实态。
    bump_session_revocation();
    db::audit(
        "owner",
        "user.disable",
        &user_id.to_string(),
        &disabled.to_string(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db() -> std::sync::MutexGuard<'static, ()> {
        let g = super::super::db::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let p = std::env::temp_dir().join(format!(
            "collab-test-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("POLARIS_COLLAB_DB", p);
        g
    }

    #[test]
    fn hash_roundtrip() {
        let h = hash_password("hunter2!").unwrap();
        assert!(verify_password("hunter2!", &h));
        assert!(!verify_password("wrong", &h));
    }

    #[test]
    fn login_flow() {
        let _g = tmp_db();
        assert!(is_bootstrap().unwrap());
        create_user("alice", "s3cret-8", "owner", "Alice").unwrap();
        assert!(!is_bootstrap().unwrap());
        let (u, tok) = login("alice", "s3cret-8", "dev1").unwrap();
        assert_eq!(u.role, "owner");
        assert_eq!(check_session(&tok).unwrap().username, "alice");
        assert!(login("alice", "nope", "dev1").is_err());
        logout(&tok).unwrap();
        assert!(check_session(&tok).is_err());
    }

    #[test]
    fn new_account_credentials_accept_boundaries() {
        assert!(validate_new_account_credentials("a_1", "12345678").is_ok());
        assert!(validate_new_account_credentials(
            "a123456789012345678901234567890b",
            &"密".repeat(PASSWORD_MAX_CHARS),
        )
        .is_ok());
    }

    #[test]
    fn new_account_credentials_reject_invalid_usernames() {
        for username in [
            "ab",
            "a12345678901234567890123456789012",
            "_alice",
            "alice-",
            "ali ce",
            "alice/ops",
            "\u{00e1}lice",
            " alice ",
        ] {
            assert!(
                validate_new_account_credentials(username, "12345678").is_err(),
                "unexpectedly accepted username {username:?}"
            );
        }
    }

    #[test]
    fn new_account_credentials_reject_invalid_password_lengths() {
        assert!(validate_new_account_credentials("alice", "1234567").is_err());
        assert!(validate_new_account_credentials("alice", &"x".repeat(129)).is_err());
    }

    #[test]
    fn all_account_creation_paths_share_credential_validation() {
        let _g = tmp_db();

        assert!(create_user("bad user", "12345678", "collaborator", "Bad").is_err());
        assert!(is_bootstrap().unwrap());

        assert!(create_initial_owner("owner", "1234567", "Owner").is_err());
        assert!(is_bootstrap().unwrap());
    }
}
