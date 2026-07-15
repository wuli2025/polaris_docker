//! ffmpeg CLI 兜底层(P0 仍走它,forge-codec 实现后改 router)
//!
//! 设计:forge_video::render_deck_fx_video 当前直接调 ffmpeg;
//! forge-codec H264Encoder/SimpleMp4Muxer/OpusEncoder 真实实现后,改为:
//!   1) 先试 self.encode_x → 成功落 mp4 返
//!   2) 失败 NeedFallback → 调本模块的 run_ffmpeg_fallback(args)
//!   3) 报错清晰:"forge-codec 不可用(openh264 缺 / ffmpeg 缺 / 编解码不支持),
//!      fallback 到 ffmpeg,输出在 XXX"

use crate::{ForgeError, Result};
use std::path::Path;
use std::process::Command;

/// 探测 ffmpeg 是否在 PATH
pub fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 兜底:把 PNG 帧序列 + 音轨合成 mp4(原 forge_video 路径)
pub fn run_ffmpeg_fallback(
    frames_dir: &Path,
    audio: Option<&Path>,
    out_mp4: &Path,
    fps: u32,
) -> Result<()> {
    if !ffmpeg_available() {
        return Err(ForgeError::NeedFallback {
            reason: "ffmpeg 不在 PATH,无法 fallback;Docker 镜像应有 /usr/bin/ffmpeg".into(),
        });
    }
    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y")
        .arg("-framerate")
        .arg(fps.to_string())
        .arg("-i")
        .arg(frames_dir.join("f%05d.png"));
    if let Some(a) = audio {
        cmd.arg("-i").arg(a);
    }
    cmd.arg("-c:v")
        .arg("libx264")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-preset")
        .arg("veryfast")
        .arg("-crf")
        .arg("23");
    if audio.is_some() {
        cmd.arg("-c:a").arg("aac").arg("-b:a").arg("128k");
    }
    cmd.arg(out_mp4);
    let out = cmd.output().map_err(|e| ForgeError::Codec {
        encoder: "ffmpeg_fallback".into(),
        reason: format!("ffmpeg 启动失败: {e}"),
    })?;
    if !out.status.success() {
        return Err(ForgeError::Codec {
            encoder: "ffmpeg_fallback".into(),
            reason: format!(
                "ffmpeg exit {}: {}",
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr)
                    .chars()
                    .take(400)
                    .collect::<String>()
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffmpeg_probe_is_safe() {
        // 只探测,不假定结果(可能在没 ffmpeg 的环境跑)
        let _ = ffmpeg_available();
    }
}
