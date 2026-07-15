//! 应用路径的单一来源。
//!
//! 此前 `~/Polaris`、`~/Polaris/data` 等拼接散落在 20+ 个文件里,各自
//! `UserDirs::new()` + `.join("Polaris")`。这里收口成一组命名函数;新代码一律走
//! 这里,旧调用点随模块重构逐步迁入。

use std::path::PathBuf;

/// 用户主目录;取不到时退化为当前目录(与既有各模块的兜底一致)。
pub fn home_dir() -> PathBuf {
    directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 应用根目录 `~/Polaris`:用户可见的一切(项目/技能/数据)都在它下面。
pub fn polaris_root() -> PathBuf {
    home_dir().join("Polaris")
}

/// 应用数据目录 `~/Polaris/data`:各模块的 JSON 配置/状态落盘处。
pub fn data_dir() -> PathBuf {
    polaris_root().join("data")
}

/// data 目录下的单个文件,如 `data_file("voice.json")`。
pub fn data_file(name: &str) -> PathBuf {
    data_dir().join(name)
}

/// 跨平台「子树包含」判断 —— 专治 Windows 上两类**误判越界**:
/// 1. `std::fs::canonicalize` 在 Windows 会加 `\\?\`(及 `\\?\UNC\`)扩展长度前缀。
///    若比较两端一端有前缀、一端没有(例如某端 canonicalize 失败回退原值),裸
///    `Path::starts_with` 必为假,合法路径被当成越界。
/// 2. Windows 文件系统大小写不敏感,但 `Path::starts_with` 大小写敏感;根目录存储时
///    的大小写与 canonicalize 返回的真实大小写不一致即误判。
///
/// 故先剥扩展长度前缀、再按平台规整大小写,最后用**组件级** `starts_with` 比较
/// (组件级可避免 `C:\foobar` 命中 `C:\foo` 这种伪前缀)。
/// (原 kb/access.rs;chat 产物护栏与 kb 访问护栏共用 → 下沉横切基建。)
pub fn path_contains(base: &std::path::Path, child: &std::path::Path) -> bool {
    fn norm(p: &std::path::Path) -> PathBuf {
        let s = p.to_string_lossy().to_string();
        let s = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{rest}")
        } else if let Some(rest) = s.strip_prefix(r"\\?\") {
            rest.to_string()
        } else {
            s
        };
        if cfg!(windows) {
            PathBuf::from(s.to_lowercase())
        } else {
            PathBuf::from(s)
        }
    }
    norm(child).starts_with(norm(base))
}

/// 产物目录 `~/Polaris/data/artifacts`。
pub fn artifacts_dir() -> PathBuf {
    data_dir().join("artifacts")
}

/// 项目目录 `~/Polaris/projects`。
pub fn projects_dir() -> PathBuf {
    polaris_root().join("projects")
}

/// 技能目录 `~/Polaris/skills`。
pub fn skills_dir() -> PathBuf {
    polaris_root().join("skills")
}

/// 本地模型目录 `~/Polaris/models`。
pub fn models_dir() -> PathBuf {
    polaris_root().join("models")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_anchored_under_root() {
        let root = polaris_root();
        assert!(root.ends_with("Polaris"));
        assert!(data_dir().starts_with(&root));
        assert!(artifacts_dir().starts_with(data_dir()));
        assert_eq!(data_file("voice.json"), data_dir().join("voice.json"));
        for d in [projects_dir(), skills_dir(), models_dir()] {
            assert!(d.starts_with(&root));
        }
    }
}
