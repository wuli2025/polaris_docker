//! 专家 CLAUDE.md 文档 — 每个专家一份可编辑的 .md 提示词（放仓库源码、编译期内嵌）。
//!
//! 取数顺序:
//!   1. `src/templates/experts/<group>/<id>.md` —— 该专家**亲自写好的**提示词(强烈推荐,
//!      调它就是调这个文件,改完重编即生效)。文件内可用下方变量占位,也可全篇手写散文。
//!   2. 找不到该文件 → 回落 `GENERIC.md` 通用骨架 + 变量替换(保证任何专家都有内容)。
//!
//! 变量: {{NAME}} · {{ID}} · {{GROUP}} · {{ROLE}} · {{DESCRIPTION}} · {{KEYWORDS}} ·
//!       {{CAPABILITIES}} · {{TRIGGER_SIGNALS}} · {{COMPLEMENTS}} ·
//!       {{EXCLUSIVE_WITH}} · {{COST_TIER}} · {{TIMESTAMP}}

use include_dir::{include_dir, Dir};
use std::time::{SystemTime, UNIX_EPOCH};

/// 整个专家提示词目录编进二进制 —— 每加一个 `<group>/<id>.md` 文件,重编后自动可用。
static EXPERTS_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/src/templates/experts");

/// 取某专家的完整提示词正文(供 expert_apply / expert_export / route_block 注入)。
///
/// 先找该专家专属 .md(`experts/<group>/<id>.md`),没有再用 GENERIC 骨架。两条路都会做
/// 变量替换,所以专属文件既能全篇手写、也能内嵌 {{NAME}} 等占位由元数据补全。
pub fn build_expert_doc(
    ref_path: &str,
    name: &str,
    role: &str,
    description: &str,
    keywords: &[String],
    capabilities: &[String],
    trigger_signals: &[String],
    complements: &str,
    exclusive_with: &[String],
    cost_tier: u8,
) -> Option<String> {
    // ref_path 形如 "experts/marketing/visual-designer.md";嵌入目录的根就是 experts/,
    // 所以去掉前缀后用 "marketing/visual-designer.md" 查。
    let rel = ref_path.trim_start_matches("experts/");
    let parts: Vec<&str> = rel.trim_end_matches(".md").split('/').collect();
    let id = parts.last().unwrap_or(&"unknown").to_string();
    let group = parts
        .get(parts.len().saturating_sub(2))
        .unwrap_or(&"unknown")
        .to_string();

    // ① 专家专属提示词文件优先;② 否则通用骨架。
    let template: String = EXPERTS_DIR
        .get_file(rel)
        .and_then(|f| f.contents_utf8())
        .map(|s| s.to_string())
        .unwrap_or_else(|| GENERIC_TEMPLATE.to_string());

    let timestamp = current_date();
    let mut result = template;
    result = result.replace("{{NAME}}", name);
    result = result.replace("{{ID}}", &id);
    result = result.replace("{{GROUP}}", &group);
    result = result.replace("{{ROLE}}", role);
    result = result.replace("{{DESCRIPTION}}", description);
    result = result.replace("{{KEYWORDS}}", &keywords.join("、"));
    result = result.replace(
        "{{CAPABILITIES}}",
        &capabilities
            .iter()
            .map(|s| format!("- **{}**", s))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    result = result.replace(
        "{{TRIGGER_SIGNALS}}",
        &trigger_signals
            .iter()
            .map(|s| format!("- **{}**", s))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    result = result.replace("{{COMPLEMENTS}}", complements);
    result = result.replace(
        "{{EXCLUSIVE_WITH}}",
        &exclusive_with
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("、"),
    );
    result = result.replace("{{COST_TIER}}", &cost_tier.to_string());
    result = result.replace("{{TIMESTAMP}}", &timestamp);
    Some(result)
}

fn current_date() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| chrono::DateTime::from_timestamp(d.as_secs() as i64, 0))
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| String::new())
}

/// GENERIC.md 通用骨架（编译期内嵌）—— 仅当某专家还没有专属 .md 时回落使用。
const GENERIC_TEMPLATE: &str = include_str!("../templates/experts/GENERIC.md");
