//! polaris-forge — Polaris Forge 渲染引擎 CLI。
//!
//! 设计给三类调用方:
//! 1. **agent(claude CLI)**:对话里模型直接跑 `polaris-forge spec-pptx …` 出可编辑 PPT,
//!    非生图模型只需写 JSON——「AI 出决策,代码执行」。
//! 2. **Docker/NAS**:镜像内置本 CLI,slim 镜像(无 chromium)也能出原生 PPT。
//! 3. **脚本/CI**:所有输出一律 JSON(stdout),失败 JSON 到 stderr + 非零退出码。
//!
//! 与桌面端/Server 共用同一份引擎源码(polaris_app_lib),行为字节级一致。

use polaris_app_lib as app;
use serde_json::{json, Value};

const HELP: &str = r#"polaris-forge — Polaris Forge 渲染引擎 CLI

用法:
  polaris-forge preflight
      探测本机渲染能力(chromium/ffmpeg/中文字体/TTS key),报「能出什么、缺啥降级」。

  polaris-forge spec-pptx --spec=<polaris.slides.json|JSON字符串> --out=<out.pptx>
      结构化 spec → 原生 100% 可编辑 .pptx(真文本框/形状/项目符号,零浏览器依赖)。

  polaris-forge pptx --deck=<deck.html> --out=<out.pptx> [--width=1920] [--height=1080]
                     [--slides=N] [--no-text]
      deck.html → .pptx 分层导出:无字背景截图 + 可见文本框(可编辑);--no-text 纯图。

  polaris-forge shot --url=<URL|文件> --out=<out.png> [--width=1280] [--height=720] [--scale=1]
      网页/本地 HTML 截图(chromium headless)。

  polaris-forge pack --out=<out.pptx> <img1.png> <img2.png> …
      现成图片序列打成 .pptx(每页一张全幅图)。

  polaris-forge video --deck=<deck.html> --out=<out.mp4> [--sps=3.0] [--fps=30]
                      [--width=1920] [--height=1080] [--slides=N] [--audio=<mp3>]
                      [--narration=<文本>] [--transition=0.5] [--motion]
      deck.html → .mp4(逐页截图 + ffmpeg)。

  polaris-forge tts --text=<文本> --out=<out.mp3> [--voice=<音色>] [--lang-boost=<语种>]
      文本配音(MiniMax 主力,macOS 离线 say 兜底)。

  polaris-forge validate --pptx=<file.pptx>
      校验 .pptx 包结构(自写最小 OOXML 校验器)。

约定:成功 → JSON 到 stdout,退出码 0;失败 → {"ok":false,"error":…} 到 stderr,退出码 1。
"#;

fn flag(args: &[String], name: &str) -> Option<String> {
    let eq = format!("--{name}=");
    for (i, a) in args.iter().enumerate() {
        if let Some(v) = a.strip_prefix(&eq) {
            return Some(v.to_string());
        }
        if a == &format!("--{name}") {
            // --name value 形式(下一个参数不是另一个 flag 才算值)
            if let Some(v) = args.get(i + 1) {
                if !v.starts_with("--") {
                    return Some(v.clone());
                }
            }
        }
    }
    None
}

fn flag_u32(args: &[String], name: &str, default: u32) -> u32 {
    flag(args, name).and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == &format!("--{name}"))
}

fn req(args: &[String], name: &str) -> Result<String, String> {
    flag(args, name).ok_or_else(|| format!("缺少必填参数 --{name}(--help 看用法)"))
}

fn run(cmd: &str, args: &[String]) -> Result<Value, String> {
    match cmd {
        "preflight" => Ok(app::forge::forge_preflight()),
        "spec-pptx" => {
            app::forge::spec_to_pptx_sync(req(args, "spec")?, req(args, "out")?)
        }
        "pptx" => app::forge_pptx::render_deck_to_pptx(
            &req(args, "deck")?,
            &req(args, "out")?,
            flag_u32(args, "width", 1920),
            flag_u32(args, "height", 1080),
            !has(args, "no-text"),
            flag(args, "slides").and_then(|v| v.parse().ok()),
        ),
        "shot" => app::forge_pptx::screenshot(
            &req(args, "url")?,
            &req(args, "out")?,
            flag_u32(args, "width", 1280),
            flag_u32(args, "height", 720),
            flag_u32(args, "scale", 1),
        ),
        "pack" => {
            let out = req(args, "out")?;
            let images: Vec<String> = args
                .iter()
                .filter(|a| !a.starts_with("--") && Some(a.as_str()) != flag(args, "out").as_deref())
                .cloned()
                .collect();
            if images.is_empty() {
                return Err("pack 需要至少一张图片路径".into());
            }
            app::forge_pptx::build_pptx(&images, &out)
        }
        "video" => app::forge_video::render_deck_to_video(
            &req(args, "deck")?,
            &req(args, "out")?,
            flag(args, "sps").and_then(|v| v.parse().ok()).unwrap_or(3.0),
            flag_u32(args, "fps", 30),
            flag_u32(args, "width", 1920),
            flag_u32(args, "height", 1080),
            flag(args, "slides").and_then(|v| v.parse().ok()),
            flag(args, "audio"),
            flag(args, "narration"),
            flag(args, "transition").and_then(|v| v.parse().ok()),
            has(args, "motion"),
        ),
        "tts" => app::forge_tts::synth(
            &req(args, "text")?,
            &req(args, "out")?,
            flag(args, "voice").as_deref(),
            flag(args, "lang-boost").as_deref(),
        ),
        "validate" => {
            let v = app::forge_pptx::validate_pptx(&req(args, "pptx")?)?;
            serde_json::to_value(&v).map_err(|e| e.to_string())
        }
        other => Err(format!("未知子命令 {other}(--help 看用法)")),
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cmd = argv.first().cloned().unwrap_or_default();
    if cmd.is_empty() || cmd == "--help" || cmd == "-h" || cmd == "help" {
        println!("{HELP}");
        std::process::exit(if cmd.is_empty() { 2 } else { 0 });
    }
    let rest = &argv[1..];
    match run(&cmd, rest) {
        Ok(v) => {
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()));
        }
        Err(e) => {
            eprintln!("{}", json!({ "ok": false, "command": cmd, "error": e }));
            std::process::exit(1);
        }
    }
}
