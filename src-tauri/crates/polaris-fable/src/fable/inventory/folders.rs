use super::*;

// ───────────────────────── 扫描文件夹(盘点 = 扫描 + 选目录)─────────────────────────
//
// 「盘点」点开后先扫一眼文件夹结构(只读目录项、不读内容,秒级),让用户勾选要盘点的文件夹
// 再开始建库。范围**不局限于知识库**:除了知识库根 + NAS 挂载点(默认勾上),还会列出本机的
// 盘符 / 桌面 / 外置卷(默认不勾,用户按需勾选),于是知识库之外的任意文件夹也能盘进文件库。
// 设计:初次只列「根 + 第一层子目录」,更深层在用户点开时按需懒加载(fable_scan_folder_children),
// 于是 C/D 盘也能一层层点进任意深度而不必一次扫全盘。每个文件夹给直属文件数 + 是否还有更深子目录
// (前端据此显示展开箭头);各文件夹的「递归总大小」由 fable_folder_size 限并发地按需算。
// 系统/缓存目录(Windows、Program Files…)用 [`skip_dir_scan`] 整棵跳过。

/// 第一层文件夹总数上限(超出截断,前端提示「列表已截断」)。更深层按需懒加载,不受此限。
const FOLDER_SCAN_CAP: usize = 5000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanRootInfo {
    /// 根绝对路径(也是 path / parent / root 字段的同源串)。
    pub path: String,
    /// 显示名(知识库 / C: 盘 / 桌面 / 挂载点…)。
    pub label: String,
    /// 默认是否勾选(知识库 + NAS = true;盘符/桌面 = false,用户按需勾)。
    pub default_on: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderNode {
    /// 绝对路径(与盘点 walker 看到的 `entry.path()` 同源 → 可直接当 root/exclude 用)。
    pub path: String,
    /// 父目录绝对路径(顶层文件夹的父 = 所属根)。
    pub parent: String,
    /// 显示名(末段)。
    pub name: String,
    /// 所属根的绝对路径。
    pub root: String,
    /// 相对根的深度(1=顶层)。
    pub depth: usize,
    /// 该文件夹直属文件数(不含子目录)。
    pub files: u64,
    /// 是否还有更深的(未被跳过的)子目录 → 前端可展开。
    pub has_children: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderScan {
    pub roots: Vec<ScanRootInfo>,
    pub folders: Vec<FolderNode>,
    pub truncated: bool,
}

/// 盘点可选的全部根:知识库根 + NAS 挂载点(默认勾)+ 本机盘符/桌面/外置卷(默认不勾)。
fn scan_root_candidates(explicit: Option<String>) -> Vec<ScanRootInfo> {
    // 显式指定一个根 → 只扫它(默认勾上)。
    if let Some(r) = explicit
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
    {
        let label = Path::new(&r)
            .file_name()
            .map(super::decode_fs)
            .unwrap_or_else(|| r.clone());
        return vec![ScanRootInfo {
            path: r,
            label,
            default_on: true,
        }];
    }
    let mut out: Vec<ScanRootInfo> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let kb = crate::kb::kb_root();
    // App 数据根的友好名(下载/微信/QQ…),给下方贴标签用(inventory_roots 已含这些路径)。
    let app_labels: std::collections::HashMap<String, String> = app_data_roots()
        .into_iter()
        .map(|r| (r.path, r.label))
        .collect();
    // 默认勾:知识库根 + NAS 挂载点 + App 数据下载目录(沿用盘点默认根集合)。
    for r in inventory_roots(None) {
        // 有界探测:NAS 挂载点掉线时 is_dir 会 stat 几十秒吊死选择器 → 超死线判不可达即跳过
        // (死线见 [`probe_secs`],放宽到 12s 容忍冷连接 NAS 的首次慢响应)。
        if !super::sched::dir_reachable(Path::new(&r), probe_secs()) || !seen.insert(r.clone()) {
            continue;
        }
        let label = if r == kb {
            "知识库".to_string()
        } else if let Some(l) = app_labels.get(&r) {
            l.clone()
        } else {
            Path::new(&r)
                .file_name()
                .map(super::decode_fs)
                .unwrap_or_else(|| r.clone())
        };
        out.push(ScanRootInfo {
            path: r,
            label,
            default_on: true,
        });
    }
    // 本机盘符 / 桌面 / 外置卷 / 挂载点(复用全盘资源归集的跨平台根)。
    // default_on 直接沿用 scan_roots 的判断(现在「一个不落」——所有真实存在的盘符/卷默认都勾),
    // 这样首次盘点就能把整机所有可达的盘都纳入,用户想缩小范围再手动取消。
    for sr in crate::scan::scan_roots() {
        // 同上:挂载点/网络卷可能僵死,有界探测(死线见 [`probe_secs`]),不可达就不进选择器。
        if !super::sched::dir_reachable(Path::new(&sr.path), probe_secs())
            || !seen.insert(sr.path.clone())
        {
            continue;
        }
        out.push(ScanRootInfo {
            path: sr.path,
            label: sr.label,
            default_on: sr.default_on,
        });
    }
    out
}

/// 列出某目录的直属子文件夹(只读一层目录项;每个子目录再读一层估直属文件数 + 是否可展开)。
/// `root` = 所属盘点根(用于算 depth 并回填 FolderNode.root)。
fn list_child_folders(dir: &Path, root: &str) -> Vec<FolderNode> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    // 只有正浏览「本机整盘根目录顶层」(如 C:\ 直属子目录)时才叠加 OS 目录黑名单,把 Windows、
    // Program Files 等藏起来;往里钻一层后(用户自己的文件夹)只剪永远跳的噪音,这样里面名叫
    // library/boot 的子目录照常显示、可选可盘 ——「文件夹里的都能归类进库」也要在选择器里看得见。
    // 映射的 NAS 盘符是远程盘(非本机系统盘)→ 不当整盘重剪,选择器才会和盘点一样把它顶层那些
    // 名叫 system/library 的 NAS 共享如实列出(否则用户在选择器里根本看不到、也就盘不到它们)。
    let os_top = is_os_disk_root(root) && !is_remote_root(root) && Path::new(root) == dir;
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() || !ft.is_dir() {
            continue;
        }
        let name = super::decode_fs(&entry.file_name());
        if skip_dir_always(&name) || (os_top && skip_dir_os(&name)) {
            continue;
        }
        subdirs.push(entry.path());
    }
    let root_path = Path::new(root);
    let mut out: Vec<FolderNode> = Vec::with_capacity(subdirs.len());
    for sub in subdirs {
        // 直属文件数 + 是否还有更深(未被跳过的)子目录 → 前端据此显示「可展开」。
        let mut files = 0u64;
        let mut has_children = false;
        if let Ok(rd2) = std::fs::read_dir(&sub) {
            for e2 in rd2.flatten() {
                let Ok(ft2) = e2.file_type() else { continue };
                if ft2.is_symlink() {
                    continue;
                }
                if ft2.is_dir() {
                    // 孙级目录必在顶层之下 → 只看「永远跳」,有真子目录即可展开。
                    let n2 = super::decode_fs(&e2.file_name());
                    if !skip_dir_always(&n2) {
                        has_children = true;
                    }
                } else if ft2.is_file() {
                    files += 1;
                }
            }
        }
        let name = sub
            .file_name()
            .map(super::decode_fs)
            .unwrap_or_else(|| sub.to_string_lossy().into_owned());
        let depth = sub
            .strip_prefix(root_path)
            .map(|r| r.components().count())
            .unwrap_or(1)
            .max(1);
        out.push(FolderNode {
            path: sub.to_string_lossy().into_owned(),
            parent: dir.to_string_lossy().into_owned(),
            name,
            root: root.to_string(),
            depth,
            files,
            has_children,
        });
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// 列出可盘点根 + 各根的「第一层」子文件夹;更深的层级由前端展开时按需懒加载
/// (见 [`fable_scan_folder_children`]),这样 C/D 盘等也能一层层点开,不必一次扫全盘。
pub fn scan_folders(explicit: Option<String>) -> Result<FolderScan, String> {
    let roots = scan_root_candidates(explicit);
    if roots.is_empty() {
        return Err("没有可扫描的根目录(知识库未初始化,也无可访问的盘符/挂载点)".into());
    }
    let mut folders: Vec<FolderNode> = Vec::new();
    let mut truncated = false;
    for root in &roots {
        // 列子目录要 read_dir 挂载点,死 NAS 会吊死整个选择器 → 每根加死线(见 [`probe_secs`]),
        // 超时就跳过这个根(其它健康根照常展示),用户点「盘点」绝不转圈卡死。
        let rp = root.path.clone();
        let children = super::sched::with_deadline(probe_secs(), move || {
            list_child_folders(Path::new(&rp), &rp)
        })
        .unwrap_or_default();
        for node in children {
            folders.push(node);
            if folders.len() >= FOLDER_SCAN_CAP {
                truncated = true;
                break;
            }
        }
        if truncated {
            break;
        }
    }
    Ok(FolderScan {
        roots,
        folders,
        truncated,
    })
}

/// 盘点前先扫一眼文件夹结构(根 + 第一层)。
/// `(async)`:枚举盘符 + 读顶层目录在掉线的映射网盘(Z: 等)上可能久卡 → 派到工作线程,不冻 UI。
#[cfg_attr(feature = "desktop", tauri::command(async))]
pub fn fable_scan_folders(root: Option<String>) -> Result<FolderScan, String> {
    scan_folders(root)
}

/// 懒加载:点开某个文件夹时才扫它的直属子文件夹(支持一层层往下钻到任意深度)。
/// `(async)`:`with_deadline` 内部已开旁路线程,但调用线程仍要 `recv_timeout` 等满死线
/// (NAS 上≈12s)→ 主线程跑会冻 UI(每展开一个文件夹冻一次)。派到工作线程即解。
#[cfg_attr(feature = "desktop", tauri::command(async))]
pub fn fable_scan_folder_children(root: String, path: String) -> Result<Vec<FolderNode>, String> {
    // 展开子目录:is_dir + read_dir 都可能卡死 NAS → 整体加死线(见 [`probe_secs`]),超时返回空
    // (该项显示为不可展开),请求线程绝不被吊死。
    Ok(super::sched::with_deadline(probe_secs(), move || {
        let p = Path::new(&path);
        if !p.is_dir() {
            return Vec::new();
        }
        list_child_folders(p, &root)
    })
    .unwrap_or_default())
}

/// 文件夹递归总量(总文件数 + 总字节数),给选择器里显示「这个文件夹有多大」。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderSize {
    pub files: u64,
    pub bytes: u64,
}

// 累加进原子计数器(而非 `&mut u64`):这样即便外层撞上死线被中途掐断,已经数到的部分也
// 留在计数器里能读回来 —— 大目录至少给个「下限体积」,而不是一刀切归 0。
fn folder_size_rec(dir: &Path, files: &AtomicU64, bytes: &AtomicU64, remote: bool, stop: &AtomicBool) {
    use std::sync::atomic::Ordering::Relaxed;
    // `stop` = 调用方的死线熄火位:with_deadline 超时只是让请求线程先走,旁路线程若继续
    // 满盘递归会白烧几分钟 IO(整盘兜底递归场景)→ 每层进门先看停止位,置位即整棵放弃。
    if stop.load(Relaxed) {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        // 巨型平铺目录(几十万直属文件)也要能及时收手,不能只等下一层递归边界。
        if stop.load(Relaxed) {
            return;
        }
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            let name = super::decode_fs(&entry.file_name());
            // 大小要反映「这个文件夹会被归类进库的真实体量」→ 只剪永远跳的噪音(.git/依赖/回收站)。
            if skip_dir_always(&name) {
                continue;
            }
            folder_size_rec(&entry.path(), files, bytes, remote, stop);
        } else if ft.is_file() {
            if let Ok(m) = entry.metadata() {
                bytes.fetch_add(on_disk_size(&entry.path(), &m, remote), Relaxed);
                files.fetch_add(1, Relaxed);
            }
        }
    }
}

/// 整盘根 / 网络盘根的「已用容量」= 磁盘总量 − 可用空间。一整块盘逐文件走完要几分钟、必撞死线
/// 返 0,但用户问「这个盘多大」要的本就是已用容量 → `GetDiskFreeSpaceExW` 即时拿到,准确零遍历。
/// 拿不到(盘掉线 / 非 Windows)返回 None,调用方退回递归(带死线、超时给部分值)。
#[cfg(windows)]
fn disk_used_bytes(path: &str) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let t = path.trim().trim_end_matches(['/', '\\']);
    let b = t.as_bytes();
    let root = if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        format!("{}:\\", b[0] as char) // "C:\"
    } else if t.starts_with("\\\\") || t.starts_with("//") {
        format!("{t}\\") // UNC 根 "\\server\share\"
    } else {
        return None;
    };
    let wide: Vec<u16> = std::ffi::OsStr::new(&root)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free_avail: u64 = 0;
    let mut total: u64 = 0;
    let mut total_free: u64 = 0;
    // SAFETY: wide 是 NUL 结尾的合法宽字符串;三个 out 指针均指向本栈上的有效 u64。
    let ok =
        unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free_avail, &mut total, &mut total_free) };
    if ok == 0 || total == 0 {
        return None;
    }
    Some(total.saturating_sub(total_free))
}

#[cfg(not(windows))]
fn disk_used_bytes(_path: &str) -> Option<u64> {
    None // mac/Docker:整盘根("/"、/Volumes/*)退回递归部分值;后续可接 statvfs
}

/// 递归统计一个文件夹的总文件数与总字节数(skip_dir_scan 剪枝;符号链接跳过)。
/// 前端在选择器里按需、限并发地逐个文件夹调用,把大小填进对应行。
/// `(async)`:同上 —— 调用线程要等满 10s 死线,主线程跑会冻 UI(选择器里每行都调一次,
/// 冻得最频繁)→ 派到工作线程。
#[cfg_attr(feature = "desktop", tauri::command(async))]
pub fn fable_folder_size(path: String) -> Result<FolderSize, String> {
    use std::sync::atomic::Ordering::Relaxed;
    // 整盘 / 网络盘根:走「已用容量」即时返回,不做注定撞死线的全盘遍历(那只会一直显示 0 ——
    // 正是 C:\ / D:\ / Z:\ 这些最该看到体积的行之前显示空白的根因)。
    if is_os_disk_root(&path) || is_remote_root(&path) {
        let pc = path.clone();
        // 掉线网络盘上 GetDiskFreeSpaceExW 也可能卡 → 同样套死线兜底。
        if let Some(Some(used)) =
            super::sched::with_deadline(probe_secs(), move || disk_used_bytes(&pc))
        {
            return Ok(FolderSize {
                files: 0,
                bytes: used,
            });
        }
        // 拿不到容量(盘掉线等)→ 落到下面的递归(同样带死线)。
    }
    // 普通文件夹:递归累加,带 10s 死线;超时也把「已数到的部分」读回来(原子计数器),给个下限
    // 体积而非 0,避免大目录永远显示空白。
    let files = std::sync::Arc::new(AtomicU64::new(0));
    let bytes = std::sync::Arc::new(AtomicU64::new(0));
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let (fc, bc, sc) = (files.clone(), bytes.clone(), stop.clone());
    let remote = is_remote_root(&path); // 网络盘:跳过逐文件实占往返,直接取逻辑大小
    let timed_out = super::sched::with_deadline(10, move || {
        let p = Path::new(&path);
        if p.is_dir() {
            folder_size_rec(p, &fc, &bc, remote, &sc);
        }
    })
    .is_none();
    // 超时后旁路线程虽被 detach,但共享着停止位:置位让它在下一层递归 / 下一个目录项处就
    // 收手,不再对整块盘满盘烧 IO(选择器逐行调本命令,detach 线程叠起来能把磁盘拖垮)。
    if timed_out {
        stop.store(true, Relaxed);
    }
    Ok(FolderSize {
        files: files.load(Relaxed),
        bytes: bytes.load(Relaxed),
    })
}
