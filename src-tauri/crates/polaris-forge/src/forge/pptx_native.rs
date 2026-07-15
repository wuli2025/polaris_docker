//! Polaris Forge · spec JSON → **原生可编辑 .pptx**(路线 B,「传统PPT」模式正解)。
//!
//! 模型出决策(polaris.slides.json:版式选择 + 结构化内容),本模块确定性落 OOXML——
//! 全部元素是真文本框/真形状/真填充,用户在 PowerPoint/WPS/Keynote 里 100% 可编辑;
//! **零截图、零浏览器**:Docker slim(无 chromium)也能出 PPT,纯文本模型即可驱动。
//!
//! spec v1(9 版式 × 原生原语):
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
//!     {"layout":"closing", "title":"…", "subtitle":"…"}
//!   ]
//! }
//! ```
//! 每页可带 `"notes":"…"` → 写进演讲者备注页(notesSlide)。
//! 未知 layout 宽容降级为 bullets(尽量出东西,warnings 里报)。
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
        let p = Para {
            text: title,
            size_pt: 26,
            bold: true,
            ..Para::plain(title, 26, pal.ink)
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

/// 单页 spec → spTree 内容 XML。未知版式宽容降级 bullets,warnings 收集。
fn slide_content(sl: &Value, pal: &Palette, warnings: &mut Vec<String>, page: usize) -> String {
    let layout = s_str(sl, "layout");
    let mut id = 10u32;
    let mut s = String::new();
    match layout {
        "title" | "closing" => {
            let kicker = s_str(sl, "kicker");
            if !kicker.is_empty() {
                let p = Para {
                    align: "ctr",
                    bold: true,
                    ..Para::plain(kicker, 14, pal.accent)
                };
                s.push_str(&text_box(id, 160, 218, 960, 32, "t", &para_xml(&p, pal)));
                id += 1;
            }
            let title = if s_str(sl, "title").is_empty() && layout == "closing" {
                "谢谢"
            } else {
                s_str(sl, "title")
            };
            let p = Para {
                align: "ctr",
                bold: true,
                ..Para::plain(title, 40, pal.ink)
            };
            s.push_str(&text_box(id, 80, 268, 1120, 110, "t", &para_xml(&p, pal)));
            id += 1;
            s.push_str(&solid_rect(id, 598, 392, 84, 4, pal.accent));
            id += 1;
            let sub = s_str(sl, "subtitle");
            if !sub.is_empty() {
                let p = Para {
                    align: "ctr",
                    ..Para::plain(sub, 17, pal.muted)
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
                    ..Para::plain(kicker, 14, pal.accent)
                };
                s.push_str(&text_box(id, 116, 296, 1000, 32, "t", &para_xml(&p, pal)));
                id += 1;
            }
            let p = Para {
                bold: true,
                ..Para::plain(s_str(sl, "title"), 34, pal.ink)
            };
            s.push_str(&text_box(id, 116, 336, 1040, 90, "t", &para_xml(&p, pal)));
        }
        "two-col" => {
            let (h, nid) = header(s_str(sl, "title"), pal, id);
            s.push_str(&h);
            id = nid;
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
                                ..Para::plain(head, 17, pal.accent)
                            },
                            pal,
                        ));
                    }
                    warn_bad_type(col.get("points"), &format!("{key}.points"), page, warnings);
                    paras.push_str(&points_paras(col.get("points"), 15, pal));
                    if !paras.is_empty() {
                        s.push_str(&round_card(id, x, 168, 544, 470, pal));
                        id += 1;
                        s.push_str(&text_box(id, x + 28, 196, 544 - 56, 470 - 56, "t", &paras));
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
                            ..Para::plain(head, 17, pal.accent)
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
                                ..Para::plain(line.trim(), 14, pal.ink)
                            },
                            pal,
                        ));
                    }
                }
                paras.push_str(&points_paras(it.get("points"), 14, pal));
                s.push_str(&text_box(id, x + 24, 204, w - 48, 430 - 48, "t", &paras));
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
                    paras.push_str(&para_xml(
                        &Para {
                            align: "ctr",
                            bold: true,
                            space_after_pt: 10,
                            ..Para::plain(value, 44, pal.accent)
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
                            ..Para::plain(label, 16, pal.ink)
                        },
                        pal,
                    ));
                }
                let desc = s_str(it, "desc");
                if !desc.is_empty() {
                    paras.push_str(&para_xml(
                        &Para {
                            align: "ctr",
                            ..Para::plain(desc, 12, pal.muted)
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
                            ..Para::plain(head, 16, pal.ink)
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
                            ..Para::plain(line.trim(), 13, pal.muted)
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
            let p = Para {
                align: "ctr",
                italic: true,
                ..Para::plain(s_str(sl, "text"), 26, pal.ink)
            };
            s.push_str(&text_box(id, 160, 250, 960, 220, "ctr", &para_xml(&p, pal)));
            id += 1;
            let by = s_str(sl, "by");
            if !by.is_empty() {
                let byline = format!("—— {by}");
                let p = Para {
                    align: "ctr",
                    ..Para::plain(&byline, 15, pal.muted)
                };
                s.push_str(&text_box(id, 160, 490, 960, 40, "t", &para_xml(&p, pal)));
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
            let paras = points_paras(sl.get("points"), 17, pal);
            if !paras.is_empty() {
                s.push_str(&text_box(id, 80, 176, 1120, 470, "t", &paras));
            }
        }
    }
    s
}

fn native_slide_xml(content: &str, pal: &Palette) -> String {
    format!(
        "{decl}<p:sld xmlns:a=\"{a}\" xmlns:r=\"{r}\" xmlns:p=\"{p}\"><p:cSld>{bg}<p:spTree>\
<p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>\
<p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"0\" cy=\"0\"/></a:xfrm></p:grpSpPr>\
{content}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>",
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
    let mut slide_xmls: Vec<String> = Vec::with_capacity(n);
    let mut notes: Vec<Option<String>> = Vec::with_capacity(n);
    for (i, sl) in slides.iter().enumerate() {
        let content = slide_content(sl, &pal, &mut warnings, i + 1);
        slide_xmls.push(native_slide_xml(&content, &pal));
        let nt = s_str(sl, "notes").trim().to_string();
        notes.push(if nt.is_empty() { None } else { Some(nt) });
    }
    let has_notes = notes.iter().any(|n| n.is_some());

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
    Ok(json!({
        "ok": true,
        "out": out_path,
        "slides": n,
        "theme": theme_name,
        "notes_pages": notes_pages,
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
        assert!(!s1.contains("<p:pic>"), "原生页不应有图片");
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
        assert!(s1.contains("sz=\"4400\""), "value 应为 44pt 大字");
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
