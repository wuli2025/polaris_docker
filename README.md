# Polaris · Docker 版（含远程更新）

把 Polaris（Tauri 桌面 AI 工作台）跑成**浏览器访问的容器服务**，与桌面版**共用同一份源码**。
镜像由 GitHub Actions 构建并推送到 **GHCR**，用户侧一条 `./update.sh` 即可**远程更新**——不本地重建、不丢数据。

当前版本：**v1.0.2**（含「传统 PPT 可编辑化 + spec 原生导出」）。

---

## 一、最快上手（拉预构建镜像，推荐）

```bash
# 方式 A：一键安装脚本（从 Cloudflare 拉安装包，不用 clone 整仓）
curl -fsSL https://llmwiki.cloud/docker/install.sh | bash
cd polaris-docker
# 编辑 .env 填鉴权，然后：
docker compose up -d
# 打开 http://localhost:8080
```

```bash
# 方式 B：clone 本仓库
git clone https://github.com/wuli2025/polaris_docker.git && cd polaris_docker
cp .env.example .env      # 编辑填鉴权
docker compose up -d      # 自动拉 ghcr.io/wuli2025/polaris:latest
```

> **镜像为私有**（仓库保持 private 的决策）：`docker compose up -d` 拉取前需先登录一次 GHCR：
> `docker login ghcr.io -u wuli2025`（密码用 GitHub PAT，勾 `read:packages` 权限即可，登录一次永久有效）。
> 不想登录的话用 `docker-compose.build.yml` 从源码本地构建（见第三节）。

镜像口味（`POLARIS_TAG`）：

| TAG | 内容 | 体积 |
| --- | --- | --- |
| `latest` / `1.0.2`（默认 slim） | 聊天 / KB / 网站生成 + **传统 PPT 原生导出**（零浏览器） | 小 |
| `full` / `1.0.2-full` | 上面 + chromium + ffmpeg + CJK 字体（deck 截图 PPT / 视频） | 大 |

```bash
# 要 full：
POLARIS_TAG=full docker compose up -d
```

---

## 二、远程更新

```bash
./update.sh                 # = docker compose pull && docker compose up -d（slim）
POLARIS_TAG=full ./update.sh
# Windows： ./update.ps1
```

数据卷 `polaris-data` / `polaris-claude` / `polaris-config` **原样保留**，只换镜像层。

---

## 三、想自己从源码构建

```bash
docker compose -f docker-compose.yml -f docker-compose.build.yml up -d --build
# full：
POLARIS_RENDER=1 docker compose -f docker-compose.yml -f docker-compose.build.yml up -d --build
```

---

## 四、群晖 / NAS 部署

见 [`DEPLOY-SYNOLOGY.md`](./DEPLOY-SYNOLOGY.md) 与 [`DOCKER.md`](./DOCKER.md)（架构、鉴权、加固、GPU 解耦）。
群晖用 `docker-compose.synology.yml`（bind 挂载到 `/volume1`、PUID 降权等）。

---

## 五、镜像怎么来的

`.github/workflows/image.yml`：push 到 `main` 或打 `v*` 标签即在 GitHub Actions 构建 slim+full
并推到 `ghcr.io/wuli2025/polaris`。安装包（compose+脚本+.env.example）另托管在 Cloudflare
（`https://llmwiki.cloud/docker/`）与本仓库 Releases，二选一下载。
