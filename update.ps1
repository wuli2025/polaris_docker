# ─────────────────────────────────────────────────────────────
# Polaris Docker · 远程更新 (Windows / PowerShell)
#   拉取 GHCR 上最新预构建镜像并热替换容器。不本地重建、不动数据卷。
#   用法： ./update.ps1            （默认 latest=slim）
#          $env:POLARIS_TAG="full"; ./update.ps1   （full 口味：截图PPT/视频）
# ─────────────────────────────────────────────────────────────
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

if (-not $env:POLARIS_TAG) { $env:POLARIS_TAG = "latest" }
Write-Host "[polaris] 拉取最新镜像  ghcr.io/wuli2025/polaris:$($env:POLARIS_TAG) ..."
docker compose pull

Write-Host "[polaris] 重建容器（数据卷不动）..."
docker compose up -d

Write-Host "[polaris] 清理悬空旧镜像 ..."
docker image prune -f *> $null

Write-Host "[polaris] OK 已更新到最新镜像。打开 http://localhost:8080"
