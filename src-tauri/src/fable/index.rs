//! 向量车道(RAG)—— 文本 chunk → 嵌入 → SQLite 向量落库。
//!
//! 嵌入/重排走感官坞(sense.rs)当前生效的服务商:默认 硅基流动 BGE-M3 /
//! bge-reranker(钥匙②,免费)。PRD v5 §2.2「嵌入主路=硅基免费,本地 ONNX 兜底后续接入」。
//!
//! 工程姿势(PRD「巡夜人/滴灌」):一次构建只消化一个预算额(默认 4000 chunk),
//! 幂等续跑 —— files.chunked 标记位,断了再点继续;429 限速指数退避。

use super::{cancelled, lex_available, open_db, FlagGuard, CANCEL, INDEXING};
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
/// 单文件多个 chunk 批的并发嵌入度。嵌入是网络往返(每批可达数百 ms~数秒),长文档会切成
/// 几十上百批,旧实现严格串行 → 总耗时 ≈ 批数 × 单批延迟。embed_texts 是纯网络调用、无共享
/// 态 → 可并发;限到 3 路兼顾吞吐与免费档限速(每批内部仍有 429 指数退避兜底)。
const EMBED_CONCURRENCY: usize = 3;

/// 实际并发嵌入度:`POLARIS_EMBED_CONCURRENCY` 可覆盖(clamp 到 [1,16])。本地 ONNX 嵌入
/// (POLARIS_LOCAL_EMBED)是 CPU 密集,冷启动满负荷会抢光核心拖垮 UI;设 `=1` 让嵌入串行、
/// 给 UI 留核(配合 POLARIS_EMBED_THREADS 限 ONNX 内部线程效果更好)。
fn embed_concurrency() -> usize {
    if let Ok(v) = std::env::var("POLARIS_EMBED_CONCURRENCY") {
        if let Ok(n) = v.trim().parse::<usize>() {
            return n.clamp(1, 16);
        }
    }
    EMBED_CONCURRENCY
}
/// 单次 build 处理的文件数上限(FTS-only 文件不耗嵌入预算,需独立护栏,幂等续跑)。
const MAX_FILES_PER_BUILD: u64 = 8000;
/// P1-4 文件类型分流:这些扩展名是大体量、低语义价值的数据/日志类,**不花钱做向量**
/// (精确查找走 FTS 倒排即可),只覆盖真有文字、真常被语义搜的「精华」。
const EMBED_SKIP_EXTS: &[&str] = &["log", "csv", "tsv", "ndjson"];

fn embeddable(ext: &str, size: i64) -> bool {
    size <= MAX_EMBED_FILE_BYTES && !EMBED_SKIP_EXTS.contains(&ext.to_ascii_lowercase().as_str())
}

// ───────────────────────── 嵌入 / 重排客户端 ─────────────────────────

/// 进程级共享 HTTP Agent。ureq::Agent 内部是 Arc + 连接池,Clone 廉价、Send+Sync,**复用同一个
/// 即可在多次请求间保活 TCP/TLS 连接**。此前每次调用都 `build()` 一个全新 Agent → 连接池形同
/// 虚设,每个嵌入批 / 每次查询嵌入都要重做一次 TLS 握手(对 siliconflow 这类 HTTPS 往返,握手
/// 本身就是几十~上百 ms)。索引构建会打成千上万批(且 EMBED_CONCURRENCY 路并发共享此池),
/// 查询冷路也复用暖连接 —— 嵌入吞吐与首字延迟同时受益。
static HTTP_AGENT: Lazy<ureq::Agent> = Lazy::new(|| {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(120))
        .build()
});

fn agent_http() -> ureq::Agent {
    HTTP_AGENT.clone()
}

/// 批量嵌入。429 退避重试 3 次;其余错误直接报(可读信息,UI 原样展示)。
pub fn embed_texts(texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
    // 本地开源嵌入(POLARIS_LOCAL_EMBED=1):绕开云 API 限速/网络往返;模型同源(bge-m3)故向量兼容。
    #[cfg(feature = "local-embed")]
    if crate::fable::embed_local::enabled() {
        return crate::fable::embed_local::embed(texts);
    }
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
    // 本地开源重排(POLARIS_LOCAL_EMBED=1):本地 bge-reranker-v2-m3,省 ~600ms API 往返。
    #[cfg(feature = "local-embed")]
    if crate::fable::embed_local::enabled() {
        return crate::fable::embed_local::rerank(query, docs, top_n);
    }
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

/// 直接在 f32 小端字节上算点积(向量均已归一化 → 即余弦),省掉 [`blob_to_vec`] 的中间
/// `Vec<f32>` 堆分配 —— 检索精排每候选省一次分配(大库一次查询数百候选)。`blob` 字节数须
/// 为 `qv.len()*4`(维度/模型一致),否则 `None`(脏数据/旧维度向量,调用方跳过)。
pub(crate) fn dot_blob(qv: &[f32], blob: &[u8]) -> Option<f32> {
    if blob.len() != qv.len() * 4 {
        return None;
    }
    let mut s = 0f32;
    for (i, q) in qv.iter().enumerate() {
        let o = i * 4;
        s += q * f32::from_le_bytes([blob[o], blob[o + 1], blob[o + 2], blob[o + 3]]);
    }
    Some(s)
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

/// 是否**具备把文本变向量的能力** —— 本地开源嵌入(local-embed)**或**云 API 嵌入服务商。
/// `active_provider("embed")` 只认 `kind=api`+有 key 的云服务商,**不计本地档**;但本地档
/// (v1.4.2,bge-m3 ONNX)离线就能产向量。渐进式「智能归类」据此决定要不要跑「全量向量化 →
/// 按内容语义重聚」——只看云 key 会让纯本地用户永远停在结构归类、永远走不到「按意思」。
pub fn embed_capable() -> bool {
    #[cfg(feature = "local-embed")]
    if crate::fable::embed_local::enabled() {
        return true;
    }
    crate::sense::active_provider("embed").is_some()
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
    let pending_sql = match (embed_ok, lex_ok) {
        (true, true) => {
            "SELECT f.id, r.path, f.relpath, f.ext, f.size, f.chunked, f.ftsed
             FROM files f JOIN roots r ON r.id=f.root_id
             WHERE f.kind='text' AND f.size<=?1 AND (f.chunked=0 OR f.ftsed=0)
             ORDER BY f.size ASC LIMIT 32"
        }
        (false, true) => {
            "SELECT f.id, r.path, f.relpath, f.ext, f.size, f.chunked, f.ftsed
             FROM files f JOIN roots r ON r.id=f.root_id
             WHERE f.kind='text' AND f.size<=?1 AND f.ftsed=0
             ORDER BY f.size ASC LIMIT 32"
        }
        _ => {
            "SELECT f.id, r.path, f.relpath, f.ext, f.size, f.chunked, f.ftsed
             FROM files f JOIN roots r ON r.id=f.root_id
             WHERE f.kind='text' AND f.size<=?1 AND f.chunked=0
             ORDER BY f.size ASC LIMIT 32"
        }
    };

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
            // 无 key 时整块跳过:chunked 保持 0,补 key 后再点构建即补建向量(认字腿已先行覆盖)。
            if embed_ok && chunked == 0 {
                if embeddable(&ext, size) && !text.is_empty() {
                    let chunks = chunk_text(&text);
                    if !chunks.is_empty() {
                        // 重嵌入前清旧 chunk(mtime 变更后 chunked 被重置的场景)
                        conn.execute("DELETE FROM chunks WHERE file_id=?1", [file_id])
                            .map_err(|e| e.to_string())?;
                        let groups: Vec<&[String]> = chunks.chunks(EMBED_BATCH).collect();
                        // ── 并发嵌入各批 ──
                        // embed_texts 纯网络调用、无共享态 → 多批可并发取证。长文档(几十上百批)
                        // 由「批数 × 单批延迟」降到「批数/并发 × 单批延迟」;小文档单批时退化为原行为。
                        // 任一批出错即整体上抛(与旧逐批 `?` 同语义):此前已 DELETE 旧 chunk、
                        // 且未标记 chunked=1,下轮重扫会重试,不留半截向量。
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
                            // 嵌入错误(断网/限速/TLS 闪断)**不再整体放弃**:几百 GB 的索引
                            // 跑几小时,一次网络抖动就清零代价太大。此前已 DELETE 旧 chunk、未标
                            // chunked=1 → 本文件留待下轮重试;优雅停在已索引处(认字腿+已成功的
                            // 向量都已逐文件提交落库),报可重试。
                            let mut embed_err: Option<String> = None;
                            for (i, r) in collected.into_inner().unwrap() {
                                match r {
                                    Ok(mut vecs) => {
                                        for v in vecs.iter_mut() {
                                            normalize(v); // P1-3:入库归一化一次 → 查询退化成纯点积
                                        }
                                        all_vecs[i] = vecs;
                                    }
                                    Err(e) => {
                                        embed_err = Some(e);
                                        break;
                                    }
                                }
                            }
                            if let Some(e) = embed_err {
                                stopped = format!("嵌入中断(可再点继续补建向量):{e}");
                                break 'outer;
                            }
                        }
                        // 写库:整文件单事务批量插入(seq = batch_i*EMBED_BATCH+i,顺序稳定)。
                        conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
                        {
                            let mut stmt = conn
                                .prepare_cached(
                                    "INSERT OR REPLACE INTO chunks(file_id,seq,text,dim,vec,model,bits)
                                     VALUES(?1,?2,?3,?4,?5,?6,?7)",
                                )
                                .map_err(|e| e.to_string())?;
                            for (batch_i, group) in groups.iter().enumerate() {
                                let vecs = &all_vecs[batch_i];
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
                        }
                        conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
                        chunks_added += chunks.len() as u64;
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

    // 剩余工作:与 pending_sql 同口径 —— 无 key 时只数还没进倒排(ftsed=0)的文件,
    // 别把「待补向量」(chunked=0)算成欠账,否则永远显示一堆 pending 误导用户。
    let pending_count_sql = match (embed_ok, lex_ok) {
        (true, true) => {
            "SELECT COUNT(*) FROM files WHERE kind='text' AND size<=?1 AND (chunked=0 OR ftsed=0)"
        }
        (false, true) => "SELECT COUNT(*) FROM files WHERE kind='text' AND size<=?1 AND ftsed=0",
        _ => "SELECT COUNT(*) FROM files WHERE kind='text' AND size<=?1 AND chunked=0",
    };
    let files_pending: i64 = conn
        .query_row(pending_count_sql, [MAX_LEX_FILE_BYTES], |r| r.get(0))
        .unwrap_or(0);
    // 首次跨过规模门槛时自动建一次 IVF 倒排单元(20TB 级检索开箱即亚秒);未取消、有向量可聚类时才做。
    if !cancelled() && embed_ok {
        maybe_optimize(&conn, &model);
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
        let s = build_index(200_000, &|f, c, _cur| progress(total_files + f, total_chunks + c, 0))?;
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
                break format!("剩 {} 个文件无法嵌入(超限/不可读),已尽力完成", s.files_pending);
            }
        } else {
            stalled = 0;
        }
        last_pending = s.files_pending;
    };
    Ok(IndexSummary {
        files_done: total_files,
        chunks_added: total_chunks,
        files_pending: 0,
        seconds: started.elapsed().as_secs_f64(),
        stopped,
    })
}

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
    let mut centroids: Vec<Vec<u8>> =
        (0..k).map(|i| sample[(i * stride).min(sample.len() - 1)].clone()).collect();
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
    let model = active_embed_model()
        .ok_or("没有可用的嵌入服务商,无法确定向量模型")?;
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

    // 落质心:先清本模型旧 cell,再插入新质心并记下各自 rowid。
    conn.execute("DELETE FROM vec_cells WHERE model=?1", [&model]).map_err(|e| e.to_string())?;
    let mut cell_ids: Vec<i64> = Vec::with_capacity(centroids.len());
    {
        conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
        {
            let mut ins = conn
                .prepare_cached("INSERT INTO vec_cells(model, dim, bits, n) VALUES(?1,?2,?3,0)")
                .map_err(|e| e.to_string())?;
            for c in &centroids {
                ins.execute(rusqlite::params![model, dim, c]).map_err(|e| e.to_string())?;
                cell_ids.push(conn.last_insert_rowid());
            }
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
                up.execute(rusqlite::params![id, cell_ids[ci]]).map_err(|e| e.to_string())?;
            }
        }
        conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
        assigned += batch.len() as u64;
    }

    Ok(OptimizeSummary {
        model,
        chunks: n as u64,
        cells: cell_ids.len() as u64,
        assigned,
        seconds: started.elapsed().as_secs_f64(),
        note: if cancelled() { "已取消(部分分配)".into() } else { "完成".into() },
    })
}

/// 构建尾声的自动维护:首次跨过 IVF 门槛(尚无 cell)时顺带建一次倒排单元,
/// 让向量车道在大规模下「开箱即亚秒」,无需用户手动点优化。已有 cell 后的增量
/// 维护(随新数据增长重训)交给显式 `fable_index_optimize`/CLI `fable optimize`(巡夜)。
fn maybe_optimize(conn: &rusqlite::Connection, model: &str) {
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
        .query_row("SELECT COUNT(*) FROM vec_cells WHERE model=?1", [model], |r| r.get(0))
        .unwrap_or(0);
    if cells == 0 {
        let _ = optimize_vectors(); // 首次自动建;失败不影响构建结果(检索退回全扫)
    }
}

// ───────────────────────── 命令(后台线程 + 事件)─────────────────────────

fn emit(app: &AppHandle, payload: Value) {
    let _ = app.emit("fable:index", payload);
}

/// 开始(或继续)构建向量索引。立即返回,进度走 `fable:index` 事件。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fable_index_start(app: AppHandle, max_chunks: Option<usize>) -> Result<(), String> {
    let Some(index_guard) = FlagGuard::acquire(&INDEXING) else {
        return Err("索引构建已在进行中".into());
    };
    CANCEL.store(false, Ordering::SeqCst);
    let budget = max_chunks.unwrap_or(4000).clamp(100, 200_000);
    std::thread::spawn(move || {
        // 守卫 move 进线程:正常结束或 panic 栈展开都会释放 INDEXING 闸(防永久锁死)。
        let _index_guard = index_guard;
        let app2 = app.clone();
        let result = build_index(budget, &move |files, chunks, current| {
            emit(
                &app2,
                json!({ "kind": "progress", "files": files, "chunks": chunks, "current": current }),
            );
        });
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

/// 重建向量 IVF 倒排单元(20TB 级 ANN 的「优化/建索引」步)。返回汇总;
/// 与构建/盘点共用 INDEXING 闸(进行中则拒绝),用 RAII 守卫确保 panic 也释放闸。
///
/// 桌面端一律 async + spawn_blocking:optimize_vectors() 要跑二值 k-means(最多 8 轮、
/// 采样上 10 万行)再全表分批 UPDATE chunks.cell,中库(5000 万 chunk)可达 5~15s。直接当
/// 同步 Tauri 命令会在 WebView 主线程上跑 → 阻塞 >5s 被 Windows 判「无响应」强杀(AppHangB1)。
/// server flavor 无 UI 主线程、dispatch 本就在 spawn_blocking 中,保持同步直调即可。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn fable_index_optimize() -> Result<OptimizeSummary, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let Some(_guard) = FlagGuard::acquire(&INDEXING) else {
            return Err("索引任务进行中,稍后再优化".into());
        };
        CANCEL.store(false, Ordering::SeqCst);
        optimize_vectors()
    })
    .await
    .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
pub fn fable_index_optimize() -> Result<OptimizeSummary, String> {
    let Some(_guard) = FlagGuard::acquire(&INDEXING) else {
        return Err("索引任务进行中,稍后再优化".into());
    };
    CANCEL.store(false, Ordering::SeqCst);
    optimize_vectors()
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
    fn dot_blob_matches_blob_to_vec_path() {
        // dot_blob 必须与「blob_to_vec 后逐元素相乘求和」逐位一致(这是它替换的旧路径)。
        let qv = [0.1f32, -0.2, 0.3, 0.5, -0.7];
        let dv = [0.4f32, 0.4, -0.1, 0.2, 0.9];
        let blob = vec_to_blob(&dv);
        let want: f32 = blob_to_vec(&blob).iter().zip(qv.iter()).map(|(a, b)| a * b).sum();
        let got = dot_blob(&qv, &blob).expect("维度一致应返回 Some");
        assert!((got - want).abs() < 1e-6, "got={got} want={want}");
        // 维度不符(脏数据/旧维度向量)→ None,调用方跳过而非误算。
        assert!(dot_blob(&qv, &vec_to_blob(&[1.0f32, 2.0])).is_none());
        assert!(dot_blob(&qv, &blob[..blob.len() - 1]).is_none()); // 截断字节
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
