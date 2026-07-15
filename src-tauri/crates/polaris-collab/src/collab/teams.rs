//! collab/teams.rs —— 团队(GitHub org 式协作形态)。
//!
//! 形态对标 GitHub:注册只要账号密码 → 建团队/被搜索用户名拉进团队 → 团队下挂项目,
//! 团队成员自动可见团队内所有项目。一人可在多个团队,不同团队各有各的项目。
//! 权限仍是确定性判定:团队 owner 才能拉人/踢人/建项目;全局 owner(主机管理员)全通。
use rusqlite::params;

use super::db::{self, now, open_db};

#[derive(serde::Serialize, Clone, Debug)]
pub struct Team {
    pub id: i64,
    pub name: String,
    pub created_at: i64,
    /// 当前用户在该团队的角色(owner|member),列表接口填充。
    pub my_role: String,
    pub member_count: i64,
}

/// 建团队:创建者自动成为团队 owner。任何登录用户可建(GitHub 式)。
pub fn create(name: &str, creator_user_id: i64, actor: &str) -> Result<Team, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("团队名不能为空".into());
    }
    let conn = open_db()?;
    conn.execute(
        "INSERT INTO teams(name,created_at) VALUES(?1,?2)",
        params![name, now()],
    )
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            "团队名已存在".to_string()
        } else {
            format!("建团队失败: {e}")
        }
    })?;
    let id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO team_members(team_id,user_id,role) VALUES(?1,?2,'owner')",
        params![id, creator_user_id],
    )
    .map_err(|e| e.to_string())?;
    db::audit(actor, "team.create", &id.to_string(), name);
    Ok(Team {
        id,
        name: name.into(),
        created_at: now(),
        my_role: "owner".into(),
        member_count: 1,
    })
}

/// 我所在的团队列表(带我的角色与人数)。
pub fn list_mine(user_id: i64) -> Result<Vec<Team>, String> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare(
            "SELECT t.id,t.name,t.created_at,m.role,\
             (SELECT COUNT(*) FROM team_members WHERE team_id=t.id) \
             FROM teams t JOIN team_members m ON m.team_id=t.id \
             WHERE m.user_id=?1 AND t.archived=0 ORDER BY t.id DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![user_id], |r| {
            Ok(Team {
                id: r.get(0)?,
                name: r.get(1)?,
                created_at: r.get(2)?,
                my_role: r.get(3)?,
                member_count: r.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// 我在团队里的角色。None=非成员。
pub fn my_role(team_id: i64, user_id: i64) -> Option<String> {
    let conn = open_db().ok()?;
    conn.query_row(
        "SELECT role FROM team_members WHERE team_id=?1 AND user_id=?2",
        params![team_id, user_id],
        |r| r.get(0),
    )
    .ok()
}

#[derive(serde::Serialize)]
pub struct TeamMember {
    pub user_id: i64,
    pub username: String,
    pub display_name: String,
    pub role: String,
}

pub fn members(team_id: i64) -> Result<Vec<TeamMember>, String> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare(
            "SELECT m.user_id,u.username,u.display_name,m.role FROM team_members m \
             JOIN users u ON u.id=m.user_id WHERE m.team_id=?1 ORDER BY m.role DESC,u.username",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![team_id], |r| {
            Ok(TeamMember {
                user_id: r.get(0)?,
                username: r.get(1)?,
                display_name: r.get(2)?,
                role: r.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// 按用户名拉人进团队(GitHub 式邀请)。返回被拉用户名。
pub fn add_member_by_username(
    team_id: i64,
    username: &str,
    role: &str,
    actor: &str,
) -> Result<String, String> {
    let conn = open_db()?;
    let uid: i64 = conn
        .query_row(
            "SELECT id FROM users WHERE username=?1 AND disabled=0",
            params![username.trim()],
            |r| r.get(0),
        )
        .map_err(|_| format!("找不到用户「{}」(需对方先注册)", username.trim()))?;
    let role = if role == "owner" { "owner" } else { "member" };
    conn.execute(
        "INSERT OR REPLACE INTO team_members(team_id,user_id,role) VALUES(?1,?2,?3)",
        params![team_id, uid, role],
    )
    .map_err(|e| e.to_string())?;
    db::audit(
        actor,
        "team.member.add",
        &format!("{team_id}/{username}"),
        role,
    );
    Ok(username.trim().to_string())
}

pub fn remove_member(team_id: i64, user_id: i64, actor: &str) -> Result<(), String> {
    let conn = open_db()?;
    // 防呆:不许移除最后一个 owner(团队会失控)。
    let owners: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM team_members WHERE team_id=?1 AND role='owner'",
            params![team_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let is_owner = my_role(team_id, user_id).as_deref() == Some("owner");
    if is_owner && owners <= 1 {
        return Err("不能移除团队最后一位管理者".into());
    }
    conn.execute(
        "DELETE FROM team_members WHERE team_id=?1 AND user_id=?2",
        params![team_id, user_id],
    )
    .map_err(|e| e.to_string())?;
    db::audit(
        actor,
        "team.member.remove",
        &format!("{team_id}/{user_id}"),
        "",
    );
    Ok(())
}

/// 用户名搜索(拉人自动补全用)。只回不敏感字段,限 20 条。
#[derive(serde::Serialize)]
pub struct UserHit {
    pub id: i64,
    pub username: String,
    pub display_name: String,
}

pub fn search_users(q: &str) -> Result<Vec<UserHit>, String> {
    let q = q.trim();
    if q.is_empty() {
        return Ok(vec![]);
    }
    let conn = open_db()?;
    let pat = format!("%{}%", q.replace('%', "").replace('_', ""));
    let mut stmt = conn
        .prepare(
            "SELECT id,username,display_name FROM users \
             WHERE disabled=0 AND (username LIKE ?1 OR display_name LIKE ?1) \
             ORDER BY username LIMIT 20",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![pat], |r| {
            Ok(UserHit {
                id: r.get(0)?,
                username: r.get(1)?,
                display_name: r.get(2)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (std::sync::MutexGuard<'static, ()>, i64, i64) {
        let g = super::super::db::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::set_var(
            "POLARIS_COLLAB_DB",
            std::env::temp_dir().join(format!("teams-{ts}.db")),
        );
        let conn = open_db().unwrap();
        conn.execute(
            "INSERT INTO users(username,pass_hash,created_at) VALUES('alice','x',0)",
            [],
        )
        .unwrap();
        let a = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO users(username,pass_hash,created_at) VALUES('bob','x',0)",
            [],
        )
        .unwrap();
        let b = conn.last_insert_rowid();
        (g, a, b)
    }

    #[test]
    fn github_style_flow() {
        let (_g, alice, bob) = setup();
        // alice 建团队,自动 owner
        let t = create("剧本组", alice, "alice").unwrap();
        assert_eq!(t.my_role, "owner");
        // 按用户名拉 bob
        add_member_by_username(t.id, "bob", "member", "alice").unwrap();
        assert_eq!(members(t.id).unwrap().len(), 2);
        assert_eq!(my_role(t.id, bob).as_deref(), Some("member"));
        // bob 的团队列表能看到
        let mine = list_mine(bob).unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].member_count, 2);
        // 拉不存在的用户报清晰错误
        assert!(add_member_by_username(t.id, "ghost", "member", "alice")
            .unwrap_err()
            .contains("找不到用户"));
        // 团队名唯一
        assert!(create("剧本组", bob, "bob").is_err());
        // 搜索
        let hits = search_users("bo").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].username, "bob");
        // 最后一个 owner 不可移除
        assert!(remove_member(t.id, alice, "x").is_err());
        remove_member(t.id, bob, "alice").unwrap();
        assert_eq!(members(t.id).unwrap().len(), 1);
    }
}
