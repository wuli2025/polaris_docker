//! polaris-wiki — llmwiki 知识网构建域(分仓规划 v2 第 6 仓·v2 新增)。
//!
//! 构建管线 wiki/compile.rs(原 kb/compile.rs)从主 crate 原样搬入(git mv 保历史)。
//! 模块内部 `crate::kb::…`/`crate::doctor::…` 等旧路径由本文件的别名原位保住。

pub mod wiki;

// crate 根别名: 3→2 向下依赖的旧路径。
pub use polaris_fable::kb;
pub use polaris_kernel::{doctor, provider};
pub use polaris_runtime as runtime;
pub use polaris_runtime::host;
