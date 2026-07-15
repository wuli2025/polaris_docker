use super::*;

/// 检测用户消息是否包含创建 skill 的意图
pub fn detect_skill_creation_intent(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    let triggers = [
        "创建skill",
        "新建skill",
        "写skill",
        "做一个skill",
        "skill创建",
        "skill新建",
        "skill制作",
        "创建技能",
        "新建技能",
        "写技能",
    ];
    triggers.iter().any(|t| lower.contains(t))
}

/// 检测是否是"需要浏览器 / 网页自动化"的任务
pub fn detect_browser_intent(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    let triggers = [
        // URL / 英文
        "http://",
        "https://",
        "www.",
        "browser",
        "scrape",
        "scraping",
        "crawl",
        "playwright",
        "selenium",
        "puppeteer",
        "captcha",
        "cloudflare",
        // 中文
        "网页",
        "网站",
        "浏览器",
        "打开链接",
        "打开网址",
        "抓取",
        "爬取",
        "爬虫",
        "登录网",
        "网页截图",
        "网页自动化",
        "填表单",
        "网上下单",
        "自动化操作网页",
    ];
    triggers.iter().any(|t| lower.contains(t))
}

/// 检测是否是「高层、多步、需临场决策的网页自动化」任务 → 自动激活 browser-use 浏览器智能体
/// （它驱动 CloakBrowser）。与 detect_browser_intent 区分:那个覆盖一切网页动作(含简单单步截图/
/// 抓取,激活 cloak-browser);这个只命中「给目标让它自主跑多步流程」的智能体场景。两者可同时命中,
/// browser-use 的 prompt 本就以 CloakBrowser 为底层,不冲突。
pub fn detect_browser_use_intent(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    let triggers = [
        // 英文 · 直指本技能 / 智能体浏览
        "browser-use",
        "browser use",
        "browser agent",
        "autonomous browser",
        "web agent",
        // 中文 · 智能体 / 自主多步
        "浏览器智能体",
        "网页智能体",
        "自主浏览",
        "自主操作",
        "自动操作网页",
        "自动跑流程",
        "自动完成网页",
        "自动帮我在",
        "替我在网",
        "自动登录并",
        "自动下单",
        "自动预订",
        "自动填表并提交",
        "自动在网站",
        "帮我在网站上",
    ];
    triggers.iter().any(|t| lower.contains(t))
}

/// 检测是否是「做 PPT / 演示文稿」的任务。命中即自动激活 polaris-deck-studio
/// （自家高级引擎，覆盖传统 .pptx 与网页幻灯片；缺席才退回通用 pptx 技能），
/// 不再要求用户先去技能中心安装 / 在对话框点选 —— 这是「无法产出 PPT」的首要原因。
pub fn detect_pptx_intent(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    let triggers = [
        // 英文
        "ppt",
        "pptx",
        "powerpoint",
        "slide deck",
        "slides",
        "keynote",
        "presentation",
        // 中文
        "幻灯片",
        "演示文稿",
        "演示文档",
        "做个演示",
        "做一个演示",
        "做份演示",
        "汇报材料",
        "路演",
        "宣讲",
        "述职",
        "答辩",
    ];
    triggers.iter().any(|t| lower.contains(t))
}

/// 检测是否是「做网站 / 网页 / HTML 页面成品」的创作任务。命中即自动激活
/// polaris-web-studio（「网站生成」引擎：主题体系 + 高级动效 + 自包含单文件）——
/// 与已隐藏的「网站生成」面板同款引擎，现在全靠对话触发。
/// 与 detect_browser_intent（浏览/抓取，激活 cloak-browser）刻意区分：
/// 这里只收「做出一个网页成品」的创作短语，不收裸「网页/网站」—— 那多半是要打开/抓取。
pub fn detect_web_create_intent(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    let triggers = [
        // 中文 · 建站 / 做页
        "做个网站",
        "做一个网站",
        "做网站",
        "建个网站",
        "建一个网站",
        "搭个网站",
        "搭建网站",
        "搭一个网站",
        "建站",
        "生成网站",
        "写个网站",
        "帮我做网站",
        "做个网页",
        "做一个网页",
        "做网页",
        "生成网页",
        "写个网页",
        "做成网页",
        "落地页",
        "着陆页",
        "做个官网",
        "做官网",
        "建官网",
        "个人网站",
        "作品集网站",
        "宣传页",
        "介绍页",
        "单页网站",
        "展示页",
        // 中文 · HTML 成品（用户常直说「做成 html 文件」）
        "做个html",
        "做一个html",
        "生成html",
        "写个html",
        "写一个html",
        "做成html",
        "html文件",
        "html 文件",
        "html页面",
        "html 页面",
        "网页文件",
        // 中文 · HTML 高频错别字（htlm 几乎必是 html 的手滑，漏检整条链路静默失效）
        "htlm",
        // 中文 · 重设计/美化已有页面（走 taste.md T9 重设计协议；仍不收裸「改版/重新设计」——
        // 那可能改的是 logo/海报/App，只收明确指向网页/网站/页面的组合）
        "改版网站",
        "网站改版",
        "改版官网",
        "官网改版",
        "网页改版",
        "页面改版",
        "改版页面",
        "重构页面",
        "页面重构",
        "重新设计网站",
        "重新设计网页",
        "重新设计页面",
        "重新设计官网",
        "美化网页",
        "美化页面",
        "美化网站",
        // 英文
        "landing page",
        "make a website",
        "build a website",
        "create a website",
        "make a web page",
        "build a web page",
        "make a webpage",
        "build a webpage",
        "create a webpage",
        "html page",
        "single page site",
        "portfolio site",
        "redesign the site",
        "redesign the website",
        "redesign the page",
        "redesign my site",
        "redesign my website",
        "website redesign",
    ];
    triggers.iter().any(|t| lower.contains(t))
}

/// 检测是否是「生成图片 / 文生图 / AI 绘画」的任务（仅针对写实照片、AI 绘画类**位图**，
/// 不含图表 / 流程图 / SVG —— 那些可由代码生成，不受供应商生图能力限制）。
/// 命中即自动激活 image-gen 技能，让它先把「当前供应商能不能真的生图」讲清楚。
pub fn detect_image_intent(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    let triggers = [
        // 英文
        "generate an image",
        "generate image",
        "create an image",
        "make an image",
        "draw me",
        "text-to-image",
        "an illustration",
        "a poster",
        "ai art",
        // 中文 · 动词类
        "生图",
        "生成图片",
        "生成图像",
        "生成一张图",
        "生成一幅",
        "文生图",
        "ai 作图",
        "ai作图",
        "ai 画",
        "ai画",
        "画一张",
        "画一幅",
        "画个",
        "画张",
        "画幅",
        "帮我画",
        "给我画",
        "做张图",
        "做一张图",
        "做个图",
        "来张图",
        "出张图",
        // 中文 · 名词类（强烈暗示位图绘制）
        "配图",
        "海报",
        "插画",
        "插图",
        "封面图",
        "宣传图",
        "壁纸",
        "头像图",
    ];
    triggers.iter().any(|t| lower.contains(t))
}

/// 检测是否是「下载大文件」的任务。命中即自动激活 turbo-download 技能，
/// 把 aria2c 多连接分段下载的完整跨平台配方注入本轮——配合 always-on 的
/// 大文件下载公约(见 chat.rs download_convention)，让拉模型/数据集/镜像默认提速。
pub fn detect_download_intent(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    let triggers = [
        // 英文
        "download",
        "wget",
        "curl ",
        "aria2",
        "fetch the",
        ".tar.gz",
        ".tar.bz2",
        ".zip",
        ".iso",
        ".bin",
        ".gguf",
        ".safetensors",
        "torrent",
        // 中文
        "下载",
        "下个",
        "拉取",
        "拉一下",
        "拉个",
        "获取文件",
        "大文件",
        "压缩包",
        "镜像包",
        "数据集",
        "模型权重",
        "安装包",
        "离线包",
    ];
    triggers.iter().any(|t| lower.contains(t))
}

/// 检测是否是「请教毛主席」的任务（原对话框开关，现改为技能 + 意图自动激活：
/// 消息里写「请教毛主席」等说法即注入毛选式分析指令，无需任何按钮）。
pub fn detect_mao_consult_intent(prompt: &str) -> bool {
    let triggers = [
        "请教毛主席",
        "请教一下毛主席",
        "问问毛主席",
        "问一下毛主席",
        "毛主席怎么看",
        "毛主席会怎么",
        "以毛主席的视角",
        "用毛主席的视角",
        "从毛主席的视角",
        "用毛选分析",
        "毛选式分析",
    ];
    triggers.iter().any(|t| prompt.contains(t))
}

/// 按任务意图自动激活的 skill（不依赖用户在对话框点选）。可返回多个。
/// 创建技能意图 → skill-creator；网页/浏览器自动化 → cloak-browser；
/// 做 PPT → polaris-deck-studio（自家高级引擎）；做网站/网页/HTML → polaris-web-studio；
/// 生成图片 → image-gen；请教毛主席 → consult-mao。
/// 「演示工坊」「网站生成」两个 UI 入口已从侧栏隐藏，对话意图触发是它们现在的唯一入口。
pub fn auto_skills_for_intent(prompt: &str) -> Vec<(SkillMeta, String)> {
    let mut out = Vec::new();
    if detect_skill_creation_intent(prompt) {
        if let Some(s) = find("skill-creator") {
            out.push(s);
        }
    }
    if detect_mao_consult_intent(prompt) {
        // 只在已安装时注入(skill 随「毛主席」名人资料包一起装到用户技能目录;
        // 没装资料包就注入指令只会让模型对着空目录瞎找)
        if let Some(s) = find("consult-mao") {
            if s.0.installed {
                out.push(s);
            }
        }
    }
    if detect_browser_intent(prompt) {
        if let Some(s) = find("cloak-browser") {
            out.push(s);
        }
    }
    if detect_browser_use_intent(prompt) {
        if let Some(s) = find(BROWSER_USE_ID) {
            out.push(s);
        }
    }
    if detect_pptx_intent(prompt) {
        // 自家高级引擎优先: polaris-deck-studio(36 套主题 + 高级动效 + spec→原生可编辑
        // .pptx), 一套引擎同时覆盖「传统 PPT」与「网页 PPT」两种交付 —— 与已隐藏的
        // 「演示工坊」面板同款。deck 是启动时 seed 到用户目录、不在 catalog:首启落盘未完成
        // 时 find 会 miss,故 miss 即同步补种一次(seed 幂等,已种则秒返)再找 —— 保证注入的
        // SKILL.md 指向真实文件;仍找不到才退回通用 pptx 技能。
        let deck = find(DECK_ID).or_else(|| {
            seed_deck_studio_skill();
            find(DECK_ID)
        });
        if let Some(s) = deck.or_else(|| find("pptx")) {
            out.push(s);
        }
    }
    if detect_web_create_intent(prompt) {
        // 同 deck 处理首启竞态:web 线没有 pptx 那样的兜底,不补种就会静默失效。
        let web = find(WEB_ID).or_else(|| {
            seed_web_studio_skill();
            find(WEB_ID)
        });
        if let Some(s) = web {
            out.push(s);
        }
    }
    if detect_image_intent(prompt) {
        if let Some(s) = find("image-gen") {
            out.push(s);
        }
    }
    if detect_download_intent(prompt) {
        if let Some(s) = find("turbo-download") {
            out.push(s);
        }
    }
    if detect_dev_intent(prompt) {
        // 项目skill(开发七纪律合一卡): 命中开发类任务即注入,模型按小节自行取用
        if let Some(s) = find("project-skill") {
            out.push(s);
        }
    }
    out
}

/// 是否是「做项目/写代码」类任务。宁可漏报不误报:命中的都是明确的工程词,
/// 日常问答/创作/自媒体类消息不会带进纪律卡。裸 contains 有误报教训
/// (".go"⊂.gov.cn、"review"⊂preview、"崩溃"⊂心态崩溃),故通用词一律绑定工程语境,
/// 源码后缀走边界匹配。
pub fn detect_dev_intent(prompt: &str) -> bool {
    let p = prompt.to_lowercase();
    const HINTS: &[&str] = &[
        // 中文工程词(避开「上线/架构/接口/崩溃」这类日常高频歧义词)
        "写代码",
        "改代码",
        "代码库",
        "重构",
        "修 bug",
        "修bug",
        "报错",
        "编译",
        "单测",
        "测试用例",
        "跑测试",
        "发版",
        "代码迁移",
        "数据迁移",
        "依赖升级",
        "代码审查",
        "审代码",
        "闪退",
        "调试",
        "系统架构",
        "架构设计",
        "函数",
        // 英文/工具链词(git/cargo 绑定子命令,防 digit/legit/cargo pants)
        "bug",
        "debug",
        "refactor",
        "compile",
        "npm ",
        "pnpm ",
        "github",
        "git commit",
        "git push",
        "git pull",
        "git merge",
        "git rebase",
        "cargo build",
        "cargo test",
        "cargo check",
        "cargo run",
        "pull request",
        "code review",
        "unit test",
        "stack trace",
        "panic",
    ];
    if HINTS.iter().any(|h| p.contains(h)) {
        return true;
    }
    // 源码后缀:后一字符必须不是字母数字(或串尾),挡住 .gov.cn/.tsinghua/.rss 类误报
    const EXTS: &[&str] = &[".rs", ".ts", ".tsx", ".vue", ".py", ".go", ".java", ".sql"];
    EXTS.iter().any(|ext| contains_ext_token(&p, ext))
}

/// `ext` 作为完整后缀出现(后随非字母数字或行尾)才算命中。
fn contains_ext_token(p: &str, ext: &str) -> bool {
    let mut from = 0;
    while let Some(i) = p[from..].find(ext) {
        let end = from + i + ext.len();
        match p[end..].chars().next() {
            None => return true,
            Some(c) if !c.is_ascii_alphanumeric() => return true,
            _ => from = end,
        }
    }
    false
}

#[cfg(test)]
mod intent_tests {
    use super::*;

    // 「做网站/网页/HTML 成品」创作短语要命中 —— 这是隐藏工坊入口后的唯一触发路径
    #[test]
    fn web_create_intent_hits_creation_phrases() {
        assert!(detect_web_create_intent("帮我做个网站介绍我们的产品"));
        assert!(detect_web_create_intent("把这份文案做成HTML 页面"));
        assert!(detect_web_create_intent("给新品做一个落地页"));
        assert!(detect_web_create_intent(
            "please build a website for my studio"
        ));
    }

    // taste 融合新增：重设计/美化/高频错别字要命中；裸「改版/重新设计」（可能改 logo/App）仍不触发
    #[test]
    fn web_create_intent_hits_redesign_and_typos() {
        assert!(detect_web_create_intent("公司官网改版，风格要现代一点"));
        assert!(detect_web_create_intent("把这个宣传页面改版成深色风"));
        assert!(detect_web_create_intent("重新设计网页的导航和配色"));
        assert!(detect_web_create_intent("生成htlm文件到桌面"));
        assert!(detect_web_create_intent("can you redesign my website?"));
        assert!(!detect_web_create_intent("重新设计一下我们的 logo"));
        assert!(!detect_web_create_intent("这次改版加了哪些新功能"));
    }

    // 纯浏览/抓取语句不能误触发网站生成（那归 cloak-browser 管）
    #[test]
    fn web_create_intent_ignores_browsing() {
        assert!(!detect_web_create_intent(
            "打开网页 https://example.com 看看"
        ));
        assert!(!detect_web_create_intent("帮我抓取这个网站的数据"));
        assert!(!detect_web_create_intent("去官网下载最新安装包"));
    }

    // PPT 意图命中时应优先注入自家高级引擎 polaris-deck-studio(预装,必在)
    #[test]
    fn ppt_intent_activates_deck_studio() {
        let ids: Vec<String> = auto_skills_for_intent("帮我把这份稿子做成 PPT")
            .into_iter()
            .map(|(m, _)| m.id)
            .collect();
        assert!(ids.iter().any(|i| i == DECK_ID), "实际注入: {:?}", ids);
    }

    // 开发意图:工程消息命中、日常/自媒体消息不命中(误报=纪律卡污染闲聊)
    #[test]
    fn dev_intent_hits_engineering_only() {
        assert!(detect_dev_intent("这段代码报错了帮我看看"));
        assert!(detect_dev_intent("重构一下 retrieve.rs 的融合层"));
        assert!(detect_dev_intent("git commit 前先跑测试用例"));
        // 曾经的误报源:域名子串/preview/日常词
        assert!(!detect_dev_intent("帮我查 www.gov.cn 上的政策"));
        assert!(!detect_dev_intent("去 tsinghua.edu.cn 找找资料"));
        assert!(!detect_dev_intent("给我 preview 一下这篇文章"));
        assert!(!detect_dev_intent("我今天心态崩溃了"));
        assert!(!detect_dev_intent("新品上线的宣传文案怎么写"));
        assert!(!detect_dev_intent("画一张公司组织架构图"));
    }
}
