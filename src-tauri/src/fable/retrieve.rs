//! 塌平混检(神经层)—— grep 车道 ∥ 向量车道 并行 → RRF 融合 → 重排。
//!
//! PRD v5 §1「神经:四 tier 塌平混检」+ 用户拍板「grep 搜索和 RAG 并行,CPU 还很多」:
//! - **grep 车道**:多核 work-stealing 扫盘点表里的文本文件(字面/分词命中,零依赖
//!   零索引延迟 —— 盘点完成那一刻起就能搜,这就是 L1a「首小时全盘可搜」的搜);
//! - **向量车道**:查询嵌入 → 流式暴力余弦(SQLite 顺序读 vec BLOB,十万级亚秒;
//!   千万级在此函数内换 ANN,签名不变);
//! - 两车道 `thread::scope` 真并行,先到先等,RRF(k=60)塌平融合;
//! - 有重排服务商时对融合 top-40 精排一次,失败静默保持 RRF 序(可降级)。

use super::{open_db, worker_count};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// grep 车道单文件上限(超大文本不参与全文扫,靠 agent 的定向 Grep 工具)。
const MAX_GREP_FILE_BYTES: i64 = 4_000_000;
/// grep 车道单次检索的文件数/总字节预算(1TB 级护栏;按 size 升序取,小文件优先)。
const MAX_GREP_FILES: i64 = 20_000;
const MAX_GREP_TOTAL_BYTES: u64 = 800 * 1024 * 1024;

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
    score: f32,
}

fn grep_lane(query: &str) -> Result<(Vec<GrepHit>, bool), String> {
    let conn = open_db()?;
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

    let q_full = query.trim().to_lowercase();
    if q_full.is_empty() {
        return Ok((Vec::new(), false));
    }
    // 全句命中权重高;空格分词(≥2 字符)各记 1 分 —— 中文整句即全句通道,CJK 不强行分词
    let tokens: Vec<String> = q_full
        .split_whitespace()
        .map(|t| t.to_string())
        .filter(|t| t.chars().count() >= 2 && *t != q_full)
        .collect();

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
                if spent.fetch_add(size as u64, Ordering::Relaxed) > MAX_GREP_TOTAL_BYTES {
                    truncated.store(true, Ordering::Relaxed);
                    break;
                }
                let abs = std::path::Path::new(&root).join(&rel);
                let Ok(bytes) = std::fs::read(&abs) else { continue };
                if bytes.iter().take(4096).any(|&b| b == 0) {
                    continue; // 二进制伪文本
                }
                let text = String::from_utf8_lossy(&bytes);
                let lower = text.to_lowercase();
                let mut score = 0f32;
                if lower.contains(q_full.as_str()) {
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
                // 取最多 2 条命中行做摘录(行号按原文)
                let mut snippets = 0;
                for (i, line) in text.lines().enumerate() {
                    let ll = line.to_lowercase();
                    let hit_full = ll.contains(q_full.as_str());
                    let hit_tok = tokens.iter().any(|t| ll.contains(t.as_str()));
                    if hit_full || hit_tok {
                        let snippet: String = line.trim().chars().take(160).collect();
                        hits.lock().unwrap().push(GrepHit {
                            path: rel.clone(),
                            abspath: abs.to_string_lossy().into_owned(),
                            line: i + 1,
                            snippet,
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
    Ok((out, truncated.load(Ordering::Relaxed)))
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
    let qv = super::index::embed_texts(&[query.to_string()])?
        .into_iter()
        .next()
        .ok_or("查询嵌入为空")?;
    let qnorm = (qv.iter().map(|x| x * x).sum::<f32>()).sqrt().max(1e-6);

    let conn = open_db()?;
    let mut stmt = conn
        .prepare(
            "SELECT c.vec, c.seq, c.text, f.relpath, r.path
             FROM chunks c JOIN files f ON f.id=c.file_id JOIN roots r ON r.id=f.root_id",
        )
        .map_err(|e| e.to_string())?;
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    // 流式余弦 + 有界 top 表(避免把全部 chunk 文本拉进内存)
    let mut top: Vec<VecHit> = Vec::with_capacity(top_k * 2 + 1);
    let mut min_score = f32::MIN;
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let blob: Vec<u8> = row.get(0).map_err(|e| e.to_string())?;
        let v = super::index::blob_to_vec(&blob);
        if v.len() != qv.len() {
            continue; // 换过嵌入模型留下的异维向量,重建索引可清
        }
        let dot: f32 = v.iter().zip(qv.iter()).map(|(a, b)| a * b).sum();
        let vnorm = (v.iter().map(|x| x * x).sum::<f32>()).sqrt().max(1e-6);
        let score = dot / (vnorm * qnorm);
        if top.len() >= top_k * 2 && score <= min_score {
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
        if top.len() > top_k * 2 {
            top.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
            top.truncate(top_k * 2);
            min_score = top.last().map(|h| h.score).unwrap_or(f32::MIN);
        }
    }
    top.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    top.truncate(top_k * 2);
    Ok(top)
}

// ───────────────────────── 融合 + 重排 ─────────────────────────

/// 核心检索(三壳共用)。mode: hybrid | grep | vector。
pub fn search(query: &str, top_k: usize, mode: &str) -> Result<FableSearchResult, String> {
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
    let (n_grep, n_vec) = (grep_hits.len(), vec_hits.len());

    // RRF 融合:key = path#location,分数 Σ 1/(60+rank)
    struct Fused {
        hit: FableHit,
        rrf: f32,
    }
    let mut fused: HashMap<String, Fused> = HashMap::new();
    for (rank, h) in grep_hits.into_iter().enumerate() {
        let key = format!("{}#L{}", h.path, h.line);
        let rrf = 1.0 / (60.0 + rank as f32);
        fused
            .entry(key)
            .and_modify(|f| {
                f.rrf += rrf;
                if !f.hit.lanes.contains(&"grep".to_string()) {
                    f.hit.lanes.push("grep".into());
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
            });
    }
    for (rank, h) in vec_hits.into_iter().enumerate() {
        let key = format!("{}#C{}", h.path, h.seq);
        let rrf = 1.0 / (60.0 + rank as f32);
        let snippet: String = h.text.chars().take(220).collect();
        fused
            .entry(key)
            .and_modify(|f| {
                f.rrf += rrf;
                if !f.hit.lanes.contains(&"vector".to_string()) {
                    f.hit.lanes.push("vector".into());
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
            });
    }
    let mut merged: Vec<Fused> = fused.into_values().collect();
    merged.sort_by(|a, b| b.rrf.partial_cmp(&a.rrf).unwrap_or(std::cmp::Ordering::Equal));
    merged.truncate(40);

    // 精排(可降级):有重排服务商且是混检时,对 top-40 重排一次
    let mut reranked = false;
    if mode == "hybrid" && merged.len() > 1 && crate::sense::active_provider("rerank").is_some() {
        let docs: Vec<String> = merged.iter().map(|f| f.hit.snippet.clone()).collect();
        if let Ok(order) = super::index::rerank(query, &docs, merged.len()) {
            let mut reordered: Vec<Fused> = Vec::with_capacity(merged.len());
            let mut taken = vec![false; merged.len()];
            for (idx, score) in &order {
                if let Some(f) = merged.get(*idx) {
                    if !taken[*idx] {
                        taken[*idx] = true;
                        let mut f2 = Fused { hit: f.hit.clone(), rrf: f.rrf };
                        f2.hit.score = *score;
                        reordered.push(f2);
                    }
                }
            }
            for (i, f) in merged.iter().enumerate() {
                if !taken[i] {
                    reordered.push(Fused { hit: f.hit.clone(), rrf: f.rrf });
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

// ───────────────────────── 命令 ─────────────────────────

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fable_search(
    query: String,
    top_k: Option<usize>,
    mode: Option<String>,
) -> Result<FableSearchResult, String> {
    let mode = mode.unwrap_or_else(|| "hybrid".into());
    if !["hybrid", "grep", "vector"].contains(&mode.as_str()) {
        return Err("mode 只接受 hybrid | grep | vector".into());
    }
    search(query.trim(), top_k.unwrap_or(12), &mode)
}
