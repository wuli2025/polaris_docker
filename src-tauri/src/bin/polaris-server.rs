//! Docker(server) 二进制入口：起 axum HTTP/WS 服务，复用全部 Rust 引擎。
//! 构建：cargo build --release --bin polaris-server --no-default-features --features server

#[cfg(feature = "server")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 自检：docker 一键更新通道是否就绪（POLARIS_DOCKER_SOCKET=1 + bind mount /var/run/docker.sock）
    let updater_enabled = std::env::var("POLARIS_DOCKER_SOCKET").ok().as_deref() == Some("1");
    let socket_present = std::path::Path::new("/var/run/docker.sock").exists();
    let update_script = std::path::Path::new("/usr/local/bin/update.sh").exists();
    println!(
        "[polaris-server] docker 一键更新: enabled={} socket={} update.sh={}",
        updater_enabled, socket_present, update_script
    );

    if let Err(e) = polaris_app_lib::server::serve().await {
        eprintln!("[polaris-server] 致命错误: {e:#}");
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(not(feature = "server"))]
fn main() {
    eprintln!("polaris-server 需要 `--features server` 构建。");
}
