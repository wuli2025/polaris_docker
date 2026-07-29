//! collab/fsface.rs —— 路径关押的远程文件访问(隧道另一端的「盘」)。
//!
//! 只在 collab-host 下编译(桌面 hosting 与 Docker server 壳都能挂,故不绑 collab-net)。
//! 浏览根有两个来源,合并生效:
//!   1. 环境变量 `POLARIS_FS_ROOTS`(冒号/分号分隔)—— 给 server / Docker 用;
//!      配套 `POLARIS_FS_WRITE=1` 把这批根一并开成可写(默认只读)。
//!   2. 落库的 meta 键 `fs_roots` —— 给桌面用:双击启动设不上环境变量,故互联页里
//!      勾选的共享目录持久化到 collab.db,是「浏览盘」在桌面能用的关键。
//!      每行 `路径` 或 `路径\tw`(带 `w` = 该根允许写)。**老版本存的是纯路径,解析成
//!      只读** —— 升级后既有共享目录不会凭空变可写。
//!
//! 权限模型是「默认只读,逐根点选放开写」。写盘比读盘危险一个量级(远端能删你的文件),
//! 所以写权限:①按根独立开关,不是全局一刀;②走 [`resolve_write`] 单独关押 —— 它比读
//! 路径多两道闸:**父目录必须 canonicalize 后仍在根内**(挡「根内软链指向根外」再写穿),
//! **目标自身是软链一律拒**(挡覆盖软链改写根外文件)。读路径的词法兜底对写不够用。
//!
//! 所有相对路径先经组件级规范化(拒 `..`、拒绝对路径/盘符),拼到根上后再 canonicalize
//! 双保险挡符号链接逃逸。fail-closed:无根、穿越、根外、逃逸、根不可写一律拒绝。
#![cfg(feature = "collab-host")]

use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

/// 落库共享目录的 meta 键(换行分隔;不用 `;`/`:` 是因 Windows 盘符 `D:\` 自带冒号)。
const K_FS_ROOTS: &str = "fs_roots";
/// 写入时的临时后缀:先写它再原子改名,半途断线不会留下一个截断的正式文件。
const TMP_SUFFIX: &str = ".polaris-upload";

#[derive(Serialize)]
pub struct FsEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: u64,
}

/// 一个共享根:路径 + 是否允许远端写。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ShareRoot {
    pub path: String,
    #[serde(default)]
    pub write: bool,
}

// ────────────────────────────── 共享根的来源与落库 ──────────────────────────────

/// 环境变量来源的根(server/docker)。分隔符按平台惯例:**Windows 只认 `;`**
/// (盘符自带 `:`,按 `:` 拆会把 `D:\share` 劈成 `D` 和 `\share`,两个都不存在 ——
/// 表现是「设了 POLARIS_FS_ROOTS 却什么都读不到」),类 Unix 认 `:` 与 `;`。
/// `POLARIS_FS_WRITE=1` 时这批根可写(整批一致 —— env 配置没有逐根表达的地方)。
fn env_entries() -> Vec<ShareRoot> {
    let write = std::env::var("POLARIS_FS_WRITE")
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    std::env::var("POLARIS_FS_ROOTS")
        .ok()
        .into_iter()
        .flat_map(|s| {
            let is_sep = |c: char| c == ';' || (!cfg!(windows) && c == ':');
            s.split(is_sep)
                .map(|p| p.trim().to_string())
                .collect::<Vec<_>>()
        })
        .filter(|p| !p.is_empty())
        .map(|path| ShareRoot { path, write })
        .collect()
}

/// 解析落库的一行。`路径` = 只读;`路径\tw` = 可写。空行/空路径 → None。
fn parse_line(line: &str) -> Option<ShareRoot> {
    let mut it = line.split('\t');
    let path = it.next().unwrap_or("").trim().to_string();
    if path.is_empty() {
        return None;
    }
    let write = it.next().unwrap_or("").contains('w');
    Some(ShareRoot { path, write })
}

fn dump_line(e: &ShareRoot) -> String {
    if e.write {
        format!("{}\tw", e.path)
    } else {
        e.path.clone()
    }
}

/// 落库(UI 勾选)来源的根。
fn persisted_entries() -> Vec<ShareRoot> {
    crate::collab::db::meta_get(K_FS_ROOTS)
        .map(|s| s.lines().filter_map(parse_line).collect())
        .unwrap_or_default()
}

/// 生效的共享根 = 环境变量 + 落库,按路径去重(重复项的写位取**或** ——
/// 任一来源开了写就算开)。空 = 关闭远程访问(fail-closed)。
pub fn entries() -> Vec<ShareRoot> {
    let mut out: Vec<ShareRoot> = Vec::new();
    for e in env_entries().into_iter().chain(persisted_entries()) {
        match out.iter_mut().find(|x| x.path == e.path) {
            Some(prev) => prev.write |= e.write,
            None => out.push(e),
        }
    }
    out
}

/// 生效的浏览根(读)。保持老签名:调用方(hosting 自述)只关心路径。
pub fn roots() -> Vec<PathBuf> {
    entries().into_iter().map(|e| PathBuf::from(e.path)).collect()
}

/// 本机对远端开放的能力:能读吗、有没有任何一个根可写。
/// 挂载端据此决定盘挂成读写还是只读(读写盘上每次写仍逐根复核,这里只是给 UI/协商用)。
pub fn caps() -> (bool, bool) {
    let es = entries();
    (!es.is_empty(), es.iter().any(|e| e.write))
}

/// UI 读:当前在本机勾选共享出去的目录(仅落库那部分)。
pub fn shared_entries() -> Vec<ShareRoot> {
    persisted_entries()
}

/// UI 读(兼容旧壳):只要路径清单。
pub fn shared_roots() -> Vec<String> {
    persisted_entries().into_iter().map(|e| e.path).collect()
}

/// UI 写:整体覆盖共享目录清单。每条必须是**已存在的目录**,否则整体拒绝(fail-closed)。
/// 空清单 = 关闭桌面侧共享(env 若另设了根仍生效)。返回规整后的清单供前端回填。
pub fn set_shared_entries(items: &[ShareRoot]) -> Result<Vec<ShareRoot>, String> {
    let mut clean: Vec<ShareRoot> = Vec::new();
    for raw in items {
        let t = raw.path.trim();
        if t.is_empty() {
            continue;
        }
        let p = PathBuf::from(t);
        if !p.is_dir() {
            return Err(format!("不是有效目录:{t}"));
        }
        let path = p.to_string_lossy().to_string();
        match clean.iter_mut().find(|x| x.path == path) {
            Some(prev) => prev.write |= raw.write,
            None => clean.push(ShareRoot { path, write: raw.write }),
        }
    }
    let blob = clean.iter().map(dump_line).collect::<Vec<_>>().join("\n");
    crate::collab::db::meta_set(K_FS_ROOTS, &blob)?;
    let desc = clean
        .iter()
        .map(|e| format!("{}{}", e.path, if e.write { "(可写)" } else { "" }))
        .collect::<Vec<_>>()
        .join(" | ");
    crate::collab::db::audit(
        "local",
        "fs.share",
        if clean.is_empty() { "cleared" } else { "set" },
        &desc,
    );
    Ok(clean)
}

/// UI 写(兼容旧壳):只给路径清单 = 全部只读。
pub fn set_shared_roots(paths: &[String]) -> Result<Vec<String>, String> {
    let items: Vec<ShareRoot> = paths
        .iter()
        .map(|p| ShareRoot { path: p.clone(), write: false })
        .collect();
    Ok(set_shared_entries(&items)?.into_iter().map(|e| e.path).collect())
}

// ────────────────────────────── 关押与路由 ──────────────────────────────

/// 相对路径 → 安全的相对 PathBuf。拒 `..`、绝对路径、盘符前缀。
fn normalize(rel: &str) -> Result<PathBuf, String> {
    let mut safe = PathBuf::new();
    for comp in Path::new(rel).components() {
        match comp {
            Component::Normal(c) => safe.push(c),
            Component::CurDir => {}
            // `..`、`/`(RootDir)、盘符(Prefix)一律拒绝 —— 绝对路径与穿越挡在这里。
            _ => return Err("非法路径(禁止 .. / 绝对路径)".into()),
        }
    }
    Ok(safe)
}

/// 把相对请求路径关押进某个根:规范化后拼到根上,再确认仍落在某个根内。
/// 存在的目标额外 canonicalize 挡符号链接逃逸;不存在的退回词法前缀(读路径够用)。
pub fn resolve_jailed(roots: &[PathBuf], rel: &str) -> Result<PathBuf, String> {
    let safe = normalize(rel)?;
    for root in roots {
        let full = root.join(&safe);
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

/// 写目标专用关押。比读多两道闸,理由见文件头:
///  · **父目录** canonicalize 后必须仍在根内 —— 挡「根内有个软链指向根外」然后往里写;
///  · 目标自身若已是软链,直接拒 —— 挡「覆盖软链」改写根外文件。
///
/// 返回落定后的绝对路径(父目录已 canonicalize,故这条路径不再含软链跳板)。
pub fn resolve_write(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let safe = normalize(rel)?;
    if safe.as_os_str().is_empty() {
        return Err("不能直接写共享根本身".into());
    }
    let full = root.join(&safe);
    let rc = root
        .canonicalize()
        .map_err(|e| format!("共享根不可用: {e}"))?;
    let parent = full.parent().ok_or("路径没有上级目录")?;
    let pc = parent
        .canonicalize()
        .map_err(|e| format!("上级目录不存在或不可用: {e}"))?;
    if !pc.starts_with(&rc) {
        return Err("路径不在允许的共享根内".into());
    }
    let name = full.file_name().ok_or("路径缺文件名")?;
    let target = pc.join(name);
    // 已存在且是软链 → 拒。symlink_metadata 不跟随链接,这是判「它本身是不是链」的唯一姿势。
    if let Ok(md) = std::fs::symlink_metadata(&target) {
        if md.file_type().is_symlink() {
            return Err("目标是符号链接,拒绝写入".into());
        }
    }
    Ok(target)
}

/// 根的展示名:末段目录名;盘符根(`C:\`)无末段,退化成剥掉分隔符的整串(`C`)。
fn root_label(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| {
            p.to_string_lossy()
                .trim_matches(|c| c == '\\' || c == '/' || c == ':')
                .to_string()
        })
}

/// 多根时的虚拟顶层:每个根一个 `(label, 根)`,同名根按序补 ` (2)`、` (3)` 消歧。
/// label 就是对端(挂载盘/文件中心)看到的顶层文件夹名,也是相对路径的第一段。
fn labeled(es: &[ShareRoot]) -> Vec<(String, ShareRoot)> {
    let mut out: Vec<(String, ShareRoot)> = Vec::new();
    for e in es {
        let base = root_label(Path::new(&e.path));
        let mut label = base.clone();
        let mut i = 2;
        while out.iter().any(|(l, _)| l == &label) {
            label = format!("{base} ({i})");
            i += 1;
        }
        out.push((label, e.clone()));
    }
    out
}

/// 把相对路径路由到具体的根,返回 `(根, 根内相对路径)`。
/// 单根:不消耗首段(兼容既有浏览端语义);多根:首段是虚拟顶层的 label。
fn route(rel: &str) -> Result<(ShareRoot, String), String> {
    let es = entries();
    if es.is_empty() {
        return Err("本机未开放远程访问(共享目录清单为空)".into());
    }
    if es.len() == 1 {
        return Ok((es[0].clone(), rel.to_string()));
    }
    let trimmed = rel.trim_matches(|c| c == '/' || c == '\\');
    let (head, rest) = match trimmed.find(['/', '\\']) {
        Some(i) => (&trimmed[..i], &trimmed[i + 1..]),
        None => (trimmed, ""),
    };
    for (label, e) in labeled(&es) {
        if label == head {
            return Ok((e, rest.to_string()));
        }
    }
    Err(format!("路径不在允许的共享根内(未知顶层「{head}」)"))
}

/// 路由到一个**可写**的根。根存在但没开写 → 明确报「只读」,别糊成「找不到」。
fn route_write(rel: &str) -> Result<(PathBuf, String), String> {
    let (e, rest) = route(rel)?;
    if !e.write {
        return Err(format!(
            "共享目录「{}」是只读的 —— 要远端可写,请在对端「互联 · 我共享的盘」里给它打开写权限",
            root_label(Path::new(&e.path))
        ));
    }
    Ok((PathBuf::from(e.path), rest))
}

/// 多根时的空路径 = 虚拟顶层,不落到任何真实目录。
fn is_virtual_top(rel: &str) -> bool {
    entries().len() > 1 && rel.trim_matches(|c| c == '/' || c == '\\').is_empty()
}

fn mtime_of(md: &std::fs::Metadata) -> u64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ────────────────────────────── 读 ──────────────────────────────

/// 列目录(相对根,"" = 根)。多根时根目录是虚拟顶层:每个共享根一个文件夹。
pub fn list(rel: &str) -> Result<Vec<FsEntry>, String> {
    if is_virtual_top(rel) {
        return Ok(labeled(&entries())
            .into_iter()
            .map(|(label, e)| {
                let mtime = std::fs::metadata(&e.path).map(|m| mtime_of(&m)).unwrap_or(0);
                FsEntry { name: label, is_dir: true, size: 0, mtime }
            })
            .collect());
    }
    let (e, rest) = route(rel)?;
    let dir = resolve_jailed(&[PathBuf::from(e.path)], &rest)?;
    let mut out = Vec::new();
    for ent in std::fs::read_dir(&dir).map_err(|e| format!("读目录失败: {e}"))? {
        let ent = match ent {
            Ok(x) => x,
            Err(_) => continue,
        };
        let md = match ent.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        out.push(FsEntry {
            name: ent.file_name().to_string_lossy().to_string(),
            is_dir: md.is_dir(),
            size: md.len(),
            mtime: mtime_of(&md),
        });
    }
    // 目录在前,同类按名不分大小写升序。
    out.sort_by(|a, b| (b.is_dir, a.name.to_lowercase()).cmp(&(a.is_dir, b.name.to_lowercase())));
    Ok(out)
}

/// 单个条目的属性。挂载盘的 PROPFIND(文件)/HEAD 走这条 —— 比「列父目录再找一遍」
/// 快一个量级(大目录里父目录列举是 O(n) 系统调用 + O(n) 网络载荷)。
pub fn stat(rel: &str) -> Result<FsEntry, String> {
    if is_virtual_top(rel) {
        return Ok(FsEntry { name: String::new(), is_dir: true, size: 0, mtime: 0 });
    }
    let (e, rest) = route(rel)?;
    let p = resolve_jailed(&[PathBuf::from(e.path)], &rest)?;
    let md = std::fs::metadata(&p).map_err(|e| format!("stat 失败: {e}"))?;
    let name = p
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(FsEntry { name, is_dir: md.is_dir(), size: md.len(), mtime: mtime_of(&md) })
}

/// 打开文件供**流式**读(下载/挂载盘):路由 + 关押,返回 (句柄, 总长, mtime 秒)。
/// 不整读进内存 → 没有大小上限;调用方自己 seek(Range)与分块。
pub fn open_jailed(rel: &str) -> Result<(std::fs::File, u64, u64), String> {
    let (e, rest) = route(rel)?;
    let f = resolve_jailed(&[PathBuf::from(e.path)], &rest)?;
    let md = std::fs::metadata(&f).map_err(|e| format!("stat 失败: {e}"))?;
    if md.is_dir() {
        return Err("目标是目录,不能下载".into());
    }
    let file = std::fs::File::open(&f).map_err(|e| format!("打开文件失败: {e}"))?;
    Ok((file, md.len(), mtime_of(&md)))
}

// ────────────────────────────── 写 ──────────────────────────────

/// 一次写入的落点:先写 `tmp`,收完整了再原子改名成 `final_path`。
/// 半途断线只留下一个临时文件(下次同名写入会覆盖它),正式文件永远不是截断态。
#[derive(Debug)]
pub struct WriteTarget {
    pub tmp: PathBuf,
    pub final_path: PathBuf,
}

/// 开一次文件写入。返回临时文件句柄 + 落点。调用方写完调 [`commit_write`],
/// 失败调 [`abort_write`]。
pub fn begin_write(rel: &str) -> Result<(std::fs::File, WriteTarget), String> {
    let (root, rest) = route_write(rel)?;
    let final_path = resolve_write(&root, &rest)?;
    if final_path.is_dir() {
        return Err("目标是一个目录,不能当文件写".into());
    }
    let mut tmp = final_path.clone().into_os_string();
    tmp.push(TMP_SUFFIX);
    let tmp = PathBuf::from(tmp);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&tmp)
        .map_err(|e| format!("建临时文件失败: {e}"))?;
    Ok((file, WriteTarget { tmp, final_path }))
}

/// 临时文件 → 正式文件。Windows 的 rename 不跨越已存在目标,故先删旧的。
pub fn commit_write(t: &WriteTarget) -> Result<(), String> {
    if t.final_path.exists() {
        std::fs::remove_file(&t.final_path).map_err(|e| format!("旧文件占位删不掉: {e}"))?;
    }
    std::fs::rename(&t.tmp, &t.final_path).map_err(|e| format!("改名落定失败: {e}"))
}

pub fn abort_write(t: &WriteTarget) {
    let _ = std::fs::remove_file(&t.tmp);
}

/// 建目录(等价 MKCOL)。父目录必须已存在 —— WebDAV 语义如此,也避免手滑造出一串空目录。
pub fn mkdir(rel: &str) -> Result<(), String> {
    let (root, rest) = route_write(rel)?;
    let p = resolve_write(&root, &rest)?;
    if p.exists() {
        return Err("同名文件/目录已存在".into());
    }
    std::fs::create_dir(&p).map_err(|e| format!("建目录失败: {e}"))
}

/// 删文件或目录(目录递归)。
pub fn remove(rel: &str) -> Result<(), String> {
    let (root, rest) = route_write(rel)?;
    let p = resolve_write(&root, &rest)?;
    let md = std::fs::symlink_metadata(&p).map_err(|e| format!("目标不存在: {e}"))?;
    if md.is_dir() {
        std::fs::remove_dir_all(&p).map_err(|e| format!("删目录失败: {e}"))
    } else {
        std::fs::remove_file(&p).map_err(|e| format!("删文件失败: {e}"))
    }
}

/// 改名/移动。源与目标**都**要落在可写根内(跨根移动只要两边都可写就允许)。
pub fn rename(rel: &str, dest_rel: &str) -> Result<(), String> {
    let (sroot, srest) = route_write(rel)?;
    let src = resolve_write(&sroot, &srest)?;
    if !src.exists() {
        return Err("源路径不存在".into());
    }
    let (droot, drest) = route_write(dest_rel)?;
    let dst = resolve_write(&droot, &drest)?;
    if dst.exists() {
        // 覆盖式移动:WebDAV 的 Overwrite: T 是默认值,资源管理器覆盖粘贴靠它。
        if dst.is_dir() {
            std::fs::remove_dir_all(&dst).map_err(|e| format!("目标目录清不掉: {e}"))?;
        } else {
            std::fs::remove_file(&dst).map_err(|e| format!("目标文件清不掉: {e}"))?;
        }
    }
    match std::fs::rename(&src, &dst) {
        Ok(()) => Ok(()),
        // 跨设备/跨盘 rename 会失败(EXDEV),退回「拷贝 + 删源」。
        Err(_) => {
            copy_tree(&src, &dst)?;
            if src.is_dir() {
                std::fs::remove_dir_all(&src).map_err(|e| format!("移动后清源失败: {e}"))
            } else {
                std::fs::remove_file(&src).map_err(|e| format!("移动后清源失败: {e}"))
            }
        }
    }
}

/// 复制(WebDAV COPY)。同上:两端都要在可写根内。
pub fn copy(rel: &str, dest_rel: &str) -> Result<(), String> {
    let (sroot, srest) = route_write(rel)?;
    let src = resolve_write(&sroot, &srest)?;
    if !src.exists() {
        return Err("源路径不存在".into());
    }
    let (droot, drest) = route_write(dest_rel)?;
    let dst = resolve_write(&droot, &drest)?;
    if dst.exists() {
        if dst.is_dir() {
            std::fs::remove_dir_all(&dst).map_err(|e| format!("目标目录清不掉: {e}"))?;
        } else {
            std::fs::remove_file(&dst).map_err(|e| format!("目标文件清不掉: {e}"))?;
        }
    }
    copy_tree(&src, &dst)
}

/// 递归拷贝。目录树在**对端本机**复制,不经隧道往返 —— 这是 COPY 走服务端的全部意义。
fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    let md = std::fs::symlink_metadata(src).map_err(|e| format!("读源属性失败: {e}"))?;
    if md.file_type().is_symlink() {
        return Err("源是符号链接,拒绝复制".into());
    }
    if md.is_dir() {
        std::fs::create_dir_all(dst).map_err(|e| format!("建目标目录失败: {e}"))?;
        for ent in std::fs::read_dir(src).map_err(|e| format!("读源目录失败: {e}"))? {
            let ent = ent.map_err(|e| format!("读源目录项失败: {e}"))?;
            copy_tree(&ent.path(), &dst.join(ent.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dst).map(|_| ()).map_err(|e| format!("复制失败: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 落库共享目录:设→读→并入 entries() 往返;非目录整体拒绝;写位随行落库。
    /// 互设 POLARIS_COLLAB_DB,须与 db 其它测试串行(借 db 的 TEST_LOCK)。
    #[test]
    fn shared_roots_persist_and_merge() {
        let _g = crate::collab::db::TEST_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join("polaris-fsface-share-test.db");
        let _ = std::fs::remove_file(&tmp);
        std::env::set_var("POLARIS_COLLAB_DB", &tmp);
        std::env::remove_var("POLARIS_FS_ROOTS");
        std::env::remove_var("POLARIS_FS_WRITE");

        // 空 = 关闭访问。
        assert!(shared_roots().is_empty());
        assert!(roots().is_empty());
        assert_eq!(caps(), (false, false));

        // 用一个真实存在的目录(临时目录本身)。
        let dir = std::env::temp_dir();
        let ds = dir.to_string_lossy().to_string();
        let saved = set_shared_roots(&[ds.clone(), ds.clone()]).unwrap(); // 重复项应去重
        assert_eq!(saved.len(), 1, "重复目录应去重");
        assert_eq!(shared_roots(), saved);
        assert!(roots().iter().any(|p| p == &dir), "roots() 须并入落库目录");
        assert_eq!(caps(), (true, false), "旧口径(纯路径)必须解析成只读");

        // 打开写位 → 落库 → 读回来仍是可写。
        let rw = set_shared_entries(&[ShareRoot { path: ds.clone(), write: true }]).unwrap();
        assert!(rw[0].write);
        assert_eq!(caps(), (true, true));
        assert!(shared_entries()[0].write, "写位必须持久化");

        // 不存在的目录 → 整体拒绝,清单不变。
        let bad = dir.join("definitely-not-here-xyz");
        assert!(set_shared_roots(&[bad.to_string_lossy().to_string()]).is_err());
        assert_eq!(shared_entries(), rw, "失败不应改动已存清单");

        // 空清单 = 停止共享。
        assert!(set_shared_roots(&[]).unwrap().is_empty());
        assert!(shared_roots().is_empty());

        std::env::remove_var("POLARIS_COLLAB_DB");
        let _ = std::fs::remove_file(&tmp);
    }

    /// 多根:根目录合成虚拟顶层(每根一个文件夹,重名消歧),首段路由进对应根。
    #[test]
    fn multi_root_virtual_top() {
        let tmp = std::env::temp_dir().join("polaris-fsface-multiroot");
        let a = tmp.join("alpha");
        let b = tmp.join("beta");
        std::fs::create_dir_all(a.join("inner")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("inner").join("x.txt"), b"hi").unwrap();
        let es = vec![
            ShareRoot { path: a.to_string_lossy().into(), write: false },
            ShareRoot { path: b.to_string_lossy().into(), write: false },
        ];

        // 虚拟顶层的名字。
        let labels = labeled(&es);
        assert_eq!(labels[0].0, "alpha");
        assert_eq!(labels[1].0, "beta");
        // 同名根消歧:第二个 alpha 变 "alpha (2)"。
        let dup = labeled(&[es[0].clone(), es[0].clone()]);
        assert_eq!(dup[1].0, "alpha (2)");

        // 关押后真读得到文件。
        let f = resolve_jailed(&[a.clone()], "inner/x.txt").unwrap();
        assert_eq!(std::fs::read(f).unwrap(), b"hi");
        let _ = std::fs::remove_dir_all(&tmp);
    }

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

    /// 写路径的关押:比读严。父目录不存在 → 拒(不静默造目录);根本身 → 拒;
    /// `..` → 拒。父目录 canonicalize 后不在根内 → 拒。
    #[test]
    fn write_jail_is_stricter_than_read() {
        let base = std::env::temp_dir().join("polaris-fsface-writejail");
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        // 正常:父目录存在且在根内。
        let p = resolve_write(&root, "sub/new.txt").unwrap();
        assert!(p.starts_with(root.canonicalize().unwrap()));

        // 根本身不能当写目标。
        assert!(resolve_write(&root, "").is_err());
        // 父目录不存在 → 拒(读路径的词法兜底在这里是不够的)。
        assert!(resolve_write(&root, "nope/deep/x.txt").is_err());
        // 穿越 → 拒。
        assert!(resolve_write(&root, "../outside/x.txt").is_err());
        assert!(resolve_write(&root, "/etc/passwd").is_err());

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 只读根拒写:route_write 必须报「只读」,而不是含糊的「找不到」。
    #[test]
    fn readonly_root_refuses_writes() {
        let _g = crate::collab::db::TEST_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join("polaris-fsface-ro-test.db");
        let _ = std::fs::remove_file(&tmp);
        std::env::set_var("POLARIS_COLLAB_DB", &tmp);
        std::env::remove_var("POLARIS_FS_ROOTS");
        std::env::remove_var("POLARIS_FS_WRITE");

        let dir = std::env::temp_dir().join("polaris-fsface-ro-root");
        std::fs::create_dir_all(&dir).unwrap();
        let ds = dir.to_string_lossy().to_string();

        set_shared_entries(&[ShareRoot { path: ds.clone(), write: false }]).unwrap();
        let e = begin_write("x.txt").unwrap_err();
        assert!(e.contains("只读"), "err={e}");
        assert!(mkdir("d").unwrap_err().contains("只读"));
        assert!(remove("x.txt").unwrap_err().contains("只读"));

        // 打开写位后同一条路径就能写了,且写完是原子落定。
        set_shared_entries(&[ShareRoot { path: ds.clone(), write: true }]).unwrap();
        {
            use std::io::Write;
            let (mut f, t) = begin_write("x.txt").unwrap();
            f.write_all(b"hello").unwrap();
            drop(f);
            assert!(t.tmp.exists() && !t.final_path.exists(), "落定前只该有临时文件");
            commit_write(&t).unwrap();
        }
        assert_eq!(std::fs::read(dir.join("x.txt")).unwrap(), b"hello");
        assert_eq!(stat("x.txt").unwrap().size, 5);

        // 目录 / 改名 / 删除 一条龙。
        mkdir("d").unwrap();
        assert!(dir.join("d").is_dir());
        rename("x.txt", "d/y.txt").unwrap();
        assert!(!dir.join("x.txt").exists() && dir.join("d/y.txt").exists());
        copy("d/y.txt", "d/z.txt").unwrap();
        assert_eq!(std::fs::read(dir.join("d/z.txt")).unwrap(), b"hello");
        remove("d").unwrap();
        assert!(!dir.join("d").exists());

        std::env::remove_var("POLARIS_COLLAB_DB");
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// env 来源的根:默认只读,`POLARIS_FS_WRITE=1` 才可写。
    #[test]
    fn env_roots_write_gate() {
        let _g = crate::collab::db::TEST_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join("polaris-fsface-env-test.db");
        let _ = std::fs::remove_file(&tmp);
        std::env::set_var("POLARIS_COLLAB_DB", &tmp);
        let dir = std::env::temp_dir();
        std::env::set_var("POLARIS_FS_ROOTS", dir.to_string_lossy().to_string());

        std::env::remove_var("POLARIS_FS_WRITE");
        assert_eq!(caps(), (true, false), "env 根默认只读");
        std::env::set_var("POLARIS_FS_WRITE", "1");
        assert_eq!(caps(), (true, true), "POLARIS_FS_WRITE=1 才开写");

        std::env::remove_var("POLARIS_FS_WRITE");
        std::env::remove_var("POLARIS_FS_ROOTS");
        std::env::remove_var("POLARIS_COLLAB_DB");
        let _ = std::fs::remove_file(&tmp);
    }
}
