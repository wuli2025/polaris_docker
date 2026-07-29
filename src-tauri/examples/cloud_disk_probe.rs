//! 真机端到端:把**云服务器的硬盘**挂成本机一块盘符,并读写它。
//!
//! 链路(全程真网络,没有任何模拟):
//! ```text
//!   资源管理器 → Windows WebClient → 本机 loopback WebDAV 桥(fsmount)
//!     → iroh 隧道(打洞/中继) → 云服务器上的 polaris-server(对端地址见 peer.json)
//!     → fsface 路径关押 + 写位 → 容器里的 /home/polaris/Polaris/share
//! ```
//!
//! 跑法(Windows 桌面):
//!   cargo run --example cloud_disk_probe --features desktop,collab-host,collab-net
//!
//! 参数走环境变量,默认指向那台云机:
//!   POLARIS_PROBE_NODE  对端 iroh NodeId
//!   POLARIS_PROBE_TOKEN 对端 owner 令牌
#![cfg(all(feature = "collab-host", feature = "collab-net"))]

/// 对端信息只从环境变量来,**代码里不留默认值** —— 这个仓是公开的,而 owner 令牌
/// 等于对端那台机器的钥匙(还带共享目录写权限)。日常用 `scripts/cloud-disk.ps1`
/// 会从 `%LOCALAPPDATA%\Polaris\mesh\peer.json` 自动补齐这几个变量。
fn env_required(key: &str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            eprintln!(
                "缺少环境变量 {key}。设 POLARIS_PROBE_NODE(<NodeId>@IP:端口)与 \
                 POLARIS_PROBE_TOKEN(对端 owner 令牌)后重跑;\
                 或把它们写进 %LOCALAPPDATA%\\Polaris\\mesh\\peer.json。"
            );
            std::process::exit(2);
        }
    }
}
const PORT: u16 = 18990;

fn check(step: &str, ok: bool, detail: impl std::fmt::Debug) {
    if ok {
        println!("PASS {step}");
    } else {
        println!("FAIL {step} —— {detail:?}");
        std::process::exit(1);
    }
}

/// 直接问对端(经隧道本地口),用来在盘符之外独立复核一遍。
fn upstream(path: &str, token: &str) -> Result<serde_json::Value, String> {
    ureq::get(&format!("http://127.0.0.1:{PORT}{path}"))
        .set("Authorization", &format!("Bearer {token}"))
        .timeout(std::time::Duration::from_secs(20))
        .call()
        .map_err(|e| format!("{e}"))?
        .into_json()
        .map_err(|e| format!("{e}"))
}

#[tokio::main]
async fn main() {
    // 本机 `~/Polaris/data` 目前是一个**指向自己**的 junction(2026-07-25 建的,自引用 =
    // 谁也进不去),iroh 的 device.key 与 collab.db 都落不进去。探针不动用户的磁盘,
    // 改用两个官方支持的环境变量把落点挪到临时目录 —— 这不影响链路本身的真实性:
    // 隧道、对端、盘符全是真的,只是本机密钥换了个存放处。
    let tmp = std::env::temp_dir().join("polaris-cloud-probe");
    std::fs::create_dir_all(&tmp).expect("建探针临时目录");
    if std::env::var("POLARIS_DEVICE_KEY").is_err() {
        std::env::set_var("POLARIS_DEVICE_KEY", tmp.join("device.key"));
    }
    if std::env::var("POLARIS_COLLAB_DB").is_err() {
        std::env::set_var("POLARIS_COLLAB_DB", tmp.join("collab.db"));
    }

    let node = env_required("POLARIS_PROBE_NODE");
    let token = env_required("POLARIS_PROBE_TOKEN");
    println!("对端 NodeId: {}…{}", &node[..12], &node[node.len() - 6..]);

    // ── ① 起 iroh 隧道 ──
    let n = node.clone();
    let t0 = std::time::Instant::now();
    let r = tokio::task::spawn_blocking(move || {
        polaris_app_lib::collab::tunnel::connect_client(&n, PORT)
    })
    .await
    .unwrap();
    check("iroh 隧道建立", r.is_ok(), &r);
    println!("     (握手 {:?})", t0.elapsed());

    // ── ② 隧道通不通:直接问对端要能力位 ──
    let mut caps = Err("还没试".to_string());
    for _ in 0..10 {
        caps = tokio::task::spawn_blocking({
            let t = token.clone();
            move || upstream("/api/fs/caps", &t)
        })
        .await
        .unwrap();
        if caps.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(900)).await;
    }
    check("经隧道读到对端能力位", caps.is_ok(), &caps);
    let caps = caps.unwrap();
    println!("     caps = {caps}");
    check(
        "对端报告可写(它开了 POLARIS_FS_WRITE)",
        caps["write"].as_bool() == Some(true),
        &caps,
    );

    // 记一下延迟:一次 list 往返的墙钟(广州 ↔ 本地)。
    let t0 = std::time::Instant::now();
    let _ = tokio::task::spawn_blocking({
        let t = token.clone();
        move || upstream("/api/fs/list?path=", &t)
    })
    .await
    .unwrap();
    println!("     一次目录列举往返:{:?}", t0.elapsed());

    // ── ③ 挂成本机盘符 ──
    let v = polaris_app_lib::fsmount::fs_mount(
        "cloud-probe".into(),
        "云服务器".into(),
        node.clone(),
        PORT,
        token.clone(),
    )
    .await
    .expect("fs_mount 不该报错");
    println!("fs_mount → {v}");
    let drive = v["drive"].as_str().unwrap_or("").to_string();
    check("挂上了盘符", !drive.is_empty(), &v);
    check("协商成读写盘", v["writable"].as_bool() == Some(true), &v);
    check(
        "桌面图标已建",
        v["shortcut"].as_str().map(|s| !s.is_empty()).unwrap_or(false),
        &v,
    );
    println!("     盘符 = {drive}  桌面图标 = {}", v["shortcut"]);

    // ── ④ 走资源管理器同一条路读 ──
    let listing = std::fs::read_dir(format!("{drive}\\")).map(|r| {
        r.flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>()
    });
    check(
        "盘符列出云端目录(含中文名)",
        listing
            .as_ref()
            .map(|v| v.iter().any(|n| n == "来自云服务器.txt"))
            .unwrap_or(false),
        &listing,
    );
    let got = std::fs::read_to_string(format!("{drive}\\来自云服务器.txt"));
    check(
        "盘符读云端文件内容",
        got.as_deref().map(|s| s.contains("云服务器")).unwrap_or(false),
        &got,
    );
    let t0 = std::time::Instant::now();
    let big = std::fs::read(format!("{drive}\\子目录\\3MB测试.bin"));
    check(
        "盘符读 3MB 文件",
        big.as_ref().map(|b| b.len()) .ok() == Some(3_000_000),
        big.as_ref().map(|b| b.len()),
    );
    println!("     3MB 下行耗时 {:?}", t0.elapsed());

    // ── ⑤ 往云端硬盘写 ──
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let name = format!("本机写上去的-{stamp}.txt");
    let body = format!("这行字是从 Windows 桌面经 iroh 隧道写进云服务器硬盘的({stamp})");
    let w = std::fs::write(format!("{drive}\\{name}"), body.as_bytes());
    check("往云端盘写文件", w.is_ok(), &w);

    // 用**对端自己的接口**复核(不看本地缓存):文件真在云服务器上。
    let back = tokio::task::spawn_blocking({
        let t = token.clone();
        let n = name.clone();
        move || {
            ureq::get(&format!(
                "http://127.0.0.1:{PORT}/api/fs/read?path={}",
                n.chars()
                    .map(|c| {
                        let mut b = [0u8; 4];
                        c.encode_utf8(&mut b)
                            .bytes()
                            .map(|x| format!("%{x:02X}"))
                            .collect::<String>()
                    })
                    .collect::<String>()
            ))
            .set("Authorization", &format!("Bearer {t}"))
            .timeout(std::time::Duration::from_secs(20))
            .call()
            .map_err(|e| format!("{e}"))
            .and_then(|r| r.into_string().map_err(|e| format!("{e}")))
        }
    })
    .await
    .unwrap();
    check("云端确认收到(内容逐字一致)", back.as_deref() == Ok(body.as_str()), &back);

    // 建目录 + 删除,把写面走全。
    let d = format!("{drive}\\本机建的目录-{stamp}");
    check("在云端盘建目录", std::fs::create_dir(&d).is_ok(), &d);
    check("删掉刚建的目录", std::fs::remove_dir(&d).is_ok(), &d);
    check(
        "删掉刚写的文件",
        std::fs::remove_file(format!("{drive}\\{name}")).is_ok(),
        &name,
    );

    // ── ⑥ 收尾 ──
    polaris_app_lib::fsmount::fs_unmount("cloud-probe".into())
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    check(
        "卸载后盘符消失",
        !std::path::Path::new(&format!("{drive}\\")).exists(),
        &drive,
    );
    println!("\nALL PASS —— 云服务器的硬盘在本机当过一块可读写的盘用了");
}
