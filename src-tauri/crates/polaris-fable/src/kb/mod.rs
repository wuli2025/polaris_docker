//! 板块 ② 维基知识库 — MVP 实现
//!
//! 设计依据: PRD-v6 §8 + v5.1 §3-§7
//! - 三层目录铁律: raw/ output/ wiki/ (新建空 KB 时创建)
//! - 关键词加权评分搜索 (PRD §8.8): 标题 +10, 课程标签 +8, 正文 +1
//! - 双链 [[wiki-link]] 解析 -> 图谱节点+边
//! - YAML frontmatter 提取 category (PRD §8.5)
//!
//! MVP 缩水:
//! - 不做 Embedding (Karpathy 论点: 结构化 wiki + 长上下文 > 向量)
//! - 不做 SimHash 去重 (留 §8.6, 后续接入)
//! - 索引常驻内存, 进程重启时重扫 (后续走 SQLite)

// 模块拆分(纯移动): 原 `crate::kb::xxx` 公有路径经 `pub use 子模块::*` 门面保持零变化,
// lib.rs generate_handler! 与 server/echo/fable 等外部引用一律不用改。

pub mod packs;
pub mod scan;
// 知识网构建管线(原 kb/compile.rs)已归位 polaris-wiki crate;调用方已切 `wiki::kb_compile`
// (fable 仓不得向上依赖 wiki —— 依赖层级 3→2 只许 wiki 向下)。
pub mod access;
pub mod enrich;
pub mod graph;
pub mod ingest;
pub mod maintain;
pub mod search;
pub mod threat;

// headless claude 只读管线已下沉 polaris-kernel::headless(fable/wiki 共用);
// 此处转发保住 `kb::run_claude_readonly` 等全部旧调用路径(含 enrich 的 `use super::*`)。
pub use polaris_kernel::headless::{
    extract_balanced_json, run_claude_readonly, run_claude_readonly_timeout,
};

// 共享依赖统一在此升为 pub(crate) 供子模块 `use super::*` 取用(与原单文件同一作用域语义)。
pub(crate) use crate::convert;
#[cfg(not(feature = "desktop"))]
pub(crate) use crate::host::AppHandle;
pub(crate) use anyhow::Result;
pub(crate) use directories::{ProjectDirs, UserDirs};
pub(crate) use once_cell::sync::Lazy;
pub(crate) use parking_lot::RwLock;
pub(crate) use regex::Regex;
pub(crate) use serde::{Deserialize, Serialize};
pub(crate) use std::collections::HashMap;
pub(crate) use std::fs;
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::sync::atomic::Ordering;
pub(crate) use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(feature = "desktop")]
pub(crate) use tauri::{AppHandle, Emitter, Manager};
pub(crate) use walkdir::WalkDir;

pub use access::*;
pub use enrich::*;
pub use graph::*;
pub use ingest::*;
pub use maintain::*;
pub use packs::*;
pub use scan::*;
pub use search::*;
pub use threat::*;
