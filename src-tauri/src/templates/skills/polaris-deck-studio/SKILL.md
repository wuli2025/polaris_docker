---
id: polaris-deck-studio
name: Polaris 演示工坊（PPT / 网页幻灯片）
description: 把文案、文档或讲稿做成有设计感的演示。主路线是原生可编辑的传统 PPT：模型只出 spec(polaris.slides.json)决策版式与内容，Polaris 引擎确定性落 OOXML——真文本框、真形状，PowerPoint/WPS/Keynote 里 100% 可改；11 种固定版式 + freeform 自由版式（9 类盒子/坐标轴/click 单击动画）、6 套色板、每页口播备注，还可切 engine:python 走 python-pptx。次路线是自包含可翻页的网页 deck(.html)：36 套主题、键盘翻页/演讲者备注/打印 PDF、高级动效。
source: official
author: Polaris
created_at: 0
---

# Polaris 演示工坊

> 输入一段文案、一份文档或一份讲稿 → 编排成结构化 spec → 输出一份**原生可编辑**的演示。
> 一套技能两条路线：
> - **传统 PPT（spec 路线 · 主路线，推荐）**：你只写一份 `polaris.slides.json`（决策版式与内容），`polaris-forge spec-pptx` 确定性转成**原生 100% 可编辑**的 `.pptx`——真文本框/真形状/真项目符号，零浏览器依赖。用户拿到能直接改字换色挪位置，这是本工坊的护城河。
> - **网页 PPT（HTML-deck 路线 · 次路线）**：一个自包含 `.html`，可翻页、可全屏、可打印为 PDF、可直接分享；还能把 deck.html 逐页「无字背景截图 + 真文本框」分层导出成视觉像素级还原主题的 `.pptx`。

技能资源目录（已随 App 落盘）：`~/Polaris/skills/polaris-deck-studio/`
```
（传统 PPT / spec 路线用）
designers/           11 位设计师人格 + 美学地基(_foundation) + 花名册(INDEX.md)
（网页 PPT / HTML-deck 路线用）
design.md            ★网页 deck 设计规范(对比度铁律/颜色纪律/防压字)，写网页 deck 前必读
assets/base.css      幻灯片引擎 + 设计 token（来自 open-design，MIT）
assets/themes.css    36 套网页主题（[data-theme] 属性选择器）
assets/runtime.js    翻页 / 主题切换(T) / 概览(O) / 全屏(F) / 打印(P) / #/N 深链
assets/motion.css    高级动效样式（仅网页 deck）
assets/motion.js     神经网络背景 / 逐字标题 / 数字滚动（仅网页 deck）
templates/deck.html  网页 deck 起始模板（含 5 页示例 + 动画用法）
scripts/install-deps.mjs   装 playwright + pptxgenjs（仅 deck→PPT 截图导出需要；只装库，禁浏览器自动下载）
scripts/export-pptx.mjs    deck.html → .pptx（逐页截图，整版图嵌入）
scripts/find-browser.mjs   定位本机/自带浏览器给 Playwright（不下载；与 Rust find_chromium 同链）
```

## 核心约定（先读这段）

**传统 PPT = spec 路线，这是主路线，也是本工坊的护城河。** 走它你**不写 HTML、不截图、不调浏览器**，只产出一个 JSON：

```
polaris.slides.json  →  polaris-forge spec-pptx  →  演示.pptx
```

为什么这么设计：引擎手写 OOXML，产物是真文本框/真形状/真填充，用户拿到 .pptx 能直接改字换色挪位置。截图式 PPT（每页一张大图）做不到这点。副作用是纯文本模型即可驱动，无 chromium 的环境也能出 PPT。

**铁律**：spec 是唯一真源。预览、导出、继续修改全部基于它。改稿就改 spec 再重转，**绝不**另起新文件。

**走哪条路线**：用户说「网页 / 网页版 / html / 可翻页 / 分享链接」→ 走**网页 PPT（HTML-deck，见第七节）**；其余一律走**传统 PPT（spec 路线，第一～六节）**——包括只说「做个 PPT / 做份演示 / 幻灯片 / 可编辑 / PowerPoint」。拿不准就走传统 PPT。

**前端「制作配置」**：「演示工坊」面板会在提示词里给出——**输出模式**（`pptx` / `html`）、**主题 id**、**页数上限 / 画幅比例 / 信息密度**、**正文**（直接粘贴的文案，或上传文件的绝对路径，先 `Read` 它们）、**产物目录**（产物存到这里，回答末尾列出绝对路径）。没有面板配置时（普通对话直接说「做个 PPT / 做份演示」——工坊面板入口已隐藏，**对话触发就是主路径**）按上面的路线判断走 spec 还是 html，**质量标准不因来自普通对话而降档**。

---

## 一、传统 PPT · spec v1 格式（主路线 · 权威，与引擎逐字对齐）

```json
{
  "version": 1,
  "theme": "minimal-white",
  "slides": [
    {"layout": "title", "kicker": "…", "title": "…", "subtitle": "…", "notes": "…"}
  ]
}
```

顶层三个键：`version` 恒为 `1`；`theme` 取下面 6 套色板之一；`slides` 是页数组，**最多 300 页**，不能为空。
**每页都可带 `"notes": "…"`** → 写进 PowerPoint 的演讲者备注页（口播稿/讲述提示，观众看不到，投影也不显示）。多行用 `\n`。

### 6 套色板（`theme` 取值）

| id | 气质 | 适用 |
|---|---|---|
| `minimal-white` | 近白暖米 + 赭金强调（**默认**） | 最稳的传统 PPT 气质，公开课/汇报通吃 |
| `warm-paper` | 暖纸米黄 + 赭橘 | 语文/历史/人文,有纸感 |
| `forest` | 浅绿白 + 森绿 | 生物/地理/环境 |
| `tech-blue` | 白 + 亮蓝 | 数理化/信息技术/工作汇报 |
| `ink-gold` | 墨黑 + 暗金（深色） | 高年级/讲座/发布会式 |
| `deep-space` | 深蓝黑 + 蓝紫（深色） | 天文/科技/未来感 |

写错或未知色板 → 静默回退 `minimal-white`，但会进 warnings。**大小写敏感,照抄 id。**

### 11 种固定版式（+ `freeform` 自由版式）

字段表如下。**未列出的字段引擎不读**，写了也是白写；`layout` 缺失或未知 → 降级按 `bullets` 渲染（进 warnings）。

#### `title` / `closing` — 封面 / 结尾
```json
{"layout": "title", "kicker": "八年级下·物理", "title": "浮力", "subtitle": "第一课时　阿基米德原理"}
```
- `kicker` 小字眉标（强调色，可选）、`title` 主标题、`subtitle` 副标题（可选）
- `closing` 字段与 `title` 完全相同；`closing` 省略 `title` 时自动填「谢谢」

#### `section` — 章节过渡页
```json
{"layout": "section", "kicker": "环节二", "title": "探究：浮力大小与什么有关"}
```
- 只有 `kicker`（可选）+ `title`。左侧有强调色竖条。**没有 subtitle，别写。**

#### `bullets` — 要点页（缺省版式）
```json
{"layout": "bullets", "title": "学习目标", "points": [
  "能说出浮力的定义",
  {"text": "会用弹簧测力计测浮力", "sub": ["称重法：F浮 = G - F示", "误差来源：读数与水面接触"]}
]}
```
- `points` 数组，每项两种写法：
  - **字符串** → 一级 bullet（`•`，强调色）
  - **对象** `{"text": "…", "sub": ["…", "…"]}` → 一级 bullet + 二级子条（`–`，弱化色，小 3pt）
- `points` 不是数组（比如误给了字符串）→ 整页内容被丢弃并进 warnings。**务必给数组。**

#### `two-col` — 左右分栏
```json
{"layout": "two-col", "title": "浮力 vs 重力",
 "left":  {"head": "浮力", "points": ["方向竖直向上", "来自液体压强差"]},
 "right": {"head": "重力", "points": ["方向竖直向下", "来自地球吸引"]}}
```
- `left` / `right` 各是 `{head, points}`：`head` 栏标题（强调色粗体，可选）、`points` 同 bullets 规则（支持 `sub`）
- 两栏各自渲染成一张圆角卡片。左右都空则整页空白。

#### `compare` — 并列对比卡（**2–4 张**）
```json
{"layout": "compare", "title": "三种测量方法", "items": [
  {"head": "称重法", "body": "先测重力\n再测浸入后示数", "points": ["最常用", "误差小"]},
  {"head": "排水法", "body": "测排开液体的重力"}
]}
```
- `items` 每项 `{head, body, points}`，三个都可选：
  - `head` 卡标题（强调色粗体）
  - `body` 正文，**`\n` 分行**，每行一段
  - `points` bullet 列表（同 bullets 规则）— *引擎支持但头注释没写，可放心用*
- **超过 4 张只渲染前 4 张**，其余丢弃并进 warnings。要 5 项以上就拆页。

#### `stats` — 大数字指标（**1–4 张**）
```json
{"layout": "stats", "title": "这节课的三个数", "items": [
  {"value": "9.8", "label": "N/kg", "desc": "本节取 g = 9.8"},
  {"value": "1×10³", "label": "水的密度", "desc": "kg/m³"}
]}
```
- `items` 每项 `{value, label, desc}`：`value` 超大强调色数字、`label` 名称（粗体）、`desc` 补充说明（弱化小字，可选）
- **超过 4 张只渲染前 4 张**并进 warnings。

#### `timeline` — 流程 / 步骤（**2–5 步**）
```json
{"layout": "timeline", "title": "探究步骤", "steps": [
  {"head": "提出问题", "body": "浮力大小跟什么有关？"},
  {"head": "猜想", "body": "可能跟排开液体体积有关\n也可能跟液体密度有关"}
]}
```
- `steps` 每项 `{head, body}`：`head` 步骤名、`body` 说明（**`\n` 分行**）
- 引擎自动编号（1,2,3…）画圆节点 + 连接线，**不要自己在 head 里写「1.」「第一步」**
- **超过 5 步只渲染前 5 步**并进 warnings。多了就拆成两页。

#### `quote` — 引语 / 金句
```json
{"layout": "quote", "text": "纸上得来终觉浅，绝知此事要躬行。", "by": "陆游"}
```
- `text` 引语正文（斜体大字）、`by` 出处（可选，引擎自动加「—— 」前缀，**别自己加破折号**）
- **没有 `title`**，别写。

#### `image-full` — 全幅配图 + 大标题（封面 / 情境页）
```json
{"layout": "image-full", "image": "D:/课件/img/spring.png",
 "kicker": "语文 · 二年级下册", "title": "找春天", "subtitle": "第一课时"}
```
- `image` 配图的**本地绝对路径**（png/jpg），引擎自动铺满整页并压一层半透明暗蒙版垫文字
- 其余字段同 `title`：`kicker` / `title` / `subtitle`
- 蒙版上的字**恒为白色**（不随色板变），所以配图**别用大面积浅色/高频细节**——中间留白的图最好

#### `image-text` — 图文分栏（讲解页主力）
```json
{"layout": "image-text", "image": "D:/课件/img/bud.png", "side": "left",
 "title": "春天藏在哪里", "head": "仔细找一找",
 "points": ["嫩芽 —— 春天的眉毛", {"text": "小溪 —— 春天的琴声", "sub": ["听：叮叮咚咚"]}]}
```
- `image` 同上；`side`：`"left"`（默认，图在左）或 `"right"`
- `title` 页标题、`head` 文字侧小标题（可选）、`points` 同 bullets 规则（支持 `sub`）

**配图的三条硬约定**：
1. **只有这两个版式吃 `image`**。在 bullets/compare/stats 上写 `image` → 忽略 + warning。
2. 图按 **cover** 填满图框（等比缩放 + 两侧对称裁切），**不会变形**。但极端长条图会被裁掉很多，配图请尽量接近目标画幅：`image-full` 用 `16:9`，`image-text` 用 `1:1` 或 `4:3`。
3. 图**必须先存在于磁盘**再写进 spec。路径错 / 图坏 → 该页降级成无图版式 + warning，不会中断出片，但你会得到一页平淡的字。

#### `freeform` — 自由版式（固定版式框不住时的出口）
固定 11 种版式排不出你要的效果时用它：一页里任意摆放盒子，坐标用 **1280×720 逻辑 px**（16:9 画布，`x` 向右、`y` 向下）。
```json
{"layout": "freeform", "boxes": [
  {"type": "scrim", "x": 0, "y": 0, "w": 1280, "h": 720, "color": "#000", "alpha": 30},
  {"type": "rect",  "x": 0, "y": 0, "w": 1280, "h": 10, "color": "accent"},
  {"type": "card",  "x": 80, "y": 120, "w": 500, "h": 420},
  {"type": "text",  "x": 110, "y": 150, "w": 440, "h": 120, "text": "自由标题",
   "size": 40, "color": "ink", "align": "ctr", "bold": true},
  {"type": "text",  "x": 110, "y": 300, "w": 440, "h": 200,
   "lines": ["第一行", "第二行"], "size": 18, "color": "muted"},
  {"type": "image", "x": 640, "y": 120, "w": 560, "h": 420,
   "image": "D:/课件/img/a.png", "cover": true, "rounded": true}
]}
```
**盒子 `type` 一览（9 类，17 个取值）**——`|` 两侧是同义词，随便写哪个：

| type | 是什么 | 专属字段 |
|---|---|---|
| `text` | 文本框 | `text` 单行 **或** `lines` 多行数组；`size`（默认 18，范围 4–400）、`align`(`l`/`ctr`/`r`)、`anchor`(`t`/`ctr`/`b`)、`bold`、`italic` |
| `rect` \| `bar` | 实色矩形/色条 | `color`（默认 accent） |
| `card` | 圆角卡片 | 无（配色随色板走） |
| `scrim` | 半透明蒙版 | `color`（默认 `#000`）、`alpha` 0–100（默认 50） |
| `image` \| `pic` | 真图片框 | `cover`（默认 true）、`rounded`（默认 false） |
| `line` \| `arrow` \| `axis` | 直线 / 箭头 / 坐标轴 | 终点 `x2`（默认 `x+w`）、`y2`（默认 `y`）；`arrow`/`axis` 自带箭头，`line` 写 `"arrow": true` 也能带；`"dash": true` 虚线 |
| `polyline` \| `curve` \| `polygon` | 折线 / 曲线 / 多边形 | `points` 点数组（**≥2 点**，不足则跳过该盒 + warning）；`polygon` 或 `"closed": true` 闭合；闭合后可 `fill` 填充 |
| `ellipse` \| `circle` | 椭圆 / 圆 | 给 `r` → 以 `(x,y)` 为**圆心**画半径 r 的圆；不给 `r` → 用 `x/y/w/h` 当外接框。可 `fill` |
| `point` \| `dot` | 实心标记点 | 以 `(x,y)` 为**圆心**，`r` 默认 6 |

- **线条/形状类通用**：`color` 描边色、`width` 线宽 1–40（默认 3）、`fill` 填充色（可选，不给则空心）。
- **每盒必给 `x/y/w/h`**（`line` 可用 `x2/y2` 定终点，`circle`/`point` 可用 `r` 定半径）。
- 颜色可写 `#RRGGBB`/`#RGB` 或色板词：`ink muted accent card line bg bg2 white black`。
- 一页可放多张 `image`，各自带 `image` 路径，按出现顺序嵌图。缺盒/坏图/未知 type 只降级该盒 + warning，不毁整页。

**⚠️ `freeform` 的 `text` 不走自适应字号。** 固定版式的字号由引擎按内容量自动算（放不下会自己缩），但 freeform 的 `size` **你给多少就是多少**（只 clamp 到 4–400）。字多框小 → 直接溢出，引擎不救你。写完自己按 1280×720 心算一遍：一个汉字宽 ≈ `size × 1.33` px，一行放得下 `w ÷ (size × 1.33)` 个字。

##### freeform 专属：`click` 单击逐步动画

任意盒子可加 `"click": N` —— **第 N 次单击时淡入出现**（`0` 或不写 = 随页立即显示）。

```json
{"layout": "freeform", "boxes": [
  {"type": "axis", "x": 200, "y": 560, "x2": 1080, "y2": 560, "color": "ink"},
  {"type": "axis", "x": 200, "y": 560, "x2": 200,  "y2": 140, "color": "ink"},
  {"type": "text", "x": 240, "y": 180, "w": 300, "h": 40, "text": "① 先看纵轴：浮力", "click": 1},
  {"type": "polyline", "points": [[200,560],[500,400],[900,200]], "color": "accent", "width": 4, "click": 2},
  {"type": "point", "x": 500, "y": 400, "r": 8, "fill": "accent", "click": 2},
  {"type": "text", "x": 560, "y": 380, "w": 400, "h": 40, "text": "② 排开体积越大，浮力越大", "click": 3}
]}
```

- **同一个 `click` 号的盒子在一次单击里一起出现**（上例第 2 击同时出曲线和那个点）；号从小到大依次触发，**不必连号**。
- 引擎生成的是**真 OOXML `<p:timing>`**，写法与 PowerPoint 自身一致 —— 放映时真能一步步点出来，导出后在 PowerPoint 里也还是真动画，不是假的。
- **这是分步讲授的杀手锏**：数学/物理的图「一笔笔加」（先坐标轴 → 再曲线 → 再标注）、解题步骤逐步揭示、先问后答（问题 `click:0`，答案 `click:1`）。**讲授节奏能被控制**，观众不会一上来就看到答案（**教学场景**尤其吃这套：学生不会一上来就看到答案）。
- **只有 `freeform` 支持**；固定版式没有这个字段，写了也不读。

**别滥用 freeform**：能用固定版式就用固定版式（它们已调好间距字号，且自适应）。`freeform` 是「就差这一页排不出来」时才动的手术刀 —— 但**画图（坐标轴/受力分析/几何图形/流程连线）和需要逐步动画的页，它是唯一的路**，该用就用。用了就自己负责别让元素重叠出界。

---

## 二、配图怎么来：`polaris-forge image`

```bash
polaris-forge image --prompt="<画面描述>" --out=<绝对路径.png> [--ratio=16:9]
```
- 走 MiniMax `image-01`，纯 Rust、零 Python。画幅：`1:1` `16:9` `4:3` `3:2` `2:3` `3:4` `9:16` `21:9`
- key 自动取（供应商坞的 MiniMax 条目 / 环境变量 `MINIMAX_API_KEY`），**你不用管也不要去找 key**
- 返回 JSON 里 `format` 是**真实**格式：MiniMax 常在你写 `out.png` 时回 JPEG。**这不影响使用**——按你写的路径引用即可，pptx 打包按内容认格式。别改扩展名，改了 spec 引用就断
- 生图失败（额度/限流）→ 报错。**不要卡在这里**：去掉该页 `image` 改用无图版式，把演示先交出来，末尾说明哪几页缺图

**写 prompt 的纪律（信息类配图 ≠ 艺术创作）**：
- **必须写「无文字」**。生图模型写中文必糊成鬼画符，一旦入页整份演示的可信度就没了
- 说清**风格 + 主体 + 光线 + 背景**，例：`儿童水彩插画,特写,一株嫩芽从泥土里探出头,嫩绿色,晨光,干净背景,无文字`
- 气质对味：轻快场景用水彩/手绘/明亮；写实主题清晰专业、少卡通（**教学场景建议**：小学用水彩/手绘/明亮，初中写实清晰，高中克制专业、少卡通）
- **密度按「每 2 页左右 1 张」**（8 页 ≈ 3–4 张，12 页 ≈ 5–6 张）——见「五、版面三条硬规矩 ①」。通篇无图和每页有图**同样不合格**
- **配图是道具不是装饰**：只给「讲不清楚才需要看」的地方配图（观察对象、情境导入、装置示意）。为好看而配的图是认知负担，不如留白

---

## 三、engine：要「无限版式 / 复用 Python 排版」时

spec **顶层**可加 `"engine"` 字段选渲染梯队（缺省 = 原生 Rust 引擎，零安装、最稳）：
- `"native"`（或不写）：纯 Rust 原生引擎。零依赖、三平台恒可用，就是上面这些版式 + `freeform`。**默认走这条**。
- `"python"`：交给 `py/pptx_bridge.py`（python-pptx）渲染**同一份 spec**——想用 Python 完整能力造任意版式、或复用 `build/engine.py` 已调好的排版时用。**代价：需本机装 `python-pptx`，非零安装**；装不上直接报错。
- `"auto"`：优先 Python，本机没有 python-pptx 就**静默回退原生引擎**并在 warnings 里留痕。想「能用 Python 就用、不能也别断」时选它。
```json
{"engine": "auto", "theme": "ink-gold", "slides": [ … ]}
```
> 加版式的正路：先试 `freeform`（零安装）；还不够，就在 `py/pptx_bridge.py` 里加分支（该文件头有扩展说明），走 `engine:"python"`。

---

## 四、制作流程（传统 PPT）

### 0. 选设计师（可选，但强烈建议）
读 `designers/INDEX.md` 花名册（11 位设计师 + auto 路由表）。用户指定就用指定的；没指定就按路由表**根据内容气质自动请一位**（向上汇报/党政→瑞士格大师、产品发布→发布会大师、种草→小红书大师、数据面板→玻璃酥大师、文旅华彩→国潮彩大师…判断不了用发布会大师兜底）。**教学场景建议**：课件优先 `pedagogy-clarity`（课件大师·认知减负师，「一页只教一件事」）；中小学/亲子可取 `doodle-hand`（手绘涂鸦）或 `clay-soft`（粘土）的气质。

传统 PPT 由引擎按色板确定性渲染，设计师**不影响像素**，但影响你的**内容决策**：一页放多少信息、用哪种版式、标题怎么起、什么该拆页。读该设计师 `.md` 的信息架构与禁忌部分，据此编排。

### 1. 读懂输入
用户可能给：正文文案、素材文件绝对路径（**先 Read**）、一份既定大纲/脚本。给了既定流程时**服从它的既有顺序**，不自作主张重排。（**教学场景**：给了教案就服从教案的活动流程，不打乱教学环节。）

### 2. 编排 spec
把内容按信息类型**混排版式**——这是好 PPT 与烂 PPT 的分水岭：

- 讲**并列关系** → `compare`，不是 bullets
- 讲**先后/流程** → `timeline`，不是 bullets
- 讲**数据/量级** → `stats`，不是 bullets
- 讲**对立/对照** → `two-col`
- **要看见才讲得清**（观察对象/情境/装置）→ `image-text`；**封面与情境导入** → `image-full`
- **换大主题** → 插 `section` 过渡
- 剩下的才 `bullets`

**通篇 bullets 是失败的 spec。** 一份 12 页的演示至少该出现 3 种以上版式。

**编排完，逐页对着「五、版面三条硬规矩」自查一遍再写盘**：
1. **配图数 ≈ 页数 ÷ 2**？（8 页该有 3–4 张。一张没有 → 加 `image-full` 封面 + 2–3 张关键讲解页）
2. **每页文字量在 3–5 行舒服区间**？（超 6 行 → 拆页；只有 1 行 → 并页或换版式）
3. **freeform 页的盒子两两不重叠、都在 40px 安全边距内、`size` 心算放得下**？

这三条是**验收线**。自查发现问题当场改，别写盘后等用户挑。

### 2.5 先落 spec，再生图，最后转换（边做边可见）
顺序必须是：**①写盘 → ②生图 → ③转换**。

1. **编排完就立刻把完整 spec 存盘**。要配图的页直接把**计划路径**写进 `image` 字段（如 `<产物目录>/img/01.png`，此刻文件还不存在没关系）——Polaris 的实时预览是逐页点亮的，spec 一落盘用户就能看到全部文字页，没生出来的图显示「配图待载入」占位框。**别把 spec 攒到生完图才写**，那会让用户对着空屏干等几分钟。
2. 内容很长时，可以先存一份**只含前几页的合法 spec**（JSON 必须完整合法），再增补到全量——每保存一次，预览就多亮几页。
3. 然后跑 `polaris-forge image` 把图逐张生到刚才写的路径上（可连跑几条），预览里的占位框会自动变成真图。
4. **最后**才做第四步的 spec→pptx 转换——带着不存在的图路径转换会得到「配图不可用」warning，全部图落盘后再转就没有。

配图密度**每 2 页左右 1 张**（8 页 ≈ 3–4 张，12 页 ≈ 5–6 张）——见「五、版面三条硬规矩 ①」。通篇无图是文字墙，每页有图是噪音，两头都不合格。

### 3. 存到产物目录
文件名**必须**是 `polaris.slides.json`（前端靠这个名字找它做预览和兜底转换，改名整条路线瘫痪）。

### 4. 转 .pptx
```bash
polaris-forge spec-pptx --spec=<产物目录>/polaris.slides.json --out=<产物目录>/演示.pptx
```
CLI 在 `~/Polaris/bin/`（Windows 为 `%USERPROFILE%\Polaris\bin\polaris-forge.exe`），Docker 镜像已内置在 PATH。

**CLI 不存在也不用慌**：把 spec 按上述文件名存好即可，Polaris 桌面端会自动调内置引擎完成转换。**不要**因为 CLI 缺失就改去写 HTML 或截图——那会毁掉可编辑性。

### 5. 回答末尾用**绝对路径**列出产物文件。

---

## 五、版面三条硬规矩（**先过这三条，再谈内容**）

这三条是验收线，不是建议。违反任意一条，这份演示就是废的——投影到墙上，谁都看得出来。

### ① 配图密度：**每 2 页左右 1 张图**

- 8 页 ≈ 3–4 张，12 页 ≈ 5–6 张，16 页 ≈ 7–8 张。**通篇无图 = 不合格**（一堵文字墙没人看得下去）；**每页都有图 = 同样不合格**（图变噪音，盖过内容）。
- 优先给这些页配：封面（`image-full` 定调）、情境导入、观察对象/装置示意/实物、抽象概念的具象类比。
- 纯推导页、纯罗列页、小结页**不要配图**——那里图是干扰（**教学场景**：练习页同理）。
- **配图是道具不是装饰**：只给「讲不清楚才需要看」的地方配。为好看而配的图是认知负担，不如留白。

### ② 文字占位：**每个框都要「填得住」，既不溢出也不空荡**

- **固定版式**（11 种）：引擎的 `autofit` 会按内容量自动定字号，放不下自己缩——所以你只要**控制内容量**：
  - **别塞爆**：单页正文超 6 行、单个 bullet 超 25 字、`compare`/`stats` 卡片正文超 3 行 → 拆页或删字。塞爆的后果是字号被压到很小，投影上后排看不见。
  - **别太空**：一页只有 1 条 bullet、`two-col` 只填了 left 不填 right、`stats` 只放 1 个数字 → 大片死白，不如并页或换版式。**每页正文 3–5 行是舒服区间**。
- **`freeform`**：字号 `size` **你给多少就是多少，引擎零兜底**（见上文 ⚠️）。写完必须按 1280×720 心算校验：一个汉字宽 ≈ `size × 1.33` px，`w ÷ (size × 1.33)` = 一行能放几个字。**算出来放不下就当场改小 `size` 或加宽 `w`**，别指望引擎救你。
- 图文页（`image-text`）文字侧同样受这条约束——图占半边，文字空间只有半页，更要凝练。

### ③ 不要相互覆盖：**任何两个元素不许重叠**

- **固定版式天然安全**（引擎算好坐标），这条主要约束 **`freeform`**——它是绝对定位，**你写多少就画在哪，引擎不做任何重叠检查**。
- 写完 freeform 页，**逐个盒子过一遍矩形相交**：盒 A `(x, y, w, h)` 与盒 B 重叠 ⟺ `A.x < B.x+B.w && B.x < A.x+A.w && A.y < B.y+B.h && B.y < A.y+A.h`。有相交就挪开或缩小。
- 常见翻车点：①文字盒压在坐标轴/曲线上 → 把标注挪到曲线**外侧**留 20px 以上间隙；②多个 `text` 盒竖排时 `y` 间距小于 `size × 1.33`（行高）→ 必然贴字；③`image` 盒盖住文字 → 图文分区放，或用 `scrim` 蒙版 + 白字（这是**唯一**允许的「有意覆盖」）。
- **画布边界也是覆盖**：所有盒子必须落在 `0 ≤ x`、`0 ≤ y`、`x+w ≤ 1280`、`y+h ≤ 720` 之内，页边留 **≥40px** 安全边距，别顶到边。
- 这是我们相对同类产品的**真实优势**：豆包/飞书幻灯片没有重排引擎，靠事后 lint 查重叠；我们的固定版式是事前保证。**别在 freeform 上把这个优势亲手丢掉。**

---

## 五·五、内容纪律

- **一页一个认知焦点**。单页正文超 6 行就拆页。
- **标题短**：能 6 字不写 12 字。标题是路标，不是句子。
- **要点凝练**：bullet 写关键词短语，不写完整长句；完整表述放 `notes` 口播稿里。
- **`notes` 别偷懒**：每页写清这页要讲什么、怎么引导、可能的疑问点。这是本工坊相对普通 PPT 的核心价值——投影出去的是骨架，讲述者手里是备注。（**教学场景**尤其吃这套：投影给学生看的是骨架，教师看的是备注，可能的学生疑问都写进 notes。）
- **深色色板看场景**：投影/日光环境下深色底常糊。`ink-gold`/`deep-space` 适合讲座/发布会；日常汇报、**课堂教学**优先浅色板。
- **不要在 spec 里塞 Markdown**：`**加粗**`、`# 标题`、`- 列表` 会被原样当文字渲染出来。加粗/字号/颜色全由版式决定。

## 六、改稿协议

用户说「第 3 页换成对比卡」「换个主题」「再加一页总结」时：

1. **直接改 `polaris.slides.json` 原文件**，文件名不变，别另起新文件
2. 重新跑 `polaris-forge spec-pptx` **覆盖导出**同一个 `.pptx`
3. CLI 不可用则改完 spec 即可（桌面端按 mtime 判旧，会自动重转）

只改 spec 不重转 pptx 是常见疏忽——用户拿到的导出会永远停在第一版。**能跑 CLI 就一定重跑。**

---

## 七、网页 PPT（HTML-deck · 次路线）

用户明确要 `.html` 网页幻灯片、可翻页/可分享、或要「一线发布会」级视觉动效时走这条：产出**自包含单文件** `.html`（所有 CSS/JS 内联，双击即开）。这条路线不经 spec、也不出可编辑的 .pptx——用户要「PPT」而没说「网页」时，一律回到上面的传统 PPT。

### ★ 主题 = `auto`（即 UI 的「AI 自由发挥」）= 默认高级感
`auto` **不是**「随便挑一个」，而是**默认做出一眼高级、有感染力的观感**：
- **优先深色 / 质感主题**，**不要默认白底**。首选：`aurora`（极光渐变辉光）、`glassmorphism`（毛玻璃）、`pitch-deck-vc`（融资路演）、`vaporwave`（蒸汽波）、`cyberpunk-neon`（赛博霓虹）、`tokyo-night`（东京夜）。
- 配方：**深底 + 多色渐变强调（`.gradient-text` 用在关键词上）+ 超大标题（封面 `.h1` 可到 110–160px）+ 克制留白 + 大数字金句页**。少字、字大、一页一事。
- **★ 丰富色彩，拒绝单色寡淡**：高级演示不是「一个主色走天下」。按大师配色思路建**多层色系**——主渐变用 2–3 个相邻或互补色相（落在 `.gradient-text`、装饰光晕、色带上），另配 1–2 个点缀强调色（数据高亮 / 图标 / `.pill` / `.card-accent`），分章节还可做 accent 微变奏（每章强调色轻微偏移，整份 deck 像一套策划过的系列海报）。色彩丰富 ≠ 花哨：彩色主要落在**装饰层**（渐变、辉光、色卡、图形、大数字），所有文字仍严守 design.md 第 0 条对比度铁律。
- **★ 版式即设计**：网页 deck 同样严禁「每页标题 + 列表」通篇一个模子——封面 / 章节页 / 大数字金句 / 两栏对比 / 卡片网格 / 漏斗 / 引用页混排，10 页以上至少出现 5 种页型；字阶档差拉满（封面超大标题与正文至少差 4–6 倍），层级靠 字号 × 字重 × 明度 三件套一起做。
- 仅当内容**明显属于**学术 / 公文 / 财报 / 法务等需要素白严肃的场景，才退回浅色主题（如 `academic-paper`、`corporate-clean`、`minimal-white`）——浅色主题同样要有讲究的色彩层次（如暖纸底 + 赭石/黛蓝双强调），不是白底黑字了事。
- 用户填了「自定义风格补充」时以其为准（如「黑金高级」→ 在深色主题上叠加金色强调）。

### 主题（36 套，data-theme 取值）

| 分组 | id |
|---|---|
| 高级感首选（深色/质感） | `aurora` `glassmorphism` `pitch-deck-vc` `vaporwave` `cyberpunk-neon` `tokyo-night` |
| 深色 | `dracula` `nord` `terminal-green` `blueprint` `catppuccin-mocha` `gruvbox-dark` `retro-tv` `rose-pine` |
| 浅色 | `minimal-white` `editorial-serif` `swiss-grid` `magazine-bold` `japanese-minimal` `xiaohongshu-white` `academic-paper` `corporate-clean` `soft-pastel` `arctic-cool` `bauhaus` `catppuccin-latte` `engineering-whiteprint` `midcentury` `news-broadcast` `sharp-mono` `solarized-light` `sunset-warm` |
| 特色 | `neo-brutalism` `memphis-pop` `rainbow-gradient` `y2k-chrome` |

应用主题 = 在 `<html data-theme="aurora">`。运行时按 `T` 可循环切换预览。

### 网页 deck 制作步骤

#### 0. ★ 先选设计师，再定「微设计规格」(定完再分页)
> **第一步永远是选设计师**（本工坊的灵魂）：
> 1. 读 `designers/INDEX.md`（11 位设计师花名册 + auto 路由表）。用户指定了就用指定的；没指定就按路由表**根据内容气质自动请一位**（向上汇报/党政→瑞士格大师、产品发布→发布会大师、种草→小红书大师、开发者/数据面板→玻璃酥大师、课程→课件大师、文旅华彩→国潮彩大师…判断不了用发布会大师兜底）。
> 2. 读该设计师的 `designers/<id>.md` 全文，照它的色板/字阶/版式/装饰/动效/禁忌做；它的「拿手三套系」用户可再挑一套，没挑用第一套。
> 3. 照该设计师第 10 节「实现映射」起手：`data-theme` + 要覆写的 token + 推荐页型。
> 4. **读 `designers/_foundation/taste.md`（10 条工艺纪律）**：按设计师 frontmatter 的默认拨盘（±1）设定 V/M/D 三拨盘，输出一行设计判读（T2 格式）；交付前跑文末 Pre-Flight 清单（通用节 + deck 特供节），打不了勾即返工。网页 deck 在产物**首行**写遥测锚点 `<!-- designer: <id> · dials V/M/D · preflight n/n -->`。
> 5. **想要更强的动效表现**：翻**扩展参考库** `designers-refero/`——从约 285 个一线产品官网归纳出的 11 大流派 / 24 位「动效见长」的设计师 + `_foundation/motion-library.md`（M1–M27 动效技法）+ `_foundation/recipes.md`（R-Mn 执行配方，动效代码抄配方改参数）。先读 `designers-refero/INDEX.md` 挑流派与人，再按其配方落地。
>
> **再读 `design.md`**，尤其第 0 条对比度铁律：**深底必浅字、浅底必深字**，绝不让深字压深底、浅字压浅底，小字尤甚。只用 token、不写死颜色，对比度就自动成立。设计师的性格配方 + design.md 的地基铁律，两者一起守。

动手前先钉死本次演示的视觉基调,后面每页都照它走,才有统一的高级感:
- **色板**:背景基调 + 主渐变(2–3 个色相) + 1–2 个点缀强调色 + 文字色(主/辅各一)。深色高级风优先,色彩要丰富有层次(见 `auto` 配方的「丰富色彩」纪律);丰富落在装饰层,文字色仍只用 token。
- **字阶**:封面超大标题 / 页标题 / 正文 / 大数字,各一个量级,档差拉开。
- **动画基调**:统一用 `anim-fade-up` 入场 + 列表 `anim-stagger-list` 错峰;克制,别页页花哨。
> 这一步对应设计人格的「设计先行」方法论。
> **动效分模式**:`html`(网页幻灯片)可叠加全套高级动效(神经网络背景/逐字标题/数字滚动,见步骤 2.5);spec/导出路线是离散静态页,**不用** Canvas/滚动动效(motion.js 在导出时也会自动关闭)。

#### 1. 规划内容 → 分页
把正文拆成「一页一个信息点」的结构。好演示的铁律：**每页只讲一件事，字少、字大、留白多**。封面 / 要点列表 / 大数字金句 / 两栏对比 / 结尾，是最常用的页型。演讲者要说但观众不该看到的内容，放进 `<div class="notes">…</div>`（默认隐藏，按 `S` 在演讲者视图看）。

#### 2. 用引擎写 deck.html
照 `templates/deck.html` 的骨架写。核心约定（全在 `base.css` 里）：
- 容器 `<div class="deck">`，每页一个 `<section class="slide" data-title="...">`
- 版式原语：`.grid .g2/.g3/.g4`、`.row`、`.card`/`.card-accent`/`.card-hover`、`.pill`、`.lede`、`.kicker`、`.gradient-text`、`.center`、`.funnel`（转化漏斗，见下）
- 标题：`.h1`/`.h2`/`h1.title`/`h2.title`/`.h3`
- 动画：元素加 `class="anim-fade-up"`（或 `anim-fade/anim-zoom/anim-slide-left/anim-slide-right`）；列表容器加 `anim-stagger-list`，子项设 `style="--i:0/1/2…"` 做错峰入场
- 页脚/进度/概览：`<div class="deck-footer"><span class="slide-number"></span></div>`、`<div class="progress-bar"><span></span></div>`、`<div class="overview"></div>`

##### 转化漏斗版式 `.funnel`（数据转化/留存页用）
阶梯式漏斗，每段宽度相等、高度按 `--lvl`（该环节量 ÷ 漏斗顶端量，0~1）自动降下并随之调深色。颜色全走主题 token，深/浅主题都成立。骨架：
```html
<div class="funnel anim-stagger-list">
  <div class="funnel-step" style="--lvl:1;--i:0">      <!-- 顶端 --lvl=1 -->
    <div class="funnel-head"><span class="fs-tag">环节 01</span></div>
    <div class="fs-name">访问</div>
    <div class="fs-num" data-count="109">0<span class="u">万</span></div>
    <div class="fs-foot"><div class="kv"><span class="dot"></span>独立去重数</div></div>
  </div>
  <div class="funnel-step" style="--lvl:.71;--i:1">     <!-- --lvl=本环节量÷顶端量 -->
    <div class="funnel-head"><span class="fs-tag">环节 02</span><span class="fs-rate">↓ 71%</span></div>
    <div class="fs-name">搜索</div>
    <div class="fs-num" data-count="77.4">0<span class="u">万</span></div>
    <div class="fs-foot"><div class="kv"><span class="dot"></span>独立去重数</div></div>
  </div>
  <!-- 末两环节同构，--lvl 依次更小 -->
</div>
```
要点：`--lvl` 一个变量同时决定高度与色深；`fs-rate`（段间转化率）只放在第 2 段起的 `.funnel-head` 里；建议 3~5 段，超过 5 段会过窄。大数字挂 `data-count` 时仅在开启动效(`data-motion`)的 html 路线滚动，否则静态显示。

#### 2.5 高级动效（仅 html 网页幻灯片，深色主题默认开 / 浅色严肃主题关）
让网页 PPT 像一线发布会，而不是静态翻页。零依赖纯原生，**导出 .pptx 时自动关闭、不污染截图**：
- **全局背景/光晕**：在 `<html data-theme="tokyo-night" data-motion>` 上加 `data-motion`，motion.js 自动注入神经网络 Canvas 背景 + 鼠标跟随光晕（仅深色主题好看；motion.css 会把 `.deck` 设透明让 Canvas 透出，body 的主题底色作背景）。主色可设 `--motion-accent:#xxxxxx; --motion-glow:rgba(...);` 覆盖。
- **封面逐字标题**：给封面大标题加 `data-kinetic`（该页激活时每字错峰滑入）。
- **数据页数字滚动**：给大数字加 `data-count="95" data-suffix="%"`（该页激活时从 0 滚到目标值）。例：`<div class="h1" data-count="300" data-suffix="%">0</div>`。
- 触发对接翻页（页激活才动），**不是**滚动；deck 自带的进度条/翻页不受影响。
- **别给** `academic-paper`/`corporate-clean`/`minimal-white` 等浅色严肃主题开（Canvas 干扰阅读）。`prefers-reduced-motion` 时自动停 Canvas、动效落终值。

#### 3. ★ 做成自包含单文件
**把 `assets/base.css` 与 `assets/themes.css` 的内容内联进 `<style>`，把 `assets/runtime.js` 内联进 `<script>`**，删掉对 `../assets/*` 的外链。**启用了高级动效(2.5)就再内联 `assets/motion.css`（进 `<style>`）+ `assets/motion.js`（进 `<script>`，放在 runtime.js 之后）**。这样产出的 `deck.html` 是**单文件**，可独立分享、可被截图导出、不依赖技能目录。读取：
```bash
cat ~/Polaris/skills/polaris-deck-studio/assets/base.css
cat ~/Polaris/skills/polaris-deck-studio/assets/themes.css
cat ~/Polaris/skills/polaris-deck-studio/assets/runtime.js
cat ~/Polaris/skills/polaris-deck-studio/assets/motion.css   # 仅启用动效时
cat ~/Polaris/skills/polaris-deck-studio/assets/motion.js    # 仅启用动效时
```
把 deck.html 存到**产物目录**（文件名如 `演示-<主题>.html`）。

#### 4a. 模式 = html（网页幻灯片）
到此就完成了。在回答末尾给出 `deck.html` 的绝对路径，并说明：双击用浏览器打开；`←/→/空格` 翻页、`F` 全屏、`O` 概览、`T` 换主题、`P`/`Ctrl+P` 导出 PDF。

#### 4c. 网页 deck → PPT（要像素级主题视觉时用）
已写好自包含 deck.html 后（如用户先要了网页版又要 PPT）：
```bash
polaris-forge pptx --deck="<产物目录>/演示-<主题>.html" --out="<产物目录>/演示-<主题>.pptx" --width=1920 --height=1080
```
分层导出：每页先提取文本框（坐标/字号/颜色），背景按「隐藏文字」重新截图 → 真文本框叠在无字背景上 = **视觉还原 + 文字可编辑**（挪开文字无重影）。需要环境里有 chromium/Chrome/Edge（CLI 自动探测；Docker 需 full 镜像）。
CLI 不可用时的旧路（Node，最后手段）：先 `node ~/Polaris/skills/polaris-deck-studio/scripts/install-deps.mjs`（只装 JS 库，浏览器用本机 Edge/Chrome，**不会自动下载 chromium**），再跑 `scripts/export-pptx.mjs --deck=… --out=… --width=1920 --height=1080`（整版图嵌入，文字不可编辑）。浏览器由 `find-browser.mjs` 自动定位；缺浏览器就走 Ctrl+P 打印 PDF 兜底。

### 网页 deck 的兜底（依赖缺失也不能卡死）
- deck 截图路缺 chromium / `npm` 装不上 → 改让用户用浏览器打开 deck.html 后 **`Ctrl+P` → 另存为 PDF**（`base.css` 已含 `@media print` 分页，每页一张）；或退传统 PPT spec 路线（牺牲主题精确视觉，换 100% 可编辑）。
- 始终给出已经成功产出的那份文件的绝对路径，别让用户两手空空。

### 网页 deck 的画幅
默认 16:9（导出用 1920×1080）。若用户要 4:3，截图用 `--width=1440 --height=1080`，并把 `export-pptx.mjs` 里 `defineLayout`/`addImage` 的 13.333×7.5 改为 10×7.5（脚本注释处）。
