use super::*;

// ───────────────────────── 语义聚类(复用已存向量) ─────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterBuildSummary {
    pub clusters: usize,
    pub files: usize,
    pub seconds: f64,
    pub note: String,
}

pub(crate) struct FileVec {
    file_id: i64,
    root_id: i64,
    relpath: String,
    name: String,
    vec: Vec<f32>,
}

pub(crate) fn normalize(v: &mut [f32]) {
    let n = (v.iter().map(|x| x * x).sum::<f32>()).sqrt();
    if n > 1e-6 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
}

pub(crate) fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// 词法归类的哈希特征维度(稀疏 token → 固定维,余弦可比)。
pub(crate) const LEX_DIM: usize = 128;

pub(crate) fn hash_token(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// 文件的词法特征向量:**按意思,不按格式**——文件名分词(权重 2.4)主导,文件夹段(0.7)次之。
/// **刻意不喂扩展名**:否则一堆 .html / .png / .log 会按「格式」抱团,聚出「网页」「图片」这种
/// 不是给人看的分类;用户要的是「报税」「装修」这类**主题**分类(格式维度另有「按语言/类型」筛选条)。
/// 相近命名的文件在余弦下自然靠拢。哈希进 LEX_DIM 维。
pub(crate) fn lexical_vec(relpath: &str, name: &str, _ext: &str) -> Vec<f32> {
    let mut v = vec![0f32; LEX_DIM];
    let segs: Vec<&str> = relpath.split('/').collect();
    for seg in segs.iter().take(segs.len().saturating_sub(1)) {
        let low = seg.trim().to_lowercase();
        if low.is_empty() || GENERIC_DIRS.contains(&low.as_str()) {
            continue;
        }
        v[(hash_token(&low) % LEX_DIM as u64) as usize] += 0.7;
    }
    for tok in tokenize(name) {
        v[(hash_token(&tok) % LEX_DIM as u64) as usize] += 2.4;
    }
    v
}

/// 词法兜底:加载范围内文件的**样本**(上限 6000)→ 归一化词法向量,用来算 k-means 质心。
///
/// 取样两路并集(各自去重、合计 ≤ `CAP`):
///  ① **最近改动的 `RECENT` 个**(mtime 倒序)—— 用户「当下在忙」的那摊活儿,务必让它**自成质心**,
///     否则在被某个大旧库(几十万文件)占满的库里,纯均匀取样会让最近的活儿一个质心都分不到、
///     被并进某个大旧簇里 → 星图上彻底看不见,用户感觉「不懂我」;
///  ② **id 哈希均匀散布全库**补齐到 `CAP` —— 保证所有老主题也都有质心、覆盖到全库。
/// 真正的「全覆盖指派」在 [`cluster_build_on`] 里对**全部文件**做(O(N·k)),取样只决定质心位置,
/// 故加 recency 偏置不影响覆盖率(仍 1.0),只让「最近主题」在星图里冒出来。
pub(crate) fn load_lexical_files(
    conn: &rusqlite::Connection,
    filter: &str,
) -> Result<Vec<FileVec>, String> {
    const CAP: usize = 6000;
    const RECENT: usize = 2000;
    let mut out: Vec<FileVec> = Vec::new();
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut take = |sql: String| -> Result<(), String> {
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for (id, root_id, relpath, name, ext) in rows.flatten() {
            if out.len() >= CAP {
                break;
            }
            if !seen.insert(id) {
                continue; // 已被「最近」路取过,不重复算
            }
            let mut v = lexical_vec(&relpath, &name, &ext);
            normalize(&mut v);
            out.push(FileVec {
                file_id: id,
                root_id,
                relpath,
                name,
                vec: v,
            });
        }
        Ok(())
    };
    // ① 最近改动(mtime>0 跳过读不出时间的);② 哈希均匀补齐全库。
    take(format!(
        "SELECT f.id, f.root_id, f.relpath, f.name, f.ext FROM files f
         WHERE 1=1{filter} AND f.mtime>0 ORDER BY f.mtime DESC LIMIT {RECENT}"
    ))?;
    take(format!(
        "SELECT f.id, f.root_id, f.relpath, f.name, f.ext FROM files f
         WHERE 1=1{filter} ORDER BY (f.id * 2654435761) % 1000003 LIMIT {CAP}"
    ))?;
    Ok(out)
}

/// 归类用的「向量来源」:
/// - `Auto`:有已存嵌入向量走语义、否则自动退词法(默认,向后兼容)。
/// - `Lexical`:强制走结构/词法(路径+文件名+扩展名哈希),**秒级、零嵌入依赖** —— 文件中心
///   v3 的 T0「骨架图谱」用它,盘点完立刻出簇,不等任何向量。
/// - `Semantic`:强制走语义(均值池化已存向量);没向量则优雅退词法,绝不报错卡住流程。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClusterMode {
    Auto,
    Lexical,
    Semantic,
}

/// 单根/全库重建语义聚类:每文件 = 其 chunk 向量均值池化 → 球面 k-means(余弦) →
/// 写回 files.cluster_id + clusters 表。纯数学,不调嵌入 API。
/// 无嵌入向量时自动退化为词法归类(见 [`load_lexical_files`]),保证永远可用。
pub fn cluster_build(root: Option<String>) -> Result<ClusterBuildSummary, String> {
    cluster_build_mode(root, ClusterMode::Auto)
}

/// clusters 表重建互斥闸:数学聚类(CLUSTERING)、LLM 聚类(LLM_CLUSTERING)、渐进归类
/// (SMART_CLUSTERING)是三把独立的**入口**闸,彼此可并发走到「删全表重建」——由于
/// clusters.id 无 AUTOINCREMENT,rowid 会被复用,先删后建的一方会让另一方的分批指派
/// 把文件静默挂到语义无关的新簇上。所有真正重建 clusters 表的路径都必须先过这把闸。
pub(crate) static CLUSTERS_TABLE_WRITE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 见 [`ClusterMode`]:按指定向量来源重建聚类。`cluster_build` = `Auto`。
pub fn cluster_build_mode(
    root: Option<String>,
    want: ClusterMode,
) -> Result<ClusterBuildSummary, String> {
    let Some(_rebuild) = crate::fable::FlagGuard::acquire(&CLUSTERS_TABLE_WRITE) else {
        return Err("另一个归类任务正在重建簇表,稍后再试".into());
    };
    let started = std::time::Instant::now();
    let conn = open_db()?;
    let ids = resolve_root_ids(&conn, &root);
    cluster_build_on(&conn, &ids, want, started)
}

/// 聚类核心(可注入连接,便于在隔离 db 上做准确度评测,见 `tests::cluster_eval_*`)。
/// `cluster_build_mode` 仅负责 open_db + 解析根,再委托本函数。
pub(crate) fn cluster_build_on(
    conn: &rusqlite::Connection,
    ids: &[i64],
    want: ClusterMode,
    started: std::time::Instant,
) -> Result<ClusterBuildSummary, String> {
    let filter = if ids.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        format!(" AND f.root_id IN ({})", list.join(","))
    };

    // 向量来源:Lexical 直接走结构特征(不读 chunks,秒级);其余先取已存嵌入向量。
    let mut mode = "semantic";
    let mut files: Vec<FileVec> = if want == ClusterMode::Lexical {
        mode = "lexical";
        load_lexical_files(&conn, &filter)?
    } else {
        // 均值池化:流式累加每个文件的 chunk 向量
        let mut acc: HashMap<i64, (Vec<f32>, u32, i64, String, String)> = HashMap::new();
        {
            let sql = format!(
                "SELECT c.file_id, c.vec, f.root_id, f.relpath, f.name
                 FROM chunks c JOIN files f ON f.id=c.file_id
                 WHERE f.kind='text'{filter}"
            );
            let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
            let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
            while let Some(row) = rows.next().map_err(|e| e.to_string())? {
                let file_id: i64 = row.get(0).map_err(|e| e.to_string())?;
                let blob: Vec<u8> = row.get(1).map_err(|e| e.to_string())?;
                let v = crate::fable::index::blob_to_vec(&blob);
                let root_id: i64 = row.get(2).map_err(|e| e.to_string())?;
                let relpath: String = row.get(3).map_err(|e| e.to_string())?;
                let name: String = row.get(4).map_err(|e| e.to_string())?;
                let entry = acc
                    .entry(file_id)
                    .or_insert_with(|| (vec![0.0; v.len()], 0, root_id, relpath, name));
                if entry.0.len() == v.len() {
                    for (a, b) in entry.0.iter_mut().zip(v.iter()) {
                        *a += b;
                    }
                    entry.1 += 1;
                }
            }
        }
        let v: Vec<FileVec> = acc
            .into_iter()
            .filter(|(_, (_, n, ..))| *n > 0)
            .map(|(file_id, (mut vec, n, root_id, relpath, name))| {
                for x in vec.iter_mut() {
                    *x /= n as f32;
                }
                normalize(&mut vec);
                FileVec {
                    file_id,
                    root_id,
                    relpath,
                    name,
                    vec,
                }
            })
            .collect();
        if v.len() < 2 {
            // 没有(足够的)嵌入向量 → 退化为「结构/词法」归类:对全部文件用
            // 文件夹 + 文件名分词 + 扩展名的哈希特征向量,跑同一套球面 k-means。
            // 无需任何 key、离线即可用 —— 保证「智能归类」永远点得动、永远能把相似文件放一起;
            // 配了硅基 key 并建好向量索引后,Auto/Semantic 自动走上面的语义路(更准)。
            mode = "lexical";
            load_lexical_files(&conn, &filter)?
        } else {
            v
        }
    };

    if files.len() < 2 {
        return Ok(ClusterBuildSummary {
            clusters: 0,
            files: files.len(),
            seconds: started.elapsed().as_secs_f64(),
            note: "可归类的文件不足(<2),先点「盘点」扫描磁盘文件再归类".into(),
        });
    }
    // 稳定顺序(file_id 升序),让确定性初始化可复现
    files.sort_by_key(|f| f.file_id);

    let n = files.len();
    let file_vecs: Vec<Vec<f32>> = files.iter().map(|f| f.vec.clone()).collect();
    // 一级(叶):细粒度语义簇 —— 比 √n 再细一点(×1.4),让主题分得更碎、星图更有层次。
    let k = (((n as f64).sqrt() * 1.4).round() as usize)
        .clamp(4, 32)
        .min(n);
    let (assign, leaf_centroids) = spherical_kmeans(&file_vecs, k);

    // 叶簇成员(剔空簇)
    let mut members_all: Vec<Vec<usize>> = vec![Vec::new(); leaf_centroids.len()];
    for (i, &c) in assign.iter().enumerate() {
        members_all[c].push(i);
    }
    let leaf_idx: Vec<usize> = (0..members_all.len())
        .filter(|&c| !members_all[c].is_empty())
        .collect();
    let members: Vec<Vec<usize>> = leaf_idx.iter().map(|&c| members_all[c].clone()).collect();
    let n_leaf = members.len();

    // 二级(父):叶簇质心再聚合成「顶层主题」。叶簇 ≥4 才分两级,否则全部顶层。
    let two_level = n_leaf >= 4;
    let parent_of_leaf: Vec<usize> = if two_level {
        let k_parent = ((n_leaf as f64).sqrt().ceil() as usize)
            .clamp(3, 9)
            .min(n_leaf);
        let cvecs: Vec<Vec<f32>> = leaf_idx
            .iter()
            .map(|&c| leaf_centroids[c].clone())
            .collect();
        spherical_kmeans(&cvecs, k_parent).0
    } else {
        (0..n_leaf).collect()
    };
    let n_parents = parent_of_leaf
        .iter()
        .copied()
        .max()
        .map(|m| m + 1)
        .unwrap_or(0);

    // ── 原子换代:删旧簇 + 清 cluster_id + 写新簇放进【同一个】事务 ──
    // 旧实现这三条 DELETE/UPDATE 在事务外各自自动提交,随后才 BEGIN 插新簇;中途任一
    // INSERT 失败或进程被杀 → 新簇被回滚而旧簇已永久删除,用户归类一夜清零。现在成败
    // 一体:要么旧归类原样保留,要么新归类完整落地,不再出现「旧的没了、新的没来」。
    let built_at = chrono::Local::now().timestamp_millis();
    let mut new_clusters = 0usize;
    // 每个叶簇的 (db id, 质心向量),供事务提交后把【全部文件】指派到最近质心(全覆盖)。
    let mut leaf_db: Vec<(i64, Vec<f32>)> = Vec::with_capacity(n_leaf);
    let mut parent_ids: Vec<i64> = vec![0; n_parents];
    conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
    let txn_res: Result<(), String> = (|| {
        // 清旧簇(对涉及的根)。簇 id 即将重排,旧关系边一并清掉,免得 cluster_edges 残留指向已删簇
        // (虽然 build_file_graph 会按现存簇过滤、不会渲染脏边,但清掉更干净、避免长期累积)。
        if ids.is_empty() {
            conn.execute("DELETE FROM clusters", []).ok();
            conn.execute("DELETE FROM cluster_edges", []).ok();
            conn.execute("UPDATE files SET cluster_id=0", []).ok();
        } else {
            let list: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
            let inlist = list.join(",");
            conn.execute(
                &format!("DELETE FROM clusters WHERE root_id IN ({inlist})"),
                [],
            )
            .ok();
            conn.execute(
                &format!("DELETE FROM cluster_edges WHERE root_id IN ({inlist})"),
                [],
            )
            .ok();
            conn.execute(
                &format!("UPDATE files SET cluster_id=0 WHERE root_id IN ({inlist})"),
                [],
            )
            .ok();
        }

        // 先写顶层主题(父簇:自身不直接挂文件,颜色按主题分配)。
        if two_level {
            for p in 0..n_parents {
                // 该主题下所有文件 = 旗下叶簇成员之并
                let mut union: Vec<usize> = Vec::new();
                for (li, &leaf_p) in parent_of_leaf.iter().enumerate() {
                    if leaf_p == p {
                        union.extend_from_slice(&members[li]);
                    }
                }
                if union.is_empty() {
                    continue;
                }
                let (label, keywords) = name_cluster(&files, &union);
                let color = CLUSTER_PALETTE[p % CLUSTER_PALETTE.len()];
                conn.execute(
                    "INSERT INTO clusters(root_id,label,color,keywords,size,built_at,parent) VALUES(?1,?2,?3,?4,?5,?6,0)",
                    rusqlite::params![rep_root(&files, &union), label, color, keywords, union.len() as i64, built_at],
                )
                .map_err(|e| e.to_string())?;
                parent_ids[p] = conn.last_insert_rowid();
            }
        }

        // 再写叶簇(两级时 parent=父 id 且与父同色;单级时 parent=0)并把【样本】文件挂到叶簇。
        for (li, mem) in members.iter().enumerate() {
            let (label, keywords) = name_cluster(&files, mem);
            let p = parent_of_leaf[li];
            let (parent, color) = if two_level {
                (parent_ids[p], CLUSTER_PALETTE[p % CLUSTER_PALETTE.len()])
            } else {
                (0i64, CLUSTER_PALETTE[new_clusters % CLUSTER_PALETTE.len()])
            };
            conn.execute(
                "INSERT INTO clusters(root_id,label,color,keywords,size,built_at,parent) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                rusqlite::params![rep_root(&files, mem), label, color, keywords, mem.len() as i64, built_at, parent],
            )
            .map_err(|e| e.to_string())?;
            let cluster_id = conn.last_insert_rowid();
            {
                let mut stmt = conn
                    .prepare_cached("UPDATE files SET cluster_id=?1 WHERE id=?2")
                    .map_err(|e| e.to_string())?;
                for &i in mem {
                    stmt.execute(rusqlite::params![cluster_id, files[i].file_id])
                        .map_err(|e| e.to_string())?;
                }
            }
            leaf_db.push((cluster_id, leaf_centroids[leaf_idx[li]].clone()));
            new_clusters += 1;
        }
        Ok(())
    })();
    if let Err(e) = txn_res {
        // 失败整体回滚:旧归类原样保留;显式 ROLLBACK 不留悬挂事务(连接可能是测试注入、还要复用)。
        let _ = conn.execute_batch("ROLLBACK");
        return Err(e);
    }
    if let Err(e) = conn.execute_batch("COMMIT") {
        let _ = conn.execute_batch("ROLLBACK");
        return Err(e.to_string());
    }

    // ── 全覆盖关键修(治「几分钟路径只归 6000、大库覆盖率暴跌」)──
    // 词法档:质心由样本(≤6000)算出,但要把**全部文件**指派到最近质心,而非只归样本。
    // 指派是 O(N·k) 纯点积,弱机也就几秒。语义档不做(无嵌入的文件没向量;全量嵌入交 T2)。
    //
    // 写入策略:全量指派可达几十万行,若裹在单个写事务里,持写锁会超过 open_db 的 20s
    // busy_timeout,并行盘点的 writer COMMIT 直接报错。改为【先读后算、分批写回】:
    // 只读扫描在事务外流式算好 (file_id → 簇 id),再每 ASSIGN_BATCH 行一个短事务提交,
    // 批间让出写锁。中断最坏是部分文件暂留 cluster_id=0(簇本体上面已提交,下轮归类可续),不丢簇。
    let mut total_files = n;
    if mode == "lexical" && !leaf_db.is_empty() {
        // 1) 只读扫描:逐行算最近质心,不开写事务、不持写锁。
        let sql = format!("SELECT f.id, f.relpath, f.name, f.ext FROM files f WHERE 1=1{filter}");
        let mut pairs: Vec<(i64, i64)> = Vec::new(); // (file_id, 最近叶簇 id),47 万行也只 ~8MB
        let mut counts: HashMap<i64, i64> = HashMap::new();
        {
            let mut sel = conn.prepare(&sql).map_err(|e| e.to_string())?;
            let mut rows = sel.query([]).map_err(|e| e.to_string())?;
            while let Some(row) = rows.next().map_err(|e| e.to_string())? {
                let id: i64 = row.get(0).map_err(|e| e.to_string())?;
                let relpath: String = row.get(1).map_err(|e| e.to_string())?;
                let name: String = row.get(2).map_err(|e| e.to_string())?;
                let ext: String = row.get(3).map_err(|e| e.to_string())?;
                let mut v = lexical_vec(&relpath, &name, &ext);
                normalize(&mut v);
                let mut best = leaf_db[0].0;
                let mut best_s = f32::MIN;
                for (cid, cen) in &leaf_db {
                    let s = dot(cen, &v);
                    if s > best_s {
                        best_s = s;
                        best = *cid;
                    }
                }
                pairs.push((id, best));
                *counts.entry(best).or_insert(0) += 1;
            }
        }
        total_files = pairs.len();
        // 2) 分批写回:每批一个短事务,提交即让锁;单批失败回滚该批并带错返回
        //    (已提交的批仍有效,未写到的文件保持 cluster_id=0,下轮归类可续)。
        const ASSIGN_BATCH: usize = 8000;
        for chunk in pairs.chunks(ASSIGN_BATCH) {
            conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
            let step: Result<(), String> = (|| {
                let mut up = conn
                    .prepare_cached("UPDATE files SET cluster_id=?1 WHERE id=?2")
                    .map_err(|e| e.to_string())?;
                for &(id, cid) in chunk {
                    up.execute(rusqlite::params![cid, id])
                        .map_err(|e| e.to_string())?;
                }
                Ok(())
            })();
            if let Err(e) = step {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e);
            }
            conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
        }
        // 3) 叶簇 size = 全量指派后的真实计数;父簇 size = 旗下叶簇之和。
        //    量级只有簇数(几十行),沿用自动提交的宽容写法即可。
        let mut psum: HashMap<i64, i64> = HashMap::new();
        for (li, (cid, _)) in leaf_db.iter().enumerate() {
            let c = counts.get(cid).copied().unwrap_or(0);
            conn.execute(
                "UPDATE clusters SET size=?1 WHERE id=?2",
                rusqlite::params![c, cid],
            )
            .ok();
            if two_level {
                *psum.entry(parent_ids[parent_of_leaf[li]]).or_insert(0) += c;
            }
        }
        for (pid, c) in &psum {
            conn.execute(
                "UPDATE clusters SET size=?1 WHERE id=?2",
                rusqlite::params![c, pid],
            )
            .ok();
        }
    }

    Ok(ClusterBuildSummary {
        clusters: new_clusters,
        files: total_files,
        seconds: started.elapsed().as_secs_f64(),
        note: if mode == "semantic" {
            format!("已按语义把 {total_files} 个已嵌入文本归成 {new_clusters} 簇")
        } else {
            format!(
                "已按文件夹/名称把 {total_files} 个文件归成 {new_clusters} 簇 · 配硅基 key 并建向量索引后可升级为语义归类"
            )
        },
    })
}

/// 确定性球面 k-means(余弦):farthest-first 初始化 + Lloyd 迭代。
/// 输入向量须已 L2 归一化。返回 (每点所属簇下标, 各簇质心)。两级归类两层都复用它。
pub(crate) fn spherical_kmeans(vecs: &[Vec<f32>], k: usize) -> (Vec<usize>, Vec<Vec<f32>>) {
    let n = vecs.len();
    if n == 0 || k == 0 {
        return (vec![0; n], Vec::new());
    }
    let k = k.min(n);
    let dim = vecs[0].len();
    // 确定性初始化:farthest-first traversal(余弦),避免依赖随机数
    let mut centroids: Vec<Vec<f32>> = Vec::with_capacity(k);
    centroids.push(vecs[0].clone());
    while centroids.len() < k {
        let mut best_i = 0usize;
        let mut best_d = f32::MIN;
        for (i, v) in vecs.iter().enumerate() {
            let max_sim = centroids.iter().map(|c| dot(c, v)).fold(f32::MIN, f32::max);
            let d = -max_sim;
            if d > best_d {
                best_d = d;
                best_i = i;
            }
        }
        centroids.push(vecs[best_i].clone());
    }
    // Lloyd 迭代(球面)
    let mut assign = vec![0usize; n];
    for _ in 0..16 {
        let mut changed = false;
        for (i, v) in vecs.iter().enumerate() {
            let mut best = 0usize;
            let mut best_sim = f32::MIN;
            for (ci, c) in centroids.iter().enumerate() {
                let s = dot(c, v);
                if s > best_sim {
                    best_sim = s;
                    best = ci;
                }
            }
            if assign[i] != best {
                assign[i] = best;
                changed = true;
            }
        }
        let mut sums = vec![vec![0.0f32; dim]; k];
        let mut counts = vec![0u32; k];
        for (i, v) in vecs.iter().enumerate() {
            let c = assign[i];
            for (a, b) in sums[c].iter_mut().zip(v.iter()) {
                *a += b;
            }
            counts[c] += 1;
        }
        for ci in 0..k {
            if counts[ci] > 0 {
                for x in sums[ci].iter_mut() {
                    *x /= counts[ci] as f32;
                }
                normalize(&mut sums[ci]);
                centroids[ci] = std::mem::take(&mut sums[ci]);
            }
        }
        if !changed {
            break;
        }
    }
    (assign, centroids)
}

/// 簇代表根 = 成员里出现最多的 root_id。
pub(crate) fn rep_root(files: &[FileVec], members: &[usize]) -> i64 {
    let mut freq: HashMap<i64, usize> = HashMap::new();
    for &i in members {
        *freq.entry(files[i].root_id).or_insert(0) += 1;
    }
    freq.into_iter()
        .max_by_key(|(_, c)| *c)
        .map(|(r, _)| r)
        .unwrap_or(0)
}

/// 簇命名:优先成员里出现最多的「非通用」目录段;退化用文件名高频词。
pub(crate) fn name_cluster(files: &[FileVec], members: &[usize]) -> (String, String) {
    let mut dir_freq: HashMap<String, usize> = HashMap::new();
    let mut tok_freq: HashMap<String, usize> = HashMap::new();
    for &i in members {
        let rel = &files[i].relpath;
        let segs: Vec<&str> = rel.split('/').collect();
        // 目录段(去掉文件名)
        for seg in segs.iter().take(segs.len().saturating_sub(1)) {
            let s = seg.trim();
            if s.is_empty() {
                continue;
            }
            let low = s.to_lowercase();
            if GENERIC_DIRS.contains(&low.as_str()) {
                continue;
            }
            *dir_freq.entry(s.to_string()).or_insert(0) += 1;
        }
        // 文件名分词
        for tok in tokenize(&files[i].name) {
            *tok_freq.entry(tok).or_insert(0) += 1;
        }
    }
    let threshold = (members.len() as f64 * 0.34).ceil() as usize;
    let top_dir = dir_freq
        .iter()
        .filter(|(_, c)| **c >= threshold.max(2))
        .max_by_key(|(_, c)| **c)
        .map(|(d, _)| d.clone());

    let mut toks: Vec<(String, usize)> = tok_freq.into_iter().collect();
    toks.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let keywords: Vec<String> = toks.iter().take(4).map(|(t, _)| t.clone()).collect();

    let label = match top_dir {
        Some(d) => d,
        None => {
            if keywords.is_empty() {
                "未命名".to_string()
            } else {
                keywords
                    .iter()
                    .take(2)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" · ")
            }
        }
    };
    (label, keywords.join(" "))
}

/// 文件名 → 词:按非字母数字切英文 token(≥2),CJK 连续段整体当一个词。
pub(crate) fn tokenize(name: &str) -> Vec<String> {
    let stem: &str = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_cjk = String::new();
    let flush_ascii = |cur: &mut String, out: &mut Vec<String>| {
        if cur.chars().count() >= 2 {
            out.push(cur.to_lowercase());
        }
        cur.clear();
    };
    for ch in stem.chars() {
        if ch.is_ascii_alphanumeric() {
            cur.push(ch);
            if !cur_cjk.is_empty() {
                out.push(std::mem::take(&mut cur_cjk));
            }
        } else if ('\u{4e00}'..='\u{9fff}').contains(&ch) {
            cur_cjk.push(ch);
            flush_ascii(&mut cur, &mut out);
        } else {
            flush_ascii(&mut cur, &mut out);
            if cur_cjk.chars().count() >= 2 {
                out.push(std::mem::take(&mut cur_cjk));
            } else {
                cur_cjk.clear();
            }
        }
    }
    flush_ascii(&mut cur, &mut out);
    if cur_cjk.chars().count() >= 2 {
        out.push(cur_cjk);
    }
    // 过滤纯数字 + 常见噪声词 + **格式/技术词**(html/css/log/exe…)——这些当关键词或兜底簇名
    // 都是「机器味」,不是给人看的;主题词(报税/装修)才留。
    const TECH_TOK: &[&str] = &[
        "copy", "final", "html", "htm", "css", "js", "ts", "jsx", "tsx", "json", "xml", "yaml",
        "yml", "log", "logs", "tmp", "temp", "exe", "dll", "bin", "obj", "bak", "cache", "min",
        "index", "deck", "output", "raw", "dist", "build", "node", "vendor", "static",
    ];
    out.retain(|t| !t.chars().all(|c| c.is_ascii_digit()) && !TECH_TOK.contains(&t.as_str()));
    out
}

/// 把杂乱/带噪文件名清洗成可读标题(纯字符串,无 IO,grid 里现算):
/// 去扩展名 → 分隔符(_ - + ~ . 空格 括号 ·)切词 → 丢纯数字/时间戳/长哈希/常见噪声词
/// (img/screenshot/副本/微信图片…)→ 余下用空格连。清完为空(纯哈希图片名等)退回去扩展名的原名。
/// 这是「本地档」标题;AI 档会把更难的(纯乱码/纯哈希)写进 titles 表覆盖它。
pub(crate) fn clean_title(name: &str) -> String {
    const NOISE: &[&str] = &[
        "copy",
        "final",
        "副本",
        "未命名",
        "untitled",
        "new",
        "draft",
        "tmp",
        "temp",
        "out",
        "img",
        "image",
        "photo",
        "pic",
        "dsc",
        "vid",
        "video",
        "screenshot",
        "截图",
        "屏幕截图",
        "微信图片",
        "mmexport",
        "download",
        "下载",
        "wechat",
        "qq图片",
    ];
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in stem.chars() {
        if matches!(
            ch,
            '_' | '-' | '+' | '~' | '.' | ' ' | '(' | ')' | '[' | ']' | '{' | '}' | '·' | '@' | '#'
        ) {
            let t = cur.trim();
            if !t.is_empty() {
                parts.push(t.to_string());
            }
            cur.clear();
        } else {
            cur.push(ch);
        }
    }
    let t = cur.trim();
    if !t.is_empty() {
        parts.push(t.to_string());
    }

    let is_noise = |t: &str| -> bool {
        let low = t.to_lowercase();
        if t.chars().all(|c| c.is_ascii_digit()) {
            return true; // 纯数字:计数器/年份/时间戳片段
        }
        if t.len() >= 8 && t.chars().all(|c| c.is_ascii_hexdigit()) {
            return true; // 长 hex:哈希样
        }
        NOISE.contains(&low.as_str())
    };

    let kept: Vec<String> = parts.into_iter().filter(|t| !is_noise(t)).collect();
    let title = kept.join(" ");
    let title = title.trim();
    if title.chars().count() >= 2 {
        title.to_string()
    } else {
        stem.trim().to_string() // 全是噪声/太短 → 退回原名(去扩展名),总比空好
    }
}

// ───────────────────────── 缩略图批量预热(可选,后台友好) ─────────────────────────

/// 给一批绝对路径预生成缩略图缓存(前端进入网格时可后台调,加速滚动)。
/// 返回成功生成/命中缓存的数量。多核并行。
pub fn warm_thumbs(paths: Vec<String>, max: u32) -> usize {
    let done = AtomicUsize::new(0);
    let stack = Mutex::new(paths);
    std::thread::scope(|s| {
        for _ in 0..worker_count() {
            let (stack, done) = (&stack, &done);
            s.spawn(move || loop {
                let item = { stack.lock().unwrap().pop() };
                let Some(p) = item else { break };
                if let Ok(Some(_)) = thumb(p, max) {
                    done.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });
    done.load(Ordering::Relaxed)
}
