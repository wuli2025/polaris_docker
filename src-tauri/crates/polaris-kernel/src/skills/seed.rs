use super::*;

/// 启动时确保「课件视频工坊」技能在 ~/Polaris/skills 落盘（多文件，含可执行脚本）。
///
/// 这是支撑「生成课件类视频」UI 的基础设施技能，所以是「确保存在」而非「尊重删除」：
/// - 目录缺失（含被用户删除）→ 重新落盘
/// - 已落盘但版本旧（`.polaris_version` < `PVS_VERSION`）→ 覆盖更新（让脚本修复随更新下发）
/// - 已是最新 → 跳过
///
/// best-effort：任何失败都只是让该 UI 功能暂不可用，不应阻断 App 启动。
pub fn seed_video_studio_skill() {
    let Some(root) = skills_dir() else {
        return;
    };
    let dest = root.join(PVS_ID);
    let ver_file = dest.join(".polaris_version");
    let stored = fs::read_to_string(&ver_file).unwrap_or_default();
    let present = dest.join("skill.md").exists();
    if present && stored.trim() == PVS_VERSION {
        return; // 已是最新，无需重写
    }
    if write_video_studio_files(&dest).is_ok() {
        let _ = fs::write(&ver_file, PVS_VERSION);
    }

    // 顺带刷新已安装的 web-video-presentation 里的 Polaris 助手 minimax-tts.mjs，
    // 让「多语言配音」(language_boost) 的引擎修复随同一次版本更新下发——
    // 不动 ConardLi 原包文件，只覆盖我们自己叠加的助手脚本，且仅在它已存在时。
    let wvp_tts = root.join(WVP_ID).join("polaris").join("minimax-tts.mjs");
    if wvp_tts.exists() {
        let _ = fs::write(&wvp_tts, WVP_MINIMAX_TTS);
    }
}

/// 启动时确保「演示工坊」技能在 ~/Polaris/skills 落盘（多文件，含资源 + 导出脚本）。
///
/// 与 `seed_video_studio_skill` 同策略：目录缺失 / 版本旧（`.polaris_version` < `DECK_VERSION`）
/// 就（重）写；已是最新则跳过。best-effort，失败只让该 UI 暂不可用，不阻断启动。
pub fn seed_deck_studio_skill() {
    let Some(root) = skills_dir() else {
        return;
    };
    let dest = root.join(DECK_ID);
    let ver_file = dest.join(".polaris_version");
    let stored = fs::read_to_string(&ver_file).unwrap_or_default();
    let present = dest.join("skill.md").exists();
    if present && stored.trim() == DECK_VERSION {
        return;
    }
    if write_deck_studio_files(&dest).is_ok() {
        let _ = fs::write(&ver_file, DECK_VERSION);
    }
}

/// 把内嵌的「演示工坊」全部文件写到目标目录（建好子目录树）。
/// 技能正文写成小写 `skill.md`，与扫描约定一致。
fn write_deck_studio_files(dest: &Path) -> Result<(), String> {
    let assets = dest.join("assets");
    let templates = dest.join("templates");
    let scripts = dest.join("scripts");
    fs::create_dir_all(&assets).map_err(|e| e.to_string())?;
    fs::create_dir_all(&templates).map_err(|e| e.to_string())?;
    fs::create_dir_all(&scripts).map_err(|e| e.to_string())?;
    fs::write(dest.join("skill.md"), DECK_SKILL_MD).map_err(|e| e.to_string())?;
    // SKILL.md 第一步就让模型读 design.md(设计规范) —— 此前漏落盘,模型只能对着空路径。
    fs::write(dest.join("design.md"), DECK_DESIGN_MD).map_err(|e| e.to_string())?;
    fs::write(dest.join("LICENSE"), DECK_LICENSE).map_err(|e| e.to_string())?;
    fs::write(assets.join("base.css"), DECK_BASE_CSS).map_err(|e| e.to_string())?;
    fs::write(assets.join("themes.css"), DECK_THEMES_CSS).map_err(|e| e.to_string())?;
    fs::write(assets.join("runtime.js"), DECK_RUNTIME_JS).map_err(|e| e.to_string())?;
    fs::write(assets.join("motion.css"), DECK_MOTION_CSS).map_err(|e| e.to_string())?;
    fs::write(assets.join("motion.js"), DECK_MOTION_JS).map_err(|e| e.to_string())?;
    fs::write(templates.join("deck.html"), DECK_TEMPLATE).map_err(|e| e.to_string())?;
    fs::write(scripts.join("install-deps.mjs"), DECK_INSTALL_DEPS).map_err(|e| e.to_string())?;
    fs::write(scripts.join("export-pptx.mjs"), DECK_EXPORT_PPTX).map_err(|e| e.to_string())?;
    fs::write(scripts.join("find-browser.mjs"), DECK_FIND_BROWSER).map_err(|e| e.to_string())?;
    write_designers(dest)?;
    Ok(())
}

/// 启动时确保「网站生成」技能在 ~/Polaris/skills 落盘。策略同上（版本号比对覆盖）。
pub fn seed_web_studio_skill() {
    let Some(root) = skills_dir() else {
        return;
    };
    let dest = root.join(WEB_ID);
    let ver_file = dest.join(".polaris_version");
    let stored = fs::read_to_string(&ver_file).unwrap_or_default();
    let present = dest.join("skill.md").exists();
    if present && stored.trim() == WEB_VERSION {
        return;
    }
    if write_web_studio_files(&dest).is_ok() {
        let _ = fs::write(&ver_file, WEB_VERSION);
    }
}

/// 把内嵌的「网站生成」全部文件写到目标目录。themes.css 复用 deck-studio 的同一份内容。
fn write_web_studio_files(dest: &Path) -> Result<(), String> {
    let assets = dest.join("assets");
    let templates = dest.join("templates");
    fs::create_dir_all(&assets).map_err(|e| e.to_string())?;
    fs::create_dir_all(&templates).map_err(|e| e.to_string())?;
    fs::write(dest.join("skill.md"), WEB_SKILL_MD).map_err(|e| e.to_string())?;
    fs::write(dest.join("LICENSE"), WEB_LICENSE).map_err(|e| e.to_string())?;
    fs::write(assets.join("site.css"), WEB_SITE_CSS).map_err(|e| e.to_string())?;
    fs::write(assets.join("themes.css"), DECK_THEMES_CSS).map_err(|e| e.to_string())?;
    fs::write(assets.join("runtime.js"), WEB_RUNTIME_JS).map_err(|e| e.to_string())?;
    fs::write(assets.join("motion.css"), WEB_MOTION_CSS).map_err(|e| e.to_string())?;
    fs::write(assets.join("motion.js"), WEB_MOTION_JS).map_err(|e| e.to_string())?;
    fs::write(templates.join("site.html"), WEB_TEMPLATE).map_err(|e| e.to_string())?;
    write_designers(dest)?; // 网站生成复用同一份设计师人格包
    Ok(())
}

/// 启动时确保「极速下载」技能在 ~/Polaris/skills 落盘（含可执行 Python 脚本）。
/// 策略同上（版本号比对覆盖）。best-effort：失败只让该技能脚本暂不可用，不阻断启动。
pub fn seed_turbo_download_skill() {
    let Some(root) = skills_dir() else {
        return;
    };
    let dest = root.join(TURBO_ID);
    let ver_file = dest.join(".polaris_version");
    let stored = fs::read_to_string(&ver_file).unwrap_or_default();
    let present = dest.join("skill.md").exists();
    if present && stored.trim() == TURBO_VERSION {
        return;
    }
    if write_turbo_download_files(&dest).is_ok() {
        let _ = fs::write(&ver_file, TURBO_VERSION);
    }
}

/// 把内嵌的「极速下载」全部文件写到目标目录（含 scripts/ 与 references/ 子树）。
/// 技能正文写成小写 `skill.md`，与 `scan_user_skills` 约定一致。
fn write_turbo_download_files(dest: &Path) -> Result<(), String> {
    let scripts = dest.join("scripts");
    let references = dest.join("references");
    fs::create_dir_all(&scripts).map_err(|e| e.to_string())?;
    fs::create_dir_all(&references).map_err(|e| e.to_string())?;
    fs::write(dest.join("skill.md"), TURBO_SKILL_MD).map_err(|e| e.to_string())?;
    fs::write(scripts.join("fast_download.py"), TURBO_FAST_DL).map_err(|e| e.to_string())?;
    fs::write(references.join("aria2_flags.md"), TURBO_FLAGS_MD).map_err(|e| e.to_string())?;
    Ok(())
}

/// 启动时确保「项目检测」检查技能在 ~/Polaris/skills 落盘(含 check.ps1/check.sh)。
/// 策略同上(版本号比对覆盖)。best-effort:失败只让协作检查闸回退到"技能缺失=fail",不阻断启动。
pub fn seed_project_check_skill() {
    let Some(root) = skills_dir() else {
        return;
    };
    let dest = root.join(PROJECT_CHECK_ID);
    let ver_file = dest.join(".polaris_version");
    let stored = fs::read_to_string(&ver_file).unwrap_or_default();
    let present = dest.join("skill.md").exists();
    if present && stored.trim() == PROJECT_CHECK_VERSION {
        return;
    }
    if write_project_check_files(&dest).is_ok() {
        let _ = fs::write(&ver_file, PROJECT_CHECK_VERSION);
    }
}

/// 把内嵌的「项目检测」文件写到目标目录(含 scripts/ 子树)。
/// 技能正文写成小写 `skill.md`,与 `scan_user_skills` 约定一致。
fn write_project_check_files(dest: &Path) -> Result<(), String> {
    let scripts = dest.join("scripts");
    fs::create_dir_all(&scripts).map_err(|e| e.to_string())?;
    fs::write(dest.join("skill.md"), PROJECT_CHECK_SKILL_MD).map_err(|e| e.to_string())?;
    fs::write(scripts.join("check.ps1"), PROJECT_CHECK_PS1).map_err(|e| e.to_string())?;
    let sh = scripts.join("check.sh");
    fs::write(&sh, PROJECT_CHECK_SH).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&sh, fs::Permissions::from_mode(0o755));
    }
    Ok(())
}

/// 检查技能的执行入口(collab/checks.rs 按此协议跑脚本)。
#[derive(Debug, Clone, serde::Serialize)]
pub struct CheckSkillEntry {
    pub skill_id: String,
    /// 入口脚本绝对路径(按当前平台选 check_entry_windows / check_entry_unix)。
    pub entry: PathBuf,
    /// PowerShell 入口(true)还是 sh 入口(false)。
    pub windows: bool,
    pub timeout_secs: u64,
}

/// 解析某技能的检查协议(frontmatter 扁平键 check_entry_windows/check_entry_unix/check_timeout_secs)。
/// 只信 ~/Polaris/skills 下主机自装的技能——绝不从任务分支读,防协作者注入检查脚本。
pub fn resolve_check_skill(id: &str) -> Result<CheckSkillEntry, String> {
    // id 复用 delete_skill 同款安全闸:拒路径穿越。
    if id.is_empty()
        || id.contains("..")
        || id.contains('/')
        || id.contains('\\')
        || id.contains(':')
    {
        return Err(format!("检查技能 id 非法: {id}"));
    }
    let root = skills_dir().ok_or("无法获取用户目录")?;
    let dir = root.join(id);
    let skill_md = dir.join("skill.md");
    let content = fs::read_to_string(&skill_md)
        .map_err(|_| format!("检查技能 {id} 未安装(缺 skill.md),请在技能中心安装或重启应用"))?;
    let mut entry_win = String::new();
    let mut entry_unix = String::new();
    let mut timeout: u64 = 600;
    for line in content.lines().take(60) {
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            match k.trim() {
                "check_entry_windows" => entry_win = v.to_string(),
                "check_entry_unix" => entry_unix = v.to_string(),
                "check_timeout_secs" => timeout = v.parse().unwrap_or(600),
                _ => {}
            }
        }
    }
    let windows = cfg!(windows);
    let rel = if windows { &entry_win } else { &entry_unix };
    if rel.is_empty() {
        return Err(format!(
            "技能 {id} 未声明检查入口(check_entry_windows/check_entry_unix),不是检查技能"
        ));
    }
    if rel.contains("..") {
        return Err(format!("技能 {id} 检查入口路径非法: {rel}"));
    }
    let entry = dir.join(rel);
    if !entry.is_file() {
        return Err(format!("技能 {id} 检查入口脚本不存在: {rel}"));
    }
    Ok(CheckSkillEntry {
        skill_id: id.to_string(),
        entry,
        windows,
        timeout_secs: timeout.clamp(30, 3600),
    })
}

/// 列出本机已安装、声明了检查协议的技能(检查设置下拉用)。返回 (id, name)。
pub fn list_check_capable() -> Vec<(String, String)> {
    scan_user_skills()
        .into_iter()
        .filter(|s| resolve_check_skill(&s.id).is_ok())
        .map(|s| (s.id, s.name))
        .collect()
}

/// 启动时确保「浏览器智能体 browser-use」技能在 ~/Polaris/skills 落盘（含可执行 runner）。
/// 策略同上（版本号比对覆盖）。best-effort：失败只让该技能脚本暂不可用，不阻断 App 启动。
/// runner 必须真落到磁盘，spawn 的 claude agent 才能 `uv run …/browser_use_runner.py` 跑它。
pub fn seed_browser_use_skill() {
    let Some(root) = skills_dir() else {
        return;
    };
    let dest = root.join(BROWSER_USE_ID);
    let ver_file = dest.join(".polaris_version");
    let stored = fs::read_to_string(&ver_file).unwrap_or_default();
    let present = dest.join("skill.md").exists();
    if present && stored.trim() == BROWSER_USE_VERSION {
        return;
    }
    if write_browser_use_files(&dest).is_ok() {
        let _ = fs::write(&ver_file, BROWSER_USE_VERSION);
    }
}

/// 把内嵌的「浏览器智能体」文件写到目标目录（含 scripts/ 子树）。
/// 技能正文写成小写 `skill.md`，与 `scan_user_skills` 约定一致。
fn write_browser_use_files(dest: &Path) -> Result<(), String> {
    let scripts = dest.join("scripts");
    fs::create_dir_all(&scripts).map_err(|e| e.to_string())?;
    fs::write(dest.join("skill.md"), BROWSER_USE_SKILL_MD).map_err(|e| e.to_string())?;
    fs::write(scripts.join("browser_use_runner.py"), BROWSER_USE_RUNNER)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 把内嵌的「课件视频工坊」全部文件写到目标目录（建好子目录树）。
/// 技能正文写成小写 `skill.md` —— 与 `scan_user_skills` / `write_skill_file` 的约定一致，
/// 避免大小写敏感文件系统（Linux/macOS 构建）下扫描不到。
fn write_video_studio_files(dest: &Path) -> Result<(), String> {
    let scripts = dest.join("scripts");
    let pipeline = scripts.join("pipeline");
    let refs = dest.join("references");
    fs::create_dir_all(&pipeline).map_err(|e| e.to_string())?;
    fs::create_dir_all(&refs).map_err(|e| e.to_string())?;
    fs::write(dest.join("skill.md"), PVS_SKILL_MD).map_err(|e| e.to_string())?;
    fs::write(dest.join("manifest.json"), PVS_MANIFEST).map_err(|e| e.to_string())?;
    fs::write(scripts.join("install-deps.mjs"), PVS_INSTALL_DEPS).map_err(|e| e.to_string())?;
    fs::write(scripts.join("find-browser.mjs"), PVS_FIND_BROWSER).map_err(|e| e.to_string())?;
    fs::write(scripts.join("run.mjs"), PVS_RUN).map_err(|e| e.to_string())?;
    fs::write(pipeline.join("03-record.mjs"), PVS_RECORD).map_err(|e| e.to_string())?;
    fs::write(refs.join("WORKFLOW.md"), PVS_WORKFLOW).map_err(|e| e.to_string())?;
    Ok(())
}

// 注：原 `migrate_consult_mao_for_seeded_kb`（为早期播种过毛主席资料库的老用户启动时
// 自动补装 consult-mao 技能）已移除。现「请教毛主席」默认隐藏，只在用户主动安装
// 「毛主席」名人资料包（kb_pack_install）时才装该技能，启动时不再自动补装。

/// 启动时确保「壹伴排版优化」技能在 ~/Polaris/skills 落盘（多文件，含 wechat_yiban.py 可执行脚本）。
///
/// 与 deck/video studio 同策略：目录缺失 / 版本旧（`.polaris_version` < `WECHAT_TS_VERSION`）就（重）写；
/// 已是最新则跳过。脚本必须真落到磁盘，spawn 的 claude agent 才能 `python …/wechat_yiban.py` 跑它。
/// best-effort：失败只让「壹伴直送草稿」暂不可用，不阻断 App 启动。
pub fn seed_wechat_typesetter_skill() {
    let Some(root) = skills_dir() else {
        return;
    };
    let dest = root.join(WECHAT_TS_ID);
    let ver_file = dest.join(".polaris_version");
    let stored = fs::read_to_string(&ver_file).unwrap_or_default();
    let present = dest.join("skill.md").exists();
    if present && stored.trim() == WECHAT_TS_VERSION {
        return;
    }
    if write_wechat_typesetter_files(&dest).is_ok() {
        let _ = fs::write(&ver_file, WECHAT_TS_VERSION);
    }
}

/// 把内嵌的「壹伴排版优化」文件写到目标目录。技能正文写成小写 `skill.md`，与扫描约定一致。
fn write_wechat_typesetter_files(dest: &Path) -> Result<(), String> {
    let scripts = dest.join("scripts");
    fs::create_dir_all(&scripts).map_err(|e| e.to_string())?;
    fs::write(dest.join("skill.md"), WECHAT_TS_SKILL_MD).map_err(|e| e.to_string())?;
    fs::write(scripts.join("wechat_yiban.py"), WECHAT_TS_YIBAN_PY).map_err(|e| e.to_string())?;
    Ok(())
}

/// 启动时确保「微信聊天 · 每日待办」技能在 ~/Polaris/skills 落盘（多文件，含 wx_daily.py / wx_setup.py
/// 可执行脚本 + wx_config.example.json）。与壹伴排版同策略：目录缺失 / 版本旧就（重）写；已最新则跳过。
/// 注意：**不覆盖**用户已填好的 wx_config.json（内含 master key 与本机路径），只补样例。
/// best-effort：失败只让「微信每日待办」暂不可用，不阻断 App 启动。
pub fn seed_wechat_tasks_skill() {
    let Some(root) = skills_dir() else {
        return;
    };
    let dest = root.join(WECHAT_TASKS_ID);
    let ver_file = dest.join(".polaris_version");
    let stored = fs::read_to_string(&ver_file).unwrap_or_default();
    let present = dest.join("skill.md").exists();
    if present && stored.trim() == WECHAT_TASKS_VERSION {
        return;
    }
    if write_wechat_tasks_files(&dest).is_ok() {
        let _ = fs::write(&ver_file, WECHAT_TASKS_VERSION);
    }
}

/// 把内嵌的「微信每日待办」文件写到目标目录。技能正文写成小写 `skill.md`，与扫描约定一致。
/// wx_config.json（用户机密：master key + 路径）若已存在则保留不动。
fn write_wechat_tasks_files(dest: &Path) -> Result<(), String> {
    let scripts = dest.join("scripts");
    fs::create_dir_all(&scripts).map_err(|e| e.to_string())?;
    fs::write(dest.join("skill.md"), WECHAT_TASKS_SKILL_MD).map_err(|e| e.to_string())?;
    fs::write(scripts.join("wx_daily.py"), WECHAT_TASKS_DAILY_PY).map_err(|e| e.to_string())?;
    fs::write(scripts.join("wx_setup.py"), WECHAT_TASKS_SETUP_PY).map_err(|e| e.to_string())?;
    fs::write(
        scripts.join("wx_config.example.json"),
        WECHAT_TASKS_CONFIG_EXAMPLE,
    )
    .map_err(|e| e.to_string())?;
    // macOS 专属文件：wx_mac.py 与内存扫描器源码（放 scripts/mac/，首次运行 cc 编译）。
    fs::write(scripts.join("wx_mac.py"), WECHAT_TASKS_MAC_PY).map_err(|e| e.to_string())?;
    let mac_dir = scripts.join("mac");
    fs::create_dir_all(&mac_dir).map_err(|e| e.to_string())?;
    fs::write(
        mac_dir.join("find_all_keys_macos.c"),
        WECHAT_TASKS_MAC_SCANNER_C,
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
