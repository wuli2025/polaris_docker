//! polaris-collab — 多人协作引擎(分仓规划 v2 第 5 仓的仓内形态)。
//!
//! 源码从主 crate `src/collab/` 原样搬入(git mv 保历史), 壳件 apihub/hosting 归位
//! 壳仓。模块内部 `crate::…` 旧路径由本文件的别名原位保住(Phase 1 同一手法)。

pub mod collab;

// crate 根别名: 向下依赖的旧路径(checks/http 用 skills, http 用 host shim)。
pub use polaris_kernel::skills;
pub use polaris_runtime as runtime;
pub use polaris_runtime::host;
