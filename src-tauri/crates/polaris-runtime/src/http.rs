//! HTTP 客户端构造的单一入口(统一 ureq 栈)。
//!
//! 此前 15+ 处各自 `ureq::AgentBuilder::new().timeout(..)`,超时口径五花八门。
//! 这里按用途给三档命名构造;新代码一律走这里,旧调用点随模块重构逐步迁入。

use std::time::Duration;

/// 快探测(健康检查/本地服务):连接 3s / 整体 5s。
pub fn probe_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(3))
        .timeout(Duration::from_secs(5))
        .build()
}

/// 常规 API 调用:连接 10s / 整体 30s。
pub fn api_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
}

/// 慢任务(LLM 推理/大文件/转写):连接 15s / 读 180s。
pub fn slow_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(180))
        .build()
}
