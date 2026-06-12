//! 向量车道(RAG)—— 文本 chunk → 嵌入 → SQLite 向量落库。
//!
//! 嵌入/重排走感官坞(sense.rs)当前生效的服务商:默认 硅基流动 BGE-M3 /
//! bge-reranker(钥匙②,免费)。PRD v5 §2.2「嵌入主路=硅基免费,本地 ONNX 兜底后续接入」。
//!
//! 工程姿势(PRD「巡夜人/滴灌」):一次构建只消化一个预算额(默认 4000 chunk),
//! 幂等续跑 —— files.chunked 标记位,断了再点继续;429 限速指数退避。

use super::{cancelled, open_db, CANCEL, INDEXING};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::time::Duration;

#[cfg(feature = "desktop")]
use tauri::{AppHandle, Emitter};
#[cfg(not(feature = "desktop"))]
use crate::host::AppHandle;

/// 单文件参与嵌入的大小上限(超大文本先靠 grep 车道;后续可分卷)。
const MAX_EMBED_FILE_BYTES: i64 = 2_000_000;
/// 单文件 chunk 上限(防单个巨文件吃光预算)。
const MAX_CHUNKS_PER_FILE: usize = 64;
/// 每请求批量条数(硅基免费档友好值)。
const EMBED_BATCH: usize = 16;

// ───────────────────────── 嵌入 / 重排客户端 ─────────────────────────

fn agent_http() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(120))
        .build()
}

/// 批量嵌入。429 退避重试 3 次;其余错误直接报(可读信息,UI 原样展示)。
pub fn embed_texts(texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
    let p = crate::sense::active_provider("embed")
        .ok_or("没有可用的嵌入服务商:在「设置 › 寓言计划 API」给硅基流动填 key(免费),或检查云感官总闸")?;
    let key = crate::sense::effective_key(&p);
    let base = p.base_url.trim_end_matches('/');
    let url = format!("{base}/v1/embeddings");
    let http = agent_http();
    let mut delay = 2u64;
    for attempt in 0..4 {
        let resp = http
            .post(&url)
            .set("authorization", &format!("Bearer {key}"))
            .send_json(json!({ "model": p.default_model, "input": texts }));
        match resp {
            Ok(r) => {
                let v: Value = r.into_json().map_err(|e| format!("嵌入响应解析失败: {e}"))?;
                let data = v
                    .get("data")
                    .and_then(|d| d.as_array())
                    .ok_or("嵌入响应缺 data 数组")?;
                let mut out: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
                for item in data {
                    let emb = item
                        .get("embedding")
                        .and_then(|e| e.as_array())
                        .ok_or("嵌入响应缺 embedding")?;
                    out.push(emb.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect());
                }
                if out.len() != texts.len() {
                    return Err(format!("嵌入条数不符: 发 {} 回 {}", texts.len(), out.len()));
                }
                return Ok(out);
            }
            Err(ureq::Error::Status(429, _)) if attempt < 3 => {
                std::thread::sleep(Duration::from_secs(delay));
                delay *= 2;
            }
            Err(ureq::Error::Status(code, r)) => {
                let body = r.into_string().unwrap_or_default();
                let brief: String = body.chars().take(200).collect();
                return Err(format!("嵌入接口 HTTP {code}: {brief}"));
            }
            Err(e) => return Err(format!("嵌入接口网络错误: {e}")),
        }
    }
    Err("嵌入接口持续限速(429),稍后再试".into())
}

/// 重排:返回按相关度降序的 (原 index, 分数)。失败属可降级(调用方保持原序)。
pub fn rerank(query: &str, docs: &[String], top_n: usize) -> Result<Vec<(usize, f32)>, String> {
    let p = crate::sense::active_provider("rerank").ok_or("没有可用的重排服务商")?;
    let key = crate::sense::effective_key(&p);
    let base = p.base_url.trim_end_matches('/');
    let resp = agent_http()
        .post(&format!("{base}/v1/rerank"))
        .set("authorization", &format!("Bearer {key}"))
        .send_json(json!({
            "model": p.default_model,
            "query": query,
            "documents": docs,
            "top_n": top_n,
        }))
        .map_err(|e| format!("重排接口失败: {e}"))?;
    let v: Value = resp.into_json().map_err(|e| format!("重排响应解析失败: {e}"))?;
    let results = v
        .get("results")
        .and_then(|r| r.as_array())
        .ok_or("重排响应缺 results")?;
    Ok(results
        .iter()
        .filter_map(|r| {
            let idx = r.get("index")?.as_u64()? as usize;
            let score = r.get("relevance_score")?.as_f64()? as f32;
            Some((idx, score))
        })
        .collect())
}

// ───────────────────────── chunker ─────────────────────────

/// 段落聚合式切块:按空行聚段到 ~1600 字符;超长段硬切(200 字符重叠)。
/// 全按 char 计数,杜绝多字节边界 panic。
pub(crate) fn chunk_text(s: &str) -> Vec<String> {
    const TARGET: usize = 1600;
    const OVERLAP: usize = 200;
    let mut chunks: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_chars = 0usize;
    let flush = |cur: &mut String, cur_chars: &mut usize, chunks: &mut Vec<String>| {
        let t = cur.trim();
        if t.chars().count() >= 24 {
            chunks.push(t.to_string());
        }
        cur.clear();
        *cur_chars = 0;
    };
    for para in s.split("\n\n") {
        let plen = para.chars().count();
        if plen > TARGET {
            flush(&mut cur, &mut cur_chars, &mut chunks);
            // 超长段:滑窗硬切
            let cs: Vec<char> = para.chars().collect();
            let mut start = 0usize;
            while start < cs.len() {
                let end = (start + TARGET).min(cs.len());
                chunks.push(cs[start..end].iter().collect::<String>().trim().to_string());
                if end == cs.len() {
                    break;
                }
                start = end.saturating_sub(OVERLAP);
            }
            continue;
        }
        if cur_chars + plen > TARGET {
            flush(&mut cur, &mut cur_chars, &mut chunks);
        }
        if !cur.is_empty() {
            cur.push_str("\n\n");
        }
        cur.push_str(para);
        cur_chars += plen + 2;
        if chunks.len() >= MAX_CHUNKS_PER_FILE {
            break;
        }
    }
    flush(&mut cur, &mut cur_chars, &mut chunks);
    chunks.retain(|c| !c.is_empty());
    chunks.truncate(MAX_CHUNKS_PER_FILE);
    chunks
}

pub(crate) fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

pub(crate) fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

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

/// 同步构建:消化 pending 文本文件直到预算耗尽。`progress(files_done, chunks_added, current)`。
pub fn build_index(
    max_chunks: usize,
    progress: &dyn Fn(u64, u64, &str),
) -> Result<IndexSummary, String> {
    // 先探一次嵌入可用性,免得扫半天才报 key 缺失
    let _ = crate::sense::active_provider("embed")
        .ok_or("没有可用的嵌入服务商:在「设置 › 寓言计划 API」给硅基流动填 key(免费),或检查云感官总闸")?;
    let started = std::time::Instant::now();
    let conn = open_db()?;
    let mut files_done = 0u64;
    let mut chunks_added = 0u64;
    let mut stopped = "全部完成".to_string();

    loop {
        if cancelled() {
            stopped = "已取消".into();
            break;
        }
        if chunks_added >= max_chunks as u64 {
            stopped = format!("本轮预算({max_chunks} chunk)耗尽,可再点继续");
            break;
        }
        // 小文件优先:先把海量小文档变可检索,大部头排后
        let batch: Vec<(i64, String, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT f.id, r.path, f.relpath FROM files f JOIN roots r ON r.id=f.root_id
                     WHERE f.kind='text' AND f.chunked=0 AND f.size<=?1
                     ORDER BY f.size ASC LIMIT 32",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([MAX_EMBED_FILE_BYTES], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
                })
                .map_err(|e| e.to_string())?;
            rows.flatten().collect()
        };
        if batch.is_empty() {
            break;
        }
        for (file_id, root, rel) in batch {
            if cancelled() || chunks_added >= max_chunks as u64 {
                break;
            }
            let abs = std::path::Path::new(&root).join(&rel);
            let text = match std::fs::read(&abs) {
                Ok(bytes) => {
                    if bytes.iter().take(4096).any(|&b| b == 0) {
                        String::new() // 伪文本(二进制改名),跳过
                    } else {
                        String::from_utf8_lossy(&bytes).into_owned()
                    }
                }
                Err(_) => String::new(), // 文件已消失/不可读:标记完成,下轮重扫会清
            };
            let chunks = chunk_text(&text);
            if !chunks.is_empty() {
                // 重嵌入前清旧 chunk(mtime 变更后 chunked 被重置的场景)
                conn.execute("DELETE FROM chunks WHERE file_id=?1", [file_id])
                    .map_err(|e| e.to_string())?;
                for (batch_i, group) in chunks.chunks(EMBED_BATCH).enumerate() {
                    let vecs = embed_texts(group)?;
                    conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
                    {
                        let mut stmt = conn
                            .prepare_cached(
                                "INSERT OR REPLACE INTO chunks(file_id,seq,text,dim,vec)
                                 VALUES(?1,?2,?3,?4,?5)",
                            )
                            .map_err(|e| e.to_string())?;
                        for (i, (t, v)) in group.iter().zip(vecs.iter()).enumerate() {
                            stmt.execute(rusqlite::params![
                                file_id,
                                (batch_i * EMBED_BATCH + i) as i64,
                                t,
                                v.len() as i64,
                                vec_to_blob(v)
                            ])
                            .map_err(|e| e.to_string())?;
                        }
                    }
                    conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
                    chunks_added += group.len() as u64;
                }
            }
            conn.execute("UPDATE files SET chunked=1 WHERE id=?1", [file_id])
                .map_err(|e| e.to_string())?;
            files_done += 1;
            progress(files_done, chunks_added, &rel);
        }
    }

    let files_pending: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE kind='text' AND chunked=0 AND size<=?1",
            [MAX_EMBED_FILE_BYTES],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(IndexSummary {
        files_done,
        chunks_added,
        files_pending: files_pending as u64,
        seconds: started.elapsed().as_secs_f64(),
        stopped,
    })
}

// ───────────────────────── 命令(后台线程 + 事件)─────────────────────────

fn emit(app: &AppHandle, payload: Value) {
    let _ = app.emit("fable:index", payload);
}

/// 开始(或继续)构建向量索引。立即返回,进度走 `fable:index` 事件。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fable_index_start(app: AppHandle, max_chunks: Option<usize>) -> Result<(), String> {
    if INDEXING.swap(true, Ordering::SeqCst) {
        return Err("索引构建已在进行中".into());
    }
    CANCEL.store(false, Ordering::SeqCst);
    let budget = max_chunks.unwrap_or(4000).clamp(100, 200_000);
    std::thread::spawn(move || {
        let app2 = app.clone();
        let result = build_index(budget, &move |files, chunks, current| {
            emit(
                &app2,
                json!({ "kind": "progress", "files": files, "chunks": chunks, "current": current }),
            );
        });
        INDEXING.store(false, Ordering::SeqCst);
        match result {
            Ok(s) => emit(
                &app,
                json!({
                    "kind": "done", "files": s.files_done, "chunks": s.chunks_added,
                    "pending": s.files_pending, "seconds": s.seconds, "stopped": s.stopped,
                }),
            ),
            Err(e) => emit(&app, json!({ "kind": "error", "message": e })),
        }
    });
    Ok(())
}
