//! Polaris Forge · codec 板块
//!
//! 替 ffmpeg CLI 的 Rust 自研路线。设计见 workspace/research-notes/task-b.md。
//! 三层抽象:
//!   trait Encoder  (H.264 视频,mp4 muxer)
//!   trait AudioEncoder (Opus 音频)
//!   trait Loudness (ebur128 响度归一化)
//!   trait FxFrameSink (fx → codec 帧序列桥,见 task-c §C.4)
//!
//! 工业级化:
//!   - thiserror 统一 ForgeError(任务 c §D.1)
//!   - main impl 失败一律返回 CodecError::NeedFallback → 上层调 ffmpeg CLI 兜底
//!   - 自写最小 BMFF mp4 muxer(不引 mp4-rust 2.7 年停滞,任务 b §2)
//!   - openh264-rs source feature 静态捆绑(绕开 Cisco BINARY_LICENSE 商用分发约束)

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Forge 统一错误模型(任务 c §D.1,通用层放 P2 合并,本版只列 codec 分支)
#[derive(Debug, Error, Serialize, Deserialize)]
pub enum ForgeError {
    #[error("io: {path}: {src}")]
    Io { path: String, src: String },
    #[error("codec: {encoder}: {reason}")]
    Codec { encoder: String, reason: String },
    #[error("codec fallback needed: {reason}")]
    NeedFallback { reason: String },
    #[error("invalid frame: {reason}")]
    InvalidFrame { reason: String },
    #[error("audio: {reason}")]
    Audio { reason: String },
    #[error("loudness: {reason}")]
    Loudness { reason: String },
}

pub type Result<T> = std::result::Result<T, ForgeError>;

/// fx → codec 帧序列桥(任务 c §C.4.1 定义签名,P2-B 填实现)
/// forge-codec 实现 H264Sink;forge_capture 端按 t_ms 顺序发帧。
#[derive(Debug, Clone)]
pub struct FxFrame {
    pub t_ms: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub keyframe: bool,
}

#[async_trait::async_trait]
pub trait FxFrameSink: Send {
    async fn write_frame(&mut self, f: FxFrame) -> Result<()>;
    async fn finish(self: Box<Self>) -> Result<Vec<u8>>;
}

pub mod audio;
pub mod encoder;
pub mod ffmpeg_fallback;
pub mod loudness;
pub mod muxer;
