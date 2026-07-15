//! Skill 系统 — MVP v0.4
//!
//! 统一目录 catalog（编译期内置 + 可安装市场）+ 用户 skill（磁盘持久化，~/Polaris/skills/）
//!
//! - 预装 skill（preinstalled=true）：开箱即用，始终 installed
//! - 市场 skill（preinstalled=false）：列在「市场精选」，点「安装」即复制到用户目录
//! - 用户自建 skill：create_skill 写盘，source = user
//! - 安装 / 创建都会立即出现在技能中心；前端负责安装后自动激活（无需额外授权步骤）

pub(crate) use serde::{Deserialize, Serialize};
pub(crate) use std::collections::HashSet;
pub(crate) use std::fs;
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::process::Command;

mod catalog;
mod commands;
mod intent;
mod seed;
mod store;
mod templates;

pub use catalog::*;
pub use commands::*;
pub use intent::*;
pub use seed::*;
pub use store::*;
pub use templates::*;
