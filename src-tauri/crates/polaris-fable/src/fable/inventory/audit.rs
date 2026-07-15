use super::*;

/// 「按语言归类」回填:给所有还没定语言(lang='')的文件补上语言标签。
/// 代码/媒体零 IO 当场定;文稿读文件头嗅探自然语言(中文/英文/其他)。多核并行、幂等续跑。
/// 旧库(刚加 lang 列,全为 '')或新盘点后的文稿都靠它补齐。返回本轮回填条数。
#[derive(Debug, Clone, Serialize)]
pub struct AuditReport {
    pub mode: String,
    pub roots: u64,
    pub files_total: u64,
    /// dup_of / superseded_by 指向已不存在的文件 id(悬挂引用;健康库应为 0)。
    pub dangling_refs: u64,
    /// roots.files 记录数与实际 files 行数不一致的根个数(计数漂移)。
    pub roots_count_drift: u64,
    /// sample 模式:实际抽查的目录数。
    pub dirs_sampled: u64,
    /// 盘上有、库里无(漏收)。
    pub missing_in_db: u64,
    /// 库里有、盘上无(幻影,可能是删除未对账或抽样期文件刚消失)。
    pub missing_on_disk: u64,
    /// 盘上 mtime 与库里不一致(原地改写盲区);fix 模式当场重置 chunked/ftsed 待重建。
    pub mtime_drift: u64,
    /// 抽到但读不到的目录数(NAS 掉线 / 权限),不计入漏收。
    pub unreachable_dirs: u64,
    /// fix 模式作废缓存的目录数(下轮增量必重扫)。
    pub fixed_dirs: u64,
    pub seconds: f64,
}

/// 盘点完整性对账(填补「扫到的 vs 库里的」一直缺的显式对账)。三档:
/// - `counters`(默认,秒级纯 SQL):totals + 悬挂 dup/supersede 引用 + roots 计数漂移。
/// - `sample`(分钟级,随机抽 K 个目录**绕过 mtime 缓存**真 read_dir + stat):量化漏收/幻影/
///   **mtime 漂移**(直接测「原地追加写不碰目录」盲区的实际发生率)。
/// - `fix`:在 sample 基础上,漂移文件当场重置 chunked/ftsed 待重建,漏收目录作废其 dirs 缓存
///   (复用 [`invalidate_skipped_parents`] 的同款自愈:mtime=0 → 下轮增量必 read_dir)。
/// 掉线 NAS 的目录读不到时计入 `unreachable_dirs` 而非误报漏收(不重蹈 seen 抹库教训)。
#[cfg_attr(feature = "desktop", tauri::command(async))]
pub fn fable_audit(mode: Option<String>, sample: Option<usize>) -> Result<AuditReport, String> {
    let started = Instant::now();
    let mode = mode.unwrap_or_else(|| "counters".into());
    let do_sample = mode == "sample" || mode == "fix";
    let do_fix = mode == "fix";
    let conn = open_db()?;

    let one =
        |sql: &str| -> u64 { conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0) as u64 };
    let roots = one("SELECT COUNT(*) FROM roots");
    let files_total = one("SELECT COUNT(*) FROM files");
    let dangling_refs = one("SELECT COUNT(*) FROM files
         WHERE (dup_of<>0 AND dup_of NOT IN (SELECT id FROM files))
            OR (superseded_by<>0 AND superseded_by NOT IN (SELECT id FROM files))");
    let roots_count_drift = one("SELECT COUNT(*) FROM roots r
         WHERE r.files <> (SELECT COUNT(*) FROM files f WHERE f.root_id=r.id)");

    let mut rep = AuditReport {
        mode: mode.clone(),
        roots,
        files_total,
        dangling_refs,
        roots_count_drift,
        dirs_sampled: 0,
        missing_in_db: 0,
        missing_on_disk: 0,
        mtime_drift: 0,
        unreachable_dirs: 0,
        fixed_dirs: 0,
        seconds: 0.0,
    };

    if do_sample {
        let k = sample.unwrap_or(200).clamp(1, 5000);
        let dirs: Vec<(i64, String, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT d.root_id, d.relpath, r.path FROM dirs d
                     JOIN roots r ON r.id=d.root_id ORDER BY RANDOM() LIMIT ?1",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([k as i64], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            rows.flatten().collect()
        };
        for (root_id, drel, root_path) in dirs {
            if cancelled() {
                break;
            }
            rep.dirs_sampled += 1;
            let abs_dir =
                super::reencode_fs_path(&PathBuf::from(&root_path).join(&drel).to_string_lossy());
            let rd = match std::fs::read_dir(&abs_dir) {
                Ok(rd) => rd,
                Err(_) => {
                    rep.unreachable_dirs += 1;
                    continue;
                }
            };
            // 盘上直属文件:显示名 → mtime 秒(与盘点写库同口径)。
            let mut disk: HashSet<String> = HashSet::new();
            let mut dir_missing = 0u64;
            for ent in rd.flatten() {
                let ft = match ent.file_type() {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                if ft.is_dir() {
                    continue;
                }
                let name = super::decode_fs(ent.file_name().as_os_str());
                let rel = if drel.is_empty() {
                    name.clone()
                } else {
                    format!("{drel}/{name}")
                };
                disk.insert(name);
                let disk_mtime = ent
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let db_mtime: Option<i64> = conn
                    .query_row(
                        "SELECT mtime FROM files WHERE root_id=?1 AND relpath=?2",
                        rusqlite::params![root_id, rel],
                        |r| r.get(0),
                    )
                    .ok();
                match db_mtime {
                    None => {
                        rep.missing_in_db += 1;
                        dir_missing += 1;
                    }
                    Some(m) if m != disk_mtime => {
                        rep.mtime_drift += 1;
                        if do_fix {
                            let _ = conn.execute(
                                "UPDATE files SET chunked=0, ftsed=0 WHERE root_id=?1 AND relpath=?2",
                                rusqlite::params![root_id, rel],
                            );
                        }
                    }
                    _ => {}
                }
            }
            // 幻影:库里记的直属文件盘上没有。只取「恰好多一段」的直属子(不含更深层)。
            let db_children: Vec<String> = if drel.is_empty() {
                match conn.prepare(
                    "SELECT relpath FROM files WHERE root_id=?1 AND relpath NOT LIKE '%/%'",
                ) {
                    Ok(mut s) => s
                        .query_map([root_id], |r| r.get::<_, String>(0))
                        .map(|rs| rs.flatten().collect())
                        .unwrap_or_default(),
                    Err(_) => Vec::new(),
                }
            } else {
                match conn.prepare(
                    "SELECT relpath FROM files WHERE root_id=?1
                     AND relpath LIKE ?2||'/%' AND relpath NOT LIKE ?2||'/%/%'",
                ) {
                    Ok(mut s) => s
                        .query_map(rusqlite::params![root_id, drel], |r| r.get::<_, String>(0))
                        .map(|rs| rs.flatten().collect())
                        .unwrap_or_default(),
                    Err(_) => Vec::new(),
                }
            };
            for rel in db_children {
                let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
                if !disk.contains(&name) {
                    rep.missing_on_disk += 1;
                }
            }
            // fix:本目录有漏收 → 作废其缓存,下轮增量必重扫补齐。
            if do_fix && dir_missing > 0 {
                let n = conn
                    .execute(
                        "UPDATE dirs SET mtime=0 WHERE root_id=?1 AND relpath=?2",
                        rusqlite::params![root_id, drel],
                    )
                    .unwrap_or(0);
                rep.fixed_dirs += n as u64;
            }
        }
    }

    rep.seconds = started.elapsed().as_secs_f64();
    Ok(rep)
}

/// **`(async)`**:单次调用最多读 1.6 万个文件头(NAS 上更是逐个网络往返);同步命令会把
/// 这段 IO 钉在主线程、回填期间 UI 冻住 → 同 [`fable_audit`] 派到工作线程。
#[cfg_attr(feature = "desktop", tauri::command(async))]
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
                    let Some((id, ext, kind, root, rel)) = item else {
                        break;
                    };
                    let mut lang = quick_lang(&ext, &kind);
                    if lang.is_empty() {
                        // 文稿:读头嗅探自然语言;不可读/二进制 → 其他。
                        // DB 里存的是 decode_fs 后的显示路径;Unix 上 GBK 名文件的磁盘字节与其
                        // 不同,直接 open 恒失败 → 先经 reencode_fs_path 还原真实路径再读
                        // (同 scan.rs 消失判定的口径),否则这些文件被误标「未识别」且永不重试。
                        let abs = super::reencode_fs_path(
                            &Path::new(&root).join(&rel).to_string_lossy(),
                        );
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
                let v = if lang.is_empty() {
                    "未识别"
                } else {
                    lang.as_str()
                };
                stmt.execute(rusqlite::params![v, id])
                    .map_err(|e| e.to_string())?;
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
