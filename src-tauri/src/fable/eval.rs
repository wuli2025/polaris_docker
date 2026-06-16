//! 评测集(考卷)—— P2-3:把「稳定准确」从口号变成可证明的数字。
//!
//! 出处:《检索 20TB 级整改报告 v2》P2-3「缺评测集 → 改动好坏只能靠肉眼」。
//! 做法:一份「标准问答 → 期望命中文件」的 JSON 考卷,每次改完检索就跑一遍,产出两个硬指标:
//! - **召回率(recall@k)**:有多少个问题,前 k 条命中里至少出现了一个期望文件;
//! - **MRR**(平均倒数排名):期望文件第一次出现的排名的倒数的均值 —— 既看「找没找到」,也看「排得靠不靠前」。
//!
//! 这也是验证「重排闸门」(P2-1)、「文件级 RRF」(P0-1)等改动调得好不好的唯一标尺。
//! 评测集默认落 `~/Polaris/data/fable_eval.json`,先用 [`write_template`] 写一份样例再按需填。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ───────────────────────── 考卷模型 ─────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct EvalCase {
    /// 标准问题(原样喂检索)。
    pub query: String,
    /// 期望命中的文件(相对盘点根的路径,**子串**匹配即算命中;大小写/斜杠不敏感)。
    #[serde(default)]
    pub expect: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EvalSet {
    #[serde(default)]
    pub cases: Vec<EvalCase>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalCaseResult {
    pub query: String,
    pub expected: Vec<String>,
    /// 期望文件第一次出现的排名(1 起);None = 前 k 条里没出现。
    pub hit_rank: Option<usize>,
    /// 实际返回的前若干条路径(便于人工看「错在哪」)。
    pub top_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalReport {
    pub total_cases: usize,
    /// 实际参与评测的题数(跳过空问题/无期望项的题)。
    pub evaluated: usize,
    pub k: usize,
    pub mode: String,
    /// 召回率:命中题数 / 评测题数。
    pub recall_at_k: f32,
    /// 平均倒数排名。
    pub mrr: f32,
    pub details: Vec<EvalCaseResult>,
}

// ───────────────────────── 纯函数(可单测)─────────────────────────

/// 规范化路径用于子串匹配:小写 + 反斜杠转正斜杠。
fn norm(p: &str) -> String {
    p.to_lowercase().replace('\\', "/")
}

/// 期望文件在返回路径里第一次出现的排名(1 起)。任一 expect 项是某条返回路径的子串即算命中。
pub fn first_hit_rank(top_paths: &[String], expect: &[String]) -> Option<usize> {
    if expect.is_empty() {
        return None;
    }
    let exp: Vec<String> = expect.iter().map(|e| norm(e)).filter(|e| !e.is_empty()).collect();
    for (i, p) in top_paths.iter().enumerate() {
        let np = norm(p);
        if exp.iter().any(|e| np.contains(e.as_str())) {
            return Some(i + 1);
        }
    }
    None
}

/// 从每题的命中排名聚合出 (recall@k, mrr)。空集合 → (0,0)。
pub fn aggregate(ranks: &[Option<usize>]) -> (f32, f32) {
    if ranks.is_empty() {
        return (0.0, 0.0);
    }
    let n = ranks.len() as f32;
    let hit = ranks.iter().filter(|r| r.is_some()).count() as f32;
    let mrr: f32 = ranks.iter().map(|r| r.map(|x| 1.0 / x as f32).unwrap_or(0.0)).sum();
    (hit / n, mrr / n)
}

// ───────────────────────── 运行 ─────────────────────────

fn eval_path(custom: Option<String>) -> Result<PathBuf, String> {
    if let Some(c) = custom.map(|c| c.trim().to_string()).filter(|c| !c.is_empty()) {
        return Ok(PathBuf::from(c));
    }
    super::db_path()
        .map(|p| p.with_file_name("fable_eval.json"))
        .ok_or_else(|| "无法定位用户目录".to_string())
}

/// 跑一遍考卷:逐题检索 → 算命中排名 → 聚合 recall@k + MRR。
pub fn run_eval(custom: Option<String>, top_k: usize, mode: &str) -> Result<EvalReport, String> {
    let path = eval_path(custom)?;
    let raw = std::fs::read_to_string(&path).map_err(|_| {
        format!(
            "找不到评测集 {}:先调 fable_eval_template 写一份样例,填上「标准问答 → 期望命中文件」再跑。",
            path.display()
        )
    })?;
    let set: EvalSet =
        serde_json::from_str(&raw).map_err(|e| format!("评测集 JSON 解析失败: {e}"))?;

    let top_k = top_k.clamp(1, 50);
    let mut details = Vec::new();
    let mut ranks: Vec<Option<usize>> = Vec::new();
    for case in &set.cases {
        let q = case.query.trim();
        if q.is_empty() || case.expect.is_empty() {
            continue; // 没问题或没标准答案的题不计入
        }
        let res = super::retrieve::search(q, top_k, mode, None)?;
        let top_paths: Vec<String> = res.hits.iter().map(|h| h.path.clone()).collect();
        let rank = first_hit_rank(&top_paths, &case.expect);
        ranks.push(rank);
        details.push(EvalCaseResult {
            query: q.to_string(),
            expected: case.expect.clone(),
            hit_rank: rank,
            top_paths,
        });
    }
    let (recall_at_k, mrr) = aggregate(&ranks);
    Ok(EvalReport {
        total_cases: set.cases.len(),
        evaluated: ranks.len(),
        k: top_k,
        mode: mode.to_string(),
        recall_at_k,
        mrr,
        details,
    })
}

const TEMPLATE: &str = r#"{
  "_说明": "标准问答考卷:query=标准问题,expect=期望命中的文件(相对盘点根的路径,子串匹配即算命中)。",
  "_用法": "填 50~100 题覆盖你最常问的检索 → 每次改完检索跑 fable_eval,看 recall_at_k 与 mrr 有没有掉。",
  "cases": [
    { "query": "示例:营业时间 / 开放时间", "expect": ["faq.md", "营业时间.txt"] },
    { "query": "示例:退款政策怎么走", "expect": ["policy/refund.md"] }
  ]
}
"#;

/// 写一份评测集样例(已存在则不覆盖)。返回写入路径。
pub fn write_template(custom: Option<String>) -> Result<String, String> {
    let path = eval_path(custom)?;
    if path.exists() {
        return Err(format!("评测集已存在,未覆盖: {}", path.display()));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败: {e}"))?;
    }
    std::fs::write(&path, TEMPLATE).map_err(|e| format!("写评测集样例失败: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

// ───────────────────────── 命令(薄包装)─────────────────────────

/// 跑考卷,返回 recall@k + MRR 报告。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fable_eval(
    path: Option<String>,
    top_k: Option<usize>,
    mode: Option<String>,
) -> Result<EvalReport, String> {
    let mode = mode.unwrap_or_else(|| "hybrid".into());
    if !["hybrid", "grep", "vector"].contains(&mode.as_str()) {
        return Err("mode 只接受 hybrid | grep | vector".into());
    }
    run_eval(path, top_k.unwrap_or(12), &mode)
}

/// 在默认位置写一份评测集样例。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fable_eval_template(path: Option<String>) -> Result<String, String> {
    write_template(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_substring_case_insensitive() {
        let tops = vec!["docs/FAQ.md".to_string(), "a/b.txt".to_string()];
        assert_eq!(first_hit_rank(&tops, &["faq.md".to_string()]), Some(1));
        assert_eq!(first_hit_rank(&tops, &["B.TXT".to_string()]), Some(2));
        assert_eq!(first_hit_rank(&tops, &["nowhere".to_string()]), None);
        assert_eq!(first_hit_rank(&tops, &[]), None);
    }

    #[test]
    fn rank_handles_backslash_paths() {
        let tops = vec!["sub\\dir\\note.md".to_string()];
        assert_eq!(first_hit_rank(&tops, &["dir/note.md".to_string()]), Some(1));
    }

    #[test]
    fn aggregate_recall_and_mrr() {
        // 三题:命中第1、命中第2、没命中 → recall=2/3,mrr=(1 + 0.5 + 0)/3
        let (recall, mrr) = aggregate(&[Some(1), Some(2), None]);
        assert!((recall - 2.0 / 3.0).abs() < 1e-6);
        assert!((mrr - (1.0 + 0.5) / 3.0).abs() < 1e-6);
        assert_eq!(aggregate(&[]), (0.0, 0.0));
    }
}
