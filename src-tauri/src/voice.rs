//! 语音输入模块「极速说」· 防污染引擎 + 配置 + 个人词表
//!
//! 设计出处:桌面《Polaris-语音输入模块-PRD-v3.html》§7「防污染·双档」+ §8 设置形态。
//!
//! 本文件落地 PRD 的「纯逻辑」半边(零重型原生依赖、可单测):
//!   - 配置:激活方式 / 引擎 / 防污染档位 / 流畅模式 / 润色 / 改键(`~/Polaris/data/voice.json`)
//!   - 个人词表:hotwords / corrections / weights(`~/Polaris/data/voice_lexicon.json`)
//!   - 防污染·秒达档(默认):Layer1 热词清单(喂 sherpa hotwords)+ Layer2-lite 拼音对齐纠错
//!       · corrections 精确表(跨脚本:扣带式→codex)
//!       · 拼音编辑距离模糊匹配(同脚本同音:北极心→北极星),纯本地 ~毫秒级
//!   - 防污染·重型档:在秒达基础上叠加 LLM 拼音+语义纠错(接供应商坞,后续阶段接入,
//!       当前先结构化降级为秒达,标记 heavy_pending)
//!   - 词表自学:从文本挖高频技术专名(mine_terms),供回声层「做梦」周期刷新
//!
//! 录音(cpal)/全局热键(rdev)/注入(enigo)/推理(sherpa-rs)等重型原生件是另一阶段,
//! 本文件不引入,以免破坏现有 build。与 sense.rs 同构:JSON 落盘、原子写、内置种子。

use directories::UserDirs;
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use pinyin::ToPinyin;

// AppHandle:桌面 = tauri,Docker = host shim(与 sense.rs 同策略)。仅实时语音命令用。
#[cfg(feature = "desktop")]
use tauri::AppHandle;
#[cfg(not(feature = "desktop"))]
use crate::host::AppHandle;

// ───────────────────────── 数据模型 ─────────────────────────

/// 语音输入配置。默认即「按住右 Alt 说话 + 本地 SenseVoice + 秒达防污染」。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceConfig {
    /// "hold"(按住说,默认) | "free"(自由说,双击开/再按关)
    pub activation: String,
    /// 激活键:"ralt"(默认) | "rctrl" | "capslock" | "f9" | "mouse_x2" ...
    pub hotkey: String,
    /// 识别引擎 = 感官坞服务商 id;默认本地 SenseVoice-Small。
    pub engine: String,
    /// 流畅模式:开 = 真流式 Paraformer(首次需下载),关 = SenseVoice 模拟流式。
    pub fluent_mode: bool,
    /// 说完 LLM 润色(去语气词/补标点);默认关,最快。
    pub polish: bool,
    /// 防污染档位:"off" | "lite"(秒达,默认) | "heavy"(重型)。
    pub antipollute: String,
    /// 拼音模糊纠错的音节编辑距离阈值(越大越激进越易误伤);默认 1。
    pub pinyin_threshold: u32,
    /// 浮窗位置:"bottom"(底部居中,默认) | "cursor"(跟随光标)。
    pub overlay_pos: String,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            activation: "hold".into(),
            hotkey: "ralt".into(),
            engine: "local-sensevoice".into(),
            fluent_mode: false,
            polish: false,
            antipollute: "lite".into(),
            pinyin_threshold: 1,
            overlay_pos: "bottom".into(),
        }
    }
}

/// 个人词表 —— 防污染的活字典,可手编、可从历史自学。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VoiceLexicon {
    /// 高频专名:Layer1 喂 sherpa hotwords + Layer2 拼音模糊匹配的回填目标。
    #[serde(default)]
    pub hotwords: Vec<String>,
    /// 歧义/同音错词 → 规范词(精确替换,跨脚本也能纠:扣带式→codex)。
    #[serde(default)]
    pub corrections: BTreeMap<String, String>,
    /// 词频权重(做梦统计更新;多个候选命中时取高权重)。
    #[serde(default)]
    pub weights: BTreeMap<String, u32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct VoiceStore {
    #[serde(default)]
    config: VoiceConfig,
    #[serde(default)]
    lexicon: VoiceLexicon,
}

static STORE: Lazy<RwLock<VoiceStore>> = Lazy::new(|| RwLock::new(VoiceStore::default()));

fn data_dir() -> Option<PathBuf> {
    UserDirs::new().map(|u| u.home_dir().join("Polaris").join("data"))
}
fn store_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join("voice.json"))
}

fn atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

fn persist() {
    let Some(path) = store_path() else { return };
    let txt = serde_json::to_string_pretty(&*STORE.read()).unwrap_or_default();
    let _ = atomic_write(&path, &txt);
}

// ───────────────────────── 种子词表(技术高频词)─────────────────────────
// PRD §12 待定问:「秒达档拼音库是否随安装包内置一份通用词」→ 内置一份 Polaris/技术种子,
// 让新用户开箱即能纠常见技术专名;hotwords 只做偏置(安全),corrections 只放最明确的几条。

fn seed_lexicon() -> VoiceLexicon {
    let hotwords: Vec<String> = [
        "Polaris", "北极星", "codex", "Claude", "forge", "fable", "Tauri", "Rust",
        "sherpa-onnx", "SenseVoice", "Paraformer", "FunASR", "群晖", "Docker",
        "Ollama", "通义千问", "MiniMax", "知识库", "感官坞", "检索枢纽",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    let mut corrections = BTreeMap::new();
    // 仅放最明确的同音/形近误写(来自 PRD 与用户实例),保守起步避免误伤。
    for (wrong, right) in [
        ("扣带式", "codex"),
        ("北极心", "北极星"),
        ("群辉", "群晖"),
        ("夏尔帕", "sherpa-onnx"),
        ("达克", "Docker"),
    ] {
        corrections.insert(wrong.to_string(), right.to_string());
    }

    let mut weights = BTreeMap::new();
    for w in &hotwords {
        weights.insert(w.clone(), 1);
    }
    VoiceLexicon { hotwords, corrections, weights }
}

/// 启动时调用:读盘;首次(无文件)写入种子词表。
pub fn init() {
    let loaded: Option<VoiceStore> = store_path()
        .filter(|p| p.exists())
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok());
    match loaded {
        Some(s) => {
            *STORE.write() = s;
        }
        None => {
            let mut s = VoiceStore::default();
            s.lexicon = seed_lexicon();
            *STORE.write() = s;
            persist();
        }
    }
}

// ───────────────────────── 拼音工具 ─────────────────────────

fn is_cjk(c: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&c) || ('\u{3400}'..='\u{4DBF}').contains(&c)
}

/// 纯 CJK 串 → 无调音节序列;含任一无法转拼音的 CJK 字则返回 None(只对纯 CJK 窗口模糊匹配)。
fn cjk_syllables(s: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() || !chars.iter().all(|c| is_cjk(*c)) {
        return None;
    }
    let mut out = Vec::with_capacity(chars.len());
    for py in s.to_pinyin() {
        match py {
            Some(p) => out.push(p.plain().to_string()),
            None => return None,
        }
    }
    if out.len() == chars.len() {
        Some(out)
    } else {
        None
    }
}

/// 音节序列编辑距离(Levenshtein,以「整段音节」为单位)。
fn syllable_lev(a: &[String], b: &[String]) -> usize {
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut cur = vec![0usize; m + 1];
    for i in 1..=n {
        cur[0] = i;
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[m]
}

// ───────────────────────── 防污染核心 ─────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct AntiChange {
    pub from: String,
    pub to: String,
    /// "exact"(corrections 表) | "pinyin"(拼音模糊)
    pub layer: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AntiPolluteResult {
    pub text: String,
    pub changes: Vec<AntiChange>,
    /// 实际生效档位:"off" | "lite" | "heavy"
    pub tier: String,
    /// 重型 LLM 纠错尚未接入供应商坞 → true 时仅秒达生效。
    pub heavy_pending: bool,
}

/// 识别结果(原文 + 防污染终稿 + 改动 + 耗时)。voice_asr 运行时填充。
#[derive(Debug, Clone, Serialize)]
pub struct TranscribeResult {
    /// ASR 原始转写
    pub raw: String,
    /// 防污染后的终稿
    pub text: String,
    pub changes: Vec<AntiChange>,
    pub tier: String,
    pub ms: u64,
}

/// 秒达档:corrections 精确替换 + 拼音模糊回填。纯本地,无网络。
pub fn anti_pollute_lite(text: &str, lex: &VoiceLexicon, threshold: u32) -> (String, Vec<AntiChange>) {
    let mut changes: Vec<AntiChange> = Vec::new();
    let mut out = text.to_string();

    // ── Layer2a:corrections 精确替换(长键优先,避免子串相互吞)──
    let mut pairs: Vec<(&String, &String)> = lex.corrections.iter().collect();
    pairs.sort_by_key(|(w, _)| std::cmp::Reverse(w.chars().count()));
    for (wrong, right) in pairs {
        if wrong.is_empty() || wrong == right {
            continue;
        }
        if out.contains(wrong.as_str()) {
            out = out.replace(wrong.as_str(), right);
            changes.push(AntiChange {
                from: wrong.clone(),
                to: right.clone(),
                layer: "exact".into(),
            });
        }
    }

    // ── Layer2b:拼音模糊匹配(只针对纯 CJK 热词,窗口同长等长回填)──
    // 候选热词:纯 CJK、≥2 字(避免单字误伤)、能取到音节;高权重优先。
    let mut cands: Vec<(Vec<char>, Vec<String>, u32)> = Vec::new();
    for hw in &lex.hotwords {
        let chars: Vec<char> = hw.chars().collect();
        if chars.len() < 2 {
            continue;
        }
        if let Some(syl) = cjk_syllables(hw) {
            let w = lex.weights.get(hw).copied().unwrap_or(1);
            cands.push((chars, syl, w));
        }
    }
    cands.sort_by_key(|(_, _, w)| std::cmp::Reverse(*w));

    let mut chars: Vec<char> = out.chars().collect();
    for (hw_chars, hw_syl, _) in &cands {
        let l = hw_chars.len();
        if l == 0 || l > chars.len() {
            continue;
        }
        let mut i = 0usize;
        while i + l <= chars.len() {
            let window: String = chars[i..i + l].iter().collect();
            // 已经就是该热词 → 跳过
            if window == hw_chars.iter().collect::<String>() {
                i += l;
                continue;
            }
            let mut matched = false;
            if let Some(win_syl) = cjk_syllables(&window) {
                let d = syllable_lev(&win_syl, hw_syl) as u32;
                if d <= threshold {
                    // 回填:等长替换,索引稳定
                    for (k, c) in hw_chars.iter().enumerate() {
                        chars[i + k] = *c;
                    }
                    changes.push(AntiChange {
                        from: window,
                        to: hw_chars.iter().collect(),
                        layer: "pinyin".into(),
                    });
                    matched = true;
                }
            }
            if matched {
                i += l;
            } else {
                i += 1;
            }
        }
    }
    out = chars.into_iter().collect();

    (out, changes)
}

/// 按当前配置档位对一段文本做防污染。重型 LLM 纠错为后续阶段(标 heavy_pending)。
pub fn anti_pollute(text: &str) -> AntiPolluteResult {
    let (tier, threshold, lex) = {
        let s = STORE.read();
        (s.config.antipollute.clone(), s.config.pinyin_threshold, s.lexicon.clone())
    };
    match tier.as_str() {
        "off" => AntiPolluteResult {
            text: text.to_string(),
            changes: vec![],
            tier: "off".into(),
            heavy_pending: false,
        },
        "heavy" => {
            // 重型 = 秒达 + LLM(待接供应商坞)。当前先跑秒达,标记 heavy_pending。
            let (out, changes) = anti_pollute_lite(text, &lex, threshold);
            AntiPolluteResult {
                text: out,
                changes,
                tier: "heavy".into(),
                heavy_pending: true,
            }
        }
        _ => {
            let (out, changes) = anti_pollute_lite(text, &lex, threshold);
            AntiPolluteResult {
                text: out,
                changes,
                tier: "lite".into(),
                heavy_pending: false,
            }
        }
    }
}

// ───────────────────────── 词表自学(mine_terms)─────────────────────────

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "you", "that", "this", "with", "are", "was", "but", "not",
    "have", "has", "from", "they", "what", "your", "our", "can", "all", "out", "get",
    "com", "www", "http", "https", "html", "json", "true", "false", "null",
];

/// 从文本挖高频技术专名(ASCII 技术词为主,CJK 专名抽取待接 jieba/做梦阶段)。
/// 返回 (词, 频次) 按频次降序。纯函数,可单测。
pub fn mine_terms(text: &str) -> Vec<(String, u32)> {
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, counts: &mut BTreeMap<String, u32>| {
        if cur.len() >= 3 {
            let lower = cur.to_lowercase();
            // 全数字 / 停用词跳过
            if !lower.chars().all(|c| c.is_ascii_digit()) && !STOPWORDS.contains(&lower.as_str()) {
                *counts.entry(cur.clone()).or_insert(0) += 1;
            }
        }
        cur.clear();
    };
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            cur.push(c);
        } else {
            flush(&mut cur, &mut counts);
        }
    }
    flush(&mut cur, &mut counts);

    let mut v: Vec<(String, u32)> = counts.into_iter().filter(|(_, n)| *n >= 1).collect();
    // 频次降序,同频按字母序稳定
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

// ───────────────────────── Tauri 命令 ─────────────────────────

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn voice_config_get() -> VoiceConfig {
    STORE.read().config.clone()
}

/// 改配置(只动传入字段)。
#[cfg_attr(feature = "desktop", tauri::command)]
#[allow(clippy::too_many_arguments)]
pub fn voice_config_set(
    activation: Option<String>,
    hotkey: Option<String>,
    engine: Option<String>,
    fluent_mode: Option<bool>,
    polish: Option<bool>,
    antipollute: Option<String>,
    pinyin_threshold: Option<u32>,
    overlay_pos: Option<String>,
) -> Result<VoiceConfig, String> {
    {
        let mut s = STORE.write();
        let c = &mut s.config;
        if let Some(v) = activation {
            if v == "hold" || v == "free" {
                c.activation = v;
            }
        }
        if let Some(v) = hotkey {
            if !v.trim().is_empty() {
                c.hotkey = v.trim().to_string();
            }
        }
        if let Some(v) = engine {
            if !v.trim().is_empty() {
                c.engine = v.trim().to_string();
            }
        }
        if let Some(v) = fluent_mode {
            c.fluent_mode = v;
        }
        if let Some(v) = polish {
            c.polish = v;
        }
        if let Some(v) = antipollute {
            if v == "off" || v == "lite" || v == "heavy" {
                c.antipollute = v;
            }
        }
        if let Some(v) = pinyin_threshold {
            c.pinyin_threshold = v.min(3);
        }
        if let Some(v) = overlay_pos {
            if v == "bottom" || v == "cursor" {
                c.overlay_pos = v;
            }
        }
    }
    persist();
    Ok(STORE.read().config.clone())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn voice_lexicon_get() -> VoiceLexicon {
    STORE.read().lexicon.clone()
}

/// 加热词(去重;权重起 1)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn voice_hotword_add(word: String) -> Result<VoiceLexicon, String> {
    let w = word.trim().to_string();
    if w.is_empty() {
        return Err("热词不能为空".into());
    }
    {
        let mut s = STORE.write();
        if !s.lexicon.hotwords.iter().any(|x| x == &w) {
            s.lexicon.hotwords.push(w.clone());
        }
        s.lexicon.weights.entry(w).or_insert(1);
    }
    persist();
    Ok(STORE.read().lexicon.clone())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn voice_hotword_remove(word: String) -> Result<VoiceLexicon, String> {
    {
        let mut s = STORE.write();
        s.lexicon.hotwords.retain(|x| x != &word);
        s.lexicon.weights.remove(&word);
    }
    persist();
    Ok(STORE.read().lexicon.clone())
}

/// 加/改一条纠错映射(错词 → 规范词)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn voice_correction_add(wrong: String, right: String) -> Result<VoiceLexicon, String> {
    let (wrong, right) = (wrong.trim().to_string(), right.trim().to_string());
    if wrong.is_empty() || right.is_empty() {
        return Err("错词与规范词都不能为空".into());
    }
    if wrong == right {
        return Err("错词与规范词相同".into());
    }
    {
        let mut s = STORE.write();
        s.lexicon.corrections.insert(wrong, right);
    }
    persist();
    Ok(STORE.read().lexicon.clone())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn voice_correction_remove(wrong: String) -> Result<VoiceLexicon, String> {
    {
        let mut s = STORE.write();
        s.lexicon.corrections.remove(&wrong);
    }
    persist();
    Ok(STORE.read().lexicon.clone())
}

/// 对一段文本试跑防污染(设置页「测一下」/ 实际上屏前都走它)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn voice_anti_pollute(text: String) -> AntiPolluteResult {
    anti_pollute(&text)
}

/// 识别一个音频文件(16k 单声道 wav)→ 防污染 → 终稿。
/// 命令恒注册(签名稳定);真识别需 `voice-asr` feature 编译 + 已下载 SenseVoice 模型。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn voice_transcribe_file(path: String) -> Result<TranscribeResult, String> {
    #[cfg(feature = "voice-asr")]
    {
        crate::voice_asr::transcribe_file(&path)
    }
    #[cfg(not(feature = "voice-asr"))]
    {
        let _ = path;
        Err("语音识别运行时未编译:用 `--features voice-asr` 构建即可启用本地 SenseVoice 识别".into())
    }
}

/// 启用实时语音输入(按住右 Alt 说话 → 流式上字 → 松手注入)。
/// 命令恒注册;真运行需 `voice-asr` + 桌面 + 已下载 SenseVoice 模型。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn voice_listen_start(app: AppHandle) -> Result<(), String> {
    #[cfg(feature = "voice-live")]
    {
        crate::voice_live::start(app)
    }
    #[cfg(not(feature = "voice-live"))]
    {
        let _ = app;
        Err("实时语音输入未编译:需以 `--features voice-live` 构建桌面版(Docker/浏览器走「上传音频识别」,见 voice_transcribe_file)".into())
    }
}

/// 停用实时语音输入(全局热键事件被忽略)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn voice_listen_stop() -> Result<(), String> {
    #[cfg(feature = "voice-live")]
    {
        crate::voice_live::stop();
    }
    Ok(())
}

/// 开始听写(输入框麦克风/右Alt 触发):录音转写,文字经 `voice:dictation` 事件回前端输入框。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn voice_dictate_start(app: AppHandle) -> Result<(), String> {
    #[cfg(feature = "voice-live")]
    {
        crate::voice_live::dictate_start(app)
    }
    #[cfg(not(feature = "voice-live"))]
    {
        let _ = app;
        Err("本机麦克风听写未编译(桌面 `--features voice-live`)。浏览器/Docker 请走上传音频识别:voice_transcribe_file".into())
    }
}

/// 停止听写 → 整段识别 + 防污染 → emit `voice:dictation { text }`。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn voice_dictate_stop() -> Result<(), String> {
    #[cfg(feature = "voice-live")]
    {
        crate::voice_live::dictate_stop();
    }
    Ok(())
}

/// 隐式学习:用户把识别错词手动改成对的 → 记进 corrections。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn voice_learn_correction(wrong: String, right: String) -> Result<VoiceLexicon, String> {
    voice_correction_add(wrong, right)
}

#[derive(Debug, Clone, Serialize)]
pub struct MinedTerm {
    pub term: String,
    pub count: u32,
}

/// 从给定文本挖高频专名并并入热词(做梦/手动「从历史学词」用)。
/// 传 top 限制并入数量;返回挖到的全部候选供前端展示。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn voice_lexicon_learn(text: String, top: Option<usize>) -> Result<Vec<MinedTerm>, String> {
    let mined = mine_terms(&text);
    let limit = top.unwrap_or(20).min(mined.len());
    {
        let mut s = STORE.write();
        for (term, count) in mined.iter().take(limit) {
            if !s.lexicon.hotwords.iter().any(|x| x == term) {
                s.lexicon.hotwords.push(term.clone());
            }
            let w = s.lexicon.weights.entry(term.clone()).or_insert(0);
            *w = w.saturating_add(*count);
        }
    }
    persist();
    Ok(mined
        .into_iter()
        .map(|(term, count)| MinedTerm { term, count })
        .collect())
}

// ───────────────────────── 单元测试 ─────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn lex_with(hotwords: &[&str], corrections: &[(&str, &str)]) -> VoiceLexicon {
        let mut l = VoiceLexicon::default();
        l.hotwords = hotwords.iter().map(|s| s.to_string()).collect();
        for (w, r) in corrections {
            l.corrections.insert(w.to_string(), r.to_string());
        }
        for h in &l.hotwords {
            l.weights.insert(h.clone(), 1);
        }
        l
    }

    #[test]
    fn exact_correction_cross_script() {
        let lex = lex_with(&[], &[("扣带式", "codex")]);
        let (out, ch) = anti_pollute_lite("把设置改成扣带式那种形态", &lex, 1);
        assert!(out.contains("codex"), "got: {out}");
        assert!(!out.contains("扣带式"));
        assert_eq!(ch.len(), 1);
        assert_eq!(ch[0].layer, "exact");
    }

    #[test]
    fn pinyin_fuzzy_homophone() {
        // 北极心(xin) → 北极星(xing):音节编辑距离 1
        let lex = lex_with(&["北极星"], &[]);
        let (out, ch) = anti_pollute_lite("我们用北极心这个名字", &lex, 1);
        assert!(out.contains("北极星"), "got: {out}");
        assert!(ch.iter().any(|c| c.layer == "pinyin" && c.to == "北极星"));
    }

    #[test]
    fn pinyin_threshold_blocks_far_words() {
        // 完全不同音的词不应被改成热词
        let lex = lex_with(&["北极星"], &[]);
        let (out, _) = anti_pollute_lite("今天天气很好啊", &lex, 1);
        assert_eq!(out, "今天天气很好啊");
    }

    #[test]
    fn no_change_when_already_correct() {
        let lex = lex_with(&["北极星"], &[]);
        let (out, ch) = anti_pollute_lite("北极星很亮", &lex, 1);
        assert_eq!(out, "北极星很亮");
        assert!(ch.is_empty());
    }

    #[test]
    fn single_char_hotword_never_fuzzy() {
        // 单字热词不参与模糊(避免大面积误伤)
        let lex = lex_with(&["星"], &[]);
        let (out, ch) = anti_pollute_lite("心心相印", &lex, 1);
        assert_eq!(out, "心心相印");
        assert!(ch.is_empty());
    }

    #[test]
    fn mine_terms_counts_and_filters() {
        let text = "Polaris uses sherpa-onnx and Polaris loves codex codex codex the the the";
        let mined = mine_terms(text);
        // codex 出现 3 次应排前;停用词 the/and 被过滤
        assert_eq!(mined[0].0.to_lowercase(), "codex");
        assert!(!mined.iter().any(|(t, _)| t == "the"));
        assert!(mined.iter().any(|(t, _)| t == "sherpa-onnx"));
    }

    #[test]
    fn syllable_lev_basic() {
        let a: Vec<String> = ["bei", "ji", "xin"].iter().map(|s| s.to_string()).collect();
        let b: Vec<String> = ["bei", "ji", "xing"].iter().map(|s| s.to_string()).collect();
        assert_eq!(syllable_lev(&a, &b), 1);
    }
}
