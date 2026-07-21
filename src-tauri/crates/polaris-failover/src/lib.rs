//! 供应商故障转移滑窗(纯逻辑)。
//!
//! 语义(沿袭 mica PRD 5.5):
//! - 5 分钟滑动窗口内累计第 3 次失败 → `record_failure` 返回 true, 表示该切备用;
//!   同时清空该 id 的计数(避免第 4、5 次失败连环触发多次切换)。
//! - `record_success` 清零: 偶发抖动(窗口内 1~2 次失败后恢复)不积累。
//! - **不自动切回**(防抖动/乒乓): 本 crate 根本没有「恢复」概念, 切回由用户确认,
//!   这是刻意的产品决策而非功能缺失。
//! - 可注入时钟: 测试拨假时钟, 不 sleep。

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 时钟抽象(同 polaris-watchdog 的取向: 闭包省一层 trait)。
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

pub const DEFAULT_WINDOW: Duration = Duration::from_secs(300);
pub const DEFAULT_THRESHOLD: usize = 3;

pub struct FailoverTracker {
    window: Duration,
    threshold: usize,
    clock: Clock,
    /// id → 窗口内失败时刻列表。Vec 而非环形队列: 阈值只有 3, retain 开销可忽略。
    failures: Mutex<HashMap<String, Vec<Instant>>>,
}

impl Default for FailoverTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl FailoverTracker {
    pub fn new() -> Self {
        Self::with_clock(DEFAULT_WINDOW, DEFAULT_THRESHOLD, Arc::new(Instant::now))
    }

    pub fn with_clock(window: Duration, threshold: usize, clock: Clock) -> Self {
        Self {
            window,
            threshold: threshold.max(1),
            clock,
            failures: Mutex::new(HashMap::new()),
        }
    }

    /// 记一次(网络/鉴权/欠费类)失败。返回 true = 窗口内达到阈值, 该切备用了。
    /// 触发即清零该 id: 一次阈值只提示一次切换。
    pub fn record_failure(&self, id: &str) -> bool {
        let now = (self.clock)();
        let mut map = self.failures.lock();
        let list = map.entry(id.to_string()).or_default();
        // 窗口滑动: 只留窗口内的失败
        list.retain(|t| now.saturating_duration_since(*t) < self.window);
        list.push(now);
        if list.len() >= self.threshold {
            list.clear();
            true
        } else {
            false
        }
    }

    /// 请求成功: 该供应商计数清零(偶发抖动不积累)。
    pub fn record_success(&self, id: &str) {
        self.failures.lock().remove(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fake_tracker() -> (FailoverTracker, Arc<AtomicU64>) {
        let base = Instant::now();
        let offset = Arc::new(AtomicU64::new(0));
        let o = offset.clone();
        let clock: Clock = Arc::new(move || base + Duration::from_secs(o.load(Ordering::SeqCst)));
        (
            FailoverTracker::with_clock(DEFAULT_WINDOW, DEFAULT_THRESHOLD, clock),
            offset,
        )
    }

    #[test]
    fn third_failure_in_window_triggers() {
        let (tr, t) = fake_tracker();
        assert!(!tr.record_failure("a"));
        t.store(60, Ordering::SeqCst);
        assert!(!tr.record_failure("a"));
        t.store(120, Ordering::SeqCst);
        assert!(tr.record_failure("a")); // 5 分钟内第 3 次 → 切
        // 触发后清零: 紧接着的失败重新从 1 计
        assert!(!tr.record_failure("a"));
    }

    #[test]
    fn window_slides_old_failures_out() {
        let (tr, t) = fake_tracker();
        assert!(!tr.record_failure("a")); // t=0
        t.store(100, Ordering::SeqCst);
        assert!(!tr.record_failure("a")); // t=100
        // t=0 的那次已滑出 5 分钟窗口 → 窗口内只有 t=100 + 本次 = 2 次, 不触发
        t.store(301, Ordering::SeqCst);
        assert!(!tr.record_failure("a"));
    }

    #[test]
    fn success_resets_counter() {
        let (tr, _t) = fake_tracker();
        assert!(!tr.record_failure("a"));
        assert!(!tr.record_failure("a"));
        tr.record_success("a"); // 恢复 → 清零
        assert!(!tr.record_failure("a"));
        assert!(!tr.record_failure("a"));
        assert!(tr.record_failure("a")); // 清零后重新数满 3 次才触发
    }

    #[test]
    fn ids_are_isolated() {
        let (tr, _t) = fake_tracker();
        assert!(!tr.record_failure("a"));
        assert!(!tr.record_failure("a"));
        assert!(!tr.record_failure("b")); // b 的失败不给 a 计数
        assert!(tr.record_failure("a"));
    }
}
