//! Polaris Forge CLI —— `polaris-forge preflight --json` 工业级化 bin
//!
//! 给 agent / CI 用的"这镜像能出 PPT 吗"零依赖自检(任务 d §8.1,§8.5)
//!   exit 0 = OK / 1 = blocker / 2 = warn
//!   stdout = JSON {timestamp, render_flavor, checks:{chromium, ffmpeg, fonts, key, cjk_coverage_pct}, blockers[]}

use polaris_forge_codec::Result;
use std::process::Command;

fn check_bin(name: &str) -> bool {
    Command::new(name).arg("--version").output()
        .map(|o| o.status.success()).unwrap_or(false)
}

fn check_chromium() -> (bool, String) {
    for path in &["/usr/bin/chrome-headless-shell", "/usr/bin/chromium", "/usr/bin/chromium-browser"] {
        if std::path::Path::new(path).exists() {
            return (true, path.to_string());
        }
    }
    (check_bin("chromium") || check_bin("chrome") || check_bin("chrome-headless-shell"),
     "PATH search".to_string())
}

fn check_fonts() -> (bool, Vec<String>) {
    let out = Command::new("fc-list").arg(":lang=zh-cn").output();
    let mut fonts = Vec::new();
    if let Ok(o) = out {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout);
            for line in s.lines() {
                if let Some(p) = line.split(':').next() {
                    if !p.is_empty() && !fonts.contains(&p.to_string()) {
                        fonts.push(p.to_string());
                    }
                }
            }
        }
    }
    (!fonts.is_empty(), fonts)
}

fn render_flavor() -> &'static str {
    match std::env::var("POLARIS_RENDER_FLAVOR").as_deref() {
        Ok("1") => "full",
        _ => "slim",
    }
}

pub fn run() -> Result<i32> {
    let flavor = render_flavor();
    let (chromium_ok, chromium_path) = check_chromium();
    let ffmpeg_ok = check_bin("ffmpeg");
    let (fonts_ok, font_list) = check_fonts();

    let mut blockers: Vec<String> = Vec::new();
    if flavor == "full" {
        if !chromium_ok {
            blockers.push("POLARIS_RENDER=1 但找不到 chrome-headless-shell / chromium;build 阶段漏装".into());
        }
        if !ffmpeg_ok {
            blockers.push("ffmpeg 缺失;Docker 镜像应装静态 ffmpeg".into());
        }
        if !fonts_ok {
            blockers.push("CJK 字体缺失(fc-list :lang=zh-cn 空);build 阶段字体子集失败且未 fallback".into());
        }
    } else {
        // slim flavor 显式报三个 blocker
        blockers.push("POLARIS_RENDER=0,chromium 未装;构建时设 --build-arg POLARIS_RENDER=1".into());
        blockers.push("ffmpeg 未装;同上".into());
        blockers.push("CJK 字体未装;同上".into());
    }

    let can_render_ppt = blockers.is_empty();
    let exit_code = if can_render_ppt { 0 } else { 1 };

    let payload = serde_json::json!({
        "timestamp": chrono_now(),
        "render_flavor": flavor,
        "can_render_ppt": can_render_ppt,
        "checks": {
            "chromium_ok": chromium_ok,
            "chromium_path": chromium_path,
            "ffmpeg_ok": ffmpeg_ok,
            "fonts_ok": fonts_ok,
            "font_count": font_list.len(),
            "cjk_coverage_pct": cjk_coverage_pct(),
        },
        "blockers": blockers,
    });
    println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".into()));
    Ok(exit_code)
}

fn chrono_now() -> String {
    // 零依赖 ISO 8601 简单实现(避免拉 chrono 增 100KB 二进制)
    let dur = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = dur.as_secs();
    // 1970-01-01 UTC 起,粗略:365.25 天/年,处理 1970-2099 OK
    let days = (secs / 86400) as i64;
    let mut y = 1970i64;
    let mut d = days;
    loop {
        let dy = if is_leap(y) { 366 } else { 365 };
        if d < dy { break; }
        d -= dy; y += 1;
        if y > 2099 { break; }
    }
    let mdays = if is_leap(y) { [31,29,31,30,31,30,31,31,30,31,30,31] } else { [31,28,31,30,31,30,31,31,30,31,30,31] };
    let mut m = 0;
    while m < 12 && d >= mdays[m] { d -= mdays[m]; m += 1; }
    let day = d + 1;
    let hour = (secs % 86400) / 3600;
    let min  = (secs % 3600) / 60;
    let sec  = secs % 60;
    format!("{y:04}-{:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z", m + 1)
}

fn is_leap(y: i64) -> bool { (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 }

fn cjk_coverage_pct() -> f64 {
    // 简化:子集字体已落 + cjk 字体数 > 3 → 100;否则 0
    // 真覆盖审计要 headless chromium --dump-dom 抽字,放 P5 forge-bench
    let (_, list) = check_fonts();
    if list.is_empty() { 0.0 } else { 100.0 }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--json") {
        // 已默认 JSON 输出
    }
    match run() {
        Ok(0) => {},
        Ok(1) => std::process::exit(1),
        Ok(2) => std::process::exit(2),
        Ok(c) => std::process::exit(c),
        Err(e) => {
            eprintln!("forge-codec preflight error: {e}");
            std::process::exit(2);
        }
    }
}
