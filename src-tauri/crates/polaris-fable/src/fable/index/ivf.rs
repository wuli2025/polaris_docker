use super::*;

// ───────────────────────── 向量 IVF 优化(20TB 级 ANN)─────────────────────────
//
// 把全部向量按「二值质心」聚成若干倒排单元(cell);查询时只在最近的 nprobe 个 cell
// 里粗筛(+cell=-1 的未分配新数据),把向量车道从「每查询全表 O(N) 扫」降到
// ~O(N·nprobe/cells)。质心是二值码,训练(k-means)与分配都只用汉明 popcount,
// 弱 NAS CPU 也跑得动;未建 cell 时检索自动退回全扫(零回归)。

/// IVF 启用门槛:同模型 chunk 数低于此值时,全表暴力扫已是亚秒级,不建 cell(省训练成本)。
const IVF_MIN_CHUNKS: i64 = 50_000;
/// 训练采样上限(在采样上跑 k-means,弱 CPU 也快;分配仍覆盖全量)。
const IVF_SAMPLE: usize = 100_000;
/// 二值 k-means 迭代轮数(二值质心收敛快)。
const IVF_ITERS: usize = 8;

/// 一组二值码按位多数表决求质心:某位上「置 1 的成员数 > 半数」则该位为 1。
fn majority_bits(members: &[&[u8]], nbytes: usize) -> Vec<u8> {
    let mut counts = vec![0i32; nbytes * 8];
    for m in members {
        for (bytei, &b) in m.iter().enumerate().take(nbytes) {
            for bit in 0..8 {
                if b & (1 << bit) != 0 {
                    counts[bytei * 8 + bit] += 1;
                }
            }
        }
    }
    let half = members.len() as i32 / 2;
    let mut out = vec![0u8; nbytes];
    for (i, c) in counts.iter().enumerate() {
        if *c > half {
            out[i / 8] |= 1 << (i % 8);
        }
    }
    out
}

/// 返回 `bits` 在 `centroids` 里汉明最近的下标(centroids 非空;等长项才参与)。
fn nearest_centroid(bits: &[u8], centroids: &[Vec<u8>]) -> usize {
    let mut best = 0usize;
    let mut bestd = u32::MAX;
    for (i, c) in centroids.iter().enumerate() {
        if c.len() != bits.len() {
            continue;
        }
        let d = hamming(bits, c);
        if d < bestd {
            bestd = d;
            best = i;
        }
    }
    best
}

/// 在采样 bits 上跑二值 k-means,产出 ≤k 个二值质心(空输入/k=0 返回空)。
/// 初始质心用等距抽样(确定性,免随机源,可复现);空簇保留旧质心不退化为全 0。
fn train_binary_centroids(sample: &[Vec<u8>], k: usize, iters: usize) -> Vec<Vec<u8>> {
    if sample.is_empty() || k == 0 {
        return Vec::new();
    }
    let nbytes = sample[0].len();
    let k = k.min(sample.len());
    let stride = (sample.len() / k).max(1);
    let mut centroids: Vec<Vec<u8>> = (0..k)
        .map(|i| sample[(i * stride).min(sample.len() - 1)].clone())
        .collect();
    for _ in 0..iters {
        let mut buckets: Vec<Vec<&[u8]>> = vec![Vec::new(); centroids.len()];
        for s in sample {
            let c = nearest_centroid(s, &centroids);
            buckets[c].push(s.as_slice());
        }
        for (ci, bucket) in buckets.iter().enumerate() {
            if !bucket.is_empty() {
                centroids[ci] = majority_bits(bucket, nbytes);
            }
        }
    }
    centroids
}

#[derive(Debug, Clone, Serialize)]
pub struct OptimizeSummary {
    pub model: String,
    pub chunks: u64,
    pub cells: u64,
    pub assigned: u64,
    pub seconds: f64,
    /// 提前结束/跳过的说明
    pub note: String,
}

/// 训练 IVF 质心并把每个向量分配到最近 cell(20TB 级 ANN 的「建索引」步,适合巡夜/大批入库后跑)。
/// 同模型 chunk < `IVF_MIN_CHUNKS` 时跳过(全扫已够快)。可被取消(分配按批轮询 CANCEL)。
pub fn optimize_vectors() -> Result<OptimizeSummary, String> {
    let started = std::time::Instant::now();
    let conn = open_db()?;
    let model = active_embed_model().ok_or("没有可用的嵌入服务商,无法确定向量模型")?;
    let (n, dim): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(MAX(dim),0) FROM chunks WHERE model=?1 AND bits IS NOT NULL",
            [&model],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    if n < IVF_MIN_CHUNKS {
        return Ok(OptimizeSummary {
            model,
            chunks: n as u64,
            cells: 0,
            assigned: 0,
            seconds: started.elapsed().as_secs_f64(),
            note: format!("chunk 数 {n} < {IVF_MIN_CHUNKS}:全表暴力扫已足够快,跳过建 cell"),
        });
    }
    // K ≈ √N(IVF 经验值),封顶 8192;采样训练集等距抽样。
    let k = ((n as f64).sqrt() as usize).clamp(64, 8192);
    let sample_n = IVF_SAMPLE.min(n as usize);
    let stride = (n as usize / sample_n.max(1)).max(1);
    let sample: Vec<Vec<u8>> = {
        // 内存治理:旧实现 SELECT 整列后在 Rust 侧 `i % stride` 抽样 —— 虽然 `out` 有上限,
        // 但 query_map 闭包对**每一行**都 `get::<Vec<u8>>` 把 bits BLOB 拷进堆再丢弃,
        // 千万级 chunk 下白白 materialize 整列(每条 ~128B,合计 ~GB 的瞬时分配 + 透 mmap
        // 读穿整列)。把等距抽样下推到 SQL:`id % stride = 0` 让引擎只为命中行取 BLOB,
        // BLOB 读取量从「全表」降到「sample_n 条」。stride=1(小库)时恒真,等价取全部 ≤ LIMIT。
        let mut stmt = conn
            .prepare(
                "SELECT bits FROM chunks WHERE model=?1 AND dim=?2 AND bits IS NOT NULL \
                 AND (id % ?3)=0 LIMIT ?4",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(
                rusqlite::params![model, dim, stride as i64, sample_n as i64],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .map_err(|e| e.to_string())?;
        rows.flatten().collect()
    };
    let centroids = train_binary_centroids(&sample, k, IVF_ITERS);
    if centroids.is_empty() {
        return Err("IVF 训练得到空质心(无可用 bits)".into());
    }

    // 落质心:清旧 cell、把本模型全部 chunk 重置回 cell=-1、插入新质心,三步同一事务。
    // 重置必须与删质心同事务:否则分配循环被取消/崩溃后,未重分配的 chunk 仍指着已删除
    // (甚至被 rowid 复用成语义无关)的旧 cell —— 查询探针探不到、repair_vectors 只修 -1、
    // maybe_optimize 只数 -1,没有任何机制自愈,向量召回被静默破坏。重置成 -1 后它们
    // 走粗筛的 `cell=-1 OR ...` 兜底,慢但正确,且能被现有修复/触发机制接住。
    let mut cell_ids: Vec<i64> = Vec::with_capacity(centroids.len());
    {
        conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
        let seeded = (|| -> Result<(), String> {
            conn.execute("DELETE FROM vec_cells WHERE model=?1", [&model])
                .map_err(|e| e.to_string())?;
            conn.execute(
                "UPDATE chunks SET cell=-1 WHERE model=?1 AND cell<>-1",
                [&model],
            )
            .map_err(|e| e.to_string())?;
            let mut ins = conn
                .prepare_cached("INSERT INTO vec_cells(model, dim, bits, n) VALUES(?1,?2,?3,0)")
                .map_err(|e| e.to_string())?;
            for c in &centroids {
                ins.execute(rusqlite::params![model, dim, c])
                    .map_err(|e| e.to_string())?;
                cell_ids.push(conn.last_insert_rowid());
            }
            Ok(())
        })();
        if let Err(e) = seeded {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
        conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
    }

    // 分配:按主键 keyset 分页流式读(内存有界、避免在开着的游标上改表),逐批 UPDATE cell。
    let mut last_id = 0i64;
    let mut assigned = 0u64;
    loop {
        if cancelled() {
            break;
        }
        let batch: Vec<(i64, Vec<u8>)> = {
            let mut stmt = conn
                .prepare_cached(
                    "SELECT id, bits FROM chunks
                     WHERE model=?1 AND dim=?2 AND bits IS NOT NULL AND id>?3
                     ORDER BY id LIMIT 10000",
                )
                .map_err(|e| e.to_string())?;
            let rows: Vec<(i64, Vec<u8>)> = stmt
                .query_map(rusqlite::params![model, dim, last_id], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
                })
                .map_err(|e| e.to_string())?
                .flatten()
                .collect();
            rows
        };
        if batch.is_empty() {
            break;
        }
        last_id = batch.last().map(|x| x.0).unwrap_or(last_id);
        conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
        {
            let mut up = conn
                .prepare_cached("UPDATE chunks SET cell=?2 WHERE id=?1")
                .map_err(|e| e.to_string())?;
            for (id, bits) in &batch {
                let ci = nearest_centroid(bits, &centroids);
                up.execute(rusqlite::params![id, cell_ids[ci]])
                    .map_err(|e| e.to_string())?;
            }
        }
        conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
        assigned += batch.len() as u64;
    }

    // 回填每个 cell 的成员计数 n(P1⑤ 修):此前 INSERT 时写死 n=0、分配后从不更新 → vec_cells.n
    // 永远是 0。虽然检索探针目前不读 n(只按质心汉明距离选 cell),但「全是 0」让任何按密度做
    // nprobe 自适应 / 监控 cell 倾斜的逻辑失效,也是「索引是坏的」的直接证据。分配完一次性回填。
    if !cancelled() {
        conn.execute(
            "UPDATE vec_cells SET n=COALESCE(
                 (SELECT COUNT(*) FROM chunks WHERE chunks.cell=vec_cells.id AND chunks.model=vec_cells.model), 0)
             WHERE model=?1",
            [&model],
        )
        .map_err(|e| e.to_string())?;
    }

    Ok(OptimizeSummary {
        model,
        chunks: n as u64,
        cells: cell_ids.len() as u64,
        assigned,
        seconds: started.elapsed().as_secs_f64(),
        note: if cancelled() {
            "已取消(部分分配)".into()
        } else {
            "完成".into()
        },
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct RepairSummary {
    pub model: String,
    /// 清掉的陈旧向量数(model='' / 无 bits 的旧版本迁移残留)
    pub purged_stale: u64,
    /// 因陈旧向量清空而重置 chunked=0、待重嵌的文件数
    pub reset_files: u64,
    /// 增量重分配进现有 cell 的「未分配(cell=-1)」向量数
    pub reassigned: u64,
    pub cells: u64,
    pub seconds: f64,
    pub note: String,
}

/// IVF / 向量健康修复(P1⑤,不重训、廉价、可巡夜常跑):
///  ① 清陈旧向量:`model=''` 或无 `bits` 的旧迁移残留进不了二值粗筛车道、会被静默漏召回或错误打分;
///     删掉并把「因此再无当前模型向量」的文件标 `chunked=0`,等增量构建按当前模型重嵌。
///  ② 增量重分配:已建 cell 后新入库的 `cell=-1` 向量,**不重训质心**、只就近指派到现有 cell —— 把
///     「每查询对这些增量全表扫」收敛回 ANN 探针。比 `optimize_vectors` 全量重训便宜得多。
///  ③ 回填 `vec_cells.n` 成员计数。
/// 与构建/盘点共用 INDEXING 闸由命令层把守;此函数纯算可被取消。
pub fn repair_vectors() -> Result<RepairSummary, String> {
    let started = std::time::Instant::now();
    let conn = open_db()?;
    let model = active_embed_model().ok_or("没有可用的嵌入服务商,无法确定向量模型")?;

    // ── ① 清陈旧向量 + 重置受影响文件 ──
    let stale_files: Vec<i64> = {
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT file_id FROM chunks \
                 WHERE model='' OR model IS NULL OR bits IS NULL",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<i64> = stmt
            .query_map([], |r| r.get::<_, i64>(0))
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        rows
    };
    let purged_stale = conn
        .execute(
            "DELETE FROM chunks WHERE model='' OR model IS NULL OR bits IS NULL",
            [],
        )
        .map_err(|e| e.to_string())? as u64;
    let mut reset_files = 0u64;
    for fid in &stale_files {
        // 仅当该文件已无「当前模型」向量时才重置 chunked,避免给本就有好向量的文件做无谓重嵌。
        let remain: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM chunks WHERE file_id=?1 AND model=?2",
                rusqlite::params![fid, model],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if remain == 0 {
            reset_files += conn
                .execute(
                    "UPDATE files SET chunked=0 WHERE id=?1 AND chunked=1",
                    [fid],
                )
                .map_err(|e| e.to_string())? as u64;
        }
    }

    // ── ② 增量重分配 cell=-1 → 现有质心(不重训)──
    let centroids: Vec<Vec<u8>> = {
        let mut stmt = conn
            .prepare("SELECT id, bits FROM vec_cells WHERE model=?1 ORDER BY id")
            .map_err(|e| e.to_string())?;
        let rows: Vec<Vec<u8>> = stmt
            .query_map([&model], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
            })
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|(_, b)| b)
            .collect();
        rows
    };
    let cell_ids: Vec<i64> = {
        let mut stmt = conn
            .prepare("SELECT id FROM vec_cells WHERE model=?1 ORDER BY id")
            .map_err(|e| e.to_string())?;
        let rows: Vec<i64> = stmt
            .query_map([&model], |r| r.get::<_, i64>(0))
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        rows
    };
    let mut reassigned = 0u64;
    if !centroids.is_empty() {
        let mut last_id = 0i64;
        loop {
            if cancelled() {
                break;
            }
            let batch: Vec<(i64, Vec<u8>)> = {
                let mut stmt = conn
                    .prepare_cached(
                        "SELECT id, bits FROM chunks
                         WHERE model=?1 AND bits IS NOT NULL AND cell=-1 AND id>?2
                         ORDER BY id LIMIT 10000",
                    )
                    .map_err(|e| e.to_string())?;
                let rows: Vec<(i64, Vec<u8>)> = stmt
                    .query_map(rusqlite::params![model, last_id], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
                    })
                    .map_err(|e| e.to_string())?
                    .flatten()
                    .collect();
                rows
            };
            if batch.is_empty() {
                break;
            }
            last_id = batch.last().map(|x| x.0).unwrap_or(last_id);
            conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
            {
                let mut up = conn
                    .prepare_cached("UPDATE chunks SET cell=?2 WHERE id=?1")
                    .map_err(|e| e.to_string())?;
                for (id, bits) in &batch {
                    let ci = nearest_centroid(bits, &centroids);
                    up.execute(rusqlite::params![id, cell_ids[ci]])
                        .map_err(|e| e.to_string())?;
                }
            }
            conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
            reassigned += batch.len() as u64;
        }
    }

    // ── ③ 回填成员计数 ──
    conn.execute(
        "UPDATE vec_cells SET n=COALESCE(
             (SELECT COUNT(*) FROM chunks WHERE chunks.cell=vec_cells.id AND chunks.model=vec_cells.model), 0)
         WHERE model=?1",
        [&model],
    )
    .map_err(|e| e.to_string())?;

    Ok(RepairSummary {
        model,
        purged_stale,
        reset_files,
        reassigned,
        cells: cell_ids.len() as u64,
        seconds: started.elapsed().as_secs_f64(),
        note: if cancelled() {
            "已取消(部分修复)".into()
        } else if centroids.is_empty() {
            "已清陈旧+回填计数;尚无 cell(规模未到门槛,向量走全扫)".into()
        } else {
            "完成".into()
        },
    })
}

/// 构建尾声的自动维护:首次跨过 IVF 门槛(尚无 cell)时顺带建一次倒排单元,
/// 让向量车道在大规模下「开箱即亚秒」,无需用户手动点优化。已有 cell 后的增量
/// 维护(随新数据增长重训)交给显式 `fable_index_optimize`/CLI `fable optimize`(巡夜)。
pub(crate) fn maybe_optimize(conn: &rusqlite::Connection, model: &str) {
    let total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM chunks WHERE model=?1 AND bits IS NOT NULL",
            [model],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if total < IVF_MIN_CHUNKS {
        return;
    }
    let cells: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM vec_cells WHERE model=?1",
            [model],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if cells == 0 {
        let _ = optimize_vectors(); // 首次自动建;失败不影响构建结果(检索退回全扫)
        return;
    }
    // 已建 cell:增量数据(cell=-1)堆积过多时,廉价地就近折进现有 cell(不重训),
    // 避免向量车道对这批新数据退化成全表扫。门槛设保守值,免每轮构建尾都做无谓扫描。
    const REASSIGN_TRIGGER: i64 = 20_000;
    let unassigned: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM chunks WHERE model=?1 AND bits IS NOT NULL AND cell=-1",
            [model],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if unassigned >= REASSIGN_TRIGGER {
        let _ = repair_vectors(); // 含增量重分配+回填 n;失败不影响构建结果
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ivf_majority_and_nearest_centroid() {
        // 多数表决:3 成员,某位 ≥2 个置 1 → 该位为 1。
        let a = vec![0b0000_0001u8];
        let b = vec![0b0000_0011u8];
        let c = vec![0b0000_0010u8];
        let maj = majority_bits(&[&a, &b, &c], 1);
        assert_eq!(maj[0], 0b0000_0011); // bit0(a,b)、bit1(b,c)各 2/3 > 半数
                                         // 汉明最近质心。
        let cents = vec![vec![0b0000_0000u8], vec![0b1111_1111u8]];
        assert_eq!(nearest_centroid(&[0b0000_0001u8], &cents), 0);
        assert_eq!(nearest_centroid(&[0b1111_1110u8], &cents), 1);
    }

    #[test]
    fn ivf_train_separates_two_clusters() {
        // 两簇:全 0 与 全 1。训练出的两个质心应把两类样本分到不同 cell。
        let mut sample: Vec<Vec<u8>> = Vec::new();
        for _ in 0..50 {
            sample.push(vec![0u8, 0u8]);
        }
        for _ in 0..50 {
            sample.push(vec![0xFFu8, 0xFFu8]);
        }
        let cents = train_binary_centroids(&sample, 2, IVF_ITERS);
        assert_eq!(cents.len(), 2);
        let c0 = nearest_centroid(&[0u8, 0u8], &cents);
        let c1 = nearest_centroid(&[0xFFu8, 0xFFu8], &cents);
        assert_ne!(c0, c1, "两个分得很开的簇应落到不同质心");
        // 空输入 / k=0 安全返回空。
        assert!(train_binary_centroids(&[], 4, 4).is_empty());
        assert!(train_binary_centroids(&sample, 0, 4).is_empty());
    }

    #[test]
    fn ivf_n_backfill_counts_members_per_cell() {
        // P1⑤ 修:vec_cells.n 此前恒为 0;回填 SQL 必须把每个 cell 的实际成员数算对,
        // 且只算「同模型」成员(跨模型不串)。用 in-memory 库最小复刻 chunks/vec_cells。
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE chunks(id INTEGER PRIMARY KEY, model TEXT, cell INTEGER);
             CREATE TABLE vec_cells(id INTEGER PRIMARY KEY, model TEXT, n INTEGER DEFAULT 0);
             INSERT INTO vec_cells(id,model,n) VALUES (10,'m',0),(11,'m',0),(12,'m',0),(20,'other',0);
             -- m 模型:cell 10 三个、cell 11 一个、cell 12 零个、未分配(-1)两个
             INSERT INTO chunks(model,cell) VALUES
               ('m',10),('m',10),('m',10),('m',11),('m',-1),('m',-1),
               -- 另一模型的成员落在同号 cell 上,绝不能被算进 m 的计数
               ('other',10),('other',10);",
        )
        .unwrap();
        conn.execute(
            "UPDATE vec_cells SET n=COALESCE(
                 (SELECT COUNT(*) FROM chunks WHERE chunks.cell=vec_cells.id AND chunks.model=vec_cells.model), 0)
             WHERE model=?1",
            ["m"],
        )
        .unwrap();
        let n = |id: i64| -> i64 {
            conn.query_row("SELECT n FROM vec_cells WHERE id=?1", [id], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(n(10), 3, "cell 10 应有 3 个 m 成员(不含 other 的同号干扰)");
        assert_eq!(n(11), 1);
        assert_eq!(n(12), 0, "空 cell 计数为 0,而非保留旧值");
        assert_eq!(n(20), 0, "other 模型未被本次回填触碰");
    }
}
