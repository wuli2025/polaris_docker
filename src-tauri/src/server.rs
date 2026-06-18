//! Docker(server) 外壳 —— axum HTTP/WS 服务，替代 Tauri 桌面外壳。
//!
//! - `POST /api/invoke {cmd,args}`：把前端 `invoke()` 分发到各引擎模块函数（≈75 命令）。
//! - `GET  /ws`：把各模块 `app.emit(topic,payload)` 广播给浏览器（替代 Tauri event）。
//! - `POST /api/upload`：multipart 上传，替代桌面原生文件对话框（返回服务端临时路径）。
//! - `GET  /api/file?path=`：受限静态文件读取（iframe 预览 / 图片）。
//! - 其余路径：托管打包好的前端 `dist/`（SPA fallback）。
//!
//! 设计要点：引擎模块（kb/chat/conv/...）源码与桌面版**完全相同**，仅外壳不同。

use crate::host::{AppHandle, Event};
use axum::{
    body::Body,
    extract::{ws::Message, ws::WebSocket, DefaultBodyLimit, Multipart, Query, State, WebSocketUpgrade},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct AppState {
    pub app: AppHandle,
    pub tx: broadcast::Sender<Event>,
    pub auth_token: Arc<Option<String>>,
    pub web_dir: PathBuf,
}

impl AppState {
    fn app(&self) -> AppHandle {
        self.app.clone()
    }
}

/// 入口：初始化各引擎模块 + 起 axum。由 bin/polaris-server.rs 调用。
pub async fn serve() -> anyhow::Result<()> {
    // 广播频道：所有 emit 走这里 → 全部 WS 订阅者。容量给大些，避免流式 token 丢帧。
    let (tx, _rx) = broadcast::channel::<Event>(16384);
    let app = AppHandle::new(tx.clone());

    // 让 spawn 的 claude CLI 的 cwd 落在数据根 ~/Polaris：项目/KB/产物都在其下，
    // claude 自动信任整棵树。桌面版靠 `CARGO_MANIFEST_DIR` 的父级，但那是编译期路径，
    // 容器运行时不存在 → 这里显式把进程工作目录设到数据根，避免 claude 落到 `/`。
    if let Some(u) = directories::UserDirs::new() {
        let data_root = u.home_dir().join("Polaris");
        let _ = std::fs::create_dir_all(&data_root);
        if let Err(e) = std::env::set_current_dir(&data_root) {
            eprintln!("[polaris-server] 设工作目录失败({}): {e}", data_root.display());
        }
    }

    // ── 初始化各模块（与桌面 lib.rs setup 等价，去掉桌面专属部分）──
    if let Err(e) = crate::kb::init(&app) {
        eprintln!("[polaris-server] kb::init 失败: {e}");
    }
    let _ = crate::conv::init(&app);
    let _ = crate::chat::init(&app);
    let _ = crate::claude_md::init(&app);
    let _ = crate::provider::init(&app);
    crate::skills::seed_video_studio_skill();
    crate::skills::seed_deck_studio_skill();
    crate::skills::seed_web_studio_skill();
    crate::skills::seed_wechat_typesetter_skill();
    // 老用户迁移：早期版本首启播种过毛主席资料库的，补装 consult-mao 技能。
    crate::skills::migrate_consult_mao_for_seeded_kb();
    // 飞书网关「开机自动启动」（若用户开了 auto_start 且凭证齐全）。
    crate::feishu::auto_start_if_enabled(&app);
    // 寓言计划:感官 API 坞 + 回声层「每日做梦」调度 + 检索枢纽(与桌面 setup 等价)。
    crate::sense::init();
    crate::voice::init();
    crate::echo::start_scheduler(app.clone());
    crate::fable::init();

    let auth_token = std::env::var("POLARIS_AUTH_TOKEN")
        .ok()
        .filter(|s| !s.is_empty());
    if auth_token.is_some() {
        println!("[polaris-server] 已启用访问口令 (POLARIS_AUTH_TOKEN)");
    } else {
        println!("[polaris-server] ⚠ 未设访问口令，服务对所有可达网络开放");
    }

    let web_dir = std::env::var("POLARIS_WEB_DIR").unwrap_or_else(|_| "/srv/web".to_string());
    let web_dir = PathBuf::from(web_dir);

    let state = AppState {
        app,
        tx,
        auth_token: Arc::new(auth_token),
        web_dir: web_dir.clone(),
    };

    let app_router = Router::new()
        .route("/api/invoke", post(invoke))
        .route("/api/upload", post(upload))
        .route("/api/file", get(serve_file))
        .route("/api/health", get(|| async { "ok" }))
        .route("/api/status", get(status))
        .route("/ws", get(ws_handler))
        .fallback(get(spa_fallback))
        // 上传整体进内存; 不设上限则单个大 body 直接 OOM 服务进程。512MB 足够覆盖
        // 知识库/视频素材, 又挡掉恶意巨包。(/ws 流式不受此限, 上传走 multipart 受限)
        .layer(DefaultBodyLimit::max(512 * 1024 * 1024))
        .with_state(state);

    let port: u16 = std::env::var("POLARIS_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!("[polaris-server] 监听 http://0.0.0.0:{port} (前端目录: {})", web_dir.display());

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app_router).await?;
    Ok(())
}

// ───────────────────────── /api/status 运维水位(R7)─────────────────────────
//
// 给群晖运维/监控用的水位接口: 容器内存(贴近 mem_limit/OOM 风险)、宿主内存、数据盘用量
// (防写满)、claude 配置在位、推理端点(R3)状态。全部 best-effort: 读不到的项返回
// available:false 而非报错, 非 Linux 环境(开发机)也能编译运行。与 /api/health 一样不需口令
// (只暴露粗粒度水位, 不含敏感数据)。

async fn status(State(state): State<AppState>) -> Response {
    let auth_set = state.auth_token.is_some();
    // 含 df 子进程 + 推理端点探测(阻塞/网络), 丢到阻塞线程池, 勿卡 async worker。
    let v = tokio::task::spawn_blocking(move || collect_status(auth_set))
        .await
        .unwrap_or_else(|_| json!({ "ok": false, "error": "status 采集失败" }));
    Json(v).into_response()
}

fn collect_status(auth_set: bool) -> Value {
    let data_root = directories::UserDirs::new()
        .map(|u| u.home_dir().join("Polaris"))
        .unwrap_or_else(|| PathBuf::from("/root/Polaris"));
    json!({
        "ok": true,
        "service": "polaris-server",
        "auth_token_set": auth_set,
        "chat_timeout_secs": std::env::var("POLARIS_CHAT_TIMEOUT_SECS")
            .ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
        "container_memory": cgroup_mem(),
        "host_memory": meminfo_mem(),
        "data_disk": disk_usage(&data_root),
        "claude_config": claude_config_status(),
        "infer": crate::infer::status_json(),
        "forge": crate::forge::forge_preflight(),
    })
}

fn pct(used: u64, total: u64) -> Option<f64> {
    if total == 0 {
        None
    } else {
        Some(((used as f64 / total as f64) * 1000.0).round() / 10.0)
    }
}

/// cgroup v2 容器内存(比宿主内存更贴近 mem_limit / OOM 风险)。
fn cgroup_mem() -> Value {
    let used = std::fs::read_to_string("/sys/fs/cgroup/memory.current")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok());
    // memory.max 为 "max" 表示未设上限 → parse 失败即视为无上限。
    let limit = std::fs::read_to_string("/sys/fs/cgroup/memory.max")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok());
    match (used, limit) {
        (Some(u), Some(l)) => json!({ "used_bytes": u, "limit_bytes": l, "used_pct": pct(u, l) }),
        (Some(u), None) => json!({
            "used_bytes": u, "limit_bytes": null, "used_pct": null,
            "note": "未设容器内存上限(memory.max=max)，建议设 mem_limit 防泄漏拖垮整机"
        }),
        _ => json!({ "available": false, "note": "非 cgroup v2 环境或无权读取" }),
    }
}

/// 宿主可用内存(/proc/meminfo)。
fn meminfo_mem() -> Value {
    let Ok(txt) = std::fs::read_to_string("/proc/meminfo") else {
        return json!({ "available": false });
    };
    let kb_to_bytes = |line: &str, key: &str| -> Option<u64> {
        line.strip_prefix(key)
            .and_then(|r| r.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
            .map(|k| k * 1024)
    };
    let mut total = None;
    let mut avail = None;
    for line in txt.lines() {
        if total.is_none() {
            if let Some(b) = kb_to_bytes(line, "MemTotal:") {
                total = Some(b);
            }
        }
        if avail.is_none() {
            if let Some(b) = kb_to_bytes(line, "MemAvailable:") {
                avail = Some(b);
            }
        }
    }
    match (total, avail) {
        (Some(t), Some(a)) => json!({
            "total_bytes": t, "available_bytes": a, "used_pct": pct(t.saturating_sub(a), t)
        }),
        _ => json!({ "available": false }),
    }
}

/// 数据盘用量(df -kP <path>)。防「容器写满 /volume1 卷拖垮 DSM」的水位来源。
fn disk_usage(path: &Path) -> Value {
    let Ok(out) = std::process::Command::new("df").arg("-kP").arg(path).output() else {
        return json!({ "available": false, "note": "df 不可用" });
    };
    if !out.status.success() {
        return json!({ "available": false });
    }
    let txt = String::from_utf8_lossy(&out.stdout);
    if let Some(line) = txt.lines().nth(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() >= 4 {
            let total = f[1].parse::<u64>().ok().map(|k| k * 1024);
            let used = f[2].parse::<u64>().ok().map(|k| k * 1024);
            let avail = f[3].parse::<u64>().ok().map(|k| k * 1024);
            if let (Some(t), Some(u), Some(a)) = (total, used, avail) {
                return json!({
                    "path": path.to_string_lossy(),
                    "total_bytes": t, "used_bytes": u, "available_bytes": a,
                    "used_pct": pct(u, t)
                });
            }
        }
    }
    json!({ "available": false })
}

/// claude 全局配置文件在位检测(印证 CLAUDE_CONFIG_DIR 落卷修复)。
fn claude_config_status() -> Value {
    let (dir, cfg) = match std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .filter(|s| !s.is_empty())
    {
        // 设了 CONFIG_DIR → .claude.json 落在该目录内。
        Some(d) => {
            let p = Path::new(&d).join(".claude.json");
            (d, p)
        }
        // 未设 → 默认在 HOME 根。
        None => {
            let home = directories::UserDirs::new()
                .map(|u| u.home_dir().to_path_buf())
                .unwrap_or_else(|| PathBuf::from("/root"));
            (home.to_string_lossy().to_string(), home.join(".claude.json"))
        }
    };
    json!({ "config_dir": dir, "config_file_present": cfg.is_file() })
}

// ───────────────────────── 鉴权 ─────────────────────────

fn check_auth(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(expected) = state.auth_token.as_ref() else {
        return true; // 未设口令 → 放行
    };
    let got = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.strip_prefix("Bearer ").unwrap_or(s).to_string())
        .or_else(|| {
            headers
                .get("x-polaris-token")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        });
    got.as_deref() == Some(expected.as_str())
}

// ───────────────────────── /api/invoke 分发 ─────────────────────────

#[derive(serde::Deserialize)]
struct InvokeReq {
    cmd: String,
    #[serde(default)]
    args: Value,
}

async fn invoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<InvokeReq>,
) -> Response {
    if !check_auth(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"未授权 (口令错误)"}))).into_response();
    }
    let cmd = req.cmd;
    let args = req.args;
    let app = state.app();

    // chat_send 是 async（其余皆 sync）。单独处理。
    if cmd == "chat_send" {
        let inner = args.get("args").cloned().unwrap_or(Value::Null);
        let parsed: Result<crate::chat::ChatSendArgs, _> = serde_json::from_value(inner);
        return match parsed {
            Ok(a) => match crate::chat::chat_send(app, a).await {
                Ok(req_id) => Json(json!(req_id)).into_response(),
                Err(e) => err_resp(e),
            },
            Err(e) => err_resp(format!("chat_send 参数解析失败: {e}")),
        };
    }

    // 其余命令同步执行，丢到阻塞线程池（内含 ureq 网络/文件 IO，勿阻塞 async worker）。
    let out = tokio::task::spawn_blocking(move || dispatch_sync(&cmd, &args, app)).await;
    match out {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err_resp(e),
        Err(e) => err_resp(format!("内部任务失败: {e}")),
    }
}

fn err_resp(e: String) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e }))).into_response()
}

fn ok<T: Serialize>(t: T) -> Result<Value, String> {
    serde_json::to_value(t).map_err(|e| e.to_string())
}

// 参数提取器（前端 invoke 走 camelCase 键）
fn req_str(a: &Value, k: &str) -> Result<String, String> {
    a.get(k)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("缺少字符串参数 `{k}`"))
}
fn opt_str(a: &Value, k: &str) -> Option<String> {
    a.get(k).and_then(|v| {
        if v.is_null() {
            None
        } else {
            v.as_str().map(|s| s.to_string())
        }
    })
}
fn opt_usize(a: &Value, k: &str) -> Option<usize> {
    a.get(k).and_then(|v| v.as_u64()).map(|n| n as usize)
}
fn opt_bool(a: &Value, k: &str) -> Option<bool> {
    a.get(k).and_then(|v| v.as_bool())
}
fn opt_f64(a: &Value, k: &str) -> Option<f64> {
    a.get(k).and_then(|v| v.as_f64())
}
fn opt_u8(a: &Value, k: &str) -> Option<u8> {
    a.get(k).and_then(|v| v.as_u64()).map(|n| n.min(255) as u8)
}
fn bool_def(a: &Value, k: &str, d: bool) -> bool {
    a.get(k).and_then(|v| v.as_bool()).unwrap_or(d)
}
fn vec_str(a: &Value, k: &str) -> Vec<String> {
    a.get(k)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn dispatch_sync(cmd: &str, a: &Value, app: AppHandle) -> Result<Value, String> {
    use crate::*;
    match cmd {
        // ── KB ──
        "kb_root" => ok(kb::kb_root()),
        "kb_default_root" => ok(kb::kb_default_root()),
        "kb_set_root" => ok(kb::kb_set_root(req_str(a, "newPath")?)?),
        "kb_scan" => ok(kb::kb_scan()?),
        "kb_compile" => ok(kb::kb_compile(app)?),
        "kb_list" => ok(kb::kb_list(opt_str(a, "subdir"))),
        "kb_read" => ok(kb::kb_read(req_str(a, "relPath")?)?),
        "kb_delete" => ok(kb::kb_delete(req_str(a, "relPath")?)?),
        "kb_clear" => ok(kb::kb_clear()?),
        "kb_search" => ok(kb::kb_search(req_str(a, "query")?, opt_usize(a, "topK"))),
        "kb_ingest" => ok(kb::kb_ingest(req_str(a, "sourcePath")?)?),
        "kb_upload_files" => ok(kb::kb_upload_files(vec_str(a, "paths"))),
        "kb_convert_batch" => ok(kb::kb_convert_batch(vec_str(a, "paths"))?),
        "kb_graph" => ok(kb::kb_graph()),
        "kb_lint" => ok(kb::kb_lint()),
        "kb_enrich_links" => ok(kb::kb_enrich_links(app)?),
        "kb_dedup" => ok(kb::kb_dedup(app)?),
        "kb_pack_list" => ok(kb::kb_pack_list()),
        "kb_pack_install" => ok(kb::kb_pack_install(app, req_str(a, "id")?)?),
        "kb_pack_remove" => ok(kb::kb_pack_remove(req_str(a, "id")?)?),

        // ── 全盘资源归集 ──
        "scan_roots" => ok(scan::scan_roots()),
        "scan_resources" => ok(scan::scan_resources(vec_str(a, "roots"), opt_usize(a, "max"))?),

        // ── 寓言计划 · 感官 API 坞 ──
        "sense_list" => ok(sense::sense_list()),
        "sense_set" => ok(sense::sense_set(
            req_str(a, "id")?,
            opt_str(a, "apiKey"),
            opt_str(a, "baseUrl"),
            opt_bool(a, "enabled"),
            opt_str(a, "defaultModel"),
        )?),
        "sense_switches_set" => ok(sense::sense_switches_set(
            opt_bool(a, "cloudEnabled"),
            opt_bool(a, "audioEgress"),
            opt_bool(a, "imageEgress"),
            opt_f64(a, "budgetMonthlyCny"),
        )?),
        "sense_test" => ok(sense::sense_test(req_str(a, "id")?)?),
        "sense_pack_install" => ok(sense::sense_pack_install(app, req_str(a, "id")?)?),
        "sense_pack_remove" => ok(sense::sense_pack_remove(req_str(a, "id")?)?),

        // ── 语音输入「极速说」· 防污染 + 配置 + 个人词表 ──
        "voice_config_get" => ok(voice::voice_config_get()),
        "voice_config_set" => ok(voice::voice_config_set(
            opt_str(a, "activation"),
            opt_str(a, "hotkey"),
            opt_str(a, "engine"),
            opt_bool(a, "fluentMode"),
            opt_bool(a, "polish"),
            opt_str(a, "antipollute"),
            a.get("pinyinThreshold").and_then(|v| v.as_u64()).map(|n| n as u32),
            opt_str(a, "overlayPos"),
        )?),
        "voice_lexicon_get" => ok(voice::voice_lexicon_get()),
        "voice_hotword_add" => ok(voice::voice_hotword_add(req_str(a, "word")?)?),
        "voice_hotword_remove" => ok(voice::voice_hotword_remove(req_str(a, "word")?)?),
        "voice_correction_add" => {
            ok(voice::voice_correction_add(req_str(a, "wrong")?, req_str(a, "right")?)?)
        }
        "voice_correction_remove" => ok(voice::voice_correction_remove(req_str(a, "wrong")?)?),
        "voice_anti_pollute" => ok(voice::voice_anti_pollute(req_str(a, "text")?)),
        "voice_transcribe_file" => ok(voice::voice_transcribe_file(req_str(a, "path")?)?),
        "voice_listen_start" => ok(voice::voice_listen_start(app)?),
        "voice_listen_stop" => ok(voice::voice_listen_stop()?),
        "voice_dictate_start" => ok(voice::voice_dictate_start(app)?),
        "voice_dictate_stop" => ok(voice::voice_dictate_stop()?),
        "voice_learn_correction" => {
            ok(voice::voice_learn_correction(req_str(a, "wrong")?, req_str(a, "right")?)?)
        }
        "voice_lexicon_learn" => {
            ok(voice::voice_lexicon_learn(req_str(a, "text")?, opt_usize(a, "top"))?)
        }

        // ── 寓言计划 · 回声层(对话沉淀/做梦)──
        "conv_archive_conversation" => ok(conv::conv_archive_conversation(
            req_str(a, "id")?,
            bool_def(a, "archived", true),
        )?),
        "echo_status" => ok(echo::echo_status()),
        "echo_set" => ok(echo::echo_set(
            opt_bool(a, "enabled"),
            opt_u8(a, "hour"),
            opt_bool(a, "runOnBoot"),
        )),
        "echo_dream_now" => ok(echo::echo_dream_now(app)?),
        "echo_distill_conversation" => {
            ok(echo::echo_distill_conversation(app, req_str(a, "convId")?)?)
        }
        "echo_briefing_today" => ok(echo::echo_briefing_today()),
        "echo_briefing_dismiss" => ok(echo::echo_briefing_dismiss(req_str(a, "id")?)),
        "echo_briefing_run" => ok(echo::echo_briefing_run(app)?),
        "kb_overview_get" => ok(kb::kb_overview_get()),

        // ── 寓言计划 · 检索枢纽(盘点 L1a + 向量索引 + 塌平混检)──
        "fable_status" => ok(fable::fable_status()?),
        "fable_cancel" => ok(fable::fable_cancel()),
        "fable_inventory_start" => ok(fable::inventory::fable_inventory_start(
            app,
            Some(vec_str(a, "roots")),
            Some(vec_str(a, "exclude")),
            a.get("full").and_then(|v| v.as_bool()),
        )?),
        "fable_scan_folders" => ok(fable::inventory::fable_scan_folders(opt_str(a, "root"))?),
        "fable_scan_folder_children" => ok(fable::inventory::fable_scan_folder_children(
            req_str(a, "root")?,
            req_str(a, "path")?,
        )?),
        "fable_folder_size" => ok(fable::inventory::fable_folder_size(req_str(a, "path")?)?),
        "fable_backfill_lang" => ok(fable::inventory::fable_backfill_lang()?),

        // ── 企业 Schema 知识库(本体)——desktop 走 #[tauri::command],server/Docker 须在此显式接 dispatch ──
        "ontology_schemas" => ok(fable::ontology::ontology_schemas()?),
        "ontology_overview" => ok(fable::ontology::ontology_overview()?),
        "ontology_seed" => ok(fable::ontology::ontology_seed(req_str(a, "schemaId")?)?),
        "ontology_extract" => ok(fable::ontology::ontology_extract(app, req_str(a, "schemaId")?)?),
        "ontology_triples" => ok(fable::ontology::ontology_triples(
            req_str(a, "schemaId")?,
            opt_usize(a, "limit").map(|v| v as u32),
        )?),
        "fable_index_start" => {
            ok(fable::index::fable_index_start(app, opt_usize(a, "maxChunks"))?)
        }
        "fable_index_optimize" => ok(fable::index::fable_index_optimize()?),
        "fable_search" => ok(fable::retrieve::fable_search(
            req_str(a, "query")?,
            opt_usize(a, "topK"),
            opt_str(a, "mode"),
            opt_str(a, "scope"),
        )?),
        "fable_eval" => ok(fable::eval::fable_eval(
            opt_str(a, "path"),
            opt_usize(a, "topK"),
            opt_str(a, "mode"),
        )?),
        "fable_eval_template" => ok(fable::eval::fable_eval_template(opt_str(a, "path"))?),

        // ── 文件中心(可视化文件库)──
        "file_overview" => ok(fable::files::file_overview(opt_str(a, "root"))?),
        "file_grid" => ok(fable::files::file_grid(
            opt_str(a, "root"),
            a.get("clusterId").and_then(|v| v.as_i64()),
            opt_str(a, "kind"),
            opt_str(a, "lang"),
            opt_str(a, "sort"),
            opt_str(a, "query"),
            opt_usize(a, "page"),
            opt_usize(a, "pageSize"),
        )?),
        "file_thumb" => ok(fable::files::file_thumb(
            req_str(a, "abspath")?,
            a.get("max").and_then(|v| v.as_u64()).map(|n| n as u32),
        )?),
        "file_gist" => ok(fable::files::file_gist(req_str(a, "abspath")?)?),
        "file_cluster_build" => ok(fable::files::file_cluster_build(app, opt_str(a, "root"))?),
        "file_smart_cluster" => {
            ok(fable::files::file_smart_cluster(app, opt_str(a, "root"), opt_bool(a, "quick"))?)
        }
        "file_profile_html" => ok(fable::files::file_profile_html(opt_str(a, "root"))?),
        "file_suggest_workflows" => ok(fable::files::suggest_workflows(opt_str(a, "root"))?),
        "file_graph" => ok(fable::files::file_graph(opt_str(a, "root"))?),
        "file_warm_thumbs" => ok(fable::files::file_warm_thumbs(
            vec_str(a, "paths"),
            a.get("max").and_then(|v| v.as_u64()).map(|n| n as u32),
        )?),
        "file_cluster_llm" => ok(fable::files::file_cluster_llm(app, opt_str(a, "root"))?),
        "file_titles_llm" => ok(fable::files::file_titles_llm(app, opt_str(a, "root"))?),
        "file_titles_clear" => ok(fable::files::file_titles_clear()?),
        "file_cluster_model_get" => ok(fable::files::file_cluster_model_get()),
        "file_cluster_model_set" => ok(fable::files::file_cluster_model_set(
            opt_bool(a, "enabled"),
            opt_str(a, "baseUrl"),
            opt_str(a, "model"),
            opt_str(a, "apiKey"),
        )?),

        // ── Conv ──
        "conv_list_projects" => ok(conv::conv_list_projects()),
        "conv_create_project" => ok(conv::conv_create_project(req_str(a, "name")?)?),
        "conv_set_project_kb_scope" => ok(conv::conv_set_project_kb_scope(
            req_str(a, "projectId")?,
            opt_str(a, "kbScope"),
        )?),
        "conv_open_project_dir" => ok(conv::conv_open_project_dir(req_str(a, "projectId")?)?),
        "conv_archive_project" => ok(conv::conv_archive_project(req_str(a, "projectId")?)?),
        "conv_list_conversations" => ok(conv::conv_list_conversations(req_str(a, "projectId")?)),
        "conv_create_conversation" => ok(conv::conv_create_conversation(req_str(a, "projectId")?)?),
        "conv_delete_conversation" => {
            ok(conv::conv_delete_conversation(req_str(a, "conversationId")?)?)
        }
        "conv_get_messages" => ok(conv::conv_get_messages(req_str(a, "conversationId")?)),
        "conv_rename_conversation" => ok(conv::conv_rename_conversation(
            req_str(a, "conversationId")?,
            req_str(a, "title")?,
        )?),

        // ── Persona ──
        "persona_list" => ok(persona::persona_list()),
        "persona_apply" => ok(persona::persona_apply(
            req_str(a, "projectId")?,
            req_str(a, "personaId")?,
            bool_def(a, "overwrite", false),
        )?),

        // ── Expert / 专家团（Docker/web 版同样要能用专家市场、向导推荐、一键入驻）──
        "expert_list" => ok(expert::expert_list()),
        "expert_list_by_group" => ok(expert::expert_list_by_group(req_str(a, "group")?)),
        "expert_groups" => ok(expert::expert_groups()),
        "expert_route" => {
            let req: expert::RouteRequest =
                serde_json::from_value(a.get("req").cloned().unwrap_or(Value::Null))
                    .map_err(|e| format!("expert_route 参数解析失败: {e}"))?;
            ok(expert::expert_route(req))
        }
        "expert_get" => ok(expert::expert_get(req_str(a, "id")?)),
        "expert_match_auto" => ok(expert::expert_match_auto(req_str(a, "query")?)),
        "expert_apply" => ok(expert::expert_apply(
            req_str(a, "projectId")?,
            req_str(a, "expertId")?,
            bool_def(a, "overwrite", false),
        )?),
        "expert_avatar" => ok(expert::expert_avatar(req_str(a, "id")?)),
        "expert_avatar_slots" => ok(expert::expert_avatar_slots()),
        "expert_team_spawn" => ok(expert::expert_team_spawn(
            req_str(a, "projectId")?,
            req_str(a, "taskDescription")?,
        )),
        "expert_agents_status" => ok(expert::expert_agents_status(req_str(a, "projectId")?)),
        "expert_teams" => ok(expert::expert_teams()),
        "expert_team_get" => ok(expert::expert_team_get(req_str(a, "id")?)),
        "team_apply" => ok(expert::team_apply(
            req_str(a, "projectId")?,
            req_str(a, "teamId")?,
            bool_def(a, "overwrite", false),
        )?),
        "expert_export" => ok(expert::expert_export(req_str(a, "id")?)?),
        "team_export" => ok(expert::team_export(req_str(a, "id")?)?),
        "expert_route_debug" => ok(expert::expert_route_debug(req_str(a, "query")?)),
        "expert_recommend_from_kb" => ok(expert::expert_recommend_from_kb(opt_str(a, "scope"))),

        // ── Chat (sync 部分) ──
        "chat_cancel" => ok(chat::chat_cancel(req_str(a, "reqId")?)?),
        "chat_build_manifest" => ok(chat::chat_build_manifest(opt_str(a, "conversationId"))),
        "chat_attach_files" => ok(chat::chat_attach_files(
            opt_str(a, "conversationId"),
            vec_str(a, "paths"),
        )),
        "chat_attach_image" => ok(chat::chat_attach_image(
            opt_str(a, "conversationId"),
            req_str(a, "name")?,
            req_str(a, "dataBase64")?,
        )?),
        "open_url" => ok(chat::open_url(req_str(a, "url")?)?),
        "artifact_read" => ok(chat::artifact_read(req_str(a, "path")?)?),
        "artifact_write" => ok(chat::artifact_write(
            req_str(a, "path")?,
            req_str(a, "content")?,
        )?),
        "artifact_open_external" => ok(chat::artifact_open_external(req_str(a, "path")?)?),
        "artifact_reveal" => ok(chat::artifact_reveal(req_str(a, "path")?)?),
        "artifact_list" => ok(chat::artifact_list(opt_str(a, "conversationId"))),
        "artifact_search" => ok(chat::artifact_search(req_str(a, "query")?)),

        // ── Project（容器内降级：list/status 可用，run/stop 受限但保留）──
        "project_list" => ok(project::project_list(opt_str(a, "conversationId"))),
        "project_status" => ok(project::project_status(req_str(a, "root")?)),
        "project_run" => ok(project::project_run(app, req_str(a, "root")?)?),
        "project_stop" => ok(project::project_stop(app, req_str(a, "root")?)?),

        // ── CLAUDE.md ──
        "claude_md_list_projects" => ok(claude_md::claude_md_list_projects()),
        "claude_md_kb_info" => ok(claude_md::claude_md_kb_info()),
        "claude_md_read" => ok(claude_md::claude_md_read(
            req_str(a, "area")?,
            opt_str(a, "projectId"),
        )?),
        "claude_md_write" => ok(claude_md::claude_md_write(
            req_str(a, "area")?,
            opt_str(a, "projectId"),
            req_str(a, "content")?,
        )?),

        // ── Skills ──
        "list_skills" => ok(skills::list_skills()),
        "get_skill" => ok(skills::get_skill(req_str(a, "id")?)?),
        "create_skill" => {
            let args = skills::CreateSkillArgs {
                id: req_str(a, "id")?,
                name: req_str(a, "name")?,
                description: req_str(a, "description")?,
                system_prompt: opt_str(a, "systemPrompt")
                    .or_else(|| opt_str(a, "system_prompt"))
                    .unwrap_or_default(),
            };
            ok(skills::create_skill(args)?)
        }
        "install_skill" => ok(skills::install_skill(req_str(a, "id")?)?),
        "import_skill" => ok(skills::import_skill(req_str(a, "source")?)?),
        "delete_skill" => ok(skills::delete_skill(req_str(a, "id")?)?),

        // ── Provider + 用量 + Codex ──
        "provider_list" => ok(provider::provider_list()?),
        "provider_switch" => ok(provider::provider_switch(req_str(a, "id")?)?),
        "provider_set_link_mode" => ok(provider::provider_set_link_mode(bool_def(a, "link", false))?),
        "provider_save" => {
            let input: provider::ProviderInput =
                serde_json::from_value(a.get("input").cloned().unwrap_or(Value::Null))
                    .map_err(|e| format!("provider_save 参数解析失败: {e}"))?;
            ok(provider::provider_save(input)?)
        }
        "provider_delete" => ok(provider::provider_delete(req_str(a, "id")?)?),
        "usage_summary" => ok(provider::usage_summary()?),
        "codex_status" => ok(provider::codex_status()?),
        "codex_start_login" => ok(provider::codex_start_login()?),
        "codex_poll_login" => ok(provider::codex_poll_login(
            req_str(a, "deviceCode")?,
            req_str(a, "userCode")?,
        )?),
        "claude_oauth_status" => ok(provider::claude_oauth_status()?),
        "claude_start_login" => ok(provider::claude_start_login()?),
        "claude_finish_login" => ok(provider::claude_finish_login(
            req_str(a, "pasted")?,
            req_str(a, "verifier")?,
            req_str(a, "state")?,
        )?),
        "codex_proxy_info" => ok(codex_proxy::codex_proxy_info()),

        // ── 推理后端(R3)：外部 GPU 节点端点状态(含连通性探测)──
        "infer_status" => ok(infer::status_json()),

        // ── Forge 渲染能力 preflight：跨平台「能出 PPT/视频吗、缺啥降级」透明上报 ──
        "forge_preflight" => ok(forge::forge_preflight()),
        // ── Forge 渲染：截图 + 纯 Rust OOXML 打 .pptx（三平台同一份，替 pptxgenjs）──
        "forge_build_pptx" => forge::forge_build_pptx(vec_str(a, "images"), req_str(a, "out")?),
        "forge_screenshot" => forge::forge_screenshot(
            req_str(a, "url")?,
            req_str(a, "out")?,
            opt_usize(a, "width").map(|n| n as u32),
            opt_usize(a, "height").map(|n| n as u32),
            opt_usize(a, "scale").map(|n| n as u32),
        ),
        // spec JSON → 原生可编辑 .pptx(路线 B 传统PPT,零浏览器 → slim 镜像也能出 PPT)
        "forge_spec_to_pptx" => forge::spec_to_pptx_sync(req_str(a, "spec")?, req_str(a, "out")?),
        // 桌面同名命令是 async 包装(防冻 UI); 这里本就在阻塞线程池, 直调同步内核
        "forge_deck_to_pptx" => forge::deck_to_pptx_sync(
            req_str(a, "deck")?,
            req_str(a, "out")?,
            opt_usize(a, "width").map(|n| n as u32),
            opt_usize(a, "height").map(|n| n as u32),
            a.get("searchable").and_then(|v| v.as_bool()),
            opt_usize(a, "slides"),
        ),
        "forge_deck_to_video" => forge::forge_deck_to_video(
            req_str(a, "deck")?,
            req_str(a, "out")?,
            a.get("secondsPerSlide").and_then(|v| v.as_f64()),
            opt_usize(a, "fps").map(|n| n as u32),
            opt_usize(a, "width").map(|n| n as u32),
            opt_usize(a, "height").map(|n| n as u32),
            opt_usize(a, "slides"),
            opt_str(a, "audio"),
            opt_str(a, "narration"),
            a.get("transition").and_then(|v| v.as_f64()),
            a.get("motion").and_then(|v| v.as_bool()),
        ),
        "forge_deck_fx_video" => forge::forge_deck_fx_video(
            req_str(a, "deck")?,
            req_str(a, "out")?,
            opt_usize(a, "fps").map(|n| n as u32),
            a.get("durationMs").and_then(|v| v.as_u64()),
            opt_usize(a, "width").map(|n| n as u32),
            opt_usize(a, "height").map(|n| n as u32),
            opt_usize(a, "slide"),
        ),
        "forge_tts" => forge::forge_tts(
            req_str(a, "text")?,
            req_str(a, "out")?,
            opt_str(a, "voice"),
            opt_str(a, "languageBoost"),
        ),

        // ── 环境医生（容器内只读检测；安装类降级为提示）──
        "env_check" => ok(doctor::env_check()),
        "env_fix_path" => ok(doctor::env_fix_path()?),
        "env_claude_update_check" => ok(doctor::env_claude_update_check()),
        "env_install_claude" | "env_install_node" | "env_install_pwsh" | "env_update_claude" => {
            Err("容器环境已预装运行所需组件，无需在此安装。如需升级请更新镜像 (docker pull)。".to_string())
        }
        "env_cancel" => ok(doctor::env_cancel(req_str(a, "reqId")?)?),

        // ── 飞书 / 企微 / 自媒体账号 ──
        "feishu_get_config" => ok(feishu::feishu_get_config()),
        "feishu_set_config" => {
            let cfg: feishu::FeishuConfig =
                serde_json::from_value(a.get("config").cloned().unwrap_or(Value::Null))
                    .map_err(|e| format!("feishu_set_config 参数解析失败: {e}"))?;
            ok(feishu::feishu_set_config(cfg)?)
        }
        "feishu_test_connection" => ok(feishu::feishu_test_connection()),
        "feishu_create_qr" => ok(feishu::feishu_create_qr()?),
        "feishu_open_console" => ok(feishu::feishu_open_console()?),
        "feishu_gateway_start" => ok(feishu::feishu_gateway_start(app)?),
        "feishu_gateway_stop" => ok(feishu::feishu_gateway_stop(app)?),
        "feishu_gateway_status" => ok(feishu::feishu_gateway_status()),
        "wecom_scan_create" => ok(wecom::wecom_scan_create(req_str(a, "source")?)?),
        "media_accounts_status" => ok(accounts::media_accounts_status()),
        "media_account_forget" => ok(accounts::media_account_forget(req_str(a, "platform")?)?),

        // ── 降级/桌面专属：给惰性 stub，保证前端不报错 ──
        "sandbox_status" => ok(json!({
            "docker_installed": false, "docker_running": false, "image_built": false,
            "image_name": "polaris-sandbox:alpine", "container_running": false,
            "container_name": "polaris-sandbox",
            "notes": ["容器(Docker)模式：Docker-in-Docker 沙箱本期降级，不可用"]
        })),
        "sandbox_build_image" | "sandbox_start" | "sandbox_stop" | "sandbox_exec" => {
            Err("容器模式下沙箱板块已降级（Docker-in-Docker 风险高）。".to_string())
        }
        "cube_config_get" => ok(json!({"backend":"docker","endpoint":"","apiKey":""})),
        "cube_config_set" => ok(a.get("config").cloned().unwrap_or(json!({"backend":"docker"}))),
        "cube_status" => ok(json!({
            "backend":"docker","endpoint":"","configured":false,"reachable":false,
            "note":"容器模式 - 无沙箱探测"
        })),
        "updater_get_state" => ok(json!({"phase":"idle","note":"容器版用 docker pull 更新"})),
        "updater_check" => ok(json!({"phase":"idle"})),
        "updater_apply" => Err("容器版请用 docker pull 拉新镜像更新。".to_string()),

        // ── 容器自更新(前端 useUpdater.ts 容器线调用)──
        // docker_status:报「能不能自更新」给 UpdatePanel(POLARIS_DOCKER_SOCKET 开关 + docker.sock 在位
        //   + 当前镜像 tag + update.sh 是否打进镜像)。
        "docker_status" => ok(json!({
            "updater_enabled": std::env::var("POLARIS_DOCKER_SOCKET").map(|v| v == "1").unwrap_or(false),
            "socket_present": std::path::Path::new("/var/run/docker.sock").exists(),
            "current_tag": std::env::var("POLARIS_TAG").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "latest".to_string()),
            "update_script": std::path::Path::new("/usr/local/bin/update.sh").exists(),
        })),
        // docker_update:跑 /usr/local/bin/update.sh(默认模式)——它经 docker.sock 用「自己的镜像」
        //   起一个独立替身容器执行 pull + up -d(不能在被替换的容器里直接 up,compose 会随旧容器被杀)。
        //   脚本起完 detached 替身即返回;真正的替换由替身异步完成(约 1~3 分钟,期间连接断,刷新即可)。
        "docker_update" => {
            if !bool_def(a, "confirm", false) {
                return Err("更新需要确认 (confirm: true)".to_string());
            }
            if !std::env::var("POLARIS_DOCKER_SOCKET").map(|v| v == "1").unwrap_or(false) {
                return Err("远程更新未启用:请在 compose 设 POLARIS_DOCKER_SOCKET=1 并挂载 /var/run/docker.sock。".to_string());
            }
            if !std::path::Path::new("/var/run/docker.sock").exists() {
                return Err("/var/run/docker.sock 未挂载,容器无法自更新。".to_string());
            }
            if !std::path::Path::new("/usr/local/bin/update.sh").exists() {
                return Err("/usr/local/bin/update.sh 不存在(镜像未含更新脚本)。".to_string());
            }
            let tag = std::env::var("POLARIS_TAG").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "latest".to_string());
            match std::process::Command::new("/usr/local/bin/update.sh").output() {
                Ok(out) => ok(json!({
                    "success": out.status.success(),
                    "exit_code": out.status.code(),
                    "tag": tag,
                    "stdout": String::from_utf8_lossy(&out.stdout).to_string(),
                    "stderr": String::from_utf8_lossy(&out.stderr).to_string(),
                    "note": "替身已出发。拉取完成后当前容器会被替换(约 1~3 分钟,取决于网速),期间连接会断,稍后刷新页面即可。",
                })),
                Err(e) => Err(format!("启动 update.sh 失败: {e}")),
            }
        }

        other => Err(format!("未知命令: {other}")),
    }
}

// ───────────────────────── WebSocket（emit 推流）─────────────────────────

async fn ws_handler(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    // WS 鉴权走 query token（浏览器 WS 不便带自定义 header）。
    if let Some(expected) = state.auth_token.as_ref() {
        if params.get("token").map(String::as_str) != Some(expected.as_str()) {
            return (StatusCode::UNAUTHORIZED, "未授权").into_response();
        }
    }
    let rx = state.tx.subscribe();
    ws.on_upgrade(move |socket| ws_loop(socket, rx))
}

async fn ws_loop(socket: WebSocket, mut rx: broadcast::Receiver<Event>) {
    let (mut sender, mut receiver) = socket.split();
    // 读侧：仅用于探测客户端关闭（前端浏览器模式不向后端 emit）。
    let mut closed = tokio::spawn(async move { while let Some(Ok(_)) = receiver.next().await {} });

    loop {
        tokio::select! {
            recv = rx.recv() => match recv {
                Ok(ev) => {
                    let frame = json!({ "topic": ev.topic, "payload": ev.payload });
                    if sender.send(Message::Text(frame.to_string())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue, // 落后则跳过旧帧
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = &mut closed => break,
        }
    }
}

// ───────────────────────── 文件上传（替代原生文件对话框）─────────────────────────

/// 浏览器拖拽/选择文件 → 存到服务端临时目录 → 返回服务端绝对路径列表。
/// 前端随后用这些路径调 `kb_upload_files` / `chat_attach_files`（它们吃服务端路径）。
async fn upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    if !check_auth(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"未授权"}))).into_response();
    }
    let base = upload_dir();
    if let Err(e) = std::fs::create_dir_all(&base) {
        return err_resp(format!("创建上传目录失败: {e}"));
    }
    let mut saved: Vec<Value> = Vec::new();
    while let Ok(Some(field)) = multipart.next_field().await {
        let fname = field
            .file_name()
            .map(sanitize_filename)
            .unwrap_or_else(|| "upload.bin".to_string());
        let data = match field.bytes().await {
            Ok(b) => b,
            Err(e) => return err_resp(format!("读取上传字段失败: {e}")),
        };
        let dst = unique_path(&base, &fname);
        if let Err(e) = std::fs::write(&dst, &data) {
            return err_resp(format!("写入上传文件失败: {e}"));
        }
        saved.push(json!({
            "name": fname,
            "path": dst.to_string_lossy().replace('\\', "/"),
            "size": data.len(),
        }));
    }
    Json(json!({ "files": saved })).into_response()
}

fn upload_dir() -> PathBuf {
    if let Some(u) = directories::UserDirs::new() {
        u.home_dir().join("Polaris").join("uploads-inbox")
    } else {
        PathBuf::from("/tmp/polaris-uploads")
    }
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

fn unique_path(base: &Path, fname: &str) -> PathBuf {
    let p = base.join(fname);
    if !p.exists() {
        return p;
    }
    let stem = Path::new(fname)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let ext = Path::new(fname).extension().and_then(|s| s.to_str());
    let mut i = 1u32;
    loop {
        let cand = match ext {
            Some(e) => base.join(format!("{stem}-{i}.{e}")),
            None => base.join(format!("{stem}-{i}")),
        };
        if !cand.exists() {
            return cand;
        }
        i += 1;
    }
}

// ───────────────────────── 受限文件读取（iframe 预览 / 图片）─────────────────────────

#[derive(serde::Deserialize)]
struct FileQuery {
    path: String,
    /// 鉴权 token：window.open/<a download> 等导航请求带不了 Authorization 头，故走 query（与 /ws 同理）。
    #[serde(default)]
    token: Option<String>,
    /// download=1 → 加 Content-Disposition: attachment 强制下载（网页版「下载文件」按钮用）。
    #[serde(default)]
    download: Option<String>,
}

async fn serve_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
) -> Response {
    // 鉴权：header（Authorization/x-polaris-token）或 query ?token=（导航请求兜底）。
    let authed = match state.auth_token.as_ref() {
        None => true,
        Some(exp) => q.token.as_deref() == Some(exp.as_str()) || check_auth(&state, &headers),
    };
    if !authed {
        return (StatusCode::UNAUTHORIZED, "未授权").into_response();
    }
    let path = PathBuf::from(&q.path);
    // 安全闸：只允许读 KB 根 / ~/Polaris / /data 下的文件。
    let allowed = allowed_roots();
    let canon = match std::fs::canonicalize(&path) {
        Ok(p) => p,
        Err(_) => return (StatusCode::NOT_FOUND, "文件不存在").into_response(),
    };
    if !allowed.iter().any(|root| crate::kb::path_contains(root, &canon)) {
        return (StatusCode::FORBIDDEN, "路径不在允许范围").into_response();
    }
    match tokio::fs::read(&canon).await {
        Ok(bytes) => {
            let ct = mime_for(&canon);
            let mut resp = ([(header::CONTENT_TYPE, ct)], bytes).into_response();
            if q.download.as_deref() == Some("1") {
                let fname = canon
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("download");
                // RFC 5987：filename* 用 UTF-8 百分号编码，兼容中文名。
                let cd = format!("attachment; filename*=UTF-8''{}", pct_encode(fname));
                if let Ok(v) = header::HeaderValue::from_str(&cd) {
                    resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
                }
            }
            resp
        }
        Err(_) => (StatusCode::NOT_FOUND, "读取失败").into_response(),
    }
}

/// RFC 5987 百分号编码：unreserved 原样，其余按 UTF-8 字节转 %XX。
fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{:02X}", b));
        }
    }
    out
}

fn allowed_roots() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    let kb = PathBuf::from(crate::kb::kb_root());
    if let Ok(c) = std::fs::canonicalize(&kb) {
        v.push(c);
    }
    if let Some(u) = directories::UserDirs::new() {
        if let Ok(c) = std::fs::canonicalize(u.home_dir().join("Polaris")) {
            v.push(c);
        }
    }
    if let Ok(c) = std::fs::canonicalize("/data") {
        v.push(c);
    }
    v
}

fn mime_for(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "md" | "markdown" | "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

// ───────────────────────── 前端静态托管（SPA fallback）─────────────────────────

async fn spa_fallback(State(state): State<AppState>, uri: axum::http::Uri) -> Response {
    let rel = uri.path().trim_start_matches('/');
    // 安全闸: rel 取自原始 URL, 裸 socket 客户端能塞 `../../etc/passwd`(hyper 不规范化
    // `..` 段)。任一段为 `..` 或绝对/盘符前缀 → 当 SPA 路由回 index.html, 绝不拼出 web_dir。
    let traversal = rel.split(['/', '\\']).any(|seg| seg == "..")
        || Path::new(rel).is_absolute()
        || rel.contains(':');
    let mut candidate = if traversal {
        state.web_dir.join("index.html")
    } else {
        state.web_dir.join(rel)
    };
    // 目录或不存在 → 回 index.html（SPA 路由）。
    if rel.is_empty() || !candidate.is_file() {
        candidate = state.web_dir.join("index.html");
    }
    // 双保险: canonicalize 后必须仍落在 web_dir 内(防符号链接/漏网的相对段)。
    if let (Ok(canon), Ok(root)) = (
        std::fs::canonicalize(&candidate),
        std::fs::canonicalize(&state.web_dir),
    ) {
        if !crate::kb::path_contains(&root, &canon) {
            candidate = state.web_dir.join("index.html");
        }
    }
    match tokio::fs::read(&candidate).await {
        Ok(bytes) => {
            let ct = mime_for(&candidate);
            Response::builder()
                .header(header::CONTENT_TYPE, ct)
                .body(Body::from(bytes))
                .unwrap()
        }
        Err(_) => (StatusCode::NOT_FOUND, "前端资源缺失").into_response(),
    }
}
