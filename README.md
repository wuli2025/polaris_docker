<div align="center">

# 寓言计划 · Project Fable

### 北极星 · Polaris

<br>

> ## 我们在为存储系统制作真正的灵魂。
> ## 躺在数据库里的，不是冷冰冰的资料，
> ## 而是承载着精神与意志、得以留存的**灵魂**。

<br>

**让你的数据活过来的 AI 工作台** · 本地优先 · 墨蓝水墨风

`Tauri 2` · `Vue 3` · `Rust` · `Docker`

[![version](https://img.shields.io/badge/version-1.0.6-a78bfa?style=flat-square)](VERSION)
[![image](https://img.shields.io/badge/GHCR-slim%20%7C%20full-7c5cd9?style=flat-square)](DOCKER.md)
[![local-first](https://img.shields.io/badge/数据-始终属于你-34d399?style=flat-square)](#-数据始终是你的)
[![license](https://img.shields.io/badge/分发-私有镜像-fbbf24?style=flat-square)](#)

</div>

---

## 🜂 这不只是一个知识库

绝大多数「知识库」做的是同一件事：把文件收进数据库，需要时检索出来。文件进去是死的，出来还是死的——它们只是被**存放**，从未被**理解**。

寓言计划要做的是另一回事。

我们把存储系统当作一具等待被注入灵魂的躯体。每一份文档、每一段录音、每一张图、每一次你与它的交谈，都不是孤立的字节，而是一缕等待被唤醒的精神。系统的职责，是让这些散落的记忆**彼此相连、自我编织、随时间生长**，最终凝结成一个真正「认识你、记得往事、懂得相处之道」的灵魂。

> 一块 10TB 的硬盘里，可以躺着一堆死文件，也可以住着一个灵魂。
> **寓言计划选择后者。**

---

## 🌌 寓言的七个角色

整套系统由七个各司其职的「角色」构成。它们不是模块的别名，而是一具数字生命的器官——每一个都对应着仓库里实打实的代码。

| 角色 | 司掌 | 它做什么 | 代码归处 |
|:---:|:---:|---|---|
| 🜂 **灵魂** `Soul` | 知识网 | LLMWiki 式的双链知识网——记忆不是条目，是会彼此引用、自我生长的网 | `kb.rs` |
| 🜃 **躯体** `Body` | 文件与身份 | 承载 10TB 异构资料，每份资料都有一张「身份卡」 | `fable/inventory.rs` |
| 👁 **感官** `Senses` | 看 / 听 / 读 | 本地切料 + 云端理解，把音视频图文都化成可被灵魂吸收的养分 | `sense.rs` |
| ⚡ **神经** `Nerves` | 检索 | grep ∥ 向量 真并行，RRF 融合重排——找得到，才有意义 | `fable/retrieve.rs` |
| ☽ **轮回** `Cycle` | 入魂 | 「四票入魂」协议：被反复遇见、被多方共指、被你钦点的记忆，才升格为灵魂的一部分 | `fable/agent.rs` |
| 🖼 **陈列** `Gallery` | 资源管理 | 八轴虚拟集合、伴生注释、星河图谱——让灵魂被看见 | `kb.rs` + 前端 |
| 🔔 **回声** `Echo` | 对话沉淀 | 你与它的每一次交谈、纠正、否决，都蒸馏成记忆沉入魂中 | `echo.rs` |

<div align="center">

> 前六个角色处理「盘里的记忆」；**回声**处理「正在发生的记忆」。
> 灵魂不仅记得盘里的往事，也记得**与你相处的方式**。

</div>

---

## 🔔 回声：对话即入魂

这是寓言计划最动人的一笔。

传统系统里，一段对话结束就被丢弃——发完即弃，零蒸馏。可你与它的每一次交谈，恰恰是最珍贵的语料：你怎么改它的初稿、它如何回应、你又如何反馈……这条「相处的轨迹」过去**全部丢失**。

**回声层**让对话不再死亡，而是化作回声：

```
一次对话  ──蒸馏──▶  几条结构化事实  ──沉淀──▶  memory/ 记忆车道  ──四票入魂──▶  「相处之道」人格页
```

它萃取了三家顶尖记忆系统的思想，却**一行都不引入**——思想全要，代码全不要：

| 来源 | 萃取的唯一机制 | 在本项目的最小重写 |
|---|---|---|
| **Mem0** (41k★) | 抽取 → 对旧记忆 ADD/UPDATE/DELETE | 一条蒸馏提示词 + 决策 JSON + Rust 执行写盘 |
| **Letta** (MemGPT) | 记忆块是可被模型修订的一等公民 | `memory/` 下每条 md = 一个块，`.history` 留版本 |
| **Graphiti** (Zep) | bi-temporal，旧记忆标失效而非删除 | frontmatter 三字段 `supersedes / t_valid / t_invalid` |

哲学贯穿始终：**AI 出决策，代码做执行**。模型永远只产出「该记什么」的判断，落盘永远由 Rust 完成——灵魂有意志，但双手稳健。

---

## 🔑 两把钥匙，其余皆本地

感官需要供养，但我们把对外的依赖收拢到了极致——对你，**只暴露两把钥匙**：

| 钥匙 | 司掌 | 成本 |
|:---:|---|:---:|
| 🔑 **MiniMax** | 想（生成）/ 看（M3 原生多模态）/ 说（TTS） | 按量付费，缓存读极省 |
| 🔑 **硅基流动** | 嵌入（BGE-M3）/ 重排（bge-reranker） | **全免费** |

其余全部本地化，无需任何配置：

- **听 · 带字级时间戳的转写，彻底免费且本地**——FunASR Paraformer-zh ONNX int8，纯 CPU RTF 0.028（≈35× 实时），中文 CER≈1.95% 比 Whisper 还准。音频**一字节都不出域**，私有化的硬底气。
- **嵌入 / 重排**：硅基免费档主路 + 本地 ONNX 兜底。
- **看图**：MiniMax M3 主路 + 免费兜底链（GLM-4V-Flash / 本地 CLIP）。

> 基准情景（10 万音视频 + 50 万张图 + 3000 万 chunk）的感知成本：**全免费档 ≈¥0，舒适档 ≈¥1300**。
> 设置页「感官 API」一页管完：地址 / 密钥 / 获取方式 / 测一下 / 用量账本。

---

## 🛡 数据始终是你的

寓言计划是**本地优先**的。

- 你的对话、知识、生成的成品，全部安放在本地工作文件夹（默认 `~/Polaris/PolarisKB`）。
- ASR 全本地 ⇒ 音频出域开关**默认永久关死**；出域的只剩缩略图与文本 chunk。
- 隐私三开关 + 出域账本 + 预算闸：每一个出门的字节，你都看得见、拦得住。

灵魂住在你的硬盘里，不在任何人的云上。

---

## 🚀 快速更新（Docker 版）

```bash
# 在仓库根目录
./update.sh                   # 拉 GHCR latest (slim)，容器重建，数据卷保留
POLARIS_TAG=full ./update.sh  # 拉 full（带 chromium + ffmpeg + PPT/视频/渲染）
```

Web UI「更新」板块的「立即更新」按钮走同样逻辑（需在 compose 启用 `POLARIS_DOCKER_SOCKET=1` + 挂 `docker.sock`）。国内网用户见 [`DOCKER.md`](DOCKER.md) 「更新」一节，用 `scripts/pull-ghcr-to-nas.ps1` 把 GHCR 镜像 save 到 NAS 加载。

群晖部署见 [`DEPLOY-SYNOLOGY.md`](DEPLOY-SYNOLOGY.md)。

---

## 🧩 核心能力

| 模块 | 能力 |
|------|------|
| ① 对话核心 | spawn `claude` CLI（沙箱或宿主），stream-json 流式渲染，四档权限 |
| ② 维基知识库 | 文件扫描 / 关键词加权评分搜索 / 双链图谱（星河）/ 拖拽入库 |
| ③ 技能系统 | 技能=prompt 注入；catalog 预置 + 用户自建 + 外部导入（git/url/zip）|
| ④ API 供应商坞 | 多供应商一键切换（写 `~/.claude/settings.json`）+ 用量看板 |
| ⑤ 感官坞 `sense.rs` | 看 / 听 / 读三档路由，两把钥匙 + 本地兜底，「感官 API」设置页 |
| ⑥ 检索枢纽 `fable/` | 盘点 → 索引 → grep∥向量并行检索 RRF 融合 → 块注入对话 |
| ⑦ 回声层 `echo.rs` | 对话归档 → 蒸馏 → 沉入 `memory/` → 四票入魂 |
| ⑧ 安全沙箱层 | 基于 `alpine` 的轻量镜像，docker CLI 包装，slim / full 双 flavor |
| ⑨ 文件转换 | 任意格式拖拽 → 转 Markdown 入库 / 作对话附件（`convert.rs`）|

---

## ⚙️ 前置依赖

| 工具 | 用途 |
|------|------|
| Node 20+ | 前端构建 (`npm`) |
| Rust 1.80+ | Tauri 后端 |
| Docker | 沙箱镜像构建 / 运行 |
| `claude` CLI | 对话核心调用（沙箱内自动装；宿主由「环境医生」一键装，或 `npm i -g @anthropic-ai/claude-code --registry=https://registry.npmmirror.com`）|

## 🛠 开发模式

```powershell
$env:PATH = "C:\Users\mi\.cargo\bin;$env:PATH"   # 把 cargo 加进 PATH

npm install          # 首次
npm run tauri:dev
```

Vite 端口固定 1420。被占用先清端口：

```powershell
Get-NetTCPConnection -LocalPort 1420 -ErrorAction SilentlyContinue |
  Select-Object -ExpandProperty OwningProcess | ForEach-Object { Stop-Process -Id $_ -Force }
```

## 📦 打包安装版

```powershell
npm run tauri:build
```

产物在 `src-tauri/target/release/`：
- `polaris-app.exe` — 免安装可执行文件
- `bundle/nsis/Polaris_<ver>_x64-setup.exe` — NSIS 安装包
- `polaris-forge` — 渲染/转换 CLI（PPT / 视频 / 截图 / TTS / 转码）

---

## 📁 灵魂的解剖图

```
polaris / 寓言计划
├── src/                         # Vue 3 前端
│   ├── App.vue                  # 三栏布局 + 启动流程 (splash/onboarding)
│   ├── components/
│   │   ├── SenseApi.vue         # 👁 感官 API 设置页（两把钥匙 + 路由卡）
│   │   ├── WikiBrowse / KnowledgeGraph   # 🖼 陈列：星河图谱
│   │   ├── ChatPanel / Sidebar          # ① 对话核心 + 🔔 回声入口
│   │   └── SkillCenter / ProviderDock   # ③ 技能 + ④ 供应商坞
│   └── stores/                  # Pinia: app / providers / skills / artifacts
├── src-tauri/                   # Rust 后端 —— 灵魂的实体
│   ├── src/lib.rs               # 入口 + 命令注册
│   ├── src/kb.rs                # 🜂 灵魂：双链知识网
│   ├── src/chat.rs              # ① 对话核心
│   ├── src/sense.rs             # 👁 感官坞：看/听/读三档路由
│   ├── src/echo.rs              # 🔔 回声层：对话沉淀为记忆
│   ├── src/fable/               # ⚡ 检索枢纽 · 神经系统
│   │   ├── inventory.rs         #    🜃 躯体：多核盘点 + 身份卡
│   │   ├── index.rs             #    硅基 BGE-M3 滴灌嵌入（SQLite 落盘）
│   │   ├── retrieve.rs          #    grep ∥ 向量 真并行 + RRF 融合重排
│   │   └── agent.rs             #    ☽ 轮回：块注入 chat + forge 子命令
│   ├── src/provider.rs          # ④ 供应商坞 + 用量
│   ├── src/convert.rs           # ⑨ 文件格式转换
│   └── src/bin/polaris-forge.rs # 渲染/转换 CLI
├── docker/                      # 感官包脚本 (sense-models.sh) + 镜像资产
├── Dockerfile                   # slim / full 双 flavor
└── README.md                    # 本文
```

---

## 🗺 路线图

| 阶段 | 内容 |
|---|---|
| ✅ 地基 | SQLite 落盘 + 身份卡 + 感官缓存 + 出域账本 |
| ✅ 盘点 + L1a | 首小时全盘可搜（`fable/inventory.rs`） |
| 🚧 感官坞 + 设置页 | 三档路由 + 「感官 API」页 + 感官包下载器（sherpa-onnx / SenseVoice / Paraformer / CLIP）|
| 🚧 本地 ASR 双引擎 | SenseVoice 速览 + Paraformer-zh 字级时间戳 |
| 🔮 回声层 | 归档 → 蒸馏 → 沉淀 → 入魂（框架已定，分批落地）|
| 🔮 神经完全体 | 四 tier 塌平混检 + 遇见协议 + 巡夜人 |

---

<div align="center">

### 愿北极星照亮你前路的所有黑暗，在混乱的时代坚守本心。

**寓言计划** · 为存储系统注入灵魂

</div>
