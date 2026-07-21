//! Polaris Forge · spec JSON → **原生可编辑 .pptx**(路线 B,「传统PPT」模式正解)。
//!
//! 模型出决策(polaris.slides.json:版式选择 + 结构化内容),本模块确定性落 OOXML——
//! 全部元素是真文本框/真形状/真填充,用户在 PowerPoint/WPS/Keynote 里 100% 可编辑;
//! **零截图、零浏览器**:Docker slim(无 chromium)也能出 PPT,纯文本模型即可驱动。
//!
//! spec v1(11 版式 × 原生原语):
//! ```json
//! {
//!   "version": 1,
//!   "theme": "minimal-white",          // 内置色板名,见 PALETTES
//!   "slides": [
//!     {"layout":"title",   "title":"…", "subtitle":"…", "kicker":"…"},
//!     {"layout":"section", "title":"…", "kicker":"…"},
//!     {"layout":"bullets", "title":"…", "points":["…", {"text":"…","sub":["…"]}]},
//!     {"layout":"two-col", "title":"…", "left":{"head":"…","points":["…"]}, "right":{…}},
//!     {"layout":"compare", "title":"…", "items":[{"head":"…","body":"…"}, …]},  // 2–4 卡
//!     {"layout":"stats",   "title":"…", "items":[{"value":"83%","label":"…","desc":"…"}, …]}, // 1–4 大数字
//!     {"layout":"timeline","title":"…", "steps":[{"head":"…","body":"…"}, …]},  // 2–5 步
//!     {"layout":"quote",   "text":"…", "by":"…"},
//!     {"layout":"closing", "title":"…", "subtitle":"…"},
//!     {"layout":"image-full","image":"/abs/a.png","title":"…","subtitle":"…","kicker":"…"},
//!     {"layout":"image-text","image":"/abs/b.jpg","title":"…","points":["…"],"side":"left"},
//!     {"layout":"freeform","boxes":[                     // 自由版式:摆脱固定版式,任意排布
//!        {"type":"text","x":80,"y":90,"w":700,"h":120,"text":"…","size":40,"color":"#fff","align":"ctr","bold":true},
//!        {"type":"rect","x":0,"y":0,"w":1280,"h":8,"color":"accent"},
//!        {"type":"scrim","x":0,"y":0,"w":1280,"h":720,"color":"#000","alpha":40},
//!        {"type":"card","x":700,"y":160,"w":480,"h":300},
//!        {"type":"image","x":720,"y":180,"w":440,"h":300,"image":"/abs/c.png","cover":true,"rounded":true}
//!     ]}
//!   ]
//! }
//! ```
//! `freeform` 是「不被固定版式框住」的出口:一页里想放几个盒子放几个,x/y/w/h 用 1280×720 逻辑 px
//! 自由定位。**盒子 9 类 / 17 个 type 取值**(`|` 两侧同义):
//!   - `text`             文本框。`text` 单行 或 `lines` 多行数组;`size`(默认 18,clamp 4–400)
//!                        `align`(l|ctr|r) `anchor`(t|ctr|b) `bold` `italic`。**不走 autofit**
//!   - `rect`|`bar`       实色条。`color`(默认 accent)
//!   - `card`             圆角卡(配色随色板)
//!   - `scrim`            半透明蒙版。`color`(默认 000000) `alpha` 0–100(默认 50)
//!   - `image`|`pic`      真图。`cover`(默认 true) `rounded`(默认 false);按出现顺序吃各 image 盒的路径
//!   - `line`|`arrow`|`axis`         直线/箭头/坐标轴。`x2`(默认 x+w) `y2`(默认 y) `arrow` `dash`
//!   - `polyline`|`curve`|`polygon`  折线/曲线/多边形。`points`(≥2 点) `closed` `fill`
//!   - `ellipse`|`circle`            椭圆/圆。给 `r` 则 (x,y) 为圆心;否则 x/y/w/h 为外接框。`fill`
//!   - `point`|`dot`                 实心标记点。(x,y) 为圆心,`r` 默认 6
//! 线条/形状类通用:`color` 描边 / `width` 线宽 1–40(默认 3) / `fill` 填充(可选)。
//! 颜色可写 `#RRGGBB`/`#RGB` 或色板词 `ink|muted|accent|card|line|bg|bg2|white|black`。
//! **任意盒子可加 `click: N`** → 第 N 次单击时淡入(0/缺省=随页显示);同号一起出,号升序触发。
//! 走 `build_timing()` 生成真 OOXML `<p:timing>`,与 PowerPoint 自身写法一致 —— 导出后仍是真动画。
//! 缺盒子/坏图/未知 type 只降级该盒 + warning,不毁整页。要「无限版式/复用 Python 排版资产」走
//! `engine:"python"` 交给 py/pptx_bridge.py(同一份 spec,python-pptx 渲染),见 spec_to_pptx_sync 路由。
//!
//! ⚠ 改了本模块的版式字段/盒子类型/色板/上限,**必须同步** src/templates/skills/polaris-deck-studio/SKILL.md
//! 并把 skills/templates.rs 的 DECK_VERSION +1(否则已安装用户的 skill.md 不会被重写,模型按旧约定
//! 产 spec → 能力静默用不上)。本段与 SKILL.md 逐字对齐是硬约定,2026-07 曾漂移过一轮:引擎加了
//! 4 类盒子 + click 动画,文档没跟 → 模型半年不知道能画坐标轴。
//! 每页可带 `"notes":"…"` → 写进演讲者备注页(notesSlide)。
//! 未知 layout 宽容降级为 bullets(尽量出东西,warnings 里报)。
//!
//! 配图(`image`:本地绝对路径,png/jpg):**真 `<p:pic>` 图片框**,不是背景贴图——用户在
//! PowerPoint 里可选中、换图、挪位。图按 **cover 语义**用 `a:srcRect` 裁切填满图框,
//! 绝不 stretch 变形(人物/图表拉长是课件里最刺眼的廉价感来源)。仅 image-* 两个版式吃
//! `image` 字段;其余版式写了也不读。图缺失/损坏 → 降级为无图版式 + warnings,不中断出片。
//!
//! 坐标系:1280×720 逻辑 px 画布,1px = 9525 EMU(96dpi 标准换算),16:9。

use serde_json::{json, Value};
use std::io::Write;
use zip::write::SimpleFileOptions;

use crate::forge::pptx::{
    slide_layout_xml, slide_master_xml, theme_xml, xml_decl, xml_escape, NS_A, NS_CT, NS_P, NS_R,
    NS_REL,
};

/// 1 逻辑 px(96dpi)= 9525 EMU。画布 1280×720 → 12192000×6858000(标准 16:9)。
const PX: i64 = 9525;
const CANVAS_W: i64 = 1280;
const CANVAS_H: i64 = 720;
const CX: u64 = (CANVAS_W * PX) as u64;
const CY: u64 = (CANVAS_H * PX) as u64;
const MAX_SLIDES: usize = 300;

/// 内置色板(对齐 deck 主题气质;传统 PPT 求规整,深浅各半)。
/// 字段:背景渐变两端 / 正文 / 弱化 / 强调 / 卡片底 / 卡片描边。
struct Palette {
    bg1: &'static str,
    bg2: &'static str,
    ink: &'static str,
    muted: &'static str,
    accent: &'static str,
    card: &'static str,
    card_line: &'static str,
}

fn palette(name: &str) -> (&'static str, Palette) {
    match name {
        "ink-gold" => (
            "ink-gold",
            Palette {
                bg1: "16181D",
                bg2: "1F232B",
                ink: "F2F0E9",
                muted: "A8A49A",
                accent: "D4B06A",
                card: "20242C",
                card_line: "2E333D",
            },
        ),
        "deep-space" => (
            "deep-space",
            Palette {
                bg1: "0B0F1A",
                bg2: "131A2A",
                ink: "E8ECF6",
                muted: "93A0B8",
                accent: "7AA2F7",
                card: "16203A",
                card_line: "263250",
            },
        ),
        "warm-paper" => (
            "warm-paper",
            Palette {
                bg1: "FAF6EE",
                bg2: "F3EDE0",
                ink: "3A2F25",
                muted: "8A7E6F",
                accent: "B3672A",
                card: "FFFFFF",
                card_line: "E5DCCB",
            },
        ),
        "forest" => (
            "forest",
            Palette {
                bg1: "F4F7F2",
                bg2: "E9F0E7",
                ink: "1E2A22",
                muted: "6B7A6F",
                accent: "2F7A4F",
                card: "FFFFFF",
                card_line: "D7E2D6",
            },
        ),
        "tech-blue" => (
            "tech-blue",
            Palette {
                bg1: "FFFFFF",
                bg2: "EEF3FA",
                ink: "16324F",
                muted: "5D7187",
                accent: "1F6FD6",
                card: "FFFFFF",
                card_line: "D8E2EE",
            },
        ),
        // 默认:近白暖米,最稳的「传统 PPT」气质。
        _ => (
            "minimal-white",
            Palette {
                bg1: "FFFFFF",
                bg2: "F6F5F0",
                ink: "1F1F1F",
                muted: "6B6B6B",
                accent: "A07520",
                card: "FFFFFF",
                card_line: "E6E3D8",
            },
        ),
    }
}

// ─────────────────────── OOXML 原语 ───────────────────────

/// 段落属性打包:字号 pt、加粗、斜体、颜色、对齐、可选 bullet 级别(0=•,1=–)。
struct Para<'a> {
    text: &'a str,
    size_pt: i64,
    bold: bool,
    italic: bool,
    color: &'a str,
    align: &'a str, // l|ctr|r
    bullet: Option<u8>,
    space_after_pt: i64, // 段后距 pt(0=不写)
}

impl<'a> Para<'a> {
    fn plain(text: &'a str, size_pt: i64, color: &'a str) -> Self {
        Para {
            text,
            size_pt,
            bold: false,
            italic: false,
            color,
            align: "l",
            bullet: None,
            space_after_pt: 0,
        }
    }
}

fn para_xml(p: &Para<'_>, pal: &Palette) -> String {
    let mut ppr = String::new();
    // marL/indent:bullet 悬挂缩进;级别 1 再缩一档。
    let bullet_attr = match p.bullet {
        Some(0) => " marL=\"285750\" indent=\"-285750\"",
        Some(_) => " marL=\"571500\" indent=\"-285750\" lvl=\"1\"",
        None => "",
    };
    ppr.push_str(&format!("<a:pPr algn=\"{}\"{}>", p.align, bullet_attr));
    if p.space_after_pt > 0 {
        ppr.push_str(&format!(
            "<a:spcAft><a:spcPts val=\"{}\"/></a:spcAft>",
            p.space_after_pt * 100
        ));
    }
    match p.bullet {
        Some(0) => ppr.push_str(&format!(
            "<a:buClr><a:srgbClr val=\"{}\"/></a:buClr><a:buFont typeface=\"Arial\"/><a:buChar char=\"•\"/>",
            pal.accent
        )),
        Some(_) => ppr.push_str(&format!(
            "<a:buClr><a:srgbClr val=\"{}\"/></a:buClr><a:buFont typeface=\"Arial\"/><a:buChar char=\"–\"/>",
            pal.muted
        )),
        None => ppr.push_str("<a:buNone/>"),
    }
    ppr.push_str("</a:pPr>");
    format!(
        "<a:p>{ppr}<a:r><a:rPr lang=\"zh-CN\" sz=\"{}\" b=\"{}\" i=\"{}\">\
<a:solidFill><a:srgbClr val=\"{}\"/></a:solidFill>\
<a:latin typeface=\"Calibri\"/><a:ea typeface=\"Microsoft YaHei\"/></a:rPr>\
<a:t>{}</a:t></a:r></a:p>",
        p.size_pt * 100,
        if p.bold { 1 } else { 0 },
        if p.italic { 1 } else { 0 },
        p.color,
        xml_escape(p.text)
    )
}

/// 文本框(px 坐标);paras 为已拼好的 <a:p> 串。anchor: t|ctr|b。
fn text_box(id: u32, x: i64, y: i64, w: i64, h: i64, anchor: &str, paras: &str) -> String {
    format!(
        "<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"text{id}\"/><p:cNvSpPr txBox=\"1\"/><p:nvPr/></p:nvSpPr>\
<p:spPr><a:xfrm><a:off x=\"{}\" y=\"{}\"/><a:ext cx=\"{}\" cy=\"{}\"/></a:xfrm>\
<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom><a:noFill/></p:spPr>\
<p:txBody><a:bodyPr wrap=\"square\" lIns=\"0\" tIns=\"0\" rIns=\"0\" bIns=\"0\" anchor=\"{anchor}\"><a:normAutofit/></a:bodyPr>\
{paras}</p:txBody></p:sp>",
        x * PX, y * PX, w * PX, h * PX
    )
}

/// 实色矩形(强调线/色条)。
fn solid_rect(id: u32, x: i64, y: i64, w: i64, h: i64, color: &str) -> String {
    format!(
        "<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"bar{id}\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>\
<p:spPr><a:xfrm><a:off x=\"{}\" y=\"{}\"/><a:ext cx=\"{}\" cy=\"{}\"/></a:xfrm>\
<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom>\
<a:solidFill><a:srgbClr val=\"{color}\"/></a:solidFill><a:ln><a:noFill/></a:ln></p:spPr>\
<p:txBody><a:bodyPr/><a:p/></p:txBody></p:sp>",
        x * PX,
        y * PX,
        w * PX,
        h * PX
    )
}

/// 圆角卡片(deck .card 的原生等价物)。
fn round_card(id: u32, x: i64, y: i64, w: i64, h: i64, pal: &Palette) -> String {
    format!(
        "<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"card{id}\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>\
<p:spPr><a:xfrm><a:off x=\"{}\" y=\"{}\"/><a:ext cx=\"{}\" cy=\"{}\"/></a:xfrm>\
<a:prstGeom prst=\"roundRect\"><a:avLst><a:gd name=\"adj\" fmla=\"val 6000\"/></a:avLst></a:prstGeom>\
<a:solidFill><a:srgbClr val=\"{}\"/></a:solidFill>\
<a:ln w=\"12700\"><a:solidFill><a:srgbClr val=\"{}\"/></a:solidFill></a:ln></p:spPr>\
<p:txBody><a:bodyPr/><a:p/></p:txBody></p:sp>",
        x * PX, y * PX, w * PX, h * PX, pal.card, pal.card_line
    )
}

/// 强调色圆形 + 居中数字(timeline 步骤节点)。文字用 bg1 反衬强调色,深浅色板都可读。
fn circle_num(id: u32, x: i64, y: i64, d: i64, label: &str, pal: &Palette) -> String {
    let p = Para {
        align: "ctr",
        bold: true,
        ..Para::plain(label, 18, pal.bg1)
    };
    format!(
        "<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"step{id}\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>\
<p:spPr><a:xfrm><a:off x=\"{}\" y=\"{}\"/><a:ext cx=\"{}\" cy=\"{}\"/></a:xfrm>\
<a:prstGeom prst=\"ellipse\"><a:avLst/></a:prstGeom>\
<a:solidFill><a:srgbClr val=\"{}\"/></a:solidFill><a:ln><a:noFill/></a:ln></p:spPr>\
<p:txBody><a:bodyPr anchor=\"ctr\" lIns=\"0\" tIns=\"0\" rIns=\"0\" bIns=\"0\"/>{}</p:txBody></p:sp>",
        x * PX, y * PX, d * PX, d * PX, pal.accent, para_xml(&p, pal)
    )
}

// ─────────────────────── 配图原语 ───────────────────────

/// 配图槽:spec 的 `image` 字段解析后的结果。原始像素尺寸用于算 cover 裁切,
/// 没有它就只能 stretch —— 那会把 16:9 的图硬塞进方框拉变形。
struct SlideImage {
    bytes: Vec<u8>,
    ext: &'static str,
    w: u32,
    h: u32,
}

/// 读图 + 嗅探格式 + 取原始尺寸(只读文件头,不整幅解码)。
/// 失败一律返 Err 交由调用方降级为「无图版式 + warning」,绝不 panic、不中断出片。
fn load_slide_image(path: &str) -> Result<SlideImage, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读不到 {path}: {e}"))?;
    if bytes.len() < 16 {
        return Err(format!("{path} 不像图片(仅 {} 字节)", bytes.len()));
    }
    // 按魔数认扩展名——不信文件后缀(模型常把 jpg 存成 .png)。ext 要与 Content_Types
    // 的 Default 声明对得上,认错会让 PowerPoint 报「需要修复」。
    let ext = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpeg"
    } else {
        return Err(format!("{path} 既不是 PNG 也不是 JPEG,已跳过"));
    };
    let (w, h) = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|e| format!("{path} 格式识别失败: {e}"))?
        .into_dimensions()
        .map_err(|e| format!("{path} 读尺寸失败: {e}"))?;
    if w == 0 || h == 0 {
        return Err(format!("{path} 尺寸为 0"));
    }
    Ok(SlideImage { bytes, ext, w, h })
}

/// 真图片框(`<p:pic>`)。按 **cover** 语义:等比缩放填满 (w,h),溢出部分用
/// `a:srcRect` 从两侧对称裁掉 —— 与 CSS `object-fit: cover` 同义。
/// srcRect 单位是千分之一个百分点(l="12500" = 从左裁 12.5%)。
///
/// `rounded`:图文分栏里的图走圆角(与 round_card 同 adj,视觉成套),全幅图必须直角
/// (圆角会在页边露出四个背景色缺口)。前端预览 slidesSpec.ts 的 .pic 圆角与此一一对应,
/// 改这里记得同步改那边 —— 预览即导出。
fn pic_xml(
    id: u32,
    rid: &str,
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    img_w: u32,
    img_h: u32,
    rounded: bool,
) -> String {
    let target = w as f64 / h as f64;
    let src = img_w as f64 / img_h as f64;
    let (mut l, mut t) = (0i64, 0i64);
    if src > target {
        // 图更宽 → 左右各裁一半溢出
        let keep = target / src;
        l = (((1.0 - keep) / 2.0) * 100_000.0).round() as i64;
    } else if src < target {
        // 图更高 → 上下各裁一半溢出
        let keep = src / target;
        t = (((1.0 - keep) / 2.0) * 100_000.0).round() as i64;
    }
    // 极端长条图裁掉 >45% 就没法看了,夹住:宁可留点边也别只剩中间一条。
    l = l.clamp(0, 45_000);
    t = t.clamp(0, 45_000);
    let src_rect = if l > 0 || t > 0 {
        format!("<a:srcRect l=\"{l}\" t=\"{t}\" r=\"{l}\" b=\"{t}\"/>")
    } else {
        String::new()
    };
    let geom = if rounded {
        "<a:prstGeom prst=\"roundRect\"><a:avLst><a:gd name=\"adj\" fmla=\"val 6000\"/></a:avLst></a:prstGeom>"
    } else {
        "<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom>"
    };
    format!(
        "<p:pic><p:nvPicPr><p:cNvPr id=\"{id}\" name=\"图片{id}\"/>\
<p:cNvPicPr><a:picLocks noChangeAspect=\"1\"/></p:cNvPicPr><p:nvPr/></p:nvPicPr>\
<p:blipFill><a:blip r:embed=\"{rid}\"/>{src_rect}<a:stretch><a:fillRect/></a:stretch></p:blipFill>\
<p:spPr><a:xfrm><a:off x=\"{}\" y=\"{}\"/><a:ext cx=\"{}\" cy=\"{}\"/></a:xfrm>\
{geom}</p:spPr></p:pic>",
        x * PX,
        y * PX,
        w * PX,
        h * PX
    )
}

// ─────────────────────── 数学矢量原语(freeform 精绘几何/函数图) ───────────────────────

/// 直线/带箭头线(坐标轴、辅助线)。(x1,y1)→(x2,y2),width_px 描边;arrow 末端箭头;dash 虚线。
/// 用 line 预设 + flipH/flipV 把「左上→右下」的对角线摆到任意两点之间。
fn line_xml(
    id: u32,
    x1: i64,
    y1: i64,
    x2: i64,
    y2: i64,
    color: &str,
    width_px: i64,
    arrow: bool,
    dash: bool,
) -> String {
    let (ox, oy) = (x1.min(x2), y1.min(y2));
    let cx = (x1 - x2).abs().max(1);
    let cy = (y1 - y2).abs().max(1);
    let flip_h = if x2 < x1 { " flipH=\"1\"" } else { "" };
    let flip_v = if y2 < y1 { " flipV=\"1\"" } else { "" };
    let w = width_px.max(1) * PX;
    let dash_xml = if dash { "<a:prstDash val=\"dash\"/>" } else { "" };
    let tail = if arrow {
        "<a:tailEnd type=\"triangle\" w=\"med\" len=\"med\"/>"
    } else {
        ""
    };
    format!(
        "<p:cxnSp><p:nvCxnSpPr><p:cNvPr id=\"{id}\" name=\"line{id}\"/><p:cNvCxnSpPr/><p:nvPr/></p:nvCxnSpPr>\
<p:spPr><a:xfrm{flip_h}{flip_v}><a:off x=\"{}\" y=\"{}\"/><a:ext cx=\"{}\" cy=\"{}\"/></a:xfrm>\
<a:prstGeom prst=\"line\"><a:avLst/></a:prstGeom>\
<a:ln w=\"{w}\" cap=\"rnd\"><a:solidFill><a:srgbClr val=\"{color}\"/></a:solidFill>{dash_xml}{tail}</a:ln></p:spPr>\
</p:cxnSp>",
        ox * PX, oy * PX, cx * PX, cy * PX
    )
}

/// 折线/多边形(函数曲线、几何形):按 points 顶点连线;closed 闭合,fill 可填充。
/// custGeom 路径坐标相对包围盒左上角,单位 EMU(与 ext 同空间)。
fn polyline_xml(
    id: u32,
    points: &[(i64, i64)],
    stroke: &str,
    width_px: i64,
    closed: bool,
    fill: Option<&str>,
) -> String {
    if points.len() < 2 {
        return String::new();
    }
    let minx = points.iter().map(|p| p.0).min().unwrap();
    let miny = points.iter().map(|p| p.1).min().unwrap();
    let maxx = points.iter().map(|p| p.0).max().unwrap();
    let maxy = points.iter().map(|p| p.1).max().unwrap();
    let pw = (maxx - minx).max(1) * PX;
    let ph = (maxy - miny).max(1) * PX;
    let mut path = String::new();
    for (i, (px, py)) in points.iter().enumerate() {
        let rx = (px - minx) * PX;
        let ry = (py - miny) * PX;
        if i == 0 {
            path.push_str(&format!("<a:moveTo><a:pt x=\"{rx}\" y=\"{ry}\"/></a:moveTo>"));
        } else {
            path.push_str(&format!("<a:lnTo><a:pt x=\"{rx}\" y=\"{ry}\"/></a:lnTo>"));
        }
    }
    if closed {
        path.push_str("<a:close/>");
    }
    let fill_xml = match fill {
        Some(c) => format!("<a:solidFill><a:srgbClr val=\"{c}\"/></a:solidFill>"),
        None => "<a:noFill/>".to_string(),
    };
    let ww = width_px.max(1) * PX;
    format!(
        "<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"poly{id}\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>\
<p:spPr><a:xfrm><a:off x=\"{}\" y=\"{}\"/><a:ext cx=\"{pw}\" cy=\"{ph}\"/></a:xfrm>\
<a:custGeom><a:avLst/><a:gdLst/><a:ahLst/><a:cxnLst/><a:rect l=\"0\" t=\"0\" r=\"{pw}\" b=\"{ph}\"/>\
<a:pathLst><a:path w=\"{pw}\" h=\"{ph}\">{path}</a:path></a:pathLst></a:custGeom>\
{fill_xml}<a:ln w=\"{ww}\" cap=\"rnd\"><a:solidFill><a:srgbClr val=\"{stroke}\"/></a:solidFill></a:ln></p:spPr>\
<p:txBody><a:bodyPr/><a:p/></p:txBody></p:sp>",
        minx * PX, miny * PX
    )
}

/// 椭圆/圆(几何图、标记点)。fill=None 只描边(空心);Some 填充(实心点/扇形底)。
fn ellipse_xml(
    id: u32,
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    stroke: &str,
    width_px: i64,
    fill: Option<&str>,
) -> String {
    let fill_xml = match fill {
        Some(c) => format!("<a:solidFill><a:srgbClr val=\"{c}\"/></a:solidFill>"),
        None => "<a:noFill/>".to_string(),
    };
    let ww = width_px.max(1) * PX;
    format!(
        "<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"ell{id}\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>\
<p:spPr><a:xfrm><a:off x=\"{}\" y=\"{}\"/><a:ext cx=\"{}\" cy=\"{}\"/></a:xfrm>\
<a:prstGeom prst=\"ellipse\"><a:avLst/></a:prstGeom>{fill_xml}\
<a:ln w=\"{ww}\"><a:solidFill><a:srgbClr val=\"{stroke}\"/></a:solidFill></a:ln></p:spPr>\
<p:txBody><a:bodyPr/><a:p/></p:txBody></p:sp>",
        x * PX,
        y * PX,
        w.max(1) * PX,
        h.max(1) * PX
    )
}

/// 半透明蒙版(scrim):压在全幅图上垫文字,否则亮图上的白字必糊。
/// alpha 用千分点(55000 = 55% 不透明)。
fn scrim_rect(id: u32, x: i64, y: i64, w: i64, h: i64, color: &str, alpha: u32) -> String {
    format!(
        "<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"scrim{id}\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>\
<p:spPr><a:xfrm><a:off x=\"{}\" y=\"{}\"/><a:ext cx=\"{}\" cy=\"{}\"/></a:xfrm>\
<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom>\
<a:solidFill><a:srgbClr val=\"{color}\"><a:alpha val=\"{alpha}\"/></a:srgbClr></a:solidFill>\
<a:ln><a:noFill/></a:ln></p:spPr><p:txBody><a:bodyPr/><a:p/></p:txBody></p:sp>",
        x * PX,
        y * PX,
        w * PX,
        h * PX
    )
}

/// 页背景:上下渐变(p:bg,真填充,用户可在 PowerPoint 里整页换色)。
fn slide_bg(pal: &Palette) -> String {
    format!(
        "<p:bg><p:bgPr><a:gradFill><a:gsLst>\
<a:gs pos=\"0\"><a:srgbClr val=\"{}\"/></a:gs>\
<a:gs pos=\"100000\"><a:srgbClr val=\"{}\"/></a:gs>\
</a:gsLst><a:lin ang=\"5400000\" scaled=\"1\"/></a:gradFill><a:effectLst/></p:bgPr></p:bg>",
        pal.bg1, pal.bg2
    )
}

// ─────────────────────── 版式布局 ───────────────────────

/// 内容页公共题头:标题 + 强调下划线。返回 (XML, 下一个可用 id)。
fn header(title: &str, pal: &Palette, mut id: u32) -> (String, u32) {
    let mut s = String::new();
    if !title.is_empty() {
        // 页标题 26→32:实测最长标题 16 个汉字,32pt 下宽 682px,题头框有 1120px,富余很大。
        // 极长标题走 autofit 收回去,不撑破题头带。
        let size = autofit(
            &[FitLine {
                em: em_width(title),
                rel: 0,
                after: 0.0,
            }],
            1120,
            64,
            22,
            32,
        );
        let p = Para {
            text: title,
            size_pt: size,
            bold: true,
            ..Para::plain(title, size, pal.ink)
        };
        s.push_str(&text_box(id, 80, 50, 1120, 64, "t", &para_xml(&p, pal)));
        id += 1;
        s.push_str(&solid_rect(id, 80, 122, 72, 4, pal.accent));
        id += 1;
    }
    (s, id)
}

fn s_str<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

// ─────────────────────── 自适应字号(autofit) ───────────────────────
//
// 字号写死是「一边很空、字还很小」的根因:同一个 17pt 既要伺候 3 行的页,也要伺候 12 行的页
// —— 结果前者空一大片、后者刚好。这里按**每页实际内容量**反算能放下的最大字号:行少就撑大,
// 行多才收。上界给得比过去的固定值大得多,下界兜住极端长页。

/// 1pt = 96/72 px(与 PX 常量同一套 96dpi 换算)。
const PT2PX: f64 = 96.0 / 72.0;
/// 行高系数:与 PowerPoint 单倍行距的实测观感对齐(略留余量,宁可小一点也别撑爆)。
const LINE_H: f64 = 1.32;

/// 估算「em 宽度」:CJK/全角字占 1em,拉丁/数字约 0.55em。
/// 不做真字体度量 —— 引擎不解析 TTF,估算够用:autofit 只需要知道「会不会折行」。
fn em_width(s: &str) -> f64 {
    s.chars()
        .map(|c| {
            let u = c as u32;
            // CJK 统一表意 / 兼容表意 / 全角标点 / CJK 标点 → 全角
            if (0x2E80..=0x9FFF).contains(&u)
                || (0xF900..=0xFAFF).contains(&u)
                || (0xFF00..=0xFFEF).contains(&u)
                || (0x3000..=0x303F).contains(&u)
            {
                1.0
            } else {
                0.55
            }
        })
        .sum()
}

/// 一行待排文本的度量输入。`rel` = 相对主字号的偏移(二级子条 -3),`after` = 段后距 pt。
struct FitLine {
    em: f64,
    rel: i64,
    after: f64,
}

/// 在 [min_pt, max_pt] 里挑**最大的、能在 (w,h) px 盒内放下全部行**的字号。
/// 逐档下探而非解析求解:档位少(十几档),而且折行是阶跃的,闭式解反而不准。
fn autofit(lines: &[FitLine], w: i64, h: i64, min_pt: i64, max_pt: i64) -> i64 {
    if lines.is_empty() {
        return max_pt;
    }
    for size in (min_pt..=max_pt).rev() {
        let mut total = 0.0f64;
        for l in lines {
            let s = (size + l.rel).max(6) as f64;
            // 该字号下一行能放多少 em → folding 出行数
            let per_row = (w as f64 / (s * PT2PX)).max(1.0);
            let rows = (l.em / per_row).ceil().max(1.0);
            total += rows * s * LINE_H * PT2PX + l.after * PT2PX;
        }
        if total <= h as f64 {
            return size;
        }
    }
    min_pt
}

/// 从 points 数组构造度量行,规则与 [`points_paras`] 的排版**一一对应**
/// (改了那边的 space_after / 子条降档,这里必须同步,否则 autofit 会算错)。
fn point_fit_lines(points: Option<&Value>, out: &mut Vec<FitLine>) {
    let Some(arr) = points.and_then(|v| v.as_array()) else {
        return;
    };
    for p in arr {
        if let Some(t) = p.as_str() {
            out.push(FitLine {
                em: em_width(t),
                rel: 0,
                after: 8.0,
            });
        } else if let Some(o) = p.as_object() {
            let t = o.get("text").and_then(|x| x.as_str()).unwrap_or("");
            if !t.is_empty() {
                out.push(FitLine {
                    em: em_width(t),
                    rel: 0,
                    after: 4.0,
                });
            }
            if let Some(subs) = o.get("sub").and_then(|x| x.as_array()) {
                for s in subs.iter().filter_map(|x| x.as_str()) {
                    out.push(FitLine {
                        em: em_width(s),
                        rel: -3,
                        after: 4.0,
                    });
                }
            }
        }
    }
}

/// bullet 悬挂缩进吃掉的宽度(marL=285750 EMU)。算可用宽时要扣。
const BULLET_INDENT: i64 = 30;

/// freeform 盒子的整型坐标/尺寸(接受 JSON number,含小数一律取整)。
fn s_i64(v: &Value, k: &str, default: i64) -> i64 {
    v.get(k)
        .and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f.round() as i64)))
        .unwrap_or(default)
}

fn s_bool(v: &Value, k: &str, default: bool) -> bool {
    v.get(k).and_then(|x| x.as_bool()).unwrap_or(default)
}

/// 颜色归一化:`#RRGGBB`/`#RGB`/无#十六进制/色板词 → OOXML 用的裸 6 位大写 hex。
/// 认不出就退 `fallback`(而非硬塞非法值让 PowerPoint 报修复)。
fn norm_color(raw: &str, pal: &Palette, fallback: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        return fallback.to_string();
    }
    match t.to_ascii_lowercase().as_str() {
        "ink" | "text" => return pal.ink.to_string(),
        "muted" => return pal.muted.to_string(),
        "accent" => return pal.accent.to_string(),
        "card" => return pal.card.to_string(),
        "line" => return pal.card_line.to_string(),
        "bg" | "bg1" => return pal.bg1.to_string(),
        "bg2" => return pal.bg2.to_string(),
        "white" => return "FFFFFF".to_string(),
        "black" => return "000000".to_string(),
        _ => {}
    }
    let hex = t.trim_start_matches('#');
    if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        hex.to_ascii_uppercase()
    } else if hex.len() == 3 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        hex.chars().flat_map(|c| [c, c]).collect::<String>().to_ascii_uppercase()
    } else {
        fallback.to_string()
    }
}

/// freeform 页里各 image 盒子的图路径(按出现顺序);非 freeform 页返回空。
/// 与渲染时消费的顺序严格一致,故预载与渲染都靠这个顺序对齐 rId。
fn freeform_image_paths(sl: &Value) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(boxes) = sl.get("boxes").and_then(|x| x.as_array()) {
        for b in boxes {
            let ty = s_str(b, "type");
            if ty == "image" || ty == "pic" {
                v.push(s_str(b, "image").trim().to_string());
            }
        }
    }
    v
}

/// spec 由 LLM 产出,字段类型错(如 points 给了字符串)很现实——静默丢整页内容比
/// 未知版式更隐蔽,必须与之同等待遇进 warnings。
fn warn_bad_type(v: Option<&Value>, what: &str, page: usize, warnings: &mut Vec<String>) {
    if let Some(x) = v {
        if !x.is_array() && !x.is_null() {
            warnings.push(format!("第 {page} 页 {what} 不是数组,内容已忽略"));
        }
    }
}

/// points 数组 → bullet 段落串(支持 string 或 {text, sub:[…]} 两级)。
fn points_paras(points: Option<&Value>, size_pt: i64, pal: &Palette) -> String {
    let mut out = String::new();
    let Some(arr) = points.and_then(|v| v.as_array()) else {
        return out;
    };
    for p in arr {
        if let Some(t) = p.as_str() {
            out.push_str(&para_xml(
                &Para {
                    bullet: Some(0),
                    space_after_pt: 8,
                    ..Para::plain(t, size_pt, pal.ink)
                },
                pal,
            ));
        } else if let Some(o) = p.as_object() {
            let t = o.get("text").and_then(|x| x.as_str()).unwrap_or("");
            if !t.is_empty() {
                out.push_str(&para_xml(
                    &Para {
                        bullet: Some(0),
                        space_after_pt: 4,
                        ..Para::plain(t, size_pt, pal.ink)
                    },
                    pal,
                ));
            }
            if let Some(subs) = o.get("sub").and_then(|x| x.as_array()) {
                for sline in subs {
                    if let Some(st) = sline.as_str() {
                        out.push_str(&para_xml(
                            &Para {
                                bullet: Some(1),
                                space_after_pt: 4,
                                ..Para::plain(st, size_pt - 3, pal.muted)
                            },
                            pal,
                        ));
                    }
                }
            }
        }
    }
    out
}

/// 配图在页内的关系 id。rId1=slideLayout、rId2=notesSlide(可选),图固定占 rId3 ——
/// rels 的 Id 只要与引用处对得上即可,不要求连号,固定值省掉一层「按有无备注算偏移」的耦合。
const IMG_RID: &str = "rId3";

/// 单页 spec → spTree 内容 XML。未知版式宽容降级 bullets,warnings 收集。
/// `img` 为该页配图的原始像素尺寸(已成功载入时才有);image-* 版式缺图即降级。
/// freeform 各 image 盒子的 rId 从此起编(避开 rId1 layout / rId2 notes / rId3 单图槽)。
const FREE_IMG_RID_BASE: u32 = 10;

fn slide_content(
    sl: &Value,
    pal: &Palette,
    warnings: &mut Vec<String>,
    page: usize,
    img: Option<(u32, u32)>,
    free: &[Option<SlideImage>],
    anims: &mut Vec<(u32, u32)>,
) -> String {
    let layout = s_str(sl, "layout");
    let mut id = 10u32;
    let mut s = String::new();
    // image-* 版式没拿到图 → 退成对应的无图版式,别整页空白。缺图多半是模型写错路径
    // 或生图失败,静默出一页白纸比报错更难查,故必须进 warnings。
    let layout = match (layout, img.is_some()) {
        ("image-full", false) => {
            warnings.push(format!("第 {page} 页 image-full 缺可用配图,已按 title 渲染"));
            "title"
        }
        ("image-text", false) => {
            warnings.push(format!("第 {page} 页 image-text 缺可用配图,已按 bullets 渲染"));
            "bullets"
        }
        (l, _) => l,
    };
    match layout {
        "image-full" => {
            // 全幅图 + 暗蒙版 + 居中大标题(与 title 版式同坐标,换到图上)。
            let (iw, ih) = img.unwrap();
            s.push_str(&pic_xml(
                id, IMG_RID, 0, 0, CANVAS_W, CANVAS_H, iw, ih, false,
            ));
            id += 1;
            s.push_str(&scrim_rect(id, 0, 0, CANVAS_W, CANVAS_H, "000000", 50_000));
            id += 1;
            // 蒙版上一律用白字:色板的 ink 在深色蒙版上可能是深色(warm-paper/forest 等浅板),
            // 照搬会糊成一片。
            let kicker = s_str(sl, "kicker");
            if !kicker.is_empty() {
                let p = Para {
                    align: "ctr",
                    bold: true,
                    ..Para::plain(kicker, 17, "FFFFFF")
                };
                s.push_str(&text_box(id, 160, 218, 960, 32, "t", &para_xml(&p, pal)));
                id += 1;
            }
            // 与 title 版式同一套字号策略(图上白字),两者观感必须一致。
            let ftitle = s_str(sl, "title");
            let tsize = autofit(
                &[FitLine {
                    em: em_width(ftitle),
                    rel: 0,
                    after: 0.0,
                }],
                1120,
                110,
                30,
                50,
            );
            let p = Para {
                align: "ctr",
                bold: true,
                ..Para::plain(ftitle, tsize, "FFFFFF")
            };
            s.push_str(&text_box(id, 80, 268, 1120, 110, "t", &para_xml(&p, pal)));
            id += 1;
            s.push_str(&solid_rect(id, 598, 392, 84, 4, pal.accent));
            id += 1;
            let sub = s_str(sl, "subtitle");
            if !sub.is_empty() {
                let ssize = autofit(
                    &[FitLine {
                        em: em_width(sub),
                        rel: 0,
                        after: 0.0,
                    }],
                    960,
                    70,
                    14,
                    24,
                );
                let p = Para {
                    align: "ctr",
                    ..Para::plain(sub, ssize, "E8E8E8")
                };
                s.push_str(&text_box(id, 160, 420, 960, 70, "t", &para_xml(&p, pal)));
            }
        }
        "image-text" => {
            // 图文分栏:图占一半(与 two-col 的卡片同几何),要点占另一半。
            let (iw, ih) = img.unwrap();
            let (h, nid) = header(s_str(sl, "title"), pal, id);
            s.push_str(&h);
            id = nid;
            let img_right = s_str(sl, "side").eq_ignore_ascii_case("right");
            let (img_x, txt_x) = if img_right { (656, 80) } else { (80, 656) };
            s.push_str(&pic_xml(id, IMG_RID, img_x, 168, 544, 470, iw, ih, true));
            id += 1;
            warn_bad_type(sl.get("points"), "points", page, warnings);
            let mut fl = Vec::new();
            if !s_str(sl, "head").is_empty() {
                fl.push(FitLine {
                    em: em_width(s_str(sl, "head")),
                    rel: 2,
                    after: 10.0,
                });
            }
            point_fit_lines(sl.get("points"), &mut fl);
            let tsize = autofit(&fl, 544 - BULLET_INDENT, 446, 13, 30);
            let mut paras = String::new();
            let head = s_str(sl, "head");
            if !head.is_empty() {
                paras.push_str(&para_xml(
                    &Para {
                        bold: true,
                        space_after_pt: 10,
                        ..Para::plain(head, tsize + 2, pal.accent)
                    },
                    pal,
                ));
            }
            paras.push_str(&points_paras(sl.get("points"), tsize, pal));
            if !paras.is_empty() {
                // 与图框(y=168..638)垂直居中对齐:文字侧短时贴着上边,会和旁边的图错位。
                s.push_str(&text_box(id, txt_x, 180, 544, 446, "ctr", &paras));
            }
        }
        "title" | "closing" => {
            let kicker = s_str(sl, "kicker");
            if !kicker.is_empty() {
                let p = Para {
                    align: "ctr",
                    bold: true,
                    ..Para::plain(kicker, 17, pal.accent)
                };
                s.push_str(&text_box(id, 160, 218, 960, 32, "t", &para_xml(&p, pal)));
                id += 1;
            }
            let title = if s_str(sl, "title").is_empty() && layout == "closing" {
                "谢谢"
            } else {
                s_str(sl, "title")
            };
            // 封面主标题 40→50(实测最长 15 字,50pt 下 1000px < 1120 框);超长自动收。
            let tsize = autofit(
                &[FitLine {
                    em: em_width(title),
                    rel: 0,
                    after: 0.0,
                }],
                1120,
                110,
                30,
                50,
            );
            let p = Para {
                align: "ctr",
                bold: true,
                ..Para::plain(title, tsize, pal.ink)
            };
            s.push_str(&text_box(id, 80, 268, 1120, 110, "t", &para_xml(&p, pal)));
            id += 1;
            s.push_str(&solid_rect(id, 598, 392, 84, 4, pal.accent));
            id += 1;
            let sub = s_str(sl, "subtitle");
            if !sub.is_empty() {
                let ssize = autofit(
                    &[FitLine {
                        em: em_width(sub),
                        rel: 0,
                        after: 0.0,
                    }],
                    960,
                    70,
                    14,
                    24,
                );
                let p = Para {
                    align: "ctr",
                    ..Para::plain(sub, ssize, pal.muted)
                };
                s.push_str(&text_box(id, 160, 420, 960, 70, "t", &para_xml(&p, pal)));
            }
        }
        "section" => {
            s.push_str(&solid_rect(id, 80, 290, 8, 130, pal.accent));
            id += 1;
            let kicker = s_str(sl, "kicker");
            if !kicker.is_empty() {
                let p = Para {
                    bold: true,
                    ..Para::plain(kicker, 17, pal.accent)
                };
                s.push_str(&text_box(id, 116, 296, 1000, 32, "t", &para_xml(&p, pal)));
                id += 1;
            }
            // 章节页整页就一句话,更该大:34→44。
            let stitle = s_str(sl, "title");
            let size = autofit(
                &[FitLine {
                    em: em_width(stitle),
                    rel: 0,
                    after: 0.0,
                }],
                1040,
                90,
                26,
                44,
            );
            let p = Para {
                bold: true,
                ..Para::plain(stitle, size, pal.ink)
            };
            s.push_str(&text_box(id, 116, 336, 1040, 90, "t", &para_xml(&p, pal)));
        }
        "two-col" => {
            let (h, nid) = header(s_str(sl, "title"), pal, id);
            s.push_str(&h);
            id = nid;
            // 两栏共用同一字号:左右各算一次取小的,否则一栏大一栏小,像两页拼起来的。
            let col_size = ["left", "right"]
                .iter()
                .filter_map(|k| sl.get(*k))
                .map(|col| {
                    let mut fl = Vec::new();
                    if !s_str(col, "head").is_empty() {
                        fl.push(FitLine {
                            em: em_width(s_str(col, "head")),
                            rel: 2,
                            after: 8.0,
                        });
                    }
                    point_fit_lines(col.get("points"), &mut fl);
                    autofit(&fl, 488 - BULLET_INDENT, 414, 13, 24)
                })
                .min()
                .unwrap_or(15);
            for (i, key) in ["left", "right"].iter().enumerate() {
                let x = 80 + (i as i64) * 576;
                if let Some(col) = sl.get(*key) {
                    let mut paras = String::new();
                    let head = s_str(col, "head");
                    if !head.is_empty() {
                        paras.push_str(&para_xml(
                            &Para {
                                bold: true,
                                space_after_pt: 8,
                                ..Para::plain(head, col_size + 2, pal.accent)
                            },
                            pal,
                        ));
                    }
                    warn_bad_type(col.get("points"), &format!("{key}.points"), page, warnings);
                    paras.push_str(&points_paras(col.get("points"), col_size, pal));
                    if !paras.is_empty() {
                        s.push_str(&round_card(id, x, 168, 544, 470, pal));
                        id += 1;
                        // 卡内居中:短内容顶着卡片上沿会让下半张卡空着,像卡画大了。
                        s.push_str(&text_box(id, x + 28, 196, 544 - 56, 470 - 56, "ctr", &paras));
                        id += 1;
                    }
                }
            }
        }
        "compare" => {
            let (h, nid) = header(s_str(sl, "title"), pal, id);
            s.push_str(&h);
            id = nid;
            warn_bad_type(sl.get("items"), "items", page, warnings);
            let full = sl
                .get("items")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            if full > 4 {
                warnings.push(format!(
                    "第 {page} 页 compare 仅渲染前 4 项,丢弃 {} 项",
                    full - 4
                ));
            }
            let items: Vec<&Value> = sl
                .get("items")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().take(4).collect())
                .unwrap_or_default();
            let n = items.len().max(1) as i64;
            let gap = 28i64;
            let w = (1120 - gap * (n - 1)) / n;
            // 全卡统一字号(各卡算一次取最小):卡片并排,字号不齐会很扎眼。
            // 2 卡时每卡宽 546,4 卡时只剩 236 —— 宽度进了 autofit,窄卡自然收得更小。
            let card_size = items
                .iter()
                .map(|it| {
                    let mut fl = Vec::new();
                    if !s_str(it, "head").is_empty() {
                        fl.push(FitLine {
                            em: em_width(s_str(it, "head")),
                            rel: 3,
                            after: 8.0,
                        });
                    }
                    for line in s_str(it, "body").split('\n').filter(|l| !l.trim().is_empty()) {
                        fl.push(FitLine {
                            em: em_width(line.trim()),
                            rel: 0,
                            after: 6.0,
                        });
                    }
                    point_fit_lines(it.get("points"), &mut fl);
                    autofit(&fl, w - 48 - BULLET_INDENT, 382, 11, 22)
                })
                .min()
                .unwrap_or(14);
            for (i, it) in items.iter().enumerate() {
                let x = 80 + (i as i64) * (w + gap);
                s.push_str(&round_card(id, x, 180, w, 430, pal));
                id += 1;
                let mut paras = String::new();
                let head = s_str(it, "head");
                if !head.is_empty() {
                    paras.push_str(&para_xml(
                        &Para {
                            bold: true,
                            space_after_pt: 8,
                            ..Para::plain(head, card_size + 3, pal.accent)
                        },
                        pal,
                    ));
                }
                let body = s_str(it, "body");
                if !body.is_empty() {
                    for line in body.split('\n').filter(|l| !l.trim().is_empty()) {
                        paras.push_str(&para_xml(
                            &Para {
                                space_after_pt: 6,
                                ..Para::plain(line.trim(), card_size, pal.ink)
                            },
                            pal,
                        ));
                    }
                }
                paras.push_str(&points_paras(it.get("points"), card_size, pal));
                s.push_str(&text_box(id, x + 24, 204, w - 48, 430 - 48, "ctr", &paras));
                id += 1;
            }
        }
        "stats" => {
            // 大数字指标页: 1–4 张卡,每卡 value(超大强调色) + label + 可选 desc。
            let (h, nid) = header(s_str(sl, "title"), pal, id);
            s.push_str(&h);
            id = nid;
            warn_bad_type(sl.get("items"), "items", page, warnings);
            let full = sl
                .get("items")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            if full > 4 {
                warnings.push(format!(
                    "第 {page} 页 stats 仅渲染前 4 项,丢弃 {} 项",
                    full - 4
                ));
            }
            let items: Vec<&Value> = sl
                .get("items")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().take(4).collect())
                .unwrap_or_default();
            let n = items.len().max(1) as i64;
            let gap = 28i64;
            let w = (1120 - gap * (n - 1)) / n;
            for (i, it) in items.iter().enumerate() {
                let x = 80 + (i as i64) * (w + gap);
                s.push_str(&round_card(id, x, 220, w, 320, pal));
                id += 1;
                let mut paras = String::new();
                let value = s_str(it, "value");
                if !value.is_empty() {
                    // 大数字是 stats 页的主角,单独 autofit:短数字(如 9.8)撑到 60pt,
                    // 长公式(如 F₁l₁ = F₂l₂)自动收,不挤出卡片。
                    let vsize = autofit(
                        &[FitLine {
                            em: em_width(value),
                            rel: 0,
                            after: 0.0,
                        }],
                        w - 40,
                        120,
                        22,
                        60,
                    );
                    paras.push_str(&para_xml(
                        &Para {
                            align: "ctr",
                            bold: true,
                            space_after_pt: 10,
                            ..Para::plain(value, vsize, pal.accent)
                        },
                        pal,
                    ));
                }
                let label = s_str(it, "label");
                if !label.is_empty() {
                    paras.push_str(&para_xml(
                        &Para {
                            align: "ctr",
                            bold: true,
                            space_after_pt: 6,
                            ..Para::plain(label, 20, pal.ink)
                        },
                        pal,
                    ));
                }
                let desc = s_str(it, "desc");
                if !desc.is_empty() {
                    // desc 常是长句(实测最长 25 个汉字),窄卡里必须收得住。
                    let dsize = autofit(
                        &[FitLine {
                            em: em_width(desc),
                            rel: 0,
                            after: 0.0,
                        }],
                        w - 40,
                        90,
                        10,
                        16,
                    );
                    paras.push_str(&para_xml(
                        &Para {
                            align: "ctr",
                            ..Para::plain(desc, dsize, pal.muted)
                        },
                        pal,
                    ));
                }
                s.push_str(&text_box(id, x + 20, 244, w - 40, 320 - 48, "ctr", &paras));
                id += 1;
            }
        }
        "timeline" => {
            // 流程/路线图页: 2–5 步,数字圆节点 + 连接线 + 每步 head/body。
            let (h, nid) = header(s_str(sl, "title"), pal, id);
            s.push_str(&h);
            id = nid;
            warn_bad_type(sl.get("steps"), "steps", page, warnings);
            let full = sl
                .get("steps")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            if full > 5 {
                warnings.push(format!(
                    "第 {page} 页 timeline 仅渲染前 5 步,丢弃 {} 步",
                    full - 5
                ));
            }
            let steps: Vec<&Value> = sl
                .get("steps")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().take(5).collect())
                .unwrap_or_default();
            let n = steps.len().max(1) as i64;
            let gap = 24i64;
            let w = (1120 - gap * (n - 1)) / n;
            // 连接线先画(在圆下层),从首步圆心到末步圆心。
            if n > 1 {
                let x0 = 80 + w / 2;
                let span = (n - 1) * (w + gap);
                s.push_str(&solid_rect(id, x0, 250, span, 3, pal.card_line));
                id += 1;
            }
            // 各步统一字号取最小:步骤并排,字号不齐比字小更难看。
            let step_size = steps
                .iter()
                .map(|st| {
                    let mut fl = Vec::new();
                    if !s_str(st, "head").is_empty() {
                        fl.push(FitLine {
                            em: em_width(s_str(st, "head")),
                            rel: 3,
                            after: 6.0,
                        });
                    }
                    for line in s_str(st, "body").split('\n').filter(|l| !l.trim().is_empty()) {
                        fl.push(FitLine {
                            em: em_width(line.trim()),
                            rel: 0,
                            after: 4.0,
                        });
                    }
                    autofit(&fl, w, 320, 10, 20)
                })
                .min()
                .unwrap_or(13);
            for (i, st) in steps.iter().enumerate() {
                let x = 80 + (i as i64) * (w + gap);
                let num = (i + 1).to_string();
                s.push_str(&circle_num(id, x + w / 2 - 22, 230, 44, &num, pal));
                id += 1;
                let mut paras = String::new();
                let head = s_str(st, "head");
                if !head.is_empty() {
                    paras.push_str(&para_xml(
                        &Para {
                            align: "ctr",
                            bold: true,
                            space_after_pt: 6,
                            ..Para::plain(head, step_size + 3, pal.ink)
                        },
                        pal,
                    ));
                }
                let body = s_str(st, "body");
                for line in body.split('\n').filter(|l| !l.trim().is_empty()) {
                    paras.push_str(&para_xml(
                        &Para {
                            align: "ctr",
                            space_after_pt: 4,
                            ..Para::plain(line.trim(), step_size, pal.muted)
                        },
                        pal,
                    ));
                }
                if !paras.is_empty() {
                    s.push_str(&text_box(id, x, 296, w, 320, "t", &paras));
                    id += 1;
                }
            }
        }
        "quote" => {
            let p = Para {
                bold: true,
                ..Para::plain("\u{201C}", 96, pal.accent)
            };
            s.push_str(&text_box(id, 100, 120, 200, 130, "t", &para_xml(&p, pal)));
            id += 1;
            // 金句页就该大:短句给到 40pt,长引文(如整阕词)自动收。
            let qtext = s_str(sl, "text");
            let qsize = autofit(
                &[FitLine {
                    em: em_width(qtext),
                    rel: 0,
                    after: 0.0,
                }],
                960,
                220,
                18,
                40,
            );
            let p = Para {
                align: "ctr",
                italic: true,
                ..Para::plain(qtext, qsize, pal.ink)
            };
            s.push_str(&text_box(id, 160, 250, 960, 220, "ctr", &para_xml(&p, pal)));
            id += 1;
            let by = s_str(sl, "by");
            if !by.is_empty() {
                let byline = format!("—— {by}");
                let p = Para {
                    align: "ctr",
                    ..Para::plain(&byline, 18, pal.muted)
                };
                s.push_str(&text_box(id, 160, 490, 960, 40, "t", &para_xml(&p, pal)));
            }
        }
        "freeform" => {
            // 自由版式:一页任意盒子,x/y/w/h 逻辑 px。摆脱固定版式的出口。
            let empty: Vec<Value> = Vec::new();
            let boxes = sl.get("boxes").and_then(|x| x.as_array()).unwrap_or(&empty);
            if boxes.is_empty() {
                warnings.push(format!("第 {page} 页 freeform 无 boxes,出空白页"));
            }
            let mut fi = 0usize; // 第几个 image 盒子(与 free 切片、rId 对齐)
            for b in boxes {
                let x = s_i64(b, "x", 0);
                let y = s_i64(b, "y", 0);
                let w = s_i64(b, "w", 100).max(1);
                let h = s_i64(b, "h", 100).max(1);
                // click:第几次单击时淡入出现(0/缺省=随页立即显示)。用于数学图「一笔笔加」。
                let click = s_i64(b, "click", 0).max(0) as u32;
                let stroke = norm_color(s_str(b, "color"), pal, pal.ink);
                let width = s_i64(b, "width", 3).clamp(1, 40);
                let fill_opt = b
                    .get("fill")
                    .and_then(|x| x.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| norm_color(s, pal, pal.accent));
                let sid = id; // 本盒将占用的形状 id(供动画定位);仅在真产出形状时记账。
                let mut emitted = false;
                match s_str(b, "type") {
                    "line" | "arrow" | "axis" => {
                        let x2 = s_i64(b, "x2", x + w);
                        let y2 = s_i64(b, "y2", y);
                        let arrow = s_str(b, "type") == "arrow"
                            || s_str(b, "type") == "axis"
                            || s_bool(b, "arrow", false);
                        let dash = s_bool(b, "dash", false);
                        s.push_str(&line_xml(id, x, y, x2, y2, &stroke, width, arrow, dash));
                        id += 1;
                        emitted = true;
                    }
                    "polyline" | "curve" | "polygon" => {
                        let pts = parse_points(b.get("points"));
                        if pts.len() >= 2 {
                            let closed =
                                s_str(b, "type") == "polygon" || s_bool(b, "closed", false);
                            s.push_str(&polyline_xml(
                                id,
                                &pts,
                                &stroke,
                                width,
                                closed,
                                fill_opt.as_deref(),
                            ));
                            id += 1;
                            emitted = true;
                        } else {
                            warnings.push(format!("第 {page} 页 freeform 折线缺 points(≥2 点)"));
                        }
                    }
                    "ellipse" | "circle" => {
                        // circle:给 r 则以 (x,y) 为圆心画直径 2r 的圆;否则用 x/y/w/h 作外接框。
                        let r = s_i64(b, "r", 0);
                        let (ex, ey, ew, eh) = if r > 0 {
                            (x - r, y - r, 2 * r, 2 * r)
                        } else {
                            (x, y, w, h)
                        };
                        s.push_str(&ellipse_xml(id, ex, ey, ew, eh, &stroke, width, fill_opt.as_deref()));
                        id += 1;
                        emitted = true;
                    }
                    "point" | "dot" => {
                        // 实心标记点:以 (x,y) 为圆心,半径 r(默认 6px)。
                        let r = s_i64(b, "r", 6).max(1);
                        let color = fill_opt.clone().unwrap_or(stroke.clone());
                        s.push_str(&ellipse_xml(
                            id,
                            x - r,
                            y - r,
                            2 * r,
                            2 * r,
                            &color,
                            1,
                            Some(&color),
                        ));
                        id += 1;
                        emitted = true;
                    }
                    "text" => {
                        let align = match s_str(b, "align") {
                            "center" | "ctr" | "c" => "ctr",
                            "right" | "r" | "end" => "r",
                            _ => "l",
                        };
                        let anchor = match s_str(b, "anchor") {
                            "middle" | "center" | "ctr" => "ctr",
                            "bottom" | "b" => "b",
                            _ => "t",
                        };
                        let size = s_i64(b, "size", 18).clamp(4, 400);
                        let color = norm_color(s_str(b, "color"), pal, pal.ink);
                        let bold = s_bool(b, "bold", false);
                        let italic = s_bool(b, "italic", false);
                        let mut paras = String::new();
                        if let Some(lines) = b.get("lines").and_then(|x| x.as_array()) {
                            for ln in lines {
                                if let Some(t) = ln.as_str() {
                                    let p = Para {
                                        align,
                                        bold,
                                        italic,
                                        ..Para::plain(t, size, &color)
                                    };
                                    paras.push_str(&para_xml(&p, pal));
                                }
                            }
                        } else {
                            let p = Para {
                                align,
                                bold,
                                italic,
                                ..Para::plain(s_str(b, "text"), size, &color)
                            };
                            paras.push_str(&para_xml(&p, pal));
                        }
                        if !paras.is_empty() {
                            s.push_str(&text_box(id, x, y, w, h, anchor, &paras));
                            id += 1;
                            emitted = true;
                        }
                    }
                    "rect" | "bar" => {
                        let color = norm_color(s_str(b, "color"), pal, pal.accent);
                        s.push_str(&solid_rect(id, x, y, w, h, &color));
                        id += 1;
                        emitted = true;
                    }
                    "card" => {
                        s.push_str(&round_card(id, x, y, w, h, pal));
                        id += 1;
                        emitted = true;
                    }
                    "scrim" => {
                        let color = norm_color(s_str(b, "color"), pal, "000000");
                        let alpha = (s_i64(b, "alpha", 50).clamp(0, 100) as u32) * 1000;
                        s.push_str(&scrim_rect(id, x, y, w, h, &color, alpha));
                        id += 1;
                        emitted = true;
                    }
                    "image" | "pic" => {
                        let rid = format!("rId{}", FREE_IMG_RID_BASE + fi as u32);
                        match free.get(fi).and_then(|o| o.as_ref()) {
                            Some(im) => {
                                let cover = s_bool(b, "cover", true);
                                let rounded = s_bool(b, "rounded", false);
                                // cover:按原始尺寸 srcRect 裁切填满;非 cover:令 img 比例=框比例 → 拉伸铺满(不裁)。
                                let (iw, ih) = if cover {
                                    (im.w, im.h)
                                } else {
                                    (w as u32, h as u32)
                                };
                                s.push_str(&pic_xml(id, &rid, x, y, w, h, iw, ih, rounded));
                                id += 1;
                                emitted = true;
                            }
                            None => warnings.push(format!(
                                "第 {page} 页 freeform 第 {} 个图框无可用图,已跳过",
                                fi + 1
                            )),
                        }
                        fi += 1;
                    }
                    other => warnings.push(format!(
                        "第 {page} 页 freeform 未知盒子类型 \"{other}\",已跳过"
                    )),
                }
                // 记账:这个盒子确实产出了形状且要求单击显现 → 进动画序列。
                if emitted && click > 0 {
                    anims.push((sid, click));
                }
            }
        }
        other => {
            // bullets / 缺省 / 未知版式(宽容降级,尽量出东西)。缺 layout 与前端预览
            // 同样按 bullets 处理,不算错不告警;真未知才提醒。
            if other != "bullets" && !other.is_empty() {
                warnings.push(format!("第 {page} 页未知版式 \"{other}\",按 bullets 渲染"));
            }
            warn_bad_type(sl.get("points"), "points", page, warnings);
            let (h, nid) = header(s_str(sl, "title"), pal, id);
            s.push_str(&h);
            id = nid;
            // 内容少就把字撑大:3 行的页给到 36pt,12 行的页自动收到能放下为止。
            let mut fl = Vec::new();
            point_fit_lines(sl.get("points"), &mut fl);
            let size = autofit(&fl, 1120 - BULLET_INDENT, 470, 16, 36);
            let paras = points_paras(sl.get("points"), size, pal);
            if !paras.is_empty() {
                // anchor=ctr 而非 t:autofit 之后仍放不满时(如整页只有 3 条要点),
                // 顶对齐会在下方留一大片死白 —— 看着像没做完。居中则像有意的呼吸感。
                // 内容密的页本来就撑满,居中与顶对齐无差别,故无条件居中即可。
                s.push_str(&text_box(id, 80, 176, 1120, 470, "ctr", &paras));
            }
        }
    }
    s
}

/// 解析 freeform 盒子的 `points`:`[[x,y],[x,y],…]`(数字对) → 逻辑 px 顶点。
fn parse_points(v: Option<&Value>) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    if let Some(arr) = v.and_then(|x| x.as_array()) {
        for p in arr {
            if let Some(pt) = p.as_array() {
                if pt.len() >= 2 {
                    let x = pt[0].as_f64().map(|f| f.round() as i64);
                    let y = pt[1].as_f64().map(|f| f.round() as i64);
                    if let (Some(x), Some(y)) = (x, y) {
                        out.push((x, y));
                    }
                }
            }
        }
    }
    out
}

/// 单击构建动画:anims=(shape_id, click)。同 click 号的形状**一次单击一起淡入**;click 号升序
/// 即为逐步显现顺序。带动画的形状在放映时**初始隐藏**,单击到它那一步才现——这正是「点一下加一笔」。
/// 生成的 `<p:timing>` 贴合 PowerPoint 自身写法(mainSeq → 每步 par → clickEffect/withEffect + set 可见 + fade)。
fn build_timing(anims: &[(u32, u32)]) -> String {
    if anims.is_empty() {
        return String::new();
    }
    let mut clicks: Vec<u32> = anims.iter().map(|a| a.1).collect();
    clicks.sort_unstable();
    clicks.dedup();
    // 时间线节点 id 独立编号空间(与形状 id 无关),从 2 起(1 留给 tmRoot、seq/mainSeq 用固定 2/3)。
    let mut nid = 10u32;
    let mut next = || {
        let v = nid;
        nid += 1;
        v
    };
    let mut steps = String::new();
    for c in &clicks {
        let shapes: Vec<u32> = anims.iter().filter(|a| a.1 == *c).map(|a| a.0).collect();
        let mut effects = String::new();
        for (j, sid) in shapes.iter().enumerate() {
            let node = if j == 0 { "clickEffect" } else { "withEffect" };
            let (idc, idd, ide) = (next(), next(), next());
            effects.push_str(&format!(
                "<p:par><p:cTn id=\"{idc}\" presetClass=\"entr\" presetID=\"10\" presetSubtype=\"0\" fill=\"hold\" grpId=\"0\" nodeType=\"{node}\">\
<p:stCondLst><p:cond delay=\"0\"/></p:stCondLst><p:childTnLst>\
<p:set><p:cBhvr><p:cTn id=\"{idd}\" dur=\"1\" fill=\"hold\"><p:stCondLst><p:cond delay=\"0\"/></p:stCondLst></p:cTn>\
<p:tgtEl><p:spTgt spid=\"{sid}\"/></p:tgtEl><p:attrNameLst><p:attrName>style.visibility</p:attrName></p:attrNameLst></p:cBhvr>\
<p:to><p:strVal val=\"visible\"/></p:to></p:set>\
<p:animEffect transition=\"in\" filter=\"fade\"><p:cBhvr><p:cTn id=\"{ide}\" dur=\"400\"/>\
<p:tgtEl><p:spTgt spid=\"{sid}\"/></p:tgtEl></p:cBhvr></p:animEffect>\
</p:childTnLst></p:cTn></p:par>"
            ));
        }
        let (ida, idb) = (next(), next());
        steps.push_str(&format!(
            "<p:par><p:cTn id=\"{ida}\" fill=\"hold\"><p:stCondLst><p:cond delay=\"indefinite\"/></p:stCondLst><p:childTnLst>\
<p:par><p:cTn id=\"{idb}\" fill=\"hold\"><p:stCondLst><p:cond delay=\"0\"/></p:stCondLst><p:childTnLst>\
{effects}</p:childTnLst></p:cTn></p:par></p:childTnLst></p:cTn></p:par>"
        ));
    }
    // bldLst:声明每个受控形状按整体入场(build="p" 不适用于非文本,用形状级 bld)。
    let mut bld = String::new();
    for (sid, _) in anims {
        bld.push_str(&format!("<p:bldP spid=\"{sid}\" grpId=\"0\"/>"));
    }
    format!(
        "<p:timing><p:tnLst><p:par><p:cTn id=\"1\" dur=\"indefinite\" restart=\"never\" nodeType=\"tmRoot\">\
<p:childTnLst><p:seq concurrent=\"1\" nextAc=\"seek\"><p:cTn id=\"2\" dur=\"indefinite\" nodeType=\"mainSeq\">\
<p:childTnLst>{steps}</p:childTnLst></p:cTn>\
<p:prevCondLst><p:cond evt=\"onPrev\" delay=\"0\"><p:tgtEl><p:sldTgt/></p:tgtEl></p:cond></p:prevCondLst>\
<p:nextCondLst><p:cond evt=\"onNext\" delay=\"0\"><p:tgtEl><p:sldTgt/></p:tgtEl></p:cond></p:nextCondLst>\
</p:seq></p:childTnLst></p:cTn></p:par></p:tnLst><p:bldLst>{bld}</p:bldLst></p:timing>"
    )
}

fn native_slide_xml(content: &str, pal: &Palette, timing: &str) -> String {
    format!(
        "{decl}<p:sld xmlns:a=\"{a}\" xmlns:r=\"{r}\" xmlns:p=\"{p}\"><p:cSld>{bg}<p:spTree>\
<p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>\
<p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"0\" cy=\"0\"/></a:xfrm></p:grpSpPr>\
{content}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr>{timing}</p:sld>",
        decl = xml_decl(), a = NS_A, r = NS_R, p = NS_P, bg = slide_bg(pal)
    )
}

// ─────────────────────── 备注页(notesSlide) ───────────────────────

fn notes_master_xml() -> String {
    format!(
        "{decl}<p:notesMaster xmlns:a=\"{a}\" xmlns:r=\"{r}\" xmlns:p=\"{p}\"><p:cSld>\
<p:bg><p:bgPr><a:solidFill><a:srgbClr val=\"FFFFFF\"/></a:solidFill><a:effectLst/></p:bgPr></p:bg><p:spTree>\
<p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>\
<p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"0\" cy=\"0\"/></a:xfrm></p:grpSpPr>\
</p:spTree></p:cSld>\
<p:clrMap bg1=\"lt1\" tx1=\"dk1\" bg2=\"lt2\" tx2=\"dk2\" accent1=\"accent1\" accent2=\"accent2\" accent3=\"accent3\" accent4=\"accent4\" accent5=\"accent5\" accent6=\"accent6\" hlink=\"hlink\" folHlink=\"folHlink\"/>\
</p:notesMaster>",
        decl = xml_decl(), a = NS_A, r = NS_R, p = NS_P
    )
}

fn notes_slide_xml(notes: &str) -> String {
    let paras: String = notes
        .split('\n')
        .map(|l| {
            format!(
                "<a:p><a:r><a:rPr lang=\"zh-CN\"/><a:t>{}</a:t></a:r></a:p>",
                xml_escape(l)
            )
        })
        .collect();
    format!(
        "{decl}<p:notes xmlns:a=\"{a}\" xmlns:r=\"{r}\" xmlns:p=\"{p}\"><p:cSld><p:spTree>\
<p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>\
<p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"0\" cy=\"0\"/></a:xfrm></p:grpSpPr>\
<p:sp><p:nvSpPr><p:cNvPr id=\"2\" name=\"Notes Placeholder\"/><p:cNvSpPr><a:spLocks noGrp=\"1\"/></p:cNvSpPr>\
<p:nvPr><p:ph type=\"body\" idx=\"1\"/></p:nvPr></p:nvSpPr><p:spPr/>\
<p:txBody><a:bodyPr/>{paras}</p:txBody></p:sp>\
</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:notes>",
        decl = xml_decl(), a = NS_A, r = NS_R, p = NS_P
    )
}

// ─────────────────────── 打包 ───────────────────────

/// spec JSON 字符串 → .pptx。返回 {ok,out,slides,theme,notes_pages,warnings}。
pub fn build_pptx_from_spec(spec_json: &str, out_path: &str) -> Result<Value, String> {
    let spec: Value =
        serde_json::from_str(spec_json).map_err(|e| format!("spec JSON 解析失败: {e}"))?;
    let slides = spec
        .get("slides")
        .and_then(|v| v.as_array())
        .ok_or("spec 缺 slides 数组")?;
    if slides.is_empty() {
        return Err("spec.slides 为空,没有可生成的页".into());
    }
    if slides.len() > MAX_SLIDES {
        return Err(format!("页数 {} 超过上限 {MAX_SLIDES}", slides.len()));
    }
    let requested_theme = spec.get("theme").and_then(|v| v.as_str()).unwrap_or("");
    let (theme_name, pal) = palette(requested_theme);
    let n = slides.len();

    // 每页内容 + 备注。
    let mut warnings: Vec<String> = Vec::new();
    // 未知色板静默回退会让用户不知情(大小写写错都中招),与未知版式同等待遇。
    if !requested_theme.is_empty() && theme_name != requested_theme {
        warnings.push(format!(
            "未知色板 \"{requested_theme}\",已回退 {theme_name}"
        ));
    }
    // 配图先载入:slide_content 要靠原始尺寸算 cover 裁切,必须先于渲染。
    // 任何一张图坏掉都只降级该页(warning),不牵连整份 —— 一次生图失败毁掉整套课件不可接受。
    let mut images: Vec<Option<SlideImage>> = Vec::with_capacity(n);
    for (i, sl) in slides.iter().enumerate() {
        let page = i + 1;
        let path = s_str(sl, "image").trim();
        let layout = s_str(sl, "layout");
        let wants_img = layout == "image-full" || layout == "image-text";
        let im = if path.is_empty() {
            None
        } else if !wants_img {
            // 写了 image 却用了不吃图的版式:模型的常见误解,静默忽略会让人以为图丢了。
            warnings.push(format!(
                "第 {page} 页 \"{layout}\" 版式不支持配图,image 字段已忽略(要配图请用 image-full / image-text)"
            ));
            None
        } else {
            match load_slide_image(path) {
                Ok(im) => Some(im),
                Err(e) => {
                    warnings.push(format!("第 {page} 页配图不可用: {e}"));
                    None
                }
            }
        };
        images.push(im);
    }

    // freeform 页的盒内配图(每页可多张,按 boxes 里 image 盒出现顺序对齐 rId)。
    // 与单图槽同策略:任一张坏掉只降级该盒,不牵连整份。
    let mut free_imgs: Vec<Vec<Option<SlideImage>>> = Vec::with_capacity(n);
    for (i, sl) in slides.iter().enumerate() {
        let page = i + 1;
        let mut row = Vec::new();
        if s_str(sl, "layout") == "freeform" {
            for (k, path) in freeform_image_paths(sl).into_iter().enumerate() {
                if path.is_empty() {
                    row.push(None);
                    continue;
                }
                match load_slide_image(&path) {
                    Ok(im) => row.push(Some(im)),
                    Err(e) => {
                        warnings.push(format!("第 {page} 页 freeform 第 {} 图不可用: {e}", k + 1));
                        row.push(None);
                    }
                }
            }
        }
        free_imgs.push(row);
    }

    let mut slide_xmls: Vec<String> = Vec::with_capacity(n);
    let mut notes: Vec<Option<String>> = Vec::with_capacity(n);
    for (i, sl) in slides.iter().enumerate() {
        let dims = images[i].as_ref().map(|im| (im.w, im.h));
        let mut anims: Vec<(u32, u32)> = Vec::new();
        let content = slide_content(sl, &pal, &mut warnings, i + 1, dims, &free_imgs[i], &mut anims);
        let timing = build_timing(&anims);
        slide_xmls.push(native_slide_xml(&content, &pal, &timing));
        let nt = s_str(sl, "notes").trim().to_string();
        notes.push(if nt.is_empty() { None } else { Some(nt) });
    }
    let has_notes = notes.iter().any(|n| n.is_some());
    let has_images = images.iter().any(|x| x.is_some())
        || free_imgs.iter().any(|r| r.iter().any(|o| o.is_some()));

    if let Some(parent) = std::path::Path::new(out_path).parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    // 原子写:先写 .tmp 再 rename —— 半路失败不毁旧文件;目标被 PowerPoint 占用时给明确提示。
    let tmp_path = format!("{out_path}.tmp");
    let mut tmp_guard = crate::forge::pptx::TmpGuard(std::path::PathBuf::from(&tmp_path), true);
    let file =
        std::fs::File::create(&tmp_path).map_err(|e| format!("创建 {tmp_path} 失败: {e}"))?;
    let mut zip = zip::ZipWriter::new(file);
    let opt = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let put =
        |zip: &mut zip::ZipWriter<std::fs::File>, name: &str, data: &[u8]| -> Result<(), String> {
            zip.start_file(name, opt)
                .map_err(|e| format!("zip 写 {name} 失败: {e}"))?;
            zip.write_all(data)
                .map_err(|e| format!("zip 写入 {name} 失败: {e}"))?;
            Ok(())
        };

    // [Content_Types].xml
    let mut ct = String::from(xml_decl());
    ct.push_str(&format!("<Types xmlns=\"{NS_CT}\">"));
    ct.push_str("<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>");
    ct.push_str("<Default Extension=\"xml\" ContentType=\"application/xml\"/>");
    if has_images {
        // 两种都声明:同一份 spec 里 png 与 jpg 混用是常态(生图出 png、素材是 jpg)。
        ct.push_str("<Default Extension=\"png\" ContentType=\"image/png\"/>");
        ct.push_str("<Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>");
    }
    ct.push_str("<Override PartName=\"/ppt/presentation.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml\"/>");
    ct.push_str("<Override PartName=\"/ppt/slideMasters/slideMaster1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml\"/>");
    ct.push_str("<Override PartName=\"/ppt/slideLayouts/slideLayout1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml\"/>");
    ct.push_str("<Override PartName=\"/ppt/theme/theme1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.theme+xml\"/>");
    if has_notes {
        ct.push_str("<Override PartName=\"/ppt/theme/theme2.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.theme+xml\"/>");
        ct.push_str("<Override PartName=\"/ppt/notesMasters/notesMaster1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.notesMaster+xml\"/>");
    }
    for i in 1..=n {
        ct.push_str(&format!("<Override PartName=\"/ppt/slides/slide{i}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slide+xml\"/>"));
        if notes[i - 1].is_some() {
            ct.push_str(&format!("<Override PartName=\"/ppt/notesSlides/notesSlide{i}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.notesSlide+xml\"/>"));
        }
    }
    ct.push_str("</Types>");
    put(&mut zip, "[Content_Types].xml", ct.as_bytes())?;

    // _rels/.rels
    put(
        &mut zip,
        "_rels/.rels",
        format!(
            "{}<Relationships xmlns=\"{NS_REL}\"><Relationship Id=\"rId1\" Type=\"{NS_R}/officeDocument\" Target=\"ppt/presentation.xml\"/></Relationships>",
            xml_decl()
        )
        .as_bytes(),
    )?;

    // ppt/presentation.xml — rId1=master, rId2=notesMaster(可选), 之后 slides, 最后 theme。
    let slide_rid_base = if has_notes { 2 } else { 1 }; // slides 从 rId(base+1) 起
    let mut pres = String::from(xml_decl());
    pres.push_str(&format!(
        "<p:presentation xmlns:a=\"{NS_A}\" xmlns:r=\"{NS_R}\" xmlns:p=\"{NS_P}\">"
    ));
    pres.push_str(
        "<p:sldMasterIdLst><p:sldMasterId id=\"2147483648\" r:id=\"rId1\"/></p:sldMasterIdLst>",
    );
    if has_notes {
        pres.push_str("<p:notesMasterIdLst><p:notesMasterId r:id=\"rId2\"/></p:notesMasterIdLst>");
    }
    pres.push_str("<p:sldIdLst>");
    for i in 1..=n {
        pres.push_str(&format!(
            "<p:sldId id=\"{}\" r:id=\"rId{}\"/>",
            255 + i,
            slide_rid_base + i
        ));
    }
    pres.push_str("</p:sldIdLst>");
    pres.push_str(&format!(
        "<p:sldSz cx=\"{CX}\" cy=\"{CY}\"/><p:notesSz cx=\"6858000\" cy=\"9144000\"/></p:presentation>"
    ));
    put(&mut zip, "ppt/presentation.xml", pres.as_bytes())?;

    // ppt/_rels/presentation.xml.rels
    let mut prels = String::from(xml_decl());
    prels.push_str(&format!("<Relationships xmlns=\"{NS_REL}\">"));
    prels.push_str(&format!("<Relationship Id=\"rId1\" Type=\"{NS_R}/slideMaster\" Target=\"slideMasters/slideMaster1.xml\"/>"));
    if has_notes {
        prels.push_str(&format!("<Relationship Id=\"rId2\" Type=\"{NS_R}/notesMaster\" Target=\"notesMasters/notesMaster1.xml\"/>"));
    }
    for i in 1..=n {
        prels.push_str(&format!(
            "<Relationship Id=\"rId{}\" Type=\"{NS_R}/slide\" Target=\"slides/slide{i}.xml\"/>",
            slide_rid_base + i
        ));
    }
    prels.push_str(&format!(
        "<Relationship Id=\"rId{}\" Type=\"{NS_R}/theme\" Target=\"theme/theme1.xml\"/>",
        slide_rid_base + n + 1
    ));
    prels.push_str("</Relationships>");
    put(
        &mut zip,
        "ppt/_rels/presentation.xml.rels",
        prels.as_bytes(),
    )?;

    // theme / master / layout(与图片版共用同一套最小合法骨架)。
    put(&mut zip, "ppt/theme/theme1.xml", theme_xml().as_bytes())?;
    put(
        &mut zip,
        "ppt/slideMasters/slideMaster1.xml",
        slide_master_xml(CX, CY).as_bytes(),
    )?;
    put(
        &mut zip,
        "ppt/slideMasters/_rels/slideMaster1.xml.rels",
        format!(
            "{}<Relationships xmlns=\"{NS_REL}\"><Relationship Id=\"rId1\" Type=\"{NS_R}/slideLayout\" Target=\"../slideLayouts/slideLayout1.xml\"/><Relationship Id=\"rId2\" Type=\"{NS_R}/theme\" Target=\"../theme/theme1.xml\"/></Relationships>",
            xml_decl()
        )
        .as_bytes(),
    )?;
    put(
        &mut zip,
        "ppt/slideLayouts/slideLayout1.xml",
        slide_layout_xml(CX, CY).as_bytes(),
    )?;
    put(
        &mut zip,
        "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
        format!(
            "{}<Relationships xmlns=\"{NS_REL}\"><Relationship Id=\"rId1\" Type=\"{NS_R}/slideMaster\" Target=\"../slideMasters/slideMaster1.xml\"/></Relationships>",
            xml_decl()
        )
        .as_bytes(),
    )?;
    if has_notes {
        // notesMaster 按惯例配独立 theme part(共享 theme1 有 Office 修复风险)。
        put(&mut zip, "ppt/theme/theme2.xml", theme_xml().as_bytes())?;
        put(
            &mut zip,
            "ppt/notesMasters/notesMaster1.xml",
            notes_master_xml().as_bytes(),
        )?;
        put(
            &mut zip,
            "ppt/notesMasters/_rels/notesMaster1.xml.rels",
            format!(
                "{}<Relationships xmlns=\"{NS_REL}\"><Relationship Id=\"rId1\" Type=\"{NS_R}/theme\" Target=\"../theme/theme2.xml\"/></Relationships>",
                xml_decl()
            )
            .as_bytes(),
        )?;
    }

    // 每页 slide + rels + 可选 notesSlide。
    for (idx, sx) in slide_xmls.iter().enumerate() {
        let i = idx + 1;
        put(&mut zip, &format!("ppt/slides/slide{i}.xml"), sx.as_bytes())?;
        let mut srels = String::from(xml_decl());
        srels.push_str(&format!("<Relationships xmlns=\"{NS_REL}\">"));
        srels.push_str(&format!("<Relationship Id=\"rId1\" Type=\"{NS_R}/slideLayout\" Target=\"../slideLayouts/slideLayout1.xml\"/>"));
        if notes[idx].is_some() {
            srels.push_str(&format!("<Relationship Id=\"rId2\" Type=\"{NS_R}/notesSlide\" Target=\"../notesSlides/notesSlide{i}.xml\"/>"));
        }
        if let Some(im) = &images[idx] {
            // 每页一份图字节(不去重):同一张图跨页复用在课件里罕见,换来的是 rels 简单、
            // 删页不会连坐其它页的图。
            put(
                &mut zip,
                &format!("ppt/media/image{i}.{}", im.ext),
                &im.bytes,
            )?;
            srels.push_str(&format!(
                "<Relationship Id=\"{IMG_RID}\" Type=\"{NS_R}/image\" Target=\"../media/image{i}.{}\"/>",
                im.ext
            ));
        }
        // freeform 盒内多图:media 文件名带盒序号,rId 与渲染时(FREE_IMG_RID_BASE+k)一致。
        for (k, opt) in free_imgs[idx].iter().enumerate() {
            if let Some(im) = opt {
                put(
                    &mut zip,
                    &format!("ppt/media/image{i}_{k}.{}", im.ext),
                    &im.bytes,
                )?;
                srels.push_str(&format!(
                    "<Relationship Id=\"rId{}\" Type=\"{NS_R}/image\" Target=\"../media/image{i}_{k}.{}\"/>",
                    FREE_IMG_RID_BASE + k as u32,
                    im.ext
                ));
            }
        }
        srels.push_str("</Relationships>");
        put(
            &mut zip,
            &format!("ppt/slides/_rels/slide{i}.xml.rels"),
            srels.as_bytes(),
        )?;
        if let Some(nt) = &notes[idx] {
            put(
                &mut zip,
                &format!("ppt/notesSlides/notesSlide{i}.xml"),
                notes_slide_xml(nt).as_bytes(),
            )?;
            put(
                &mut zip,
                &format!("ppt/notesSlides/_rels/notesSlide{i}.xml.rels"),
                format!(
                    "{}<Relationships xmlns=\"{NS_REL}\"><Relationship Id=\"rId1\" Type=\"{NS_R}/notesMaster\" Target=\"../notesMasters/notesMaster1.xml\"/><Relationship Id=\"rId2\" Type=\"{NS_R}/slide\" Target=\"../slides/slide{i}.xml\"/></Relationships>",
                    xml_decl()
                )
                .as_bytes(),
            )?;
        }
    }

    let f = zip.finish().map_err(|e| format!("zip 收尾失败: {e}"))?;
    drop(f); // Windows: rename 前先关句柄
    std::fs::rename(&tmp_path, out_path).map_err(|e| {
        format!("写入 {out_path} 失败(若该文件正在 PowerPoint/WPS 中打开,请先关闭再重试): {e}")
    })?;
    tmp_guard.1 = false;
    let notes_pages = notes.iter().filter(|x| x.is_some()).count();
    let image_count = images.iter().filter(|x| x.is_some()).count()
        + free_imgs
            .iter()
            .map(|r| r.iter().filter(|o| o.is_some()).count())
            .sum::<usize>();
    Ok(json!({
        "ok": true,
        "out": out_path,
        "slides": n,
        "theme": theme_name,
        "notes_pages": notes_pages,
        "images": image_count,
        "editable": true,
        "warnings": warnings,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn read_part(path: &std::path::Path, part: &str) -> String {
        let f = std::fs::File::open(path).unwrap();
        let mut z = zip::ZipArchive::new(f).unwrap();
        let mut s = String::new();
        z.by_name(part).unwrap().read_to_string(&mut s).unwrap();
        s
    }

    const SPEC: &str = r#"{
        "version": 1,
        "theme": "ink-gold",
        "slides": [
            {"layout":"title","kicker":"POLARIS","title":"传统PPT可编辑化","subtitle":"spec → 原生 OOXML","notes":"开场白:为什么做这件事"},
            {"layout":"bullets","title":"三条路线","points":["分层导出",{"text":"原生生成","sub":["零浏览器","Docker slim 可用"]},"外部 CLI(否决)"]},
            {"layout":"two-col","title":"对比","left":{"head":"路线A","points":["保真"]},"right":{"head":"路线B","points":["可编辑"]}},
            {"layout":"compare","title":"三平台","items":[{"head":"Win","body":"WebView2"},{"head":"mac","body":"WKWebView"},{"head":"Docker","body":"无浏览器\n靠原生"}]},
            {"layout":"quote","text":"AI 出决策,代码执行","by":"Polaris KB 哲学"},
            {"layout":"closing","subtitle":"polaris.slides.json"}
        ]
    }"#;

    #[test]
    fn spec_builds_valid_editable_package() {
        let dir = std::env::temp_dir().join("polaris_native_pptx_test");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("native.pptx");
        let r = build_pptx_from_spec(SPEC, &out.to_string_lossy()).expect("应成功");
        assert_eq!(r["slides"], 6);
        assert_eq!(r["theme"], "ink-gold");
        assert_eq!(r["notes_pages"], 1);
        assert_eq!(r["warnings"].as_array().unwrap().len(), 0);
        // 自写校验器吃得下(共用图片版的 part 骨架)。
        let v = crate::forge::pptx::validate_pptx(&out.to_string_lossy()).unwrap();
        assert!(v.ok, "校验失败: {:?}", v.errors);
        assert_eq!(v.slides_found, 6);
        // slide1: 真文本(非图片、非隐形),带主题色。
        let s1 = read_part(&out, "ppt/slides/slide1.xml");
        assert!(s1.contains("传统PPT可编辑化"));
        assert!(!s1.contains("<p:pic>"), "title 版式不吃配图,不应有图片框");
        assert!(!s1.contains("<a:alpha val=\"0\"/>"), "不应有隐形层");
        assert!(s1.contains("val=\"D4B06A\""), "应用 ink-gold 强调色");
        assert!(s1.contains("typeface=\"Microsoft YaHei\""), "中文 ea 字体");
        // bullets 页:真 buChar 项目符号 + 两级。
        let s2 = read_part(&out, "ppt/slides/slide2.xml");
        assert!(s2.contains("<a:buChar char=\"•\"/>"));
        assert!(s2.contains("<a:buChar char=\"–\"/>"));
        assert!(s2.contains("lvl=\"1\""));
        // compare 页:圆角卡片。
        let s4 = read_part(&out, "ppt/slides/slide4.xml");
        assert!(s4.contains("prst=\"roundRect\""));
        // 备注页 + 其 rels + presentation 挂 notesMaster。
        let n1 = read_part(&out, "ppt/notesSlides/notesSlide1.xml");
        assert!(n1.contains("开场白"));
        let pres = read_part(&out, "ppt/presentation.xml");
        assert!(pres.contains("<p:notesMasterIdLst>"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 造一张 w×h 的纯色 PNG 当配图素材(不依赖任何外部文件/网络)。
    fn write_png(path: &std::path::Path, w: u32, h: u32) {
        let buf = image::RgbImage::from_pixel(w, h, image::Rgb([120u8, 160, 90]));
        buf.save(path).unwrap();
    }

    #[test]
    fn autofit_grows_sparse_pages_and_shrinks_dense_ones() {
        // 这条测的是本次改动的核心承诺:字号跟着内容量走。
        // em 宽度估算:中文 1em、拉丁 0.55em。
        assert!((em_width("浮力") - 2.0).abs() < 0.01);
        assert!((em_width("abc") - 1.65).abs() < 0.01);
        assert!((em_width("浮力 F") - (2.0 + 0.55 * 2.0)).abs() < 0.01);

        let line = |s: &str| FitLine {
            em: em_width(s),
            rel: 0,
            after: 8.0,
        };
        // bullets 盒:1120-30 宽、470 高、[16,30]。
        let (w, h) = (1120 - BULLET_INDENT, 470);
        let sparse: Vec<FitLine> = vec![line("能说出浮力的定义"), line("会测浮力"), line("能解释沉浮")];
        let dense: Vec<FitLine> = (0..12).map(|_| line("这是一条比较长的要点内容用来占满整页")).collect();

        let a = autofit(&sparse, w, h, 16, 30);
        let b = autofit(&dense, w, h, 16, 30);
        assert_eq!(a, 30, "3 行的页应顶到上界");
        assert!(b < a, "12 行的页必须比 3 行的小: {b} vs {a}");
        assert!(b >= 16, "再挤也不该跌破下界: {b}");

        // 单调性:行越多字号只能变小或持平,绝不反弹。
        let mut prev = 99;
        for n in 1..=16 {
            let ls: Vec<FitLine> = (0..n).map(|_| line("要点内容示例")).collect();
            let s = autofit(&ls, w, h, 16, 30);
            assert!(s <= prev, "行数 {n} 时字号反弹了: {s} > {prev}");
            prev = s;
        }

        // 真放得下:autofit 选出的字号,按同一套公式回算总高必须 ≤ 盒高。
        for lines in [&sparse, &dense] {
            let s = autofit(lines, w, h, 16, 30);
            let total: f64 = lines
                .iter()
                .map(|l| {
                    let sz = (s + l.rel).max(6) as f64;
                    let per_row = (w as f64 / (sz * PT2PX)).max(1.0);
                    (l.em / per_row).ceil().max(1.0) * sz * LINE_H * PT2PX + l.after * PT2PX
                })
                .sum();
            assert!(total <= h as f64, "选出的 {s}pt 撑爆了: {total} > {h}");
        }

        // 窄盒(4 卡 compare 的单卡)必须比宽盒收得更小。
        let narrow = autofit(&sparse, 236 - 48, 382, 11, 22);
        let wide = autofit(&sparse, 546 - 48, 382, 11, 22);
        assert!(narrow <= wide, "窄卡不该比宽卡字大: {narrow} vs {wide}");
    }

    #[test]
    fn image_layouts_embed_real_pictures_with_cover_crop() {
        let dir = std::env::temp_dir().join("polaris_native_pptx_img");
        let _ = std::fs::create_dir_all(&dir);
        // 宽图(3:1)进 16:9 全幅框 → 该左右裁;高图(1:2)进 544x470 框 → 该上下裁。
        let wide = dir.join("wide.png");
        let tall = dir.join("tall.png");
        write_png(&wide, 1200, 400);
        write_png(&tall, 400, 800);
        let out = dir.join("img.pptx");
        let spec = format!(
            r#"{{"theme":"warm-paper","slides":[
                {{"layout":"image-full","image":{:?},"kicker":"情境","title":"春天来啦","subtitle":"去找春天","notes":"先让学生说"}},
                {{"layout":"image-text","image":{:?},"side":"right","title":"观察","head":"看一看","points":["嫩芽","溪水"]}}
            ]}}"#,
            wide.to_string_lossy(),
            tall.to_string_lossy()
        );
        let r = build_pptx_from_spec(&spec, &out.to_string_lossy()).expect("应成功");
        assert_eq!(r["images"], 2);
        assert_eq!(r["warnings"].as_array().unwrap().len(), 0, "不应有告警");
        let v = crate::forge::pptx::validate_pptx(&out.to_string_lossy()).unwrap();
        assert!(v.ok, "校验失败: {:?}", v.errors);

        // 图字节真进包 + Content_Types 声明 png。
        let f = std::fs::File::open(&out).unwrap();
        let z = zip::ZipArchive::new(f).unwrap();
        let names: Vec<String> = z.file_names().map(|s| s.to_string()).collect();
        assert!(names.iter().any(|n| n == "ppt/media/image1.png"));
        assert!(names.iter().any(|n| n == "ppt/media/image2.png"));
        drop(z);
        let ct = read_part(&out, "[Content_Types].xml");
        assert!(ct.contains("Extension=\"png\""));

        // 第1页: 真图片框 + 蒙版 + 白标题, rels 挂 rId3。
        let s1 = read_part(&out, "ppt/slides/slide1.xml");
        assert!(s1.contains("<p:pic>"), "image-full 应有真图片框");
        assert!(s1.contains("r:embed=\"rId3\""));
        assert!(s1.contains("<a:alpha val=\"50000\"/>"), "应有半透明蒙版");
        assert!(s1.contains("春天来啦"));
        let r1 = read_part(&out, "ppt/slides/_rels/slide1.xml.rels");
        assert!(r1.contains("../media/image1.png"));

        // cover 裁切: 3:1 宽图进 16:9 → 只裁左右(l=r>0, t=b=0)。
        let src = regex_first(&s1, "<a:srcRect [^/]*/>").expect("宽图应被裁");
        assert!(src.contains("t=\"0\"") || !src.contains(" t="), "宽图不该上下裁: {src}");
        assert!(!src.contains("l=\"0\""), "宽图应左右裁: {src}");

        // 第2页: 高图进横框 → 只裁上下; side=right 时图在右半(x=656px)。
        let s2 = read_part(&out, "ppt/slides/slide2.xml");
        let src2 = regex_first(&s2, "<a:srcRect [^/]*/>").expect("高图应被裁");
        assert!(!src2.contains("t=\"0\""), "高图应上下裁: {src2}");
        assert!(s2.contains(&format!("<a:off x=\"{}\"", 656 * 9525)), "side=right 图应在右半");

        // 预览即导出:图角必须与前端 slidesSpec.ts 的 .pic 一致 ——
        // 分栏图圆角(.pic{border-radius:8px})、全幅图直角(.ifull .pic{border-radius:0})。
        // 任一边单独改就会预览一个样导出一个样,故在此钉死。
        assert!(
            s1.contains("prst=\"rect\"") && !s1.contains("roundRect"),
            "image-full 必须直角(圆角会在页边露出背景缺口)"
        );
        assert!(s2.contains("roundRect"), "image-text 的图应圆角,与卡片成套");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn freeform_lays_out_arbitrary_boxes_with_images() {
        let dir = std::env::temp_dir().join("polaris_native_pptx_free");
        let _ = std::fs::create_dir_all(&dir);
        let pic = dir.join("p.png");
        write_png(&pic, 800, 600);
        let out = dir.join("free.pptx");
        let spec = format!(
            r##"{{"theme":"ink-gold","slides":[
                {{"layout":"freeform","boxes":[
                    {{"type":"rect","x":0,"y":0,"w":1280,"h":10,"color":"accent"}},
                    {{"type":"scrim","x":0,"y":0,"w":1280,"h":720,"color":"#000","alpha":30}},
                    {{"type":"card","x":80,"y":120,"w":500,"h":400}},
                    {{"type":"text","x":100,"y":140,"w":460,"h":120,"text":"自由标题","size":40,"color":"#FFFFFF","align":"ctr","bold":true}},
                    {{"type":"text","x":100,"y":280,"w":460,"h":200,"lines":["第一行","第二行"],"size":18,"color":"muted"}},
                    {{"type":"image","x":640,"y":120,"w":560,"h":400,"image":{:?},"cover":true,"rounded":true}}
                ]}}
            ]}}"##,
            pic.to_string_lossy()
        );
        let r = build_pptx_from_spec(&spec, &out.to_string_lossy()).expect("应成功");
        assert_eq!(r["slides"], 1);
        assert_eq!(r["images"], 1, "freeform 盒内图应计数");
        assert_eq!(r["warnings"].as_array().unwrap().len(), 0, "不应有告警");
        let v = crate::forge::pptx::validate_pptx(&out.to_string_lossy()).unwrap();
        assert!(v.ok, "校验失败: {:?}", v.errors);

        let s1 = read_part(&out, "ppt/slides/slide1.xml");
        assert!(s1.contains("自由标题"));
        assert!(s1.contains("第一行") && s1.contains("第二行"), "多行 text 应逐行成段");
        assert!(s1.contains("<p:pic>") && s1.contains("r:embed=\"rId10\""), "盒内图从 rId10 起编");
        assert!(s1.contains("prst=\"roundRect\""), "rounded 图应圆角");
        assert!(s1.contains("<a:alpha val=\"30000\"/>"), "scrim alpha=30 → 30000");
        // 盒内图字节真进包,rels 对得上 rId10。
        let r1 = read_part(&out, "ppt/slides/_rels/slide1.xml.rels");
        assert!(r1.contains("Id=\"rId10\"") && r1.contains("../media/image1_0.png"));
        let f = std::fs::File::open(&out).unwrap();
        let z = zip::ZipArchive::new(f).unwrap();
        assert!(z.file_names().any(|n| n == "ppt/media/image1_0.png"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn freeform_math_figure_builds_on_click() {
        let dir = std::env::temp_dir().join("polaris_native_pptx_anim");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("anim.pptx");
        // 一张「函数图逐步显现」:坐标轴(第1击)→ 曲线(第2击)→ 切点(第3击)→ 切线(第3击,同击).
        let spec = r##"{"theme":"minimal-white","slides":[
            {"layout":"freeform","boxes":[
                {"type":"axis","x":200,"y":600,"x2":1100,"y2":600,"color":"ink","width":3,"click":1},
                {"type":"axis","x":250,"y":650,"x2":250,"y2":120,"color":"ink","width":3,"click":1},
                {"type":"curve","points":[[300,560],[500,300],[700,220],[900,320],[1000,480]],"color":"accent","width":4,"click":2},
                {"type":"point","x":700,"y":220,"r":7,"fill":"#D00000","click":3},
                {"type":"line","x":520,"y":300,"x2":880,"y2":170,"color":"#D00000","width":2,"dash":true,"click":3},
                {"type":"text","x":600,"y":80,"w":400,"h":40,"text":"切线斜率 = f′(x₀)","size":20,"color":"muted","align":"ctr","click":3}
            ]}
        ]}"##;
        let r = build_pptx_from_spec(spec, &out.to_string_lossy()).expect("应成功");
        assert_eq!(r["warnings"].as_array().unwrap().len(), 0, "不应有告警");
        let v = crate::forge::pptx::validate_pptx(&out.to_string_lossy()).unwrap();
        assert!(v.ok, "校验失败: {:?}", v.errors);
        let s1 = read_part(&out, "ppt/slides/slide1.xml");
        // 矢量原语真落地。
        assert!(s1.contains("prst=\"line\""), "坐标轴/切线是 line 连接形状");
        assert!(s1.contains("<a:custGeom>"), "曲线是 custGeom 折线");
        assert!(s1.contains("prst=\"ellipse\""), "切点是实心圆");
        assert!(s1.contains("tailEnd type=\"triangle\""), "坐标轴带箭头");
        assert!(s1.contains("prstDash val=\"dash\""), "切线是虚线");
        // 动画时间线:三次单击、fade 入场、绑到形状。
        assert!(s1.contains("<p:timing>") && s1.contains("nodeType=\"mainSeq\""));
        assert!(s1.contains("nodeType=\"clickEffect\""), "每一击有 clickEffect 触发");
        assert!(s1.contains("nodeType=\"withEffect\""), "同击多形状用 withEffect 并现");
        assert_eq!(s1.matches("<p:cond delay=\"indefinite\"/>").count(), 3, "三个单击步骤");
        assert!(s1.contains("filter=\"fade\"") && s1.contains("<p:bldLst>"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 极简子串抓取(不引 regex 依赖):取首个匹配 `<a:srcRect …/>`。
    fn regex_first(hay: &str, _pat: &str) -> Option<String> {
        let i = hay.find("<a:srcRect")?;
        let j = hay[i..].find("/>")? + i + 2;
        Some(hay[i..j].to_string())
    }

    #[test]
    fn broken_image_degrades_with_warning_not_failure() {
        let dir = std::env::temp_dir().join("polaris_native_pptx_imgbad");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("bad.pptx");
        // ① 路径不存在 ② 非图片版式却给了 image
        let spec = r#"{"slides":[
            {"layout":"image-full","image":"/no/such/file.png","title":"仍要出片"},
            {"layout":"bullets","image":"/x.png","title":"要点","points":["a"]}
        ]}"#;
        let r = build_pptx_from_spec(spec, &out.to_string_lossy()).expect("坏图不该让整份失败");
        assert_eq!(r["slides"], 2);
        assert_eq!(r["images"], 0);
        let w = r["warnings"].as_array().unwrap();
        assert!(w.iter().any(|x| x.as_str().unwrap().contains("配图不可用")));
        assert!(w.iter().any(|x| x.as_str().unwrap().contains("不支持配图")));
        // 降级后仍是一份合法可打开的 pptx,标题还在。
        let v = crate::forge::pptx::validate_pptx(&out.to_string_lossy()).unwrap();
        assert!(v.ok, "校验失败: {:?}", v.errors);
        assert!(read_part(&out, "ppt/slides/slide1.xml").contains("仍要出片"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spec_without_notes_omits_notes_parts() {
        let dir = std::env::temp_dir().join("polaris_native_pptx_nonotes");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("plain.pptx");
        let spec = r#"{"slides":[{"layout":"bullets","title":"T","points":["a"]}]}"#;
        let r = build_pptx_from_spec(spec, &out.to_string_lossy()).unwrap();
        assert_eq!(r["notes_pages"], 0);
        assert_eq!(r["theme"], "minimal-white");
        let f = std::fs::File::open(&out).unwrap();
        let z = zip::ZipArchive::new(f).unwrap();
        let names: Vec<&str> = z.file_names().collect();
        assert!(
            !names.iter().any(|n| n.contains("notesMaster")),
            "无备注不应有 notesMaster"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stats_and_timeline_layouts_render_natively() {
        let dir = std::env::temp_dir().join("polaris_native_pptx_rich");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("rich.pptx");
        let spec = r#"{
            "theme": "deep-space",
            "slides": [
                {"layout":"stats","title":"关键指标","items":[
                    {"value":"83%","label":"覆盖率","desc":"含边缘场景"},
                    {"value":"3.2x","label":"提速"}
                ]},
                {"layout":"timeline","title":"路线图","steps":[
                    {"head":"调研","body":"两周"},
                    {"head":"落地","body":"四周\n含验收"},
                    {"head":"推广"}
                ]}
            ]
        }"#;
        let r = build_pptx_from_spec(spec, &out.to_string_lossy()).expect("应成功");
        assert_eq!(r["slides"], 2);
        assert_eq!(r["warnings"].as_array().unwrap().len(), 0);
        let v = crate::forge::pptx::validate_pptx(&out.to_string_lossy()).unwrap();
        assert!(v.ok, "校验失败: {:?}", v.errors);
        // stats 页: 卡片 + 强调色大数字。
        let s1 = read_part(&out, "ppt/slides/slide1.xml");
        assert!(s1.contains("prst=\"roundRect\""));
        assert!(s1.contains("83%"));
        // value 已从写死 44pt 改为 autofit:"83%" 这么短,该顶到上界 60pt。
        // (旧断言写死 sz="4400",正是「字号不随内容变」的化石。)
        assert!(
            s1.contains("sz=\"6000\""),
            "短 value 应 autofit 到上界 60pt"
        );
        // timeline 页: 圆形节点 + 连接线 + 步骤序号。
        let s2 = read_part(&out, "ppt/slides/slide2.xml");
        assert!(s2.contains("prst=\"ellipse\""));
        assert!(s2.contains("<a:t>1</a:t>"));
        assert!(s2.contains("<a:t>3</a:t>"));
        assert!(s2.contains("路线图"));
        // 超限截断要进 warnings。
        let spec_over = r#"{"slides":[{"layout":"timeline","steps":[
            {"head":"a"},{"head":"b"},{"head":"c"},{"head":"d"},{"head":"e"},{"head":"f"}]}]}"#;
        let r2 = build_pptx_from_spec(spec_over, &out.to_string_lossy()).unwrap();
        let w = r2["warnings"].as_array().unwrap();
        assert!(w.iter().any(|x| x.as_str().unwrap().contains("timeline")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_layout_degrades_to_bullets_with_warning() {
        let dir = std::env::temp_dir().join("polaris_native_pptx_unknown");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("u.pptx");
        let spec = r#"{"slides":[{"layout":"galaxy","title":"X","points":["p1"]}]}"#;
        let r = build_pptx_from_spec(spec, &out.to_string_lossy()).unwrap();
        let w = r["warnings"].as_array().unwrap();
        assert_eq!(w.len(), 1);
        assert!(w[0].as_str().unwrap().contains("galaxy"));
        let s1 = read_part(&out, "ppt/slides/slide1.xml");
        assert!(s1.contains("p1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_empty_and_bad_spec() {
        let out = std::env::temp_dir().join("never.pptx");
        assert!(build_pptx_from_spec("not json", &out.to_string_lossy()).is_err());
        assert!(build_pptx_from_spec(r#"{"slides":[]}"#, &out.to_string_lossy()).is_err());
        assert!(build_pptx_from_spec(r#"{}"#, &out.to_string_lossy()).is_err());
    }

    #[test]
    fn spec_text_is_xml_escaped() {
        let dir = std::env::temp_dir().join("polaris_native_pptx_escape");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("e.pptx");
        let spec =
            r#"{"slides":[{"layout":"bullets","title":"<script>&\"x\"","points":["a<b>"]}]}"#;
        build_pptx_from_spec(spec, &out.to_string_lossy()).unwrap();
        let s1 = read_part(&out, "ppt/slides/slide1.xml");
        assert!(s1.contains("&lt;script&gt;&amp;"));
        assert!(!s1.contains("<script>"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
