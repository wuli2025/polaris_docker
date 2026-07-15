//! polaris-forge — 成品渲染引擎(分仓规划 v2 第 4 仓的仓内形态)。
//!
//! 源码从主 crate `src/forge/` 原样搬入(git mv 保历史), 模块内部 `crate::forge::…`
//! 与 `crate::figma_bridge`/`crate::runtime` 旧路径由本文件的别名原位保住 —— 与主 crate
//! Phase 0 的「crate 根别名」同一手法, 搬迁对模块体零改动。
//! 壳侧经 `pub use polaris_forge::forge;` 别名, generate_handler!/dispatch 路径不变。

pub mod forge;

// crate 根别名: 保 `crate::figma_bridge::…`(Phase 0 文件归位时的旧路径)。
pub use forge::figma_bridge;
// 保 `crate::runtime::…`(横切基建经独立 crate 引入)。
pub use polaris_runtime as runtime;
