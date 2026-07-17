//! 生图供应商坞 —— 与聊天供应商**完全独立**的一张表。
//!
//! **为什么不复用 `store.rs` 那张表**(这是本文件存在的全部理由, 别合并):
//! 那套的 `provider_switch` / `cfg_for_view` / `detect_current` 一律把「有 base_url 的条目」
//! 当聊天家处理 —— 把地址套进 `ANTHROPIC_BASE_URL`、按 base_url 反查当前家。而生图家的
//! endpoint 是 `/v1/image_generation` 这种**喂给 claude 必挂**的地址。混进同一张表, 只要
//! 任何一处守卫漏了(switch/detect/purge 三条路径), 用户点一下就把聊天整条链路搞死;
//! `store.rs` 里 `detect_current` 的注释还留着「切一次 MiniMax 就再也切不回来」的血泪。
//! 独立一张表 = **数据结构上就不可能串味**, 不依赖任何人记得加守卫。
//!
//! 落盘 `~/Polaris/data/image-providers.json`, 与 `providers.json` 井水不犯河水。

use super::*;

/// 请求/响应形态。生图各家没有事实标准, 但**绝大多数非 MiniMax 的家都抄 OpenAI 的形状**
/// (豆包方舟即是), 故只分两档, 够覆盖且不过度抽象。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFlavor {
    /// MiniMax: body 带 aspect_ratio/prompt_optimizer → `data.image_urls[0]`
    Minimax,
    /// OpenAI 及兼容(豆包方舟等): body 带 size → `data[0].url` 或 `data[0].b64_json`
    Openai,
}

impl Default for ImageFlavor {
    fn default() -> Self {
        Self::Minimax
    }
}

/// 落盘结构。**字段名即 JSON 契约** —— 壳(polaris-app/polaris-cli)读它、翻成
/// `forge::image::ImageCfg` 再喂引擎; forge 永远不认识本类型(crate 边界铁律:
/// forge 只向下依赖 runtime, 引擎联动由壳编排)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredImageProvider {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub flavor: ImageFlavor,
    pub endpoint: String,
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    /// 备注 / 官网, 纯展示
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImageStore {
    /// 当前启用的生图家 id; 空 = 未配置(生图能力关闭)
    #[serde(default)]
    pub current_id: String,
    #[serde(default)]
    pub items: Vec<StoredImageProvider>,
}

/// 预设 —— **只是免手输的模板**, 用户仍需自己填 Key。地址/模型都可改。
pub struct ImagePreset {
    pub id: &'static str,
    pub name: &'static str,
    pub flavor: ImageFlavor,
    pub endpoint: &'static str,
    pub model: &'static str,
    pub note: &'static str,
}

pub const IMAGE_PRESETS: &[ImagePreset] = &[
    ImagePreset {
        id: "minimax-image",
        name: "MiniMax 图像",
        flavor: ImageFlavor::Minimax,
        endpoint: "https://api.minimaxi.com/v1/image_generation",
        model: "image-01",
        note: "国内可直连;画幅走 aspect_ratio(16:9 等)",
    },
    ImagePreset {
        id: "openai-image",
        name: "OpenAI 图像",
        flavor: ImageFlavor::Openai,
        endpoint: "https://api.openai.com/v1/images/generations",
        model: "gpt-image-1",
        note: "官方或任何兼容网关;画幅走 size(1024x1024 等)",
    },
    ImagePreset {
        id: "doubao-image",
        name: "豆包 Seedream(方舟)",
        flavor: ImageFlavor::Openai,
        endpoint: "https://ark.cn-beijing.volces.com/api/v3/images/generations",
        model: "doubao-seedream-4-0-250828",
        note: "火山方舟;说 OpenAI 形状,模型名需在方舟控制台确认",
    },
];

fn image_store_path() -> Option<PathBuf> {
    let user = UserDirs::new()?;
    Some(
        user.home_dir()
            .join("Polaris")
            .join("data")
            .join("image-providers.json"),
    )
}

fn load_store() -> ImageStore {
    let Some(p) = image_store_path() else {
        return ImageStore::default();
    };
    let Ok(s) = fs::read_to_string(p) else {
        return ImageStore::default();
    };
    serde_json::from_str(&s).unwrap_or_default()
}

fn save_store(st: &ImageStore) -> Result<(), String> {
    let p = image_store_path().ok_or("拿不到用户目录")?;
    if let Some(d) = p.parent() {
        fs::create_dir_all(d).map_err(|e| format!("建目录失败: {e}"))?;
    }
    let s = serde_json::to_string_pretty(st).map_err(|e| format!("序列化失败: {e}"))?;
    fs::write(&p, s).map_err(|e| format!("写 {} 失败: {e}", p.display()))
}

// ───────────────────────── 视图 / 入参 ─────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageProviderView {
    pub id: String,
    pub name: String,
    pub flavor: ImageFlavor,
    pub endpoint: String,
    pub model: String,
    pub note: String,
    /// 只回「配没配 key」, **绝不回明文** —— 与聊天坞 hasKey 同一口径
    pub has_key: bool,
    pub is_current: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageProviderListResult {
    pub items: Vec<ImageProviderView>,
    pub current_id: String,
    /// 预设模板(前端「新建」时给候选, 免手输)
    pub presets: Vec<ImagePresetView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagePresetView {
    pub id: String,
    pub name: String,
    pub flavor: ImageFlavor,
    pub endpoint: String,
    pub model: String,
    pub note: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageProviderInput {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub flavor: ImageFlavor,
    pub endpoint: String,
    pub model: String,
    /// 空 = 不改动已存的 key(编辑时不必重填)
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub note: String,
}

/// 壳拿去喂 forge 的最终配置(已解析出 key)。
#[derive(Debug, Clone)]
pub struct ImageGenConfig {
    pub endpoint: String,
    pub model: String,
    pub api_key: String,
    pub flavor: ImageFlavor,
    pub name: String,
}

/// 当前启用的生图配置。无配置 / 无 key → None(= 生图能力关闭)。
/// 这是壳(lib.rs 的 forge_image / apihub / polaris-cli)取配置的**唯一入口**。
pub fn current_image_config() -> Option<ImageGenConfig> {
    let st = load_store();
    let it = st.items.iter().find(|x| x.id == st.current_id)?;
    let key = it.api_key.trim();
    if key.is_empty() || it.endpoint.trim().is_empty() || it.model.trim().is_empty() {
        return None;
    }
    Some(ImageGenConfig {
        endpoint: it.endpoint.trim().to_string(),
        model: it.model.trim().to_string(),
        api_key: key.to_string(),
        flavor: it.flavor,
        name: it.name.clone(),
    })
}

// ───────────────────────── 命令 ─────────────────────────

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn image_provider_list() -> Result<ImageProviderListResult, String> {
    let st = load_store();
    Ok(ImageProviderListResult {
        items: st
            .items
            .iter()
            .map(|it| ImageProviderView {
                id: it.id.clone(),
                name: it.name.clone(),
                flavor: it.flavor,
                endpoint: it.endpoint.clone(),
                model: it.model.clone(),
                note: it.note.clone(),
                has_key: !it.api_key.trim().is_empty(),
                is_current: it.id == st.current_id,
            })
            .collect(),
        current_id: st.current_id.clone(),
        presets: IMAGE_PRESETS
            .iter()
            .map(|p| ImagePresetView {
                id: p.id.to_string(),
                name: p.name.to_string(),
                flavor: p.flavor,
                endpoint: p.endpoint.to_string(),
                model: p.model.to_string(),
                note: p.note.to_string(),
            })
            .collect(),
    })
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn image_provider_save(input: ImageProviderInput) -> Result<ImageProviderListResult, String> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err("名称不能为空".into());
    }
    let endpoint = input.endpoint.trim();
    if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
        return Err("请求地址必须以 http:// 或 https:// 开头".into());
    }
    if input.model.trim().is_empty() {
        return Err("模型名不能为空".into());
    }

    let mut st = load_store();
    let id = if input.id.trim().is_empty() {
        format!("img-{}", now_ms())
    } else {
        input.id.trim().to_string()
    };

    // 编辑时 api_key 留空 = 保持原 key 不变(前端不回显明文, 不能让空值把 key 抹掉)
    let existing_key = st
        .items
        .iter()
        .find(|x| x.id == id)
        .map(|x| x.api_key.clone())
        .unwrap_or_default();
    let api_key = if input.api_key.trim().is_empty() {
        existing_key
    } else {
        input.api_key.trim().to_string()
    };

    let rec = StoredImageProvider {
        id: id.clone(),
        name: name.to_string(),
        flavor: input.flavor,
        endpoint: endpoint.to_string(),
        model: input.model.trim().to_string(),
        api_key,
        note: input.note.trim().to_string(),
    };
    match st.items.iter_mut().find(|x| x.id == id) {
        Some(slot) => *slot = rec,
        None => st.items.push(rec),
    }
    // 第一个配好 key 的条目自动设为当前 —— 省掉用户「存了却没生效」的困惑
    if st.current_id.is_empty() {
        st.current_id = id;
    }
    save_store(&st)?;
    image_provider_list()
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn image_provider_delete(id: String) -> Result<ImageProviderListResult, String> {
    let mut st = load_store();
    st.items.retain(|x| x.id != id);
    if st.current_id == id {
        // 删掉的正是当前家 → 顺延到还剩的第一个, 没了就置空(生图能力随之关闭)
        st.current_id = st.items.first().map(|x| x.id.clone()).unwrap_or_default();
    }
    save_store(&st)?;
    image_provider_list()
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn image_provider_switch(id: String) -> Result<ImageProviderListResult, String> {
    let mut st = load_store();
    if !st.items.iter().any(|x| x.id == id) {
        return Err(format!("没有 id={id} 的生图供应商"));
    }
    st.current_id = id;
    save_store(&st)?;
    image_provider_list()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flavor_serializes_lowercase() {
        // 前端与落盘都按小写字面量认 flavor, 改了就是破坏兼容。
        assert_eq!(
            serde_json::to_string(&ImageFlavor::Minimax).unwrap(),
            "\"minimax\""
        );
        assert_eq!(
            serde_json::to_string(&ImageFlavor::Openai).unwrap(),
            "\"openai\""
        );
    }

    #[test]
    fn store_roundtrips_and_old_file_without_flavor_still_loads() {
        let st = ImageStore {
            current_id: "a".into(),
            items: vec![StoredImageProvider {
                id: "a".into(),
                name: "n".into(),
                flavor: ImageFlavor::Openai,
                endpoint: "https://x/y".into(),
                model: "m".into(),
                api_key: "k".into(),
                note: String::new(),
            }],
        };
        let s = serde_json::to_string(&st).unwrap();
        let back: ImageStore = serde_json::from_str(&s).unwrap();
        assert_eq!(back.items[0].flavor, ImageFlavor::Openai);
        // 缺 flavor/api_key/note 的老档要能读(serde default), 否则升级即丢配置
        let old = r#"{"current_id":"a","items":[{"id":"a","name":"n","endpoint":"https://x","model":"m"}]}"#;
        let o: ImageStore = serde_json::from_str(old).unwrap();
        assert_eq!(o.items[0].flavor, ImageFlavor::Minimax); // default
        assert!(o.items[0].api_key.is_empty());
    }

    #[test]
    fn presets_are_well_formed() {
        for p in IMAGE_PRESETS {
            assert!(p.endpoint.starts_with("https://"), "{} 的地址应是 https", p.id);
            assert!(!p.model.is_empty(), "{} 缺模型名", p.id);
        }
        // MiniMax 那家必须是 minimax 形状 —— forge 按 flavor 分流请求体/响应解析
        let mm = IMAGE_PRESETS.iter().find(|p| p.id == "minimax-image").unwrap();
        assert_eq!(mm.flavor, ImageFlavor::Minimax);
    }
}
