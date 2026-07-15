use super::*;

// ───────────────────────── 簇配色(与星河主题同源的高级色) ─────────────────────────

pub(crate) const CLUSTER_PALETTE: &[&str] = &[
    "#5b8cff", "#8b6cff", "#c264d6", "#e0736b", "#e0a24b", "#6fcf97", "#42c8d4", "#5fa8e6",
    "#d4b06a", "#b487e0", "#e08aae", "#7ec8a0", "#9aa0e6", "#d49a6a", "#6cc0c0", "#cf9fd6",
    "#7f9cf5", "#e6a4c4",
];

/// 命名启发式忽略的通用目录段(它们当簇名没区分度)。
/// 当簇标签会显得「不是给人看」的目录名:① 通用容器名(files/data/新建文件夹)② 技术/格式/
/// 工程目录名(html/css/js/log/node_modules/target…)。命名与词法向量都跳过它们,免得聚出
/// 「html」「dist」「log」这类机器味分类——用户要主题(报税/装修),不是格式或工程目录。
pub(crate) const GENERIC_DIRS: &[&str] = &[
    // 通用容器
    "raw",
    "wiki",
    "output",
    "memory",
    "src",
    "docs",
    "doc",
    "data",
    "assets",
    "public",
    "dist",
    "build",
    "tmp",
    "temp",
    "files",
    "file",
    "新建文件夹",
    "downloads",
    "下载",
    "documents",
    "文档",
    "desktop",
    "桌面",
    "untitled",
    "misc",
    "other",
    "others",
    "杂项",
    "其它",
    "其他",
    // 技术/格式/工程目录(常是软件生成、非人看)
    "html",
    "htm",
    "css",
    "js",
    "ts",
    "jsx",
    "tsx",
    "json",
    "xml",
    "yaml",
    "yml",
    "log",
    "logs",
    "cache",
    "caches",
    "bin",
    "obj",
    "lib",
    "libs",
    "include",
    "vendor",
    "target",
    "node_modules",
    "venv",
    "__pycache__",
    ".git",
    ".idea",
    ".vscode",
    "static",
    "scripts",
    "styles",
    "fonts",
    "icons",
    "thumbnails",
    "thumbs",
    "cache_data",
];

// ───────────────────────── 通用小工具 ─────────────────────────

pub(crate) fn data_dir() -> Option<PathBuf> {
    crate::fable::db_path().and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

pub(crate) fn thumbs_dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("thumbs"))
}

pub(crate) fn hash_key(parts: &[&str]) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for p in parts {
        p.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

/// 标准 base64 编码(避免引第三方 base64 crate)。
pub(crate) fn b64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

pub(crate) fn human_size(bytes: u64) -> String {
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
pub(crate) fn resolve_root_ids(conn: &rusqlite::Connection, root: &Option<String>) -> Vec<i64> {
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
        if let Ok(rows) = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        }) {
            all.extend(rows.flatten());
        }
    }
    maximal_root_ids(&all)
}

/// 从 (id, path) 列表里挑出极大根:凡 path 嵌套在另一条 path 之下的都剔除。
/// 拆成纯函数便于单测。Windows 路径不分大小写、分隔符统一成 `/` 再比。
pub(crate) fn maximal_root_ids(all: &[(i64, String)]) -> Vec<i64> {
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
pub(crate) fn in_clause(ids: &[i64]) -> String {
    if ids.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        format!(" AND f.root_id IN ({})", list.join(","))
    }
}

/// 文件的「语言归类」标签:优先用回填好的 lang 列;为空时由 ext/kind 当场推(代码/媒体准确,
/// 文稿尚未回填自然语言 → 归「文档·待识别」)。grid 过滤与 overview 折叠共用同一口径。
pub(crate) fn language_label(stored: &str, ext: &str, kind: &str) -> String {
    if !stored.is_empty() {
        return stored.to_string();
    }
    let q = crate::fable::inventory::quick_lang(ext, kind);
    if q.is_empty() {
        "文档·待识别".to_string()
    } else {
        q
    }
}

// ───────────────────────── 概览缓存 + 单飞(并发合并) ─────────────────────────
//
// file_overview 对 files 表(可达百万行)跑多个全表 GROUP BY 聚合, 每次开一个带 mmap/cache
// 预算的连接。UI 挂载/刷新常并发触发同一概览 → N 路各扫全表 + 各占一份 cache, 延迟随并发
// 放大(实测 8 路 ≈5.7× 单路)且内存高水位。修法: 同 root 的并发调用**单飞**(只一个真算,
// 其余等它的结果), 并对结果做**短 TTL 缓存**(概览是只读聚合, 秒级陈旧无害)——一阵并发
// 爆发塌缩成一次扫描。TTL=0 关闭缓存(eval/需强一致时), 经 POLARIS_FABLE_OVERVIEW_TTL_MS 覆写。

struct OverviewCacheEntry {
    at: std::time::Instant,
    val: FileOverview,
}

static OVERVIEW_CACHE: once_cell::sync::Lazy<parking_lot::Mutex<HashMap<String, OverviewCacheEntry>>> =
    once_cell::sync::Lazy::new(|| parking_lot::Mutex::new(HashMap::new()));
// 每 root 一把单飞锁: 并发同 root 调用在此串行, 只有第一个真算, 其余醒来即命中新鲜缓存。
static OVERVIEW_FLIGHT: once_cell::sync::Lazy<
    parking_lot::Mutex<HashMap<String, std::sync::Arc<parking_lot::Mutex<()>>>>,
> = once_cell::sync::Lazy::new(|| parking_lot::Mutex::new(HashMap::new()));

fn overview_ttl() -> std::time::Duration {
    let ms = std::env::var("POLARIS_FABLE_OVERVIEW_TTL_MS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(5000);
    std::time::Duration::from_millis(ms)
}

/// 概览(带并发合并 + 短 TTL 缓存)。缓存键 = 规范化 root(None → 空串 = 全库)。
pub fn overview(root: Option<String>) -> Result<FileOverview, String> {
    let ttl = overview_ttl();
    if ttl.is_zero() {
        return overview_uncached(root); // 关闭缓存: 直算(eval/强一致)
    }
    let key = root.clone().unwrap_or_default();

    // 快路径: 命中新鲜缓存直接返回(短锁, 不跨计算)。
    if let Some(e) = OVERVIEW_CACHE.lock().get(&key) {
        if e.at.elapsed() < ttl {
            return Ok(e.val.clone());
        }
    }

    // 取本 key 的单飞锁(并发同 root 在此串行)。
    let flight = OVERVIEW_FLIGHT.lock().entry(key.clone()).or_default().clone();
    let _g = flight.lock();

    // 二次检查: 等锁期间别的调用可能刚算完并填了缓存。
    if let Some(e) = OVERVIEW_CACHE.lock().get(&key) {
        if e.at.elapsed() < ttl {
            return Ok(e.val.clone());
        }
    }

    // 由我真算一次并回填缓存(只我持单飞锁, 不阻塞别的 root)。
    let val = overview_uncached(root)?;
    OVERVIEW_CACHE.lock().insert(
        key,
        OverviewCacheEntry {
            at: std::time::Instant::now(),
            val: val.clone(),
        },
    );
    Ok(val)
}

fn overview_uncached(root: Option<String>) -> Result<FileOverview, String> {
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

    let one =
        |sql: &str| -> u64 { conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0) as u64 };
    let text_files = one(&format!(
        "SELECT COUNT(*) FROM files f WHERE f.kind='text'{filter}"
    ));
    let embedded_files = one(&format!(
        "SELECT COUNT(*) FROM files f WHERE f.kind='text' AND f.chunked=1{filter}"
    ));

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
        scanning: crate::fable::SCANNING.load(Ordering::Relaxed),
        indexing: crate::fable::INDEXING.load(Ordering::Relaxed),
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
pub(crate) fn source_tag(root_path: &str, relpath: &str) -> &'static str {
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
    if let Some(l) = lang
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "all")
    {
        let exts = crate::fable::inventory::exts_for_lang(l);
        if !exts.is_empty() {
            let list: Vec<String> = exts.iter().map(|e| format!("'{e}'")).collect();
            where_sql.push_str(&format!(" AND LOWER(f.ext) IN ({})", list.join(",")));
        } else if let Some(k) = crate::fable::inventory::kind_for_media_lang(l) {
            where_sql.push_str(&format!(" AND f.kind='{k}'"));
        } else if l == "文档·待识别" {
            let codes: Vec<String> = crate::fable::inventory::CODE_EXTS
                .iter()
                .map(|e| format!("'{e}'"))
                .collect();
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
        where_sql.push_str(&format!(
            " AND (LOWER(f.name) LIKE '%{safe}%' OR LOWER(f.relpath) LIKE '%{safe}%')"
        ));
    }
    let order = match sort.as_deref() {
        Some("name") => "f.name ASC",
        Some("size") => "f.size DESC",
        Some("kind") => "f.kind ASC, f.name ASC",
        _ => "f.mtime DESC",
    };

    let total: u64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM files f {where_sql}"),
            [],
            |r| r.get::<_, i64>(0),
        )
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
    Ok(FileGridPage {
        items,
        total,
        page,
        page_size,
    })
}
