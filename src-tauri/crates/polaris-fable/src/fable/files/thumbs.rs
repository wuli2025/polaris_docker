use super::*;

// ───────────────────────── 缩略图 / 首帧 ─────────────────────────

pub(crate) const IMG_DECODE: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "bmp"];
pub(crate) const VIDEO_EXTS: &[&str] = &[
    "mp4", "mkv", "mov", "avi", "wmv", "flv", "webm", "m4v", "mpg", "mpeg",
];

pub(crate) fn ext_of(path: &str) -> String {
    Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

pub(crate) fn jpeg_data_url(rgb: &image::DynamicImage) -> Result<String, String> {
    let mut buf = Vec::new();
    image::DynamicImage::ImageRgb8(rgb.to_rgb8())
        .write_to(
            &mut std::io::Cursor::new(&mut buf),
            image::ImageFormat::Jpeg,
        )
        .map_err(|e| format!("缩略图编码失败: {e}"))?;
    Ok(format!("data:image/jpeg;base64,{}", b64(&buf)))
}

/// 生成(或读缓存)缩略图,统一返回 data URL;无法出图返回 None(前端落类型图标)。
pub fn thumb(abspath: String, max: u32) -> Result<Option<String>, String> {
    // 显示路径可能是 GBK 名解码出的 UTF-8;还原成磁盘真实路径再读(否则 GBK 图片出不了图)。
    let real = crate::fable::reencode_fs_path(&abspath);
    let p = real.as_path();
    if !p.is_file() {
        return Ok(None);
    }
    let ext = ext_of(&abspath);
    let max = max.clamp(96, 640);
    // 缓存键 = 路径 + mtime + size + 边长
    let meta = std::fs::metadata(p).map_err(|e| e.to_string())?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let key = hash_key(&[
        &abspath,
        &mtime.to_string(),
        &meta.len().to_string(),
        &max.to_string(),
    ]);
    let cache = thumbs_dir().map(|d| d.join(format!("{key}.jpg")));
    if let Some(c) = &cache {
        if let Ok(bytes) = std::fs::read(c) {
            return Ok(Some(format!("data:image/jpeg;base64,{}", b64(&bytes))));
        }
    }

    let dyn_img: Option<image::DynamicImage> = if IMG_DECODE.contains(&ext.as_str()) {
        image::open(p).ok().map(|i| i.thumbnail(max, max))
    } else if VIDEO_EXTS.contains(&ext.as_str()) {
        video_frame(p, max)
    } else {
        None
    };
    let Some(img) = dyn_img else { return Ok(None) };

    // 写缓存(best-effort)+ 返回 data URL
    if let Some(c) = &cache {
        if let Some(dir) = c.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut buf = Vec::new();
        if image::DynamicImage::ImageRgb8(img.to_rgb8())
            .write_to(
                &mut std::io::Cursor::new(&mut buf),
                image::ImageFormat::Jpeg,
            )
            .is_ok()
        {
            let _ = std::fs::write(c, &buf);
            return Ok(Some(format!("data:image/jpeg;base64,{}", b64(&buf))));
        }
    }
    jpeg_data_url(&img).map(Some)
}

/// 视频首帧:best-effort 调系统/渲染版 ffmpeg 抽第 1 秒一帧 → image 解码缩放。缺 ffmpeg 返回 None。
///
/// **必须带超时**:损坏 / 半截 / 格式刁钻的视频会让 ffmpeg 永久挂死,而本函数被文件中心 grid
/// 逐图调用(常跑在多核缩略图线程上)—— 一个挂死的 ffmpeg 进程就能拖死整个 grid 加载、表现为
/// 「文件中心卡死」。复用 [`crate::runtime::run_with_timeout`]:超 15s 即杀进程返回,绝不永久阻塞。
pub(crate) fn video_frame(p: &Path, max: u32) -> Option<image::DynamicImage> {
    // tmp 名只含路径哈希时,同一视频的并发抽帧(warm_thumbs 多核预热 vs file_thumb 请求不同
    // max)会指向同一文件 → 两个 ffmpeg 互相踩踏、其一解到半截图还顺手删掉对方的成果。
    // 掺入目标尺寸 + 进程内原子递增序号,每次调用独占自己的 tmp。
    static VF_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = VF_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!(
        "polaris-vf-{}-{max}-{seq}.jpg",
        hash_key(&[&p.to_string_lossy()])
    ));
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args([
        "-y",
        "-ss",
        "1",
        "-i",
        &p.to_string_lossy(),
        "-frames:v",
        "1",
        "-vf",
        &format!("scale={max}:-1"),
        &tmp.to_string_lossy(),
    ]);
    // 超时即杀进程(Mac/Win/Docker 同构):坏视频再也卡不死缩略图线程。
    if crate::runtime::run_with_timeout(cmd, 15, "ffmpeg 视频首帧").is_err() {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    let img = image::open(&tmp).ok().map(|i| i.thumbnail(max, max));
    let _ = std::fs::remove_file(&tmp);
    img
}
