//! grep 车道:倒排(FTS5)+ 短词 LIKE + 磁盘兜底子串扫,多核回读候选精确算分抽行。

use super::*;

/// grep 车道单文件上限(超大文本不参与全文扫,靠 agent 的定向 Grep 工具)。
const MAX_GREP_FILE_BYTES: i64 = 4_000_000;
/// grep 车道单次检索的文件数/总字节预算(实时扫描兜底路才用;FTS 倒排路无此上限)。
const MAX_GREP_FILES: i64 = 20_000;
const MAX_GREP_TOTAL_BYTES: u64 = 800 * 1024 * 1024;
/// FTS 倒排命中后,最多回读多少个候选文件做精确算分 + 抽行(按 bm25 相关度优先)。
const FTS_CAND_LIMIT: i64 = 400;
/// 短词补召(≤2 码点,只能走无索引的前置通配 LIKE 全表子串扫)的墙钟预算(秒)。
/// 稀有短词会扫完整张 lex(数十万文档正文)达数十秒 → 到点中断、以部分候选降级,
/// 把「单条短词查询拖垮整个 server」的历史超时雪崩压成有界代价。
const SHORT_LIKE_BUDGET_SECS: u64 = 6;

pub(crate) struct GrepHit {
    pub(crate) path: String,
    pub(crate) abspath: String,
    pub(crate) line: usize,
    pub(crate) snippet: String,
    /// 命中行 ± 邻近若干行的上下文窗口(只给重排「读全文打分」用,不展示)。
    pub(crate) context: String,
    pub(crate) score: f32,
}

/// 多核回读候选文件 → 精确算分 + 抽命中行/上下文窗口。FTS 路与实时扫描路共用此算分口径。
/// `byte_budget=Some(n)` 时(实时扫描)按字节预算截断并回报 truncated;`None`(FTS 路)不截断。
fn scan_and_score(
    candidates: Vec<(String, String, i64)>,
    q_full: &str,
    tokens: &[String],
    byte_budget: Option<u64>,
) -> (Vec<GrepHit>, bool) {
    let stack = Mutex::new(candidates);
    let hits = Mutex::new(Vec::<GrepHit>::new());
    let spent = AtomicU64::new(0);
    let truncated = std::sync::atomic::AtomicBool::new(false);
    // 已判僵死的远程根(全池共享):候选文件散在 NAS 上时,一个僵死挂载会让 worker 卡在
    // read 上永不返回 → thread::scope join 不了 → 整个同步 search() 冻死。远程根的读走
    // 有界旁路,超时即整根拉黑;本地盘照旧直读(零额外开销)。
    let dead_roots = Mutex::new(std::collections::HashSet::<String>::new());

    std::thread::scope(|s| {
        for _ in 0..worker_count() {
            let (stack, hits, spent, truncated) = (&stack, &hits, &spent, &truncated);
            let (q_full, tokens) = (&q_full, &tokens);
            let dead_roots = &dead_roots;
            s.spawn(move || loop {
                let item = { stack.lock().unwrap().pop() };
                let Some((root, rel, size)) = item else { break };
                if let Some(budget) = byte_budget {
                    if spent.fetch_add(size as u64, Ordering::Relaxed) > budget {
                        truncated.store(true, Ordering::Relaxed);
                        break;
                    }
                }
                // 显示路径 → 真实字节路径(GBK 名文件在 Unix 上直接 read 恒失败,
                // 实时扫描会静默跳过它们)。IO 用还原路径;**返回给上层的 abspath 必须
                // 保留 UTF-8 显示路径**——对还原路径再 to_string_lossy 会把 GBK 字节
                // 打成 �,前端打开/agent 取证/二次 reencode 全都还原不回真实文件。
                let abs_display = std::path::Path::new(&root)
                    .join(&rel)
                    .to_string_lossy()
                    .into_owned();
                let abs = crate::fable::reencode_fs_path(&abs_display);
                let bytes = if crate::fable::inventory::is_remote_root(&root) {
                    if dead_roots.lock().unwrap().contains(&root) {
                        continue; // 该根已判僵死:零 IO 跳过
                    }
                    let p = abs.clone();
                    match crate::fable::sched::with_deadline(8, move || std::fs::read(&p).ok()) {
                        None => {
                            dead_roots.lock().unwrap().insert(root.clone());
                            continue; // 读超时:整根拉黑,搜索绝不因死 NAS 冻死
                        }
                        Some(None) => continue,
                        Some(Some(b)) => b,
                    }
                } else {
                    match std::fs::read(&abs) {
                        Ok(b) => b,
                        Err(_) => continue,
                    }
                };
                if bytes.iter().take(4096).any(|&b| b == 0) {
                    continue; // 二进制伪文本
                }
                let text = String::from_utf8_lossy(&bytes);
                let lower = text.to_lowercase();
                let mut score = 0f32;
                if lower.contains(*q_full) {
                    score += 3.0;
                }
                for t in tokens.iter() {
                    if lower.contains(t.as_str()) {
                        score += 1.0;
                    }
                }
                if score <= 0.0 {
                    continue;
                }
                let lines: Vec<&str> = text.lines().collect();
                // 复用整文件的小写副本 `lower`(上面已算)逐行切片做命中判定,免去每行再
                // `to_lowercase()` 分配一个 String(大文件几千行 × 数百候选 = 数百万次分配)。
                // `to_lowercase` 不增删换行符,故 `lower.lines()` 与 `text.lines()` 行号对齐;
                // 仍用 `.get(i)` 防御任何极端不齐。展示/上下文取原文 `lines`(保留大小写)。
                let lower_lines: Vec<&str> = lower.lines().collect();
                // 取最多 2 条命中行做摘录(行号按原文);并截一段上下文窗口给重排读全文。
                let mut snippets = 0;
                for (i, line) in lines.iter().enumerate() {
                    let ll = lower_lines.get(i).copied().unwrap_or("");
                    let hit_full = ll.contains(*q_full);
                    let hit_tok = tokens.iter().any(|t| ll.contains(t.as_str()));
                    if hit_full || hit_tok {
                        let snippet: String = line.trim().chars().take(160).collect();
                        // 命中行 ±2 行拼成上下文窗口(P2-1:让重排专家读到的不只是孤零零一行)。
                        let lo = i.saturating_sub(2);
                        let hi = (i + 3).min(lines.len());
                        let context: String = lines[lo..hi].join("\n").chars().take(700).collect();
                        hits.lock().unwrap().push(GrepHit {
                            path: rel.clone(),
                            abspath: abs_display.clone(),
                            line: i + 1,
                            snippet,
                            context,
                            score: score + if hit_full { 0.5 } else { 0.0 },
                        });
                        snippets += 1;
                        if snippets >= 2 {
                            break;
                        }
                    }
                }
            });
        }
    });

    let mut out = hits.into_inner().unwrap();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(60);
    (out, truncated.load(Ordering::Relaxed))
}

/// 认字腿(P1-2 + CJK 修):两路候选合并 ——
/// - **倒排路**(快、可扩展):FTS5 trigram 取 ≥3 码点检索词的候选;自然句靠三元组 OR 也有候选。
/// - **实时扫描路**(子串、覆盖未索引文件):仅当查询含 trigram 服务不了的短词(独立 1~2 字
///   CJK 概念词、2 字拉丁词,如「模型 索引」)、或倒排无候选时才补扫,带字节预算护栏。
///
/// 旧实现的致命缺陷:整句当一个 FTS 短语 → 「模型 索引」「检索 重排」「<整句自然语言>」全部零召回
/// (实测 0 命中)。现在 split_query 把 CJK 句切成概念词、fts_query_expr 用三元组 OR 取候选,
/// scan_and_score 按概念词子串算分,自然句/双关键词都能命中。
pub(crate) fn grep_lane(query: &str) -> Result<(Vec<GrepHit>, bool), String> {
    let (q_full, terms) = split_query(query);
    if q_full.is_empty() {
        return Ok((Vec::new(), false));
    }
    let conn = open_db()?;

    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut fts_cand: Vec<(String, String, i64)> = Vec::new();
    let lex_ok = lex_available(&conn);

    // —— 倒排路 ——
    if lex_ok {
        if let Some(expr) = fts_query_expr(&q_full) {
            let mut stmt = conn
                .prepare(
                    "SELECT r.path, f.relpath, f.size FROM lex l
                     JOIN files f ON f.id=l.rowid JOIN roots r ON r.id=f.root_id
                     WHERE l.body MATCH ?1 ORDER BY bm25(lex) LIMIT ?2",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(rusqlite::params![expr, FTS_CAND_LIMIT], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            for row in rows.flatten() {
                if seen.insert((row.0.clone(), row.1.clone())) {
                    fts_cand.push(row);
                }
            }
        }
    }

    // 纯标点/纯空白等「无内容词」查询:倒排也没候选 → 直接收工,别空扫一整轮磁盘(实测省 ~1s)。
    if terms.is_empty() && fts_cand.is_empty() {
        return Ok((Vec::new(), false));
    }

    // —— 短词补召(trigram 服务不了的 ≤2 码点概念词,如「模型」「索引」)——
    // 优先 lex LIKE:读 DB 页(OS 缓存,快)、覆盖**全部**已索引文件,命中后只回读这几百个文件;
    // 比扫 2 万个磁盘文件快一两个数量级(实测 ~1s → 数十 ms)。
    let mut like_cand: Vec<(String, String, i64)> = Vec::new();
    let mut short_truncated = false;
    if lex_ok && has_short_terms(&q_full) {
        let shorts: Vec<&String> = terms
            .iter()
            .filter(|t| t.chars().count() <= 2)
            .take(8)
            .collect();
        // 命中多的短词很快撞满 LIMIT 就停;稀有短词会扫完整张 lex(数十万文档正文)达数十秒。
        // 挂一个墙钟预算:另起计时线程,到点 conn.interrupt() 中断当前扫描(SQLITE_INTERRUPT →
        // query_map 迭代随即停止,已取到的即部分候选),把最坏代价从「拖垮整个 server」压成
        // 「有界降级、少量漏召」。sqlite3_interrupt 只影响其返回前正在跑的语句 —— 故务必在跑
        // 后续磁盘兜底查询**之前** join 掉计时线程,确保中断不会误伤本连接的下一条查询。
        // 长词 FTS(有索引,<1s)不受影响。
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(SHORT_LIKE_BUDGET_SECS);
        let handle = conn.get_interrupt_handle();
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done_timer = done.clone();
        let fired_timer = fired.clone();
        let timer = std::thread::spawn(move || {
            while std::time::Instant::now() < deadline {
                if done_timer.load(Ordering::Relaxed) {
                    return; // 扫描提前完成,不必等满预算,也不触发中断
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            handle.interrupt();
            fired_timer.store(true, Ordering::Relaxed); // 真正打了中断 → 结果可能不全
        });
        {
            let mut stmt = conn
                .prepare(
                    "SELECT r.path, f.relpath, f.size FROM lex l
                     JOIN files f ON f.id=l.rowid JOIN roots r ON r.id=f.root_id
                     WHERE l.body LIKE ?1 LIMIT ?2",
                )
                .map_err(|e| e.to_string())?;
            for t in shorts {
                if std::time::Instant::now() >= deadline {
                    short_truncated = true;
                    break;
                }
                // 检索词只含 CJK / 字母数字(atoms 已过滤),不含 LIKE 元字符,直接拼 %term%。
                let pat = format!("%{t}%");
                let rows = match stmt.query_map(rusqlite::params![pat, FTS_CAND_LIMIT], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                }) {
                    Ok(rows) => rows,
                    // 中断(超预算)即当作部分候选收工,不把 SQLITE_INTERRUPT 冒泡成整条检索失败。
                    Err(_) => {
                        short_truncated = true;
                        break;
                    }
                };
                for row in rows.flatten() {
                    if seen.insert((row.0.clone(), row.1.clone())) {
                        like_cand.push(row);
                    }
                }
            }
        }
        // 通知计时线程收工并 join:保证 interrupt() 要么没发生、要么已完全返回,
        // 后续磁盘兜底查询绝不会被这次中断波及。
        done.store(true, Ordering::Relaxed);
        let _ = timer.join();
        // 计时线程真打了中断(哪怕是在 query_map 迭代中途)→ 短词候选不全,标记降级。
        if fired.load(Ordering::Relaxed) {
            short_truncated = true;
        }
    }

    // —— 磁盘兜底(有界子串扫盘)——
    // 仅当倒排 + LIKE 都没候选(目标可能落在**未索引**文件里),或 lex 整个没就绪时才扫;
    // 命中索引时绝不触发,把昂贵的全盘扫描留给真正必要的场景。
    let need_disk = !lex_ok || (fts_cand.is_empty() && like_cand.is_empty());
    let scan_cand: Vec<(String, String, i64)> = if need_disk {
        let mut stmt = conn
            .prepare(
                "SELECT r.path, f.relpath, f.size FROM files f JOIN roots r ON r.id=f.root_id
                 WHERE f.kind='text' AND f.size<=?1 ORDER BY f.size ASC LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([MAX_GREP_FILE_BYTES, MAX_GREP_FILES], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.flatten()
            .filter(|row| seen.insert((row.0.clone(), row.1.clone())))
            .collect()
    } else {
        Vec::new()
    };
    drop(conn);

    // 倒排 / LIKE 候选已有界(≤ 几百)→ 无预算、不截断;磁盘兜底候选带字节预算护栏。
    let mut hits = Vec::new();
    let mut truncated = false;
    let indexed_cand: Vec<(String, String, i64)> = fts_cand.into_iter().chain(like_cand).collect();
    if !indexed_cand.is_empty() {
        let (h, _) = scan_and_score(indexed_cand, &q_full, &terms, None);
        hits.extend(h);
    }
    if !scan_cand.is_empty() {
        let (h, t) = scan_and_score(scan_cand, &q_full, &terms, Some(MAX_GREP_TOTAL_BYTES));
        hits.extend(h);
        truncated = t;
    }
    // 两路命中合并后重排、截断(各路内部已 ≤60,合并后再收一次)。
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(60);
    // 短词补召被预算截断也算「结果可能不全」,并入 truncated 让上层/前端可提示降级。
    Ok((hits, truncated || short_truncated))
}
