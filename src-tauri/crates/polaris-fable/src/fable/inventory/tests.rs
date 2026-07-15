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
    assert_eq!(
        natural_lang("这是一段中文文本,讲的是知识库检索系统"),
        "中文"
    );
    assert_eq!(
        natural_lang("This is an English document about retrieval."),
        "英文"
    );
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
    for n in [
        "Windows",
        "Program Files",
        "AppData",
        "node_modules",
        "$Recycle.Bin",
        "@eaDir",
    ] {
        assert!(skip_dir_scan(n), "{n} 应被剪掉");
    }
    for n in ["Downloads", "Documents", "WeChat Files", "datasets"] {
        assert!(!skip_dir_scan(n), "{n} 不应被剪掉");
    }
}

#[test]
fn two_tier_prune_keeps_user_named_dirs() {
    // 「永远跳」的噪音:任何根都剪(版本仓/依赖/回收站/NAS/$系统)。
    for n in [
        ".git",
        "node_modules",
        "$Recycle.Bin",
        "@eaDir",
        "#recycle",
        ".venv",
    ] {
        assert!(skip_dir_always(n), "{n} 应永远剪");
    }
    // OS 目录黑名单:只在整盘扫描时叠加,本身不是「永远跳」。
    for n in [
        "windows",
        "program files",
        "library",
        "boot",
        "recovery",
        "intel",
    ] {
        assert!(skip_dir_os(n), "{n} 应属 OS 黑名单");
        assert!(
            !skip_dir_always(n),
            "{n} 不该被永远跳(用户同名文件夹要保留)"
        );
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
        "Photos Library.photoslibrary",
        "Polaris.app",
        "MyKit.framework",
        "Project.xcodeproj",
        "Movie.fcpbundle",
        "Some.bundle",
        "Debug.dSYM",
    ] {
        assert!(is_macos_package_dir(n), "{n} 应判为 mac 包目录");
        assert!(skip_dir_always(n), "{n} 应永远跳");
    }
    // 名字里带点、但不是包扩展的普通用户目录 → 绝不误伤。
    for n in [
        "v1.2",
        "report.final",
        "我的资料",
        "data.backup",
        "2024.照片",
        "node_modules",
    ] {
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
    let queue = Arc::new(crate::fable::sched::WorkQueue::new(vec![PathBuf::from(
        root,
    )]));
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
        if finished {
            "扫完整盘"
        } else {
            "达上限(部分)"
        },
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
    assert_eq!(
        strip_extended_prefix(r"\\?\UNC\100.78.103.101\tx"),
        r"\\100.78.103.101\tx"
    );
    assert_eq!(
        strip_extended_prefix(r"\\?\UNC\nas\share\sub"),
        r"\\nas\share\sub"
    );
    assert_eq!(strip_extended_prefix(r"\\?\C:\Users\me"), r"C:\Users\me");
    assert_eq!(
        strip_extended_prefix(r"C:\already\normal"),
        r"C:\already\normal"
    );
    assert_eq!(strip_extended_prefix(r"\\nas\share"), r"\\nas\share");
}

#[test]
fn is_os_disk_root_only_whole_disks() {
    for r in [r"C:\", "C:", r"D:\", "/", "z:/"] {
        assert!(is_os_disk_root(r), "{r} 应判为整盘根");
    }
    for r in [
        r"C:\Users\me\proj",
        "/data",
        "/volume1/photos",
        r"D:\我的资料",
        "/Volumes/USB",
    ] {
        assert!(
            !is_os_disk_root(r),
            "{r} 是文件夹/卷,不是整盘根(里面的全归类)"
        );
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
        conn.query_row("SELECT seen FROM files WHERE relpath=?1", [rel], |r| {
            r.get(0)
        })
        .unwrap()
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
    let root_id: i64 = conn
        .query_row("SELECT id FROM roots ORDER BY id DESC LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    let nfiles = |c: &rusqlite::Connection| -> i64 {
        c.query_row(
            "SELECT COUNT(*) FROM files WHERE root_id=?1",
            [root_id],
            |r| r.get(0),
        )
        .unwrap()
    };
    let ndirs = |c: &rusqlite::Connection| -> i64 {
        c.query_row(
            "SELECT COUNT(*) FROM dirs WHERE root_id=?1",
            [root_id],
            |r| r.get(0),
        )
        .unwrap()
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
    assert!(
        has(&conn, "top.txt"),
        "没变目录里的 top.txt 绝不能被代际对账误删"
    );
    assert!(
        has(&conn, "A/a1.txt") && has(&conn, "A/a2.txt"),
        "A 里旧文件仍在"
    );
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

/// fable_audit 端到端(默认 `#[ignore]`,单独跑):counters 对账 + sample 检出漏收/mtime 漂移 +
/// fix 作废缓存与重置标记。`cargo test ... --lib fable_audit_e2e -- --ignored --exact --nocapture`
#[test]
#[ignore]
fn fable_audit_e2e() {
    use std::io::Write;
    let base = std::env::temp_dir().join(format!("polaris_audit_{}", std::process::id()));
    let root = base.join("root");
    let db = base.join("fable_test.db");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(root.join("d")).unwrap();
    let write = |p: &Path, s: &str| {
        let mut f = std::fs::File::create(p).unwrap();
        f.write_all(s.as_bytes()).unwrap();
    };
    write(&root.join("d").join("keep.txt"), "keep");
    write(&root.join("d").join("drift.txt"), "v1");
    std::env::set_var("POLARIS_FABLE_DB", &db);
    let root_s = root.to_string_lossy().to_string();
    let empty = HashSet::new();
    let noop = |_f: u64, _b: u64| {};

    scan_root(&root_s, &empty, false, &noop).unwrap();

    // counters:干净库应无悬挂引用、无计数漂移。
    let c = fable_audit(Some("counters".into()), None).unwrap();
    assert_eq!(c.dangling_refs, 0);
    assert_eq!(c.roots_count_drift, 0, "刚扫完 roots.files 应与实际一致");
    assert!(c.files_total >= 2);

    // 制造盲区:① 原地改写 drift.txt(不碰目录 mtime)→ 增量察觉不到;② 直接新增 sneak.txt。
    std::thread::sleep(std::time::Duration::from_millis(1100));
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(root.join("d").join("drift.txt"))
            .unwrap();
        f.write_all(b"v2-much-longer-content").unwrap();
    }
    write(&root.join("d").join("sneak.txt"), "sneaked-in");

    // sample:强制抽满,应检出 1 漏收(sneak)+ 1 mtime 漂移(drift)。
    let s = fable_audit(Some("sample".into()), Some(5000)).unwrap();
    assert!(s.dirs_sampled >= 1);
    assert_eq!(s.missing_in_db, 1, "sneak.txt 应报漏收");
    assert_eq!(s.mtime_drift, 1, "drift.txt 原地改写应报 mtime 漂移");

    // fix:同上并落自愈——drift 文件 chunked/ftsed 归零,d 目录缓存作废。
    let conn = open_db().unwrap();
    conn.execute(
        "UPDATE files SET chunked=1, ftsed=1 WHERE relpath='d/drift.txt'",
        [],
    )
    .unwrap();
    let f = fable_audit(Some("fix".into()), Some(5000)).unwrap();
    assert!(f.fixed_dirs >= 1, "有漏收的目录应被作废缓存");
    let drift_chunked: i64 = conn
        .query_row(
            "SELECT chunked FROM files WHERE relpath='d/drift.txt'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(drift_chunked, 0, "漂移文件标记应被重置待重建");

    drop(conn);
    std::env::remove_var("POLARIS_FABLE_DB");
    let _ = std::fs::remove_dir_all(&base);
}

/// 重命名/移动免重嵌端到端(默认 `#[ignore]`,单独跑):把一个已索引文件移到另一目录(同名),
/// 增量重扫后旧行应「改指新路径、保 id 保 chunk」,而非删旧+增新触发重嵌。
/// `cargo test --manifest-path src-tauri/Cargo.toml --lib rename_move_reuses_index -- --ignored --exact --nocapture`
#[test]
#[ignore]
fn rename_move_reuses_index() {
    use std::io::Write;
    let base = std::env::temp_dir().join(format!("polaris_rename_{}", std::process::id()));
    let root = base.join("root");
    let db = base.join("fable_test.db");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(root.join("dir1")).unwrap();
    let content = "hello rename world 内容用于内容指纹核验";
    {
        let mut f = std::fs::File::create(root.join("dir1").join("doc.txt")).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }
    std::env::set_var("POLARIS_FABLE_DB", &db);
    let root_s = root.to_string_lossy().to_string();
    let empty = HashSet::new();
    let noop = |_f: u64, _b: u64| {};

    scan_root(&root_s, &empty, false, &noop).unwrap();
    let conn = open_db().unwrap();
    let file_id: i64 = conn
        .query_row(
            "SELECT id FROM files WHERE relpath='dir1/doc.txt'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    // 模拟「已建索引」:标 chunked=1、写入与磁盘内容一致的指纹、挂一条 chunk。
    let hash = crate::fable::index::content_fingerprint(content);
    conn.execute(
        "UPDATE files SET chunked=1, ftsed=1, content_hash=?1, doc_key='doc' WHERE id=?2",
        rusqlite::params![hash, file_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO chunks(file_id,seq,text,dim,vec) VALUES(?1,0,'x',1,x'00')",
        [file_id],
    )
    .unwrap();

    // 目录 mtime 按秒;睡过 1 秒边界让移动后 dir1/dir2 的 mtime 与首扫不同秒,增量必重读。
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::create_dir_all(root.join("dir2")).unwrap();
    std::fs::rename(
        root.join("dir1").join("doc.txt"),
        root.join("dir2").join("doc.txt"),
    )
    .unwrap();

    // 用完整盘点(full=true)确定性触发对账:不依赖「移动是否改了源目录 mtime」的文件系统行为
    // (增量下若源目录 mtime 未变,旧文件会以幽灵形式留存,由 dedupe_scan 作为兜底清成完全重复)。
    scan_root(&root_s, &empty, true, &noop).unwrap();

    // 同一 id 现落在 dir2/doc.txt,chunked 保持 1、chunk 未删(零重嵌)。
    let new_rel: String = conn
        .query_row("SELECT relpath FROM files WHERE id=?1", [file_id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(new_rel, "dir2/doc.txt", "旧行应改指移动后的新路径");
    let chunked: i64 = conn
        .query_row("SELECT chunked FROM files WHERE id=?1", [file_id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(chunked, 1, "移动后仍标已索引,不触发重嵌");
    let nchunks: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM chunks WHERE file_id=?1",
            [file_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(nchunks, 1, "已建向量随文件一起保留");
    let at_new: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE relpath='dir2/doc.txt'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(at_new, 1, "新路径不应另起一行(占位行已被并入旧行)");
    let at_old: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE relpath='dir1/doc.txt'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(at_old, 0, "旧路径行已迁走");

    drop(conn);
    std::env::remove_var("POLARIS_FABLE_DB");
    let _ = std::fs::remove_dir_all(&base);
}

/// 端到端(默认 `#[ignore]`,单独手动跑):复现并验证修复「一次读失败就永久漏扫」。
/// `cargo test --manifest-path src-tauri/Cargo.toml --lib skipped_subtree_self_heals -- --ignored --exact --nocapture`
///
/// 场景:某子目录在一轮盘点里读失败 / 被看门狗放弃(本测试用「删它的 dirs 缓存行」模拟代际对账
/// 清掉它后留下的状态),而其父目录 mtime 没变。不修的话,下次增量盘点会跳过父目录、邻接表里又
/// 没了这个子目录 → 子树**永久漏扫**(线上「经常有些文件扫不到」的根因)。[`invalidate_skipped_parents`]
/// 把父目录 mtime 置 0 令其下次必被重新 `read_dir`,从而补扫回来。本测试先复现漏扫,再验证自愈。
#[test]
#[ignore]
fn skipped_subtree_self_heals() {
    use std::io::Write;
    let base = std::env::temp_dir().join(format!("polaris_inv_heal_{}", std::process::id()));
    let root = base.join("root");
    let db = base.join("fable_test.db");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(root.join("A").join("SUB")).unwrap();
    let write = |p: &Path, s: &str| {
        let mut f = std::fs::File::create(p).unwrap();
        f.write_all(s.as_bytes()).unwrap();
    };
    write(&root.join("A").join("a1.txt"), "a1");
    write(&root.join("A").join("SUB").join("s1.txt"), "s1");

    std::env::set_var("POLARIS_FABLE_DB", &db);
    let root_s = root.to_string_lossy().to_string();
    let empty = HashSet::new();
    let noop = |_f: u64, _b: u64| {};

    // ① 首扫:2 个文件,4 个目录(""/A/A/SUB)。
    let s1 = scan_root(&root_s, &empty, false, &noop).unwrap();
    assert_eq!(s1.files, 2, "首扫应有 2 个文件");
    let conn = open_db().unwrap();
    let root_id: i64 = conn
        .query_row("SELECT id FROM roots ORDER BY id DESC LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    let has = |c: &rusqlite::Connection, rel: &str| -> bool {
        c.query_row(
            "SELECT COUNT(*) FROM files WHERE root_id=?1 AND relpath=?2",
            rusqlite::params![root_id, rel],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
            > 0
    };
    let dir_mtime = |c: &rusqlite::Connection, rel: &str| -> i64 {
        c.query_row(
            "SELECT mtime FROM dirs WHERE root_id=?1 AND relpath=?2",
            rusqlite::params![root_id, rel],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(has(&conn, "A/SUB/s1.txt"), "首扫应收录 A/SUB/s1.txt");
    assert!(dir_mtime(&conn, "A") != 0, "A 目录首扫后应有非零缓存 mtime");

    // ② 模拟「A/SUB 那轮读失败 → 代际对账清掉它的 dirs 行」留下的状态(A 的 mtime 不变,因为 A
    //    直属项没增删)。同时在 A/SUB 里**新增**文件(A 的 mtime 仍不变,SUB 早已存在)——这正是
    //    增量盘点的盲区。
    conn.execute(
        "DELETE FROM dirs WHERE root_id=?1 AND relpath='A/SUB'",
        [root_id],
    )
    .unwrap();
    write(&root.join("A").join("SUB").join("s2-new.txt"), "s2-new");

    // ③ 复现漏扫:不调自愈,直接增量重扫 → A 走「免遍历」跳过、邻接表里没了 A/SUB → 新文件被漏。
    let _ = scan_root(&root_s, &empty, false, &noop).unwrap();
    assert!(
        !has(&conn, "A/SUB/s2-new.txt"),
        "复现:不自愈时 A/SUB 被永久漏扫,新文件 s2-new.txt 扫不到"
    );

    // ④ 自愈:把 A/SUB 的父目录(A)缓存 mtime 置 0(= invalidate_skipped_parents 在一轮收尾把它
    //    列入 skipped 后做的事),令下次增量必重读 A。
    let healed = invalidate_skipped_parents(
        &conn,
        root_id,
        root.as_path(),
        &[root.join("A").join("SUB").to_string_lossy().into_owned()],
    );
    assert_eq!(healed, 1, "应作废 1 个父目录(A)");
    assert_eq!(dir_mtime(&conn, "A"), 0, "A 的缓存 mtime 应被置 0");

    // ⑤ 再增量重扫 → A 被真正 read_dir → A/SUB 重新发现 → 漏掉的新文件补回。
    let _ = scan_root(&root_s, &empty, false, &noop).unwrap();
    assert!(
        has(&conn, "A/SUB/s2-new.txt"),
        "自愈后增量盘点应补扫回此前漏掉的 A/SUB/s2-new.txt"
    );
    assert!(has(&conn, "A/SUB/s1.txt"), "原有文件仍在");

    drop(conn);
    std::env::remove_var("POLARIS_FABLE_DB");
    let _ = std::fs::remove_dir_all(&base);
}
