//! 内核→引擎桥 (架构 Phase 0 · 分仓规划 v2 §4 的 KbBridge 依赖倒置落点)
//!
//! chat(未来 kernel 仓)对检索引擎(kb/fable, 未来 polaris-fable 仓)与专家团
//! (expert, 未来 polaris-experts 仓)的全部依赖收口于此: chat 只认这里的 trait 与
//! Lite DTO, 不再 import 任何引擎模块 —— 抽仓时本文件随 kernel 走, 引擎实现由
//! 外壳注入(见 `crate::wiring`), 与 lib.rs 的 `KbLocator`(sandbox 板块)同一模式。
//!
//! 注入时机: 桌面 `run()` setup 与 server `serve()` 的 init 序列(均在任何命令可
//! 执行之前调用 `wiring::wire_engine_bridges()`)。未注入时优雅降级: KB 召回/家底
//! 概览/专家路由静默跳过 —— 与「未拼装该引擎」的极简形态语义一致, 不 panic。
//!
//! 性能: 桥只在**每条消息**组 prompt 时走一次量级(非逐 token 热路径), OnceLock 读
//! + 动态分发的开销为纳秒级; DTO 映射复用的都是本就要 clone 的字段, 零新增拷贝。

use std::sync::{Arc, OnceLock};

// ───────────────────────── 检索引擎桥 (kb + fable 同属未来 fable 仓, 共用一桥) ─────────────────────────

/// `kb::KbHit` 的内核侧投影: 只带 chat 实际消费的字段(score 等引擎细节不过桥)。
pub struct KbHitLite {
    pub title: String,
    pub path: String,
    pub snippet: String,
}

/// fable 混检命中的内核侧投影(标题由调用方从 path 派生, 不过桥)。
pub struct RagHitLite {
    pub path: String,
    pub snippet: String,
}

/// `kb::KbOverview` 的内核侧投影(四层家底计数)。
pub struct KbOverviewLite {
    pub root: String,
    pub wiki: usize,
    pub raw_md: usize,
    pub output: usize,
    pub memory: usize,
}

/// `fable::FableStatus` 的内核侧投影: 只带「索引就绪判定 + 家底概览」用到的三个计数。
pub struct FableStatusLite {
    pub files_total: u64,
    pub chunks_total: u64,
    pub lex_files: u64,
}

pub trait KbBridge: Send + Sync {
    /// KB 根目录(未配置返回空串, 语义同 `kb::kb_root`)。
    fn root(&self) -> String;
    /// 四层家底概览(root 为空表示 KB 未就绪)。
    fn overview(&self) -> KbOverviewLite;
    /// Karpathy 式结构化 wiki 上下文块(语义同 `kb::kb_context_block_scoped`; 空串=无内容)。
    /// claude_md 主上下文渲染用 —— kernel 侧不再 import kb。
    fn context_block_scoped(&self, scope: Option<&str>) -> String;
    /// 检索枢纽注入块(语义同 `fable::agent::fable_context_block`; 空串=未盘点)。
    fn fable_context_block(&self, full: bool) -> String;
    /// 关键词/标题检索同步核(语义同 `kb::kb_search_sync`)。
    fn search_sync(&self, query: String, top_k: Option<usize>) -> Vec<KbHitLite>;
    /// fable 索引状态(打不开 fable.db 等失败情形返回 None)。
    fn fable_status(&self) -> Option<FableStatusLite>;
    /// fable 混检(语义同 `fable::retrieve::search`); Err 折叠为 None, 由调用方走关键词兜底。
    fn rag_search(
        &self,
        query: &str,
        top_k: usize,
        mode: &str,
        scope: Option<&str>,
    ) -> Option<Vec<RagHitLite>>;
}

// ───────────────────────── 专家团桥 (expert, 未来 experts 仓) ─────────────────────────

/// 专家团对 chat 暴露的三个决策点。`ExpertMatch` 等引擎内部类型不过桥:
/// 「召集成队 + 生成分工块」在实现侧一步完成(`team_block_spawn`), 内核只拿最终注入文本。
pub trait ExpertBridge: Send + Sync {
    /// 是否为多专家任务(语义同 `expert::detect_multi_expert_task`)。
    fn detect_multi_expert_task(&self, prompt: &str) -> bool;
    /// 召集专家团并生成分工注入块(组合 `expert_team_spawn` + `team_block`)。
    fn team_block_spawn(&self, project_id: String, prompt: String) -> Option<String>;
    /// 智能匹配单/少量专家的视角注入块(语义同 `expert::route_block`)。
    fn route_block(&self, prompt: &str) -> Option<String>;
}

// ───────────────────────── 注入点 ─────────────────────────

static KB_BRIDGE: OnceLock<Arc<dyn KbBridge>> = OnceLock::new();
static EXPERT_BRIDGE: OnceLock<Arc<dyn ExpertBridge>> = OnceLock::new();

/// 外壳启动时注入检索引擎实现(重复注入被忽略, 幂等)。
pub fn set_kb_bridge(b: Arc<dyn KbBridge>) {
    let _ = KB_BRIDGE.set(b);
}

/// 外壳启动时注入专家团实现(重复注入被忽略, 幂等)。
pub fn set_expert_bridge(b: Arc<dyn ExpertBridge>) {
    let _ = EXPERT_BRIDGE.set(b);
}

pub(crate) fn kb_bridge() -> Option<&'static Arc<dyn KbBridge>> {
    KB_BRIDGE.get()
}

pub(crate) fn expert_bridge() -> Option<&'static Arc<dyn ExpertBridge>> {
    EXPERT_BRIDGE.get()
}
