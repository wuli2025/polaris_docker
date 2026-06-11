# ─────────────────────────────────────────────────────────────
# Polaris Docker · 一键安装 (Windows / PowerShell)
#   用法： irm https://llmwiki.cloud/docker/install.ps1 | iex
#          或  ./install.ps1 [目标目录]
# ─────────────────────────────────────────────────────────────
param([string]$Dir = "polaris-docker")
$ErrorActionPreference = "Stop"
$base = if ($env:POLARIS_DOCKER_BASE) { $env:POLARIS_DOCKER_BASE } else { "https://llmwiki.cloud/docker" }

Write-Host "[polaris] 安装包来源：$base"
New-Item -ItemType Directory -Force -Path $Dir | Out-Null
Set-Location $Dir
foreach ($f in @("docker-compose.yml","docker-compose.build.yml",".env.example","update.ps1","DOCKER.md","DEPLOY-SYNOLOGY.md")) {
  Write-Host "  v $f"
  Invoke-WebRequest -UseBasicParsing "$base/$f" -OutFile $f
}
if (-not (Test-Path ".env")) { Copy-Item ".env.example" ".env" }

Write-Host ""
Write-Host "[polaris] OK 安装包已就绪。下一步："
Write-Host "  1) 编辑 .env 填鉴权（ANTHROPIC_* 或 POLARIS_AUTH_TOKEN）"
Write-Host "  2) docker login ghcr.io -u wuli2025   # 镜像私有,首次需登录(密码=GitHub PAT,勾 read:packages)"
Write-Host "  3) docker compose up -d               # 拉取 GHCR 预构建镜像并启动"
Write-Host "  4) 浏览器打开 http://localhost:8080"
Write-Host "  以后远程更新： ./update.ps1"
