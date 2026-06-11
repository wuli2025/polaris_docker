#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────
# Polaris Docker · 一键安装（拉安装包 → 起预构建镜像）
#   下载 compose + 脚本 + .env.example 到一个目录，不需要 clone 整个仓库。
#   用法： curl -fsSL https://llmwiki.cloud/docker/install.sh | bash
#          或  ./install.sh [目标目录]
# ─────────────────────────────────────────────────────────────
set -euo pipefail
BASE="${POLARIS_DOCKER_BASE:-https://llmwiki.cloud/docker}"
DIR="${1:-polaris-docker}"

echo "[polaris] 安装包来源：$BASE"
mkdir -p "$DIR"; cd "$DIR"
for f in docker-compose.yml docker-compose.build.yml .env.example update.sh DOCKER.md DEPLOY-SYNOLOGY.md; do
  echo "  ↓ $f"
  curl -fsSL "$BASE/$f" -o "$f"
done
chmod +x update.sh 2>/dev/null || true
[ -f .env ] || cp .env.example .env

cat <<'EOF'

[polaris] ✅ 安装包已就绪。下一步：
  1) 编辑 .env 填鉴权（ANTHROPIC_API_KEY 或 ANTHROPIC_BASE_URL+TOKEN；公网务必设 POLARIS_AUTH_TOKEN）
  2) docker login ghcr.io -u wuli2025   # 镜像私有,首次需登录(密码=GitHub PAT,勾 read:packages)
     #   没有访问权限？用 docker-compose.build.yml 从源码本地构建(见 DOCKER.md)
  3) docker compose up -d               # 拉取 GHCR 预构建镜像并启动
  4) 浏览器打开 http://localhost:8080
  以后远程更新： ./update.sh
EOF
