<#
.SYNOPSIS
  在 Windows 上把 GHCR 上的 polaris 镜像拉到群晖 NAS（绕开 NAS 国内网拉不动 GHCR 的问题）。

.DESCRIPTION
  流程：
    1. docker pull ghcr.io/wuli2025/polaris:<TAG>      （在 Windows 跑，Windows 一般有代理/能上 GHCR）
    2. docker save -o polaris-web-<tag>.tar            （本地临时目录）
    3. Copy-Item 到 Z:\群晖\                              （Z: 已经是 NAS 的 SMB 挂载）
    4. SSH 进 NAS，docker load + docker compose up -d   （不重建镜像，仅替换容器）

  适用：NAS 国内网拉不动 GHCR、但 Windows 能拉。

.PARAMETER Tag
  镜像 tag。默认 latest（slim 口味）。full 用 POLARIS_RENDER=1 重出的镜像。

.PARAMETER SkipPull
  跳过 pull（用本地已加载的镜像直接 save）。调试用。

.PARAMETER SkipRestart
  加载到 NAS 但不重启容器（你可以手动进 DSM Container Manager 重启）。

.EXAMPLE
  .\pull-ghcr-to-nas.ps1
  .\pull-ghcr-to-nas.ps1 -Tag full
  .\pull-ghcr-to-nas.ps1 -Tag v1.0.3 -SkipRestart
#>
[CmdletBinding()]
param(
    [string]$Tag = "latest",
    [switch]$SkipPull,
    [switch]$SkipRestart
)

$ErrorActionPreference = "Stop"
$Image       = "ghcr.io/wuli2025/polaris:$Tag"
$NasHost     = "192.168.123.154"
$NasUser     = "zz"
$NasShareSrc = "\\$NasHost\tx\群晖\polaris-app"     # SMB 路径（如果 Z: 已挂载，复制会更快）
$TmpTar      = Join-Path $env:TEMP "polaris-web-$Tag.tar"
$TarOnNas    = "/volume1/tx/群晖/polaris-web-$Tag.tar"

function Step($msg) { Write-Host "`n>>> $msg" -ForegroundColor Cyan }
function Ok($msg)   { Write-Host "  OK   $msg" -ForegroundColor Green }
function Warn($msg) { Write-Host "  WARN $msg" -ForegroundColor Yellow }
function Fail($msg) { Write-Host "  FAIL $msg" -ForegroundColor Red; throw $msg }

# ── 0. 依赖检查 ─────────────────────────────────────────────
Step "0. 检查 Docker Desktop 是否在跑"
$dockerOk = $false
foreach ($i in 1..20) {
    if (& docker info 2>$null) { $dockerOk = $true; break }
    if ($i -eq 1) { Warn "Docker Desktop 还没就绪，等最多 ~60s..." }
    Start-Sleep -Seconds 3
}
if (-not $dockerOk) { Fail "Docker Desktop 没起来。请先手动启动 Docker Desktop。" }
Ok "Docker Desktop OK"

# ── 1. 在 Windows 拉 GHCR 镜像 ─────────────────────────────
if (-not $SkipPull) {
    Step "1. 拉取 $Image"
    & docker pull $Image
    if ($LASTEXITCODE -ne 0) { Fail "docker pull 失败：Windows 上也拉不到 GHCR（公司网络封了？）。" }
    Ok "拉取完成"
} else {
    Warn "SkipPull 已指定：直接用本地镜像 save（不会重新拉）"
}

# ── 2. docker save 到本地临时 tar ─────────────────────────
Step "2. docker save -> $TmpTar"
if (Test-Path $TmpTar) { Remove-Item $TmpTar -Force }
& docker save -o $TmpTar $Image
if ($LASTEXITCODE -ne 0) { Fail "docker save 失败" }
$mb = [math]::Round((Get-Item $TmpTar).Length / 1MB, 1)
Ok "导出完成：$mb MB"

# ── 3. 拷贝到 NAS（优先 Z:，否则走 SMB） ─────────────────
$copied = $false
$mountZ = "Z:\群晖"
if (Test-Path $mountZ) {
    Step "3a. 拷贝到 Z: (本地挂载的 NAS 共享)"
    $dst = Join-Path $mountZ "polaris-web-$Tag.tar"
    Copy-Item $TmpTar $dst -Force
    $mbNas = [math]::Round((Get-Item $dst).Length / 1MB, 1)
    Ok "Z: 上大小：$mbNas MB"
    $copied = $true
} elseif (Test-Path $NasShareSrc) {
    Step "3b. 拷贝到 $NasShareSrc （通过 SMB）"
    $dst = Join-Path $NasShareSrc "polaris-web-$Tag.tar"
    Copy-Item $TmpTar $dst -Force
    Ok "SMB 拷贝完成"
    $copied = $true
} else {
    Warn "Z: 也没挂载、SMB \\$NasHost\tx 也连不上。请先挂载 Z: 或确保 SMB 通。"
    Fail "无法把 tar 传到 NAS"
}

# ── 4. SSH 进 NAS，docker load + 重启 ───────────────────
if ($SkipRestart) {
    Warn "SkipRestart 已指定：tar 已落到 NAS（$TarOnNas），请手动到 DSM Container Manager 重建容器。"
    return
}

# 复用 polaris-app 项目里既有的 askpass 助手（已存在则直接用）
$askpass = Join-Path $env:TEMP "nas_askpass.cmd"
if (-not (Test-Path $askpass)) {
    Write-Host "  需要 NAS $NasUser 的 SSH 密码来 docker load + restart。" -ForegroundColor Yellow
    $pwdSecure = Read-Host "  请输入 NAS $NasUser 密码" -AsSecureString
    $nasPwd = [System.Runtime.InteropServices.Marshal]::PtrToStringAuto(
        [System.Runtime.InteropServices.Marshal]::SecureStringToBSTR($pwdSecure))
    Set-Content -Path $askpass -Value "@echo $nasPwd" -Encoding ascii
} else {
    # 从本机 askpass 文件取回密码给 sudo -S 用——密码只存在于本机临时文件，绝不写进仓库
    $nasPwd = (Get-Content $askpass -First 1) -replace '^@echo ', ''
}
$env:SSH_ASKPASS = $askpass
$env:SSH_ASKPASS_REQUIRE = "force"
$env:DISPLAY = "localhost:0"

Step "4. SSH 进 NAS，docker load + 重建容器"
$dockerNas = "/var/packages/ContainerManager/target/usr/bin/docker"
$projDir   = "/volume1/tx/群晖/polaris-app"
$restartCmd = @"
echo '[1/3] docker load';
echo '$nasPwd' | sudo -S $dockerNas load -i '$TarOnNas' 2>&1 | tail -3;
echo '[2/3] docker tag image to polaris-web:latest so compose can find it';
echo '$nasPwd' | sudo -S $dockerNas tag $Image polaris-web:latest 2>&1;
echo '[3/3] docker compose up -d (no-build)';
cd '$projDir' && echo '$nasPwd' | sudo -S bash -c "cd '$projDir' && POLARIS_RENDER=0 POLARIS_TAG='$Tag' $dockerNas compose -f docker-compose.synology.yml up -d --no-build" 2>&1 | tail -10;
"@
ssh -o PreferredAuthentications=password -o PubkeyAuthentication=no `
    -o ConnectTimeout=10 -o StrictHostKeyChecking=no `
    "$NasUser@$NasHost" $restartCmd

if ($LASTEXITCODE -eq 0) {
    Ok "🎉 更新完成。打开 http://$NasHost`:8080/?token=<POLARIS_AUTH_TOKEN> 看新版本。"
} else {
    Fail "SSH 步骤退出码 $LASTEXITCODE"
}
