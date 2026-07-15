use super::*;

// ───────────────────────── OAuth 回环回调 (一键授权公共设施) ─────────────────────────
//
// 桌面端把 Claude / Codex 两条授权流升级成与官方 CLI 原生 login 同款的「本地回环回调」:
// 后端在 127.0.0.1 起一次性 HTTP 监听 → 浏览器点 Authorize → 授权服务器把 code 重定向
// 回 localhost → 后端自动换 token 落盘。用户全程只点一次授权, 零复制零回贴。
// 端口被占或 server flavor(浏览器在远端, 回环打不回来)自动降级旧流程:
// Claude → 手工回贴授权码, Codex → Device Code。

/// 回环授权整体时限 (秒): 浏览器里迟迟不点授权, 超时自动收监听释放端口
pub(crate) const LOOPBACK_TIMEOUT_SECS: u64 = 600;

/// 一次回环授权会话: 监听线程写状态, poll 命令读; cancel 置位后监听线程 ≤200ms 自退释放端口。
pub(crate) struct LoopbackSession {
    /// (status, message): pending | ok | failed
    status: parking_lot::Mutex<(String, String)>,
    cancel: AtomicBool,
}

impl LoopbackSession {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            status: parking_lot::Mutex::new(("pending".into(), String::new())),
            cancel: AtomicBool::new(false),
        })
    }
    pub(crate) fn set(&self, status: &str, message: &str) {
        *self.status.lock() = (status.into(), message.into());
    }
}

pub(crate) static CLAUDE_LOOPBACK: Lazy<parking_lot::Mutex<Option<Arc<LoopbackSession>>>> =
    Lazy::new(|| parking_lot::Mutex::new(None));
pub(crate) static CODEX_LOOPBACK: Lazy<parking_lot::Mutex<Option<Arc<LoopbackSession>>>> =
    Lazy::new(|| parking_lot::Mutex::new(None));

/// 轮询结果 (claude_login_poll / codex_login_poll 共用)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginPollResult {
    /// idle(无进行中会话, 如应用重启过) | pending | ok | failed
    pub status: String,
    pub message: String,
}

pub(crate) fn loopback_poll(
    slot: &parking_lot::Mutex<Option<Arc<LoopbackSession>>>,
) -> LoginPollResult {
    match slot.lock().as_ref() {
        None => LoginPollResult {
            status: "idle".into(),
            message: String::new(),
        },
        Some(s) => {
            let (status, message) = s.status.lock().clone();
            LoginPollResult { status, message }
        }
    }
}

/// 叫停并丢弃当前会话 (监听线程见 cancel 置位后自退, 端口随之释放)
pub(crate) fn loopback_stop(slot: &parking_lot::Mutex<Option<Arc<LoopbackSession>>>) {
    if let Some(old) = slot.lock().take() {
        old.cancel.store(true, Ordering::SeqCst);
    }
}

/// 抢回环端口: 先叫停旧会话再绑定, 短暂重试等旧监听线程退出释放端口。
/// 仍绑不上(被外部程序占, 如正在 login 的官方 CLI)→ None, 调用方降级旧流程。
pub(crate) fn loopback_take_port(
    slot: &parking_lot::Mutex<Option<Arc<LoopbackSession>>>,
    port: u16,
) -> Option<TcpListener> {
    loopback_stop(slot);
    for _ in 0..8 {
        if let Ok(l) = TcpListener::bind(("127.0.0.1", port)) {
            return Some(l);
        }
        thread::sleep(Duration::from_millis(120));
    }
    None
}

/// 极简 HTTP 应答 (一次性连接, 写完即半关)
fn loopback_respond(stream: &mut TcpStream, status_line: &str, content_type: &str, body: &str) {
    let resp = format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        len = body.len(),
    );
    let _ = std::io::Write::write_all(stream, resp.as_bytes());
    let _ = stream.shutdown(std::net::Shutdown::Write);
}

/// 授权结果落地页 (浏览器里给用户看的最后一页)
fn loopback_page(ok: bool, brand: &str, detail: &str) -> String {
    let (icon, title, tone) = if ok {
        ("&#10003;", "授权成功", "#34c08b")
    } else {
        ("&#10005;", "授权未完成", "#e5673f")
    };
    format!(
        r#"<!doctype html><html lang="zh"><head><meta charset="utf-8"><title>{title} · Polaris</title>
<meta name="viewport" content="width=device-width,initial-scale=1">
<style>
  body{{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;
    background:#101418;color:#e8edf2;font:15px/1.7 -apple-system,"Segoe UI","Microsoft YaHei",sans-serif}}
  .card{{text-align:center;padding:44px 52px;border:1px solid #ffffff1f;border-radius:16px;
    background:#ffffff0a;box-shadow:0 18px 60px #00000055;max-width:420px}}
  .icon{{width:52px;height:52px;line-height:52px;margin:0 auto 14px;border-radius:50%;
    font-size:24px;color:#fff;background:{tone}}}
  h1{{font-size:19px;margin:0 0 6px}} p{{margin:0;color:#9aa7b3;font-size:13.5px;word-break:break-all}}
</style></head><body><div class="card">
  <div class="icon">{icon}</div><h1>{title}</h1>
  <p>{brand}{sep}{detail}</p><p style="margin-top:10px">可以关闭此页, 回到 Polaris。</p>
</div></body></html>"#,
        sep = if detail.is_empty() { "" } else { " · " },
    )
}

/// 从 query string 取参数并做 percent 解码 ('+' 视作空格)
pub(crate) fn loopback_query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == key {
            return Some(loopback_pct_decode(v));
        }
    }
    None
}

pub(crate) fn loopback_pct_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = |c: u8| (c as char).to_digit(16);
                if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 回环授权主循环: 等浏览器带 code 回调 → 校验 state 防串话 → 执行 finish(换 token 落盘)
/// → 按真实结果回落地页。只认 want_path; favicon 等杂请求回 404 继续等; error= 直接判败。
pub(crate) fn loopback_run(
    listener: TcpListener,
    want_path: &str,
    expected_state: &str,
    brand: &str,
    session: &LoopbackSession,
    timeout: Duration,
    finish: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), String> {
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("回环监听设置失败: {e}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        if session.cancel.load(Ordering::SeqCst) {
            return Err("授权已取消".into());
        }
        if Instant::now() > deadline {
            return Err("等待浏览器授权超时, 请重新发起".into());
        }
        let mut stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(150));
                continue;
            }
            Err(e) => return Err(format!("回环监听故障: {e}")),
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

        // 只需请求行 (`GET /path?query HTTP/1.1`); 读到首个换行即可, 16KB 上限防灌爆
        let mut buf: Vec<u8> = Vec::with_capacity(1024);
        let mut chunk = [0u8; 1024];
        while !buf.contains(&b'\n') && buf.len() < 16 * 1024 {
            match std::io::Read::read(&mut stream, &mut chunk) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(_) => break,
            }
        }
        let line = String::from_utf8_lossy(&buf);
        let target = line
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("")
            .to_string();
        let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));

        if path != want_path {
            loopback_respond(
                &mut stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                "not found",
            );
            continue;
        }
        if let Some(err) = loopback_query_param(query, "error") {
            let detail = loopback_query_param(query, "error_description").unwrap_or(err);
            loopback_respond(
                &mut stream,
                "200 OK",
                "text/html; charset=utf-8",
                &loopback_page(false, brand, &detail),
            );
            return Err(format!("授权被拒绝: {detail}"));
        }
        let code = loopback_query_param(query, "code").unwrap_or_default();
        let state_ok = loopback_query_param(query, "state")
            .map(|s| s == expected_state)
            .unwrap_or(false);
        if code.is_empty() || !state_ok {
            loopback_respond(
                &mut stream,
                "200 OK",
                "text/html; charset=utf-8",
                &loopback_page(
                    false,
                    brand,
                    "回调参数不完整或不属于本次授权, 请回 Polaris 重新发起",
                ),
            );
            return Err("回调缺少授权码或 state 不一致, 请重新发起授权".into());
        }

        // 换 token 落盘完成后再回落地页, 页面结果与真实结果一致
        return match finish(&code) {
            Ok(()) => {
                loopback_respond(
                    &mut stream,
                    "200 OK",
                    "text/html; charset=utf-8",
                    &loopback_page(true, brand, "凭据已写入本机"),
                );
                Ok(())
            }
            Err(e) => {
                loopback_respond(
                    &mut stream,
                    "200 OK",
                    "text/html; charset=utf-8",
                    &loopback_page(false, brand, &e),
                );
                Err(e)
            }
        };
    }
}
