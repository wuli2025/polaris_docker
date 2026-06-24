//! 「盘管理」— NAS 网络盘(SMB)的记忆与一键映射。
//!
//! 背景:盘点(文件清点)要扫 NAS 上的资料,得先把 SMB 共享映射成 Windows 盘符(net use)。
//! 用户希望「记住我登陆过的 NAS、记住登陆方式,下次点一下就连上」。本模块:
//!   1. 持久化 NAS 连接档(主机/共享/账号/密码/偏好盘符)→ `~/Polaris/data/nas.json`(仅本机)。
//!   2. 自动发现「之前登陆过 / 远程登陆过」的网络盘(枚举当前映射 + 解析 `net use` 的记忆),
//!      合并进清单,让 Windows 早就记着的连接也能一键重连。
//!   3. `nas_connect` / `nas_disconnect` 用 `net use` 映射/断开盘符;映射后该盘符立即可被盘点扫到。
//!
//! 凭据按项目现有做法**明文存本地**(同 sense.json / 飞书凭据),不上传。映射动作仅 Windows
//! 桌面端真正执行;server/docker flavor 只做记忆与清单(连接返回平台提示),保证编译与降级。

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::process::Command;

/// 一条 NAS 连接档(持久化进 nas.json)。
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct NasRecord {
    /// 稳定 id;空则按 host+share 生成(同一目标只记一条,天然去重)。
    #[serde(default)]
    pub id: String,
    /// 展示名。
    #[serde(default)]
    pub label: String,
    /// 主机/IP,如 `100.78.103.101` 或 `DiskStation`。
    #[serde(default)]
    pub host: String,
    /// 共享名,如 `tx`(不含反斜杠)。
    #[serde(default)]
    pub share: String,
    #[serde(default)]
    pub username: String,
    /// 明文密码(仅本机);保存表单留空 = 沿用旧密码。
    #[serde(default)]
    pub password: String,
    /// 偏好盘符,单字母如 `Z`(可空 = 自动挑空闲盘符)。
    #[serde(default)]
    pub drive: String,
    /// 重启后保持映射。
    #[serde(default = "default_true")]
    pub persistent: bool,
    /// 上次连接成功时间(unix 秒)。
    #[serde(default)]
    pub last_connected: Option<i64>,
}

fn default_true() -> bool {
    true
}

/// 发给前端的一条 NAS(档案 + 运行态;不回传密码)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NasView {
    pub id: String,
    pub label: String,
    pub host: String,
    pub share: String,
    pub username: String,
    pub has_password: bool,
    pub drive: String,
    pub persistent: bool,
    pub last_connected: Option<i64>,
    /// `\\host\share`(或仅 `\\host`)。
    pub unc: String,
    /// 当前是否已映射成盘符。
    pub connected: bool,
    /// 当前映射到哪个盘符(如 `Z`)。
    pub current_drive: Option<String>,
    /// 这条是从系统里自动发现、尚未保存的(连一下即记住)。
    pub discovered: bool,
    /// 一句话状态。
    pub status: String,
}

// ───────────────────────── 持久化 ─────────────────────────

fn data_dir() -> Option<PathBuf> {
    directories::UserDirs::new().map(|u| u.home_dir().join("Polaris").join("data"))
}
fn store_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join("nas.json"))
}

fn load() -> Vec<NasRecord> {
    let Some(p) = store_path() else {
        return vec![];
    };
    let Ok(txt) = fs::read_to_string(&p) else {
        return vec![];
    };
    serde_json::from_str(&txt).unwrap_or_default()
}

fn save_all(list: &[NasRecord]) -> Result<(), String> {
    let p = store_path().ok_or("无法定位数据目录")?;
    if let Some(dir) = p.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let txt = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
    let tmp = p.with_extension("json.tmp");
    fs::write(&tmp, txt).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &p).map_err(|e| e.to_string())
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ───────────────────────── 规整辅助 ─────────────────────────

/// host+share → 稳定 id(同目标只一条)。
fn make_id(host: &str, share: &str) -> String {
    let raw = format!("{}/{}", host.trim().to_lowercase(), share.trim().to_lowercase());
    let id: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let trimmed = id.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "nas".into()
    } else {
        trimmed
    }
}

/// `\\host\share`,share 为空时退回 `\\host`。
fn unc_of(rec: &NasRecord) -> String {
    let host = rec.host.trim().trim_start_matches(r"\\");
    let share = rec.share.trim().trim_matches('\\');
    if share.is_empty() {
        format!(r"\\{host}")
    } else {
        format!(r"\\{host}\{share}")
    }
}

fn unc_key(unc: &str) -> String {
    unc.trim_end_matches('\\').to_lowercase()
}

/// 拆 `\\host\share` → (host, share)。
fn split_unc(unc: &str) -> (String, String) {
    let body = unc.trim_start_matches('\\');
    let mut it = body.splitn(2, '\\');
    let host = it.next().unwrap_or("").to_string();
    let share = it.next().unwrap_or("").trim_end_matches('\\').to_string();
    (host, share)
}

fn default_label(host: &str, share: &str) -> String {
    if share.is_empty() {
        host.to_string()
    } else {
        format!("{host} · {share}")
    }
}

fn view_of(rec: &NasRecord, current_drive: Option<String>, discovered: bool) -> NasView {
    let unc = unc_of(rec);
    let connected = current_drive.is_some();
    let status = if connected {
        format!("已连接 · {}:", current_drive.clone().unwrap_or_default())
    } else if rec.last_connected.is_some() {
        "已记住 · 未连接,点「连接」一键挂载".into()
    } else if discovered {
        "系统里发现的连接 · 点「连接」即记住".into()
    } else {
        "未连接".into()
    };
    NasView {
        id: rec.id.clone(),
        label: if rec.label.trim().is_empty() {
            default_label(&rec.host, &rec.share)
        } else {
            rec.label.clone()
        },
        host: rec.host.clone(),
        share: rec.share.clone(),
        username: rec.username.clone(),
        has_password: !rec.password.is_empty(),
        drive: rec.drive.clone(),
        persistent: rec.persistent,
        last_connected: rec.last_connected,
        unc,
        connected,
        current_drive,
        discovered,
        status,
    }
}

// ───────────────────────── 命令 ─────────────────────────

/// 列出 NAS:已保存档 + 系统里自动发现的(当前映射 ∪ net use 记忆),并标注实时连接态。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn nas_list() -> Vec<NasView> {
    let saved = load();
    let connected = connected_map(); // unc_key → 盘符
    let mut views = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for rec in &saved {
        let key = unc_key(&unc_of(rec));
        seen.insert(key.clone());
        let cur = connected.get(&key).cloned();
        views.push(view_of(rec, cur, false));
    }

    // 自动发现:当前已映射的网络盘 + Windows 记忆里的连接(含已断开)。
    for unc in discovered_uncs() {
        let key = unc_key(&unc);
        if seen.contains(&key) || key.ends_with('$') {
            continue; // 已保存,或隐藏管理共享(C$/ADMIN$/IPC$)
        }
        seen.insert(key.clone());
        let (host, share) = split_unc(&unc);
        if host.is_empty() {
            continue;
        }
        let rec = NasRecord {
            id: make_id(&host, &share),
            label: default_label(&host, &share),
            host,
            share,
            persistent: true,
            ..Default::default()
        };
        let cur = connected.get(&key).cloned();
        views.push(view_of(&rec, cur, true));
    }

    views
}

/// 保存/更新一条 NAS 档(留空密码 = 沿用旧密码)。返回不含密码的档。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn nas_save(record: NasRecord) -> Result<NasRecord, String> {
    let mut rec = record;
    rec.host = rec.host.trim().trim_start_matches(r"\\").to_string();
    rec.share = rec.share.trim().trim_matches('\\').to_string();
    if rec.host.is_empty() {
        return Err("请填主机地址(IP 或主机名)".into());
    }
    rec.drive = rec
        .drive
        .trim()
        .trim_end_matches(':')
        .to_uppercase()
        .chars()
        .take(1)
        .collect();
    if rec.id.trim().is_empty() {
        rec.id = make_id(&rec.host, &rec.share);
    }
    if rec.label.trim().is_empty() {
        rec.label = default_label(&rec.host, &rec.share);
    }

    let mut list = load();
    if let Some(existing) = list.iter_mut().find(|r| r.id == rec.id) {
        if rec.password.is_empty() {
            rec.password = existing.password.clone();
        }
        rec.last_connected = existing.last_connected;
        *existing = rec.clone();
    } else {
        list.push(rec.clone());
    }
    save_all(&list)?;
    rec.password = String::new();
    Ok(rec)
}

/// 忘记一条 NAS 档(只删记忆,不动当前映射)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn nas_forget(id: String) -> Result<String, String> {
    let mut list = load();
    let before = list.len();
    list.retain(|r| r.id != id);
    if list.len() == before {
        return Ok("没有这条记录,无需删除。".into());
    }
    save_all(&list)?;
    Ok("已从盘管理移除(已映射的盘不受影响)。".into())
}

/// 连接(映射成盘符)。成功后把这条档记住并盖上「上次连接」时间(发现的连接也就此记住)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn nas_connect(record: NasRecord) -> Result<String, String> {
    let mut rec = record;
    rec.host = rec.host.trim().trim_start_matches(r"\\").to_string();
    rec.share = rec.share.trim().trim_matches('\\').to_string();
    if rec.host.is_empty() {
        return Err("请填主机地址(IP 或主机名)".into());
    }
    if rec.share.is_empty() {
        return Err("请填共享名(NAS 上要挂载的那个共享文件夹)".into());
    }

    let msg = do_connect(&rec)?;

    // 连成功 → 记住 + 盖时间。密码留空时沿用旧密码(发现的连接首连可能没存密码,靠 Windows 记忆)。
    let id = if rec.id.trim().is_empty() {
        make_id(&rec.host, &rec.share)
    } else {
        rec.id.clone()
    };
    let now = now_secs();
    let mut list = load();
    if let Some(e) = list.iter_mut().find(|r| r.id == id) {
        e.last_connected = Some(now);
        if !rec.password.is_empty() {
            e.password = rec.password.clone();
        }
        if !rec.drive.trim().is_empty() {
            e.drive = rec.drive.trim().trim_end_matches(':').to_uppercase();
        }
    } else {
        let mut nr = rec.clone();
        nr.id = id;
        if nr.label.trim().is_empty() {
            nr.label = default_label(&nr.host, &nr.share);
        }
        nr.last_connected = Some(now);
        list.push(nr);
    }
    let _ = save_all(&list);
    Ok(msg)
}

/// 断开(取消盘符映射)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn nas_disconnect(record: NasRecord) -> Result<String, String> {
    do_disconnect(&record)
}

// ───────────────────────── 平台实现:Windows ─────────────────────────

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 当前已映射的网络盘:unc_key → 盘符字母。
#[cfg(windows)]
fn connected_map() -> std::collections::HashMap<String, String> {
    let mut m = std::collections::HashMap::new();
    for (letter, unc) in enum_remote_drives() {
        m.insert(unc_key(&unc), letter.to_string());
    }
    m
}

/// 自动发现的 UNC:当前已映射 ∪ Windows 记忆(net use,含已断开)。
#[cfg(windows)]
fn discovered_uncs() -> Vec<String> {
    let mut out: Vec<String> = enum_remote_drives().into_iter().map(|(_, u)| u).collect();
    for u in remembered_via_net_use() {
        let k = unc_key(&u);
        if !out.iter().any(|x| unc_key(x) == k) {
            out.push(u);
        }
    }
    out
}

/// 枚举当前所有网络盘 → (盘符, UNC)。纯内核态(GetLogicalDrives + WNetGetConnectionW),不卡死。
#[cfg(windows)]
fn enum_remote_drives() -> Vec<(char, String)> {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    use windows_sys::Win32::System::WindowsProgramming::DRIVE_REMOTE;

    let mut out = Vec::new();
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = format!("{letter}:\\");
        if unsafe { GetDriveTypeW(to_wide(&root).as_ptr()) } != DRIVE_REMOTE {
            continue;
        }
        if let Some(unc) = drive_unc(letter) {
            out.push((letter, unc));
        }
    }
    out
}

/// 读盘符背后的 UNC(`\\host\share`);非网络盘/未映射返回 None。
#[cfg(windows)]
fn drive_unc(letter: char) -> Option<String> {
    use windows_sys::Win32::NetworkManagement::WNet::WNetGetConnectionW;
    let local = to_wide(&format!("{letter}:")); // 不带结尾反斜杠
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let rc = unsafe { WNetGetConnectionW(local.as_ptr(), buf.as_mut_ptr(), &mut len) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(0);
    let s = String::from_utf16_lossy(&buf[..end]);
    if s.starts_with(r"\\") {
        Some(s)
    } else {
        None
    }
}

/// 解析 `net use` 列表,捞出 Windows 记忆里的连接 UNC(含已断开的)。
/// 与语言无关:不靠表头词,逐行抓 `\\…` 远程名即可。
#[cfg(windows)]
fn remembered_via_net_use() -> Vec<String> {
    let mut cmd = Command::new("net");
    cmd.arg("use");
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let Ok(out) = cmd.output() else {
        return vec![];
    };
    let text = decode_console(&out.stdout);
    let mut found = Vec::new();
    for line in text.lines() {
        for tok in line.split_whitespace() {
            if tok.starts_with(r"\\") && tok.len() > 2 {
                let u = tok.trim_end_matches('\\').to_string();
                let k = unc_key(&u);
                if !found.iter().any(|x: &String| unc_key(x) == k) {
                    found.push(u);
                }
            }
        }
    }
    found
}

/// 控制台输出多为 OEM 代码页(中文系统 GBK);优先 UTF-8,失败按 GBK 兜底,保证错误信息可读。
#[cfg(windows)]
fn decode_console(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => {
            let (cow, _, _) = encoding_rs::GBK.decode(bytes);
            cow.into_owned()
        }
    }
}

/// 挑盘符:偏好优先(没被占),否则从 Z 往 D 找第一个空闲。
#[cfg(windows)]
fn pick_drive(rec: &NasRecord) -> Result<char, String> {
    use windows_sys::Win32::Storage::FileSystem::GetLogicalDrives;
    let mask = unsafe { GetLogicalDrives() };
    let used = |c: u8| {
        let idx = (c.to_ascii_uppercase() - b'A') as u32;
        idx < 26 && mask & (1 << idx) != 0
    };
    if let Some(c) = rec.drive.trim().chars().next() {
        let up = c.to_ascii_uppercase();
        if up.is_ascii_alphabetic() && !used(up as u8) {
            return Ok(up);
        }
    }
    for c in (b'D'..=b'Z').rev() {
        if !used(c) {
            return Ok(c as char);
        }
    }
    Err("没有空闲盘符可用了(请先断开一个网络盘)".into())
}

#[cfg(windows)]
fn do_connect(rec: &NasRecord) -> Result<String, String> {
    let unc = unc_of(rec);
    let key = unc_key(&unc);
    if let Some((letter, _)) = enum_remote_drives().into_iter().find(|(_, u)| unc_key(u) == key) {
        return Ok(format!("已经连上了:{letter}: → {unc}"));
    }
    let drive = pick_drive(rec)?;
    let mut cmd = Command::new("net");
    cmd.arg("use").arg(format!("{drive}:")).arg(&unc);
    if !rec.password.is_empty() {
        cmd.arg(&rec.password);
    }
    if !rec.username.trim().is_empty() {
        cmd.arg(format!("/user:{}", rec.username.trim()));
    }
    cmd.arg(if rec.persistent {
        "/persistent:yes"
    } else {
        "/persistent:no"
    });
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().map_err(|e| format!("启动 net use 失败:{e}"))?;
    if out.status.success() {
        Ok(format!("已连接:{drive}: → {unc}。现在点「盘点」就能扫到它了。"))
    } else {
        let mut err = decode_console(&out.stderr);
        err.push_str(&decode_console(&out.stdout));
        let err = err.trim();
        Err(if err.is_empty() {
            "连接失败:net use 返回错误(请检查主机/共享名/账号密码,以及 NAS 是否可达)".into()
        } else {
            format!("连接失败:{err}")
        })
    }
}

#[cfg(windows)]
fn do_disconnect(rec: &NasRecord) -> Result<String, String> {
    let key = unc_key(&unc_of(rec));
    let Some((letter, _)) = enum_remote_drives().into_iter().find(|(_, u)| unc_key(u) == key)
    else {
        return Ok("这个 NAS 当前没有映射,无需断开。".into());
    };
    let mut cmd = Command::new("net");
    cmd.arg("use").arg(format!("{letter}:")).arg("/delete").arg("/y");
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().map_err(|e| format!("启动 net use 失败:{e}"))?;
    if out.status.success() {
        Ok(format!("已断开 {letter}: 的映射。"))
    } else {
        let mut err = decode_console(&out.stderr);
        err.push_str(&decode_console(&out.stdout));
        Err(format!("断开失败:{}", err.trim()))
    }
}

// ───────────────────────── 平台实现:非 Windows(server/docker/mac)─────────────────────────

#[cfg(not(windows))]
fn connected_map() -> std::collections::HashMap<String, String> {
    std::collections::HashMap::new()
}

#[cfg(not(windows))]
fn discovered_uncs() -> Vec<String> {
    Vec::new()
}

#[cfg(not(windows))]
fn do_connect(_rec: &NasRecord) -> Result<String, String> {
    Err("网络盘映射当前仅 Windows 桌面端支持;此处可记住 NAS,请到 Windows 端连接。".into())
}

#[cfg(not(windows))]
fn do_disconnect(_rec: &NasRecord) -> Result<String, String> {
    Err("网络盘映射当前仅 Windows 桌面端支持。".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_stable_and_sanitized() {
        let a = make_id("100.78.103.101", "tx");
        assert_eq!(a, make_id("  100.78.103.101 ", "TX"), "大小写/空白应规整成同一 id");
        assert!(!a.contains('.') && !a.contains('/') && !a.is_empty());
    }

    #[test]
    fn unc_build_and_split_roundtrip() {
        let rec = NasRecord {
            host: "100.78.103.101".into(),
            share: "tx".into(),
            ..Default::default()
        };
        let unc = unc_of(&rec);
        assert_eq!(unc, r"\\100.78.103.101\tx");
        let (h, s) = split_unc(&unc);
        assert_eq!((h.as_str(), s.as_str()), ("100.78.103.101", "tx"));
        // host-only(无共享)不应崩
        let host_only = NasRecord { host: "nas".into(), ..Default::default() };
        assert_eq!(unc_of(&host_only), r"\\nas");
    }

    #[test]
    fn unc_key_normalizes() {
        assert_eq!(unc_key(r"\\Host\Share\"), r"\\host\share");
    }

    /// 真机集成:跑真实的 Windows 网络盘枚举 + net use 解析,打印 nas_list 结果。
    /// `cargo test -p polaris-app --lib nas -- --ignored --nocapture` 手动跑。
    #[test]
    #[ignore]
    fn real_machine_list() {
        let views = nas_list();
        eprintln!("nas_list 返回 {} 条:", views.len());
        for v in &views {
            eprintln!(
                "  - {} | {} | connected={} drive={:?} discovered={} | {}",
                v.label, v.unc, v.connected, v.current_drive, v.discovered, v.status
            );
        }
    }
}
