//! 板块 ① 对话核心 - 项目 + 对话 + 消息持久化
//!
//! MVP: 单文件 JSON (`~/Polaris/data/state.json`), 全局 RwLock 保护
//! 后续接 ② Wiki 的 storage::* (SQLite), API 不动

use anyhow::Result;
use directories::UserDirs;
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(feature = "desktop")]
use tauri::AppHandle;
#[cfg(not(feature = "desktop"))]
use crate::host::AppHandle;

// ───────────────────────── Types ─────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    #[serde(default)]
    pub archived: bool,
    /// 板块⑫ 人格模块：该项目套用的预设人格 id（自定义为 None）。仅用于前端显示图标/便于更新。
    #[serde(default)]
    pub persona_id: Option<String>,
    /// 该人格绑定的专属知识库范围（KB 根下相对子目录，None/空=全局 PolarisKB）。
    #[serde(default)]
    pub kb_scope: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// 回声层(寓言计划 v5 §6):归档 = 移出主列表的纯状态位,可逆;蒸馏取材时跳过。
    /// 老 state.json 没有此字段 → serde 默认 false,向后兼容。
    #[serde(default)]
    pub archived: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub conversation_id: String,
    pub role: String, // user | assistant | tool
    pub content: String,
    pub created_at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    projects: Vec<Project>,
    #[serde(default)]
    conversations: Vec<Conversation>,
    #[serde(default)]
    messages: Vec<Message>,
}

/// 默认赠送的「毛主席」项目名(前端据此识别该项目, 显示彩蛋空状态)
pub const MAO_PROJECT_NAME: &str = "毛主席";
const MAO_PERSONA_TEMPLATE: &str = include_str!("templates/mao_persona_claude.md");

// ───────────────────────── State ─────────────────────────

static STATE: Lazy<RwLock<State>> = Lazy::new(|| RwLock::new(State::default()));
static STATE_PATH: Lazy<RwLock<PathBuf>> = Lazy::new(|| RwLock::new(PathBuf::new()));

// ───────────────────────── Init / persist ────────────────

pub fn init(_app: &AppHandle) -> Result<()> {
    let user = UserDirs::new().ok_or_else(|| anyhow::anyhow!("no user dir"))?;
    let dir = user.home_dir().join("Polaris").join("data");
    fs::create_dir_all(&dir)?;
    let path = dir.join("state.json");
    *STATE_PATH.write() = path.clone();

    let mut state: State = if path.exists() {
        let txt = fs::read_to_string(&path).unwrap_or_default();
        match serde_json::from_str(&txt) {
            Ok(s) => s,
            Err(e) => {
                // 解析失败别静默 unwrap_or_default() 清空全部历史: 先把损坏文件留底
                // (state.json.corrupt.bak), 给用户/支持留挽救机会, 再回落空状态。
                if !txt.trim().is_empty() {
                    let bak = path.with_extension("json.corrupt.bak");
                    let _ = fs::write(&bak, &txt);
                    eprintln!("[conv] state.json 解析失败({e}), 已备份到 {bak:?} 并回落空状态");
                }
                State::default()
            }
        }
    } else {
        State::default()
    };

    // 首次启动: 自建一个"默认项目"
    if state.projects.is_empty() {
        let pid = new_id("p");
        let now = now_ms();
        state.projects.push(Project {
            id: pid.clone(),
            name: "默认项目".into(),
            created_at: now,
            archived: false,
            persona_id: None,
            kb_scope: None,
        });
    }

    // 注: 此前这里还会首启赠送「毛主席」项目 —— 已随「名人资料包」改版移除,
    // 改为安装毛主席资料包时由 `ensure_mao_project` 创建。

    *STATE.write() = state;
    persist();
    Ok(())
}

/// 「毛主席」资料包安装时调用: 找到/新建「毛主席」项目(插到最前), 写入人格 CLAUDE.md
/// 并绑定专属资料库 scope(`raw/毛主席`)。幂等; 用户删了项目后重装资料包会重建。
pub fn ensure_mao_project() {
    {
        let mut state = STATE.write();
        let mao_pid = match state.projects.iter().position(|p| p.name == MAO_PROJECT_NAME) {
            Some(i) => state.projects[i].id.clone(),
            None => {
                let pid = new_id("p");
                state.projects.insert(
                    0,
                    Project {
                        id: pid.clone(),
                        name: MAO_PROJECT_NAME.into(),
                        created_at: now_ms(),
                        archived: false,
                        persona_id: Some("mao".into()),
                        kb_scope: Some("raw/毛主席".into()),
                    },
                );
                pid
            }
        };
        write_mao_persona(&mao_pid);
        if let Some(p) = state.projects.iter_mut().find(|p| p.id == mao_pid) {
            if p.persona_id.is_none() {
                p.persona_id = Some("mao".into());
            }
            if p.kb_scope.is_none() {
                p.kb_scope = Some("raw/毛主席".into());
            }
        }
    }
    persist();
}

/// 把毛主席人格 CLAUDE.md 写到该项目目录 `~/Polaris/projects/<id>/CLAUDE.md`。
/// 已存在则不覆盖(尊重用户改动)。路径须与 `claude_md` 模块一致。
fn write_mao_persona(project_id: &str) {
    let Some(user) = UserDirs::new() else { return };
    let dir = user
        .home_dir()
        .join("Polaris")
        .join("projects")
        .join(project_id);
    let path = dir.join("CLAUDE.md");
    if path.exists() {
        return;
    }
    if fs::create_dir_all(&dir).is_ok() {
        let _ = fs::write(&path, MAO_PERSONA_TEMPLATE);
    }
}

/// 原子落盘: 临时文件 + rename。每条消息都会 persist(), 裸 fs::write 在断电/崩溃时
/// 会把 state.json 截成半截 JSON, 下次启动解析失败 → 全部项目/对话静默蒸发。rename
/// 在同卷原子, 目标要么旧要么新, 绝不残缺。范式同 provider::atomic_write。
fn atomic_write_state(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".polaris.tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

fn persist() {
    let st = STATE.read();
    let path = STATE_PATH.read().clone();
    if path.as_os_str().is_empty() {
        return;
    }
    if let Ok(txt) = serde_json::to_string_pretty(&*st) {
        let _ = atomic_write_state(&path, &txt);
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn new_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CTR: AtomicU64 = AtomicU64::new(0);
    let ts = now_ms() as u64;
    let c = CTR.fetch_add(1, Ordering::Relaxed);
    format!("{}-{:x}-{:x}", prefix, ts, c)
}

// ───────────────────────── Internal API (chat::send 用) ──

/// 反查 conversation 对应的 project_id (chat::send 注入 CLAUDE.md 时用)
pub fn project_id_of_conversation(conversation_id: &str) -> Option<String> {
    STATE
        .read()
        .conversations
        .iter()
        .find(|c| c.id == conversation_id)
        .map(|c| c.project_id.clone())
}

/// 取某对话的全部消息(按时间升序)。chat::send 注入「对话历史」时用,
/// 避免外部直接锁 STATE。等价于 `conv_get_messages` 命令的内部版。
pub fn get_messages(conversation_id: &str) -> Vec<Message> {
    let mut list: Vec<Message> = STATE
        .read()
        .messages
        .iter()
        .filter(|m| m.conversation_id == conversation_id)
        .cloned()
        .collect();
    list.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    list
}

/// 列出某项目下的全部对话(按 updated_at 倒序, 最近的在前)。
/// chat::send 构建「跨对话产物地图」时用。
pub fn conversations_of_project(project_id: &str) -> Vec<Conversation> {
    let mut list: Vec<Conversation> = STATE
        .read()
        .conversations
        .iter()
        .filter(|c| c.project_id == project_id)
        .cloned()
        .collect();
    list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    list
}

/// 列出所有未归档的项目 (claude_md 模块用,避免直接锁 STATE)
pub fn list_active_projects() -> Vec<Project> {
    STATE
        .read()
        .projects
        .iter()
        .filter(|p| !p.archived)
        .cloned()
        .collect()
}

/// 该项目绑定的知识库 scope（板块⑫；空/None=全局）。claude_md::render_for_project 注入时用。
pub fn project_kb_scope(project_id: &str) -> Option<String> {
    STATE
        .read()
        .projects
        .iter()
        .find(|p| p.id == project_id)
        .and_then(|p| p.kb_scope.clone())
        .filter(|s| !s.trim().is_empty())
}

/// 设置项目的人格与知识库 scope（persona::persona_apply 用）。
pub fn set_project_persona(
    project_id: &str,
    persona_id: Option<String>,
    kb_scope: Option<String>,
) {
    {
        let mut st = STATE.write();
        if let Some(p) = st.projects.iter_mut().find(|p| p.id == project_id) {
            p.persona_id = persona_id;
            p.kb_scope = kb_scope;
        }
    }
    persist();
}

pub fn append_message(conversation_id: &str, role: &str, content: &str) -> Result<String> {
    let id = new_id("m");
    let now = now_ms();
    {
        let mut st = STATE.write();
        // 找到 conversation, 顺便更新 updated_at + 推断 title (首条 user 消息)
        let mut should_set_title: Option<String> = None;
        for c in st.conversations.iter_mut() {
            if c.id == conversation_id {
                c.updated_at = now;
                if c.title == "新对话" && role == "user" {
                    let snippet: String = content.chars().take(24).collect();
                    should_set_title = Some(snippet);
                }
                break;
            }
        }
        if let Some(t) = should_set_title {
            for c in st.conversations.iter_mut() {
                if c.id == conversation_id {
                    c.title = t;
                    break;
                }
            }
        }
        st.messages.push(Message {
            id: id.clone(),
            conversation_id: conversation_id.to_string(),
            role: role.to_string(),
            content: content.to_string(),
            created_at: now,
        });
    }
    persist();
    Ok(id)
}

// ───────────────────────── Tauri commands ────────────────

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn conv_list_projects() -> Vec<Project> {
    STATE
        .read()
        .projects
        .iter()
        .filter(|p| !p.archived)
        .cloned()
        .collect()
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn conv_create_project(name: String) -> Result<Project, String> {
    let p = Project {
        id: new_id("p"),
        name: if name.trim().is_empty() {
            "新项目".into()
        } else {
            name.trim().to_string()
        },
        created_at: now_ms(),
        archived: false,
        persona_id: None,
        kb_scope: None,
    };
    STATE.write().projects.push(p.clone());
    persist();
    Ok(p)
}

/// 手动设置项目的知识库 scope（人格工坊里的下拉）。persona_id 维持不变。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn conv_set_project_kb_scope(project_id: String, kb_scope: Option<String>) -> Result<(), String> {
    let persona = STATE
        .read()
        .projects
        .iter()
        .find(|p| p.id == project_id)
        .and_then(|p| p.persona_id.clone());
    set_project_persona(&project_id, persona, kb_scope.filter(|s| !s.trim().is_empty()));
    Ok(())
}

/// project_id 直接拼进文件系统路径, 必须挡掉 `..` / 路径分隔符 / 盘符,
/// 否则前端传 `..\..\dir` 可让 create_dir_all / 写 CLAUDE.md 越出 projects 根。
/// 真实 id 由 `new_id("p")` 生成(纯字母数字), 故该闸不会误伤合法项目。
pub fn is_safe_project_id(id: &str) -> bool {
    !id.is_empty()
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains("..")
        && !id.contains(':')
}

/// 该项目在磁盘上的工作目录 `~/Polaris/projects/<id>/`(须与 write_mao_persona / claude_md 一致)。
fn project_dir(project_id: &str) -> Option<PathBuf> {
    if !is_safe_project_id(project_id) {
        return None;
    }
    let user = UserDirs::new()?;
    Some(
        user.home_dir()
            .join("Polaris")
            .join("projects")
            .join(project_id),
    )
}

/// 在系统文件管理器中打开该项目的工作目录(不存在则先建好, 否则 explorer 会报路径不存在)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn conv_open_project_dir(project_id: String) -> Result<(), String> {
    let dir = project_dir(&project_id).ok_or_else(|| "no user dir".to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // 路径可能含空格, 用 raw_arg 引号包裹; 正斜杠转反斜杠
        let win_path = path.replace('/', "\\");
        std::process::Command::new("explorer")
            .raw_arg(format!("\"{}\"", win_path))
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn conv_archive_project(project_id: String) -> Result<(), String> {
    let mut st = STATE.write();
    for p in st.projects.iter_mut() {
        if p.id == project_id {
            p.archived = true;
        }
    }
    drop(st);
    persist();
    Ok(())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn conv_list_conversations(project_id: String) -> Vec<Conversation> {
    let mut list: Vec<Conversation> = STATE
        .read()
        .conversations
        .iter()
        // 归档的对话移出列表(回声层动作一:纯状态位,文件/消息都保留,可逆)
        .filter(|c| c.project_id == project_id && !c.archived)
        .cloned()
        .collect();
    list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    list
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn conv_create_conversation(project_id: String) -> Result<Conversation, String> {
    let st = STATE.read();
    if !st.projects.iter().any(|p| p.id == project_id) {
        return Err(format!("project {} 不存在", project_id));
    }
    drop(st);
    let now = now_ms();
    let c = Conversation {
        id: new_id("c"),
        project_id,
        title: "新对话".into(),
        created_at: now,
        updated_at: now,
        archived: false,
    };
    STATE.write().conversations.push(c.clone());
    persist();
    Ok(c)
}

/// 归档/取消归档一个对话(回声层动作一:纯状态位,可逆)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn conv_archive_conversation(id: String, archived: bool) -> Result<(), String> {
    {
        let mut state = STATE.write();
        let c = state
            .conversations
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| format!("没有对话 '{id}'"))?;
        c.archived = archived;
    }
    persist();
    Ok(())
}

/// 回声层(echo.rs)蒸馏取材:since_ms 之后有更新、未归档的对话 → (标题, 文字稿)。
/// 文字稿只含 user/assistant 轮次;超长截尾保留最新内容。
pub(crate) fn transcripts_since(
    since_ms: i64,
    max_convs: usize,
    per_conv_chars: usize,
) -> Vec<(String, String)> {
    let state = STATE.read();
    let mut convs: Vec<&Conversation> = state
        .conversations
        .iter()
        .filter(|c| c.updated_at > since_ms && !c.archived)
        .collect();
    convs.sort_by_key(|c| std::cmp::Reverse(c.updated_at));
    convs.truncate(max_convs);
    convs
        .iter()
        .map(|c| {
            let mut buf = String::new();
            for msg in state.messages.iter().filter(|m| m.conversation_id == c.id) {
                let who = match msg.role.as_str() {
                    "user" => "用户",
                    "assistant" => "助手",
                    _ => continue,
                };
                buf.push_str(who);
                buf.push_str(": ");
                buf.push_str(msg.content.trim());
                buf.push('\n');
            }
            let s = if buf.chars().count() > per_conv_chars {
                let tail: String = {
                    let chars: Vec<char> = buf.chars().collect();
                    chars[chars.len() - per_conv_chars..].iter().collect()
                };
                format!("…(前文截断)\n{tail}")
            } else {
                buf
            };
            (c.title.clone(), s)
        })
        .collect()
}

/// 回声层「沉淀为记忆」单条用:取某一对话的文字稿 → (标题, 文字稿)。
/// 与 transcripts_since 同口径(只含 user/assistant、超长截尾留最新),但**不看 archived**
/// ——用户在侧栏手动点的就是这一条,归档与否都该能沉淀。空对话返回 None。
pub(crate) fn transcript_of(id: &str) -> Option<(String, String)> {
    const PER_CONV_CHARS: usize = 12_000;
    let state = STATE.read();
    let c = state.conversations.iter().find(|c| c.id == id)?;
    let mut buf = String::new();
    for msg in state.messages.iter().filter(|m| m.conversation_id == c.id) {
        let who = match msg.role.as_str() {
            "user" => "用户",
            "assistant" => "助手",
            _ => continue,
        };
        buf.push_str(who);
        buf.push_str(": ");
        buf.push_str(msg.content.trim());
        buf.push('\n');
    }
    if buf.trim().is_empty() {
        return None;
    }
    let s = if buf.chars().count() > PER_CONV_CHARS {
        let chars: Vec<char> = buf.chars().collect();
        let tail: String = chars[chars.len() - PER_CONV_CHARS..].iter().collect();
        format!("…(前文截断)\n{tail}")
    } else {
        buf
    };
    Some((c.title.clone(), s))
}

/// 把一条对话渲染成文字稿(只含 user/assistant),超 `cap` 字符留最新尾部。
/// transcripts_since / transcript_of 的共用截取口径,抽出来给老项目采样复用。
fn render_transcript(state: &State, conv_id: &str, cap: usize) -> String {
    let mut buf = String::new();
    for msg in state.messages.iter().filter(|m| m.conversation_id == conv_id) {
        let who = match msg.role.as_str() {
            "user" => "用户",
            "assistant" => "助手",
            _ => continue,
        };
        buf.push_str(who);
        buf.push_str(": ");
        buf.push_str(msg.content.trim());
        buf.push('\n');
    }
    if buf.chars().count() > cap {
        let chars: Vec<char> = buf.chars().collect();
        let tail: String = chars[chars.len() - cap..].iter().collect();
        format!("…(前文截断)\n{tail}")
    } else {
        buf
    }
}

/// 回声层晨报取材②:翻出「几个月前曾大量讨论、之后冷掉、看着没收尾」的老对话,
/// 每天轮换采样几条 —— 让做梦不只盯着昨天,也提醒主人那些半截搁置的项目。
///
/// 判定「未完成的样子」(全部满足):
///  - 未归档;
///  - 已冷却:最后活跃在 14 天前,不跟当下热对话(由 transcripts_since 处理)抢;
///  - 不太久远:在 ~8 个月内,够得上「几个月前」而非远古;
///  - 有分量:user/assistant 消息数 ≥ 6,即当时「大量出现」过;
///  - 收尾信号弱:最后一条是用户发言(助手没接上),或尾部出现 待办/继续/下一步/未完成/回头/稍后/下次/TODO 等续作词。
///
/// 命中后按消息多寡排序,再以「今天的日序」为偏移轮换取 `max_convs` 条(每天换一批,故曰随机)。
pub(crate) fn stale_unfinished_transcripts(
    now_ms: i64,
    max_convs: usize,
    per_conv_chars: usize,
) -> Vec<(String, String)> {
    const DAY: i64 = 24 * 3600 * 1000;
    const CUES: [&str; 8] =
        ["待办", "继续", "下一步", "未完成", "回头", "稍后", "下次", "todo"];
    let cold_after = now_ms - 14 * DAY; // 14 天没动过才算冷
    let lookback_from = now_ms - 240 * DAY; // ~8 个月内才算「几个月前」

    let state = STATE.read();
    // (conv, user/assistant 消息数)
    let mut cand: Vec<(&Conversation, usize)> = Vec::new();
    for c in state.conversations.iter() {
        if c.archived || c.updated_at >= cold_after || c.updated_at < lookback_from {
            continue;
        }
        let msgs: Vec<&Message> = state
            .messages
            .iter()
            .filter(|m| m.conversation_id == c.id && (m.role == "user" || m.role == "assistant"))
            .collect();
        if msgs.len() < 6 {
            continue;
        }
        let last_is_user = msgs.last().map(|m| m.role == "user").unwrap_or(false);
        let has_cue = msgs.iter().rev().take(8).any(|m| {
            let lc = m.content.to_lowercase();
            CUES.iter().any(|w| lc.contains(w))
        });
        if last_is_user || has_cue {
            cand.push((c, msgs.len()));
        }
    }
    if cand.is_empty() {
        return Vec::new();
    }
    // 讨论得多的优先,同等再看谁更近
    cand.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.updated_at.cmp(&a.0.updated_at)));

    // 每天轮换一个起点,避免天天提同几个搁置项目
    let n = cand.len();
    let offset = if n > max_convs { (now_ms / DAY) as usize % n } else { 0 };
    (0..max_convs.min(n))
        .map(|i| {
            let (c, _) = cand[(offset + i) % n];
            let body = render_transcript(&state, &c.id, per_conv_chars);
            (format!("{}(几个月前 · 疑未收尾)", c.title), body)
        })
        .collect()
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn conv_delete_conversation(conversation_id: String) -> Result<(), String> {
    let mut st = STATE.write();
    st.conversations.retain(|c| c.id != conversation_id);
    st.messages.retain(|m| m.conversation_id != conversation_id);
    drop(st);
    persist();
    Ok(())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn conv_get_messages(conversation_id: String) -> Vec<Message> {
    get_messages(&conversation_id)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn conv_rename_conversation(conversation_id: String, title: String) -> Result<(), String> {
    let mut st = STATE.write();
    for c in st.conversations.iter_mut() {
        if c.id == conversation_id {
            c.title = title.clone();
            c.updated_at = now_ms();
        }
    }
    drop(st);
    persist();
    Ok(())
}
