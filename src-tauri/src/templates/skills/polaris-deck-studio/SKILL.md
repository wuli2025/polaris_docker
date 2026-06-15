---
id: polaris-deck-studio
name: Polaris 演示工坊（PPT / 网页幻灯片）
description: 把文案或文档做成有设计感的幻灯片。一套引擎两种交付：自包含可翻页的网页 deck(.html)，或像素级还原主题的 .pptx。内置 17 套主题(借力 open-design)，键盘翻页/演讲者备注/打印 PDF。
source: official
author: Polaris
created_at: 0
---

# Polaris 演示工坊

> 输入一段文案或一份文档 → 选一套主题 → 输出一份**好看**的演示。
> 三种交付物：
> - **网页幻灯片**：一个自包含 `.html`，可翻页、可全屏、可打印为 PDF、可直接分享。
> - **传统 PPT（spec 路线，推荐）**：写一份 `polaris.slides.json` 结构化 spec → `polaris-forge spec-pptx` 确定性转换成**原生 100% 可编辑**的 `.pptx`（真文本框/真形状/真项目符号，零浏览器依赖）。
> - **网页 deck 导出 PPT**：deck.html 逐页「无字背景截图 + 真文本框」分层导出，视觉像素级还原主题，文字仍可编辑。

技能资源目录（已随 App 落盘）：`~/Polaris/skills/polaris-deck-studio/`
```
assets/base.css      幻灯片引擎 + 设计 token（来自 open-design，MIT）
assets/themes.css    17 套主题（[data-theme] 属性选择器）
assets/runtime.js    翻页 / 主题切换(T) / 概览(O) / 全屏(F) / 打印(P) / #/N 深链
templates/deck.html  起始模板（含 5 页示例 + 动画用法）
scripts/install-deps.mjs   装 playwright + pptxgenjs（仅 PPT 导出需要；只装库，禁浏览器自动下载）
scripts/export-pptx.mjs    deck.html → .pptx（逐页截图，整版图嵌入）
scripts/find-browser.mjs   定位本机/自带浏览器给 Playwright（不下载；与 Rust find_chromium 同链）
```

---

## 调用方式（前端会传一段「制作配置」）

「演示工坊」面板会在提示词里给出：
- **输出模式**：`html`（网页幻灯片）或 `pptx`（PPT）
- **主题 id**：见下表（或 `auto` = 你自行挑最合适的）
- **页数上限 / 画幅比例 / 信息密度**
- **正文**：直接粘贴的文案，或上传文件的绝对路径（先 `Read` 它们）
- **产物目录**：最终文件要保存到这里，并在回答末尾列出绝对路径

没有上述配置时（用户在普通对话里直接说「做个 PPT/网页演示」），用合理默认：主题走 **`auto`（高级感）**、16:9、≤12 页、中等密度、输出 `html`。

### ★ 主题 = `auto`（即 UI 的「AI 自由发挥」）= 默认高级感
`auto` **不是**「随便挑一个」，而是**默认做出一眼高级、有感染力的观感**：
- **优先深色 / 质感主题**，**不要默认白底**。首选：`aurora`（极光渐变辉光）、`glassmorphism`（毛玻璃）、`pitch-deck-vc`（融资路演）、`vaporwave`（蒸汽波）、`cyberpunk-neon`（赛博霓虹）、`tokyo-night`（东京夜）。
- 配方：**深底 + 渐变强调色（`.gradient-text` 用在关键词上）+ 超大标题（封面 `.h1` 可到 110–160px）+ 克制留白 + 大数字金句页**。少字、字大、一页一事。
- 仅当内容**明显属于**学术 / 公文 / 财报 / 法务等需要素白严肃的场景，才退回浅色主题（如 `academic-paper`、`corporate-clean`、`minimal-white`）。
- 用户填了「自定义风格补充」时以其为准（如「黑金高级」→ 在深色主题上叠加金色强调）。

---

## 主题（36 套，data-theme 取值）

| 分组 | id |
|---|---|
| 高级感首选（深色/质感） | `aurora` `glassmorphism` `pitch-deck-vc` `vaporwave` `cyberpunk-neon` `tokyo-night` |
| 深色 | `dracula` `nord` `terminal-green` `blueprint` `catppuccin-mocha` `gruvbox-dark` `retro-tv` `rose-pine` |
| 浅色 | `minimal-white` `editorial-serif` `swiss-grid` `magazine-bold` `japanese-minimal` `xiaohongshu-white` `academic-paper` `corporate-clean` `soft-pastel` `arctic-cool` `bauhaus` `catppuccin-latte` `engineering-whiteprint` `midcentury` `news-broadcast` `sharp-mono` `solarized-light` `sunset-warm` |
| 特色 | `neo-brutalism` `memphis-pop` `rainbow-gradient` `y2k-chrome` |

应用主题 = 在 `<html data-theme="aurora">`。运行时按 `T` 可循环切换预览。

---

## 制作步骤

### 1. 规划内容 → 分页
把正文拆成「一页一个信息点」的结构。好演示的铁律：**每页只讲一件事，字少、字大、留白多**。封面 / 要点列表 / 大数字金句 / 两栏对比 / 结尾，是最常用的页型。演讲者要说但观众不该看到的内容，放进 `<div class="notes">…</div>`（默认隐藏，按 `S` 在演讲者视图看）。

### 2. 用引擎写 deck.html
照 `templates/deck.html` 的骨架写。核心约定（全在 `base.css` 里）：
- 容器 `<div class="deck">`，每页一个 `<section class="slide" data-title="...">`
- 版式原语：`.grid .g2/.g3/.g4`、`.row`、`.card`/`.card-accent`/`.card-hover`、`.pill`、`.lede`、`.kicker`、`.gradient-text`、`.center`
- 标题：`.h1`/`.h2`/`h1.title`/`h2.title`/`.h3`
- 动画：元素加 `class="anim-fade-up"`（或 `anim-fade/anim-zoom/anim-slide-left/anim-slide-right`）；列表容器加 `anim-stagger-list`，子项设 `style="--i:0/1/2…"` 做错峰入场
- 页脚/进度/概览：`<div class="deck-footer"><span class="slide-number"></span></div>`、`<div class="progress-bar"><span></span></div>`、`<div class="overview"></div>`

### 3. ★ 做成自包含单文件（两种模式都这么做）
**把 `assets/base.css` 与 `assets/themes.css` 的内容内联进 `<style>`，把 `assets/runtime.js` 内联进 `<script>`**，删掉对 `../assets/*` 的外链。这样产出的 `deck.html` 是**单文件**，可独立分享、可被截图导出、不依赖技能目录。读取这三个文件：
```bash
cat ~/Polaris/skills/polaris-deck-studio/assets/base.css
cat ~/Polaris/skills/polaris-deck-studio/assets/themes.css
cat ~/Polaris/skills/polaris-deck-studio/assets/runtime.js
```
把 deck.html 存到**产物目录**（文件名如 `演示-<主题>.html`）。

### 4a. 模式 = html（网页幻灯片）
到此就完成了。在回答末尾给出 `deck.html` 的绝对路径，并说明：双击用浏览器打开；`←/→/空格` 翻页、`F` 全屏、`O` 概览、`T` 换主题、`P`/`Ctrl+P` 导出 PDF。

### 4b. 模式 = pptx · ★ 传统 PPT（spec 路线，首选）
**不写 deck.html**。把内容编排成一份 `polaris.slides.json`（结构化 spec），存到产物目录，再用 Polaris 自带 CLI 一步转换成**原生 100% 可编辑**的 .pptx（真文本框/真形状/真项目符号，零浏览器、零 npm 依赖）：
```bash
polaris-forge spec-pptx --spec="<产物目录>/polaris.slides.json" --out="<产物目录>/演示.pptx"
# CLI 位于 ~/Polaris/bin/（Windows: %USERPROFILE%\Polaris\bin\polaris-forge.exe），Docker 镜像已内置在 PATH。
# CLI 不存在时：把 spec 按上述文件名存好即可，Polaris 桌面端会自动完成转换。
```

#### polaris.slides.json 格式（v1，严格遵守）
```json
{
  "version": 1,
  "theme": "minimal-white",
  "slides": [
    {"layout":"title",   "kicker":"眉题(可选)", "title":"主标题", "subtitle":"副题(可选)", "notes":"口播稿(可选,进PPT备注页)"},
    {"layout":"section", "kicker":"PART 1", "title":"章节名"},
    {"layout":"bullets", "title":"页标题", "points":["要点", {"text":"要点", "sub":["子点","子点"]}]},
    {"layout":"two-col", "title":"对比页", "left":{"head":"栏头","points":["…"]}, "right":{"head":"栏头","points":["…"]}},
    {"layout":"compare", "title":"多卡对比", "items":[{"head":"卡头","body":"卡内文,可\n多行"}, {"head":"…","body":"…"}]},
    {"layout":"stats",   "title":"关键数据", "items":[{"value":"83%","label":"指标名","desc":"补充说明(可选)"}]},
    {"layout":"timeline","title":"路线/流程", "steps":[{"head":"步骤名","body":"一句话说明(可选)"}]},
    {"layout":"quote",   "text":"金句", "by":"出处(可选)"},
    {"layout":"closing", "title":"结尾(默认:谢谢)", "subtitle":"…"}
  ]
}
```
- `theme` 六选一：`minimal-white`(近白暖米/默认) `ink-gold`(黑金) `deep-space`(深空蓝) `warm-paper`(暖纸) `forest`(森绿) `tech-blue`(科技蓝)。按用户所选主题的气质就近映射。
- 写作要领：标题短、要点凝练（每点 ≤ 28 字）、一页一事；`compare` 卡片 2–4 个、`stats` 大数字 1–4 个、`timeline` 步骤 2–5 步；演讲内容写进每页 `notes`（用户在 PowerPoint 备注栏直接拿到口播稿）。
- **★ 版式要混排，这是观感的关键**：整份 PPT **严禁通篇 bullets**——按信息类型选版式：开场 `title`、分章 `section`、数据冲击 `stats`、流程/路线 `timeline`、双方对照 `two-col`、多项并列 `compare`、点睛 `quote`、要点才用 `bullets`。一份 10 页的演示至少应出现 4 种不同版式。
- 画幅固定 16:9。产出后回答末尾给出 `.pptx` 和 `polaris.slides.json` 的绝对路径。

### 4c. 网页 deck → PPT（要像素级主题视觉时用）
已写好自包含 deck.html 后（如用户先要了网页版又要 PPT）：
```bash
polaris-forge pptx --deck="<产物目录>/演示-<主题>.html" --out="<产物目录>/演示-<主题>.pptx" --width=1920 --height=1080
```
分层导出：每页先提取文本框（坐标/字号/颜色），背景按「隐藏文字」重新截图 → 真文本框叠在无字背景上 = **视觉还原 + 文字可编辑**（挪开文字无重影）。需要环境里有 chromium/Chrome/Edge（CLI 自动探测；Docker 需 full 镜像）。
CLI 不可用时的旧路（Node，最后手段）：先 `node ~/Polaris/skills/polaris-deck-studio/scripts/install-deps.mjs`（只装 JS 库，浏览器用本机 Edge/Chrome，**不会自动下载 chromium**），再跑 `scripts/export-pptx.mjs --deck=… --out=… --width=1920 --height=1080`（整版图嵌入，文字不可编辑）。浏览器由 `find-browser.mjs` 自动定位；缺浏览器就走 Ctrl+P 打印 PDF 兜底。

---

## 兜底（依赖缺失也不能卡死）
- 传统 PPT spec 路线**没有外部依赖**，写好 spec 就赢了一半：CLI 不在 → 存好 `polaris.slides.json`，Polaris 桌面端会自动转换。
- deck 截图路缺 chromium / `npm` 装不上 → 改让用户用浏览器打开 deck.html 后 **`Ctrl+P` → 另存为 PDF**（`base.css` 已含 `@media print` 分页，每页一张）；或退传统 PPT spec 路线（牺牲主题精确视觉，换 100% 可编辑）。
- 始终给出已经成功产出的那份文件的绝对路径，别让用户两手空空。

## 画幅
默认 16:9（导出用 1920×1080）。若用户要 4:3，截图用 `--width=1440 --height=1080`，并把 `export-pptx.mjs` 里 `defineLayout`/`addImage` 的 13.333×7.5 改为 10×7.5（脚本注释处）。
