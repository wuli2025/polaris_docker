//! collab/chat.rs —— 任务级多轮对话(v8 增补:协作者↔负责人↔主 Agent 的微调通道)。
//!
//! 与 review_rounds(验收工单轮次)互补:这里是随时来回的轻量消息,不动六态状态机。
//! author_user_id=0 且 role='ai' 表示主 Agent 的回复(仍只是建议/申请,落地必经 lead 通路)。
//! idem_key 供成员端 outbox 断线补传去重:同键重复投递返回既有消息而非再插一条。

use super::db;
use super::tasks;

/// 单条消息正文上限(16KB):对话是微调通道,不是文件通道,防刷爆库。
const BODY_MAX: usize = 16 * 1024;

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct TaskMessage {
    pub id: i64,
    pub task_id: i64,
    /// 发出时任务所处的验收轮次(对齐 review_rounds,便于回看"第几轮聊了什么")。
    pub round: i64,
    pub author_user_id: i64,
    pub author_name: String,
    /// lead | assignee | member | ai
    pub role: String,
    pub body: String,
    pub created_at: i64,
}

fn row_to_msg(r: &rusqlite::Row) -> rusqlite::Result<TaskMessage> {
    Ok(TaskMessage {
        id: r.get(0)?,
        task_id: r.get(1)?,
        round: r.get(2)?,
        author_user_id: r.get(3)?,
        author_name: r.get(4)?,
        role: r.get(5)?,
        body: r.get(6)?,
        created_at: r.get(7)?,
    })
}

const COLS: &str = "id, task_id, round, author_user_id, author_name, role, body, created_at";

/// 发消息。idem_key 撞库(补传重复)时幂等返回既有消息。
pub fn post(
    task_id: i64,
    author_user_id: i64,
    author_name: &str,
    role: &str,
    body: &str,
    idem_key: Option<&str>,
) -> Result<TaskMessage, String> {
    let body = body.trim();
    if body.is_empty() {
        return Err("消息不能为空".into());
    }
    if body.len() > BODY_MAX {
        return Err("消息超长(上限 16KB),请拆分或改走文件通道".into());
    }
    let card = tasks::get(task_id)?; // 顺带校验任务存在
    let conn = db::open_db()?;
    if let Some(key) = idem_key.filter(|k| !k.trim().is_empty()) {
        // 幂等:同键已存在 → 直接返回那条(补传方按成功处理)。
        if let Ok(m) = conn.query_row(
            &format!("SELECT {COLS} FROM task_messages WHERE idem_key = ?1"),
            rusqlite::params![key],
            |r| row_to_msg(r),
        ) {
            return Ok(m);
        }
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    conn.execute(
        "INSERT INTO task_messages(task_id, round, author_user_id, author_name, role, body, idem_key, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            task_id,
            card.round,
            author_user_id,
            author_name,
            role,
            body,
            idem_key.filter(|k| !k.trim().is_empty()),
            now
        ],
    )
    .map_err(|e| format!("消息落库失败: {e}"))?;
    let id = conn.last_insert_rowid();
    conn.query_row(
        &format!("SELECT {COLS} FROM task_messages WHERE id = ?1"),
        rusqlite::params![id],
        |r| row_to_msg(r),
    )
    .map_err(|e| format!("回读消息失败: {e}"))
}

/// 拉消息:after_id 之后的增量(0=从头),按 id 升序,limit 上限 200。
pub fn list(task_id: i64, after_id: i64, limit: i64) -> Result<Vec<TaskMessage>, String> {
    let limit = limit.clamp(1, 200);
    let conn = db::open_db()?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {COLS} FROM task_messages WHERE task_id = ?1 AND id > ?2 ORDER BY id LIMIT ?3"
        ))
        .map_err(|e| format!("查询失败: {e}"))?;
    let rows = stmt
        .query_map(rusqlite::params![task_id, after_id, limit], |r| {
            row_to_msg(r)
        })
        .map_err(|e| format!("查询失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取失败: {e}"))?;
    Ok(rows)
}

/// 最近 n 条(给主 Agent 拼上下文用),按时间正序返回。
pub fn recent(task_id: i64, n: i64) -> Result<Vec<TaskMessage>, String> {
    let n = n.clamp(1, 100);
    let conn = db::open_db()?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {COLS} FROM (SELECT {COLS} FROM task_messages WHERE task_id = ?1 ORDER BY id DESC LIMIT ?2) ORDER BY id"
        ))
        .map_err(|e| format!("查询失败: {e}"))?;
    let rows = stmt
        .query_map(rusqlite::params![task_id, n], |r| row_to_msg(r))
        .map_err(|e| format!("查询失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取失败: {e}"))?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(tag: &str) -> std::sync::MutexGuard<'static, ()> {
        let g = db::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::set_var(
            "POLARIS_COLLAB_DB",
            std::env::temp_dir().join(format!("chat-{tag}-{ts}.db")),
        );
        g
    }

    fn mk_task() -> i64 {
        let conn = db::open_db().unwrap();
        conn.execute("INSERT INTO projects(name,created_at) VALUES('p',0)", [])
            .unwrap();
        let pid = conn.last_insert_rowid();
        let card = tasks::create(pid, "t", "b", "src", "c1", "tester").unwrap();
        card.id
    }

    #[test]
    fn post_list_paginate_and_idem() {
        let _g = setup("basic");
        let tid = mk_task();

        // 发三条,顺序与分页。
        let m1 = post(tid, 1, "甲", "assignee", "第一条", None).unwrap();
        let _ = post(tid, 2, "乙", "lead", "第二条", None).unwrap();
        let m3 = post(tid, 0, "主Agent", "ai", "第三条", None).unwrap();
        let all = list(tid, 0, 50).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].body, "第一条");
        let tail = list(tid, m1.id, 50).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[1].id, m3.id);

        // 幂等键:同键重复投递返回同一条,不再插新行。
        let a = post(tid, 1, "甲", "assignee", "补传消息", Some("key-1")).unwrap();
        let b = post(tid, 1, "甲", "assignee", "补传消息", Some("key-1")).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(list(tid, 0, 50).unwrap().len(), 4);

        // 校验:空/超长拒收,不存在的任务拒收。
        assert!(post(tid, 1, "甲", "assignee", "  ", None).is_err());
        assert!(post(tid, 1, "甲", "assignee", &"x".repeat(BODY_MAX + 1), None).is_err());
        assert!(post(99999, 1, "甲", "assignee", "hi", None).is_err());

        // recent:取最近 2 条且正序。
        let r = recent(tid, 2).unwrap();
        assert_eq!(r.len(), 2);
        assert!(r[0].id < r[1].id);
    }
}
