use super::*;

// ───────────────────────── 「让 AI 更懂你」桌面画像 ─────────────────────────
//
// 引导流程收尾:盘点 + 归类 + 索引跑完后,根据 fable.db 现有统计(类型分布 / 语义主题 / 体量)
// **确定性地**生成一张自包含 HTML「知识画像」落到桌面 —— 不调大模型,秒级、必成、可离线打开。
// 让用户直观看到「AI 已经大概懂我了」:你有什么、AI 怎么理解、接下来能替你做什么。

pub(crate) fn human_bytes(b: u64) -> String {
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

pub(crate) fn pf_kind_label(k: &str) -> &'static str {
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

pub(crate) fn kind_color(k: &str) -> &'static str {
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
pub(crate) struct WorkflowHint {
    title: String,
    detail: String,
}

/// 据类型分布派生建议工作流(命中阈值才给,避免无中生有)。
pub(crate) fn workflow_hints(ov: &FileOverview) -> Vec<WorkflowHint> {
    let cnt = |k: &str| -> u64 {
        ov.by_kind
            .iter()
            .find(|x| x.kind == k)
            .map(|x| x.count)
            .unwrap_or(0)
    };
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
            detail: format!(
                "你有 {} 个压缩包。我可以梳理里面有什么,把有用的内容归进资源库。",
                cnt("archive")
            ),
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
pub(crate) fn understanding_lines(ov: &FileOverview) -> Vec<String> {
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
        out.push(
            "文本的语义索引正在后台建,建好后我就能按意思(而不只是文件名)帮你找东西了。".into(),
        );
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
pub(crate) fn recent_parent_label(relpath: &str) -> String {
    let norm = relpath.replace('\\', "/");
    let segs: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() <= 1 {
        return "(根目录)".to_string();
    }
    let dirs = &segs[..segs.len() - 1]; // 去掉文件名本身
    let tail = if dirs.len() > 2 {
        &dirs[dirs.len() - 2..]
    } else {
        dirs
    };
    tail.join("/")
}

/// 秒差 → 中文相对时间(让「最近在动」一眼可读)。
pub(crate) fn rel_time_cn(secs: i64) -> String {
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
pub(crate) fn recent_activity_digest(root: &Option<String>) -> String {
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
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
        ))
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
            Agg {
                count: 0,
                newest: mtime,
                examples: Vec::new(),
            }
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
pub(crate) fn suggest_workflows_directive(ov: &FileOverview, recent: &str) -> String {
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
        kinds = if kinds.is_empty() {
            "—".into()
        } else {
            kinds.join("、")
        },
        langs = if langs.is_empty() {
            "—".into()
        } else {
            langs.join("、")
        },
        themes = themes,
        recent = if recent.trim().is_empty() {
            "(暂无近期改动记录)\n"
        } else {
            recent
        },
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
            let cwd = if kb_root.exists() {
                kb_root
            } else {
                std::env::temp_dir()
            };
            crate::kb::run_claude_readonly(&cwd, &prompt, |_k, _t| {})?
        };
        let raw =
            crate::kb::extract_balanced_json(&collected).ok_or("模型没有返回可解析的 JSON")?;
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
pub(crate) fn profile_html(root: Option<String>) -> Result<String, String> {
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
        themes.push_str(
            r#"<span class="theme dim">还没归主题 —— 在文件中心点「智能归类」即可</span>"#,
        );
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
