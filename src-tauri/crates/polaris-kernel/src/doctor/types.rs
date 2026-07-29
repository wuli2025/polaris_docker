//! 环境医生 —— 视图模型 / 流式事件 / 缓存·更新信息 (纯移动自 doctor.rs)。

use serde::Serialize;

// ───────────────────────── 视图模型 ─────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolStatus {
    /// 稳定标识: claude | pwsh | node | npm | uv | python
    pub key: String,
    /// 展示名
    pub name: String,
    /// 是否在机器上找到 (PATH 命中或已知安装位置存在)
    pub found: bool,
    /// 版本号 (探测到才有)
    pub version: Option<String>,
    /// 解析到的可执行文件路径 (正斜杠)
    pub path: Option<String>,
    /// 是否能通过 PATH 直接发现 (即终端里敲命令能用)
    pub on_path: bool,
    /// 是否是「必须」(claude 必须; 其余推荐)
    pub required: bool,
    /// 一句话状态说明 / 安装建议
    pub hint: String,
    /// 命中的是不是**随安装包内置**的那份 (见 doctor::bundled)。
    /// true ⇒ 面板显示「随应用内置」, 不再催用户安装 —— 它本就免安装、免管理员。
    pub bundled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvReport {
    /// "windows" | "macos" | "linux" ...
    pub os: String,
    pub claude: ToolStatus,
    pub pwsh: ToolStatus,
    pub node: ToolStatus,
    pub npm: ToolStatus,
    /// uv —— Python 脚本运行时的统一托管者 (脚本执行公约依赖它; 一个二进制管解释器+依赖)。
    pub uv: ToolStatus,
    /// 系统 Python —— 仅作信息展示。found=false 时多半是「只有 Store 占位符」(detect 已滤掉),
    /// 脚本一律由 uv 按需托管, 不依赖这一项, 故 not required。
    pub python: ToolStatus,
    /// claude.exe 应在 / 已在的目录 (用于「修复 PATH」)
    pub claude_dir: Option<String>,
    /// 该目录是否已在「用户 PATH」里 (Windows)。false ⇒ 需要修复
    pub claude_dir_on_user_path: bool,
    /// 是否有 claude 可用的 shell —— 真身 PowerShell 7 (非 Store 别名) 或 Git Bash
    /// (含**随安装包内置**的那份)。false ⇒ 即便装了 claude, 对话里也会报「找不到 PowerShell / bash」。
    pub shell_ready: bool,
    /// 当前这个 shell 是不是随应用内置的 Git Bash。true ⇒ 面板据此说明「无需另装 PowerShell 7」。
    pub shell_bundled: bool,
    /// 整体是否就绪 (claude 已装 **且** 有可用 shell 才算真能跑起来)
    pub ready: bool,
    /// **运行环境由镜像托管** (server/容器 flavor) —— claude 等组件随镜像预烤, 升级靠
    /// `docker pull` 换镜像, 而非在面板里点安装/更新。
    ///
    /// 为什么要这个字段: 容器里 `env_claude_update_check` 是放行的(能查出有新版), 但
    /// `env_install_claude` / `env_update_claude` 在 apihub 层被挡 → 面板会渲染出一个
    /// 「更新到 x.y.z」按钮而**点了必然报错**。而且镜像把 claude 版本锁死后, 随上游发版
    /// 这个必失败的按钮只会越来越常出现。前端据此改渲染成一行静态提示。
    ///
    /// 判定: 非 desktop feature 即 server flavor(Docker 镜像用 `-p polaris-cli` 构建,
    /// 不开 desktop) —— 后端权威, 免得前端靠 `isTauri` 之类的迹象猜。
    pub image_managed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathFixResult {
    pub ok: bool,
    /// 实际加入 PATH 的目录
    pub dir: Option<String>,
    /// "added" | "present" | "process_only" | "skipped"
    pub status: String,
    pub message: String,
}

// ───────────────────────── 流式事件 ─────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvStreamEvent {
    pub req_id: String,
    /// "log" | "error" | "done"
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<String>,
    /// done 时: 是否成功
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    /// done 时: 收尾说明 (含 PATH 配置结果)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UvCacheInfo {
    /// uv 是否可用 (装了才谈缓存)
    pub available: bool,
    /// 缓存目录 (`uv cache dir`)
    pub dir: Option<String>,
    /// 缓存占用字节数
    pub bytes: u64,
    /// 人类可读大小 (如 "1.3 GB")
    pub human: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeUpdateInfo {
    /// 是否已安装 (装了才谈更新)
    pub installed: bool,
    /// 当前版本 (纯 x.y.z, 解析不出则原样)
    pub current: Option<String>,
    /// 镜像上的最新版本
    pub latest: Option<String>,
    /// 是否有可用更新 (latest > current)
    pub update_available: bool,
    /// 是否成功查到了 latest (网络/镜像可用)
    pub checked: bool,
    /// 一句话说明
    pub message: String,
}
