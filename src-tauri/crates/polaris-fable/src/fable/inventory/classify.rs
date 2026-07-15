// ───────────────────────── 文件分类 ─────────────────────────

const TEXT_EXTS: &[&str] = &[
    "md", "txt", "rs", "py", "js", "ts", "tsx", "jsx", "mjs", "json", "jsonl", "yaml", "yml",
    "toml", "html", "htm", "css", "csv", "tsv", "log", "xml", "ini", "cfg", "conf", "sh", "ps1",
    "bat", "cmd", "sql", "vue", "go", "java", "c", "cpp", "h", "hpp", "rb", "php", "srt", "vtt",
    "tex", "rst", "org",
];
const DOC_EXTS: &[&str] = &[
    "pdf", "docx", "doc", "pptx", "ppt", "xlsx", "xls", "epub", "mobi",
];
const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "heic", "svg", "tif", "tiff", "raw", "cr2", "nef",
];
const AUDIO_EXTS: &[&str] = &[
    "mp3", "wav", "flac", "m4a", "aac", "ogg", "wma", "opus", "amr",
];
const VIDEO_EXTS: &[&str] = &[
    "mp4", "mkv", "mov", "avi", "wmv", "flv", "webm", "m4v", "mpg", "mpeg",
];
const ARCHIVE_EXTS: &[&str] = &["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "iso", "dmg"];

pub(crate) fn classify(ext: &str) -> &'static str {
    let e = ext.to_ascii_lowercase();
    let e = e.as_str();
    if TEXT_EXTS.contains(&e) {
        "text"
    } else if DOC_EXTS.contains(&e) {
        "doc"
    } else if IMAGE_EXTS.contains(&e) {
        "image"
    } else if AUDIO_EXTS.contains(&e) {
        "audio"
    } else if VIDEO_EXTS.contains(&e) {
        "video"
    } else if ARCHIVE_EXTS.contains(&e) {
        "archive"
    } else {
        "other"
    }
}

// ───────────────────────── 按「语言」归类 ─────────────────────────
//
// 用户诉求:文件归类要「按语言」(编程语言 / 自然语言),不要按应用名或粗粒度类型。
// 三层判定:① 代码/标记类 → 编程语言(扩展名精确判定,零 IO);② 媒体/压缩 → 大类;
// ③ 文稿(md/txt/doc…)→ 自然语言(读文件头按 CJK 占比嗅探,放在回填里做,避免拖慢盘点)。

/// 扩展名 → 编程语言/标记语言(「按语言归类」的精确信号,零 IO)。None = 非代码类。
pub(crate) fn prog_lang(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "py" | "pyw" | "pyi" | "ipynb" => "Python",
        "rs" => "Rust",
        "js" | "mjs" | "cjs" => "JavaScript",
        "ts" => "TypeScript",
        "tsx" | "jsx" => "React/JSX",
        "vue" => "Vue",
        "go" => "Go",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "c" | "h" => "C",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "C++",
        "cs" => "C#",
        "rb" => "Ruby",
        "php" => "PHP",
        "swift" => "Swift",
        "scala" => "Scala",
        "sh" | "bash" | "zsh" => "Shell",
        "ps1" | "psm1" | "psd1" => "PowerShell",
        "bat" | "cmd" => "Batch",
        "sql" => "SQL",
        "r" => "R",
        "lua" => "Lua",
        "dart" => "Dart",
        "pl" | "pm" => "Perl",
        "html" | "htm" => "HTML",
        "css" | "scss" | "sass" | "less" => "CSS/样式",
        "json" | "jsonl" | "ndjson" => "JSON",
        "yaml" | "yml" => "YAML",
        "toml" | "ini" | "cfg" | "conf" => "配置",
        "xml" => "XML",
        _ => return None,
    })
}

/// kind → 媒体/压缩大类(非代码、非文稿的语言归类兜底)。None = 文稿类(交自然语言嗅探)。
fn media_lang(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "image" => "图片",
        "video" => "视频",
        "audio" => "音频",
        "archive" => "压缩包",
        "other" => "其他文件",
        _ => return None, // text / doc → 文稿,按自然语言归
    })
}

/// 全部受支持的代码/标记扩展名(grid 按语言反查用)。与 [`prog_lang`] 的 match 同源。
pub(crate) const CODE_EXTS: &[&str] = &[
    "py", "pyw", "pyi", "ipynb", "rs", "js", "mjs", "cjs", "ts", "tsx", "jsx", "vue", "go", "java",
    "kt", "kts", "c", "h", "cpp", "cc", "cxx", "hpp", "hh", "hxx", "cs", "rb", "php", "swift",
    "scala", "sh", "bash", "zsh", "ps1", "psm1", "psd1", "bat", "cmd", "sql", "r", "lua", "dart",
    "pl", "pm", "html", "htm", "css", "scss", "sass", "less", "json", "jsonl", "ndjson", "yaml",
    "yml", "toml", "ini", "cfg", "conf", "xml",
];

/// 某编程/标记语言 → 对应扩展名集合(grid 按语言过滤;代码语言由扩展名确定,不依赖回填)。
/// 空 = 该标签不是代码语言(改按 lang 列 / kind 过滤)。
pub(crate) fn exts_for_lang(label: &str) -> Vec<&'static str> {
    CODE_EXTS
        .iter()
        .copied()
        .filter(|e| prog_lang(e) == Some(label))
        .collect()
}

/// 媒体/压缩语言标签 → 对应 kind(grid 过滤用)。None = 非媒体标签。
pub(crate) fn kind_for_media_lang(label: &str) -> Option<&'static str> {
    Some(match label {
        "图片" => "image",
        "视频" => "video",
        "音频" => "audio",
        "压缩包" => "archive",
        "其他文件" => "other",
        _ => return None,
    })
}

/// 盘点时即可定的语言(零 IO):代码看扩展名、媒体看 kind;文稿返回 ""(留待回填读头嗅探)。
pub(crate) fn quick_lang(ext: &str, kind: &str) -> String {
    if let Some(l) = prog_lang(ext) {
        return l.to_string();
    }
    media_lang(kind).unwrap_or("").to_string()
}

/// 读文件头嗅探自然语言:CJK 占比 ≥10% → 中文;拉丁字母为主 → 英文;否则其他语言。
pub(crate) fn natural_lang(sample: &str) -> &'static str {
    let (mut cjk, mut latin, mut letters) = (0usize, 0usize, 0usize);
    for c in sample.chars().take(8000) {
        if ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{3400}'..='\u{4dbf}').contains(&c) {
            cjk += 1;
            letters += 1;
        } else if c.is_ascii_alphabetic() {
            latin += 1;
            letters += 1;
        } else if c.is_alphabetic() {
            letters += 1;
        }
    }
    if letters < 8 {
        return "其他语种";
    }
    if cjk as f32 / letters as f32 >= 0.10 {
        "中文"
    } else if latin as f32 / letters as f32 >= 0.6 {
        "英文"
    } else {
        "其他语种"
    }
}

/// 读文件头(≤16KB)做文本采样;二进制(含 NUL)或不可读返回 None。
pub(crate) fn read_head_sample(abs: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(abs).ok()?;
    let mut buf = vec![0u8; 16 * 1024];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    if buf.iter().take(1024).any(|&b| b == 0) {
        return None; // 二进制(含改名的伪文本 / docx/pdf 等容器)
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}
