//! collab/tasks.rs —— 任务卡六态状态机(v8 第四节)。
//!
//! 一张卡 = 一个任务 = 一条分支。状态由这里的确定性代码推进,Issue 标签只是展示投影。
//! 六态:pending(待领取) → in_progress(进行中) → review(待验收) → merged(已合并) → archived(归档),
//! 外加 cancelled 终态。「打回」不是独立状态而是动作:review → in_progress(同分支续改,round+1)。
//! 打回熔断:同卡满 3 轮自动置 escalated 标记(抄送 owner,方案兜底 13)。
use rusqlite::{params, Connection, TransactionBehavior};

use super::db::{self, now, open_db};

/// 打回熔断阈值:第 3 轮起抄送 owner(AI 指挥的尽头必须站着一个人)。
pub const ESCALATE_ROUNDS: i64 = 3;

#[derive(serde::Serialize, Clone, Debug)]
pub struct TaskCard {
    pub id: i64,
    pub project_id: i64,
    pub title: String,
    pub body: String,
    pub scope: String,
    pub criteria: String,
    pub assignee: Option<i64>,
    pub state: String,
    pub round: i64,
    pub branch: String,
    pub pr_id: Option<i64>,
    pub issue_no: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

fn row_to_card(r: &rusqlite::Row) -> rusqlite::Result<TaskCard> {
    Ok(TaskCard {
        id: r.get(0)?,
        project_id: r.get(1)?,
        title: r.get(2)?,
        body: r.get(3)?,
        scope: r.get(4)?,
        criteria: r.get(5)?,
        assignee: r.get(6)?,
        state: r.get(7)?,
        round: r.get(8)?,
        branch: r.get(9)?,
        pr_id: r.get(10)?,
        issue_no: r.get(11)?,
        created_at: r.get(12)?,
        updated_at: r.get(13)?,
    })
}

const CARD_COLS: &str = "id,project_id,title,body,scope,criteria,assignee,state,round,branch,pr_id,issue_no,created_at,updated_at";

/// 建卡。四要素缺一不可(做什么/改哪里/验收标准/标题)——防"拆得云山雾罩"。
pub fn create(
    project_id: i64,
    title: &str,
    body: &str,
    scope: &str,
    criteria: &str,
    actor: &str,
) -> Result<TaskCard, String> {
    let (title, body, scope, criteria) = (title.trim(), body.trim(), scope.trim(), criteria.trim());
    if title.is_empty() || body.is_empty() || scope.is_empty() || criteria.is_empty() {
        return Err("任务卡四要素不全:标题、做什么、改哪里(scope)、验收标准都必填".into());
    }
    let conn = open_db()?;
    let t = now();
    conn.execute(
        "INSERT INTO tasks(project_id,title,body,scope,criteria,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?6)",
        params![project_id, title, body, scope, criteria, t],
    )
    .map_err(|e| format!("建卡失败: {e}"))?;
    let id = conn.last_insert_rowid();
    db::audit(actor, "task.create", &id.to_string(), title);
    get(id)
}

pub fn get(task_id: i64) -> Result<TaskCard, String> {
    let conn = open_db()?;
    get_on(&conn, task_id)
}

fn get_on(conn: &Connection, task_id: i64) -> Result<TaskCard, String> {
    conn.query_row(
        &format!("SELECT {CARD_COLS} FROM tasks WHERE id=?1"),
        params![task_id],
        row_to_card,
    )
    .map_err(|_| format!("任务 #{task_id} 不存在"))
}

pub fn list(project_id: i64) -> Result<Vec<TaskCard>, String> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {CARD_COLS} FROM tasks WHERE project_id=?1 ORDER BY updated_at DESC"
        ))
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![project_id], row_to_card)
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// 状态机合法迁移表——非法迁移一律拒绝,这是"状态由代码推进"的硬保证。
fn allowed(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("pending", "in_progress")
            | ("pending", "cancelled")
            | ("in_progress", "review")
            | ("in_progress", "cancelled")
            | ("review", "in_progress")   // 打回
            | ("review", "merged")
            | ("review", "cancelled")
            | ("merged", "archived")
    )
}

fn set_state(
    conn: &rusqlite::Connection,
    task_id: i64,
    from: &str,
    to: &str,
) -> Result<(), String> {
    if !allowed(from, to) {
        return Err(format!("非法状态迁移: {from} → {to}"));
    }
    let n = conn
        .execute(
            "UPDATE tasks SET state=?1, updated_at=?2 WHERE id=?3 AND state=?4",
            params![to, now(), task_id, from],
        )
        .map_err(|e| e.to_string())?;
    if n != 1 {
        return Err(format!(
            "任务 #{task_id} 状态已变化(期望 {from}),请刷新重试"
        ));
    }
    Ok(())
}

/// 领取:待领取 → 进行中,记 assignee 与分支名(task/日期-成员-短名)。
pub fn claim(task_id: i64, user_id: i64, username: &str) -> Result<TaskCard, String> {
    let mut conn = open_db()?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| format!("领取事务失败: {e}"))?;
    let card = get_on(&tx, task_id)?;
    let slug: String = card
        .title
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(16)
        .collect::<String>()
        .to_lowercase();
    let date = chrono::Local::now().format("%Y%m%d").to_string();
    let branch = if slug.is_empty() {
        format!("task/{date}-{username}-{task_id}")
    } else {
        format!("task/{date}-{username}-{slug}")
    };
    set_state(&tx, task_id, &card.state, "in_progress")?;
    tx.execute(
        "UPDATE tasks SET assignee=?1, branch=?2, updated_at=?3 WHERE id=?4",
        params![user_id, branch, now(), task_id],
    )
    .map_err(|e| e.to_string())?;
    let out = get_on(&tx, task_id)?;
    tx.commit().map_err(|e| format!("提交领取失败: {e}"))?;
    db::audit(username, "task.claim", &task_id.to_string(), &branch);
    Ok(out)
}

/// 提交:进行中 → 待验收(关联 PR)。
pub fn submit(task_id: i64, pr_id: Option<i64>, actor: &str) -> Result<TaskCard, String> {
    let mut conn = open_db()?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| format!("提交任务事务失败: {e}"))?;
    let card = get_on(&tx, task_id)?;
    set_state(&tx, task_id, &card.state, "review")?;
    if let Some(pr) = pr_id {
        tx.execute(
            "UPDATE tasks SET pr_id=?1, updated_at=?2 WHERE id=?3",
            params![pr, now(), task_id],
        )
        .map_err(|e| e.to_string())?;
    }
    let out = get_on(&tx, task_id)?;
    tx.commit().map_err(|e| format!("提交任务失败: {e}"))?;
    db::audit(
        actor,
        "task.submit",
        &task_id.to_string(),
        &pr_id.map(|p| p.to_string()).unwrap_or_default(),
    );
    Ok(out)
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct ReviewOutcome {
    pub card: TaskCard,
    /// 打回满 ESCALATE_ROUNDS 轮:true=已触发熔断,应抄送 owner。
    pub escalated: bool,
}

/// 验收:通过(review→merged 由合并闸门调用 mark_merged,这里只记意见)或打回。
/// 打回:review → in_progress,round+1,意见入 review_rounds(JSON,逐条挂验收标准)。
pub fn review(
    task_id: i64,
    reviewer: &str,
    pass: bool,
    comments_json: &str,
) -> Result<ReviewOutcome, String> {
    let mut conn = open_db()?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| format!("验收事务失败: {e}"))?;
    let card = get_on(&tx, task_id)?;
    if card.state != "review" {
        return Err(format!("任务 #{task_id} 不在待验收状态"));
    }
    let round = card.round + 1;
    tx.execute(
        "INSERT INTO review_rounds(task_id,round,reviewer,verdict,comments,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
        params![task_id, round, reviewer, if pass { "pass" } else { "reject" }, comments_json, now()],
    )
    .map_err(|e| e.to_string())?;
    let escalated = if pass {
        // 通过:停在 review,等合并闸门(机器闸+放行闸)调 mark_merged。
        tx.execute(
            "UPDATE tasks SET round=?1, updated_at=?2 WHERE id=?3",
            params![round, now(), task_id],
        )
        .map_err(|e| e.to_string())?;
        false
    } else {
        set_state(&tx, task_id, "review", "in_progress")?;
        tx.execute(
            "UPDATE tasks SET round=?1, updated_at=?2 WHERE id=?3",
            params![round, now(), task_id],
        )
        .map_err(|e| e.to_string())?;
        round >= ESCALATE_ROUNDS
    };
    let out = get_on(&tx, task_id)?;
    tx.commit().map_err(|e| format!("提交验收失败: {e}"))?;
    db::audit(
        reviewer,
        if pass {
            "task.review.pass"
        } else {
            "task.review.reject"
        },
        &task_id.to_string(),
        &format!("round={round}"),
    );
    Ok(ReviewOutcome {
        card: out,
        escalated,
    })
}

/// 合并闸门放行后落状态:review → merged。
pub fn mark_merged(task_id: i64, actor: &str) -> Result<TaskCard, String> {
    let mut conn = open_db()?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let card = get_on(&tx, task_id)?;
    set_state(&tx, task_id, &card.state, "merged")?;
    let out = get_on(&tx, task_id)?;
    tx.commit().map_err(|e| e.to_string())?;
    db::audit(actor, "task.merged", &task_id.to_string(), &card.branch);
    Ok(out)
}

pub fn archive(task_id: i64, actor: &str) -> Result<TaskCard, String> {
    let mut conn = open_db()?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let card = get_on(&tx, task_id)?;
    set_state(&tx, task_id, &card.state, "archived")?;
    let out = get_on(&tx, task_id)?;
    tx.commit().map_err(|e| e.to_string())?;
    db::audit(actor, "task.archive", &task_id.to_string(), "");
    Ok(out)
}

pub fn cancel(task_id: i64, actor: &str) -> Result<TaskCard, String> {
    let mut conn = open_db()?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let card = get_on(&tx, task_id)?;
    set_state(&tx, task_id, &card.state, "cancelled")?;
    let out = get_on(&tx, task_id)?;
    tx.commit().map_err(|e| e.to_string())?;
    db::audit(actor, "task.cancel", &task_id.to_string(), "");
    Ok(out)
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct ReviewRound {
    pub round: i64,
    pub reviewer: String,
    pub verdict: String,
    pub comments: String,
    pub created_at: i64,
}

/// 全部轮次留痕(第 N 轮能看到前 N-1 轮的完整脉络)。
pub fn rounds(task_id: i64) -> Result<Vec<ReviewRound>, String> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare("SELECT round,reviewer,verdict,comments,created_at FROM review_rounds WHERE task_id=?1 ORDER BY round")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![task_id], |r| {
            Ok(ReviewRound {
                round: r.get(0)?,
                reviewer: r.get(1)?,
                verdict: r.get(2)?,
                comments: r.get(3)?,
                created_at: r.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// 项目动态时间线条目(GitHub activity feed 式)。kind: review|task。
#[derive(serde::Serialize)]
pub struct ActivityItem {
    pub kind: String,
    pub actor: String,
    pub task_id: i64,
    pub title: String,
    /// review: "pass/reject · 第N轮";task: 当前 state
    pub detail: String,
    pub at: i64,
}

/// 项目动态:验收轮次 + 任务状态变化合成,时间倒序。
/// 不读 audit 表——其 target 格式跨模块不统一,合成查询确定性更好。
pub fn activity(project_id: i64, limit: i64) -> Result<Vec<ActivityItem>, String> {
    let conn = open_db()?;
    let mut items: Vec<ActivityItem> = Vec::new();
    // ① 验收/打回留痕(谁验的、过没过、第几轮)
    let mut stmt = conn
        .prepare(
            "SELECT r.reviewer, r.task_id, t.title, r.verdict, r.round, r.created_at
             FROM review_rounds r JOIN tasks t ON r.task_id=t.id
             WHERE t.project_id=?1 ORDER BY r.created_at DESC LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![project_id, limit], |r| {
            Ok(ActivityItem {
                kind: "review".into(),
                actor: r.get(0)?,
                task_id: r.get(1)?,
                title: r.get(2)?,
                detail: format!("{} · 第{}轮", r.get::<_, String>(3)?, r.get::<_, i64>(4)?),
                at: r.get(5)?,
            })
        })
        .map_err(|e| e.to_string())?;
    items.extend(rows.flatten());
    // ② 任务状态流(谁在做、到哪一态)。display_name 空串退 username。
    let mut stmt = conn
        .prepare(
            "SELECT COALESCE(CASE WHEN u.display_name='' THEN u.username ELSE u.display_name END, ''),
                    t.id, t.title, t.state, t.updated_at
             FROM tasks t LEFT JOIN users u ON t.assignee=u.id
             WHERE t.project_id=?1 ORDER BY t.updated_at DESC LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![project_id, limit], |r| {
            Ok(ActivityItem {
                kind: "task".into(),
                actor: r.get(0)?,
                task_id: r.get(1)?,
                title: r.get(2)?,
                detail: r.get(3)?,
                at: r.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;
    items.extend(rows.flatten());
    items.sort_by(|a, b| b.at.cmp(&a.at));
    items.truncate(limit.max(0) as usize);
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db() -> std::sync::MutexGuard<'static, ()> {
        let g = super::super::db::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let p = std::env::temp_dir().join(format!(
            "collab-tasks-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("POLARIS_COLLAB_DB", p);
        g
    }

    fn mk_project() -> (i64, i64) {
        let conn = open_db().unwrap();
        conn.execute(
            "INSERT INTO projects(name,created_at) VALUES('demo',?1)",
            params![now()],
        )
        .unwrap();
        let pid = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO users(username,pass_hash,created_at) VALUES('ming','x',?1)",
            params![now()],
        )
        .unwrap();
        (pid, conn.last_insert_rowid())
    }

    #[test]
    fn full_lifecycle_with_rejects() {
        let _g = tmp_db();
        let (pid, uid) = mk_project();
        // 四要素缺一不可
        assert!(create(pid, "t", "", "src/", "c1", "boss").is_err());
        let card = create(
            pid,
            "开场重写",
            "重写第三章开场",
            "script/ch03/",
            "1.对白自然 2.用新角色名",
            "boss",
        )
        .unwrap();
        assert_eq!(card.state, "pending");

        let card = claim(card.id, uid, "ming").unwrap();
        assert_eq!(card.state, "in_progress");
        assert!(card.branch.starts_with("task/"));

        let card = submit(card.id, Some(42), "ming").unwrap();
        assert_eq!(card.state, "review");

        // 打回两轮
        let r1 = review(
            card.id,
            "lead:tech",
            false,
            "[{\"item\":1,\"note\":\"对白生硬\"}]",
        )
        .unwrap();
        assert_eq!(r1.card.state, "in_progress");
        assert!(!r1.escalated);
        submit(card.id, Some(42), "ming").unwrap();
        let r2 = review(card.id, "lead:tech", false, "[]").unwrap();
        assert!(!r2.escalated);
        // 第三轮打回触发熔断
        submit(card.id, Some(42), "ming").unwrap();
        let r3 = review(card.id, "lead:tech", false, "[]").unwrap();
        assert!(r3.escalated);

        // 第四轮通过 → merged → archived
        submit(card.id, Some(42), "ming").unwrap();
        let r4 = review(card.id, "boss", true, "[]").unwrap();
        assert_eq!(r4.card.state, "review");
        let card = mark_merged(card.id, "gate").unwrap();
        assert_eq!(card.state, "merged");
        let card = archive(card.id, "gate").unwrap();
        assert_eq!(card.state, "archived");

        // 全程留痕 4 轮
        assert_eq!(rounds(card.id).unwrap().len(), 4);
        // 归档后不可再动
        assert!(cancel(card.id, "x").is_err());
    }

    #[test]
    fn activity_merges_reviews_and_tasks() {
        let _g = tmp_db();
        let (pid, uid) = mk_project();
        let card = create(pid, "首页改版", "改首页", "src/", "1.过验收", "boss").unwrap();
        claim(card.id, uid, "ming").unwrap();
        submit(card.id, None, "ming").unwrap();
        review(card.id, "boss", false, "[]").unwrap();
        let items = activity(pid, 30).unwrap();
        // 两类条目都在:一条 review(打回)+ 至少一条 task(状态流)
        assert!(items
            .iter()
            .any(|i| i.kind == "review" && i.detail.contains("reject")));
        assert!(items
            .iter()
            .any(|i| i.kind == "task" && i.title == "首页改版"));
        // 时间倒序
        for w in items.windows(2) {
            assert!(w[0].at >= w[1].at);
        }
        // limit 生效
        assert_eq!(activity(pid, 1).unwrap().len(), 1);
    }

    #[test]
    fn illegal_transitions_rejected() {
        let _g = tmp_db();
        let (pid, _uid) = mk_project();
        let card = create(pid, "卡", "做事", "src/", "标准", "boss").unwrap();
        assert!(submit(card.id, None, "x").is_err()); // pending 不能直接提交
        assert!(mark_merged(card.id, "x").is_err()); // pending 不能直接合并
    }
}
