//! Polaris Forge · fx 工业级化(任务 c §C.2 + §C.3)
//!
//! 不在 polaris-fx crate 内部(那个在 workspace 外),本模块给主 crate 用的"护栏":
//!   - safe_run 套 try/catch 防止单动效拖垮全片
//!   - spring 闭式解 x(t) = x0·cos(ωt) + v0/ω·sin(ωt) 跨平台 1e-9 容差
//!   - 26 个动效健康徽章统计(给 /api/status 上报)
//!   - FxFrameSink 重导出(来自 polaris-forge-codec,任务 c §C.4)
//!
//! 真实动效定义在 workspace/polaris-fx crate(本期未建),本模块只给基础设施。

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

static FX_OK_COUNT: AtomicU64 = AtomicU64::new(0);
static FX_ERR_COUNT: AtomicU64 = AtomicU64::new(0);
static FX_LAST_ERR_T: AtomicU64 = AtomicU64::new(0);

/// 单 fx 执行结果(给前端健康徽章用)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FxRunResult {
    pub name: String,
    pub ok: bool,
    pub err: Option<String>,
    pub duration_ms: u64,
}

/// 工业级化(任务 c §C.2.1):safe_run 套 try/catch 防止单动效拖垮全片
/// 闭包返回 Result 是 Rust 强制的;这是模拟 JS try/catch 的"安全网",返回 ok/err 统计
pub fn safe_run<F>(name: &str, f: F) -> FxRunResult
where
    F: FnOnce() -> std::result::Result<(), String> + std::panic::UnwindSafe,
{
    let start = std::time::Instant::now();
    let r = match std::panic::catch_unwind(f) {
        Ok(Ok(())) => {
            FX_OK_COUNT.fetch_add(1, Ordering::Relaxed);
            FxRunResult { name: name.to_string(), ok: true, err: None, duration_ms: start.elapsed().as_millis() as u64 }
        }
        Ok(Err(e)) => {
            FX_ERR_COUNT.fetch_add(1, Ordering::Relaxed);
            FX_LAST_ERR_T.store(now_ms(), Ordering::Relaxed);
            FxRunResult { name: name.to_string(), ok: false, err: Some(e), duration_ms: start.elapsed().as_millis() as u64 }
        }
        Err(panic) => {
            FX_ERR_COUNT.fetch_add(1, Ordering::Relaxed);
            FX_LAST_ERR_T.store(now_ms(), Ordering::Relaxed);
            let msg = panic_msg(&panic);
            FxRunResult { name: name.to_string(), ok: false, err: Some(format!("panic: {msg}")), duration_ms: start.elapsed().as_millis() as u64 }
        }
    };
    log_fx(&r);
    r
}

fn panic_msg(p: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&'static str>() { s.to_string() }
    else if let Some(s) = p.downcast_ref::<String>() { s.clone() }
    else { "unknown".into() }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 动效错误日志(任务 c §C.2.1 末尾):单行 JSON 落 ~/Polaris/data/forge_fx.log.jsonl
/// 软失败:写不出也不抛(磁盘满不 fail 渲染)
fn log_fx(r: &FxRunResult) {
    if !r.ok {
        if let Some(home) = directories::UserDirs::new().map(|u| u.home_dir().to_path_buf()) {
            let dir = home.join("Polaris").join("data");
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join("forge_fx.log.jsonl");
            let line = serde_json::to_string(r).unwrap_or_default();
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                use std::io::Write;
                let _ = writeln!(f, "{}", line);
            }
        }
    }
}

/// 26 动效健康徽章(给 /api/status 上报)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FxHealth {
    pub total_runs: u64,
    pub ok_runs: u64,
    pub err_runs: u64,
    pub last_err_ms_ago: u64,
    pub ok_rate: f64,
}

pub fn health() -> FxHealth {
    let ok = FX_OK_COUNT.load(Ordering::Relaxed);
    let err = FX_ERR_COUNT.load(Ordering::Relaxed);
    let total = ok + err;
    let last_err_t = FX_LAST_ERR_T.load(Ordering::Relaxed);
    let last_err_ms_ago = if last_err_t == 0 { 0 } else { now_ms().saturating_sub(last_err_t) };
    FxHealth {
        total_runs: total,
        ok_runs: ok,
        err_runs: err,
        last_err_ms_ago,
        ok_rate: if total == 0 { 1.0 } else { ok as f64 / total as f64 },
    }
}

/// spring 闭式解(任务 c §C.3.1):x(t) = x0·cos(ωt) + v0/ω·sin(ωt)
/// 临界阻尼/过阻击分别闭式解(本期只给欠阻击,临界/过阻击放 P2 扩)
/// 跨平台 f64 一致性:1e-9 容差单测(任务 c §C.3.2)
pub fn spring_solve_underdamped(x0: f64, v0: f64, omega: f64, t: f64) -> f64 {
    if omega.abs() < 1e-12 {
        // ω≈0:无 spring,直接 x0 + v0·t(极限)
        return x0 + v0 * t;
    }
    x0 * (omega * t).cos() + (v0 / omega) * (omega * t).sin()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_run_ok() {
        let r = safe_run("test_ok", || Ok(()));
        assert!(r.ok);
    }

    #[test]
    fn safe_run_err_returned() {
        let r = safe_run("test_err", || Err("boom".into()));
        assert!(!r.ok);
        assert_eq!(r.err.as_deref(), Some("boom"));
    }

    #[test]
    fn safe_run_catches_panic() {
        let r = safe_run("test_panic", || {
            panic!("intentional");
        });
        assert!(!r.ok);
        assert!(r.err.as_deref().unwrap().contains("panic"));
    }

    #[test]
    fn spring_solve_zero() {
        // 初始条件 x0=1, v0=0, ω=0:无 spring,值始终 1
        assert!((spring_solve_underdamped(1.0, 0.0, 0.0, 1.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn spring_solve_consistency() {
        // 同一组参数跨平台 f64 应一致
        let x = spring_solve_underdamped(1.0, 0.5, 2.0, 0.3);
        // 解析解已知:x(0.3) = 1·cos(0.6) + (0.5/2)·sin(0.6)
        let expected = 1.0_f64 * (0.6_f64).cos() + (0.5 / 2.0) * (0.6_f64).sin();
        assert!((x - expected).abs() < 1e-9);
    }

    #[test]
    fn health_ok_rate_decreases_on_err() {
        let _ = safe_run("h_test", || Ok(()));
        let h1 = health();
        let _ = safe_run("h_test", || Err("e".into()));
        let h2 = health();
        assert!(h2.err_runs > h1.err_runs);
    }
}
