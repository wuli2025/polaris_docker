//! 外部系统集成:飞书/企微机器人网关、Codex 反代、NAS 挂载。
//! 只做协议对接,不承载业务逻辑;各集成互不依赖。
pub mod codex_proxy;
pub mod feishu;
pub mod nas;
pub mod wecom;
