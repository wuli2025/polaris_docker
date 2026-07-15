use super::*;

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
pub(crate) fn probe_secs() -> u64 {
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
    Dir {
        rel: String,
        mtime: i64,
        fcount: i64,
        fbytes: i64,
    },
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
pub(crate) fn rel_of(p: &Path, root: &Path) -> String {
    p.strip_prefix(root)
        .ok()
        .map(|r| super::decode_fs(r.as_os_str()).replace('\\', "/"))
        .unwrap_or_default()
}

/// 相对路径的父目录(顶层 / 根 → "")。从 dirs 缓存的全部 key 反推「父→子目录」邻接表用。
pub(crate) fn parent_rel(rel: &str) -> &str {
    match rel.rfind('/') {
        Some(i) => &rel[..i],
        None => "",
    }
}

/// 自愈核心:把「本轮被跳过 / 反复读失败 / 看门狗放弃的目录」的**父目录**在 `dirs` 缓存里 mtime
/// 置 0,作废父目录的「免遍历」资格。
///
/// 为什么必须这么做:被跳过的目录本轮没产生 `Msg` → 它的 `dirs` 行随后被代际对账(`seen<>gen`)
/// 清掉;而它父目录的 mtime 多半没变,下次**增量**盘点会对父目录走「免遍历」跳过、且邻接表(由
/// `dirs` 现存行反推)里已不含这个子目录 → 该子树**永远不会再被 `read_dir` 发现**,哪怕 NAS /
/// 外置盘 / 权限早已恢复,也只有用户手动点「完整盘点」才找得回(线上「经常有些文件扫不到」的根因)。
/// 把父目录 mtime 置 0 后,下次增量盘点比对 `cur_mtime != 0 && cached == cur` 必然失败 → 真正
/// `read_dir` 父目录 → 重新发现并补扫漏掉的子树,直到挂载恢复后自然收敛。父目录此刻 `seen=gen`
/// (被跳过时 `touch_dir` 过 / 被扫到时 `up_dir` 过),不会被代际对账的 DELETE 误伤。
///
/// 返回实际作废的父目录行数(供测试 / 诊断)。`skipped_paths` 为空时零成本返回。
pub(crate) fn invalidate_skipped_parents(
    conn: &rusqlite::Connection,
    root_id: i64,
    root_path: &Path,
    skipped_paths: &[String],
) -> usize {
    if skipped_paths.is_empty() {
        return 0;
    }
    let Ok(mut up_parent) = conn.prepare("UPDATE dirs SET mtime=0 WHERE root_id=?1 AND relpath=?2")
    else {
        return 0;
    };
    let mut seen_parents: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut n = 0usize;
    for p in skipped_paths {
        let rel = rel_of(Path::new(p), root_path);
        let parent = parent_rel(&rel).to_string();
        if seen_parents.insert(parent.clone()) {
            n += up_parent
                .execute(rusqlite::params![root_id, parent])
                .unwrap_or(0);
        }
    }
    n
}

/// 跨文件重命名/移动识别是否开启(`POLARIS_RENAME_MATCH=0` 一键关闭 → 退回「删旧+增新」)。
fn rename_match_enabled() -> bool {
    std::env::var("POLARIS_RENAME_MATCH")
        .map(|v| v.trim() != "0")
        .unwrap_or(true)
}

/// 重命名/移动免重嵌:在「本轮消失(gone)且带索引」的文件里,找本轮新增(seen=gen、chunked=0)
/// 中「**同名同大小**且内容指纹一致」的**唯一**新文件 → 把旧行的路径/mtime 改指过去、保 id 保
/// 已建 chunks/lex,并删掉新占位行。整目录移动因此零重嵌(也顺带省一大笔嵌入网络开销)。
///
/// 判据严到「同名 + 同 size + 内容哈希相等」:同名同大小候选通常就是被移动的那一个,读一次核验哈希
/// (盘点时新文件尚未索引、无哈希,只能现读现算)即可确认;命中不唯一 / 无候选 → 维持原「删旧+增新」,
/// 宁可多嵌一次也不错接。仅在同 root 内匹配(跨盘移动第一期不处理)。返回被判为「移动」的旧 id 集合。
fn reconcile_renames(
    conn: &rusqlite::Connection,
    root_id: i64,
    root_base: &Path,
    gen: i64,
    gone: &[i64],
) -> HashSet<i64> {
    let mut moved: HashSet<i64> = HashSet::new();
    if !rename_match_enabled() || gone.is_empty() {
        return moved;
    }
    // 只有「带可复用索引状态」的消失文件才值得救(chunked=1 且有内容指纹)。name 一并取出
    // 作同名预筛,免逐行再查一次。IN 列表按 512 分批,避开 SQLite 变量上限(同旁边删除逻辑)。
    let mut reusable: Vec<(i64, i64, String, String)> = Vec::new();
    for chunk in gone.chunks(512) {
        let ph = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT id, size, content_hash, name FROM files
             WHERE id IN ({ph}) AND chunked=1 AND content_hash<>''"
        );
        let Ok(mut stmt) = conn.prepare(&sql) else {
            return moved;
        };
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        });
        match rows {
            Ok(rs) => reusable.extend(rs.filter_map(|r| r.ok())),
            Err(_) => return moved,
        }
    }
    if reusable.is_empty() {
        return moved;
    }
    let mut used_new: HashSet<i64> = HashSet::new();
    // 指纹核验要整文件读入内存,2GB 大文本一次就分配 2GB → 超上限直接放弃 rename 判定,
    // 维持「删旧+增新」(宁可重嵌一次也不炸内存)。
    const FP_MAX_BYTES: i64 = 64 * 1024 * 1024;
    // 同名同大小的候选可能被多个旧文件反复核验 → 按候选 id 缓存已算指纹,一个候选只读一次盘。
    let mut fp_cache: std::collections::HashMap<i64, Option<String>> =
        std::collections::HashMap::new();
    for (old_id, size, ohash, oname) in reusable {
        if cancelled() {
            break;
        }
        if size > FP_MAX_BYTES {
            continue;
        }
        // 同名 + 同大小 + 本轮新增未索引的候选(通常 0 或 1 条)
        let cands: Vec<(i64, String, String, i64)> = {
            let Ok(mut stmt) = conn.prepare(
                "SELECT id, relpath, name, mtime FROM files
                 WHERE root_id=?1 AND seen=?2 AND chunked=0 AND size=?3 AND name=?4 LIMIT 16",
            ) else {
                continue;
            };
            let rows = stmt.query_map(rusqlite::params![root_id, gen, size, oname], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            });
            match rows {
                Ok(rs) => rs.filter_map(|r| r.ok()).collect(),
                Err(_) => continue,
            }
        };
        // 读候选核验内容指纹,要求「恰好一个」匹配才判移动
        let mut hit: Option<(i64, String, String, i64)> = None;
        let mut hit_count = 0u32;
        for c in cands {
            if used_new.contains(&c.0) {
                continue;
            }
            let chash = fp_cache
                .entry(c.0)
                .or_insert_with(|| {
                    let abs = super::reencode_fs_path(&root_base.join(&c.1).to_string_lossy());
                    let bytes = std::fs::read(&abs).ok()?;
                    if bytes.iter().take(4096).any(|&b| b == 0) {
                        return None; // 伪文本,内容哈希只对文本有意义
                    }
                    Some(super::index::content_fingerprint(&String::from_utf8_lossy(
                        &bytes,
                    )))
                })
                .clone();
            let Some(chash) = chash else {
                continue; // 读不到 / 伪文本(缓存住,不再重读)
            };
            if chash == ohash {
                hit_count += 1;
                if hit_count > 1 {
                    break; // 不唯一 → 放弃,维持删旧+增新
                }
                hit = Some(c);
            }
        }
        if hit_count == 1 {
            if let Some((new_id, new_rel, new_name, new_mtime)) = hit {
                // 先删新占位行释放 UNIQUE(root_id, relpath),再把旧行改指过去(保 id/保 chunks)。
                if conn
                    .execute("DELETE FROM files WHERE id=?1", [new_id])
                    .is_ok()
                    && conn
                        .execute(
                            "UPDATE files SET relpath=?2, name=?3, mtime=?4, seen=?5 WHERE id=?1",
                            rusqlite::params![old_id, new_rel, new_name, new_mtime, gen],
                        )
                        .is_ok()
                {
                    used_new.insert(new_id);
                    moved.insert(old_id);
                }
            }
        }
    }
    moved
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
        .query_row("SELECT id FROM roots WHERE path=?1", [&root_canon], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;

    // ── 增量盘点:载入上一轮的目录缓存(rel → mtime + 直属文件数/字节)──────────────
    // 这一笔查询(~目录数行,几万级,本地 SQLite 毫秒级)换来重扫时「没变的子树整棵免遍历」。
    // `full=true` 或第一次盘点(缓存空)→ 不命中,等同全量。`dir_children` 由全部 key 反推父子
    // 邻接表:跳过某目录时据它把子目录排进队列(自己不 read_dir 也能继续往下钻、逐个比对)。
    let mut dir_mt: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut dir_stat: std::collections::HashMap<String, (u64, u64)> =
        std::collections::HashMap::new();
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
        dir_children
            .entry(parent_rel(rel).to_string())
            .or_default()
            .push(rel.clone());
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
            .map(|_| {
                Mutex::new(Beat {
                    dir: None,
                    abandoned: false,
                })
            })
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
                                        root_id,
                                        row.relpath,
                                        row.name,
                                        row.ext,
                                        row.kind,
                                        row.lang,
                                        row.size as i64,
                                        row.mtime,
                                        gen
                                    ])
                                    .map_err(|e| e.to_string())?;
                            }
                            Msg::Dir {
                                rel,
                                mtime,
                                fcount,
                                fbytes,
                            } => {
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
                let cur_mtime = if known_mtime != 0 {
                    known_mtime
                } else {
                    dir_mtime(&dir)
                };
                let unchanged =
                    cur_mtime != 0 && dir_mt.get(&rel).map(|m| *m == cur_mtime).unwrap_or(false);
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
                            // c 是缓存里 '/' 分隔的 relpath,Windows 上 join 出「\ 与 / 混排」的字符串,
                            // 与 exclude 集合里的原生 '\' 路径永远失配 → 比较前统一成平台分隔符,
                            // 否则增量轮里被排除的子树照样入库。
                            if !exclude.is_empty() {
                                let key = if std::path::MAIN_SEPARATOR == '\\' {
                                    child.to_string_lossy().replace('/', "\\")
                                } else {
                                    child.to_string_lossy().into_owned()
                                };
                                if exclude.contains(&key) {
                                    continue;
                                }
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
                                    if skip_dir_always(&name) || (heavy_prune && skip_dir_os(&name))
                                    {
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
                            let _ = tx.send(Msg::Dir {
                                rel,
                                mtime: cur_mtime,
                                fcount,
                                fbytes,
                            });
                        }
                        Err(_) => {
                            // 读目录失败(权限抖动 / NAS 瞬断):不丢弃也不原地卡住别人,而是降级到
                            // 队尾「最后再试」;超过 [`MAX_DIR_ATTEMPTS`] 仍失败才列为「已跳过」。
                            if job.attempts < MAX_DIR_ATTEMPTS {
                                queue.demote((dir.clone(), known_mtime), job.attempts + 1);
                            } else {
                                problematic
                                    .lock()
                                    .unwrap()
                                    .push(dir.to_string_lossy().into_owned());
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
                        let hit =
                            if !b.abandoned && b.dir.is_some() && last[i].1.elapsed() > deadline {
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
    writer
        .join()
        .map_err(|_| "writer 线程 panic".to_string())??;

    if cancelled() {
        return Err("已取消".into());
    }

    let files = n_files.load(Ordering::Relaxed);
    let bytes = n_bytes.load(Ordering::Relaxed);
    progress(files, bytes); // 收尾再报一次,确保进度条落到最终值
                            // 本轮被跳过/反复读失败/看门狗放弃的目录绝对路径(用于①报数 ②下面给它们的父目录作废增量缓存,
                            // 根治「一次读失败就永久漏扫」——见下方自愈逻辑)。
    let skipped_paths: Vec<String> = {
        let mut v = problematic.lock().unwrap();
        for (d, _mt) in skipped_dirs {
            v.push(d.to_string_lossy().into_owned());
        }
        v.clone()
    };
    let skipped = skipped_paths.len() as u64;

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
        let mut gone: Vec<i64> = stale
            .into_iter()
            .filter(|(_, rel)| {
                // relpath 用 '/',Path::join 在 Windows 上也认 '/',无需替换分隔符。
                // rel 是 decode_fs() 后的显示路径, Unix 上 GBK 名文件的磁盘字节与其
                // 不同 → 先经 reencode_fs_path 还原真实路径, 否则 stat 恒失败被误删。
                let abs = super::reencode_fs_path(&root_base.join(rel).to_string_lossy());
                // Path::exists() 把 EACCES/SMB 认证过期等一切 IO 错误折叠成「不存在」,
                // 会把只是暂时读不到的文件连 chunks(向量)+lex(倒排)一起误删。
                // 只认 NotFound 才判「真消失」; 其余错误(权限/网络抖动)一律保留待下轮。
                let parent_ok = abs
                    .parent()
                    .map(|p| std::fs::symlink_metadata(p).is_ok())
                    .unwrap_or(false);
                parent_ok
                    && matches!(
                        std::fs::symlink_metadata(&abs),
                        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound
                    )
            })
            .map(|(id, _)| id)
            .collect();

        // 重命名/移动免重嵌:把「其实只是被移动了」的文件从 gone 里摘出来(改指新路径、保留向量),
        // 剩下的才是真正消失、该删的。整目录移动因此零重嵌。
        let moved = reconcile_renames(&conn, root_id, &root_base, gen, &gone);
        if !moved.is_empty() {
            gone.retain(|id| !moved.contains(id));
        }

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
                // 悬挂去重指针自愈:canonical 被删 → 其副本失去归并目标,清 dup_of 且重置 chunked=0
                // 让它们重新入索引(下次 dedupe_scan 会在其中重选 canonical);被压制的新版被删 →
                // 清 superseded_by,让旧版不再被降权。避免删原件后副本/旧版永久隐身。
                conn.execute(
                    &format!("UPDATE files SET dup_of=0, chunked=0 WHERE dup_of IN ({ph})"),
                    rusqlite::params_from_iter(batch.iter()),
                )
                .map_err(|e| e.to_string())?;
                conn.execute(
                    &format!("UPDATE files SET superseded_by=0 WHERE superseded_by IN ({ph})"),
                    rusqlite::params_from_iter(batch.iter()),
                )
                .map_err(|e| e.to_string())?;
            }
        }
        // 目录缓存对账:本轮没确认(seen<>gen)的 dirs 行清掉 —— 目录已消失,或本轮没扫到
        // (挂载掉线 / 反复读失败被跳过)。后者只是丢掉它的「免遍历」资格,下次重扫当成新目录
        // 完整 read_dir 再补回缓存,绝不波及 files(文件的删除另由上面「逐个 stat 确认真消失」把关)。
        conn.execute(
            "DELETE FROM dirs WHERE root_id=?1 AND seen<>?2",
            rusqlite::params![root_id, gen],
        )
        .map_err(|e| e.to_string())?;

        // 自愈:防「一次读失败就永久漏扫」。被跳过/读失败的目录本轮没产生 Msg → 它的 dirs 行刚被
        // 上面按 seen<>gen 清掉;而它的**父目录** mtime 多半没变,下次增量盘点会对父目录走「免遍历」
        // 跳过、且邻接表(由 dirs 现存行反推)里已不含这个子目录 → 该子树**再也不会被 read_dir 发现**,
        // 哪怕 NAS/挂载早已恢复,也只有手动「完整盘点」才找得回。修法见 [`invalidate_skipped_parents`]。
        invalidate_skipped_parents(&conn, root_id, root_path.as_path(), &skipped_paths);

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
pub(crate) fn strip_extended_prefix(s: &str) -> String {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    s.strip_prefix(r"\\?\")
        .map(|x| x.to_string())
        .unwrap_or_else(|| s.to_string())
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
pub(crate) fn on_disk_size(path: &Path, meta: &std::fs::Metadata, remote: bool) -> u64 {
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
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
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
