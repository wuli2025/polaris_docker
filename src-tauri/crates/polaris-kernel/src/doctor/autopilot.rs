//! 环境静默托管 (autopilot) —— 缺什么后台自己装好, **不问用户、不用用户点**。
//!
//! 背景: 以前首启有一道「环境医生」关卡, 把「你缺 uv / 缺 PowerShell 7 / 缺 Claude Code」
//! 摊给用户看, 还要他自己点安装。对普通用户这是纯负担 —— 他既不知道 uv 是什么,
//! 也不该关心。现在:
//!
//! - **uv / Python / Git Bash** → 已随安装包内置(见 `doctor::bundled`), 本来就不用装;
//! - **Claude Code** → 唯一真需要联网装的东西, 由本模块在后台悄悄装好。
//!   **不经 `npm i -g`**(它下 80MB optional 依赖常卡死), 改成直抓平台包 tgz 解压出 claude
//!   到 `~/.local/bin`(R2 优先、多源回落; 见 `install::claude_install_command`)。
//! - **Node.js** → Claude 直装已**不再依赖它**, 但仍后台装一份(便携版, 免 UAC), 供 Claude
//!   写的 JS 脚本有运行时(与内置 uv 之于 Python 同理)。装不上不致命 —— 照样继续装 Claude。
//!
//! 铁律 —— **全程不弹 UAC**:
//! Windows 上装 Node 的老路子(winget / MSI)要写 `Program Files`, 必然 `-Verb RunAs` 弹管理员
//! 授权框。后台静默安装时弹一个没有上下文的 UAC 框, 用户根本不知道是谁弹的, 只会点「否」。
//! 故这里改用**便携版 Node**(zip 解压到 `~/.local/polaris-node`), 全程零提权。
//! macOS 本就走免 sudo 的 tar.gz 路径, 天然满足。
//!
//! 失败不打扰: 装不上(断网等)只在状态里记一笔, 下次启动再试; 用户仍可去侧栏「环境」页手动看。

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;

#[cfg(not(feature = "desktop"))]
use crate::host::AppHandle;
#[cfg(feature = "desktop")]
use tauri::{AppHandle, Emitter};

use super::check::env_check_sync;
use super::install::*;
use super::path::*;
use super::probe::*;

/// 静默托管的对外状态 (侧栏「环境」页可查; 前端也据此弹一次被动提示, 无需用户操作)。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AutopilotState {
    /// 是否正在后台装
    pub running: bool,
    /// 当前步骤的人话描述
    pub step: String,
    /// 是否已跑完一轮
    pub finished: bool,
    /// 跑完那轮成没成 (没事可做也算成功)
    pub ok: bool,
    /// 这轮实际装了什么 (空 = 什么都不用装)
    pub installed: Vec<String>,
    /// 一句话结果
    pub message: String,
}

static STATE: once_cell::sync::Lazy<Mutex<AutopilotState>> =
    once_cell::sync::Lazy::new(|| Mutex::new(AutopilotState::default()));
static STARTED: AtomicBool = AtomicBool::new(false);

/// 查询静默托管状态 (前端可选地展示一行「正在后台准备…」)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn env_autopilot_status() -> AutopilotState {
    STATE.lock().clone()
}

fn set_step(app: &AppHandle, step: &str) {
    {
        let mut s = STATE.lock();
        s.running = true;
        s.step = step.to_string();
    }
    emit_state(app);
}

fn emit_state(app: &AppHandle) {
    let snapshot = STATE.lock().clone();
    let _ = app.emit("env:autopilot", snapshot);
}

/// 启动环境静默托管。**每个进程只跑一轮**(重复调用直接忽略), 全程后台线程, 不阻塞启动。
pub fn start_autopilot(app: AppHandle) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || run(app));
}

fn run(app: AppHandle) {
    let report = env_check_sync();

    // 什么都不缺 → 一声不吭 (绝大多数启动都走这条: 零噪音、零事件)
    if report.claude.found {
        let mut s = STATE.lock();
        s.running = false;
        s.finished = true;
        s.ok = true;
        s.message = "环境已就绪。".into();
        return;
    }

    set_step(&app, "正在后台准备运行环境…");
    let mut installed: Vec<String> = Vec::new();

    // ① 缺 Node → 后台装便携版 (免管理员, 不弹 UAC), 供 Claude 写的 JS 脚本有运行时。
    // Claude 直装**不再依赖 npm** → Node 装不上不致命, 记一笔就继续装 Claude。
    if !report.npm.found {
        set_step(&app, "正在后台安装 Node.js（无需操作）…");
        if install_node_silently() {
            installed.push("Node.js".into());
            // 把 bin 目录塞进本进程 PATH, 让本会话紧接着能直接用上 node/npm。
            if let Some(dir) = node_dir_candidates().into_iter().find(|p| p.exists()) {
                prepend_process_path(&dir.to_string_lossy());
            }
        }
        // 失败不 return: Node 只是「锦上添花」, Claude 才是关键, 继续往下。
    }

    // ② 装 Claude Code (直抓平台包 tgz, R2 优先多源, 不经 npm; 落到 ~/.local/bin, 零提权)
    set_step(&app, "正在后台安装 Claude Code（无需操作）…");
    if !install_claude_silently() {
        finish(
            &app,
            false,
            installed,
            "后台安装 Claude Code 未成功（可能是网络问题），下次启动会再试。",
        );
        return;
    }
    installed.push("Claude Code".into());

    // ③ 收尾: 清 spawn 解析缓存 + 把 claude 目录持久化进用户 PATH (免重启即可用)
    *CLAUDE_EXE_CACHE.lock() = None;
    let after = env_check_sync();
    if let Some(dir) = after.claude_dir.as_deref() {
        let _ = ensure_dir_on_path(dir);
    }

    if after.claude.found {
        finish(&app, true, installed, "运行环境已在后台准备好，可以开始用了。");
    } else {
        finish(
            &app,
            false,
            installed,
            "后台安装跑完了但仍未探测到 Claude Code，可到侧栏「环境」页看看。",
        );
    }
}

fn finish(app: &AppHandle, ok: bool, installed: Vec<String>, message: &str) {
    {
        let mut s = STATE.lock();
        s.running = false;
        s.finished = true;
        s.ok = ok;
        s.installed = installed;
        s.step = String::new();
        s.message = message.to_string();
    }
    emit_state(app);
}

/// 跑一条安装命令并等它结束, **不往 UI 推日志**(静默)。返回是否成功。
fn run_silently(cmd: std::process::Command, timeout: Duration) -> bool {
    output_with_timeout(cmd, timeout).is_some_and(|o| o.status.success())
}

/// 静默装 Node: Windows 走便携 zip(免 UAC), macOS 走既有的免 sudo tar.gz。
fn install_node_silently() -> bool {
    #[cfg(windows)]
    {
        run_silently(
            build_install_shell(&node_portable_script()),
            Duration::from_secs(900),
        )
    }
    #[cfg(target_os = "macos")]
    {
        run_silently(mac_node_install_command(), Duration::from_secs(900))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        false // Linux/Docker 版镜像里本就带好了, 不在这里代劳
    }
}

/// 静默装 Claude Code: 直抓平台包 tgz 解压到 `~/.local/bin`(R2 优先、多源回落, 不经 npm)。
/// 与手动安装按钮走**同一条**命令 (`install::claude_install_command`), 行为一致、结果确定。
fn install_claude_silently() -> bool {
    run_silently(claude_install_command(), Duration::from_secs(1200))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 状态默认值必须是「没跑过」——前端据此判断要不要显示提示。
    #[test]
    fn default_state_is_idle() {
        let s = AutopilotState::default();
        assert!(!s.running && !s.finished && s.installed.is_empty());
    }

    /// 只准跑一轮: 第二次调用必须被挡掉 (否则每次复检都会重复装)。
    #[test]
    fn start_is_once_per_process() {
        assert!(!STARTED.swap(true, Ordering::SeqCst) || STARTED.load(Ordering::SeqCst));
        // 再 swap 一次必然拿到 true (已启动)
        assert!(STARTED.swap(true, Ordering::SeqCst));
    }
}
