//! 自写最小 mp4 muxer(ISO/IEC 14496-12 / 14496-14 fragment 模式)
//!
//! 设计理由:mp4-rust 2.7 年未更新,写路径非流式(任务 b §2)。
//! 自写 500-800 行可控,落 ftyp/moov/moof/mdat/traf/trun 增量写盘。
//!
//! 本版给 trait + 骨架;P1.5 落流式增量写。

use crate::{Result, FxFrame};

/// mp4 muxer trait
///  - Mp4Muxer 真实实现(自写 BMFF,P1.5 落)
///  - 失败一律 ForgeError::NeedFallback → ffmpeg 兜底
pub trait Mp4Muxer: Send {
    /// 起始:写 ftyp + moov box
    fn begin(&mut self, width: u32, height: u32, fps: u32) -> Result<()>;
    /// 推一帧 H.264 编码后 NAL(Annex-B 格式)+ 对应时间戳(秒)
    fn push_video_nal(&mut self, nal: &[u8], pts_sec: f64) -> Result<()>;
    /// 推一帧音频(已编码 PCM/Opus 字节)
    fn push_audio(&mut self, samples: &[i16], sample_rate: u32, channels: u8) -> Result<()>;
    /// 收尾:flush + 落文件
    fn finish(self: Box<Self>, out_path: &str) -> Result<()>;
}

/// mp4 muxer stub(本期不实现,ForgeError::NeedFallback 让上层走 ffmpeg)
pub struct SimpleMp4Muxer;

impl SimpleMp4Muxer {
    pub fn new() -> Result<Self> {
        Err(crate::ForgeError::NeedFallback {
            reason: "SimpleMp4Muxer stub(P1.5 自写 BMFF 待实);forge_video 仍走 ffmpeg".into(),
        })
    }
}

impl Mp4Muxer for SimpleMp4Muxer {
    fn begin(&mut self, _w: u32, _h: u32, _fps: u32) -> Result<()> { unreachable!() }
    fn push_video_nal(&mut self, _nal: &[u8], _pts_sec: f64) -> Result<()> { unreachable!() }
    fn push_audio(&mut self, _samples: &[i16], _sr: u32, _ch: u8) -> Result<()> { unreachable!() }
    fn finish(self: Box<Self>, _out: &str) -> Result<()> { unreachable!() }
}

/// 把 FxFrame(RGBA 帧) + H.264Encoder 接到 Mp4Muxer 的三段流水线
/// 真实实现在 P1.5 落,本期给连接顺序
pub fn fx_to_mp4_pipeline(_frames: Vec<FxFrame>, _out_mp4: &str) -> Result<()> {
    Err(crate::ForgeError::NeedFallback {
        reason: "fx_to_mp4_pipeline P1.5 待实;forge_video::render_deck_fx_video 仍走 ffmpeg CLI".into(),
    })
}
