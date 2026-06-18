//! 盘点引擎(L1a)—— 多线程并行全盘扫描 → SQLite。
//!
//! PRD v5 §7「P0.5 盘点+L1a:首小时全盘可搜」。设计:
//! - N 个 walker 线程(共享目录栈,work-stealing)只做 read_dir + stat,吃满多核;
//! - 1 个 writer 线程独占写连接,2000 行一个事务批量落库(SQLite 写入瓶颈在事务数);
//! - 「seen 代际」机制:全量重扫后自动清掉已消失文件(及其 chunks),幂等可重入;
//! - mtime/size 没变的文件保留 chunked 标记 → 重扫不会废掉已建好的向量索引。

use super::{cancelled, open_db, worker_count, FlagGuard, CANCEL, SCANNING};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use super::sched::WorkQueue;

/// 单个目录从被取出到扫完(read_dir + 逐项 stat)的「死线」:超过此值看门狗判定该 worker
/// 卡死(NAS 挂载掉线 / 网络盘僵死 / 权限挂起),记账释放、把目录列入「已跳过」,盘点照常完成。
/// 取值要远大于任何正常目录(本地盘 read_dir 毫秒级,慢 NAS 大目录秒级),只兜真·僵死。
/// 可经 `POLARIS_SCAN_DIR_DEADLINE_SECS` 调(NAS 极慢时调大,本地想更快兜底调小)。
fn dir_deadline() -> Duration {
    let secs = std::env::var("POLARIS_SCAN_DIR_DEADLINE_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&s| s >= 5)
        .unwrap_or(60);
    Duration::from_secs(secs)
}

/// 读目录瞬断(权限抖动 / NAS 短暂不可达)时,把该目录降级到队尾重试的最大次数;超次即列为
/// 「已跳过」。重试是「调到最后再试」,不原地阻塞别人 —— 见 [`WorkQueue::demote`]。
const MAX_DIR_ATTEMPTS: u32 = 2;

/// 挂载点「可达性探测」的死线(秒)。映射的 NAS/网络盘**冷连接**首个 `is_dir`/`read_dir` 常要
/// 数秒(SMB 握手 + 唤醒休眠的群晖硬盘)——旧实现一律卡 3s 判不可达,会把「其实活着、只是第一
/// 下慢」的 NAS 盘直接挡在盘点与选择器之外(用户「Z 盘采集不到」的根因之一)。放宽到默认 12s,
/// 可经 `POLARIS_SCAN_PROBE_SECS` 调。注:这只是「要不要尝试这个根」的预检,真扫起来还有
/// 每目录看门狗兜底,放宽预检不会让死 NAS 把盘点拖死。
fn probe_secs() -> u64 {
    std::env::var("POLARIS_SCAN_PROBE_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&s| s >= 2)
        .unwrap_or(12)
}

/// 每个 worker 的心跳:它正卡在哪个目录(`None`=空闲)、是否已被看门狗判定卡死。
/// 「卡了多久」不再记在这里 —— 改由看门狗结合 worker 的「已处理目录项计数」自行判断,
/// 这样「目录大但仍在稳定吐项」与「真·僵死(计数纹丝不动)」能被区分开(见盘点看门狗)。
struct Beat {
    dir: Option<PathBuf>,
    abandoned: bool,
}

#[cfg(feature = "desktop")]
use tauri::{AppHandle, Emitter};
#[cfg(not(feature = "desktop"))]
use crate::host::AppHandle;

// ───────────────────────── 文件分类 ─────────────────────────

const TEXT_EXTS: &[&str] = &[
    "md", "txt", "rs", "py", "js", "ts", "tsx", "jsx", "mjs", "json", "jsonl", "yaml", "yml",
    "toml", "html", "htm", "css", "csv", "tsv", "log", "xml", "ini", "cfg", "conf", "sh", "ps1",
    "bat", "cmd", "sql", "vue", "go", "java", "c", "cpp", "h", "hpp", "rb", "php", "srt", "vtt",
    "tex", "rst", "org",
];
const DOC_EXTS: &[&str] = &["pdf", "docx", "doc", "pptx", "ppt", "xlsx", "xls", "epub", "mobi"];
const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "heic", "svg", "tif", "tiff", "raw", "cr2", "nef",
];
const AUDIO_EXTS: &[&str] = &["mp3", "wav", "flac", "m4a", "aac", "ogg", "wma", "opus", "amr"];
const VIDEO_EXTS: &[&str] = &["mp4", "mkv", "mov", "avi", "wmv", "flv", "webm", "m4v", "mpg", "mpeg"];
const ARCHIVE_EXTS: &[&str] = &["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "iso", "dmg"];

pub(crate) fn classify(ext: &str) -> &'static str {
    let e = ext.to_ascii_lowercase();
    let e = e.as_str();
    if TEXT_EXTS.contains(&e) {
        "text"
    } else if DOC_EXTS.contains(&e) {
        "doc"
    } else if IMAGE_EXTS.contains(&e) {
        "image"
    } else if AUDIO_EXTS.contains(&e) {
        "audio"
    } else if VIDEO_EXTS.contains(&e) {
        "video"
    } else if ARCHIVE_EXTS.contains(&e) {
        "archive"
    } else {
        "other"
    }
}

// ───────────────────────── 按「语言」归类 ─────────────────────────
//
// 用户诉求:文件归类要「按语言」(编程语言 / 自然语言),不要按应用名或粗粒度类型。
// 三层判定:① 代码/标记类 → 编程语言(扩展名精确判定,零 IO);② 媒体/压缩 → 大类;
// ③ 文稿(md/txt/doc…)→ 自然语言(读文件头按 CJK 占比嗅探,放在回填里做,避免拖慢盘点)。

/// 扩展名 → 编程语言/标记语言(「按语言归类」的精确信号,零 IO)。None = 非代码类。
pub(crate) fn prog_lang(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "py" | "pyw" | "pyi" | "ipynb" => "Python",
        "rs" => "Rust",
        "js" | "mjs" | "cjs" => "JavaScript",
        "ts" => "TypeScript",
        "tsx" | "jsx" => "React/JSX",
        "vue" => "Vue",
        "go" => "Go",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "c" | "h" => "C",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "C++",
        "cs" => "C#",
        "rb" => "Ruby",
        "php" => "PHP",
        "swift" => "Swift",
        "scala" => "Scala",
        "sh" | "bash" | "zsh" => "Shell",
        "ps1" | "psm1" | "psd1" => "PowerShell",
        "bat" | "cmd" => "Batch",
        "sql" => "SQL",
        "r" => "R",
        "lua" => "Lua",
        "dart" => "Dart",
        "pl" | "pm" => "Perl",
        "html" | "htm" => "HTML",
        "css" | "scss" | "sass" | "less" => "CSS/样式",
        "json" | "jsonl" | "ndjson" => "JSON",
        "yaml" | "yml" => "YAML",
        "toml" | "ini" | "cfg" | "conf" => "配置",
        "xml" => "XML",
        _ => return None,
    })
}

/// kind → 媒体/压缩大类(非代码、非文稿的语言归类兜底)。None = 文稿类(交自然语言嗅探)。
fn media_lang(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "image" => "图片",
        "video" => "视频",
        "audio" => "音频",
        "archive" => "压缩包",
        "other" => "其他文件",
        _ => return None, // text / doc → 文稿,按自然语言归
    })
}

/// 全部受支持的代码/标记扩展名(grid 按语言反查用)。与 [`prog_lang`] 的 match 同源。
pub(crate) const CODE_EXTS: &[&str] = &[
    "py", "pyw", "pyi", "ipynb", "rs", "js", "mjs", "cjs", "ts", "tsx", "jsx", "vue", "go", "java",
    "kt", "kts", "c", "h", "cpp", "cc", "cxx", "hpp", "hh", "hxx", "cs", "rb", "php", "swift",
    "scala", "sh", "bash", "zsh", "ps1", "psm1", "psd1", "bat", "cmd", "sql", "r", "lua", "dart",
    "pl", "pm", "html", "htm", "css", "scss", "sass", "less", "json", "jsonl", "ndjson", "yaml",
    "yml", "toml", "ini", "cfg", "conf", "xml",
];

/// 某编程/标记语言 → 对应扩展名集合(grid 按语言过滤;代码语言由扩展名确定,不依赖回填)。
/// 空 = 该标签不是代码语言(改按 lang 列 / kind 过滤)。
pub(crate) fn exts_for_lang(label: &str) -> Vec<&'static str> {
    CODE_EXTS.iter().copied().filter(|e| prog_lang(e) == Some(label)).collect()
}

/// 媒体/压缩语言标签 → 对应 kind(grid 过滤用)。None = 非媒体标签。
pub(crate) fn kind_for_media_lang(label: &str) -> Option<&'static str> {
    Some(match label {
        "图片" => "image",
        "视频" => "video",
        "音频" => "audio",
        "压缩包" => "archive",
        "其他文件" => "other",
        _ => return None,
    })
}

/// 盘点时即可定的语言(零 IO):代码看扩展名、媒体看 kind;文稿返回 ""(留待回填读头嗅探)。
pub(crate) fn quick_lang(ext: &str, kind: &str) -> String {
    if let Some(l) = prog_lang(ext) {
        return l.to_string();
    }
    media_lang(kind).unwrap_or("").to_string()
}

/// 读文件头嗅探自然语言:CJK 占比 ≥10% → 中文;拉丁字母为主 → 英文;否则其他语言。
pub(crate) fn natural_lang(sample: &str) -> &'static str {
    let (mut cjk, mut latin, mut letters) = (0usize, 0usize, 0usize);
    for c in sample.chars().take(8000) {
        if ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{3400}'..='\u{4dbf}').contains(&c) {
            cjk += 1;
            letters += 1;
        } else if c.is_ascii_alphabetic() {
            latin += 1;
            letters += 1;
        } else if c.is_alphabetic() {
            letters += 1;
        }
    }
    if letters < 8 {
        return "其他语种";
    }
    if cjk as f32 / letters as f32 >= 0.10 {
        "中文"
    } else if latin as f32 / letters as f32 >= 0.6 {
        "英文"
    } else {
        "其他语种"
    }
}

/// 读文件头(≤16KB)做文本采样;二进制(含 NUL)或不可读返回 None。
pub(crate) fn read_head_sample(abs: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(abs).ok()?;
    let mut buf = vec![0u8; 16 * 1024];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    if buf.iter().take(1024).any(|&b| b == 0) {
        return None; // 二进制(含改名的伪文本 / docx/pdf 等容器)
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// 扫描时跳过的目录名(系统/缓存/版本仓;@eaDir、#recycle 是群晖特产)。
const SKIP_DIRS: &[&str] = &[
    ".git", ".svn", "node_modules", "target", ".fable", ".history", ".quarantine", "__pycache__",
    ".venv", "venv", "$RECYCLE.BIN", "System Volume Information", ".Trash", ".Trashes",
    "@eaDir", "#recycle", "#snapshot", ".DocumentRevisions-V100", ".Spotlight-V100",
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
    "windows", "program files", "program files (x86)", "programdata", "perflogs", "msocache",
    "$recycle.bin", "system volume information", "recovery", "appdata", "$windows.~bs",
    "$windows.~ws", "intel", "amd", "nvidia", "site-packages", "anaconda3", "miniconda3",
    "library", "applications", "boot", "proc", "sys", "dev",
    // macOS 根级系统目录(整盘扫 `/` 时才剪):/System 密封系统卷、/private(var/tmp/etc)、
    // /cores 崩溃转储、/Network、/automount —— 全是系统态,扫进去既极慢又毫无用户文档。
    "system", "private", "cores", "network", "automount",
];

/// macOS「包/库目录」——以扩展名结尾、在 Finder 里显示成**单个文件**、内部却塞着成千上万份
/// 资源的目录:应用 `.app`、框架 `.framework`、媒体库 `.photoslibrary`/`.fcpbundle`…。
/// 用户从不想把它们的内部资源逐个归类进库;不跳的话(尤其 `~/Pictures` 下的 `.photoslibrary`
/// 动辄**十万级**文件、`/Applications` 里每个 `.app` 又有几千文件)会让**每次盘点慢上数倍**、
/// 还把文件库塞满机器味碎文件。整棵跳过 —— 任何平台同名目录(含拷到 NAS/Win 上的)都几乎
/// 一定是 mac 包,跳了无害。这是 macOS 盘点慢的头号来源。
fn is_macos_package_dir(name: &str) -> bool {
    const PKG_EXT: &[&str] = &[
        ".app", ".framework", ".bundle", ".appex", ".dsym", ".xcarchive", ".xcassets",
        ".xcodeproj", ".photoslibrary", ".fcpbundle", ".imovielibrary", ".tvlibrary",
        ".aplibrary", ".musiclibrary",
    ];
    let low = name.to_ascii_lowercase();
    PKG_EXT.iter().any(|e| low.ends_with(e))
}

/// 「永远跳过」的目录:版本仓 / 依赖 / 回收站 / NAS 系统目录(`@`/`#`)/ `$` 系统目录 /
/// macOS 包目录([`is_macos_package_dir`])。这些从来不是用户文档,任何根、任何深度都跳——
/// 即便用户显式选了某文件夹也不会想要它们。
fn skip_dir_always(name: &str) -> bool {
    skip_dir(name) || name.starts_with('$') || is_macos_package_dir(name)
}

/// 「仅整盘扫描时叠加」的操作系统目录黑名单(windows / program files / appdata / library …)。
/// 这些名字在系统盘根下是 OS 目录该跳;但在用户自己挑的文件夹里同名子目录(如一个叫
/// `library` 的资料夹)却是真数据——所以只在扫整块系统盘(见 [`is_os_disk_root`])时才剪。
fn skip_dir_os(name: &str) -> bool {
    let low = name.to_ascii_lowercase();
    SCAN_EXTRA_SKIP.iter().any(|s| *s == low)
}

/// 重剪 = 永远跳 + OS 目录。给文件夹选择器顶层与嵌套根去重([`covered_by`])用,保守。
fn skip_dir_scan(name: &str) -> bool {
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
        let wide: Vec<u16> =
            std::ffi::OsStr::new(&drive).encode_wide().chain(std::iter::once(0)).collect();
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
fn covered_by(parent: &str, child: &str) -> bool {
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

// ───────────────────────── 扫描核心(三壳共用)─────────────────────────

struct FileRow {
    relpath: String,
    name: String,
    ext: String,
    kind: &'static str,
    /// 「按语言归类」标签:代码=编程语言、媒体=大类;文稿盘点时为 ""(回填读头嗅探自然语言)。
    lang: String,
    size: u64,
    mtime: i64,
}

/// walker → writer 的消息(增量盘点三态)。
enum Msg {
    /// 一个文件:照常 upsert 进 files(mtime/size 没变则保留 chunked/ftsed 标记)。
    File(FileRow),
    /// 一个**改过/新**的目录:read_dir 完后记下它的 mtime + 直属文件数/字节,供下次增量比对。
    Dir { rel: String, mtime: i64, fcount: i64, fbytes: i64 },
    /// 一个**没变**的目录(mtime 命中缓存,整棵跳过 read_dir):把它的直属文件标记「本轮见过」
    /// (否则代际对账会把它们误判消失而删),并 touch 自己的 dirs 行(mtime 不变,只刷 seen)。
    Skip { rel: String },
}

// 工作单元 = `(待扫目录绝对路径, 它的 mtime)`。mtime 由父目录的 read_dir 顺手带出,免再 stat;
// 0 = 未知(根 / 跳过路径排进来的子目录),pop 时现 stat 一次。增量盘点据 mtime 判定「变没变」。

/// 一个目录的修改时间(Unix 秒;读不到返回 0)。增量盘点据此判定「这个目录变没变」。
fn dir_mtime(p: &Path) -> i64 {
    std::fs::metadata(p)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 目录绝对路径 → 相对盘点根的路径('/' 分隔;根自身 = "")。dirs 缓存的主键就是它。
fn rel_of(p: &Path, root: &Path) -> String {
    p.strip_prefix(root)
        .ok()
        .map(|r| super::decode_fs(r.as_os_str()).replace('\\', "/"))
        .unwrap_or_default()
}

/// 相对路径的父目录(顶层 / 根 → "")。从 dirs 缓存的全部 key 反推「父→子目录」邻接表用。
fn parent_rel(rel: &str) -> &str {
    match rel.rfind('/') {
        Some(i) => &rel[..i],
        None => "",
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanSummary {
    pub root: String,
    pub files: u64,
    pub bytes: u64,
    pub removed: u64,
    /// 因不可达(挂载掉线 / 权限挂起 / 反复读失败)被看门狗判定卡死、降级后仍失败而**跳过**的目录数。
    /// >0 表示这次盘点没冻死、但有部分目录没扫到(常因 NAS 掉线),如实上报供用户重扫。
    pub skipped: u64,
    pub seconds: f64,
    pub workers: usize,
}

/// 同步扫描一个根(CLI 直接调;桌面/Docker 由 `fable_inventory_start` 包后台线程)。
/// `progress(files, bytes)` 每 ~5000 个文件回调一次。
/// `exclude` = 用户在「扫描」步骤里取消勾选的文件夹绝对路径集合,整棵跳过(空集=全盘点)。
///
/// `full=false`(默认/智能增量):重扫时**目录 mtime 命中 dirs 缓存就整棵跳过 read_dir**——
/// 该目录的增删改名必然没变(目录 mtime 在直属项变动时才刷新),只把直属文件标记「还在」、
/// 递归进子目录继续比对。改过的目录才真正 read_dir。第一次盘点无缓存 → 等同全量。
/// 唯一抓不到的:某文件被「原地追加写入、且不碰其所在目录」(罕见,如日志续写)——其内容
/// 变了但增量察觉不到,要等一次 `full=true` 才更新。`full=true`(完整盘点):忽略缓存,
/// 每个目录都 read_dir,顺带刷新 dirs 缓存供下次增量。
///
/// 剪枝分两档(治「文件夹里的东西要全归类进库」):
/// - **永远跳**(版本仓 / 依赖 / 回收站 / NAS / `$` 系统目录):任何根都跳。
/// - **OS 目录黑名单**(windows/program files/library/boot…):仅当本根是「一整块系统盘」
///   ([`is_os_disk_root`])时才叠加。用户显式挑的文件夹 / 外置卷 / NAS 挂载点 → 不叠加,
///   里面的同名子目录(如自己的 `library` 资料夹)照常全部归类进文件库。
pub fn scan_root(
    root: &str,
    exclude: &HashSet<String>,
    full: bool,
    progress: &(dyn Fn(u64, u64) + Sync),
) -> Result<ScanSummary, String> {
    // 是否叠加 OS 目录黑名单:整盘扫描 = 重剪;显式文件夹/卷/挂载点 = 只剪永远跳的噪音。
    // 关键:映射进来的 NAS 盘符(Z: 这类)虽是 `X:\` 形状,却**不是**本机系统盘 → 退回轻剪,
    // 否则 library/system/private 等 NAS 常见共享名会被整棵丢掉(见 [`is_remote_root`])。
    let remote = is_remote_root(root);
    let heavy_prune = is_os_disk_root(root) && !remote;
    let root_path = PathBuf::from(root);
    if !root_path.is_dir() {
        return Err(format!("根目录不存在或不是目录: {root}"));
    }
    let root_canon = dunce_canonical(&root_path);
    let started = std::time::Instant::now();
    let gen = chrono::Local::now().timestamp_millis();

    // root 行就位
    let conn = open_db()?;
    conn.execute(
        "INSERT INTO roots(path) VALUES(?1) ON CONFLICT(path) DO NOTHING",
        [&root_canon],
    )
    .map_err(|e| e.to_string())?;
    let root_id: i64 = conn
        .query_row("SELECT id FROM roots WHERE path=?1", [&root_canon], |r| r.get(0))
        .map_err(|e| e.to_string())?;

    // ── 增量盘点:载入上一轮的目录缓存(rel → mtime + 直属文件数/字节)──────────────
    // 这一笔查询(~目录数行,几万级,本地 SQLite 毫秒级)换来重扫时「没变的子树整棵免遍历」。
    // `full=true` 或第一次盘点(缓存空)→ 不命中,等同全量。`dir_children` 由全部 key 反推父子
    // 邻接表:跳过某目录时据它把子目录排进队列(自己不 read_dir 也能继续往下钻、逐个比对)。
    let mut dir_mt: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut dir_stat: std::collections::HashMap<String, (u64, u64)> = std::collections::HashMap::new();
    if !full {
        if let Ok(mut stmt) =
            conn.prepare("SELECT relpath, mtime, fcount, fbytes FROM dirs WHERE root_id=?1")
        {
            if let Ok(rows) = stmt.query_map([root_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            }) {
                for (rel, mt, fc, fb) in rows.flatten() {
                    dir_mt.insert(rel.clone(), mt);
                    dir_stat.insert(rel, (fc as u64, fb as u64));
                }
            }
        }
    }
    let mut dir_children: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for rel in dir_mt.keys() {
        if rel.is_empty() {
            continue; // 根没有父
        }
        dir_children.entry(parent_rel(rel).to_string()).or_default().push(rel.clone());
    }
    let dir_mt = Arc::new(dir_mt);
    let dir_stat = Arc::new(dir_stat);
    let dir_children = Arc::new(dir_children);
    drop(conn);

    // ── 工业级协作调度(永不冻结)──────────────────────────────────────────
    // 旧实现用 `thread::scope` + 共享栈 + `sleep(2ms)` 忙等:一个目录卡在 read_dir(NAS 掉线/
    // 权限挂起)就会让 scope 末尾的 join 永久阻塞 → 整个盘点冻死(「点盘点卡死」的头号根因)。
    // 新实现:WorkQueue(Condvar 零忙等 + 在途记账 + 存活 worker 计数)+ 每目录心跳看门狗。
    //   · worker 卡在某目录超 [`dir_deadline`] → 看门狗判定卡死、记账释放、列入「已跳过」;
    //   · 完成判据 `in_flight==0 && (队空 || 存活 worker==0)` 数学上保证协调线程必然返回;
    //   · 真·僵死的 worker 线程被 detach(不 join),其阻塞 syscall 随挂载恢复/进程退出回收。
    let (tx, rx) = mpsc::channel::<Msg>();
    let n_files = Arc::new(AtomicU64::new(0));
    let n_bytes = Arc::new(AtomicU64::new(0));
    let workers = worker_count();
    // 工作单元带 mtime(根的未知 → 0,pop 时现 stat);子目录由父 read_dir 顺手带出其 mtime。
    let queue = Arc::new(WorkQueue::new(vec![(root_path.clone(), 0i64)]));
    queue.set_live_workers(workers);
    let beats: Arc<Vec<Mutex<Beat>>> = Arc::new(
        (0..workers)
            .map(|_| Mutex::new(Beat { dir: None, abandoned: false }))
            .collect(),
    );
    // 每个 worker 的「已处理目录项」累加计数(Relaxed,热循环里零锁开销)。看门狗据此区分
    // 「真卡死(计数不动)」与「目录大、仍在稳定吐项(计数仍在涨)」——后者绝不误杀,这是
    // NAS 等慢盘上「扫得彻底、不丢子树」的关键。worker 取到新目录时也 +1,给看门狗重置死线基线。
    let progressed: Arc<Vec<AtomicU64>> =
        Arc::new((0..workers).map(|_| AtomicU64::new(0)).collect());
    let problematic: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let scan_done = Arc::new(AtomicBool::new(false));
    let root_arc = Arc::new(root_path.clone());
    let exclude_arc = Arc::new(exclude.clone());

    // writer 线程:独占连接,批量事务。**靠 scan_done 收尾**(不再依赖通道关闭)——
    // 这样即便有卡死的 worker 还握着 tx 克隆,writer 也不会被永久挂在 recv 上(旧实现的二级冻结)。
    let writer = {
        let scan_done = scan_done.clone();
        std::thread::spawn(move || -> Result<(), String> {
            let conn = open_db()?;
            let mut batch: Vec<Msg> = Vec::with_capacity(2048);
            let flush = |conn: &rusqlite::Connection, batch: &mut Vec<Msg>| -> Result<(), String> {
                if batch.is_empty() {
                    return Ok(());
                }
                conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
                {
                    let mut ins_file = conn
                        .prepare_cached(
                            "INSERT INTO files(root_id,relpath,name,ext,kind,lang,size,mtime,chunked,seen)
                             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,0,?9)
                             ON CONFLICT(root_id,relpath) DO UPDATE SET
                               name=excluded.name, ext=excluded.ext, kind=excluded.kind,
                               -- 文稿回填得到的自然语言(中文/英文)别被重扫的 '' 覆盖:仅当新值非空才更新。
                               lang=CASE WHEN excluded.lang!='' THEN excluded.lang ELSE files.lang END,
                               chunked=CASE WHEN files.mtime=excluded.mtime AND files.size=excluded.size
                                            THEN files.chunked ELSE 0 END,
                               ftsed=CASE WHEN files.mtime=excluded.mtime AND files.size=excluded.size
                                          THEN files.ftsed ELSE 0 END,
                               size=excluded.size, mtime=excluded.mtime, seen=excluded.seen",
                        )
                        .map_err(|e| e.to_string())?;
                    // 改过/新目录:记下 mtime + 直属文件数/字节(供下次增量比对),刷 seen。
                    let mut up_dir = conn
                        .prepare_cached(
                            "INSERT INTO dirs(root_id,relpath,mtime,fcount,fbytes,seen)
                             VALUES(?1,?2,?3,?4,?5,?6)
                             ON CONFLICT(root_id,relpath) DO UPDATE SET
                               mtime=excluded.mtime, fcount=excluded.fcount,
                               fbytes=excluded.fbytes, seen=excluded.seen",
                        )
                        .map_err(|e| e.to_string())?;
                    // 没变目录:只刷自己 dirs 行的 seen(mtime/计数不变,保留)。
                    let mut touch_dir = conn
                        .prepare_cached("UPDATE dirs SET seen=?3 WHERE root_id=?1 AND relpath=?2")
                        .map_err(|e| e.to_string())?;
                    // 没变目录:把它的**直属**文件 seen 刷成本轮代际(否则代际对账会误删)。
                    // 用 [lo,hi) 区间走 UNIQUE(root_id,relpath) 索引(BINARY 比较),instr/substr
                    // 把范围收窄到「直属」(子目录里的文件由各自目录处理,不在此刷,避免越权遮蔽删除)。
                    // substr/length/instr 在 SQLite 里按字符算,故 off 用 rel 的字符数 +2(跳 rel + '/')。
                    let mut bump_files = conn
                        .prepare_cached(
                            "UPDATE files SET seen=?1 WHERE root_id=?2
                             AND relpath>=?3 AND relpath<?4 AND instr(substr(relpath,?5),'/')=0",
                        )
                        .map_err(|e| e.to_string())?;
                    // 根的直属文件(relpath 不含 '/'):无前缀区间可用,直接 instr 过滤(根至多跳一次)。
                    let mut bump_root = conn
                        .prepare_cached(
                            "UPDATE files SET seen=?2 WHERE root_id=?1 AND instr(relpath,'/')=0",
                        )
                        .map_err(|e| e.to_string())?;
                    for msg in batch.drain(..) {
                        match msg {
                            Msg::File(row) => {
                                ins_file
                                    .execute(rusqlite::params![
                                        root_id, row.relpath, row.name, row.ext, row.kind, row.lang,
                                        row.size as i64, row.mtime, gen
                                    ])
                                    .map_err(|e| e.to_string())?;
                            }
                            Msg::Dir { rel, mtime, fcount, fbytes } => {
                                up_dir
                                    .execute(rusqlite::params![
                                        root_id, rel, mtime, fcount, fbytes, gen
                                    ])
                                    .map_err(|e| e.to_string())?;
                            }
                            Msg::Skip { rel } => {
                                touch_dir
                                    .execute(rusqlite::params![root_id, rel, gen])
                                    .map_err(|e| e.to_string())?;
                                if rel.is_empty() {
                                    bump_root
                                        .execute(rusqlite::params![root_id, gen])
                                        .map_err(|e| e.to_string())?;
                                } else {
                                    let lo = format!("{rel}/");
                                    let hi = format!("{rel}0"); // '0' = '/'(0x2F)+1 → [lo,hi) 恰好框住 rel/* 全部直属及更深项
                                    let off = rel.chars().count() as i64 + 2; // 1-based:跳过 rel + '/'
                                    bump_files
                                        .execute(rusqlite::params![gen, root_id, lo, hi, off])
                                        .map_err(|e| e.to_string())?;
                                }
                            }
                        }
                    }
                }
                conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
                Ok(())
            };
            loop {
                match rx.recv_timeout(Duration::from_millis(150)) {
                    Ok(msg) => {
                        batch.push(msg);
                        if batch.len() >= 2048 {
                            flush(&conn, &mut batch)?;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if scan_done.load(Ordering::SeqCst) {
                            // 收尾:把刚到的尾巴排空再退,绝不漏行。
                            while let Ok(msg) = rx.try_recv() {
                                batch.push(msg);
                                if batch.len() >= 2048 {
                                    flush(&conn, &mut batch)?;
                                }
                            }
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            flush(&conn, &mut batch)?;
            Ok(())
        })
    };

    // walker 线程池:从 WorkQueue 取目录,扫到的子目录回压入队;每件活儿前后打心跳。
    for i in 0..workers {
        let tx = tx.clone();
        let queue = queue.clone();
        let beats = beats.clone();
        let progressed = progressed.clone();
        let problematic = problematic.clone();
        let n_files = n_files.clone();
        let n_bytes = n_bytes.clone();
        let exclude = exclude_arc.clone();
        let root_path = root_arc.clone();
        let dir_mt = dir_mt.clone();
        let dir_stat = dir_stat.clone();
        let dir_children = dir_children.clone();
        std::thread::spawn(move || {
            while let Some(job) = queue.pop() {
                let (dir, known_mtime) = job.item;
                {
                    let mut b = beats[i].lock().unwrap();
                    b.dir = Some(dir.clone());
                    b.abandoned = false;
                }
                // 取到新目录:计数 +1,让看门狗看到「有进度」从而重置该 worker 的死线基线
                // (否则可能拿上一件活儿留下的旧基线,刚接手就被误判卡死)。
                progressed[i].fetch_add(1, Ordering::Relaxed);

                // 增量盘点:本目录 mtime 命中缓存 → 整棵跳过 read_dir(及里面所有文件的逐项 stat)。
                // mtime 由父目录的 read_dir 顺手带出(known_mtime,免再 stat);根 / 跳过路径排进来的
                // 子目录 known=0 → 现 stat 一次拿 mtime(NAS 上一次往返,远比 read_dir 整个目录省)。
                let rel = rel_of(&dir, root_path.as_path());
                let cur_mtime = if known_mtime != 0 { known_mtime } else { dir_mtime(&dir) };
                let unchanged = cur_mtime != 0
                    && dir_mt.get(&rel).map(|m| *m == cur_mtime).unwrap_or(false);
                if unchanged {
                    // 直属文件没变:从缓存把它们的数量/字节计入进度(报数不缩水),并标记「本轮见过」;
                    // 子目录排进队列各自比对(它们的内层可能变了——目录 mtime 只反映直属项的增删改名)。
                    if let Some((fc, fb)) = dir_stat.get(&rel) {
                        n_files.fetch_add(*fc, Ordering::Relaxed);
                        n_bytes.fetch_add(*fb, Ordering::Relaxed);
                    }
                    let _ = tx.send(Msg::Skip { rel: rel.clone() });
                    if let Some(children) = dir_children.get(&rel) {
                        for c in children {
                            let child = root_path.join(c);
                            // 本轮被取消勾选的子文件夹 → 不再往里钻(与 read_dir 路径一致地尊重 exclude)。
                            if !exclude.is_empty()
                                && exclude.contains(child.to_string_lossy().as_ref())
                            {
                                continue;
                            }
                            progressed[i].fetch_add(1, Ordering::Relaxed);
                            queue.push((child, 0)); // known=0 → pop 时现 stat 比对
                        }
                    }
                    // 落到下方共享的「结算心跳 + complete」收尾(不 read_dir)。
                } else {
                    match std::fs::read_dir(&dir) {
                        Ok(rd) => {
                            // 改过/新目录:边扫边累计直属文件数/字节,扫完写回 dirs 缓存供下次增量。
                            let mut fcount = 0i64;
                            let mut fbytes = 0i64;
                            for entry in rd.flatten() {
                                // 每吐一项就记一次进度:只要还在稳定吐项,看门狗就知道没卡死。
                                progressed[i].fetch_add(1, Ordering::Relaxed);
                                let Ok(ft) = entry.file_type() else { continue };
                                if ft.is_symlink() {
                                    continue;
                                }
                                // 非 UTF-8 名(Linux/Docker 上的 GBK 中文名)解回中文,避免乱码 �。
                                let name = super::decode_fs(&entry.file_name());
                                if ft.is_dir() {
                                    // 永远跳的噪音任何根都剪;OS 目录黑名单只在整盘扫描时叠加,
                                    // 这样显式挑的文件夹里的东西「全归类进库」。
                                    if skip_dir_always(&name) || (heavy_prune && skip_dir_os(&name)) {
                                        continue;
                                    }
                                    let child = entry.path();
                                    // 用户在「扫描」步骤取消勾选的文件夹 → 整棵跳过。
                                    if !exclude.is_empty()
                                        && exclude.contains(child.to_string_lossy().as_ref())
                                    {
                                        continue;
                                    }
                                    // 子目录的 mtime 顺手从本次 read_dir 的项里取出(Windows/SMB 上免额外
                                    // 往返),带进队列 → 子目录 pop 时无需再 stat 即可比对增量。
                                    let cmt = entry
                                        .metadata()
                                        .ok()
                                        .and_then(|m| m.modified().ok())
                                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                        .map(|d| d.as_secs() as i64)
                                        .unwrap_or(0);
                                    queue.push((child, cmt));
                                } else if ft.is_file() {
                                    let Ok(meta) = entry.metadata() else { continue };
                                    let p = entry.path();
                                    let frel = p
                                        .strip_prefix(root_path.as_path())
                                        .map(|r| super::decode_fs(r.as_os_str()).replace('\\', "/"))
                                        .unwrap_or_else(|_| name.clone());
                                    let ext = p
                                        .extension()
                                        .map(|e| e.to_string_lossy().to_ascii_lowercase())
                                        .unwrap_or_default();
                                    let mtime = meta
                                        .modified()
                                        .ok()
                                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                        .map(|d| d.as_secs() as i64)
                                        .unwrap_or(0);
                                    let size = on_disk_size(&p, &meta, remote);
                                    n_files.fetch_add(1, Ordering::Relaxed);
                                    n_bytes.fetch_add(size, Ordering::Relaxed);
                                    fcount += 1;
                                    fbytes += size as i64;
                                    let kind = classify(&ext);
                                    let _ = tx.send(Msg::File(FileRow {
                                        relpath: frel,
                                        name,
                                        kind,
                                        lang: quick_lang(&ext, kind), // 代码/媒体当场定;文稿留 "" 待回填
                                        ext,
                                        size,
                                        mtime,
                                    }));
                                }
                            }
                            let _ = tx.send(Msg::Dir { rel, mtime: cur_mtime, fcount, fbytes });
                        }
                        Err(_) => {
                            // 读目录失败(权限抖动 / NAS 瞬断):不丢弃也不原地卡住别人,而是降级到
                            // 队尾「最后再试」;超过 [`MAX_DIR_ATTEMPTS`] 仍失败才列为「已跳过」。
                            if job.attempts < MAX_DIR_ATTEMPTS {
                                queue.demote((dir.clone(), known_mtime), job.attempts + 1);
                            } else {
                                problematic.lock().unwrap().push(dir.to_string_lossy().into_owned());
                            }
                        }
                    }
                }
                // 结算心跳:看门狗若已判定本 worker 卡死(已 abandon 记账)→ 本线程就此退场,
                // 既不再 complete(避免重复减在途)也不 worker_exited(abandon 已减存活数)。
                let was_abandoned = {
                    let mut b = beats[i].lock().unwrap();
                    b.dir = None;
                    std::mem::replace(&mut b.abandoned, false)
                };
                if was_abandoned {
                    return;
                }
                queue.complete();
            }
            queue.worker_exited();
        });
    }
    drop(tx); // 协调线程不再持 tx;writer 靠 scan_done 收尾,不依赖通道关闭

    // 看门狗:每 250ms 巡一遍心跳。**只有当某 worker「在忙、且自上次巡查以来一个目录项都没新
    // 处理过」持续超过死线**,才判定它真·卡死(僵死的 read_dir/stat 系统调用)、记账释放、列入
    // 已跳过。换言之死线指的是「零进度的时长」而非「处理这个目录的总时长」—— 于是 NAS 上一个动辄
    // 十万项的大目录,只要还在稳定吐项就永不被误杀(旧实现按总时长 60s 一刀切,大目录被整棵丢弃,
    // 正是「扫不全」的头号原因)。真·僵死时计数纹丝不动,死线一到照样果断放弃,绝不冻结盘点。
    {
        let queue = queue.clone();
        let beats = beats.clone();
        let progressed = progressed.clone();
        let problematic = problematic.clone();
        let scan_done = scan_done.clone();
        let deadline = dir_deadline();
        let nworkers = workers;
        std::thread::spawn(move || {
            // 每槽:(上次见到的进度计数, 那一刻)。计数变了就刷新基线;长时间不变才算卡死。
            let mut last: Vec<(u64, Instant)> =
                (0..nworkers).map(|_| (0u64, Instant::now())).collect();
            loop {
                std::thread::sleep(Duration::from_millis(250));
                if scan_done.load(Ordering::SeqCst) || cancelled() {
                    break;
                }
                for (i, slot) in beats.iter().enumerate() {
                    let cur = progressed[i].load(Ordering::Relaxed);
                    if cur != last[i].0 {
                        last[i] = (cur, Instant::now()); // 有进度 → 重置死线基线
                        continue;
                    }
                    let stuck = {
                        let mut b = slot.lock().unwrap();
                        // 仅当「在忙(dir 有值)、未被放弃、且零进度已超死线」才判卡死。
                        let hit = if !b.abandoned
                            && b.dir.is_some()
                            && last[i].1.elapsed() > deadline
                        {
                            b.dir.as_ref().map(|p| p.to_string_lossy().into_owned())
                        } else {
                            None
                        };
                        if hit.is_some() {
                            b.abandoned = true;
                        }
                        hit
                    };
                    if let Some(path) = stuck {
                        problematic.lock().unwrap().push(path);
                        // 释放在途 + 存活 worker -1:可能令 live_workers 归零 → 满足完成判据、解除冻结。
                        queue.abandon();
                        last[i] = (cur, Instant::now()); // 放弃后基线归位,避免重复触发
                    }
                }
            }
        });
    }

    // 协调线程(本线程):零忙等地等盘点了结,其间交错上报进度;取消则关闭队列。
    let mut last = 0u64;
    loop {
        if cancelled() {
            queue.cancel();
            break;
        }
        let f = n_files.load(Ordering::Relaxed);
        if f != last {
            progress(f, n_bytes.load(Ordering::Relaxed));
            last = f;
        }
        if queue.wait_until_done_for(Duration::from_millis(200)) {
            break;
        }
    }
    scan_done.store(true, Ordering::SeqCst);

    // 剩余未处理目录:正常为空;若挂载掉线令 worker 卡死而提前了结,这些就是没扫到的目录。
    let skipped_dirs = queue.drain_remaining();
    // 仅 join writer(它靠 scan_done 在 ≤150ms 内收尾,有界);worker/看门狗 detach:健康 worker
    // 此刻在途已归零、即将自行退出,卡死的随其阻塞 syscall 自然回收 —— 绝不 join 卡死线程。
    writer.join().map_err(|_| "writer 线程 panic".to_string())??;

    if cancelled() {
        return Err("已取消".into());
    }

    let files = n_files.load(Ordering::Relaxed);
    let bytes = n_bytes.load(Ordering::Relaxed);
    progress(files, bytes); // 收尾再报一次,确保进度条落到最终值
    let skipped: u64 = {
        let mut v = problematic.lock().unwrap();
        for (d, _mt) in skipped_dirs {
            v.push(d.to_string_lossy().into_owned());
        }
        v.len() as u64
    };

    // 护栏:本轮扫到 0 文件 → 几乎一定是「根临时读不到」(NAS 挂载掉线 / 权限抖动 / 路径没挂上),
    // 而非用户真把整个根清空了。此时若照常跑 seen 代际删除,会把该根上一轮的全部记录连同
    // 已建好的向量一并抹掉 → 文件中心「盘点完右边一下子全没了」。故扫到 0 文件就**跳过删除、
    // 也不刷新 roots 计数**,保留上一轮已知状态;待挂载恢复后重扫自然对账。
    // (真要清空一个根:删到只剩 1 个占位文件即可触发正常代际清理。)
    let conn = open_db()?;
    let removed = if files == 0 {
        0
    } else {
        // 代际清理不能「本轮没扫到就删」:seen<>gen 只代表这一轮没遇见,而「没遇见」
        // 大多是父目录临时读不到(NAS 掉线 / 外置盘没插 / 权限抖动 / 软链被跳过),
        // 文件其实还在。照删就会出现「下次登陆,有些数据从知识库里没了」(连向量/倒排一起抹)。
        // 故改为「逐个 stat 确认真消失」:文件仍在(含读不到、软链)→ 保留;父目录整个
        // 掉线(子树不可达)→ 保留;仅当「父目录还在、文件确实不存在」才判定真删除。
        let stale: Vec<(i64, String)> = {
            let mut stmt = conn
                .prepare("SELECT id, relpath FROM files WHERE root_id=?1 AND seen<>?2")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(rusqlite::params![root_id, gen], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(|e| e.to_string())?;
            rows.filter_map(|r| r.ok()).collect()
        };
        let root_base = PathBuf::from(&root_canon);
        let gone: Vec<i64> = stale
            .into_iter()
            .filter(|(_, rel)| {
                // relpath 用 '/',Path::join 在 Windows 上也认 '/',无需替换分隔符。
                let abs = root_base.join(rel);
                let parent_ok = abs.parent().map(|p| p.exists()).unwrap_or(false);
                parent_ok && !abs.exists()
            })
            .map(|(id, _)| id)
            .collect();

        let mut n = 0u64;
        if !gone.is_empty() {
            let lex_on = super::lex_available(&conn);
            // IN 列表分批,避开 SQLite 变量上限(默认 ~999/32766)。
            for batch in gone.chunks(512) {
                let ph = vec!["?"; batch.len()].join(",");
                conn.execute(
                    &format!("DELETE FROM chunks WHERE file_id IN ({ph})"),
                    rusqlite::params_from_iter(batch.iter()),
                )
                .map_err(|e| e.to_string())?;
                if lex_on {
                    // P1-2:消失文件同步清出 FTS 倒排(lex 未编入时跳过)。rowid=file_id。
                    conn.execute(
                        &format!("DELETE FROM lex WHERE rowid IN ({ph})"),
                        rusqlite::params_from_iter(batch.iter()),
                    )
                    .map_err(|e| e.to_string())?;
                }
                n += conn
                    .execute(
                        &format!("DELETE FROM files WHERE id IN ({ph})"),
                        rusqlite::params_from_iter(batch.iter()),
                    )
                    .map_err(|e| e.to_string())? as u64;
            }
        }
        // 目录缓存对账:本轮没确认(seen<>gen)的 dirs 行清掉 —— 目录已消失,或本轮没扫到
        // (挂载掉线 / 反复读失败被跳过)。后者只是丢掉它的「免遍历」资格,下次重扫当成新目录
        // 完整 read_dir 再补回缓存,绝不波及 files(文件的删除另由上面「逐个 stat 确认真消失」把关)。
        conn.execute("DELETE FROM dirs WHERE root_id=?1 AND seen<>?2", rusqlite::params![root_id, gen])
            .map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE roots SET scanned_at=?2, files=?3, bytes=?4 WHERE id=?1",
            rusqlite::params![root_id, gen, files as i64, bytes as i64],
        )
        .map_err(|e| e.to_string())?;
        n
    };

    Ok(ScanSummary {
        root: root_canon,
        files,
        bytes,
        removed,
        skipped,
        seconds: started.elapsed().as_secs_f64(),
        workers,
    })
}

/// Windows 的 canonicalize 会出 `\\?\` 前缀(已在 PPTX 审计里踩过坑),手工剥掉。
/// 网络盘(映射进来的群晖 NAS,如 `Z:`)canonicalize 后是 `\\?\UNC\server\share` —— 若按
/// 普通前缀只剥 `\\?\` 会留下 `UNC\server\share` 这种**非法路径**(于是这条根的文件打不开、
/// 对账 stat 也失效)。UNC 前缀要还原成 `\\server\share` 才是有效路径。
fn dunce_canonical(p: &Path) -> String {
    let c = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    strip_extended_prefix(&c.to_string_lossy())
}

/// 剥掉 Windows 扩展长度前缀,得到「正常」可用路径(纯字符串变换,便于单测):
/// - `\\?\UNC\host\share\...` → `\\host\share\...`(网络盘必须还原成合法 UNC)
/// - `\\?\C:\...`             → `C:\...`
/// - 其它原样返回。
fn strip_extended_prefix(s: &str) -> String {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    s.strip_prefix(r"\\?\").map(|x| x.to_string()).unwrap_or_else(|| s.to_string())
}

/// 文件「磁盘实占」字节数,而非 `metadata().len()` 报的逻辑大小。
///
/// 为什么必须用实占:稀疏文件(WSL/Docker 的 `*.vhdx`、虚拟机盘、测试用占位大文件)
/// 的逻辑大小可达声称的几十 GB,但磁盘上几乎不占空间。照逻辑大小累加会让「总量」
/// 虚高好几倍(实测一台机 D:\ 真实 371 GB 被算成 2.8 TB,光 60 个稀疏 .mkv 就虚报 2.3 TB)。
/// NTFS 压缩卷同理——实占小于逻辑。这里统一取磁盘实占,口径才与资源管理器的「占用」一致。
///
/// `remote=true`(映射的 NAS/网络盘)时直接取逻辑大小:`GetCompressedFileSizeW` 在网络盘上
/// 本就常失败回退,但它**每个文件一次网络往返**——成千上万文件串起来能把一个目录的处理时间
/// 拖到几十秒、撞上看门狗死线被判卡死整棵丢掉。网络盘上稀疏/压缩文件少见,逻辑大小够用,
/// 省掉这趟往返让 NAS 扫描快上数量级,也才扫得全。
///
/// 本地盘提速:`GetCompressedFileSizeW` 是按路径的额外系统调用(内部要打开文件查实占),
/// 大库上几十万文件累计是单文件主要开销。但「逻辑大小 ≠ 实占」**只发生在稀疏 / NTFS 压缩
/// 文件**上,而这两类在文件属性里有标志位(SPARSE / COMPRESSED),已随目录枚举缓存进 `meta`
/// (`file_attributes()` 零额外 syscall)。于是:普通文件(99%+)直接用 `meta.len()`,只对
/// 真·稀疏/压缩文件才掏这趟实占查询 —— 既保住「稀疏盘不虚高」的正确性,又把绝大多数文件的
/// 那次额外 syscall 省掉,本地全盘扫描显著加速。
fn on_disk_size(path: &Path, meta: &std::fs::Metadata, remote: bool) -> u64 {
    if remote {
        return meta.len();
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Foundation::GetLastError;
        use windows_sys::Win32::Storage::FileSystem::{
            GetCompressedFileSizeW, FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_SPARSE_FILE,
            INVALID_FILE_SIZE,
        };
        // 非稀疏、非压缩 → 实占≈逻辑大小,免掉按路径的 GetCompressedFileSizeW 系统调用。
        if meta.file_attributes() & (FILE_ATTRIBUTE_SPARSE_FILE | FILE_ATTRIBUTE_COMPRESSED) == 0 {
            return meta.len();
        }
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let mut high: u32 = 0;
        // SAFETY: wide 是以 NUL 结尾的合法宽字符串;high 是有效可写指针。
        let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
        // INVALID_FILE_SIZE(0xFFFFFFFF)既可能是出错,也可能是合法低位 → 需查 GetLastError 区分。
        if low == INVALID_FILE_SIZE {
            let err = unsafe { GetLastError() };
            if err != 0 {
                return meta.len(); // 取不到实占(网络盘/权限)→ 保守回退逻辑大小
            }
        }
        return ((high as u64) << 32) | (low as u64);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = path;
        return meta.blocks().saturating_mul(512);
    }
    #[allow(unreachable_code)]
    meta.len()
}

// ───────────────────────── 命令(后台线程 + 事件)─────────────────────────

fn emit(app: &AppHandle, payload: Value) {
    let _ = app.emit("fable:inventory", payload);
}

/// 解析「盘点哪些根」。
/// - 显式传 root → 只盘这一个;
/// - 否则:`POLARIS_INVENTORY_ROOTS`(PATH 分隔:Win 用 `;`、Unix 用 `:`)+ 约定挂载点
///   `<KB父目录>/nas`(群晖 Docker 把各 NAS 共享 bind 到这里)+ 知识库根。
///
/// 桌面版没有 nas 挂载点、也不设环境变量 → 退化成单根 = 知识库根(行为不变)。
/// 容器版能据此把 `/root/Polaris/nas/<share>` 整个挂载点一并盘点,文件中心遂能看到全 NAS。
fn inventory_roots(explicit: Option<String>) -> Vec<String> {
    if let Some(r) = explicit
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
    {
        return vec![r];
    }
    let mut roots: Vec<String> = Vec::new();
    if let Ok(v) = std::env::var("POLARIS_INVENTORY_ROOTS") {
        for p in std::env::split_paths(&v) {
            let s = p.to_string_lossy().trim().to_string();
            if !s.is_empty() {
                roots.push(s);
            }
        }
    }
    let kb = crate::kb::kb_root();
    // 约定:NAS 各共享 bind-mount 到 <KB父目录>/nas/<share>(见 docker-compose.synology)。
    if let Some(parent) = std::path::Path::new(&kb).parent() {
        let nas = parent.join("nas");
        if nas.is_dir() {
            roots.push(nas.to_string_lossy().to_string());
        }
    }
    // 始终把知识库根纳入盘点。
    if !kb.trim().is_empty() {
        roots.push(kb);
    }
    // App 数据下载/接收目录(微信/QQ/浏览器…),默认纳入盘点 —— 用户最关心的「收到的文件」
    // 大多在这里,且常埋在 Documents 深处或 appdata(整盘扫会被剪掉),故单列为默认根。
    for r in app_data_roots() {
        roots.push(r.path);
    }
    roots.sort();
    roots.dedup();
    roots
}

/// 「App 数据」下载 / 接收目录预设(浏览器下载、微信、QQ/TIM、企业微信…)。
/// 这些目录里全是用户真实下载 / 收到的文件,但常埋在 `Documents` 深处、甚至 `AppData`
/// (被 [`skip_dir_scan`] 整棵剪掉)里 → 整盘盘点极易漏。故把它们提成**默认勾选的独立根**:
/// 既「一键收下载」,又因为是**显式根**(walker 从这里起步、永不途经名为 appdata 的目录),
/// 天然绕过 appdata 黑名单 —— 这正是「对 appdata 只放行这几个已知子路径」的白名单例外。
/// 只返回真实存在的目录;同一类(版本/路径不同)给多个候选,命中即收、按路径去重。
fn app_data_roots() -> Vec<ScanRootInfo> {
    let mut out: Vec<ScanRootInfo> = Vec::new();
    let Some(home) = directories::UserDirs::new().map(|u| u.home_dir().to_path_buf()) else {
        return out;
    };
    // (相对 home 的子路径段, 显示名)。
    let candidates: &[(&[&str], &str)] = &[
        (&["Downloads"], "下载"),
        (&["Documents", "WeChat Files"], "微信文件"), // 微信 3.x
        (&["Documents", "xwechat_files"], "微信文件"), // 微信 4.x
        (&["Documents", "WeChatFiles"], "微信文件"),
        (&["Documents", "Tencent Files"], "QQ/TIM 文件"), // 含各账号的 FileRecv
        (&["Documents", "WXWork"], "企业微信文件"),
    ];
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (segs, label) in candidates {
        let mut p = home.clone();
        for s in *segs {
            p = p.join(s);
        }
        if p.is_dir() {
            let path = p.to_string_lossy().to_string();
            if seen.insert(path.clone()) {
                out.push(ScanRootInfo { path, label: (*label).to_string(), default_on: true });
            }
        }
    }
    out
}

/// 开始盘点。立即返回,进度走 `fable:inventory` 事件。
/// - `roots` = 用户在选择器里勾选的要盘点的文件夹/盘符(可以是知识库之外的任意目录);
///   缺省/空 → 退回默认(知识库根 + 约定的 NAS 挂载点)。
/// - `exclude` = 勾选范围内又被取消的子文件夹绝对路径(整棵跳过)。
/// - `full` = 是否完整盘点(忽略目录缓存、每个目录都 read_dir);缺省/false = 智能增量
///   (只摸 mtime 变过的子树,见 [`scan_root`]),日常重扫快一个数量级。
///
/// **`(async)`**:函数体里对每个根做 [`dir_reachable`] 有界探测(死 NAS 上每根最多卡
/// `probe_secs`≈12s)。同步 tauri 命令跑在主线程会冻住 UI;标 `(async)` 让 tauri 把这段
/// 同步活儿派到工作线程,主线程不被吊死(冷 NAS 盘上点「盘点」UI 仍跟手)。
#[cfg_attr(feature = "desktop", tauri::command(async))]
pub fn fable_inventory_start(
    app: AppHandle,
    roots: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    full: Option<bool>,
) -> Result<(), String> {
    let full = full.unwrap_or(false);
    // 显式勾选优先;没传则退回默认根集合。去重 + 只留真实目录。
    let picked: Vec<String> = roots
        .unwrap_or_default()
        .into_iter()
        .map(|r| r.trim_end_matches(['/', '\\']).to_string())
        .filter(|r| !r.is_empty())
        .collect();
    let candidates: Vec<String> = if picked.is_empty() {
        inventory_roots(None)
    } else {
        picked
    };
    // 有界探测可达性:挂载点掉线时 is_dir 会吊死「开始盘点」请求 → 超死线判不可达即剔除(scan_root
    // 内部还有看门狗兜底,这里先把死根挡在请求路径外,点「盘点」立刻有反应)。
    // **连不上的根不再默默丢弃**,而是收进 `unreachable` 一并报给前端 —— 否则用户只看到「盘点完成」
    // 却不知道群晖 NAS / 拔掉的外置盘这次根本没扫到。远程盘(映射的 NAS、UNC)给更长的冷连接时间:
    // Tailscale/SMB 首次握手本就慢,免得「只是第一下慢」被误判不可达而整盘采集不到。
    let mut roots: Vec<String> = Vec::new();
    let mut unreachable: Vec<String> = Vec::new();
    for r in candidates {
        let secs = if is_remote_root(&r) { probe_secs().max(25) } else { probe_secs() };
        if super::sched::dir_reachable(std::path::Path::new(&r), secs) {
            roots.push(r);
        } else {
            unreachable.push(r);
        }
    }
    roots.sort();
    roots.dedup();
    unreachable.sort();
    unreachable.dedup();
    // 去掉「嵌套根」:若 B 在 A 之内**且 A 扫得到 B**,盘 A 已覆盖 B,留 A 去 B(免重复)。
    // 排序后父目录必排在子目录前,顺序扫描即可。注意「扫得到」要排除中途被剪枝的情况:
    // 例如 B = …/AppData/…/Downloads 在 A = C:\ 之内,但扫 C:\ 时 `appdata` 整棵被
    // [`skip_dir_scan`] 剪掉、根本到不了 B → 此时必须保留 B(否则下载目录又被吞没)。
    {
        let mut kept: Vec<String> = Vec::new();
        for r in roots.into_iter() {
            let inside = kept.iter().any(|k| covered_by(k, &r));
            if !inside {
                kept.push(r);
            }
        }
        roots = kept;
    }
    if roots.is_empty() {
        if !unreachable.is_empty() {
            return Err(format!(
                "这些位置连接不上,已跳过:{} —— 检查网络 / Tailscale / 外置盘连接后重新盘点即可。",
                unreachable.join("、")
            ));
        }
        return Err("没有可盘点的根目录(知识库未初始化,也无可访问的挂载点)".into());
    }
    let exclude: HashSet<String> = exclude
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.trim_end_matches(['/', '\\']).to_string())
        .filter(|p| !p.is_empty())
        .collect();
    let Some(scan_guard) = FlagGuard::acquire(&SCANNING) else {
        return Err("盘点已在进行中".into());
    };
    CANCEL.store(false, Ordering::SeqCst);
    std::thread::spawn(move || {
        // 守卫 move 进线程:正常结束或 panic 栈展开都会释放 SCANNING 闸(防永久锁死)。
        let _scan_guard = scan_guard;
        // 多根串行盘点;进度按「已盘过的根」累加,前端看到的是全量计数。
        let mut acc_files = 0u64;
        let mut acc_bytes = 0u64;
        let mut acc_removed = 0u64;
        let mut acc_skipped = 0u64;
        let mut acc_secs = 0.0f64;
        let mut workers = 0usize;
        let mut last_err: Option<String> = None;
        for r in &roots {
            if cancelled() {
                break;
            }
            let app_p = app.clone();
            let base_f = acc_files;
            let base_b = acc_bytes;
            match scan_root(r, &exclude, full, &move |files, bytes| {
                emit(
                    &app_p,
                    json!({ "kind": "progress", "files": base_f + files, "bytes": base_b + bytes }),
                );
            }) {
                Ok(s) => {
                    acc_files += s.files;
                    acc_bytes += s.bytes;
                    acc_removed += s.removed;
                    acc_skipped += s.skipped;
                    acc_secs += s.seconds;
                    workers = s.workers;
                }
                Err(e) => last_err = Some(e),
            }
        }
        if cancelled() {
            emit(&app, json!({ "kind": "error", "message": "已取消" }));
        } else if acc_files == 0 {
            let msg = match (last_err, unreachable.is_empty()) {
                (Some(e), _) => e,
                (None, false) => format!(
                    "这些位置连接不上,已跳过:{} —— 检查网络 / Tailscale / 外置盘连接后重新盘点即可。",
                    unreachable.join("、")
                ),
                (None, true) => "未扫描到任何文件".into(),
            };
            emit(&app, json!({ "kind": "error", "message": msg }));
        } else {
            // `unreachable` 一并带回:本轮成功扫了 C/D 盘,但群晖 NAS / 外置盘没连上时,前端据此弹个
            // 温和提示框(「XX 这次没连上,已跳过」),而不是让用户误以为「盘点完成 = 全都扫到了」。
            emit(
                &app,
                json!({
                    "kind": "done", "files": acc_files, "bytes": acc_bytes,
                    "removed": acc_removed, "skipped": acc_skipped,
                    "seconds": acc_secs, "workers": workers,
                    "roots": roots.len(),
                    "unreachable": unreachable,
                    "full": full,
                }),
            );
        }
    });
    Ok(())
}

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
    if let Some(r) = explicit.map(|r| r.trim().to_string()).filter(|r| !r.is_empty()) {
        let label = Path::new(&r)
            .file_name()
            .map(super::decode_fs)
            .unwrap_or_else(|| r.clone());
        return vec![ScanRootInfo { path: r, label, default_on: true }];
    }
    let mut out: Vec<ScanRootInfo> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let kb = crate::kb::kb_root();
    // App 数据根的友好名(下载/微信/QQ…),给下方贴标签用(inventory_roots 已含这些路径)。
    let app_labels: std::collections::HashMap<String, String> =
        app_data_roots().into_iter().map(|r| (r.path, r.label)).collect();
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
        out.push(ScanRootInfo { path: r, label, default_on: true });
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
        out.push(ScanRootInfo { path: sr.path, label: sr.label, default_on: sr.default_on });
    }
    out
}

/// 列出某目录的直属子文件夹(只读一层目录项;每个子目录再读一层估直属文件数 + 是否可展开)。
/// `root` = 所属盘点根(用于算 depth 并回填 FolderNode.root)。
fn list_child_folders(dir: &Path, root: &str) -> Vec<FolderNode> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
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
        let depth = sub.strip_prefix(root_path).map(|r| r.components().count()).unwrap_or(1).max(1);
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
        let children =
            super::sched::with_deadline(probe_secs(), move || list_child_folders(Path::new(&rp), &rp))
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
    Ok(FolderScan { roots, folders, truncated })
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
fn folder_size_rec(dir: &Path, files: &AtomicU64, bytes: &AtomicU64, remote: bool) {
    use std::sync::atomic::Ordering::Relaxed;
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
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
            folder_size_rec(&entry.path(), files, bytes, remote);
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
    let wide: Vec<u16> =
        std::ffi::OsStr::new(&root).encode_wide().chain(std::iter::once(0)).collect();
    let mut free_avail: u64 = 0;
    let mut total: u64 = 0;
    let mut total_free: u64 = 0;
    // SAFETY: wide 是 NUL 结尾的合法宽字符串;三个 out 指针均指向本栈上的有效 u64。
    let ok = unsafe {
        GetDiskFreeSpaceExW(wide.as_ptr(), &mut free_avail, &mut total, &mut total_free)
    };
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
            return Ok(FolderSize { files: 0, bytes: used });
        }
        // 拿不到容量(盘掉线等)→ 落到下面的递归(同样带死线)。
    }
    // 普通文件夹:递归累加,带 10s 死线;超时也把「已数到的部分」读回来(原子计数器),给个下限
    // 体积而非 0,避免大目录永远显示空白。
    let files = std::sync::Arc::new(AtomicU64::new(0));
    let bytes = std::sync::Arc::new(AtomicU64::new(0));
    let (fc, bc) = (files.clone(), bytes.clone());
    let remote = is_remote_root(&path); // 网络盘:跳过逐文件实占往返,直接取逻辑大小
    super::sched::with_deadline(10, move || {
        let p = Path::new(&path);
        if p.is_dir() {
            folder_size_rec(p, &fc, &bc, remote);
        }
    });
    Ok(FolderSize { files: files.load(Relaxed), bytes: bytes.load(Relaxed) })
}

/// 「按语言归类」回填:给所有还没定语言(lang='')的文件补上语言标签。
/// 代码/媒体零 IO 当场定;文稿读文件头嗅探自然语言(中文/英文/其他)。多核并行、幂等续跑。
/// 旧库(刚加 lang 列,全为 '')或新盘点后的文稿都靠它补齐。返回本轮回填条数。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fable_backfill_lang() -> Result<u64, String> {
    let conn = open_db()?;
    let mut done = 0u64;
    loop {
        if cancelled() {
            break;
        }
        // 取一批未定语言的文件(连 root 路径,文稿要据此读头)。
        let batch: Vec<(i64, String, String, String, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT f.id, f.ext, f.kind, r.path, f.relpath FROM files f
                     JOIN roots r ON r.id=f.root_id WHERE f.lang='' LIMIT 4096",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            rows.flatten().collect()
        };
        if batch.is_empty() {
            break;
        }
        // 多核算语言:代码/媒体 quick_lang 零 IO;文稿读头嗅探(work-stealing 栈)。
        let stack = Mutex::new(batch);
        let out: Mutex<Vec<(i64, String)>> = Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for _ in 0..worker_count() {
                let (stack, out) = (&stack, &out);
                s.spawn(move || loop {
                    let item = { stack.lock().unwrap().pop() };
                    let Some((id, ext, kind, root, rel)) = item else { break };
                    let mut lang = quick_lang(&ext, &kind);
                    if lang.is_empty() {
                        // 文稿:读头嗅探自然语言;不可读/二进制 → 其他。
                        let abs = Path::new(&root).join(&rel);
                        lang = read_head_sample(&abs)
                            .map(|sample| natural_lang(&sample))
                            .unwrap_or("未识别")
                            .to_string();
                    }
                    out.lock().unwrap().push((id, lang));
                });
            }
        });
        // 单事务写回这一批。
        let updates = out.into_inner().unwrap();
        conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
        {
            let mut stmt = conn
                .prepare_cached("UPDATE files SET lang=?1 WHERE id=?2")
                .map_err(|e| e.to_string())?;
            for (id, lang) in &updates {
                // 给个非空哨兵避免再次入选(理论上 lang 已非空)。
                let v = if lang.is_empty() { "未识别" } else { lang.as_str() };
                stmt.execute(rusqlite::params![v, id]).map_err(|e| e.to_string())?;
            }
        }
        conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
        done += updates.len() as u64;
        // 单次调用封顶 ~16K 文件:桌面前端循环调用,每次都短(不冻界面),返回 0 即收工。
        if done >= 16_384 {
            break;
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prog_lang_maps_extensions() {
        assert_eq!(prog_lang("py"), Some("Python"));
        assert_eq!(prog_lang("rs"), Some("Rust"));
        assert_eq!(prog_lang("tsx"), Some("React/JSX"));
        assert_eq!(prog_lang("md"), None); // 文稿交自然语言
        assert_eq!(prog_lang("png"), None);
    }

    #[test]
    fn natural_lang_detects_script() {
        assert_eq!(natural_lang("这是一段中文文本,讲的是知识库检索系统"), "中文");
        assert_eq!(natural_lang("This is an English document about retrieval."), "英文");
        assert_eq!(natural_lang("123 456 !!! ==="), "其他语种"); // 字母太少
    }

    #[test]
    fn quick_lang_code_and_media() {
        assert_eq!(quick_lang("py", "text"), "Python");
        assert_eq!(quick_lang("png", "image"), "图片");
        assert_eq!(quick_lang("md", "text"), ""); // 文稿留空待回填
    }

    #[test]
    fn skip_dir_scan_prunes_system_and_appdata() {
        for n in ["Windows", "Program Files", "AppData", "node_modules", "$Recycle.Bin", "@eaDir"] {
            assert!(skip_dir_scan(n), "{n} 应被剪掉");
        }
        for n in ["Downloads", "Documents", "WeChat Files", "datasets"] {
            assert!(!skip_dir_scan(n), "{n} 不应被剪掉");
        }
    }

    #[test]
    fn two_tier_prune_keeps_user_named_dirs() {
        // 「永远跳」的噪音:任何根都剪(版本仓/依赖/回收站/NAS/$系统)。
        for n in [".git", "node_modules", "$Recycle.Bin", "@eaDir", "#recycle", ".venv"] {
            assert!(skip_dir_always(n), "{n} 应永远剪");
        }
        // OS 目录黑名单:只在整盘扫描时叠加,本身不是「永远跳」。
        for n in ["windows", "program files", "library", "boot", "recovery", "intel"] {
            assert!(skip_dir_os(n), "{n} 应属 OS 黑名单");
            assert!(!skip_dir_always(n), "{n} 不该被永远跳(用户同名文件夹要保留)");
        }
        // 核心诉求:用户自己挑的文件夹里名叫 library/boot 的子目录 = 真数据,不许永远跳。
        for n in ["library", "boot", "applications", "我的资料", "项目"] {
            assert!(!skip_dir_always(n), "{n} 在显式文件夹里应被归类进库");
        }
    }

    #[test]
    fn macos_packages_always_skipped_but_dotted_user_dirs_kept() {
        // mac 包/库目录(Finder 里像单个文件、内部成千上万碎文件)→ 永远跳,治 macOS 盘点慢。
        for n in [
            "Photos Library.photoslibrary", "Polaris.app", "MyKit.framework",
            "Project.xcodeproj", "Movie.fcpbundle", "Some.bundle", "Debug.dSYM",
        ] {
            assert!(is_macos_package_dir(n), "{n} 应判为 mac 包目录");
            assert!(skip_dir_always(n), "{n} 应永远跳");
        }
        // 名字里带点、但不是包扩展的普通用户目录 → 绝不误伤。
        for n in ["v1.2", "report.final", "我的资料", "data.backup", "2024.照片", "node_modules"] {
            assert!(!is_macos_package_dir(n), "{n} 不该被当 mac 包误跳");
        }
        // macOS 根级系统目录只在整盘扫 `/` 时剪(进 OS 黑名单),本身不「永远跳」(免误伤用户同名夹)。
        for n in ["system", "private", "cores"] {
            assert!(skip_dir_os(n), "{n} 应属整盘扫的 OS 黑名单");
            assert!(!skip_dir_always(n), "{n} 不该被永远跳");
        }
    }

    /// 真机验证(默认 `#[ignore]`,只在手动跑时执行):
    /// `cargo test --manifest-path src-tauri/Cargo.toml --lib scan_real_z_drive -- --ignored --nocapture`
    ///
    /// 用**真实的剪枝判定 + 真实的工业级调度器**(`WorkQueue` + 多核 worker)遍历映射的 Z 盘,
    /// 只计数不写库(绝不碰用户真实 fable.db)。验证三件事:① Z: 被判为远程盘 → 不当系统盘狠剪;
    /// ② 整盘可达、能一路扫进去(报文件数/字节数/类型分布/顶层分布);③ 量化老规则(heavy_prune)
    /// 本会多丢多少目录。带 4 分钟墙钟上限,超时报「已扫到的量 + 仍在继续」,证明吞吐与可达性。
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn scan_real_z_drive() {
        use std::collections::BTreeMap;
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};

        let root = r"Z:\";
        if !Path::new(root).is_dir() {
            eprintln!("Z: 未挂载,跳过本测试");
            return;
        }
        // ① 远程盘判定 + 剪枝档位
        let remote = is_remote_root(root);
        let heavy = is_os_disk_root(root) && !remote;
        eprintln!("──────── Z 盘盘点真机验证 ────────");
        eprintln!("is_remote_root(Z:\\) = {remote}   (期望 true:映射的 NAS 网络盘)");
        eprintln!("heavy_prune        = {heavy}   (期望 false:不当系统盘狠剪,里面全归类)");
        assert!(remote, "Z: 应被判为远程网络盘");
        assert!(!heavy, "远程盘不应启用 OS 目录黑名单重剪");

        // ② 真调度器 + 真剪枝,多核计数遍历(复刻 scan_root,只把写库换成累加计数)。
        let queue = Arc::new(crate::fable::sched::WorkQueue::new(vec![PathBuf::from(root)]));
        let workers = crate::fable::worker_count();
        queue.set_live_workers(workers);
        let files = Arc::new(AtomicU64::new(0));
        let bytes = Arc::new(AtomicU64::new(0));
        let ndirs = Arc::new(AtomicU64::new(0));
        let extra_pruned = Arc::new(AtomicU64::new(0)); // 老 heavy_prune 本会多丢的目录数
        let by_kind: Arc<Mutex<BTreeMap<&'static str, (u64, u64)>>> =
            Arc::new(Mutex::new(BTreeMap::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let started = Instant::now();

        let mut handles = Vec::new();
        for _ in 0..workers {
            let (queue, files, bytes, ndirs, extra_pruned, by_kind, stop) = (
                queue.clone(),
                files.clone(),
                bytes.clone(),
                ndirs.clone(),
                extra_pruned.clone(),
                by_kind.clone(),
                stop.clone(),
            );
            handles.push(std::thread::spawn(move || {
                while let Some(job) = queue.pop() {
                    let dir = job.item;
                    if stop.load(Ordering::Relaxed) {
                        queue.complete(); // 超时:不再下钻,快速把队列抽干
                        continue;
                    }
                    if let Ok(rd) = std::fs::read_dir(&dir) {
                        for entry in rd.flatten() {
                            let Ok(ft) = entry.file_type() else { continue };
                            if ft.is_symlink() {
                                continue;
                            }
                            let name = crate::fable::decode_fs(&entry.file_name());
                            if ft.is_dir() {
                                if skip_dir_always(&name) {
                                    continue; // 永远跳的噪音(@eaDir/#recycle/.git…)
                                }
                                // 老代码对远程盘也 heavy_prune,会再剪 skip_dir_os —— 量化它的误伤。
                                if skip_dir_os(&name) {
                                    extra_pruned.fetch_add(1, Ordering::Relaxed);
                                }
                                ndirs.fetch_add(1, Ordering::Relaxed);
                                queue.push(entry.path());
                            } else if ft.is_file() {
                                if let Ok(m) = entry.metadata() {
                                    let ext = entry
                                        .path()
                                        .extension()
                                        .map(|e| e.to_string_lossy().to_ascii_lowercase())
                                        .unwrap_or_default();
                                    let kind = classify(&ext);
                                    let sz = on_disk_size(&entry.path(), &m, true);
                                    files.fetch_add(1, Ordering::Relaxed);
                                    bytes.fetch_add(sz, Ordering::Relaxed);
                                    let mut bk = by_kind.lock().unwrap();
                                    let e = bk.entry(kind).or_insert((0, 0));
                                    e.0 += 1;
                                    e.1 += sz;
                                }
                            }
                        }
                    }
                    queue.complete();
                }
                queue.worker_exited();
            }));
        }

        // 主线程:每 5s 报一次进度;到 4 分钟墙钟上限则置 stop,让 worker 收尾。
        let cap = Duration::from_secs(240);
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let (inflight, qlen, live) = queue.stats();
            let done = inflight == 0 && (qlen == 0 || live == 0);
            if started.elapsed().as_millis() % 5000 < 600 {
                eprintln!(
                    "  …已扫 {} 文件 / {:.1} GB / {} 目录(队列 {qlen},耗时 {:.0}s)",
                    files.load(Ordering::Relaxed),
                    bytes.load(Ordering::Relaxed) as f64 / 1e9,
                    ndirs.load(Ordering::Relaxed),
                    started.elapsed().as_secs_f64(),
                );
            }
            if done {
                break;
            }
            if started.elapsed() > cap && !stop.load(Ordering::Relaxed) {
                eprintln!("  (到 4 分钟上限,停止下钻,抽干在途…)");
                stop.store(true, Ordering::Relaxed);
            }
        }
        for h in handles {
            let _ = h.join();
        }

        let f = files.load(Ordering::Relaxed);
        let b = bytes.load(Ordering::Relaxed);
        let finished = !stop.load(Ordering::Relaxed);
        eprintln!("──────── 结果 ────────");
        eprintln!(
            "{} · 文件 {f} 个 · 总量 {:.1} GB · 目录 {} 个 · 耗时 {:.0}s",
            if finished { "扫完整盘" } else { "达上限(部分)" },
            b as f64 / 1e9,
            ndirs.load(Ordering::Relaxed),
            started.elapsed().as_secs_f64(),
        );
        eprintln!("按类型分布(文件数 / 体量):");
        let bk = by_kind.lock().unwrap();
        let mut rows: Vec<_> = bk.iter().collect();
        rows.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
        for (kind, (cnt, sz)) in rows {
            eprintln!("  {kind:>8}: {cnt:>9} 个 · {:>8.1} GB", *sz as f64 / 1e9);
        }
        let ep = extra_pruned.load(Ordering::Relaxed);
        eprintln!(
            "修复影响:老代码(把 Z: 当系统盘 heavy_prune)本会再整棵剪掉 {ep} 个目录\
             (名叫 system/library/private/bin/boot… 的 NAS 共享/文件夹),现在全部纳入。"
        );
        assert!(f > 0, "Z: 应至少扫到一些文件");
    }

    /// 网络盘判定:UNC 路径恒为远程;不存在的盘符根 GetDriveType 返回 NO_ROOT_DIR(非 REMOTE)
    /// → false。真实映射的 NAS 盘符在真机上才返回 true(依赖系统驱动器表,这里只回归确定分支)。
    #[cfg(windows)]
    #[test]
    fn unc_paths_are_remote() {
        assert!(is_remote_root(r"\\nas\share"));
        assert!(is_remote_root("//nas/share/sub"));
        assert!(!is_remote_root(r"C:\Users\me")); // 本地系统盘 = 非远程
    }

    /// 核心回归:映射的 NAS 盘符虽是 `X:\` 形状(被 [`is_os_disk_root`] 判为整盘),但若是远程盘
    /// 就**不该**叠加 OS 目录黑名单 —— 否则 library/system/private 等 NAS 常见共享名被整棵丢掉。
    /// 这里用纯逻辑组合断言,不依赖具体盘是否真挂着。
    #[test]
    fn remote_disk_root_skips_heavy_prune() {
        // heavy_prune 的真值 = is_os_disk_root && !is_remote_root。非远程的 `/`、`C:\` 仍重剪。
        assert!(is_os_disk_root("/") && !is_remote_root("/"));
        assert!(is_os_disk_root(r"C:\") && !is_remote_root(r"C:\"));
        // UNC 永远是远程 → 即便 is_os_disk_root(对 UNC 为 false)也不会重剪,语义一致。
        #[cfg(windows)]
        assert!(is_remote_root(r"\\nas\share"));
    }

    /// 扩展长度前缀剥离:网络盘 canonicalize 出的 `\\?\UNC\host\share` 必须还原成合法 `\\host\share`
    /// (旧实现只剥 `\\?\` 会留下非法的 `UNC\host\share`,导致这条根的文件打不开、对账失效)。
    #[test]
    fn strips_unc_extended_prefix() {
        assert_eq!(strip_extended_prefix(r"\\?\UNC\100.78.103.101\tx"), r"\\100.78.103.101\tx");
        assert_eq!(strip_extended_prefix(r"\\?\UNC\nas\share\sub"), r"\\nas\share\sub");
        assert_eq!(strip_extended_prefix(r"\\?\C:\Users\me"), r"C:\Users\me");
        assert_eq!(strip_extended_prefix(r"C:\already\normal"), r"C:\already\normal");
        assert_eq!(strip_extended_prefix(r"\\nas\share"), r"\\nas\share");
    }

    #[test]
    fn is_os_disk_root_only_whole_disks() {
        for r in [r"C:\", "C:", r"D:\", "/", "z:/"] {
            assert!(is_os_disk_root(r), "{r} 应判为整盘根");
        }
        for r in [r"C:\Users\me\proj", "/data", "/volume1/photos", r"D:\我的资料", "/Volumes/USB"] {
            assert!(!is_os_disk_root(r), "{r} 是文件夹/卷,不是整盘根(里面的全归类)");
        }
    }

    #[test]
    fn covered_by_respects_pruned_path() {
        // 普通嵌套:扫父能到子 → 视为已覆盖,子根可去重。
        assert!(covered_by(r"C:\data", r"C:\data\sub\deep"));
        assert!(covered_by("/mnt/a", "/mnt/a/b/c"));
        // 相同路径。
        assert!(covered_by(r"C:\data", r"C:\data"));
        // 不在父之内。
        assert!(!covered_by(r"C:\data", r"D:\data\x"));
        // 关键:子根埋在 appdata 内,扫父会在 appdata 处剪枝、到不了 → 不算覆盖,须保留。
        assert!(!covered_by(
            r"C:\Users\me",
            r"C:\Users\me\AppData\Roaming\app\Downloads"
        ));
        // 前缀像但非真子目录(data2 不是 data 的子目录)→ 不覆盖。
        assert!(!covered_by(r"C:\data", r"C:\data2\x"));
    }

    // ───────────────────────── 增量盘点 ─────────────────────────

    #[test]
    fn parent_rel_walks_up_one_level() {
        assert_eq!(parent_rel("a/b/c"), "a/b");
        assert_eq!(parent_rel("a/b"), "a");
        assert_eq!(parent_rel("a"), ""); // 顶层 → 根
        assert_eq!(parent_rel(""), ""); // 根 → 根
        assert_eq!(parent_rel("资料/项目/稿"), "资料/项目"); // CJK 同样按 '/' 分段
    }

    #[test]
    fn rel_of_is_root_relative_slash_path() {
        let root = Path::new(r"C:\kb");
        assert_eq!(rel_of(Path::new(r"C:\kb"), root), ""); // 根自身 = ""
        assert_eq!(rel_of(Path::new(r"C:\kb\a"), root), "a");
        assert_eq!(rel_of(Path::new(r"C:\kb\a\b"), root), "a/b"); // 反斜杠归一成 '/'
    }

    /// 增量「跳过没变目录」最易错的一步:把某目录的**直属**文件 seen 刷成本轮代际,
    /// 而**不**波及它的子目录文件(那些由各自目录处理)、也不碰兄弟目录。这里用内存 SQLite
    /// 照搬 writer 里的区间 + instr 直属过滤,断言只命中直属、CJK 路径也对。
    #[test]
    fn skip_bump_touches_only_direct_children() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files(root_id INTEGER, relpath TEXT, seen INTEGER);
             INSERT INTO files VALUES
               (1,'a.txt',1),          -- 根直属
               (1,'d/b.txt',1),        -- d 直属(应被刷)
               (1,'d/e/c.txt',1),      -- d 的孙级(不该刷,留给 d/e 自己)
               (1,'d2/x.txt',1),       -- 兄弟目录 d2(不该刷)
               (1,'资料/f.txt',1),     -- CJK 直属(应被刷)
               (1,'资料/子/g.txt',1);  -- CJK 孙级(不该刷)
             ",
        )
        .unwrap();
        let gen = 99i64;
        // 与 writer 的 bump_files 完全同款 SQL。
        let bump = |conn: &rusqlite::Connection, rel: &str| {
            let lo = format!("{rel}/");
            let hi = format!("{rel}0");
            let off = rel.chars().count() as i64 + 2;
            conn.execute(
                "UPDATE files SET seen=?1 WHERE root_id=?2
                 AND relpath>=?3 AND relpath<?4 AND instr(substr(relpath,?5),'/')=0",
                rusqlite::params![gen, 1i64, lo, hi, off],
            )
            .unwrap();
        };
        bump(&conn, "d");
        bump(&conn, "资料");
        // 根直属用另一条(无前缀区间)。
        conn.execute(
            "UPDATE files SET seen=?2 WHERE root_id=?1 AND instr(relpath,'/')=0",
            rusqlite::params![1i64, gen],
        )
        .unwrap();

        let seen = |rel: &str| -> i64 {
            conn.query_row("SELECT seen FROM files WHERE relpath=?1", [rel], |r| r.get(0)).unwrap()
        };
        assert_eq!(seen("a.txt"), gen, "根直属应被刷");
        assert_eq!(seen("d/b.txt"), gen, "d 的直属应被刷");
        assert_eq!(seen("资料/f.txt"), gen, "CJK 直属应被刷");
        assert_eq!(seen("d/e/c.txt"), 1, "孙级不该被 d 的 bump 波及(留给 d/e)");
        assert_eq!(seen("资料/子/g.txt"), 1, "CJK 孙级同理不该被波及");
        assert_eq!(seen("d2/x.txt"), 1, "兄弟目录 d2 绝不被 d 的区间扫到");
    }

    /// 端到端(默认 `#[ignore]`,**单独**手动跑,避开进程级 DB / MIGRATED 竞争):
    /// `cargo test --manifest-path src-tauri/Cargo.toml --lib incremental_rescan_e2e -- --ignored --exact --nocapture`
    ///
    /// 真盘一棵临时目录树(库指到临时文件,绝不碰用户库)→ 改动 → 增量重扫,验证:
    /// ① 改过目录里「新增/删除」被正确反映;② 没变目录里的文件**不被代际对账误删**;
    /// ③ 文件总数/dirs 缓存正确;④ 记录的取舍:某文件被「原地改写、没碰其所在目录」时增量
    /// 察觉不到(其 DB mtime 不变),而一次完整盘点(full=true)能补回。
    #[test]
    #[ignore]
    fn incremental_rescan_e2e() {
        use std::io::Write;
        let base = std::env::temp_dir().join(format!("polaris_inv_e2e_{}", std::process::id()));
        let root = base.join("root");
        let db = base.join("fable_test.db");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(root.join("A")).unwrap();
        std::fs::create_dir_all(root.join("B").join("SUB")).unwrap();
        let write = |p: &Path, s: &str| {
            let mut f = std::fs::File::create(p).unwrap();
            f.write_all(s.as_bytes()).unwrap();
        };
        write(&root.join("top.txt"), "top");
        write(&root.join("A").join("a1.txt"), "a1");
        write(&root.join("A").join("a2.txt"), "a2");
        write(&root.join("B").join("b1.txt"), "b1");
        write(&root.join("B").join("SUB").join("s1.txt"), "s1-original");

        std::env::set_var("POLARIS_FABLE_DB", &db);
        let root_s = root.to_string_lossy().to_string();
        let empty = HashSet::new();
        let noop = |_f: u64, _b: u64| {};

        // ① 首扫(无缓存 → 等同全量):6 个文件,4 个目录(""/A/B/B/SUB)。
        let s1 = scan_root(&root_s, &empty, false, &noop).unwrap();
        assert_eq!(s1.files, 5, "首扫应有 5 个文件");
        let conn = open_db().unwrap();
        let root_id: i64 =
            conn.query_row("SELECT id FROM roots ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).unwrap();
        let nfiles = |c: &rusqlite::Connection| -> i64 {
            c.query_row("SELECT COUNT(*) FROM files WHERE root_id=?1", [root_id], |r| r.get(0)).unwrap()
        };
        let ndirs = |c: &rusqlite::Connection| -> i64 {
            c.query_row("SELECT COUNT(*) FROM dirs WHERE root_id=?1", [root_id], |r| r.get(0)).unwrap()
        };
        let has = |c: &rusqlite::Connection, rel: &str| -> bool {
            c.query_row(
                "SELECT COUNT(*) FROM files WHERE root_id=?1 AND relpath=?2",
                rusqlite::params![root_id, rel],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
                > 0
        };
        let mtime_of = |c: &rusqlite::Connection, rel: &str| -> i64 {
            c.query_row(
                "SELECT mtime FROM files WHERE root_id=?1 AND relpath=?2",
                rusqlite::params![root_id, rel],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(nfiles(&conn), 5);
        assert_eq!(ndirs(&conn), 4, "应缓存 4 个目录(根/A/B/B/SUB)");
        let s1_mtime_scan1 = mtime_of(&conn, "B/SUB/s1.txt");

        // 等过 1 秒边界(mtime 按秒存),保证后续改动的目录 mtime 与首扫记录不同秒,增量必察觉。
        std::thread::sleep(std::time::Duration::from_millis(1100));

        // ② 改动:A 加文件(A 目录 mtime 变)、删 B/b1(B 目录 mtime 变)、**原地改写** B/SUB/s1
        //    (只动文件、不动 B/SUB 目录 → 增量该「跳过 B/SUB」从而察觉不到 s1 的内容变化)。
        write(&root.join("A").join("a3.txt"), "a3-new");
        std::fs::remove_file(root.join("B").join("b1.txt")).unwrap();
        {
            // 原地改写:truncate + 写,不删不改名,B/SUB 目录 mtime 不变。
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(root.join("B").join("SUB").join("s1.txt"))
                .unwrap();
            f.write_all(b"s1-modified-in-place-much-longer").unwrap();
        }

        // ③ 增量重扫。
        let s2 = scan_root(&root_s, &empty, false, &noop).unwrap();
        assert!(has(&conn, "A/a3.txt"), "增量应发现新增文件 A/a3.txt");
        assert!(!has(&conn, "B/b1.txt"), "增量应删除已消失的 B/b1.txt");
        assert!(has(&conn, "top.txt"), "没变目录里的 top.txt 绝不能被代际对账误删");
        assert!(has(&conn, "A/a1.txt") && has(&conn, "A/a2.txt"), "A 里旧文件仍在");
        assert!(has(&conn, "B/SUB/s1.txt"), "没变目录里的 s1 仍在");
        assert_eq!(nfiles(&conn), 5, "top + A(a1/a2/a3) + B/SUB/s1 = 5");
        assert_eq!(s2.files, 5, "增量汇报的文件数含跳过子树,口径不缩水");
        assert_eq!(
            mtime_of(&conn, "B/SUB/s1.txt"),
            s1_mtime_scan1,
            "记录的取舍:原地改写、没碰目录 → 增量察觉不到,s1 的 DB mtime 应仍是首扫的旧值"
        );

        // ④ 完整盘点(full=true)忽略缓存逐目录重扫 → 补回 s1 的新 mtime。
        let _s3 = scan_root(&root_s, &empty, true, &noop).unwrap();
        assert!(
            mtime_of(&conn, "B/SUB/s1.txt") > s1_mtime_scan1,
            "完整盘点应补回原地改写文件的新 mtime"
        );

        drop(conn);
        std::env::remove_var("POLARIS_FABLE_DB");
        let _ = std::fs::remove_dir_all(&base);
    }
}
