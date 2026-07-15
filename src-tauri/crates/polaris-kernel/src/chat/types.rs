//! 对话核心的 serde 类型: 命令入参 / 流事件。
//! (从 chat.rs 纯移动拆出, 逻辑零变化)

use serde::{Deserialize, Serialize};

// ───────────────────────── Types ─────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Manual,
    AutoCurrent,
    AutoAll,
    Deny,
}

impl PermissionMode {
    pub(crate) fn cli_value(&self) -> &'static str {
        match self {
            PermissionMode::Manual => "default",
            PermissionMode::AutoCurrent => "acceptEdits",
            // AutoAll 不再 bypass permissions，与 AutoCurrent 一致
            PermissionMode::AutoAll => "acceptEdits",
            PermissionMode::Deny => "plan",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSendArgs {
    pub prompt: String,
    pub permission_mode: PermissionMode,
    #[serde(default)]
    pub use_sandbox: bool,
    #[serde(default)]
    pub skill_ids: Option<Vec<String>>,
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// 目标模式：完成条件。设置后注入「持续推进直到达成」指令。
    #[serde(default)]
    pub goal: Option<String>,
    /// 「动态编排」：把本轮当成多智能体编排——编排器拆成 N 个独立子任务，
    /// 用 Task 子代理并行扇出，每条流水线 实现→对抗式校验→修复，最后汇总。
    #[serde(default)]
    pub dynamic_workflow: bool,
    /// 「知识库严格搜索」：打开时才把 KB 结构化 wiki + 双链地图注入上下文。
    /// 默认 false 以节省 token，日常任务不注入大段 KB 导航。
    #[serde(default)]
    pub use_kb: bool,
    /// 「分批长任务」：把一次超长生成(如 60 页 PPT)拆成多轮有界批次。
    /// 注入分批构建协议——先产 `polaris.build.json` 计划清单, 每轮只建 ≤batch_size 个
    /// pending 单元并回写状态; 由前端编排循环驱动多轮、断线从清单下一个 pending 续跑。
    /// 缘由: 单轮把 60 页全吐完会让流式连接跑太久被掐(socket closed → exit 1), 分批让
    /// 每轮输出有界、context 不随页数膨胀、崩了也不丢已落盘的批次。
    #[serde(default)]
    pub batch_build: bool,
    /// 每批最多构建几个单元(页/章/文件)。None 时用默认值。
    #[serde(default)]
    pub batch_size: Option<usize>,
    /// 智能体模式: "single" | "expert-team" | "auto-match"
    /// 专家团模式下自动检测任务复杂度，必要时注入多专家召集信息。
    #[serde(default)]
    pub agent_mode: Option<String>,
    /// 工作模式: "fast"(默认·快速) | "work"(工作·纯 Claude Code)。
    /// - 快速模式: 强制快速调用知识库(快档召回, 跳重排 ~1.8s→~0.25s)+ 快速回答; 工具精简
    ///   (弃 Task/NotebookEdit)、提示词瘦身(跳「可运行项目」「长任务」约定)、上下文预算调小、
    ///   权限默认自动批准 —— 一切为「秒级查库 + 秒级回答」。
    /// - 工作模式: 纯 Claude Code —— 放开全套工具、注入全部约定(可运行项目/长任务)、KB 召回走
    ///   全质量 hybrid(带重排), 面向写代码/跑项目/产出复杂成品。
    /// None 视为 fast —— 不带此字段的旧调用方(如分批长任务)默认走快速预设。
    #[serde(default)]
    pub work_mode: Option<String>,
    /// 本对话选定的供应商 id(来自左下角「API 供应商」中心, 自动识别已配的那些)。
    /// None / "" / "auto" = Auto 档(沿用应用全局当前供应商)。具体 id = 本对话钉死这家,
    /// 逐命令注入其 env 实现「每个对话各用各的 API」真隔离, 与全局开关、其它并发对话解耦。
    #[serde(default)]
    pub provider_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatStreamEvent {
    pub req_id: String,
    pub kind: String, // delta | tool | error | done
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
}
