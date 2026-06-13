//! 语音识别运行时 —— 本地 SenseVoice(sherpa-onnx via sherpa-rs)。
//!
//! 仅在 `voice-asr` feature 下编译(默认关,保护现有 build)。负责把音频 → 文本,
//! 再交给 voice.rs 的防污染管线(秒达/重型档)出终稿。模型从感官坞同一份目录加载:
//!   `~/Polaris/models/sensevoice-small/{model.int8.onnx, tokens.txt}`
//! (与 sense.rs 的 SENSE_PACKS 下载器同源,不重复下载)。
//!
//! 运行时需把 sherpa-onnx 动态库放进 PATH(sherpa-rs BUILDING.md);download-binaries
//! 会把库产到 target 下。

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::time::Instant;

use sherpa_rs::sense_voice::{SenseVoiceConfig, SenseVoiceRecognizer};

use crate::voice::{anti_pollute, TranscribeResult};

/// 进程内单例识别器(SenseVoiceRecognizer 是 Send+Sync;首次用时加载,之后复用)。
static REC: Lazy<Mutex<Option<SenseVoiceRecognizer>>> = Lazy::new(|| Mutex::new(None));

fn model_dir() -> Option<PathBuf> {
    crate::sense::models_root().map(|r| r.join("sensevoice-small"))
}

/// 确保识别器就位;模型缺失给可读错误(指向设置页下载感官包)。
pub(crate) fn ensure_recognizer() -> Result<(), String> {
    {
        if REC.lock().is_some() {
            return Ok(());
        }
    }
    let dir = model_dir().ok_or("无法定位模型目录")?;
    let model = dir.join("model.int8.onnx");
    let tokens = dir.join("tokens.txt");
    if !model.exists() || !tokens.exists() {
        return Err(format!(
            "SenseVoice 模型未下载({});去「设置 → 感官 API」下载「SenseVoice-Small」感官包",
            dir.display()
        ));
    }
    let config = SenseVoiceConfig {
        model: model.to_string_lossy().into_owned(),
        tokens: tokens.to_string_lossy().into_owned(),
        language: "auto".into(),
        use_itn: true,
        num_threads: Some(num_threads()),
        ..Default::default()
    };
    let rec = SenseVoiceRecognizer::new(config)
        .map_err(|e| format!("加载 SenseVoice 失败: {e}"))?;
    *REC.lock() = Some(rec);
    Ok(())
}

fn num_threads() -> i32 {
    std::thread::available_parallelism()
        .map(|n| (n.get() as i32 - 1).max(1).min(4))
        .unwrap_or(2)
}

/// 识别一段 f32 单声道采样(任意采样率,内部按传入 sr 处理)→ 防污染 → 终稿。
pub fn transcribe_samples(sample_rate: u32, samples: &[f32]) -> Result<TranscribeResult, String> {
    ensure_recognizer()?;
    let started = Instant::now();
    let raw = {
        let mut guard = REC.lock();
        let rec = guard.as_mut().ok_or("识别器未初始化")?;
        let result = rec.transcribe(sample_rate, samples);
        result.text.trim().to_string()
    };
    let anti = anti_pollute(&raw);
    Ok(TranscribeResult {
        raw,
        text: anti.text,
        changes: anti.changes,
        tier: anti.tier,
        ms: started.elapsed().as_millis() as u64,
    })
}

/// 识别一个 wav 文件(16k 单声道)→ 防污染 → 终稿。供「测一下识别」用。
pub fn transcribe_file(path: &str) -> Result<TranscribeResult, String> {
    let (samples, sr) =
        sherpa_rs::read_audio_file(path).map_err(|e| format!("读音频失败({path}): {e}"))?;
    transcribe_samples(sr, &samples)
}

// ───────────────────────── 自测(需模型 + 测试音频在位才真跑)─────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 端到端冒烟:若 `~/Polaris/models/sensevoice-small` + 测试 wav 就位则真跑识别并打印,
    /// 否则跳过(不让缺模型/缺音频卡测试)。运行:
    ///   cargo test --features desktop,voice-asr asr_smoke -- --nocapture
    #[test]
    fn asr_smoke() {
        let Some(dir) = model_dir() else { return };
        if !dir.join("model.int8.onnx").exists() {
            eprintln!("[asr_smoke] 跳过:模型未下载 {}", dir.display());
            return;
        }
        // 测试音频约定放 ~/Polaris/models/sensevoice-small/test_wavs/zh.wav
        let wav = dir.join("test_wavs").join("zh.wav");
        if !wav.exists() {
            eprintln!("[asr_smoke] 跳过:测试音频缺失 {}", wav.display());
            return;
        }
        let r = transcribe_file(&wav.to_string_lossy()).expect("识别失败");
        eprintln!("[asr_smoke] 原文 = {}", r.raw);
        eprintln!("[asr_smoke] 终稿 = {}  (档位 {}, {} 处改动, {}ms)", r.text, r.tier, r.changes.len(), r.ms);
        assert!(!r.raw.is_empty(), "识别结果为空");

        // 多语种:英文 wav 若在位也跑一遍
        let en = dir.join("test_wavs").join("en.wav");
        if en.exists() {
            if let Ok(re) = transcribe_file(&en.to_string_lossy()) {
                eprintln!("[asr_smoke] EN 原文 = {}", re.raw);
            }
        }
    }

    /// 完整管线体验:识别原文 + 防污染纠同音错(用种子词表)。
    /// 模拟「把话说成同音错词」→ 看防污染秒达档纠回。
    #[test]
    fn asr_pipeline_demo() {
        crate::voice::init(); // 装入种子词表(含 扣带式→codex / 北极心→北极星)
        // 模拟一段「听岔了」的转写:把 codex 听成「扣带式」、北极星听成「北极心」、群晖听成「群辉」
        let misheard = "帮我把设置改成扣带式那种形态，名字叫北极心，部署到群辉上";
        let r = crate::voice::anti_pollute(misheard);
        eprintln!("[pipeline] 听岔的原文 = {misheard}");
        eprintln!("[pipeline] 防污染终稿 = {}", r.text);
        for c in &r.changes {
            eprintln!("[pipeline]   纠正: {} → {} ({})", c.from, c.to, c.layer);
        }
        assert!(r.text.contains("codex"), "codex 未纠回: {}", r.text);
        assert!(r.text.contains("北极星"), "北极星 未纠回: {}", r.text);
        assert!(r.text.contains("群晖"), "群晖 未纠回: {}", r.text);
    }
}
