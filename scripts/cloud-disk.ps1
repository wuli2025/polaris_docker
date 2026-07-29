# 云服务器硬盘 → 本机盘符（iroh 组网 + WebDAV 桥）
#
#   .\scripts\cloud-disk.ps1 start    挂上（后台常驻，关掉这个窗口也不掉）
#   .\scripts\cloud-disk.ps1 status   看现在挂没挂
#   .\scripts\cloud-disk.ps1 stop     卸载并退出常驻进程
#
# 盘之所以需要一个常驻进程托着：WebDAV 桥和 iroh 隧道都活在进程里，进程一退盘就成死挂载。

param([Parameter(Position = 0)][ValidateSet('start', 'stop', 'status')] [string]$Action = 'status')

$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
$Exe = Join-Path $Root 'src-tauri\target\release\examples\cloud_disk_keep.exe'
$MeshDir = Join-Path $env:LOCALAPPDATA 'Polaris\mesh'
$Log = Join-Path $MeshDir 'cloud-disk.log'
$PidFile = Join-Path $MeshDir 'cloud-disk.pid'
# 对端信息落**本机**配置,不进仓库 —— 这个仓是公开的,owner 令牌一旦推上去
# 就等于把那台机器的钥匙贴到网上(它还有共享目录写权限)。
$PeerFile = Join-Path $MeshDir 'peer.json'

# 从 peer.json 补齐环境变量(已经手工设过的 env 优先,方便临时连别的主机)。
#   {
#     "node":   "<NodeId>@<公网IP>:<UDP口>",   ← 带 @地址 这一段才会直连;
#                                                少了它会绕 n0 公共中继(实测德国),
#                                                每跳往返从 ~10ms 变 370ms。
#                                                端口须与云机 .env 的 POLARIS_IROH_PORT
#                                                和 compose 发布的 UDP 口三处一致。
#     "token":  "<对端 owner 令牌>",
#     "relays": "http://<自建中继>,https://<兜底中继>"  ← 打洞失败时才用得上,但默认那套
#                                                n0 公共中继在欧美,掉上去每跳几百毫秒。
#   }
function Import-Peer {
    if (-not (Test-Path $PeerFile)) {
        throw "缺少对端配置 $PeerFile。新建它并填 {`"node`":`"<NodeId>@IP:端口`",`"token`":`"<owner 令牌>`",`"relays`":`"<中继,逗号分隔>`"};也可以直接设 POLARIS_PROBE_NODE / POLARIS_PROBE_TOKEN / POLARIS_RELAYS 环境变量。"
    }
    $cfg = Get-Content $PeerFile -Raw | ConvertFrom-Json
    if (-not $env:POLARIS_PROBE_NODE -and $cfg.node) { $env:POLARIS_PROBE_NODE = $cfg.node }
    if (-not $env:POLARIS_PROBE_TOKEN -and $cfg.token) { $env:POLARIS_PROBE_TOKEN = $cfg.token }
    if (-not $env:POLARIS_RELAYS -and $cfg.relays) { $env:POLARIS_RELAYS = $cfg.relays }
}

function Get-Keeper {
    if (-not (Test-Path $PidFile)) { return $null }
    $id = (Get-Content $PidFile -Raw).Trim()
    try { Get-Process -Id $id -ErrorAction Stop } catch { $null }
}

switch ($Action) {
    'start' {
        if (Get-Keeper) { Write-Host '已经在跑了。'; break }
        if (-not (Test-Path $Exe)) {
            throw "还没编译：在 src-tauri 下跑 cargo build --release --example cloud_disk_keep"
        }
        New-Item -ItemType Directory -Force $MeshDir | Out-Null
        Import-Peer
        $p = Start-Process -FilePath $Exe -RedirectStandardOutput $Log `
            -RedirectStandardError "$Log.err" -WindowStyle Hidden -PassThru
        Set-Content $PidFile $p.Id
        Write-Host "已启动 (pid $($p.Id))，日志 $Log"
        Start-Sleep -Seconds 12
        Get-Content $Log -Tail 12
    }
    'stop' {
        $k = Get-Keeper
        if (-not $k) { Write-Host '没在跑。'; break }
        # 常驻进程收到 Ctrl+C 会先卸盘；后台进程收不到，直接杀掉后手工卸盘符。
        Stop-Process -Id $k.Id -Force
        Remove-Item $PidFile -ErrorAction SilentlyContinue
        Get-PSDrive -PSProvider FileSystem -ErrorAction SilentlyContinue |
            Where-Object { $_.DisplayRoot -like 'http://127.0.0.1:*' } |
            ForEach-Object { & net use "$($_.Name):" /delete /y | Out-Null }
        Write-Host '已停止并卸载。'
    }
    'status' {
        $k = Get-Keeper
        if ($k) { Write-Host "常驻进程在跑 (pid $($k.Id))" } else { Write-Host '常驻进程没在跑' }
        net use | Select-String '127.0.0.1'
        if (Test-Path $Log) { Write-Host '--- 日志尾 ---'; Get-Content $Log -Tail 8 }
    }
}
