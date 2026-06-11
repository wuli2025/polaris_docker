#!/bin/sh
# Polaris 容器入口 —— tini 之下运行；按需把权限降到非 root（群晖 PUID/PGID 场景）。
#   · 不设 PUID/PGID  → 以 root 运行（与既有开发机行为完全一致，零影响）
#   · 设了 PUID/PGID  → 建同号用户、把数据目录 chown 给它、用 gosu 降权运行
# 这样群晖共享文件夹里产生的文件属主与宿主一致，宿主侧可正常管理，且不再以 root 跑容器。
set -e

# 容器内固定数据路径（host 侧可为命名卷或 /volume1 bind mount）。
DATA_DIRS="/root/Polaris /root/.claude /root/.config"

ensure_dirs() {
  for d in $DATA_DIRS; do mkdir -p "$d"; done
}

# 飞书桥 SDK seed：命名卷会盖掉镜像里 /root/Polaris 下的内容,故构建期把 SDK 预装在
# /opt/feishu-bridge,这里在卷挂好后补进卷内。免去容器首启联网 npm install(NAS 容器出网受限常失败)。
# 判据对齐 Rust 侧 ensure_bridge(查 @larksuiteoapi 而非裸 node_modules):
# 早前容器内 npm install 失败可能留下残缺 node_modules,裸目录判据会永远跳过 seed。
seed_feishu_bridge() {
  BRIDGE=/root/Polaris/feishu-bridge
  if [ -d /opt/feishu-bridge/node_modules ] && [ ! -d "$BRIDGE/node_modules/@larksuiteoapi" ]; then
    mkdir -p "$BRIDGE"
    cp -a /opt/feishu-bridge/. "$BRIDGE/"
    echo "[entrypoint] 已 seed 飞书桥 SDK → $BRIDGE"
  fi
}

if [ -n "$PUID" ] && [ -n "$PGID" ]; then
  # ── 非 root 模式（群晖推荐）──────────────────────────────────
  if ! getent group "$PGID" >/dev/null 2>&1; then
    groupadd -g "$PGID" polaris 2>/dev/null || addgroup --gid "$PGID" polaris 2>/dev/null || true
  fi
  if ! getent passwd "$PUID" >/dev/null 2>&1; then
    useradd -u "$PUID" -g "$PGID" -d /root -M polaris 2>/dev/null \
      || adduser --uid "$PUID" --gid "$PGID" --home /root --disabled-password --gecos "" polaris 2>/dev/null || true
  fi
  ensure_dirs
  seed_feishu_bridge
  # HOME(/root) 及数据目录归属运行用户，确保 claude 配置/缓存可写。
  chown "$PUID:$PGID" /root 2>/dev/null || true
  for d in $DATA_DIRS; do chown -R "$PUID:$PGID" "$d" 2>/dev/null || true; done
  echo "[entrypoint] 以非 root 运行 UID=$PUID GID=$PGID"
  exec gosu "$PUID:$PGID" polaris-server "$@"
fi

# ── 默认：root 模式（未设 PUID/PGID，与既有行为一致）─────────────
ensure_dirs
seed_feishu_bridge
echo "[entrypoint] 以 root 运行（未设 PUID/PGID）"
exec polaris-server "$@"
