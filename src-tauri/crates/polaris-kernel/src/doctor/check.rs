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
        // PowerShell 7 —— **纯可选**。Claude 在 Windows 上要的只是「一个可用的 shell」,
        // 而应用已内置 Git Bash(见 doctor::bundled), 故这里不再催装、更不会自动替用户装。
        let pwsh = s.spawn(|| {
            detect(
                "pwsh",
                "PowerShell 7",
                "pwsh",
                &["--version"],
                &pwsh_candidates(),
                false,
                "未安装 —— 可选 (应用已内置 Git Bash 供 Claude 使用, 无需安装)",
            )
        });
        // node/npm 必须带上安装目录候选: 只靠 PATH 的话, 「装好了但本进程 PATH 是旧快照」
        // (Windows MSI/winget 只写注册表; GUI 从 Finder 拉起只有极简 PATH) 会被误报成「未安装」。
        let node = s.spawn(|| {
            detect(
                "node",
                "Node.js",
                "node",
                &["--version"],
                &node_candidates(),
                false,
                "未安装 (npm 安装方式需要它)",
            )
        });
        let npm = s.spawn(|| {
            detect(
                "npm",
                "npm",
                "npm",
                &["--version"],
                &npm_candidates(),
                false,
                "未安装",
            )
        });
        // uv —— **纯可选的加速器**, 不是必需品。跑 Python 脚本由内置解释器打头阵
        // (见 chat::prompt::script_convention); uv 只是把「建隔离环境 + 装依赖」做得更省事,
        // 没有它照样干活, 故这里不催装。
        let uv = s.spawn(|| {
            detect(
                "uv",
                "uv",
                "uv",
                &["--version"],
                &uv_candidates(),
                false,
                "未安装 —— 可选加速器(不装也不影响: Python 脚本走内置解释器 + pip --user)",
            )
        });
        // Python —— PATH 上的系统 Python 优先, 否则用随包内置的便携解释器(自带 pip、标准库齐全)。
        // detect 已滤掉 WindowsApps 的 0 字节占位符, 故「只有 Store 占位符」的机器不会误判成已装。
        let python = s.spawn(|| {
            let cands = python_candidates();
            let hint = "未找到 Python —— 正常情况下应用已内置一份, 可到侧栏「环境」页复检";
            let mut p = detect("python", "Python", "python", &["--version"], &cands, false, hint);
            // Windows 上 `python` 常只剩占位符; 退一步认 `python3`(detect 同样滤占位符)。
            if !p.found {
                let p3 = detect("python3", "Python", "python3", &["--version"], &cands, false, hint);
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

    // 可用 shell: Windows 需真身 pwsh (detect 已滤掉 Store 别名) 或 Git Bash —— 后者现在
    // **随安装包内置**, 故干净 Windows 也开箱即就绪, 不必再自动替用户装 PowerShell 7。
    // 类 Unix(含 macOS) 自带 /bin/sh、zsh/bash, claude 直接可用 → 恒就绪。
    #[cfg(windows)]
    let (shell_ready, shell_bundled) = {
        let bash = git_bash_path();
        let bundled = bash.as_deref().is_some_and(super::bundled::is_bundled);
        (pwsh.found || bash.is_some(), bundled && !pwsh.found)
    };
    #[cfg(not(windows))]
    let (shell_ready, shell_bundled) = (true, false);
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
        shell_bundled,
        ready,
        // server/容器 flavor: 组件随镜像预烤, 装/更新在 apihub 层就被挡 → 面板别给按钮。
        image_managed: !cfg!(feature = "desktop"),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `image_managed` 必须随 flavor 翻转 —— 容器/server 版据此**不渲染**安装/更新按钮
    /// (那些在 apihub 层被挡, 给按钮就是给一个点了必报错的入口)。
    /// 谁要是改了 feature 划分或漏填这个字段, 这里当场炸, 而不是等用户在容器里点出错误弹窗。
    #[test]
    fn image_managed_tracks_flavor() {
        let r = env_check_sync();
        assert_eq!(
            r.image_managed,
            !cfg!(feature = "desktop"),
            "image_managed 应 = 非 desktop flavor(server/容器版为 true)"
        );
    }
}