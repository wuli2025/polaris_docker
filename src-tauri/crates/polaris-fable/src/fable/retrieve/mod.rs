//! 塌平混检(神经层)—— grep 车道 ∥ 向量车道 并行 → RRF 融合 → 重排。
//!
//! PRD v5 §1「神经:四 tier 塌平混检」+ 用户拍板「grep 搜索和 RAG 并行,CPU 还很多」:
//! - **grep 车道**:多核 work-stealing 扫盘点表里的文本文件(字面/分词命中,零依赖
//!   零索引延迟 —— 盘点完成那一刻起就能搜,这就是 L1a「首小时全盘可搜」的搜);
//! - **向量车道**:查询嵌入 → 流式暴力余弦(SQLite 顺序读 vec BLOB,十万级亚秒;
//!   千万级在此函数内换 ANN,签名不变);
//! - 两车道 `thread::scope` 真并行,先到先等,RRF(k=60)塌平融合;
//! - 有重排服务商时对融合 top-40 精排一次,失败静默保持 RRF 序(可降级)。

// 模块拆分(纯移动): 原 `crate::fable::retrieve::xxx` 公有路径经 `pub use 子模块::*` 门面保持零变化,
// lib.rs generate_handler! / server.rs / chat / kb / eval 等外部引用一律不用改。

pub mod ai;
pub mod commands;
pub mod grep;
pub mod model;
pub mod params;
pub mod rerank_cache;
pub mod search;
pub mod text;
pub mod vector;

// 共享依赖统一在此升为 pub(crate) 供子模块 `use super::*` 取用(与原单文件同一作用域语义)。
// 深度加一层:fable 级符号在此再导出到 retrieve 层,子文件的 `super::xxx` 零改动命中
// (含把兄弟模块 `index` 再导出,让子文件里的 `super::index::hamming` 等路径原样成立)。
pub(crate) use super::index;
pub(crate) use super::{lex_available, open_db, open_db_gauged, worker_count};
pub(crate) use once_cell::sync::Lazy;
pub(crate) use serde::Serialize;
pub(crate) use std::collections::{HashMap, VecDeque};
pub(crate) use std::sync::atomic::{AtomicU64, Ordering};
pub(crate) use std::sync::Mutex;

pub(crate) use ai::*;
pub use commands::*;
pub(crate) use grep::*;
pub use model::*;
pub(crate) use params::*;
pub(crate) use rerank_cache::*;
pub use search::*;
pub(crate) use text::*;
pub(crate) use vector::*;
