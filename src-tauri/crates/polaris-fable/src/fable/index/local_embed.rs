use super::*;

// ───────────────────── 本地嵌入引擎(寓言计划「本地模型下载」)─────────────────────
//
// 报告点名的头号提速杠杆 = 启用本地 ONNX 嵌入/重排,绕开云 API 限速(35/秒 → 受本地核数限,
// 无限速天花板)+ 查询不走网络(延迟 3.2s → ~0.3s)。这三条命令把它接成 UI 一键可下/可启:
//  · status  — 此构建有没有编进本地引擎、模型下没下、开没开;
//  · download — 一键拉权重(国内走 hf-mirror);
//  · set_enabled — 持久化「启用本地嵌入」开关(重启仍生效)。
// **平台说明**:本地引擎(fastembed 的 onnxruntime)与桌面语音(sherpa 的 onnxruntime)互斥,
// 故 Windows 桌面发版(带 voice-live)不编入本引擎 → 该构建 status.compiled=false、下载返回可读
// 提示;Docker/NAS 版(不带语音)与 `--features local-embed` 构建则全功能可用。

#[derive(Debug, Clone, Serialize)]
pub struct LocalEmbedStatus {
    /// 本构建是否编入了本地嵌入引擎(local-embed feature)。
    pub compiled: bool,
    /// 模型权重是否已下载就位。
    pub ready: bool,
    /// 是否已启用本地路径(env 或 UI 落盘标记)。
    pub enabled: bool,
    /// 模型缓存目录(展示用)。
    pub dir: String,
}

/// 本地嵌入引擎状态(三壳共用,任何构建都能查 —— 没编进时如实回 compiled=false)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fable_local_embed_status() -> Result<LocalEmbedStatus, String> {
    #[cfg(feature = "local-embed")]
    {
        Ok(LocalEmbedStatus {
            compiled: true,
            ready: crate::fable::embed_local::ready(),
            enabled: crate::fable::embed_local::enabled(),
            dir: crate::fable::embed_local::cache_dir()
                .to_string_lossy()
                .into_owned(),
        })
    }
    #[cfg(not(feature = "local-embed"))]
    {
        Ok(LocalEmbedStatus {
            compiled: false,
            ready: false,
            enabled: false,
            dir: String::new(),
        })
    }
}

/// 一键下载本地模型权重。立即返回,进度走 `fable:localembed` 事件(phase/done/error)。
/// 未编入本地引擎的构建直接返回可读提示(不静默)。
#[cfg_attr(feature = "desktop", tauri::command)]
#[allow(unused_variables)]
pub fn fable_local_embed_download(app: AppHandle) -> Result<(), String> {
    #[cfg(feature = "local-embed")]
    {
        if crate::fable::embed_local::ready() {
            let _ = app.emit(
                "fable:localembed",
                json!({ "kind": "done", "message": "模型已就位" }),
            );
            return Ok(());
        }
        std::thread::spawn(move || {
            let _ = app.emit(
                "fable:localembed",
                json!({ "kind": "phase", "message": "正在下载本地模型(BGE-M3 + 重排,约 1.2GB,国内走 hf-mirror)…" }),
            );
            match crate::fable::embed_local::download() {
                Ok(dir) => {
                    let _ = app.emit(
                        "fable:localembed",
                        json!({ "kind": "done", "message": format!("本地模型已就位:{dir}") }),
                    );
                }
                Err(e) => {
                    let _ = app.emit("fable:localembed", json!({ "kind": "error", "message": e }));
                }
            }
        });
        Ok(())
    }
    #[cfg(not(feature = "local-embed"))]
    {
        Err("此版本未编入本地嵌入引擎(桌面版与本机语音引擎互斥)。本地嵌入请用 Docker/NAS 版,或用 `--features local-embed` 构建的桌面版。".into())
    }
}

/// 持久化「启用本地嵌入」开关(重启仍生效)。未编入引擎时,开启会给出可读提示。
#[cfg_attr(feature = "desktop", tauri::command)]
#[allow(unused_variables)]
pub fn fable_local_embed_set_enabled(on: bool) -> Result<LocalEmbedStatus, String> {
    #[cfg(feature = "local-embed")]
    {
        if on && !crate::fable::embed_local::ready() {
            return Err("本地模型还没下载,请先点「下载本地引擎」。".into());
        }
        crate::fable::embed_local::set_enabled(on)?;
        return fable_local_embed_status();
    }
    #[cfg(not(feature = "local-embed"))]
    {
        if on {
            return Err("此版本未编入本地嵌入引擎,无法启用(详见状态说明)。".into());
        }
        fable_local_embed_status()
    }
}
