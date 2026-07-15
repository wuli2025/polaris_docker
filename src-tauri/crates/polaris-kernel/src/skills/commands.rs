use super::*;

// ═══════════════════════════════════════════════════════════════
// Tauri Commands
// ═══════════════════════════════════════════════════════════════

/// 技能列表。桌面端 async + spawn_blocking:scan_user_skills 要 walk 用户技能目录并
/// 逐文件读 SKILL.md,首帧就会被调到,同步跑在主线程会挤占首屏。server flavor 无 UI
/// 主线程、dispatch 本就在 spawn_blocking 中,保持同步直调。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn list_skills() -> Vec<SkillMeta> {
    tauri::async_runtime::spawn_blocking(list_skills_sync)
        .await
        .unwrap_or_default()
}
#[cfg(not(feature = "desktop"))]
pub fn list_skills() -> Vec<SkillMeta> {
    list_skills_sync()
}

fn list_skills_sync() -> Vec<SkillMeta> {
    let user = scan_user_skills();
    let user_ids: HashSet<String> = user.iter().map(|s| s.id.clone()).collect();

    let cat = catalog();
    let cat_ids: HashSet<&str> = cat.iter().map(|c| c.id).collect();

    let mut list = Vec::new();

    // 1. 目录技能（市场 + 预装）
    for c in &cat {
        let in_user_dir = user_ids.contains(c.id);
        list.push(SkillMeta {
            id: c.id.into(),
            name: c.name.into(),
            description: c.description.into(),
            source: c.source.into(),
            installed: c.preinstalled || in_user_dir,
            removable: in_user_dir,
            category: skill_category(c.id).into(),
        });
    }

    // 2. 纯用户自建技能（不在目录里的）
    for u in &user {
        if !cat_ids.contains(u.id.as_str()) {
            list.push(SkillMeta {
                id: u.id.clone(),
                name: u.name.clone(),
                description: u.description.clone(),
                source: u.source.clone(),
                installed: true,
                removable: true,
                category: "我的创建".into(),
            });
        }
    }

    list
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn get_skill(id: String) -> Result<SkillMeta, String> {
    find(&id)
        .map(|(meta, _)| meta)
        .ok_or_else(|| format!("Skill '{}' 不存在", id))
}

#[derive(Debug, Deserialize)]
pub struct CreateSkillArgs {
    pub id: String,
    pub name: String,
    pub description: String,
    pub system_prompt: String,
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn create_skill(args: CreateSkillArgs) -> Result<(), String> {
    // 校验 id: 只允许小写字母、数字、-、_
    if !args
        .id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err("Skill ID 只能包含小写字母、数字、-、_".into());
    }
    write_skill_file(
        &args.id,
        &args.name,
        &args.description,
        "user",
        "user",
        &args.system_prompt,
    )
}

/// 从市场安装一个目录技能：复制模板到用户目录，保留原始 source。
/// 安装即拥有，立即出现在技能中心（前端负责自动激活）。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn install_skill(id: String) -> Result<(), String> {
    // 多文件打包技能走专用安装路径（clone 整包 + 叠加 Polaris 助手 + 拼 skill.md）。
    if id == WVP_ID {
        return install_web_video_presentation();
    }
    let c = find_catalog(&id).ok_or_else(|| format!("市场中没有技能 '{}'", id))?;
    write_skill_file(
        c.id,
        c.name,
        c.description,
        c.source,
        "registry",
        c.system_prompt,
    )
}

/// 安装「网页演示视频」打包技能：clone 原仓库整目录 → 叠加 Polaris 助手脚本
/// → 拼 skill.md（Polaris 增强说明 + 原 SKILL 正文）→ 跑一次依赖自检。
fn install_web_video_presentation() -> Result<(), String> {
    let Some(root) = skills_dir() else {
        return Err("无法获取用户目录".into());
    };
    let dest = root.join(WVP_ID);

    // 1. clone 到临时目录
    let tmp = make_temp_dir()?;
    let repo = tmp.join("repo");
    let repo_s = repo.to_string_lossy();
    let clone_res = run_cmd("git", &["clone", "--depth", "1", WVP_REPO, repo_s.as_ref()]);
    if let Err(e) = clone_res {
        let _ = fs::remove_dir_all(&tmp);
        return Err(format!("下载技能包失败（需要 git + 联网）：{}", e));
    }
    let src = repo.join("skills").join("web-video-presentation");
    if !src.exists() {
        let _ = fs::remove_dir_all(&tmp);
        return Err("仓库中未找到 skills/web-video-presentation".into());
    }

    // 2. 全新覆盖目标目录并复制整包
    if dest.exists() {
        fs::remove_dir_all(&dest).map_err(|e| e.to_string())?;
    }
    let copy_res = copy_dir_all(&src, &dest);
    let _ = fs::remove_dir_all(&tmp);
    copy_res?;

    // 3. 叠加 Polaris 助手脚本（编译期内嵌）
    let pol = dest.join("polaris");
    fs::create_dir_all(&pol).map_err(|e| e.to_string())?;
    fs::write(pol.join("minimax-tts.mjs"), WVP_MINIMAX_TTS).map_err(|e| e.to_string())?;
    fs::write(pol.join("scaffold.mjs"), WVP_SCAFFOLD).map_err(|e| e.to_string())?;
    fs::write(pol.join("bootstrap.mjs"), WVP_BOOTSTRAP).map_err(|e| e.to_string())?;

    // 4. 拼 skill.md = frontmatter + Polaris 增强说明(替换 PKG 路径) + 原 SKILL 正文
    let orig = fs::read_to_string(dest.join("SKILL.md")).map_err(|e| e.to_string())?;
    let body = strip_frontmatter(&orig);
    let pkg_str = dest.to_string_lossy().to_string();
    let addendum = WVP_ADDENDUM.replace("__PKG_DIR__", &pkg_str);
    let fm = format!(
        "---\nid: {}\nname: 网页演示视频制作（ConardLi·Polaris集成）\ndescription: 把文稿做成16:9可点击翻页的网页演示再录屏成片，配音自动调用 Polaris 内置 MiniMax。完整脚手架+23主题+音频流水线，Windows 走 Node 版一键跑通。\nsource: third-party\nauthor: ConardLi/Polaris\ncreated_at: {}\n---\n\n",
        WVP_ID,
        now_secs()
    );
    let skill = format!("{}{}\n\n{}", fm, addendum, body);
    fs::write(dest.join("skill.md"), skill).map_err(|e| e.to_string())?;

    // 5. 依赖自检（best-effort，不阻断安装）
    let _ = Command::new("node").arg(pol.join("bootstrap.mjs")).output();

    Ok(())
}

// ═══════════════════════════════════════════════════════════════
// 外部导入 / 下载（不限来源，鼓励从外面拿）
//   本地：.md 文件 / .zip 压缩包 / 技能目录
//   远程：http(s) 的 .md 或 .zip / git 仓库 URL（可装整套技能合集）
// ═══════════════════════════════════════════════════════════════

/// 把任意来源的 skill 导入用户目录，返回导入成功的 skill id 列表（供前端自动激活）。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn import_skill(source: String) -> Result<Vec<String>, String> {
    let src = source.trim();
    if src.is_empty() {
        return Err("来源为空".into());
    }

    let is_remote = src.starts_with("http://")
        || src.starts_with("https://")
        || src.starts_with("git@")
        || src.ends_with(".git");

    if is_remote {
        import_from_remote(src)
    } else {
        import_from_local(Path::new(src))
    }
}

fn import_from_remote(src: &str) -> Result<Vec<String>, String> {
    let tmp = make_temp_dir()?;
    let lower = src.to_lowercase();

    let result = if lower.ends_with(".md") {
        let md = tmp.join("skill.md");
        download(src, &md)?;
        import_one_md(&md, "imported").map(|id| vec![id])
    } else if lower.ends_with(".zip") {
        let zip = tmp.join("download.zip");
        download(src, &zip)?;
        let out = tmp.join("unzipped");
        fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        unzip(&zip, &out)?;
        import_from_dir(&out)
    } else {
        // .git 结尾、git@、或 github/gitlab 等仓库 URL → clone 后扫描全部技能
        let dest = tmp.join("repo");
        let dest_s = dest.to_string_lossy();
        // `--` 终止选项解析: 否则 src 以 `-` 开头(如 --upload-pack=…)会被 git 当 flag → 参数注入。
        run_cmd(
            "git",
            &["clone", "--depth", "1", "--", src, dest_s.as_ref()],
        )?;
        import_from_dir(&dest)
    };

    let _ = fs::remove_dir_all(&tmp);
    result
}

fn import_from_local(path: &Path) -> Result<Vec<String>, String> {
    if !path.exists() {
        return Err(format!("路径不存在: {}", path.display()));
    }
    if path.is_dir() {
        return import_from_dir(path);
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "md" => import_one_md(path, "imported").map(|id| vec![id]),
        "zip" => {
            let tmp = make_temp_dir()?;
            let out = tmp.join("unzipped");
            fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            unzip(path, &out)?;
            let r = import_from_dir(&out);
            let _ = fs::remove_dir_all(&tmp);
            r
        }
        other => Err(format!("不支持的文件类型: .{}", other)),
    }
}

/// 递归扫描目录里所有 SKILL.md / skill.md，逐个导入（支持技能合集）
fn import_from_dir(dir: &Path) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    for entry in walkdir::WalkDir::new(dir).into_iter().flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let fname = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if fname.eq_ignore_ascii_case("skill.md") {
            if let Ok(id) = import_one_md(p, "imported") {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
    }
    if ids.is_empty() {
        return Err("未在来源中找到任何 SKILL.md / skill.md".into());
    }
    Ok(ids)
}

/// 导入单个 md：有 frontmatter 按字段解析，无 frontmatter 则整篇即正文。
/// 规范化后写到 ~/Polaris/skills/<id>/skill.md。
fn import_one_md(md: &Path, default_source: &str) -> Result<String, String> {
    let raw = fs::read_to_string(md).map_err(|e| e.to_string())?;

    let (id_raw, name_raw, description, src) = if let Ok(s) = parse_skill_file(md) {
        (s.id, s.name, s.description, s.source)
    } else {
        // 无 frontmatter：用所在目录名（退而求其次文件名）当 id，正文 = 全文
        let base = md
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .filter(|s| !["unzipped", "repo", "skills", ""].contains(s))
            .map(|s| s.to_string())
            .or_else(|| {
                md.file_stem()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "imported-skill".to_string());
        (base.clone(), base, String::new(), "user".to_string())
    };

    // 正文：parse 成功用其 system_prompt，否则用去掉 frontmatter 的全文
    let body = match parse_skill_file(md) {
        Ok(s) => s.system_prompt,
        Err(_) => raw.trim().to_string(),
    };

    let id = {
        let cleaned: String = id_raw
            .to_lowercase()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let cleaned = cleaned.trim_matches('-').to_string();
        if cleaned.is_empty() {
            "imported-skill".to_string()
        } else {
            cleaned
        }
    };
    let name = if name_raw.trim().is_empty() {
        id.clone()
    } else {
        name_raw
    };
    let source = if src == "user" {
        default_source.to_string()
    } else {
        src
    };

    write_skill_file(&id, &name, &description, &source, "imported", &body)?;
    Ok(id)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn delete_skill(id: String) -> Result<(), String> {
    // 安全闸: id 直接拼进 remove_dir_all 的路径, 必须挡掉 `..` / 路径分隔符 / 盘符,
    // 否则前端(或被注入的 webview 脚本)能传 `..\..\Docs` 或绝对路径删任意目录。
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || id.contains(':')
    {
        return Err("非法技能 ID".into());
    }
    let Some(root) = skills_dir() else {
        return Err("无法获取用户目录".into());
    };
    // 物理存在于用户目录 → 直接移除（用户自建 / 已安装市场技能都走这里）
    if root.join(&id).exists() {
        return remove_user_skill(&id);
    }
    // 不在用户目录：可能是预装技能（不可删）或根本不存在
    if find_catalog(&id).map(|c| c.preinstalled).unwrap_or(false) {
        return Err("预装技能不可删除".into());
    }
    Err("技能不存在".into())
}
