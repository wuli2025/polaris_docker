#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────
# Polaris Docker · 远程更新（替身容器模式）
#
# 两条更新源，按 env 自动选择：
#   ① R2 镜像体（POLARIS_UPDATE_URL 已设）——本机/NAS 拉不动 GHCR 私有镜像时用：
#        从 Cloudflare R2 下载 `docker save | zstd` 的镜像体 → sha256 校验 →
#        docker load → 按旧容器配置原样重建 → 健康检查 → 失败自动回滚。
#        不依赖 docker compose，纯 docker run 起的容器也能换（NAS 现状）。
#   ② GHCR + compose（POLARIS_UPDATE_URL 未设）——经典路径，保留向后兼容。
#
# 三种运行场景（两源共用）：
#   宿主机   ./update.sh                 直接在宿主跑（少用，一般 NAS 是容器内点更新）
#   容器内   server.rs docker_update     ⚠️ 不能在被替换的容器里直接重建——重建命令
#            会随旧容器一起被杀。改为经 docker.sock 派一个独立「替身」容器执行换装。
#   替身     update.sh --helper          下载/校验/load/重建/健康检查/回滚 → 自毁(--rm)
# ─────────────────────────────────────────────────────────────
set -euo pipefail

TAG="${POLARIS_TAG:-latest}"
MODE="normal"
for arg in "$@"; do
  case "$arg" in
    --helper) MODE="helper" ;;
    *) ;;  # server.rs 可能传 --non-interactive 等，无交互，忽略
  esac
done

log() { echo "[polaris] $*"; }
hlog() { echo "[polaris-helper] $*"; }

# 从扁平 manifest（KEY=VALUE，容器内无 jq 也能解析）安全取值。
mf_get() { printf '%s\n' "$MF" | grep -E "^$1=" | head -1 | cut -d= -f2-; }

# 按「目标容器」当前配置重建（换镜像）。供 R2 替身复用。回滚也走它。
#   $1 = 目标容器名   $2 = 要起的镜像引用
recreate_from() {
  local target="$1" image="$2"
  local binds ports restart mem shm name
  binds="$(docker inspect -f '{{range .HostConfig.Binds}}{{println .}}{{end}}' "$target" 2>/dev/null || true)"
  restart="$(docker inspect -f '{{.HostConfig.RestartPolicy.Name}}' "$target" 2>/dev/null || echo always)"
  mem="$(docker inspect -f '{{.HostConfig.Memory}}' "$target" 2>/dev/null || echo 0)"
  shm="$(docker inspect -f '{{.HostConfig.ShmSize}}' "$target" 2>/dev/null || echo 0)"

  local args=(-d --name "$target" --restart "${restart:-always}")
  [ "${mem:-0}" != "0" ] && args+=(--memory "$mem")
  [ "${shm:-0}" != "0" ] && args+=(--shm-size "$shm")

  # 端口：HostConfig.PortBindings → -p [ip:]hostport:containerport
  while IFS= read -r line; do
    [ -n "$line" ] && args+=(-p "$line")
  done < <(docker inspect -f '{{range $p,$c := .HostConfig.PortBindings}}{{range $c}}{{if .HostIp}}{{.HostIp}}:{{end}}{{.HostPort}}:{{$p}}{{println}}{{end}}{{end}}' "$target" 2>/dev/null | sed 's#/tcp##')

  # 卷绑定（含 /var/run/docker.sock，自动带回；逐行避免值含空格出错）
  while IFS= read -r line; do
    [ -n "$line" ] && args+=(-v "$line")
  done <<< "$binds"

  # 环境变量：只带回用户/运行相关键（POLARIS_*/ANTHROPIC_*/PUID/PGID/CLAUDE_CONFIG_DIR/TZ），
  # 其余镜像默认 ENV 由新镜像自带，不重复注入。整行读取，保留含特殊字符的值。
  while IFS= read -r line; do
    case "$line" in
      POLARIS_*|ANTHROPIC_*|PUID=*|PGID=*|CLAUDE_CONFIG_DIR=*|TZ=*) args+=(-e "$line") ;;
    esac
  done < <(docker inspect -f '{{range .Config.Env}}{{println .}}{{end}}' "$target" 2>/dev/null)

  docker rm -f "$target" >/dev/null 2>&1 || true
  docker run "${args[@]}" "$image"
}

# 新容器健康探测（容器内 8080 自检）。成功返回 0。
wait_healthy() {
  local target="$1" i code
  for i in $(seq 1 40); do
    code="$(docker exec "$target" curl -fsS -o /dev/null -w '%{http_code}' http://127.0.0.1:8080/api/health 2>/dev/null || true)"
    [ "$code" = "200" ] && return 0
    sleep 2
  done
  return 1
}

# ════════════════════════════════════════════════════════════════
# R2 替身：真正干活的地方（下载→校验→load→重建→健康检查→回滚）
# ════════════════════════════════════════════════════════════════
r2_helper() {
  local target="${POLARIS_TARGET:-polaris-web}"
  local base="${POLARIS_UPDATE_URL:?替身缺 POLARIS_UPDATE_URL}"
  local work="${POLARIS_UPD_WORK:-/tmp/polaris-upd}"
  mkdir -p "$work"; cd "$work"

  hlog "拉取更新清单 $base/polaris-image-manifest.txt"
  MF="$(curl -fsSL --retry 5 --retry-delay 2 --max-time 60 "$base/polaris-image-manifest.txt")"
  local ver file sha img cur
  ver="$(mf_get version)"; file="$(mf_get file)"; sha="$(mf_get sha256)"; img="$(mf_get image)"
  [ -n "$file" ] && [ -n "$sha" ] && [ -n "$img" ] || { hlog "❌ manifest 不完整"; exit 1; }

  # 版本比对：与目标容器当前镜像 label 比，相等且非强制则跳过（幂等、省带宽）。
  cur="$(docker inspect -f '{{index .Config.Labels "org.polaris.version"}}' "$target" 2>/dev/null || true)"
  if [ -n "$ver" ] && [ "$ver" = "$cur" ] && [ "${POLARIS_FORCE:-0}" != "1" ]; then
    hlog "✅ 已是最新 ($ver)，无需更新（POLARIS_FORCE=1 可强制）"
    exit 0
  fi
  hlog "目标版本 $ver（当前 ${cur:-未知}），镜像 $img，文件 $file"

  local parts; parts="$(mf_get parts)"; parts="${parts:-1}"
  if [ "${parts:-1}" -gt 1 ]; then
    # 并行下载所有分片(各自断点续传)→ 聚合带宽,比顺序单连接快数倍;全成才拼接。
    hlog "下载镜像体（$parts 个分片，并行 + 断点续传）..."
    local i=0 p pids=""
    while [ "$i" -lt "$parts" ]; do
      p="$(printf '%s.part%02d' "$file" "$i")"
      curl -fsSL --retry 8 --retry-delay 3 -C - -o "$p" "$base/$p" &
      pids="$pids $!"
      i=$((i+1))
    done
    local fail=0 pid
    for pid in $pids; do wait "$pid" || fail=1; done
    [ "$fail" = 0 ] || { hlog "❌ 分片下载失败（运行容器未动）"; exit 1; }
    : > "$file"; i=0
    while [ "$i" -lt "$parts" ]; do
      p="$(printf '%s.part%02d' "$file" "$i")"
      cat "$p" >> "$file"; rm -f "$p"
      i=$((i+1))
    done
  else
    hlog "下载镜像体（断点续传）..."
    curl -fsSL --retry 8 --retry-delay 3 -C - -o "$file" "$base/$file"
  fi

  hlog "校验 sha256 ..."
  echo "$sha  $file" | sha256sum -c - || { hlog "❌ sha256 不匹配，已中止（运行容器未动）"; rm -f "$file"; exit 1; }

  # docker save 的层已是压缩态,镜像体优先用「裸 tar」(不再 gzip,体积更小、省两端 CPU);
  # 仍兼容旧的 .tar.gz(gunzip 解)。按文件名后缀分流。
  hlog "docker load ..."
  case "$file" in
    *.gz) gunzip -c "$file" | docker load ;;
    *)    docker load -i "$file" ;;
  esac

  hlog "备份旧镜像以便回滚 ..."
  local oldimg
  oldimg="$(docker inspect -f '{{.Config.Image}}' "$target" 2>/dev/null || true)"
  [ -n "$oldimg" ] && docker tag "$oldimg" polaris-rollback:prev >/dev/null 2>&1 || true

  hlog "按旧容器配置重建 $target → $img ..."
  recreate_from "$target" "$img" >/dev/null

  hlog "健康检查新容器 ..."
  if wait_healthy "$target"; then
    hlog "✅ 已更新到 $ver ($img)"
    rm -f "$file"
    docker image prune -f >/dev/null 2>&1 || true
    exit 0
  fi

  hlog "❌ 新版本健康检查失败 → 回滚到旧镜像 ..."
  if [ -n "$oldimg" ]; then
    recreate_from "$target" "polaris-rollback:prev" >/dev/null
    if wait_healthy "$target"; then hlog "↩️ 已回滚，服务恢复（旧版本）"; else hlog "⚠️ 回滚后仍不健康，请人工介入"; fi
  fi
  rm -f "$file"
  exit 1
}

# ════════════════════════════════════════════════════════════════
# compose 替身（GHCR 经典路径，向后兼容）
# ════════════════════════════════════════════════════════════════
compose_helper() {
  cd "${POLARIS_COMPOSE_DIR:?替身容器缺 POLARIS_COMPOSE_DIR}"
  hlog "拉取镜像 (tag=${TAG}) ..."
  docker compose pull
  hlog "重建容器（数据卷不动）..."
  docker compose up -d --no-build
  docker image prune -f >/dev/null 2>&1 || true
  hlog "✅ 完成"
}

# ── 替身入口 ──────────────────────────────────────────────────────
if [ "$MODE" = "helper" ]; then
  if [ -n "${POLARIS_UPDATE_URL:-}" ]; then r2_helper; else compose_helper; fi
  exit 0
fi

# ── 容器内：派出替身 ──────────────────────────────────────────────
if [ -f /.dockerenv ]; then
  command -v docker >/dev/null || { echo "[polaris] 镜像内缺 docker CLI（旧版镜像？），请先手动更新一次镜像" >&2; exit 1; }
  [ -S /var/run/docker.sock ] || { echo "[polaris] docker.sock 未挂载，无法自更新" >&2; exit 1; }

  CID="$(hostname)"  # 容器短 ID
  SELF_IMAGE="$(docker inspect -f '{{.Config.Image}}' "$CID")"
  SELF_NAME="$(docker inspect -f '{{.Name}}' "$CID" | sed 's#^/##')"

  # ── R2 源：派 R2 替身 ──
  if [ -n "${POLARIS_UPDATE_URL:-}" ]; then
    log "派出 R2 替身：源=$POLARIS_UPDATE_URL 目标容器=$SELF_NAME"
    docker run -d --rm \
      --name "polaris-updater-$$" \
      -v /var/run/docker.sock:/var/run/docker.sock \
      -e POLARIS_UPDATE_URL="$POLARIS_UPDATE_URL" \
      -e POLARIS_TARGET="$SELF_NAME" \
      -e POLARIS_TAG="$TAG" \
      -e POLARIS_FORCE="${POLARIS_FORCE:-0}" \
      --entrypoint /usr/local/bin/update.sh \
      "$SELF_IMAGE" --helper
    log "✅ R2 替身已出发。下载+校验+热替换完成后当前容器会被换掉（取决于网速约 1~5 分钟），期间连接断开，稍后刷新页面即可。"
    exit 0
  fi

  # ── compose 源：派 compose 替身（需 compose 标签）──
  WD="$(docker inspect -f '{{ index .Config.Labels "com.docker.compose.project.working_dir" }}' "$CID")"
  FILES="$(docker inspect -f '{{ index .Config.Labels "com.docker.compose.project.config_files" }}' "$CID")"
  PROJ="$(docker inspect -f '{{ index .Config.Labels "com.docker.compose.project" }}' "$CID")"
  if [ -z "$WD" ] || [ -z "$PROJ" ]; then
    echo "[polaris] 非 compose 启动且未设 POLARIS_UPDATE_URL，无法自更新（请设 R2 更新源）" >&2
    exit 1
  fi
  log "派出 compose 替身：project=${PROJ} dir=${WD} tag=${TAG}"
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
  log "✅ 替身已出发，稍后刷新页面即可。"
  exit 0
fi

# ── 宿主机直跑（少用）────────────────────────────────────────────
if [ -n "${POLARIS_UPDATE_URL:-}" ]; then
  POLARIS_TARGET="${POLARIS_TARGET:-polaris-web}" r2_helper
else
  cd "$(dirname "$0")"
  POLARIS_TAG="$TAG" docker compose pull
  POLARIS_TAG="$TAG" docker compose up -d
  docker image prune -f >/dev/null 2>&1 || true
  log "✅ 已更新。打开 http://localhost:8080"
fi
