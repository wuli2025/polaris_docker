//! 盘点引擎(L1a)—— 多线程并行全盘扫描 → SQLite。
//!
//! PRD v5 §7「P0.5 盘点+L1a:首小时全盘可搜」。设计:
//! - N 个 walker 线程(共享目录栈,work-stealing)只做 read_dir + stat,吃满多核;
//! - 1 个 writer 线程独占写连接,2000 行一个事务批量落库(SQLite 写入瓶颈在事务数);
//! - 「seen 代际」机制:全量重扫后自动清掉已消失文件(及其 chunks),幂等可重入;
//! - mtime/size 没变的文件保留 chunked 标记 → 重扫不会废掉已建好的向量索引。

// 模块拆分(纯移动): 原 `crate::fable::inventory::xxx` 公有路径经 `pub use 子模块::*` 门面保持零变化,
// lib.rs generate_handler! / server.rs 等外部引用一律不用改。

pub mod audit;
pub mod classify;
pub mod commands;
pub mod folders;
pub mod prune;
pub mod scan;
#[cfg(test)]
mod tests;

// 共享依赖统一在此升为 pub(crate) 供子模块 `use super::*` 取用(与原单文件同一作用域语义)。
pub(crate) use super::sched::WorkQueue;
pub(crate) use super::{cancelled, open_db, worker_count, FlagGuard, CANCEL, SCANNING};
pub(crate) use super::{decode_fs, lex_available, reencode_fs_path};
pub(crate) use super::{index, sched};
pub(crate) use serde::Serialize;
pub(crate) use serde_json::{json, Value};
pub(crate) use std::collections::HashSet;
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
pub(crate) use std::sync::{mpsc, Arc, Mutex};
pub(crate) use std::time::{Duration, Instant};

#[cfg(not(feature = "desktop"))]
pub(crate) use crate::host::AppHandle;
#[cfg(feature = "desktop")]
pub(crate) use tauri::{AppHandle, Emitter};

pub use audit::*;
pub(crate) use classify::*;
pub use commands::*;
pub use folders::*;
pub(crate) use prune::*;
pub use scan::*;
