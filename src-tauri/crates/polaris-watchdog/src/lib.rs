//! 任务看门狗(纯逻辑)。蓝本: 有戏剧场 mica engine/api.rs·cli.rs 的三层看门狗里的
//! 前两层(空闲 idle + 绝对硬顶 hard cap)。
//!
//! 设计取向:
//! - **只判不杀**: `check()` 只产出 verdict, 杀进程/回报前端由接线方负责 ——
//!   接线方(如 polaris-kernel)往往还有第三层「进程树深检」的否决权(静默但在干活
//!   的构建/ffmpeg 不该被杀), 那层依赖平台探测, 不属于纯逻辑, 故不进本 crate。
//! - **空闲优先于绝对超时**: 只要持续有输出(touch), idle 永不触发 —— 活跃的长任务
//!   (批量 PPT/长脚本)不会被误杀; 硬顶是防「靠零星输出永久霸占资源」的最后兜底。
//! - **可注入时钟**: 测试不 sleep 长时间, 拨快假时钟即可。

use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 看门狗配置。任一项为 0 = 该维度关闭(与 kernel 现有 env 开关语义一致)。
#[derive(Debug, Clone, Copy)]
pub struct WatchdogConfig {
    /// 连续无输出多少秒判「疑似挂死」。默认 300。
    pub idle_secs: u64,
    /// 任务总时长硬顶(无论是否有输出)。默认 1800。
    pub hard_cap_secs: u64,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            idle_secs: 300,
            hard_cap_secs: 1800,
        }
    }
}

/// 判定结果。携带秒数供接线方拼装用户可读的错误文案。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogVerdict {
    /// 连续空闲超阈: 进程疑似卡死(网络吊死/无界扫描), 应终止。
    IdleKilled { idle_secs: u64 },
    /// 总时长超硬顶: 无条件收回, 防失控任务永久霸占资源。
    HardTimeout { elapsed_secs: u64 },
}

/// 时钟抽象: 生产用真实 `Instant::now`, 测试注入假时钟。
/// 用闭包而非 trait, 省一层定义, 且 `Arc<dyn Fn>` 可在测试里共享拨动。
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

pub struct Watchdog {
    cfg: WatchdogConfig,
    clock: Clock,
    started: Instant,
    /// 最近一次活动时刻。Mutex 而非 Atomic: Instant 无原子表示, 且 touch 频率
    /// (每行输出一次)远不足以让无竞争 parking_lot 锁成为热点。
    last_activity: Mutex<Instant>,
}

impl Watchdog {
    pub fn new(cfg: WatchdogConfig) -> Self {
        Self::with_clock(cfg, Arc::new(Instant::now))
    }

    pub fn with_clock(cfg: WatchdogConfig, clock: Clock) -> Self {
        let now = clock();
        Self {
            cfg,
            clock,
            started: now,
            last_activity: Mutex::new(now),
        }
    }

    /// 收到任何输出就调: 刷新活动时刻 → idle 计时从零重来。
    pub fn touch(&self) {
        *self.last_activity.lock() = (self.clock)();
    }

    /// 当前连续空闲时长(供接线方打日志/降频决策)。
    pub fn idle(&self) -> Duration {
        (self.clock)().saturating_duration_since(*self.last_activity.lock())
    }

    /// 任务总时长。
    pub fn elapsed(&self) -> Duration {
        (self.clock)().saturating_duration_since(self.started)
    }

    /// 判定。硬顶优先(它无条件, 语义更强); 其次 idle。
    /// 返回 None = 一切正常, 接线方继续下一轮轮询。
    pub fn check(&self) -> Option<WatchdogVerdict> {
        let elapsed = self.elapsed();
        if self.cfg.hard_cap_secs > 0 && elapsed >= Duration::from_secs(self.cfg.hard_cap_secs) {
            return Some(WatchdogVerdict::HardTimeout {
                elapsed_secs: elapsed.as_secs(),
            });
        }
        let idle = self.idle();
        if self.cfg.idle_secs > 0 && idle >= Duration::from_secs(self.cfg.idle_secs) {
            return Some(WatchdogVerdict::IdleKilled {
                idle_secs: idle.as_secs(),
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 假时钟: base + 可拨动的秒偏移。不 sleep, 测试瞬间完成。
    fn fake_clock() -> (Clock, Arc<AtomicU64>) {
        let base = Instant::now();
        let offset = Arc::new(AtomicU64::new(0));
        let o = offset.clone();
        let clock: Clock = Arc::new(move || base + Duration::from_secs(o.load(Ordering::SeqCst)));
        (clock, offset)
    }

    #[test]
    fn defaults_and_quiet_start() {
        let (clock, _t) = fake_clock();
        let wd = Watchdog::with_clock(WatchdogConfig::default(), clock);
        assert_eq!(wd.check(), None); // 刚启动不误报
    }

    #[test]
    fn idle_fires_only_without_output() {
        let (clock, t) = fake_clock();
        let wd = Watchdog::with_clock(
            WatchdogConfig {
                idle_secs: 300,
                hard_cap_secs: 0,
            },
            clock,
        );
        t.store(299, Ordering::SeqCst);
        assert_eq!(wd.check(), None);
        t.store(300, Ordering::SeqCst);
        assert_eq!(wd.check(), Some(WatchdogVerdict::IdleKilled { idle_secs: 300 }));
    }

    #[test]
    fn touch_resets_idle_forever() {
        // 核心保证: 持续有输出的长任务永不触发 idle。
        let (clock, t) = fake_clock();
        let wd = Watchdog::with_clock(
            WatchdogConfig {
                idle_secs: 10,
                hard_cap_secs: 0,
            },
            clock,
        );
        for s in 1..=1000u64 {
            t.store(s, Ordering::SeqCst);
            wd.touch(); // 每秒都有输出
            assert_eq!(wd.check(), None, "第 {s} 秒不该触发");
        }
        // 输出一停, 阈值一到就触发
        t.store(1010, Ordering::SeqCst);
        assert_eq!(wd.check(), Some(WatchdogVerdict::IdleKilled { idle_secs: 10 }));
    }

    #[test]
    fn hard_cap_fires_despite_activity() {
        let (clock, t) = fake_clock();
        let wd = Watchdog::with_clock(
            WatchdogConfig {
                idle_secs: 300,
                hard_cap_secs: 1800,
            },
            clock,
        );
        t.store(1799, Ordering::SeqCst);
        wd.touch();
        assert_eq!(wd.check(), None);
        t.store(1800, Ordering::SeqCst);
        wd.touch(); // 有输出也拦不住硬顶
        assert_eq!(
            wd.check(),
            Some(WatchdogVerdict::HardTimeout { elapsed_secs: 1800 })
        );
    }

    #[test]
    fn zero_disables_dimension() {
        let (clock, t) = fake_clock();
        let wd = Watchdog::with_clock(
            WatchdogConfig {
                idle_secs: 0,
                hard_cap_secs: 0,
            },
            clock,
        );
        t.store(1_000_000, Ordering::SeqCst);
        assert_eq!(wd.check(), None); // 双 0 = 完全关闭
    }
}
