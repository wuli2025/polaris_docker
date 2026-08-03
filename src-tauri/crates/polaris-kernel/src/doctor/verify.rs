//! 环境医生 · **深度校验**: 安装冲突扫描 + 「真的能跑」实测。
//!
//! `env_check` 回答的是「装没装」——**查得到文件就算就绪**。但真实机器上大量故障是
//! 「查得到、跑不了」或「这边跑得了、那边跑不了」:
//!
//! - **装重了**: npm 全局一份 + `~/.local/bin` 一份, 版本还不一样 —— 更新了其中一份,
//!   另一份仍被 PATH 先命中, 于是「更新完版本纹丝不动」;
//! - **终端与应用解析到不同的那份**: 应用启动时会预热进程 PATH (`prime_path_for_claude`),
//!   所以**应用能跑不等于用户在 CMD/PowerShell 里敲 `claude` 能跑** —— 反过来也一样;
//! - **Store 执行别名遮挡**: `%LOCALAPPDATA%\Microsoft\WindowsApps` 里的 0 字节占位符排在
//!   PATH 前面, 终端里敲下去直接跳应用商店;
//! - **外部 ANTHROPIC_\* 覆盖**: 用户自己在系统环境变量 / `~/.zshrc` 里设过 key 或 base_url,
//!   会盖掉应用内选的供应商, 表现为「面板选了 A, 实际走了 B」;
//! - **配置文件损坏**: `~/.claude.json` 半截 JSON → claude 每次启动即崩。
//!
//! 本模块的每一条结论都**真的起了子进程**去验, 而不是查文件在不在。三个层次:
//! ① `exec` —— 用解析出的绝对路径跑 `claude --version` (应用内 spawn 的同一条路径);
//! ② `terminal` —— **复现「新开一个终端」的 PATH 语义**再跑一次 (Windows 从注册表重建
//!    机器级+用户级 PATH 交给 `cmd.exe` 解析裸名; 类 Unix 把 PATH 压回系统最小集后走登录 shell),
//!    这才是「能不能在电脑终端里用」的唯一可信答案;
//! ③ `smoke` (deep) —— 真发一次最小请求 (`claude --print`), 端到端确证「真正可用」。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::path::*;
use super::probe::*;
use super::types::*;
use crate::runtime::procs::no_window;

/// 版本探测类子命令的墙钟上限 (与 probe.rs 的探测一致)。
const PROBE_TIMEOUT: Duration = Duration::from_secs(25);
/// 端到端冒烟的墙钟上限 —— 真发一次请求要过网络, 给足但必须有顶。
const SMOKE_TIMEOUT: Duration = Duration::from_secs(120);
/// 冒烟的固定参数。与 chat 真正 spawn 时同源 (`--print` + bypass 权限), 保证「这里能过 =
/// 对话里也能过」; 但**工具集只给 Read** —— 冒烟跑在用户真机上, 物理上不给写的能力。
/// 抽成常量是为了让单测能直接盯住它 (见 `smoke_args_are_read_only`), 防有人图省事放开权限。
const SMOKE_ARGS: [&str; 6] = [
    "--print",
    "--output-format",
    "text",
    "--permission-mode=bypassPermissions",
    "--allowedTools",
    "Read",
];

// ───────────────────────── 小工具 ─────────────────────────

fn ms_since(t: Instant) -> u64 {
    t.elapsed().as_millis() as u64
}

/// 截断多行输出, 只留前几行 —— 面板里展示用, 不是日志窗口。
fn brief(s: &str, max_lines: usize, max_chars: usize) -> Option<String> {
    let joined = s
        .lines()
        .map(|l| l.trim_end())
        .filter(|l| !l.trim().is_empty())
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n");
    if joined.is_empty() {
        return None;
    }
    Some(if joined.chars().count() > max_chars {
        let cut: String = joined.chars().take(max_chars).collect();
        format!("{cut}…")
    } else {
        joined
    })
}

/// 敏感值掩码 —— 只留头 4 尾 4。**绝不回传明文**: 这份报告会经数据面走到手机端。
fn mask(v: &str) -> String {
    let n = v.chars().count();
    if n <= 8 {
        return "••••".to_string();
    }
    let head: String = v.chars().take(4).collect();
    let tail: String = v.chars().skip(n - 4).collect();
    format!("{head}••••{tail}")
}

fn out_text(out: &std::process::Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    let e = String::from_utf8_lossy(&out.stderr);
    if !e.trim().is_empty() {
        if !s.trim().is_empty() {
            s.push('\n');
        }
        s.push_str(&e);
    }
    s
}

fn first_nonempty(s: &str) -> Option<String> {
    s.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|s| s.to_string())
}

/// 路径归一 (小写 + 去尾斜杠), 用于去重 / 比对。
fn norm(p: &Path) -> String {
    p.to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_lowercase()
}

/// 由一个 claude 可执行文件推出它所属的「安装根」——
/// npm 装的 `<prefix>/node_modules/@anthropic-ai/claude-code/bin/claude.exe` 与
/// `<prefix>/claude.cmd` 是**同一份安装的两个入口**, 必须归成一类, 否则一份安装会被
/// 误报成两份冲突。其余情况取父目录。
fn install_root(p: &Path) -> PathBuf {
    let comps: Vec<_> = p.components().collect();
    if let Some(i) = comps.iter().position(|c| c.as_os_str() == "node_modules") {
        return comps[..i].iter().collect();
    }
    p.parent().map(|x| x.to_path_buf()).unwrap_or_default()
}

/// 给安装根起个人话名字 (面板里比一长串路径好认)。
fn root_label(root: &Path) -> String {
    let n = norm(root);
    let home = home_dir().map(|h| norm(&h)).unwrap_or_default();
    if !home.is_empty() {
        if n == format!("{home}\\.local\\bin") || n == format!("{home}/.local/bin") {
            return "官方脚本 / 直装 (~/.local/bin)".into();
        }
        if n.contains("polaris-node") {
            return "应用内置便携 Node 的全局安装".into();
        }
    }
    if npm_global_prefix().map(|p| norm(&p)) == Some(n.clone()) {
        return "npm 全局安装".into();
    }
    if n.contains("windowsapps") {
        return "Microsoft Store 执行别名".into();
    }
    to_fwd(root)
}

/// 挑一个「能直接跑」的入口来探版本: 优先 `.exe`, 再 `.cmd`, 否则第一个。
fn best_entry(paths: &[PathBuf]) -> Option<PathBuf> {
    let rank = |p: &Path| -> u8 {
        match p
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
        {
            Some(e) if e == "exe" => 0,
            Some(e) if e == "cmd" || e == "bat" => 1,
            None => {
                if cfg!(windows) {
                    2
                } else {
                    0
                }
            }
            _ => 2,
        }
    };
    let mut v = paths.to_vec();
    v.sort_by_key(|p| rank(p));
    v.into_iter().next()
}

// ───────────────────────── 「新开终端」语义的探测 ─────────────────────────

/// 在**新开终端的环境**里探到的东西。
struct TerminalProbe {
    /// 终端里 `where claude` / `command -v claude` 的命中 —— **首个即终端实际会跑的那份**
    hits: Vec<PathBuf>,
    /// 终端里 `claude --version` 的输出首行 (跑起来了才有)
    version: Option<String>,
    /// 展示用命令行
    cmdline: String,
    /// 原始输出 (失败时给用户看)
    raw: String,
}

/// 复现「用户刚新开一个终端」的 PATH。
///
/// Windows: 新终端的 PATH = **注册表里的机器级 + 用户级**, 与本进程 PATH 无关 ——
/// 本进程那份早被 `prime_path_for_claude` 前插过 claude / node / pwsh 目录, 拿它去测
/// 「终端里能不能用」等于自问自答。读不到注册表 (极少见) 才退回进程 PATH。
#[cfg(windows)]
fn fresh_terminal_path() -> Option<String> {
    let machine = read_machine_path().unwrap_or_default();
    let user = read_user_path().unwrap_or_default();
    let parts: Vec<&str> = [machine.trim(), user.trim()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(parts.join(";"))
}

/// 类 Unix: 新终端的 PATH 由登录 shell 读 profile 现算。这里先把 PATH 压回系统最小集
/// (= GUI 进程从 Finder 拿到的那份), 再交给登录 shell 自己拼 —— 应用启动时并进进程 PATH
/// 的那些目录不会漏进来干扰判断。
#[cfg(not(windows))]
fn minimal_unix_path() -> &'static str {
    "/usr/bin:/bin:/usr/sbin:/sbin"
}

/// 给「模拟终端」的子进程套上干净环境: 覆写 PATH, 并摘掉应用自己注入的变量
/// (`CLAUDE_CODE_GIT_BASH_PATH` 是启动预热设的, 真终端里没有)。
fn terminal_env(cmd: &mut Command, path: Option<&str>) {
    if let Some(p) = path {
        cmd.env("PATH", p);
    }
    cmd.env_remove("CLAUDE_CODE_GIT_BASH_PATH");
    cmd.stdin(Stdio::null());
    no_window(cmd);
}

#[cfg(windows)]
fn terminal_probe() -> TerminalProbe {
    let path = fresh_terminal_path();
    let mk = |args: &[&str]| {
        let mut c = Command::new("cmd");
        c.arg("/d").arg("/c").args(args);
        terminal_env(&mut c, path.as_deref());
        c
    };
    // ① 终端会命中哪些 claude (where.exe 的顺序 = cmd 解析裸名的顺序)
    let hits = output_with_timeout(mk(&["where", "claude"]), PROBE_TIMEOUT)
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    // ② 真的敲一次 `claude --version`
    let out = output_with_timeout(mk(&["claude", "--version"]), PROBE_TIMEOUT);
    let raw = out.as_ref().map(out_text).unwrap_or_default();
    let version = out
        .as_ref()
        .filter(|o| o.status.success())
        .and_then(|o| first_nonempty(&String::from_utf8_lossy(&o.stdout)));
    TerminalProbe {
        hits,
        version,
        cmdline: "cmd /c claude --version   (PATH = 注册表 机器级 + 用户级)".into(),
        raw,
    }
}

#[cfg(not(windows))]
fn terminal_probe() -> TerminalProbe {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let base = minimal_unix_path();
    let mk = |script: &str| {
        let mut c = Command::new(&shell);
        c.args(["-lc", script]);
        terminal_env(&mut c, Some(base));
        c
    };
    let hits = output_with_timeout(mk("command -v claude"), PROBE_TIMEOUT)
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let out = output_with_timeout(mk("claude --version"), PROBE_TIMEOUT);
    let raw = out.as_ref().map(out_text).unwrap_or_default();
    let version = out
        .as_ref()
        .filter(|o| o.status.success())
        .and_then(|o| first_nonempty(&String::from_utf8_lossy(&o.stdout)));
    TerminalProbe {
        hits,
        version,
        cmdline: format!("{shell} -lc 'claude --version'   (PATH 压回系统最小集, 由登录 shell 自行加载)"),
        raw,
    }
}

// ───────────────────────── 各步实跑 ─────────────────────────

/// ① 应用内实跑: 用 `resolve_claude_exe` 解析出的**绝对路径**执行 —— 与 chat 真正 spawn
/// claude 时走的是同一条解析路径, 所以这一步绿 = 「在应用里发消息能跑起来」。
fn step_exec(exe: Option<&Path>) -> (bool, VerifyStep) {
    let t = Instant::now();
    let Some(exe) = exe else {
        return (
            false,
            VerifyStep {
                key: "exec".into(),
                name: "应用内可执行".into(),
                status: "fail".into(),
                detail: "没解析出任何 claude 可执行文件 —— 尚未安装, 或安装位置不在已知候选里。"
                    .into(),
                command: None,
                output: None,
                ms: ms_since(t),
            },
        );
    };
    let out = output_with_timeout(command_at(exe, &["--version"]), PROBE_TIMEOUT);
    let ok = out.as_ref().is_some_and(|o| o.status.success());
    let ver = out
        .as_ref()
        .and_then(|o| first_nonempty(&String::from_utf8_lossy(&o.stdout)));
    let raw = out.as_ref().map(out_text).unwrap_or_default();
    let detail = match (ok, &ver) {
        (true, Some(v)) => format!("跑起来了: {v}"),
        (true, None) => "进程正常退出, 但没有版本输出 (claude 输出格式可能变了)。".into(),
        (false, _) if raw.trim().is_empty() => {
            "起不来 —— 进程没有任何输出 (超时 / 被杀 / 文件损坏)。可尝试重新安装。".into()
        }
        (false, _) => "起不来 —— 文件存在但执行失败, 见下方输出。可尝试重新安装。".into(),
    };
    (
        ok,
        VerifyStep {
            key: "exec".into(),
            name: "应用内可执行".into(),
            status: if ok { "ok" } else { "fail" }.into(),
            detail,
            command: Some(format!("{} --version", to_fwd(exe))),
            output: brief(&raw, 6, 400),
            ms: ms_since(t),
        },
    )
}

/// ② 终端实跑 —— 「能不能在电脑终端里用」。
fn step_terminal(term: &TerminalProbe, app_runnable: bool, t: Instant) -> VerifyStep {
    let ok = term.version.is_some();
    let detail = match (ok, app_runnable) {
        (true, _) => {
            let v = term.version.clone().unwrap_or_default();
            let which = term
                .hits
                .first()
                .map(|p| format!("  (命中 {})", to_fwd(p)))
                .unwrap_or_default();
            format!("终端里敲 `claude` 能跑: {v}{which}")
        }
        (false, true) => "**应用内能跑, 但新开的终端里跑不起来** —— 典型是 claude 所在目录只进了\
             本进程 PATH, 没写进用户 PATH。点上方「修复 PATH」后新开终端即可。"
            .into(),
        (false, false) => "终端里也跑不起来 —— claude 尚未安装 / 安装损坏, 先把上一项修好。".into(),
    };
    VerifyStep {
        key: "terminal".into(),
        name: "电脑终端里可运行".into(),
        status: if ok { "ok" } else { "fail" }.into(),
        detail,
        command: Some(term.cmdline.clone()),
        output: (!ok).then(|| brief(&term.raw, 6, 400)).flatten(),
        ms: ms_since(t),
    }
}

/// ③ claude 依赖的 shell 实跑 —— claude 所有命令类工具都靠它。Windows 上必须是**真身**
/// pwsh 或 Git Bash (Store 别名在无控制台的子进程里起不来, 见 `is_app_exec_alias`)。
fn step_shell() -> VerifyStep {
    let t = Instant::now();
    #[cfg(windows)]
    let (exe, args, label): (Option<PathBuf>, Vec<&str>, &str) = {
        if let Some(bash) = git_bash_path() {
            (Some(bash), vec!["-c", "echo polaris-shell-ok"], "Git Bash")
        } else if let Some(p) = pwsh_candidates().into_iter().find(|p| p.exists()) {
            (
                Some(p),
                vec!["-NoProfile", "-NonInteractive", "-Command", "'polaris-shell-ok'"],
                "PowerShell 7",
            )
        } else {
            (None, vec![], "")
        }
    };
    #[cfg(not(windows))]
    let (exe, args, label): (Option<PathBuf>, Vec<&str>, &str) = (
        Some(PathBuf::from("/bin/sh")),
        vec!["-c", "echo polaris-shell-ok"],
        "系统 sh",
    );

    let Some(exe) = exe else {
        return VerifyStep {
            key: "shell".into(),
            name: "Claude 可用的 Shell".into(),
            status: "fail".into(),
            detail: "找不到可用 shell (真身 PowerShell 7 / Git Bash) —— claude 里的命令类工具会全部报错。\
                 正常情况下应用已内置 Git Bash, 出现此项说明内置资源缺失, 建议重装应用或装 PowerShell 7。"
                .into(),
            command: None,
            output: None,
            ms: ms_since(t),
        };
    };
    let out = output_with_timeout(command_at(&exe, &args), PROBE_TIMEOUT);
    let raw = out.as_ref().map(out_text).unwrap_or_default();
    let ok = out.as_ref().is_some_and(|o| o.status.success()) && raw.contains("polaris-shell-ok");
    let bundled = super::bundled::is_bundled(&exe);
    VerifyStep {
        key: "shell".into(),
        name: "Claude 可用的 Shell".into(),
        status: if ok { "ok" } else { "fail" }.into(),
        detail: if ok {
            format!(
                "{label} 实跑通过{}。",
                if bundled { " (随应用内置)" } else { "" }
            )
        } else {
            format!("{label} 起不来 —— claude 里执行命令的工具会失败。")
        },
        command: Some(format!("{} -c echo …", to_fwd(&exe))),
        output: (!ok).then(|| brief(&raw, 4, 300)).flatten(),
        ms: ms_since(t),
    }
}

/// claude 的配置文件里哪些是坏的 (存在但不是合法 JSON) —— 半截 JSON 会让 claude 每次启动即崩,
/// 而 `--version` 有时还能过, 所以必须单独查。返回 (路径, 错因)。
fn broken_config_files() -> Vec<(String, String)> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    [
        home.join(".claude.json"),
        home.join(".claude").join("settings.json"),
        home.join(".claude").join("settings.local.json"),
    ]
    .into_iter()
    .filter(|p| p.is_file())
    .filter_map(|p| {
        let txt = std::fs::read_to_string(&p).ok()?;
        if txt.trim().is_empty() {
            return Some((to_fwd(&p), "文件是空的".to_string()));
        }
        match serde_json::from_str::<serde_json::Value>(&txt) {
            Ok(_) => None,
            Err(e) => Some((to_fwd(&p), e.to_string())),
        }
    })
    .collect()
}

/// ④ 配置文件完好性。
fn step_config(broken: &[(String, String)]) -> VerifyStep {
    let t = Instant::now();
    if broken.is_empty() {
        return VerifyStep {
            key: "config".into(),
            name: "Claude 配置文件".into(),
            status: "ok".into(),
            detail: "~/.claude.json 与 ~/.claude/settings*.json 均可正常解析 (或尚未生成)。".into(),
            command: None,
            output: None,
            ms: ms_since(t),
        };
    }
    VerifyStep {
        key: "config".into(),
        name: "Claude 配置文件".into(),
        status: "fail".into(),
        detail: format!(
            "有 {} 个配置文件不是合法 JSON —— claude 启动时会直接报错退出。备份后删除损坏的那个即可 (claude 会重建)。",
            broken.len()
        ),
        command: None,
        output: brief(
            &broken
                .iter()
                .map(|(p, e)| format!("{p}: {e}"))
                .collect::<Vec<_>>()
                .join("\n"),
            4,
            400,
        ),
        ms: ms_since(t),
    }
}

/// ⑤ 认证就绪度。**只看有没有凭据, 不回传任何明文**。
/// 注意这一步永远不判 fail: 官方登录在 macOS 上可能存在 keychain 里、查不到文件也照样能用,
/// 真要确证得走深度冒烟 (step_smoke)。
fn step_auth() -> VerifyStep {
    let t = Instant::now();
    let injected: Vec<&str> = ["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"]
        .into_iter()
        .filter(|k| {
            std::env::var(k)
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
        })
        .collect();
    let base = std::env::var("ANTHROPIC_BASE_URL").unwrap_or_default();
    let creds = home_dir()
        .map(|h| h.join(".claude").join(".credentials.json"))
        .filter(|p| p.is_file());

    let (status, detail) = if !injected.is_empty() {
        (
            "ok",
            format!(
                "应用会为 claude 注入供应商凭据 ({}){}。",
                injected.join(" / "),
                if base.trim().is_empty() {
                    String::new()
                } else {
                    format!("; 上游 {}", base.trim())
                }
            ),
        )
    } else if creds.is_some() {
        (
            "ok",
            "已用 Claude 官方账号登录 (~/.claude/.credentials.json)。".to_string(),
        )
    } else {
        (
            "warn",
            "没查到可用凭据 —— 若还没在「供应商」页配 API、也没用官方账号登录, claude 起得来但发不出请求。\
             (macOS 官方登录可能存在钥匙串里, 查不到文件不代表没登录, 可用下方「深度检测」确证。)"
                .to_string(),
        )
    };
    VerifyStep {
        key: "auth".into(),
        name: "凭据就绪".into(),
        status: status.into(),
        detail,
        command: None,
        output: None,
        ms: ms_since(t),
    }
}

/// ⑥ 端到端冒烟 —— **真发一次最小请求**。这是「真正可用」的唯一硬证据: 前面几步全绿也可能
/// 卡在没登录 / key 失效 / 上游不通 / 被墙。只在 deep=true 时跑 (会消耗一点点额度)。
///
/// 参数与 chat 真正 spawn 时同源 (`--print` + 只读工具 + bypass 权限), 保证「这里能过 =
/// 对话里也能过」; 工作目录用临时目录, 不碰用户项目。
fn step_smoke(exe: Option<&Path>, app_runnable: bool) -> VerifyStep {
    let t = Instant::now();
    if !app_runnable {
        return VerifyStep {
            key: "smoke".into(),
            name: "端到端冒烟 (真发一次请求)".into(),
            status: "skip".into(),
            detail: "claude 都起不来, 冒烟跳过。".into(),
            command: None,
            output: None,
            ms: ms_since(t),
        };
    }
    let exe = exe.expect("app_runnable 为真时必有解析出的路径");
    let mut cmd = command_at(exe, &SMOKE_ARGS);
    cmd.current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    harden_child_env(&mut cmd); // loopback NO_PROXY + 内置运行时 + 清干扰变量
    crate::provider::scope_child_claude(&mut cmd); // 跟对话用同一家供应商

    // 提示词故意极短: 冒烟只为证明「这条链路通」, 不为产出内容。
    let prompt = "只回复两个字符: OK";
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return VerifyStep {
                key: "smoke".into(),
                name: "端到端冒烟 (真发一次请求)".into(),
                status: "fail".into(),
                detail: format!("调起 claude 失败: {e}"),
                command: Some(format!("{} --print …", to_fwd(exe))),
                output: None,
                ms: ms_since(t),
            }
        }
    };
    if let Some(mut si) = child.stdin.take() {
        use std::io::Write as _;
        let _ = si.write_all(prompt.as_bytes());
    } // drop → 关 stdin, claude 才知道输入结束

    // 墙钟看门狗: 到点 kill, 免得没配好凭据时挂死在这里。
    let out = wait_with_timeout(child, SMOKE_TIMEOUT);
    let (ok, raw) = match &out {
        Some(o) => (o.status.success(), out_text(o)),
        None => (false, format!("超时 {}s, 已终止。", SMOKE_TIMEOUT.as_secs())),
    };
    let body = out
        .as_ref()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let real_ok = ok && !body.is_empty();
    VerifyStep {
        key: "smoke".into(),
        name: "端到端冒烟 (真发一次请求)".into(),
        status: if real_ok { "ok" } else { "fail" }.into(),
        detail: if real_ok {
            format!("链路通 —— claude 真的回话了: {}", brief(&body, 1, 80).unwrap_or_default())
        } else if ok {
            "claude 正常退出但没有任何回复 —— 多半是没登录 / 未配 API key。".into()
        } else {
            "发请求失败 —— 见下方输出 (常见: 未登录、key 失效、上游不通或被代理挡住)。".into()
        },
        command: Some(format!("{} --print \"{prompt}\"", to_fwd(exe))),
        output: brief(&raw, 8, 600),
        ms: ms_since(t),
    }
}

/// 等一个已 spawn 的子进程, 超时则 kill 并回 None。stdout/stderr 各由独立线程读到 EOF,
/// 避免管道写满反压自锁 (同 `output_with_timeout` 的做法, 这里因需要先喂 stdin 才分开写)。
fn wait_with_timeout(mut child: std::process::Child, timeout: Duration) -> Option<std::process::Output> {
    let mut op = child.stdout.take()?;
    let mut ep = child.stderr.take()?;
    let (tx_o, rx_o) = std::sync::mpsc::channel();
    let (tx_e, rx_e) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = std::io::Read::read_to_end(&mut op, &mut b);
        let _ = tx_o.send(b);
    });
    std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = std::io::Read::read_to_end(&mut ep, &mut b);
        let _ = tx_e.send(b);
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    };
    Some(std::process::Output {
        status,
        stdout: rx_o.recv().unwrap_or_default(),
        stderr: rx_e.recv().unwrap_or_default(),
    })
}

// ───────────────────────── 安装冲突扫描 ─────────────────────────

fn conflict(
    key: &str,
    severity: &str,
    title: &str,
    detail: String,
    paths: Vec<String>,
    fixable: bool,
) -> EnvConflict {
    EnvConflict {
        key: key.into(),
        severity: severity.into(),
        title: title.into(),
        detail,
        paths,
        fixable,
    }
}

/// 用户**自己**设过的 ANTHROPIC_*/CLAUDE_* 环境变量 (持久化那一层, 不是本进程被应用改过的那份)。
/// Windows 读注册表用户级; 类 Unix 扫 shell 配置。返回 (变量名, 掩码后的值)。
fn external_managed_vars() -> Vec<(String, String)> {
    const KEYS: [&str; 5] = [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_MODEL",
        "CLAUDE_CONFIG_DIR",
    ];
    #[cfg(windows)]
    {
        // 一次 PowerShell 调用把 5 个用户级变量全取回来 (逐个取要开 5 个进程, 太慢)
        let names = KEYS
            .iter()
            .map(|k| format!("'{k}'"))
            .collect::<Vec<_>>()
            .join(",");
        let script = format!(
            "$o=@{{}}; foreach($n in {names}){{ $v=[Environment]::GetEnvironmentVariable($n,'User'); \
if($v){{ $o[$n]=$v }} }}; $o | ConvertTo-Json -Compress"
        );
        let mut cmd = Command::new("powershell");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
        cmd.stdin(Stdio::null());
        no_window(&mut cmd);
        let Some(out) = output_with_timeout(cmd, PROBE_TIMEOUT) else {
            return Vec::new();
        };
        let body = String::from_utf8_lossy(&out.stdout);
        let Ok(v) = serde_json::from_str::<serde_json::Value>(body.trim()) else {
            return Vec::new();
        };
        let Some(map) = v.as_object() else {
            return Vec::new();
        };
        return map
            .iter()
            .filter_map(|(k, val)| {
                let s = val.as_str()?.trim();
                (!s.is_empty()).then(|| {
                    // base_url / model 不是秘密, 明文展示更有用; key/token 一律掩码
                    let shown = if k.ends_with("_KEY") || k.ends_with("_TOKEN") {
                        mask(s)
                    } else {
                        s.to_string()
                    };
                    (k.clone(), shown)
                })
            })
            .collect();
    }
    #[cfg(not(windows))]
    {
        let Some(home) = home_dir() else {
            return Vec::new();
        };
        let mut found: BTreeMap<String, String> = BTreeMap::new();
        for rc in [".zshrc", ".zprofile", ".bash_profile", ".bashrc", ".profile"] {
            let Ok(txt) = std::fs::read_to_string(home.join(rc)) else {
                continue;
            };
            for line in txt.lines() {
                let l = line.trim();
                if l.starts_with('#') {
                    continue;
                }
                let l = l.strip_prefix("export ").unwrap_or(l);
                for k in KEYS {
                    let Some(rest) = l.strip_prefix(k).and_then(|r| r.strip_prefix('=')) else {
                        continue;
                    };
                    let val = rest.trim().trim_matches(['"', '\'']).trim();
                    if val.is_empty() {
                        continue;
                    }
                    let shown = if k.ends_with("_KEY") || k.ends_with("_TOKEN") {
                        mask(val)
                    } else {
                        val.to_string()
                    };
                    found.insert(format!("{k}  ({rc})"), shown);
                }
            }
        }
        return found.into_iter().collect();
    }
}

/// 「装重了」这一族冲突的**纯逻辑**部分: Store 别名遮挡 / 多份并存 / 版本不一致 /
/// 应用与终端跑的不是同一份。抽成不碰全局态的函数 —— 探测(`which`/`--version`)在外面做完喂进来,
/// 这样单测能用真实文件摆出各种冲突局面来验, 而不是靠「本机恰好装重了」才测得到。
///
/// `version_of` 由调用方注入 (真实实现是跑 `--version`; 测试里给个查表函数)。
fn install_conflicts(
    all: &[PathBuf],
    app_exe: Option<&Path>,
    term_first: Option<&Path>,
    version_of: &dyn Fn(&Path) -> Option<String>,
) -> Vec<EnvConflict> {
    let mut out: Vec<EnvConflict> = Vec::new();

    // Store 执行别名: 排在 PATH 前面就会把终端里的 `claude` 劫持到应用商店
    let aliases: Vec<&PathBuf> = all.iter().filter(|p| is_app_exec_alias(p)).collect();
    if !aliases.is_empty() {
        out.push(conflict(
            "claude-store-alias",
            "medium",
            "存在 Microsoft Store 执行别名占位",
            "`WindowsApps` 下的 0 字节占位符会抢在真身之前被 PATH 命中 —— 终端里敲 `claude` \
             可能直接跳到应用商店。可在「设置 → 应用 → 应用执行别名」里关掉它。"
                .into(),
            aliases.iter().map(|p| to_fwd(p)).collect(),
            false,
        ));
    }

    // ── ② 按安装根分组: 一份安装的多个入口归一类, 真正的多份并存才算冲突 ──
    let real: Vec<PathBuf> = all
        .iter()
        .filter(|p| !is_app_exec_alias(p))
        .cloned()
        .collect();
    let mut groups: BTreeMap<String, (PathBuf, Vec<PathBuf>)> = BTreeMap::new();
    for p in &real {
        let root = install_root(p);
        groups
            .entry(norm(&root))
            .or_insert_with(|| (root, Vec::new()))
            .1
            .push(p.clone());
    }

    // 每份安装探一次版本 (用它自己那个能直接跑的入口)
    let mut versions: BTreeMap<String, Option<String>> = BTreeMap::new();
    for (k, (_, entries)) in &groups {
        let v = best_entry(entries).and_then(|e| version_of(&e));
        versions.insert(k.clone(), v);
    }

    if groups.len() > 1 {
        let lines: Vec<String> = groups
            .iter()
            .map(|(k, (root, _))| {
                let v = versions
                    .get(k)
                    .cloned()
                    .flatten()
                    .unwrap_or_else(|| "版本未知".into());
                format!("{} — {v}  [{}]", to_fwd(root), root_label(root))
            })
            .collect();
        out.push(conflict(
            "claude-multi",
            "high",
            &format!("检测到 {} 份 Claude Code 并存", groups.len()),
            "多份并存时, 到底跑哪份完全取决于 PATH 顺序 —— 常见后果是「更新完版本号纹丝不动」\
             (更新的是 A, 跑的是 B)。建议只保留一份: 若用应用的一键安装, 保留 `~/.local/bin` 那份, \
             用 `npm uninstall -g @anthropic-ai/claude-code` 删掉 npm 那份。"
                .into(),
            lines,
            false,
        ));

        // 版本还不一致 → 更凶: 行为在两个终端里都不一样
        let distinct: std::collections::BTreeSet<String> =
            versions.values().flatten().cloned().collect();
        if distinct.len() > 1 {
            out.push(conflict(
                "claude-version-mismatch",
                "high",
                "并存的 Claude Code 版本不一致",
                "同一台机器上跑出两个不同版本 —— 应用里和终端里的行为、可用参数都会不同。\
                 删到只剩一份即可。"
                    .into(),
                distinct.into_iter().collect(),
                false,
            ));
        }
    }

    // ── ③ 应用解析到的那份 vs 终端会跑的那份 ──
    if let (Some(app), Some(t0)) = (app_exe, term_first) {
        let a = norm(&install_root(app));
        let b = norm(&install_root(t0));
        if a != b {
            out.push(conflict(
                "claude-app-vs-terminal",
                "high",
                "应用与终端跑的不是同一份 Claude Code",
                "应用里发消息用的是前者, 你在终端里敲 `claude` 用的是后者 —— 版本、登录状态、\
                 MCP 配置都可能对不上。删掉多余的那份, 或调整 PATH 顺序让两边一致。"
                    .into(),
                vec![
                    format!("应用: {}", to_fwd(app)),
                    format!("终端: {}", to_fwd(t0)),
                ],
                false,
            ));
        }
    }

    out
}

/// 扫描全部安装冲突。`term` 是「新开终端」那次探测的结果 —— 判「终端与应用跑的不是同一份」全靠它。
fn scan_conflicts(
    app_exe: Option<&Path>,
    term: &TerminalProbe,
    broken_cfg: &[(String, String)],
) -> Vec<EnvConflict> {
    // ── ①②③ 收集本机所有 claude 入口 (PATH 命中 + 终端命中 + 已知候选), 去重后交给纯逻辑 ──
    let mut all: Vec<PathBuf> = which_all("claude");
    all.extend(term.hits.iter().cloned().filter(|p| p.exists()));
    all.extend(claude_candidates().into_iter().filter(|p| p.exists()));
    let mut seen = std::collections::HashSet::new();
    all.retain(|p| seen.insert(norm(p)));

    let mut out = install_conflicts(
        &all,
        app_exe,
        term.hits.first().map(|p| p.as_path()),
        &|p| probe_version_at(p, &["--version"]),
    );

    // ── ④ Node / npm 侧的冲突 ──
    let node_dirs: std::collections::BTreeSet<String> = which_all("node")
        .iter()
        .filter(|p| !is_app_exec_alias(p))
        .filter_map(|p| p.parent().map(norm))
        .collect();
    if node_dirs.len() > 1 {
        out.push(conflict(
            "node-multi",
            "low",
            &format!("PATH 上有 {} 份 Node.js", node_dirs.len()),
            "多版本共存 (nvm / fnm / 系统装 混用) 本身没问题, 但 `npm i -g` 装的 claude 只对\
             当前那一份可见 —— 切换 Node 版本后可能突然「找不到 claude」。"
                .into(),
            node_dirs.iter().map(|s| s.replace('\\', "/")).collect(),
            false,
        ));
    }
    // npm 装的 claude 需要 Node 18+
    let npm_installed = app_exe.is_some_and(|p| {
        p.components().any(|c| c.as_os_str() == "node_modules")
            || npm_global_prefix().is_some_and(|pre| p.parent() == Some(pre.as_path()))
    });
    if npm_installed {
        if let Some(major) = probe_version("node", &["--version"])
            .and_then(|v| {
                v.trim()
                    .trim_start_matches('v')
                    .split('.')
                    .next()
                    .map(|s| s.to_string())
            })
            .and_then(|s| s.parse::<u32>().ok())
        {
            if major < 18 {
                out.push(conflict(
                    "node-too-old",
                    "medium",
                    &format!("Node.js 版本过低 (v{major})"),
                    "npm 方式安装的 Claude Code 需要 Node 18 以上, 更低版本会在启动时报语法错误。\
                     升级 Node, 或改用应用的一键安装 (直抓平台包, 完全不依赖 Node)。"
                        .into(),
                    vec![format!("node v{major}")],
                    false,
                ));
            }
        }
    }
    // npm 全局前缀不在用户 PATH → npm 装的任何全局命令在新终端里都找不到
    #[cfg(windows)]
    if let (Some(prefix), Some(user_path)) = (npm_global_prefix(), read_user_path()) {
        if !path_contains_dir(&user_path, &prefix.to_string_lossy()) {
            out.push(conflict(
                "npm-prefix-off-path",
                "medium",
                "npm 全局目录不在用户 PATH 里",
                "`npm i -g` 装的命令 (含 claude) 在新开的终端里都会「找不到」。点「修复 PATH」\
                 或手动把该目录加进用户环境变量。"
                    .into(),
                vec![to_fwd(&prefix)],
                true,
            ));
        }
    }

    // ── ⑤ 外部设置的 ANTHROPIC_* / CLAUDE_CONFIG_DIR ──
    let ext = external_managed_vars();
    if !ext.is_empty() {
        out.push(conflict(
            "external-anthropic-env",
            "medium",
            "系统里有你自己设的 Claude 相关环境变量",
            "这些变量会被 claude 直接读取, 可能盖掉应用内「供应商」页选的那家 —— 表现为\
             「面板选了 A, 实际走了 B」或反复报鉴权失败。若不是刻意为之, 建议删掉它们, \
             把供应商配置交给应用统一管。"
                .into(),
            ext.into_iter().map(|(k, v)| format!("{k} = {v}")).collect(),
            false,
        ));
    }

    // ── ⑥ 配置文件损坏 (与 step_config 同源, 这里再登记一条冲突以便统一展示严重度) ──
    if !broken_cfg.is_empty() {
        out.push(conflict(
            "claude-config-broken",
            "high",
            "Claude 配置文件损坏",
            "不是合法 JSON 的配置会让 claude 启动即崩。备份后删掉损坏的那个文件, claude 会自动重建。"
                .into(),
            broken_cfg.iter().map(|(p, e)| format!("{p} — {e}")).collect(),
            false,
        ));
    }

    // 严重度排序: high → medium → low
    let rank = |s: &str| match s {
        "high" => 0,
        "medium" => 1,
        _ => 2,
    };
    out.sort_by_key(|c| rank(&c.severity));
    out
}

// ───────────────────────── Command ─────────────────────────

/// 深度校验。`deep=true` 时额外真发一次最小请求做端到端冒烟 (会走网络、消耗极少量额度)。
///
/// 桌面端 async + spawn_blocking: 内部要串起十来个子进程探测 (含一次可能长达 2 分钟的冒烟),
/// 同步跑会把 Tauri 主线程钉死。server flavor 的 dispatch 本就在 spawn_blocking 中, 保持同步。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn env_verify(deep: bool) -> VerifyReport {
    tauri::async_runtime::spawn_blocking(move || env_verify_sync(deep))
        .await
        .unwrap_or_else(|_| env_verify_sync(deep))
}
#[cfg(not(feature = "desktop"))]
pub fn env_verify(deep: bool) -> VerifyReport {
    env_verify_sync(deep)
}

/// 同步版本体。桌面 flavor 下 [`env_verify`] 是 async(见上),所以**不带运行时的调用方**
/// (examples / 测试 / 脚本)必须走这条,否则拿到的是个 Future,取字段即编译不过。
pub fn env_verify_sync(deep: bool) -> VerifyReport {
    let os = std::env::consts::OS.to_string();
    let exe = resolve_claude_exe();

    let (app_runnable, exec_step) = step_exec(exe.as_deref());

    let t_term = Instant::now();
    let term = terminal_probe();
    let terminal_runnable = term.version.is_some();

    let broken = broken_config_files();

    let mut steps = vec![
        exec_step,
        step_terminal(&term, app_runnable, t_term),
        step_shell(),
        step_config(&broken),
        step_auth(),
    ];
    if deep {
        steps.push(step_smoke(exe.as_deref(), app_runnable));
    }

    let conflicts = scan_conflicts(exe.as_deref(), &term, &broken);
    let has_high = conflicts.iter().any(|c| c.severity == "high");
    let shell_ok = steps.iter().any(|s| s.key == "shell" && s.status == "ok");
    let smoke_bad = steps
        .iter()
        .any(|s| s.key == "smoke" && s.status == "fail");
    let ok = app_runnable && shell_ok && !has_high && !smoke_bad;

    let summary = if !app_runnable {
        "Claude Code 跑不起来 —— 先按上面的提示装好 / 修好。".to_string()
    } else if has_high {
        format!(
            "能跑, 但检测到 {} 处会真出问题的安装冲突, 建议先清理。",
            conflicts.iter().filter(|c| c.severity == "high").count()
        )
    } else if !shell_ok {
        "Claude Code 能起来, 但没有可用 shell —— 它执行命令的工具会全部失败。".to_string()
    } else if !terminal_runnable {
        "应用内一切正常; 但在电脑终端里敲 `claude` 还不可用 —— 点「修复 PATH」即可。".to_string()
    } else if smoke_bad {
        "环境本身没问题, 但真发请求失败 —— 多半是凭据 / 网络, 见冒烟那一项。".to_string()
    } else if deep {
        "全部通过 —— 应用内、电脑终端里都能跑, 且真发请求成功。".to_string()
    } else {
        "全部通过 —— 应用内、电脑终端里都能跑 (想确证能真发请求, 点「深度检测」)。".to_string()
    };

    VerifyReport {
        os,
        ok,
        app_runnable,
        terminal_runnable,
        deep,
        steps,
        conflicts,
        summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// npm 装的两个入口 (前缀根的 shim + node_modules 里的原生 exe) 必须归成**同一份安装**,
    /// 否则一份安装会被误报成「两份并存」的 high 冲突, 天天吓用户。
    #[test]
    fn install_root_groups_npm_entries_together() {
        let shim = PathBuf::from(r"D:\npm\claude.cmd");
        let native =
            PathBuf::from(r"D:\npm\node_modules\@anthropic-ai\claude-code\bin\claude.exe");
        assert_eq!(norm(&install_root(&shim)), norm(&install_root(&native)));
        // 原生脚本装的那份是**另一份**, 不能被并进去
        let local = PathBuf::from(r"C:\Users\x\.local\bin\claude.exe");
        assert_ne!(norm(&install_root(&local)), norm(&install_root(&shim)));
    }

    /// 探版本要挑「能直接跑」的入口: Windows 上 `.exe` > `.cmd` > 无扩展名的 sh 脚本。
    #[test]
    #[cfg(windows)]
    fn best_entry_prefers_exe() {
        let v = vec![
            PathBuf::from(r"D:\npm\claude"),
            PathBuf::from(r"D:\npm\claude.cmd"),
            PathBuf::from(r"D:\npm\claude.exe"),
        ];
        assert_eq!(best_entry(&v), Some(PathBuf::from(r"D:\npm\claude.exe")));
    }

    /// 掩码必须真的掩住: 不能把明文 key 顺着报告漏到手机端。
    #[test]
    fn mask_hides_secret_body() {
        let m = mask("sk-ant-api03-abcdefghijklmnop");
        assert!(m.starts_with("sk-a") && m.ends_with("mnop"));
        assert!(!m.contains("api03"));
        assert_eq!(mask("short"), "••••"); // 短值整条掩掉, 不泄露长度以外的信息
    }

    #[test]
    fn brief_trims_and_drops_blank_lines() {
        assert_eq!(brief("", 3, 100), None);
        assert_eq!(brief("  \n \n", 3, 100), None);
        assert_eq!(brief("a\n\nb\nc\nd", 3, 100).as_deref(), Some("a\nb\nc"));
        assert_eq!(brief("abcdef", 3, 3).as_deref(), Some("abc…"));
    }

    /// 一份 npm 安装的两个入口 **不得**被报成冲突 —— 这是最容易误报、也最吓人的一条。
    #[test]
    fn single_npm_install_reports_no_conflict() {
        let all = vec![
            PathBuf::from(r"D:\npm\claude.cmd"),
            PathBuf::from(r"D:\npm\node_modules\@anthropic-ai\claude-code\bin\claude.exe"),
        ];
        let v = |_: &Path| Some("2.1.220".to_string());
        let out = install_conflicts(&all, Some(&all[1]), Some(&all[0]), &v);
        assert!(out.is_empty(), "同一份安装不该报冲突, 实得: {out:?}");
    }

    /// npm 一份 + 原生脚本一份, 版本还不同 —— 必须同时报「多份并存」与「版本不一致」,
    /// 且两条都是 high (这正是「更新完版本号纹丝不动」的根因)。
    #[test]
    fn two_installs_with_different_versions_report_both_conflicts() {
        let npm = PathBuf::from(r"D:\npm\claude.cmd");
        let native = PathBuf::from(r"C:\Users\x\.local\bin\claude.exe");
        let all = vec![npm.clone(), native.clone()];
        let v = |p: &Path| {
            Some(if p == npm.as_path() { "2.0.1" } else { "2.1.220" }.to_string())
        };
        let out = install_conflicts(&all, Some(&native), Some(&npm), &v);
        let keys: Vec<&str> = out.iter().map(|c| c.key.as_str()).collect();
        assert!(keys.contains(&"claude-multi"), "应报多份并存: {keys:?}");
        assert!(
            keys.contains(&"claude-version-mismatch"),
            "版本不同应额外报版本冲突: {keys:?}"
        );
        // 应用解析到 native、终端首个命中 npm → 还要报「两边跑的不是同一份」
        assert!(
            keys.contains(&"claude-app-vs-terminal"),
            "应用与终端解析到不同安装时必须报出来: {keys:?}"
        );
        assert!(out.iter().all(|c| c.severity == "high"));
        // 报告里要能看到两份各自的版本, 否则用户不知道该删哪个
        let multi = out.iter().find(|c| c.key == "claude-multi").unwrap();
        assert!(multi.paths.iter().any(|l| l.contains("2.0.1")));
        assert!(multi.paths.iter().any(|l| l.contains("2.1.220")));
    }

    /// Store 执行别名 (WindowsApps 下的 0 字节占位) 必须被认出来并单独报, 且**不能**被
    /// 当成一份真安装并进「多份并存」—— 否则干净机器会平白多出一条 high 冲突。
    #[test]
    #[cfg(windows)]
    fn store_alias_is_flagged_but_not_counted_as_install() {
        let dir = std::env::temp_dir()
            .join("polaris-verify-test")
            .join("WindowsApps");
        std::fs::create_dir_all(&dir).expect("建测试目录");
        let alias = dir.join("claude.exe");
        std::fs::write(&alias, b"").expect("造 0 字节占位符"); // 真身别名就是 0 字节重解析点
        let real = PathBuf::from(r"C:\Users\x\.local\bin\claude.exe");

        let out = install_conflicts(
            &[alias.clone(), real.clone()],
            Some(&real),
            Some(&real),
            &|_| Some("2.1.220".into()),
        );
        let keys: Vec<&str> = out.iter().map(|c| c.key.as_str()).collect();
        assert!(keys.contains(&"claude-store-alias"), "应认出别名: {keys:?}");
        assert!(
            !keys.contains(&"claude-multi"),
            "别名不是一份安装, 不该凑成「多份并存」: {keys:?}"
        );
        let _ = std::fs::remove_file(&alias);
    }

    /// 冲突按严重度排序 —— 面板从上往下看, high 必须在最前面。
    #[test]
    fn conflicts_sorted_high_first() {
        let mut v = vec![
            conflict("a", "low", "t", "d".into(), vec![], false),
            conflict("b", "high", "t", "d".into(), vec![], false),
            conflict("c", "medium", "t", "d".into(), vec![], false),
        ];
        let rank = |s: &str| match s {
            "high" => 0,
            "medium" => 1,
            _ => 2,
        };
        v.sort_by_key(|c| rank(&c.severity));
        assert_eq!(
            v.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(),
            vec!["b", "c", "a"]
        );
    }

    /// 冒烟必须**只读**: 它跑在用户真机上, 参数里绝不能出现放开写权限的工具集。
    /// (防回归 —— 有人图省事把 allowedTools 改成全放开, 冒烟就会真去改用户的文件。)
    #[test]
    fn smoke_args_are_read_only() {
        let tools_idx = SMOKE_ARGS
            .iter()
            .position(|a| *a == "--allowedTools")
            .expect("冒烟必须显式限定工具集");
        assert_eq!(SMOKE_ARGS[tools_idx + 1], "Read", "冒烟只放行 Read");
        for bad in ["Write", "Edit", "Bash", "acceptEdits"] {
            assert!(
                !SMOKE_ARGS.iter().any(|a| a.contains(bad)),
                "冒烟参数不得含 {bad}"
            );
        }
    }

    /// 整条链路不 panic, 且字段自洽 (本机装没装 claude 都要能跑完)。
    #[test]
    fn verify_sync_is_self_consistent() {
        let r = env_verify_sync(false);
        assert!(!r.deep);
        assert!(!r.steps.is_empty());
        assert!(
            r.steps.iter().all(|s| matches!(
                s.status.as_str(),
                "ok" | "fail" | "warn" | "skip"
            )),
            "step.status 只能是这四种"
        );
        assert!(
            r.conflicts
                .iter()
                .all(|c| matches!(c.severity.as_str(), "high" | "medium" | "low")),
            "conflict.severity 只能是这三种"
        );
        // 没装 claude 时 ok 必须为 false —— 不能把「查不到」报成健康
        if !r.app_runnable {
            assert!(!r.ok);
        }
        assert!(!r.summary.is_empty());
    }
}
