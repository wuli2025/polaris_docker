use super::*;

// ───────── 「网页演示视频」打包技能（ConardLi · Polaris 集成）─────────
// 这是个多文件技能包：安装时 git clone 原仓库整目录到用户技能目录，再叠加
// 三个 Polaris 助手脚本（这里编译期内嵌），最后拼出 skill.md。
pub(crate) const WVP_ID: &str = "web-video-presentation";
pub(crate) const WVP_REPO: &str = "https://github.com/ConardLi/garden-skills";
pub(crate) const WVP_ADDENDUM: &str =
    include_str!("../../../../src/templates/skills/web-video-presentation/addendum.md");
pub(crate) const WVP_MINIMAX_TTS: &str =
    include_str!("../../../../src/templates/skills/web-video-presentation/minimax-tts.mjs");
pub(crate) const WVP_SCAFFOLD: &str =
    include_str!("../../../../src/templates/skills/web-video-presentation/scaffold.mjs");
pub(crate) const WVP_BOOTSTRAP: &str =
    include_str!("../../../../src/templates/skills/web-video-presentation/bootstrap.mjs");

// ───────── 「课件视频工坊」多文件技能（Polaris 自研，编译期内嵌，启动落盘）─────────
// 支撑「生成课件类视频」UI 的基础设施技能：含 SKILL.md + 三个脚本 + 链路文档。
// 不像 WVP 走 git clone，这套全自研，所有文件编译期内嵌、启动时确保落到 ~/Polaris/skills，
// 以便：① 全新安装即可用；② 改了脚本后随 App 更新下发（靠 PVS_VERSION 比对覆盖）。
pub(crate) const PVS_ID: &str = "polaris-video-studio";
// 改动内嵌脚本/SKILL.md 后必须 +1，让已安装用户在下次启动时拿到更新。
pub(crate) const PVS_VERSION: &str = "4";
pub(crate) const PVS_SKILL_MD: &str =
    include_str!("../../../../src/templates/skills/polaris-video-studio/SKILL.md");
pub(crate) const PVS_MANIFEST: &str =
    include_str!("../../../../src/templates/skills/polaris-video-studio/manifest.json");
pub(crate) const PVS_INSTALL_DEPS: &str =
    include_str!("../../../../src/templates/skills/polaris-video-studio/scripts/install-deps.mjs");
pub(crate) const PVS_RUN: &str =
    include_str!("../../../../src/templates/skills/polaris-video-studio/scripts/run.mjs");
pub(crate) const PVS_RECORD: &str =
    include_str!("../../../../src/templates/skills/polaris-video-studio/scripts/pipeline/03-record.mjs");
pub(crate) const PVS_FIND_BROWSER: &str =
    include_str!("../../../../src/templates/skills/polaris-video-studio/scripts/find-browser.mjs");
pub(crate) const PVS_WORKFLOW: &str =
    include_str!("../../../../src/templates/skills/polaris-video-studio/references/WORKFLOW.md");

// ───────── 「演示工坊」多文件技能（PPT / 网页幻灯片，Polaris 自研，编译期内嵌，启动落盘）─────────
// 支撑「PPT 演示」「网页幻灯片」两个 UI 入口的基础设施技能：幻灯片引擎(base.css，来自
// open-design，MIT) + 17 套主题 + 自研 runtime.js + deck 模板 + PPTX 导出脚本 + SKILL.md。
// 与 PVS 同套路：全部编译期内嵌、启动时确保落到 ~/Polaris/skills（靠 DECK_VERSION 比对覆盖）。
pub(crate) const DECK_ID: &str = "polaris-deck-studio";
// 改动任一内嵌资源后必须 +1，让已安装用户下次启动拿到更新。
pub(crate) const DECK_VERSION: &str = "13";
pub(crate) const DECK_SKILL_MD: &str =
    include_str!("../../../../src/templates/skills/polaris-deck-studio/SKILL.md");
pub(crate) const DECK_DESIGN_MD: &str =
    include_str!("../../../../src/templates/skills/polaris-deck-studio/design.md");
pub(crate) const DECK_LICENSE: &str =
    include_str!("../../../../src/templates/skills/polaris-deck-studio/LICENSE");
pub(crate) const DECK_BASE_CSS: &str =
    include_str!("../../../../src/templates/skills/polaris-deck-studio/assets/base.css");
pub(crate) const DECK_THEMES_CSS: &str =
    include_str!("../../../../src/templates/skills/polaris-deck-studio/assets/themes.css");
pub(crate) const DECK_RUNTIME_JS: &str =
    include_str!("../../../../src/templates/skills/polaris-deck-studio/assets/runtime.js");
pub(crate) const DECK_MOTION_CSS: &str =
    include_str!("../../../../src/templates/skills/polaris-deck-studio/assets/motion.css");
pub(crate) const DECK_MOTION_JS: &str =
    include_str!("../../../../src/templates/skills/polaris-deck-studio/assets/motion.js");
pub(crate) const DECK_TEMPLATE: &str =
    include_str!("../../../../src/templates/skills/polaris-deck-studio/templates/deck.html");
pub(crate) const DECK_INSTALL_DEPS: &str =
    include_str!("../../../../src/templates/skills/polaris-deck-studio/scripts/install-deps.mjs");
pub(crate) const DECK_EXPORT_PPTX: &str =
    include_str!("../../../../src/templates/skills/polaris-deck-studio/scripts/export-pptx.mjs");
pub(crate) const DECK_FIND_BROWSER: &str =
    include_str!("../../../../src/templates/skills/polaris-deck-studio/scripts/find-browser.mjs");

// ───────── 设计师人格包（designers/，编译期内嵌，随 deck-studio 落盘）─────────
// 「选设计师」体系：11 位设计师人格 + 美学地基(_foundation) + 总索引(INDEX.md)。
// auto 模式按 INDEX.md 路由表按内容气质选人；用户指定则用指定的。网站生成复用同一份包。
pub(crate) const DECK_DESIGNERS: &[(&str, &str)] = &[
    (
        "INDEX.md",
        include_str!("../../../../src/templates/skills/polaris-deck-studio/designers/INDEX.md"),
    ),
    (
        "_foundation/aesthetics.md",
        include_str!("../../../../src/templates/skills/polaris-deck-studio/designers/_foundation/aesthetics.md"),
    ),
    (
        "_foundation/rubric.md",
        include_str!("../../../../src/templates/skills/polaris-deck-studio/designers/_foundation/rubric.md"),
    ),
    (
        "_foundation/taste.md",
        include_str!("../../../../src/templates/skills/polaris-deck-studio/designers/_foundation/taste.md"),
    ),
    (
        "bento-grid.md",
        include_str!("../../../../src/templates/skills/polaris-deck-studio/designers/bento-grid.md"),
    ),
    (
        "clay-soft.md",
        include_str!("../../../../src/templates/skills/polaris-deck-studio/designers/clay-soft.md"),
    ),
    (
        "doodle-hand.md",
        include_str!("../../../../src/templates/skills/polaris-deck-studio/designers/doodle-hand.md"),
    ),
    (
        "glass-crisp.md",
        include_str!("../../../../src/templates/skills/polaris-deck-studio/designers/glass-crisp.md"),
    ),
    (
        "keynote-tech.md",
        include_str!("../../../../src/templates/skills/polaris-deck-studio/designers/keynote-tech.md"),
    ),
    (
        "memphis-pop.md",
        include_str!("../../../../src/templates/skills/polaris-deck-studio/designers/memphis-pop.md"),
    ),
    (
        "mist-gradient.md",
        include_str!("../../../../src/templates/skills/polaris-deck-studio/designers/mist-gradient.md"),
    ),
    (
        "oriental-grandeur.md",
        include_str!("../../../../src/templates/skills/polaris-deck-studio/designers/oriental-grandeur.md"),
    ),
    (
        "pedagogy-clarity.md",
        include_str!("../../../../src/templates/skills/polaris-deck-studio/designers/pedagogy-clarity.md"),
    ),
    (
        "swiss-modernist.md",
        include_str!("../../../../src/templates/skills/polaris-deck-studio/designers/swiss-modernist.md"),
    ),
    (
        "xhs-life.md",
        include_str!("../../../../src/templates/skills/polaris-deck-studio/designers/xhs-life.md"),
    ),
];

/// 把设计师人格包写到 <dest>/designers/（含 _foundation 子目录）。deck-studio 与 web-studio 共用。
pub(crate) fn write_designers(dest: &Path) -> Result<(), String> {
    let designers = dest.join("designers");
    fs::create_dir_all(designers.join("_foundation")).map_err(|e| e.to_string())?;
    // 先剪除已裁撤的旧人格文件：write 只覆盖不删除，裁掉的设计师 .md 会残留在旧安装里，
    // 让 auto 路由仍能选到「不存在于花名册」的幽灵设计师。只扫顶层 *.md（_foundation 在子目录，
    // read_dir 非递归，不受影响），凡不在 DECK_DESIGNERS 白名单里的一律删。
    let keep: std::collections::HashSet<&str> =
        DECK_DESIGNERS.iter().map(|(rel, _)| *rel).collect();
    if let Ok(entries) = fs::read_dir(&designers) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("md") {
                if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                    if !keep.contains(name) {
                        let _ = fs::remove_file(&p);
                    }
                }
            }
        }
    }
    for (rel, content) in DECK_DESIGNERS {
        fs::write(designers.join(rel), content).map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ───────── 「网站生成」技能（落地页/单页站点，Polaris 自研，编译期内嵌，启动落盘）─────────
// 支撑「网站生成」UI 入口。复用 deck-studio 的 17 套主题（DECK_THEMES_CSS，不重复源文件），
// 配一套网站组件 site.css + 滚动揭示 runtime.js + 站点模板 + SKILL.md。
pub(crate) const WEB_ID: &str = "polaris-web-studio";
pub(crate) const WEB_VERSION: &str = "6";
pub(crate) const WEB_SKILL_MD: &str =
    include_str!("../../../../src/templates/skills/polaris-web-studio/SKILL.md");
pub(crate) const WEB_LICENSE: &str = include_str!("../../../../src/templates/skills/polaris-web-studio/LICENSE");
pub(crate) const WEB_SITE_CSS: &str =
    include_str!("../../../../src/templates/skills/polaris-web-studio/assets/site.css");
pub(crate) const WEB_RUNTIME_JS: &str =
    include_str!("../../../../src/templates/skills/polaris-web-studio/assets/runtime.js");
pub(crate) const WEB_MOTION_CSS: &str =
    include_str!("../../../../src/templates/skills/polaris-web-studio/assets/motion.css");
pub(crate) const WEB_MOTION_JS: &str =
    include_str!("../../../../src/templates/skills/polaris-web-studio/assets/motion.js");
pub(crate) const WEB_TEMPLATE: &str =
    include_str!("../../../../src/templates/skills/polaris-web-studio/templates/site.html");

// ───────── 「壹伴排版优化」多文件技能（公众号排版，编译期内嵌，启动落盘）─────────
// 升级成多文件：SKILL.md（只产语义正文）+ scripts/wechat_yiban.py（壹伴样式引擎 + CloakBrowser
// 驱动）。和 deck/video studio 同套路：编译期内嵌、启动确保落到 ~/Polaris/skills（靠版本号比对覆盖），
// 这样脚本能被 spawn 的 claude agent 在磁盘上直接 `python …/wechat_yiban.py` 执行。
pub(crate) const WECHAT_TS_ID: &str = "wechat-md-typesetter";
// 改动 SKILL.md 或 wechat_yiban.py 后必须 +1，让已安装用户下次启动拿到更新。
// v2：修真机测出的两个 bug —— cloakbrowser API 是 headless= 非 headed=；render 改 about:blank+evaluate 避开 set_content 等 "load" 超时。
// v3：publish 健壮性 —— 跨「所有标签页×所有 frame」找编辑器（写图文常开新标签）；登录后自动点一次「写图文」入口；
//     兜底 render 复用已开 ctx 开新标签（不再另起同步 Playwright→消除 'Sync API inside asyncio loop' 崩溃）。
// v4：按真机 dump 校 SELECTORS —— editor_body 加新版 ProseMirror；title_input 改专指文章标题(避开草稿箱搜索框)；
//     new_article_entry 精确点「写图文」。注：editor_body 仍需真机在编辑器打开态确认一次。
// v5：两段解耦根治「老卡在上传」—— 注入改走「粘贴通道」(合成 ClipboardEvent，走 ProseMirror 事务，
//     和真壹伴/135editor 同路；innerHTML 硬塞会被 PM 清掉/不入草稿数据)，配三级降级+字数校验+保存回执；
//     publish 拆两段(先纯文字稳传入草稿，再套样式，样式挂了文字也已落地)；新增 --text-only 与
//     restyle 模式(对已存草稿原地换主题，normalize 后幂等不叠样式)；--timeout 可调(默认 300s)。
// v6：panel 模式(壹伴插件形态)——编辑器页面右侧注入可视化面板：7 套主题模板点选换肤 +「AI 改风格」
//     大白话定制(expose_function 桥→python→claude CLI 生成主题 JSON)+清除样式+保存草稿；
//     STYLIZE 支持主题对象/bg 整体背景/overrides 微调/plain 素颜，新增清新绿/活力橙/米纸预设。
// v7：按真机反馈三修——①背景改不了：bg 从「包 section」改「按块铺设」(每块自带 background 内联,
//     编辑器剥不掉,135editor 同法)；②换肤改「原地直改活 DOM」优先(像浏览器插件改 HTML),粘贴通道
//     降为兜底；③AI 用不了：expose_function 桥换成页面变量轮询握手(__polarisAI.pending↔__polarisAIResult)；
//     模板升到 8 套×6 种标题形态(h2Mode)，新增黛青。
// v8：长图链路(用户拍板的新路线,根治编辑器改字数/清样式)——snapshot=成品 HTML→全页截长图(@2x)
//     →段落空隙切片(下钻单子链找段落层;clip 配 full_page=True 才能裁视口外)+manifest;
//     publish-image=切片转 dataURL→File→合成 paste 贴进编辑器(原生欢迎零清洗)→等 img 落位/
//     换 mmbiz 外链→真文字导语(--intro)→填标题→存草稿。MediaOps 加「长图模式」开关(__longimg,
//     隐式带上本技能)。snapshot 已本地实测(单/多切片+段落切点目检);publish-image 待真机。
// v9：耗时根治+保存前移——snapshot 整页只成像一次(Pillow 本地裁切,缺 Pillow 退回逐段截图);
//     publish-image 贴图改「落位即贴下一张」,换 mmbiz 外链统一收尾等一轮(此前逐张串行死等
//     +累计判定级联烧满超时,是「传半天」的根因);file-input 通道缓存有效 input;
//     **填标题+首次保存挪到贴图之前**(草稿条目先进草稿箱——此前全贴完才保存,进程中途被
//     上层超时杀掉→保存从未执行→草稿箱空,用户实证);保存等回执期间点掉挡路确认弹窗。
// v10：按用户反馈调长图观感——①字号/行高/段距整体调大放疏(THEMES size→17/17.5,lh→~2.0,
//     h1/h2/h3 与段落间距全加大),长图不再「太紧凑」;②snapshot 改铺满整幅+主题色画布
//     (无 bg 主题给暖纸色 #f7f5ef),弃用 render 那层 max-width:677 居中外壳→**杜绝白边**;
//     ③默认截一整张(--no-slice;SLICE_MAX_CSS 2800→12000);④正文铁律加「开头不写摘要/导语」
//     +「h2 加 emoji 图标、关键句做 blockquote 卡片」营造配图感(SKILL.md 同步)。
//     MediaOps 交付改两个功能键二选一:「排版 HTML 文件」(render 出文件自传) /「截图上传」(长图)。
// v11：小红书交付对齐公众号——新增 --mode cards(自包含图卡 HTML 逐卡元素截图 @2x→PNG×N+manifest,
//     卡约定 <section class="card"> 3:4 竖版 1080×1440,样式上游自己写不套主题);MediaOps 小红书
//     交付改两个功能键二选一:「图卡截图」(__cardimg,默认)/「排版 HTML 文件」(__htmlfile),
//     planPrompt 补小红书交付分支;xiaohongshu-pipeline.md 渲染/投递步同步。
pub(crate) const WECHAT_TS_VERSION: &str = "11";
pub(crate) const WECHAT_TS_SKILL_MD: &str =
    include_str!("../../../../src/templates/skills/wechat-md-typesetter/SKILL.md");
pub(crate) const WECHAT_TS_YIBAN_PY: &str =
    include_str!("../../../../src/templates/skills/wechat-md-typesetter/scripts/wechat_yiban.py");

// ───────── 「极速下载」多文件技能（aria2c 多连接下载器，编译期内嵌，启动落盘）─────────
// 跨平台大文件下载器：SKILL.md + scripts/fast_download.py（纯 stdlib，自动装 aria2 + 优雅回退）
// + references/aria2_flags.md（L3 参数速查）。和 deck/video studio 同套路：编译期内嵌、启动确保
// 落到 ~/Polaris/skills，让 spawn 的 claude agent 能直接 `uv run …/fast_download.py` 执行。
pub(crate) const TURBO_ID: &str = "turbo-download";
// 改动 SKILL.md / fast_download.py / 参数表后必须 +1，让已安装用户下次启动拿到更新。
pub(crate) const TURBO_VERSION: &str = "1";
pub(crate) const TURBO_SKILL_MD: &str = include_str!("../../../../src/templates/skills/turbo-download/SKILL.md");
pub(crate) const TURBO_FAST_DL: &str =
    include_str!("../../../../src/templates/skills/turbo-download/scripts/fast_download.py");
pub(crate) const TURBO_FLAGS_MD: &str =
    include_str!("../../../../src/templates/skills/turbo-download/references/aria2_flags.md");

// ───────── 「浏览器智能体 browser-use」多文件技能（驱动 CloakBrowser，编译期内嵌，启动落盘）─────────
// 高层多步网页自动化:给一句目标,browser-use 智能体自跑「看页面→决策→操作」循环。底层浏览器
// 强制走 CloakBrowser 的隐身 Chromium(经 CDP 连接,绝不用 browser-use 自带浏览器 —— 用户硬规矩)。
// 和 deck/turbo 同套路:编译期内嵌、启动确保落到 ~/Polaris/skills(版本号比对覆盖),让 spawn 的
// claude agent 能直接 `uv run …/browser_use_runner.py` 跑它。
pub(crate) const BROWSER_USE_ID: &str = "browser-use";
// 改动 SKILL.md / browser_use_runner.py 后必须 +1,让已安装用户下次启动拿到更新。
pub(crate) const BROWSER_USE_VERSION: &str = "1";
pub(crate) const BROWSER_USE_SKILL_MD: &str =
    include_str!("../../../../src/templates/skills/browser-use/SKILL.md");
pub(crate) const BROWSER_USE_RUNNER: &str =
    include_str!("../../../../src/templates/skills/browser-use/scripts/browser_use_runner.py");

// ───────── 「微信聊天 · 每日待办」多文件技能（本地解密微信→挖待办→写晨报，编译期内嵌，启动落盘）─────────
// SKILL.md + scripts/wx_daily.py（每日：复用缓存 master key 解密导出→挖「你回过话且最近几天别人
// 发来未回」的消息→写进 kb_root/memory/briefing 晨报）+ scripts/wx_setup.py（一次性 hook 抓 key）
// + scripts/wx_config.example.json。脚本纯 stdlib，解密导出 shell 到 wechat-decrypt 的 venv。
// 和其它多文件技能同套路：编译期内嵌、启动确保落到 ~/Polaris/skills（版本号比对覆盖）。
pub(crate) const WECHAT_TASKS_ID: &str = "wechat-tasks";
// 改动 SKILL.md / wx_daily.py / wx_setup.py / wx_mac.py / find_all_keys_macos.c 后必须 +1，
// 让已安装用户下次启动拿到更新。v2：新增 macOS 支持（内存扫描抓 key + 自包含解密导出）。
pub(crate) const WECHAT_TASKS_VERSION: &str = "2";
pub(crate) const WECHAT_TASKS_SKILL_MD: &str =
    include_str!("../../../../src/templates/skills/wechat-tasks/SKILL.md");
pub(crate) const WECHAT_TASKS_DAILY_PY: &str =
    include_str!("../../../../src/templates/skills/wechat-tasks/scripts/wx_daily.py");
pub(crate) const WECHAT_TASKS_SETUP_PY: &str =
    include_str!("../../../../src/templates/skills/wechat-tasks/scripts/wx_setup.py");
pub(crate) const WECHAT_TASKS_CONFIG_EXAMPLE: &str =
    include_str!("../../../../src/templates/skills/wechat-tasks/scripts/wx_config.example.json");
// macOS 专属：自包含解密/导出流程模块 + Mach VM 内存抓 key 扫描器（源码，首次运行 cc 编译）。
pub(crate) const WECHAT_TASKS_MAC_PY: &str =
    include_str!("../../../../src/templates/skills/wechat-tasks/scripts/wx_mac.py");
pub(crate) const WECHAT_TASKS_MAC_SCANNER_C: &str =
    include_str!("../../../../src/templates/skills/wechat-tasks/scripts/mac/find_all_keys_macos.c");

// ───────── 「项目检测」多文件技能(协作检查闸的默认检查项,编译期内嵌,启动落盘)─────────
// SKILL.md(声明 check_entry_* / check_timeout_secs 检查协议)+ scripts/check.ps1 + check.sh。
// 检查闸(collab/checks.rs)按协议执行入口脚本,退出码 0=pass 非 0=fail —— 确定性脚本,
// AI 不进判定路径。项目未指定 check_skill 时默认用它;团队可复制改造出自己的检查技能。
pub const PROJECT_CHECK_ID: &str = "project-check-default";
// 改动 SKILL.md / check.ps1 / check.sh 后必须 +1,让已安装用户下次启动拿到更新。
pub(crate) const PROJECT_CHECK_VERSION: &str = "1";
pub(crate) const PROJECT_CHECK_SKILL_MD: &str =
    include_str!("../../../../src/templates/skills/project-check/SKILL.md");
pub(crate) const PROJECT_CHECK_PS1: &str =
    include_str!("../../../../src/templates/skills/project-check/check.ps1");
pub(crate) const PROJECT_CHECK_SH: &str =
    include_str!("../../../../src/templates/skills/project-check/check.sh");
