//! collab/projects.rs —— 项目与成员关系(授权表的"资源侧")。
//!
//! 权限判定的确定性入口:is_member / member_role。主 Agent 的每个指挥工具
//! 执行前都要过这里(三问预过滤之一:目标是不是本项目资源)。
use rusqlite::params;

use super::db::{self, now, open_db};

#[derive(serde::Serialize, Clone, Debug)]
pub struct Project {
    pub id: i64,
    pub name: String,
    pub repo: String,
    pub lead_expert_id: Option<String>,
    pub charter_path: String,
    pub created_at: i64,
    pub archived: bool,
    /// 所属团队(GitHub org→repo 式)。None=独立项目(仅显式成员可见)。
    pub team_id: Option<i64>,
    /// 进行中任务数(pending+in_progress)——侧栏徽章,GitHub repo 列表式。list_for 填,其余场景 0。
    pub open_count: i64,
    /// 待验收任务数(review)——同上。
    pub review_count: i64,
    /// 管理者放行的全项目共享可见路径(CSV)。协作者开工时并入稀疏集,人人可见这些目录。
    pub shared_scope: String,
}

fn row_to_project(r: &rusqlite::Row) -> rusqlite::Result<Project> {
    Ok(Project {
        id: r.get(0)?,
        name: r.get(1)?,
        repo: r.get(2)?,
        lead_expert_id: r.get(3)?,
        charter_path: r.get(4)?,
        created_at: r.get(5)?,
        archived: r.get::<_, i64>(6)? != 0,
        team_id: r.get(7)?,
        open_count: 0,
        review_count: 0,
        shared_scope: r.get(8)?,
    })
}

const COLS: &str =
    "id,name,repo,lead_expert_id,charter_path,created_at,archived,team_id,shared_scope";

/// 设共享可见路径(CSV)。can_admin 校验在调用侧(http 层)。
pub fn set_shared_scope(project_id: i64, shared_scope: &str, actor: &str) -> Result<(), String> {
    let conn = open_db()?;
    conn.execute(
        "UPDATE projects SET shared_scope=?1 WHERE id=?2",
        params![shared_scope.trim(), project_id],
    )
    .map_err(|e| e.to_string())?;
    db::audit(
        actor,
        "project.shared_scope",
        &project_id.to_string(),
        shared_scope.trim(),
    );
    Ok(())
}

pub fn create(
    name: &str,
    repo: &str,
    team_id: Option<i64>,
    owner_user_id: i64,
    actor: &str,
) -> Result<Project, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("项目名不能为空".into());
    }
    let conn = open_db()?;
    conn.execute(
        "INSERT INTO projects(name,repo,team_id,created_at) VALUES(?1,?2,?3,?4)",
        params![name, repo.trim(), team_id, now()],
    )
    .map_err(|e| format!("建项目失败: {e}"))?;
    let id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO project_members(project_id,user_id,role) VALUES(?1,?2,'owner')",
        params![id, owner_user_id],
    )
    .map_err(|e| e.to_string())?;
    db::audit(actor, "project.create", &id.to_string(), name);
    get(id)
}

pub fn get(id: i64) -> Result<Project, String> {
    let conn = open_db()?;
    conn.query_row(
        &format!("SELECT {COLS} FROM projects WHERE id=?1"),
        params![id],
        row_to_project,
    )
    .map_err(|_| format!("项目 #{id} 不存在"))
}

/// 列出某用户可见的项目(owner 角色看全部;普通用户=显式成员 ∪ 所在团队的项目)。
pub fn list_for(user_id: i64, is_owner: bool) -> Result<Vec<Project>, String> {
    let conn = open_db()?;
    let sql = if is_owner {
        format!("SELECT {COLS} FROM projects WHERE archived=0 ORDER BY id DESC")
    } else {
        format!(
            "SELECT {COLS} FROM projects WHERE archived=0 AND (id IN \
             (SELECT project_id FROM project_members WHERE user_id=?1) \
             OR team_id IN (SELECT team_id FROM team_members WHERE user_id=?1)) ORDER BY id DESC"
        )
    };
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = if is_owner {
        stmt.query_map([], row_to_project)
    } else {
        stmt.query_map(params![user_id], row_to_project)
    }
    .map_err(|e| e.to_string())?;
    let mut list: Vec<Project> = rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?;
    // 侧栏徽章:进行中/待验收计数。项目数量级小,逐个聚合查询可接受。
    let mut cnt = conn
        .prepare(
            "SELECT
               SUM(CASE WHEN state IN ('pending','in_progress') THEN 1 ELSE 0 END),
               SUM(CASE WHEN state='review' THEN 1 ELSE 0 END)
             FROM tasks WHERE project_id=?1",
        )
        .map_err(|e| e.to_string())?;
    for p in &mut list {
        if let Ok((o, r)) = cnt.query_row(params![p.id], |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                r.get::<_, Option<i64>>(1)?.unwrap_or(0),
            ))
        }) {
            p.open_count = o;
            p.review_count = r;
        }
    }
    Ok(list)
}

pub fn add_member(project_id: i64, user_id: i64, role: &str, actor: &str) -> Result<(), String> {
    let conn = open_db()?;
    conn.execute(
        "INSERT OR REPLACE INTO project_members(project_id,user_id,role) VALUES(?1,?2,?3)",
        params![project_id, user_id, role],
    )
    .map_err(|e| e.to_string())?;
    db::audit(
        actor,
        "project.member.add",
        &format!("{project_id}/{user_id}"),
        role,
    );
    Ok(())
}

pub fn remove_member(project_id: i64, user_id: i64, actor: &str) -> Result<(), String> {
    let conn = open_db()?;
    conn.execute(
        "DELETE FROM project_members WHERE project_id=?1 AND user_id=?2",
        params![project_id, user_id],
    )
    .map_err(|e| e.to_string())?;
    db::audit(
        actor,
        "project.member.remove",
        &format!("{project_id}/{user_id}"),
        "",
    );
    Ok(())
}

/// 成员在项目内的角色。None = 非成员(拒绝访问的确定性依据)。
/// 判定顺序:显式项目成员 → 所属团队成员(团队 owner 映射为项目 owner,member 映射为协作者)。
pub fn member_role(project_id: i64, user_id: i64) -> Option<String> {
    let conn = open_db().ok()?;
    if let Ok(r) = conn.query_row(
        "SELECT role FROM project_members WHERE project_id=?1 AND user_id=?2",
        params![project_id, user_id],
        |r| r.get::<_, String>(0),
    ) {
        return Some(r);
    }
    conn.query_row(
        "SELECT m.role FROM team_members m JOIN projects p ON p.team_id=m.team_id \
         WHERE p.id=?1 AND m.user_id=?2",
        params![project_id, user_id],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .map(|team_role| {
        if team_role == "owner" {
            "owner".into()
        } else {
            "collaborator".into()
        }
    })
}

/// 项目管理权(验收/放行/改设置):全局 owner、项目 owner、或所属团队 owner。
pub fn can_admin(project_id: i64, user_id: i64, global_owner: bool) -> bool {
    global_owner || member_role(project_id, user_id).as_deref() == Some("owner")
}

/// 按用户名把人加进项目(GitHub 式)。返回 user_id。
pub fn add_member_by_username(
    project_id: i64,
    username: &str,
    role: &str,
    actor: &str,
) -> Result<i64, String> {
    let conn = open_db()?;
    let uid: i64 = conn
        .query_row(
            "SELECT id FROM users WHERE username=?1 AND disabled=0",
            params![username.trim()],
            |r| r.get(0),
        )
        .map_err(|_| format!("找不到用户「{}」(需对方先注册)", username.trim()))?;
    add_member(project_id, uid, role, actor)?;
    Ok(uid)
}

#[derive(serde::Serialize)]
pub struct Member {
    pub user_id: i64,
    pub username: String,
    pub display_name: String,
    pub role: String,
}

pub fn members(project_id: i64) -> Result<Vec<Member>, String> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare(
            "SELECT m.user_id,u.username,u.display_name,m.role FROM project_members m \
             JOIN users u ON u.id=m.user_id WHERE m.project_id=?1 ORDER BY m.role DESC, u.username",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![project_id], |r| {
            Ok(Member {
                user_id: r.get(0)?,
                username: r.get(1)?,
                display_name: r.get(2)?,
                role: r.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// 设定项目的主 Agent 人格(None=纯人工)。owner 专属操作,调用侧把关。
pub fn set_lead(project_id: i64, expert_id: Option<&str>, actor: &str) -> Result<(), String> {
    let conn = open_db()?;
    conn.execute(
        "UPDATE projects SET lead_expert_id=?1 WHERE id=?2",
        params![expert_id, project_id],
    )
    .map_err(|e| e.to_string())?;
    db::audit(
        actor,
        "project.lead.set",
        &project_id.to_string(),
        expert_id.unwrap_or("(纯人工)"),
    );
    Ok(())
}
