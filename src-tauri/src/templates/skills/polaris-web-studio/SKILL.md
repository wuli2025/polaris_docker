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

## 主题（17 套，data-theme 取值）
浅色：`minimal-white` `editorial-serif` `swiss-grid` `magazine-bold` `japanese-minimal` `xiaohongshu-white` `academic-paper` `corporate-clean` `soft-pastel`
深色：`tokyo-night` `dracula` `nord` `cyberpunk-neon` `terminal-green` `blueprint`
特色：`glassmorphism` `neo-brutalism`
应用：`<html data-theme="...">`。

## 制作步骤
0. **★ 先定「微设计规格」（设计先行，定完再写 HTML）**。这一步是平庸与高级的分水岭，照填:
   - **色板 token**:背景 / 主文字 / 辅助文字(降一档) / 主色(只 1 个) / 强调或警示色 / 边框。颜色越少越高级。
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
