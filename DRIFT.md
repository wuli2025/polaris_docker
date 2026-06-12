# DRIFT.md — Docker 仓相对主仓的「落后债」清单

> 配合 [.docker-owned](.docker-owned) 与 [scripts/sync-guard.ps1](scripts/sync-guard.ps1) 一起看。
>
> `.docker-owned` 管的是「**Docker 有、主仓没有**」的分叉（保护它别被覆盖）。
> 本文件管的是反方向：「**主仓有、Docker 还没追上**」的落后债——这些**不能**在普通同步里
> 顺手并，因为它们牵一发动全身，必须开一个**单独的、跑完整 `cargo build` 验证过**的 catch-up pass。
> 在那之前，`.docker-owned` 把相关文件标成 OWNED，让守卫脚本拦住盲覆盖。

更新于：2026-06-12（v1.0.5 同步时盘点）

---

## D1. polaris-forge CLI 没进 Docker 镜像（最大一笔）

**现状**：主仓把渲染引擎封了一个独立 crate `crates/polaris-cli`，出 `polaris-forge` 二进制
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
