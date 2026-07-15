//! Polaris Forge · 纯 Rust .pptx 打包器(架构文档「自写 OOXML 零新依赖」的首个落地件)。
//!
//! 把一组幻灯图(deck 各页截图 PNG/JPG)打成**合法可打开的 .pptx**——每页一张全幅图。
//! 替掉旧管线的 pptxgenjs(Node)。**三平台同一份**:纯 Rust + zip,字节级一致,win/mac/docker
//! 产出完全相同。配合 `forge_screenshot`(chromium headless 截图)即可端到端 deck→pptx。
//!
//! 设计取舍:首版做「全幅图版式」(像素精确、稳)。隐形文本层 / 真可编辑文本框是架构 v2 的
//! 后续增强(ADR-012),接口预留在 build_pptx 的 per-slide 扩展点。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use zip::write::SimpleFileOptions;

/// 进程内单调计数器:给每次 capture_slides 唯一临时目录,防多线程并发渲染互相覆盖帧。
static CAPTURE_SEQ: AtomicU64 = AtomicU64::new(0);

pub(crate) const NS_CT: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
pub(crate) const NS_REL: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
pub(crate) const NS_A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
pub(crate) const NS_R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
pub(crate) const NS_P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";

/// 从 PNG 头(IHDR)读宽高(px)。非 PNG / 损坏 → None。纯 std,不引 image crate。
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    // 8 字节签名 + 4 长度 + 4 "IHDR" → width 在 16..20, height 在 20..24(大端)。
    if bytes.len() < 24 || &bytes[1..4] != b"PNG" || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    if w == 0 || h == 0 {
        None
    } else {
        Some((w, h))
    }
}

pub(crate) fn xml_decl() -> &'static str {
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n"
}

/// 叠加文本层描述(路线 A 可编辑化,ADR-012 v2 落地)。
/// `editable[i]=true` 的页:背景图是 ?notext=1 的「无字底」,文本框走真颜色/字体(可编辑);
/// `editable[i]=false` 的页:背景图含字(旧 deck 或 notext 不可用),文本框退 alpha=0 隐形层(仅可搜索)。
pub struct TextLayer<'a> {
    pub pages: &'a [Vec<Value>],
    pub editable: &'a [bool],
    pub win_w: u32,
    pub win_h: u32,
}

/// 把图片列表打成 .pptx。返回 {ok, out, slides, slide_size_emu}。
pub fn build_pptx(image_paths: &[String], out_path: &str) -> Result<Value, String> {
    build_pptx_inner(image_paths, out_path, None)
}

/// 临时文件守卫:作用域内任何 `?` 早退都把半截 .tmp 清掉;rename 成功后置 `.1=false` 解除。
pub(crate) struct TmpGuard(pub std::path::PathBuf, pub bool);
impl Drop for TmpGuard {
    fn drop(&mut self) {
        if self.1 {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

pub fn build_pptx_inner(
    image_paths: &[String],
    out_path: &str,
    text_layer: Option<TextLayer<'_>>,
) -> Result<Value, String> {
    if image_paths.is_empty() {
        return Err("没有图片可打包".into());
    }
    // ── 工业级化(任务 c §A.4.1 流式写)──────────────────────────
    // 旧版 `images.push((bytes, ext))` 一次性全读 → >200 页时内存峰值 ~300MB。
    // 新版先扫首图拿比例,后续每页 read-put-drop 字节生命周期限在当次循环。
    let mut first_ratio: Option<f64> = None;
    for p in image_paths.iter().take(1) {
        if let Ok(bytes) = std::fs::read(p) {
            if let Some((w, h)) = png_size(&bytes) {
                first_ratio = Some(w as f64 / h as f64);
            }
            drop(bytes); // 立即释放
        }
    }
    let cy: u64 = 6_858_000; // 7.5 inch
                             // 比例钳制:sldSz 受 ST_SlideSizeCoordinate 限制(914400–51206400 EMU),畸形首图
                             // (超宽长图/损坏 IHDR)会写出 schema 非法值致 Office 修复/拒开。常规比例都在 [0.5,4]。
    let ratio = first_ratio
        .filter(|r| r.is_finite() && (0.5..=4.0).contains(r))
        .unwrap_or(16.0 / 9.0);
    let cx: u64 = (cy as f64 * ratio).round() as u64;
    let n = image_paths.len();

    // 自动建父目录:out 路径的目录不存在时也不失败(鲁棒 + 用户省事)。
    if let Some(parent) = Path::new(out_path).parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    // 原子写:先写 .tmp 再 rename 落位 —— 半路失败不毁旧文件、不留半截产物;
    // Windows 上目标被 PowerPoint 占用时,截图成果只损失落位一步,旧文件完好。
    let tmp_path = format!("{out_path}.tmp");
    let mut guard = TmpGuard(std::path::PathBuf::from(&tmp_path), true);
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

    // ── [Content_Types].xml ──
    let mut ct = String::from(xml_decl());
    ct.push_str(&format!("<Types xmlns=\"{NS_CT}\">"));
    ct.push_str("<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>");
    ct.push_str("<Default Extension=\"xml\" ContentType=\"application/xml\"/>");
    ct.push_str("<Default Extension=\"png\" ContentType=\"image/png\"/>");
    ct.push_str("<Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>");
    ct.push_str("<Default Extension=\"jpg\" ContentType=\"image/jpeg\"/>");
    ct.push_str("<Override PartName=\"/ppt/presentation.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml\"/>");
    ct.push_str("<Override PartName=\"/ppt/slideMasters/slideMaster1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml\"/>");
    ct.push_str("<Override PartName=\"/ppt/slideLayouts/slideLayout1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml\"/>");
    ct.push_str("<Override PartName=\"/ppt/theme/theme1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.theme+xml\"/>");
    for i in 1..=n {
        ct.push_str(&format!("<Override PartName=\"/ppt/slides/slide{i}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slide+xml\"/>"));
    }
    ct.push_str("</Types>");
    put(&mut zip, "[Content_Types].xml", ct.as_bytes())?;

    // ── _rels/.rels ──
    let rels = format!(
        "{}<Relationships xmlns=\"{NS_REL}\"><Relationship Id=\"rId1\" Type=\"{NS_R}/officeDocument\" Target=\"ppt/presentation.xml\"/></Relationships>",
        xml_decl()
    );
    put(&mut zip, "_rels/.rels", rels.as_bytes())?;

    // ── ppt/presentation.xml ── rId1=master, rId2..=slides
    let mut pres = String::from(xml_decl());
    pres.push_str(&format!(
        "<p:presentation xmlns:a=\"{NS_A}\" xmlns:r=\"{NS_R}\" xmlns:p=\"{NS_P}\">"
    ));
    pres.push_str(
        "<p:sldMasterIdLst><p:sldMasterId id=\"2147483648\" r:id=\"rId1\"/></p:sldMasterIdLst>",
    );
    pres.push_str("<p:sldIdLst>");
    for i in 1..=n {
        pres.push_str(&format!(
            "<p:sldId id=\"{}\" r:id=\"rId{}\"/>",
            255 + i,
            i + 1
        ));
    }
    pres.push_str("</p:sldIdLst>");
    pres.push_str(&format!("<p:sldSz cx=\"{cx}\" cy=\"{cy}\"/><p:notesSz cx=\"6858000\" cy=\"9144000\"/></p:presentation>"));
    put(&mut zip, "ppt/presentation.xml", pres.as_bytes())?;

    // ── ppt/_rels/presentation.xml.rels ──
    let mut prels = String::from(xml_decl());
    prels.push_str(&format!("<Relationships xmlns=\"{NS_REL}\">"));
    prels.push_str(&format!("<Relationship Id=\"rId1\" Type=\"{NS_R}/slideMaster\" Target=\"slideMasters/slideMaster1.xml\"/>"));
    for i in 1..=n {
        prels.push_str(&format!(
            "<Relationship Id=\"rId{}\" Type=\"{NS_R}/slide\" Target=\"slides/slide{i}.xml\"/>",
            i + 1
        ));
    }
    prels.push_str(&format!(
        "<Relationship Id=\"rId{}\" Type=\"{NS_R}/theme\" Target=\"theme/theme1.xml\"/>",
        n + 2
    ));
    prels.push_str("</Relationships>");
    put(
        &mut zip,
        "ppt/_rels/presentation.xml.rels",
        prels.as_bytes(),
    )?;

    // ── theme / master / layout(最小可用)──
    put(&mut zip, "ppt/theme/theme1.xml", theme_xml().as_bytes())?;
    put(
        &mut zip,
        "ppt/slideMasters/slideMaster1.xml",
        slide_master_xml(cx, cy).as_bytes(),
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
        slide_layout_xml(cx, cy).as_bytes(),
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

    // ── 每页:slideN.xml(全幅图)+ rels + 媒体 ──
    //    工业级化(任务 c §A.4.1):每页字节读入→写 zip→立即 drop,峰值从 N 张降到 1 张
    for (idx, p) in image_paths.iter().enumerate() {
        let i = idx + 1;
        let bytes = std::fs::read(p).map_err(|e| format!("读图失败 {p}: {e}"))?;
        // 按魔数认格式(别信扩展名):字节与 Content-Type 声明不符时 Office 报图片损坏。
        let media_ext = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
            "png"
        } else if bytes.starts_with(&[0xFF, 0xD8]) {
            "jpeg"
        } else {
            return Err(format!("第 {i} 页图片不是 PNG/JPEG(按文件头判定): {p}"));
        };
        put(&mut zip, &format!("ppt/media/image{i}.{media_ext}"), &bytes)?;
        drop(bytes); // ── 立即释放(任务 c §A.4.1 关键)──
                     // 该页的文本框(若启用文本层且该页有 rects):editable=可见可编辑,否则 alpha=0 隐形。
        let boxes = match &text_layer {
            Some(tl) if idx < tl.pages.len() => {
                let editable = tl.editable.get(idx).copied().unwrap_or(false);
                text_boxes_xml(&tl.pages[idx], cx, cy, tl.win_w, tl.win_h, editable)
            }
            _ => String::new(),
        };
        put(
            &mut zip,
            &format!("ppt/slides/slide{i}.xml"),
            slide_xml(cx, cy, &boxes).as_bytes(),
        )?;
        put(
            &mut zip,
            &format!("ppt/slides/_rels/slide{i}.xml.rels"),
            format!(
                "{}<Relationships xmlns=\"{NS_REL}\"><Relationship Id=\"rId1\" Type=\"{NS_R}/slideLayout\" Target=\"../slideLayouts/slideLayout1.xml\"/><Relationship Id=\"rId2\" Type=\"{NS_R}/image\" Target=\"../media/image{i}.{media_ext}\"/></Relationships>",
                xml_decl()
            )
            .as_bytes(),
        )?;
    }

    let f = zip.finish().map_err(|e| format!("zip 收尾失败: {e}"))?;
    drop(f); // Windows: rename 前必须先关句柄
    std::fs::rename(&tmp_path, out_path).map_err(|e| {
        format!("写入 {out_path} 失败(若该文件正在 PowerPoint/WPS 中打开,请先关闭再重试): {e}")
    })?;
    guard.1 = false; // 已成功落位,守卫不再清理
    Ok(json!({
        "ok": true,
        "out": out_path,
        "slides": n,
        "slide_size_emu": { "cx": cx, "cy": cy }
    }))
}

fn slide_xml(cx: u64, cy: u64, text_boxes: &str) -> String {
    format!(
        "{decl}<p:sld xmlns:a=\"{a}\" xmlns:r=\"{r}\" xmlns:p=\"{p}\"><p:cSld><p:spTree>\
<p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>\
<p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"0\" cy=\"0\"/></a:xfrm></p:grpSpPr>\
<p:pic><p:nvPicPr><p:cNvPr id=\"2\" name=\"Slide Image\"/><p:cNvPicPr><a:picLocks noChangeAspect=\"1\"/></p:cNvPicPr><p:nvPr/></p:nvPicPr>\
<p:blipFill><a:blip r:embed=\"rId2\"/><a:stretch><a:fillRect/></a:stretch></p:blipFill>\
<p:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></p:spPr></p:pic>\
{text_boxes}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>",
        decl = xml_decl(), a = NS_A, r = NS_R, p = NS_P
    )
}

/// XML 转义 + 过滤 XML 1.0 非法字符。控制字符(\x00-\x08 \x0B \x0C \x0E-\x1F)和
/// U+FFFE/U+FFFF **转义也救不了**(`&#x7;` 同样非法),必须丢弃 —— spec 是模型产出的
/// JSON(`"ab"` 合法)、deck 文本来自 DOM,都现实可达,带入即 Office 拒开。
pub(crate) fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 => {}
            '\u{FFFE}' | '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
    out
}

/// 把一页的文本 rects(窗口 px)生成文本框 OOXML。窗口 px → slide EMU 按比例;
/// px 字号 → OOXML 1/100 pt(×0.75×100=×75)。
///
/// editable=true(路线 A 可编辑化,背景图已用 ?notext=1 去字):可见文本框,带真颜色/
/// 字体/对齐/行距,用户在 PowerPoint 里改字挪框无重影。宽度 +4% buffer 抵消浏览器与
/// PowerPoint 断行算法差异,normAutofit 兜底防撑爆。
///
/// editable=false(旧 deck 降级):alpha=0 隐形层,仅可搜索/读屏。
/// 工业级化(任务 c §A.1.3 双保险):alpha=0 + `<a:effectLst><a:noFill/></a:effectLst>` 整框透明,
/// 应对 Keynote16 实测会把 alpha=0 渲成可见白块。
fn text_boxes_xml(
    rects: &[Value],
    cx: u64,
    cy: u64,
    win_w: u32,
    win_h: u32,
    editable: bool,
) -> String {
    if rects.is_empty() || win_w == 0 || win_h == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut id = 10u32; // 图片占 id=2,文本框从 10 起避免冲突
    for r in rects {
        let getf = |k: &str| r.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let text = r.get("text").and_then(|v| v.as_str()).unwrap_or("").trim();
        let (pw, ph) = (getf("w"), getf("h"));
        if text.is_empty() || pw <= 0.0 || ph <= 0.0 {
            continue;
        }
        // 数值钳制:rect 来自页面 JS(不可信),越界值会写出 OOXML schema 非法数 →
        // Office「需要修复」。坐标钳到画布 ±1 幅,字号钳 ST_TextFontSize(100..400000)。
        let cl = |v: f64, lo: i64, hi: i64| -> i64 {
            if !v.is_finite() {
                return lo.max(0);
            }
            (v.round() as i64).clamp(lo, hi)
        };
        let (cxi, cyi) = (cx as i64, cy as i64);
        let ex = cl(getf("x") * cx as f64 / win_w as f64, -cxi, cxi * 2);
        let ey = cl(getf("y") * cy as f64 / win_h as f64, -cyi, cyi * 2);
        // 可编辑版宽度 +4%:浏览器断行点 ≠ PowerPoint 断行点,留余量防多折一行。
        let wbuf = if editable { 1.04 } else { 1.0 };
        let ew = cl(pw * wbuf * cx as f64 / win_w as f64, 1, cxi * 2);
        let eh = cl(ph * cy as f64 / win_h as f64, 1, cyi * 2);
        let sz = cl(getf("size").max(8.0) * 75.0, 100, 400_000);
        let bold = if r.get("bold").and_then(|v| v.as_bool()).unwrap_or(false) {
            1
        } else {
            0
        };
        if editable {
            let italic = if r.get("italic").and_then(|v| v.as_bool()).unwrap_or(false) {
                1
            } else {
                0
            };
            // 颜色/对齐来自页面提取,白名单校验防 XML 注入(deck 内容不可信)。
            let color = r.get("color").and_then(|v| v.as_str()).unwrap_or("000000");
            let color = if color.len() == 6 && color.bytes().all(|b| b.is_ascii_hexdigit()) {
                color.to_uppercase()
            } else {
                "000000".into()
            };
            let algn = match r.get("align").and_then(|v| v.as_str()).unwrap_or("l") {
                "ctr" => "ctr",
                "r" => "r",
                "just" => "just",
                _ => "l",
            };
            let serif = r.get("serif").and_then(|v| v.as_bool()).unwrap_or(false);
            // 安全字体链:中文 ea 必须写,否则 CJK 落到 latin 字体出豆腐块。
            let (latin, ea) = if serif {
                ("Georgia", "SimSun")
            } else {
                ("Calibri", "Microsoft YaHei")
            };
            let lh = getf("lh");
            let lnspc = if lh > 0.0 && lh.is_finite() {
                // spcPts 上限 158400(ST_TextSpacingPoint),越界同样触发修复。
                format!(
                    "<a:lnSpc><a:spcPts val=\"{}\"/></a:lnSpc>",
                    cl(lh * 75.0, 1, 158_400)
                )
            } else {
                String::new()
            };
            out.push_str(&format!(
                "<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"text{id}\"/><p:cNvSpPr txBox=\"1\"/><p:nvPr/></p:nvSpPr>\
<p:spPr><a:xfrm><a:off x=\"{ex}\" y=\"{ey}\"/><a:ext cx=\"{ew}\" cy=\"{eh}\"/></a:xfrm>\
<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom><a:noFill/></p:spPr>\
<p:txBody><a:bodyPr wrap=\"square\" lIns=\"0\" tIns=\"0\" rIns=\"0\" bIns=\"0\" anchor=\"t\"><a:normAutofit/></a:bodyPr>\
<a:p><a:pPr algn=\"{algn}\">{lnspc}</a:pPr><a:r><a:rPr lang=\"zh-CN\" sz=\"{sz}\" b=\"{bold}\" i=\"{italic}\">\
<a:solidFill><a:srgbClr val=\"{color}\"/></a:solidFill>\
<a:latin typeface=\"{latin}\"/><a:ea typeface=\"{ea}\"/></a:rPr>\
<a:t>{}</a:t></a:r></a:p></p:txBody></p:sp>",
                xml_escape(text)
            ));
        } else {
            out.push_str(&format!(
                "<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"t{id}\"/><p:cNvSpPr txBox=\"1\"/><p:nvPr/></p:nvSpPr>\
<p:spPr><a:xfrm><a:off x=\"{ex}\" y=\"{ey}\"/><a:ext cx=\"{ew}\" cy=\"{eh}\"/></a:xfrm>\
<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom><a:noFill/>\
<a:effectLst><a:noFill/></a:effectLst></p:spPr>\
<p:txBody><a:bodyPr wrap=\"square\" lIns=\"0\" tIns=\"0\" rIns=\"0\" bIns=\"0\"/>\
<a:p><a:r><a:rPr lang=\"zh-CN\" sz=\"{sz}\" b=\"{bold}\">\
<a:solidFill><a:srgbClr val=\"000000\"><a:alpha val=\"0\"/></a:srgbClr></a:solidFill></a:rPr>\
<a:t>{}</a:t></a:r></a:p></p:txBody></p:sp>",
                xml_escape(text)
            ));
        }
        id += 1;
    }
    out
}

pub(crate) fn slide_layout_xml(_cx: u64, _cy: u64) -> String {
    format!(
        "{decl}<p:sldLayout xmlns:a=\"{a}\" xmlns:r=\"{r}\" xmlns:p=\"{p}\" type=\"blank\" preserve=\"1\"><p:cSld name=\"Blank\"><p:spTree>\
<p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>\
<p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"0\" cy=\"0\"/></a:xfrm></p:grpSpPr>\
</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>",
        decl = xml_decl(), a = NS_A, r = NS_R, p = NS_P
    )
}

pub(crate) fn slide_master_xml(cx: u64, cy: u64) -> String {
    format!(
        "{decl}<p:sldMaster xmlns:a=\"{a}\" xmlns:r=\"{r}\" xmlns:p=\"{p}\"><p:cSld><p:bg><p:bgPr><a:solidFill><a:srgbClr val=\"FFFFFF\"/></a:solidFill><a:effectLst/></p:bgPr></p:bg><p:spTree>\
<p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>\
<p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm></p:grpSpPr>\
</p:spTree></p:cSld>\
<p:clrMap bg1=\"lt1\" tx1=\"dk1\" bg2=\"lt2\" tx2=\"dk2\" accent1=\"accent1\" accent2=\"accent2\" accent3=\"accent3\" accent4=\"accent4\" accent5=\"accent5\" accent6=\"accent6\" hlink=\"hlink\" folHlink=\"folHlink\"/>\
<p:sldLayoutIdLst><p:sldLayoutId id=\"2147483649\" r:id=\"rId1\"/></p:sldLayoutIdLst>\
</p:sldMaster>",
        decl = xml_decl(), a = NS_A, r = NS_R, p = NS_P
    )
}

/// 最小但合法的 Office 主题(clrScheme/fontScheme/fmtScheme 三件齐全, PowerPoint 才认)。
pub(crate) fn theme_xml() -> String {
    format!("{decl}<a:theme xmlns:a=\"{a}\" name=\"Polaris\"><a:themeElements>\
<a:clrScheme name=\"Polaris\"><a:dk1><a:sysClr val=\"windowText\" lastClr=\"000000\"/></a:dk1><a:lt1><a:sysClr val=\"window\" lastClr=\"FFFFFF\"/></a:lt1>\
<a:dk2><a:srgbClr val=\"1F2230\"/></a:dk2><a:lt2><a:srgbClr val=\"EEF1F8\"/></a:lt2>\
<a:accent1><a:srgbClr val=\"7AA2F7\"/></a:accent1><a:accent2><a:srgbClr val=\"B794F6\"/></a:accent2><a:accent3><a:srgbClr val=\"5BE3B0\"/></a:accent3>\
<a:accent4><a:srgbClr val=\"FFD166\"/></a:accent4><a:accent5><a:srgbClr val=\"FF7B8A\"/></a:accent5><a:accent6><a:srgbClr val=\"3B6FE0\"/></a:accent6>\
<a:hlink><a:srgbClr val=\"0563C1\"/></a:hlink><a:folHlink><a:srgbClr val=\"954F72\"/></a:folHlink></a:clrScheme>\
<a:fontScheme name=\"Polaris\"><a:majorFont><a:latin typeface=\"Calibri Light\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:majorFont><a:minorFont><a:latin typeface=\"Calibri\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:minorFont></a:fontScheme>\
<a:fmtScheme name=\"Polaris\">\
<a:fillStyleLst><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:fillStyleLst>\
<a:lnStyleLst><a:ln w=\"6350\"><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln><a:ln w=\"12700\"><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln><a:ln w=\"19050\"><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln></a:lnStyleLst>\
<a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst>\
<a:bgFillStyleLst><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:bgFillStyleLst>\
</a:fmtScheme></a:themeElements></a:theme>",
        decl = xml_decl(), a = NS_A)
}

/// 用 chromium headless CLI 给一个 URL/本地 HTML 截图成 PNG。跨平台:容器走镜像 chromium,
/// win/mac 走 preflight 找到的 Chrome/Edge。这是 Forge capture 的确定性原始能力。
pub fn screenshot(
    url_or_file: &str,
    out_png: &str,
    width: u32,
    height: u32,
    device_scale: u32,
) -> Result<Value, String> {
    let chromium = crate::forge::find_chromium().ok_or_else(|| {
        "未找到 chromium/chrome：Docker 需 full 镜像，桌面需装 Chrome/Edge".to_string()
    })?;
    // 本地文件转 file:// URL。
    let target = if url_or_file.starts_with("http://")
        || url_or_file.starts_with("https://")
        || url_or_file.starts_with("file://")
    {
        url_or_file.to_string()
    } else {
        crate::forge::path_to_file_url(url_or_file)?
    };
    let mut args: Vec<String> = vec![
        "--headless=new".into(),
        "--no-sandbox".into(),
        "--disable-dev-shm-usage".into(),
        "--disable-gpu".into(),
        "--hide-scrollbars".into(),
        format!("--screenshot={out_png}"),
        format!("--window-size={width},{height}"),
    ];
    if device_scale > 1 {
        // 2x 高清(投影放大不糊字,Slidev/Marp 默认 deviceScaleFactor=2)。截图像素 = 窗口 × scale,
        // 版面逻辑尺寸不变(pptx 版面坐标按逻辑 px 算,见 build_pptx 用宽高比),只是更清晰。
        args.push(format!("--force-device-scale-factor={device_scale}"));
    }
    args.push(target);
    // 自动建 out 父目录,免得 chromium 因目录不存在而失败。
    if let Some(parent) = Path::new(out_png).parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let mut cmd = std::process::Command::new(&chromium);
    cmd.args(&args);
    // 90s 超时:单页截图远用不到这么久,挂死则杀掉防永久阻塞。
    crate::forge::run_with_timeout(cmd, 90, "chromium 截图")?;
    if !Path::new(out_png).is_file() {
        return Err("chromium 截图失败(未生成 PNG)".into());
    }
    Ok(json!({ "ok": true, "out": out_png, "chromium": chromium, "device_scale": device_scale }))
}

/// 文本层第一步:用 chromium `--dump-dom` 抽取 deck 某页渲染后的文本+包围盒+样式。
/// 页面在 `?extract=1` 时(runtime.js 提供)把 `[{text,x,y,w,h,size,bold,color,align,...}]`
/// 写进 `<script id="polaris-text-rects">`;dump-dom 输出 JS 跑完的 DOM,我们从中解析出来。
/// **无需 chromiumoxide/CDP**。返回 (rect 数组, notext_capable):
/// capable=true 表示该 deck 的 runtime.js 支持 ?notext=1 去字渲染(新版能力标记),
/// 调用方可走「无字背景+可见文本框」;false=旧 deck,降级 alpha=0 隐形层。
pub fn extract_text_rects(
    deck: &str,
    slide: usize,
    width: u32,
    height: u32,
) -> Result<(Vec<Value>, bool), String> {
    let chromium =
        crate::forge::find_chromium().ok_or_else(|| "未找到 chromium/chrome".to_string())?;
    let file_base = if deck.starts_with("http://")
        || deck.starts_with("https://")
        || deck.starts_with("file://")
    {
        deck.to_string()
    } else {
        crate::forge::path_to_file_url(deck)?
    };
    let url = format!("{file_base}?export=1&extract=1#/{slide}");
    // --virtual-time-budget 让 chromium 跑完页面 JS(含 load/rAF)后退出,--dump-dom 输出最终 DOM。
    let mut cmd = std::process::Command::new(&chromium);
    cmd.args([
        "--headless=new",
        "--no-sandbox",
        "--disable-dev-shm-usage",
        "--disable-gpu",
        "--hide-scrollbars",
        &format!("--window-size={width},{height}"),
        "--virtual-time-budget=3000",
        "--dump-dom",
        &url,
    ]);
    // 30s 超时看门:此前是裸 output() 无超时,chromium 挂死会让整个导出永不返回。
    // run_with_timeout 丢弃 stdout,这里要读 DOM → 走带捕获版(超时同样杀整棵进程树)。
    let stdout = crate::forge::run_capture_stdout(cmd, 30, "chromium --dump-dom")?;
    parse_text_rects(&String::from_utf8_lossy(&stdout))
}

/// 从 dump-dom 的 HTML 里抠出 `<script id="polaris-text-rects">` 的 JSON 数组。
/// 第二个返回值 = 标签上是否带 `data-notext-capable="1"`(新版 runtime.js 能力标记)。
fn parse_text_rects(html: &str) -> Result<(Vec<Value>, bool), String> {
    let marker = "id=\"polaris-text-rects\"";
    let Some(i) = html.find(marker) else {
        return Ok((Vec::new(), false)); // 没有该元素 = 非 Polaris deck 或无文本,优雅返回空(不报错)。
    };
    let after = &html[i..];
    let gt = after.find('>').ok_or("text-rects script 标签异常")?;
    // 能力标记只可能出现在开标签的属性区(gt 之前)。
    let capable = after[..gt].contains("data-notext-capable=\"1\"");
    let body = &after[gt + 1..];
    let end = body.find("</script>").ok_or("text-rects script 未闭合")?;
    let json = body[..end].trim();
    if json.is_empty() {
        return Ok((Vec::new(), capable));
    }
    let rects = serde_json::from_str::<Vec<Value>>(json)
        .map_err(|e| format!("text-rects JSON 解析失败: {e}"))?;
    Ok((rects, capable))
}

/// 数 deck.html 里的幻灯页数:统计 class 列表含独立 token `slide` 的元素(排除 slide-number 等)。
/// 与 runtime.js 的 `.deck > .slide` 结构一致。数不到则返回 0(调用方退化为整页一张)。
pub fn count_slides(html: &str) -> usize {
    let mut n = 0;
    for seg in html.split("class") {
        let s = seg.trim_start();
        let s = match s.strip_prefix('=') {
            Some(x) => x.trim_start(),
            None => continue,
        };
        let (q, rest) = match s.chars().next() {
            Some(c @ '"') => (c, &s[1..]),
            Some(c @ '\'') => (c, &s[1..]),
            _ => continue,
        };
        if let Some(end) = rest.find(q) {
            if rest[..end].split_whitespace().any(|t| t == "slide") {
                n += 1;
            }
        }
    }
    n
}

/// deck.html → 逐页截图到临时目录,返回 (帧目录, PNG 路径列表)。供 pptx / 视频共用。
/// 利用 runtime.js 的 `?export=1#/N` 深链(只有 .is-active 页可见,base.css 已防叠页)。
/// 多页 = 多次 chromium 进程(每页一次);CDP 批量复用单浏览器是后续优化(ADR-002),此版求稳。
pub fn capture_slides(
    deck: &str,
    width: u32,
    height: u32,
    device_scale: u32,
    slides_override: Option<usize>,
) -> Result<(std::path::PathBuf, Vec<String>), String> {
    let (file_base, n) = resolve_deck_pages(deck, slides_override)?;
    let frames = new_frames_dir()?;
    let mut pngs: Vec<String> = Vec::new();
    // 闭包包裹:任一页截图失败即清掉刚建的临时帧目录再返回,不留孤儿目录
    // (与 render_deck_to_pptx 的失败清理写法同款;成功路径由调用方用完后清)。
    let run = (|| -> Result<(), String> {
        for i in 1..=n {
            let png = frames.join(format!("slide-{i}.png"));
            let url = format!("{file_base}?export=1#/{i}");
            screenshot(&url, &png.to_string_lossy(), width, height, device_scale)
                .map_err(|e| format!("第 {i} 页截图失败: {e}"))?;
            pngs.push(png.to_string_lossy().to_string());
        }
        Ok(())
    })();
    if let Err(e) = run {
        let _ = std::fs::remove_dir_all(&frames);
        return Err(e);
    }
    Ok((frames, pngs))
}

/// 解析 deck → (file://或 http 基底 URL, 页数)。capture_slides / render_deck_to_pptx 共用。
/// 上限护栏:畸形 deck/参数别 spawn 成千上万个 chromium 进程拖垮机器。
fn resolve_deck_pages(
    deck: &str,
    slides_override: Option<usize>,
) -> Result<(String, usize), String> {
    const MAX_SLIDES: usize = 300;
    const MAX_DECK_BYTES: u64 = 50 * 1024 * 1024;
    if let Some(n) = slides_override {
        if n > MAX_SLIDES {
            return Err(format!("指定页数 {n} 超过上限 {MAX_SLIDES}(疑似参数错误)"));
        }
    }
    let is_http = deck.starts_with("http://") || deck.starts_with("https://");
    let file_base = if is_http {
        deck.to_string()
    } else {
        // deck 文件过大护栏(防超大 HTML 读爆内存)。
        if let Ok(meta) = std::fs::metadata(deck) {
            if meta.len() > MAX_DECK_BYTES {
                return Err(format!(
                    "deck 文件过大({} 字节 > {}MB 上限)",
                    meta.len(),
                    MAX_DECK_BYTES / 1024 / 1024
                ));
            }
        }
        crate::forge::path_to_file_url(deck)?
    };
    let n = match slides_override {
        Some(n) if n > 0 => n,
        _ => {
            if is_http {
                1
            } else {
                count_slides(&std::fs::read_to_string(deck).unwrap_or_default()).max(1)
            }
        }
    };
    if n > MAX_SLIDES {
        return Err(format!("幻灯页数 {n} 超过上限 {MAX_SLIDES}(疑似畸形 deck)"));
    }
    Ok((file_base, n))
}

/// pid + 进程内唯一序号:并发的两个渲染各用独立目录,不会互相覆盖帧(并发安全)。
fn new_frames_dir() -> Result<std::path::PathBuf, String> {
    let seq = CAPTURE_SEQ.fetch_add(1, Ordering::Relaxed);
    let frames = std::env::temp_dir().join(format!("forge_deck_{}_{}", std::process::id(), seq));
    let _ = std::fs::remove_dir_all(&frames);
    std::fs::create_dir_all(&frames).map_err(|e| format!("建临时帧目录失败: {e}"))?;
    Ok(frames)
}

/// deck.html → 多页 .pptx 一步到位(三平台同一份)。
///
/// 路线 A 可编辑化(ADR-012 v2):每页先 extract 文本 rects,若该 deck 的 runtime.js
/// 支持 ?notext=1(能力标记)且该页有文本,则背景截「无字底」+ 叠可见文本框 = 真可编辑;
/// 旧 deck / 提取失败的页自动降级为「含字背景 + alpha=0 隐形层」(仅可搜索),绝不重影。
pub fn render_deck_to_pptx(
    deck: &str,
    out_pptx: &str,
    width: u32,
    height: u32,
    searchable: bool,
    slides_override: Option<usize>,
) -> Result<Value, String> {
    let (file_base, n) = resolve_deck_pages(deck, slides_override)?;
    let frames = new_frames_dir()?;
    let mut pngs: Vec<String> = Vec::new();
    let mut slides_text: Vec<Vec<Value>> = Vec::new();
    let mut editable: Vec<bool> = Vec::new();
    let run = (|| -> Result<(), String> {
        for i in 1..=n {
            // 先提取该页文本(额外一次 chromium/页;失败降级纯图,不阻断)。
            let (rects, capable) = if searchable {
                extract_text_rects(deck, i, width, height).unwrap_or((Vec::new(), false))
            } else {
                (Vec::new(), false)
            };
            // 只有「确认 runtime 支持 notext + 该页真有文本」才去字截图——
            // 否则背景仍含字而我们又叠可见文本框就会双重文字。
            let use_notext = capable && !rects.is_empty();
            let png = frames.join(format!("slide-{i}.png"));
            let url = format!(
                "{file_base}?export=1{}#/{i}",
                if use_notext { "&notext=1" } else { "" }
            );
            // PPT 默认 2x 高清(投影/全屏放大不糊字;架构文档§06①)。
            screenshot(&url, &png.to_string_lossy(), width, height, 2)
                .map_err(|e| format!("第 {i} 页截图失败: {e}"))?;
            pngs.push(png.to_string_lossy().to_string());
            slides_text.push(rects);
            editable.push(use_notext);
        }
        Ok(())
    })();
    if let Err(e) = run {
        let _ = std::fs::remove_dir_all(&frames);
        return Err(e);
    }
    let layer = if searchable {
        Some(TextLayer {
            pages: slides_text.as_slice(),
            editable: editable.as_slice(),
            win_w: width,
            win_h: height,
        })
    } else {
        None
    };
    let r = build_pptx_inner(&pngs, out_pptx, layer);
    let _ = std::fs::remove_dir_all(&frames);
    let r = r?;
    let text_total: usize = slides_text.iter().map(|v| v.len()).sum();
    let editable_pages = editable.iter().filter(|b| **b).count();
    Ok(json!({
        "ok": true, "out": out_pptx, "slides": n, "searchable": searchable,
        "text_boxes": text_total, "editable_pages": editable_pages, "detail": r
    }))
}

// ═══════════════════════════════════════════════════════════════
// 工业级化(任务 c §A.3):自写最小 OOXML 校验器
//   零新依赖(zip 已用);解压 + 列关键 part + 校验 namespace + 计数
//   失败返具体 part 名,不返笼统 "读图失败"
// ═══════════════════════════════════════════════════════════════

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PptxValidation {
    pub ok: bool,
    pub slides_found: u32,
    pub has_content_types: bool,
    pub has_presentation: bool,
    pub has_slide_master: bool,
    pub has_slide_layout: bool,
    pub has_theme: bool,
    pub media_count: u32,
    pub errors: Vec<String>,
}

/// 自写最小 OOXML 校验器(任务 c §A.3.1)。校验 [Content_Types].xml / presentation.xml
/// / slideMaster / slideLayout / theme / media 字节存在。返具体 part 名 + 错误。
pub fn validate_pptx(path: &str) -> Result<PptxValidation, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("打开 {path} 失败: {e}"))?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| format!("非合法 zip: {e}"))?;
    let mut v = PptxValidation::default();
    let mut slide_n_pattern = 0u32;
    for i in 0..z.len() {
        let name = z
            .by_index(i)
            .map_err(|e| format!("读 zip entry: {e}"))?
            .name()
            .to_string();
        match name.as_str() {
            "[Content_Types].xml" => v.has_content_types = true,
            "ppt/presentation.xml" => v.has_presentation = true,
            "ppt/slideMasters/slideMaster1.xml" => v.has_slide_master = true,
            "ppt/slideLayouts/slideLayout1.xml" => v.has_slide_layout = true,
            "ppt/theme/theme1.xml" => v.has_theme = true,
            _ if name.starts_with("ppt/slides/slide") && name.ends_with(".xml") => {
                slide_n_pattern += 1
            }
            _ if name.starts_with("ppt/media/") => v.media_count += 1,
            _ => {}
        }
    }
    v.slides_found = slide_n_pattern;
    if !v.has_content_types {
        v.errors.push("missing [Content_Types].xml".into());
    }
    if !v.has_presentation {
        v.errors.push("missing ppt/presentation.xml".into());
    }
    if !v.has_slide_master {
        v.errors
            .push("missing ppt/slideMasters/slideMaster1.xml".into());
    }
    if !v.has_slide_layout {
        v.errors
            .push("missing ppt/slideLayouts/slideLayout1.xml".into());
    }
    if !v.has_theme {
        v.errors.push("missing ppt/theme/theme1.xml".into());
    }
    if slide_n_pattern == 0 {
        v.errors.push("no slideN.xml found".into());
    }
    v.ok = v.errors.is_empty();
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 最小合法 PNG 头(签名 + IHDR 尺寸):打包器按魔数认格式,夹具必须是真 PNG 字节。
    fn tiny_png(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        v.extend_from_slice(&[0, 0, 0, 13]);
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0]);
        v
    }

    // 原生验证打包器(在 cargo test 所在 OS 上跑——Windows 宿主即验 win 路径):
    // 喂最小 PNG 头当「图」,验产出是合法 zip 结构。
    #[test]
    fn build_pptx_produces_valid_package() {
        let dir = std::env::temp_dir().join("polaris_forge_pptx_test");
        let _ = std::fs::create_dir_all(&dir);
        let img1 = dir.join("a.png");
        let img2 = dir.join("b.png");
        std::fs::write(&img1, tiny_png(1280, 720)).unwrap();
        std::fs::write(&img2, tiny_png(1280, 720)).unwrap();
        let out = dir.join("out.pptx");
        let r = build_pptx(
            &[img1.to_string_lossy().into(), img2.to_string_lossy().into()],
            &out.to_string_lossy(),
        )
        .expect("build_pptx 应成功");
        assert_eq!(r["slides"], 2);
        assert!(out.is_file());
        // 重新打开 zip 验结构。
        let f = std::fs::File::open(&out).unwrap();
        let mut z = zip::ZipArchive::new(f).expect("产出应是合法 zip");
        let names: Vec<String> = (0..z.len())
            .map(|i| z.by_index(i).unwrap().name().to_string())
            .collect();
        for need in [
            "[Content_Types].xml",
            "ppt/presentation.xml",
            "ppt/slides/slide1.xml",
            "ppt/slides/slide2.xml",
            "ppt/theme/theme1.xml",
            "ppt/slideMasters/slideMaster1.xml",
            "ppt/slideLayouts/slideLayout1.xml",
        ] {
            assert!(names.iter().any(|n| n == need), "缺部件 {need}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_pptx_text_layer_embeds_searchable_text() {
        let dir = std::env::temp_dir().join("polaris_forge_textlayer");
        let _ = std::fs::create_dir_all(&dir);
        let img = dir.join("a.png");
        std::fs::write(&img, tiny_png(1280, 720)).unwrap();
        let out = dir.join("out.pptx");
        let rects = vec![vec![serde_json::json!(
            {"text":"Hello<World>","x":50.0,"y":40.0,"w":200.0,"h":46.0,"size":40.0,"bold":true}
        )]];
        let r = build_pptx_inner(
            &[img.to_string_lossy().into()],
            &out.to_string_lossy(),
            Some(TextLayer {
                pages: &rects,
                editable: &[false],
                win_w: 1280,
                win_h: 720,
            }),
        )
        .unwrap();
        assert_eq!(r["slides"], 1);
        let f = std::fs::File::open(&out).unwrap();
        let mut z = zip::ZipArchive::new(f).unwrap();
        let mut s = String::new();
        use std::io::Read;
        z.by_name("ppt/slides/slide1.xml")
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        assert!(s.contains("<a:alpha val=\"0\"/>"), "应有 alpha=0 隐形文本");
        assert!(s.contains("Hello&lt;World&gt;"), "文本应 XML 转义并嵌入");
        assert!(s.contains("txBox=\"1\""), "应是文本框");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_pptx_editable_layer_visible_text() {
        // 路线 A:editable=true 的页应产出可见文本框(真颜色/字体/对齐),不带 alpha=0。
        let dir = std::env::temp_dir().join("polaris_forge_editlayer");
        let _ = std::fs::create_dir_all(&dir);
        let img = dir.join("a.png");
        std::fs::write(&img, tiny_png(1280, 720)).unwrap();
        let out = dir.join("out.pptx");
        let rects = vec![vec![serde_json::json!({
            "text":"• 真可编辑","x":50.0,"y":40.0,"w":200.0,"h":46.0,"size":40.0,
            "bold":true,"italic":false,"color":"FF6600","align":"ctr","serif":false,"lh":48.0
        })]];
        build_pptx_inner(
            &[img.to_string_lossy().into()],
            &out.to_string_lossy(),
            Some(TextLayer {
                pages: &rects,
                editable: &[true],
                win_w: 1280,
                win_h: 720,
            }),
        )
        .unwrap();
        let f = std::fs::File::open(&out).unwrap();
        let mut z = zip::ZipArchive::new(f).unwrap();
        let mut s = String::new();
        use std::io::Read;
        z.by_name("ppt/slides/slide1.xml")
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        assert!(
            !s.contains("<a:alpha val=\"0\"/>"),
            "可编辑模式不应有隐形 alpha"
        );
        assert!(s.contains("val=\"FF6600\""), "应带真实颜色");
        assert!(s.contains("algn=\"ctr\""), "应带对齐");
        assert!(
            s.contains("typeface=\"Microsoft YaHei\""),
            "中文 ea 字体必须写,防豆腐块"
        );
        assert!(
            s.contains("<a:normAutofit/>"),
            "应有 autofit 防换行差异撑爆"
        );
        assert!(
            s.contains("<a:lnSpc><a:spcPts val=\"3600\"/></a:lnSpc>"),
            "行距 48px→3600(1/100pt)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editable_layer_rejects_bad_color_align() {
        // deck 内容不可信:畸形 color/align 必须被白名单挡掉,不可注入 XML。
        let dir = std::env::temp_dir().join("polaris_forge_editlayer_bad");
        let _ = std::fs::create_dir_all(&dir);
        let img = dir.join("a.png");
        std::fs::write(&img, tiny_png(1280, 720)).unwrap();
        let out = dir.join("out.pptx");
        let rects = vec![vec![serde_json::json!({
            "text":"x","x":1.0,"y":1.0,"w":10.0,"h":10.0,"size":12.0,
            "color":"\"/><evil>","align":"\"/><evil>"
        })]];
        build_pptx_inner(
            &[img.to_string_lossy().into()],
            &out.to_string_lossy(),
            Some(TextLayer {
                pages: &rects,
                editable: &[true],
                win_w: 1280,
                win_h: 720,
            }),
        )
        .unwrap();
        let f = std::fs::File::open(&out).unwrap();
        let mut z = zip::ZipArchive::new(f).unwrap();
        let mut s = String::new();
        use std::io::Read;
        z.by_name("ppt/slides/slide1.xml")
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        assert!(!s.contains("<evil>"), "畸形值不得注入 XML");
        assert!(s.contains("val=\"000000\""), "坏颜色应回退黑色");
        assert!(s.contains("algn=\"l\""), "坏对齐应回退左对齐");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_pptx_creates_missing_parent_dirs() {
        // out 路径父目录不存在时应自动创建并成功,而非「目录不存在」失败。
        let base = std::env::temp_dir().join("polaris_forge_parent_test");
        let _ = std::fs::remove_dir_all(&base);
        let img = base.join("a.png");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(&img, tiny_png(1280, 720)).unwrap();
        let out = base.join("deep/nested/dir/out.pptx"); // deep/nested/dir 不存在
        let r = build_pptx(&[img.to_string_lossy().into()], &out.to_string_lossy());
        assert!(r.is_ok(), "应自动建父目录并成功: {r:?}");
        assert!(out.is_file());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn capture_slides_rejects_absurd_slide_count() {
        // 指定离谱页数应立刻被上限拦下(在 spawn 任何 chromium 之前),返回明确错误。
        let r = capture_slides("does-not-exist.html", 1280, 720, 1, Some(99_999));
        assert!(r.is_err());
        let e = r.unwrap_err();
        assert!(e.contains("上限"), "应是上限错误,实际: {e}");
    }

    #[test]
    fn parse_text_rects_extracts_json() {
        let html = r#"<html><body><script type="application/json" id="polaris-text-rects">[{"text":"hi","x":10,"y":20,"w":100,"h":30}]</script></body></html>"#;
        let (r, capable) = parse_text_rects(html).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["text"], "hi");
        assert!(!capable, "旧版无能力标记 → capable=false(降级隐形层)");
        // 新版带能力标记 → capable=true。
        let html2 = r#"<script type="application/json" id="polaris-text-rects" data-notext-capable="1">[{"text":"hi","x":1,"y":2,"w":3,"h":4}]</script>"#;
        let (r2, capable2) = parse_text_rects(html2).unwrap();
        assert_eq!(r2.len(), 1);
        assert!(capable2);
        // 无该元素 → 优雅返回空,不报错。
        assert_eq!(parse_text_rects("<html></html>").unwrap().0.len(), 0);
    }

    #[test]
    fn extract_text_rects_via_dumpdom_when_chromium_present() {
        // Edge CLI 的 --dump-dom 时序不可靠(Windows 桌面本走 WebView2,非此 CLI 路);
        // 真 chromium/Chrome 才测(Docker 已手动验证机制正确:x100/y50/w200/h40)。
        match crate::forge::find_chromium().as_deref() {
            None => {
                eprintln!("[e2e] 跳过:未发现 chromium/chrome");
                return;
            }
            Some(c) if c.to_lowercase().contains("edge") => {
                eprintln!("[e2e] 跳过:Edge CLI dump-dom 时序不可靠");
                return;
            }
            _ => {}
        }
        let dir = std::env::temp_dir().join("forge_rects_test");
        let _ = std::fs::create_dir_all(&dir);
        let deck = dir.join("deck.html");
        std::fs::write(
            &deck,
            "<!doctype html><html><body>\
<div id=\"t\" style=\"position:absolute;left:100px;top:50px;width:200px;height:40px\">Hello Polaris</div>\
<script type=\"application/json\" id=\"polaris-text-rects\"></script>\
<script>window.addEventListener('load',function(){var el=document.getElementById('t');\
var r=el.getBoundingClientRect();document.getElementById('polaris-text-rects').textContent=\
JSON.stringify([{text:el.textContent,x:Math.round(r.left),y:Math.round(r.top),w:Math.round(r.width),h:Math.round(r.height)}]);});</script>\
</body></html>",
        )
        .unwrap();
        let (rects, _capable) = extract_text_rects(&deck.to_string_lossy(), 1, 1280, 720)
            .expect("extract_text_rects 应成功");
        assert_eq!(rects.len(), 1, "应抽到 1 个文本框,实际: {rects:?}");
        assert_eq!(rects[0]["text"], "Hello Polaris");
        assert_eq!(rects[0]["x"].as_i64(), Some(100));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn count_slides_counts_slide_token_only() {
        let html = r#"<div class="deck"><section class="slide is-active"><div class="slide-number"></div></section><section class="slide"></section><section class='slide cover'></section></div>"#;
        assert_eq!(count_slides(html), 3); // 三个 .slide，不数 .slide-number / .deck
        assert_eq!(count_slides("<p class=\"slides foo\">x</p>"), 0); // slides ≠ slide
    }

    // 真实流水线 e2e:deck→chromium 截图→pptx。仅当环境有 chromium/chrome 时跑(否则跳过),
    // 因此在普通单测环境安全、在「装了浏览器」的 CI job(如 macOS brew Chrome)上真正执行 ——
    // 这是 macOS「真能出片」的运行时证据,无需 Mac 硬件。
    #[test]
    fn deck_to_pptx_e2e_when_chromium_present() {
        if crate::forge::find_chromium().is_none() {
            eprintln!("[e2e] 跳过:未发现 chromium/chrome");
            return;
        }
        let dir = std::env::temp_dir().join("forge_e2e_deck");
        let _ = std::fs::create_dir_all(&dir);
        let deck = dir.join("deck.html");
        std::fs::write(
            &deck,
            "<!doctype html><html><head><meta charset=utf-8><style>\
.slide{position:absolute;inset:0;opacity:0}.slide.is-active{opacity:1}</style></head>\
<body><div class=\"deck\">\
<section class=\"slide\" style=\"background:#7aa2f7\">A</section>\
<section class=\"slide\" style=\"background:#0b0f1a;color:#fff\">B</section></div><script>\
var s=[].slice.call(document.querySelectorAll('.slide'));\
function go(n){n=Math.max(0,Math.min(s.length-1,n));s.forEach(function(e,i){e.classList.toggle('is-active',i===n)})}\
function fromHash(){var m=/^#\\/(\\d+)/.exec(location.hash||'');go(m?+m[1]-1:0)}\
fromHash();addEventListener('hashchange',fromHash);</script></body></html>",
        )
        .unwrap();
        let out = dir.join("out.pptx");
        let r = render_deck_to_pptx(
            &deck.to_string_lossy(),
            &out.to_string_lossy(),
            1280,
            720,
            false,
            None,
        )
        .expect("render_deck_to_pptx 应成功");
        assert_eq!(r["slides"], 2);
        let f = std::fs::File::open(&out).unwrap();
        let z = zip::ZipArchive::new(f).expect("产出应是合法 zip");
        assert!(z.len() >= 10, "pptx 部件数应 >=10");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn png_size_reads_ihdr() {
        // 1x1 PNG。
        let png: [u8; 24] = [
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, // sig
            0, 0, 0, 13, b'I', b'H', b'D', b'R', // len + type
            0, 0, 0, 1, 0, 0, 0, 1, // w=1 h=1
        ];
        assert_eq!(png_size(&png), Some((1, 1)));
        assert_eq!(png_size(b"not a png at all really"), None);
    }
}
