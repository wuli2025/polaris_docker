//! 向量车道:IVF 探针 → 二值汉明粗筛(多核分片)→ f32 点积精排 → 回读文本。

use super::*;

pub(crate) struct VecHit {
    pub(crate) path: String,
    pub(crate) abspath: String,
    pub(crate) seq: i64,
    pub(crate) text: String,
    pub(crate) score: f32,
}

/// 二值粗筛(P1-1 + 多核):在 `bits` 列上算汉明距离选出 top `cand_n` 个候选 chunk id。
/// 读量只有 f32 的 1/32。按主键 `id` 区间把扫描分片到 worker_count 个线程并行(各开连接,
/// WAL 并发读),每片本地留 top cand_n 再归并 —— 此前是单线程,大库下成为瓶颈(grep 车道
/// 早已多核;IVF 的 cell=-1 子句令「新嵌入未重训」的增量向量每查询全表扫,这条热路尤其受益)。
///
/// 归并正确性:全局 top cand_n ⊆ ⋃(各片 top cand_n)—— 任一全局前 cand_n 的元素,在其所在
/// 分片内排名也 ≤ cand_n(全局更优者至多 cand_n-1 个,落到该片只会更少),故每片留 cand_n 足够。
/// P2-2 只认与当前模型一致、维度匹配的向量。`probes` 非空 → 只扫探针 cell + 未分配新数据。
fn coarse_candidates(
    qbits: &[u8],
    model: &str,
    dim: i64,
    cand_n: usize,
    probes: &[i64],
) -> Result<Vec<(i64, u32)>, String> {
    // cell 过滤片段:片段里的裸 `?` 由 SQLite 续编号在 ?1..?4 之后(?5、?6…),与 probes
    // 在参数表中的位置对齐。probes 为空 → 无片段(全表回退)。
    let cell_frag = if probes.is_empty() {
        String::new()
    } else {
        let csv = probes.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        format!(" AND (cell=-1 OR cell IN ({csv}))")
    };

    // 参与粗筛的行的 id 跨度 → 等分给各 worker(id 唯一,跨度 ≥ 行数;跨度小即行数少,单线程即可)。
    let (id_min, id_max): (Option<i64>, Option<i64>) = {
        let conn = open_db()?;
        let sql = format!(
            "SELECT MIN(id), MAX(id) FROM chunks \
             WHERE dim=?1 AND model=?2 AND bits IS NOT NULL{cell_frag}"
        );
        let mut params: Vec<rusqlite::types::Value> = vec![
            rusqlite::types::Value::Integer(dim),
            rusqlite::types::Value::Text(model.to_string()),
        ];
        for p in probes {
            params.push(rusqlite::types::Value::Integer(*p));
        }
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        stmt.query_row(rusqlite::params_from_iter(params.iter()), |r| {
            Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?))
        })
        .map_err(|e| e.to_string())?
    };
    let (Some(lo), Some(hi)) = (id_min, id_max) else {
        return Ok(Vec::new()); // 该模型无可粗筛的向量
    };
    let span = hi - lo + 1;
    // 小库不分片:省掉多开连接/起线程的固定开销(跨度小 ⇒ 行数少,单线程已够快)。
    let w = if span < 50_000 { 1 } else { worker_count() };
    coarse_scan_ranged(qbits, model, dim, cand_n, probes, &cell_frag, lo, hi, w)
}

/// [`coarse_candidates`] 的分片扫描内核:在 `[lo,hi]` 上按 `w` 路并行做汉明粗筛并归并。
/// 抽出来是为了让测试能强制 `w>1`(真机库 id 跨度可能小于自动分片阈值),逐位对拍单线程结果。
#[allow(clippy::too_many_arguments)]
fn coarse_scan_ranged(
    qbits: &[u8],
    model: &str,
    dim: i64,
    cand_n: usize,
    probes: &[i64],
    cell_frag: &str,
    lo: i64,
    hi: i64,
    w: usize,
) -> Result<Vec<(i64, u32)>, String> {
    let w = w.max(1);
    let span = hi - lo + 1;
    // [lo, hi] 等分成 w 个左闭右开区间(末片右界 hi+1),覆盖完整且互不相交。
    // ceil(span/w) 手算(i64::div_ceil 尚不稳定;span≥1、w≥1 无溢出)。
    let step = ((span + w as i64 - 1) / w as i64).max(1);
    let ranges: Vec<(i64, i64)> = (0..w as i64)
        .map(|i| (lo + i * step, (lo + (i + 1) * step).min(hi + 1)))
        .filter(|(a, b)| a < b)
        .collect();

    let collected: Mutex<Vec<(i64, u32)>> = Mutex::new(Vec::new());
    let mut cand: Vec<(i64, u32)> = {
        let mut first_err: Option<String> = None;
        std::thread::scope(|s| {
            let handles: Vec<_> = ranges
                .iter()
                .map(|&(rlo, rhi)| {
                    let collected = &collected;
                    s.spawn(move || -> Result<(), String> {
                        // 计量连接:这是并发开连接的热点(最多 w≤12 路同时持有),
                        // 守卫 drop 时自减,超软上限只告警不阻塞 —— 让用户能看见最坏并发度。
                        let conn = open_db_gauged()?;
                        let sql = format!(
                            "SELECT id, bits FROM chunks \
                             WHERE dim=?1 AND model=?2 AND bits IS NOT NULL \
                             AND id>=?3 AND id<?4{cell_frag}"
                        );
                        let mut params: Vec<rusqlite::types::Value> = vec![
                            rusqlite::types::Value::Integer(dim),
                            rusqlite::types::Value::Text(model.to_string()),
                            rusqlite::types::Value::Integer(rlo),
                            rusqlite::types::Value::Integer(rhi),
                        ];
                        for p in probes {
                            params.push(rusqlite::types::Value::Integer(*p));
                        }
                        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
                        let mut rows = stmt
                            .query(rusqlite::params_from_iter(params.iter()))
                            .map_err(|e| e.to_string())?;
                        let mut local: Vec<(i64, u32)> = Vec::with_capacity(cand_n + 1);
                        let mut worst = u32::MAX;
                        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
                            // 借用读 bits,免去每行一次 Vec<u8> 堆分配(大库百万行 → 省百万次)。
                            let bits = row
                                .get_ref(1)
                                .map_err(|e| e.to_string())?
                                .as_blob()
                                .map_err(|e| e.to_string())?;
                            if bits.len() != qbits.len() {
                                continue;
                            }
                            let h = super::index::hamming(qbits, bits);
                            if local.len() >= cand_n && h >= worst {
                                continue;
                            }
                            let id: i64 = row.get(0).map_err(|e| e.to_string())?;
                            local.push((id, h));
                            if local.len() > cand_n {
                                local.sort_by_key(|x| x.1);
                                local.truncate(cand_n);
                                worst = local.last().map(|x| x.1).unwrap_or(u32::MAX);
                            }
                        }
                        collected.lock().unwrap().extend(local);
                        Ok(())
                    })
                })
                .collect();
            for h in handles {
                match h.join() {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        let _ = first_err.get_or_insert(e);
                    }
                    Err(_) => {
                        let _ = first_err.get_or_insert_with(|| "向量粗筛线程 panic".into());
                    }
                }
            }
        });
        if let Some(e) = first_err {
            return Err(e);
        }
        collected.into_inner().unwrap()
    };
    cand.sort_by_key(|x| x.1);
    cand.truncate(cand_n);
    Ok(cand)
}

pub(crate) fn vector_lane(query: &str, top_k: usize) -> Result<Vec<VecHit>, String> {
    // P1-5:查询嵌入走 LRU 缓存(已归一化);断网/限速时上抛,search() 静默降级保 grep/FTS 腿。
    let qv = super::index::embed_query(query)?;
    let model = super::index::active_embed_model().unwrap_or_default();
    let qbits = super::index::bits_of(&qv);
    // 返回给融合层的向量候选数。原 top_k*2 偏窄 —— 语义相关但在向量腿排 20+ 的文件会在进融合
    // **之前**就被丢掉,recall 漏召。放宽到 top_k*4(且不低于 30):粗筛池本就有数百候选,多带
    // 几十个进 RRF 几乎零成本,而 RRF 名次衰减(rank 30 仅 ~1/90)天然压住额外噪声 —— recall 净赚。
    let want = (top_k * 4).max(30);

    let conn = open_db()?;

    // ── IVF 探针(20TB ANN):若该模型已建倒排单元,先在质心里找最近的 nprobe 个 cell,
    //    第一段只在这些 cell(+cell=-1 的未分配新数据)里粗筛,把全表 O(N) 扫降到
    //    ~O(N·nprobe/cells);未建 cell 时 probes 为空 → 退回全表扫(零回归)。 ──
    let probes: Vec<i64> = {
        let mut stmt = conn
            .prepare("SELECT id, bits, n FROM vec_cells WHERE model=?1 AND dim=?2")
            .map_err(|e| e.to_string())?;
        let cells: Vec<(i64, Vec<u8>, i64)> = stmt
            .query_map(rusqlite::params![model, qv.len() as i64], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        if cells.is_empty() {
            Vec::new()
        } else {
            // 用回填的成员计数 n 跳过空 cell:nprobe 个探针槽全部指向有成员的 cell → 同等
            // 探针预算下有效候选覆盖更广、向量召回更稳。**零回归兜底**:若全部 cell 的 n 都是
            // 0(索引早于 n 回填修复、计数尚未刷新),`nonempty` 为空 → 退回「按汉明在全部 cell
            // 里选」的旧行为(召回安全),等一次 fable_index_repair 刷新 n 后自动启用密度感知。
            let nonempty: Vec<&(i64, Vec<u8>, i64)> = cells.iter().filter(|c| c.2 > 0).collect();
            let pool: Vec<&(i64, Vec<u8>, i64)> = if nonempty.is_empty() {
                cells.iter().collect()
            } else {
                nonempty
            };
            // nprobe ≈ √(有效 cell 数),夹在 [8,64]:扫约 nprobe/pool 比例的向量。
            let nprobe = ((pool.len() as f64).sqrt() as usize).clamp(8, 64);
            let mut scored: Vec<(u32, i64)> = pool
                .iter()
                .filter(|(_, b, _)| b.len() == qbits.len())
                .map(|(id, b, _)| (super::index::hamming(&qbits, b), *id))
                .collect();
            scored.sort_by_key(|x| x.0);
            scored.truncate(nprobe);
            scored.into_iter().map(|x| x.1).collect()
        }
    };

    // ── 第一段 · 二值粗筛(多核分片,实现见 coarse_candidates)──
    let cand_n = (top_k * 8).max(200);
    let dim = qv.len() as i64;
    let cand = coarse_candidates(&qbits, &model, dim, cand_n, &probes)?;

    // ── 第二段 · 精排(P1-3):分两步省 IO / 分配 ──
    //   ① 只读候选的 (id, vec),借用 blob 算点积打分(无 JOIN、不物化全文);
    //   ② 仅对最终入选的 want(≈top_k·2)条回读 seq/text/路径并拼装。
    // 此前对全部 cand_n(≥200)条都 JOIN files/roots 且把每条 chunk 全文 String 物化进内存,
    // 而真正进入融合的只有 want(~24)条 → 白读 ~8 倍全文、白做 ~8 倍 JOIN 行物化。现在重载荷
    // (全文 + 两段路径 + JOIN)只搬入选条;粗筛已把候选收到几百,二段读 (id,vec) 也是顺序小读。
    let mut top: Vec<VecHit> = Vec::new();
    if !cand.is_empty() {
        let ids: Vec<i64> = cand.iter().map(|x| x.0).collect();
        // 步骤①:打分(只读 vec,借用算点积)。
        let mut scored: Vec<(i64, f32)> = Vec::with_capacity(ids.len());
        for group in ids.chunks(500) {
            let placeholders = group.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!("SELECT c.id, c.vec FROM chunks c WHERE c.id IN ({placeholders})");
            let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
            let mut rows = stmt
                .query(rusqlite::params_from_iter(group.iter()))
                .map_err(|e| e.to_string())?;
            while let Some(row) = rows.next().map_err(|e| e.to_string())? {
                let blob = row
                    .get_ref(1)
                    .map_err(|e| e.to_string())?
                    .as_blob()
                    .map_err(|e| e.to_string())?;
                let Some(score) = super::index::dot_blob(&qv, blob) else {
                    continue;
                };
                let id: i64 = row.get(0).map_err(|e| e.to_string())?;
                scored.push((id, score));
            }
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(want);
        // 步骤②:仅回读入选条的 seq/text/路径(JOIN 只跑 want 行),按 id 建 map 再按分数序拼装。
        if !scored.is_empty() {
            let win_ids: Vec<i64> = scored.iter().map(|x| x.0).collect();
            let mut meta: HashMap<i64, (i64, String, String, String)> =
                HashMap::with_capacity(win_ids.len());
            for group in win_ids.chunks(500) {
                let placeholders = group.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                let sql = format!(
                    "SELECT c.id, c.seq, c.text, f.relpath, r.path FROM chunks c
                     JOIN files f ON f.id=c.file_id JOIN roots r ON r.id=f.root_id
                     WHERE c.id IN ({placeholders})"
                );
                let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
                let mut rows = stmt
                    .query(rusqlite::params_from_iter(group.iter()))
                    .map_err(|e| e.to_string())?;
                while let Some(row) = rows.next().map_err(|e| e.to_string())? {
                    let id: i64 = row.get(0).map_err(|e| e.to_string())?;
                    let seq: i64 = row.get(1).map_err(|e| e.to_string())?;
                    let text: String = row.get(2).map_err(|e| e.to_string())?;
                    let rel: String = row.get(3).map_err(|e| e.to_string())?;
                    let root: String = row.get(4).map_err(|e| e.to_string())?;
                    meta.insert(id, (seq, text, rel, root));
                }
            }
            // 按分数序拼装(scored 已降序);缺失项(并发删改的极端情况)跳过。
            for (id, score) in scored {
                if let Some((seq, text, rel, root)) = meta.remove(&id) {
                    top.push(VecHit {
                        abspath: std::path::Path::new(&root)
                            .join(&rel)
                            .to_string_lossy()
                            .into_owned(),
                        path: rel,
                        seq,
                        text,
                        score,
                    });
                }
            }
        }
    } else {
        // 兜底:同模型向量里没有任何 bits(理论上不出现,留作健壮性)→ 暴力精扫,仍按 model 过滤。
        //
        // 内存治理:旧实现一条 SQL 就 JOIN + SELECT 整列 `text`(每 chunk 1–2KB),粗筛初期
        // min_score=MIN 几乎每行都过闸 → 把成千上万行的整段文本 String 全 materialize 进堆,
        // 在百万级 chunk 库上可吃掉数 GB。改两段式:
        //   stage-1 只读 (id, vec) 算分,维护 top-want 的 (id,score)(零文本、零 JOIN);
        //   stage-2 仅对最终入选的 ~want 条回读 text + 路径。文本驻留从「全表」降到「want 条」。
        let mut cand: Vec<(i64, f32)> = Vec::with_capacity(want + 1);
        {
            let mut stmt = conn
                .prepare("SELECT c.id, c.vec FROM chunks c WHERE c.dim=?1 AND c.model=?2")
                .map_err(|e| e.to_string())?;
            let mut rows = stmt
                .query(rusqlite::params![qv.len() as i64, model])
                .map_err(|e| e.to_string())?;
            let mut min_score = f32::MIN;
            while let Some(row) = rows.next().map_err(|e| e.to_string())? {
                let blob = row
                    .get_ref(1)
                    .map_err(|e| e.to_string())?
                    .as_blob()
                    .map_err(|e| e.to_string())?;
                let Some(score) = super::index::dot_blob(&qv, blob) else {
                    continue;
                };
                if cand.len() >= want && score <= min_score {
                    continue;
                }
                let id: i64 = row.get(0).map_err(|e| e.to_string())?;
                cand.push((id, score));
                if cand.len() > want {
                    cand.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                    cand.truncate(want);
                    min_score = cand.last().map(|c| c.1).unwrap_or(f32::MIN);
                }
            }
        }
        // stage-2:仅回读入选 chunk 的文本与路径(逐条按主键查,命中索引,数量 ≤ want)。
        let mut stmt = conn
            .prepare(
                "SELECT c.seq, c.text, f.relpath, r.path FROM chunks c
                 JOIN files f ON f.id=c.file_id JOIN roots r ON r.id=f.root_id
                 WHERE c.id=?1",
            )
            .map_err(|e| e.to_string())?;
        for (id, score) in cand {
            let row = stmt.query_row(rusqlite::params![id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            });
            if let Ok((seq, text, rel, root)) = row {
                top.push(VecHit {
                    abspath: std::path::Path::new(&root)
                        .join(&rel)
                        .to_string_lossy()
                        .into_owned(),
                    path: rel,
                    seq,
                    text,
                    score,
                });
            }
        }
    }
    top.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    top.truncate(want);
    Ok(top)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真机端到端:用本机已建的 fable.db 验证「分片并行粗筛」与「单线程暴力全扫」选出的
    /// 候选**距离分布逐位一致**(选最小 cand_n 个距离无歧义,即便边界并列)。强制 w=8 以真正
    /// 触发分片(真机库 id 跨度可能小于自动阈值)。无库/库太小则跳过,不拖累常规 CI。
    /// 取一条真实向量的 bits 当查询 → 必含一个距离 0 的自命中。
    #[test]
    fn coarse_scan_parallel_matches_bruteforce_on_real_db() {
        let Ok(conn) = open_db() else { return };
        let model = "BAAI/bge-m3";
        let dim: i64 = 1024;
        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM chunks WHERE model=?1 AND dim=?2 AND bits IS NOT NULL",
                rusqlite::params![model, dim],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if total < 1000 {
            eprintln!("[real-db] 跳过:本机无足量向量(total={total})");
            return;
        }
        let (lo, hi): (i64, i64) = conn
            .query_row(
                "SELECT MIN(id), MAX(id) FROM chunks WHERE model=?1 AND dim=?2 AND bits IS NOT NULL",
                rusqlite::params![model, dim],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let (self_id, qbits): (i64, Vec<u8>) = conn
            .query_row(
                "SELECT id, bits FROM chunks WHERE model=?1 AND dim=?2 AND bits IS NOT NULL \
                 ORDER BY id LIMIT 1",
                rusqlite::params![model, dim],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let cand_n = 200usize;

        // 单线程暴力参考(把全部 bits 读进来逐个算汉明,取最小 cand_n)。
        let t_bf = std::time::Instant::now();
        let mut bf: Vec<(i64, u32)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT id, bits FROM chunks WHERE model=?1 AND dim=?2 AND bits IS NOT NULL",
                )
                .unwrap();
            let rows = stmt
                .query_map(rusqlite::params![model, dim], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
                })
                .unwrap();
            rows.flatten()
                .filter(|(_, b)| b.len() == qbits.len())
                .map(|(id, b)| (id, crate::fable::index::hamming(&qbits, &b)))
                .collect()
        };
        bf.sort_by_key(|x| x.1);
        bf.truncate(cand_n);
        let bf_ms = t_bf.elapsed().as_secs_f64() * 1000.0;

        // 被测:单线程(w=1)与分片并行(w=8)两条真实代码路径。
        let t1 = std::time::Instant::now();
        let serial = coarse_scan_ranged(&qbits, model, dim, cand_n, &[], "", lo, hi, 1).unwrap();
        let s_ms = t1.elapsed().as_secs_f64() * 1000.0;
        let t8 = std::time::Instant::now();
        let par = coarse_scan_ranged(&qbits, model, dim, cand_n, &[], "", lo, hi, 8).unwrap();
        let p_ms = t8.elapsed().as_secs_f64() * 1000.0;

        // 自命中(距离 0)必须在两路结果里。
        assert!(serial.iter().any(|&(id, h)| id == self_id && h == 0));
        assert!(par.iter().any(|&(id, h)| id == self_id && h == 0));

        // 距离分布逐位一致:并行分片归并 == 单线程 == 暴力参考。
        let dist = |v: &[(i64, u32)]| {
            let mut d: Vec<u32> = v.iter().map(|x| x.1).collect();
            d.sort();
            d
        };
        let (ds, dp, db) = (dist(&serial), dist(&par), dist(&bf));
        assert_eq!(ds.len(), cand_n, "应选满 cand_n 个候选");
        assert_eq!(ds, db, "单线程粗筛距离分布须与暴力参考一致");
        assert_eq!(
            dp, db,
            "分片并行(w=8)距离分布须与暴力参考一致 —— 分片/归并正确"
        );

        eprintln!(
            "[real-db coarse] N={total} cand_n={cand_n} | 暴力(读全f32略)≈{bf_ms:.1}ms \
             单线程粗筛={s_ms:.1}ms 分片x8={p_ms:.1}ms 提速x{:.2}",
            s_ms / p_ms.max(0.001)
        );
    }
}
