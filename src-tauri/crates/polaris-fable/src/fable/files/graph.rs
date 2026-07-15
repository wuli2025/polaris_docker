use super::*;

/// 文件中心「星图」数据:把语义簇 + 抽样文件组织成与知识图谱同构的 [`crate::kb::KbGraph`]
/// (root=我的资料 / folder=主题簇 / doc=文件星点),让 KnowledgeGraph.vue 的星河渲染直接复用。
/// 抽样防止上万文件拖垮 cytoscape:每簇最多 PER 个文件星点,总计最多 CAP。
pub(crate) fn build_file_graph(root: Option<String>) -> Result<crate::kb::KbGraph, String> {
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
        let sql = format!(
            "SELECT id, label, color, parent, summary FROM clusters{cfilter} ORDER BY size DESC"
        );
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
            let src = if parent == 0 {
                ROOT_ID.to_string()
            } else {
                format!("c{parent}")
            };
            edges.push(KbEdge {
                source: src,
                target: format!("c{id}"),
                rel: None,
            });
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
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
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
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
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
                edges.push(KbEdge {
                    source: format!("c{cid}"),
                    target: format!("f{fid}"),
                    rel: None,
                });
            }
        }
    }
    Ok(KbGraph { nodes, edges })
}
