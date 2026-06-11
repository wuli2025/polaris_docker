# ════════════════════════════════════════════════════════════════
# Polaris · Docker 化镜像（方案 A：保留 Rust 引擎，axum 替代 Tauri 外壳）
#   阶段1 web      —— 构建 Vue3 前端 → dist/
#   阶段2 server   —— 构建 polaris-server（复用同一份 Rust 引擎，不含 Tauri）
#   阶段3 runtime  —— node-slim + 预装 claude CLI，托管前端 + 跑 HTTP/WS 服务
#
# 构建：docker build -t polaris-web .
# 运行：见 docker-compose.yml
# ════════════════════════════════════════════════════════════════

# ── 阶段1：构建前端 ──────────────────────────────────────────────
# v1.0.2 起恢复真实前端构建(vue-tsc 类型债已清零)→ Docker 版与桌面版同一套 UI,
# 含「传统 PPT 可编辑化」。先拷清单装依赖(层缓存),再拷源码跑 vue-tsc + vite build。
FROM node:20-slim AS web
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci
COPY index.html vite.config.ts tsconfig.json tsconfig.node.json ./
COPY src ./src
COPY public ./public
RUN npm run build

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

# 2a) 依赖缓存层：先只拷清单 + crates 源 + 空占位 src，预编译全部第三方依赖。
#     之后改业务代码不会重编 axum/tokio 等重型依赖 → Windows 更新后 Docker 快速重建。
COPY src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/build.rs ./src-tauri/
COPY src-tauri/crates ./src-tauri/crates
RUN mkdir -p src-tauri/src/bin \
    && echo 'fn main(){}' > src-tauri/src/bin/polaris-server.rs \
    && echo '' > src-tauri/src/main.rs \
    && echo '' > src-tauri/src/lib.rs \
    && cargo build --profile release-fast \
        --manifest-path src-tauri/Cargo.toml \
        --bin polaris-server --no-default-features --features server \
    ; rm -rf src-tauri/src

# 2b) 真实源码层：拷源码 + 资源 + assets(feishu/wecom 的 include_str!)，编出 polaris-server。
COPY src-tauri/src ./src-tauri/src
COPY src-tauri/assets ./src-tauri/assets
COPY src-tauri/resources ./src-tauri/resources
# 触碰 mtime 确保 cargo 重编 polaris-app crate 本体（而非缓存的空壳）。
# 顺手编 polaris-forge CLI(crates/polaris-cli):容器内 agent 直接命令行出
# PPT/截图/视频——传统PPT(spec→原生OOXML)零浏览器,slim 镜像也能出片。
RUN touch src-tauri/src/main.rs src-tauri/src/lib.rs \
    && cargo build --profile release-fast \
        --manifest-path src-tauri/Cargo.toml \
        --bin polaris-server --no-default-features --features server \
    && cargo build --profile release-fast \
        --manifest-path src-tauri/Cargo.toml \
        -p polaris-cli \
    && cp src-tauri/target/release-fast/polaris-server /usr/local/bin/polaris-server \
    && cp src-tauri/target/release-fast/polaris-forge /usr/local/bin/polaris-forge

# ── 阶段3：运行时 ────────────────────────────────────────────────
FROM node:20-slim AS runtime
# claude CLI 跑 Bash/脚本工具需要：bash、git、python3(pptx/xlsx 等技能)、ripgrep、ca 证书。
RUN apt-get update && apt-get install -y --no-install-recommends \
        bash git ca-certificates curl python3 python3-pip python3-venv ripgrep \
        tini gosu \
    && rm -rf /var/lib/apt/lists/* \
    && npm install -g @anthropic-ai/claude-code \
    && npm cache clean --force

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
RUN if [ "$POLARIS_RENDER" = "1" ]; then \
        apt-get update && apt-get install -y --no-install-recommends \
            fonts-noto-cjk fonts-noto-color-emoji fontconfig \
        && pip install --no-cache-dir --break-system-packages fonttools brotli 2>/dev/null \
            || pip install --no-cache-dir fonttools brotli \
        && mkdir -p /out \
        && python3 /docker/subset_cjk.py \
            || echo "[subset] 子集失败,降级全语种 102MB" \
        && if [ -d /out ] && [ -n "$(ls -A /out 2>/dev/null)" ]; then \
               mkdir -p /usr/share/fonts/truetype/noto-cjk-subset \
               && cp /out/*.woff2 /usr/share/fonts/truetype/noto-cjk-subset/ \
               && fc-cache -fv > /dev/null 2>&1 \
               && echo "[subset] SC 字体子集已落 /usr/share/fonts/truetype/noto-cjk-subset/"; \
           fi \
        && rm -rf /var/lib/apt/lists/* ; \
    fi

# ── 渲染栈(可选 flavor)——Polaris Forge 跨平台 PRD §05 ──
#   POLARIS_RENDER=0 → polaris:slim   聊天/KB/网站生成 + **传统PPT 仍可出**
#                                 (polaris-forge spec-pptx 原生 OOXML,零浏览器)
#   POLARIS_RENDER=1 → polaris:full   +chromium(deck 截图/CloakBrowser)+ffmpeg(视频)
#                                 +xvfb(虚拟显示)+libnss3 等原生库+CJK 字体子集(阶段3.5 落)
# 构建 full：docker build --build-arg POLARIS_RENDER=1 -t polaris-web:full 。
# 为什么装完整 chromium 而非 chrome-headless-shell(后者可省 ~150MB):
#   CloakBrowser **有头模式**(公众号扫码登录/直传,xvfb 虚拟屏)headless-shell 跑不了;
#   等公众号链路改纯 headless 后可降级 headless-shell(工业级化阶段 0 的 235MB 目标)。
# CJK 字体是「最隐蔽必踩」坑：缺了 deck 截图全是 □□□，preflight 会用 fc-list 探测并亮红灯。
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
        echo "[build] POLARIS_RENDER=0 → slim 镜像(无浏览器渲染栈;传统PPT 走 polaris-forge 原生路线仍可用)" ; \
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

# 引擎二进制 + 前端静态 + 资源种子
COPY --from=server /usr/local/bin/polaris-server /usr/local/bin/polaris-server
COPY --from=web    /app/dist /srv/web
COPY src-tauri/resources /app/resources

ENV HOME=/root \
    POLARIS_RESOURCE_DIR=/app/resources \
    POLARIS_WEB_DIR=/srv/web \
    POLARIS_PORT=8080 \
    # claude headless 默认非交互；让其在容器里直接用环境变量鉴权
    CI=1

# 入口脚本：tini 作 PID 1（镜像内自带，回收 claude spawn 的子进程僵尸，
# 不再依赖 compose `init: true` 在群晖 Container Manager 下是否生效）；
# 脚本按 PUID/PGID 决定 root / 非 root 运行。sed 去 CR 防 Windows 换行致 exec 失败。
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN sed -i 's/\r$//' /usr/local/bin/docker-entrypoint.sh \
    && chmod +x /usr/local/bin/docker-entrypoint.sh

EXPOSE 8080
# tini -g 杀进程组(SIGTERM 给整个进程组而非只 tini 直接子进程);
# sh -c 套 chromiumoxide/chromium 启动时,sh 退出后子进程会变孤儿,-g 一次穿透
ENTRYPOINT ["/usr/bin/tini", "-g", "--", "/usr/local/bin/docker-entrypoint.sh"]
