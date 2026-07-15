//! 对话附件(拖拽上传/剪贴板贴图)与附件类型识别。
//! (从 chat.rs 纯移动拆出, 逻辑零变化)

use crate::{conv, convert};
use serde::Serialize;
use std::path::{Path, PathBuf};

use super::artifacts::conversation_dir;

// ───────────────────────── 对话附件 (拖拽上传) ─────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachedFile {
    pub name: String,
    /// 复制后在会话 uploads 目录里的绝对路径 (正斜杠)
    pub path: String,
    /// text | image | pdf | office | binary —— 前端选图标用
    pub kind: String,
    pub size: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 对话拖拽上传:把文件复制进「会话 uploads 目录」,返回附件清单。
/// 与「知识库上传」是两条不同的路径 —— 这里只把文件挂到当前对话,
/// 前端发送时把这些绝对路径写进 prompt,claude 用 Read 工具按需读取。
// command(async): 最多 50 个文件复制 + PDF 文本提取是重 IO, 同步命令会钉住 UI 主线程
// (v1.5.2 同族问题)。属性形式挪到工作线程, fn 保持同步签名(server dispatch 直调不受影响)。
#[cfg_attr(feature = "desktop", tauri::command(async))]
pub fn chat_attach_files(conversation_id: Option<String>, paths: Vec<String>) -> Vec<AttachedFile> {
    const MAX: usize = 50;
    if let Some(cid) = conversation_id.as_deref() {
        if let Err(e) = conv::ensure_conversation_writable(cid) {
            return paths
                .iter()
                .take(MAX)
                .map(|p| AttachedFile {
                    name: file_name_of(Path::new(p)),
                    path: String::new(),
                    kind: "binary".into(),
                    size: 0,
                    ok: false,
                    error: Some(e.clone()),
                })
                .collect();
        }
    }
    let dir = conversation_dir(conversation_id.as_deref()).join("uploads");
    let _ = std::fs::create_dir_all(&dir);

    let mut out = Vec::new();
    for p in paths.iter().take(MAX) {
        let src = PathBuf::from(p);
        if src.is_dir() {
            // 目录:浅层展开其中的文件
            if let Ok(rd) = std::fs::read_dir(&src) {
                for e in rd.flatten() {
                    let ep = e.path();
                    if ep.is_file() && out.len() < MAX {
                        push_attach(&dir, &ep, &mut out);
                    }
                }
            }
            continue;
        }
        if !src.is_file() {
            out.push(AttachedFile {
                name: file_name_of(&src),
                path: String::new(),
                kind: "binary".into(),
                size: 0,
                ok: false,
                error: Some("文件不存在".into()),
            });
            continue;
        }
        push_attach(&dir, &src, &mut out);
    }
    out
}

/// 剪贴板贴图:base64 解码后落到会话 uploads 目录(粘贴截图 → 附件管线)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn chat_attach_image(
    conversation_id: Option<String>,
    name: String,
    data_base64: String,
) -> Result<AttachedFile, String> {
    const MAX_BYTES: usize = 20 * 1024 * 1024;
    if let Some(cid) = conversation_id.as_deref() {
        conv::ensure_conversation_writable(cid)?;
    }
    let bytes = b64_decode(&data_base64).ok_or("图片数据解析失败")?;
    if bytes.is_empty() {
        return Err("空图片".into());
    }
    if bytes.len() > MAX_BYTES {
        return Err("图片超过 20MB 上限".into());
    }
    let dir = conversation_dir(conversation_id.as_deref()).join("uploads");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // 只接受简单文件名,杜绝路径穿越
    let safe: String = name
        .chars()
        .filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    let safe = if safe.trim().is_empty() {
        "pasted.png".to_string()
    } else {
        safe
    };
    let dst = unique_upload_path(&dir, &safe);
    std::fs::write(&dst, &bytes).map_err(|e| e.to_string())?;
    Ok(AttachedFile {
        name: file_name_of(&dst),
        path: dst.to_string_lossy().replace('\\', "/"),
        kind: "image".into(),
        size: bytes.len() as u64,
        ok: true,
        error: None,
    })
}

/// 标准 base64 解码(零新依赖;容忍换行,支持 padding)。
fn b64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits = 0u32;
    for &c in s.as_bytes() {
        if c == b'\n' || c == b'\r' || c == b'=' {
            continue;
        }
        let v = val(c)?;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

fn file_name_of(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| p.to_string_lossy().to_string())
}

fn push_attach(dir: &Path, src: &Path, out: &mut Vec<AttachedFile>) {
    let name = file_name_of(src);
    let size = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
    let dst = unique_upload_path(dir, &name);
    match std::fs::copy(src, &dst) {
        Ok(_) => {
            // PDF / Office 文件: Claude Read 工具读不了二进制, 先提取文本成 .md,
            // 只把 .md 路径传给 Claude (原文件仍留 uploads 目录供用户自行查看)。
            let ext = src
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            let convertible = matches!(
                ext.as_str(),
                "pdf"
                    | "docx"
                    | "doc"
                    | "xlsx"
                    | "xls"
                    | "xlsm"
                    | "xlsb"
                    | "pptx"
                    | "ppt"
                    | "ods"
                    | "odt"
                    | "odp"
            );
            if convertible {
                match convert::convert_to_markdown(src) {
                    Ok(Some(text)) => {
                        let stem = src
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| name.clone());
                        let md_name = format!("{}.extracted.md", stem);
                        let md_dst = unique_upload_path(dir, &md_name);
                        if std::fs::write(&md_dst, text.as_bytes()).is_ok() {
                            out.push(AttachedFile {
                                name: md_name,
                                path: md_dst.to_string_lossy().replace('\\', "/"),
                                kind: "text".into(),
                                size: text.len() as u64,
                                ok: true,
                                error: None,
                            });
                            return;
                        }
                        // write 失败 → 回退到原文件(带错误)
                        out.push(AttachedFile {
                            name,
                            path: String::new(),
                            kind: attach_kind(src).into(),
                            size,
                            ok: false,
                            error: Some("PDF/Office 文本提取成功但写入失败".into()),
                        });
                        return;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        out.push(AttachedFile {
                            name,
                            path: String::new(),
                            kind: attach_kind(src).into(),
                            size,
                            ok: false,
                            error: Some(format!("文本提取失败: {e}")),
                        });
                        return;
                    }
                }
            }
            // 图片 / 纯文本 / 无需转换的二进制 → 原样返回
            out.push(AttachedFile {
                name,
                path: dst.to_string_lossy().replace('\\', "/"),
                kind: attach_kind(src).into(),
                size,
                ok: true,
                error: None,
            });
        }
        Err(e) => out.push(AttachedFile {
            name,
            path: String::new(),
            kind: "binary".into(),
            size,
            ok: false,
            error: Some(e.to_string()),
        }),
    }
}

fn unique_upload_path(dir: &Path, fname: &str) -> PathBuf {
    let first = dir.join(fname);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match fname.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (fname.to_string(), String::new()),
    };
    for n in 2..10_000 {
        let cand = dir.join(format!("{stem} ({n}){ext}"));
        if !cand.exists() {
            return cand;
        }
    }
    first
}

fn attach_kind(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "avif" | "svg" => "image",
        "pdf" => "pdf",
        "docx" | "doc" | "pptx" | "ppt" | "xlsx" | "xls" | "ods" | "odt" | "odp" => "office",
        "txt" | "md" | "markdown" | "csv" | "tsv" | "json" | "yaml" | "yml" | "xml" | "html"
        | "htm" | "log" | "rs" | "js" | "ts" | "py" | "go" | "java" | "c" | "cpp" | "css"
        | "vue" | "sh" | "toml" | "ini" => "text",
        _ => "binary",
    }
}
