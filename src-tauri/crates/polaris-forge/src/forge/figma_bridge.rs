//! Figma 往返桥（回程半边）：用 Figma REST API 拉取文件节点树 + 图片填充 + 矢量 SVG 导出，
//! 前端(`src/lib/figmaPull.ts`)把节点树转成绝对定位 HTML 替换画布。
//! 去程走 html.to.design 社区插件（网页端可用），不自研写入端——REST API 不允许写文件，
//! 自研写入必须做 Figma 插件且网页端装不了开发版插件，性价比极差。
//!
//! 图片填充的临时 URL 两周会过期，必须当场下载内嵌成 data URI 再进 HTML。

use base64::Engine;
use std::collections::HashMap;
use std::io::Read;
use std::time::{Duration, Instant};

const API: &str = "https://api.figma.com";
/// 单张图片上限（再大内嵌进 HTML 会把产物撑爆）
const IMG_CAP_BYTES: usize = 4 * 1024 * 1024;
/// 所有内嵌资源总预算
const IMG_TOTAL_CAP: usize = 24 * 1024 * 1024;
/// 一次最多导出多少个矢量节点为 SVG
const SVG_ID_CAP: usize = 60;
/// 单次拉取最多尝试下载多少张图（含失败/超大，防慢图无界拖住阻塞线程）
const IMG_COUNT_CAP: usize = 40;
/// 整单下载 wall-clock 上限：超时后停止再尝试，返回已拿到的部分（server 侧 invoke 超时无法取消 spawn_blocking，必须自截止）
const DL_DEADLINE: Duration = Duration::from_secs(90);

/// 从整段 URL 或裸 key 里剥出 file key：
/// `figma.com/design/<key>/标题?...`、`/file/<key>/`、`/board/<key>/` 或直接粘 key。
fn file_key(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("参数无效:请先粘贴 Figma 文件链接".into());
    }
    for marker in ["/design/", "/file/", "/board/", "/proto/"] {
        if let Some(i) = s.find(marker) {
            let rest = &s[i + marker.len()..];
            let key: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            if !key.is_empty() {
                return Ok(key);
            }
        }
    }
    if !s.contains('/') && s.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Ok(s.to_string());
    }
    Err("参数无效:识别不了这个链接，请粘贴完整的 Figma 文件 URL（figma.com/design/…）".into())
}

fn get_json(url: &str, token: &str) -> Result<serde_json::Value, String> {
    let resp = ureq::get(url)
        .set("X-Figma-Token", token)
        .timeout(std::time::Duration::from_secs(60))
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(403, _) => {
                "Figma 拒绝访问(403)：令牌无效，或它没有这个文件的查看权限".to_string()
            }
            ureq::Error::Status(404, _) => "找不到该 Figma 文件(404)：检查链接是否正确".to_string(),
            ureq::Error::Status(429, _) => "Figma 限流(429)：稍等几十秒再试".to_string(),
            other => format!("请求 Figma 失败: {other}"),
        })?;
    resp.into_json::<serde_json::Value>()
        .map_err(|e| format!("解析 Figma 响应失败: {e}"))
}

/// 下载并转 data URI；超单图/总预算/整单截止返回 None（跳过该图，不整单失败）。
fn download_data_uri(url: &str, budget_left: &mut usize, deadline: Instant) -> Option<String> {
    // 单图超时钳到「剩余整单时间」与 60s 的较小者：截止已过则直接跳过
    let remaining = deadline.checked_duration_since(Instant::now())?;
    let resp = ureq::get(url)
        .timeout(remaining.min(Duration::from_secs(60)))
        .call()
        .ok()?;
    let mime = {
        let ct = resp.content_type();
        if ct.is_empty() {
            "image/png".to_string()
        } else {
            ct.to_string()
        }
    };
    let mut buf: Vec<u8> = Vec::new();
    resp.into_reader()
        .take((IMG_CAP_BYTES + 1) as u64)
        .read_to_end(&mut buf)
        .ok()?;
    if buf.is_empty() || buf.len() > IMG_CAP_BYTES || buf.len() > *budget_left {
        return None;
    }
    *budget_left -= buf.len();
    Some(format!(
        "data:{};base64,{}",
        mime,
        base64::engine::general_purpose::STANDARD.encode(&buf)
    ))
}

#[derive(serde::Serialize)]
pub struct FigmaPull {
    /// GET /v1/files/:key 的原始 JSON（节点树），转换逻辑在前端好迭代
    pub doc: serde_json::Value,
    /// imageRef → data URI（图片填充；临时 URL 会过期所以当场内嵌）
    pub images: HashMap<String, String>,
}

fn figma_pull_sync(file: &str, token: &str) -> Result<FigmaPull, String> {
    let key = file_key(file)?;
    let doc = get_json(&format!("{API}/v1/files/{key}"), token)?;
    let mut images = HashMap::new();
    // 图片填充清单（失败不挡主流程：没有图片的文件这里可能 404/空）
    if let Ok(fills) = get_json(&format!("{API}/v1/files/{key}/images"), token) {
        let mut budget = IMG_TOTAL_CAP;
        let deadline = Instant::now() + DL_DEADLINE;
        let mut attempts = 0usize;
        if let Some(map) = fills.pointer("/meta/images").and_then(|v| v.as_object()) {
            for (image_ref, u) in map {
                if attempts >= IMG_COUNT_CAP || Instant::now() >= deadline {
                    break;
                }
                if let Some(url) = u.as_str() {
                    if url.is_empty() {
                        continue;
                    }
                    attempts += 1; // 失败/超大也计数，防慢图刷满尝试上限外无界重试
                    if let Some(uri) = download_data_uri(url, &mut budget, deadline) {
                        images.insert(image_ref.clone(), uri);
                    }
                }
            }
        }
    }
    Ok(FigmaPull { doc, images })
}

fn figma_export_svgs_sync(
    file: &str,
    ids: &[String],
    token: &str,
) -> Result<HashMap<String, String>, String> {
    let key = file_key(file)?;
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let joined = ids
        .iter()
        .take(SVG_ID_CAP)
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let v = get_json(
        &format!("{API}/v1/images/{key}?ids={joined}&format=svg"),
        token,
    )?;
    let mut out = HashMap::new();
    let mut budget = IMG_TOTAL_CAP;
    let deadline = Instant::now() + DL_DEADLINE;
    let mut attempts = 0usize;
    if let Some(map) = v.pointer("/images").and_then(|x| x.as_object()) {
        for (id, u) in map {
            if attempts >= IMG_COUNT_CAP || Instant::now() >= deadline {
                break;
            }
            if let Some(url) = u.as_str() {
                if url.is_empty() {
                    continue;
                }
                attempts += 1;
                if let Some(uri) = download_data_uri(url, &mut budget, deadline) {
                    out.insert(id.clone(), uri);
                }
            }
        }
    }
    Ok(out)
}

// 桌面端 async + spawn_blocking：几十秒的网络往返绝不能钉在主线程（同步命令冻 UI 的老坑）。
// server flavor dispatch 本就在 spawn_blocking 里跑，保持同步直调。

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn figma_pull(file: String, token: String) -> Result<FigmaPull, String> {
    tauri::async_runtime::spawn_blocking(move || figma_pull_sync(&file, &token))
        .await
        .map_err(|e| format!("后台任务失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
pub fn figma_pull(file: String, token: String) -> Result<FigmaPull, String> {
    figma_pull_sync(&file, &token)
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn figma_export_svgs(
    file: String,
    ids: Vec<String>,
    token: String,
) -> Result<HashMap<String, String>, String> {
    tauri::async_runtime::spawn_blocking(move || figma_export_svgs_sync(&file, &ids, &token))
        .await
        .map_err(|e| format!("后台任务失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
pub fn figma_export_svgs(
    file: String,
    ids: Vec<String>,
    token: String,
) -> Result<HashMap<String, String>, String> {
    figma_export_svgs_sync(&file, &ids, &token)
}

#[cfg(test)]
mod tests {
    use super::file_key;

    #[test]
    fn key_from_design_url() {
        assert_eq!(
            file_key("https://www.figma.com/design/AbC123xyz/我的页面?node-id=1-2").unwrap(),
            "AbC123xyz"
        );
    }
    #[test]
    fn key_from_file_url() {
        assert_eq!(file_key("https://www.figma.com/file/K9/t").unwrap(), "K9");
    }
    #[test]
    fn bare_key_ok() {
        assert_eq!(file_key("AbC123").unwrap(), "AbC123");
    }
    #[test]
    fn garbage_rejected() {
        assert!(file_key("https://example.com/nope").is_err());
        assert!(file_key("").is_err());
    }
}
