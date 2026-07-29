//! fsmount 真机端到端探针:模拟一个对端 fsface 上游 → fs_mount 起 WebDAV 桥并
//! `net use` 真挂盘符 → 直接 PROPFIND/GET 验协议 → **经资源管理器同一条路(WebClient)
//! 读写挂载盘上的文件** → fs_unmount 收干净。
//!
//! 上游不是手写的假实现:list/stat/caps/write/op 全部转调**真的 `collab::fsface`**,
//! 共享根经 `POLARIS_FS_ROOTS` 注入。所以这个探针验的是完整链路 ——
//! 资源管理器 → WebClient → DAV 桥 → HTTP → fsface 路径关押/写位 → 真磁盘。
//! 唯一没覆盖的是 http.rs 的 Bearer 角色闸(与读走同一条,形状未变),故这里自己模拟它。
//!
//! 两个阶段,证明权限开关**两个方向**都真的生效:
//!   A 只读(不设 POLARIS_FS_WRITE):读全过 + 写必须被拒;
//!   B 可写(POLARIS_FS_WRITE=1)  :建/写/改名/删全过,且落到对端真实目录里。
//!
//! 跑法:`cargo run --example fsmount_probe --features desktop,collab-host`(Windows 桌面;
//! 全程只动 loopback 与一个临时盘符,结束自动卸载)。任一步失败进程退出码非 0。
#![cfg(feature = "collab-host")]

use axum::{
    extract::{DefaultBodyLimit, Query},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post, put},
    Router,
};
use serde_json::json;
use std::collections::HashMap;

const TOKEN: &str = "probe-token-123";

fn check(step: &str, ok: bool, detail: &str) {
    if ok {
        println!("PASS {step}");
    } else {
        println!("FAIL {step}: {detail}");
        std::process::exit(1);
    }
}

/// 模拟 http.rs 的 Bearer 闸(真闸的形状:没带对令牌一律 403)。
fn authed(headers: &HeaderMap) -> bool {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == format!("Bearer {TOKEN}"))
        .unwrap_or(false)
}

fn deny(e: String) -> axum::response::Response {
    (StatusCode::FORBIDDEN, axum::Json(json!({"error": e}))).into_response()
}

fn path_of(q: &HashMap<String, String>) -> String {
    q.get("path").cloned().unwrap_or_default()
}

async fn up_list(headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> impl IntoResponse {
    if !authed(&headers) {
        return deny("forbidden".into());
    }
    match polaris_app_lib::collab::fsface::list(&path_of(&q)) {
        Ok(entries) => axum::Json(json!({ "entries": entries })).into_response(),
        Err(e) => deny(e),
    }
}

async fn up_stat(headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> impl IntoResponse {
    if !authed(&headers) {
        return deny("forbidden".into());
    }
    match polaris_app_lib::collab::fsface::stat(&path_of(&q)) {
        Ok(entry) => axum::Json(json!({ "entry": entry })).into_response(),
        Err(e) => deny(e),
    }
}

async fn up_caps(headers: HeaderMap) -> impl IntoResponse {
    if !authed(&headers) {
        return deny("forbidden".into());
    }
    let (read, write) = polaris_app_lib::collab::fsface::caps();
    axum::Json(json!({ "read": read, "write": write })).into_response()
}

async fn up_read(headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> impl IntoResponse {
    if !authed(&headers) {
        return deny("forbidden".into());
    }
    match polaris_app_lib::collab::fsface::open_jailed(&path_of(&q)) {
        Ok((mut f, _, _)) => {
            use std::io::Read;
            let mut b = Vec::new();
            match f.read_to_end(&mut b) {
                Ok(_) => b.into_response(),
                Err(e) => deny(format!("读失败: {e}")),
            }
        }
        Err(e) => deny(e),
    }
}

async fn up_write(
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    if !authed(&headers) {
        return deny("forbidden".into());
    }
    use polaris_app_lib::collab::fsface as F;
    let (mut f, t) = match F::begin_write(&path_of(&q)) {
        Ok(v) => v,
        Err(e) => return deny(e),
    };
    use std::io::Write;
    if let Err(e) = f.write_all(&body) {
        F::abort_write(&t);
        return deny(format!("写盘失败: {e}"));
    }
    drop(f);
    match F::commit_write(&t) {
        Ok(()) => axum::Json(json!({"ok": true, "bytes": body.len()})).into_response(),
        Err(e) => {
            F::abort_write(&t);
            deny(e)
        }
    }
}

async fn up_op(
    headers: HeaderMap,
    axum::extract::Path(op): axum::extract::Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    if !authed(&headers) {
        return deny("forbidden".into());
    }
    use polaris_app_lib::collab::fsface as F;
    let rel = path_of(&q);
    let dest = q.get("dest").cloned().unwrap_or_default();
    let out = match op.as_str() {
        "mkdir" => F::mkdir(&rel),
        "delete" => F::remove(&rel),
        "move" => F::rename(&rel, &dest),
        "copy" => F::copy(&rel, &dest),
        other => Err(format!("不认识的操作 {other}")),
    };
    match out {
        Ok(()) => axum::Json(json!({"ok": true})).into_response(),
        Err(e) => deny(e),
    }
}

/// 挂一次盘,返回 (davPort, drive)。对端未就绪/挂不上直接判失败退出。
async fn mount(src: &str, up_port: u16, expect_writable: bool) -> String {
    let v = polaris_app_lib::fsmount::fs_mount(
        src.into(),
        "探针盘".into(),
        "not-a-real-node-id".into(), // 看门狗重连会解析失败,无副作用
        up_port,
        TOKEN.into(),
    )
    .await
    .expect("fs_mount 不应报错");
    println!("fs_mount({src}) → {v}");
    check("对端探活(上游就绪)", v["ok"].as_bool() == Some(true), &v.to_string());
    check(
        &format!("读写档位协商正确(期望 writable={expect_writable})"),
        v["writable"].as_bool() == Some(expect_writable),
        &v.to_string(),
    );
    let drive = v["drive"].as_str().unwrap_or("").to_string();
    if drive.is_empty() {
        println!("FAIL 盘符验证:net use 未挂上 —— {}", v["error"]);
        std::process::exit(1);
    }
    println!("已挂载盘符:{drive}");
    drive
}

#[tokio::main]
async fn main() {
    // 独占一份临时 collab.db:fsface 读落库共享根时会开库,别碰真库。
    let db = std::env::temp_dir().join("polaris-fsmount-probe.db");
    let _ = std::fs::remove_file(&db);
    std::env::set_var("POLARIS_COLLAB_DB", &db);

    // ── 造一个「对端共享目录」:普通文件 + 中文名子目录 ──
    let root = std::env::temp_dir().join("polaris-fsmount-probe");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("中文 目录")).unwrap();
    std::fs::write(root.join("hello.txt"), b"hello from remote").unwrap();
    std::fs::write(root.join("中文 目录").join("b.md"), "# 你好".as_bytes()).unwrap();
    // 8MB 伪随机文件:流式泵要跨 32 个 256KB 块,校验和能抓「块序错/丢块/重块」。
    let big: Vec<u8> = (0..8 * 1024 * 1024u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
        .collect();
    std::fs::write(root.join("big.bin"), &big).unwrap();
    // 共享根走**落库**那条路(桌面互联页勾选目录用的就是它),不用 POLARIS_FS_ROOTS ——
    // 这样连「点选放开写权限」这个开关本身都一并验到了。
    std::env::remove_var("POLARIS_FS_ROOTS");
    std::env::remove_var("POLARIS_FS_WRITE");
    let share = |write: bool| {
        polaris_app_lib::collab::fsface::set_shared_entries(&[
            polaris_app_lib::collab::fsface::ShareRoot {
                path: root.to_string_lossy().to_string(),
                write,
            },
        ])
        .expect("设共享根不应失败")
    };
    share(false); // 阶段 A:只读

    // ── 起模拟上游(顶替 iroh 隧道本地口的位置),转调真 fsface ──
    let up = Router::new()
        .route("/api/fs/list", get(up_list))
        .route("/api/fs/stat", get(up_stat))
        .route("/api/fs/caps", get(up_caps))
        .route("/api/fs/read", get(up_read))
        .route("/api/fs/write", put(up_write))
        .route("/api/fs/op/:op", post(up_op))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024));
    let l = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let up_port = l.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(l, up).await.unwrap() });
    println!("mock 上游在 127.0.0.1:{up_port}(转调真 fsface)");

    // ══════════════ 阶段 A:只读盘 ══════════════
    println!("\n── 阶段 A:只读共享 ──");
    let drive = mount("probe-ro", up_port, false).await;

    // 桥协议直验:密钥路径挡门。
    let status = polaris_app_lib::fsmount::fs_mount_status();
    let dav_port = status[0]["davPort"].as_u64().unwrap() as u16;
    let no_secret = ureq::get(&format!("http://127.0.0.1:{dav_port}/hello.txt")).call();
    check(
        "无密钥路径被拒(404)",
        matches!(no_secret, Err(ureq::Error::Status(404, _))),
        &format!("{no_secret:?}"),
    );
    check("status 有记录", status.len() == 1, &format!("{status:?}"));

    // 走资源管理器同一条路(WebClient)读挂载盘。
    let got = std::fs::read(format!("{drive}\\hello.txt"));
    check(
        "盘符读普通文件",
        got.as_deref().map(|b| b == b"hello from remote").unwrap_or(false),
        &format!("{got:?}"),
    );
    let listing = std::fs::read_dir(format!("{drive}\\")).map(|r| {
        r.flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>()
    });
    check(
        "盘符列根目录(含中文目录)",
        listing
            .as_ref()
            .map(|v| v.iter().any(|n| n == "hello.txt") && v.iter().any(|n| n == "中文 目录"))
            .unwrap_or(false),
        &format!("{listing:?}"),
    );
    let got_big = std::fs::read(format!("{drive}\\big.bin"));
    check(
        "盘符读 8MB 文件(流式泵逐字节一致)",
        got_big.as_deref().map(|b| b == big.as_slice()).unwrap_or(false),
        &format!("len={:?}", got_big.as_ref().map(|b| b.len())),
    );
    let got2 = std::fs::read(format!("{drive}\\中文 目录\\b.md"));
    check(
        "盘符读中文路径文件",
        got2.as_deref().map(|b| b == "# 你好".as_bytes()).unwrap_or(false),
        &format!("{got2:?}"),
    );
    // 只读档位:写入必须失败,且对端目录里不能多出东西。
    let w = std::fs::write(format!("{drive}\\should-fail.txt"), b"x");
    check("只读盘写入被拒", w.is_err(), "写入居然成功了");
    check(
        "只读盘写入没落到对端磁盘",
        !root.join("should-fail.txt").exists(),
        "对端目录里真多出了文件 —— 写位闸没兜住",
    );

    polaris_app_lib::fsmount::fs_unmount("probe-ro".into()).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    check(
        "卸载后盘符消失",
        !std::path::Path::new(&format!("{drive}\\")).exists(),
        "盘符还在",
    );

    // ══════════════ 阶段 B:可写盘 ══════════════
    println!("\n── 阶段 B:放开写权限(等价于互联页里点一下「可写」)──");
    share(true);
    let drive = mount("probe-rw", up_port, true).await;

    // 1) 新建文件:经 WebClient 走 LOCK → PUT → PROPPATCH → UNLOCK 整套。
    //    验收看**对端真实目录**(不看盘符),避开 WebClient 的本地缓存干扰。
    let w = std::fs::write(format!("{drive}\\写进来的.txt"), "写盘成功".as_bytes());
    check("盘符新建文件(中文名)", w.is_ok(), &format!("{w:?}"));
    let landed = std::fs::read(root.join("写进来的.txt"));
    check(
        "文件真落到对端目录且内容一致",
        landed.as_deref().map(|b| b == "写盘成功".as_bytes()).unwrap_or(false),
        &format!("{landed:?}"),
    );

    // 2) 5MB 文件:跨多块的上传(ChanReader 分块喂 ureq)。WebClient 默认单文件
    //    上限 50MB,5MB 稳在闸内。
    let payload: Vec<u8> = (0..5 * 1024 * 1024u32)
        .map(|i| (i.wrapping_mul(40503) >> 16) as u8)
        .collect();
    let w = std::fs::write(format!("{drive}\\up.bin"), &payload);
    check("盘符写 5MB 文件", w.is_ok(), &format!("{w:?}"));
    let landed = std::fs::read(root.join("up.bin"));
    check(
        "5MB 上传逐字节一致(分块喂流没错序/丢块)",
        landed.as_deref().map(|b| b == payload.as_slice()).unwrap_or(false),
        &format!("len={:?}", landed.as_ref().map(|b| b.len())),
    );

    // 3) 建目录(MKCOL)。
    let r = std::fs::create_dir(format!("{drive}\\新建目录"));
    check("盘符建目录", r.is_ok(), &format!("{r:?}"));
    check("目录真落到对端", root.join("新建目录").is_dir(), "对端没有这个目录");

    // 4) 改名/移动(MOVE)。
    let r = std::fs::rename(
        format!("{drive}\\写进来的.txt"),
        format!("{drive}\\新建目录\\改名了.txt"),
    );
    check("盘符改名+移动", r.is_ok(), &format!("{r:?}"));
    check(
        "移动在对端生效(源没了、目标在)",
        !root.join("写进来的.txt").exists() && root.join("新建目录/改名了.txt").exists(),
        "对端的移动结果不对",
    );

    // 5) 删除(DELETE)。
    let r = std::fs::remove_file(format!("{drive}\\up.bin"));
    check("盘符删文件", r.is_ok(), &format!("{r:?}"));
    check("删除在对端生效", !root.join("up.bin").exists(), "对端文件还在");

    // 6) 穿越防线:盘符层拦不住的话,fsface 也必须拦住 —— 直接对上游发一发越界写。
    let esc = ureq::put(&format!(
        "http://127.0.0.1:{up_port}/api/fs/write?path=..%2Fescaped.txt"
    ))
    .set("Authorization", &format!("Bearer {TOKEN}"))
    .send_bytes(b"nope");
    check(
        "越界写(../)被 fsface 拒",
        matches!(esc, Err(ureq::Error::Status(403, _))),
        &format!("{esc:?}"),
    );
    check(
        "越界写没落盘",
        !std::env::temp_dir().join("escaped.txt").exists(),
        "居然写到共享根外面去了",
    );

    polaris_app_lib::fsmount::fs_unmount("probe-rw".into()).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    check(
        "卸载后盘符消失",
        !std::path::Path::new(&format!("{drive}\\")).exists(),
        "盘符还在",
    );

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(&db);
    println!("\nALL PASS");
}
