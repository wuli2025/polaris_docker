//! 融合 + 重排:两车道并行召回 → 文件级 RRF → 新压旧降权 → 路径/覆盖/整句加权 → 精排闸门。

use super::*;

/// 相对路径是否落在 scope 内(scope 按盘点根相对路径的**首段**匹配,大小写不敏感):
/// - `None` / 空 → 全盘(零回归);
/// - `Some("wiki")` → 仅首段为 wiki 的命中(妈妈库子树);
/// - `Some("!wiki")` → 仅首段**不是** wiki 的命中(「外面整个库」= raw/output/memory…)。
fn path_in_scope(path: &str, scope: Option<&str>) -> bool {
    let scope = match scope {
        None => return true,
        Some(s) if s.trim().is_empty() => return true,
        Some(s) => s.trim(),
    };
    let p = path.replace('\\', "/");
    let first = p.split('/').next().unwrap_or("");
    match scope.strip_prefix('!') {
        Some(neg) => !first.eq_ignore_ascii_case(neg),
        None => first.eq_ignore_ascii_case(scope),
    }
}

/// 核心检索(三壳共用)。mode: hybrid | grep | vector。
/// `scope`:可选的盘点根相对路径首段过滤(见 [`path_in_scope`]);None=全盘。
pub fn search(
    query: &str,
    top_k: usize,
    mode: &str,
    scope: Option<&str>,
) -> Result<FableSearchResult, String> {
    let started = std::time::Instant::now();
    // 防御性截断:超长查询(误把整篇文档/日志粘进搜索框)会撑爆嵌入请求体与 FTS5 MATCH
    // 表达式(单个 ~2MB 短语),且对召回毫无增益 —— 检索意图在前几十字就已表达。截到 2000
    // 字符,正常查询零影响;这是「最严峻输入」下保证向量/倒排两腿都不被拖垮的硬护栏。
    const MAX_QUERY_CHARS: usize = 2000;
    let clamped: String;
    let query: &str = if query.chars().count() > MAX_QUERY_CHARS {
        clamped = query.chars().take(MAX_QUERY_CHARS).collect();
        &clamped
    } else {
        query
    };
    let top_k = top_k.clamp(1, 50);
    let want_grep = mode != "vector";
    let want_vec = mode != "grep";

    // 两车道真并行(thread::scope);单车道失败不连坐 —— grep 永远可用,向量缺 key 时降级
    let mut grep_res: Result<(Vec<GrepHit>, bool), String> = Ok((Vec::new(), false));
    let mut vec_res: Result<Vec<VecHit>, String> = Ok(Vec::new());
    std::thread::scope(|s| {
        let g = want_grep.then(|| s.spawn(|| grep_lane(query)));
        let v = want_vec.then(|| s.spawn(|| vector_lane(query, top_k)));
        if let Some(h) = g {
            grep_res = h.join().unwrap_or_else(|_| Err("grep 车道 panic".into()));
        }
        if let Some(h) = v {
            vec_res = h.join().unwrap_or_else(|_| Err("向量车道 panic".into()));
        }
    });

    let (grep_hits, grep_truncated) = match grep_res {
        Ok(x) => x,
        Err(e) if mode == "grep" => return Err(e),
        Err(_) => (Vec::new(), false),
    };
    let vec_hits = match vec_res {
        Ok(x) => x,
        Err(e) if mode == "vector" => return Err(e),
        Err(_) => Vec::new(), // hybrid 下向量车道缺 key/断网 → 静默降级成纯 grep
    };
    // scope 过滤:命中后按相对路径首段筛(妈妈库 wiki / 外库 !wiki / 全盘 None);零回归。
    let grep_hits: Vec<GrepHit> = grep_hits
        .into_iter()
        .filter(|h| path_in_scope(&h.path, scope))
        .collect();
    // 条件融合质量闸(见 [`vec_min_score`]):低余弦的向量命中是「凑数噪声」,门控掉它们
    // 防止把强词法命中挤下榜首。默认阈值 0.0 → 恒为真 → 零回归。vec_hits 已按余弦降序,
    // 这里是「整条腿要么够好要么不掺和」的逐条绝对门控,而非相对降权。
    let vmin = vec_min_score();
    let vec_hits: Vec<VecHit> = vec_hits
        .into_iter()
        .filter(|h| path_in_scope(&h.path, scope) && h.score >= vmin)
        .collect();
    let (n_grep, n_vec) = (grep_hits.len(), vec_hits.len());

    // ── P0-1 修:RRF 融合 key 降到**文件级** ──
    // 原 bug:grep 用 `path#L行号`、向量用 `path#C段号`,两套编号天然不相交 → 同一文件被两路
    // 命中也永远进不了 and_modify 分支,`lanes` 恒单元素,RRF「两路同时命中加权顶上」彻底失效。
    // 现在两路都按 `path` 归并:同一文件被 grep + 向量都命中时,rrf 真正叠加、lanes 含两者。
    struct Fused {
        hit: FableHit,
        rrf: f32,
        /// 重排专家「读全文打分」用的文本(向量=chunk 全文 / grep=命中行上下文窗口);不展示。
        doc: String,
        /// 被新版本压制时,新版本的相对路径(供前端标注);None=无更新版本。
        sup_path: Option<String>,
    }
    let (rrf_k, w_grep, mut w_vec) = rrf_params();
    // 关键词型查询给向量腿动态降权(向量腿在此类查询上最弱、易注入噪声)。自然语句保持满权。
    if is_keyword_query(query) {
        w_vec *= kw_vec_damp();
    }
    let mut fused: HashMap<String, Fused> = HashMap::new();
    for (rank, h) in grep_hits.into_iter().enumerate() {
        let key = h.path.clone();
        let rrf = w_grep / (rrf_k + rank as f32);
        fused
            .entry(key)
            .and_modify(|f| {
                f.rrf += rrf;
                if !f.hit.lanes.contains(&"grep".to_string()) {
                    f.hit.lanes.push("grep".into());
                }
                if h.context.len() > f.doc.len() {
                    f.doc = h.context.clone();
                }
            })
            .or_insert(Fused {
                hit: FableHit {
                    path: h.path,
                    abspath: h.abspath,
                    location: format!("L{}", h.line),
                    snippet: h.snippet,
                    score: 0.0,
                    lanes: vec!["grep".into()],
                    superseded_by_path: None,
                },
                rrf,
                doc: h.context,
                sup_path: None,
            });
    }
    for (rank, h) in vec_hits.into_iter().enumerate() {
        let key = h.path.clone();
        let rrf = w_vec / (rrf_k + rank as f32);
        let snippet: String = h.text.chars().take(220).collect();
        fused
            .entry(key)
            .and_modify(|f| {
                f.rrf += rrf;
                if !f.hit.lanes.contains(&"vector".to_string()) {
                    f.hit.lanes.push("vector".into());
                }
                if h.text.len() > f.doc.len() {
                    f.doc = h.text.clone();
                }
            })
            .or_insert(Fused {
                hit: FableHit {
                    path: h.path,
                    abspath: h.abspath,
                    location: format!("C{}", h.seq),
                    snippet,
                    score: 0.0,
                    lanes: vec!["vector".into()],
                    superseded_by_path: None,
                },
                rrf,
                doc: h.text,
                sup_path: None,
            });
    }
    // ── 新压旧:被同目录同名新版本压制的命中降权(不剔除,新版没索引到时旧版仍可达)──
    // dedupe_scan 已在 files.superseded_by 里标好「谁被谁压制」;这里按候选相对路径批量取标记,
    // abspath 消歧(同名 relpath 可能横跨多个根),命中者 rrf ×decay 并记下新版路径供前端标注。
    // POLARIS_SUPERSEDE_DECAY=1.0 一键关闭(不降权、不查库)。
    let decay = supersede_decay();
    if decay < 1.0 && !fused.is_empty() {
        if let Ok(conn) = open_db() {
            // relpath → Vec<(root_path, superseded_by_id)>
            let mut by_rel: HashMap<String, Vec<(String, i64)>> = HashMap::new();
            let keys: Vec<String> = fused.keys().cloned().collect();
            for batch in keys.chunks(400) {
                let ph = vec!["?"; batch.len()].join(",");
                let sql = format!(
                    "SELECT f.relpath, r.path, f.superseded_by FROM files f
                     JOIN roots r ON r.id=f.root_id
                     WHERE f.superseded_by<>0 AND f.relpath IN ({ph})"
                );
                if let Ok(mut stmt) = conn.prepare(&sql) {
                    let rows = stmt.query_map(rusqlite::params_from_iter(batch.iter()), |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, i64>(2)?,
                        ))
                    });
                    if let Ok(rs) = rows {
                        for (rel, root, sup) in rs.flatten() {
                            by_rel.entry(rel).or_default().push((root, sup));
                        }
                    }
                }
            }
            if !by_rel.is_empty() {
                // 收集被压制文件的新版本 id → 相对路径(供前端标注),一次查完。
                let sup_ids: Vec<i64> = by_rel
                    .values()
                    .flatten()
                    .map(|(_, id)| *id)
                    .collect::<std::collections::HashSet<i64>>()
                    .into_iter()
                    .collect();
                let mut id_path: HashMap<i64, String> = HashMap::new();
                for batch in sup_ids.chunks(400) {
                    let ph = vec!["?"; batch.len()].join(",");
                    let sql = format!("SELECT id, relpath FROM files WHERE id IN ({ph})");
                    if let Ok(mut stmt) = conn.prepare(&sql) {
                        if let Ok(rs) = stmt
                            .query_map(rusqlite::params_from_iter(batch.iter()), |r| {
                                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                            })
                        {
                            for (id, rel) in rs.flatten() {
                                id_path.insert(id, rel);
                            }
                        }
                    }
                }
                for f in fused.values_mut() {
                    if let Some(cands) = by_rel.get(&f.hit.path) {
                        // abspath 消歧:命中行的 abspath 应以某个根路径开头。
                        let sup = cands
                            .iter()
                            .find(|(root, _)| {
                                f.hit.abspath.replace('\\', "/").starts_with(
                                    &std::path::Path::new(root)
                                        .to_string_lossy()
                                        .replace('\\', "/"),
                                )
                            })
                            .or_else(|| cands.first());
                        if let Some((_, sup_id)) = sup {
                            f.rrf *= decay;
                            f.sup_path = id_path.get(sup_id).cloned();
                        }
                    }
                }
            }
        }
    }

    // ── 路径/文件名命中加权(融合层,覆盖两腿)──
    // 查询词出现在文件路径/名里是「几乎必相关」的强信号,但两腿都只算正文命中。这里给每个
    // 候选按其路径与查询的重合补一个同量纲加分 → 文件名点题的文件稳稳上浮。默认权重 >0(通用
    // 增益);设 POLARIS_PATH_BOOST=0 可关。
    // 路径加权 + 全词覆盖/整句加权:三者同量纲、都在融合层补分。共用一次 split_query。
    let pboost_w = path_boost_w();
    let cov_w = coverage_boost_w();
    let phrase_w = phrase_boost_w();
    if pboost_w > 0.0 || cov_w > 0.0 || phrase_w > 0.0 {
        let (q_full, terms) = split_query(query);
        for f in fused.values_mut() {
            if pboost_w > 0.0 {
                f.rrf += path_boost(&f.hit.path, &q_full, &terms, pboost_w);
            }
            if cov_w > 0.0 || phrase_w > 0.0 {
                // doc = 该候选的匹配文本(向量腿 chunk 全文 / grep 腿命中行上下文),已在内存,零额外 IO。
                let doc_lower = f.doc.to_lowercase();
                f.rrf += coverage_phrase_boost(&doc_lower, &q_full, &terms, cov_w, phrase_w);
            }
        }
    }
    let mut merged: Vec<Fused> = fused.into_values().collect();
    merged.sort_by(|a, b| {
        b.rrf
            .partial_cmp(&a.rrf)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    merged.truncate(rerank_n());

    // ── P2-1 精排闸门(详解 §4/§5):只在「该精排」时才请专家 ──
    // 条件:混检 + 有重排服务商 + 候选≥3 + 前两名分数咬得紧(难分高下,正是粗筛分不清、
    // 精排价值最大的场景)。一骑绝尘 / 候选过少 / 服务不可用 → 直接保持融合序(优雅降级)。
    // 重排仅在 mode 恰为 "hybrid" 时触发。**契约**: 想要「双车道融合但不重排」的快档(快速模式
    // 召回走这条, 见 chat::forced_recall_block), 传一个既非 "grep" 也非 "vector" 的多车道 mode
    // (如 "grep_vec") —— want_grep/want_vec 都为真(两腿都跑), 但因 != "hybrid" 直接跳过这层网络
    // 重排, 把召回从 ~1.8s 降到 ~250ms。改这里的判断前请同步 forced_recall_block。
    let mut reranked = false;
    let gate_close = merged.len() >= 2 && {
        let (r1, r2) = (merged[0].rrf, merged[1].rrf);
        r1 > 0.0 && (r1 - r2) / r1 < rerank_gate()
    };
    if cloud_rerank_enabled()
        && mode == "hybrid"
        && merged.len() >= 3
        && gate_close
        && crate::sense::active_provider("rerank").is_some()
    {
        // 喂**全文**(向量 chunk 全文 / grep 命中行上下文窗口),不再喂展示用 160/220 字碎片。
        let docs: Vec<String> = merged.iter().map(|f| f.doc.clone()).collect();
        // 查询级缓存(P2-1 ③):同一查询 + 同一候选签名命中则跳过这次网络调用。
        let sig = rerank_sig(
            query,
            &merged
                .iter()
                .map(|f| (&f.hit.path, &f.hit.location))
                .collect::<Vec<_>>(),
        );
        let order = match rerank_cache_get(&sig) {
            Some(o) => Some(o),
            None => match super::index::rerank(query, &docs, merged.len()) {
                Ok(o) => {
                    rerank_cache_put(sig, o.clone());
                    Some(o)
                }
                Err(_) => None,
            },
        };
        if let Some(order) = order {
            let mut reordered: Vec<Fused> = Vec::with_capacity(merged.len());
            let mut taken = vec![false; merged.len()];
            for (idx, score) in &order {
                if let Some(f) = merged.get(*idx) {
                    if !taken[*idx] {
                        taken[*idx] = true;
                        reordered.push(Fused {
                            hit: {
                                let mut h = f.hit.clone();
                                h.score = *score;
                                h
                            },
                            rrf: f.rrf,
                            doc: f.doc.clone(),
                            sup_path: f.sup_path.clone(),
                        });
                    }
                }
            }
            for (i, f) in merged.iter().enumerate() {
                if !taken[i] {
                    reordered.push(Fused {
                        hit: f.hit.clone(),
                        rrf: f.rrf,
                        doc: f.doc.clone(),
                        sup_path: f.sup_path.clone(),
                    });
                }
            }
            merged = reordered;
            reranked = true;
        }
    }

    let hits: Vec<FableHit> = merged
        .into_iter()
        .take(top_k)
        .map(|mut f| {
            if f.hit.score == 0.0 {
                f.hit.score = f.rrf;
            }
            f.hit.superseded_by_path = f.sup_path;
            f.hit
        })
        .collect();

    Ok(FableSearchResult {
        query: query.to_string(),
        mode: mode.to_string(),
        hits,
        grep_hits: n_grep,
        vector_hits: n_vec,
        reranked,
        grep_truncated,
        ms: started.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_filter_first_segment() {
        // None / 空 → 全盘放行
        assert!(path_in_scope("wiki/概念/x.md", None));
        assert!(path_in_scope("raw/a.md", Some("")));
        assert!(path_in_scope("raw/a.md", Some("  ")));
        // 正向:仅首段命中
        assert!(path_in_scope("wiki/概念/x.md", Some("wiki")));
        assert!(!path_in_scope("raw/a.md", Some("wiki")));
        assert!(!path_in_scope("output/r.md", Some("wiki")));
        // 反向 !wiki:首段不是 wiki 的才放行(「外面整个库」)
        assert!(!path_in_scope("wiki/概念/x.md", Some("!wiki")));
        assert!(path_in_scope("raw/a.md", Some("!wiki")));
        assert!(path_in_scope("output/r.md", Some("!wiki")));
        // 反斜杠路径(Windows)也按首段判定
        assert!(path_in_scope("wiki\\概念\\x.md", Some("wiki")));
        // 大小写不敏感
        assert!(path_in_scope("WIKI/x.md", Some("wiki")));
    }
}
