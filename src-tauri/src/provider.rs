//! 板块 ⑥ API 供应商坞 — Claude Code 供应商切换 + token 用量/成本看板
//!
//! 剥离自 cc-switch 的 Claude 供应商能力, 与 Polaris 墨蓝水墨前端融为一体。
//! - 每个供应商携带一份完整 `settings_config`(env + includeCoAuthoredBy/attribution
//!   等顶层键)。
//! - 联动/隔离两档(store.link_global, 默认**隔离**):
//!   * 隔离 — 切换只写 Polaris 进程 env(spawn 的 claude 子进程继承, 且进程 env 实测
//!     优先于 settings.json), 终端里用户自己的 `claude` 完全不受影响 —— 根治
//!     「Polaris 切 MiniMax 把外部 CLI 一起带跑」的串台。
//!   * 联动 — 行为同旧版: 额外把 settings_config 合并写进 `~/.claude/settings.json`
//!     (只接管我们管理的键, 其余原样保留; 首次改动前 .polaris.bak 备份),
//!     终端 CLI 跟着 Polaris 一起切。
//! - 用量看板: 读 `~/.claude/projects/**/*.jsonl`(ccusage 思路), 聚合 token + 按内置
//!   定价表估算成本, 今日/周/月/年 + 14 天趋势。零额外网络、零额外依赖。
//! - Codex / Copilot: 说 OpenAI 协议, 让 `claude` 直连需翻译代理(cc-switch 的 proxy/,
//!   1.5MB+), 与轻量化冲突 → 不路由。Codex 授权委托官方 `codex` CLI。

use anyhow::Result;
use directories::UserDirs;
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
#[cfg(feature = "desktop")]
use tauri::AppHandle;
#[cfg(not(feature = "desktop"))]
use crate::host::AppHandle;
use walkdir::WalkDir;

// 构建期注入的「粉丝福利」MiniMax key(XOR 滚动混淆字节, 见 build.rs)。
include!(concat!(env!("OUT_DIR"), "/gift_key.rs"));

const DEFAULT_TOKEN_FIELD: &str = "ANTHROPIC_AUTH_TOKEN";
const API_KEY_FIELD: &str = "ANTHROPIC_API_KEY";

/// 切换时先从 live env 清掉这些受管键, 再套用供应商配置 → 切换结果确定。
const MANAGED_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ENABLE_TOOL_SEARCH",
    "DISABLE_AUTOUPDATER",
    "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS",
    "CLAUDE_CODE_EFFORT_LEVEL",
    // 模型钉选 —— 纳入受管, 切换时先清后套, 否则上一家的模型名会串到下一家
    // (例: 切回 Claude 官方却残留 MiniMax-M3 → 官方拿去请求 Anthropic 直接报错)。
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    // Polaris 联动写全局时盖的戳, 见 apply_settings_config —— 隔离模式净化的最强证据
    "POLARIS_LINKED",
];

/// 模型钉选的四档键 —— 第三方单模型供应商把这四档全设成同一个 model id,
/// 后台小任务(走 HAIKU 档)就不会回落 Claude 默认名而被网关当未知模型处理。
const MODEL_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
];
const MANAGED_TOP_KEYS: &[&str] = &["attribution", "includeCoAuthoredBy"];

// ───────────────────────── 预设供应商表 (全量 55) ─────────────────────────
// base_url / apiKeyField / category 取自 cc-switch claudeProviderPresets。
// kind: official(清空 env) | key(写 base+token) | codex / copilot(需授权代理)

struct Preset {
    id: &'static str,
    name: &'static str,
    base_url: &'static str,
    token_field: &'static str,
    category: &'static str,
    kind: &'static str,
}

const PRESETS: &[Preset] = &[
    Preset { id: "claude-official", name: "Claude 官方", base_url: "", token_field: DEFAULT_TOKEN_FIELD, category: "official", kind: "official" },
    Preset { id: "shengsuanyun", name: "胜算云", base_url: "https://router.shengsuanyun.com/api", token_field: DEFAULT_TOKEN_FIELD, category: "aggregator", kind: "key" },
    Preset { id: "patewayai", name: "PatewayAI", base_url: "https://api.pateway.ai", token_field: API_KEY_FIELD, category: "third_party", kind: "key" },
    Preset { id: "agentplan", name: "火山方舟 Agentplan", base_url: "https://ark.cn-beijing.volces.com/api/coding", token_field: DEFAULT_TOKEN_FIELD, category: "cn_official", kind: "key" },
    Preset { id: "byteplus", name: "BytePlus", base_url: "https://ark.ap-southeast.bytepluses.com/api/coding", token_field: DEFAULT_TOKEN_FIELD, category: "cn_official", kind: "key" },
    Preset { id: "doubaoseed", name: "豆包 Seed", base_url: "https://ark.cn-beijing.volces.com/api/compatible", token_field: DEFAULT_TOKEN_FIELD, category: "cn_official", kind: "key" },
    Preset { id: "gemini-native", name: "Gemini Native", base_url: "https://generativelanguage.googleapis.com", token_field: API_KEY_FIELD, category: "third_party", kind: "key" },
    Preset { id: "deepseek", name: "DeepSeek 深度求索", base_url: "https://api.deepseek.com/anthropic", token_field: DEFAULT_TOKEN_FIELD, category: "cn_official", kind: "key" },
    Preset { id: "zhipu-glm", name: "智谱 GLM", base_url: "https://open.bigmodel.cn/api/anthropic", token_field: DEFAULT_TOKEN_FIELD, category: "cn_official", kind: "key" },
    Preset { id: "zhipu-glm-en", name: "智谱 GLM 国际", base_url: "https://api.z.ai/api/anthropic", token_field: DEFAULT_TOKEN_FIELD, category: "cn_official", kind: "key" },
    Preset { id: "baidu-qianfan-coding-plan", name: "百度千帆 Coding", base_url: "https://qianfan.baidubce.com/anthropic/coding", token_field: DEFAULT_TOKEN_FIELD, category: "cn_official", kind: "key" },
    Preset { id: "bailian", name: "阿里百炼", base_url: "https://dashscope.aliyuncs.com/apps/anthropic", token_field: DEFAULT_TOKEN_FIELD, category: "cn_official", kind: "key" },
    Preset { id: "bailian-for-coding", name: "阿里百炼 Coding", base_url: "https://coding.dashscope.aliyuncs.com/apps/anthropic", token_field: DEFAULT_TOKEN_FIELD, category: "cn_official", kind: "key" },
    Preset { id: "kimi", name: "Kimi 月之暗面", base_url: "https://api.moonshot.cn/anthropic", token_field: DEFAULT_TOKEN_FIELD, category: "cn_official", kind: "key" },
    Preset { id: "kimi-for-coding", name: "Kimi For Coding", base_url: "https://api.kimi.com/coding/", token_field: DEFAULT_TOKEN_FIELD, category: "cn_official", kind: "key" },
    Preset { id: "stepfun", name: "StepFun 阶跃", base_url: "https://api.stepfun.com/step_plan", token_field: DEFAULT_TOKEN_FIELD, category: "cn_official", kind: "key" },
    Preset { id: "stepfun-en", name: "StepFun en", base_url: "https://api.stepfun.ai/step_plan", token_field: DEFAULT_TOKEN_FIELD, category: "cn_official", kind: "key" },
    Preset { id: "modelscope", name: "ModelScope 魔搭", base_url: "https://api-inference.modelscope.cn", token_field: DEFAULT_TOKEN_FIELD, category: "aggregator", kind: "key" },
    Preset { id: "kat-coder", name: "KAT-Coder", base_url: "https://vanchin.streamlake.ai/api/gateway/v1/endpoints/${ENDPOINT_ID}/claude-code-proxy", token_field: DEFAULT_TOKEN_FIELD, category: "cn_official", kind: "key" },
    Preset { id: "longcat", name: "LongCat", base_url: "https://api.longcat.chat/anthropic", token_field: DEFAULT_TOKEN_FIELD, category: "cn_official", kind: "key" },
    Preset { id: "minimax", name: "MiniMax", base_url: "https://api.minimaxi.com/anthropic", token_field: DEFAULT_TOKEN_FIELD, category: "cn_official", kind: "key" },
    Preset { id: "minimax-en", name: "MiniMax en", base_url: "https://api.minimax.io/anthropic", token_field: DEFAULT_TOKEN_FIELD, category: "cn_official", kind: "key" },
    Preset { id: "bailing", name: "百灵 BaiLing", base_url: "https://api.tbox.cn/api/anthropic", token_field: DEFAULT_TOKEN_FIELD, category: "cn_official", kind: "key" },
    Preset { id: "aihubmix", name: "AiHubMix", base_url: "https://aihubmix.com", token_field: API_KEY_FIELD, category: "aggregator", kind: "key" },
    Preset { id: "siliconflow", name: "SiliconFlow 硅基流动", base_url: "https://api.siliconflow.cn", token_field: DEFAULT_TOKEN_FIELD, category: "aggregator", kind: "key" },
    Preset { id: "siliconflow-en", name: "SiliconFlow en", base_url: "https://api.siliconflow.com", token_field: DEFAULT_TOKEN_FIELD, category: "aggregator", kind: "key" },
    Preset { id: "dmxapi", name: "DMXAPI", base_url: "https://www.dmxapi.cn", token_field: DEFAULT_TOKEN_FIELD, category: "aggregator", kind: "key" },
    Preset { id: "packycode", name: "PackyCode", base_url: "https://www.packyapi.com", token_field: DEFAULT_TOKEN_FIELD, category: "third_party", kind: "key" },
    Preset { id: "claudeapi", name: "ClaudeAPI", base_url: "https://gw.claudeapi.com", token_field: DEFAULT_TOKEN_FIELD, category: "aggregator", kind: "key" },
    Preset { id: "claudecn", name: "ClaudeCN", base_url: "https://claudecn.top", token_field: DEFAULT_TOKEN_FIELD, category: "third_party", kind: "key" },
    Preset { id: "runapi", name: "RunAPI", base_url: "https://runapi.co", token_field: DEFAULT_TOKEN_FIELD, category: "aggregator", kind: "key" },
    Preset { id: "relaxycode", name: "RelaxyCode", base_url: "https://www.relaxycode.com", token_field: DEFAULT_TOKEN_FIELD, category: "third_party", kind: "key" },
    Preset { id: "cubence", name: "Cubence", base_url: "https://api.cubence.com", token_field: DEFAULT_TOKEN_FIELD, category: "third_party", kind: "key" },
    Preset { id: "aigocode", name: "AIGoCode", base_url: "https://api.aigocode.com", token_field: DEFAULT_TOKEN_FIELD, category: "third_party", kind: "key" },
    Preset { id: "rightcode", name: "RightCode", base_url: "https://www.right.codes/claude", token_field: DEFAULT_TOKEN_FIELD, category: "third_party", kind: "key" },
    Preset { id: "aicodemirror", name: "AICodeMirror", base_url: "https://api.aicodemirror.com/api/claudecode", token_field: DEFAULT_TOKEN_FIELD, category: "third_party", kind: "key" },
    Preset { id: "crazyrouter", name: "CrazyRouter", base_url: "https://cn.crazyrouter.com", token_field: DEFAULT_TOKEN_FIELD, category: "third_party", kind: "key" },
    Preset { id: "sssaicode", name: "SSSAiCode", base_url: "https://node-hk.sssaicode.com/api", token_field: DEFAULT_TOKEN_FIELD, category: "third_party", kind: "key" },
    Preset { id: "compshare", name: "优云智算", base_url: "https://api.modelverse.cn", token_field: DEFAULT_TOKEN_FIELD, category: "aggregator", kind: "key" },
    Preset { id: "compshare-coding-plan", name: "优云智算 Coding", base_url: "https://cp.compshare.cn", token_field: DEFAULT_TOKEN_FIELD, category: "aggregator", kind: "key" },
    Preset { id: "micu", name: "Micu", base_url: "https://www.micuapi.ai", token_field: DEFAULT_TOKEN_FIELD, category: "third_party", kind: "key" },
    Preset { id: "ctok-ai", name: "CTok.ai", base_url: "https://api.ctok.ai", token_field: DEFAULT_TOKEN_FIELD, category: "third_party", kind: "key" },
    Preset { id: "e-flowcode", name: "E-FlowCode", base_url: "https://e-flowcode.cc", token_field: DEFAULT_TOKEN_FIELD, category: "third_party", kind: "key" },
    Preset { id: "openrouter", name: "OpenRouter", base_url: "https://openrouter.ai/api", token_field: DEFAULT_TOKEN_FIELD, category: "aggregator", kind: "key" },
    Preset { id: "therouter", name: "TheRouter", base_url: "https://api.therouter.ai", token_field: DEFAULT_TOKEN_FIELD, category: "aggregator", kind: "key" },
    Preset { id: "novita-ai", name: "Novita AI", base_url: "https://api.novita.ai/anthropic", token_field: DEFAULT_TOKEN_FIELD, category: "aggregator", kind: "key" },
    Preset { id: "github-copilot", name: "GitHub Copilot", base_url: "https://api.githubcopilot.com", token_field: DEFAULT_TOKEN_FIELD, category: "third_party", kind: "copilot" },
    Preset { id: "codex", name: "Codex (ChatGPT)", base_url: "https://chatgpt.com/backend-api/codex", token_field: DEFAULT_TOKEN_FIELD, category: "third_party", kind: "codex" },
    Preset { id: "lemondata", name: "LemonData", base_url: "https://api.lemondata.cc", token_field: API_KEY_FIELD, category: "third_party", kind: "key" },
    Preset { id: "nvidia", name: "Nvidia", base_url: "https://integrate.api.nvidia.com", token_field: DEFAULT_TOKEN_FIELD, category: "aggregator", kind: "key" },
    Preset { id: "pipellm", name: "PIPELLM", base_url: "https://cc-api.pipellm.ai", token_field: DEFAULT_TOKEN_FIELD, category: "aggregator", kind: "key" },
    Preset { id: "xiaomi-mimo", name: "小米 MiMo", base_url: "https://api.xiaomimimo.com/anthropic", token_field: DEFAULT_TOKEN_FIELD, category: "cn_official", kind: "key" },
    Preset { id: "xiaomi-mimo-token-plan-china", name: "小米 MiMo Token Plan", base_url: "https://token-plan-cn.xiaomimimo.com/anthropic", token_field: DEFAULT_TOKEN_FIELD, category: "cn_official", kind: "key" },
    Preset { id: "aws-bedrock-aksk", name: "AWS Bedrock (AKSK)", base_url: "https://bedrock-runtime.${AWS_REGION}.amazonaws.com", token_field: DEFAULT_TOKEN_FIELD, category: "cloud_provider", kind: "key" },
    Preset { id: "aws-bedrock-api-key", name: "AWS Bedrock (API Key)", base_url: "https://bedrock-runtime.${AWS_REGION}.amazonaws.com", token_field: DEFAULT_TOKEN_FIELD, category: "cloud_provider", kind: "key" },
];

fn preset_by_id(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// 分类 → 状态点颜色 (统一色板, 比 50 个随机色更显高级感)
fn color_for(category: &str) -> &'static str {
    match category {
        "official" => "#D97757",
        "cn_official" => "#2c6fff",
        "aggregator" => "#7c5cff",
        "third_party" => "#e8833a",
        "cloud_provider" => "#ff9900",
        _ => "#2c4661",
    }
}

fn website_from_base(base: &str) -> String {
    let b = base.trim();
    if b.is_empty() {
        return String::new();
    }
    // 取 scheme://host 作为官网链接 (去掉路径与 ${占位})
    if let Some(rest) = b.strip_prefix("https://").or_else(|| b.strip_prefix("http://")) {
        let host = rest.split('/').next().unwrap_or(rest);
        if host.contains('$') {
            return String::new();
        }
        return format!("https://{host}");
    }
    String::new()
}

// ───────────────────────── 持久化 store ─────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoredProvider {
    id: String,
    name: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    website_url: String,
    #[serde(default)]
    token_field: String,
    #[serde(default)]
    settings_config: Value,
}

// 旧版结构 (上一轮), 仅用于一次性迁移
#[derive(Debug, Clone, Default, Deserialize)]
struct LegacyCustom {
    id: String,
    name: String,
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    auth_token: String,
    #[serde(default)]
    token_field: String,
}
#[derive(Debug, Clone, Default, Deserialize)]
struct LegacyKey {
    #[serde(default)]
    auth_token: String,
    #[serde(default)]
    token_field: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    current_id: String,
    /// true = 联动(切换写 ~/.claude/settings.json, 终端 CLI 跟着变);
    /// false(默认) = 隔离(只作用于 Polaris 自己 spawn 的 claude, 走进程 env)。
    /// 老 store 没有此字段 → serde 默认 false → 升级即自动隔离, 串台就此止住。
    #[serde(default)]
    link_global: bool,
    #[serde(default)]
    items: Vec<StoredProvider>,
    // legacy（迁移后清空, 不再写出）
    #[serde(default, skip_serializing)]
    custom: Vec<LegacyCustom>,
    #[serde(default, skip_serializing)]
    keys: HashMap<String, LegacyKey>,
}

static STORE: Lazy<RwLock<Store>> = Lazy::new(|| RwLock::new(Store::default()));
static STORE_PATH: Lazy<RwLock<PathBuf>> = Lazy::new(|| RwLock::new(PathBuf::new()));
/// 串行化对 settings.json / providers.json 的「读-改-写」。
/// Tauri 命令可并发跑在线程池上, 两个 provider_switch 同时进来若不串行化, 会交错写同一份
/// settings.json → 撕裂成半截。此锁保证整条 RMW 原子, 与 atomic_write 一起根治配置损坏。
static IO_LOCK: Lazy<parking_lot::Mutex<()>> = Lazy::new(|| parking_lot::Mutex::new(()));

/// 还原构建期注入的「粉丝福利」MiniMax key。
/// 二进制内为 XOR 混淆字节, 此处解出明文; 未注入(本地 dev 构建)时返回空串。
/// 提醒: 客户端解密逻辑随包一起分发, 混淆只是延缓提取, 不构成真正保护。
fn gift_minimax_key() -> String {
    if GIFT_MINIMAX_OBF.is_empty() || GIFT_MINIMAX_PAD.is_empty() {
        return String::new();
    }
    let bytes: Vec<u8> = GIFT_MINIMAX_OBF
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ GIFT_MINIMAX_PAD[i % GIFT_MINIMAX_PAD.len()])
        .collect();
    String::from_utf8(bytes).unwrap_or_default()
}

/// 还原源码内置的「免费额度赠送」Kimi For Coding token(XOR 混淆, 见 build.rs)。
/// 与 MiniMax 不同, 此 key 默认随源码内置 → 任何构建(含本地 dev)都非空, 开箱即用。
fn gift_kimi_key() -> String {
    if GIFT_KIMI_OBF.is_empty() || GIFT_KIMI_PAD.is_empty() {
        return String::new();
    }
    let bytes: Vec<u8> = GIFT_KIMI_OBF
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ GIFT_KIMI_PAD[i % GIFT_KIMI_PAD.len()])
        .collect();
    String::from_utf8(bytes).unwrap_or_default()
}

/// 首启一次性把「粉丝福利」MiniMax 供应商(含构建期注入的 key)种进 store。
/// 用 marker(`<data>/.gift_minimax_seeded`)记录, 之后即便用户在坞里删除/改空,
/// 重启也 **不会** 再种 —— 尊重用户的删除(沿用资料库播种的语义)。
/// 未注入 key(dev 构建)时直接跳过。返回是否新种了内容。
fn seed_gift_minimax(store: &mut Store, data_dir: &Path) -> bool {
    let key = gift_minimax_key();
    if key.is_empty() {
        return false;
    }
    let marker = data_dir.join(".gift_minimax_seeded");
    if marker.exists() {
        return false;
    }
    // 不管后面有没有真种进去, 都打 marker, 避免每次启动重试 + 尊重删除。
    let _ = fs::write(&marker, b"seeded\n");

    // 用户已自配同 id 供应商则不覆盖。
    if store.items.iter().any(|i| i.id == "minimax") {
        return false;
    }
    // 钉 MiniMax-M3(官方 Claude Code 文档推荐):四档全设成 M3, 后台小任务也走 M3,
    // 不再回落 Claude 默认 haiku 名被网关当未知模型。
    let cfg = config_with_model(
        default_config("https://api.minimaxi.com/anthropic", DEFAULT_TOKEN_FIELD, &key),
        "MiniMax-M3",
    );
    store.items.push(StoredProvider {
        id: "minimax".to_string(),
        name: "MiniMax".to_string(),
        note: "粉丝福利 · 预置额度，开箱即用 · MiniMax-M3".to_string(),
        website_url: "https://www.minimaxi.com".to_string(),
        token_field: DEFAULT_TOKEN_FIELD.to_string(),
        settings_config: cfg,
    });
    true
}

/// 首启一次性把「免费额度赠送」Kimi For Coding(含源码内置 token)种进 store。
/// 语义与 [`seed_gift_minimax`] 一致:marker(`<data>/.gift_kimi_seeded`)记一次,
/// 用户事后删除/改空也不再回种 —— 尊重删除。dev 构建未注入 key 时直接跳过。
fn seed_gift_kimi(store: &mut Store, data_dir: &Path) -> bool {
    let key = gift_kimi_key();
    if key.is_empty() {
        return false;
    }
    let marker = data_dir.join(".gift_kimi_seeded");
    if marker.exists() {
        return false;
    }
    let _ = fs::write(&marker, b"seeded\n");

    // 用户已自配 kimi-for-coding 则不覆盖。
    if store.items.iter().any(|i| i.id == "kimi-for-coding") {
        return false;
    }
    // 钉 kimi-for-coding(K2.7 Code):四档全设成同一 model id, 后台小任务也走它,
    // 不回落 Claude 默认 haiku 名被网关当未知模型(与 MiniMax 同理)。
    let cfg = config_with_model(
        default_config("https://api.kimi.com/coding/", DEFAULT_TOKEN_FIELD, &key),
        "kimi-for-coding",
    );
    store.items.push(StoredProvider {
        id: "kimi-for-coding".to_string(),
        name: "Kimi For Coding".to_string(),
        note: "免费额度赠送 · 开箱即用 · K2.7 Code".to_string(),
        website_url: "https://www.kimi.com/code".to_string(),
        token_field: DEFAULT_TOKEN_FIELD.to_string(),
        settings_config: cfg,
    });
    true
}

/// 往 settings_config 的 env 里钉模型:把 MODEL_ENV_KEYS 四档全设成同一个 model id。
/// model 为空则原样返回(不钉)。
fn config_with_model(mut cfg: Value, model: &str) -> Value {
    let model = model.trim();
    if model.is_empty() {
        return cfg;
    }
    if !cfg.is_object() {
        cfg = json!({});
    }
    let obj = cfg.as_object_mut().unwrap();
    let env = obj.entry("env".to_string()).or_insert_with(|| json!({}));
    if !env.is_object() {
        *env = json!({});
    }
    let env = env.as_object_mut().unwrap();
    for k in MODEL_ENV_KEYS {
        env.insert((*k).to_string(), Value::String(model.to_string()));
    }
    cfg
}

/// Codex 路由配置: 把 base_url 指到本地翻译代理, 钉模型为 gpt-5.5(含小任务档),
/// AUTH_TOKEN 给个占位串(代理只认 ~/.codex/auth.json, 不看这个), 让 claude 愿意发请求。
fn codex_route_config(port: u16) -> Value {
    let mut env = Map::new();
    env.insert(
        "ANTHROPIC_BASE_URL".into(),
        Value::String(format!("http://127.0.0.1:{port}")),
    );
    env.insert(
        "ANTHROPIC_AUTH_TOKEN".into(),
        Value::String("polaris-codex-proxy".into()),
    );
    for k in MODEL_ENV_KEYS {
        env.insert((*k).into(), Value::String("gpt-5.5".into()));
    }
    env.insert(
        "ANTHROPIC_SMALL_FAST_MODEL".into(),
        Value::String("gpt-5.5".into()),
    );
    json!({ "env": Value::Object(env) })
}

pub fn init(_app: &AppHandle) -> Result<()> {
    let user = UserDirs::new().ok_or_else(|| anyhow::anyhow!("no user dir"))?;
    let dir = user.home_dir().join("Polaris").join("data");
    fs::create_dir_all(&dir)?;
    let path = dir.join("providers.json");
    *STORE_PATH.write() = path.clone();

    let mut store: Store = if path.exists() {
        let txt = fs::read_to_string(&path).unwrap_or_default();
        match serde_json::from_str(&txt) {
            Ok(s) => s,
            Err(_) => {
                // 解析失败别静默 default —— 那会让用户所有已存供应商/API key 凭空消失。
                // 先把损坏文件留底, 用户仍可手工抢救, 再回落空 store。
                if !txt.trim().is_empty() {
                    let mut bak = path.as_os_str().to_owned();
                    bak.push(".corrupt.bak");
                    let _ = fs::copy(&path, PathBuf::from(bak));
                }
                Store::default()
            }
        }
    } else {
        Store::default()
    };

    // 一次性迁移旧 custom / keys → items
    let mut migrated = false;
    let legacy_custom = std::mem::take(&mut store.custom);
    let legacy_keys = std::mem::take(&mut store.keys);
    for c in legacy_custom {
        if store.items.iter().any(|i| i.id == c.id) {
            continue;
        }
        let field = if c.token_field.is_empty() {
            DEFAULT_TOKEN_FIELD.to_string()
        } else {
            c.token_field.clone()
        };
        store.items.push(StoredProvider {
            id: c.id,
            name: c.name,
            note: String::new(),
            website_url: String::new(),
            token_field: field.clone(),
            settings_config: default_config(&c.base_url, &field, &c.auth_token),
        });
        migrated = true;
    }
    for (pid, k) in legacy_keys {
        if store.items.iter().any(|i| i.id == pid) {
            continue;
        }
        if let Some(p) = preset_by_id(&pid) {
            let field = if k.token_field.is_empty() {
                p.token_field.to_string()
            } else {
                k.token_field.clone()
            };
            store.items.push(StoredProvider {
                id: pid.clone(),
                name: p.name.to_string(),
                note: String::new(),
                website_url: String::new(),
                token_field: field.clone(),
                settings_config: default_config(p.base_url, &field, &k.auth_token),
            });
            migrated = true;
        }
    }

    // 首启一次性种「粉丝福利」MiniMax + 「免费额度赠送」Kimi For Coding(含内置 key)。
    let gifted_minimax = seed_gift_minimax(&mut store, &dir);
    let gifted_kimi = seed_gift_kimi(&mut store, &dir);

    *STORE.write() = store;
    if migrated || gifted_minimax || gifted_kimi {
        persist();
    }

    {
        let store = STORE.read().clone();
        let views = build_views(&store);
        if store.link_global {
            // 联动: 若上次退出时正路由到 Codex(本地代理), 重启后端口可能变 → 重新拉起
            // 代理并校正 ANTHROPIC_BASE_URL, 否则 settings.json 残留旧端口 claude 连不上。
            if detect_current(&views, &store) == "codex" {
                if let Ok(port) = crate::codex_proxy::ensure_running() {
                    let cfg = codex_route_config(port);
                    let _ = apply_settings_config(&cfg);
                    apply_process_env(&cfg);
                }
            }
        } else {
            // 隔离(默认):
            // ① 净化 —— 联动时代(或旧版本)写进全局 settings.json 的受管键还躺在那里
            //    污染外部 CLI。旧规则只认「全局 base_url == 当前供应商」, 用户切回官方后
            //    current 的 base 变空, 残留永远匹配不上、永远清不掉(实测就是「切一次
            //    MiniMax 就再也切不回来」)。证据判定改走 global_env_is_ours 四级证据。
            purge_global_residue(&views, &store);
            // ② 重启后进程 env 是空的, 把当前供应商重新作用上去, 否则 Polaris 内的
            //    选择一重启就回落官方。配置不全(如 key 被删)就静默跳过 = 官方。
            if let Some(v) = views.iter().find(|v| v.id == store.current_id) {
                if let Ok(cfg) = cfg_for_view(v) {
                    apply_process_env(&cfg);
                }
            }
        }
    }
    Ok(())
}

/// 原子落盘: 先写同目录临时文件, 再 rename 覆盖目标。
/// rename 在同一文件系统内是原子的 (Windows 的 `fs::rename` 用 MoveFileExW+REPLACE_EXISTING),
/// 故进程在写一半时崩溃/断电只会留下 `.polaris.tmp`, 目标文件要么旧内容要么新内容,
/// 绝不会被截成半截 JSON —— 这是「torn write 破坏 claude 配置 / 静默清空 API key」的根治。
fn atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".polaris.tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

fn persist() {
    let path = STORE_PATH.read().clone();
    if path.as_os_str().is_empty() {
        return;
    }
    let _io = IO_LOCK.lock();
    if let Ok(txt) = serde_json::to_string_pretty(&*STORE.read()) {
        let _ = atomic_write(&path, &txt);
    }
}

/// 用 base_url + token 构造最小 settings_config
fn default_config(base: &str, token_field: &str, token: &str) -> Value {
    let mut env = Map::new();
    let base = base.trim();
    if !base.is_empty() {
        env.insert("ANTHROPIC_BASE_URL".into(), Value::String(base.into()));
    }
    let token = token.trim();
    if !token.is_empty() {
        let field = if token_field.is_empty() {
            DEFAULT_TOKEN_FIELD
        } else {
            token_field
        };
        env.insert(field.into(), Value::String(token.into()));
    }
    json!({ "env": Value::Object(env) })
}

fn cfg_env_str(cfg: &Value, key: &str) -> String {
    cfg.get("env")
        .and_then(|e| e.get(key))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

// ───────────────────────── 视图模型 ─────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderView {
    pub id: String,
    pub name: String,
    pub note: String,
    pub base_url: String,
    pub token_field: String,
    pub category: String,
    pub website_url: String,
    pub color: String,
    pub kind: String,
    pub is_preset: bool,
    pub has_key: bool,
    pub auth_token: String,
    pub settings_config: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderListResult {
    pub providers: Vec<ProviderView>,
    pub current_id: String,
    /// true = 联动(写全局 settings.json), false = 隔离(仅 Polaris 内生效)
    pub link_global: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInput {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub website_url: String,
    #[serde(default)]
    pub token_field: Option<String>,
    #[serde(default)]
    pub settings_config: Value,
}

fn normalize_url(u: &str) -> String {
    u.trim().trim_end_matches('/').to_string()
}

fn make_view(
    id: &str,
    name: &str,
    note: &str,
    token_field: &str,
    category: &str,
    kind: &str,
    is_preset: bool,
    preset_base: &str,
    website: &str,
    cfg: Value,
) -> ProviderView {
    let env_base = cfg_env_str(&cfg, "ANTHROPIC_BASE_URL");
    let base_url = if env_base.is_empty() {
        preset_base.to_string()
    } else {
        env_base
    };
    let token = cfg_env_str(&cfg, token_field);
    let has_key = match kind {
        "official" => true,
        "codex" | "copilot" => false,
        _ => !token.is_empty(),
    };
    let website = if website.is_empty() {
        website_from_base(&base_url)
    } else {
        website.to_string()
    };
    ProviderView {
        id: id.to_string(),
        name: name.to_string(),
        note: note.to_string(),
        base_url,
        token_field: token_field.to_string(),
        category: category.to_string(),
        website_url: website,
        color: color_for(category).to_string(),
        kind: kind.to_string(),
        is_preset,
        has_key,
        auth_token: token,
        settings_config: cfg,
    }
}

fn build_views(store: &Store) -> Vec<ProviderView> {
    let mut out: Vec<ProviderView> = Vec::with_capacity(PRESETS.len() + store.items.len());

    for p in PRESETS {
        let stored = store.items.iter().find(|i| i.id == p.id);
        let token_field = stored
            .map(|s| s.token_field.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| p.token_field.to_string());
        let cfg = stored
            .map(|s| s.settings_config.clone())
            .unwrap_or_else(|| default_config(p.base_url, &token_field, ""));
        let note = stored.map(|s| s.note.as_str()).unwrap_or("");
        out.push(make_view(
            p.id, p.name, note, &token_field, p.category, p.kind, true, p.base_url, "", cfg,
        ));
    }

    for it in &store.items {
        if preset_by_id(&it.id).is_some() {
            continue; // 预设覆盖已在上方合并
        }
        let token_field = if it.token_field.is_empty() {
            DEFAULT_TOKEN_FIELD.to_string()
        } else {
            it.token_field.clone()
        };
        out.push(make_view(
            &it.id,
            &it.name,
            &it.note,
            &token_field,
            "custom",
            "custom",
            false,
            "",
            &it.website_url,
            it.settings_config.clone(),
        ));
    }

    out
}

fn detect_current(views: &[ProviderView], store: &Store) -> String {
    // 联动: 真相在全局 settings.json(外部 cc-switch 等改动也能被察觉);
    // 隔离: 全局与我们无关, 真相在本进程 env(apply_process_env 设的)。
    let live_base = if store.link_global {
        read_live_env()
            .get("ANTHROPIC_BASE_URL")
            .and_then(|v| v.as_str())
            .map(normalize_url)
            .unwrap_or_default()
    } else {
        std::env::var("ANTHROPIC_BASE_URL")
            .map(|s| normalize_url(&s))
            .unwrap_or_default()
    };

    if live_base.is_empty() {
        if store.current_id == "claude-official" || store.current_id.is_empty() {
            return "claude-official".to_string();
        }
        if let Some(v) = views.iter().find(|v| v.id == store.current_id) {
            if normalize_url(&v.base_url).is_empty() {
                return v.id.clone();
            }
        }
        return "claude-official".to_string();
    }

    if let Some(v) = views
        .iter()
        .find(|v| !v.base_url.is_empty() && normalize_url(&v.base_url) == live_base)
    {
        return v.id.clone();
    }
    if !store.current_id.is_empty() && views.iter().any(|v| v.id == store.current_id) {
        return store.current_id.clone();
    }
    String::new()
}

// ───────────────────────── ~/.claude/settings.json 读写 ─────────────────────────

fn claude_dir() -> Option<PathBuf> {
    UserDirs::new().map(|u| u.home_dir().join(".claude"))
}
fn claude_settings_path() -> Option<PathBuf> {
    claude_dir().map(|d| d.join("settings.json"))
}

fn read_live_env() -> Map<String, Value> {
    let Some(path) = claude_settings_path() else {
        return Map::new();
    };
    let Ok(txt) = fs::read_to_string(&path) else {
        return Map::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&txt) else {
        return Map::new();
    };
    v.get("env").and_then(|e| e.as_object()).cloned().unwrap_or_default()
}

/// 隔离模式下判定「全局 settings.json 的受管 env 是不是我们(联动时代/旧版本)写的」。
/// 证据从强到弱:
/// ① POLARIS_LINKED 戳(新版联动写全局时盖的);
/// ② base_url 命中当前供应商(旧规则, 保留兼容);
/// ③ base_url 命中任一已知供应商 **且** token 与我们存的该家 key 一致 —— 用户已切回
///    官方、残留还指着上一家时, ①②全失效, 残留永远清不掉(实测踩坑「切一次就切不
///    回来」), 全靠这条兜底; key 不同则视为用户自己在终端配的, 不动。
/// ④ base 已清但模型钉选残留(等于我们某家钉的模型) —— 官方端点带着 MiniMax-M3
///    这种钉选必然 4xx, 属我们的残留, 清。
fn global_env_is_ours(live: &Map<String, Value>, views: &[ProviderView], store: &Store) -> bool {
    if live.contains_key("POLARIS_LINKED") {
        return true;
    }
    let live_str = |k: &str| {
        live.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let live_base = normalize_url(&live_str("ANTHROPIC_BASE_URL"));
    if !live_base.is_empty() {
        if let Some(cur) = views.iter().find(|v| v.id == store.current_id) {
            if normalize_url(&cur.base_url) == live_base {
                return true;
            }
        }
        return views.iter().any(|v| {
            !v.auth_token.trim().is_empty()
                && !v.base_url.is_empty()
                && normalize_url(&v.base_url) == live_base
                && [DEFAULT_TOKEN_FIELD, API_KEY_FIELD]
                    .iter()
                    .any(|f| live_str(f) == v.auth_token.trim())
        });
    }
    MODEL_ENV_KEYS.iter().any(|k| {
        let m = live_str(k);
        !m.is_empty()
            && views
                .iter()
                .any(|v| cfg_env_str(&v.settings_config, k).trim() == m)
    })
}

/// 隔离模式的残留体检: 全局 settings.json 里若还躺着我们写的受管键, 清回官方。
/// init 启动时和每次切换都跑一遍 —— 无证据时只读不写, 幂等且零成本。
fn purge_global_residue(views: &[ProviderView], store: &Store) {
    let live = read_live_env();
    if !live.is_empty() && global_env_is_ours(&live, views, store) {
        let _ = apply_settings_config(&json!({ "env": {} }));
    }
}

/// 把供应商 settings_config 合并写进 live settings.json：
/// 先从 live 清掉受管 env/top 键，再套用 cfg 的 env 与顶层键，其余 live 键原样保留。
fn apply_settings_config(cfg: &Value) -> Result<(), String> {
    // 整条「读 settings.json → 合并 → 写回」串行化, 防并发切换交错撕裂用户实时配置。
    let _io = IO_LOCK.lock();
    let path = claude_settings_path().ok_or_else(|| "无法定位用户主目录".to_string())?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let mut root: Value = if path.exists() {
        let txt = fs::read_to_string(&path).map_err(|e| format!("读 settings.json 失败: {e}"))?;
        if txt.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&txt)
                .map_err(|e| format!("settings.json 不是合法 JSON, 已中止以免破坏: {e}"))?
        }
    } else {
        json!({})
    };

    if path.exists() {
        let bak = path.with_extension("json.polaris.bak");
        if !bak.exists() {
            let _ = fs::copy(&path, &bak);
        }
    }

    if !root.is_object() {
        root = json!({});
    }
    let obj = root.as_object_mut().unwrap();

    // 清受管顶层键
    for k in MANAGED_TOP_KEYS {
        obj.remove(*k);
    }
    // env: 清受管键后套用 cfg.env
    let env = obj
        .entry("env".to_string())
        .or_insert_with(|| json!({}));
    if !env.is_object() {
        *env = json!({});
    }
    let env = env.as_object_mut().unwrap();
    for k in MANAGED_ENV_KEYS {
        env.remove(*k);
    }
    if let Some(src_env) = cfg.get("env").and_then(|e| e.as_object()) {
        for (k, v) in src_env {
            env.insert(k.clone(), v.clone());
        }
    }
    // 真正路由了全局(env 带 base_url)就盖 POLARIS_LINKED 戳: 日后隔离模式的净化
    // 凭这个戳就能确认残留是我们写的, 不再依赖「当前供应商恰好没换」这种弱证据。
    let routed = env
        .get("ANTHROPIC_BASE_URL")
        .and_then(|v| v.as_str())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if routed {
        env.insert("POLARIS_LINKED".into(), Value::String("1".into()));
    }
    // 顶层键 (除 env) 套用
    if let Some(src) = cfg.as_object() {
        for (k, v) in src {
            if k == "env" {
                continue;
            }
            obj.insert(k.clone(), v.clone());
        }
    }

    let txt = serde_json::to_string_pretty(&root)
        .map_err(|e| format!("序列化 settings.json 失败: {e}"))?;
    atomic_write(&path, &txt).map_err(|e| format!("写 settings.json 失败: {e}"))?;
    Ok(())
}

/// 给「生图」用的当前供应商画像：返回 (当前供应商展示名, 是否疑似具备真实生图能力)。
///
/// 真相：供应商坞里 55 家全部是 Anthropic 协议的文本 / 代码大模型，**没有一个能生图**；
/// 真要生图得另配一份独立的图像 API（如 OpenAI gpt-image）。所以默认「不支持」，
/// 仅当 settings.json 的 env 或进程环境里检测到 `OPENAI_API_KEY` 时才认为可尝试真实生图。
pub fn image_gen_capability() -> (String, bool) {
    let store = STORE.read().clone();
    let views = build_views(&store);
    let cur = detect_current(&views, &store);
    let name = views
        .iter()
        .find(|v| v.id == cur)
        .map(|v| v.name.clone())
        .unwrap_or_else(|| "Claude 官方".to_string());

    let live = read_live_env();
    let has_image_key = live
        .get("OPENAI_API_KEY")
        .and_then(|v| v.as_str())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
        || std::env::var("OPENAI_API_KEY")
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);

    (name, has_image_key)
}

// ───────────────────────── Commands: 供应商 ─────────────────────────

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn provider_list() -> Result<ProviderListResult, String> {
    let store = STORE.read().clone();
    let providers = build_views(&store);
    let current_id = detect_current(&providers, &store);
    Ok(ProviderListResult {
        providers,
        current_id,
        link_global: store.link_global,
    })
}

/// 把供应商视图换算成待生效的 settings_config(codex 会顺带拉起本地翻译代理)。
fn cfg_for_view(v: &ProviderView) -> Result<Value, String> {
    if v.kind == "copilot" {
        return Err("GitHub Copilot 说 OpenAI 协议, 翻译代理暂未覆盖".to_string());
    }
    if v.kind == "codex" {
        // Codex(ChatGPT) → 路由到本地翻译代理: 先确认已授权, 再拉起代理并把
        // ANTHROPIC_BASE_URL 指到 127.0.0.1:port, claude 即透明用上 ChatGPT 订阅。
        let authed = codex_auth_path()
            .map(|p| codex_auth_has_tokens(&p))
            .unwrap_or(false);
        if !authed {
            return Err("请先授权 ChatGPT (Codex), 再切换到它".to_string());
        }
        let port = crate::codex_proxy::ensure_running()?;
        return Ok(codex_route_config(port));
    }
    if v.kind == "official" {
        return Ok(json!({ "env": {} }));
    }
    if v.auth_token.trim().is_empty() {
        return Err("该供应商尚未配置 API Key, 请先在弹窗中填写".to_string());
    }
    Ok(v.settings_config.clone())
}

/// 同步当前进程 env: spawn 出去的 claude 子进程会继承父进程 env, 而进程 env 通常**优先于**
/// settings.json 的 env(实测), 不先清后置就会出现:
///   ① 切到 M3 → 进程被 set 了 ANTHROPIC_BASE_URL=minimaxi
///   ② 切回官方 → 只清了 settings, 父进程残留仍把 claude 拖到 minimaxi → 一直只能用 M3
/// 先按 MANAGED_ENV_KEYS 全清, 再把新 cfg.env 写进当前进程 —— 切换结果确定。
/// 隔离模式下这就是切换的**唯一**生效通道。
fn apply_process_env(cfg: &Value) {
    for k in MANAGED_ENV_KEYS {
        std::env::remove_var(k);
    }
    if let Some(src_env) = cfg.get("env").and_then(|e| e.as_object()) {
        for (k, val) in src_env {
            if let Some(s) = val.as_str() {
                std::env::set_var(k, s);
            }
        }
    }
}

// ───────────────── 隔离模式·私有 claude 配置目录(会话账本隔离) ─────────────────
//
// 配置隔离(进程 env)只解决「外部 CLI 用哪家」; 但 claude 的会话 jsonl 仍写进共享
// `~/.claude/projects/`, cc-switch 这类监控按分钟扫那里记账 → 监控里永远有
// Polaris 自动任务的 MiniMax 行。深隔离: 隔离模式下跑**非官方**供应商时, 给子进程
// CLAUDE_CONFIG_DIR=~/Polaris/claude-home, 会话记录/customApiKeyResponses 全落私有
// 目录, 共享账本只剩用户本人的会话。官方档仍用共享 ~/.claude —— OAuth 凭证在那,
// 且官方会话本来就该记在用户自己的账上。

/// 隔离模式下第三方/Codex 任务的私有 claude 配置目录。
pub fn private_claude_home() -> Option<PathBuf> {
    UserDirs::new().map(|u| u.home_dir().join("Polaris").join("claude-home"))
}

/// 所有已存供应商 key 的「尾 20 字符」—— claude 在 .claude.json 的
/// customApiKeyResponses.approved 里就是按这个尾巴记录「该 key 已被用户批准」。
/// 预先播种进私有目录, headless 首启不会因 key 审批交互被卡死。
fn provider_key_tails(store: &Store) -> Vec<String> {
    let mut tails: Vec<String> = Vec::new();
    for it in &store.items {
        for field in [DEFAULT_TOKEN_FIELD, API_KEY_FIELD] {
            let tok = cfg_env_str(&it.settings_config, field);
            let tok = tok.trim();
            if tok.is_empty() {
                continue;
            }
            let chars: Vec<char> = tok.chars().collect();
            let tail: String = chars[chars.len().saturating_sub(20)..].iter().collect();
            if !tails.contains(&tail) {
                tails.push(tail);
            }
        }
    }
    tails
}

/// 创建并播种私有配置目录(幂等, 内容没变就不落盘):
/// .claude.json 里 hasCompletedOnboarding=true + 全部供应商 key 尾巴进 approved。
fn ensure_private_home(home: &Path, store: &Store) -> Result<(), String> {
    fs::create_dir_all(home).map_err(|e| format!("创建私有 claude 目录失败: {e}"))?;
    let path = home.join(".claude.json");
    let mut root: Value = if path.exists() {
        let txt = fs::read_to_string(&path).unwrap_or_default();
        serde_json::from_str(&txt).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };
    if !root.is_object() {
        root = json!({});
    }
    let obj = root.as_object_mut().unwrap();
    let mut changed = false;

    if obj.get("hasCompletedOnboarding").and_then(|v| v.as_bool()) != Some(true) {
        obj.insert("hasCompletedOnboarding".into(), Value::Bool(true));
        changed = true;
    }

    let resp = obj
        .entry("customApiKeyResponses".to_string())
        .or_insert_with(|| json!({ "approved": [], "rejected": [] }));
    if !resp.is_object() {
        *resp = json!({ "approved": [], "rejected": [] });
        changed = true;
    }
    let resp = resp.as_object_mut().unwrap();
    let approved = resp
        .entry("approved".to_string())
        .or_insert_with(|| json!([]));
    if !approved.is_array() {
        *approved = json!([]);
        changed = true;
    }
    let arr = approved.as_array_mut().unwrap();
    for tail in provider_key_tails(store) {
        if !arr.iter().any(|v| v.as_str() == Some(tail.as_str())) {
            arr.push(Value::String(tail));
            changed = true;
        }
    }

    if changed {
        let txt = serde_json::to_string_pretty(&root)
            .map_err(|e| format!("序列化私有 .claude.json 失败: {e}"))?;
        atomic_write(&path, &txt).map_err(|e| format!("写私有 .claude.json 失败: {e}"))?;
    }
    Ok(())
}

/// 给宿主机 spawn 的 claude 子进程套供应商作用域。chat.rs / kb.rs 所有宿主 spawn
/// 点统一调这一个入口; 不满足深隔离条件(联动 / 官方档 / env 没有 base_url)时什么都不做,
/// 子进程照旧用共享 ~/.claude。
pub fn scope_child_claude(cmd: &mut Command) {
    let store = STORE.read().clone();
    if store.link_global {
        return;
    }
    if store.current_id.is_empty() || store.current_id == "claude-official" {
        return;
    }
    // 进程 env 没有 base_url = 实际跑在官方档(如配置不全回落), 不隔离。
    if std::env::var("ANTHROPIC_BASE_URL")
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return;
    }
    let Some(home) = private_claude_home() else {
        return;
    };
    // 播种失败宁可回落共享目录, 也不让任务带着半成品配置首启卡死。
    if ensure_private_home(&home, &store).is_err() {
        return;
    }
    cmd.env("CLAUDE_CONFIG_DIR", &home);
}

/// 按对话选定的供应商 id, 把该供应商配置**逐命令**注入到这条 claude 子进程 —— 实现
/// 「每个对话各用各的 API、互不串台」的真隔离。与 `scope_child_claude`(吃全局当前)不同:
/// 这里不依赖、也不改全局进程 env, 而是直接在 `Command` 上先清掉继承来的受管键、再套上
/// **本对话这家**的 env, 因此并发的多条对话(各绑不同供应商)同时在跑也不会互相覆盖。
///
/// 语义:
/// - `None` / `""` / `"auto"` —— 「Auto」档: 不做逐命令注入, 回落到 `scope_child_claude`
///   (沿用应用全局当前供应商, 继承进程 env)。新对话默认即此, 行为与旧版完全一致。
/// - 具体 id —— 解析该供应商(codex 会顺带确保本地翻译代理在跑), 清受管键后注入它的 env;
///   非官方第三方再套私有 `CLAUDE_CONFIG_DIR`(会话账本不污染共享 ~/.claude)。
/// - 找不到该 id / 未配 key / 未授权 —— 安全回落到全局当前, 绝不让对话因配置缺失发不出去。
pub fn scope_child_claude_by_id(cmd: &mut Command, provider_id: Option<&str>) {
    let id = provider_id.map(|s| s.trim()).unwrap_or("");
    if id.is_empty() || id == "auto" {
        scope_child_claude(cmd);
        return;
    }
    let store = STORE.read().clone();
    let views = build_views(&store);
    let Some(v) = views.iter().find(|v| v.id == id) else {
        scope_child_claude(cmd);
        return;
    };
    // 解析待生效 env(codex 会确保本地代理在跑并把 base_url 指到 127.0.0.1:port)。
    // 未配 key / 未授权 → 回落全局当前, 不阻断对话。
    let cfg = match cfg_for_view(v) {
        Ok(c) => c,
        Err(_) => {
            scope_child_claude(cmd);
            return;
        }
    };
    // 逐命令注入: 先把继承自 Polaris 进程的受管键全清(否则全局那家的 base_url/token 会漏进来),
    // 再套本对话这家 —— 这条 claude 从此自带完整配置, 与全局开关、与其它并发对话彻底解耦。
    for k in MANAGED_ENV_KEYS {
        cmd.env_remove(k);
    }
    if let Some(env) = cfg.get("env").and_then(|e| e.as_object()) {
        for (k, val) in env {
            if let Some(s) = val.as_str() {
                cmd.env(k, s);
            }
        }
    }
    // 会话账本隔离: 非官方第三方 → 私有 CLAUDE_CONFIG_DIR; 官方仍用共享 ~/.claude(OAuth 凭据在那)。
    let base = cfg
        .get("env")
        .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if v.kind != "official" && !base.is_empty() {
        if let Some(home) = private_claude_home() {
            if ensure_private_home(&home, &store).is_ok() {
                cmd.env("CLAUDE_CONFIG_DIR", &home);
            }
        }
    }
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn provider_switch(id: String) -> Result<String, String> {
    let store = STORE.read().clone();
    let views = build_views(&store);
    let v = views
        .iter()
        .find(|v| v.id == id)
        .ok_or_else(|| format!("供应商不存在: {id}"))?;

    let cfg = cfg_for_view(v)?;
    // 联动才碰全局 settings.json; 隔离只走进程 env, 外部 CLI 原封不动 ——
    // 但顺手做一次残留体检: 全局若还躺着我们(旧版/联动时代)写的受管键, 先清掉,
    // 用户不用重启 Polaris 外部 CLI 就立即恢复干净。
    if store.link_global {
        apply_settings_config(&cfg)?;
    } else {
        purge_global_residue(&views, &store);
    }
    apply_process_env(&cfg);

    STORE.write().current_id = id.clone();
    persist();
    Ok(id)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn provider_set_link_mode(link: bool) -> Result<bool, String> {
    STORE.write().link_global = link;
    persist();

    let store = STORE.read().clone();
    let views = build_views(&store);
    let cur = views.iter().find(|v| v.id == store.current_id);
    if link {
        // 开联动: 当前供应商立刻写入全局, 终端 CLI 即刻跟上。
        if let Some(v) = cur {
            if let Ok(cfg) = cfg_for_view(v) {
                apply_settings_config(&cfg)?;
                apply_process_env(&cfg);
            }
        }
    } else {
        // 关联动(转隔离): 全局退回官方(只清我们的受管键, 其余原样保留),
        // Polaris 自身改用进程 env 维持当前选择 —— 终端立刻恢复干净。
        apply_settings_config(&json!({ "env": {} }))?;
        if let Some(v) = cur {
            if let Ok(cfg) = cfg_for_view(v) {
                apply_process_env(&cfg);
            }
        }
    }
    Ok(link)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn provider_save(input: ProviderInput) -> Result<String, String> {
    let token_field = input
        .token_field
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_TOKEN_FIELD.to_string());

    // settings_config 兜底为 {env:{}}
    let cfg = if input.settings_config.is_object() {
        input.settings_config.clone()
    } else {
        json!({ "env": {} })
    };

    let id = input
        .id
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("custom-{}", now_ms()));

    let item = StoredProvider {
        id: id.clone(),
        name: input.name.trim().to_string(),
        note: input.note.trim().to_string(),
        website_url: normalize_url(&input.website_url),
        token_field,
        settings_config: cfg,
    };

    let mut store = STORE.write();
    if let Some(existing) = store.items.iter_mut().find(|i| i.id == id) {
        *existing = item;
    } else {
        store.items.push(item);
    }
    drop(store);
    persist();
    Ok(id)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn provider_delete(id: String) -> Result<(), String> {
    let mut store = STORE.write();
    store.items.retain(|i| i.id != id);
    if store.current_id == id {
        store.current_id = "claude-official".to_string();
    }
    drop(store);
    persist();
    Ok(())
}

// ───────────────────────── Commands: Codex 授权 (原生 Device Code OAuth) ─────────────────────────
//
// 抄自 cc-switch 新版 `codex_oauth_auth.rs` 的 OpenAI Device Code 流程, 但**不背它的翻译代理**:
// Polaris 不路由 Codex, 拿到的 token 按官方 codex CLI 的 `~/.codex/auth.json` 格式落盘, 让外部
// `codex` CLI 直接复用。这样「点授权」彻底不依赖 codex CLI 是否已装, 后端直接拉起浏览器授权页。
//
// 三步: ① POST usercode 取 device_auth_id + user_code, 同时开浏览器到验证页;
//        ② 前端按 interval 轮询 token 端点 (403/404=等待用户授权);
//        ③ 用户授权后返回 authorization_code + code_verifier, 换 access/refresh/id_token 落盘。

/// OpenAI OAuth 客户端 ID (与官方 Codex CLI 相同)
pub(crate) const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_DEVICE_USERCODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const CODEX_DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
pub(crate) const CODEX_OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_DEVICE_VERIFY_URL: &str = "https://auth.openai.com/codex/device";
/// Device Code 流程约定的 redirect_uri (OpenAI 服务端固定)
const CODEX_DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const CODEX_USER_AGENT: &str = "polaris-codex-oauth";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexStatus {
    pub installed: bool,
    pub logged_in: bool,
    pub auth_path: String,
}

pub(crate) fn codex_auth_path() -> Option<PathBuf> {
    UserDirs::new().map(|u| u.home_dir().join(".codex").join("auth.json"))
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn codex_status() -> Result<CodexStatus, String> {
    let installed = Command::new("codex")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let auth_path = codex_auth_path();
    // 授权与否只看 ~/.codex/auth.json 是否有 ChatGPT tokens —— 与 codex CLI 是否已装解耦。
    let logged_in = auth_path
        .as_ref()
        .map(|p| codex_auth_has_tokens(p))
        .unwrap_or(false);
    Ok(CodexStatus {
        installed,
        logged_in,
        auth_path: auth_path.map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
    })
}

/// auth.json 存在且带 ChatGPT OAuth tokens (区别于纯 API key 登录)
pub(crate) fn codex_auth_has_tokens(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| {
            v.get("tokens")
                .and_then(|t| t.get("access_token"))
                .and_then(|a| a.as_str())
                .map(|s| !s.is_empty())
        })
        .unwrap_or(false)
}

/// `codex_start_login` 返回给前端的设备授权信息
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexDeviceLogin {
    /// device_auth_id, 轮询时回传
    pub device_code: String,
    /// 展示给用户的配对码
    pub user_code: String,
    /// 浏览器验证页 (已自动打开, UI 也显示便于手动打开)
    pub verification_uri: String,
    /// 建议轮询间隔 (秒)
    pub interval: u64,
    /// 设备码有效期 (秒)
    pub expires_in: u64,
}

/// `codex_poll_login` 返回: status = "pending" | "ok"
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexPollResult {
    pub status: String,
}

#[derive(Deserialize)]
struct CodexDeviceCodeResp {
    device_auth_id: String,
    user_code: String,
    #[serde(default)]
    interval: Option<Value>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct CodexDevicePollSuccess {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Deserialize)]
struct CodexTokenResp {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

/// 提取 ureq 错误里的状态码/文案, 拼成可读消息
fn codex_http_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let body = body.chars().take(300).collect::<String>();
            format!("HTTP {code} - {body}")
        }
        ureq::Error::Transport(t) => format!("网络错误: {t}"),
    }
}

/// 解析 interval 字段 (服务端可能给数字或字符串), 加 3 秒安全余量
fn codex_parse_interval(v: Option<&Value>) -> u64 {
    let raw = match v {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(5),
        Some(Value::String(s)) => s.parse::<u64>().unwrap_or(5),
        _ => 5,
    };
    raw.max(1) + 3
}

/// 带超时的 OAuth agent: 设备授权 / 轮询 / 换 token 都是非流式请求-响应, 给整条 call 30s
/// 全局 deadline, 防 OpenAI 认证端点黑洞把 Tauri 命令线程挂死 (轮询命令更会每次挂一条)。
fn codex_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .build()
}

/// ① 启动 Device Code 流程并打开浏览器验证页
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn codex_start_login() -> Result<CodexDeviceLogin, String> {
    let resp = codex_agent()
        .post(CODEX_DEVICE_USERCODE_URL)
        .set("Content-Type", "application/json")
        .set("User-Agent", CODEX_USER_AGENT)
        .send_json(json!({ "client_id": CODEX_CLIENT_ID }))
        .map_err(|e| format!("发起 ChatGPT 设备授权失败: {}", codex_http_err(e)))?;

    let device: CodexDeviceCodeResp = resp
        .into_json()
        .map_err(|e| format!("解析设备码响应失败: {e}"))?;

    let interval = codex_parse_interval(device.interval.as_ref());
    let expires_in = device.expires_in.unwrap_or(900);

    // 自动拉起浏览器到验证页 (失败不致命, UI 仍展示链接 + 配对码供手动打开)
    let _ = codex_open_browser(CODEX_DEVICE_VERIFY_URL);

    Ok(CodexDeviceLogin {
        device_code: device.device_auth_id,
        user_code: device.user_code,
        verification_uri: CODEX_DEVICE_VERIFY_URL.to_string(),
        interval,
        expires_in,
    })
}

/// ② 轮询授权状态; 成功则换 token 并落盘 ~/.codex/auth.json
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn codex_poll_login(device_code: String, user_code: String) -> Result<CodexPollResult, String> {
    let pending = || Ok(CodexPollResult { status: "pending".into() });

    let resp = match codex_agent()
        .post(CODEX_DEVICE_TOKEN_URL)
        .set("Content-Type", "application/json")
        .set("User-Agent", CODEX_USER_AGENT)
        .send_json(json!({ "device_auth_id": device_code, "user_code": user_code }))
    {
        Ok(r) => r,
        // 403/404 = 用户尚未在浏览器完成授权, 继续轮询
        Err(ureq::Error::Status(403, _)) | Err(ureq::Error::Status(404, _)) => return pending(),
        Err(ureq::Error::Status(410, _)) => {
            return Err("设备码已过期, 请重新发起授权".into())
        }
        Err(e) => return Err(format!("轮询授权状态失败: {}", codex_http_err(e))),
    };

    let success: CodexDevicePollSuccess = resp
        .into_json()
        .map_err(|e| format!("解析授权响应失败: {e}"))?;

    // ③ authorization_code + code_verifier 换 access/refresh/id_token
    let tokens = codex_exchange_code(&success.authorization_code, &success.code_verifier)?;
    let refresh_token = tokens
        .refresh_token
        .clone()
        .ok_or_else(|| "授权响应缺少 refresh_token".to_string())?;
    let account_id = codex_account_id(&tokens);

    codex_write_auth_json(&tokens, &refresh_token, account_id.as_deref())?;
    Ok(CodexPollResult { status: "ok".into() })
}

/// 用 authorization_code + code_verifier 换 token
fn codex_exchange_code(code: &str, code_verifier: &str) -> Result<CodexTokenResp, String> {
    let resp = codex_agent()
        .post(CODEX_OAUTH_TOKEN_URL)
        .set("User-Agent", CODEX_USER_AGENT)
        .send_form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", CODEX_DEVICE_REDIRECT_URI),
            ("client_id", CODEX_CLIENT_ID),
            ("code_verifier", code_verifier),
        ])
        .map_err(|e| format!("换取 Token 失败: {}", codex_http_err(e)))?;
    resp.into_json()
        .map_err(|e| format!("解析 Token 响应失败: {e}"))
}

/// 从 id_token / access_token (JWT) 中提取 chatgpt_account_id
fn codex_account_id(tokens: &CodexTokenResp) -> Option<String> {
    let from = |jwt: &str| -> Option<String> {
        let claims = codex_jwt_claims(jwt)?;
        claims
            .get("chatgpt_account_id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| {
                claims
                    .get("https://api.openai.com/auth")
                    .and_then(|a| a.get("chatgpt_account_id"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .or_else(|| {
                claims
                    .get("organizations")
                    .and_then(|o| o.as_array())
                    .and_then(|a| a.first())
                    .and_then(|o| o.get("id"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
    };
    tokens
        .id_token
        .as_deref()
        .and_then(from)
        .or_else(|| from(&tokens.access_token))
}

/// 解析 JWT 的 payload (第二段) 为 JSON
fn codex_jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = codex_b64url_decode(payload)?;
    serde_json::from_slice(&bytes).ok()
}

/// base64url (无填充) 解码 —— 不引第三方 base64 crate
pub(crate) fn codex_b64url_decode(input: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in input.bytes() {
        if c == b'=' {
            break;
        }
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// 按官方 codex CLI 格式写 ~/.codex/auth.json, 外部 `codex` CLI 可直接复用
fn codex_write_auth_json(
    tokens: &CodexTokenResp,
    refresh_token: &str,
    account_id: Option<&str>,
) -> Result<(), String> {
    let path = codex_auth_path().ok_or_else(|| "无法定位用户主目录".to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建 ~/.codex 目录失败: {e}"))?;
    }

    let auth = json!({
        "OPENAI_API_KEY": Value::Null,
        // 现行 codex CLI / 社区插件(llm-openai-via-codex)按 auth_mode 区分 ChatGPT 订阅
        // 与纯 API key 登录; 缺它会被判成 API-key 模式而拒用订阅额度。务必写 "chatgpt"。
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": tokens.id_token.clone().unwrap_or_default(),
            "access_token": tokens.access_token,
            "refresh_token": refresh_token,
            "account_id": account_id.unwrap_or_default(),
        },
        "last_refresh": codex_rfc3339_now(),
    });

    let content = serde_json::to_string_pretty(&auth)
        .map_err(|e| format!("序列化 auth.json 失败: {e}"))?;
    // auth.json 含 refresh/access/id token:① 原子写防写一半撕裂 → 外部 codex CLI 读到坏 JSON;
    // ② Unix 下收紧到 0600,NAS/Docker 多用户主机上不让同机其他用户读走凭证。
    atomic_write(&path, &content).map_err(|e| format!("写入 ~/.codex/auth.json 失败: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// 当前 UTC 时间的 RFC3339 字符串 (codex CLI 解析 last_refresh 用), 不引 chrono
pub(crate) fn codex_rfc3339_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Howard Hinnant 的 civil_from_days 算法
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// 打开系统默认浏览器到指定 URL (跨平台)
fn codex_open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW(0x0800_0000): 别闪黑窗
        Command::new("cmd")
            .args(["/C", "start", "", url])
            .creation_flags(0x0800_0000)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(url).spawn().map_err(|e| e.to_string())?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(url).spawn().map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ───────────────────────── Commands: Claude 官方订阅授权 (PKCE Authorization Code) ─────────────────────────
//
// Claude 官方订阅登录(与 `claude setup-token` 同源的 OAuth 流):浏览器登录 + 手工回贴授权码。
// 不依赖外部 CLI —— 后端自起浏览器到 claude.ai 授权页,用户登录授权后页面给出 `code#state`,
// 贴回 Polaris,后端用 PKCE code_verifier 换 access/refresh token,按官方
// `~/.claude/.credentials.json` 的 `claudeAiOauth` 结构落盘。外部 `claude` CLI 与 Polaris
// 自起的 claude 都能直接复用这份订阅凭据,无需在外壳里再登录一次。
//
// 两步: ① claude_start_login 生成 PKCE(S256)+ state,拼授权 URL(已自动开浏览器),
//          把 verifier/state 回给前端(本地 IPC,无状态后端,免全局可变态);
//        ② claude_finish_login(回贴的 code、verifier、state)换 token 落盘。

const CLAUDE_OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CLAUDE_OAUTH_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const CLAUDE_OAUTH_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const CLAUDE_OAUTH_REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const CLAUDE_OAUTH_SCOPES: &str = "org:create_api_key user:profile user:inference";

fn claude_credentials_path() -> Option<PathBuf> {
    claude_dir().map(|d| d.join(".credentials.json"))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeAuthStatus {
    pub logged_in: bool,
    pub cred_path: String,
}

/// .credentials.json 存在且带非空 claudeAiOauth.accessToken
fn claude_creds_has_token(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| {
            v.get("claudeAiOauth")
                .and_then(|o| o.get("accessToken"))
                .and_then(|a| a.as_str())
                .map(|s| !s.is_empty())
        })
        .unwrap_or(false)
}

/// 是否已登录 Claude 官方订阅
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn claude_oauth_status() -> Result<ClaudeAuthStatus, String> {
    let path = claude_credentials_path();
    let logged_in = path
        .as_ref()
        .map(|p| claude_creds_has_token(p))
        .unwrap_or(false);
    Ok(ClaudeAuthStatus {
        logged_in,
        cred_path: path
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
    })
}

/// `claude_start_login` 返回给前端的授权信息
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeLoginStart {
    /// 授权页 URL(已自动打开, UI 也展示便于手动打开)
    pub authorize_url: String,
    /// PKCE code_verifier, 回贴换 token 时原样带回
    pub verifier: String,
    /// 防串话 state, 回贴换 token 时原样带回 (授权码尾部 #state 须与之一致)
    pub state: String,
}

/// ① 生成 PKCE(S256)+ state, 拼授权 URL 并打开浏览器
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn claude_start_login() -> Result<ClaudeLoginStart, String> {
    let verifier = claude_rand_b64url(32)?;
    let state = claude_rand_b64url(32)?;
    let challenge = claude_b64url_encode(&claude_sha256(verifier.as_bytes()));
    let authorize_url = format!(
        "{base}?code=true&client_id={cid}&response_type=code&redirect_uri={redir}&scope={scope}&code_challenge={chal}&code_challenge_method=S256&state={state}",
        base = CLAUDE_OAUTH_AUTHORIZE_URL,
        cid = CLAUDE_OAUTH_CLIENT_ID,
        redir = claude_url_encode(CLAUDE_OAUTH_REDIRECT_URI),
        scope = claude_url_encode(CLAUDE_OAUTH_SCOPES),
        chal = challenge,
        state = state,
    );
    // 自动拉起浏览器到授权页 (失败不致命, UI 仍展示链接供手动打开)
    let _ = codex_open_browser(&authorize_url);
    Ok(ClaudeLoginStart {
        authorize_url,
        verifier,
        state,
    })
}

#[derive(Deserialize)]
struct ClaudeTokenResp {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
}

/// ② 用户回贴的授权码(授权页给的是 `code#state`)+ verifier/state 换 token 落盘
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn claude_finish_login(
    pasted: String,
    verifier: String,
    state: String,
) -> Result<ClaudeAuthStatus, String> {
    let pasted = pasted.trim();
    if pasted.is_empty() {
        return Err("请先粘贴授权码".into());
    }
    // 授权页给的是 `code#state`:拆出 code,并核对尾部 state 防串话 / 防贴错
    let mut parts = pasted.splitn(2, '#');
    let code = parts.next().unwrap_or("").trim().to_string();
    let returned_state = parts.next().map(|s| s.trim().to_string());
    if let Some(rs) = returned_state.as_ref() {
        if !rs.is_empty() && rs != &state {
            return Err("授权码与本次请求不匹配(state 不一致),请重新发起授权".into());
        }
    }
    if code.is_empty() {
        return Err("授权码为空".into());
    }

    let resp = codex_agent()
        .post(CLAUDE_OAUTH_TOKEN_URL)
        .set("Content-Type", "application/json")
        .set("User-Agent", CODEX_USER_AGENT)
        .send_json(json!({
            "grant_type": "authorization_code",
            "code": code,
            "state": state,
            "client_id": CLAUDE_OAUTH_CLIENT_ID,
            "redirect_uri": CLAUDE_OAUTH_REDIRECT_URI,
            "code_verifier": verifier,
        }))
        .map_err(|e| format!("换取 Claude Token 失败: {}", codex_http_err(e)))?;

    let tokens: ClaudeTokenResp = resp
        .into_json()
        .map_err(|e| format!("解析 Token 响应失败: {e}"))?;
    let refresh = tokens.refresh_token.clone().unwrap_or_default();
    let expires_at = claude_expires_at_ms(tokens.expires_in.unwrap_or(0));
    let scope_str = tokens.scope.as_deref().unwrap_or(CLAUDE_OAUTH_SCOPES);
    let scopes: Vec<&str> = scope_str.split_whitespace().collect();

    claude_write_credentials(&tokens.access_token, &refresh, expires_at, &scopes)?;
    claude_oauth_status()
}

/// 按官方 `~/.claude/.credentials.json` 的 claudeAiOauth 结构写;合并保留文件里已有的其它键。
fn claude_write_credentials(
    access: &str,
    refresh: &str,
    expires_at: u64,
    scopes: &[&str],
) -> Result<(), String> {
    let path = claude_credentials_path().ok_or_else(|| "无法定位 ~/.claude".to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建 ~/.claude 目录失败: {e}"))?;
    }
    // 保留已有其它键(如 codeWorkspaceTrust 等),只覆盖 claudeAiOauth 块
    let mut root: Value = fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}));
    if !root.is_object() {
        root = json!({});
    }
    root["claudeAiOauth"] = json!({
        "accessToken": access,
        "refreshToken": refresh,
        "expiresAt": expires_at,
        "scopes": scopes,
    });
    let content = serde_json::to_string_pretty(&root).map_err(|e| format!("序列化凭据失败: {e}"))?;
    // 原子写防撕裂(外部 claude CLI 并发读不会读到坏 JSON);Unix 收紧 0600 不让同机他人读走凭证。
    atomic_write(&path, &content).map_err(|e| format!("写入 ~/.claude/.credentials.json 失败: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// 当前毫秒时间戳 + expires_in(秒)→ claudeAiOauth.expiresAt(毫秒)
fn claude_expires_at_ms(expires_in_secs: u64) -> u64 {
    now_ms() + expires_in_secs.saturating_mul(1000)
}

/// 加密安全随机 n 字节 → base64url(无填充)。verifier/state 必须不可预测。
fn claude_rand_b64url(n: usize) -> Result<String, String> {
    let mut buf = vec![0u8; n];
    getrandom::getrandom(&mut buf).map_err(|e| format!("生成安全随机数失败: {e}"))?;
    Ok(claude_b64url_encode(&buf))
}

/// SHA-256(PKCE S256 的 code_challenge 用)
fn claude_sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    let out = h.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

/// base64url 编码(无填充)—— 与 codex_b64url_decode 对偶
fn claude_b64url_encode(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(T[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(T[(n & 63) as usize] as char);
        }
    }
    out
}

/// 极简 percent-encoding:只放行 RFC3986 unreserved,其余按 %XX 编码(够 query 值用)
fn claude_url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ───────────────────────── Commands: 用量看板 ─────────────────────────

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenBucket {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub total: u64,
    pub requests: u64,
    pub cost: f64,
}

impl TokenBucket {
    fn add(&mut self, u: &Usage, cost: f64) {
        self.input += u.input;
        self.output += u.output;
        self.cache_read += u.cache_read;
        self.cache_creation += u.cache_creation;
        self.total += u.input + u.output + u.cache_read + u.cache_creation;
        self.requests += 1;
        self.cost += cost;
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyUsage {
    pub date: String,
    pub label: String,
    pub total: u64,
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub available: bool,
    pub today: TokenBucket,
    pub week: TokenBucket,
    pub month: TokenBucket,
    pub year: TokenBucket,
    pub daily: Vec<DailyUsage>,
}

struct Usage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_creation: u64,
}

/// 模型 → (input, output, cache_write, cache_read) USD / 1M tokens。估算用。
fn model_price(model: &str) -> (f64, f64, f64, f64) {
    let m = model.to_ascii_lowercase();
    if m.contains("opus") {
        (15.0, 75.0, 18.75, 1.5)
    } else if m.contains("haiku") {
        (0.8, 4.0, 1.0, 0.08)
    } else if m.contains("sonnet") {
        (3.0, 15.0, 3.75, 0.3)
    } else if m.contains("gpt") || m.contains("codex") || m.starts_with("o1") || m.starts_with("o3")
    {
        (1.25, 10.0, 1.5625, 0.125)
    } else if m.contains("gemini") {
        (1.25, 10.0, 1.625, 0.31)
    } else if m.contains("deepseek") {
        (0.27, 1.1, 0.027, 0.027)
    } else if m.contains("glm") {
        (0.6, 2.2, 0.11, 0.11)
    } else if m.contains("kimi") || m.contains("moonshot") {
        (0.6, 2.5, 0.15, 0.15)
    } else if m.contains("qwen") || m.contains("minimax") {
        (0.4, 1.2, 0.08, 0.08)
    } else {
        (3.0, 15.0, 3.75, 0.3) // 未知 → Sonnet 档
    }
}

fn line_cost(u: &Usage, model: &str) -> f64 {
    let (pin, pout, pcw, pcr) = model_price(model);
    (u.input as f64 * pin
        + u.output as f64 * pout
        + u.cache_creation as f64 * pcw
        + u.cache_read as f64 * pcr)
        / 1_000_000.0
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn usage_summary() -> Result<UsageSummary, String> {
    // 共享 ~/.claude/projects + 隔离模式的私有账本, 两处都算 —— 深隔离只是把
    // 第三方会话从外部监控的视野里挪走, Polaris 自己的看板仍要看全。
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = claude_dir().map(|d| d.join("projects")) {
        dirs.push(d);
    }
    if let Some(d) = private_claude_home().map(|d| d.join("projects")) {
        dirs.push(d);
    }
    dirs.retain(|d| d.exists());
    if dirs.is_empty() {
        return Ok(empty_summary());
    }

    let today_days = today_utc_days();
    let today_str = ymd_string(today_days);
    let week_cut = ymd_string(today_days - 6);
    let month_cut = ymd_string(today_days - 29);
    let year_cut = ymd_string(today_days - 364);

    // 14 天趋势窗
    let mut trend_window: Vec<(String, String)> = Vec::with_capacity(14);
    for off in (0..14).rev() {
        let d = today_days - off;
        let s = ymd_string(d);
        let label = s.get(5..).unwrap_or(&s).to_string();
        trend_window.push((s, label));
    }
    let trend_set: HashSet<String> = trend_window.iter().map(|(s, _)| s.clone()).collect();
    let mut by_day: HashMap<String, (u64, f64)> = HashMap::new();

    let mut today = TokenBucket::default();
    let mut week = TokenBucket::default();
    let mut month = TokenBucket::default();
    let mut year = TokenBucket::default();
    let mut seen: HashSet<String> = HashSet::new();

    for entry in dirs.iter().flat_map(|d| WalkDir::new(d).into_iter().flatten()) {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.path().extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(file) = fs::File::open(entry.path()) else {
            continue;
        };
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let Ok(line) = line else { continue };
            if line.trim().is_empty() || !line.contains("\"usage\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
                continue;
            }
            let Some(msg) = v.get("message") else { continue };
            let Some(usage_v) = msg.get("usage") else {
                continue;
            };
            if let Some(mid) = msg.get("id").and_then(|x| x.as_str()) {
                if !seen.insert(mid.to_string()) {
                    continue;
                }
            }
            let u = Usage {
                input: usage_v.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                output: usage_v.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                cache_read: usage_v.get("cache_read_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                cache_creation: usage_v.get("cache_creation_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
            };
            let line_tokens = u.input + u.output + u.cache_read + u.cache_creation;
            if line_tokens == 0 {
                continue;
            }
            let model = msg.get("model").and_then(|x| x.as_str()).unwrap_or("");
            let cost = line_cost(&u, model);

            let date = v
                .get("timestamp")
                .and_then(|t| t.as_str())
                .map(|s| s.chars().take(10).collect::<String>())
                .unwrap_or_default();
            if date.is_empty() {
                continue;
            }

            if date.as_str() >= year_cut.as_str() {
                year.add(&u, cost);
                if date.as_str() >= month_cut.as_str() {
                    month.add(&u, cost);
                    if date.as_str() >= week_cut.as_str() {
                        week.add(&u, cost);
                        if date == today_str {
                            today.add(&u, cost);
                        }
                    }
                }
            }
            if trend_set.contains(&date) {
                let e = by_day.entry(date).or_insert((0, 0.0));
                e.0 += line_tokens;
                e.1 += cost;
            }
        }
    }

    let daily: Vec<DailyUsage> = trend_window
        .into_iter()
        .map(|(date, label)| {
            let (total, cost) = by_day.get(&date).copied().unwrap_or((0, 0.0));
            DailyUsage { date, label, total, cost }
        })
        .collect();

    Ok(UsageSummary {
        available: true,
        today,
        week,
        month,
        year,
        daily,
    })
}

fn empty_summary() -> UsageSummary {
    UsageSummary {
        available: false,
        today: TokenBucket::default(),
        week: TokenBucket::default(),
        month: TokenBucket::default(),
        year: TokenBucket::default(),
        daily: Vec::new(),
    }
}

// ───────────────────────── Commands: 套餐额度 / 实时余额 ─────────────────────────
//
// 「把每个套餐的额度显示出来」:对当前供应商调用其官方余额/用量接口拿实时数字。
// 现实约束:55 家里只有少数公开了余额查询接口, 且各家路径 / 字段各不相同, 没有统一标准。
// 故采用「逐家适配 + 优雅降级」:
//   * balance     —— 取到真实数字(Moonshot/Kimi 平台、DeepSeek、SiliconFlow)。
//   * alive       —— 订阅制套餐无额度接口, 仅探活 + 给控制台链接(Kimi For Coding 即此类:
//                     套餐额度每 7 天刷新、只在 Kimi Code 控制台可见, 无公开 REST 接口)。
//   * unsupported —— 该家未提供额度接口, 引导去控制台。
//   * no_key / error —— 未配 key / 查询失败。
// 全部走 12s 超时的阻塞 ureq(与 codex 授权同款), 由用户点击触发, 非后台轮询。

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderBalance {
    pub id: String,
    /// 是否取到了真实可量化的额度数字(kind == "balance")
    pub available: bool,
    /// balance | alive | unsupported | no_key | error
    pub kind: String,
    /// 主显示文案(如 "¥48.59" / "已激活 · 套餐有效" / "未提供查询接口")
    pub label: String,
    /// 次级说明(如 "代金券 ¥46.59 · 现金 ¥3.00")
    pub detail: String,
    /// 控制台 / 官网链接(可空)
    pub console_url: String,
}

/// 余额查询专用 agent:非流式请求-响应, 给 12s 全局 deadline 防认证端点黑洞挂死命令线程。
fn balance_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(12))
        .build()
}

/// 取 base_url 的纯主机名(去 scheme 与路径)。
fn host_only(base: &str) -> String {
    let b = base.trim();
    let b = b
        .strip_prefix("https://")
        .or_else(|| b.strip_prefix("http://"))
        .unwrap_or(b);
    b.split('/').next().unwrap_or(b).to_string()
}

/// GET 一个带 Bearer 鉴权的 JSON 接口(余额类接口都是这套)。
fn balance_get_json(url: &str, token: &str) -> Result<Value, String> {
    let resp = balance_agent()
        .get(url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("User-Agent", "polaris-balance")
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, r) => {
                let body = r.into_string().unwrap_or_default();
                let body = body.chars().take(180).collect::<String>();
                format!("HTTP {code} — {body}")
            }
            ureq::Error::Transport(t) => format!("网络错误: {t}"),
        })?;
    resp.into_json::<Value>()
        .map_err(|e| format!("解析响应失败: {e}"))
}

/// 查询某供应商的「套餐额度 / 实时余额」。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn provider_balance(id: String) -> Result<ProviderBalance, String> {
    let store = STORE.read().clone();
    let views = build_views(&store);
    let v = views
        .iter()
        .find(|v| v.id == id)
        .ok_or_else(|| format!("供应商不存在: {id}"))?;

    let mk = |kind: &str, label: &str, detail: &str, console: &str| ProviderBalance {
        id: id.clone(),
        available: kind == "balance",
        kind: kind.to_string(),
        label: label.to_string(),
        detail: detail.to_string(),
        console_url: console.to_string(),
    };

    let token = v.auth_token.trim().to_string();
    if token.is_empty() && v.kind != "official" {
        return Ok(mk(
            "no_key",
            "未配置 Key",
            "先填入 API Key 再查询套餐额度",
            &v.website_url,
        ));
    }

    match id.as_str() {
        // Moonshot / Kimi 开放平台(按量付费)—— 真实人民币余额。
        "kimi" => {
            let host = host_only(&v.base_url);
            let host = if host.is_empty() { "api.moonshot.cn".to_string() } else { host };
            let url = format!("https://{host}/v1/users/me/balance");
            let j = balance_get_json(&url, &token)?;
            let d = j.get("data").cloned().unwrap_or(Value::Null);
            let avail = d.get("available_balance").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let voucher = d.get("voucher_balance").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let cash = d.get("cash_balance").and_then(|x| x.as_f64()).unwrap_or(0.0);
            Ok(mk(
                "balance",
                &format!("¥{avail:.2}"),
                &format!("代金券 ¥{voucher:.2} · 现金 ¥{cash:.2}"),
                "https://platform.moonshot.cn/console",
            ))
        }
        // Kimi For Coding —— 订阅套餐, 无公开额度接口, 用 /v1/models 探活 + 控制台链接。
        "kimi-for-coding" => {
            let url = format!("{}/v1/models", v.base_url.trim_end_matches('/'));
            match balance_get_json(&url, &token) {
                Ok(_) => Ok(mk(
                    "alive",
                    "已激活 · 套餐有效",
                    "订阅套餐额度每 7 天刷新, 剩余额度/速率请在 Kimi Code 控制台查看",
                    "https://www.kimi.com/code/console",
                )),
                Err(e) => Ok(mk("error", "校验失败", &e, "https://www.kimi.com/code/console")),
            }
        }
        // DeepSeek —— GET /user/balance, balance_infos[0].total_balance(字符串)。
        "deepseek" => {
            let j = balance_get_json("https://api.deepseek.com/user/balance", &token)?;
            let info = j
                .get("balance_infos")
                .and_then(|a| a.as_array())
                .and_then(|a| a.first())
                .cloned()
                .unwrap_or(Value::Null);
            let cur = info.get("currency").and_then(|x| x.as_str()).unwrap_or("CNY");
            let total = info.get("total_balance").and_then(|x| x.as_str()).unwrap_or("0");
            let granted = info.get("granted_balance").and_then(|x| x.as_str()).unwrap_or("0");
            let sym = if cur == "USD" { "$" } else { "¥" };
            Ok(mk(
                "balance",
                &format!("{sym}{total}"),
                &format!("赠送 {sym}{granted} · 货币 {cur}"),
                "https://platform.deepseek.com",
            ))
        }
        // SiliconFlow —— GET /v1/user/info, data.totalBalance(字符串)。
        "siliconflow" | "siliconflow-en" => {
            let url = format!("{}/v1/user/info", v.base_url.trim_end_matches('/'));
            let j = balance_get_json(&url, &token)?;
            let d = j.get("data").cloned().unwrap_or(Value::Null);
            let total = d.get("totalBalance").and_then(|x| x.as_str()).unwrap_or("0");
            let charge = d.get("chargeBalance").and_then(|x| x.as_str()).unwrap_or("0");
            let bal = d.get("balance").and_then(|x| x.as_str()).unwrap_or("0");
            Ok(mk(
                "balance",
                &format!("¥{total}"),
                &format!("充值 ¥{charge} · 赠送 ¥{bal}"),
                "https://cloud.siliconflow.cn/account/balance",
            ))
        }
        // MiniMax —— 未公开额度查询接口, 引导去控制台。
        "minimax" | "minimax-en" => Ok(mk(
            "unsupported",
            "控制台查看",
            "MiniMax 未提供公开额度查询接口, 余额请在平台控制台查看",
            "https://platform.minimaxi.com",
        )),
        // 官方 Claude 订阅 —— 用量按订阅档, 无额度数字接口。
        "claude-official" => Ok(mk(
            "unsupported",
            "订阅制",
            "Claude 官方订阅按档计费, 用量请见下方 Token 统计或 claude.ai",
            "https://claude.ai/settings/usage",
        )),
        _ => Ok(mk(
            "unsupported",
            "未提供查询接口",
            "该供应商未提供公开额度查询接口",
            &v.website_url,
        )),
    }
}

// ───────────────────────── 工具函数 ─────────────────────────

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn today_utc_days() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (secs / 86400) as i64
}

/// 天数 → YYYY-MM-DD (Howard Hinnant civil_from_days, 无外部依赖)
fn ymd_string(z: i64) -> String {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + if m <= 2 { 1 } else { 0 };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

// ───────────────────────── 单测: Claude PKCE 密码学正确性 ─────────────────────────
#[cfg(test)]
mod claude_oauth_tests {
    use super::*;

    /// base64url(无填充)对已知向量正确,且与 codex_b64url_decode 互逆
    #[test]
    fn b64url_encode_known_vectors() {
        assert_eq!(claude_b64url_encode(b""), "");
        assert_eq!(claude_b64url_encode(b"f"), "Zg");
        assert_eq!(claude_b64url_encode(b"fo"), "Zm8");
        assert_eq!(claude_b64url_encode(b"foo"), "Zm9v");
        assert_eq!(claude_b64url_encode(b"foob"), "Zm9vYg");
        assert_eq!(claude_b64url_encode(b"fooba"), "Zm9vYmE");
        assert_eq!(claude_b64url_encode(b"foobar"), "Zm9vYmFy");
        // 含会被标准 base64 编成 '+' '/' 的字节,base64url 必须出 '-' '_'
        let enc = claude_b64url_encode(&[0xfb, 0xff, 0xbf]);
        assert!(!enc.contains('+') && !enc.contains('/') && !enc.contains('='));
        // 编码→解码 round-trip
        let raw: Vec<u8> = (0u8..=255).collect();
        let back = codex_b64url_decode(&claude_b64url_encode(&raw)).unwrap();
        assert_eq!(raw, back);
    }

    /// SHA-256 对 "abc" 的标准向量(NIST FIPS 180-4)
    #[test]
    fn sha256_known_vector() {
        let d = claude_sha256(b"abc");
        let hex: String = d.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// PKCE S256 端到端:RFC 7636 附录 B 的官方测试向量
    /// verifier "dBjft...60M" → challenge "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    #[test]
    fn pkce_s256_rfc7636_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = claude_b64url_encode(&claude_sha256(verifier.as_bytes()));
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    /// 随机 verifier/state 不可预测且长度足够(32 字节 → 43 字符 base64url)
    #[test]
    fn rand_b64url_len_and_uniqueness() {
        let a = claude_rand_b64url(32).unwrap();
        let b = claude_rand_b64url(32).unwrap();
        assert_eq!(a.len(), 43);
        assert_ne!(a, b);
        assert!(!a.contains('=') && !a.contains('+') && !a.contains('/'));
    }

    /// query 值百分号编码:空格不留原样,unreserved 不动
    #[test]
    fn url_encode_query_value() {
        assert_eq!(
            claude_url_encode("org:create_api_key user:profile"),
            "org%3Acreate_api_key%20user%3Aprofile"
        );
        assert_eq!(claude_url_encode("aZ09-_.~"), "aZ09-_.~");
    }
}

#[cfg(test)]
mod per_conv_scope_tests {
    use super::*;
    use std::collections::HashMap;
    use std::process::Command;

    /// 收集一条 Command 上「显式设置/移除」的 env 覆写: key → Some(值) 表示设了某值,
    /// key → None 表示被 env_remove(阻止从父进程继承)。inherited(没动过的)不出现在这里。
    fn cmd_env_overrides(cmd: &Command) -> HashMap<String, Option<String>> {
        cmd.get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|s| s.to_string_lossy().into_owned()),
                )
            })
            .collect()
    }

    /// 核心隔离机制: 对话显式选「Claude 官方」时, 即便父进程(全局当前供应商)残留着
    /// ANTHROPIC_BASE_URL/TOKEN, 也会被本条命令逐键 env_remove 顶掉 —— 这条 claude
    /// 因此走官方端点, 与全局开关、与其它并发对话彻底解耦。这正是「每对话隔离」的根。
    #[test]
    fn forced_official_clears_inherited_global_env() {
        // 模拟「全局当前 = 某第三方」在父进程 env 里留下的痕迹
        std::env::set_var("ANTHROPIC_BASE_URL", "https://global-thirdparty.example/anthropic");
        std::env::set_var("ANTHROPIC_AUTH_TOKEN", "global-token");

        let mut cmd = Command::new("claude");
        scope_child_claude_by_id(&mut cmd, Some("claude-official"));
        let ov = cmd_env_overrides(&cmd);

        // 受管键必须被显式移除(值为 None), 而不是放任继承全局那家
        for k in ["ANTHROPIC_BASE_URL", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY", "ANTHROPIC_MODEL"] {
            assert_eq!(ov.get(k), Some(&None), "受管键 {k} 应被 env_remove 顶掉继承值");
        }
        // 官方档不套私有 CLAUDE_CONFIG_DIR(OAuth 凭据在共享 ~/.claude)
        assert!(!ov.contains_key("CLAUDE_CONFIG_DIR"), "官方不应改 CLAUDE_CONFIG_DIR");
    }

    /// Auto 档(None/""/"auto"): 不做逐命令注入。全局当前为官方时, scope_child_claude
    /// 提前返回 → 命令上零 env 覆写(纯继承父进程), 行为与旧版完全一致。
    #[test]
    fn auto_is_passthrough_when_global_official() {
        {
            let mut s = STORE.write();
            s.link_global = false;
            s.current_id = "claude-official".to_string();
        }
        for id in [None, Some(""), Some("auto")] {
            let mut cmd = Command::new("claude");
            scope_child_claude_by_id(&mut cmd, id);
            assert!(
                cmd_env_overrides(&cmd).is_empty(),
                "Auto 档({id:?}) 不应在命令上留下任何 env 覆写"
            );
        }
    }
}
