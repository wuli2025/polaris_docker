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
    /// 每日做梦总开关(默认关,用户在「寓言计划 API」页打开)
    #[serde(default)]
    pub enabled: bool,
    /// 做梦钟点(本地时区 0–23,默认 3 = 凌晨三点)
    #[serde(default = "default_hour")]
    pub hour: u8,
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
    3
}

impl Default for EchoCfg {
    fn default() -> Self {
        Self { enabled: false, hour: 3, last_dream_day: String::new(), last_dream_ms: 0, log: vec![] }
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
    pub last_dream_day: String,
    pub dreaming: bool,
    /// memory/ 车道里的记忆条数
    pub memory_count: usize,
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

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn echo_status() -> EchoStatus {
    let cfg = CFG.read();
    EchoStatus {
        enabled: cfg.enabled,
        hour: cfg.hour,
        last_dream_day: cfg.last_dream_day.clone(),
        dreaming: DREAMING.load(Ordering::Relaxed),
        memory_count: count_memories(),
        log: cfg.log.clone(),
    }
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn echo_set(enabled: Option<bool>, hour: Option<u8>) -> EchoStatus {
    {
        let mut cfg = CFG.write();
        if let Some(e) = enabled {
            cfg.enabled = e;
        }
        if let Some(h) = hour {
            cfg.hour = h.min(23);
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

// ───────────────────────── 调度器 ─────────────────────────

/// 启动后台调度:每 10 分钟看一眼钟;到点(本地 hour 命中 + 今天没梦过 + 开关开)就做梦。
/// 桌面与 server 两个 flavor 都在启动时调用。
pub fn start_scheduler(app: AppHandle) {
    load_cfg();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(600));
        let due = {
            let cfg = CFG.read();
            cfg.enabled && local_hour() == cfg.hour && cfg.last_dream_day != local_day()
        };
        if due {
            let _ = spawn_dream(app.clone(), false);
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

/// 每日做梦:取 last_dream_ms 之后有更新的对话(全量),交给共用蒸馏核。
fn dream(app: &AppHandle, manual: bool) -> Result<(usize, String), String> {
    emit_dream(app, "phase", Some("收集今天的对话…".into()), None);
    let since = {
        let cfg = CFG.read();
        if cfg.last_dream_ms > 0 { cfg.last_dream_ms } else { now_ms() - 24 * 3600 * 1000 }
    };
    let transcripts = crate::conv::transcripts_since(since, 8, 12_000);
    if transcripts.is_empty() {
        return Ok((0, if manual { "没有新对话可沉淀".into() } else { "今夜无梦(无新对话)".into() }));
    }
    distill_and_write(app, &transcripts)
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
