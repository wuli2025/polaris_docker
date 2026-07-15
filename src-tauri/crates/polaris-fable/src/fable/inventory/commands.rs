use super::*;

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
pub(crate) fn inventory_roots(explicit: Option<String>) -> Vec<String> {
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
pub(crate) fn app_data_roots() -> Vec<ScanRootInfo> {
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
                out.push(ScanRootInfo {
                    path,
                    label: (*label).to_string(),
                    default_on: true,
                });
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
        .map(|r| {
            let t = r.trim_end_matches(['/', '\\']);
            // Win32 下剥掉尾分隔符会把 "C:\" 变成 "C:" = 盘符相对路径(指向该盘的**当前目录**,
            // 通常就是进程 CWD)→ 整盘勾选被静默偷换成扫工作目录。补回 "\" 还原真正的盘根
            // (同 folders.rs disk_used_bytes 的重建口径)。
            let b = t.as_bytes();
            if cfg!(windows) && b.len() == 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
                format!("{t}\\")
            } else {
                t.to_string()
            }
        })
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
        let secs = if is_remote_root(&r) {
            probe_secs().max(25)
        } else {
            probe_secs()
        };
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
        // 单根失败逐条记 (根, 错误):以前只留 last_err 且仅在颗粒无收时上报,「C 盘成功、D 盘
        // 失败」会被「盘点完成」完全吞掉 → 现在随 done 事件如实带回(同 unreachable 的做法)。
        let mut failed: Vec<(String, String)> = Vec::new();
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
                Err(e) => failed.push((r.clone(), e)),
            }
        }
        if cancelled() {
            emit(&app, json!({ "kind": "error", "message": "已取消" }));
        } else if acc_files == 0 {
            let msg = match (failed.is_empty(), unreachable.is_empty()) {
                (false, _) => failed
                    .iter()
                    .map(|(root, e)| format!("{root}:{e}"))
                    .collect::<Vec<_>>()
                    .join(";"),
                (true, false) => format!(
                    "这些位置连接不上,已跳过:{} —— 检查网络 / Tailscale / 外置盘连接后重新盘点即可。",
                    unreachable.join("、")
                ),
                (true, true) => "未扫描到任何文件".into(),
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
                    // 部分根盘点失败(权限 / DB 写入错等,可达但扫不成)也逐条带回,前端据此提示
                    // 「XX 这次没扫成」,而不是让「盘点完成」掩盖半截结果。
                    "failed": failed
                        .iter()
                        .map(|(root, e)| json!({ "root": root, "error": e }))
                        .collect::<Vec<Value>>(),
                    "full": full,
                }),
            );
        }
    });
    Ok(())
}
