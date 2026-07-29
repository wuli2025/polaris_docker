//! collab/mesh.rs —— 同账号设备网的**设备目录**(权威侧)。
//!
//! 解决的问题:此前接入一台远程主机要手工粘一串 PLRK1 连接码,N 台设备两两互连是 N² 次
//! 粘贴。Tailscale 的体验是「登录同一个账号 → 设备自己就成了网」。这张目录就是那个
//! 「自己成网」缺的一环:每台设备登录后把自己的 iroh NodeId 挂上来,同 uid 的其它设备
//! 拉一次清单就知道该连谁。
//!
//! **只有账号权威(云机)跑这张表。** 成员主机不存别人的设备,也不需要 —— 它们只是
//! 向权威报到、取清单。
//!
//! 凭据设计(这里是本模块唯一需要小心的地方):
//!  · 入网时权威颁一把**设备密钥**(32 字节随机),库里只留 sha256,明文只在颁发那一次回给设备。
//!  · 这把密钥能做两件事:报到取清单、以及**换取一张身份断言**。后者意味着它等同该账号的
//!    长期凭据 —— 所以按设备独立颁发、可按设备独立吊销,丢一台不必改密码。
//!  · 密钥**不用来直接访问别的设备**。设备之间进门仍走原来那条路:拿断言去对端换本机会话
//!    ([`super::authority::login_with_assertion`])。于是「谁能进我的机器」始终由**对端自己的
//!    成员资格**说了算,云机从不持有任何一台设备的访问权 —— 云机被拿下也开不了你家的盘。
use base64::Engine;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::db::{self, now, open_db};

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// 设备心跳周期(客户端 60s 一次)的 3 倍余量:超过这个没报到就算离线。
const ONLINE_WINDOW: i64 = 200;

#[derive(Serialize, Clone, Debug)]
pub struct Peer {
    pub node_id: String,
    pub name: String,
    pub os: String,
    pub ver: String,
    pub last_seen: i64,
    pub online: bool,
}

fn hash_key(k: &str) -> String {
    let d = Sha256::digest(k.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn new_key() -> Result<String, String> {
    let mut buf = [0u8; 32];
    getrandom::getrandom(&mut buf).map_err(|e| format!("生成设备密钥失败: {e}"))?;
    Ok(format!("mk_{}", B64.encode(buf)))
}

fn row_to_peer(
    r: &rusqlite::Row,
    t: i64,
) -> rusqlite::Result<Peer> {
    let last_seen: i64 = r.get(4)?;
    Ok(Peer {
        node_id: r.get(0)?,
        name: r.get(1)?,
        os: r.get(2)?,
        ver: r.get(3)?,
        last_seen,
        online: t - last_seen <= ONLINE_WINDOW,
    })
}

/// 设备入网:登记 NodeId 并颁一把新的设备密钥(返回明文,只此一次)。
///
/// 同 node_id 再来 = **换一把新密钥**(重装、重新登录都走这条)。这是有意的幂等口径:
/// 一台机器的身份是它的 NodeId,不是密钥;重登不该在目录里堆出第二个「同一台机器」。
/// 顺带把 revoked 清零 —— 用户亲自重新登录进来,就是撤回了那次吊销。
pub fn enroll(uid: &str, node_id: &str, name: &str, os: &str, ver: &str) -> Result<String, String> {
    let node_id = node_id.trim();
    if node_id.is_empty() {
        return Err("设备缺 NodeId(本机 iroh 还没就绪,稍后重试)".into());
    }
    if uid.trim().is_empty() {
        return Err("缺账号 uid".into());
    }
    let key = new_key()?;
    let conn = open_db()?;
    conn.execute(
        "INSERT INTO mesh_nodes(node_id,uid,name,os,ver,key_hash,created_at,last_seen,revoked) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?7,0) \
         ON CONFLICT(node_id) DO UPDATE SET \
           uid=excluded.uid, name=excluded.name, os=excluded.os, ver=excluded.ver, \
           key_hash=excluded.key_hash, last_seen=excluded.last_seen, revoked=0",
        params![node_id, uid.trim(), name.trim(), os.trim(), ver.trim(), hash_key(&key), now()],
    )
    .map_err(|e| format!("设备入网失败: {e}"))?;
    db::audit(uid, "mesh.enroll", node_id, name);
    Ok(key)
}

/// 设备密钥 → (uid, node_id)。吊销的、不认识的一律拒。
pub fn resolve_key(key: &str) -> Result<(String, String), String> {
    let conn = open_db()?;
    let row: Option<(String, String, i64)> = conn
        .query_row(
            "SELECT uid,node_id,revoked FROM mesh_nodes WHERE key_hash=?1",
            params![hash_key(key.trim())],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| format!("查设备失败: {e}"))?;
    match row {
        Some((_, _, revoked)) if revoked != 0 => {
            Err("这台设备已被移出设备网 —— 重新登录账号即可重新入网".into())
        }
        Some((uid, node_id, _)) => Ok((uid, node_id)),
        None => Err("设备密钥无效 —— 请重新登录账号入网".into()),
    }
}

/// 同一账号名下的其它设备(不含自己)。吊销的不出现在清单里。
pub fn peers(uid: &str, exclude_node: &str) -> Result<Vec<Peer>, String> {
    let t = now();
    let conn = open_db()?;
    let mut st = conn
        .prepare(
            "SELECT node_id,name,os,ver,last_seen FROM mesh_nodes \
             WHERE uid=?1 AND revoked=0 AND node_id<>?2 ORDER BY last_seen DESC",
        )
        .map_err(|e| format!("查设备清单失败: {e}"))?;
    let rows = st
        .query_map(params![uid, exclude_node], |r| row_to_peer(r, t))
        .map_err(|e| format!("查设备清单失败: {e}"))?;
    Ok(rows.flatten().collect())
}

/// 心跳 + 取清单(一次往返办两件事:设备网的轮询频率不低,能省一趟就省一趟)。
pub fn announce(key: &str, name: &str, os: &str, ver: &str) -> Result<(String, Vec<Peer>), String> {
    let (uid, node_id) = resolve_key(key)?;
    let conn = open_db()?;
    // 名字/版本每次报到都跟着更新:改了机器名、升了版本,清单里立刻是新的。
    conn.execute(
        "UPDATE mesh_nodes SET last_seen=?1, \
           name=CASE WHEN ?2<>'' THEN ?2 ELSE name END, \
           os=CASE WHEN ?3<>'' THEN ?3 ELSE os END, \
           ver=CASE WHEN ?4<>'' THEN ?4 ELSE ver END \
         WHERE node_id=?5",
        params![now(), name.trim(), os.trim(), ver.trim(), node_id],
    )
    .map_err(|e| format!("设备报到失败: {e}"))?;
    let list = peers(&uid, &node_id)?;
    Ok((uid, list))
}

/// 把自己账号名下的一台设备移出设备网(丢了电脑的应急操作)。
/// 只能踢**同 uid** 的设备 —— 拿着自己的密钥踢不掉别人账号的机器。
pub fn revoke(uid: &str, node_id: &str) -> Result<(), String> {
    let conn = open_db()?;
    let n = conn
        .execute(
            "UPDATE mesh_nodes SET revoked=1 WHERE node_id=?1 AND uid=?2",
            params![node_id.trim(), uid],
        )
        .map_err(|e| format!("移出设备网失败: {e}"))?;
    if n == 0 {
        return Err("这台设备不在你账号的设备网里".into());
    }
    db::audit(uid, "mesh.revoke", node_id, "");
    Ok(())
}

/// uid → (username, email, display_name)。给设备换取身份断言时填断言载荷用。
pub fn account_of(uid: &str) -> Result<(String, String, String), String> {
    let conn = open_db()?;
    conn.query_row(
        "SELECT username,email,display_name,disabled FROM users WHERE uid=?1",
        params![uid],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        },
    )
    .optional()
    .map_err(|e| format!("查账号失败: {e}"))?
    .ok_or_else(|| "这个账号在账号中心已不存在".to_string())
    .and_then(|(u, e, d, disabled)| {
        if disabled != 0 {
            Err("账号已停用".into())
        } else {
            Ok((u, e, d))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db(tag: &str) -> std::sync::MutexGuard<'static, ()> {
        let g = db::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = std::env::temp_dir().join(format!("mesh-test-{}-{tag}.db", std::process::id()));
        let _ = std::fs::remove_file(&p);
        std::env::set_var("POLARIS_COLLAB_DB", &p);
        g
    }

    /// 入网 → 报到 → 互见:两台设备同一个 uid,各自只看见对方。
    #[test]
    fn two_devices_of_one_account_see_each_other() {
        let _g = tmp_db("pair");
        let k1 = enroll("acct_1", "node-aaa", "台式机", "windows", "2.6.0").unwrap();
        let k2 = enroll("acct_1", "node-bbb", "NAS", "linux", "2.6.0").unwrap();
        assert_ne!(k1, k2, "每台设备一把独立密钥");

        let (uid, p1) = announce(&k1, "台式机", "windows", "2.6.0").unwrap();
        assert_eq!(uid, "acct_1");
        assert_eq!(p1.len(), 1, "只该看见对方一台");
        assert_eq!(p1[0].node_id, "node-bbb");
        assert!(p1[0].online, "刚报到过必须算在线");

        let (_, p2) = announce(&k2, "NAS", "linux", "2.6.0").unwrap();
        assert_eq!(p2[0].node_id, "node-aaa");
    }

    /// 账号隔离:别人账号的设备绝不出现在我的清单里。
    #[test]
    fn other_accounts_are_invisible() {
        let _g = tmp_db("iso");
        let mine = enroll("acct_me", "node-mine", "我的", "windows", "1").unwrap();
        enroll("acct_other", "node-other", "别人的", "linux", "1").unwrap();
        let (_, list) = announce(&mine, "", "", "").unwrap();
        assert!(list.is_empty(), "看见了不属于本账号的设备:{list:?}");
    }

    /// 吊销即失效:密钥不能再报到;本人重新登录(再 enroll)可自助恢复。
    #[test]
    fn revoke_then_reenroll() {
        let _g = tmp_db("revoke");
        let k = enroll("acct_1", "node-x", "旧机", "windows", "1").unwrap();
        enroll("acct_1", "node-y", "另一台", "windows", "1").unwrap();

        revoke("acct_1", "node-x").unwrap();
        assert!(announce(&k, "", "", "").is_err(), "吊销后不该还能报到");
        // 别人的账号踢不动我的设备。
        assert!(revoke("acct_other", "node-y").is_err());

        // 重新入网:拿到新密钥,旧密钥永久作废。
        let k2 = enroll("acct_1", "node-x", "旧机", "windows", "1").unwrap();
        assert!(announce(&k2, "", "", "").is_ok());
        assert!(announce(&k, "", "", "").is_err(), "旧密钥必须仍然无效");
    }

    /// 重复入网不制造重影:同一台机器登两次,目录里仍是一台。
    #[test]
    fn reenroll_is_idempotent_on_node_id() {
        let _g = tmp_db("dup");
        enroll("acct_1", "node-a", "机器", "windows", "1").unwrap();
        let k = enroll("acct_1", "node-a", "机器改了名", "windows", "2").unwrap();
        enroll("acct_1", "node-b", "另一台", "linux", "1").unwrap();
        let (_, list) = announce(&k, "", "", "").unwrap();
        assert_eq!(list.len(), 1, "同 node_id 重复入网不该多出一台:{list:?}");
    }

    #[test]
    fn bad_key_is_refused() {
        let _g = tmp_db("badkey");
        enroll("acct_1", "node-a", "机器", "windows", "1").unwrap();
        assert!(resolve_key("mk_totally-made-up").is_err());
        assert!(announce("", "", "", "").is_err());
    }
}
