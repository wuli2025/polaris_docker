//! 内置资料包 (下载=本地拷贝 seed-kb) —— 自原 kb.rs 纯移动, 逻辑零改动。

use super::*;

/// 资料包定义(编译期目录)。payload 走 `resources/seed-kb/<dir>`，仍随安装包分发，
/// 「下载」即本地拷贝，离线可用；将来要做远程包再扩 source 字段。
pub(crate) struct KbPackDef {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    /// `resources/seed-kb/` 与 `raw/` 下共用的目录名
    dir: &'static str,
    /// 配套 skill(技能目录 id)，安装/移除资料包时一并装/卸
    skill_id: &'static str,
}

pub(crate) fn pack_catalog() -> Vec<KbPackDef> {
    vec![KbPackDef {
        id: "mao",
        name: "毛主席",
        description: "《毛泽东选集》等著作的结构化资料库。装入后消息里写「请教毛主席」即可让他用毛选式大白话 + 矛盾分析法客观分析问题、生成标注来源的 HTML；同时自动创建「毛主席」人格项目。",
        dir: "毛主席",
        skill_id: "consult-mao",
    }]
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KbPackMeta {
    pub id: String,
    pub name: String,
    pub description: String,
    pub skill_id: String,
    pub installed: bool,
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn kb_pack_list() -> Vec<KbPackMeta> {
    let root = KB_ROOT.read().clone();
    pack_catalog()
        .into_iter()
        .map(|p| KbPackMeta {
            id: p.id.into(),
            name: p.name.into(),
            description: p.description.into(),
            skill_id: p.skill_id.into(),
            installed: root.join("raw").join(p.dir).exists(),
        })
        .collect()
}

/// 安装资料包：拷资料到 `raw/<名人>/` + 重扫索引 + 装配套 skill。返回索引文件总数。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn kb_pack_install(app: AppHandle, id: String) -> Result<usize, String> {
    let pack = pack_catalog()
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("没有资料包 '{}'", id))?;
    let src = seed_source(&app)
        .map(|s| s.join(pack.dir))
        .filter(|s| s.exists())
        .ok_or("安装包内未找到该资料包的数据(资源目录缺失)")?;
    let root = KB_ROOT.read().clone();
    copy_dir_recursive(&src, &root.join("raw").join(pack.dir)).map_err(|e| e.to_string())?;
    let docs = scan_all(&root);
    let n = docs.len();
    *INDEX.write() = docs;
    // 配套 skill(含资料库使用方法)装到用户技能目录。best-effort: 失败不回滚资料。
    let _ = crate::skills::install_skill(pack.skill_id.to_string());
    // 毛主席包附带「毛主席」人格项目(人格 CLAUDE.md + 专属 KB scope)
    if pack.id == "mao" {
        crate::conv::ensure_mao_project();
    }
    Ok(n)
}

/// 移除资料包：删 `raw/<名人>/` + 重扫索引 + 卸配套 skill。返回索引文件总数。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn kb_pack_remove(id: String) -> Result<usize, String> {
    let pack = pack_catalog()
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("没有资料包 '{}'", id))?;
    let root = KB_ROOT.read().clone();
    let dst = root.join("raw").join(pack.dir);
    if dst.exists() {
        fs::remove_dir_all(&dst).map_err(|e| e.to_string())?;
    }
    let _ = crate::skills::delete_skill(pack.skill_id.to_string());
    let docs = scan_all(&root);
    let n = docs.len();
    *INDEX.write() = docs;
    Ok(n)
}

/// 定位打进安装包的资料库种子目录(其内含 `毛主席/` 等资料包数据)。
/// 发布版走 Tauri `resource_dir`; 开发期回退到 `src-tauri/resources/seed-kb`。
pub(crate) fn seed_source(app: &AppHandle) -> Option<PathBuf> {
    if let Ok(rd) = app.path().resource_dir() {
        for cand in [rd.join("resources").join("seed-kb"), rd.join("seed-kb")] {
            if cand.exists() {
                return Some(cand);
            }
        }
    }
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("resources")
        .join("seed-kb");
    if dev.exists() {
        Some(dev)
    } else {
        None
    }
}

/// 递归拷贝目录内容到目标; 已存在的文件跳过(不覆盖用户改动)。
pub(crate) fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    for entry in WalkDir::new(src).into_iter().flatten() {
        let p = entry.path();
        let rel = match p.strip_prefix(src) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            if !target.exists() {
                fs::copy(p, &target)?;
            }
        }
    }
    Ok(())
}
