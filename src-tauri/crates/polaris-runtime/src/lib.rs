//! 横切基建(runtime):路径解析 / 子进程池与超时看门 / HTTP 客户端的唯一入口。
//! 双壳(desktop/server)共用,不依赖 tauri,任何模块都可安全引用。
//!
//! 分仓规划 v2 · 目录→crate 的第一块:独立 crate 后边界由编译器物理保证
//! (本 crate 认不得任何业务板块,业务板块经主 crate 的 `pub use polaris_runtime as runtime;`
//! 别名照旧走 `crate::runtime::…` 路径,调用方零改动)。
// host shim(双壳事件广播 AppHandle 替身): server 壳与桌面内嵌协作主机共用,
// 引擎 crate 在 `cfg(not(feature = "desktop"))` 下经各自根别名取用。
pub mod host;
pub mod http;
pub mod paths;
pub mod procs;

pub use procs::run_with_timeout;
