//! 摄入 kb_ingest / 批量上传转换 / 摄入缓存 / 文件名与路径安全 —— 自原 kb.rs 纯移动, 逻辑零改动。

use super::*;

/// Ingest 单文件:任意格式 → 转 markdown 写入 raw/(不可转的原样复制),增量刷新索引。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn kb_ingest(source_path: String) -> Result<String, String> {
    let root = KB_ROOT.read().clone();
    let mut cache = IngestCache::load(&root);
    let rel = ingest_one(&root, &PathBuf::from(&source_path), &mut cache);
    cache.save(&root);
    let rel = rel?;
    // 增量: 只解析新文件加入 INDEX, 避免全量重扫
    let full = root.join(&rel);
    if let Ok(rp) = full.strip_prefix(&root) {
        if let Some(doc) = parse_doc(&full, rp) {
            index_add_doc(doc);
        }
    }
    Ok(rel)
}

/// 知识库拖拽上传:批量(可含目录,自动展开)。每个文件转 markdown 入 raw/,
/// 全部处理完只重扫一次索引。返回逐文件结果(失败不影响其余)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn kb_upload_files(paths: Vec<String>) -> Vec<KbUploadResult> {
    // 完整性:用户勾选的文件必须**全部**归档,不能像旧版那样到 500 就静默丢弃(界面还提示
    // 「正在归档 N 个」却只进 500 个)。上限抬到 50000(远超资源扫描表 20000 行上限,故勾选多少
    // 归多少);仅作防呆护栏拦住「拖进一个含几十万文件的超大目录」这类病态输入。
    const MAX_FILES: usize = 50_000;
    let root = KB_ROOT.read().clone();
    let files = expand_to_files(&paths, MAX_FILES);
    let truncated = files.len() >= MAX_FILES;
    // 整批共用一个缓存: 未变且产物仍在的源跳过转换, 结束统一落盘。
    let mut cache = IngestCache::load(&root);

    let mut results = Vec::with_capacity(files.len());
    for f in &files {
        let name = f
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| f.to_string_lossy().to_string());
        match ingest_one(&root, f, &mut cache) {
            Ok(rel) => results.push(KbUploadResult {
                name,
                rel_path: rel,
                ok: true,
                message: String::new(),
            }),
            Err(e) => results.push(KbUploadResult {
                name,
                rel_path: String::new(),
                ok: false,
                message: e,
            }),
        }
    }
    cache.save(&root);

    // 万一真撞上 5 万护栏:显式告知用户还有剩余(别静默丢),让其分批或缩小范围。
    if truncated {
        results.push(KbUploadResult {
            name: "(已达单次归档上限)".into(),
            rel_path: String::new(),
            ok: false,
            message: format!(
                "本次按上限归档了 {MAX_FILES} 个文件;若还有更多,请再点一次归档(已归档的会自动跳过)。"
            ),
        });
    }

    // 增量: 逐个解析成功入库的文件加入 INDEX, 避免全量重扫
    for r in &results {
        if !r.ok || r.rel_path.is_empty() {
            continue;
        }
        let full = root.join(&r.rel_path);
        if let Ok(rp) = full.strip_prefix(&root) {
            if let Some(doc) = parse_doc(&full, rp) {
                index_add_doc(doc);
            }
        }
    }

    results
}

#[derive(Serialize)]
pub struct KbUploadResult {
    pub name: String,
    pub rel_path: String,
    pub ok: bool,
    pub message: String,
}

// ───────────────────────── 批量转换 md (管理页「批量转换 md 文件」) ─────────────────────────
//
// 与拖拽上传/ingest 的差别: 这是「只要 markdown」的批量通道 ——
// 可抽文本的 (PDF/Word/Excel/PPT/文本/代码) 转成 .md 入 raw/;
// 视频类明确跳过 (主要针对非视频类文件, 视频留给将来的 ASR 链路);
// 图片/音频/压缩包等抽不出文本的也跳过**而不是原样复制**, 避免把大体积二进制灌进知识库。

/// 视频扩展名 (小写)。注意不含 "ts" —— 那会误伤 TypeScript 源码 (TEXT_EXTS 按文本转)。
pub(crate) const VIDEO_EXTS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "mpg", "mpeg", "m2ts", "3gp", "rmvb",
    "rm", "vob", "ogv",
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KbConvertReport {
    /// 扫到的文件总数
    pub total: usize,
    /// 成功转成 md 的数量 (含缓存命中复用)
    pub converted: usize,
    /// 视频类跳过数
    pub skipped_video: usize,
    /// 其它跳过数 (图片/音频/压缩包等不可抽文本, 以及 KB 内已是 md 的文件)
    pub skipped_other: usize,
    /// 失败明细 "文件名: 原因"
    pub failed: Vec<String>,
}

/// 批量转换: 路径(文件或文件夹, 文件夹递归展开)下的非视频类文件 → markdown 入 raw/ 并增量索引。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn kb_convert_batch(paths: Vec<String>) -> Result<KbConvertReport, String> {
    const MAX_FILES: usize = 2000;
    let root = KB_ROOT.read().clone();
    let raw_dir = root.join("raw");
    fs::create_dir_all(&raw_dir).map_err(|e| e.to_string())?;

    let files = expand_to_files(&paths, MAX_FILES);
    if files.is_empty() {
        return Err("没找到文件: 请确认填的是存在的文件或文件夹绝对路径".into());
    }

    let mut cache = IngestCache::load(&root);
    let mut report = KbConvertReport {
        total: files.len(),
        converted: 0,
        skipped_video: 0,
        skipped_other: 0,
        failed: Vec::new(),
    };
    let mut new_rels: Vec<String> = Vec::new();

    for f in &files {
        let ext = f
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if VIDEO_EXTS.contains(&ext.as_str()) {
            report.skipped_video += 1;
            continue;
        }
        // KB 根内已是 md 的文件不重转, 防止用户把 KB 根自己填进来时自吞出 "(2)" 副本
        if ext == "md" && f.starts_with(&root) {
            report.skipped_other += 1;
            continue;
        }
        match convert_one_md(&root, &raw_dir, f, &mut cache) {
            Ok(Some(rel)) => {
                report.converted += 1;
                new_rels.push(rel);
            }
            Ok(None) => report.skipped_other += 1,
            Err(e) => {
                let name = f
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| f.to_string_lossy().to_string());
                report.failed.push(format!("{name}: {e}"));
            }
        }
    }
    cache.save(&root);

    // 增量索引新产物, 避免全量重扫
    for rel in &new_rels {
        let full = root.join(rel);
        if let Ok(rp) = full.strip_prefix(&root) {
            if let Some(doc) = parse_doc(&full, rp) {
                index_add_doc(doc);
            }
        }
    }
    Ok(report)
}

/// 单文件「只要 md」转换: 可抽文本 → 写 raw/<stem>.md 并记缓存; 不可抽 → Ok(None) 跳过。
/// 与 ingest_one 的差别: 不做「不可转就原样复制」的兜底。
pub(crate) fn convert_one_md(
    root: &Path,
    raw_dir: &Path,
    src: &Path,
    cache: &mut IngestCache,
) -> Result<Option<String>, String> {
    if !src.is_file() {
        return Err("不是文件".into());
    }
    let src_key = src.to_string_lossy().replace('\\', "/");
    let fingerprint = content_fingerprint(src);
    if let Some(fp) = &fingerprint {
        if let Some(raw_rel) = cache.lookup_valid(root, &src_key, fp) {
            // 只复用 md 产物; 旧通道原样复制进来的非 md 产物不算"已转换"
            if raw_rel.ends_with(".md") {
                return Ok(Some(raw_rel));
            }
        }
    }
    let Some(md) = convert::convert_to_markdown(src)? else {
        return Ok(None);
    };
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "untitled".into());
    let dst = unique_path(raw_dir, &stem, "md");
    let titled = format!("# {stem}\n\n{md}");
    fs::write(&dst, titled).map_err(|e| e.to_string())?;
    let rel = rel_of(root, &dst);
    if let Some(fp) = fingerprint {
        cache.record(src_key, fp, rel.clone());
    }
    Ok(Some(rel))
}

// ───────────────────────── 增量入库缓存 (借鉴 llm_wiki ingest-cache) ─────────────────────────
//
// 痛点: 重复拖同一批资料入库, 每次都全量重转 (PDF/docx 抽取很贵)。
// 借鉴 llm_wiki: 给源文件算内容指纹, 指纹没变 → 跳过转换, 直接复用上次产物。
// 关键的第二步 (llm_wiki 的「防幽灵条目」洞察): 命中缓存还要**校验产物仍在磁盘上**,
// 否则旧产物被删后缓存还指着它, 会"跳过"导致库里凭空少一篇。
// 用 std 的 DefaultHasher (siphash) 做内容指纹 —— 仅需变更检测, 不引入 sha2 依赖。

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct IngestCache {
    /// 源文件绝对路径 → (内容指纹, 产物 raw 相对路径)
    #[serde(default)]
    entries: HashMap<String, (String, String)>,
    #[serde(skip)]
    dirty: bool,
}

pub(crate) fn ingest_cache_path(root: &Path) -> PathBuf {
    root.join(".polaris_ingest_cache.json")
}

/// 计算文件内容指纹 (siphash, 仅用于变更检测)。读失败返回 None。
pub(crate) fn content_fingerprint(src: &Path) -> Option<String> {
    use std::hash::Hasher;
    let bytes = fs::read(src).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write(&bytes);
    Some(format!("{:x}", h.finish()))
}

impl IngestCache {
    fn load(root: &Path) -> Self {
        fs::read_to_string(ingest_cache_path(root))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, root: &Path) {
        if self.dirty {
            if let Ok(s) = serde_json::to_string(self) {
                let _ = fs::write(ingest_cache_path(root), s);
            }
        }
    }

    /// 命中缓存且产物仍在磁盘 → 返回可复用的 raw 相对路径。
    fn lookup_valid(&self, root: &Path, src_key: &str, fp: &str) -> Option<String> {
        let (cached_fp, raw_rel) = self.entries.get(src_key)?;
        if cached_fp != fp {
            return None; // 内容变了
        }
        if !root.join(raw_rel).exists() {
            return None; // 防幽灵条目: 产物已被删
        }
        Some(raw_rel.clone())
    }

    /// 该源已记录的旧产物相对路径(用于内容变更时删除陈旧副本)。
    fn stale_artifact(&self, src_key: &str) -> Option<String> {
        self.entries.get(src_key).map(|(_, rel)| rel.clone())
    }
    fn record(&mut self, src_key: String, fp: String, raw_rel: String) {
        self.entries.insert(src_key, (fp, raw_rel));
        self.dirty = true;
    }
}

/// 把一个源文件落到 KB 的 raw/:
/// - 命中增量缓存(内容未变且产物仍在) → 跳过转换, 复用上次产物
/// - 可抽文本 → 写 `raw/<stem>.md`
/// - 不可抽(图片/二进制) → 原样复制 `raw/<filename>`
/// 返回写入的相对路径(正斜杠)。
pub(crate) fn ingest_one(
    root: &Path,
    src: &Path,
    cache: &mut IngestCache,
) -> Result<String, String> {
    if !src.is_file() {
        return Err(format!("不是文件: {}", src.to_string_lossy()));
    }
    let raw_dir = root.join("raw");
    fs::create_dir_all(&raw_dir).map_err(|e| e.to_string())?;

    // 增量缓存: 内容指纹未变且产物仍在磁盘 → 直接复用, 跳过昂贵的转换。
    let src_key = src.to_string_lossy().replace('\\', "/");
    let fingerprint = content_fingerprint(src);
    if let Some(fp) = &fingerprint {
        if let Some(raw_rel) = cache.lookup_valid(root, &src_key, fp) {
            return Ok(raw_rel);
        }
    }

    // 指纹变了(源文件被编辑过重新拖入): 先删旧产物。否则 unique_path 会另写 "stem (2).md",
    // 旧的陈旧内容永远留在 raw/ 和 INDEX 里, 被搜索/图谱/编译当成独立页一并引用。
    if let Some(old_rel) = cache.stale_artifact(&src_key) {
        let old = root.join(&old_rel);
        if old.exists() {
            let _ = fs::remove_file(&old);
        }
    }

    let raw_rel = ingest_convert_write(root, src, &raw_dir)?;
    if let Some(fp) = fingerprint {
        cache.record(src_key, fp, raw_rel.clone());
    }
    Ok(raw_rel)
}

/// 实际的转换+落盘 (从 ingest_one 拆出, 便于缓存命中时整体跳过)。
pub(crate) fn ingest_convert_write(
    root: &Path,
    src: &Path,
    raw_dir: &Path,
) -> Result<String, String> {
    match convert::convert_to_markdown(src)? {
        Some(md) => {
            let stem = src
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "untitled".into());
            let dst = unique_path(raw_dir, &stem, "md");
            // 顶部补一个标题便于 KB 索引与预览;但转换结果若已自带 H1(源本就是带 `# 标题` 的
            // markdown,或 converter 已产出标题)就不再叠加,避免正文顶部出现两行重复的 `# 标题`。
            let titled = if md.trim_start().starts_with("# ") {
                md
            } else {
                format!("# {stem}\n\n{md}")
            };
            fs::write(&dst, titled).map_err(|e| e.to_string())?;
            Ok(rel_of(root, &dst))
        }
        None => {
            let fname = src
                .file_name()
                .ok_or_else(|| "无文件名".to_string())?
                .to_string_lossy()
                .to_string();
            let (stem, ext) = split_name(&fname);
            let dst = unique_path(raw_dir, &stem, &ext);
            fs::copy(src, &dst).map_err(|e| e.to_string())?;
            Ok(rel_of(root, &dst))
        }
    }
}

/// 展开输入路径:目录递归取文件,文件直接收,去重并限量。
pub(crate) fn expand_to_files(paths: &[String], cap: usize) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for p in paths {
        if out.len() >= cap {
            break;
        }
        let pb = PathBuf::from(p);
        if pb.is_dir() {
            for e in WalkDir::new(&pb).into_iter().flatten() {
                if e.path().is_file() {
                    out.push(e.path().to_path_buf());
                    if out.len() >= cap {
                        break;
                    }
                }
            }
        } else if pb.is_file() {
            out.push(pb);
        }
    }
    out
}

/// 在 dir 下生成不冲突的路径 `<stem>.<ext>`,冲突则追加 ` (2)` ` (3)` …
pub(crate) fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let safe = sanitize_stem(stem);
    let first = dir.join(format!("{safe}.{ext}"));
    if !first.exists() {
        return first;
    }
    for n in 2..10_000 {
        let cand = dir.join(format!("{safe} ({n}).{ext}"));
        if !cand.exists() {
            return cand;
        }
    }
    first
}

/// 去掉文件名里对 Windows 非法的字符
pub(crate) fn sanitize_stem(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c })
        .collect();
    let t = cleaned.trim().trim_matches('.').trim();
    if t.is_empty() {
        "untitled".into()
    } else {
        t.to_string()
    }
}

pub(crate) fn split_name(fname: &str) -> (String, String) {
    match fname.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), e.to_string()),
        _ => (fname.to_string(), "bin".to_string()),
    }
}

pub(crate) fn rel_of(root: &Path, full: &Path) -> String {
    full.strip_prefix(root)
        .unwrap_or(full)
        .to_string_lossy()
        .replace('\\', "/")
}

// ───────────────────────── 安全路径护栏 (借鉴 llm_wiki isSafeIngestPath) ─────────────────────────
//
// 编译器 (kb_compile) 给 headless claude 开了写权限自由落盘 wiki 页。万一模型(或被注入)给出
// `C:\Windows\...` 这种绝对路径、`../../` 越界、或 Windows 保留名, 就可能写坏库外文件。
// 这是一道**纯函数**护栏: 校验「应当落在 wiki/ 下的相对路径」是否安全。7 层校验, 一条不过即拒。
// 用于编译后审计 (kb_lint) 与任何接受模型生成路径的入口。

/// Windows 设备保留名 (任意大小写, 含带扩展名形式如 `CON.md` 也保留)。
pub(crate) const WIN_RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// 校验一个「本应落在 wiki/ 下」的相对路径是否安全可写。
/// 返回 `Err(原因)` 列出第一个不通过的校验项。
pub fn is_safe_wiki_relpath(raw: &str) -> Result<(), String> {
    // ① 无控制字符
    if raw.chars().any(|c| c.is_control()) {
        return Err("含控制字符".into());
    }
    // ② 拒绝绝对路径 (Unix `/`、UNC `\\`、Windows 盘符 `C:`)
    if raw.starts_with('/') || raw.starts_with('\\') {
        return Err("是绝对路径".into());
    }
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Err("含 Windows 盘符".into());
    }
    // ③ 规范化反斜杠后逐段检查
    let norm = raw.replace('\\', "/");
    let segs: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return Err("路径为空".into());
    }
    for seg in &segs {
        // ④ 无 `.` / `..` 越界段
        if *seg == ".." || *seg == "." {
            return Err("含 .. 或 . 越界段".into());
        }
        // ⑤ Windows 保留名 (取扩展名前的主名判断)
        let stem = seg.split('.').next().unwrap_or(seg).to_lowercase();
        if WIN_RESERVED.contains(&stem.as_str()) {
            return Err(format!("含 Windows 保留名: {seg}"));
        }
        // ⑥ 段尾不得为空格或点 (Windows 会静默剥离, 造成路径歧义)
        if seg.ends_with(' ') || seg.ends_with('.') {
            return Err(format!("段尾有空格或点: {seg}"));
        }
    }
    // ⑦ 必须落在 wiki/ 下且为 markdown
    if segs[0] != "wiki" {
        return Err("未落在 wiki/ 下".into());
    }
    if !(norm.ends_with(".md") || norm.ends_with(".markdown")) {
        return Err("不是 .md 文件".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_path_accepts_normal_wiki_pages() {
        assert!(is_safe_wiki_relpath("wiki/概念/主观主义.md").is_ok());
        assert!(is_safe_wiki_relpath("wiki/index.md").is_ok());
        assert!(is_safe_wiki_relpath("wiki/实体/a/b.markdown").is_ok());
    }

    #[test]
    fn safe_path_rejects_escapes_and_absolutes() {
        assert!(is_safe_wiki_relpath("../etc/passwd.md").is_err()); // 越界 (且不在 wiki/)
        assert!(is_safe_wiki_relpath("wiki/../../x.md").is_err()); // .. 段
        assert!(is_safe_wiki_relpath("/wiki/x.md").is_err()); // 绝对
        assert!(is_safe_wiki_relpath("C:/wiki/x.md").is_err()); // 盘符
        assert!(is_safe_wiki_relpath("\\\\srv\\wiki\\x.md").is_err()); // UNC
    }

    #[test]
    fn safe_path_rejects_reserved_and_outside_wiki() {
        assert!(is_safe_wiki_relpath("wiki/CON.md").is_err()); // Windows 保留名
        assert!(is_safe_wiki_relpath("wiki/nul.md").is_err());
        assert!(is_safe_wiki_relpath("raw/x.md").is_err()); // 不在 wiki/
        assert!(is_safe_wiki_relpath("wiki/note.txt").is_err()); // 非 md
        assert!(is_safe_wiki_relpath("wiki/sub /page.md").is_err()); // 目录段尾空格
        assert!(is_safe_wiki_relpath("wiki/sub./page.md").is_err()); // 目录段尾点
    }
}
