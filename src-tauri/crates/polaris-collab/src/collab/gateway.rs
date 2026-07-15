//! collab/gateway.rs —— 云机中继网关(真·中继完整形态)。
//!
//! 桌面主机在 NAT 后,主动 outbound 连云机注册(POST /api/gw/register 带自己的 iroh NodeId)。
//! 云机用**单个 iroh Endpoint**(gateway.key)为每个注册主机维持一个本地 TCP 监听
//! (127.0.0.1:port_N),该监听上每条 TCP 连接开一条到该主机 NodeId 的 iroh 双向流 ——
//! 主机侧 tunnel.rs `host_listen` 把流转发到桌面本机 8484(apihub 全量数据面)。
//!
//! 手机连 `https://cloud/h/<hostId>/api/*`,云机 HTTP 反代剥掉 `/h/<hostId>` 前缀,
//! 转发到 `http://127.0.0.1:port_N/api/*`(经 iroh 隧道到桌面)。手机在任何网络、零安装。
//!
//! 与 tunnel.rs 成员侧(单主机全局 static)的区别:这里一个 Endpoint 连 N 个主机,
//! 每主机独立本地端口 + 连接缓存。ALPN 与 tunnel.rs 一致(`polaris/1`)。
//!
//! Phase 2a:HTTP 反代(ureq,同步,复用 git_proxy 范式)。WS(/ws 流式)见 Phase 2b。
#![cfg(all(feature = "collab-net", feature = "collab-host"))]

use axum::{
    body::Body,
    extract::ws::{Message as AxMsg, WebSocket, WebSocketUpgrade},
    extract::{Path as AxPath, Query, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use iroh::endpoint::presets;
use iroh::endpoint::{Connection, IdleTimeout, QuicTransportConfig};
use iroh::{Endpoint, EndpointId, SecretKey};
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use crate::collab::http::CollabState;

/// 与 tunnel.rs 一致的应用层协议标识:网关作为「成员」连主机,两端必须相同。
const ALPN: &[u8] = b"polaris/1";
const KEEP_ALIVE: Duration = Duration::from_secs(15);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// 每主机分配的本地回环端口从这里起递增(避开常用端口段)。
static NEXT_PORT: AtomicU16 = AtomicU16::new(19000);

struct HostEntry {
    node_id: String,
    local_port: u16,
    name: String,
}

/// hostId(=主机 NodeId 串) → 该主机的本地监听端口 + 元信息。
static REGISTRY: Lazy<RwLock<HashMap<String, HostEntry>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

/// 网关自己的 iroh Endpoint(单例,连所有主机复用)。
static ENDPOINT: Lazy<tokio::sync::Mutex<Option<Endpoint>>> =
    Lazy::new(|| tokio::sync::Mutex::new(None));

/// 每主机的活跃连接缓存(健康巡检 / open_bi 复用)。key = host node_id。
static CONNS: Lazy<RwLock<HashMap<String, Connection>>> = Lazy::new(|| RwLock::new(HashMap::new()));

// ── 网关设备密钥(与 tunnel.rs device.key 同款,独立文件) ──────────────────

fn gateway_key_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("POLARIS_GATEWAY_KEY") {
        let p = p.trim();
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    directories::UserDirs::new().map(|u| {
        u.home_dir()
            .join("Polaris")
            .join("data")
            .join("gateway.key")
    })
}

fn get_or_create_gateway_key() -> Result<[u8; 32], String> {
    let path = gateway_key_path().ok_or("无法定位用户目录")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建数据目录失败: {e}"))?;
    }
    if path.exists() {
        let raw =
            std::fs::read_to_string(&path).map_err(|e| format!("读 gateway.key 失败: {e}"))?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(raw.trim())
            .map_err(|e| format!("gateway.key 损坏: {e}"))?;
        if bytes.len() != 32 {
            return Err("gateway.key 长度异常".into());
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        return Ok(seed);
    }
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).map_err(|e| format!("生成密钥失败: {e}"))?;
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(seed);
    std::fs::write(&path, encoded.as_bytes()).map_err(|e| format!("写 gateway.key 失败: {e}"))?;
    Ok(seed)
}

/// 网关的 iroh NodeId(桌面主机注册后要把它加进自己的设备白名单,否则隧道拒连)。
pub fn gateway_node_id() -> Result<String, String> {
    let seed = get_or_create_gateway_key()?;
    Ok(SecretKey::from_bytes(&seed).public().to_string())
}

// ── Endpoint 惰性初始化 ─────────────────────────────────────────────────────

async fn ensure_endpoint() -> Result<Endpoint, String> {
    let mut g = ENDPOINT.lock().await;
    if let Some(ep) = g.as_ref() {
        return Ok(ep.clone());
    }
    let seed = get_or_create_gateway_key()?;
    let transport = QuicTransportConfig::builder()
        .keep_alive_interval(KEEP_ALIVE)
        .max_idle_timeout(Some(
            IdleTimeout::try_from(IDLE_TIMEOUT).expect("idle timeout 常量合法"),
        ))
        .build();
    let ep = Endpoint::builder(presets::N0)
        .secret_key(SecretKey::from_bytes(&seed))
        .transport_config(transport)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .map_err(|e| format!("网关 iroh endpoint 绑定失败: {e}"))?;
    *g = Some(ep.clone());
    eprintln!("[gateway] iroh Endpoint 就绪 node_id={}", ep.id());
    Ok(ep)
}

/// 取一条到指定主机的健康连接:缓存里已死的丢弃重建。
async fn healthy_conn(host_node_id: &str) -> Result<Connection, String> {
    if let Some(c) = CONNS.read().get(host_node_id).cloned() {
        if c.close_reason().is_none() {
            return Ok(c);
        }
        CONNS.write().remove(host_node_id);
    }
    let ep = ensure_endpoint().await?;
    let host_id: EndpointId = host_node_id
        .trim()
        .parse()
        .map_err(|e| format!("主机 NodeId 非法: {e}"))?;
    let c = ep
        .connect(host_id, ALPN)
        .await
        .map_err(|e| format!("连主机失败: {e}"))?;
    CONNS.write().insert(host_node_id.to_string(), c.clone());
    eprintln!("[gateway] 已连上主机 {host_node_id}");
    Ok(c)
}

// ── 注册主机:分配本地端口 + 起 iroh 桥接监听 ──────────────────────────────

/// 注册一个桌面主机。幂等:已注册返回原端口。返回分配的本地回环端口。
pub async fn register_host(host_node_id: &str, name: &str) -> Result<u16, String> {
    let host_node_id = host_node_id.trim().to_string();
    if host_node_id.is_empty() {
        return Err("主机 NodeId 为空".into());
    }
    // 先验 NodeId 合法。
    let _: EndpointId = host_node_id
        .parse()
        .map_err(|e| format!("主机 NodeId 非法: {e}"))?;
    if let Some(e) = REGISTRY.read().get(&host_node_id) {
        return Ok(e.local_port);
    }

    // 分配本地端口并绑监听。
    let mut listener = None;
    for _ in 0..64 {
        let p = NEXT_PORT.fetch_add(1, Ordering::SeqCst);
        if let Ok(l) = tokio::net::TcpListener::bind(("127.0.0.1", p)).await {
            listener = Some((l, p));
            break;
        }
    }
    let (listener, port) = listener.ok_or("网关本地端口耗尽")?;

    REGISTRY.write().insert(
        host_node_id.clone(),
        HostEntry {
            node_id: host_node_id.clone(),
            local_port: port,
            name: name.to_string(),
        },
    );

    // 后台:该主机的本地监听 → 每条 TCP 开一条 iroh 双向流。
    let host = host_node_id.clone();
    tokio::spawn(async move {
        eprintln!("[gateway] 主机 {host} 本地桥接监听 127.0.0.1:{port}");
        loop {
            let (mut tcp, _peer) = match listener.accept().await {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("[gateway] 本地 accept 失败: {e}");
                    continue;
                }
            };
            let host = host.clone();
            tokio::spawn(async move {
                // 取健康连接开流;失败重建一次。
                let bi = match healthy_conn(&host).await {
                    Ok(c) => match c.open_bi().await {
                        Ok(s) => Some(s),
                        Err(_) => {
                            CONNS.write().remove(&host);
                            match healthy_conn(&host).await {
                                Ok(c2) => c2.open_bi().await.ok(),
                                Err(_) => None,
                            }
                        }
                    },
                    Err(e) => {
                        eprintln!("[gateway] {e}");
                        None
                    }
                };
                let Some((send, recv)) = bi else { return };
                let mut stream = tokio::io::join(recv, send);
                let _ = tokio::io::copy_bidirectional(&mut tcp, &mut stream).await;
            });
        }
    });

    Ok(port)
}

/// 已注册主机数(状态/调试用)。
pub fn registered_count() -> usize {
    REGISTRY.read().len()
}

/// 查某主机的本地端口。
fn local_port_of(host_id: &str) -> Option<u16> {
    REGISTRY.read().get(host_id).map(|e| e.local_port)
}

// ── HTTP 反代:/h/:id/*rest → 127.0.0.1:port_N/rest ─────────────────────────

/// `GET/POST /h/:hostId/*rest` 反代到该主机的本地桥接端口(经 iroh 到桌面 apihub)。
/// 鉴权透传:手机带的 Bearer token / query token 由桌面主机侧(apihub)校验,网关不拦
/// (网关只做路由,身份判定留给持有账号库的桌面主机)。ureq 同步转发,放 spawn_blocking。
pub async fn gateway_proxy(
    State(_state): State<CollabState>,
    AxPath((host_id, rest)): AxPath<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
    method: Method,
    headers: HeaderMap,
    ws: Option<WebSocketUpgrade>,
    body: axum::body::Bytes,
) -> Response {
    let Some(port) = local_port_of(&host_id) else {
        return (
            StatusCode::NOT_FOUND,
            format!("主机 {host_id} 未注册或已离线"),
        )
            .into_response();
    };
    let qs = build_qs(&q);
    // WebSocket 升级(手机 /ws 流式)→ 走双向 pump 反代;普通 HTTP 走下面 ureq。
    if let Some(ws) = ws {
        let backend = format!("ws://127.0.0.1:{port}/{rest}{qs}");
        return ws.on_upgrade(move |client| async move {
            match tokio_tungstenite::connect_async(&backend).await {
                Ok((server, _)) => pump_ws(client, server).await,
                Err(e) => eprintln!("[gateway] 连后端 WS 失败({backend}): {e}"),
            }
        });
    }
    let url = format!("http://127.0.0.1:{port}/{rest}{qs}");
    let m = method.as_str().to_string();
    // 透传关键请求头(鉴权/内容类型);Host 由 ureq 依 url 自设。
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body_vec = body.to_vec();

    let out = tokio::task::spawn_blocking(move || -> Result<(u16, String, Vec<u8>), String> {
        let mut req = ureq::request(&m, &url).timeout(Duration::from_secs(600));
        if let Some(a) = &auth {
            req = req.set("Authorization", a);
        }
        if let Some(c) = &ct {
            req = req.set("Content-Type", c);
        }
        if let Some(a) = &accept {
            req = req.set("Accept", a);
        }
        let resp = if body_vec.is_empty() && (m == "GET" || m == "HEAD") {
            req.call()
        } else {
            req.send_bytes(&body_vec)
        };
        let resp = match resp {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r, // 保留上游状态码
            Err(e) => return Err(format!("经隧道转发到主机失败: {e}")),
        };
        let status = resp.status();
        let ctype = resp.content_type().to_string();
        let mut buf = Vec::new();
        use std::io::Read;
        resp.into_reader()
            .take(2 * 1024 * 1024 * 1024)
            .read_to_end(&mut buf)
            .map_err(|e| format!("读主机响应失败: {e}"))?;
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
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("网关任务失败: {e}"),
        )
            .into_response(),
    }
}

/// 最小 URL 编码(query 值透传;与 http.rs urlencode 同款,gateway 内自带避免跨模块 pub)。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// query map → `?k=v&...`(空则空串)。HTTP 与 WS 反代共用。
fn build_qs(q: &HashMap<String, String>) -> String {
    if q.is_empty() {
        return String::new();
    }
    let enc: Vec<String> = q
        .iter()
        .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
        .collect();
    format!("?{}", enc.join("&"))
}

// ── WS 反代:手机 WebSocket ↔ 后端(经隧道到桌面 apihub /ws) ─────────────────

type BackendWs =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// 双向 pump:任一端关闭即整体收工。axum 与 tungstenite 的 Message 手动互转。
async fn pump_ws(client: WebSocket, server: BackendWs) {
    let (mut cw, mut cr) = client.split();
    let (mut sw, mut sr) = server.split();
    let c2s = async {
        while let Some(Ok(m)) = cr.next().await {
            if sw.send(ax_to_tung(m)).await.is_err() {
                break;
            }
        }
    };
    let s2c = async {
        while let Some(Ok(m)) = sr.next().await {
            if cw.send(tung_to_ax(m)).await.is_err() {
                break;
            }
        }
    };
    tokio::select! {
        _ = c2s => {}
        _ = s2c => {}
    }
}

fn ax_to_tung(m: AxMsg) -> tokio_tungstenite::tungstenite::Message {
    use tokio_tungstenite::tungstenite::Message as T;
    match m {
        AxMsg::Text(s) => T::Text(s.into()),
        AxMsg::Binary(b) => T::Binary(b.into()),
        AxMsg::Ping(b) => T::Ping(b.into()),
        AxMsg::Pong(b) => T::Pong(b.into()),
        AxMsg::Close(_) => T::Close(None),
    }
}

fn tung_to_ax(m: tokio_tungstenite::tungstenite::Message) -> AxMsg {
    use tokio_tungstenite::tungstenite::Message as T;
    match m {
        T::Text(s) => AxMsg::Text(s.as_str().to_string()),
        T::Binary(b) => AxMsg::Binary(b.to_vec()),
        T::Ping(b) => AxMsg::Ping(b.to_vec()),
        T::Pong(b) => AxMsg::Pong(b.to_vec()),
        T::Close(_) => AxMsg::Close(None),
        T::Frame(_) => AxMsg::Binary(Vec::new()),
    }
}
