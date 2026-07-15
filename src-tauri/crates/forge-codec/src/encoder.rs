//! H.264 视频编码器 + mp4 muxer 桥
//!
//! 实现路线:
//!   H264Encoder 走 openh264-rs 静态捆绑 → 自写最小 BMFF muxer(ISO/IEC 14496-12)
//!   任何环节失败 → return ForgeError::NeedFallback → 上层 ffmpeg CLI 兜底
//!
//! 接口定义在本版稳定,实现分两轮:
//!   P1.5 落 H264Encoder 骨架(能编出合法 H.264 NAL 流,落 .h264 文件即可)
//!   P2   落 BMFF muxer,完整 .mp4 输出

use crate::{ForgeError, Result};

/// H.264 编码配置(对齐 openh264 默认值 + Polaris 视频参数)
#[derive(Debug, Clone)]
pub struct EncodeConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_bps: u32, // openh264 推荐 1.5-2x 目标(同码率质量差 libx264 medium 2-3dB)
    pub keyframe_interval: u32, // 默认 60 帧(2s@30fps)
}

impl Default for EncodeConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30,
            bitrate_bps: 4_000_000,
            keyframe_interval: 60,
        }
    }
}

/// H.264 编码器 trait
///  - H264Encoder 真实实现(openh264-rs,P1.5 落)
///  - FfmpegEncoder 逃生口(本期给 stub,实现见 ffmpeg_fallback.rs)
#[async_trait::async_trait]
pub trait VideoEncoder: Send {
    async fn encode_rgba(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
        keyframe: bool,
    ) -> Result<Vec<u8>>;
    async fn finish(self: Box<Self>) -> Result<Vec<u8>>;
}

/// openh264-rs 真实编码器(P1.5 填实现)
///  本版给 stub:直接返 NeedFallback,让链路优先走 ffmpeg 兜底验证
pub struct H264Encoder {
    pub cfg: EncodeConfig,
    pub nals: Vec<u8>, // 占位,真实实现时连续 NAL 流
}

impl H264Encoder {
    pub fn new(cfg: EncodeConfig) -> Result<Self> {
        // P1.5:openh264::OpenH264API::new() 拉 encoder,落 nals Vec<H264NAL>
        // 当前 stub:返 NeedFallback,让上层先调 ffmpeg_fallback
        Err(ForgeError::NeedFallback {
            reason: "H264Encoder stub(P1.5 待实);forge_video 仍走 ffmpeg_fallback".into(),
        })
    }
}

#[async_trait::async_trait]
impl VideoEncoder for H264Encoder {
    async fn encode_rgba(
        &mut self,
        _rgba: &[u8],
        _w: u32,
        _h: u32,
        _keyframe: bool,
    ) -> Result<Vec<u8>> {
        Err(ForgeError::NeedFallback {
            reason: "H264Encoder stub".into(),
        })
    }
    async fn finish(self: Box<Self>) -> Result<Vec<u8>> {
        Err(ForgeError::NeedFallback {
            reason: "H264Encoder stub".into(),
        })
    }
}
