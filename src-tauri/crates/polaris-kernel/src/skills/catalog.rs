use super::*;

// ═══════════════════════════════════════════════════════════════
// 统一目录 Catalog（编译期，只读）
// ═══════════════════════════════════════════════════════════════

#[derive(Debug, Clone)]
pub struct CatalogSkill {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub source: &'static str, // official | third-party
    /// true = 预装（始终可用，无需安装），false = 市场技能（需点安装）
    pub preinstalled: bool,
    pub system_prompt: &'static str,
}

pub(crate) fn catalog() -> Vec<CatalogSkill> {
    vec![
        // ── 预装（开箱即用） ──
        CatalogSkill {
            id: "deep-research",
            name: "深度搜索",
            description: "使用 LLM 大规模联网搜索相关内容，自动检索、汇总、交叉验证多来源信息",
            source: "third-party",
            preinstalled: true,
            system_prompt: include_str!("../../../../src/templates/skills/deep-research.md"),
        },
        // ── 极速下载（预装、默认开启）：大文件 aria2c 多连接分段下载，意图命中即自动激活 ──
        CatalogSkill {
            id: TURBO_ID,
            name: "极速下载 TurboDownload",
            description: "下载大文件(>200MB)用自带跨平台 Python 脚本调 aria2c 多连接分段并行替代单线 wget，实测批量快 6.7 倍；自动按平台装 aria2、探测大小、优雅回退单线/curl，跨 Windows/Mac/Linux/群晖。拉模型/数据集/镜像/依赖包默认走它",
            source: "official",
            preinstalled: true,
            system_prompt: TURBO_SKILL_MD,
        },
        // ── 项目检测(预装):协作检查闸的默认检查技能,也是团队自定义检查的模板 ──
        CatalogSkill {
            id: PROJECT_CHECK_ID,
            name: "项目检测(协作检查)",
            description: "多人协作检查闸的默认项目检测:按仓库探测工具链(cargo check/npm lint·typecheck·build/ruff)跑确定性脚本,退出码定 pass/fail,AI 不参与判定。想自定义团队检查项,复制本技能改脚本即可",
            source: "official",
            preinstalled: true,
            system_prompt: PROJECT_CHECK_SKILL_MD,
        },
        // ── 名人资料包配套技能（随知识库「名人资料包」一起装/卸，不单独预装） ──
        CatalogSkill {
            id: "consult-mao",
            name: "请教毛主席",
            description: "化身毛主席，用毛选式大白话+矛盾分析法客观分析问题：调毛主席资料库取证、站在未来看今天，生成标注来源的自包含 HTML。随「毛主席」名人资料包一起安装，装好后消息里写「请教毛主席」即自动激活",
            source: "official",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/consult-mao.md"),
        },
        CatalogSkill {
            id: "skill-creator",
            name: "Skill 创建向导",
            description: "引导用户创建自定义 Skill，自动生成模板和配置文件",
            source: "official",
            preinstalled: true,
            system_prompt: include_str!("../../../../src/templates/skills/skill-creator.md"),
        },
        // ── 市场（点安装即用） ──
        CatalogSkill {
            id: "pdf",
            name: "PDF 文档处理",
            description: "提取 / 生成 / 编辑 PDF：抽取文本表格、合并拆分、Markdown 转 PDF、表单与 OCR",
            source: "official",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/pdf.md"),
        },
        CatalogSkill {
            id: "xlsx",
            name: "Excel 表格",
            description: "读取分析与生成 Excel：透视统计、公式、图表、多 sheet 报表",
            source: "official",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/xlsx.md"),
        },
        CatalogSkill {
            id: "pptx",
            name: "PPT 演示文稿",
            description: "把 PDF / 文档 / 数据转成有高级感的 PPT：母版配色、版式层级、图表，python-pptx 生成",
            source: "official",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/pptx.md"),
        },
        CatalogSkill {
            id: "edge-tts",
            name: "语音合成 Edge-TTS",
            description: "把文本转成自然语音音频，多语言多音色，免费无需 key",
            source: "third-party",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/edge-tts.md"),
        },
        CatalogSkill {
            id: "hyperframes",
            name: "视频动画 Hyperframes",
            description: "用逐帧 / 分镜方式生成短视频与动画，ffmpeg 合成，可配 Edge-TTS 旁白",
            source: "third-party",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/hyperframes.md"),
        },
        CatalogSkill {
            id: "web-search",
            name: "联网搜索",
            description: "实时联网检索，基于 Tavily / Brave 等真实来源回答并交叉验证",
            source: "third-party",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/web-search.md"),
        },
        CatalogSkill {
            id: "image-gen",
            name: "AI 生图",
            description: "按描述生成图片：先检测当前供应商是否真的支持生图，不支持时用中文说明并改用「很有图片质感的 HTML」兜底",
            source: "third-party",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/image-gen.md"),
        },
        // ── 源自 ConardLi 教程：完整可跑的网页演示视频技能包 ──
        CatalogSkill {
            id: WVP_ID,
            name: "网页演示视频制作（ConardLi·Polaris集成）",
            description: "把文稿做成 16:9 可点击翻页的网页演示再录屏成片。安装即下载完整脚手架+23主题+音频流水线，依赖自动装；配音自动调用 Polaris 内置 MiniMax（无需 mmx-cli / 登录 / GroupId）。Windows 走 Node 版一键跑通。",
            source: "third-party",
            preinstalled: false,
            system_prompt: WVP_ADDENDUM,
        },
        // ── 源自 ConardLi 教程的两套向导 ──
        CatalogSkill {
            id: "web-video-presentation-guide",
            name: "网页演示视频制作向导",
            description: "把文稿做成 16:9 可点击翻页的网页演示再录屏成片：逐检查点告诉你此刻该做什么，并引导引入 ConardLi 的 web-video-presentation 原 skill",
            source: "third-party",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/web-video-presentation-guide.md"),
        },
        CatalogSkill {
            id: "harness-practices",
            name: "Harness 工程实践向导",
            description: "把 Claude Code 调教成生产力 harness：盘点瓶颈 → 技能化/供应商切换(CC Switch)/MiniMax CLI/子代理编排，逐步告诉你现在该做什么",
            source: "third-party",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/harness-practices.md"),
        },
        // ── 自媒体全链路运营（交互决策版，与「自动化」里的两条流程同源） ──
        CatalogSkill {
            id: "wechat-pipeline",
            name: "微信公众号 · 全链路运营",
            description: "选题→风格→成稿→排版出图一条龙；每个决策点先讲思考再给编号选项让你挑、也可直接输入覆盖；风格可调；支持全自动",
            source: "third-party",
            preinstalled: true,
            system_prompt: include_str!("../../../../src/templates/skills/wechat-pipeline.md"),
        },
        CatalogSkill {
            id: "xiaohongshu-pipeline",
            name: "小红书 · 全链路运营",
            description: "选题→风格→文案→图卡渲染一条龙；每个决策点先讲思考再给编号选项让你挑、也可直接输入覆盖；风格可调；支持全自动",
            source: "third-party",
            preinstalled: true,
            system_prompt: include_str!("../../../../src/templates/skills/xiaohongshu-pipeline.md"),
        },
        // ── 自媒体全链路·配套三件套（选题前置 / 数据复盘 / 社群应对，补全闭环） ──
        CatalogSkill {
            id: "hot-topic-radar",
            name: "选题雷达",
            description: "联网抓热点+对标爆文，归纳成 3-5 个选题方向、每个给 2-3 个具体选题并做爆款拆解（为什么火/适合哪个平台/时效难度），编号供勾选；读 KB 避免撞题。可独立用，也是全链路第一步",
            source: "third-party",
            preinstalled: true,
            system_prompt: include_str!("../../../../src/templates/skills/hot-topic-radar.md"),
        },
        CatalogSkill {
            id: "content-analytics-report",
            name: "数据复盘 · 运营周报",
            description: "把一批已发文章/笔记的数据做成运营周报：逐篇打优劣势、找「哪类选题/标题/发布时机」数据好的规律、给下轮主攻方向，并回写 KB 反哺选题",
            source: "third-party",
            preinstalled: true,
            system_prompt: include_str!("../../../../src/templates/skills/content-analytics-report.md"),
        },
        CatalogSkill {
            id: "community-engagement",
            name: "评论 · 社群应对",
            description: "把评论/私信分类（提问/夸赞/抬杠/求合作/负面），按账号人格逐条起草回复，标出需本人亲自处理的高敏感项，并把高频疑问提炼成选题线索回写 KB",
            source: "third-party",
            preinstalled: true,
            system_prompt: include_str!("../../../../src/templates/skills/community-engagement.md"),
        },
        CatalogSkill {
            id: "xhs-mao-pipeline",
            name: "小红书 · 毛选风格发布",
            description: "调毛主席知识库析毛选文风→就给定主题写小红书爆款文案→出图(HTML图卡转截图 或 AI配图)→调 post-to-xhs 浏览器自动发布;发前必人工确认、可先预览、需扫码登录",
            source: "third-party",
            preinstalled: true,
            system_prompt: include_str!("../../../../src/templates/skills/xhs-mao-pipeline.md"),
        },
        // ── 壹伴排版优化（公众号排版 + CloakBrowser 直送草稿，根治格式错） ──
        CatalogSkill {
            id: "wechat-md-typesetter",
            name: "壹伴排版优化",
            description: "壹伴式排版：只产出干净语义正文（零内联样式），随包壹伴脚本在 CloakBrowser 公众号编辑器 DOM 上按约定风格一键套样式（标题色块/引用卡/分割线/列表转段落全内联），填标题存草稿（绝不自动发布）——根治粘贴格式错乱",
            source: "third-party",
            preinstalled: true,
            system_prompt: WECHAT_TS_SKILL_MD,
        },
        // ── 微信聊天 · 每日待办（本地解密微信→挖「该回谁」→进晨报；配套每日自动化流程） ──
        CatalogSkill {
            id: WECHAT_TASKS_ID,
            name: "微信聊天 · 每日待办",
            description: "每天本地解密微信聊天，从「你回过话的私聊 + 你活跃的群里、最近几天别人发来你还没回」的消息里挖出待办，写进晨报卡片，点一下就帮你拟回复。一次性 hook 抓密钥后每天自动复用。全本地、不上传、不发布",
            source: "official",
            preinstalled: false,
            system_prompt: WECHAT_TASKS_SKILL_MD,
        },
        // ── 源自 ClaudeSkills 合集的两个内容创作技能（全链路成稿/出图时调用） ──
        CatalogSkill {
            id: "gz-wechat-article-writer",
            name: "公众号文章创作（ClaudeSkills）",
            description: "微信公众号文章创作助手：风格灵活适配（企业官号/个人技术博客/活动回顾/产品评测），优化标题与结构。全链路成稿阶段的内容引擎",
            source: "third-party",
            preinstalled: true,
            system_prompt: include_str!("../../../../src/templates/skills/gz-wechat-article-writer.md"),
        },
        CatalogSkill {
            id: "gz-notion-infographic",
            name: "信息图 / 小红书图文（ClaudeSkills）",
            description: "按大纲自动研究并生成高质量可视化：Notion 手绘风信息图组图 / PPTX，适合小红书图文与社媒传播图。全链路渲染阶段的图卡引擎",
            source: "third-party",
            preinstalled: true,
            system_prompt: include_str!("../../../../src/templates/skills/gz-notion-infographic.md"),
        },
        // ═══ 开发工程师工具箱（市场精选，点安装即用）═══
        // 来源:obra superpowers(工程纪律)+ Trae 官方/社区 TRAE-Skills 仓(开发场景)
        // + WorkBuddy 类聚合市场的高频品类。均 preinstalled=false,不污染每轮对话,按需安装/自动激活。
        // ── 项目skill(superpowers 精简重构成单卡): 开发七纪律合一 ——
        //    模型已经够强,不教怎么做只圈行为红线。全文约 40 行 7 小节(读懂项目/放哪层/
        //    审查/抓虫/测试/迁移/发版),detect_dev_intent 命中开发类任务即自动注入,
        //    模型按小节自行取用。
        CatalogSkill {
            id: "project-skill",
            name: "项目skill",
            description: "做项目/写编码时自动生效的开发七纪律:先读懂项目、先想放哪层、审查安全/性能/边界、抓虫复现→定位→证据→修复→回归、补真测试、迁移先拆阶段、发版前查全。只约束行为不教做法",
            source: "official",
            preinstalled: true,
            system_prompt: include_str!("../../../../src/templates/skills/project-skill.md"),
        },
        // ── Trae 开发场景系(官方市场 + 社区 TRAE-Skills 仓 150+ 精选) ──
        CatalogSkill {
            id: "git-commit",
            name: "智能提交 Conventional Commits",
            description: "把工作区改动整理成规范 commit:看 diff 懂意图→合理拆分暂存→生成 type(scope): 祈使句标题+解释为什么的正文;优先新建不 amend、不跳 hook、不带密钥。源自 Trae",
            source: "third-party",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/git-commit.md"),
        },
        CatalogSkill {
            id: "gh-cli",
            name: "GitHub CLI 速查",
            description: "用 gh 命令行管仓库/Issue/PR/Actions/Release,不必开网页;含常用 PR/CI 日志/release 配方与 --json 取结构化结果。破坏性动作先确认。源自 Trae",
            source: "third-party",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/gh-cli.md"),
        },
        CatalogSkill {
            id: "frontend-ui",
            name: "高级前端 UI(去 AI 味)",
            description: "做网页/组件/落地页时产出像真人精修的界面:避开紫蓝渐变+居中卡片+emoji 堆砌的通用 AI 模板,讲排版层级/有主张配色/留白节奏/状态细节/响应式可访问。源自 Trae 官方市场",
            source: "third-party",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/frontend-ui.md"),
        },
        CatalogSkill {
            id: "rest-api-design",
            name: "REST API 设计",
            description: "设计/评审 HTTP API 遵循 REST 约定:名词复数资源、方法语义与幂等、状态码正确、统一错误体、分页过滤版本化鉴权幂等键。源自 Trae",
            source: "third-party",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/rest-api-design.md"),
        },
        CatalogSkill {
            id: "docker-deploy",
            name: "Docker 容器化部署",
            description: "把应用打成精简可复现的镜像:多阶段构建+小基镜像+缓存层+非 root+.dockerignore;compose 配资源上限/日志轮转/机密走 env;构建后实跑验证。源自 Trae",
            source: "third-party",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/docker-deploy.md"),
        },
        CatalogSkill {
            id: "sql-optimization",
            name: "SQL 查询优化",
            description: "让慢查询变快:先 EXPLAIN 量执行计划→定位缺索引/回表/JOIN/N+1→针对性改(复合/覆盖索引、避函数失效、游标分页)→真实数据量验证;索引非越多越好。源自 Trae",
            source: "third-party",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/sql-optimization.md"),
        },
        // ── 安全审计 + 技术文档(Trae 安全/文档 + WorkBuddy 聚合市场高频品类) ──
        CatalogSkill {
            id: "security-audit",
            name: "安全审计(Web/依赖)",
            description: "防御性安全:按注入(SQL/命令/路径穿越/XSS/SSRF)、鉴权与会话(越权/机密/CSRF)、依赖漏洞(npm/pip/cargo audit)系统过一遍,给严重级+复现+修法。只审自己代码不助攻。源自 Trae/WorkBuddy",
            source: "third-party",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/security-audit.md"),
        },
        CatalogSkill {
            id: "tech-writing",
            name: "技术文档(README/API/Changelog)",
            description: "把项目写成读者用得起来的文档:README(一句话定位+可复制的快速开始+用法+配置)、API 文档(契约+例子)、Changelog(Keep a Changelog 分类);写真话、例子能跑通。源自 Trae/WorkBuddy",
            source: "third-party",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/tech-writing.md"),
        },
        // ═══ 2026-07 扩容:GitHub 高星 skill 精选(superpowers 248k★ / anthropics官方 159k★ /
        //     financial-services 33k★ / awesome 合集),按人群补齐 开发/测试/财会/设计 四组。
        //     全部 preinstalled=false 市场件,按需安装不污染对话。 ═══
        // ── 开发编程(superpowers 系 + 官方) ──
        CatalogSkill {
            id: "systematic-debugging",
            name: "系统化调试(根因四阶段)",
            description: "修 bug 铁律:没查到根因不许动手。调查(读全堆栈/稳定复现/边界取证/反向追坏值)→模式对比(找能跑的相似代码逐项比差异)→单假设验证(一次只动一个变量)→先写失败测试固化再修;连修 3 次失败必须停下质疑架构。源自 superpowers(GitHub 248k star)",
            source: "third-party",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/systematic-debugging.md"),
        },
        CatalogSkill {
            id: "writing-plans",
            name: "实施计划编写",
            description: "把多步开发任务写成「零上下文执行者照着打字就能干」的计划:精确路径+完整代码+可跑命令,每步 2-5 分钟粒度,禁止 TBD/「处理边界情况」类占位词,跨任务命名逐字一致,写完自审四查。源自 superpowers(GitHub 248k star)",
            source: "third-party",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/writing-plans.md"),
        },
        CatalogSkill {
            id: "verification-before-completion",
            name: "完成前验证(证据门)",
            description: "根治「Done!应该没问题」式虚报:宣称完成/修好/测试过之前必走五步证据门(定验证命令→新鲜跑→读全输出和退出码→核对论断→才许开口),禁用「应该/大概」,回归要红→绿全程,子任务自报不可信须看 diff。源自 superpowers(GitHub 248k star)",
            source: "third-party",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/verification-before-completion.md"),
        },
        CatalogSkill {
            id: "mcp-builder",
            name: "MCP 工具开发",
            description: "给 AI Agent 造 MCP server 的四阶段:按 agent 任务视角设计工具(非 API 直译)+可自愈的错误信息→先搭鉴权/分页地基再写工具(Zod/Pydantic schema+行为注解)→inspector 实测→出 10 道可验证评测题打分。源自 Anthropic 官方 skills 仓(GitHub 159k star)",
            source: "official",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/mcp-builder.md"),
        },
        // ── 测试质检 ──
        CatalogSkill {
            id: "webapp-testing",
            name: "网页应用 E2E 测试",
            description: "浏览器自动化测网页的基本功:侦察后行动——networkidle 之前禁止查 DOM,截图/看渲染后结构才定 selector;语义定位(get_by_role)不用脆弱链,auto-waiting 断言禁 sleep,连 console 错误一起报;托管 dev server 生命周期不留孤儿进程。源自 Anthropic 官方 skills 仓(GitHub 159k star)",
            source: "official",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/webapp-testing.md"),
        },
        CatalogSkill {
            id: "e2e-test-pipeline",
            name: "自动化测试流水线",
            description: "从零给应用建 E2E 测试的四段流水线:探索(实际逛应用出上下文文档)→用例设计(前置/步骤/预期,人工评审门)→脚本化(Page Object+getByRole+storageState 多角色登录态,写完跑绿)→维护(失败归因,只提议不自动改,权限断言绝不静默改)。综合 Playwright 生产实践",
            source: "third-party",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/e2e-test-pipeline.md"),
        },
        CatalogSkill {
            id: "bug-report-repro",
            name: "Bug 复现与回归",
            description: "QA 处理 bug 的闭环:复现(逐变量排查,概率性 bug 跑脚本量化复现率)→最小化(删到最小步骤集+git bisect 定引入点)→标准报告(现象+条件标题/编号步骤/预期实际/复现率/严重级)→修后转自动化回归用例固化。复现不了就如实标注,不许凭想象写报告",
            source: "third-party",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/bug-report-repro.md"),
        },
        // ── 财务会计(官方 financial-services 33k★ 投行级规范改写) ──
        CatalogSkill {
            id: "financial-model",
            name: "财务建模(三表+DCF)",
            description: "建「活的」Excel 财务模型:分段确认制不许一口气建到底;公式全写进单元格禁硬编码,输入蓝字/公式黑字配色;三表勾稽铁律(BS 每期平衡+CF 期末现金对 BS);DCF 固化 FCF 顺序/CAPM 市值权重/期末价值占比黄旗/5×5 全公式敏感性表;交付前公式错误清零。源自 Anthropic 官方 financial-services(GitHub 33k star)",
            source: "official",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/financial-model.md"),
        },
        CatalogSkill {
            id: "invoice-audit",
            name: "发票报销批量整理",
            description: "散落的发票/收据(PDF/图片)批量处理:pdfplumber 提字段(认增值税发票代码/号码/价税合计/税额),扫描件 OCR,失败标「待人工复核」绝不编数;同号发票查重防重复报销;统一重命名归档;出 CSV 台账+汇总报告。原件只复制不动。源自 awesome-claude-skills 合集(GitHub 56k star)+官方 pdf 管线",
            source: "third-party",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/invoice-audit.md"),
        },
        CatalogSkill {
            id: "bookkeeping-recon",
            name: "对账与模型审计",
            description: "月结两件套:对账走六桶法(两边归一化→全外连接→匹配/金额差/时点差/单边各入桶,差异逐笔归因出双报告);Excel 模型审计猎杀公式里的硬编码/模式断裂/断链/BS 不平,输出 sheet+单元格+严重级+修法问题表。只报告不动手,改动须逐项批准。源自 Anthropic 官方 financial-services(GitHub 33k star)",
            source: "official",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/bookkeeping-recon.md"),
        },
        // ── 设计美工(官方 skills 仓设计系) ──
        CatalogSkill {
            id: "canvas-design",
            name: "海报视觉设计",
            description: "美术馆级海报/封面/视觉稿:先写设计哲学(原创美学流派,五维展开)再动手,从主题挖隐性引用扎根配色;HTML 精确排版后截图出 PNG/PDF;90%视觉10%文字、元素零重叠、禁卡通贴纸感和 AI 模板脸。源自 Anthropic 官方 skills 仓(GitHub 159k star)",
            source: "official",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/canvas-design.md"),
        },
        CatalogSkill {
            id: "brand-guidelines",
            name: "品牌主题系统",
            description: "让 PPT/海报/网页/文档吃同一套品牌规范:有 VI 就固化成规范卡(色值 hex 精确复制/字体带兜底/logo 规则)严格执行;没 VI 就出 3-5 套命名主题实际渲染给用户选,确认后全文应用并校验对比度。规范卡可存成自定义 skill 长期复用。源自 Anthropic 官方 theme-factory+brand-guidelines(GitHub 159k star)",
            source: "official",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/brand-guidelines.md"),
        },
        CatalogSkill {
            id: "algorithmic-art",
            name: "生成艺术 Algorithmic Art",
            description: "p5.js 创意编码:算法哲学→选一个主手法做深(流场/L-system/细胞自动机/噪声位移)→交付自包含 HTML 打开即跑;randomSeed+noiseSeed 双设保证同 seed 复现,带 seed 导航和 PNG 下载,参数须源于算法内在自由度;纯 CSS/Canvas/p5.js 禁 WebGL 重依赖。源自 Anthropic 官方 skills 仓(GitHub 159k star)",
            source: "official",
            preinstalled: false,
            system_prompt: include_str!("../../../../src/templates/skills/algorithmic-art.md"),
        },
        // ── 默认浏览器插件（预装、默认开启，可随时移除） ──
        CatalogSkill {
            id: "cloak-browser",
            name: "CloakBrowser 浏览器",
            description: "Agent 默认浏览器：源码级隐身 Chromium，drop-in 替换 Playwright，过 Cloudflare / 反爬。可随时关闭移除",
            source: "third-party",
            preinstalled: true,
            system_prompt: include_str!("../../../../src/templates/skills/cloak-browser.md"),
        },
        // ── 浏览器智能体（预装、默认开启）：高层多步网页自动化,底层走 CloakBrowser ──
        CatalogSkill {
            id: BROWSER_USE_ID,
            name: "浏览器智能体 browser-use",
            description: "给一句高层目标,browser-use 智能体自跑「看页面→决策→点击/输入/翻页」循环完成多步网页任务,不用手写 Playwright 步骤。底层浏览器强制走 CloakBrowser 隐身 Chromium(过 Cloudflare/反爬),绝不用其自带浏览器。复杂多步网页自动化用它,简单单步截图/抓取用 CloakBrowser",
            source: "third-party",
            preinstalled: true,
            system_prompt: BROWSER_USE_SKILL_MD,
        },
    ]
}

pub(crate) fn find_catalog(id: &str) -> Option<CatalogSkill> {
    catalog().into_iter().find(|c| c.id == id)
}

/// 目录技能的市场分组。集中一处映射，新增技能记得来这里归组，漏归落「通用」。
pub(crate) fn skill_category(id: &str) -> &'static str {
    match id {
        // 办公文档
        "pdf" | "xlsx" | "pptx" | "deep-research" | "web-search" => "办公文档",
        // 财务会计
        "financial-model" | "invoice-audit" | "bookkeeping-recon" => "财务会计",
        // 开发编程
        "project-skill"
        | "git-commit"
        | "gh-cli"
        | "frontend-ui"
        | "rest-api-design"
        | "docker-deploy"
        | "sql-optimization"
        | "security-audit"
        | "tech-writing"
        | "systematic-debugging"
        | "writing-plans"
        | "verification-before-completion"
        | "mcp-builder" => "开发编程",
        // 测试质检
        "webapp-testing" | "e2e-test-pipeline" | "bug-report-repro" => "测试质检",
        // 设计美工
        "canvas-design" | "brand-guidelines" | "algorithmic-art" | "image-gen" => "设计美工",
        // 自媒体运营
        "wechat-pipeline"
        | "xiaohongshu-pipeline"
        | "hot-topic-radar"
        | "content-analytics-report"
        | "community-engagement"
        | "xhs-mao-pipeline"
        | "wechat-md-typesetter"
        | "gz-wechat-article-writer"
        | "gz-notion-infographic" => "自媒体运营",
        // 音视频
        "edge-tts" | "hyperframes" | "web-video-presentation" | "web-video-presentation-guide" => {
            "音视频"
        }
        // 自动化与浏览器
        "cloak-browser" | "browser-use" | "turbo-download" | "wechat-tasks" => "自动化与浏览器",
        _ => "通用",
    }
}
