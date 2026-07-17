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

const PKG_URL: &str = "https://registry.npmmirror.com/@anthropic-ai/claude-code/latest";

/// npm 镜像上 Claude Code 的最新版本号 (`npm view ... version`, 走 npmmirror)。
/// 用**解析出的 npm 绝对路径**跑 —— 裸名走 PATH 时, 用户改过 npm 前缀 / 刚装完 Node 本进程 PATH
/// 还没刷新, 都会让这里直接哑掉 → 面板报「无法获取最新版本号」。
fn npm_view_latest() -> Option<String> {
    let npm = resolve_npm_exe()?;
    let out = output_with_timeout(
        command_at(
            &npm,
            &[
                "view",
                "@anthropic-ai/claude-code",
                "version",
                "--registry=https://registry.npmmirror.com",
            ],
        ),
        Duration::from_secs(20),
    )?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|s| s.to_string())
}

/// 从 registry 的 `/latest` 清单 JSON 里取 `version`。取 `/latest` 而非整个 packument ——
/// 后者含全部历史版本、有好几 MB, 只为读一个版本号不值当。
fn version_from_manifest(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let s = v.get("version")?.as_str()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// 没有 npm 时的兜底: 直接打 npmmirror 的 registry HTTP 接口取版本号。
/// Windows 走 PowerShell(系统自带), 类 Unix 走 curl(mac/Linux 自带) —— 老版本这里在非 Windows
/// 上直接返回 None, 于是 mac 上一旦 npm 不在 PATH 就彻底查不到最新版。
fn registry_latest_via_http() -> Option<String> {
    #[cfg(windows)]
    let mut cmd = {
        let script = format!("(Invoke-WebRequest -UseBasicParsing '{PKG_URL}').Content");
        let mut c = Command::new("powershell");
        c.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = Command::new("curl");
        c.args(["-fsSL", PKG_URL]);
        c
    };
    cmd.stdin(Stdio::null());
    no_window(&mut cmd);
    let out = output_with_timeout(cmd, Duration::from_secs(20))?;
    if !out.status.success() {
        return None;
    }
    version_from_manifest(&String::from_utf8_lossy(&out.stdout))
}

/// 检测 Claude Code 是否有新版本: 当前版本 (`claude --version`) vs 镜像 latest。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn env_claude_update_check() -> ClaudeUpdateInfo {
    // 按解析出的**绝对路径**探当前版本, 裸名仅兜底。老代码只用裸名, 而「claude 装了但不在 PATH」
    // 是面板显式支持的常态 (有「已安装 (不在 PATH)」状态 +「修复 PATH」按钮) —— 那种机器上
    // current 恒为 None, 下面的 (cur, latest) 匹配就恒落到 `_ => false`, **有新版也永远显示
    // 「已是最新」**, 且横幅还是「已是最新版本 ()」这种版本号为空的怪话。
    let exe = resolve_claude_exe();
    let current_raw = exe
        .as_deref()
        .and_then(|p| probe_version_at(p, &["--version"]))
        .or_else(|| probe_version("claude", &["--version"]));
    let installed = current_raw.is_some() || exe.is_some();
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
            let (update_available, message) = match (cur_semver, lv) {
                (Some(c), Some(n)) if n > c => (
                    true,
                    format!("发现新版本 {l} (当前 {})。", current.clone().unwrap_or_default()),
                ),
                (Some(_), Some(_)) => (
                    false,
                    format!("已是最新版本 ({})。", current.clone().unwrap_or_default()),
                ),
                // 查到了 latest 但读不出当前版本 (claude 起不来 / 输出格式变了)。
                // 不能像老代码那样默认「已是最新」—— 那是在把「不知道」谎报成「没更新」。
                // 如实说明并把更新按钮亮出来: 装一次最新版无害, 顺带把这台机器修回可探测状态。
                (None, Some(_)) => (
                    true,
                    format!("读不出当前版本 (claude 可能不在 PATH), 可直接更新到 {l}。"),
                ),
                // latest 解析不出 semver → 无从比较, 不提示更新
                (_, None) => (false, format!("镜像返回的版本号无法解析: {l}。")),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_from_manifest_reads_latest_manifest() {
        let body = r#"{"name":"@anthropic-ai/claude-code","version":"2.1.212","bin":{}}"#;
        assert_eq!(version_from_manifest(body), Some("2.1.212".into()));
        assert_eq!(version_from_manifest("not json"), None);
        assert_eq!(version_from_manifest(r#"{"name":"x"}"#), None); // 无 version 字段
        assert_eq!(version_from_manifest(r#"{"version":""}"#), None); // 空版本不当数
    }

    #[test]
    fn extract_semver_picks_first_triplet() {
        assert_eq!(extract_semver("2.1.212 (Claude Code)"), Some((2, 1, 212)));
        assert_eq!(extract_semver("v20.18.1"), Some((20, 18, 1)));
        assert_eq!(extract_semver("no version here"), None);
        // 比较是元组序: 212 > 99 (不是字符串序)
        assert!(extract_semver("2.1.212") > extract_semver("2.1.99"));
    }
}

/// claude 这一份是不是 npm 装的 —— npm 装的必然落在 npm 全局前缀里 (前缀根的 `claude.cmd` shim,
/// 或 `<前缀>/node_modules/@anthropic-ai/claude-code/bin/claude.exe` 那个原生二进制)。
/// 其余当作官方原生脚本装 (`~/.local/bin/claude.exe`)。
fn claude_is_npm_install(exe: &std::path::Path) -> bool {
    if exe.components().any(|c| c.as_os_str() == "node_modules") {
        return true;
    }
    match (exe.parent(), npm_global_prefix()) {
        (Some(dir), Some(prefix)) => dir == prefix,
        _ => false,
    }
}

/// 更新 Claude Code 到最新版。**更新方式必须跟安装方式一致**:
/// - **npm 装的** → `npm i -g @latest` 走国内 npmmirror (与默认安装同源, 国内最快);
/// - **官方原生脚本装的** (claude 在 `~/.local/bin/claude.exe`) → 走它自带的 `claude update`,
///   **就地**替换那个 exe。
///
/// 老代码不管怎么装的一律跑 `npm i -g @latest`: 在原生装的机器上, npm 只会往全局前缀里**另装
/// 一份**, 而 PATH 命中 / `resolve_claude_exe` 解析到的仍是原来那个 exe → 「更新完成」横幅照弹,
/// 复检版本却纹丝不动, 按钮永远停在「更新到 x.y.z」, 点多少次都一样。
///
/// 复用流式安装管线; 成功后清解析缓存并自动修 PATH (与首次安装一致)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn env_update_claude(app: AppHandle) -> Result<String, String> {
    let req_id = next_req_id();
    let native = resolve_claude_exe().filter(|p| !claude_is_npm_install(p));
    let cmd = match &native {
        Some(exe) => {
            let mut c = command_at(exe, &["update"]);
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
            c
        }
        None => build_install_shell(
            "npm install -g @anthropic-ai/claude-code@latest \
--registry=https://registry.npmmirror.com",
        ),
    };
    stream_install(app, req_id.clone(), cmd, true, "Claude Code 更新");
    Ok(req_id)
}
