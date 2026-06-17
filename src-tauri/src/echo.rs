//! 寓言计划 · 回声层(Echo)—— 对话沉淀 / 每日做梦
//!
//! 出处:桌面《寓言计划-PRD-v5.html》§6 + 《Polaris-记忆系统升级方案》。
//! 爆改原则:「思想全要,代码全不要」——
//! - Mem0 的两阶段抽取   → 一条蒸馏提示词 + 决策 JSON(kb_dedup 的现成模式换提示词);
//! - Letta 的可修订记忆块 → memory/ 下每条 md 一个块,UPDATE 前旧版进 memory/.history/;
//! - Graphiti 的时序失效  → frontmatter `supersedes` 字段约定,零引擎。
//!
//! 「做梦」= 每日定时(默认凌晨 3 点)把当天的对话蒸馏成 feedback-episode /
//! 稳定事实,写进 KB 的第四车道 `memory/`(注入只给 index 地图,不进 wiki 全文区)。
//! AI 出决策(只读 claude 输出 JSON),代码执行改动(Rust 写盘)—— 模型物理上无写权。

use directories::UserDirs;
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "desktop")]
use tauri::{AppHandle, Emitter};
#[cfg(not(feature = "desktop"))]
use crate::host::AppHandle;

// ───────────────────────── 配置与状态 ─────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EchoCfg {
    /// 每日做梦总开关(**默认开** —— 让 AI 开箱即「越用越懂你」;可在「自动化」里关)
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 做梦钟点(本地时区 0–23,默认 8 = 早上八点;此时机器多半已开机)
    #[serde(default = "default_hour")]
    pub hour: u8,
    /// 开机补做:错过了当天的做梦钟点(如机器当时关着),开机后稍候自动补一次(每天仅一次)。
    /// 实现「开机就立马执行 / 每天固定时间」二选一都能命中。默认开。
    #[serde(default = "default_true")]
    pub run_on_boot: bool,
    /// 最近一次做梦的本地日期(YYYY-MM-DD),用于「一天只梦一次」
    #[serde(default)]
    pub last_dream_day: String,
    /// 最近一次做梦时刻(ms),作为下次取材的起点
    #[serde(default)]
    pub last_dream_ms: i64,
    #[serde(default)]
    pub log: Vec<DreamLog>,
}

fn default_hour() -> u8 {
    8
}
fn default_true() -> bool {
    true
}

impl Default for EchoCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            hour: 8,
            run_on_boot: true,
            last_dream_day: String::new(),
            last_dream_ms: 0,
            log: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamLog {
    pub ts: i64,
    pub day: String,
    /// 本次蒸馏写入/修订的记忆条数
    pub episodes: usize,
    pub summary: String,
}

static CFG: Lazy<RwLock<EchoCfg>> = Lazy::new(|| RwLock::new(EchoCfg::default()));
static DREAMING: AtomicBool = AtomicBool::new(false);

fn cfg_path() -> Option<PathBuf> {
    UserDirs::new().map(|u| u.home_dir().join("Polaris").join("data").join("echo.json"))
}

fn load_cfg() {
    let cfg = cfg_path()
        .filter(|p| p.exists())
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    *CFG.write() = cfg;
}

fn persist_cfg() {
    let Some(path) = cfg_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let txt = serde_json::to_string_pretty(&*CFG.read()).unwrap_or_default();
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, &txt).is_ok() {
        let _ = fs::rename(&tmp, &path);
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn local_day() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}
fn local_hour() -> u8 {
    use chrono::Timelike;
    chrono::Local::now().hour() as u8
}

// ───────────────────────── 事件 ─────────────────────────

#[derive(Debug, Clone, Serialize)]
struct DreamEvent {
    kind: String, // phase | delta | done | error
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    episodes: Option<usize>,
}

fn emit_dream(app: &AppHandle, kind: &str, text: Option<String>, episodes: Option<usize>) {
    let _ = app.emit("echo:dream", DreamEvent { kind: kind.into(), text, episodes });
}

// ───────────────────────── 命令 ─────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct EchoStatus {
    pub enabled: bool,
    pub hour: u8,
    pub run_on_boot: bool,
    pub last_dream_day: String,
    pub dreaming: bool,
    /// memory/ 车道里的记忆条数
    pub memory_count: usize,
    /// 今天的晨报建议条数(未忽略的)
    pub briefing_today: usize,
    pub log: Vec<DreamLog>,
}

fn memory_root() -> PathBuf {
    PathBuf::from(crate::kb::kb_root()).join("memory")
}

fn count_memories() -> usize {
    let root = memory_root();
    if !root.exists() {
        return 0;
    }
    walkdir::WalkDir::new(&root)
        .into_iter()
        .flatten()
        .filter(|e| {
            e.path().is_file()
                && e.path().extension().map(|x| x == "md").unwrap_or(false)
                && e.file_name() != "index.md"
                && !e.path().components().any(|c| c.as_os_str() == ".history")
        })
        .count()
}

// ───────────────────────── 晨报建议(每日工程化建议)─────────────────────────
//
// 「让 AI 更懂你」最锋利的一刀。内容产生原理(三步,见 kb_portrait + suggest_directive):
//   ① **观察整座知识库** → 全库画像(四层家底 / 语言·类型分布 / AI 命名的主题簇);
//   ② 据画像在心里勾出**用户画像**(主人是谁 / 在做什么 / 关注与擅长什么);
//   ③ 落到近期新内容 + 搁置老项目, 产出 ≤N 条既贴画像、又指到具体素材的可落地建议。
// 结果落进 memory/briefing/<date>.json, 对话框顶部呈现, 点「让我去做」= 把 action 当 prompt
// 发起一轮对话。关键升级:不再只盯昨天的零碎 —— 只要库里有东西, 建议就长在「主人是谁」之上。

/// 一条晨报建议(写盘 + 给前端)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    pub id: String,
    /// 一句话建议
    pub title: String,
    /// 类别:progress(新进展)/ wrapup(收尾搁置)/ workflow(可复用工作流)/ organize(整理资料)
    /// —— 前端据此选图标与配色。空则按默认展示。
    #[serde(default)]
    pub kind: String,
    /// 依据来源的简短标签(某段对话标题 / 某份文件名 / 某个老项目名,≤16 字)——「懂你」的落点。
    #[serde(default)]
    pub source: String,
    /// 基于哪条新内容(引文/文件名)
    #[serde(default)]
    pub why: String,
    /// 具体怎么做、需要哪些东西
    #[serde(default)]
    pub how: String,
    /// 一键动作:点「让我去做」直接作为新对话的 prompt 发起
    #[serde(default)]
    pub action: String,
    /// 用户已忽略(不再在对话框顶部展示)
    #[serde(default)]
    pub dismissed: bool,
}

/// AI 蒸馏输出的建议(无 id/dismissed,由 Rust 补)。
#[derive(Debug, Deserialize)]
struct SuggestionIn {
    title: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    why: String,
    #[serde(default)]
    how: String,
    #[serde(default)]
    action: String,
}

fn briefing_dir() -> PathBuf {
    memory_root().join("briefing")
}
fn briefing_file(day: &str) -> PathBuf {
    briefing_dir().join(format!("{day}.json"))
}

fn read_briefing(day: &str) -> Vec<Suggestion> {
    fs::read_to_string(briefing_file(day))
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<Suggestion>>(&t).ok())
        .unwrap_or_default()
}

fn write_briefing(day: &str, items: &[Suggestion]) {
    let dir = briefing_dir();
    let _ = fs::create_dir_all(&dir);
    let path = briefing_file(day);
    if let Ok(txt) = serde_json::to_string_pretty(items) {
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, &txt).is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
    }
}

/// 今天未忽略的建议条数(给 EchoStatus 显示)。
fn briefing_today_count() -> usize {
    read_briefing(&local_day()).into_iter().filter(|s| !s.dismissed).count()
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn echo_status() -> EchoStatus {
    let cfg = CFG.read();
    EchoStatus {
        enabled: cfg.enabled,
        hour: cfg.hour,
        run_on_boot: cfg.run_on_boot,
        last_dream_day: cfg.last_dream_day.clone(),
        dreaming: DREAMING.load(Ordering::Relaxed),
        memory_count: count_memories(),
        briefing_today: briefing_today_count(),
        log: cfg.log.clone(),
    }
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn echo_set(enabled: Option<bool>, hour: Option<u8>, run_on_boot: Option<bool>) -> EchoStatus {
    {
        let mut cfg = CFG.write();
        if let Some(e) = enabled {
            cfg.enabled = e;
        }
        if let Some(h) = hour {
            cfg.hour = h.min(23);
        }
        if let Some(b) = run_on_boot {
            cfg.run_on_boot = b;
        }
    }
    persist_cfg();
    echo_status()
}

/// 手动「现在做一次梦」(全量:取最近所有新对话)。后台线程跑,进度走 `echo:dream` 事件。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn echo_dream_now(app: AppHandle) -> Result<(), String> {
    spawn_dream(app, true)
}

/// 把**单条对话**立刻沉淀为记忆(侧栏「⋯ › 沉淀为记忆」)。复用做梦管线,
/// 但不动每日调度状态(last_dream_day/ms)——它是一次性的手动沉淀,不顶替「今天的梦」。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn echo_distill_conversation(app: AppHandle, conv_id: String) -> Result<(), String> {
    if DREAMING.swap(true, Ordering::SeqCst) {
        return Err("正在沉淀中,请稍候".into());
    }
    std::thread::spawn(move || {
        let result = distill_one(&app, &conv_id);
        finish_job(&app, result, false);
    });
    Ok(())
}

/// 今天未忽略的晨报建议(对话框顶部展示用)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn echo_briefing_today() -> Vec<Suggestion> {
    read_briefing(&local_day()).into_iter().filter(|s| !s.dismissed).collect()
}

/// 忽略一条建议(不再展示;幂等)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn echo_briefing_dismiss(id: String) -> Vec<Suggestion> {
    let day = local_day();
    let mut items = read_briefing(&day);
    let mut changed = false;
    for s in items.iter_mut() {
        if s.id == id && !s.dismissed {
            s.dismissed = true;
            changed = true;
        }
    }
    if changed {
        write_briefing(&day, &items);
    }
    items.into_iter().filter(|s| !s.dismissed).collect()
}

/// 手动「现在生成一次晨报」(不动每日做梦状态):取近期新对话 + 新资料 → 产建议。后台跑。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn echo_briefing_run(app: AppHandle) -> Result<(), String> {
    if DREAMING.swap(true, Ordering::SeqCst) {
        return Err("正在做梦/生成中,请稍候".into());
    }
    std::thread::spawn(move || {
        // 与每日做梦同口径:近一周新内容,近期没东西就逐级往前回溯;并入搁置老项目。
        let (transcripts, files, stale, window) = gather_briefing_material(now_ms());
        let result = match generate_briefing(&app, &transcripts, &files, &stale) {
            Ok(n) => Ok((n, format!("生成 {n} 条建议(取材{window})"))),
            Err(e) => Err(e),
        };
        finish_job(&app, result, false);
    });
    Ok(())
}

// ───────────────────────── 调度器 ─────────────────────────

/// 启动后台调度。两条触发:
/// ① **开机补做**(run_on_boot):启动后稍候(让 KB/盘点就绪),若今天还没做过梦就补一次
///    —— 命中「开机就立马执行」(机器在固定钟点是关着的情况下也不漏)。
/// ② **每日定时**:每 10 分钟看一眼钟,到点(本地 hour 命中 + 今天没梦过 + 开关开)就做梦。
/// 一天只梦一次(last_dream_day 把两条触发去重)。桌面与 server 两个 flavor 启动时都调用。
pub fn start_scheduler(app: AppHandle) {
    load_cfg();
    std::thread::spawn(move || {
        // ① 开机补做:延迟 90s 让应用/知识库就绪后再判定,避免和启动抢资源。
        std::thread::sleep(std::time::Duration::from_secs(90));
        let boot_due = {
            let cfg = CFG.read();
            cfg.enabled && cfg.run_on_boot && cfg.last_dream_day != local_day()
        };
        if boot_due {
            let _ = spawn_dream(app.clone(), false);
        }
        // ② 每日定时巡检。
        loop {
            std::thread::sleep(std::time::Duration::from_secs(600));
            let due = {
                let cfg = CFG.read();
                cfg.enabled && local_hour() == cfg.hour && cfg.last_dream_day != local_day()
            };
            if due {
                let _ = spawn_dream(app.clone(), false);
            }
        }
    });
}

fn spawn_dream(app: AppHandle, manual: bool) -> Result<(), String> {
    if DREAMING.swap(true, Ordering::SeqCst) {
        return Err("正在做梦中,请稍候".into());
    }
    std::thread::spawn(move || {
        let result = dream(&app, manual);
        finish_job(&app, result, true);
    });
    Ok(())
}

/// 一次沉淀作业收尾:解锁 → 记日志(成功时) →(可选)推进每日调度状态 → emit done/error。
/// `advance_schedule`:每日做梦走 true(记下「今天梦过了」);单条手动沉淀走 false。
fn finish_job(app: &AppHandle, result: Result<(usize, String), String>, advance_schedule: bool) {
    DREAMING.store(false, Ordering::SeqCst);
    match result {
        Ok((n, summary)) => {
            {
                let mut cfg = CFG.write();
                if advance_schedule {
                    cfg.last_dream_day = local_day();
                    cfg.last_dream_ms = now_ms();
                }
                cfg.log.insert(0, DreamLog { ts: now_ms(), day: local_day(), episodes: n, summary: summary.clone() });
                cfg.log.truncate(30);
            }
            persist_cfg();
            emit_dream(app, "done", Some(summary), Some(n));
        }
        Err(e) => emit_dream(app, "error", Some(e), None),
    }
}

// ───────────────────────── 做梦管线 ─────────────────────────

#[derive(Debug, Deserialize)]
struct DreamDecision {
    /// add | update | skip
    action: String,
    #[serde(default)]
    file: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    reason: String,
}

/// memory/ 相对路径白名单校验:必须落在 memory/ 下、.md 结尾、无穿越无绝对路径。
fn is_safe_memory_relpath(rel: &str) -> bool {
    let r = rel.replace('\\', "/");
    r.starts_with("memory/")
        && r.ends_with(".md")
        && !r.contains("..")
        && !r.contains(':')
        && !r.starts_with('/')
        && r != "memory/index.md"
}

/// 每日做梦:先把「上次做梦以来」的新对话沉淀成记忆,再据更宽的取材生成晨报建议。
/// 晨报取材不止盯昨天:① 近一周的新对话与新资料;② 几个月前曾大量讨论、之后冷掉、
/// 看着没收尾的老项目(每天轮换采样)—— 既盘点新进展,也提醒主人那些半截搁置的事。
fn dream(app: &AppHandle, manual: bool) -> Result<(usize, String), String> {
    const DAY: i64 = 24 * 3600 * 1000;
    emit_dream(app, "phase", Some("收集对话与新资料…".into()), None);
    // ① 沉淀取材:严格按「上次做梦以来」,避免把同一段对话反复蒸馏。
    let since = {
        let cfg = CFG.read();
        if cfg.last_dream_ms > 0 { cfg.last_dream_ms } else { now_ms() - DAY }
    };
    let day_transcripts = crate::conv::transcripts_since(since, 8, 12_000);
    // ② 晨报取材:近一周新内容为主;**近期没东西就逐级往前回溯**(月→季→年→不限),
    //    宁可拿旧素材给建议也别因为「昨天没动静」就空着。stale 老项目始终并入。
    let (recent_transcripts, new_files, stale, window) = gather_briefing_material(now_ms());

    // 即便昨天没有任何新动静,只要整座库非空就仍据「全库画像」生成晨报 —— 让对话框
    // 顶部的建议板块始终长在「主人是谁」之上,而不是动不动就空着。
    if day_transcripts.is_empty()
        && recent_transcripts.is_empty()
        && new_files.is_empty()
        && stale.is_empty()
        && kb_portrait().is_empty()
    {
        return Ok((
            0,
            if manual { "没有新对话/新资料可处理".into() } else { "今夜无梦(无新增)".into() },
        ));
    }
    // 沉淀对话为记忆(只用「上次以来」的新对话,防重复蒸馏)。
    let written = if day_transcripts.is_empty() {
        0usize
    } else {
        distill_and_write(app, &day_transcripts)?.0
    };
    // 据「近一周新内容 + 旧项目回顾」生成工程化晨报建议(失败不影响沉淀结果)。
    // 失败别再静默吞 0:打日志 + 发一条 phase 事件留痕,免得「晨报 0 条」成黑箱
    // (排障时无从分辨「模型真没建议」还是「这步报错了」)。
    let n_sug = match generate_briefing(app, &recent_transcripts, &new_files, &stale) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("[echo] 生成晨报失败(沉淀不受影响): {e}");
            emit_dream(app, "phase", Some(format!("晨报生成失败: {e}")), None);
            0
        }
    };
    let summary = if written == 0 && n_sug == 0 {
        "今日没有值得沉淀的记忆, 也没有可提的工程化建议".to_string()
    } else {
        format!(
            "沉淀 {written} 条记忆 · 晨报 {n_sug} 条建议({window} {} 段对话 / {} 份新资料 + {} 个旧项目回顾)",
            recent_transcripts.len(),
            new_files.len(),
            stale.len()
        )
    };
    Ok((written, summary))
}

/// 扫 KB 的 raw/ 与 output/ 下、mtime 晚于 `since` 的文件(「你近期新加入的资料」),
/// 按 mtime 新→旧取前 `cap` 个返回 (显示名, 相对 KB 根的路径)。零盘点依赖、有界遍历。
fn recent_additions(since_ms: i64, cap: usize) -> Vec<(String, String)> {
    let kb_root = PathBuf::from(crate::kb::kb_root());
    if kb_root.as_os_str().is_empty() {
        return Vec::new();
    }
    let mut found: Vec<(i64, String, String)> = Vec::new(); // (mtime_ms, name, rel)
    let mut scanned = 0usize;
    for sub in ["raw", "output"] {
        let dir = kb_root.join(sub);
        if !dir.exists() {
            continue;
        }
        for entry in walkdir::WalkDir::new(&dir).into_iter().flatten() {
            scanned += 1;
            if scanned > 60_000 {
                break; // 有界:超大库不在做梦时拖死
            }
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            if mtime <= since_ms {
                continue;
            }
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let rel = p
                .strip_prefix(&kb_root)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| name.clone());
            found.push((mtime, name, rel));
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.truncate(cap);
    found.into_iter().map(|(_, n, r)| (n, r)).collect()
}

/// 单条沉淀:取这一条对话的文字稿,交给共用蒸馏核。
fn distill_one(app: &AppHandle, conv_id: &str) -> Result<(usize, String), String> {
    emit_dream(app, "phase", Some("读取这条对话…".into()), None);
    let t = crate::conv::transcript_of(conv_id).ok_or("找不到该对话,或对话为空")?;
    distill_and_write(app, std::slice::from_ref(&t))
}

/// 蒸馏核(做梦与单条沉淀共用):把若干 (标题, 文字稿) 蒸成 memory/ 记忆,
/// AI 出决策(只读 claude 输出 JSON),Rust 校验路径后写盘 + 重建 index。
fn distill_and_write(
    app: &AppHandle,
    transcripts: &[(String, String)],
) -> Result<(usize, String), String> {
    if transcripts.is_empty() {
        return Ok((0, "没有可沉淀的内容".into()));
    }

    let kb_root = PathBuf::from(crate::kb::kb_root());
    if kb_root.as_os_str().is_empty() || !kb_root.exists() {
        return Err("知识库根目录不可用,无法沉淀记忆".into());
    }
    let mem_root = kb_root.join("memory");
    let _ = fs::create_dir_all(mem_root.join("feedback"));
    let _ = fs::create_dir_all(mem_root.join("facts"));

    // 既有记忆索引给模型当「旧记忆」上下文,支撑 Mem0 式 ADD/UPDATE/SKIP 决策
    let existing_index = fs::read_to_string(mem_root.join("index.md")).unwrap_or_default();

    let mut convo_block = String::new();
    for (title, text) in transcripts {
        convo_block.push_str(&format!("\n### 对话「{title}」\n{text}\n"));
    }

    let prompt = dream_directive(&local_day(), &existing_index, &convo_block);
    emit_dream(app, "phase", Some(format!("蒸馏 {} 段对话…", transcripts.len())), None);

    let collected = crate::kb::run_claude_readonly(&kb_root, &prompt, |kind, text| {
        if kind == "delta" {
            emit_dream(app, "delta", Some(text.to_string()), None);
        }
    })?;

    let json = crate::kb::extract_balanced_json(&collected)
        .ok_or("蒸馏输出里找不到 JSON 决策")?;
    let decisions: Vec<DreamDecision> =
        serde_json::from_str(&json).map_err(|e| format!("决策 JSON 解析失败: {e}"))?;

    // 代码执行改动:校验路径 → UPDATE 留 .history → 写盘
    let mut written = 0usize;
    for d in &decisions {
        if d.action == "skip" || d.content.trim().is_empty() {
            continue;
        }
        if !is_safe_memory_relpath(&d.file) {
            emit_dream(app, "phase", Some(format!("跳过不安全路径: {}", d.file)), None);
            continue;
        }
        let dst = kb_root.join(d.file.replace('\\', "/"));
        if let Some(dir) = dst.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if d.action == "update" && dst.exists() {
            // Letta 式修订 + Graphiti 式不失忆:旧版本进 memory/.history/
            let hist = mem_root.join(".history");
            let _ = fs::create_dir_all(&hist);
            let stem = dst.file_stem().and_then(|s| s.to_str()).unwrap_or("mem");
            let _ = fs::copy(&dst, hist.join(format!("{stem}.{}.md", now_ms())));
        }
        if fs::write(&dst, d.content.trim()).is_ok() {
            written += 1;
            emit_dream(app, "phase", Some(format!("✦ {} {} — {}", d.action, d.file, d.reason)), None);
        }
    }

    rebuild_index(&mem_root);
    let summary = if written == 0 {
        "对话里没有值得沉淀的非显然记忆".to_string()
    } else {
        format!("沉淀 {written} 条记忆(取材 {} 段对话)", transcripts.len())
    };
    Ok((written, summary))
}

/// 重建 memory/index.md —— 唯一全文注入件,一行一条(防臃肿铁律:注地图不注全文)。
fn rebuild_index(mem_root: &Path) {
    let mut lines: Vec<String> = vec![
        "# 记忆索引(回声层)".into(),
        String::new(),
        "<!-- 由做梦管线自动维护;一行一条,正文按需 Read。 -->".into(),
        String::new(),
    ];
    for entry in walkdir::WalkDir::new(mem_root).sort_by_file_name().into_iter().flatten() {
        let p = entry.path();
        if !p.is_file()
            || p.extension().map(|x| x != "md").unwrap_or(true)
            || p.file_name().map(|n| n == "index.md").unwrap_or(false)
            || p.components().any(|c| c.as_os_str() == ".history")
        {
            continue;
        }
        let rel = p.strip_prefix(mem_root).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default();
        let body = fs::read_to_string(p).unwrap_or_default();
        // 取 frontmatter 之后第一行非空文本当摘要
        let mut in_fm = false;
        let mut fm_done = false;
        let mut hook = String::new();
        for (i, line) in body.lines().enumerate() {
            let t = line.trim();
            if i == 0 && t == "---" {
                in_fm = true;
                continue;
            }
            if in_fm && !fm_done {
                if t == "---" {
                    fm_done = true;
                }
                continue;
            }
            if !t.is_empty() && !t.starts_with('#') {
                hook = t.chars().take(80).collect();
                break;
            }
        }
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("mem");
        lines.push(format!("- [{stem}]({rel}) — {hook}"));
    }
    let _ = fs::write(mem_root.join("index.md"), lines.join("\n"));
}

/// 蒸馏提示词:feedback-episode schema 来自《记忆系统升级方案》§3,原样固化。
fn dream_directive(day: &str, existing_index: &str, convo_block: &str) -> String {
    format!(
        r#"你是 Polaris 的「记忆蒸馏者」。下面是最近的对话文字稿,请把其中**值得长期留存的非显然信息**蒸馏成记忆文件。

## 蒸馏什么(按价值排序)
1. **反馈线(最值钱)**:用户怎么纠正/否决/认可了助手的做法 → feedback-episode;
2. 用户的稳定偏好、工作习惯、目标与约束 → 稳定事实;
3. 项目级的非显然决策与原因 → 稳定事实。
**不记**:寒暄、一次性任务细节、能从代码/文件重新推导的内容、密钥等敏感信息。

## 旧记忆索引(判断该 add 新条目还是 update 旧条目;同主题别开新文件)
{existing_index}

## 输出格式(严格)
只输出一个 JSON 数组,不要其它文字。每个元素:
{{"action":"add|update|skip","file":"memory/feedback/{day}-<kebab-slug>.md 或 memory/facts/<kebab-slug>.md","content":"完整 markdown(含 frontmatter)","reason":"一句话"}}
- 最多 6 条;没有值得沉淀的就输出 []。
- feedback-episode 的 content 模板:
---
name: <slug>
type: feedback-episode
date: {day}
supersedes: <被本条修订的旧文件名或 null>
tags: [<标签>]
---
**意图** <用户想要什么>
**我的初版** <助手最初怎么做>
**用户的修改** <用户怎么纠正>
**沉淀的规则** <一句可执行的规则,可用 [[双链]] 连旧记忆>
- 稳定事实的 content 模板:
---
name: <slug>
type: fact
date: {day}
tags: [<标签>]
---
<事实本身;**Why** 一行;**How to apply** 一行>

## 对话文字稿
{convo_block}
"#
    )
}

// ───────────────────────── 晨报生成 ─────────────────────────

/// 知识库画像(全库观察)—— 晨报的「用户画像」事实底座。
///
/// 这是「内容产生原理」升级的核心:旧版只盯着**昨天新增的零碎**(新对话/新文件),
/// 像个只会复述近况的人;真正「懂你」的助理得先**通观你整座库是关于什么的** ——
/// 你沉淀了哪些权威知识、攒了哪些资料、库里以什么语言/类型为主、被智能归类出哪些主题,
/// 据此在脑子里勾勒「主人是谁、在做什么、关注与擅长什么」,再据这画像给建议。
///
/// 取材全部只读且有界(各维度取 Top-N):
/// - 四车道家底(权威 wiki / 原始资料 / 成品 / 记忆)—— 来自内存索引,O(n) 极快;
/// - 盘点文件库的语言分布 / 类型分布 / AI 命名的主题簇 —— 来自文件库 SQL 总览。
/// 库整体为空(全新用户)时返回空串,不硬凑画像。
fn kb_portrait() -> String {
    let ov = crate::kb::kb_overview();
    let files = crate::fable::files::overview(None).ok();
    let total_files = files.as_ref().map(|f| f.total_files).unwrap_or(0);
    // 整座库空空如也(四车道无文档、盘点无文件)→ 不画像,留给新用户向导去引导。
    if ov.total_docs == 0 && total_files == 0 {
        return String::new();
    }

    let mut s = String::new();
    s.push_str(
        "## 知识库画像(全库观察 —— 先据此勾勒「主人是谁 / 在做什么 / 关注与擅长什么」)\n",
    );
    s.push_str(&format!(
        "- 四层家底:权威 wiki {} 篇 · 原始资料 {} 篇 · 成品 {} 篇 · 记忆 {} 条(共索引 {} 篇文档)\n",
        ov.wiki, ov.raw_md, ov.output, ov.memory, ov.total_docs
    ));
    if let Some(f) = files.as_ref() {
        s.push_str(&format!("- 盘点文件总量:{} 个\n", f.total_files));
        let langs: Vec<String> =
            f.by_lang.iter().take(8).map(|l| format!("{}({})", l.lang, l.count)).collect();
        if !langs.is_empty() {
            s.push_str(&format!("- 语言分布(看 ta 在用什么):{}\n", langs.join(" · ")));
        }
        let kinds: Vec<String> =
            f.by_kind.iter().take(6).map(|k| format!("{}({})", k.kind, k.count)).collect();
        if !kinds.is_empty() {
            s.push_str(&format!("- 类型分布:{}\n", kinds.join(" · ")));
        }
        // 主题簇(AI 给整座库命的名)——「关注/擅长什么」最直接的证据。优先顶层主题。
        let mut topics: Vec<String> = f
            .clusters
            .iter()
            .filter(|c| c.parent == 0 && !c.label.trim().is_empty())
            .take(12)
            .map(|c| format!("{}({})", c.label.trim(), c.size))
            .collect();
        if topics.is_empty() {
            topics = f
                .clusters
                .iter()
                .filter(|c| !c.label.trim().is_empty())
                .take(12)
                .map(|c| format!("{}({})", c.label.trim(), c.size))
                .collect();
        }
        if !topics.is_empty() {
            s.push_str(&format!("- 主要主题(整库智能归类):{}\n", topics.join(" · ")));
        }
    }
    s.push('\n');
    s
}

/// 晨报取材:**优先盯最近两天(尤其昨天)的新对话 + 新资料** —— 那才是主人当前正在
/// 做的事,今日建议就该围着它转;**这两天没动静才逐级往前回溯**(一周 → 一个月 →
/// 三个月 → 一年 → 不限历史),命中第一个有素材的窗口即止 —— 宁可拿旧素材给建议,
/// 也别因为「这两天没动静」就给主人一片空白。
/// `stale`(几个月前疑似搁置的老项目)始终单独并入,与窗口无关。
/// 返回 (近期新对话, 近期新资料, 搁置老项目, 取材窗口中文描述)。
fn gather_briefing_material(
    now: i64,
) -> (Vec<(String, String)>, Vec<(String, String)>, Vec<(String, String)>, String) {
    const DAY: i64 = 24 * 3600 * 1000;
    let stale = crate::conv::stale_unfinished_transcripts(now, 3, 4_000);
    // (回溯天数, 中文描述);0 天 = 不限,取全部历史。先盯最近两天(尤其昨天),空了才放宽。
    for (days, label) in [
        (2i64, "最近两天"),
        (7, "近一周"),
        (30, "近一个月"),
        (90, "近三个月"),
        (365, "近一年"),
        (0, "全部历史"),
    ] {
        let since = if days == 0 { 0 } else { now - days * DAY };
        let transcripts = crate::conv::transcripts_since(since, 8, 6_000);
        let files = recent_additions(since, 40);
        if !transcripts.is_empty() || !files.is_empty() {
            return (transcripts, files, stale, label.to_string());
        }
    }
    // 全程都没有新对话/新资料:只剩 stale(可能也空)。
    (Vec::new(), Vec::new(), stale, "无新内容".to_string())
}

/// 据「近一周新内容 + 几个月前疑似搁置的老项目」生成晨报建议(≤4 条), 写 memory/briefing/<today>.json。
/// AI 出建议(只读 claude 输出 JSON 数组), Rust 补 id/落盘。无素材或无值得提的 → 写空、返回 0。
fn generate_briefing(
    app: &AppHandle,
    transcripts: &[(String, String)],
    new_files: &[(String, String)],
    stale: &[(String, String)],
) -> Result<usize, String> {
    // 全库画像作为「用户画像」的事实底座:只要库里有东西,哪怕昨天没新动静也能给建议。
    let portrait = kb_portrait();
    if transcripts.is_empty() && new_files.is_empty() && stale.is_empty() && portrait.is_empty() {
        return Ok(0);
    }
    let kb_root = PathBuf::from(crate::kb::kb_root());
    if kb_root.as_os_str().is_empty() || !kb_root.exists() {
        return Ok(0);
    }
    emit_dream(app, "phase", Some("观察全库画像 → 据用户画像生成工程化建议…".into()), None);

    // 素材顺序刻意:**先全库画像(立人设)**,再近期新内容/老项目(给落点)。
    let mut material = String::new();
    if !portrait.is_empty() {
        material.push_str(&portrait);
    }
    if !new_files.is_empty() {
        material.push_str("## 近期新加入的资料\n");
        for (name, rel) in new_files {
            material.push_str(&format!("- {name}  (`{rel}`)\n"));
        }
        material.push('\n');
    }
    if !transcripts.is_empty() {
        material.push_str("## 近期新对话(节选,按时间从新到旧排列 —— 排在最前的就是最近一两天、尤其昨天的活动)\n");
        for (title, text) in transcripts {
            let snip: String = text.chars().take(1500).collect();
            material.push_str(&format!("\n### 对话「{title}」\n{snip}\n"));
        }
        material.push('\n');
    }
    if !stale.is_empty() {
        material.push_str("## 几个月前曾大量讨论、似乎没收尾的老项目(供回顾,是否重启/收尾由主人判断)\n");
        for (title, text) in stale {
            let snip: String = text.chars().take(1200).collect();
            material.push_str(&format!("\n### 项目「{title}」\n{snip}\n"));
        }
    }

    let day = local_day();
    let prompt = suggest_directive(&day, &material);
    let collected = crate::kb::run_claude_readonly(&kb_root, &prompt, |kind, text| {
        if kind == "delta" {
            emit_dream(app, "delta", Some(text.to_string()), None);
        }
    })?;
    let json =
        crate::kb::extract_balanced_json(&collected).ok_or("晨报输出里找不到 JSON 数组")?;
    let raw: Vec<SuggestionIn> =
        serde_json::from_str(&json).map_err(|e| format!("建议 JSON 解析失败: {e}"))?;

    let items: Vec<Suggestion> = raw
        .into_iter()
        .filter(|s| !s.title.trim().is_empty())
        .take(4)
        .enumerate()
        .map(|(i, s)| Suggestion {
            id: format!("{day}-{i}"),
            title: s.title.trim().to_string(),
            // 只接受白名单类别,异常值归一到 progress,免得前端拿到脏值。
            kind: match s.kind.trim() {
                "wrapup" | "workflow" | "organize" | "progress" => s.kind.trim().to_string(),
                _ => "progress".to_string(),
            },
            // 依据标签控长,过长截断(按字符,兼容 CJK)。
            source: s.source.trim().chars().take(20).collect::<String>(),
            why: s.why.trim().to_string(),
            how: s.how.trim().to_string(),
            action: s.action.trim().to_string(),
            dismissed: false,
        })
        .collect();

    write_briefing(&day, &items);
    emit_dream(app, "phase", Some(format!("晨报:{} 条建议", items.len())), None);
    Ok(items.len())
}

/// 晨报提示词。内容产生原理(本次升级的核心):
/// **先观察整座知识库 → 在心里勾出「用户画像」→ 再据画像给建议**,而不是只复述昨天的零碎。
/// 仍守「反泛泛」铁律:每条都要指到具体素材、能立刻执行,否则宁可输出空数组。
fn suggest_directive(day: &str, material: &str) -> String {
    format!(
        r#"你是 Polaris 的私人工程顾问,长期观察主人这座知识库。今天是 {day}。
下面给你三类材料:
**① 知识库画像**(整座库的全局观察:四层家底、语言/类型分布、被智能归类出的主题);
**② 近期新加入的内容**(最近两天为主的新对话 + 新资料,**对话按从新到旧排列**);
**③ 几个月前曾大量讨论、之后冷掉、看着没收尾的老项目**(标注「疑未收尾」)。

## 怎么想(内容产生原理,务必照此推理)
1. **先读①画像,在心里给主人画像**:这个人是谁?在做什么?主要关注/擅长哪些方向?
   ——从主题簇、语言分布、家底结构里读出来(例:Rust+大量「检索/盘点」主题 ⇒ 在做本地知识引擎;
   大量中文资料+「自媒体/视频」主题 ⇒ 在做内容创作)。**画像只在心里建,不要输出。**
2. **再落到②③的具体素材,且以②里最近的活动为主锚点**:②的对话从新到旧排,**排在最前面的就是最近一两天、尤其昨天的事——那是主人此刻正在做的,今日建议必须优先围绕它**;越靠后/越旧的素材只作背景,别喧宾夺主。据画像判断「顺着 ta 这一两天在做的,接下来最该推进 / 最该收尾的是什么」。
3. 产出不超过 4 条建议,**每条都既贴合画像、又指到具体素材**,且**多数应落在最近一两天的活动上** —— 像一个真懂 ta 的助理在盘点「你昨天在搞 X,那今天不妨…」。

## 硬要求
- **每条都要指到具体素材**:某段对话、某份资料、某个主题簇或某个老项目;指不到的通用建议一律不要。
- 既可就②新内容提进展,也可挑某个**搁置的老项目**建议「收尾 / 重启」,还可据①画像里**某个大主题**建议把它系统化(沉淀成 wiki / 固化成工作流);但都要具体到那个主题/项目本身,别泛泛说「整理旧项目」。
- **拒绝泛泛而谈**:不要「多记录」「保持学习」「关注趋势」这类空话;要能立刻执行的下一步。
- 每条给出:类别(kind)、依据来源标签(source)、做什么(title)、依据哪条素材(why)、具体怎么做/需要什么(how)、一句可直接发给我执行的指令(action)。
- **kind** 从这四选一:`progress`(就近期新内容推进展)、`wrapup`(给搁置老项目收尾/重启)、`workflow`(把反复做的事固化成可复用流程)、`organize`(整理/归类某批资料或某个大主题)。
- **source** 是「懂你」的落点:用 ≤16 字点名**真实出现过的那条素材**——某主题簇名、某段对话标题、某份文件名、或某个老项目名(如「红楼视频」「EDGAR 语料」「飞书采集」)。务必真实,不要编。
- **why** 里点明这条建议是怎么从画像 + 素材推出来的(例:「库里 Rust 与『检索』主题最重,而 X 对话刚提到 Y → 建议…」)。
- 没有值得提的就输出 `[]`。

## 输出格式(严格:只输出一个 JSON 数组,不要其它文字)
[
  {{"kind":"progress|wrapup|workflow|organize","source":"≤16字·点名真实素材","title":"一句话建议","why":"由画像+哪条素材推出(可引主题名/文件名/原话)","how":"具体步骤与所需材料","action":"可直接作为对话发起的一句话指令"}}
]

## 材料
{material}
"#
    )
}
