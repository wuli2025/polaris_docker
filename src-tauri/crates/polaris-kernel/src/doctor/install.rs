//! 流式安装管线 + 安装命令 (Claude/Node/PowerShell/uv) + 取消 (纯移动)。

#[cfg(not(feature = "desktop"))]
use crate::host::AppHandle;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(feature = "desktop")]
use tauri::{AppHandle, Emitter};

use super::check::*;
use super::path::*;
use super::probe::*;
use super::types::*;
use crate::runtime::procs::{no_window, CHILDREN};

static REQ_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn next_req_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let c = REQ_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("env-{:x}-{:x}", ts, c)
}

/// 安装 Claude Code。method: "npm" (默认, 经国内镜像) | "native" (官方原生脚本, 兜底)。
/// 流式把安装日志通过 `env:stream` 事件推给前端; 成功后自动修 PATH。
/// 跨平台: Windows 经 PowerShell, macOS/Linux 经 `sh`(npm 方式两端一致; native 各走各的官方脚本)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn env_install_claude(app: AppHandle, method: Option<String>) -> Result<String, String> {
    let method = method.unwrap_or_else(|| "npm".to_string());
    let inner = claude_install_cmd(&method);
    let req_id = next_req_id();
    let cmd = build_install_shell(&inner);
    stream_install(app, req_id.clone(), cmd, true, "Claude Code");
    Ok(req_id)
}

/// 安装 Node.js LTS —— npm 安装方式的前置依赖。两端都走「国内镜像优先」, 装完把 bin 目录
/// 塞进进程 PATH (stream_install 收尾处), 故 `fix_path_after=false`。
///
/// - **Windows**: 两层策略 —— ① 有 winget 先用 winget; ② 缺失/失败 → 下载官方 MSI (npmmirror
///   镜像加速) 静默安装 (Win10 常无 winget, 故必须有 MSI 兜底)。
/// - **macOS**: 下载 Node 官方 darwin tar.gz (走 npmmirror 二进制镜像, 国内可达) **免 sudo**
///   解压到 `~/.local/polaris-node`, 并把其 `bin` 写进 shell 配置 —— 不动系统目录、不弹 UAC/密码。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn env_install_node(app: AppHandle) -> Result<String, String> {
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = &app;
        return Err(
            "Node.js 自动安装目前支持 Windows 与 macOS; Linux 请用系统包管理器安装。".into(),
        );
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        let req_id = next_req_id();
        #[cfg(windows)]
        let cmd = build_powershell(NODE_INSTALL_SCRIPT);
        #[cfg(target_os = "macos")]
        let cmd = build_install_shell(MAC_NODE_INSTALL_SCRIPT);
        stream_install(app, req_id.clone(), cmd, false, "Node.js");
        Ok(req_id)
    }
}

/// macOS Node.js 安装脚本 (POSIX sh) —— 免 sudo: 下载官方 darwin tar.gz 解压到
/// `~/.local/polaris-node`, 把 `bin` 写进 zsh/bash 配置。下载走 npmmirror 二进制镜像 (国内可达),
/// 不行再退 nodejs.org 直连。选 20.x LTS, 与 Windows 一致。
#[cfg(target_os = "macos")]
const MAC_NODE_INSTALL_SCRIPT: &str = r#"
VER=20.18.1
ARCH=$(uname -m)
case "$ARCH" in
  arm64|aarch64) NARCH=arm64 ;;
  x86_64) NARCH=x64 ;;
  *) NARCH=x64 ;;
esac
PKG="node-v${VER}-darwin-${NARCH}.tar.gz"
DEST="$HOME/.local"
NODE_DIR="$DEST/polaris-node"
TMP="$(mktemp -d)"
TARBALL="$TMP/$PKG"
echo "目标架构: $NARCH; Node 版本: v$VER"
OK=0
for U in \
  "https://cdn.npmmirror.com/binaries/node/v${VER}/${PKG}" \
  "https://npmmirror.com/mirrors/node/v${VER}/${PKG}" \
  "https://nodejs.org/dist/v${VER}/${PKG}" ; do
  echo "下载: $U"
  if curl -fsSL "$U" -o "$TARBALL" && [ -s "$TARBALL" ]; then OK=1; break; fi
  echo "  下载失败, 试下一个镜像..."
done
if [ "$OK" != "1" ]; then echo "Node.js 下载失败 (检查网络/代理后重试)。"; rm -rf "$TMP"; exit 1; fi
mkdir -p "$DEST"
rm -rf "$NODE_DIR"
mkdir -p "$NODE_DIR"
tar -xzf "$TARBALL" -C "$NODE_DIR" --strip-components=1 || { echo "解压失败。"; rm -rf "$TMP"; exit 1; }
rm -rf "$TMP"
BIN="$NODE_DIR/bin"
if [ ! -x "$BIN/node" ]; then echo "解压后未找到 node 可执行文件。"; exit 1; fi
# 把 node bin 写进 shell 配置 (zsh 为 macOS 默认; 同时照顾 bash/sh), 已存在则不重复
LINE="export PATH=\"$BIN:\$PATH\""
for RC in "$HOME/.zshrc" "$HOME/.zprofile" "$HOME/.bash_profile" "$HOME/.profile" ; do
  touch "$RC" 2>/dev/null || true
  grep -qF "$BIN" "$RC" 2>/dev/null || printf '\n# Added by Polaris (Node.js)\n%s\n' "$LINE" >> "$RC"
done
export PATH="$BIN:$PATH"
echo "Node.js 安装完成: node $("$BIN/node" -v), npm $("$BIN/npm" -v)"
echo "已写入 ~/.zshrc 等; 本次会话已即时生效。"
"#;

/// Node.js LTS 安装脚本: winget 优先, 失败则下载官方 MSI (国内 npmmirror 镜像加速) 静默安装。
/// 选 20.x LTS ("Iron"): 长期支持、兼容 Windows 10。
const NODE_INSTALL_SCRIPT: &str = r#"
$ErrorActionPreference = 'Continue'
# ① 优先 winget (能拿最新 LTS, 自带配 PATH)
$wg = Get-Command winget -ErrorAction SilentlyContinue
if ($wg) {
  Write-Output '检测到 winget, 优先用它安装 Node.js LTS...'
  & winget install --id OpenJS.NodeJS.LTS -e --source winget --accept-package-agreements --accept-source-agreements
  if ($LASTEXITCODE -eq 0) { Write-Output 'Node.js (winget) 安装完成。'; exit 0 }
  Write-Output ('winget 安装未成功 (退出码 ' + $LASTEXITCODE + '), 改用直接下载 MSI...')
} else {
  Write-Output '未检测到 winget (Windows 10 常见), 改用直接下载官方 MSI...'
}
# ② 下载官方 Node LTS MSI -> %TEMP% -> msiexec 静默安装。下载路径走国内 npmmirror 镜像兜底。
$ver = '20.18.1'
$arch = switch ($env:PROCESSOR_ARCHITECTURE) { 'ARM64' { 'arm64' } 'AMD64' { 'x64' } default { 'x86' } }
$msi = "node-v$ver-$arch.msi"
$dst = Join-Path $env:TEMP $msi
$urls = @(
  "https://cdn.npmmirror.com/binaries/node/v$ver/$msi",
  "https://npmmirror.com/mirrors/node/v$ver/$msi",
  "https://nodejs.org/dist/v$ver/$msi"
)
$ok = $false
foreach ($u in $urls) {
  try {
    Write-Output "下载: $u"
    Invoke-WebRequest -Uri $u -OutFile $dst -UseBasicParsing -TimeoutSec 600
    if ((Test-Path $dst) -and ((Get-Item $dst).Length -gt 1MB)) { $ok = $true; break }
  } catch {
    Write-Output ("  下载失败: " + $_.Exception.Message)
  }
}
if (-not $ok) {
  Write-Output 'Node.js 安装包下载失败 (可检查网络 / 代理后重试)。'
  exit 1
}
# 安装到 Program Files 需要管理员权限 -> 用 RunAs 触发 UAC (拒绝则友好报错, 不静默失败)
Write-Output "安装中 (msiexec, 会弹一次 UAC 授权): $dst"
try {
  $p = Start-Process msiexec.exe -ArgumentList ('/i "' + $dst + '" /quiet /norestart') -Wait -PassThru -Verb RunAs
} catch {
  Write-Output ('安装启动失败 (可能未授予管理员权限): ' + $_.Exception.Message)
  exit 1
}
Remove-Item $dst -ErrorAction SilentlyContinue
if ($p.ExitCode -ne 0) { Write-Output ('msiexec 退出码 ' + $p.ExitCode); exit 1 }
Write-Output 'Node.js 安装完成。'
"#;

/// 安装 PowerShell 7。成功无需改 PATH (MSI / winget 安装都会自带配 PATH)。
///
/// 之前只用 `winget`, 但很多机器上要么没有 winget、要么 winget 源在国内拉不动
/// → 用户报「PowerShell 7 下载不了」。这里改成**两层策略**:
/// ① 有 winget 先用 winget (官方、能拿最新版);
/// ② winget 缺失 / 失败 → **直接下载官方 MSI 再 msiexec 静默安装**, 且下载走
///    国内可达的 GitHub 文件代理 (gh-proxy / ghfast) 兜底, 实在不行再走 GitHub 直连。
///    这就是「下载路径」修复 —— 明确把 MSI 落到 `%TEMP%` 再装, 不再黑盒依赖 winget。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn env_install_pwsh(app: AppHandle) -> Result<String, String> {
    if !cfg!(windows) {
        return Err("PowerShell 7 自动安装仅支持 Windows。".into());
    }
    let req_id = next_req_id();
    let cmd = build_powershell(PWSH_INSTALL_SCRIPT);
    stream_install(app, req_id.clone(), cmd, false, "PowerShell 7");
    Ok(req_id)
}

/// PowerShell 7 安装脚本: winget 优先, 失败则下载官方 MSI (国内代理加速) 静默安装。
/// 版本仅用于 MSI 兜底直链 (winget 路径自动取最新); 选 7.4.x LTS, 稳定且长期可用。
const PWSH_INSTALL_SCRIPT: &str = r#"
$ErrorActionPreference = 'Continue'
# ① 优先 winget (能拿最新版, 自带配 PATH)
$wg = Get-Command winget -ErrorAction SilentlyContinue
if ($wg) {
  Write-Output '检测到 winget, 优先用它安装 PowerShell 7...'
  & winget install --id Microsoft.PowerShell -e --source winget --accept-package-agreements --accept-source-agreements
  if ($LASTEXITCODE -eq 0) { Write-Output 'PowerShell 7 (winget) 安装完成。'; exit 0 }
  Write-Output ('winget 安装未成功 (退出码 ' + $LASTEXITCODE + '), 改用直接下载 MSI...')
} else {
  Write-Output '未检测到 winget, 改用直接下载官方 MSI...'
}
# ② 下载官方 MSI -> %TEMP% -> msiexec 静默安装。下载路径走国内可达的 GitHub 代理兜底。
$ver = '7.4.6'
$arch = switch ($env:PROCESSOR_ARCHITECTURE) { 'ARM64' { 'arm64' } 'AMD64' { 'x64' } default { 'x86' } }
$msi = "PowerShell-$ver-win-$arch.msi"
$dst = Join-Path $env:TEMP $msi
$rel = "https://github.com/PowerShell/PowerShell/releases/download/v$ver/$msi"
$urls = @(
  "https://gh-proxy.com/$rel",
  "https://ghfast.top/$rel",
  "https://ghproxy.net/$rel",
  $rel
)
$ok = $false
foreach ($u in $urls) {
  try {
    Write-Output "下载: $u"
    Invoke-WebRequest -Uri $u -OutFile $dst -UseBasicParsing -TimeoutSec 600
    if ((Test-Path $dst) -and ((Get-Item $dst).Length -gt 1MB)) { $ok = $true; break }
  } catch {
    Write-Output ("  下载失败: " + $_.Exception.Message)
  }
}
if (-not $ok) {
  Write-Output 'PowerShell 7 安装包下载失败 (可检查网络 / 代理后重试)。'
  exit 1
}
# 安装到 Program Files 需要管理员权限 -> 用 RunAs 触发 UAC (拒绝则友好报错, 不静默失败)
Write-Output "安装中 (msiexec, 会弹一次 UAC 授权): $dst"
try {
  $p = Start-Process msiexec.exe -ArgumentList ('/i "' + $dst + '" /quiet /norestart ADD_PATH=1') -Wait -PassThru -Verb RunAs
} catch {
  Write-Output ('安装启动失败 (可能未授予管理员权限): ' + $_.Exception.Message)
  exit 1
}
Remove-Item $dst -ErrorAction SilentlyContinue
if ($p.ExitCode -ne 0) { Write-Output ('msiexec 退出码 ' + $p.ExitCode); exit 1 }
Write-Output 'PowerShell 7 安装完成。'
"#;

// ───────────────────────── uv (Python 运行时托管) 安装 ─────────────────────────

/// 安装 uv —— Python 脚本运行时的统一托管者。装到用户目录 `~/.local/bin`(免管理员/sudo),
/// 顺带写一份「国内镜像」uv 配置(仅当用户尚无配置时), 让之后 `uv python install` / 依赖解析走
/// 清华 PyPI + gh-proxy 的 python-build-standalone, 国内可达。装完无需改注册表: `env_check` 与
/// 启动期 `prime_path_for_claude` 都会把 `~/.local/bin` 并进进程 PATH, claude 当轮即可 `uv run`。
///
/// 下载走 GitHub Release 直链 + 国内文件代理(gh-proxy / ghfast)兜底(与 PowerShell 7 同策略),
/// 解压即用(uv 是单文件二进制, 无安装器、无 UAC)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn env_install_uv(app: AppHandle) -> Result<String, String> {
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = &app;
        return Err(
            "uv 自动安装目前支持 Windows 与 macOS; Linux 请用 `curl -LsSf https://astral.sh/uv/install.sh | sh`。"
                .into(),
        );
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        let req_id = next_req_id();
        #[cfg(windows)]
        let cmd = build_powershell(UV_INSTALL_SCRIPT);
        #[cfg(target_os = "macos")]
        let cmd = build_install_shell(MAC_UV_INSTALL_SCRIPT);
        stream_install(app, req_id.clone(), cmd, false, "uv");
        Ok(req_id)
    }
}

/// Windows uv 安装脚本: 下载官方 release zip(国内代理加速)→ 解压到 `~/.local/bin` → 写国内镜像配置。
/// uv 是 MIT/Apache 双许可的单文件二进制, 解压即用, 不需要管理员权限。
#[cfg(windows)]
const UV_INSTALL_SCRIPT: &str = r#"
$ErrorActionPreference = 'Continue'
$arch = switch ($env:PROCESSOR_ARCHITECTURE) { 'ARM64' { 'aarch64' } 'AMD64' { 'x86_64' } default { 'x86_64' } }
$asset = "uv-$arch-pc-windows-msvc.zip"
$rel = "https://github.com/astral-sh/uv/releases/latest/download/$asset"
$urls = @(
  "https://gh-proxy.com/$rel",
  "https://ghfast.top/$rel",
  "https://ghproxy.net/$rel",
  $rel
)
$dst = Join-Path $env:TEMP $asset
$ok = $false
foreach ($u in $urls) {
  try {
    Write-Output "下载: $u"
    Invoke-WebRequest -Uri $u -OutFile $dst -UseBasicParsing -TimeoutSec 600
    if ((Test-Path $dst) -and ((Get-Item $dst).Length -gt 500KB)) { $ok = $true; break }
  } catch {
    Write-Output ("  下载失败: " + $_.Exception.Message)
  }
}
if (-not $ok) { Write-Output 'uv 下载失败 (可检查网络 / 代理后重试)。'; exit 1 }
$bin = Join-Path $env:USERPROFILE '.local\bin'
New-Item -ItemType Directory -Force -Path $bin | Out-Null
try {
  Expand-Archive -Path $dst -DestinationPath $bin -Force
} catch {
  Write-Output ('解压失败: ' + $_.Exception.Message); Remove-Item $dst -ErrorAction SilentlyContinue; exit 1
}
Remove-Item $dst -ErrorAction SilentlyContinue
$uv = Join-Path $bin 'uv.exe'
if (-not (Test-Path $uv)) { Write-Output 'uv.exe 解压后未找到。'; exit 1 }
# 国内镜像配置: 仅当用户尚无 uv.toml 时写入, 尊重既有配置
$cfgDir = Join-Path $env:APPDATA 'uv'
$cfg = Join-Path $cfgDir 'uv.toml'
if (-not (Test-Path $cfg)) {
  New-Item -ItemType Directory -Force -Path $cfgDir | Out-Null
  $toml = @'
# 由北极星写入: Python 解释器与依赖走国内镜像, 国内可达。删掉本文件即恢复 uv 默认源。
python-install-mirror = "https://gh-proxy.com/https://github.com/astral-sh/python-build-standalone/releases/download"
index-url = "https://pypi.tuna.tsinghua.edu.cn/simple"
'@
  Set-Content -Path $cfg -Value $toml -Encoding UTF8
  Write-Output "已写入 uv 国内镜像配置: $cfg"
}
Write-Output ("uv 安装完成: " + (& $uv --version))
Write-Output "已装到 $bin —— 重启后终端可直接用; 本应用内 Claude 立即可用 (uv run)。"
"#;

/// macOS uv 安装脚本 (POSIX sh): 下载 release tar.gz(国内代理加速)→ 解压到 `~/.local/bin` →
/// 写国内镜像配置 + 写 PATH 进 shell 配置(与 Node 安装一致)。免 sudo。
#[cfg(target_os = "macos")]
const MAC_UV_INSTALL_SCRIPT: &str = r#"
ARCH=$(uname -m)
case "$ARCH" in
  arm64|aarch64) UARCH=aarch64 ;;
  x86_64) UARCH=x86_64 ;;
  *) UARCH=x86_64 ;;
esac
ASSET="uv-${UARCH}-apple-darwin.tar.gz"
REL="https://github.com/astral-sh/uv/releases/latest/download/${ASSET}"
TMP="$(mktemp -d)"
TARBALL="$TMP/$ASSET"
OK=0
for U in \
  "https://gh-proxy.com/$REL" \
  "https://ghfast.top/$REL" \
  "$REL" ; do
  echo "下载: $U"
  if curl -fsSL "$U" -o "$TARBALL" && [ -s "$TARBALL" ]; then OK=1; break; fi
  echo "  下载失败, 试下一个镜像..."
done
if [ "$OK" != "1" ]; then echo "uv 下载失败 (检查网络/代理后重试)。"; rm -rf "$TMP"; exit 1; fi
BIN="$HOME/.local/bin"
mkdir -p "$BIN"
tar -xzf "$TARBALL" -C "$BIN" --strip-components=1 || { echo "解压失败。"; rm -rf "$TMP"; exit 1; }
rm -rf "$TMP"
if [ ! -x "$BIN/uv" ]; then echo "uv 解压后未找到。"; exit 1; fi
# 国内镜像配置: 仅当用户尚无 uv.toml 时写入
CFG="$HOME/.config/uv/uv.toml"
if [ ! -f "$CFG" ]; then
  mkdir -p "$HOME/.config/uv"
  cat > "$CFG" <<'EOF'
# 由北极星写入: Python 解释器与依赖走国内镜像, 国内可达。删掉本文件即恢复 uv 默认源。
python-install-mirror = "https://gh-proxy.com/https://github.com/astral-sh/python-build-standalone/releases/download"
index-url = "https://pypi.tuna.tsinghua.edu.cn/simple"
EOF
  echo "已写入 uv 国内镜像配置: $CFG"
fi
# 把 ~/.local/bin 写进 shell 配置 (zsh 为 macOS 默认), 已存在则不重复
LINE="export PATH=\"$BIN:\$PATH\""
for RC in "$HOME/.zshrc" "$HOME/.zprofile" "$HOME/.bash_profile" "$HOME/.profile" ; do
  touch "$RC" 2>/dev/null || true
  grep -qF "$BIN" "$RC" 2>/dev/null || printf '\n# Added by Polaris (uv)\n%s\n' "$LINE" >> "$RC"
done
echo "uv 安装完成: $("$BIN/uv" --version)"
echo "已写入 ~/.zshrc 等; 本应用内 Claude 立即可用 (uv run)。"
"#;

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn env_cancel(req_id: String) -> Result<(), String> {
    if let Some(mut child) = CHILDREN.remove(&req_id) {
        let _ = child.kill();
    }
    Ok(())
}

// ───────────────────────── 内部: 流式安装 ─────────────────────────

/// 构造一个跑给定内联命令的系统 shell 进程:
/// - Windows → PowerShell (见 `build_powershell`);
/// - 类 Unix(含 macOS) → `sh -lc`(`-l` 走登录配置以拿到用户 PATH, npm 全局 bin 才在内)。
/// 安装/更新 Claude Code 这类跨平台命令统一走它。
pub(crate) fn build_install_shell(inner: &str) -> Command {
    #[cfg(windows)]
    {
        build_powershell(inner)
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.args(["-lc", inner]);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }
}

/// Claude Code 的安装命令串 (按方式选择, 两端默认一致)。
/// - `npm` (**默认, 两端**): `npm i -g @anthropic-ai/claude-code --registry=npmmirror.com`。
///   原生二进制 (win32 / darwin-arm64 / darwin-x64 …) 经 `optionalDependencies` 由 npmmirror
///   **同源镜像**分发, postinstall 只把它拷成 `bin/claude` —— 整个过程**不碰 claude.ai / GCS**,
///   故国内 (含 macOS) 可装。这是国内最稳的路径, 需要 Node.js (缺则先 `env_install_node`)。
/// - `native` (**兜底, 境外网络**): 官方原生脚本 —— Windows `install.ps1` / macOS·Linux
///   `install.sh`。它从 claude.ai/GCS 拉二进制, **国内常被墙 → 默认不走**, 仅给能访问外网的人。
fn claude_install_cmd(method: &str) -> String {
    match method {
        "native" => {
            #[cfg(windows)]
            {
                "irm https://claude.ai/install.ps1 | iex".to_string()
            }
            #[cfg(not(windows))]
            {
                "curl -fsSL https://claude.ai/install.sh | bash".to_string()
            }
        }
        _ => "npm install -g @anthropic-ai/claude-code --registry=https://registry.npmmirror.com"
            .to_string(),
    }
}

/// 构造一个跑给定内联命令的 PowerShell 进程 (Bypass 执行策略, 以便 iex 远程脚本)。
fn build_powershell(inner: &str) -> Command {
    let mut cmd = Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        inner,
    ]);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    no_window(&mut cmd);
    cmd
}

fn emit(app: &AppHandle, ev: EnvStreamEvent) {
    let _ = app.emit("env:stream", ev);
}

/// 起子进程, 双线程读 stdout/stderr → `env:stream` 日志; 退出后(可选)修 PATH, 再发 done。
pub(crate) fn stream_install(
    app: AppHandle,
    req_id: String,
    mut cmd: Command,
    fix_path_after: bool,
    label: &str,
) {
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            emit(
                &app,
                EnvStreamEvent {
                    req_id,
                    kind: "done".into(),
                    line: None,
                    ok: Some(false),
                    message: Some(format!("启动安装进程失败: {e} (系统 shell 是否可用?)")),
                },
            );
            return;
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    CHILDREN.insert(req_id.clone(), child);

    // stderr 线程
    if let Some(stderr) = stderr {
        let app_e = app.clone();
        let req_e = req_id.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                let Ok(line) = line else { continue };
                if line.trim().is_empty() {
                    continue;
                }
                emit(
                    &app_e,
                    EnvStreamEvent {
                        req_id: req_e.clone(),
                        kind: "log".into(),
                        line: Some(line),
                        ok: None,
                        message: None,
                    },
                );
            }
        });
    }

    // stdout 线程 (主): 读完 → wait → 修 PATH → done
    let label = label.to_string();
    std::thread::spawn(move || {
        if let Some(stdout) = stdout {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { continue };
                if line.trim().is_empty() {
                    continue;
                }
                emit(
                    &app,
                    EnvStreamEvent {
                        req_id: req_id.clone(),
                        kind: "log".into(),
                        line: Some(line),
                        ok: None,
                        message: None,
                    },
                );
            }
        }

        let child_opt = CHILDREN.remove(&req_id);
        let success = if let Some(mut child) = child_opt {
            child.wait().map(|s| s.success()).unwrap_or(false)
        } else {
            // 被 cancel 掉了
            emit(
                &app,
                EnvStreamEvent {
                    req_id: req_id.clone(),
                    kind: "done".into(),
                    line: None,
                    ok: Some(false),
                    message: Some("安装已取消。".into()),
                },
            );
            return;
        };

        let mut message = if success {
            format!("{label} 安装完成。")
        } else {
            format!("{label} 安装未成功 (进程非零退出)，可查看上方日志或改用其他方式重试。")
        };

        // 装完 Node: 把 node bin 目录塞进**进程** PATH, 让同一会话里紧接着的 npm/claude 安装
        // 立刻找得到 npm —— 安装器(Win MSI / mac 写 shell 配置)只为「新进程/新终端」配 PATH,
        // 本进程不刷新就会「装了 Node 还说没 npm」。两端通用 (node_dir_candidates 按平台给候选)。
        if success {
            if let Some(dir) = node_dir_candidates().into_iter().find(|p| p.exists()) {
                prepend_process_path(&dir.to_string_lossy());
            }
        }

        // 装完 claude 的路径可能变了 → 清空 chat spawn 的解析缓存, 下次重新解析
        if success {
            *CLAUDE_EXE_CACHE.lock() = None;
            // 若真身 pwsh 已就位 (本次刚装好, 或本就装了), 顺手把它的目录注入 PATH(进程+用户),
            // 让本进程 spawn 的 claude 立刻用上 —— 装完 PowerShell 7 免重启即可对话。
            if let Some(fix) = ensure_pwsh_on_path() {
                if fix.ok && fix.status == "added" {
                    message.push('\n');
                    message.push_str(&fix.message);
                }
            }
        }

        // 成功后自动修 PATH (改环境变量) —— 这是「装完即可用」的关键
        if success && fix_path_after {
            let report = env_check_sync();
            if let Some(dir) = report.claude_dir {
                let fix = ensure_dir_on_path(&dir);
                message.push('\n');
                message.push_str(&fix.message);
            }
        }

        emit(
            &app,
            EnvStreamEvent {
                req_id: req_id.clone(),
                kind: "done".into(),
                line: None,
                ok: Some(success),
                message: Some(message),
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把内嵌的安装脚本原文 dump 到临时文件, 供外部用 PowerShell AST 解析器做语法校验
    /// (内嵌脚本的语法错误只会在「真正安装」时才暴露, 这里提前抓出来)。`--ignored` 手动跑。
    #[test]
    #[ignore]
    fn dump_install_scripts() {
        let dir = std::env::temp_dir();
        let node = dir.join("polaris_node_install.ps1");
        let pwsh = dir.join("polaris_pwsh_install.ps1");
        std::fs::write(&node, NODE_INSTALL_SCRIPT).unwrap();
        std::fs::write(&pwsh, PWSH_INSTALL_SCRIPT).unwrap();
        println!("NODE_SCRIPT={}", node.display());
        println!("PWSH_SCRIPT={}", pwsh.display());
    }
}
