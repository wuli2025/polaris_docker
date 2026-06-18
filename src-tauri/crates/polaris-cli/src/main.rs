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

  polaris-forge fable status
      检索枢纽(寓言计划)状态:盘点文件数/向量 chunk 数/嵌入服务商。

  polaris-forge fable inventory --root=<目录>
      多核并行全盘盘点 → ~/Polaris/data/fable.db(L1a,首小时全盘可搜)。

  polaris-forge fable index [--max-chunks=4000]
      构建(或继续)向量索引:文本 chunk → 嵌入(感官坞服务商)→ 落库;幂等续跑。

  polaris-forge fable search --q=<查询> [--top=12] [--mode=hybrid|grep|vector] [--scope=wiki|!wiki]
      塌平混检:认字腿(FTS5 倒排,不漏文件)∥ 认意思腿(向量两段式 ANN)并行
      → 文件级 RRF 融合 → 闸门重排,JSON 命中。

  polaris-forge fable eval [--set=<考卷.json>] [--top=12] [--mode=hybrid] [--init]
      跑评测集(考卷)→ recall@k + MRR,把「准不准」变成数字;--init 先写一份样例。

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
        // 寓言计划 · 检索枢纽:agent 的全盘检索 shell 工具(grep ∥ RAG 混检)
        "fable" => {
            // 嵌入/重排要读感官坞配置(~/Polaris/data/sense.json)
            app::sense::init();
            let sub = args.first().map(|s| s.as_str()).unwrap_or("");
            let rest = if args.is_empty() { args } else { &args[1..] };
            match sub {
                "status" => serde_json::to_value(app::fable::status()?).map_err(|e| e.to_string()),
                "inventory" => {
                    let root = req(rest, "root")?;
                    let exclude = std::collections::HashSet::new();
                    // CLI 一次性盘点 → 默认完整(每目录都 read_dir;顺带建立目录缓存供桌面端后续增量)。
                    let incremental = flag(rest, "incremental").is_some();
                    let summary = app::fable::inventory::scan_root(
                        &root,
                        &exclude,
                        !incremental,
                        &|files, bytes| {
                            eprintln!("[fable] 已盘点 {files} 个文件 / {:.1} GB", bytes as f64 / 1e9);
                        },
                    )?;
                    serde_json::to_value(summary).map_err(|e| e.to_string())
                }
                "index" => {
                    let budget = flag(rest, "max-chunks")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(4000usize);
                    let summary = app::fable::index::build_index(budget, &|files, chunks, cur| {
                        eprintln!("[fable] 文件 {files} / chunk {chunks} — {cur}");
                    })?;
                    serde_json::to_value(summary).map_err(|e| e.to_string())
                }
                "search" => {
                    let q = req(rest, "q")?;
                    let top = flag(rest, "top").and_then(|v| v.parse().ok()).unwrap_or(12);
                    let mode = flag(rest, "mode").unwrap_or_else(|| "hybrid".into());
                    let scope = flag(rest, "scope");
                    serde_json::to_value(app::fable::retrieve::search(
                        &q,
                        top,
                        &mode,
                        scope.as_deref(),
                    )?)
                    .map_err(|e| e.to_string())
                }
                "optimize" => {
                    // 重建向量 IVF 倒排单元(20TB 级 ANN「建索引」;大批入库后/巡夜跑)。
                    serde_json::to_value(app::fable::index::optimize_vectors()?)
                        .map_err(|e| e.to_string())
                }
                "eval" => {
                    // --init 先写一份评测集样例;否则跑考卷出 recall@k + MRR。
                    if has(rest, "init") {
                        let p = app::fable::eval::write_template(flag(rest, "set"))?;
                        serde_json::to_value(serde_json::json!({ "template": p }))
                            .map_err(|e| e.to_string())
                    } else {
                        let top = flag(rest, "top").and_then(|v| v.parse().ok()).unwrap_or(12);
                        let mode = flag(rest, "mode").unwrap_or_else(|| "hybrid".into());
                        serde_json::to_value(app::fable::eval::run_eval(
                            flag(rest, "set"),
                            top,
                            &mode,
                        )?)
                        .map_err(|e| e.to_string())
                    }
                }
                "backfill-lang" => {
                    // 给所有文件补「语言」归类标签(代码/媒体零 IO;文稿读头嗅探中文/英文)。
                    let n = app::fable::inventory::fable_backfill_lang()?;
                    serde_json::to_value(serde_json::json!({ "backfilled": n }))
                        .map_err(|e| e.to_string())
                }
                "overview" => {
                    // 文件中心总览(含 by_lang 按语言分布),验「按语言归类」用。
                    serde_json::to_value(app::fable::files::overview(flag(rest, "root"))?)
                        .map_err(|e| e.to_string())
                }
                other => Err(format!("未知 fable 子命令 {other}(--help 看用法)")),
            }
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
