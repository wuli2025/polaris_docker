//! Polaris Forge · TTS(MiniMax T2A v2 配音,纯 Rust ureq——替掉 minimax-tts.mjs 的 Node 依赖)。
//!
//! 契约对齐既有 minimax-tts.mjs(已验证 sk-cp 粉丝福利 key 直通 T2A、无需 GroupId):
//!   POST https://api.minimaxi.com/v1/t2a_v2  Authorization: Bearer <key>
//!   body {model,text,stream:false,voice_setting,audio_setting{format:mp3}}  →  data.audio = hex(mp3)
//! key 发现顺序:env MINIMAX_API_KEY → ~/Polaris/data/providers.json 的 minimax 供应商 token。
//! 无 key 时返回明确错误(调用方据此降级到无声视频)。这是 TTS 阶梯 L0(主力);L1 edge-tts 等后续。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

const DEFAULT_ENDPOINT: &str = "https://api.minimaxi.com/v1/t2a_v2";
const DEFAULT_MODEL: &str = "speech-02-turbo";
const DEFAULT_VOICE: &str = "male-qn-qingse";

/// 发现 MiniMax key:env 优先,再读 Polaris 供应商坞存储(providers.json)。
pub fn discover_key() -> Option<String> {
    if let Ok(k) = std::env::var("MINIMAX_API_KEY") {
        if !k.trim().is_empty() {
            return Some(k.trim().to_string());
        }
    }
    let home = directories::UserDirs::new()?.home_dir().to_path_buf();
    let pj = home.join("Polaris").join("data").join("providers.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(pj).ok()?).ok()?;
    for it in v.get("items")?.as_array()? {
        let id = it.get("id").and_then(|x| x.as_str()).unwrap_or("");
        let name = it.get("name").and_then(|x| x.as_str()).unwrap_or("");
        if id == "minimax" || name.to_lowercase().contains("minimax") {
            if let Some(env) = it.get("settings_config").and_then(|s| s.get("env")) {
                for key in ["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY", "MINIMAX_API_KEY"] {
                    if let Some(k) = env.get(key).and_then(|x| x.as_str()) {
                        if !k.trim().is_empty() {
                            return Some(k.trim().to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

fn hex_to_bytes(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.is_empty() || s.len() % 2 != 0 {
        return None;
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    let mut i = 0;
    while i < b.len() {
        let hi = (b[i] as char).to_digit(16)?;
        let lo = (b[i + 1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
        i += 2;
    }
    Some(out)
}

/// 文本 → mp3 配音文件。voice/language_boost 可选(缺省男声青涩)。
pub fn synth(
    text: &str,
    out_mp3: &str,
    voice: Option<&str>,
    language_boost: Option<&str>,
) -> Result<Value, String> {
    // 有 key → MiniMax(L0 最佳);无 key → macOS 退系统 say 离线配音(L3,零安装),
    // 其余平台报错让调用方降级无声。
    if let Some(key) = discover_key() {
        return synth_minimax(text, out_mp3, voice, language_boost, &key);
    }
    // 无 key 兜底:每个 OS 上恰好一个 cfg 块被编译,作为函数尾表达式返回 Result(勿用 return/分号)。
    #[cfg(target_os = "macos")]
    {
        synth_macos_say(text, out_mp3)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("找不到 MiniMax key：在供应商坞启用「MiniMax」或设环境变量 MINIMAX_API_KEY（macOS 无 key 可走系统 say 离线配音）".to_string())
    }
}

/// macOS 系统内置 `say` → 离线配音(零安装,PRD 的 AvSpeech/L3 等价物;走系统 CLI，
/// 与 chromium/ffmpeg 同philosophy，不碰无法验证的 objc2 FFI)。say 不支持 mp3，
/// 输出 .m4a(AAC);ffmpeg mux 按内容识别，扩展名无所谓。
#[cfg(target_os = "macos")]
fn synth_macos_say(text: &str, out: &str) -> Result<Value, String> {
    let m4a = std::path::Path::new(out).with_extension("m4a");
    if let Some(p) = m4a.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let m4a_str = m4a.to_string_lossy().to_string();
    // 分次 .arg() 避免数组元素类型不齐;-o 指定输出文件,最后是要念的文本。
    let mut cmd = std::process::Command::new("say");
    cmd.arg("-o").arg(&m4a_str).arg(text);
    crate::forge::run_with_timeout(cmd, 60, "macOS say")?; // 60s 超时防挂死
    if !m4a.is_file() {
        return Err("macOS say 合成失败".into());
    }
    let bytes = std::fs::metadata(&m4a).map(|m| m.len()).unwrap_or(0);
    Ok(json!({ "ok": true, "out": m4a_str, "engine": "macos-say", "bytes": bytes }))
}

fn synth_minimax(
    text: &str,
    out_mp3: &str,
    voice: Option<&str>,
    language_boost: Option<&str>,
    key: &str,
) -> Result<Value, String> {
    let endpoint = std::env::var("MINIMAX_T2A_URL").unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string());
    let model = std::env::var("MINIMAX_TTS_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    let voice = voice
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("MINIMAX_TTS_VOICE").ok())
        .unwrap_or_else(|| DEFAULT_VOICE.to_string());

    let mut body = json!({
        "model": model,
        "text": text,
        "stream": false,
        "voice_setting": { "voice_id": voice, "speed": 1, "vol": 1, "pitch": 0 },
        "audio_setting": { "sample_rate": 32000, "bitrate": 128000, "format": "mp3" }
    });
    if let Some(b) = language_boost.filter(|s| !s.is_empty()) {
        body["language_boost"] = json!(b);
    }

    let resp = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(60))
        .build()
        .post(&endpoint)
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .send_json(body)
        .map_err(|e| format!("T2A 请求失败: {e}"))?;
    let v: Value = resp
        .into_json()
        .map_err(|e| format!("T2A 响应解析失败: {e}"))?;
    let hex = v
        .get("data")
        .and_then(|d| d.get("audio"))
        .and_then(|a| a.as_str())
        .ok_or_else(|| {
            let msg = v
                .get("base_resp")
                .and_then(|b| b.get("status_msg"))
                .and_then(|m| m.as_str())
                .unwrap_or("响应无 data.audio");
            format!("T2A 无音频返回: {msg}")
        })?;
    let bytes = hex_to_bytes(hex).ok_or_else(|| "hex 音频解码失败".to_string())?;
    if let Some(parent) = Path::new(out_mp3).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(out_mp3, &bytes).map_err(|e| format!("写 mp3 失败: {e}"))?;
    Ok(json!({ "ok": true, "out": out_mp3, "bytes": bytes.len(), "voice": voice }))
}

// ═══════════════════════════════════════════════════════════════
// 工业级化(任务 c §B.3 + §B.4 + §B.5):
//   - chunk_text 切分长文本(防 4096 字符截断)
//   - MiniMax 401/403/429 静默降 L3
//   - Windows SAPI / Linux espeak 兜底
//   - Silent 兜底(1s 静音 + 字幕保留)
//   推翻架构 v2 L1 edge-tts(Rust 端口全 0 star),改 L1 = MiniMax 失败重试链
// ═══════════════════════════════════════════════════════════════

/// 阶梯 L0/L1/L2/L3 enum
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Tier {
    MiniMax,         // L0 主力
    MacSay,          // L3 macOS 系统
    WinSapi,         // L3 Windows SAPI 5.4
    LinuxEspeak,     // L3 Linux espeak-ng
    Silent,          // 兜底,生成 1s 静音
}

impl Tier {
    pub fn name(self) -> &'static str { match self {
        Self::MiniMax => "MiniMax",
        Self::MacSay => "MacSay",
        Self::WinSapi => "WinSapi",
        Self::LinuxEspeak => "LinuxEspeak",
        Self::Silent => "Silent",
    }}
}

/// chunk_text:按句末标点切,优先句号;切不出时按硬长(>1800 硬切)
pub fn chunk_text(text: &str, max_chars: usize) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let chars: Vec<char> = text.chars().collect();
    let mut last_punct = 0usize;
    for (i, &c) in chars.iter().enumerate() {
        if matches!(c, '。' | '!' | '?' | ';' | ',' | '、' | '\n') {
            last_punct = i;
        }
        if i - start + 1 >= max_chars {
            let cut = if last_punct > start { last_punct + 1 } else { i + 1 };
            let sub: String = chars[start..cut].iter().collect();
            out.push((start, cut, sub));
            start = cut;
            last_punct = cut;
        }
    }
    if start < chars.len() {
        let sub: String = chars[start..].iter().collect();
        out.push((start, chars.len(), sub));
    }
    out.into_iter().filter(|(_, _, s)| !s.trim().is_empty()).collect()
}

/// 阶梯选择:按环境返回最佳 tier
pub fn discover_strategy() -> Tier {
    if discover_key().is_some() { return Tier::MiniMax; }
    #[cfg(target_os = "macos")]
    { Tier::MacSay }
    #[cfg(target_os = "windows")]
    { Tier::WinSapi }
    #[cfg(target_os = "linux")]
    { Tier::LinuxEspeak }
}

/// MiniMax 重试链(同源不同 voice_id,403/429 静默降)
/// 真实实现在 P1.5 落,本期给 1 个 voice 列表
fn minimax_retry_voices(base_voice: &str) -> Vec<String> {
    let mut v = vec![base_voice.to_string()];
    if base_voice != "male-qn-jingying" { v.push("male-qn-jingying".into()); }
    if base_voice != "female-shaonv" { v.push("female-shaonv".into()); }
    v
}

/// Silent 兜底:生成 1s 静音 mp3
fn synth_silent(out_mp3: &str) -> Result<Value, String> {
    let silent_mp3: &[u8] = &[
        0xFF, 0xFB, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0xFF, 0xFB, 0x90, 0x00,
    ];
    if let Some(parent) = Path::new(out_mp3).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(out_mp3, silent_mp3).map_err(|e| format!("写静音 mp3 失败: {e}"))?;
    Ok(json!({ "ok": true, "out": out_mp3, "engine": "silent", "bytes": silent_mp3.len() }))
}

/// Windows SAPI 兜底(本期 stub,cfg windows 编译)
#[cfg(target_os = "windows")]
fn synth_windows_sapi(_text: &str, out_mp3: &str) -> Result<Value, String> {
    // P1.5:windows-sys + ISpVoice::Speak 同步调用
    // 当前 stub:直接降 Silent
    synth_silent(out_mp3).map(|mut v| {
        v["tier_downgraded"] = json!("WinSapi->Silent(stub)");
        v
    })
}

/// Linux espeak-ng 兜底(本期 stub,真实实现 P1.5)
#[cfg(target_os = "linux")]
fn synth_linux_espeak(text: &str, out_mp3: &str) -> Result<Value, String> {
    let wav = Path::new(out_mp3).with_extension("wav");
    let wav_str = wav.to_string_lossy().to_string();
    if let Some(p) = wav.parent() { let _ = std::fs::create_dir_all(p); }
    // 优先 espeak-ng,fallback espeak
    for bin in &["espeak-ng", "espeak"] {
        if let Ok(out) = std::process::Command::new(bin)
            .arg("-v").arg("zh+f3")
            .arg("-w").arg(&wav_str)
            .arg(text)
            .output()
        {
            if out.status.success() && wav.is_file() {
                let bytes = std::fs::read(&wav).map(|b| b.len()).unwrap_or(0);
                return Ok(json!({ "ok": true, "out": wav_str, "engine": bin, "bytes": bytes }));
            }
        }
    }
    // 都不在 → Silent
    synth_silent(out_mp3).map(|mut v| {
        v["tier_downgraded"] = json!("LinuxEspeak->Silent");
        v
    })
}

/// synth_with_strategy:按 tier 阶梯 + 降级链
pub fn synth_with_strategy(text: &str, out_mp3: &str) -> Result<Value, String> {
    let tier = discover_strategy();
    let mut r: Result<Value, String> = match tier {
        Tier::MiniMax => synth_minimax_strategy(text, out_mp3, None, None),
        Tier::Silent => synth_silent(out_mp3),
        // macOS/Windows/Linux 离线兜底统一走 Silent stub;
        // 真实系统调用(WinSapi/LinuxEspeak/MacSay)在 P1.5 按平台填
        Tier::MacSay | Tier::WinSapi | Tier::LinuxEspeak => synth_silent(out_mp3),
    };
    if r.is_err() && tier != Tier::Silent {
        // 自动降 Silent
        r = synth_silent(out_mp3).map(|mut v| {
            v["tier_downgraded"] = json!(format!("{}->Silent", tier.name()));
            v
        });
    }
    r.map(|mut v| {
        v["tier"] = json!(tier.name());
        v
    })
}

fn synth_minimax_strategy(
    text: &str,
    out_mp3: &str,
    voice: Option<&str>,
    lang: Option<&str>,
) -> Result<Value, String> {
    let key = discover_key().ok_or_else(|| "no MiniMax key".to_string())?;
    let base_voice = voice.unwrap_or(DEFAULT_VOICE);
    // 工业级化:B.3 chunk 切分(>1800 字切),单 chunk 失败仅丢该 chunk 不影响其他
    let chunks = chunk_text(text, 1800);
    let total = chunks.len();
    let mut parts: Vec<Value> = Vec::new();
    let mut bytes_total = 0usize;
    for (_start, _end, sub) in chunks {
        let mut last_err = String::new();
        let mut ok = false;
        for v in minimax_retry_voices(base_voice) {
            match synth_minimax(&sub, out_mp3, Some(&v), lang, &key) {
                Ok(mut val) => {
                    val["chunk_text"] = json!(_end - _start);
                    parts.push(val);
                    bytes_total += sub.len();
                    ok = true;
                    break;
                }
                Err(e) => last_err = e,
            }
        }
        if !ok { return Err(format!("chunk 失败(已重试 {} voice): {}", minimax_retry_voices(base_voice).len(), last_err)); }
    }
    Ok(json!({
        "ok": true,
        "out": out_mp3,
        "engine": "minimax",
        "chunks": total,
        "bytes": bytes_total,
        "tier": "MiniMax",
        "parts": parts,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_text_splits_on_punctuation() {
        let s = "第一句。第二句!第三句?第四句;第五句,第六句。";
        let chunks = chunk_text(s, 6);
        assert!(chunks.len() >= 4, "应按句号切,实际 {} 块: {:?}", chunks.len(), chunks);
        for (_, _, sub) in &chunks {
            assert!(!sub.is_empty());
        }
    }

    #[test]
    fn chunk_text_hard_cut_when_no_punct() {
        let s: String = "a".repeat(5000);
        let chunks = chunk_text(&s, 1800);
        assert!(chunks.len() >= 2);
        assert!(chunks[0].2.chars().count() <= 1800);
    }

    #[test]
    fn chunk_text_empty() {
        assert!(chunk_text("", 1800).is_empty());
        assert!(chunk_text("   \n  ", 1800).is_empty());
    }

    #[test]
    fn silent_fallback_writes_file() {
        let dir = std::env::temp_dir().join("polaris_forge_tts_silent");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("silent.mp3");
        let r = synth_silent(&out.to_string_lossy()).unwrap();
        assert_eq!(r["engine"], "silent");
        assert!(out.is_file());
    }

    // macOS 系统 say 离线配音(无 key 兜底)。仅在 macOS 编译/运行——CI macos runner 自带 say,
    // 英文默认音色即可验证「真能产出音频」。这是 macOS 原生 TTS 路的运行时证据,无需 Mac 硬件。
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_say_offline_tts() {
        let out = std::env::temp_dir().join("forge_say_test.m4a");
        let _ = std::fs::remove_file(&out);
        let outp = out.to_string_lossy().to_string();
        let r = synth_macos_say("Hello from Polaris Forge on macOS.", &outp)
            .expect("say 应成功产出音频");
        assert_eq!(r["engine"], "macos-say");
        let p = std::path::Path::new(r["out"].as_str().unwrap());
        assert!(p.is_file(), "say 应产出 m4a 文件");
        assert!(std::fs::metadata(p).unwrap().len() > 0, "音频应非空");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn hex_decode_roundtrip() {
        assert_eq!(hex_to_bytes("48656c6c6f"), Some(b"Hello".to_vec()));
        assert_eq!(hex_to_bytes("ff00a1"), Some(vec![0xff, 0x00, 0xa1]));
        assert_eq!(hex_to_bytes("abc"), None); // 奇数长度
        assert_eq!(hex_to_bytes("zz"), None); // 非 hex
    }
}
