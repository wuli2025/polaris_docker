//! 知识库维护同步原语(原 kb/compile.rs 的 Wave B 基础设施, 分仓 Phase 1 归位 kb):
//! compile(wiki 仓) / enrich_links / dedup 三者都 spawn 后台线程改写同一批 wiki 文件,
//! 共用这把全局互斥与原子落盘。`pub`: wiki 构建管线(3→2 依赖)跨 crate 取用。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub static KB_COMPILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 知识库维护互斥: compile / enrich_links / dedup 并发跑会互相覆盖(lost update)
/// 甚至 dedup 删文件时 enrich 正在写它。用一个全局忙标志串行化,
/// RAII guard 在线程结束(Drop)时自动释放。
pub static KB_TASK_BUSY: AtomicBool = AtomicBool::new(false);

pub struct KbTaskGuard;
impl Drop for KbTaskGuard {
    fn drop(&mut self) {
        KB_TASK_BUSY.store(false, Ordering::SeqCst);
    }
}

/// 抢占维护锁; 已有任务在跑则返回 Err(前端可提示稍候)。把返回的 guard `move` 进后台线程,
/// 线程跑完(正常/出错/panic)都会 Drop 释放。
pub fn acquire_kb_task() -> Result<KbTaskGuard, String> {
    if KB_TASK_BUSY
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("已有知识库维护任务在运行, 请等它结束后再试".into());
    }
    Ok(KbTaskGuard)
}

/// KB 内容原子落盘: 临时文件 + rename(同卷原子)。dedup/enrich 改写 wiki 页时若裸 fs::write
/// 中途崩溃会把页面截成半截, 丢失 AI/用户内容。统一走这里。
pub fn kb_atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".polaris.tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}
