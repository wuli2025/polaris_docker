//! 远程账号管理的真 HTTP 端到端探针(单进程起一台「云机」,全程走 axum 路由 + ureq)。
//!
//! 单测只验到 auth.rs 那一层;这个探针补的是**接口层**:owner 会话门槛、字段语义、
//! 以及最要紧的那条 —— 远程建出来的账号,能不能真的当全局账号在 `/api/account/login` 登进去。
//!
//! 用法:
//!   cargo run -p polaris-collab --example admin_accounts_probe --features collab-host

use serde_json::{json, Value};

const TOKEN: &str = "probe-setup-token";

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

fn get(base: &str, path: &str, bearer: &str) -> (u16, Value) {
    match ureq::get(&format!("{base}{path}"))
        .set("Authorization", &format!("Bearer {bearer}"))
        .call()
    {
        Ok(r) => (r.status(), r.into_json().unwrap_or(Value::Null)),
        Err(ureq::Error::Status(code, r)) => (code, r.into_json().unwrap_or(Value::Null)),
        Err(e) => panic!("请求 {path} 失败: {e}"),
    }
}

fn pass(what: &str) {
    println!("  ✓ {what}");
}

#[tokio::main]
async fn main() {
    // 一台干净的「云机」:临时库 + 临时签名私钥 + 权威模式。
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("polaris-admin-probe-{stamp}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("POLARIS_COLLAB_DB", dir.join("collab.db"));
    std::env::set_var("POLARIS_ACCOUNT_KEY", dir.join("account.key"));
    std::env::set_var("POLARIS_ACCOUNT_AUTHORITY", "1");
    std::env::set_var("POLARIS_AUTH_TOKEN", TOKEN);

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

    let out = tokio::task::spawn_blocking(move || probe(&base)).await;
    if let Err(e) = out {
        eprintln!("探针崩了: {e}");
        std::process::exit(1);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn probe(base: &str) {
    // ① 首启建 owner
    let (code, v) = post(
        base,
        "/api/collab/bootstrap",
        Some(TOKEN),
        json!({"username":"alice","password":"s3cret-88","displayName":"Alice","deviceId":"probe"}),
    );
    assert_eq!(code, 200, "bootstrap 失败: {v}");
    let owner = v["token"].as_str().unwrap().to_string();
    pass("首启建 owner 并拿到会话");

    // ② owner 远程建号(这就是「不必 SSH 进服务器」的那一步)
    let (code, v) = post(
        base,
        "/api/collab/admin/account_create",
        Some(&owner),
        json!({"username":"bob","password":"s3cret-88","displayName":"Bob",
               "email":"bob@example.com","role":"collaborator"}),
    );
    assert_eq!(code, 200, "建号失败: {v}");
    let uid = v["uid"].as_str().unwrap_or_default().to_string();
    assert!(!uid.is_empty(), "权威机建号必须签出全局 uid: {v}");
    let bob_id = v["user"]["id"].as_i64().unwrap();
    pass("owner 远程建号,并签出全局 uid");

    // ③ 关键:远程建的账号能直接当全局账号登(说明它不是个「只在本机能用」的半成品)
    let (code, v) = post(
        base,
        "/api/account/login",
        None,
        json!({"username":"bob","password":"s3cret-88"}),
    );
    assert_eq!(code, 200, "全局登录失败: {v}");
    assert_eq!(v["uid"].as_str(), Some(uid.as_str()));
    assert!(!v["assertion"].as_str().unwrap_or_default().is_empty());
    pass("新账号能在账号中心登录并拿到身份断言(可去任意主机进门)");

    // ④ 邮箱也能当账号名登(绑定确实写进去了)
    let (code, _) = post(
        base,
        "/api/account/login",
        None,
        json!({"email":"bob@example.com","password":"s3cret-88"}),
    );
    assert_eq!(code, 200, "邮箱登录失败");
    pass("邮箱登录同样放行(账号中心全局登录)");

    // ④-b 关键:Web 登录框实际打的是 /api/collab/login。在账号权威自己身上它走**本机密码库**
    // (upstream_url 为空,不转发),此前那条路只认用户名 —— 于是「在云机绑了邮箱却登不了云机界面」。
    let (code, v) = post(
        base,
        "/api/collab/login",
        None,
        json!({"username":"bob@example.com","password":"s3cret-88","deviceId":"probe-mail"}),
    );
    assert_eq!(code, 200, "邮箱登不了主机 Web(/api/collab/login): {v}");
    // 拿邮箱登进来,会话里记的必须仍是用户名
    assert_eq!(
        v["user"]["username"].as_str(),
        Some("bob"),
        "用邮箱登进来后会话里记成了邮箱"
    );
    pass("邮箱能登主机 Web 登录框,且会话记的仍是用户名");

    // ⑤ 改资料 + 改角色;缺席的字段不动
    let (code, v) = post(
        base,
        "/api/collab/admin/account_update",
        Some(&owner),
        json!({"userId": bob_id, "displayName":"鲍勃", "role":"visitor"}),
    );
    assert_eq!(code, 200, "改号失败: {v}");
    let (_, list) = get(base, "/api/collab/admin/users", &owner);
    let row = list
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"].as_i64() == Some(bob_id))
        .unwrap()
        .clone();
    assert_eq!(row["display_name"].as_str(), Some("鲍勃"));
    assert_eq!(row["role"].as_str(), Some("visitor"));
    assert_eq!(row["email"].as_str(), Some("bob@example.com"), "没改的字段被动了");
    assert_eq!(row["uid"].as_str(), Some(uid.as_str()));
    pass("改昵称/角色生效,未提交的邮箱字段纹丝不动");

    // ⑥ 重置密码 → 旧密码作废、新密码可用
    let (code, v) = post(
        base,
        "/api/collab/admin/user_reset_password",
        Some(&owner),
        json!({"userId": bob_id, "newPassword":"n3w-passw0rd"}),
    );
    assert_eq!(code, 200, "重置密码失败: {v}");
    let (code, _) = post(
        base,
        "/api/account/login",
        None,
        json!({"username":"bob","password":"s3cret-88"}),
    );
    assert_ne!(code, 200, "旧密码居然还能登");
    let (code, _) = post(
        base,
        "/api/account/login",
        None,
        json!({"username":"bob","password":"n3w-passw0rd"}),
    );
    assert_eq!(code, 200, "新密码登不进");
    pass("owner 重置密码:旧密码即刻失效,新密码可用");

    // ⑦ 非 owner 碰这组接口一律 403(bob 现在是 visitor)
    let (_, v) = post(
        base,
        "/api/collab/login",
        None,
        json!({"username":"bob","password":"n3w-passw0rd","deviceId":"probe2"}),
    );
    let bob_tok = v["token"].as_str().unwrap_or_default().to_string();
    assert!(!bob_tok.is_empty(), "bob 登不进本机: {v}");
    for path in [
        "/api/collab/admin/account_create",
        "/api/collab/admin/account_update",
        "/api/collab/admin/account_delete",
        "/api/collab/admin/account_uid_backfill",
    ] {
        let (code, _) = post(base, path, Some(&bob_tok), json!({"userId": 1}));
        assert_eq!(code, 403, "{path} 对非 owner 没有拦住");
    }
    pass("非 owner 调这四个口全部 403");

    // ⑧ 自保闸:不能删自己、不能给自己降级、不能删最后一个 owner
    let owner_id = list
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["username"].as_str() == Some("alice"))
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let (code, v) = post(
        base,
        "/api/collab/admin/account_delete",
        Some(&owner),
        json!({"userId": owner_id}),
    );
    assert_eq!(code, 400, "居然允许删自己");
    assert!(v["error"].as_str().unwrap_or_default().contains("自己"));
    let (code, _) = post(
        base,
        "/api/collab/admin/account_update",
        Some(&owner),
        json!({"userId": owner_id, "role":"collaborator"}),
    );
    assert_eq!(code, 400, "居然允许给自己降级");
    pass("删自己/给自己降级都被拦下(管理面不会把自己锁在门外)");

    // ⑨ 删号:会话连坐失效
    let (code, v) = post(
        base,
        "/api/collab/admin/account_delete",
        Some(&owner),
        json!({"userId": bob_id}),
    );
    assert_eq!(code, 200, "删号失败: {v}");
    let (code, _) = get(base, "/api/collab/me", &bob_tok);
    assert_ne!(code, 200, "人删了会话还活着");
    let (code, _) = post(
        base,
        "/api/account/login",
        None,
        json!({"username":"bob","password":"n3w-passw0rd"}),
    );
    assert_ne!(code, 200, "账号删了还能全局登录");
    pass("删号即刻生效:会话作废、全局登录关门");

    println!("\n全部通过 —— 服务器远程账号管理链路可用。");
}
