# Polaris 云服务器部署指南（Docker 一键上云）

> 面向「只会把文件传到服务器」的用户。照着从上到下做即可。
> 适用：一台 Linux 云服务器（Ubuntu 22.04/24.04、Debian 12 等），2 核 4G 起步，建议 4 核 8G。
>
> ⚠ 说明：本套 Dockerfile/compose **未在开发机实测构建**（开发机 WSL 无 docker）。
> 首次构建若报错，按文末「构建排错」逐条对照。

## 0. 装 Docker（服务器上执行一次）

```bash
curl -fsSL https://get.docker.com | bash
# 验证
docker --version && docker compose version
```

## 1. 把代码放到服务器

方式 A（推荐，便于以后升级）——git 克隆：

```bash
git clone <你的仓库地址> polaris-app
cd polaris-app
```

方式 B——本地打包上传：

```bash
# 本地（排除垃圾目录，压缩会小很多）
tar --exclude node_modules --exclude src-tauri/target --exclude .git \
    -czf polaris.tgz -C /path/to polaris-app
scp polaris.tgz root@服务器IP:~
# 服务器
tar xzf polaris.tgz && cd polaris-app
```

## 2. 填环境变量

```bash
cp .env.server.example .env
nano .env
```

必填两项：

- `POLARIS_AUTH_TOKEN`：机器级 owner 访问口令，必须使用独立强随机值。
  生成一个：`openssl rand -hex 32`
  （容器**默认是免口令**的——那是给 NAT 后面的家用 NAS 设的。云机对公网可达，
  这一项必须显式设上，否则等于把 owner 接口敞开。）
- `POLARIS_BIND_IP`：保持默认 `127.0.0.1`。这让 8080 只供宿主机上的 HTTPS
  反向代理访问，避免 owner 口令、聊天和文件走公网明文。
- LLM 接入（三选一）：`ANTHROPIC_API_KEY`，或第三方端点的
  `ANTHROPIC_BASE_URL` + `ANTHROPIC_AUTH_TOKEN`，或都留空、起服务后在
  App 内「供应商」面板登录 Claude 订阅。

## 3. 构建并启动

```bash
docker compose up -d --build
```

首次构建要编译 Rust，视机器 10–30 分钟属正常。看进度：

```bash
docker compose logs -f polaris
```

## 4. 验证

```bash
curl http://localhost:8080/api/health
# 期望返回 ok
curl -f http://localhost:8080/api/ready
# 期望返回 ready；数据卷、SQLite 或前端入口异常时会返回 503
docker compose ps   # polaris 应为 healthy
```

不要在云安全组开放 8080。先用 Caddy / Nginx / Traefik 把你的 HTTPS 域名反代到
`127.0.0.1:8080`，只开放 TCP 443；证书必须有效，并建议开启 HSTS。以下是最小 Caddyfile
示意（把域名换成已解析到本机的域名）：

```caddyfile
polaris.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

若开 iroh 中继，按需另放行 443 TCP/UDP。只有在完全可信的隔离内网调试时，才可把
`POLARIS_BIND_IP` 改为 `0.0.0.0`；不要用这个设置直接暴露公网。

## 5. 首次打开：建 owner 账号

浏览器访问：

```
https://polaris.example.com/?token=你的POLARIS_AUTH_TOKEN
```

页面读取口令后会立即从地址栏和浏览器历史中清除，并只保留在本次浏览器会话。不要把含
`?token=` 的初始链接发到群聊、工单或截图中。

首次进入会引导创建 **owner（所有者）账号**——这是多人协作的最高权限账号，
账号密码务必记牢。之后队友的账号由 owner 在管理面板发「邀请票据」加入。

协作项目当前只绑定 `POLARIS_REPO_ROOT` 内已经存在的本地 Git 仓库，不接受远程 URL，
也不会自动 clone。Docker 默认目录是 `/home/polaris/Polaris/repos`；请先把仓库放入对应
volume/挂载目录，再由 owner 在界面创建项目。此限制用于阻止成员借 Git 操作触达宿主机任意目录。

## 6.（实验性）Gitea 管理员

compose 带无头 Gitea（仅容器内可访问，未对公网开端口，且已禁自助注册），但当前项目
创建→仓库创建/克隆→成员 ACL 尚未自动编排，生产流程不要依赖这一 profile。
第一次需要创建管理员：

```bash
docker compose exec -u git gitea gitea admin user create \
  --admin --username "$GITEA_ADMIN_USER" \
  --password "$GITEA_ADMIN_PASSWORD" --email "$GITEA_ADMIN_EMAIL"
```

（变量即 .env 里 GITEA_ADMIN_* 三项；也可直接写明文。）

## 7.（实验性，当前未接入产品流程）iroh-relay

该容器只是中继实验组件；当前桌面发布未启用并接通完整的 NodeId 配对/隧道流程，不能靠
它实现“粘贴配对码自动连主机”。生产多人协作请使用上面的 HTTPS Docker 主机。若仅做
协议开发，需要 443 端口空闲并放行 443 TCP+UDP。

```bash
# 1) 生成纯 IP 自签证书（把 IP 换成你的公网 IP）
bash docker/relay/gen-cert.sh 203.0.113.7

# 2) 准备配置
cp docker/relay/config.toml.example docker/relay/config.toml

# 3) 带 relay profile 启动
docker compose --profile relay up -d
```

## 8. 数据备份与恢复

所有数据都在三个 docker volume 里：`polaris-data`（知识库/collab.db 等）、
`polaris-claude`（LLM 登录态）、`gitea-data`。

备份（打成 tar 落在当前目录）：

```bash
for v in polaris-data polaris-claude gitea-data; do
  docker run --rm -v ${PWD%/*}/_:/x -v $(basename $PWD)_$v:/data -v $PWD:/backup \
    debian:bookworm-slim tar czf /backup/$v-$(date +%F).tgz -C /data .
done
```

> 提示：volume 实际名字带 compose 项目前缀，`docker volume ls` 确认后替换。

恢复：新机器先 `docker compose up -d` 生成空卷再停掉，然后：

```bash
docker run --rm -v <卷名>:/data -v $PWD:/backup debian:bookworm-slim \
  bash -c "cd /data && tar xzf /backup/<备份文件>.tgz"
docker compose up -d
```

## 9. 升级

```bash
cd polaris-app
git pull                          # 方式 B 用户：重新上传解压覆盖
docker compose up -d --build      # 重建镜像并滚动重启，数据在卷里不受影响
docker image prune -f             # 清理旧镜像层
```

## 构建排错

| 症状 | 处理 |
| --- | --- |
| `npm ci` 报 lock 不同步 | 确认上传了根目录 `package-lock.json`；或临时把 Dockerfile 里 `npm ci` 改 `npm install` |
| `vue-tsc` 类型报错卡住前端 | 本地先跑 `npm run build` 确认能过；不能过就先修类型错误 |
| Rust 编译报缺系统库（`*-sys` build failed） | 看报错缺什么，在 Dockerfile stage2 的 apt 行补装（已预装 pkg-config/libssl-dev/cmake/nasm/clang） |
| 报找不到 `polaris-server` bin | 构建命令必须是 `cargo build --release -p polaris-cli --bin polaris-server`（bin 在 crates/polaris-cli，不在主包） |
| 内存不足被 OOM 杀（编译期） | 加 swap：`fallocate -l 4G /swap && chmod 600 /swap && mkswap /swap && swapon /swap` |
| relay 镜像拉不到 | 见 docker-compose.yml relay 服务内注释，改用源码构建或 `cargo install iroh-relay` |
| healthcheck 一直 starting | `docker compose logs polaris` 看启动日志；常见是 .env 没填导致鉴权/供应商初始化告警（不致命）或端口被占 |
