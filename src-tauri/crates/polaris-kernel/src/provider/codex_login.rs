use super::*;

// ───────────────────────── Commands: Codex 授权 (回环一键 + Device Code 兜底) ─────────────────────────
//
// 桌面端首选「授权码 + PKCE 回环回调」—— 与官方 codex CLI 原生 login 同款
// (localhost:1455/auth/callback): 点授权 → 浏览器登录并点 Authorize → code 自动
// 重定向回本机 → 后端换 token 落盘, 用户零核对零回贴。
// 1455 被占(多半是外部 codex CLI 正在 login)或 server flavor(浏览器在远端,
// 回环打不回来)时降级 Device Code 流程(抄自 cc-switch `codex_oauth_auth.rs`,
// 但**不背它的翻译代理**)。两条路拿到的 token 都按官方 codex CLI 的
// `~/.codex/auth.json` 格式落盘, 外部 `codex` CLI 直接复用, 不依赖其是否已装。

/// OpenAI OAuth 客户端 ID (与官方 Codex CLI 相同)
pub(crate) const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_DEVICE_USERCODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const CODEX_DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
pub(crate) const CODEX_OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_DEVICE_VERIFY_URL: &str = "https://auth.openai.com/codex/device";
/// Device Code 流程约定的 redirect_uri (OpenAI 服务端固定)
const CODEX_DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
pub(crate) const CODEX_USER_AGENT: &str = "polaris-codex-oauth";
// 回环一键授权 (与官方 codex CLI login 同款参数, 见其 codex-rs/login/src/server.rs)
const CODEX_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const CODEX_LOOPBACK_PORT: u16 = 1455;
const CODEX_LOOPBACK_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const CODEX_LOOPBACK_SCOPES: &str = "openid profile email offline_access";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexStatus {
    pub installed: bool,
    pub logged_in: bool,
    pub auth_path: String,
}

pub(crate) fn codex_auth_path() -> Option<PathBuf> {
    UserDirs::new().map(|u| u.home_dir().join(".codex").join("auth.json"))
}

/// Codex 安装/登录状态。桌面端 async + spawn_blocking:要 spawn `codex --version`
/// 子进程 + 读 auth.json,首帧就会被调到,同步跑在主线程会挤占首屏 IPC。
/// server flavor dispatch 本就在 spawn_blocking 中,保持同步直调。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn codex_status() -> Result<CodexStatus, String> {
    tauri::async_runtime::spawn_blocking(codex_status_sync)
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
pub fn codex_status() -> Result<CodexStatus, String> {
    codex_status_sync()
}

fn codex_status_sync() -> Result<CodexStatus, String> {
    let installed = Command::new("codex")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let auth_path = codex_auth_path();
    // 授权与否只看 ~/.codex/auth.json 是否有 ChatGPT tokens —— 与 codex CLI 是否已装解耦。
    let logged_in = auth_path
        .as_ref()
        .map(|p| codex_auth_has_tokens(p))
        .unwrap_or(false);
    Ok(CodexStatus {
        installed,
        logged_in,
        auth_path: auth_path
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
    })
}

/// auth.json 存在且带 ChatGPT OAuth tokens (区别于纯 API key 登录)
pub(crate) fn codex_auth_has_tokens(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| {
            v.get("tokens")
                .and_then(|t| t.get("access_token"))
                .and_then(|a| a.as_str())
                .map(|s| !s.is_empty())
        })
        .unwrap_or(false)
}

/// `codex_start_login` 返回给前端的授权信息
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexDeviceLogin {
    /// auto = 回环一键授权(device/user_code 为空, 前端轮询 codex_login_poll);
    /// device = 设备码流程(前端轮询 codex_poll_login)
    pub mode: String,
    /// device_auth_id, 轮询时回传 (device 模式)
    pub device_code: String,
    /// 展示给用户的配对码 (device 模式)
    pub user_code: String,
    /// 浏览器授权/验证页 (已自动打开, UI 也显示便于手动打开)
    pub verification_uri: String,
    /// 建议轮询间隔 (秒)
    pub interval: u64,
    /// 本次授权有效期 (秒)
    pub expires_in: u64,
}

/// `codex_poll_login` 返回: status = "pending" | "ok"
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexPollResult {
    pub status: String,
}

#[derive(Deserialize)]
struct CodexDeviceCodeResp {
    device_auth_id: String,
    user_code: String,
    #[serde(default)]
    interval: Option<Value>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct CodexDevicePollSuccess {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Deserialize)]
struct CodexTokenResp {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

/// 提取 ureq 错误里的状态码/文案, 拼成可读消息
pub(crate) fn codex_http_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let body = body.chars().take(300).collect::<String>();
            format!("HTTP {code} - {body}")
        }
        ureq::Error::Transport(t) => format!("网络错误: {t}"),
    }
}

/// 解析 interval 字段 (服务端可能给数字或字符串), 加 3 秒安全余量
fn codex_parse_interval(v: Option<&Value>) -> u64 {
    let raw = match v {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(5),
        Some(Value::String(s)) => s.parse::<u64>().unwrap_or(5),
        _ => 5,
    };
    raw.max(1) + 3
}

/// 带超时的 OAuth agent: 设备授权 / 轮询 / 换 token 都是非流式请求-响应, 给整条 call 30s
/// 全局 deadline, 防 OpenAI 认证端点黑洞把 Tauri 命令线程挂死 (轮询命令更会每次挂一条)。
pub(crate) fn codex_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .build()
}

/// ① 发起授权: 桌面端首选回环一键授权(浏览器点 Authorize 即完成), 兜底 Device Code
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn codex_start_login() -> Result<CodexDeviceLogin, String> {
    // 回环一键: server flavor 浏览器在远端打不回本机, 只在桌面端启用
    if cfg!(feature = "desktop") {
        if let Some(listener) = loopback_take_port(&CODEX_LOOPBACK, CODEX_LOOPBACK_PORT) {
            let verifier = claude_rand_b64url(48)?;
            let state = claude_rand_b64url(32)?;
            let challenge = claude_b64url_encode(&claude_sha256(verifier.as_bytes()));
            let authorize_url = format!(
                "{base}?response_type=code&client_id={cid}&redirect_uri={redir}&scope={scope}&code_challenge={chal}&code_challenge_method=S256&id_token_add_organizations=true&codex_cli_simplified_flow=true&state={state}&originator=codex_cli_rs",
                base = CODEX_AUTHORIZE_URL,
                cid = CODEX_CLIENT_ID,
                redir = claude_url_encode(CODEX_LOOPBACK_REDIRECT_URI),
                scope = claude_url_encode(CODEX_LOOPBACK_SCOPES),
                chal = challenge,
                state = state,
            );
            let session = LoopbackSession::new();
            *CODEX_LOOPBACK.lock() = Some(session.clone());
            thread::spawn(move || {
                let r = loopback_run(
                    listener,
                    "/auth/callback",
                    &state,
                    "ChatGPT (Codex)",
                    &session,
                    Duration::from_secs(LOOPBACK_TIMEOUT_SECS),
                    |code| {
                        let tokens =
                            codex_exchange_code(code, &verifier, CODEX_LOOPBACK_REDIRECT_URI)?;
                        let refresh = tokens
                            .refresh_token
                            .clone()
                            .ok_or_else(|| "授权响应缺少 refresh_token".to_string())?;
                        let account_id = codex_account_id(&tokens);
                        codex_write_auth_json(&tokens, &refresh, account_id.as_deref())
                    },
                );
                match r {
                    Ok(()) => session.set("ok", ""),
                    Err(e) => session.set("failed", &e),
                }
            });
            let _ = codex_open_browser(&authorize_url);
            return Ok(CodexDeviceLogin {
                mode: "auto".into(),
                device_code: String::new(),
                user_code: String::new(),
                verification_uri: authorize_url,
                interval: 2,
                expires_in: LOOPBACK_TIMEOUT_SECS,
            });
        }
    }

    // Device Code 兜底 (1455 被占 / server flavor)
    let resp = codex_agent()
        .post(CODEX_DEVICE_USERCODE_URL)
        .set("Content-Type", "application/json")
        .set("User-Agent", CODEX_USER_AGENT)
        .send_json(json!({ "client_id": CODEX_CLIENT_ID }))
        .map_err(|e| format!("发起 ChatGPT 设备授权失败: {}", codex_http_err(e)))?;

    let device: CodexDeviceCodeResp = resp
        .into_json()
        .map_err(|e| format!("解析设备码响应失败: {e}"))?;

    let interval = codex_parse_interval(device.interval.as_ref());
    let expires_in = device.expires_in.unwrap_or(900);

    // 自动拉起浏览器到验证页 (失败不致命, UI 仍展示链接 + 配对码供手动打开)
    let _ = codex_open_browser(CODEX_DEVICE_VERIFY_URL);

    Ok(CodexDeviceLogin {
        mode: "device".into(),
        device_code: device.device_auth_id,
        user_code: device.user_code,
        verification_uri: CODEX_DEVICE_VERIFY_URL.to_string(),
        interval,
        expires_in,
    })
}

/// 回环一键授权进度 (auto 模式前端轮询用)
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn codex_login_poll() -> Result<LoginPollResult, String> {
    Ok(loopback_poll(&CODEX_LOOPBACK))
}

/// 取消进行中的回环授权 (关卡片/重开时释放 1455 端口)
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn codex_login_cancel() -> Result<(), String> {
    loopback_stop(&CODEX_LOOPBACK);
    Ok(())
}

/// ② 轮询授权状态; 成功则换 token 并落盘 ~/.codex/auth.json
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn codex_poll_login(device_code: String, user_code: String) -> Result<CodexPollResult, String> {
    let pending = || {
        Ok(CodexPollResult {
            status: "pending".into(),
        })
    };

    let resp = match codex_agent()
        .post(CODEX_DEVICE_TOKEN_URL)
        .set("Content-Type", "application/json")
        .set("User-Agent", CODEX_USER_AGENT)
        .send_json(json!({ "device_auth_id": device_code, "user_code": user_code }))
    {
        Ok(r) => r,
        // 403/404 = 用户尚未在浏览器完成授权, 继续轮询
        Err(ureq::Error::Status(403, _)) | Err(ureq::Error::Status(404, _)) => return pending(),
        Err(ureq::Error::Status(410, _)) => return Err("设备码已过期, 请重新发起授权".into()),
        Err(e) => return Err(format!("轮询授权状态失败: {}", codex_http_err(e))),
    };

    let success: CodexDevicePollSuccess = resp
        .into_json()
        .map_err(|e| format!("解析授权响应失败: {e}"))?;

    // ③ authorization_code + code_verifier 换 access/refresh/id_token
    let tokens = codex_exchange_code(
        &success.authorization_code,
        &success.code_verifier,
        CODEX_DEVICE_REDIRECT_URI,
    )?;
    let refresh_token = tokens
        .refresh_token
        .clone()
        .ok_or_else(|| "授权响应缺少 refresh_token".to_string())?;
    let account_id = codex_account_id(&tokens);

    codex_write_auth_json(&tokens, &refresh_token, account_id.as_deref())?;
    Ok(CodexPollResult {
        status: "ok".into(),
    })
}

/// 用 authorization_code + code_verifier 换 token (redirect_uri 须与授权时一致)
fn codex_exchange_code(
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<CodexTokenResp, String> {
    let resp = codex_agent()
        .post(CODEX_OAUTH_TOKEN_URL)
        .set("User-Agent", CODEX_USER_AGENT)
        .send_form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", CODEX_CLIENT_ID),
            ("code_verifier", code_verifier),
        ])
        .map_err(|e| format!("换取 Token 失败: {}", codex_http_err(e)))?;
    resp.into_json()
        .map_err(|e| format!("解析 Token 响应失败: {e}"))
}

/// 从 id_token / access_token (JWT) 中提取 chatgpt_account_id
fn codex_account_id(tokens: &CodexTokenResp) -> Option<String> {
    let from = |jwt: &str| -> Option<String> {
        let claims = codex_jwt_claims(jwt)?;
        claims
            .get("chatgpt_account_id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| {
                claims
                    .get("https://api.openai.com/auth")
                    .and_then(|a| a.get("chatgpt_account_id"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .or_else(|| {
                claims
                    .get("organizations")
                    .and_then(|o| o.as_array())
                    .and_then(|a| a.first())
                    .and_then(|o| o.get("id"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
    };
    tokens
        .id_token
        .as_deref()
        .and_then(from)
        .or_else(|| from(&tokens.access_token))
}

/// 解析 JWT 的 payload (第二段) 为 JSON
fn codex_jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = codex_b64url_decode(payload)?;
    serde_json::from_slice(&bytes).ok()
}

/// base64url (无填充) 解码 —— 不引第三方 base64 crate
pub(crate) fn codex_b64url_decode(input: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in input.bytes() {
        if c == b'=' {
            break;
        }
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// 按官方 codex CLI 格式写 ~/.codex/auth.json, 外部 `codex` CLI 可直接复用
fn codex_write_auth_json(
    tokens: &CodexTokenResp,
    refresh_token: &str,
    account_id: Option<&str>,
) -> Result<(), String> {
    let path = codex_auth_path().ok_or_else(|| "无法定位用户主目录".to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建 ~/.codex 目录失败: {e}"))?;
    }

    let auth = json!({
        "OPENAI_API_KEY": Value::Null,
        // 现行 codex CLI / 社区插件(llm-openai-via-codex)按 auth_mode 区分 ChatGPT 订阅
        // 与纯 API key 登录; 缺它会被判成 API-key 模式而拒用订阅额度。务必写 "chatgpt"。
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": tokens.id_token.clone().unwrap_or_default(),
            "access_token": tokens.access_token,
            "refresh_token": refresh_token,
            "account_id": account_id.unwrap_or_default(),
        },
        "last_refresh": codex_rfc3339_now(),
    });

    let content =
        serde_json::to_string_pretty(&auth).map_err(|e| format!("序列化 auth.json 失败: {e}"))?;
    // auth.json 含 refresh/access/id token:① 原子写防写一半撕裂 → 外部 codex CLI 读到坏 JSON;
    // ② Unix 下收紧到 0600,NAS/Docker 多用户主机上不让同机其他用户读走凭证。
    atomic_write(&path, &content).map_err(|e| format!("写入 ~/.codex/auth.json 失败: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// 当前 UTC 时间的 RFC3339 字符串 (codex CLI 解析 last_refresh 用), 不引 chrono
pub(crate) fn codex_rfc3339_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Howard Hinnant 的 civil_from_days 算法
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// 打开系统默认浏览器到指定 URL (跨平台)
pub(crate) fn codex_open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // rundll32 不解析 &,URL 原样透传(cmd start 会在 & 处截断 —— OAuth 授权 URL
        // 含多个 & 参数,截断后授权页直接报错)。CREATE_NO_WINDOW(0x0800_0000): 别闪黑窗。
        Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .creation_flags(0x0800_0000)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(url)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
