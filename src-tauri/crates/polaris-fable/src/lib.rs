//! polaris-fable — 检索/KB 引擎(分仓规划 v2 第 3 仓的仓内形态)。
//!
//! 源码从主 crate 原样搬入(git mv 保历史): fable/ + kb/。模块内部 `crate::…`
//! 旧路径由本文件的模块声明与别名原位保住 —— 与主 crate Phase 0 的「crate 根别名」
//! 同一手法。chat(kernel)对本引擎的调用不走这里, 走 chat::bridges 的壳侧注入。

pub mod fable;
pub mod kb;

// crate 根别名: echo/sense/scan 的 Phase 0 归位旧路径(`crate::sense::…` 等)。
pub use fable::{echo, scan, sense};
// 向下依赖(conv 转写/skills 装载/convert 转换/横切基建)的旧路径别名。
pub use polaris_kernel::{conv, convert, skills};
pub use polaris_runtime as runtime;
pub use polaris_runtime::host;
