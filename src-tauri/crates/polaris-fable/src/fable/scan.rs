//! 板块 · 全盘资源归集 — 跨平台扫描 + 启发式预览 + 价值评分
//!
//! 设计依据: 桌面 PRD「全盘资源归集 v4」。
//! - 扫描全程**只读**: 只 list + 读文件头, 绝不删改源文件。
//! - 跨平台扫描根 (Win 盘符 / mac 家目录+Volumes / Docker 挂载卷)。
//! - 黑名单剪枝(系统/缓存/依赖/敏感目录) + 白名单后缀 + 价值评分 + 启发式「大概内容」。
//! - 归档不在本模块: 复用 kb::kb_upload_files 把选中文件复制入资源库 raw/;
//!   「摄入核心层」= 归档后再跑 kb::kb_compile(构建知识网)。
//!
//! 本模块零外部新依赖(std + walkdir,后者 kb.rs 已在用)。

use serde::Serialize;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

// ───────────────────────── 数据结构 ─────────────────────────

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ScanRoot {
    /// 稳定标识(也是扫描时传回的 path 来源)
    pub id: String,
    /// 显示名,如「桌面」「C: 盘」
    pub label: String,
    /// 绝对路径
    pub path: String,
    /// desktop | drive | home | volume | mounted
    pub kind: String,
    /// 默认是否勾选
    pub default_on: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ScanRow {
    /// 稳定 id(路径哈希)
    pub id: String,
    pub path: String,
    pub name: String,
    pub ext: String,
    /// doc | sheet | slide | data | image | audio | video | archive | code | text | other
    pub kind: String,
    /// 大概内容(启发式;binary 类先给占位,待「智能摘要」增强)
    pub preview: String,
    pub size: u64,
    pub size_h: String,
    /// 修改时间(unix 秒)
    pub mtime: i64,
    /// 价值评分 1-5
    pub score: u8,
    /// 建议去向: resource | resource+core | skip
    pub suggest: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub rows: Vec<ScanRow>,
    /// 遍历过的文件总数(含被跳过的)
    pub total_seen: u64,
    /// 命中(进表)的资源数
    pub hit: usize,
    /// 因不在白名单/太小等被跳过的数
    pub skipped: u64,
    /// 是否因达到上限被截断
    pub truncated: bool,
}

// ───────────────────────── 扫描根(跨平台) ─────────────────────────

fn home_dir() -> Option<std::path::PathBuf> {
    directories::UserDirs::new().map(|u| u.home_dir().to_path_buf())
}

// ───────────── Windows 工业级盘符/卷枚举 ─────────────
//
// 旧实现只是 A-Z 逐个 `Path::exists()` 探测,会漏两类盘、且可能卡死:
//   1. **挂到文件夹的卷 / 存储池**(没有盘符,如 D:\Mounts\Data、Storage Spaces)——
//      旧法完全看不到。这里用 FindFirstVolume/FindNextVolume 逐卷枚举,一个不落。
//   2. **掉线的网络驱动器**:`.exists()` 会发真实 IO 卡几十秒;改用 GetLogicalDrives
//      位掩码(瞬时、纯内核态,不碰介质)+ GetDriveType 归类,绝不卡死。
// 同时读卷标(GetVolumeInformation)让用户一眼认出哪个盘是哪个,并按盘型给友好后缀。
#[cfg(target_os = "windows")]
fn to_wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// 读卷标(失败/空 → None)。已用 SetThreadErrorMode 抑制无介质弹窗,可放心对任意盘调用。
#[cfg(target_os = "windows")]
fn win_volume_label(wroot: &[u16]) -> Option<String> {
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;
    let mut name = [0u16; 256];
    let ok = unsafe {
        GetVolumeInformationW(
            wroot.as_ptr(),
            name.as_mut_ptr(),
            name.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    if ok == 0 {
        return None;
    }
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    (len > 0).then(|| String::from_utf16_lossy(&name[..len]))
}

/// 某个卷(\\?\Volume{GUID}\)对应的全部挂载路径(双 null 结尾多字符串)。
#[cfg(target_os = "windows")]
fn win_volume_mount_paths(volume_name: &[u16]) -> Vec<String> {
    use windows_sys::Win32::Storage::FileSystem::GetVolumePathNamesForVolumeNameW;
    let mut buf = vec![0u16; 1024];
    let mut ret_len: u32 = 0;
    let ok = unsafe {
        GetVolumePathNamesForVolumeNameW(
            volume_name.as_ptr(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            &mut ret_len,
        )
    };
    if ok == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    for i in 0..buf.len() {
        if buf[i] == 0 {
            if i == start {
                break; // 空串 = 列表结束
            }
            out.push(String::from_utf16_lossy(&buf[start..i]));
            start = i + 1;
        }
    }
    out
}

#[cfg(target_os = "windows")]
fn windows_roots() -> Vec<ScanRoot> {
    use std::collections::HashSet;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        FindFirstVolumeW, FindNextVolumeW, FindVolumeClose, GetDriveTypeW, GetLogicalDrives,
    };
    use windows_sys::Win32::System::Diagnostics::Debug::{
        SetThreadErrorMode, SEM_FAILCRITICALERRORS,
    };
    use windows_sys::Win32::System::WindowsProgramming::{
        DRIVE_CDROM, DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOTE, DRIVE_REMOVABLE,
    };

    // 枚举期间抑制「驱动器中没有磁盘」等关键错误弹窗(线程局部,不影响别处)。
    let mut prev_mode: u32 = 0;
    unsafe {
        SetThreadErrorMode(SEM_FAILCRITICALERRORS, &mut prev_mode);
    }

    let mut out: Vec<ScanRoot> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // 桌面(置顶,默认勾)。
    if let Some(home) = home_dir() {
        let desk = home.join("Desktop");
        if desk.exists() {
            let p = desk.to_string_lossy().to_string();
            seen.insert(p.clone());
            out.push(ScanRoot {
                id: p.clone(),
                label: "桌面".into(),
                path: p,
                kind: "desktop".into(),
                default_on: true,
            });
        }
    }

    // ① 有盘符的盘:GetLogicalDrives 位掩码 → 逐个 GetDriveType 归类 + 读卷标。
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = format!("{}:\\", letter);
        let wroot = to_wide(&root);
        let (kind_word, default_on) = match unsafe { GetDriveTypeW(wroot.as_ptr()) } {
            DRIVE_FIXED => ("本地", true),
            DRIVE_REMOVABLE => ("可移动", true),
            DRIVE_REMOTE => ("网络", true),
            DRIVE_CDROM => ("光驱", false),
            DRIVE_RAMDISK => ("内存盘", true),
            _ => ("", true), // unknown / no_root_dir 也照列,绝不漏
        };
        let label = match (win_volume_label(&wroot), kind_word.is_empty()) {
            (Some(v), false) => format!("{letter}: 盘 · {v}({kind_word})"),
            (Some(v), true) => format!("{letter}: 盘 · {v}"),
            (None, false) => format!("{letter}: 盘({kind_word})"),
            (None, true) => format!("{letter}: 盘"),
        };
        seen.insert(root.clone());
        out.push(ScanRoot {
            id: root.clone(),
            label,
            path: root,
            kind: "drive".into(),
            default_on,
        });
    }

    // ② 无盘符的卷:挂到文件夹的硬盘 / 存储池——旧法完全漏掉,这里逐卷捞回来。
    let mut name = [0u16; 260];
    let h = unsafe { FindFirstVolumeW(name.as_mut_ptr(), name.len() as u32) };
    if h != INVALID_HANDLE_VALUE {
        loop {
            for mount in win_volume_mount_paths(&name) {
                // 盘符根(如 "C:\")上面已收;只补挂到文件夹的挂载点。
                let is_drive_letter = mount.len() == 3 && mount.as_bytes().get(1) == Some(&b':');
                if is_drive_letter || seen.contains(&mount) || !Path::new(&mount).is_dir() {
                    continue;
                }
                let shown = mount.trim_end_matches('\\').to_string();
                let label = win_volume_label(&to_wide(&mount))
                    .map(|v| format!("卷 · {v}({shown})"))
                    .unwrap_or_else(|| format!("卷 · {shown}"));
                seen.insert(mount.clone());
                out.push(ScanRoot {
                    id: mount.clone(),
                    label,
                    path: mount,
                    kind: "volume".into(),
                    default_on: true,
                });
            }
            name = [0u16; 260];
            if unsafe { FindNextVolumeW(h, name.as_mut_ptr(), name.len() as u32) } == 0 {
                break;
            }
        }
        unsafe {
            FindVolumeClose(h);
        }
    }

    // ③ 同一台 NAS 上「没映射成盘符」的其它共享。
    // 用户把群晖映射成 Z:(=tx 共享)后,docker / web / web_packages 等共享**不会**自动出现 →
    // 平台就漏扫了那几百 GB(数据集全在 docker 共享里)。这里据已映射的网络盘反解出 NAS 主机,
    // 枚举该主机的全部磁盘共享,把「还没映射的」补成可勾选的盘点根(UNC 路径,凭据复用当前已登录
    // 会话,无需另外输账号密码)。整段有界(8s):NAS 掉线 / 慢也只是「这次没发现额外共享」,
    // 绝不冻结选择器。
    if let Some(shares) = with_timeout(8, windows_discover_nas_shares) {
        for sr in shares {
            if seen.insert(sr.path.clone()) {
                out.push(sr);
            }
        }
    }

    // 还原线程错误模式。
    unsafe {
        SetThreadErrorMode(prev_mode, std::ptr::null_mut());
    }
    out
}

/// 把可能阻塞(WNet 网络枚举)的活儿放后台线程跑,超死线就放弃,绝不吊死调用方(选择器线程)。
/// 超时的线程被 detach:它阻塞在 WNet syscall 上,随网络恢复 / 进程退出自然回收。
#[cfg(windows)]
fn with_timeout<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(std::time::Duration::from_secs(secs)).ok()
}

/// `\\host\share` → `\\host`(取 UNC 主机段)。
#[cfg(windows)]
fn unc_host(unc: &str) -> Option<String> {
    let rest = unc.strip_prefix(r"\\")?;
    let host = rest.split('\\').next()?;
    if host.is_empty() {
        None
    } else {
        Some(format!(r"\\{host}"))
    }
}

/// 已映射的网络盘符(如 `Z:`)反解出它指向的 UNC(`\\host\share`)。
#[cfg(windows)]
fn win_drive_unc(letter: char) -> Option<String> {
    use windows_sys::Win32::NetworkManagement::WNet::WNetGetConnectionW;
    let local = to_wide(&format!("{letter}:")); // 注意:不带结尾反斜杠
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: local 以 NUL 结尾;buf/len 是有效可写缓冲。
    let rc = unsafe { WNetGetConnectionW(local.as_ptr(), buf.as_mut_ptr(), &mut len) };
    if rc != 0 {
        return None; // 非 NO_ERROR
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(0);
    let s = String::from_utf16_lossy(&buf[..end]);
    if s.starts_with(r"\\") {
        Some(s)
    } else {
        None
    }
}

/// 枚举某 NAS 主机(`\\host`)上的全部磁盘共享,返回各共享的 UNC(`\\host\share`)。
/// 用 WNet 枚举(等同 `net view \\host`),凭据复用当前登录会话已缓存的那份。
#[cfg(windows)]
fn win_host_disk_shares(host_unc: &str) -> Vec<String> {
    use std::ffi::c_void;
    use windows_sys::Win32::NetworkManagement::WNet::{
        WNetCloseEnum, WNetEnumResourceW, WNetOpenEnumW, NETRESOURCEW, RESOURCETYPE_DISK,
        RESOURCEUSAGE_CONTAINER, RESOURCE_GLOBALNET,
    };
    let mut out = Vec::new();
    let mut remote = to_wide(host_unc);
    // SAFETY: NETRESOURCEW 是 POD,zeroed 合法;随后只填我们关心的字段。
    let mut nr: NETRESOURCEW = unsafe { std::mem::zeroed() };
    nr.dwScope = RESOURCE_GLOBALNET;
    nr.dwType = RESOURCETYPE_DISK;
    nr.dwUsage = RESOURCEUSAGE_CONTAINER;
    nr.lpRemoteName = remote.as_mut_ptr();
    let mut handle: *mut c_void = std::ptr::null_mut();
    // SAFETY: &nr 在调用期间有效;handle 是有效可写指针。
    let rc = unsafe { WNetOpenEnumW(RESOURCE_GLOBALNET, RESOURCETYPE_DISK, 0, &nr, &mut handle) };
    if rc != 0 {
        return out;
    }
    const CAP: usize = 256;
    // 用 Vec<NETRESOURCEW> 当缓冲 → 保证 8 字节对齐(结构含指针);枚举结果的字符串紧随结构尾部,
    // 指针回指本缓冲,只要 count<=CAP 就都在界内。
    let mut buf: Vec<NETRESOURCEW> = Vec::with_capacity(CAP);
    loop {
        let mut count: u32 = u32::MAX; // -1 = 尽量多塞
        let mut size: u32 = (CAP * std::mem::size_of::<NETRESOURCEW>()) as u32;
        // SAFETY: handle 有效;count/size 可写;buf 有 CAP 容量、size 与之匹配。
        let rc = unsafe {
            WNetEnumResourceW(
                handle,
                &mut count,
                buf.as_mut_ptr() as *mut c_void,
                &mut size,
            )
        };
        if rc != 0 || count == 0 {
            break; // ERROR_NO_MORE_ITEMS 或出错或本批为空
        }
        for i in 0..count as usize {
            // SAFETY: i<count<=CAP,entry 在缓冲界内;lpRemoteName 指向缓冲尾部的合法宽串。
            let entry = unsafe { &*buf.as_ptr().add(i) };
            if entry.lpRemoteName.is_null() {
                continue;
            }
            let name = unsafe { wide_ptr_to_string(entry.lpRemoteName) };
            if !name.is_empty() {
                out.push(name);
            }
        }
    }
    // SAFETY: handle 由 WNetOpenEnumW 取得,尚未关闭。
    unsafe {
        WNetCloseEnum(handle);
    }
    out
}

/// 读 NUL 结尾的宽字符串。SAFETY: 调用方保证 p 指向合法、以 NUL 结尾的宽串。
#[cfg(windows)]
unsafe fn wide_ptr_to_string(p: *const u16) -> String {
    let mut len = 0usize;
    while *p.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
}

/// 发现「已映射网络盘所在 NAS 主机上、尚未映射成盘符」的其它磁盘共享,补成盘点根。
#[cfg(windows)]
fn windows_discover_nas_shares() -> Vec<ScanRoot> {
    use std::collections::HashSet;
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    use windows_sys::Win32::System::WindowsProgramming::DRIVE_REMOTE;

    let mut roots = Vec::new();
    let mask = unsafe { GetLogicalDrives() };
    let mut mapped: HashSet<String> = HashSet::new(); // 已映射成盘符的共享 UNC(小写、去尾\)
    let mut hosts: Vec<String> = Vec::new(); // 去重的 \\host
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = format!("{letter}:\\");
        if unsafe { GetDriveTypeW(to_wide(&root).as_ptr()) } != DRIVE_REMOTE {
            continue;
        }
        let Some(unc) = win_drive_unc(letter) else {
            continue;
        };
        mapped.insert(unc.trim_end_matches('\\').to_lowercase());
        if let Some(host) = unc_host(&unc) {
            let hl = host.to_lowercase();
            if !hosts.iter().any(|h| h.to_lowercase() == hl) {
                hosts.push(host);
            }
        }
    }
    for host in &hosts {
        for share_unc in win_host_disk_shares(host) {
            let norm = share_unc.trim_end_matches('\\').to_lowercase();
            if mapped.contains(&norm) {
                continue; // 已映射(如 Z:=tx)→ 由盘符那条扫,别重复
            }
            let share_name = norm.rsplit('\\').next().unwrap_or("");
            // 跳过隐藏管理共享(C$ / ADMIN$ / IPC$ 等以 $ 结尾)。
            if share_name.is_empty() || share_name.ends_with('$') {
                continue;
            }
            let shown = share_unc.trim_end_matches('\\').to_string();
            let host_disp = unc_host(&shown).unwrap_or_default();
            roots.push(ScanRoot {
                id: shown.clone(),
                label: format!("NAS 共享 · {share_name}({host_disp})"),
                path: shown,
                kind: "mounted".into(),
                default_on: true,
            });
        }
    }
    roots
}

fn roots_impl() -> Vec<ScanRoot> {
    let mut out = Vec::new();

    #[cfg(target_os = "windows")]
    {
        out.extend(windows_roots());
    }

    #[cfg(target_os = "macos")]
    {
        // 个人文件夹常用子目录(默认勾)。HOME 整夹默认不勾(里面有 Library 噪音,按需再开)。
        if let Some(home) = home_dir() {
            for (sub, label) in [
                ("Desktop", "桌面"),
                ("Documents", "文稿"),
                ("Downloads", "下载"),
                ("Movies", "影片"),
                ("Music", "音乐"),
                ("Pictures", "图片"),
            ] {
                let p = home.join(sub);
                if p.exists() {
                    out.push(ScanRoot {
                        id: p.to_string_lossy().to_string(),
                        label: label.into(),
                        path: p.to_string_lossy().to_string(),
                        kind: "home".into(),
                        default_on: true,
                    });
                }
            }
        }
        // 外置 / 网络 / 磁盘映像卷:macOS 一律自动挂到 /Volumes,逐个收 —— 一个不落。
        if let Ok(rd) = fs::read_dir("/Volumes") {
            for e in rd.flatten() {
                let p = e.path();
                // 陈旧/掉线的 SMB/AFP 网络卷, is_dir()(底层 stat)可能阻塞数十秒 —— 会卡住
                // 「列可盘点的盘」这一步(它不在 inventory 的 probe 死线保护内)。故把 stat 放后台
                // 线程, 2s 内不回就当它「不响应」:仍列出(/Volumes 下条目本就是挂载卷)但默认不勾,
                // 真正扫描时由 inventory 自己的 dir_deadline 兜底。超时线程随 stat 自行结束(分离)。
                let (is_dir, responsive) = {
                    let pp = p.clone();
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let _ = tx.send(pp.is_dir());
                    });
                    match rx.recv_timeout(std::time::Duration::from_secs(2)) {
                        Ok(v) => (v, true),
                        Err(_) => (true, false),
                    }
                };
                if is_dir {
                    let name = e.file_name().to_string_lossy().to_string();
                    let label = if responsive {
                        format!("卷 · {name}")
                    } else {
                        format!("卷 · {name}(未响应)")
                    };
                    out.push(ScanRoot {
                        id: p.to_string_lossy().to_string(),
                        label,
                        path: p.to_string_lossy().to_string(),
                        kind: "volume".into(),
                        // 未响应的网络卷不默认勾, 避免拖慢首次盘点。
                        default_on: responsive,
                    });
                }
            }
        }
        // 系统盘根:可扫但默认不勾(避免一上来就遍历整块系统盘;OS 目录由 skip_dir_scan 跳)。
        out.push(ScanRoot {
            id: "/".into(),
            label: "系统盘 /".into(),
            path: "/".into(),
            kind: "drive".into(),
            default_on: false,
        });
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Linux / Docker:容器看不到宿主盘,只能扫 bind mount 进来的卷。
        // 工业级 = 读 /proc/mounts 发现**所有**真实挂载点(不再只认 5 个写死路径),
        // 这样用户无论把宿主盘 bind 到哪个容器路径都能被扫到。
        use std::collections::HashSet;
        let mut seen: HashSet<String> = HashSet::new();

        if let Some(home) = home_dir() {
            let p = home.to_string_lossy().to_string();
            if seen.insert(p.clone()) {
                out.push(ScanRoot {
                    id: p.clone(),
                    label: "工作区(HOME)".into(),
                    path: p,
                    kind: "home".into(),
                    default_on: true,
                });
            }
        }

        // /proc/mounts 发现的真实挂载 + 约定挂载点(/proc 不可读时兜底)。
        let mut mounts = proc_mount_roots();
        for cand in [
            "/root/Polaris/nas",
            "/data",
            "/mnt",
            "/media",
            "/volume1",
            "/host",
        ] {
            mounts.push(cand.to_string());
        }
        mounts.sort();
        mounts.dedup();
        for m in mounts {
            if !Path::new(&m).is_dir() || !seen.insert(m.clone()) {
                continue;
            }
            let label = Path::new(&m)
                .file_name()
                .map(|n| format!("挂载 · {}", n.to_string_lossy()))
                .unwrap_or_else(|| format!("挂载 · {m}"));
            out.push(ScanRoot {
                id: m.clone(),
                label,
                path: m,
                kind: "mounted".into(),
                default_on: true,
            });
        }
    }

    out
}

/// 读 `/proc/mounts` 发现所有「真实文件系统」的挂载点(容器里 = 从宿主 bind 进来的盘)。
/// 跳过伪文件系统(proc/sysfs/tmpfs/overlay…)、容器根 `/`、以及系统路径,只留可当资料盘扫的目录。
#[cfg(all(unix, not(target_os = "macos")))]
fn proc_mount_roots() -> Vec<String> {
    const PSEUDO_FS: &[&str] = &[
        "proc",
        "sysfs",
        "cgroup",
        "cgroup2",
        "tmpfs",
        "devtmpfs",
        "devpts",
        "mqueue",
        "overlay",
        "shm",
        "securityfs",
        "debugfs",
        "tracefs",
        "bpf",
        "pstore",
        "autofs",
        "binfmt_misc",
        "configfs",
        "fusectl",
        "hugetlbfs",
        "rpc_pipefs",
        "nsfs",
        "ramfs",
        "fuse.lxcfs",
        "squashfs",
    ];
    let Ok(text) = std::fs::read_to_string("/proc/mounts") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let _dev = it.next();
        let (Some(mnt_raw), Some(fstype)) = (it.next(), it.next()) else {
            continue;
        };
        if PSEUDO_FS.contains(&fstype) {
            continue;
        }
        let mnt = unescape_mount_octal(mnt_raw);
        if mnt == "/" {
            continue; // 容器根 overlay,扫它等于扫整个容器
        }
        // 系统路径黑名单:按**路径分量边界**匹配(相等或 `<sys>/…` 才算命中),不能用裸 starts_with
        // ——否则用户把宿主盘 bind 到名字恰好以系统前缀打头的容器路径(如 `/etcdata`、`/run-data`、
        // `/snapshots`、`/bootcamp`)会被误判成系统目录而整根排除,这些挂载里的文件于是「扫不到」。
        if [
            "/proc", "/sys", "/dev", "/run", "/etc", "/var/lib", "/snap", "/boot",
        ]
        .iter()
        .any(|p| {
            let p = p.trim_end_matches('/');
            mnt == p || mnt.starts_with(format!("{p}/").as_str())
        }) {
            continue;
        }
        if Path::new(&mnt).is_dir() {
            out.push(mnt);
        }
    }
    out
}

/// `/proc/mounts` 把空格等用八进制转义(`\040`=空格、`\011`=Tab、`\134`=反斜杠),还原之。
#[cfg(all(unix, not(target_os = "macos")))]
fn unescape_mount_octal(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1].is_ascii_digit()
            && b[i + 2].is_ascii_digit()
            && b[i + 3].is_ascii_digit()
        {
            if let Some(n) = std::str::from_utf8(&b[i + 1..i + 4])
                .ok()
                .and_then(|o| u8::from_str_radix(o, 8).ok())
            {
                out.push(n);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ───────────────────────── 黑/白名单 ─────────────────────────

/// 目录名(小写)命中即整棵剪掉。系统 / 缓存 / 依赖 / 敏感。
fn is_pruned_dir(name: &str) -> bool {
    // 以 . 或 @ 开头的目录一律跳过(配置/缓存/群晖系统目录,如 .git .ssh @appdata)
    if name.starts_with('.') || name.starts_with('@') || name.starts_with('$') {
        return true;
    }
    let n = name.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "windows"
            | "program files"
            | "program files (x86)"
            | "programdata"
            | "system volume information"
            | "recovery"
            | "appdata"
            | "node_modules"
            | "target"
            | "dist"
            | "build"
            | "__pycache__"
            | "venv"
            | "site-packages"
            | "vendor"
            | "obj"
            | "bin"
            | "anaconda3"
            | "miniconda3"
            | "library"        // mac ~/Library
            | "applications"
            | "polariskb" // 别把知识库自己扫进来
    )
}

/// 后缀 → 类型;不在表内返回 None(跳过)。
fn classify_ext(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "pdf" | "doc" | "docx" | "rtf" | "odt" | "pages" => "doc",
        "md" | "markdown" | "txt" => "text",
        "xls" | "xlsx" | "csv" | "tsv" | "ods" | "numbers" => "sheet",
        "ppt" | "pptx" | "key" | "odp" => "slide",
        "json" | "xml" | "yaml" | "yml" | "parquet" | "sqlite" | "db" | "ndjson" => "data",
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "tiff" | "tif" | "bmp" | "svg" => {
            "image"
        }
        "mp3" | "wav" | "flac" | "m4a" | "aac" | "ogg" => "audio",
        "mp4" | "mov" | "mkv" | "avi" | "webm" | "m4v" => "video",
        "zip" | "7z" | "rar" | "tar" | "gz" | "bz2" | "xz" => "archive",
        "py" | "js" | "ts" | "tsx" | "jsx" | "rs" | "go" | "java" | "c" | "cpp" | "h" | "hpp"
        | "sh" | "html" | "css" | "vue" | "sql" | "ipynb" | "toml" | "ini" => "code",
        _ => return None,
    })
}

/// 文本类(可直接读头部当预览)。
fn is_textual(kind: &str) -> bool {
    matches!(kind, "text" | "code" | "sheet" | "data")
    // 仅当真是文本(下方按扩展再判 csv/json/txt/md/code)
}

// ───────────────────────── 预览 / 评分 ─────────────────────────

fn human_size(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// 读文件头部若干字节(只读,不整文件入内存)。
fn read_head(path: &Path, max: usize) -> Option<String> {
    let f = fs::File::open(path).ok()?;
    let mut buf = vec![0u8; max];
    let mut h = f.take(max as u64);
    let n = h.read(&mut buf).ok()?;
    buf.truncate(n);
    Some(String::from_utf8_lossy(&buf).to_string())
}

/// 启发式「大概内容」。文本类读头部取前几行;其它给类型占位(待 AI 摘要)。
fn make_preview(path: &Path, ext: &str, kind: &str, size: u64) -> String {
    let textual = matches!(
        ext,
        "txt" | "md" | "markdown" | "csv" | "tsv" | "json" | "ndjson"
    ) || (kind == "code");
    if textual {
        if let Some(head) = read_head(path, 4096) {
            if ext == "csv" || ext == "tsv" {
                // 表头一行 + 列数
                if let Some(first) = head.lines().find(|l| !l.trim().is_empty()) {
                    let sep = if ext == "tsv" { '\t' } else { ',' };
                    let cols = first.split(sep).count();
                    let h: String = first.chars().take(80).collect();
                    return format!("{cols} 列:{h}…");
                }
            }
            let snippet: String = head
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .take(3)
                .collect::<Vec<_>>()
                .join(" · ");
            let snippet = snippet.replace(['\u{0}', '\r'], "");
            let snippet: String = snippet.chars().take(140).collect();
            if !snippet.trim().is_empty() {
                return snippet;
            }
        }
    }
    // binary / 不可直读 → 类型占位
    let label = match kind {
        "doc" => "文档",
        "slide" => "演示文稿",
        "image" => "图片",
        "audio" => "音频",
        "video" => "视频",
        "archive" => "压缩包",
        "data" => "数据文件",
        _ => "文件",
    };
    format!("{label} · {}（点「智能摘要」识别内容）", human_size(size))
}

/// 价值评分 1-5(位置 / 时间 / 命名 / 体积)。
fn score_row(path: &Path, name: &str, kind: &str, size: u64, mtime: i64, now: i64) -> u8 {
    let mut s: i32 = 3;
    let lower = path.to_string_lossy().to_ascii_lowercase();
    // 位置:常见有用目录加分
    if [
        "desktop",
        "documents",
        "downloads",
        "文档",
        "桌面",
        "工作",
        "项目",
        "report",
        "report",
    ]
    .iter()
    .any(|k| lower.contains(k))
    {
        s += 1;
    }
    // 时效:近半年 +1,超三年 -1
    let age = now - mtime;
    if age >= 0 && age < 60 * 60 * 24 * 180 {
        s += 1;
    } else if age > 60 * 60 * 24 * 365 * 3 {
        s -= 1;
    }
    // 命名噪音
    let nl = name.to_ascii_lowercase();
    if [
        "新建",
        "未命名",
        "untitled",
        "tmp",
        "temp",
        "copy",
        "副本",
        "~$",
        "新建文本",
    ]
    .iter()
    .any(|k| nl.contains(k))
    {
        s -= 2;
    }
    // 体积
    if size == 0 {
        s -= 2;
    }
    // 文档/演示天然偏有用
    if matches!(kind, "doc" | "slide") {
        s += 1;
    }
    s.clamp(1, 5) as u8
}

fn suggest_for(score: u8, kind: &str) -> &'static str {
    if score <= 2 {
        "skip"
    } else if score >= 4 && matches!(kind, "doc" | "slide" | "text") {
        "resource+core"
    } else {
        "resource"
    }
}

/// 路径稳定 id(简单哈希)。
fn path_id(path: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    format!("{:x}", h.finish())
}

// ───────────────────────── 命令 ─────────────────────────

/// **`(async)`**:根枚举含对各挂载点的网络探测(死 NAS 上一个根就能卡几秒)。同步 tauri
/// 命令跑在主线程会冻住 UI → 派到工作线程(签名不变,前端零改动)。
#[cfg_attr(feature = "desktop", tauri::command(async))]
pub fn scan_roots() -> Vec<ScanRoot> {
    roots_impl()
}

/// 扫描给定根下的有用资源。只读;返回多维表格行。
/// max: 命中上限(默认 20000),达到即截断,防止极端目录拖死。
///
/// **`(async)`**:全盘 WalkDir 深度 14、上限可到 20 万文件,分钟级 IO;同步命令会把这段
/// 活儿钉死在主线程上(扫描期间整个 UI 冻住)→ 同 fable_audit 一样派到工作线程。
#[cfg_attr(feature = "desktop", tauri::command(async))]
pub fn scan_resources(roots: Vec<String>, max: Option<usize>) -> Result<ScanReport, String> {
    if roots.is_empty() {
        return Err("未选择扫描范围".into());
    }
    let cap = max.unwrap_or(20_000).clamp(100, 200_000);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mut rows: Vec<ScanRow> = Vec::new();
    let mut total_seen: u64 = 0;
    let mut skipped: u64 = 0;
    let mut truncated = false;

    'outer: for root in &roots {
        let rp = Path::new(root);
        if !rp.exists() {
            continue;
        }
        let walker = WalkDir::new(rp)
            .max_depth(14)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                // 目录:命中黑名单则整棵剪掉
                if e.file_type().is_dir() && e.depth() > 0 {
                    let name = e.file_name().to_string_lossy();
                    return !is_pruned_dir(&name);
                }
                true
            });

        for entry in walker.filter_map(|r| r.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            total_seen += 1;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            let kind = match classify_ext(&ext) {
                Some(k) => k,
                None => {
                    skipped += 1;
                    continue;
                }
            };
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            let size = meta.len();
            // 过滤明显的图标/缩略图碎图
            if kind == "image" && size < 20 * 1024 {
                skipped += 1;
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);

            let preview = make_preview(path, &ext, kind, size);
            let score = score_row(path, &name, kind, size, mtime, now);
            let suggest = suggest_for(score, kind);
            let p = path.to_string_lossy().to_string();

            rows.push(ScanRow {
                id: path_id(&p),
                path: p,
                name,
                ext,
                kind: kind.to_string(),
                preview,
                size,
                size_h: human_size(size),
                mtime,
                score,
                suggest: suggest.to_string(),
            });

            if rows.len() >= cap {
                truncated = true;
                break 'outer;
            }
        }
    }

    // 默认按价值降序、再按修改时间降序
    rows.sort_by(|a, b| b.score.cmp(&a.score).then(b.mtime.cmp(&a.mtime)));
    let hit = rows.len();
    let _ = is_textual; // 预留:后续真正分流文本/二进制预览

    Ok(ScanReport {
        rows,
        total_seen,
        hit,
        skipped,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 工业级盘符枚举:GetLogicalDrives 报告的每一个盘符都必须出现在 scan_roots 里,
    /// 一个不落(这是用户的硬要求)。同时回归「桌面」始终在列。
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_lists_every_logical_drive() {
        use windows_sys::Win32::Storage::FileSystem::GetLogicalDrives;
        let roots = roots_impl();
        // 打印实测枚举结果,便于人工核对盘标/盘型。
        for r in &roots {
            eprintln!("[scan_root] {:<10} {}", r.kind, r.label);
        }
        let mask = unsafe { GetLogicalDrives() };
        for i in 0..26u32 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let want = format!("{}:\\", (b'A' + i as u8) as char);
            assert!(
                roots.iter().any(|r| r.path == want),
                "GetLogicalDrives 报告了 {want} 但 scan_roots 漏掉了它"
            );
        }
        // 没有任何盘符时也不该 panic;有盘符则至少有一个 drive 根。
        if mask != 0 {
            assert!(roots.iter().any(|r| r.kind == "drive"));
        }
    }

    /// 真机探查(默认 ignore,需连着 NAS 时手动 `--ignored --nocapture` 跑):打印自动发现的
    /// 「同一台 NAS 上未映射成盘符」的其它共享。用于验证 WNet 枚举真能捞到 docker/web 等共享。
    #[cfg(target_os = "windows")]
    #[test]
    #[ignore]
    fn windows_discovers_sibling_nas_shares() {
        let shares = windows_discover_nas_shares();
        eprintln!("发现 {} 个未映射的 NAS 共享:", shares.len());
        for s in &shares {
            eprintln!("  [{}] {} -> {}", s.kind, s.label, s.path);
        }
    }
}
