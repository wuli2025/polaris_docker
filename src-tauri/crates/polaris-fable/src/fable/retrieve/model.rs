//! 检索结果对外数据模型(命令返回体)。

use super::*;

#[derive(Debug, Clone, Serialize)]
pub struct FableHit {
    /// 相对盘点根的路径
    pub path: String,
    pub abspath: String,
    /// "L42" 行号 或 "C3" chunk 序号
    pub location: String,
    pub snippet: String,
    pub score: f32,
    /// 命中车道: grep / vector(融合后可能两者都有)
    pub lanes: Vec<String>,
    /// 若本文件被同目录同名的新版本压制,这里给出新版本的相对路径(供前端标「有新版本」)。
    /// 命中仍返回(降权可达),None = 无更新版本。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FableSearchResult {
    pub query: String,
    pub mode: String,
    pub hits: Vec<FableHit>,
    pub grep_hits: usize,
    pub vector_hits: usize,
    pub reranked: bool,
    /// grep 车道是否因预算截断(命中可能不全,建议 agent 换更窄的定向 Grep)
    pub grep_truncated: bool,
    pub ms: u64,
}
