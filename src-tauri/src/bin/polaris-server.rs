//! Docker(server) 二进制入口：起 axum HTTP/WS 服务，复用全部 Rust 引擎。
//! 构建：cargo build --release --bin polaris-server --no-default-features --features server

#[cfg(feature = "server")]
fn main() -> anyhow::Result<()> {
    // CPU 防卡死:显式限住 tokio 线程池(原 #[tokio::main] 默认 max_blocking_threads=512 太大)。
    // 所有同步命令(检索/盘点触发等)走 spawn_blocking,并发重请求会瞬间拉起几百个 OS 线程争 CPU。
    // worker_threads 默认=可用核数(cgroup cpuset 下 available_parallelism 读到绑核数);blocking 收到 64。
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(cores.max(2))
        .max_blocking_threads(64)
        .enable_all()
        .build()?;
    rt.block_on(async {
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
    })
}

#[cfg(not(feature = "server"))]
fn main() {
    eprintln!("polaris-server 需要 `--features server` 构建。");
}
