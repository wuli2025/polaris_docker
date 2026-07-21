//! 本地路由(翻译代理)—— 让 `claude` 用上非 Anthropic 协议的上游(cc-switch 式本地转发)。
//!
//! 思路同 cc-switch 的 `proxy/`, 但**不背 axum/hyper/reqwest/tokio 全家桶**: 入站用
//! `std::net` 手写少量路由的 HTTP/1.1 + SSE 本地服务, 出站继续用现成的 `ureq`(流式读响应体)。
//!
//!   claude ──Anthropic /p/{id}/v1/messages(SSE)──▶ 本路由 ──┬─Responses(SSE)─▶ chatgpt.com/backend-api/codex
//!          ◀──────────── Anthropic SSE ───────────         └─chat/completions(SSE)─▶ 任意 OpenAI 兼容上游
//!
//! 路径里的 `{id}` 是供应商坞的供应商 id, **每请求实时**经 `provider::route_target(id)` 解析
//! 上游(base_url / key / 默认模型)——改 key、换模型立即生效, 并发对话可各指不同家不串台;
//! 裸 `/v1/messages`(无前缀, 旧版 settings.json 残留)兼容回落 ChatGPT。
//!
//! - ChatGPT 上游: 鉴权读 `~/.codex/auth.json`(坞里 Codex 授权写好的 OAuth token),
//!   将过期或上游 401 时用 refresh_token 静默刷新并回写。翻译走 Responses 协议:
//!   system→instructions、messages→input、tool_use/tool_result ↔ function_call/…。
//! - OpenAI 兼容上游(GPT API / 各家网关): 鉴权 Bearer 该供应商存的 key, 翻译走
//!   chat/completions: system→messages[0]、tool_use→tool_calls、tool_result→role:"tool",
//!   流式把 delta.content / delta.tool_calls 增量翻成 Anthropic 的 content_block_delta。
//! - 思维链(reasoning / reasoning_content)一律不透传。
//! - 模型: claude 发来的是坞里钉的模型名, 原样透传; claude-* / 空回落该家默认。
//!
//! 注: 上游请求契约(必需头/字段)可能随官方调整, 出错文案会经 `last_error()` 暴露到坞里。

use crate::provider::{
    codex_auth_path, codex_b64url_decode, codex_rfc3339_now, current_provider_id,
    note_provider_failure, note_provider_success, route_target, RouteTarget, CODEX_CLIENT_ID,
    CODEX_OAUTH_TOKEN_URL,
};
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BACKEND_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const DEFAULT_MODEL: &str = "gpt5.6-sol";
const USER_AGENT: &str = "polaris-codex-proxy";
const BASE_PORT: u16 = 8765;
const PORT_TRIES: u16 = 25;
/// 入站 socket 读/写超时: 半发请求或谎报 Content-Length 的慢连接不会让连接线程永久阻塞泄漏。
const IO_TIMEOUT: Duration = Duration::from_secs(60);
/// 上游连接建立超时。
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// 上游 SSE 流「两次读之间」的最大静默 (超过即判上游半死) —— **不是整条流的总时长上限**,
/// 故不会误杀真的在持续吐 token 的长回复; 只在上游接受连接后彻底不发数据时兜底解除阻塞。
const UPSTREAM_READ_TIMEOUT: Duration = Duration::from_secs(120);
/// 请求体大小硬上限: 挡 `Content-Length` 谎报超大值导致 `vec![0u8; N]` 瞬时巨量分配的内存放大 DoS。
const MAX_BODY: usize = 64 * 1024 * 1024;

static PROXY_PORT: Lazy<RwLock<Option<u16>>> = Lazy::new(|| RwLock::new(None));
static LAST_ERROR: Lazy<RwLock<String>> = Lazy::new(|| RwLock::new(String::new()));
static COUNTER: AtomicU64 = AtomicU64::new(1);

fn set_error(s: String) {
    *LAST_ERROR.write() = s;
}

/// 当前代理端口(未启动则 None)
pub fn port() -> Option<u16> {
    *PROXY_PORT.read()
}

/// 最近一次上游/鉴权错误(供坞里展示)
pub fn last_error() -> String {
    LAST_ERROR.read().clone()
}

/// 确保代理已启动, 返回监听端口。已在跑则直接返回端口(幂等)。
pub fn ensure_running() -> Result<u16, String> {
    if let Some(p) = *PROXY_PORT.read() {
        return Ok(p);
    }
    let mut guard = PROXY_PORT.write();
    if let Some(p) = *guard {
        return Ok(p); // 另一线程刚起好
    }
    let listener = bind_port()?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("读取代理端口失败: {e}"))?
        .port();
    std::thread::spawn(move || accept_loop(listener));
    *guard = Some(port);
    Ok(port)
}

/// 从 BASE_PORT 起逐个尝试绑定, 避开被占用端口
fn bind_port() -> Result<TcpListener, String> {
    for off in 0..PORT_TRIES {
        let p = BASE_PORT + off;
        if let Ok(l) = TcpListener::bind(("127.0.0.1", p)) {
            return Ok(l);
        }
    }
    Err(format!(
        "无法在 {}–{} 间绑定本地端口(都被占用)",
        BASE_PORT,
        BASE_PORT + PORT_TRIES - 1
    ))
}

fn accept_loop(listener: TcpListener) {
    for stream in listener.incoming().flatten() {
        std::thread::spawn(move || {
            let _ = handle_conn(stream);
        });
    }
}

// ───────────────────────── HTTP/1.1 入站 ─────────────────────────

fn handle_conn(mut stream: TcpStream) -> std::io::Result<()> {
    // 入站读/写都设超时, 防慢连接/半发请求把连接线程永久阻塞 → 线程只增不减泄漏。
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let clone = stream.try_clone()?;
    let _ = clone.set_read_timeout(Some(IO_TIMEOUT));
    let mut reader = BufReader::new(clone);

    // 请求行: METHOD PATH HTTP/1.1
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    // `/p/{供应商id}/...` → 按 id 路由上游; 无前缀 → 旧版兼容, 回落 ChatGPT。
    let (route_id, path) = split_route(&path);

    // 头: 取 Content-Length + 少数需要透传给上游的 Anthropic 头, 读到空行为止
    let mut content_length = 0usize;
    let mut fwd_headers: Vec<(String, String)> = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let t = line.trim_end();
        if t.is_empty() {
            break;
        }
        if let Some((k, v)) = t.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            if key == "content-length" {
                content_length = v.trim().parse().unwrap_or(0);
            } else if key == "anthropic-version" || key == "anthropic-beta" {
                // 透传目标要带上这些协议头(鉴权头不透传 —— 由路由按供应商实时注入)
                fwd_headers.push((key, v.trim().to_string()));
            }
        }
    }

    if content_length > MAX_BODY {
        anthropic_error(&mut stream, 413, "请求体过大");
        return Ok(());
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }

    if method == "POST" && path.starts_with("/v1/messages") {
        // 每请求实时解析路由目标: 改 key / 换模型 / 切供应商立即生效(热切换的根)。
        let target = match route_target(&route_id) {
            Ok(t) => t,
            Err(e) => {
                set_error(e.clone());
                anthropic_error(&mut stream, 400, &e);
                return Ok(());
            }
        };
        let is_count = path.starts_with("/v1/messages/count_tokens");
        // 故障转移归因 id: `/p/{id}` 前缀直接可信; 旧版无前缀路径按「当前供应商」归因。
        // 本地路由是唯一每请求过手的地方 —— 直连模式的失败发生在 claude CLI 内部,
        // kernel 看不见, 故滑窗只接在这条路径上(取舍见 store::note_provider_failure)。
        let fo_id = if route_id.is_empty() {
            current_provider_id()
        } else {
            route_id.clone()
        };
        match target {
            // Anthropic 兼容上游 → 纯透传(含 count_tokens: 上游有真接口, 原样转)
            RouteTarget::AnthropicCompat {
                base_url,
                api_key,
                token_field,
            } => passthrough_forward(
                &mut stream,
                &path,
                &fwd_headers,
                &body,
                &base_url,
                &api_key,
                &token_field,
                &fo_id,
            ),
            t => {
                if is_count {
                    // 翻译目标没有可透传的 count_tokens; 粗略估算(字符数/4), 别 404 卡住 claude。
                    let est = (body.len() / 4).max(1) as u64;
                    let out =
                        serde_json::to_vec(&json!({ "input_tokens": est })).unwrap_or_default();
                    write_simple(&mut stream, 200, "application/json", &out);
                } else {
                    handle_messages(&mut stream, &body, t, &fo_id);
                }
            }
        }
    } else if method == "GET" {
        write_simple(
            &mut stream,
            200,
            "application/json",
            b"{\"ok\":true,\"service\":\"polaris-local-router\"}",
        );
    } else {
        anthropic_error(&mut stream, 404, "未知路由");
    }
    Ok(())
}

/// 拆路径前缀: `/p/{id}/rest` → (id, "/rest"); 无 `/p/` 前缀 → ("", 原路径)。
fn split_route(path: &str) -> (String, String) {
    if let Some(rest) = path.strip_prefix("/p/") {
        let mut it = rest.splitn(2, '/');
        let id = it.next().unwrap_or("").to_string();
        let sub = it
            .next()
            .map(|s| format!("/{s}"))
            .unwrap_or_else(|| "/".to_string());
        (id, sub)
    } else {
        (String::new(), path.to_string())
    }
}

fn write_simple(stream: &mut TcpStream, code: u16, ctype: &str, body: &[u8]) {
    let status = match code {
        200 => "200 OK",
        400 => "400 Bad Request",
        401 => "401 Unauthorized",
        404 => "404 Not Found",
        413 => "413 Payload Too Large",
        502 => "502 Bad Gateway",
        _ => "500 Internal Server Error",
    };
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

/// 以 Anthropic 错误格式(顶层 JSON)回吐, claude 会把 message 显示给用户
fn anthropic_error(stream: &mut TcpStream, code: u16, message: &str) {
    let etype = match code {
        400 => "invalid_request_error",
        401 => "authentication_error",
        404 => "not_found_error",
        _ => "api_error",
    };
    let body = json!({ "type": "error", "error": { "type": etype, "message": message } });
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    write_simple(stream, code, "application/json", &bytes);
}

fn write_event(stream: &mut TcpStream, event: &str, data: &Value) -> std::io::Result<()> {
    let payload = serde_json::to_string(data).unwrap_or_else(|_| "{}".into());
    let frame = format!("event: {event}\ndata: {payload}\n\n");
    stream.write_all(frame.as_bytes())?;
    stream.flush()
}

// ───────────────────────── /v1/messages 主流程 ─────────────────────────

fn handle_messages(stream: &mut TcpStream, body: &[u8], target: RouteTarget, fo_id: &str) {
    let req: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return anthropic_error(stream, 400, &format!("请求体不是合法 JSON: {e}")),
    };
    let want_stream = req.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    match target {
        RouteTarget::Chatgpt => handle_chatgpt(stream, &req, want_stream, fo_id),
        RouteTarget::OpenAiCompat {
            base_url,
            api_key,
            model,
        } => handle_openai_compat(stream, &req, want_stream, &base_url, &api_key, &model, fo_id),
        // AnthropicCompat 在 handle_conn 就地透传, 不会走到这里
        RouteTarget::AnthropicCompat { .. } => {
            anthropic_error(stream, 500, "内部路由错误: 透传目标不应进翻译分支")
        }
    }
}

// ───────────────────────── Anthropic 兼容上游: 纯透传 ─────────────────────────

/// 把 Anthropic 请求原样转发到 Anthropic 兼容上游, 响应字节流原样回吐(SSE 逐块 flush)。
/// 只做两件事: ①按供应商 token_field 实时注入鉴权头(ANTHROPIC_API_KEY → x-api-key,
/// 其余 → Authorization Bearer); ②透传 anthropic-version / anthropic-beta 协议头。
/// 上游 4xx/5xx 也原样转达 —— claude 要看到真实报错才好自愈/提示。
fn passthrough_forward(
    stream: &mut TcpStream,
    path: &str,
    fwd_headers: &[(String, String)],
    body: &[u8],
    base_url: &str,
    api_key: &str,
    token_field: &str,
    fo_id: &str,
) {
    let url = format!("{}{}", base_url.trim_end_matches('/'), path);
    let mut req = upstream_agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .set("Accept", "text/event-stream")
        .set("User-Agent", USER_AGENT);
    let mut has_version = false;
    for (k, v) in fwd_headers {
        if k == "anthropic-version" {
            has_version = true;
        }
        req = req.set(k, v);
    }
    if !has_version {
        req = req.set("anthropic-version", "2023-06-01");
    }
    req = if token_field == "ANTHROPIC_API_KEY" {
        req.set("x-api-key", api_key)
    } else {
        req.set("Authorization", &format!("Bearer {api_key}"))
    };

    let resp = match req.send_bytes(body) {
        Ok(r) => {
            note_provider_success(fo_id); // 2xx: 清故障滑窗计数
            r
        }
        // 上游错误状态: 不翻译不吞掉, 连状态码带 body 原样转达;
        // 鉴权/欠费/服务端故障类顺手计入故障滑窗(达阈自动切备用供应商)。
        Err(ureq::Error::Status(code, r)) => {
            if status_counts_as_failure(code) {
                note_provider_failure(fo_id);
            }
            r
        }
        Err(ureq::Error::Transport(t)) => {
            note_provider_failure(fo_id); // 网络类失败: 连不上/超时
            let m = format!("连接上游失败: {t}");
            set_error(m.clone());
            return anthropic_error(stream, 502, &m);
        }
    };
    relay_response(stream, resp);
}

fn reason_phrase(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        529 => "Overloaded",
        _ => "Status",
    }
}

/// 上游响应(状态码 + Content-Type + body 字节流)原样中继给客户端; SSE 逐块 flush 保低延迟。
fn relay_response(stream: &mut TcpStream, resp: ureq::Response) {
    let code = resp.status();
    let ct = resp
        .header("Content-Type")
        .unwrap_or("application/json")
        .to_string();
    if code >= 400 {
        set_error(format!("上游 HTTP {code}(透传)"));
    }
    let head = format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: {ct}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        reason_phrase(code)
    );
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    let mut reader = resp.into_reader();
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if stream.write_all(&buf[..n]).is_err() {
                    break;
                }
                let _ = stream.flush();
            }
            Err(_) => break,
        }
    }
    let _ = stream.flush();
}

/// ChatGPT 订阅上游(Responses 协议, OAuth 鉴权 + 401 静默刷新重试)
fn handle_chatgpt(stream: &mut TcpStream, req: &Value, want_stream: bool, fo_id: &str) {
    let auth = match load_auth() {
        Ok(a) => a,
        Err(e) => {
            set_error(e.clone());
            return anthropic_error(stream, 401, &e);
        }
    };

    let upstream = match build_responses_body(req) {
        Ok(b) => b,
        Err(e) => return anthropic_error(stream, 400, &e),
    };

    // 调上游: 401/403 先刷新 token 重试一次。注意故障滑窗只记「重试后仍失败」——
    // 静默刷新本身是设计内的自愈, 刷新成功不算供应商坏。
    let resp = match call_upstream(&auth, &upstream) {
        Ok(r) => {
            note_provider_success(fo_id);
            r
        }
        Err(UpstreamErr::Unauthorized) => match refresh_auth(&auth) {
            Ok(a2) => match call_upstream(&a2, &upstream) {
                Ok(r) => {
                    note_provider_success(fo_id);
                    r
                }
                Err(e) => {
                    if upstream_err_counts(&e) {
                        note_provider_failure(fo_id);
                    }
                    let m = e.message();
                    set_error(m.clone());
                    return anthropic_error(stream, 502, &m);
                }
            },
            Err(e) => {
                note_provider_failure(fo_id); // 刷新 token 也失败 = 鉴权类故障
                set_error(e.clone());
                return anthropic_error(stream, 401, &e);
            }
        },
        Err(e) => {
            if upstream_err_counts(&e) {
                note_provider_failure(fo_id);
            }
            let m = e.message();
            set_error(m.clone());
            return anthropic_error(stream, 502, &m);
        }
    };

    if want_stream {
        stream_translate(stream, resp, Flavor::Responses, DEFAULT_MODEL);
    } else {
        buffer_translate(stream, resp, Flavor::Responses, DEFAULT_MODEL);
    }
}

/// OpenAI 兼容上游(chat/completions, Bearer key 鉴权; key 是死的, 401 不重试)
fn handle_openai_compat(
    stream: &mut TcpStream,
    req: &Value,
    want_stream: bool,
    base_url: &str,
    api_key: &str,
    default_model: &str,
    fo_id: &str,
) {
    let upstream = match build_chat_body(req, default_model) {
        Ok(b) => b,
        Err(e) => return anthropic_error(stream, 400, &e),
    };

    let resp = match call_upstream_openai(base_url, api_key, &upstream) {
        Ok(r) => {
            note_provider_success(fo_id);
            r
        }
        Err(UpstreamErr::Unauthorized) => {
            note_provider_failure(fo_id); // key 是死的, 401 无从自愈 → 直接计入
            let m = "上游拒绝了 API Key (401/403), 请在坞里核对该供应商的 Key".to_string();
            set_error(m.clone());
            return anthropic_error(stream, 401, &m);
        }
        Err(e) => {
            if upstream_err_counts(&e) {
                note_provider_failure(fo_id);
            }
            let m = e.message();
            set_error(m.clone());
            return anthropic_error(stream, 502, &m);
        }
    };

    let model = req
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(default_model)
        .to_string();
    if want_stream {
        stream_translate(stream, resp, Flavor::OpenAiChat, &model);
    } else {
        buffer_translate(stream, resp, Flavor::OpenAiChat, &model);
    }
}

/// 故障滑窗的失败分类: 只有「供应商坏了」的信号才计入 —— 鉴权失效(401/403)、
/// 欠费(402)、服务端故障(5xx)。400 参数错是我们/claude 的问题, 429 是临时限流,
/// 都不该触发切换供应商。
fn status_counts_as_failure(code: u16) -> bool {
    matches!(code, 401 | 402 | 403) || code >= 500
}

/// UpstreamErr 版分类(翻译分支用): Unauthorized/Transport 必计, Http 按状态码。
fn upstream_err_counts(e: &UpstreamErr) -> bool {
    match e {
        UpstreamErr::Unauthorized | UpstreamErr::Transport(_) => true,
        UpstreamErr::Http(c, _) => status_counts_as_failure(*c),
    }
}

enum UpstreamErr {
    Unauthorized,
    Http(u16, String),
    Transport(String),
}
impl UpstreamErr {
    fn message(&self) -> String {
        match self {
            UpstreamErr::Unauthorized => "ChatGPT 授权已失效, 请在坞里重新授权 Codex".into(),
            UpstreamErr::Http(c, b) => format!("上游 HTTP {c}: {b}"),
            UpstreamErr::Transport(t) => format!("连接上游失败: {t}"),
        }
    }
}

/// 带超时的上游 agent: connect/read/write 都设上限。read 用 per-read 超时 (见 UPSTREAM_READ_TIMEOUT
/// 注释), 不设整条 call 的全局 deadline, 以免误杀正在持续吐 token 的长 SSE 回复。
/// 进程级共享(OnceLock): Agent 持有连接池, 每次现建 = 每个请求都重做 TCP+TLS 握手
/// 且上一条连接直接丢弃。共享后同上游(chatgpt.com)的连续请求走 keep-alive 复用。
fn upstream_agent() -> ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT
        .get_or_init(|| {
            ureq::AgentBuilder::new()
                .timeout_connect(UPSTREAM_CONNECT_TIMEOUT)
                .timeout_read(UPSTREAM_READ_TIMEOUT)
                .timeout_write(IO_TIMEOUT)
                .build()
        })
        .clone()
}

fn call_upstream(auth: &Auth, body: &Value) -> Result<ureq::Response, UpstreamErr> {
    let session = gen_uuid();
    let r = upstream_agent()
        .post(BACKEND_URL)
        .set("Authorization", &format!("Bearer {}", auth.access_token))
        .set("chatgpt-account-id", &auth.account_id)
        .set("OpenAI-Beta", "responses=experimental")
        .set("originator", "codex_cli_rs")
        .set("session_id", &session)
        .set("Accept", "text/event-stream")
        .set("Content-Type", "application/json")
        .set("User-Agent", USER_AGENT)
        .send_json(body.clone());
    match r {
        Ok(resp) => Ok(resp),
        Err(ureq::Error::Status(401, _)) | Err(ureq::Error::Status(403, _)) => {
            Err(UpstreamErr::Unauthorized)
        }
        Err(ureq::Error::Status(code, resp)) => {
            let b = resp.into_string().unwrap_or_default();
            Err(UpstreamErr::Http(code, b.chars().take(400).collect()))
        }
        Err(ureq::Error::Transport(t)) => Err(UpstreamErr::Transport(t.to_string())),
    }
}

/// base_url → chat/completions 端点。约定与 cc-switch 一致:
/// 已带 /chat/completions 原样用; 以 /v1 结尾补 /chat/completions; 其余补 /v1/chat/completions。
fn openai_chat_url(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    if b.ends_with("/chat/completions") {
        b.to_string()
    } else if b.ends_with("/v1") {
        format!("{b}/chat/completions")
    } else {
        format!("{b}/v1/chat/completions")
    }
}

fn call_upstream_openai(
    base_url: &str,
    api_key: &str,
    body: &Value,
) -> Result<ureq::Response, UpstreamErr> {
    let r = upstream_agent()
        .post(&openai_chat_url(base_url))
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Accept", "text/event-stream")
        .set("Content-Type", "application/json")
        .set("User-Agent", USER_AGENT)
        .send_json(body.clone());
    match r {
        Ok(resp) => Ok(resp),
        Err(ureq::Error::Status(401, _)) | Err(ureq::Error::Status(403, _)) => {
            Err(UpstreamErr::Unauthorized)
        }
        Err(ureq::Error::Status(code, resp)) => {
            let b = resp.into_string().unwrap_or_default();
            Err(UpstreamErr::Http(code, b.chars().take(400).collect()))
        }
        Err(ureq::Error::Transport(t)) => Err(UpstreamErr::Transport(t.to_string())),
    }
}

// ───────────────────────── Anthropic → Responses 请求翻译 ─────────────────────────

fn build_responses_body(req: &Value) -> Result<Value, String> {
    let model = map_model(req.get("model").and_then(|v| v.as_str()).unwrap_or(""));
    let mut instructions = extract_system(req);
    if instructions.trim().is_empty() {
        instructions = "You are a helpful coding assistant.".to_string();
    }
    let input = build_input(req);
    if input.is_empty() {
        return Err("messages 为空".into());
    }
    let tools = build_tools(req);

    let mut body = json!({
        "model": model,
        "instructions": instructions,
        "input": input,
        "store": false,
        "stream": true,
        // gpt5.6-sol 是最新 ChatGPT 模型; 推理模型(o1/o3)用 reasoning 字段
        "reasoning": { "effort": "medium" },
        // 关键: ChatGPT 后端在 `store:false` 下带 `reasoning` 时, **必须**同时声明
        // `include: ["reasoning.encrypted_content"]`, 否则整条 /responses 请求被 400 拒
        // (官方 codex CLI client.rs 也是 reasoning⟹include 成对发; 缺它 → 授权成功却每条对话都失败)。
        // 我们不透传思维链, 收到的 reasoning 增量在 drive_upstream 里被忽略, 只为满足后端契约。
        "include": ["reasoning.encrypted_content"],
    });
    let obj = body.as_object_mut().unwrap();
    if !tools.is_empty() {
        obj.insert("tools".into(), Value::Array(tools));
        obj.insert("tool_choice".into(), json!("auto"));
        obj.insert("parallel_tool_calls".into(), json!(false));
    }
    if let Some(mt) = req.get("max_tokens").and_then(|v| v.as_u64()) {
        obj.insert("max_output_tokens".into(), json!(mt));
    }
    if let Some(t) = req.get("temperature").and_then(|v| v.as_f64()) {
        obj.insert("temperature".into(), json!(t));
    }
    Ok(body)
}

/// 防输错归一化：`gpt5.x` 系列少横杠是最常见笔误（上游真实 id 为 `gpt-5.x`），
/// 在路由层自动补上——用户配 `gpt5.6-sol` 与 `gpt-5.6-sol` 等效，杜绝 404。
fn normalize_gpt(m: &str) -> String {
    let low = m.to_ascii_lowercase();
    if low.starts_with("gpt") && !low.starts_with("gpt-") && low.len() > 3 {
        format!("gpt-{}", &m[3..])
    } else {
        m.to_string()
    }
}

/// 模型映射: 空→默认; gpt-*/o*/codex 透传(归一化); 其余(claude-* 等)→默认 codex 模型
fn map_model(m: &str) -> String {
    let low = m.to_ascii_lowercase();
    if low.is_empty() {
        return DEFAULT_MODEL.into();
    }
    if low.contains("codex")
        || low.starts_with("gpt")
        || low.starts_with("o1")
        || low.starts_with("o3")
        || low.starts_with("o4")
    {
        return normalize_gpt(m);
    }
    DEFAULT_MODEL.into()
}

/// system: 字符串或 block 数组 → 拼成 instructions
fn extract_system(req: &Value) -> String {
    match req.get("system") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

/// messages → Responses input 项数组。文本/图片合成 message 项; tool_use/tool_result
/// 单独成 function_call / function_call_output 项(遇到时先把已累积的 message parts 落盘)。
fn build_input(req: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let Some(msgs) = req.get("messages").and_then(|m| m.as_array()) else {
        return out;
    };
    for msg in msgs {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        match msg.get("content") {
            Some(Value::String(s)) => out.push(message_item(role, s)),
            Some(Value::Array(blocks)) => {
                let mut parts: Vec<Value> = Vec::new();
                let flush = |out: &mut Vec<Value>, parts: &mut Vec<Value>| {
                    if !parts.is_empty() {
                        out.push(wrap_message(role, std::mem::take(parts)));
                    }
                };
                for b in blocks {
                    match b.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                        "text" => {
                            let text = b.get("text").and_then(|t| t.as_str()).unwrap_or("");
                            let kind = if role == "assistant" {
                                "output_text"
                            } else {
                                "input_text"
                            };
                            parts.push(json!({ "type": kind, "text": text }));
                        }
                        "image" => {
                            if let Some(url) = image_data_url(b) {
                                parts.push(json!({ "type": "input_image", "image_url": url }));
                            }
                        }
                        "tool_use" => {
                            flush(&mut out, &mut parts);
                            let id = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            let args = b.get("input").cloned().unwrap_or_else(|| json!({}));
                            let args_str =
                                serde_json::to_string(&args).unwrap_or_else(|_| "{}".into());
                            out.push(json!({
                                "type": "function_call",
                                "call_id": id,
                                "name": name,
                                "arguments": args_str,
                            }));
                        }
                        "tool_result" => {
                            flush(&mut out, &mut parts);
                            let id = b.get("tool_use_id").and_then(|v| v.as_str()).unwrap_or("");
                            out.push(json!({
                                "type": "function_call_output",
                                "call_id": id,
                                "output": tool_result_text(b),
                            }));
                        }
                        _ => {}
                    }
                }
                if !parts.is_empty() {
                    out.push(wrap_message(role, parts));
                }
            }
            _ => {}
        }
    }
    out
}

fn message_item(role: &str, text: &str) -> Value {
    let kind = if role == "assistant" {
        "output_text"
    } else {
        "input_text"
    };
    json!({ "type": "message", "role": role, "content": [{ "type": kind, "text": text }] })
}
fn wrap_message(role: &str, parts: Vec<Value>) -> Value {
    json!({ "type": "message", "role": role, "content": parts })
}

fn tool_result_text(b: &Value) -> String {
    match b.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|x| x.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn image_data_url(b: &Value) -> Option<String> {
    let src = b.get("source")?;
    match src.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        "base64" => {
            let media = src
                .get("media_type")
                .and_then(|t| t.as_str())
                .unwrap_or("image/png");
            let data = src.get("data").and_then(|t| t.as_str()).unwrap_or("");
            Some(format!("data:{media};base64,{data}"))
        }
        "url" => src.get("url").and_then(|t| t.as_str()).map(String::from),
        _ => None,
    }
}

fn build_tools(req: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let Some(tools) = req.get("tools").and_then(|t| t.as_array()) else {
        return out;
    };
    for t in tools {
        let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let desc = t.get("description").and_then(|v| v.as_str()).unwrap_or("");
        let schema = t
            .get("input_schema")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object" }));
        out.push(json!({
            "type": "function",
            "name": name,
            "description": desc,
            "parameters": schema,
        }));
    }
    out
}

// ───────────────────────── Anthropic → chat/completions 请求翻译 ─────────────────────────

/// 推理系模型(o1/o3/o4/gpt-5*, 含新命名 gpt5.6-sol 等无连字符写法): 只认
/// max_completion_tokens, 且拒绝自定义 temperature。
fn is_reasoning_model(m: &str) -> bool {
    let low = m.to_ascii_lowercase();
    low.starts_with("o1")
        || low.starts_with("o3")
        || low.starts_with("o4")
        || low.starts_with("gpt-5")
        || low.starts_with("gpt5")
}

/// OpenAI 侧模型映射: claude 发来的一般是坞里钉的模型名, 原样透传;
/// 空 / claude-*(没钉时的 Claude 默认名, 上游必不认)→ 回落该家默认模型。
fn map_model_openai(m: &str, default_model: &str) -> String {
    let t = m.trim();
    if t.is_empty() || t.to_ascii_lowercase().starts_with("claude") {
        normalize_gpt(default_model)
    } else {
        normalize_gpt(t)
    }
}

/// Anthropic /v1/messages 请求 → OpenAI chat/completions 请求。
/// system→messages[0](role:system); assistant 的 text+tool_use→content+tool_calls;
/// user 的 tool_result→独立 role:"tool" 消息(按 block 顺序落, 天然紧跟对应 tool_calls);
/// 图片→image_url(data URI)。思维链不透传。
fn build_chat_body(req: &Value, default_model: &str) -> Result<Value, String> {
    let model = map_model_openai(
        req.get("model").and_then(|v| v.as_str()).unwrap_or(""),
        default_model,
    );
    let mut messages: Vec<Value> = Vec::new();
    let system = extract_system(req);
    if !system.trim().is_empty() {
        messages.push(json!({ "role": "system", "content": system }));
    }

    let Some(msgs) = req.get("messages").and_then(|m| m.as_array()) else {
        return Err("messages 为空".into());
    };
    for msg in msgs {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        match msg.get("content") {
            Some(Value::String(s)) => messages.push(json!({ "role": role, "content": s })),
            Some(Value::Array(blocks)) => {
                if role == "assistant" {
                    let mut text = String::new();
                    let mut tool_calls: Vec<Value> = Vec::new();
                    for b in blocks {
                        match b.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                            "text" => {
                                text.push_str(b.get("text").and_then(|t| t.as_str()).unwrap_or(""))
                            }
                            "tool_use" => {
                                let id = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                let args = b.get("input").cloned().unwrap_or_else(|| json!({}));
                                let args_str =
                                    serde_json::to_string(&args).unwrap_or_else(|_| "{}".into());
                                tool_calls.push(json!({
                                    "id": id,
                                    "type": "function",
                                    "function": { "name": name, "arguments": args_str },
                                }));
                            }
                            _ => {}
                        }
                    }
                    if text.is_empty() && tool_calls.is_empty() {
                        continue;
                    }
                    let mut m = serde_json::Map::new();
                    m.insert("role".into(), json!("assistant"));
                    m.insert(
                        "content".into(),
                        if text.is_empty() { Value::Null } else { json!(text) },
                    );
                    if !tool_calls.is_empty() {
                        m.insert("tool_calls".into(), Value::Array(tool_calls));
                    }
                    messages.push(Value::Object(m));
                } else {
                    // user: text/image 累积成一条; tool_result 单独成 role:"tool" 消息
                    let mut parts: Vec<Value> = Vec::new();
                    let mut has_image = false;
                    let flush = |messages: &mut Vec<Value>, parts: &mut Vec<Value>, has_image: &mut bool| {
                        if parts.is_empty() {
                            return;
                        }
                        // 纯单段文本降成 string(网关兼容面最大); 含图才用 parts 数组
                        let content = if !*has_image && parts.len() == 1 {
                            parts[0]
                                .get("text")
                                .cloned()
                                .unwrap_or_else(|| json!(""))
                        } else {
                            Value::Array(std::mem::take(parts))
                        };
                        parts.clear();
                        *has_image = false;
                        messages.push(json!({ "role": "user", "content": content }));
                    };
                    for b in blocks {
                        match b.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                            "text" => {
                                let t = b.get("text").and_then(|t| t.as_str()).unwrap_or("");
                                parts.push(json!({ "type": "text", "text": t }));
                            }
                            "image" => {
                                if let Some(url) = image_data_url(b) {
                                    has_image = true;
                                    parts.push(
                                        json!({ "type": "image_url", "image_url": { "url": url } }),
                                    );
                                }
                            }
                            "tool_result" => {
                                flush(&mut messages, &mut parts, &mut has_image);
                                let id =
                                    b.get("tool_use_id").and_then(|v| v.as_str()).unwrap_or("");
                                messages.push(json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": tool_result_text(b),
                                }));
                            }
                            _ => {}
                        }
                    }
                    flush(&mut messages, &mut parts, &mut has_image);
                }
            }
            _ => {}
        }
    }
    if messages.iter().all(|m| m.get("role") == Some(&json!("system"))) {
        return Err("messages 为空".into());
    }

    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": true,
        // 最后一帧带 usage(标准扩展, 主流网关都认); 不认的顶多不回 usage, 不影响正文
        "stream_options": { "include_usage": true },
    });
    let obj = body.as_object_mut().unwrap();

    let tools = build_tools(req);
    if !tools.is_empty() {
        let wrapped: Vec<Value> = tools
            .into_iter()
            .map(|t| {
                // build_tools 产出 Responses 扁平格式, chat/completions 要包一层 function
                json!({
                    "type": "function",
                    "function": {
                        "name": t.get("name").cloned().unwrap_or_default(),
                        "description": t.get("description").cloned().unwrap_or_default(),
                        "parameters": t.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object" })),
                    }
                })
            })
            .collect();
        obj.insert("tools".into(), Value::Array(wrapped));
        obj.insert("tool_choice".into(), json!("auto"));
    }
    if let Some(mt) = req.get("max_tokens").and_then(|v| v.as_u64()) {
        // 推理系模型只认 max_completion_tokens(发 max_tokens 直接 400)
        let key = if is_reasoning_model(&model) {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        obj.insert(key.into(), json!(mt));
    }
    if !is_reasoning_model(&model) {
        if let Some(t) = req.get("temperature").and_then(|v| v.as_f64()) {
            obj.insert("temperature".into(), json!(t));
        }
    }
    Ok(body)
}

// ───────────────────────── Responses SSE → Anthropic SSE 响应翻译 ─────────────────────────

/// 从上游 Responses 流里解析出的规范化事件
enum Norm {
    TextDelta(String),
    ToolStart {
        id: String,
        name: String,
    },
    ToolArgs(String),
    ToolStop,
    Done {
        stop: String,
        input_tokens: u64,
        output_tokens: u64,
    },
    Failed(String),
}

/// 逐行读上游 SSE, 把 Responses 事件归一成 Norm 回调给 emit。
fn drive_upstream<R: Read>(resp_reader: R, mut emit: impl FnMut(Norm)) {
    let mut reader = BufReader::new(resp_reader);
    let mut saw_tool = false;
    let mut in_tool = false;
    let mut tool_args_streamed = false;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        let line = line.trim_end();
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        match v.get("type").and_then(|x| x.as_str()).unwrap_or("") {
            "response.output_text.delta" => {
                if let Some(d) = v.get("delta").and_then(|x| x.as_str()) {
                    emit(Norm::TextDelta(d.to_string()));
                }
            }
            "response.output_item.added" => {
                let item = v.get("item");
                if item.and_then(|i| i.get("type")).and_then(|x| x.as_str())
                    == Some("function_call")
                {
                    let item = item.unwrap();
                    let id = item
                        .get("call_id")
                        .and_then(|x| x.as_str())
                        .or_else(|| item.get("id").and_then(|x| x.as_str()))
                        .unwrap_or("")
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    saw_tool = true;
                    in_tool = true;
                    tool_args_streamed = false;
                    emit(Norm::ToolStart { id, name });
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(d) = v.get("delta").and_then(|x| x.as_str()) {
                    tool_args_streamed = true;
                    emit(Norm::ToolArgs(d.to_string()));
                }
            }
            "response.function_call_arguments.done" => {
                // 后端只给完整 arguments 不给增量时, 在此补发一次
                if in_tool && !tool_args_streamed {
                    if let Some(a) = v.get("arguments").and_then(|x| x.as_str()) {
                        emit(Norm::ToolArgs(a.to_string()));
                    }
                }
            }
            "response.output_item.done" => {
                if in_tool
                    && v.get("item")
                        .and_then(|i| i.get("type"))
                        .and_then(|x| x.as_str())
                        == Some("function_call")
                {
                    in_tool = false;
                    emit(Norm::ToolStop);
                }
            }
            "response.completed" => {
                let (mut it, mut ot) = (0u64, 0u64);
                if let Some(u) = v.get("response").and_then(|r| r.get("usage")) {
                    it = u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                    ot = u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                }
                emit(Norm::Done {
                    stop: if saw_tool { "tool_use" } else { "end_turn" }.into(),
                    input_tokens: it,
                    output_tokens: ot,
                });
            }
            "response.failed" | "error" => {
                let msg = v
                    .get("response")
                    .and_then(|r| r.get("error"))
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .or_else(|| {
                        v.get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(|m| m.as_str())
                    })
                    .or_else(|| v.get("message").and_then(|m| m.as_str()))
                    .unwrap_or("上游返回错误")
                    .to_string();
                emit(Norm::Failed(msg));
            }
            _ => {}
        }
    }
}

/// 上游协议风味 → 选哪个 SSE 归一化驱动
#[derive(Clone, Copy)]
enum Flavor {
    /// ChatGPT Responses 协议
    Responses,
    /// OpenAI chat/completions 协议
    OpenAiChat,
}

fn drive(flavor: Flavor, reader: impl Read, emit: impl FnMut(Norm)) {
    match flavor {
        Flavor::Responses => drive_upstream(reader, emit),
        Flavor::OpenAiChat => drive_openai_chat(reader, emit),
    }
}

/// 逐行读 chat/completions SSE, 归一成 Norm 回调给 emit。
/// 与 Responses 不同, 该协议没有显式的 completed 事件 —— 以 `[DONE]`/EOF 收口,
/// finish_reason 与(stream_options 带回的)usage 在途中记下, 收口时统一 emit Done。
fn drive_openai_chat<R: Read>(resp_reader: R, mut emit: impl FnMut(Norm)) {
    let mut reader = BufReader::new(resp_reader);
    let mut saw_tool = false;
    let mut tool_open = false;
    let mut cur_tool_index: i64 = -1;
    let mut finish = String::new();
    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        let line = line.trim_end();
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            break;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        // 上游把错误塞进流里(部分网关的习惯)→ 直接失败收口
        if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("上游返回错误")
                .to_string();
            emit(Norm::Failed(msg));
            return;
        }
        // usage 帧(include_usage 时最后单独一帧, choices 为空)
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            input_tokens = u
                .get("prompt_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(input_tokens);
            output_tokens = u
                .get("completion_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(output_tokens);
        }
        let Some(choice) = v
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first())
        else {
            continue;
        };
        if let Some(delta) = choice.get("delta") {
            // 思维链增量(reasoning_content, DeepSeek 等方言)刻意不透传, 与 Responses 同款
            if let Some(t) = delta.get("content").and_then(|x| x.as_str()) {
                if !t.is_empty() {
                    emit(Norm::TextDelta(t.to_string()));
                }
            }
            if let Some(tcs) = delta.get("tool_calls").and_then(|x| x.as_array()) {
                for tc in tcs {
                    let idx = tc.get("index").and_then(|x| x.as_i64()).unwrap_or(0);
                    if idx != cur_tool_index {
                        if tool_open {
                            emit(Norm::ToolStop);
                        }
                        cur_tool_index = idx;
                        tool_open = true;
                        saw_tool = true;
                        let id = tc
                            .get("id")
                            .and_then(|x| x.as_str())
                            .filter(|s| !s.is_empty())
                            .map(String::from)
                            .unwrap_or_else(|| format!("call_{idx}"));
                        let name = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        emit(Norm::ToolStart { id, name });
                    }
                    if let Some(a) = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|x| x.as_str())
                    {
                        if !a.is_empty() {
                            emit(Norm::ToolArgs(a.to_string()));
                        }
                    }
                }
            }
        }
        if let Some(f) = choice.get("finish_reason").and_then(|x| x.as_str()) {
            finish = f.to_string();
        }
    }
    if tool_open {
        emit(Norm::ToolStop);
    }
    let stop = if saw_tool || finish == "tool_calls" {
        "tool_use"
    } else if finish == "length" {
        "max_tokens"
    } else {
        "end_turn"
    };
    emit(Norm::Done {
        stop: stop.into(),
        input_tokens,
        output_tokens,
    });
}

/// 流式: 把 Norm 事件实时翻成 Anthropic SSE 写回 claude
fn stream_translate(client: &mut TcpStream, resp: ureq::Response, flavor: Flavor, model: &str) {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
    if client.write_all(head.as_bytes()).is_err() {
        return;
    }

    let msg_id = format!("msg_{}", gen_id());
    let _ = write_event(
        client,
        "message_start",
        &json!({
            "type": "message_start",
            "message": {
                "id": msg_id,
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": Value::Null,
                "stop_sequence": Value::Null,
                "usage": { "input_tokens": 0, "output_tokens": 0 },
            }
        }),
    );

    let mut index: i64 = -1;
    let mut text_open = false;
    let mut tool_open = false;
    let mut done_sent = false;

    drive(flavor, resp.into_reader(), |ev| match ev {
        Norm::TextDelta(t) => {
            if tool_open {
                let _ = write_event(
                    client,
                    "content_block_stop",
                    &json!({ "type": "content_block_stop", "index": index }),
                );
                tool_open = false;
            }
            if !text_open {
                index += 1;
                let _ = write_event(
                    client,
                    "content_block_start",
                    &json!({
                        "type": "content_block_start", "index": index,
                        "content_block": { "type": "text", "text": "" }
                    }),
                );
                text_open = true;
            }
            let _ = write_event(
                client,
                "content_block_delta",
                &json!({
                    "type": "content_block_delta", "index": index,
                    "delta": { "type": "text_delta", "text": t }
                }),
            );
        }
        Norm::ToolStart { id, name } => {
            if text_open {
                let _ = write_event(
                    client,
                    "content_block_stop",
                    &json!({ "type": "content_block_stop", "index": index }),
                );
                text_open = false;
            }
            index += 1;
            tool_open = true;
            let _ = write_event(
                client,
                "content_block_start",
                &json!({
                    "type": "content_block_start", "index": index,
                    "content_block": { "type": "tool_use", "id": id, "name": name, "input": {} }
                }),
            );
        }
        Norm::ToolArgs(a) => {
            if tool_open {
                let _ = write_event(
                    client,
                    "content_block_delta",
                    &json!({
                        "type": "content_block_delta", "index": index,
                        "delta": { "type": "input_json_delta", "partial_json": a }
                    }),
                );
            }
        }
        Norm::ToolStop => {
            if tool_open {
                let _ = write_event(
                    client,
                    "content_block_stop",
                    &json!({ "type": "content_block_stop", "index": index }),
                );
                tool_open = false;
            }
        }
        Norm::Done {
            stop,
            input_tokens,
            output_tokens,
        } => {
            if text_open {
                let _ = write_event(
                    client,
                    "content_block_stop",
                    &json!({ "type": "content_block_stop", "index": index }),
                );
                text_open = false;
            }
            if tool_open {
                let _ = write_event(
                    client,
                    "content_block_stop",
                    &json!({ "type": "content_block_stop", "index": index }),
                );
                tool_open = false;
            }
            let _ = write_event(
                client,
                "message_delta",
                &json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": stop, "stop_sequence": Value::Null },
                    "usage": { "input_tokens": input_tokens, "output_tokens": output_tokens }
                }),
            );
            let _ = write_event(client, "message_stop", &json!({ "type": "message_stop" }));
            done_sent = true;
        }
        Norm::Failed(msg) => {
            set_error(msg.clone());
            let _ = write_event(
                client,
                "error",
                &json!({
                    "type": "error", "error": { "type": "api_error", "message": msg }
                }),
            );
            done_sent = true;
        }
    });

    if !done_sent {
        // 上游中途断流: 优雅收尾, 别让 claude 一直挂着
        if text_open {
            let _ = write_event(
                client,
                "content_block_stop",
                &json!({ "type": "content_block_stop", "index": index }),
            );
        }
        if tool_open {
            let _ = write_event(
                client,
                "content_block_stop",
                &json!({ "type": "content_block_stop", "index": index }),
            );
        }
        let _ = write_event(
            client,
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn", "stop_sequence": Value::Null },
                "usage": { "input_tokens": 0, "output_tokens": 0 }
            }),
        );
        let _ = write_event(client, "message_stop", &json!({ "type": "message_stop" }));
    }
    let _ = client.flush();
}

/// 非流式(stream:false): 累积成完整 Anthropic message JSON 一次性返回
fn buffer_translate(client: &mut TcpStream, resp: ureq::Response, flavor: Flavor, model: &str) {
    let mut blocks: Vec<Value> = Vec::new();
    let mut cur_text = String::new();
    let mut cur_tool: Option<(String, String, String)> = None; // id, name, args
    let mut stop_reason = "end_turn".to_string();
    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;
    let mut err: Option<String> = None;

    drive(flavor, resp.into_reader(), |ev| match ev {
        Norm::TextDelta(t) => cur_text.push_str(&t),
        Norm::ToolStart { id, name } => {
            if !cur_text.is_empty() {
                blocks.push(json!({ "type": "text", "text": cur_text.clone() }));
                cur_text.clear();
            }
            cur_tool = Some((id, name, String::new()));
        }
        Norm::ToolArgs(a) => {
            if let Some(t) = cur_tool.as_mut() {
                t.2.push_str(&a);
            }
        }
        Norm::ToolStop => {
            if let Some((id, name, args)) = cur_tool.take() {
                let input: Value = serde_json::from_str(&args).unwrap_or_else(|_| json!({}));
                blocks.push(json!({ "type": "tool_use", "id": id, "name": name, "input": input }));
            }
        }
        Norm::Done {
            stop,
            input_tokens: it,
            output_tokens: ot,
        } => {
            stop_reason = stop;
            input_tokens = it;
            output_tokens = ot;
        }
        Norm::Failed(m) => err = Some(m),
    });
    if !cur_text.is_empty() {
        blocks.push(json!({ "type": "text", "text": cur_text }));
    }

    if let Some(m) = err {
        set_error(m.clone());
        return anthropic_error(client, 502, &m);
    }
    let body = json!({
        "id": format!("msg_{}", gen_id()),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": blocks,
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": { "input_tokens": input_tokens, "output_tokens": output_tokens },
    });
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    write_simple(client, 200, "application/json", &bytes);
}

// ───────────────────────── 鉴权: 读 ~/.codex/auth.json + 刷新 ─────────────────────────

struct Auth {
    access_token: String,
    refresh_token: String,
    account_id: String,
}

fn load_auth() -> Result<Auth, String> {
    let mut auth = load_auth_file()?;
    // access_token 将过期则主动刷新(失败不致命: 还可能在上游 401 时再刷一次)
    if token_expiring(&auth.access_token) {
        if let Ok(a2) = refresh_auth(&auth) {
            auth = a2;
        }
    }
    Ok(auth)
}

/// 只读盘解析 auth.json,不触发刷新(refresh_auth 单飞锁内重读盘要用它,避免递归/死锁)。
fn load_auth_file() -> Result<Auth, String> {
    let path = codex_auth_path().ok_or_else(|| "无法定位 ~/.codex/auth.json".to_string())?;
    let text = std::fs::read_to_string(&path)
        .map_err(|_| "未找到 ChatGPT 授权, 请先在坞里授权 Codex".to_string())?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("auth.json 解析失败: {e}"))?;
    let tokens = v
        .get("tokens")
        .ok_or_else(|| "auth.json 缺少 tokens, 请重新授权".to_string())?;
    let access = tokens
        .get("access_token")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if access.is_empty() {
        return Err("ChatGPT 授权无效(缺 access_token), 请重新授权".into());
    }
    let auth = Auth {
        access_token: access,
        refresh_token: tokens
            .get("refresh_token")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        account_id: tokens
            .get("account_id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
    };
    Ok(auth)
}

/// 解 JWT 的 exp, 距现在不足 60s 即视为将过期
fn token_expiring(access: &str) -> bool {
    let Some(payload) = access.split('.').nth(1) else {
        return false;
    };
    let Some(bytes) = codex_b64url_decode(payload) else {
        return false;
    };
    let Ok(claims) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    let Some(exp) = claims.get("exp").and_then(|x| x.as_i64()) else {
        return false;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    now + 60 >= exp
}

/// 刷新单飞锁:claude 会并发发请求(主对话 + haiku 小任务),两个连接线程同时 401 会
/// 同时进刷新 → 双写 auth.json 且 OAuth refresh_token 轮换时后完成者用旧 refresh_token
/// 顶掉新的(下次刷新即失效,用户被迫重新扫码)。锁内先重读盘:别人刚刷完就直接用成果。
static REFRESH_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

fn refresh_auth(auth: &Auth) -> Result<Auth, String> {
    if auth.refresh_token.is_empty() {
        return Err("缺少 refresh_token, 请重新授权 ChatGPT".into());
    }
    let _flight = REFRESH_LOCK.lock();
    // 排队期间别的线程可能已完成刷新并回写:盘上 token 变了且没到期 → 直接复用。
    if let Ok(cur) = load_auth_file() {
        if cur.access_token != auth.access_token && !token_expiring(&cur.access_token) {
            return Ok(cur);
        }
    }
    let resp = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .build()
        .post(CODEX_OAUTH_TOKEN_URL)
        .set("User-Agent", USER_AGENT)
        .send_form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &auth.refresh_token),
            ("client_id", CODEX_CLIENT_ID),
            ("scope", "openid profile email"),
        ])
        .map_err(|e| format!("刷新 ChatGPT token 失败: {}", short_err(e)))?;
    let v: Value = resp
        .into_json()
        .map_err(|e| format!("解析刷新响应失败: {e}"))?;
    let access = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if access.is_empty() {
        return Err("刷新响应缺少 access_token".into());
    }
    let refresh = v
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .map(String::from)
        .unwrap_or_else(|| auth.refresh_token.clone());
    let id_token = v
        .get("id_token")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let new = Auth {
        access_token: access,
        refresh_token: refresh,
        account_id: auth.account_id.clone(),
    };
    persist_auth(&new, &id_token);
    Ok(new)
}

/// 刷新后按官方格式回写 auth.json(id_token 若未返回则保留旧值), 外部 codex CLI 也能续用
fn persist_auth(auth: &Auth, id_token: &str) {
    let Some(path) = codex_auth_path() else {
        return;
    };
    let existing_id = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| {
            v.get("tokens")
                .and_then(|t| t.get("id_token"))
                .and_then(|x| x.as_str())
                .map(String::from)
        })
        .unwrap_or_default();
    let id = if id_token.is_empty() {
        existing_id
    } else {
        id_token.to_string()
    };
    let body = json!({
        "OPENAI_API_KEY": Value::Null,
        // 见 provider::codex_write_auth_json —— 刷新回写也要带 auth_mode, 否则覆盖掉
        // 外部 codex CLI 原写入的标记, 事后 codex/社区插件会拒认这份订阅凭证。
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": id,
            "access_token": auth.access_token,
            "refresh_token": auth.refresh_token,
            "account_id": auth.account_id,
        },
        "last_refresh": codex_rfc3339_now(),
    });
    if let Ok(txt) = serde_json::to_string_pretty(&body) {
        // 原子回写:auth.json 是授权凭证,裸 fs::write 崩在半截会撕坏 JSON,
        // refresh_token 一丢用户就得重新扫码。与全仓其它落盘同款走 atomic_write。
        let _ = crate::provider::atomic_write(&path, &txt);
    }
}

fn short_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let b = resp.into_string().unwrap_or_default();
            format!("HTTP {code} {}", b.chars().take(200).collect::<String>())
        }
        ureq::Error::Transport(t) => format!("网络错误: {t}"),
    }
}

// ───────────────────────── 杂项 ─────────────────────────

fn gen_id() -> String {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{n:x}{c:x}")
}

/// 伪 uuid v4(够后端当 session_id 用), 不引 uuid crate
fn gen_uuid() -> String {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    let a = (n & 0xffff_ffff) as u32;
    let b = ((n >> 32) & 0xffff) as u16;
    let cc = (((n >> 48) & 0x0fff) as u16) | 0x4000;
    let d = ((c & 0x3fff) as u16) | 0x8000;
    let e = (n >> 16) & 0xffff_ffff_ffff;
    format!("{a:08x}-{b:04x}-{cc:04x}-{d:04x}-{e:012x}")
}

// ───────────────────────── Command: 代理状态(供坞展示) ─────────────────────────

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexProxyInfo {
    pub running: bool,
    pub port: u16,
    pub last_error: String,
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn codex_proxy_info() -> CodexProxyInfo {
    let p = port();
    CodexProxyInfo {
        running: p.is_some(),
        port: p.unwrap_or(0),
        last_error: last_error(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn map_model_passthrough_and_default() {
        assert_eq!(map_model("gpt-5-codex"), "gpt-5-codex");
        assert_eq!(map_model("o3-mini"), "o3-mini");
        assert_eq!(map_model("codex-foo"), "codex-foo");
        // 空 / claude-* / 未知 → 回落默认 codex 模型
        assert_eq!(map_model("").as_str(), DEFAULT_MODEL);
        assert_eq!(map_model("claude-opus-4").as_str(), DEFAULT_MODEL);
    }

    #[test]
    fn extract_system_handles_all_shapes() {
        assert_eq!(extract_system(&json!({"system": "hi"})), "hi");
        assert_eq!(
            extract_system(&json!({"system": [{"text":"a"},{"text":"b"}]})),
            "a\n\nb"
        );
        assert_eq!(extract_system(&json!({})), "");
        // 畸形: system 是数字 / block 缺 text → 不 panic, 返回空串
        assert_eq!(extract_system(&json!({"system": 42})), "");
        assert_eq!(extract_system(&json!({"system": [{"nope": 1}]})), "");
    }

    #[test]
    fn build_responses_body_rejects_empty_messages() {
        // 空 / 缺 messages → Err (而非 panic)
        assert!(build_responses_body(&json!({"model":"gpt-5","messages":[]})).is_err());
        assert!(build_responses_body(&json!({})).is_err());
    }

    #[test]
    fn build_responses_body_minimal_ok() {
        let req = json!({
            "model": "gpt-5-codex",
            "messages": [{"role":"user","content":"hello"}],
            "max_tokens": 100,
            "temperature": 0.5,
        });
        let body = build_responses_body(&req).expect("应翻译成功");
        assert_eq!(body["model"], "gpt-5-codex");
        assert_eq!(body["max_output_tokens"], 100);
        assert_eq!(body["stream"], true);
        assert!(body["input"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false));
    }

    #[test]
    fn build_responses_body_pairs_reasoning_with_encrypted_include() {
        // 回归护栏: 只要带 reasoning + store:false, 就必须同时声明
        // include=["reasoning.encrypted_content"], 否则 ChatGPT 后端 400 拒, 授权后每条对话都失败。
        let req = json!({
            "model": "gpt5.6-sol",
            "messages": [{"role":"user","content":"hi"}],
        });
        let body = build_responses_body(&req).expect("应翻译成功");
        assert_eq!(body["store"], false);
        assert!(body.get("reasoning").is_some(), "reasoning 应在场");
        let include = body["include"].as_array().expect("include 必须是数组");
        assert!(
            include.iter().any(|v| v == "reasoning.encrypted_content"),
            "store:false + reasoning 时必须带 reasoning.encrypted_content"
        );
    }

    #[test]
    fn normalize_gpt_fixes_missing_dash() {
        // 防输错：gpt5.x 少横杠自动补成上游真实 id 形态 gpt-5.x
        assert_eq!(normalize_gpt("gpt5.6-sol"), "gpt-5.6-sol");
        assert_eq!(map_model("gpt5.6-sol"), "gpt-5.6-sol");
        assert_eq!(map_model_openai("gpt5.6-terra", "x"), "gpt-5.6-terra");
        // 本就带横杠 / 非 gpt 前缀不受影响
        assert_eq!(normalize_gpt("gpt-5.6-sol"), "gpt-5.6-sol");
        assert_eq!(normalize_gpt("o3-mini"), "o3-mini");
    }

    #[test]
    fn split_route_variants() {
        assert_eq!(
            split_route("/v1/messages"),
            ("".into(), "/v1/messages".into())
        );
        assert_eq!(
            split_route("/p/kimi-x/v1/messages"),
            ("kimi-x".into(), "/v1/messages".into())
        );
        assert_eq!(split_route("/p/abc"), ("abc".into(), "/".into()));
        assert_eq!(
            split_route("/p/abc/v1/messages/count_tokens?x=1"),
            ("abc".into(), "/v1/messages/count_tokens?x=1".into())
        );
    }

    #[test]
    fn openai_chat_url_join_rules() {
        assert_eq!(
            openai_chat_url("https://api.openai.com/v1"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            openai_chat_url("https://gw.example.com"),
            "https://gw.example.com/v1/chat/completions"
        );
        assert_eq!(
            openai_chat_url("https://gw.example.com/api/v1/chat/completions"),
            "https://gw.example.com/api/v1/chat/completions"
        );
        assert_eq!(
            openai_chat_url("https://gw.example.com/v1/"),
            "https://gw.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn build_chat_body_minimal_and_reasoning() {
        let req = json!({
            "model": "gpt-4o", "system": "be nice",
            "messages": [{"role":"user","content":"hello"}],
            "max_tokens": 100, "temperature": 0.5,
        });
        let b = build_chat_body(&req, "gpt5.6-sol").expect("应翻译成功");
        assert_eq!(b["model"], "gpt-4o");
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][1]["content"], "hello");
        assert_eq!(b["max_tokens"], 100);
        assert_eq!(b["temperature"], 0.5);
        assert_eq!(b["stream"], true);

        // claude-* 回落默认; gpt-5* 是推理系 → max_completion_tokens、temperature 不透传
        let req2 = json!({
            "model": "claude-opus-4",
            "messages": [{"role":"user","content":"hi"}],
            "max_tokens": 50, "temperature": 0.9,
        });
        let b2 = build_chat_body(&req2, "gpt5.6-sol").expect("应翻译成功");
        // 默认模型回落时经 normalize_gpt 归一化为上游真实 id 形态（补横杠）
        assert_eq!(b2["model"], "gpt-5.6-sol");
        assert!(b2.get("max_tokens").is_none());
        assert_eq!(b2["max_completion_tokens"], 50);
        assert!(b2.get("temperature").is_none());

        // 空 messages → Err
        assert!(build_chat_body(&json!({"messages": []}), "gpt5.6-sol").is_err());
    }

    #[test]
    fn build_chat_body_tools_and_tool_round() {
        let req = json!({
            "model": "gpt-4o",
            "messages": [
                {"role":"user","content":"weather?"},
                {"role":"assistant","content":[
                    {"type":"text","text":"checking"},
                    {"type":"tool_use","id":"call_1","name":"get_weather","input":{"city":"SH"}}
                ]},
                {"role":"user","content":[
                    {"type":"tool_result","tool_use_id":"call_1","content":"sunny"},
                    {"type":"text","text":"and tomorrow?"}
                ]}
            ],
            "tools": [{"name":"get_weather","description":"d","input_schema":{"type":"object"}}],
        });
        let b = build_chat_body(&req, "gpt5.6-sol").expect("应翻译成功");
        let msgs = b["messages"].as_array().unwrap();
        // [user, assistant(text+tool_calls), tool, user]
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"], "checking");
        assert_eq!(msgs[1]["tool_calls"][0]["function"]["name"], "get_weather");
        assert_eq!(msgs[2]["role"], "tool");
        assert_eq!(msgs[2]["tool_call_id"], "call_1");
        assert_eq!(msgs[2]["content"], "sunny");
        assert_eq!(msgs[3]["role"], "user");
        assert_eq!(msgs[3]["content"], "and tomorrow?");
        // chat/completions 的 tools 要包一层 function
        assert_eq!(b["tools"][0]["type"], "function");
        assert_eq!(b["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(b["tool_choice"], "auto");
    }

    #[test]
    fn drive_openai_chat_text_tools_usage() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_9\",\"function\":{\"name\":\"f\",\"arguments\":\"{\\\"a\\\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\":1}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3}}\n\n",
            "data: [DONE]\n\n",
        );
        let mut evs: Vec<String> = Vec::new();
        let mut args = String::new();
        drive_openai_chat(std::io::Cursor::new(sse.as_bytes()), |n| match n {
            Norm::TextDelta(t) => evs.push(format!("text:{t}")),
            Norm::ToolStart { id, name } => evs.push(format!("start:{id}:{name}")),
            Norm::ToolArgs(a) => args.push_str(&a),
            Norm::ToolStop => evs.push("stop".into()),
            Norm::Done {
                stop,
                input_tokens,
                output_tokens,
            } => evs.push(format!("done:{stop}:{input_tokens}:{output_tokens}")),
            Norm::Failed(m) => evs.push(format!("fail:{m}")),
        });
        assert_eq!(evs[0], "text:Hel");
        assert_eq!(evs[1], "text:lo");
        assert_eq!(evs[2], "start:call_9:f");
        assert_eq!(args, "{\"a\":1}");
        assert_eq!(evs[3], "stop");
        assert_eq!(evs[4], "done:tool_use:7:3");
    }

    #[test]
    fn drive_openai_chat_error_frame_no_done() {
        let sse = "data: {\"error\":{\"message\":\"quota exceeded\"}}\n\n";
        let mut fail = String::new();
        let mut done = false;
        drive_openai_chat(std::io::Cursor::new(sse.as_bytes()), |n| match n {
            Norm::Failed(m) => fail = m,
            Norm::Done { .. } => done = true,
            _ => {}
        });
        assert_eq!(fail, "quota exceeded");
        assert!(!done, "错误收口后不应再 emit Done");
    }

    /// 真 TCP 端到端: 假 OpenAI 上游 ← 本地路由 ← 原生 socket 客户端。
    /// 验证: /p/{id} 按 id 解析上游、Bearer key/端点拼接正确、SSE 全程翻成 Anthropic 事件。
    #[test]
    fn e2e_openai_route_over_real_tcp() {
        use std::io::{Read as _, Write as _};
        use std::sync::{Arc, Mutex};

        // 假上游: 收一条请求(记下请求头), 回一段 chat/completions SSE
        let upstream = TcpListener::bind("127.0.0.1:0").expect("bind 假上游");
        let up_port = upstream.local_addr().unwrap().port();
        let captured: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let cap2 = captured.clone();
        // 循环 accept: 客户端侧带重试(满载并行测试下 Windows 回环偶发 RST), 每次重试都要有上游可用
        std::thread::spawn(move || {
            for s in upstream.incoming().flatten() {
                let mut s = s;
                let mut buf = [0u8; 65536];
                let mut head: Vec<u8> = Vec::new();
                loop {
                    let n = s.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    head.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&head).to_string();
                    if let Some(pos) = text.find("\r\n\r\n") {
                        let cl = text
                            .lines()
                            .find_map(|l| {
                                let low = l.to_ascii_lowercase();
                                low.strip_prefix("content-length:")
                                    .map(|v| v.trim().to_string())
                            })
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(0);
                        if head.len() >= pos + 4 + cl {
                            break;
                        }
                    }
                }
                *cap2.lock().unwrap() = String::from_utf8_lossy(&head).to_string();
                let body = concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}\n\n",
                    "data: [DONE]\n\n",
                );
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = s.write_all(resp.as_bytes());
            }
        });

        // 把假上游注册成一个 OpenAI 协议供应商(STORE_PATH 未初始化 → persist 静默跳过, 不落盘)
        crate::provider::provider_save(crate::provider::ProviderInput {
            id: Some("t-openai-e2e".into()),
            name: "e2e".into(),
            note: String::new(),
            website_url: String::new(),
            token_field: None,
            protocol: Some("openai".into()),
            settings_config: json!({"env": {
                "ANTHROPIC_BASE_URL": format!("http://127.0.0.1:{up_port}"),
                "ANTHROPIC_AUTH_TOKEN": "sk-e2e",
                "ANTHROPIC_MODEL": "gpt-test",
            }}),
        })
        .expect("注册供应商");

        let port = ensure_running().expect("本地路由应能启动");
        let req_body = serde_json::to_string(&json!({
            "model": "gpt-test", "stream": true, "max_tokens": 64,
            "messages": [{"role":"user","content":"hi"}],
        }))
        .unwrap();
        let req = format!(
            "POST /p/t-openai-e2e/v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            req_body.len(),
            req_body
        );
        // 全量并行测试满载时 Windows 回环偶发 RST(与被测逻辑无关的环境抖动)→ 重试最多 3 次;
        // 契约断言不放松: 任一次完整读到响应就按同一标准校验。
        let mut out = String::new();
        let mut read_err = None;
        for attempt in 0u64..3 {
            let mut c = TcpStream::connect(("127.0.0.1", port)).expect("连本地路由");
            c.write_all(req.as_bytes()).unwrap();
            out.clear();
            read_err = c.read_to_string(&mut out).err();
            if read_err.is_none() && out.contains("message_stop") {
                break;
            }
            std::thread::sleep(Duration::from_millis(120 * (attempt + 1)));
        }
        assert!(
            out.contains("message_start"),
            "响应应是 Anthropic SSE; 读到=[{out}] read_err={read_err:?} last_error={}",
            last_error()
        );
        assert!(out.contains("text_delta"), "应有文本增量: {out}");
        assert!(out.contains("你好"), "文本应透传: {out}");
        assert!(out.contains("\"input_tokens\":5"), "usage 应带回: {out}");
        assert!(out.contains("message_stop"), "应正常收口: {out}");
        let head = captured.lock().unwrap().clone();
        assert!(
            head.starts_with("POST /v1/chat/completions"),
            "上游应收到 chat/completions: {head}"
        );
        assert!(
            head.contains("Bearer sk-e2e"),
            "上游应收到该供应商的 Bearer key: {head}"
        );

        let _ = crate::provider::provider_delete("t-openai-e2e".to_string());
    }

    /// 真 TCP 端到端(透传): 假 Anthropic 上游 ← 本地路由 ← 原生 socket 客户端。
    /// 验证: 路由总开关下 Anthropic 兼容供应商按 /p/{id} 透传 —— 路径拼接(base 带子路径)、
    /// Bearer key 实时注入、anthropic-version 透传、SSE 字节原样中继。
    #[test]
    fn e2e_anthropic_passthrough_over_real_tcp() {
        use std::io::{Read as _, Write as _};
        use std::sync::{Arc, Mutex};

        let upstream = TcpListener::bind("127.0.0.1:0").expect("bind 假上游");
        let up_port = upstream.local_addr().unwrap().port();
        let captured: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let cap2 = captured.clone();
        std::thread::spawn(move || {
            for s in upstream.incoming().flatten() {
                let mut s = s;
                let mut buf = [0u8; 65536];
                let mut head: Vec<u8> = Vec::new();
                loop {
                    let n = s.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    head.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&head).to_string();
                    if let Some(pos) = text.find("\r\n\r\n") {
                        let cl = text
                            .lines()
                            .find_map(|l| {
                                let low = l.to_ascii_lowercase();
                                low.strip_prefix("content-length:")
                                    .map(|v| v.trim().to_string())
                            })
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(0);
                        if head.len() >= pos + 4 + cl {
                            break;
                        }
                    }
                }
                *cap2.lock().unwrap() = String::from_utf8_lossy(&head).to_string();
                // 真 Anthropic SSE 片段 —— 透传应逐字节原样到达客户端
                let body = concat!(
                    "event: message_start\ndata: {\"type\":\"message_start\"}\n\n",
                    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"透传OK\"}}\n\n",
                    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
                );
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = s.write_all(resp.as_bytes());
            }
        });

        // Anthropic 兼容供应商(base 带子路径 /anthropic, 验证透传路径拼接)
        crate::provider::provider_save(crate::provider::ProviderInput {
            id: Some("t-anth-e2e".into()),
            name: "透传e2e".into(),
            note: String::new(),
            website_url: String::new(),
            token_field: None,
            protocol: None,
            settings_config: json!({"env": {
                "ANTHROPIC_BASE_URL": format!("http://127.0.0.1:{up_port}/anthropic"),
                "ANTHROPIC_AUTH_TOKEN": "sk-anth-e2e",
            }}),
        })
        .expect("注册供应商");

        let port = ensure_running().expect("本地路由应能启动");
        let req_body = r#"{"model":"m","stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
        let req = format!(
            "POST /p/t-anth-e2e/v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nanthropic-version: 2023-06-01\r\nanthropic-beta: fine-grained-tool-streaming-2025-05-14\r\nContent-Length: {}\r\n\r\n{}",
            req_body.len(),
            req_body
        );
        let mut out = String::new();
        let mut read_err = None;
        for attempt in 0u64..3 {
            let mut c = TcpStream::connect(("127.0.0.1", port)).expect("连本地路由");
            c.write_all(req.as_bytes()).unwrap();
            out.clear();
            read_err = c.read_to_string(&mut out).err();
            if read_err.is_none() && out.contains("message_stop") {
                break;
            }
            std::thread::sleep(Duration::from_millis(120 * (attempt + 1)));
        }

        assert!(
            out.contains("event: message_start"),
            "SSE 应原样中继; 读到=[{out}] read_err={read_err:?} last_error={}",
            last_error()
        );
        assert!(out.contains("透传OK"), "正文应逐字节原样: {out}");
        assert!(out.contains("event: message_stop"), "收口事件应在: {out}");
        let head = captured.lock().unwrap().clone();
        assert!(
            head.starts_with("POST /anthropic/v1/messages"),
            "base 子路径应拼上: {head}"
        );
        assert!(
            head.contains("Bearer sk-anth-e2e"),
            "AUTH_TOKEN 档应注入 Bearer: {head}"
        );
        assert!(
            head.to_ascii_lowercase().contains("anthropic-version: 2023-06-01"),
            "协议头应透传: {head}"
        );
        assert!(
            head.to_ascii_lowercase()
                .contains("anthropic-beta: fine-grained-tool-streaming-2025-05-14"),
            "beta 头应透传: {head}"
        );

        let _ = crate::provider::provider_delete("t-anth-e2e".to_string());
    }

    #[test]
    fn build_responses_body_tolerates_malformed_messages() {
        // content/结构各种畸形: 只要求不 panic (健壮性回归保护)
        let weird = json!({
            "model": "gpt-5",
            "messages": [
                {"role":"user","content": 12345},
                {"role":"assistant"},
                {"content": [{"type":"text"}]},
                {"role":"user","content":[{"type":"image","source":{}}]},
                {"role":"user","content":[{"type":"tool_use"}]},
                {"role":"user","content":[{"type":"tool_result"}]},
                "not-an-object",
                42
            ]
        });
        let _ = build_responses_body(&weird);
    }
}
