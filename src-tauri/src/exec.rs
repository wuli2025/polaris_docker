//! 受控远程执行：经 iroh 隧道互联的对端可在本机跑命令（B 方案，非 SSH）。
//!
//! 开这个口子等于把本机的一部分执行能力交给对端，所以闸门叠了四道，任何一道不过都不执行：
//!
//!   1. **主机侧总开关**（`meta.exec_enabled`，默认关）—— 不开，端点一律 403。
//!      这不是环境变量而是落库开关：环境变量在桌面双击启动的场景里根本没人设得上。
//!   2. **鉴权 fail-closed** —— 见 `apihub::exec` handler。基础面有「没设访问口令就
//!      合成 owner 全放行」的历史开放模式（apihub.rs 的 resolve_app_auth_token），
//!      那对读接口尚可，对 exec 是**无鉴权 shell**。故 exec 额外要求存在真实凭据来源，
//!      开放模式下直接拒，且不给开关绕过。
//!   3. **模式闸** —— 默认 `Whitelist`：命令名必须在册、且不过 shell 解释器；
//!      `Shell` 模式要主机侧显式解锁并**自带过期时间**（`meta.exec_shell_until`），
//!      过期自动落回白名单，避免「某天忘了自己开着」。
//!   4. **全审计** —— 每条命令（含被拒的）落 collab audit 表，互联页可查流水。
//!
//! 没有做目录关押：调用方可指定任意 cwd（产品决策）。白名单模式下危害有限（不过 shell、
//! 命令在册），Shell 模式下就是真 shell —— 靠第 1/2/3 道闸兜底。

use serde::{Deserialize, Serialize};
use std::process::Stdio;
use std::time::Duration;

/// 单次执行的输出上限（stdout/stderr 各自）。超出截断并置 truncated，
/// 防对端一条 `cargo build` 把几十 MB 日志经隧道灌回来。
const MAX_OUTPUT: usize = 256 * 1024;
/// 默认/最大墙钟超时。没有超时的远程执行 = 一条命令钉死一个连接。
const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_TIMEOUT_SECS: u64 = 600;

// ───────────────────────── 策略（主机侧，落 meta 表） ─────────────────────────

const K_ENABLED: &str = "exec_enabled";
const K_SHELL_UNTIL: &str = "exec_shell_until";

/// 主机侧当前的远程执行策略。前端互联页据此渲染开关。
#[derive(Debug, Clone, Serialize)]
pub struct ExecPolicy {
    /// 总开关。关 = 端点 403。
    pub enabled: bool,
    /// Shell 模式解锁到期的 unix 秒；0/过期 = 白名单模式。
    pub shell_until: i64,
    /// 便于前端直接显示，不用自己算过期。
    pub mode: &'static str,
    /// 白名单模式下允许的命令名（给前端展示，让用户知道对端能跑什么）。
    pub whitelist: Vec<&'static str>,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn shell_until() -> i64 {
    crate::collab::db::meta_get(K_SHELL_UNTIL)
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0)
}

/// Shell 模式是否**此刻**有效。过期即自动落回白名单（不需要定时任务清理）。
fn shell_unlocked() -> bool {
    shell_until() > now_secs()
}

pub fn policy() -> ExecPolicy {
    let enabled = crate::collab::db::meta_get(K_ENABLED)
        .map(|v| v == "1")
        .unwrap_or(false);
    let su = shell_until();
    ExecPolicy {
        enabled,
        shell_until: su,
        mode: if su > now_secs() { "shell" } else { "whitelist" },
        whitelist: WHITELIST.to_vec(),
    }
}

/// 设置策略。`shell_minutes`：Some(n)=解锁 shell 模式 n 分钟（n=0 立即落回白名单），
/// None=不动 shell 档位。总开关关闭时**顺手清掉 shell 解锁**，避免下次开总开关时
/// 悄悄继承一个还没过期的 shell 授权。
pub fn set_policy(enabled: bool, shell_minutes: Option<i64>) -> Result<ExecPolicy, String> {
    crate::collab::db::meta_set(K_ENABLED, if enabled { "1" } else { "0" })?;
    if !enabled {
        crate::collab::db::meta_set(K_SHELL_UNTIL, "0")?;
    } else if let Some(m) = shell_minutes {
        let until = if m <= 0 { 0 } else { now_secs() + m * 60 };
        crate::collab::db::meta_set(K_SHELL_UNTIL, &until.to_string())?;
    }
    let p = policy();
    crate::collab::db::audit(
        "local",
        "exec.policy",
        if p.enabled { "enabled" } else { "disabled" },
        &format!("mode={} shell_until={}", p.mode, p.shell_until),
    );
    Ok(p)
}

// ── tauri 命令（主机侧本机操作；必须 lib.rs + apihub 双注册）──

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn exec_policy_get() -> ExecPolicy {
    policy()
}

/// `shellMinutes`：解锁 Shell 模式的分钟数（省略=不动档位，0=立即落回白名单）。
#[cfg_attr(feature = "desktop", tauri::command)]
#[allow(non_snake_case)]
pub fn exec_policy_set(enabled: bool, shellMinutes: Option<i64>) -> Result<ExecPolicy, String> {
    set_policy(enabled, shellMinutes)
}

// ───────────────────────── 白名单 ─────────────────────────

/// 白名单模式允许的可执行文件名（不含扩展名，Windows 上自动匹配 .exe/.cmd/.bat）。
///
/// 诚实说明：`node` / `python` / `claude` / `codex` 这类本身就能执行任意代码，
/// 收进白名单后，本模式的边界更接近「防手滑、防误操作」而非「防恶意对端」。
/// 真要防恶意，就别开总开关——这是第 1 道闸存在的意义。
const WHITELIST: &[&str] = &[
    // 版本控制
    "git", "gh",
    // 只读查看/检索
    "ls", "dir", "cat", "type", "head", "tail", "rg", "grep", "find", "tree", "wc", "diff",
    "pwd", "whoami", "hostname", "date", "uname", "df", "du", "free", "ps",
    // 构建/包管理
    "npm", "npx", "pnpm", "yarn", "node", "deno", "bun",
    "cargo", "rustc", "rustup",
    "python", "python3", "pip", "pip3", "uv", "uvx",
    "go", "java", "mvn", "gradle", "dotnet", "make",
    // 容器/编排（只是能调，命令本身仍受对方 docker 权限约束）
    "docker", "kubectl",
    // AI CLI —— 「互相调用 CLI 端」的正主
    "claude", "codex",
];

/// Windows 上这几个不是 exe 而是 PowerShell 内建，直接 spawn 会 ENOENT。
/// 走 `powershell -NoProfile -Command` 转发；参数已过元字符校验，不存在注入。
#[cfg(windows)]
const WIN_BUILTINS: &[&str] = &["ls", "dir", "cat", "type", "pwd", "wc", "diff", "ps"];

/// shell 元字符：白名单模式下命令名与任一参数含这些字符即拒。
/// 我们本就不过 shell（直接 spawn），拦它们是**纵深防御**——防止某个被调用的程序
/// 自己把参数再喂给 shell（`npm run`、`make`、`git -c core.pager=...` 都干得出来）。
const META_CHARS: &[char] = &[
    '|', '&', ';', '$', '`', '>', '<', '\n', '\r', '(', ')', '{', '}', '\0',
];

fn has_meta(s: &str) -> bool {
    s.chars().any(|c| META_CHARS.contains(&c))
}

/// 校验白名单模式下的一条命令。Ok(()) = 放行。
fn check_whitelist(cmd: &str, args: &[String]) -> Result<(), String> {
    // 命令名必须是纯名字：带路径分隔符就能绕开白名单指向任意 exe。
    if cmd.contains('/') || cmd.contains('\\') || cmd.contains("..") {
        return Err("白名单模式下命令名不能带路径,只能是命令本身".into());
    }
    if has_meta(cmd) {
        return Err("命令名含非法字符".into());
    }
    // Windows 上大小写不敏感，且允许用户写 git.exe。
    let base = cmd
        .strip_suffix(".exe")
        .or_else(|| cmd.strip_suffix(".cmd"))
        .or_else(|| cmd.strip_suffix(".bat"))
        .unwrap_or(cmd);
    if !WHITELIST.iter().any(|w| w.eq_ignore_ascii_case(base)) {
        return Err(format!(
            "命令 {cmd} 不在白名单内。主机侧可在互联页临时解锁 Shell 模式,或改用白名单内的命令"
        ));
    }
    for a in args {
        if has_meta(a) {
            return Err(format!(
                "参数含 shell 元字符(| & ; $ ` > < 等),白名单模式拒绝:{a}"
            ));
        }
    }
    Ok(())
}

// ───────────────────────── 请求 / 响应 ─────────────────────────

#[derive(Debug, Deserialize)]
pub struct ExecRequest {
    /// 白名单模式：可执行文件名。Shell 模式：忽略（用 line）。
    #[serde(default)]
    pub cmd: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Shell 模式专用：整行命令，交给 powershell/bash 解释。
    #[serde(default)]
    pub line: String,
    /// 工作目录。空 = 继承本进程 cwd。按产品决策不做关押，但必须真实存在。
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct ExecResult {
    pub ok: bool,
    /// 进程退出码；被超时杀掉时为 None。
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// 输出是否被 MAX_OUTPUT 截断。
    pub truncated: bool,
    /// 是否因超时被杀。
    pub timed_out: bool,
    pub duration_ms: u64,
    /// 实际执行的命令行（审计回显，前端直接展示）。
    pub command: String,
    pub mode: &'static str,
}

fn clip(buf: Vec<u8>) -> (String, bool) {
    let truncated = buf.len() > MAX_OUTPUT;
    let slice = if truncated { &buf[..MAX_OUTPUT] } else { &buf[..] };
    (String::from_utf8_lossy(slice).into_owned(), truncated)
}

/// 执行一条远程命令。`actor` 用于审计（调用方身份）。
///
/// 调用前 handler 必须已经过鉴权；本函数只负责总开关、模式闸、执行与审计。
pub async fn run(actor: &str, req: ExecRequest) -> Result<ExecResult, String> {
    let p = policy();
    if !p.enabled {
        crate::collab::db::audit(actor, "exec.denied", &req.cmd, "远程执行总开关未开");
        return Err("本机未开启远程执行(主机侧互联页可开)".into());
    }
    let shell_mode = shell_unlocked();

    // 组装命令。
    let started = std::time::Instant::now();
    let (program, argv, display) = if shell_mode && !req.line.trim().is_empty() {
        let line = req.line.trim().to_string();
        #[cfg(windows)]
        let (prog, a) = (
            "powershell".to_string(),
            vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                line.clone(),
            ],
        );
        #[cfg(not(windows))]
        let (prog, a) = (
            "/bin/sh".to_string(),
            vec!["-c".to_string(), line.clone()],
        );
        (prog, a, line)
    } else {
        // 白名单模式（或 shell 模式但用了 cmd+args 的结构化形式）。
        //
        // 顺序要紧:「白名单模式带了 line」必须先于「cmd 为空」判。对端在白名单模式下
        // 发整行 shell 时 cmd 正好是空的,若先报「缺少参数 cmd」,对方既看不懂真正原因
        // (该让主机侧解锁 Shell),这次尝试也不会落审计 —— 主机侧就看不见有人在敲这道门。
        if !shell_mode && !req.line.trim().is_empty() {
            crate::collab::db::audit(actor, "exec.denied", &req.line, "白名单模式不接受整行 shell");
            return Err("本机当前是白名单模式,不接受整行 shell 命令;请用 cmd + args,或让主机侧解锁 Shell 模式".into());
        }
        if req.cmd.trim().is_empty() {
            return Err("缺少参数 cmd".into());
        }
        if !shell_mode {
            check_whitelist(&req.cmd, &req.args).inspect_err(|e| {
                crate::collab::db::audit(actor, "exec.denied", &req.cmd, e);
            })?;
        }
        let display = if req.args.is_empty() {
            req.cmd.clone()
        } else {
            format!("{} {}", req.cmd, req.args.join(" "))
        };
        // Windows 内建命令没有 exe，转 powershell 承载（参数已过元字符校验）。
        #[cfg(windows)]
        {
            let base = req.cmd.to_ascii_lowercase();
            if WIN_BUILTINS.contains(&base.as_str()) {
                let mut a = vec![
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    display.clone(),
                ];
                a.shrink_to_fit();
                ("powershell".to_string(), a, display)
            } else {
                (req.cmd.clone(), req.args.clone(), display)
            }
        }
        #[cfg(not(windows))]
        {
            (req.cmd.clone(), req.args.clone(), display)
        }
    };

    let timeout = req
        .timeout_secs
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .clamp(1, MAX_TIMEOUT_SECS);

    let mut c = tokio::process::Command::new(&program);
    c.args(&argv)
        .stdin(Stdio::null()) // 远程执行没有交互终端：不给 stdin，防命令等输入挂死到超时
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if !req.cwd.trim().is_empty() {
        let dir = std::path::PathBuf::from(req.cwd.trim());
        if !dir.is_dir() {
            return Err(format!("工作目录不存在:{}", req.cwd));
        }
        c.current_dir(dir);
    }
    // Windows：不弹控制台窗口（主机可能正锁屏/在托盘驻留）。
    // tokio::process::Command 自带 creation_flags(Windows),不需 std 的 CommandExt。
    #[cfg(windows)]
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

    let child = c
        .spawn()
        .map_err(|e| format!("启动失败({program}):{e}"))?;

    let (out, timed_out) =
        match tokio::time::timeout(Duration::from_secs(timeout), child.wait_with_output()).await {
            Ok(Ok(o)) => (Some(o), false),
            Ok(Err(e)) => return Err(format!("执行失败:{e}")),
            // 超时：kill_on_drop 保证 child 被 drop 时进程组收掉。
            Err(_) => (None, true),
        };

    let duration_ms = started.elapsed().as_millis() as u64;
    let (stdout, stderr, exit_code, t1, t2) = match out {
        Some(o) => {
            let (so, t1) = clip(o.stdout);
            let (se, t2) = clip(o.stderr);
            (so, se, o.status.code(), t1, t2)
        }
        None => (
            String::new(),
            format!("命令超时({timeout}s)已终止"),
            None,
            false,
            false,
        ),
    };
    let ok = !timed_out && exit_code == Some(0);
    let mode = if shell_mode { "shell" } else { "whitelist" };
    crate::collab::db::audit(
        actor,
        "exec.run",
        &display,
        &format!(
            "mode={mode} cwd={} exit={} {}ms{}",
            if req.cwd.trim().is_empty() { "-" } else { req.cwd.trim() },
            exit_code.map(|c| c.to_string()).unwrap_or_else(|| "timeout".into()),
            duration_ms,
            if t1 || t2 { " truncated" } else { "" }
        ),
    );

    Ok(ExecResult {
        ok,
        exit_code,
        stdout,
        stderr,
        truncated: t1 || t2,
        timed_out,
        duration_ms,
        command: display,
        mode,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitelist_accepts_plain_command() {
        assert!(check_whitelist("git", &["status".into(), "--short".into()]).is_ok());
        assert!(check_whitelist("git.exe", &["log".into()]).is_ok());
        assert!(check_whitelist("GIT", &[]).is_ok(), "命令名大小写不敏感");
    }

    #[test]
    fn whitelist_rejects_unlisted() {
        let e = check_whitelist("rm", &["-rf".into(), "/".into()]).unwrap_err();
        assert!(e.contains("不在白名单"), "{e}");
    }

    #[test]
    fn whitelist_rejects_path_escape() {
        // 带路径就能指向白名单外的任意 exe，必须拒。
        assert!(check_whitelist("../../evil", &[]).is_err());
        assert!(check_whitelist("/usr/bin/rm", &[]).is_err());
        assert!(check_whitelist("C:\\Windows\\System32\\cmd.exe", &[]).is_err());
    }

    #[test]
    fn whitelist_rejects_meta_chars_in_args() {
        // 纵深防御：即便我们不过 shell，被调程序可能自己过。
        for bad in ["a|b", "a;b", "a&b", "$(x)", "`x`", "a>b", "a\nb"] {
            assert!(
                check_whitelist("git", &["log".into(), bad.to_string()]).is_err(),
                "应拒绝元字符 {bad}"
            );
        }
    }

    #[test]
    fn whitelist_allows_normal_flags_and_paths_in_args() {
        // 参数里的路径是正常的（只有命令名不许带路径）。
        assert!(check_whitelist("git", &["-C".into(), "D:\\repo".into(), "status".into()]).is_ok());
        assert!(check_whitelist("rg", &["--hidden".into(), "foo.*bar".into()]).is_ok());
    }

    // ── 端到端：走真实的 run()（真临时库 + 真起进程），验证四道闸的实际行为 ──
    //
    // 这些测试互设 POLARIS_COLLAB_DB，必须串行（TEST_LOCK），且不能与 db 的其它
    // 测试并发。用 cargo 自己当被测命令：跑 cargo test 时它必然在 PATH 上。

    fn with_tmp_db(name: &str) -> std::path::PathBuf {
        let tmp = std::env::temp_dir().join(format!("polaris-exec-test-{name}.db"));
        let _ = std::fs::remove_file(&tmp);
        std::env::set_var("POLARIS_COLLAB_DB", &tmp);
        tmp
    }

    fn req(cmd: &str, args: &[&str]) -> ExecRequest {
        ExecRequest {
            cmd: cmd.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            line: String::new(),
            cwd: String::new(),
            timeout_secs: Some(60),
        }
    }

    #[tokio::test]
    async fn gate1_disabled_by_default_rejects() {
        let _g = crate::collab::db::TEST_LOCK.lock().unwrap();
        let tmp = with_tmp_db("gate1");
        // 全新库 = 没有 exec_enabled 键 = 默认关。
        assert!(!policy().enabled, "远程执行必须默认关闭");
        let e = run("tester", req("cargo", &["--version"])).await.unwrap_err();
        assert!(e.contains("未开启远程执行"), "{e}");
        std::env::remove_var("POLARIS_COLLAB_DB");
        let _ = std::fs::remove_file(&tmp);
    }

    #[tokio::test]
    async fn whitelist_mode_runs_listed_and_blocks_rest() {
        let _g = crate::collab::db::TEST_LOCK.lock().unwrap();
        let tmp = with_tmp_db("wl");
        set_policy(true, None).unwrap();
        assert_eq!(policy().mode, "whitelist", "开总开关后默认必须是白名单模式");

        // 在册命令真跑起来，拿到真实退出码与 stdout。
        let r = run("tester", req("cargo", &["--version"])).await.unwrap();
        assert!(r.ok, "cargo --version 应成功: {:?}", r);
        assert_eq!(r.exit_code, Some(0));
        assert!(r.stdout.contains("cargo"), "stdout={}", r.stdout);
        assert_eq!(r.mode, "whitelist");

        // 不在册的命令：拒在闸门，不落到 spawn。
        let e = run("tester", req("rm", &["-rf", "x"])).await.unwrap_err();
        assert!(e.contains("不在白名单"), "{e}");

        // 白名单模式不接受整行 shell。
        let mut lreq = req("", &[]);
        lreq.line = "cargo --version | findstr cargo".into();
        let e = run("tester", lreq).await.unwrap_err();
        assert!(e.contains("白名单模式"), "{e}");

        std::env::remove_var("POLARIS_COLLAB_DB");
        let _ = std::fs::remove_file(&tmp);
    }

    #[tokio::test]
    async fn shell_mode_unlocks_then_expires() {
        let _g = crate::collab::db::TEST_LOCK.lock().unwrap();
        let tmp = with_tmp_db("shell");
        set_policy(true, Some(30)).unwrap();
        assert_eq!(policy().mode, "shell", "解锁 30 分钟后应是 shell 模式");

        // 整行命令交给 shell，管道可用。
        let mut lreq = req("", &[]);
        lreq.line = if cfg!(windows) {
            "echo hello-shell".into()
        } else {
            "echo hello-shell | tr a-z A-Z".into()
        };
        let r = run("tester", lreq).await.unwrap();
        assert!(r.ok, "shell 整行命令应成功: {:?}", r);
        assert!(
            r.stdout.to_lowercase().contains("hello-shell"),
            "stdout={}",
            r.stdout
        );
        assert_eq!(r.mode, "shell");

        // 过期即自动落回白名单——不需要任何定时清理任务。
        crate::collab::db::meta_set(K_SHELL_UNTIL, &(now_secs() - 1).to_string()).unwrap();
        assert_eq!(policy().mode, "whitelist", "shell 授权过期后必须自动落回白名单");

        // 关总开关会顺手清掉 shell 解锁，防下次开启时继承悬空授权。
        set_policy(true, Some(30)).unwrap();
        set_policy(false, None).unwrap();
        set_policy(true, None).unwrap();
        assert_eq!(policy().mode, "whitelist", "关开关必须清掉 shell 解锁");

        std::env::remove_var("POLARIS_COLLAB_DB");
        let _ = std::fs::remove_file(&tmp);
    }

    #[tokio::test]
    async fn timeout_kills_runaway_command() {
        let _g = crate::collab::db::TEST_LOCK.lock().unwrap();
        let tmp = with_tmp_db("timeout");
        set_policy(true, Some(30)).unwrap(); // shell 模式才好造一条长命令
        let mut lreq = req("", &[]);
        lreq.line = if cfg!(windows) {
            "Start-Sleep -Seconds 30".into()
        } else {
            "sleep 30".into()
        };
        lreq.timeout_secs = Some(2);
        let started = std::time::Instant::now();
        let r = run("tester", lreq).await.unwrap();
        assert!(r.timed_out, "应被超时终止: {:?}", r);
        assert_eq!(r.exit_code, None);
        assert!(
            started.elapsed().as_secs() < 10,
            "超时后必须立即返回,实际 {:?}",
            started.elapsed()
        );
        std::env::remove_var("POLARIS_COLLAB_DB");
        let _ = std::fs::remove_file(&tmp);
    }

    #[tokio::test]
    async fn audit_records_every_attempt() {
        let _g = crate::collab::db::TEST_LOCK.lock().unwrap();
        let tmp = with_tmp_db("audit");
        set_policy(true, None).unwrap();
        let _ = run("alice", req("cargo", &["--version"])).await.unwrap();
        let _ = run("mallory", req("rm", &["-rf", "/"])).await.unwrap_err();

        let rows = crate::collab::db::audit_recent(50).unwrap();
        assert!(
            rows.iter().any(|r| r.actor == "alice" && r.action == "exec.run"),
            "成功执行必须落审计"
        );
        assert!(
            rows.iter().any(|r| r.actor == "mallory" && r.action == "exec.denied"),
            "被拒的尝试也必须落审计(否则看不见有人在敲门)"
        );
        std::env::remove_var("POLARIS_COLLAB_DB");
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn clip_truncates_over_limit() {
        let (s, t) = clip(vec![b'x'; MAX_OUTPUT + 10]);
        assert!(t);
        assert_eq!(s.len(), MAX_OUTPUT);
        let (s2, t2) = clip(vec![b'y'; 10]);
        assert!(!t2);
        assert_eq!(s2.len(), 10);
    }
}
