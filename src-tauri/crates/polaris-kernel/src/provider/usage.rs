use super::*;

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

// command(async): WalkDir 全量遍历 ~/.claude/projects 并逐行解析 jsonl(一年后可达
// 数百 MB), 同步命令会钉住 UI 主线程(v1.5.2 同族问题)。属性形式挪到工作线程,
// fn 保持同步签名(server dispatch 直调不受影响)。
#[cfg_attr(feature = "desktop", tauri::command(async))]
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

    for entry in dirs
        .iter()
        .flat_map(|d| WalkDir::new(d).into_iter().flatten())
    {
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
            let Some(msg) = v.get("message") else {
                continue;
            };
            let Some(usage_v) = msg.get("usage") else {
                continue;
            };
            if let Some(mid) = msg.get("id").and_then(|x| x.as_str()) {
                if !seen.insert(mid.to_string()) {
                    continue;
                }
            }
            let u = Usage {
                input: usage_v
                    .get("input_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
                output: usage_v
                    .get("output_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
                cache_read: usage_v
                    .get("cache_read_input_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
                cache_creation: usage_v
                    .get("cache_creation_input_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
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
            DailyUsage {
                date,
                label,
                total,
                cost,
            }
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
// command(async): 同步 ureq 网络请求(超时 12s), 同步命令会把 UI 主线程钉住整个
// 请求时长(v1.5.2 同族问题)。属性形式挪到工作线程, fn 保持同步签名(server
// dispatch 直调不受影响)。
#[cfg_attr(feature = "desktop", tauri::command(async))]
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
            let host = if host.is_empty() {
                "api.moonshot.cn".to_string()
            } else {
                host
            };
            let url = format!("https://{host}/v1/users/me/balance");
            let j = balance_get_json(&url, &token)?;
            let d = j.get("data").cloned().unwrap_or(Value::Null);
            let avail = d
                .get("available_balance")
                .and_then(|x| x.as_f64())
                .unwrap_or(0.0);
            let voucher = d
                .get("voucher_balance")
                .and_then(|x| x.as_f64())
                .unwrap_or(0.0);
            let cash = d
                .get("cash_balance")
                .and_then(|x| x.as_f64())
                .unwrap_or(0.0);
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
                Err(e) => Ok(mk(
                    "error",
                    "校验失败",
                    &e,
                    "https://www.kimi.com/code/console",
                )),
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
            let cur = info
                .get("currency")
                .and_then(|x| x.as_str())
                .unwrap_or("CNY");
            let total = info
                .get("total_balance")
                .and_then(|x| x.as_str())
                .unwrap_or("0");
            let granted = info
                .get("granted_balance")
                .and_then(|x| x.as_str())
                .unwrap_or("0");
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
            let total = d
                .get("totalBalance")
                .and_then(|x| x.as_str())
                .unwrap_or("0");
            let charge = d
                .get("chargeBalance")
                .and_then(|x| x.as_str())
                .unwrap_or("0");
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
