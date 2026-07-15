//! Docker(server) 外壳的「宿主句柄」shim。
//!
//! 桌面版的引擎模块函数签名里写的是 `app: AppHandle`（tauri），函数体里调用
//! `app.emit("topic", payload)`、`app.clone()`、`app.path().resource_dir()`。
//! server 构建下用 `#[cfg(not(feature = "desktop"))] use crate::host::AppHandle;`
//! 把这些调用原样接到这里——**因此 17 个引擎模块的函数体一行都不用改**，
//! 桌面 / Docker 共用同一份源码（满足「Windows 更新后 Docker 快速更新」）。

use serde::Serialize;
use std::path::PathBuf;
use tokio::sync::broadcast;

/// 一条推给浏览器前端的事件：topic（对应桌面 `listen(topic)`）+ JSON payload。
#[derive(Clone, Debug)]
pub struct Event {
    pub topic: String,
    pub payload: serde_json::Value,
    /// 事件受众:None=广播给所有连接;Some(username)=只投递给该用户(owner 全收)。
    /// 多人协作(/ws 按用户过滤)的最小侵入实现——引擎模块无感,定向事件走 emit_to。
    pub audience: Option<String>,
    /// 预序列化的 WS 帧(`{"topic":…,"payload":…}` 的最终 JSON 串):emit 时序列化**一次**,
    /// N 条 WS 连接共享同一份 Arc,ws_loop 不再对每条连接各跑一遍 serde(热路径主要收益)。
    /// payload 仍保留 Value 形态,供 hosting 的 UI 桥(tauri emit)与调试用。
    pub frame: std::sync::Arc<str>,
}

impl Event {
    /// 构造事件并预序列化一帧。Value 序列化不会失败;兜底帧仅防御性存在。
    pub fn new(topic: String, payload: serde_json::Value, audience: Option<String>) -> Self {
        // 字段序对齐旧实现:旧 `json!({"topic","payload"})` 走 serde_json 的 BTreeMap,
        // 输出是字典序 `payload` 在前;serde 结构体按声明序,故这里也把 payload 放前面,
        // 让新旧帧逐字节一致(防按帧字节签名/哈希的客户端因键序变化失配)。
        #[derive(Serialize)]
        struct Frame<'a> {
            payload: &'a serde_json::Value,
            topic: &'a str,
        }
        let frame: std::sync::Arc<str> = serde_json::to_string(&Frame {
            payload: &payload,
            topic: &topic,
        })
        .unwrap_or_else(|_| "{\"payload\":null,\"topic\":\"\"}".to_string())
        .into();
        Self {
            topic,
            payload,
            audience,
            frame,
        }
    }
}

/// server 模式下替代 `tauri::AppHandle` 的轻量句柄（Clone + Send + Sync）。
/// 内部持有一个广播发送端：所有 emit 都广播给全部 WS 订阅者，前端按 reqId/runId 自行过滤。
#[derive(Clone)]
pub struct AppHandle {
    tx: broadcast::Sender<Event>,
}

impl AppHandle {
    pub fn new(tx: broadcast::Sender<Event>) -> Self {
        Self { tx }
    }

    /// 新建一个订阅端（每个 WS 连接一个）。
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    /// 克隆底层发送端（极少用到；emit 已覆盖绝大多数场景）。
    pub fn sender(&self) -> broadcast::Sender<Event> {
        self.tx.clone()
    }

    /// 对应 `tauri::Emitter::emit`：序列化 payload → 广播。
    /// 无 WS 订阅者时 `send` 返回 Err（频道里暂时没人），按桌面 `let _ = emit` 的语义忽略。
    pub fn emit<S: Serialize>(&self, topic: &str, payload: S) -> Result<(), serde_json::Error> {
        let value = serde_json::to_value(payload)?;
        let _ = self.tx.send(Event::new(topic.to_string(), value, None));
        Ok(())
    }

    /// emit 的 Value 直通版:payload 已是 Value 时跳过 `to_value` 的整树重建
    /// (泛型 emit 对 Value 入参也会经 Serializer 深拷贝一遍)。hosting 的对话流桥
    /// (tauri 字符串 → Value → bus)这类热路径用它。
    pub fn emit_value(&self, topic: &str, payload: serde_json::Value) {
        let _ = self.tx.send(Event::new(topic.to_string(), payload, None));
    }

    /// 定向 emit:只投递给指定用户的连接(以及 owner)。协作事件(任务卡流转、
    /// 打回意见、个人对话流)用这个,避免 A 的对话流推给所有人(方案硬伤2)。
    pub fn emit_to<S: Serialize>(
        &self,
        user: &str,
        topic: &str,
        payload: S,
    ) -> Result<(), serde_json::Error> {
        let value = serde_json::to_value(payload)?;
        let _ = self
            .tx
            .send(Event::new(topic.to_string(), value, Some(user.to_string())));
        Ok(())
    }

    /// 对应 `tauri::Manager::path()`，仅实现引擎用到的 `resource_dir()`。
    pub fn path(&self) -> PathShim {
        PathShim
    }
}

/// 对应 tauri 的 PathResolver 的极简替身。
pub struct PathShim;

impl PathShim {
    /// 资源目录：镜像把 `src-tauri/resources` 拷到 `$POLARIS_RESOURCE_DIR`(默认 `/app/resources`)，
    /// kb.rs `seed_source` 会在其下找 `seed-kb/`（默认资料库种子）。
    pub fn resource_dir(&self) -> Result<PathBuf, std::io::Error> {
        let dir =
            std::env::var("POLARIS_RESOURCE_DIR").unwrap_or_else(|_| "/app/resources".to_string());
        Ok(PathBuf::from(dir))
    }
}
