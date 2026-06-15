# Docker 容器化部署

把应用打成镜像、跑起来、可部署。产出精简、可复现、安全的容器。

## Dockerfile 要点
1. **选小基镜像**：能用 `alpine` / `slim` / `distroless` 就别用全量。多阶段构建：编译阶段装工具链，运行阶段只拷产物。
2. **善用缓存层**：先拷依赖清单（package.json / requirements.txt / Cargo.toml）装依赖，再拷源码 —— 改代码不重装依赖。
3. **非 root 运行**：建普通用户跑进程，别用 root。
4. **明确入口**：`EXPOSE` 端口、`ENV` 配置、`CMD`/`ENTRYPOINT` 启动。
5. **配 `.dockerignore`**：排除 node_modules / target / .git / 本地缓存，缩小构建上下文。

## 多阶段示例骨架
```dockerfile
FROM node:20-slim AS build
WORKDIR /app
COPY package*.json ./
RUN npm ci
COPY . .
RUN npm run build

FROM node:20-slim
WORKDIR /app
COPY --from=build /app/dist ./dist
COPY --from=build /app/node_modules ./node_modules
USER node
EXPOSE 3000
CMD ["node", "dist/main.js"]
```

## compose / 运行
- 多服务用 `docker-compose.yml`：服务 / 网络 / 卷 / 健康检查 / 依赖顺序。
- 必设资源上限（`mem_limit`）、日志轮转，避免吃爆宿主。
- 配置走环境变量 / `.env`，**机密别写进镜像**。
- 数据持久化用具名卷 / bind mount。

## 约定
- 构建后实际 `docker run` 起一次验证能跑、端口通。
- 镜像体积异常大时排查：是不是把构建工具 / 缓存 / 多语言字体打进了运行层。
