//! 专家 CLAUDE.md 文档 — 运行时从 GENERIC.md 模板变量替换生成具体专家正文
//!
//! 变量: {{NAME}} · {{ID}} · {{ROLE}} · {{DESCRIPTION}} · {{KEYWORDS}} ·
//!       {{CAPABILITIES}} · {{TRIGGER_SIGNALS}} · {{COMPLEMENTS}} ·
//!       {{EXCLUSIVE_WITH}} · {{COST_TIER}} · {{TIMESTAMP}}

use std::time::{SystemTime, UNIX_EPOCH};

/// 用专家元数据对 GENERIC.md 模板做变量替换，
/// 供 expert_apply / expert_export 在写入 CLAUDE.md 前填充完整内容。
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
    let template = GENERIC_TEMPLATE;
    let timestamp = current_date();

    let parts: Vec<&str> = ref_path
        .trim_start_matches("experts/")
        .trim_end_matches(".md")
        .split('/')
        .collect();
    let id = parts.last().unwrap_or(&"unknown").to_string();
    let group = parts.get(parts.len().saturating_sub(2)).unwrap_or(&"unknown");

    let mut result = template.to_string();
    result = result.replace("{{NAME}}", name);
    result = result.replace("{{ID}}", &id);
    result = result.replace("{{GROUP}}", group);
    result = result.replace("{{ROLE}}", role);
    result = result.replace("{{DESCRIPTION}}", description);
    result = result.replace("{{KEYWORDS}}", &keywords.join("、"));
    result = result.replace("{{CAPABILITIES}}", &capabilities.iter().map(|s| format!("- **{}**", s)).collect::<Vec<_>>().join("\n"));
    result = result.replace("{{TRIGGER_SIGNALS}}", &trigger_signals.iter().map(|s| format!("- **{}**", s)).collect::<Vec<_>>().join("\n"));
    result = result.replace("{{COMPLEMENTS}}", complements);
    result = result.replace("{{EXCLUSIVE_WITH}}", &exclusive_with.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("、"));
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


/// GENERIC.md 模板（编译期内嵌）
const GENERIC_TEMPLATE: &str = include_str!("../templates/experts/GENERIC.md");
