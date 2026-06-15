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
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

#[cfg(feature = "desktop")]
use tauri::{AppHandle, Emitter};
#[cfg(not(feature = "desktop"))]
use crate::host::AppHandle;

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
    /// 0 = 顶层主题文件夹;否则为所属父主题的簇 id(语义两级归类)。
    pub parent: i64,
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
            "SELECT id, label, color, keywords, size, parent FROM clusters{cfilter} ORDER BY size DESC"
        );
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(rows) = stmt.query_map([], |r| {
                Ok(ClusterView {
                    id: r.get(0)?,
                    label: r.get(1)?,
                    color: r.get(2)?,
                    keywords: r.get(3)?,
                    size: r.get::<_, i64>(4)? as u64,
                    parent: r.get::<_, i64>(5)?,
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
    /// 智能显示标题:AI 起的名(若有)否则本地清洗文件名;前端用它当卡片主标题,原名做副标题/悬停。
    pub title: String,
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
            // 选中的可能是顶层主题(父簇,自身不直接挂文件)或叶簇 —— 统一展开:
            // 命中该簇本身或其任意子簇下的文件。
            where_sql.push_str(&format!(
                " AND f.cluster_id IN (SELECT id FROM clusters WHERE id={cid} OR parent={cid})"
            ));
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
        "SELECT f.id, r.path, f.relpath, f.name, f.ext, f.kind, f.size, f.mtime, f.cluster_id, t.title
         FROM files f JOIN roots r ON r.id=f.root_id
         LEFT JOIN titles t ON t.file_id=f.id {where_sql}
         ORDER BY {order} LIMIT {page_size} OFFSET {offset}"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            let root: String = r.get(1)?;
            let rel: String = r.get(2)?;
            let abspath = Path::new(&root).join(&rel).to_string_lossy().into_owned();
            let name: String = r.get(3)?;
            let kind: String = r.get(5)?;
            let size = r.get::<_, i64>(6)? as u64;
            let thumbable = kind == "image" || kind == "video";
            // AI 起的名优先;否则本地清洗原始文件名(去时间戳/哈希/计数器/分隔符)。
            let stored: Option<String> = r.get::<_, Option<String>>(9).ok().flatten();
            let title = stored
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| clean_title(&name));
            Ok(FileCard {
                id: r.get(0)?,
                path: rel,
                abspath,
                name,
                title,
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
    // 显示路径可能是 GBK 名解码出的 UTF-8;还原成磁盘真实路径再读(否则 GBK 图片出不了图)。
    let real = super::reencode_fs_path(&abspath);
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
    let real = super::reencode_fs_path(&abspath);
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
    let file_vecs: Vec<Vec<f32>> = files.iter().map(|f| f.vec.clone()).collect();
    // 一级(叶):细粒度语义簇
    let k = ((n as f64).sqrt().round() as usize).clamp(3, 18).min(n);
    let (assign, leaf_centroids) = spherical_kmeans(&file_vecs, k);

    // 叶簇成员(剔空簇)
    let mut members_all: Vec<Vec<usize>> = vec![Vec::new(); leaf_centroids.len()];
    for (i, &c) in assign.iter().enumerate() {
        members_all[c].push(i);
    }
    let leaf_idx: Vec<usize> = (0..members_all.len()).filter(|&c| !members_all[c].is_empty()).collect();
    let members: Vec<Vec<usize>> = leaf_idx.iter().map(|&c| members_all[c].clone()).collect();
    let n_leaf = members.len();

    // 二级(父):叶簇质心再聚合成「顶层主题」。叶簇 ≥4 才分两级,否则全部顶层。
    let two_level = n_leaf >= 4;
    let parent_of_leaf: Vec<usize> = if two_level {
        let k_parent = ((n_leaf as f64).sqrt().ceil() as usize).clamp(2, 6).min(n_leaf);
        let cvecs: Vec<Vec<f32>> = leaf_idx.iter().map(|&c| leaf_centroids[c].clone()).collect();
        spherical_kmeans(&cvecs, k_parent).0
    } else {
        (0..n_leaf).collect()
    };
    let n_parents = parent_of_leaf.iter().copied().max().map(|m| m + 1).unwrap_or(0);

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

    // 先写顶层主题(父簇:自身不直接挂文件,颜色按主题分配)。
    let mut parent_ids: Vec<i64> = vec![0; n_parents];
    if two_level {
        for p in 0..n_parents {
            // 该主题下所有文件 = 旗下叶簇成员之并
            let mut union: Vec<usize> = Vec::new();
            for (li, &leaf_p) in parent_of_leaf.iter().enumerate() {
                if leaf_p == p {
                    union.extend_from_slice(&members[li]);
                }
            }
            if union.is_empty() {
                continue;
            }
            let (label, keywords) = name_cluster(&files, &union);
            let color = CLUSTER_PALETTE[p % CLUSTER_PALETTE.len()];
            conn.execute(
                "INSERT INTO clusters(root_id,label,color,keywords,size,built_at,parent) VALUES(?1,?2,?3,?4,?5,?6,0)",
                rusqlite::params![rep_root(&files, &union), label, color, keywords, union.len() as i64, built_at],
            )
            .map_err(|e| e.to_string())?;
            parent_ids[p] = conn.last_insert_rowid();
        }
    }

    // 再写叶簇(两级时 parent=父 id 且与父同色;单级时 parent=0)并把文件挂到叶簇。
    for (li, mem) in members.iter().enumerate() {
        let (label, keywords) = name_cluster(&files, mem);
        let p = parent_of_leaf[li];
        let (parent, color) = if two_level {
            (parent_ids[p], CLUSTER_PALETTE[p % CLUSTER_PALETTE.len()])
        } else {
            (0i64, CLUSTER_PALETTE[new_clusters % CLUSTER_PALETTE.len()])
        };
        conn.execute(
            "INSERT INTO clusters(root_id,label,color,keywords,size,built_at,parent) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![rep_root(&files, mem), label, color, keywords, mem.len() as i64, built_at, parent],
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

/// 确定性球面 k-means(余弦):farthest-first 初始化 + Lloyd 迭代。
/// 输入向量须已 L2 归一化。返回 (每点所属簇下标, 各簇质心)。两级归类两层都复用它。
fn spherical_kmeans(vecs: &[Vec<f32>], k: usize) -> (Vec<usize>, Vec<Vec<f32>>) {
    let n = vecs.len();
    if n == 0 || k == 0 {
        return (vec![0; n], Vec::new());
    }
    let k = k.min(n);
    let dim = vecs[0].len();
    // 确定性初始化:farthest-first traversal(余弦),避免依赖随机数
    let mut centroids: Vec<Vec<f32>> = Vec::with_capacity(k);
    centroids.push(vecs[0].clone());
    while centroids.len() < k {
        let mut best_i = 0usize;
        let mut best_d = f32::MIN;
        for (i, v) in vecs.iter().enumerate() {
            let max_sim = centroids.iter().map(|c| dot(c, v)).fold(f32::MIN, f32::max);
            let d = -max_sim;
            if d > best_d {
                best_d = d;
                best_i = i;
            }
        }
        centroids.push(vecs[best_i].clone());
    }
    // Lloyd 迭代(球面)
    let mut assign = vec![0usize; n];
    for _ in 0..16 {
        let mut changed = false;
        for (i, v) in vecs.iter().enumerate() {
            let mut best = 0usize;
            let mut best_sim = f32::MIN;
            for (ci, c) in centroids.iter().enumerate() {
                let s = dot(c, v);
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
        let mut sums = vec![vec![0.0f32; dim]; k];
        let mut counts = vec![0u32; k];
        for (i, v) in vecs.iter().enumerate() {
            let c = assign[i];
            for (a, b) in sums[c].iter_mut().zip(v.iter()) {
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
    (assign, centroids)
}

/// 簇代表根 = 成员里出现最多的 root_id。
fn rep_root(files: &[FileVec], members: &[usize]) -> i64 {
    let mut freq: HashMap<i64, usize> = HashMap::new();
    for &i in members {
        *freq.entry(files[i].root_id).or_insert(0) += 1;
    }
    freq.into_iter().max_by_key(|(_, c)| *c).map(|(r, _)| r).unwrap_or(0)
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

/// 把杂乱/带噪文件名清洗成可读标题(纯字符串,无 IO,grid 里现算):
/// 去扩展名 → 分隔符(_ - + ~ . 空格 括号 ·)切词 → 丢纯数字/时间戳/长哈希/常见噪声词
/// (img/screenshot/副本/微信图片…)→ 余下用空格连。清完为空(纯哈希图片名等)退回去扩展名的原名。
/// 这是「本地档」标题;AI 档会把更难的(纯乱码/纯哈希)写进 titles 表覆盖它。
fn clean_title(name: &str) -> String {
    const NOISE: &[&str] = &[
        "copy", "final", "副本", "未命名", "untitled", "new", "draft", "tmp", "temp", "out",
        "img", "image", "photo", "pic", "dsc", "vid", "video", "screenshot", "截图", "屏幕截图",
        "微信图片", "mmexport", "download", "下载", "wechat", "qq图片",
    ];
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in stem.chars() {
        if matches!(
            ch,
            '_' | '-' | '+' | '~' | '.' | ' ' | '(' | ')' | '[' | ']' | '{' | '}' | '·' | '@' | '#'
        ) {
            let t = cur.trim();
            if !t.is_empty() {
                parts.push(t.to_string());
            }
            cur.clear();
        } else {
            cur.push(ch);
        }
    }
    let t = cur.trim();
    if !t.is_empty() {
        parts.push(t.to_string());
    }

    let is_noise = |t: &str| -> bool {
        let low = t.to_lowercase();
        if t.chars().all(|c| c.is_ascii_digit()) {
            return true; // 纯数字:计数器/年份/时间戳片段
        }
        if t.len() >= 8 && t.chars().all(|c| c.is_ascii_hexdigit()) {
            return true; // 长 hex:哈希样
        }
        NOISE.contains(&low.as_str())
    };

    let kept: Vec<String> = parts.into_iter().filter(|t| !is_noise(t)).collect();
    let title = kept.join(" ");
    let title = title.trim();
    if title.chars().count() >= 2 {
        title.to_string()
    } else {
        stem.trim().to_string() // 全是噪声/太短 → 退回原名(去扩展名),总比空好
    }
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

// ───────────────────────── 大模型语义归类(免嵌入 key) ─────────────────────────
//
// 用户没有硅基嵌入 key,但聊天大模型(claude/配置的供应商)已经接通 → 直接让它读
// 文件清单、按主题归类,Rust 写回 cluster_id + clusters 表,并在桌面生成 HTML 报告。
// 复用回声层「做梦」同一套:run_claude_readonly(无头跑已连接模型)+ extract_balanced_json。

// ── 归类专用模型(可选):独立于「对话供应商」,可指向便宜/免费的 OpenAI 兼容端点 ──
// 不配 → AI 归类沿用你聊天那个大模型;配了 → 走这个(例:硅基流动免费对话模型,省钱)。

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ClusterModelCfg {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    model: String,
}

fn cluster_model_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join("cluster_model.json"))
}
fn load_cluster_model() -> ClusterModelCfg {
    cluster_model_path()
        .filter(|p| p.exists())
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}
fn save_cluster_model(cfg: &ClusterModelCfg) -> Result<(), String> {
    let path = cluster_model_path().ok_or("无法定位数据目录")?;
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let txt = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, txt).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}
/// 生效的归类模型:enabled + key + base_url + model 四件齐才算。
fn active_cluster_model() -> Option<ClusterModelCfg> {
    let c = load_cluster_model();
    let ok = c.enabled
        && !c.api_key.trim().is_empty()
        && !c.base_url.trim().is_empty()
        && !c.model.trim().is_empty();
    ok.then_some(c)
}
/// OpenAI 兼容 chat completion(硅基流动 / 任意兼容端点)。
fn chat_complete(cfg: &ClusterModelCfg, prompt: &str) -> Result<String, String> {
    let url = format!("{}/v1/chat/completions", cfg.base_url.trim_end_matches('/'));
    let http = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(180))
        .build();
    let resp = http
        .post(&url)
        .set("authorization", &format!("Bearer {}", cfg.api_key.trim()))
        .send_json(json!({
            "model": cfg.model,
            "messages": [{ "role": "user", "content": prompt }],
            "temperature": 0.2,
            "stream": false,
        }));
    match resp {
        Ok(r) => {
            let v: Value = r.into_json().map_err(|e| format!("归类模型响应解析失败: {e}"))?;
            v.get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| "归类模型响应里没有 content".to_string())
        }
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_default();
            let brief: String = body.chars().take(220).collect();
            Err(format!("归类模型 HTTP {code}: {brief}"))
        }
        Err(e) => Err(format!("归类模型网络错误: {e}")),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterModelView {
    pub enabled: bool,
    pub base_url: String,
    pub model: String,
    pub key_set: bool,
}
fn cluster_model_view(c: &ClusterModelCfg) -> ClusterModelView {
    ClusterModelView {
        enabled: c.enabled,
        base_url: c.base_url.clone(),
        model: c.model.clone(),
        key_set: !c.api_key.trim().is_empty(),
    }
}

static LLM_CLUSTERING: AtomicBool = AtomicBool::new(false);

/// 喂给大模型的文件清单上限(控上下文;超出按 mtime 倒序取最近的)。
const LLM_FILE_CAP: usize = 240;

struct FileLite {
    id: i64,
    relpath: String,
    name: String,
    kind: String,
}

#[derive(Debug, Deserialize)]
struct LlmGroup {
    #[serde(default)]
    label: String,
    #[serde(default)]
    files: Vec<Value>,
    /// 两级归类:本组若是「大主题」,其下的子主题放这里(子主题再各自带 files)。
    #[serde(default)]
    groups: Vec<LlmGroup>,
}

/// 报告用:一个顶层主题 + 其下若干子主题(子主题各自的成员下标)。
struct ReportTheme {
    label: String,
    color: String,
    children: Vec<(String, Vec<usize>)>,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn emit_llm(app: &AppHandle, payload: Value) {
    let _ = app.emit("file:cluster_llm", payload);
}

/// 加载范围内文件(mtime 倒序,上限 LLM_FILE_CAP)给大模型归类。
fn load_files_for_llm(conn: &rusqlite::Connection, filter: &str) -> Result<Vec<FileLite>, String> {
    let sql = format!(
        "SELECT f.id, f.relpath, f.name, f.kind FROM files f
         WHERE 1=1{filter} ORDER BY f.mtime DESC LIMIT {LLM_FILE_CAP}"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(FileLite {
                id: r.get(0)?,
                relpath: r.get(1)?,
                name: r.get(2)?,
                kind: r.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.flatten().collect())
}

fn llm_cluster_directive(files: &[FileLite]) -> String {
    let mut list = String::new();
    for (i, f) in files.iter().enumerate() {
        list.push_str(&format!("[{i}] {} ({})\n", f.relpath, f.kind));
    }
    format!(
        r#"你是文件库的「语义归类员」。下面是用户文件库里的文件清单(每行:[序号] 相对路径 (类型))。
请**纯按内容主题 / 思想类型**把它们归成**两级**树:先归成几个「大主题」,每个大主题下再分若干「子主题」。
完全按语义归——把同一思想 / 同一话题 / 同一用途的文件放在一起,**不要按文件类型(图片/视频/文档)来分**。

要求:
- 大主题 3~8 个;每个大主题下 2~6 个子主题;子主题尽量 ≥2 个文件;
- 大主题是宽泛的思想/领域(如「产品设计」「财务合同」「学习资料」);子主题是其下更细的话题;
- 用文件名、目录、内容线索推断主题;同系列/同项目/同话题归一起;
- 每个文件最多归一个子主题;实在归不进的可不出现;
- 所有标签都用简短贴切的**中文**(大主题 2~8 字、子主题 4~12 字),别用「其它/杂项」这种空标签。

**只输出一个 JSON 数组,不要任何额外文字、不要 markdown 代码围栏**。格式为大主题数组,每个大主题含 groups 子主题数组:
[{{"label":"大主题","groups":[{{"label":"子主题","files":[序号,序号,...]}}, ...]}}, ...]

文件清单({} 个):
{list}"#,
        files.len()
    )
}

/// 把序号/路径字符串解析回文件下标。
fn resolve_index(v: &Value, files: &[FileLite]) -> Option<usize> {
    if let Some(n) = v.as_u64() {
        let i = n as usize;
        return (i < files.len()).then_some(i);
    }
    if let Some(s) = v.as_str() {
        if let Ok(i) = s.trim().parse::<usize>() {
            return (i < files.len()).then_some(i);
        }
        // 退化:按相对路径 / 文件名匹配
        return files
            .iter()
            .position(|f| f.relpath == s || f.name == s || f.relpath.ends_with(s));
    }
    None
}

fn cluster_llm_run(app: &AppHandle, root: Option<String>) -> Result<(usize, usize, String), String> {
    emit_llm(app, json!({ "kind": "phase", "text": "收集文件清单…" }));
    let conn = open_db()?;
    let ids = resolve_root_ids(&conn, &root);
    let filter = if ids.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        format!(" AND f.root_id IN ({})", list.join(","))
    };
    let files = load_files_for_llm(&conn, &filter)?;
    if files.len() < 2 {
        return Err("可归类的文件不足(<2),先点「盘点」扫描磁盘文件".into());
    }

    let prompt = llm_cluster_directive(&files);
    // 配了独立归类模型 → 直连它(省钱);否则用聊天那个大模型(run_claude_readonly)。
    let collected = if let Some(cfg) = active_cluster_model() {
        emit_llm(
            app,
            json!({ "kind": "phase", "text": format!("用独立归类模型「{}」给 {} 个文件归类…", cfg.model, files.len()) }),
        );
        chat_complete(&cfg, &prompt)?
    } else {
        emit_llm(
            app,
            json!({ "kind": "phase", "text": format!("用已连接的对话大模型给 {} 个文件归类…", files.len()) }),
        );
        let kb_root = PathBuf::from(crate::kb::kb_root());
        let cwd = if kb_root.exists() { kb_root } else { std::env::temp_dir() };
        crate::kb::run_claude_readonly(&cwd, &prompt, |kind, _text| {
            if kind == "delta" {
                emit_llm(app, json!({ "kind": "tick" })); // 心跳,不外泄正文
            }
        })?
    };
    let raw = crate::kb::extract_balanced_json(&collected)
        .ok_or("大模型没有返回可解析的 JSON(可换更强的模型,或稍后重试)")?;
    let groups: Vec<LlmGroup> =
        serde_json::from_str(&raw).map_err(|e| format!("归类 JSON 解析失败: {e}"))?;

    emit_llm(app, json!({ "kind": "phase", "text": "写回归类 + 生成桌面报告…" }));

    // 清旧簇(范围内)
    if ids.is_empty() {
        conn.execute("DELETE FROM clusters", []).ok();
        conn.execute("UPDATE files SET cluster_id=0", []).ok();
    } else {
        let inlist: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        let inlist = inlist.join(",");
        conn.execute(&format!("DELETE FROM clusters WHERE root_id IN ({inlist})"), []).ok();
        conn.execute(&format!("UPDATE files SET cluster_id=0 WHERE root_id IN ({inlist})"), []).ok();
    }

    let built_at = chrono::Local::now().timestamp_millis();
    let mut report_themes: Vec<ReportTheme> = Vec::new();
    let mut assigned = 0usize;
    let mut n_clusters = 0usize; // 叶簇数(实际承载文件的子主题)
    let mut color_i = 0usize;

    // 一个 group 的 files 字段 → 去重后的成员下标
    let resolve_members = |g: &LlmGroup| -> Vec<usize> {
        let mut m: Vec<usize> = Vec::new();
        for v in &g.files {
            if let Some(i) = resolve_index(v, &files) {
                if !m.contains(&i) {
                    m.push(i);
                }
            }
        }
        m
    };

    conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
    for top in &groups {
        let theme_label = if top.label.trim().is_empty() {
            "未命名主题".to_string()
        } else {
            top.label.trim().to_string()
        };
        // 子主题:模型给了 groups 用之;没给则把本组自身当成唯一子主题(扁平兜底,层级仍统一)
        let mut children: Vec<(String, Vec<usize>)> = Vec::new();
        if !top.groups.is_empty() {
            for sub in &top.groups {
                let m = resolve_members(sub);
                if m.is_empty() {
                    continue;
                }
                let lab = if sub.label.trim().is_empty() {
                    theme_label.clone()
                } else {
                    sub.label.trim().to_string()
                };
                children.push((lab, m));
            }
        } else {
            let m = resolve_members(top);
            if !m.is_empty() {
                children.push((theme_label.clone(), m));
            }
        }
        if children.is_empty() {
            continue;
        }

        let color = CLUSTER_PALETTE[color_i % CLUSTER_PALETTE.len()].to_string();
        color_i += 1;
        let total: usize = children.iter().map(|(_, m)| m.len()).sum();
        let root_id: i64 = conn
            .query_row("SELECT root_id FROM files WHERE id=?1", [files[children[0].1[0]].id], |r| {
                r.get(0)
            })
            .unwrap_or(0);

        // 顶层主题(父簇:不直接挂文件,size = 旗下文件总数)
        conn.execute(
            "INSERT INTO clusters(root_id,label,color,keywords,size,built_at,parent) VALUES(?1,?2,?3,'',?4,?5,0)",
            rusqlite::params![root_id, theme_label, color, total as i64, built_at],
        )
        .map_err(|e| e.to_string())?;
        let parent_id = conn.last_insert_rowid();

        // 子主题(叶簇:与父同色,挂文件)
        for (lab, m) in &children {
            let croot: i64 = conn
                .query_row("SELECT root_id FROM files WHERE id=?1", [files[m[0]].id], |r| r.get(0))
                .unwrap_or(root_id);
            conn.execute(
                "INSERT INTO clusters(root_id,label,color,keywords,size,built_at,parent) VALUES(?1,?2,?3,'',?4,?5,?6)",
                rusqlite::params![croot, lab, color, m.len() as i64, built_at, parent_id],
            )
            .map_err(|e| e.to_string())?;
            let cid = conn.last_insert_rowid();
            {
                let mut stmt = conn
                    .prepare_cached("UPDATE files SET cluster_id=?1 WHERE id=?2")
                    .map_err(|e| e.to_string())?;
                for &i in m {
                    stmt.execute(rusqlite::params![cid, files[i].id]).map_err(|e| e.to_string())?;
                }
            }
            assigned += m.len();
            n_clusters += 1;
        }
        report_themes.push(ReportTheme { label: theme_label, color, children });
    }
    conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;

    let report_path = write_report_html(&files, &report_themes, assigned)?;
    Ok((n_clusters, assigned, report_path))
}

/// 生成自包含 HTML 报告 → 桌面,返回文件路径。
fn write_report_html(
    files: &[FileLite],
    themes: &[ReportTheme],
    assigned: usize,
) -> Result<String, String> {
    let now = chrono::Local::now();
    let stamp = now.format("%Y%m%d-%H%M%S").to_string();
    let human = now.format("%Y-%m-%d %H:%M").to_string();

    let mut n_sub = 0usize;
    let mut cards = String::new();
    for t in themes {
        let total: usize = t.children.iter().map(|(_, m)| m.len()).sum();
        let mut subs = String::new();
        for (lab, members) in &t.children {
            n_sub += 1;
            let mut items = String::new();
            for &i in members {
                let f = &files[i];
                items.push_str(&format!(
                    r#"<li><span class="dot" style="background:{c}"></span><span class="fn">{name}</span><span class="fp">{path}</span><span class="fk">{kind}</span></li>"#,
                    c = esc(&t.color),
                    name = esc(&f.name),
                    path = esc(&f.relpath),
                    kind = esc(&f.kind),
                ));
            }
            subs.push_str(&format!(
                r#"<div class="subgrp"><div class="subgrp-h"><span class="sbadge">{n}</span>{lab}</div><ul>{items}</ul></div>"#,
                n = members.len(),
                lab = esc(lab),
                items = items,
            ));
        }
        cards.push_str(&format!(
            r#"<section class="cluster"><header style="--c:{c}"><span class="badge">{n}</span><h2>{label}</h2></header><div class="subs">{subs}</div></section>"#,
            c = esc(&t.color),
            n = total,
            label = esc(&t.label),
            subs = subs,
        ));
    }

    let html = format!(
        r##"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>文件中心 · AI 语义归类报告</title>
<style>
:root{{--bg:#0f1115;--panel:#171a21;--line:#252a33;--ink:#e8e8e6;--mut:#9aa0ab;--gold:#d4b06a}}
*{{box-sizing:border-box}}
body{{margin:0;background:linear-gradient(180deg,#0f1115,#13161c);color:var(--ink);
font-family:-apple-system,"Segoe UI","PingFang SC","Microsoft YaHei",sans-serif;line-height:1.6}}
.wrap{{max-width:1040px;margin:0 auto;padding:48px 32px 100px}}
h1{{font-size:26px;margin:0 0 4px;letter-spacing:.5px}}
.sub{{color:var(--mut);font-size:13px}}
.stats{{display:flex;gap:28px;margin:24px 0 32px;padding:18px 22px;background:var(--panel);
border:1px solid var(--line);border-radius:14px}}
.stat .v{{font-size:24px;font-weight:650}}.stat .l{{color:var(--mut);font-size:12px}}
.grid{{display:grid;grid-template-columns:repeat(auto-fill,minmax(320px,1fr));gap:16px}}
.cluster{{background:var(--panel);border:1px solid var(--line);border-radius:14px;overflow:hidden}}
.cluster header{{display:flex;align-items:center;gap:10px;padding:14px 18px;
background:color-mix(in srgb,var(--c) 14%,transparent);border-bottom:1px solid var(--line)}}
.cluster header::before{{content:"";width:10px;height:10px;border-radius:50%;background:var(--c);
box-shadow:0 0 10px var(--c)}}
.cluster h2{{font-size:15px;margin:0;flex:1}}
.badge{{font-size:12px;color:#0f1115;background:var(--c);padding:1px 9px;border-radius:99px;font-weight:700}}
.subs{{padding:6px 0}}
.subgrp{{border-bottom:1px solid var(--line)}}
.subgrp:last-child{{border-bottom:none}}
.subgrp-h{{display:flex;align-items:center;gap:8px;padding:9px 18px 4px;font-size:12.5px;font-weight:600;color:var(--ink)}}
.sbadge{{font-size:10.5px;color:var(--mut);background:rgba(255,255,255,.06);padding:0 7px;border-radius:99px}}
.cluster ul{{list-style:none;margin:0;padding:2px 0 8px;max-height:420px;overflow:auto}}
.cluster li{{display:grid;grid-template-columns:auto 1fr auto;align-items:center;gap:8px;
padding:6px 18px;font-size:12.5px;border-bottom:1px solid rgba(255,255,255,.03)}}
.cluster li:hover{{background:rgba(255,255,255,.03)}}
.dot{{width:6px;height:6px;border-radius:50%}}
.fn{{color:var(--ink);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}}
.fp{{grid-column:2;color:var(--mut);font-size:10.5px;font-family:ui-monospace,Consolas,monospace;
overflow:hidden;text-overflow:ellipsis;white-space:nowrap}}
.fk{{color:var(--gold);font-size:10px;text-transform:uppercase}}
.foot{{margin-top:40px;color:#5a606b;font-size:12px;text-align:center}}
</style></head><body><div class="wrap">
<h1>文件中心 · AI 语义归类报告</h1>
<div class="sub">由已连接的大模型按语义两级归类 · 生成于 {human}</div>
<div class="stats">
<div class="stat"><div class="v">{ng}</div><div class="l">大主题</div></div>
<div class="stat"><div class="v">{ns}</div><div class="l">子主题</div></div>
<div class="stat"><div class="v">{na}</div><div class="l">已归类文件</div></div>
<div class="stat"><div class="v">{nt}</div><div class="l">参与归类</div></div>
</div>
<div class="grid">{cards}</div>
<div class="foot">Polaris 文件中心 · AI 语义两级归类 · 大模型按思想主题分组,非嵌入向量</div>
</div></body></html>"##,
        human = esc(&human),
        ng = themes.len(),
        ns = n_sub,
        na = assigned,
        nt = files.len(),
        cards = cards,
    );

    let desktop = directories::UserDirs::new()
        .and_then(|u| u.desktop_dir().map(|d| d.to_path_buf()))
        .or_else(|| directories::UserDirs::new().map(|u| u.home_dir().to_path_buf()))
        .ok_or("找不到桌面目录")?;
    let path = desktop.join(format!("文件中心-AI归类报告-{stamp}.html"));
    std::fs::write(&path, html).map_err(|e| format!("写报告失败: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

// ───────────────────────── AI 智能命名(可选;免嵌入 key) ─────────────────────────
//
// 「本地档」清洗名(clean_title)救不了的(纯哈希/纯乱码/纯时间戳图片名),交给已连接的大模型
// 按目录+类型+名字线索起个可读中文标题,写进 titles 表覆盖显示(磁盘文件名不动)。复用归类那套
// (独立归类模型 or run_claude_readonly + extract_balanced_json)。

static LLM_TITLING: AtomicBool = AtomicBool::new(false);

fn emit_title(app: &AppHandle, payload: Value) {
    let _ = app.emit("file:title_llm", payload);
}

#[derive(Debug, Deserialize)]
struct LlmTitle {
    #[serde(default)]
    i: Value,
    #[serde(default)]
    title: String,
}

fn titles_llm_directive(files: &[FileLite]) -> String {
    let mut list = String::new();
    for (i, f) in files.iter().enumerate() {
        list.push_str(&format!("[{i}] {} | {} ({})\n", f.name, f.relpath, f.kind));
    }
    format!(
        r#"你是文件库的「智能命名员」。下面每行是一个文件:[序号] 原文件名 | 相对路径 (类型)。
很多原文件名是乱码、哈希、时间戳或无意义的(如 IMG_20230101、a1b2c3d4.jpg、新建文档)。
请根据**文件名线索 + 所在目录 + 类型**,为每个文件起一个**简短、可读、能概括内容**的中文标题(4~16 字)。

要求:
- 标题要像人给文件起的名,别保留原始乱码/哈希/纯时间戳;
- 拿不准的就根据目录和类型给个合理概括(如「项目截图」「会议记录」「数据表」);
- 不要带扩展名;不要加引号;每个文件都要有标题。

**只输出一个 JSON 数组,无任何额外文字、无 markdown 围栏**:
[{{"i":序号,"title":"标题"}}, ...]

文件清单({} 个):
{list}"#,
        files.len()
    )
}

fn titles_llm_run(app: &AppHandle, root: Option<String>) -> Result<usize, String> {
    emit_title(app, json!({ "kind": "phase", "text": "收集文件清单…" }));
    let conn = open_db()?;
    let ids = resolve_root_ids(&conn, &root);
    let filter = if ids.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        format!(" AND f.root_id IN ({})", list.join(","))
    };
    let files = load_files_for_llm(&conn, &filter)?;
    if files.is_empty() {
        return Err("没有可命名的文件,先点「盘点」扫描磁盘文件".into());
    }

    let prompt = titles_llm_directive(&files);
    let collected = if let Some(cfg) = active_cluster_model() {
        emit_title(
            app,
            json!({ "kind": "phase", "text": format!("用独立归类模型「{}」给 {} 个文件起名…", cfg.model, files.len()) }),
        );
        chat_complete(&cfg, &prompt)?
    } else {
        emit_title(
            app,
            json!({ "kind": "phase", "text": format!("用已连接的对话大模型给 {} 个文件起名…", files.len()) }),
        );
        let kb_root = PathBuf::from(crate::kb::kb_root());
        let cwd = if kb_root.exists() { kb_root } else { std::env::temp_dir() };
        crate::kb::run_claude_readonly(&cwd, &prompt, |kind, _text| {
            if kind == "delta" {
                emit_title(app, json!({ "kind": "tick" }));
            }
        })?
    };
    let raw = crate::kb::extract_balanced_json(&collected)
        .ok_or("大模型没有返回可解析的 JSON(可换更强的模型,或稍后重试)")?;
    let arr: Vec<LlmTitle> =
        serde_json::from_str(&raw).map_err(|e| format!("标题 JSON 解析失败: {e}"))?;

    emit_title(app, json!({ "kind": "phase", "text": "写回标题…" }));
    let made_at = chrono::Local::now().timestamp_millis();
    let mut n = 0usize;
    conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
    {
        let mut stmt = conn
            .prepare_cached(
                "INSERT OR REPLACE INTO titles(file_id,title,source,made_at) VALUES(?1,?2,'llm',?3)",
            )
            .map_err(|e| e.to_string())?;
        for t in &arr {
            let title = t.title.trim();
            if title.is_empty() {
                continue;
            }
            if let Some(idx) = resolve_index(&t.i, &files) {
                stmt.execute(rusqlite::params![files[idx].id, title, made_at])
                    .map_err(|e| e.to_string())?;
                n += 1;
            }
        }
    }
    conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
    Ok(n)
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

/// 用已连接的大模型按语义归类(免嵌入 key)+ 桌面生成 HTML 报告。
/// 后台线程跑,进度走 `file:cluster_llm` 事件(phase/tick/done/error)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_cluster_llm(app: AppHandle, root: Option<String>) -> Result<(), String> {
    if LLM_CLUSTERING.swap(true, Ordering::SeqCst) {
        return Err("AI 归类正在进行中".into());
    }
    std::thread::spawn(move || {
        let res = cluster_llm_run(&app, root);
        LLM_CLUSTERING.store(false, Ordering::SeqCst);
        match res {
            Ok((clusters, assigned, report)) => emit_llm(
                &app,
                json!({ "kind": "done", "clusters": clusters, "assigned": assigned, "report": report }),
            ),
            Err(e) => emit_llm(&app, json!({ "kind": "error", "message": e })),
        }
    });
    Ok(())
}

/// AI 智能命名:给杂乱/乱码文件名起可读中文标题,写进 titles 表(只覆盖显示,不改磁盘)。
/// 后台线程跑,进度走 `file:title_llm` 事件(phase/tick/done/error)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_titles_llm(app: AppHandle, root: Option<String>) -> Result<(), String> {
    if LLM_TITLING.swap(true, Ordering::SeqCst) {
        return Err("AI 命名正在进行中".into());
    }
    std::thread::spawn(move || {
        let res = titles_llm_run(&app, root);
        LLM_TITLING.store(false, Ordering::SeqCst);
        match res {
            Ok(n) => emit_title(&app, json!({ "kind": "done", "count": n })),
            Err(e) => emit_title(&app, json!({ "kind": "error", "message": e })),
        }
    });
    Ok(())
}

/// 清空 AI 标题 → 卡片标题回落到本地清洗名(撤销 AI 命名)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_titles_clear() -> Result<usize, String> {
    let conn = open_db()?;
    conn.execute("DELETE FROM titles", []).map_err(|e| e.to_string())
}

/// 读「归类专用模型」配置(key 只回 key_set,不回明文)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_cluster_model_get() -> ClusterModelView {
    cluster_model_view(&load_cluster_model())
}

/// 存「归类专用模型」配置。api_key 传空字符串=保留旧 key(方便只改模型不重填 key)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_cluster_model_set(
    enabled: Option<bool>,
    base_url: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
) -> Result<ClusterModelView, String> {
    let mut c = load_cluster_model();
    if let Some(e) = enabled {
        c.enabled = e;
    }
    if let Some(b) = base_url {
        if !b.trim().is_empty() {
            c.base_url = b.trim().to_string();
        }
    }
    if let Some(m) = model {
        if !m.trim().is_empty() {
            c.model = m.trim().to_string();
        }
    }
    if let Some(k) = api_key {
        if !k.trim().is_empty() {
            c.api_key = k.trim().to_string(); // 空=保留旧 key
        }
    }
    if c.base_url.trim().is_empty() {
        c.base_url = "https://api.siliconflow.cn".into();
    }
    save_cluster_model(&c)?;
    Ok(cluster_model_view(&c))
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

    #[test]
    fn clean_title_strips_noise_keeps_meaning() {
        // 时间戳/计数器/分隔符清掉,保留有意义的词
        assert_eq!(clean_title("全澳房产_dataset_v2 (3).csv"), "全澳房产 dataset v2");
        // 纯噪声图片名:无可保留 → 退回去扩展名的原名(总比空好)
        assert_eq!(clean_title("IMG_20230101_123456.jpg"), "IMG_20230101_123456");
        // 长 hex 哈希被丢
        assert_eq!(clean_title("a1b2c3d4e5f6 报告.pdf"), "报告");
        // 正常中文名原样
        assert_eq!(clean_title("会议纪要.docx"), "会议纪要");
    }
}
