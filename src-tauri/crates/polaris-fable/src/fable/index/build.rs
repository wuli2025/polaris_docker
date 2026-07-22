use super::*;

// ───────────────────────── 构建管线(三壳共用)─────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct IndexSummary {
    pub files_done: u64,
    pub chunks_added: u64,
    pub files_pending: u64,
    pub seconds: f64,
    /// 提前停的原因(预算耗尽/取消/全部完成)
    pub stopped: String,
}

/// 文本内容指纹:全文 SHA-256 取前 128 位(32 hex),前缀 `f:` 标明「全文哈希」(留 `s:` 采样哈希扩展位)。
/// 作用于索引管线已读入内存的文本,零额外 IO。跨路径同内容 → 同指纹,是精确去重与「移动免重嵌」的判据。
pub(crate) fn content_fingerprint(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    let d = h.finalize();
    let mut s = String::with_capacity(34);
    s.push_str("f:");
    for b in d.iter().take(16) {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 文件名归一化键:去掉扩展名与「版本噪声」token(v2 / 日期 / final / 副本 / (1) …),小写后拼接。
/// 仅在「同 root + 同目录 + 同 ext」范围内做等值比较,用于识别「同一份资料的不同版本」→ 新压旧。
/// 保守起见只剪能被分隔符切出的独立噪声 token(CJK 无分隔的「报告最终版」整体保留,交给内容哈希兜底)。
pub(crate) fn doc_key(name: &str) -> String {
    // stem:去掉最后一个扩展名(隐藏文件 .gitignore 的前导点不算扩展名)
    let stem = match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    };
    let lower = stem.to_lowercase();
    let is_noise = |t: &str| -> bool {
        // v1 / v10 / ver2
        let vtail = t.strip_prefix("ver").or_else(|| t.strip_prefix('v'));
        if let Some(rest) = vtail {
            if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
        }
        // 纯数字:≤3 位副本序号,或 8 位日期(20230101)
        if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) {
            let n = t.len();
            if n <= 3 || n == 8 {
                return true;
            }
        }
        matches!(
            t,
            "final"
                | "finalversion"
                | "copy"
                | "draft"
                | "new"
                | "old"
                | "latest"
                | "最终"
                | "最终版"
                | "定稿"
                | "副本"
                | "草稿"
                | "最新"
                | "修改"
                | "修订"
        )
    };
    let mut kept = String::new();
    for tok in lower.split(|c: char| {
        matches!(
            c,
            ' ' | '\t'
                | '　'
                | '-'
                | '_'
                | '.'
                | '('
                | ')'
                | '['
                | ']'
                | '（'
                | '）'
                | '【'
                | '】'
                | '·'
                | ','
        )
    }) {
        let t = tok.trim();
        if t.is_empty() || is_noise(t) {
            continue;
        }
        kept.push_str(t);
    }
    if kept.is_empty() {
        // 全被剪光(如文件名就叫「v2」):回退为去分隔符的原 stem,避免空 key 误并
        lower.chars().filter(|c| !c.is_whitespace()).collect()
    } else {
        kept
    }
}

/// 把攒到的「跨文件 chunk 缓冲」一次性嵌入并落库。
///
/// 「凑批只凑计算,落库仍按文件」:先把整个缓冲切成 batch 宽的组、并发嵌完**所有**批,
/// 全部成功后才在单事务里逐文件 DELETE 旧 chunk + INSERT 新 chunk + 标 chunked=1。
/// 任一批出错 → 整个 flush 放弃(不 BEGIN、旧 chunk 未动、涉及文件保持 chunked=0),
/// 返回 Err,由调用方留待下轮重试。海量小文件时把「每文件一次 API 往返」聚成满批,
/// 限速档(瓶颈是请求数而非字节)吞吐显著抬升。
///
/// `keys[i]=(file_id, seq)` 与 `texts[i]` 平行对齐。返回提交的 chunk 数。
fn flush_embed_buffer(
    conn: &rusqlite::Connection,
    keys: &[(i64, i64)],
    texts: &[String],
    model: &str,
    batch: usize,
) -> Result<u64, String> {
    debug_assert_eq!(keys.len(), texts.len());
    if texts.is_empty() {
        return Ok(0);
    }
    // ── 并发嵌入所有批(与旧单文件路径同构:纯网络、无共享态)──
    let groups: Vec<&[String]> = texts.chunks(batch).collect();
    let mut all_vecs: Vec<Vec<Vec<f32>>> = vec![Vec::new(); groups.len()];
    {
        let next = std::sync::atomic::AtomicUsize::new(0);
        let collected: Mutex<Vec<(usize, Result<Vec<Vec<f32>>, String>)>> =
            Mutex::new(Vec::with_capacity(groups.len()));
        let nthreads = embed_concurrency().min(groups.len()).max(1);
        std::thread::scope(|s| {
            for _ in 0..nthreads {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= groups.len() {
                        break;
                    }
                    let r = embed_texts(groups[i]);
                    collected.lock().unwrap().push((i, r));
                });
            }
        });
        for (i, r) in collected.into_inner().unwrap() {
            match r {
                Ok(mut vecs) => {
                    for v in vecs.iter_mut() {
                        normalize(v); // 入库归一化一次 → 查询退化成纯点积
                    }
                    all_vecs[i] = vecs;
                }
                Err(e) => return Err(e), // 整个 flush 放弃,旧 chunk 未动、chunked 仍 0
            }
        }
    }
    // 展平回与 keys 对齐的顺序
    let mut flat: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
    for g in all_vecs {
        for v in g {
            flat.push(v);
        }
    }
    if flat.len() != keys.len() {
        return Err(format!(
            "嵌入返回数与请求数不符({} vs {})",
            flat.len(),
            keys.len()
        ));
    }
    // 涉及的 file_id(保序去重)——DELETE 旧 chunk 与 UPDATE chunked 各做一次
    let mut file_ids: Vec<i64> = Vec::new();
    for (fid, _) in keys {
        if file_ids.last() != Some(fid) && !file_ids.contains(fid) {
            file_ids.push(*fid);
        }
    }
    // ── 单事务落库:DELETE 移进事务(旧 chunk 直到新 chunk 提交才消失,失败时旧向量仍可检索)──
    // 出错必须显式 ROLLBACK:带着打开的事务返回会让同连接后续的 maybe_optimize 等写操作
    // 撞 busy_timeout 白等 20s(事务只在连接 drop 时才隐式回滚)。
    conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
    let landed = (|| -> Result<(), String> {
        for fid in &file_ids {
            conn.execute("DELETE FROM chunks WHERE file_id=?1", [fid])
                .map_err(|e| e.to_string())?;
        }
        let mut stmt = conn
            .prepare_cached(
                "INSERT OR REPLACE INTO chunks(file_id,seq,text,dim,vec,model,bits)
                 VALUES(?1,?2,?3,?4,?5,?6,?7)",
            )
            .map_err(|e| e.to_string())?;
        for (((fid, seq), t), v) in keys.iter().zip(texts.iter()).zip(flat.iter()) {
            stmt.execute(rusqlite::params![
                fid,
                seq,
                t,
                v.len() as i64,
                vec_to_blob(v),
                model,
                bits_of(v),
            ])
            .map_err(|e| e.to_string())?;
        }
        for fid in &file_ids {
            conn.execute("UPDATE files SET chunked=1 WHERE id=?1", [fid])
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    if let Err(e) = landed {
        let _ = conn.execute_batch("ROLLBACK");
        return Err(e);
    }
    conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
    Ok(keys.len() as u64)
}

/// chunk 上下文头实验开关(默认关)。开启后每个 chunk 前缀「【文件名 · 父目录】」,给向量注入
/// 文件级上下文(文献与工程上验证过的 contextual-header 手法),尤其利于「文件名点题、正文不点题」。
fn chunk_header_enabled() -> bool {
    std::env::var("POLARIS_CHUNK_HEADER")
        .map(|v| v.trim() == "1")
        .unwrap_or(false)
}

/// 由相对路径构造上下文头「【文件名 · 父目录】\n」,总长截到 ~80 字符(不挤占 chunk 正文预算)。
fn chunk_header_for(rel: &str) -> String {
    let norm = rel.replace('\\', "/");
    let name = norm.rsplit('/').next().unwrap_or(&norm);
    let parent = {
        let p = match norm.rfind('/') {
            Some(i) => &norm[..i],
            None => "",
        };
        p.rsplit('/').next().unwrap_or("")
    };
    let mut inner = if parent.is_empty() {
        name.to_string()
    } else {
        format!("{name} · {parent}")
    };
    if inner.chars().count() > 72 {
        inner = inner.chars().take(72).collect();
    }
    format!("【{inner}】\n")
}

/// 同步构建:消化 pending 文本文件直到预算耗尽。`progress(files_done, chunks_added, current)`。
pub fn build_index(
    max_chunks: usize,
    progress: &dyn Fn(u64, u64, &str),
) -> Result<IndexSummary, String> {
    let started = std::time::Instant::now();
    let conn = open_db()?;
    // ── 认字腿 / 认意思腿解耦(关键修)──
    // 旧实现:没有嵌入 key 直接 `?` 整体放弃 → 倒排(FTS,**零网络**)也跟着建不起来,
    // 全盘几百 GB 资料只有 ~5% 进过索引,绝大多数搜不到。现在两腿独立:
    // - 有 key → 认字腿 + 认意思腿都建;
    // - 无 key → 只建认字腿(全盘文本进 FTS 倒排,关键词秒搜),向量留待补 key 后再建;
    // - 两腿都不可用(FTS 未就绪 + 无 key)→ 才报错。
    let embed_ok = crate::sense::active_provider("embed").is_some();
    let lex_ok = lex_available(&conn); // P1-2:FTS5 就绪才走倒排,否则只做向量(实时扫描兜全文)
    if !embed_ok && !lex_ok {
        return Err("没有可用的嵌入服务商,且 FTS 倒排未就绪:在「设置 › 寓言计划 API」给硅基流动填 key(免费)以建向量;或重建数据库以启用全文倒排。".into());
    }
    let model = active_embed_model().unwrap_or_default(); // P2-2:落到每个 chunk 上做版本隔离
    let mut files_done = 0u64;
    let mut chunks_added = 0u64;
    let mut stopped = "全部完成".to_string();

    // pending 文件 = 还要建索引的文本文件。按可用的腿决定「待办」条件,避免选中标记不掉、空转:
    // - 两腿都在: chunked=0 OR ftsed=0
    // - 仅认字腿(无 key): ftsed=0  ← 不因 chunked=0 反复空转(等补 key 再嵌)
    // - 仅认意思腿(FTS 未就绪): chunked=0
    // dup_of=0:内容完全重复的副本(dedupe_scan 已删其 chunks 并指向 canonical)不再花嵌入钱。
    let pending_sql = match (embed_ok, lex_ok) {
        (true, true) => {
            "SELECT f.id, r.path, f.relpath, f.ext, f.size, f.chunked, f.ftsed
             FROM files f JOIN roots r ON r.id=f.root_id
             WHERE f.kind='text' AND f.size<=?1 AND f.dup_of=0 AND (f.chunked=0 OR f.ftsed=0)
             ORDER BY f.size ASC LIMIT 32"
        }
        (false, true) => {
            "SELECT f.id, r.path, f.relpath, f.ext, f.size, f.chunked, f.ftsed
             FROM files f JOIN roots r ON r.id=f.root_id
             WHERE f.kind='text' AND f.size<=?1 AND f.dup_of=0 AND f.ftsed=0
             ORDER BY f.size ASC LIMIT 32"
        }
        _ => {
            "SELECT f.id, r.path, f.relpath, f.ext, f.size, f.chunked, f.ftsed
             FROM files f JOIN roots r ON r.id=f.root_id
             WHERE f.kind='text' AND f.size<=?1 AND f.dup_of=0 AND f.chunked=0
             ORDER BY f.size ASC LIMIT 32"
        }
    };

    // 跨文件 chunk 缓冲(凑批只凑计算,落库仍按文件)。keys[i]=(file_id, seq) 与 buf_texts[i] 平行。
    // 攒够 embed_coalesce_target() 就 flush 一次;POLARIS_EMBED_COALESCE=0 时目标=1,退回逐文件。
    let coalesce_target = embed_coalesce_target();
    let mut buf_keys: Vec<(i64, i64)> = Vec::new();
    let mut buf_texts: Vec<String> = Vec::new();
    // 已进缓冲但尚未 flush 的 file_id:它们 chunked 仍为 0,会被 pending 查询再次选中——
    // 不排除会被重复读盘/切块/嵌入(小文件场景费用放大 ~3 倍,计数虚高、预算虚耗)。
    let mut buffered_ids: std::collections::HashSet<i64> = std::collections::HashSet::new();

    // 挂载僵死防护:整文件读在死 NAS 上会无限阻塞,且阻塞中的 syscall 连「取消」都拦不住
    // (cancelled() 只在循环边界轮询)→ 读走 with_deadline 有界旁路。某根一旦读超时即整根
    // 拉黑(其余待办零 IO 跳过、不再逐文件撞 20s),健康根照常构建。跳过的文件**不标记完成**
    // ——若标 chunked/ftsed=1,空文本落库,挂载恢复后 mtime 没变、重扫也不会重置,等于造出
    // 永久检索盲区;保持 0 让下轮「继续」自然重试。
    const READ_DEADLINE_SECS: u64 = 20;
    let mut skipped_reads: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut dead_read_roots: std::collections::HashSet<String> = std::collections::HashSet::new();
    'outer: loop {
        if cancelled() {
            stopped = "已取消".into();
            break;
        }
        if chunks_added >= max_chunks as u64 {
            stopped = format!("本轮预算({max_chunks} chunk)耗尽,可再点继续");
            break;
        }
        if files_done >= MAX_FILES_PER_BUILD {
            stopped = format!("本轮文件预算({MAX_FILES_PER_BUILD} 文件)耗尽,可再点继续");
            break;
        }
        // 小文件优先:先把海量小文档变可检索,大部头排后
        let batch: Vec<(i64, String, String, String, i64, i64, i64)> = {
            let mut stmt = conn.prepare(pending_sql).map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([MAX_LEX_FILE_BYTES], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, i64>(6)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            rows.flatten().collect()
        };
        // 剔除已在缓冲中的文件(见 buffered_ids 注释)。剔完为空但缓冲非空 = 待办只剩
        // 缓冲中的文件 → 强制 flush 让它们 chunked 置位,否则 pending 永远选中同一批死循环。
        let batch: Vec<_> = batch
            .into_iter()
            .filter(|row| !buffered_ids.contains(&row.0) && !skipped_reads.contains(&row.0))
            .collect();
        if batch.is_empty() {
            if cancelled() {
                stopped = "已取消".into();
                break; // 取消优先:缓冲丢弃(chunked 保持 0 下轮补),不再发起整批网络请求
            }
            if !buf_texts.is_empty() {
                match flush_embed_buffer(&conn, &buf_keys, &buf_texts, &model, embed_batch()) {
                    Ok(n) => chunks_added += n,
                    Err(e) => {
                        stopped = format!("嵌入中断(可再点继续补建向量):{e}");
                        buf_keys.clear();
                        buf_texts.clear();
                        break;
                    }
                }
                buf_keys.clear();
                buf_texts.clear();
                buffered_ids.clear();
                continue;
            }
            break;
        }
        // ── 先读后写(关键修):IO/切块全在事务外,事务只包纯内存 DB 写 ──
        // 旧实现 BEGIN 之后才逐个 std::fs::read + chunk_text:写锁横跨整批慢 IO/CPU
        // (死 NAS 单读 20s 死线、一批 32 个),盘点收尾/智能归类/审计等并行写者全被钉在
        // busy_timeout(20s)后面 —— 「建索引把整个应用拖卡」的数据层主根因。
        // 范式对齐 lexical.rs/dedupe.rs:Phase A 读盘+指纹+切块(零事务;嵌入 flush 自带
        // 短事务,此刻无批事务不必 COMMIT/BEGIN 腾挪),Phase B 单短事务批量落库(毫秒级)。
        // SQLite 写瓶颈在事务数(每次自动提交一次 fsync),每批仍是 1 个事务,吞吐不回退。
        // Phase B 中途 `?` 上抛时连接随函数退出回滚 —— 本批标记重做,幂等无害。
        struct Prep {
            file_id: i64,
            text: String,
            fingerprint: Option<(String, String)>,
            do_lex: bool,
            mark_chunked: bool,
        }
        let mut prepared: Vec<Prep> = Vec::with_capacity(batch.len());
        // 嵌入 flush 失败不立刻退出:本批已读好的轻量写(倒排/指纹)先经 Phase B 落库,
        // 不白读白切;stopped 已在失败点设好,落完即 break 'outer。
        let mut abort_after_flushfail = false;
        for (file_id, root, rel, ext, size, chunked, ftsed) in batch {
            if cancelled() || chunks_added >= max_chunks as u64 || files_done >= MAX_FILES_PER_BUILD
            {
                break;
            }
            // DB 里存的是 decode_fs 转出的 UTF-8 显示路径;真实 IO 必须 reencode 还原字节,
            // 否则 Unix 上 GBK 名文件读失败被当「已消失」,空文本落库成永久检索盲区。
            let abs = super::reencode_fs_path(
                &std::path::Path::new(&root).join(&rel).to_string_lossy(),
            );
            if dead_read_roots.contains(&root) {
                skipped_reads.insert(file_id);
                continue; // 该根已判僵死:零 IO 跳过,不标记完成
            }
            let read = {
                let p = abs.clone();
                // _bg:读在旁路线程上发生,优先级不继承 → 旁路自己降后台档,IO 让路前台。
                crate::fable::sched::with_deadline_bg(READ_DEADLINE_SECS, move || {
                    std::fs::read(&p).map_err(|e| e.kind())
                })
            };
            let text = match read {
                None => {
                    // 读超时(挂载僵死):跳过 + 整根拉黑,不标记完成(见上方大注释)。
                    skipped_reads.insert(file_id);
                    dead_read_roots.insert(root.clone());
                    continue;
                }
                Some(Ok(bytes)) => {
                    if bytes.iter().take(4096).any(|&b| b == 0) {
                        String::new() // 伪文本(二进制改名),跳过
                    } else {
                        String::from_utf8_lossy(&bytes).into_owned()
                    }
                }
                // 真消失(NotFound):标记完成,下轮重扫会清行。
                Some(Err(std::io::ErrorKind::NotFound)) => String::new(),
                // 瞬断/权限抖动:跳过不标记(标了=空文本落库且 mtime 不变永不重建,
                // 一次抖动就把该文件永久踢出检索),下轮重试。
                Some(Err(_)) => {
                    skipped_reads.insert(file_id);
                    continue;
                }
            };

            // ── 内容指纹 + 版本键(去重/新压旧/移动免重嵌的地基;事务外算好,Phase B 只写)──
            // 索引读文件时顺手算,零额外 IO;只对真文本算(空/伪文本留 ''=无指纹)。
            // 文件只在 chunked=0 或 ftsed=0 时才进本批,处理完不再入选 → 指纹不会重复回写。
            let fingerprint = if text.is_empty() {
                None
            } else {
                let name = std::path::Path::new(&rel)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Some((content_fingerprint(&text), doc_key(&name)))
            };

            // ── 向量层(认意思腿):切块 + 攒缓冲(CPU 在事务外);P1-4 只覆盖「精华」文本 ──
            // 无 key 时整块跳过:chunked 保持 0,补 key 后再点构建即补建向量(认字腿已先行覆盖)。
            let mut buffered = false;
            if embed_ok && chunked == 0 && embeddable(&ext, size) && !text.is_empty() {
                let mut chunks = chunk_text(&text);
                // 实验:给每个 chunk 前缀「【文件名 · 父目录】」上下文头(默认关,POLARIS_CHUNK_HEADER=1
                // 开)。同嵌入空间、不动 chunks.model → 只影响新建/重建的 chunk,存量随自然重建轮换,
                // 绝不触发全库重嵌。命中率须经 eval A/B 达标(nDCG/recall ≥+2pt)再考虑默认开。
                if chunk_header_enabled() {
                    let hdr = chunk_header_for(&rel);
                    if !hdr.is_empty() {
                        for c in chunks.iter_mut() {
                            *c = format!("{hdr}{c}");
                        }
                    }
                }
                if !chunks.is_empty() {
                    // 攒进跨文件缓冲(整文件的 chunk 一次性加入,seq 从 0 起,不跨 flush)。
                    for (i, c) in chunks.into_iter().enumerate() {
                        buf_keys.push((file_id, i as i64));
                        buf_texts.push(c);
                    }
                    buffered = true;
                    buffered_ids.insert(file_id);
                    // 攒够目标就 flush:嵌完落库,chunked=1 由 flush 内对涉及文件统一置位。
                    // flush 自带事务;Phase A 无批事务在身,无需 COMMIT/BEGIN 腾挪。
                    if buf_texts.len() >= coalesce_target {
                        match flush_embed_buffer(&conn, &buf_keys, &buf_texts, &model, embed_batch())
                        {
                            Ok(n) => {
                                chunks_added += n;
                                buffered_ids.clear(); // flush 成功已统一置 chunked=1,不再需要挡重选
                            }
                            Err(e) => {
                                // 整个 flush 放弃:涉及文件保持 chunked=0(旧 chunk 未动),下轮重试。
                                // 清空缓冲避免收尾 flush 再次撞同一错误。
                                stopped = format!("嵌入中断(可再点继续补建向量):{e}");
                                abort_after_flushfail = true;
                            }
                        }
                        buf_keys.clear();
                        buf_texts.clear();
                    }
                }
            }
            prepared.push(Prep {
                file_id,
                text,
                fingerprint,
                do_lex: lex_ok && ftsed == 0,
                // 不可嵌入 / 空文本 / 无 chunk:向量决策已完成,Phase B 标 chunked=1 防重复选中。
                // 已进缓冲的文件不标 —— 由 flush 成功后统一置位(失败则保持 0 重试)。
                mark_chunked: embed_ok && chunked == 0 && !buffered,
            });
            files_done += 1;
            progress(files_done, chunks_added, &rel);
            if abort_after_flushfail {
                break;
            }
        }
        // ── Phase B:单短事务批量落库(指纹/倒排/标记全是纯内存写,写锁毫秒级即释放)──
        // 预算/取消/flush 失败的内层 break 也走到这里:已读好的文件照常落库,提交多少算多少。
        conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
        for p in &prepared {
            if let Some((chash, dkey)) = &p.fingerprint {
                let _ = conn.execute(
                    "UPDATE files SET content_hash=?1, doc_key=?2 WHERE id=?3",
                    rusqlite::params![chash, dkey, p.file_id],
                );
            }
            // P1-2 全文倒排(认字腿):提前建好,查词秒回、覆盖全部文本文件。
            if p.do_lex {
                conn.execute("DELETE FROM lex WHERE rowid=?1", [p.file_id])
                    .map_err(|e| e.to_string())?;
                if !p.text.is_empty() {
                    conn.execute(
                        "INSERT INTO lex(rowid, body) VALUES(?1, ?2)",
                        rusqlite::params![p.file_id, p.text],
                    )
                    .map_err(|e| e.to_string())?;
                }
                conn.execute("UPDATE files SET ftsed=1 WHERE id=?1", [p.file_id])
                    .map_err(|e| e.to_string())?;
            }
            if p.mark_chunked {
                conn.execute("UPDATE files SET chunked=1 WHERE id=?1", [p.file_id])
                    .map_err(|e| e.to_string())?;
            }
        }
        conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
        if abort_after_flushfail {
            break 'outer;
        }
    }

    // ── 收尾:把残留缓冲一次性落库 ──
    // 正常/预算耗尽退出都要把攒着的 chunk 嵌完(否则这些文件白读白切,下轮重来)。
    // 取消时丢弃:涉及文件保持 chunked=0,下轮重扫自然补上,不浪费网络。
    if !cancelled() && !buf_texts.is_empty() {
        match flush_embed_buffer(&conn, &buf_keys, &buf_texts, &model, embed_batch()) {
            Ok(n) => chunks_added += n,
            Err(e) => {
                if stopped == "全部完成" {
                    stopped = format!("嵌入中断(可再点继续补建向量):{e}");
                }
            }
        }
    }

    // 剩余工作:与 pending_sql 同口径 —— 无 key 时只数还没进倒排(ftsed=0)的文件,
    // 别把「待补向量」(chunked=0)算成欠账,否则永远显示一堆 pending 误导用户。
    let pending_count_sql = match (embed_ok, lex_ok) {
        (true, true) => {
            "SELECT COUNT(*) FROM files WHERE kind='text' AND size<=?1 AND dup_of=0 AND (chunked=0 OR ftsed=0)"
        }
        (false, true) => "SELECT COUNT(*) FROM files WHERE kind='text' AND size<=?1 AND dup_of=0 AND ftsed=0",
        _ => "SELECT COUNT(*) FROM files WHERE kind='text' AND size<=?1 AND dup_of=0 AND chunked=0",
    };
    let files_pending: i64 = conn
        .query_row(pending_count_sql, [MAX_LEX_FILE_BYTES], |r| r.get(0))
        .unwrap_or(0);
    // 首次跨过规模门槛时自动建一次 IVF 倒排单元(20TB 级检索开箱即亚秒);未取消、有向量可聚类时才做。
    if !cancelled() && embed_ok {
        maybe_optimize(&conn, &model);
    }
    // 有根因读超时被整根拉黑 → 如实上报,别让「全部完成」掩盖半截结果。
    if !dead_read_roots.is_empty() && stopped == "全部完成" {
        stopped = "部分存储无响应(NAS 掉线?),其上文件已跳过 —— 恢复连接后再点继续".into();
    }
    Ok(IndexSummary {
        files_done,
        chunks_added,
        files_pending: files_pending as u64,
        seconds: started.elapsed().as_secs_f64(),
        stopped,
    })
}

/// 一次性把待办文本**全部**嵌入完:循环调用 [`build_index`] 直到 pending 清零 / 取消 / 卡住。
/// 文件中心 v3「后台精修」(T2)用 —— 向量化全库后再做一次真·语义归类。
/// 与普通索引构建共用 `INDEXING` 闸(进行中则拒绝),RAII 守卫保证 panic 栈展开也释放。
/// `progress(files_done_total, chunks_added_total, files_pending)`:每消化一个文件 + 每轮末各回调。
pub fn build_index_full(progress: &dyn Fn(u64, u64, u64)) -> Result<IndexSummary, String> {
    let Some(_guard) = FlagGuard::acquire(&INDEXING) else {
        return Err("索引构建已在进行中(稍后会自动续上)".into());
    };
    CANCEL.store(false, Ordering::SeqCst);
    let started = std::time::Instant::now();
    let mut total_files = 0u64;
    let mut total_chunks = 0u64;
    let mut last_pending = u64::MAX;
    let mut stalled = 0u32;
    let stopped = loop {
        // 大预算单轮:尽量多消化、少轮次开销;MAX_FILES_PER_BUILD 仍在内部封顶单轮文件数。
        let s = build_index(200_000, &|f, c, _cur| {
            progress(total_files + f, total_chunks + c, 0)
        })?;
        total_files += s.files_done;
        total_chunks += s.chunks_added;
        progress(total_files, total_chunks, s.files_pending);
        if cancelled() {
            break "已取消".to_string();
        }
        if s.files_pending == 0 {
            break "全部完成".to_string();
        }
        // 防空转:连续两轮 pending 不再下降(剩的都是超限大文件/不可读)→ 收工,别死循环。
        if s.files_done == 0 || s.files_pending >= last_pending {
            stalled += 1;
            if stalled >= 2 {
                break format!(
                    "剩 {} 个文件无法嵌入(超限/不可读),已尽力完成",
                    s.files_pending
                );
            }
        } else {
            stalled = 0;
        }
        last_pending = s.files_pending;
    };
    // 全量索引收尾自动去重/新压旧:指纹刚算齐是最佳时机,让「新压旧」无需用户手动触发即生效。
    // 纯 SQL 分组打标 + 幂等,大库秒级;best-effort(失败不影响索引结果),取消时跳过。
    if !cancelled() {
        let _ = dedupe_scan(false);
    }
    Ok(IndexSummary {
        files_done: total_files,
        chunks_added: total_chunks,
        files_pending: 0,
        seconds: started.elapsed().as_secs_f64(),
        stopped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_fingerprint_is_stable_and_content_sensitive() {
        let a = content_fingerprint("同一份内容");
        let b = content_fingerprint("同一份内容");
        let c = content_fingerprint("换了内容");
        assert_eq!(a, b, "相同内容指纹必须一致");
        assert_ne!(a, c, "不同内容指纹必须不同");
        assert!(a.starts_with("f:"), "全文哈希前缀 f:");
        assert_eq!(a.len(), 2 + 32, "f: + 32 hex");
    }

    #[test]
    fn chunk_header_builds_bracketed_context() {
        let h = chunk_header_for("项目/合同/2023年度报告.docx");
        assert!(h.starts_with("【") && h.ends_with("】\n"));
        assert!(h.contains("2023年度报告.docx"));
        assert!(h.contains("合同"), "含父目录名");
        // 顶层文件无父目录段,只放文件名
        let top = chunk_header_for("readme.txt");
        assert!(top.contains("readme.txt") && !top.contains(" · "));
    }

    /// e2e(**单进程跑**:`cargo test -p polaris-fable --lib build_index_lex_only_e2e -- --ignored`;
    /// 与其它测试同进程会撞 MIGRATED 单次迁移闸/环境变量):纯词法路径(测试进程 STORE 为空,
    /// 无嵌入服务商)的 build_index 全链路 + 「先读后写」重构回归闸 ——
    /// ① 正文进 FTS 倒排、指纹/doc_key 回填、ftsed/chunked 置位、pending 清零;
    /// ② 真消失文件(NotFound)标记完成不留死待办;伪二进制(含 \0)不进倒排;
    /// ③ 二次调用幂等 no-op;
    /// ④ 构建期间并发写者(模拟盘点/归类)持续小事务写,最大等锁时长必须远小于
    ///    busy_timeout(20s)—— 写锁不再被文件 IO 钉住,「建索引拖垮全应用」的回归哨兵。
    #[test]
    #[ignore]
    fn build_index_lex_only_e2e() {
        let base = std::env::temp_dir().join(format!("polaris_bidx_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root_dir = base.join("root");
        std::fs::create_dir_all(&root_dir).unwrap();
        std::env::set_var("POLARIS_FABLE_DB", base.join("fable_test.db"));

        // 盘面:600 个真文本(内容含可检索关键词)+ 1 个伪二进制 + 1 个库里有行、盘上没有的文件
        const N_TEXT: i64 = 600;
        for i in 0..N_TEXT {
            std::fs::write(
                root_dir.join(format!("doc{i}.txt")),
                format!("北极星知识库测试文档 {i}:先读后写重构验证正文。"),
            )
            .unwrap();
        }
        std::fs::write(base.join("root").join("fake.txt"), b"PK\x00\x01binary").unwrap();

        let conn = open_db().unwrap();
        conn.execute(
            "INSERT INTO roots(id, path) VALUES(1, ?1)",
            [root_dir.to_string_lossy().to_string()],
        )
        .unwrap();
        let ins = |id: i64, rel: &str| {
            conn.execute(
                "INSERT INTO files(id, root_id, relpath, name, ext, kind, size, mtime, chunked, ftsed, seen)
                 VALUES(?1, 1, ?2, ?2, 'txt', 'text', 64, 1000, 0, 0, 1)",
                rusqlite::params![id, rel],
            )
            .unwrap();
        };
        for i in 0..N_TEXT {
            ins(i + 1, &format!("doc{i}.txt"));
        }
        ins(N_TEXT + 1, "fake.txt");
        ins(N_TEXT + 2, "missing.txt"); // 盘上不存在 → NotFound → 标记完成
        drop(conn);

        // 并发写者:构建全程每 10ms 一笔小事务写(模拟盘点收尾/归类落库),测最大等锁时长。
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let c = open_db().unwrap();
                let mut max_ms = 0u128;
                let mut errors = 0u32;
                let mut i = 0i64;
                while !stop.load(Ordering::SeqCst) {
                    let t = std::time::Instant::now();
                    let r: Result<(), rusqlite::Error> = (|| {
                        c.execute_batch("BEGIN IMMEDIATE")?;
                        c.execute(
                            "UPDATE files SET seen=1 WHERE id=?1",
                            [(i % N_TEXT) + 1],
                        )?;
                        c.execute_batch("COMMIT")?;
                        Ok(())
                    })();
                    if r.is_err() {
                        let _ = c.execute_batch("ROLLBACK");
                        errors += 1;
                    }
                    max_ms = max_ms.max(t.elapsed().as_millis());
                    i += 1;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                (max_ms, errors)
            })
        };

        let sum = build_index(200_000, &|_, _, _| {}).expect("build_index");
        stop.store(true, Ordering::SeqCst);
        let (max_ms, errors) = writer.join().unwrap();

        // ① 全部消化:602 个待办全处理,pending 清零,自然收尾。
        assert_eq!(sum.files_done as i64, N_TEXT + 2);
        assert_eq!(sum.files_pending, 0);
        assert_eq!(sum.stopped, "全部完成");
        let conn = open_db().unwrap();
        let lex_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM lex", [], |r| r.get(0))
            .unwrap();
        assert_eq!(lex_rows, N_TEXT, "只有真文本进倒排(伪二进制/消失文件不进)");
        // trigram 分词:MATCH 词须 ≥3 字符(见 mod.rs 的 lex 建表注释)。
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM lex WHERE lex MATCH '重构验证'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, N_TEXT, "关键词可检索到全部真文本");
        let fp_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE content_hash LIKE 'f:%' AND doc_key<>''",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fp_rows, N_TEXT, "真文本全部回填内容指纹与版本键");
        let ftsed_all: i64 = conn
            .query_row("SELECT COUNT(*) FROM files WHERE ftsed=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ftsed_all, N_TEXT + 2, "伪二进制/消失文件也标记完成,不留死待办");
        drop(conn);

        // ③ 幂等:再跑一遍应 no-op。
        let again = build_index(200_000, &|_, _, _| {}).expect("build_index again");
        assert_eq!(again.files_done, 0, "二次调用应无待办");

        // ④ 并发写者全程零锁错误、最大等锁远小于 busy_timeout —— 写锁没被 IO 钉住。
        assert_eq!(errors, 0, "构建期间并发小写者不应撞锁失败");
        assert!(
            max_ms < 5_000,
            "并发写者最大等锁 {max_ms}ms,应远小于 busy_timeout(20s)"
        );

        std::env::remove_var("POLARIS_FABLE_DB");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn doc_key_folds_version_noise() {
        // 同一份资料的不同版本 → 同 key
        let base = doc_key("季度报告.docx");
        assert_eq!(doc_key("季度报告 v2.docx"), base);
        assert_eq!(doc_key("季度报告 final.docx"), base);
        assert_eq!(doc_key("季度报告(1).docx"), base);
        assert_eq!(doc_key("季度报告_副本.docx"), base);
        assert_eq!(
            doc_key("季度报告-20230101.pdf"),
            base,
            "扩展名不进 key,日期被剪"
        );
        // 不同资料 → 不同 key
        assert_ne!(doc_key("季度报告.docx"), doc_key("年度预算.docx"));
        // 年份是有意义的 4 位数字,不当副本序号剪掉
        assert_ne!(doc_key("报告 2023.docx"), doc_key("报告 2024.docx"));
        // 文件名本身就是噪声时不塌成空 key
        assert!(!doc_key("v2.docx").is_empty());
    }
}
