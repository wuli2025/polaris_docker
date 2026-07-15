//! 色彩调配引擎 — 给一个种子色(或主题情绪),自动生成一整套**协调、对比度达标**的主题。
//!
//! 这是全 app「色彩调配能力」的唯一真源:输出的 token 形状对齐 open-design
//! (bg / surface / border / 三级文字 / accent×3 / grad / glow),deck / 网页 / PPT /
//! 信息图 / 视频都从这里取色,不再只能从固定 36 套里挑。
//!
//! 设计原则:
//!   ① 协调 = 强调色按色彩和谐(同类 +28° / 互补 +180°)派生,背景/卡片是种子色的极低饱和暗(或亮)调;
//!   ② 不翻车 = 三级文字按 WCAG 相对亮度对背景做对比度兜底(text-1≥7、text-2≥4.5),不够就自动提/压明度;
//!   ③ 深浅两版 = 同一种子同时出 light / dark,任何媒介按场景取用;
//!   ④ 纯色彩数学,零依赖,三平台同构。
//!
//! 入口: palette_generate(seed, mood, mode)

use serde::Serialize;

// ───────────────────────── 颜色基元 ─────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
struct Rgb {
    r: f64,
    g: f64,
    b: f64,
} // 0..255

#[derive(Clone, Copy, Debug)]
struct Hsl {
    h: f64, // 0..360
    s: f64, // 0..1
    l: f64, // 0..1
}

fn clamp01(x: f64) -> f64 {
    x.max(0.0).min(1.0)
}

fn parse_hex(s: &str) -> Option<Rgb> {
    let s = s.trim().trim_start_matches('#');
    let s = if s.len() == 3 {
        // #abc → #aabbcc
        s.chars().flat_map(|c| [c, c]).collect::<String>()
    } else {
        s.to_string()
    };
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(Rgb {
        r: r as f64,
        g: g as f64,
        b: b as f64,
    })
}

fn to_hex(c: Rgb) -> String {
    format!(
        "#{:02X}{:02X}{:02X}",
        c.r.round().clamp(0.0, 255.0) as u8,
        c.g.round().clamp(0.0, 255.0) as u8,
        c.b.round().clamp(0.0, 255.0) as u8
    )
}

fn rgb_to_hsl(c: Rgb) -> Hsl {
    let r = c.r / 255.0;
    let g = c.g / 255.0;
    let b = c.b / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    let (h, s) = if d.abs() < 1e-9 {
        (0.0, 0.0)
    } else {
        let s = d / (1.0 - (2.0 * l - 1.0).abs());
        let h = if (max - r).abs() < 1e-9 {
            ((g - b) / d).rem_euclid(6.0)
        } else if (max - g).abs() < 1e-9 {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        (h * 60.0, s)
    };
    Hsl {
        h: h.rem_euclid(360.0),
        s: clamp01(s),
        l: clamp01(l),
    }
}

fn hsl_to_rgb(c: Hsl) -> Rgb {
    let h = c.h.rem_euclid(360.0) / 360.0;
    let s = clamp01(c.s);
    let l = clamp01(c.l);
    if s.abs() < 1e-9 {
        let v = l * 255.0;
        return Rgb { r: v, g: v, b: v };
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let hue = |mut t: f64| -> f64 {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 1.0 / 2.0 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    Rgb {
        r: hue(h + 1.0 / 3.0) * 255.0,
        g: hue(h) * 255.0,
        b: hue(h - 1.0 / 3.0) * 255.0,
    }
}

/// WCAG 相对亮度(用于对比度)。
fn rel_luminance(c: Rgb) -> f64 {
    let f = |v: f64| {
        let v = v / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b)
}

/// WCAG 对比度(1..21)。
fn contrast(a: Rgb, b: Rgb) -> f64 {
    let la = rel_luminance(a);
    let lb = rel_luminance(b);
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

fn with_l(h: Hsl, l: f64) -> Hsl {
    Hsl { l: clamp01(l), ..h }
}
fn with_sl(h: Hsl, s: f64, l: f64) -> Hsl {
    Hsl {
        s: clamp01(s),
        l: clamp01(l),
        ..h
    }
}
fn rot(h: Hsl, deg: f64) -> Hsl {
    Hsl {
        h: (h.h + deg).rem_euclid(360.0),
        ..h
    }
}

/// 从背景出发,沿明度方向找到第一个对比度达标的文字色(保留色相,做有色中性字)。
fn text_for(bg: Rgb, hue: f64, sat: f64, start_l: f64, target: f64, dark_bg: bool) -> Rgb {
    let mut l = start_l;
    let step = if dark_bg { 0.02 } else { -0.02 }; // 深底往亮调,浅底往暗调
    for _ in 0..60 {
        let c = hsl_to_rgb(Hsl { h: hue, s: sat, l });
        if contrast(c, bg) >= target {
            return c;
        }
        l = clamp01(l + step);
        if l <= 0.0 || l >= 1.0 {
            break;
        }
    }
    hsl_to_rgb(Hsl {
        h: hue,
        s: sat,
        l: clamp01(l),
    })
}

/// 沿明度调强调色直到对背景对比度达标(保色相/饱和):浅底压暗、深底提亮。
/// 治「绿/黄等高亮色相在白底上看不见」。
fn accent_fit(mut a: Hsl, bg: Rgb, target: f64, dark_bg: bool) -> Hsl {
    let step = if dark_bg { 0.02 } else { -0.02 };
    for _ in 0..40 {
        if contrast(hsl_to_rgb(a), bg) >= target {
            break;
        }
        a.l = clamp01(a.l + step);
        if a.l <= 0.04 || a.l >= 0.96 {
            break;
        }
    }
    a
}

// ───────────────────────── 主题 token ─────────────────────────

/// 一套主题色(对齐 open-design token)。所有字段为 #RRGGBB。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Palette {
    pub mode: String, // "light" | "dark"
    pub bg: String,
    pub bg_soft: String,
    pub surface: String,
    pub surface2: String,
    pub border: String,
    pub border_strong: String,
    pub text1: String,
    pub text2: String,
    pub text3: String,
    pub accent: String,
    pub accent2: String,
    pub accent3: String,
    /// 背景柔光团色(贴近 bg 的微亮调,做 radial-glow 渐变背景用)
    pub glow: String,
    /// 渐变两端
    pub grad_from: String,
    pub grad_to: String,
    /// 自检:三级文字对 bg 的实测对比度(便于前端/调试看是否达标)
    pub contrast_text1: f64,
    pub contrast_text2: f64,
    pub contrast_text3: f64,
}

fn build_dark(seed: Hsl) -> Palette {
    let h = seed.h;
    let bg = Hsl {
        h,
        s: 0.16,
        l: 0.065,
    };
    let bg_rgb = hsl_to_rgb(bg);
    // accent:在深底上要亮而不刺眼,且保证对底对比度达标
    let accent = accent_fit(
        with_sl(seed, clamp01(seed.s.max(0.58).min(0.88)), 0.62),
        bg_rgb,
        3.5,
        true,
    );
    let accent2 = accent_fit(with_l(rot(accent, 28.0), 0.66), bg_rgb, 3.0, true);
    let accent3 = accent_fit(with_sl(rot(accent, 174.0), 0.62, 0.64), bg_rgb, 3.0, true);

    let bg_soft = with_l(bg, 0.045);
    let surface = Hsl {
        h,
        s: 0.16,
        l: 0.115,
    };
    let surface2 = with_l(surface, 0.155);
    let border = Hsl {
        h,
        s: 0.16,
        l: 0.255,
    };
    let border_strong = with_l(border, 0.36);
    let glow = Hsl {
        h,
        s: 0.32,
        l: 0.17,
    };

    let t1 = text_for(bg_rgb, h, 0.12, 0.94, 7.5, true);
    let t2 = text_for(bg_rgb, h, 0.14, 0.72, 4.6, true);
    let t3 = text_for(bg_rgb, h, 0.14, 0.55, 3.0, true);

    Palette {
        mode: "dark".into(),
        bg: to_hex(bg_rgb),
        bg_soft: to_hex(hsl_to_rgb(bg_soft)),
        surface: to_hex(hsl_to_rgb(surface)),
        surface2: to_hex(hsl_to_rgb(surface2)),
        border: to_hex(hsl_to_rgb(border)),
        border_strong: to_hex(hsl_to_rgb(border_strong)),
        text1: to_hex(t1),
        text2: to_hex(t2),
        text3: to_hex(t3),
        accent: to_hex(hsl_to_rgb(accent)),
        accent2: to_hex(hsl_to_rgb(accent2)),
        accent3: to_hex(hsl_to_rgb(accent3)),
        glow: to_hex(hsl_to_rgb(glow)),
        grad_from: to_hex(hsl_to_rgb(accent)),
        grad_to: to_hex(hsl_to_rgb(accent2)),
        contrast_text1: (contrast(t1, bg_rgb) * 100.0).round() / 100.0,
        contrast_text2: (contrast(t2, bg_rgb) * 100.0).round() / 100.0,
        contrast_text3: (contrast(t3, bg_rgb) * 100.0).round() / 100.0,
    }
}

fn build_light(seed: Hsl) -> Palette {
    let h = seed.h;
    let bg = Hsl {
        h,
        s: 0.10,
        l: 0.985,
    };
    let bg_rgb = hsl_to_rgb(bg);
    // accent:在白底上要够深才压得住,且保证对底对比度达标(绿/黄等高亮色相自动压暗)
    let accent = accent_fit(
        with_sl(seed, clamp01(seed.s.max(0.62)), 0.48),
        bg_rgb,
        3.2,
        false,
    );
    let accent2 = accent_fit(with_l(rot(accent, 28.0), 0.52), bg_rgb, 3.0, false);
    let accent3 = accent_fit(with_sl(rot(accent, 174.0), 0.6, 0.46), bg_rgb, 3.0, false);

    let bg_soft = with_l(bg, 0.955);
    let surface = Hsl {
        h,
        s: 0.10,
        l: 0.995,
    };
    let surface2 = with_l(surface, 0.945);
    let border = Hsl {
        h,
        s: 0.22,
        l: 0.88,
    };
    let border_strong = with_l(border, 0.74);
    let glow = Hsl {
        h,
        s: 0.45,
        l: 0.93,
    };

    let t1 = text_for(bg_rgb, h, 0.30, 0.14, 11.0, false);
    let t2 = text_for(bg_rgb, h, 0.22, 0.36, 5.0, false);
    let t3 = text_for(bg_rgb, h, 0.18, 0.55, 3.0, false);

    Palette {
        mode: "light".into(),
        bg: to_hex(bg_rgb),
        bg_soft: to_hex(hsl_to_rgb(bg_soft)),
        surface: to_hex(hsl_to_rgb(surface)),
        surface2: to_hex(hsl_to_rgb(surface2)),
        border: to_hex(hsl_to_rgb(border)),
        border_strong: to_hex(hsl_to_rgb(border_strong)),
        text1: to_hex(t1),
        text2: to_hex(t2),
        text3: to_hex(t3),
        accent: to_hex(hsl_to_rgb(accent)),
        accent2: to_hex(hsl_to_rgb(accent2)),
        accent3: to_hex(hsl_to_rgb(accent3)),
        glow: to_hex(hsl_to_rgb(glow)),
        grad_from: to_hex(hsl_to_rgb(accent)),
        grad_to: to_hex(hsl_to_rgb(accent2)),
        contrast_text1: (contrast(t1, bg_rgb) * 100.0).round() / 100.0,
        contrast_text2: (contrast(t2, bg_rgb) * 100.0).round() / 100.0,
        contrast_text3: (contrast(t3, bg_rgb) * 100.0).round() / 100.0,
    }
}

/// 主题情绪 → 种子色相(没给 hex 时用)。
fn mood_to_seed(mood: &str) -> &'static str {
    let m = mood.to_lowercase();
    let has = |ks: &[&str]| ks.iter().any(|k| m.contains(k));
    if has(&["科技", "tech", "数码", "未来", "ai", "蓝"]) {
        "#3B6CFF"
    } else if has(&["商务", "金融", "企业", "靛", "稳重"]) {
        "#2563EB"
    } else if has(&["活力", "热情", "运动", "橙", "能量"]) {
        "#FF6B2C"
    } else if has(&["自然", "环保", "健康", "绿", "清新"]) {
        "#2FAE66"
    } else if has(&["医疗", "医药", "青", "干净"]) {
        "#0EA5A5"
    } else if has(&["优雅", "高端", "紫", "神秘", "艺术"]) {
        "#8B5CF6"
    } else if has(&["奢", "金", "高级", "尊贵", "典雅"]) {
        "#C9A227"
    } else if has(&["热烈", "喜庆", "红", "警示"]) {
        "#E0445A"
    } else if has(&["温暖", "亲和", "粉", "柔"]) {
        "#FF5C8A"
    } else {
        "#3B6CFF" // 默认科技蓝
    }
}

/// 生成结果:深浅两版 + 可直接用的输出适配。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ThemeResult {
    /// 实际使用的种子色
    pub seed: String,
    pub light: Palette,
    pub dark: Palette,
    /// deck / 网页 / 信息图直接贴的 CSS([data-theme="custom"] 覆盖块,含深浅)
    pub css: String,
    /// PPT(pptx.md THEMES)直接填的字典文本(深 / 浅各一行)
    pub pptx_dark: String,
    pub pptx_light: String,
}

/// CSS 适配:把一套 Palette 写成 open-design token 覆盖(deck/web/信息图直接吃)。
fn palette_css_vars(p: &Palette) -> String {
    format!(
        "--bg:{};--bg-soft:{};--surface:{};--surface-2:{};--border:{};--border-strong:{};\
--text-1:{};--text-2:{};--text-3:{};--accent:{};--accent-2:{};--accent-3:{};\
--grad:linear-gradient(135deg,{},{});--glow:{};",
        p.bg,
        p.bg_soft,
        p.surface,
        p.surface2,
        p.border,
        p.border_strong,
        p.text1,
        p.text2,
        p.text3,
        p.accent,
        p.accent2,
        p.accent3,
        p.grad_from,
        p.grad_to,
        p.glow
    )
}

/// PPT 适配:写成 pptx.md THEMES 那一行的字典(hex 去 # 给 0x)。
fn palette_pptx(name: &str, p: &Palette) -> String {
    fn x(s: &str) -> &str {
        s.trim_start_matches('#')
    }
    format!(
        "\"{}\": dict(bg=0x{}, surf=0x{}, border=0x{}, t1=0x{}, t2=0x{}, t3=0x{}, accent=0x{}, radius=0.10),",
        name, x(&p.bg), x(&p.surface), x(&p.border), x(&p.text1), x(&p.text2), x(&p.text3), x(&p.accent)
    )
}

/// 核心:种子色(可空,空则用 mood)→ 协调主题(深浅两版 + CSS/PPT 适配)。
pub fn generate(seed: Option<&str>, mood: Option<&str>) -> Result<ThemeResult, String> {
    let seed_hex = match (seed, mood) {
        (Some(s), _) if parse_hex(s).is_some() => s.to_string(),
        (_, Some(m)) => mood_to_seed(m).to_string(),
        _ => "#3B6CFF".to_string(),
    };
    let rgb = parse_hex(&seed_hex).ok_or_else(|| format!("无法解析种子色: {seed_hex}"))?;
    let hsl = rgb_to_hsl(rgb);

    let light = build_light(hsl);
    let dark = build_dark(hsl);
    let css = format!(
        "[data-theme=\"custom\"]{{{}}}\n[data-theme=\"custom-dark\"]{{{}}}",
        palette_css_vars(&light),
        palette_css_vars(&dark)
    );
    let pptx_dark = palette_pptx("custom-dark", &dark);
    let pptx_light = palette_pptx("custom-light", &light);

    Ok(ThemeResult {
        seed: to_hex(rgb),
        light,
        dark,
        css,
        pptx_dark,
        pptx_light,
    })
}

// ───────────────────────── Tauri 命令 ─────────────────────────

/// 色彩调配:给种子色(#hex)或主题情绪,出一整套协调主题(深浅两版 + CSS/PPT 适配)。
/// 全 app 各产出(deck / 网页 / PPT / 信息图 / 视频)共用此能力现调配色。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn palette_generate(seed: Option<String>, mood: Option<String>) -> Result<ThemeResult, String> {
    generate(seed.as_deref(), mood.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(h: &str) -> Rgb {
        parse_hex(h).unwrap()
    }

    #[test]
    fn hex_roundtrip_and_hsl() {
        let c = p("#4F8CFF");
        let back = parse_hex(&to_hex(c)).unwrap();
        assert!(
            (c.r - back.r).abs() < 1.0 && (c.g - back.g).abs() < 1.0 && (c.b - back.b).abs() < 1.0
        );
        // hsl 往返
        let h = rgb_to_hsl(c);
        let r2 = hsl_to_rgb(h);
        assert!((c.r - r2.r).abs() < 2.0 && (c.g - r2.g).abs() < 2.0 && (c.b - r2.b).abs() < 2.0);
    }

    #[test]
    fn contrast_known_values() {
        // 纯黑/纯白对比 = 21
        assert!((contrast(p("#000000"), p("#FFFFFF")) - 21.0).abs() < 0.1);
        // 同色 = 1
        assert!((contrast(p("#777777"), p("#777777")) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn generated_theme_meets_contrast() {
        // 多个种子色:三级文字对各自 bg 的对比度都要达标(不翻车的核心保证)。
        for seed in [
            "#4F8CFF", "#E0445A", "#2FAE66", "#C9A227", "#8B5CF6", "#0EA5A5",
        ] {
            let r = generate(Some(seed), None).unwrap();
            for pal in [&r.light, &r.dark] {
                let bg = p(&pal.bg);
                assert!(
                    contrast(p(&pal.text1), bg) >= 7.0,
                    "{seed} {} text1 对比不足: {}",
                    pal.mode,
                    contrast(p(&pal.text1), bg)
                );
                assert!(
                    contrast(p(&pal.text2), bg) >= 4.5,
                    "{seed} {} text2 对比不足",
                    pal.mode
                );
                assert!(
                    contrast(p(&pal.text3), bg) >= 3.0,
                    "{seed} {} text3 对比不足",
                    pal.mode
                );
                // 强调色对背景也要看得见(≥3,大色块/标题级)
                assert!(
                    contrast(p(&pal.accent), bg) >= 3.0,
                    "{seed} {} accent 对比不足: {}",
                    pal.mode,
                    contrast(p(&pal.accent), bg)
                );
            }
        }
    }

    #[test]
    fn harmony_is_coherent() {
        // accent2 与 accent 应同类(色相差≈28°),accent3 应近互补(≈174°)。
        let r = generate(Some("#3B6CFF"), None).unwrap();
        let a = rgb_to_hsl(p(&r.dark.accent)).h;
        let a2 = rgb_to_hsl(p(&r.dark.accent2)).h;
        let a3 = rgb_to_hsl(p(&r.dark.accent3)).h;
        let dh = |x: f64, y: f64| ((x - y + 540.0).rem_euclid(360.0) - 180.0).abs();
        assert!(
            dh(a, a2) > 12.0 && dh(a, a2) < 45.0,
            "accent2 不是同类色: {}",
            dh(a, a2)
        );
        assert!(dh(a, a3) > 150.0, "accent3 不是互补色: {}", dh(a, a3));
    }

    #[test]
    fn mood_maps_to_seed() {
        assert_eq!(
            generate(None, Some("做个科技感的发布会")).unwrap().seed,
            "#3B6CFF"
        );
        assert_eq!(
            generate(None, Some("自然环保主题")).unwrap().seed,
            "#2FAE66"
        );
        assert_eq!(
            generate(None, Some("高级奢华尊贵")).unwrap().seed,
            "#C9A227"
        );
    }

    /// 打印若干种子生成的 PPT 主题字典 + 光晕色,供真机渲染 demo 用。
    /// cargo test --features server demo_print -- --ignored --nocapture
    #[test]
    #[ignore]
    fn demo_print() {
        for seed in ["#C9A227", "#0EA5A5", "#8B5CF6"] {
            let r = generate(Some(seed), None).unwrap();
            println!("SEED {seed}");
            println!("  {}", r.pptx_dark);
            println!("  glow_dark={}", r.dark.glow);
            println!("  {}", r.pptx_light);
        }
    }

    #[test]
    fn outputs_are_usable() {
        let r = generate(Some("#4F8CFF"), None).unwrap();
        assert!(r.css.contains("[data-theme=\"custom\"]") && r.css.contains("--accent:"));
        assert!(r.pptx_dark.contains("dict(bg=0x") && r.pptx_dark.contains("accent=0x"));
    }
}
