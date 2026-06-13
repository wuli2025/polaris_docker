//! 文件中心(File Center)—— 把盘点表里的散乱文件「同类放一起」可视化。
//!
//! 设计承《文件中心-PRD》:
//! - 归类逻辑三层:① 类型+文件夹+时间(零成本兜底)② 语义聚类(复用已存向量,
//!   零新增嵌入调用,本文件主轴)③ 双链关系(kb_graph 已有,前端另接);
//! - 「展示出来好看」:缩略图/首帧/类型图标。缩略图统一以 data URL 返回(三壳同构,
//!   桌面/Docker/Web 都无需 asset 协议或文件服务),磁盘缓存避免重复解码;
//! - 「内容速览」:按需 + 缓存的本地抽取式 gist(零 token,默认不调 LLM)。
//!
//! 铁律(与 fable 其余模块同构):AI 出决策、代码执行;单一事实源 fable.db;
//! 全部命令同步、无 AppHandle 依赖 → 桌面 / Docker / CLI 三壳共用同一份。

use super::{open_db, worker_count};
use serde::Serialize;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

// ───────────────────────── 簇配色(与星河主题同源的高级色) ─────────────────────────

const CLUSTER_PALETTE: &[&str] = &[
    "#5b8cff", "#8b6cff", "#c264d6", "#e0736b", "#e0a24b", "#6fcf97", "#42c8d4", "#5fa8e6",
    "#d4b06a", "#b487e0", "#e08aae", "#7ec8a0", "#9aa0e6", "#d49a6a", "#6cc0c0", "#cf9fd6",
    "#7f9cf5", "#e6a4c4",
];

/// 命名启发式忽略的通用目录段(它们当簇名没区分度)。
const GENERIC_DIRS: &[&str] = &[
    "raw", "wiki", "output", "memory", "src", "docs", "doc", "data", "assets", "public",
    "dist", "build", "tmp", "temp", "files", "file", "新建文件夹", "downloads", "下载",
    "documents", "文档", "desktop", "桌面",
];

// ───────────────────────── 通用小工具 ─────────────────────────

fn data_dir() -> Option<PathBuf> {
    super::db_path().and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

fn thumbs_dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("thumbs"))
}

fn hash_key(parts: &[&str]) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for p in parts {
        p.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

/// 标准 base64 编码(避免引第三方 base64 crate)。
fn b64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

fn human_size(bytes: u64) -> String {
    const U: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

// ───────────────────────── 总览 ─────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KindCount {
    pub kind: String,
    pub count: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterView {
    pub id: i64,
    pub label: String,
    pub color: String,
    pub keywords: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootView {
    pub id: i64,
    pub path: String,
    pub files: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOverview {
    pub roots: Vec<RootView>,
    pub active_root: Option<String>,
    pub total_files: u64,
    pub total_bytes: u64,
    pub by_kind: Vec<KindCount>,
    pub clusters: Vec<ClusterView>,
    pub text_files: u64,
    pub embedded_files: u64,
    pub has_embed_provider: bool,
    pub clustered: bool,
    pub scanning: bool,
    pub indexing: bool,
}

/// 把 root 参数解析成 root_id 过滤子句(None = 全部根)。
fn resolve_root_ids(conn: &rusqlite::Connection, root: &Option<String>) -> Vec<i64> {
    let mut ids = Vec::new();
    let sql = if root.as_ref().map(|r| !r.trim().is_empty()).unwrap_or(false) {
        "SELECT id FROM roots WHERE path=?1"
    } else {
        "SELECT id FROM roots"
    };
    if let Ok(mut stmt) = conn.prepare(sql) {
        let map = |r: &rusqlite::Row| r.get::<_, i64>(0);
        let rows = if sql.contains("?1") {
            stmt.query_map([root.clone().unwrap_or_default()], map)
        } else {
            stmt.query_map([], map)
        };
        if let Ok(rows) = rows {
            for id in rows.flatten() {
                ids.push(id);
            }
        }
    }
    ids
}

/// IN (...) 子句 + 是否有效。空 = 不加过滤(全部)。
fn in_clause(ids: &[i64]) -> String {
    if ids.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        format!(" AND f.root_id IN ({})", list.join(","))
    }
}

pub fn overview(root: Option<String>) -> Result<FileOverview, String> {
    let conn = open_db()?;
    // 根列表(给前端做切换器)
    let mut roots = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT id, path, files FROM roots ORDER BY id")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(RootView {
                    id: r.get(0)?,
                    path: r.get(1)?,
                    files: r.get::<_, i64>(2)? as u64,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows.flatten() {
            roots.push(r);
        }
    }
    let ids = resolve_root_ids(&conn, &root);
    let filter = in_clause(&ids);

    // 类型分布
    let mut by_kind = Vec::new();
    let mut total_files = 0u64;
    let mut total_bytes = 0u64;
    {
        let sql = format!(
            "SELECT f.kind, COUNT(*), COALESCE(SUM(f.size),0) FROM files f WHERE 1=1{filter} GROUP BY f.kind"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(KindCount {
                    kind: r.get(0)?,
                    count: r.get::<_, i64>(1)? as u64,
                    bytes: r.get::<_, i64>(2)? as u64,
                })
            })
            .map_err(|e| e.to_string())?;
        for k in rows.flatten() {
            total_files += k.count;
            total_bytes += k.bytes;
            by_kind.push(k);
        }
    }
    by_kind.sort_by(|a, b| b.count.cmp(&a.count));

    // 簇
    let mut clusters = Vec::new();
    {
        let cfilter = if ids.is_empty() {
            String::new()
        } else {
            let list: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
            format!(" WHERE root_id IN ({})", list.join(","))
        };
        let sql = format!(
            "SELECT id, label, color, keywords, size FROM clusters{cfilter} ORDER BY size DESC"
        );
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(rows) = stmt.query_map([], |r| {
                Ok(ClusterView {
                    id: r.get(0)?,
                    label: r.get(1)?,
                    color: r.get(2)?,
                    keywords: r.get(3)?,
                    size: r.get::<_, i64>(4)? as u64,
                })
            }) {
                for c in rows.flatten() {
                    clusters.push(c);
                }
            }
        }
    }

    let one = |sql: &str| -> u64 {
        conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0) as u64
    };
    let text_files = one(&format!("SELECT COUNT(*) FROM files f WHERE f.kind='text'{filter}"));
    let embedded_files =
        one(&format!("SELECT COUNT(*) FROM files f WHERE f.kind='text' AND f.chunked=1{filter}"));

    Ok(FileOverview {
        active_root: root,
        roots,
        total_files,
        total_bytes,
        by_kind,
        clustered: !clusters.is_empty(),
        clusters,
        text_files,
        embedded_files,
        has_embed_provider: crate::sense::active_provider("embed").is_some(),
        scanning: super::SCANNING.load(Ordering::Relaxed),
        indexing: super::INDEXING.load(Ordering::Relaxed),
    })
}

// ───────────────────────── 网格(分页) ─────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileCard {
    pub id: i64,
    pub path: String,
    pub abspath: String,
    pub name: String,
    pub ext: String,
    pub kind: String,
    pub size: u64,
    pub size_h: String,
    pub mtime: i64,
    pub cluster_id: i64,
    pub thumbable: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileGridPage {
    pub items: Vec<FileCard>,
    pub total: u64,
    pub page: usize,
    pub page_size: usize,
}

#[allow(clippy::too_many_arguments)]
pub fn grid(
    root: Option<String>,
    cluster_id: Option<i64>,
    kind: Option<String>,
    sort: Option<String>,
    query: Option<String>,
    page: usize,
    page_size: usize,
) -> Result<FileGridPage, String> {
    let conn = open_db()?;
    let ids = resolve_root_ids(&conn, &root);
    let mut where_sql = String::from("WHERE 1=1");
    where_sql.push_str(&in_clause(&ids));
    if let Some(cid) = cluster_id {
        if cid > 0 {
            where_sql.push_str(&format!(" AND f.cluster_id={cid}"));
        }
    }
    let kinds: Vec<&str> = match kind.as_deref() {
        Some("media") => vec!["image", "video"],
        Some(k) if !k.is_empty() && k != "all" => vec![k],
        _ => vec![],
    };
    if !kinds.is_empty() {
        let list: Vec<String> = kinds.iter().map(|k| format!("'{k}'")).collect();
        where_sql.push_str(&format!(" AND f.kind IN ({})", list.join(",")));
    }
    // 文件名子串过滤(语义/全文检索走 fable_search,这里只做轻量的名字过滤)
    let q = query.unwrap_or_default();
    let q = q.trim();
    if !q.is_empty() {
        let safe = q.replace('\'', "''").to_lowercase();
        where_sql.push_str(&format!(" AND (LOWER(f.name) LIKE '%{safe}%' OR LOWER(f.relpath) LIKE '%{safe}%')"));
    }
    let order = match sort.as_deref() {
        Some("name") => "f.name ASC",
        Some("size") => "f.size DESC",
        Some("kind") => "f.kind ASC, f.name ASC",
        _ => "f.mtime DESC",
    };

    let total: u64 = conn
        .query_row(&format!("SELECT COUNT(*) FROM files f {where_sql}"), [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap_or(0) as u64;

    let page_size = page_size.clamp(12, 400);
    let offset = page.saturating_mul(page_size);
    let sql = format!(
        "SELECT f.id, r.path, f.relpath, f.name, f.ext, f.kind, f.size, f.mtime, f.cluster_id
         FROM files f JOIN roots r ON r.id=f.root_id {where_sql}
         ORDER BY {order} LIMIT {page_size} OFFSET {offset}"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            let root: String = r.get(1)?;
            let rel: String = r.get(2)?;
            let abspath = Path::new(&root).join(&rel).to_string_lossy().into_owned();
            let kind: String = r.get(5)?;
            let size = r.get::<_, i64>(6)? as u64;
            let thumbable = kind == "image" || kind == "video";
            Ok(FileCard {
                id: r.get(0)?,
                path: rel,
                abspath,
                name: r.get(3)?,
                ext: r.get(4)?,
                kind,
                size,
                size_h: human_size(size),
                mtime: r.get(7)?,
                cluster_id: r.get(8)?,
                thumbable,
            })
        })
        .map_err(|e| e.to_string())?;
    let items: Vec<FileCard> = rows.flatten().collect();
    Ok(FileGridPage { items, total, page, page_size })
}

// ───────────────────────── 缩略图 / 首帧 ─────────────────────────

const IMG_DECODE: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "bmp"];
const VIDEO_EXTS: &[&str] =
    &["mp4", "mkv", "mov", "avi", "wmv", "flv", "webm", "m4v", "mpg", "mpeg"];

fn ext_of(path: &str) -> String {
    Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn jpeg_data_url(rgb: &image::DynamicImage) -> Result<String, String> {
    let mut buf = Vec::new();
    image::DynamicImage::ImageRgb8(rgb.to_rgb8())
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Jpeg)
        .map_err(|e| format!("缩略图编码失败: {e}"))?;
    Ok(format!("data:image/jpeg;base64,{}", b64(&buf)))
}

/// 生成(或读缓存)缩略图,统一返回 data URL;无法出图返回 None(前端落类型图标)。
pub fn thumb(abspath: String, max: u32) -> Result<Option<String>, String> {
    let p = Path::new(&abspath);
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
    let key = hash_key(&[&abspath, &mtime.to_string(), &meta.len().to_string(), &max.to_string()]);
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
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Jpeg)
            .is_ok()
        {
            let _ = std::fs::write(c, &buf);
            return Ok(Some(format!("data:image/jpeg;base64,{}", b64(&buf))));
        }
    }
    jpeg_data_url(&img).map(Some)
}

/// 视频首帧:best-effort 调系统/渲染版 ffmpeg 抽第 1 秒一帧 → image 解码缩放。缺 ffmpeg 返回 None。
fn video_frame(p: &Path, max: u32) -> Option<image::DynamicImage> {
    let tmp = std::env::temp_dir().join(format!("polaris-vf-{}.jpg", hash_key(&[&p.to_string_lossy()])));
    let status = std::process::Command::new("ffmpeg")
        .args([
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
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let img = image::open(&tmp).ok().map(|i| i.thumbnail(max, max));
    let _ = std::fs::remove_file(&tmp);
    img
}

// ───────────────────────── 内容速览(抽取式,零 token) ─────────────────────────

const TEXT_GIST_EXTS: &[&str] = &[
    "md", "txt", "rs", "py", "js", "ts", "tsx", "jsx", "mjs", "json", "yaml", "yml", "toml",
    "html", "htm", "css", "csv", "tsv", "log", "xml", "vue", "go", "java", "c", "cpp", "rb",
    "php", "srt", "vtt", "tex", "rst", "org", "sql", "sh", "ps1",
];
const DOC_GIST_EXTS: &[&str] = &["pdf", "docx", "doc", "pptx", "ppt", "xlsx", "xls", "epub"];

/// 从纯文本里抽一句话速览:标题(# 或 frontmatter title)+ 首个有意义段落。
fn extract_gist(text: &str) -> String {
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
    let p = Path::new(&abspath);
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
    if let Ok(cached) =
        conn.query_row("SELECT text FROM gists WHERE key=?1", [&key], |r| r.get::<_, String>(0))
    {
        if !cached.is_empty() {
            return Ok(cached);
        }
    }

    let ext = ext_of(&abspath);
    let result = if TEXT_GIST_EXTS.contains(&ext.as_str()) {
        let bytes = std::fs::read(p).map_err(|e| e.to_string())?;
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

fn kind_label(ext: &str) -> &'static str {
    match super::inventory::classify(ext) {
        "text" => "文本",
        "doc" => "文档",
        "image" => "图片",
        "audio" => "音频",
        "video" => "视频",
        "archive" => "压缩包",
        _ => "未知",
    }
}

// ───────────────────────── 语义聚类(复用已存向量) ─────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterBuildSummary {
    pub clusters: usize,
    pub files: usize,
    pub seconds: f64,
    pub note: String,
}

struct FileVec {
    file_id: i64,
    root_id: i64,
    relpath: String,
    name: String,
    vec: Vec<f32>,
}

fn normalize(v: &mut [f32]) {
    let n = (v.iter().map(|x| x * x).sum::<f32>()).sqrt();
    if n > 1e-6 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// 词法归类的哈希特征维度(稀疏 token → 固定维,余弦可比)。
const LEX_DIM: usize = 128;

fn hash_token(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// 文件的词法特征向量:文件夹段(权重 1.5)+ 文件名分词(1.0)+ 扩展名(0.8)哈希进 LEX_DIM 维。
/// 共享同一目录/相近命名/同类型的文件在余弦下自然靠拢。
fn lexical_vec(relpath: &str, name: &str, ext: &str) -> Vec<f32> {
    let mut v = vec![0f32; LEX_DIM];
    let segs: Vec<&str> = relpath.split('/').collect();
    for seg in segs.iter().take(segs.len().saturating_sub(1)) {
        let low = seg.trim().to_lowercase();
        if low.is_empty() || GENERIC_DIRS.contains(&low.as_str()) {
            continue;
        }
        v[(hash_token(&low) % LEX_DIM as u64) as usize] += 1.5;
    }
    for tok in tokenize(name) {
        v[(hash_token(&tok) % LEX_DIM as u64) as usize] += 1.0;
    }
    if !ext.is_empty() {
        v[(hash_token(&ext.to_lowercase()) % LEX_DIM as u64) as usize] += 0.8;
    }
    v
}

/// 词法兜底:加载范围内全部文件(上限 6000,mtime 倒序)→ 归一化词法向量。
fn load_lexical_files(conn: &rusqlite::Connection, filter: &str) -> Result<Vec<FileVec>, String> {
    let sql = format!(
        "SELECT f.id, f.root_id, f.relpath, f.name, f.ext FROM files f
         WHERE 1=1{filter} ORDER BY f.mtime DESC LIMIT 6000"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (id, root_id, relpath, name, ext) in rows.flatten() {
        let mut v = lexical_vec(&relpath, &name, &ext);
        normalize(&mut v);
        out.push(FileVec { file_id: id, root_id, relpath, name, vec: v });
    }
    Ok(out)
}

/// 单根/全库重建语义聚类:每文件 = 其 chunk 向量均值池化 → 球面 k-means(余弦) →
/// 写回 files.cluster_id + clusters 表。纯数学,不调嵌入 API。
/// 无嵌入向量时自动退化为词法归类(见 [`load_lexical_files`]),保证永远可用。
pub fn cluster_build(root: Option<String>) -> Result<ClusterBuildSummary, String> {
    let started = std::time::Instant::now();
    let conn = open_db()?;
    let ids = resolve_root_ids(&conn, &root);
    let filter = if ids.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        format!(" AND f.root_id IN ({})", list.join(","))
    };

    // 均值池化:流式累加每个文件的 chunk 向量
    let mut acc: HashMap<i64, (Vec<f32>, u32, i64, String, String)> = HashMap::new();
    {
        let sql = format!(
            "SELECT c.file_id, c.vec, f.root_id, f.relpath, f.name
             FROM chunks c JOIN files f ON f.id=c.file_id
             WHERE f.kind='text'{filter}"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let file_id: i64 = row.get(0).map_err(|e| e.to_string())?;
            let blob: Vec<u8> = row.get(1).map_err(|e| e.to_string())?;
            let v = super::index::blob_to_vec(&blob);
            let root_id: i64 = row.get(2).map_err(|e| e.to_string())?;
            let relpath: String = row.get(3).map_err(|e| e.to_string())?;
            let name: String = row.get(4).map_err(|e| e.to_string())?;
            let entry = acc
                .entry(file_id)
                .or_insert_with(|| (vec![0.0; v.len()], 0, root_id, relpath, name));
            if entry.0.len() == v.len() {
                for (a, b) in entry.0.iter_mut().zip(v.iter()) {
                    *a += b;
                }
                entry.1 += 1;
            }
        }
    }

    let mut files: Vec<FileVec> = acc
        .into_iter()
        .filter(|(_, (_, n, ..))| *n > 0)
        .map(|(file_id, (mut v, n, root_id, relpath, name))| {
            for x in v.iter_mut() {
                *x /= n as f32;
            }
            normalize(&mut v);
            FileVec { file_id, root_id, relpath, name, vec: v }
        })
        .collect();

    let mut mode = "semantic";
    if files.len() < 2 {
        // 没有(足够的)嵌入向量 → 退化为「结构/词法」归类:对全部文件用
        // 文件夹 + 文件名分词 + 扩展名的哈希特征向量,跑同一套球面 k-means。
        // 无需任何 key、离线即可用 —— 保证「智能归类」永远点得动、永远能把相似文件放一起;
        // 配了硅基 key 并建好向量索引后,本函数自动走上面的语义路(更准)。
        mode = "lexical";
        files = load_lexical_files(&conn, &filter)?;
        if files.len() < 2 {
            return Ok(ClusterBuildSummary {
                clusters: 0,
                files: files.len(),
                seconds: started.elapsed().as_secs_f64(),
                note: "可归类的文件不足(<2),先点「盘点」扫描磁盘文件再归类".into(),
            });
        }
    }
    // 稳定顺序(file_id 升序),让确定性初始化可复现
    files.sort_by_key(|f| f.file_id);

    let n = files.len();
    let dim = files[0].vec.len();
    let k = ((n as f64).sqrt().round() as usize).clamp(3, 18).min(n);

    // 确定性初始化:farthest-first traversal(余弦),避免依赖随机数
    let mut centroids: Vec<Vec<f32>> = Vec::with_capacity(k);
    centroids.push(files[0].vec.clone());
    while centroids.len() < k {
        let mut best_i = 0usize;
        let mut best_d = f32::MIN;
        for (i, f) in files.iter().enumerate() {
            // 到已选中心的最大相似度 → 取「最不相似」的当新中心
            let max_sim = centroids.iter().map(|c| dot(c, &f.vec)).fold(f32::MIN, f32::max);
            let d = -max_sim;
            if d > best_d {
                best_d = d;
                best_i = i;
            }
        }
        centroids.push(files[best_i].vec.clone());
    }

    // Lloyd 迭代(球面)
    let mut assign = vec![0usize; n];
    for _ in 0..16 {
        let mut changed = false;
        for (i, f) in files.iter().enumerate() {
            let mut best = 0usize;
            let mut best_sim = f32::MIN;
            for (ci, c) in centroids.iter().enumerate() {
                let s = dot(c, &f.vec);
                if s > best_sim {
                    best_sim = s;
                    best = ci;
                }
            }
            if assign[i] != best {
                assign[i] = best;
                changed = true;
            }
        }
        // 重算中心
        let mut sums = vec![vec![0.0f32; dim]; k];
        let mut counts = vec![0u32; k];
        for (i, f) in files.iter().enumerate() {
            let c = assign[i];
            for (a, b) in sums[c].iter_mut().zip(f.vec.iter()) {
                *a += b;
            }
            counts[c] += 1;
        }
        for ci in 0..k {
            if counts[ci] > 0 {
                for x in sums[ci].iter_mut() {
                    *x /= counts[ci] as f32;
                }
                normalize(&mut sums[ci]);
                centroids[ci] = std::mem::take(&mut sums[ci]);
            }
        }
        if !changed {
            break;
        }
    }

    // 按簇成员命名 + 着色,并把空簇剔除、重编号
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); k];
    for (i, &c) in assign.iter().enumerate() {
        members[c].push(i);
    }

    // 清旧簇(对涉及的根)
    if ids.is_empty() {
        conn.execute("DELETE FROM clusters", []).ok();
        conn.execute("UPDATE files SET cluster_id=0", []).ok();
    } else {
        let list: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        let inlist = list.join(",");
        conn.execute(&format!("DELETE FROM clusters WHERE root_id IN ({inlist})"), []).ok();
        conn.execute(&format!("UPDATE files SET cluster_id=0 WHERE root_id IN ({inlist})"), []).ok();
    }

    let built_at = chrono::Local::now().timestamp_millis();
    let mut new_clusters = 0usize;
    conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
    for mem in members.iter() {
        if mem.is_empty() {
            continue;
        }
        let (label, keywords) = name_cluster(&files, mem);
        // 簇的代表根 = 成员里出现最多的 root_id
        let mut root_freq: HashMap<i64, usize> = HashMap::new();
        for &i in mem {
            *root_freq.entry(files[i].root_id).or_insert(0) += 1;
        }
        let root_id = root_freq.into_iter().max_by_key(|(_, c)| *c).map(|(r, _)| r).unwrap_or(0);
        let color = CLUSTER_PALETTE[new_clusters % CLUSTER_PALETTE.len()];
        conn.execute(
            "INSERT INTO clusters(root_id,label,color,keywords,size,built_at) VALUES(?1,?2,?3,?4,?5,?6)",
            rusqlite::params![root_id, label, color, keywords, mem.len() as i64, built_at],
        )
        .map_err(|e| e.to_string())?;
        let cluster_id = conn.last_insert_rowid();
        {
            let mut stmt = conn
                .prepare_cached("UPDATE files SET cluster_id=?1 WHERE id=?2")
                .map_err(|e| e.to_string())?;
            for &i in mem {
                stmt.execute(rusqlite::params![cluster_id, files[i].file_id])
                    .map_err(|e| e.to_string())?;
            }
        }
        new_clusters += 1;
    }
    conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;

    Ok(ClusterBuildSummary {
        clusters: new_clusters,
        files: n,
        seconds: started.elapsed().as_secs_f64(),
        note: if mode == "semantic" {
            format!("已按语义把 {n} 个已嵌入文本归成 {new_clusters} 簇")
        } else {
            format!(
                "已按文件夹/名称把 {n} 个文件归成 {new_clusters} 簇 · 配硅基 key 并建向量索引后可升级为语义归类"
            )
        },
    })
}

/// 簇命名:优先成员里出现最多的「非通用」目录段;退化用文件名高频词。
fn name_cluster(files: &[FileVec], members: &[usize]) -> (String, String) {
    let mut dir_freq: HashMap<String, usize> = HashMap::new();
    let mut tok_freq: HashMap<String, usize> = HashMap::new();
    for &i in members {
        let rel = &files[i].relpath;
        let segs: Vec<&str> = rel.split('/').collect();
        // 目录段(去掉文件名)
        for seg in segs.iter().take(segs.len().saturating_sub(1)) {
            let s = seg.trim();
            if s.is_empty() {
                continue;
            }
            let low = s.to_lowercase();
            if GENERIC_DIRS.contains(&low.as_str()) {
                continue;
            }
            *dir_freq.entry(s.to_string()).or_insert(0) += 1;
        }
        // 文件名分词
        for tok in tokenize(&files[i].name) {
            *tok_freq.entry(tok).or_insert(0) += 1;
        }
    }
    let threshold = (members.len() as f64 * 0.34).ceil() as usize;
    let top_dir = dir_freq
        .iter()
        .filter(|(_, c)| **c >= threshold.max(2))
        .max_by_key(|(_, c)| **c)
        .map(|(d, _)| d.clone());

    let mut toks: Vec<(String, usize)> = tok_freq.into_iter().collect();
    toks.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let keywords: Vec<String> = toks.iter().take(4).map(|(t, _)| t.clone()).collect();

    let label = match top_dir {
        Some(d) => d,
        None => {
            if keywords.is_empty() {
                "未命名".to_string()
            } else {
                keywords.iter().take(2).cloned().collect::<Vec<_>>().join(" · ")
            }
        }
    };
    (label, keywords.join(" "))
}

/// 文件名 → 词:按非字母数字切英文 token(≥2),CJK 连续段整体当一个词。
fn tokenize(name: &str) -> Vec<String> {
    let stem: &str = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_cjk = String::new();
    let flush_ascii = |cur: &mut String, out: &mut Vec<String>| {
        if cur.chars().count() >= 2 {
            out.push(cur.to_lowercase());
        }
        cur.clear();
    };
    for ch in stem.chars() {
        if ch.is_ascii_alphanumeric() {
            cur.push(ch);
            if !cur_cjk.is_empty() {
                out.push(std::mem::take(&mut cur_cjk));
            }
        } else if ('\u{4e00}'..='\u{9fff}').contains(&ch) {
            cur_cjk.push(ch);
            flush_ascii(&mut cur, &mut out);
        } else {
            flush_ascii(&mut cur, &mut out);
            if cur_cjk.chars().count() >= 2 {
                out.push(std::mem::take(&mut cur_cjk));
            } else {
                cur_cjk.clear();
            }
        }
    }
    flush_ascii(&mut cur, &mut out);
    if cur_cjk.chars().count() >= 2 {
        out.push(cur_cjk);
    }
    // 过滤纯数字/常见噪声词
    out.retain(|t| !t.chars().all(|c| c.is_ascii_digit()) && t != "copy" && t != "final");
    out
}

// ───────────────────────── 缩略图批量预热(可选,后台友好) ─────────────────────────

/// 给一批绝对路径预生成缩略图缓存(前端进入网格时可后台调,加速滚动)。
/// 返回成功生成/命中缓存的数量。多核并行。
pub fn warm_thumbs(paths: Vec<String>, max: u32) -> usize {
    let done = AtomicUsize::new(0);
    let stack = Mutex::new(paths);
    std::thread::scope(|s| {
        for _ in 0..worker_count() {
            let (stack, done) = (&stack, &done);
            s.spawn(move || loop {
                let item = { stack.lock().unwrap().pop() };
                let Some(p) = item else { break };
                if let Ok(Some(_)) = thumb(p, max) {
                    done.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });
    done.load(Ordering::Relaxed)
}

// ───────────────────────── 命令(薄包装;三壳共用) ─────────────────────────

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_overview(root: Option<String>) -> Result<FileOverview, String> {
    overview(root)
}

#[cfg_attr(feature = "desktop", tauri::command)]
#[allow(clippy::too_many_arguments)]
pub fn file_grid(
    root: Option<String>,
    cluster_id: Option<i64>,
    kind: Option<String>,
    sort: Option<String>,
    query: Option<String>,
    page: Option<usize>,
    page_size: Option<usize>,
) -> Result<FileGridPage, String> {
    grid(
        root,
        cluster_id,
        kind,
        sort,
        query,
        page.unwrap_or(0),
        page_size.unwrap_or(60),
    )
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_thumb(abspath: String, max: Option<u32>) -> Result<Option<String>, String> {
    thumb(abspath, max.unwrap_or(360))
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_gist(abspath: String) -> Result<String, String> {
    gist(abspath)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_cluster_build(root: Option<String>) -> Result<ClusterBuildSummary, String> {
    cluster_build(root)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_warm_thumbs(paths: Vec<String>, max: Option<u32>) -> Result<usize, String> {
    Ok(warm_thumbs(paths, max.unwrap_or(360)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64_roundtrip_basic() {
        assert_eq!(b64(b"Man"), "TWFu");
        assert_eq!(b64(b"Ma"), "TWE=");
        assert_eq!(b64(b"M"), "TQ==");
    }

    #[test]
    fn tokenize_splits_cjk_and_ascii() {
        let t = tokenize("全澳房产_dataset_v2.csv");
        assert!(t.iter().any(|x| x == "全澳房产"));
        assert!(t.iter().any(|x| x == "dataset"));
        // 纯数字 / 版本号被过滤
        assert!(!t.iter().any(|x| x == "2"));
    }

    #[test]
    fn extract_gist_prefers_title_and_first_para() {
        let g = extract_gist("# 标题行\n\n这是第一段正文内容。");
        assert!(g.contains("标题行"));
        assert!(g.contains("第一段"));
    }

    #[test]
    fn human_size_scales() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
    }
}
