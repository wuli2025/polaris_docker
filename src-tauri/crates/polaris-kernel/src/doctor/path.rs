//! 用户/进程 PATH 管理 + 启动期预热 (纯移动)。

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::probe::*;
use super::types::*;
use crate::runtime::procs::no_window;

// ───────────────────────── 用户 PATH (Windows) ─────────────────────────

/// 读「用户级 PATH」(注册表 HKCU\Environment), 经 PowerShell .NET API 拿。
#[cfg(windows)]
pub(crate) fn read_user_path() -> Option<String> {
    let mut cmd = Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "[Environment]::GetEnvironmentVariable('Path','User')",
    ]);
    cmd.stdin(Stdio::null());
    no_window(&mut cmd);
    let out = output_with_timeout(cmd, Duration::from_secs(20))?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(not(windows))]
pub(crate) fn read_user_path() -> Option<String> {
    None
}

/// dir 是否(忽略大小写/尾斜杠)出现在分号分隔的 PATH 串里。
pub(crate) fn path_contains_dir(path_str: &str, dir: &str) -> bool {
    let norm = |s: &str| s.trim().trim_end_matches(['\\', '/']).to_lowercase();
    let target = norm(dir);
    if target.is_empty() {
        return false;
    }
    path_str.split(';').any(|p| norm(p) == target)
}

/// 把 dir 前插进**当前进程 PATH** (若尚不在其中)。仅改本进程、不碰注册表; 返回是否真加了。
/// 这是「装完 / 启动后不重启即可 spawn claude」的底座 —— 子进程继承本进程 env。
pub(crate) fn prepend_process_path(dir: &str) -> bool {
    let dir = dir.trim();
    if dir.is_empty() {
        return false;
    }
    let proc_path = std::env::var("PATH").unwrap_or_default();
    if path_contains_dir(&proc_path, dir) {
        return false;
    }
    let sep = if cfg!(windows) { ';' } else { ':' };
    let new = if proc_path.is_empty() {
        dir.to_string()
    } else {
        format!("{dir}{sep}{proc_path}")
    };
    std::env::set_var("PATH", new);
    true
}

/// 把 dir 追加进「用户 PATH」(持久化, 注册表) + 当前进程 PATH (立即生效)。
/// Windows 专属; 其余平台仅尝试改进程 PATH。
pub(crate) fn ensure_dir_on_path(dir: &str) -> PathFixResult {
    let dir = dir.trim();
    if dir.is_empty() || !PathBuf::from(dir).exists() {
        return PathFixResult {
            ok: false,
            dir: Some(dir.to_string()),
            status: "skipped".into(),
            message: "目标目录不存在, 无法加入 PATH (请先安装)。".into(),
        };
    }

    // ① 当前进程 PATH (prepend → 本次会话立即能 spawn claude, 无需重启 app)
    prepend_process_path(dir);

    // ② 用户级持久化 PATH (Windows)。用显式 return 收尾, 避免 cfg 块尾表达式歧义。
    #[cfg(windows)]
    {
        if let Some(user_path) = read_user_path() {
            if path_contains_dir(&user_path, dir) {
                return PathFixResult {
                    ok: true,
                    dir: Some(dir.to_string()),
                    status: "present".into(),
                    message: format!("{dir} 已在用户 PATH 中 (进程 PATH 也已同步)。"),
                };
            }
        }
        return match append_user_path(dir) {
            Ok(_) => PathFixResult {
                ok: true,
                dir: Some(dir.to_string()),
                status: "added".into(),
                message: format!(
                    "已把 {dir} 加入用户 PATH 并同步到当前进程。新开的终端 / 重启后均生效。"
                ),
            },
            Err(e) => PathFixResult {
                ok: false,
                dir: Some(dir.to_string()),
                status: "process_only".into(),
                message: format!(
                    "已加入当前进程 PATH, 但持久化到用户 PATH 失败: {e}。可手动把 {dir} 加到 PATH。"
                ),
            },
        };
    }
    #[cfg(not(windows))]
    {
        return persist_unix_path(dir);
    }
}

/// 类 Unix(含 macOS) 持久化 PATH: 把 `export PATH="dir:$PATH"` 追加进 shell 配置
/// (zsh 为 macOS 默认, 同时照顾 bash/sh), 已存在则跳过。进程 PATH 已在调用处 prepend。
#[cfg(not(windows))]
fn persist_unix_path(dir: &str) -> PathFixResult {
    use std::io::Write;
    let home = match home_dir() {
        Some(h) => h,
        None => {
            return PathFixResult {
                ok: true,
                dir: Some(dir.to_string()),
                status: "process_only".into(),
                message: format!("已加入当前进程 PATH ({dir})。"),
            }
        }
    };
    let line = format!("export PATH=\"{dir}:$PATH\"");
    let mut wrote = false;
    let mut already = false;
    for rc in [".zshrc", ".zprofile", ".bash_profile", ".profile"] {
        let p = home.join(rc);
        let existing = std::fs::read_to_string(&p).unwrap_or_default();
        if existing.contains(dir) {
            already = true;
            continue;
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p)
        {
            let _ = writeln!(f, "\n# Added by Polaris\n{line}");
            wrote = true;
        }
    }
    if wrote {
        PathFixResult {
            ok: true,
            dir: Some(dir.to_string()),
            status: "added".into(),
            message: format!(
                "已把 {dir} 写进 shell 配置 (~/.zshrc 等) 并同步当前进程。新开终端即生效。"
            ),
        }
    } else if already {
        PathFixResult {
            ok: true,
            dir: Some(dir.to_string()),
            status: "present".into(),
            message: format!("{dir} 已在 shell 配置中 (进程 PATH 已同步)。"),
        }
    } else {
        PathFixResult {
            ok: true,
            dir: Some(dir.to_string()),
            status: "process_only".into(),
            message: format!("已加入当前进程 PATH ({dir})。"),
        }
    }
}

/// 通过 PowerShell .NET API 把 dir 追加进用户 PATH (会广播 WM_SETTINGCHANGE)。
#[cfg(windows)]
fn append_user_path(dir: &str) -> Result<(), String> {
    // 单引号转义: PowerShell 里单引号字符串内的 ' 写成 ''
    let safe = dir.replace('\'', "''");
    let script = format!(
        "$d = '{safe}'; \
$u = [Environment]::GetEnvironmentVariable('Path','User'); \
if ($null -eq $u) {{ $u = '' }}; \
$parts = $u.Split(';') | Where-Object {{ $_ -ne '' }}; \
if ($parts -notcontains $d) {{ \
  $base = $u.TrimEnd(';'); \
  if ($base -eq '') {{ $new = $d }} else {{ $new = $base + ';' + $d }}; \
  [Environment]::SetEnvironmentVariable('Path', $new, 'User'); \
  Write-Output 'ADDED' \
}} else {{ Write-Output 'PRESENT' }}"
    );
    let mut cmd = Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    cmd.stdin(Stdio::null());
    no_window(&mut cmd);
    let out = cmd
        .output()
        .map_err(|e| format!("调用 PowerShell 写 PATH 失败: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// 把 uv (与随包内置的 Python) 目录并进**本进程** PATH。
/// 内置那份放在用户自装之后 —— 用户装过就用他的, 没装才吃内置的。
/// 这里只上 uv/python(不会与系统命令重名), msys 的 unix 工具目录**不上进程 PATH**
/// (它有 find/sort 等与 System32 同名的 exe), 那批只前插进 claude 子进程,
/// 见 `doctor::probe::inject_bundled_runtime`。
pub fn ensure_uv_on_process_path() {
    if let Some(dir) = uv_bin_dir() {
        if dir.join(uv_exe_name()).exists() {
            prepend_process_path(&dir.to_string_lossy());
        }
    }
    for dir in [super::bundled::uv_dir(), super::bundled::python_dir()]
        .into_iter()
        .flatten()
    {
        prepend_process_path(&dir.to_string_lossy());
    }
}

/// 由一个具体的 claude 可执行文件路径推出「该上 PATH 的目录」。
/// npm 装解析到的常是 `node_modules/.../bin/claude.exe` —— 该上 PATH 的是 npm 全局前缀
/// (放 `claude.cmd` 的地方, npm 通常已替我们加好), 而非内部 bin 目录; 其余情况取父目录。
pub(crate) fn claude_dir_from_path(p: &std::path::Path) -> Option<PathBuf> {
    if p.components().any(|c| c.as_os_str() == "node_modules") {
        if let Some(prefix) = npm_global_prefix() {
            return Some(prefix);
        }
    }
    p.parent().map(|x| x.to_path_buf())
}

/// claude 应该落脚的目录 (用于「修复 PATH」): 已解析路径的父目录优先, 否则 ~/.local/bin。
pub(crate) fn claude_dir_for_fix(claude: &ToolStatus) -> Option<PathBuf> {
    if let Some(p) = &claude.path {
        let pb = PathBuf::from(p.replace('/', std::path::MAIN_SEPARATOR_STR));
        return claude_dir_from_path(&pb);
    }
    home_dir().map(|h| h.join(".local").join("bin"))
}

/// 装完 PowerShell 7 后, 把它的目录 (`C:\Program Files\PowerShell\7`) 塞进 PATH (进程 + 用户),
/// 让**本进程**后续 spawn 的 claude 立刻找到真身, 而不是 WindowsApps 里起不来的 Store 别名 —— 装完免重启即用。
/// 真身不存在 (没装成功) 时返回 None。
pub(crate) fn ensure_pwsh_on_path() -> Option<PathFixResult> {
    let exe = pwsh_candidates().into_iter().find(|p| p.exists())?;
    let dir = exe.parent()?.to_string_lossy().to_string();
    Some(ensure_dir_on_path(&dir))
}

/// 启动期环境预热 —— 让本进程**之后** spawn 的 claude CLI 一定「找得到、且有 shell 可用」,
/// 不必等用户走一遍「环境医生 / 安装」, 也不必重启 app。对应「环境配置时把 PATH 改成适合
/// claude code CLI 调用, 避免类似(找不到 claude / 找不到 shell)的问题」。
///
/// 只改**当前进程** env (set_var), **不写注册表** —— 启动期保持轻量、幂等、无副作用;
/// 持久化仍由「安装成功」与显式「修复 PATH」按钮负责。三件事, 每件仅在尚未满足时才动:
/// ① claude 所在目录 → 进程 PATH (即便 app 从一个 PATH 不含它的上下文被拉起也能裸名命中);
/// ② 真身 PowerShell 7 目录 → 进程 PATH (claude 在 Windows 靠 pwsh/git-bash 跑工具, 缺则报错);
/// ③ 找到 Git Bash 就设 `CLAUDE_CODE_GIT_BASH_PATH` (claude 在 Windows 默认偏好 git-bash)。
///
/// 内部会跑 where.exe / `npm prefix -g` (可能各几百 ms), 故在后台线程里做, 不阻塞 app 启动。
pub fn prime_path_for_claude() {
    std::thread::spawn(prime_path_for_claude_inner);
}

/// macOS/Linux: 从 Finder/Dock 启动的 GUI 进程只继承极简 PATH (`/usr/bin:/bin:/usr/sbin:/sbin`),
/// 拿不到用户 shell (`~/.zprofile`/`~/.profile` 里 —— 我们装 Node 时及 Homebrew 都写在这) 配的
/// node/npm/claude 目录 → `which`/spawn 全落空。跑一次**登录 shell** 把真实 PATH 取回来。
/// 用 `-lc`(登录、非交互): 读 `.zprofile`/`.profile`/`.bash_profile`, 不读 `.zshrc`, 既能拿到
/// 我们写的 node 目录与 Homebrew, 又避开交互式 shell 无 tty 时可能的卡顿。
#[cfg(not(windows))]
fn login_shell_path() -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let mut cmd = Command::new(&shell);
    cmd.args(["-lc", "printf %s \"$PATH\""]);
    cmd.stdin(Stdio::null());
    let out = output_with_timeout(cmd, Duration::from_secs(20))?;
    if !out.status.success() {
        return None;
    }
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if p.is_empty() {
        None
    } else {
        Some(p)
    }
}

/// 把登录 shell 的 PATH 里「进程 PATH 还没有的目录」并到进程 PATH 末尾 (系统目录仍优先,
/// 仅补充用户目录, 不抢占)。让本进程之后 `which`/spawn 与终端里行为一致。
#[cfg(not(windows))]
fn merge_login_path_into_process(login_path: &str) {
    use std::collections::HashSet;
    let cur = std::env::var("PATH").unwrap_or_default();
    let have: HashSet<String> = cur
        .split(':')
        .map(|s| s.trim_end_matches('/').to_string())
        .collect();
    let adds: Vec<&str> = login_path
        .split(':')
        .filter(|s| !s.is_empty() && !have.contains(&s.trim_end_matches('/').to_string()))
        .collect();
    if adds.is_empty() {
        return;
    }
    let merged = if cur.is_empty() {
        adds.join(":")
    } else {
        format!("{cur}:{}", adds.join(":"))
    };
    std::env::set_var("PATH", merged);
}

/// 预热的实际逻辑 (同步)。抽出来便于单测直接调用并断言, 公开入口只负责丢到后台线程。
pub(crate) fn prime_path_for_claude_inner() {
    // ⓪ macOS/Linux: 先并入登录 shell 的真实 PATH (Finder 启动只有极简 PATH), 再直接补上
    // 本应用装 Node 的目录 —— 即便 shell 配置因故没生效, node/npm/claude 也都找得到。
    #[cfg(not(windows))]
    {
        if let Some(lp) = login_shell_path() {
            merge_login_path_into_process(&lp);
        }
        for dir in node_dir_candidates() {
            if dir.exists() {
                prepend_process_path(&dir.to_string_lossy());
            }
        }
    }
    // ⓪.5 Windows: PATH 里找不到 node 时, 把 Node 安装目录补进**进程** PATH。
    // MSI/winget 只写注册表 PATH, **运行中的进程 PATH 是启动时的快照、不会刷新** → 用户在 app
    // 开着的时候装完 Node, 面板仍说没有 npm、更新检测也拿不到 npm。仅在 PATH 确实解析不出 node
    // 时才补, 不抢占用户自己配的 node (nvm 等多版本管理器)。
    #[cfg(windows)]
    if which_all("node").is_empty() {
        if let Some(dir) = node_dir_candidates()
            .into_iter()
            .find(|d| d.join("node.exe").exists())
        {
            prepend_process_path(&dir.to_string_lossy());
        }
    }
    // ① claude 目录上进程 PATH
    if let Some(exe) = resolve_claude_exe() {
        if let Some(dir) = claude_dir_from_path(&exe) {
            prepend_process_path(&dir.to_string_lossy());
        }
    }
    // ①.5 uv 目录上进程 PATH —— 脚本执行公约要求 claude 用 `uv run` 跑 Python, 它的 shell 得找得到 uv
    ensure_uv_on_process_path();
    // ② 真身 pwsh 目录上进程 PATH (滤掉 Store 别名: pwsh_candidates 只列 Program Files 真身)
    #[cfg(windows)]
    if let Some(exe) = pwsh_candidates().into_iter().find(|p| p.exists()) {
        if let Some(dir) = exe.parent() {
            prepend_process_path(&dir.to_string_lossy());
        }
    }
    // ③ Git Bash → 环境变量 (用户没显式设过才补)
    #[cfg(windows)]
    if std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH").is_none() {
        if let Some(bash) = git_bash_path() {
            std::env::set_var("CLAUDE_CODE_GIT_BASH_PATH", bash);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_contains_dir_is_case_and_slash_insensitive() {
        let p = r"C:\Program Files\PowerShell\7;C:\Users\mi\.local\bin";
        assert!(path_contains_dir(p, r"c:\program files\powershell\7")); // 大小写不敏感
        assert!(path_contains_dir(p, r"C:\Users\mi\.local\bin\")); // 尾斜杠归一
        assert!(!path_contains_dir(p, r"C:\Program Files\nodejs"));
        assert!(!path_contains_dir(p, "")); // 空目标不误命中
    }

    #[test]
    fn claude_dir_from_path_picks_parent_or_npm_prefix() {
        // 普通原生 exe → 取父目录
        let native = PathBuf::from(r"C:\Users\mi\.local\bin\claude.exe");
        assert_eq!(
            claude_dir_from_path(&native),
            Some(PathBuf::from(r"C:\Users\mi\.local\bin"))
        );
        // node_modules 路径 → 永不返回那个内部 bin 目录 (要么 npm 前缀, 要么至少不是 .../bin 自身的误用)
        let npmish = PathBuf::from(r"D:\npm\node_modules\@anthropic-ai\claude-code\bin\claude.exe");
        let dir = claude_dir_from_path(&npmish).expect("应解析出某个目录");
        assert!(
            !dir.ends_with("claude.exe"),
            "返回的应是目录而非文件: {dir:?}"
        );
    }

    /// 所有会改进程 PATH/env 的断言放进同一个测试串行跑, 避免与其他测试并发改 env 抢同一全局态。
    #[test]
    fn prime_and_prepend_behaviour() {
        // prepend 幂等: 首次真加、PATH 命中、再加返回 false
        let marker = r"Z:\polaris-test-marker-dir-do-not-exist";
        let first = prepend_process_path(marker);
        let path_now = std::env::var("PATH").unwrap_or_default();
        assert!(first, "首次应真的前插");
        assert!(
            path_contains_dir(&path_now, marker),
            "前插后 PATH 应含该目录"
        );
        assert!(
            !prepend_process_path(marker),
            "已在 PATH 中应返回 false (幂等)"
        );

        // resolve_claude_exe: 若本机装了 claude, 解析出的路径必须真实存在 (Windows 上偏好 .exe)
        if let Some(exe) = resolve_claude_exe() {
            assert!(exe.exists(), "解析出的 claude 路径应存在: {exe:?}");
            #[cfg(windows)]
            {
                let is_exe = exe
                    .extension()
                    .map(|e| e.eq_ignore_ascii_case("exe"))
                    .unwrap_or(false);
                // 本机同时有 .exe 与 .cmd 时, 必须挑 .exe (chat spawn 只认 .exe)
                let has_exe_alt = which_all("claude").iter().any(|p| {
                    p.extension()
                        .map(|e| e.eq_ignore_ascii_case("exe"))
                        .unwrap_or(false)
                });
                if has_exe_alt {
                    assert!(is_exe, "存在 .exe 候选时应优先解析为 .exe, 实得: {exe:?}");
                }
            }
        }

        // 预热不应 panic; 且 (Windows) 若真身 pwsh 存在, 其目录预热后应在进程 PATH 中 → claude 能找到 shell
        prime_path_for_claude_inner();
        #[cfg(windows)]
        if let Some(pwsh) = pwsh_candidates().into_iter().find(|p| p.exists()) {
            let dir = pwsh.parent().unwrap().to_string_lossy().to_string();
            let path_after = std::env::var("PATH").unwrap_or_default();
            assert!(
                path_contains_dir(&path_after, &dir),
                "预热后真身 pwsh 目录应在进程 PATH: {dir}"
            );
        }
    }
}
