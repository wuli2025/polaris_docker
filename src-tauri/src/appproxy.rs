//! appproxy.rs —— 「应用直投」:把电脑当服务器,手机只跑一个空壳网页端。
//!
//! 与「隔空同屏(beam.rs)」的分工:
//!  - beam = 投一个**文件**(打包成静态网页,两端同看一页)。
//!  - appproxy = 投一个**正在本机跑的应用**(带后端的活服务),手机上操作的是真实应用本体,
//!    后端计算全在电脑上跑。手机端只有 DOM,没有业务逻辑。
//!
//! 为什么这比远程桌面好:
//!  - 远程桌面传的是**像素**(Mbps 级、糊、文字不可选、触摸要模拟鼠标);
//!    这里传的是**HTTP 报文**(KB 级、原生 DOM、可选可搜、触摸就是触摸、按手机屏自适应)。
//!  - 断线重连不丢状态:状态在电脑上的应用里,手机只是个视图。
//!
//! 边界(必须讲清楚,不能让人误以为什么都能投):
//!  - 只能投**开了 TCP HTTP 端口**的应用 —— 开发服务器(Vite/Next/Flask…)、带内置 HTTP
//!    后端或 sidecar 的桌面应用。
//!  - **Tauri/Electron 生产包默认不开端口**(Tauri 走 `tauri://` 自定义协议、Electron 走
//!    `file://`),那种情况这里投不了,只能开它的 dev server,或退回画面流(那就是远程桌面)。
//!  - `app_scan_ports` 会把本机所有「说 HTTP 的端口」扫出来,一试便知。
//!
//! 实现要点:
//!  - 反代直接用 **裸 TCP 说 HTTP/1.1**,不引 HTTP 客户端:请求头改写后原样转发,
//!    响应按 chunked / content-length / EOF 三种成帧方式**流式**回传(SSE、大文件、
//!    长轮询都不会被缓死)。
//!  - WebSocket / 任意 Upgrade:拿 hyper 的 `OnUpgrade`,握手成功后 `copy_bidirectional`
//!    裸转字节 —— HMR、实时推送照常。
//!  - **粘性回退**:应用页面里的绝对路径(`/assets/x.js`)不会带 `/app/{slug}` 前缀,
//!    所以进门时种一个 cookie 记住「这个客户端正在用哪个应用」,凡是没被 polaris 自己的
//!    路由接走的请求,一律按该 cookie 转给那个应用。这样不用改写 HTML,零侵入。
#![cfg(feature = "collab-host")]

use crate::apihub::ApiState;
use axum::{
    body::Body,
    extract::{Path as AxPath, Query, Request, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 落库键:已发布应用清单(JSON 数组)。
const K_APPS: &str = "apps_published";
/// 会话 cookie(能种上就种,种不上也不影响 —— 见下方「为什么会话 id 在路径里」)。
const C_SID: &str = "polaris_app_sid";
/// 会话有效期:每次命中续期。重启主机即全部作废(会话只在内存里)。
const SESS_TTL: Duration = Duration::from_secs(12 * 3600);

/// 连上游、读响应头的超时。应用本身慢没关系(body 是流式的),但握不上手要快点报错。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);
/// 请求体上限(表单/上传):再大就该走 /api/upload 了。
const MAX_REQ_BODY: usize = 64 * 1024 * 1024;
/// 响应头最大长度,防上游发疯把内存撑爆。
const MAX_HEAD: usize = 64 * 1024;

// ────────────────────────────── 已发布应用 ──────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PubApp {
    /// URL 里的短名(小写字母/数字/连字符),`/app/{slug}/` 即入口。
    pub slug: String,
    pub name: String,
    /// 本机监听端口(127.0.0.1)。
    pub port: u16,
    /// 打开时的初始路径,默认 `/`。
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub note: String,
}

fn load_apps() -> Vec<PubApp> {
    crate::collab::db::meta_get(K_APPS)
        .and_then(|s| serde_json::from_str::<Vec<PubApp>>(&s).ok())
        .unwrap_or_default()
}

/// 读已发布应用清单。
pub fn list() -> Vec<PubApp> {
    load_apps()
}

/// 整体覆盖清单。校验 slug 合法、端口非零、不指向 polaris 自己(否则请求会自噬成环)。
pub fn set(apps: Vec<PubApp>) -> Result<Vec<PubApp>, String> {
    let self_port = crate::hosting_port();
    let mut clean: Vec<PubApp> = Vec::new();
    for mut a in apps {
        a.slug = a.slug.trim().to_ascii_lowercase();
        a.name = a.name.trim().to_string();
        a.path = a.path.trim().to_string();
        if a.slug.is_empty()
            || !a
                .slug
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(format!(
                "短名不合法:{}(只能用小写字母、数字、连字符)",
                a.slug
            ));
        }
        if a.port == 0 {
            return Err(format!("应用「{}」没填端口", a.name));
        }
        if Some(a.port) == self_port {
            return Err(format!(
                "端口 {} 是北极星自己的服务端口,发布它会让请求自己转给自己",
                a.port
            ));
        }
        if clean.iter().any(|x| x.slug == a.slug) {
            return Err(format!("短名重复:{}", a.slug));
        }
        if a.name.is_empty() {
            a.name = a.slug.clone();
        }
        if !a.path.starts_with('/') {
            a.path = format!("/{}", a.path.trim_start_matches('/'));
        }
        clean.push(a);
    }
    let json = serde_json::to_string(&clean).map_err(|e| e.to_string())?;
    crate::collab::db::meta_set(K_APPS, &json)?;
    crate::collab::db::audit(
        "local",
        "app.publish",
        if clean.is_empty() { "cleared" } else { "set" },
        &clean
            .iter()
            .map(|a| format!("{}:{}", a.slug, a.port))
            .collect::<Vec<_>>()
            .join(" | "),
    );
    Ok(clean)
}

fn find(slug: &str) -> Option<PubApp> {
    load_apps().into_iter().find(|a| a.slug == slug)
}

// ────────────────────────────── 端口探测 ──────────────────────────────

#[derive(Serialize, Clone, Debug)]
pub struct PortHit {
    pub port: u16,
    /// 上游自报的 Server 头(空 = 没给)。
    pub server: String,
    /// 首页 `<title>`,给人认出是哪个应用。
    pub title: String,
    pub status: u16,
    /// 是否已经在发布清单里。
    pub published: bool,
}

/// 常见的本机 HTTP 端口候选。刻意不做全端口扫描 —— 65535 个连接会被防火墙/杀软当扫描行为。
fn candidates() -> Vec<u16> {
    let mut v: Vec<u16> = vec![
        1420, 1421, 1430, 1431, 1432, // tauri / polaris 自家 dev
        3000, 3001, 3002, 3003, 4000, 4200, 4173, // node / vite preview / angular
        5000, 5001, 5050, 5173, 5174, 5175, 5176, 5177, // flask / vite
        6006, 7000, 7001, 7777, // storybook / 杂
        8000, 8001, 8008, 8080, 8081, 8082, 8088, 8090, 8100, 8181, 8501, 8888, 8899, 9000, 9001,
        9090, 9200, 11434, // ollama
        19000, 19006, // expo
    ];
    for a in load_apps() {
        v.push(a.port);
    }
    if let Some(p) = crate::hosting_port() {
        v.retain(|x| *x != p);
    }
    v.sort_unstable();
    v.dedup();
    v
}

/// 扫一遍候选端口,返回「确实在说 HTTP」的那些。并发跑,单口最多 ~1.2s。
pub async fn scan_ports() -> Vec<PortHit> {
    let published: Vec<u16> = load_apps().iter().map(|a| a.port).collect();
    let mut tasks = Vec::new();
    for p in candidates() {
        tasks.push(tokio::spawn(async move { probe_one(p).await }));
    }
    let mut out = Vec::new();
    for t in tasks {
        if let Ok(Some(mut hit)) = t.await {
            hit.published = published.contains(&hit.port);
            out.push(hit);
        }
    }
    out.sort_by_key(|h| h.port);
    out
}

async fn probe_one(port: u16) -> Option<PortHit> {
    let mut s = tokio::time::timeout(
        Duration::from_millis(300),
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .ok()?
    .ok()?;
    let req = format!(
        "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUser-Agent: polaris-probe\r\nAccept: text/html\r\nConnection: close\r\n\r\n"
    );
    tokio::time::timeout(Duration::from_millis(400), s.write_all(req.as_bytes()))
        .await
        .ok()?
        .ok()?;
    let mut buf = vec![0u8; 16 * 1024];
    let mut got = 0usize;
    // 读到够判断为止(或超时)。不求读全,只要头 + 一点 body 拿到 <title> 就行。
    let deadline = tokio::time::Instant::now() + Duration::from_millis(800);
    loop {
        let n = match tokio::time::timeout_at(deadline, s.read(&mut buf[got..])).await {
            Ok(Ok(0)) | Err(_) => break,
            Ok(Ok(n)) => n,
            Ok(Err(_)) => break,
        };
        got += n;
        if got >= buf.len() {
            break;
        }
    }
    if got == 0 {
        return None;
    }
    let text = String::from_utf8_lossy(&buf[..got]);
    if !text.starts_with("HTTP/") {
        return None; // 端口开着但不说 HTTP(数据库、redis 之类)
    }
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let server = header_of(&text, "server");
    let title = between(&text, "<title>", "</title>")
        .map(|t| t.trim().chars().take(60).collect::<String>())
        .unwrap_or_default();
    Some(PortHit {
        port,
        server,
        title,
        status,
        published: false,
    })
}

fn header_of(head: &str, key: &str) -> String {
    for line in head.lines() {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case(key) {
                return v.trim().chars().take(60).collect();
            }
        }
    }
    String::new()
}

fn between<'a>(s: &'a str, a: &str, b: &str) -> Option<&'a str> {
    let i = s.to_ascii_lowercase().find(a)? + a.len();
    let rest = &s[i..];
    let j = rest.to_ascii_lowercase().find(b)?;
    Some(&rest[..j])
}

// ────────────────────────── 打开会话(能力票据)──────────────────────────
//
// **为什么会话 id 放在路径里,而不是只靠 cookie**:
// 手机壳是 Capacitor WebView,页面源是 `http://localhost`,而应用经隧道挂在
// `http://127.0.0.1:<隧道口>` —— iframe 里它是**第三方**。安卓 WebView 默认
// `acceptThirdPartyCookies=false`,cookie 根本种不上,纯 cookie 方案在 App 内必挂。
// 所以真正的凭据是路径里的一次性会话 id(`/x/{sid}/…`):
//   · 页面自身与相对资源天然带着前缀;
//   · 绝对路径资源(`/assets/x.js`)走粘性回退,靠 **Referer 里的 `/x/{sid}/`** 认回来;
//   · cookie 仍然照种 —— 能种上的环境(系统浏览器)就多一层保险。
// 会话 id 是**只对这一个应用有效**的能力票据,owner 令牌不会出现在任何 URL / Referer 里。

struct Sess {
    slug: String,
    expire: std::time::Instant,
}

static SESSIONS: once_cell::sync::Lazy<parking_lot::Mutex<HashMap<String, Sess>>> =
    once_cell::sync::Lazy::new(|| parking_lot::Mutex::new(HashMap::new()));

/// 会话票据 = 能力凭据,**RNG 失败必须上抛,绝不能退回可预测值**。
/// (旧版 `let _ = getrandom(..)` 在 RNG 失败时会留下 18 字节全零 → 固定可猜的 sid,
/// 等于鉴权绕过。对齐 hosting.rs 的 random_access_token 把错误 `?` 上抛。)
fn new_sid() -> Result<String, String> {
    let mut b = [0u8; 18];
    getrandom::getrandom(&mut b).map_err(|e| format!("生成会话票据失败: {e}"))?;
    Ok(base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        b,
    ))
}

/// 建一个打开会话。调用方必须先鉴权(tauri 命令天然本机;HTTP 走 invoke 的 owner 闸)。
pub fn open_session(slug: &str, user: &str) -> Result<String, String> {
    let app = find(slug).ok_or_else(|| format!("没有已发布的应用:{slug}"))?;
    let sid = new_sid()?;
    let mut g = SESSIONS.lock();
    let now = std::time::Instant::now();
    g.retain(|_, s| s.expire > now); // 顺手清过期,不另起清理线程
    g.insert(
        sid.clone(),
        Sess {
            slug: app.slug.clone(),
            expire: now + SESS_TTL,
        },
    );
    drop(g);
    // 谁在什么时候打开了哪个应用要留痕(会话本身只在内存里,重启即失效,故不落库)。
    crate::collab::db::audit(user, "app.open", &app.slug, &format!("port={}", app.port));
    Ok(sid)
}

/// 取会话并续期。返回它绑定的应用。
fn touch(sid: &str) -> Option<PubApp> {
    let mut g = SESSIONS.lock();
    let s = g.get_mut(sid)?;
    let now = std::time::Instant::now();
    if s.expire <= now {
        g.remove(sid);
        return None;
    }
    s.expire = now + SESS_TTL;
    let slug = s.slug.clone();
    drop(g);
    find(&slug)
}

// ────────────────────────────── 路由 ──────────────────────────────

/// `/x/{sid}/…` = 会话内代理;`/app/{slug}?t=令牌` = 直链入口(换成会话后跳过去)。
pub fn routes(state: ApiState) -> Router {
    Router::new()
        .route("/x/:sid", any(sess_entry))
        // `/x/{sid}/` 必须单列:axum 的 `*rest` 通配至少要吃掉一个字符,
        // 光有 `/x/:sid/*rest` 时「带尾斜杠的首页」会漏进 fallback 变 404(e2e 抓到的)。
        .route("/x/:sid/", any(sess_entry))
        .route("/x/:sid/*rest", any(sess_entry))
        .route("/app/:slug", any(direct_entry))
        .with_state(state)
}

/// 会话内的一切请求。
async fn sess_entry(AxPath(params): AxPath<HashMap<String, String>>, req: Request) -> Response {
    let sid = params.get("sid").cloned().unwrap_or_default();
    let rest = params.get("rest").cloned().unwrap_or_default();
    let Some(app) = touch(&sid) else {
        return (
            StatusCode::UNAUTHORIZED,
            "这个应用会话已过期或不存在 —— 请在北极星里重新打开一次",
        )
            .into_response();
    };
    let mut target = if rest.is_empty() {
        if app.path.is_empty() {
            "/".to_string()
        } else {
            app.path.clone()
        }
    } else {
        format!("/{rest}")
    };
    if let Some(q) = req.uri().query() {
        target.push('?');
        target.push_str(q);
    }
    let mut resp = proxy(app.port, &target, req).await;
    // 能种上就种:系统浏览器里 cookie 有效,绝对路径资源连 Referer 都不用看。
    push_cookie(
        &mut resp,
        &format!("{C_SID}={sid}; Path=/; HttpOnly; SameSite=Lax"),
    );
    resp
}

#[derive(Deserialize)]
struct DirectQuery {
    /// owner 令牌(仅用于换一张会话票据,换完立刻从 URL 里消失)。
    #[serde(default)]
    t: Option<String>,
}

/// 直链入口:`/app/{slug}?t=<owner令牌>` → 建会话 → 302 到 `/x/{sid}/`。
/// 桌面互联页「用浏览器打开」与分享给自己另一台设备时用这条。
async fn direct_entry(
    State(state): State<ApiState>,
    AxPath(params): AxPath<HashMap<String, String>>,
    Query(q): Query<DirectQuery>,
    headers: HeaderMap,
) -> Response {
    let slug = params.get("slug").cloned().unwrap_or_default();
    let token =
        q.t.clone()
            .or_else(|| crate::collab::http::bearer_of(&headers));
    let Some(ctx) = authorize(&state, token).await else {
        return (
            StatusCode::UNAUTHORIZED,
            "未授权 —— 请从北极星手机端 / 互联页打开应用",
        )
            .into_response();
    };
    if crate::collab::http::role_rank(&ctx.role) < 3 {
        return (StatusCode::FORBIDDEN, "远程使用本机应用需要 owner 权限").into_response();
    }
    match open_session(&slug, &ctx.username) {
        Ok(sid) => {
            // 进门 URL 带着 `?t=<owner令牌>`。加 `Referrer-Policy: no-referrer`,浏览器跳到
            // `/x/{sid}/` 后不会把这个含令牌的 URL 当 Referer 发出去 —— 否则被代理的应用
            // (可能是第三方 dev server)能从 Referer 里读到主机口令。换票据后令牌即弃。
            let mut resp = Response::builder()
                .status(StatusCode::FOUND)
                .header(header::LOCATION, format!("/x/{sid}/"))
                .header("Referrer-Policy", "no-referrer")
                .body(Body::empty())
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            push_cookie(
                &mut resp,
                &format!("{C_SID}={sid}; Path=/; HttpOnly; SameSite=Lax"),
            );
            resp
        }
        Err(e) => (StatusCode::NOT_FOUND, e).into_response(),
    }
}

/// 粘性回退:应用页面里的绝对路径(`/assets/x.js`、`/socket.io/…`)不带 `/x/{sid}` 前缀。
/// 解析顺序:cookie → **Referer 里的 `/x/{sid}/`**。两者都没有就是 404,不放行任何未凭票请求。
/// polaris 自己的路由已在前面被匹配走,这里再显式排除,免得把打错的 API 路径误转给应用。
pub async fn sticky_fallback(req: Request) -> Response {
    let path = req.uri().path().to_string();
    if path.starts_with("/api") || path.starts_with("/ws") || path.starts_with("/x/") {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    }
    let sid = parse_cookies(req.headers())
        .get(C_SID)
        .cloned()
        .or_else(|| sid_from_referer(req.headers()));
    let Some(sid) = sid else {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    };
    let Some(app) = touch(&sid) else {
        return (StatusCode::UNAUTHORIZED, "应用会话已过期,请重新打开").into_response();
    };
    let target = match req.uri().query() {
        Some(q) => format!("{path}?{q}"),
        None => path,
    };
    proxy(app.port, &target, req).await
}

/// 从 `Referer: http://host/x/{sid}/…` 里抠出会话 id。
fn sid_from_referer(h: &HeaderMap) -> Option<String> {
    let r = h.get(header::REFERER)?.to_str().ok()?;
    let i = r.find("/x/")? + 3;
    let rest = &r[i..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let sid = &rest[..end];
    if sid.is_empty() {
        None
    } else {
        Some(sid.to_string())
    }
}

async fn authorize(
    state: &ApiState,
    token: Option<String>,
) -> Option<crate::collab::http::AuthCtx> {
    let auth_token = state.auth_token.clone();
    // 应用直投自带路径票据(/x/{sid}/),鉴权不吃「内网免口令」:反代把本机应用整个暴露出去,
    // 且隧道对端流量落在 127.0.0.1,按来源判会把远端一律当内网。
    let gate = crate::apihub::OriginGate::closed();
    tokio::task::spawn_blocking(move || {
        crate::apihub::resolve_app_auth_token(&auth_token, token.as_deref(), gate)
    })
    .await
    .ok()
    .flatten()
}

fn parse_cookies(h: &HeaderMap) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some(v) = h.get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        for part in v.split(';') {
            if let Some((k, val)) = part.split_once('=') {
                out.insert(k.trim().to_string(), val.trim().to_string());
            }
        }
    }
    out
}

fn push_cookie(resp: &mut Response, v: &str) {
    if let Ok(hv) = HeaderValue::from_str(v) {
        resp.headers_mut().append(header::SET_COOKIE, hv);
    }
}

// ────────────────────────────── 反向代理本体 ──────────────────────────────

/// 逐跳头:按 RFC 不能转发。另外把我们自己的 cookie 摘掉,别泄给被代理的应用。
const HOP: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

fn is_hop(name: &str) -> bool {
    HOP.iter().any(|h| name.eq_ignore_ascii_case(h))
}

/// 把 Cookie 头里属于 polaris 的项摘掉,其余原样转给应用 —— 被代理的应用不该看见
/// 我们的会话票据(它拿到也只能用来打自己,但没有理由给)。
fn strip_our_cookies(v: &str) -> String {
    v.split(';')
        .map(|s| s.trim())
        .filter(|s| {
            let k = s.split('=').next().unwrap_or("").trim();
            k != C_SID && !k.starts_with("polaris_")
        })
        .collect::<Vec<_>>()
        .join("; ")
}

async fn proxy(port: u16, path_and_query: &str, mut req: Request) -> Response {
    // ⚠️ 请求走私防线:axum 的 Path 提取器会对路径参数做 percent-decode,
    // 于是 `/x/{sid}/foo%0d%0aX-Real-IP:1.2.3.4` 解码后带真实 CR/LF,
    // 若原样拼进请求行就能向上游注入任意头 / 走私第二个请求。
    // 请求行里出现任何控制字符一律拒。(sticky_fallback 用未解码的 uri().path(),
    // 本不受影响,但统一在此把关最稳妥。)
    if path_and_query.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return (StatusCode::BAD_REQUEST, "非法请求路径").into_response();
    }
    let is_upgrade = req
        .headers()
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);

    // 组请求头(两条路共用)。头值同样禁控制字符 —— HeaderValue 类型已挡住裸 CR/LF,
    // 但转发时我们是手拼字符串,多一道显式判定不吃亏(见下方循环)。
    let method = req.method().clone();
    let mut head = format!("{} {} HTTP/1.1\r\n", method.as_str(), path_and_query);
    head.push_str(&format!("Host: 127.0.0.1:{port}\r\n"));
    for (k, v) in req.headers() {
        let name = k.as_str();
        if name.eq_ignore_ascii_case("host") || name.eq_ignore_ascii_case("content-length") {
            continue;
        }
        // Upgrade 请求要保留 connection/upgrade,否则上游不会升级。
        if is_hop(name)
            && !(is_upgrade
                && (name.eq_ignore_ascii_case("connection")
                    || name.eq_ignore_ascii_case("upgrade")))
        {
            continue;
        }
        let Ok(val) = v.to_str() else { continue };
        if name.eq_ignore_ascii_case("cookie") {
            let cleaned = strip_our_cookies(val);
            if cleaned.is_empty() {
                continue;
            }
            head.push_str(&format!("Cookie: {cleaned}\r\n"));
            continue;
        }
        head.push_str(&format!("{name}: {val}\r\n"));
    }

    if is_upgrade {
        return proxy_upgrade(port, head, req).await;
    }

    // 普通请求:先把体读完(要算 Content-Length),再一次写出去。
    let body = std::mem::replace(req.body_mut(), Body::empty());
    let bytes = match axum::body::to_bytes(body, MAX_REQ_BODY).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "请求体过大").into_response(),
    };
    head.push_str(&format!("Content-Length: {}\r\n", bytes.len()));
    head.push_str("Connection: close\r\n\r\n");

    let mut sock = match connect(port).await {
        Ok(s) => s,
        Err(e) => return bad_gateway(port, &e),
    };
    if sock.write_all(head.as_bytes()).await.is_err()
        || (!bytes.is_empty() && sock.write_all(&bytes).await.is_err())
    {
        return bad_gateway(port, "写请求失败(应用可能刚退出)");
    }
    let _ = sock.flush().await;

    let (raw_head, pre) = match tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut sock)).await {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return bad_gateway(port, &e),
        Err(_) => return bad_gateway(port, "应用响应超时"),
    };
    build_response(raw_head, pre, sock)
}

async fn connect(port: u16) -> Result<tokio::net::TcpStream, String> {
    tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .map_err(|_| "连接超时".to_string())?
    .map_err(|e| e.to_string())
}

fn bad_gateway(port: u16, why: &str) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        format!(
            "连不上本机 127.0.0.1:{port} —— {why}。\
             那个应用还在跑吗?(Tauri/Electron 生产包默认不开 HTTP 端口,投不了)"
        ),
    )
        .into_response()
}

/// 读响应头(到 `\r\n\r\n`),返回 (头文本, 已经多读进来的 body 前缀)。
async fn read_head(sock: &mut tokio::net::TcpStream) -> Result<(String, Vec<u8>), String> {
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];
    loop {
        let n = sock.read(&mut tmp).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("应用没有返回响应就断开了".into());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_crlfcrlf(&buf) {
            let head = String::from_utf8_lossy(&buf[..pos]).into_owned();
            let pre = buf[pos + 4..].to_vec();
            return Ok((head, pre));
        }
        if buf.len() > MAX_HEAD {
            return Err("响应头过大".into());
        }
    }
}

fn find_crlfcrlf(b: &[u8]) -> Option<usize> {
    b.windows(4).position(|w| w == b"\r\n\r\n")
}

/// 上游响应头 → axum 响应,body 按成帧方式流式回传。
fn build_response(raw_head: String, pre: Vec<u8>, sock: tokio::net::TcpStream) -> Response {
    let mut lines = raw_head.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .and_then(|c| StatusCode::from_u16(c).ok())
        .unwrap_or(StatusCode::BAD_GATEWAY);

    let mut builder = Response::builder().status(status);
    let mut chunked = false;
    let mut content_len: Option<u64> = None;
    for line in lines {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        if k.eq_ignore_ascii_case("transfer-encoding") {
            chunked = v.to_ascii_lowercase().contains("chunked");
            continue; // 成帧交给 hyper 重做
        }
        if k.eq_ignore_ascii_case("content-length") {
            content_len = v.parse::<u64>().ok();
            continue;
        }
        if is_hop(k) {
            continue;
        }
        if let (Ok(name), Ok(val)) = (HeaderName::try_from(k), HeaderValue::from_str(v)) {
            builder = builder.header(name, val);
        }
    }

    // 读法:chunked → 边读边解块;有 Content-Length → 读满就停;都没有 → 读到 EOF。
    let mode = if chunked {
        BodyMode::Chunked
    } else if let Some(n) = content_len {
        BodyMode::Exact(n)
    } else {
        BodyMode::ToEof
    };
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(16);
    tokio::spawn(pump(sock, pre, mode, tx));
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

enum BodyMode {
    Chunked,
    Exact(u64),
    ToEof,
}

/// 把上游 body 抽进 channel。三种成帧各自处理;任何一步出错就把错误送下去,
/// 让客户端看到「连接中断」而不是拿到一半的内容却以为是完整的。
async fn pump(
    mut sock: tokio::net::TcpStream,
    pre: Vec<u8>,
    mode: BodyMode,
    tx: tokio::sync::mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
) {
    let mut buf = pre;
    match mode {
        BodyMode::Exact(total) => {
            let mut sent: u64 = 0;
            if !buf.is_empty() {
                let take = buf.len().min((total - sent) as usize);
                sent += take as u64;
                if tx.send(Ok(buf[..take].to_vec())).await.is_err() {
                    return;
                }
            }
            let mut tmp = vec![0u8; 32 * 1024];
            while sent < total {
                match sock.read(&mut tmp).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let take = n.min((total - sent) as usize);
                        sent += take as u64;
                        if tx.send(Ok(tmp[..take].to_vec())).await.is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                }
            }
        }
        BodyMode::ToEof => {
            if !buf.is_empty() && tx.send(Ok(std::mem::take(&mut buf))).await.is_err() {
                return;
            }
            let mut tmp = vec![0u8; 32 * 1024];
            loop {
                match sock.read(&mut tmp).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(Ok(tmp[..n].to_vec())).await.is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                }
            }
        }
        BodyMode::Chunked => {
            let mut tmp = vec![0u8; 32 * 1024];
            loop {
                // 找块长度行
                let line_end = match find_crlf(&buf) {
                    Some(p) => p,
                    None => match sock.read(&mut tmp).await {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&tmp[..n]);
                            continue;
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e)).await;
                            return;
                        }
                    },
                };
                let size_line = String::from_utf8_lossy(&buf[..line_end]).to_string();
                let size_hex = size_line.split(';').next().unwrap_or("").trim().to_string();
                let Ok(size) = usize::from_str_radix(&size_hex, 16) else {
                    let _ = tx
                        .send(Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "上游 chunked 编码损坏",
                        )))
                        .await;
                    return;
                };
                buf.drain(..line_end + 2);
                if size == 0 {
                    break; // 末块(尾部 trailer 直接丢弃)
                }
                // **流式吐块**:不再把整块读进内存(旧版 `buf.len() < size + 2` 会为一个
                // 4GB 巨块先在 RAM 里凑齐 = 内存放大 DoS)。先吐已缓冲的部分,剩下的边读边吐。
                let mut left = size;
                while left > 0 {
                    if buf.is_empty() {
                        match sock.read(&mut tmp).await {
                            Ok(0) => {
                                let _ = tx
                                    .send(Err(std::io::Error::new(
                                        std::io::ErrorKind::UnexpectedEof,
                                        "上游在块中途断开",
                                    )))
                                    .await;
                                return;
                            }
                            Ok(n) => buf.extend_from_slice(&tmp[..n]),
                            Err(e) => {
                                let _ = tx.send(Err(e)).await;
                                return;
                            }
                        }
                    }
                    let take = buf.len().min(left);
                    let piece: Vec<u8> = buf.drain(..take).collect();
                    left -= take;
                    if tx.send(Ok(piece)).await.is_err() {
                        return;
                    }
                }
                // 块尾 CRLF:可能还没到,补读两字节。
                while buf.len() < 2 {
                    match sock.read(&mut tmp).await {
                        Ok(0) => return,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        Err(e) => {
                            let _ = tx.send(Err(e)).await;
                            return;
                        }
                    }
                }
                buf.drain(..2);
            }
        }
    }
}

fn find_crlf(b: &[u8]) -> Option<usize> {
    b.windows(2).position(|w| w == b"\r\n")
}

// ────────────────────────────── WebSocket / Upgrade ──────────────────────────────

/// Upgrade 请求:先把握手原样转给上游,上游回 101 就把两条连接对接起来裸转字节。
/// 这样 Vite HMR、Socket.IO、应用自己的实时推送都能用 —— 远程桌面给不了这个。
async fn proxy_upgrade(port: u16, mut head: String, mut req: Request) -> Response {
    head.push_str("\r\n");
    let on_upgrade = req.extensions_mut().remove::<hyper::upgrade::OnUpgrade>();
    let Some(on_upgrade) = on_upgrade else {
        return (StatusCode::BAD_REQUEST, "这个连接不支持协议升级(WebSocket)").into_response();
    };

    let mut sock = match connect(port).await {
        Ok(s) => s,
        Err(e) => return bad_gateway(port, &e),
    };
    if sock.write_all(head.as_bytes()).await.is_err() {
        return bad_gateway(port, "写升级请求失败");
    }
    let (raw_head, pre) = match tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut sock)).await {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return bad_gateway(port, &e),
        Err(_) => return bad_gateway(port, "升级握手超时"),
    };
    let mut lines = raw_head.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    if status != 101 {
        // 上游拒绝升级:把它的响应如实回给客户端(可能是 404/403,不该伪装成成功)。
        return build_response(raw_head, pre, sock);
    }

    let mut builder = Response::builder().status(StatusCode::SWITCHING_PROTOCOLS);
    for line in lines {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        // 101 必须保留 connection/upgrade,这里只挡真正无意义的。
        if k.eq_ignore_ascii_case("transfer-encoding") || k.eq_ignore_ascii_case("content-length") {
            continue;
        }
        if let (Ok(name), Ok(val)) = (HeaderName::try_from(k), HeaderValue::from_str(v)) {
            builder = builder.header(name, val);
        }
    }

    tokio::spawn(async move {
        let Ok(upgraded) = on_upgrade.await else {
            return;
        };
        let mut client = hyper_util::rt::TokioIo::new(upgraded);
        // 上游握手时可能已经多读了几个字节的帧数据,先补回给客户端再对接。
        if !pre.is_empty() && client.write_all(&pre).await.is_err() {
            return;
        }
        let _ = tokio::io::copy_bidirectional(&mut client, &mut sock).await;
    });

    builder
        .body(Body::empty())
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

// ────────────────────────────── tauri 命令 ──────────────────────────────

/// 已发布的本机应用清单。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn app_pub_list() -> Vec<PubApp> {
    list()
}

/// 整体覆盖发布清单(空 = 全部下架)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn app_pub_set(apps: Vec<PubApp>) -> Result<Vec<PubApp>, String> {
    set(apps)
}

/// 扫本机在说 HTTP 的端口,供「发布应用」时挑。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn app_scan_ports() -> Vec<PortHit> {
    scan_ports().await
}

/// 打开一个已发布应用:换一张只对它有效的会话票据,返回可直接加载的相对地址。
/// 手机壳把 `{base}{url}` 塞进 iframe 或交给系统浏览器即可。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn app_open(slug: String) -> Result<serde_json::Value, String> {
    let sid = open_session(&slug, "local")?;
    Ok(serde_json::json!({ "sid": sid, "url": format!("/x/{sid}/") }))
}

// ────────────────────────────── 测试 ──────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_only_our_cookies() {
        let v = format!("a=1; {C_SID}=secret; b=2; polaris_x=zzz");
        let out = strip_our_cookies(&v);
        assert_eq!(out, "a=1; b=2", "只摘 polaris 自己的项: {out}");
        assert!(!out.contains("secret"), "会话票据绝不能转给被代理的应用");
    }

    /// Referer 兜底是安卓 WebView(第三方 cookie 被拦)里绝对路径资源的唯一救命稻草。
    #[test]
    fn sid_recovered_from_referer() {
        let mut h = HeaderMap::new();
        h.insert(
            header::REFERER,
            HeaderValue::from_static("http://127.0.0.1:18630/x/AbC-123/index.html?v=1"),
        );
        assert_eq!(sid_from_referer(&h).as_deref(), Some("AbC-123"));
        assert!(sid_from_referer(&HeaderMap::new()).is_none());
    }

    /// 会话票据只对它绑定的那个应用有效;不存在的 sid 一律取不到。
    #[test]
    fn session_ticket_scoped_and_expiring() {
        assert!(touch("no-such-sid").is_none(), "假票据必须取不到");
        assert!(
            open_session("definitely-not-published", "u").is_err(),
            "没发布的应用不能开会话"
        );
        let a = new_sid().unwrap();
        let b = new_sid().unwrap();
        assert_ne!(a, b, "会话 id 必须每次不同");
        assert!(a.len() >= 20, "会话 id 要足够长: {a}");
    }

    #[test]
    fn hop_headers_filtered() {
        assert!(is_hop("Connection") && is_hop("transfer-encoding") && is_hop("Upgrade"));
        assert!(!is_hop("content-type") && !is_hop("cookie"));
    }

    #[test]
    fn head_boundary_found() {
        let b = b"HTTP/1.1 200 OK\r\nA: b\r\n\r\nbody";
        assert_eq!(find_crlfcrlf(b), Some(21));
        assert_eq!(find_crlf(b"ab\r\ncd"), Some(2));
    }

    #[test]
    fn title_and_header_extracted() {
        let s = "HTTP/1.1 200 OK\r\nServer: vite\r\n\r\n<html><head><TITLE>VN Studio</TITLE>";
        assert_eq!(header_of(s, "server"), "vite");
        assert_eq!(between(s, "<title>", "</title>"), Some("VN Studio"));
    }

    /// slug 只准小写字母数字连字符;端口 0 与自噬端口要拒。
    #[test]
    fn publish_validation() {
        let bad_slug = vec![PubApp {
            slug: "Has Space".into(),
            name: "x".into(),
            port: 5173,
            path: String::new(),
            note: String::new(),
        }];
        assert!(set(bad_slug).is_err(), "非法 slug 必须拒");

        let zero_port = vec![PubApp {
            slug: "ok".into(),
            name: "x".into(),
            port: 0,
            path: String::new(),
            note: String::new(),
        }];
        assert!(set(zero_port).is_err(), "端口 0 必须拒");
    }

    /// 真上游 + 真 axum + 真 socket 的端到端往返。
    ///
    /// 反代最容易错的是**成帧**:content-length 读少一字节、chunked 忘了解块,
    /// 都会表现成「页面偶尔缺一截」这种极难排查的毛病 —— 必须真跑一遍才算数。
    /// 只在 server flavor 跑:desktop 的 AppHandle 是 tauri 真句柄,测试里造不出
    /// (与 apihub 的 exec_gate_tests 同款门控)。
    /// 手动跑:`cargo test --no-default-features --features server appproxy::e2e`
    #[cfg(all(feature = "server", not(feature = "desktop")))]
    mod e2e {
        use super::super::*;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        /// 迷你上游:三条路由,分别覆盖 content-length / chunked / 绝对路径子资源。
        async fn upstream() -> u16 {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = l.local_addr().unwrap().port();
            tokio::spawn(async move {
                loop {
                    let Ok((mut s, _)) = l.accept().await else {
                        return;
                    };
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 8192];
                        let Ok(n) = s.read(&mut buf).await else {
                            return;
                        };
                        let req = String::from_utf8_lossy(&buf[..n]).to_string();
                        let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                        let resp = if path.starts_with("/chunky") {
                            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\
                             Transfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n3\r\ndef\r\n0\r\n\r\n"
                                .to_string()
                        } else if path.starts_with("/abs/asset.js") {
                            "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\n\
                             Content-Length: 5\r\n\r\nasset"
                                .to_string()
                        } else {
                            let body = "<html><title>T</title>hello</html>";
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                                 Content-Length: {}\r\n\r\n{body}",
                                body.len()
                            )
                        };
                        let _ = s.write_all(resp.as_bytes()).await;
                    });
                }
            });
            port
        }

        /// 裸 socket 发一条 HTTP 请求,返回 (响应头, **解块后的** body)。
        ///
        /// 必须自己解块:hyper 会按自己的节奏重新分块回传,拿原始字节做断言只会
        /// 断言到「传输编码」而不是「内容」—— 第一版就这么写错过。
        async fn get2(port: u16, path: &str, referer: Option<&str>) -> (String, String) {
            let raw = get(port, path, referer).await;
            let Some(p) = raw.find("\r\n\r\n") else {
                return (raw, String::new());
            };
            let (head, rest) = (raw[..p].to_string(), &raw[p + 4..]);
            if !head
                .to_ascii_lowercase()
                .contains("transfer-encoding: chunked")
            {
                return (head, rest.to_string());
            }
            let mut body = String::new();
            let mut s = rest;
            loop {
                let Some(e) = s.find("\r\n") else { break };
                let Ok(n) = usize::from_str_radix(s[..e].trim(), 16) else {
                    break;
                };
                if n == 0 {
                    break;
                }
                let start = e + 2;
                if start + n > s.len() {
                    break;
                }
                body.push_str(&s[start..start + n]);
                s = &s[start + n + 2..];
            }
            (head, body)
        }

        /// 裸 socket 发一条 HTTP 请求,返回整份响应文本。
        async fn get(port: u16, path: &str, referer: Option<&str>) -> String {
            let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            let mut req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n");
            if let Some(r) = referer {
                req.push_str(&format!("Referer: {r}\r\n"));
            }
            req.push_str("Connection: close\r\n\r\n");
            s.write_all(req.as_bytes()).await.unwrap();
            let mut out = Vec::new();
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(10), s.read_to_end(&mut out))
                    .await;
            String::from_utf8_lossy(&out).into_owned()
        }

        #[tokio::test]
        async fn proxy_roundtrip_framing_and_referer() {
            let _g = crate::collab::db::TEST_LOCK.lock().unwrap();
            let tmp = std::env::temp_dir().join("polaris-appproxy-e2e.db");
            let _ = std::fs::remove_file(&tmp);
            std::env::set_var("POLARIS_COLLAB_DB", &tmp);

            let up = upstream().await;
            set(vec![PubApp {
                slug: "demo".into(),
                name: "Demo".into(),
                port: up,
                path: "/".into(),
                note: String::new(),
            }])
            .expect("发布应用");
            let sid = open_session("demo", "tester").expect("开会话");

            // 起代理:api 路由 + 粘性回退(与 hosting.rs 的挂法一致)
            let (tx, _rx) = tokio::sync::broadcast::channel(16);
            let state = crate::apihub::ApiState {
                app: crate::host::AppHandle::new(tx.clone()),
                tx,
                auth_token: std::sync::Arc::new(None),
                open_no_auth: false,
            };
            let router = routes(state).merge(Router::new().fallback(sticky_fallback));
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let front = l.local_addr().unwrap().port();
            tokio::spawn(async move {
                let _ = axum::serve(l, router).await;
            });
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;

            // ① content-length 成帧:一字不多一字不少
            let (h, b) = get2(front, &format!("/x/{sid}/"), None).await;
            assert!(h.contains("200 OK"), "首页应 200: {h}");
            assert_eq!(b, "<html><title>T</title>hello</html>", "正文必须逐字一致");

            // ② chunked:必须解块拿到原文,而不是把上游块头当内容透传
            let (h, b) = get2(front, &format!("/x/{sid}/chunky"), None).await;
            assert!(h.contains("200 OK"), "chunky 应 200: {h}");
            assert_eq!(b, "abcdef", "chunked 应还原成 abcdef");

            // ③ 绝对路径子资源:没 cookie,只靠 Referer 认回会话(安卓 WebView 场景)
            let referer = format!("http://127.0.0.1:{front}/x/{sid}/index.html");
            let (h, b) = get2(front, "/abs/asset.js", Some(&referer)).await;
            assert!(h.contains("200 OK"), "Referer 兜底应命中: {h}");
            assert_eq!(b, "asset", "子资源内容应完整");

            // ④ 没票据的绝对路径请求必须 404,不能裸放行
            let r = get(front, "/abs/asset.js", None).await;
            assert!(r.contains("404"), "无票据必须拒: {r}");

            // ⑤ 假会话 id 必须 401
            let r = get(front, "/x/not-a-real-sid/", None).await;
            assert!(r.contains("401"), "假票据必须 401: {r}");

            // ⑥ 请求走私防线:路径里的 %0d%0a 解码后是 CR/LF,必须 400 而不是注入上游
            let r = get(front, &format!("/x/{sid}/a%0d%0aX-Evil:1"), None).await;
            assert!(r.contains("400"), "含 CRLF 的路径必须 400: {r}");

            std::env::remove_var("POLARIS_COLLAB_DB");
            let _ = std::fs::remove_file(&tmp);
        }

        /// 对**真 dev server**(`npm run dev` 起的那种)走完整链路:
        /// 扫端口认出它 → 发布 → 开会话 → 经反代取首页与绝对路径子资源(Referer 认票),
        /// 顺带量「直连 vs 反代」延迟 —— 这正是用户在手机上的真实体验路径。
        /// 跑法(先在另一个终端把 dev server 起好):
        ///   POLARIS_APPCAST_PORT=1431 cargo test --no-default-features --features server \
        ///     appproxy::tests::e2e::real_dev_server -- --ignored --nocapture
        #[tokio::test]
        #[ignore = "需要本机先起一个真 dev server,用 POLARIS_APPCAST_PORT 指定端口"]
        async fn real_dev_server() {
            let Some(port) = std::env::var("POLARIS_APPCAST_PORT")
                .ok()
                .and_then(|s| s.parse::<u16>().ok())
            else {
                eprintln!("未设 POLARIS_APPCAST_PORT,无事可测");
                return;
            };
            let _g = crate::collab::db::TEST_LOCK.lock().unwrap();
            let tmp = std::env::temp_dir().join("polaris-appproxy-real.db");
            let _ = std::fs::remove_file(&tmp);
            std::env::set_var("POLARIS_COLLAB_DB", &tmp);

            // ① 「开它的 dev server 再扫端口」:扫描必须认出这个端口在说 HTTP
            let hits = scan_ports().await;
            assert!(
                hits.iter().any(|h| h.port == port),
                "扫描应认出 dev server 端口 {port},实得: {:?}",
                hits.iter().map(|h| h.port).collect::<Vec<_>>()
            );

            set(vec![PubApp {
                slug: "dev".into(),
                name: "Dev".into(),
                port,
                path: "/".into(),
                note: String::new(),
            }])
            .expect("发布 dev server");
            let sid = open_session("dev", "tester").expect("开会话");

            let (tx, _rx) = tokio::sync::broadcast::channel(16);
            let state = crate::apihub::ApiState {
                app: crate::host::AppHandle::new(tx.clone()),
                tx,
                auth_token: std::sync::Arc::new(None),
                open_no_auth: false,
            };
            let router = routes(state).merge(Router::new().fallback(sticky_fallback));
            // POLARIS_APPCAST_FRONT 固定入口端口(默认随机):配合下面的 HOLD,
            // 让真浏览器/无头 Chrome 能亲手打进来体验,而不只有断言。
            let want: u16 = std::env::var("POLARIS_APPCAST_FRONT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let l = tokio::net::TcpListener::bind(("127.0.0.1", want))
                .await
                .unwrap();
            let front = l.local_addr().unwrap().port();
            tokio::spawn(async move {
                let _ = axum::serve(l, router).await;
            });
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            eprintln!("入口: http://127.0.0.1:{front}/x/{sid}/");

            // ② 首页经反代逐字节可达,量端到端延迟
            let t0 = std::time::Instant::now();
            let (h, b) = get2(front, &format!("/x/{sid}/"), None).await;
            let via_proxy = t0.elapsed();
            assert!(h.contains("200"), "dev server 首页应 200: {h}");
            let lower = b.to_ascii_lowercase();
            assert!(
                lower.contains("<script") || lower.contains("<html"),
                "首页应是 HTML,实得前 200 字: {}",
                b.chars().take(200).collect::<String>()
            );

            // ③ 从首页抠一个绝对路径子资源,不带 cookie 只带 Referer 认票
            //    (安卓 WebView 拦第三方 cookie 后的真实路径)
            if let Some(src) = b.split("src=\"/").nth(1).and_then(|s| s.split('"').next()) {
                let path = format!("/{src}");
                let referer = format!("http://127.0.0.1:{front}/x/{sid}/");
                let t1 = std::time::Instant::now();
                let (h2, b2) = get2(front, &path, Some(&referer)).await;
                assert!(h2.contains("200"), "绝对路径子资源应经 Referer 认回: {h2}");
                eprintln!("子资源 {path}: {} 字节,{:?}", b2.len(), t1.elapsed());
            }

            // ④ 直连 vs 反代:反代不该比直连慢出量级(都在本机,差值就是反代自身开销)
            let t2 = std::time::Instant::now();
            let _ = get(port, "/", None).await;
            let direct = t2.elapsed();
            eprintln!(
                "首页 {} 字节 | 直连 {direct:?} vs 经反代 {via_proxy:?}",
                b.len()
            );

            // ⑤ POLARIS_APPCAST_HOLD_SECS>0:把入口压住不退,给外部浏览器留出
            //    「亲手打开体验」的窗口(无头 Chrome 截图 / 真人点开都行)。
            let hold: u64 = std::env::var("POLARIS_APPCAST_HOLD_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if hold > 0 {
                eprintln!("压住入口 {hold}s,请打开上面的地址体验…");
                tokio::time::sleep(std::time::Duration::from_secs(hold)).await;
            }

            std::env::remove_var("POLARIS_COLLAB_DB");
            let _ = std::fs::remove_file(&tmp);
        }
    }

    #[test]
    fn cookie_parse() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; polaris_app=vn"),
        );
        let c = parse_cookies(&h);
        assert_eq!(c.get("polaris_app").map(String::as_str), Some("vn"));
        assert_eq!(c.get("a").map(String::as_str), Some("1"));
    }
}
