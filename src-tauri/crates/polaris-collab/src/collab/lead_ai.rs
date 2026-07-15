//! collab/lead_ai.rs —— 主 Agent 的 AI 侧(v8 6b「主 Agent 上岗」)。
//!
//! 模型只产出**申请**:拆卡草案、验收意见草稿。每一次落地动作都经 lead.rs 的
//! guard(授权表三问)与状态机执行——模型输出永远进不了权限通路。
//! 模型配置独立于聊天供应商(lead_model.json:OpenAI 兼容端点),没配则 AI 功能
//! 整体不可用,看板照常(纯人工模式是一等公民)。
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

use super::db;
use super::lead;
use super::tasks;

#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
pub struct LeadModelCfg {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
}

fn cfg_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("POLARIS_LEAD_MODEL_CFG") {
        if !p.trim().is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    directories::UserDirs::new().map(|u| {
        u.home_dir()
            .join("Polaris")
            .join("data")
            .join("lead_model.json")
    })
}

pub fn load_cfg() -> LeadModelCfg {
    cfg_path()
        .filter(|p| p.exists())
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_cfg(cfg: &LeadModelCfg) -> Result<(), String> {
    let path = cfg_path().ok_or("无法定位数据目录")?;
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    std::fs::write(
        &path,
        serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn active_cfg() -> Result<LeadModelCfg, String> {
    let c = load_cfg();
    if c.enabled
        && !c.base_url.trim().is_empty()
        && !c.api_key.trim().is_empty()
        && !c.model.trim().is_empty()
    {
        Ok(c)
    } else {
        Err("主 Agent 模型未配置(设置里填 OpenAI 兼容端点),或已停用——看板可继续纯人工使用".into())
    }
}

/// OpenAI 兼容单轮调用。返回(文本, 估算 token)。
fn complete(cfg: &LeadModelCfg, system: &str, user: &str) -> Result<(String, i64), String> {
    let url = format!("{}/v1/chat/completions", cfg.base_url.trim_end_matches('/'));
    let http = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(180))
        .build();
    let resp = http
        .post(&url)
        .set("authorization", &format!("Bearer {}", cfg.api_key.trim()))
        .send_json(json!({
            "model": cfg.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user}
            ],
            "temperature": 0.2,
            "stream": false,
        }));
    let v: Value = match resp {
        Ok(r) => r
            .into_json()
            .map_err(|e| format!("主 Agent 模型响应解析失败: {e}"))?,
        Err(ureq::Error::Status(code, r)) => {
            let brief: String = r
                .into_string()
                .unwrap_or_default()
                .chars()
                .take(220)
                .collect();
            return Err(format!("主 Agent 模型 HTTP {code}: {brief}"));
        }
        Err(e) => return Err(format!("主 Agent 模型网络错误: {e}")),
    };
    let text = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|t| t.as_str())
        .ok_or("主 Agent 模型响应里没有 content")?
        .to_string();
    let tokens = v
        .get("usage")
        .and_then(|u| u.get("total_tokens"))
        .and_then(|t| t.as_i64())
        .unwrap_or(((system.len() + user.len() + text.len()) / 3) as i64);
    Ok((text, tokens))
}

/// 从模型输出里抠 JSON(容忍 ```json 围栏与前后废话)。
fn extract_json(text: &str) -> Result<Value, String> {
    let t = text.trim();
    let candidate = if let Some(i) = t.find("```") {
        let rest = &t[i + 3..];
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        rest.split("```").next().unwrap_or(rest)
    } else {
        t
    };
    let start = candidate.find(['[', '{']).ok_or("模型输出中没有 JSON")?;
    let s = &candidate[start..];
    serde_json::from_str(s.trim()).map_err(|e| format!("模型输出 JSON 解析失败: {e}"))
}

/// 拆解目标 → 任务卡草案(不落库;auto_dispatch 开着才由调用侧真正建卡)。
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct CardDraft {
    pub title: String,
    pub body: String,
    pub scope: String,
    pub criteria: String,
}

/// ① 拆解:owner 目标 → 卡草案列表。guard 过了才会调模型(预算闸也在 guard 里)。
pub fn ai_decompose(
    project_id: i64,
    goal: &str,
    member_hint: &str,
) -> Result<Vec<CardDraft>, String> {
    let actor = lead::guard(project_id, lead::LeadAction::CreateTask)?;
    let cfg = active_cfg()?;
    let system = "你是项目主脑(主 Agent)。把 owner 的目标拆成可执行的任务卡。\
        每张卡四要素齐全:title(短标题)、body(做什么、背景)、scope(预计改动的目录/文件前缀,逗号分隔)、\
        criteria(逐条可判定的验收标准,用换行分隔)。写不出可判定验收标准的活不要硬拆,\
        改为输出一张 title 以「待澄清:」开头的卡说明缺什么信息。\
        只输出 JSON 数组:[{\"title\":...,\"body\":...,\"scope\":...,\"criteria\":...}],不超过 8 张。";
    let user = format!("目标:\n{goal}\n\n成员画像(供工作量与分工参考):\n{member_hint}");
    let (text, tokens) = complete(&cfg, system, &user)?;
    lead::add_usage(project_id, tokens)?;
    db::audit(
        &actor,
        "lead.ai.decompose",
        &project_id.to_string(),
        &format!("tokens={tokens}"),
    );
    let v = extract_json(&text)?;
    let drafts: Vec<CardDraft> =
        serde_json::from_value(v).map_err(|e| format!("卡草案结构不符: {e}"))?;
    Ok(drafts
        .into_iter()
        .filter(|d| !d.title.trim().is_empty())
        .collect())
}

/// ② 验收:对照验收标准审 diff → 意见草稿(pass 建议 + 逐条意见)。不直接落状态机。
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct ReviewDraft {
    pub pass: bool,
    pub comments: Vec<ReviewComment>,
    pub summary: String,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct ReviewComment {
    pub item: i64,
    pub note: String,
}

pub fn ai_review(project_id: i64, task_id: i64, diff_text: &str) -> Result<ReviewDraft, String> {
    let actor = lead::guard(project_id, lead::LeadAction::Review)?;
    let card = tasks::get(task_id)?;
    if card.project_id != project_id {
        return Err("任务不属于本项目".into());
    }
    let cfg = active_cfg()?;
    // 历史轮次:第 N 轮能看到前 N-1 轮(不重复、不打架)。
    let history = tasks::rounds(task_id)?
        .iter()
        .map(|r| format!("第{}轮[{}]: {}", r.round, r.verdict, r.comments))
        .collect::<Vec<_>>()
        .join("\n");
    let system = "你是项目主脑,对照任务卡的验收标准逐条审查改动。\
        只输出 JSON:{\"pass\":bool,\"summary\":\"一句话结论\",\
        \"comments\":[{\"item\":标准序号(从1),\"note\":\"哪条没达标、差在哪、怎么改算过\"}]}。\
        pass=true 时 comments 为空数组。历史轮次里提过且已改好的不要重复提。";
    let diff_capped: String = diff_text.chars().take(60_000).collect();
    let user = format!(
        "任务卡:{}\n做什么:{}\n验收标准:\n{}\n\n历史轮次:\n{}\n\n本次改动 diff:\n{}",
        card.title,
        card.body,
        card.criteria,
        if history.is_empty() {
            "(首轮)"
        } else {
            &history
        },
        diff_capped
    );
    let (text, tokens) = complete(&cfg, system, &user)?;
    lead::add_usage(project_id, tokens)?;
    db::audit(
        &actor,
        "lead.ai.review",
        &task_id.to_string(),
        &format!("tokens={tokens}"),
    );
    let v = extract_json(&text)?;
    serde_json::from_value(v).map_err(|e| format!("验收草稿结构不符: {e}"))
}

/// ④ 任务对话回复:把任务卡 + 最近 30 条对话拼成多轮上下文,产出一条回复文本。
/// 仍只是建议——写进消息流(chat.rs),不碰状态机;走 Review 授权位与预算闸。
pub fn ai_task_reply(project_id: i64, task_id: i64) -> Result<String, String> {
    let actor = lead::guard(project_id, lead::LeadAction::Review)?;
    let card = tasks::get(task_id)?;
    if card.project_id != project_id {
        return Err("任务不属于本项目".into());
    }
    let cfg = active_cfg()?;
    let system = "你是项目主脑(主 Agent),在任务卡的对话线程里回复协作者。\
        依据任务卡的目标与验收标准,针对最新问题给出具体、可操作的建议;\
        可以建议改法、指出验收差距,但你没有决定权:状态变更/合并/放行由负责人另行操作,\
        不要宣称已通过或已合并。直接输出回复正文,中文,不要围栏、不要 JSON。";
    let history = super::chat::recent(task_id, 30)?
        .iter()
        .map(|m| format!("[{}·{}] {}", m.author_name, m.role, m.body))
        .collect::<Vec<_>>()
        .join("\n");
    let user = format!(
        "任务卡:{}\n做什么:{}\n验收标准:\n{}\n\n对话记录(旧→新):\n{}\n\n请回复最新一条。",
        card.title,
        card.body,
        card.criteria,
        if history.is_empty() {
            "(还没有消息,请开场介绍你能帮什么)"
        } else {
            &history
        }
    );
    let (text, tokens) = complete(&cfg, system, &user)?;
    lead::add_usage(project_id, tokens)?;
    db::audit(
        &actor,
        "lead.ai.task_reply",
        &task_id.to_string(),
        &format!("tokens={tokens}"),
    );
    Ok(text.trim().to_string())
}

/// ③ 融合草案:冲突块两侧 → 融合文本建议(必落 PR 分支、必经人确认,调用侧保证)。
pub fn ai_fuse(project_id: i64, ours: &str, theirs: &str, context: &str) -> Result<String, String> {
    let actor = lead::guard(project_id, lead::LeadAction::Adjudicate)?;
    let cfg = active_cfg()?;
    let system = "你是项目主脑,为一个 git 三方合并冲突块起草融合版本。\
        理解两侧意图,产出兼容双方目的的最终文本。只输出融合后的文本本身,不要解释、不要围栏。";
    let user =
        format!("冲突上下文:{context}\n\n=== main 侧 ===\n{ours}\n\n=== 任务分支侧 ===\n{theirs}");
    let (text, tokens) = complete(&cfg, system, &user)?;
    lead::add_usage(project_id, tokens)?;
    db::audit(
        &actor,
        "lead.ai.fuse",
        &project_id.to_string(),
        &format!("tokens={tokens}"),
    );
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_tolerates_fences() {
        let v = extract_json("好的,以下是结果:\n```json\n[{\"title\":\"a\",\"body\":\"b\",\"scope\":\"s\",\"criteria\":\"c\"}]\n```").unwrap();
        assert!(v.is_array());
        let v2 = extract_json("{\"pass\":true,\"summary\":\"ok\",\"comments\":[]}").unwrap();
        assert_eq!(v2["pass"], true);
        assert!(extract_json("完全没有 json").is_err());
    }

    #[test]
    fn unconfigured_model_gives_clear_error() {
        let _g = super::super::db::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::set_var(
            "POLARIS_COLLAB_DB",
            std::env::temp_dir().join(format!("leadai-{ts}.db")),
        );
        std::env::set_var(
            "POLARIS_LEAD_MODEL_CFG",
            std::env::temp_dir().join(format!("leadai-cfg-{ts}.json")),
        );
        // 建项目并任命主 Agent,但模型未配置 → 明确错误而非崩溃
        let conn = super::super::db::open_db().unwrap();
        conn.execute(
            "INSERT INTO projects(name,lead_expert_id,created_at) VALUES('p','tech',0)",
            [],
        )
        .unwrap();
        let pid = conn.last_insert_rowid();
        let err = ai_decompose(pid, "做个网站", "").unwrap_err();
        assert!(err.contains("未配置"), "实际: {err}");
    }
}
