use super::*;

// ───────────────────────── chunker ─────────────────────────

/// 结构感知切块:段落聚合到 ~1600 字符;超长段硬切(200 字符重叠);全按 char 计数杜绝多字节
/// 边界 panic。相较旧「纯空行聚段」多做三件事,直接抬检索命中与溯源精度:
///   ① Markdown 标题(`#`..`######`)是**硬边界** —— 不跨章节聚段,语义边界不再被切碎;
///   ② 每块前缀面包屑「【章节 › 子节｜pN】」,把文件内**局部上下文**注进向量(利于「正文不点题、
///      标题点题」的召回)、并让命中片段自带页码/章节溯源(前端零改动即可展示);
///   ③ 消化 `[[page:N]]` 页码标记(convert.rs 从 PDF 抽取时注入)—— 只更新当前页、**不**强制断块,
///      故不会把一页碎成多块,块以其**起始页**标注。
/// 面包屑不占正文预算之外的语义(同嵌入空间);只影响新建/重建的 chunk,不触发全库重嵌。
pub(crate) fn chunk_text(s: &str) -> Vec<String> {
    const TARGET: usize = 1600;
    const OVERLAP: usize = 200;

    // ── 预解析:把段落归类成 页码标记 / 标题 / 正文 ──
    enum Seg {
        Page(u32),
        Heading(usize, String),
        Body(String),
    }
    let mut segs: Vec<Seg> = Vec::new();
    for para in s.split("\n\n") {
        let t = para.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(n) = parse_page_marker(t) {
            segs.push(Seg::Page(n));
            continue;
        }
        let first_line = t.lines().next().unwrap_or("");
        if let Some((level, title)) = parse_atx_heading(first_line) {
            segs.push(Seg::Heading(level, title));
            // 标题同段可能跟着正文(单换行),拆出来当独立正文段
            let rest = t[first_line.len()..].trim();
            if !rest.is_empty() {
                segs.push(Seg::Body(rest.to_string()));
            }
            continue;
        }
        segs.push(Seg::Body(t.to_string()));
    }

    // ── 聚段 ──
    let mut chunks: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_chars = 0usize;
    let mut crumb = String::new(); // 当前 cur 首段进入时定格的面包屑
    let mut section: Vec<String> = Vec::new(); // 标题栈,index = level-1
    let mut page: Option<u32> = None;

    let flush = |cur: &mut String, cur_chars: &mut usize, crumb: &str, chunks: &mut Vec<String>| {
        push_chunk(crumb, cur, chunks);
        cur.clear();
        *cur_chars = 0;
    };

    for seg in segs {
        if chunks.len() >= MAX_CHUNKS_PER_FILE {
            break;
        }
        match seg {
            Seg::Page(n) => page = Some(n), // 只更新当前页,不断块
            Seg::Heading(level, title) => {
                flush(&mut cur, &mut cur_chars, &crumb, &mut chunks); // 标题是硬边界
                section.truncate(level.saturating_sub(1));
                while section.len() + 1 < level {
                    section.push(String::new()); // 跳级标题补空位
                }
                section.push(title);
            }
            Seg::Body(b) => {
                let plen = b.chars().count();
                if plen > TARGET {
                    flush(&mut cur, &mut cur_chars, &crumb, &mut chunks);
                    let cr = make_crumb(&section, page);
                    let cs: Vec<char> = b.chars().collect();
                    let mut start = 0usize;
                    while start < cs.len() {
                        let end = (start + TARGET).min(cs.len());
                        let piece: String = cs[start..end].iter().collect();
                        push_chunk(&cr, &piece, &mut chunks);
                        if end == cs.len() {
                            break;
                        }
                        start = end.saturating_sub(OVERLAP);
                    }
                    continue;
                }
                if cur_chars + plen > TARGET {
                    flush(&mut cur, &mut cur_chars, &crumb, &mut chunks);
                }
                if cur.is_empty() {
                    crumb = make_crumb(&section, page); // 定格首段面包屑
                } else {
                    cur.push_str("\n\n");
                }
                cur.push_str(&b);
                cur_chars += plen + 2;
            }
        }
    }
    flush(&mut cur, &mut cur_chars, &crumb, &mut chunks);
    chunks.retain(|c| !c.is_empty());
    chunks.truncate(MAX_CHUNKS_PER_FILE);
    chunks
}

/// 解析 `[[page:N]]` 页码标记段(convert.rs 从 PDF 逐页抽取时注入)。
fn parse_page_marker(t: &str) -> Option<u32> {
    t.trim()
        .strip_prefix("[[page:")?
        .strip_suffix("]]")?
        .trim()
        .parse()
        .ok()
}

/// 解析 ATX 标题行 `#`..`######`(须 `#` 后有空白,避免误判 `#话题标签`)。返回(层级, 标题文本)。
fn parse_atx_heading(line: &str) -> Option<(usize, String)> {
    let l = line.trim_start();
    let hashes = l.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let after = &l[hashes..];
    if !after.starts_with(' ') && !after.starts_with('\t') {
        return None;
    }
    let title = after.trim();
    if title.is_empty() {
        None
    } else {
        Some((hashes, title.to_string()))
    }
}

/// 由标题栈 + 页码拼面包屑「【章节 › 子节｜pN】」;都为空则返回空串(无前缀)。截到 96 字符。
fn make_crumb(section: &[String], page: Option<u32>) -> String {
    let path = section
        .iter()
        .filter(|t| !t.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" › ");
    let mut parts: Vec<String> = Vec::new();
    if !path.is_empty() {
        parts.push(path);
    }
    if let Some(p) = page {
        parts.push(format!("p{p}"));
    }
    if parts.is_empty() {
        return String::new();
    }
    let mut inner = parts.join("｜");
    if inner.chars().count() > 96 {
        inner = inner.chars().take(96).collect();
    }
    format!("【{inner}】\n")
}

/// 给正文加面包屑前缀(面包屑为空则原样返回)。
fn with_crumb(crumb: &str, content: &str) -> String {
    if crumb.is_empty() {
        content.to_string()
    } else {
        format!("{crumb}{content}")
    }
}

/// 落一个 chunk:正文非空、且**成品(面包屑+正文)**≥24 字才入库。按成品长度而非纯正文卡门槛,
/// 是为了不丢「标题点题、正文极短」的章节 —— 面包屑里的章节名本身是可检索内容,连同短正文一起
/// 进向量,恰好补上「标题-only / 短条目」结构此前会被静默丢弃的语义召回(codex 审查发现的回归点)。
fn push_chunk(crumb: &str, content: &str, chunks: &mut Vec<String>) {
    let c = content.trim();
    let n = c.chars().count();
    if crumb.is_empty() {
        // 无结构:沿用旧门槛,<24 字的裸文本当碎块丢弃。
        if n >= 24 {
            chunks.push(c.to_string());
        }
    } else {
        // 有章节/页码面包屑:章节名本身即信号,短正文也保留(仅挡 <2 字的单字噪声)。
        if n >= 2 {
            chunks.push(with_crumb(crumb, c));
        }
    }
}

pub(crate) fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

pub(crate) fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// 直接在 f32 小端字节上算点积(向量均已归一化 → 即余弦),省掉 [`blob_to_vec`] 的中间
/// `Vec<f32>` 堆分配 —— 检索精排每候选省一次分配(大库一次查询数百候选)。`blob` 字节数须
/// 为 `qv.len()*4`(维度/模型一致),否则 `None`(脏数据/旧维度向量,调用方跳过)。
pub(crate) fn dot_blob(qv: &[f32], blob: &[u8]) -> Option<f32> {
    if blob.len() != qv.len() * 4 {
        return None;
    }
    let mut s = 0f32;
    for (i, q) in qv.iter().enumerate() {
        let o = i * 4;
        s += q * f32::from_le_bytes([blob[o], blob[o + 1], blob[o + 2], blob[o + 3]]);
    }
    Some(s)
}

// ───────────────────────── 归一化 / 二值量化(P1-3 / P1-1)─────────────────────────

/// L2 归一化(就地)。入库前归一化一次 → 查询余弦退化成纯点积,省掉「每查询给每个向量现算模长」。
pub(crate) fn normalize(v: &mut [f32]) {
    let n = (v.iter().map(|x| x * x).sum::<f32>()).sqrt();
    if n > 1e-12 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
}

/// 符号位打包成二值码(dim 位 → ⌈dim/8⌉ 字节)。两段式 ANN 第一段用它算汉明距离做角度粗筛,
/// 读量只有 f32 的 1/32。归一化向量上,汉明距离与角度强相关 → 粗筛召回有保证。
pub(crate) fn bits_of(v: &[f32]) -> Vec<u8> {
    let mut out = vec![0u8; v.len().div_ceil(8)];
    for (i, &x) in v.iter().enumerate() {
        if x >= 0.0 {
            out[i / 8] |= 1 << (i % 8);
        }
    }
    out
}

/// 两个等长二值码的汉明距离(位不同的个数)。
pub(crate) fn hamming(a: &[u8], b: &[u8]) -> u32 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x ^ y).count_ones())
        .sum()
}

/// 当前生效的嵌入模型标识(= provider.default_model)。用于 P2-2 版本隔离与查询缓存键。
pub fn active_embed_model() -> Option<String> {
    crate::sense::active_provider("embed").map(|p| p.default_model)
}

/// 是否**具备把文本变向量的能力** —— 本地开源嵌入(local-embed)**或**云 API 嵌入服务商。
/// `active_provider("embed")` 只认 `kind=api`+有 key 的云服务商,**不计本地档**;但本地档
/// (v1.4.2,bge-m3 ONNX)离线就能产向量。渐进式「智能归类」据此决定要不要跑「全量向量化 →
/// 按内容语义重聚」——只看云 key 会让纯本地用户永远停在结构归类、永远走不到「按意思」。
pub fn embed_capable() -> bool {
    #[cfg(feature = "local-embed")]
    if crate::fable::embed_local::enabled() {
        return true;
    }
    crate::sense::active_provider("embed").is_some()
}

// ───────────────────────── 查询嵌入缓存(P1-5)─────────────────────────

/// 极简 LRU:HashMap 存值 + VecDeque 记最近使用顺序。容量满时淘汰最久未用。
struct QueryCache {
    cap: usize,
    map: HashMap<String, Vec<f32>>,
    order: VecDeque<String>,
}
impl QueryCache {
    fn get(&mut self, k: &str) -> Option<Vec<f32>> {
        let v = self.map.get(k)?.clone();
        self.order.retain(|x| x != k);
        self.order.push_back(k.to_string());
        Some(v)
    }
    fn put(&mut self, k: String, v: Vec<f32>) {
        if self.map.insert(k.clone(), v).is_none() {
            self.order.push_back(k);
            while self.order.len() > self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
        } else {
            self.order.retain(|x| x != &k);
            self.order.push_back(k);
        }
    }
}
static QUERY_CACHE: Lazy<Mutex<QueryCache>> = Lazy::new(|| {
    Mutex::new(QueryCache {
        cap: 256,
        map: HashMap::new(),
        order: VecDeque::new(),
    })
});

/// 查询嵌入(P1-5):LRU 缓存命中直接返回**归一化**向量(高并发下重复查询零接口开销);
/// 未命中才打一次嵌入接口。失败上抛 —— 调用方按可降级处理(向量腿静默退场,grep/FTS 腿照常)。
pub fn embed_query(query: &str) -> Result<Vec<f32>, String> {
    let model = active_embed_model().unwrap_or_default();
    let key = format!("{model}\u{0}{query}");
    if let Some(v) = QUERY_CACHE.lock().unwrap().get(&key) {
        return Ok(v);
    }
    let mut v = match embed_texts(&[query.to_string()]) {
        Ok(vs) => vs.into_iter().next().ok_or("查询嵌入为空")?,
        Err(e) => {
            // 云嵌入失败(断网/限速/服务挂)→ 若本地模型已下载就位且当前非本地档(本地档失败再退本地
            // 无意义),退回本地 BGE-M3 现算。同 1024 维空间,兼容云建的既有索引 → 查询韧性不靠云。
            #[cfg(feature = "local-embed")]
            {
                if !crate::fable::embed_local::enabled() && crate::fable::embed_local::ready() {
                    crate::fable::embed_local::embed(&[query.to_string()])?
                        .into_iter()
                        .next()
                        .ok_or("查询嵌入为空")?
                } else {
                    return Err(e);
                }
            }
            #[cfg(not(feature = "local-embed"))]
            {
                return Err(e);
            }
        }
    };
    normalize(&mut v);
    QUERY_CACHE.lock().unwrap().put(key, v.clone());
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_splits_on_heading_and_prefixes_breadcrumb() {
        let doc = "# 合同总则\n\n本合同由甲乙双方签订,内容涵盖各项条款约定细则。\n\n\
                   ## 付款方式\n\n乙方应于每月五日前支付当期款项,逾期按日计息处理。";
        let cs = chunk_text(doc);
        assert_eq!(cs.len(), 2, "两个章节 → 硬边界切成两块");
        assert!(
            cs[0].starts_with("【合同总则】\n"),
            "首块带章节面包屑: {}",
            cs[0]
        );
        assert!(
            cs[1].starts_with("【合同总则 › 付款方式】\n"),
            "子节面包屑含父路径: {}",
            cs[1]
        );
        assert!(cs[1].contains("逾期按日计息"), "正文保留");
    }

    #[test]
    fn chunk_carries_page_marker_into_crumb_without_fragmenting() {
        // 页码标记只更新当前页、不断块;块以起始页标注。
        let doc = "[[page:3]]\n\n扫描件正文第一段落,足够长以越过二十四字最小长度门槛要求。\n\n\
                   继续同页的第二段落文字,仍旧属于同一物理页面之内容范围。";
        let cs = chunk_text(doc);
        assert_eq!(cs.len(), 1, "同页两段聚成一块,不因页标记碎裂");
        assert!(cs[0].starts_with("【p3】\n"), "块带起始页码: {}", cs[0]);
    }

    #[test]
    fn chunk_heading_plus_page_combined_crumb() {
        let doc = "[[page:5]]\n\n## 第二章 交付\n\n交付标准依照附件甲所列各项技术指标逐条进行验收并书面确认。";
        let cs = chunk_text(doc);
        assert_eq!(cs.len(), 1);
        assert!(
            cs[0].starts_with("【第二章 交付｜p5】\n"),
            "章节+页码合并面包屑: {}",
            cs[0]
        );
    }

    #[test]
    fn chunk_long_paragraph_hardsplit_keeps_crumb_and_overlap() {
        let long: String = "甲".repeat(4000); // 单段远超 TARGET
        let doc = format!("## 长章\n\n{long}");
        let cs = chunk_text(&doc);
        assert!(cs.len() >= 3, "4000 字应硬切多块: {}", cs.len());
        assert!(
            cs.iter().all(|c| c.starts_with("【长章】\n")),
            "每片都带面包屑"
        );
    }

    #[test]
    fn chunk_no_structure_still_works_without_crumb() {
        // 无标题无页码的纯文本:不加面包屑,行为等价旧「段落聚合」。
        let doc = "普通一段文本内容,没有任何标题或页码标记出现在其中任何位置处。";
        let cs = chunk_text(doc);
        assert_eq!(cs.len(), 1);
        assert!(!cs[0].starts_with("【"), "无结构不加面包屑: {}", cs[0]);
    }

    /// 建索引效果实测:POLARIS_DEMO_FILE=<真实文件> 走真实代码路径(convert → chunk_text),
    /// 打印总块数与前几块(含面包屑/页码标记),肉眼看新版结构感知分块在真实文档上的产出。
    /// 默认 #[ignore],手动跑:
    ///   cargo test --lib --features desktop chunk_build_index_demo -- --ignored --nocapture
    #[test]
    #[ignore]
    fn chunk_build_index_demo() {
        let Ok(path) = std::env::var("POLARIS_DEMO_FILE") else {
            eprintln!("设 POLARIS_DEMO_FILE=<文件路径> 后再跑");
            return;
        };
        let p = std::path::Path::new(&path);
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let text = if matches!(ext.as_str(), "md" | "txt" | "markdown") {
            String::from_utf8_lossy(&std::fs::read(p).expect("读文件")).into_owned()
        } else {
            crate::convert::convert_to_markdown(p)
                .expect("convert 失败")
                .expect("无可抽取文本")
        };
        let chunks = chunk_text(&text);
        eprintln!("\n════ {path}");
        eprintln!("原文 {} 字 → {} 块", text.chars().count(), chunks.len());
        let has_page = text.contains("[[page:");
        eprintln!("含PDF页码标记: {has_page}");
        for (i, c) in chunks.iter().take(4).enumerate() {
            let head: String = c.chars().take(160).collect();
            eprintln!("\n── 块#{i} ({}字) ──\n{head}", c.chars().count());
        }
        // 健壮性断言:无空块、无低于最小门槛的裸碎块。
        assert!(chunks.iter().all(|c| !c.trim().is_empty()), "不应有空块");
    }

    #[test]
    fn chunk_short_body_under_heading_still_emitted() {
        // 回归防护:标题点题、正文极短(<24字)时,靠面包屑补足成品长度 → 仍入向量,不丢语义召回。
        let doc = "## 违约责任\n\n见附件三。";
        let cs = chunk_text(doc);
        assert_eq!(cs.len(), 1, "短正文+章节面包屑应保留: {cs:?}");
        assert!(cs[0].starts_with("【违约责任】\n"));
        assert!(cs[0].contains("见附件三"));
        // 无面包屑的同等短正文仍按旧规则丢弃(纯噪声不入库)。
        assert!(chunk_text("见附件三。").is_empty());
    }

    #[test]
    fn atx_heading_requires_space_after_hashes() {
        assert_eq!(parse_atx_heading("## 标题"), Some((2, "标题".to_string())));
        assert_eq!(parse_atx_heading("#话题标签"), None, "无空白不是标题");
        assert_eq!(parse_atx_heading("####### 七级"), None, "超六级不是标题");
        assert_eq!(parse_page_marker("[[page:12]]"), Some(12));
        assert_eq!(parse_page_marker("正文"), None);
    }

    #[test]
    fn normalize_makes_unit_length() {
        let mut v = vec![3.0f32, 4.0];
        normalize(&mut v);
        let n = (v.iter().map(|x| x * x).sum::<f32>()).sqrt();
        assert!((n - 1.0).abs() < 1e-6);
        // 零向量不应除零崩溃,保持全零。
        let mut z = vec![0.0f32; 4];
        normalize(&mut z);
        assert!(z.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn bits_pack_and_hamming() {
        // 符号位:正/零 → 1,负 → 0。8 维正好 1 字节。
        let v = [1.0f32, -1.0, 2.0, -3.0, 0.0, -0.1, 5.0, -9.0];
        let b = bits_of(&v); // 位:1,0,1,0,1,0,1,0 → 0b01010101 = 0x55
        assert_eq!(b.len(), 1);
        assert_eq!(b[0], 0b0101_0101);
        // 自己跟自己汉明距离 0;翻转一位 → 距离 1。
        assert_eq!(hamming(&b, &b), 0);
        let mut b2 = b.clone();
        b2[0] ^= 0b0000_0001;
        assert_eq!(hamming(&b, &b2), 1);
        // 维度非 8 的整数倍:9 维 → 2 字节。
        assert_eq!(bits_of(&[0.0f32; 9]).len(), 2);
    }

    #[test]
    fn dot_blob_matches_blob_to_vec_path() {
        // dot_blob 必须与「blob_to_vec 后逐元素相乘求和」逐位一致(这是它替换的旧路径)。
        let qv = [0.1f32, -0.2, 0.3, 0.5, -0.7];
        let dv = [0.4f32, 0.4, -0.1, 0.2, 0.9];
        let blob = vec_to_blob(&dv);
        let want: f32 = blob_to_vec(&blob)
            .iter()
            .zip(qv.iter())
            .map(|(a, b)| a * b)
            .sum();
        let got = dot_blob(&qv, &blob).expect("维度一致应返回 Some");
        assert!((got - want).abs() < 1e-6, "got={got} want={want}");
        // 维度不符(脏数据/旧维度向量)→ None,调用方跳过而非误算。
        assert!(dot_blob(&qv, &vec_to_blob(&[1.0f32, 2.0])).is_none());
        assert!(dot_blob(&qv, &blob[..blob.len() - 1]).is_none()); // 截断字节
    }

    #[test]
    fn query_cache_lru_evicts_oldest() {
        let mut c = QueryCache {
            cap: 2,
            map: HashMap::new(),
            order: VecDeque::new(),
        };
        c.put("a".into(), vec![1.0]);
        c.put("b".into(), vec![2.0]);
        assert!(c.get("a").is_some()); // 访问 a → a 变最近
        c.put("c".into(), vec![3.0]); // 淘汰最久未用 = b
        assert!(c.get("b").is_none());
        assert!(c.get("a").is_some());
        assert!(c.get("c").is_some());
    }
}
