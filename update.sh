#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────
# Polaris Docker · 远程更新（替身容器模式）
#
# 三种运行场景：
#   宿主机   ./update.sh                 经典路径：docker compose pull && up -d
#   容器内   do_docker_update 调用       ⚠️ 不能直接在被替换的容器里跑 up -d——
#            compose 客户端会随旧容器一起被杀，新容器永远起不来，服务直接挂掉。
#            改为：经 docker.sock 用「自己的镜像」起一个独立替身容器，由替身
#            执行 pull + up -d。替身不在被替换的容器里，替换不会波及它。
#   替身     update.sh --helper          cd 项目目录 → pull → up -d → 自毁(--rm)
#
# compose 项目目录/文件/项目名无需额外配置：docker compose 启动的容器自带
#   com.docker.compose.project{,.working_dir,.config_files} 标签，inspect 自己即得。
#   （要求 compose 文件在项目目录内——标准布局即满足。）
# 用法：  ./update.sh                     （默认 latest=slim）
#         POLARIS_TAG=full ./update.sh    （full 口味：截图PPT/视频）
# ─────────────────────────────────────────────────────────────
set -euo pipefail

TAG="${POLARIS_TAG:-latest}"
MODE="normal"
for arg in "$@"; do
  case "$arg" in
    --helper) MODE="helper" ;;
    *) ;;  # server.rs 会传 --non-interactive 等，本来就无交互，忽略
  esac
done

# ── 替身模式：在独立容器里完成真正的 pull + 重建 ──────────────────
if [ "$MODE" = "helper" ]; then
  cd "${POLARIS_COMPOSE_DIR:?替身容器缺 POLARIS_COMPOSE_DIR}"
  # COMPOSE_FILE / COMPOSE_PROJECT_NAME / POLARIS_TAG 由发起方经 env 传入
  echo "[polaris-helper] 拉取镜像 (tag=${TAG}) ..."
  docker compose pull
  echo "[polaris-helper] 重建容器（数据卷不动）..."
  docker compose up -d --no-build
  echo "[polaris-helper] 清理悬空旧镜像 ..."
  docker image prune -f >/dev/null 2>&1 || true
  echo "[polaris-helper] ✅ 完成"
  exit 0
fi

# ── 容器内：派出替身 ──────────────────────────────────────────────
if [ -f /.dockerenv ]; then
  command -v docker >/dev/null || { echo "[polaris] 镜像内缺 docker CLI（旧版镜像？），请先手动更新一次镜像" >&2; exit 1; }
  [ -S /var/run/docker.sock ] || { echo "[polaris] docker.sock 未挂载，无法自更新" >&2; exit 1; }

  CID="$(hostname)"   # compose 不设 hostname: 时，hostname = 容器短 ID
  WD="$(docker inspect -f '{{ index .Config.Labels "com.docker.compose.project.working_dir" }}' "$CID")"
  FILES="$(docker inspect -f '{{ index .Config.Labels "com.docker.compose.project.config_files" }}' "$CID")"
  PROJ="$(docker inspect -f '{{ index .Config.Labels "com.docker.compose.project" }}' "$CID")"
  SELF_IMAGE="$(docker inspect -f '{{ .Config.Image }}' "$CID")"
  if [ -z "$WD" ] || [ -z "$PROJ" ]; then
    echo "[polaris] 容器缺 compose 标签（不是 docker compose 启动的？），无法自更新" >&2
    exit 1
  fi

  echo "[polaris] 派出替身容器执行更新: project=${PROJ} dir=${WD} tag=${TAG}"
  docker run -d --rm \
    --name "polaris-updater-$$" \
    -v /var/run/docker.sock:/var/run/docker.sock \
    -v "${WD}:${WD}" \
    -e POLARIS_COMPOSE_DIR="${WD}" \
    -e COMPOSE_FILE="${FILES//,/:}" \
    -e COMPOSE_PROJECT_NAME="${PROJ}" \
    -e POLARIS_TAG="${TAG}" \
    --entrypoint /usr/local/bin/update.sh \
    "${SELF_IMAGE}" --helper
  echo "[polaris] ✅ 替身已出发。拉取完成后当前容器会被替换（取决于网速约 1~3 分钟），期间连接断开，稍后刷新页面即可。"
  exit 0
fi

# ── 宿主机经典路径 ────────────────────────────────────────────────
cd "$(dirname "$0")"
echo "[polaris] 拉取最新镜像 (tag=${TAG}) ..."
POLARIS_TAG="$TAG" docker compose pull
echo "[polaris] 重建容器（数据卷不动）..."
POLARIS_TAG="$TAG" docker compose up -d
echo "[polaris] 清理悬空旧镜像 ..."
docker image prune -f >/dev/null 2>&1 || true
echo "[polaris] ✅ 已更新到最新镜像。打开 http://localhost:8080"
