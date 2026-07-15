//! Docker(server) 外壳 —— axum HTTP/WS 服务，替代 Tauri 桌面外壳。
//!
//! - `POST /api/invoke {cmd,args}`：把前端 `invoke()` 分发到各引擎模块函数（≈75 命令）。
//! - `GET  /ws`：把各模块 `app.emit(topic,payload)` 广播给浏览器（替代 Tauri event）。
//! - `POST /api/upload`：multipart 上传，替代桌面原生文件对话框（返回服务端临时路径）。
//! - `GET  /api/file?path=`：受限静态文件读取（iframe 预览 / 图片）。
//! - 其余路径：托管打包好的前端 `dist/`（SPA fallback）。
//!
//! 设计要点：引擎模块（kb/chat/conv/...）源码与桌面版**完全相同**，仅外壳不同。

use crate::apihub::{api_router, mime_for, ApiState};
use crate::host::{AppHandle, Event};
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;

#[derive(Clone)]
pub struct AppState {
    pub app: AppHandle,
    pub tx: broadcast::Sender<Event>,
    pub auth_token: Arc<Option<String>>,
    pub web_dir: PathBuf,
    readiness: Arc<ReadinessState>,
}

#[derive(Clone)]
struct ReadinessState {
    data_root: PathBuf,
    init_errors: Vec<String>,
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
    let data_root = directories::UserDirs::new()
        .map(|u| u.home_dir().join("Polaris"))
        .unwrap_or_else(|| PathBuf::from("/root/Polaris"));
    let mut init_errors = Vec::new();
    if let Err(e) = std::fs::create_dir_all(&data_root) {
        init_errors.push(format!("数据目录不可创建: {e}"));
    }
    if let Err(e) = std::env::set_current_dir(&data_root) {
        init_errors.push(format!("数据目录不可进入: {e}"));
        eprintln!(
            "[polaris-server] 设工作目录失败({}): {e}",
            data_root.display()
        );
    }

    // ── 初始化各模块（与桌面 lib.rs setup 等价，去掉桌面专属部分）──
    if let Err(e) = crate::kb::init(&app) {
        init_errors.push(format!("kb: {e}"));
        eprintln!("[polaris-server] kb::init 失败: {e}");
    }
    // 内核桥注入(kb/fable/expert → chat): 与桌面 setup 同一时机, 任何 chat 请求之前。
    crate::wiring::wire_engine_bridges();
    if let Err(e) = crate::conv::init(&app) {
        init_errors.push(format!("conv: {e}"));
    }
    if let Err(e) = crate::chat::init(&app) {
        init_errors.push(format!("chat: {e}"));
    }
    if let Err(e) = crate::claude_md::init(&app) {
        init_errors.push(format!("claude_md: {e}"));
    }
    if let Err(e) = crate::provider::init(&app) {
        init_errors.push(format!("provider: {e}"));
    }
    crate::skills::seed_video_studio_skill();
    crate::skills::seed_deck_studio_skill();
    crate::skills::seed_web_studio_skill();
    crate::skills::seed_wechat_typesetter_skill();
    crate::skills::seed_wechat_tasks_skill();
    crate::skills::seed_project_check_skill();
    // 注：「请教毛主席」默认隐藏 —— 仅在用户主动安装「毛主席」资料包时装 consult-mao 技能，
    // 启动时不再自动补装（盘上已有数据保留，不删）。
    // 飞书网关「开机自动启动」（若用户开了 auto_start 且凭证齐全）。
    crate::integrations::feishu::auto_start_if_enabled(&app);
    // 寓言计划:感官 API 坞 + 回声层「每日做梦」调度 + 检索枢纽(与桌面 setup 等价)。
    crate::sense::init();
    crate::voice::init();
    crate::echo::start_scheduler(app.clone());
    crate::fable::init();
    // 协作:晨会定时盘点(有主 Agent 的项目每天推 collab:morning 到面板)。
    {
        let app2 = app.clone();
        crate::collab::lead::start_morning_scheduler(move |topic, v| {
            let _ = app2.emit(topic, v);
        });
    }

    let explicit = std::env::var("POLARIS_AUTH_TOKEN")
        .ok()
        .filter(|s| !s.is_empty());
    let allow_open = std::env::var("POLARIS_ALLOW_OPEN")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    // 安全默认:未设口令且未显式选择开放 → **自动生成本次随机口令并打印**,而不是裸奔对所有
    // 网络开放(旧默认会合成 owner 全放行,而 /api/invoke 能触到 docker_update/provider_save 等
    // 高危命令)。要固定口令设 POLARIS_AUTH_TOKEN;要维持旧的「无口令全开放」显式设 POLARIS_ALLOW_OPEN=1。
    let auth_token = match explicit {
        Some(t) => {
            println!("[polaris-server] 已启用访问口令 (POLARIS_AUTH_TOKEN)");
            Some(t)
        }
        None if allow_open => {
            println!("[polaris-server] ⚠ POLARIS_ALLOW_OPEN=1:未设口令,服务对所有可达网络开放(请仅在完全可信网络使用)");
            None
        }
        None => {
            let gen = crate::collab::auth::random_token();
            println!("[polaris-server] 🔐 未设 POLARIS_AUTH_TOKEN,已自动生成本次访问口令:");
            println!("[polaris-server]     {gen}");
            println!("[polaris-server]     客户端用此口令连接;固定口令请设 POLARIS_AUTH_TOKEN,");
            println!(
                "[polaris-server]     要维持旧的「无口令全开放」请显式设 POLARIS_ALLOW_OPEN=1。"
            );
            Some(gen)
        }
    };

    let web_dir = std::env::var("POLARIS_WEB_DIR").unwrap_or_else(|_| "/srv/web".to_string());
    let web_dir = PathBuf::from(web_dir);

    let auth_token = Arc::new(auth_token);
    let state = AppState {
        app: app.clone(),
        tx: tx.clone(),
        auth_token: auth_token.clone(),
        web_dir: web_dir.clone(),
        readiness: Arc::new(ReadinessState {
            data_root,
            init_errors,
        }),
    };
    // 应用数据面(invoke/upload/file/ws)已抽至 crate::apihub(双壳共用);
    // server 壳与桌面 hosting 各自构造 ApiState 并 merge api_router。
    let api_state = ApiState {
        app: app.clone(),
        tx: tx.clone(),
        auth_token: auth_token.clone(),
    };

    let port: u16 = std::env::var("POLARIS_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080);
    // 协作面状态(crate::collab::http,双壳共用):与 AppState 共享事件广播与口令;
    // advertise = 票据分享码里带的「主机可达地址」,成员端凭码零填写入伙。
    let collab_state = crate::collab::http::CollabState {
        app: state.app.clone(),
        auth_token: state.auth_token.clone(),
        advertise: Arc::new(parking_lot::RwLock::new(
            crate::collab::http::detect_advertise_urls(port),
        )),
    };

    let app_router = Router::new()
        .route("/api/health", get(health))
        .route("/api/live", get(health))
        .route("/api/ready", get(ready))
        .route("/api/status", get(status))
        // ★ DOCKER 分叉:主仓无此路由。容器里没有 Tauri IPC 拿版本号,前端
        //   useUpdater.ts 的 ensureCurrentVersion() 靠它显示「当前版本」(否则显示 v—)。
        //   与 /api/health 同为公开端点(DOCKER.md/DEPLOY-SYNOLOGY.md 记的 curl 自检口径)。
        .route("/api/version", get(version_info))
        .fallback(get(spa_fallback))
        .with_state(state)
        // 应用数据面(invoke/upload/file/ws)—— 抽出的双壳共用路由,已自带 ApiState。
        .merge(api_router(api_state))
        // /api/collab/* 与 /git/* 全部路由(crate::collab::http,双壳共用)。
        // 三者都是 Router<()>(各自 with_state),merge 在后;layer 在 merge 之后 →
        // 下面的 body 上限与 CORS 同样罩住数据面与协作端点。
        .merge(crate::collab::http::collab_router(collab_state, false))
        // 普通 JSON/表单统一 2MB；只有 /api/upload 单独放宽到 512MB。此前把 512MB
        // 套在整棵路由上，未认证登录/bootstrap 也能迫使服务缓冲巨型请求体。
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        // 桌面客户端(Tauri webview,源 http://tauri.localhost)连远端主机是跨源请求;
        // 用 Bearer token 鉴权(非 cookie),故 permissive(允许任意 Origin、方法、头,
        // 不放行凭证)即可,顺带自动处理 OPTIONS 预检。同源的 Docker/Web 前端不受影响。
        .layer(CorsLayer::permissive());

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    // 隧道上游对齐:服务器壳的真实端口告知 tunnel(与 hosting.rs 同理)。
    #[cfg(feature = "collab-net")]
    crate::collab::tunnel::set_upstream_port(port);
    println!(
        "[polaris-server] 监听 http://0.0.0.0:{port} (前端目录: {})",
        web_dir.display()
    );

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let mut server_task = tokio::spawn(async move {
        axum::serve(listener, app_router)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
    });
    tokio::select! {
        joined = &mut server_task => {
            joined.map_err(|e| anyhow::anyhow!("HTTP 服务任务异常: {e}"))??;
        }
        _ = shutdown_signal() => {
            println!("[polaris-server] 收到退出信号，停止接收新请求并保存状态…");
            let _ = shutdown_tx.send(());
            // 先停止会继续产生写入/子进程的集成，再落盘对话状态。
            let _ = crate::integrations::feishu::feishu_gateway_stop(app.clone());
            crate::collab::gitea::stop();
            crate::conv::flush();
            if tokio::time::timeout(Duration::from_secs(15), &mut server_task)
                .await
                .is_err()
            {
                eprintln!("[polaris-server] 15 秒内仍有长连接未退出，强制结束服务任务");
                server_task.abort();
                let _ = server_task.await;
            }
        }
    }
    crate::conv::flush();
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

// ───────────────────────── /api/status 运维水位(R7)─────────────────────────
//
// 给群晖运维/监控用的水位接口: 容器内存(贴近 mem_limit/OOM 风险)、宿主内存、数据盘用量
// (防写满)、claude 配置在位、推理端点(R3)状态。全部 best-effort: 读不到的项返回
// available:false 而非报错, 非 Linux 环境(开发机)也能编译运行。详细状态会暴露端点和
// 宿主资源信息，因此仅 owner 可读；结果短时缓存且单航班采集，避免并发耗尽阻塞线程池。

static STATUS_CACHE: Lazy<tokio::sync::Mutex<Option<(Instant, Value)>>> =
    Lazy::new(|| tokio::sync::Mutex::new(None));

async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    // 鉴权复用 apihub 的基础面解析(与 /api/invoke 同语义);role_rank 在 collab::http。
    let Some(ctx) = crate::apihub::app_ctx_headers(&state.auth_token, &headers) else {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"未授权"}))).into_response();
    };
    if crate::collab::http::role_rank(&ctx.role) < 3 {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"状态详情仅主机管理员可读"})),
        )
            .into_response();
    }
    let auth_set = state.auth_token.is_some();
    let init_errors = state.readiness.init_errors.clone();
    let mut cache = STATUS_CACHE.lock().await;
    if let Some((at, value)) = cache.as_ref() {
        if at.elapsed() < Duration::from_secs(15) {
            return Json(value.clone()).into_response();
        }
    }
    // 含 df 子进程 + 推理端点探测(阻塞/网络), 丢到阻塞线程池, 勿卡 async worker。
    let v = tokio::task::spawn_blocking(move || collect_status(auth_set, init_errors))
        .await
        .unwrap_or_else(|_| json!({ "ok": false, "error": "status 采集失败" }));
    *cache = Some((Instant::now(), v.clone()));
    Json(v).into_response()
}

fn collect_status(auth_set: bool, init_errors: Vec<String>) -> Value {
    let data_root = directories::UserDirs::new()
        .map(|u| u.home_dir().join("Polaris"))
        .unwrap_or_else(|| PathBuf::from("/root/Polaris"));
    json!({
        "ok": true,
        "service": "polaris-server",
        "auth_token_set": auth_set,
        "startup": {
            "ready": init_errors.is_empty(),
            "errors": init_errors,
        },
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
    let Ok(out) = std::process::Command::new("df")
        .arg("-kP")
        .arg(path)
        .output()
    else {
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
            (
                home.to_string_lossy().to_string(),
                home.join(".claude.json"),
            )
        }
    };
    json!({ "config_dir": dir, "config_file_present": cfg.is_file() })
}

/// 公开存活探针必须是常量时间、无外部 IO，避免探针本身成为阻塞池 DoS 放大器。
/// 详细依赖/资源状态由 owner-only 且带缓存的 `/api/status` 提供。
async fn health() -> Response {
    "ok".into_response()
}

/// ★ DOCKER 分叉(主仓无此 handler)。
///
/// 版本号来源 = 镜像里的 `/app/VERSION`（Dockerfile 由仓库根 VERSION 文件 COPY 进来，
/// 与 image.yml 打的镜像 tag 同源）。读不到才退回编译期的 CARGO_PKG_VERSION —— 两者
/// 平时一致；分家只会发生在「本地 docker build 时忘了同步 VERSION」，那时更该露出真实值。
///
/// 只吐版本 + 自更新能力位，与 `docker_status` 同口径（前端 UpdatePanel 两处都用）。
/// 公开无鉴权：与 /api/health 一致，DOCKER.md 记的 `curl .../api/version` 自检靠它。
async fn version_info() -> Response {
    let version = tokio::fs::read_to_string("/app/VERSION")
        .await
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    Json(json!({
        "version": version,
        "flavor": "docker",
        "updater_enabled": std::env::var("POLARIS_DOCKER_SOCKET").map(|v| v == "1").unwrap_or(false),
        "socket_present": std::path::Path::new("/var/run/docker.sock").exists(),
    }))
    .into_response()
}

static READY_CACHE: Lazy<tokio::sync::Mutex<Option<(Instant, bool)>>> =
    Lazy::new(|| tokio::sync::Mutex::new(None));

/// 不泄露环境详情的就绪探针：核心初始化成功、前端入口存在、数据卷可真实写入、
/// 协作 SQLite 可打开。结果短时缓存并单航班执行，避免公开探针放大磁盘/线程池压力。
async fn ready(State(state): State<AppState>) -> Response {
    let mut cache = READY_CACHE.lock().await;
    if let Some((at, is_ready)) = cache.as_ref() {
        if at.elapsed() < Duration::from_secs(10) {
            return if *is_ready {
                "ready".into_response()
            } else {
                (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response()
            };
        }
    }
    let readiness = state.readiness.clone();
    let web_dir = state.web_dir.clone();
    let is_ready = tokio::task::spawn_blocking(move || readiness_check(&readiness, &web_dir))
        .await
        .unwrap_or(false);
    *cache = Some((Instant::now(), is_ready));
    if is_ready {
        "ready".into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response()
    }
}

fn readiness_check(state: &ReadinessState, web_dir: &Path) -> bool {
    if !state.init_errors.is_empty() || !web_dir.join("index.html").is_file() {
        return false;
    }
    let probe = state
        .data_root
        .join(format!(".polaris-readiness-{}", std::process::id()));
    let wrote = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&probe)
        .and_then(|mut f| {
            use std::io::Write as _;
            f.write_all(b"ready")?;
            f.sync_all()
        })
        .is_ok();
    let _ = std::fs::remove_file(&probe);
    if !wrote {
        return false;
    }
    crate::collab::db::open_db()
        .and_then(|conn| {
            conn.query_row("SELECT 1", [], |_| Ok(()))
                .map_err(|e| e.to_string())
        })
        .is_ok()
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
