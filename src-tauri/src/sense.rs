//! 寓言计划 · 感官 API 坞(Sense Dock)
//!
//! 设计出处:桌面《寓言计划-PRD-v5.html》§2/§4 ——
//! - 每种「感官」(听·速览 / 听·深读 / 看 / 嵌入 / 重排 / 读·扫描件)一组服务商;
//! - 本地服务商挂「感官包」(模型文件,按需下载,Win/Mac 不随安装包分发);
//! - 云服务商带 base_url + api_key + 「获取方式」指引 + 一键探活;
//! - 两把钥匙策略:MiniMax(想/看/说,key 可复用供应商坞)+ 硅基流动(嵌/排,免费)。
//!
//! 与 provider.rs(生成模型坞)同构:JSON 落盘 `~/Polaris/data/sense.json`、
//! 原子写、内置注册表与用户改动合并(用户只持有 key/enabled/default_model 等增量)。

use directories::UserDirs;
use once_cell::sync::Lazy;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[cfg(feature = "desktop")]
use tauri::{AppHandle, Emitter};
#[cfg(not(feature = "desktop"))]
use crate::host::AppHandle;

// ───────────────────────── 数据模型 ─────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SenseModel {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub recommended: bool,
}

/// 一个感官服务商(云 API 或本地模型)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SenseProvider {
    pub id: String,
    pub name: String,
    /// asr_fast | asr_timed | vision | embed | rerank | docparse
    pub sense: String,
    /// "api" | "local"
    pub kind: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub base_url: String,
    /// 明文存本机(与供应商坞同策略:编辑便利优先);列表接口只回尾 4 位掩码。
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub default_model: String,
    #[serde(default)]
    pub models: Vec<SenseModel>,
    /// 「点击这里获取 API Key」跳转地址
    #[serde(default)]
    pub get_key_url: String,
    #[serde(default)]
    pub docs_url: String,
    /// 卡片上的一句话说明(价格/限制/亮点)
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub free: bool,
    /// 选填增强(默认折叠在兜底位,不填不报错)
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub recommended: bool,
    /// kind=local 时挂的感官包 id
    #[serde(default)]
    pub pack_id: Option<String>,
}

/// 隐私/预算开关(寓言计划「出域五件套」的开关部分)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SenseSwitches {
    /// 云感官总闸
    pub cloud_enabled: bool,
    /// 音频出域 —— ASR 已全本地化,默认永久关
    pub audio_egress: bool,
    /// 缩略图/帧出域(看图需要)
    pub image_egress: bool,
    /// 月度预算闸(人民币);0 = 只用免费档
    pub budget_monthly_cny: f64,
}

impl Default for SenseSwitches {
    fn default() -> Self {
        Self { cloud_enabled: true, audio_egress: false, image_egress: true, budget_monthly_cny: 0.0 }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SenseStore {
    #[serde(default)]
    items: Vec<SenseProvider>,
    #[serde(default)]
    switches: SenseSwitches,
}

static STORE: Lazy<RwLock<SenseStore>> = Lazy::new(|| RwLock::new(SenseStore::default()));
/// 正在下载的感官包(防双发)
static PACK_BUSY: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));

fn data_dir() -> Option<PathBuf> {
    UserDirs::new().map(|u| u.home_dir().join("Polaris").join("data"))
}
fn store_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join("sense.json"))
}
/// 感官包(模型文件)统一落 ~/Polaris/models/<pack_id>/
pub fn models_root() -> Option<PathBuf> {
    UserDirs::new().map(|u| u.home_dir().join("Polaris").join("models"))
}

fn atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

fn persist() {
    let Some(path) = store_path() else { return };
    let txt = serde_json::to_string_pretty(&*STORE.read()).unwrap_or_default();
    let _ = atomic_write(&path, &txt);
}

// ───────────────────────── 内置注册表 ─────────────────────────
// 出处:寓言计划 v5 调研(2026-06):MiniMax M3 原生多模态(想/看/说一把钥匙);
// MiniMax 无 ASR、embo-01 已实质下架、无 rerank → 听=本地免费,嵌/排=硅基免费。

fn m(id: &str, name: &str, note: &str, rec: bool) -> SenseModel {
    SenseModel { id: id.into(), name: name.into(), note: note.into(), recommended: rec }
}

fn builtin_registry() -> Vec<SenseProvider> {
    let p = |id: &str,
             name: &str,
             sense: &str,
             kind: &str,
             base: &str,
             model: &str,
             models: Vec<SenseModel>,
             get_key: &str,
             docs: &str,
             note: &str,
             free: bool,
             optional: bool,
             rec: bool,
             pack: Option<&str>| SenseProvider {
        id: id.into(),
        name: name.into(),
        sense: sense.into(),
        kind: kind.into(),
        enabled: rec, // 推荐项默认启用;选填项默认关
        base_url: base.into(),
        api_key: String::new(),
        default_model: model.into(),
        models,
        get_key_url: get_key.into(),
        docs_url: docs.into(),
        note: note.into(),
        free,
        optional,
        recommended: rec,
        pack_id: pack.map(|s| s.to_string()),
    };
    vec![
        // ── 听 · 速览(L1,不要时间戳,要快要免费)──
        p("local-sensevoice", "SenseVoice Small", "asr_fast", "local", "", "sensevoice-small-int8",
          vec![m("sensevoice-small-int8", "SenseVoice-Small int8", "中文比 Whisper 快 ~15×,CJK 强", true)],
          "", "https://github.com/FunAudioLLM/SenseVoice",
          "本地模型 · 零出域零成本;CPU 即可跑(sherpa-onnx 运行时随推理引擎接入下发)", true, false, true,
          Some("sensevoice-small")),
        p("siliconflow-asr", "硅基流动 SenseVoice", "asr_fast", "api",
          "https://api.siliconflow.cn", "FunAudioLLM/SenseVoiceSmall",
          vec![m("FunAudioLLM/SenseVoiceSmall", "SenseVoice-Small", "免费 · 无时间戳 · 单文件≤1h/50MB", true)],
          "https://cloud.siliconflow.cn/account/ak", "https://docs.siliconflow.cn",
          "免费云转写(机器太弱时的兜底);注意返回无时间戳", true, false, false, None),
        // ── 听 · 深读(L2,字级时间戳)──
        p("local-paraformer", "Paraformer-zh(字级时间戳)", "asr_timed", "local", "", "paraformer-zh-int8",
          vec![m("paraformer-zh-int8", "Paraformer-zh int8", "字级时间戳直出 · 中文 CER≈1.95% · CPU RTF≈0.03(服务器级实测)", true)],
          "", "https://k2-fsa.github.io/sherpa/onnx/pretrained_models/offline-paraformer/paraformer-models.html",
          "本地模型 · 免费无限量 · 零出域;v5 把「唯一花钱项」归零的主角", true, false, true,
          Some("paraformer-zh")),
        p("tencent-asr", "腾讯云 录音文件识别", "asr_timed", "api",
          "https://asr.tencentcloudapi.com", "16k_zh",
          vec![m("16k_zh", "录音文件识别(中文)", "句级+字级时间戳 · 大陆直连", true)],
          "https://console.cloud.tencent.com/cam/capi", "https://cloud.tencent.com/document/product/1093",
          "每月 10 小时永久免费;超量 1.75 元/h;暂不支持一键探活(签名 v3)", true, true, false, None),
        p("groq-whisper", "Groq Whisper", "asr_timed", "api",
          "https://api.groq.com/openai", "whisper-large-v3-turbo",
          vec![m("whisper-large-v3-turbo", "Whisper v3 turbo", "字+段级时间戳 · $0.04/h · 217× 实时", true)],
          "https://console.groq.com/keys", "https://console.groq.com/docs/speech-to-text",
          "需代理访问,默认不启用;免费层 2000 次/天", false, true, false, None),
        // ── 看(帧描述/打标)──
        p("minimax-vl", "MiniMax M3(看图)", "vision", "api",
          "https://api.minimaxi.com", "MiniMax-M3",
          vec![m("MiniMax-M3", "MiniMax-M3", "原生多模态收图收视频 · 1M 上下文 · ¥2.1/M 输入", true)],
          "https://platform.minimaxi.com/user-center/basic-information/interface-key",
          "https://platform.minimaxi.com/docs/api-reference/api-overview",
          "钥匙①:一个 key 看图想事两不误;留空自动复用供应商坞的 MiniMax key", false, false, true, None),
        p("zhipu-glm4v", "智谱 GLM-4V-Flash", "vision", "api",
          "https://open.bigmodel.cn/api/paas/v4", "glm-4v-flash",
          vec![m("glm-4v-flash", "GLM-4V-Flash", "官方永久免费 · 并发低适合滴灌", true)],
          "https://open.bigmodel.cn/usercenter/apikeys", "https://docs.bigmodel.cn",
          "免费看图兜底(慢但零成本)", true, true, false, None),
        // ── 嵌入 ──
        p("siliconflow-embed", "硅基流动 BGE-M3", "embed", "api",
          "https://api.siliconflow.cn", "BAAI/bge-m3",
          vec![m("BAAI/bge-m3", "BGE-M3", "免费 · 8192 上下文 · 中文检索强", true)],
          "https://cloud.siliconflow.cn/account/ak", "https://docs.siliconflow.cn",
          "钥匙②:免费嵌入主路(本地 ONNX 兜底后续接入)", true, false, true, None),
        // ── 重排 ──
        p("siliconflow-rerank", "硅基流动 bge-reranker", "rerank", "api",
          "https://api.siliconflow.cn", "BAAI/bge-reranker-v2-m3",
          vec![m("BAAI/bge-reranker-v2-m3", "bge-reranker-v2-m3", "免费 · 中文强 · 单次前向", true)],
          "https://cloud.siliconflow.cn/account/ak", "https://docs.siliconflow.cn",
          "钥匙②:免费重排主路", true, false, true, None),
        // ── 读 · 扫描件→md ──
        p("mineru", "MinerU 在线解析", "docparse", "api",
          "https://mineru.net/api/v4", "mineru",
          vec![m("mineru", "MinerU", "中文复杂版面最优(OmniDocBench 95%+)", true)],
          "https://mineru.net/apiManage", "https://mineru.net",
          "免费额度(申请制);暂不支持一键探活", true, true, false, None),
        p("doc2x", "Doc2X", "docparse", "api",
          "https://v2.doc2x.noedgeai.com", "doc2x",
          vec![m("doc2x", "Doc2X", "≈0.01 元/页", false)],
          "https://doc2x.noedgeai.com", "https://doc2x.noedgeai.com",
          "付费便宜档(选填);暂不支持一键探活", false, true, false, None),
    ]
}

/// 感官分组的展示元数据(顺序即页面顺序)。
const SENSE_GROUPS: &[(&str, &str, &str)] = &[
    ("asr_fast", "听 · 速览转写", "L1 身份卡用:快、免费、不要时间戳"),
    ("asr_timed", "听 · 深读转写(字级时间戳)", "L2 全文转写:时间码定位回放;本地 Paraformer 免费直出"),
    ("vision", "看 · 视觉理解", "无声视频关键帧描述/照片打标;采样件出域,原文件永不出域"),
    ("embed", "嵌入", "混合检索的向量腿"),
    ("rerank", "重排", "检索精排"),
    ("docparse", "读 · 扫描件解析", "扫描 PDF/图片文档 → Markdown(LLMWiki 进料口)"),
];

// ───────────────────────── 感官包(本地模型下载)─────────────────────────
// Win/Mac 不随安装包分发模型 —— 用户在设置页按需下载;Docker full 镜像可预置
// (docker/sense-models.sh 用同一份 URL 清单)。下载源 hf-mirror 优先(国内直连)。

pub struct SensePackFile {
    pub name: &'static str,
    /// 依序尝试(镜像在前)
    pub urls: &'static [&'static str],
    pub approx_mb: f64,
}

pub struct SensePack {
    pub id: &'static str,
    pub name: &'static str,
    pub files: &'static [SensePackFile],
    pub note: &'static str,
}

pub const SENSE_PACKS: &[SensePack] = &[
    SensePack {
        id: "sensevoice-small",
        name: "SenseVoice-Small int8(听 · 速览)",
        note: "多语种快转写,中文比 Whisper 快 ~15×;sherpa-onnx 格式",
        files: &[
            SensePackFile {
                name: "model.int8.onnx",
                urls: &[
                    "https://hf-mirror.com/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/resolve/main/model.int8.onnx",
                    "https://huggingface.co/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/resolve/main/model.int8.onnx",
                ],
                approx_mb: 239.0,
            },
            SensePackFile {
                name: "tokens.txt",
                urls: &[
                    "https://hf-mirror.com/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/resolve/main/tokens.txt",
                    "https://huggingface.co/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/resolve/main/tokens.txt",
                ],
                approx_mb: 0.3,
            },
        ],
    },
    SensePack {
        id: "paraformer-zh",
        name: "Paraformer-zh int8(听 · 深读,字级时间戳)",
        note: "中文最准 + 字级时间戳直出;sherpa-onnx 格式(2023-09-14 版支持时间戳)",
        files: &[
            SensePackFile {
                name: "model.int8.onnx",
                urls: &[
                    "https://hf-mirror.com/csukuangfj/sherpa-onnx-paraformer-zh-2023-09-14/resolve/main/model.int8.onnx",
                    "https://huggingface.co/csukuangfj/sherpa-onnx-paraformer-zh-2023-09-14/resolve/main/model.int8.onnx",
                ],
                approx_mb: 232.0,
            },
            SensePackFile {
                name: "tokens.txt",
                urls: &[
                    "https://hf-mirror.com/csukuangfj/sherpa-onnx-paraformer-zh-2023-09-14/resolve/main/tokens.txt",
                    "https://huggingface.co/csukuangfj/sherpa-onnx-paraformer-zh-2023-09-14/resolve/main/tokens.txt",
                ],
                approx_mb: 0.4,
            },
        ],
    },
];

fn pack_dir(id: &str) -> Option<PathBuf> {
    models_root().map(|r| r.join(id))
}

/// 包是否已就位:目录里每个文件都存在且非空。
fn pack_installed(pack: &SensePack) -> bool {
    let Some(dir) = pack_dir(pack.id) else { return false };
    pack.files.iter().all(|f| {
        dir.join(f.name)
            .metadata()
            .map(|m| m.len() > 0)
            .unwrap_or(false)
    })
}

fn pack_size_mb(pack: &SensePack) -> f64 {
    let Some(dir) = pack_dir(pack.id) else { return 0.0 };
    pack.files
        .iter()
        .filter_map(|f| dir.join(f.name).metadata().ok())
        .map(|m| m.len() as f64 / 1024.0 / 1024.0)
        .sum()
}

#[derive(Debug, Clone, Serialize)]
pub struct SensePackView {
    pub id: String,
    pub name: String,
    pub note: String,
    pub approx_mb: f64,
    pub installed: bool,
    pub size_mb: f64,
    pub downloading: bool,
}

fn pack_views() -> Vec<SensePackView> {
    let busy = PACK_BUSY.lock();
    SENSE_PACKS
        .iter()
        .map(|p| SensePackView {
            id: p.id.into(),
            name: p.name.into(),
            note: p.note.into(),
            approx_mb: p.files.iter().map(|f| f.approx_mb).sum(),
            installed: pack_installed(p),
            size_mb: pack_size_mb(p),
            downloading: busy.contains(p.id),
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
struct PackEvent {
    id: String,
    kind: String, // phase | progress | done | error
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pct: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    received_mb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_mb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

fn emit_pack(app: &AppHandle, ev: PackEvent) {
    let _ = app.emit("sense:pack", ev);
}

/// 流式下载单文件到 .part 再改名;每 ~1MB 或 1% 推一次进度。
fn download_file(
    app: &AppHandle,
    pack_id: &str,
    file: &SensePackFile,
    dst: &Path,
) -> Result<(), String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(120))
        .build();
    let mut last_err = String::new();
    for url in file.urls {
        emit_pack(app, PackEvent {
            id: pack_id.into(), kind: "phase".into(), file: Some(file.name.into()),
            pct: None, received_mb: None, total_mb: None,
            message: Some(format!("连接 {url}")),
        });
        let resp = match agent.get(url).call() {
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
        let part = dst.with_extension("part");
        let mut out = match fs::File::create(&part) {
            Ok(f) => f,
            Err(e) => return Err(format!("创建临时文件失败: {e}")),
        };
        let mut reader = resp.into_reader();
        let mut buf = vec![0u8; 256 * 1024];
        let mut received: u64 = 0;
        let mut last_emit: u64 = 0;
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    last_err = format!("{url}: 读取中断 {e}");
                    let _ = fs::remove_file(&part);
                    received = u64::MAX; // 标记失败,落到外层 continue
                    break;
                }
            };
            use std::io::Write as _;
            if let Err(e) = out.write_all(&buf[..n]) {
                let _ = fs::remove_file(&part);
                return Err(format!("写盘失败: {e}"));
            }
            received += n as u64;
            if received - last_emit >= 1024 * 1024 {
                last_emit = received;
                let pct = if total > 0 { ((received as f64 / total as f64) * 100.0) as u32 } else { 0 };
                emit_pack(app, PackEvent {
                    id: pack_id.into(), kind: "progress".into(), file: Some(file.name.into()),
                    pct: Some(pct.min(99)),
                    received_mb: Some(received as f64 / 1048576.0),
                    total_mb: if total > 0 { Some(total as f64 / 1048576.0) } else { None },
                    message: None,
                });
            }
        }
        if received == u64::MAX {
            continue; // 本源失败,试下一个镜像
        }
        drop(out);
        // 体积合理性闸:声明大小的一半以下视为半截/错误页
        if total > 0 && received < total {
            last_err = format!("{url}: 下载不完整({received}/{total})");
            let _ = fs::remove_file(&part);
            continue;
        }
        if (received as f64) < file.approx_mb * 1048576.0 * 0.5 && file.approx_mb > 1.0 {
            last_err = format!("{url}: 文件过小({received} 字节),疑似镜像错误页");
            let _ = fs::remove_file(&part);
            continue;
        }
        fs::rename(&part, dst).map_err(|e| format!("落位失败: {e}"))?;
        return Ok(());
    }
    Err(if last_err.is_empty() { "全部下载源不可达".into() } else { last_err })
}

/// 安装感官包:后台线程逐文件下载,进度走 `sense:pack` 事件。立即返回。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn sense_pack_install(app: AppHandle, id: String) -> Result<(), String> {
    let pack = SENSE_PACKS
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("没有感官包 '{id}'"))?;
    {
        let mut busy = PACK_BUSY.lock();
        if busy.contains(pack.id) {
            return Err("该感官包正在下载中".into());
        }
        busy.insert(pack.id.to_string());
    }
    let dir = pack_dir(pack.id).ok_or("无法定位模型目录")?;
    let pack_id = pack.id.to_string();
    std::thread::spawn(move || {
        let pack = SENSE_PACKS.iter().find(|p| p.id == pack_id).unwrap();
        let result = (|| -> Result<(), String> {
            fs::create_dir_all(&dir).map_err(|e| format!("建目录失败: {e}"))?;
            for f in pack.files {
                let dst = dir.join(f.name);
                if dst.metadata().map(|m| m.len() > 0).unwrap_or(false) {
                    continue; // 幂等续装:已有的文件跳过
                }
                download_file(&app, &pack_id, f, &dst)?;
            }
            Ok(())
        })();
        PACK_BUSY.lock().remove(&pack_id);
        match result {
            Ok(()) => emit_pack(&app, PackEvent {
                id: pack_id.clone(), kind: "done".into(), file: None, pct: Some(100),
                received_mb: None, total_mb: None, message: Some("安装完成".into()),
            }),
            Err(e) => emit_pack(&app, PackEvent {
                id: pack_id.clone(), kind: "error".into(), file: None, pct: None,
                received_mb: None, total_mb: None, message: Some(e),
            }),
        }
    });
    Ok(())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn sense_pack_remove(id: String) -> Result<(), String> {
    if PACK_BUSY.lock().contains(&id) {
        return Err("该感官包正在下载中,稍后再删".into());
    }
    let pack = SENSE_PACKS
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("没有感官包 '{id}'"))?;
    if let Some(dir) = pack_dir(pack.id) {
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

// ───────────────────────── 初始化与合并 ─────────────────────────

/// 启动时调用:读盘 + 与内置注册表合并(注册表新增项自动出现;用户改动保留)。
pub fn init() {
    let mut store: SenseStore = store_path()
        .filter(|p| p.exists())
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let registry = builtin_registry();
    for reg in registry {
        match store.items.iter_mut().find(|s| s.id == reg.id) {
            Some(existing) => {
                // 用户持有的增量字段保留;展示性字段跟随注册表升级
                existing.name = reg.name;
                existing.sense = reg.sense;
                existing.kind = reg.kind;
                existing.models = reg.models;
                existing.get_key_url = reg.get_key_url;
                existing.docs_url = reg.docs_url;
                existing.note = reg.note;
                existing.free = reg.free;
                existing.optional = reg.optional;
                existing.recommended = reg.recommended;
                existing.pack_id = reg.pack_id;
                if existing.base_url.trim().is_empty() {
                    existing.base_url = reg.base_url;
                }
                if existing.default_model.trim().is_empty() {
                    existing.default_model = reg.default_model;
                }
            }
            None => store.items.push(reg),
        }
    }
    *STORE.write() = store;
    persist();
}

/// 供应商坞 MiniMax key 复用:寓言计划 v5「两把钥匙」——感官坞 MiniMax 项 key 留空时,
/// 自动取 `~/Polaris/data/providers.json` 里 minimax 供应商的 token。
fn provider_dock_minimax_key() -> Option<String> {
    let pj = data_dir()?.join("providers.json");
    let v: Value = serde_json::from_str(&fs::read_to_string(pj).ok()?).ok()?;
    let items = v.get("items")?.as_array()?;
    for it in items {
        let id = it.get("id").and_then(|x| x.as_str()).unwrap_or("");
        if id != "minimax" && id != "minimax-en" {
            continue;
        }
        let env = it
            .get("settings_config")
            .or_else(|| it.get("settingsConfig"))
            .and_then(|c| c.get("env"))?;
        for k in ["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"] {
            if let Some(t) = env.get(k).and_then(|x| x.as_str()) {
                if !t.trim().is_empty() {
                    return Some(t.trim().to_string());
                }
            }
        }
    }
    None
}

/// 取某服务商「实际生效」的 key(MiniMax 项支持复用供应商坞)。
pub fn effective_key(p: &SenseProvider) -> String {
    if !p.api_key.trim().is_empty() {
        return p.api_key.trim().to_string();
    }
    if p.id.starts_with("minimax") {
        return provider_dock_minimax_key().unwrap_or_default();
    }
    String::new()
}

/// 给检索枢纽(fable/)用:取某感官「当前生效」的云服务商 ——
/// enabled + key 就绪,recommended 优先。云感官总闸关闭时返回 None。
pub fn active_provider(sense: &str) -> Option<SenseProvider> {
    let store = STORE.read();
    if !store.switches.cloud_enabled {
        return None;
    }
    let mut candidates: Vec<&SenseProvider> = store
        .items
        .iter()
        .filter(|p| p.sense == sense && p.kind == "api" && p.enabled)
        .filter(|p| !effective_key(p).is_empty())
        .collect();
    candidates.sort_by_key(|p| if p.recommended { 0 } else { 1 });
    candidates.first().map(|p| (*p).clone())
}

fn mask_key(k: &str) -> String {
    let t = k.trim();
    if t.is_empty() {
        return String::new();
    }
    let tail: String = t.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("●●●●●●{tail}")
}

// ───────────────────────── 列表 / 修改 / 开关 ─────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct SenseProviderView {
    pub id: String,
    pub name: String,
    pub sense: String,
    pub kind: String,
    pub enabled: bool,
    pub base_url: String,
    pub api_key_masked: String,
    /// key 已配置(含 MiniMax 复用供应商坞的情况)
    pub key_ready: bool,
    /// 复用来源说明(如「已复用供应商坞 MiniMax key」)
    pub key_source: String,
    pub default_model: String,
    pub models: Vec<SenseModel>,
    pub get_key_url: String,
    pub docs_url: String,
    pub note: String,
    pub free: bool,
    pub optional: bool,
    pub recommended: bool,
    pub pack_id: Option<String>,
    /// kind=local:包是否已装好(= 可用)
    pub installed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SenseGroupView {
    pub sense: String,
    pub label: String,
    pub desc: String,
    pub providers: Vec<SenseProviderView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SenseOverview {
    pub groups: Vec<SenseGroupView>,
    pub switches: SenseSwitches,
    pub packs: Vec<SensePackView>,
    pub models_dir: String,
}

fn provider_view(p: &SenseProvider) -> SenseProviderView {
    let eff = effective_key(p);
    let own = !p.api_key.trim().is_empty();
    let installed = p
        .pack_id
        .as_deref()
        .and_then(|pid| SENSE_PACKS.iter().find(|k| k.id == pid))
        .map(pack_installed)
        .unwrap_or(false);
    SenseProviderView {
        id: p.id.clone(),
        name: p.name.clone(),
        sense: p.sense.clone(),
        kind: p.kind.clone(),
        enabled: p.enabled,
        base_url: p.base_url.clone(),
        api_key_masked: mask_key(&p.api_key),
        key_ready: !eff.is_empty(),
        key_source: if !own && !eff.is_empty() && p.id.starts_with("minimax") {
            "已复用供应商坞的 MiniMax key".into()
        } else {
            String::new()
        },
        default_model: p.default_model.clone(),
        models: p.models.clone(),
        get_key_url: p.get_key_url.clone(),
        docs_url: p.docs_url.clone(),
        note: p.note.clone(),
        free: p.free,
        optional: p.optional,
        recommended: p.recommended,
        pack_id: p.pack_id.clone(),
        installed,
    }
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn sense_list() -> SenseOverview {
    let store = STORE.read();
    let groups = SENSE_GROUPS
        .iter()
        .map(|(sense, label, desc)| SenseGroupView {
            sense: (*sense).into(),
            label: (*label).into(),
            desc: (*desc).into(),
            providers: store
                .items
                .iter()
                .filter(|p| p.sense == *sense)
                .map(provider_view)
                .collect(),
        })
        .collect();
    SenseOverview {
        groups,
        switches: store.switches.clone(),
        packs: pack_views(),
        models_dir: models_root().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
    }
}

/// 修改一个服务商(只动传入的字段)。api_key 传空串 = 清除。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn sense_set(
    id: String,
    api_key: Option<String>,
    base_url: Option<String>,
    enabled: Option<bool>,
    default_model: Option<String>,
) -> Result<SenseOverview, String> {
    {
        let mut store = STORE.write();
        let p = store
            .items
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| format!("没有服务商 '{id}'"))?;
        if let Some(k) = api_key {
            p.api_key = k.trim().to_string();
        }
        if let Some(b) = base_url {
            if !b.trim().is_empty() {
                p.base_url = b.trim().trim_end_matches('/').to_string();
            }
        }
        if let Some(e) = enabled {
            p.enabled = e;
        }
        if let Some(dm) = default_model {
            if !dm.trim().is_empty() {
                p.default_model = dm.trim().to_string();
            }
        }
    }
    persist();
    Ok(sense_list())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn sense_switches_set(
    cloud_enabled: Option<bool>,
    audio_egress: Option<bool>,
    image_egress: Option<bool>,
    budget_monthly_cny: Option<f64>,
) -> Result<SenseOverview, String> {
    {
        let mut store = STORE.write();
        if let Some(v) = cloud_enabled {
            store.switches.cloud_enabled = v;
        }
        if let Some(v) = audio_egress {
            store.switches.audio_egress = v;
        }
        if let Some(v) = image_egress {
            store.switches.image_egress = v;
        }
        if let Some(v) = budget_monthly_cny {
            store.switches.budget_monthly_cny = v.max(0.0);
        }
    }
    persist();
    Ok(sense_list())
}

// ───────────────────────── 一键探活 ─────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct SenseTestResult {
    pub ok: bool,
    pub latency_ms: u64,
    pub message: String,
}

/// 每家一条最小探活请求;不支持的服务商给可读说明而非报错。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn sense_test(id: String) -> Result<SenseTestResult, String> {
    let p = {
        let store = STORE.read();
        store
            .items
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| format!("没有服务商 '{id}'"))?
    };
    if p.kind == "local" {
        let installed = p
            .pack_id
            .as_deref()
            .and_then(|pid| SENSE_PACKS.iter().find(|k| k.id == pid))
            .map(pack_installed)
            .unwrap_or(false);
        return Ok(SenseTestResult {
            ok: installed,
            latency_ms: 0,
            message: if installed {
                "模型文件已就位(推理引擎接入后即可用)".into()
            } else {
                "模型未下载 —— 点「下载」装入感官包".into()
            },
        });
    }
    let key = effective_key(&p);
    if key.is_empty() {
        return Ok(SenseTestResult { ok: false, latency_ms: 0, message: "未填 API Key".into() });
    }
    // 探活路径按服务商风格区分
    enum Probe {
        Models(&'static str),
        Chat(&'static str),
        None,
    }
    let probe = match p.id.as_str() {
        "siliconflow-asr" | "siliconflow-embed" | "siliconflow-rerank" => Probe::Models("/v1/models"),
        "groq-whisper" => Probe::Models("/v1/models"),
        "minimax-vl" => Probe::Chat("/v1/chat/completions"),
        "zhipu-glm4v" => Probe::Chat("/chat/completions"),
        _ => Probe::None,
    };
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .build();
    let base = p.base_url.trim_end_matches('/');
    let started = Instant::now();
    let resp = match probe {
        Probe::None => {
            return Ok(SenseTestResult {
                ok: true,
                latency_ms: 0,
                message: "已保存;该服务商暂不支持一键探活,将在实际调用时验证".into(),
            })
        }
        Probe::Models(path) => agent
            .get(&format!("{base}{path}"))
            .set("authorization", &format!("Bearer {key}"))
            .call(),
        Probe::Chat(path) => agent
            .post(&format!("{base}{path}"))
            .set("authorization", &format!("Bearer {key}"))
            .send_json(serde_json::json!({
                "model": p.default_model,
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 1
            })),
    };
    let ms = started.elapsed().as_millis() as u64;
    Ok(match resp {
        Ok(_) => SenseTestResult { ok: true, latency_ms: ms, message: format!("连通正常({ms}ms)") },
        Err(ureq::Error::Status(401, _)) | Err(ureq::Error::Status(403, _)) => SenseTestResult {
            ok: false,
            latency_ms: ms,
            message: "密钥无效或无权限(401/403)".into(),
        },
        Err(ureq::Error::Status(429, _)) => SenseTestResult {
            ok: true,
            latency_ms: ms,
            message: "已连通,但当前限速(429)—— 免费档正常现象".into(),
        },
        Err(ureq::Error::Status(code, _)) => SenseTestResult {
            ok: false,
            latency_ms: ms,
            message: format!("端点可达但返回 HTTP {code}(key 已保存,实际调用时再验证)"),
        },
        Err(ureq::Error::Transport(t)) => SenseTestResult {
            ok: false,
            latency_ms: ms,
            message: format!("网络不通:{t}"),
        },
    })
}
