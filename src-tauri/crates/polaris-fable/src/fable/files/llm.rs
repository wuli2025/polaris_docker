use super::*;

// ───────────────────────── 大模型语义归类(免嵌入 key) ─────────────────────────
//
// 用户没有硅基嵌入 key,但聊天大模型(claude/配置的供应商)已经接通 → 直接让它读
// 文件清单、按主题归类,Rust 写回 cluster_id + clusters 表。
// 复用回声层「做梦」同一套:run_claude_readonly(无头跑已连接模型)+ extract_balanced_json。

// ── 归类专用模型(可选):独立于「对话供应商」,可指向便宜/免费的 OpenAI 兼容端点 ──
// 不配 → AI 归类沿用你聊天那个大模型;配了 → 走这个(例:硅基流动免费对话模型,省钱)。

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ClusterModelCfg {
    #[serde(default)]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) base_url: String,
    #[serde(default)]
    pub(crate) api_key: String,
    #[serde(default)]
    pub(crate) model: String,
}

pub(crate) fn cluster_model_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join("cluster_model.json"))
}
pub(crate) fn load_cluster_model() -> ClusterModelCfg {
    cluster_model_path()
        .filter(|p| p.exists())
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}
pub(crate) fn save_cluster_model(cfg: &ClusterModelCfg) -> Result<(), String> {
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
pub(crate) fn active_cluster_model() -> Option<ClusterModelCfg> {
    let c = load_cluster_model();
    let ok = c.enabled
        && !c.api_key.trim().is_empty()
        && !c.base_url.trim().is_empty()
        && !c.model.trim().is_empty();
    ok.then_some(c)
}
/// OpenAI 兼容 chat completion(硅基流动 / 任意兼容端点)。
pub(crate) fn chat_complete(cfg: &ClusterModelCfg, prompt: &str) -> Result<String, String> {
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
            let v: Value = r
                .into_json()
                .map_err(|e| format!("归类模型响应解析失败: {e}"))?;
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
pub(crate) fn cluster_model_view(c: &ClusterModelCfg) -> ClusterModelView {
    ClusterModelView {
        enabled: c.enabled,
        base_url: c.base_url.clone(),
        model: c.model.clone(),
        key_set: !c.api_key.trim().is_empty(),
    }
}

pub(crate) static LLM_CLUSTERING: AtomicBool = AtomicBool::new(false);

/// 喂给大模型的文件清单上限(控上下文;超出按 mtime 倒序取最近的)。
pub(crate) const LLM_FILE_CAP: usize = 240;

pub(crate) struct FileLite {
    id: i64,
    relpath: String,
    name: String,
    kind: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LlmGroup {
    #[serde(default)]
    label: String,
    #[serde(default)]
    files: Vec<Value>,
    /// 两级归类:本组若是「大主题」,其下的子主题放这里(子主题再各自带 files)。
    #[serde(default)]
    groups: Vec<LlmGroup>,
}

pub(crate) fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub(crate) fn emit_llm(app: &AppHandle, payload: Value) {
    let _ = app.emit("file:cluster_llm", payload);
}

/// 加载范围内文件(mtime 倒序,上限 LLM_FILE_CAP)给大模型归类。
pub(crate) fn load_files_for_llm(
    conn: &rusqlite::Connection,
    filter: &str,
) -> Result<Vec<FileLite>, String> {
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

pub(crate) fn llm_cluster_directive(files: &[FileLite]) -> String {
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
pub(crate) fn resolve_index(v: &Value, files: &[FileLite]) -> Option<usize> {
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

pub(crate) fn cluster_llm_run(
    app: &AppHandle,
    root: Option<String>,
) -> Result<(usize, usize, String), String> {
    // 与数学聚类共用 clusters 表重建闸:两条路都是「删全表→重建」,并发时 rowid 复用
    // 会把一方的文件指派静默挂到另一方的新簇(见 cluster.rs::CLUSTERS_TABLE_WRITE)。
    let Some(_rebuild) =
        crate::fable::FlagGuard::acquire(&super::cluster::CLUSTERS_TABLE_WRITE)
    else {
        return Err("另一个归类任务正在重建簇表,稍后再试".into());
    };
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
        let cwd = if kb_root.exists() {
            kb_root
        } else {
            std::env::temp_dir()
        };
        // 墙钟超时:无超时版挂死会把 CLUSTERS_TABLE_WRITE 一类一次性闸永久钉死(归类此后一直拒绝)。
        crate::kb::run_claude_readonly_timeout(
            &cwd,
            &prompt,
            |kind, _text| {
                if kind == "delta" {
                    emit_llm(app, json!({ "kind": "tick" })); // 心跳,不外泄正文
                }
            },
            std::time::Duration::from_secs(600),
        )?
    };
    let raw = crate::kb::extract_balanced_json(&collected)
        .ok_or("大模型没有返回可解析的 JSON(可换更强的模型,或稍后重试)")?;
    let groups: Vec<LlmGroup> =
        serde_json::from_str(&raw).map_err(|e| format!("归类 JSON 解析失败: {e}"))?;

    emit_llm(app, json!({ "kind": "phase", "text": "写回归类…" }));

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

    // ── 原子换代:删旧簇 + 清 cluster_id + 写回新归类放进【同一个】事务 ──
    // 旧实现先在事务外自动提交三条 DELETE/UPDATE,随后才 BEGIN 插新簇;中途任一 INSERT
    // 失败或进程被杀 → 新簇被回滚而旧簇已永久删除,归类一夜清零。现在成败一体:要么旧
    // 归类原样保留,要么新归类完整落地。(本路径挂簇的文件数受大模型清单上限约束,量级
    // 小,单事务即可,不需要 cluster.rs 那种几十万行的分批指派。)
    conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
    let txn_res: Result<(), String> = (|| {
        // 清旧簇(范围内)+ 旧关系边(簇 id 即将重排)。
        if ids.is_empty() {
            conn.execute("DELETE FROM clusters", []).ok();
            conn.execute("DELETE FROM cluster_edges", []).ok();
            conn.execute("UPDATE files SET cluster_id=0", []).ok();
        } else {
            let inlist: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
            let inlist = inlist.join(",");
            conn.execute(
                &format!("DELETE FROM clusters WHERE root_id IN ({inlist})"),
                [],
            )
            .ok();
            conn.execute(
                &format!("DELETE FROM cluster_edges WHERE root_id IN ({inlist})"),
                [],
            )
            .ok();
            conn.execute(
                &format!("UPDATE files SET cluster_id=0 WHERE root_id IN ({inlist})"),
                [],
            )
            .ok();
        }

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
                .query_row(
                    "SELECT root_id FROM files WHERE id=?1",
                    [files[children[0].1[0]].id],
                    |r| r.get(0),
                )
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
                    .query_row(
                        "SELECT root_id FROM files WHERE id=?1",
                        [files[m[0]].id],
                        |r| r.get(0),
                    )
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
                        stmt.execute(rusqlite::params![cid, files[i].id])
                            .map_err(|e| e.to_string())?;
                    }
                }
                assigned += m.len();
                n_clusters += 1;
            }
        }
        Ok(())
    })();
    if let Err(e) = txn_res {
        // 失败整体回滚:旧归类原样保留;显式 ROLLBACK 不留悬挂事务。
        let _ = conn.execute_batch("ROLLBACK");
        return Err(e);
    }
    if let Err(e) = conn.execute_batch("COMMIT") {
        let _ = conn.execute_batch("ROLLBACK");
        return Err(e.to_string());
    }

    Ok((n_clusters, assigned, String::new()))
}

// ───────────────── 文件中心 v3 · 簇画像 + 大模型命名/关系(读懂全库,不止 240) ─────────────────
//
// 核心洞察:大模型不读「文件」,读「向量聚类后的簇画像」—— 成本与文件数脱钩,几万文件也只看几十段
// 摘要。[`cluster_build`](全量、无 240 上限)先把库分成簇,本段让模型给每个簇起**亲切的人话名**
// +一句**温暖的概括** + 推断**簇间关系**。既覆盖全量,又让用户觉得「它很懂我」。

pub(crate) fn kind_cn(k: &str) -> &'static str {
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
pub(crate) fn loose_i64(v: &Value) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    v.as_str().and_then(|s| s.trim().parse::<i64>().ok())
}

/// 一个簇的「画像」:喂给大模型命名/关系用。纯结构信号 + 代表文件名,**不抽 gist、不读盘**,
/// 故秒级可成 —— T1 能快速出第一波。
pub(crate) struct ClusterDigest {
    pub(crate) id: i64,
    pub(crate) parent: i64,
    pub(crate) label: String,
    pub(crate) keywords: String,
    pub(crate) size: i64,
    pub(crate) folders: Vec<String>,
    pub(crate) samples: Vec<String>,
    pub(crate) kinds: Vec<(String, usize)>,
}

/// 为范围内**每个簇**(大主题 + 子主题)生成画像。大主题的样本取自旗下子簇文件。
pub(crate) fn collect_cluster_digests(
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
        let sql = format!(
            "SELECT id, parent, label, keywords, size FROM clusters{cfilter} ORDER BY size DESC"
        );
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
        let inlist: String = leaf_ids
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
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
pub(crate) fn digest_directive(digests: &[ClusterDigest]) -> String {
    let mut body = String::new();
    for d in digests {
        let role = if d.parent == 0 {
            "大主题"
        } else {
            "子主题"
        };
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
            let ks: Vec<String> = d
                .kinds
                .iter()
                .take(4)
                .map(|(k, n)| format!("{}×{}", kind_cn(k), n))
                .collect();
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
pub(crate) struct LlmNameRel {
    #[serde(default)]
    pub(crate) names: Vec<LlmName>,
    #[serde(default)]
    pub(crate) relations: Vec<LlmRel>,
    /// 「按意思合并」:每组是一串簇 id,模型认为它们其实是同一类东西、只是被文件夹/命名拆开了
    /// (发票/invoices/报销单 → 一类)。服务端只接受**同父叶簇**的合并(同层、同主题旗下),
    /// 防把跨主题的簇乱并;并入后该组文件改挂最大簇,余簇删除。见 apply_names_and_relations。
    #[serde(default)]
    merges: Vec<Vec<Value>>,
}
#[derive(Debug, Deserialize)]
pub(crate) struct LlmName {
    #[serde(default)]
    pub(crate) id: Value,
    #[serde(default)]
    name: String,
    #[serde(default)]
    summary: String,
}
#[derive(Debug, Deserialize)]
pub(crate) struct LlmRel {
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
pub(crate) fn apply_names_and_relations(
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
            let mut stmt = conn
                .prepare("SELECT id, parent, size FROM clusters")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })
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
            if !g
                .iter()
                .all(|id| pmap.get(id).copied().unwrap_or(-2) == par)
            {
                continue;
            }
            // survivor = 组内最大簇(size 最大;并列取最小 id,确定性)。
            g.sort_by(|a, b| smap.get(b).cmp(&smap.get(a)).then(a.cmp(b)));
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
            let Some(id0) = loose_i64(&n.id) else {
                continue;
            };
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
            up.execute(rusqlite::params![name, summary, id])
                .map_err(|e| e.to_string())?;
            renamed += 1;
        }
    }
    // 清范围内旧关系边,再重建(幂等)。
    if ids.is_empty() {
        conn.execute("DELETE FROM cluster_edges", []).ok();
    } else {
        let inlist: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        conn.execute(
            &format!(
                "DELETE FROM cluster_edges WHERE root_id IN ({})",
                inlist.join(",")
            ),
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
            if a == b
                || !work_valid.contains(&a)
                || !work_valid.contains(&b)
                || !seen.insert((a, b))
            {
                continue;
            }
            let label: String = r.label.trim().chars().take(12).collect();
            let rid = croot.get(&a).copied().unwrap_or(0);
            ins.execute(rusqlite::params![rid, a, b, label, built_at])
                .map_err(|e| e.to_string())?;
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
pub(crate) fn cluster_rename_llm(
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
        let mut stmt = conn
            .prepare("SELECT id, root_id FROM clusters")
            .map_err(|e| e.to_string())?;
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
        let cwd = if kb_root.exists() {
            kb_root
        } else {
            std::env::temp_dir()
        };
        // 墙钟超时同上:命名卡死不能钉死后续归类。
        crate::kb::run_claude_readonly_timeout(
            &cwd,
            &prompt,
            |kind, _t| {
                if kind == "delta" {
                    emit_cluster(app, json!({ "kind": "tick", "tier": tier }));
                }
            },
            std::time::Duration::from_secs(600),
        )?
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

// ───────────────────────── AI 智能命名(可选;免嵌入 key) ─────────────────────────
//
// 「本地档」清洗名(clean_title)救不了的(纯哈希/纯乱码/纯时间戳图片名),交给已连接的大模型
// 按目录+类型+名字线索起个可读中文标题,写进 titles 表覆盖显示(磁盘文件名不动)。复用归类那套
// (独立归类模型 or run_claude_readonly + extract_balanced_json)。

pub(crate) static LLM_TITLING: AtomicBool = AtomicBool::new(false);

pub(crate) fn emit_title(app: &AppHandle, payload: Value) {
    let _ = app.emit("file:title_llm", payload);
}

#[derive(Debug, Deserialize)]
pub(crate) struct LlmTitle {
    #[serde(default)]
    i: Value,
    #[serde(default)]
    title: String,
}

pub(crate) fn titles_llm_directive(files: &[FileLite]) -> String {
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

pub(crate) fn titles_llm_run(app: &AppHandle, root: Option<String>) -> Result<usize, String> {
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
        let cwd = if kb_root.exists() {
            kb_root
        } else {
            std::env::temp_dir()
        };
        // 墙钟超时同上。
        crate::kb::run_claude_readonly_timeout(
            &cwd,
            &prompt,
            |kind, _text| {
                if kind == "delta" {
                    emit_title(app, json!({ "kind": "tick" }));
                }
            },
            std::time::Duration::from_secs(600),
        )?
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
