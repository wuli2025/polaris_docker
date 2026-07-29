//! 随安装包分发的运行时 (uv / Python / Git Bash) 的定位与注入。
//!
//! 出发点: 以前环境医生把 uv 与 PowerShell 7 当成「用户必须自己装」的东西 —— 启动关甚至会
//! **自动替用户装 PowerShell 7**(弹 UAC、走 winget/MSI、国内还常拉不动)。这对「开箱即用」
//! 是反的。现在把三样东西直接打进安装包(见 `scripts/fetch-runtimes.mjs` + `bundle.resources`):
//!
//! | 组件      | 位置                    | 解决什么                                        |
//! |-----------|-------------------------|-------------------------------------------------|
//! | uv        | `runtime/uv/uv.exe`     | Claude 写的 Python 脚本 `uv run` 即跑            |
//! | Python    | `runtime/python/`       | 便携解释器, `uv run` 免联网下解释器              |
//! | Git Bash  | `runtime/git/` (仅 Win) | Claude Code 在 Windows 跑 Bash 工具必须有 bash   |
//!
//! 全部**免安装、免管理员、不写注册表、不动用户 PATH** —— 只在本进程与 claude 子进程的
//! 环境里生效, 卸载即干净。用户自己装过的同名工具**优先**(PATH 命中在前), 内置的只作兜底。
//!
//! ★ 两个实测坑 (2026-07-23, 别想当然):
//! ① MinGit **没有 `bash.exe`**, 只有 `usr/bin/sh.exe` —— 而它就是 bash 5.3 本体。抓取脚本
//!    已复制出 `bash.exe`, 这里按 bash.exe 找。
//! ② **非登录 bash (`bash -c`) 不读 `/etc/profile`** → PATH 里没有 `/usr/bin`,
//!    `ls/cat/grep/sed/awk` 全部找不到; 更坏的是 `find`/`sort` 会**静默命中 System32 的
//!    同名 exe**(语义完全不同, 结果是错的却不报错)。故 `child_path_prefix` 把
//!    `usr/bin`、`mingw64/bin` **前插**进 claude 子进程的 PATH, 让 unix 工具压过 System32。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 随包运行时的根目录 (`runtime/`)。找不到 (未打包 / 开发期没抓) 返回 None —— 一切照旧回落到
/// 「用户自己装的那份」, 不影响老用户。
pub fn runtime_root() -> Option<&'static Path> {
    static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    ROOT.get_or_init(resolve_root).as_deref()
}

fn resolve_root() -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    // ① 显式覆盖 (测试 / 特殊部署)
    if let Some(p) = std::env::var_os("POLARIS_RUNTIME_DIR") {
        cands.push(PathBuf::from(p));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // ② Windows/Linux: 资源与 exe 同级
            cands.push(dir.join("runtime"));
            // ③ macOS .app: Contents/MacOS/polaris → Contents/Resources/runtime
            cands.push(dir.join("../Resources/runtime"));
        }
    }
    // ④ 开发期: <仓库>/src-tauri/runtime (本 crate 在 src-tauri/crates/polaris-kernel)
    cands.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("runtime"),
    );
    cands.into_iter().find(|p| is_runtime_root(p))
}

/// 只认「确实装着我们那三样之一」的目录, 免得把某个同名空目录当成运行时根。
fn is_runtime_root(p: &Path) -> bool {
    p.join("uv").is_dir() || p.join("python").is_dir() || p.join("git").is_dir()
}

fn exe_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

/// 内置 uv 可执行文件。
pub fn uv_exe() -> Option<PathBuf> {
    let p = runtime_root()?.join("uv").join(exe_name("uv"));
    p.exists().then_some(p)
}

/// 内置 uv 所在目录 (上 PATH 用)。
pub fn uv_dir() -> Option<PathBuf> {
    uv_exe().and_then(|p| p.parent().map(Path::to_path_buf))
}

/// 内置 Python 解释器。Windows 在根, 类 Unix 在 `bin/`。
pub fn python_exe() -> Option<PathBuf> {
    let root = runtime_root()?.join("python");
    let p = if cfg!(windows) {
        root.join("python.exe")
    } else {
        root.join("bin").join("python3")
    };
    p.exists().then_some(p)
}

/// 内置 Python 所在目录 (上 PATH 用 —— uv 靠 PATH 发现解释器, 免得再去联网下一份)。
pub fn python_dir() -> Option<PathBuf> {
    python_exe().and_then(|p| p.parent().map(Path::to_path_buf))
}

/// 内置 Git Bash (仅 Windows 有意义; 类 Unix 用系统自带 shell, 恒 None)。
///
/// ★ 必须是 `usr/bin/bash.exe`, **不能**用 `bin/bash.exe`(2026-07-23 实测):
/// msys 的 bash 依赖**同目录**的 `msys-2.0.dll`, 而 `bin/` 下没有它 —— 那份裸跑即
/// `0xC0000135 DLL_NOT_FOUND`, 只在我们恰好把 `usr/bin` 注入进 PATH 时才活。
/// Claude Code 自己 spawn 这个 bash 时未必带着我们的 PATH, 故只认能独立跑的那份。
pub fn git_bash() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let p = runtime_root()?
        .join("git")
        .join("usr")
        .join("bin")
        .join("bash.exe");
    p.exists().then_some(p)
}

/// 内置 Git Bash 的 unix 工具目录 (`usr/bin`、`mingw64/bin`)。
///
/// 见文件头坑②: 非登录 bash 不读 `/etc/profile`, 不把这两个目录前插进子进程 PATH 的话,
/// `grep/sed/ls` 直接找不到, 而 `find`/`sort` 会命中 System32 的同名 exe —— **静默给错结果**。
pub fn git_unix_dirs() -> Vec<PathBuf> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let Some(root) = runtime_root() else {
        return Vec::new();
    };
    let git = root.join("git");
    [git.join("usr").join("bin"), git.join("mingw64").join("bin")]
        .into_iter()
        .filter(|p| p.is_dir())
        .collect()
}

/// 某个路径是不是我们内置的那份 (面板上标「随应用内置」用)。
pub fn is_bundled(p: &Path) -> bool {
    runtime_root().is_some_and(|root| p.starts_with(root))
}

/// 要前插进 **claude 子进程** PATH 的目录, 按优先级从高到低。
///
/// 只给子进程用, **不改本进程 PATH** —— msys 的 `find.exe`/`sort.exe` 与 Windows System32
/// 同名但语义不同, 让它们污染整个应用进程会有远处的怪问题; 而 claude 的工具调用全都经 bash,
/// 正是需要 unix 语义的地方。
///
/// git 的 unix 目录**仅在用到内置 bash 时**才注入: 用户自己装了 Git for Windows 的机器
/// 保持今天的行为一字不变(它的 bash 自会读 `/etc/profile`), 不给既有环境添变数。
pub fn child_path_prefix(use_bundled_bash: bool) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if use_bundled_bash {
        dirs.extend(git_unix_dirs());
    }
    dirs.extend(uv_dir());
    dirs.extend(python_dir());
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 空目录不能被当成运行时根 —— 否则一路解析出不存在的 uv/bash 路径。
    #[test]
    fn empty_dir_is_not_a_runtime_root() {
        let tmp = std::env::temp_dir().join("polaris-bundled-empty-test");
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(!is_runtime_root(&tmp));
        let _ = std::fs::remove_dir(&tmp);
    }

    /// 三样里有任意一样就算数 (mac 不打 git, 也得认得出来)。
    #[test]
    fn dir_with_any_component_is_a_runtime_root() {
        let tmp = std::env::temp_dir().join("polaris-bundled-uvonly-test");
        std::fs::create_dir_all(tmp.join("uv")).unwrap();
        assert!(is_runtime_root(&tmp));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 没启用内置 bash 时不能把 msys 目录塞进子进程 PATH (会盖掉 System32 的 find/sort)。
    #[test]
    fn git_dirs_only_injected_when_bundled_bash_is_used() {
        let without = child_path_prefix(false);
        for d in git_unix_dirs() {
            assert!(
                !without.contains(&d),
                "未使用内置 bash 时不应注入 msys 目录: {}",
                d.display()
            );
        }
    }

    /// 内置运行时在场时, 解析出来的路径必须真实存在 (防「拼得出路径但文件不在」)。
    #[test]
    fn resolved_paths_exist_when_runtime_is_present() {
        if runtime_root().is_none() {
            return; // 没抓运行时的环境 (如 CI 的纯单测) 跳过
        }
        for p in [uv_exe(), python_exe(), git_bash()].into_iter().flatten() {
            assert!(p.exists(), "解析出的路径不存在: {}", p.display());
            assert!(is_bundled(&p), "应被认作内置: {}", p.display());
        }
    }
}
