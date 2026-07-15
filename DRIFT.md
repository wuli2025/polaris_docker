# DRIFT.md — Docker 仓相对主仓的「落后债」清单

> 配合 [.docker-owned](.docker-owned) 与 [scripts/sync-guard.ps1](scripts/sync-guard.ps1) 一起看。
>
> `.docker-owned` 管的是「**Docker 有、主仓没有**」的分叉（保护它别被覆盖）。
> 本文件管的是反方向：「**主仓有、Docker 还没追上**」的落后债——这些**不能**在普通同步里
> 顺手并，因为它们牵一发动全身，必须开一个**单独的、跑完整 `cargo build` 验证过**的 catch-up pass。

更新于：2026-07-15（v2.1.0 catch-up）

---

## 当前状态：落后债 = 0 ✅

2026-07-15 的 v2.1.0 catch-up 把积压的 **3 个版本 / 663 文件 / ~10 万行**一次还清，
docker 仓的共享源码现已与主仓 v2.1.0 **同构**。旧的 D0 / D1 条目全部作废（见文末「历史」）。

今后按常规节奏同步即可，别再攒这种规模的债。**攒债的代价不是线性的**：这次真正的工作量
不在那 663 个文件（绝大多数无脑取新版即可），而在于「**判断哪些是真分叉**」——
而判断成本随落后的版本数增长。

---

## v2.1.0 catch-up 做了什么（2026-07-15）

**背景**：docker 仓 VERSION 停在 1.7.1，主仓已到 2.1.0，期间主仓做了**分仓重构**
（src-tauri/src/ 下大批模块迁入 crates/，4 crate → 11 crate）。

**结论先行**：真正带 docker 语义的共享源码只有 **4 个半文件**，其余全是「落后漂移」，
直接取主仓新版即可。判定方法 = **blob 哈希比对**（见 .docker-owned 顶部教训 ①）。

| 做的事 | 说明 |
| --- | --- |
| 整树对齐 | `src-tauri/src`、`src-tauri/crates`、`src` 全换成主仓 v2.1.0（先删后拷，消除搬家后的陈旧文件） |
| **架构对齐** | docker 原先把 `polaris-server`/`polaris-forge` 的 `[[bin]]` 放在主包 `src/bin/`（旧 D1 的决定）；现改为**采用主仓的 `crates/polaris-cli`**。`src-tauri/src/bin/` 已删除 |
| Dockerfile 构建段重写 | 旧的「stub `src/bin/*.rs` 预热依赖」层在新布局下**不再可行**（stub 主包 lib.rs → polaris-cli 找不到 polaris-app 的符号必炸）。改为整包一次编 `-p polaris-cli`，依赖缓存交给 image.yml 的 GHA cache |
| **+collab-net** | 镜像新增 `--features collab-net`。**这是 NAS 侧连不出 NodeId 的实测根因**——没有它，桌面「设备联盟」的 P2P 远程盘直连不可用 |
| local-embed 转发 | `crates/polaris-cli/Cargo.toml` 加 `local-embed = ["polaris-app/local-embed"]`（主仓没有），使一条 `-p polaris-cli --features collab-net,local-embed` 即出双 bin |
| ort feature 保住 | `crates/polaris-fable/Cargo.toml` 的 fastembed 仍用 `ort-load-dynamic`（主仓换成了 download-binaries，与本镜像的 ORT_DYLIB_PATH 分层冲突） |
| `/api/version` 补齐 | **它一直是个幽灵路由**：DOCKER.md / DEPLOY-SYNOLOGY.md / Dockerfile 注释 / 前端 useUpdater 全都引用它，但 server.rs 里从来没有过这个路由 → 前端那句「修显示 v—」的逻辑其实一直在吃 404。本轮真正实现（读 `/app/VERSION`） |
| 容器自更新重落 | `useUpdater.ts`(+106) / `UpdatePanel.vue`(+177) 三方合并到主仓 v2.1.0 上（基点 `archive/docker-pre-cleanup-2026-06-11`）。**注意**：主仓新增了冷启动退避重试，docker 那版没有——是**合流**不是叠加 |
| 6 个未提交修复落库 | 见 commit `36e8111`。它们当时只躺在工作区，**再晚一步就被这次整树覆盖抹掉了** |

**已验证**：`cargo check -p polaris-cli --features collab-net,local-embed` 绿；`npm run build`
（含 vue-tsc）绿。
**未验证**：镜像真机构建、NAS 端到端自更新与 `/api/version` 实际返回——靠 image.yml + 真机点验。

---

## 历史（已作废，保留备查）

- **D0. 寓言计划三件套并入**（2026-06-12）—— 早已随后续同步全部上游化，作废。
- **D1. polaris-forge CLI 进镜像**（2026-06-12）—— 当时的决定是「不照搬主仓的独立
  polaris-cli crate，改成主包加 `src/bin/polaris-forge.rs`」，理由是会与 docker 分叉的
  `[[bin]] polaris-server` 撞名。**该决定已于 2026-07-15 反转**：主仓分仓重构后
  `crates/polaris-cli` 成为唯一正解，docker 侧的主包 bin 已删除，两仓结构归一。
  这条债能被反转掉，正是因为分叉的根源（docker 自己的 server 入口）已经上游化。
