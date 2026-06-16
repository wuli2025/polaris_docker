# PPT 演示文稿模式

你处于「PPTX」模式，要**真正产出一个能打开的 .pptx 文件**，而不只是描述大纲。本模式由「做 PPT / 幻灯片 / 演示文稿」意图自动激活。

仿主流 AI PPT（豆包 / Gamma / 悟空）的打法：**大纲先行 → 选主题 → 按页型模板化渲染 → 直出 .pptx**。内容与设计分离——你只负责把内容填进结构化的 `SLIDES` 数组，配色版式交给主题库与页型库兜底。设计语言抄 open-design（卡片 + 柔和阴影 + 三级文字 + 强调色），确保「就算抠字重排也好看」。

## 铁律：必须落地一个文件，禁止静默失败
- 结束前**务必确认 .pptx 已写到磁盘**（用代码 `os.path.exists` + 文件大小 > 0 校验），确认后才说「已生成」。
- 任何一步失败（缺 Python / 装包失败 / 脚本报错），都要**用中文如实告诉用户卡在哪**，并立即走下面的兜底，绝不假装成功。

## 工作流总览（两段式，但绝不为「选题」而卡住）
1. **大纲先行**：先在对话里给一份结构化大纲（每页：页型 + 标题 + 要点），让用户一眼校准、好改。
2. **确认即渲染**：用户说「确认 / 可以 / 出吧」或指出要改哪页 → 改完直接渲染成 .pptx。
- **不要反问「你想做什么 / 给我个题目」**——哪怕只有一句话题目也自己拟好大纲。大纲是给用户**校准**用的，不是用来要选题的。这是「产不出 PPT」最大的坑。
- 用户一开始就说「直接做 / 不用确认」或已给了完整要求 → **跳过等待，大纲 + 渲染一气呵成**。

## 第 0 步 · 环境自检（先做，按结果分支）
.pptx 用 Python 库 `python-pptx` 生成。先探测可用的 Python：Windows 上依次试 `python`、`py`、`python3`；其它平台试 `python3`、`python`。

```bash
python --version || py --version || python3 --version
```

**分支 A — 有 Python**：确保 `python-pptx` 就绪，装包优先用国内镜像（用户多在国内，直连 PyPI 常超时）：
```bash
python -m pip install --quiet python-pptx pypdf -i https://pypi.tuna.tsinghua.edu.cn/simple
# 镜像失败再退默认源：
python -m pip install --quiet python-pptx pypdf
```
- `python-pptx`：生成 / 编辑 .pptx；`pypdf`：需要读 PDF 内容时再用。
- 装完用 `python -c "import pptx; print(pptx.__version__)"` 验证导入成功，再继续。

**分支 B — 没有 Python，或装包怎么都失败**：
1. **先用中文明确告诉用户**：「生成真正的 .pptx 需要 Python 环境（python-pptx），当前机器上没检测到 / 装不上，原因是 ___」。
2. 然后**用兜底方案先交付**：生成一个**单文件、自包含的 HTML 幻灯片**（16:9、键盘翻页、深色标题留白排版），存到产物目录，让用户立刻有东西用、可在侧边栏预览、也能打印成 PDF。
3. 末尾告诉用户：装好 Python 后我可以把这份内容**导出成真正的 .pptx**。不要因为缺环境就什么都不产出。

## 第 1 步 · 大纲先行
从用户的题目 / 文档 / 附件出发，**立刻**在对话里给一份大纲，逐页标注 **页型**（见下「页型库」），形如：

```
主题建议：corporate-clean（商务蓝）  ·  共 10 页
 1. [cover]    标题页 — 《2025 年中总结》/ 副标题
 2. [toc]      目录 — 业绩 / 复盘 / 计划
 3. [section]  章节 01 — 上半年业绩
 4. [stats]    关键数据 — 营收 1.2 亿(+37%) · 新客 8.6k · NPS 62
 5. [cards]    三大增长引擎 — 三张卡，各一句话
 6. [two_col]  复盘 — 做对了 vs 踩了坑
 7. [timeline] 下半年路线 — 四步
 8. [quote]    一句话定调
 9. [bullets]  风险提示（≤5 条）
10. [closing]  谢谢 / 联系方式
```
给完大纲一句话收尾：「确认就出 PPT，或告诉我改哪页 / 换主题」。给了 PDF/文档就先用 `pypdf` 抽文本、按章节切分映射到页。没指定页数默认 8–12 页。**别从头到尾全 bullets**——大纲阶段就要按内容把页型选对（见纪律）。

## 第 2 步 · 选主题 + 选页型（内容与设计分离，照 open-design 的设计语言）
**主题库**（抄 open-design 的设计令牌：背景 / 卡片底 surface / 描边 / 三级文字 / 强调色。用户没指定就按内容气质自己选一个，写进大纲、允许换）：

| id | 气质 | 适用 |
|---|---|---|
| `corporate-clean` | 商务蓝·浅底干净（默认） | 汇报 / 总结 / 路演 |
| `pitch-deck-vc` | 科技深蓝·发布会感 | 产品 / 技术 / 融资路演 |
| `minimal-white` | 极简纯白 | 设计感 / 通用 |
| `tokyo-night` | 深色霓虹蓝紫 | 技术 / 酷感 |
| `corporate-warm` | 暖橘浅底 | 文创 / 生活 / 教育 |
| `aurora` | 极光深色·青紫 | 前沿 / 愿景 / 强冲击 |

**页型库**（**每块内容挑最贴合的页型，这是「好看」的第一杠杆**）：
`cover` 封面 · `toc` 目录 · `section` 章节分隔 · `bullets` 要点页 · `cards` 并列卡片 · `stats` 数据大字 · `two_col` 双栏对比 · `timeline` 步骤时间轴 · `quote` 金句 · `closing` 结尾。

### 排版纪律（铁律，违反就是「低端 PPT」——必须遵守）
1. **一页一个核心**。讲不下就拆页，绝不堆。
2. **大字少字**。每页正文 ≤ 6 行；标题大、关键数字/短句超大。字小密 = 烂。
3. **能不用 bullets 就不用**：
   - 3–4 个**并列点** → 用 `cards`（带柔和阴影的卡片），**不要**罗列圆点。
   - 数字 / 指标 / 成果 → `stats`（超大数字）。
   - 步骤 / 流程 / 路线 → `timeline`。
   - 对比 / 前后 / 优劣 → `two_col`。
   - 一句重点 → `quote`，整页就这一句。
   - 实在是平铺要点才用 `bullets`，且 **≤ 5 条、每条 ≤ 14 字**。
4. **留白是设计**。引擎已给足边距，别再自己塞满。
5. 结构示例（10–14 页常见骨架）：`cover → toc → section → cards → stats → two_col → timeline → quote → closing`，按内容增删，**别从头到尾全 bullets**（那正是上一版「效果烂」的根因）。

## 第 3 步 · 渲染 .pptx（可直接套用的引擎）
下面是引擎：主题库 `THEMES`（open-design 令牌）+ 卡片/阴影/三级文字原语 + 页型渲染器 `RENDER` + 渲染循环。**你只需改 `THEME` 和 `SLIDES` 两个变量**填入大纲内容；保存路径换成**产物目录的绝对路径**。别动原语和 `fix_pptx`。

```python
import os, zipfile, re, shutil
from pptx import Presentation
from pptx.util import Inches, Pt
from pptx.dml.color import RGBColor
from pptx.enum.text import PP_ALIGN, MSO_ANCHOR
from pptx.enum.lang import MSO_LANGUAGE_ID        # 1.x 在 enum.lang（不是 enum.text）
from pptx.enum.shapes import MSO_SHAPE
from pptx.oxml.ns import qn, nsdecls
from pptx.oxml import parse_xml

def C(h):  # 0xRRGGBB → RGBColor
    return RGBColor((h >> 16) & 0xFF, (h >> 8) & 0xFF, h & 0xFF)

# ── 主题库：抄 open-design 设计令牌（bg 背景 / surf 卡片底 / border 描边 / 三级文字 / accent 强调）──
THEMES = {
    "corporate-clean": dict(bg=0xFFFFFF, surf=0xFFFFFF, border=0xDCE6F2, t1=0x0E1726, t2=0x445268, t3=0x8A96AA, accent=0x2563EB, radius=0.09),
    "pitch-deck-vc":   dict(bg=0x0E1116, surf=0x161B22, border=0x2A313C, t1=0xE9EDF5, t2=0xAAB4C5, t3=0x6B7686, accent=0x5B8CFF, radius=0.09),
    "minimal-white":   dict(bg=0xFFFFFF, surf=0xFFFFFF, border=0xE7E7EA, t1=0x111216, t2=0x55596A, t3=0x8A8F9E, accent=0x3B6CFF, radius=0.12),
    "tokyo-night":     dict(bg=0x1A1B26, surf=0x24283B, border=0x2E3350, t1=0xC0CAF5, t2=0xA9B1D6, t3=0x565F89, accent=0x7AA2F7, radius=0.08),
    "corporate-warm":  dict(bg=0xFFF5EC, surf=0xFFFAF4, border=0xF1DBC8, t1=0x2B1D18, t2=0x5E463C, t3=0x9A8276, accent=0xFF6B4A, radius=0.12),
    "aurora":          dict(bg=0x0B1020, surf=0x121A30, border=0x232C44, t1=0xE6F0FF, t2=0xAEBFDC, t3=0x5F6F92, accent=0x6EE7B7, radius=0.10),
}

def set_font(run, latin="Segoe UI"):
    # 数字/英文用 Segoe UI（好看），东亚字形用微软雅黑；两端都指定，避免数字被套中文字体显土。
    run.font.name = latin
    rPr = run._r.get_or_add_rPr()
    latin_el = rPr.find(qn('a:latin'))
    ea = rPr.find(qn('a:ea'))
    if ea is None:
        ea = rPr.makeelement(qn('a:ea'), {})
        if latin_el is not None:
            latin_el.addnext(ea)        # a:ea 必须排在 a:latin 之后，否则部分阅读器报修复
        else:
            rPr.append(ea)
    ea.set('typeface', '微软雅黑')
    run.font.language_id = MSO_LANGUAGE_ID.SIMPLIFIED_CHINESE

def soft_shadow(shape, alpha=18):
    # open-design 的灵魂：卡片柔和投影（大模糊 + 低透明度）。没有它，卡片像贴纸 = 低端。
    spPr = shape._element.spPr
    for e in spPr.findall(qn('a:effectLst')):
        spPr.remove(e)
    spPr.append(parse_xml(
        '<a:effectLst %s><a:outerShdw blurRad="180000" dist="40000" dir="5400000" rotWithShape="0">'
        '<a:srgbClr val="000000"><a:alpha val="%d000"/></a:srgbClr></a:outerShdw></a:effectLst>'
        % (nsdecls('a'), alpha)))

def _new(prs, th):
    s = prs.slides.add_slide(prs.slide_layouts[6])   # 空白版式，全自排
    s.background.fill.solid(); s.background.fill.fore_color.rgb = C(th["bg"])
    return s

def bar(slide, l, t, w, h, color):                   # 强调短线 / 色条
    sp = slide.shapes.add_shape(MSO_SHAPE.RECTANGLE, Inches(l), Inches(t), Inches(w), Inches(h))
    sp.fill.solid(); sp.fill.fore_color.rgb = C(color); sp.line.fill.background()
    sp.shadow.inherit = False
    return sp

def card(slide, l, t, w, h, th):                     # open-design .card：surface 底 + 描边 + 柔和阴影 + 圆角
    sp = slide.shapes.add_shape(MSO_SHAPE.ROUNDED_RECTANGLE, Inches(l), Inches(t), Inches(w), Inches(h))
    try: sp.adjustments[0] = th.get("radius", 0.09)
    except Exception: pass
    sp.fill.solid(); sp.fill.fore_color.rgb = C(th["surf"])
    sp.line.color.rgb = C(th["border"]); sp.line.width = Pt(1)
    sp.shadow.inherit = False
    if th.get("radius", 0.09) > 0: soft_shadow(sp)
    return sp

def circle(slide, l, t, d, th, label):               # timeline 数字节点
    sp = slide.shapes.add_shape(MSO_SHAPE.OVAL, Inches(l), Inches(t), Inches(d), Inches(d))
    sp.fill.solid(); sp.fill.fore_color.rgb = C(th["accent"]); sp.line.fill.background()
    sp.shadow.inherit = False
    tf = sp.text_frame
    tf.margin_left = tf.margin_right = tf.margin_top = tf.margin_bottom = 0
    p = tf.paragraphs[0]; p.alignment = PP_ALIGN.CENTER
    r = p.add_run(); r.text = label
    r.font.size = Pt(18); r.font.bold = True; r.font.color.rgb = C(th["bg"]); set_font(r)
    return sp

def para_box(slide, l, t, w, h, paras, anchor=MSO_ANCHOR.TOP):
    # paras: list of dict(text, size, color, bold=False, italic=False, align=PP_ALIGN.LEFT, after=6, line=None)
    tf = slide.shapes.add_textbox(Inches(l), Inches(t), Inches(w), Inches(h)).text_frame
    tf.word_wrap = True; tf.vertical_anchor = anchor
    tf.margin_left = tf.margin_right = tf.margin_top = tf.margin_bottom = 0
    for i, d in enumerate(paras):
        p = tf.paragraphs[0] if i == 0 else tf.add_paragraph()
        p.alignment = d.get("align", PP_ALIGN.LEFT)
        p.space_after = Pt(d.get("after", 6))
        if d.get("line"): p.line_spacing = d["line"]
        r = p.add_run(); r.text = d["text"]
        r.font.size = Pt(d["size"]); r.font.bold = d.get("bold", False)
        r.font.italic = d.get("italic", False); r.font.color.rgb = C(d["color"]); set_font(r)
    return tf

def head(s, th, title):                              # 内容页公共题头：标题(t1) + accent 短线，返回内容起始 y
    para_box(s, 1.0, 0.62, 11.3, 0.9, [dict(text=title or "", size=29, color=th["t1"], bold=True)])
    bar(s, 1.0, 1.5, 0.7, 0.05, th["accent"])
    return 1.95

# ── 页型渲染器：每个吃 (prs, th, d) ──
def page_cover(prs, th, d):
    s = _new(prs, th)
    if d.get("kicker"):
        para_box(s, 1.0, 2.05, 11.3, 0.5, [dict(text=d["kicker"].upper(), size=14, color=th["accent"], bold=True)])
    para_box(s, 1.0, 2.55, 11.3, 1.7, [dict(text=d["title"], size=50, color=th["t1"], bold=True, line=1.04)])
    bar(s, 1.0, 4.4, 1.0, 0.06, th["accent"])
    if d.get("subtitle"):
        para_box(s, 1.0, 4.65, 11.0, 1.0, [dict(text=d["subtitle"], size=20, color=th["t2"], line=1.4)])

def page_section(prs, th, d):
    s = _new(prs, th)
    para_box(s, 1.0, 2.1, 5.0, 2.0, [dict(text=d.get("no", "01"), size=84, color=th["accent"], bold=True)])
    bar(s, 1.0, 4.25, 1.0, 0.06, th["accent"])
    para_box(s, 1.0, 4.5, 11.0, 1.3, [dict(text=d.get("title", ""), size=36, color=th["t1"], bold=True, line=1.1)])
    if d.get("subtitle"):
        para_box(s, 1.0, 5.6, 11.0, 0.8, [dict(text=d["subtitle"], size=18, color=th["t2"])])

def page_toc(prs, th, d):
    s = _new(prs, th); y = head(s, th, d.get("title", "目录"))
    for i, it in enumerate(d["items"][:7]):
        yy = y + i * 0.62
        para_box(s, 1.0, yy, 0.9, 0.6, [dict(text="%02d" % (i + 1), size=20, color=th["accent"], bold=True)])
        para_box(s, 1.9, yy, 10.3, 0.6, [dict(text=str(it), size=21, color=th["t1"])])

def page_bullets(prs, th, d):
    s = _new(prs, th); y = head(s, th, d.get("title", ""))
    paras = [dict(text="•  " + str(it), size=21, color=th["t1"], after=14, line=1.25) for it in d["points"][:6]]
    para_box(s, 1.0, y + 0.1, 11.3, 4.4, paras)

def page_cards(prs, th, d):
    s = _new(prs, th); y = head(s, th, d.get("title", ""))
    items = d["items"][:3]; n = max(1, len(items)); gap = 0.4
    w = (11.3 - gap * (n - 1)) / n
    for i, it in enumerate(items):
        x = 1.0 + i * (w + gap)
        card(s, x, y + 0.1, w, 3.7, th)
        para_box(s, x + 0.35, y + 0.45, w - 0.7, 0.6, [dict(text=it.get("head", ""), size=19, color=th["accent"], bold=True)])
        bps = [dict(text=ln.strip(), size=16, color=th["t2"], after=8, line=1.35)
               for ln in str(it.get("body", "")).split("\n") if ln.strip()]
        if bps: para_box(s, x + 0.35, y + 1.15, w - 0.7, 2.4, bps)

def page_stats(prs, th, d):
    s = _new(prs, th); y = head(s, th, d.get("title", ""))
    items = d["items"][:4]; n = max(1, len(items)); gap = 0.4
    w = (11.3 - gap * (n - 1)) / n
    for i, it in enumerate(items):
        x = 1.0 + i * (w + gap)
        card(s, x, y + 0.3, w, 3.0, th)
        para_box(s, x + 0.2, y + 0.7, w - 0.4, 1.4, [dict(text=it.get("value", ""), size=58, color=th["accent"], bold=True, align=PP_ALIGN.CENTER)])
        para_box(s, x + 0.2, y + 2.2, w - 0.4, 0.6, [dict(text=it.get("label", ""), size=17, color=th["t1"], bold=True, align=PP_ALIGN.CENTER)])
        if it.get("desc"):
            para_box(s, x + 0.2, y + 2.78, w - 0.4, 0.6, [dict(text=it["desc"], size=13, color=th["t3"], align=PP_ALIGN.CENTER)])

def page_two_col(prs, th, d):
    s = _new(prs, th); y = head(s, th, d.get("title", ""))
    cols = [(d.get("left_title", ""), d.get("left", [])), (d.get("right_title", ""), d.get("right", []))]
    gap = 0.5; w = (11.3 - gap) / 2
    for i, (h_, pts) in enumerate(cols):
        x = 1.0 + i * (w + gap)
        card(s, x, y + 0.1, w, 3.9, th)
        para_box(s, x + 0.4, y + 0.5, w - 0.8, 0.6, [dict(text=h_, size=19, color=th["accent"], bold=True)])
        bps = [dict(text="•  " + str(it), size=16, color=th["t2"], after=10, line=1.3) for it in pts[:5]]
        if bps: para_box(s, x + 0.4, y + 1.2, w - 0.8, 2.6, bps)

def page_timeline(prs, th, d):
    s = _new(prs, th); y = head(s, th, d.get("title", ""))
    steps = d["steps"][:5]; n = max(1, len(steps)); gap = 0.3
    w = (11.3 - gap * (n - 1)) / n; cy = y + 0.5; dia = 0.6
    if n > 1:
        x0 = 1.0 + w / 2; span = (n - 1) * (w + gap)
        bar(s, x0, cy + dia / 2 - 0.015, span, 0.03, th["border"])
    for i, st in enumerate(steps):
        x = 1.0 + i * (w + gap)
        circle(s, x + w / 2 - dia / 2, cy, dia, th, str(i + 1))
        para_box(s, x, cy + dia + 0.2, w, 0.6, [dict(text=st.get("head", ""), size=16, color=th["t1"], bold=True, align=PP_ALIGN.CENTER)])
        bps = [dict(text=ln.strip(), size=13, color=th["t3"], after=4, align=PP_ALIGN.CENTER, line=1.3)
               for ln in str(st.get("body", "")).split("\n") if ln.strip()]
        if bps: para_box(s, x, cy + dia + 0.78, w, 1.6, bps)

def page_quote(prs, th, d):
    s = _new(prs, th)
    para_box(s, 1.0, 1.4, 2.0, 1.3, [dict(text="“", size=90, color=th["accent"], bold=True)])
    para_box(s, 1.4, 2.7, 10.5, 2.4, [dict(text=d["text"], size=33, color=th["t1"], bold=True, align=PP_ALIGN.CENTER, line=1.25)])
    if d.get("by"):
        para_box(s, 1.4, 5.2, 10.5, 0.6, [dict(text="— " + d["by"], size=18, color=th["t2"], align=PP_ALIGN.CENTER)])

def page_closing(prs, th, d):
    s = _new(prs, th)
    para_box(s, 1.0, 2.6, 11.3, 1.6, [dict(text=d.get("title", "谢谢观看"), size=46, color=th["t1"], bold=True, align=PP_ALIGN.CENTER)])
    bar(s, 6.17, 4.3, 1.0, 0.06, th["accent"])
    if d.get("subtitle"):
        para_box(s, 1.0, 4.6, 11.3, 0.8, [dict(text=d["subtitle"], size=19, color=th["t2"], align=PP_ALIGN.CENTER)])

RENDER = {
    "cover": page_cover, "toc": page_toc, "section": page_section, "bullets": page_bullets,
    "cards": page_cards, "stats": page_stats, "two_col": page_two_col, "timeline": page_timeline,
    "quote": page_quote, "closing": page_closing,
    "bignum": page_stats,   # 兼容旧名
}

# ════════ 只改这两个变量：主题 + 大纲内容 ════════
THEME = "corporate-clean"
SLIDES = [
    {"type": "cover",   "kicker": "2025 年中", "title": "上半年经营回顾", "subtitle": "增长 · 复盘 · 下半年计划"},
    {"type": "toc",     "title": "目录", "items": ["业绩概览", "增长引擎", "复盘", "下半年路线"]},
    {"type": "section", "no": "01", "title": "业绩概览", "subtitle": "三个关键数字"},
    {"type": "stats",   "title": "关键指标", "items": [
        {"value": "1.2亿", "label": "营收", "desc": "同比 +37%"},
        {"value": "8.6k", "label": "新增客户", "desc": "环比 +22%"},
        {"value": "62", "label": "NPS", "desc": "行业前 10%"}]},
    {"type": "cards",   "title": "三大增长引擎", "items": [
        {"head": "渠道下沉", "body": "新增 3 省代理\n县域覆盖翻倍"},
        {"head": "产品升级", "body": "旗舰款复购 +18%\n毛利改善 4pt"},
        {"head": "私域运营", "body": "社群 GMV 占比 27%\n获客成本 -31%"}]},
    {"type": "two_col", "title": "复盘", "left_title": "做对了", "left": ["押对旗舰款", "私域早布局"],
     "right_title": "踩了坑", "right": ["库存预估偏高", "新区招人慢"]},
    {"type": "timeline", "title": "下半年路线", "steps": [
        {"head": "Q3 拓渠道", "body": "再下沉 2 省"}, {"head": "Q3 上新", "body": "第二曲线"},
        {"head": "Q4 冲量", "body": "大促备战"}, {"head": "Q4 复盘", "body": "年度收口"}]},
    {"type": "quote",   "text": "把对的事做厚，而不是把多的事做薄。", "by": "年度战略定调"},
    {"type": "closing", "title": "谢谢观看", "subtitle": "Q&A · 联系方式"},
]
# ════════════════════════════════════════════════

prs = Presentation()
prs.slide_width, prs.slide_height = Inches(13.333), Inches(7.5)   # 16:9
th = THEMES[THEME]
for d in SLIDES:
    RENDER.get(d["type"], page_bullets)(prs, th, d)

OUT = r"<产物目录绝对路径>/演示文稿.pptx"   # ← 换成已授权的产物目录
prs.save(OUT)

# ── 后处理：修复 python-pptx 已知兼容性问题（WPS/Mac/各种阅读器拒收）──
#   1) app.xml <Slides> 计数没更新  2) app.xml PresentationFormat 固定 4:3
#   3) presentation.xml sldSz type="screen4x3" 与 16:9 尺寸矛盾
#   4) slideLayout 里 p14:creationId（Office 2010 扩展，WPS Mac 不识别）
def fix_pptx(path):
    n = len(Presentation(path).slides)
    tmp = path + ".tmp"
    with zipfile.ZipFile(path, "r") as zin, zipfile.ZipFile(tmp, "w", zipfile.ZIP_DEFLATED) as zout:
        for info in zin.infolist():
            data = zin.read(info.filename); fn = info.filename
            is_layout = fn.startswith("ppt/slideLayouts/slideLayout") and fn.endswith(".xml")
            if fn in ("docProps/app.xml", "ppt/presentation.xml") or is_layout:
                txt = data.decode("utf-8")
                if fn == "docProps/app.xml":
                    txt = re.sub(r"<Slides>\d+</Slides>", f"<Slides>{n}</Slides>", txt)
                    txt = re.sub(r"<PresentationFormat>[^<]+</PresentationFormat>",
                                 "<PresentationFormat>On-screen Show (16:9)</PresentationFormat>", txt)
                elif fn == "ppt/presentation.xml":
                    txt = txt.replace('type="screen4x3"', '')
                else:
                    txt = re.sub(r"<p:extLst>.*?</p:extLst>", "", txt, flags=re.DOTALL)
                data = txt.encode("utf-8")
            zout.writestr(info, data)
    shutil.move(tmp, path)

fix_pptx(OUT)
assert os.path.exists(OUT) and os.path.getsize(OUT) > 0, "保存失败"
print("SAVED", OUT, os.path.getsize(OUT), "bytes")
```

- 需要图表时用 `python-pptx` 原生 chart；需要配图时配合 image-gen 技能（注意：当前供应商多半不支持真实生图，详见该技能）。
- 想加页型就再写一个 `page_xxx(prs, th, d)` 注册进 `RENDER`，渲染循环不用动。

## 输出
- 用中文说明演示结构与亮点（用了哪个主题、几页、各页型怎么分布）。
- 把 .pptx 产出到**已授权的产物目录**（绝对路径），并在末尾点明文件名与页数。
- 走了 HTML 兜底时，明确说这是「HTML 幻灯片」替代方案，以及如何升级成真 .pptx。
