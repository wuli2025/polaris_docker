//! polaris-kernel — 核心内核(分仓规划 v2 第 2 仓的仓内形态)。
//!
//! 源码从主 crate 原样搬入(git mv 保历史): chat/ provider/ doctor/ skills/
//! integrations/ + conv.rs + claude_md.rs。模块内部 `crate::…` 旧路径由本文件的
//! 模块声明与别名原位保住 —— 与主 crate Phase 0 的「crate 根别名」同一手法。
//!
//! 对引擎(kb/fable/expert)的依赖全部收口于 `chat::bridges` 的 trait 注入,
//! 壳侧 `wiring::wire_engine_bridges()` 拼装;未注入时优雅降级(极简拼装形态)。

pub mod chat;
pub mod claude_md;
pub mod conv;
pub mod convert;
pub mod doctor;
pub mod headless;
pub mod integrations;
pub mod provider;
pub mod skills;

// crate 根别名: 保 `crate::runtime::…` 与 `crate::host::…`(横切基建/双壳事件 shim)。
pub use polaris_runtime as runtime;
pub use polaris_runtime::host;
