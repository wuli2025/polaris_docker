//! 文件中心(File Center)—— 把盘点表里的散乱文件「同类放一起」可视化。
//!
//! 设计承《文件中心-PRD》:
//! - 归类逻辑三层:① 类型+文件夹+时间(零成本兜底)② 语义聚类(复用已存向量,
//!   零新增嵌入调用,本文件主轴)③ 双链关系(kb_graph 已有,前端另接);
//! - 「展示出来好看」:缩略图/首帧/类型图标。缩略图统一以 data URL 返回(三壳同构,
//!   桌面/Docker/Web 都无需 asset 协议或文件服务),磁盘缓存避免重复解码;
//! - 「内容速览」:按需 + 缓存的本地抽取式 gist(零 token,默认不调 LLM)。
//!
//! 铁律(与 fable 其余模块同构):AI 出决策、代码执行;单一事实源 fable.db;
//! 全部命令同步、无 AppHandle 依赖 → 桌面 / Docker / CLI 三壳共用同一份。

// 模块拆分(纯移动): 原 `crate::fable::files::xxx` 公有路径经 `pub use 子模块::*` 门面保持零变化,
// lib.rs generate_handler! 与 server.rs 等外部引用一律不用改。

pub mod cluster;
pub mod commands;
pub mod gist;
pub mod graph;
pub mod llm;
pub mod overview;
pub mod profile;
#[cfg(test)]
mod tests;
pub mod thumbs;

// 共享依赖统一在此升为 pub(crate) 供子模块 `use super::*` 取用(与原单文件同一作用域语义)。

pub(crate) use super::{open_db, worker_count, FlagGuard};
pub(crate) use serde::{Deserialize, Serialize};
pub(crate) use serde_json::{json, Value};
pub(crate) use std::collections::HashMap;
pub(crate) use std::hash::{Hash, Hasher};
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
pub(crate) use std::sync::Mutex;
pub(crate) use std::time::Duration;

#[cfg(not(feature = "desktop"))]
pub(crate) use crate::host::AppHandle;
#[cfg(feature = "desktop")]
pub(crate) use tauri::{AppHandle, Emitter};

pub use cluster::*;
pub use commands::*;
pub use gist::*;
pub(crate) use graph::*; // graph 子模块目前只有 pub(crate) 项(build_file_graph)
pub use llm::*;
pub use overview::*;
pub use profile::*;
pub use thumbs::*;
