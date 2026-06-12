# DRIFT.md — Docker 仓相对主仓的「落后债」清单

> 配合 [.docker-owned](.docker-owned) 与 [scripts/sync-guard.ps1](scripts/sync-guard.ps1) 一起看。
>
> `.docker-owned` 管的是「**Docker 有、主仓没有**」的分叉（保护它别被覆盖）。
> 本文件管的是反方向：「**主仓有、Docker 还没追上**」的落后债——这些**不能**在普通同步里
> 顺手并，因为它们牵一发动全身，必须开一个**单独的、跑完整 `cargo build` 验证过**的 catch-up pass。
> 在那之前，`.docker-owned` 把相关文件标成 OWNED，让守卫脚本拦住盲覆盖。

更新于：2026-06-12（v1.0.5 同步时盘点；同日并入寓言计划三件套,见 D0）

---

## D0. 寓言计划三件套已并入（2026-06-12,主仓尚未 commit 的前瞻同步）✅

**内容**：`sense.rs`(感官 API 坞) + `echo.rs`(回声层做梦) + `fable/`(检索枢纽:盘点 L1a /
向量索引 / grep∥RAG 塌平混检) + 前端 `SenseApi.vue` 整页 + `Settings.vue`/`App.vue`/`app.ts`
增量 + `conv.rs`/`kb.rs`/`claude_md.rs` 增量(均为主仓正向增量,非 OWNED,整拷) + `docker/sense-models.sh`。
手并的 OWNED 文件：`Cargo.toml`(+chrono+rusqlite bundled)、`lib.rs`(+3 个 `pub mod`)、
`server.rs`(+opt_bool/opt_f64/opt_u8 助手 + sense/echo/fable dispatch + init 三行)、
`src/bin/polaris-forge.rs`(+`fable` 子命令组,容器内 agent 的全盘检索工具)。
**验证**：`cargo check --lib/--bins --no-default-features --features server` 绿 + `vue-tsc` 绿(Windows)。
**注意**：主仓该批改动同步时尚未 commit——主仓 commit 后做常规同步,这批共享文件 hash 应当
已一致(对得上就跳过)。镜像未重建;下次 CI 因 rusqlite(bundled) 编 C 会略变慢属正常。

---

## D1. polaris-forge CLI 进 Docker 镜像 ✅ 已完成（2026-06-12，commit 见下）

**结论**：已落地并 Windows 真机验证（`cargo build --bin polaris-forge --features server` 绿、
`preflight` 出 JSON、`spec-pptx` 端到端出合法可编辑 .pptx 并 validate 通过）。做法**未照搬主仓的
独立 polaris-cli crate**（会和 _pdocker 主包 `[[bin]] polaris-server` 撞名、还要动 docker 分叉的
server 入口），改成**主包加 `src/bin/polaris-forge.rs` + `[[bin]]`**（docker 不跑 tauri bundler，
主包加 bin 安全）。补齐了 `forge_pptx_native.rs`（spec→OOXML）、`forge::spec_to_pptx_sync`、
`forge_pptx.rs` 的 `TmpGuard`+10 个符号放宽 `pub(crate)`。Dockerfile 出双 bin，DOCKER.md 补文档。

<details><summary>原始诊断（保留备查）</summary>

**当初现状**：主仓把渲染引擎封了一个独立 crate `crates/polaris-cli`，出 `polaris-forge` 二进制
（子命令 preflight / pptx / spec-pptx / video / tts / validate），让容器内 agent 命令行出片。
群晖那份工作副本的 Dockerfile/DOCKER.md 已经在用它。

**Docker (_pdocker) 还没追上**：
- 没有 `crates/polaris-cli` crate；`polaris-server` 仍是**主包 `[[bin]]`**（住 `src-tauri/src/bin/polaris-server.rs`）。
- lib 里缺 `forge_pptx_native.rs`（spec→原生可编辑 OOXML，「slim 也能出传统 PPT」靠它）。
- Dockerfile stage 2 还是 `cargo build --bin polaris-server`，没出 `polaris-forge`。

**为什么不在普通同步里顺手做**：这是结构迁移——加 crate + 改 workspace `members`/`[[bin]]` +
补 `forge_pptx_native.rs` + 主 lib 的 `server` feature 要能在 _pdocker 这棵**已 drift 的树**上编过 +
Dockerfile 改 `-p polaris-cli` 出双 bin。任一处对不齐，镜像就挂在 build 阶段。
**必须开专门 pass、跑完整 `cargo build -p polaris-cli --no-default-features --features server` 验证后再推。**

**追的时候怎么做**（备忘）：
1. 拷主仓 `crates/polaris-cli/`（薄 crate：Cargo.toml + src/main.rs + src/bin/polaris-server.rs）。
2. 拷主仓 `src-tauri/src/forge_pptx_native.rs`，并按主仓 `lib.rs` 补 `pub mod forge_pptx_native;`。
   ⚠️ `lib.rs`/`chat.rs` 是 C2 drift 文件，得连带 catch-up，别只动一行。
3. `src-tauri/Cargo.toml`：`members` 已含 `crates/*`，确认 polaris-cli 被收；主包那两个
   `[[bin]]`（polaris-server / polaris-app）按主仓策略处理（Docker 不跑 tauri bundler，
   主包留 `[[bin]] polaris-server` 本身不炸，但要避免和 polaris-cli 重名冲突——二选一）。
4. Dockerfile stage 2a 改 `--lib`，stage 2b 改 `cargo build -p polaris-cli` 并 `cp` 两个 bin。
5. DOCKER.md 补 `polaris-forge` 用法（群晖那份可直接抄文案）。
6. **跑完整镜像 build（slim+full）验证后**再 push。

（实际落地时走了更省事的「主包加 bin」路线，见上方 ✅ 小结；本备忘的 polaris-cli crate 路线未采用。）

</details>

## D2. chat.rs 提示词装配落后

主仓 `chat.rs` 有 `longtask_convention` / 创作模式（CREATIVE_SKILL_IDS）/ `script_convention` 等
always-on 注入；_pdocker 的 `chat.rs` 是老版装配（kb_first→skill→reply_style→output→project→batch），
**没有**这些。属 reinforcement-only（少了不崩，只是行为不如桌面端克制/稳健）。
追的时候要连 lib.rs 的相关导出一起核对（C2 已标 OWNED）。

## D3. deck / 模板版本

主仓 `DECK_VERSION` 已到 5、版式扩到 9 版式；_pdocker 的 deck 模板停在更早版本。
属渐进增强，单独追时连 `slidesSpec.ts`/`deckThemes.ts`/`runtime.js` 一起对齐。

---

## 同步纪律一句话

- **从主仓拿新功能**：`pwsh scripts/sync-guard.ps1 -Apply` —— SAFE 文件自动并，OWNED 留手并。
- **加了新的 Docker-only 分叉**：登记进 `.docker-owned`，跑 `-Audit` 体检。
- **追 D1–D3 这种落后债**：单开 pass，跑完整 `cargo build` / 镜像 build 验证，**别混进日常同步**。
