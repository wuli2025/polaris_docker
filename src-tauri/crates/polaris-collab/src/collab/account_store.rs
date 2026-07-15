//! collab/account_store.rs —— 账号镜像「本地权威 + 云端密文兜底」(v8 用户要求)。
//!
//! 拓扑:主机(家里/NAS)是账号权威;云端 Docker 服务器存一份**加密镜像**,以防主机
//! 握手失败/失联时账号资料丢失。零知识:加密钥从 host.key 派生、永不出主机,
//! 云端只保管密文 blob——被黑也解不开一个字节。恢复动作只能在持有 host.key 的机器上做。
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rusqlite::params;
use sha2::{Digest, Sha256};

use super::db::{self, now, open_db};
use super::identity;

/// 镜像加密钥:sha256(host.key 种子 || 域分隔标签)。
fn mirror_key() -> Result<[u8; 32], String> {
    let seed = identity::get_or_create_host_key()?;
    let mut h = Sha256::new();
    h.update(seed);
    h.update(b"polaris-account-mirror-v1");
    Ok(h.finalize().into())
}

/// 导出账号镜像:users + devices + tickets 全量 → JSON → ChaCha20-Poly1305 密文。
/// 返回 base64url(nonce || ciphertext),并同步写进本地 cloud_mirror 表(版本+1)。
pub fn export_blob() -> Result<String, String> {
    let conn = open_db()?;
    let dump_table = |sql: &str| -> Result<Vec<serde_json::Value>, String> {
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let ncols = stmt.column_count();
        let names: Vec<String> = (0..ncols)
            .map(|i| stmt.column_name(i).unwrap_or("").to_string())
            .collect();
        let rows = stmt
            .query_map([], |r| {
                let mut obj = serde_json::Map::new();
                for (i, name) in names.iter().enumerate() {
                    let v: rusqlite::types::Value = r.get(i)?;
                    let jv = match v {
                        rusqlite::types::Value::Null => serde_json::Value::Null,
                        rusqlite::types::Value::Integer(n) => serde_json::json!(n),
                        rusqlite::types::Value::Real(f) => serde_json::json!(f),
                        rusqlite::types::Value::Text(s) => serde_json::json!(s),
                        rusqlite::types::Value::Blob(b) => {
                            serde_json::json!(base64::engine::general_purpose::STANDARD.encode(b))
                        }
                    };
                    obj.insert(name.clone(), jv);
                }
                Ok(serde_json::Value::Object(obj))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    };
    let payload = serde_json::json!({
        "v": 1,
        "exported_at": now(),
        "users": dump_table("SELECT * FROM users")?,
        "devices": dump_table("SELECT * FROM devices")?,
        "tickets": dump_table("SELECT * FROM tickets")?,
    });
    let plain = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;

    let key = mirror_key()?;
    let cipher = ChaCha20Poly1305::new((&key).into());
    let mut nonce_bytes = [0u8; 12];
    getrandom::getrandom(&mut nonce_bytes).map_err(|e| e.to_string())?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, plain.as_ref())
        .map_err(|e| format!("加密失败: {e}"))?;
    let mut blob = nonce_bytes.to_vec();
    blob.extend_from_slice(&ct);

    // 本地留底(cloud_mirror 表:云端存的就是这份密文)。
    let version: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version),0)+1 FROM cloud_mirror",
            [],
            |r| r.get(0),
        )
        .unwrap_or(1);
    conn.execute(
        "INSERT OR REPLACE INTO cloud_mirror(id,version,updated_at,blob) VALUES(1,?1,?2,?3)",
        params![version, now(), blob],
    )
    .map_err(|e| e.to_string())?;
    db::audit("host", "mirror.export", &version.to_string(), "");
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(blob))
}

/// 云端侧:保管密文(不解密,解不开)。
pub fn store_remote_blob(blob_b64: &str) -> Result<i64, String> {
    let blob = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(blob_b64.trim())
        .map_err(|e| format!("blob 格式错误: {e}"))?;
    if blob.len() < 13 {
        return Err("blob 太短,不是有效镜像".into());
    }
    let conn = open_db()?;
    let version: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version),0)+1 FROM cloud_mirror",
            [],
            |r| r.get(0),
        )
        .unwrap_or(1);
    conn.execute(
        "INSERT OR REPLACE INTO cloud_mirror(id,version,updated_at,blob) VALUES(1,?1,?2,?3)",
        params![version, now(), blob],
    )
    .map_err(|e| e.to_string())?;
    db::audit("cloud", "mirror.store", &version.to_string(), "");
    Ok(version)
}

/// 取出本地保管的密文(主机推云端、或从云端拉回后本地读取都走它)。
pub fn load_blob() -> Result<Option<(i64, String)>, String> {
    let conn = open_db()?;
    let row = conn.query_row(
        "SELECT version, blob FROM cloud_mirror WHERE id=1",
        [],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)),
    );
    match row {
        Ok((v, blob)) => Ok(Some((
            v,
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(blob),
        ))),
        Err(_) => Ok(None),
    }
}

/// 恢复:解密镜像(需要 host.key)→ 在**账号表为空**的库上重建 users/devices。
/// 非空库拒绝恢复(防误覆盖);tickets 不恢复(一次性凭据,过期即弃)。
pub fn restore_blob(blob_b64: &str) -> Result<usize, String> {
    let blob = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(blob_b64.trim())
        .map_err(|e| format!("blob 格式错误: {e}"))?;
    if blob.len() < 13 {
        return Err("blob 太短".into());
    }
    let key = mirror_key()?;
    let cipher = ChaCha20Poly1305::new((&key).into());
    let plain = cipher
        .decrypt(Nonce::from_slice(&blob[..12]), &blob[12..])
        .map_err(|_| "解密失败:host.key 不匹配(镜像只能用原主机身份恢复)".to_string())?;
    let payload: serde_json::Value = serde_json::from_slice(&plain).map_err(|e| e.to_string())?;

    let conn = open_db()?;
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if n > 0 {
        return Err("当前库已有账号,拒绝覆盖式恢复(如确需,请先备份并清空 users)".into());
    }
    let mut restored = 0usize;
    for u in payload
        .get("users")
        .and_then(|v| v.as_array())
        .unwrap_or(&vec![])
    {
        conn.execute(
            "INSERT INTO users(id,username,pass_hash,role,display_name,created_at,disabled) \
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                u.get("id").and_then(|x| x.as_i64()),
                u.get("username").and_then(|x| x.as_str()).unwrap_or(""),
                u.get("pass_hash").and_then(|x| x.as_str()).unwrap_or(""),
                u.get("role")
                    .and_then(|x| x.as_str())
                    .unwrap_or("collaborator"),
                u.get("display_name").and_then(|x| x.as_str()).unwrap_or(""),
                u.get("created_at").and_then(|x| x.as_i64()).unwrap_or(0),
                u.get("disabled").and_then(|x| x.as_i64()).unwrap_or(0),
            ],
        )
        .map_err(|e| format!("恢复用户失败: {e}"))?;
        restored += 1;
    }
    for d in payload
        .get("devices")
        .and_then(|v| v.as_array())
        .unwrap_or(&vec![])
    {
        let _ = conn.execute(
            "INSERT OR IGNORE INTO devices(id,user_id,name,node_id,pubkey_fp,added_at,revoked) \
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                d.get("id").and_then(|x| x.as_str()).unwrap_or(""),
                d.get("user_id").and_then(|x| x.as_i64()).unwrap_or(0),
                d.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                d.get("node_id").and_then(|x| x.as_str()).unwrap_or(""),
                d.get("pubkey_fp").and_then(|x| x.as_str()).unwrap_or(""),
                d.get("added_at").and_then(|x| x.as_i64()).unwrap_or(0),
                d.get("revoked").and_then(|x| x.as_i64()).unwrap_or(0),
            ],
        );
    }
    db::audit("host", "mirror.restore", &restored.to_string(), "");
    Ok(restored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_restore_roundtrip() {
        let _g = super::super::db::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::set_var("POLARIS_COLLAB_DB", dir.join(format!("mirror-a-{ts}.db")));
        std::env::set_var("POLARIS_HOST_KEY", dir.join(format!("mirror-key-{ts}")));

        super::super::auth::create_user("alice", "s3cret-9", "owner", "Alice").unwrap();
        super::super::identity::add_device(1, "本机", "node-abc").unwrap();
        let blob = export_blob().unwrap();

        // 换一个空库(同一把 host.key)→ 恢复成功,登录凭原密码可用。
        std::env::set_var("POLARIS_COLLAB_DB", dir.join(format!("mirror-b-{ts}.db")));
        let n = restore_blob(&blob).unwrap();
        assert_eq!(n, 1);
        let (u, _tok) = super::super::auth::login("alice", "s3cret-9", "dev").unwrap();
        assert_eq!(u.role, "owner");
        // 设备白名单也回来了
        assert!(super::super::identity::is_node_allowed("node-abc"));
        // 非空库拒绝再次恢复
        assert!(restore_blob(&blob).is_err());

        // 换 host.key → 解不开(零知识)
        std::env::set_var("POLARIS_COLLAB_DB", dir.join(format!("mirror-c-{ts}.db")));
        std::env::set_var("POLARIS_HOST_KEY", dir.join(format!("mirror-key2-{ts}")));
        assert!(restore_blob(&blob).unwrap_err().contains("解密失败"));
    }

    #[test]
    fn remote_store_is_opaque() {
        let _g = super::super::db::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::set_var("POLARIS_COLLAB_DB", dir.join(format!("mirror-r-{ts}.db")));
        std::env::set_var("POLARIS_HOST_KEY", dir.join(format!("mirror-rk-{ts}")));
        let v1 =
            store_remote_blob(&base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([9u8; 40]))
                .unwrap();
        let v2 =
            store_remote_blob(&base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7u8; 40]))
                .unwrap();
        assert!(v2 > v1);
        assert!(load_blob().unwrap().is_some());
        assert!(store_remote_blob("xx").is_err());
    }
}
