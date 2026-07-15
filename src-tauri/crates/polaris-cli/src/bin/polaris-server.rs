//! Docker(server) 二进制入口：起 axum HTTP/WS 服务，复用全部 Rust 引擎。
//!
//! ★ 为什么住在 polaris-cli 工作区 crate 而非主包 [[bin]](47d1e0c 实证):
//!   tauri bundler 会按主包 [[bin]] 列表连坐 copy 二进制,desktop 默认特性下
//!   server 门控的 main 不存在 → bundler 报 "does not exist" 打包必炸。
//!   工作区成员的 bin 对 bundler 完全不可见,Win+Mac 桌面包零影响。
//!
//! 构建：cargo build -p polaris-cli --release
//!       (同时产出 polaris-forge 与 polaris-server,server feature 经依赖恒开)

fn main() {
    // CPU 防卡死:显式限住线程池,别让重负载请求把线程数撑爆。
    // worker_threads 默认=可用核数(cgroup cpuset 下 available_parallelism 会读到绑核数);
    // max_blocking_threads 默认 512 太大 —— 所有同步命令(检索/盘点触发等)走 spawn_blocking,
    // 并发重请求会瞬间拉起几百个 OS 线程争 CPU,收到 64 足够并发又不膨胀。
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(cores.max(2))
        .max_blocking_threads(64)
        .enable_all()
        .build()
        .expect("[polaris-server] tokio runtime 初始化失败");
    if let Err(e) = rt.block_on(polaris_app_lib::server::serve()) {
        eprintln!("[polaris-server] 致命错误: {e:#}");
        std::process::exit(1);
    }
}
