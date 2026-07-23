//! 邮箱验证码登录的**真收发**端到端探针:真的经 SMTP 发一封信出去,
//! 再真的把它从收件箱取回来完成登录 —— 单测只能验到「码对不对」,
//! 验不了「信发不发得出去、收不收得到」,而后者才是这条功能在生产上翻车的地方。
//!
//! 收信这一步靠外部脚本(Python 的 imaplib),因为本仓不该为一个探针背一个 IMAP 依赖。
//!
//! 用法(凭据只走环境变量,**不准写进源码**):
//!   $env:QQ_USER='xxx@qq.com'; $env:QQ_PASS='<SMTP授权码>'
//!   $env:POLARIS_MAIL_FETCH='<fetch_code.py 的路径>'
//!   cargo run -p polaris-collab --example mail_login_probe --features collab-host
//!
//! 三个环境变量缺任何一个即跳过(退出码 0),方便在没有邮箱凭据的机器上无脑跑。

use serde_json::{json, Value};

const TOKEN: &str = "mail-probe-token";

fn post(base: &str, path: &str, bearer: Option<&str>, body: Value) -> (u16, Value) {
    let mut req = ureq::post(&format!("{base}{path}"));
    if let Some(t) = bearer {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    match req.send_json(body) {
        Ok(r) => (r.status(), r.into_json().unwrap_or(Value::Null)),
        Err(ureq::Error::Status(code, r)) => (code, r.into_json().unwrap_or(Value::Null)),
        Err(e) => panic!("请求 {path} 失败: {e}"),
    }
}

fn pass(what: &str) {
    println!("  ✓ {what}");
}

/// 调外部脚本把验证码从收件箱取回来。`want` 是邮件主题里的动作词(登录/找回密码…)。
fn fetch_code(script: &str, want: &str) -> String {
    let out = std::process::Command::new("python")
        .arg(script)
        .arg(want)
        .output()
        .expect("跑取码脚本失败(python 不在 PATH?)");
    if !out.status.success() {
        panic!(
            "没能从收件箱取到「{want}」验证码: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[tokio::main]
async fn main() {
    let (Ok(user), Ok(_), Ok(script)) = (
        std::env::var("QQ_USER"),
        std::env::var("QQ_PASS"),
        std::env::var("POLARIS_MAIL_FETCH"),
    ) else {
        println!("跳过:未设 QQ_USER / QQ_PASS / POLARIS_MAIL_FETCH");
        return;
    };

    // 一台干净的「云机」:临时库 + 临时签名私钥 + 权威模式。SMTP 走真实凭据
    // (POLARIS_SMTP_USER/PASS 由 mail::config() 从环境读)。
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("polaris-mail-probe-{stamp}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("POLARIS_COLLAB_DB", dir.join("collab.db"));
    std::env::set_var("POLARIS_ACCOUNT_KEY", dir.join("account.key"));
    std::env::set_var("POLARIS_ACCOUNT_AUTHORITY", "1");
    std::env::set_var("POLARIS_AUTH_TOKEN", TOKEN);
    std::env::set_var("POLARIS_SMTP_USER", &user);
    // POLARIS_SMTP_PASS 直接沿用进程里的 QQ_PASS
    std::env::set_var("POLARIS_SMTP_PASS", std::env::var("QQ_PASS").unwrap());

    let (tx, _rx) = tokio::sync::broadcast::channel(64);
    let state = polaris_collab::collab::http::CollabState {
        app: polaris_collab::host::AppHandle::new(tx),
        auth_token: std::sync::Arc::new(Some(TOKEN.to_string())),
        advertise: Default::default(),
    };
    let router = polaris_collab::collab::http::collab_router(state, false);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    println!("云机(账号权威)已起在 {base}\n");

    let out = tokio::task::spawn_blocking(move || probe(&base, &user, &script)).await;
    if let Err(e) = out {
        eprintln!("探针崩了: {e}");
        std::process::exit(1);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn probe(base: &str, mailbox: &str, script: &str) {
    // ① 首启建 owner(它占掉 bootstrap),再由 owner 远程建一个绑了真邮箱的账号
    let (code, v) = post(
        base,
        "/api/collab/bootstrap",
        Some(TOKEN),
        json!({"username":"alice","password":"s3cret-88","displayName":"Alice","deviceId":"probe"}),
    );
    assert_eq!(code, 200, "bootstrap 失败: {v}");
    let owner = v["token"].as_str().unwrap().to_string();
    pass("首启建 owner");

    let (code, v) = post(
        base,
        "/api/collab/admin/account_create",
        Some(&owner),
        json!({"username":"mailbot","password":"s3cret-88","displayName":"邮箱登录测试","email":mailbox,"role":"collaborator"}),
    );
    assert_eq!(code, 200, "建号失败: {v}");
    pass(&format!("owner 建号并绑定真实邮箱 {mailbox}"));

    // ② 邮箱服务确实配上了
    let (code, v) = post(base, "/api/collab/email/status", None, json!({}));
    let configured = if code == 405 {
        // status 是 GET
        match ureq::get(&format!("{base}/api/collab/email/status")).call() {
            Ok(r) => r.into_json::<Value>().unwrap()["configured"] == json!(true),
            Err(e) => panic!("查邮箱状态失败: {e}"),
        }
    } else {
        v["configured"] == json!(true)
    };
    assert!(configured, "邮箱服务没配上");
    pass("邮箱服务已配置(SMTP 凭据可用)");

    // ③ 真发一封登录验证码
    let (code, v) = post(
        base,
        "/api/collab/email/send_code",
        None,
        json!({"email": mailbox, "purpose": "login"}),
    );
    assert_eq!(code, 200, "发验证码失败: {v}");
    pass("验证码已经 SMTP 真发出去");

    // ④ 真从收件箱把它取回来
    let the_code = fetch_code(script, "登录");
    assert_eq!(the_code.len(), 6, "取回来的码不像 6 位: {the_code}");
    pass(&format!("从收件箱取回验证码({the_code})"));

    // ⑤ 拿它换本机会话 —— 全程没有密码
    let (code, v) = post(
        base,
        "/api/collab/email/login",
        None,
        json!({"email": mailbox, "code": the_code, "deviceId": "probe-mail"}),
    );
    assert_eq!(code, 200, "验证码登录失败: {v}");
    assert_eq!(v["user"]["username"].as_str(), Some("mailbot"));
    let session = v["token"].as_str().unwrap().to_string();
    pass("验证码登录成功,签出本机会话(全程没用密码)");

    // ⑥ 会话是真能用的 —— 必须硬断言,不能「请求失败就悄悄跳过」,
    // 那样等于没测(第一版就是这么写的,而且 /api/collab/session 根本不存在)。
    let v: Value = ureq::get(&format!("{base}/api/collab/me"))
        .set("Authorization", &format!("Bearer {session}"))
        .call()
        .expect("拿签出来的会话调 /api/collab/me 失败")
        .into_json()
        .unwrap();
    assert_eq!(
        v["user"]["username"].as_str().or(v["username"].as_str()),
        Some("mailbot"),
        "会话查不出人: {v}"
    );
    pass("会话可用(/api/collab/me 认它)");

    // ⑦ 验证码是一次性的:同一枚再用一次必须被拒
    let (code, _) = post(
        base,
        "/api/collab/email/login",
        None,
        json!({"email": mailbox, "code": the_code, "deviceId": "probe-mail2"}),
    );
    assert_ne!(code, 200, "验证码居然能重复使用");
    pass("验证码一次性:同一枚再用即拒");

    // ⑧ 错码登不进
    let (code, _) = post(
        base,
        "/api/collab/email/login",
        None,
        json!({"email": mailbox, "code": "000000", "deviceId": "probe-mail3"}),
    );
    assert_ne!(code, 200, "错码居然放行");
    pass("错码拒绝");

    // ⑨ 全局验证码登录:再发一枚,换一张能进任意主机的身份断言。
    // 发码有 60s 重发频控 —— 等够再发,而不是「被挡下就跳过」(跳过=这条新端点根本没验)。
    println!("  · 等 65s 让开发码频控…");
    std::thread::sleep(std::time::Duration::from_secs(65));
    let (code, v) = post(
        base,
        "/api/collab/email/send_code",
        None,
        json!({"email": mailbox, "purpose": "login"}),
    );
    assert_eq!(code, 200, "第二枚验证码发不出去: {v}");
    let c2 = fetch_code(script, "登录");
    assert_ne!(c2, the_code, "取回来的还是上一枚旧码");
    let (code, v) = post(
        base,
        "/api/account/login_code",
        None,
        json!({"email": mailbox, "code": c2}),
    );
    assert_eq!(code, 200, "全局验证码登录失败: {v}");
    assert!(!v["assertion"].as_str().unwrap_or_default().is_empty());
    assert!(!v["uid"].as_str().unwrap_or_default().is_empty());
    assert_eq!(v["user"]["username"].as_str(), Some("mailbot"));
    pass("全局验证码登录:换到身份断言(可去任意成员主机进门)");

    // 「停用的账号不能凭验证码进来」由单测 verified_email_login_respects_gates 覆盖 ——
    // 那条不依赖真实邮箱,放这儿反而要多烧一枚码去撞 60s 频控。
    println!("\n全部通过 —— 邮箱验证码登录的发信/收信/换会话闭环成立。");
}
