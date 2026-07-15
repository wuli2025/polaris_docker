use super::*;

// ═══════════════════════════════════════════════════════════════
// 用户 Skills（磁盘持久化）
// ═══════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize)]
pub struct UserSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    /// 来源：用户自建为 "user"；从市场安装时保留原始 source（official / third-party）
    pub source: String,
    pub author: String,
    pub created_at: i64,
    #[serde(skip)]
    pub system_prompt: String,
}

/// 用户 skills 根目录: ~/Polaris/skills/
pub(crate) fn skills_dir() -> Option<PathBuf> {
    directories::UserDirs::new().map(|u| u.home_dir().join("Polaris").join("skills"))
}

pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 扫描用户 skills 目录，返回所有用户 skill
pub(crate) fn scan_user_skills() -> Vec<UserSkill> {
    let Some(root) = skills_dir() else {
        return vec![];
    };
    let Ok(entries) = fs::read_dir(&root) else {
        return vec![];
    };

    let mut skills = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill_file = path.join("skill.md");
        if !skill_file.exists() {
            continue;
        }
        if let Ok(skill) = parse_skill_file(&skill_file) {
            skills.push(skill);
        }
    }
    skills.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    skills
}

/// 解析 skill.md 文件: YAML frontmatter + body
pub(crate) fn parse_skill_file(path: &Path) -> Result<UserSkill, String> {
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let lines: Vec<&str> = content.lines().collect();

    // 找 frontmatter 边界 ---
    if lines.len() < 3 || lines[0].trim() != "---" {
        return Err("missing frontmatter".into());
    }
    let mut end_idx = 0;
    for (i, line) in lines.iter().enumerate().skip(1) {
        if line.trim() == "---" {
            end_idx = i;
            break;
        }
    }
    if end_idx == 0 {
        return Err("unclosed frontmatter".into());
    }

    // 解析 frontmatter key: value
    let mut id = String::new();
    let mut name = String::new();
    let mut description = String::new();
    let mut source = "user".to_string();
    let mut author = "user".to_string();
    let mut created_at = 0i64;

    for line in &lines[1..end_idx] {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim();
            let v = v.trim().trim_matches('"').trim_matches('\'');
            match k {
                "id" => id = v.to_string(),
                "name" => name = v.to_string(),
                "description" => description = v.to_string(),
                "source" => source = v.to_string(),
                "author" => author = v.to_string(),
                "created_at" => created_at = v.parse().unwrap_or(0),
                _ => {}
            }
        }
    }

    let system_prompt = lines[end_idx + 1..].join("\n").trim().to_string();

    if id.is_empty() {
        // fallback: 用目录名做 id
        id = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
    }
    if name.is_empty() {
        name = id.clone();
    }

    Ok(UserSkill {
        id,
        name,
        description,
        source,
        author,
        created_at,
        system_prompt,
    })
}

/// 把一份 skill.md 写到用户目录（创建 / 安装共用）
pub(crate) fn write_skill_file(
    id: &str,
    name: &str,
    description: &str,
    source: &str,
    author: &str,
    system_prompt: &str,
) -> Result<(), String> {
    let Some(root) = skills_dir() else {
        return Err("无法获取用户目录".into());
    };
    let dir = root.join(id);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let content = format!(
        "---\nid: {}\nname: {}\ndescription: {}\nsource: {}\nauthor: {}\ncreated_at: {}\n---\n\n{}\n",
        id,
        name,
        description,
        source,
        author,
        now_secs(),
        system_prompt
    );

    fs::write(dir.join("skill.md"), content).map_err(|e| e.to_string())?;
    Ok(())
}

/// 删除用户目录里的 skill 副本（= 卸载 / 删除）
pub(crate) fn remove_user_skill(id: &str) -> Result<(), String> {
    let Some(root) = skills_dir() else {
        return Err("无法获取用户目录".into());
    };
    let dir = root.join(id);
    if !dir.exists() {
        return Err("技能不存在".into());
    }
    fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(())
}

// ═══════════════════════════════════════════════════════════════
// 统一接口（catalog + 用户）
// ═══════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize)]
pub struct SkillMeta {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source: String,
    /// 是否已拥有可用（预装 / 已安装 / 用户自建）
    pub installed: bool,
    /// 是否可删除（物理存在于用户目录，可卸载 / 删除）
    pub removable: bool,
    /// 市场分组（按人群/用途）；用户自建 = 「我的创建」
    pub category: String,
}

/// 查找 skill（优先用户目录副本，再 catalog），返回元信息 + system_prompt
pub fn find(id: &str) -> Option<(SkillMeta, String)> {
    // 先查用户目录（允许覆盖同名 catalog skill）
    for user in scan_user_skills() {
        if user.id == id {
            return Some((
                SkillMeta {
                    category: skill_category(&user.id).into(),
                    id: user.id,
                    name: user.name,
                    description: user.description,
                    source: user.source,
                    installed: true,
                    removable: true,
                },
                user.system_prompt,
            ));
        }
    }
    // 再查 catalog
    find_catalog(id).map(|c| {
        (
            SkillMeta {
                id: c.id.into(),
                name: c.name.into(),
                description: c.description.into(),
                source: c.source.into(),
                installed: c.preinstalled,
                removable: false,
                category: skill_category(c.id).into(),
            },
            c.system_prompt.to_string(),
        )
    })
}

/// 递归复制目录树。
pub(crate) fn copy_dir_all(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            fs::copy(&from, &to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// 去掉 markdown 开头的 YAML frontmatter（`---\n…\n---\n`），返回正文。
pub(crate) fn strip_frontmatter(s: &str) -> String {
    let s = s.trim_start_matches('\u{feff}');
    if let Some(rest) = s.strip_prefix("---") {
        if let Some(idx) = rest.find("\n---") {
            // 跳过 "\n---" 后到该行结尾
            let after = &rest[idx + 4..];
            let after = after.strip_prefix('\n').unwrap_or(after);
            return after.trim_start_matches('\r').trim_start().to_string();
        }
    }
    s.to_string()
}

// ── 外部工具封装（用系统自带 git / curl / tar，免新增 Rust 依赖） ──

pub(crate) fn make_temp_dir() -> Result<PathBuf, String> {
    let base = std::env::temp_dir().join(format!("polaris-skill-import-{}", now_secs()));
    fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    Ok(base)
}

pub(crate) fn run_cmd(cmd: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("无法执行 {}：{}（请确认系统已安装 {}）", cmd, e, cmd))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("{} 执行失败：{}", cmd, err.trim()));
    }
    Ok(())
}

pub(crate) fn download(url: &str, dest: &Path) -> Result<(), String> {
    let dest_s = dest.to_string_lossy();
    run_cmd("curl", &["-L", "--fail", "-s", "-o", dest_s.as_ref(), url])
}

pub(crate) fn unzip(zip: &Path, dest: &Path) -> Result<(), String> {
    // Win11 / macOS / Linux 自带 bsdtar 可解 .zip
    let zip_s = zip.to_string_lossy();
    let dest_s = dest.to_string_lossy();
    run_cmd("tar", &["-xf", zip_s.as_ref(), "-C", dest_s.as_ref()])
}
