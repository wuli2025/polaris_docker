//! 业务专家团 — 「配几个做对应业务的专家团队」。
//!
//! 每个团 = 一位领衔战略/统帅 + 4 位对应业务专家。智能匹配优先在团内召人，
//! 让一句话需求能稳定命中一支成建制的队伍，而不是零散个人。
//!
//! 团本体仍是一段编排型 CLAUDE.md（运行时由成员卡片组装），复用既有
//! persona_apply → 写项目 CLAUDE.md 链路。团里永不写死「先谁后谁」——
//! 顺序由战略师运行时按任务决定（Kimi Agent Swarm 哲学）。

use crate::expert::ExpertCard;
use serde::Serialize;

/// 一支业务专家团（对外给前端市场卡片用）。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ExpertTeam {
    pub id: String,
    pub name: String,
    pub icon: String,
    /// 一句话定位
    pub tagline: String,
    /// 详细说明
    pub description: String,
    /// 领衔者（战略/统帅）专家 id
    pub lead_id: String,
    /// 成员专家 id（不含 lead）
    pub member_ids: Vec<String>,
    /// 业务标签，喂智能匹配 + 卡片展示
    pub tags: Vec<String>,
}

fn t(
    id: &str,
    name: &str,
    icon: &str,
    tagline: &str,
    description: &str,
    lead_id: &str,
    member_ids: &[&str],
    tags: &[&str],
) -> ExpertTeam {
    ExpertTeam {
        id: id.into(),
        name: name.into(),
        icon: icon.into(),
        tagline: tagline.into(),
        description: description.into(),
        lead_id: lead_id.into(),
        member_ids: member_ids.iter().map(|s| s.to_string()).collect(),
        tags: tags.iter().map(|s| s.to_string()).collect(),
    }
}

/// 8 支成建制业务团（覆盖产品/创作/研究/数据/安全/上线/增长/质量）。
pub fn all_teams() -> Vec<ExpertTeam> {
    vec![
        t(
            "team-fullstack-product",
            "全栈产品团",
            "🚀",
            "把一个想法做成能上线的产品",
            "战略师领衔，从需求到上线一条龙：产品定义→后端边界→前端体验→部署发布。适合「帮我做个 X 应用/网站/SaaS」这类成品需求。",
            "chief-strategist",
            &["product-manager", "backend-architect", "frontend-engineer", "devops-engineer"],
            &["产品", "全栈", "上线", "SaaS", "应用"],
        ),
        t(
            "team-creative-content",
            "创作内容团",
            "🎨",
            "PPT / 网页 / 自媒体 / 视频，要美要打动人",
            "文案官领衔（含品牌叙事），成品兼顾「好看」和「打动人」：文案口播×视觉设计×落地工程×内容分发。",
            "copywriter",
            &["visual-designer", "frontend-engineer", "social-media-manager", "content-marketer"],
            &["创作", "PPT", "视觉", "文案", "自媒体"],
        ),
        t(
            "team-research-diligence",
            "研究尽调团",
            "🔬",
            "调研 / 选型 / 尽调，结论带来源",
            "深度研究领衔，多源检索×竞品对标×市场规模×趋势研判×数据印证，结论可追溯。",
            "deep-research",
            &["competitive-analyst", "market-researcher", "trend-analyst", "data-analyst"],
            &["研究", "调研", "尽调", "选型", "竞品"],
        ),
        t(
            "team-data-insight",
            "数据洞察团",
            "📊",
            "从数据管道到可视化洞察",
            "数据科学家领衔，数据工程×指标分析×查询优化×可视化叙事，把原始数据讲成能决策的故事。",
            "data-scientist",
            &["data-engineer", "data-analyst", "database-optimizer", "dataviz-storyteller"],
            &["数据", "分析", "建模", "可视化", "洞察"],
        ),
        t(
            "team-security-compliance",
            "安全合规团",
            "🛡️",
            "审计 / 渗透 / 威胁建模 / 合规",
            "安全审计员领衔，OWASP 审计×STRIDE 威胁建模×隐私工程×合规落地，对外暴露前先过这一关。",
            "security-auditor",
            &["threat-modeling-expert", "compliance-privacy", "privacy-engineer"],
            &["安全", "渗透", "合规", "审计", "隐私"],
        ),
        t(
            "team-devops-launch",
            "运维上线团",
            "⚙️",
            "容器化 → 发布 → 可观测 → 止血",
            "DevOps 工程师领衔，镜像瘦身×集群编排×SLO 可观测×事故响应，让产品稳稳上线、出事能止血。",
            "devops-engineer",
            &["docker-expert", "kubernetes-architect", "sre-engineer", "incident-responder"],
            &["部署", "容器", "上线", "运维", "可观测"],
        ),
        t(
            "team-growth-marketing",
            "增长营销团",
            "📣",
            "获客 / 增长 / 内容矩阵",
            "增长黑客领衔，内容矩阵×SEO 收录×增长实验×社媒运营，把产品推到用户面前并留住。",
            "growth-hacker",
            &["content-marketer", "seo-specialist", "growth-experimenter", "social-media-manager"],
            &["增长", "营销", "获客", "SEO", "内容"],
        ),
        t(
            "team-quality-refactor",
            "质量重构团",
            "🔬",
            "评审 / 测试 / 重构 / 性能",
            "代码评审员领衔，缺陷评审×自动化测试×坏味道重构×性能调优，让代码合并前可靠、长期可维护。",
            "code-reviewer",
            &["test-automator", "refactoring-specialist", "performance-engineer", "debugger"],
            &["质量", "测试", "重构", "性能", "评审"],
        ),
    ]
}

/// 用业务团 + 成员卡片组装一段「战略师领衔·按需召集」的编排型 CLAUDE.md。
/// 团里永不写死执行顺序：列「谁能干什么活、何时召」，顺序运行时算。
pub fn build_team_doc(team: &ExpertTeam, lead: &ExpertCard, members: &[ExpertCard]) -> String {
    let mut s = String::new();
    s.push_str(&format!("# {} {}\n\n", team.icon, team.name));
    s.push_str(&format!("> {}\n\n", team.tagline));
    s.push_str(&format!("{}\n\n", team.description));

    s.push_str("## 你是这支团队的编排者（战略师领衔）\n\n");
    s.push_str(&format!(
        "由 **{}** 领衔。读懂用户目标后，**按情况临时组阵**——\
         不是每次都把全队拉上场。默认先用单 agent；当任务确实需要分工、\
         且并行有收益时，才召集对应专家。成本纪律：一次最多 4~5 人，\
         独立子任务才并行，紧耦合任务退回串行（防 fake parallelism）。\n\n",
        lead.name
    ));

    s.push_str("## 候选专家（能力候选池，不是执行顺序）\n\n");
    s.push_str(&format!(
        "- 🧭 **{}**（领衔）— {}。何时召：{}\n",
        lead.name, lead.role, lead.description
    ));
    for m in members {
        s.push_str(&format!(
            "- **{}** — {}。何时召：{}\n",
            m.name, m.role, m.description
        ));
    }
    s.push('\n');

    s.push_str("## 工作方式\n\n");
    s.push_str("1. **先拆子任务**：把目标拆成若干「子任务」，每个子任务才去召对应专家；简单任务不拆，直接干。\n");
    s.push_str(
        "2. **召集即解释**：召一个专家时，简述「为什么是 TA」（命中的需求点 + 补的能力维度）。\n",
    );
    s.push_str(
        "3. **默认并行、紧耦合克制**：独立子任务可并行推进；有先后依赖的串行做，别假并行。\n",
    );
    s.push_str("4. **单一收口**：多分支产出由你（领衔者）合并成一份交付，不堆砌半成品。\n\n");

    // 统一质量标尺 —— 只留可判定门槛, 不留清单回声; 领衔者收口即按此打回。
    s.push_str("## 交付门槛（团队铁律 · 任一不满足即未完成，领衔者打回重做）\n\n");
    s.push_str("- **视觉**：统一设计语言（≤3 主色、统一字阶、8 的倍数间距）；严禁 emoji 当图标、占位符残留、AI 套版感；视觉体系不自创——从前端大师库（`skills/polaris-deck-studio/designers/INDEX.md` 的 11 位设计师花名册）选一位并守其色板与禁忌，用户指定风格则按用户的。\n");
    s.push_str("- **内容**：文案有钩子、信息准确、无「待补充」残留。\n");
    s.push_str("- **工程**：成品自包含可直接打开、响应式不破版、无碎图与控制台报错。\n");
    s.push_str("- **收口**：多分支产出由领衔者合并为一份打磨过的成品，任一门槛不过即退回对应专家重做。\n\n");
    s.push_str("## 冲突消解\n\n");
    s.push_str("用户的明确要求 > 团队铁律 > 通用风格。用户要求与铁律冲突时，一句话点明代价，然后按用户的来；信息不足且猜错代价高时，先问一个最小澄清问题再动手。\n\n");

    s.push_str("---\n_本团由北极星「业务专家团」自动组装；成员可在对话中追加 / 换人。_\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teams_reference_real_experts() {
        let experts = crate::expert::all_experts_for_test();
        let ids: std::collections::HashSet<_> = experts.iter().map(|e| e.id.as_str()).collect();
        for team in all_teams() {
            assert!(
                ids.contains(team.lead_id.as_str()),
                "{} 的 lead {} 不存在",
                team.id,
                team.lead_id
            );
            for m in &team.member_ids {
                assert!(ids.contains(m.as_str()), "{} 的成员 {} 不存在", team.id, m);
            }
        }
    }

    #[test]
    fn team_ids_unique() {
        let teams = all_teams();
        let mut ids: Vec<&str> = teams.iter().map(|t| t.id.as_str()).collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(before, ids.len(), "团 id 不应重复");
    }
}
