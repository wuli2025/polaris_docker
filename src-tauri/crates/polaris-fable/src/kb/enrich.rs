//! 自动补双链 kb_enrich_links + 合并去重 kb_dedup —— 自原 kb.rs 纯移动, 逻辑零改动。

use super::*;

// ───────────────────────── 自动补双链 (借鉴 llm_wiki enrich-wikilinks) ─────────────────────────
//
// 旗舰示范: 「让 AI 只动嘴, 代码动手」。只读 claude 读 wiki 页 + 候选标题, 返回
// `[{page, term, target}]` 链接建议; Rust 执行替换 —— 只替**首次出现**、跳过 frontmatter /
// 已链接 / 代码区, 正文一字不多改。模型物理上没有写权限, 从根上杜绝它改乱正文。

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KbEnrichEvent {
    pub run_id: String,
    pub kind: String, // phase | tool | delta | done | error
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied: Option<usize>,
}

#[derive(Deserialize)]
pub(crate) struct LinkSuggestion {
    page: String,
    term: String,
    target: String,
}

/// 纯函数: 在 `body` 中把 `term` 的**首次明文出现**替换为 `[[target]]`(或 `[[target|term]]`)。
/// 跳过: frontmatter 区、已有 `[[...]]` 内部、行内代码 `` `..` `` 与围栏代码块、已是双链的同名词。
/// 命中并替换返回 `Some(新正文)`; 没有可替换的明文出现返回 `None`(不改动)。
pub(crate) fn apply_wikilink(body: &str, term: &str, target: &str) -> Option<String> {
    if term.is_empty() {
        return None;
    }
    let chars: Vec<char> = body.chars().collect();
    let term_chars: Vec<char> = term.chars().collect();
    let n = chars.len();
    let tn = term_chars.len();

    // 定位 frontmatter 结束位置 (第二个 `---` 行之后), 之前的内容不动。
    let fm_end = frontmatter_end_char_idx(&chars);

    let mut i = fm_end;
    let mut in_fence = false; // ``` 围栏代码块
    let mut in_inline = false; // `..` 行内代码
    let mut link_depth = 0i32; // [[..]] 内
    let mut at_line_start = true;
    while i < n {
        // 围栏: 行首三连反引号切换
        if at_line_start
            && i + 2 < n
            && chars[i] == '`'
            && chars[i + 1] == '`'
            && chars[i + 2] == '`'
        {
            in_fence = !in_fence;
            i += 3;
            at_line_start = false;
            continue;
        }
        let c = chars[i];
        if c == '\n' {
            at_line_start = true;
            in_inline = false; // 行内代码不跨行
            i += 1;
            continue;
        }
        at_line_start = false;
        if !in_fence && c == '`' {
            in_inline = !in_inline;
            i += 1;
            continue;
        }
        if i + 1 < n && c == '[' && chars[i + 1] == '[' {
            link_depth += 1;
            i += 2;
            continue;
        }
        if i + 1 < n && c == ']' && chars[i + 1] == ']' && link_depth > 0 {
            link_depth -= 1;
            i += 2;
            continue;
        }
        // 命中明文 term?
        if !in_fence
            && !in_inline
            && link_depth == 0
            && i + tn <= n
            && chars[i..i + tn] == term_chars[..]
        {
            // 前一个非空白字符不能是 `[`(避免 [[ 紧邻) — link_depth 已挡住, 这里再防 `[term`
            let prev_ok = i == 0 || chars[i - 1] != '[';
            if prev_ok {
                let replacement = if term == target {
                    format!("[[{target}]]")
                } else {
                    format!("[[{target}|{term}]]")
                };
                let mut out = String::new();
                out.extend(chars[..i].iter());
                out.push_str(&replacement);
                out.extend(chars[i + tn..].iter());
                return Some(out);
            }
        }
        i += 1;
    }
    None
}

/// 返回 frontmatter 之后正文起始的字符下标 (无 frontmatter 则 0)。
pub(crate) fn frontmatter_end_char_idx(chars: &[char]) -> usize {
    // 必须以 `---\n` 开头
    if chars.len() < 4 || chars[0] != '-' || chars[1] != '-' || chars[2] != '-' {
        return 0;
    }
    // 找第二个独占一行的 `---`
    let mut i = 0;
    let mut line_start = 0;
    let mut seen_first = false;
    while i < chars.len() {
        if chars[i] == '\n' {
            let line: String = chars[line_start..i].iter().collect();
            if line.trim() == "---" {
                if seen_first {
                    return i + 1; // 第二个 --- 行的换行之后
                }
                seen_first = true;
            }
            line_start = i + 1;
        }
        i += 1;
    }
    0
}

/// 「自动补双链」: 只读 claude 给出 `[{page,term,target}]` 建议, Rust 执行替换。
/// 立即返回 run_id; 进度走 `kb:enrich` 事件, 完成发 `done` (附实际应用条数)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn kb_enrich_links(app: AppHandle) -> Result<String, String> {
    let root = KB_ROOT.read().clone();
    if root.as_os_str().is_empty() || !root.exists() {
        return Err("知识库根目录不存在".into());
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let c = KB_COMPILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let run_id = format!("kbe-{:x}-{:x}", ts, c);

    // 候选目标 = 现有 wiki 页标题清单 (供模型选择链接到哪)
    let titles: Vec<String> = {
        let idx = INDEX.read();
        idx.iter()
            .filter(|d| {
                d.rel_path.starts_with("wiki/")
                    && !is_wiki_meta_page(&d.rel_path.replace('\\', "/"))
            })
            .map(|d| d.title.clone())
            .filter(|t| t.chars().count() >= 2)
            .collect()
    };
    if titles.is_empty() {
        return Err("wiki/ 暂无可链接的页面, 请先构建知识网".into());
    }

    let _kb_task = acquire_kb_task()?;
    let run_id_t = run_id.clone();
    std::thread::spawn(move || {
        let _kb_task = _kb_task; // 持锁直到本线程结束(Drop 释放)
        let emit = |kind: &str, text: Option<String>, applied: Option<usize>| {
            let _ = app.emit(
                "kb:enrich",
                KbEnrichEvent {
                    run_id: run_id_t.clone(),
                    kind: kind.into(),
                    text,
                    applied,
                },
            );
        };
        emit(
            "phase",
            Some("分析 wiki 页面、寻找可补的双链…".into()),
            None,
        );

        let vocab = titles.join("\n");
        let prompt = format!(
            "# 任务: 为知识库 wiki 找出应补的双链 (只输出 JSON, 不要改任何文件)\n\n\
你的工作目录是知识库根。下面是 wiki/ 现有页面的**标题清单**(可作为双链目标):\n\n{vocab}\n\n\
请用 Read/Glob/Grep 浏览 `wiki/` 下的内容页 (跳过 index.md 与各 _index.md), 找出正文里\
**以纯文本形式出现、但还没做成 `[[双链]]`** 的术语, 且该术语正好等于(或非常接近)上面清单里的某个标题。\n\n\
## 输出 (严格)\n\
只输出一个 JSON 数组, 每项形如 `{{\"page\": \"wiki/概念/x.md\", \"term\": \"正文里出现的词\", \"target\": \"清单里的目标标题\"}}`。\n\
- term 必须是该 page 正文里**逐字出现**的子串。\n\
- target 必须是上面清单里的标题之一。\n\
- 同一 page 同一 term 只给一条。最多 80 条。\n\
- **不要写入或修改任何文件**, 不要输出 JSON 以外的任何解释文字。\n\n\
现在开始, 直接输出 JSON 数组。"
        );

        let raw = match run_claude_readonly(&root, &prompt, |kind, text| {
            if kind == "tool" {
                emit("tool", Some(text.to_string()), None);
            } else if kind == "delta" && !text.is_empty() {
                emit("delta", Some(text.chars().take(80).collect()), None);
            }
        }) {
            Ok(r) => r,
            Err(e) => {
                emit("error", Some(e), None);
                emit("done", Some("补链未完成".into()), Some(0));
                return;
            }
        };

        let suggestions: Vec<LinkSuggestion> = extract_balanced_json(&raw)
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();
        emit(
            "phase",
            Some(format!("收到 {} 条建议, 代码执行替换…", suggestions.len())),
            None,
        );

        // 现存 wiki 标题集 (校验 target 合法)
        let valid_targets: std::collections::HashSet<String> = {
            let idx = INDEX.read();
            idx.iter()
                .filter(|d| d.rel_path.starts_with("wiki/"))
                .map(|d| d.title.clone())
                .collect()
        };

        // 按 page 聚合, 逐页一次性读写, 顺序应用其建议 (每条改首次出现)。
        use std::collections::BTreeMap;
        let mut by_page: BTreeMap<String, Vec<LinkSuggestion>> = BTreeMap::new();
        for s in suggestions {
            by_page
                .entry(s.page.replace('\\', "/"))
                .or_default()
                .push(s);
        }

        let mut applied = 0usize;
        for (page, sugs) in by_page {
            // 安全: page 必须是 wiki/ 下合法路径且文件存在
            if is_safe_wiki_relpath(&page).is_err() {
                continue;
            }
            let full = root.join(&page);
            let Ok(mut content) = fs::read_to_string(&full) else {
                continue;
            };
            let mut changed = false;
            for s in sugs {
                if !valid_targets.contains(&s.target) {
                    continue;
                }
                if let Some(updated) = apply_wikilink(&content, &s.term, &s.target) {
                    content = updated;
                    changed = true;
                    applied += 1;
                }
            }
            if changed {
                if kb_atomic_write(&full, &content).is_ok() {
                    emit(
                        "phase",
                        Some(format!(
                            "已补链: {}",
                            page.rsplit('/').next().unwrap_or(&page)
                        )),
                        None,
                    );
                }
            }
        }

        // 重扫刷新索引/图谱
        let docs = scan_all(&root);
        *INDEX.write() = docs;
        emit(
            "done",
            Some(format!("补链完成: 共应用 {applied} 处双链")),
            Some(applied),
        );
    });

    Ok(run_id)
}

// ───────────────────────── 智能去重 (借鉴 llm_wiki dedup + page-merge) ─────────────────────────
//
// 「摄入即编译」反复跑会写出同主题的多篇页面, 越积越乱。借鉴 llm_wiki 两段式:
// ① 规则粗筛 (按归一化标题分组, 便宜) → ② 只读 claude 细判 (真重复? 谁当主页? confidence)
// → ③ Rust 执行合并: **锁定主页 type/title/created**, 把重复页正文并入主页(不丢知识),
//    重写全库 `[[重复页]]` 双链指向主页, 删重复页文件 + 清 index 条目。

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KbDedupEvent {
    pub run_id: String,
    pub kind: String, // phase | tool | delta | done | error
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged: Option<usize>,
}

#[derive(Deserialize)]
pub(crate) struct DedupVerdict {
    #[serde(default)]
    duplicate: bool,
    #[serde(default)]
    confidence: String,
    #[serde(default)]
    canonical: String,
    #[serde(default)]
    pages: Vec<String>,
}

/// 归一化标题: 小写 + 去空白与常见标点 —— 用于规则粗筛分组。
pub(crate) fn normalize_title(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && !"-_()（）[]【】·.,，。:：、/\\|".contains(*c))
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// 重写正文里指向 `from` 标题的双链, 改为指向 `to`, 保留别名/锚点后缀。
/// `[[from]]` → `[[to]]`; `[[from|别名]]` → `[[to|别名]]`; `[[from#节]]` → `[[to#节]]`。大小写不敏感匹配 from。
pub(crate) fn rewrite_wikilink_target(body: &str, from: &str, to: &str) -> String {
    let from_lc = from.to_lowercase();
    let mut out = String::with_capacity(body.len());
    let chars: Vec<char> = body.chars().collect();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        if i + 1 < n && chars[i] == '[' && chars[i + 1] == '[' {
            // 找到匹配的 ]]
            if let Some(close) = find_link_close(&chars, i + 2) {
                let inner: String = chars[i + 2..close].iter().collect();
                // 拆 target | alias / target # sec
                let (target, suffix) = split_link_inner(&inner);
                if target.trim().to_lowercase() == from_lc {
                    out.push_str("[[");
                    out.push_str(to);
                    out.push_str(&suffix);
                    out.push_str("]]");
                    i = close + 2;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// 从 `start` 起找 `]]` 的起始下标 (不跨越下一个 `[[`)。
pub(crate) fn find_link_close(chars: &[char], start: usize) -> Option<usize> {
    let n = chars.len();
    let mut i = start;
    while i + 1 < n {
        if chars[i] == ']' && chars[i + 1] == ']' {
            return Some(i);
        }
        if chars[i] == '[' && chars[i + 1] == '[' {
            return None; // 嵌套/未闭合, 放弃
        }
        i += 1;
    }
    None
}

/// 把 `[[inner]]` 的内部拆成 (目标, 后缀)。后缀含分隔符, 如 `|别名` 或 `#节`。
pub(crate) fn split_link_inner(inner: &str) -> (String, String) {
    if let Some(p) = inner.find(['|', '#']) {
        (inner[..p].to_string(), inner[p..].to_string())
    } else {
        (inner.to_string(), String::new())
    }
}

/// 「智能去重」: 规则粗筛 + 只读 claude 细判 + Rust 合并。
/// 立即返回 run_id; 进度走 `kb:dedup`, 完成发 `done` (附合并页数)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn kb_dedup(app: AppHandle) -> Result<String, String> {
    let root = KB_ROOT.read().clone();
    if root.as_os_str().is_empty() || !root.exists() {
        return Err("知识库根目录不存在".into());
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let c = KB_COMPILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let run_id = format!("kbd-{:x}-{:x}", ts, c);

    // 规则粗筛: 按归一化标题分组, 取 size≥2 的组 (附路径/标题/正文片段供 claude 判断)
    let groups: Vec<Vec<(String, String, String)>> = {
        let idx = INDEX.read();
        let mut by_norm: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
        for d in idx.iter() {
            let rp = d.rel_path.replace('\\', "/");
            if !rp.starts_with("wiki/") || is_wiki_meta_page(&rp) {
                continue;
            }
            let snippet: String = read_doc_body(&d.rel_path)
                .map(|b| b.trim().chars().take(160).collect())
                .unwrap_or_default();
            by_norm.entry(normalize_title(&d.title)).or_default().push((
                rp,
                d.title.clone(),
                snippet,
            ));
        }
        by_norm.into_values().filter(|g| g.len() >= 2).collect()
    };

    if groups.is_empty() {
        return Err("规则粗筛未发现疑似重复页 (标题归一化后无碰撞)".into());
    }

    let _kb_task = acquire_kb_task()?;
    let run_id_t = run_id.clone();
    std::thread::spawn(move || {
        let _kb_task = _kb_task; // 持锁直到本线程结束(Drop 释放)
        let emit = |kind: &str, text: Option<String>, merged: Option<usize>| {
            let _ = app.emit(
                "kb:dedup",
                KbDedupEvent {
                    run_id: run_id_t.clone(),
                    kind: kind.into(),
                    text,
                    merged,
                },
            );
        };
        emit(
            "phase",
            Some(format!(
                "规则粗筛出 {} 组疑似重复, 请 AI 细判…",
                groups.len()
            )),
            None,
        );

        // 拼候选清单给 claude
        let mut cand = String::new();
        for (gi, g) in groups.iter().enumerate() {
            cand.push_str(&format!("## 组 {gi}\n"));
            for (rp, title, snip) in g {
                cand.push_str(&format!("- `{rp}` | 标题: {title} | 摘要: {snip}\n"));
            }
            cand.push('\n');
        }
        let prompt = format!(
            "# 任务: 判断这些 wiki 页是否真重复 (只输出 JSON, 不要改任何文件)\n\n\
下面是按标题相似度粗筛出的若干**疑似重复组**(每组列了路径/标题/正文摘要)。\
必要时可用 Read 打开页面看全文再判断。\n\n{cand}\n\
## 输出 (严格)\n\
只输出一个 JSON 数组, 每组一项: \
`{{\"pages\": [\"组内全部路径\"], \"duplicate\": true/false, \"confidence\": \"high|medium|low\", \"canonical\": \"应保留为主页的路径\", \"reason\": \"一句话\"}}`。\n\
- 仅当确属讲同一事物的重复页才标 duplicate=true。\n\
- canonical 选内容最全/质量最好的那篇, 必须是该组 pages 之一。\n\
- **不要写入或修改任何文件**, 不要输出 JSON 以外的解释。\n\n\
现在开始, 直接输出 JSON 数组。"
        );

        let raw = match run_claude_readonly(&root, &prompt, |kind, text| {
            if kind == "tool" {
                emit("tool", Some(text.to_string()), None);
            }
        }) {
            Ok(r) => r,
            Err(e) => {
                emit("error", Some(e), None);
                emit("done", Some("去重未完成".into()), Some(0));
                return;
            }
        };

        let verdicts: Vec<DedupVerdict> = extract_balanced_json(&raw)
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();
        emit(
            "phase",
            Some("AI 判定完成, 代码执行合并…".to_string()),
            None,
        );

        let mut merged = 0usize;
        for v in verdicts {
            if !v.duplicate || v.confidence.eq_ignore_ascii_case("low") {
                continue; // 保守: 低置信不动
            }
            let canonical = v.canonical.replace('\\', "/");
            if is_safe_wiki_relpath(&canonical).is_err() || !root.join(&canonical).exists() {
                continue;
            }
            for dup in v.pages.iter().map(|p| p.replace('\\', "/")) {
                if dup == canonical {
                    continue;
                }
                if is_safe_wiki_relpath(&dup).is_err() || !root.join(&dup).exists() {
                    continue;
                }
                if merge_duplicate_page(&root, &canonical, &dup).is_ok() {
                    merged += 1;
                    emit(
                        "phase",
                        Some(format!(
                            "已合并 {} → {}",
                            dup.rsplit('/').next().unwrap_or(&dup),
                            canonical.rsplit('/').next().unwrap_or(&canonical)
                        )),
                        None,
                    );
                }
            }
        }

        let docs = scan_all(&root);
        *INDEX.write() = docs;
        emit(
            "done",
            Some(format!("去重完成: 合并 {merged} 个重复页")),
            Some(merged),
        );
    });

    Ok(run_id)
}

/// 把重复页 `dup` 合并进主页 `canonical` (路径均为 KB 相对、已校验存在):
/// ① 把 dup 正文并入 canonical 末尾「合并自」区 (不丢知识); 主页 frontmatter 原样保留(锁定 type/title/created)。
/// ② 全库重写 `[[dup标题]]` → `[[canonical标题]]`。
/// ③ 删 dup 文件, 清 wiki/index.md 里指向 dup 的行。
pub(crate) fn merge_duplicate_page(root: &Path, canonical: &str, dup: &str) -> Result<(), String> {
    let stem = |rp: &str| -> String {
        let n = rp.replace('\\', "/");
        let base = n.rsplit('/').next().unwrap_or(&n).to_string();
        base.strip_suffix(".md")
            .or_else(|| base.strip_suffix(".markdown"))
            .unwrap_or(&base)
            .to_string()
    };
    // 标题取 INDEX 里的 title, 回退到文件名 stem
    let title_of = |rp: &str| -> String {
        let idx = INDEX.read();
        idx.iter()
            .find(|d| d.rel_path.replace('\\', "/") == rp)
            .map(|d| d.title.clone())
            .unwrap_or_else(|| stem(rp))
    };
    let canon_title = title_of(canonical);
    let dup_title = title_of(dup);

    let dup_full = root.join(dup);
    let canon_full = root.join(canonical);
    let dup_body = fs::read_to_string(&dup_full).map_err(|e| e.to_string())?;
    // 剥掉 dup 的 frontmatter, 只并正文
    let dup_content = RE_FRONTMATTER.replace(&dup_body, "").trim().to_string();

    // ① 并入主页末尾 (主页 frontmatter 不动 → 锁定 type/title/created)
    let mut canon_body = fs::read_to_string(&canon_full).map_err(|e| e.to_string())?;
    if !canon_body.ends_with('\n') {
        canon_body.push('\n');
    }
    canon_body.push_str(&format!(
        "\n<!-- 合并自 {dup} (kb_dedup) -->\n## (并入) {dup_title}\n\n{dup_content}\n"
    ));
    kb_atomic_write(&canon_full, &canon_body).map_err(|e| e.to_string())?;

    // ② 全库重写双链 [[dup_title]] → [[canon_title]]
    if !dup_title.eq_ignore_ascii_case(&canon_title) {
        for entry in WalkDir::new(root.join("wiki")).into_iter().flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
            if ext != "md" && ext != "markdown" {
                continue;
            }
            if p == canon_full || p == dup_full {
                continue; // dup 即将删除; canon 末尾刚并入, 不必自指重写
            }
            if let Ok(content) = fs::read_to_string(p) {
                if content.contains(&format!("[[{dup_title}")) {
                    let rewritten = rewrite_wikilink_target(&content, &dup_title, &canon_title);
                    if rewritten != content {
                        let _ = kb_atomic_write(p, &rewritten);
                    }
                }
            }
        }
    }

    // ③ 删 dup 文件 + 清 index.md 里指向 dup 的行
    fs::remove_file(&dup_full).map_err(|e| e.to_string())?;
    let index_md = root.join("wiki").join("index.md");
    if let Ok(idx_content) = fs::read_to_string(&index_md) {
        let needle_link = format!("[[{dup_title}]]");
        let needle_path = dup;
        let kept: Vec<&str> = idx_content
            .lines()
            .filter(|ln| !(ln.contains(&needle_link) || ln.contains(needle_path)))
            .collect();
        let new_idx = kept.join("\n");
        if new_idx != idx_content {
            let _ = kb_atomic_write(&index_md, &new_idx);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrich_links_first_occurrence_skips_code_and_existing() {
        // 首次出现替换为带别名的双链
        let body = "# 标题\n\n马克思主义是核心。再提马克思主义。\n";
        let out = apply_wikilink(body, "马克思主义", "马克思主义").unwrap();
        assert!(out.contains("[[马克思主义]]是核心"));
        // 只替首次: 第二处仍是纯文本
        assert_eq!(out.matches("[[马克思主义]]").count(), 1);
        assert!(out.contains("再提马克思主义"));
    }

    #[test]
    fn enrich_links_alias_when_term_differs() {
        let body = "讨论了实践论的要点。";
        let out = apply_wikilink(body, "实践论", "实践论(著作)").unwrap();
        assert!(out.contains("[[实践论(著作)|实践论]]"));
    }

    #[test]
    fn enrich_links_skips_frontmatter_and_already_linked() {
        // frontmatter 里的同名词不动
        let body = "---\ntitle: 矛盾论\ntype: concept\n---\n\n正文提到矛盾论。";
        let out = apply_wikilink(body, "矛盾论", "矛盾论").unwrap();
        assert!(out.contains("title: 矛盾论")); // frontmatter 未被改
        assert!(out.contains("正文提到[[矛盾论]]"));

        // 已是双链则不再重复包裹
        let linked = "已经有 [[矛盾论]] 了。";
        assert!(apply_wikilink(linked, "矛盾论", "矛盾论").is_none());
    }

    #[test]
    fn enrich_links_skips_inline_code() {
        let body = "用 `kb_compile` 命令构建。";
        assert!(apply_wikilink(body, "kb_compile", "kb_compile").is_none());
    }

    #[test]
    fn normalize_title_collapses_punctuation_and_case() {
        assert_eq!(normalize_title("矛盾论"), normalize_title("矛盾论 "));
        assert_eq!(
            normalize_title("On Practice"),
            normalize_title("on  practice")
        );
        assert_eq!(normalize_title("实践-论(草)"), normalize_title("实践论草"));
    }

    #[test]
    fn rewrite_wikilink_target_keeps_alias_and_section() {
        let body = "见 [[旧页]] 和 [[旧页|别名]] 与 [[旧页#某节]], 但 [[别的页]] 不动。";
        let out = rewrite_wikilink_target(body, "旧页", "新页");
        assert!(out.contains("[[新页]]"));
        assert!(out.contains("[[新页|别名]]"));
        assert!(out.contains("[[新页#某节]]"));
        assert!(out.contains("[[别的页]]")); // 未匹配的保持原样
        assert!(!out.contains("[[旧页"));
    }
}
