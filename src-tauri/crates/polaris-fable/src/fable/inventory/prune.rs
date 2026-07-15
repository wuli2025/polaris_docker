/// 扫描时跳过的目录名(系统/缓存/版本仓;@eaDir、#recycle 是群晖特产)。
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".svn",
    "node_modules",
    "target",
    ".fable",
    ".history",
    ".quarantine",
    "__pycache__",
    ".venv",
    "venv",
    "$RECYCLE.BIN",
    "System Volume Information",
    ".Trash",
    ".Trashes",
    "@eaDir",
    "#recycle",
    "#snapshot",
    ".DocumentRevisions-V100",
    ".Spotlight-V100",
];

fn skip_dir(name: &str) -> bool {
    // 群晖/NAS 系统目录一律以 `@` 或 `#` 打头(@eaDir 缩略图、@docker 层、@database、
    // @appstore、#recycle、#snapshot…),用户数据从不放这里 → 整盘盘点时跳过,免噪音免爆量。
    if name.starts_with('@') || name.starts_with('#') {
        return true;
    }
    SKIP_DIRS.iter().any(|s| s.eq_ignore_ascii_case(name))
}

/// 盘点支持「不只盘知识库,也能盘整盘/桌面/其它文件夹」后,扫描会触达 C:/D: 这类系统盘。
/// 这些操作系统/缓存/依赖目录用户数据从不放、且体量巨大 → 扫文件夹和盘点时都整棵跳过,
/// 避免把 Windows、Program Files 卷进文件库。在 [`skip_dir`] 基础上再加一层系统目录黑名单。
const SCAN_EXTRA_SKIP: &[&str] = &[
    "windows",
    "program files",
    "program files (x86)",
    "programdata",
    "perflogs",
    "msocache",
    "$recycle.bin",
    "system volume information",
    "recovery",
    "appdata",
    "$windows.~bs",
    "$windows.~ws",
    "intel",
    "amd",
    "nvidia",
    "site-packages",
    "anaconda3",
    "miniconda3",
    "library",
    "applications",
    "boot",
    "proc",
    "sys",
    "dev",
    // macOS 根级系统目录(整盘扫 `/` 时才剪):/System 密封系统卷、/private(var/tmp/etc)、
    // /cores 崩溃转储、/Network、/automount —— 全是系统态,扫进去既极慢又毫无用户文档。
    "system",
    "private",
    "cores",
    "network",
    "automount",
];

/// macOS「包/库目录」——以扩展名结尾、在 Finder 里显示成**单个文件**、内部却塞着成千上万份
/// 资源的目录:应用 `.app`、框架 `.framework`、媒体库 `.photoslibrary`/`.fcpbundle`…。
/// 用户从不想把它们的内部资源逐个归类进库;不跳的话(尤其 `~/Pictures` 下的 `.photoslibrary`
/// 动辄**十万级**文件、`/Applications` 里每个 `.app` 又有几千文件)会让**每次盘点慢上数倍**、
/// 还把文件库塞满机器味碎文件。整棵跳过 —— 任何平台同名目录(含拷到 NAS/Win 上的)都几乎
/// 一定是 mac 包,跳了无害。这是 macOS 盘点慢的头号来源。
pub(crate) fn is_macos_package_dir(name: &str) -> bool {
    const PKG_EXT: &[&str] = &[
        ".app",
        ".framework",
        ".bundle",
        ".appex",
        ".dsym",
        ".xcarchive",
        ".xcassets",
        ".xcodeproj",
        ".photoslibrary",
        ".fcpbundle",
        ".imovielibrary",
        ".tvlibrary",
        ".aplibrary",
        ".musiclibrary",
    ];
    let low = name.to_ascii_lowercase();
    PKG_EXT.iter().any(|e| low.ends_with(e))
}

/// 「永远跳过」的目录:版本仓 / 依赖 / 回收站 / NAS 系统目录(`@`/`#`)/ `$` 系统目录 /
/// macOS 包目录([`is_macos_package_dir`])。这些从来不是用户文档,任何根、任何深度都跳——
/// 即便用户显式选了某文件夹也不会想要它们。
pub(crate) fn skip_dir_always(name: &str) -> bool {
    skip_dir(name) || name.starts_with('$') || is_macos_package_dir(name)
}

/// 「仅整盘扫描时叠加」的操作系统目录黑名单(windows / program files / appdata / library …)。
/// 这些名字在系统盘根下是 OS 目录该跳;但在用户自己挑的文件夹里同名子目录(如一个叫
/// `library` 的资料夹)却是真数据——所以只在扫整块系统盘(见 [`is_os_disk_root`])时才剪。
pub(crate) fn skip_dir_os(name: &str) -> bool {
    let low = name.to_ascii_lowercase();
    SCAN_EXTRA_SKIP.iter().any(|s| *s == low)
}

/// 重剪 = 永远跳 + OS 目录。给文件夹选择器顶层与嵌套根去重([`covered_by`])用,保守。
pub(crate) fn skip_dir_scan(name: &str) -> bool {
    skip_dir_always(name) || skip_dir_os(name)
}

/// 这个根是不是「一整块系统盘」:Windows 盘符根(`C:\`)或 Unix 根(`/`)。
/// 只有这种根扫描时才叠加 OS 目录黑名单;用户显式挑的某个文件夹、外置卷、NAS 挂载点都不是,
/// 它们一律「文件夹里的全归类进库」(只剪永远跳的版本仓/回收站噪音)。
pub(crate) fn is_os_disk_root(path: &str) -> bool {
    let p = path.trim();
    if p == "/" {
        return true;
    }
    let t = p.trim_end_matches(['/', '\\']);
    let b = t.as_bytes();
    b.len() == 2 && b[1] == b':' && b[0].is_ascii_alphabetic()
}

/// 这个根是不是「网络/远程盘」:Windows 把群晖/NAS 共享映射成的盘符(如 `Z:`)、或 UNC
/// (`\\server\share`)都属于此类。判定用 `GetDriveTypeW == DRIVE_REMOTE`。
///
/// 为什么要单独认它:[`is_os_disk_root`] 只看路径形状,会把 `Z:\` 也判成「一整块系统盘」→
/// 于是盘点用 `heavy_prune` 模式,把任何名叫 library/system/private/bin/boot… 的目录(NAS 上
/// 很常见的用户共享名)在**任意深度**整棵剪掉 → 大量 NAS 数据被静默丢弃、扫不全。映射进来的
/// NAS 盘其实等同「外置卷/挂载点」,应当「里面的全归类进库」,只剪永远跳的噪音(@eaDir/#recycle)。
/// 所以盘点时 `heavy_prune = is_os_disk_root && !is_remote_root`,远程盘退回轻剪。
/// 非 Windows 一律 false(mac/Docker 的 NAS 走 /Volumes、bind mount,本就不当系统盘重剪)。
#[cfg(windows)]
pub(crate) fn is_remote_root(path: &str) -> bool {
    let t = path.trim();
    if t.starts_with("\\\\") || t.starts_with("//") {
        return true; // UNC 路径 = 网络盘
    }
    let b = t.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
        use windows_sys::Win32::System::WindowsProgramming::DRIVE_REMOTE;
        let drive = format!("{}:\\", b[0] as char); // GetDriveType 要盘符根
        let wide: Vec<u16> = std::ffi::OsStr::new(&drive)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: wide 是 NUL 结尾的合法宽字符串。
        return unsafe { GetDriveTypeW(wide.as_ptr()) == DRIVE_REMOTE };
    }
    false
}

#[cfg(not(windows))]
pub(crate) fn is_remote_root(_path: &str) -> bool {
    false
}

/// 盘 `parent` 时是否真能扫到 `child`:child 在 parent 之内,且从 parent 到 child 的
/// 每一段目录名都不会被 [`skip_dir_scan`] 剪掉(否则 walker 会在中途剪枝、到不了 child)。
/// 嵌套根去重用它:被剪枝挡住的子根不算「已覆盖」,须独立保留(典型=appdata 内的下载目录)。
pub(crate) fn covered_by(parent: &str, child: &str) -> bool {
    // Windows 路径不分大小写("C:\Data" 与 "c:\data" 是同一目录)→ 统一小写再比
    // (对齐 files/overview.rs maximal_root_ids 的口径),否则前缀判不出嵌套、同一棵树被盘两遍。
    // 段名喂给 skip_dir_scan 前被小写不影响判定:其内部各黑名单本就大小写无关。
    let norm = |p: &str| {
        if cfg!(windows) {
            p.to_lowercase()
        } else {
            p.to_string()
        }
    };
    let (parent, child) = (norm(parent), norm(child));
    if child == parent {
        return true;
    }
    let pn = parent.trim_end_matches(['/', '\\']);
    let rest = match child
        .strip_prefix(&format!("{pn}/"))
        .or_else(|| child.strip_prefix(&format!("{pn}\\")))
    {
        Some(s) => s,
        None => return false,
    };
    !rest
        .split(['/', '\\'])
        .any(|seg| !seg.is_empty() && skip_dir_scan(seg))
}
