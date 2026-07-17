//! 板块 ⑥ API 供应商坞 — Claude Code 供应商切换 + token 用量/成本看板
//!
//! 剥离自 cc-switch 的 Claude 供应商能力, 与 Polaris 墨蓝水墨前端融为一体。
//! - 每个供应商携带一份完整 `settings_config`(env + includeCoAuthoredBy/attribution
//!   等顶层键)。
//! - 联动/隔离两档(store.link_global, 默认**隔离**):
//!   * 隔离 — 切换只写 Polaris 进程 env(spawn 的 claude 子进程继承, 且进程 env 实测
//!     优先于 settings.json), 终端里用户自己的 `claude` 完全不受影响 —— 根治
//!     「Polaris 切 MiniMax 把外部 CLI 一起带跑」的串台。
//!   * 联动 — 行为同旧版: 额外把 settings_config 合并写进 `~/.claude/settings.json`
//!     (只接管我们管理的键, 其余原样保留; 首次改动前 .polaris.bak 备份),
//!     终端 CLI 跟着 Polaris 一起切。
//! - 用量看板: 读 `~/.claude/projects/**/*.jsonl`(ccusage 思路), 聚合 token + 按内置
//!   定价表估算成本, 今日/周/月/年 + 14 天趋势。零额外网络、零额外依赖。
//! - Codex / Copilot: 说 OpenAI 协议, 让 `claude` 直连需翻译代理(cc-switch 的 proxy/,
//!   1.5MB+), 与轻量化冲突 → 不路由。Codex 授权委托官方 `codex` CLI。

// 模块拆分(纯移动): 原 `crate::provider::xxx` 公有路径经 `pub use 子模块::*` 门面保持零变化,
// lib.rs generate_handler! 与 server / chat / kb / integrations 等外部引用一律不用改
// (tauri 宏生成的 __cmd__xxx 项随 glob 一并带出)。子文件统一 `use super::*`。

pub mod claude_login;
pub mod codex_login;
/// 生图供应商坞 —— **独立于 store.rs 那张聊天表**(理由见文件头, 别合并)。
pub mod image_store;
pub mod oauth_loopback;
pub mod store;
pub mod usage;

// 共享依赖统一在此升为 pub(crate) 供子模块 `use super::*` 取用(与原单文件同一作用域语义)。
#[cfg(not(feature = "desktop"))]
pub(crate) use crate::host::AppHandle;
pub(crate) use anyhow::Result;
pub(crate) use directories::UserDirs;
pub(crate) use once_cell::sync::Lazy;
pub(crate) use parking_lot::RwLock;
pub(crate) use serde::{Deserialize, Serialize};
pub(crate) use serde_json::{json, Map, Value};
pub(crate) use std::collections::{HashMap, HashSet};
pub(crate) use std::fs;
pub(crate) use std::io::{BufRead, BufReader};
pub(crate) use std::net::{TcpListener, TcpStream};
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::process::{Command, Stdio};
pub(crate) use std::sync::atomic::{AtomicBool, Ordering};
pub(crate) use std::sync::Arc;
pub(crate) use std::thread;
pub(crate) use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
#[cfg(feature = "desktop")]
pub(crate) use tauri::AppHandle;
pub(crate) use walkdir::WalkDir;

// 构建期注入的「粉丝福利」MiniMax key(XOR 滚动混淆字节, 见 build.rs)。
include!(concat!(env!("OUT_DIR"), "/gift_key.rs"));

// ───────────────────────── 工具函数 ─────────────────────────

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn today_utc_days() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (secs / 86400) as i64
}

/// 天数 → YYYY-MM-DD (Howard Hinnant civil_from_days, 无外部依赖)
fn ymd_string(z: i64) -> String {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + if m <= 2 { 1 } else { 0 };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

pub use claude_login::*;
pub use codex_login::*;
pub use image_store::*;
pub use oauth_loopback::*;
pub use store::*;
pub use usage::*;
