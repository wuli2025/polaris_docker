//! 检索命令(薄包装):桌面 async + spawn_blocking(避免嵌入抢锁冻 UI 主线程被判无响应),
//! server flavor 同步直调。契约保持 `fable::retrieve::fable_search[_ai]` 路径不变。

use super::*;

/// 混检命令。桌面端 async + spawn_blocking:hybrid 检索要做 grep + 向量 + 重排,且
/// 查询嵌入会去抢后台索引正持有的 embedder 全局锁——这一等若发生在 Tauri 主线程上,
/// WebView 消息泵停摆 >5s 就被判「无响应」强杀。挪到阻塞线程池等锁,UI 始终不冻。
/// server flavor 无 UI 主线程、dispatch_sync 本就在 spawn_blocking 中,保持同步。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn fable_search(
    query: String,
    top_k: Option<usize>,
    mode: Option<String>,
    scope: Option<String>,
) -> Result<FableSearchResult, String> {
    tauri::async_runtime::spawn_blocking(move || fable_search_sync(query, top_k, mode, scope))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
pub fn fable_search(
    query: String,
    top_k: Option<usize>,
    mode: Option<String>,
    scope: Option<String>,
) -> Result<FableSearchResult, String> {
    fable_search_sync(query, top_k, mode, scope)
}

/// 内层同步实现:两个 flavor 共用,避免重复校验逻辑。
fn fable_search_sync(
    query: String,
    top_k: Option<usize>,
    mode: Option<String>,
    scope: Option<String>,
) -> Result<FableSearchResult, String> {
    let mode = mode.unwrap_or_else(|| "hybrid".into());
    if !["hybrid", "grep", "vector"].contains(&mode.as_str()) {
        return Err("mode 只接受 hybrid | grep | vector".into());
    }
    let scope = scope.as_deref().map(str::trim).filter(|s| !s.is_empty());
    search(query.trim(), top_k.unwrap_or(12), &mode, scope)
}

/// **AI 辅助检索命令**(深度档):原查询 hybrid + claude 多查询扩写融合(见 [`search_ai_sync`])。
/// 起 headless claude 数秒级,故只在用户主动「深度搜索」时调,不进默认每次检索。失败优雅退回混检。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn fable_search_ai(
    query: String,
    top_k: Option<usize>,
    scope: Option<String>,
) -> Result<FableSearchResult, String> {
    tauri::async_runtime::spawn_blocking(move || fable_search_ai_sync(query, top_k, scope))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
pub fn fable_search_ai(
    query: String,
    top_k: Option<usize>,
    scope: Option<String>,
) -> Result<FableSearchResult, String> {
    fable_search_ai_sync(query, top_k, scope)
}

fn fable_search_ai_sync(
    query: String,
    top_k: Option<usize>,
    scope: Option<String>,
) -> Result<FableSearchResult, String> {
    let scope = scope.as_deref().map(str::trim).filter(|s| !s.is_empty());
    search_ai_sync(query.trim(), top_k.unwrap_or(12), scope)
}
