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
    extract::{ConnectInfo, DefaultBodyLimit, Multipart, Query, State, WebSocketUpgrade},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
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
    /// 「免口令」总闸(见 origin_gate)。仅 Docker server 壳在**管理员没显式设**
    /// POLARIS_AUTH_TOKEN 时置 true —— 家用 NAS 走的就是这一档:任何能连上的来源
    /// 直接进,不弹口令框(理由见下面「为什么默认免口令」)。桌面 hosting 恒 false:
    /// 它有自己的分享码口令,隧道对端流量还会被转成 127.0.0.1,不能按来源放行。
    pub open_no_auth: bool,
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
        .route("/api/auth/state", get(auth_state))
        .route("/api/auth/setup", post(auth_setup))
        .route("/api/invoke", post(invoke))
        .route(
            "/api/upload",
            post(upload).layer(DefaultBodyLimit::max(512 * 1024 * 1024)),
        )
        .route("/api/file", get(serve_file))
        .route("/api/exec", post(exec_ep))
        .route("/ws", get(ws_handler))
        .with_state(state.clone())
        // 应用直投:/app/{slug}/… 反代到本机在跑的 HTTP 应用(自带鉴权与 owner 闸)。
        .merge(crate::appproxy::routes(state))
}

// ───────────────────────── 鉴权 ─────────────────────────
//
// 基础应用面(对话/知识库/文件…)与 collab 面**分开**:维持历史语义——没设全局口令就
// 开放(合成 owner),避免"有人建了协作账号就把整个 App 锁死"。真会话 token 仍会升级成
// 真实身份并受命令角色闸约束。多用户部署要连基础命令也强制登录时设 POLARIS_REQUIRE_LOGIN=1。

/// 是否显式要求登录。抽出来是因为 exec 端点要独立判「开放模式」——
/// 开放模式会合成 owner 全放行,那对读接口尚可,对远程执行等于无鉴权 shell。
fn require_login_env() -> bool {
    std::env::var("POLARIS_REQUIRE_LOGIN")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

// ── 为什么默认不要访问口令(2026-07-25 起)──────────────────────────────────
//
// 访问口令这套东西在家用 NAS 上是**净负债**:
//  · NAS 挂在家用路由的 NAT 后面,拿到的是运营商动态分配、还常带 CGNAT 的地址,没做
//    端口映射时公网压根连不上;真要远程用,走的是 P2P/Tailscale 那条隧道,那条自带身份;
//  · 口令又最容易忘 —— 网页向导让人随手设一个,几周后没人记得,而且没有找回入口,
//    结果是用户被自己的软件锁在门外(这已经是第二次栽在口令上:上一次是随机生成的)。
// 所以现在:**没设 POLARIS_AUTH_TOKEN = 谁都不用口令**,不分内外网;网页里那个「首次
// 设置口令」向导一并撤掉(见 auth_setup),历史落盘的口令也不再生效 —— 否则老用户升级
// 上来照样被自己几周前随手设的那串东西挡在门外,这次改动就白做了。
//
// 要重新上锁只有一条明路,且必须是管理员的显式动作:
//  · `POLARIS_AUTH_TOKEN=<口令>` —— 所有来源一律校验;
//  · `POLARIS_REQUIRE_LOGIN=1`  —— 走账号体系,按人管权限。
// `/api/exec`(远程 shell)不吃免口令这条豁免,没真凭据永远拒绝 —— 见 exec_ep。

/// 生效口令 = **只认环境变量**。None = 这台机器不设口令(免口令模式)。
pub(crate) fn effective_token(env_token: &Option<String>) -> Option<String> {
    env_token.clone()
}

/// 反代场景下是否信任 X-Forwarded-For。默认不信:见 origin_gate 的取舍。
fn trust_proxy_env() -> bool {
    std::env::var("POLARIS_TRUST_PROXY")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// 私网/回环/本机直连网段 —— 这些地址不可能从公网路由过来。
/// 100.64.0.0/10 是 CGNAT 段,Tailscale 的 100.x 也落在这里(tailnet 自带设备白名单)。
fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private() // 10/8, 172.16/12, 192.168/16
                || v4.is_link_local() // 169.254/16
                || (o[0] == 100 && (64..=127).contains(&o[1])) // 100.64/10 CGNAT + Tailscale
        }
        IpAddr::V6(v6) => {
            // IPv4-mapped(::ffff:a.b.c.d)要拆回 v4 判,否则内网 v4 经 v6 套接字进来会被当公网。
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_ip(IpAddr::V4(v4));
            }
            let seg = v6.segments();
            v6.is_loopback()
                || (seg[0] & 0xfe00) == 0xfc00 // fc00::/7 ULA
                || (seg[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
        }
    }
}

/// 免口令模式下**是否还要看来源**。默认 0 = 不看(谁连上谁能用,家用 NAS 的日常)。
/// 显式 `POLARIS_LAN_ONLY=1` 才退回老行为:只放行内网来源、公网来源一律拒。
/// 给「机器确实有公网入口、又不想设口令」的部署留的中间档,普通 NAS 用户碰不到。
fn lan_only_env() -> bool {
    std::env::var("POLARIS_LAN_ONLY")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// 一次请求的免口令判定。分两半:`enabled` 是本壳允不允许走这条路(见
/// ApiState::open_no_auth),`origin_ok` 是这一条请求的来源够不够格
/// —— 默认恒 true,只有 POLARIS_LAN_ONLY=1 时才真去判内外网。
#[derive(Clone, Copy)]
pub(crate) struct OriginGate {
    enabled: bool,
    origin_ok: bool,
}

impl OriginGate {
    /// 这一条请求能不能吃「免口令」这条豁免。协作面的 bootstrap 闸复用它，
    /// 好让两个面用的是同一把尺子（含 POLARIS_LAN_ONLY 的内外网判定）。
    /// 只有 server 壳(与单测)用得到：桌面壳不走 bootstrap 免口令那条路。
    #[cfg(any(feature = "server", test))]
    pub(crate) fn is_open(&self) -> bool {
        self.enabled && self.origin_ok
    }
}

impl OriginGate {
    /// 关死:显式设过口令的部署、exec 这类高危端点、以及一切拿不准来源的地方走它。
    pub(crate) fn closed() -> Self {
        Self {
            enabled: false,
            origin_ok: false,
        }
    }
    fn allows(&self) -> bool {
        self.enabled && self.origin_ok
    }
}

/// 判来源。默认直接放行(免口令 = 不分内外网);只有 POLARIS_LAN_ONLY=1 时才按下面
/// 三条**保守优先**地判内网:
///  ① 拿不到对端地址(没挂 ConnectInfo / 非 TCP)→ 当公网,要口令。
///  ② 带了 X-Forwarded-For / X-Real-IP 说明前面有反代,此时对端 IP 恒是反代自己
///     (常是 127.0.0.1),按它判会把全世界当内网 —— 除非运维显式 POLARIS_TRUST_PROXY=1
///     声明这层反代可信,否则一律当公网。云机走 install-cloud.sh 必写死 POLARIS_AUTH_TOKEN,
///     enabled 本就是 false,这条只是再兜一层。
///  ③ 其余按 TCP 对端地址判私网。
fn origin_gate(enabled: bool, peer: Option<SocketAddr>, headers: &HeaderMap) -> OriginGate {
    // 管理员显式要求登录 = 要按人管权限,别在这儿偷偷放行。
    origin_gate_with(
        enabled && !require_login_env(),
        lan_only_env(),
        peer,
        headers,
    )
}

/// origin_gate 的纯函数内核(两个开关都由调用方传入)。测试直接打这层,免得改进程级
/// 环境变量跟并行跑的其它用例互踩。
fn origin_gate_with(
    enabled: bool,
    lan_only: bool,
    peer: Option<SocketAddr>,
    headers: &HeaderMap,
) -> OriginGate {
    if !lan_only {
        return OriginGate {
            enabled,
            origin_ok: true,
        };
    }
    let forwarded = headers
        .get("x-forwarded-for")
        .or_else(|| headers.get("x-real-ip"));
    let client_ip: Option<IpAddr> = match (forwarded, trust_proxy_env()) {
        (Some(v), true) => v
            .to_str()
            .ok()
            .and_then(|s| s.split(',').next())
            .and_then(|s| s.trim().parse().ok()),
        (Some(_), false) => None, // 有反代但未声明可信 → 判不了,当公网
        (None, _) => peer.map(|a| a.ip()),
    };
    OriginGate {
        enabled,
        origin_ok: client_ip.map(is_private_ip).unwrap_or(false),
    }
}

#[cfg(test)]
mod origin_gate_tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    /// 协作面 bootstrap 复用 `is_open()` 当闸(见 server.rs 的 from_fn 层)。
    /// 这里钉死它的两条语义：免口令且来源够格才为真；LAN_ONLY 下公网来源必须为假 ——
    /// 否则「只放行内网」的部署会被公网来客抢注第一个 owner 账号。
    #[test]
    fn is_open_是免口令与来源两个条件的与() {
        let h = HeaderMap::new();
        let lan = Some(SocketAddr::new(ip("192.168.1.5"), 5000));
        let wan = Some(SocketAddr::new(ip("8.8.8.8"), 5000));

        // 没开免口令 → 恒假，来源再内网也不放
        assert!(!origin_gate_with(false, false, lan, &h).is_open());
        assert!(!origin_gate_with(false, true, lan, &h).is_open());

        // 免口令 + 不分内外网(默认) → 谁都放
        assert!(origin_gate_with(true, false, lan, &h).is_open());
        assert!(origin_gate_with(true, false, wan, &h).is_open());
        assert!(origin_gate_with(true, false, None, &h).is_open());

        // 免口令 + LAN_ONLY → 只放内网；公网、以及拿不到对端地址的都拒
        assert!(origin_gate_with(true, true, lan, &h).is_open());
        assert!(!origin_gate_with(true, true, wan, &h).is_open());
        assert!(!origin_gate_with(true, true, None, &h).is_open());
    }

    #[test]
    fn 私网段判定() {
        for s in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.254",
            "192.168.1.5",
            "169.254.1.1",
            "100.64.0.1",
            "100.78.103.101", // 实测的 Tailscale 地址
            "100.127.255.254",
        ] {
            assert!(is_private_ip(ip(s)), "{s} 应判为内网");
        }
        for s in [
            "8.8.8.8",
            "1.1.1.1",
            "172.32.0.1",  // 172.16/12 的上界外
            "100.63.0.1",  // 100.64/10 的下界外
            "100.128.0.1", // 100.64/10 的上界外
            "11.0.0.1",
        ] {
            assert!(!is_private_ip(ip(s)), "{s} 应判为公网");
        }
    }

    #[test]
    fn ipv6_与_v4_映射() {
        assert!(is_private_ip(ip("::1")));
        assert!(is_private_ip(ip("fc00::1")));
        assert!(is_private_ip(ip("fe80::1")));
        // v4-mapped 必须拆回 v4 判,否则内网 v4 经 v6 套接字进来会被误判成公网
        assert!(is_private_ip(ip("::ffff:192.168.1.1")));
        assert!(!is_private_ip(ip("::ffff:8.8.8.8")));
        assert!(!is_private_ip(ip("2001:db8::1")));
    }

    fn gate(
        enabled: bool,
        lan_only: bool,
        peer: Option<&str>,
        hdrs: &[(&str, &str)],
    ) -> OriginGate {
        let mut h = HeaderMap::new();
        for (k, v) in hdrs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        let addr = peer.map(|s| SocketAddr::new(ip(s), 12345));
        origin_gate_with(enabled, lan_only, addr, &h)
    }

    #[test]
    fn 壳没开就一律不放行() {
        assert!(!gate(false, false, Some("192.168.1.5"), &[]).allows());
        assert!(!gate(false, true, Some("192.168.1.5"), &[]).allows());
    }

    #[test]
    fn 默认不分内外网一律放行() {
        // 现行默认:没设 POLARIS_AUTH_TOKEN = 谁都不用口令,来源不再是门槛。
        assert!(gate(true, false, Some("192.168.1.5"), &[]).allows());
        assert!(gate(true, false, Some("203.0.113.7"), &[]).allows());
        // 连「拿不到对端地址 / 反代后面」这些判不出来源的情况也照进,不再吃闭门羹。
        assert!(gate(true, false, None, &[]).allows());
        assert!(gate(
            true,
            false,
            Some("127.0.0.1"),
            &[("x-forwarded-for", "8.8.8.8")]
        )
        .allows());
    }

    #[test]
    fn lan_only_内网来源放行_公网来源不放行() {
        assert!(gate(true, true, Some("192.168.1.5"), &[]).allows());
        assert!(gate(true, true, Some("127.0.0.1"), &[]).allows());
        assert!(!gate(true, true, Some("203.0.113.7"), &[]).allows());
    }

    #[test]
    fn lan_only_拿不到对端地址时保守当公网() {
        assert!(!gate(true, true, None, &[]).allows());
    }

    #[test]
    fn lan_only_有反代且未声明可信时保守当公网() {
        // 反代场景对端恒是反代自己(127.0.0.1)。若按它判,全世界都成了内网。
        assert!(!gate(
            true,
            true,
            Some("127.0.0.1"),
            &[("x-forwarded-for", "8.8.8.8")]
        )
        .allows());
        assert!(!gate(true, true, Some("127.0.0.1"), &[("x-real-ip", "8.8.8.8")]).allows());
        // 连伪造成内网 IP 也不认(没声明可信就压根不读这个头)
        assert!(!gate(
            true,
            true,
            Some("127.0.0.1"),
            &[("x-forwarded-for", "192.168.1.5")]
        )
        .allows());
    }

    #[test]
    fn 鉴权分支() {
        // 免口令模式(机器没设 POLARIS_AUTH_TOKEN):默认 origin_ok 恒 true。
        let open = OriginGate {
            enabled: true,
            origin_ok: true,
        };
        // 只有 POLARIS_LAN_ONLY=1 且来源判成公网时才会是这个形状。
        let lan_only_wan = OriginGate {
            enabled: true,
            origin_ok: false,
        };

        // ① 没设口令 → 直接进,不需要任何凭据(NAS 用户的日常,也是这次改动的目的)
        let ctx = resolve_app_auth_token(&None, None, open).expect("免口令模式应放行");
        assert_eq!(ctx.role, "owner");
        assert_eq!(ctx.username, "local");

        // ② POLARIS_LAN_ONLY=1 + 公网来源 → 拒。这里绝不能掉进下面的「开放模式」兜底,
        //    否则那个开关等于没有。
        assert!(
            resolve_app_auth_token(&None, None, lan_only_wan).is_none(),
            "LAN_ONLY 下公网来源必须拒"
        );

        // ③ 显式设了口令 → 壳把 enabled 置 false → 任何来源都必须校验,
        //    否则等于把管理员刻意设的口令静默作废。
        let explicit = Some("my-secret".to_string());
        assert!(
            resolve_app_auth_token(&explicit, None, OriginGate::closed()).is_none(),
            "显式口令绝不能被来源绕过"
        );
        let ctx = resolve_app_auth_token(&explicit, Some("my-secret"), OriginGate::closed())
            .expect("口令对应放行");
        assert_eq!(ctx.username, "admin");

        // ④ 桌面 hosting 那种 enabled=false + 无口令 → 维持历史的全放行
        let ctx = resolve_app_auth_token(&None, None, OriginGate::closed()).expect("开放模式放行");
        assert_eq!(ctx.role, "owner");
    }

    #[test]
    fn 落盘的老口令不再生效() {
        // 网页向导设过的口令曾经写在 collab.db,忘了就没有找回入口 —— 现在 effective_token
        // 只认环境变量,老库里那条即使还在也彻底失效,升级上来的用户不会被卡在门外。
        assert!(effective_token(&None).is_none());
        assert_eq!(
            effective_token(&Some("env-token".into())).as_deref(),
            Some("env-token")
        );
    }
}

/// 基础面鉴权核心:按访问口令 + 传入 token 解析身份。server.rs 的壳专属端点
/// (/api/status 等)也复用它,故 pub(crate)、且不吃 State(只吃 auth_token 引用)。
pub(crate) fn resolve_app_auth_token(
    auth_token: &Option<String>,
    token: Option<&str>,
    gate: OriginGate,
) -> Option<AuthCtx> {
    // 全局口令命中 = owner(单人 Docker 管理员)。口令只来自 POLARIS_AUTH_TOKEN,
    // 网页里再也设不出第二个来源(见 effective_token 上面那段)。
    let effective = effective_token(auth_token);
    if let Some(expected) = effective.as_ref() {
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
    // 免口令模式:gate.enabled 由调用方算出,成立的条件是 —— 本壳是 Docker server 壳、
    // 没设 POLARIS_AUTH_TOKEN、也没开 POLARIS_REQUIRE_LOGIN。此时任何能连上的来源
    // 直接是 owner,不弹任何框(家用 NAS 的日常)。
    // 唯一的例外是管理员显式打开 POLARIS_LAN_ONLY=1:那时 origin_ok 才真去判内外网,
    // 公网来源在这里 **拒**,绝不能掉到下面的「开放模式」兜底里去 —— 否则那个开关白设。
    if gate.enabled {
        return gate.origin_ok.then(|| AuthCtx {
            user_id: 0,
            username: "local".into(),
            role: "owner".into(),
        });
    }
    // 是否强制登录:设了全局口令,或显式打开 POLARIS_REQUIRE_LOGIN。
    let require_login = effective.is_some() || require_login_env();
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
pub(crate) fn app_ctx_headers(
    auth_token: &Option<String>,
    headers: &HeaderMap,
    gate: OriginGate,
) -> Option<AuthCtx> {
    resolve_app_auth_token(auth_token, bearer_of(headers).as_deref(), gate)
}

/// server.rs 壳专属端点用的来源判定(它拿不到 ApiState,只有 AppState)。
#[cfg(feature = "server")]
pub(crate) fn server_origin_gate(
    open_no_auth: bool,
    peer: Option<SocketAddr>,
    headers: &HeaderMap,
) -> OriginGate {
    origin_gate(open_no_auth, peer, headers)
}

/// 协作面 `bootstrap`(建第一个账号)专用的来源闸。
///
/// 与 [`server_origin_gate`] 只差一点：**不看 `POLARIS_REQUIRE_LOGIN`**。
/// 那个开关的意思是「所有请求都要登录账号」，可库里零账号时根本没有账号可登 ——
/// 若连建号也按它拦掉，`REQUIRE_LOGIN=1` + 没设口令的新机器就永久锁死了
/// (旧版靠自动生成的随机口令兜住，免口令改动把那条路撤了)。
/// `POLARIS_LAN_ONLY` 照旧生效：它管的是「谁够得着」，与要不要登录是两回事。
#[cfg(feature = "server")]
pub(crate) fn server_bootstrap_gate(
    open_no_auth: bool,
    peer: Option<SocketAddr>,
    headers: &HeaderMap,
) -> OriginGate {
    origin_gate_with(open_no_auth, lan_only_env(), peer, headers)
}

/// resolve_app_auth_token 的异步壳:鉴权含同步 SQLite 查询(check_session),直接跑在
/// axum async worker 上会钉住 reactor,挪进阻塞线程池。会话短缓存(collab/auth.rs)命中
/// 时闭包内不落库,这层 spawn_blocking 兜的是未命中/首次。
async fn resolve_app_auth(
    state: &ApiState,
    token: Option<String>,
    gate: OriginGate,
) -> Option<AuthCtx> {
    let auth_token = state.auth_token.clone();
    tokio::task::spawn_blocking(move || resolve_app_auth_token(&auth_token, token.as_deref(), gate))
        .await
        .ok()
        .flatten()
}

/// 端点侧的免口令判定入口:把 ConnectInfo(可能没挂)+ 头 交给 origin_gate。
/// 「本壳允许」之外再加一条硬条件:**这台机器没设访问口令**。设了就全员校验。
fn gate_of(
    state: &ApiState,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: &HeaderMap,
) -> OriginGate {
    let enabled = state.open_no_auth && effective_token(&state.auth_token).is_none();
    origin_gate(enabled, peer.map(|c| c.0), headers)
}

async fn app_ctx(state: &ApiState, headers: &HeaderMap, gate: OriginGate) -> Option<AuthCtx> {
    resolve_app_auth(state, bearer_of(headers), gate).await
}

// ── /api/auth/state 与 /api/auth/setup ────────────────────────────────────
//
// 两个端点都**不鉴权**(没口令可鉴)。/state 是只读探针,前端据它决定要不要弹口令框;
// /setup 曾经是「首次访问向导」的落盘入口,现已停用(见上面「为什么默认不要访问口令」),
// 保留路由只为给老前端一个说得清的错,而不是 404。

#[derive(serde::Serialize)]
struct AuthState {
    /// 这台机器是否设了访问口令(即有没有 POLARIS_AUTH_TOKEN)。
    initialized: bool,
    /// 口令只可能由 POLARIS_AUTH_TOKEN 管着,网页里改不了。恒等于 initialized。
    env_managed: bool,
    /// 网页端能不能设口令。**恒 false**:首次访问向导已停用。
    can_setup: bool,
    /// 当前访客是不是靠「免口令」进来的。
    open_to_me: bool,
}

async fn auth_state(
    State(state): State<ApiState>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
) -> Response {
    let env_managed = state.auth_token.is_some();
    let initialized = effective_token(&state.auth_token).is_some();
    let gate = gate_of(&state, peer, &headers);
    Json(AuthState {
        initialized,
        env_managed,
        can_setup: false,
        open_to_me: gate.allows(),
    })
    .into_response()
}

#[derive(serde::Deserialize)]
struct SetupReq {
    #[allow(dead_code)] // 端点已停用,请求体只为兼容老前端而保留
    token: String,
}

async fn auth_setup(Json(_req): Json<SetupReq>) -> Response {
    // 410 而不是 403:这不是权限不够,是这个功能没了。老前端(缓存的旧 index.html)
    // 撞上它会直接放弃向导,不会把用户卡在一个永远失败的弹框里。
    (
        StatusCode::GONE,
        Json(json!({
            "error": "网页端设置访问口令已停用:本机默认免口令。要上锁请给容器设 POLARIS_AUTH_TOKEN"
        })),
    )
        .into_response()
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

/// 受控远程执行端点(B 方案)。刻意**不**并进 /api/invoke:
/// invoke 的闸是「命令在不在分发表」,exec 的闸是「总开关+模式+白名单」,
/// 两套语义混一个入口迟早互相漏。闸门顺序与理由见 exec.rs 头注释。
async fn exec_ep(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(req): Json<crate::exec::ExecRequest>,
) -> Response {
    let gate = OriginGate::closed();
    // 闸 2:fail-closed。没设访问口令且没强制登录 = 基础面免口令(合成 owner),
    // 那对 exec 就是**无鉴权 shell**。这里直接拒,且不提供开关绕过。
    // 注:这里刻意**不**走「内网免口令」(OriginGate::closed()),exec 必须拿到真凭据 ——
    // 内网免口令是为了让家用 NAS 用户能进对话/知识库,不是为了给局域网开一个无鉴权 shell。
    if effective_token(&state.auth_token).is_none() && !require_login_env() {
        crate::collab::db::audit(
            "anonymous",
            "exec.denied",
            &req.cmd,
            "开放模式(未设访问口令)",
        );
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"本机未设访问口令,远程执行已禁用。请给容器设 POLARIS_AUTH_TOKEN(或 POLARIS_REQUIRE_LOGIN=1)再用"})),
        )
            .into_response();
    }
    let Some(ctx) = app_ctx(&state, &headers, gate).await else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"未授权 (口令错误或会话失效)"})),
        )
            .into_response();
    };
    if role_rank(&ctx.role) < 3 {
        crate::collab::db::audit(&ctx.username, "exec.denied", &req.cmd, "角色不足");
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"远程执行需要 owner 权限"})),
        )
            .into_response();
    }
    match crate::exec::run(&ctx.username, req).await {
        // 命令跑完即 200(哪怕退出码非 0)——那是命令的结果,不是接口的错误;
        // 调用方看 exit_code/ok 字段。只有闸门拒绝/启动失败才给非 2xx。
        Ok(r) => Json(r).into_response(),
        Err(e) => {
            // 闸门拒绝是 403(不是 500):调用方据此提示「让主机侧开开关/解锁 Shell」。
            let status = if e.contains("未开启远程执行")
                || e.contains("不在白名单")
                || e.contains("白名单模式")
                || e.contains("元字符")
                || e.contains("不能带路径")
            {
                StatusCode::FORBIDDEN
            } else if e.contains("缺少参数")
                || e.contains("工作目录不存在")
                || e.contains("启动失败")
            {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, Json(json!({ "error": e }))).into_response()
        }
    }
}

async fn invoke(
    State(state): State<ApiState>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Json(req): Json<InvokeRequest>,
) -> Response {
    let gate = gate_of(&state, peer, &headers);
    let Some(ctx) = app_ctx(&state, &headers, gate).await else {
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


    // ── 「隔空同屏」三条命令在这里就地处理,不进 dispatch ──────────────────────
    // 理由:beam_send 要往 **本次请求所属的广播总线**(state.tx)里投递,而 dispatch_* 只拿得到
    // AppHandle;桌面壳里 AppHandle 是 tauri 句柄,emit 只到本机 webview,手机永远收不到。
    // 在这一层处理还有个好处:双壳(desktop/server)共用同一段代码,不用在两个 dispatch 里各写一份。
    if cmd == "beam_send" {
        // 允许 {msg:{…}} 与直接把字段摊平两种写法(手机端少一层包装)。
        let raw = args
            .get("msg")
            .cloned()
            .unwrap_or(Value::Object(args.as_object().cloned().unwrap_or_default()));
        return match crate::beam::normalize(raw) {
            Ok(msg) => {
                let _ = state.tx.send(crate::host::Event::new(
                    crate::beam::TOPIC.to_string(),
                    msg.clone(),
                    None,
                ));
                crate::collab::db::audit(
                    &ctx.username,
                    "beam.send",
                    msg.get("act").and_then(|v| v.as_str()).unwrap_or(""),
                    msg.get("path").and_then(|v| v.as_str()).unwrap_or(""),
                );
                Json(json!({ "delivered": true, "msg": msg })).into_response()
            }
            Err(e) => invoke_err_resp(e),
        };
    }
    if cmd == "beam_pack" || cmd == "beam_export" {
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return invoke_err_resp("缺少字符串参数 `path`".into()),
        };
        let out_dir = args
            .get("outDir")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let want_export = cmd == "beam_export";
        // 打包要读文件 + base64 整份编码,是纯 CPU/IO 活,必须离开 async worker。
        let joined = tokio::task::spawn_blocking(move || {
            if want_export {
                crate::beam::export(&path, out_dir).map(|p| json!(p))
            } else {
                crate::beam::pack(&path)
                    .and_then(|d| serde_json::to_value(d).map_err(|e| e.to_string()))
            }
        })
        .await;
        return match joined {
            Ok(Ok(v)) => Json(v).into_response(),
            Ok(Err(e)) => invoke_err_resp(e),
            Err(e) => err_resp(format!("打包任务失败: {e}")),
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

        // ── 受控远程执行策略(exec 本体走 /api/exec 专用端点,不走 invoke) ──
        "exec_policy_get" => ok(crate::exec::exec_policy_get()),
        "exec_policy_set" => ok(crate::exec::exec_policy_set(
            a.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false),
            a.get("shellMinutes").and_then(|v| v.as_i64()),
        )?),
        // ── 远程盘共享目录(主机侧开放哪些目录给对端访问,逐目录点选放开写) ──
        "fs_share_get" => ok(crate::fsshare::fs_share_get()),
        "fs_share_set" => ok(crate::fsshare::fs_share_set(vec_str(a, "paths"))?),
        "fs_share_list" => ok(crate::fsshare::fs_share_list()),
        "fs_share_save" => ok(crate::fsshare::fs_share_save(vec_val(a, "items"))?),
        // ── 应用直投:手机要知道主机发布了哪些应用(点开后走 /app/{slug}/) ──
        "app_pub_list" => ok(crate::appproxy::app_pub_list()),
        "app_open" => ok(crate::appproxy::app_open(req_str(a, "slug")?)?),

        // ── 对话辅助(chat_send 在 invoke 里单独特判) ──
        "chat_cancel" => ok(chat::chat_cancel(req_str(a, "reqId")?)?),
        "chat_is_running" => ok(chat::chat_is_running(req_str(a, "reqId")?)),
        // 附件路径必须过闸(见 gate_attach_paths 的大注释):它会把给定路径的文件
        // 整份拷进会话 uploads 目录, 而那目录是 artifact_read 放行的 —— 不拦就等于
        // 给文件中心那道闸开了条绕行道。
        "chat_attach_files" => {
            let paths = vec_str(a, "paths");
            gate_attach_paths(&paths)?;
            ok(chat::chat_attach_files(opt_str(a, "conversationId"), paths))
        }
        "chat_attach_image" => ok(chat::chat_attach_image(
            opt_str(a, "conversationId"),
            req_str(a, "name")?,
            req_str(a, "dataBase64")?,
        )?),
        "chat_build_manifest" => ok(chat::chat_build_manifest(opt_str(a, "conversationId"))),

        // ── 隔空同屏说明页(beam_pack/export/send 在 invoke 上层特判,这里补同步读命令) ──
        "beam_doc_path" => ok(crate::beam::beam_doc_path()?),

        // ── 产物(手机产物 chip 预览走 /api/file;这里给读取/列举备用) ──
        "artifact_read" => ok(chat::artifact_read(req_str(a, "path")?)?),
        "artifact_list" => ok(chat::artifact_list(opt_str(a, "conversationId")).await),
        "artifact_search" => ok(chat::artifact_search(req_str(a, "query")?).await),

        // ── 电脑上的项目(手机说「打开××项目」后,新会话建在该项目下 → claude 的 cwd
        //     就是这个项目绑定的工作目录, 等同电脑上 `cd <repo> && claude`) ──
        // 不开放 archive/open_dir:归档是破坏性的、在电脑上弹资源管理器对手机也没意义。
        "conv_list_projects" => ok(conv::conv_list_projects()),
        "conv_create_project" => ok(conv::conv_create_project(req_str(a, "name")?)?),
        "conv_set_project_work_dir" => ok(conv::conv_set_project_work_dir(
            req_str(a, "projectId")?,
            opt_str(a, "workDir"),
        )?),
        // ── 会话读取(手机历史主要走本地存储,这些为兼容/备用) ──
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
        // provider_list 的 auth_token / settings_config 现已在内核出口统一打码
        // (store.rs 的 mask_secret);这里再窄一道 —— 手机选择器只要这几个字段,
        // 连打了码的密钥和整份 settings_config 都不必下发。两道闸互为保险。
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

        // ── 语音输入(手机端按住说话:录 WAV 传回主机,主机拿火山凭据转写)──
        // 只开放转写这一个口,配置读写仍留在桌面 —— 火山凭据不经手机下发。
        "voice_transcribe_audio" => {
            let audio = req_str(a, "audio")?;
            let fmt = opt_str(a, "format");
            // 转写是 **同步阻塞** 的 ureq 请求(可跑几十秒)。desktop flavor 下
            // dispatch_desktop 是直接 await 在 tokio worker 上跑的(不像 server 壳外面
            // 套了 spawn_blocking),不甩进阻塞池就会钉死一个 worker —— 踩过的坑。
            let r = tokio::task::spawn_blocking(move || voice::voice_transcribe_audio(audio, fmt))
                .await
                .map_err(|e| format!("语音转写任务失败: {e}"))??;
            ok(r)
        }

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
/// 可选的对象数组(共享清单 `[{path, write}]` 这类)。缺失/非数组 → 空 vec。
fn vec_val(a: &Args, k: &str) -> Vec<serde_json::Value> {
    a.get(k)
        .and_then(|v| v.as_array())
        .map(|arr| arr.to_vec())
        .unwrap_or_default()
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

// ───────────────────── 容器自更新（Docker 壳）─────────────────────

/// 容器内更新脚本的固定路径（Dockerfile 把仓库根 update.sh COPY 到这里）。
/// 整段只在 server 壳编译 —— 桌面壳走 Tauri updater，用不到（否则 dead_code 警告）。
#[cfg(not(feature = "desktop"))]
const UPDATE_SCRIPT: &str = "/usr/local/bin/update.sh";

/// 「网页上能不能一键更新」= docker.sock 挂了 + 更新脚本在镜像里。返回
/// `(enabled, socket_present, script_present)`。
///
/// ★ 老版本还额外要求显式 `POLARIS_DOCKER_SOCKET=1` 才放行。群晖 Container Manager
///   图形界面装的容器根本没人去加这个环境变量 → 「立即更新」按钮恒灰、点不动，
///   这是「更新点不了/拉不动」的根因之一。现在这个 env 只保留**显式关闭**语义
///   （`=0` / `=false` 才关），有 sock 就认。
#[cfg(not(feature = "desktop"))]
pub(crate) fn docker_updater_bits() -> (bool, bool, bool) {
    let socket = std::path::Path::new("/var/run/docker.sock").exists();
    let script = std::path::Path::new(UPDATE_SCRIPT).exists();
    let disabled = std::env::var("POLARIS_DOCKER_SOCKET")
        .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
        .unwrap_or(false);
    (socket && script && !disabled, socket, script)
}

/// 跑 `update.sh --check`（只查不动），把它吐的 KEY=VALUE 解析成 JSON。
///
/// 为什么把「查版本」也交给 shell：更新源是一串镜像站（Cloudflare / GitHub /
/// 国内加速），逐源回退的逻辑已经在 update.sh 里，Rust 侧再实现一遍必然两边漂移；
/// 而且 `--check` **不需要 docker.sock** —— 没挂 sock 的容器也能如实告诉用户
/// 「有新版 x.y.z」，再引导去 SSH 兜底，而不是给一个哑掉的灰按钮。
#[cfg(not(feature = "desktop"))]
fn docker_check_update() -> Value {
    if !std::path::Path::new(UPDATE_SCRIPT).exists() {
        return json!({
            "ok": false, "has_update": false,
            "error": "镜像里没有 /usr/local/bin/update.sh（旧版镜像），请先手动装一次新镜像",
        });
    }
    let out = match std::process::Command::new(UPDATE_SCRIPT).arg("--check").output() {
        Ok(o) => o,
        Err(e) => {
            return json!({"ok": false, "has_update": false, "error": format!("启动 update.sh --check 失败: {e}")})
        }
    };
    let mut map = serde_json::Map::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some((k, v)) = line.split_once('=') {
            let (k, v) = (k.trim(), v.trim());
            if k.is_empty() {
                continue;
            }
            let val = match k {
                "ok" | "has_update" => json!(v == "1"),
                _ => json!(v),
            };
            map.insert(k.to_string(), val);
        }
    }
    if map.is_empty() {
        return json!({
            "ok": false, "has_update": false,
            "error": format!("update.sh --check 没有输出：{}", String::from_utf8_lossy(&out.stderr)),
        });
    }
    map.entry("ok").or_insert(json!(false));
    map.entry("has_update").or_insert(json!(false));
    Value::Object(map)
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
        // 应用直投:远端要知道主机发布了哪些本机应用
        "app_pub_list" => ok(crate::appproxy::app_pub_list()),
        "app_open" => ok(crate::appproxy::app_open(req_str(a, "slug")?)?),
        "beam_doc_path" => ok(crate::beam::beam_doc_path()?),
        // 受控远程执行策略(exec 本体走 /api/exec 专用端点,不走 invoke)
        "exec_policy_get" => ok(crate::exec::exec_policy_get()),
        "exec_policy_set" => ok(crate::exec::exec_policy_set(
            a.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false),
            a.get("shellMinutes").and_then(|v| v.as_i64()),
        )?),
        // 远程盘共享目录(主机侧开放哪些目录给对端访问,逐目录点选放开写)
        "fs_share_get" => ok(crate::fsshare::fs_share_get()),
        "fs_share_set" => ok(crate::fsshare::fs_share_set(vec_str(a, "paths"))?),
        "fs_share_list" => ok(crate::fsshare::fs_share_list()),
        "fs_share_save" => ok(crate::fsshare::fs_share_save(vec_val(a, "items"))?),
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
            opt_str(a, "volcAppKey"),
            opt_str(a, "volcAccessKey"),
            opt_str(a, "volcAsrModel"),
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
        // 手机端语音输入:录好的 WAV(base64)传回来,由主机拿火山凭据转写。
        "voice_transcribe_audio" => ok(voice::voice_transcribe_audio(
            req_str(a, "audio")?,
            opt_str(a, "format"),
        )?),
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
        "chat_is_running" => ok(chat::chat_is_running(req_str(a, "reqId")?)),
        "chat_build_manifest" => ok(chat::chat_build_manifest(opt_str(a, "conversationId"))),
        // 同手机数据面:server 壳(Docker/NAS)的浏览器 UI 也只经 /api/upload 拿路径,
        // 没有原生文件对话框 —— 加闸不影响它的任何正常用法。
        "chat_attach_files" => {
            let paths = vec_str(a, "paths");
            gate_attach_paths(&paths)?;
            ok(chat::chat_attach_files(opt_str(a, "conversationId"), paths))
        }
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
        // 深度校验:纯只读探测(起子进程跑 --version / 扫冲突),容器内直通。
        // deep=true 会真发一次请求做端到端冒烟,默认 false —— 前端不传就不花额度。
        "env_verify" => ok(doctor::env_verify(bool_def(a, "deep", false))),
        // 静默托管状态: 容器版从不自己装东西(组件随镜像预装), 如实回一个「没跑过」的空状态
        "env_autopilot_status" => ok(doctor::env_autopilot_status()),
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
        // docker_status:报「能不能自更新」给 UpdatePanel(docker.sock 在位 + update.sh 打进镜像;
        //   判定口径见 docker_updater_bits)。
        "docker_status" => {
            let (enabled, socket, script) = docker_updater_bits();
            ok(json!({
                "updater_enabled": enabled,
                "socket_present": socket,
                "update_script": script,
                "current_tag": std::env::var("POLARIS_TAG").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "latest".to_string()),
            }))
        }
        // docker_check_update:只查不动 —— 逐个镜像源拉 manifest,回「当前/最新/有没有新版」。
        //   不需要 docker.sock,所以没挂 sock 的容器也能看到有新版(再引导 SSH 兜底)。
        "docker_check_update" => ok(docker_check_update()),
        // docker_update:跑 /usr/local/bin/update.sh(默认模式)——它经 docker.sock 用「自己的镜像」
        //   起一个独立替身容器执行 pull + up -d(不能在被替换的容器里直接 up,compose 会随旧容器被杀)。
        //   脚本起完 detached 替身即返回;真正的替换由替身异步完成(约 1~3 分钟,期间连接断,刷新即可)。
        "docker_update" => {
            if !bool_def(a, "confirm", false) {
                return Err("更新需要确认 (confirm: true)".to_string());
            }
            let (enabled, socket, script) = docker_updater_bits();
            if !script {
                return Err("/usr/local/bin/update.sh 不存在(镜像未含更新脚本,旧版镜像请先手动装一次新的)。".to_string());
            }
            if !socket {
                return Err("/var/run/docker.sock 未挂载,容器无法自己换镜像。请在容器设置里加上这个卷映射后重建容器,或在 NAS 上用 SSH 一行命令更新。".to_string());
            }
            if !enabled {
                return Err(
                    "远程更新被显式关闭(POLARIS_DOCKER_SOCKET=0),去掉这个环境变量即可。"
                        .to_string(),
                );
            }
            let tag = std::env::var("POLARIS_TAG")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "latest".to_string());
            // force:版本号相同也重装(用户在网页上点了「强制重装」;update.sh 认 POLARIS_FORCE=1)。
            let mut cmd = std::process::Command::new(UPDATE_SCRIPT);
            if bool_def(a, "force", false) {
                cmd.env("POLARIS_FORCE", "1");
            }
            match cmd.output() {
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
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    // WS 鉴权走 query token（浏览器 WS 不便带自定义 header）。
    let gate = gate_of(&state, peer, &headers);
    let Some(ctx) = resolve_app_auth(&state, params.get("token").cloned(), gate).await else {
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
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let gate = gate_of(&state, peer, &headers);
    let Some(ctx) = app_ctx(&state, &headers, gate).await else {
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

// ───────────────────────── 远端数据面的附件路径闸 ─────────────────────────
//
// `chat_attach_files` 收的是**主机绝对路径**, 并把该路径的文件整份**拷进会话 uploads 目录** ——
// 而那个目录正落在 `artifact_read` 的放行范围内。于是「随便给个路径 → 拷进对话 → 读回来」
// 就把 2026-07-29 给文件中心补的那道闸(fable/files/mod.rs)整个绕过去了, 同族同因。
//
// 闸只加在 HTTP 数据面(手机 / Docker·NAS server 壳)。桌面壳的前端走 Tauri 命令直调,
// 根本不经过 apihub —— 拖拽磁盘上任意文件当附件照常, 一点不受影响。
//
// 放行两类:
//   ① `/api/upload` 的收件箱 —— 手机端**唯一**的合法来路(先 upload 拿到服务端临时路径,
//      再把它喂给本命令, 见 mobile/src/lib/chat.ts 的 sendMessage);
//   ② 文件中心已放行的根(已盘点 roots + 知识库根)—— 留给「从文件中心挑主机文件当附件」
//      这类界面, 与 file_gist/file_thumb 现在能看见的范围完全一致, 不多开一寸。
// 其余一律拒。不存在的路径直接放过: 它拷不出任何东西, 交给命令自己报「文件不存在」,
// 免得把「打错字」和「越权」混成同一句话。
fn gate_attach_paths(paths: &[String]) -> Result<(), String> {
    gate_attach_paths_in(paths, &upload_dir())
}

/// 同上, 收件箱目录可注入 —— 单测不必去碰真实 home 目录。
fn gate_attach_paths_in(paths: &[String], inbox: &Path) -> Result<(), String> {
    // 收件箱可能还没建出来(没人上传过), canonicalize 会失败 → 那就只剩文件中心那一类。
    let inbox = inbox.canonicalize().ok();
    for p in paths {
        let Ok(canon) = Path::new(p).canonicalize() else {
            continue;
        };
        let in_inbox = inbox.as_ref().is_some_and(|d| canon.starts_with(d));
        if in_inbox || crate::fable::files::file_center_path_allowed(&canon.to_string_lossy()) {
            continue;
        }
        return Err("路径越界, 拒绝访问".into());
    }
    Ok(())
}

#[cfg(test)]
mod attach_gate_tests {
    use super::*;

    /// 越界路径必须被拒 —— 这是「拷进对话再读回来」那条绕行链的入口。
    /// (测试环境没有 fable 库 → 文件中心那一类恒为否, 于是只剩收件箱这一条放行路。)
    #[test]
    fn 附件路径闸_只放行收件箱与文件中心() {
        let base = std::env::temp_dir().join(format!("attach-gate-{}", std::process::id()));
        let inbox = base.join("uploads-inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let inside = inbox.join("手机传上来的.txt");
        std::fs::write(&inside, "ok").unwrap();
        let outside = base.join("主机上的机密.txt");
        std::fs::write(&outside, "SECRET").unwrap();

        let s = |p: &Path| p.to_string_lossy().to_string();
        assert!(gate_attach_paths_in(&[s(&inside)], &inbox).is_ok(), "收件箱内必须放行");

        let err = gate_attach_paths_in(&[s(&outside)], &inbox);
        assert!(err.is_err(), "收件箱外必须拒绝, 实际={err:?}");
        assert!(err.unwrap_err().contains("越界"));

        // `..` 穿越同样打不穿(两端都 canonicalize 后再比)。
        let traversal = inbox.join("..").join("主机上的机密.txt");
        assert!(
            gate_attach_paths_in(&[s(&traversal)], &inbox).is_err(),
            "`..` 穿越必须拒绝"
        );

        // 一批里只要有一条越界就整批拒 —— 别让合法路径给非法路径打掩护。
        assert!(
            gate_attach_paths_in(&[s(&inside), s(&outside)], &inbox).is_err(),
            "混入越界路径的整批必须拒"
        );

        // 不存在的路径不归本闸管(拷不出东西), 交由命令自己报「文件不存在」。
        assert!(gate_attach_paths_in(&[s(&base.join("没有这个文件"))], &inbox).is_ok());

        let _ = std::fs::remove_dir_all(&base);
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
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Query(q): Query<FileQuery>,
) -> Response {
    let gate = gate_of(&state, peer, &headers);
    let ctx = match app_ctx(&state, &headers, gate).await {
        Some(c) => Some(c),
        None => resolve_app_auth(&state, q.token.clone(), gate).await,
    };
    let Some(ctx) = ctx else {
        return (StatusCode::UNAUTHORIZED, "未授权").into_response();
    };
    // 读文件放到 collaborator/lead(rank>=2):手机端团队成员要能点开对话产物看。
    // 仍然 fail-closed 挡住 visitor/未知角色;真正的边界是下面的 allowed_roots 白名单
    // ——只有知识库根、~/Polaris/{data/artifacts,projects,uploads-inbox} 和项目绑定的
    // 工作目录能读,整机文件系统读不到。逐用户 ACL 做好前不再往下放。
    if role_rank(&ctx.role) < 2 {
        return (StatusCode::FORBIDDEN, "文件访问需要协作者及以上权限").into_response();
    }
    let allowed = allowed_roots();
    let canon = match resolve_readable(&q.path, &allowed) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
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

/// 把请求里的 `path` 解析成一个**确实在白名单内**的真实文件。
///
/// 绝对路径照旧直接 canonicalize;新增的是**相对路径 / `~/` 开头 / 裸文件名**也认 ——
/// 手机端「点开对话正文里提到的文件」拿到的往往是助手随手写的相对路径
/// (`docs/报告.md`、`~/Polaris/projects/x/index.html`),按进程 CWD 去解析要么落空、
/// 要么落到意想不到的目录。这里改成逐个白名单根去拼,第一个命中的算数。
///
/// **安全性不变**:无论走哪条,最后一律拿 canonicalize 之后的真实路径复核白名单,
/// `..` 穿越、符号链接逃逸照旧被 `path_contains` 挡住;白名单本身一个字没放宽。
///
/// 实现已下沉 [`crate::beam`](crate::beam::resolve_readable_path) —— 「隔空同屏」的打包器
/// 要用同一把闸,两处各写一份迟早漂移。这里只负责把拒绝原因翻成 HTTP 状态码。
fn resolve_readable(raw: &str, allowed: &[PathBuf]) -> Result<PathBuf, Response> {
    crate::beam::resolve_readable_path(raw, allowed).map_err(|d| match d {
        crate::beam::PathDenied::NotFound => (StatusCode::NOT_FOUND, "文件不存在").into_response(),
        crate::beam::PathDenied::Outside => {
            (StatusCode::FORBIDDEN, "路径不在允许范围").into_response()
        }
    })
}

fn allowed_roots() -> Vec<PathBuf> {
    crate::beam::allowed_roots()
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

// ── 「免口令」的 HTTP 层 e2e ────────────────────────────────────────────────
//
// 单测只能证明判定函数对,证明不了整条链路上真的没人再拦。故这里起真服务发真请求,
// 两种挂法(有/无 ConnectInfo)都跑一遍 —— 反代后面、非 TCP 传输等拿不到对端地址的
// 场景走的正是后者,以前它会被当公网拒掉,那是「打不开」投诉的一大来源。
#[cfg(all(test, feature = "server", not(feature = "desktop")))]
mod open_access_e2e_tests {
    use super::*;

    /// 起真服务。connect_info=true 时按 server.rs 的方式挂 ConnectInfo。
    async fn serve(
        auth_token: Option<String>,
        open_no_auth: bool,
        connect_info: bool,
    ) -> (u16, tokio::sync::oneshot::Sender<()>) {
        let (tx, _rx) = broadcast::channel(64);
        let state = ApiState {
            app: AppHandle::new(tx.clone()),
            tx,
            auth_token: Arc::new(auth_token),
            open_no_auth,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stx, srx) = tokio::sync::oneshot::channel::<()>();
        let router = api_router(state);
        tokio::spawn(async move {
            let shutdown = async {
                let _ = srx.await;
            };
            if connect_info {
                let _ = axum::serve(
                    listener,
                    router.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .with_graceful_shutdown(shutdown)
                .await;
            } else {
                let _ = axum::serve(listener, router)
                    .with_graceful_shutdown(shutdown)
                    .await;
            }
        });
        (port, stx)
    }

    /// 不带任何凭据打一发 /api/invoke,返回状态码。
    /// 用一条不存在的命令:鉴权过了会走到分发并得 404「未知命令」,鉴权没过则是 401。
    /// 401 vs 404 正好把「鉴权层」和「分发层」分开,不必依赖任何真命令与数据库。
    async fn probe(port: u16) -> u16 {
        tokio::task::spawn_blocking(move || {
            match ureq::post(&format!("http://127.0.0.1:{port}/api/invoke"))
                .send_json(json!({"cmd":"__auth_probe","args":{}}))
            {
                Ok(r) => r.status(),
                Err(ureq::Error::Status(c, _)) => c,
                Err(e) => panic!("请求失败: {e}"),
            }
        })
        .await
        .unwrap()
    }

    /// 没设 POLARIS_AUTH_TOKEN → 直接进,不能 401。
    #[tokio::test]
    async fn 没设口令时直接进得去() {
        let (port, stop) = serve(None, true, true).await;
        let code = probe(port).await;
        let _ = stop.send(());
        assert_ne!(code, 401, "免口令模式不该被任何口令框挡住");
        assert_eq!(code, 404, "应已过鉴权、走到分发拿到「未知命令」");
    }

    /// 管理员显式设了口令 → 一律校验,免口令那条路不生效。
    #[tokio::test]
    async fn 显式口令不被绕过() {
        let (port, stop) = serve(Some("my-secret".into()), true, true).await;
        let code = probe(port).await;
        let _ = stop.send(());
        assert_eq!(code, 401, "显式设的口令不能被静默作废");
    }

    /// 拿不到对端地址(反代后面 / 没挂 ConnectInfo)也照进 —— 这条以前是 401,
    /// 群晖反代、隧道转发过来的用户就卡在这儿,现在不再拦。
    #[tokio::test]
    async fn 拿不到对端地址也进得去() {
        let (port, stop) = serve(None, true, false).await;
        let code = probe(port).await;
        let _ = stop.send(());
        assert_eq!(code, 404, "免口令模式下不看来源,应已过鉴权走到分发");
    }

    /// /api/auth/state:免口令机器要明确告诉前端「别弹框」(can_setup=false + open_to_me=true),
    /// 否则老前端会照旧弹「设置访问口令」向导,又造出一个没人记得住的口令。
    #[tokio::test]
    async fn 未设口令时前端不该弹任何框() {
        let (port, stop) = serve(None, true, true).await;
        let body = tokio::task::spawn_blocking(move || {
            ureq::get(&format!("http://127.0.0.1:{port}/api/auth/state"))
                .call()
                .unwrap()
                .into_string()
                .unwrap()
        })
        .await
        .unwrap();
        let _ = stop.send(());
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["initialized"], false);
        assert_eq!(v["can_setup"], false, "网页端设口令的向导已停用");
        assert_eq!(v["open_to_me"], true);
    }

    /// 网页端设口令的入口已停用:不论机器有没有口令,一律 410,不再往库里写任何东西。
    #[tokio::test]
    async fn 网页端不再能设口令() {
        for token in [None, Some("env-token".to_string())] {
            let (port, stop) = serve(token, true, true).await;
            let code = tokio::task::spawn_blocking(move || {
                match ureq::post(&format!("http://127.0.0.1:{port}/api/auth/setup"))
                    .send_json(json!({"token":"whatever"}))
                {
                    Ok(r) => r.status(),
                    Err(ureq::Error::Status(c, _)) => c,
                    Err(e) => panic!("请求失败: {e}"),
                }
            })
            .await
            .unwrap();
            let _ = stop.send(());
            assert_eq!(code, 410, "首次访问向导已停用,应直接告知功能没了");
        }
    }
}

// ── /api/exec 的鉴权闸(HTTP 层真起服务测) ──────────────────────────────────
//
// exec 是本仓唯一「外部可触发本机任意进程」的入口,它的边界不能只靠编译期保证。
// 这组测试起真 axum 服务、发真 HTTP 请求,逐条验证闸门。
// 只在 server flavor 跑:桌面 flavor 的 AppHandle 是 tauri 真句柄,测试里造不出。
#[cfg(all(test, feature = "server", not(feature = "desktop")))]
mod exec_gate_tests {
    use super::*;

    /// 起一个只挂 api_router 的真服务,返回端口与停机开关。
    async fn serve(auth_token: Option<String>) -> (u16, tokio::sync::oneshot::Sender<()>) {
        let (tx, _rx) = broadcast::channel(64);
        let state = ApiState {
            app: AppHandle::new(tx.clone()),
            tx,
            auth_token: Arc::new(auth_token),
            // 这组测试盯的是 exec 的 fail-closed:不论壳开不开免口令,没真凭据都得拒。
            // 置 false 走的是「桌面 hosting / 开放模式」那条兜底,更严格地覆盖到分支。
            open_no_auth: false,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stx, srx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, api_router(state))
                .with_graceful_shutdown(async {
                    let _ = srx.await;
                })
                .await;
        });
        (port, stx)
    }

    /// 发一条 exec 请求,返回 (状态码, 响应体)。
    async fn post_exec(port: u16, token: Option<&str>, body: Value) -> (u16, String) {
        let token = token.map(|s| s.to_string());
        tokio::task::spawn_blocking(move || {
            let mut r = ureq::post(&format!("http://127.0.0.1:{port}/api/exec"));
            if let Some(t) = token {
                r = r.set("Authorization", &format!("Bearer {t}"));
            }
            match r.send_json(body) {
                Ok(resp) => (resp.status(), resp.into_string().unwrap_or_default()),
                Err(ureq::Error::Status(code, resp)) => {
                    (code, resp.into_string().unwrap_or_default())
                }
                Err(e) => panic!("请求失败: {e}"),
            }
        })
        .await
        .unwrap()
    }

    fn tmp_db(name: &str) -> std::path::PathBuf {
        let tmp = std::env::temp_dir().join(format!("polaris-execgate-{name}.db"));
        let _ = std::fs::remove_file(&tmp);
        std::env::set_var("POLARIS_COLLAB_DB", &tmp);
        tmp
    }

    /// 最关键的一条:没设访问口令时,基础面会「合成 owner 全放行」(开放模式)。
    /// 那对读接口尚可,对 exec 等于把无鉴权 shell 挂在隧道上。必须 fail-closed,
    /// **且不受远程执行总开关影响** —— 哪怕开关开着也得拒。
    #[tokio::test(flavor = "multi_thread")]
    async fn open_mode_is_fail_closed_even_when_enabled() {
        let _g = crate::collab::db::TEST_LOCK.lock().unwrap();
        let tmp = tmp_db("open");
        std::env::remove_var("POLARIS_REQUIRE_LOGIN");
        crate::exec::set_policy(true, None).unwrap(); // 开关开着,也必须拒

        let (port, stx) = serve(None).await; // None = 未设访问口令 = 开放模式
        let (code, body) = post_exec(port, None, json!({"cmd":"cargo","args":["--version"]})).await;
        assert_eq!(code, 403, "开放模式必须拒绝 exec,实际 body={body}");
        assert!(body.contains("未设访问口令"), "body={body}");

        // 同一台服务上,/api/invoke 在开放模式下仍照旧放行 —— 证明这条 fail-closed
        // 是 exec 专属加严,没有殃及既有的基础面语义。
        let ok = tokio::task::spawn_blocking(move || {
            ureq::post(&format!("http://127.0.0.1:{port}/api/invoke"))
                .send_json(json!({"cmd":"kb_root","args":{}}))
                .map(|r| r.status())
        })
        .await
        .unwrap();
        assert_eq!(ok.ok(), Some(200), "开放模式下 /api/invoke 应维持原行为");

        let _ = stx.send(());
        std::env::remove_var("POLARIS_COLLAB_DB");
        let _ = std::fs::remove_file(&tmp);
    }

    /// 设了访问口令后:不带/带错 token = 401;带对 token 但总开关没开 = 403。
    #[tokio::test(flavor = "multi_thread")]
    async fn token_required_then_switch_required() {
        let _g = crate::collab::db::TEST_LOCK.lock().unwrap();
        let tmp = tmp_db("token");
        std::env::remove_var("POLARIS_REQUIRE_LOGIN");
        crate::exec::set_policy(false, None).unwrap(); // 总开关关

        let (port, stx) = serve(Some("s3cret".into())).await;
        let body = json!({"cmd":"cargo","args":["--version"]});

        let (code, _) = post_exec(port, None, body.clone()).await;
        assert_eq!(code, 401, "无 token 必须 401");
        let (code, _) = post_exec(port, Some("wrong"), body.clone()).await;
        assert_eq!(code, 401, "错 token 必须 401");

        let (code, b) = post_exec(port, Some("s3cret"), body.clone()).await;
        assert_eq!(code, 403, "口令对但总开关没开,必须 403");
        assert!(b.contains("未开启远程执行"), "body={b}");

        let _ = stx.send(());
        std::env::remove_var("POLARIS_COLLAB_DB");
        let _ = std::fs::remove_file(&tmp);
    }

    /// 四道闸全过:真起进程、真拿到 stdout 与退出码;白名单外的命令仍是 403。
    #[tokio::test(flavor = "multi_thread")]
    async fn full_pass_runs_and_still_enforces_whitelist() {
        let _g = crate::collab::db::TEST_LOCK.lock().unwrap();
        let tmp = tmp_db("pass");
        std::env::remove_var("POLARIS_REQUIRE_LOGIN");
        crate::exec::set_policy(true, None).unwrap();

        let (port, stx) = serve(Some("s3cret".into())).await;

        let (code, b) = post_exec(
            port,
            Some("s3cret"),
            json!({"cmd":"cargo","args":["--version"]}),
        )
        .await;
        assert_eq!(code, 200, "闸门全过应 200,body={b}");
        let v: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(v["ok"], true, "body={b}");
        assert_eq!(v["exit_code"], 0);
        assert_eq!(v["mode"], "whitelist");
        assert!(
            v["stdout"].as_str().unwrap_or("").contains("cargo"),
            "应拿到真实 stdout,body={b}"
        );

        // 白名单闸仍在:鉴权过了不等于什么都能跑。
        let (code, b) =
            post_exec(port, Some("s3cret"), json!({"cmd":"rm","args":["-rf","/"]})).await;
        assert_eq!(code, 403, "白名单外命令必须 403,body={b}");

        let _ = stx.send(());
        std::env::remove_var("POLARIS_COLLAB_DB");
        let _ = std::fs::remove_file(&tmp);
    }

    // ── /api/file 的白名单闸(手机端「远程预览」的读口)────────────────────────
    //
    // 起真服务发真 GET。两条都是踩过的坑:
    //  ① 项目绑了工作目录后,产物落在用户自选目录里,白名单没跟上 → 手机端预览一律
    //     403「路径不在允许范围」(功能加了、读口没跟上);
    //  ② 放开工作目录不等于开放整机:目录外的文件必须照旧 403。
    #[tokio::test(flavor = "multi_thread")]
    async fn file_whitelist_covers_project_work_dir_only() {
        let _g = crate::collab::db::TEST_LOCK.lock().unwrap();
        let tmp = tmp_db("file");
        std::env::remove_var("POLARIS_REQUIRE_LOGIN");

        // 工作目录(白名单内)与它外面的一个目录(白名单外)
        let root = std::env::temp_dir().join("polaris-filegate");
        let inside = root.join("work");
        let outside = root.join("elsewhere");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let art = inside.join("dashboard.html");
        std::fs::write(&art, "<h1>看板</h1>").unwrap();
        let secret = outside.join("secret.txt");
        std::fs::write(&secret, "不该被读到").unwrap();

        // 建项目并绑工作目录(测试里 conv 没 init,STATE_PATH 为空 → persist 是空操作,
        // 不会污染真实 state.json)
        let p = crate::conv::conv_create_project("预览白名单测试".into()).unwrap();
        crate::conv::conv_set_project_work_dir(
            p.id.clone(),
            Some(inside.to_string_lossy().to_string()),
        )
        .unwrap();

        let (port, stx) = serve(Some("s3cret".into())).await;
        let get = |path: std::path::PathBuf| async move {
            tokio::task::spawn_blocking(move || {
                // pct_encode 只留 unreserved,Windows 路径的 `:` `\` 都会转义,当查询值够用
                let url = format!(
                    "http://127.0.0.1:{port}/api/file?path={}",
                    pct_encode(&path.to_string_lossy())
                );
                match ureq::get(&url).set("Authorization", "Bearer s3cret").call() {
                    Ok(r) => (r.status(), r.into_string().unwrap_or_default()),
                    Err(ureq::Error::Status(c, r)) => (c, r.into_string().unwrap_or_default()),
                    Err(e) => panic!("请求失败: {e}"),
                }
            })
            .await
            .unwrap()
        };

        let (code, body) = get(art.clone()).await;
        assert_eq!(code, 200, "工作目录内的产物必须能读,body={body}");
        assert!(body.contains("看板"), "应拿到真实文件内容,body={body}");

        let (code, body) = get(secret.clone()).await;
        assert_eq!(code, 403, "工作目录外的文件必须照旧拒绝,body={body}");
        assert!(body.contains("不在允许范围"), "body={body}");

        // ── 相对路径:手机端点开「对话正文里提到的文件」拿到的往往是助手随手写的
        //    相对路径(`报告/周报.md`),按进程 CWD 解析必落空 → 得逐个白名单根去拼。
        let get_s = |raw: String| async move {
            tokio::task::spawn_blocking(move || {
                let url = format!("http://127.0.0.1:{port}/api/file?path={}", pct_encode(&raw));
                match ureq::get(&url).set("Authorization", "Bearer s3cret").call() {
                    Ok(r) => (r.status(), r.into_string().unwrap_or_default()),
                    Err(ureq::Error::Status(c, r)) => (c, r.into_string().unwrap_or_default()),
                    Err(e) => panic!("请求失败: {e}"),
                }
            })
            .await
            .unwrap()
        };
        std::fs::create_dir_all(inside.join("报告")).unwrap();
        std::fs::write(inside.join("报告/周报.md"), "# 本周进展").unwrap();

        let (code, body) = get_s("报告/周报.md".to_string()).await;
        assert_eq!(code, 200, "相对路径应能在白名单根下解析到,body={body}");
        assert!(body.contains("本周进展"), "body={body}");

        let (code, body) = get_s("./dashboard.html".to_string()).await;
        assert_eq!(code, 200, "`./` 前缀也要认,body={body}");
        assert!(body.contains("看板"), "body={body}");

        // 相对路径不等于可以往上爬:`..` 穿越到白名单外仍旧拒绝(canonicalize 后复核白名单)。
        let (code, body) = get_s("../elsewhere/secret.txt".to_string()).await;
        assert_ne!(code, 200, "相对路径 `..` 穿越必须挡住,body={body}");

        let (code, body) = get_s("不存在的文件.md".to_string()).await;
        assert_eq!(
            code, 404,
            "解析不到的相对路径应报不存在而非权限,body={body}"
        );

        let _ = stx.send(());
        std::env::remove_var("POLARIS_COLLAB_DB");
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_dir_all(&root);
    }
}

// ── 手机端「说一句就进电脑上的项目」的数据面链路 ────────────────────────────
//
// 盯的是一条真踩过的坑:手机原先拿本地生成的 `m-<时间戳>` 当 conversationId,主机不认识
// 就把它挂到「第一个未归档项目」下(conv::ensure_writable_or_create),而 claude 的 cwd 是
// 按 conversation→project→work_dir 解析的 —— 于是用户在手机上选哪个项目根本不起作用,
// 活干在了另一个目录里。修法是:选了项目就走 conv_create_conversation 拿主机发的真会话 id。
// 这里起真服务、发真 /api/invoke,把手机那套调用顺序原样走一遍,并一路验到 cwd 的落点。
#[cfg(all(test, feature = "server", not(feature = "desktop")))]
mod mobile_project_e2e_tests {
    use super::*;

    async fn serve(auth_token: Option<String>) -> (u16, tokio::sync::oneshot::Sender<()>) {
        let (tx, _rx) = broadcast::channel(64);
        let state = ApiState {
            app: AppHandle::new(tx.clone()),
            tx,
            auth_token: Arc::new(auth_token),
            open_no_auth: false,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stx, srx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, api_router(state))
                .with_graceful_shutdown(async {
                    let _ = srx.await;
                })
                .await;
        });
        (port, stx)
    }

    /// 学手机端 net.ts 的 invoke():POST /api/invoke {cmd,args} + Bearer。返回 (状态码, 响应体)。
    async fn invoke(port: u16, cmd: &str, args: Value) -> (u16, Value) {
        let cmd = cmd.to_string();
        tokio::task::spawn_blocking(move || {
            let sent = ureq::post(&format!("http://127.0.0.1:{port}/api/invoke"))
                .set("Authorization", "Bearer s3cret")
                .send_json(json!({"cmd": cmd, "args": args}));
            let (code, text) = match sent {
                Ok(r) => (r.status(), r.into_string().unwrap_or_default()),
                Err(ureq::Error::Status(c, r)) => (c, r.into_string().unwrap_or_default()),
                Err(e) => panic!("请求失败: {e}"),
            };
            (
                code,
                serde_json::from_str::<Value>(&text).unwrap_or(Value::Null),
            )
        })
        .await
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn phone_picks_project_then_cwd_follows_it() {
        let _g = crate::collab::db::TEST_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join("polaris-mobileproj.db");
        let _ = std::fs::remove_file(&tmp);
        std::env::set_var("POLARIS_COLLAB_DB", &tmp);
        std::env::remove_var("POLARIS_REQUIRE_LOGIN");

        // 电脑上两个真实存在的文件夹(用户的两个大项目仓库)
        let root = std::env::temp_dir().join("polaris-mobileproj");
        let alpha = root.join("alpha-repo");
        let beta = root.join("beta-repo");
        std::fs::create_dir_all(&alpha).unwrap();
        std::fs::create_dir_all(&beta).unwrap();

        let (port, stx) = serve(Some("s3cret".into())).await;

        // ① 手机在「电脑项目」页建项目并绑文件夹(WorkScreen 的新建 + 绑定工作目录)
        let (c, pa) = invoke(port, "conv_create_project", json!({"name":"阿尔法"})).await;
        assert_eq!(c, 200, "建项目应成功,body={pa}");
        let pa_id = pa["id"].as_str().expect("项目应有 id").to_string();
        let (c, pb) = invoke(port, "conv_create_project", json!({"name":"贝塔"})).await;
        assert_eq!(c, 200, "建项目应成功,body={pb}");
        let pb_id = pb["id"].as_str().unwrap().to_string();

        for (pid, dir) in [(&pa_id, &alpha), (&pb_id, &beta)] {
            let (c, b) = invoke(
                port,
                "conv_set_project_work_dir",
                json!({"projectId": pid, "workDir": dir.to_string_lossy()}),
            )
            .await;
            assert_eq!(c, 200, "绑定工作目录应成功,body={b}");
        }

        // 手输路径打错是手机上最常见的失手 —— 必须当场报错,不能默默存下一个无效 cwd
        let (c, b) = invoke(
            port,
            "conv_set_project_work_dir",
            json!({"projectId": pa_id, "workDir": root.join("并不存在").to_string_lossy()}),
        )
        .await;
        assert_ne!(c, 200, "不存在的目录必须拒绝,body={b}");
        assert_eq!(
            crate::conv::project_work_dir(&pa_id).as_deref(),
            Some(alpha.to_string_lossy().as_ref()),
            "被拒的绑定不能把原来那个有效目录改掉"
        );

        // ② 手机拉项目清单(说「打开××项目」时就是在这份清单里匹配名字/文件夹名)
        let (c, list) = invoke(port, "conv_list_projects", json!({})).await;
        assert_eq!(c, 200);
        let arr = list.as_array().expect("应返回数组");
        let found = arr
            .iter()
            .find(|p| p["id"].as_str() == Some(pa_id.as_str()))
            .expect("清单里应有刚建的项目");
        assert_eq!(
            found["work_dir"].as_str(),
            Some(alpha.to_string_lossy().as_ref()),
            "清单必须带 work_dir —— 手机端顶栏显示、按文件夹名匹配都靠它"
        );

        // ③ 选中阿尔法后开新对话:走 conv_create_conversation(projectId),拿主机发的真 id
        let (c, conv) = invoke(port, "conv_create_conversation", json!({"projectId": pa_id})).await;
        assert_eq!(c, 200, "在项目下建会话应成功,body={conv}");
        let cid = conv["id"].as_str().expect("会话应有 id").to_string();

        // ④ 这一跳正是 chat 管线解析 cwd 的路径:conversation → project → work_dir
        assert_eq!(
            crate::conv::project_id_of_conversation(&cid).as_deref(),
            Some(pa_id.as_str()),
            "会话必须归属于手机上选中的那个项目"
        );
        assert_eq!(
            crate::conv::project_work_dir(&pa_id).as_deref(),
            Some(alpha.to_string_lossy().as_ref()),
            "claude 的 cwd 就是这个值 —— 手机上说的活应该落在 alpha-repo 里"
        );

        // ⑤ 换到贝塔项目再开一条:cwd 必须跟着换,两条会话互不串目录
        let (c, conv2) = invoke(port, "conv_create_conversation", json!({"projectId": pb_id})).await;
        assert_eq!(c, 200, "body={conv2}");
        let cid2 = conv2["id"].as_str().unwrap().to_string();
        assert_eq!(
            crate::conv::project_id_of_conversation(&cid2).as_deref(),
            Some(pb_id.as_str())
        );
        assert_ne!(
            crate::conv::project_work_dir(&pa_id),
            crate::conv::project_work_dir(&pb_id),
            "两个项目应各在各的文件夹里"
        );

        // ⑥ 对照组 = 修复前的老行为:裸 `m-<ts>` 会话被挂到「第一个未归档项目」,
        //    跟用户选的那个项目毫无关系。这条固化下来,免得哪天回退了没人发现。
        let bare = format!("m-{}", 1_700_000_000_000u64);
        crate::conv::ensure_writable_or_create(&bare).unwrap();
        let default_pid = crate::conv::conv_list_projects()
            .first()
            .map(|p| p.id.clone())
            .unwrap();
        assert_eq!(
            crate::conv::project_id_of_conversation(&bare).as_deref(),
            Some(default_pid.as_str()),
            "裸会话落到第一个未归档项目 —— 这正是手机端非得显式建会话不可的原因"
        );

        let _ = stx.send(());
        std::env::remove_var("POLARIS_COLLAB_DB");
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_dir_all(&root);
    }
}
