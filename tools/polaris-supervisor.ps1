<#
  Polaris 监督式自启 / 看门狗 (Windows)
  ─────────────────────────────────────
  目标:让 Polaris 桌面端「永远在跑」——进程崩溃/退出后自动拉起,带崩溃环路退避
  (防止崩→拉→崩 的热循环把 CPU 打满),并把每次重启与内存高水位记进日志。

  这是大厂长驻服务的标配「监督者(supervisor)」层:应用自身已做了进程内的稳定硬化,
  这一层兜住「万一真的崩了」的最后一道——重新拉起,且可观测。

  用法:
    # 前台运行(测试):
    pwsh -ExecutionPolicy Bypass -File .\polaris-supervisor.ps1

    # 指定 exe 路径:
    pwsh -File .\polaris-supervisor.ps1 -ExePath "C:\Users\<you>\AppData\Local\Polaris\Polaris.exe"

    # 注册为「登录时自动启动」的计划任务(开机即有人盯着,reboot-proof):
    pwsh -File .\polaris-supervisor.ps1 -Install

    # 取消注册:
    pwsh -File .\polaris-supervisor.ps1 -Uninstall

  说明:
    * 不会开多个 Polaris:启动前先看是否已有同名进程在跑,有就只监督不重开。
    * 崩溃退避:连续崩溃时等待时间指数增长(2s→4s→…→封顶 5min),稳定运行 60s 后清零。
    * 内存高水位:超过 -MemWarnMB 只「告警 + 记日志」,默认不杀(避免打断用户正在做的事)。
      若你确实想让它在内存爆表时重启,加 -RestartOnMem。
#>
[CmdletBinding()]
param(
  [string]$ExePath,
  [int]$MemWarnMB = 6144,          # 内存高水位告警阈值 (MiB),默认 6GiB
  [switch]$RestartOnMem,           # 超阈值时是否重启(默认仅告警)
  [int]$MemRestartMB = 12288,      # 若开 -RestartOnMem,此阈值才真重启 (默认 12GiB)
  [string]$LogPath = "$env:LOCALAPPDATA\Polaris\supervisor.log",
  [switch]$Install,
  [switch]$Uninstall
)

$ErrorActionPreference = 'Stop'
$TaskName = 'PolarisSupervisor'

function Write-Log([string]$msg) {
  $line = "{0}  {1}" -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $msg
  Write-Host $line
  try {
    $dir = Split-Path -Parent $LogPath
    if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    Add-Content -Path $LogPath -Value $line -Encoding utf8
  } catch { }
}

function Resolve-ExePath {
  if ($ExePath -and (Test-Path $ExePath)) { return (Resolve-Path $ExePath).Path }
  # 常见安装位置 → 退回到本仓 dev 构建产物
  $candidates = @(
    "$env:LOCALAPPDATA\Polaris\Polaris.exe",
    "$env:ProgramFiles\Polaris\Polaris.exe",
    "${env:ProgramFiles(x86)}\Polaris\Polaris.exe",
    (Join-Path (Split-Path -Parent $PSScriptRoot) 'src-tauri\target\release\polaris-app.exe'),
    (Join-Path (Split-Path -Parent $PSScriptRoot) 'src-tauri\target\debug\polaris-app.exe')
  )
  foreach ($c in $candidates) { if ($c -and (Test-Path $c)) { return (Resolve-Path $c).Path } }
  return $null
}

if ($Install) {
  $self = $MyInvocation.MyCommand.Path
  $pwsh = (Get-Command pwsh -ErrorAction SilentlyContinue)?.Source
  if (-not $pwsh) { $pwsh = (Get-Command powershell).Source }
  $action = New-ScheduledTaskAction -Execute $pwsh -Argument "-WindowStyle Hidden -ExecutionPolicy Bypass -File `"$self`""
  $trigger = New-ScheduledTaskTrigger -AtLogOn
  $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1)
  Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger -Settings $settings -Force | Out-Null
  Write-Host "已注册计划任务 '$TaskName'(登录时自启)。日志:$LogPath"
  return
}
if ($Uninstall) {
  try { Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false; Write-Host "已取消 '$TaskName'。" }
  catch { Write-Host "未找到计划任务 '$TaskName'。" }
  return
}

$exe = Resolve-ExePath
if (-not $exe) {
  Write-Log "[fatal] 找不到 Polaris 可执行文件。请用 -ExePath 指定。"
  exit 1
}
$procName = [System.IO.Path]::GetFileNameWithoutExtension($exe)
Write-Log "[start] 监督者启动。目标=$exe  进程名=$procName  内存告警=${MemWarnMB}MiB  日志=$LogPath"

$backoff = 2
while ($true) {
  # 已有实例?只监督不重开(防多开)。
  $proc = Get-Process -Name $procName -ErrorAction SilentlyContinue | Select-Object -First 1
  if (-not $proc) {
    try {
      $proc = Start-Process -FilePath $exe -PassThru
      Write-Log "[launch] 已拉起 $procName (PID=$($proc.Id))"
    } catch {
      Write-Log "[error] 拉起失败: $_  ;${backoff}s 后重试"
      Start-Sleep -Seconds $backoff
      $backoff = [Math]::Min($backoff * 2, 300)
      continue
    }
  }

  $startedAt = Get-Date
  # 盯着这个进程,直到它退出;期间每 15s 抽查一次内存。
  while ($true) {
    Start-Sleep -Seconds 15
    $proc.Refresh()
    if ($proc.HasExited) {
      $lived = [int]((Get-Date) - $startedAt).TotalSeconds
      Write-Log "[exit] $procName 退出 (存活 ${lived}s, exitCode=$($proc.ExitCode))"
      if ($lived -ge 60) { $backoff = 2 }   # 稳定跑过 60s → 退避清零
      else {
        Write-Log "[crashloop] 启动后 ${lived}s 内即退出;${backoff}s 退避后再拉起"
        Start-Sleep -Seconds $backoff
        $backoff = [Math]::Min($backoff * 2, 300)
      }
      break
    }
    # 内存高水位:WorkingSet64(实际占用物理内存)
    $memMB = [int]($proc.WorkingSet64 / 1MB)
    if ($memMB -ge $MemWarnMB) {
      Write-Log "[mem] $procName 内存 ${memMB}MiB ≥ 告警阈值 ${MemWarnMB}MiB"
      if ($RestartOnMem -and $memMB -ge $MemRestartMB) {
        Write-Log "[mem-restart] ≥ ${MemRestartMB}MiB,按 -RestartOnMem 重启进程"
        try { $proc.CloseMainWindow() | Out-Null; Start-Sleep -Seconds 8; if (-not $proc.HasExited) { $proc.Kill() } } catch { }
      }
    }
  }
}
