//! 远程盘共享设置(主机侧):把本机哪些目录开放给互联对端访问,以及**哪些允许写**。
//!
//! 真正的路径关押、列目录、读写字节在 `polaris_collab::collab::fsface`;这里只是薄薄的
//! tauri/apihub 命令壳,把 UI 的「共享目录清单」读写到 collab.db。fsface 只在 collab-host
//! 下编译,故非 host 构建给出退化实现(读空、写报错),保证任意特性组合都能编过。
//!
//! 两套命令并存,不是重复:
//!  · `fs_share_get`/`fs_share_set` —— 只认路径的老口径,手机壳与老前端在用,**语义是只读**;
//!  · `fs_share_list`/`fs_share_save` —— 带 `write` 位的新口径,桌面互联页的「点选放开写」走它。
//!  老命令保持「设进去的都是只读」这一原状 —— 老前端点一下不该悄悄把盘变成可写。

use serde_json::{json, Value};

/// 读当前共享出去的目录清单(绝对路径)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fs_share_get() -> Vec<String> {
    #[cfg(feature = "collab-host")]
    {
        return crate::collab::fsface::shared_roots();
    }
    #[cfg(not(feature = "collab-host"))]
    {
        Vec::new()
    }
}

/// 整体覆盖共享目录清单(全部按只读)。每条须是已存在目录,否则整体拒绝;空清单=停止共享。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fs_share_set(paths: Vec<String>) -> Result<Vec<String>, String> {
    #[cfg(feature = "collab-host")]
    {
        return crate::collab::fsface::set_shared_roots(&paths);
    }
    #[cfg(not(feature = "collab-host"))]
    {
        let _ = paths;
        Err("此构建未启用主机功能(collab-host),无法共享盘".into())
    }
}

/// 读共享清单(带写位)。`[{ path, write }]`。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fs_share_list() -> Vec<Value> {
    #[cfg(feature = "collab-host")]
    {
        return crate::collab::fsface::shared_entries()
            .into_iter()
            .map(|e| json!({ "path": e.path, "write": e.write }))
            .collect();
    }
    #[cfg(not(feature = "collab-host"))]
    {
        Vec::new()
    }
}

/// 整体覆盖共享清单(带写位)。缺 `write` 字段一律当只读 —— 默认关是这条线的安全底线。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fs_share_save(items: Vec<Value>) -> Result<Vec<Value>, String> {
    #[cfg(feature = "collab-host")]
    {
        let parsed: Vec<crate::collab::fsface::ShareRoot> = items
            .iter()
            .filter_map(|v| {
                let path = v.get("path")?.as_str()?.to_string();
                if path.trim().is_empty() {
                    return None;
                }
                Some(crate::collab::fsface::ShareRoot {
                    path,
                    write: v.get("write").and_then(|x| x.as_bool()).unwrap_or(false),
                })
            })
            .collect();
        return Ok(crate::collab::fsface::set_shared_entries(&parsed)?
            .into_iter()
            .map(|e| json!({ "path": e.path, "write": e.write }))
            .collect());
    }
    #[cfg(not(feature = "collab-host"))]
    {
        let _ = items;
        Err("此构建未启用主机功能(collab-host),无法共享盘".into())
    }
}
