//! 百人专家团模块 — 运行时动态召集 + 可解释路由
//!
//! 思想来源: WorkBuddy「专家团」+ Kimi Agent Swarm「无预定义角色/运行时召人」
//! Polaris 实现: 专家 = 能力候选池(CLAUDE.md)，运行时按触发信号 RRF 召回，
//! 每次召集给出「为什么是你」理由 + 备选。
//!
//! 入口: expert_list() / expert_route() / expert_match_auto() / expert_apply()

mod expert_groups;
mod avatars;
mod expert_docs;
mod teams;

pub use teams::ExpertTeam;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use chrono::Utc;

/// 专家能力卡 — 一张「能力候选池」卡片，不含任何执行顺序/依赖关系。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ExpertCard {
    pub id: String,
    pub name: String,
    /// 图标（emoji 或 SVG path）
    pub icon: String,
    /// 角色定位一句话
    pub role: String,
    /// 详细描述（会嵌入主体 CLAUDE.md）
    pub description: String,
    /// ★为什么选它：命中即解释路由原因（词/短语列表）
    pub trigger_signals: Vec<String>,
    /// ★补哪一维：防同质团队
    pub complements: String,
    /// 关键词（喂 FTS5 trigram 检索）
    pub keywords: Vec<String>,
    /// 能力权限列表
    pub capabilities: Vec<String>,
    /// CLAUDE.md 模板路径（编译期内嵌）
    pub claude_md_ref: String,
    /// 推荐模型 hint
    pub model_hint: String,
    /// 成本档: 1=便宜路由/初筛, 2=中档专业活, 3=贵档深度推理
    pub cost_tier: u8,
    /// 互斥列表（同质专家同进会增加协调成本）
    pub exclusive_with: Vec<String>,
    /// 来源仓库
    pub source: String,
    /// 许可
    pub license: String,
    /// 专家分组
    pub group: String,
}

/// 路由结果 — 包含推荐理由
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ExpertMatch {
    /// 专家卡片引用
    pub expert: ExpertCard,
    /// 命中信号（子任务里出现的触发词）
    pub hit_signals: Vec<String>,
    /// 相似度分（0.0 ~ 1.0）
    pub similarity: f32,
    /// 补的维度
    pub complements: String,
    /// 是否是主选（false=备选）
    pub is_primary: bool,
}

/// 单专家活跃状态
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ExpertAgentStatus {
    pub expert_id: String,
    pub name: String,
    pub status: String, // "idle" | "working" | "done"
    pub last_active: String,
}

/// 对话模式
#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ChatMode {
    /// 单 agent（无专家加成，最便宜）
    SingleAgent,
    /// 单专家（从花名册选一个）
    SingleExpert,
    /// 专家团（战略师领衔，按需组阵）
    ExpertTeam,
    /// 智能匹配（一句话描述需求，自动路由到最合适专家）
    AutoMatch,
}

impl Default for ChatMode {
    fn default() -> Self {
        // 默认自动匹配专家
        ChatMode::AutoMatch
    }
}

/// 路由请求
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteRequest {
    pub query: String,
    /// 最多返回多少个（默认 5，含主选+备选）
    pub limit: Option<usize>,
    /// 指定分组过滤（如 "系统架构"）
    pub group_filter: Option<String>,
}

/// ───────────────────────── 100 专家花名册 ─────────────────────────

/// 100+ 专家花名册 —— 构建一次，全程复用。
/// `build_experts()` 每张卡要做 ~16 次 String 分配 + 多个 Vec collect，
/// 100 张 = 每次调用数千次堆分配。而 `route_block` 在**每条聊天消息**都跑
/// （AutoMatch 是默认模式），旧实现等于每条消息重建整份花名册 → 明显卡顿。
/// 改为 Lazy 静态：只读热路径走 `all_experts_ref()` 零分配，
/// 仅需 owned 列表的命令（发给前端）才 `clone()`。
/// 已裁撤专家（2026-06-16 提示词组合审计后下线：跨组重复 / 高度冗余 / 场景太冷门 / 团队仪式岗）。
/// 用户明确保留 game-developer、embedded-systems，故不在此列。从花名册过滤掉 → 不再出现在
/// 列表 / 智能路由 / 专家团。源 ec() 定义暂留 expert_groups.rs，后续可物理删除（功能已等同删除）。
const RETIRED: &[&str] = &[
    // —— cut（删除）——
    "nlp-engineer", "rl-engineer", "technical-writer-pro", "graphql-architect",
    "event-sourcing-architect", "platform-engineer-devops", "csharp-pro", "blockchain-developer",
    "context-manager", "delivery-manager", "scrum-master", "scientific-researcher", "osint-analyst",
    // —— merge（并入他人后下线被合并的一方）——
    "llm-architect", "mlops-engineer", "flutter-expert", "brand-storyteller", "payment-integration",
    "microservices-architect", "mermaid-expert", "multi-agent-coordinator", "knowledge-synthesizer",
    "ops-engineer", "business-analyst", "qa-expert", "tech-debt-strategist", "research-analyst",
    "penetration-tester", "appsec-coder", "license-counsel", "deployment-engineer",
    "data-contract-engineer", "strategy-planner",
];

static EXPERTS: once_cell::sync::Lazy<Vec<ExpertCard>> = once_cell::sync::Lazy::new(|| {
    expert_groups::build_experts()
        .into_iter()
        .filter(|e| !RETIRED.contains(&e.id.as_str()))
        .collect()
});

/// 只读借用花名册（评分/路由等热路径用，零克隆）。
fn all_experts_ref() -> &'static [ExpertCard] {
    &EXPERTS
}

/// 拿一份 owned 花名册（需返回给前端序列化时用）。
fn all_experts() -> Vec<ExpertCard> {
    EXPERTS.clone()
}

/// 专家团状态表: project_id -> Vec<ExpertAgentStatus>
/// 线程安全
static EXPERT_TEAMS: once_cell::sync::Lazy<Arc<Mutex<HashMap<String, Vec<ExpertAgentStatus>>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

/// 判断任务是否需要多专家（并行/分工/组队检测）
pub fn detect_multi_expert_task(task: &str) -> bool {
    let t = task.to_lowercase();

    // 并行关键词
    let parallel_kw = ["并行", "同时", "分别", "各自", "拆成", "分工", "团队", "组队", "多人", "多步"];
    for kw in &parallel_kw {
        if t.contains(*kw) {
            return true;
        }
    }

    // 列表式任务: 3+ 子任务以换行/bullet 分割
    let lines: Vec<_> = task
        .split(|c| c == '\n' || c == '\r')
        .filter(|l| !l.trim().is_empty())
        .collect();
    // 统计看起来像子任务项的行(以 bullet/数字/顿号开头)
    let bullet_count = lines.iter().filter(|l| {
        let l = l.trim();
        l.starts_with('-') || l.starts_with('*') || l.starts_with('·')
            || l.starts_with('●') || l.starts_with('○')
            || (l.len() > 1 && l.chars().next().unwrap().is_numeric())
            || l.starts_with('1') || l.starts_with('2') || l.starts_with('3')
            || l.starts_with('①') || l.starts_with('②') || l.starts_with('③')
    }).count();

    bullet_count >= 3 || lines.len() >= 3
}

/// 召集专家团
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn expert_team_spawn(
    project_id: String,
    task_description: String,
) -> Vec<ExpertMatch> {
    let matches = expert_route(RouteRequest {
        query: task_description.clone(),
        limit: Some(5),
        group_filter: None,
    });

    // 初始化项目团队状态
    {
        let mut teams = EXPERT_TEAMS.lock().unwrap();
        if !teams.contains_key(&project_id) {
            let initial: Vec<ExpertAgentStatus> = matches
                .iter()
                .map(|m| ExpertAgentStatus {
                    expert_id: m.expert.id.clone(),
                    name: m.expert.name.clone(),
                    status: "idle".into(),
                    last_active: Utc::now().to_rfc3339(),
                })
                .collect();
            teams.insert(project_id.clone(), initial);
        }
    }

    // 标记主选/备选: 前2名为主选(is_primary=true)，其余备选
    let mut result = Vec::new();
    for (i, m) in matches.into_iter().enumerate() {
        let mut m = m;
        m.is_primary = i < 2;
        result.push(m);
    }
    result
}

/// 查询项目当前专家团状态
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn expert_agents_status(project_id: String) -> Vec<ExpertAgentStatus> {
    let teams = EXPERT_TEAMS.lock().unwrap();
    teams.get(&project_id).cloned().unwrap_or_default()
}

/// 全量专家列表
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn expert_list() -> Vec<ExpertCard> {
    all_experts()
}

/// 按分组获取专家
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn expert_list_by_group(group: String) -> Vec<ExpertCard> {
    all_experts()
        .into_iter()
        .filter(|e| e.group == group)
        .collect()
}

/// 全部分组列表。计数从裁撤后的真实花名册动态统计（避免与 RETIRED 不一致）。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn expert_groups() -> Vec<ExpertGroup> {
    let meta: &[(&str, &str, &str)] = &[
        ("orchestration", "编排/统帅", "🧭"),
        ("system_arch", "系统架构", "🏛"),
        ("language", "语言专精", "⌨"),
        ("frontend", "前端/移动", "📱"),
        ("devops", "DevOps/基础设施", "⚙"),
        ("data", "数据", "📊"),
        ("ai_ml", "AI/机器学习", "🧠"),
        ("security", "安全/合规", "🛡"),
        ("quality", "质量/治理", "🔬"),
        ("specialty", "专项技术", "🧩"),
        ("docs", "文档/技术写作", "📝"),
        ("product", "产品/项目/战略", "📐"),
        ("research", "研究/分析", "🔎"),
        ("marketing", "营销/内容", "📣"),
    ];
    let experts = all_experts_ref();
    meta.iter()
        .map(|(id, name, icon)| ExpertGroup {
            id: (*id).into(),
            name: (*name).into(),
            icon: (*icon).into(),
            count: experts.iter().filter(|e| e.group == *id).count(),
        })
        .collect()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpertGroup {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub count: usize,
}

/// 两路召回 + RRF 融合，返回按分降序的 (id, 原始分)。
fn ranked_scores(query_lower: &str, experts: &[ExpertCard]) -> Vec<(String, f32)> {
    let signal_scores = signal_match_score(query_lower, experts);
    let keyword_scores = keyword_match_score(query_lower, experts);
    let rrf = rrf_fuse(&signal_scores, &keyword_scores, 60.0);
    let mut v: Vec<(String, f32)> = rrf.into_iter().collect();
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Less));
    v
}

/// 专家路由 — RRF 召回 + 信号命中。
/// similarity 归一化到 0..1（除以本批最高分），便于前端进度条 / 阈值判断；
/// is_primary 用「排名前 2」而非绝对分阈值（RRF 原始分很小，绝对阈值会永远不成立）。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn expert_route(req: RouteRequest) -> Vec<ExpertMatch> {
    let limit = req.limit.unwrap_or(5);
    let query_lower = req.query.to_lowercase();
    let experts = all_experts_ref();

    let ranked = ranked_scores(&query_lower, experts);
    let max = ranked.first().map(|(_, s)| *s).unwrap_or(0.0);

    ranked
        .into_iter()
        .take(limit)
        .enumerate()
        .map(|(rank, (id, score))| {
            let expert = experts.iter().find(|e| e.id == id).unwrap();
            let hit_signals = find_hit_signals(expert, &query_lower);
            let similarity = if max > 0.0 { (score / max).min(1.0) } else { 0.0 };
            ExpertMatch {
                expert: expert.clone(),
                hit_signals,
                similarity,
                complements: expert.complements.to_string(),
                is_primary: rank < 2 && score > 0.0,
            }
        })
        .collect()
}

/// 单专家路由（指定 id）
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn expert_get(id: String) -> Option<ExpertCard> {
    all_experts_ref().iter().find(|e| e.id == id).cloned()
}

/// 智能匹配 — 根据用户描述自动路由最合适专家
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn expert_match_auto(query: String) -> Vec<ExpertMatch> {
    expert_route(RouteRequest {
        query,
        limit: Some(3),
        group_filter: None,
    })
}

/// 返回专家（或专家团 id）头像的 base64 Data URL（供前端 <img src> 直接使用）。
/// 9 张卡通形象编译期内嵌，按 id 稳定散列分配，打包后任何环境都能出图。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn expert_avatar(id: String) -> Option<String> {
    Some(avatars::avatar_data_url(&id))
}

/// 一次性取全部 9 张头像（Data URL，按槽位 0..9）。前端拉一次 + 本地 FNV-1a 槽位散列，
/// 把 100+ 张卡片的逐个取头像 IPC 收成一次，治列表卡顿。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn expert_avatar_slots() -> Vec<String> {
    avatars::avatar_slots()
}

/// 把某专家的 CLAUDE.md 模板应用到指定项目：写入该项目 CLAUDE.md + 记录 persona_id。
/// `overwrite=false` 且已有非占位内容时拒绝覆盖（交前端二次确认后再 true）。
///
/// 专家团预设（如 "team-general"）会写成战略师领衔的编排型 CLAUDE.md，
/// 与 persona_apply 走同一条写 CLAUDE.md 链路；区别是 expert_apply 读模板文件，
/// persona_apply 用编译期内嵌的 preset body。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn expert_apply(
    project_id: String,
    expert_id: String,
    overwrite: bool,
) -> Result<(), String> {
    // 查找专家
    let expert = all_experts_ref()
        .iter()
        .find(|e| e.id == expert_id)
        .cloned()
        .ok_or_else(|| format!("未知专家: {}", expert_id))?;

    // 用专家元数据构建完整的 CLAUDE.md 正文
    let body = expert_docs::build_expert_doc(
        &expert.claude_md_ref,
        &expert.name,
        &expert.role,
        &expert.description,
        &expert.keywords,
        &expert.capabilities,
        &expert.trigger_signals,
        &expert.complements,
        &expert.exclusive_with,
        expert.cost_tier,
    )
    .ok_or_else(|| format!("专家模板构建失败: {}", expert.claude_md_ref))?;

    // 项目 CLAUDE.md 路径（复用人格模块的同一路径）
    let path = project_claude_md_path(&project_id).ok_or("无法确定项目路径")?;
    if !overwrite && path.exists() {
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if !existing.trim().is_empty()
            && !existing.contains(crate::claude_md::PLACEHOLDER_MARKER)
        {
            return Err("该项目已有人格内容，确认覆盖请重试。".into());
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, body).map_err(|e| e.to_string())?;

    // 记录到项目状态（与 persona_apply 共用同一个状态字段）
    crate::conv::set_project_persona(&project_id, Some(expert_id.clone()), None);
    Ok(())
}

/// 项目 CLAUDE.md 路径（须与 persona::project_claude_md_path 一致）
fn project_claude_md_path(project_id: &str) -> Option<std::path::PathBuf> {
    use directories::UserDirs;
    if !crate::conv::is_safe_project_id(project_id) {
        return None;
    }
    let user = UserDirs::new()?;
    Some(
        user.home_dir()
            .join("Polaris")
            .join("projects")
            .join(project_id)
            .join("CLAUDE.md"),
    )
}

// ───────────────────────── 路由算法 ─────────────────────────

fn signal_match_score(query: &str, experts: &[ExpertCard]) -> HashMap<String, f32> {
    let mut scores: HashMap<String, f32> = HashMap::new();
    for expert in experts {
        let mut hits = 0;
        for signal in &expert.trigger_signals {
            if query.contains(&signal.to_lowercase()) {
                hits += 1;
            }
        }
        if hits > 0 {
            let raw = hits as f32 / expert.trigger_signals.len() as f32;
            scores.insert(expert.id.clone(), raw);
        }
    }
    scores
}

fn keyword_match_score(query: &str, experts: &[ExpertCard]) -> HashMap<String, f32> {
    let mut scores: HashMap<String, f32> = HashMap::new();
    for expert in experts {
        let mut hits = 0;
        for kw in &expert.keywords {
            if query.contains(&kw.to_lowercase()) {
                hits += 1;
            }
        }
        if hits > 0 {
            scores.insert(expert.id.clone(), hits as f32 * 0.3);
        }
    }
    scores
}

/// RRF (RecipRank Fusion) — 两路分数融合
fn rrf_fuse(signal_scores: &HashMap<String, f32>, keyword_scores: &HashMap<String, f32>, k: f32) -> HashMap<String, f32> {
    let mut combined: HashMap<String, f32> = HashMap::new();

    // 信号路排名
    let mut signal_rank: Vec<_> = signal_scores.iter().collect();
    signal_rank.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Less));
    for (rank, (id, score)) in signal_rank.iter().enumerate() {
        let rrf = 1.0 / (k + (rank + 1) as f32);
        let e = combined.entry((*id).clone()).or_insert(0.0);
        *e += rrf * *score;
    }

    // 关键词路排名
    let mut kw_rank: Vec<_> = keyword_scores.iter().collect();
    kw_rank.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Less));
    for (rank, (id, score)) in kw_rank.iter().enumerate() {
        let rrf = 1.0 / (k + (rank + 1) as f32);
        let e = combined.entry((*id).clone()).or_insert(0.0);
        *e += rrf * *score;
    }

    combined
}

fn find_hit_signals(expert: &ExpertCard, query: &str) -> Vec<String> {
    expert
        .trigger_signals
        .iter()
        .filter(|s| query.contains(&s.to_lowercase()))
        .cloned()
        .collect()
}

// ───────────────────────── 业务专家团 ─────────────────────────

/// 测试用：拿到全量专家（供 teams 模块单测校验成员 id）。
#[cfg(test)]
pub(crate) fn all_experts_for_test() -> Vec<ExpertCard> {
    all_experts()
}

/// 全部业务专家团（领衔 + 成员的成建制队伍）。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn expert_teams() -> Vec<ExpertTeam> {
    teams::all_teams()
}

/// 取单个业务团。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn expert_team_get(id: String) -> Option<ExpertTeam> {
    teams::all_teams().into_iter().find(|t| t.id == id)
}

/// 把某业务团应用到项目：组装战略师领衔的编排型 CLAUDE.md 写入项目，记录到 persona_id。
/// 与 expert_apply / persona_apply 同一条写 CLAUDE.md 链路。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn team_apply(project_id: String, team_id: String, overwrite: bool) -> Result<(), String> {
    let team = teams::all_teams()
        .into_iter()
        .find(|t| t.id == team_id)
        .ok_or_else(|| format!("未知专家团: {}", team_id))?;
    let experts = all_experts_ref();
    let lead = experts
        .iter()
        .find(|e| e.id == team.lead_id)
        .ok_or_else(|| format!("团领衔专家缺失: {}", team.lead_id))?;
    let members: Vec<ExpertCard> = team
        .member_ids
        .iter()
        .filter_map(|mid| experts.iter().find(|e| &e.id == mid).cloned())
        .collect();

    let body = teams::build_team_doc(&team, lead, &members);

    let path = project_claude_md_path(&project_id).ok_or("无法确定项目路径")?;
    if !overwrite && path.exists() {
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if !existing.trim().is_empty() && !existing.contains(crate::claude_md::PLACEHOLDER_MARKER) {
            return Err("该项目已有专家内容，确认覆盖请重试。".into());
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, body).map_err(|e| e.to_string())?;

    // 初始化团队看板状态（lead + members 全 idle）
    {
        let mut teams_state = EXPERT_TEAMS.lock().unwrap();
        let mut roster: Vec<ExpertAgentStatus> = Vec::new();
        roster.push(ExpertAgentStatus {
            expert_id: lead.id.clone(),
            name: lead.name.clone(),
            status: "idle".into(),
            last_active: Utc::now().to_rfc3339(),
        });
        for m in &members {
            roster.push(ExpertAgentStatus {
                expert_id: m.id.clone(),
                name: m.name.clone(),
                status: "idle".into(),
                last_active: Utc::now().to_rfc3339(),
            });
        }
        teams_state.insert(project_id.clone(), roster);
    }

    crate::conv::set_project_persona(&project_id, Some(team_id), None);
    Ok(())
}

/// 「下载」某专家：返回该专家完整的 CLAUDE.md 文本，前端可保存成文件。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn expert_export(id: String) -> Result<String, String> {
    let expert = all_experts_ref()
        .iter()
        .find(|e| e.id == id)
        .cloned()
        .ok_or_else(|| format!("未知专家: {}", id))?;
    expert_docs::build_expert_doc(
        &expert.claude_md_ref,
        &expert.name,
        &expert.role,
        &expert.description,
        &expert.keywords,
        &expert.capabilities,
        &expert.trigger_signals,
        &expert.complements,
        &expert.exclusive_with,
        expert.cost_tier,
    )
    .ok_or_else(|| format!("专家文档构建失败: {}", id))
}

/// 「下载」某业务团：返回该团完整的编排型 CLAUDE.md 文本。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn team_export(id: String) -> Result<String, String> {
    let team = teams::all_teams()
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| format!("未知专家团: {}", id))?;
    let experts = all_experts_ref();
    let lead = experts
        .iter()
        .find(|e| e.id == team.lead_id)
        .ok_or_else(|| format!("团领衔专家缺失: {}", team.lead_id))?;
    let members: Vec<ExpertCard> = team
        .member_ids
        .iter()
        .filter_map(|mid| experts.iter().find(|e| &e.id == mid).cloned())
        .collect();
    Ok(teams::build_team_doc(&team, lead, &members))
}

/// 路由调试一行：每个候选专家的命中明细，给「调试」面板用。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ExpertDebugRow {
    pub id: String,
    pub name: String,
    pub group: String,
    pub hit_signals: Vec<String>,
    pub similarity: f32,
    pub would_select: bool,
}

/// 调试某条查询的智能匹配：返回所有命中专家的打分明细（按分降序）。
/// 让用户能看清「为什么这个被选 / 那个没被选」，便于调信号词。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn expert_route_debug(query: String) -> Vec<ExpertDebugRow> {
    let q = query.to_lowercase();
    let experts = all_experts_ref();
    let ranked = ranked_scores(&q, experts);
    let max = ranked.first().map(|(_, s)| *s).unwrap_or(0.0);

    ranked
        .into_iter()
        .enumerate()
        .map(|(rank, (id, score))| {
            let e = experts.iter().find(|e| e.id == id).unwrap();
            ExpertDebugRow {
                id: e.id.clone(),
                name: e.name.clone(),
                group: e.group.clone(),
                hit_signals: find_hit_signals(e, &q),
                similarity: if max > 0.0 { (score / max).min(1.0) } else { 0.0 },
                // 排名前 3 且有命中即视为「会被召集」（绝对分阈值对 RRF 不成立）
                would_select: rank < 3 && score > 0.0,
            }
        })
        .collect::<Vec<_>>()
}

/// 给 chat.rs 用的智能匹配注入块：根据用户消息路由出最合适的 1~3 位专家，
/// 生成「召集这些专家 + 为什么 + 成本纪律」的提示片段。无命中则返回 None（不污染普通对话）。
pub fn route_block(query: &str) -> Option<String> {
    // expert_route 只返回有命中（score>0）的专家；空 = 闲聊/无领域信号 → 不注入。
    let hit = expert_route(RouteRequest {
        query: query.to_string(),
        limit: Some(3),
        group_filter: None,
    });
    if hit.is_empty() {
        return None;
    }
    let primary = &hit[0].expert;
    let mut s = String::new();
    s.push_str(
        "【智能匹配·专家团】本轮自动匹配到主理专家。请**严格以下述专家的标准、品味与方法**作答 —— \
         这是该专家完整的工作准则,优先级高于泛泛的通用风格(若与默认风格冲突,以专家准则为准):\n\n",
    );
    s.push_str(&format!("# 主理专家:{}（{}）\n\n", primary.name, primary.role));
    // ★关键:注入该专家**完整提示词正文**(来自 templates/experts/<group>/<id>.md,可本地编辑),
    //   而不是过去那一行"命中/补维度"标签 —— 否则改了提示词也驱动不了模型。
    if let Some(body) = expert_docs::build_expert_doc(
        &primary.claude_md_ref,
        &primary.name,
        &primary.role,
        &primary.description,
        &primary.keywords,
        &primary.capabilities,
        &primary.trigger_signals,
        &primary.complements,
        &primary.exclusive_with,
        primary.cost_tier,
    ) {
        s.push_str(&body);
        s.push('\n');
    }
    // 备选(其余命中)只给一行,供主理专家需要时借力,不喧宾夺主。
    if hit.len() > 1 {
        s.push_str("\n## 可借力的备选专家\n");
        for m in &hit[1..] {
            s.push_str(&format!(
                "- **{}**（{}）— 补「{}」\n",
                m.expert.name, m.expert.role, m.complements
            ));
        }
    }
    s.push_str(
        "\n纪律:以主理专家的标准为准、备选为辅;任务简单就主理专家一人直接干到好,\
         确需分工且并行有收益时才召备选(一次≤4~5 人,紧耦合则串行)。",
    );
    Some(s)
}

// ───────────────────────── 让 AI 更懂你：按知识库反推专家团 ─────────────────────────

/// 知识库 → 专家团推荐结果。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct KbRecommendation {
    /// 最匹配的业务团（信号太弱时为 None → 退回智能匹配默认）
    pub team: Option<ExpertTeam>,
    /// 推荐理由（可读，给用户看「为什么是这支团」）
    pub reason: String,
    /// 整体最匹配的前若干专家
    pub top_experts: Vec<ExpertCard>,
    /// 从知识库里识别出的主题词（驱动推荐的信号）
    pub matched_topics: Vec<String>,
    /// 参与分析的知识库文档数
    pub corpus_size: usize,
}

/// 把知识库文件清单压成一段「主题语料」：目录/文件名里就含强领域信号
/// （如 raw/股票、raw/数学、raw/毛主席），无需读正文即可反推领域。
fn kb_corpus(scope: Option<String>) -> (String, usize, Vec<String>) {
    // 大库保护：单遍取前 4000 条路径（领域信号早已饱和）+ 总数，
    // 避免在巨型 KB 上把几百万条路径全克隆出来再丢弃。语料另有字符封顶。
    let (paths, n) = crate::kb::kb_list_sample(scope, 4000);
    let mut topics: Vec<String> = Vec::new();
    let mut corpus = String::new();
    for p in paths.iter() {
        if corpus.len() > 200_000 {
            break;
        }
        // 取每段路径分量（尤其顶层目录名 = 领域），喂进语料
        for seg in p.split(|c| c == '/' || c == '\\') {
            let seg = seg
                .trim_end_matches(".md")
                .trim_end_matches(".txt")
                .trim();
            if seg.is_empty() || seg == "raw" || seg == "wiki" {
                continue;
            }
            corpus.push_str(seg);
            corpus.push(' ');
            // 顶层目录类的短词当主题候选收集（去重，限量）
            if seg.chars().count() <= 8 && !topics.iter().any(|t| t == seg) && topics.len() < 24 {
                topics.push(seg.to_string());
            }
        }
    }
    (corpus, n, topics)
}

/// 按知识库内容反推「该配哪支专家团」，并给出理由。让平台 AI 更懂用户。
/// `scope` 可限定知识库子目录（如某项目绑定的 raw/股票）；None = 全库。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn expert_recommend_from_kb(scope: Option<String>) -> KbRecommendation {
    let (corpus, corpus_size, topics) = kb_corpus(scope);
    let q = corpus.to_lowercase();
    let experts = all_experts_ref();

    // 整体最匹配专家（复用路由打分）
    let signal = signal_match_score(&q, experts);
    let keyword = keyword_match_score(&q, experts);
    let rrf = rrf_fuse(&signal, &keyword, 60.0);
    let mut ranked: Vec<(&String, &f32)> = rrf.iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Less));
    let top_ids: Vec<String> = ranked.iter().take(10).map(|(id, _)| (*id).clone()).collect();
    let top_experts: Vec<ExpertCard> = top_ids
        .iter()
        .take(3)
        .filter_map(|id| experts.iter().find(|e| &e.id == id).cloned())
        .collect();

    // 给每支团打分：成员命中 top 专家 ×2 + 团标签命中语料 ×1
    let teams = teams::all_teams();
    let mut best: Option<(ExpertTeam, i32)> = None;
    for team in teams {
        let mut score = 0i32;
        let roster: Vec<&String> = std::iter::once(&team.lead_id).chain(team.member_ids.iter()).collect();
        for r in &roster {
            if top_ids.iter().any(|t| &t == r) {
                score += 2;
            }
        }
        for tag in &team.tags {
            if q.contains(&tag.to_lowercase()) {
                score += 1;
            }
        }
        if best.as_ref().map(|(_, s)| score > *s).unwrap_or(true) {
            best = Some((team, score));
        }
    }

    // 主题词：命中了 top 专家触发信号的那些，叠加目录主题词
    let mut matched_topics: Vec<String> = Vec::new();
    for e in &top_experts {
        for s in find_hit_signals(e, &q) {
            if !matched_topics.iter().any(|t| t == &s) {
                matched_topics.push(s);
            }
        }
    }
    for t in topics.into_iter().take(8) {
        if !matched_topics.iter().any(|x| x == &t) {
            matched_topics.push(t);
        }
    }

    match best {
        Some((team, score)) if score >= 2 && corpus_size > 0 => {
            let names: Vec<&str> = top_experts.iter().map(|e| e.name.as_str()).collect();
            let reason = format!(
                "你的知识库里出现了{}等主题，与「{}」最契合（领衔 + 成员覆盖了 {} 等专家）。一键入驻即可让对话默认带上这支团的视角。",
                if matched_topics.is_empty() { "相关".to_string() } else { matched_topics.iter().take(4).cloned().collect::<Vec<_>>().join("、") },
                team.name,
                names.join("、"),
            );
            KbRecommendation { team: Some(team), reason, top_experts, matched_topics, corpus_size }
        }
        _ => KbRecommendation {
            team: None,
            reason: if corpus_size == 0 {
                "知识库还是空的——先往知识库放些资料，我就能反推你需要哪支专家团。当前默认「智能匹配」每轮按你的话自动召集专家。".into()
            } else {
                "知识库领域信号还不够强，暂用「智能匹配」默认模式（每轮按你的话自动召集最合适的专家）。资料更丰富后这里会给出更精准的团队推荐。".into()
            },
            top_experts,
            matched_topics,
            corpus_size,
        },
    }
}

// ───────────────────────── Tauri commands ─────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 一次性把全部专家导出成各自的可编辑 .md(放仓库源码,以后调它就是调这些文件)。
    /// 已存在的文件不覆盖(保住手写的 visual-designer 等)。跑法:
    ///   cargo test seed_expert_files -- --ignored --nocapture
    #[test]
    #[ignore]
    fn seed_expert_files() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/templates");
        let mut written = 0;
        for e in all_experts() {
            let path = root.join(&e.claude_md_ref); // experts/<group>/<id>.md
            if path.exists() {
                continue; // 不覆盖已手写的专属提示词
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            // 文件不存在 → build_expert_doc 回落 GENERIC 骨架并填好该专家元数据
            let body = expert_docs::build_expert_doc(
                &e.claude_md_ref,
                &e.name,
                &e.role,
                &e.description,
                &e.keywords,
                &e.capabilities,
                &e.trigger_signals,
                &e.complements,
                &e.exclusive_with,
                e.cost_tier,
            )
            .unwrap();
            std::fs::write(&path, body).unwrap();
            written += 1;
        }
        println!("seeded {} expert files", written);
    }

    #[test]
    fn all_experts_count() {
        // 2026-06-16 审计后裁撤约 33 个冗余/冷门专家（见 RETIRED），常驻精简到 ~72。
        let count = all_experts().len();
        assert!(count >= 60, "专家数量应 >= 60，实际 {}", count);
        assert!(
            !all_experts().iter().any(|e| RETIRED.contains(&e.id.as_str())),
            "已裁撤专家不应出现在花名册"
        );
    }

    #[test]
    fn routing_returns_results() {
        let results = expert_route(RouteRequest {
            query: "帮我做一个带支付的 SaaS 落地页，要好看，并发上线".into(),
            limit: Some(5),
            group_filter: None,
        });
        assert!(!results.is_empty(), "路由应返回结果");
        for r in &results {
            assert!(!r.expert.trigger_signals.is_empty(), "{} 缺 trigger_signals", r.expert.id);
        }
    }

    #[test]
    fn auto_match_returns_primary() {
        let results = expert_match_auto("帮我做个支付功能".into());
        assert!(!results.is_empty());
        // 主选应该有较高的相似度
        let primary = &results[0];
        assert!(primary.similarity > 0.0);
    }

    #[test]
    fn all_experts_have_unique_ids() {
        let ids: Vec<_> = all_experts().iter().map(|e| e.id.clone()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(ids.len(), sorted.len(), "专家 id 不应重复");
    }

    #[test]
    fn expert_groups_complete() {
        let groups = expert_groups();
        let total: usize = groups.iter().map(|g| g.count).sum();
        assert_eq!(total, all_experts().len(), "分组计数应等于专家总数");
    }

    /// 归一化：最匹配的相似度应为 1.0，且前 2 名 is_primary（治「绝对阈值永不成立」回归）。
    #[test]
    fn route_normalizes_and_marks_primary() {
        let results = expert_route(RouteRequest {
            query: "帮我接入支付，做对账和订阅计费".into(),
            limit: Some(5),
            group_filter: None,
        });
        assert!(!results.is_empty());
        assert!((results[0].similarity - 1.0).abs() < 1e-6, "首位相似度应归一化为 1.0");
        assert!(results[0].is_primary, "首位应为主选");
        for r in &results {
            assert!(r.similarity >= 0.0 && r.similarity <= 1.0, "相似度应在 0..1");
        }
    }

    /// debug：有命中查询应至少一行 would_select=true（治「召集徽标永不出现」）。
    #[test]
    fn route_debug_has_selectable() {
        let rows = expert_route_debug("做一个带支付的前端落地页".into());
        assert!(!rows.is_empty(), "应有命中行");
        assert!(rows.iter().any(|r| r.would_select), "应至少一行 would_select");
        assert!(rows[0].similarity <= 1.0);
    }

    /// route_block：明确领域查询应注入；纯闲聊不应注入。
    #[test]
    fn route_block_fires_on_domain_not_chitchat() {
        assert!(route_block("帮我写个 python 异步爬虫").is_some(), "领域查询应注入专家块");
        assert!(route_block("嗯嗯好的谢谢你").is_none(), "闲聊不应注入");
    }

    /// route_block 现在注入主理专家**完整提示词正文**(来自可编辑的 .md),而非过去的一行标签。
    /// "ppt" 应路由到视觉设计,并把 visual-designer.md 的实质内容带进来 —— 这才是「真的匹配上有
    /// 审美的人格、且让它的准则驱动模型」。
    #[test]
    fn route_block_injects_full_expert_prompt() {
        let block = route_block("你帮我写一个这个的ppt").expect("ppt 应命中专家");
        assert!(block.contains("视觉设计"), "应路由到视觉设计专家");
        assert!(
            block.contains("演示美学") || block.contains("大字少字"),
            "应注入 visual-designer.md 的实质提示词(证明吃的是文件全文而非标签),实际开头:\n{}",
            &block[..block.len().min(400)]
        );
    }
}
