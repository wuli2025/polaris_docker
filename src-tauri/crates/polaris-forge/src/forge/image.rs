//! Polaris Forge · 文生图(多家可配,纯 Rust ureq)。
//!
//! **配置由壳注入**(`ImageCfg`),本文件不认识 kernel 的供应商坞 —— crate 边界铁律:
//! forge 只向下依赖 runtime,引擎联动一律由壳(polaris-app / polaris-cli)编排。
//! 这也顺手拆了个雷:早期版本靠 `tts::discover_key()` 手工解析 providers.json,那是条
//! 绕过 kernel API 的平行通道,kernel 改字段名它读不到、还会**静默回落到硬编码 image-01
//! 假装成功**。现在配置进不来就明确报错,不猜。
//!
//! 支持两种形状(`Flavor`),覆盖目前所有目标家:
//! - `Minimax`: body {model,prompt,n,response_format:"url",aspect_ratio,prompt_optimizer}
//!              → `data.image_urls[0]`(临时 URL)→ 下载落盘
//! - `Openai` : body {model,prompt,n,size}  → `data[0].url` 或 `data[0].b64_json`
//!              (OpenAI 官方 / 豆包方舟 / 各兼容网关都是这形状)
//!
//! 存在的理由:spec 路线的 `image-full` / `image-text` 版式吃本地图片路径,得有人把图变出来。
//! 用纯 Rust 而非 sidecar 调 genimg.py —— forge 已有 ureq,不引 Python 运行时(Docker slim
//! 里没有 python 也能出带图课件,与「零浏览器」同一个理由)。

use serde_json::{json, Value};
use std::time::Duration;

/// 生图慢(常 20–60s),给足超时;比 genimg.py 的 180s 略紧,失败快点暴露。
const GEN_TIMEOUT: u64 = 150;
const DOWNLOAD_TIMEOUT: u64 = 120;
/// 通用画幅档。MiniMax 直接吃这个字符串;OpenAI 系要换算成 size(见 `ratio_to_size`)。
/// 传别的会被网关拒,不如本地先挡住给出可读错误。
const RATIOS: &[&str] = &["1:1", "16:9", "4:3", "3:2", "2:3", "3:4", "9:16", "21:9"];

/// 请求/响应形状。与 kernel 的 `ImageFlavor` 一一对应,由壳负责翻译
/// —— 两边都是 2 个值的闭集,新增一档时编译器会在壳的 match 处点名。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    Minimax,
    Openai,
}

/// 壳注入的生图配置。
#[derive(Debug, Clone)]
pub struct ImageCfg {
    pub endpoint: String,
    pub model: String,
    pub api_key: String,
    pub flavor: Flavor,
}

/// 画幅 → OpenAI 系的 size。各家支持的档位不同,这里只保证「长宽比对得上」,
/// 具体尺寸取常见可用值;网关不认时会原样把错误抛给用户,比我们猜一个更诚实。
fn ratio_to_size(ratio: &str) -> &'static str {
    match ratio {
        "1:1" => "1024x1024",
        "16:9" | "21:9" => "1536x864",
        "4:3" | "3:2" => "1344x1008",
        "9:16" => "864x1536",
        "3:4" | "2:3" => "1008x1344",
        _ => "1024x1024",
    }
}

fn agent(timeout: u64) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(timeout))
        .build()
}

/// 解 b64_json。gpt-image-* 默认就回这个而不是 URL。
fn b64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .ok()
}

/// 按 flavor 组装请求体。抽出来是为了能单测 —— 请求体写错是最容易静默跑偏的地方。
pub fn build_body(cfg: &ImageCfg, prompt: &str, ratio: &str) -> Value {
    match cfg.flavor {
        Flavor::Minimax => json!({
            "model": cfg.model,
            "prompt": prompt,
            "n": 1,
            "response_format": "url",
            "aspect_ratio": ratio,
            "prompt_optimizer": true,
        }),
        // OpenAI 系不认 aspect_ratio,只认 size;也没有 prompt_optimizer。
        // 不发 response_format: gpt-image-* 只回 b64_json 且**显式传该字段会 400**,
        // 下面两种回法都认,让网关自己选。
        Flavor::Openai => json!({
            "model": cfg.model,
            "prompt": prompt,
            "n": 1,
            "size": ratio_to_size(ratio),
        }),
    }
}

/// 从响应里取「图」。回 Ok(Got::Url) 或 Ok(Got::B64);取不到则回可读错误。
#[derive(Debug)]
enum Got {
    Url(String),
    B64(String),
}

fn extract(cfg: &ImageCfg, v: &Value) -> Result<Got, String> {
    match cfg.flavor {
        Flavor::Minimax => {
            if let Some(u) = v
                .get("data")
                .and_then(|d| d.get("image_urls"))
                .and_then(|a| a.as_array())
                .and_then(|a| a.first())
                .and_then(|x| x.as_str())
            {
                return Ok(Got::Url(u.to_string()));
            }
            // MiniMax 把业务错误塞在 200 的 base_resp 里(status_code != 0),
            // 不看它会把「余额不足」当成空结果一路重试到超时。
            let br = v.get("base_resp").cloned().unwrap_or_else(|| v.clone());
            let code = br.get("status_code").and_then(|x| x.as_i64()).unwrap_or(-1);
            let msg = br
                .get("status_msg")
                .and_then(|x| x.as_str())
                .unwrap_or("未知错误");
            Err(format!("MiniMax 拒绝(status_code={code}): {msg}"))
        }
        Flavor::Openai => {
            let first = v.get("data").and_then(|d| d.as_array()).and_then(|a| a.first());
            if let Some(d) = first {
                if let Some(u) = d.get("url").and_then(|x| x.as_str()) {
                    return Ok(Got::Url(u.to_string()));
                }
                if let Some(b) = d.get("b64_json").and_then(|x| x.as_str()) {
                    return Ok(Got::B64(b.to_string()));
                }
            }
            // OpenAI 系的错误在 200 里也可能是 {"error":{"message":...}}
            let msg = v
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|x| x.as_str())
                .unwrap_or("响应里既没有 data[0].url 也没有 data[0].b64_json");
            Err(format!("上游拒绝: {msg}"))
        }
    }
}

/// 判断 MiniMax 的错误码是否「重试也是白搭」(鉴权失败 / 余额不足 / 参数非法)。
fn minimax_fatal(err: &str) -> bool {
    ["1004", "1008", "1013", "2013"]
        .iter()
        .any(|c| err.contains(&format!("status_code={c}")))
}

/// 文生图 → 落盘 png/jpg。ratio 缺省 16:9(课件配图主力画幅)。
/// `cfg` 由壳从生图供应商坞取(`kernel::provider::current_image_config()`)。
///
/// 返回 {ok,out,bytes,ratio,model,attempts}。失败返 Err(可读原因),调用方(SKILL/模型)
/// 据此决定是重试、换 prompt 还是退无图版式 —— 生图失败不该让整套课件卡住。
pub fn generate(
    cfg: &ImageCfg,
    prompt: &str,
    out_path: &str,
    ratio: Option<&str>,
) -> Result<Value, String> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err("prompt 为空".into());
    }
    let ratio = ratio.unwrap_or("16:9").trim();
    if !RATIOS.contains(&ratio) {
        return Err(format!(
            "不支持的画幅 \"{ratio}\";可用: {}",
            RATIOS.join(" / ")
        ));
    }
    if cfg.api_key.trim().is_empty() {
        return Err("生图供应商没配 Key:到「设置 → 生图模型」填一个".into());
    }

    let body = build_body(cfg, prompt, ratio);

    // 生图接口偶发限流/超时是常态(genimg.py 也是重试 4 次),退避重试;
    // 但 4xx 认证/参数错重试无意义,直接抛。
    let mut last = String::new();
    let mut got: Option<Got> = None;
    let mut attempts = 0u32;
    for i in 0..4 {
        attempts = i + 1;
        match agent(GEN_TIMEOUT)
            .post(&cfg.endpoint)
            .set("Authorization", &format!("Bearer {}", cfg.api_key))
            .set("Content-Type", "application/json")
            .send_json(body.clone())
        {
            Ok(resp) => {
                let v: Value = resp
                    .into_json()
                    .map_err(|e| format!("响应不是合法 JSON: {e}"))?;
                match extract(cfg, &v) {
                    Ok(g) => {
                        got = Some(g);
                        break;
                    }
                    Err(e) => {
                        if minimax_fatal(&e) {
                            return Err(e);
                        }
                        last = e;
                    }
                }
            }
            Err(ureq::Error::Status(code, r)) => {
                let txt = r.into_string().unwrap_or_default();
                last = format!("HTTP {code}: {}", txt.chars().take(200).collect::<String>());
                if (400..500).contains(&code) && code != 429 {
                    return Err(last);
                }
            }
            Err(e) => last = format!("请求失败: {e}"),
        }
        std::thread::sleep(Duration::from_secs(2 + (i as u64) * 3));
    }
    let Some(got) = got else {
        return Err(format!("生图失败(试了 {attempts} 次): {last}"));
    };

    if let Some(parent) = std::path::Path::new(out_path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("建目录失败: {e}"))?;
        }
    }
    // 取字节:URL 是临时的,拿到就存。下载单独重试——生成成功但下载抖动时不该重新烧一次生图额度。
    // b64 路线(gpt-image-* 的默认回法)不用下载,直接解。
    let mut bytes: Vec<u8> = Vec::new();
    match got {
        Got::B64(b) => {
            bytes = b64_decode(&b).ok_or("b64_json 解不开(不是合法 base64)")?;
        }
        Got::Url(url) => {
            let mut derr = String::new();
            for i in 0..3 {
                match agent(DOWNLOAD_TIMEOUT).get(&url).call() {
                    Ok(r) => {
                        let mut buf: Vec<u8> = Vec::new();
                        match std::io::Read::read_to_end(&mut r.into_reader(), &mut buf) {
                            Ok(_) if buf.len() > 1024 => {
                                bytes = buf;
                                break;
                            }
                            Ok(_) => derr = format!("下载内容过小({} 字节)", buf.len()),
                            Err(e) => derr = format!("读取失败: {e}"),
                        }
                    }
                    Err(e) => derr = format!("下载失败: {e}"),
                }
                std::thread::sleep(Duration::from_secs(2 + i * 2));
            }
            if bytes.is_empty() {
                return Err(format!("图已生成但取不回来: {derr}"));
            }
        }
    }
    // 确认真是 PNG/JPEG 再落盘:网关出错时可能回一段 HTML/JSON,
    // 存成 .png 会一路带到 pptx 打包才炸,那时更难查。
    let format = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpeg"
    } else {
        return Err("下载到的不是 PNG/JPEG(网关可能返回了错误页)".into());
    };
    // 按请求的路径原样存 —— **不**按真实格式改扩展名:spec 里引用的是这个路径,
    // 改名会让引用落空。MiniMax 现在回的其实是 JPEG(哪怕你写 out.png),故 pptx
    // 打包一侧按魔数认格式,不信后缀;`format` 字段把真相摆出来,别让它静默。
    std::fs::write(out_path, &bytes).map_err(|e| format!("写 {out_path} 失败: {e}"))?;

    Ok(json!({
        "ok": true,
        "out": std::path::Path::new(out_path)
            .canonicalize()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| out_path.to_string()),
        "bytes": bytes.len(),
        "format": format,
        "ratio": ratio,
        "model": cfg.model,
        "attempts": attempts,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(flavor: Flavor) -> ImageCfg {
        ImageCfg {
            endpoint: "https://example.invalid/gen".into(),
            model: "m-1".into(),
            api_key: "k".into(),
            flavor,
        }
    }

    #[test]
    fn rejects_bad_input_before_touching_network() {
        // 空 prompt / 非法画幅必须本地就挡下,不浪费一次真实调用。
        let c = cfg(Flavor::Minimax);
        assert!(generate(&c, "", "x.png", None).unwrap_err().contains("prompt"));
        let e = generate(&c, "猫", "x.png", Some("5:7")).unwrap_err();
        assert!(e.contains("不支持的画幅"), "{e}");
        assert!(e.contains("16:9"), "错误里应列出可用画幅: {e}");
        // 没配 Key 也必须本地挡下, 而不是拿空 Bearer 去撞网关
        let mut nk = cfg(Flavor::Minimax);
        nk.api_key = "  ".into();
        assert!(generate(&nk, "猫", "x.png", None).unwrap_err().contains("Key"));
    }

    #[test]
    fn body_shape_differs_per_flavor() {
        // MiniMax 吃 aspect_ratio; OpenAI 系不认它、只认 size —— 发错了就是 400 或静默跑偏。
        let mm = build_body(&cfg(Flavor::Minimax), "猫", "16:9");
        assert_eq!(mm["aspect_ratio"], "16:9");
        assert_eq!(mm["response_format"], "url");
        assert!(mm.get("size").is_none(), "MiniMax 不该带 size");

        let oa = build_body(&cfg(Flavor::Openai), "猫", "16:9");
        assert_eq!(oa["size"], "1536x864");
        assert!(oa.get("aspect_ratio").is_none(), "OpenAI 系不认 aspect_ratio");
        // gpt-image-* 显式传 response_format 会 400, 必须不发
        assert!(oa.get("response_format").is_none(), "OpenAI 系不该带 response_format");
    }

    #[test]
    fn ratio_to_size_keeps_aspect() {
        // 换算后的长宽比必须和请求的画幅一致, 否则出的图会被拉伸
        for (r, expect_landscape) in [("16:9", true), ("9:16", false), ("1:1", true)] {
            let s = ratio_to_size(r);
            let (w, h): (u32, u32) = {
                let mut it = s.split('x');
                (
                    it.next().unwrap().parse().unwrap(),
                    it.next().unwrap().parse().unwrap(),
                )
            };
            assert_eq!(w >= h, expect_landscape, "{r} -> {s} 方向不对");
        }
        assert_eq!(ratio_to_size("16:9"), "1536x864");
        // 未知画幅回落正方形而不是 panic
        assert_eq!(ratio_to_size("weird"), "1024x1024");
    }

    #[test]
    fn extract_reads_both_shapes_and_surfaces_errors() {
        use serde_json::json;
        // MiniMax 正常回法
        let v = json!({"data":{"image_urls":["https://x/y.png"]}});
        assert!(matches!(extract(&cfg(Flavor::Minimax), &v), Ok(Got::Url(u)) if u == "https://x/y.png"));
        // MiniMax 把业务错误塞在 200 里 —— 必须当错误抛, 不能当空结果重试到超时
        let v = json!({"base_resp":{"status_code":1008,"status_msg":"余额不足"}});
        let e = extract(&cfg(Flavor::Minimax), &v).unwrap_err();
        assert!(e.contains("1008") && e.contains("余额不足"), "{e}");
        assert!(minimax_fatal(&e), "余额不足应判为不可重试");
        // 限流类错误则应允许重试
        let v = json!({"base_resp":{"status_code":1002,"status_msg":"限流"}});
        assert!(!minimax_fatal(&extract(&cfg(Flavor::Minimax), &v).unwrap_err()));

        // OpenAI 系两种回法都要认
        let v = json!({"data":[{"url":"https://a/b.png"}]});
        assert!(matches!(extract(&cfg(Flavor::Openai), &v), Ok(Got::Url(_))));
        let v = json!({"data":[{"b64_json":"aGk="}]});
        assert!(matches!(extract(&cfg(Flavor::Openai), &v), Ok(Got::B64(_))));
        // 错误体要把上游的话原样带出来, 而不是「取不到图」这种废话
        let v = json!({"error":{"message":"model not found"}});
        assert!(extract(&cfg(Flavor::Openai), &v).unwrap_err().contains("model not found"));
    }

    #[test]
    fn b64_decode_works() {
        assert_eq!(b64_decode("aGk=").unwrap(), b"hi");
        assert!(b64_decode("!!!not-base64!!!").is_none());
    }
}
