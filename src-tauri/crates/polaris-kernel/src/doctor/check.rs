//! 环境体检命令 env_check / env_fix_path (纯移动)。

use super::path::*;
use super::probe::*;
use super::types::*;

// ───────────────────────── Commands ─────────────────────────

/// 环境体检。桌面端 async + spawn_blocking:它是首启的「启动门」(App 相位 env 阶段
/// await 它才放行进 ready),同步版会把 6 个 `xxx --version` 子进程探测串行跑在
/// Tauri 主线程上,Windows 上累加秒级、直接拖慢首屏。丢进阻塞线程池,主线程零阻塞。
/// server flavor 无 UI 主线程、dispatch 本就在 spawn_blocking 中,保持同步直调。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn env_check() -> EnvReport {
    tauri::async_runtime::spawn_blocking(env_check_sync)
        .await
        // JoinError 仅在探测代码 panic 时出现(detect 自带超时,正常不会);
        // 兜底同步重跑一次,保证命令契约(总能给出 EnvReport)不变。
        .unwrap_or_else(|_| env_check_sync())
}
#[cfg(not(feature = "desktop"))]
pub fn env_check() -> EnvReport {
    env_check_sync()
}

/// 同步核:server flavor 与本文件内部(修 PATH / 装完复检)直接调用。
/// 6 个工具探测互不依赖,用 scoped threads 并行跑 → 总耗时 = 最慢的单个探测。
pub(crate) fn env_check_sync() -> EnvReport {
    let os = std::env::consts::OS.to_string();

    // 体检顺手把 uv 目录并进本进程 PATH(幂等): 用户刚在本面板装完 uv 后, 前端会立即复检一次,
    // 这一步保证之后 spawn 的 claude 当轮就能 `uv run`, 无需重启 app。
    ensure_uv_on_process_path();

    let (claude, pwsh, node, npm, uv, python) = std::thread::scope(|s| {
        let claude = s.spawn(|| {
            detect(
                "claude",
                "Claude Code",
                "claude",
                &["--version"],
                &claude_candidates(),
                true,
                "未安装 —— 可一键安装 (官方脚本)",
            )
        });
        let pwsh = s.spawn(|| {
            detect(
                "pwsh",
                "PowerShell 7",
                "pwsh",
                &["--version"],
                &pwsh_candidates(),
                false,
                "未安装 —— 建议安装 (winget)",
            )
        });
        let node = s.spawn(|| {
            detect(
                "node",
                "Node.js",
                "node",
                &["--version"],
                &[],
                false,
                "未安装 (npm 安装方式需要它)",
            )
        });
        let npm = s.spawn(|| detect("npm", "npm", "npm", &["--version"], &[], false, "未安装"));
        // uv —— Python 脚本运行时的统一托管者(脚本执行公约依赖它)。候选含 ~/.local/bin/uv(.exe)。
        let uv = s.spawn(|| {
            detect(
                "uv",
                "uv",
                "uv",
                &["--version"],
                &uv_bin_dir()
                    .map(|d| vec![d.join(uv_exe_name())])
                    .unwrap_or_default(),
                false,
                "未安装 —— 一键安装后, Claude 写的 Python 脚本即可 `uv run` 跑(自动管解释器+依赖)",
            )
        });
        // 系统 Python —— 仅信息展示。detect 已滤掉 WindowsApps 的 0 字节占位符, 故「只有 Store 占位符」
        // 的机器这里 found=false, 如实反映「没有可用 Python」(脚本改由 uv 托管, 不依赖此项)。
        let python = s.spawn(|| {
            let mut p = detect(
                "python",
                "Python",
                "python",
                &["--version"],
                &[],
                false,
                "无可用系统 Python(脚本已由 uv 按需托管, 无需手动安装)",
            );
            // Windows 上 `python` 常只剩占位符; 退一步认 `python3`(detect 同样滤占位符)。
            if !p.found {
                let p3 = detect(
                    "python3",
                    "Python",
                    "python3",
                    &["--version"],
                    &[],
                    false,
                    "无可用系统 Python(脚本已由 uv 按需托管, 无需手动安装)",
                );
                if p3.found {
                    p = p3;
                    p.key = "python".to_string();
                }
            }
            p
        });
        (
            claude.join().expect("detect claude panicked"),
            pwsh.join().expect("detect pwsh panicked"),
            node.join().expect("detect node panicked"),
            npm.join().expect("detect npm panicked"),
            uv.join().expect("detect uv panicked"),
            python.join().expect("detect python panicked"),
        )
    });

    // PATH 体检: claude 安装目录是否在用户 PATH 里
    let claude_dir = claude_dir_for_fix(&claude);
    let claude_dir_on_user_path = match (&claude_dir, read_user_path()) {
        (Some(d), Some(up)) => path_contains_dir(&up, &d.to_string_lossy()),
        // 没装 / 拿不到用户 PATH → 当作「无需提示修复」(待安装后再判)
        _ => true,
    };

    // 可用 shell: Windows 需真身 pwsh (detect 已滤掉 Store 别名) 或 Git Bash;
    // 类 Unix(含 macOS) 自带 /bin/sh、zsh/bash, claude 直接可用 → 恒就绪。
    #[cfg(windows)]
    let shell_ready = pwsh.found || git_bash_path().is_some();
    #[cfg(not(windows))]
    let shell_ready = true;
    let ready = claude.found && shell_ready;

    EnvReport {
        os,
        claude,
        pwsh,
        node,
        npm,
        uv,
        python,
        claude_dir: claude_dir.as_deref().map(to_fwd),
        claude_dir_on_user_path,
        shell_ready,
        ready,
    }
}

/// 修复 PATH: 把 claude 所在目录写进用户 PATH + 当前进程 PATH。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn env_fix_path() -> Result<PathFixResult, String> {
    let report = env_check_sync();
    match report.claude_dir {
        Some(d) => Ok(ensure_dir_on_path(&d)),
        None => Ok(PathFixResult {
            ok: false,
            dir: None,
            status: "skipped".into(),
            message: "尚未找到 Claude Code 安装目录, 请先安装。".into(),
        }),
    }
}
