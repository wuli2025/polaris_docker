//! Claude Code 更新检测 / 更新 (纯移动)。

use std::process::{Command, Stdio};
use std::time::Duration;

#[cfg(not(feature = "desktop"))]
use crate::host::AppHandle;
use crate::runtime::procs::no_window;
#[cfg(feature = "desktop")]
use tauri::AppHandle;

use super::install::*;
use super::probe::*;
use super::types::*;

/// 把 "1.0.44 (Claude Code)" 这类串里第一个形如 a.b.c 的版本号解析成元组。
fn parse_triplet(tok: &str) -> Option<(u64, u64, u64)> {
    let mut it = tok.split('.');
    let a = it.next()?.parse::<u64>().ok()?;
    let b = it.next()?.parse::<u64>().ok()?;
    let c = it.next()?.parse::<u64>().ok()?;
    Some((a, b, c))
}

fn extract_semver(s: &str) -> Option<(u64, u64, u64)> {
    for tok in s.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        if tok.is_empty() {
            continue;
        }
        if let Some(t) = parse_triplet(tok) {
            return Some(t);
        }
    }
    None
}

/// npm 镜像上 Claude Code 的最新版本号 (`npm view ... version`, 走 npmmirror)。
fn npm_view_latest() -> Option<String> {
    let pkg = "@anthropic-ai/claude-code";
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.args([
            "/c",
            "npm",
            "view",
            pkg,
            "version",
            "--registry=https://registry.npmmirror.com",
        ]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = Command::new("npm");
        c.args([
            "view",
            pkg,
            "version",
            "--registry=https://registry.npmmirror.com",
        ]);
        c
    };
    cmd.stdin(Stdio::null());
    no_window(&mut cmd);
    let out = output_with_timeout(cmd, Duration::from_secs(20))?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|s| s.to_string())
}

/// 没有 npm 时的兜底: 直接打 npmmirror 的 registry HTTP 接口取 dist-tags.latest。
#[cfg(windows)]
fn registry_latest_via_http() -> Option<String> {
    let script = "(Invoke-RestMethod -UseBasicParsing \
'https://registry.npmmirror.com/@anthropic-ai/claude-code').'dist-tags'.latest";
    let mut cmd = Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    cmd.stdin(Stdio::null());
    no_window(&mut cmd);
    let out = output_with_timeout(cmd, Duration::from_secs(20))?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!v.is_empty()).then_some(v)
}

#[cfg(not(windows))]
fn registry_latest_via_http() -> Option<String> {
    None
}

/// 检测 Claude Code 是否有新版本: 当前版本 (`claude --version`) vs 镜像 latest。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn env_claude_update_check() -> ClaudeUpdateInfo {
    let current_raw = probe_version("claude", &["--version"]);
    let installed = current_raw.is_some() || resolve_claude_exe().is_some();
    if !installed {
        return ClaudeUpdateInfo {
            installed: false,
            current: None,
            latest: None,
            update_available: false,
            checked: false,
            message: "未检测到 Claude Code, 请先安装。".into(),
        };
    }

    // 当前版本: 优先展示解析出的纯 semver, 否则原样
    let cur_semver = current_raw.as_deref().and_then(extract_semver);
    let current = cur_semver
        .map(|(a, b, c)| format!("{a}.{b}.{c}"))
        .or_else(|| current_raw.clone());

    let latest = npm_view_latest().or_else(registry_latest_via_http);
    match latest {
        Some(l) => {
            let lv = extract_semver(&l);
            let update_available = match (cur_semver, lv) {
                (Some(c), Some(n)) => n > c,
                _ => false,
            };
            let message = if update_available {
                format!(
                    "发现新版本 {l} (当前 {})。",
                    current.clone().unwrap_or_default()
                )
            } else {
                format!("已是最新版本 ({})。", current.clone().unwrap_or_default())
            };
            ClaudeUpdateInfo {
                installed: true,
                current,
                latest: Some(l),
                update_available,
                checked: true,
                message,
            }
        }
        None => ClaudeUpdateInfo {
            installed: true,
            current,
            latest: None,
            update_available: false,
            checked: false,
            message: "无法获取最新版本号 (可检查网络 / npm 后重试)。".into(),
        },
    }
}

/// 更新 Claude Code 到最新版 —— 走国内 npmmirror, 与默认安装方式同源, 国内最快。
/// 复用流式安装管线; 成功后清解析缓存并自动修 PATH (与首次安装一致)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn env_update_claude(app: AppHandle) -> Result<String, String> {
    let inner = "npm install -g @anthropic-ai/claude-code@latest \
--registry=https://registry.npmmirror.com";
    let req_id = next_req_id();
    let cmd = build_install_shell(inner);
    stream_install(app, req_id.clone(), cmd, true, "Claude Code 更新");
    Ok(req_id)
}
