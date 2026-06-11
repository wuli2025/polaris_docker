//! 音频编解码(PCM ↔ Opus 编码 + symphonia 解码 MP3/AAC/FLAC/Vorbis/ALAC/WAV)
//!
//! 选型理由(任务 b §5):
//!   - symphonia 0.6 纯解码(MP3/AAC-LC/FLAC/Vorbis/ALAC/WAV 全覆盖,MPL-2.0)
//!   - audiopus 0.2 Opus 编码(libopus 绑定,5 年停滞可接受,spec 极稳)
//!   - 拒绝 FDK-AAC(商用付费给 Fraunhofer) + 拒绝 LAME MP3(LGPL 灰区)

use crate::{ForgeError, Result};

/// PCM → Opus 编码器
/// 真实实现 P1.5 落 audiopus::Encoder 链;本期给 stub
pub struct OpusEncoder {
    pub sample_rate: u32,  // 48000
    pub channels: u8,      // 1 or 2
    pub bitrate_bps: u32,  // 默认 64000
}

impl OpusEncoder {
    pub fn new(sample_rate: u32, channels: u8, bitrate_bps: u32) -> Result<Self> {
        // P1.5:audiopus::Encoder::create_state() 真实实现
        Ok(Self { sample_rate, channels, bitrate_bps })
    }

    /// 编码 20ms 一帧 PCM(960 samples per channel @48kHz)
    pub fn encode_frame(&mut self, _pcm: &[i16]) -> Result<Vec<u8>> {
        // P1.5:encoder.encode_float / encode → Vec<u8>
        // 本期 stub:返 NeedFallback
        Err(ForgeError::NeedFallback {
            reason: "OpusEncoder stub(P1.5 待实);forge_tts 仍输出 mp3(下游 ffmpeg 合轨)".into(),
        })
    }
}

/// symphonia 解码器(P1.5 落 symphonia::core::audio::Decoder)
/// 本期给 stub
pub struct SymphoniaDecoder;

impl SymphoniaDecoder {
    pub fn open(_path: &str) -> Result<Self> {
        Ok(Self)
    }

    pub fn decode_all_pcm(&mut self) -> Result<(Vec<i16>, u32, u8)> {
        // (samples, sample_rate, channels)
        Err(ForgeError::NeedFallback {
            reason: "SymphoniaDecoder stub(P1.5 待实);forge_video 仍用 ffmpeg 抽音".into(),
        })
    }
}
