//! 常驻 claude agent 进程池 (conv_id → 常驻子进程)。
//!
//! 为什么要常驻: claude CLI 是 bun 打包的单文件, 每次 spawn 自举 ~6.4s(解包+加载,
//! 外部无法加速), 旧路径每条消息都重新 spawn → 首 token 8-10s。改用
//! `--input-format stream-json` 让进程常驻: 每轮往 stdin 写一行 user 消息,
//! 实测第 2 轮起首事件 1.4-2.6s(还吃到服务端 prompt cache)。每轮以 CLI 输出的
//! `{"type":"result",...}` 事件为收尾界标。
//!
//! 语义要点(与旧「每轮重发全量 prompt」的差异):
//! - **首条消息**(或进程重建后的第一条)发完整拼装 prompt(系统前置+KB+历史);
//! - **续轮**只发「本轮新增部分」: claude 进程内自己保持上下文, 会话级不变的
//!   系统前置/CLAUDE.md/历史 不再重发(见 [`PromptPlan`] 的三类分段)。
//! - 配置指纹([`Fingerprint`])变了 → 旧进程语义失效(权限/供应商/模型/工具面都
//!   烙在进程启动参数里), 只能杀掉重建, 本条按首条处理。
//!
//! 生命周期: LRU 上限 2 个常驻进程(每个 ~200-400MB 内存); 空闲 10 分钟 TTL 惰性
//! 收割(每次发消息时顺手扫, 不开常驻轮询线程 —— 多留几分钟只是内存, 不烧 CPU);
//! 进程死亡/stdin 写失败 → 透明重建不报错; 停止按钮/对话删除/应用退出 → 杀进程,
//! 下轮自动重建。开关: POLARIS_PERSISTENT_AGENT(默认开, =0 回落旧的每消息 spawn)。

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStderr, ChildStdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Instant, SystemTime};

#[cfg(not(feature = "desktop"))]
use crate::host::AppHandle;
#[cfg(feature = "desktop")]
use tauri::AppHandle;

use crate::runtime::procs::{kill_tree, CHILDREN};

use super::pipeline;
use super::types::ChatStreamEvent;

// ───────────────────────── 开关 ─────────────────────────

/// 常驻 agent 总开关: 默认开; POLARIS_PERSISTENT_AGENT=0 回落旧的每消息 spawn 路径
/// (旧路径完整保留在 pipeline.rs, 排障/对比随时可切)。
pub(super) fn persistent_enabled() -> bool {
    std::env::var("POLARIS_PERSISTENT_AGENT")
        .map(|v| v.trim() != "0")
        .unwrap_or(true)
}

// ───────────────────────── PromptPlan ─────────────────────────

/// prompt 分段: 首轮/续轮的拆分依据。
/// - `Session`: 会话级不变(KB 铁律/风格/CLAUDE.md/KB 概览/记忆地图/对话历史) ——
///   claude 进程内自持上下文, 续轮重发只是烧 token + 稀释注意力, 剔除;
/// - `Keyed`: 意图门控块(skill/脚本公约/下载公约等), 本轮触发且**本会话还没发过**
///   才发(按 key 去重) —— 首轮没触发、第 5 轮才触发的公约不能因"续轮剔除"而永远缺席;
/// - `Turn`: 每轮都变(KB 召回增量/手改产物提醒/专家块/用户问题), 续轮照发。
pub(super) enum Seg {
    Session(String),
    Keyed(String, String),
    Turn(String),
}

/// 有序分段列表: 旧路径拼接顺序原样保留(full_text = 逐段连接), 常驻续轮按
/// 分段类别取舍(continuation_text)。
#[derive(Default)]
pub(super) struct PromptPlan {
    segs: Vec<Seg>,
}

impl PromptPlan {
    pub(super) fn new() -> Self {
        Self::default()
    }
    pub(super) fn session(&mut self, s: String) {
        self.segs.push(Seg::Session(s));
    }
    pub(super) fn keyed(&mut self, key: impl Into<String>, s: String) {
        self.segs.push(Seg::Keyed(key.into(), s));
    }
    pub(super) fn turn(&mut self, s: String) {
        self.segs.push(Seg::Turn(s));
    }

    /// 全量文本: 旧路径每轮用它; 常驻路径首轮(含进程重建后第一轮)用它。
    pub(super) fn full_text(&self) -> String {
        let mut out = String::new();
        for seg in &self.segs {
            match seg {
                Seg::Session(s) | Seg::Turn(s) => out.push_str(s),
                Seg::Keyed(_, s) => out.push_str(s),
            }
        }
        out
    }

    /// 续轮文本: 剔除会话级不变段与已发过的门控块, 相对顺序保持不变。
    pub(super) fn continuation_text(&self, sent: &HashSet<String>) -> String {
        let mut out = String::new();
        for seg in &self.segs {
            match seg {
                Seg::Session(_) => {}
                Seg::Keyed(k, s) => {
                    if !sent.contains(k) {
                        out.push_str(s);
                    }
                }
                Seg::Turn(s) => out.push_str(s),
            }
        }
        out
    }

    /// 本轮出现的全部门控 key(写入成功后并入会话的 sent_keys)。
    pub(super) fn keys(&self) -> Vec<String> {
        self.segs
            .iter()
            .filter_map(|s| match s {
                Seg::Keyed(k, _) => Some(k.clone()),
                _ => None,
            })
            .collect()
    }
}

// ───────────────────────── 指纹 ─────────────────────────

/// 会话配置指纹: 这些量全部烙在 claude 进程的**启动参数/环境**里(--permission-mode/
/// --allowedTools/--model/--add-dir/供应商 env), 常驻进程无法中途改 → 任一变化只能
/// 杀旧重спавн。creative 也入指纹: 它决定 KB 铁律/回答风格这两段会话级前置的取向,
/// 中途翻转时靠重建让新前置以"首条"身份生效(创作/非创作切换很少见, 偶付一次 6.4s 可接受)。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Fingerprint {
    pub perm: String,
    pub provider_id: String,
    pub work_full: bool,
    pub with_task: bool,
    pub model: String,
    pub add_dirs: Vec<String>,
    pub creative: bool,
    /// 本项目绑定的工作目录(claude cwd)。cwd 是 spawn 时钉死、进程中途改不了的, 故必须入
    /// 指纹: 用户切换项目工作目录后, 旧常驻进程还在老 cwd 里跑, 判变才会杀旧重建。空=默认 cwd。
    pub cwd: String,
}

// ───────────────────────── PoolCore (纯记账, 可测) ─────────────────────────

/// LRU 上限: 每个常驻 claude ~200-400MB, 2 个封顶 → 最多 ~800MB, 桌面可接受。
const MAX_SESSIONS: usize = 2;
/// 空闲 TTL: 10 分钟没消息就收割(惰性检查, 在每次发消息时扫)。
/// 注意: 「正在跑轮次」的会话不论多久都豁免(见 sweep_expired 的 busy 谓词)。
const IDLE_TTL_MS: u64 = 10 * 60 * 1000;

struct Entry<T> {
    conv: String,
    fp: Fingerprint,
    last_used: u64,
    payload: T,
}

/// 池的纯记账核: 指纹比较/LRU/TTL 全在这里, 不碰真进程 → 单测用假 payload +
/// 注入的毫秒时钟即可覆盖, 不 sleep 不 spawn。真池 [`POOL`] 以 SessionProc 为 payload。
struct PoolCore<T> {
    entries: Vec<Entry<T>>,
}

impl<T> PoolCore<T> {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// TTL 收割: 摘出所有空闲超时条目(杀进程由调用方在锁外做)。
    /// busy 谓词豁免「正在跑轮次」的会话: last_used 只在轮次**开始**时刷新, 一轮跑得
    /// 比 TTL 久的长任务(几十分钟编译/整夜脚本)绝不能被当闲置误杀 —— 否则任何其它
    /// 对话一发消息/预热, 这里的惰性清扫就会把进行中的长任务整树杀掉且静默收尾。
    fn sweep_expired(&mut self, now: u64, busy: impl Fn(&T) -> bool) -> Vec<T> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.entries.len() {
            if now.saturating_sub(self.entries[i].last_used) >= IDLE_TTL_MS
                && !busy(&self.entries[i].payload)
            {
                out.push(self.entries.remove(i).payload);
            } else {
                i += 1;
            }
        }
        out
    }

    /// 取条目(指纹 + payload)。
    fn get_mut(&mut self, conv: &str) -> Option<(&Fingerprint, &mut T)> {
        self.entries
            .iter_mut()
            .find(|e| e.conv == conv)
            .map(|e| (&e.fp, &mut e.payload))
    }

    fn touch(&mut self, conv: &str, now: u64) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.conv == conv) {
            e.last_used = now;
        }
    }

    fn remove(&mut self, conv: &str) -> Option<T> {
        let i = self.entries.iter().position(|e| e.conv == conv)?;
        Some(self.entries.remove(i).payload)
    }

    /// 插入新会话; 超过 LRU 上限时摘出最久未用的旧会话返回给调用方收割。
    /// busy 谓词与 sweep_expired 同义: 逐出只在**非忙碌**条目里挑, 全员在跑轮次时
    /// 宁可临时超上限(多占一个进程的内存, 轮次结束后由 TTL 收回), 也不杀进行中的任务。
    fn insert(
        &mut self,
        conv: String,
        fp: Fingerprint,
        payload: T,
        now: u64,
        busy: impl Fn(&T) -> bool,
    ) -> Vec<T> {
        // 同 conv 旧条目先摘(调用方应已处理, 这里兜底防泄漏)
        let mut evicted: Vec<T> = self.remove(&conv).into_iter().collect();
        self.entries.push(Entry {
            conv,
            fp,
            last_used: now,
            payload,
        });
        while self.entries.len() > MAX_SESSIONS {
            // 新插入者恒在末位(只会移除它前面的条目), 不列为逐出候选 —— 否则「其余
            // 全员忙碌」时会把刚建好的会话自己逐出去, 新对话的消息就发不出去了。
            let new_idx = self.entries.len() - 1;
            let candidate = self
                .entries
                .iter()
                .enumerate()
                .filter(|(i, e)| *i != new_idx && !busy(&e.payload))
                .min_by_key(|(_, e)| e.last_used)
                .map(|(i, _)| i);
            match candidate {
                Some(idx) => evicted.push(self.entries.remove(idx).payload),
                None => break, // 其余全员忙碌: 不杀活轮, 容忍暂时超上限
            }
        }
        evicted
    }

    /// 超编收敛:insert 在「其余全员忙碌」时容忍超上限(不杀活轮),这里在**检出路径**
    /// 把编制收回来 —— 轮次结束后条目不再忙碌,下一次任何对话发消息/预热即逐出
    /// 「最久未用 · 非忙碌 · 非当前 conv」者。没有它,超编会因每次使用都 touch 续命
    /// 而永久滞留(N 个常驻进程 × 200-400MB,违背上限 2 的资源承诺)。
    fn trim_over_cap(&mut self, keep_conv: &str, busy: impl Fn(&T) -> bool) -> Vec<T> {
        let mut evicted = Vec::new();
        while self.entries.len() > MAX_SESSIONS {
            let candidate = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.conv != keep_conv && !busy(&e.payload))
                .min_by_key(|(_, e)| e.last_used)
                .map(|(i, _)| i);
            match candidate {
                Some(idx) => evicted.push(self.entries.remove(idx).payload),
                None => break, // 仍全员忙碌(或只剩当前 conv):等下次检出再收
            }
        }
        evicted
    }

    fn drain_all(&mut self) -> Vec<T> {
        self.entries.drain(..).map(|e| e.payload).collect()
    }

    fn iter_mut(&mut self) -> impl Iterator<Item = (&str, &mut T)> {
        self.entries
            .iter_mut()
            .map(|e| (e.conv.as_str(), &mut e.payload))
    }
}

// ───────────────────────── 真池 ─────────────────────────

/// 会话共享态: reader/stderr/看门狗/取消 四方跨线程共用。
struct SessionShared {
    conv: String,
    pid: u32,
    /// 进程仍可用(EOF/主动杀后置 false; 池条目下轮惰性清理)。
    alive: AtomicBool,
    /// 本次死亡已由取消/看门狗主动执行并自行告知前端 → reader 收 EOF 不再报错
    /// (对齐旧路径「chat_cancel 先摘 Child, reader 查不到就不发异常退出 error」的语义)。
    kill_reported: AtomicBool,
    /// 当前活跃轮的 req_id: 看门狗以「它被清空」为本轮结束界标 —— 轮与轮之间的
    /// 静默不是卡死, 绝不能被 idle 杀。
    active_req: Mutex<Option<String>>,
    /// 当前活跃轮的看门狗(stderr 线程按行 touch 用; 轮结束置 None)。
    watchdog: Mutex<Option<Arc<polaris_watchdog::Watchdog>>>,
    /// stderr 尾巴(封顶), 进程猝死时拼进错误信息辅助诊断。
    stderr_tail: Mutex<String>,
}

/// 一个常驻会话进程: 池锁下只做轻操作, kill/wait/写 stdin 全在池锁外。
struct SessionProc {
    /// 摘出后在锁外 kill/wait(所有权在手 = reader 无法先 reap → pid 不会被复用误杀)。
    child: Option<Child>,
    stdin: Arc<Mutex<ChildStdin>>,
    job_tx: mpsc::Sender<TurnJob>,
    shared: Arc<SessionShared>,
    /// 已发过的门控块 key(见 PromptPlan::Keyed)。
    sent_keys: HashSet<String>,
    /// 首条消息(完整 prompt)是否已成功写入 —— 续轮判定依据。
    first_done: bool,
}

static POOL: Lazy<Mutex<PoolCore<SessionProc>>> = Lazy::new(|| Mutex::new(PoolCore::new()));

/// 单调毫秒时钟(进程启动起算): 只做 LRU/TTL 相对比较, 不需要墙钟。
fn now_ms() -> u64 {
    static START: Lazy<Instant> = Lazy::new(Instant::now);
    START.elapsed().as_millis() as u64
}

/// 收割一个会话进程(池锁外调用): 标记死亡→杀整树→reap。
/// reported=true 表示"死因已由调用方告知前端"(取消/看门狗/退出), reader 不再补报 error。
/// 「正在跑轮次」判定(TTL 清扫/LRU 逐出的豁免谓词): 进程活着且本轮 active_req 未被
/// reader 清空。进程死了(alive=false)即便 active_req 残留也不算忙 —— 死条目必须可清。
fn session_busy(sp: &SessionProc) -> bool {
    sp.shared.alive.load(Ordering::SeqCst) && sp.shared.active_req.lock().is_some()
}

fn reap(mut s: SessionProc, reported: bool) {
    if reported {
        s.shared.kill_reported.store(true, Ordering::SeqCst);
    }
    s.shared.alive.store(false, Ordering::SeqCst);
    if let Some(mut c) = s.child.take() {
        kill_tree(s.shared.pid);
        let _ = c.kill();
        let _ = c.wait();
    }
}

// ───────────────────────── 对外收割入口 ─────────────────────────

/// 停止按钮: req_id 命中某会话的活跃轮 → 杀常驻进程(简单可靠, 下轮自动重建)。
/// 返回 false = 不是常驻轮(旧路径/或尚未注册), 调用方走旧的 CHILDREN 逻辑。
pub fn cancel_by_req(req_id: &str) -> bool {
    let victim = {
        let mut g = POOL.lock();
        let conv = g
            .iter_mut()
            .find(|(_, sp)| sp.shared.active_req.lock().as_deref() == Some(req_id))
            .map(|(c, _)| c.to_string());
        conv.and_then(|c| g.remove(&c))
    };
    match victim {
        Some(s) => {
            // 先标记再杀: reader 收 EOF 时按「已告知」处理, 只发 done 不发 error
            // (与旧路径 chat_cancel 的静默收尾语义一致)。
            reap(s, true);
            true
        }
        None => false,
    }
}

/// 某 req 是否仍是池内某会话的活跃轮(前端「无声死亡」看门狗查询用)。
pub fn is_active_req(req_id: &str) -> bool {
    let mut g = POOL.lock();
    let active = g.iter_mut().any(|(_, sp)| {
        sp.shared.alive.load(Ordering::SeqCst)
            && sp.shared.active_req.lock().as_deref() == Some(req_id)
    });
    active
}

/// 对话删除时收割其常驻进程(没有则无事)。
pub fn kill_conv(conv_id: &str) {
    let victim = POOL.lock().remove(conv_id);
    if let Some(s) = victim {
        reap(s, true);
    }
}

/// 应用退出时收割全部常驻进程(接 App 退出钩子, 与 CHILDREN.kill_all 并列)。
pub fn kill_all() {
    let all = POOL.lock().drain_all();
    for s in all {
        reap(s, true);
    }
}

// ───────────────────────── 本轮任务 ─────────────────────────

/// 发给会话 reader 线程的「一轮」上下文: reader 常驻在 stdout 上, 每收到一个 job
/// 就以该轮的 req_id/产物快照/看门狗处理事件流, 直到 result 事件收尾。
struct TurnJob {
    req_id: String,
    conv_id: String,
    app: AppHandle,
    art_dir: PathBuf,
    art_before: HashMap<PathBuf, SystemTime>,
    image_notice: Option<String>,
    watchdog: Arc<polaris_watchdog::Watchdog>,
}

/// run_persistent_turn 的入参(从 chat_send_pipeline 拼装现场带过来)。
pub(super) struct TurnParams {
    pub app: AppHandle,
    pub req_id: String,
    pub conv_id: String,
    pub plan: PromptPlan,
    pub perm: String,
    pub art_dir: PathBuf,
    pub art_before: HashMap<PathBuf, SystemTime>,
    pub image_notice: Option<String>,
    pub with_task: bool,
    pub work_full: bool,
    pub provider_id: Option<String>,
    pub creative: bool,
    /// 本项目绑定的工作目录(用户选的真实仓库), None=用默认 cwd。透传给 spawn_on_host + 进指纹。
    pub work_dir: Option<String>,
}

/// 由本轮参数算配置指纹 —— run_persistent_turn 与 prewarm 必须用**同一个**算法,
/// 否则预热起的进程会因指纹算法差异被真发送误判「不符」而白白杀掉重建。
fn fingerprint_of(p: &TurnParams) -> Fingerprint {
    // cwd 与 add_dirs 必须用**同一个** effective_cwd 口径算(add_dirs 靠它判 KB/产物在不在
    // cwd 子树), 且这里算出的 cwd 必须与 spawn_session→spawn_on_host 真用的 cwd 完全一致。
    let cwd = pipeline::effective_cwd(p.work_dir.as_deref().map(std::path::Path::new));
    Fingerprint {
        perm: p.perm.clone(),
        // None/"auto"/"" 都是「跟随全局当前供应商」同一档, 归一化后指纹才稳定。
        provider_id: match p.provider_id.as_deref() {
            None | Some("") | Some("auto") => "auto".into(),
            Some(x) => x.into(),
        },
        work_full: p.work_full,
        with_task: p.with_task,
        model: pipeline::model_for_mode(p.work_full).unwrap_or_default(),
        add_dirs: pipeline::extra_claude_dirs(&cwd, &p.art_dir),
        creative: p.creative,
        cwd: cwd.to_string_lossy().to_string(),
    }
}

/// 预热入口(chat_prewarm 命令 → 后台线程调用): 用户还在打字时就把常驻进程起好。
///
/// 为什么有效: claude CLI spawn 后**立即自举**(~6.4s bun 解包+加载), 不等首条 stdin
/// 消息(实测) —— 把这段自举挪进「用户打字期间」, 对话首条消息的首响即可从 ~10s 压到
/// ~3s。语义:
/// - 池里已有该 conv 的活进程且指纹一致 → no-op(不 touch LRU: 预热不算"使用");
/// - 否则按当前设置 spawn 一个空会话入池(first_done=false, 不发任何消息), 之后
///   run_persistent_turn 检出指纹一致直接复用、照常发全量首条 prompt;
/// - creative 取决于本轮 prompt 命中哪些 skill, 预热时还没有 prompt, 按 false 预估
///   (绝大多数轮次非创作); 猜错时真发送检出指纹不符照常杀旧重建 —— 预热白做但无害。
///
/// best-effort: 任何失败只打日志不上抛(预热失败 ≠ 对话失败, 真发送有完整重建路径)。
pub fn prewarm(
    app: AppHandle,
    conv_id: String,
    perm: String,
    with_task: bool,
    work_full: bool,
    provider_id: Option<String>,
) {
    if !persistent_enabled() {
        return;
    }
    // 产物目录与真发送同源(artifacts_dir 是纯 conv_id 映射): art_dir 进指纹的
    // add_dirs, 取值路径必须一致, 否则预热指纹永远对不上。
    let art_dir = super::artifacts::artifacts_dir(Some(&conv_id));
    let _ = std::fs::create_dir_all(&art_dir);
    // 工作目录从 conv → project 反查(与真发送 chat_send_pipeline 同源), 必须一致否则预热指纹
    // 永远对不上、白起进程。这里内部解析而不改 prewarm 签名, 免动 chat_prewarm 调用方。
    let work_dir = crate::conv::project_id_of_conversation(&conv_id)
        .and_then(|pid| crate::conv::project_work_dir(&pid));
    let p = TurnParams {
        app,
        req_id: String::new(), // 预热没有"轮", 不发消息 → 无 req_id/plan/快照
        conv_id: conv_id.clone(),
        plan: PromptPlan::new(),
        perm,
        art_dir,
        art_before: HashMap::new(),
        image_notice: None,
        with_task,
        work_full,
        provider_id,
        creative: false,
        work_dir,
    };
    let fp = fingerprint_of(&p);

    // ── 检出: 已有活进程且指纹一致 → no-op(顺手 TTL 收割; 锁内只摘, 杀在锁外) ──
    let mut to_reap: Vec<SessionProc> = Vec::new();
    let hit = {
        let mut g = POOL.lock();
        to_reap.extend(g.sweep_expired(now_ms(), session_busy));
        to_reap.extend(g.trim_over_cap(&conv_id, session_busy));
        match g.get_mut(&conv_id) {
            Some((cur_fp, sp)) if *cur_fp == fp && sp.shared.alive.load(Ordering::SeqCst) => true,
            Some(_) => {
                // 指纹变了或进程已死: 摘出去重建 —— 与 run_persistent_turn 同一套规则。
                to_reap.extend(g.remove(&conv_id));
                false
            }
            None => false,
        }
    };
    for s in to_reap {
        reap(s, true);
    }
    if hit {
        return;
    }

    // ── spawn(锁外, 数秒) + 入池 ──
    let proc = match spawn_session(&p) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("[persistent-prewarm conv={conv_id}] 预热 spawn 失败(忽略): {e}");
            return;
        }
    };
    // spawn 期间用户可能已真发消息、run_persistent_turn 抢先建好了会话 —— 此时绝不能
    // 用预热进程顶掉现役者(它可能正跑着一轮), 尊重现役、丢弃预热品。
    let (discard, evicted) = {
        let mut g = POOL.lock();
        let occupied = g
            .get_mut(&conv_id)
            .is_some_and(|(_, sp)| sp.shared.alive.load(Ordering::SeqCst));
        if occupied {
            (Some(proc), Vec::new())
        } else {
            (None, g.insert(conv_id.clone(), fp, proc, now_ms(), session_busy))
        }
    };
    if let Some(s) = discard {
        reap(s, true);
    }
    for s in evicted {
        reap(s, true);
    }
}

/// 常驻路径主入口: 检出/重建会话进程 → 写本轮消息 → 挂 job + 看门狗。
/// 进程死亡/写失败 → 透明重建并把本条按「首条」处理(不向用户报错), 最多重试 2 次。
pub(super) fn run_persistent_turn(p: TurnParams) -> Result<(), String> {
    let fp = fingerprint_of(&p);

    let mut last_err = String::from("(未知)");
    for _attempt in 0..2 {
        // ── 1. 池内检出: TTL 惰性收割 + 指纹/死活校验(锁内只摘, 杀在锁外) ──
        let mut to_reap: Vec<SessionProc> = Vec::new();
        let mut reusable = false;
        {
            let mut g = POOL.lock();
            to_reap.extend(g.sweep_expired(now_ms(), session_busy));
            to_reap.extend(g.trim_over_cap(&p.conv_id, session_busy));
            if let Some((cur_fp, sp)) = g.get_mut(&p.conv_id) {
                if *cur_fp == fp && sp.shared.alive.load(Ordering::SeqCst) {
                    reusable = true;
                    // 就在锁内把本轮登记上(见下方 insert 处的大注释):否则从这里放锁
                    // 到第 5 步真正注册之间,这个会话在 session_busy 眼里是「空闲」,
                    // 会被另一条并发对话的 trim_over_cap/insert 当 LRU 牺牲品逐掉。
                    *sp.shared.active_req.lock() = Some(p.req_id.clone());
                } else {
                    // 指纹变了(权限/供应商/模型/创作切换)或进程已死 → 杀旧重建
                    to_reap.extend(g.remove(&p.conv_id));
                }
            }
        }
        for s in to_reap {
            reap(s, true);
        }

        // ── 2. 需要则重спавн(锁外 spawn, 插入时 LRU 逐出的旧会话也在锁外收割) ──
        if !reusable {
            let proc = match spawn_session(&p) {
                Ok(x) => x,
                Err(e) => {
                    last_err = e;
                    continue;
                }
            };
            // ⚠ 本轮登记必须**先于入池**。
            //
            // session_busy = alive && active_req.is_some(),而新 spawn 出来的会话
            // active_req 还是 None —— 旧写法要等到第 5 步才登记。于是「入池 → 检出」
            // 这段窗口里,它在所有逐出谓词眼里都是一个「空闲会话」:
            //   · 另一条并发对话第 1 步的 trim_over_cap 会挑中它(它 last_used 最老);
            //   · 另一条并发对话第 2 步的 insert 也会挑中它(insert 只护自己那一个新条目)。
            // MAX_SESSIONS=2,所以只要同时新建 3 条以上对话,落后的那些必被抢先者收割,
            // 到第 3 步就报「会话条目在检出后消失(并发收割)」——2 次重试打光后用户直接
            // 看到发送失败。2026-07-29 压测实测:4 路并发新建对话 3/4 失败,9 路 8/9 失败。
            //
            // 登记提前后它一入池就是「忙碌」,谁也逐不走;所有失败路径(stdin 写失败 /
            // reader 已退出 / 条目消失)本来就会 remove+reap,reap 置 alive=false,
            // 而 session_busy 要求 alive —— 不会留下逐不掉的僵尸条目。
            *proc.shared.active_req.lock() = Some(p.req_id.clone());
            let evicted =
                POOL.lock()
                    .insert(p.conv_id.clone(), fp.clone(), proc, now_ms(), session_busy);
            for s in evicted {
                reap(s, true);
            }
        }

        // ── 3. 取会话句柄 + 决定首条/续轮文本 ──
        let (stdin, job_tx, shared, sent, is_first) = {
            let mut g = POOL.lock();
            let Some((_, sp)) = g.get_mut(&p.conv_id) else {
                last_err = "会话条目在检出后消失(并发收割)".into();
                continue;
            };
            (
                sp.stdin.clone(),
                sp.job_tx.clone(),
                sp.shared.clone(),
                sp.sent_keys.clone(),
                !sp.first_done,
            )
        };
        let text = if is_first {
            p.plan.full_text()
        } else {
            p.plan.continuation_text(&sent)
        };
        // stream-json 输入协议: 每轮一行 user 消息。serde 负责转义, 天然单行。
        let line = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": [{ "type": "text", "text": text }] }
        })
        .to_string();

        // ── 4. 写 stdin(锁外; 失败 = 进程死了 → 杀掉重来, 本条按首条) ──
        let write_ok = {
            let mut si = stdin.lock();
            si.write_all(line.as_bytes())
                .and_then(|_| si.write_all(b"\n"))
                .and_then(|_| si.flush())
        };
        if let Err(e) = write_ok {
            last_err = format!("常驻进程 stdin 写失败: {e}");
            if let Some(s) = POOL.lock().remove(&p.conv_id) {
                reap(s, true);
            }
            continue;
        }
        // 写成功才记账: sent_keys 并入本轮门控 key、置 first_done、touch LRU。
        {
            let mut g = POOL.lock();
            if let Some((_, sp)) = g.get_mut(&p.conv_id) {
                for k in p.plan.keys() {
                    sp.sent_keys.insert(k);
                }
                sp.first_done = true;
            }
            g.touch(&p.conv_id, now_ms());
        }

        // ── 5. 注册本轮 + 派发 job + 挂看门狗 ──
        let wd = Arc::new(polaris_watchdog::Watchdog::new(
            polaris_watchdog::WatchdogConfig {
                idle_secs: pipeline::watchdog_config().0,
                hard_cap_secs: pipeline::watchdog_config().1,
            },
        ));
        *shared.active_req.lock() = Some(p.req_id.clone());
        *shared.watchdog.lock() = Some(wd.clone());
        let job = TurnJob {
            req_id: p.req_id.clone(),
            conv_id: p.conv_id.clone(),
            app: p.app.clone(),
            art_dir: p.art_dir.clone(),
            art_before: p.art_before.clone(),
            image_notice: p.image_notice.clone(),
            watchdog: wd.clone(),
        };
        if job_tx.send(job).is_err() {
            // reader 线程已死(进程 EOF) → 同 stdin 失败: 杀掉重来
            *shared.active_req.lock() = None;
            last_err = "常驻进程 reader 已退出".into();
            if let Some(s) = POOL.lock().remove(&p.conv_id) {
                reap(s, true);
            }
            continue;
        }
        let (idle_secs, hard_cap) = pipeline::watchdog_config();
        if idle_secs > 0 || hard_cap > 0 {
            spawn_turn_watchdog(
                shared.clone(),
                wd,
                p.app.clone(),
                p.req_id.clone(),
                p.conv_id.clone(),
                hard_cap,
            );
        }

        // 停止在「注册前窄窗口」到达的兜底(同旧路径 spawn 后补查): 有标记即刻杀会话,
        // reader 收 EOF 静默收尾发 done。
        if CHILDREN.take_cancel(&p.req_id) {
            cancel_by_req(&p.req_id);
        }
        return Ok(());
    }
    Err(format!("常驻 claude 进程不可用(已重试): {last_err}"))
}

// ───────────────────────── spawn + 常驻线程 ─────────────────────────

/// 起一个常驻会话进程: spawn 参数全套沿用旧路径(process_group/no_window/
/// harden_child_env/scope_child_claude_by_id 都在 spawn_on_host 里), 只多
/// `--input-format stream-json` 让 stdin 变成逐轮消息通道而非一次性 prompt。
fn spawn_session(p: &TurnParams) -> Result<SessionProc, String> {
    let mut child = pipeline::spawn_on_host(
        "",
        &p.perm,
        &p.art_dir,
        p.with_task,
        p.work_full,
        p.provider_id.as_deref(),
        true, // persistent: 加 --input-format stream-json
        p.work_dir.as_deref().map(std::path::Path::new),
    )?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "常驻 claude 子进程没有 stdin".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "常驻 claude 子进程没有 stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "常驻 claude 子进程没有 stderr".to_string())?;
    let shared = Arc::new(SessionShared {
        conv: p.conv_id.clone(),
        pid: child.id(),
        alive: AtomicBool::new(true),
        kill_reported: AtomicBool::new(false),
        active_req: Mutex::new(None),
        watchdog: Mutex::new(None),
        stderr_tail: Mutex::new(String::new()),
    });
    let (tx, rx) = mpsc::channel::<TurnJob>();
    {
        let sh = shared.clone();
        std::thread::spawn(move || reader_loop(stdout, rx, sh));
    }
    {
        let sh = shared.clone();
        let app = p.app.clone();
        std::thread::spawn(move || stderr_loop(stderr, sh, app));
    }
    Ok(SessionProc {
        child: Some(child),
        stdin: Arc::new(Mutex::new(stdin)),
        job_tx: tx,
        shared,
        sent_keys: HashSet::new(),
        first_done: false,
    })
}

/// 会话级 stdout reader: 常驻在管道上, 每收到一个 TurnJob 就以该轮上下文消费事件流,
/// 直到 result 事件(本轮收尾)或 EOF(进程死亡)。事件解析/合批/落库全复用旧路径的
/// TurnStream(pipeline.rs), 首尾事件契约(meta→delta/tool/artifact/error→done)不变。
/// 轮与轮之间 reader 阻塞在 job 队列上, 不读 stdout —— 进程 idle 期偶发的杂散行
/// (init/system 事件)留在管道缓冲, 下一轮开头被读到并按未知事件忽略, 无副作用。
fn reader_loop(stdout: ChildStdout, rx: mpsc::Receiver<TurnJob>, shared: Arc<SessionShared>) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(job) = rx.recv() {
        let mut ts = pipeline::TurnStream::new(
            job.app.clone(),
            job.req_id.clone(),
            Some(job.conv_id.clone()),
            job.image_notice.clone(),
        );
        let mut got_result = false;
        for line in lines.by_ref() {
            let Ok(line) = line else { continue };
            if line.trim().is_empty() {
                continue;
            }
            job.watchdog.touch(); // 有产出就不算挂死(看门狗 idle 判据)
            if ts.feed_line(&line) {
                got_result = true; // result 事件 = 本轮收尾界标
                break;
            }
        }
        ts.flush();
        // 先清活跃标记再收尾: 看门狗以 active_req 清空为「本轮结束」, 之后的轮间
        // 静默不受 idle 判定管辖(常驻进程等下一条消息是常态, 不是卡死)。
        *shared.active_req.lock() = None;
        *shared.watchdog.lock() = None;
        // 轮次收尾续 TTL: last_used 只在轮次**开始**时刷新, 一轮跑得比 TTL(10min)久的
        // 长任务结束瞬间就已"过期", busy 豁免也随 active_req 清空失效 —— 任何别的对话
        // 一发消息, 惰性清扫就会把这个刚跑完、还带着服务端 prompt cache 的热进程收割掉,
        // 用户续问被迫重建(6.4s + 丢缓存)。这里补一次 touch, 给完整 TTL 窗口。
        // 锁序: 此刻不持任何锁, 单独取 POOL, 与全局 POOL→active_req 顺序不冲突。
        POOL.lock().touch(&shared.conv, now_ms());
        if !got_result {
            // EOF = 进程死亡。主动杀(取消/看门狗/退出)已各自告知过前端, 这里只对
            // 「自然猝死」补一条 error; 随后照常 finish 发 done, 前端正常收尾,
            // 下一轮 run_persistent_turn 检出 alive=false 自动重建。
            shared.alive.store(false, Ordering::SeqCst);
            if !shared.kill_reported.load(Ordering::SeqCst) {
                let tail = shared.stderr_tail.lock().clone();
                pipeline::emit_event(
                    &job.app,
                    ChatStreamEvent {
                        req_id: job.req_id.clone(),
                        kind: "error".into(),
                        text: Some(format!(
                            "claude 常驻进程中途退出, 本轮中断(下一条消息将自动重建)\n--- stderr ---\n{}",
                            if tail.is_empty() { "(stderr 为空)" } else { &tail }
                        )),
                        tool: None,
                        conversation_id: Some(job.conv_id.clone()),
                    },
                );
            }
        }
        ts.finish(&job.art_dir, &job.art_before);
        if !got_result {
            break; // stdout 已到 EOF, reader 使命结束; 池条目由下轮/收割路径清理
        }
    }
}

/// 会话级 stderr reader: 行为对齐旧路径(每行 emit error 事件 + touch 看门狗),
/// 只是错误要挂到「当前活跃轮」上; 轮间的 stderr 没有归属 req, 只进尾巴缓冲。
fn stderr_loop(stderr: ChildStderr, shared: Arc<SessionShared>, app: AppHandle) {
    const MAX_TAIL: usize = 64 * 1024;
    let reader = BufReader::new(stderr);
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(wd) = shared.watchdog.lock().clone() {
            wd.touch();
        }
        {
            let mut tail = shared.stderr_tail.lock();
            if tail.len() < MAX_TAIL {
                tail.push_str(&line);
                tail.push('\n');
            }
        }
        if let Some(req) = shared.active_req.lock().clone() {
            pipeline::emit_event(
                &app,
                ChatStreamEvent {
                    req_id: req,
                    kind: "error".into(),
                    text: Some(format!("[stderr] {line}")),
                    tool: None,
                    conversation_id: Some(shared.conv.clone()),
                },
            );
        } else {
            eprintln!("[persistent-agent conv={}] stderr: {line}", shared.conv);
        }
    }
}

/// 本轮看门狗线程: 计时核仍是 polaris-watchdog(idle+硬顶), 判据与旧路径一致
/// (含 sample_tree 深检否决权)。与旧路径的关键差异: **以 result 事件为界** ——
/// active_req 被 reader 清空即本轮结束, 线程退出; 轮间静默永不触发 idle 杀。
/// 触发 → 杀常驻进程整树并告知前端, 下轮自动重建。
fn spawn_turn_watchdog(
    shared: Arc<SessionShared>,
    wd: Arc<polaris_watchdog::Watchdog>,
    app: AppHandle,
    req_id: String,
    conv_id: String,
    hard_cap: u64,
) {
    std::thread::spawn(move || {
        use polaris_watchdog::WatchdogVerdict;
        let base_tick = std::time::Duration::from_secs(5);
        let mut tick = base_tick;
        let mut last_cpu: Option<u64> = None;
        loop {
            std::thread::sleep(tick);
            // 本轮已收尾(result 到达)或进程已亡 → 看门狗使命结束。
            if !shared.alive.load(Ordering::SeqCst)
                || shared.active_req.lock().as_deref() != Some(req_id.as_str())
            {
                break;
            }
            let verdict = wd.check();
            let over_cap = matches!(verdict, Some(WatchdogVerdict::HardTimeout { .. }));
            let idle = wd.idle();
            match verdict {
                None => {
                    last_cpu = None;
                    tick = base_tick;
                    continue;
                }
                Some(WatchdogVerdict::IdleKilled { idle_secs }) => {
                    // 深检否决权(同旧路径): 有活子孙或 CPU 在推进 = 静默但在干活, 不杀。
                    if let Some(s) = pipeline::sample_tree(shared.pid) {
                        let cpu_advancing = last_cpu.is_none_or(|prev| s.cpu > prev);
                        last_cpu = Some(s.cpu);
                        if s.descendants > 0 || cpu_advancing {
                            tick = std::time::Duration::from_secs(30);
                            continue;
                        }
                        eprintln!(
                            "[persistent-watchdog] req={req_id} 空闲 {idle_secs}s 且进程树静止, 判挂死回收常驻进程"
                        );
                    } else {
                        eprintln!(
                            "[persistent-watchdog] req={req_id} 空闲 {idle_secs}s, 深检不可用, 回收常驻进程"
                        );
                    }
                }
                Some(WatchdogVerdict::HardTimeout { elapsed_secs }) => {
                    eprintln!(
                        "[persistent-watchdog] req={req_id} 本轮总时长 {elapsed_secs}s 超硬顶 {hard_cap}s, 回收常驻进程"
                    );
                }
            }
            // 再确认仍是本轮活跃进程才杀(防收尾/取消竞态), 摘出后锁外收割。
            let victim = {
                let mut g = POOL.lock();
                let same = g
                    .get_mut(&conv_id)
                    .is_some_and(|(_, sp)| Arc::ptr_eq(&sp.shared, &shared));
                if same && shared.active_req.lock().as_deref() == Some(req_id.as_str()) {
                    g.remove(&conv_id)
                } else {
                    None
                }
            };
            if let Some(s) = victim {
                // 先杀干净再告知(同旧路径缘由: 防旧进程缓冲尾巴混进 error 之后的新气泡);
                // reap 已置 kill_reported → reader 收 EOF 只发 done 静默收尾。
                reap(s, true);
                let reason = if over_cap {
                    format!("任务超硬顶时限: 本轮总时长超过 {hard_cap}s, 已被看门狗强制终止(常驻进程将在下一条消息自动重建)")
                } else {
                    format!(
                        "空闲超时, 进程疑似卡死已终止: claude 进程连续 {}s 无任何输出且进程树静止(下一条消息自动重建)",
                        idle.as_secs()
                    )
                };
                pipeline::emit_event(
                    &app,
                    ChatStreamEvent {
                        req_id: req_id.clone(),
                        kind: "error".into(),
                        text: Some(reason),
                        tool: None,
                        conversation_id: Some(conv_id.clone()),
                    },
                );
            }
            break;
        }
    });
}

// ───────────────────────── 单测 (纯记账, 时钟注入) ─────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(perm: &str, model: &str) -> Fingerprint {
        Fingerprint {
            perm: perm.into(),
            provider_id: "auto".into(),
            work_full: false,
            with_task: false,
            model: model.into(),
            add_dirs: vec!["/kb".into()],
            creative: false,
            cwd: String::new(),
        }
    }

    /// 指纹比较: 任一字段变化都判不等 → 触发杀旧重建。
    #[test]
    fn fingerprint_change_detected() {
        let a = fp("acceptEdits", "");
        assert_eq!(a, fp("acceptEdits", ""));
        assert_ne!(a, fp("plan", ""), "权限档位变化必须判不等");
        assert_ne!(a, fp("acceptEdits", "opus"), "model 变化必须判不等");
        let mut b = fp("acceptEdits", "");
        b.add_dirs.push("/art".into());
        assert_ne!(a, b, "add-dirs 变化必须判不等");
        let mut c = fp("acceptEdits", "");
        c.creative = true;
        assert_ne!(a, c, "创作模式翻转必须判不等(前置指令取向不同)");
        let mut d = fp("acceptEdits", "");
        d.cwd = "/home/me/big-project".into();
        assert_ne!(a, d, "项目工作目录(cwd)变化必须判不等(cwd 进程中途改不了, 须杀旧重建)");
    }

    /// 无忙碌者的默认谓词(旧行为)。
    fn idle_only(_: &u32) -> bool {
        false
    }

    /// LRU 上限 2: 第 3 个会话插入时, 最久未用的被逐出; touch 能续命。
    #[test]
    fn lru_evicts_least_recently_used() {
        let mut core: PoolCore<u32> = PoolCore::new();
        assert!(core.insert("a".into(), fp("p", ""), 1, 0, idle_only).is_empty());
        assert!(core.insert("b".into(), fp("p", ""), 2, 100, idle_only).is_empty());
        // touch a → b 变成最久未用
        core.touch("a", 200);
        let evicted = core.insert("c".into(), fp("p", ""), 3, 300, idle_only);
        assert_eq!(evicted, vec![2], "应逐出最久未用的 b(payload=2)");
        assert!(core.get_mut("a").is_some());
        assert!(core.get_mut("b").is_none());
        assert!(core.get_mut("c").is_some());
    }

    /// 同 conv 重复插入不叠加条目(旧 payload 作为被逐出者返回)。
    #[test]
    fn reinsert_same_conv_replaces() {
        let mut core: PoolCore<u32> = PoolCore::new();
        core.insert("a".into(), fp("p", ""), 1, 0, idle_only);
        let evicted = core.insert("a".into(), fp("q", ""), 9, 10, idle_only);
        assert_eq!(evicted, vec![1]);
        let (f, v) = core.get_mut("a").unwrap();
        assert_eq!((f.perm.as_str(), *v), ("q", 9));
    }

    /// TTL 惰性收割: 空闲 ≥10 分钟的条目被摘出, 新鲜的留下。时钟注入, 不 sleep。
    #[test]
    fn ttl_sweep_reaps_idle_sessions() {
        let mut core: PoolCore<u32> = PoolCore::new();
        core.insert("old".into(), fp("p", ""), 1, 0, idle_only);
        core.insert("new".into(), fp("p", ""), 2, 5 * 60 * 1000, idle_only);
        let reaped = core.sweep_expired(10 * 60 * 1000, idle_only); // old 恰满 10min, new 才 5min
        assert_eq!(reaped, vec![1]);
        assert!(core.get_mut("old").is_none());
        assert!(core.get_mut("new").is_some());
        // touch 后不再过期
        core.touch("new", 14 * 60 * 1000);
        assert!(core.sweep_expired(20 * 60 * 1000, idle_only).is_empty());
    }

    /// 长任务保命核心: 正在跑轮次(busy)的会话, TTL 到点也**绝不**被清扫;
    /// 轮次结束(不再 busy)后才可回收。
    #[test]
    fn ttl_sweep_never_reaps_busy_session() {
        let mut core: PoolCore<u32> = PoolCore::new();
        core.insert("longtask".into(), fp("p", ""), 1, 0, idle_only);
        // 轮次开跑 7 小时后别的对话触发清扫: busy 豁免, 条目必须还在
        let reaped = core.sweep_expired(7 * 3600 * 1000, |v| *v == 1);
        assert!(reaped.is_empty(), "进行中的长任务绝不能被 TTL 误杀");
        assert!(core.get_mut("longtask").is_some());
        // 轮次结束后同一时刻再扫: 正常回收
        let reaped = core.sweep_expired(7 * 3600 * 1000, idle_only);
        assert_eq!(reaped, vec![1]);
    }

    /// 长任务保命之二: LRU 逐出只挑非忙碌者; 全员忙碌时容忍超上限, 不杀活轮。
    #[test]
    fn lru_never_evicts_busy_session() {
        let mut core: PoolCore<u32> = PoolCore::new();
        core.insert("a".into(), fp("p", ""), 1, 0, idle_only);
        core.insert("b".into(), fp("p", ""), 2, 100, idle_only);
        // a 最久未用但正在跑轮次 → 逐出的必须是 b
        let evicted = core.insert("c".into(), fp("p", ""), 3, 200, |v| *v == 1);
        assert_eq!(evicted, vec![2], "忙碌的 a 不可逐出, 应逐出空闲的 b");
        // a、c 都忙 → 第 4 个插入不逐出任何人(包括 d 自己), 暂时超上限
        let evicted = core.insert("d".into(), fp("p", ""), 4, 300, |v| *v == 1 || *v == 3);
        assert!(evicted.is_empty(), "其余全员忙碌时宁可超上限也不杀活轮");
        assert!(core.get_mut("a").is_some() && core.get_mut("c").is_some());
        assert!(core.get_mut("d").is_some(), "新插入者不可被自己逐出");
    }

    /// 并发新建对话不得互相收割(2026-07-29 压测实测的 P0 回归钉子)。
    ///
    /// 旧行为:新 spawn 的会话要到「写完 stdin、派发 job」那步才登记 active_req,
    /// 在此之前 session_busy 判它空闲 → 另一条并发新对话的 trim_over_cap/insert
    /// 顺手就把它逐掉,受害者回头检出时报「会话条目在检出后消失(并发收割)」,
    /// 重试打光后用户看到发送失败(4 路并发 3/4 失败,9 路 8/9 失败)。
    /// 修法是把本轮登记提前到入池之前 —— 这里用「入池即忙碌」来复现那个不变量。
    #[test]
    fn 并发新建的会话入池即忙碌_不会被彼此逐出() {
        let mut core: PoolCore<u32> = PoolCore::new();
        // 三条对话几乎同时新建;每条一入池就算「忙碌」(= 已登记本轮)。
        let all_busy = |_: &u32| true;
        for (i, c) in ["c1", "c2", "c3"].iter().enumerate() {
            let evicted = core.insert((*c).into(), fp("p", ""), i as u32, i as u64, all_busy);
            assert!(
                evicted.is_empty(),
                "{c} 入池不该逐掉任何一条已登记的并发新对话,实际逐出 {evicted:?}"
            );
        }
        for c in ["c1", "c2", "c3"] {
            assert!(core.get_mut(c).is_some(), "{c} 必须还在池里(否则检出会落空)");
        }
        // 另一条对话的检出路径也不能把它们收走。
        assert!(
            core.trim_over_cap("c3", all_busy).is_empty(),
            "全员忙碌时 trim_over_cap 必须原地等待"
        );
        assert_eq!(core.entries.len(), 3, "宁可暂时超编,也不能杀掉在飞的轮次");

        // 反证:若登记没提前(入池时仍判空闲),第三条就会把第一条挤掉 —— 那正是旧 bug。
        let mut old: PoolCore<u32> = PoolCore::new();
        old.insert("c1".into(), fp("p", ""), 1, 0, idle_only);
        old.insert("c2".into(), fp("p", ""), 2, 1, idle_only);
        let evicted = old.insert("c3".into(), fp("p", ""), 3, 2, idle_only);
        assert_eq!(evicted, vec![1], "复现旧行为:未登记的新会话会被后来者收割");
    }

    /// 超编收敛闭环: 全员忙碌导致超上限后, 轮次结束(不再忙)的下一次检出必须把编制
    /// 收回上限 —— 只逐「最久未用 · 非忙碌 · 非当前 conv」, 忙碌期间则原地等待。
    #[test]
    fn trim_over_cap_converges_after_turns_end() {
        let mut core: PoolCore<u32> = PoolCore::new();
        core.insert("a".into(), fp("p", ""), 1, 0, idle_only);
        core.insert("b".into(), fp("p", ""), 2, 100, idle_only);
        // a、b 忙时插入 c → 超编到 3
        core.insert("c".into(), fp("p", ""), 3, 200, |v| *v == 1 || *v == 2);
        assert_eq!(core.entries.len(), 3, "前置: 全员忙碌导致超编");
        // 全员仍忙 → 不裁(当前 conv=c)
        assert!(core
            .trim_over_cap("c", |v| *v == 1 || *v == 2)
            .is_empty());
        assert_eq!(core.entries.len(), 3);
        // 轮次结束(无人忙碌) → 检出 c 时收敛回 2, 逐出最久未用的 a
        let evicted = core.trim_over_cap("c", idle_only);
        assert_eq!(evicted, vec![1], "应逐出最久未用的 a");
        assert_eq!(core.entries.len(), 2);
        assert!(core.get_mut("b").is_some() && core.get_mut("c").is_some());
        // 当前 conv 自己是最久未用者时不可被逐出
        let mut core2: PoolCore<u32> = PoolCore::new();
        core2.insert("x".into(), fp("p", ""), 7, 0, idle_only);
        core2.insert("y".into(), fp("p", ""), 8, 10, |v| *v == 7);
        core2.insert("z".into(), fp("p", ""), 9, 20, |v| *v == 7 || *v == 8);
        let ev = core2.trim_over_cap("x", idle_only);
        assert_eq!(ev, vec![8], "keep_conv=x 受保护, 逐出次久未用的 y");
    }

    /// PromptPlan 首条/续轮拆分: 续轮剔除 Session 段与已发过的 Keyed 段, 保留 Turn 段。
    #[test]
    fn prompt_plan_split_first_vs_continuation() {
        let mut plan = PromptPlan::new();
        plan.session("SYS|".into());
        plan.keyed("script", "SCRIPT|".into());
        plan.turn("RECALL|".into());
        plan.keyed("download", "DL|".into());
        plan.turn("Q1".into());
        assert_eq!(plan.full_text(), "SYS|SCRIPT|RECALL|DL|Q1");
        // 首轮把 script 发过了, download 是本轮新触发 → 续轮只带 未发过的 keyed + turn
        let sent: HashSet<String> = ["script".to_string()].into_iter().collect();
        assert_eq!(plan.continuation_text(&sent), "RECALL|DL|Q1");
        let all: HashSet<String> = plan.keys().into_iter().collect();
        assert_eq!(all.len(), 2);
        // 全部发过后, 续轮只剩每轮都变的部分
        assert_eq!(plan.continuation_text(&all), "RECALL|Q1");
    }
}
