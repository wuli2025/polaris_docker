//! collab/apihub.rs —— 应用数据面 HTTP(invoke / upload / file / ws),**双壳共用**。
//!
//! 从 server.rs 抽出,让「桌面内嵌主机(hosting.rs)」也能对远端(手机/中继网关)提供
//! 与 Docker server 壳**完全一致**的应用能力:≈200 条命令分发、文件上传/预览、事件流。
//! server.rs 仅保留壳专属部分(前端托管 SPA、/api/status 水位、就绪探针)。
//!
//! 事件句柄 `AppHandle` 双壳二选一(与 chat/pipeline.rs 同款):
//!  - server 壳:`crate::host::AppHandle`(broadcast shim),命令 emit 直接进 `tx` → /ws。
//!  - desktop 主机:`tauri::AppHandle`,命令 emit 进 tauri 事件系统;hosting.rs 另架
//!    一座 `tauri.listen("chat:stream") → bus` 单向桥把对话流灌进 `tx`,再经 /ws 送手机。
//!
//! 鉴权沿用 server.rs 原语义(基础面宽松:无口令则合成 owner;真会话 token 升级真实身份)。
#![cfg(feature = "collab-host")]

#[cfg(not(feature = "desktop"))]
use crate::host::AppHandle;
#[cfg(feature = "desktop")]
use tauri::AppHandle;

use crate::collab::http::{bearer_of, err_resp, ok, role_rank, ws_loop, AuthCtx};
use crate::host::Event;
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Query, State, WebSocketUpgrade},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::broadcast;

/// 应用数据面所需的最小状态(AppState 的子集):事件句柄 + 广播发送端 + 访问口令。
/// server 壳里 app 与 tx 是同一条广播;desktop 主机里 app=tauri、tx=hosting bus(靠桥连通)。
#[derive(Clone)]
pub struct ApiState {
    pub app: AppHandle,
    pub tx: broadcast::Sender<Event>,
    pub auth_token: Arc<Option<String>>,
}

impl ApiState {
    fn app(&self) -> AppHandle {
        self.app.clone()
    }
}

/// 应用数据面路由(invoke/upload/file/ws)。返回 `Router<()>`(已 with_state),
/// 供 server.rs 与 hosting.rs 各自 `.merge()` 进主路由。
/// /api/upload 单挂 512MB body 上限;其余端点由调用方整体 2MB 上限约束。
pub fn api_router(state: ApiState) -> Router {
    Router::new()
        .route("/api/invoke", post(invoke))
        .route(
            "/api/upload",
            post(upload).layer(DefaultBodyLimit::max(512 * 1024 * 1024)),
        )
        .route("/api/file", get(serve_file))
        .route("/ws", get(ws_handler))
        .with_state(state)
}

// ───────────────────────── 鉴权 ─────────────────────────
//
// 基础应用面(对话/知识库/文件…)与 collab 面**分开**:维持历史语义——没设全局口令就
// 开放(合成 owner),避免"有人建了协作账号就把整个 App 锁死"。真会话 token 仍会升级成
// 真实身份并受命令角色闸约束。多用户部署要连基础命令也强制登录时设 POLARIS_REQUIRE_LOGIN=1。

/// 基础面鉴权核心:按访问口令 + 传入 token 解析身份。server.rs 的壳专属端点
/// (/api/status 等)也复用它,故 pub(crate)、且不吃 State(只吃 auth_token 引用)。
pub(crate) fn resolve_app_auth_token(
    auth_token: &Option<String>,
    token: Option<&str>,
) -> Option<AuthCtx> {
    // 全局口令命中 = owner(单人 Docker 管理员)。
    if let Some(expected) = auth_token.as_ref() {
        if token == Some(expected.as_str()) {
            return Some(AuthCtx {
                user_id: 0,
                username: "admin".into(),
                role: "owner".into(),
            });
        }
    }
    // 带了有效会话 token → 用真实身份(多用户下据此过角色闸)。
    if let Some(t) = token {
        if let Ok(u) = crate::collab::auth::check_session(t) {
            return Some(AuthCtx {
                user_id: u.id,
                username: u.username,
                role: u.role,
            });
        }
    }
    // 是否强制登录:设了全局口令,或显式打开 POLARIS_REQUIRE_LOGIN。
    let require_login = auth_token.is_some()
        || std::env::var("POLARIS_REQUIRE_LOGIN")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
    if require_login {
        return None; // 上面没拿到有效凭据 → 拒绝
    }
    // 开放模式:合成 owner(历史行为,单人场景全放行)。
    Some(AuthCtx {
        user_id: 0,
        username: "local".into(),
        role: "owner".into(),
    })
}

/// 从请求头(Bearer)解析基础面身份。server.rs 壳专属端点复用,故 pub(crate)+同门控。
#[cfg(feature = "server")]
pub(crate) fn app_ctx_headers(auth_token: &Option<String>, headers: &HeaderMap) -> Option<AuthCtx> {
    resolve_app_auth_token(auth_token, bearer_of(headers).as_deref())
}

/// resolve_app_auth_token 的异步壳:鉴权含同步 SQLite 查询(check_session),直接跑在
/// axum async worker 上会钉住 reactor,挪进阻塞线程池。会话短缓存(collab/auth.rs)命中
/// 时闭包内不落库,这层 spawn_blocking 兜的是未命中/首次。
async fn resolve_app_auth(state: &ApiState, token: Option<String>) -> Option<AuthCtx> {
    let auth_token = state.auth_token.clone();
    tokio::task::spawn_blocking(move || resolve_app_auth_token(&auth_token, token.as_deref()))
        .await
        .ok()
        .flatten()
}

async fn app_ctx(state: &ApiState, headers: &HeaderMap) -> Option<AuthCtx> {
    resolve_app_auth(state, bearer_of(headers)).await
}

/// 基础 `/api/invoke` 目前操作机器级项目/知识库/供应商配置,没有逐用户资源 ACL;
/// chat 还可调用 Bash/PowerShell。ACL 完成前统一 fail-closed 为 owner;团队成员只用 `/api/collab/*`。
fn required_role(_cmd: &str) -> u8 {
    3
}

// ───────────────────────── /api/invoke 分发 ─────────────────────────

// 信封与参数记账下沉契约层(polaris-protocol, 分仓规划 v2 第 1 仓种子):
// Args 记录分发代码实际读过哪些顶层参数, 没被读的 = 拼错名/契约漂移 —— 默认经
// `x-polaris-unknown-args` 响应头曝光, POLARIS_STRICT_ARGS=1 时直接 400。
use polaris_protocol::{strict_args_enabled, Args, InvokeRequest};

/// 把命令错误串按「客户端错误 vs 服务端错误」映射到合适的 HTTP 状态码,而非一律 500。
fn invoke_err_resp(e: String) -> Response {
    let status = if e.starts_with("未知命令") {
        StatusCode::NOT_FOUND
    } else if (e.contains("参数")
        && (e.contains("缺少") || e.contains("解析失败") || e.contains("无效")))
        // 非法枚举值(如 fable_search「mode 只接受 hybrid | grep | vector」)是客户端错误,
        // 此前落进兜底 500(spot-check 揪出错误分类)。
        || e.contains("只接受")
    {
        StatusCode::BAD_REQUEST
    } else if e.contains("(403)") {
        StatusCode::FORBIDDEN
    } else if e.contains("(404)") {
        StatusCode::NOT_FOUND
    } else if e.contains("(429)") {
        StatusCode::TOO_MANY_REQUESTS
    } else if e.contains("insufficient") || e.contains("余额") {
        // 上游供应商余额/额度失败(如云嵌入 403 account balance):外部依赖失败,
        // 非本服务端 bug。映射 502 让客户端提示「换供应商/充值」而非报「服务器崩了」。
        StatusCode::BAD_GATEWAY
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    (status, Json(json!({ "error": e }))).into_response()
}

async fn invoke(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(req): Json<InvokeRequest>,
) -> Response {
    let Some(ctx) = app_ctx(&state, &headers).await else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"未授权 (口令错误或会话失效)"})),
        )
            .into_response();
    };
    if role_rank(&ctx.role) < required_role(&req.cmd) {
        crate::collab::db::audit(&ctx.username, "invoke.denied", &req.cmd, "角色不足");
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": format!("权限不足:命令 {} 需要更高角色", req.cmd)})),
        )
            .into_response();
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
                Err(e) => invoke_err_resp(e),
            },
            Err(e) => invoke_err_resp(format!("chat_send 参数解析失败: {e}")),
        };
    }

    // 其余命令同步执行，丢到阻塞线程池（内含 ureq 网络/文件 IO，勿阻塞 async worker）。
    // 必须设超时：阻塞池只有 64 线程，慢命令无超时会一条条钉死线程。
    let timeout_secs: u64 = std::env::var("POLARIS_INVOKE_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let cmd_for_err = cmd.clone();

    // 分发按 flavor 分裂:
    //  - server 壳:引擎命令是同步 fn → spawn_blocking 丢阻塞池,外套超时。
    //  - desktop 主机:引擎命令是 async 薄包装(内部自带 spawn_blocking)→ 直接 await,
    //    走精简 dispatch_desktop(覆盖手机数据面所需命令;全量命令用 Docker/NAS server 版)。
    // out 统一成 Result<Result<Value,String>, tokio::task::JoinError> 供下方一致处理。
    #[cfg(not(feature = "desktop"))]
    let out: Result<(Result<Value, String>, Vec<String>), tokio::task::JoinError> = {
        let fut = tokio::task::spawn_blocking(move || {
            let a = Args::new(args);
            let r = dispatch_sync(&cmd, &a, app);
            (r, a.unknown_keys())
        });
        if timeout_secs == 0 {
            fut.await
        } else {
            match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), fut).await {
                Ok(joined) => joined,
                Err(_) => {
                    return (
                        StatusCode::GATEWAY_TIMEOUT,
                        Json(json!({
                            "error": format!(
                                "命令 {cmd_for_err} 执行超时({timeout_secs}s)，已停止等待（任务可能仍在后台运行）"
                            )
                        })),
                    )
                        .into_response();
                }
            }
        }
    };
    #[cfg(feature = "desktop")]
    let out: Result<(Result<Value, String>, Vec<String>), tokio::task::JoinError> = {
        let a = Args::new(args);
        let res = if timeout_secs == 0 {
            dispatch_desktop(&cmd, &a, app).await
        } else {
            match tokio::time::timeout(
                std::time::Duration::from_secs(timeout_secs),
                dispatch_desktop(&cmd, &a, app),
            )
            .await
            {
                Ok(r) => r,
                Err(_) => {
                    return (
                        StatusCode::GATEWAY_TIMEOUT,
                        Json(json!({
                            "error": format!(
                                "命令 {cmd_for_err} 执行超时({timeout_secs}s)，已停止等待（任务可能仍在后台运行）"
                            )
                        })),
                    )
                        .into_response();
                }
            }
        };
        Ok((res, a.unknown_keys()))
    };

    match out {
        Ok((Ok(v), unknown)) => {
            if unknown.is_empty() {
                return Json(v).into_response();
            }
            // 未知参数 = 客户端拼错名/契约漂移(top_k vs topK 一类),此前被静默容忍产生
            // 错误业务结果。默认曝光不破坏既有客户端;严格模式直接拒绝。
            if strict_args_enabled() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": format!(
                        "未知参数: {}(命令 {} 未读取任何同名参数;各命令参数名以 tauri.ts 为准)",
                        unknown.join(", "), cmd_for_err
                    )})),
                )
                    .into_response();
            }
            let mut resp = Json(v).into_response();
            if let Ok(hv) = axum::http::HeaderValue::from_str(&unknown.join(",")) {
                resp.headers_mut().insert("x-polaris-unknown-args", hv);
            }
            resp
        }
        Ok((Err(e), _)) => invoke_err_resp(e),
        Err(e) => err_resp(format!("内部任务失败: {e}")),
    }
}

/// 桌面内嵌主机的命令分发(desktop flavor)。desktop 下引擎命令是 async 薄包装,故这里
/// `await`。只覆盖**手机远程数据面实际用到的命令**(文件浏览/预览、对话辅助、会话读取);
/// 其余命令请用 Docker/NAS server 版(全量 dispatch_sync)。手机的账号/项目/任务走
/// /api/collab/*(collab_router),不经此分发。
#[cfg(feature = "desktop")]
async fn dispatch_desktop(cmd: &str, a: &Args, _app: AppHandle) -> Result<Value, String> {
    use crate::*;
    match cmd {
        // ── 文件中心(手机「文件」页 + 预览) ──
        "file_overview" => ok(fable::files::file_overview(opt_str(a, "root")).await?),
        "file_grid" => ok(fable::files::file_grid(
            opt_str(a, "root"),
            a.get("clusterId").and_then(|v| v.as_i64()),
            opt_str(a, "kind"),
            opt_str(a, "lang"),
            opt_str(a, "sort"),
            opt_str(a, "query"),
            opt_usize(a, "page"),
            opt_usize(a, "pageSize"),
        )
        .await?),
        "file_thumb" => ok(fable::files::file_thumb(
            req_str(a, "abspath")?,
            a.get("max").and_then(|v| v.as_u64()).map(|n| n as u32),
        )
        .await?),
        "file_gist" => ok(fable::files::file_gist(req_str(a, "abspath")?).await?),

        // ── 知识库检索(手机备用) ──
        "kb_search" => ok(kb::kb_search(req_str(a, "query")?, opt_usize(a, "topK")).await),

        // ── 对话辅助(chat_send 在 invoke 里单独特判) ──
        "chat_cancel" => ok(chat::chat_cancel(req_str(a, "reqId")?)?),
        "chat_attach_files" => ok(chat::chat_attach_files(
            opt_str(a, "conversationId"),
            vec_str(a, "paths"),
        )),
        "chat_attach_image" => ok(chat::chat_attach_image(
            opt_str(a, "conversationId"),
            req_str(a, "name")?,
            req_str(a, "dataBase64")?,
        )?),
        "chat_build_manifest" => ok(chat::chat_build_manifest(opt_str(a, "conversationId"))),

        // ── 产物(手机产物 chip 预览走 /api/file;这里给读取/列举备用) ──
        "artifact_read" => ok(chat::artifact_read(req_str(a, "path")?)?),
        "artifact_list" => ok(chat::artifact_list(opt_str(a, "conversationId")).await),
        "artifact_search" => ok(chat::artifact_search(req_str(a, "query")?).await),

        // ── 会话读取(手机历史主要走本地存储,这些为兼容/备用) ──
        "conv_list_projects" => ok(conv::conv_list_projects()),
        "conv_list_conversations" => ok(conv::conv_list_conversations(req_str(a, "projectId")?)),
        "conv_get_messages" => ok(conv::conv_get_messages(req_str(a, "conversationId")?)),
        "conv_create_conversation" => {
            ok(conv::conv_create_conversation(req_str(a, "projectId")?)?)
        }
        "conv_delete_conversation" => {
            ok(conv::conv_delete_conversation(req_str(a, "conversationId")?)?)
        }
        // 设备联盟遥测:本机资源实况(远端设备经中继/隧道取用)。
        "sys_stats" => ok(crate::sysstat::sample()),

        // ── 输入区选择器(手机豆包式输入条:模型/技能;只读) ──
        // 桌面版 provider_list 原样含 auth_token 真 key(仅桌面本机 UI 可见);
        // 手机数据面必须脱敏 —— 只给选择器要用的字段,绝不下发密钥与 settings_config。
        "provider_list" => {
            let r = provider::provider_list()?;
            ok(json!({
                "providers": r.providers.iter().map(|p| json!({
                    "id": p.id,
                    "name": p.name,
                    "category": p.category,
                    "protocol": p.protocol,
                    "color": p.color,
                    "hasKey": p.has_key,
                })).collect::<Vec<Value>>(),
                "currentId": r.current_id,
            }))
        }
        "list_skills" => ok(skills::list_skills().await),

        _ => Err(format!(
            "命令 {cmd} 在桌面主机模式暂不支持(手机远程仅开放文件/对话数据面;全部命令请用 Docker/NAS server 版)"
        )),
    }
}

// 参数提取器（前端 invoke 走 camelCase 键）
fn req_str(a: &Args, k: &str) -> Result<String, String> {
    a.get(k)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("缺少字符串参数 `{k}`"))
}
fn opt_str(a: &Args, k: &str) -> Option<String> {
    a.get(k).and_then(|v| {
        if v.is_null() {
            None
        } else {
            v.as_str().map(|s| s.to_string())
        }
    })
}
fn opt_usize(a: &Args, k: &str) -> Option<usize> {
    a.get(k).and_then(|v| v.as_u64()).map(|n| n as usize)
}
// 下列取参 helper 只被 dispatch_sync 用,与它同门控,否则 desktop 编译时报 dead_code。
#[cfg(not(feature = "desktop"))]
fn opt_bool(a: &Args, k: &str) -> Option<bool> {
    a.get(k).and_then(|v| v.as_bool())
}
#[cfg(not(feature = "desktop"))]
fn opt_f64(a: &Args, k: &str) -> Option<f64> {
    a.get(k).and_then(|v| v.as_f64())
}
#[cfg(not(feature = "desktop"))]
fn opt_u8(a: &Args, k: &str) -> Option<u8> {
    a.get(k).and_then(|v| v.as_u64()).map(|n| n.min(255) as u8)
}
#[cfg(not(feature = "desktop"))]
fn bool_def(a: &Args, k: &str, d: bool) -> bool {
    a.get(k).and_then(|v| v.as_bool()).unwrap_or(d)
}
fn vec_str(a: &Args, k: &str) -> Vec<String> {
    a.get(k)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}
/// 必填字符串数组:缺失/非数组/元素非字符串都报 400,避免参数错被伪装成「空结果」
#[cfg(not(feature = "desktop"))]
fn req_vec_str(a: &Args, k: &str) -> Result<Vec<String>, String> {
    let arr = a
        .get(k)
        .ok_or_else(|| format!("缺少数组参数 `{k}`"))?
        .as_array()
        .ok_or_else(|| format!("参数 `{k}` 无效:必须是字符串数组"))?;
    arr.iter()
        .map(|x| {
            x.as_str()
                .map(|s| s.to_string())
                .ok_or_else(|| format!("参数 `{k}` 无效:数组元素必须是字符串"))
        })
        .collect()
}

/// server 壳全量命令分发(≈200 命令,同步直调各引擎函数)。desktop 下这些引擎命令是
/// async 薄包装(见 dispatch_desktop),签名不兼容,故本函数**仅 server flavor 编译**。
#[cfg(not(feature = "desktop"))]
fn dispatch_sync(cmd: &str, a: &Args, app: AppHandle) -> Result<Value, String> {
    use crate::*;
    match cmd {
        // ── KB ──
        "kb_root" => ok(kb::kb_root()),
        "kb_default_root" => ok(kb::kb_default_root()),
        // 设备联盟遥测:云主机/server 壳自采本机资源。
        "sys_stats" => ok(crate::sysstat::sample()),
        "kb_set_root" => ok(kb::kb_set_root(req_str(a, "newPath")?)?),
        "kb_scan" => ok(kb::kb_scan_sync()?),
        "kb_compile" => ok(wiki::kb_compile(app)?),
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
        "kb_scan_sources" => ok(kb::kb_scan_sources()),
        "kb_quarantine" => ok(kb::kb_quarantine(req_str(a, "relPath")?)?),
        "kb_pack_list" => ok(kb::kb_pack_list()),
        "kb_pack_install" => ok(kb::kb_pack_install(app, req_str(a, "id")?)?),
        "kb_pack_remove" => ok(kb::kb_pack_remove(req_str(a, "id")?)?),

        // ── 全盘资源归集 ──
        "scan_roots" => ok(scan::scan_roots()),
        "scan_resources" => ok(scan::scan_resources(
            vec_str(a, "roots"),
            opt_usize(a, "max"),
        )?),

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
            a.get("pinyinThreshold")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32),
            opt_str(a, "overlayPos"),
            opt_str(a, "polishApiBase"),
            opt_str(a, "polishApiKey"),
            opt_str(a, "polishModel"),
        )?),
        "voice_lexicon_get" => ok(voice::voice_lexicon_get()),
        "voice_hotword_add" => ok(voice::voice_hotword_add(req_str(a, "word")?)?),
        "voice_hotword_remove" => ok(voice::voice_hotword_remove(req_str(a, "word")?)?),
        "voice_correction_add" => ok(voice::voice_correction_add(
            req_str(a, "wrong")?,
            req_str(a, "right")?,
        )?),
        "voice_correction_remove" => ok(voice::voice_correction_remove(req_str(a, "wrong")?)?),
        "voice_anti_pollute" => ok(voice::voice_anti_pollute(req_str(a, "text")?)),
        // AI 整形试跑(设置页「测一下整形」):纯 HTTP 调 LLM,容器内可用
        "voice_polish" => ok(voice::voice_polish(req_str(a, "text")?)?),
        "voice_transcribe_file" => ok(voice::voice_transcribe_file(req_str(a, "path")?)?),
        "voice_listen_start" => ok(voice::voice_listen_start(app)?),
        "voice_listen_stop" => ok(voice::voice_listen_stop()?),
        "voice_dictate_start" => ok(voice::voice_dictate_start(app)?),
        "voice_dictate_stop" => ok(voice::voice_dictate_stop()?),
        "voice_learn_correction" => ok(voice::voice_learn_correction(
            req_str(a, "wrong")?,
            req_str(a, "right")?,
        )?),
        "voice_lexicon_learn" => ok(voice::voice_lexicon_learn(
            req_str(a, "text")?,
            opt_usize(a, "top"),
        )?),

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
        "echo_clear_context" => ok(echo::echo_clear_context(app, req_str(a, "convId")?)?),
        // Figma 往返桥（回程拉取）
        "figma_pull" => ok(figma_bridge::figma_pull(
            req_str(a, "file")?,
            req_str(a, "token")?,
        )?),
        "figma_export_svgs" => ok(figma_bridge::figma_export_svgs(
            req_str(a, "file")?,
            req_vec_str(a, "ids")?,
            req_str(a, "token")?,
        )?),
        "echo_briefing_today" => ok(echo::echo_briefing_today()),
        "echo_briefing_dismiss" => ok(echo::echo_briefing_dismiss(req_str(a, "id")?)),
        "echo_briefing_run" => ok(echo::echo_briefing_run(app)?),
        "kb_overview_get" => ok(kb::kb_overview_get()),

        // ── 寓言计划 · 检索枢纽(盘点 L1a + 向量索引 + 塌平混检)──
        "fable_status" => ok(fable::fable_status()?),
        "fable_cancel" => ok(fable::fable_cancel(opt_str(a, "task"))),
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
        "fable_audit" => ok(fable::inventory::fable_audit(
            opt_str(a, "mode"),
            opt_usize(a, "sample"),
        )?),

        // ── 企业 Schema 知识库(本体)——desktop 走 #[tauri::command],server/Docker 须在此显式接 dispatch ──
        "ontology_schemas" => ok(fable::ontology::ontology_schemas()?),
        "ontology_overview" => ok(fable::ontology::ontology_overview()?),
        "ontology_seed" => ok(fable::ontology::ontology_seed(req_str(a, "schemaId")?)?),
        "ontology_extract" => ok(fable::ontology::ontology_extract(
            app,
            req_str(a, "schemaId")?,
        )?),
        "ontology_triples" => ok(fable::ontology::ontology_triples(
            req_str(a, "schemaId")?,
            opt_usize(a, "limit").map(|v| v as u32),
        )?),
        "fable_index_start" => ok(fable::index::fable_index_start(
            app,
            opt_usize(a, "maxChunks"),
        )?),
        "fable_lex_build_start" => ok(fable::index::fable_lex_build_start(app)?),
        "fable_index_optimize" => ok(fable::index::fable_index_optimize()?),
        "fable_index_repair" => ok(fable::index::fable_index_repair()?),
        "fable_dedupe_scan" => ok(fable::index::fable_dedupe_scan(Some(bool_def(
            a, "backfill", false,
        )))?),
        "fable_local_embed_status" => ok(fable::index::fable_local_embed_status()?),
        "fable_local_embed_download" => ok(fable::index::fable_local_embed_download(app)?),
        "fable_local_embed_set_enabled" => ok(fable::index::fable_local_embed_set_enabled(
            bool_def(a, "on", false),
        )?),
        "fable_search" => ok(fable::retrieve::fable_search(
            req_str(a, "query")?,
            opt_usize(a, "topK"),
            opt_str(a, "mode"),
            opt_str(a, "scope"),
        )?),
        "fable_search_ai" => ok(fable::retrieve::fable_search_ai(
            req_str(a, "query")?,
            opt_usize(a, "topK"),
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
        "file_smart_cluster" => ok(fable::files::file_smart_cluster(
            app,
            opt_str(a, "root"),
            opt_bool(a, "quick"),
        )?),
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
        "conv_project_bind_collab" => ok(conv::conv_project_bind_collab(
            req_str(a, "projectId")?,
            a.get("collabProjectId")
                .and_then(|v| v.as_i64())
                .ok_or("缺 collabProjectId")?,
            req_str(a, "collabHost").unwrap_or_default(),
        )?),
        "conv_set_project_kb_scope" => ok(conv::conv_set_project_kb_scope(
            req_str(a, "projectId")?,
            opt_str(a, "kbScope"),
        )?),
        "conv_set_project_work_dir" => ok(conv::conv_set_project_work_dir(
            req_str(a, "projectId")?,
            opt_str(a, "workDir"),
        )?),
        "conv_open_project_dir" => ok(conv::conv_open_project_dir(req_str(a, "projectId")?)?),
        "conv_archive_project" => ok(conv::conv_archive_project(req_str(a, "projectId")?)?),
        "conv_list_conversations" => ok(conv::conv_list_conversations(req_str(a, "projectId")?)),
        "conv_create_conversation" => ok(conv::conv_create_conversation(req_str(a, "projectId")?)?),
        "conv_delete_conversation" => ok(conv::conv_delete_conversation(req_str(
            a,
            "conversationId",
        )?)?),
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

        // ── 配色引擎(全 app 配色唯一真源)──
        // server dispatch 曾漏注册 → web/server 端 palette_generate 一律 404(codex 深测揪出)。
        // 桌面 generate_handler 早已注册;补齐双壳一致。注意参数是 mood(不是 mode)。
        "palette_generate" => ok(palette::palette_generate(opt_str(a, "seed"), opt_str(a, "mood"))?),

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
        "provider_set_link_mode" => ok(provider::provider_set_link_mode(bool_def(
            a, "link", false,
        ))?),
        "provider_set_route_mode" => ok(provider::provider_set_route_mode(bool_def(
            a, "route", false,
        ))?),
        "provider_save" => {
            let input: provider::ProviderInput =
                serde_json::from_value(a.get("input").cloned().unwrap_or(Value::Null))
                    .map_err(|e| format!("provider_save 参数解析失败: {e}"))?;
            ok(provider::provider_save(input)?)
        }
        "provider_delete" => ok(provider::provider_delete(req_str(a, "id")?)?),
        "usage_summary" => ok(provider::usage_summary()?),
        "provider_balance" => ok(provider::provider_balance(req_str(a, "id")?)?),
        // ── 生图供应商坞(独立表)+ 生图 ──
        "image_provider_list" => ok(provider::image_provider_list()?),
        "image_provider_save" => {
            let input: provider::ImageProviderInput =
                serde_json::from_value(a.get("input").cloned().unwrap_or(Value::Null))
                    .map_err(|e| format!("image_provider_save 参数解析失败: {e}"))?;
            ok(provider::image_provider_save(input)?)
        }
        "image_provider_delete" => ok(provider::image_provider_delete(req_str(a, "id")?)?),
        "image_provider_switch" => ok(provider::image_provider_switch(req_str(a, "id")?)?),
        // 桌面同名命令是 async 包装; apihub 本就在阻塞线程池里, 直调同步内核。
        "forge_image" => ok(crate::imagegen::forge_image_sync(
            req_str(a, "prompt")?,
            req_str(a, "out")?,
            opt_str(a, "ratio"),
        )?),
        "codex_status" => ok(provider::codex_status()?),
        "codex_start_login" => ok(provider::codex_start_login(Some(bool_def(
            a,
            "forceDevice",
            false,
        )))?),
        "codex_poll_login" => ok(provider::codex_poll_login(
            req_str(a, "deviceCode")?,
            req_str(a, "userCode")?,
        )?),
        "codex_login_poll" => ok(provider::codex_login_poll()?),
        "codex_login_cancel" => ok(provider::codex_login_cancel()?),
        "claude_oauth_status" => ok(provider::claude_oauth_status()?),
        "claude_start_login" => ok(provider::claude_start_login(Some(bool_def(
            a,
            "forceManual",
            false,
        )))?),
        "claude_login_poll" => ok(provider::claude_login_poll()?),
        "claude_login_cancel" => ok(provider::claude_login_cancel()?),
        "claude_finish_login" => ok(provider::claude_finish_login(
            req_str(a, "pasted")?,
            req_str(a, "verifier")?,
            req_str(a, "state")?,
        )?),
        "codex_proxy_info" => ok(integrations::codex_proxy::codex_proxy_info()),

        // ── 推理后端(R3)：外部 GPU 节点端点状态(含连通性探测)──
        "infer_status" => ok(infer::status_json()),

        // ── Forge 渲染能力 preflight：跨平台「能出 PPT/视频吗、缺啥降级」透明上报 ──
        "forge_preflight" => ok(forge::forge_preflight()),
        // ── Forge 渲染：截图 + 纯 Rust OOXML 打 .pptx（三平台同一份，替 pptxgenjs）──
        "forge_build_pptx" => forge::build_pptx_sync(vec_str(a, "images"), req_str(a, "out")?),
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
        "forge_deck_to_video" => forge::deck_to_video_sync(
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
        "forge_deck_fx_video" => forge::deck_fx_video_sync(
            req_str(a, "deck")?,
            req_str(a, "out")?,
            opt_usize(a, "fps").map(|n| n as u32),
            a.get("durationMs").and_then(|v| v.as_u64()),
            opt_usize(a, "width").map(|n| n as u32),
            opt_usize(a, "height").map(|n| n as u32),
            opt_usize(a, "slide"),
        ),
        "forge_tts" => forge::forge_tts_sync(
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
            Err(
                "容器环境已预装运行所需组件，无需在此安装。如需升级请更新镜像 (docker pull)。"
                    .to_string(),
            )
        }
        // uv 未预烤进容器镜像,自动安装脚本也只支持 Win/mac 桌面 → 给明确指引而非 404
        "env_install_uv" => Err(
            "容器环境不支持在线安装 uv:请进容器执行 `curl -LsSf https://astral.sh/uv/install.sh | sh`,或更新预装 uv 的镜像 (docker pull)。"
                .to_string(),
        ),
        // uv 缓存治理:纯子进程调用 `uv cache dir/clean`,容器内直通(没装 uv 时函数自会报「未找到」)
        "env_uv_cache_info" => ok(doctor::env_uv_cache_info()),
        "env_uv_cache_clean" => ok(doctor::env_uv_cache_clean()?),
        "env_cancel" => ok(doctor::env_cancel(req_str(a, "reqId")?)?),

        // ── 飞书 / 企微 / 自媒体账号 ──
        "feishu_get_config" => ok(integrations::feishu::feishu_get_config()),
        "feishu_set_config" => {
            let cfg: integrations::feishu::FeishuConfig =
                serde_json::from_value(a.get("config").cloned().unwrap_or(Value::Null))
                    .map_err(|e| format!("feishu_set_config 参数解析失败: {e}"))?;
            ok(integrations::feishu::feishu_set_config(cfg)?)
        }
        "feishu_test_connection" => ok(integrations::feishu::feishu_test_connection()),
        "feishu_create_qr" => ok(integrations::feishu::feishu_create_qr()?),
        "feishu_open_console" => ok(integrations::feishu::feishu_open_console()?),
        "feishu_gateway_start" => ok(integrations::feishu::feishu_gateway_start(app)?),
        "feishu_gateway_stop" => ok(integrations::feishu::feishu_gateway_stop(app)?),
        "feishu_gateway_status" => ok(integrations::feishu::feishu_gateway_status()),
        "wecom_scan_create" => ok(integrations::wecom::wecom_scan_create(req_str(
            a, "source",
        )?)?),
        "media_accounts_status" => ok(accounts::media_accounts_status()),
        "media_account_forget" => ok(accounts::media_account_forget(req_str(a, "platform")?)?),

        // ── 盘管理(NAS 网络盘记忆 + 映射)──
        "nas_list" => ok(crate::integrations::nas::nas_list()),
        "nas_save" => {
            let rec = serde_json::from_value(a.get("record").cloned().unwrap_or(Value::Null))
                .map_err(|e| format!("record 参数无效：{e}"))?;
            ok(crate::integrations::nas::nas_save(rec)?)
        }
        "nas_forget" => ok(crate::integrations::nas::nas_forget(req_str(a, "id")?)?),
        "nas_connect" => {
            let rec = serde_json::from_value(a.get("record").cloned().unwrap_or(Value::Null))
                .map_err(|e| format!("record 参数无效：{e}"))?;
            ok(crate::integrations::nas::nas_connect(rec)?)
        }
        "nas_disconnect" => {
            let rec = serde_json::from_value(a.get("record").cloned().unwrap_or(Value::Null))
                .map_err(|e| format!("record 参数无效：{e}"))?;
            ok(crate::integrations::nas::nas_disconnect(rec)?)
        }

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
        "cube_config_set" => ok(a
            .get("config")
            .cloned()
            .unwrap_or(json!({"backend":"docker"}))),
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
            if !std::env::var("POLARIS_DOCKER_SOCKET")
                .map(|v| v == "1")
                .unwrap_or(false)
            {
                return Err("远程更新未启用:请在 compose 设 POLARIS_DOCKER_SOCKET=1 并挂载 /var/run/docker.sock。".to_string());
            }
            if !std::path::Path::new("/var/run/docker.sock").exists() {
                return Err("/var/run/docker.sock 未挂载,容器无法自更新。".to_string());
            }
            if !std::path::Path::new("/usr/local/bin/update.sh").exists() {
                return Err("/usr/local/bin/update.sh 不存在(镜像未含更新脚本)。".to_string());
            }
            let tag = std::env::var("POLARIS_TAG")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "latest".to_string());
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
    State(state): State<ApiState>,
    Query(params): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    // WS 鉴权走 query token（浏览器 WS 不便带自定义 header）。
    let Some(ctx) = resolve_app_auth(&state, params.get("token").cloned()).await else {
        return (StatusCode::UNAUTHORIZED, "未授权").into_response();
    };
    if role_rank(&ctx.role) < 3 {
        return (StatusCode::FORBIDDEN, "基础事件流需要 owner 权限").into_response();
    }
    let rx = state.tx.subscribe();
    ws.on_upgrade(move |socket| ws_loop(socket, rx, ctx))
}

// ───────────────────────── 文件上传（替代原生文件对话框）─────────────────────────

/// 浏览器拖拽/选择文件 → 存到服务端临时目录 → 返回服务端绝对路径列表。
async fn upload(
    State(state): State<ApiState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let Some(ctx) = app_ctx(&state, &headers).await else {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"未授权"}))).into_response();
    };
    if role_rank(&ctx.role) < 3 {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"文件上传需要 owner 权限"})),
        )
            .into_response();
    }
    let base = upload_dir();
    if let Err(e) = std::fs::create_dir_all(&base) {
        return err_resp(format!("创建上传目录失败: {e}"));
    }
    use tokio::io::AsyncWriteExt;
    let mut saved: Vec<Value> = Vec::new();
    loop {
        let mut field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => return err_resp(format!("上传流中断: {e}")),
        };
        let fname = field
            .file_name()
            .map(sanitize_filename)
            .unwrap_or_else(|| "upload.bin".to_string());
        let (dst, mut f) = match create_unique(&base, &fname).await {
            Ok(v) => v,
            Err(e) => return err_resp(format!("创建上传文件失败: {e}")),
        };
        let mut size: u64 = 0;
        loop {
            match field.chunk().await {
                Ok(Some(chunk)) => {
                    size += chunk.len() as u64;
                    if let Err(e) = f.write_all(&chunk).await {
                        drop(f);
                        let _ = tokio::fs::remove_file(&dst).await;
                        return err_resp(format!("写入上传文件失败: {e}"));
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    drop(f);
                    let _ = tokio::fs::remove_file(&dst).await;
                    return err_resp(format!("读取上传字段失败: {e}"));
                }
            }
        }
        if let Err(e) = f.flush().await {
            return err_resp(format!("写入上传文件失败: {e}"));
        }
        saved.push(json!({
            "name": fname,
            "path": dst.to_string_lossy().replace('\\', "/"),
            "size": size,
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

/// 原子占名 + 创建:`create_new` 一步完成「唯一名探测 + 建文件」。
/// 旧写法先 `exists()` 探测再 `File::create`(截断式),两个并发同名上传会探到同一个
/// 「唯一」名 → 互相截断写花文件;`create_new` 撞名返回 AlreadyExists,递增序号重试即可。
async fn create_unique(base: &Path, fname: &str) -> std::io::Result<(PathBuf, tokio::fs::File)> {
    let stem = Path::new(fname)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file")
        .to_string();
    let ext = Path::new(fname)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string());
    let mut i = 0u32;
    loop {
        let cand = if i == 0 {
            base.join(fname)
        } else {
            match &ext {
                Some(e) => base.join(format!("{stem}-{i}.{e}")),
                None => base.join(format!("{stem}-{i}")),
            }
        };
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&cand)
            .await
        {
            Ok(f) => return Ok((cand, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                i += 1;
                continue;
            }
            Err(e) => return Err(e),
        }
    }
}

// ───────────────────────── 受限文件读取（iframe 预览 / 图片）─────────────────────────

#[derive(serde::Deserialize)]
struct FileQuery {
    path: String,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    download: Option<String>,
}

async fn serve_file(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
) -> Response {
    let ctx = match app_ctx(&state, &headers).await {
        Some(c) => Some(c),
        None => resolve_app_auth(&state, q.token.clone()).await,
    };
    let Some(ctx) = ctx else {
        return (StatusCode::UNAUTHORIZED, "未授权").into_response();
    };
    if role_rank(&ctx.role) < 3 {
        return (StatusCode::FORBIDDEN, "文件访问需要 owner 权限").into_response();
    }
    let path = PathBuf::from(&q.path);
    let allowed = allowed_roots();
    let canon = match std::fs::canonicalize(&path) {
        Ok(p) => p,
        Err(_) => return (StatusCode::NOT_FOUND, "文件不存在").into_response(),
    };
    if !allowed
        .iter()
        .any(|root| crate::kb::path_contains(root, &canon))
    {
        return (StatusCode::FORBIDDEN, "路径不在允许范围").into_response();
    }
    let file = match tokio::fs::File::open(&canon).await {
        Ok(f) => f,
        Err(_) => return (StatusCode::NOT_FOUND, "读取失败").into_response(),
    };
    let stream = futures_util::stream::unfold(file, |mut f| async move {
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 64 * 1024];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<_, std::io::Error>(axum::body::Bytes::from(buf)), f))
            }
            Err(e) => Some((Err(e), f)),
        }
    });
    let mut resp = Body::from_stream(stream).into_response();
    if let Ok(v) = header::HeaderValue::from_str(mime_for(&canon)) {
        resp.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    resp.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("private, no-store"),
    );
    let active_content = matches!(
        canon
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "html" | "htm" | "svg" | "js" | "mjs" | "cjs"
    );
    if q.download.as_deref() == Some("1") || active_content {
        let fname = canon
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("download");
        let cd = format!("attachment; filename*=UTF-8''{}", pct_encode(fname));
        if let Ok(v) = header::HeaderValue::from_str(&cd) {
            resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
        }
    }
    resp
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
        let home = u.home_dir().join("Polaris");
        for root in [
            home.join("data/artifacts"),
            home.join("projects"),
            home.join("uploads-inbox"),
        ] {
            if let Ok(c) = std::fs::canonicalize(root) {
                v.push(c);
            }
        }
    }
    v
}

pub(crate) fn mime_for(p: &Path) -> &'static str {
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
