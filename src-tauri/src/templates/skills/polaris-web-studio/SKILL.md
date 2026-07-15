---
id: polaris-web-studio
name: Polaris 网站生成（落地页 / 单页站点）
description: 把需求或文案做成一个有设计感、响应式的网站 HTML（自包含单文件）。借力 open-design 风格：玻璃导航 + 渐变大标题 + bento 功能区 + 数据 + 价格卡 + CTA + 页脚，17 套主题，滚动揭示动效。
source: official
author: Polaris
created_at: 0
---

# Polaris 网站生成

> 输入需求/文案 → 选风格与站点类型 → 输出一个**自包含、响应式**的网站 `.html`（双击即开、可直接部署/分享）。
> 设计语言借鉴 open-design：玻璃态吸顶导航、渐变大标题、bento 卡片网格、数据条、价格卡、CTA 横幅、多列页脚、滚动揭示动效。

技能资源目录（已随 App 落盘）：`~/Polaris/skills/polaris-web-studio/`
```
assets/site.css     网站组件库（nav/hero/bento/stats/pricing/cta/footer/btn，响应式）
assets/themes.css   17 套主题（[data-theme] 属性选择器，与 PPT 演示同源）
assets/runtime.js   滚动揭示(.reveal→.in) + T 键预览换主题
assets/motion.css   高级动效层（神经网络背景/鼠标光晕/进度条/逐字/数字滚动，可选）
assets/motion.js    高级动效运行时（零依赖、自动降级；data-motion / data-kinetic / data-count 触发）
templates/site.html 起始模板（完整一页站点骨架）
```

## 调用方式（前端会传一段「网站配置」）
- **站点类型**：`landing`(产品落地页) / `portfolio`(作品集) / `product`(SaaS 介绍) / `blog`(博客首页) / `event`(活动)
- **主题 id**：见下（或 `auto` 自挑）
- **品牌名 / 主张**、**正文/需求**（或上传文件绝对路径，先 Read）
- **产物目录**：最终 `.html` 存这里，回答末尾列绝对路径

没有上述配置时（用户在普通对话里直接说「做个网站 / 落地页 / 做成 HTML 页面」——工坊面板入口已隐藏，**对话触发就是主路径**），用合理默认：类型按措辞判断（默认 `landing`）、主题走 **`auto`** —— `auto` 不是随便挑，而是**默认高级质感**：优先 `glassmorphism`/`tokyo-night`/`cyberpunk-neon` 等深色/质感主题 + 丰富色彩层次；仅明显严肃场景（政企/学术/公文）退浅色。品牌名/主张从用户内容里提炼；产物存到当前工作目录并给绝对路径。**质量标准不因来自普通对话而降档**。

## 主题（17 套，data-theme 取值）
浅色：`minimal-white` `editorial-serif` `swiss-grid` `magazine-bold` `japanese-minimal` `xiaohongshu-white` `academic-paper` `corporate-clean` `soft-pastel`
深色：`tokyo-night` `dracula` `nord` `cyberpunk-neon` `terminal-green` `blueprint`
特色：`glassmorphism` `neo-brutalism`
应用：`<html data-theme="...">`。

## 制作步骤
0. **★ 先选设计师（本工坊的灵魂），再定「微设计规格」**。
   - 读 `designers/INDEX.md`（11 位设计师花名册 + auto 路由表，随本技能已落盘在技能目录）。用户指定就用指定的；没指定按路由表按内容气质自动请一位（SaaS/AI 官网→弥散光大师、功能汇总→便当格大师、开发者/数据面板→玻璃酥大师、消费种草→小红书大师、创意招聘→波普糖大师…判断不了用发布会大师兜底）。
   - 读该设计师 `designers/<id>.md` 全文，照它第 10 节「实现映射」的 web 部分起手（`data-theme` + token 覆写 + 区块顺序 + motion 开关），并守它的色板/字阶/装饰/禁忌。「拿手三套系」用户可再挑一套，没挑用第一套。
   - **读 `designers/_foundation/taste.md`（10 条工艺纪律）**：按设计师 frontmatter 的默认拨盘（±1）设定 V/M/D 三拨盘，输出一行设计判读（T2 格式）；写完后跑文末 Pre-Flight 清单（web 特供节必查），任何一条打不了勾即返工；产物 HTML **首行**写遥测锚点 `<!-- designer: <id> · dials V/M/D · preflight n/n -->`。
   - **改已有页面走 T9 重设计协议**：用户说「改版/美化/重新设计」时先定模式（新建/保品牌演进/推翻重来），保品牌演进要先审计旧页的品牌 token 与信息架构再动手，URL/锚点、导航文案、表单字段**永不静默更改**。
   - 然后再照下面填「微设计规格」——设计师定了大方向，这步定本次的具体数值。这一步是平庸与高级的分水岭，照填:
   - **色板 token**:背景 / 主文字 / 辅助文字(降一档) / 主渐变(2–3 个相邻或互补色相,Hero 大标题 `.gradient-text` 与 `.btn-grad` 用) / 1–2 个点缀强调色(数据/图标/pill) / 边框。**色彩要丰富有层次**——高级感来自色相之间的呼应与明度控制,不是把颜色砍到只剩一个;每个区块可做 accent 微变奏(功能区/数据区/价格区各自轻微偏移),整页像一套策划过的系列视觉。丰富落在装饰层(渐变、辉光、色块、图形),**文字与背景的对比度铁律不破:深底浅字、浅底深字,小字尤甚**。
   - **字阶**:超大标题 / 区块标题 / 正文 / 等宽数据,各一个字号+字重,**档差拉开**。
   - **间距**:区块间距、四周边距(≥屏宽 8%)、最大内容宽度。
   - **动效清单**:本次要用哪几个(逐字标题 / 数字滚动 / 卡片错峰揭示 / 神经网络背景 / 鼠标光晕)。深色站默认开,浅色严肃站克制。
   - **逐区块入场**:每个区块写一行「怎么进场、什么顺序」。
   - 铁律:**纯代码渲染 = 技术自信**,零图片素材也要靠 Canvas / CSS 渐变 / 大字排版撑住高级感。
1. **定信息架构**：按站点类型排版块顺序。落地页常用：导航 → Hero(大标题+主张+双 CTA+信任 pill) → 功能(bento) → 数据 → 价格 → CTA 横幅 → 页脚。作品集换成 项目网格；博客换成 文章卡片流。
2. **用 site.css 的组件写**（class 词表）：
   - 布局：`.container` `.section`/`.section.tight` `.grid .cols-2/3/4` `.bento`(内 `.card.wide/.tall`)
   - 文案：`.eyebrow` `.section-title` `.section-sub` `.gradient-text` `.lede`
   - 导航：`.nav>.nav-inner>(.brand,.nav-links,.btn)`（玻璃吸顶）
   - 区块：`.hero`、`.stats>.stat>(.num,.lbl)`、`.price-card(.featured)`、`.cta`、`.footer>.footer-grid`
   - 按钮/标签：`.btn .btn-primary/.btn-grad/.btn-ghost`、`.pill .pill-accent`
   - 动效：需要入场的元素加 `class="reveal"`（runtime 滚动时加 `.in` 淡入上移）
2.5 **高级动效（可选，深色站默认开 / 浅色严肃站默认关）**——这是追平一线落地页的关键，零依赖纯原生:
   - **全局背景/光晕/进度条**：在 `<html data-theme="..." data-motion>` 上加 `data-motion`，motion.js 会自动注入神经网络 Canvas 背景 + 鼠标跟随光晕 + 顶部滚动进度条。主色默认矩阵绿；可在主题/根样式设 `--motion-accent:#xxxxxx; --motion-glow:rgba(...);` 改色。
   - **逐字标题**：给 Hero 大标题加 `data-kinetic`（每个字会错峰滑入）。
   - **数字滚动**：给数据区的数字元素加 `data-count="5000000"`（可选 `data-suffix="%"`），进视口时从 0 滚到目标值。例：`<span class="num" data-count="95" data-suffix="%">0</span>`。
   - **降级已内置**：`prefers-reduced-motion` 时自动停 Canvas、动画直接落终值；粒子数按屏宽分档（≤80/120/180）。**别给学术/公文/暖色品牌站开**（粒子干扰阅读）。
3. **★ 做成自包含单文件**：把 `assets/site.css` + `assets/themes.css` 内联进 `<style>`、`assets/runtime.js` 内联进 `<script>`，删掉对 `../assets/*` 的外链。**启用了高级动效就再内联 `assets/motion.css`（进 `<style>`）+ `assets/motion.js`（进 `<script>`）**。读取：
   ```bash
   cat ~/Polaris/skills/polaris-web-studio/assets/site.css
   cat ~/Polaris/skills/polaris-web-studio/assets/themes.css
   cat ~/Polaris/skills/polaris-web-studio/assets/runtime.js
   cat ~/Polaris/skills/polaris-web-studio/assets/motion.css   # 仅启用动效时
   cat ~/Polaris/skills/polaris-web-studio/assets/motion.js    # 仅启用动效时
   ```
   存到产物目录（文件名如 `网站-<主题>.html`）。
4. 回答末尾给出 `.html` 绝对路径，说明：双击用浏览器打开；响应式；按 `T` 可预览换主题。

## 内容质量要求
- 文案具体、有信息量，别用「Lorem ipsum」占位；价格/数据用合理示意值并标注「示意」。
- 真·响应式：手机宽度下导航 links 自动隐藏、多列塌成单列（site.css 已含断点，别破坏）。
- 配图用 emoji 图标 / CSS 渐变块 / inline SVG，不要外链不存在的图片。

## 继续修改
用户可能发来「把价格改三档/换深色主题/加一段 FAQ/Hero 文案改成…」——**直接在原 .html 上改并覆盖保存，文件名不变**，末尾给绝对路径。
