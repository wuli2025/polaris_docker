//! 板块 ① 对话核心 — MVP v0.2 (stderr 透传 + 项目/对话历史)
//!
//! 设计依据: PRD-v6 §7
//! - chat_send: 立即返回 req_id; 重活(组装 prompt/KB 注入/spawn claude CLI/emit
//!   chat:stream)在后台线程跑(chat_send_pipeline), 事件契约不变
//! - 同时读 stdout + stderr (单独线程), stderr 转 error 事件
//! - child.wait 完成后, 检查 exit code, 非 0 时 emit error
//! - 沙箱模式预检容器是否在运行, 不在时直接返回错误
//! - 整合 conv 模块, 自动写 user/assistant 消息
//!
//! 模块拆分(纯移动): pipeline(主流程) / prompt(提示词) / artifacts(产物) /
//! attach(附件) / types(类型)。原 `crate::chat::xxx` 公有路径经 re-export 保持零变化。

pub mod artifacts;
pub mod attach;
// 内核→引擎桥(kb/fable/expert 依赖倒置端口, 由外壳启动时经 wiring 注入实现)
pub mod bridges;
pub mod pipeline;
pub mod prompt;
pub mod types;

#[cfg(not(feature = "desktop"))]
use crate::host::AppHandle;
use std::process::Command;
#[cfg(feature = "desktop")]
use tauri::AppHandle;

// 原 chat.rs 的公有面原样再导出: lib.rs 的 generate_handler! 与
// server/feishu/project 等外部引用的 `crate::chat::xxx` 路径全部零改动。
pub use artifacts::artifacts_dir;
pub use artifacts::{
    artifact_list, artifact_open_external, artifact_read, artifact_reveal, artifact_search,
    artifact_write, ArtifactEntry, ArtifactPayload, ArtifactSearchHit, ARTIFACT_MARKER_PREFIX,
};
pub use attach::{chat_attach_files, chat_attach_image, AttachedFile};
pub use pipeline::{chat_build_manifest, chat_cancel, chat_send};
pub use types::{ChatSendArgs, ChatStreamEvent, PermissionMode};

// tauri::command 生成的 __cmd__* 宏也要跟着 re-export, generate_handler!(chat::xxx)
// 才能在 chat:: 路径下找到它们(宏与函数是两个名字空间)。
#[cfg(feature = "desktop")]
pub use artifacts::{
    __cmd__artifact_list, __cmd__artifact_open_external, __cmd__artifact_read,
    __cmd__artifact_reveal, __cmd__artifact_search, __cmd__artifact_write,
};
#[cfg(feature = "desktop")]
pub use artifacts::{
    __tauri_command_name_artifact_list, __tauri_command_name_artifact_open_external,
    __tauri_command_name_artifact_read, __tauri_command_name_artifact_reveal,
    __tauri_command_name_artifact_search, __tauri_command_name_artifact_write,
};
#[cfg(feature = "desktop")]
pub use attach::{__cmd__chat_attach_files, __cmd__chat_attach_image};
#[cfg(feature = "desktop")]
pub use attach::{
    __tauri_command_name_chat_attach_files, __tauri_command_name_chat_attach_image,
};
#[cfg(feature = "desktop")]
pub use pipeline::{__cmd__chat_build_manifest, __cmd__chat_cancel, __cmd__chat_send};
#[cfg(feature = "desktop")]
pub use pipeline::{
    __tauri_command_name_chat_build_manifest, __tauri_command_name_chat_cancel,
    __tauri_command_name_chat_send,
};

pub fn init(_app: &AppHandle) -> Result<(), anyhow::Error> {
    Ok(())
}

/// 在系统默认浏览器打开外部链接(回复正文里的 http/https 链接)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn open_url(url: String) -> Result<(), String> {
    let u = url.trim();
    if !(u.starts_with("http://") || u.starts_with("https://")) {
        return Err("仅允许打开 http/https 链接".into());
    }
    #[cfg(target_os = "windows")]
    {
        // rundll32 不解析 &,URL 原样透传(cmd start 会在 & 处截断)
        Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", u])
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(u)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open")
            .arg(u)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
