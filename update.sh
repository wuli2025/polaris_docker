#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────
# Polaris Docker · 远程更新
#   拉取 GHCR 上最新预构建镜像并热替换容器。不本地重建、不动数据卷。
#   数据卷 polaris-data / polaris-claude / polaris-config 原样保留。
#   用法： ./update.sh            （默认 latest=slim）
#          POLARIS_TAG=full ./update.sh   （full 口味：截图PPT/视频）
# ─────────────────────────────────────────────────────────────
set -euo pipefail
cd "$(dirname "$0")"

TAG="${POLARIS_TAG:-latest}"
echo "[polaris] 拉取最新镜像  ghcr.io/wuli2025/polaris:${TAG} ..."
POLARIS_TAG="$TAG" docker compose pull

echo "[polaris] 重建容器（数据卷不动）..."
POLARIS_TAG="$TAG" docker compose up -d

echo "[polaris] 清理悬空旧镜像 ..."
docker image prune -f >/dev/null 2>&1 || true

echo "[polaris] ✅ 已更新到最新镜像。打开 http://localhost:8080"
