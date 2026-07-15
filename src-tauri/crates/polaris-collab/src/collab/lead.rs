//! collab/lead.rs —— 主 Agent 调度中枢的确定性内核(v8 第三节)。
//!
//! 主 Agent 不是新进程:它是"被授予指挥工具面的一个专家会话"。本文件只包含
//! **确定性部分**:授权表三问、指挥六件套的落地执行、晨会盘点数据、token 预算闸。
//! 大模型只能"申请"操作——每个申请先过 guard(),三问全对才执行:
//!   ① 该项目是否任命了主 Agent?
//!   ② 操作在不在其授权位上?
//!   ③ 目标是不是本项目资源?
//! 权限类 API(加成员/发票据/吊销设备/改角色)物理不在本文件——不是提示词禁止,是不可达。
use rusqlite::params;

use super::db::{self, now, open_db};
use super::{projects, tasks};

/// 主 Agent 的授权位(owner 可随时改;默认全保守=仅出意见)。
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct LeadGrants {
    pub can_merge: bool,
    pub can_reassign: bool,
    pub auto_dispatch: bool,
    pub token_budget: i64,
}

impl Default for LeadGrants {
    fn default() -> Self {
        Self {
            can_merge: false,
            can_reassign: false,
            auto_dispatch: false,
            token_budget: 200_000,
        }
    }
}

pub fn get_grants(project_id: i64) -> Result<LeadGrants, String> {
    let conn = open_db()?;
    conn.query_row(
        "SELECT can_merge,can_reassign,auto_dispatch,token_budget FROM lead_grants WHERE project_id=?1",
        params![project_id],
        |r| {
            Ok(LeadGrants {
                can_merge: r.get::<_, i64>(0)? != 0,
                can_reassign: r.get::<_, i64>(1)? != 0,
                auto_dispatch: r.get::<_, i64>(2)? != 0,
                token_budget: r.get(3)?,
            })
        },
    )
    .or_else(|_| Ok(LeadGrants::default()))
}

/// owner 设授权位(一键降档就是把位清零)。
pub fn set_grants(project_id: i64, g: &LeadGrants, actor: &str) -> Result<(), String> {
    let conn = open_db()?;
    conn.execute(
        "INSERT OR REPLACE INTO lead_grants(project_id,can_merge,can_reassign,auto_dispatch,token_budget) \
         VALUES(?1,?2,?3,?4,?5)",
        params![project_id, g.can_merge as i64, g.can_reassign as i64, g.auto_dispatch as i64, g.token_budget],
    )
    .map_err(|e| e.to_string())?;
    db::audit(
        actor,
        "lead.grants.set",
        &project_id.to_string(),
        &serde_json::to_string(g).unwrap_or_default(),
    );
    Ok(())
}

/// 指挥动作枚举——工具面的全集。注意:没有任何权限类动作。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LeadAction {
    CreateTask, // 拆解建卡
    Assign,     // 分派/改派
    Review,     // 验收(出意见:通过/打回)
    Adjudicate, // 冲突裁决(出处置意见/融合草案落 PR 分支)
    Merge,      // 合并放行(受 can_merge 位)
    Nudge,      // 催办
}

/// 三问预过滤。全对返回主 Agent 的 actor 名("lead:<expert_id>"),任何一问不过即拒。
pub fn guard(project_id: i64, action: LeadAction) -> Result<String, String> {
    // 一问:项目在册主 Agent?
    let p = projects::get(project_id)?;
    let Some(expert) = p.lead_expert_id.clone().filter(|s| !s.is_empty()) else {
        return Err("该项目未任命主 Agent(纯人工模式)".into());
    };
    // 二问:操作在授权位上?(建卡/验收/裁决/催办是基本盘;改派与合并要显式授权)
    let g = get_grants(project_id)?;
    match action {
        LeadAction::Assign if !g.can_reassign => {
            return Err("主 Agent 未被授予改派权(owner 可在项目设置开启)".into())
        }
        LeadAction::Merge if !g.can_merge => {
            return Err("主 Agent 未被授予自动合并权,请申请人工放行".into())
        }
        _ => {}
    }
    // 三问由调用侧携带的目标校验完成(tasks::get 的 project_id 必须等于本项目,见各工具)。
    // token 预算闸:烧穿即暂停指挥。
    if budget_left(project_id, g.token_budget)? <= 0 {
        return Err("主 Agent 今日 token 预算已耗尽,指挥暂停(看板照常,owner 可调预算)".into());
    }
    Ok(format!("lead:{expert}"))
}

/// 校验任务确属本项目(三问之③)。
fn ensure_task_in(project_id: i64, task_id: i64) -> Result<tasks::TaskCard, String> {
    let card = tasks::get(task_id)?;
    if card.project_id != project_id {
        return Err(format!(
            "任务 #{task_id} 不属于项目 #{project_id},拒绝跨项目操作"
        ));
    }
    Ok(card)
}

// ───────────────────────── 指挥六件套 ─────────────────────────

/// ① 拆解:建任务卡(四要素强制)。
pub fn lead_create_task(
    project_id: i64,
    title: &str,
    body: &str,
    scope: &str,
    criteria: &str,
) -> Result<tasks::TaskCard, String> {
    let actor = guard(project_id, LeadAction::CreateTask)?;
    tasks::create(project_id, title, body, scope, criteria, &actor)
}

/// ② 分派/改派:只能派给项目成员表里的人。
pub fn lead_assign(project_id: i64, task_id: i64, user_id: i64) -> Result<tasks::TaskCard, String> {
    let actor = guard(project_id, LeadAction::Assign)?;
    ensure_task_in(project_id, task_id)?;
    if projects::member_role(project_id, user_id).is_none() {
        return Err("目标不是项目成员,拒绝分派".into());
    }
    let conn = open_db()?;
    conn.execute(
        "UPDATE tasks SET assignee=?1, updated_at=?2 WHERE id=?3",
        params![user_id, now(), task_id],
    )
    .map_err(|e| e.to_string())?;
    db::audit(
        &actor,
        "lead.assign",
        &task_id.to_string(),
        &user_id.to_string(),
    );
    tasks::get(task_id)
}

/// ③ 验收:出意见(通过/打回)。合并是另一个受闸动作。
pub fn lead_review(
    project_id: i64,
    task_id: i64,
    pass: bool,
    comments_json: &str,
) -> Result<tasks::ReviewOutcome, String> {
    let actor = guard(project_id, LeadAction::Review)?;
    ensure_task_in(project_id, task_id)?;
    tasks::review(task_id, &actor, pass, comments_json)
}

/// ⑤ 合并放行:验收已过 + 试算干净由调用侧(mergectl)保证;这里只判授权位并落状态。
pub fn lead_approve_merge(project_id: i64, task_id: i64) -> Result<tasks::TaskCard, String> {
    let actor = guard(project_id, LeadAction::Merge)?;
    let card = ensure_task_in(project_id, task_id)?;
    if card.state != "review" {
        return Err("任务不在待验收状态,不能放行合并".into());
    }
    db::audit(
        &actor,
        "lead.merge.approve",
        &task_id.to_string(),
        &card.branch,
    );
    Ok(card)
}

/// ⑥ 催办:纯读 + 留痕,产出催办列表(超期无动静的卡)。
pub fn lead_nudge(project_id: i64, stale_hours: i64) -> Result<Vec<tasks::TaskCard>, String> {
    let actor = guard(project_id, LeadAction::Nudge)?;
    let cutoff = now() - stale_hours * 3600;
    let stale: Vec<_> = tasks::list(project_id)?
        .into_iter()
        .filter(|c| c.state == "in_progress" && c.updated_at < cutoff)
        .collect();
    if !stale.is_empty() {
        db::audit(
            &actor,
            "lead.nudge",
            &project_id.to_string(),
            &format!("{} 张卡超期", stale.len()),
        );
    }
    Ok(stale)
}

// ───────────────────────── 晨会盘点(纯读) ─────────────────────────

#[derive(serde::Serialize)]
pub struct MorningReport {
    pub project_id: i64,
    pub merged_yesterday: Vec<tasks::TaskCard>,
    pub rejected_open: Vec<tasks::TaskCard>, // 被打回待续改
    pub review_queue: Vec<tasks::TaskCard>,  // 待验收
    pub stale: Vec<tasks::TaskCard>,         // 超 48h 无动静
    pub unclaimed: Vec<tasks::TaskCard>,     // 待领取
    pub escalated: Vec<tasks::TaskCard>,     // 打回满 3 轮
}

/// 晨会盘点数据——不依赖主 Agent 在线,纯状态机数据,owner 亲自当主脑时同样用它。
pub fn morning_report(project_id: i64) -> Result<MorningReport, String> {
    let all = tasks::list(project_id)?;
    let day_ago = now() - 24 * 3600;
    let two_days = now() - 48 * 3600;
    Ok(MorningReport {
        project_id,
        merged_yesterday: all
            .iter()
            .filter(|c| c.state == "merged" && c.updated_at >= day_ago)
            .cloned()
            .collect(),
        rejected_open: all
            .iter()
            .filter(|c| c.state == "in_progress" && c.round > 0)
            .cloned()
            .collect(),
        review_queue: all
            .iter()
            .filter(|c| c.state == "review")
            .cloned()
            .collect(),
        stale: all
            .iter()
            .filter(|c| c.state == "in_progress" && c.updated_at < two_days)
            .cloned()
            .collect(),
        unclaimed: all
            .iter()
            .filter(|c| c.state == "pending")
            .cloned()
            .collect(),
        escalated: all
            .iter()
            .filter(|c| {
                c.round >= tasks::ESCALATE_ROUNDS && c.state != "merged" && c.state != "archived"
            })
            .cloned()
            .collect(),
    })
}

// ───────────────────────── token 预算闸 ─────────────────────────

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// 记一笔主 Agent 的 token 消耗。
pub fn add_usage(project_id: i64, tokens: i64) -> Result<(), String> {
    let conn = open_db()?;
    conn.execute(
        "INSERT INTO lead_usage(project_id,day,tokens) VALUES(?1,?2,?3) \
         ON CONFLICT(project_id,day) DO UPDATE SET tokens=tokens+?3",
        params![project_id, today(), tokens],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 今日剩余预算。
pub fn budget_left(project_id: i64, budget: i64) -> Result<i64, String> {
    let conn = open_db()?;
    let used: i64 = conn
        .query_row(
            "SELECT tokens FROM lead_usage WHERE project_id=?1 AND day=?2",
            params![project_id, today()],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(budget - used)
}

// ───────────────────────── 晨会定时器 ─────────────────────────

/// 晨会调度:每天固定时刻(默认 08:30,POLARIS_MORNING_TIME=HH:MM 覆写)对每个任命了
/// 主 Agent 的项目产出晨会盘点,经回调推给外壳(server 广播 / 桌面 emit / 飞书通道)。
/// 主 Agent 离线也不影响:盘点是纯读数据,错过的次日自动重提(状态在库,不在会话)。
pub fn start_morning_scheduler<F>(emit: F)
where
    F: Fn(&str, serde_json::Value) + Send + 'static,
{
    std::thread::Builder::new()
        .name("collab-morning".into())
        .spawn(move || {
            let mut last_fired = String::new(); // 防同一天重复触发
            loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
                let target = std::env::var("POLARIS_MORNING_TIME").unwrap_or_else(|_| "08:30".into());
                let now_hm = chrono::Local::now().format("%H:%M").to_string();
                let today = today();
                if now_hm != target || last_fired == today {
                    continue;
                }
                last_fired = today.clone();
                let Ok(conn) = open_db() else { continue };
                let ids: Vec<i64> = conn
                    .prepare("SELECT id FROM projects WHERE archived=0 AND lead_expert_id IS NOT NULL AND lead_expert_id!=''")
                    .and_then(|mut s| s.query_map([], |r| r.get(0)).map(|rows| rows.flatten().collect()))
                    .unwrap_or_default();
                for pid in ids {
                    if let Ok(report) = morning_report(pid) {
                        if let Ok(v) = serde_json::to_value(&report) {
                            db::audit("scheduler", "lead.morning", &pid.to_string(), "");
                            emit("collab:morning", v);
                        }
                    }
                }
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (std::sync::MutexGuard<'static, ()>, i64, i64) {
        let g = super::super::db::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let p = std::env::temp_dir().join(format!(
            "collab-lead-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("POLARIS_COLLAB_DB", p);
        let conn = open_db().unwrap();
        conn.execute(
            "INSERT INTO users(username,pass_hash,created_at) VALUES('boss','x',?1)",
            params![now()],
        )
        .unwrap();
        let uid = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO projects(name,created_at) VALUES('demo',?1)",
            params![now()],
        )
        .unwrap();
        let pid = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO project_members(project_id,user_id,role) VALUES(?1,?2,'owner')",
            params![pid, uid],
        )
        .unwrap();
        (g, pid, uid)
    }

    #[test]
    fn guard_three_questions() {
        let (_g, pid, uid) = setup();
        // 未任命主 Agent → 一问不过
        assert!(lead_create_task(pid, "卡", "做事", "src/", "标准").is_err());
        projects::set_lead(pid, Some("tech-lead"), "boss").unwrap();
        // 任命后基本盘可用
        let card = lead_create_task(pid, "卡", "做事", "src/", "标准").unwrap();
        // 改派默认无权(二问)
        assert!(lead_assign(pid, card.id, uid).is_err());
        set_grants(
            pid,
            &LeadGrants {
                can_reassign: true,
                ..Default::default()
            },
            "boss",
        )
        .unwrap();
        assert!(lead_assign(pid, card.id, uid).is_ok());
        // 派给非成员被拒
        assert!(lead_assign(pid, card.id, 9999).is_err());
        // 合并默认无权
        assert!(lead_approve_merge(pid, card.id).is_err());
    }

    #[test]
    fn cross_project_rejected_and_budget() {
        let (_g, pid, _uid) = setup();
        projects::set_lead(pid, Some("tech-lead"), "boss").unwrap();
        // 三问:跨项目操作拒绝
        let conn = open_db().unwrap();
        conn.execute(
            "INSERT INTO projects(name,created_at) VALUES('other',?1)",
            params![now()],
        )
        .unwrap();
        let other = conn.last_insert_rowid();
        conn.execute(
            "UPDATE projects SET lead_expert_id='x' WHERE id=?1",
            params![other],
        )
        .unwrap();
        let card = lead_create_task(pid, "卡", "做事", "src/", "标准").unwrap();
        assert!(lead_review(other, card.id, true, "[]").is_err());
        // 预算烧穿 → 指挥暂停
        set_grants(
            pid,
            &LeadGrants {
                token_budget: 100,
                ..Default::default()
            },
            "boss",
        )
        .unwrap();
        add_usage(pid, 200).unwrap();
        assert!(lead_create_task(pid, "又一张", "做事", "src/", "标准").is_err());
        // 但看板(纯读盘点)照常
        assert!(morning_report(pid).is_ok());
    }
}
