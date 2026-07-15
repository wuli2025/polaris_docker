//! 向量车道(RAG)—— 文本 chunk → 嵌入 → SQLite 向量落库。
//!
//! 嵌入/重排走感官坞(sense.rs)当前生效的服务商:默认 硅基流动 BGE-M3 /
//! bge-reranker(钥匙②,免费)。PRD v5 §2.2「嵌入主路=硅基免费,本地 ONNX 兜底后续接入」。
//!
//! 工程姿势(PRD「巡夜人/滴灌」):一次构建只消化一个预算额(默认 4000 chunk),
//! 幂等续跑 —— files.chunked 标记位,断了再点继续;429 限速指数退避。

// 模块拆分(纯移动): 原 `crate::fable::index::xxx` 公有路径经 `pub use 子模块::*` 门面保持零变化,
// lib.rs generate_handler! / server.rs / retrieve.rs 等外部引用一律不用改。

pub mod build;
pub mod client;
pub mod commands;
pub mod dedupe;
pub mod ivf;
pub mod lexical;
pub mod local_embed;
pub mod math;

// 共享依赖统一在此升为 pub(crate) 供子模块 `use super::*` 取用(与原单文件同一作用域语义)。
// 深度加一层:fable 级符号在此再导出到 index 层,子文件的 `super::xxx` 零改动命中。
pub(crate) use super::{
    cancelled, lex_available, open_db, reencode_fs_path, FlagGuard, CANCEL, INDEXING,
};
pub(crate) use once_cell::sync::Lazy;
pub(crate) use serde::Serialize;
pub(crate) use serde_json::{json, Value};
pub(crate) use std::collections::{HashMap, VecDeque};
pub(crate) use std::sync::atomic::Ordering;
pub(crate) use std::sync::Mutex;
pub(crate) use std::time::Duration;

#[cfg(not(feature = "desktop"))]
pub(crate) use crate::host::AppHandle;
#[cfg(feature = "desktop")]
pub(crate) use tauri::{AppHandle, Emitter};

pub use build::*;
pub use client::*;
pub use commands::*;
pub use dedupe::*;
pub use ivf::*;
pub use lexical::*;
pub use local_embed::*;
pub use math::*;
