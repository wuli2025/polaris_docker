# 项目检测默认脚本(Windows)。退出码 0=pass,非 0=fail;工具缺失=跳过该项。
# 环境变量:POLARIS_CHECK_DIR(worktree)、POLARIS_CHECK_PROFILE、POLARIS_TASK_ID。
$ErrorActionPreference = "Continue"
$dir = if ($env:POLARIS_CHECK_DIR) { $env:POLARIS_CHECK_DIR } else { (Get-Location).Path }
Set-Location $dir
$failed = $false

function Run-Check([string]$name, [string]$exe, [string[]]$checkArgs) {
    if (-not (Get-Command $exe -ErrorAction SilentlyContinue)) {
        Write-Output "[$name] 工具缺失($exe),跳过"
        return
    }
    Write-Output "[$name] 开始"
    & $exe @checkArgs 2>&1 | Out-String -Stream | Select-Object -Last 200 | Write-Output
    if ($LASTEXITCODE -ne 0) {
        Write-Output "[$name] 失败(exit=$LASTEXITCODE)"
        $script:failed = $true
    } else {
        Write-Output "[$name] 通过"
    }
}

if (Test-Path "Cargo.toml") {
    Run-Check "cargo check" "cargo" @("check", "--quiet")
}
if (Test-Path "package.json") {
    try { $pkg = Get-Content "package.json" -Raw | ConvertFrom-Json } catch { $pkg = $null }
    foreach ($s in @("lint", "typecheck", "build")) {
        if ($pkg -and $pkg.scripts -and $pkg.scripts.PSObject.Properties[$s]) {
            Run-Check "npm run $s" "npm" @("run", $s)
        }
    }
}
if ((Test-Path "pyproject.toml") -or (Test-Path "ruff.toml")) {
    Run-Check "ruff check" "ruff" @("check", ".")
}

if ($failed) { exit 1 } else { Write-Output "项目检测全部通过"; exit 0 }
