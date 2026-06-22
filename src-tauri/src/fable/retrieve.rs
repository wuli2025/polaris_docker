//! 塌平混检(神经层)—— grep 车道 ∥ 向量车道 并行 → RRF 融合 → 重排。
//!
//! PRD v5 §1「神经:四 tier 塌平混检」+ 用户拍板「grep 搜索和 RAG 并行,CPU 还很多」:
//! - **grep 车道**:多核 work-stealing 扫盘点表里的文本文件(字面/分词命中,零依赖
//!   零索引延迟 —— 盘点完成那一刻起就能搜,这就是 L1a「首小时全盘可搜」的搜);
//! - **向量车道**:查询嵌入 → 流式暴力余弦(SQLite 顺序读 vec BLOB,十万级亚秒;
//!   千万级在此函数内换 ANN,签名不变);
//! - 两车道 `thread::scope` 真并行,先到先等,RRF(k=60)塌平融合;
//! - 有重排服务商时对融合 top-40 精排一次,失败静默保持 RRF 序(可降级)。

use super::{lex_available, open_db, open_db_gauged, worker_count};
use once_cell::sync::Lazy;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// grep 车道单文件上限(超大文本不参与全文扫,靠 agent 的定向 Grep 工具)。
const MAX_GREP_FILE_BYTES: i64 = 4_000_000;
/// grep 车道单次检索的文件数/总字节预算(实时扫描兜底路才用;FTS 倒排路无此上限)。
const MAX_GREP_FILES: i64 = 20_000;
const MAX_GREP_TOTAL_BYTES: u64 = 800 * 1024 * 1024;
/// FTS 倒排命中后,最多回读多少个候选文件做精确算分 + 抽行(按 bm25 相关度优先)。
const FTS_CAND_LIMIT: i64 = 400;
/// 重排候选窗口 N(融合后取前 N 精排;详解第 6 节「甜点区」)。
const RERANK_N: usize = 40;

// ───────────────────────── 结果模型 ─────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct FableHit {
    /// 相对盘点根的路径
    pub path: String,
    pub abspath: String,
    /// "L42" 行号 或 "C3" chunk 序号
    pub location: String,
    pub snippet: String,
    pub score: f32,
    /// 命中车道: grep / vector(融合后可能两者都有)
    pub lanes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FableSearchResult {
    pub query: String,
    pub mode: String,
    pub hits: Vec<FableHit>,
    pub grep_hits: usize,
    pub vector_hits: usize,
    pub reranked: bool,
    /// grep 车道是否因预算截断(命中可能不全,建议 agent 换更窄的定向 Grep)
    pub grep_truncated: bool,
    pub ms: u64,
}

// ───────────────────────── grep 车道 ─────────────────────────

struct GrepHit {
    path: String,
    abspath: String,
    line: usize,
    snippet: String,
    /// 命中行 ± 邻近若干行的上下文窗口(只给重排「读全文打分」用,不展示)。
    context: String,
    score: f32,
}

/// CJK(中日韩)表意文字判断 —— 这些字之间没有空格,必须自行切词。
fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{3400}'..='\u{4DBF}'   // 扩展 A
        | '\u{4E00}'..='\u{9FFF}' // 基本汉字
        | '\u{F900}'..='\u{FAFF}' // 兼容表意
        | '\u{3040}'..='\u{30FF}' // 日文假名
    )
}

/// CJK 功能词/填充词(2 字),作检索词无区分度 —— 从二元组里剔除以降噪(自然句里满是这种)。
const CJK_STOP: &[&str] = &[
    "我想", "想了", "了解", "怎么", "么做", "是怎", "做的", "一下", "知道", "什么", "这个", "那个",
    "可以", "因为", "所以", "但是", "如果", "就是", "没有", "已经", "这样", "一个", "一些", "现在",
    "时候", "出来", "起来", "相关", "资料", "的话", "进行", "通过", "对于", "以及", "或者", "还是",
    "为了", "需要", "应该", "如何", "请问", "帮我", "告诉",
];

/// 把查询切成原子:`(拉丁/数字词, CJK 连续段)`。空白与标点都作分隔符。
fn atoms(query: &str) -> (Vec<String>, Vec<String>) {
    let mut latin = Vec::new();
    let mut runs = Vec::new();
    let mut cur_latin = String::new();
    let mut cur_cjk = String::new();
    for ch in query.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            if !cur_cjk.is_empty() {
                runs.push(std::mem::take(&mut cur_cjk));
            }
            cur_latin.push(ch);
        } else if is_cjk(ch) {
            if !cur_latin.is_empty() {
                latin.push(std::mem::take(&mut cur_latin));
            }
            cur_cjk.push(ch);
        } else {
            if !cur_latin.is_empty() {
                latin.push(std::mem::take(&mut cur_latin));
            }
            if !cur_cjk.is_empty() {
                runs.push(std::mem::take(&mut cur_cjk));
            }
        }
    }
    if !cur_latin.is_empty() {
        latin.push(cur_latin);
    }
    if !cur_cjk.is_empty() {
        runs.push(cur_cjk);
    }
    (latin, runs)
}

/// 把查询拆成「全句 + 内容词」。**内容词 = 拉丁词(≥2)+ CJK 重叠二元组(滤功能词)+ 单字 CJK**。
/// 关键改动:CJK 自然句不再当一个大短语 —— 「我想了解模型索引」会切出 `模型`/`索引` 等概念词,
/// 这样子串算分(scan_and_score)能逐概念命中,自然句不再零召回。
pub(crate) fn split_query(query: &str) -> (String, Vec<String>) {
    let q_full = query.trim().to_lowercase();
    let (latin, runs) = atoms(&q_full);
    let mut terms: Vec<String> = Vec::new();
    for w in latin {
        if w.chars().count() >= 2 {
            terms.push(w);
        }
    }
    for run in runs {
        let chars: Vec<char> = run.chars().collect();
        if chars.len() == 1 {
            terms.push(run);
        } else {
            for w in chars.windows(2) {
                let bg: String = w.iter().collect();
                if !CJK_STOP.contains(&bg.as_str()) {
                    terms.push(bg);
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    terms.retain(|t| seen.insert(t.clone()));
    terms.truncate(40);
    (q_full, terms)
}

/// FTS5(trigram)只能命中 ≥3 个码点的项。从查询里取**可被 trigram 服务**的检索词:
/// 拉丁词(≥3)+ 每个 CJK 段(≥3 字)的重叠三元组。OR 拼接(非整句短语,故自然句也有候选)。
/// 返回 None 表示没有 ≥3 项 → 调用方靠实时子串扫描兜底。
fn fts_query_expr(query: &str) -> Option<String> {
    let esc = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let (latin, runs) = atoms(query);
    let mut terms: Vec<String> = Vec::new();
    for w in latin {
        if w.chars().count() >= 3 {
            terms.push(w);
        }
    }
    for run in runs {
        let chars: Vec<char> = run.chars().collect();
        if chars.len() >= 3 {
            for w in chars.windows(3) {
                terms.push(w.iter().collect());
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    terms.retain(|t| seen.insert(t.clone()));
    terms.truncate(60);
    if terms.is_empty() {
        None
    } else {
        Some(
            terms
                .iter()
                .map(|t| esc(t))
                .collect::<Vec<_>>()
                .join(" OR "),
        )
    }
}

/// 查询里是否含 trigram **无法**索引的短概念词(独立 1~2 字 CJK 段、或 2 字拉丁词)。
/// 有则补一趟实时子串扫描(覆盖 2 字中文关键词 + 未进倒排的文件);没有则纯走快的倒排路。
fn has_short_terms(query: &str) -> bool {
    let (latin, runs) = atoms(query);
    latin.iter().any(|w| w.chars().count() == 2)
        || runs.iter().any(|r| {
            let n = r.chars().count();
            n == 1 || n == 2
        })
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

    std::thread::scope(|s| {
        for _ in 0..worker_count() {
            let (stack, hits, spent, truncated) = (&stack, &hits, &spent, &truncated);
            let (q_full, tokens) = (&q_full, &tokens);
            s.spawn(move || loop {
                let item = { stack.lock().unwrap().pop() };
                let Some((root, rel, size)) = item else { break };
                if let Some(budget) = byte_budget {
                    if spent.fetch_add(size as u64, Ordering::Relaxed) > budget {
                        truncated.store(true, Ordering::Relaxed);
                        break;
                    }
                }
                let abs = std::path::Path::new(&root).join(&rel);
                let Ok(bytes) = std::fs::read(&abs) else {
                    continue;
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
                            abspath: abs.to_string_lossy().into_owned(),
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
fn grep_lane(query: &str) -> Result<(Vec<GrepHit>, bool), String> {
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
    if lex_ok && has_short_terms(&q_full) {
        let shorts: Vec<&String> = terms
            .iter()
            .filter(|t| t.chars().count() <= 2)
            .take(8)
            .collect();
        let mut stmt = conn
            .prepare(
                "SELECT r.path, f.relpath, f.size FROM lex l
                 JOIN files f ON f.id=l.rowid JOIN roots r ON r.id=f.root_id
                 WHERE l.body LIKE ?1 LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        for t in shorts {
            // 检索词只含 CJK / 字母数字(atoms 已过滤),不含 LIKE 元字符,直接拼 %term%。
            let pat = format!("%{t}%");
            let rows = stmt
                .query_map(rusqlite::params![pat, FTS_CAND_LIMIT], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            for row in rows.flatten() {
                if seen.insert((row.0.clone(), row.1.clone())) {
                    like_cand.push(row);
                }
            }
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
    Ok((hits, truncated))
}

// ───────────────────────── 向量车道 ─────────────────────────

struct VecHit {
    path: String,
    abspath: String,
    seq: i64,
    text: String,
    score: f32,
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

fn vector_lane(query: &str, top_k: usize) -> Result<Vec<VecHit>, String> {
    // P1-5:查询嵌入走 LRU 缓存(已归一化);断网/限速时上抛,search() 静默降级保 grep/FTS 腿。
    let qv = super::index::embed_query(query)?;
    let model = super::index::active_embed_model().unwrap_or_default();
    let qbits = super::index::bits_of(&qv);
    let want = (top_k * 2).max(1);

    let conn = open_db()?;

    // ── IVF 探针(20TB ANN):若该模型已建倒排单元,先在质心里找最近的 nprobe 个 cell,
    //    第一段只在这些 cell(+cell=-1 的未分配新数据)里粗筛,把全表 O(N) 扫降到
    //    ~O(N·nprobe/cells);未建 cell 时 probes 为空 → 退回全表扫(零回归)。 ──
    let probes: Vec<i64> = {
        let mut stmt = conn
            .prepare("SELECT id, bits FROM vec_cells WHERE model=?1 AND dim=?2")
            .map_err(|e| e.to_string())?;
        let cells: Vec<(i64, Vec<u8>)> = stmt
            .query_map(rusqlite::params![model, qv.len() as i64], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
            })
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        if cells.is_empty() {
            Vec::new()
        } else {
            // nprobe ≈ √cells,夹在 [8,64]:扫约 nprobe/cells 比例的向量(K=5500 时约 1%)。
            let nprobe = ((cells.len() as f64).sqrt() as usize).clamp(8, 64);
            let mut scored: Vec<(u32, i64)> = cells
                .iter()
                .filter(|(_, b)| b.len() == qbits.len())
                .map(|(id, b)| (super::index::hamming(&qbits, b), *id))
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
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
        });
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
                    cand.sort_by(|a, b| {
                        b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
                    });
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

// ───────────────────────── 融合 + 重排 ─────────────────────────

/// 相对路径是否落在 scope 内(scope 按盘点根相对路径的**首段**匹配,大小写不敏感):
/// - `None` / 空 → 全盘(零回归);
/// - `Some("wiki")` → 仅首段为 wiki 的命中(妈妈库子树);
/// - `Some("!wiki")` → 仅首段**不是** wiki 的命中(「外面整个库」= raw/output/memory…)。
fn path_in_scope(path: &str, scope: Option<&str>) -> bool {
    let scope = match scope {
        None => return true,
        Some(s) if s.trim().is_empty() => return true,
        Some(s) => s.trim(),
    };
    let p = path.replace('\\', "/");
    let first = p.split('/').next().unwrap_or("");
    match scope.strip_prefix('!') {
        Some(neg) => !first.eq_ignore_ascii_case(neg),
        None => first.eq_ignore_ascii_case(scope),
    }
}

/// 核心检索(三壳共用)。mode: hybrid | grep | vector。
/// `scope`:可选的盘点根相对路径首段过滤(见 [`path_in_scope`]);None=全盘。
pub fn search(
    query: &str,
    top_k: usize,
    mode: &str,
    scope: Option<&str>,
) -> Result<FableSearchResult, String> {
    let started = std::time::Instant::now();
    // 防御性截断:超长查询(误把整篇文档/日志粘进搜索框)会撑爆嵌入请求体与 FTS5 MATCH
    // 表达式(单个 ~2MB 短语),且对召回毫无增益 —— 检索意图在前几十字就已表达。截到 2000
    // 字符,正常查询零影响;这是「最严峻输入」下保证向量/倒排两腿都不被拖垮的硬护栏。
    const MAX_QUERY_CHARS: usize = 2000;
    let clamped: String;
    let query: &str = if query.chars().count() > MAX_QUERY_CHARS {
        clamped = query.chars().take(MAX_QUERY_CHARS).collect();
        &clamped
    } else {
        query
    };
    let top_k = top_k.clamp(1, 50);
    let want_grep = mode != "vector";
    let want_vec = mode != "grep";

    // 两车道真并行(thread::scope);单车道失败不连坐 —— grep 永远可用,向量缺 key 时降级
    let mut grep_res: Result<(Vec<GrepHit>, bool), String> = Ok((Vec::new(), false));
    let mut vec_res: Result<Vec<VecHit>, String> = Ok(Vec::new());
    std::thread::scope(|s| {
        let g = want_grep.then(|| s.spawn(|| grep_lane(query)));
        let v = want_vec.then(|| s.spawn(|| vector_lane(query, top_k)));
        if let Some(h) = g {
            grep_res = h.join().unwrap_or_else(|_| Err("grep 车道 panic".into()));
        }
        if let Some(h) = v {
            vec_res = h.join().unwrap_or_else(|_| Err("向量车道 panic".into()));
        }
    });

    let (grep_hits, grep_truncated) = match grep_res {
        Ok(x) => x,
        Err(e) if mode == "grep" => return Err(e),
        Err(_) => (Vec::new(), false),
    };
    let vec_hits = match vec_res {
        Ok(x) => x,
        Err(e) if mode == "vector" => return Err(e),
        Err(_) => Vec::new(), // hybrid 下向量车道缺 key/断网 → 静默降级成纯 grep
    };
    // scope 过滤:命中后按相对路径首段筛(妈妈库 wiki / 外库 !wiki / 全盘 None);零回归。
    let grep_hits: Vec<GrepHit> = grep_hits
        .into_iter()
        .filter(|h| path_in_scope(&h.path, scope))
        .collect();
    let vec_hits: Vec<VecHit> = vec_hits
        .into_iter()
        .filter(|h| path_in_scope(&h.path, scope))
        .collect();
    let (n_grep, n_vec) = (grep_hits.len(), vec_hits.len());

    // ── P0-1 修:RRF 融合 key 降到**文件级** ──
    // 原 bug:grep 用 `path#L行号`、向量用 `path#C段号`,两套编号天然不相交 → 同一文件被两路
    // 命中也永远进不了 and_modify 分支,`lanes` 恒单元素,RRF「两路同时命中加权顶上」彻底失效。
    // 现在两路都按 `path` 归并:同一文件被 grep + 向量都命中时,rrf 真正叠加、lanes 含两者。
    struct Fused {
        hit: FableHit,
        rrf: f32,
        /// 重排专家「读全文打分」用的文本(向量=chunk 全文 / grep=命中行上下文窗口);不展示。
        doc: String,
    }
    let mut fused: HashMap<String, Fused> = HashMap::new();
    for (rank, h) in grep_hits.into_iter().enumerate() {
        let key = h.path.clone();
        let rrf = 1.0 / (60.0 + rank as f32);
        fused
            .entry(key)
            .and_modify(|f| {
                f.rrf += rrf;
                if !f.hit.lanes.contains(&"grep".to_string()) {
                    f.hit.lanes.push("grep".into());
                }
                if h.context.len() > f.doc.len() {
                    f.doc = h.context.clone();
                }
            })
            .or_insert(Fused {
                hit: FableHit {
                    path: h.path,
                    abspath: h.abspath,
                    location: format!("L{}", h.line),
                    snippet: h.snippet,
                    score: 0.0,
                    lanes: vec!["grep".into()],
                },
                rrf,
                doc: h.context,
            });
    }
    for (rank, h) in vec_hits.into_iter().enumerate() {
        let key = h.path.clone();
        let rrf = 1.0 / (60.0 + rank as f32);
        let snippet: String = h.text.chars().take(220).collect();
        fused
            .entry(key)
            .and_modify(|f| {
                f.rrf += rrf;
                if !f.hit.lanes.contains(&"vector".to_string()) {
                    f.hit.lanes.push("vector".into());
                }
                if h.text.len() > f.doc.len() {
                    f.doc = h.text.clone();
                }
            })
            .or_insert(Fused {
                hit: FableHit {
                    path: h.path,
                    abspath: h.abspath,
                    location: format!("C{}", h.seq),
                    snippet,
                    score: 0.0,
                    lanes: vec!["vector".into()],
                },
                rrf,
                doc: h.text,
            });
    }
    let mut merged: Vec<Fused> = fused.into_values().collect();
    merged.sort_by(|a, b| {
        b.rrf
            .partial_cmp(&a.rrf)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    merged.truncate(RERANK_N);

    // ── P2-1 精排闸门(详解 §4/§5):只在「该精排」时才请专家 ──
    // 条件:混检 + 有重排服务商 + 候选≥3 + 前两名分数咬得紧(难分高下,正是粗筛分不清、
    // 精排价值最大的场景)。一骑绝尘 / 候选过少 / 服务不可用 → 直接保持融合序(优雅降级)。
    // 重排仅在 mode 恰为 "hybrid" 时触发。**契约**: 想要「双车道融合但不重排」的快档(快速模式
    // 召回走这条, 见 chat::forced_recall_block), 传一个既非 "grep" 也非 "vector" 的多车道 mode
    // (如 "grep_vec") —— want_grep/want_vec 都为真(两腿都跑), 但因 != "hybrid" 直接跳过这层网络
    // 重排, 把召回从 ~1.8s 降到 ~250ms。改这里的判断前请同步 forced_recall_block。
    let mut reranked = false;
    let gate_close = merged.len() >= 2 && {
        let (r1, r2) = (merged[0].rrf, merged[1].rrf);
        r1 > 0.0 && (r1 - r2) / r1 < 0.25
    };
    if mode == "hybrid"
        && merged.len() >= 3
        && gate_close
        && crate::sense::active_provider("rerank").is_some()
    {
        // 喂**全文**(向量 chunk 全文 / grep 命中行上下文窗口),不再喂展示用 160/220 字碎片。
        let docs: Vec<String> = merged.iter().map(|f| f.doc.clone()).collect();
        // 查询级缓存(P2-1 ③):同一查询 + 同一候选签名命中则跳过这次网络调用。
        let sig = rerank_sig(
            query,
            &merged
                .iter()
                .map(|f| (&f.hit.path, &f.hit.location))
                .collect::<Vec<_>>(),
        );
        let order = match rerank_cache_get(&sig) {
            Some(o) => Some(o),
            None => match super::index::rerank(query, &docs, merged.len()) {
                Ok(o) => {
                    rerank_cache_put(sig, o.clone());
                    Some(o)
                }
                Err(_) => None,
            },
        };
        if let Some(order) = order {
            let mut reordered: Vec<Fused> = Vec::with_capacity(merged.len());
            let mut taken = vec![false; merged.len()];
            for (idx, score) in &order {
                if let Some(f) = merged.get(*idx) {
                    if !taken[*idx] {
                        taken[*idx] = true;
                        reordered.push(Fused {
                            hit: {
                                let mut h = f.hit.clone();
                                h.score = *score;
                                h
                            },
                            rrf: f.rrf,
                            doc: f.doc.clone(),
                        });
                    }
                }
            }
            for (i, f) in merged.iter().enumerate() {
                if !taken[i] {
                    reordered.push(Fused {
                        hit: f.hit.clone(),
                        rrf: f.rrf,
                        doc: f.doc.clone(),
                    });
                }
            }
            merged = reordered;
            reranked = true;
        }
    }

    let hits: Vec<FableHit> = merged
        .into_iter()
        .take(top_k)
        .map(|mut f| {
            if f.hit.score == 0.0 {
                f.hit.score = f.rrf;
            }
            f.hit
        })
        .collect();

    Ok(FableSearchResult {
        query: query.to_string(),
        mode: mode.to_string(),
        hits,
        grep_hits: n_grep,
        vector_hits: n_vec,
        reranked,
        grep_truncated,
        ms: started.elapsed().as_millis() as u64,
    })
}

// ───────────────────────── 重排结果缓存(P2-1 ③)─────────────────────────

/// 缓存签名 = 查询 + 候选集(path#location 有序拼接)。候选集变了就重算,故签名编入全部候选键。
fn rerank_sig(query: &str, cands: &[(&String, &String)]) -> String {
    let mut s = String::with_capacity(query.len() + cands.len() * 24);
    s.push_str(query);
    for (p, loc) in cands {
        s.push('\u{0}');
        s.push_str(p);
        s.push('#');
        s.push_str(loc);
    }
    s
}

struct RerankCache {
    cap: usize,
    map: HashMap<String, Vec<(usize, f32)>>,
    order: VecDeque<String>,
}
static RERANK_CACHE: Lazy<Mutex<RerankCache>> = Lazy::new(|| {
    Mutex::new(RerankCache {
        cap: 128,
        map: HashMap::new(),
        order: VecDeque::new(),
    })
});

fn rerank_cache_get(sig: &str) -> Option<Vec<(usize, f32)>> {
    let mut c = RERANK_CACHE.lock().unwrap();
    let v = c.map.get(sig)?.clone();
    c.order.retain(|x| x != sig);
    c.order.push_back(sig.to_string());
    Some(v)
}

fn rerank_cache_put(sig: String, val: Vec<(usize, f32)>) {
    let mut c = RERANK_CACHE.lock().unwrap();
    if c.map.insert(sig.clone(), val).is_none() {
        c.order.push_back(sig);
        while c.order.len() > c.cap {
            if let Some(old) = c.order.pop_front() {
                c.map.remove(&old);
            }
        }
    } else {
        c.order.retain(|x| x != &sig);
        c.order.push_back(sig);
    }
}

// ───────────────────────── 命令 ─────────────────────────

/// 混检命令。桌面端 async + spawn_blocking:hybrid 检索要做 grep + 向量 + 重排,且
/// 查询嵌入会去抢后台索引正持有的 embedder 全局锁——这一等若发生在 Tauri 主线程上,
/// WebView 消息泵停摆 >5s 就被判「无响应」强杀。挪到阻塞线程池等锁,UI 始终不冻。
/// server flavor 无 UI 主线程、dispatch_sync 本就在 spawn_blocking 中,保持同步。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn fable_search(
    query: String,
    top_k: Option<usize>,
    mode: Option<String>,
    scope: Option<String>,
) -> Result<FableSearchResult, String> {
    tauri::async_runtime::spawn_blocking(move || fable_search_sync(query, top_k, mode, scope))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
pub fn fable_search(
    query: String,
    top_k: Option<usize>,
    mode: Option<String>,
    scope: Option<String>,
) -> Result<FableSearchResult, String> {
    fable_search_sync(query, top_k, mode, scope)
}

/// 内层同步实现:两个 flavor 共用,避免重复校验逻辑。
fn fable_search_sync(
    query: String,
    top_k: Option<usize>,
    mode: Option<String>,
    scope: Option<String>,
) -> Result<FableSearchResult, String> {
    let mode = mode.unwrap_or_else(|| "hybrid".into());
    if !["hybrid", "grep", "vector"].contains(&mode.as_str()) {
        return Err("mode 只接受 hybrid | grep | vector".into());
    }
    let scope = scope.as_deref().map(str::trim).filter(|s| !s.is_empty());
    search(query.trim(), top_k.unwrap_or(12), &mode, scope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_query_segments_cjk_and_latin() {
        let (full, terms) = split_query("  Open Hours 营业时间 ");
        assert_eq!(full, "open hours 营业时间");
        // 拉丁词原样保留
        assert!(terms.contains(&"open".to_string()));
        assert!(terms.contains(&"hours".to_string()));
        // CJK 段切成重叠二元组(概念词),而非整段一个 token
        assert!(terms.contains(&"营业".to_string()));
        assert!(terms.contains(&"时间".to_string()));
    }

    #[test]
    fn split_query_drops_cjk_stopword_bigrams() {
        // 自然句:功能词二元组(我想/想了/了解/怎么…)应被滤掉,内容词(模型/索引)保留
        let (_full, terms) = split_query("我想了解模型索引是怎么做的");
        assert!(terms.contains(&"模型".to_string()));
        assert!(terms.contains(&"索引".to_string()));
        assert!(!terms.contains(&"我想".to_string()));
        assert!(!terms.contains(&"怎么".to_string()));
    }

    #[test]
    fn fts_expr_uses_trigrams_not_whole_phrase() {
        // CJK ≥3 段 → 出三元组 OR(自然句/拼接词也有候选),不再是整句一个短语
        let expr = fts_query_expr("知识库检索").unwrap();
        assert!(expr.contains("\"知识库\""));
        assert!(expr.contains("\"识库检\""));
        assert!(expr.contains(" OR "));
        // 纯 2 字 CJK(独立概念)→ trigram 索引不了 → None(靠实时扫描兜底)
        assert!(fts_query_expr("模型").is_none());
        assert!(fts_query_expr("模型 索引").is_none());
        // 拉丁 ≥3 词原样成短语;<3 拉丁(a/bc)被丢,故标点切词后无 ≥3 项 → None
        assert_eq!(fts_query_expr("config").unwrap(), "\"config\"");
        assert!(fts_query_expr("a\"bc").is_none());
    }

    #[test]
    fn has_short_terms_detects_bare_cjk_concepts() {
        // 含独立 2 字 CJK 概念 → 需补实时扫描(trigram 服务不了)
        assert!(has_short_terms("模型"));
        assert!(has_short_terms("模型 索引"));
        // 长 CJK 段(自然句/拼接词)→ trigram 三元组够用,不必慢扫
        assert!(!has_short_terms("知识库检索系统"));
        assert!(!has_short_terms("我想了解模型索引是怎么做的"));
        // 2 字拉丁词也算短词
        assert!(has_short_terms("ab"));
        assert!(!has_short_terms("abcd"));
    }

    #[test]
    fn scope_filter_first_segment() {
        // None / 空 → 全盘放行
        assert!(path_in_scope("wiki/概念/x.md", None));
        assert!(path_in_scope("raw/a.md", Some("")));
        assert!(path_in_scope("raw/a.md", Some("  ")));
        // 正向:仅首段命中
        assert!(path_in_scope("wiki/概念/x.md", Some("wiki")));
        assert!(!path_in_scope("raw/a.md", Some("wiki")));
        assert!(!path_in_scope("output/r.md", Some("wiki")));
        // 反向 !wiki:首段不是 wiki 的才放行(「外面整个库」)
        assert!(!path_in_scope("wiki/概念/x.md", Some("!wiki")));
        assert!(path_in_scope("raw/a.md", Some("!wiki")));
        assert!(path_in_scope("output/r.md", Some("!wiki")));
        // 反斜杠路径(Windows)也按首段判定
        assert!(path_in_scope("wiki\\概念\\x.md", Some("wiki")));
        // 大小写不敏感
        assert!(path_in_scope("WIKI/x.md", Some("wiki")));
    }

    #[test]
    fn rerank_sig_changes_with_candidates() {
        let p1 = "a/b.md".to_string();
        let l1 = "L3".to_string();
        let l2 = "C5".to_string();
        let s1 = rerank_sig("q", &[(&p1, &l1)]);
        let s2 = rerank_sig("q", &[(&p1, &l2)]); // 候选位置变了 → 签名必须变
        let s3 = rerank_sig("q2", &[(&p1, &l1)]); // 查询变了 → 签名必须变
        assert_ne!(s1, s2);
        assert_ne!(s1, s3);
        assert_eq!(s1, rerank_sig("q", &[(&p1, &l1)])); // 同输入同签名(确定性)
    }

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

    #[test]
    fn rerank_cache_roundtrip() {
        let sig = "unit-test-sig-xyz".to_string();
        assert!(rerank_cache_get(&sig).is_none());
        rerank_cache_put(sig.clone(), vec![(2, 0.9), (0, 0.5)]);
        assert_eq!(rerank_cache_get(&sig), Some(vec![(2, 0.9), (0, 0.5)]));
    }
}
