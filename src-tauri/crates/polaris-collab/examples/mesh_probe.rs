//! 同账号设备网的真 HTTP 端到端探针。
//!
//! 单测只验到 mesh.rs 那张表;这个探针补的是**整条链**,而且是跨三个独立 collab.db 的:
//!
//! ```text
//!   云机(账号权威 + 设备目录)     主机 A(我的台式机)      主机 B(我的 NAS)
//!        │  signup/login              │                        │
//!        │◀─ enroll(断言) ─ A ────────┤                        │
//!        │◀─ enroll(断言) ─ B ───────────────────────────────── ┤
//!        │── peers ──▶ A 看见 B       │                        │
//!        │◀─ assert ── A              │                        │
//!        │                        A ── login_assertion ──────▶ │  换到 B 的本机会话
//!        │                            │           A 拿这个 token 读 B 的盘 ✓
//! ```
//!
//! 要证明的几件事(每一条都是这次改动的关键断言):
//!  1. 同账号两台设备**互相看得见**,不必粘任何连接码;
//!  2. 设备密钥能自助换断言,断言能进对端的门 —— 全程**没有任何一台设备的令牌经过云机**;
//!  3. 陌生账号(在云端注册合法、但不是 B 的成员)**进不去** B —— 云端有号 ≠ 谁家都能进;
//!  4. 换到的会话 token 真能用:拿它读 B 共享出来的盘;
//!  5. 吊销即刻生效,重新入网可自助恢复。
//!
//! 用法:
//!   cargo run -p polaris-collab --example mesh_probe --features collab-host

use serde_json::{json, Value};

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

fn need(what: &str, ok: bool, detail: impl std::fmt::Debug) {
    if ok {
        println!("  ✓ {what}");
    } else {
        println!("  ✗ {what} —— {detail:?}");
        std::process::exit(1);
    }
}

/// 起一台「机器」:独立的 collab.db,可选权威模式,返回 (base_url, 库路径)。
///
/// 三台机器在同一个进程里,而 collab.db 的路径来自**进程级环境变量** —— 所以每次请求
/// 打到哪台机器,必须由 handler 执行时的 env 决定。这里给每台机器包一层中间件,在进入
/// 路由前把 env 切到它自己的库上;探针是单线程发请求(逐条 ureq 同步调用),故不会打架。
/// 这是探针专用的手法,生产里一个进程只有一台机器,不存在这个问题。
async fn spawn_host(dir: &std::path::Path, tag: &str, authority: bool) -> String {
    let db = dir.join(format!("{tag}.db"));
    let key = dir.join(format!("{tag}.key"));
    let db_s = db.to_string_lossy().to_string();
    let key_s = key.to_string_lossy().to_string();
    let is_auth = authority;

    let (tx, _rx) = tokio::sync::broadcast::channel(64);
    let state = polaris_collab::collab::http::CollabState {
        app: polaris_collab::host::AppHandle::new(tx),
        auth_token: std::sync::Arc::new(None),
        advertise: Default::default(),
    };
    let router = polaris_collab::collab::http::collab_router(state, false).layer(
        axum::middleware::from_fn(move |req: axum::extract::Request, next: axum::middleware::Next| {
            let db_s = db_s.clone();
            let key_s = key_s.clone();
            async move {
                std::env::set_var("POLARIS_COLLAB_DB", &db_s);
                std::env::set_var("POLARIS_ACCOUNT_KEY", &key_s);
                if is_auth {
                    std::env::set_var("POLARIS_ACCOUNT_AUTHORITY", "1");
                } else {
                    std::env::remove_var("POLARIS_ACCOUNT_AUTHORITY");
                }
                next.run(req).await
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    base
}

#[tokio::main]
async fn main() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("polaris-mesh-probe-{stamp}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("POLARIS_ACCOUNT_OPEN_SIGNUP", "1");

    let cloud = spawn_host(&dir, "cloud", true).await;
    let host_a = spawn_host(&dir, "hostA", false).await;
    let host_b = spawn_host(&dir, "hostB", false).await;
    println!("云机(权威+设备目录) {cloud}");
    println!("主机 A(台式机)      {host_a}");
    println!("主机 B(NAS)         {host_b}\n");

    // 主机 B 共享一个目录出去(等价于用户在互联页勾了一个目录并打开写位)。
    let share = dir.join("nas-share");
    std::fs::create_dir_all(&share).unwrap();
    std::fs::write(share.join("来自NAS.txt"), "NAS 上的文件".as_bytes()).unwrap();

    let d = dir.clone();
    let out = tokio::task::spawn_blocking(move || {
        probe(&cloud, &host_a, &host_b, &d, &share)
    })
    .await;
    if let Err(e) = out {
        eprintln!("探针崩了: {e}");
        std::process::exit(1);
    }
    let _ = std::fs::remove_dir_all(&dir);
    println!("\nALL PASS");
}

fn probe(cloud: &str, host_a: &str, host_b: &str, dir: &std::path::Path, share: &std::path::Path) {
    const NODE_A: &str = "node-aaaaaaaaaaaaaaaaaaaa";
    const NODE_B: &str = "node-bbbbbbbbbbbbbbbbbbbb";

    // ── ① 云端注册一个全局账号 ──
    println!("① 云端账号中心:注册 + 登录");
    let (code, v) = post(
        cloud,
        "/api/account/signup",
        None,
        json!({"username":"wuli","password":"s3cret-88","displayName":"武力","email":""}),
    );
    need("注册成功并拿到 uid", code == 200 && v["uid"].is_string(), &v);
    let uid = v["uid"].as_str().unwrap().to_string();

    let (code, v) = post(
        cloud,
        "/api/account/login",
        None,
        json!({"username":"wuli","password":"s3cret-88"}),
    );
    need("登录拿到身份断言", code == 200 && v["assertion"].is_string(), &v);
    let assertion_a = v["assertion"].as_str().unwrap().to_string();

    // ── ② 两台设备入网 ──
    println!("\n② 设备入网(拿断言换长期设备密钥)");
    let (code, v) = post(
        cloud,
        "/api/mesh/enroll",
        None,
        json!({"assertion": assertion_a, "nodeId": NODE_A, "name":"我的台式机","os":"windows","ver":"2.6.0"}),
    );
    need("主机 A 入网", code == 200 && v["meshKey"].is_string(), &v);
    need("入网返回的 uid 与账号一致", v["uid"] == json!(uid), &v);
    let key_a = v["meshKey"].as_str().unwrap().to_string();

    // B 要一张自己的断言(同一个账号,再登一次即可 —— 断言是短命的,不共用)
    let (_, v) = post(
        cloud,
        "/api/account/login",
        None,
        json!({"username":"wuli","password":"s3cret-88"}),
    );
    let assertion_b = v["assertion"].as_str().unwrap().to_string();
    let (code, v) = post(
        cloud,
        "/api/mesh/enroll",
        None,
        json!({"assertion": assertion_b, "nodeId": NODE_B, "name":"我的 NAS","os":"linux","ver":"2.6.0"}),
    );
    need("主机 B 入网", code == 200 && v["meshKey"].is_string(), &v);
    let key_b = v["meshKey"].as_str().unwrap().to_string();
    need("两台设备各自一把密钥", key_a != key_b, ());

    // 伪造的断言进不了网(签名验不过)。
    let (code, _) = post(
        cloud,
        "/api/mesh/enroll",
        None,
        json!({"assertion":"PA1.eyJmYWtlIjoxfQ.AAAA","nodeId":"node-evil","name":"坏人"}),
    );
    need("伪造断言入网被拒", code >= 400, code);

    // ── ③ 互相看见(这就是「不必再粘连接码」的那一刻)──
    println!("\n③ 同账号设备互相可见");
    let (code, v) = post(cloud, "/api/mesh/announce", Some(&key_a), json!({}));
    need("A 报到成功", code == 200, &v);
    let peers = v["peers"].as_array().cloned().unwrap_or_default();
    need("A 的名册里正好一台(B)", peers.len() == 1, &peers);
    need("看见的是 B", peers[0]["node_id"] == json!(NODE_B), &peers[0]);
    need("B 显示在线", peers[0]["online"] == json!(true), &peers[0]);
    need("名字/系统跟着报到更新", peers[0]["name"] == json!("我的 NAS"), &peers[0]);

    let (_, v) = post(cloud, "/api/mesh/announce", Some(&key_b), json!({}));
    need(
        "B 的名册里看见 A",
        v["peers"][0]["node_id"] == json!(NODE_A),
        &v["peers"],
    );

    // 没有密钥 / 假密钥都进不来。
    let (code, _) = post(cloud, "/api/mesh/announce", None, json!({}));
    need("无密钥报到被拒", code == 401, code);
    let (code, _) = post(cloud, "/api/mesh/announce", Some("mk_bogus"), json!({}));
    need("假密钥报到被拒", code >= 400, code);

    // ── ④ 设备密钥自助换断言 → 进对端的门 ──
    println!("\n④ A 用设备密钥自助换断言,去 B 换本机会话");
    let (code, v) = post(cloud, "/api/mesh/assert", Some(&key_a), json!({}));
    need("换到新断言", code == 200 && v["assertion"].is_string(), &v);
    let fresh = v["assertion"].as_str().unwrap().to_string();

    // 主机 B 必须先「信任」这个账号中心,否则它不认任何断言 —— 桌面上这一步由
    // mesh_join 的 authority::pin_explicit 完成(用户填地址那一刻)。这里手工复现它:
    // 先取公钥(会打到云机,故 env 会被中间件切到云库),再切回 B 的库写三个 meta 键。
    // 顺序必须是「先取后切」:反了就会把 B 的信任写进云机自己的库里。
    let pubkey: String = ureq::get(&format!("{cloud}/api/account/pubkey"))
        .call()
        .expect("取权威公钥")
        .into_json::<Value>()
        .expect("公钥响应")["publicKey"]
        .as_str()
        .expect("publicKey 字段")
        .to_string();
    std::env::set_var("POLARIS_COLLAB_DB", dir.join("hostB.db"));
    std::env::remove_var("POLARIS_ACCOUNT_AUTHORITY");
    polaris_collab::collab::db::meta_set("authority_url", cloud).unwrap();
    polaris_collab::collab::db::meta_set("authority_pub", &pubkey).unwrap();
    polaris_collab::collab::db::meta_set(
        "authority_kid",
        &polaris_collab::collab::authority::kid_of(&pubkey),
    )
    .unwrap();
    need("主机 B 已信任云端账号中心(TOFU 钉公钥)", true, ());

    // 主机 B 是空库:第一个用云端账号登进来的人就是它的 owner(等价于 bootstrap)。
    let (code, v) = post(
        host_b,
        "/api/collab/login_assertion",
        None,
        json!({"assertion": fresh, "deviceId": NODE_A}),
    );
    need("A 在 B 上换到本机会话 token", code == 200 && v["token"].is_string(), &v);
    need("A 在 B 上是 owner(空库首登)", v["user"]["role"] == json!("owner"), &v);
    let token_on_b = v["token"].as_str().unwrap().to_string();

    // ── ⑤ 拿这个 token 真读 B 的盘 ──
    println!("\n⑤ 用换到的 token 读 B 共享出来的盘");
    // B 侧开放共享目录(env 注入 = 等价于 NAS 上 docker-compose 里配的那份)。
    std::env::set_var("POLARIS_FS_ROOTS", share.to_string_lossy().to_string());
    std::env::set_var("POLARIS_FS_WRITE", "1");

    let (code, v) = get(host_b, "/api/fs/caps", &token_on_b);
    need("读到 B 的能力位", code == 200, &v);
    need("B 报告可写(它开了写位)", v["write"] == json!(true), &v);

    let (code, v) = get(host_b, "/api/fs/list?path=", &token_on_b);
    need("列到 B 的共享目录", code == 200, &v);
    let names: Vec<String> = v["entries"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| e["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    need("看见 NAS 上那个文件", names.iter().any(|n| n == "来自NAS.txt"), &names);

    // 写一把:这是「同账号盘可读写」的最终验收。
    let put = ureq::put(&format!("{host_b}/api/fs/write?path=A%E5%86%99%E7%9A%84.txt"))
        .set("Authorization", &format!("Bearer {token_on_b}"))
        .send_bytes("A 从台式机写过来的".as_bytes());
    need("A 往 B 的盘里写文件", put.is_ok(), format!("{put:?}"));
    let landed = std::fs::read(share.join("A写的.txt"));
    need(
        "文件真落在 B 的磁盘上",
        landed.as_deref().ok() == Some("A 从台式机写过来的".as_bytes()),
        &landed,
    );

    // 没令牌读不到(闸还在)。
    let anon = ureq::get(&format!("{host_b}/api/fs/list?path=")).call();
    need("无令牌读 B 的盘被拒", anon.is_err(), format!("{anon:?}"));

    // ── ⑥ 陌生账号:云端有号 ≠ 进得去别人的机器 ──
    println!("\n⑥ 陌生账号进不去别人的主机");
    let (_, v) = post(
        cloud,
        "/api/account/signup",
        None,
        json!({"username":"mallory","password":"s3cret-88","displayName":"路人","email":""}),
    );
    let mallory = v["assertion"].as_str().unwrap_or("").to_string();
    need("陌生人也拿到了合法断言(云端确实认他)", !mallory.is_empty(), &v);
    let (code, v) = post(
        host_b,
        "/api/collab/login_assertion",
        None,
        json!({"assertion": mallory, "deviceId":"node-mallory"}),
    );
    need(
        "但他进不去 B(要邀请)",
        code >= 400 && v["error"].as_str().unwrap_or("").contains("邀请"),
        &v,
    );

    // ── ⑦ 吊销即刻生效,重新入网可恢复 ──
    println!("\n⑦ 吊销与自助恢复");
    let (code, v) = post(cloud, "/api/mesh/revoke", Some(&key_b), json!({"nodeId": NODE_A}));
    need("B 把 A 移出设备网(同账号可互踢)", code == 200, &v);
    let (code, v) = post(cloud, "/api/mesh/announce", Some(&key_a), json!({}));
    need("A 的旧密钥立刻失效", code >= 400, &v);
    let (_, v) = post(cloud, "/api/mesh/announce", Some(&key_b), json!({}));
    need("A 从 B 的名册里消失", v["peers"].as_array().unwrap().is_empty(), &v["peers"]);

    // 本人重新登录 → 重新入网 → 立刻回到名册。
    let (_, v) = post(
        cloud,
        "/api/account/login",
        None,
        json!({"username":"wuli","password":"s3cret-88"}),
    );
    let again = v["assertion"].as_str().unwrap().to_string();
    let (code, v) = post(
        cloud,
        "/api/mesh/enroll",
        None,
        json!({"assertion": again, "nodeId": NODE_A, "name":"我的台式机","os":"windows","ver":"2.6.0"}),
    );
    need("重新入网成功", code == 200, &v);
    let key_a2 = v["meshKey"].as_str().unwrap().to_string();
    need("拿到的是一把新密钥", key_a2 != key_a, ());
    let (code, _) = post(cloud, "/api/mesh/announce", Some(&key_a), json!({}));
    need("旧密钥仍然无效", code >= 400, code);
    let (_, v) = post(cloud, "/api/mesh/announce", Some(&key_b), json!({}));
    need(
        "A 回到 B 的名册,且仍是一台(没造出重影)",
        v["peers"].as_array().map(|a| a.len()) == Some(1),
        &v["peers"],
    );

    let _ = dir;
    std::env::remove_var("POLARIS_FS_ROOTS");
    std::env::remove_var("POLARIS_FS_WRITE");
}
