use super::*;

const DEFAULT_TOKEN_FIELD: &str = "ANTHROPIC_AUTH_TOKEN";
const API_KEY_FIELD: &str = "ANTHROPIC_API_KEY";

/// 本地路由模式下写进供应商配置的**占位** token(路由不看它, 真 key 由路由实时注入)。
/// 借 key 的路径必须认得它 —— 把占位串当成真 key 借出去, 拿去打上游必 401。
const LOCAL_ROUTE_TOKEN: &str = "polaris-local-router";

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

// ───────────────────────── 预设供应商表 (全量 56) ─────────────────────────
// base_url / apiKeyField / category 取自 cc-switch claudeProviderPresets。
// kind: official(清空 env) | key(写 base+token) | codex(ChatGPT 订阅·本地路由)
//       | openai(OpenAI 协议 API key·本地路由) | copilot(暂未支持)

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
    Preset { id: "openai", name: "OpenAI (GPT API)", base_url: "https://api.openai.com/v1", token_field: DEFAULT_TOKEN_FIELD, category: "third_party", kind: "openai" },
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
    if let Some(rest) = b
        .strip_prefix("https://")
        .or_else(|| b.strip_prefix("http://"))
    {
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
    /// 上游协议: ""(默认, Anthropic 兼容·claude 直连) | "openai"(OpenAI chat/completions,
    /// 切换时经本地路由翻译转发)。老 store 无此字段 → serde 默认 "" → 行为不变。
    #[serde(default)]
    protocol: String,
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
pub(crate) struct Store {
    #[serde(default)]
    current_id: String,
    /// true = 联动(切换写 ~/.claude/settings.json, 终端 CLI 跟着变);
    /// false(默认) = 隔离(只作用于 Polaris 自己 spawn 的 claude, 走进程 env)。
    /// 老 store 没有此字段 → serde 默认 false → 升级即自动隔离, 串台就此止住。
    #[serde(default)]
    link_global: bool,
    /// 本地路由总开关(cc-switch 式代理模式)。true = Anthropic 兼容供应商也统一走
    /// 127.0.0.1 本地路由透传(key 由路由实时注入, 改 key 即刻生效、不进子进程 env);
    /// false(默认) = 直连(base/key 直接注入环境)。GPT/Codex 等非 Anthropic 协议
    /// 不受此开关影响 —— 它们必须经路由翻译, 恒走路由。
    #[serde(default)]
    route_local: bool,
    #[serde(default)]
    items: Vec<StoredProvider>,
    // legacy（迁移后清空, 不再写出）
    #[serde(default, skip_serializing)]
    custom: Vec<LegacyCustom>,
    #[serde(default, skip_serializing)]
    keys: HashMap<String, LegacyKey>,
}

pub(crate) static STORE: Lazy<RwLock<Store>> = Lazy::new(|| RwLock::new(Store::default()));
static STORE_PATH: Lazy<RwLock<PathBuf>> = Lazy::new(|| RwLock::new(PathBuf::new()));
/// 串行化对 settings.json / providers.json 的「读-改-写」。
/// Tauri 命令可并发跑在线程池上, 两个 provider_switch 同时进来若不串行化, 会交错写同一份
/// settings.json → 撕裂成半截。此锁保证整条 RMW 原子, 与 atomic_write 一起根治配置损坏。
static IO_LOCK: Lazy<parking_lot::Mutex<()>> = Lazy::new(|| parking_lot::Mutex::new(()));

// ─────────────────── 故障转移(polaris-failover 滑窗) ───────────────────
// 5 分钟窗口内同一供应商第 3 次网络/鉴权/欠费类失败 → 自动切到列表里下一个可用
// 供应商, 并 emit 事件让前端 toast。不自动切回(防抖动), 切回由用户在坞里确认。
static FAILOVER: Lazy<polaris_failover::FailoverTracker> =
    Lazy::new(polaris_failover::FailoverTracker::new);
/// init() 时存一份 AppHandle 供故障转移线程 emit 事件用 —— 失败上报点(本地路由
/// codex_proxy 的 TCP 线程)手里没有 app 句柄, 只能走全局。
static FAILOVER_APP: Lazy<RwLock<Option<AppHandle>>> = Lazy::new(|| RwLock::new(None));

/// 当前激活供应商 id(本地路由无 `/p/{id}` 前缀的旧版路径需要它做故障归因)。
pub fn current_provider_id() -> String {
    STORE.read().current_id.clone()
}

/// 请求成功: 清零该供应商的失败计数(偶发抖动不积累)。
pub fn note_provider_success(id: &str) {
    FAILOVER.record_success(id);
}

/// 记一次网络/鉴权/欠费类失败; 达到滑窗阈值时自动切换到下一个可用供应商。
/// 取舍说明: 最理想的上报点是「claude 子进程每次上游请求的结果」, 但直连模式下
/// 请求发生在 claude CLI 内部, kernel 看不见 —— 所以接在**本地路由**(codex_proxy,
/// cc-switch 式代理, 每请求过手)的错误分类点上: 透传 4xx/5xx、OpenAI 兼容 401、
/// ChatGPT 刷新失败、连接超时等都会走到这里。直连供应商享受不到自动转移, 属已知边界。
pub fn note_provider_failure(id: &str) {
    if !FAILOVER.record_failure(id) {
        return; // 未达阈值: 只计数
    }
    // 达阈: 挑「当前供应商列表里失败者之后的下一个可用项」(环形), 沿用 provider_switch
    // 完成真正切换(env/settings 写入逻辑零重复)。仅当失败者就是当前供应商才切 ——
    // 用户手动在用别家时, 后台某路由请求失败不该抢方向盘。
    let (views, current_id) = {
        let store = STORE.read().clone();
        (build_views(&store), store.current_id.clone())
    };
    if current_id != id {
        return;
    }
    let Some(pos) = views.iter().position(|v| v.id == id) else {
        return;
    };
    let failed_name = views[pos].name.clone();
    // 环形找下一个「有 key 且不是失败者」的供应商; 找不到备用就只广播失败, 不切。
    let next = (1..views.len())
        .map(|off| &views[(pos + off) % views.len()])
        .find(|v| v.has_key && v.id != id)
        .cloned();
    let Some(next) = next else {
        eprintln!("[provider-failover] {failed_name} 连续失败, 但没有其他可用供应商, 不切换");
        return;
    };
    match provider_switch(next.id.clone()) {
        Ok(_) => {
            let msg = format!("供应商「{failed_name}」连续失败, 已自动切换到「{}」", next.name);
            eprintln!("[provider-failover] {msg}");
            emit_failover(&json!({
                "kind": "switched",
                "from": id,
                "fromName": failed_name,
                "to": next.id,
                "toName": next.name,
                "message": msg,
            }));
        }
        Err(e) => eprintln!("[provider-failover] 自动切换失败: {e}"),
    }
}

fn emit_failover(payload: &Value) {
    #[cfg(feature = "desktop")]
    {
        use tauri::Emitter;
        if let Some(app) = FAILOVER_APP.read().as_ref() {
            let _ = app.emit("provider://failover", payload.clone());
        }
    }
    #[cfg(not(feature = "desktop"))]
    {
        if let Some(app) = FAILOVER_APP.read().as_ref() {
            let _ = app.emit("provider://failover", payload.clone());
        }
    }
}

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

/// 供 Polaris 内部小调用(如「语音整形」)借用的 MiniMax key:优先取用户已在坞里
/// 自存的 minimax / minimax-en 供应商 key, 否则回落到构建期注入的「粉丝福利」key。
/// 都没有则返回空串。让语音整形等功能即便用户没单独填 key 也能开箱用上福利额度。
pub fn minimax_borrow_key() -> String {
    {
        let store = STORE.read();
        for id in ["minimax", "minimax-en"] {
            if let Some(it) = store.items.iter().find(|i| i.id == id) {
                for field in [DEFAULT_TOKEN_FIELD, API_KEY_FIELD] {
                    let tok = cfg_env_str(&it.settings_config, field);
                    if !tok.trim().is_empty() {
                        return tok.trim().to_string();
                    }
                }
            }
        }
    }
    gift_minimax_key()
}

/// **生图**专用借 key:只借用户自己在坞里存的 minimax / minimax-en(Coding Plan)key,
/// **不**回落粉丝福利 key —— 生图按张烧额度, 共享福利 key 扛不住, 只留给语音整形这类小调用。
/// 实测(2026-07-21): `sk-cp-` 前缀的 Coding Plan key 可直接调 `/v1/image_generation`。
/// 回 (key, 是否国际站) —— 国际站 key 须打 api.minimax.io, 国内站打 api.minimaxi.com,
/// 打错主站会 401, 由调用方按此选 endpoint。
pub fn minimax_borrow_key_for_image() -> Option<(String, bool)> {
    fn pick(st: &Store) -> Option<(String, bool)> {
        for id in ["minimax", "minimax-en"] {
            if let Some(it) = st.items.iter().find(|i| i.id == id) {
                for field in [DEFAULT_TOKEN_FIELD, API_KEY_FIELD] {
                    let tok = cfg_env_str(&it.settings_config, field);
                    if !tok.trim().is_empty() {
                        return Some((tok.trim().to_string(), id == "minimax-en"));
                    }
                }
            }
        }
        None
    }
    // 壳内(Tauri/server)STORE 由 init() 装载, 直接读内存。但对话里的生图跑在
    // `polaris-forge image` **子进程**里 —— 没有 AppHandle 不走 init, STORE 恒空;
    // 以 STORE_PATH 是否已设判「未初始化」, 是则从磁盘只读解析 providers.json,
    // 否则聊天出图的借用链路在子进程里永远断(实测就是这么断的)。
    if !STORE_PATH.read().as_os_str().is_empty() {
        return pick(&STORE.read());
    }
    let user = UserDirs::new()?;
    let path = user
        .home_dir()
        .join("Polaris")
        .join("data")
        .join("providers.json");
    let txt = fs::read_to_string(path).ok()?;
    let st: Store = serde_json::from_str(&txt).ok()?;
    pick(&st)
}

/// **火山方舟**系聊天供应商的 id, 按优先级排 —— 「火山方舟 Agentplan」摆第一位,
/// 它就是用户口里的「我们火山的 agentplan」。BytePlus 是同一套方舟的海外站。
const VOLC_ARK_IDS: &[&str] = &["agentplan", "doubaoseed", "byteplus"];

/// **生图**专用借 key(火山方舟):借用户已在聊天坞里配好的方舟 key, 拼出方舟的生图端点。
///
/// 与 MiniMax 那条借用同一个理由 —— 用户已经为方舟付过费/配过 key, 没道理再让他去生图坞
/// 重填一遍。回 `(key, endpoint)`: endpoint 由该家 base_url 的**主机名**推出, 因为方舟
/// 国内站(ark.cn-beijing.volces.com)与海外站(ark.ap-southeast.bytepluses.com)是两套域名,
/// 拿错域名必 401;而 base_url 的路径部分(`/api/coding` 之类)是聊天套餐专用前缀,
/// **生图要走 `/api/v3`**, 不能照抄。
pub fn volc_ark_borrow_key_for_image() -> Option<(String, String)> {
    let (key, host) = volc_ark_borrow()?;
    Some((key, format!("https://{host}/api/v3/images/generations")))
}

/// 借用聊天坞里配好的**火山方舟** key, 回 `(key, 主机名)`。
///
/// 生图(images/generations)与语音识别(chat/completions 的 input_audio)都用它 ——
/// 用户已经为方舟付过费/配过 key, 没道理再让他去别处重填一遍。
/// 只回主机名不回完整地址, 因为方舟国内站(ark.cn-beijing.volces.com)与海外站
/// (ark.ap-southeast.bytepluses.com)是两套域名(拿错必 401), 而 base_url 的路径部分
/// (`/api/coding` 之类)是**聊天套餐专用前缀**, 生图/转写都得走 `/api/v3`, 不能照抄。
pub fn volc_ark_borrow() -> Option<(String, String)> {
    fn pick(st: &Store) -> Option<(String, String)> {
        for id in VOLC_ARK_IDS {
            let Some(it) = st.items.iter().find(|i| &i.id == id) else {
                continue;
            };
            let mut key = String::new();
            for field in [DEFAULT_TOKEN_FIELD, API_KEY_FIELD] {
                let tok = cfg_env_str(&it.settings_config, field);
                if !tok.trim().is_empty() {
                    key = tok.trim().to_string();
                    break;
                }
            }
            // 「本地路由」模式下这家的 base_url 会被改写成 127.0.0.1 的 /p/{id}, token 是
            // 占位串 —— 拿它去打方舟必失败, 而且一旦返回 Some 就把整条借用链堵死
            // (火山优先 → MiniMax 也轮不到)。开关关掉后旧配置也可能残留, 所以按内容判而非按开关判。
            if key.is_empty() || key == LOCAL_ROUTE_TOKEN {
                continue;
            }
            let host = ark_host(&cfg_env_str(&it.settings_config, "ANTHROPIC_BASE_URL"), id);
            return Some((key, host));
        }
        None
    }
    // 与 minimax_borrow_key_for_image 同款双路读取:壳内读内存 STORE, 生图子进程
    // (polaris-forge image, 没有 AppHandle 不走 init, STORE 恒空)则直接读磁盘,
    // 否则「对话里出图」这条最常用的链路借不到 key。
    if !STORE_PATH.read().as_os_str().is_empty() {
        return pick(&STORE.read());
    }
    let user = UserDirs::new()?;
    let path = user
        .home_dir()
        .join("Polaris")
        .join("data")
        .join("providers.json");
    let txt = fs::read_to_string(path).ok()?;
    let st: Store = serde_json::from_str(&txt).ok()?;
    pick(&st)
}

/// 从已配的 base_url 里取方舟主机名; 取不到就按 id 回默认站点。
///
/// 「取不到」包括三种:清空过 / 含 `${}` 变量 / **指向本地路由的回环地址** ——
/// 最后这种最阴:本地路由开关开过(或旧配置残留)时 base_url 是
/// `http://127.0.0.1:{port}/p/agentplan`, 照抄主机名就会拼出 `https://127.0.0.1:{port}/api/v3/...`
/// (在 http 端口上说 TLS), 生图与语音转写双双打空炮。一律回落到该家的真站点。
fn ark_host(base_url: &str, id: &str) -> String {
    let b = base_url.trim();
    if let Some(rest) = b
        .strip_prefix("https://")
        .or_else(|| b.strip_prefix("http://"))
    {
        let host = rest.split('/').next().unwrap_or("").trim();
        let bare = host.split(':').next().unwrap_or(host);
        let loopback = matches!(bare, "127.0.0.1" | "localhost" | "::1" | "0.0.0.0");
        if !host.is_empty() && !host.contains('$') && !loopback {
            return host.to_string();
        }
    }
    if id == "byteplus" {
        "ark.ap-southeast.bytepluses.com".to_string()
    } else {
        "ark.cn-beijing.volces.com".to_string()
    }
}

/// 还原构建期注入的「免费额度赠送」Kimi For Coding token(XOR 混淆, 见 build.rs)。
/// 与 MiniMax 同 —— 仅从环境变量注入(CI secret POLARIS_GIFT_KIMI_KEY);未注入
/// (本地 dev / 无 secret 构建)时返回空串, seed_gift_kimi 见空即跳过, 不种子。
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
        default_config(
            "https://api.minimaxi.com/anthropic",
            DEFAULT_TOKEN_FIELD,
            &key,
        ),
        "MiniMax-M3",
    );
    store.items.push(StoredProvider {
        id: "minimax".to_string(),
        name: "MiniMax".to_string(),
        note: "粉丝福利 · 预置额度，开箱即用 · MiniMax-M3".to_string(),
        website_url: "https://www.minimaxi.com".to_string(),
        token_field: DEFAULT_TOKEN_FIELD.to_string(),
        protocol: String::new(),
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
        protocol: String::new(),
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

/// OpenAI 协议供应商未钉模型时的默认模型名。
const DEFAULT_OPENAI_MODEL: &str = "gpt5.6-sol";

/// 本地路由配置: 把 base_url 指到本地路由的 `/p/{id}` 前缀(路由按 id 实时解析该家上游,
/// 每对话可并发指向不同家而不串台), 钉四档模型(含小任务档), AUTH_TOKEN 给个占位串
/// (路由不看它 —— ChatGPT 认 ~/.codex/auth.json, OpenAI 协议认 store 里该家的 key),
/// 让 claude 愿意发请求。
fn local_route_config(port: u16, id: &str, model: &str) -> Value {
    let mut env = Map::new();
    env.insert(
        "ANTHROPIC_BASE_URL".into(),
        Value::String(format!("http://127.0.0.1:{port}/p/{id}")),
    );
    env.insert(
        "ANTHROPIC_AUTH_TOKEN".into(),
        Value::String(LOCAL_ROUTE_TOKEN.into()),
    );
    for k in MODEL_ENV_KEYS {
        env.insert((*k).into(), Value::String(model.into()));
    }
    env.insert(
        "ANTHROPIC_SMALL_FAST_MODEL".into(),
        Value::String(model.into()),
    );
    json!({ "env": Value::Object(env) })
}

/// 供应商钉选的模型名(ANTHROPIC_MODEL), 空则回落 OpenAI 默认。
fn pinned_model_or_default(cfg: &Value) -> String {
    let m = cfg_env_str(cfg, "ANTHROPIC_MODEL");
    let m = m.trim();
    if m.is_empty() {
        DEFAULT_OPENAI_MODEL.to_string()
    } else {
        m.to_string()
    }
}

/// 本地路由的上游目标 —— codex_proxy 按请求路径里的供应商 id 实时解析(改 key/换模型
/// 立即生效, 不用重启会话), 这是 cc-switch 式「本地转发热切换」的根。
pub enum RouteTarget {
    /// ChatGPT 订阅(Responses 协议, 鉴权走 ~/.codex/auth.json)
    Chatgpt,
    /// OpenAI 兼容 chat/completions(鉴权走该供应商存的 API key)
    OpenAiCompat {
        base_url: String,
        api_key: String,
        model: String,
    },
    /// Anthropic 兼容上游 —— 本地路由总开关下的纯透传(不翻译, 只按 token_field
    /// 实时注入鉴权头): ANTHROPIC_API_KEY → x-api-key, 其余 → Authorization Bearer。
    AnthropicCompat {
        base_url: String,
        api_key: String,
        token_field: String,
    },
}

/// 按供应商 id 解析路由目标。空 id / "codex" → ChatGPT(兼容旧版裸端口 base_url)。
pub fn route_target(id: &str) -> Result<RouteTarget, String> {
    if id.is_empty() || id == "codex" {
        return Ok(RouteTarget::Chatgpt);
    }
    let store = STORE.read().clone();
    let views = build_views(&store);
    let v = views
        .iter()
        .find(|v| v.id == id)
        .ok_or_else(|| format!("未知路由目标: {id}"))?;
    if v.kind == "codex" {
        return Ok(RouteTarget::Chatgpt);
    }
    if v.protocol == "openai" {
        let key = v.auth_token.trim();
        if key.is_empty() {
            return Err(format!("供应商「{}」未配置 API Key", v.name));
        }
        let base = normalize_url(&v.base_url);
        if base.is_empty() {
            return Err(format!("供应商「{}」未配置请求地址", v.name));
        }
        return Ok(RouteTarget::OpenAiCompat {
            base_url: base,
            api_key: key.to_string(),
            model: pinned_model_or_default(&v.settings_config),
        });
    }
    // Anthropic 兼容(key/custom)→ 透传目标。不看 route_local 开关: 只要配置指到了
    // /p/{id}(开关开过 / 旧配置残留), 就该被正确转发, 比拒接更稳。
    if v.protocol.is_empty() && (v.kind == "key" || v.kind == "custom") {
        let key = v.auth_token.trim();
        if key.is_empty() {
            return Err(format!("供应商「{}」未配置 API Key", v.name));
        }
        let base = normalize_url(&v.base_url);
        if base.is_empty() {
            return Err(format!("供应商「{}」未配置请求地址", v.name));
        }
        return Ok(RouteTarget::AnthropicCompat {
            base_url: base,
            api_key: key.to_string(),
            token_field: if v.token_field.is_empty() {
                DEFAULT_TOKEN_FIELD.to_string()
            } else {
                v.token_field.clone()
            },
        });
    }
    Err(format!("供应商「{}」不经本地路由", v.name))
}

pub fn init(_app: &AppHandle) -> Result<()> {
    // 故障转移事件通道: 失败上报点在无 app 句柄的 TCP 线程里, 这里存一份全局。
    *FAILOVER_APP.write() = Some(_app.clone());
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
            protocol: String::new(),
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
                protocol: String::new(),
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
            // 联动: 若上次退出时正走本地路由(Codex / OpenAI 协议 / 路由总开关), 重启后
            // 端口可能变 → 重新拉起路由并校正 ANTHROPIC_BASE_URL, 否则 settings.json
            // 残留旧端口 claude 连不上。直连供应商的 settings.json 本来就是对的, 不重写。
            let cur = detect_current(&views, &store);
            if let Some(v) = views.iter().find(|v| v.id == cur) {
                if let Ok(cfg) = cfg_for_view(v, store.route_local) {
                    if cfg_env_str(&cfg, "ANTHROPIC_BASE_URL").starts_with("http://127.0.0.1:") {
                        let _ = apply_settings_config(&cfg);
                        apply_process_env(&cfg);
                    }
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
                if let Ok(cfg) = cfg_for_view(v, store.route_local) {
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
pub(crate) fn atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    // tmp 名带进程内唯一序号:固定名下两个并发写者(如 codex 刷新回写 vs 重新授权落盘)
    // 会互相截断对方的 tmp,先 rename 的一方可能把「对方的半截内容」提交成目标文件。
    // 唯一名让各写各的,rename 仍原子,后完成者整体覆盖(last-wins,不会撕裂)。
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".polaris.{}.{}.tmp", std::process::id(), n));
    let tmp = PathBuf::from(tmp);
    // 写完必须 sync_all 再 rename: rename 只保证目录项切换原子, 不保证 tmp 数据已刷盘。
    // 断电时 rename(元数据)可能先于数据落盘 → 目标变 0 字节/半截, 「绝不残缺」失效。
    // (rename 后不 fsync 父目录: 最坏回退到旧文件, 仍是完整 JSON, 可接受。)
    {
        use std::io::Write;
        let mut f = fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    // 收紧权限:providers.json 内含明文 API key —— unix 上设 0o600(仅属主可读写),防同机其它
    // 用户、或文件被同步/备份进宽权限目录时被顺手读走。在 rename 前设,确保最终文件落地即 600。
    // (Windows 用户配置目录已按用户 ACL 隔离,此处不额外处理。真·防同用户恶意软件仍需 OS 钥匙串
    //  级静态加密 —— 见 security 备忘,作为后续带真机验证的独立改动。)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
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
    /// 上游协议: "" = Anthropic 兼容(claude 直连) | "openai" = OpenAI chat/completions
    /// (经本地路由翻译) | "chatgpt" = ChatGPT 订阅(codex, 经本地路由)。
    pub protocol: String,
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
    /// 本地路由总开关: true = 所有供应商统一经 127.0.0.1 本地路由转发(热切换)
    pub route_local: bool,
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
    /// ""/None = Anthropic 兼容(直连); "openai" = OpenAI 协议(经本地路由转发)
    #[serde(default)]
    pub protocol: Option<String>,
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
    protocol: &str,
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
    // 预设 kind 即协议来源; 自定义供应商由存储的 protocol 决定。
    let protocol = match kind {
        "codex" => "chatgpt",
        "openai" => "openai",
        _ => protocol,
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
        protocol: protocol.to_string(),
        is_preset,
        has_key,
        auth_token: token,
        settings_config: cfg,
    }
}

pub(crate) fn build_views(store: &Store) -> Vec<ProviderView> {
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
            p.id,
            p.name,
            note,
            &token_field,
            p.category,
            p.kind,
            "",
            true,
            p.base_url,
            "",
            cfg,
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
            &it.protocol,
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

pub(crate) fn claude_dir() -> Option<PathBuf> {
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
    v.get("env")
        .and_then(|e| e.as_object())
        .cloned()
        .unwrap_or_default()
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
    let env = obj.entry("env".to_string()).or_insert_with(|| json!({}));
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

/// 给「生图」用的画像：返回 (可用的生图家展示名, 是否真能生图)。
///
/// 本表(聊天供应商坞)里 55 家全是文本/代码大模型,**没有一个能生图** —— 生图配置住在
/// 独立的生图坞(`image_store.rs`,理由见其文件头)。这里只是把它转出来。
///
/// 旧实现是「settings.json 或进程 env 里有非空 `OPENAI_API_KEY` 就算支持」—— 那是张**空头
/// 支票**:环境变量里有个 key 不代表我们真会去调它(当时压根没有生图调用路径),而
/// `prompt.rs` 却照着它跟用户承诺「可以在 API 供应商里配置图像 API」。现在按**真配了生图家
/// 且填了 Key** 判定,承诺才对得上实现。
pub fn image_gen_capability() -> (String, bool) {
    match image_store::current_image_config() {
        Some(c) => (format!("{}({})", c.name, c.model), true),
        None => (String::new(), false),
    }
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
        route_local: store.route_local,
    })
}

/// 把供应商视图换算成待生效的 settings_config(codex / OpenAI 协议会顺带拉起本地路由)。
/// `route_all` = 本地路由总开关(store.route_local): 开着时 Anthropic 兼容供应商
/// 也统一改走 127.0.0.1 本地路由透传(cc-switch 式代理模式)。
fn cfg_for_view(v: &ProviderView, route_all: bool) -> Result<Value, String> {
    if v.kind == "copilot" {
        return Err("GitHub Copilot 需 GitHub OAuth + token 交换, 本地路由暂未覆盖".to_string());
    }
    if v.kind == "codex" {
        // Codex(ChatGPT) → 路由到本地翻译代理: 先确认已授权, 再拉起路由并把
        // ANTHROPIC_BASE_URL 指到 127.0.0.1:port/p/codex, claude 即透明用上 ChatGPT 订阅。
        let authed = codex_auth_path()
            .map(|p| codex_auth_has_tokens(&p))
            .unwrap_or(false);
        if !authed {
            return Err("请先授权 ChatGPT (Codex), 再切换到它".to_string());
        }
        let port = crate::integrations::codex_proxy::ensure_running()?;
        return Ok(local_route_config(port, "codex", "gpt5.6-sol"));
    }
    if v.protocol == "openai" {
        // OpenAI 协议(GPT API / 各家 OpenAI 兼容网关)→ 同样走本地路由转发:
        // claude 说 Anthropic 协议发到 127.0.0.1:port/p/{id}, 路由实时查 store 里
        // 这家的 base_url + key, 翻译成 chat/completions 转发 —— cc-switch 式本地转发。
        if v.auth_token.trim().is_empty() {
            return Err("该供应商尚未配置 API Key, 请先在弹窗中填写".to_string());
        }
        if normalize_url(&v.base_url).is_empty() {
            return Err("该供应商尚未配置请求地址, 请先在弹窗中填写".to_string());
        }
        let port = crate::integrations::codex_proxy::ensure_running()?;
        let model = pinned_model_or_default(&v.settings_config);
        return Ok(local_route_config(port, &v.id, &model));
    }
    if v.kind == "official" {
        return Ok(json!({ "env": {} }));
    }
    if v.auth_token.trim().is_empty() {
        return Err("该供应商尚未配置 API Key, 请先在弹窗中填写".to_string());
    }
    if route_all && !normalize_url(&v.base_url).is_empty() {
        // 本地路由总开关: Anthropic 兼容供应商也统一走 /p/{id} 透传。base 指到本地路由,
        // key 从 env 里拿掉换占位(真 key 由路由每请求实时注入 —— 改 key 即刻生效,
        // 且不再进子进程 env); 模型钉选等其余 env 原样保留。
        let port = crate::integrations::codex_proxy::ensure_running()?;
        let mut cfg = v.settings_config.clone();
        if !cfg.is_object() {
            cfg = json!({});
        }
        let obj = cfg.as_object_mut().unwrap();
        let env = obj.entry("env".to_string()).or_insert_with(|| json!({}));
        if !env.is_object() {
            *env = json!({});
        }
        let env = env.as_object_mut().unwrap();
        env.insert(
            "ANTHROPIC_BASE_URL".into(),
            Value::String(format!("http://127.0.0.1:{port}/p/{}", v.id)),
        );
        env.remove("ANTHROPIC_AUTH_TOKEN");
        env.remove("ANTHROPIC_API_KEY");
        let field = if v.token_field.is_empty() {
            DEFAULT_TOKEN_FIELD
        } else {
            v.token_field.as_str()
        };
        env.insert(field.into(), Value::String(LOCAL_ROUTE_TOKEN.into()));
        return Ok(cfg);
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
    // 解析待生效 env(codex / OpenAI 协议 / 路由总开关下会确保本地路由在跑)。
    // 未配 key / 未授权 → 回落全局当前, 不阻断对话。
    let cfg = match cfg_for_view(v, store.route_local) {
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

    let cfg = cfg_for_view(v, store.route_local)?;
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
            if let Ok(cfg) = cfg_for_view(v, store.route_local) {
                apply_settings_config(&cfg)?;
                apply_process_env(&cfg);
            }
        }
    } else {
        // 关联动(转隔离): 全局退回官方(只清我们的受管键, 其余原样保留),
        // Polaris 自身改用进程 env 维持当前选择 —— 终端立刻恢复干净。
        apply_settings_config(&json!({ "env": {} }))?;
        if let Some(v) = cur {
            if let Ok(cfg) = cfg_for_view(v, store.route_local) {
                apply_process_env(&cfg);
            }
        }
    }
    Ok(link)
}

/// 本地路由总开关(cc-switch 式代理模式)。开 = 当前及后续切换的 Anthropic 兼容供应商
/// 统一改走 127.0.0.1 本地路由透传(热切换: 改 key 即刻生效); 关 = 回到直连。
/// 切换开关立即对「当前供应商」重新生效, 联动模式下全局 settings.json 同步校正。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn provider_set_route_mode(route: bool) -> Result<bool, String> {
    STORE.write().route_local = route;
    persist();

    let store = STORE.read().clone();
    let views = build_views(&store);
    if let Some(v) = views.iter().find(|v| v.id == store.current_id) {
        if let Ok(cfg) = cfg_for_view(v, store.route_local) {
            if store.link_global {
                apply_settings_config(&cfg)?;
            }
            apply_process_env(&cfg);
        }
    }
    Ok(route)
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

    // 协议只认 "openai", 其余一律归一成 ""(Anthropic 兼容), 防前端传来脏值。
    let protocol = match input.protocol.as_deref().map(str::trim) {
        Some("openai") => "openai".to_string(),
        _ => String::new(),
    };

    let item = StoredProvider {
        id: id.clone(),
        name: input.name.trim().to_string(),
        note: input.note.trim().to_string(),
        website_url: normalize_url(&input.website_url),
        token_field,
        protocol,
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

#[cfg(test)]
mod volc_ark_tests {
    use super::*;

    #[test]
    fn ark_host_takes_configured_host_but_never_the_local_router() {
        // 正常:照抄用户配的主机名(国内 / 海外两套站点都要认)
        assert_eq!(
            ark_host("https://ark.cn-beijing.volces.com/api/coding", "agentplan"),
            "ark.cn-beijing.volces.com"
        );
        assert_eq!(
            ark_host("https://ark.ap-southeast.bytepluses.com/api/coding", "byteplus"),
            "ark.ap-southeast.bytepluses.com"
        );
        // 开过「本地路由」(或旧配置残留)时 base_url 指向回环 —— 照抄会拼出
        // https://127.0.0.1:port/api/v3/...(在 http 端口上说 TLS), 生图/转写双双打空炮。
        for b in [
            "http://127.0.0.1:8791/p/agentplan",
            "http://localhost:8791/p/agentplan",
            "http://0.0.0.0:8791/p/agentplan",
        ] {
            assert_eq!(
                ark_host(b, "agentplan"),
                "ark.cn-beijing.volces.com",
                "回环地址必须回落到真站点: {b}"
            );
        }
        // 空 / 含变量 → 按 id 回默认站点
        assert_eq!(ark_host("", "agentplan"), "ark.cn-beijing.volces.com");
        assert_eq!(ark_host("", "byteplus"), "ark.ap-southeast.bytepluses.com");
        assert_eq!(
            ark_host("https://${ARK_HOST}/api", "agentplan"),
            "ark.cn-beijing.volces.com"
        );
    }

    #[test]
    fn image_endpoint_uses_v3_not_the_chat_plan_prefix() {
        // base_url 的 `/api/coding` 是**聊天套餐**前缀, 生图必须走 /api/v3 —— 照抄就是 404。
        let host = ark_host("https://ark.cn-beijing.volces.com/api/coding", "agentplan");
        let ep = format!("https://{host}/api/v3/images/generations");
        assert!(!ep.contains("/api/coding"), "不该把聊天套餐前缀带进生图地址: {ep}");
        assert!(ep.ends_with("/api/v3/images/generations"), "{ep}");
    }

    #[test]
    fn local_route_placeholder_token_is_not_a_real_key() {
        // 占位串被当成真 key 借出去的话, 打上游必 401, 而且会把整条借用链堵死
        // (火山优先 → MiniMax 永远轮不到)。这里钉住常量, 防它跟 local_route_config 走散。
        assert_eq!(LOCAL_ROUTE_TOKEN, "polaris-local-router");
        let cfg = local_route_config(8791, "agentplan", "m");
        assert_eq!(
            cfg_env_str(&cfg, DEFAULT_TOKEN_FIELD),
            LOCAL_ROUTE_TOKEN,
            "本地路由写进去的占位串必须与借 key 侧判定的常量是同一个"
        );
    }
}

#[cfg(test)]
mod local_route_tests {
    use super::*;

    fn save_openai(id: &str, cfg: Value) {
        provider_save(ProviderInput {
            id: Some(id.to_string()),
            name: format!("测试 {id}"),
            note: String::new(),
            website_url: String::new(),
            token_field: None,
            protocol: Some("openai".into()),
            settings_config: cfg,
        })
        .expect("保存应成功");
    }

    /// OpenAI 协议供应商: route_target 解析出上游三元组(尾斜杠归一 + 模型钉选),
    /// cfg_for_view 生成指向本地路由 /p/{id} 的配置 —— 这是本地转发热切换的两半。
    #[test]
    fn openai_provider_routes_via_local_router() {
        save_openai(
            "t-openai-route",
            json!({"env": {
                "ANTHROPIC_BASE_URL": "https://gw.example.com/v1/",
                "ANTHROPIC_AUTH_TOKEN": "sk-test",
                "ANTHROPIC_MODEL": "gpt-5.5-mini",
            }}),
        );

        match route_target("t-openai-route").expect("应解析成功") {
            RouteTarget::OpenAiCompat {
                base_url,
                api_key,
                model,
            } => {
                assert_eq!(base_url, "https://gw.example.com/v1");
                assert_eq!(api_key, "sk-test");
                assert_eq!(model, "gpt-5.5-mini");
            }
            _ => panic!("应是 OpenAiCompat"),
        }

        let store = STORE.read().clone();
        let views = build_views(&store);
        let v = views
            .iter()
            .find(|v| v.id == "t-openai-route")
            .expect("视图应在");
        assert_eq!(v.protocol, "openai");
        let cfg = cfg_for_view(v, false).expect("cfg 应生成(顺带拉起本地路由)");
        let base = cfg_env_str(&cfg, "ANTHROPIC_BASE_URL");
        assert!(
            base.starts_with("http://127.0.0.1:"),
            "应指向本地路由: {base}"
        );
        assert!(base.ends_with("/p/t-openai-route"), "应带 /p/{{id}} 前缀: {base}");
        for k in MODEL_ENV_KEYS {
            assert_eq!(cfg_env_str(&cfg, k), "gpt-5.5-mini", "{k} 应钉到该家模型");
        }

        provider_delete("t-openai-route".to_string()).unwrap();
    }

    #[test]
    fn route_target_fallbacks_and_errors() {
        // 空 id / codex → ChatGPT(兼容旧版裸端口 base_url)
        assert!(matches!(route_target(""), Ok(RouteTarget::Chatgpt)));
        assert!(matches!(route_target("codex"), Ok(RouteTarget::Chatgpt)));
        assert!(route_target("no-such-provider-xyz").is_err());

        // 没配 key 的 openai 供应商 → 拒绝路由
        save_openai(
            "t-openai-nokey",
            json!({"env": {"ANTHROPIC_BASE_URL": "https://gw.example.com"}}),
        );
        assert!(route_target("t-openai-nokey").is_err());

        // Anthropic 兼容供应商 → 透传目标(路由总开关下会被指到 /p/{id}, 目标始终可解析)
        provider_save(ProviderInput {
            id: Some("t-anthropic-direct".into()),
            name: "直连".into(),
            note: String::new(),
            website_url: String::new(),
            token_field: None,
            protocol: None,
            settings_config: json!({"env": {
                "ANTHROPIC_BASE_URL": "https://a.example.com",
                "ANTHROPIC_AUTH_TOKEN": "k",
            }}),
        })
        .unwrap();
        assert!(matches!(
            route_target("t-anthropic-direct"),
            Ok(RouteTarget::AnthropicCompat { .. })
        ));

        provider_delete("t-openai-nokey".to_string()).unwrap();
        provider_delete("t-anthropic-direct".to_string()).unwrap();
    }

    /// 本地路由总开关: Anthropic 兼容供应商 ①route_target 解析成 AnthropicCompat 透传目标;
    /// ②开关开 → cfg 改指 /p/{id}、真 key 摘出换占位、模型钉选保留; ③开关关 → 原配置直连。
    #[test]
    fn route_all_switch_reroutes_anthropic_provider() {
        provider_save(ProviderInput {
            id: Some("t-anthropic-routed".into()),
            name: "透传测试".into(),
            note: String::new(),
            website_url: String::new(),
            token_field: None,
            protocol: None,
            settings_config: json!({"env": {
                "ANTHROPIC_BASE_URL": "https://api.example-relay.com/anthropic",
                "ANTHROPIC_AUTH_TOKEN": "sk-real-key",
                "ANTHROPIC_MODEL": "SomeModel-X",
            }}),
        })
        .expect("保存应成功");

        match route_target("t-anthropic-routed").expect("应解析成功") {
            RouteTarget::AnthropicCompat {
                base_url,
                api_key,
                token_field,
            } => {
                assert_eq!(base_url, "https://api.example-relay.com/anthropic");
                assert_eq!(api_key, "sk-real-key");
                assert_eq!(token_field, DEFAULT_TOKEN_FIELD);
            }
            _ => panic!("应是 AnthropicCompat"),
        }

        let store = STORE.read().clone();
        let views = build_views(&store);
        let v = views
            .iter()
            .find(|v| v.id == "t-anthropic-routed")
            .expect("视图应在");

        // 开关开: 走本地路由, key 不落 env
        let routed = cfg_for_view(v, true).expect("路由 cfg 应生成");
        let base = cfg_env_str(&routed, "ANTHROPIC_BASE_URL");
        assert!(base.starts_with("http://127.0.0.1:"), "应指向本地路由: {base}");
        assert!(base.ends_with("/p/t-anthropic-routed"));
        assert_eq!(
            cfg_env_str(&routed, "ANTHROPIC_AUTH_TOKEN"),
            "polaris-local-router",
            "真 key 不应进 env"
        );
        assert_eq!(
            cfg_env_str(&routed, "ANTHROPIC_MODEL"),
            "SomeModel-X",
            "模型钉选应原样保留"
        );

        // 开关关: 原配置直连
        let direct = cfg_for_view(v, false).expect("直连 cfg 应生成");
        assert_eq!(
            cfg_env_str(&direct, "ANTHROPIC_BASE_URL"),
            "https://api.example-relay.com/anthropic"
        );
        assert_eq!(cfg_env_str(&direct, "ANTHROPIC_AUTH_TOKEN"), "sk-real-key");

        provider_delete("t-anthropic-routed".to_string()).unwrap();
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
        std::env::set_var(
            "ANTHROPIC_BASE_URL",
            "https://global-thirdparty.example/anthropic",
        );
        std::env::set_var("ANTHROPIC_AUTH_TOKEN", "global-token");

        let mut cmd = Command::new("claude");
        scope_child_claude_by_id(&mut cmd, Some("claude-official"));
        let ov = cmd_env_overrides(&cmd);

        // 受管键必须被显式移除(值为 None), 而不是放任继承全局那家
        for k in [
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_MODEL",
        ] {
            assert_eq!(
                ov.get(k),
                Some(&None),
                "受管键 {k} 应被 env_remove 顶掉继承值"
            );
        }
        // 官方档不套私有 CLAUDE_CONFIG_DIR(OAuth 凭据在共享 ~/.claude)
        assert!(
            !ov.contains_key("CLAUDE_CONFIG_DIR"),
            "官方不应改 CLAUDE_CONFIG_DIR"
        );
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
