# REST API 设计

设计 / 评审 HTTP API 时遵循 REST 约定，让接口可预测、易用、好演进。

## 资源与 URL
- 用**名词复数**表资源：`/users`、`/users/{id}`、`/users/{id}/orders`。
- 不在 URL 放动词（不是 `/getUser`）；动作用 HTTP 方法表达。
- 层级表关系，别超过两层深；复杂查询用查询参数。

## 方法与语义
- `GET` 读（幂等、无副作用）、`POST` 建、`PUT` 整体替换（幂等）、`PATCH` 局部更新、`DELETE` 删。
- 幂等性要对：重试 `PUT`/`DELETE` 不应产生额外效果。

## 状态码
- 2xx：200 OK / 201 Created（带 `Location`）/ 204 No Content。
- 4xx：400 入参错 / 401 未认证 / 403 无权限 / 404 不存在 / 409 冲突 / 422 校验失败 / 429 限流。
- 5xx：500 服务端错。别用 200 包错误。

## 约定
- **分页**：`?page=&limit=` 或游标 `?cursor=`，响应带总数 / next。
- **过滤 / 排序 / 字段**：`?status=active&sort=-created_at&fields=id,name`。
- **版本化**：`/v1/...` 或 `Accept` 头；破坏性变更才升版本。
- **错误体统一**：`{ "error": { "code": "...", "message": "...", "details": [...] } }`。
- **鉴权**：`Authorization: Bearer <token>`；幂等键 `Idempotency-Key` 防重复提交。
- 文档化：每个端点写清入参 / 出参 / 错误码（可配 Swagger/OpenAPI）。
