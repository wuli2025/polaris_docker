//! 语音输入实时链路 —— 按住热键说话 → 流式上字 → 松手识别注入。
//!
//! 仅在 `voice-live` feature 下编译(麦克风 cpal/全局热键 rdev/注入 enigo 是桌面专属;
//! 该 feature 自动带上 `voice-asr` 识别核)。Docker/浏览器不编译本模块,走「上传音频→识别」。
//! 链路:rdev 全局热键(右 Alt 按住/松手) → cpal 采集 → 滑窗模拟流式(每 ~350ms
//! 重识别一次 emit `voice:partial`) → 松手整段识别 + 防污染 → enigo 注入焦点应用 +
//! emit `voice:final`。前端 VoiceOverlay 听这些事件画浮窗。
//!
//! 设计照 Handy(cjpais/Handy)的 Rust/Tauri 经验:全局钩子→采集→ASR→enigo 注入。

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use enigo::{Enigo, Keyboard, Settings};
use serde_json::json;
use tauri::{AppHandle, Emitter};

enum Ctrl {
    Start,
    Stop,
}

static STARTED: AtomicBool = AtomicBool::new(false);
static ENABLED: AtomicBool = AtomicBool::new(false);
static CTRL_TX: Lazy<Mutex<Option<Sender<Ctrl>>>> = Lazy::new(|| Mutex::new(None));

/// 启用实时语音输入:首次调用拉起热键线程 + 采集/识别管理线程(之后幂等,仅置 ENABLED)。
pub fn start(app: AppHandle) -> Result<(), String> {
    // 模型未就位先给清晰错误(别等按下热键才闷声失败)。
    crate::voice::asr::ensure_recognizer()?;
    ENABLED.store(true, Ordering::SeqCst);
    if STARTED.swap(true, Ordering::SeqCst) {
        return Ok(()); // 线程已在,仅重新启用
    }
    let target = key_from_cfg();
    let (tx, rx) = channel::<Ctrl>();
    *CTRL_TX.lock() = Some(tx.clone());
    spawn_manager(app, rx);
    spawn_hotkey(tx, target);
    Ok(())
}

/// 停用:热键事件被忽略(rdev::listen 无法干净停止,故用 ENABLED 闸门)。
pub fn stop() {
    ENABLED.store(false, Ordering::SeqCst);
}

// ───────────────────────── 听写模式(输入框麦克风按钮)─────────────────────────
// 与全局「按住右Alt注入焦点应用」不同:听写是显式 toggle(点麦克风/右Alt 开,再点关),
// 录音转写后**不 enigo 注入**,而是 emit `voice:dictation` 把文字交给聊天输入框。

static DICTATING: AtomicBool = AtomicBool::new(false);

/// 开始听写(幂等):录音 → 每 350ms emit `voice:partial` → 直到 stop。
pub fn dictate_start(app: AppHandle) -> Result<(), String> {
    crate::voice::asr::ensure_recognizer()?;
    if DICTATING.swap(true, Ordering::SeqCst) {
        return Ok(()); // 已在听写
    }
    std::thread::spawn(move || dictate_session(app));
    Ok(())
}

/// 停止听写 → 触发整段识别 + 防污染 + emit `voice:dictation { text }`。
pub fn dictate_stop() {
    DICTATING.store(false, Ordering::SeqCst);
}

pub fn dictating() -> bool {
    DICTATING.load(Ordering::SeqCst)
}

fn dictate_session(app: AppHandle) {
    let buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let (stream, in_sr) = match build_stream(buf.clone()) {
        Ok(x) => x,
        Err(e) => {
            DICTATING.store(false, Ordering::SeqCst);
            let _ = app.emit("voice:dictation", json!({ "error": e }));
            return;
        }
    };
    if let Err(e) = stream.play() {
        DICTATING.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "voice:dictation",
            json!({ "error": format!("启动采集失败: {e}") }),
        );
        return;
    }
    let _ = app.emit("voice:listening", true);
    while DICTATING.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(350));
        let samples = snapshot_16k(&buf, in_sr);
        if samples.len() > (16000.0 * 0.3) as usize {
            if let Ok(r) = crate::voice::asr::transcribe_samples(16000, &samples) {
                let _ = app.emit("voice:partial", json!({ "text": r.raw }));
            }
        }
    }
    drop(stream);
    let samples = snapshot_16k(&buf, in_sr);
    let _ = app.emit("voice:listening", false);
    if samples.len() < (16000.0 * 0.2) as usize {
        let _ = app.emit("voice:dictation", json!({ "text": "", "cancelled": true }));
        return;
    }
    match crate::voice::asr::transcribe_samples(16000, &samples) {
        Ok(r) => {
            // 开了 AI 整形则松手后再过一遍 LLM(去语气词/顺句/列表化);默认关 = 零额外延迟。
            let polished = crate::voice::polish_if_enabled(&r.text);
            let _ = app.emit(
                "voice:dictation",
                json!({ "text": polished, "raw": r.raw, "polished": polished != r.text }),
            );
        }
        Err(e) => {
            let _ = app.emit("voice:dictation", json!({ "error": e }));
        }
    }
}

fn key_from_cfg() -> rdev::Key {
    use rdev::Key;
    match crate::voice::voice_config_get().hotkey.as_str() {
        "rctrl" => Key::ControlRight,
        "capslock" => Key::CapsLock,
        "f9" => Key::F9,
        // 右 Alt 在 Windows 多为 AltGr;默认。
        _ => Key::AltGr,
    }
}

// ───────────────────────── 热键线程 ─────────────────────────

fn spawn_hotkey(tx: Sender<Ctrl>, target: rdev::Key) {
    std::thread::spawn(move || {
        let down = AtomicBool::new(false);
        let cb = move |ev: rdev::Event| match ev.event_type {
            rdev::EventType::KeyPress(k) if k == target => {
                // 去自动重复:只在「从未按下→按下」沿触发
                if !down.swap(true, Ordering::SeqCst) && ENABLED.load(Ordering::SeqCst) {
                    let _ = tx.send(Ctrl::Start);
                }
            }
            rdev::EventType::KeyRelease(k) if k == target => {
                if down.swap(false, Ordering::SeqCst) && ENABLED.load(Ordering::SeqCst) {
                    let _ = tx.send(Ctrl::Stop);
                }
            }
            _ => {}
        };
        if let Err(e) = rdev::listen(cb) {
            eprintln!("[voice] 全局热键监听失败: {e:?}");
        }
    });
}

// ───────────────────────── 采集/识别管理线程 ─────────────────────────

fn spawn_manager(app: AppHandle, rx: Receiver<Ctrl>) {
    std::thread::spawn(move || loop {
        match rx.recv() {
            Ok(Ctrl::Start) => record_and_recognize(&app, &rx),
            Ok(Ctrl::Stop) => {} // 空闲时的杂散 Stop,忽略
            Err(_) => break,     // 发送端全丢弃 → 退出
        }
    });
}

/// 一次「按住—说话—松手」会话:建流采集 → 滑窗流式 partial → 松手终识别+防污染+注入。
fn record_and_recognize(app: &AppHandle, rx: &Receiver<Ctrl>) {
    let buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let (stream, in_sr) = match build_stream(buf.clone()) {
        Ok(x) => x,
        Err(e) => {
            let _ = app.emit("voice:final", json!({ "error": e }));
            return;
        }
    };
    if let Err(e) = stream.play() {
        let _ = app.emit(
            "voice:final",
            json!({ "error": format!("启动采集失败: {e}") }),
        );
        return;
    }
    let _ = app.emit("voice:listening", true);

    // 滑窗模拟流式:每 ~350ms 重识别当前累积音频,emit partial。
    loop {
        match rx.recv_timeout(Duration::from_millis(350)) {
            Ok(Ctrl::Stop) => break,
            Ok(Ctrl::Start) => {} // 录音中再来的 Start,忽略
            Err(RecvTimeoutError::Timeout) => {
                let samples = snapshot_16k(&buf, in_sr);
                if samples.len() > (16000.0 * 0.3) as usize {
                    if let Ok(r) = crate::voice::asr::transcribe_samples(16000, &samples) {
                        let _ = app.emit("voice:partial", json!({ "text": r.raw }));
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    drop(stream); // 停止采集
    let samples = snapshot_16k(&buf, in_sr);
    let _ = app.emit("voice:listening", false);

    if samples.len() < (16000.0 * 0.2) as usize {
        // 太短(误触/没说话):不注入
        let _ = app.emit(
            "voice:final",
            json!({ "text": "", "raw": "", "cancelled": true }),
        );
        return;
    }
    match crate::voice::asr::transcribe_samples(16000, &samples) {
        Ok(r) => {
            // 松手后整形(若开启),注入的是「想写的字」而非「说的话」;失败回落原文。
            let polished = crate::voice::polish_if_enabled(&r.text);
            let _ = app.emit(
                "voice:final",
                json!({
                    "text": polished,
                    "raw": r.raw,
                    "changes": r.changes,
                    "tier": r.tier,
                    "ms": r.ms,
                    "polished": polished != r.text,
                }),
            );
            inject_text(&polished);
        }
        Err(e) => {
            let _ = app.emit("voice:final", json!({ "error": e }));
        }
    }
}

/// 取当前缓冲快照并重采样到 16k 单声道。
fn snapshot_16k(buf: &Arc<Mutex<Vec<f32>>>, in_sr: u32) -> Vec<f32> {
    let snap = buf.lock().clone();
    resample_to_16k(&snap, in_sr)
}

/// 线性插值重采样到 16kHz。
fn resample_to_16k(samples: &[f32], in_sr: u32) -> Vec<f32> {
    if in_sr == 16000 || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = 16000.0 / in_sr as f32;
    let out_len = ((samples.len() as f32) * ratio) as usize;
    let mut out = Vec::with_capacity(out_len);
    let last = samples.len() - 1;
    for i in 0..out_len {
        let src = i as f32 / ratio;
        let i0 = src.floor() as usize;
        let i1 = (i0 + 1).min(last);
        let frac = src - i0 as f32;
        out.push(samples[i0] * (1.0 - frac) + samples[i1] * frac);
    }
    out
}

/// 累积下混成单声道,带 ~30s 上限防失控增长。
fn push_mono(buf: &Arc<Mutex<Vec<f32>>>, data: &[f32], ch: usize, cap: usize) {
    let mut b = buf.lock();
    if b.len() >= cap {
        return;
    }
    if ch <= 1 {
        b.extend_from_slice(data);
    } else {
        for frame in data.chunks(ch) {
            let s: f32 = frame.iter().sum::<f32>() / ch as f32;
            b.push(s);
        }
    }
}

fn build_stream(buf: Arc<Mutex<Vec<f32>>>) -> Result<(cpal::Stream, u32), String> {
    let host = cpal::default_host();
    let dev = host.default_input_device().ok_or("没有可用麦克风设备")?;
    let cfg = dev
        .default_input_config()
        .map_err(|e| format!("读麦克风配置失败: {e}"))?;
    let in_sr = cfg.sample_rate().0;
    let ch = cfg.channels() as usize;
    let cap = (in_sr as usize) * ch.max(1) * 30; // ~30s
    let err_fn = |e| eprintln!("[voice] 采集流错误: {e}");
    let sc: cpal::StreamConfig = cfg.config();

    let stream = match cfg.sample_format() {
        cpal::SampleFormat::F32 => {
            let b = buf.clone();
            dev.build_input_stream(
                &sc,
                move |data: &[f32], _: &_| push_mono(&b, data, ch, cap),
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let b = buf.clone();
            dev.build_input_stream(
                &sc,
                move |data: &[i16], _: &_| {
                    let f: Vec<f32> = data.iter().map(|s| *s as f32 / 32768.0).collect();
                    push_mono(&b, &f, ch, cap);
                },
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let b = buf.clone();
            dev.build_input_stream(
                &sc,
                move |data: &[u16], _: &_| {
                    let f: Vec<f32> = data
                        .iter()
                        .map(|s| (*s as f32 - 32768.0) / 32768.0)
                        .collect();
                    push_mono(&b, &f, ch, cap);
                },
                err_fn,
                None,
            )
        }
        other => return Err(format!("不支持的采样格式: {other:?}")),
    }
    .map_err(|e| format!("建采集流失败: {e}"))?;
    Ok((stream, in_sr))
}

/// 把终稿「敲」进当前焦点应用。失败仅日志(注入兜底/受保护输入框由前端提示走剪贴板)。
fn inject_text(text: &str) {
    if text.trim().is_empty() {
        return;
    }
    match Enigo::new(&Settings::default()) {
        Ok(mut e) => {
            if let Err(err) = e.text(text) {
                eprintln!("[voice] 注入失败: {err}");
            }
        }
        Err(e) => eprintln!("[voice] enigo 初始化失败: {e}"),
    }
}
