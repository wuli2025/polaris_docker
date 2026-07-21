//! collab/gitea.rs —— 无头 Gitea 托管（v8 方案:本地私有 Git 服务,极光当它的保姆）。
//!
//! 职责边界:
//! - 进程保姆:spawn `gitea web`,守护线程健康检查(/api/healthz),挂了带指数退避自动重起;
//! - 首启配置:生成 custom/conf/app.ini(仅监听 127.0.0.1、关注册、开 LFS、sqlite3),已存在绝不覆盖;
//! - 管理员引导:CLI 建 polaris-admin + 生成 access token 存 collab.db;
//! - REST 薄封装:建用户/发用户 token/建仓/保护 main/加协作者——全部幂等(已存在视为成功)。
//!
//! 铁律:二进制不自动下载(用户自己放);main 分支人人禁直推(enable_push=false),合并只走裁决通路。
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use rusqlite::params;

use directories::UserDirs;

use super::db::open_db;

// ───────────────────────── 路径约定 ─────────────────────────

/// gitea 工作目录:默认 `~/Polaris/gitea/`,测试可用 POLARIS_GITEA_HOME 覆写。
pub fn gitea_home() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("POLARIS_GITEA_HOME") {
        if !p.trim().is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    UserDirs::new()
        .map(|u| u.home_dir().join("Polaris").join("gitea"))
        .ok_or_else(|| "找不到用户主目录".to_string())
}

/// gitea 二进制路径:POLARIS_GITEA_BIN 覆写,否则工作目录下的 gitea(.exe)。
pub fn gitea_bin() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("POLARIS_GITEA_BIN") {
        if !p.trim().is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    let name = if cfg!(windows) { "gitea.exe" } else { "gitea" };
    Ok(gitea_home()?.join(name))
}

/// HTTP 端口:默认 3000,POLARIS_GITEA_PORT 覆写。
fn gitea_port() -> u16 {
    std::env::var("POLARIS_GITEA_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000)
}

fn app_ini_path() -> Result<PathBuf, String> {
    Ok(gitea_home()?.join("custom").join("conf").join("app.ini"))
}

fn base_url() -> String {
    // Docker compose 下 Gitea 是独立容器(POLARIS_GITEA_HOST=gitea);单机托管默认 127.0.0.1。
    let host = std::env::var("POLARIS_GITEA_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    format!("http://{host}:{}", gitea_port())
}

fn api(path: &str) -> String {
    format!("{}/api/v1{path}", base_url())
}

// ───────────────────────── 首启配置 ─────────────────────────

/// 32 字节 OS 加密安全随机 → hex 字符串(SECRET_KEY / INTERNAL_TOKEN 素材 / 随机密码)。
fn rand_hex() -> Result<String, String> {
    let mut buf = [0u8; 32];
    getrandom::getrandom(&mut buf).map_err(|e| format!("生成安全随机数失败: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// 首次生成 app.ini(已存在则原样返回其路径,绝不覆盖——密钥只随机一次落盘)。
pub fn ensure_config() -> Result<PathBuf, String> {
    let home = gitea_home()?;
    let ini = app_ini_path()?;
    if ini.exists() {
        return Ok(ini);
    }
    std::fs::create_dir_all(ini.parent().unwrap()).map_err(|e| format!("建目录失败: {e}"))?;
    std::fs::create_dir_all(home.join("data")).map_err(|e| format!("建目录失败: {e}"))?;
    std::fs::create_dir_all(home.join("log")).map_err(|e| format!("建目录失败: {e}"))?;
    let port = gitea_port();
    let secret_key = rand_hex()?;
    let internal_token = rand_hex()?;
    // 路径统一正斜杠,gitea 在 Windows 上也认。
    let h = home.to_string_lossy().replace('\\', "/");
    let content = format!(
        r#"; Polaris 无头 Gitea 配置(首启自动生成,之后不再覆盖)
APP_NAME = Polaris Git
RUN_MODE = prod
WORK_PATH = {h}

[server]
PROTOCOL = http
HTTP_ADDR = 127.0.0.1
HTTP_PORT = {port}
ROOT_URL = http://127.0.0.1:{port}/
DISABLE_SSH = true
LFS_START_SERVER = true
OFFLINE_MODE = true

[database]
DB_TYPE = sqlite3
PATH = {h}/data/gitea.db

[security]
INSTALL_LOCK = true
SECRET_KEY = {secret_key}
INTERNAL_TOKEN = {internal_token}

[service]
DISABLE_REGISTRATION = true
REQUIRE_SIGNIN_VIEW = true

[log]
MODE = file
ROOT_PATH = {h}/log
LEVEL = Info

[lfs]
PATH = {h}/data/lfs
"#
    );
    std::fs::write(&ini, content).map_err(|e| format!("写 app.ini 失败: {e}"))?;
    Ok(ini)
}

// ───────────────────────── 进程守护(仿 feishu 网关) ─────────────────────────

struct GiteaProc {
    child: Option<std::process::Child>,
}

static PROC: Lazy<Mutex<GiteaProc>> = Lazy::new(|| Mutex::new(GiteaProc { child: None }));
/// 用户意图开关:stop() 置 false,守护线程见 false 即退出不再重启。
static SHOULD_RUN: AtomicBool = AtomicBool::new(false);
static HEALTHY: AtomicBool = AtomicBool::new(false);
static PID: AtomicU32 = AtomicU32::new(0);
static RESTARTS: AtomicU64 = AtomicU64::new(0);

fn kill_child() {
    let mut g = PROC.lock();
    if let Some(mut c) = g.child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    PID.store(0, Ordering::Relaxed);
}

/// 起一次 gitea 子进程(不等它退出,由守护线程健康检查)。
fn spawn_once(bin: &PathBuf, ini: &PathBuf, home: &PathBuf) -> Result<(), String> {
    let mut cmd = Command::new(bin);
    cmd.arg("web")
        .arg("-c")
        .arg(ini)
        .arg("-w")
        .arg(home)
        .current_dir(home)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let child = cmd.spawn().map_err(|e| format!("拉起 gitea 失败: {e}"))?;
    PID.store(child.id(), Ordering::Relaxed);
    PROC.lock().child = Some(child);
    Ok(())
}

/// GET /api/healthz,3 秒超时。
fn health_check() -> bool {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(3))
        .build();
    matches!(agent.get(&format!("{}/api/healthz", base_url())).call(), Ok(r) if r.status() == 200)
}

/// 启动无头 Gitea:确保配置 → spawn → 守护线程(每 10s 健康检查,连挂 3 次带退避重启)。
pub fn start() -> Result<(), String> {
    // start/stop 全程互斥:SHOULD_RUN 既是「运行意愿」又是「占位」,若 stop 在
    // start 的配置/spawn 中途清零标志,会留下无守护的 gitea 子进程且 status 报未运行。
    // 锁内完成「占位→spawn→起守护」整个序列,stop 只能在其前或其后完整发生。
    let _guard = START_STOP_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // compare_exchange 原子占位:load+store 分离时两个并发 start 都能读到 false,
    // 各 spawn 一个 gitea(端口冲突/PID 覆盖)+ 各起一个守护线程。占位失败即已在运行。
    if SHOULD_RUN
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("Gitea 已在运行".into());
    }
    // 占位后任何失败路径都要回滚标志,否则以后永远启不动。
    let prep = (|| {
        let bin = gitea_bin()?;
        if !bin.exists() {
            return Err(format!(
                "未找到 gitea 二进制,请从 https://about.gitea.com/products/gitea/ 下载对应平台版本放到 {}",
                bin.display()
            ));
        }
        let home = gitea_home()?;
        let ini = ensure_config()?;
        spawn_once(&bin, &ini, &home)?;
        Ok((bin, ini, home))
    })();
    let (bin, ini, home) = match prep {
        Ok(t) => t,
        Err(e) => {
            SHOULD_RUN.store(false, Ordering::SeqCst);
            return Err(e);
        }
    };
    RESTARTS.store(0, Ordering::Relaxed);

    // 守护线程:每 10s 摸一次 /api/healthz;连续 3 次失败且用户没喊停 → kill 重启,
    // 指数退避 1s,2s,4s…上限 60s,恢复健康后退避清零(仿 feishu.rs 网关守护)。
    std::thread::spawn(move || {
        let mut fails = 0u32;
        let mut backoff = 1u64;
        while SHOULD_RUN.load(Ordering::Relaxed) {
            // 10s 分片睡,方便 stop() 快速生效
            for _ in 0..100 {
                if !SHOULD_RUN.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if !SHOULD_RUN.load(Ordering::Relaxed) {
                break;
            }
            if health_check() {
                HEALTHY.store(true, Ordering::Relaxed);
                fails = 0;
                backoff = 1; // 恢复 → 退避清零
                continue;
            }
            HEALTHY.store(false, Ordering::Relaxed);
            fails += 1;
            if fails < 3 {
                continue;
            }
            // 连挂 3 次:kill → 退避 → 重启
            kill_child();
            let mut waited = 0u64;
            while waited < backoff * 10 && SHOULD_RUN.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));
                waited += 1;
            }
            if !SHOULD_RUN.load(Ordering::Relaxed) {
                break;
            }
            if spawn_once(&bin, &ini, &home).is_ok() {
                RESTARTS.fetch_add(1, Ordering::Relaxed);
            }
            fails = 0;
            backoff = (backoff * 2).min(60);
        }
        HEALTHY.store(false, Ordering::Relaxed);
    });
    Ok(())
}

/// start/stop 互斥锁(见 [`start`] 头注释):防 stop 撤销一个正在启动中的实例。
static START_STOP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 停止:置 should_run=false 并 kill 子进程,守护线程随即退出。
pub fn stop() {
    let _guard = START_STOP_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    SHOULD_RUN.store(false, Ordering::Relaxed);
    kill_child();
    HEALTHY.store(false, Ordering::Relaxed);
}

/// 运行状态:{running, healthy, pid, restarts}。
pub fn status() -> serde_json::Value {
    serde_json::json!({
        "running": SHOULD_RUN.load(Ordering::Relaxed),
        "healthy": HEALTHY.load(Ordering::Relaxed),
        "pid": PID.load(Ordering::Relaxed),
        "restarts": RESTARTS.load(Ordering::Relaxed),
    })
}

// ───────────────────────── 管理员引导 ─────────────────────────

const ADMIN_USER: &str = "polaris-admin";

/// gitea_admin 表(id 恒为 1,单管理员):不动 db.rs 的 migrate,本模块自建自管。
fn ensure_admin_table(conn: &rusqlite::Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS gitea_admin(
            id INTEGER PRIMARY KEY CHECK(id=1),
            username TEXT NOT NULL,
            token TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS gitea_user_tokens(
            username TEXT PRIMARY KEY,
            token TEXT NOT NULL
        );
        "#,
    )
    .map_err(|e| format!("建 gitea 表失败: {e}"))
}

/// 读库里缓存的管理员 token。
fn stored_admin_token() -> Result<Option<String>, String> {
    let conn = open_db()?;
    ensure_admin_table(&conn)?;
    conn.query_row("SELECT token FROM gitea_admin WHERE id=1", [], |r| {
        r.get::<_, String>(0)
    })
    .map(Some)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        e => Err(e.to_string()),
    })
}

/// 跑一条 gitea CLI,返回 stdout(失败带 stderr 报错)。
fn run_cli(args: &[&str]) -> Result<String, String> {
    let bin = gitea_bin()?;
    let home = gitea_home()?;
    let mut cmd = Command::new(&bin);
    cmd.args(args).current_dir(&home);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("执行 gitea CLI 失败: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(format!(
            "gitea CLI 出错: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// 管理员引导:无缓存 token 则 CLI 建号(已存在忽略)→ 生成 access token → 存库。
pub fn ensure_admin() -> Result<String, String> {
    if let Some(t) = stored_admin_token()? {
        return Ok(t);
    }
    let ini = ensure_config()?;
    let ini_s = ini.to_string_lossy().to_string();
    let pwd = rand_hex()?;
    // 建号;「已存在」类报错忽略(幂等)
    if let Err(e) = run_cli(&[
        "admin",
        "user",
        "create",
        "--admin",
        "--username",
        ADMIN_USER,
        "--password",
        &pwd,
        "--email",
        "admin@polaris.local",
        "--must-change-password=false",
        "-c",
        &ini_s,
    ]) {
        if !e.contains("already exists") && !e.contains("已存在") {
            return Err(e);
        }
    }
    let token = run_cli(&[
        "admin",
        "user",
        "generate-access-token",
        "--username",
        ADMIN_USER,
        "--scopes",
        "all",
        "--raw",
        "-c",
        &ini_s,
    ])?;
    let token = token.lines().last().unwrap_or("").trim().to_string();
    if token.is_empty() {
        return Err("生成管理员 token 失败:CLI 输出为空".into());
    }
    let conn = open_db()?;
    ensure_admin_table(&conn)?;
    conn.execute(
        "INSERT OR REPLACE INTO gitea_admin(id,username,token) VALUES(1,?1,?2)",
        params![ADMIN_USER, token],
    )
    .map_err(|e| format!("存管理员 token 失败: {e}"))?;
    Ok(token)
}

// ───────────────────────── REST 薄封装 ─────────────────────────

// 进程级共享: Agent 持有连接池, 每次现建 = 每个 REST 调用重做 TCP 握手且丢弃连接。
// 协作同步(outbox flush 等)对同一 Gitea 主机连发多个请求, 共享后走 keep-alive。
fn agent() -> ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT
        .get_or_init(|| {
            ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(10))
                .build()
        })
        .clone()
}

/// 带 Bearer 的 POST/PUT,把「已存在」状态码集合视为成功(幂等)。
fn send_json(
    method: &str,
    url: &str,
    token: &str,
    sudo: Option<&str>,
    body: serde_json::Value,
    ok_exists: &[u16],
) -> Result<Option<serde_json::Value>, String> {
    let mut req = agent()
        .request(method, url)
        .set("Authorization", &format!("Bearer {token}"));
    if let Some(u) = sudo {
        req = req.set("Sudo", u);
    }
    match req.send_json(body) {
        Ok(r) => Ok(r.into_json().ok()),
        Err(ureq::Error::Status(code, _)) if ok_exists.contains(&code) => Ok(None),
        Err(ureq::Error::Status(code, r)) => {
            let msg = r.into_string().unwrap_or_default();
            Err(format!("Gitea API {method} {url} 失败({code}): {msg}"))
        }
        Err(e) => Err(format!("Gitea API 请求失败: {e}")),
    }
}

/// 确保用户存在(随机密码,人不用密码登录,全走 token)。409/422 已存在视为成功。
pub fn ensure_user(username: &str) -> Result<(), String> {
    let token = ensure_admin()?;
    send_json(
        "POST",
        &api("/admin/users"),
        &token,
        None,
        serde_json::json!({
            "username": username,
            "email": format!("{username}@polaris.local"),
            "password": rand_hex()?,
            "must_change_password": false,
        }),
        &[409, 422],
    )?;
    Ok(())
}

/// 取用户 token:先查缓存表;没有则以管理员 sudo 身份替该用户发 write:repository token,存库返回。
pub fn user_token(username: &str) -> Result<String, String> {
    let conn = open_db()?;
    ensure_admin_table(&conn)?;
    if let Ok(t) = conn.query_row(
        "SELECT token FROM gitea_user_tokens WHERE username=?1",
        params![username],
        |r| r.get::<_, String>(0),
    ) {
        return Ok(t);
    }
    let admin = ensure_admin()?;
    let name = format!("polaris-{}", super::db::now());
    let v = send_json(
        "POST",
        &api(&format!("/users/{username}/tokens")),
        &admin,
        Some(username),
        serde_json::json!({ "name": name, "scopes": ["write:repository"] }),
        &[],
    )?
    .ok_or("发用户 token:响应为空")?;
    let t = v
        .get("sha1")
        .and_then(|x| x.as_str())
        .ok_or("发用户 token:响应缺 sha1")?
        .to_string();
    conn.execute(
        "INSERT OR REPLACE INTO gitea_user_tokens(username,token) VALUES(?1,?2)",
        params![username, t],
    )
    .map_err(|e| format!("缓存用户 token 失败: {e}"))?;
    Ok(t)
}

/// 确保仓库存在(管理员替 owner 建)。409 已存在视为成功。
pub fn ensure_repo(owner: &str, name: &str, private: bool) -> Result<(), String> {
    let token = ensure_admin()?;
    send_json(
        "POST",
        &api(&format!("/admin/users/{owner}/repos")),
        &token,
        None,
        serde_json::json!({ "name": name, "private": private, "auto_init": true, "default_branch": "main" }),
        &[409],
    )?;
    Ok(())
}

/// 保护 main:人人禁直推(enable_push=false),合并只能走 PR 裁决通路。已存在视为成功。
pub fn protect_main(owner: &str, repo: &str) -> Result<(), String> {
    let token = ensure_admin()?;
    send_json(
        "POST",
        &api(&format!("/repos/{owner}/{repo}/branch_protections")),
        &token,
        None,
        serde_json::json!({ "branch_name": "main", "enable_push": false }),
        &[403, 409],
    )?;
    Ok(())
}

/// 加协作者(默认 write)。PUT 天然幂等。
pub fn add_collaborator(
    owner: &str,
    repo: &str,
    username: &str,
    permission: &str,
) -> Result<(), String> {
    let token = ensure_admin()?;
    let perm = if permission.is_empty() {
        "write"
    } else {
        permission
    };
    send_json(
        "PUT",
        &api(&format!("/repos/{owner}/{repo}/collaborators/{username}")),
        &token,
        None,
        serde_json::json!({ "permission": perm }),
        &[],
    )?;
    Ok(())
}

// ───────────────────────── 单元测试(不依赖真实 gitea 二进制) ─────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    /// app.ini 首次生成 + 幂等:第二次调用不覆盖,SECRET_KEY 两次读到同值。
    #[test]
    fn test_ensure_config_idempotent() {
        let _g = super::super::db::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("polaris-gitea-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("POLARIS_GITEA_HOME", &dir);
        std::env::set_var("POLARIS_GITEA_PORT", "3999");

        let ini = ensure_config().expect("首次生成 app.ini");
        assert!(ini.exists());
        let c1 = std::fs::read_to_string(&ini).unwrap();
        assert!(c1.contains("HTTP_ADDR = 127.0.0.1"));
        assert!(c1.contains("HTTP_PORT = 3999"));
        assert!(c1.contains("DISABLE_REGISTRATION = true"));
        assert!(c1.contains("INSTALL_LOCK = true"));
        assert!(c1.contains("LFS_START_SERVER = true"));
        assert!(c1.contains("DB_TYPE = sqlite3"));

        // 幂等:第二次不覆盖,随机密钥不变
        let ini2 = ensure_config().expect("二次调用");
        assert_eq!(ini, ini2);
        let c2 = std::fs::read_to_string(&ini2).unwrap();
        assert_eq!(
            c1, c2,
            "app.ini 不应被覆盖,SECRET_KEY/INTERNAL_TOKEN 只随机一次"
        );

        std::env::remove_var("POLARIS_GITEA_HOME");
        std::env::remove_var("POLARIS_GITEA_PORT");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 随机密钥:两次生成互不相同且长度正确(hex 64 位)。
    #[test]
    fn test_rand_hex() {
        let a = rand_hex().unwrap();
        let b = rand_hex().unwrap();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// token 缓存表读写:gitea_admin 单行 + gitea_user_tokens 按用户名缓存。
    #[test]
    fn test_token_tables() {
        let _g = super::super::db::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let db = std::env::temp_dir().join(format!("polaris-gitea-db-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        std::env::set_var("POLARIS_COLLAB_DB", &db);

        // 初始无管理员 token
        assert_eq!(stored_admin_token().unwrap(), None);
        // 写入后可读回
        {
            let conn = open_db().unwrap();
            ensure_admin_table(&conn).unwrap();
            conn.execute(
                "INSERT OR REPLACE INTO gitea_admin(id,username,token) VALUES(1,?1,?2)",
                params![ADMIN_USER, "tok-admin-1"],
            )
            .unwrap();
            conn.execute(
                "INSERT OR REPLACE INTO gitea_user_tokens(username,token) VALUES(?1,?2)",
                params!["alice", "tok-alice"],
            )
            .unwrap();
        }
        assert_eq!(
            stored_admin_token().unwrap().as_deref(),
            Some("tok-admin-1")
        );
        let conn = open_db().unwrap();
        let t: String = conn
            .query_row(
                "SELECT token FROM gitea_user_tokens WHERE username=?1",
                params!["alice"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(t, "tok-alice");
        // 覆盖写(INSERT OR REPLACE)也幂等
        conn.execute(
            "INSERT OR REPLACE INTO gitea_user_tokens(username,token) VALUES(?1,?2)",
            params!["alice", "tok-alice-2"],
        )
        .unwrap();
        let t2: String = conn
            .query_row(
                "SELECT token FROM gitea_user_tokens WHERE username=?1",
                params!["alice"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(t2, "tok-alice-2");
        drop(conn);

        std::env::remove_var("POLARIS_COLLAB_DB");
        let _ = std::fs::remove_file(&db);
    }
}
