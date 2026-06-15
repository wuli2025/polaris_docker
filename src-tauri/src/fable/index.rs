//! 向量车道(RAG)—— 文本 chunk → 嵌入 → SQLite 向量落库。
//!
//! 嵌入/重排走感官坞(sense.rs)当前生效的服务商:默认 硅基流动 BGE-M3 /
//! bge-reranker(钥匙②,免费)。PRD v5 §2.2「嵌入主路=硅基免费,本地 ONNX 兜底后续接入」。
//!
//! 工程姿势(PRD「巡夜人/滴灌」):一次构建只消化一个预算额(默认 4000 chunk),
//! 幂等续跑 —— files.chunked 标记位,断了再点继续;429 限速指数退避。

use super::{cancelled, lex_available, open_db, CANCEL, INDEXING};
use once_cell::sync::Lazy;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::Duration;

#[cfg(feature = "desktop")]
use tauri::{AppHandle, Emitter};
#[cfg(not(feature = "desktop"))]
use crate::host::AppHandle;

/// 单文件参与嵌入的大小上限(超大文本不嵌入,但仍进 FTS 全文倒排,靠 lex 兜底)。
const MAX_EMBED_FILE_BYTES: i64 = 2_000_000;
/// 单文件进 FTS 倒排的大小上限(覆盖比嵌入更广的文本文件)。
const MAX_LEX_FILE_BYTES: i64 = 4_000_000;
/// 单文件 chunk 上限(P0-2 修:原 64 段≈100KB 后**静默截断**,长书/长 PDF 后 90% 召回黑洞;
/// 抬到 2000 段——在 2MB 嵌入上限内任何文件都能整篇入向量,不再悄悄丢内容;真超大文件由
/// FTS 倒排覆盖,二者合起来保证「该召回的都召回」)。
const MAX_CHUNKS_PER_FILE: usize = 2000;
/// 每请求批量条数(硅基免费档友好值)。
const EMBED_BATCH: usize = 16;
/// 单次 build 处理的文件数上限(FTS-only 文件不耗嵌入预算,需独立护栏,幂等续跑)。
const MAX_FILES_PER_BUILD: u64 = 8000;
/// P1-4 文件类型分流:这些扩展名是大体量、低语义价值的数据/日志类,**不花钱做向量**
/// (精确查找走 FTS 倒排即可),只覆盖真有文字、真常被语义搜的「精华」。
const EMBED_SKIP_EXTS: &[&str] = &["log", "csv", "tsv", "ndjson"];

fn embeddable(ext: &str, size: i64) -> bool {
    size <= MAX_EMBED_FILE_BYTES && !EMBED_SKIP_EXTS.contains(&ext.to_ascii_lowercase().as_str())
}

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

// ───────────────────────── 归一化 / 二值量化(P1-3 / P1-1)─────────────────────────

/// L2 归一化(就地)。入库前归一化一次 → 查询余弦退化成纯点积,省掉「每查询给每个向量现算模长」。
pub(crate) fn normalize(v: &mut [f32]) {
    let n = (v.iter().map(|x| x * x).sum::<f32>()).sqrt();
    if n > 1e-12 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
}

/// 符号位打包成二值码(dim 位 → ⌈dim/8⌉ 字节)。两段式 ANN 第一段用它算汉明距离做角度粗筛,
/// 读量只有 f32 的 1/32。归一化向量上,汉明距离与角度强相关 → 粗筛召回有保证。
pub(crate) fn bits_of(v: &[f32]) -> Vec<u8> {
    let mut out = vec![0u8; v.len().div_ceil(8)];
    for (i, &x) in v.iter().enumerate() {
        if x >= 0.0 {
            out[i / 8] |= 1 << (i % 8);
        }
    }
    out
}

/// 两个等长二值码的汉明距离(位不同的个数)。
pub(crate) fn hamming(a: &[u8], b: &[u8]) -> u32 {
    a.iter().zip(b.iter()).map(|(x, y)| (x ^ y).count_ones()).sum()
}

/// 当前生效的嵌入模型标识(= provider.default_model)。用于 P2-2 版本隔离与查询缓存键。
pub fn active_embed_model() -> Option<String> {
    crate::sense::active_provider("embed").map(|p| p.default_model)
}

// ───────────────────────── 查询嵌入缓存(P1-5)─────────────────────────

/// 极简 LRU:HashMap 存值 + VecDeque 记最近使用顺序。容量满时淘汰最久未用。
struct QueryCache {
    cap: usize,
    map: HashMap<String, Vec<f32>>,
    order: VecDeque<String>,
}
impl QueryCache {
    fn get(&mut self, k: &str) -> Option<Vec<f32>> {
        let v = self.map.get(k)?.clone();
        self.order.retain(|x| x != k);
        self.order.push_back(k.to_string());
        Some(v)
    }
    fn put(&mut self, k: String, v: Vec<f32>) {
        if self.map.insert(k.clone(), v).is_none() {
            self.order.push_back(k);
            while self.order.len() > self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
        } else {
            self.order.retain(|x| x != &k);
            self.order.push_back(k);
        }
    }
}
static QUERY_CACHE: Lazy<Mutex<QueryCache>> = Lazy::new(|| {
    Mutex::new(QueryCache { cap: 256, map: HashMap::new(), order: VecDeque::new() })
});

/// 查询嵌入(P1-5):LRU 缓存命中直接返回**归一化**向量(高并发下重复查询零接口开销);
/// 未命中才打一次嵌入接口。失败上抛 —— 调用方按可降级处理(向量腿静默退场,grep/FTS 腿照常)。
pub fn embed_query(query: &str) -> Result<Vec<f32>, String> {
    let model = active_embed_model().unwrap_or_default();
    let key = format!("{model}\u{0}{query}");
    if let Some(v) = QUERY_CACHE.lock().unwrap().get(&key) {
        return Ok(v);
    }
    let mut v = embed_texts(&[query.to_string()])?
        .into_iter()
        .next()
        .ok_or("查询嵌入为空")?;
    normalize(&mut v);
    QUERY_CACHE.lock().unwrap().put(key, v.clone());
    Ok(v)
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
    let model = active_embed_model().unwrap_or_default(); // P2-2:落到每个 chunk 上做版本隔离
    let lex_ok = lex_available(&conn); // P1-2:FTS5 就绪才走倒排,否则只做向量(实时扫描兜全文)
    let mut files_done = 0u64;
    let mut chunks_added = 0u64;
    let mut stopped = "全部完成".to_string();

    // pending 文件 = 还要嵌入(chunked=0)或还要进倒排(ftsed=0)的文本文件。
    // lex 不可用时去掉 ftsed 条件,免得永远选中标记不掉、空转。
    let pending_sql = if lex_ok {
        "SELECT f.id, r.path, f.relpath, f.ext, f.size, f.chunked, f.ftsed
         FROM files f JOIN roots r ON r.id=f.root_id
         WHERE f.kind='text' AND f.size<=?1 AND (f.chunked=0 OR f.ftsed=0)
         ORDER BY f.size ASC LIMIT 32"
    } else {
        "SELECT f.id, r.path, f.relpath, f.ext, f.size, f.chunked, f.ftsed
         FROM files f JOIN roots r ON r.id=f.root_id
         WHERE f.kind='text' AND f.size<=?1 AND f.chunked=0
         ORDER BY f.size ASC LIMIT 32"
    };

    loop {
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
        if batch.is_empty() {
            break;
        }
        for (file_id, root, rel, ext, size, chunked, ftsed) in batch {
            if cancelled()
                || chunks_added >= max_chunks as u64
                || files_done >= MAX_FILES_PER_BUILD
            {
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

            // ── P1-2 全文倒排(认字腿):提前建好,查词秒回、覆盖全部文本文件 ──
            if lex_ok && ftsed == 0 {
                conn.execute("DELETE FROM lex WHERE rowid=?1", [file_id])
                    .map_err(|e| e.to_string())?;
                if !text.is_empty() {
                    conn.execute(
                        "INSERT INTO lex(rowid, body) VALUES(?1, ?2)",
                        rusqlite::params![file_id, text],
                    )
                    .map_err(|e| e.to_string())?;
                }
                conn.execute("UPDATE files SET ftsed=1 WHERE id=?1", [file_id])
                    .map_err(|e| e.to_string())?;
            }

            // ── 向量层(认意思腿):P1-4 只覆盖「精华」文本(按类型/大小分流)──
            if chunked == 0 {
                if embeddable(&ext, size) && !text.is_empty() {
                    let chunks = chunk_text(&text);
                    if !chunks.is_empty() {
                        // 重嵌入前清旧 chunk(mtime 变更后 chunked 被重置的场景)
                        conn.execute("DELETE FROM chunks WHERE file_id=?1", [file_id])
                            .map_err(|e| e.to_string())?;
                        for (batch_i, group) in chunks.chunks(EMBED_BATCH).enumerate() {
                            let mut vecs = embed_texts(group)?;
                            for v in vecs.iter_mut() {
                                normalize(v); // P1-3:入库归一化一次 → 查询退化成纯点积
                            }
                            conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
                            {
                                let mut stmt = conn
                                    .prepare_cached(
                                        "INSERT OR REPLACE INTO chunks(file_id,seq,text,dim,vec,model,bits)
                                         VALUES(?1,?2,?3,?4,?5,?6,?7)",
                                    )
                                    .map_err(|e| e.to_string())?;
                                for (i, (t, v)) in group.iter().zip(vecs.iter()).enumerate() {
                                    stmt.execute(rusqlite::params![
                                        file_id,
                                        (batch_i * EMBED_BATCH + i) as i64,
                                        t,
                                        v.len() as i64,
                                        vec_to_blob(v),
                                        model,        // P2-2 版本隔离
                                        bits_of(v),   // P1-1 二值粗筛位
                                    ])
                                    .map_err(|e| e.to_string())?;
                                }
                            }
                            conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
                            chunks_added += group.len() as u64;
                        }
                    }
                }
                // 不论是否嵌入(被分流跳过的也算「向量决策已完成」),标记 chunked=1 防重复选中。
                conn.execute("UPDATE files SET chunked=1 WHERE id=?1", [file_id])
                    .map_err(|e| e.to_string())?;
            }
            files_done += 1;
            progress(files_done, chunks_added, &rel);
        }
    }

    // 剩余工作 = 还要嵌入(chunked=0)或还要进倒排(ftsed=0,仅 lex 可用时)的文本文件。
    let pending_count_sql = if lex_ok {
        "SELECT COUNT(*) FROM files WHERE kind='text' AND size<=?1 AND (chunked=0 OR ftsed=0)"
    } else {
        "SELECT COUNT(*) FROM files WHERE kind='text' AND size<=?1 AND chunked=0"
    };
    let files_pending: i64 = conn
        .query_row(pending_count_sql, [MAX_LEX_FILE_BYTES], |r| r.get(0))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_makes_unit_length() {
        let mut v = vec![3.0f32, 4.0];
        normalize(&mut v);
        let n = (v.iter().map(|x| x * x).sum::<f32>()).sqrt();
        assert!((n - 1.0).abs() < 1e-6);
        // 零向量不应除零崩溃,保持全零。
        let mut z = vec![0.0f32; 4];
        normalize(&mut z);
        assert!(z.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn bits_pack_and_hamming() {
        // 符号位:正/零 → 1,负 → 0。8 维正好 1 字节。
        let v = [1.0f32, -1.0, 2.0, -3.0, 0.0, -0.1, 5.0, -9.0];
        let b = bits_of(&v); // 位:1,0,1,0,1,0,1,0 → 0b01010101 = 0x55
        assert_eq!(b.len(), 1);
        assert_eq!(b[0], 0b0101_0101);
        // 自己跟自己汉明距离 0;翻转一位 → 距离 1。
        assert_eq!(hamming(&b, &b), 0);
        let mut b2 = b.clone();
        b2[0] ^= 0b0000_0001;
        assert_eq!(hamming(&b, &b2), 1);
        // 维度非 8 的整数倍:9 维 → 2 字节。
        assert_eq!(bits_of(&[0.0f32; 9]).len(), 2);
    }

    #[test]
    fn routing_skips_data_dumps_within_size() {
        assert!(embeddable("md", 1000));
        assert!(embeddable("MD", 1000)); // 大小写不敏感
        assert!(!embeddable("log", 1000)); // 日志类不嵌入(P1-4)
        assert!(!embeddable("csv", 1000));
        assert!(!embeddable("md", MAX_EMBED_FILE_BYTES + 1)); // 超嵌入上限不嵌入
    }

    #[test]
    fn query_cache_lru_evicts_oldest() {
        let mut c = QueryCache { cap: 2, map: HashMap::new(), order: VecDeque::new() };
        c.put("a".into(), vec![1.0]);
        c.put("b".into(), vec![2.0]);
        assert!(c.get("a").is_some()); // 访问 a → a 变最近
        c.put("c".into(), vec![3.0]); // 淘汰最久未用 = b
        assert!(c.get("b").is_none());
        assert!(c.get("a").is_some());
        assert!(c.get("c").is_some());
    }
}
