//! uv 缓存治理 env_uv_cache_info / env_uv_cache_clean (纯移动)。

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::probe::*;
use super::types::*;
use crate::runtime::procs::no_window;

/// 人类可读字节数。
fn human_bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} {}", U[0])
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// 递归统计目录字节数 (跟随符号链接会有重复风险, 故不跟随; 仅用于缓存大小估算, 不求绝对精确)。
fn dir_size(p: &std::path::Path) -> u64 {
    let mut total = 0u64;
    let Ok(rd) = std::fs::read_dir(p) else {
        return 0;
    };
    for ent in rd.flatten() {
        let Ok(ft) = ent.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            total = total.saturating_add(dir_size(&ent.path()));
        } else if let Ok(md) = ent.metadata() {
            total = total.saturating_add(md.len());
        }
    }
    total
}

/// uv 缓存目录及占用大小 —— 给「环境医生」展示 + 决定是否提示清理(uv 缓存放任会涨到数 GB)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn env_uv_cache_info() -> UvCacheInfo {
    let Some(uv) = resolve_uv_exe() else {
        return UvCacheInfo {
            available: false,
            dir: None,
            bytes: 0,
            human: "0 B".into(),
        };
    };
    // `uv cache dir` 打印缓存目录路径
    let mut cmd = Command::new(&uv);
    cmd.args(["cache", "dir"]);
    cmd.stdin(Stdio::null());
    no_window(&mut cmd);
    let dir = output_with_timeout(cmd, Duration::from_secs(20))
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim())
                .find(|l| !l.is_empty())
                .map(|s| s.to_string())
        });
    let (bytes, dir_str) = match &dir {
        Some(d) => {
            let pb = PathBuf::from(d);
            (
                if pb.exists() { dir_size(&pb) } else { 0 },
                Some(to_fwd(&pb)),
            )
        }
        None => (0, None),
    };
    UvCacheInfo {
        available: true,
        dir: dir_str,
        bytes,
        human: human_bytes(bytes),
    }
}

/// 清理 uv 缓存 (`uv cache clean`) —— 释放空间。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn env_uv_cache_clean() -> Result<String, String> {
    let uv = resolve_uv_exe().ok_or_else(|| "未找到 uv (请先安装)。".to_string())?;
    let mut cmd = Command::new(&uv);
    cmd.args(["cache", "clean"]);
    cmd.stdin(Stdio::null());
    no_window(&mut cmd);
    let out = output_with_timeout(cmd, Duration::from_secs(120))
        .ok_or_else(|| "`uv cache clean` 执行超时或无法启动。".to_string())?;
    if out.status.success() {
        Ok("uv 缓存已清理。".to_string())
    } else {
        Err(format!(
            "清理失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}
