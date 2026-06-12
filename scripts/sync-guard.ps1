<#
.SYNOPSIS
  Docker 同步守卫 —— 把「选择性 graft，禁整覆盖」的口头纪律固化成可执行检查。

.DESCRIPTION
  polaris_docker 仓与主仓 (polaris-app) 选择性同步。本脚本读 .docker-owned 保护清单，
  把主仓本版改动的文件分成两类，**杜绝 Docker-only 改动被静默覆盖**：

    OWNED （命中 .docker-owned）          → 拒绝 Copy-Item，必须逐 hunk 手并。
    SAFE  （共享且未分叉）                → 允许整文件覆盖（那份本来就是主仓快照）。

  三种用法：
    -Plan    （默认）列出本版主仓改了哪些文件、各自分类，不动磁盘。
    -Apply   只对 SAFE 文件执行 Copy-Item（OWNED 永远跳过，仅打印提醒）。
    -Audit   用 git 反查 Docker 实际分叉了哪些文件，提醒 .docker-owned 漏登记了谁。

.EXAMPLE
  pwsh scripts/sync-guard.ps1                          # 看本版要并哪些、哪些得手并
  pwsh scripts/sync-guard.ps1 -Apply                   # 自动并安全文件，OWNED 留给你手并
  pwsh scripts/sync-guard.ps1 -Audit                   # 体检：清单是否覆盖了所有分叉文件
#>
[CmdletBinding(DefaultParameterSetName = 'Plan')]
param(
  # 主仓路径（默认假设与本仓同级：..\polaris-app）。
  [string]$WinRepo = (Join-Path (Split-Path $PSScriptRoot -Parent) '..\polaris-app'),
  # 主仓「本版改动」的起点 ref（默认 = 主仓最近一个 tag）。
  [string]$SinceRef = '',
  [Parameter(ParameterSetName = 'Apply')][switch]$Apply,
  [Parameter(ParameterSetName = 'Audit')][switch]$Audit
)

$ErrorActionPreference = 'Stop'
$DockerRepo = Split-Path $PSScriptRoot -Parent
$ownedFile  = Join-Path $DockerRepo '.docker-owned'
if (-not (Test-Path $ownedFile)) { throw ".docker-owned 不存在于 $DockerRepo" }

# ── 读保护清单 → glob 正则 ───────────────────────────────────────
$patterns = Get-Content $ownedFile | ForEach-Object { $_.Trim() } |
  Where-Object { $_ -and -not $_.StartsWith('#') }

function Test-Owned([string]$rel) {
  $rel = $rel -replace '\\', '/'
  foreach ($p in $patterns) {
    # glob → regex：** = 任意层级，* = 同层任意，其余转义。
    $rx = [regex]::Escape($p) -replace '\\\*\\\*', '.*' -replace '\\\*', '[^/]*'
    if ($rel -match "^$rx$") { return $true }
  }
  return $false
}

# ── -Audit：git 反查实际分叉 vs 清单覆盖 ─────────────────────────
if ($Audit) {
  Push-Location $DockerRepo
  try {
    $syncPt = (git log --grep='^sync:' -1 --format='%H').Trim()
    if (-not $syncPt) { throw "找不到上次 sync: commit（无法反查分叉）" }
    Write-Host "上次同步点: $syncPt  ($((git log -1 --format='%s' $syncPt)))" -ForegroundColor Cyan
    $diverged = git diff --name-only "$syncPt..HEAD" | Sort-Object -Unique
    $missing = @()
    foreach ($f in $diverged) {
      # 忽略纯产物 / 缓存
      if ($f -match '__pycache__|\.pyc$') { continue }
      if (-not (Test-Owned $f)) { $missing += $f }
    }
    if ($missing) {
      Write-Host "`n⚠️ 以下文件 Docker 已分叉，但 .docker-owned 没登记（可能被误覆盖风险）：" -ForegroundColor Yellow
      $missing | ForEach-Object { Write-Host "   $_" -ForegroundColor Yellow }
      Write-Host "`n→ 确认是 Docker-only 逻辑就加进 .docker-owned；若只是上次同步顺带改的共享文件可忽略。"
    } else {
      Write-Host "`n✅ .docker-owned 覆盖了全部分叉文件，清单是健康的。" -ForegroundColor Green
    }
  } finally { Pop-Location }
  return
}

# ── -Plan / -Apply：分类主仓本版改动 ─────────────────────────────
if (-not (Test-Path $WinRepo)) { throw "主仓不存在: $WinRepo（用 -WinRepo 指定）" }
$WinRepo = (Resolve-Path $WinRepo).Path

Push-Location $WinRepo
try {
  if (-not $SinceRef) { $SinceRef = (git describe --tags --abbrev=0).Trim() }
  Write-Host "主仓: $WinRepo" -ForegroundColor Cyan
  Write-Host "本版改动范围: $SinceRef..HEAD`n" -ForegroundColor Cyan
  $changed = git diff --name-only "$SinceRef..HEAD" | Sort-Object -Unique
} finally { Pop-Location }

$owned = @(); $safe = @(); $skip = @()
foreach ($f in $changed) {
  $src = Join-Path $WinRepo $f
  $dst = Join-Path $DockerRepo $f
  if (-not (Test-Path $src)) { continue }                 # 主仓删除的，不动 Docker
  if (Test-Owned $f) { $owned += $f; continue }           # 分叉文件：禁覆盖
  if (-not (Test-Path $dst)) { $skip += $f; continue }    # Docker 没这文件：多半 desktop-only，跳过
  $safe += $f
}

Write-Host "── OWNED（逐 hunk 手并，禁整覆盖；$($owned.Count) 个）──" -ForegroundColor Yellow
$owned | ForEach-Object { Write-Host "  ✋ $_" -ForegroundColor Yellow }
Write-Host "`n── SAFE（共享未分叉，可整覆盖；$($safe.Count) 个）──" -ForegroundColor Green
$safe  | ForEach-Object { Write-Host "  ✓ $_" -ForegroundColor Green }
Write-Host "`n── SKIP（Docker 无此文件，疑 desktop-only；$($skip.Count) 个）──" -ForegroundColor DarkGray
$skip  | ForEach-Object { Write-Host "  · $_" -ForegroundColor DarkGray }

if ($Apply) {
  Write-Host "`n[Apply] 仅覆盖 SAFE 文件…" -ForegroundColor Cyan
  foreach ($f in $safe) {
    $src = Join-Path $WinRepo $f; $dst = Join-Path $DockerRepo $f
    New-Item -ItemType Directory -Force (Split-Path $dst) | Out-Null
    Copy-Item $src $dst -Force
    Write-Host "  copied  $f"
  }
  Write-Host "`n✋ OWNED 文件未动——请对照主仓逐 hunk 手并后再 commit。" -ForegroundColor Yellow
} else {
  Write-Host "`n（这是 -Plan 预览。加 -Apply 才会覆盖 SAFE 文件；OWNED 永远只能手并。）" -ForegroundColor DarkGray
}
