//! 工业级协作调度器(Fable Scheduler)—— 给盘点等「多核扫一棵会阻塞的目录树」的活儿
//! 提供**永不冻结**的工作队列。Mac/Windows/Docker 同构(纯 std:Mutex + Condvar + Instant)。
//!
//! 设计目标(对应用户诉求「工业级 CPU 调度 + 任何进程别卡死 + 有问题就调到队尾」):
//!
//! 1. **零忙等**:worker 空闲时在 [`Condvar`] 上挂起,不再 `sleep(2ms)` 自旋抢锁烧 CPU。
//!    为防漏唤醒/取消不及时,等待带 500ms 上限的超时(非忙等:每秒最多 2 次空醒),
//!    既靠 notify 即时唤醒,又有兜底周期复查 —— 这是「不依赖完美 notify」的硬化姿势。
//!
//! 2. **数学上保证完成**:完成判据 = `in_flight==0 && (队列空 || 存活 worker==0)`。
//!    - 正常扫完:队列排空且没有在途目录 → 完成;
//!    - 整个挂载点掉线、所有 worker 都卡死在 `read_dir`:看门狗逐个判定卡死并记账,
//!      `live_workers` 归零 → 完成判据照样成立。**绝不会出现「队列里还有目录、却没人处理、
//!      协调线程永久 join 等待」的冻结**(这正是旧 `thread::scope` 盘点卡死的根因)。
//!
//! 3. **问题项调到队尾**([`WorkQueue::demote`]):某目录读取出错(权限抖动/NAS 瞬断)时,
//!    不丢弃也不原地重试卡住别人,而是 `push_front` 到「队尾」(本队列 `pop_back` 取件,
//!    故 front = 最后才被取),让健康目录先扫完,问题目录排到最后再试 —— 名副其实的降级。
//!
//! 看门狗(心跳 + 死线判定 + 卡死记账)是盘点专属逻辑,放在 `inventory.rs`;本模块只提供
//! 与具体工作无关的队列原语,泛型 `T`,可被未来其它「会阻塞的扇出活儿」复用,并自带单测。

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// 一个待处理工作单元。`attempts` 记重试次数,供「问题项降级到队尾、超次即放弃」用。
pub struct Job<T> {
    pub item: T,
    pub attempts: u32,
}

struct Inner<T> {
    /// 双端队列:`pop_back` 取件(LIFO = 深度优先,队列深度 ~ 目录树高度,省内存);
    /// 普通新活儿 `push_back`(后进先出、就近扫);降级项 `push_front`(排到最后才取)。
    q: VecDeque<Job<T>>,
    /// 已 pop 出去、尚未 complete/abandon 的在途单元数。
    in_flight: usize,
    /// 仍有处理能力的 worker 数。worker 正常退场或被看门狗判定卡死都会减一。
    live_workers: usize,
    /// 主动关闭(取消):令 pop 立即返回 None、等待者全部醒来。
    closed: bool,
}

/// 永不冻结的协作工作队列(见模块文档)。`T` = 工作单元(盘点里是目录 `PathBuf`)。
pub struct WorkQueue<T> {
    inner: Mutex<Inner<T>>,
    cond: Condvar,
}

impl<T> WorkQueue<T> {
    /// 用初始工作单元播种(各记 0 次尝试)。`live_workers` 由 [`set_live_workers`] 设定。
    pub fn new(seed: impl IntoIterator<Item = T>) -> Self {
        let q = seed.into_iter().map(|item| Job { item, attempts: 0 }).collect();
        WorkQueue {
            inner: Mutex::new(Inner { q, in_flight: 0, live_workers: 0, closed: false }),
            cond: Condvar::new(),
        }
    }

    /// 设定存活 worker 数(开工前调一次,等于即将 spawn 的 worker 数)。
    pub fn set_live_workers(&self, n: usize) {
        self.inner.lock().unwrap().live_workers = n;
    }

    /// 追加普通工作(扫到的子目录),排到「队首端」就近优先处理(深度优先)。
    pub fn push(&self, item: T) {
        {
            let mut g = self.inner.lock().unwrap();
            g.q.push_back(Job { item, attempts: 0 });
        }
        self.cond.notify_one();
    }

    /// **降级**:把出问题的单元调到「队尾」(最后才会被取),并累加尝试次数。
    pub fn demote(&self, item: T, attempts: u32) {
        {
            let mut g = self.inner.lock().unwrap();
            g.q.push_front(Job { item, attempts });
        }
        self.cond.notify_one();
    }

    /// 取一件活儿。阻塞直到:有活儿(返回 `Some`,在途数 +1)/ 已扫完(返回 `None`)/
    /// 取消(返回 `None`)。**绝不忙等**:无活儿时在 Condvar 上挂起,带 500ms 兜底复查。
    pub fn pop(&self) -> Option<Job<T>> {
        let mut g = self.inner.lock().unwrap();
        loop {
            if g.closed || crate::fable::cancelled() {
                return None;
            }
            if let Some(job) = g.q.pop_back() {
                g.in_flight += 1;
                return Some(job);
            }
            // 队列空:在途归零即代表全树扫完,worker 退场。
            if g.in_flight == 0 {
                return None;
            }
            let (ng, _) = self.cond.wait_timeout(g, Duration::from_millis(500)).unwrap();
            g = ng;
        }
    }

    /// 完成一件活儿(在途数 -1)。可能触发「全部扫完」,唤醒协调线程与其余等待者。
    pub fn complete(&self) {
        {
            let mut g = self.inner.lock().unwrap();
            g.in_flight = g.in_flight.saturating_sub(1);
        }
        self.cond.notify_all();
    }

    /// 看门狗判定某 worker 卡死:释放它占的在途单元 + 存活 worker -1(该线程就此退场,
    /// 由调用方保证不再 complete)。可能令 `live_workers` 归零 → 满足完成判据、解除冻结。
    pub fn abandon(&self) {
        {
            let mut g = self.inner.lock().unwrap();
            g.in_flight = g.in_flight.saturating_sub(1);
            g.live_workers = g.live_workers.saturating_sub(1);
        }
        self.cond.notify_all();
    }

    /// worker 正常退场(pop 返回 None 后调):存活 worker -1。
    pub fn worker_exited(&self) {
        {
            let mut g = self.inner.lock().unwrap();
            g.live_workers = g.live_workers.saturating_sub(1);
        }
        self.cond.notify_all();
    }

    /// 是否已全部了结:在途归零,且(队列空 ∨ 没有存活 worker 能再处理)。
    fn is_done(g: &Inner<T>) -> bool {
        g.in_flight == 0 && (g.q.is_empty() || g.live_workers == 0)
    }

    /// 协调线程在此挂起,直到全部了结或被取消。**零忙等**(Condvar + 500ms 兜底复查)。
    pub fn wait_until_done(&self) {
        let mut g = self.inner.lock().unwrap();
        while !g.closed && !crate::fable::cancelled() && !Self::is_done(&g) {
            let (ng, _) = self.cond.wait_timeout(g, Duration::from_millis(500)).unwrap();
            g = ng;
        }
    }

    /// 限时版:最多等 `dur` 后返回「是否已全部了结/取消」。供协调线程交错上报进度用
    /// (等一小段→报一次进度→再等),既不忙等又能让进度条平滑推进。
    pub fn wait_until_done_for(&self, dur: Duration) -> bool {
        let g = self.inner.lock().unwrap();
        if g.closed || crate::fable::cancelled() || Self::is_done(&g) {
            return true;
        }
        let (g, _) = self.cond.wait_timeout(g, dur).unwrap();
        g.closed || crate::fable::cancelled() || Self::is_done(&g)
    }

    /// 取走队列里剩余未处理的工作单元(扫完后:正常路径返回空;若因挂载掉线令所有 worker
    /// 卡死而提前了结,这里就是「没扫到、已跳过」的目录,交给调用方如实上报)。
    pub fn drain_remaining(&self) -> Vec<T> {
        let mut g = self.inner.lock().unwrap();
        g.q.drain(..).map(|j| j.item).collect()
    }

    /// 取消:令所有等待者立即醒来并退出。
    pub fn cancel(&self) {
        self.inner.lock().unwrap().closed = true;
        self.cond.notify_all();
    }

    /// (测试用)当前 (在途数, 队列长度, 存活 worker)。
    #[cfg(test)]
    pub fn stats(&self) -> (usize, usize, usize) {
        let g = self.inner.lock().unwrap();
        (g.in_flight, g.q.len(), g.live_workers)
    }
}

// ───────────────────────── 旁路死线(请求路径防吊死)─────────────────────────
//
// 给「可能在 NAS 挂载点上阻塞的同步活儿」加死线 —— 典型:文件夹选择器对挂载点 `read_dir` /
// `is_dir`、递归算目录大小。这些直接跑在 HTTP/命令请求线程里,一旦挂载掉线(Docker/群晖把 NAS
// bind 进来,网络盘僵死是常态),整个请求被吊死 → 前端转圈到天荒地老 = 用户感知「卡死」。
// 由于阻塞的 syscall 无法从外部中断,这里把活儿丢到旁路线程跑,请求线程只等一个有界时长:
// 超时即返回(旁路线程 detach,其阻塞 syscall 随挂载恢复/进程退出回收),**请求线程绝不被吊死**。

/// 在带死线的旁路线程上跑一段可能阻塞的同步活儿;超时返回 `None`(旁路线程被 detach)。
/// `f` 的返回值需 `Send + 'static`(read_dir 结果 / 目录大小等都满足)。
pub fn with_deadline<T, F>(secs: u64, f: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    // 容量 1:接收端超时走人后,旁路线程 send 失败(通道断开)即返回,不会泄漏阻塞在 send 上。
    let (tx, rx) = std::sync::mpsc::sync_channel::<T>(1);
    std::thread::spawn(move || {
        let v = f();
        let _ = tx.send(v);
    });
    rx.recv_timeout(std::time::Duration::from_secs(secs)).ok()
}

/// 有界 `is_dir`:对挂载点探测可达性,挂载掉线最多卡 `secs` 秒即判不可达(返回 false),
/// 而非让 `Path::is_dir()` 在死 NAS 上 stat 几十秒吊死请求。
pub fn dir_reachable(path: &std::path::Path, secs: u64) -> bool {
    let p = path.to_path_buf();
    with_deadline(secs, move || p.is_dir()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// 旁路死线:可能阻塞的活儿(这里 sleep 5s 模拟死 NAS 的 read_dir)在死线内返回 None,
    /// 请求线程绝不被吊死;正常快活儿则照常拿到结果。
    #[test]
    fn with_deadline_caps_a_hang() {
        let start = std::time::Instant::now();
        let slow: Option<u32> = with_deadline(1, || {
            std::thread::sleep(Duration::from_secs(5));
            42
        });
        assert_eq!(slow, None, "超时返回 None");
        assert!(start.elapsed() < Duration::from_secs(3), "必须在死线附近返回,不等满 5s");
        let fast: Option<u32> = with_deadline(5, || 7);
        assert_eq!(fast, Some(7), "正常活儿照常返回结果");
    }

    /// 全树扫完:N worker 并发取件,子节点动态入队,最终所有节点恰好处理一次、无死锁。
    #[test]
    fn drains_a_tree_without_deadlock() {
        // 模拟一棵树:节点 i(<100)派生两个子 2i+1、2i+2(仍 <100)。
        let q = Arc::new(WorkQueue::new(vec![0usize]));
        let processed = Arc::new(AtomicUsize::new(0));
        let workers = 4;
        q.set_live_workers(workers);
        let mut handles = Vec::new();
        for _ in 0..workers {
            let q = q.clone();
            let processed = processed.clone();
            handles.push(std::thread::spawn(move || {
                while let Some(job) = q.pop() {
                    let n = job.item;
                    for c in [2 * n + 1, 2 * n + 2] {
                        if c < 100 {
                            q.push(c);
                        }
                    }
                    processed.fetch_add(1, Ordering::SeqCst);
                    q.complete();
                }
                q.worker_exited();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(processed.load(Ordering::SeqCst), 100, "0..100 每个节点恰处理一次");
        let (inflight, qlen, live) = q.stats();
        assert_eq!((inflight, qlen, live), (0, 0, 0), "扫完后清零");
    }

    /// 降级:出问题的单元被 push 到队尾,健康项先于它被取(LIFO 下 front = 最后取)。
    #[test]
    fn demote_sends_item_to_the_back() {
        let q: WorkQueue<&str> = WorkQueue::new(vec!["a"]);
        q.set_live_workers(1);
        // 先取出 a,在途 +1。
        let a = q.pop().unwrap();
        assert_eq!(a.item, "a");
        // 把出问题的 a 降级到队尾,再压入健康的 b、c(就近端)。
        q.demote("a", 1);
        q.push("b");
        q.push("c");
        q.complete(); // a 这一轮了结
        // pop_back 取件:c、b 先出,被降级的 a 最后出。
        assert_eq!(q.pop().unwrap().item, "c");
        assert_eq!(q.pop().unwrap().item, "b");
        let again = q.pop().unwrap();
        assert_eq!(again.item, "a");
        assert_eq!(again.attempts, 1, "降级累加尝试次数");
    }

    /// 关键反冻结:队列里还有活儿,但所有 worker 都「卡死」(只 abandon、不 complete)。
    /// live_workers 归零 → wait_until_done 必须返回(而非永久等待),剩余项可被取出上报。
    #[test]
    fn all_workers_stuck_does_not_freeze() {
        let q: WorkQueue<usize> = WorkQueue::new(vec![1, 2, 3, 4, 5]);
        let n = 2;
        q.set_live_workers(n);
        // 两个 worker 各取一件后「卡死」:看门狗 abandon 之(在途 -1、存活 -1)。
        let _j1 = q.pop().unwrap();
        let _j2 = q.pop().unwrap();
        q.abandon();
        q.abandon();
        // 此刻:在途 0、存活 0,但队列里还剩 3 件 → 完成判据成立(没人能再处理)。
        q.wait_until_done(); // 不得冻结
        let left = q.drain_remaining();
        assert_eq!(left.len(), 3, "剩余未处理项可被取出,交由调用方上报为已跳过");
    }

    /// 真·多线程反冻结(贴近 `scan_root` 的 worker + 心跳 + 看门狗 + abandon 组合,用真实 OS
    /// 线程与真实时序,而非同步手动记账):一个 worker 真的「卡死」(sleep 5s 远超死线,模拟
    /// 僵死的 `read_dir`)。看门狗线程检测到心跳超死线 → `abandon()` 记账释放。断言:协调线程
    /// `wait_until_done` 远早于 5s 返回 —— 卡死的 worker 绝不冻结整个盘点。
    #[test]
    fn real_threads_stuck_worker_does_not_freeze() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Instant;
        let q: Arc<WorkQueue<usize>> = Arc::new(WorkQueue::new(0..6));
        let workers = 3usize;
        q.set_live_workers(workers);
        // 每个 worker 一个心跳槽:(是否在忙, 起始时刻, 是否已被看门狗判定卡死)。
        let beats: Arc<Vec<Mutex<(bool, Instant, bool)>>> =
            Arc::new((0..workers).map(|_| Mutex::new((false, Instant::now(), false))).collect());
        let scan_done = Arc::new(AtomicBool::new(false));
        let deadline = Duration::from_millis(300);

        // worker:取活儿、打心跳;item==0 那件「卡死」(sleep 5s 远超死线),其余秒回。
        for i in 0..workers {
            let (q, beats) = (q.clone(), beats.clone());
            std::thread::spawn(move || {
                while let Some(job) = q.pop() {
                    {
                        let mut b = beats[i].lock().unwrap();
                        *b = (true, Instant::now(), false);
                    }
                    if job.item == 0 {
                        std::thread::sleep(Duration::from_secs(5)); // 模拟卡死的 read_dir
                    }
                    let abandoned = {
                        let mut b = beats[i].lock().unwrap();
                        b.0 = false;
                        std::mem::replace(&mut b.2, false)
                    };
                    if abandoned {
                        return; // 看门狗已记账,本线程退场(不再 complete / worker_exited)
                    }
                    q.complete();
                }
                q.worker_exited();
            });
        }
        // 看门狗:超死线判卡死 → abandon(释放在途 + 存活 worker -1)。
        {
            let (q, beats, scan_done) = (q.clone(), beats.clone(), scan_done.clone());
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(50));
                if scan_done.load(Ordering::SeqCst) {
                    break;
                }
                for slot in beats.iter() {
                    let hit = {
                        let mut b = slot.lock().unwrap();
                        if b.0 && !b.2 && b.1.elapsed() > deadline {
                            b.2 = true;
                            true
                        } else {
                            false
                        }
                    };
                    if hit {
                        q.abandon();
                    }
                }
            });
        }
        // 协调线程:等了结。卡死 worker 在睡 5s,但 wait_until_done 必须远早返回。
        let start = Instant::now();
        q.wait_until_done();
        scan_done.store(true, Ordering::SeqCst);
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(3),
            "卡死 worker 不得冻结协调线程(实际用时 {elapsed:?},应≈死线 300ms)"
        );
    }

    /// 取消:wait_until_done 与 pop 立即返回。
    #[test]
    fn cancel_unblocks_everything() {
        let q: Arc<WorkQueue<usize>> = Arc::new(WorkQueue::new(vec![1]));
        q.set_live_workers(1);
        let _held = q.pop().unwrap(); // 在途 1,永不 complete → 正常下 wait 会一直挂
        let qc = q.clone();
        let waiter = std::thread::spawn(move || qc.wait_until_done());
        // 另一线程取消 → 等待者必须醒来返回。
        q.cancel();
        waiter.join().unwrap();
        assert!(q.pop().is_none(), "取消后 pop 立即返回 None");
    }
}
