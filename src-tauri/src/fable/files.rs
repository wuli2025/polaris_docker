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

use super::{open_db, worker_count, FlagGuard};
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
/// 当簇标签会显得「不是给人看」的目录名:① 通用容器名(files/data/新建文件夹)② 技术/格式/
/// 工程目录名(html/css/js/log/node_modules/target…)。命名与词法向量都跳过它们,免得聚出
/// 「html」「dist」「log」这类机器味分类——用户要主题(报税/装修),不是格式或工程目录。
const GENERIC_DIRS: &[&str] = &[
    // 通用容器
    "raw", "wiki", "output", "memory", "src", "docs", "doc", "data", "assets", "public",
    "dist", "build", "tmp", "temp", "files", "file", "新建文件夹", "downloads", "下载",
    "documents", "文档", "desktop", "桌面", "untitled", "misc", "other", "others", "杂项", "其它", "其他",
    // 技术/格式/工程目录(常是软件生成、非人看)
    "html", "htm", "css", "js", "ts", "jsx", "tsx", "json", "xml", "yaml", "yml",
    "log", "logs", "cache", "caches", "bin", "obj", "lib", "libs", "include", "vendor",
    "target", "node_modules", "venv", "__pycache__", ".git", ".idea", ".vscode",
    "static", "scripts", "styles", "fonts", "icons", "thumbnails", "thumbs", "cache_data",
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

/// 「按语言归类」的一档:编程语言(Python/Rust…)/ 自然语言(中文/英文)/ 媒体大类(图片/视频…)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LangCount {
    pub lang: String,
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
    /// 按语言归类的分布(编程语言 / 自然语言 / 媒体大类)。
    pub by_lang: Vec<LangCount>,
    pub clusters: Vec<ClusterView>,
    pub text_files: u64,
    pub embedded_files: u64,
    pub has_embed_provider: bool,
    pub clustered: bool,
    pub scanning: bool,
    pub indexing: bool,
}

/// 把 root 参数解析成 root_id 过滤子句(None = 全部根)。
///
/// 「全部根」时只保留**极大根**——剔除嵌套在另一个根之下的根。盘点会把用户选过的每个
/// 文件夹都各记成一个 root,日积月累常出现 `D:\` 与 `D:\polaris\...`、`C:\` 与
/// `C:\Windows\System32` 这种父子并存。父根扫描时已把子根的文件全收过一遍,若把两边的
/// 文件数/体积直接相加,同一批文件会被数 2~3 遍(实测把真实量抬成约 8 倍虚高)。只统计
/// 极大根即可去重,且非破坏性(不动库,父根被删后子根自然重新参与)。
fn resolve_root_ids(conn: &rusqlite::Connection, root: &Option<String>) -> Vec<i64> {
    // 显式指定单根 → 精确匹配。
    if let Some(r) = root.as_ref().map(|r| r.trim()).filter(|r| !r.is_empty()) {
        let mut ids = Vec::new();
        if let Ok(mut stmt) = conn.prepare("SELECT id FROM roots WHERE path=?1") {
            if let Ok(rows) = stmt.query_map([r], |row| row.get::<_, i64>(0)) {
                ids.extend(rows.flatten());
            }
        }
        return ids;
    }
    // 全部根 → 取极大根去重叠。
    let mut all: Vec<(i64, String)> = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT id, path FROM roots") {
        if let Ok(rows) =
            stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
        {
            all.extend(rows.flatten());
        }
    }
    maximal_root_ids(&all)
}

/// 从 (id, path) 列表里挑出极大根:凡 path 嵌套在另一条 path 之下的都剔除。
/// 拆成纯函数便于单测。Windows 路径不分大小写、分隔符统一成 `/` 再比。
fn maximal_root_ids(all: &[(i64, String)]) -> Vec<i64> {
    fn norm(p: &str) -> String {
        let s = p.replace('\\', "/");
        let s = s.trim_end_matches('/').to_string();
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s
        }
    }
    let normed: Vec<(i64, String)> = all.iter().map(|(id, p)| (*id, norm(p))).collect();
    normed
        .iter()
        .filter(|(id, p)| {
            // 若存在另一条根 op 是 p 的祖先(p 以 "op/" 开头)→ p 是子根,剔除。
            !normed.iter().any(|(oid, op)| {
                oid != id && p.len() > op.len() && p.starts_with(&format!("{op}/"))
            })
        })
        .map(|(id, _)| *id)
        .collect()
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

/// 文件的「语言归类」标签:优先用回填好的 lang 列;为空时由 ext/kind 当场推(代码/媒体准确,
/// 文稿尚未回填自然语言 → 归「文档·待识别」)。grid 过滤与 overview 折叠共用同一口径。
fn language_label(stored: &str, ext: &str, kind: &str) -> String {
    if !stored.is_empty() {
        return stored.to_string();
    }
    let q = super::inventory::quick_lang(ext, kind);
    if q.is_empty() {
        "文档·待识别".to_string()
    } else {
        q
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

    // 按语言分布:GROUP BY (lang, ext, kind),Rust 里折成语言标签。代码/媒体即便 lang 列还没回填
    // 也能由 ext/kind 当场推出(零等待);文稿的中文/英文需回填 lang 后才细分,未回填前归「文档·待识别」。
    let mut by_lang = {
        let mut agg: HashMap<String, (u64, u64)> = HashMap::new();
        let sql = format!(
            "SELECT f.lang, f.ext, f.kind, COUNT(*), COALESCE(SUM(f.size),0)
             FROM files f WHERE 1=1{filter} GROUP BY f.lang, f.ext, f.kind"
        );
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(rows) = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)? as u64,
                    r.get::<_, i64>(4)? as u64,
                ))
            }) {
                for (lang, ext, kind, count, bytes) in rows.flatten() {
                    let label = language_label(&lang, &ext, &kind);
                    let e = agg.entry(label).or_insert((0, 0));
                    e.0 += count;
                    e.1 += bytes;
                }
            }
        }
        agg.into_iter()
            .map(|(lang, (count, bytes))| LangCount { lang, count, bytes })
            .collect::<Vec<_>>()
    };
    by_lang.sort_by(|a, b| b.count.cmp(&a.count));

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
        by_lang,
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
    /// 来源徽标:下载 / 微信 / QQ / 企业微信 / ""(普通文件,不显示)。按根路径 + relpath 识别。
    pub source: String,
}

/// 文件来源标签:按所属根路径末段 + 相对路径识别「下载 / 微信 / QQ…」。
/// 纯路径判断、零 IO;空串 = 普通文件。与 inventory::app_data_roots 的预设根对应。
fn source_tag(root_path: &str, relpath: &str) -> &'static str {
    let last = root_path
        .replace('\\', "/")
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_lowercase();
    let rel = relpath.replace('\\', "/").to_lowercase();
    if last == "downloads" {
        "下载"
    } else if last.contains("wechat") {
        // wechat files / xwechat_files / wechatfiles
        "微信"
    } else if last == "wxwork" {
        "企业微信"
    } else if last == "tencent files" || rel.contains("filerecv") {
        "QQ"
    } else {
        ""
    }
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
    lang: Option<String>,
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
    // 按语言过滤:代码/标记语言按扩展名集合(不依赖回填)、媒体按 kind、自然语言按回填好的 lang 列。
    if let Some(l) = lang.as_deref().map(str::trim).filter(|s| !s.is_empty() && *s != "all") {
        let exts = super::inventory::exts_for_lang(l);
        if !exts.is_empty() {
            let list: Vec<String> = exts.iter().map(|e| format!("'{e}'")).collect();
            where_sql.push_str(&format!(" AND LOWER(f.ext) IN ({})", list.join(",")));
        } else if let Some(k) = super::inventory::kind_for_media_lang(l) {
            where_sql.push_str(&format!(" AND f.kind='{k}'"));
        } else if l == "文档·待识别" {
            let codes: Vec<String> =
                super::inventory::CODE_EXTS.iter().map(|e| format!("'{e}'")).collect();
            where_sql.push_str(&format!(
                " AND f.lang='' AND f.kind IN ('text','doc') AND LOWER(f.ext) NOT IN ({})",
                codes.join(",")
            ));
        } else {
            let safe = l.replace('\'', "''");
            where_sql.push_str(&format!(" AND f.lang='{safe}'"));
        }
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
            let source = source_tag(&root, &rel).to_string();
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
                source,
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
///
/// **必须带超时**:损坏 / 半截 / 格式刁钻的视频会让 ffmpeg 永久挂死,而本函数被文件中心 grid
/// 逐图调用(常跑在多核缩略图线程上)—— 一个挂死的 ffmpeg 进程就能拖死整个 grid 加载、表现为
/// 「文件中心卡死」。复用 [`crate::forge::run_with_timeout`]:超 15s 即杀进程返回,绝不永久阻塞。
fn video_frame(p: &Path, max: u32) -> Option<image::DynamicImage> {
    let tmp = std::env::temp_dir().join(format!("polaris-vf-{}.jpg", hash_key(&[&p.to_string_lossy()])));
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
    if crate::forge::run_with_timeout(cmd, 15, "ffmpeg 视频首帧").is_err() {
        let _ = std::fs::remove_file(&tmp);
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

/// 文件的词法特征向量:**按意思,不按格式**——文件名分词(权重 2.4)主导,文件夹段(0.7)次之。
/// **刻意不喂扩展名**:否则一堆 .html / .png / .log 会按「格式」抱团,聚出「网页」「图片」这种
/// 不是给人看的分类;用户要的是「报税」「装修」这类**主题**分类(格式维度另有「按语言/类型」筛选条)。
/// 相近命名的文件在余弦下自然靠拢。哈希进 LEX_DIM 维。
fn lexical_vec(relpath: &str, name: &str, _ext: &str) -> Vec<f32> {
    let mut v = vec![0f32; LEX_DIM];
    let segs: Vec<&str> = relpath.split('/').collect();
    for seg in segs.iter().take(segs.len().saturating_sub(1)) {
        let low = seg.trim().to_lowercase();
        if low.is_empty() || GENERIC_DIRS.contains(&low.as_str()) {
            continue;
        }
        v[(hash_token(&low) % LEX_DIM as u64) as usize] += 0.7;
    }
    for tok in tokenize(name) {
        v[(hash_token(&tok) % LEX_DIM as u64) as usize] += 2.4;
    }
    v
}

/// 词法兜底:加载范围内文件的**样本**(上限 6000)→ 归一化词法向量,用来算 k-means 质心。
///
/// 取样两路并集(各自去重、合计 ≤ `CAP`):
///  ① **最近改动的 `RECENT` 个**(mtime 倒序)—— 用户「当下在忙」的那摊活儿,务必让它**自成质心**,
///     否则在被某个大旧库(几十万文件)占满的库里,纯均匀取样会让最近的活儿一个质心都分不到、
///     被并进某个大旧簇里 → 星图上彻底看不见,用户感觉「不懂我」;
///  ② **id 哈希均匀散布全库**补齐到 `CAP` —— 保证所有老主题也都有质心、覆盖到全库。
/// 真正的「全覆盖指派」在 [`cluster_build_on`] 里对**全部文件**做(O(N·k)),取样只决定质心位置,
/// 故加 recency 偏置不影响覆盖率(仍 1.0),只让「最近主题」在星图里冒出来。
fn load_lexical_files(conn: &rusqlite::Connection, filter: &str) -> Result<Vec<FileVec>, String> {
    const CAP: usize = 6000;
    const RECENT: usize = 2000;
    let mut out: Vec<FileVec> = Vec::new();
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut take = |sql: String| -> Result<(), String> {
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
        for (id, root_id, relpath, name, ext) in rows.flatten() {
            if out.len() >= CAP {
                break;
            }
            if !seen.insert(id) {
                continue; // 已被「最近」路取过,不重复算
            }
            let mut v = lexical_vec(&relpath, &name, &ext);
            normalize(&mut v);
            out.push(FileVec { file_id: id, root_id, relpath, name, vec: v });
        }
        Ok(())
    };
    // ① 最近改动(mtime>0 跳过读不出时间的);② 哈希均匀补齐全库。
    take(format!(
        "SELECT f.id, f.root_id, f.relpath, f.name, f.ext FROM files f
         WHERE 1=1{filter} AND f.mtime>0 ORDER BY f.mtime DESC LIMIT {RECENT}"
    ))?;
    take(format!(
        "SELECT f.id, f.root_id, f.relpath, f.name, f.ext FROM files f
         WHERE 1=1{filter} ORDER BY (f.id * 2654435761) % 1000003 LIMIT {CAP}"
    ))?;
    Ok(out)
}

/// 归类用的「向量来源」:
/// - `Auto`:有已存嵌入向量走语义、否则自动退词法(默认,向后兼容)。
/// - `Lexical`:强制走结构/词法(路径+文件名+扩展名哈希),**秒级、零嵌入依赖** —— 文件中心
///   v3 的 T0「骨架图谱」用它,盘点完立刻出簇,不等任何向量。
/// - `Semantic`:强制走语义(均值池化已存向量);没向量则优雅退词法,绝不报错卡住流程。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClusterMode {
    Auto,
    Lexical,
    Semantic,
}

/// 单根/全库重建语义聚类:每文件 = 其 chunk 向量均值池化 → 球面 k-means(余弦) →
/// 写回 files.cluster_id + clusters 表。纯数学,不调嵌入 API。
/// 无嵌入向量时自动退化为词法归类(见 [`load_lexical_files`]),保证永远可用。
pub fn cluster_build(root: Option<String>) -> Result<ClusterBuildSummary, String> {
    cluster_build_mode(root, ClusterMode::Auto)
}

/// 见 [`ClusterMode`]:按指定向量来源重建聚类。`cluster_build` = `Auto`。
pub fn cluster_build_mode(
    root: Option<String>,
    want: ClusterMode,
) -> Result<ClusterBuildSummary, String> {
    let started = std::time::Instant::now();
    let conn = open_db()?;
    let ids = resolve_root_ids(&conn, &root);
    cluster_build_on(&conn, &ids, want, started)
}

/// 聚类核心(可注入连接,便于在隔离 db 上做准确度评测,见 `tests::cluster_eval_*`)。
/// `cluster_build_mode` 仅负责 open_db + 解析根,再委托本函数。
fn cluster_build_on(
    conn: &rusqlite::Connection,
    ids: &[i64],
    want: ClusterMode,
    started: std::time::Instant,
) -> Result<ClusterBuildSummary, String> {
    let filter = if ids.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        format!(" AND f.root_id IN ({})", list.join(","))
    };

    // 向量来源:Lexical 直接走结构特征(不读 chunks,秒级);其余先取已存嵌入向量。
    let mut mode = "semantic";
    let mut files: Vec<FileVec> = if want == ClusterMode::Lexical {
        mode = "lexical";
        load_lexical_files(&conn, &filter)?
    } else {
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
        let v: Vec<FileVec> = acc
            .into_iter()
            .filter(|(_, (_, n, ..))| *n > 0)
            .map(|(file_id, (mut vec, n, root_id, relpath, name))| {
                for x in vec.iter_mut() {
                    *x /= n as f32;
                }
                normalize(&mut vec);
                FileVec { file_id, root_id, relpath, name, vec }
            })
            .collect();
        if v.len() < 2 {
            // 没有(足够的)嵌入向量 → 退化为「结构/词法」归类:对全部文件用
            // 文件夹 + 文件名分词 + 扩展名的哈希特征向量,跑同一套球面 k-means。
            // 无需任何 key、离线即可用 —— 保证「智能归类」永远点得动、永远能把相似文件放一起;
            // 配了硅基 key 并建好向量索引后,Auto/Semantic 自动走上面的语义路(更准)。
            mode = "lexical";
            load_lexical_files(&conn, &filter)?
        } else {
            v
        }
    };

    if files.len() < 2 {
        return Ok(ClusterBuildSummary {
            clusters: 0,
            files: files.len(),
            seconds: started.elapsed().as_secs_f64(),
            note: "可归类的文件不足(<2),先点「盘点」扫描磁盘文件再归类".into(),
        });
    }
    // 稳定顺序(file_id 升序),让确定性初始化可复现
    files.sort_by_key(|f| f.file_id);

    let n = files.len();
    let file_vecs: Vec<Vec<f32>> = files.iter().map(|f| f.vec.clone()).collect();
    // 一级(叶):细粒度语义簇 —— 比 √n 再细一点(×1.4),让主题分得更碎、星图更有层次。
    let k = (((n as f64).sqrt() * 1.4).round() as usize).clamp(4, 32).min(n);
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
        let k_parent = ((n_leaf as f64).sqrt().ceil() as usize).clamp(3, 9).min(n_leaf);
        let cvecs: Vec<Vec<f32>> = leaf_idx.iter().map(|&c| leaf_centroids[c].clone()).collect();
        spherical_kmeans(&cvecs, k_parent).0
    } else {
        (0..n_leaf).collect()
    };
    let n_parents = parent_of_leaf.iter().copied().max().map(|m| m + 1).unwrap_or(0);

    // 清旧簇(对涉及的根)。簇 id 即将重排,旧关系边一并清掉,免得 cluster_edges 残留指向已删簇
    // (虽然 build_file_graph 会按现存簇过滤、不会渲染脏边,但清掉更干净、避免长期累积)。
    if ids.is_empty() {
        conn.execute("DELETE FROM clusters", []).ok();
        conn.execute("DELETE FROM cluster_edges", []).ok();
        conn.execute("UPDATE files SET cluster_id=0", []).ok();
    } else {
        let list: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        let inlist = list.join(",");
        conn.execute(&format!("DELETE FROM clusters WHERE root_id IN ({inlist})"), []).ok();
        conn.execute(&format!("DELETE FROM cluster_edges WHERE root_id IN ({inlist})"), []).ok();
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

    // 再写叶簇(两级时 parent=父 id 且与父同色;单级时 parent=0)并把【样本】文件挂到叶簇。
    // 同时记下每个叶簇的 (db id, 质心向量),供下面把【全部文件】指派到最近质心(全覆盖)。
    let mut leaf_db: Vec<(i64, Vec<f32>)> = Vec::with_capacity(n_leaf);
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
        leaf_db.push((cluster_id, leaf_centroids[leaf_idx[li]].clone()));
        new_clusters += 1;
    }

    // ── 全覆盖关键修(治「几分钟路径只归 6000、大库覆盖率暴跌」)──
    // 词法档:质心由样本(≤6000)算出,但要把**全部文件**指派到最近质心,而非只归样本。
    // 指派是 O(N·k) 纯点积,弱机也就几秒。语义档不做(无嵌入的文件没向量;全量嵌入交 T2)。
    let mut total_files = n;
    if mode == "lexical" && !leaf_db.is_empty() {
        let sql = format!("SELECT f.id, f.relpath, f.name, f.ext FROM files f WHERE 1=1{filter}");
        let mut counts: HashMap<i64, i64> = HashMap::new();
        {
            let mut sel = conn.prepare(&sql).map_err(|e| e.to_string())?;
            let mut up = conn
                .prepare_cached("UPDATE files SET cluster_id=?1 WHERE id=?2")
                .map_err(|e| e.to_string())?;
            let mut rows = sel.query([]).map_err(|e| e.to_string())?;
            let mut assigned_all = 0i64;
            while let Some(row) = rows.next().map_err(|e| e.to_string())? {
                let id: i64 = row.get(0).map_err(|e| e.to_string())?;
                let relpath: String = row.get(1).map_err(|e| e.to_string())?;
                let name: String = row.get(2).map_err(|e| e.to_string())?;
                let ext: String = row.get(3).map_err(|e| e.to_string())?;
                let mut v = lexical_vec(&relpath, &name, &ext);
                normalize(&mut v);
                let mut best = leaf_db[0].0;
                let mut best_s = f32::MIN;
                for (cid, cen) in &leaf_db {
                    let s = dot(cen, &v);
                    if s > best_s {
                        best_s = s;
                        best = *cid;
                    }
                }
                up.execute(rusqlite::params![best, id]).map_err(|e| e.to_string())?;
                *counts.entry(best).or_insert(0) += 1;
                assigned_all += 1;
            }
            total_files = assigned_all as usize;
        }
        // 叶簇 size = 全量指派后的真实计数;父簇 size = 旗下叶簇之和。
        let mut psum: HashMap<i64, i64> = HashMap::new();
        for (li, (cid, _)) in leaf_db.iter().enumerate() {
            let c = counts.get(cid).copied().unwrap_or(0);
            conn.execute("UPDATE clusters SET size=?1 WHERE id=?2", rusqlite::params![c, cid]).ok();
            if two_level {
                *psum.entry(parent_ids[parent_of_leaf[li]]).or_insert(0) += c;
            }
        }
        for (pid, c) in &psum {
            conn.execute("UPDATE clusters SET size=?1 WHERE id=?2", rusqlite::params![c, pid]).ok();
        }
    }

    conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;

    Ok(ClusterBuildSummary {
        clusters: new_clusters,
        files: total_files,
        seconds: started.elapsed().as_secs_f64(),
        note: if mode == "semantic" {
            format!("已按语义把 {total_files} 个已嵌入文本归成 {new_clusters} 簇")
        } else {
            format!(
                "已按文件夹/名称把 {total_files} 个文件归成 {new_clusters} 簇 · 配硅基 key 并建向量索引后可升级为语义归类"
            )
        },
    })
}

/// 文件中心「星图」数据:把语义簇 + 抽样文件组织成与知识图谱同构的 [`crate::kb::KbGraph`]
/// (root=我的资料 / folder=主题簇 / doc=文件星点),让 KnowledgeGraph.vue 的星河渲染直接复用。
/// 抽样防止上万文件拖垮 cytoscape:每簇最多 PER 个文件星点,总计最多 CAP。
fn build_file_graph(root: Option<String>) -> Result<crate::kb::KbGraph, String> {
    use crate::kb::{KbEdge, KbGraph, KbNode};
    let conn = open_db()?;
    let ids = resolve_root_ids(&conn, &root);
    let cfilter = if ids.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        format!(" WHERE root_id IN ({})", list.join(","))
    };
    let mut nodes: Vec<KbNode> = Vec::new();
    let mut edges: Vec<KbEdge> = Vec::new();
    const ROOT_ID: &str = "__me__";
    nodes.push(KbNode {
        id: ROOT_ID.into(),
        title: "我的资料".into(),
        category: String::new(),
        kind: "root".into(),
        summary: None,
    });

    // 主题簇节点 + 层级边(顶层接 root,子主题接父簇)。category 携带**簇色** → 前端按语义簇着色,
    // 一眼看出电脑上分了几个语义聚类(每个簇一种颜色,旗下文件同色);summary 携带 AI 的一句话画像。
    let mut cluster_set: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut cluster_ids: Vec<i64> = Vec::new();
    let mut colors: HashMap<i64, String> = HashMap::new();
    {
        let sql =
            format!("SELECT id, label, color, parent, summary FROM clusters{cfilter} ORDER BY size DESC");
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for (id, label, color, parent, summary) in rows.flatten() {
            cluster_ids.push(id);
            cluster_set.insert(id);
            colors.insert(id, color.clone());
            nodes.push(KbNode {
                id: format!("c{id}"),
                title: label,
                category: color,
                kind: "folder".into(),
                summary: (!summary.trim().is_empty()).then_some(summary),
            });
            let src = if parent == 0 { ROOT_ID.to_string() } else { format!("c{parent}") };
            edges.push(KbEdge { source: src, target: format!("c{id}"), rel: None });
        }
    }
    if cluster_ids.is_empty() {
        return Ok(KbGraph { nodes, edges });
    }

    // 簇间语义关系边(AI 推断:同源/进阶/方法论…),只连两端都在本范围渲染的簇 → 星图成真·关系图谱。
    {
        let efilter = if ids.is_empty() {
            String::new()
        } else {
            let list: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
            format!(" WHERE root_id IN ({})", list.join(","))
        };
        let sql = format!("SELECT src, dst, label FROM cluster_edges{efilter}");
        if let Ok(mut stmt) = conn.prepare(&sql) {
            if let Ok(rows) = stmt.query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?))
            }) {
                for (src, dst, label) in rows.flatten() {
                    if cluster_set.contains(&src) && cluster_set.contains(&dst) {
                        edges.push(KbEdge {
                            source: format!("c{src}"),
                            target: format!("c{dst}"),
                            rel: Some(if label.trim().is_empty() {
                                "相关".to_string()
                            } else {
                                label
                            }),
                        });
                    }
                }
            }
        }
    }

    // 抽样文件星点(挂到各自 cluster_id;每簇 ≤PER,总计 ≤CAP),标题优先用 AI 名。
    // 排序**优先报告性文件(文档/文本)与视频**,让星图主要呈现这些有内容、用户最在意的资料。
    const PER: usize = 40;
    const CAP: usize = 1200;
    let ffilter = if ids.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        format!(" AND f.root_id IN ({})", list.join(","))
    };
    let sql = format!(
        "SELECT f.id, f.cluster_id, COALESCE(t.title, f.name) AS title
         FROM files f LEFT JOIN titles t ON t.file_id=f.id
         WHERE f.cluster_id>0{ffilter}
         ORDER BY CASE WHEN f.kind IN ('doc','text') THEN 0 WHEN f.kind='video' THEN 1 ELSE 2 END,
                  f.mtime DESC"
    );
    let mut per_count: HashMap<i64, usize> = HashMap::new();
    let mut total = 0usize;
    if let Ok(mut stmt) = conn.prepare(&sql) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?))
        }) {
            for (fid, cid, title) in rows.flatten() {
                if total >= CAP {
                    break;
                }
                let c = per_count.entry(cid).or_insert(0);
                if *c >= PER {
                    continue;
                }
                *c += 1;
                total += 1;
                nodes.push(KbNode {
                    id: format!("f{fid}"),
                    title,
                    category: colors.get(&cid).cloned().unwrap_or_default(),
                    kind: "doc".into(),
                    summary: None,
                });
                edges.push(KbEdge { source: format!("c{cid}"), target: format!("f{fid}"), rel: None });
            }
        }
    }
    Ok(KbGraph { nodes, edges })
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
    // 过滤纯数字 + 常见噪声词 + **格式/技术词**(html/css/log/exe…)——这些当关键词或兜底簇名
    // 都是「机器味」,不是给人看的;主题词(报税/装修)才留。
    const TECH_TOK: &[&str] = &[
        "copy", "final", "html", "htm", "css", "js", "ts", "jsx", "tsx", "json", "xml", "yaml",
        "yml", "log", "logs", "tmp", "temp", "exe", "dll", "bin", "obj", "bak", "cache", "min",
        "index", "deck", "output", "raw", "dist", "build", "node", "vendor", "static",
    ];
    out.retain(|t| !t.chars().all(|c| c.is_ascii_digit()) && !TECH_TOK.contains(&t.as_str()));
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
// 文件清单、按主题归类,Rust 写回 cluster_id + clusters 表。
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

**命名是重中之重**:这是给「本人」看的个人知识库,标签要让他扫一眼就认出「这就是我那堆 XX」。
- 一律用**用户自己会用的中文大白话**,像他平时怎么称呼这些文件就怎么叫:
  「我的合同」「报税资料」「装修」「考研复习」「孩子照片」「工作汇报」「旅行」「副业接单」「发票收据」……
- 大主题可带「我的」口吻更亲切(如「我的项目」「我的财务」);**绝不要英文、绝不要技术黑话、绝不要文件夹原名**(像 raw/output/新建文件夹 这类);
- 出现最多、最近频繁出现的话题优先单独成主题(用户最关心高频的);
- 别用「其它 / 杂项 / 未分类」这种空标签——再小的一摊也给个具体中文名。

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

    emit_llm(app, json!({ "kind": "phase", "text": "写回归类…" }));

    // 清旧簇(范围内)+ 旧关系边(簇 id 即将重排)。
    if ids.is_empty() {
        conn.execute("DELETE FROM clusters", []).ok();
        conn.execute("DELETE FROM cluster_edges", []).ok();
        conn.execute("UPDATE files SET cluster_id=0", []).ok();
    } else {
        let inlist: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        let inlist = inlist.join(",");
        conn.execute(&format!("DELETE FROM clusters WHERE root_id IN ({inlist})"), []).ok();
        conn.execute(&format!("DELETE FROM cluster_edges WHERE root_id IN ({inlist})"), []).ok();
        conn.execute(&format!("UPDATE files SET cluster_id=0 WHERE root_id IN ({inlist})"), []).ok();
    }

    let built_at = chrono::Local::now().timestamp_millis();
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
    }
    conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;

    Ok((n_clusters, assigned, String::new()))
}

// ───────────────── 文件中心 v3 · 簇画像 + 大模型命名/关系(读懂全库,不止 240) ─────────────────
//
// 核心洞察:大模型不读「文件」,读「向量聚类后的簇画像」—— 成本与文件数脱钩,几万文件也只看几十段
// 摘要。[`cluster_build`](全量、无 240 上限)先把库分成簇,本段让模型给每个簇起**亲切的人话名**
// +一句**温暖的概括** + 推断**簇间关系**。既覆盖全量,又让用户觉得「它很懂我」。

fn kind_cn(k: &str) -> &'static str {
    match k {
        "text" => "文本",
        "doc" => "文档",
        "image" => "图片",
        "audio" => "音频",
        "video" => "视频",
        "archive" => "压缩包",
        _ => "其它",
    }
}

/// 把大模型给的 id 字段(可能是数字或字符串)宽松解析成簇 id。
fn loose_i64(v: &Value) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    v.as_str().and_then(|s| s.trim().parse::<i64>().ok())
}

/// 一个簇的「画像」:喂给大模型命名/关系用。纯结构信号 + 代表文件名,**不抽 gist、不读盘**,
/// 故秒级可成 —— T1 能快速出第一波。
struct ClusterDigest {
    id: i64,
    parent: i64,
    label: String,
    keywords: String,
    size: i64,
    folders: Vec<String>,
    samples: Vec<String>,
    kinds: Vec<(String, usize)>,
}

/// 为范围内**每个簇**(大主题 + 子主题)生成画像。大主题的样本取自旗下子簇文件。
fn collect_cluster_digests(
    conn: &rusqlite::Connection,
    ids: &[i64],
) -> Result<Vec<ClusterDigest>, String> {
    let cfilter = if ids.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        format!(" WHERE root_id IN ({})", list.join(","))
    };
    let clusters: Vec<(i64, i64, String, String, i64)> = {
        let sql =
            format!("SELECT id, parent, label, keywords, size FROM clusters{cfilter} ORDER BY size DESC");
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.flatten().collect()
    };
    if clusters.is_empty() {
        return Ok(Vec::new());
    }
    // parent → 旗下叶簇 id
    let mut children: HashMap<i64, Vec<i64>> = HashMap::new();
    for (id, parent, ..) in &clusters {
        if *parent != 0 {
            children.entry(*parent).or_default().push(*id);
        }
    }
    let mut digests = Vec::with_capacity(clusters.len());
    for (id, parent, label, keywords, size) in &clusters {
        let leaf_ids: Vec<i64> = children.get(id).cloned().unwrap_or_else(|| vec![*id]);
        let inlist: String =
            leaf_ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT f.relpath, f.name, f.kind, t.title
             FROM files f LEFT JOIN titles t ON t.file_id=f.id
             WHERE f.cluster_id IN ({inlist})
             ORDER BY CASE WHEN f.kind IN ('doc','text') THEN 0 WHEN f.kind='video' THEN 1 ELSE 2 END,
                      f.mtime DESC
             LIMIT 80"
        );
        let mut dir_freq: HashMap<String, usize> = HashMap::new();
        let mut kind_freq: HashMap<String, usize> = HashMap::new();
        let mut samples: Vec<String> = Vec::new();
        {
            let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            for (relpath, name, kind, title) in rows.flatten() {
                let segs: Vec<&str> = relpath.split('/').collect();
                for seg in segs.iter().take(segs.len().saturating_sub(1)) {
                    let s = seg.trim();
                    if s.is_empty() || GENERIC_DIRS.contains(&s.to_lowercase().as_str()) {
                        continue;
                    }
                    *dir_freq.entry(s.to_string()).or_insert(0) += 1;
                }
                *kind_freq.entry(kind).or_insert(0) += 1;
                if samples.len() < 12 {
                    let nm = title
                        .filter(|t| !t.trim().is_empty())
                        .unwrap_or_else(|| clean_title(&name));
                    let nm = nm.trim().to_string();
                    if !nm.is_empty() && !samples.contains(&nm) {
                        samples.push(nm);
                    }
                }
            }
        }
        let mut folders: Vec<(String, usize)> = dir_freq.into_iter().collect();
        folders.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let folders: Vec<String> = folders.into_iter().take(4).map(|(d, _)| d).collect();
        let mut kinds: Vec<(String, usize)> = kind_freq.into_iter().collect();
        kinds.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        digests.push(ClusterDigest {
            id: *id,
            parent: *parent,
            label: label.clone(),
            keywords: keywords.clone(),
            size: *size,
            folders,
            samples,
            kinds,
        });
    }
    Ok(digests)
}

/// 把簇画像拼成「起名 + 关系」指令(只让模型读这几十段摘要,与文件总数无关)。
fn digest_directive(digests: &[ClusterDigest]) -> String {
    let mut body = String::new();
    for d in digests {
        let role = if d.parent == 0 { "大主题" } else { "子主题" };
        body.push_str(&format!(
            "[id={}] {}(文件 {} 个,现名「{}」)\n",
            d.id, role, d.size, d.label
        ));
        if !d.folders.is_empty() {
            body.push_str(&format!("  常见目录: {}\n", d.folders.join(" / ")));
        }
        if !d.keywords.trim().is_empty() {
            body.push_str(&format!("  关键词: {}\n", d.keywords.trim()));
        }
        if !d.samples.is_empty() {
            body.push_str(&format!("  代表文件: {}\n", d.samples.join("、")));
        }
        if !d.kinds.is_empty() {
            let ks: Vec<String> =
                d.kinds.iter().take(4).map(|(k, n)| format!("{}×{}", kind_cn(k), n)).collect();
            body.push_str(&format!("  类型: {}\n", ks.join(" ")));
        }
    }
    format!(
        r#"你是用户私人文件库的「知识管家」,非常懂这个人。下面是他文件库里**已经聚好的若干簇**
(每个簇带:id、是大主题还是子主题、文件数、现有粗略名字、常见目录、关键词、代表文件名、类型分布)。

请为**每一个簇**做两件事,让主人一眼觉得「这个软件太懂我了」:
1. 起一个**他自己会用的中文大白话名字**(像他平时怎么称呼这堆东西):
   「我的报税资料」「装修」「考研复习」「孩子的照片」「工作汇报」「副业接单」「合同发票」……
   - 大主题可带「我的」更亲切;子主题更具体;
   - **绝不要**英文、技术黑话、或 raw/output/新建文件夹 这类目录原名;也别用「其它/杂项/未分类」。
   - **绝不要拿文件格式/类型当名字**:哪怕一簇全是网页(html)/图片/视频/压缩包,也要按它们**讲的是什么事**
     来命名——一堆网页报告叫「项目周报」别叫「网页/html」;一堆图片若是旅行照就叫「旅行照片」别叫「图片」;
     下面每簇的「类型: …」只是帮你判断内容,**不是让你把格式名写成簇名**。
   - **用类别名,别用某一个的名字**:一簇大多是同一类东西时,叫这类东西的统称——
     一堆电影叫「电影」别叫「教父」;一堆照片叫「照片」别叫某张图名;一堆发票/报表叫「发票报表」
     别只叫「年度利润表」;一堆合同叫「合同」。簇名要能涵盖簇里**大多数**文件,而非只贴合某一个。
2. 写一句**温暖、具体、像朋友帮你整理完说的话**(summary,12~30 字),例如
   「你 2023-2024 报税要用的材料都收在这了」「准备考研那阵子刷的题和笔记」。

再**按意思合并**(merges):如果发现**几个簇其实是同一类东西,只是被文件夹/命名拆开了**——
例「发票」「invoices」「报销单」其实都是发票报销;「2022照片」「2023照片」其实都是照片;
「考研数学」「考研英语」若你觉得该合在「考研复习」下——就把它们的 id 列成一组放进 merges,
让它们并成一簇(并完用一个统称命名)。**只在确实同一类时合并,不同主题千万别硬并;宁可不并,不要乱并。**

再**推断簇与簇之间的关系**(relations):如某簇是另一簇的「方法论 / 前置 / 进阶 / 同源 / 印证 / 配套」。
只在确有关系时连,用簇 id 表方向(from→to);没把握就少连,别硬凑。

**只输出一个 JSON 对象,不要任何额外文字、不要 markdown 围栏**,格式:
{{"names":[{{"id":簇id,"name":"大白话名字","summary":"一句温暖概括"}}, ...],
  "merges":[[簇id,簇id,...], ...],
  "relations":[{{"from":簇id,"to":簇id,"label":"关系(如 方法论/进阶/同源)"}}, ...]}}

簇清单({} 个):
{body}"#,
        digests.len()
    )
}

#[derive(Debug, Deserialize, Default)]
struct LlmNameRel {
    #[serde(default)]
    names: Vec<LlmName>,
    #[serde(default)]
    relations: Vec<LlmRel>,
    /// 「按意思合并」:每组是一串簇 id,模型认为它们其实是同一类东西、只是被文件夹/命名拆开了
    /// (发票/invoices/报销单 → 一类)。服务端只接受**同父叶簇**的合并(同层、同主题旗下),
    /// 防把跨主题的簇乱并;并入后该组文件改挂最大簇,余簇删除。见 apply_names_and_relations。
    #[serde(default)]
    merges: Vec<Vec<Value>>,
}
#[derive(Debug, Deserialize)]
struct LlmName {
    #[serde(default)]
    id: Value,
    #[serde(default)]
    name: String,
    #[serde(default)]
    summary: String,
}
#[derive(Debug, Deserialize)]
struct LlmRel {
    #[serde(default)]
    from: Value,
    #[serde(default)]
    to: Value,
    #[serde(default)]
    label: String,
}

/// 校验 + 落库:先按模型给的 merges **按意思合并同义簇**,再把 names/relations 写进
/// clusters.label/summary + 重建 cluster_edges。纯函数式校验逻辑抽出来便于单测
/// (见 tests::rename_apply_*)。返回 (改名数, 关系边数, 合并掉的簇数)。
fn apply_names_and_relations(
    conn: &rusqlite::Connection,
    ids: &[i64],
    parsed: &LlmNameRel,
    valid: &std::collections::HashSet<i64>,
    croot: &HashMap<i64, i64>,
    built_at: i64,
) -> Result<(usize, usize, usize), String> {
    let mut renamed = 0usize;
    let mut edges = 0usize;
    let mut merged = 0usize;
    conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;

    // ── 0. 按意思合并(merges):把模型认定「其实是同一类、只是被文件夹/命名拆开」的簇并成一簇 ──
    // 安全闸:只合并**同父叶簇**(同层、同主题旗下;不动顶层大主题、不动有子簇的父簇),survivor 取最大簇;
    // 组内文件改挂 survivor,余簇删除。合并是「同父兄弟」之间故父簇总文件数不变(无需重算父 size)。
    let mut work_valid = valid.clone();
    let mut remap: HashMap<i64, i64> = HashMap::new(); // 被并旧 id → survivor
    if !parsed.merges.is_empty() {
        // 现有簇的 parent / size,以及「谁是别人的父」(父簇不可参与合并,否则孤立其子簇)。
        let mut pmap: HashMap<i64, i64> = HashMap::new();
        let mut smap: HashMap<i64, i64> = HashMap::new();
        let mut parent_set: std::collections::HashSet<i64> = std::collections::HashSet::new();
        {
            let mut stmt =
                conn.prepare("SELECT id, parent, size FROM clusters").map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))
                .map_err(|e| e.to_string())?;
            for (id, parent, size) in rows.flatten() {
                pmap.insert(id, parent);
                smap.insert(id, size);
                if parent != 0 {
                    parent_set.insert(parent);
                }
            }
        }
        for group in &parsed.merges {
            // 组内:合法 + 仍存活 + 不是某簇的父 + 在本范围 valid 里的簇 id(去重)。
            let mut g: Vec<i64> = group
                .iter()
                .filter_map(loose_i64)
                .filter(|id| work_valid.contains(id) && !parent_set.contains(id))
                .collect();
            g.sort_unstable();
            g.dedup();
            if g.len() < 2 {
                continue;
            }
            // 必须**同父**(同层、同主题旗下)才合并 —— 防把跨主题的叶簇乱并。
            let par = pmap.get(&g[0]).copied().unwrap_or(-1);
            if !g.iter().all(|id| pmap.get(id).copied().unwrap_or(-2) == par) {
                continue;
            }
            // survivor = 组内最大簇(size 最大;并列取最小 id,确定性)。
            g.sort_by(|a, b| {
                smap.get(b).cmp(&smap.get(a)).then(a.cmp(b))
            });
            let survivor = g[0];
            for &loser in &g[1..] {
                conn.execute(
                    "UPDATE files SET cluster_id=?1 WHERE cluster_id=?2",
                    rusqlite::params![survivor, loser],
                )
                .map_err(|e| e.to_string())?;
                conn.execute("DELETE FROM clusters WHERE id=?1", rusqlite::params![loser])
                    .map_err(|e| e.to_string())?;
                work_valid.remove(&loser);
                remap.insert(loser, survivor);
                merged += 1;
            }
            // survivor 真实大小重算(= 吸收后旗下文件计数);更新 smap 供后续组判断。
            conn.execute(
                "UPDATE clusters SET size=(SELECT COUNT(*) FROM files WHERE cluster_id=?1) WHERE id=?1",
                rusqlite::params![survivor],
            )
            .map_err(|e| e.to_string())?;
            if let Ok(ns) = conn.query_row(
                "SELECT size FROM clusters WHERE id=?1",
                rusqlite::params![survivor],
                |r| r.get::<_, i64>(0),
            ) {
                smap.insert(survivor, ns);
            }
        }
    }

    // 命名:被并旧 id 顺手指到 survivor;校验落在合并后仍存活的簇上。
    {
        let mut up = conn
            .prepare_cached("UPDATE clusters SET label=?1, summary=?2 WHERE id=?3")
            .map_err(|e| e.to_string())?;
        for n in &parsed.names {
            let Some(id0) = loose_i64(&n.id) else { continue };
            let id = remap.get(&id0).copied().unwrap_or(id0);
            if !work_valid.contains(&id) {
                continue;
            }
            let name = n.name.trim();
            if name.is_empty() {
                continue;
            }
            let name: String = name.chars().take(24).collect();
            let summary: String = n.summary.trim().chars().take(60).collect();
            up.execute(rusqlite::params![name, summary, id]).map_err(|e| e.to_string())?;
            renamed += 1;
        }
    }
    // 清范围内旧关系边,再重建(幂等)。
    if ids.is_empty() {
        conn.execute("DELETE FROM cluster_edges", []).ok();
    } else {
        let inlist: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        conn.execute(
            &format!("DELETE FROM cluster_edges WHERE root_id IN ({})", inlist.join(",")),
            [],
        )
        .ok();
    }
    {
        let mut ins = conn
            .prepare_cached(
                "INSERT INTO cluster_edges(root_id,src,dst,label,built_at) VALUES(?1,?2,?3,?4,?5)",
            )
            .map_err(|e| e.to_string())?;
        let mut seen: std::collections::HashSet<(i64, i64)> = std::collections::HashSet::new();
        for r in &parsed.relations {
            let (Some(a0), Some(b0)) = (loose_i64(&r.from), loose_i64(&r.to)) else {
                continue;
            };
            // 关系端点也跟着合并重映射(被并簇 → survivor),再去重去自环。
            let a = remap.get(&a0).copied().unwrap_or(a0);
            let b = remap.get(&b0).copied().unwrap_or(b0);
            if a == b || !work_valid.contains(&a) || !work_valid.contains(&b) || !seen.insert((a, b)) {
                continue;
            }
            let label: String = r.label.trim().chars().take(12).collect();
            let rid = croot.get(&a).copied().unwrap_or(0);
            ins.execute(rusqlite::params![rid, a, b, label, built_at]).map_err(|e| e.to_string())?;
            edges += 1;
            if edges >= 200 {
                break; // 关系边封顶,防爆图
            }
        }
    }
    conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
    Ok((renamed, edges, merged))
}

/// 让大模型**读簇画像**给全库的簇起亲切名 + 一句概括 + 簇间关系,然后落库。
/// 失败可降级:调用方(编排器)捕获错误后保留 cluster_build 的启发式名,不卡流程。
/// 返回 (改名簇数, 关系边数, 保留位)。
fn cluster_rename_llm(
    app: &AppHandle,
    root: Option<String>,
    tier: &str,
) -> Result<(usize, usize, String), String> {
    let conn = open_db()?;
    let ids = resolve_root_ids(&conn, &root);
    let digests = collect_cluster_digests(&conn, &ids)?;
    if digests.is_empty() {
        return Ok((0, 0, String::new()));
    }
    let valid: std::collections::HashSet<i64> = digests.iter().map(|d| d.id).collect();
    // 簇 → 所属根(关系边 root_id 按 src 簇定,范围删除对得上)。
    let mut croot: HashMap<i64, i64> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT id, root_id FROM clusters").map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| e.to_string())?;
        for (id, rid) in rows.flatten() {
            croot.insert(id, rid);
        }
    }
    let prompt = digest_directive(&digests);

    let collected = if let Some(cfg) = active_cluster_model() {
        emit_cluster(
            app,
            json!({ "kind": "phase", "tier": tier, "text": format!("用归类模型「{}」读懂 {} 个主题、起名…", cfg.model, digests.len()) }),
        );
        chat_complete(&cfg, &prompt)?
    } else {
        emit_cluster(
            app,
            json!({ "kind": "phase", "tier": tier, "text": format!("AI 正在读懂你的 {} 个主题、起亲切的名字…", digests.len()) }),
        );
        let kb_root = PathBuf::from(crate::kb::kb_root());
        let cwd = if kb_root.exists() { kb_root } else { std::env::temp_dir() };
        crate::kb::run_claude_readonly(&cwd, &prompt, |kind, _t| {
            if kind == "delta" {
                emit_cluster(app, json!({ "kind": "tick", "tier": tier }));
            }
        })?
    };
    let raw = crate::kb::extract_balanced_json(&collected)
        .ok_or("大模型没有返回可解析的 JSON(可换更强的模型,或稍后重试)")?;
    let parsed: LlmNameRel =
        serde_json::from_str(&raw).map_err(|e| format!("命名 JSON 解析失败: {e}"))?;

    let built_at = chrono::Local::now().timestamp_millis();
    let (renamed, edges, merged) =
        apply_names_and_relations(&conn, &ids, &parsed, &valid, &croot, built_at)?;
    if merged > 0 {
        emit_cluster(
            app,
            json!({ "kind": "phase", "tier": tier, "text": format!("AI 又按意思把 {merged} 个同义簇并进了相近主题") }),
        );
    }
    Ok((renamed, edges, String::new()))
}

// ───────────────────────── 「让 AI 更懂你」桌面画像 ─────────────────────────
//
// 引导流程收尾:盘点 + 归类 + 索引跑完后,根据 fable.db 现有统计(类型分布 / 语义主题 / 体量)
// **确定性地**生成一张自包含 HTML「知识画像」落到桌面 —— 不调大模型,秒级、必成、可离线打开。
// 让用户直观看到「AI 已经大概懂我了」:你有什么、AI 怎么理解、接下来能替你做什么。

fn human_bytes(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

fn pf_kind_label(k: &str) -> &'static str {
    match k {
        "text" => "文本",
        "doc" => "文档",
        "image" => "图片",
        "audio" => "音频",
        "video" => "视频",
        "archive" => "压缩包",
        _ => "其它",
    }
}

fn kind_color(k: &str) -> &'static str {
    match k {
        "text" => "#5fa8e6",
        "doc" => "#8b6cff",
        "image" => "#6fcf97",
        "audio" => "#e0a24b",
        "video" => "#e0736b",
        "archive" => "#93a0b4",
        _ => "#8a8f98",
    }
}

/// 一条「建议工作流」:据用户文件构成推断的、AI 能立刻替他做的事。
struct WorkflowHint {
    title: String,
    detail: String,
}

/// 据类型分布派生建议工作流(命中阈值才给,避免无中生有)。
fn workflow_hints(ov: &FileOverview) -> Vec<WorkflowHint> {
    let cnt = |k: &str| -> u64 { ov.by_kind.iter().find(|x| x.kind == k).map(|x| x.count).unwrap_or(0) };
    let mut out: Vec<WorkflowHint> = Vec::new();
    if cnt("video") >= 5 {
        out.push(WorkflowHint {
            title: "把影像素材做成作品集".into(),
            detail: format!(
                "你有 {} 个视频。我可以挑出代表作、配上文案与封面,生成一份可分享的作品集页面。",
                cnt("video")
            ),
        });
    }
    if cnt("doc") + cnt("text") >= 8 {
        out.push(WorkflowHint {
            title: "为你的文档写一篇结构化总结".into(),
            detail: format!(
                "你有 {} 份文档/文本。我可以通读后按主题归纳要点、抽取待办与关键结论,出一份总览。",
                cnt("doc") + cnt("text")
            ),
        });
    }
    if cnt("image") >= 20 {
        out.push(WorkflowHint {
            title: "整理图片成相册 / 图集".into(),
            detail: format!(
                "你有 {} 张图片。我可以按场景/时间归类,挑出精选,排成图集或九宫格。",
                cnt("image")
            ),
        });
    }
    if cnt("audio") >= 3 {
        out.push(WorkflowHint {
            title: "把录音转写并归档".into(),
            detail: format!(
                "你有 {} 段音频。我可以转写成文字、提炼摘要,沉淀进知识库随时可搜。",
                cnt("audio")
            ),
        });
    }
    if cnt("archive") >= 3 {
        out.push(WorkflowHint {
            title: "解包并整理压缩资料".into(),
            detail: format!("你有 {} 个压缩包。我可以梳理里面有什么,把有用的内容归进资源库。", cnt("archive")),
        });
    }
    if out.is_empty() {
        out.push(WorkflowHint {
            title: "从一个问题开始".into(),
            detail: "告诉我你最近在忙的事,我会沿着你的文件库找证据、帮你往前推进。".into(),
        });
    }
    out
}

/// AI 对用户的「一句话理解」(据主导类型 + 体量,口吻像助理读完资料后的感受)。
fn understanding_lines(ov: &FileOverview) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let top = ov.by_kind.first();
    if let Some(t) = top {
        out.push(format!(
            "我盘了你 {} 个文件、共 {},其中以「{}」最多。",
            ov.total_files,
            human_bytes(ov.total_bytes),
            pf_kind_label(&t.kind)
        ));
    }
    let leaf_themes = ov.clusters.iter().filter(|c| c.parent != 0).count();
    let top_themes = ov.clusters.iter().filter(|c| c.parent == 0).count();
    if top_themes > 0 {
        out.push(format!("我把它们归成了 {top_themes} 个大主题、{leaf_themes} 个子主题 —— 大致摸清了你关心什么。"));
    }
    if ov.embedded_files > 0 {
        out.push(format!(
            "已为 {}/{} 份文本建好语义索引,你可以直接问我「我那份关于⋯的资料在哪」。",
            ov.embedded_files, ov.text_files
        ));
    } else if ov.text_files > 0 {
        out.push("文本的语义索引正在后台建,建好后我就能按意思(而不只是文件名)帮你找东西了。".into());
    }
    out
}

// ── 智能向导收尾「建议工作流」:大模型据**真实知识库**智能匹配,而非固定阈值套话 ──

/// 一条注入对话框的建议:标题 + 「为什么是你」的依据 + 用户第一人称的提示词。
/// why = 一句话点名「他的哪个主题/文件夹/多少个文件」让我提这条 —— 收尾页据此让用户一眼觉得
/// 「这是独属于我的任务」,而不是放之四海皆准的套话(`#[serde(default)]`:模型漏给也不炸,空着即可)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestedFlow {
    pub title: String,
    #[serde(default)]
    pub why: String,
    pub prompt: String,
}

impl SuggestedFlow {
    /// 兜底:LLM 不可用 / 解析失败时,用确定性的类型阈值建议(workflow_hints)转一份,绝不空手。
    /// why 取 detail 的首句(如「你有 12 个视频」),依旧是据他真实文件的依据,不是空话。
    fn fallback(ov: &FileOverview) -> Vec<SuggestedFlow> {
        workflow_hints(ov)
            .into_iter()
            .map(|h| {
                let why = h
                    .detail
                    .split(['。', ',', ',', '.'])
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                SuggestedFlow {
                    title: h.title,
                    why,
                    prompt: format!(
                        "{}\n\n请基于我的知识库,先说清你打算怎么做、会用到我哪些资料,再开始。",
                        h.detail
                    ),
                }
            })
            .collect()
    }
}

/// relpath → 「最能说明在忙什么」的上级文件夹标签(取末两级目录;无目录则「(根目录)」)。
fn recent_parent_label(relpath: &str) -> String {
    let norm = relpath.replace('\\', "/");
    let segs: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() <= 1 {
        return "(根目录)".to_string();
    }
    let dirs = &segs[..segs.len() - 1]; // 去掉文件名本身
    let tail = if dirs.len() > 2 { &dirs[dirs.len() - 2..] } else { dirs };
    tail.join("/")
}

/// 秒差 → 中文相对时间(让「最近在动」一眼可读)。
fn rel_time_cn(secs: i64) -> String {
    let s = secs.max(0);
    if s < 86_400 {
        "今天".into()
    } else if s < 2 * 86_400 {
        "昨天".into()
    } else if s < 14 * 86_400 {
        format!("{}天前", s / 86_400)
    } else if s < 60 * 86_400 {
        format!("{}周前", s / (7 * 86_400))
    } else if s < 730 * 86_400 {
        format!("{}个月前", s / (30 * 86_400))
    } else {
        format!("{}年前", s / (365 * 86_400))
    }
}

/// 「最近改动的文件」按上级文件夹聚合 → 一段中文证据,喂给建议官,让收尾工作流
/// **锚定他此刻真在忙的几摊**(画像里只有主题/类型分布、没有时间线;不给这段,模型只能照主题名
/// 泛泛而谈)。拉最近改动的 ~300 个文件,按末两级目录聚合,取「最新文件夹」前 12 个,
/// 每个带:相对时间 + 近期改动数 + 几个例子文件名。失败一律返回空串(收尾页绝不能因此卡住)。
fn recent_activity_digest(root: &Option<String>) -> String {
    let Ok(conn) = open_db() else {
        return String::new();
    };
    let ids = resolve_root_ids(&conn, root);
    let filter = in_clause(&ids);
    let sql = format!(
        "SELECT f.name, f.relpath, f.mtime FROM files f
         WHERE 1=1{filter} AND f.mtime>0 ORDER BY f.mtime DESC LIMIT 300"
    );
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return String::new();
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
    }) else {
        return String::new();
    };
    let now = chrono::Local::now().timestamp();
    struct Agg {
        count: usize,
        newest: i64,
        examples: Vec<String>,
    }
    // rows 已按 mtime 倒序 → 文件夹首次出现的顺序 = 按「各自最新文件」排序,直接取前几个即可。
    let mut order: Vec<String> = Vec::new();
    let mut map: std::collections::HashMap<String, Agg> = std::collections::HashMap::new();
    for (name, relpath, mtime) in rows.flatten() {
        let dir = recent_parent_label(&relpath);
        let e = map.entry(dir.clone()).or_insert_with(|| {
            order.push(dir.clone());
            Agg { count: 0, newest: mtime, examples: Vec::new() }
        });
        e.count += 1;
        let nm = name.trim();
        if e.examples.len() < 3 && !nm.is_empty() {
            e.examples.push(nm.to_string());
        }
    }
    if order.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for dir in order.iter().take(12) {
        let a = &map[dir];
        out.push_str(&format!(
            "- {dir} — 最近{ago}动过、近期 {cnt} 个改动(如 {ex})\n",
            dir = dir,
            ago = rel_time_cn(now - a.newest),
            cnt = a.count,
            ex = a.examples.join("、"),
        ));
    }
    out
}

/// 把知识库画像(主题 / 类型 / 语言 + 最近在动的文件夹)摊成给大模型读的中文摘要,要它据此给「具体到我」的建议。
fn suggest_workflows_directive(ov: &FileOverview, recent: &str) -> String {
    let kinds: Vec<String> = ov
        .by_kind
        .iter()
        .map(|k| format!("{}×{}", pf_kind_label(&k.kind), k.count))
        .collect();
    let langs: Vec<String> = ov
        .by_lang
        .iter()
        .take(12)
        .map(|l| format!("{}×{}", l.lang, l.count))
        .collect();
    // 主题:顶层主题 + 其下子主题名,这是「智能匹配」的核心依据。
    let mut themes = String::new();
    for top in ov.clusters.iter().filter(|c| c.parent == 0) {
        let subs: Vec<&str> = ov
            .clusters
            .iter()
            .filter(|c| c.parent == top.id)
            .map(|c| c.label.as_str())
            .collect();
        if subs.is_empty() {
            themes.push_str(&format!("- {}({} 项)\n", top.label, top.size));
        } else {
            themes.push_str(&format!("- {}:{}\n", top.label, subs.join(" / ")));
        }
    }
    if themes.is_empty() {
        themes.push_str("(还没归出主题,可据类型/语言分布与抽查文件来推断)\n");
    }

    format!(
        r#"你是这个人**个人/企业知识库**的「行动建议官」。下面是他**真实的**知识库画像。
请只基于他自己的资料,提出 3~5 条「我能立刻替他做的事」——每条都必须**具体到他的主题**,
绝不能是「整理文档 / 总结资料」这种放之四海皆准、换谁都成立的套话。

知识库画像:
- 共 {total} 个文件,{bytes}
- 类型分布:{kinds}
- 语言 / 领域分布:{langs}
- AI 归出的主题:
{themes}
- 他最近在动的文件夹(按最近改动时间排序,**最能代表他此刻在忙什么**):
{recent}
你可以(可选)用 Grep / Read 抽查几个文件,让建议更贴他的真实内容(别超过 3~4 次,够用就停)。

每条建议给三个字段:
- title:6~18 字中文短语,点名他的**具体主题 + 这件事的动作**(如「给《XX 落地页》加高级入场动效」「对《YY 项目》跑高强度压测」,而不是「整理学习资料」);
- why:8~24 字中文,**点名依据**——他的哪个主题 / 文件夹 / 多少个文件让你提这条,让他一眼觉得「这是冲着我来的」
  (如「你最近在动的《预算》文件夹有 12 个改动」「你有 320 张设计稿」),**必须引用上面画像里的真实数字 / 名字**,不能空泛;
- prompt:**用户第一人称**写的、可直接发给我执行的指令。**不要怕长**——把「整个解决问题的工作流」写清楚:
  目标 → 我希望你走的步骤 → 期望产出 → 怎么算做完(验收标准),并要求我先讲计划再动手。
  点名他的具体主题 / 文件夹 / 文件名,让这条像是为他量身定的。

这类「成体系的工作流」最受欢迎,可据他的资料择优产出(只是**示例方向**,不要硬套、更不要全选):
- 给某个前端 / 落地页加一套高级动效与微交互(逐元素入场、滚动视差、悬停反馈、暗色适配);
- 把散落的多份 PRD / 需求文档归类、对齐、合并成一份结构化总览(冲突点、优先级、里程碑);
- 把一批报错 / 日志归类成「根因 → 影响面 → 修复建议」清单;
- 对某个项目做高强度压测 / 并发与 CPU 调度测试(给出测试矩阵、指标、判定阈值、跑法);
- 据他最近在忙的几摊,排一份「明日全行动计划」(按时段、依赖、优先级排好,带验收点);
- 把某摊重复劳动固化成一条可复用的标准流程 / 检查清单。

硬要求:
- **至少 1~2 条必须紧扣上面「他最近在动的文件夹」**——那是他正在干的事,要让他一眼觉得「这正是我现在要的」;
- 其余几条覆盖他**体量大 / 高频**的主题,或上面的成体系工作流,不同建议不重主题、动作各异;
- 绝不能是「整理文档 / 总结资料」这种换谁都成立的套话;全用中文。

**只输出一个 JSON 数组,不要任何额外文字、不要 markdown 代码围栏**:
[{{"title":"…","why":"…","prompt":"…"}}, ...]"#,
        total = ov.total_files,
        bytes = human_bytes(ov.total_bytes),
        kinds = if kinds.is_empty() { "—".into() } else { kinds.join("、") },
        langs = if langs.is_empty() { "—".into() } else { langs.join("、") },
        themes = themes,
        recent = if recent.trim().is_empty() { "(暂无近期改动记录)\n" } else { recent },
    )
}

/// 据真实知识库用大模型智能匹配建议(同步阻塞,数秒;由调用方放到后台线程)。
/// 任意环节失败 → 回落到确定性建议,保证永远有可用结果。
pub fn suggest_workflows(root: Option<String>) -> Result<Vec<SuggestedFlow>, String> {
    let ov = overview(root.clone())?;
    if ov.total_files == 0 {
        return Err("文件库还是空的,先「盘点」扫描磁盘文件".into());
    }
    // 时间线证据:他最近在动哪几摊(画像不含时间线,这段是建议「具体到他最近在干的」的关键)。
    let recent = recent_activity_digest(&root);
    let result = (|| -> Result<Vec<SuggestedFlow>, String> {
        let prompt = suggest_workflows_directive(&ov, &recent);
        // 配了独立归类模型 → 直连它(省钱);否则用聊天那个大模型(可 Read/Grep 抽查真文件)。
        let collected = if let Some(cfg) = active_cluster_model() {
            chat_complete(&cfg, &prompt)?
        } else {
            let kb_root = PathBuf::from(crate::kb::kb_root());
            let cwd = if kb_root.exists() { kb_root } else { std::env::temp_dir() };
            crate::kb::run_claude_readonly(&cwd, &prompt, |_k, _t| {})?
        };
        let raw = crate::kb::extract_balanced_json(&collected)
            .ok_or("模型没有返回可解析的 JSON")?;
        let flows: Vec<SuggestedFlow> =
            serde_json::from_str(&raw).map_err(|e| format!("建议 JSON 解析失败: {e}"))?;
        let flows: Vec<SuggestedFlow> = flows
            .into_iter()
            .filter(|f| !f.title.trim().is_empty() && !f.prompt.trim().is_empty())
            .take(6)
            .collect();
        if flows.is_empty() {
            return Err("模型返回了空建议".into());
        }
        Ok(flows)
    })();
    // LLM 路径任何失败都安静回落,不把错误抛给向导收尾页(那一步必须永远有卡片可点)。
    Ok(result.unwrap_or_else(|_| SuggestedFlow::fallback(&ov)))
}

/// 生成「让 AI 更懂你」自包含 HTML → 桌面,返回文件路径。
fn profile_html(root: Option<String>) -> Result<String, String> {
    let ov = overview(root)?;
    if ov.total_files == 0 {
        return Err("文件库还是空的,先「盘点」扫描磁盘文件再生成画像".into());
    }
    let now = chrono::Local::now();
    let stamp = now.format("%Y%m%d-%H%M%S").to_string();
    let human = now.format("%Y-%m-%d %H:%M").to_string();

    // 类型分布条
    let max_count = ov.by_kind.iter().map(|k| k.count).max().unwrap_or(1).max(1);
    let mut kinds = String::new();
    for k in &ov.by_kind {
        let w = (k.count as f64 / max_count as f64 * 100.0).max(3.0);
        kinds.push_str(&format!(
            r#"<div class="krow"><span class="kl"><span class="kdot" style="background:{c}"></span>{lab}</span><span class="kbar"><span class="kfill" style="width:{w:.1}%;background:{c}"></span></span><span class="kn">{n}</span><span class="kb">{b}</span></div>"#,
            c = kind_color(&k.kind),
            lab = esc(pf_kind_label(&k.kind)),
            w = w,
            n = k.count,
            b = esc(&human_bytes(k.bytes)),
        ));
    }

    // 语义主题(顶层主题,按 size 已倒序)
    let mut themes = String::new();
    for c in ov.clusters.iter().filter(|c| c.parent == 0).take(24) {
        themes.push_str(&format!(
            r#"<span class="theme" style="--c:{c}"><span class="tdot"></span>{lab}<span class="tn">{n}</span></span>"#,
            c = esc(&c.color),
            lab = esc(&c.label),
            n = c.size,
        ));
    }
    if themes.is_empty() {
        themes.push_str(r#"<span class="theme dim">还没归主题 —— 在文件中心点「智能归类」即可</span>"#);
    }

    let mut understanding = String::new();
    for l in understanding_lines(&ov) {
        understanding.push_str(&format!("<li>{}</li>", esc(&l)));
    }
    let mut flows = String::new();
    for w in workflow_hints(&ov) {
        flows.push_str(&format!(
            r#"<div class="flow"><div class="ft">{t}</div><div class="fd">{d}</div></div>"#,
            t = esc(&w.title),
            d = esc(&w.detail),
        ));
    }

    let html = format!(
        r##"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>让 AI 更懂你 · 你的知识画像</title>
<style>
:root{{--bg:#0f1115;--panel:#171a21;--line:#252a33;--ink:#e8e8e6;--mut:#9aa0ab;--gold:#d4b06a}}
*{{box-sizing:border-box}}
body{{margin:0;background:radial-gradient(140% 100% at 50% 0%,#171b24,#0f1115);color:var(--ink);
font-family:-apple-system,"Segoe UI","PingFang SC","Microsoft YaHei",sans-serif;line-height:1.65}}
.wrap{{max-width:980px;margin:0 auto;padding:56px 32px 110px}}
.eyebrow{{color:var(--gold);font-size:12px;letter-spacing:3px;text-transform:uppercase}}
h1{{font-size:30px;margin:8px 0 6px;letter-spacing:.5px}}
.sub{{color:var(--mut);font-size:13px}}
.stats{{display:flex;flex-wrap:wrap;gap:26px;margin:26px 0 30px;padding:20px 24px;background:var(--panel);
border:1px solid var(--line);border-radius:16px}}
.stat .v{{font-size:26px;font-weight:680;font-variant-numeric:tabular-nums}}.stat .l{{color:var(--mut);font-size:12px}}
.card{{background:var(--panel);border:1px solid var(--line);border-radius:16px;padding:22px 24px;margin:16px 0}}
.card h2{{font-size:15px;margin:0 0 14px;display:flex;align-items:center;gap:8px}}
.card h2::before{{content:"";width:8px;height:8px;border-radius:50%;background:var(--gold);box-shadow:0 0 8px var(--gold)}}
.understand{{list-style:none;margin:0;padding:0}}
.understand li{{padding:7px 0 7px 22px;position:relative;color:var(--ink);font-size:14px}}
.understand li::before{{content:"›";position:absolute;left:4px;color:var(--gold)}}
.krow{{display:grid;grid-template-columns:78px 1fr auto auto;align-items:center;gap:12px;padding:5px 0;font-size:13px}}
.kl{{display:flex;align-items:center;gap:7px;color:var(--ink)}}
.kdot{{width:8px;height:8px;border-radius:50%}}
.kbar{{height:8px;background:rgba(255,255,255,.05);border-radius:99px;overflow:hidden}}
.kfill{{display:block;height:100%;border-radius:99px}}
.kn{{color:var(--ink);font-variant-numeric:tabular-nums;min-width:48px;text-align:right}}
.kb{{color:var(--mut);font-size:11.5px;min-width:64px;text-align:right}}
.themes{{display:flex;flex-wrap:wrap;gap:8px}}
.theme{{--c:#8b6cff;display:inline-flex;align-items:center;gap:7px;padding:5px 12px;font-size:12.5px;
background:color-mix(in srgb,var(--c) 14%,transparent);border:1px solid color-mix(in srgb,var(--c) 32%,transparent);
border-radius:99px}}
.theme.dim{{color:var(--mut);background:none;border-color:var(--line)}}
.tdot{{width:7px;height:7px;border-radius:50%;background:var(--c);box-shadow:0 0 7px var(--c)}}
.tn{{color:var(--mut);font-size:11px}}
.flows{{display:grid;grid-template-columns:repeat(auto-fill,minmax(280px,1fr));gap:14px}}
.flow{{padding:16px 18px;background:rgba(255,255,255,.02);border:1px solid var(--line);border-radius:14px}}
.ft{{font-size:14px;font-weight:620;margin-bottom:6px}}
.fd{{color:var(--mut);font-size:12.5px}}
.foot{{margin-top:44px;color:#5a606b;font-size:12px;text-align:center}}
</style></head><body><div class="wrap">
<div class="eyebrow">Polaris · 知识画像</div>
<h1>让 AI 更懂你</h1>
<div class="sub">基于本机盘点结果生成 · {human} · 完全离线,内容不出本机</div>
<div class="stats">
<div class="stat"><div class="v">{tf}</div><div class="l">个文件</div></div>
<div class="stat"><div class="v">{tb}</div><div class="l">总体量</div></div>
<div class="stat"><div class="v">{nk}</div><div class="l">种类型</div></div>
<div class="stat"><div class="v">{nt}</div><div class="l">个主题</div></div>
</div>
<div class="card"><h2>AI 对你的理解</h2><ul class="understand">{understand}</ul></div>
<div class="card"><h2>你的文件构成</h2>{kinds}</div>
<div class="card"><h2>你关心的主题</h2><div class="themes">{themes}</div></div>
<div class="card"><h2>我能立刻替你做的事</h2><div class="flows">{flows}</div></div>
<div class="foot">Polaris 文件中心 · 据 fable.db 统计确定性生成,不调用大模型 · 想深入就回到对话里直接问我</div>
</div></body></html>"##,
        human = esc(&human),
        tf = ov.total_files,
        tb = esc(&human_bytes(ov.total_bytes)),
        nk = ov.by_kind.len(),
        nt = ov.clusters.iter().filter(|c| c.parent == 0).count(),
        understand = understanding,
        kinds = kinds,
        themes = themes,
        flows = flows,
    );

    let desktop = directories::UserDirs::new()
        .and_then(|u| u.desktop_dir().map(|d| d.to_path_buf()))
        .or_else(|| directories::UserDirs::new().map(|u| u.home_dir().to_path_buf()))
        .ok_or("找不到桌面目录")?;
    let path = desktop.join(format!("让AI更懂你-知识画像-{stamp}.html"));
    std::fs::write(&path, html).map_err(|e| format!("写画像失败: {e}"))?;
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

// 文件中心几个「读大库 / 读盘上文件(可能是慢 SMB 的 NAS 盘)」的命令:桌面端一律 async +
// spawn_blocking,把重活挪离 Tauri 主线程,绝不冻 WebView 消息泵(否则大库 GROUP BY 或 NAS
// 一抖,主线程阻塞 >5s 就被 Windows 判「无响应」强杀)。server flavor 无 UI 主线程可冻、且
// dispatch_sync 本就在 spawn_blocking 中,保持同步直调内层即可。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn file_overview(root: Option<String>) -> Result<FileOverview, String> {
    tauri::async_runtime::spawn_blocking(move || overview(root))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
pub fn file_overview(root: Option<String>) -> Result<FileOverview, String> {
    overview(root)
}

#[cfg(feature = "desktop")]
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn file_grid(
    root: Option<String>,
    cluster_id: Option<i64>,
    kind: Option<String>,
    lang: Option<String>,
    sort: Option<String>,
    query: Option<String>,
    page: Option<usize>,
    page_size: Option<usize>,
) -> Result<FileGridPage, String> {
    tauri::async_runtime::spawn_blocking(move || {
        grid(
            root,
            cluster_id,
            kind,
            lang,
            sort,
            query,
            page.unwrap_or(0),
            page_size.unwrap_or(60),
        )
    })
    .await
    .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
#[allow(clippy::too_many_arguments)]
pub fn file_grid(
    root: Option<String>,
    cluster_id: Option<i64>,
    kind: Option<String>,
    lang: Option<String>,
    sort: Option<String>,
    query: Option<String>,
    page: Option<usize>,
    page_size: Option<usize>,
) -> Result<FileGridPage, String> {
    grid(
        root,
        cluster_id,
        kind,
        lang,
        sort,
        query,
        page.unwrap_or(0),
        page_size.unwrap_or(60),
    )
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn file_thumb(abspath: String, max: Option<u32>) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || thumb(abspath, max.unwrap_or(360)))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
pub fn file_thumb(abspath: String, max: Option<u32>) -> Result<Option<String>, String> {
    thumb(abspath, max.unwrap_or(360))
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn file_gist(abspath: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || gist(abspath))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
pub fn file_gist(abspath: String) -> Result<String, String> {
    gist(abspath)
}

/// 归类(纯数学聚类)进行中闸 —— 防双发,panic 栈展开也释放(见 [`FlagGuard`])。
static CLUSTERING: AtomicBool = AtomicBool::new(false);

fn emit_cluster(app: &AppHandle, payload: Value) {
    let _ = app.emit("file:cluster", payload);
}

/// 文件中心 v3 渐进式智能归类进行中闸(独立于 CLUSTERING/LLM_CLUSTERING,防双发)。
static SMART_CLUSTERING: AtomicBool = AtomicBool::new(false);

/// 文件中心 v3 渐进式智能归类:后台一个线程顺序推进三档,全程发 `file:cluster` 事件。
///  - **T0 骨架**:`cluster_build(Lexical)` 秒级、零嵌入,先把全库分簇 → `tier=skeleton`;
///  - **T1 初级**:`cluster_rename_llm` 让大模型读簇画像起亲切名 + 关系 → `tier=ai-primary`;
///  - **T2 精修**:配了嵌入服务商时,`build_index_full` 全量向量化 → `cluster_build(Semantic)`
///    语义重聚 → `cluster_rename_llm` 再命名 → `tier=semantic`(用户要的「向量化完后再归一次」)。
///
/// 每档完成 emit `{kind:"tier", tier, note}` 让前端原地刷新星图;全部结束 emit `kind:"done"`。
/// LLM 档失败可降级:保留 `cluster_build` 的启发式名、继续后续档,绝不卡住。
/// `deep=false`(快速档)只跑 T0 词法全覆盖 + T1 AI 命名就收尾——几秒归全库 + 一次 AI 调用,
/// 远低于 2 分钟,**给新用户向导用**(向导自己在收尾另起后台建索引,这里再触发 T2 全量向量化
/// 会与之冲突、且大库要几十分钟爆掉「2 分钟」预期)。`deep=true`(文件中心按钮)才追加 T2。
fn smart_cluster_progressive(app: &AppHandle, root: Option<String>, deep: bool) -> Result<(), String> {
    // ── T0:结构骨架(秒级,零嵌入)──
    emit_cluster(
        app,
        json!({ "kind": "phase", "tier": "skeleton", "text": "正在快速归类(按结构)…几秒就好" }),
    );
    let s0 = cluster_build_mode(root.clone(), ClusterMode::Lexical)?;
    emit_cluster(
        app,
        json!({
            "kind": "tier", "tier": "skeleton", "clusters": s0.clusters, "files": s0.files,
            "note": format!("已把 {} 个文件快速归成 {} 簇,正在请 AI 起名…", s0.files, s0.clusters),
        }),
    );

    // ── T1:AI 初级命名 + 关系(读簇画像,不读文件,成本与文件数无关)──
    let mut report = String::new();
    match cluster_rename_llm(app, root.clone(), "ai-primary") {
        Ok((renamed, edges, rep)) => {
            report = rep.clone();
            emit_cluster(
                app,
                json!({
                    "kind": "tier", "tier": "ai-primary", "renamed": renamed, "edges": edges, "report": rep,
                    "note": format!("AI 已读懂并命名 {renamed} 个主题、理出 {edges} 条关系"),
                }),
            );
        }
        Err(e) => {
            // 起名失败不致命:骨架名仍在,提示后继续。
            emit_cluster(
                app,
                json!({
                    "kind": "tier", "tier": "ai-primary",
                    "note": format!("AI 命名暂不可用({e}),已先按结构归好;稍后可重试"),
                }),
            );
        }
    }

    // ── T2:全量向量化 → 语义重聚 → 再命名(配了嵌入能力时;全程后台)──
    // 「嵌入能力」= 云 API 服务商 **或** 本地开源嵌入(local-embed,离线就能产向量);
    // 后者此前不被计入 → 纯本地用户永远停在结构归类、走不到「按内容语义」这一档。见 embed_capable。
    if deep && super::index::embed_capable() {
        emit_cluster(
            app,
            json!({ "kind": "phase", "tier": "semantic", "text": "后台精修:正在把全部资料向量化(可关页面去忙别的)…" }),
        );
        let app_idx = app.clone();
        let idx = super::index::build_index_full(&move |files, _chunks, pending| {
            emit_cluster(
                &app_idx,
                json!({
                    "kind": "phase", "tier": "semantic",
                    "text": format!("后台精修:已向量化 {files} 个文件{}",
                        if pending > 0 { format!(",还剩约 {pending} 个") } else { String::new() }),
                }),
            );
        });
        match idx {
            Ok(_) => {
                emit_cluster(
                    app,
                    json!({ "kind": "phase", "tier": "semantic", "text": "向量化完成,正在按内容语义重新归类…" }),
                );
                let s2 = cluster_build_mode(root.clone(), ClusterMode::Semantic)?;
                let rep2 = match cluster_rename_llm(app, root.clone(), "semantic") {
                    Ok((_, _, rep)) => rep,
                    Err(_) => report.clone(),
                };
                emit_cluster(
                    app,
                    json!({
                        "kind": "tier", "tier": "semantic", "clusters": s2.clusters, "files": s2.files, "report": rep2,
                        "note": format!("已按内容语义把 {} 个文件精修归成 {} 簇", s2.files, s2.clusters),
                    }),
                );
                report = rep2;
            }
            Err(e) => {
                emit_cluster(
                    app,
                    json!({ "kind": "phase", "tier": "semantic", "text": format!("后台向量化未完成:{e}(已保留 AI 初级归类)") }),
                );
            }
        }
    }

    emit_cluster(app, json!({ "kind": "done", "report": report, "note": "智能归类完成" }));
    Ok(())
}

/// 重建语义/结构聚类(复用已存向量,纯数学,不调嵌入 API)。**后台线程跑**,进度走
/// `file:cluster` 事件(phase/done/error)—— 切走文件中心也不中断,回来仍见结果。
///
/// 改自旧同步命令:上千文件时均值池化(逐 chunk 反序列化)+ 球面 k-means(16 轮 Lloyd)
/// 是 0.1–0.5s 的纯 CPU 阻塞,放在 Tauri 同步命令里会冻结 WebView 主线程。挪到后台线程后
/// 界面全程可点,与 [`file_cluster_llm`] / [`fable_inventory_start`] 同构。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_cluster_build(app: AppHandle, root: Option<String>) -> Result<(), String> {
    let Some(guard) = FlagGuard::acquire(&CLUSTERING) else {
        return Err("归类正在进行中".into());
    };
    emit_cluster(&app, json!({ "kind": "phase", "text": "正在把相似文件归类…" }));
    std::thread::spawn(move || {
        let _guard = guard; // panic 栈展开也释放闸,防永久锁死
        match cluster_build(root) {
            Ok(s) => emit_cluster(
                &app,
                json!({
                    "kind": "done", "clusters": s.clusters, "files": s.files,
                    "seconds": s.seconds, "note": s.note,
                }),
            ),
            Err(e) => emit_cluster(&app, json!({ "kind": "error", "message": e })),
        }
    });
    Ok(())
}

/// 文件中心 v3 渐进式智能归类(秒级骨架 → AI 初级命名+关系 → 全量向量化后语义重聚再命名)。
/// 后台线程跑,进度/各档完成走 `file:cluster` 事件(phase / tick / tier / done / error);
/// 切走文件中心也不中断,与 [`file_cluster_build`] / [`file_cluster_llm`] 同构。
///
/// `quick=Some(true)`:只跑 T0+T1(全覆盖词法 + AI 命名)就收尾,不追加耗时的 T2 全量向量化
/// —— 新用户向导用(几秒 + 一次 AI 调用,远低于 2 分钟);其余(含文件中心按钮)默认深档跑全程。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_smart_cluster(app: AppHandle, root: Option<String>, quick: Option<bool>) -> Result<(), String> {
    let Some(guard) = FlagGuard::acquire(&SMART_CLUSTERING) else {
        return Err("智能归类正在进行中".into());
    };
    let deep = !quick.unwrap_or(false);
    emit_cluster(&app, json!({ "kind": "phase", "tier": "skeleton", "text": "正在启动智能归类…" }));
    std::thread::spawn(move || {
        let _guard = guard; // panic 栈展开也释放闸,防永久锁死
        if let Err(e) = smart_cluster_progressive(&app, root, deep) {
            emit_cluster(&app, json!({ "kind": "error", "message": e }));
        }
    });
    Ok(())
}

/// 「让 AI 更懂你」:据盘点统计确定性生成知识画像 HTML → 桌面,返回文件路径(同步;不调大模型)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_profile_html(root: Option<String>) -> Result<String, String> {
    profile_html(root)
}

/// 智能向导收尾「建议工作流」:大模型据**真实知识库**智能匹配,而非固定阈值套话。
/// 桌面端为 async + spawn_blocking,避免数秒的大模型调用冻结主线程 WebView;
/// server flavor 由 dispatch_sync 直接调内层 [`suggest_workflows`](已在 spawn_blocking 中)。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn file_suggest_workflows(root: Option<String>) -> Result<Vec<SuggestedFlow>, String> {
    tauri::async_runtime::spawn_blocking(move || suggest_workflows(root))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}

/// 文件中心「星图」:语义簇 + 抽样文件 → 与知识图谱同构的 KbGraph(供 KnowledgeGraph.vue 星河渲染)。
/// 桌面端 async + spawn_blocking,大库建图不冻 UI 主线程(理由同 [`file_overview`])。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn file_graph(root: Option<String>) -> Result<crate::kb::KbGraph, String> {
    tauri::async_runtime::spawn_blocking(move || build_file_graph(root))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
pub fn file_graph(root: Option<String>) -> Result<crate::kb::KbGraph, String> {
    build_file_graph(root)
}

/// 缩略图预取:批量解码盘上图片(可能是慢 NAS 盘),桌面端 async + spawn_blocking 不冻 UI。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn file_warm_thumbs(paths: Vec<String>, max: Option<u32>) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || warm_thumbs(paths, max.unwrap_or(360)))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))
}
#[cfg(not(feature = "desktop"))]
pub fn file_warm_thumbs(paths: Vec<String>, max: Option<u32>) -> Result<usize, String> {
    Ok(warm_thumbs(paths, max.unwrap_or(360)))
}

/// 用已连接的大模型按语义归类(免嵌入 key)。
/// 后台线程跑,进度走 `file:cluster_llm` 事件(phase/tick/done/error)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_cluster_llm(app: AppHandle, root: Option<String>) -> Result<(), String> {
    let Some(guard) = FlagGuard::acquire(&LLM_CLUSTERING) else {
        return Err("AI 归类正在进行中".into());
    };
    std::thread::spawn(move || {
        let _guard = guard; // panic 栈展开也释放闸,防永久锁死
        let res = cluster_llm_run(&app, root);
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
    let Some(guard) = FlagGuard::acquire(&LLM_TITLING) else {
        return Err("AI 命名正在进行中".into());
    };
    std::thread::spawn(move || {
        let _guard = guard; // panic 栈展开也释放闸,防永久锁死
        let res = titles_llm_run(&app, root);
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

    // ── 文件中心 v3 渐进式归类 ──

    #[test]
    fn loose_i64_accepts_num_and_str() {
        assert_eq!(loose_i64(&Value::from(5i64)), Some(5));
        assert_eq!(loose_i64(&Value::from("7")), Some(7));
        assert_eq!(loose_i64(&Value::from(" 9 ")), Some(9));
        assert_eq!(loose_i64(&Value::from("x")), None);
        assert_eq!(loose_i64(&Value::Null), None);
    }

    #[test]
    fn name_rel_json_parses_mixed_id_types() {
        // 模型可能把 id 写成数字或字符串,两种都要吃下。
        let raw = r#"{"names":[{"id":1,"name":"我的报税","summary":"2023 报税材料"},
                                {"id":"2","name":"装修"}],
                      "relations":[{"from":1,"to":2,"label":"配套"}]}"#;
        let p: LlmNameRel = serde_json::from_str(raw).unwrap();
        assert_eq!(p.names.len(), 2);
        assert_eq!(p.relations.len(), 1);
        assert_eq!(loose_i64(&p.names[1].id), Some(2));
    }

    #[test]
    fn rename_apply_validates_and_writes() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE clusters(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 0,
                label TEXT NOT NULL DEFAULT '', color TEXT NOT NULL DEFAULT '', keywords TEXT NOT NULL DEFAULT '',
                size INTEGER NOT NULL DEFAULT 0, built_at INTEGER NOT NULL DEFAULT 0, parent INTEGER NOT NULL DEFAULT 0,
                summary TEXT NOT NULL DEFAULT '');
             CREATE TABLE cluster_edges(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 0,
                src INTEGER NOT NULL, dst INTEGER NOT NULL, label TEXT NOT NULL DEFAULT '', built_at INTEGER NOT NULL DEFAULT 0);
             INSERT INTO clusters(id,label) VALUES(1,'簇A'),(2,'簇B');",
        )
        .unwrap();
        // id=99 越界、from=3 越界、2→2 自环 —— 都必须被挡掉。
        let raw = r#"{"names":[{"id":1,"name":"我的报税","summary":"报税材料都在这"},
                                {"id":99,"name":"越界忽略"}],
                      "relations":[{"from":1,"to":2,"label":"同源"},
                                   {"from":2,"to":2,"label":"自环丢"},
                                   {"from":3,"to":1,"label":"越界丢"}]}"#;
        let parsed: LlmNameRel = serde_json::from_str(raw).unwrap();
        let valid: std::collections::HashSet<i64> = [1i64, 2].into_iter().collect();
        let mut croot = HashMap::new();
        croot.insert(1i64, 0i64);
        croot.insert(2i64, 0i64);
        let (renamed, edges, _merged) =
            apply_names_and_relations(&conn, &[], &parsed, &valid, &croot, 123).unwrap();
        assert_eq!(renamed, 1, "只有 id=1 在范围内且有名字");
        assert_eq!(edges, 1, "只有 1→2 合法(自环 / 越界被丢)");
        let label: String =
            conn.query_row("SELECT label FROM clusters WHERE id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(label, "我的报税");
        let summary: String =
            conn.query_row("SELECT summary FROM clusters WHERE id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(summary, "报税材料都在这");
        // id=2 未被命名 → 保留原名。
        let label2: String =
            conn.query_row("SELECT label FROM clusters WHERE id=2", [], |r| r.get(0)).unwrap();
        assert_eq!(label2, "簇B");
        let (s, d, l): (i64, i64, String) = conn
            .query_row("SELECT src,dst,label FROM cluster_edges", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!((s, d, l), (1, 2, "同源".to_string()));
    }

    #[test]
    fn rename_apply_is_idempotent_rebuild() {
        // 二次 apply 应清掉旧关系边再重建,不累积。
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE clusters(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 0,
                label TEXT NOT NULL DEFAULT '', color TEXT NOT NULL DEFAULT '', keywords TEXT NOT NULL DEFAULT '',
                size INTEGER NOT NULL DEFAULT 0, built_at INTEGER NOT NULL DEFAULT 0, parent INTEGER NOT NULL DEFAULT 0,
                summary TEXT NOT NULL DEFAULT '');
             CREATE TABLE cluster_edges(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 0,
                src INTEGER NOT NULL, dst INTEGER NOT NULL, label TEXT NOT NULL DEFAULT '', built_at INTEGER NOT NULL DEFAULT 0);
             INSERT INTO clusters(id,label) VALUES(1,'A'),(2,'B');",
        )
        .unwrap();
        let raw = r#"{"names":[],"relations":[{"from":1,"to":2,"label":"同源"}]}"#;
        let parsed: LlmNameRel = serde_json::from_str(raw).unwrap();
        let valid: std::collections::HashSet<i64> = [1i64, 2].into_iter().collect();
        let croot: HashMap<i64, i64> = [(1i64, 0i64), (2, 0)].into_iter().collect();
        apply_names_and_relations(&conn, &[], &parsed, &valid, &croot, 1).unwrap();
        apply_names_and_relations(&conn, &[], &parsed, &valid, &croot, 2).unwrap();
        let n: i64 =
            conn.query_row("SELECT COUNT(*) FROM cluster_edges", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "重跑不累积关系边");
    }

    #[test]
    fn merge_consolidates_same_parent_leaves_and_remaps() {
        // 按意思合并:同父叶簇 {1,2,3} 并成最大簇(1),文件改挂、余簇删除、size 重算;
        // 跨父组 [1,4] 被拒;指向被并簇的命名/关系自动重映射到 survivor。
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE clusters(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 0,
                label TEXT NOT NULL DEFAULT '', color TEXT NOT NULL DEFAULT '', keywords TEXT NOT NULL DEFAULT '',
                size INTEGER NOT NULL DEFAULT 0, built_at INTEGER NOT NULL DEFAULT 0, parent INTEGER NOT NULL DEFAULT 0,
                summary TEXT NOT NULL DEFAULT '');
             CREATE TABLE cluster_edges(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 0,
                src INTEGER NOT NULL, dst INTEGER NOT NULL, label TEXT NOT NULL DEFAULT '', built_at INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE files(id INTEGER PRIMARY KEY, cluster_id INTEGER NOT NULL DEFAULT 0);
             INSERT INTO clusters(id,parent,size,label) VALUES
                (10,0,6,'大主题'),(1,10,3,'发票'),(2,10,2,'invoices'),(3,10,1,'报销单'),(4,99,5,'别的');
             INSERT INTO files(id,cluster_id) VALUES
                (101,1),(102,1),(103,1),(201,2),(202,2),(301,3);",
        )
        .unwrap();
        // names 指向被并簇 id=2 → 应改到 survivor 1;relation 2→4 → 应映射成 1→4。
        let raw = r#"{"names":[{"id":2,"name":"发票报销","summary":"发票和报销单都在这"},
                                {"id":4,"name":"其它东西"}],
                      "merges":[[1,2,3],[1,4]],
                      "relations":[{"from":2,"to":4,"label":"配套"}]}"#;
        let parsed: LlmNameRel = serde_json::from_str(raw).unwrap();
        let valid: std::collections::HashSet<i64> = [10i64, 1, 2, 3, 4].into_iter().collect();
        let croot: HashMap<i64, i64> = [(10i64, 0), (1, 0), (2, 0), (3, 0), (4, 0)].into_iter().collect();
        let (renamed, edges, merged) =
            apply_names_and_relations(&conn, &[], &parsed, &valid, &croot, 1).unwrap();
        assert_eq!(merged, 2, "1/2/3 同父并入 survivor=1,合并掉 2 个");
        // survivor=1(最大簇)留下,2/3 删除。
        let gone: i64 = conn
            .query_row("SELECT COUNT(*) FROM clusters WHERE id IN (2,3)", [], |r| r.get(0))
            .unwrap();
        assert_eq!(gone, 0, "被并簇 2/3 已删");
        // survivor 吸收全部 6 个文件,size 重算为 6。
        let nf: i64 =
            conn.query_row("SELECT COUNT(*) FROM files WHERE cluster_id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(nf, 6, "原 1/2/3 的文件全改挂 survivor=1");
        let sz: i64 =
            conn.query_row("SELECT size FROM clusters WHERE id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(sz, 6, "survivor size 重算 = 实际文件数");
        // 跨父组 [1,4] 被拒 → 4 仍在。
        let kept: i64 =
            conn.query_row("SELECT COUNT(*) FROM clusters WHERE id=4", [], |r| r.get(0)).unwrap();
        assert_eq!(kept, 1, "跨父合并被拒,簇 4 保留");
        // 命名重映射:id=2 → survivor 1,故 1 被命名「发票报销」。
        let label1: String =
            conn.query_row("SELECT label FROM clusters WHERE id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(label1, "发票报销");
        assert_eq!(renamed, 2, "id=2(→1) 与 id=4 各命名一次");
        // 关系重映射:2→4 变 1→4。
        assert_eq!(edges, 1);
        let (s, d): (i64, i64) = conn
            .query_row("SELECT src,dst FROM cluster_edges", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((s, d), (1, 4), "关系端点跟随合并重映射");
    }

    #[test]
    fn merge_skips_parent_clusters() {
        // 父簇(有子簇者)绝不可被并 —— 否则孤立其子簇。merges 里含父簇 id 的组应被安全跳过。
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE clusters(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 0,
                label TEXT NOT NULL DEFAULT '', color TEXT NOT NULL DEFAULT '', keywords TEXT NOT NULL DEFAULT '',
                size INTEGER NOT NULL DEFAULT 0, built_at INTEGER NOT NULL DEFAULT 0, parent INTEGER NOT NULL DEFAULT 0,
                summary TEXT NOT NULL DEFAULT '');
             CREATE TABLE cluster_edges(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 0,
                src INTEGER NOT NULL, dst INTEGER NOT NULL, label TEXT NOT NULL DEFAULT '', built_at INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE files(id INTEGER PRIMARY KEY, cluster_id INTEGER NOT NULL DEFAULT 0);
             INSERT INTO clusters(id,parent,size) VALUES (10,0,1),(20,0,1),(1,10,1),(2,20,1);",
        )
        .unwrap();
        // 两个顶层父簇 10、20 都各有子簇 → 都在 parent_set,合并 [10,20] 必须被拒。
        let raw = r#"{"merges":[[10,20]],"names":[],"relations":[]}"#;
        let parsed: LlmNameRel = serde_json::from_str(raw).unwrap();
        let valid: std::collections::HashSet<i64> = [10i64, 20, 1, 2].into_iter().collect();
        let croot: HashMap<i64, i64> = HashMap::new();
        let (_r, _e, merged) =
            apply_names_and_relations(&conn, &[], &parsed, &valid, &croot, 1).unwrap();
        assert_eq!(merged, 0, "父簇不参与合并");
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM clusters", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 4, "无簇被删");
    }

    #[test]
    fn digest_directive_includes_ids_and_samples() {
        let d = ClusterDigest {
            id: 7,
            parent: 0,
            label: "财务".into(),
            keywords: "发票 报税".into(),
            size: 12,
            folders: vec!["财务/2023".into()],
            samples: vec!["增值税申报表".into()],
            kinds: vec![("doc".into(), 10)],
        };
        let s = digest_directive(&[d]);
        assert!(s.contains("id=7"));
        assert!(s.contains("增值税申报表"));
        assert!(s.contains("发票 报税"));
        assert!(s.contains("文档×10"));
    }

    // ───────────────────────── 聚类准确度评测台(真大模型介入) ─────────────────────────
    //
    // 目标:量化「几分钟路径」(T0 词法骨架 + T1 大模型命名)在**贴近真人杂乱硬盘**的语料上的
    //   ① 覆盖率(是否真把全部文件都归了,不再 240/6000 截断)
    //   ② 聚类纯度(同主题文件是否落进同一簇)
    //   ③ 命名准确度(AI 簇名是否命中该簇主导主题 + 是否亲切中文)
    //   ④ 关系边数量
    // 隔离:用临时 db,绝不碰用户真实 ~/Polaris/data/fable.db。
    // 触发:仅当置 POLARIS_CLUSTER_EVAL=1 才跑(普通 cargo test 跳过);真大模型走 run_claude_readonly
    //   (置 EVAL_NO_LLM=1 则只测 T0,不调模型)。结果按 EVAL_OUT 追加一行 JSON。

    fn env_u64(k: &str, d: u64) -> u64 {
        std::env::var(k).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(d)
    }
    // 可复现 LCG(避免 rand 依赖,种子可控)。
    fn lcg(s: &mut u64) -> u64 {
        *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *s >> 33
    }

    struct EvalTopic {
        id: usize,
        folder: &'static str,
        ext: &'static str,
        kind: &'static str,
        stems: &'static [&'static str],
        aliases: &'static [&'static str],
    }
    fn eval_topics() -> Vec<EvalTopic> {
        vec![
            EvalTopic { id: 0, folder: "财务/报税", ext: "xlsx", kind: "doc",
                stems: &["增值税申报表", "个税专项扣除", "年度利润表", "记账凭证", "京东发票", "报销单", "工资表", "对账单"],
                aliases: &["报税", "财务", "税", "发票", "报销", "账", "工资", "对账", "凭证", "利润", "扣除", "报表", "单据"] },
            EvalTopic { id: 1, folder: "装修/新房", ext: "jpg", kind: "image",
                stems: &["客厅效果图", "水电改造预算", "家具清单", "施工合同", "瓷砖选样", "全屋定制报价", "卫生间布局"],
                aliases: &["装修", "房", "家具", "施工", "效果图", "户型", "布局", "选材", "报价", "预算"] },
            EvalTopic { id: 2, folder: "考研/复习", ext: "pdf", kind: "doc",
                stems: &["数学强化讲义", "英语真题2022", "政治大纲笔记", "专业课总结", "错题本", "肖四肖八", "高数公式"],
                aliases: &["考研", "复习", "真题", "笔记", "讲义", "学习", "数学", "英语", "政治", "错题", "公式"] },
            EvalTopic { id: 3, folder: "照片/宝宝", ext: "jpg", kind: "image",
                stems: &["周岁照", "幼儿园运动会", "全家福", "第一次走路", "生日蛋糕", "公园游玩"],
                aliases: &["照片", "宝宝", "孩子", "娃", "家庭", "全家福"] },
            EvalTopic { id: 4, folder: "工作/汇报", ext: "pptx", kind: "doc",
                stems: &["季度汇报", "周报", "项目方案v3", "OKR复盘", "需求评审纪要", "述职报告"],
                aliases: &["工作", "汇报", "项目", "报告", "周报", "方案", "复盘", "述职", "纪要", "评审"] },
            EvalTopic { id: 5, folder: "副业/接单", ext: "psd", kind: "image",
                stems: &["logo设计稿", "客户需求", "报价单", "海报终稿", "名片排版", "公众号配图"],
                aliases: &["副业", "接单", "客户", "设计", "海报", "logo", "名片", "排版", "配图"] },
            EvalTopic { id: 6, folder: "旅行/日本", ext: "pdf", kind: "doc",
                stems: &["行程单", "机票确认", "东京攻略", "酒店预订", "签证材料", "美食清单"],
                aliases: &["旅行", "旅游", "行程", "攻略", "机票", "日本", "酒店", "住宿", "签证", "美食", "东京"] },
            EvalTopic { id: 7, folder: "code/polaris", ext: "rs", kind: "text",
                stems: &["main", "lib", "server", "README", "cluster_build", "retrieve"],
                aliases: &["代码", "项目", "开发", "程序", "code", "源码"] },
            EvalTopic { id: 8, folder: "movies", ext: "mkv", kind: "video",
                stems: &["复仇者联盟", "星际穿越", "盗梦空间", "教父", "肖申克的救赎"],
                aliases: &["电影", "影视", "视频", "剧", "movie", "大片", "片", "科幻", "经典", "动作"] },
            EvalTopic { id: 9, folder: "合同", ext: "docx", kind: "doc",
                stems: &["租房合同", "劳动合同", "保密协议", "采购合同", "服务协议"],
                aliases: &["合同", "协议", "法律", "租房", "劳动"] },
        ]
    }

    struct EvalFile {
        relpath: String,
        name: String,
        ext: String,
        kind: String,
        size: i64,
        mtime: i64,
        topic: usize,
    }

    // 按场景生成贴近真人硬盘的语料 + 真值主题标签。
    //  organized = 按主题文件夹整齐摆放(文件夹信号强);flat = 全堆根目录(只靠文件名);
    //  messy = 混合 + ~15% 乱名噪声(IMG_/微信图片/副本);multiling = 含英文命名主题。
    fn gen_corpus(scenario: &str, seed: u64, size: usize) -> (Vec<EvalFile>, Vec<EvalTopic>) {
        let topics = eval_topics();
        let mut rng = seed.wrapping_add(0x9e3779b9);
        let mut out: Vec<EvalFile> = Vec::with_capacity(size);
        let noise = scenario == "messy";
        let flat = scenario == "flat";
        for i in 0..size {
            let t = &topics[(lcg(&mut rng) as usize) % topics.len()];
            let stem = t.stems[(lcg(&mut rng) as usize) % t.stems.len()];
            let variant = lcg(&mut rng) % 9000 + 1000; // 后缀,造出大量不同文件名
            let garbled = noise && (lcg(&mut rng) % 100) < 15;
            let name = if garbled {
                // 乱名(无主题信号)→ 仍标真值主题,考验「靠文件夹/同簇邻居兜底」
                let kinds = ["IMG_", "微信图片_", "DSC", "副本_未命名"];
                format!("{}{}.{}", kinds[(lcg(&mut rng) as usize) % kinds.len()], variant, t.ext)
            } else {
                format!("{stem}_{variant}.{}", t.ext)
            };
            let folder_flat = flat || (noise && (lcg(&mut rng) % 100) < 30);
            let relpath = if folder_flat {
                name.clone()
            } else {
                format!("{}/{}", t.folder, name)
            };
            out.push(EvalFile {
                relpath,
                name: name.clone(),
                ext: t.ext.to_string(),
                kind: t.kind.to_string(),
                size: 1024 + (variant as i64) * 7,
                mtime: (size - i) as i64, // 越靠前 mtime 越大(新)
                topic: t.id,
            });
        }
        (out, topics)
    }

    fn eval_schema(conn: &rusqlite::Connection) {
        conn.execute_batch(
            "CREATE TABLE roots(id INTEGER PRIMARY KEY, path TEXT);
             CREATE TABLE files(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 1,
                relpath TEXT NOT NULL, name TEXT NOT NULL, ext TEXT NOT NULL DEFAULT '',
                kind TEXT NOT NULL DEFAULT 'other', size INTEGER NOT NULL DEFAULT 0,
                mtime INTEGER NOT NULL DEFAULT 0, cluster_id INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE clusters(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 0,
                label TEXT NOT NULL DEFAULT '', color TEXT NOT NULL DEFAULT '', keywords TEXT NOT NULL DEFAULT '',
                size INTEGER NOT NULL DEFAULT 0, built_at INTEGER NOT NULL DEFAULT 0, parent INTEGER NOT NULL DEFAULT 0,
                summary TEXT NOT NULL DEFAULT '');
             CREATE TABLE cluster_edges(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 0,
                src INTEGER NOT NULL, dst INTEGER NOT NULL, label TEXT NOT NULL DEFAULT '', built_at INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE titles(file_id INTEGER PRIMARY KEY, title TEXT NOT NULL DEFAULT '', source TEXT NOT NULL DEFAULT '', made_at INTEGER NOT NULL DEFAULT 0);",
        )
        .unwrap();
    }

    #[test]
    fn cluster_eval_run() {
        if std::env::var("POLARIS_CLUSTER_EVAL").is_err() {
            return; // 普通 cargo test 跳过
        }
        let seed = env_u64("EVAL_SEED", 1);
        let size = env_u64("EVAL_SIZE", 800) as usize;
        let scenario = std::env::var("EVAL_SCENARIO").unwrap_or_else(|_| "organized".into());
        let use_llm = std::env::var("EVAL_NO_LLM").is_err();

        let (corpus, topics) = gen_corpus(&scenario, seed, size);
        let dbp = std::env::temp_dir().join(format!("polaris_eval_{seed}_{size}_{scenario}.db"));
        let _ = std::fs::remove_file(&dbp);
        let conn = rusqlite::Connection::open(&dbp).unwrap();
        eval_schema(&conn);
        conn.execute("INSERT INTO roots(id,path) VALUES(1,'/eval')", []).unwrap();
        {
            let mut ins = conn
                .prepare("INSERT INTO files(id,root_id,relpath,name,ext,kind,size,mtime,cluster_id) VALUES(?1,1,?2,?3,?4,?5,?6,?7,0)")
                .unwrap();
            for (i, f) in corpus.iter().enumerate() {
                ins.execute(rusqlite::params![
                    (i + 1) as i64, f.relpath, f.name, f.ext, f.kind, f.size, f.mtime
                ])
                .unwrap();
            }
        }
        let topic_of: HashMap<i64, usize> =
            corpus.iter().enumerate().map(|(i, f)| ((i + 1) as i64, f.topic)).collect();

        // ── T0:词法骨架(真生产函数)──
        let t0 = std::time::Instant::now();
        let summ = cluster_build_on(&conn, &[], ClusterMode::Lexical, std::time::Instant::now())
            .expect("cluster_build_on");
        let t0_ms = t0.elapsed().as_millis();

        // 读回归簇
        let assign: HashMap<i64, i64> = {
            let mut stmt = conn.prepare("SELECT id, cluster_id FROM files").unwrap();
            let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).unwrap();
            rows.flatten().collect()
        };
        let total = corpus.len();
        let covered = assign.values().filter(|&&c| c > 0).count();
        let coverage = covered as f64 / total as f64;

        // 叶簇(有文件挂着的簇)→ 各簇成员 + 主导主题。
        let mut members: HashMap<i64, Vec<i64>> = HashMap::new();
        for (&fid, &cid) in &assign {
            if cid > 0 {
                members.entry(cid).or_default().push(fid);
            }
        }
        // 纯度:每簇主导主题占比之和 / 已归类文件数。
        let mut pure_hits = 0usize;
        let mut cluster_dom: HashMap<i64, usize> = HashMap::new();
        for (&cid, mem) in &members {
            let mut tf: HashMap<usize, usize> = HashMap::new();
            for &fid in mem {
                *tf.entry(topic_of[&fid]).or_insert(0) += 1;
            }
            let (dom, cnt) = tf.into_iter().max_by_key(|&(_, c)| c).unwrap();
            pure_hits += cnt;
            cluster_dom.insert(cid, dom);
        }
        let purity = if covered > 0 { pure_hits as f64 / covered as f64 } else { 0.0 };
        let leaf_n = members.len();

        // ── T1:真大模型命名(读簇画像)──
        let mut name_acc = -1.0f64; // -1 = 未跑 LLM
        let mut name_acc_w = -1.0f64;
        let mut named_leaf = 0usize;
        let mut edges_n = 0usize;
        let mut samples: Vec<(String, String, bool)> = Vec::new(); // (主导主题文件夹, AI名, 命中)
        let mut llm_err = String::new();
        if use_llm {
            let digests = collect_cluster_digests(&conn, &[]).unwrap();
            let prompt = digest_directive(&digests);
            let cwd = std::env::temp_dir();
            match crate::kb::run_claude_readonly(&cwd, &prompt, |_, _| {}) {
                Ok(text) => match crate::kb::extract_balanced_json(&text) {
                    Some(raw) => match serde_json::from_str::<LlmNameRel>(&raw) {
                        Ok(parsed) => {
                            let valid: std::collections::HashSet<i64> =
                                digests.iter().map(|d| d.id).collect();
                            let croot: HashMap<i64, i64> =
                                digests.iter().map(|d| (d.id, 1i64)).collect();
                            let (_r, e, _m) =
                                apply_names_and_relations(&conn, &[], &parsed, &valid, &croot, 0)
                                    .unwrap();
                            edges_n = e;
                            // 读回每个叶簇的 AI 名,核对是否命中其主导主题别名。
                            let labels: HashMap<i64, String> = {
                                let mut stmt =
                                    conn.prepare("SELECT id, label FROM clusters").unwrap();
                                let rows = stmt
                                    .query_map([], |r| {
                                        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                                    })
                                    .unwrap();
                                rows.flatten().collect()
                            };
                            let mut hit = 0usize;
                            let mut hit_w = 0usize;
                            for (&cid, mem) in &members {
                                let dom = cluster_dom[&cid];
                                let label = labels.get(&cid).cloned().unwrap_or_default();
                                if !label.trim().is_empty() {
                                    named_leaf += 1;
                                }
                                let ok = topics[dom].aliases.iter().any(|a| label.contains(a));
                                if ok {
                                    hit += 1;
                                    hit_w += mem.len();
                                }
                                if samples.len() < 40 {
                                    samples.push((topics[dom].folder.to_string(), label, ok));
                                }
                            }
                            name_acc = if leaf_n > 0 { hit as f64 / leaf_n as f64 } else { 0.0 };
                            name_acc_w = if covered > 0 { hit_w as f64 / covered as f64 } else { 0.0 };
                        }
                        Err(e) => llm_err = format!("json parse: {e}"),
                    },
                    None => llm_err = "no json in model output".into(),
                },
                Err(e) => llm_err = format!("llm call: {e}"),
            }
        }

        let result = json!({
            "scenario": scenario, "seed": seed, "size": total,
            "t0_ms": t0_ms, "clusters": summ.clusters, "leaf_clusters": leaf_n,
            "coverage": (coverage * 1000.0).round() / 1000.0,
            "purity": (purity * 1000.0).round() / 1000.0,
            "name_acc": (name_acc * 1000.0).round() / 1000.0,
            "name_acc_weighted": (name_acc_w * 1000.0).round() / 1000.0,
            "named_leaf": named_leaf, "edges": edges_n,
            "llm_err": llm_err, "samples": samples.iter().map(|(f,n,ok)| json!({"topic_folder":f,"ai_name":n,"hit":ok})).collect::<Vec<_>>(),
        });
        let line = serde_json::to_string(&result).unwrap();
        println!("EVAL_RESULT {line}");
        if let Ok(out) = std::env::var("EVAL_OUT") {
            use std::io::Write as _;
            if let Ok(mut f) =
                std::fs::OpenOptions::new().create(true).append(true).open(&out)
            {
                let _ = writeln!(f, "{line}");
            }
        }
        let _ = std::fs::remove_file(&dbp);
        let _ = std::fs::remove_file(dbp.with_extension("db-wal"));
        let _ = std::fs::remove_file(dbp.with_extension("db-shm"));
    }

    #[test]
    fn maximal_roots_drops_nested() {
        // 还原线上那台机的真实根集合:D:\ 与 C:\ 之下各挂了一堆子根。
        let all = vec![
            (1, r"D:\polaris\polaris-app\src".to_string()),
            (2, r"D:\polaris\专家团队".to_string()),
            (3, r"C:\".to_string()),
            (4, r"D:\polaris\polaris-app\src-tauri".to_string()),
            (5, r"C:\Windows\System32".to_string()),
            (6, r"D:\".to_string()),
            (8, r"D:\polaris\polaris-app".to_string()),
        ];
        let mut keep = maximal_root_ids(&all);
        keep.sort();
        // 只剩两个极大根:C:\(3) 与 D:\(6);其余全是它们的子根。
        assert_eq!(keep, vec![3, 6]);
    }

    #[test]
    fn maximal_roots_keeps_siblings_and_prefix_lookalikes() {
        // 同级、以及「前缀像但不是子目录」的根都要保留(D:\foo 不是 D:\foobar 的祖先)。
        let all = vec![
            (1, r"D:\foo".to_string()),
            (2, r"D:\foobar".to_string()),
            (3, r"E:\data".to_string()),
            (4, r"D:\foo\child".to_string()),
        ];
        let mut keep = maximal_root_ids(&all);
        keep.sort();
        assert_eq!(keep, vec![1, 2, 3]); // 仅 D:\foo\child(4) 被剔除
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
    fn source_tag_recognizes_download_dirs() {
        assert_eq!(source_tag(r"C:\Users\me\Downloads", "a.pdf"), "下载");
        assert_eq!(source_tag(r"C:\Users\me\Documents\WeChat Files", "x/y.jpg"), "微信");
        assert_eq!(source_tag(r"C:\Users\me\Documents\xwechat_files", "f.docx"), "微信");
        assert_eq!(source_tag(r"C:\Users\me\Documents\WXWork", "f.zip"), "企业微信");
        // Tencent Files:按根末段,或 relpath 里的 FileRecv 命中
        assert_eq!(source_tag(r"C:\Users\me\Documents\Tencent Files", "123/FileRecv/a.7z"), "QQ");
        assert_eq!(source_tag("/data/nas/share", "2024/FileRecv/b.rar"), "QQ");
        // 普通目录 → 空(不显示徽标)
        assert_eq!(source_tag(r"D:\datasets", "housing/a.csv"), "");
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
