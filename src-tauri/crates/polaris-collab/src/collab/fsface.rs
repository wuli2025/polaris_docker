//! collab/fsface.rs —— 路径关押的远程文件浏览(隧道另一端的「盘」)。
//!
//! 只在 collab-host 下编译(桌面 hosting 与 Docker server 壳都能挂,故不绑 collab-net)。
//! 根白名单来自 POLARIS_FS_ROOTS(冒号/分号分隔);所有相对路径先经组件级规范化
//! (拒 `..`、拒绝对路径/盘符),拼到根上后再 canonicalize 双保险挡符号链接逃逸。
//! fail-closed:未设根、穿越、根外、逃逸一律拒绝。
#![cfg(feature = "collab-host")]

use serde::Serialize;
use std::path::{Component, Path, PathBuf};

#[derive(Serialize)]
pub struct FsEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: u64,
}

/// 配置的浏览根(POLARIS_FS_ROOTS)。空 = 关闭远程浏览(fail-closed)。
pub fn roots() -> Vec<PathBuf> {
    std::env::var("POLARIS_FS_ROOTS")
        .ok()
        .into_iter()
        .flat_map(|s| {
            s.split(|c| c == ':' || c == ';')
                .map(|p| PathBuf::from(p.trim()))
                .collect::<Vec<_>>()
        })
        .filter(|p| !p.as_os_str().is_empty())
        .collect()
}

/// 把相对请求路径关押进某个根:规范化(去 `.`、拒 `..`/绝对路径)后拼到根上,
/// 再确认仍落在某个根内。存在的目标额外 canonicalize 挡符号链接逃逸。
pub fn resolve_jailed(roots: &[PathBuf], rel: &str) -> Result<PathBuf, String> {
    let mut safe = PathBuf::new();
    for comp in Path::new(rel).components() {
        match comp {
            Component::Normal(c) => safe.push(c),
            Component::CurDir => {}
            // `..`、`/`(RootDir)、盘符(Prefix)一律拒绝 —— 绝对路径与穿越挡在这里。
            _ => return Err("非法路径(禁止 .. / 绝对路径)".into()),
        }
    }
    for root in roots {
        let full = root.join(&safe);
        // 存在则 canonicalize 双保险(符号链接逃逸);不存在(或平台不支持)退回词法前缀,
        // 组件循环已挡 `..`,故词法拼接不会逃出 root。
        match (full.canonicalize(), root.canonicalize()) {
            (Ok(canon), Ok(rc)) => {
                if canon.starts_with(&rc) {
                    return Ok(canon);
                }
            }
            _ => {
                if full.starts_with(root) {
                    return Ok(full);
                }
            }
        }
    }
    Err("路径不在允许的浏览根内".into())
}

/// 列目录(相对根,"" = 根)。
pub fn list(rel: &str) -> Result<Vec<FsEntry>, String> {
    let rs = roots();
    if rs.is_empty() {
        return Err("本机未开放远程浏览(POLARIS_FS_ROOTS 未设)".into());
    }
    let dir = resolve_jailed(&rs, rel)?;
    let mut out = Vec::new();
    for e in std::fs::read_dir(&dir).map_err(|e| format!("读目录失败: {e}"))? {
        let e = match e {
            Ok(e) => e,
            Err(_) => continue,
        };
        let md = match e.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        out.push(FsEntry {
            name: e.file_name().to_string_lossy().to_string(),
            is_dir: md.is_dir(),
            size: md.len(),
            mtime,
        });
    }
    // 目录在前,同类按名不分大小写升序。
    out.sort_by(|a, b| {
        (b.is_dir, a.name.to_lowercase()).cmp(&(a.is_dir, b.name.to_lowercase()))
    });
    Ok(out)
}

/// 读文件字节(供预览/下载)。上限 512MB 防误读巨物 OOM。
pub fn read_bytes(rel: &str) -> Result<Vec<u8>, String> {
    let rs = roots();
    if rs.is_empty() {
        return Err("本机未开放远程浏览(POLARIS_FS_ROOTS 未设)".into());
    }
    let f = resolve_jailed(&rs, rel)?;
    let md = std::fs::metadata(&f).map_err(|e| format!("stat 失败: {e}"))?;
    if md.is_dir() {
        return Err("目标是目录,不能下载".into());
    }
    if md.len() > 512 * 1024 * 1024 {
        return Err("文件过大(>512MB),暂不支持在线读取".into());
    }
    std::fs::read(&f).map_err(|e| format!("读文件失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jail_rejects_traversal_and_outside() {
        let root = PathBuf::from("/data/share");
        // 正常子路径通过(根不存在 → 走词法前缀分支)
        assert!(resolve_jailed(&[root.clone()], "sub/a.txt").is_ok());
        // 空 = 根,通过
        assert!(resolve_jailed(&[root.clone()], "").is_ok());
        // `.` 归一化后仍在根内
        assert!(resolve_jailed(&[root.clone()], "./sub").is_ok());
        // `..` 穿越被拒
        assert!(resolve_jailed(&[root.clone()], "../etc/passwd").is_err());
        assert!(resolve_jailed(&[root.clone()], "sub/../../x").is_err());
        // 绝对路径逃逸被拒(RootDir 组件)
        assert!(resolve_jailed(&[root.clone()], "/etc/passwd").is_err());
    }
}
