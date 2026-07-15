use super::*;

// ───────────────────────── Commands: Claude 官方订阅授权 (PKCE Authorization Code) ─────────────────────────
//
// Claude 官方订阅登录, 桌面端首选「回环一键授权」—— 与官方 claude CLI 原生 /login 同款
// (localhost:54545/callback): 点授权 → 浏览器登录并点 Authorize → code 自动重定向回本机
// → 后端换 token 落盘, 用户零复制零回贴。54545 被占或 server flavor 降级手工回贴
// (与 `claude setup-token` 同源: 授权页给 `code#state`, 贴回 Polaris 换 token)。
// 两条路都按官方 `~/.claude/.credentials.json` 的 `claudeAiOauth` 结构落盘, 外部
// `claude` CLI 与 Polaris 自起的 claude 直接复用, 无需在外壳里再登录一次。
//
// 注意: 授权 URL 里 `code=true` 是「手工回贴」模式的开关(授权页显示授权码而非重定向),
// 回环模式**不带**它, 且 redirect_uri 换成 localhost。

const CLAUDE_OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CLAUDE_OAUTH_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const CLAUDE_OAUTH_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const CLAUDE_OAUTH_REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const CLAUDE_OAUTH_SCOPES: &str = "org:create_api_key user:profile user:inference";
// 回环一键授权 (与官方 claude CLI 原生 login 同款端口/路径)
const CLAUDE_LOOPBACK_PORT: u16 = 54545;
const CLAUDE_LOOPBACK_REDIRECT_URI: &str = "http://localhost:54545/callback";

fn claude_credentials_path() -> Option<PathBuf> {
    claude_dir().map(|d| d.join(".credentials.json"))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeAuthStatus {
    pub logged_in: bool,
    pub cred_path: String,
}

/// .credentials.json 存在且带非空 claudeAiOauth.accessToken
fn claude_creds_has_token(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| {
            v.get("claudeAiOauth")
                .and_then(|o| o.get("accessToken"))
                .and_then(|a| a.as_str())
                .map(|s| !s.is_empty())
        })
        .unwrap_or(false)
}

/// 是否已登录 Claude 官方订阅
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn claude_oauth_status() -> Result<ClaudeAuthStatus, String> {
    let path = claude_credentials_path();
    let logged_in = path
        .as_ref()
        .map(|p| claude_creds_has_token(p))
        .unwrap_or(false);
    Ok(ClaudeAuthStatus {
        logged_in,
        cred_path: path
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
    })
}

/// `claude_start_login` 返回给前端的授权信息
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeLoginStart {
    /// auto = 回环一键授权(前端轮询 claude_login_poll); manual = 手工回贴授权码
    pub mode: String,
    /// 授权页 URL(已自动打开, UI 也展示便于手动打开)
    pub authorize_url: String,
    /// PKCE code_verifier, manual 模式回贴换 token 时原样带回
    pub verifier: String,
    /// 防串话 state, manual 模式回贴换 token 时原样带回 (授权码尾部 #state 须与之一致)
    pub state: String,
}

/// ① 生成 PKCE(S256)+ state, 拼授权 URL 并打开浏览器。
/// 桌面端优先回环一键授权; `force_manual` / 54545 被占 / server flavor → 手工回贴。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn claude_start_login(force_manual: Option<bool>) -> Result<ClaudeLoginStart, String> {
    let verifier = claude_rand_b64url(32)?;
    let state = claude_rand_b64url(32)?;
    let challenge = claude_b64url_encode(&claude_sha256(verifier.as_bytes()));

    if cfg!(feature = "desktop") && !force_manual.unwrap_or(false) {
        if let Some(listener) = loopback_take_port(&CLAUDE_LOOPBACK, CLAUDE_LOOPBACK_PORT) {
            let authorize_url = format!(
                "{base}?client_id={cid}&response_type=code&redirect_uri={redir}&scope={scope}&code_challenge={chal}&code_challenge_method=S256&state={state}",
                base = CLAUDE_OAUTH_AUTHORIZE_URL,
                cid = CLAUDE_OAUTH_CLIENT_ID,
                redir = claude_url_encode(CLAUDE_LOOPBACK_REDIRECT_URI),
                scope = claude_url_encode(CLAUDE_OAUTH_SCOPES),
                chal = challenge,
                state = state,
            );
            let session = LoopbackSession::new();
            *CLAUDE_LOOPBACK.lock() = Some(session.clone());
            {
                let (state, verifier) = (state.clone(), verifier.clone());
                thread::spawn(move || {
                    let r = loopback_run(
                        listener,
                        "/callback",
                        &state,
                        "Claude 官方订阅",
                        &session,
                        Duration::from_secs(LOOPBACK_TIMEOUT_SECS),
                        |code| {
                            claude_exchange_and_store(
                                code,
                                &state,
                                &verifier,
                                CLAUDE_LOOPBACK_REDIRECT_URI,
                            )
                        },
                    );
                    match r {
                        Ok(()) => session.set("ok", ""),
                        Err(e) => session.set("failed", &e),
                    }
                });
            }
            let _ = codex_open_browser(&authorize_url);
            return Ok(ClaudeLoginStart {
                mode: "auto".into(),
                authorize_url,
                verifier,
                state,
            });
        }
    }

    // 手工回贴兜底 (code=true → 授权页显示授权码供复制)
    let authorize_url = format!(
        "{base}?code=true&client_id={cid}&response_type=code&redirect_uri={redir}&scope={scope}&code_challenge={chal}&code_challenge_method=S256&state={state}",
        base = CLAUDE_OAUTH_AUTHORIZE_URL,
        cid = CLAUDE_OAUTH_CLIENT_ID,
        redir = claude_url_encode(CLAUDE_OAUTH_REDIRECT_URI),
        scope = claude_url_encode(CLAUDE_OAUTH_SCOPES),
        chal = challenge,
        state = state,
    );
    // 自动拉起浏览器到授权页 (失败不致命, UI 仍展示链接供手动打开)
    let _ = codex_open_browser(&authorize_url);
    Ok(ClaudeLoginStart {
        mode: "manual".into(),
        authorize_url,
        verifier,
        state,
    })
}

/// 回环一键授权进度 (auto 模式前端轮询用)
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn claude_login_poll() -> Result<LoginPollResult, String> {
    Ok(loopback_poll(&CLAUDE_LOOPBACK))
}

/// 取消进行中的回环授权 (关卡片/改手工时释放 54545 端口)
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn claude_login_cancel() -> Result<(), String> {
    loopback_stop(&CLAUDE_LOOPBACK);
    Ok(())
}

#[derive(Deserialize)]
struct ClaudeTokenResp {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
}

/// ② 用户回贴的授权码(授权页给的是 `code#state`)+ verifier/state 换 token 落盘
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn claude_finish_login(
    pasted: String,
    verifier: String,
    state: String,
) -> Result<ClaudeAuthStatus, String> {
    let pasted = pasted.trim();
    if pasted.is_empty() {
        return Err("请先粘贴授权码".into());
    }
    // 授权页给的是 `code#state`:拆出 code,并核对尾部 state 防串话 / 防贴错
    let mut parts = pasted.splitn(2, '#');
    let code = parts.next().unwrap_or("").trim().to_string();
    let returned_state = parts.next().map(|s| s.trim().to_string());
    if let Some(rs) = returned_state.as_ref() {
        if !rs.is_empty() && rs != &state {
            return Err("授权码与本次请求不匹配(state 不一致),请重新发起授权".into());
        }
    }
    if code.is_empty() {
        return Err("授权码为空".into());
    }

    claude_exchange_and_store(&code, &state, &verifier, CLAUDE_OAUTH_REDIRECT_URI)?;
    claude_oauth_status()
}

/// code(+state/verifier) 换 token 并按官方结构落盘 —— 回环回调与手工回贴共用。
/// redirect_uri 须与授权时一致 (回环 = localhost, 手工 = console 展示页)。
fn claude_exchange_and_store(
    code: &str,
    state: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<(), String> {
    let resp = codex_agent()
        .post(CLAUDE_OAUTH_TOKEN_URL)
        .set("Content-Type", "application/json")
        .set("User-Agent", CODEX_USER_AGENT)
        .send_json(json!({
            "grant_type": "authorization_code",
            "code": code,
            "state": state,
            "client_id": CLAUDE_OAUTH_CLIENT_ID,
            "redirect_uri": redirect_uri,
            "code_verifier": verifier,
        }))
        .map_err(|e| format!("换取 Claude Token 失败: {}", codex_http_err(e)))?;

    let tokens: ClaudeTokenResp = resp
        .into_json()
        .map_err(|e| format!("解析 Token 响应失败: {e}"))?;
    let refresh = tokens.refresh_token.clone().unwrap_or_default();
    let expires_at = claude_expires_at_ms(tokens.expires_in.unwrap_or(0));
    let scope_str = tokens.scope.as_deref().unwrap_or(CLAUDE_OAUTH_SCOPES);
    let scopes: Vec<&str> = scope_str.split_whitespace().collect();

    claude_write_credentials(&tokens.access_token, &refresh, expires_at, &scopes)
}

/// 按官方 `~/.claude/.credentials.json` 的 claudeAiOauth 结构写;合并保留文件里已有的其它键。
fn claude_write_credentials(
    access: &str,
    refresh: &str,
    expires_at: u64,
    scopes: &[&str],
) -> Result<(), String> {
    let path = claude_credentials_path().ok_or_else(|| "无法定位 ~/.claude".to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建 ~/.claude 目录失败: {e}"))?;
    }
    // 保留已有其它键(如 codeWorkspaceTrust 等),只覆盖 claudeAiOauth 块
    let mut root: Value = fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}));
    if !root.is_object() {
        root = json!({});
    }
    root["claudeAiOauth"] = json!({
        "accessToken": access,
        "refreshToken": refresh,
        "expiresAt": expires_at,
        "scopes": scopes,
    });
    let content =
        serde_json::to_string_pretty(&root).map_err(|e| format!("序列化凭据失败: {e}"))?;
    // 原子写防撕裂(外部 claude CLI 并发读不会读到坏 JSON);Unix 收紧 0600 不让同机他人读走凭证。
    atomic_write(&path, &content)
        .map_err(|e| format!("写入 ~/.claude/.credentials.json 失败: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// 当前毫秒时间戳 + expires_in(秒)→ claudeAiOauth.expiresAt(毫秒)
fn claude_expires_at_ms(expires_in_secs: u64) -> u64 {
    now_ms() + expires_in_secs.saturating_mul(1000)
}

/// 加密安全随机 n 字节 → base64url(无填充)。verifier/state 必须不可预测。
pub(crate) fn claude_rand_b64url(n: usize) -> Result<String, String> {
    let mut buf = vec![0u8; n];
    getrandom::getrandom(&mut buf).map_err(|e| format!("生成安全随机数失败: {e}"))?;
    Ok(claude_b64url_encode(&buf))
}

/// SHA-256(PKCE S256 的 code_challenge 用)
pub(crate) fn claude_sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    let out = h.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

/// base64url 编码(无填充)—— 与 codex_b64url_decode 对偶
pub(crate) fn claude_b64url_encode(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(T[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(T[(n & 63) as usize] as char);
        }
    }
    out
}

/// 极简 percent-encoding:只放行 RFC3986 unreserved,其余按 %XX 编码(够 query 值用)
pub(crate) fn claude_url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
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

// ───────────────────────── 单测: Claude PKCE 密码学正确性 ─────────────────────────
#[cfg(test)]
mod claude_oauth_tests {
    use super::*;

    /// base64url(无填充)对已知向量正确,且与 codex_b64url_decode 互逆
    #[test]
    fn b64url_encode_known_vectors() {
        assert_eq!(claude_b64url_encode(b""), "");
        assert_eq!(claude_b64url_encode(b"f"), "Zg");
        assert_eq!(claude_b64url_encode(b"fo"), "Zm8");
        assert_eq!(claude_b64url_encode(b"foo"), "Zm9v");
        assert_eq!(claude_b64url_encode(b"foob"), "Zm9vYg");
        assert_eq!(claude_b64url_encode(b"fooba"), "Zm9vYmE");
        assert_eq!(claude_b64url_encode(b"foobar"), "Zm9vYmFy");
        // 含会被标准 base64 编成 '+' '/' 的字节,base64url 必须出 '-' '_'
        let enc = claude_b64url_encode(&[0xfb, 0xff, 0xbf]);
        assert!(!enc.contains('+') && !enc.contains('/') && !enc.contains('='));
        // 编码→解码 round-trip
        let raw: Vec<u8> = (0u8..=255).collect();
        let back = codex_b64url_decode(&claude_b64url_encode(&raw)).unwrap();
        assert_eq!(raw, back);
    }

    /// SHA-256 对 "abc" 的标准向量(NIST FIPS 180-4)
    #[test]
    fn sha256_known_vector() {
        let d = claude_sha256(b"abc");
        let hex: String = d.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// PKCE S256 端到端:RFC 7636 附录 B 的官方测试向量
    /// verifier "dBjft...60M" → challenge "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    #[test]
    fn pkce_s256_rfc7636_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = claude_b64url_encode(&claude_sha256(verifier.as_bytes()));
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    /// 随机 verifier/state 不可预测且长度足够(32 字节 → 43 字符 base64url)
    #[test]
    fn rand_b64url_len_and_uniqueness() {
        let a = claude_rand_b64url(32).unwrap();
        let b = claude_rand_b64url(32).unwrap();
        assert_eq!(a.len(), 43);
        assert_ne!(a, b);
        assert!(!a.contains('=') && !a.contains('+') && !a.contains('/'));
    }

    /// query 值百分号编码:空格不留原样,unreserved 不动
    #[test]
    fn url_encode_query_value() {
        assert_eq!(
            claude_url_encode("org:create_api_key user:profile"),
            "org%3Acreate_api_key%20user%3Aprofile"
        );
        assert_eq!(claude_url_encode("aZ09-_.~"), "aZ09-_.~");
    }

    /// 回环回调 query 解析: 取参 + percent 解码 + '+' 还原空格; 缺参给 None
    #[test]
    fn loopback_query_param_decode() {
        let q = "code=ac_1%2Babc%3D%3D&state=st-42&error_description=Access+denied%21";
        assert_eq!(
            loopback_query_param(q, "code").as_deref(),
            Some("ac_1+abc==")
        );
        assert_eq!(loopback_query_param(q, "state").as_deref(), Some("st-42"));
        assert_eq!(
            loopback_query_param(q, "error_description").as_deref(),
            Some("Access denied!")
        );
        assert_eq!(loopback_query_param(q, "missing"), None);
        // 裸键(无 =)与坏 percent 序列不 panic
        assert_eq!(
            loopback_query_param("flag&x=1", "flag").as_deref(),
            Some("")
        );
        assert_eq!(loopback_pct_decode("%zz%"), "%zz%");
    }

    /// 回环授权 URL 构造纪律: 回环模式不带 code=true(那是手工回贴开关),
    /// redirect_uri 是 localhost; 手工模式带 code=true 且指向 console 展示页
    #[test]
    fn loopback_vs_manual_authorize_url_shape() {
        let redir_loop = claude_url_encode(CLAUDE_LOOPBACK_REDIRECT_URI);
        assert_eq!(redir_loop, "http%3A%2F%2Flocalhost%3A54545%2Fcallback");
        let redir_manual = claude_url_encode(CLAUDE_OAUTH_REDIRECT_URI);
        assert!(redir_manual.starts_with("https%3A%2F%2Fconsole.anthropic.com"));
    }

    /// 回环监听本机端到端: favicon 杂请求 404 不终止会话 → 真回调(percent 编码的
    /// code + 正确 state)→ finish 收到解码后的 code, 浏览器拿到成功落地页
    #[test]
    fn loopback_run_end_to_end_local() {
        use std::io::{Read, Write};
        use std::net::TcpStream;

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let session = LoopbackSession::new();
        let sess = session.clone();
        let handle = thread::spawn(move || {
            let mut got = String::new();
            let r = loopback_run(
                listener,
                "/callback",
                "st-1",
                "测试",
                &sess,
                Duration::from_secs(10),
                |code| {
                    got = code.to_string();
                    Ok(())
                },
            );
            (r, got)
        });

        // 杂请求(favicon): 404, 会话继续等
        {
            let mut s = TcpStream::connect(addr).unwrap();
            s.write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: x\r\n\r\n")
                .unwrap();
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            assert!(buf.starts_with("HTTP/1.1 404"), "杂请求应 404: {buf}");
        }
        // 真回调: code 带 percent 编码, state 正确
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"GET /callback?code=ac%2D9&state=st-1 HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut buf = String::new();
        let _ = s.read_to_string(&mut buf);
        assert!(buf.starts_with("HTTP/1.1 200"), "回调应 200: {buf}");
        assert!(buf.contains("授权成功"), "应回成功落地页: {buf}");

        let (r, code) = handle.join().unwrap();
        assert!(r.is_ok(), "会话应成功结束: {r:?}");
        assert_eq!(code, "ac-9", "finish 应拿到 percent 解码后的 code");
    }

    /// state 不符(伪造回调)必须拒绝: 会话判败, 浏览器拿到失败页
    #[test]
    fn loopback_run_rejects_wrong_state() {
        use std::io::{Read, Write};
        use std::net::TcpStream;

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let session = LoopbackSession::new();
        let sess = session.clone();
        let handle = thread::spawn(move || {
            loopback_run(
                listener,
                "/callback",
                "st-expected",
                "测试",
                &sess,
                Duration::from_secs(10),
                |_| panic!("state 不符时绝不能进入换 token"),
            )
        });
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"GET /callback?code=x&state=st-forged HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut buf = String::new();
        let _ = s.read_to_string(&mut buf);
        assert!(buf.contains("授权未完成"), "应回失败落地页: {buf}");
        assert!(handle.join().unwrap().is_err(), "state 不符应判败");
    }
}
