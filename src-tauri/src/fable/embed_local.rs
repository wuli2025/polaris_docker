//! 本地开源嵌入 / 重排（fastembed-rs · ONNX）—— 绕开硅基流动 API 的限速与网络往返。
//!
//! 模型与硅基流动**同源**：嵌入 `BAAI/bge-m3`(dense 1024 维)、重排 `bge-reranker-v2-m3`，
//! 故本地产出的向量与既有索引「同空间、兼容」——无需重嵌全库，存量 5309 条照用，
//! 新文件本地灌、查询本地算。
//!
//! 仅 `feature = "local-embed"` 时编译；运行时还需 `POLARIS_LOCAL_EMBED=1` 才真正启用
//! （否则即便编进也走云 API，便于灰度/回退）。首次用时模型下载到 `FASTEMBED_CACHE_DIR`
//! （默认 ~/Polaris/models/fastembed），之后离线加载。
//!
//! 并发：fastembed 推理对象用 `Mutex` 单例守护（ONNX 内部已多线程，外层串行无碍且省内存）。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use fastembed::{
    EmbeddingModel, RerankInitOptions, RerankerModel, TextEmbedding, TextInitOptions, TextRerank,
};

/// 运行时开关：编进了本地能力，但默认仍走云；置 `POLARIS_LOCAL_EMBED=1` 切到本地。
pub fn enabled() -> bool {
    std::env::var("POLARIS_LOCAL_EMBED").map(|v| v == "1").unwrap_or(false)
}

/// 模型缓存目录：落数据根，避免每次重下；FASTEMBED_CACHE_DIR / HF_HOME 可覆盖。
fn ensure_cache_env() {
    if std::env::var_os("FASTEMBED_CACHE_DIR").is_none() {
        let dir = directories::UserDirs::new()
            .map(|u| u.home_dir().join("Polaris").join("models").join("fastembed"))
            .unwrap_or_else(|| PathBuf::from("/root/Polaris/models/fastembed"));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("FASTEMBED_CACHE_DIR", dir);
    }
    cap_onnx_threads();
}

/// 限速旋钮：`POLARIS_EMBED_THREADS` 限制 ONNX Runtime 的推理线程数。本地 bge-m3 嵌入/重排是
/// CPU 密集，ORT 默认会吃满全部逻辑核 → 百万级大库后台索引连续跑时，UI 线程几乎拿不到 CPU
/// 时间片，消息泵错过 5s 窗口被 Windows 判无响应(AppHangB1)。把它设成「核数 - 2」之类给 UI
/// 留头寸。必须在模型(ORT session)创建**之前**设进环境；OpenMP 构建的 onnxruntime 读
/// `OMP_NUM_THREADS`，故同时写入这两个变量(已被显式设过则尊重用户值，不覆盖)。
fn cap_onnx_threads() {
    let Ok(v) = std::env::var("POLARIS_EMBED_THREADS") else { return };
    let Ok(n) = v.trim().parse::<usize>() else { return };
    if n == 0 {
        return;
    }
    let n = n.to_string();
    if std::env::var_os("OMP_NUM_THREADS").is_none() {
        std::env::set_var("OMP_NUM_THREADS", &n);
    }
    if std::env::var_os("ORT_INTRA_OP_NUM_THREADS").is_none() {
        std::env::set_var("ORT_INTRA_OP_NUM_THREADS", &n);
    }
}

static EMBED: OnceLock<Result<Mutex<TextEmbedding>, String>> = OnceLock::new();
static RERANK: OnceLock<Result<Mutex<TextRerank>, String>> = OnceLock::new();

fn embedder() -> Result<&'static Mutex<TextEmbedding>, String> {
    EMBED
        .get_or_init(|| {
            ensure_cache_env();
            TextEmbedding::try_new(TextInitOptions::new(EmbeddingModel::BGEM3))
                .map(Mutex::new)
                .map_err(|e| format!("本地嵌入模型加载失败(BAAI/bge-m3): {e}"))
        })
        .as_ref()
        .map_err(|e| e.clone())
}

fn reranker() -> Result<&'static Mutex<TextRerank>, String> {
    RERANK
        .get_or_init(|| {
            ensure_cache_env();
            TextRerank::try_new(RerankInitOptions::new(RerankerModel::BGERerankerV2M3))
                .map(Mutex::new)
                .map_err(|e| format!("本地重排模型加载失败(bge-reranker-v2-m3): {e}"))
        })
        .as_ref()
        .map_err(|e| e.clone())
}

/// 批量本地嵌入 → 与 `index::embed_texts` 同形(Vec<Vec<f32>>，dense 1024)。
pub fn embed(texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
    let m = embedder()?;
    let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
    let mut g = m.lock().map_err(|_| "本地嵌入锁中毒".to_string())?;
    g.embed(refs, None).map_err(|e| format!("本地嵌入失败: {e}"))
}

/// 本地重排 → 与 `index::rerank` 同形((原 index, 分数) 降序,截到 top_n)。
pub fn rerank(query: &str, docs: &[String], top_n: usize) -> Result<Vec<(usize, f32)>, String> {
    let m = reranker()?;
    let refs: Vec<&str> = docs.iter().map(|s| s.as_str()).collect();
    let mut g = m.lock().map_err(|_| "本地重排锁中毒".to_string())?;
    let res = g
        .rerank(query, refs, false, None)
        .map_err(|e| format!("本地重排失败: {e}"))?;
    let mut out: Vec<(usize, f32)> = res.iter().map(|r| (r.index, r.score)).collect();
    out.truncate(top_n);
    Ok(out)
}
