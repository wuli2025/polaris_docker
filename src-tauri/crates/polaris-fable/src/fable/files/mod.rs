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

pub(crate) use super::{cluster_cancelled, open_db, worker_count, FlagGuard, CANCEL_CLUSTER};
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

// ───────────────────────── 路径闸(文件中心专用)─────────────────────────
//
// `file_thumb` / `file_gist` 收的是**调用方给的绝对路径**,而这两条命令都挂在 apihub 的
// 「手机数据面」白名单里(见 apihub.rs 的 dispatch_desktop),经 iroh 隧道能被远端手机调,
// Docker/NAS server 壳更是默认免口令、局域网谁都够得着。此前它们只判 `is_file()`,
// 于是「给个绝对路径 = 读走本机任意图片 / 任意文本文件的前 8000 字」——
// 2026-07-29 压测经 127.0.0.1:8485 数据面实测:桌面上的探针文件被 file_gist 原文取回,
// 而同一个文件走 artifact_read 被正确拒绝(它早有 ensure_artifact_path 那道闸)。
// 这里给文件中心补齐同款闸:只放行「已盘点的根」与知识库根之下的文件 —— 那正是文件中心
// 本来就该看得见的范围,合法用法一个不受影响。

/// 文件中心允许访问的根:已盘点的 fable roots + 知识库根。
/// 结果缓存 2s —— 缩略图网格一屏就要校验上百个路径,每次开库查 roots 太亏;
/// 盘点新增根后最多 2s 生效,对交互无感。
fn allowed_file_roots() -> Vec<PathBuf> {
    static CACHE: Mutex<Option<(std::time::Instant, Vec<PathBuf>)>> = Mutex::new(None);
    // 单测里各用例会来回换 POLARIS_FABLE_DB,缓存会串库 → 测试下不缓存。
    if !cfg!(test) {
        if let Ok(g) = CACHE.lock() {
            if let Some((at, v)) = g.as_ref() {
                if at.elapsed() < Duration::from_secs(2) {
                    return v.clone();
                }
            }
        }
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(conn) = open_db() {
        if let Ok(mut stmt) = conn.prepare("SELECT path FROM roots") {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                roots.extend(rows.flatten().map(PathBuf::from));
            }
        }
    }
    let kb = crate::kb::kb_root();
    if !kb.is_empty() {
        roots.push(PathBuf::from(kb));
    }
    // 统一 canonicalize:比较两端都规范化才不会被 `..`、8.3 短名、符号链接绕过。
    let roots: Vec<PathBuf> = roots
        .into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect();
    if let Ok(mut g) = CACHE.lock() {
        *g = Some((std::time::Instant::now(), roots.clone()));
    }
    roots
}

/// 文件中心路径闸:把调用方给的路径规范化,并要求它落在 [`allowed_file_roots`] 之下。
/// 越界返回 Err(与 artifact_read 同一句措辞),不存在返回 Ok(None) 由调用方按原语义处置。
pub(crate) fn ensure_file_center_path(abspath: &str) -> Result<Option<PathBuf>, String> {
    // 显示路径可能是 GBK 名解码出的 UTF-8,先还原成磁盘真实路径(与旧行为一致)。
    let real = crate::fable::reencode_fs_path(abspath);
    let Ok(canon) = real.canonicalize() else {
        return Ok(None); // 不存在 / 读不到:交回调用方(缩略图给 None、速览报「文件不存在」)
    };
    let roots = allowed_file_roots();
    // 一个根都没有(还没盘点过)时不放行:宁可文件中心暂时空着,也不开天窗。
    if roots.iter().any(|r| canon.starts_with(r)) {
        Ok(Some(canon))
    } else {
        Err("路径越界, 拒绝访问".into())
    }
}

/// 同一道闸的对外只读版:路径是否在文件中心的可见范围内。
/// 给 apihub 的远端数据面复用(`chat_attach_files` 的附件路径闸),那边只需要一个是非判断。
/// 路径不存在同样判否 —— 调用方要的是「能不能碰」,不存在的路径本就无从放行。
pub fn file_center_path_allowed(abspath: &str) -> bool {
    matches!(ensure_file_center_path(abspath), Ok(Some(_)))
}

pub use cluster::*;
pub use commands::*;
pub use gist::*;
pub(crate) use graph::*; // graph 子模块目前只有 pub(crate) 项(build_file_graph)
pub use llm::*;
pub use overview::*;
pub use profile::*;
pub use thumbs::*;
