//! collab/tunnel.rs —— iroh 组网隧道（v8 方案，做法参考 n0-computer/dumbpipe）。
//!
//! 主机侧:host.key 种子 → iroh Endpoint 监听 ALPN `polaris/1`,accept 到的每条连接
//! 先查设备白名单(identity::is_node_allowed,隧道层准入硬闸),通过后每条双向流 ↔
//! TCP 127.0.0.1:8080 双向拷贝。成员侧:本地 TcpListener,每个 TCP 连接开一条到主机
//! NodeId 的 iroh 双向流。打洞失败自动走 relay(默认 n0 公共 relay,可 apply_relay_config
//! 下发自定义 relay 列表)。
//!
//! 全模块随 `collab-net` feature 编译(iroh 依赖树大,勿进 default)。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine;
use iroh::endpoint::presets;
use iroh::endpoint::{Connection, IdleTimeout, PathId, QuicTransportConfig};
use iroh::{Endpoint, EndpointId, RelayMode, RelayUrl, SecretKey};
use once_cell::sync::Lazy;

use super::identity;

/// 应用层协议标识:主机与成员两端必须一致。
const ALPN: &[u8] = b"polaris/1";

/// QUIC 保活:15s 一个 keepalive 帧,防 NAT/relay 静默回收空闲连接;
/// 60s 无响应判死(close_reason 变 Some,健康巡检据此重连)。
const KEEP_ALIVE: Duration = Duration::from_secs(15);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// hosting.rs 内嵌服务实际绑定的端口(8484-8494 扫描结果),启动成功后写入。
/// 0 = 未设置。修「隧道默认打 8080 而内嵌服务在 8484」的不一致。
static UPSTREAM_PORT: AtomicU16 = AtomicU16::new(0);

/// 由 hosting/server 启动路径调用:告知隧道本机协作服务的真实端口。
pub fn set_upstream_port(port: u16) {
    UPSTREAM_PORT.store(port, Ordering::SeqCst);
}

/// 主机侧上游:隧道对端流量最终转发到本机 polaris-server。
/// 优先级:POLARIS_TUNNEL_UPSTREAM 环境变量 > hosting 上报的真实端口 > 兜底 8080。
fn upstream_addr() -> String {
    if let Ok(s) = std::env::var("POLARIS_TUNNEL_UPSTREAM") {
        if !s.trim().is_empty() {
            return s;
        }
    }
    let p = UPSTREAM_PORT.load(Ordering::SeqCst);
    if p != 0 {
        return format!("127.0.0.1:{p}");
    }
    "127.0.0.1:8080".to_string()
}

// ── 全局状态:running / node_id / 活跃连接数 / 停止信号 / relay 配置 ──────────

static RUNNING: AtomicBool = AtomicBool::new(false);
static CONNECTIONS: AtomicUsize = AtomicUsize::new(0);
static NODE_ID: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));
static SHUTDOWN: Lazy<tokio::sync::Notify> = Lazy::new(tokio::sync::Notify::new);
/// 自定义 relay URL 列表;空 = 用 iroh 默认(n0 公共 relay)。
static RELAYS: Lazy<Mutex<Vec<String>>> = Lazy::new(|| Mutex::new(Vec::new()));
/// 连接状态机:stopped | connecting | connected | reconnecting | degraded。
static STATE: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new("stopped".to_string()));
/// 最近一次错误(中文),前端状态徽标展示;成功后清空。
static LAST_ERROR: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));
/// 成员侧与主机的当前 QUIC 连接(健康巡检/状态上报共用)。主机侧不用。
static CLIENT_CONN: Lazy<Mutex<Option<Connection>>> = Lazy::new(|| Mutex::new(None));

fn set_state(s: &str) {
    *STATE.lock().unwrap() = s.to_string();
}

fn set_error(e: impl Into<String>) {
    *LAST_ERROR.lock().unwrap() = e.into();
}

/// 活跃连接数守卫:创建 +1,Drop -1,防 panic 漏减。
struct ConnGuard;
impl ConnGuard {
    fn new() -> Self {
        CONNECTIONS.fetch_add(1, Ordering::Relaxed);
        ConnGuard
    }
}
impl Drop for ConnGuard {
    fn drop(&mut self) {
        CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
    }
}

// ── 成员端设备密钥(与 identity.rs 的 host.key 同款读写模式,不动 identity.rs)──

/// device.key 落位:`~/Polaris/data/device.key`,可经 `POLARIS_DEVICE_KEY` 覆写。
fn device_key_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("POLARIS_DEVICE_KEY") {
        let p = p.trim();
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    directories::UserDirs::new()
        .map(|u| u.home_dir().join("Polaris").join("data").join("device.key"))
}

/// 取(或首次生成)成员设备密钥,返回 32 字节种子。幂等:同路径两次调用同值。
/// 格式与 host.key 一致:base64url(no pad) 的 32 字节随机种子,unix 下 0600。
pub fn get_or_create_device_key() -> Result<[u8; 32], String> {
    let path = device_key_path().ok_or("无法定位用户目录")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建数据目录失败: {e}"))?;
    }
    if path.exists() {
        let raw = std::fs::read_to_string(&path).map_err(|e| format!("读 device.key 失败: {e}"))?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(raw.trim())
            .map_err(|e| format!("device.key 损坏: {e}"))?;
        if bytes.len() != 32 {
            return Err("device.key 长度异常".into());
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        return Ok(seed);
    }
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).map_err(|e| format!("生成密钥失败: {e}"))?;
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(seed);
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("写 device.key 失败: {e}"))?;
        f.write_all(encoded.as_bytes()).map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, encoded.as_bytes())
            .map_err(|e| format!("写 device.key 失败: {e}"))?;
    }
    Ok(seed)
}

/// 本设备密钥对应的 iroh NodeId(z-base32 公钥串)——入伙页展示/上报主机加白名单用。
pub fn node_id_of_device_key() -> Result<String, String> {
    let seed = get_or_create_device_key()?;
    Ok(SecretKey::from_bytes(&seed).public().to_string())
}

// ── relay 动态配置 ───────────────────────────────────────────────────────────

/// 接受 `{"relays":[{"url":"https://relay.example.com"}]}` 形式的配置。
/// 空列表 = 恢复默认(n0 公共 relay)。v1 忽略证书指纹字段(预留)。
pub fn apply_relay_config(json: &str) -> Result<(), String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("relay 配置非法 JSON: {e}"))?;
    let mut urls = Vec::new();
    if let Some(arr) = v.get("relays").and_then(|r| r.as_array()) {
        for item in arr {
            let u = item
                .get("url")
                .and_then(|u| u.as_str())
                .ok_or("relay 项缺 url 字段")?;
            // 先验一遍能否解析成 RelayUrl,坏 URL 直接拒收,不留到起隧道时才炸。
            let _: RelayUrl = u.parse().map_err(|e| format!("relay url 非法 {u}: {e}"))?;
            urls.push(u.to_string());
        }
    }
    *RELAYS.lock().unwrap() = urls;
    Ok(())
}

/// 用种子建 Endpoint:N0 预设(打洞 + n0 DNS 发现 + 默认 relay),有自定义 relay 则覆盖 relay_mode。
async fn build_endpoint(seed: [u8; 32]) -> Result<Endpoint, String> {
    // 保活 + 判死超时:没有它,NAT/relay 会静默回收空闲连接,直到下次请求才暴露断线。
    let transport = QuicTransportConfig::builder()
        .keep_alive_interval(KEEP_ALIVE)
        .max_idle_timeout(Some(
            IdleTimeout::try_from(IDLE_TIMEOUT).expect("idle timeout 常量合法"),
        ))
        .build();
    let mut builder = Endpoint::builder(presets::N0)
        .secret_key(SecretKey::from_bytes(&seed))
        .transport_config(transport)
        .alpns(vec![ALPN.to_vec()]);
    let relays = RELAYS.lock().unwrap().clone();
    if !relays.is_empty() {
        let mut urls: Vec<RelayUrl> = Vec::new();
        for u in &relays {
            urls.push(u.parse().map_err(|e| format!("relay url 非法 {u}: {e}"))?);
        }
        builder = builder.relay_mode(RelayMode::custom(urls));
    }
    builder
        .bind()
        .await
        .map_err(|e| format!("iroh endpoint 绑定失败: {e}"))
}

// ── 主机侧 ──────────────────────────────────────────────────────────────────

/// 主机监听:accept 循环直到 stop()。每条连接过白名单闸,每条双向流转发到上游 TCP。
pub async fn host_listen() -> Result<(), String> {
    set_state("connecting");
    let seed = identity::get_or_create_host_key()?;
    let ep = build_endpoint(seed).await.inspect_err(|e| {
        set_error(e.clone());
        set_state("stopped");
    })?;
    *NODE_ID.lock().unwrap() = ep.id().to_string();
    RUNNING.store(true, Ordering::SeqCst);
    set_state("connected");
    set_error("");
    eprintln!("[tunnel] 主机隧道已启动 node_id={}", ep.id());

    loop {
        let incoming = tokio::select! {
            _ = SHUTDOWN.notified() => break,
            inc = ep.accept() => match inc {
                Some(inc) => inc,
                None => break, // endpoint 已关闭
            },
        };
        tokio::spawn(async move {
            let conn = match incoming.accept() {
                Ok(accepting) => match accepting.await {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("[tunnel] 握手失败: {e}");
                        return;
                    }
                },
                Err(e) => {
                    eprintln!("[tunnel] accept 失败: {e}");
                    return;
                }
            };
            let remote = conn.remote_id().to_string();
            // 隧道层默认放开:数据面(apihub)所有命令都要求 owner 令牌/会话才放行 —— 令牌
            // 才是真门禁,未持令牌者即便打进隧道也只会吃 401。故「粘连接码即连」零门槛,不再
            // 要求先把设备加白名单。想要设备白名单二次防护的部署可设 POLARIS_TUNNEL_STRICT=1。
            let strict = std::env::var("POLARIS_TUNNEL_STRICT")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            if strict && !identity::is_node_allowed(&remote) {
                eprintln!("[tunnel] 拒绝未授权设备 {remote}(strict 模式)");
                conn.close(1u32.into(), b"device not allowed");
                return;
            }
            eprintln!("[tunnel] 设备接入 {remote}");
            let _guard = ConnGuard::new();
            // 每条双向流 = 成员端一个 TCP 连接。
            loop {
                let (send, recv) = match conn.accept_bi().await {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[tunnel] 设备 {remote} 断开: {e}");
                        break;
                    }
                };
                tokio::spawn(async move {
                    let upstream = upstream_addr();
                    let mut tcp = match tokio::net::TcpStream::connect(&upstream).await {
                        Ok(t) => t,
                        Err(e) => {
                            eprintln!("[tunnel] 连上游 {upstream} 失败: {e}");
                            return;
                        }
                    };
                    let mut stream = tokio::io::join(recv, send);
                    let _ = tokio::io::copy_bidirectional(&mut tcp, &mut stream).await;
                });
            }
        });
    }

    RUNNING.store(false, Ordering::SeqCst);
    set_state("stopped");
    ep.close().await;
    eprintln!("[tunnel] 主机隧道已停止");
    Ok(())
}

// ── 成员侧 ──────────────────────────────────────────────────────────────────

/// 取一条健康的主机连接:缓存里已死(close_reason=Some)的直接丢弃重建。
async fn healthy_conn(ep: &Endpoint, host_id: EndpointId) -> Result<Connection, String> {
    let cached = CLIENT_CONN.lock().unwrap().clone();
    if let Some(c) = cached {
        if c.close_reason().is_none() {
            return Ok(c);
        }
        // 连接已死:清缓存,走重建。
        *CLIENT_CONN.lock().unwrap() = None;
    }
    set_state("connecting");
    let c = ep
        .connect(host_id, ALPN)
        .await
        .map_err(|e| format!("连主机失败: {e}"))?;
    *CLIENT_CONN.lock().unwrap() = Some(c.clone());
    set_state("connected");
    set_error("");
    eprintln!("[tunnel] 已连上主机 {host_id}");
    Ok(c)
}

/// 成员端本地代理:127.0.0.1:listen_port 上每个 TCP 连接 → 一条到主机的 iroh 双向流。
/// 多条 TCP 复用同一条 QUIC 连接(多路流)。后台巡检任务每 10s 查连接健康,
/// 断了主动重连(退避 1s→2s→…→30s 封顶),不再等下一个请求才暴露断线。
pub async fn client_proxy(host_node_id: &str, listen_port: u16) -> Result<(), String> {
    let host_id: EndpointId = host_node_id
        .trim()
        .parse()
        .map_err(|e| format!("主机 NodeId 非法: {e}"))?;
    set_state("connecting");
    let seed = get_or_create_device_key()?;
    let ep = build_endpoint(seed).await.inspect_err(|e| {
        set_error(e.clone());
        set_state("stopped");
    })?;
    *NODE_ID.lock().unwrap() = ep.id().to_string();

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", listen_port))
        .await
        .map_err(|e| format!("本地端口 {listen_port} 监听失败: {e}"))?;
    RUNNING.store(true, Ordering::SeqCst);
    eprintln!("[tunnel] 成员代理已启动 127.0.0.1:{listen_port} → {host_node_id}");

    // 后台健康巡检:发现连接死亡 → 标 reconnecting 并主动重连,退避封顶 30s;
    // 重连成功即恢复 connected。空闲期断线由此兜住(QUIC keepalive 保证判死及时)。
    let watchdog = {
        let ep = ep.clone();
        tokio::spawn(async move {
            let mut backoff = Duration::from_secs(1);
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                if !RUNNING.load(Ordering::SeqCst) {
                    break;
                }
                let dead = CLIENT_CONN
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|c| c.close_reason().is_some())
                    .unwrap_or(false);
                if !dead {
                    backoff = Duration::from_secs(1);
                    continue;
                }
                set_state("reconnecting");
                loop {
                    match healthy_conn(&ep, host_id).await {
                        Ok(_) => {
                            backoff = Duration::from_secs(1);
                            break;
                        }
                        Err(e) => {
                            set_state("reconnecting");
                            set_error(&e);
                            eprintln!("[tunnel] 重连失败({e}),{}s 后再试", backoff.as_secs());
                            tokio::time::sleep(backoff).await;
                            backoff = (backoff * 2).min(Duration::from_secs(30));
                            if !RUNNING.load(Ordering::SeqCst) {
                                return;
                            }
                        }
                    }
                }
            }
        })
    };

    // 首连(失败不退出,交给巡检/下个请求重试)。
    if let Err(e) = healthy_conn(&ep, host_id).await {
        set_error(&e);
        eprintln!("[tunnel] 首次连主机失败: {e}");
    }

    loop {
        let (mut tcp, _peer) = tokio::select! {
            _ = SHUTDOWN.notified() => break,
            acc = listener.accept() => match acc {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("[tunnel] 本地 accept 失败: {e}");
                    continue;
                }
            },
        };
        // 取健康连接开流;开流失败(竞态死亡)再重建一次。
        let bi = match healthy_conn(&ep, host_id).await {
            Ok(c) => match c.open_bi().await {
                Ok(s) => Some(s),
                Err(e) => {
                    eprintln!("[tunnel] 开流失败({e}),重建连接");
                    *CLIENT_CONN.lock().unwrap() = None;
                    match healthy_conn(&ep, host_id).await {
                        Ok(c2) => c2.open_bi().await.ok(),
                        Err(_) => None,
                    }
                }
            },
            Err(e) => {
                set_error(&e);
                eprintln!("[tunnel] {e}");
                None
            }
        };
        let Some((send, recv)) = bi else { continue };
        tokio::spawn(async move {
            let _guard = ConnGuard::new();
            let mut stream = tokio::io::join(recv, send);
            let _ = tokio::io::copy_bidirectional(&mut tcp, &mut stream).await;
        });
    }

    RUNNING.store(false, Ordering::SeqCst);
    set_state("stopped");
    watchdog.abort();
    *CLIENT_CONN.lock().unwrap() = None;
    ep.close().await;
    eprintln!("[tunnel] 成员代理已停止");
    Ok(())
}

// ── 状态 / 停止 / 同步包装 ───────────────────────────────────────────────────

/// 隧道状态:{running, state, node_id, connections, upstream, relays, latency_ms, last_error}。
/// state: stopped|connecting|connected|reconnecting;latency_ms 仅成员侧有连接时给出。
pub fn status() -> serde_json::Value {
    let latency_ms = CLIENT_CONN
        .lock()
        .unwrap()
        .as_ref()
        .filter(|c| c.close_reason().is_none())
        .and_then(|c| c.rtt(PathId::ZERO))
        .map(|d| d.as_millis() as u64);
    serde_json::json!({
        "running": RUNNING.load(Ordering::SeqCst),
        "state": STATE.lock().unwrap().clone(),
        "node_id": NODE_ID.lock().unwrap().clone(),
        "connections": CONNECTIONS.load(Ordering::Relaxed),
        "upstream": upstream_addr(),
        "relays": RELAYS.lock().unwrap().clone(),
        "latency_ms": latency_ms,
        "last_error": LAST_ERROR.lock().unwrap().clone(),
    })
}

/// 本机主机 NodeId(不绑 Endpoint,直接由 host.key 种子导出)。挂牌到云机网关时上报用。
pub fn host_node_id() -> Result<String, String> {
    let seed = identity::get_or_create_host_key()?;
    Ok(SecretKey::from_bytes(&seed).public().to_string())
}

/// 主机(或成员)隧道是否在跑。挂牌前据此判断要不要先起 host_listen。
pub fn is_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

/// 通知隧道主循环退出(主机侧与成员侧通用)。
pub fn stop() {
    SHUTDOWN.notify_waiters();
}

/// 同步包装:独立线程 + 独立 tokio runtime 跑主机隧道,供非 async 调用方(桌面/服务器启动路径)。
pub fn start_host_blocking_thread() -> std::thread::JoinHandle<Result<(), String>> {
    std::thread::Builder::new()
        .name("polaris-tunnel-host".into())
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|e| format!("tokio runtime 创建失败: {e}"))?;
            rt.block_on(host_listen())
        })
        .expect("spawn tunnel host thread")
}

/// 同步包装:独立线程跑成员端代理。
pub fn start_client_blocking_thread(
    host_node_id: String,
    listen_port: u16,
) -> std::thread::JoinHandle<Result<(), String>> {
    std::thread::Builder::new()
        .name("polaris-tunnel-client".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|e| format!("tokio runtime 创建失败: {e}"))?;
            rt.block_on(client_proxy(&host_node_id, listen_port))
        })
        .expect("spawn tunnel client thread")
}

// ── 测试 ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 设备密钥幂等:同路径两次取值一致,且 NodeId 可导出。不碰网络。
    #[test]
    fn device_key_idempotent() {
        let dir = std::env::temp_dir().join(format!("polaris-tunnel-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let key_path = dir.join("device.key");
        std::env::set_var("POLARIS_DEVICE_KEY", &key_path);

        let a = get_or_create_device_key().expect("首次生成");
        let b = get_or_create_device_key().expect("二次读取");
        assert_eq!(a, b, "两次取设备密钥必须同值");
        let nid = node_id_of_device_key().expect("导出 NodeId");
        assert!(!nid.is_empty());

        std::env::remove_var("POLARIS_DEVICE_KEY");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// relay 配置解析:合法进、坏 URL 拒。
    #[test]
    fn relay_config_parse() {
        apply_relay_config(r#"{"relays":[{"url":"https://relay.example.com"}]}"#)
            .expect("合法配置");
        assert_eq!(RELAYS.lock().unwrap().len(), 1);
        assert!(apply_relay_config(r#"{"relays":[{"url":"::bad::"}]}"#).is_err());
        apply_relay_config(r#"{"relays":[]}"#).expect("清空恢复默认");
        assert!(RELAYS.lock().unwrap().is_empty());
    }

    /// 本机自环集成测试:host + client 两端各自 Endpoint,经真实 iroh 隧道打通,
    /// GET 一个本地 TCP echo,断言往返一致。需要网络(n0 发现/relay),
    /// 由 POLARIS_NET_TEST=1 门控(未设时直接跳过,CI 不跑)。
    /// 手动跑:POLARIS_NET_TEST=1 cargo test --features server,collab-net --no-default-features collab::tunnel
    #[test]
    fn loopback_echo_roundtrip() {
        if std::env::var("POLARIS_NET_TEST")
            .map(|v| v == "1")
            .unwrap_or(false)
            == false
        {
            eprintln!("[tunnel-test] 未设 POLARIS_NET_TEST=1,跳过自环网络测试");
            return;
        }
        let dir = std::env::temp_dir().join(format!("polaris-tunnel-loop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("POLARIS_HOST_KEY", dir.join("host.key"));
        std::env::set_var("POLARIS_DEVICE_KEY", dir.join("device.key"));
        std::env::set_var("POLARIS_DATA_DIR", &dir); // 若 db 支持则隔离;不支持也无妨

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            // 1) 迷你 TCP echo 作为"上游服务"
            let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let echo_addr = echo.local_addr().unwrap();
            std::env::set_var("POLARIS_TUNNEL_UPSTREAM", echo_addr.to_string());
            tokio::spawn(async move {
                loop {
                    let (mut s, _) = echo.accept().await.unwrap();
                    tokio::spawn(async move {
                        let (mut r, mut w) = s.split();
                        let _ = tokio::io::copy(&mut r, &mut w).await;
                    });
                }
            });

            // 2) 白名单放行自己的设备 NodeId(隧道闸依赖)
            let my_node = node_id_of_device_key().unwrap();
            identity::add_device(1, "loopback-test", &my_node).expect("加白名单");

            // 3) 起主机隧道,拿主机 NodeId
            let host_seed = identity::get_or_create_host_key().unwrap();
            let host_node = SecretKey::from_bytes(&host_seed).public().to_string();
            tokio::spawn(async { host_listen().await.unwrap() });
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            // 4) 起成员代理
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = l.local_addr().unwrap().port();
            drop(l);
            let hn = host_node.clone();
            tokio::spawn(async move { client_proxy(&hn, port).await.unwrap() });
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            // 5) 经隧道往返
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut c = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            let payload = b"GET /polaris-tunnel-echo HTTP/1.0\r\n\r\n";
            c.write_all(payload).await.unwrap();
            let mut buf = vec![0u8; payload.len()];
            tokio::time::timeout(std::time::Duration::from_secs(30), c.read_exact(&mut buf))
                .await
                .expect("30s 内应收到回显")
                .unwrap();
            assert_eq!(&buf[..], payload, "隧道往返内容必须一致");
            stop();
        });
        let _ = std::fs::remove_dir_all(&dir);
    }
}
