use super::*;

// ───────────────────────── 内容速览(抽取式,零 token) ─────────────────────────

pub(crate) const TEXT_GIST_EXTS: &[&str] = &[
    "md", "txt", "rs", "py", "js", "ts", "tsx", "jsx", "mjs", "json", "yaml", "yml", "toml",
    "html", "htm", "css", "csv", "tsv", "log", "xml", "vue", "go", "java", "c", "cpp", "rb", "php",
    "srt", "vtt", "tex", "rst", "org", "sql", "sh", "ps1",
];
pub(crate) const DOC_GIST_EXTS: &[&str] =
    &["pdf", "docx", "doc", "pptx", "ppt", "xlsx", "xls", "epub"];

/// 从纯文本里抽一句话速览:标题(# 或 frontmatter title)+ 首个有意义段落。
pub(crate) fn extract_gist(text: &str) -> String {
    let mut title = String::new();
    let mut body = String::new();
    let mut in_fm = false;
    let mut lines = text.lines().peekable();
    // frontmatter title
    if lines.peek().map(|l| l.trim() == "---").unwrap_or(false) {
        in_fm = true;
        lines.next();
    }
    let mut rest: Vec<&str> = Vec::new();
    for l in lines {
        if in_fm {
            if l.trim() == "---" {
                in_fm = false;
                continue;
            }
            if let Some(t) = l.strip_prefix("title:") {
                title = t.trim().trim_matches('"').to_string();
            }
            continue;
        }
        rest.push(l);
    }
    for l in &rest {
        let t = l.trim();
        if t.is_empty() {
            continue;
        }
        if title.is_empty() {
            if let Some(h) = t.strip_prefix("# ") {
                title = h.trim().to_string();
                continue;
            }
        }
        // 跳过 markdown 标记/代码栅栏行,找首个实义句
        if t.starts_with("```") || t.starts_with('|') || t.starts_with("---") {
            continue;
        }
        let clean: String = t
            .trim_start_matches(|c: char| c == '#' || c == '>' || c == '-' || c == '*' || c == ' ')
            .chars()
            .take(120)
            .collect();
        if clean.chars().count() >= 4 {
            body = clean;
            break;
        }
    }
    match (title.is_empty(), body.is_empty()) {
        (false, false) => format!("{title} — {body}"),
        (false, true) => title,
        (true, false) => body,
        (true, true) => String::new(),
    }
}

/// 按需速览:文本/文档抽取式总结(缓存);其余类型给「类型 · 大小」简述。
pub fn gist(abspath: String) -> Result<String, String> {
    let real = crate::fable::reencode_fs_path(&abspath);
    let p = real.as_path();
    if !p.is_file() {
        return Err("文件不存在".into());
    }
    let meta = std::fs::metadata(p).map_err(|e| e.to_string())?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let key = hash_key(&[&abspath, &mtime.to_string(), &meta.len().to_string()]);

    let conn = open_db()?;
    if let Ok(cached) = conn.query_row("SELECT text FROM gists WHERE key=?1", [&key], |r| {
        r.get::<_, String>(0)
    }) {
        if !cached.is_empty() {
            return Ok(cached);
        }
    }

    let ext = ext_of(&abspath);
    let result = if TEXT_GIST_EXTS.contains(&ext.as_str()) {
        // 速览只取前 8000 字符,故只读文件头若干字节即可 —— 原 `fs::read` 会把 GB 级
        // 大文本(.txt/.json/.log)整个读进内存做一个小预览,弱 NAS 上直接 OOM。256KB
        // 远超 8000 字符所需(即便全 4 字节 UTF-8 也才 32KB),既防爆内存又不影响预览质量。
        const GIST_HEAD_BYTES: usize = 256 * 1024;
        use std::io::Read;
        let mut bytes = vec![0u8; GIST_HEAD_BYTES];
        let n = std::fs::File::open(p)
            .and_then(|mut f| f.read(&mut bytes))
            .map_err(|e| e.to_string())?;
        bytes.truncate(n);
        if bytes.iter().take(4096).any(|&b| b == 0) {
            String::new()
        } else {
            let head: String = String::from_utf8_lossy(&bytes).chars().take(8000).collect();
            extract_gist(&head)
        }
    } else if DOC_GIST_EXTS.contains(&ext.as_str()) {
        match crate::convert::convert_to_markdown(p) {
            Ok(Some(md)) => {
                let head: String = md.chars().take(8000).collect();
                extract_gist(&head)
            }
            _ => String::new(),
        }
    } else {
        String::new()
    };
    let result = if result.is_empty() {
        format!("{} 文件 · {}", kind_label(&ext), human_size(meta.len()))
    } else {
        result
    };

    let _ = conn.execute(
        "INSERT OR REPLACE INTO gists(key, text, made_at) VALUES(?1,?2,?3)",
        rusqlite::params![key, result, mtime as i64],
    );
    Ok(result)
}

pub(crate) fn kind_label(ext: &str) -> &'static str {
    match crate::fable::inventory::classify(ext) {
        "text" => "文本",
        "doc" => "文档",
        "image" => "图片",
        "audio" => "音频",
        "video" => "视频",
        "archive" => "压缩包",
        _ => "未知",
    }
}
