//! polaris-protocol — 命令契约层(分仓规划 v2 第 1 仓的仓内种子)。
//!
//! 现阶段装两样东西:
//! 1. [`InvokeRequest`] —— `/api/invoke` 的信封(cmd + args),双壳共用;
//! 2. [`Args`] —— 带访问记账的参数读取器: 分发代码读过哪些顶层 key 被记录,
//!    [`Args::unknown_keys`] 给出「客户端传了、但没有任何代码读」的参数名 ——
//!    这正是 `top_k` vs `topK`、`convId` vs `conversationId` 一类拼错名/契约漂移,
//!    此前被静默容忍产生错误业务结果。默认以响应头曝光(零破坏),
//!    `POLARIS_STRICT_ARGS=1` 时由壳层直接 400 拒绝。

use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;

/// `/api/invoke` 请求信封。
#[derive(Deserialize)]
pub struct InvokeRequest {
    pub cmd: String,
    #[serde(default)]
    pub args: Value,
}

/// 严格参数模式: 开启后未知参数直接拒绝(默认关,先观测后收紧)。
pub fn strict_args_enabled() -> bool {
    std::env::var("POLARIS_STRICT_ARGS").map(|v| v == "1").unwrap_or(false)
}

/// 带访问记账的参数读取器。包一层 `Value`,所有按 key 取值都过 [`Args::get`] 记账;
/// Mutex 保证跨 await 的分发路径(desktop 薄包装是 async)仍 Send+Sync。
pub struct Args {
    inner: Value,
    seen: Mutex<HashSet<String>>,
}

impl Args {
    pub fn new(inner: Value) -> Self {
        Self { inner, seen: Mutex::new(HashSet::new()) }
    }

    /// 按顶层 key 取值(记账)。语义同 `Value::get`。
    pub fn get(&self, k: &str) -> Option<&Value> {
        self.seen.lock().insert(k.to_string());
        self.inner.get(k)
    }

    /// 整包移交(记账全部 key): 给「整个 args 反序列化成结构体」的命令用,
    /// 例如 chat_send/nas_save —— 结构体自己负责字段校验,不算未知参数。
    pub fn take_all(&self) -> &Value {
        if let Some(o) = self.inner.as_object() {
            let mut seen = self.seen.lock();
            for k in o.keys() {
                seen.insert(k.clone());
            }
        }
        &self.inner
    }

    /// 客户端传了、但没有任何分发代码读过的顶层参数名(排序稳定,便于测试断言)。
    pub fn unknown_keys(&self) -> Vec<String> {
        let Some(o) = self.inner.as_object() else { return Vec::new() };
        let seen = self.seen.lock();
        let mut ks: Vec<String> = o.keys().filter(|k| !seen.contains(*k)).cloned().collect();
        ks.sort();
        ks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tracks_unknown_top_level_keys() {
        let a = Args::new(json!({"query":"北极星","top_k":5}));
        let _ = a.get("query");
        let _ = a.get("topK"); // 代码读的是 topK,客户端给的是 top_k → top_k 应被点名
        assert_eq!(a.unknown_keys(), vec!["top_k".to_string()]);
    }

    #[test]
    fn take_all_marks_everything_and_null_args_are_clean() {
        let a = Args::new(json!({"x":1,"y":2}));
        let _ = a.take_all();
        assert!(a.unknown_keys().is_empty());
        assert!(Args::new(Value::Null).unknown_keys().is_empty());
    }
}
