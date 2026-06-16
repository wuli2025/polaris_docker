//! 塌平混检(神经层)—— grep 车道 ∥ 向量车道 并行 → RRF 融合 → 重排。
//!
//! PRD v5 §1「神经:四 tier 塌平混检」+ 用户拍板「grep 搜索和 RAG 并行,CPU 还很多」:
//! - **grep 车道**:多核 work-stealing 扫盘点表里的文本文件(字面/分词命中,零依赖
//!   零索引延迟 —— 盘点完成那一刻起就能搜,这就是 L1a「首小时全盘可搜」的搜);
//! - **向量车道**:查询嵌入 → 流式暴力余弦(SQLite 顺序读 vec BLOB,十万级亚秒;
//!   千万级在此函数内换 ANN,签名不变);
//! - 两车道 `thread::scope` 真并行,先到先等,RRF(k=60)塌平融合;
//! - 有重排服务商时对融合 top-40 精排一次,失败静默保持 RRF 序(可降级)。

use super::{lex_available, open_db, worker_count};
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

/// 把查询拆成「全句 + ≥2 字符分词」。中文整句即全句通道,CJK 不强行分词。
fn split_query(query: &str) -> (String, Vec<String>) {
    let q_full = query.trim().to_lowercase();
    let tokens: Vec<String> = q_full
        .split_whitespace()
        .map(|t| t.to_string())
        .filter(|t| t.chars().count() >= 2 && *t != q_full)
        .collect();
    (q_full, tokens)
}

/// 组 FTS5(trigram)MATCH 表达式:全句 + 各 token(均需 ≥3 字符)拼成 OR;双引号转义成短语。
/// 返回 None 表示无可用 ≥3 字符项(trigram 索引不了 1~2 字符)→ 调用方落实时扫描兜底。
fn fts_match_expr(q_full: &str, tokens: &[String]) -> Option<String> {
    let esc = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let mut terms: Vec<String> = Vec::new();
    if q_full.chars().count() >= 3 {
        terms.push(esc(q_full));
    }
    for t in tokens {
        if t.chars().count() >= 3 {
            terms.push(esc(t));
        }
    }
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" OR "))
    }
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
                let Ok(bytes) = std::fs::read(&abs) else { continue };
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
                // 取最多 2 条命中行做摘录(行号按原文);并截一段上下文窗口给重排读全文。
                let mut snippets = 0;
                for (i, line) in lines.iter().enumerate() {
                    let ll = line.to_lowercase();
                    let hit_full = ll.contains(*q_full);
                    let hit_tok = tokens.iter().any(|t| ll.contains(t.as_str()));
                    if hit_full || hit_tok {
                        let snippet: String = line.trim().chars().take(160).collect();
                        // 命中行 ±2 行拼成上下文窗口(P2-1:让重排专家读到的不只是孤零零一行)。
                        let lo = i.saturating_sub(2);
                        let hi = (i + 3).min(lines.len());
                        let context: String =
                            lines[lo..hi].join("\n").chars().take(700).collect();
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
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(60);
    (out, truncated.load(Ordering::Relaxed))
}

/// 认字腿(P1-2):优先走 FTS5 倒排(提前建好、查词秒回、**覆盖全部文本文件不漏**),
/// 命中候选后只回读这几百个文件精确算分;倒排不可用或查询过短(<3 字符)时退回实时扫描兜底。
fn grep_lane(query: &str) -> Result<(Vec<GrepHit>, bool), String> {
    let (q_full, tokens) = split_query(query);
    if q_full.is_empty() {
        return Ok((Vec::new(), false));
    }
    let conn = open_db()?;

    // —— 倒排路:lex 就绪 + 有 ≥3 字符项 ——
    if lex_available(&conn) {
        if let Some(expr) = fts_match_expr(&q_full, &tokens) {
            let candidates: Vec<(String, String, i64)> = {
                let mut stmt = conn
                    .prepare(
                        "SELECT r.path, f.relpath, f.size FROM lex l
                         JOIN files f ON f.id=l.rowid JOIN roots r ON r.id=f.root_id
                         WHERE l.body MATCH ?1 ORDER BY bm25(lex) LIMIT ?2",
                    )
                    .map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map(rusqlite::params![expr, FTS_CAND_LIMIT], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
                    })
                    .map_err(|e| e.to_string())?;
                rows.flatten().collect()
            };
            drop(conn);
            // 倒排已对全库匹配,候选即「全部命中文件按相关度排序后的前 N」→ 不存在实时扫描的漏检截断。
            let (hits, _) = scan_and_score(candidates, &q_full, &tokens, None);
            return Ok((hits, false));
        }
        // 查询不足 3 字符:trigram 索引不了 → 落实时扫描。
    }

    // —— 兜底:实时扫描(无 FTS / 查询过短),保留原字节预算护栏 ——
    let candidates: Vec<(String, String, i64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT r.path, f.relpath, f.size FROM files f JOIN roots r ON r.id=f.root_id
                 WHERE f.kind='text' AND f.size<=?1 ORDER BY f.size ASC LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([MAX_GREP_FILE_BYTES, MAX_GREP_FILES], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
            })
            .map_err(|e| e.to_string())?;
        rows.flatten().collect()
    };
    drop(conn);
    Ok(scan_and_score(candidates, &q_full, &tokens, Some(MAX_GREP_TOTAL_BYTES)))
}

// ───────────────────────── 向量车道 ─────────────────────────

struct VecHit {
    path: String,
    abspath: String,
    seq: i64,
    text: String,
    score: f32,
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

    // ── 第一段 · 二值粗筛(P1-1):只读 bits 算汉明距离(读量约 f32 的 1/32),
    //    有界 top 表选出候选;P2-2 只认与当前模型一致、维度匹配的向量。 ──
    let cand_n = (top_k * 8).max(200);
    let mut cand: Vec<(i64, u32)> = Vec::with_capacity(cand_n + 1); // (chunk id, hamming)
    {
        // probes 非空时只扫探针 cell + 未分配新数据;为空时扫全表(回退)。
        let stage1_sql = if probes.is_empty() {
            "SELECT id, bits FROM chunks WHERE dim=?1 AND model=?2 AND bits IS NOT NULL".to_string()
        } else {
            let csv = probes.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            format!(
                "SELECT id, bits FROM chunks WHERE dim=?1 AND model=?2 AND bits IS NOT NULL \
                 AND (cell=-1 OR cell IN ({csv}))"
            )
        };
        let mut params: Vec<rusqlite::types::Value> = vec![
            rusqlite::types::Value::Integer(qv.len() as i64),
            rusqlite::types::Value::Text(model.clone()),
        ];
        for p in &probes {
            params.push(rusqlite::types::Value::Integer(*p));
        }
        let mut stmt = conn.prepare(&stage1_sql).map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query(rusqlite::params_from_iter(params.iter()))
            .map_err(|e| e.to_string())?;
        let mut worst = u32::MAX;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let bits: Vec<u8> = row.get(1).map_err(|e| e.to_string())?;
            if bits.len() != qbits.len() {
                continue;
            }
            let h = super::index::hamming(&qbits, &bits);
            if cand.len() >= cand_n && h >= worst {
                continue;
            }
            let id: i64 = row.get(0).map_err(|e| e.to_string())?;
            cand.push((id, h));
            if cand.len() > cand_n {
                cand.sort_by_key(|x| x.1);
                cand.truncate(cand_n);
                worst = cand.last().map(|x| x.1).unwrap_or(u32::MAX);
            }
        }
    }
    cand.sort_by_key(|x| x.1);
    cand.truncate(cand_n);

    // ── 第二段 · 精排(P1-3):只对候选回读 f32 原始向量算点积(归一化 → 即余弦)──
    let mut top: Vec<VecHit> = Vec::new();
    if !cand.is_empty() {
        let ids: Vec<i64> = cand.iter().map(|x| x.0).collect();
        for group in ids.chunks(500) {
            let placeholders = group.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT c.vec, c.seq, c.text, f.relpath, r.path FROM chunks c
                 JOIN files f ON f.id=c.file_id JOIN roots r ON r.id=f.root_id
                 WHERE c.id IN ({placeholders})"
            );
            let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
            let mut rows = stmt
                .query(rusqlite::params_from_iter(group.iter()))
                .map_err(|e| e.to_string())?;
            while let Some(row) = rows.next().map_err(|e| e.to_string())? {
                let blob: Vec<u8> = row.get(0).map_err(|e| e.to_string())?;
                let v = super::index::blob_to_vec(&blob);
                if v.len() != qv.len() {
                    continue;
                }
                let score: f32 = v.iter().zip(qv.iter()).map(|(a, b)| a * b).sum();
                let seq: i64 = row.get(1).map_err(|e| e.to_string())?;
                let text: String = row.get(2).map_err(|e| e.to_string())?;
                let rel: String = row.get(3).map_err(|e| e.to_string())?;
                let root: String = row.get(4).map_err(|e| e.to_string())?;
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
    } else {
        // 兜底:同模型向量里没有任何 bits(理论上不出现,留作健壮性)→ 暴力精扫,仍按 model 过滤。
        let mut stmt = conn
            .prepare(
                "SELECT c.vec, c.seq, c.text, f.relpath, r.path FROM chunks c
                 JOIN files f ON f.id=c.file_id JOIN roots r ON r.id=f.root_id
                 WHERE c.dim=?1 AND c.model=?2",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query(rusqlite::params![qv.len() as i64, model])
            .map_err(|e| e.to_string())?;
        let mut min_score = f32::MIN;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let blob: Vec<u8> = row.get(0).map_err(|e| e.to_string())?;
            let v = super::index::blob_to_vec(&blob);
            if v.len() != qv.len() {
                continue;
            }
            let score: f32 = v.iter().zip(qv.iter()).map(|(a, b)| a * b).sum();
            if top.len() >= want && score <= min_score {
                continue;
            }
            let seq: i64 = row.get(1).map_err(|e| e.to_string())?;
            let text: String = row.get(2).map_err(|e| e.to_string())?;
            let rel: String = row.get(3).map_err(|e| e.to_string())?;
            let root: String = row.get(4).map_err(|e| e.to_string())?;
            top.push(VecHit {
                abspath: std::path::Path::new(&root).join(&rel).to_string_lossy().into_owned(),
                path: rel,
                seq,
                text,
                score,
            });
            if top.len() > want {
                top.sort_by(|a, b| {
                    b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
                });
                top.truncate(want);
                min_score = top.last().map(|h| h.score).unwrap_or(f32::MIN);
            }
        }
    }
    top.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
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
    let grep_hits: Vec<GrepHit> =
        grep_hits.into_iter().filter(|h| path_in_scope(&h.path, scope)).collect();
    let vec_hits: Vec<VecHit> =
        vec_hits.into_iter().filter(|h| path_in_scope(&h.path, scope)).collect();
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
    merged.sort_by(|a, b| b.rrf.partial_cmp(&a.rrf).unwrap_or(std::cmp::Ordering::Equal));
    merged.truncate(RERANK_N);

    // ── P2-1 精排闸门(详解 §4/§5):只在「该精排」时才请专家 ──
    // 条件:混检 + 有重排服务商 + 候选≥3 + 前两名分数咬得紧(难分高下,正是粗筛分不清、
    // 精排价值最大的场景)。一骑绝尘 / 候选过少 / 服务不可用 → 直接保持融合序(优雅降级)。
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
        let sig = rerank_sig(query, &merged.iter().map(|f| (&f.hit.path, &f.hit.location)).collect::<Vec<_>>());
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
                    reordered.push(Fused { hit: f.hit.clone(), rrf: f.rrf, doc: f.doc.clone() });
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
    Mutex::new(RerankCache { cap: 128, map: HashMap::new(), order: VecDeque::new() })
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

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fable_search(
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
    fn split_query_full_and_tokens() {
        let (full, toks) = split_query("  Open Hours 营业时间 ");
        assert_eq!(full, "open hours 营业时间");
        // ≥2 字符、且不等于全句的分词
        assert!(toks.contains(&"open".to_string()));
        assert!(toks.contains(&"hours".to_string()));
        assert!(toks.contains(&"营业时间".to_string()));
    }

    #[test]
    fn fts_expr_needs_three_chars() {
        // 全句 4 字符(≥3)→ 有表达式
        let (full, toks) = split_query("开放时间");
        assert!(fts_match_expr(&full, &toks).is_some());
        // 全句 2 字符且无 ≥3 字符 token → None(trigram 索引不了)
        let (full2, toks2) = split_query("时间");
        assert!(fts_match_expr(&full2, &toks2).is_none());
        // 内嵌双引号要被转义成 ""(防 FTS 语法注入/语法错)
        let expr = fts_match_expr("a\"b\"c", &[]).unwrap();
        assert_eq!(expr, "\"a\"\"b\"\"c\"");
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

    #[test]
    fn rerank_cache_roundtrip() {
        let sig = "unit-test-sig-xyz".to_string();
        assert!(rerank_cache_get(&sig).is_none());
        rerank_cache_put(sig.clone(), vec![(2, 0.9), (0, 0.5)]);
        assert_eq!(rerank_cache_get(&sig), Some(vec![(2, 0.9), (0, 0.5)]));
    }
}
