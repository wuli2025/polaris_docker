//! llmwiki 库 · 知识网构建域 (分仓规划 v2 · 第 12 仓 polaris-wiki 的仓内雏形)
//!
//! 定位: 检索是「读」, 这里是「写的那一半」—— 摄入资料时让 LLM 抽实体/概念、headless
//! 写 wiki 词条、维护双链知识网(Karpathy LLM-Wiki 思路)。与检索引擎(kb/fable)的关系:
//! wiki 骑在检索之上(依赖层级 3→2), 是全体系唯一获批依赖其他引擎的板块。
//!
//! Phase 1: 已抽为独立 crate(polaris-wiki), 调用方一律走 `wiki::kb_compile`
//! (下方 glob 门面含 `#[tauri::command]` 生成的 __cmd__ 宏, 供壳仓 generate_handler!)。
//! 词条数据本体在用户数据目录(~/Polaris), 不随代码走。

pub mod compile;

pub use compile::*;
