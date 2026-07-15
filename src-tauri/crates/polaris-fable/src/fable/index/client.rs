use super::*;

/// 单文件参与嵌入的大小上限(超大文本不嵌入,但仍进 FTS 全文倒排,靠 lex 兜底)。
const MAX_EMBED_FILE_BYTES: i64 = 2_000_000;
/// 单文件进 FTS 倒排的大小上限(覆盖比嵌入更广的文本文件)。
pub(crate) const MAX_LEX_FILE_BYTES: i64 = 4_000_000;
/// 单文件 chunk 上限(P0-2 修:原 64 段≈100KB 后**静默截断**,长书/长 PDF 后 90% 召回黑洞;
/// 抬到 2000 段——在 2MB 嵌入上限内任何文件都能整篇入向量,不再悄悄丢内容;真超大文件由
/// FTS 倒排覆盖,二者合起来保证「该召回的都召回」)。
pub(crate) const MAX_CHUNKS_PER_FILE: usize = 2000;
/// 每请求批量条数默认值。原 16 偏保守;调高 = 同样的 chunk 数**更少的网络往返**,直接抬云
/// 嵌入吞吐(报告痛点:35/秒、61.9 万要 6-28h)。32 对硅基 BGE-M3 仍在安全区,故默认上调到 32。
const EMBED_BATCH_DEFAULT: usize = 32;
/// 实际每请求批量条数:`POLARIS_EMBED_BATCH` 可覆盖(clamp 到 [1,128])。云档可调大减往返;
/// 遇到 413/请求体过大再调小。本地档内部另按 [1,64] 自钳(见 embed_local),此值只定「一次交给
/// embed_texts 的文本数」。
pub(crate) fn embed_batch() -> usize {
    std::env::var("POLARIS_EMBED_BATCH")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .map(|n| n.clamp(1, 128))
        .unwrap_or(EMBED_BATCH_DEFAULT)
}

/// 跨文件凑批目标:攒够这么多 chunk 就 flush 一次(减少 API 往返)。默认 = 批宽 × 并发度,
/// 让每次 flush 正好喂满所有并发路。`POLARIS_EMBED_COALESCE=0` → 返回 1,退回「每文件独立嵌入」
/// 的旧行为(逐文件 flush,无跨文件凑批),作为一键回退开关。
pub(crate) fn embed_coalesce_target() -> usize {
    let on = std::env::var("POLARIS_EMBED_COALESCE")
        .map(|v| v.trim() != "0")
        .unwrap_or(true);
    if !on {
        return 1;
    }
    (embed_batch() * embed_concurrency()).max(1)
}
/// 单文件多个 chunk 批的并发嵌入度。嵌入是网络往返(每批可达数百 ms~数秒),长文档会切成
/// 几十上百批,旧实现严格串行 → 总耗时 ≈ 批数 × 单批延迟。embed_texts 是纯网络调用、无共享
/// 态 → 可并发;限到 3 路兼顾吞吐与免费档限速(每批内部仍有 429 指数退避兜底)。
const EMBED_CONCURRENCY: usize = 3;

/// 实际并发嵌入度:`POLARIS_EMBED_CONCURRENCY` 可覆盖(clamp 到 [1,16])。本地 ONNX 嵌入
/// (POLARIS_LOCAL_EMBED)是 CPU 密集,冷启动满负荷会抢光核心拖垮 UI;设 `=1` 让嵌入串行、
/// 给 UI 留核(配合 POLARIS_EMBED_THREADS 限 ONNX 内部线程效果更好)。
pub(crate) fn embed_concurrency() -> usize {
    if let Ok(v) = std::env::var("POLARIS_EMBED_CONCURRENCY") {
        if let Ok(n) = v.trim().parse::<usize>() {
            return n.clamp(1, 16);
        }
    }
    EMBED_CONCURRENCY
}
/// 单次 build 处理的文件数上限(FTS-only 文件不耗嵌入预算,需独立护栏,幂等续跑)。
pub(crate) const MAX_FILES_PER_BUILD: u64 = 8000;
/// P1-4 文件类型分流:这些扩展名是大体量、低语义价值的数据/日志类,**不花钱做向量**
/// (精确查找走 FTS 倒排即可),只覆盖真有文字、真常被语义搜的「精华」。
const EMBED_SKIP_EXTS: &[&str] = &["log", "csv", "tsv", "ndjson"];

pub(crate) fn embeddable(ext: &str, size: i64) -> bool {
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
    let p = crate::sense::active_provider("embed").ok_or(
        "没有可用的嵌入服务商:在「设置 › 寓言计划 API」给硅基流动填 key(免费),或检查云感官总闸",
    )?;
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
                let v: Value = r
                    .into_json()
                    .map_err(|e| format!("嵌入响应解析失败: {e}"))?;
                let data = v
                    .get("data")
                    .and_then(|d| d.as_array())
                    .ok_or("嵌入响应缺 data 数组")?;
                // OpenAI 兼容协议里 data[i].index 才是权威对应关系:服务商乱序返回时
                // 按到达顺序收集会把向量静默错配到别的 chunk(无声的语义污染)。
                // 整批统一口径:全部条目都带 index 才按 index 归位;只要有一条缺失
                // 就整批按序(逐条回退位置会与真 index 撞槽,把合法响应误判成重复)。
                // 越界/重复整批报错,宁可重试不落错库。
                let all_indexed = data.iter().all(|it| it.get("index").and_then(|x| x.as_u64()).is_some());
                let mut out: Vec<Option<Vec<f32>>> = vec![None; texts.len()];
                for (pos, item) in data.iter().enumerate() {
                    let idx = if all_indexed {
                        item.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as usize
                    } else {
                        pos
                    };
                    let emb = item
                        .get("embedding")
                        .and_then(|e| e.as_array())
                        .ok_or("嵌入响应缺 embedding")?;
                    let slot = out
                        .get_mut(idx)
                        .ok_or_else(|| format!("嵌入响应 index 越界: {idx}"))?;
                    if slot.is_some() {
                        return Err(format!("嵌入响应 index 重复: {idx}"));
                    }
                    *slot = Some(
                        emb.iter()
                            .filter_map(|x| x.as_f64())
                            .map(|x| x as f32)
                            .collect(),
                    );
                }
                let out: Vec<Vec<f32>> = out
                    .into_iter()
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| {
                        format!("嵌入条数不符: 发 {} 回 {}", texts.len(), data.len())
                    })?;
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
    // 本地开源重排:仅当本地重排模型**真就位**(rerank_ready)才走本地;否则即便启用了本地嵌入,
    // 重排仍走云 —— 这样「启用本地嵌入」只切换嵌入(治吞吐),不连累重排的排序质量。
    #[cfg(feature = "local-embed")]
    if crate::fable::embed_local::rerank_ready() {
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
    let v: Value = resp
        .into_json()
        .map_err(|e| format!("重排响应解析失败: {e}"))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_skips_data_dumps_within_size() {
        assert!(embeddable("md", 1000));
        assert!(embeddable("MD", 1000)); // 大小写不敏感
        assert!(!embeddable("log", 1000)); // 日志类不嵌入(P1-4)
        assert!(!embeddable("csv", 1000));
        assert!(!embeddable("md", MAX_EMBED_FILE_BYTES + 1)); // 超嵌入上限不嵌入
    }
}
