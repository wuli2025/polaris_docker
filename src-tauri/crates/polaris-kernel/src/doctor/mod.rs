//! 板块 ⑦ 环境医生 (Environment Doctor) — 新用户开箱的「环境监测 + 配置安装」
//!
//! 设计目标 (PRD: 新用户点开软件应先过一道环境关):
//! - **监测**: Claude Code (`claude.exe`) 与 PowerShell 7 (`pwsh`) 是否就绪;
//!   附带 Node.js / npm (Claude Code 的可选安装路径) 的探测。
//! - **安装**: Claude Code 没装时一键安装 —— 默认走 **npm + 国内镜像**
//!   `npm i -g @anthropic-ai/claude-code --registry=https://registry.npmmirror.com`:
//!   该包的原生二进制经 `optionalDependencies` (`@anthropic-ai/claude-code-win32-x64`)
//!   同源镜像分发, postinstall 只是把它拷成 `bin/claude.exe` —— 整个安装不碰 claude.ai / GCS,
//!   故**国内可装**。装出的是真·原生 `claude.exe`, chat.rs 解析其全路径直接 spawn。
//!   官方原生脚本 `irm https://claude.ai/install.ps1 | iex` 改作兜底 (国内常被墙, 故不再首选)。
//!   npm 方式需要 Node.js —— 缺失时用 winget 装 Node; PowerShell 7 缺失时同样用 winget。
//! - **改环境变量 (关键)**: Windows 上原生安装把 `claude.exe` 落到
//!   `~/.local/bin`, 但该目录常不在 PATH —— 不修则装了也找不到。这里
//!   **双写**: ① 持久化进「用户 PATH」(注册表, `[Environment]::SetEnvironmentVariable`,
//!   会广播 WM_SETTINGCHANGE), 让以后开的终端/重启后的 app 都能找到;
//!   ② 立刻塞进**当前进程 PATH** (`std::env::set_var`), 让本次会话不重启即可
//!   spawn claude。安装成功后自动执行, 对应「你帮他配置一下 / 一定要记得改环境变量」。
//!
//! 跨平台: 探测两端通用 (Windows 走 where.exe / cmd, 类 Unix 走 which / 直接执行)。
//! 安装 Claude Code **两端默认一致走 npm+npmmirror**: 原生二进制 (win32 / darwin-arm64 /
//! darwin-x64 …) 经 optionalDependencies 由 npmmirror 同源镜像分发, 安装不碰 claude.ai/GCS,
//! 故国内 (含 macOS) 可装; 官方原生脚本 (install.ps1 / install.sh) 因从 claude.ai 拉二进制、
//! 国内常被墙, 仅作「境外网络」兜底。npm 方式需要 Node.js —— 缺失时:
//! Windows 用 winget / 官方 MSI 装, **macOS 免 sudo 下载官方 darwin tar.gz (npmmirror 镜像)**
//! 解压到 `~/.local/polaris-node` 并写 shell 配置。经 `build_install_shell` 选 PowerShell 或 sh。
//! 持久化 PATH: Windows 写注册表用户 PATH; macOS·Linux 写 `~/.zshrc` 等 shell 配置。

pub mod check;
pub mod install;
pub mod path;
pub mod probe;
pub mod types;
pub mod update;
pub mod uv_cache;

// tauri::command 生成的 __cmd__* / __tauri_command_name_* 隐藏宏项会被 glob 一并带出,
// 故 lib.rs 的 generate_handler!(doctor::xxx) 路径零改动。原 doctor.rs 的全部对外符号
// (crate::doctor::xxx) 经下列 glob re-export 保持不变。
pub use check::*;
pub use install::*;
pub use path::*;
pub use probe::*;
pub use types::*;
pub use update::*;
pub use uv_cache::*;
