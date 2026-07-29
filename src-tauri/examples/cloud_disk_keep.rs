//! 把**云服务器的硬盘**挂成本机一块可读写的盘符,并**一直挂着**(不像 `cloud_disk_probe`
//! 那样跑完就卸载)。
//!
//! 为什么需要一个常驻进程:WebDAV 桥和 iroh 隧道都活在进程里 —— 进程一退,桥没了,
//! 盘符就变成死挂载。桌面应用开着时这份活由应用自己干;在应用之外想让盘一直可用,
//! 就得有这么一个守着的小程序。
//!
//! 跑法:
//!   cargo run --release --example cloud_disk_keep
//! 停:Ctrl+C(会先卸盘再退),或删掉 stop 文件旁的进程。
//!
//! 环境变量:
//!   POLARIS_PROBE_NODE   对端 iroh NodeId
//!   POLARIS_PROBE_TOKEN  对端 owner 令牌
//!   POLARIS_MESH_HOME    本机身份落点(device.key / collab.db);默认 %LOCALAPPDATA%\Polaris\mesh
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
const SOURCE_ID: &str = "cloud-disk";

fn stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{:02}:{:02}:{:02}", secs / 3600 % 24, secs / 60 % 60, secs % 60)
}

macro_rules! say {
    ($($a:tt)*) => {{
        println!("[{}] {}", stamp(), format!($($a)*));
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }};
}

#[tokio::main]
async fn main() {
    // 本机身份(device.key/collab.db)不落 ~/Polaris/data —— 那是个自引用 junction,
    // 谁也写不进去。放一个确定存在的目录,这样每次重挂用的是同一个设备身份。
    let home = std::env::var("POLARIS_MESH_HOME").map(std::path::PathBuf::from).unwrap_or_else(|_| {
        std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into()))
            .join("Polaris")
            .join("mesh")
    });
    std::fs::create_dir_all(&home).expect("建身份目录");
    if std::env::var("POLARIS_DEVICE_KEY").is_err() {
        std::env::set_var("POLARIS_DEVICE_KEY", home.join("device.key"));
    }
    if std::env::var("POLARIS_COLLAB_DB").is_err() {
        std::env::set_var("POLARIS_COLLAB_DB", home.join("collab.db"));
    }
    say!("身份落点 {}", home.display());

    let node = env_required("POLARIS_PROBE_NODE");
    let token = env_required("POLARIS_PROBE_TOKEN");
    say!("对端 NodeId {}…{}", &node[..12], &node[node.len() - 6..]);

    // ① iroh 隧道
    let t0 = std::time::Instant::now();
    let n = node.clone();
    let r = tokio::task::spawn_blocking(move || polaris_app_lib::collab::tunnel::connect_client(&n, PORT))
        .await
        .unwrap();
    if let Err(e) = &r {
        say!("隧道建立失败:{e}");
        std::process::exit(1);
    }
    say!("隧道已建(握手 {:?}),本地口 127.0.0.1:{PORT}", t0.elapsed());

    // ② 等对端应答(打洞握手要几秒)
    let mut caps = Err("还没试".to_string());
    for _ in 0..12 {
        caps = tokio::task::spawn_blocking({
            let t = token.clone();
            move || {
                ureq::get(&format!("http://127.0.0.1:{PORT}/api/fs/caps"))
                    .set("Authorization", &format!("Bearer {t}"))
                    .timeout(std::time::Duration::from_secs(20))
                    .call()
                    .map_err(|e| format!("{e}"))?
                    .into_json::<serde_json::Value>()
                    .map_err(|e| format!("{e}"))
            }
        })
        .await
        .unwrap();
        if caps.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(900)).await;
    }
    match &caps {
        Ok(c) => say!("对端能力位 {c}"),
        Err(e) => {
            say!("对端没应答:{e}");
            std::process::exit(1);
        }
    }

    // ③ 挂盘
    let v = match polaris_app_lib::fsmount::fs_mount(
        SOURCE_ID.into(),
        "云服务器".into(),
        node.clone(),
        PORT,
        token.clone(),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            say!("挂载失败:{e}");
            std::process::exit(1);
        }
    };
    let drive = v["drive"].as_str().unwrap_or("").to_string();
    say!(
        "已挂 {}  可写={}  桌面图标={}",
        if drive.is_empty() { "(看门狗补挂中)" } else { &drive },
        v["writable"],
        v["shortcut"]
    );

    // 冒烟:走盘符列一次根目录,证明资源管理器这条路是通的。
    if !drive.is_empty() {
        match std::fs::read_dir(format!("{drive}\\")) {
            Ok(rd) => {
                let names: Vec<_> = rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
                say!("盘内根目录 {} 项:{:?}", names.len(), names);
            }
            Err(e) => say!("盘符读取异常(看门狗会重试):{e}"),
        }
    }

    say!("盘已就位 —— 这个进程只要开着,盘就一直在。Ctrl+C 卸载退出。");

    // ④ 守着:每 60s 报一次状态,Ctrl+C 时先卸盘。
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
    tick.tick().await;
    loop {
        tokio::select! {
            _ = tick.tick() => {
                for s in polaris_app_lib::fsmount::fs_mount_status() {
                    say!("心跳 drive={} ok={} writable={} err={}", s["drive"], s["ok"], s["writable"], s["error"]);
                }
            }
            _ = tokio::signal::ctrl_c() => {
                say!("收到 Ctrl+C,卸载中…");
                let _ = polaris_app_lib::fsmount::fs_unmount(SOURCE_ID.into()).await;
                tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                say!("已卸载,退出。");
                return;
            }
        }
    }
}
