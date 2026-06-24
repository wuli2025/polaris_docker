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

# 本地模型 seed：同飞书桥思路——命名卷/bind mount 会盖掉镜像里 /root/Polaris/models,
# 故构建期把模型预烤在 /opt/polaris-models,这里在卷挂好后补进卷内。容器首启即离线可用,
# 不再触发运行时联网下载(NAS 出网受限常失败)。缺源目录(slim 未预烤)则静默跳过。
# 预烤的是 fastembed(BGE-M3 嵌入 + bge-reranker-v2-m3 重排);ASR 包暂未预装,留作后续。
seed_local_models() {
  SRC=/opt/polaris-models
  DST=/root/Polaris/models
  [ -d "$SRC" ] || return 0
  for pack in fastembed sensevoice-small paraformer-zh; do
    if [ -d "$SRC/$pack" ] && [ ! -d "$DST/$pack" ]; then
      mkdir -p "$DST"
      cp -a "$SRC/$pack" "$DST/"
      echo "[entrypoint] 已 seed 本地模型 $pack → $DST/$pack"
    fi
  done
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
  seed_local_models
  # HOME(/root) 及数据目录归属运行用户，确保 claude 配置/缓存可写。
  chown "$PUID:$PGID" /root 2>/dev/null || true
  for d in $DATA_DIRS; do chown -R "$PUID:$PGID" "$d" 2>/dev/null || true; done

  # ── docker.sock 自更新支持（Web UI「一键更新」所需）─────────────────
  # 群晖非 root 跑时，宿主 sock 属主是 root:<某GID>(DSM 默认 root:root)，降权后的
  # polaris 用户不在该组 → 调 sock 报 permission denied。挂了 sock 就探测其 GID，
  # 建同号组并把 polaris 加进去；再用「只给 UID」的 gosu 带上附属组。
  # 仅在显式挂了 sock 时生效——没挂 sock 的部署完全不受影响，零额外授权。
  if [ -S /var/run/docker.sock ]; then
    SOCK_GID="$(stat -c '%g' /var/run/docker.sock 2>/dev/null || echo '')"
    if [ -n "$SOCK_GID" ] && [ "$SOCK_GID" != "$PGID" ]; then
      if ! getent group "$SOCK_GID" >/dev/null 2>&1; then
        groupadd -g "$SOCK_GID" dockersock 2>/dev/null \
          || addgroup --gid "$SOCK_GID" dockersock 2>/dev/null || true
      fi
      SOCK_GRP="$(getent group "$SOCK_GID" | cut -d: -f1)"
      usermod -aG "$SOCK_GID" polaris 2>/dev/null \
        || adduser polaris "${SOCK_GRP:-$SOCK_GID}" 2>/dev/null || true
      echo "[entrypoint] 已把 polaris 加入 docker.sock 组 (GID=$SOCK_GID '${SOCK_GRP:-?}')，容器内一键更新可用"
    fi
  fi

  echo "[entrypoint] 以非 root 运行 UID=$PUID GID=$PGID"
  # 用「仅 UID」形式 → gosu 会带上 polaris 的全部附属组(含上面加的 sock 组)；
  # 若写成 "$PUID:$PGID" 则 gosu 只设这一个 gid、丢掉附属组，sock 又会 permission denied。
  exec gosu "$PUID" polaris-server "$@"
fi

# ── 默认：root 模式（未设 PUID/PGID，与既有行为一致）─────────────
ensure_dirs
seed_feishu_bridge
seed_local_models
echo "[entrypoint] 以 root 运行（未设 PUID/PGID）"
exec polaris-server "$@"
