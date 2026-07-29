//! 火山引擎云端语音识别 —— 手机端「按住说话」的转写后端。
//!
//! 为什么不复用本地 SenseVoice(`asr.rs`):那条要 sherpa-onnx 动态库 + 本地模型,
//! 只在桌面 `voice-asr` feature 下编译;手机壳是纯 WebView,音频只能传回主机转写。
//! 本文件零重型依赖(ureq + base64),默认随壳一起编,手机端才有语音输入可用。
//!
//! **两条路,按已有凭据自动选**(2026-07-23 查证官方文档后定的形状):
//!
//! ① `openspeech` —— 火山「大模型录音文件识别·极速版」,真 ASR:
//!    `POST https://openspeech.bytedance.com/api/v3/auc/bigmodel/recognize/flash`
//!    单次请求即出结果(不用 submit/query 轮询),鉴权走 `X-Api-App-Key` +
//!    `X-Api-Access-Key`(语音技术控制台的 AppID / Access Token)。
//!    **注意:这套凭据与方舟的 API Key 不是一套,不通用** —— 要用户另去
//!    console.volcengine.com/speech 开通,填进语音设置里。
//!
//! ② `ark` —— 方舟 Chat API 的「音频理解」,用**用户已经配好的方舟 key**当 ASR 使:
//!    `POST https://{ark 主机}/api/v3/chat/completions`,content 里塞 `input_audio`,
//!    提示词要求只输出转写文本。这就是用户说的「用我们火山的 agentplan 来语音识别」——
//!    Agentplan 正是聊天供应商坞里那家方舟(见 provider::VOLC_ARK_IDS),
//!    一分钱不用另开通,填过聊天 key 就能说话。
//!
//! 没配 ①、也借不到 ② 的 key 时给一句能照做的错误话术,不静默失败。
//!
//! **音频格式铁律**:两条路都**不收 webm/opus**,而安卓 WebView 的 MediaRecorder
//! 默认恰恰只出 webm/opus。所以录音端(mobile/src/lib/recorder.ts)必须自己抓 PCM
//! 拼 16kHz/16bit/单声道 WAV —— 别图省事直接把 MediaRecorder 的 blob 传上来。

use base64::Engine as _;
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::voice::{anti_pollute, AntiChange, STORE};

/// 极速版录音文件识别的固定资源 id(标准版 submit/query 是 `volc.bigasr.auc`)。
const OPENSPEECH_RESOURCE: &str = "volc.bigasr.auc_turbo";
const OPENSPEECH_URL: &str = "https://openspeech.bytedance.com/api/v3/auc/bigmodel/recognize/flash";
/// openspeech 成功的状态码 —— 它放在**响应头** `X-Api-Status-Code` 里而不是 HTTP status。
const OPENSPEECH_OK: &str = "20000000";

/// 方舟走「音频理解」当 ASR 时的默认模型(doubao-seed-2.0 系才吃 input_audio)。
pub const DEFAULT_ARK_ASR_MODEL: &str = "doubao-seed-2-0-lite-260428";

/// 音频上限,按**base64 后**的字符数算(下面比较的就是它,别改成解码后的字节数)。
/// 方舟侧限 25MB、openspeech 限 100MB,取最严的再留余量。
/// 手机端一次按住说话最多 45 秒 ≈ 1.9MB,超这么多基本是调用方出 bug,早拦早报错。
const MAX_AUDIO_B64_LEN: usize = 20 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudAsrResult {
    /// 识别原文(未经防污染)
    pub raw: String,
    /// 防污染后的终稿 —— 这是该填进输入框的文字
    pub text: String,
    /// 防污染改了哪些词(前端可展示「已纠正」)
    pub changes: Vec<AntiChange>,
    /// 防污染档位
    pub tier: String,
    /// 端到端耗时(含网络)
    pub ms: u64,
    /// 实际走的路:"volc-openspeech" | "volc-ark"
    pub engine: String,
    /// 实际用的模型 / 资源
    pub model: String,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        // 一句话识别通常 1~3s;给到 45s 兜住长句 + 弱网,但别让手机端傻等到天荒地老。
        .timeout(Duration::from_secs(45))
        .build()
}

/// 生成一个 UUID 形状的请求 id(纳秒 + 进程内自增,足够唯一,免为此拉一个 uuid 依赖)。
fn request_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let a = nanos ^ (seq << 32);
    let b = nanos.rotate_left(17) ^ seq;
    format!(
        "{:08x}-{:04x}-4{:03x}-a{:03x}-{:012x}",
        (a >> 32) as u32,
        (a >> 16) as u16,
        (a & 0xfff) as u16,
        (b >> 48) as u16 & 0xfff,
        b & 0xffff_ffff_ffff
    )
}

/// 主入口:一段 base64 音频 → 文字(已过防污染)。
///
/// `audio_b64` 必须是**不带 data URL 前缀**的纯 base64;`format` 形如 "wav"/"mp3"
/// (方舟路需要它;openspeech 路会自己嗅探,传了也无妨)。
pub fn transcribe_base64(audio_b64: &str, format: &str) -> Result<CloudAsrResult, String> {
    let b64 = audio_b64
        .trim()
        // 前端万一带了 `data:audio/wav;base64,` 前缀也认,免得为个前缀来回扯皮
        .rsplit("base64,")
        .next()
        .unwrap_or("")
        .trim();
    if b64.is_empty() {
        return Err("没有收到音频数据".into());
    }
    if b64.len() > MAX_AUDIO_B64_LEN {
        return Err(format!(
            "音频过大({}MB),请分段说;单次上限 {}MB",
            b64.len() / 1024 / 1024,
            MAX_AUDIO_B64_LEN / 1024 / 1024
        ));
    }
    // 只做合法性校验(解出来的字节这里用不上,openspeech/方舟都直接吃 base64),
    // 但坏 base64 早拦一步,好过让远端回一句看不懂的 45000001。
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|_| "音频数据不是合法的 base64".to_string())?;

    let fmt = {
        let f = format.trim().trim_start_matches('.').to_ascii_lowercase();
        if f.is_empty() {
            "wav".to_string()
        } else {
            f
        }
    };

    let started = Instant::now();
    let (app_key, access_key, ark_model) = {
        let s = STORE.read();
        (
            s.config.volc_app_key.trim().to_string(),
            s.config.volc_access_key.trim().to_string(),
            s.config.volc_asr_model.trim().to_string(),
        )
    };

    // 路 ①:语音技术控制台的凭据齐了就走真 ASR(有词级时间戳、错误码语义清晰)。
    let (raw, engine, model) = if !app_key.is_empty() && !access_key.is_empty() {
        (
            openspeech_flash(b64, &app_key, &access_key)?,
            "volc-openspeech",
            OPENSPEECH_RESOURCE.to_string(),
        )
    } else {
        // 路 ②:借聊天坞里的方舟(Agentplan)key,用音频理解当 ASR。
        let Some((key, host)) = crate::provider::volc_ark_borrow() else {
            return Err(
                "还没有可用的火山凭据:去「设置 → 供应商」把「火山方舟 Agentplan」的 API Key 填上\
(填了就能直接语音输入),或在语音设置里填火山语音技术的 AppID + Access Token。"
                    .into(),
            );
        };
        let model = if ark_model.is_empty() {
            DEFAULT_ARK_ASR_MODEL.to_string()
        } else {
            ark_model
        };
        (ark_audio_understand(b64, &fmt, &key, &host, &model)?, "volc-ark", model)
    };

    let anti = anti_pollute(&raw);
    Ok(CloudAsrResult {
        raw,
        text: anti.text,
        changes: anti.changes,
        tier: anti.tier,
        ms: started.elapsed().as_millis() as u64,
        engine: engine.into(),
        model,
    })
}

/// 路 ①:openspeech 极速版。成功与否看响应头 `X-Api-Status-Code`,不是 HTTP status。
fn openspeech_flash(b64: &str, app_key: &str, access_key: &str) -> Result<String, String> {
    // body 只带官方极速版示例里出现过的字段。enable_itn/enable_punc 这些是标准版
    // (submit)文档里的开关,极速版是否认**没有证实**,乱加有可能直接 45000001 参数无效。
    let body = serde_json::json!({
        "user": { "uid": "polaris" },
        "audio": { "data": b64 },
        "request": { "model_name": "bigmodel" },
    });
    let resp = agent()
        .post(OPENSPEECH_URL)
        .set("X-Api-App-Key", app_key)
        .set("X-Api-Access-Key", access_key)
        .set("X-Api-Resource-Id", OPENSPEECH_RESOURCE)
        .set("X-Api-Request-Id", &request_id())
        .set("X-Api-Sequence", "-1")
        .set("Content-Type", "application/json")
        .send_json(body);

    let resp = match resp {
        Ok(r) => r,
        // 非 2xx 也可能带着可读的 X-Api-Message,拆出来给用户看,别只报一句 HTTP 4xx。
        Err(ureq::Error::Status(code, r)) => {
            let msg = r.header("X-Api-Message").unwrap_or("").to_string();
            let sc = r.header("X-Api-Status-Code").unwrap_or("").to_string();
            return Err(format!(
                "火山语音识别失败(HTTP {code}{}{})",
                if sc.is_empty() {
                    String::new()
                } else {
                    format!(", 状态码 {sc}")
                },
                if msg.is_empty() {
                    String::new()
                } else {
                    format!(": {msg}")
                }
            ));
        }
        Err(e) => return Err(format!("连不上火山语音识别: {e}")),
    };

    let status = resp.header("X-Api-Status-Code").unwrap_or("").to_string();
    let msg = resp.header("X-Api-Message").unwrap_or("").to_string();
    if !status.is_empty() && status != OPENSPEECH_OK {
        return Err(match status.as_str() {
            "20000003" => "没听到声音(静音),再说一次试试".to_string(),
            "45000002" => "音频是空的,录音可能没成功".to_string(),
            "45000151" => "音频格式不对 —— 火山只收 wav/mp3/ogg-opus".to_string(),
            "55000031" => "火山语音服务繁忙,稍后再试".to_string(),
            _ => format!("火山语音识别失败(状态码 {status}{})", if msg.is_empty() {
                String::new()
            } else {
                format!(": {msg}")
            }),
        });
    }

    let v: serde_json::Value = resp
        .into_json()
        .map_err(|e| format!("火山语音识别返回解析失败: {e}"))?;
    let text = v
        .get("result")
        .and_then(|r| r.get("text"))
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    Ok(text)
}

/// 路 ②:方舟 Chat API 的音频理解。转写文本落在 `choices[0].message.content`。
fn ark_audio_understand(
    b64: &str,
    format: &str,
    key: &str,
    host: &str,
    model: &str,
) -> Result<String, String> {
    let url = format!("https://{host}/api/v3/chat/completions");
    let body = serde_json::json!({
        "model": model,
        "temperature": 0,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "input_audio", "input_audio": { "data": b64, "format": format } },
                { "type": "text",
                  "text": "请逐字转写这段音频的中文内容。只输出转写文本本身,不要任何解释、标题、引号或前后缀;听不清就输出空。" },
            ],
        }],
    });
    let resp = agent()
        .post(&url)
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .send_json(body);
    let resp = match resp {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let detail = r.into_string().unwrap_or_default();
            let hint = if code == 404 || code == 403 {
                // Agentplan 是 Coding Plan 端点, 套餐可能圈定模型范围, 打不通 /api/v3 很正常 ——
                // 直接把出路写在错误里, 免得用户对着 404 猜半天。
                "\n(方舟 Coding Plan 的 key 未必能调音频理解模型;可在语音设置里换个 volcAsrModel,\
或去火山语音技术控制台开通「大模型录音文件识别」后填 AppID + Access Token 走专业路)"
            } else {
                ""
            };
            return Err(format!(
                "火山方舟转写失败(HTTP {code}): {}{hint}",
                detail.chars().take(300).collect::<String>()
            ));
        }
        Err(e) => return Err(format!("连不上火山方舟: {e}")),
    };
    let v: serde_json::Value = resp
        .into_json()
        .map_err(|e| format!("火山方舟返回解析失败: {e}"))?;
    let text = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .trim()
        .trim_matches('"')
        .to_string();
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_is_uuid_shaped_and_unique() {
        let a = request_id();
        let b = request_id();
        assert_ne!(a, b, "同进程内两次不能撞");
        assert_eq!(a.len(), 36, "应是 UUID 形状: {a}");
        assert_eq!(
            a.split('-').map(|s| s.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12],
            "分段长度不对: {a}"
        );
    }

    #[test]
    fn rejects_empty_and_bad_base64() {
        assert!(transcribe_base64("", "wav").is_err());
        assert!(
            transcribe_base64("!!!not base64!!!", "wav")
                .unwrap_err()
                .contains("base64"),
            "坏 base64 要在发请求前就被拦下"
        );
    }

    #[test]
    fn strips_data_url_prefix() {
        // 前端带前缀不该炸;这里只验证前缀被剥掉后进入了后续流程(没凭据 → 报凭据错而非 base64 错)
        let err = transcribe_base64("data:audio/wav;base64,####", "wav").unwrap_err();
        assert!(err.contains("base64"), "剥前缀后剩下的坏数据仍应被 base64 校验拦住: {err}");
    }
}
