//! 子进程池与进程树管理的单一实现。
//!
//! 此前 chat.rs 与 doctor.rs 各持一份 `CHILDREN` 静态池、chat.rs 与 project.rs
//! 各持一份 `kill_tree`、三处 `no_window` —— 全部收口到这里。
//! req_id 命名空间由调用方保证不冲突(chat 用 `req-*`,doctor 用 `env-*`)。

use once_cell::sync::Lazy;
use parking_lot::{Mutex, MutexGuard};
use std::collections::{HashMap, HashSet};
use std::process::{Child, Command};

/// 全局子进程注册表:所有「按 req_id 追踪、退出时必须回收」的子进程都登记在此。
/// App 退出钩子调用 [`kill_all`] 统一收割,不再依赖各模块自扫门前雪。
pub static CHILDREN: Lazy<ChildRegistry> = Lazy::new(ChildRegistry::new);

/// 「取消挂起」标记:stop 请求可能在 child 注册进池**之前**到达 —— 此时按 id 找不到
/// child,就把 req_id 记到这里;spawn 管线在注册前后各查一次,保证窄窗口内不漏杀。
pub struct ChildRegistry {
    map: Mutex<HashMap<String, Child>>,
    pending_cancel: Mutex<HashSet<String>>,
}

impl ChildRegistry {
    fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            pending_cancel: Mutex::new(HashSet::new()),
        }
    }

    /// 直接拿池锁(watchdog 等需要遍历/组合操作的调用方使用)。
    pub fn lock(&self) -> MutexGuard<'_, HashMap<String, Child>> {
        self.map.lock()
    }

    pub fn insert(&self, req_id: impl Into<String>, child: Child) {
        self.map.lock().insert(req_id.into(), child);
    }

    pub fn remove(&self, req_id: &str) -> Option<Child> {
        self.map.lock().remove(req_id)
    }

    /// 摘出并杀掉(含整棵进程树)。返回是否找到了该 child。
    /// 锁内只摘出 Child;kill_tree/kill/wait 是外部进程操作(Windows taskkill 常见 100ms+),
    /// 全在锁外执行,避免钉住并发的 insert(每次 chat 发送)与看门狗遍历。
    pub fn kill(&self, req_id: &str) -> bool {
        let child = self.map.lock().remove(req_id);
        match child {
            Some(mut child) => {
                kill_tree(child.id());
                let _ = child.kill();
                let _ = child.wait();
                true
            }
            None => false,
        }
    }

    /// App 退出时回收所有在飞子进程,连同它们扇出的整棵进程树。
    /// 否则用户关 App 时,长任务拉起的 dev server / node / python 会变孤儿。
    /// 同 [`kill`]:锁内只 drain,逐个收割在锁外做,防止退出期挂着多个长任务时全局停顿。
    pub fn kill_all(&self) {
        let drained: Vec<(String, Child)> = self.map.lock().drain().collect();
        for (_id, mut child) in drained {
            kill_tree(child.id());
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// 标记「取消挂起」(child 尚未注册时的 stop)。
    pub fn mark_cancel(&self, req_id: impl Into<String>) {
        self.pending_cancel.lock().insert(req_id.into());
    }

    /// 消费「取消挂起」标记:有则移除并返回 true。
    pub fn take_cancel(&self, req_id: &str) -> bool {
        self.pending_cancel.lock().remove(req_id)
    }
}

/// 按 PID kill 整个进程树。子进程在 shell/Task 工具下会拉起 python/node/dev server
/// 等子孙,只 kill 本体会留孤儿占着端口。
pub fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("taskkill");
        cmd.args(["/PID", &pid.to_string(), "/T", "/F"]);
        no_window(&mut cmd);
        let _ = cmd.output();
    }
    #[cfg(not(windows))]
    {
        // 杀进程组 (shell -c 起的子孙)。注意 output() 只有 spawn 失败才 Err;
        // killpg 因「pid 非组长/组不存在」失败时退出码非 0 但仍是 Ok,必须看 status
        // 才能退化为单杀,否则非组长 child 的子孙全部漏杀成孤儿(mac 痛点)。
        // `--` 分隔:部分 kill 实现会把 `-<pid>` 当非法选项而不是进程组。
        let group_ok = Command::new("kill")
            .args(["-TERM", "--", &format!("-{}", pid)])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !group_ok {
            let _ = Command::new("kill").args(["-TERM", "--", &pid.to_string()]).output();
        }
        // TERM 宽限后补 KILL:chromium/node 一类可能忽略 TERM。进程已退则 kill 无害。
        std::thread::sleep(std::time::Duration::from_millis(300));
        let target = if group_ok { format!("-{}", pid) } else { pid.to_string() };
        let _ = Command::new("kill").args(["-KILL", "--", &target]).output();
    }
}

/// Windows 下抑制子进程闪黑框(CREATE_NO_WINDOW);其它平台是 no-op。
#[cfg(windows)]
pub fn no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000);
}
#[cfg(not(windows))]
pub fn no_window(_cmd: &mut Command) {}

/// 跑外部命令并设超时:超时则杀进程树返回 Err,防 chromium/ffmpeg/say 挂死永久阻塞整个请求
/// (「让模块再也不会有问题」的硬化——看门狗只管 claude,管不到这些外部子进程)。
/// 调用方传入已配好 args 的 Command(stdio 由本函数置 null)。成功且退出码 0 → Ok。
/// (原住 forge/mod.rs;进程工具属横切基建,随 polaris-runtime 抽 crate 归位于此。)
pub fn run_with_timeout(mut cmd: Command, secs: u64, what: &str) -> Result<(), String> {
    use std::io::{BufRead, BufReader};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    // unix(mac/Linux): 把子进程置为新进程组组长, 超时时可 killpg 带走它扇出的整棵子孙
    // (chromium 的渲染/GPU 子进程、ffmpeg 的子代理), 否则只 child.kill() 会留孤儿挂死。
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    // 捕获 stderr(失败时带上,便于诊断「缺库/编解码器没装/字体缺失」等),stdout 仍丢弃。
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("{what} 启动失败: {e}"))?;
    // 后台线程边读边截断 stderr:既排空管道防进程写满阻塞,又只留尾部 ~4KB 防 OOM。
    let errbuf = Arc::new(Mutex::new(String::new()));
    let reader_handle = child.stderr.take().map(|se| {
        let buf = errbuf.clone();
        std::thread::spawn(move || {
            let mut r = BufReader::new(se);
            let mut line = String::new();
            loop {
                line.clear();
                match r.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let mut b = buf.lock().unwrap();
                        b.push_str(&line);
                        if b.len() > 8000 {
                            let cut = b.len() - 4000;
                            *b = b[cut..].to_string();
                        }
                    }
                }
            }
        })
    });
    let deadline = Instant::now() + Duration::from_secs(secs);
    // 轮询间隔前密后疏(10ms 起步、封顶 120ms):短命令 ~10ms 内即察觉退出;逐帧渲染路径
    // (video 900 帧/pptx 每页 1-2 次)每次调用省 ~100ms,累计分钟级。长命令仍是 120ms 粒度。
    let mut poll_ms: u64 = 10;
    // 循环只决定结局,把 join/格式化挪到循环外做一次,避免 reader_handle 在循环里被 move。
    let outcome: Result<std::process::ExitStatus, String> = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    // unix: 先 killpg 整组(child 是组长, 见 spawn 前 process_group(0)), 带走子孙;
                    // 再 child.kill() 兜底。Windows 无进程组, 仅 child.kill()(单进程, 现状不变)。
                    #[cfg(unix)]
                    unsafe {
                        libc::killpg(child.id() as i32, libc::SIGKILL);
                    }
                    let _ = child.kill();
                    let _ = child.wait(); // kill 后管道关闭,reader 线程随之结束
                    break Err(format!("{what} 超时({secs}s)被终止"));
                }
                std::thread::sleep(Duration::from_millis(poll_ms));
                poll_ms = (poll_ms * 2).min(120);
            }
            Err(e) => break Err(format!("{what} 等待失败: {e}")),
        }
    };
    // 不 join reader 线程:被杀进程的子进程(如 chromium 的子代理/cmd 的 ping)可能仍持 stderr
    // 管道,join 会阻塞到它们退出。errtail 只在失败路径进错误信息 —— 成功路径不用等,
    // 失败才给 50ms 让常规 stderr 排空后读取(诊断 best-effort,绝不阻塞)。
    if !matches!(&outcome, Ok(s) if s.success()) {
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(reader_handle); // 分离线程,随管道关闭自行结束
    let errtail = {
        let s = errbuf.lock().unwrap().trim().to_string();
        if s.is_empty() {
            String::new()
        } else {
            format!(": {s}")
        }
    };
    match outcome {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("{what} 失败(退出码 {:?}){errtail}", status.code())),
        Err(msg) => Err(format!("{msg}{errtail}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 验证 run_with_timeout 真能在超时后杀掉挂死进程并快速返回(随函数自 forge 迁入)。
    #[cfg(target_os = "windows")]
    #[test]
    fn timeout_kills_hanging_process() {
        use std::time::Instant;
        // 成功路径:立刻退出 0。
        let mut ok = Command::new("cmd");
        ok.args(["/c", "exit", "0"]);
        assert!(run_with_timeout(ok, 5, "test-ok").is_ok());
        // 超时路径:ping -n 20(~19s)应被 1s 超时杀掉,且很快返回。
        let mut hang = Command::new("cmd");
        hang.args(["/c", "ping", "-n", "20", "127.0.0.1"]);
        let t = Instant::now();
        let r = run_with_timeout(hang, 1, "test-hang");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("超时"));
        assert!(t.elapsed().as_secs() < 5, "超时后应快速返回,而非等满 19s");
        // 失败时 stderr 应进错误信息(可诊断)。
        let mut fail = Command::new("cmd");
        fail.args(["/c", "echo BOOMERR 1>&2 & exit 1"]);
        let e = run_with_timeout(fail, 5, "test-fail").unwrap_err();
        assert!(e.contains("BOOMERR"), "失败错误应含 stderr,实际: {e}");
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_hanging_process() {
        use std::time::Instant;
        assert!(run_with_timeout(Command::new("true"), 5, "test-ok").is_ok());
        let mut hang = Command::new("sleep");
        hang.arg("20");
        let t = Instant::now();
        let r = run_with_timeout(hang, 1, "test-hang");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("超时"));
        assert!(t.elapsed().as_secs() < 5);
        // 失败时 stderr 应进错误信息(可诊断)。
        let mut fail = Command::new("sh");
        fail.args(["-c", "echo BOOMERR >&2; exit 1"]);
        let e = run_with_timeout(fail, 5, "test-fail").unwrap_err();
        assert!(e.contains("BOOMERR"), "失败错误应含 stderr,实际: {e}");
    }

    fn sleeper() -> Command {
        #[cfg(windows)]
        {
            let mut c = Command::new("cmd");
            c.args(["/C", "ping -n 30 127.0.0.1 > NUL"]);
            c
        }
        #[cfg(not(windows))]
        {
            let mut c = Command::new("sleep");
            c.arg("30");
            c
        }
    }

    #[test]
    fn registry_insert_kill_roundtrip() {
        let reg = ChildRegistry::new();
        let mut cmd = sleeper();
        no_window(&mut cmd);
        let child = cmd.spawn().expect("spawn sleeper");
        reg.insert("t-1", child);
        assert!(reg.kill("t-1"), "应能找到并杀掉已注册 child");
        assert!(!reg.kill("t-1"), "重复 kill 应返回 false");
        assert!(reg.remove("t-1").is_none());
    }

    #[test]
    fn pending_cancel_is_consumed_once() {
        let reg = ChildRegistry::new();
        assert!(!reg.take_cancel("x"));
        reg.mark_cancel("x");
        assert!(reg.take_cancel("x"), "标记后第一次消费应为 true");
        assert!(!reg.take_cancel("x"), "消费后标记应清除");
    }

    #[test]
    fn kill_all_drains_pool() {
        let reg = ChildRegistry::new();
        for i in 0..2 {
            let mut cmd = sleeper();
            no_window(&mut cmd);
            reg.insert(format!("t-{i}"), cmd.spawn().expect("spawn"));
        }
        reg.kill_all();
        assert!(reg.lock().is_empty());
    }
}
