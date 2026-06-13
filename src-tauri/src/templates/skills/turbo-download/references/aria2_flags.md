# aria2c 参数与调优速查（L3 · 按需查阅）

来源：aria2 官方 manual 1.37.0 + 业界共识。脚本 `fast_download.py` 已内置最优默认，本表供需要手动调参或排障时查阅。

## 黄金三件套（单个大文件）
```bash
aria2c -x16 -s16 -k1M -c -m5 --retry-wait=2 \
  --file-allocation=none --console-log-level=warn --summary-interval=1 \
  -d <dir> -o <name> <url>
```

| 参数 | 默认 | 作用 | 推荐 |
|------|------|------|------|
| `-x, --max-connection-per-server` | 1 | 对单服务器最大连接数 | **16**（多数站点上限） |
| `-s, --split` | 5 | 一个文件拆成 N 段 | **16**（与 -x 配齐） |
| `-k, --min-split-size` | **20M** | 小于 2×SIZE 的区间不再拆 | **1M** ⚠️ 关键：不改这个，<40M 的文件根本不会多连接，提速失效 |
| `-j, --max-concurrent-downloads` | 5 | 并行下载的**文件**数（单文件的分段不计入） | 批量用 5，单文件无所谓 |
| `-c, --continue` | false | 断点续传 | **true**（务必开） |
| `-m, --max-tries` | 5 | 重试次数，0=无限 | 5（不稳网可设 0） |
| `--retry-wait` | 0 | 重试间隔秒 | 2 |
| `--optimize-concurrent-downloads` | false | 按实测带宽自动调并发 | 批量场景开 true |
| `--file-allocation` | prealloc | none/prealloc/trunc/falloc | 跨平台安全用 **none**；ext4/xfs/NTFS 想更快用 falloc |
| `--auto-file-renaming` | true | 同名追加 .1–.9999 | 默认即可；要覆盖配 `--allow-overwrite=true` 并关掉它 |
| `--conditional-get` | false | 仅当本地比远端旧才下 | 镜像/增量更新场景开 |
| `--all-proxy=` | （空）| 清空代理强制直连 | 默认直连；要走代理填 `--all-proxy=http://host:port` |

## 批量（-i 输入清单）
```bash
aria2c -i list.txt -j5 -x16 -s16 -k1M -c \
  --optimize-concurrent-downloads=true --auto-file-renaming=false \
  --allow-overwrite=false --max-tries=5 --retry-wait=2 -d <dir>
```
- 清单格式：每行一个 URL；可在 URL 下方缩进写 per-line 选项，如 `  out=子目录_文件名`（用来防止不同路径同名文件互相覆盖）。
- 支持 `-i -` 从 stdin 读清单。

## 二进制来源（脚本自动选，排障时参考）
| 平台 | 来源 |
|------|------|
| Windows | winget `aria2.aria2` / scoop / choco；便携版 `aria2-1.37.0-win-64bit-build1.zip`（github aria2/aria2） |
| macOS | `brew install aria2`（universal，覆盖 arm64+x86_64）。**无可移植静态版**，无 brew 时退化 curl |
| Linux/NAS/容器 | abcfy2 musl **全静态**：`aria2-<arch>-linux-musl_static.zip`（x86_64 / aarch64 / armv7 / i686…），零 glibc 依赖 |

## 排障
- **多连接反而慢**：服务器不支持 Range 或限并发（如 wikimedia 实测单线 245KB/s、-x16 反降到 157KB/s）。脚本已自动探测 Accept-Ranges 并在不支持时退单线。
- **下载 aria2 包本身 0 字节卡死**：GitHub objects CDN 被墙 → 设 `POLARIS_DL_PROXY=http://127.0.0.1:7890` 只给二进制下载用代理。
- **群晖无 unzip**：脚本用 Python `zipfile` 解压，不依赖系统 unzip。
- **批量被目标站封 IP**：调低 `-j` 和 `-x`，加 `--retry-wait`；有硬性 req/s 限制的站（SEC 10/s）别用多连接批量。
