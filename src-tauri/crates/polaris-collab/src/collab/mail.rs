//! collab/mail.rs —— 邮箱验证码(注册 / 找回密码)与 SMTP 发信。
//!
//! 配置两级:环境变量 POLARIS_SMTP_*(部署态,Docker -e 注入)优先,
//! 其次 collab.db meta 表(owner 在「邮箱服务设置」面板里填,桌面主机免环境变量)。
//! QQ 邮箱:host=smtp.qq.com,port=465(隐式 TLS),user=你的QQ邮箱,
//! pass=**SMTP 授权码**(不是 QQ 密码,须账号主人在 QQ 邮箱「设置→账号」开启 SMTP 领取)。
//!
//! 安全口径:
//! - 明文验证码只进邮件,库里存 argon2id 哈希;10 分钟过期,5 次错误即作废。
//! - 同邮箱同用途 60s 内不重发,每小时最多 5 封(内存态,重启清零)。
//! - SMTP 密码读取后不回显(admin 面板只给「已配置」掩码)。
#![cfg(feature = "collab-host")]

use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use once_cell::sync::Lazy;
use rusqlite::params;
use std::collections::HashMap;
use std::sync::Mutex;

use super::auth;
use super::db::{self, now, open_db};

/// 验证码有效期(秒)。
const CODE_TTL: i64 = 10 * 60;
/// 同邮箱同用途重发间隔(秒)。
const RESEND_GAP: i64 = 60;
/// 每邮箱每小时发信上限。
const HOURLY_CAP: usize = 5;
/// 单枚验证码最大试错次数。
const MAX_ATTEMPTS: i64 = 5;

/// 发信频控(内存态):email → 最近一小时发送时刻表。
static SEND_LOG: Lazy<Mutex<HashMap<String, Vec<i64>>>> = Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Clone)]
pub struct SmtpCfg {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub pass: String,
    pub from: String,
}

fn env_nonempty(k: &str) -> Option<String> {
    std::env::var(k).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn meta_nonempty(k: &str) -> Option<String> {
    db::meta_get(k).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// 读当前 SMTP 配置(env 优先,meta 兜底)。user+pass 齐了才算配置完成。
pub fn config() -> Option<SmtpCfg> {
    let user = env_nonempty("POLARIS_SMTP_USER").or_else(|| meta_nonempty("smtp_user"))?;
    let pass = env_nonempty("POLARIS_SMTP_PASS").or_else(|| meta_nonempty("smtp_pass"))?;
    let host = env_nonempty("POLARIS_SMTP_HOST")
        .or_else(|| meta_nonempty("smtp_host"))
        .unwrap_or_else(|| "smtp.qq.com".into());
    let port = env_nonempty("POLARIS_SMTP_PORT")
        .or_else(|| meta_nonempty("smtp_port"))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(465);
    let from = env_nonempty("POLARIS_SMTP_FROM")
        .or_else(|| meta_nonempty("smtp_from"))
        .unwrap_or_else(|| user.clone());
    Some(SmtpCfg { host, port, user, pass, from })
}

pub fn configured() -> bool {
    config().is_some()
}

/// 邮箱自助注册是否开放:SMTP 配置齐 + 未被显式关闭
/// (env POLARIS_EMAIL_SIGNUP=0 或 meta email_signup=0 都能关;默认开)。
pub fn signup_open() -> bool {
    if !configured() {
        return false;
    }
    if let Ok(v) = std::env::var("POLARIS_EMAIL_SIGNUP") {
        if v == "0" || v.eq_ignore_ascii_case("false") {
            return false;
        }
    }
    db::meta_get("email_signup").map(|v| v != "0").unwrap_or(true)
}

/// owner 面板保存配置(写 meta;空 pass 表示保留旧授权码不动)。
pub fn save_config(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
    from: &str,
    signup: bool,
) -> Result<(), String> {
    db::meta_set("smtp_host", host.trim())?;
    db::meta_set("smtp_port", &port.to_string())?;
    db::meta_set("smtp_user", user.trim())?;
    if !pass.trim().is_empty() {
        db::meta_set("smtp_pass", pass.trim())?;
    }
    db::meta_set("smtp_from", from.trim())?;
    db::meta_set("email_signup", if signup { "1" } else { "0" })?;
    Ok(())
}

/// 发一封纯文本信。阻塞(SMTP 往返数秒),调用方须放 spawn_blocking。
pub fn send_mail(to: &str, subject: &str, body: &str) -> Result<(), String> {
    let cfg = config().ok_or("邮箱服务未配置:请管理员先在「邮箱服务设置」填好 SMTP")?;
    let from = format!("Polaris 设备联盟 <{}>", cfg.from);
    let msg = Message::builder()
        .from(from.parse().map_err(|e| format!("发件人地址无效: {e}"))?)
        .to(to.parse().map_err(|e| format!("收件人地址无效: {e}"))?)
        .subject(subject)
        .header(ContentType::TEXT_PLAIN)
        .body(body.to_string())
        .map_err(|e| format!("组信失败: {e}"))?;
    // 465 = 隐式 TLS(QQ 邮箱默认);587 等其他端口走 STARTTLS。
    let builder = if cfg.port == 465 {
        SmtpTransport::relay(&cfg.host)
    } else {
        SmtpTransport::starttls_relay(&cfg.host)
    }
    .map_err(|e| format!("SMTP 连接器创建失败: {e}"))?;
    let mailer = builder
        .port(cfg.port)
        .credentials(Credentials::new(cfg.user.clone(), cfg.pass.clone()))
        .timeout(Some(std::time::Duration::from_secs(20)))
        .build();
    mailer
        .send(&msg)
        .map(|_| ())
        .map_err(|e| format!("发信失败(检查 SMTP 授权码/网络): {e}"))
}

fn gen_code() -> Result<String, String> {
    let mut b = [0u8; 4];
    getrandom::getrandom(&mut b).map_err(|e| format!("生成验证码失败: {e}"))?;
    Ok(format!("{:06}", u32::from_le_bytes(b) % 1_000_000))
}

fn rate_check(email: &str) -> Result<(), String> {
    let t = now();
    let mut log = SEND_LOG.lock().unwrap();
    // 防膨胀:表过大时先剪掉一小时前的旧记录。
    if log.len() > 2000 {
        log.retain(|_, v| {
            v.retain(|at| t - *at < 3600);
            !v.is_empty()
        });
    }
    let v = log.entry(email.to_string()).or_default();
    v.retain(|at| t - *at < 3600);
    if v.len() >= HOURLY_CAP {
        return Err("这个邮箱一小时内发送次数已达上限,请稍后再试".into());
    }
    if v.last().is_some_and(|last| t - *last < RESEND_GAP) {
        return Err("发送太频繁,请一分钟后再试".into());
    }
    v.push(t);
    Ok(())
}

/// 生成并发送验证码(purpose: signup|reset|login)。阻塞,须放 spawn_blocking。
pub fn issue_code(email: &str, purpose: &str) -> Result<(), String> {
    let email = auth::validate_email(email)?;
    rate_check(&email)?;
    let code = gen_code()?;
    let phc = auth::hash_password(&code)?;
    // 文案要如实说明这封信是干什么用的 —— 收信人据此判断「这是不是我本人在操作」,
    // 把登录码写成「找回密码」会让人放过一次真正的盗号尝试。
    let action = match purpose {
        "signup" => "注册账号",
        "login" => "登录",
        _ => "找回密码",
    };
    let body = format!(
        "你正在 Polaris 设备联盟{action},验证码:\n\n    {code}\n\n10 分钟内有效。如果这不是你本人的操作,请忽略这封邮件。"
    );
    // 先发信后落库:发失败不占用「最新一枚」位置,用户可立即重试。
    send_mail(&email, &format!("【Polaris】{action}验证码 {code}"), &body)?;
    let t = now();
    let conn = open_db()?;
    conn.execute(
        "INSERT INTO email_codes(email,purpose,code_hash,attempts,created_at,expires_at) \
         VALUES(?1,?2,?3,0,?4,?5) \
         ON CONFLICT(email,purpose) DO UPDATE SET code_hash=excluded.code_hash, \
         attempts=0, created_at=excluded.created_at, expires_at=excluded.expires_at",
        params![email, purpose, phc, t, t + CODE_TTL],
    )
    .map_err(|e| format!("验证码落库失败: {e}"))?;
    Ok(())
}

/// 校验验证码:过期/超试错即作废,校验成功即销毁(一次性)。
pub fn verify_code(email: &str, purpose: &str, code: &str) -> Result<(), String> {
    let email = auth::validate_email(email)?;
    let code = code.trim();
    if code.is_empty() {
        return Err("请填写邮箱验证码".into());
    }
    let conn = open_db()?;
    let row = conn.query_row(
        "SELECT code_hash,attempts,expires_at FROM email_codes WHERE email=?1 AND purpose=?2",
        params![email, purpose],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        },
    );
    let (phc, attempts, expires_at) = match row {
        Ok(v) => v,
        Err(_) => return Err("请先获取邮箱验证码".into()),
    };
    let gone = |conn: &rusqlite::Connection| {
        let _ = conn.execute(
            "DELETE FROM email_codes WHERE email=?1 AND purpose=?2",
            params![email, purpose],
        );
    };
    if now() > expires_at {
        gone(&conn);
        return Err("验证码已过期,请重新获取".into());
    }
    if attempts >= MAX_ATTEMPTS {
        gone(&conn);
        return Err("错误次数过多,验证码已作废,请重新获取".into());
    }
    if !auth::verify_password(code, &phc) {
        let _ = conn.execute(
            "UPDATE email_codes SET attempts=attempts+1 WHERE email=?1 AND purpose=?2",
            params![email, purpose],
        );
        return Err("验证码不对".into());
    }
    gone(&conn);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db() -> std::sync::MutexGuard<'static, ()> {
        let g = super::super::db::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let p = std::env::temp_dir().join(format!(
            "collab-mail-test-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("POLARIS_COLLAB_DB", p);
        g
    }

    /// 不真发信:直接落一枚已知码,验证「校验-销毁-试错」状态机。
    fn plant_code(email: &str, purpose: &str, code: &str, expires_in: i64) {
        let phc = auth::hash_password(code).unwrap();
        let t = now();
        let conn = open_db().unwrap();
        conn.execute(
            "INSERT INTO email_codes(email,purpose,code_hash,attempts,created_at,expires_at) \
             VALUES(?1,?2,?3,0,?4,?5) \
             ON CONFLICT(email,purpose) DO UPDATE SET code_hash=excluded.code_hash, \
             attempts=0, created_at=excluded.created_at, expires_at=excluded.expires_at",
            params![email, purpose, phc, t, t + expires_in],
        )
        .unwrap();
    }

    #[test]
    fn verify_flow_one_shot_and_attempts() {
        let _g = tmp_db();
        plant_code("a@qq.com", "signup", "123456", 600);
        assert!(verify_code("a@qq.com", "signup", "000000").is_err());
        assert!(verify_code("a@qq.com", "signup", "123456").is_ok());
        // 一次性:用过即销毁
        assert!(verify_code("a@qq.com", "signup", "123456").is_err());
        // 试错闸:5 次错后作废
        plant_code("b@qq.com", "reset", "654321", 600);
        for _ in 0..5 {
            assert!(verify_code("b@qq.com", "reset", "999999").is_err());
        }
        let e = verify_code("b@qq.com", "reset", "654321").unwrap_err();
        assert!(e.contains("作废"), "超试错应作废: {e}");
    }

    #[test]
    fn verify_expired_code_rejected() {
        let _g = tmp_db();
        plant_code("c@qq.com", "signup", "123456", -1);
        let e = verify_code("c@qq.com", "signup", "123456").unwrap_err();
        assert!(e.contains("过期"), "{e}");
    }

    #[test]
    fn email_validation_and_rate_gate() {
        let _g = tmp_db();
        assert!(auth::validate_email("好@qq").is_err());
        assert!(auth::validate_email("a b@qq.com").is_err());
        assert_eq!(auth::validate_email(" A@QQ.com ").unwrap(), "a@qq.com");
        // 频控:同邮箱 60s 内第二次直接拒(不真发信,rate_check 在 send 之前)。
        assert!(rate_check("gate@qq.com").is_ok());
        let e = rate_check("gate@qq.com").unwrap_err();
        assert!(e.contains("频繁"), "{e}");
    }
}
