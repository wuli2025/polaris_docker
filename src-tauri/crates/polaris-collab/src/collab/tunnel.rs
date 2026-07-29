//! collab/tunnel.rs —— iroh 组网隧道（v8 方案，做法参考 n0-computer/dumbpipe）。
//!
//! 拓扑（多隧道形态）:
//! - 主机侧: host.key 种子 → 专属 iroh Endpoint 监听 ALPN `polaris/1`,每条双向流 ↔
//!   TCP 上游(hosting/server 上报的真实端口)双向拷贝。strict 模式下过设备白名单闸。
//! - 成员侧: **所有出站隧道共享一个 Endpoint**(device.key 身份)——一个 UDP socket、
//!   一份 n0 发现记录、一条 relay 连接,N 块远程盘不再各起一套(旧版每盘一个 Endpoint,
//!   同一 device.key 多端上线会让发现记录来回抖动,互相打断打洞)。
//!   每条隧道 = 一个本地 TcpListener(127.0.0.1:port) + 到对应主机的一条 QUIC 连接,
//!   登记在 CLIENTS 注册表(port → ClientTunnel),状态/连接/重连互相独立。
//! - connect_client 幂等: 同 port 同主机已活 = no-op;disconnect_client(port) 单独拆除。
//! - 打洞失败自动走 relay(默认 n0 公共 relay,可 apply_relay_config 下发自定义列表)。
//! - mDNS 内网发现(POLARIS_MDNS=0 可关): 断外网时同局域网设备照样互相发现直连。
//!
//! 全模块随 `collab-net` feature 编译(iroh 依赖树大,勿进 default)。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use iroh::endpoint::presets;
use iroh::endpoint::{Connection, IdleTimeout, PathId, QuicTransportConfig, VarInt};
use iroh::{Endpoint, EndpointId, RelayMode, RelayUrl, SecretKey};
use once_cell::sync::Lazy;

use super::identity;

/// 应用层协议标识:主机与成员两端必须一致。
const ALPN: &[u8] = b"polaris/1";

/// QUIC 保活:15s 一个 keepalive 帧,防 NAT/relay 静默回收空闲连接;
/// 60s 无响应判死(close_reason 变 Some,健康巡检据此重连)。
const KEEP_ALIVE: Duration = Duration::from_secs(15);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// 单向转发缓冲(KB)。tokio `copy_bidirectional` 默认 8KB —— 一块 8KB 就要一次
/// 唤醒 + 一次系统调用,内网千兆/本地环回上这是纯开销。加大到 256KB 换更少的往返,
/// 代价是每条活跃流两个方向各占这么多内存(空闲流不占,缓冲随流创建随流释放)。
/// `POLARIS_TUNNEL_BUF_KB` 可调;小内存主机(NAS)想省内存就调回 32。
const DEFAULT_COPY_BUF_KB: usize = 256;

/// 转发缓冲字节数,夹在 [8KB, 4MB]:太小退化成默认,太大是拿内存换不到速度。
fn copy_buf_bytes() -> usize {
    let kb = std::env::var("POLARIS_TUNNEL_BUF_KB")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_COPY_BUF_KB);
    kb.clamp(8, 4096) * 1024
}

/// 隧道 runtime 的 worker 数:默认按机器核数来(旧版写死 2,4 核以上的机器白闲着,
/// 而 QUIC 的收发/加解密是实打实吃 CPU 的)。夹在 [2, 8]:1 核机也留 2 个免得
/// accept 与转发互相饿死,超过 8 个对这种 IO 型负载没有收益。`POLARIS_TUNNEL_WORKERS` 可调。
fn tunnel_workers() -> usize {
    std::env::var("POLARIS_TUNNEL_WORKERS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(2)
        })
        .clamp(2, 8)
}

/// TCP 段禁用 Nagle。隧道两端跑的是「一问一答」的 HTTP:Nagle 会把小的请求头/
/// 响应头攒着等下一段,与对端的延迟 ACK 撞上就是白等几十毫秒。挂载盘那种
/// 「一次 PROPFIND 一次往返」的用法对这个尤其敏感。失败只记不致命(链路照常能用)。
fn nodelay(s: &tokio::net::TcpStream, who: &str) {
    if let Err(e) = s.set_nodelay(true) {
        eprintln!("[tunnel] {who} 设置 TCP_NODELAY 失败(忽略): {e}");
    }
}

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

// ── 全局状态 ────────────────────────────────────────────────────────────────
// 主机态与客户端态彻底分家:旧版共用一套 RUNNING/STATE/CLIENT_CONN,
// 桌面「既当主机又连 NAS」或接两块盘时互相踩踏(is_running 误判、连接错用)。

/// 活跃转发流总数(主机 accept 的 + 成员代理的),仅聚合展示用。
static CONNECTIONS: AtomicUsize = AtomicUsize::new(0);
/// 自定义 relay URL 列表;空 = 用 iroh 默认(n0 公共 relay)。
static RELAYS: Lazy<Mutex<Vec<String>>> = Lazy::new(|| Mutex::new(Vec::new()));

// 主机侧
static HOST_RUNNING: AtomicBool = AtomicBool::new(false);
/// start_host_blocking_thread 幂等闸:已有主机线程在跑就不再起第二个 accept 循环
/// (旧版重复 POST /tunnel/start 会叠多个同 key Endpoint,发现记录抖动)。
static HOST_STARTED: AtomicBool = AtomicBool::new(false);
static HOST_STOPPING: AtomicBool = AtomicBool::new(false);
static HOST_STATE: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new("stopped".to_string()));
static HOST_ERROR: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));
static HOST_NODE: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));
static HOST_SHUTDOWN: Lazy<tokio::sync::Notify> = Lazy::new(tokio::sync::Notify::new);

// 成员侧
/// 一条出站隧道:本地 127.0.0.1:port ↔ 主机 host_id。状态互相独立。
struct ClientTunnel {
    host_str: String,
    host_id: EndpointId,
    /// 主机的**已知直连地址**(`NodeId@1.2.3.4:41641` 形式给进来的那部分)。
    /// 非空 = 拨号时直接把这个地址喂给 iroh,不必等 n0 DNS 发现/中继中转 ——
    /// 对「公网固定 IP + 固定 UDP 口」的云主机能省掉整条中继绕路(实测 370ms → 直连)。
    /// 空 = 老行为:只凭 NodeId,由发现与中继找路。
    direct_addrs: Vec<std::net::SocketAddr>,
    port: u16,
    /// 隧道任务存活(创建即 true,任务退出置 false;状态细节看 state)。
    alive: AtomicBool,
    stopping: AtomicBool,
    state: Mutex<String>,
    last_error: Mutex<String>,
    conn: Mutex<Option<Connection>>,
    shutdown: tokio::sync::Notify,
}

impl ClientTunnel {
    fn set_state(&self, s: &str) {
        *self.state.lock().unwrap() = s.to_string();
    }
    fn set_error(&self, e: impl Into<String>) {
        *self.last_error.lock().unwrap() = e.into();
    }
}

/// 出站隧道注册表:port → 隧道。BTreeMap 保证 status 输出稳定有序。
static CLIENTS: Lazy<Mutex<BTreeMap<u16, Arc<ClientTunnel>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));

/// 隧道专用共享 runtime:所有客户端隧道任务跑在这几个 worker 上
/// (旧版每盘一线程一 runtime,N 盘 = 2N 线程)。主机侧仍独享一线程(见下)。
/// worker 数按机器核数走,见 [`tunnel_workers`]。
static TUNNEL_RT: Lazy<tokio::runtime::Runtime> = Lazy::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(tunnel_workers())
        .thread_name("polaris-tunnel")
        .enable_all()
        .build()
        .expect("tunnel runtime 创建失败")
});

/// 共享客户端 Endpoint(device.key 身份),首个隧道触发构建,之后所有隧道复用。
static CLIENT_EP: Lazy<tokio::sync::OnceCell<Endpoint>> = Lazy::new(tokio::sync::OnceCell::new);
/// 共享 Endpoint 建成后记下 NodeId,status 聚合展示用。
static DEVICE_NODE: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));

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
/// 注意:客户端共享 Endpoint 建成后改 relay,要等全部隧道断开重连才生效(Endpoint 级配置)。
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

/// 用种子建 Endpoint:N0 预设(打洞 + n0 DNS 发现 + 默认 relay)+ mDNS 内网发现,
/// 有自定义 relay 则覆盖 relay_mode。
async fn build_endpoint(seed: [u8; 32]) -> Result<Endpoint, String> {
    // 保活 + 判死超时:没有它,NAT/relay 会静默回收空闲连接,直到下次请求才暴露断线。
    //
    // 流量窗口:QUIC 的默认值按「100ms RTT × 100Mbps」调,单流 1.25MB、连接级不设上限。
    // 两头都不合适 ——
    //  · 单流 1.25MB 在**高延迟长肥管**(比如跨境 200ms)上会把单流封在 6MB/s 左右,
    //    而这正是「拷一个大文件」的形态;放到 4MB 才够喂满。
    //  · 连接级不设上限意味着最坏情况是 `并发流数 × 单流窗口`,没有硬约束 —— 对
    //    1.6G 内存的小云机是个隐患。这里显式钉一个连接级总量(默认 32MB)作为**内存上限**,
    //    既拿到单流的速度,又不会因为对端猛开流把内存吃穿。
    // 两个都可用 env 覆盖(单位 KB):内存紧张的 NAS 调小,专线大带宽调大。
    let stream_rwnd = env_kb("POLARIS_QUIC_STREAM_WINDOW_KB", 4 * 1024).clamp(64 * 1024, 64 << 20);
    let conn_rwnd = env_kb("POLARIS_QUIC_CONN_WINDOW_KB", 32 * 1024)
        .clamp(stream_rwnd, 512 << 20);
    let transport = QuicTransportConfig::builder()
        .keep_alive_interval(KEEP_ALIVE)
        .max_idle_timeout(Some(
            IdleTimeout::try_from(IDLE_TIMEOUT).expect("idle timeout 常量合法"),
        ))
        .stream_receive_window(VarInt::from_u64(stream_rwnd).expect("窗口值已夹在合法区间"))
        .receive_window(VarInt::from_u64(conn_rwnd).expect("窗口值已夹在合法区间"))
        // 发送侧同样放到连接级上限,否则收端给了窗口发端也吐不出来。
        .send_window(conn_rwnd)
        .build();
    let mut builder = Endpoint::builder(presets::N0)
        .secret_key(SecretKey::from_bytes(&seed))
        .transport_config(transport)
        .alpns(vec![ALPN.to_vec()]);
    // 固定 UDP 端口(`POLARIS_IROH_PORT`)。默认是随机口,这对**跑在容器/NAT 后面的主机**
    // 是致命的:端口每次重启都变,docker 没法发布、防火墙没法放行 → 外面永远打不进来,
    // 打洞必失败、只能落中继(实测绕到 n0 德国中继,每跳往返 370ms)。钉死端口后
    // `-p 41641:41641/udp` + 云防火墙放行这一个口,成员端即可直连。0 = 保持随机。
    if let Some(p) = env_u16("POLARIS_IROH_PORT") {
        builder = builder
            .bind_addr((std::net::Ipv4Addr::UNSPECIFIED, p))
            .map_err(|e| format!("POLARIS_IROH_PORT={p} 绑定失败: {e}"))?;
    }
    // 自建中继也允许 env 配(容器重启即生效),不必每次重启后再 POST 一次
    // /api/collab/tunnel/relays —— 服务端进程重启后 RELAYS 是空的,靠人工补那一刀
    // 迟早会漏。env 只在 RELAYS 还没被 API 设过时兜底。
    if RELAYS.lock().unwrap().is_empty() {
        if let Ok(s) = std::env::var("POLARIS_RELAYS") {
            let list: Vec<String> = s
                .split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect();
            if !list.is_empty() {
                *RELAYS.lock().unwrap() = list;
            }
        }
    }
    let relays = RELAYS.lock().unwrap().clone();
    if !relays.is_empty() {
        let mut urls: Vec<RelayUrl> = Vec::new();
        for u in &relays {
            urls.push(u.parse().map_err(|e| format!("relay url 非法 {u}: {e}"))?);
        }
        builder = builder.relay_mode(RelayMode::custom(urls));
    }
    let ep = builder
        .bind()
        .await
        .map_err(|e| format!("iroh endpoint 绑定失败: {e}"))?;
    // mDNS 局域网发现(iroh-mdns-address-lookup):纯内网(n0 DNS/relay 全不可达)时,
    // 同网段设备也能互相发现直连。尽力而为:失败只丢内网发现,不影响正常组网;
    // POLARIS_MDNS=0 可关(某些企业网禁组播,mDNS 只会白噪音)。
    let mdns_off = std::env::var("POLARIS_MDNS").map(|v| v == "0").unwrap_or(false);
    if !mdns_off {
        match iroh_mdns_address_lookup::MdnsAddressLookup::builder().build(ep.id()) {
            Ok(mdns) => match ep.address_lookup() {
                Ok(services) => services.add(mdns),
                Err(e) => eprintln!("[tunnel] mDNS 挂载失败(忽略,仅失去纯内网发现): {e}"),
            },
            Err(e) => eprintln!("[tunnel] mDNS 启动失败(忽略,仅失去纯内网发现): {e}"),
        }
    }
    Ok(ep)
}

/// 共享客户端 Endpoint:首用构建(device.key),失败可重试(OnceCell 不留错值)。
async fn client_endpoint() -> Result<Endpoint, String> {
    let ep = CLIENT_EP
        .get_or_try_init(|| async {
            let seed = get_or_create_device_key()?;
            let ep = build_endpoint(seed).await?;
            *DEVICE_NODE.lock().unwrap() = ep.id().to_string();
            Ok::<Endpoint, String>(ep)
        })
        .await?;
    Ok(ep.clone())
}

// ── 主机侧 ──────────────────────────────────────────────────────────────────

/// 主机监听:accept 循环直到 stop()。strict 模式过白名单闸,每条双向流转发到上游 TCP。
pub async fn host_listen() -> Result<(), String> {
    HOST_STOPPING.store(false, Ordering::SeqCst);
    *HOST_STATE.lock().unwrap() = "connecting".to_string();
    let seed = identity::get_or_create_host_key()?;
    let ep = match build_endpoint(seed).await {
        Ok(ep) => ep,
        Err(e) => {
            *HOST_ERROR.lock().unwrap() = e.clone();
            *HOST_STATE.lock().unwrap() = "stopped".to_string();
            return Err(e);
        }
    };
    *HOST_NODE.lock().unwrap() = ep.id().to_string();
    HOST_RUNNING.store(true, Ordering::SeqCst);
    *HOST_STATE.lock().unwrap() = "connected".to_string();
    HOST_ERROR.lock().unwrap().clear();
    eprintln!("[tunnel] 主机隧道已启动 node_id={}", ep.id());

    loop {
        if HOST_STOPPING.load(Ordering::SeqCst) {
            break;
        }
        let incoming = tokio::select! {
            _ = HOST_SHUTDOWN.notified() => break,
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
                    nodelay(&tcp, "主机→上游");
                    let mut stream = tokio::io::join(recv, send);
                    let buf = copy_buf_bytes();
                    let _ =
                        tokio::io::copy_bidirectional_with_sizes(&mut tcp, &mut stream, buf, buf)
                            .await;
                });
            }
        });
    }

    HOST_RUNNING.store(false, Ordering::SeqCst);
    *HOST_STATE.lock().unwrap() = "stopped".to_string();
    ep.close().await;
    eprintln!("[tunnel] 主机隧道已停止");
    Ok(())
}

// ── 成员侧 ──────────────────────────────────────────────────────────────────

/// 取一条到该隧道主机的健康连接:缓存里已死(close_reason=Some)的直接丢弃重建。
/// 读一个「以 KB 为单位」的环境变量,返回字节数;缺失/写错/为 0 用 `default_kb`。
/// 配置写错不拒绝启动 —— 隧道断了比慢一点严重得多。
fn env_kb(key: &str, default_kb: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default_kb)
        * 1024
}

/// 读一个非零的 u16 环境变量;缺失/写错/为 0 都当没设(不为一个配置错误拒绝启动)。
fn env_u16(key: &str) -> Option<u16> {
    std::env::var(key).ok()?.trim().parse::<u16>().ok().filter(|p| *p != 0)
}

/// 拆 `NodeId` 与可选的已知直连地址:`<nodeid>` 或 `<nodeid>@1.2.3.4:41641[,5.6.7.8:41641]`。
/// 地址写错一律报错而不是静默丢弃 —— 悄悄退回中继绕路,现象是「能用但慢 30 倍」,
/// 那种故障没人查得出来。
fn split_node_and_addrs(s: &str) -> Result<(String, Vec<std::net::SocketAddr>), String> {
    let s = s.trim();
    let Some((id, rest)) = s.split_once('@') else {
        return Ok((s.to_string(), Vec::new()));
    };
    let mut addrs = Vec::new();
    for part in rest.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        addrs.push(
            part.parse::<std::net::SocketAddr>()
                .map_err(|e| format!("直连地址 {part} 非法(要 IP:端口): {e}"))?,
        );
    }
    Ok((id.trim().to_string(), addrs))
}

async fn healthy_conn(t: &ClientTunnel, ep: &Endpoint) -> Result<Connection, String> {
    let cached = t.conn.lock().unwrap().clone();
    if let Some(c) = cached {
        if c.close_reason().is_none() {
            return Ok(c);
        }
        // 连接已死:清缓存,走重建。
        *t.conn.lock().unwrap() = None;
    }
    t.set_state("connecting");
    // 有已知直连地址就连地址(iroh 仍会并行试中继,谁先通用谁,通了之后择优走直连路径);
    // 没有就退回只凭 NodeId,由 n0 DNS 发现 + 中继找路。
    let mut target = iroh::EndpointAddr::new(t.host_id);
    for a in &t.direct_addrs {
        target = target.with_ip_addr(*a);
    }
    let c = ep
        .connect(target, ALPN)
        .await
        .map_err(|e| format!("连主机失败: {e}"))?;
    *t.conn.lock().unwrap() = Some(c.clone());
    t.set_state("connected");
    t.set_error("");
    eprintln!("[tunnel] 已连上主机 {}(:{})", t.host_str, t.port);
    Ok(c)
}

/// 发起(或复用)一条出站隧道:127.0.0.1:listen_port ↔ host_node_id。幂等:
/// 同端口同主机且任务还活着 = 直接返回 Ok;同端口不同主机且活着 = 报错;
/// 死任务(启动失败/已停)= 拆掉重建。坏 NodeId 在这里同步报错,不再吞进后台线程。
pub fn connect_client(host_node_id: &str, listen_port: u16) -> Result<(), String> {
    let (host_str, direct_addrs) = split_node_and_addrs(host_node_id)?;
    let host_id: EndpointId = host_str
        .parse()
        .map_err(|e| format!("主机 NodeId 非法: {e}"))?;
    let t = {
        let mut reg = CLIENTS.lock().unwrap();
        if let Some(existing) = reg.get(&listen_port) {
            if existing.alive.load(Ordering::SeqCst) {
                if existing.host_str == host_str {
                    return Ok(()); // 已在跑,幂等
                }
                return Err(format!("本地端口 {listen_port} 已被另一远程源占用"));
            }
            reg.remove(&listen_port); // 死条目,重建
        }
        let t = Arc::new(ClientTunnel {
            host_str,
            host_id,
            direct_addrs,
            port: listen_port,
            alive: AtomicBool::new(true),
            stopping: AtomicBool::new(false),
            state: Mutex::new("connecting".to_string()),
            last_error: Mutex::new(String::new()),
            conn: Mutex::new(None),
            shutdown: tokio::sync::Notify::new(),
        });
        reg.insert(listen_port, t.clone());
        t
    };
    TUNNEL_RT.spawn(run_client(t));
    Ok(())
}

/// 拆除一条出站隧道(移除远程盘时用):通知任务退出并摘出注册表。幂等。
pub fn disconnect_client(listen_port: u16) {
    let t = CLIENTS.lock().unwrap().remove(&listen_port);
    if let Some(t) = t {
        t.stopping.store(true, Ordering::SeqCst);
        t.shutdown.notify_waiters();
        eprintln!("[tunnel] 已拆除隧道 127.0.0.1:{listen_port}");
    }
}

/// 一条出站隧道的主体:本地 TCP accept → 共享 Endpoint 上到主机的 QUIC 双向流。
/// 多条 TCP 复用同一条 QUIC 连接(多路流)。后台巡检每 10s 查连接健康,
/// 断了主动重连(退避 1s→2s→…→30s 封顶),不再等下一个请求才暴露断线。
async fn run_client(t: Arc<ClientTunnel>) {
    // 收尾统一走这里:任务退出即 alive=false,connect_client 见死条目会重建。
    let finish = |t: &ClientTunnel| {
        t.alive.store(false, Ordering::SeqCst);
        t.set_state("stopped");
        let conn = t.conn.lock().unwrap().take();
        if let Some(c) = conn {
            c.close(0u32.into(), b"tunnel closed");
        }
    };

    let ep = match client_endpoint().await {
        Ok(ep) => ep,
        Err(e) => {
            t.set_error(&e);
            eprintln!("[tunnel] 共享 endpoint 构建失败: {e}");
            finish(&t);
            return;
        }
    };
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", t.port)).await {
        Ok(l) => l,
        Err(e) => {
            t.set_error(format!("本地端口 {} 监听失败: {e}", t.port));
            eprintln!("[tunnel] 本地端口 {} 监听失败: {e}", t.port);
            finish(&t);
            return;
        }
    };
    eprintln!(
        "[tunnel] 成员代理已启动 127.0.0.1:{} → {}",
        t.port, t.host_str
    );

    // 后台健康巡检:发现连接死亡 → 标 reconnecting 并主动重连,退避封顶 30s;
    // 重连成功即恢复 connected。空闲期断线由此兜住(QUIC keepalive 保证判死及时)。
    let watchdog = {
        let t = t.clone();
        let ep = ep.clone();
        TUNNEL_RT.spawn(async move {
            let mut backoff = Duration::from_secs(1);
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                if t.stopping.load(Ordering::SeqCst) {
                    break;
                }
                let dead = t
                    .conn
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|c| c.close_reason().is_some())
                    .unwrap_or(false);
                if !dead {
                    backoff = Duration::from_secs(1);
                    continue;
                }
                t.set_state("reconnecting");
                loop {
                    match healthy_conn(&t, &ep).await {
                        Ok(_) => {
                            backoff = Duration::from_secs(1);
                            break;
                        }
                        Err(e) => {
                            t.set_state("reconnecting");
                            t.set_error(&e);
                            eprintln!(
                                "[tunnel] :{} 重连失败({e}),{}s 后再试",
                                t.port,
                                backoff.as_secs()
                            );
                            tokio::time::sleep(backoff).await;
                            backoff = (backoff * 2).min(Duration::from_secs(30));
                            if t.stopping.load(Ordering::SeqCst) {
                                return;
                            }
                        }
                    }
                }
            }
        })
    };

    // 首连(失败不退出,交给巡检/下个请求重试)。
    if let Err(e) = healthy_conn(&t, &ep).await {
        t.set_error(&e);
        eprintln!("[tunnel] :{} 首次连主机失败: {e}", t.port);
    }

    loop {
        if t.stopping.load(Ordering::SeqCst) {
            break;
        }
        let (mut tcp, _peer) = tokio::select! {
            _ = t.shutdown.notified() => break,
            acc = listener.accept() => match acc {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("[tunnel] :{} 本地 accept 失败: {e}", t.port);
                    continue;
                }
            },
        };
        // 取健康连接开流;开流失败(竞态死亡)再重建一次。
        let bi = match healthy_conn(&t, &ep).await {
            Ok(c) => match c.open_bi().await {
                Ok(s) => Some(s),
                Err(e) => {
                    eprintln!("[tunnel] :{} 开流失败({e}),重建连接", t.port);
                    *t.conn.lock().unwrap() = None;
                    match healthy_conn(&t, &ep).await {
                        Ok(c2) => c2.open_bi().await.ok(),
                        Err(_) => None,
                    }
                }
            },
            Err(e) => {
                t.set_error(&e);
                eprintln!("[tunnel] :{} {e}", t.port);
                None
            }
        };
        let Some((send, recv)) = bi else { continue };
        nodelay(&tcp, "成员←本地");
        tokio::spawn(async move {
            let _guard = ConnGuard::new();
            let mut stream = tokio::io::join(recv, send);
            let buf = copy_buf_bytes();
            let _ =
                tokio::io::copy_bidirectional_with_sizes(&mut tcp, &mut stream, buf, buf).await;
        });
    }

    watchdog.abort();
    finish(&t);
    eprintln!("[tunnel] 成员代理已停止 127.0.0.1:{}", t.port);
    // 注:共享 Endpoint 不关,别的隧道还在用;进程退出统一回收。
}

// ── 状态 / 停止 / 同步包装 ───────────────────────────────────────────────────

/// 当前选中路径的链路类型与 RTT:direct=打洞直连,relay=中继兜底。
/// 「NAS 链路如实标 P2P」靠这里的真值,不再前端写死。
fn conn_path(c: &Connection) -> (&'static str, Option<u64>) {
    let paths = c.paths();
    if let Some(p) = paths.iter().find(|p| p.is_selected()) {
        let kind = if p.is_relay() { "relay" } else { "direct" };
        return (kind, Some(p.rtt().as_millis() as u64));
    }
    ("", c.rtt(PathId::ZERO).map(|d| d.as_millis() as u64))
}

/// 隧道状态。顶层字段与旧版兼容(CollabView 徽标直接读),新增:
/// - `tunnels`: 每条出站隧道明细 [{port, host_node_id, running, state, path, latency_ms, last_error}]
///   其中 path: "direct"(打洞直连) | "relay"(中继兜底) | ""(未连上)。
/// - `host`: {running, state, node_id} 主机侧独立状态,不再与成员侧混一个值。
pub fn status() -> serde_json::Value {
    let host_running = HOST_RUNNING.load(Ordering::SeqCst);
    let clients: Vec<Arc<ClientTunnel>> = CLIENTS.lock().unwrap().values().cloned().collect();

    let mut tunnels = Vec::new();
    let mut best_latency: Option<u64> = None;
    let mut states: Vec<String> = Vec::new();
    let mut first_err = String::new();
    for t in &clients {
        let conn = t.conn.lock().unwrap().clone();
        let (path, latency) = conn
            .as_ref()
            .filter(|c| c.close_reason().is_none())
            .map(conn_path)
            .unwrap_or(("", None));
        if let Some(l) = latency {
            best_latency = Some(best_latency.map_or(l, |b| b.min(l)));
        }
        let state = t.state.lock().unwrap().clone();
        let err = t.last_error.lock().unwrap().clone();
        if first_err.is_empty() && !err.is_empty() {
            first_err = err.clone();
        }
        states.push(state.clone());
        tunnels.push(serde_json::json!({
            "port": t.port,
            "host_node_id": t.host_str,
            "running": t.alive.load(Ordering::SeqCst),
            "state": state,
            "path": path,
            "latency_ms": latency,
            "last_error": err,
        }));
    }

    // 聚合 state:reconnecting > connecting > connected > stopped(主机在跑算 connected)。
    let host_state = HOST_STATE.lock().unwrap().clone();
    if host_running {
        states.push(host_state.clone());
    }
    let agg = ["reconnecting", "connecting", "connected"]
        .iter()
        .find(|s| states.iter().any(|x| x == *s))
        .copied()
        .unwrap_or("stopped");

    let host_err = HOST_ERROR.lock().unwrap().clone();
    let host_node = HOST_NODE.lock().unwrap().clone();
    let device_node = DEVICE_NODE.lock().unwrap().clone();
    let any_client_alive = clients.iter().any(|t| t.alive.load(Ordering::SeqCst));

    serde_json::json!({
        "running": host_running || any_client_alive,
        "state": agg,
        "node_id": if host_running && !host_node.is_empty() { host_node.clone() } else { device_node },
        "connections": CONNECTIONS.load(Ordering::Relaxed),
        "upstream": upstream_addr(),
        "relays": RELAYS.lock().unwrap().clone(),
        "latency_ms": best_latency,
        "last_error": if !host_err.is_empty() { host_err } else { first_err },
        "host": { "running": host_running, "state": host_state, "node_id": host_node },
        "tunnels": tunnels,
    })
}

/// 本机主机 NodeId(不绑 Endpoint,直接由 host.key 种子导出)。挂牌到云机网关时上报用。
pub fn host_node_id() -> Result<String, String> {
    let seed = identity::get_or_create_host_key()?;
    Ok(SecretKey::from_bytes(&seed).public().to_string())
}

/// 主机隧道是否在跑(仅主机侧;成员侧隧道看 CLIENTS 注册表)。
/// 挂牌/hosting 据此判断要不要起 host_listen —— 旧版主客共用一个 RUNNING,
/// 连着 NAS 时误判「主机已在跑」导致 host_listen 永远不启动。
pub fn is_running() -> bool {
    HOST_RUNNING.load(Ordering::SeqCst)
}

/// 停掉全部隧道(主机 + 所有出站),应用退出/测试收尾用。
pub fn stop() {
    HOST_STOPPING.store(true, Ordering::SeqCst);
    HOST_SHUTDOWN.notify_waiters();
    let ports: Vec<u16> = CLIENTS.lock().unwrap().keys().copied().collect();
    for p in ports {
        disconnect_client(p);
    }
}

/// 同步包装:独立线程 + 独立 tokio runtime 跑主机隧道,供非 async 调用方(桌面/服务器启动路径)。
/// 幂等:已有主机线程在跑则返回 None,不再叠第二个 accept 循环。
pub fn start_host_blocking_thread() -> Option<std::thread::JoinHandle<Result<(), String>>> {
    if HOST_STARTED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return None;
    }
    let handle = std::thread::Builder::new()
        .name("polaris-tunnel-host".into())
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(tunnel_workers())
                .enable_all()
                .build()
                .map_err(|e| format!("tokio runtime 创建失败: {e}"))?;
            let res = rt.block_on(host_listen());
            HOST_STARTED.store(false, Ordering::SeqCst);
            res
        })
        .expect("spawn tunnel host thread");
    Some(handle)
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

    /// `NodeId@地址` 拆分:无 @ 保持老行为;有 @ 拆出地址;地址写错必须报错
    /// (静默丢弃会退回中继绕路,表现是「能用但慢 30 倍」,查不出来)。
    #[test]
    fn node_and_addrs_split() {
        let (id, a) = split_node_and_addrs("  abc123  ").expect("纯 NodeId");
        assert_eq!(id, "abc123");
        assert!(a.is_empty());

        let (id, a) =
            split_node_and_addrs("abc123@1.2.3.4:41641, 5.6.7.8:41641").expect("带两个地址");
        assert_eq!(id, "abc123");
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].port(), 41641);

        assert!(split_node_and_addrs("abc123@1.2.3.4").is_err()); // 缺端口
        assert!(split_node_and_addrs("abc123@不是地址:41641").is_err());
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

    /// connect_client 幂等与冲突:坏 NodeId 同步报错;同端口同主机 no-op;
    /// 同端口不同主机报「被占用」。NodeId 用固定种子构造(不碰 env/文件,
    /// 免与 device_key_idempotent 抢 POLARIS_DEVICE_KEY);连接在后台重试,不阻塞断言。
    #[test]
    fn connect_idempotent_and_conflict() {
        assert!(
            connect_client("not-a-node-id", 39990).is_err(),
            "坏 NodeId must 同步报错"
        );

        let nid = SecretKey::from_bytes(&[7u8; 32]).public().to_string();
        let nid2 = SecretKey::from_bytes(&[8u8; 32]).public().to_string();
        assert_ne!(nid, nid2);
        connect_client(&nid, 39991).expect("首连发起");
        connect_client(&nid, 39991).expect("同端口同主机幂等");
        assert!(
            connect_client(&nid2, 39991).is_err(),
            "同端口不同主机 must 报占用"
        );
        disconnect_client(39991);
    }

    /// 纯内网自证:两端 Endpoint 关死 relay、不带 n0 DNS 发现,只挂 mDNS ——
    /// 能凭 EndpointId 连通并回显,即证明断外网时同局域网设备照样成网。
    /// POLARIS_NET_TEST=1 门控(mDNS 走组播,部分 CI/企业网禁,不进默认闸)。
    #[test]
    fn mdns_lan_only_roundtrip() {
        if std::env::var("POLARIS_NET_TEST").map(|v| v == "1").unwrap_or(false) == false {
            eprintln!("[tunnel-test] 未设 POLARIS_NET_TEST=1,跳过 mDNS 纯内网测试");
            return;
        }
        use iroh_mdns_address_lookup::MdnsAddressLookup;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            // 主机端:Minimal 预设 + relay 禁用,唯一的发现渠道是 mDNS。
            let host_ep = Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::Disabled)
                .alpns(vec![ALPN.to_vec()])
                .bind()
                .await
                .expect("host endpoint");
            host_ep
                .address_lookup()
                .expect("host address_lookup")
                .add(MdnsAddressLookup::builder().build(host_ep.id()).expect("host mdns"));
            let host_id = host_ep.id();
            tokio::spawn({
                let ep = host_ep.clone();
                async move {
                    while let Some(inc) = ep.accept().await {
                        tokio::spawn(async move {
                            let Ok(acc) = inc.accept() else { return };
                            let Ok(conn) = acc.await else { return };
                            while let Ok((mut send, mut recv)) = conn.accept_bi().await {
                                tokio::spawn(async move {
                                    let _ = tokio::io::copy(&mut recv, &mut send).await;
                                    let _ = send.finish();
                                });
                            }
                        });
                    }
                }
            });

            // 成员端:同样只有 mDNS,凭 EndpointId 找主机。
            let cli_ep = Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::Disabled)
                .bind()
                .await
                .expect("client endpoint");
            cli_ep
                .address_lookup()
                .expect("client address_lookup")
                .add(MdnsAddressLookup::builder().build(cli_ep.id()).expect("client mdns"));

            let conn = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                cli_ep.connect(host_id, ALPN),
            )
            .await
            .expect("30s 内应经 mDNS 发现主机")
            .expect("mDNS-only 连接成功");
            let (mut send, mut recv) = conn.open_bi().await.expect("开流");
            send.write_all(b"lan-only-ping").await.unwrap();
            let _ = send.finish();
            let mut got = vec![0u8; b"lan-only-ping".len()];
            tokio::time::timeout(std::time::Duration::from_secs(10), recv.read_exact(&mut got))
                .await
                .expect("10s 内应收到回显")
                .unwrap();
            assert_eq!(&got[..], b"lan-only-ping", "纯内网回显必须一致");
        });
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

            // 2) 白名单放行自己的设备 NodeId。默认(非 strict)模式隧道不查白名单,
            //    这步只为顺手覆盖 strict 路径 —— 失败(如与其它测试共库时 user 1 不存在
            //    触发外键)不致命,别让环境耦合挂掉网络主链路断言。
            let my_node = node_id_of_device_key().unwrap();
            if let Err(e) = identity::add_device(1, "loopback-test", &my_node) {
                eprintln!("[tunnel-test] 加白名单失败(忽略,默认模式不查白名单): {e}");
            }

            // 3) 起主机隧道,拿主机 NodeId
            let host_seed = identity::get_or_create_host_key().unwrap();
            let host_node = SecretKey::from_bytes(&host_seed).public().to_string();
            tokio::spawn(async { host_listen().await.unwrap() });
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            // 4) 起成员隧道(共享 Endpoint + 注册表路径,即生产同款)
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = l.local_addr().unwrap().port();
            drop(l);
            connect_client(&host_node, port).expect("发起成员隧道");
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
