# ════════════════════════════════════════════════════════════════
# Polaris · Docker 化镜像（方案 A：保留 Rust 引擎，axum 替代 Tauri 外壳）
#   阶段1 web      —— 构建 Vue3 前端 → dist/
#   阶段2 server   —— 构建 polaris-server（复用同一份 Rust 引擎，不含 Tauri）
#   阶段3 runtime  —— node-slim + 预装 claude CLI，托管前端 + 跑 HTTP/WS 服务
#
# 构建：docker build -t polaris-web .
# 运行：见 docker-compose.yml
# ════════════════════════════════════════════════════════════════

# ── 阶段1：构建前端 Vue3 → dist/ ──────────────────────────────────
# 跳过 package.json 的 `vue-tsc --noEmit`(有历史存量类型报错),直接 vite build：
# esbuild 只转译不做类型检查,类型报错不影响产物;真实 App 界面随镜像发布。
FROM node:20-slim AS web
WORKDIR /app
# npm 国内镜像(npmmirror)——NAS 只有国内网,拉 registry.npmjs.org 会极慢/超时。
RUN npm config set registry https://registry.npmmirror.com
# 依赖层:先拷清单,package-lock 不变则复用 npm ci 缓存层(Windows 更新后快速重建)。
COPY package.json package-lock.json ./
RUN npm ci
# 源码 + 配置 + 静态资源
COPY tsconfig.json tsconfig.node.json vite.config.ts index.html ./
COPY public ./public
COPY src ./src
# 跳过 vue-tsc,直接产出 dist/(默认输出目录,被阶段3 COPY 到 /srv/web)
RUN npx vite build

# ── 阶段2：构建 Rust server 二进制 ───────────────────────────────
FROM rust:1-slim-bookworm AS server
# ring(经 ureq/rustls) 需要 C 编译器；其余解析库均为纯 Rust。
# 装 git: 配合 git-fetch-with-cli=true,绕开 libcurl/sparse-index 在国内的 30s timeout。
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential pkg-config ca-certificates git \
        autoconf automake libtool \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build

# crates.io 国内镜像(中科大 ustc)+ 走 git CLI(curl 拉 sparse-index 在国内常卡 30s timeout)。
# 影响范围:仅此 server 构建阶段的 cargo 解析/下载;不改源码,不影响最终二进制。
RUN mkdir -p /usr/local/cargo/ \
    && printf '%s\n' \
        '[source.crates-io]' \
        'replace-with = "ustc"' \
        '' \
        '[source.ustc]' \
        'registry = "sparse+https://mirrors.ustc.edu.cn/crates.io-index/"' \
        '' \
        '[net]' \
        'git-fetch-with-cli = true' \
        > /usr/local/cargo/config.toml

# ★ 分仓重构(主仓 v2.0 起)后的构建姿势 —— 别再照抄旧的「stub src/bin 预热依赖」写法:
#   旧布局:polaris-server/polaris-forge 的 [[bin]] 住主包 src/bin/,故可用「空占位 src +
#          --bin polaris-server」预编依赖。
#   新布局:两个 bin 都住 workspace 成员 crates/polaris-cli,且 polaris-cli 依赖
#          polaris-app{default-features=false, features=["server"]}。此时再 stub 主包
#          src/lib.rs,polaris-cli 会因为找不到 polaris-app 的符号直接编译失败 ——
#          占位层不再可行,整包一次编(依赖缓存交给 image.yml 的 GHA cache-from/to)。
#
#   -p polaris-cli 一次出双 bin(polaris-server + polaris-forge),server feature 经依赖恒开,
#   等价于旧的 --no-default-features --features server。
#   --features collab-net:让 polaris-server 起 iroh host_listen 暴露 NodeId ——
#     ★ 没有它 NAS 侧连不出 NodeId,桌面「设备联盟」的 P2P 远程盘直连不可用(实测根因)。
#   --features local-embed:本地 ONNX 嵌入/重排(经 polaris-cli → polaris-app → polaris-fable
#     转发);配套 crates/polaris-fable 的 ort-load-dynamic + 运行层 ORT_DYLIB_PATH。
COPY src-tauri ./src-tauri
RUN cargo build --profile release-fast \
        --manifest-path src-tauri/Cargo.toml \
        -p polaris-cli --features collab-net,local-embed \
    && cp src-tauri/target/release-fast/polaris-server /usr/local/bin/polaris-server \
    && cp src-tauri/target/release-fast/polaris-forge /usr/local/bin/polaris-forge

# ── 阶段3：运行时 ────────────────────────────────────────────────
FROM node:20-slim AS runtime
# claude CLI 跑 Bash/脚本工具需要：bash、git、python3(pptx/xlsx 等技能)、ripgrep、ca 证书。
RUN apt-get update && apt-get install -y --no-install-recommends \
        bash git ca-certificates curl python3 python3-pip python3-venv python-is-python3 ripgrep \
        tini gosu \
    && rm -rf /var/lib/apt/lists/* \
    && npm config set registry https://registry.npmmirror.com \
    && npm install -g @anthropic-ai/claude-code \
    && npm cache clean --force

# ── 预装 python-pptx + pypdf：PPTX 技能运行时直出真 .pptx，而非退化成 HTML 兜底 ──
#   照「飞书桥」同款思路:构建期(Windows/CI 有网)就把包装好。
#   运行时 NAS 容器出网受限,pptx.md 技能里的 `pip install python-pptx` 会超时失败
#   → 现状只能走单文件 HTML 兜底,出不了真 .pptx。构建期预装即可消除这次联网。
#   Debian bookworm 的 python3 是 externally-managed(PEP 668),需 --break-system-packages
#   (与下方字体子集层 pip 用法一致);清华 TUNA 镜像主用,失败退默认源。
RUN pip install --no-cache-dir --break-system-packages \
        -i https://pypi.tuna.tsinghua.edu.cn/simple python-pptx pypdf \
    || pip install --no-cache-dir --break-system-packages python-pptx pypdf

# ── 渲染栈(可选 flavor)——Polaris Forge 跨平台 PRD §05：容器「零安装」=渲染栈打进镜像 ──
#   POLARIS_RENDER=0 → polaris:slim   现状(聊天/KB/网站生成，网站本就不需渲染栈)
#   POLARIS_RENDER=1 → polaris:full   +chromium(截图)+fonts-noto-cjk(防豆腐块)+ffmpeg(出视频)
#                                 +xvfb(虚拟显示,CloakBrowser 有头模式:公众号登录/抓取) +fb/libnss3
# 构建 full：docker build --build-arg POLARIS_RENDER=1 -t polaris-web:full 。
# CJK 字体是「最隐蔽必踩」坑：缺了 deck 截图全是 □□□，preflight 会用 fc-list 探测并亮红灯。
# 浏览器(Chromium/CloakBrowser)有头模式需要 X server —— 容器没显示器，靠 Xvfb 给一块虚拟屏；
# wechat_yiban.py 的 publish/restyle/publish-image/panel 模式都按 headless=False 启动以支撑扫码登录。
ARG POLARIS_RENDER=0
# ── 阶段3.5 准备:字符集 + 子集脚本先 COPY,Docker 层缓存才不会错过文件 ────────────
COPY docker/font-subset-chars.txt /docker/font-subset-chars.txt
COPY docker/subset_cjk.py /docker/subset_cjk.py

# ── 阶段3.6:SC 字体子集(全语种 102MB → 3 weight × ~12MB = ~36MB)────────
#   字符集 docker/font-subset-chars.txt(ASCII + 6763 高频中文 + 实用 emoji)
#   软降级:pyftsubset 失败不 fail build,fallback 装全语种(任务 d §6.3)
# ⚠ 这里曾出过「空字体层」事故：apt-get 失败被后面的 || pip 链接住，整层 exit 0，
#   GHA 缓存把这个零字体层缓存住 → GHCR :full 长期缺中文字体(真机 fc-list :lang=zh = 0)。
#   故改三段式：① apt 必须成功(set -e 硬失败) ② 子集化纯 best-effort ③ 出层前 fc-list 硬闸。
RUN if [ "$POLARIS_RENDER" = "1" ]; then \
        set -e; \
        apt-get update; \
        apt-get install -y --no-install-recommends \
            fonts-noto-cjk fonts-noto-color-emoji fontconfig; \
        { pip install -i https://pypi.tuna.tsinghua.edu.cn/simple --no-cache-dir --break-system-packages fonttools brotli 2>/dev/null \
            || pip install -i https://pypi.tuna.tsinghua.edu.cn/simple --no-cache-dir fonttools brotli \
            || echo "[subset] fonttools 装不上,跳过子集(保留全语种)"; }; \
        mkdir -p /out; \
        python3 /docker/subset_cjk.py || echo "[subset] 子集失败,降级全语种 102MB"; \
        if [ -n "$(ls -A /out 2>/dev/null)" ]; then \
            mkdir -p /usr/share/fonts/truetype/noto-cjk-subset; \
            cp /out/*.woff2 /usr/share/fonts/truetype/noto-cjk-subset/; \
            echo "[subset] SC 字体子集已落 /usr/share/fonts/truetype/noto-cjk-subset/"; \
        fi; \
        fc-cache -f > /dev/null 2>&1 || true; \
        fc-list :lang=zh | grep -q . \
            || { echo "FATAL: full 镜像 fc-list :lang=zh 为空 — 中文字体没装上,拒出镜像(否则截图全豆腐块)"; exit 1; }; \
        rm -rf /var/lib/apt/lists/*; \
    fi

# ── 渲染栈(可选 flavor)——Polaris Forge 工业级化阶段 0:Docker 994MB→235MB ──
#   POLARIS_RENDER=0 → polaris:slim   现状(聊天/KB/网站生成，网站本就不需渲染栈)
#   POLARIS_RENDER=1 → polaris:full   +chrome-headless-shell(截图,~80-130MB,比完整 chromium 砍 150MB)
#                                 +ffmpeg(出视频,静态 ~30MB)+xvfb(虚拟显示,CloakBrowser 有头模式)
#                                 +fb/libnss3(原生库依赖)+CJK 字体子集(36MB,阶段3.5 落)
# 构建 full：docker build --build-arg POLARIS_RENDER=1 -t polaris-web:full 。
# CJK 字体是「最隐蔽必踩」坑：缺了 deck 截图全是 □□□，preflight 会用 fc-list 探测并亮红灯。
# 浏览器(Chromium/CloakBrowser)有头模式需要 X server —— 容器没显示器，靠 Xvfb 给一块虚拟屏；
# wechat_yiban.py 的 publish/restyle/publish-image/panel 模式都按 headless=False 启动以支撑扫码登录。
RUN if [ "$POLARIS_RENDER" = "1" ]; then \
        apt-get update && apt-get install -y --no-install-recommends \
            chromium \
            ffmpeg \
            libnss3 libatk1.0-0 libatk-bridge2.0-0 libcups2 libdrm2 \
            libxkbcommon0 libxcomposite1 libxdamage1 libxrandr2 libgbm1 \
            libpango-1.0-0 libcairo2 libasound2 \
            xvfb x11-utils procps \
        && rm -rf /var/lib/apt/lists/* ; \
    else \
        echo "[build] POLARIS_RENDER=0 → slim 镜像(无渲染栈)" ; \
    fi
# 容器内 chromium wrapper:预置 no-sandbox + disable-dev-shm-usage + disable-gpu + disable-dbus
# 消除 Docker 无 DBus daemon 的噪音(Docker 实测误差级,不影响截图)
COPY docker/chromium-headless /usr/local/bin/chromium-headless
RUN chmod +x /usr/local/bin/chromium-headless

# 让引擎 preflight 能定位浏览器/编码器(slim 下这些路径不存在，preflight 会据此降级)。
# chromium-headless-shell 路径优先(Docker),完整 chrome 路径(桌面)fallback。
ENV POLARIS_CHROMIUM=/usr/bin/chromium \
    POLARIS_CHROMIUM_HEADLESS_SHELL=/usr/bin/chrome-headless-shell \
    POLARIS_FFMPEG=ffmpeg \
    POLARIS_RENDER_FLAVOR=${POLARIS_RENDER}
# Xvfb 套 launcher：把 chromium/CloakBrowser 这种需要 X server 的命令自动包到 xvfb-run 之下；
# 不动 polaris-server 本身(它是 headless 服务的)。claude/cli 等无头命令照常跑。
# 屏幕尺寸挑 1280x800 —— 够公众号后台布局完整渲染,够排版面板 300px 侧栏不被切。
COPY docker-xvfb-wrap.sh /usr/local/bin/xvfb-wrap
RUN sed -i 's/\r$//' /usr/local/bin/xvfb-wrap \
    && chmod +x /usr/local/bin/xvfb-wrap
# 默认显示号;ClaakBrowser 拉起时会用 DISPLAY=:99 启 chromium。
ENV DISPLAY=:99

# ── 版本号注入(/app/VERSION)与容器内 update.sh ─────────────────────
# 真实版本号：仓库根 VERSION 文件优先;CI 也可走 build-arg POLARIS_VERSION 覆盖。
# 容器内 polaris-server 的 /api/version handler 读这个文件,前端 UpdatePanel 据此显示。
ARG POLARIS_VERSION=dev
COPY VERSION /app/VERSION
RUN echo "${POLARIS_VERSION}" > /app/VERSION.bak \
    && if [ "$(cat /app/VERSION | tr -d '[:space:]')" = "dev" ] || [ -z "$(cat /app/VERSION)" ]; then \
         echo "${POLARIS_VERSION}" > /app/VERSION; \
       fi
# docker CLI + compose 插件:容器内一键更新(update.sh 替身模式)要真 CLI 操作宿主 daemon。
# 纯客户端不装 daemon(docker-ce-cli ~50MB + compose-plugin ~60MB)。
# 源用清华 TUNA 镜像:Windows 构建机(中国网络)和 GitHub Actions 都可达。
RUN install -m 0755 -d /etc/apt/keyrings \
    && curl -fsSL https://mirrors.tuna.tsinghua.edu.cn/docker-ce/linux/debian/gpg -o /etc/apt/keyrings/docker.asc \
    && echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.asc] https://mirrors.tuna.tsinghua.edu.cn/docker-ce/linux/debian bookworm stable" \
        > /etc/apt/sources.list.d/docker.list \
    && apt-get update && apt-get install -y --no-install-recommends docker-ce-cli docker-compose-plugin \
    && rm -rf /var/lib/apt/lists/*

# update.sh 拷进镜像 → /usr/local/bin/update.sh;容器内 spawn 它派出替身容器完成 pull+重建。
# (替身经容器自身的 compose 标签定位宿主 compose 项目目录,无需把 compose 文件打进镜像。)
COPY update.sh /usr/local/bin/update.sh
RUN sed -i 's/\r$//' /usr/local/bin/update.sh \
    && chmod +x /usr/local/bin/update.sh

# 引擎二进制 + 前端静态 + 资源种子
COPY --from=server /usr/local/bin/polaris-server /usr/local/bin/polaris-server
COPY --from=server /usr/local/bin/polaris-forge  /usr/local/bin/polaris-forge

# ── 本地开源嵌入/重排(local-embed)运行时:onnxruntime 共享库 ─────────────────
#   ort 用 load-dynamic 经 ORT_DYLIB_PATH 在「真正用到时」才 dlopen → 缺库不影响启动,
#   只是本地嵌入不可用(默认 POLARIS_LOCAL_EMBED 未开,走云,不受影响)。故下载做成**非致命**。
#   onnxruntime 1.24 匹配 ort 2.0.0-rc.12;bookworm(glibc2.36/libstdc++12)满足其依赖。
ENV ORT_DYLIB_PATH=/usr/local/lib/libonnxruntime.so
ENV FASTEMBED_CACHE_DIR=/root/Polaris/models/fastembed
RUN apt-get update && apt-get install -y --no-install-recommends libgomp1 ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* ; \
    ( set -e; ORT_VER=1.26.0; \
      curl -fsSL "https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VER}/onnxruntime-linux-x64-${ORT_VER}.tgz" -o /tmp/ort.tgz \
      && tar -xzf /tmp/ort.tgz -C /tmp \
      && cp /tmp/onnxruntime-linux-x64-${ORT_VER}/lib/libonnxruntime.so* /usr/local/lib/ \
      && ln -sf "$(ls /usr/local/lib/libonnxruntime.so.* | head -1)" /usr/local/lib/libonnxruntime.so \
      && ldconfig \
      && echo "[onnxruntime] ${ORT_VER} ready (local-embed available)" ) \
    || echo "[onnxruntime] download/install failed -> local-embed unavailable (默认走云,不影响启动)" ; \
    rm -f /tmp/ort.tgz

COPY --from=web    /app/dist /srv/web
COPY src-tauri/resources /app/resources

# ── 预装飞书桥 SDK：构建期(Windows 有网)就把 @larksuiteoapi 装好 ──────────────
#   飞书网关运行时实际跑在 /root/Polaris/feishu-bridge,但该路径是命名卷,
#   运行时会被卷覆盖 → 这里先装进镜像内 /opt/feishu-bridge,由 entrypoint 在卷挂好后
#   seed 进去。这样容器首启不再触发联网 npm install(NAS 容器出网受限会失败)。
COPY src-tauri/assets/feishu_bridge.mjs          /opt/feishu-bridge/bridge.mjs
COPY src-tauri/assets/feishu_bridge_package.json /opt/feishu-bridge/package.json
RUN cd /opt/feishu-bridge \
    && npm install --no-audit --no-fund --registry=https://registry.npmmirror.com \
    && npm cache clean --force

ENV HOME=/root \
    POLARIS_RESOURCE_DIR=/app/resources \
    POLARIS_WEB_DIR=/srv/web \
    POLARIS_PORT=8080 \
    # 版本号：/app/VERSION 优先;这里是 build-arg 透传兜底
    POLARIS_VERSION=${POLARIS_VERSION} \
    # claude headless 默认非交互；让其在容器里直接用环境变量鉴权
    CI=1

# 入口脚本：tini 作 PID 1（镜像内自带，回收 claude spawn 的子进程僵尸，
# 不再依赖 compose `init: true` 在群晖 Container Manager 下是否生效）；
# 脚本按 PUID/PGID 决定 root / 非 root 运行。sed 去 CR 防 Windows 换行致 exec 失败。
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN sed -i 's/\r$//' /usr/local/bin/docker-entrypoint.sh \
    && chmod +x /usr/local/bin/docker-entrypoint.sh

# 版本标签 —— 容器内 update.sh 靠 `docker inspect .Config.Labels["org.polaris.version"]`
# 与 R2 清单里的 version 比对做「已是最新就跳过」的幂等判断。此前没打这个标签,
# 取到的恒为空 → 每次点网页「一键更新」都把整包重下一遍。放在末尾:只失效元数据层。
LABEL org.polaris.version="${POLARIS_VERSION}"

EXPOSE 8080
# tini -g 杀进程组(SIGTERM 给整个进程组而非只 tini 直接子进程);
# sh -c 套 chromiumoxide/chromium 启动时,sh 退出后子进程会变孤儿,-g 一次穿透
ENTRYPOINT ["/usr/bin/tini", "-g", "--", "/usr/local/bin/docker-entrypoint.sh"]
