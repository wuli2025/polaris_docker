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
        let cmd = build_powershell(&node_install_script());
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

/// 三个 Windows 安装脚本 (PowerShell 7 / Node.js / uv) 共用的下载前奏 —— 两项环境设置 +
/// 一个带校验和重试的下载函数。
///
/// **① 关掉 IWR 的进度条**: 本应用以**无窗口**方式 spawn PowerShell、stdout 又被重定向进管道,
/// 这种上下文下 Windows PowerShell 5.1 的进度条渲染会把 `Invoke-WebRequest` 拖慢一个数量级 ——
/// 同机同镜像实测 584 KB/s → 4266 KB/s (**7.3 倍**)。~107MB 的 PowerShell MSI 因此在网速一般的
/// 机器上会一路摸到 `-TimeoutSec` 上限而失败。**这是「PowerShell 7 有些概率装不上」的主因**:
/// 成败取决于当时网速, 所以时好时坏。
///
/// **② 显式并入 TLS 1.2**: 老的 / 受管的机器上 5.1 的默认安全协议可能仍是 TLS 1.0, 而 GitHub
/// 与各镜像早已只收 TLS 1.2+ → 握手阶段直接失败。
///
/// **③ 下载校验**: 国内 GitHub 代理挂掉时常常回「200 + 一个 HTML/JSON 错误页」, 老代码只看
/// `Length -gt 1MB`, 于是把错误页当安装包喂给 msiexec, 报一句用户看不懂的错。这里改成校验
/// **体积下限 + 文件头魔数**, 且整轮镜像都没成会再重试一轮 (连接被重置在国内很常见)。
const PS_DOWNLOAD_PRELUDE: &str = r#"
$ErrorActionPreference = 'Continue'
$ProgressPreference = 'SilentlyContinue'
try { [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12 } catch {}

# 结果写 $global:PolarisDlOk 而不是用返回值 —— 函数里的 Write-Output 会混进返回值 (PowerShell 老坑)。
$global:PolarisDlOk = $false
function Get-PolarisFile {
  param([string[]]$Urls, [string]$Dest, [string]$Magic, [long]$MinBytes)
  $global:PolarisDlOk = $false
  foreach ($round in 1..2) {
    foreach ($u in $Urls) {
      try {
        Write-Output "下载: $u"
        if (Test-Path $Dest) { Remove-Item $Dest -Force -ErrorAction SilentlyContinue }
        Invoke-WebRequest -Uri $u -OutFile $Dest -UseBasicParsing -TimeoutSec 600
        if (-not (Test-Path $Dest)) { Write-Output '  下载后没见到文件'; continue }
        $len = (Get-Item $Dest).Length
        if ($len -lt $MinBytes) {
          Write-Output ('  体积不对: {0:N0} 字节, 至少该有 {1:N0} —— 多半是代理回的错误页' -f $len, $MinBytes)
          continue
        }
        $need = [int]($Magic.Length / 2)
        $head = New-Object byte[] $need
        $fs = [IO.File]::OpenRead($Dest)
        $read = $fs.Read($head, 0, $need)
        $fs.Close()
        if ($read -lt $need) { Write-Output '  文件头都读不满, 显然不对'; continue }
        $got = (($head | ForEach-Object { $_.ToString('x2') }) -join '')
        if ($got -ne $Magic) {
          Write-Output ('  文件头不对: {0} (应为 {1}) —— 多半是代理回的错误页' -f $got, $Magic)
          continue
        }
        Write-Output ('  校验通过: {0:N1} MB' -f ($len / 1MB))
        $global:PolarisDlOk = $true
        return
      } catch {
        Write-Output ('  下载失败: ' + $_.Exception.Message)
      }
    }
    if ($round -eq 1) { Write-Output '一轮镜像都没成功, 歇 2 秒再来一轮...'; Start-Sleep -Seconds 2 }
  }
}
"#;

/// msiexec 静默安装的共用封装 (PowerShell 7 / Node.js 两个 MSI 都用)。
/// 装到 Program Files 要管理员 → `RunAs` 触发 UAC (用户拒绝则友好报错, 不静默失败)。
/// **1618 = 「另一个安装正在进行」** (撞上 Windows Update 或别的 MSI) —— 这是典型的**概率性**
/// 失败, 老代码直接判死, 这里等一会儿重试, 最多三次。结果写 $global:PolarisMsiCode。
const PS_MSIEXEC_HELPER: &str = r#"
$global:PolarisMsiCode = -1
function Install-PolarisMsi {
  param([string]$Path, [string]$ExtraArgs = '')
  foreach ($try in 1..3) {
    try {
      $a = '/i "' + $Path + '" /quiet /norestart'
      if ($ExtraArgs -ne '') { $a = $a + ' ' + $ExtraArgs }
      $p = Start-Process msiexec.exe -ArgumentList $a -Wait -PassThru -Verb RunAs
      $global:PolarisMsiCode = $p.ExitCode
    } catch {
      Write-Output ('安装启动失败 (可能未授予管理员权限): ' + $_.Exception.Message)
      $global:PolarisMsiCode = -1
      return
    }
    if ($global:PolarisMsiCode -ne 1618) { return }
    Write-Output ('另一个安装程序正在运行 (msiexec 1618), 等 15 秒重试 ({0}/3)...' -f $try)
    Start-Sleep -Seconds 15
  }
}
"#;

/// MSI = OLE 复合文档, 文件头魔数固定为 `D0CF11E0A1B11AE1`。
const MSI_MAGIC: &str = "d0cf11e0a1b11ae1";
/// zip 文件头魔数 (`PK\x03\x04`) —— uv 的 release 包。
const ZIP_MAGIC: &str = "504b0304";

/// 自家依赖包的分发基址 —— Cloudflare `/downloads/*` 由 `functions/downloads/[[path]].js`
/// 从 R2 桶 `polaris-downloads` 流式取出 (支持 Range、边缘缓存 24h、出站免费)。
/// 对应的 R2 key 前缀是 `deps/`(**不含** `downloads/`, 那是路由前缀不是 key 的一部分)。
///
/// 传新包: `wrangler r2 object put polaris-downloads/deps/<文件名> --file <本地路径> --remote`
/// 传完务必对着生产域名核字节数与文件头魔数 —— 与发版终验同一套规矩。
const DEPS_BASE: &str = "https://llmwiki.cloud/downloads/deps";

/// 这三个版本号**同时**决定「下载哪个官方包」与「R2 里该有哪个包」——
/// 改任何一个都必须把对应文件传进 R2 的 `deps/` (否则 R2 那跳 404, 白白退化到公共代理)。
const PWSH_VER: &str = "7.4.6";
const NODE_VER: &str = "20.18.1";
const UV_VER: &str = "0.11.29";

/// 把脚本正文里的占位符换成真值。用占位符而非 `format!` 的 `{}` —— PowerShell 脚本里全是
/// `${...}`/`{0:N0}` 这类花括号, 走 format! 得把每个都转义成 `{{}}`, 既难读又极易写错。
fn fill(script: &str) -> String {
    script
        .replace("DEPS_BASE", DEPS_BASE)
        .replace("MSI_MAGIC", MSI_MAGIC)
        .replace("ZIP_MAGIC", ZIP_MAGIC)
        .replace("PWSH_VER", PWSH_VER)
        .replace("NODE_VER", NODE_VER)
        .replace("UV_VER", UV_VER)
}

/// Node.js LTS 安装脚本 (前奏 + msiexec 封装 + 正文)。
fn node_install_script() -> String {
    fill(&format!(
        "{PS_DOWNLOAD_PRELUDE}\n{PS_MSIEXEC_HELPER}\n{NODE_INSTALL_BODY}"
    ))
}

/// PowerShell 7 安装脚本 (前奏 + msiexec 封装 + 正文)。
fn pwsh_install_script() -> String {
    fill(&format!(
        "{PS_DOWNLOAD_PRELUDE}\n{PS_MSIEXEC_HELPER}\n{PWSH_INSTALL_BODY}"
    ))
}

/// uv 安装脚本 (前奏 + 正文; uv 是绿色单文件, 不走 msiexec)。
#[cfg(windows)]
fn uv_install_script() -> String {
    fill(&format!("{PS_DOWNLOAD_PRELUDE}\n{UV_INSTALL_BODY}"))
}

/// macOS uv 安装脚本 (POSIX sh)。
#[cfg(target_os = "macos")]
fn mac_uv_install_script() -> String {
    fill(MAC_UV_INSTALL_SCRIPT)
}

/// Node.js LTS 安装脚本正文: winget 优先, 失败则下载官方 MSI (国内 npmmirror 镜像加速) 静默安装。
/// 选 20.x LTS ("Iron"): 长期支持、兼容 Windows 10。
const NODE_INSTALL_BODY: &str = r#"
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
# ② 下载 Node LTS MSI -> %TEMP% -> msiexec 静默安装。自家 R2 打头, 后面依次 npmmirror、官方直连。
$ver = 'NODE_VER'
$arch = switch ($env:PROCESSOR_ARCHITECTURE) { 'ARM64' { 'arm64' } 'AMD64' { 'x64' } default { 'x86' } }
$msi = "node-v$ver-$arch.msi"
$dst = Join-Path $env:TEMP $msi
$urls = @(
  "DEPS_BASE/$msi",
  "https://cdn.npmmirror.com/binaries/node/v$ver/$msi",
  "https://npmmirror.com/mirrors/node/v$ver/$msi",
  "https://nodejs.org/dist/v$ver/$msi"
)
Get-PolarisFile -Urls $urls -Dest $dst -Magic 'MSI_MAGIC' -MinBytes 15MB
if (-not $global:PolarisDlOk) {
  Write-Output 'Node.js 安装包下载失败 (可检查网络 / 代理后重试)。'
  exit 1
}
Write-Output "安装中 (msiexec, 会弹一次 UAC 授权): $dst"
Install-PolarisMsi -Path $dst
Remove-Item $dst -ErrorAction SilentlyContinue
if ($global:PolarisMsiCode -ne 0) { Write-Output ('msiexec 退出码 ' + $global:PolarisMsiCode); exit 1 }
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
    let cmd = build_powershell(&pwsh_install_script());
    stream_install(app, req_id.clone(), cmd, false, "PowerShell 7");
    Ok(req_id)
}

/// PowerShell 7 安装脚本正文: winget 优先, 失败则下载 MSI 静默安装。
/// 版本仅用于 MSI 兜底直链 (winget 路径自动取最新); 选 7.4.x LTS, 稳定且长期可用。
const PWSH_INSTALL_BODY: &str = r#"
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
# ② 下载 MSI -> %TEMP% -> msiexec 静默安装。**自家 R2 排第一** —— 公共 GitHub 代理时好时坏
#    (实测 gh-proxy.com 已 500, 故直接摘掉), 而 R2 的包是发版时自己传上去、字节数与官方逐一
#    核对过的, 出站还免费。后面仍留公共代理与 GitHub 直连兜底: R2/CF 万一挂了也装得上。
#    (arm64/x64 在 R2 里都有; 32 位 x86 没镜像 → R2 那跳 404, 自动落到后面的源, 不影响。)
$ver = 'PWSH_VER'
$arch = switch ($env:PROCESSOR_ARCHITECTURE) { 'ARM64' { 'arm64' } 'AMD64' { 'x64' } default { 'x86' } }
$msi = "PowerShell-$ver-win-$arch.msi"
$dst = Join-Path $env:TEMP $msi
$rel = "https://github.com/PowerShell/PowerShell/releases/download/v$ver/$msi"
$urls = @(
  "DEPS_BASE/$msi",
  "https://ghfast.top/$rel",
  "https://ghproxy.net/$rel",
  $rel
)
Get-PolarisFile -Urls $urls -Dest $dst -Magic 'MSI_MAGIC' -MinBytes 40MB
if (-not $global:PolarisDlOk) {
  Write-Output 'PowerShell 7 安装包下载失败 (可检查网络 / 代理后重试)。'
  exit 1
}
Write-Output "安装中 (msiexec, 会弹一次 UAC 授权): $dst"
Install-PolarisMsi -Path $dst -ExtraArgs 'ADD_PATH=1'
Remove-Item $dst -ErrorAction SilentlyContinue
if ($global:PolarisMsiCode -ne 0) { Write-Output ('msiexec 退出码 ' + $global:PolarisMsiCode); exit 1 }
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
        let cmd = build_powershell(&uv_install_script());
        #[cfg(target_os = "macos")]
        let cmd = build_install_shell(&mac_uv_install_script());
        stream_install(app, req_id.clone(), cmd, false, "uv");
        Ok(req_id)
    }
}

/// Windows uv 安装脚本正文: 下载 release zip → 解压到 `~/.local/bin` → 写国内镜像配置。
/// uv 是 MIT/Apache 双许可的单文件二进制, 解压即用, 不需要管理员权限。
///
/// **版本从 `latest/download` 改成锁定**: 要把包镜像进自家 R2 就必须锁版本 (R2 里放的是某个
/// 确定版本的字节)。副作用是升 uv 得改这里的版本号并重传 R2 —— 换来的是不再被公共代理拖累。
#[cfg(windows)]
const UV_INSTALL_BODY: &str = r#"
$arch = switch ($env:PROCESSOR_ARCHITECTURE) { 'ARM64' { 'aarch64' } 'AMD64' { 'x86_64' } default { 'x86_64' } }
$ver = 'UV_VER'
$asset = "uv-$arch-pc-windows-msvc.zip"
$rel = "https://github.com/astral-sh/uv/releases/download/$ver/$asset"
$urls = @(
  "DEPS_BASE/$asset",
  "https://ghfast.top/$rel",
  "https://ghproxy.net/$rel",
  $rel
)
$dst = Join-Path $env:TEMP $asset
Get-PolarisFile -Urls $urls -Dest $dst -Magic 'ZIP_MAGIC' -MinBytes 5MB
if (-not $global:PolarisDlOk) { Write-Output 'uv 下载失败 (可检查网络 / 代理后重试)。'; exit 1 }
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
VER='UV_VER'
REL="https://github.com/astral-sh/uv/releases/download/${VER}/${ASSET}"
TMP="$(mktemp -d)"
TARBALL="$TMP/$ASSET"
OK=0
# 自家 R2 打头 (发版时传上去、字节数与官方核对过), 公共代理与 GitHub 直连兜底。
# 每个源都校验 gzip 魔数 (1f8b) —— 代理挂掉时常回「200 + HTML 错误页」, 只看「文件非空」会把
# 错误页当安装包喂给 tar, 报一句看不懂的错。
for U in \
  "DEPS_BASE/$ASSET" \
  "https://ghfast.top/$REL" \
  "https://ghproxy.net/$REL" \
  "$REL" ; do
  echo "下载: $U"
  if curl -fsSL --retry 2 --retry-delay 2 "$U" -o "$TARBALL" && [ -s "$TARBALL" ]; then
    if [ "$(head -c 2 "$TARBALL" | od -An -tx1 | tr -d ' \n')" = "1f8b" ]; then OK=1; break; fi
    echo "  文件头不是 gzip —— 多半是代理回的错误页, 换下一个源..."
  else
    echo "  下载失败, 试下一个镜像..."
  fi
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

    /// 占位符必须全部被填掉 —— 漏一个就会把 `DEPS_BASE/xxx.msi` 这种字面量当 URL 去下载。
    #[test]
    fn scripts_have_no_unfilled_placeholders() {
        let mut all = vec![
            ("node", node_install_script()),
            ("pwsh", pwsh_install_script()),
        ];
        #[cfg(windows)]
        all.push(("uv", uv_install_script()));
        #[cfg(target_os = "macos")]
        all.push(("mac-uv", mac_uv_install_script()));
        for (name, s) in &all {
            for ph in ["DEPS_BASE", "MSI_MAGIC", "ZIP_MAGIC", "PWSH_VER", "NODE_VER", "UV_VER"] {
                assert!(!s.contains(ph), "{name} 脚本里还留着未替换的占位符 {ph}");
            }
            assert!(
                s.contains(DEPS_BASE),
                "{name} 脚本应把自家 R2 源排进候选 (否则镜像白传)"
            );
        }
    }

    /// R2 源必须排在公共代理**前面** —— 顺序错了等于没镜像 (用户仍先撞 gh-proxy 那类不稳定源)。
    #[test]
    fn r2_source_comes_first() {
        for (name, s) in [("node", node_install_script()), ("pwsh", pwsh_install_script())] {
            let r2 = s.find(DEPS_BASE).expect("应含 R2 源");
            for other in ["ghfast.top", "ghproxy.net", "cdn.npmmirror.com", "nodejs.org"] {
                if let Some(i) = s.find(other) {
                    assert!(r2 < i, "{name}: R2 源应排在 {other} 之前");
                }
            }
        }
        // 实测已 500 的 gh-proxy.com 不该再出现在下载候选里。只认 `$urls` 里的 URL 字面量形态,
        // 别把正文里解释「为什么摘掉它」的那句注释也算进来。
        assert!(!pwsh_install_script().contains("\"https://gh-proxy.com/"));
    }

    /// 关掉 IWR 进度条是「PowerShell 7 有些概率装不上」的主修 —— 实测 584KB/s → 4266KB/s。
    /// 三个 Windows 脚本一个都不能漏, 故在此锁死。
    #[test]
    fn windows_scripts_disable_progress_bar() {
        let mut all = vec![node_install_script(), pwsh_install_script()];
        #[cfg(windows)]
        all.push(uv_install_script());
        for s in &all {
            assert!(s.contains("$ProgressPreference = 'SilentlyContinue'"));
        }
    }

    /// 把最终生成的安装脚本原文 dump 到临时目录, 供真机手动跑「下载段」核对
    /// (R2 源是否命中、魔数校验是否放行)。真下载几十上百 MB, 故不进常规测试。
    /// `cargo test -p polaris-kernel dump_install_scripts -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn dump_install_scripts() {
        let dir = std::env::temp_dir();
        let mut all = vec![
            ("node", node_install_script()),
            ("pwsh", pwsh_install_script()),
        ];
        #[cfg(windows)]
        all.push(("uv", uv_install_script()));
        #[cfg(target_os = "macos")]
        all.push(("mac-uv", mac_uv_install_script()));
        for (name, src) in &all {
            let ext = if name.starts_with("mac") { "sh" } else { "ps1" };
            let p = dir.join(format!("polaris_{name}_install.{ext}"));
            let mut bytes = vec![0xEF, 0xBB, 0xBF]; // UTF-8 BOM: 见上面解析测试里的说明
            bytes.extend_from_slice(src.as_bytes());
            std::fs::write(&p, &bytes).unwrap();
            println!("{name} -> {}", p.display());
        }
    }

    /// 内嵌 PowerShell 脚本的语法错误以前只有「用户真的点安装」时才暴露。
    /// 这里用 PowerShell 自带的 AST 解析器在**测试期**就把它抓出来。
    #[test]
    #[cfg(windows)]
    fn install_scripts_parse_as_valid_powershell() {
        for (name, src) in [
            ("node", node_install_script()),
            ("pwsh", pwsh_install_script()),
            ("uv", uv_install_script()),
        ] {
            let f = std::env::temp_dir().join(format!("polaris_test_{name}_install.ps1"));
            // 必须写 UTF-8 BOM: Windows PowerShell 5.1 读**无 BOM** 的 .ps1 会按 ANSI(GBK) 解,
            // 脚本里的中文当场被搅碎、引号配不上对, 报一堆假的语法错。
            // (运行时那条路不经文件 —— build_powershell 用 `-Command` 传, Rust 经 CreateProcessW
            //  给的是 UTF-16, 没这个问题。这里是为了让 ParseFile 看到跟内存里一样的字节。)
            let mut bytes = vec![0xEF, 0xBB, 0xBF];
            bytes.extend_from_slice(src.as_bytes());
            std::fs::write(&f, &bytes).expect("写临时脚本");
            // 单引号字符串里反斜杠是字面量, 无需转义; 只需把路径里的单引号翻倍。
            let path = f.display().to_string().replace('\'', "''");
            let script = format!(
                "$errs = $null; \
                 $null = [System.Management.Automation.Language.Parser]::ParseFile('{path}', [ref]$null, [ref]$errs); \
                 if ($errs -and $errs.Count -gt 0) {{ $errs | ForEach-Object {{ Write-Output $_.ToString() }}; exit 1 }}"
            );
            let out = std::process::Command::new("powershell")
                .args(["-NoProfile", "-NonInteractive", "-Command", &script])
                .output()
                .expect("跑 PowerShell 解析器");
            assert!(
                out.status.success(),
                "{name} 安装脚本语法有错:\n{}",
                String::from_utf8_lossy(&out.stdout)
            );
            let _ = std::fs::remove_file(&f);
        }
    }
}
