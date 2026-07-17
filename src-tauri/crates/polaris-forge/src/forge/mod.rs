//! Polaris Forge · 跨平台渲染能力 preflight(对应《Forge 跨平台 PRD》§06 降级阶梯表)。
//!
//! 本模块**不做渲染**——Forge 渲染引擎(capture/codec/tts/pptx/fx)是 P0–P5 的工程路线。
//! 它先把「这台机器 / 这个容器**能走哪条渲染路、缺什么会降到哪**」探测清楚并透明上报:
//! 产品据此自动选路 + UI 红绿灯,落实两份 PRD 反复强调的「失败被设计过、每级降级都仍交付
//! 可用的东西」。三平台(Windows/macOS/Docker)各自报自己的阶梯,`cfg!(target_os)` 感知。
//!
//! 这是 Forge 工程的**第一块落地件**:在写任何重后端之前,先有一个诚实的能力地图,让用户
//! 一眼看清「我这环境出 PPT/视频走哪条路、要不要补东西」,而不是跑到一半报错。

pub mod capture; // 工业级化:持久 CDP + 5 档 fallback 链(替 video 的 per-frame CLI)
                 // Figma 往返桥(REST 拉节点树+图片内嵌):设计成品域,分仓规划 v2 同落 polaris-forge 仓
                 // (Phase 0 文件归位; lib.rs 有 crate 根别名保持 `crate::figma_bridge` 旧路径)。
pub mod figma_bridge;
pub mod fx_safe; // 工业级化:动效错误隔离 + spring 闭式解(任务 c §C.2 §C.3)
pub mod image; // 文生图(多家可配,纯 Rust)→ 配置由壳注入(见 polaris-app/src/imagegen.rs)
pub mod pptx;
pub mod pptx_native; // 路线 B:spec JSON → 原生可编辑 .pptx(零浏览器,Docker slim 可用)
pub mod tts;
pub mod video;

use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

/// 外部命令超时看门已归位横切基建(polaris-runtime):进程工具不属于渲染引擎。
/// 此处再导出保持 `forge::run_with_timeout` 旧路径(capture/pptx/tts/video 内部调用零改动)。
pub use crate::runtime::run_with_timeout;

/// 当前平台标识(给前端按平台展示对应阶梯)。
pub fn platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if Path::new("/.dockerenv").exists() || std::env::var("POLARIS_RENDER_FLAVOR").is_ok() {
        "docker"
    } else {
        "linux"
    }
}

/// 试运行一个可执行 + 版本参数, 成功(能 spawn 且退出码 0)即视为可用, 返回其名/路径。
fn probe_exe(cmd: &str, version_arg: &str) -> bool {
    Command::new(cmd)
        .arg(version_arg)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 应用自带二进制发现(零安装打包用):查 exe 同级 / bin / vendor,以及 macOS `.app/Contents/Resources`。
/// 让桌面 App 把 chromium/ffmpeg 打进包里 → 用户**什么都不用装**(objc2 原生后端之外的零安装正路)。
fn bundled_exe(names: &[&str]) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    let mut roots = vec![dir.clone(), dir.join("bin"), dir.join("vendor")];
    // macOS: exe 在 .app/Contents/MacOS/ → 资源在 ../Resources(及其 bin/)。
    if let Some(contents) = dir.parent() {
        roots.push(contents.join("Resources"));
        roots.push(contents.join("Resources").join("bin"));
    }
    for r in roots {
        for n in names {
            #[cfg(target_os = "windows")]
            {
                let pe = r.join(format!("{n}.exe"));
                if pe.is_file() {
                    return Some(pe.to_string_lossy().to_string());
                }
            }
            let p = r.join(n);
            if p.is_file() {
                return Some(p.to_string_lossy().to_string());
            }
        }
    }
    None
}

/// 本地路径 → 浏览器可加载的 file:// URL(截图/取词/视频取帧共用,三平台唯一出口)。
///
/// **Windows 关键坑**: `std::fs::canonicalize` 返回 `\\?\C:\...` 扩展长度前缀,直接拼
/// URL 会变成 `file:////?/C:/...` —— 第一个 `?` 被浏览器当 query 分隔符 → 加载失败
/// 回落新标签页 → 截图全是空白灰图却报成功(真机实测复现)。必须剥掉前缀。
/// 路径里的 `% # ?` 也要编码,否则 `?export=1#/N` 锚点错位截错页。
pub fn path_to_file_url(path: &str) -> Result<String, String> {
    let abs = std::fs::canonicalize(path).map_err(|e| format!("找不到文件 {path}: {e}"))?;
    let mut s = abs.to_string_lossy().replace('\\', "/");
    // 剥 Windows 扩展前缀: //?/C:/... → C:/... ; UNC //?/UNC/server/share → //server/share
    if let Some(rest) = s.strip_prefix("//?/UNC/") {
        s = format!("//{rest}");
    } else if let Some(rest) = s.strip_prefix("//?/") {
        s = rest.to_string();
    }
    // 只编码会破坏 URL 结构的字符(% 必须最先,避免二次编码)。
    let s = s
        .replace('%', "%25")
        .replace('#', "%23")
        .replace('?', "%3F");
    Ok(format!("file:///{}", s.trim_start_matches('/')))
}

/// 找 chromium/chrome/edge 可执行: env → 应用自带(零安装打包)→ 平台候选名探测。
pub fn find_chromium() -> Option<String> {
    if let Ok(p) = std::env::var("POLARIS_CHROMIUM") {
        if !p.is_empty() && (Path::new(&p).is_file() || probe_exe(&p, "--version")) {
            return Some(p);
        }
    }
    // 应用自带的浏览器优先(零安装):打进包的 chromium / chrome-headless-shell。
    if let Some(p) = bundled_exe(&["chrome-headless-shell", "chromium", "chrome", "Chromium"]) {
        return Some(p);
    }
    #[allow(unused_mut)] // macOS 分支才 push，其余平台不需要 mut
    let mut candidates: Vec<&str> = vec!["chromium", "chromium-browser", "google-chrome", "chrome"];
    // Windows: Edge/Chrome 常驻固定路径(不在 PATH 也能用)。
    #[cfg(target_os = "windows")]
    let win_paths = [
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
    ];
    #[cfg(target_os = "windows")]
    for p in win_paths {
        if Path::new(p).is_file() {
            return Some(p.to_string());
        }
    }
    // macOS: Chrome 标准安装路径。
    #[cfg(target_os = "macos")]
    {
        let mac = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
        if Path::new(mac).is_file() {
            return Some(mac.to_string());
        }
        candidates.push("/Applications/Chromium.app/Contents/MacOS/Chromium");
    }
    candidates
        .into_iter()
        .find(|c| probe_exe(c, "--version"))
        .map(|s| s.to_string())
}

/// ffmpeg 是否可用(逃生口 / Docker 主编码器)。
fn find_ffmpeg() -> bool {
    let cmd = std::env::var("POLARIS_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
    probe_exe(&cmd, "-version")
}

/// 中文(CJK)字体是否就位——deck 截图「最隐蔽必踩」坑: 缺了全是豆腐块 □□□。
/// Linux/Docker 用 fc-list 探测; macOS/Windows 系统自带苹方/雅黑, 视为就位。
fn has_cjk_font() -> Option<bool> {
    if cfg!(target_os = "macos") || cfg!(target_os = "windows") {
        return Some(true); // 系统自带 PingFang / Microsoft YaHei
    }
    // Linux/Docker: fc-list :lang=zh 有输出即有中文字体。
    match Command::new("fc-list").arg(":lang=zh").output() {
        Ok(o) if o.status.success() => Some(!o.stdout.is_empty()),
        _ => None, // fc-list 都没有 → 无法判定(多半也没字体)
    }
}

/// 是否配了 MiniMax key(TTS L0 主力)。best-effort: 查常见 env。
fn minimax_key_present() -> bool {
    ["MINIMAX_API_KEY", "POLARIS_MINIMAX_KEY", "MINIMAXI_API_KEY"]
        .iter()
        .any(|k| std::env::var(k).map(|v| !v.is_empty()).unwrap_or(false))
}

/// 渲染能力 preflight 总入口。返回平台 + 各能力的「就绪/将走哪条路/缺啥降到哪」。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn forge_preflight() -> Value {
    let plat = platform();
    let chromium = find_chromium();
    let ffmpeg = find_ffmpeg();
    let cjk = has_cjk_font();
    let minimax = minimax_key_present();

    // ── 截图能力(PPT/视频取帧的前提)──
    let screenshot = match plat {
        "docker" | "linux" => json!({
            "primary": "chromium CDP",
            "ready": chromium.is_some(),
            "path": chromium,
            "degrades_to": "HTML 交付 + 提示浏览器打印(Ctrl/Cmd+P)",
            "blocker": if chromium.is_none() { Some("未发现 chromium：full 镜像才装(POLARIS_RENDER=1)") } else { None }
        }),
        // 诚实申报:WebView2/WKWebView 截图后端尚未实现,实际只会调 Edge/Chrome/chromium
        // headless CLI —— ready 必须以真探测为准,否则「preflight 绿灯、运行时报错」。
        "windows" => json!({
            "primary": "Edge/Chrome headless CLI",
            "ready": chromium.is_some(),
            "cdp_available": chromium.is_some(),
            "path": chromium,
            "degrades_to": "HTML 交付 + 打印",
            "blocker": if chromium.is_none() { Some("未发现 Edge/Chrome：请安装其一(Windows 一般自带 Edge)") } else { None }
        }),
        "macos" => json!({
            "primary": "Chrome/Chromium headless CLI",
            "ready": chromium.is_some(),
            "cdp_available": chromium.is_some(),
            "path": chromium,
            "degrades_to": "HTML 交付 + 打印",
            "blocker": if chromium.is_none() { Some("未发现 Chrome/Chromium：请安装 Google Chrome(mac 无预装兜底)") } else { None }
        }),
        _ => json!({ "ready": false }),
    };

    // ── 视频编码能力 ──
    let video = match plat {
        "docker" | "linux" => json!({
            "primary": "ffmpeg (镜像自带)",
            "ready": ffmpeg,
            "degrades_to": "交付 deck.html+音频段+timeline，换环境续跑出片",
            "blocker": if !ffmpeg { Some("未发现 ffmpeg：full 镜像才装") } else { None }
        }),
        "windows" => json!({
            "primary": "Media Foundation (P2)",
            "fallback": "ffmpeg(若在 PATH)",
            "ffmpeg_available": ffmpeg,
            "ready": true,
            "degrades_to": "交付 deck+音频+timeline，可续跑"
        }),
        "macos" => json!({
            "primary": "VideoToolbox (P4-mac)",
            "fallback": "ffmpeg(若在 PATH)",
            "ffmpeg_available": ffmpeg,
            "degrades_to": "交付 deck+音频+timeline，可续跑"
        }),
        _ => json!({ "ready": false }),
    };

    // ── 配音(TTS)能力阶梯 ──
    let tts = json!({
        "l0_minimax": { "ready": minimax, "note": "主力，需 key/额度" },
        "l1_edge_free": { "ready": plat != "offline", "note": "免费神经语音(edge-tts)，需联网，P5 接入" },
        "l2_offline_piper": { "ready": false, "note": "离线兜底，P5 可选" },
        "l3_system": {
            "ready": plat == "windows" || plat == "macos",
            "note": if plat == "docker" || plat == "linux" {
                "容器无系统语音 → 出视频默认必须 MiniMax key(诚实缺口)"
            } else {
                "系统语音兜底(Win OneCore / mac AVSpeech)"
            }
        },
        "degrades_to": "出无声版 + 字幕硬烧(内容仍可用)"
    });

    // ── CJK 字体闸(Docker 关键)──
    let fonts = json!({
        "cjk_ready": cjk,
        "critical": plat == "docker" || plat == "linux",
        "note": match cjk {
            Some(true) => "中文字体就位",
            Some(false) => "⚠ 无中文字体：deck 截图会出豆腐块 □□□，应拒跑而非产废片(装 fonts-noto-cjk)",
            None => "无法探测(fc-list 缺失)，多半也无中文字体"
        }
    });

    // ── 整体可出片判定 ──
    // deck.html 截图路(网页PPT导出/视频取帧)依赖 chromium+CJK 字体;
    // 原生 spec→pptx 路(传统PPT,路线 B)纯 Rust 零外部依赖 → 三平台恒可出 PPT,
    // slim 镜像不再因缺 chromium 而 can_render_ppt:false。
    let can_render_deck_ppt = match plat {
        "docker" | "linux" => chromium.is_some() && cjk == Some(true),
        // win/mac 同样依赖真实存在的浏览器(win 一般自带 Edge;mac 没装 Chrome 就是不能)。
        _ => chromium.is_some(),
    };
    let can_render_ppt = true; // 原生路兜底(spec→OOXML,无浏览器也出真可编辑 PPT)
    let can_render_video = can_render_deck_ppt && (ffmpeg || plat == "windows" || plat == "macos");

    json!({
        "ok": true,
        "platform": plat,
        "render_flavor": std::env::var("POLARIS_RENDER_FLAVOR").ok(),
        "forge_engine": "planned (P0–P5 路线图，本 preflight 是第一块落地件)",
        "capabilities": {
            "screenshot": screenshot,
            "video": video,
            "tts": tts,
            "fonts": fonts,
            "pptx_pack": { "ready": true, "note": "纯 Rust OOXML，平台无关(引擎 P1 落地)" },
            "pptx_native": { "ready": true, "note": "spec JSON → 原生可编辑 PPT(路线 B)，零浏览器，slim/CLI 可用" },
            "animation_fx": { "ready": true, "note": "Web 标准 __fx.seek，三平台一致(引擎 P3 落地)" }
        },
        "summary": {
            "can_render_ppt": can_render_ppt,
            "can_render_deck_ppt": can_render_deck_ppt,
            "can_render_video": can_render_video,
            "blockers": preflight_blockers(plat, &chromium, ffmpeg, cjk)
        }
    })
}

// ───────────── Forge 渲染命令(跨平台:win/mac/docker 同一份) ─────────────

/// 把一组幻灯图打成 .pptx 的同步内核。server 命令路由(本就在阻塞线程池里)直调这里。
pub fn build_pptx_sync(images: Vec<String>, out: String) -> Result<Value, String> {
    crate::forge::pptx::build_pptx(&images, &out)
}

/// 把一组幻灯图打成 .pptx(纯 Rust OOXML,替 pptxgenjs)。三平台字节级一致。
/// async + spawn_blocking:多图打包要时间,同步命令默认跑主线程会冻 UI → 丢进阻塞线程池。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn forge_build_pptx(images: Vec<String>, out: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || build_pptx_sync(images, out))
        .await
        .map_err(|e| format!("打包任务异常退出: {e}"))?
}

/// deck.html → 多页 .pptx 的同步内核。server 命令路由(本就在阻塞线程池里)直调这里。
pub fn deck_to_pptx_sync(
    deck: String,
    out: String,
    width: Option<u32>,
    height: Option<u32>,
    searchable: Option<bool>,
    slides: Option<usize>,
) -> Result<Value, String> {
    crate::forge::pptx::render_deck_to_pptx(
        &deck,
        &out,
        width.unwrap_or(1920),
        height.unwrap_or(1080),
        searchable.unwrap_or(true), // 默认开隐形文本层(可搜索 PPT=差异化卖点)
        slides,
    )
}

/// deck.html → 多页 .pptx 一步到位(逐页截图 + 纯 Rust 打包)。三平台同一份。
/// async + spawn_blocking: 成品编辑器「更新 PPT」从 UI 直接调用, 逐页截图+打包要
/// 几十秒, 同步命令默认跑主线程会把整个窗口冻住 → 丢进阻塞线程池。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn forge_deck_to_pptx(
    deck: String,
    out: String,
    width: Option<u32>,
    height: Option<u32>,
    searchable: Option<bool>,
    slides: Option<usize>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        deck_to_pptx_sync(deck, out, width, height, searchable, slides)
    })
    .await
    .map_err(|e| format!("导出任务异常退出: {e}"))?
}

/// spec JSON → 原生可编辑 .pptx 的同步内核(路线 B「传统PPT」)。零截图零浏览器,
/// Docker slim / mac / win 三平台恒可用。spec 既可传 JSON 字符串也可传 .json 文件路径。
pub fn spec_to_pptx_sync(spec: String, out: String) -> Result<Value, String> {
    // BOM(U+FEFF)不算 whitespace,带 BOM 的 JSON 会被误判成文件路径 → 先剥掉再判。
    let json = if spec
        .trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with('{')
    {
        spec
    } else {
        std::fs::read_to_string(&spec).map_err(|e| format!("读 spec 文件 {spec} 失败: {e}"))?
    };
    crate::forge::pptx_native::build_pptx_from_spec(json.trim_start_matches('\u{feff}'), &out)
}

/// spec JSON → 原生可编辑 .pptx。async + spawn_blocking 防大 spec 冻 UI(与 deck 导出同策略)。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn forge_spec_to_pptx(spec: String, out: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || spec_to_pptx_sync(spec, out))
        .await
        .map_err(|e| format!("生成任务异常退出: {e}"))?
}

/// deck.html → .mp4 的同步内核。server 命令路由(本就在阻塞线程池里)直调这里。
#[allow(clippy::too_many_arguments)]
pub fn deck_to_video_sync(
    deck: String,
    out: String,
    seconds_per_slide: Option<f64>,
    fps: Option<u32>,
    width: Option<u32>,
    height: Option<u32>,
    slides: Option<usize>,
    audio: Option<String>,
    narration: Option<String>,
    transition: Option<f64>,
    motion: Option<bool>,
) -> Result<Value, String> {
    crate::forge::video::render_deck_to_video(
        &deck,
        &out,
        seconds_per_slide.unwrap_or(3.0),
        fps.unwrap_or(30),
        width.unwrap_or(1920),
        height.unwrap_or(1080),
        slides,
        audio,
        narration,
        transition,
        motion.unwrap_or(false),
    )
}

/// deck.html → .mp4(逐页截图 + ffmpeg 编码)。配音:audio=现成音频 / narration=文本走 TTS / 都无=无声。
/// async + spawn_blocking:逐页截图+ffmpeg 编码要几十秒到几分钟,同步命令默认跑主线程会冻 UI → 丢进阻塞线程池。
#[cfg(feature = "desktop")]
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn forge_deck_to_video(
    deck: String,
    out: String,
    seconds_per_slide: Option<f64>,
    fps: Option<u32>,
    width: Option<u32>,
    height: Option<u32>,
    slides: Option<usize>,
    audio: Option<String>,
    narration: Option<String>,
    transition: Option<f64>,
    motion: Option<bool>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        deck_to_video_sync(
            deck,
            out,
            seconds_per_slide,
            fps,
            width,
            height,
            slides,
            audio,
            narration,
            transition,
            motion,
        )
    })
    .await
    .map_err(|e| format!("出片任务异常退出: {e}"))?
}

/// deck 某页 CSS 动画 → 逐帧真动画视频的同步内核。server 命令路由(本就在阻塞线程池里)直调这里。
pub fn deck_fx_video_sync(
    deck: String,
    out: String,
    fps: Option<u32>,
    duration_ms: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
    slide: Option<usize>,
) -> Result<Value, String> {
    crate::forge::video::render_deck_fx_video(
        &deck,
        &out,
        fps.unwrap_or(15),
        duration_ms.unwrap_or(2000),
        width.unwrap_or(1280),
        height.unwrap_or(720),
        slide.unwrap_or(1),
    )
}

/// deck 某页 CSS 动画 → 逐帧真动画视频(__fx.seek + chromium 逐帧截图 + ffmpeg,无需 chromiumoxide)。
/// async + spawn_blocking:逐帧截图(可达数百帧)+ffmpeg 编码耗时,同步命令默认跑主线程会冻 UI → 丢进阻塞线程池。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn forge_deck_fx_video(
    deck: String,
    out: String,
    fps: Option<u32>,
    duration_ms: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
    slide: Option<usize>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        deck_fx_video_sync(deck, out, fps, duration_ms, width, height, slide)
    })
    .await
    .map_err(|e| format!("逐帧出片任务异常退出: {e}"))?
}

/// 文本 → mp3 配音的同步内核。server 命令路由(本就在阻塞线程池里)直调这里。
pub fn forge_tts_sync(
    text: String,
    out: String,
    voice: Option<String>,
    language_boost: Option<String>,
) -> Result<Value, String> {
    crate::forge::tts::synth(&text, &out, voice.as_deref(), language_boost.as_deref())
}

/// 文本 → mp3 配音(MiniMax T2A,纯 Rust)。无 key 时返回明确错误。
/// async + spawn_blocking:T2A 网络往返(长文本切块多次调用)耗时,同步命令默认跑主线程会冻 UI → 丢进阻塞线程池。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn forge_tts(
    text: String,
    out: String,
    voice: Option<String>,
    language_boost: Option<String>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || forge_tts_sync(text, out, voice, language_boost))
        .await
        .map_err(|e| format!("配音任务异常退出: {e}"))?
}

/// 用 chromium/chrome headless 给 URL/本地 HTML 截图(Forge capture 原始能力)。
///
/// `command(async)`:内部 run_with_timeout 最长 90s,同步命令默认跑主线程会把 UI 钉死
/// 90 秒。本命令被 apihub dispatch 以同名符号**同步直调**(双壳共用),不能像兄弟命令那样
/// 拆成 async fn + spawn_blocking(会破坏 dispatch 调用点),改用 tauri 的 command(async)
/// 让前端 invoke 走独立线程 —— fn 本体保持同步签名,apihub 直调零改动。
#[cfg_attr(feature = "desktop", tauri::command(async))]
pub fn forge_screenshot(
    url: String,
    out: String,
    width: Option<u32>,
    height: Option<u32>,
    scale: Option<u32>,
) -> Result<Value, String> {
    crate::forge::pptx::screenshot(
        &url,
        &out,
        width.unwrap_or(1920),
        height.unwrap_or(1080),
        scale.unwrap_or(2), // 默认 2x 高清
    )
}

/// 带超时地跑外部命令并**捕获 stdout**(run_with_timeout 丢弃 stdout;`--dump-dom` 这类
/// 要读输出的场景走本函数,否则裸 `output()` 挂死会让整个导出永不返回)。
///
/// 看门思路与 polaris-runtime procs.rs 的 run_with_timeout 同款:try_wait 前密后疏轮询 +
/// 超时杀整棵进程树(unix 下 spawn 前 process_group(0) 置成组长,kill_tree 的 `kill -pid`
/// 即 killpg 带走 chromium 扇出的子孙;windows 走 taskkill /T /F —— 均复用
/// `crate::runtime::procs::kill_tree`,不重造杀树轮子)。
/// stdout 由后台线程边读边收:既持续排空管道防子进程写满互相死锁,又设 64MB 上限防
/// 畸形页面读爆内存(超限后仍继续排空只是不再存,让子进程能正常退出)。
pub fn run_capture_stdout(mut cmd: Command, secs: u64, what: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    const MAX_STDOUT: usize = 64 * 1024 * 1024;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0); // 成组长 → 超时 kill_tree 的 killpg 能带走整组子孙
    }
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("{what} 启动失败: {e}"))?;
    let pid = child.id();
    let Some(mut so) = child.stdout.take() else {
        let _ = child.kill();
        return Err(format!("{what} 取 stdout 管道失败"));
    };
    // 后台线程排空 stdout,读完经 channel 交回(主线程只轮询 try_wait,绝不阻塞在管道上)。
    let (tx, rx) = mpsc::channel::<Result<Vec<u8>, String>>();
    let what_owned = what.to_string();
    std::thread::spawn(move || {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        let mut overflow = false;
        loop {
            match so.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if overflow {
                        continue; // 超限后仍持续排空,只是不再存
                    }
                    if buf.len() + n > MAX_STDOUT {
                        overflow = true;
                        buf = Vec::new(); // 已注定报错,提前释放
                        continue;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                // 管道被杀进程强关等 → 按已收内容处理(失败与否由退出码路径裁决)
                Err(_) => break,
            }
        }
        let _ = tx.send(if overflow {
            Err(format!(
                "{what_owned} stdout 超过 {}MB 上限",
                MAX_STDOUT / 1024 / 1024
            ))
        } else {
            Ok(buf)
        });
    });
    // 轮询间隔前密后疏(10ms 起步、封顶 120ms),与 procs.rs 同款:短命令快速察觉退出。
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut poll_ms: u64 = 10;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if Instant::now() >= deadline {
                    crate::runtime::procs::kill_tree(pid);
                    let _ = child.kill(); // 兜底
                    let _ = child.wait(); // 收尸,管道随之关闭,reader 线程自行结束
                    return Err(format!("{what} 超时({secs}s)被终止"));
                }
                std::thread::sleep(Duration::from_millis(poll_ms));
                poll_ms = (poll_ms * 2).min(120);
            }
            Err(e) => {
                crate::runtime::procs::kill_tree(pid);
                let _ = child.kill();
                return Err(format!("{what} 等待失败: {e}"));
            }
        }
    };
    if !status.success() {
        return Err(format!("{what} 失败(退出码 {:?})", status.code()));
    }
    // 进程已正常退出,stdout 写端理应关闭;若有残余子进程仍持管道则限时兜底,
    // 拿不到就杀树报错 —— 绝不无限等(本函数存在的意义)。
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(r) => r,
        Err(_) => {
            crate::runtime::procs::kill_tree(pid);
            Err(format!("{what} stdout 排空超时(疑似残余子进程持管道)"))
        }
    }
}

/// 汇总当前环境出片的拦路项(给 UI 红灯直接展示)。
fn preflight_blockers(
    plat: &str,
    chromium: &Option<String>,
    ffmpeg: bool,
    cjk: Option<bool>,
) -> Vec<String> {
    let mut b = Vec::new();
    if (plat == "docker" || plat == "linux") && chromium.is_none() {
        b.push("缺 chromium：用 full 镜像(--build-arg POLARIS_RENDER=1)".to_string());
    }
    if (plat == "docker" || plat == "linux") && cjk != Some(true) {
        b.push("缺中文字体：装 fonts-noto-cjk，否则截图豆腐块".to_string());
    }
    if (plat == "docker" || plat == "linux") && !ffmpeg {
        b.push("缺 ffmpeg：出视频需 full 镜像".to_string());
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    // 注: run_with_timeout 的超时/杀树/stderr 测试随函数迁入 polaris-runtime(procs.rs)。

    #[test]
    fn bundled_exe_safe_when_absent() {
        // 测试环境 exe 旁没有自带 chromium/ffmpeg → 返回 None,不 panic(零安装打包发现逻辑)。
        assert!(bundled_exe(&["definitely-not-bundled-xyz"]).is_none());
    }
}
