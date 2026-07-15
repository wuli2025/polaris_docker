//! 本地开源嵌入(fastembed-rs · ONNX)—— 绕开硅基流动 API 的限速与网络往返(治吞吐 35/秒天花板)。
//!
//! 嵌入模型 = **BGE-M3 INT8 单文件 ONNX**(`gpahal/bge-m3-onnx-int8` 的 `model_quantized.onnx`,
//! ~543MB),与硅基 `BAAI/bge-m3` 同源 → dense 1024 维、同空间兼容既有索引,无需重嵌全库。
//! 用 INT8 单文件而非 fp32(`BAAI/bge-m3` 的 `onnx/model.onnx`+2.2GB 外置 `model.onnx_data`):
//!  ① 体积砍半、下载快;② **单文件无外置权重** → 可纯内存加载(`commit_from_memory`)。
//!
//! **下载为何自己来、不交给 fastembed**:fastembed 内部用 hf-hub(Rust)拉模型,而 hf-hub 对
//! HuggingFace 专有响应头(x-linked-etag / x-repo-commit)有强依赖,**国内镜像 hf-mirror 不回这些头
//! → hf-hub 秒失败、一个字节都下不到**(实测 2026-06-26)。故这里改成自己用 ureq 直连
//! `resolve/main/<file>` 流式下载(镜像就是普通文件服务,直连必中),落地后用 fastembed 的
//! `try_new_from_user_defined` 从文件字节加载 —— 彻底绕开 hf-hub 那套头依赖。
//!
//! 仅 `feature = "local-embed"` 时编译;运行时还需 `POLARIS_LOCAL_EMBED=1` 或 UI 勾「启用本地嵌入」
//! 才真正启用(否则即便编进也走云 API,便于灰度/回退)。重排暂仍走云(本地重排未接,见 `rerank_ready`)。
//!
//! 平台:本地引擎(fastembed 的 onnxruntime)与桌面语音(sherpa 的 onnxruntime)互斥,故带 voice 的
//! Windows 桌面发版不编入;Docker/NAS 与 `--features local-embed` 构建全功能。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use fastembed::{Bgem3Embedding, InitOptionsUserDefined, TokenizerFiles, UserDefinedBgem3Model};

/// 嵌入模型仓库与文件清单(INT8 单文件 + 分词器四件套)。镜像在前、官方兜底。
const HF_REPO: &str = "gpahal/bge-m3-onnx-int8";
const HF_BASES: &[&str] = &["https://hf-mirror.com", "https://huggingface.co"];
/// (文件名, 约定大小 MB):大小用于半截校验(下到的字节远小于约定 → 判镜像错误页)。
const MODEL_FILES: &[(&str, f64)] = &[
    ("model_quantized.onnx", 543.0),
    ("tokenizer.json", 16.0),
    ("config.json", 0.0),
    ("special_tokens_map.json", 0.0),
    ("tokenizer_config.json", 0.0),
];

/// 启用标记文件:UI「启用本地嵌入」开关落盘于此 → 重启仍生效(不必每次设环境变量)。
fn enable_marker() -> Option<PathBuf> {
    directories::UserDirs::new().map(|u| {
        u.home_dir()
            .join("Polaris")
            .join("data")
            .join("local_embed.on")
    })
}

/// 运行时开关:置 `POLARIS_LOCAL_EMBED=1` **或** UI 勾了「启用本地嵌入」(落盘标记)即切到本地;
/// 否则仍走云 API(便于灰度/回退)。环境变量优先(可被运维强制)。
pub fn enabled() -> bool {
    if std::env::var("POLARIS_LOCAL_EMBED")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return true;
    }
    enable_marker().map(|p| p.exists()).unwrap_or(false)
}

/// 持久化「启用本地嵌入」开关(供 UI 切换)。写/删标记文件。
pub fn set_enabled(on: bool) -> Result<(), String> {
    let p = enable_marker().ok_or("无法定位数据目录")?;
    if on {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("建目录失败: {e}"))?;
        }
        std::fs::write(&p, b"1").map_err(|e| format!("写启用标记失败: {e}"))?;
    } else if p.exists() {
        std::fs::remove_file(&p).map_err(|e| format!("清启用标记失败: {e}"))?;
    }
    Ok(())
}

/// 模型根目录:`FASTEMBED_CACHE_DIR` 覆盖,默认 `~/Polaris/models/fastembed`。
pub fn cache_dir() -> PathBuf {
    if let Some(v) = std::env::var_os("FASTEMBED_CACHE_DIR") {
        return PathBuf::from(v);
    }
    directories::UserDirs::new()
        .map(|u| {
            u.home_dir()
                .join("Polaris")
                .join("models")
                .join("fastembed")
        })
        .unwrap_or_else(|| PathBuf::from("/root/Polaris/models/fastembed"))
}

/// 嵌入模型落地子目录。
fn model_dir() -> PathBuf {
    cache_dir().join("bge-m3-int8")
}

/// 嵌入模型是否已下载就位:清单里每个文件都存在且非空。
pub fn ready() -> bool {
    let dir = model_dir();
    MODEL_FILES
        .iter()
        .all(|(f, _)| dir.join(f).metadata().map(|m| m.len() > 0).unwrap_or(false))
}

/// 本地**重排**是否就位。当前未接本地重排模型 → 恒 false,重排继续走云(保排序质量);
/// 这样「启用本地嵌入」只切嵌入、不连累重排。后续要接本地重排时在此返回真实就位状态即可。
pub fn rerank_ready() -> bool {
    false
}

/// ONNX 推理 intra-op 线程数(`POLARIS_EMBED_THREADS`,0/未设 = 用满核)。给 UI 留头寸防 AppHang。
fn embed_threads() -> Option<usize> {
    std::env::var("POLARIS_EMBED_THREADS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
}

/// 流式下载单文件到 `dst`(先写 .part 再改名)。镜像在前、官方兜底;半截/错误页判废试下一源。
fn fetch_file(file: &str, dst: &std::path::Path, approx_mb: f64) -> Result<(), String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(300))
        .build();
    let part = dst.with_extension("part");
    let mut last_err = String::new();
    for base in HF_BASES {
        let url = format!("{base}/{HF_REPO}/resolve/main/{file}");
        let resp = match agent.get(&url).call() {
            Ok(r) => r,
            Err(e) => {
                last_err = format!("{url}: {e}");
                continue;
            }
        };
        let total: u64 = resp
            .header("content-length")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let mut reader = resp.into_reader();
        let mut out = match std::fs::File::create(&part) {
            Ok(f) => f,
            Err(e) => return Err(format!("创建临时文件失败: {e}")),
        };
        let copied = std::io::copy(&mut reader, &mut out);
        drop(out);
        match copied {
            Ok(received) => {
                // 半截校验:有 content-length 且没下满,或体量远小于约定大小 → 判镜像错误页,试下一源。
                if (total > 0 && received < total)
                    || (approx_mb > 1.0 && (received as f64) < approx_mb * 1_048_576.0 * 0.5)
                {
                    last_err = format!("{url}: 下载不完整/疑似错误页({received} 字节)");
                    let _ = std::fs::remove_file(&part);
                    continue;
                }
                std::fs::rename(&part, dst).map_err(|e| format!("落位失败: {e}"))?;
                return Ok(());
            }
            Err(e) => {
                last_err = format!("{url}: 读取中断 {e}");
                let _ = std::fs::remove_file(&part);
                continue;
            }
        }
    }
    Err(if last_err.is_empty() {
        "全部下载源不可达".into()
    } else {
        last_err
    })
}

/// 强制下载/就位本地嵌入模型(INT8 单文件 + 分词器四件套,~560MB)。幂等:已有文件跳过。
/// 供 UI「下载本地引擎」按钮调用 —— 把「报告头号提速杠杆(绕开云限速)」变成一键可下。返回模型目录。
pub fn download() -> Result<String, String> {
    let dir = model_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("建模型目录失败: {e}"))?;
    for (file, approx_mb) in MODEL_FILES {
        let dst = dir.join(file);
        if dst.metadata().map(|m| m.len() > 0).unwrap_or(false) {
            continue; // 幂等续下
        }
        fetch_file(file, &dst, *approx_mb)?;
    }
    Ok(dir.to_string_lossy().into_owned())
}

static EMBED: OnceLock<Mutex<Bgem3Embedding>> = OnceLock::new();
/// 初始化串行锁:并发首调只让一个线程加载 ~560MB 模型,其余等它的结果。
static EMBED_INIT: Mutex<()> = Mutex::new(());

fn embedder() -> Result<&'static Mutex<Bgem3Embedding>, String> {
    if let Some(m) = EMBED.get() {
        return Ok(m);
    }
    // 失败**不缓存**:模型未下载时首调报错,用户点完「下载本地引擎」后无需重启即可
    // 重试成功。(旧实现把 Err 永久存进 OnceLock,下载完也要重启进程才生效。)
    // 初始化 panic(如 ORT 内部断言)会毒化 std Mutex;这里只当串行闸用,没有共享
    // 状态被撕坏的问题,into_inner 恢复继续,别把一次 panic 变成永久「锁中毒」。
    let _g = EMBED_INIT.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(m) = EMBED.get() {
        return Ok(m); // 排队期间别的线程已加载成功
    }
    let dir = model_dir();
    let read = |f: &str| -> Result<Vec<u8>, String> {
        std::fs::read(dir.join(f)).map_err(|e| {
            format!("读本地模型 {f} 失败(请先在「寓言计划」点「下载本地引擎」): {e}")
        })
    };
    let onnx = read("model_quantized.onnx")?;
    let tokenizer_files = TokenizerFiles {
        tokenizer_file: read("tokenizer.json")?,
        config_file: read("config.json")?,
        special_tokens_map_file: read("special_tokens_map.json")?,
        tokenizer_config_file: read("tokenizer_config.json")?,
    };
    let model = UserDefinedBgem3Model::new(onnx, tokenizer_files);
    let mut opts = InitOptionsUserDefined::new();
    // 检索用 512 token 足矣(chunk ~1600 字符≈500-800 token);**不要设 8192**——
    // 该 ONNX 导出按 max_length 定形,设大会把每条短文档撑到满长度,colbert 输出
    // [batch,seq,1024] 爆炸式增大 → 推理实质卡死(实测 8192 时 0 CPU 挂住)。
    opts.max_length = 512;
    opts.intra_threads = embed_threads();
    let engine = Bgem3Embedding::try_new_from_user_defined(model, opts)
        .map_err(|e| format!("本地嵌入模型加载失败(bge-m3-int8): {e}"))?;
    let _ = EMBED.set(Mutex::new(engine));
    Ok(EMBED.get().expect("EMBED 刚 set 必在"))
}

/// 批量本地嵌入 → 与 `index::embed_texts` 同形(Vec<Vec<f32>>,dense 1024)。
/// 取 BGE-M3 联合输出里的 dense 腿(sparse/colbert 暂不入库)。
pub fn embed(texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
    let m = embedder()?;
    let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
    let mut g = m.lock().map_err(|_| "本地嵌入锁中毒".to_string())?;
    // fastembed 内部推理批:`POLARIS_EMBED_BATCH` 覆盖(clamp [1,64],默认 32)。
    // 本地 INT8 是 CPU 密集且 [batch,seq=512,1024] 张量随 batch 线性涨内存,故上限比云档(128)保守。
    let inner_batch = std::env::var("POLARIS_EMBED_BATCH")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .map(|n| n.clamp(1, 64))
        .unwrap_or(32);
    let out = g
        .embed(refs, Some(inner_batch))
        .map_err(|e| format!("本地嵌入失败: {e}"))?;
    Ok(out.dense)
}

/// 本地重排:当前未接(`rerank_ready` 恒 false,调用方不会路由到这里),保留签名占位。
/// 接本地重排模型时在此实现并让 `rerank_ready` 反映就位状态即可。
pub fn rerank(_query: &str, _docs: &[String], _top_n: usize) -> Result<Vec<(usize, f32)>, String> {
    Err("本地重排未接入(重排走云)".into())
}

#[cfg(test)]
mod bench {
    use super::*;
    use std::time::Instant;

    /// 本地嵌入「一键下载 + 吞吐」端到端基准(opt-in:仅 `POLARIS_BENCH_LOCAL=1` 真跑)。
    #[test]
    fn local_embed_download_and_throughput() {
        if std::env::var("POLARIS_BENCH_LOCAL").as_deref() != Ok("1") {
            return;
        }
        let t0 = Instant::now();
        let dir = download().expect("下载本地模型失败");
        eprintln!(
            "[本地嵌入] 模型就位 {:.1}s @ {dir}",
            t0.elapsed().as_secs_f64()
        );
        assert!(ready());
        let n = 96usize;
        let docs: Vec<String> = (0..n)
            .map(|i| {
                ("北极星知识库混合检索把关键词与向量两腿并行编排取证 ".to_string() + &i.to_string())
                    .repeat(3)
            })
            .collect();
        let _ = embed(&docs[..16]).expect("预热失败");
        let t1 = Instant::now();
        let v = embed(&docs).expect("嵌入失败");
        let dt = t1.elapsed().as_secs_f64();
        assert_eq!(v.len(), n);
        assert_eq!(v[0].len(), 1024);
        eprintln!(
            "[本地嵌入] {n} 条 / {dt:.2}s = {:.1} chunk/s(本地 CPU,无云限速)",
            n as f64 / dt
        );
    }
}
