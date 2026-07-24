#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────
# Polaris Docker · 远程更新（替身容器模式）
#
# 两条更新源，按容器形态自动选择（也可用 POLARIS_UPDATE_SOURCE 强制）：
#   ① 镜像体（默认，NAS 走这条）——不依赖 compose、不碰 GHCR，国内可用：
#        从「镜像源列表」下载 docker save 的 tar → sha256 校验 → docker load →
#        按旧容器配置原样重建 → 健康检查 → 失败自动回滚。
#        纯 docker run 起的容器（群晖 Container Manager 图形界面装的就是）也能换。
#   ② GHCR + compose（容器带 compose 标签时）——经典路径，保留向后兼容。
#
# 镜像源（多备份，任一条活着就能更新；按序尝试，谁先给出完整包用谁）：
#   · Cloudflare  https://llmwiki.cloud/downloads/docker            （R2，分片）
#   · GitHub      .../polaris_docker/releases/latest/download       （Release 资产，整包）
#   · gh-proxy    国内加速 GitHub                                    （整包）
#   · ghfast      国内加速 GitHub 备份                                （整包）
#   覆盖：POLARIS_UPDATE_URL=<单一源，插到最前> / POLARIS_UPDATE_MIRRORS="<空格或逗号分隔>"
#
# 三种运行场景（两源共用）：
#   宿主机   ./update.sh                 直接在宿主跑
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
    --check)  MODE="check" ;;
    *) ;;  # server.rs 可能传 --non-interactive 等，无交互，忽略
  esac
done

log() { echo "[polaris] $*"; }
hlog() { echo "[polaris-helper] $*"; }

# ── 镜像源列表 ────────────────────────────────────────────────
# 顺序 = 优先级。单条源挂掉（域名被墙 / R2 抽风 / Release 还没传完）自动换下一条，
# 所有源共用同一份 manifest 语义，最终都靠 sha256 兜底，拿错包只会失败不会装坏。
GH_REPO="${POLARIS_GH_REPO:-wuli2025/polaris_docker}"
default_mirrors() {
  cat <<EOF
https://llmwiki.cloud/downloads/docker
https://github.com/${GH_REPO}/releases/latest/download
https://gh-proxy.com/https://github.com/${GH_REPO}/releases/latest/download
https://ghfast.top/https://github.com/${GH_REPO}/releases/latest/download
EOF
}
# 去重后逐行输出。POLARIS_UPDATE_URL 兼容老容器（install-r2.sh 一直在注入它）——
# 它不再是「开不开这条路」的开关，只是把某个源顶到最前。
mirror_list() {
  {
    [ -n "${POLARIS_UPDATE_URL:-}" ] && printf '%s\n' "$POLARIS_UPDATE_URL"
    [ -n "${POLARIS_UPDATE_MIRRORS:-}" ] && printf '%s\n' "$POLARIS_UPDATE_MIRRORS" | tr ' ,' '\n\n'
    default_mirrors
  } | sed 's#/*$##' | grep -v '^$' | awk '!seen[$0]++'
}

# 从扁平 manifest（KEY=VALUE，容器内无 jq 也能解析）安全取值。
mf_get() { printf '%s\n' "$MF" | grep -E "^$1=" | head -1 | cut -d= -f2- | tr -d '\r'; }

# 逐源取清单，第一个给出完整清单（有 sha256 行）的胜出 → 设 MF / MF_SRC。
MF=""; MF_SRC=""
resolve_manifest() {
  local m
  for m in $(mirror_list); do
    MF="$(curl -fsSL --retry 2 --retry-delay 2 --max-time 30 "$m/polaris-image-manifest.txt" 2>/dev/null || true)"
    if printf '%s' "$MF" | grep -q '^sha256='; then MF_SRC="$m"; return 0; fi
    MF=""
  done
  return 1
}

# 本容器当前版本：镜像里的 /app/VERSION（Dockerfile 从仓库根 VERSION 写入，
# 与 LABEL org.polaris.version 同源）。宿主机直跑时读不到就留空。
current_version() {
  [ -r /app/VERSION ] && tr -d ' \r\n' < /app/VERSION && return 0
  printf '%s' "${POLARIS_VERSION:-}"
}

# ── --check：只查不动，KEY=VALUE 吐给调用方（apihub.rs docker_check_update 解析）──
# 不需要 docker.sock —— 没挂 sock 的容器也能知道「有没有新版」，好把用户引到 SSH 兜底。
check_only() {
  local cur latest
  cur="$(current_version)"
  if ! resolve_manifest; then
    echo "ok=0"
    echo "current=$cur"
    echo "error=所有镜像源都拉不到更新清单（容器可能不通外网）"
    exit 0
  fi
  latest="$(mf_get version)"
  echo "ok=1"
  echo "current=$cur"
  echo "latest=$latest"
  echo "image=$(mf_get image)"
  echo "file=$(mf_get file)"
  echo "size=$(mf_get size)"
  echo "source=$MF_SRC"
  # has_update 只在「远端确实更新」时为真。用 != 判会把**回退**也报成有新版:
  # 客户端可能先命中一个还没同步到位的备用源(它的清单还停在旧版),那时不该催人升级。
  # sort -V 做版本序比较;万一环境没有(非 GNU coreutils)就退回「不同即有新版」。
  local newer=0 top
  if [ -n "$latest" ] && [ -n "$cur" ] && [ "$latest" != "$cur" ]; then
    top="$(printf '%s\n%s\n' "$cur" "$latest" | sort -V 2>/dev/null | tail -1)"
    if [ -z "$top" ] || [ "$top" = "$latest" ]; then newer=1; fi
  fi
  echo "has_update=$newer"
  exit 0
}

# 按「目标容器」当前配置重建（换镜像）。供替身复用。回滚也走它。
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

  # 环境变量：只带回**用户设的**键（POLARIS_*/ANTHROPIC_*/PUID/PGID/CLAUDE_CONFIG_DIR/TZ）。
  # ★ 先排除一批「镜像自带的 ENV」——它们由新镜像自己给。把旧值带回去等于把上一版的
  #   路径/版本号钉死在新容器上：将来某版把 /srv/web 挪个地方，新容器却还吃着旧的
  #   POLARIS_WEB_DIR，起不来(健康检查失败 → 自动回滚 → 用户看到「更新不上去」)。
  #   POLARIS_VERSION 同理,带回去会让容器自报一个假版本。
  while IFS= read -r line; do
    case "$line" in
      POLARIS_VERSION=*|POLARIS_WEB_DIR=*|POLARIS_RESOURCE_DIR=*|POLARIS_CHROMIUM=*\
      |POLARIS_CHROMIUM_HEADLESS_SHELL=*|POLARIS_FFMPEG=*|POLARIS_RENDER_FLAVOR=*) ;;
      POLARIS_*|ANTHROPIC_*|PUID=*|PGID=*|CLAUDE_CONFIG_DIR=*|TZ=*) args+=(-e "$line") ;;
    esac
  done < <(docker inspect -f '{{range .Config.Env}}{{println .}}{{end}}' "$target" 2>/dev/null)

  docker rm -f "$target" >/dev/null 2>&1 || true
  docker run "${args[@]}" "$image"
}

# 新容器健康探测（容器内 8080 自检）。成功返回 0。
# 窗口 180s：首次 PUID 归属化/冷启动索引恢复可能较慢，80s 会把好容器误判失败触发回滚。
wait_healthy() {
  local target="$1" i code
  for i in $(seq 1 90); do
    code="$(docker exec "$target" curl -fsS -o /dev/null -w '%{http_code}' http://127.0.0.1:8080/api/health 2>/dev/null || true)"
    [ "$code" = "200" ] && return 0
    sleep 2
  done
  return 1
}

# ── 单文件下载（稳健断点续传）─────────────────────────────────
# 先 HEAD 取远端大小：已完整的跳过、超长的（换源换来不同布局）删掉重下、半截的才 -C - 续。
# 直接对已下完的文件发 Range:bytes=<总长>- 会被 CDN 判 416/500，老逻辑一重跑就全片报错。
remote_len() { curl -fsSL --retry 3 --retry-delay 2 --max-time 30 -I "$1" 2>/dev/null \
  | tr -d '\r' | grep -i '^content-length:' | tail -1 | awk '{print $2}'; }
fetch_one() {
  local url="$1" out="$2" total cur
  total="$(remote_len "$url")" || true
  cur=0; [ -f "$out" ] && cur="$(wc -c < "$out" 2>/dev/null | tr -d ' ')"; cur="${cur:-0}"
  if [ -n "$total" ] && [ "$cur" = "$total" ]; then return 0; fi
  if [ -n "$total" ] && [ "$cur" -gt "$total" ] 2>/dev/null; then rm -f "$out"; fi
  curl -fsSL --retry 8 --retry-delay 3 -C - -o "$out" "$url"
}

# ── 从「一个源」把镜像体拿全 ───────────────────────────────────
# 先按 manifest 的分片布局拿（Cloudflare/R2 是分片的，多连接并行=聚合带宽）；
# 分片不存在（GitHub Release 那边是一个整包资产）就退回整包。
# 拿完当场 sha256 校验：过了才算这个源成功，没过就把残件清掉换下一个源。
try_mirror() {
  local base="$1" i p pids fail
  if [ "$PARTS" -gt 1 ]; then
    hlog "  ↳ 试分片（$PARTS 片并行 + 断点续传）"
    i=0; pids=""; fail=0
    while [ "$i" -lt "$PARTS" ]; do
      p="$(printf '%s.part%02d' "$FILE" "$i")"
      fetch_one "$base/$p" "$p" & pids="$pids $!"
      i=$((i+1))
    done
    for pid in $pids; do wait "$pid" || fail=1; done
    if [ "$fail" = 0 ]; then
      : > "$FILE"; i=0
      while [ "$i" -lt "$PARTS" ]; do
        p="$(printf '%s.part%02d' "$FILE" "$i")"; cat "$p" >> "$FILE"; rm -f "$p"; i=$((i+1))
      done
      if echo "$SHA  $FILE" | sha256sum -c - >/dev/null 2>&1; then return 0; fi
      hlog "  ↳ 分片拼出来 sha256 对不上，丢弃"
      rm -f "$FILE"
    else
      hlog "  ↳ 该源没有分片（或分片下载失败），改试整包"
      i=0
      while [ "$i" -lt "$PARTS" ]; do
        rm -f "$(printf '%s.part%02d' "$FILE" "$i")"; i=$((i+1))
      done
    fi
  fi
  hlog "  ↳ 试整包（断点续传）"
  if fetch_one "$base/$FILE" "$FILE"; then
    if echo "$SHA  $FILE" | sha256sum -c - >/dev/null 2>&1; then return 0; fi
    hlog "  ↳ 整包 sha256 对不上，丢弃"
  fi
  rm -f "$FILE"
  return 1
}

# ════════════════════════════════════════════════════════════════
# 镜像体替身：真正干活的地方（下载→校验→load→重建→健康检查→回滚）
# ════════════════════════════════════════════════════════════════
image_helper() {
  local target="${POLARIS_TARGET:-polaris-web}"
  local work="${POLARIS_UPD_WORK:-/tmp/polaris-upd}"
  mkdir -p "$work"; cd "$work"

  # ── 1) 取清单：逐源尝试，第一个给出完整清单的胜出 ──
  resolve_manifest || { hlog "❌ 所有镜像源都拿不到更新清单（检查 NAS 能不能上网）"; exit 1; }
  hlog "清单来自 $MF_SRC"

  local ver img cur
  ver="$(mf_get version)"; FILE="$(mf_get file)"; SHA="$(mf_get sha256)"; img="$(mf_get image)"
  PARTS="$(mf_get parts)"; PARTS="${PARTS:-1}"
  [ -n "$FILE" ] && [ -n "$SHA" ] && [ -n "$img" ] || { hlog "❌ manifest 不完整"; exit 1; }

  # ── 2) 版本比对：与目标容器当前镜像 label 比，相等且非强制则跳过（幂等、省带宽）──
  cur="$(docker inspect -f '{{index .Config.Labels "org.polaris.version"}}' "$target" 2>/dev/null || true)"
  if [ -n "$ver" ] && [ "$ver" = "$cur" ] && [ "${POLARIS_FORCE:-0}" != "1" ]; then
    hlog "✅ 已是最新 ($ver)，无需更新（POLARIS_FORCE=1 可强制）"
    exit 0
  fi
  hlog "目标版本 $ver（当前 ${cur:-未知}），镜像 $img，文件 $FILE"

  # ── 3) 下载镜像体：清单源优先，失败逐个换源；每个源内部「分片→整包」两试 ──
  local got=0 m
  for m in $(mirror_list); do
    hlog "从 $m 下载镜像体 …"
    if try_mirror "$m"; then got=1; hlog "✅ 镜像体已就位（源：$m，sha256 已校验）"; break; fi
    hlog "  ↳ 这个源没成，换下一个"
  done
  [ "$got" = 1 ] || { hlog "❌ 所有镜像源都没能拿到完整镜像体（运行容器未动）"; exit 1; }

  # 备份必须在 docker load **之前**、且用镜像 sha256 ID（.Image）而非名字（.Config.Image）：
  # load 会让新镜像顶掉同名 tag，之后再按名字备份，备份到的其实是刚 load 的新镜像——
  # 新版起不来时「回滚」就是原地复读坏镜像，服务保持宕机。
  hlog "备份旧镜像以便回滚 ..."
  local oldimg
  oldimg="$(docker inspect -f '{{.Image}}' "$target" 2>/dev/null || true)"
  [ -n "$oldimg" ] && docker tag "$oldimg" polaris-rollback:prev >/dev/null 2>&1 || true

  # docker save 的层已是压缩态,镜像体优先用「裸 tar」(不再 gzip,体积更小、省两端 CPU);
  # 仍兼容旧的 .tar.gz(gunzip 解)。按文件名后缀分流。
  hlog "docker load ..."
  case "$FILE" in
    *.gz) gunzip -c "$FILE" | docker load ;;
    *)    docker load -i "$FILE" ;;
  esac

  hlog "按旧容器配置重建 $target → $img ..."
  recreate_from "$target" "$img" >/dev/null

  hlog "健康检查新容器 ..."
  if wait_healthy "$target"; then
    hlog "✅ 已更新到 $ver ($img)"
    rm -f "$FILE"
    docker image prune -f >/dev/null 2>&1 || true
    exit 0
  fi

  hlog "❌ 新版本健康检查失败 → 回滚到旧镜像 ..."
  if [ -n "$oldimg" ]; then
    recreate_from "$target" "polaris-rollback:prev" >/dev/null
    if wait_healthy "$target"; then hlog "↩️ 已回滚，服务恢复（旧版本）"; else hlog "⚠️ 回滚后仍不健康，请人工介入"; fi
  fi
  rm -f "$FILE"
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

# ── 只查不动 ──────────────────────────────────────────────────────
if [ "$MODE" = "check" ]; then check_only; fi

# ── 替身入口 ──────────────────────────────────────────────────────
if [ "$MODE" = "helper" ]; then
  if [ "${POLARIS_UPDATE_SOURCE:-image}" = "compose" ]; then compose_helper; else image_helper; fi
  exit 0
fi

# ── 容器内：派出替身 ──────────────────────────────────────────────
if [ -f /.dockerenv ]; then
  command -v docker >/dev/null || { echo "[polaris] 镜像内缺 docker CLI（旧版镜像？），请先手动更新一次镜像" >&2; exit 1; }
  [ -S /var/run/docker.sock ] || { echo "[polaris] docker.sock 未挂载，无法自更新" >&2; exit 1; }

  CID="$(hostname)"  # 容器短 ID
  SELF_IMAGE="$(docker inspect -f '{{.Config.Image}}' "$CID")"
  SELF_NAME="$(docker inspect -f '{{.Name}}' "$CID" | sed 's#^/##')"
  WD="$(docker inspect -f '{{ index .Config.Labels "com.docker.compose.project.working_dir" }}' "$CID")"
  FILES="$(docker inspect -f '{{ index .Config.Labels "com.docker.compose.project.config_files" }}' "$CID")"
  PROJ="$(docker inspect -f '{{ index .Config.Labels "com.docker.compose.project" }}' "$CID")"

  # 源的选择：显式 POLARIS_UPDATE_SOURCE 说了算；否则「有 compose 标签就走 compose，
  # 其余一律走镜像体」。★ 老版本这里是「没设 POLARIS_UPDATE_URL 就必须是 compose，
  # 否则报错退出」—— 群晖 Container Manager 图形界面装的容器既非 compose 也没这个 env，
  # 网页点更新永远是这句错误，这就是「更新点了没反应/拉不动」的根因之一。
  SOURCE="${POLARIS_UPDATE_SOURCE:-}"
  if [ -z "$SOURCE" ]; then
    if [ -n "$PROJ" ] && [ -n "$WD" ]; then SOURCE="compose"; else SOURCE="image"; fi
  fi

  if [ "$SOURCE" = "image" ]; then
    log "派出镜像体替身：目标容器=$SELF_NAME 源=$(mirror_list | head -1)（失败自动换备用源）"
    docker run -d --rm \
      --name "polaris-updater-$$" \
      -v /var/run/docker.sock:/var/run/docker.sock \
      -e POLARIS_UPDATE_SOURCE=image \
      -e POLARIS_UPDATE_URL="${POLARIS_UPDATE_URL:-}" \
      -e POLARIS_UPDATE_MIRRORS="${POLARIS_UPDATE_MIRRORS:-}" \
      -e POLARIS_GH_REPO="$GH_REPO" \
      -e POLARIS_TARGET="$SELF_NAME" \
      -e POLARIS_TAG="$TAG" \
      -e POLARIS_FORCE="${POLARIS_FORCE:-0}" \
      --entrypoint /usr/local/bin/update.sh \
      "$SELF_IMAGE" --helper
    log "✅ 替身已出发。下载+校验+热替换完成后当前容器会被换掉（取决于网速约 1~5 分钟），期间连接断开，稍后刷新页面即可。"
    exit 0
  fi

  log "派出 compose 替身：project=${PROJ} dir=${WD} tag=${TAG}"
  docker run -d --rm \
    --name "polaris-updater-$$" \
    -v /var/run/docker.sock:/var/run/docker.sock \
    -v "${WD}:${WD}" \
    -e POLARIS_UPDATE_SOURCE=compose \
    -e POLARIS_COMPOSE_DIR="${WD}" \
    -e COMPOSE_FILE="${FILES//,/:}" \
    -e COMPOSE_PROJECT_NAME="${PROJ}" \
    -e POLARIS_TAG="${TAG}" \
    --entrypoint /usr/local/bin/update.sh \
    "${SELF_IMAGE}" --helper
  log "✅ 替身已出发，稍后刷新页面即可。"
  exit 0
fi

# ── 宿主机直跑 ────────────────────────────────────────────────────
# 默认走镜像体（和容器内一致）；要走老的 compose pull 显式 POLARIS_UPDATE_SOURCE=compose。
if [ "${POLARIS_UPDATE_SOURCE:-image}" = "compose" ]; then
  cd "$(dirname "$0")"
  POLARIS_TAG="$TAG" docker compose pull
  POLARIS_TAG="$TAG" docker compose up -d
  docker image prune -f >/dev/null 2>&1 || true
  log "✅ 已更新。打开 http://localhost:8080"
else
  POLARIS_TARGET="${POLARIS_TARGET:-polaris-web}" image_helper
fi
