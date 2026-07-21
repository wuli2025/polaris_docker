//! 多人协作 HTTP 路由 —— 双壳共用(Docker server 壳 merge;桌面 hosting 内嵌)。
//!
//! 从 server.rs 整体迁入(逻辑零改动,只把状态从 AppState 换成 CollabState):
//! 鉴权胶水(AuthCtx/resolve_auth/角色闸)+ 全部 /api/collab/* handler + /git/* 反代
//! + WS 推流循环。server 壳经 `collab_router(state, false)` merge 复用;桌面
//! 「一键当主机」(collab/hosting.rs)经 `collab_router(state, true)` 内嵌并自带 /ws。
//!
//! 双轨鉴权(向后兼容):
//!  ① 多用户会话(collab):token 命中 sessions 表 → 得到具体用户与角色,命令过角色闸。
//!  ② 传统全局口令 POLARIS_AUTH_TOKEN:命中即视为 owner(机器管理员,单人 Docker 场景)。
//!  协作启用后(users 表非空)未带任何有效凭据 → 拒绝;未启用协作则维持旧语义。

use crate::host::{AppHandle, Event};
use axum::{
    body::Body,
    extract::{ws::Message, ws::WebSocket, Query, State, WebSocketUpgrade},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use once_cell::sync::Lazy;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::broadcast;

static LEAD_AI_RUNNING: Lazy<parking_lot::Mutex<HashSet<i64>>> =
    Lazy::new(|| parking_lot::Mutex::new(HashSet::new()));

struct LeadAiRunGuard(i64);

impl LeadAiRunGuard {
    fn acquire(project_id: i64) -> Result<Self, String> {
        let mut running = LEAD_AI_RUNNING.lock();
        if !running.insert(project_id) {
            return Err("该项目已有一个主 Agent 请求在运行，请等待完成后重试".into());
        }
        Ok(Self(project_id))
    }
}

impl Drop for LeadAiRunGuard {
    fn drop(&mut self) {
        LEAD_AI_RUNNING.lock().remove(&self.0);
    }
}

/// 协作面状态:与 server::AppState 解耦,桌面内嵌时独立构造。
#[derive(Clone)]
pub struct CollabState {
    /// 事件广播壳(server 壳与其 AppState.app 同源;桌面 hosting 独立频道)
    pub app: AppHandle,
    /// 全局访问口令(POLARIS_AUTH_TOKEN);None = 未设
    pub auth_token: Arc<Option<String>>,
    /// 票据分享码携带的「本机可达地址」(http://ip:port),外壳启动时注入
    pub advertise: Arc<parking_lot::RwLock<Vec<String>>>,
}

/// 探测本机可对外通告的地址。优先 POLARIS_ADVERTISE_URL(逗号分隔,给反代/固定域名用);
/// 否则 UDP connect 技巧取「默认路由出口 IP」与「Tailscale 口 IP」(connect 不发包,零依赖)。
pub fn detect_advertise_urls(port: u16) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(v) = std::env::var("POLARIS_ADVERTISE_URL") {
        for s in v.split(',') {
            let s = s.trim();
            if !s.is_empty() {
                out.push(s.trim_end_matches('/').to_string());
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    let probe = |target: &str| -> Option<std::net::IpAddr> {
        let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        s.connect(target).ok()?;
        s.local_addr().ok().map(|a| a.ip())
    };
    let mut ips: Vec<std::net::IpAddr> = Vec::new();
    if let Some(ip) = probe("8.8.8.8:80") {
        ips.push(ip); // 默认路由(局域网/公网口)
    }
    if let Some(ip) = probe("100.100.100.100:80") {
        if !ips.contains(&ip) {
            ips.push(ip); // Tailscale 口(100.64/10 路由存在才命中)
        }
    }
    for ip in ips {
        if !ip.is_loopback() {
            out.push(format!("http://{ip}:{port}"));
        }
    }
    out
}

// ───────────────────────── 鉴权 ─────────────────────────

/// 一次请求的鉴权结果:是谁、什么角色。
#[derive(Clone)]
pub struct AuthCtx {
    pub user_id: i64, // 0 = 合成身份(全局口令 admin / 本机单人 local)
    pub username: String,
    pub role: String, // owner|collaborator|visitor|lead
}

pub fn bearer_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.strip_prefix("Bearer ").unwrap_or(s).to_string())
        .or_else(|| {
            headers
                .get("x-polaris-token")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        })
}

/// token(header 或 ws query) → AuthCtx。None = 未授权。
pub fn resolve_auth(auth_token: &Option<String>, token: Option<&str>) -> Option<AuthCtx> {
    // 轨② 全局口令 = owner。
    if let Some(expected) = auth_token.as_ref() {
        if token == Some(expected.as_str()) {
            return Some(AuthCtx {
                user_id: 0,
                username: "admin".into(),
                role: "owner".into(),
            });
        }
    }
    // 轨① 会话 token。
    if let Some(t) = token {
        if let Ok(u) = crate::collab::auth::check_session(t) {
            return Some(AuthCtx {
                user_id: u.id,
                username: u.username,
                role: u.role,
            });
        }
    }
    // 未命中就拒绝。零账号/无全局口令也绝不能合成 owner：否则攻击者可先调用
    // owner-only 的票据接口，再 redeem 绕过 bootstrap 初始化口令。
    None
}

/// header → AuthCtx。鉴权的同步 SQLite 查询挪进阻塞线程池:直接跑在 axum async worker
/// 上会钉住 reactor(会话短缓存命中时不落库,这层 spawn_blocking 兜的是未命中/首次)。
pub async fn auth_ctx(state: &CollabState, headers: &HeaderMap) -> Option<AuthCtx> {
    let auth_token = state.auth_token.clone();
    let token = bearer_of(headers);
    tokio::task::spawn_blocking(move || resolve_auth(&auth_token, token.as_deref()))
        .await
        .ok()
        .flatten()
}

pub fn role_rank(role: &str) -> u8 {
    match role {
        "owner" => 3,
        "lead" | "collaborator" => 2,
        "visitor" => 1,
        _ => 0,
    }
}

// ───────────────────────── 小工具 ─────────────────────────

fn s_of(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn i_of(v: &Value, k: &str) -> Option<i64> {
    v.get(k).and_then(|x| x.as_i64())
}

fn forbid() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({"error":"需要 owner 权限"})),
    )
        .into_response()
}

pub fn err_resp(e: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e })),
    )
        .into_response()
}

pub fn ok<T: Serialize>(t: T) -> Result<Value, String> {
    serde_json::to_value(t).map_err(|e| e.to_string())
}

/// spawn_blocking 结果 → HTTP 响应(业务错误 → 400 而非 500,便于前端展示)。
fn unwrap_api(out: Result<Result<Value, String>, tokio::task::JoinError>) -> Response {
    match out {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
        Err(e) => err_resp(format!("内部任务失败: {e}")),
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ───────────────────────── /api/collab 账号与会话 ─────────────────────────
//
// 登录发生在隧道之内(v8 第八节):这些端点本身不再叠全局口令,密码与会话即凭据。
// bootstrap 仅在零账号时可用;admin/* 全部要求 owner 会话。

/// 首启建 owner(仅零账号时放行,防抢注)。
async fn collab_bootstrap(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    // loopback 不是身份边界：恶意网页可通过跨源请求扫描本机端口，本地普通进程也可直连。
    // 所有外壳都必须持有机器初始化口令；桌面内嵌主机会生成随机口令并仅经 Tauri IPC
    // 交给自己的前端，绝不通过 HTTP 状态接口泄露。
    let supplied = bearer_of(&headers);
    let setup_ok = state
        .auth_token
        .as_ref()
        .as_ref()
        .map(|expected| supplied.as_deref() == Some(expected.as_str()))
        .unwrap_or(false);
    if !setup_ok {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"首次初始化需要服务器访问口令"})),
        )
            .into_response();
    }
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let u = crate::collab::auth::create_initial_owner(
            &s_of(&v, "username"),
            &s_of(&v, "password"),
            &s_of(&v, "displayName"),
        )?;
        let (_, token) =
            crate::collab::auth::login(&u.username, &s_of(&v, "password"), &s_of(&v, "deviceId"))?;
        // 「把这台电脑设为主机」流程:主机自己的前端在 bootstrap 时自报 hostSelf,
        // 顺手把本机登记进设备白名单并落 meta,设备页据此点亮「主机」徽标。
        // (NAS/远程 bootstrap 不带 hostSelf → 行为与旧版完全一致。)
        if v.get("hostSelf").and_then(|x| x.as_bool()).unwrap_or(false) {
            let node = s_of(&v, "deviceId");
            if !node.is_empty() {
                let name = std::env::var("COMPUTERNAME")
                    .or_else(|_| std::env::var("HOSTNAME"))
                    .map(|h| format!("{h}(主机)"))
                    .unwrap_or_else(|_| "主机".into());
                let _ = crate::collab::identity::add_device(u.id, &name, &node);
                let _ = crate::collab::db::meta_set("host_node_id", &node);
            }
        }
        Ok(json!({"user": u, "token": token}))
    })
    .await;
    unwrap_api(out)
}

async fn collab_login(Json(v): Json<Value>) -> Response {
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let (u, token) = crate::collab::auth::login(
            &s_of(&v, "username"),
            &s_of(&v, "password"),
            &s_of(&v, "deviceId"),
        )?;
        Ok(json!({"user": u, "token": token}))
    })
    .await;
    unwrap_api(out)
}

async fn collab_logout(headers: HeaderMap) -> Response {
    let token = bearer_of(&headers).unwrap_or_default();
    let out = tokio::task::spawn_blocking(move || {
        crate::collab::auth::logout(&token).map(|_| json!({"ok": true}))
    })
    .await;
    unwrap_api(out)
}

async fn collab_me(State(state): State<CollabState>, headers: HeaderMap) -> Response {
    match auth_ctx(&state, &headers).await {
        Some(ctx) => Json(json!({"username": ctx.username, "role": ctx.role})).into_response(),
        None => (StatusCode::UNAUTHORIZED, Json(json!({"error":"未登录"}))).into_response(),
    }
}

/// 票据兑换(入伙的应用层部分;隧道层白名单在 P2 接入)。两条路,票据都记 used_by:
/// - **已登录**(带有效会话 token 且真实账号):不建新号,把本设备绑到当前账号 —— 保证
///   「同账号的人通过邀请码进来」;返回当前用户 + 原 token。
/// - 未登录:老路径,建账号+登记设备+签会话。
async fn collab_redeem(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    // 已登录路径:合成身份(user_id=0,全局口令)不算 —— 免鉴身份不允许绑设备入伙。
    let logged_in = match auth_ctx(&state, &headers).await {
        Some(ctx) if ctx.user_id > 0 => bearer_of(&headers),
        _ => None,
    };
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        if let Some(token) = logged_in {
            let user = crate::collab::auth::check_session(&token)?;
            crate::collab::identity::redeem_ticket_existing(
                &s_of(&v, "code"),
                &user,
                &s_of(&v, "deviceName"),
                &s_of(&v, "nodeId"),
            )?;
            return Ok(json!({"user": user, "token": token}));
        }
        let (u, token) = crate::collab::identity::redeem_ticket(
            &s_of(&v, "code"),
            &s_of(&v, "username"),
            &s_of(&v, "password"),
            &s_of(&v, "displayName"),
            &s_of(&v, "deviceName"),
            &s_of(&v, "nodeId"),
        )?;
        Ok(json!({"user": u, "token": token}))
    })
    .await;
    unwrap_api(out)
}

async fn collab_ticket(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    // 分享码 = 裸码 + 主机可达地址(启动时探测/环境覆写);成员端粘一串即入伙。
    let advertise = state.advertise.read().clone();
    let out = tokio::task::spawn_blocking(move || {
        let role = {
            let r = s_of(&v, "role");
            if r.is_empty() {
                "collaborator".into()
            } else {
                r
            }
        };
        let t = crate::collab::identity::create_ticket(&role, &s_of(&v, "note"))?;
        let share = crate::collab::identity::encode_share_code(&t.code, &advertise);
        ok(json!({ "code": t.code, "role": t.role, "expires_at": t.expires_at, "share": share }))
    })
    .await;
    unwrap_api(out)
}

async fn collab_users(State(state): State<CollabState>, headers: HeaderMap) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    let out = tokio::task::spawn_blocking(|| crate::collab::auth::list_users().and_then(ok)).await;
    unwrap_api(out)
}

async fn collab_user_disable(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    let out = tokio::task::spawn_blocking(move || {
        let id = v
            .get("userId")
            .and_then(|x| x.as_i64())
            .ok_or("缺 userId")?;
        let dis = v.get("disabled").and_then(|x| x.as_bool()).unwrap_or(true);
        crate::collab::auth::set_user_disabled(id, dis)?;
        // 停用即把该账号名下所有中继挂牌踢下线(关监听/断连/删挂牌行),不留「还能被反代到」的尾巴。
        #[cfg(feature = "collab-net")]
        if dis {
            crate::collab::gateway::unregister_user(id);
        }
        Ok(json!({"ok": true}))
    })
    .await;
    unwrap_api(out)
}

/// owner 直接重置任意用户密码(邮箱没绑/邮件服务没配时的兜底通道)。
/// 改密即踢掉该用户全部旧会话。
async fn collab_admin_user_reset_password(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    let actor = ctx.username.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let id = v
            .get("userId")
            .and_then(|x| x.as_i64())
            .ok_or("缺 userId")?;
        crate::collab::auth::set_password(id, &s_of(&v, "newPassword"))?;
        crate::collab::db::audit(&actor, "user.reset_password", &id.to_string(), "owner 重置");
        Ok(json!({"ok": true}))
    })
    .await;
    unwrap_api(out)
}

// ── 设备遥测(设备联盟 Phase 2):成员设备定时上报资源实况,内存态聚合 ──
// 键 = 上报方 deviceId(登录时登记为 devices.node_id)。只留最新一帧,不落库;
// 设备看板(collab_devices)按 node_id 合并出 stats/stats_at,前端如实渲染 —— 不插值不造假。
static TELEMETRY: Lazy<parking_lot::Mutex<HashMap<String, (Value, i64)>>> =
    Lazy::new(|| parking_lot::Mutex::new(HashMap::new()));
const TELEMETRY_MAX_DEVICES: usize = 256;
const TELEMETRY_MAX_BYTES: usize = 2048;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

async fn collab_telemetry_report(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 1 {
        return forbid();
    }
    let dev = s_of(&v, "deviceId");
    if dev.is_empty() || dev.len() > 128 {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":"deviceId 无效"}))).into_response();
    }
    let stats = v.get("stats").cloned().unwrap_or(Value::Null);
    if !stats.is_object() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":"stats 须为对象"}))).into_response();
    }
    if serde_json::to_string(&stats).map(|s| s.len()).unwrap_or(usize::MAX) > TELEMETRY_MAX_BYTES {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":"stats 过大"}))).into_response();
    }
    // 设备身份绑定:node_id(= deviceId)必须属当前账号(空闲则登记为其设备)。
    // 挡住成员用别台 node_id 顶替上报假数据;owner 手机首次上报即补一台设备行,遥测有卡承载。
    let name = {
        let n = s_of(&v, "name");
        if n.is_empty() {
            format!("{} 的设备", ctx.username)
        } else {
            n.chars().take(48).collect()
        }
    };
    let uid = ctx.user_id;
    let dev_bind = dev.clone();
    let bound = tokio::task::spawn_blocking(move || {
        crate::collab::identity::ensure_device_for(uid, &dev_bind, &name)
    })
    .await;
    match bound {
        Ok(Ok((_, is_new))) => {
            if is_new {
                // 首次登记留痕(只此一次,不随每帧刷 —— 避免 audit 写放大)。
                let actor = ctx.username.clone();
                let d2 = dev.clone();
                tokio::task::spawn_blocking(move || {
                    crate::collab::db::audit(&actor, "device.telemetry", &d2, "设备开始上报资源")
                });
            }
        }
        Ok(Err(e)) => return (StatusCode::FORBIDDEN, Json(json!({"error": e}))).into_response(),
        Err(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"内部错误"}))).into_response()
        }
    }
    {
        let mut map = TELEMETRY.lock();
        // 容量闸:满了先淘汰最旧一帧,防失控设备撑爆内存。
        if !map.contains_key(&dev) && map.len() >= TELEMETRY_MAX_DEVICES {
            if let Some(oldest) = map
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(k, _)| k.clone())
            {
                map.remove(&oldest);
            }
        }
        map.insert(dev, (stats, now_ms()));
    }
    Json(json!({"ok": true})).into_response()
}

async fn collab_devices(State(state): State<CollabState>, headers: HeaderMap) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    let out = tokio::task::spawn_blocking(|| -> Result<Value, String> {
        // 附 is_host:node_id 命中 meta.host_node_id 的那台就是主机(设备页徽标)。
        let host_node = crate::collab::db::meta_get("host_node_id").unwrap_or_default();
        let list = crate::collab::identity::list_devices()?;
        // 遥测合并:node_id(= 登录时的 deviceId)命中上报表 → 附最新一帧 stats。
        let telem = TELEMETRY.lock().clone();
        let out: Vec<Value> = list
            .into_iter()
            .map(|d| {
                let is_host = !host_node.is_empty() && d.node_id == host_node;
                let mut j = serde_json::to_value(&d).unwrap_or_else(|_| json!({}));
                if is_host {
                    j["is_host"] = json!(true);
                }
                if let Some((stats, at)) = telem.get(&d.node_id) {
                    j["stats"] = stats.clone();
                    j["stats_at"] = json!(at);
                }
                j
            })
            .collect();
        ok(out)
    })
    .await;
    unwrap_api(out)
}

/// 最近活动(audit 表,owner):设备接入/上报/吊销、账号事件 —— 「正在发生」tab 数据源。
async fn collab_audit_recent(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    let limit = q
        .get("limit")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(50)
        .clamp(1, 200);
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        ok(crate::collab::db::audit_recent(limit)?)
    })
    .await;
    unwrap_api(out)
}

async fn collab_device_revoke(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    let out = tokio::task::spawn_blocking(move || {
        let node_id = crate::collab::identity::revoke_device(&s_of(&v, "deviceId"))?;
        // 吊销即把这台设备在中继上的挂牌踢下线(若有);反代闸随后也会拒它。
        #[cfg(feature = "collab-net")]
        crate::collab::gateway::unregister_node(&node_id);
        #[cfg(not(feature = "collab-net"))]
        let _ = node_id;
        Ok(json!({"ok": true}))
    })
    .await;
    unwrap_api(out)
}

// ── GitHub 式注册与团队 ──

/// 注册:**默认邀请/票据制**(安全默认)。显式 POLARIS_OPEN_SIGNUP=1 才开放自助注册。
/// 为什么默认关:开放注册 = 陌生人可自助入伙,而入伙后能建项目、push 仓库,checks 会在主机上
/// 跑该仓库的构建(npm run build 直接执行 package.json 脚本;cargo check 会运行 build.rs 与
/// proc-macro)—— 本质是「在主机上执行不可信代码」= 主机 RCE。故收紧为邀请制,把能 push 代码的
/// 人限定在管理员亲自发票据请进来的可信成员。
async fn collab_signup(Json(v): Json<Value>) -> Response {
    let open = std::env::var("POLARIS_OPEN_SIGNUP")
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if !open {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"本主机为邀请制,请找管理员要邀请票据"})),
        )
            .into_response();
    }
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let u = crate::collab::auth::create_user(
            &s_of(&v, "username"),
            &s_of(&v, "password"),
            "collaborator",
            &s_of(&v, "displayName"),
        )?;
        let (_, token) =
            crate::collab::auth::login(&u.username, &s_of(&v, "password"), &s_of(&v, "deviceId"))?;
        Ok(json!({"user": u, "token": token}))
    })
    .await;
    unwrap_api(out)
}

// ── 邮箱注册 / 找回密码(验证码 = 邮箱所有权证明) ──

/// 邮箱服务状态(公开,无鉴权):前端据此决定「注册」入口亮不亮、「忘记密码」走不走邮箱。
async fn collab_email_status() -> Response {
    // configured/signup_open 都要读 collab.db(meta 表),放阻塞池。
    let out = tokio::task::spawn_blocking(|| -> Result<Value, String> {
        Ok(json!({
            "configured": crate::collab::mail::configured(),
            "signupOpen": crate::collab::mail::signup_open(),
        }))
    })
    .await;
    unwrap_api(out)
}

/// 发验证码。purpose=signup:邮箱须未被绑定;purpose=reset:邮箱须已绑定账号。
/// 频控在 mail::issue_code(60s 重发间隔 + 每小时 5 封)。
async fn collab_email_send_code(Json(v): Json<Value>) -> Response {
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let email = s_of(&v, "email");
        let purpose = s_of(&v, "purpose");
        match purpose.as_str() {
            "signup" => {
                if !crate::collab::mail::signup_open() {
                    return Err("本主机未开放邮箱注册,请联系管理员或用邀请票据入伙".into());
                }
                if crate::collab::auth::find_user_by_email(&email)?.is_some() {
                    return Err("该邮箱已注册过账号,请直接登录或走「忘记密码」".into());
                }
            }
            "reset" => {
                if !crate::collab::mail::configured() {
                    return Err("邮箱服务未配置,无法自助找回,请联系管理员重置密码".into());
                }
                if crate::collab::auth::find_user_by_email(&email)?.is_none() {
                    return Err("该邮箱没有绑定任何账号".into());
                }
            }
            _ => return Err("purpose 须为 signup 或 reset".into()),
        }
        crate::collab::mail::issue_code(&email, &purpose)?;
        Ok(json!({"ok": true}))
    })
    .await;
    unwrap_api(out)
}

/// 邮箱验证码注册:验证通过即建号(collaborator)+ 绑邮箱 + 签会话,一步登录。
async fn collab_email_signup(Json(v): Json<Value>) -> Response {
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        if !crate::collab::mail::signup_open() {
            return Err("本主机未开放邮箱注册,请联系管理员或用邀请票据入伙".into());
        }
        let email = s_of(&v, "email");
        crate::collab::mail::verify_code(&email, "signup", &s_of(&v, "code"))?;
        let username = s_of(&v, "username");
        let display = {
            let d = s_of(&v, "displayName");
            if d.trim().is_empty() {
                username.clone()
            } else {
                d
            }
        };
        let u = crate::collab::auth::create_user_with_email(
            &username,
            &s_of(&v, "password"),
            "collaborator",
            &display,
            &email,
        )?;
        let (_, token) =
            crate::collab::auth::login(&u.username, &s_of(&v, "password"), &s_of(&v, "deviceId"))?;
        Ok(json!({"user": u, "token": token}))
    })
    .await;
    unwrap_api(out)
}

/// 邮箱找回密码:验证码通过即换口令,该账号旧会话全部作废。回 username 提示用哪个名登录。
async fn collab_email_reset(Json(v): Json<Value>) -> Response {
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let email = s_of(&v, "email");
        crate::collab::mail::verify_code(&email, "reset", &s_of(&v, "code"))?;
        let u = crate::collab::auth::find_user_by_email(&email)?
            .ok_or("该邮箱没有绑定任何账号")?;
        crate::collab::auth::set_password(u.id, &s_of(&v, "newPassword"))?;
        Ok(json!({"ok": true, "username": u.username}))
    })
    .await;
    unwrap_api(out)
}

/// owner 读邮箱服务配置。SMTP 授权码永不回显,只回 passSet(是否已配置)。
async fn collab_admin_email_config_get(
    State(state): State<CollabState>,
    headers: HeaderMap,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    let out = tokio::task::spawn_blocking(|| -> Result<Value, String> {
        let cfg = crate::collab::mail::config();
        let (host, port, user, from) = match &cfg {
            Some(c) => (c.host.clone(), c.port, c.user.clone(), c.from.clone()),
            None => (
                crate::collab::db::meta_get("smtp_host").unwrap_or_else(|| "smtp.qq.com".into()),
                crate::collab::db::meta_get("smtp_port")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(465),
                crate::collab::db::meta_get("smtp_user").unwrap_or_default(),
                crate::collab::db::meta_get("smtp_from").unwrap_or_default(),
            ),
        };
        Ok(json!({
            "configured": cfg.is_some(),
            "host": host,
            "port": port,
            "user": user,
            "from": from,
            "passSet": cfg.is_some(),
            "signupOpen": crate::collab::mail::signup_open(),
        }))
    })
    .await;
    unwrap_api(out)
}

/// owner 写邮箱服务配置(pass 留空 = 保留旧授权码);testTo 非空则顺手发一封测试信验通。
async fn collab_admin_email_config_set(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    let actor = ctx.username.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let user = s_of(&v, "user");
        if user.trim().is_empty() {
            return Err("请填发信邮箱(如 1799820934@qq.com)".into());
        }
        let host = {
            let h = s_of(&v, "host");
            if h.trim().is_empty() { "smtp.qq.com".into() } else { h }
        };
        let port = i_of(&v, "port").unwrap_or(465).clamp(1, 65535) as u16;
        let from = {
            let f = s_of(&v, "from");
            if f.trim().is_empty() { user.clone() } else { f }
        };
        let signup = v.get("signupOpen").and_then(|x| x.as_bool()).unwrap_or(true);
        crate::collab::mail::save_config(&host, port, &user, &s_of(&v, "pass"), &from, signup)?;
        crate::collab::db::audit(&actor, "email.config", &user, "SMTP 配置更新");
        let test_to = s_of(&v, "testTo");
        if !test_to.trim().is_empty() {
            crate::collab::mail::send_mail(
                test_to.trim(),
                "【Polaris】邮箱服务测试",
                "看到这封信,说明 Polaris 的 SMTP 邮箱服务已配置成功。",
            )?;
        }
        Ok(json!({"ok": true, "configured": crate::collab::mail::configured()}))
    })
    .await;
    unwrap_api(out)
}

/// 用户名搜索(拉人自动补全)。登录即可用,只回不敏感字段。
async fn collab_user_search(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(_ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let kw = q.get("q").cloned().unwrap_or_default();
    let out =
        tokio::task::spawn_blocking(move || crate::collab::teams::search_users(&kw).and_then(ok))
            .await;
    unwrap_api(out)
}

async fn team_list(State(state): State<CollabState>, headers: HeaderMap) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let out = tokio::task::spawn_blocking(move || {
        crate::collab::teams::list_mine(ctx.user_id).and_then(ok)
    })
    .await;
    unwrap_api(out)
}

/// 建团队:任何登录用户(访问者除外)。
async fn team_create(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 2 {
        return forbid();
    }
    let out = tokio::task::spawn_blocking(move || {
        crate::collab::teams::create(&s_of(&v, "name"), ctx.user_id, &ctx.username).and_then(ok)
    })
    .await;
    unwrap_api(out)
}

/// 团队管理权:全局 owner 或该团队 owner。
fn is_team_admin(ctx: &AuthCtx, team_id: i64) -> bool {
    role_rank(&ctx.role) >= 3
        || crate::collab::teams::my_role(team_id, ctx.user_id).as_deref() == Some("owner")
}

async fn team_members_api(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let Some(tid) = q.get("teamId").and_then(|s| s.parse::<i64>().ok()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":"缺 teamId"}))).into_response();
    };
    // 团队成员可看成员列表
    if role_rank(&ctx.role) < 3 && crate::collab::teams::my_role(tid, ctx.user_id).is_none() {
        return (StatusCode::FORBIDDEN, Json(json!({"error":"你不在该团队"}))).into_response();
    }
    let out =
        tokio::task::spawn_blocking(move || crate::collab::teams::members(tid).and_then(ok)).await;
    unwrap_api(out)
}

/// 按用户名拉人(GitHub 式邀请)。团队 owner 专属。
async fn team_member_add(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let Some(tid) = i_of(&v, "teamId") else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":"缺 teamId"}))).into_response();
    };
    if !is_team_admin(&ctx, tid) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"只有团队管理者能拉人"})),
        )
            .into_response();
    }
    let out = tokio::task::spawn_blocking(move || {
        let name = crate::collab::teams::add_member_by_username(
            tid,
            &s_of(&v, "username"),
            &s_of(&v, "role"),
            &ctx.username,
        )?;
        Ok(json!({"ok": true, "username": name}))
    })
    .await;
    unwrap_api(out)
}

async fn team_member_remove(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let (Some(tid), Some(uid)) = (i_of(&v, "teamId"), i_of(&v, "userId")) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"缺 teamId/userId"})),
        )
            .into_response();
    };
    // 自己可以退出;移除别人要团队管理权
    if uid != ctx.user_id && !is_team_admin(&ctx, tid) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"只有团队管理者能移除成员"})),
        )
            .into_response();
    }
    let out = tokio::task::spawn_blocking(move || {
        crate::collab::teams::remove_member(tid, uid, &ctx.username).map(|_| json!({"ok": true}))
    })
    .await;
    unwrap_api(out)
}

// ── 账号镜像(云端双存) ──

/// 主机侧:导出加密镜像(owner)。返回 blob,调用方可 POST 给云端 /mirror/store。
async fn mirror_export_api(State(state): State<CollabState>, headers: HeaderMap) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    let out = tokio::task::spawn_blocking(|| {
        crate::collab::account_store::export_blob().map(|b| json!({"blob": b}))
    })
    .await;
    unwrap_api(out)
}

/// 云端侧:保管密文(owner 会话或全局口令)。云端解不开,只做保管。
async fn mirror_store_api(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    let out = tokio::task::spawn_blocking(move || {
        crate::collab::account_store::store_remote_blob(&s_of(&v, "blob"))
            .map(|ver| json!({"ok": true, "version": ver}))
    })
    .await;
    unwrap_api(out)
}

async fn mirror_pull_api(State(state): State<CollabState>, headers: HeaderMap) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    let out = tokio::task::spawn_blocking(|| {
        crate::collab::account_store::load_blob().map(|o| match o {
            Some((ver, blob)) => json!({"version": ver, "blob": blob}),
            None => json!({"version": 0, "blob": null}),
        })
    })
    .await;
    unwrap_api(out)
}

/// 恢复(空库 + 持有 host.key 的机器上才可能成功;account_store 内部还有防覆盖闸)。
async fn mirror_restore_api(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    // 灾后恢复也必须持机器访问口令；空库不再自动合成 owner。
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    let out = tokio::task::spawn_blocking(move || {
        crate::collab::account_store::restore_blob(&s_of(&v, "blob"))
            .map(|n| json!({"ok": true, "restored": n}))
    })
    .await;
    unwrap_api(out)
}

// ── 项目与任务卡 ──
//
// 权限模型(确定性两问):① 全局角色够不够(role_rank);② 是不是项目成员
// (project_members 表;全局 owner 与合成 owner 免查——他们是主机管理员)。

/// 项目成员校验:owner 直通;其余必须在成员表。
fn ensure_member(ctx: &AuthCtx, project_id: i64) -> Result<(), Response> {
    if role_rank(&ctx.role) >= 3 {
        return Ok(());
    }
    if crate::collab::projects::member_role(project_id, ctx.user_id).is_some() {
        return Ok(());
    }
    Err((
        StatusCode::FORBIDDEN,
        Json(json!({"error":"你不是该项目成员"})),
    )
        .into_response())
}

/// 建项目会授予后续 git/check/merge 能力，因此只允许主机管理员绑定本地仓库。
async fn project_create(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let team_id = i_of(&v, "teamId");
    if role_rank(&ctx.role) < 3 {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"只有主机管理员能创建项目并绑定本地仓库"})),
        )
            .into_response();
    }
    let out = tokio::task::spawn_blocking(move || {
        let repo = validate_repo_path(&s_of(&v, "repo"))?;
        crate::collab::projects::create(
            &s_of(&v, "name"),
            &repo.to_string_lossy(),
            team_id,
            ctx.user_id,
            &ctx.username,
        )
        .and_then(ok)
    })
    .await;
    unwrap_api(out)
}

async fn project_list(State(state): State<CollabState>, headers: HeaderMap) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let is_owner = role_rank(&ctx.role) >= 3;
    let out = tokio::task::spawn_blocking(move || {
        crate::collab::projects::list_for(ctx.user_id, is_owner).and_then(ok)
    })
    .await;
    unwrap_api(out)
}

async fn project_members(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let Some(pid) = q.get("projectId").and_then(|s| s.parse::<i64>().ok()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"缺 projectId"})),
        )
            .into_response();
    };
    if let Err(r) = ensure_member(&ctx, pid) {
        return r;
    }
    let out =
        tokio::task::spawn_blocking(move || crate::collab::projects::members(pid).and_then(ok))
            .await;
    unwrap_api(out)
}

/// 加项目成员:项目/团队管理者即可;支持 userId 或 username(GitHub 式搜索拉人)。
async fn project_member_add(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let Some(pid) = i_of(&v, "projectId") else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"缺 projectId"})),
        )
            .into_response();
    };
    let global_owner = role_rank(&ctx.role) >= 3;
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        if !crate::collab::projects::can_admin(pid, ctx.user_id, global_owner) {
            return Err("只有项目/团队管理者能加成员".into());
        }
        let role = {
            let r = s_of(&v, "role");
            if r.is_empty() {
                "collaborator".into()
            } else {
                r
            }
        };
        if let Some(uid) = i_of(&v, "userId") {
            crate::collab::projects::add_member(pid, uid, &role, &ctx.username)?;
        } else {
            let uname = s_of(&v, "username");
            if uname.is_empty() {
                return Err("缺 userId 或 username".into());
            }
            crate::collab::projects::add_member_by_username(pid, &uname, &role, &ctx.username)?;
        }
        Ok(json!({"ok": true}))
    })
    .await;
    unwrap_api(out)
}

async fn project_set_lead(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let global_owner = role_rank(&ctx.role) >= 3;
    let out = tokio::task::spawn_blocking(move || {
        let pid = i_of(&v, "projectId").ok_or("缺 projectId")?;
        if !crate::collab::projects::can_admin(pid, ctx.user_id, global_owner) {
            return Err("只有项目/团队管理者能任命主 Agent".into());
        }
        let expert = s_of(&v, "expertId");
        let expert = if expert.is_empty() {
            None
        } else {
            Some(expert)
        };
        crate::collab::projects::set_lead(pid, expert.as_deref(), &ctx.username)
            .map(|_| json!({"ok": true}))
    })
    .await;
    unwrap_api(out)
}

/// 设项目共享可见路径(CSV):协作者开工时并入稀疏集(scope ∪ shared_scope)。管理者专属。
async fn project_shared_scope_set(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let global_owner = role_rank(&ctx.role) >= 3;
    let out = tokio::task::spawn_blocking(move || {
        let pid = i_of(&v, "projectId").ok_or("缺 projectId")?;
        if !crate::collab::projects::can_admin(pid, ctx.user_id, global_owner) {
            return Err("只有项目/团队管理者能改共享可见路径".into());
        }
        crate::collab::projects::set_shared_scope(pid, &s_of(&v, "sharedScope"), &ctx.username)
            .map(|_| json!({"ok": true}))
    })
    .await;
    unwrap_api(out)
}

async fn task_list(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let Some(pid) = q.get("projectId").and_then(|s| s.parse::<i64>().ok()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"缺 projectId"})),
        )
            .into_response();
    };
    if let Err(r) = ensure_member(&ctx, pid) {
        return r;
    }
    let out =
        tokio::task::spawn_blocking(move || crate::collab::tasks::list(pid).and_then(ok)).await;
    unwrap_api(out)
}

/// 任务卡变更统一广播(看板实时刷新)。项目级事件,广播即可;个人定向流走 emit_to。
fn emit_task(state: &CollabState, card: &crate::collab::tasks::TaskCard) {
    let _ = state.app.emit("collab:task", card);
}

// ── 任务级对话(多轮微调通道,详见 chat.rs)──

/// 消息鉴权 + 身份归类:项目成员可读写;返回 (card, 消息 role)。
fn chat_access(
    ctx: &AuthCtx,
    task_id: i64,
) -> Result<(crate::collab::tasks::TaskCard, String), Response> {
    let card = crate::collab::tasks::get(task_id)
        .map_err(|e| (StatusCode::NOT_FOUND, Json(json!({"error": e}))).into_response())?;
    ensure_member(ctx, card.project_id)?;
    let global_owner = role_rank(&ctx.role) >= 3;
    let role = if card.assignee == Some(ctx.user_id) {
        "assignee"
    } else if crate::collab::projects::can_admin(card.project_id, ctx.user_id, global_owner) {
        "lead"
    } else {
        "member"
    };
    Ok((card, role.to_string()))
}

async fn task_messages_get(
    State(state): State<CollabState>,
    headers: HeaderMap,
    axum::extract::Path(tid): axum::extract::Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let after_id = q
        .get("afterId")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    let limit = q
        .get("limit")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(100);
    let out = tokio::task::spawn_blocking(move || -> Result<Value, Response> {
        chat_access(&ctx, tid)?;
        crate::collab::chat::list(tid, after_id, limit)
            .and_then(ok)
            .map_err(|e| (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response())
    })
    .await;
    match out {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(resp)) => resp,
        Err(e) => err_resp(format!("内部任务失败: {e}")),
    }
}

async fn task_messages_post(
    State(state): State<CollabState>,
    headers: HeaderMap,
    axum::extract::Path(tid): axum::extract::Path<i64>,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 2 {
        return forbid();
    }
    let st = state.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<Value, Response> {
        let (_card, role) = chat_access(&ctx, tid)?;
        let idem = v.get("idemKey").and_then(|k| k.as_str());
        let msg = crate::collab::chat::post(
            tid,
            ctx.user_id,
            &ctx.username,
            &role,
            &s_of(&v, "body"),
            idem,
        )
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response())?;
        // 项目级广播:任务抽屉聊天面板实时追加(/ws 与看板事件同通道)。
        let _ = st.app.emit("collab:task_message", &msg);
        ok(msg).map_err(|e| err_resp(e))
    })
    .await;
    match out {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(resp)) => resp,
        Err(e) => err_resp(format!("内部任务失败: {e}")),
    }
}

/// 主 Agent 在任务对话里回一条(lead/管理者手动触发,控制 token 用量)。
/// AI 输出仍只是建议:写进消息流,不碰状态机。
async fn task_ai_reply(
    State(state): State<CollabState>,
    headers: HeaderMap,
    axum::extract::Path(tid): axum::extract::Path<i64>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let st = state.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let card = crate::collab::tasks::get(tid)?;
        let global_owner = role_rank(&ctx.role) >= 3;
        if !crate::collab::projects::can_admin(card.project_id, ctx.user_id, global_owner) {
            return Err("只有项目/团队管理者能触发主 Agent 回复".into());
        }
        let _run_guard = LeadAiRunGuard::acquire(card.project_id)?;
        let text = crate::collab::lead_ai::ai_task_reply(card.project_id, tid)?;
        let msg = crate::collab::chat::post(tid, 0, "主Agent", "ai", &text, None)?;
        let _ = st.app.emit("collab:task_message", &msg);
        ok(msg)
    })
    .await;
    unwrap_api(out)
}

async fn task_create(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let Some(pid) = i_of(&v, "projectId") else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"缺 projectId"})),
        )
            .into_response();
    };
    if role_rank(&ctx.role) < 2 {
        return forbid();
    }
    if let Err(r) = ensure_member(&ctx, pid) {
        return r;
    }
    let st = state.clone();
    let out = tokio::task::spawn_blocking(move || {
        let card = crate::collab::tasks::create(
            pid,
            &s_of(&v, "title"),
            &s_of(&v, "body"),
            &s_of(&v, "scope"),
            &s_of(&v, "criteria"),
            &ctx.username,
        )?;
        emit_task(&st, &card);
        ok(card)
    })
    .await;
    unwrap_api(out)
}

/// 卡操作通用骨架:取卡→项目成员校验→执行→广播。
macro_rules! task_op {
    ($fn_name:ident, $body:expr) => {
        async fn $fn_name(State(state): State<CollabState>, headers: HeaderMap, Json(v): Json<Value>) -> Response {
            let Some(ctx) = auth_ctx(&state, &headers).await else { return forbid(); };
            if role_rank(&ctx.role) < 2 { return forbid(); }
            let Some(tid) = i_of(&v, "taskId") else {
                return (StatusCode::BAD_REQUEST, Json(json!({"error":"缺 taskId"}))).into_response();
            };
            let pid = match tokio::task::spawn_blocking(move || crate::collab::tasks::get(tid).map(|c| c.project_id)).await {
                Ok(Ok(p)) => p,
                Ok(Err(e)) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
                Err(e) => return err_resp(e.to_string()),
            };
            if let Err(r) = ensure_member(&ctx, pid) { return r; }
            let st = state.clone();
            let op: fn(i64, &AuthCtx, &Value, &CollabState) -> Result<Value, String> = $body;
            let out = tokio::task::spawn_blocking(move || op(tid, &ctx, &v, &st)).await;
            unwrap_api(out)
        }
    };
}

task_op!(task_claim, |tid, ctx, _v, st| {
    let card = crate::collab::tasks::claim(tid, ctx.user_id, &ctx.username)?;
    emit_task(st, &card);
    ok(card)
});

task_op!(task_submit, |tid, ctx, v, st| {
    let before = crate::collab::tasks::get(tid)?;
    let can_admin = crate::collab::projects::can_admin(
        before.project_id,
        ctx.user_id,
        role_rank(&ctx.role) >= 3,
    );
    if before.assignee != Some(ctx.user_id) && !can_admin {
        return Err("只有任务负责人或项目管理者能提交该任务".into());
    }
    let card = crate::collab::tasks::submit(tid, i_of(v, "prId"), &ctx.username)?;
    emit_task(st, &card);
    // 触发本轮检查(后台线程,不阻塞提交响应;结果经 collab:check 推送)。
    spawn_checks(st, &card);
    ok(card)
});

/// 后台线程跑一轮检查(提交触发/手动重跑共用):结果落 check_runs,进度经 collab:check 推送。
fn spawn_checks(st: &CollabState, card: &crate::collab::tasks::TaskCard) {
    let card2 = card.clone();
    let app2 = st.app.clone();
    std::thread::spawn(move || {
        let profile = crate::collab::checks::project_profile(card2.project_id);
        let repo = match project_repo_path(card2.project_id) {
            Ok(r) => r,
            Err(_) => return, // 项目没配仓库 → 无从检查,静默跳过
        };
        let emit = || {
            let _ = app2.emit(
                "collab:check",
                serde_json::json!({"taskId": card2.id, "round": card2.round}),
            );
        };
        let _ = crate::collab::checks::run_for_task(
            &repo,
            &card2.branch,
            card2.id,
            card2.round,
            &profile,
            &emit,
        );
    });
}

task_op!(task_review, |tid, ctx, v, st| {
    // 验收权=项目/团队管理者(协作者不能给自己验收)。asLead 也只能由管理者触发——
    // 落档 actor 记 lead:<expert>,且额外过 lead.rs 三问(任命/授权位/预算)。
    let card = crate::collab::tasks::get(tid)?;
    if !crate::collab::projects::can_admin(card.project_id, ctx.user_id, role_rank(&ctx.role) >= 3)
    {
        return Err("只有项目/团队管理者能验收".into());
    }
    if card.assignee == Some(ctx.user_id) {
        return Err("任务负责人不能验收自己提交的任务，请由另一位项目管理者复核".into());
    }
    let pass = v.get("pass").and_then(|x| x.as_bool()).ok_or("缺 pass")?;
    let comments = v
        .get("comments")
        .map(|c| c.to_string())
        .unwrap_or_else(|| "[]".into());
    let as_lead = v.get("asLead").and_then(|x| x.as_bool()).unwrap_or(false);
    let outc = if as_lead {
        crate::collab::lead::lead_review(card.project_id, tid, pass, &comments)?
    } else {
        crate::collab::tasks::review(tid, &ctx.username, pass, &comments)?
    };
    emit_task(st, &outc.card);
    if outc.escalated {
        // 打回熔断:抄送 owner(飞书通道接入前先走面板事件)。
        let _ = st.app.emit(
            "collab:escalate",
            serde_json::json!({"taskId": tid, "round": outc.card.round}),
        );
    }
    ok(outc)
});

task_op!(task_archive, |tid, ctx, _v, st| {
    let before = crate::collab::tasks::get(tid)?;
    if !crate::collab::projects::can_admin(
        before.project_id,
        ctx.user_id,
        role_rank(&ctx.role) >= 3,
    ) {
        return Err("只有项目管理者能归档任务".into());
    }
    let card = crate::collab::tasks::archive(tid, &ctx.username)?;
    emit_task(st, &card);
    ok(card)
});

task_op!(task_cancel, |tid, ctx, _v, st| {
    let before = crate::collab::tasks::get(tid)?;
    if !crate::collab::projects::can_admin(
        before.project_id,
        ctx.user_id,
        role_rank(&ctx.role) >= 3,
    ) {
        return Err("只有项目管理者能取消任务".into());
    }
    let card = crate::collab::tasks::cancel(tid, &ctx.username)?;
    emit_task(st, &card);
    ok(card)
});

async fn task_rounds(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let Some(tid) = q.get("taskId").and_then(|s| s.parse::<i64>().ok()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":"缺 taskId"}))).into_response();
    };
    let pid = match tokio::task::spawn_blocking(move || {
        crate::collab::tasks::get(tid).map(|c| c.project_id)
    })
    .await
    {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
        Err(e) => return err_resp(e.to_string()),
    };
    if let Err(r) = ensure_member(&ctx, pid) {
        return r;
    }
    let out =
        tokio::task::spawn_blocking(move || crate::collab::tasks::rounds(tid).and_then(ok)).await;
    unwrap_api(out)
}

/// 项目动态时间线(GitHub activity feed 式,项目主页概览 tab 数据源)。
async fn collab_activity(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let Some(pid) = q.get("projectId").and_then(|s| s.parse::<i64>().ok()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"缺 projectId"})),
        )
            .into_response();
    };
    if let Err(r) = ensure_member(&ctx, pid) {
        return r;
    }
    let limit = q
        .get("limit")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(30)
        .clamp(1, 100);
    let out = tokio::task::spawn_blocking(move || {
        crate::collab::tasks::activity(pid, limit).and_then(ok)
    })
    .await;
    unwrap_api(out)
}

// ── 检查工作流(CI-lite,GitHub status checks 式)──

/// GET /api/collab/checks?taskId= → {profile, round, runs}(round 取卡当前轮)。
async fn checks_get(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let Some(tid) = q.get("taskId").and_then(|s| s.parse::<i64>().ok()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":"缺 taskId"}))).into_response();
    };
    let pid = match tokio::task::spawn_blocking(move || {
        crate::collab::tasks::get(tid).map(|c| c.project_id)
    })
    .await
    {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
        Err(e) => return err_resp(e.to_string()),
    };
    if let Err(r) = ensure_member(&ctx, pid) {
        return r;
    }
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        crate::collab::checks::sweep_stale_running(); // 崩溃残留的 running 行超时清掉,前端不永久转圈
        let card = crate::collab::tasks::get(tid)?;
        let profile = crate::collab::checks::project_profile(card.project_id);
        let check_skill = crate::collab::checks::project_check_skill(card.project_id);
        // 用检查实际落库的最大轮次,而非 card.round(review 通过会 +1 令其漂移;见
        // checks::latest_round)。没跑过检查则回落 card.round(runs 为空,前端不显示徽章)。
        let rnd = crate::collab::checks::latest_round(tid).unwrap_or(card.round);
        let runs = crate::collab::checks::list(tid, rnd)?;
        Ok(json!({"profile": profile, "checkSkill": check_skill, "round": rnd, "runs": runs}))
    })
    .await;
    unwrap_api(out)
}

// 手动重跑本轮检查(成员即可;复用提交触发的后台线程逻辑)。
task_op!(checks_rerun, |tid, _ctx, _v, st| {
    let card = crate::collab::tasks::get(tid)?;
    if card.branch.is_empty() {
        return Err("任务尚未开分支,无从检查".into());
    }
    spawn_checks(st, &card);
    ok(json!({"ok": true}))
});

/// 检查档位设置(code/creative/off)——项目/团队管理者专属。
async fn checks_profile_set(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let global_owner = role_rank(&ctx.role) >= 3;
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let pid = i_of(&v, "projectId").ok_or("缺 projectId")?;
        if !crate::collab::projects::can_admin(pid, ctx.user_id, global_owner) {
            return Err("只有项目/团队管理者能改检查档位".into());
        }
        let profile = s_of(&v, "profile");
        if !profile.is_empty() {
            crate::collab::checks::set_project_profile(pid, &profile)?;
            crate::collab::db::audit(&ctx.username, "checks.profile", &pid.to_string(), &profile);
        }
        // 可选:同请求顺带设检查技能(前端设置面板一次提交)。传空串=回到默认内置技能。
        if let Some(skill) = v.get("checkSkill").and_then(|s| s.as_str()) {
            crate::collab::checks::set_project_check_skill(pid, skill)?;
            crate::collab::db::audit(&ctx.username, "checks.skill", &pid.to_string(), skill);
        }
        Ok(json!({"ok": true}))
    })
    .await;
    unwrap_api(out)
}

/// 主机本机已安装、可用作检查项的技能清单(检查设置下拉)。
async fn checks_skills_list(State(state): State<CollabState>, headers: HeaderMap) -> Response {
    let Some(_ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let items: Vec<Value> = crate::skills::list_check_capable()
            .into_iter()
            .map(|(id, name)| json!({"id": id, "name": name}))
            .collect();
        Ok(json!({"skills": items, "default": crate::skills::PROJECT_CHECK_ID}))
    })
    .await;
    unwrap_api(out)
}

// ── 主 Agent 授权位 + 晨会 ──

async fn lead_grants_get(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let Some(pid) = q.get("projectId").and_then(|s| s.parse::<i64>().ok()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"缺 projectId"})),
        )
            .into_response();
    };
    if let Err(r) = ensure_member(&ctx, pid) {
        return r;
    }
    let out =
        tokio::task::spawn_blocking(move || crate::collab::lead::get_grants(pid).and_then(ok))
            .await;
    unwrap_api(out)
}

/// 授权位只有项目/团队管理者能改——这是"owner 可随时收权/降档"的落点。
async fn lead_grants_set(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let global_owner = role_rank(&ctx.role) >= 3;
    let out = tokio::task::spawn_blocking(move || {
        let pid = i_of(&v, "projectId").ok_or("缺 projectId")?;
        if !crate::collab::projects::can_admin(pid, ctx.user_id, global_owner) {
            return Err("只有项目/团队管理者能改主 Agent 授权位".into());
        }
        let g: crate::collab::lead::LeadGrants =
            serde_json::from_value(v.get("grants").cloned().ok_or("缺 grants")?)
                .map_err(|e| e.to_string())?;
        crate::collab::lead::set_grants(pid, &g, &ctx.username).map(|_| json!({"ok": true}))
    })
    .await;
    unwrap_api(out)
}

async fn lead_morning(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let Some(pid) = q.get("projectId").and_then(|s| s.parse::<i64>().ok()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"缺 projectId"})),
        )
            .into_response();
    };
    if let Err(r) = ensure_member(&ctx, pid) {
        return r;
    }
    let out =
        tokio::task::spawn_blocking(move || crate::collab::lead::morning_report(pid).and_then(ok))
            .await;
    unwrap_api(out)
}

/// 主 Agent 改派(指挥件②):管理者触发,真正的授权判定在 lead.rs 三问(须 can_reassign 位)。
async fn lead_assign_api(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let st = state.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let tid = i_of(&v, "taskId").ok_or("缺 taskId")?;
        let uid = i_of(&v, "userId").ok_or("缺 userId")?;
        let card = crate::collab::tasks::get(tid)?;
        if !crate::collab::projects::can_admin(
            card.project_id,
            ctx.user_id,
            role_rank(&ctx.role) >= 3,
        ) {
            return Err("只有项目/团队管理者能触发主 Agent 改派".into());
        }
        let card = crate::collab::lead::lead_assign(card.project_id, tid, uid)?;
        emit_task(&st, &card);
        ok(card)
    })
    .await;
    unwrap_api(out)
}

/// 主 Agent 催办(指挥件⑥):盘出超期无动静的卡并留痕,返回催办清单。
async fn lead_nudge_api(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let pid = i_of(&v, "projectId").ok_or("缺 projectId")?;
        if !crate::collab::projects::can_admin(pid, ctx.user_id, role_rank(&ctx.role) >= 3) {
            return Err("只有项目/团队管理者能触发催办".into());
        }
        let hours = i_of(&v, "staleHours").unwrap_or(48).max(1);
        crate::collab::lead::lead_nudge(pid, hours).and_then(ok)
    })
    .await;
    unwrap_api(out)
}

// ── 主 Agent AI ──

async fn lead_model_get(State(state): State<CollabState>, headers: HeaderMap) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    let mut cfg = crate::collab::lead_ai::load_cfg();
    // 密钥脱敏:只回是否已配置。
    if !cfg.api_key.is_empty() {
        cfg.api_key = "•••".into();
    }
    Json(serde_json::to_value(cfg).unwrap_or_default()).into_response()
}

async fn lead_model_set(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let mut cfg: crate::collab::lead_ai::LeadModelCfg =
            serde_json::from_value(v).map_err(|e| e.to_string())?;
        // 前端传 ••• 表示保留原密钥。
        if cfg.api_key == "•••" {
            cfg.api_key = crate::collab::lead_ai::load_cfg().api_key;
        }
        crate::collab::lead_ai::save_cfg(&cfg)?;
        Ok(json!({"ok": true}))
    })
    .await;
    unwrap_api(out)
}

/// 拆卡:goal → 草案;dispatch=true 且授权允许时直接建卡(仍是四要素强制)。
async fn lead_ai_decompose(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 2 {
        return forbid();
    }
    let st = state.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let pid = i_of(&v, "projectId").ok_or("缺 projectId")?;
        if !crate::collab::projects::can_admin(pid, ctx.user_id, role_rank(&ctx.role) >= 3) {
            return Err("只有项目管理者能调用主 Agent".into());
        }
        let _run_guard = LeadAiRunGuard::acquire(pid)?;
        let drafts =
            crate::collab::lead_ai::ai_decompose(pid, &s_of(&v, "goal"), &s_of(&v, "memberHint"))?;
        let dispatch = v.get("dispatch").and_then(|x| x.as_bool()).unwrap_or(false)
            && crate::collab::lead::get_grants(pid)?.auto_dispatch;
        let mut created = Vec::new();
        if dispatch {
            for d in &drafts {
                if d.title.starts_with("待澄清") {
                    continue; // 拆不动的不硬拆,留给 owner
                }
                if let Ok(card) = crate::collab::lead::lead_create_task(
                    pid,
                    &d.title,
                    &d.body,
                    &d.scope,
                    &d.criteria,
                ) {
                    emit_task(&st, &card);
                    created.push(card);
                }
            }
        }
        Ok(json!({"drafts": drafts, "created": created}))
    })
    .await;
    unwrap_api(out)
}

/// 验收草稿:自动取任务分支相对 main 的 diff 喂给模型,产出意见草稿(不落状态机)。
async fn lead_ai_review(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 2 {
        return forbid();
    }
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let tid = i_of(&v, "taskId").ok_or("缺 taskId")?;
        let card = crate::collab::tasks::get(tid)?;
        if !crate::collab::projects::can_admin(
            card.project_id,
            ctx.user_id,
            role_rank(&ctx.role) >= 3,
        ) {
            return Err("只有项目管理者能调用主 Agent".into());
        }
        let _run_guard = LeadAiRunGuard::acquire(card.project_id)?;
        let repo = project_repo_path(card.project_id)?;
        let diff_text = git_diff_limited(&repo, &format!("main...{}", card.branch))?;
        let draft = crate::collab::lead_ai::ai_review(card.project_id, tid, &diff_text)?;
        ok(draft)
    })
    .await;
    unwrap_api(out)
}

/// 冲突融合草案(指挥件④的 AI 侧):对单个冲突块起草融合文本。
/// 只产草案不落任何东西——落地须经 merge/resolve(人工确认后先落 PR 分支)。
async fn lead_ai_fuse(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 2 {
        return forbid();
    }
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let tid = i_of(&v, "taskId").ok_or("缺 taskId")?;
        let file = s_of(&v, "file");
        let block_idx = i_of(&v, "blockIndex").ok_or("缺 blockIndex")? as usize;
        if file.is_empty() {
            return Err("缺 file".into());
        }
        let card = crate::collab::tasks::get(tid)?;
        if !crate::collab::projects::can_admin(
            card.project_id,
            ctx.user_id,
            role_rank(&ctx.role) >= 3,
        ) {
            return Err("只有项目管理者能调用主 Agent".into());
        }
        let _run_guard = LeadAiRunGuard::acquire(card.project_id)?;
        let repo = project_repo_path(card.project_id)?;
        let blocks = crate::collab::mergectl::conflict_blocks(&repo, "main", &card.branch, &file)?;
        let b = blocks
            .get(block_idx)
            .ok_or("冲突块不存在(态势可能已变化,请刷新)")?;
        let context = format!(
            "文件 {file} 第 {}–{} 行,任务卡:{}",
            b.start_line, b.end_line, card.title
        );
        let text = crate::collab::lead_ai::ai_fuse(card.project_id, &b.ours, &b.theirs, &context)?;
        Ok(json!({"text": text}))
    })
    .await;
    unwrap_api(out)
}

// ── 合并闸门 ──
//
// 三级闸(v8 5.3):① 机器闸=merge_trial 干净 + 提交者是项目成员;② 验收闸=卡上
// 最新一轮 verdict=pass;③ 放行闸=owner 亲点,或主 Agent 有 can_merge 位。

fn configured_repo_root() -> Result<PathBuf, String> {
    let root = if let Ok(raw) = std::env::var("POLARIS_REPO_ROOT") {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("POLARIS_REPO_ROOT 不能为空".into());
        }
        PathBuf::from(trimmed)
    } else {
        directories::UserDirs::new()
            .ok_or("无法确定用户目录，请设置 POLARIS_REPO_ROOT")?
            .home_dir()
            .join("Polaris")
            .join("repos")
    };
    std::fs::create_dir_all(&root).map_err(|e| format!("创建仓库根目录失败: {e}"))?;
    std::fs::canonicalize(&root).map_err(|e| format!("解析仓库根目录失败: {e}"))
}

/// 只接受管理员预先放入 POLARIS_REPO_ROOT 的本地 Git 仓库。canonicalize 后再比对，
/// 同时阻断 `..` 与目录符号链接逃逸到宿主机其它位置。
fn validate_repo_path(repo: &str) -> Result<PathBuf, String> {
    let raw = repo.trim();
    if raw.is_empty() {
        return Err("项目仓库路径不能为空".into());
    }
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err("仓库必须填写宿主机绝对路径，不能填写 Git 远程 URL".into());
    }
    let canonical =
        std::fs::canonicalize(path).map_err(|e| format!("仓库路径不存在或不可访问: {e}"))?;
    let root = configured_repo_root()?;
    if !canonical.starts_with(&root) {
        return Err(format!(
            "仓库必须位于受控目录 {} 内（可用 POLARIS_REPO_ROOT 修改）",
            root.display()
        ));
    }
    if !canonical.join(".git").exists() && !canonical.join("HEAD").is_file() {
        return Err(format!("仓库路径不是 Git 仓库: {}", canonical.display()));
    }
    Ok(canonical)
}

/// `Command::output()` 会在内存里无上限收完整 diff，巨大仓库可直接 OOM；这里边读边限到
/// 1MiB，并给 git 20 秒硬超时。超过上限后立即杀掉子进程，既保护内存也保护模型预算。
fn git_diff_limited(repo: &Path, revision: &str) -> Result<String, String> {
    use std::io::Read as _;
    use std::process::Stdio;
    use std::sync::mpsc::{self, TryRecvError};

    const LIMIT: usize = 1024 * 1024;
    let mut child = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["diff", "--unified=3", revision])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("取 diff 失败: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("无法读取 git diff 输出")?;
    let (tx, rx) = mpsc::channel::<Result<Vec<u8>, String>>();
    let reader = std::thread::spawn(move || {
        let mut out = Vec::with_capacity(64 * 1024);
        let mut buf = [0u8; 64 * 1024];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) => {
                    let _ = tx.send(Ok(out));
                    return;
                }
                Ok(n) if out.len().saturating_add(n) <= LIMIT => out.extend_from_slice(&buf[..n]),
                Ok(_) => {
                    let _ = tx.send(Err(
                        "变更超过 1MB，拒绝整包送入模型；请缩小任务范围或拆分任务".into(),
                    ));
                    return;
                }
                Err(e) => {
                    let _ = tx.send(Err(format!("读取 diff 失败: {e}")));
                    return;
                }
            }
        }
    });

    let started = std::time::Instant::now();
    let mut captured: Option<Result<Vec<u8>, String>> = None;
    loop {
        if captured.is_none() {
            match rx.try_recv() {
                Ok(result) => {
                    if result.is_err() {
                        let _ = child.kill();
                    }
                    captured = Some(result);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader.join();
                    return Err("读取 git diff 的工作线程异常退出".into());
                }
            }
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("等待 git diff 失败: {e}"))?
        {
            let result = match captured {
                Some(result) => result,
                None => rx
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .map_err(|_| "读取 git diff 超时".to_string())?,
            };
            let _ = reader.join();
            if !status.success() {
                return Err("取 diff 失败，请确认 main 与任务分支存在".into());
            }
            return result.map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
        }
        if started.elapsed() >= std::time::Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err("取 diff 超过 20 秒，已终止；请缩小任务范围".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// 项目仓库的本地路径(projects.repo 存主机侧权威仓库路径)。每次使用都重新校验，
/// 让旧版本创建的越界记录也无法继续触达任意宿主机仓库。
pub fn project_repo_path(project_id: i64) -> Result<std::path::PathBuf, String> {
    let p = crate::collab::projects::get(project_id)?;
    validate_repo_path(&p.repo)
}

/// 冲突试算(无副作用,任何成员可跑)。入参 taskId;分支取卡上记录。
async fn merge_trial_api(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    // 注意不能按全局角色早拒(role_rank<2):全局 visitor 可以是团队/项目管理者
    // (can_admin 认项目成员表),早拒会把合法管理者挡在裁决台数据源之外。
    // 资源面由下面的项目成员闸兜住:非成员(含普通 visitor)照旧 403。
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let tid = i_of(&v, "taskId").ok_or("缺 taskId")?;
        let card = crate::collab::tasks::get(tid)?;
        if role_rank(&ctx.role) < 3
            && crate::collab::projects::member_role(card.project_id, ctx.user_id).is_none()
        {
            return Err("你不是该项目成员".into());
        }
        if card.branch.is_empty() {
            return Err("任务尚未开分支".into());
        }
        let repo = project_repo_path(card.project_id)?;
        let trial = crate::collab::mergectl::merge_trial(&repo, "main", &card.branch)?;
        let (behind, ahead) = crate::collab::mergectl::behind_count(&repo, "main", &card.branch)?;
        let overlap =
            crate::collab::mergectl::scope_overlap(&repo, "main", &card.branch, &card.scope)?;
        // 冲突时顺带给出每个文件的结构化冲突块(裁决台数据源)。
        let mut blocks = serde_json::Map::new();
        if !trial.clean {
            for f in trial.conflict_files.iter().take(20) {
                if let Ok(b) =
                    crate::collab::mergectl::conflict_blocks(&repo, "main", &card.branch, f)
                {
                    blocks.insert(f.clone(), serde_json::to_value(b).unwrap_or(Value::Null));
                }
            }
        }
        Ok(json!({
            "clean": trial.clean,
            "conflictFiles": trial.conflict_files,
            "behind": behind, "ahead": ahead,
            "scopeOverlap": overlap,
            "conflictBlocks": blocks,
        }))
    })
    .await;
    unwrap_api(out)
}

/// 冲突裁决落地:逐块处置(采纳某侧/融合草案)一次性落成任务分支上的合并提交。
/// 管理者专属——融合草案(含 AI 起草)必须由人在这里确认才会落分支,且永不直写 main。
async fn merge_resolve_api(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let tid = i_of(&v, "taskId").ok_or("缺 taskId")?;
        let card = crate::collab::tasks::get(tid)?;
        if !crate::collab::projects::can_admin(
            card.project_id,
            ctx.user_id,
            role_rank(&ctx.role) >= 3,
        ) {
            return Err("只有项目/团队管理者能落地裁决".into());
        }
        if card.state != "review" {
            return Err("任务不在待验收状态,不能裁决".into());
        }
        if card.branch.is_empty() {
            return Err("任务尚未开分支".into());
        }
        let resolutions: std::collections::HashMap<
            String,
            Vec<crate::collab::mergectl::BlockResolution>,
        > = serde_json::from_value(v.get("resolutions").cloned().ok_or("缺 resolutions")?)
            .map_err(|e| format!("resolutions 结构不符: {e}"))?;
        let repo = project_repo_path(card.project_id)?;
        let oid = crate::collab::mergectl::resolve_conflicts(
            &repo,
            "main",
            &card.branch,
            &resolutions,
            &ctx.username,
        )?;
        Ok(json!({"ok": true, "commit": oid}))
    })
    .await;
    unwrap_api(out)
}

/// squash 合并放行:走满三级闸,合并后推进状态机+广播。
async fn merge_squash_api(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let st = state.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let tid = i_of(&v, "taskId").ok_or("缺 taskId")?;
        let card = crate::collab::tasks::get(tid)?;
        // 放行闸:owner 直通;主 Agent 路径走 lead_approve_merge(查 can_merge 位)。
        // asLead 与 task_review 同一铁律:也只能由项目/团队管理者代为发起——否则任意
        // 登录账号(含 visitor)都能借 asLead 抢先放行合并,且落痕记成 lead 误导审计。
        let as_lead = v.get("asLead").and_then(|x| x.as_bool()).unwrap_or(false);
        let can_admin = crate::collab::projects::can_admin(card.project_id, ctx.user_id, role_rank(&ctx.role) >= 3);
        if !can_admin {
            return Err("合并放行需要项目/团队管理者(主 Agent 路径也须由管理者代为发起)".into());
        }
        let actor = if as_lead {
            crate::collab::lead::lead_approve_merge(card.project_id, tid)?;
            format!("lead(申请人:{})", ctx.username)
        } else {
            ctx.username.clone()
        };
        // 验收闸:最新一轮必须 pass。
        let rounds = crate::collab::tasks::rounds(tid)?;
        if rounds.last().map(|r| r.verdict.as_str()) != Some("pass") {
            return Err("验收闸未过:最新一轮验收不是通过".into());
        }
        if card.state != "review" {
            return Err("任务不在待验收状态".into());
        }
        // 先解析仓库:没配仓库时给出真实原因,而不是误报「检查闸未过」。
        let repo = project_repo_path(card.project_id)?;
        // 检查闸(GitHub required checks 式):最新一轮检查须全绿,且**分支头没变**
        // (检查过后又推新提交=陈旧检查;GitHub 按 commit 查,我们按轮+SHA 补齐语义)。
        // profile=off 不拦;owner 可 force 强推(留痕审计)。
        // force 只认 can_admin 分支——as_lead 路径不许 force(主 Agent 无权跳检查)。
        let force = can_admin && !as_lead && v.get("force").and_then(|x| x.as_bool()).unwrap_or(false);
        let profile = crate::collab::checks::project_profile(card.project_id);
        if profile != "off" && !force {
            // 先扫僵尸 running(进程崩过留下的),否则那张卡永远卡在「未跑完」合不了。
            crate::collab::checks::sweep_stale_running();
            // 按「检查实际落库的最大轮次」判定,而非 card.round —— review 通过会 +1 使 card.round
            // 漂到检查记录之上(见 checks::latest_round 注释)。None=从没跑过检查 → 视为未过。
            let crnd = match crate::collab::checks::latest_round(tid) {
                Some(r) => r,
                None => return Err("检查闸未过:尚未跑过检查,请提交送验触发检查".into()),
            };
            match crate::collab::checks::all_green(tid, crnd) {
                Ok(true) => {
                    // SHA 陈旧比对:记录为空(老数据)放行兼容;比对不上要求重跑。
                    if let Some(checked_sha) = crate::collab::checks::round_sha(tid, crnd) {
                        let head = std::process::Command::new("git")
                            .arg("-C").arg(&repo)
                            .args(["rev-parse", &card.branch])
                            .output()
                            .ok()
                            .filter(|o| o.status.success())
                            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
                        if let Some(head) = head {
                            if head != checked_sha {
                                return Err("检查闸未过:检查之后分支又有新提交(检查已陈旧),请重跑检查或重新送验".into());
                            }
                        }
                    }
                }
                Ok(false) => return Err("检查闸未过:本轮检查未全绿(或未跑完)。owner 可带 force 强推".into()),
                // 查询出错必须 fail-closed:SQLite busy 等偶发错误不能被当成「通过」放行。
                Err(e) => return Err(format!("检查闸未过:检查结果查询失败({e}),请重试")),
            }
        }
        if force {
            crate::collab::db::audit(&ctx.username, "merge.force", &tid.to_string(), "跳过检查闸强推");
        }
        // 机器闸 + 执行(squash_merge 内部会重跑试算,不干净即拒)。
        let title = format!("#{} {}", card.id, card.title);
        let oid = crate::collab::mergectl::squash_merge(&repo, "main", &card.branch, &title, &actor)?;
        // 合并已落 main,此后状态机推进失败不能当普通 400 吞掉——那会让仓库与卡状态
        // 永久不一致且无人知晓。留审计并明确告知需人工核对。
        let merged = crate::collab::tasks::mark_merged(tid, &actor).map_err(|e| {
            // 双保险留痕:audit 与 mark_merged 同库,库级故障(只读/满盘)时 audit 也会
            // 静默失败——stderr 是唯一不依赖该库的持久痕迹(容器/服务日志可查)。
            eprintln!("[collab] merge.state_drift task={tid} commit={oid} err={e}");
            crate::collab::db::audit(
                &actor,
                "merge.state_drift",
                &tid.to_string(),
                &format!("squash 已落 main({oid}) 但状态机推进失败: {e}"),
            );
            format!("合并已落地(提交 {oid}),但任务状态更新失败: {e}。请手动核对该卡状态")
        })?;
        emit_task(&st, &merged);
        Ok(json!({"ok": true, "commit": oid, "card": merged}))
    })
    .await;
    unwrap_api(out)
}

/// 以卡回滚:revert 一个 squash 提交=整卡撤销。项目/团队管理者专属(主 Agent 只能"申请")。
async fn merge_revert_api(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    let global_owner = role_rank(&ctx.role) >= 3;
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let pid = i_of(&v, "projectId").ok_or("缺 projectId")?;
        if !crate::collab::projects::can_admin(pid, ctx.user_id, global_owner) {
            return Err("只有项目/团队管理者能回滚".into());
        }
        let oid = s_of(&v, "commit");
        if oid.is_empty() {
            return Err("缺 commit".into());
        }
        let repo = project_repo_path(pid)?;
        let new_oid = crate::collab::mergectl::revert_card(&repo, &oid, &ctx.username)?;
        Ok(json!({"ok": true, "commit": new_oid}))
    })
    .await;
    unwrap_api(out)
}

// ── iroh 隧道控制 ──

async fn tunnel_status_api(State(state): State<CollabState>, headers: HeaderMap) -> Response {
    let Some(_ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    #[cfg(feature = "collab-net")]
    {
        return Json(crate::collab::tunnel::status()).into_response();
    }
    #[cfg(not(feature = "collab-net"))]
    Json(json!({"running": false, "unavailable": "本构建未启用 collab-net(iroh) 功能"}))
        .into_response()
}

async fn tunnel_start_api(State(state): State<CollabState>, headers: HeaderMap) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    #[cfg(feature = "collab-net")]
    {
        crate::collab::tunnel::start_host_blocking_thread();
        return Json(json!({"ok": true})).into_response();
    }
    #[cfg(not(feature = "collab-net"))]
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({"error":"本构建未启用 collab-net 功能"})),
    )
        .into_response()
}

/// RelayMap 动态下发(v8 7.3 第2项):改中继只动主机一处,全员自动生效。
async fn tunnel_relays_api(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(_v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    #[cfg(feature = "collab-net")]
    {
        return match crate::collab::tunnel::apply_relay_config(&_v.to_string()) {
            Ok(_) => Json(json!({"ok": true})).into_response(),
            Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
        };
    }
    #[cfg(not(feature = "collab-net"))]
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({"error":"本构建未启用 collab-net 功能"})),
    )
        .into_response()
}

// ── 云机中继网关:桌面主机挂牌注册 ──

/// 桌面主机挂牌到云机网关:POST 自己的 iroh NodeId,云机起本地桥接监听并返回网关 NodeId
/// (桌面随后把它加进设备白名单,隧道才放行)。
/// 鉴权:**必须真实账号**(user_id>0)。全局口令/免鉴合成身份一律拒 —— 挂牌要绑到账号上,
/// 账号管理面才能按账号看到「谁挂了哪台主机」并一键踢下线。NodeId 经 ensure_device_for
/// 绑定该账号(已属别人的 NodeId 拒绝顶替),挂牌落库 gw_hosts(云机重启懒恢复)。
#[cfg(feature = "collab-net")]
async fn gw_register(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if ctx.user_id <= 0 {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"挂牌必须使用真实账号登录(免密口令身份不可注册主机)"})),
        )
            .into_response();
    }
    let host_node_id = s_of(&v, "hostNodeId");
    if host_node_id.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"缺少 hostNodeId"})),
        )
            .into_response();
    }
    let host_name = s_of(&v, "hostName");
    let name = if host_name.trim().is_empty() {
        "桌面主机".to_string()
    } else {
        host_name
    };
    // NodeId ↔ 账号绑定(防顶替):已属其他账号直接拒。
    {
        let uid = ctx.user_id;
        let node = host_node_id.clone();
        let nm = name.clone();
        let bound = tokio::task::spawn_blocking(move || {
            crate::collab::identity::ensure_device_for(uid, &node, &nm)
        })
        .await;
        match bound {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                return (StatusCode::FORBIDDEN, Json(json!({"error": e}))).into_response()
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("设备绑定任务失败: {e}")})),
                )
                    .into_response()
            }
        }
    }
    crate::collab::db::audit(&ctx.username, "gw.register", &host_node_id, &name);
    match crate::collab::gateway::register_host(&host_node_id, &name, ctx.user_id).await {
        Ok(port) => Json(json!({
            "ok": true,
            "gatewayNodeId": crate::collab::gateway::gateway_node_id().unwrap_or_default(),
            "hostId": host_node_id,
            "localPort": port,
        }))
        .into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({"error": e}))).into_response(),
    }
}

// ── Gitea 托管与 /git/* 反代 ──

async fn gitea_status_api(State(state): State<CollabState>, headers: HeaderMap) -> Response {
    let Some(_ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    Json(crate::collab::gitea::status()).into_response()
}

async fn gitea_start_api(State(state): State<CollabState>, headers: HeaderMap) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    let out =
        tokio::task::spawn_blocking(|| crate::collab::gitea::start().map(|_| json!({"ok": true})))
            .await;
    unwrap_api(out)
}

async fn gitea_stop_api(State(state): State<CollabState>, headers: HeaderMap) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    crate::collab::gitea::stop();
    Json(json!({"ok": true})).into_response()
}

/// git smart HTTP 反代(v8 2.3):git 客户端 Basic 认证 = Polaris 用户名 + 会话 token,
/// 验过后**替换**为该用户的 Gitea 访问令牌再转发 localhost:3000。效果:隧道放行表
/// 只有 8080 一个端口,权限两道闸(Polaris 会话 + Gitea 仓库协作者位),Gitea 对外隐身。
/// v1 为缓冲式转发(与 /api/invoke 的 512MB 上限一致);超大仓库首拉建议 LFS 分批。
async fn git_proxy(
    State(state): State<CollabState>,
    axum::extract::Path(rest): axum::extract::Path<String>,
    Query(q): Query<HashMap<String, String>>,
    method: axum::http::Method,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    // ① 验会话:Basic username:token(git 客户端) 或 Bearer(程序化调用)。
    let ctx = 'auth: {
        if let Some(basic) = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Basic "))
        {
            use base64::Engine;
            if let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(basic) {
                if let Ok(s) = String::from_utf8(raw) {
                    if let Some((user, pass)) = s.split_once(':') {
                        // 同步查库挪阻塞池(与 auth_ctx 同理),git 客户端每请求都走这里。
                        let auth_token = state.auth_token.clone();
                        let pass = pass.to_string();
                        let resolved = tokio::task::spawn_blocking(move || {
                            resolve_auth(&auth_token, Some(pass.as_str()))
                        })
                        .await
                        .ok()
                        .flatten();
                        if let Some(c) = resolved {
                            if c.username == user || c.role == "owner" {
                                break 'auth Some(c);
                            }
                        }
                    }
                }
            }
            None
        } else {
            auth_ctx(&state, &headers).await
        }
    };
    let Some(ctx) = ctx else {
        // 401 + WWW-Authenticate 让 git 客户端弹出凭据询问。
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"polaris-git\"")],
            "认证失败:用户名=Polaris 账号,密码=会话 token",
        )
            .into_response();
    };
    if role_rank(&ctx.role) < 2 {
        return (StatusCode::FORBIDDEN, "访问者角色无 git 读写权").into_response();
    }

    // ② 换发凭据并转发。
    let port: u16 = std::env::var("POLARIS_GITEA_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    let qs = if q.is_empty() {
        String::new()
    } else {
        let enc: Vec<String> = q
            .iter()
            .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
            .collect();
        format!("?{}", enc.join("&"))
    };
    // Docker compose 下 Gitea 是独立容器(服务名 gitea);单机托管则是 127.0.0.1。
    let gitea_host = std::env::var("POLARIS_GITEA_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let url = format!("http://{gitea_host}:{port}/{rest}{qs}");
    let username = ctx.username.clone();
    let m = method.as_str().to_string();
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body_vec = body.to_vec();

    let out = tokio::task::spawn_blocking(move || -> Result<(u16, String, Vec<u8>), String> {
        // 每用户 Gitea 凭据(首次自动开号+发 token)。
        crate::collab::gitea::ensure_user(&username)?;
        let token = crate::collab::gitea::user_token(&username)?;
        let mut req = ureq::request(&m, &url)
            .set("Authorization", &format!("token {token}"))
            .timeout(std::time::Duration::from_secs(600));
        if let Some(ct) = &ct {
            req = req.set("Content-Type", ct);
        }
        if let Some(a) = &accept {
            req = req.set("Accept", a);
        }
        // git 协议体是二进制;gzip 让 ureq 自行协商。
        let resp = if body_vec.is_empty() && (m == "GET" || m == "HEAD") {
            req.call()
        } else {
            req.send_bytes(&body_vec)
        };
        let resp = match resp {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r, // 保留上游错误码语义
            Err(e) => return Err(format!("转发 Gitea 失败: {e}")),
        };
        let status = resp.status();
        let ctype = resp.content_type().to_string();
        let mut buf = Vec::new();
        use std::io::Read;
        resp.into_reader()
            .take(2 * 1024 * 1024 * 1024)
            .read_to_end(&mut buf)
            .map_err(|e| format!("读上游响应失败: {e}"))?;
        Ok((status, ctype, buf))
    })
    .await;

    match out {
        Ok(Ok((status, ctype, buf))) => {
            let mut resp = Response::new(Body::from(buf));
            *resp.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            if let Ok(hv) = ctype.parse() {
                resp.headers_mut().insert(header::CONTENT_TYPE, hv);
            }
            resp
        }
        Ok(Err(e)) => (StatusCode::BAD_GATEWAY, e).into_response(),
        Err(e) => err_resp(e.to_string()),
    }
}

// ───────────────────────── WebSocket(emit 推流)─────────────────────────

/// WS 推流循环(server 壳的全量 /ws 与桌面 hosting 的协作 /ws 共用)。
/// audience 过滤:定向事件只投给受众本人;owner 全收。未标受众的机器级广播在尚无
/// team/project ACL 时只给 owner，避免聊天 delta、其它团队任务流泄露给任意登录者。
pub async fn ws_loop(socket: WebSocket, mut rx: broadcast::Receiver<Event>, ctx: AuthCtx) {
    let (mut sender, mut receiver) = socket.split();
    // 读侧:仅用于探测客户端关闭(前端浏览器模式不向后端 emit)。
    let mut closed = tokio::spawn(async move { while let Some(Ok(_)) = receiver.next().await {} });
    let is_owner = role_rank(&ctx.role) >= 3;

    loop {
        tokio::select! {
            recv = rx.recv() => match recv {
                Ok(ev) => {
                    // 按用户过滤(方案硬伤2):定向事件只投给受众本人;owner 全收(审计视角)。
                    match &ev.audience {
                        Some(aud) if !is_owner && aud != &ctx.username => continue,
                        None if !is_owner => continue,
                        _ => {}
                    }
                    // 帧在 emit 时已预序列化(Event::frame),N 条连接共享同一份 Arc<str>,
                    // 这里只剩 Arc<str> → String 一次拷贝(axum 0.7 Message::Text 要 String)。
                    if sender
                        .send(Message::Text((*ev.frame).to_string()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue, // 落后则跳过旧帧
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = &mut closed => break,
        }
    }
    // 读探测任务必须显式收尸:tokio 的 JoinHandle 被 drop 只是「分离」不是「中止」,
    // 发送侧出错/频道关闭 break 后若不 abort,每断一条 WS 就残留一个永久挂在
    // receiver.next() 上的任务,7×24 下缓慢泄漏。(对已完成的任务 abort 是无害 no-op。)
    closed.abort();
}

/// 桌面 hosting 专用 /ws:走 collab 严格鉴权(会话 token / 全局口令 / 未启用时的
/// 单人兜底),不用 server 壳那套宽松的基础面鉴权(那是给单人 Docker chat 流的)。
async fn collab_ws_handler(
    State(state): State<CollabState>,
    Query(params): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    // 鉴权同步查库挪阻塞池(与 auth_ctx 同理;WS token 走 query 而非 header)。
    let auth_token = state.auth_token.clone();
    let token = params.get("token").cloned();
    let ctx = tokio::task::spawn_blocking(move || resolve_auth(&auth_token, token.as_deref()))
        .await
        .ok()
        .flatten();
    let Some(ctx) = ctx else {
        return (StatusCode::UNAUTHORIZED, "未授权").into_response();
    };
    let rx = state.app.subscribe();
    ws.on_upgrade(move |socket| ws_loop(socket, rx, ctx))
}

// ───────────────────────── 远程文件浏览(fsface)─────────────────────────
// 隧道另一端的「盘」:owner 鉴权 + 路径关押(POLARIS_FS_ROOTS 白名单)。
// 仅需 collab-host(纯文件访问),桌面 hosting 与 Docker server 壳都挂得上。

/// GET /api/fs/list?path=<rel> → { entries: [...] }
async fn fs_list_api(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    let rel = q.get("path").cloned().unwrap_or_default();
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let entries = crate::collab::fsface::list(&rel)?;
        ok(json!({ "entries": entries }))
    })
    .await;
    match out {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => (StatusCode::FORBIDDEN, Json(json!({ "error": e }))).into_response(),
        Err(e) => err_resp(format!("内部任务失败: {e}")),
    }
}

/// GET /api/fs/read?path=<rel> → 原始字节(前端按 mime 预览/下载)。
async fn fs_read_api(
    State(state): State<CollabState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(ctx) = auth_ctx(&state, &headers).await else {
        return forbid();
    };
    if role_rank(&ctx.role) < 3 {
        return forbid();
    }
    let rel = q.get("path").cloned().unwrap_or_default();
    let out = tokio::task::spawn_blocking(move || crate::collab::fsface::read_bytes(&rel)).await;
    match out {
        Ok(Ok(bytes)) => (
            [(header::CONTENT_TYPE, "application/octet-stream")],
            bytes,
        )
            .into_response(),
        Ok(Err(e)) => (StatusCode::FORBIDDEN, Json(json!({ "error": e }))).into_response(),
        Err(e) => err_resp(format!("内部任务失败: {e}")),
    }
}

// ───────────────────────── 路由表 ─────────────────────────

/// 协作路由。`with_ws=true` 时附带 /ws(桌面 hosting 用;server 壳自己有全量 /ws,传 false 防 merge 撞路由)。
pub fn collab_router(state: CollabState, with_ws: bool) -> Router {
    let mut r = Router::new()
        // 账号/会话/票据/设备(权限判定全走 collab.db 确定性授权表)
        .route("/api/collab/bootstrap", post(collab_bootstrap))
        .route("/api/collab/login", post(collab_login))
        .route("/api/collab/logout", post(collab_logout))
        .route("/api/collab/me", get(collab_me))
        .route("/api/collab/redeem", post(collab_redeem))
        // GitHub 式:开放注册 + 用户名搜索 + 团队(一人多团队,团队下挂项目)
        .route("/api/collab/signup", post(collab_signup))
        // 邮箱验证码:注册 / 找回密码(验证码 = 邮箱所有权证明;频控在 mail 模块)
        .route("/api/collab/email/status", get(collab_email_status))
        .route("/api/collab/email/send_code", post(collab_email_send_code))
        .route("/api/collab/email/signup", post(collab_email_signup))
        .route("/api/collab/email/reset", post(collab_email_reset))
        .route(
            "/api/collab/admin/email_config",
            get(collab_admin_email_config_get).post(collab_admin_email_config_set),
        )
        .route("/api/collab/users/search", get(collab_user_search))
        .route("/api/collab/teams", get(team_list).post(team_create))
        .route(
            "/api/collab/team/members",
            get(team_members_api).post(team_member_add),
        )
        .route("/api/collab/team/member_remove", post(team_member_remove))
        .route("/api/collab/admin/ticket", post(collab_ticket))
        .route("/api/collab/admin/users", get(collab_users))
        .route("/api/collab/admin/user_disable", post(collab_user_disable))
        .route(
            "/api/collab/admin/user_reset_password",
            post(collab_admin_user_reset_password),
        )
        .route("/api/collab/admin/devices", get(collab_devices))
        .route("/api/collab/admin/audit", get(collab_audit_recent))
        // 遥测帧本就该 <2KB:单独收紧到 4KB body 上限,低权限成员无法用大包耗解析(codex #3)。
        .route(
            "/api/collab/telemetry",
            post(collab_telemetry_report).layer(axum::extract::DefaultBodyLimit::max(4096)),
        )
        .route(
            "/api/collab/admin/device_revoke",
            post(collab_device_revoke),
        )
        // 账号镜像:本地权威+云端密文兜底(零知识,解密钥永不出主机)
        .route("/api/collab/mirror/export", post(mirror_export_api))
        .route("/api/collab/mirror/store", post(mirror_store_api))
        .route("/api/collab/mirror/pull", get(mirror_pull_api))
        .route("/api/collab/mirror/restore", post(mirror_restore_api))
        // 项目与任务卡(六态状态机;权限先过项目成员表)
        .route(
            "/api/collab/projects",
            get(project_list).post(project_create),
        )
        .route(
            "/api/collab/projects/shared-scope",
            post(project_shared_scope_set),
        )
        .route(
            "/api/collab/project/members",
            get(project_members).post(project_member_add),
        )
        .route("/api/collab/project/lead", post(project_set_lead))
        .route("/api/collab/tasks", get(task_list).post(task_create))
        // 任务级对话:增量拉取 + 发消息 + 主 Agent 手动回复
        .route(
            "/api/collab/tasks/:id/messages",
            get(task_messages_get).post(task_messages_post),
        )
        .route("/api/collab/tasks/:id/ai-reply", post(task_ai_reply))
        .route("/api/collab/task/claim", post(task_claim))
        .route("/api/collab/task/submit", post(task_submit))
        .route("/api/collab/task/review", post(task_review))
        .route("/api/collab/task/rounds", get(task_rounds))
        .route("/api/collab/task/archive", post(task_archive))
        .route("/api/collab/task/cancel", post(task_cancel))
        .route("/api/collab/activity", get(collab_activity))
        // 检查工作流(CI-lite):按轮读结果 / 手动重跑 / 项目档位
        .route("/api/collab/checks", get(checks_get))
        .route("/api/collab/checks/rerun", post(checks_rerun))
        .route("/api/collab/checks/profile", post(checks_profile_set))
        .route("/api/collab/checks/skills", get(checks_skills_list))
        // 主 Agent:授权位 / 晨会 / 改派 / 催办 / 模型配置 / AI 拆卡·验收·融合
        .route(
            "/api/collab/lead/grants",
            get(lead_grants_get).post(lead_grants_set),
        )
        .route("/api/collab/lead/morning", get(lead_morning))
        .route("/api/collab/lead/assign", post(lead_assign_api))
        .route("/api/collab/lead/nudge", post(lead_nudge_api))
        .route(
            "/api/collab/lead/model",
            get(lead_model_get).post(lead_model_set),
        )
        .route("/api/collab/lead/ai/decompose", post(lead_ai_decompose))
        .route("/api/collab/lead/ai/review", post(lead_ai_review))
        .route("/api/collab/lead/ai/fuse", post(lead_ai_fuse))
        // 合并闸门(冲突试算/裁决/squash 放行/以卡回滚)
        .route("/api/collab/merge/trial", post(merge_trial_api))
        .route("/api/collab/merge/resolve", post(merge_resolve_api))
        .route("/api/collab/merge/squash", post(merge_squash_api))
        .route("/api/collab/merge/revert", post(merge_revert_api))
        // Gitea 托管(owner)与 git smart HTTP 反代(隧道只放行 8080 一个端口的关键)
        .route("/api/collab/gitea/status", get(gitea_status_api))
        .route("/api/collab/gitea/start", post(gitea_start_api))
        .route("/api/collab/gitea/stop", post(gitea_stop_api))
        .route(
            "/git/*rest",
            axum::routing::any(git_proxy)
                .layer(axum::extract::DefaultBodyLimit::max(512 * 1024 * 1024)),
        )
        // iroh 隧道(collab-net 编译才有):主机侧监听 + 状态 + RelayMap 动态下发
        .route("/api/collab/tunnel/status", get(tunnel_status_api))
        .route("/api/collab/tunnel/start", post(tunnel_start_api))
        .route("/api/collab/tunnel/relays", post(tunnel_relays_api))
        // 远程文件浏览(fsface):owner 鉴权 + 路径关押,随隧道透传到对端的盘。
        .route("/api/fs/list", get(fs_list_api))
        .route("/api/fs/read", get(fs_read_api));
    // 云机中继网关(collab-net 才有):主机挂牌 + /h/:id 反代到该主机(经 iroh 到桌面 apihub)。
    #[cfg(feature = "collab-net")]
    {
        r = r.route("/api/gw/register", post(gw_register)).route(
            "/h/:host_id/*rest",
            axum::routing::any(crate::collab::gateway::gateway_proxy)
                .layer(axum::extract::DefaultBodyLimit::max(512 * 1024 * 1024)),
        );
    }
    if with_ws {
        r = r.route("/ws", get(collab_ws_handler));
    }
    r.with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 自动探测与 env 覆写合在一个测试里跑:环境变量是进程级的,拆成两个测试
    /// 会因并行执行互相污染(一个设着 POLARIS_ADVERTISE_URL,另一个恰好读到)。
    #[test]
    fn advertise_urls_autodetect_and_env_override() {
        std::env::remove_var("POLARIS_ADVERTISE_URL");
        for u in detect_advertise_urls(8484) {
            assert!(u.starts_with("http://"));
            assert!(!u.contains("127.0.0.1"));
            assert!(u.ends_with(":8484"));
        }
        std::env::set_var(
            "POLARIS_ADVERTISE_URL",
            "http://nas.example:9000/, https://p.example",
        );
        let urls = detect_advertise_urls(8484);
        std::env::remove_var("POLARIS_ADVERTISE_URL");
        assert_eq!(
            urls,
            vec![
                "http://nas.example:9000".to_string(),
                "https://p.example".to_string()
            ]
        );
    }
}
