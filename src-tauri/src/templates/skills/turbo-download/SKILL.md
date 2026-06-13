---
id: turbo-download
name: 极速下载 TurboDownload
description: 下载大文件(>200MB)时用 aria2c 多连接分段下载替代单线 curl/wget，把被「按连接限速」的链路提速数倍（NAS 实测批量下载 6.7 倍：6.9→45.6 MB/s）。跨 Windows/macOS/Linux/群晖：自带 Python 脚本会自动探测文件大小、按平台装好 aria2、失败优雅回退到单线/ curl。拉模型/数据集/镜像/依赖包/安装器等任何大文件都默认走它。
source: official
author: Polaris
created_at: 1749800000
---

# 极速下载 TurboDownload（Polaris 默认大文件下载器）

下载**大文件（>200MB）**或**一批文件**时，不要用单线 `curl`/`wget`/`Invoke-WebRequest`——用本技能自带的跨平台脚本，它用 `aria2c` 多连接分段并行下载，实测批量场景比单线快约 **6.7 倍**。

## 怎么用（一条命令，三端通用）

脚本在 `~/Polaris/skills/turbo-download/scripts/fast_download.py`（Windows: `%USERPROFILE%\Polaris\skills\turbo-download\scripts\fast_download.py`）。**纯 Python 标准库、零第三方依赖。**

**单个大文件**：
```bash
uv run --no-project ~/Polaris/skills/turbo-download/scripts/fast_download.py \
  "<URL>" -d "<目标目录>" -o "<文件名(可选)>"
```

**一批文件**（urls.txt 每行一个 URL）：
```bash
uv run --no-project ~/Polaris/skills/turbo-download/scripts/fast_download.py \
  -i urls.txt -d "<目标目录>"
```

- 优先 `uv run --no-project`（uv 由环境医生托管，三端同构，避开 Windows 上 `python` 是 Store 占位符的坑）。
- 没有 uv 的环境（如群晖 NAS busybox）直接 `python3 fast_download.py ...` 也行——脚本只用标准库。

## 脚本替你做了什么（不用自己拼命令）
1. **找/装 aria2c**：先看 PATH 和 `~/Polaris/bin` 缓存；没有就**按平台自动装**——Windows 走 winget/scoop/choco 或下官方便携版；macOS 走 `brew install aria2`；Linux/NAS 按 CPU 架构下 abcfy2 musl **全静态**二进制（零依赖，群晖/容器都能跑）。
2. **探测**：HEAD 拿 `Content-Length` + `Accept-Ranges`。文件 <10MB 或服务器不支持 Range（多连接无意义）→ 自动单线；否则 `-x16 -s16 -k1M` 多连接。
3. **断点续传**：永远带 `-c`，中断重跑不从头。
4. **三级优雅回退**：aria2 多连接 → aria2 单线 → `curl` → `urllib`。任意一层成了就停，**保证再差也能下下来**。
5. **进度 + 收尾**：实时进度条，结束打印「大小 / 耗时 / 平均速度」。

## 重要约定
- **默认直连不走代理**（机场/代理带宽会卡死大文件）。只有当**下载源本身被墙**时才传 `--proxy http://127.0.0.1:7890`。
- **aria2 二进制自身**若因 GitHub CDN 被墙下不动，给环境变量 `POLARIS_DL_PROXY=http://127.0.0.1:7890`——它**只**用于拉那个几 MB 的 aria2 包，不影响文件下载直连。
- **批量下载注意目标站每秒请求上限**：脚本批量默认 `-j5`（5 文件并行）已较温和；若目标站有硬限（如 SEC 10 req/s），别再调高并发，否则封 IP。
- 详细 aria2 参数与调优见 `references/aria2_flags.md`（按需查阅）。

## 不适用
小文件（<200MB 且不急）用普通 `curl -L`/`wget` 即可，不必动用本技能。macOS 上若没装 Homebrew 且拉不到 aria2，脚本会自动退化成 curl 单线（仍能下，只是不加速）——提示用户 `brew install aria2` 一次即可全速。
