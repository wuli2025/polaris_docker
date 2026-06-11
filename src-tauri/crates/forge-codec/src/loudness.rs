//! EBU R128 响度归一化(ebur128 crate)
//!
//! 选型理由(任务 b §3):
//!   - ebur128 0.1.10 (Sebastian Dröge 维护,GStreamer 项目维护者)
//!   - 通过 EBU TECH 3341/3342 测试,API/ABI 兼容 libebur128
//!   - BSD-2-Clause 许可
//!
//! 模式:旁链 = 先 analyze 整段得 integrated LUFS,再按目标 -23 LUFS(ITU-R BS.1770-4
//! 流媒体标准)gain-scaling。

use crate::{ForgeError, Result};

/// 响度分析器 + gain 缩放
/// P1.5 落 ebur128::EbuR128 真实调用;本期给 stub
pub struct LoudnessAnalyzer {
    pub target_lufs: f64,    // 默认 -23(ITU-R BS.1770-4 流媒体)
    pub true_peak_db: f64,   // 默认 -1 dBTP 防爆音
}

impl Default for LoudnessAnalyzer {
    fn default() -> Self {
        Self { target_lufs: -23.0, true_peak_db: -1.0 }
    }
}

impl LoudnessAnalyzer {
    pub fn new() -> Self { Self::default() }

    /// 分析 PCM 整段(samples interleave,channels=1|2,rate=48000)
    /// 返 integrated LUFS(标量)
    pub fn analyze(&self, _pcm: &[i16], _sample_rate: u32, _channels: u8) -> Result<f64> {
        // P1.5:ebur128::EbuR128::new(channels, rate, Mode::I).add_frames_* → integrated_loudness()
        Err(ForgeError::NeedFallback {
            reason: "LoudnessAnalyzer stub(P1.5 待实);forge_video 暂不响度归一,直接合轨".into(),
        })
    }

    /// 按 target_lufs 缩放 PCM(线性 gain)
    pub fn scale_to_target(&self, pcm: &mut [i16], current_lufs: f64) -> Result<()> {
        let gain_db = self.target_lufs - current_lufs;
        let gain = (10.0_f64).powf(gain_db / 20.0);
        for s in pcm.iter_mut() {
            let v = (*s as f64) * gain;
            *s = v.clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        }
        Ok(())
    }
}
