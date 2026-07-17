//! 探测原语: which/版本探测、候选路径、工具检测、claude/uv 解析、子进程环境净化 (纯移动)。

use parking_lot::Mutex;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::types::*;
use crate::runtime::procs::no_window;

pub(crate) fn home_dir() -> Option<PathBuf> {
    directories::UserDirs::new().map(|u| u.home_dir().to_path_buf())
}

pub(crate) fn to_fwd(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// 跑一个子命令, 最多等 `timeout`; 超时则 kill 子进程并返回 None。
/// 探测类调用 (npm view / npm prefix / 版本号 / where / 读注册表 PATH 等) 用它兜底:
/// 网络卡死或进程僵死时不让 `env_check` 的探测线程永久阻塞。
/// stdout/stderr 各由独立线程读到 EOF, 避免子进程写满管道反压自锁。
pub(crate) fn output_with_timeout(
    mut cmd: Command,
    timeout: Duration,
) -> Option<std::process::Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().ok()?;
    let mut out_pipe = child.stdout.take()?;
    let mut err_pipe = child.stderr.take()?;
    let (tx_o, rx_o) = std::sync::mpsc::channel();
    let (tx_e, rx_e) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = std::io::Read::read_to_end(&mut out_pipe, &mut b);
        let _ = tx_o.send(b);
    });
    std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = std::io::Read::read_to_end(&mut err_pipe, &mut b);
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
                    return None; // 超时: 杀掉并放弃
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => return None,
        }
    };
    let stdout = rx_o.recv().unwrap_or_default();
    let stderr = rx_e.recv().unwrap_or_default();
    Some(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

/// 用 `where.exe`(Windows) / `which`(unix) 找出某命令的全部命中路径 (存在的才留)。
pub(crate) fn which_all(bin: &str) -> Vec<PathBuf> {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("where.exe");
        c.arg(bin);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = Command::new("which");
        c.args(["-a", bin]);
        c
    };
    cmd.stdin(Stdio::null());
    no_window(&mut cmd);
    let out = match output_with_timeout(cmd, Duration::from_secs(20)) {
        Some(o) => o,
        None => return Vec::new(),
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .collect()
}

/// 首个非空行 (优先 stdout, 个别工具把版本写到 stderr); 进程非零退出 → None。
fn first_line(out: &std::process::Output) -> Option<String> {
    if !out.status.success() {
        return None;
    }
    let pick = |bytes: &[u8]| -> Option<String> {
        String::from_utf8_lossy(bytes)
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .map(|s| s.to_string())
    };
    pick(&out.stdout).or_else(|| pick(&out.stderr))
}

/// Windows 上「这个命中能不能直接跑起来」的排序权重: `.exe` 可被 `Command::new` 直接 spawn;
/// `.cmd`/`.bat` 得过 `cmd /c`; **无扩展名**的(如 `C:\Program Files\nodejs\npm` 那个给 Git Bash
/// 用的 sh 脚本) 在 Windows 上压根不是可执行文件, 只配当最后兜底 —— 老代码「偏好 .exe, 否则取
/// where 的首个命中」正好在 npm 上踩这个坑: 首个命中就是那个 sh 脚本, 于是面板里 npm 那行显示
/// 的路径是个跑不起来的东西。类 Unix 无扩展名是常态, 恒 0。
fn exec_rank(p: &std::path::Path) -> u8 {
    #[cfg(windows)]
    {
        match p
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
        {
            Some(e) if e == "exe" => 0,
            Some(e) if e == "cmd" || e == "bat" => 1,
            _ => 2,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = p;
        0
    }
}

/// 用**解析出的绝对路径**构造一条可跑的命令 (已配好 stdin=null + 无窗口)。
/// Windows 上非 `.exe` 一律过 `cmd /c` —— `CreateProcessW` 只会补 `.exe`, 不认 `.cmd`/PATHEXT。
pub(crate) fn command_at(exe: &std::path::Path, args: &[&str]) -> Command {
    #[cfg(windows)]
    let mut cmd = {
        let is_exe = exe
            .extension()
            .map(|e| e.eq_ignore_ascii_case("exe"))
            .unwrap_or(false);
        if is_exe {
            let mut c = Command::new(exe);
            c.args(args);
            c
        } else {
            let mut c = Command::new("cmd");
            c.arg("/c").arg(exe).args(args);
            c
        }
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = Command::new(exe);
        c.args(args);
        c
    };
    cmd.stdin(Stdio::null());
    no_window(&mut cmd);
    cmd
}

/// 取某个**已解析出绝对路径**的工具的版本号。优先用它而非 `probe_version`(裸名):
/// 「装了但不在 PATH」是环境医生显式支持的状态 (面板有「已安装 (不在 PATH)」+「修复 PATH」),
/// 裸名探测在这种机器上必然拿不到版本号。
pub(crate) fn probe_version_at(exe: &std::path::Path, args: &[&str]) -> Option<String> {
    let out = output_with_timeout(command_at(exe, args), Duration::from_secs(20))?;
    first_line(&out)
}

/// 取某命令的版本号 (裸名走 PATH)。Windows 走 `cmd /c <bin> <args>` 以便正确解析 .exe/.cmd
/// (PATHEXT); 其余平台直接执行。仅作 `probe_version_at` 解析不出路径时的兜底。
pub(crate) fn probe_version(bin: &str, args: &[&str]) -> Option<String> {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd");
        let mut full = vec!["/c".to_string(), bin.to_string()];
        full.extend(args.iter().map(|s| s.to_string()));
        c.args(full);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = Command::new(bin);
        c.args(args);
        c
    };
    cmd.stdin(Stdio::null());
    no_window(&mut cmd);
    let out = output_with_timeout(cmd, Duration::from_secs(20))?;
    first_line(&out)
}

/// 在 Node 安装目录候选里按文件名铺开成完整路径候选。
fn exe_in_node_dirs(names: &[&str]) -> Vec<PathBuf> {
    node_dir_candidates()
        .into_iter()
        .flat_map(|d| names.iter().map(|n| d.join(n)).collect::<Vec<_>>())
        .collect()
}

/// Node.js 可执行文件候选 (给 detect 用)。
/// **必须给**: 老代码给 node/npm 传的是空候选, 于是「Node 确实装在 `C:\Program Files\nodejs`,
/// 但不在**本进程** PATH 里」时面板直接报「未安装」, 还劝用户重装一遍已经装好的 Node。
/// 这个状态在 Windows 上很常见 —— MSI/winget 只写注册表 PATH, **已在运行的进程 PATH 是快照,
/// 不会刷新**; app 被从一个尚无 Node 的上下文拉起也一样。
pub(crate) fn node_candidates() -> Vec<PathBuf> {
    exe_in_node_dirs(if cfg!(windows) {
        &["node.exe"]
    } else {
        &["node"]
    })
}

/// npm 可执行文件候选 (给 detect 用)。理由同 [`node_candidates`]。
pub(crate) fn npm_candidates() -> Vec<PathBuf> {
    exe_in_node_dirs(if cfg!(windows) {
        &["npm.cmd", "npm.exe"]
    } else {
        &["npm"]
    })
}

/// 解析一个「能直接跑」的 npm。PATH 命中按可执行性排序 (Windows: `npm.cmd` 优先于那个无扩展名的
/// sh 脚本), 全都不在 PATH 时退到 Node 安装目录候选 —— **用户刚装完 Node、本进程 PATH 尚未刷新**
/// 时也找得到, 不然「装了 Node 还说没 npm」。
pub(crate) fn resolve_npm_exe() -> Option<PathBuf> {
    let mut hits: Vec<PathBuf> = which_all("npm")
        .into_iter()
        .filter(|p| !is_app_exec_alias(p))
        .collect();
    hits.sort_by_key(|p| exec_rank(p)); // 稳定排序: 同级保持 where.exe 原序
    hits.into_iter()
        .next()
        .or_else(|| npm_candidates().into_iter().find(|p| p.exists()))
}

/// npm 全局安装前缀。走 `npm prefix -g` —— **用户可能改过前缀**(实测有人放在 `D:\Users\x\npm`,
/// 而非默认 `%APPDATA%\npm`), 硬编码默认值会漏掉。失败 / 目录不存在 → None。
pub(crate) fn npm_global_prefix() -> Option<PathBuf> {
    let npm = resolve_npm_exe()?;
    let out = output_with_timeout(command_at(&npm, &["prefix", "-g"]), Duration::from_secs(20))?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())?
        .to_string();
    let p = PathBuf::from(line);
    p.exists().then_some(p)
}

/// 某个 npm 全局前缀下 Claude Code 的**真·原生 exe** 路径
/// (`<prefix>/node_modules/@anthropic-ai/claude-code/bin/claude.exe`)。
/// postinstall 把平台二进制拷到这里; 这是可被 `Command::new` 直接 spawn 的目标,
/// 而 `<prefix>/claude.cmd` 只是调它的 shim。
fn npm_claude_native_exe(prefix: &std::path::Path) -> PathBuf {
    prefix
        .join("node_modules")
        .join("@anthropic-ai")
        .join("claude-code")
        .join("bin")
        .join("claude.exe")
}

/// 已知的 claude 可执行文件候选位置。原生 `.exe` 优先 (能直接 spawn),
/// npm 的 `claude.cmd` shim 仅作探测 / PATH 兜底。
pub(crate) fn claude_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(h) = home_dir() {
        // 官方原生脚本: ~/.local/bin/claude(.exe)
        v.push(h.join(".local").join("bin").join("claude.exe"));
        v.push(h.join(".local").join("bin").join("claude"));
        // macOS 免 sudo 装的 Node (~/.local/polaris-node) 的全局 bin: `npm i -g` 把 claude
        // 链到这里。mac GUI 从 Finder 启动时 PATH 极简、`npm prefix -g` 又拿不到 → 显式兜底,
        // 让重启后 chat spawn 仍找得到。
        v.push(
            h.join(".local")
                .join("polaris-node")
                .join("bin")
                .join("claude"),
        );
    }
    // npm 全局 (用户真实前缀): 先原生 exe, 再 shim
    if let Some(prefix) = npm_global_prefix() {
        v.push(npm_claude_native_exe(&prefix));
        v.push(prefix.join("claude.exe"));
        v.push(prefix.join("claude.cmd"));
    }
    // 默认前缀兜底 (拿不到 `npm prefix -g` 时, 例如 npm 不在 PATH)
    if let Some(h) = home_dir() {
        let appdata_npm = h.join("AppData").join("Roaming").join("npm");
        v.push(npm_claude_native_exe(&appdata_npm));
        v.push(appdata_npm.join("claude.cmd"));
        v.push(appdata_npm.join("claude.exe"));
    }
    v
}

/// chat.rs spawn 用的解析结果缓存 —— 避免每次发消息都跑 `where.exe` / `npm prefix -g`。
/// 安装成功后 (`stream_install`) 会清空, 下次重新解析。
pub(crate) static CLAUDE_EXE_CACHE: once_cell::sync::Lazy<Mutex<Option<PathBuf>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(None));

/// 子进程环境净化（借鉴 OpenCode 桌面端 sidecar：loopback 强制 NO_PROXY + 清干扰变量）。
/// 给将要 spawn 的 **宿主机 claude** 子进程套上两层防护：
///
/// ① **回环并进 NO_PROXY**：切到 Codex 时 claude 的 `ANTHROPIC_BASE_URL` 被指向
///    `http://127.0.0.1:{port}`（见 provider.rs 的 `codex_route_config`）。若用户配了系统级
///    `HTTP(S)_PROXY`（国内/企业网常见），claude 底层 HTTP 客户端会把这个 **loopback 请求也走代理**
///    → 连不上本地翻译代理、报「连接 ChatGPT 后端失败」。把回环列入 `NO_PROXY`/`no_proxy` 即绕开代理直连。
///    只补回环、不动其他代理设置 —— 代理本身（claude 直连远端 API 时要用）照常生效。
/// ② **清干扰继承变量**：`DEBUG`（让 Node 生态吐调试噪声、行为不可预测）；Linux 的 `LD_PRELOAD`（注入）。
/// ③ **root 下放行 bypassPermissions**：claude CLI 有条安全铁律——进程是 root(euid==0)时
///    拒绝 `--permission-mode=bypassPermissions` / `--dangerously-skip-permissions`，报
///    「cannot be used with root/sudo privileges」。但 **Docker 版容器必须跑 root**(群晖
///    bind mount 里 synoacl 失效 → 共享 000 权限 → 非 root 全读不了，见 nas-polaris-datasets-mount)，
///    一旦走到 kb.rs 硬编 bypass 的路径(文件中心 AI 归类 / KB 构建 / 回声层做梦 / fable 索引)
///    claude 必挂。官方逃生口 = 设 `IS_SANDBOX=1`，root 下即放行。仅 Linux 且确为 root 时设置，
///    桌面(非 root)不触发，无副作用。
pub fn harden_child_env(cmd: &mut Command) {
    for key in ["NO_PROXY", "no_proxy"] {
        let current = std::env::var(key).unwrap_or_default();
        cmd.env(key, merge_no_proxy(&current));
    }
    cmd.env_remove("DEBUG");
    #[cfg(target_os = "linux")]
    {
        cmd.env_remove("LD_PRELOAD");
        // SAFETY: getuid 是纯读、线程安全的 libc 调用，无副作用。
        if unsafe { libc::geteuid() } == 0 {
            cmd.env("IS_SANDBOX", "1");
        }
    }
}

/// 把回环主机（`127.0.0.1` / `localhost` / `::1`）并进既有 NO_PROXY 值：
/// 保留用户原有条目、大小写不敏感去重、已存在则不重复添加。抽纯函数便于单测。
pub fn merge_no_proxy(current: &str) -> String {
    let mut items: Vec<String> = current
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    for host in ["127.0.0.1", "localhost", "::1"] {
        if !items.iter().any(|v| v.eq_ignore_ascii_case(host)) {
            items.push(host.to_string());
        }
    }
    items.join(",")
}

/// 解析一个「可直接 spawn」的 claude 可执行文件全路径, 供 chat.rs 调起宿主机 CLI。
///
/// 为什么不让 chat.rs 用裸名 `Command::new("claude")`: Windows 的 `CreateProcessW` 解析裸名时
/// 只补 `.exe`、不查 PATHEXT, 而 **npm 装只在 PATH 放 `claude.cmd`** → 裸名根本找不到。
/// 这里偏好真·原生 `.exe` (PATH 命中的 .exe → 已知候选里的 .exe), 实在没有才回退到 `.cmd`;
/// 全部落空返回 None, 让调用方退回裸名靠 PATH。带进程内缓存。
pub fn resolve_claude_exe() -> Option<PathBuf> {
    // 命中缓存且文件仍在 → 直接用
    if let Some(p) = CLAUDE_EXE_CACHE.lock().as_ref() {
        if p.exists() {
            return Some(p.clone());
        }
    }
    let resolved = resolve_claude_exe_uncached();
    *CLAUDE_EXE_CACHE.lock() = resolved.clone();
    resolved
}

fn resolve_claude_exe_uncached() -> Option<PathBuf> {
    let is_exe = |p: &std::path::Path| {
        p.extension()
            .map(|e| e.eq_ignore_ascii_case("exe"))
            .unwrap_or(false)
    };
    let hits = which_all("claude"); // 已过滤为「存在的」路径
                                    // 1. PATH 命中里的 .exe (原生装常见)
    if let Some(p) = hits.iter().find(|p| is_exe(p)) {
        return Some(p.clone());
    }
    // 2. 已知候选里存在的 .exe (npm 装 → node_modules 里的原生 exe)
    let cands = claude_candidates();
    if let Some(p) = cands.iter().find(|p| is_exe(p) && p.exists()) {
        return Some(p.clone());
    }
    // 3. 退而求其次: 任意 PATH 命中 / 存在候选 (可能是 .cmd)
    hits.into_iter()
        .next()
        .or_else(|| cands.into_iter().find(|p| p.exists()))
}

pub(crate) fn pwsh_candidates() -> Vec<PathBuf> {
    vec![
        PathBuf::from(r"C:\Program Files\PowerShell\7\pwsh.exe"),
        PathBuf::from(r"C:\Program Files\PowerShell\7-preview\pwsh.exe"),
    ]
}

/// Node.js 可执行文件所在目录候选 (放 `node`/`npm`)。装完 Node 后用它把目录塞进进程 PATH,
/// 让同一会话紧接着的 npm/claude 安装立刻找得到 npm (免重启 app)。
pub(crate) fn node_dir_candidates() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        vec![
            PathBuf::from(r"C:\Program Files\nodejs"),
            PathBuf::from(r"C:\Program Files (x86)\nodejs"),
        ]
    }
    #[cfg(not(windows))]
    {
        let mut v = Vec::new();
        if let Some(h) = home_dir() {
            // 本应用免 sudo 装 Node 的落脚处 (见 MAC_NODE_INSTALL_SCRIPT), 优先
            v.push(h.join(".local").join("polaris-node").join("bin"));
            v.push(h.join(".local").join("bin"));
        }
        // Homebrew (Apple Silicon / Intel) 与系统常见位置
        v.push(PathBuf::from("/opt/homebrew/bin"));
        v.push(PathBuf::from("/usr/local/bin"));
        v
    }
}

/// Windows「应用执行别名」空壳: `%LOCALAPPDATA%\Microsoft\WindowsApps\` 下的 0 字节重解析点
/// (从 Microsoft Store 装 PowerShell 7 / Python 等会留下)。交互式终端里它能转发到 Store 真身,
/// 但**本应用是 GUI 进程、以 CREATE_NO_WINDOW 无控制台方式 spawn claude**, claude 再去拉这个
/// 别名时在该上下文下起不来 → 报「找不到 PowerShell」。故探测时把它当「没装」, 引导装
/// Program Files 里的真身 (普通 exe, 任何子进程都能稳定 spawn) 替代。
fn is_app_exec_alias(p: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        let in_windows_apps = p.components().any(|c| {
            c.as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case("WindowsApps")
        });
        if !in_windows_apps {
            return false;
        }
        // 0 字节 = 典型的执行别名占位 (reparse point), 不是真二进制
        std::fs::metadata(p).map(|m| m.len() == 0).unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        let _ = p;
        false
    }
}

/// 探测可用的 Git Bash (claude 在 Windows 上可接受的另一种 shell)。
/// 先认 `CLAUDE_CODE_GIT_BASH_PATH` 覆盖, 再扫常见安装位置。
/// 仅 Windows 需要 (扫的全是 Windows 路径); 类 Unix 用系统自带 shell, 不走这里。
#[cfg(windows)]
pub(crate) fn git_bash_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("CLAUDE_CODE_GIT_BASH_PATH") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files\Git\usr\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
}

/// 通用工具探测: which 命中 + 已知候选, 取首个可用; on_path = 是否被 PATH 发现。
pub(crate) fn detect(
    key: &str,
    name: &str,
    bin: &str,
    version_args: &[&str],
    candidates: &[PathBuf],
    required: bool,
    install_hint: &str,
) -> ToolStatus {
    // 滤掉 WindowsApps 的执行别名空壳 —— 它对无控制台 spawn 的 claude 不可用, 不能算「已装」
    let on_path_hits: Vec<PathBuf> = which_all(bin)
        .into_iter()
        .filter(|p| !is_app_exec_alias(p))
        .collect();
    let on_path = !on_path_hits.is_empty();

    // 解析出一个具体路径: PATH 命中按「能不能直接跑」排序 (见 exec_rank), 否则用存在的候选
    let resolved: Option<PathBuf> = {
        let mut hits = on_path_hits.clone();
        hits.sort_by_key(|p| exec_rank(p)); // 稳定排序: 同级保持 where.exe 原序
        hits.into_iter()
            .next()
            .or_else(|| candidates.iter().find(|p| p.exists()).cloned())
    };

    let found = resolved.is_some();
    // 按解析出的绝对路径探版本, 裸名仅作兜底 —— 否则「装了但不在 PATH」的机器上版本恒为空,
    // 面板只显示「已安装」而没有版本号, claude 那行还会连带让更新检测失效 (见 update.rs)。
    let version = resolved
        .as_deref()
        .and_then(|p| probe_version_at(p, version_args))
        .or_else(|| found.then(|| probe_version(bin, version_args)).flatten());

    let hint = if found {
        match &version {
            Some(v) => v.clone(),
            None => "已安装".to_string(),
        }
    } else {
        install_hint.to_string()
    };

    ToolStatus {
        key: key.to_string(),
        name: name.to_string(),
        found,
        version,
        path: resolved.as_deref().map(to_fwd),
        on_path,
        required,
        hint,
    }
}

// ───────────────────────── uv 解析 ─────────────────────────

pub(crate) fn uv_bin_dir() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".local").join("bin"))
}

/// uv 可执行文件名 (随平台)。
pub(crate) fn uv_exe_name() -> &'static str {
    if cfg!(windows) {
        "uv.exe"
    } else {
        "uv"
    }
}

/// 解析一个可直接 spawn 的 uv 路径: PATH 命中优先(滤掉 Store 占位符——uv 本身不是占位符,
/// 但走同一道防御闸更稳), 否则取 `~/.local/bin/uv(.exe)`。
pub(crate) fn resolve_uv_exe() -> Option<PathBuf> {
    if let Some(p) = which_all("uv").into_iter().find(|p| !is_app_exec_alias(p)) {
        return Some(p);
    }
    uv_bin_dir()
        .map(|d| d.join(uv_exe_name()))
        .filter(|p| p.exists())
}

/// 探测可用的 Git Bash, 供 chat.rs spawn 时显式喂给子 claude (跨平台签名; 类 Unix 恒 None)。
#[cfg(windows)]
pub fn detect_git_bash() -> Option<PathBuf> {
    git_bash_path()
}
#[cfg(not(windows))]
pub fn detect_git_bash() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows 上 `where npm` 首个命中是 `C:\Program Files\nodejs\npm` —— 那是给 Git Bash 用的
    /// sh 脚本, 在 Windows 上根本跑不起来。排序必须把 `npm.cmd` 顶到前面。
    #[test]
    #[cfg(windows)]
    fn exec_rank_prefers_runnable_over_extensionless_shell_script() {
        let sh = PathBuf::from(r"C:\Program Files\nodejs\npm");
        let cmd = PathBuf::from(r"C:\Program Files\nodejs\npm.cmd");
        let exe = PathBuf::from(r"C:\Program Files\nodejs\node.exe");
        assert!(exec_rank(&exe) < exec_rank(&cmd));
        assert!(exec_rank(&cmd) < exec_rank(&sh));
        // 按 where.exe 的真实输出顺序排完, 首个应是 .cmd 而非那个 sh 脚本
        let mut hits = vec![sh, cmd.clone()];
        hits.sort_by_key(|p| exec_rank(p));
        assert_eq!(hits.first(), Some(&cmd));
    }

    /// node/npm 的候选不能为空 —— 空候选正是「装了 Node 却报未安装」的根因。
    #[test]
    fn node_and_npm_candidates_are_non_empty() {
        assert!(!node_candidates().is_empty());
        assert!(!npm_candidates().is_empty());
    }

    #[test]
    fn merge_no_proxy_adds_loopback_to_empty() {
        let out = merge_no_proxy("");
        assert_eq!(out, "127.0.0.1,localhost,::1");
    }

    #[test]
    fn merge_no_proxy_preserves_existing_and_appends() {
        let out = merge_no_proxy("example.com, 10.0.0.5");
        assert_eq!(out, "example.com,10.0.0.5,127.0.0.1,localhost,::1");
    }

    #[test]
    fn merge_no_proxy_is_idempotent_case_insensitive() {
        // 用户已配了回环（含大小写差异）→ 不重复添加。
        let out = merge_no_proxy("LOCALHOST,127.0.0.1");
        assert_eq!(out, "LOCALHOST,127.0.0.1,::1");
        // 再并一次保持稳定（幂等）。
        assert_eq!(merge_no_proxy(&out), out);
    }
}
