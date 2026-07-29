//! 生图的**壳桥接** —— 全 crate 里唯一同时认识「kernel 的生图供应商坞」与
//! 「forge 的生图引擎」的地方(同 `wiring.rs` 的角色)。
//!
//! 为什么必须有这一层: crate 边界铁律写死了 forge 只向下依赖 runtime、**不认识 kernel**
//! (见 polaris-forge/Cargo.toml 头注),引擎联动一律由壳编排。所以配置不能让 forge 自己去读
//! —— 早期 `tts::discover_key()` 就是那么干的:手工解析 providers.json 的平行通道,kernel
//! 改字段名它读不到,还会**静默回落到硬编码 image-01 假装成功**。这里把配置显式翻译后注入,
//! 没配就明确报错,不猜。
//!
//! 双注册(项目铁律): 桌面走 `lib.rs` 的 generate_handler!,Web/Docker 走 `apihub.rs` 的
//! 命令名 match —— 只写一边会让另一端 404 / command not found。

use polaris_kernel::provider::{self, ImageFlavor};
use serde_json::{json, Value};

use crate::forge::image::{Flavor, ImageCfg};

/// kernel 的 flavor → forge 的 flavor。两边都是闭集,新增一档时编译器会在这里点名
/// (`match` 不写 `_` 通配就是为了这个 —— 别加)。
fn to_forge_flavor(f: ImageFlavor) -> Flavor {
    match f {
        ImageFlavor::Minimax => Flavor::Minimax,
        ImageFlavor::Openai => Flavor::Openai,
    }
}

/// 取当前生图配置并翻成 forge 的入参。没配 / 没 key → None。
fn current_cfg() -> Option<(ImageCfg, String)> {
    candidates().into_iter().next()
}

/// 按优先级排好的候选(火山方舟 → MiniMax);显式配置时只有一个。
fn candidates() -> Vec<(ImageCfg, String)> {
    provider::image_config_candidates()
        .into_iter()
        .map(|c| {
            (
                ImageCfg {
                    endpoint: c.endpoint,
                    model: c.model,
                    api_key: c.api_key,
                    flavor: to_forge_flavor(c.flavor),
                },
                c.name,
            )
        })
        .collect()
}

/// 生图同步内核。server/apihub 的命令路由本就在阻塞线程池里,直调这里
/// (与 `forge_tts_sync` 同一套路)。
pub fn forge_image_sync(
    prompt: String,
    out: String,
    ratio: Option<String>,
) -> Result<Value, String> {
    let cands = candidates();
    if cands.is_empty() {
        return Err(
            "还没配生图模型:到「设置 → API 供应商 → 生图模型」加一家并填 Key(火山方舟 / MiniMax / OpenAI 都行)"
                .into(),
        );
    }
    // 借用链有多个候选时逐个试:火山方舟排在前面, 但那是**借来的聊天套餐 key**,
    // 能不能打生图端点未经证实。第一家打不通就顺延, 而不是让原本靠 MiniMax 出图好好的
    // 用户,因为多配了一个方舟聊天 key 就突然出不了图。
    // (显式选中的那家只会有一个候选 —— 用户点名了谁就是谁, 失败如实报错, 不偷换。)
    let last = cands.len() - 1;
    let mut first_err = String::new();
    for (i, (cfg, name)) in cands.into_iter().enumerate() {
        match crate::forge::image::generate(&cfg, &prompt, &out, ratio.as_deref()) {
            Ok(mut v) => {
                // 把「用的哪家」带回去 —— 出了图但风格不对时,用户第一个想知道的就是这个。
                if let Some(o) = v.as_object_mut() {
                    o.insert("provider".into(), json!(name));
                }
                return Ok(v);
            }
            Err(e) => {
                if i == 0 {
                    first_err = format!("{name}: {e}");
                }
                if i == last {
                    // 全试完了才报错, 且把**第一家**的原因也带上 —— 只报最后一家的话,
                    // 用户会以为问题出在 MiniMax, 而真正没配好的是排在前面的火山。
                    return Err(if last == 0 {
                        format!("{name}: {e}")
                    } else {
                        format!("{name}: {e}(此前已试过 {first_err})")
                    });
                }
                eprintln!("[imagegen] {name} 生图失败, 顺延下一家: {e}");
            }
        }
    }
    unreachable!("候选非空且最后一家必定 return")
}

/// 桌面命令。生图常 20–60s,必须 async + spawn_blocking,否则钉住 Tauri 主线程整个 UI 卡死。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn forge_image(
    prompt: String,
    out: String,
    ratio: Option<String>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || forge_image_sync(prompt, out, ratio))
        .await
        .map_err(|e| format!("生图任务异常退出: {e}"))?
}

/// 生图能力探测:给 chat 的提示词门控用(有没有配、配的是哪家)。
/// 回 (家名, 是否可用) —— 与 `provider::image_gen_capability()` 同签名,替换它。
pub fn capability() -> (String, bool) {
    match current_cfg() {
        Some((cfg, name)) => (format!("{name}({})", cfg.model), true),
        None => (String::new(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flavor_translation_is_total() {
        // 两个 flavor 都必须有映射(match 无通配 => 新增枚举时这里编译失败, 这是故意的)
        assert_eq!(to_forge_flavor(ImageFlavor::Minimax), Flavor::Minimax);
        assert_eq!(to_forge_flavor(ImageFlavor::Openai), Flavor::Openai);
    }

    #[test]
    fn no_config_yields_actionable_error_not_silent_fallback() {
        // 关键回归: 老代码没 key 时会静默用硬编码 image-01 假装成功。
        // 现在没配就必须明确报错, 且错误里要告诉用户去哪配。
        if provider::current_image_config().is_none() {
            let e = forge_image_sync("猫".into(), "x.png".into(), None).unwrap_err();
            assert!(e.contains("生图模型"), "错误应指路到设置: {e}");
            assert!(!e.contains("image-01"), "不该暴露任何硬编码模型名: {e}");
            assert_eq!(capability(), (String::new(), false));
        }
    }
}
