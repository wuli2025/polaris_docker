//! 桌面「把这台电脑设为主机」:进程内嵌 axum,只挂协作端点(/api/collab/*、/git/*、/ws、/api/health)。
//! 不碰 kb/chat 等模块 —— 它们已由桌面 setup 初始化,这里只是给同事开一扇协作门。
//!
//! 端口:默认从 8484 起扫到 8494(避开 8080 —— Docker Desktop/wslrelay 常把
//! localhost:8080 的 IPv6 侧占掉,踩过「连到别人家服务」的坑)。
//! 事件:内嵌服务有自己的 broadcast 频道(远端成员走 /ws 收);另起桥接任务把
//! 事件转发给本机 Tauri UI(主机人自己的看板实时刷新)。
#![cfg(feature = "desktop")]

use crate::apihub::{api_router, ApiState};
use crate::collab::http::{collab_router, detect_advertise_urls, CollabState};
use crate::host::AppHandle as BusHandle;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

const PORT_SCAN: std::ops::Range<u16> = 8484..8495;

struct Running {
    /// 启动代次:每次 start 单调递增。占位与转正/回滚都比对代次,防 ABA——
    /// A 启动中被 stop 摘走占位、B 立即重启插入新占位后,A 完成时若只认「是占位」
    /// 就会认领 B 的占位(A 复活/B 自毁、旧 Child 被 drop 成孤儿)。
    gen: u64,
    port: u16,
    urls: Vec<String>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    /// axum serve 任务句柄:stop 时先给优雅窗口,超时 abort(drop listener 释放端口)。
    /// 只发 oneshot 不收尸的话,/ws 长连接会让 graceful shutdown 永远等不完 →
    /// 旧 serve 僵尸占 8484,再 start 落到 8485 双服务并存,反复停/开逐次泄漏端口。
    serve_task: Option<tokio::task::JoinHandle<()>>,
    /// 事件桥任务句柄(bridge_to_ui):stop 时 abort,防残留任务攒一批订阅者。
    bridge_task: Option<tauri::async_runtime::JoinHandle<()>>,
    /// tauri→bus 的对话流单向桥监听 id:stop 时 unlisten,防重复 start 累积监听。
    chat_bridge: Option<tauri::EventId>,
    /// 存一份 tauri 句柄仅为 stop 时能 unlisten 上面的 chat_bridge。
    app: tauri::AppHandle,
    /// 从不读取,纯 keep-alive:随 Running 一起 drop 时 broadcast 关闭,各 ws_loop 收尾。
    #[allow(dead_code)]
    bus: BusHandle,
    /// 仅经 Tauri IPC 返回给本机 UI，用于首次 bootstrap；绝不挂在 HTTP 路由上。
    access_token: String,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

#[derive(Serialize, Deserialize, Clone, Copy, Default)]
struct HostCfg {
    enabled: bool,
    port: Option<u16>,
    /// 允许 LAN/Tailscale 直连(绑 0.0.0.0)。打包版双击启动拿不到环境变量,
    /// 所以除 POLARIS_HOST_ALLOW_LAN 外也认这份持久化配置;数据面仍有口令/会话闸。
    #[serde(default)]
    allow_lan: bool,
}

fn cfg_path() -> Option<PathBuf> {
    directories::UserDirs::new().map(|u| u.home_dir().join("Polaris/data/collab_host.json"))
}

fn load_cfg() -> HostCfg {
    cfg_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_cfg(c: HostCfg) {
    if let Some(p) = cfg_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, serde_json::to_string(&c).unwrap_or_default());
    }
}

fn random_access_token() -> Result<String, String> {
    use std::fmt::Write as _;
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("生成主机访问口令失败: {e}"))?;
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(&mut token, "{byte:02x}");
    }
    Ok(token)
}

/// 本机 iroh 主机 NodeId(手机据此打洞 P2P 直连);未启用 collab-net 时为空串。
fn host_iroh_node_id() -> String {
    #[cfg(feature = "collab-net")]
    {
        crate::collab::tunnel::host_node_id().unwrap_or_default()
    }
    #[cfg(not(feature = "collab-net"))]
    {
        String::new()
    }
}

fn lan_access_enabled() -> bool {
    // 环境变量显式设置时优先(可强制关);未设置则看持久化配置。
    match std::env::var("POLARIS_HOST_ALLOW_LAN") {
        Ok(v) => v == "1" || v.eq_ignore_ascii_case("true"),
        Err(_) => load_cfg().allow_lan,
    }
}

/// 内核启动(不依赖 tauri,可单测):绑端口 → 建独立事件频道 → 起 axum(graceful shutdown)。
/// 返回 serve 任务的 JoinHandle:stop 侧靠它「优雅窗口 + 超时 abort」确认端口真正释放。
#[allow(clippy::type_complexity)]
pub async fn start_core(
    pref_port: Option<u16>,
    api_app: Option<tauri::AppHandle>,
) -> Result<
    (
        u16,
        Vec<String>,
        BusHandle,
        String,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    ),
    String,
> {
    let (tx, _rx) = tokio::sync::broadcast::channel::<crate::host::Event>(4096);
    // tx 需要克隆:一份包成 BusHandle 给 collab 事件面,一份给应用数据面 ApiState.tx 供 /ws 订阅。
    let bus = BusHandle::new(tx.clone());
    let configured_token = std::env::var("POLARIS_AUTH_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let access_token = match configured_token {
        Some(token) => token,
        None => random_access_token()?,
    };
    let auth_token = Some(access_token.clone());

    // 绑定地址:默认只绑 loopback(127.0.0.1)。远端成员经 iroh 隧道入内 —— 隧道有设备白名单,
    // 且把流量转发到本机 127.0.0.1(见 tunnel.rs upstream_addr),loopback 绑定对隧道零影响。
    // 这样同网段的人**无法直连**该端口打到裸明文 HTTP(此前 0.0.0.0 + 未初始化协作会兜底成 owner,
    // 等于对局域网敞开一个 owner 权限的裸接口)。
    // 想让 LAN 内不经隧道、按 IP 直连入伙 → 显式 POLARIS_HOST_ALLOW_LAN=1。即使管理员
    // 没有配置固定口令，本次进程也会生成 256-bit 随机口令，避免裸露 owner 接口。
    let allow_lan = lan_access_enabled();
    let bind_ip: &str = if allow_lan { "0.0.0.0" } else { "127.0.0.1" };

    let mut cands: Vec<u16> = Vec::new();
    if let Some(p) = pref_port {
        cands.push(p);
    }
    cands.extend(PORT_SCAN);
    let mut bound = None;
    for p in cands {
        if let Ok(l) = tokio::net::TcpListener::bind((bind_ip, p)).await {
            bound = Some((l, p));
            break;
        }
    }
    let (listener, port) = bound.ok_or("端口 8484-8494 全被占用,请释放一个再试")?;
    // 把真实端口告知隧道:此前隧道默认转发 8080 而内嵌服务在 8484-8494,
    // 未设 POLARIS_TUNNEL_UPSTREAM 时远端流量会打进黑洞。
    #[cfg(feature = "collab-net")]
    crate::collab::tunnel::set_upstream_port(port);
    let urls = detect_advertise_urls(port);

    // 口令 Arc 在 collab 面与应用数据面之间共享(同一把口令,同一份鉴权语义)。
    let auth_arc = Arc::new(auth_token);
    let state = CollabState {
        app: bus.clone(),
        auth_token: auth_arc.clone(),
        advertise: Arc::new(parking_lot::RwLock::new(urls.clone())),
    };
    // 有 tauri 句柄时挂应用数据面(invoke/upload/file/ws),让远端(手机/中继网关)拿到
    // 与 Docker server 壳一致的完整能力;此时 /ws 由 api_router 统一提供,collab 面不再
    // 单独挂 /ws(否则路由重叠 panic)—— api 的 ws_handler 订阅同一条 bus,collab 事件照收。
    let with_collab_ws = api_app.is_none();
    let mut router = collab_router(state, with_collab_ws)
        .route("/api/health", axum::routing::get(|| async { "ok" }));
    if let Some(app) = api_app {
        let api_state = ApiState {
            app,
            tx: tx.clone(),
            auth_token: auth_arc.clone(),
            // 恒 false:桌面主机的入场券是分享码里那把口令(上面 access_token),它就在
            // 分享码里、用户复制粘贴即得,不存在「忘了被锁死」的问题,故不走 Docker 那套
            // 免口令。远端成员还经 iroh 隧道转到本机 127.0.0.1,来源判定也认不出谁是谁。
            open_no_auth: false,
        };
        router = router.merge(api_router(api_state.clone()));
        // 应用直投的「粘性回退」:被代理应用页面里的绝对路径(/assets/x.js、/socket.io/…)
        // 不带 /app/{slug} 前缀,靠进门时种的 cookie 认回属于哪个应用。桌面壳根路径本来
        // 就没挂东西(前端是 tauri webview,不由这里托管),接管它零冲突;
        // Docker server 壳根上有 SPA(server.rs 已占 fallback),故这条只在桌面挂。
        let sticky: axum::Router = axum::Router::new().fallback(crate::appproxy::sticky_fallback);
        router = router.merge(sticky);
    }
    // 分享码探活 + 成员端 REST 都是跨源 → CORS 必开。普通 JSON 限 2MB；/api/upload
    // 在 api_router 内单独放宽到 512MB;git 路由在 collab_router 内单独放宽。
    let router = router
        .layer(axum::extract::DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(tower_http::cors::CorsLayer::permissive());

    // 远程文件浏览(fsface)开放根:设了 POLARIS_FS_ROOTS 才亮,提醒运维已把盘暴露给隧道对端。
    let fs_roots = crate::collab::fsface::roots();
    if !fs_roots.is_empty() {
        eprintln!("[fsface] 远程浏览已开放,根: {fs_roots:?}");
    }

    let (stx, srx) = tokio::sync::oneshot::channel::<()>();
    let serve_task = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = srx.await;
            })
            .await;
    });
    Ok((port, urls, bus, access_token, stx, serve_task))
}

/// 「启动中」占位(port=0 为标记):在 start_core(...).await **之前**原子占住 RUNNING,
/// 堵并发 start(autostart 与手点)在 await 窗口期双起并覆盖旧 Running —— 一旦覆盖,
/// 旧的 chat_bridge 监听 / bridge_task / serve_task 句柄全丢 = 永久泄漏。
/// bus 是临时哑频道,转正时整体替换;真实端口永远落在 8484-8494,port=0 不会撞。
static NEXT_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn starting_placeholder(app: tauri::AppHandle, gen: u64) -> Running {
    Running {
        gen,
        port: 0,
        urls: Vec::new(),
        shutdown: None,
        serve_task: None,
        bridge_task: None,
        chat_bridge: None,
        app,
        bus: BusHandle::new(tokio::sync::broadcast::channel(1).0),
        access_token: String::new(),
    }
}

/// 内嵌主机当前实际绑定的端口(未跑 / 启动占位期 = None)。
pub fn current_port() -> Option<u16> {
    RUNNING.lock().as_ref().map(|r| r.port).filter(|p| *p != 0)
}

/// 往内嵌主机的广播总线投一条事件(桌面 → 手机/中继的**唯一**出口)。
///
/// 返回是否真投出去了:主机没跑(或还在启动占位期)时总线只是个哑频道,投进去无人接 ——
/// 此时如实返回 false,让调用方能提示「还没设为主机,对端收不到」,而不是假装发出去了。
/// 注:总线上的事件会被 [`bridge_to_ui`] 回灌本机 tauri 事件系统,故调用方**不必**再本地 emit
/// 一遍,否则本机 UI 会收到两条一模一样的帧。
pub fn bus_emit(topic: &str, payload: serde_json::Value) -> bool {
    let g = RUNNING.lock();
    match g.as_ref() {
        Some(r) if r.port != 0 => {
            r.bus.emit_value(topic, payload);
            true
        }
        _ => false,
    }
}

fn status_json() -> serde_json::Value {
    let g = RUNNING.lock();
    // is_bootstrap()==true 表示还没有任何账号 → 前端应引导「初始化」而非「登录」。
    let needs_bootstrap = crate::collab::auth::is_bootstrap().unwrap_or(true);
    match g.as_ref() {
        // port=0 是「启动中占位」:还没真跑起来,按未运行上报(前端稍后轮询即见真状态)。
        Some(r) if r.port == 0 => serde_json::json!({
            "running": false, "starting": true, "port": 0, "urls": [],
            "needsBootstrap": needs_bootstrap, "autostart": load_cfg().enabled,
            "accessToken": "",
            "remoteAccess": false,
        }),
        Some(r) => serde_json::json!({
            "running": true, "port": r.port, "urls": r.urls,
            "needsBootstrap": needs_bootstrap, "autostart": load_cfg().enabled,
            "accessToken": r.access_token,
            "remoteAccess": lan_access_enabled(),
            "nodeId": host_iroh_node_id(),
        }),
        None => serde_json::json!({
            "running": false, "port": 0, "urls": [],
            "needsBootstrap": needs_bootstrap, "autostart": load_cfg().enabled,
            "accessToken": "",
            "remoteAccess": false,
        }),
    }
}

/// 事件桥:内嵌服务的广播 → 本机 Tauri UI(主机人自己的看板实时刷新)。
/// Lagged(积压超容被跳帧)必须 continue 而不是退出 —— `while let Ok(..)` 会在第一次
/// Lagged 时整个循环永久死亡,主机本机看板从此不再实时刷新且无任何报错
///(远端成员走 /ws 的 ws_loop 早已是 Lagged=>continue 的正确写法,这里对齐)。
fn bridge_to_ui(app: tauri::AppHandle, bus: &BusHandle) -> tauri::async_runtime::JoinHandle<()> {
    let mut rx = bus.subscribe();
    tauri::async_runtime::spawn(async move {
        use tauri::Emitter;
        use tokio::sync::broadcast::error::RecvError;
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    // chat:stream 由桌面 chat_send 直接 tauri emit 给本机 webview;bus 上的
                    // chat:stream 是 bridge_chat_to_bus 专为远端灌入的,若再 emit 回 tauri 会
                    // 与该桥形成回环(tauri→bus→tauri→…)。故本机 UI 桥跳过它。
                    if ev.topic == "chat:stream" {
                        continue;
                    }
                    let _ = app.emit(&ev.topic, ev.payload);
                }
                Err(RecvError::Lagged(n)) => {
                    eprintln!("[collab-host] UI 事件桥积压,跳过 {n} 条旧事件(继续转发)");
                    continue;
                }
                Err(RecvError::Closed) => break, // 主机已停,频道关闭 → 正常收工
            }
        }
    })
}

/// 对话流单向桥:桌面 chat_send 把 `chat:stream` emit 到 tauri 事件系统(直达本机
/// webview),这里 `listen` 捕获后再灌进内嵌主机的广播 bus → api_router 的 /ws 推给
/// 远端(手机/中继网关)。这样对话逐字流既到本机 UI、也到远端,而 chat_send/pipeline
/// 一行不改。返回监听 id,stop 时 unlisten。
fn bridge_chat_to_bus(app: &tauri::AppHandle, bus: &BusHandle) -> tauri::EventId {
    use tauri::Listener;
    let bus = bus.clone();
    app.listen("chat:stream", move |event| {
        // event.payload() 是已序列化的 JSON 串;解析回 Value 再进 bus。走 emit_value 直通
        // (泛型 emit 对 Value 入参会经 to_value 整树深拷贝一遍),Event 构造时预序列化一次
        // WS 帧,N 条远端连接共享 —— 逐字流热路径全程只有「1 次解析 + 1 次序列化」。
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(event.payload()) {
            bus.emit_value("chat:stream", v);
        }
    })
}

#[tauri::command]
pub async fn collab_host_start(
    app: tauri::AppHandle,
    port: Option<u16>,
) -> Result<serde_json::Value, String> {
    // ── 原子占位:检查与插入在同一把锁内完成。旧写法「检查通过 → start_core(...).await
    //    → 才插 RUNNING」中间隔着整个绑端口/建路由,并发 start 双起且互相覆盖 Running。
    let my_gen = NEXT_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    {
        let mut g = RUNNING.lock();
        if g.is_some() {
            drop(g);
            return Ok(status_json()); // 幂等:已在跑/正在启动就报状态
        }
        *g = Some(starting_placeholder(app.clone(), my_gen));
    }
    // 端口 0 消毒:配置里若残留 Some(0),bind(ip, 0) 会成功绑到随机端口并把「0」当真实
    // 端口上报,还与占位哨兵(port==0)撞车 —— stop 会把真服务当占位摘走造成泄漏。
    let pref = port.or(load_cfg().port).filter(|p| *p != 0);
    // 传入 tauri 句柄 → start_core 挂应用数据面(invoke/upload/file/ws);对话流经
    // bridge_chat_to_bus 灌进 bus 供远端 /ws。
    let (port, urls, bus, access_token, stx, serve_task) =
        match start_core(pref, Some(app.clone())).await {
            Ok(v) => v,
            Err(e) => {
                // 启动失败 → 回滚**自己那个代次**的占位。占位可能已被 stop 摘走、甚至
                // 已被更晚的 start 换成新占位(ABA)—— 只认 gen,绝不误删别人的。
                let mut g = RUNNING.lock();
                if g.as_ref()
                    .map(|r| r.port == 0 && r.gen == my_gen)
                    .unwrap_or(false)
                {
                    *g = None;
                }
                return Err(e);
            }
        };
    let bridge_task = bridge_to_ui(app.clone(), &bus);
    let chat_bridge = bridge_chat_to_bus(&app, &bus);
    // ── 转正:占位还在才写入。占位已被 collab_host_stop 摘走 = 启动窗口期间用户点了
    //    停止 → 立刻收掉刚起的这套(shutdown + abort + unlisten),不留泄漏、不改配置。
    let mut fresh = Some(Running {
        gen: my_gen,
        port,
        urls,
        shutdown: Some(stx),
        serve_task: Some(serve_task),
        bridge_task: Some(bridge_task),
        chat_bridge: Some(chat_bridge),
        app,
        bus,
        access_token,
    });
    {
        let mut g = RUNNING.lock();
        if let Some(r) = g.as_mut() {
            // 只认领自己代次的占位:占位被 stop 摘走后又被新 start 占上时(ABA),
            // 认领别人的占位=被停掉的实例复活+新实例自毁。
            if r.port == 0 && r.gen == my_gen {
                *r = fresh.take().expect("fresh 尚未被取走");
            }
        }
    }
    if let Some(f) = fresh {
        // 未转正 → 收掉刚起的这套栈,与 stop 同款优雅窗口(超时 abort 确保端口真正释放)。
        if let Some(s) = f.shutdown {
            let _ = s.send(());
        }
        if let Some(bt) = f.bridge_task {
            bt.abort();
        }
        if let Some(id) = f.chat_bridge {
            use tauri::Listener;
            f.app.unlisten(id);
        }
        if let Some(mut h) = f.serve_task {
            if tokio::time::timeout(std::time::Duration::from_secs(2), &mut h)
                .await
                .is_err()
            {
                h.abort();
                let _ = tokio::time::timeout(std::time::Duration::from_secs(1), &mut h).await;
            }
        }
        return Ok(status_json());
    }
    // ── iroh 主机隧道:桌面当主机时也起 host_listen,让手机经 iroh 打洞 P2P 连进来
    //    (打洞失败自动走 n0 relay 兜底)。手机 probeEntry 拿到主机 nodeId + 原生隧道可用
    //    → 优先起隧道直连桌面,而不是走裸地址。host_listen 有设备白名单硬闸。
    #[cfg(feature = "collab-net")]
    {
        if !crate::collab::tunnel::is_running() {
            crate::collab::tunnel::start_host_blocking_thread();
        }
    }
    save_cfg(HostCfg {
        enabled: true,
        port: Some(port),
        ..load_cfg()
    });
    Ok(status_json())
}

#[tauri::command]
pub fn collab_host_status() -> serde_json::Value {
    status_json()
}

/// 授权一台设备(把它的 iroh NodeId 加进白名单)。iroh host_listen 只放行白名单里的
/// NodeId —— 手机首次经 iroh 连本机前,owner 在「互联」把手机显示的 NodeId 加进来即可。
/// 幂等:已在白名单则直接返回成功。owner 级(经 tauri 命令,本机可信)。
#[tauri::command]
#[allow(non_snake_case)]
pub fn collab_host_add_device(name: String, nodeId: String) -> Result<(), String> {
    let nid = nodeId.trim();
    if nid.is_empty() || nid.len() > 256 || nid.chars().any(char::is_control) {
        return Err("设备 NodeId 无效".into());
    }
    if crate::collab::identity::is_node_allowed(nid) {
        return Ok(());
    }
    let nm = if name.trim().is_empty() {
        "我的设备"
    } else {
        name.trim()
    };
    crate::collab::identity::add_device(0, nm, nid).map(|_| ())
}

/// 切换「允许局域网 / Tailscale 直连」(绑 0.0.0.0)。持久化到 collab_host.json;主机在跑
/// 则重启以按新地址重绑,让 status 的 urls 带上局域网 IP —— 手机据此才够得着这台桌面主机。
/// 关掉则回绑 127.0.0.1(只本机可连,最安全)。数据面仍有口令/会话闸,不裸露 owner 接口。
#[tauri::command]
pub async fn collab_host_set_remote_access(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<serde_json::Value, String> {
    // 环境变量显式设置时优先于持久化配置(见 lan_access_enabled)→ UI 改不动,明确告知。
    if std::env::var("POLARIS_HOST_ALLOW_LAN").is_ok() {
        return Err("局域网访问由环境变量 POLARIS_HOST_ALLOW_LAN 固定,无法在此切换".into());
    }
    let mut cfg = load_cfg();
    cfg.allow_lan = enabled;
    save_cfg(cfg);
    // 主机在跑 → 停后重启,按新 bind_ip(0.0.0.0 / 127.0.0.1)重新绑定地址。
    let was_running = RUNNING.lock().is_some();
    if was_running {
        collab_host_stop().await?;
        collab_host_start(app, None).await
    } else {
        Ok(status_json())
    }
}

#[tauri::command]
pub async fn collab_host_stop() -> Result<serde_json::Value, String> {
    // ① 只取走信号与任务句柄,先不清 RUNNING:优雅窗口期间保持「运行中」,
    //    让并发的 start 走幂等分支返回,而不是抢先绑到 8485 造成双服务。
    //    (parking_lot 锁不能跨 await 持有 → 取出后立刻放锁。)
    let (shutdown, serve_task, bridge_task, chat_bridge, app) = {
        let mut g = RUNNING.lock();
        // 「启动中占位」(port=0):还没有任何真实句柄可收,直接摘掉占位即可 ——
        // in-flight 的 collab_host_start 转正时发现占位已消失,会自行收掉刚起的栈。
        if g.as_ref().map(|r| r.port == 0).unwrap_or(false) {
            *g = None;
            drop(g);
            save_cfg(HostCfg {
                enabled: false,
                ..load_cfg()
            });
            return Ok(status_json());
        }
        match g.as_mut() {
            Some(r) => (
                r.shutdown.take(),
                r.serve_task.take(),
                r.bridge_task.take(),
                r.chat_bridge.take(),
                Some(r.app.clone()),
            ),
            None => (None, None, None, None, None),
        }
    };
    // ② 发 graceful shutdown 信号(停止 accept 新连接)。
    if let Some(s) = shutdown {
        let _ = s.send(());
    }
    // ③ 事件桥是纯转发循环,直接 abort;对话流桥 unlisten(防重复 start 累积监听)。
    if let Some(bt) = bridge_task {
        bt.abort();
    }
    if let (Some(app), Some(id)) = (app, chat_bridge) {
        use tauri::Listener;
        app.unlisten(id);
    }
    // ④ 优雅窗口:graceful shutdown 会等所有在途连接结束,而 /ws 是长连接永不结束
    //    → 最多等 2s,超时就 abort(drop listener + 连接,端口立即释放),
    //    再短等 1s 确认任务真正退出。否则旧 serve 僵尸占 8484,stop 后立即 start
    //    只能落到 8485,反复停/开会把 8484-8494 逐个泄漏光。
    if let Some(mut h) = serve_task {
        if tokio::time::timeout(std::time::Duration::from_secs(2), &mut h)
            .await
            .is_err()
        {
            h.abort();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(1), &mut h).await;
        }
    }
    // ⑤ 全部确认退出后才清 RUNNING(顺带 drop bus → broadcast 关闭 → 各 ws_loop 收
    //    Closed 自行收尾)。stop 返回后立即 start 能重新绑回 8484。
    *RUNNING.lock() = None;
    save_cfg(HostCfg {
        enabled: false,
        ..load_cfg()
    });
    Ok(status_json())
}

/// App 启动时自动拉起(上次开过主机就续上,别让同事早上连不上)。
/// 走与手点完全相同的 collab_host_start 入口:同一套「原子占位 → 转正/回滚」防线,
/// 自启与手点并发时只会有一方真正起服务,另一方幂等返回,不再双起覆盖泄漏。
pub fn auto_start_if_enabled(app: tauri::AppHandle) {
    let cfg = load_cfg();
    if !cfg.enabled {
        return;
    }
    tauri::async_runtime::spawn(async move {
        match collab_host_start(app, cfg.port).await {
            Ok(s) => println!(
                "[collab-host] 主机自启成功,端口 {}",
                s.get("port").and_then(|v| v.as_u64()).unwrap_or(0)
            ),
            Err(e) => eprintln!("[collab-host] 主机自启失败: {e}"),
        }
    });
}

#[cfg(test)]
mod tests {
    #[tokio::test(flavor = "multi_thread")]
    async fn start_core_serves_health() {
        let _g = crate::collab::db::TEST_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("collab-host-{}.db", std::process::id()));
        std::env::set_var("POLARIS_COLLAB_DB", &tmp);
        // None = 不挂应用数据面(测试无 tauri 句柄),仅验 collab 面 + health 端点。
        let (port, _urls, _bus, _access_token, stx, serve_task) =
            super::start_core(None, None).await.expect("start_core");
        // ureq 是既有依赖:同步阻塞客户端,放 spawn_blocking 防塞 runtime。
        let body = tokio::task::spawn_blocking(move || {
            ureq::get(&format!("http://127.0.0.1:{port}/api/health"))
                .call()
                .expect("health call")
                .into_string()
                .expect("body")
        })
        .await
        .unwrap();
        assert_eq!(body, "ok");
        let _ = stx.send(());
        // 无 /ws 长连接时 graceful shutdown 应迅速完成;随后同端口必须能立刻重绑
        //(修复2的核心诉求:停机不泄漏端口,stop→start 能回到原端口)。
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), serve_task).await;
        let l = tokio::net::TcpListener::bind(("0.0.0.0", port))
            .await
            .expect("停机后应能立刻重绑同端口");
        drop(l);
        std::env::remove_var("POLARIS_COLLAB_DB");
        let _ = std::fs::remove_file(&tmp);
    }
}
