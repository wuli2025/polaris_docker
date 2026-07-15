# MCP 工具开发

给 AI Agent 造工具（MCP server）时用。裸写的通病：把 API 直译成工具、描述含糊、错误信息没法自愈、没有评测。按四阶段走。源自 Anthropic 官方 mcp-builder。

## Phase 1 · 研究与设计
- 先读目标 API 文档和 MCP 协议要点，别边写边猜。
- 工具从 **agent 的任务视角**设计，不是 API 端点的一比一映射：一个工具应完成一件对 agent 有意义的事。
- 命名统一前缀便于发现：`github_create_issue`、`github_list_prs`。
- 错误信息要**可操作**：告诉 agent 错在哪、怎么改参数重试，而不是裸抛 500。
- 选型：TypeScript 优先；远程用 Streamable HTTP，本地用 stdio。

## Phase 2 · 实现
- 先搭地基再写工具：API client、鉴权、统一错误处理、分页封装。
- 输入 schema 用 Zod（TS）/ Pydantic（Python），每个字段带描述；定义 outputSchema。
- 标注行为注解：`readOnlyHint` / `destructiveHint` / `idempotentHint` / `openWorldHint`。
- 返回内容为 agent 消化设计：结构化、可截断、大结果给分页游标。

## Phase 3 · 评审与实测
- 过一遍：类型全覆盖、无重复逻辑、机密走环境变量。
- 构建后用 `npx @modelcontextprotocol/inspector` 真连真调每个工具。

## Phase 4 · 评测
- 出 10 道**复杂、真实、只读、答案可验证且稳定**的任务题，让模型用这套工具作答，跑通率就是工具质量分。
- 评测不过 → 回去改工具描述和错误信息，通常是它们的锅。
