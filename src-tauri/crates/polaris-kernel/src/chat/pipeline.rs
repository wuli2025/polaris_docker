//! chat_send 主流程: 后台管线(拼 prompt -> spawn claude -> stream reader)、
//! 看门狗、取消、流事件解析。(从 chat.rs 纯移动拆出, 逻辑零变化)

use crate::claude_md;
use crate::conv;
#[cfg(not(feature = "desktop"))]
use crate::host::AppHandle;
use crate::skills;
use parking_lot::Mutex;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(feature = "desktop")]
use tauri::{AppHandle, Emitter};

use crate::runtime::procs::no_window;

use super::artifacts::*;
use super::prompt::*;
use super::types::*;

/// 默认预授权的联网工具 (逗号分隔, 传给 `--allowedTools`)。
/// 把内置 WebSearch / WebFetch 设为「联网搜索默认打开」: 任何权限模式都不再拦截,
/// 深度搜索 / 联网搜索因此能真正联网检索, 而不是退回内置知识。
const DEFAULT_WEB_TOOLS: &str = "WebSearch,WebFetch";

/// 非「拒绝授权」档位下额外放行的本地工具。
/// 缘由: headless (`--print`, stdin=null) 模式下没有人能逐个点「同意」, `acceptEdits`
/// 只自动批准文件编辑而 **不含执行**, 于是 claude 能写出 `create_pptx.py` 却跑不了
/// `python create_pptx.py` → .pptx / .xlsx / 图表这类「要执行脚本才能产出」的成品全部卡死
/// (实测 permission_denials 五连拒, 工具名是 Windows 的 `PowerShell`)。
/// 这里显式放行本地读写 + 执行 (Windows shell 工具叫 `PowerShell`, 跨平台再带上 `Bash`),
/// 让成品能真正落地。危险兜底仍由「拒绝授权(plan, 只读)」档位提供。
const LOCAL_WORK_TOOLS: &str = "Read,Write,Edit,Glob,Grep,Bash,PowerShell";

/// 按权限档位 (cli_value: default | acceptEdits | plan) 组装 `--allowedTools`。
/// - plan (拒绝授权 / 只读): 仅联网工具, 不放行任何本地执行;
/// - default / acceptEdits (手动 / 自动): 联网 + 本地读写执行, 成品能真正产出。
/// - with_task=true (动态编排): 额外放行 `Task` —— 否则 headless(stdin=null)下编排器
///   想扇出子代理会卡在权限确认上, 多智能体并行就跑不起来。
fn allowed_tools_for(perm: &str, with_task: bool) -> String {
    let mut tools = if perm == "plan" {
        DEFAULT_WEB_TOOLS.to_string()
    } else {
        format!("{},{}", DEFAULT_WEB_TOOLS, LOCAL_WORK_TOOLS)
    };
    if with_task {
        tools.push_str(",Task");
    }
    tools
}

/// 快速模式弃用的冗余工具(传给 `--disallowedTools`)。
/// 快速模式 = 快速调用知识库 + 快速回答, 工具面要小: 砍掉多智能体扇出(Task, 启动慢且贵)、
/// Jupyter(NotebookEdit, 纯数据科学)。Read/Write/Edit/Glob/Grep/Bash/Web 都保留 —— 仍能查库、
/// 看文件、做轻量产出; 检索提速靠 `search_convention` + KB 根 `.ignore`(ripgrep 缩范围), 不靠砍
/// Glob/Grep。disallowedTools 优先级高于 allowedTools, 即便 acceptEdits 也拦得住。
/// 返回 None = 工作模式(纯 Claude Code, 放开全套工具)。with_task(动态编排)为真时不禁 Task。
fn disallowed_tools_for(work_full: bool, with_task: bool) -> Option<String> {
    if work_full {
        return None;
    }
    let mut tools = vec!["NotebookEdit"];
    if !with_task {
        tools.push("Task");
    }
    Some(tools.join(","))
}

/// 创作型 skill: 任务 = 做成品(PPT/网页/视频/图),不是知识问答。命中任一(显式点选或
/// 意图自动激活)即进入「创作模式」: 豁免 KB-First 强制取证 + Codex 扁平回复风格
/// (两者的「先查库再作答」「压缩字数」倾向会稀释创作注意力、把成品文案写干瘪 ——
/// 实测「软件内做 PPT 不如终端裸跑 claude」的主因), 只保留数据/指令隔离安全条款。
const CREATIVE_SKILL_IDS: &[&str] = &[
    "polaris-deck-studio",
    "polaris-web-studio",
    "polaris-video-studio",
    "web-video-presentation",
    "pptx",
    "image-gen",
];

// ───────────────────────── State ─────────────────────────

// 子进程池 + 「取消挂起」标记已收口到 runtime::procs::CHILDREN(与 doctor 共池,
// req_id 前缀不同不冲突);「spawn 完成前点停止」的窄窗口语义见 ChildRegistry 文档。
use crate::runtime::procs::{kill_tree, CHILDREN};
static REQ_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 单轮 assistant 文本落库缓冲上限 (字节): 防 claude 异常死循环狂打输出把内存撑爆。
/// 超限后实时 delta 仍照常 emit 给前端, 只是不再增长落库缓冲, 末尾加一次截断标记。
const MAX_ASSISTANT_BYTES: usize = 8 * 1024 * 1024;
/// 单轮 stderr 累积上限 (字节)。
const MAX_STDERR_BYTES: usize = 1024 * 1024;

fn next_req_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let c = REQ_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("req-{:x}-{:x}", ts, c)
}

// ───────────────────────── Commands ──────────────────────

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn chat_send(app: AppHandle, args: ChatSendArgs) -> Result<String, String> {
    if args.prompt.trim().is_empty() {
        return Err("消息不能为空".into());
    }
    // HTTP body 上限是上传场景共用的 512MB，不能拿它当聊天输入上限；否则单请求即可让
    // prompt 拼装/历史落盘/CLI 参数复制产生数倍内存峰值。2MiB 已远高于正常对话需要。
    if args.prompt.len() > 2 * 1024 * 1024 {
        return Err("消息过大，单次最多 2MB；请改为上传文件后让 AI 读取".into());
    }
    if let Some(cid) = args.conversation_id.as_deref() {
        // 远程客户端(手机/中继)用本地生成的 convId,服务端 conv 表可能没有 → 自动建;
        // 桌面 UI 先 conv_create 再发,走「已存在」分支,行为不变。
        conv::ensure_writable_or_create(cid)?;
    }
    let req_id = next_req_id();

    // 轻量同步部分到此为止: 只做 req_id 生成 + user 消息落历史(便宜, 且保证「先 user
    // 后 assistant」的落库顺序)。其余重活 —— 产物目录快照(WalkDir)、CLAUDE.md 渲染、
    // 静态指令拼装、KB 强制召回(含网络嵌入往返 250ms~1.8s)、meta emit、spawn claude、
    // reader 线程挂接 —— 全部挪进后台线程, chat_send 立即返回 req_id,
    // 用户「点发送 → 气泡出现响应」之间不再被这些活钉死。
    if let Some(cid) = &args.conversation_id {
        conv::append_message(cid, "user", &args.prompt).map_err(|e| e.to_string())?;
    }

    let bg_app = app.clone();
    let bg_req = req_id.clone();
    let bg_conv = args.conversation_id.clone();
    std::thread::spawn(move || {
        // 后台线程内任何 Err/panic 都必须转成前端已能处理的事件发出去, 绝不能静默吞掉
        // 让气泡永远转圈: 旧同步路径里 spawn 失败 = invoke reject = 前端「[发送失败]」气泡
        // + 结束运行态; 这里等价复刻为 error 事件(错误气泡) + done 事件(唯一终态, 结束
        // 运行态/清 reqId)。前端 15s 无声死亡看门狗只是兜底, 不依赖它。
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            chat_send_pipeline(&bg_app, &bg_req, args)
        }));
        let err_msg: Option<String> = match run {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(e),
            Err(p) => {
                let m = p
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| p.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".into());
                Some(format!("对话后台准备阶段异常 (panic): {m}"))
            }
        };
        if let Some(msg) = err_msg {
            emit_event(
                &bg_app,
                ChatStreamEvent {
                    req_id: bg_req.clone(),
                    kind: "error".into(),
                    text: Some(msg),
                    tool: None,
                    conversation_id: bg_conv.clone(),
                },
            );
            emit_event(
                &bg_app,
                ChatStreamEvent {
                    req_id: bg_req.clone(),
                    kind: "done".into(),
                    text: None,
                    tool: None,
                    conversation_id: bg_conv,
                },
            );
            // 失败收尾时顺手清掉可能残留的「取消挂起」标记, 防 PENDING_CANCEL 积攒。
            CHILDREN.take_cancel(&bg_req);
        }
    });

    Ok(req_id)
}

/// chat_send 的重活管线(后台线程执行): 拼 prompt(静态指令按意图门控) → KB 召回 →
/// spawn claude → 挂 stdin 写入 / 看门狗 / stderr / stdout reader 线程。
/// 事件契约与旧同步实现完全一致(时序仍是 meta → stream(delta/tool/artifact/error) →
/// done), 只是全部从后台线程 emit。fast / work / 创作模式统一走这一条管线。
fn chat_send_pipeline(app: &AppHandle, req_id: &str, args: ChatSendArgs) -> Result<(), String> {
    // 用户在管线开跑前就点了停止 → 直接放弃(前端 cancel 路径已自行收尾 UI, 不发事件)。
    if CHILDREN.take_cancel(req_id) {
        return Ok(());
    }

    // 产物目录 (每个会话一份): claude 把成品文件写到这里 → 侧边栏可预览
    let art_dir = artifacts_dir(args.conversation_id.as_deref());
    let _ = std::fs::create_dir_all(&art_dir);
    let art_before = dir_snapshot(&art_dir);

    // 一体注入: Skill prompt → KB CLAUDE.md + kb_search 召回 → 用户问题
    let current_project_id = args
        .conversation_id
        .as_deref()
        .and_then(conv::project_id_of_conversation);
    let cm_ctx =
        claude_md::render_for_project(current_project_id.as_deref(), &args.prompt, args.use_kb);

    let mut final_prompt = String::new();

    // 1(先算). Skill system prompts —— 显式点选 + 按任务意图自动激活(去重)。
    //    先收集进独立缓冲: 命中创作型 skill 时本轮进「创作模式」(CREATIVE_SKILL_IDS),
    //    下面的 KB 指令与回答风格据此取舍; 拼装顺序保持不变(KB 指令仍在最前)。
    let mut skill_section = String::new();
    let mut injected: Vec<String> = Vec::new();
    // 1a. 用户在对话框显式激活的 skill
    if let Some(ids) = &args.skill_ids {
        for id in ids {
            if injected.iter().any(|x| x == id) {
                continue;
            }
            if let Some((meta, system_prompt)) = skills::find(id) {
                skill_section.push_str(&system_prompt);
                skill_section.push('\n');
                injected.push(meta.id);
            }
        }
    }
    // 1b. 按任务意图自动激活（即使对话框没点选）：
    //     创建技能 → skill-creator；网页/浏览器自动化 → cloak-browser
    for (meta, system_prompt) in skills::auto_skills_for_intent(&args.prompt) {
        if injected.iter().any(|x| *x == meta.id) {
            continue;
        }
        skill_section.push_str(&system_prompt);
        skill_section.push('\n');
        injected.push(meta.id);
    }
    let creative = injected
        .iter()
        .any(|id| CREATIVE_SKILL_IDS.contains(&id.as_str()));

    // 工作模式: 仅 "work" 进工作模式(纯 Claude Code), 其余(含 None)一律快速模式。决定:
    // ①禁用哪些冗余工具 ②注入哪些约定(可运行项目/长任务仅工作模式)③KB 召回快档 vs 全质量
    // ④上下文预算 ⑤权限/模型默认(前端)。
    let work_full = args.work_mode.as_deref() == Some("work");
    // 快速模式强制调用知识库(这是该模式的本职「快速调用知识库」)——即便前端没开 KB 开关;
    // 工作模式(纯 Claude Code)则尊重用户的 use_kb 开关, 默认不注入 KB。
    let kb_recall = (args.use_kb || !work_full) && !creative;

    // 0. KB 顶层指令 (写死, 优先级最高)
    // 普通对话 = KB-First 全量: 知识库是真相源, 必须先 4 步取证再作答、脚注溯源。
    // 创作模式 = 精简版: 只保留「数据/指令隔离」安全条款(提示词注入防线, 不随模式豁免),
    // 知识库按需自取 —— 做 PPT/网页/视频时素材已在 prompt 里, 强制取证只会稀释创作注意力。
    // 这条指令在 prompt 最前面, 离用户问题最远——但因 Claude 的"system 指令优先"特性,
    // 它仍然约束着整轮回复。配合 `claude_md::render_for_project` 注入的结构化 wiki,
    // 模型就能沿 Read/Glob/Grep + [[双链]] 自主取证。
    if creative {
        final_prompt.push_str(&kb_isolation_directive_light());
    } else {
        final_prompt.push_str(&kb_first_directive());
    }
    final_prompt.push_str("\n\n---\n\n");

    if !skill_section.is_empty() {
        final_prompt.push_str(&skill_section);
        final_prompt.push_str("\n---\n\n");
    }

    // 1.5 回答风格约定。普通对话 = Codex 式扁平(砍废话); 创作模式 = 「回复克制、成品丰满」
    //     —— 扁平风格的「压缩字数」倾向会渗进幻灯片/网页文案, 把成品也写干瘪。
    if creative {
        final_prompt.push_str(&creative_style_directive());
    } else {
        final_prompt.push_str(&reply_style_directive());
    }
    final_prompt.push_str("\n\n---\n\n");

    // 静态指令门控总闸: POLARIS_PROMPT_FULL=1 恢复全部静态指令全量注入(排障/对比用)。
    let prompt_full = prompt_full_forced();
    // 产物意图只算一次, 输出约定与脚本公约两个门共用。
    let artifact_intent = detect_artifact_intent(&args.prompt);

    // 2. 输出文件约定 (Polaris) — 让成品文件落到产物目录, 侧边栏即可预览。
    //    门控: work 模式 / 创作模式 / 消息含「生成文件·成品产物」意图 → 全量(~700 tokens);
    //    否则精简版(只告知产物目录 + 末尾报绝对路径, 2 句)。
    if prompt_full || work_full || creative || artifact_intent {
        final_prompt.push_str(&output_convention(&art_dir));
    } else {
        final_prompt.push_str(&output_convention_lite(&art_dir));
    }
    final_prompt.push_str("\n\n---\n\n");

    // 2.1 可运行项目约定 (板块⑮) — 要跑起来的应用(尤其前后端)打包成带运行清单的项目文件夹,
    //     用户在右侧点「运行」即一键启动前后端并内嵌预览。创作模式跳过(成品是单文件);
    //     **仅工作模式注入**: 快速模式只为「查库+答」, 不打包工程, 注入只会膨胀 prompt。
    if !creative && work_full {
        final_prompt.push_str(&project_convention(&art_dir));
        final_prompt.push_str("\n\n---\n\n");
    }

    // 2.2 长任务铁律: 回合结束 = claude 退出 = 其整棵子进程树被回收(防孤儿)。模型若把出片/上传/
    //     下载等耗时任务放后台再结束回复, 任务必死且无人知晓。**工作模式 + 创作模式注入**(那里
    //     才有长产出); 快速模式只做秒级问答、无长任务, 跳过这段 → 提示词更短、首 token 更快。
    if work_full || creative {
        final_prompt.push_str(longtask_convention());
        final_prompt.push_str("\n\n---\n\n");
    }

    // 2.21 脚本执行公约: 模型爱写临时脚本干活, 但用户机器上的 `python`/`python3`
    //      极可能是 Microsoft Store 的 0 字节占位符(实证截图: python3.exe 是占位符 → 做 PPT
    //      只能降级成 HTML), 裸调必失败或假成功。统一要求: Python 一律 `uv run` + PEP 723 内联
    //      依赖, 禁裸调 python/系统 pip; Node 脚本先自检可用性。uv 由环境医生预置并已注入 PATH
    //      (见 doctor::ensure_uv_on_process_path), 三端(win/mac/docker)同构。
    //      门控(此前 always-on, ~1850 tokens): work 模式(干活模式)恒注入; 创作模式恒注入
    //      (出 PPT/视频要跑导出脚本); 或消息命中 开发意图(detect_dev_intent) / 脚本·执行·
    //      批量·文件处理意图(detect_script_intent) / 产物意图(生成 xlsx/pptx/pdf 等要靠
    //      脚本落地)时注入; 普通闲聊/短问答跳过。
    if prompt_full
        || work_full
        || creative
        || artifact_intent
        || skills::detect_dev_intent(&args.prompt)
        || detect_script_intent(&args.prompt)
    {
        final_prompt.push_str(script_convention());
        final_prompt.push_str("\n\n---\n\n");
    }

    // 2.22 大文件下载公约 (按需注入): >200MB 大文件禁单线 wget, 必须 aria2c 多连接分段并行。
    //      此前每轮都注入, 但绝大多数对话(尤其办公)从不下大文件 —— 改成**仅下载意图命中才注入**,
    //      办公/日常提示词随之瘦身, 首 token 更快、也少一段无关上下文诱导模型跑题。
    if skills::detect_download_intent(&args.prompt) {
        final_prompt.push_str(download_convention());
        final_prompt.push_str("\n\n---\n\n");
    }

    // 2.23 高效检索公约: 大库上 grep/glob 全树盲扫会慢 —— 注入「先缩范围(path+
    //      glob/type 过滤)+ 结果封顶 + 用 rg 不用 GNU grep」铁律, 配合 KB 根的 .ignore 让
    //      Grep 工具(底层 ripgrep)默认就跳重目录, 检索默认快。
    //      门控(此前 always-on, ~1000 tokens): work 模式 / 显式开 KB(use_kb) / 消息含
    //      文件·查找·检索意图(detect_search_intent) → 全量; 否则一句话精简版。
    if prompt_full || work_full || args.use_kb || detect_search_intent(&args.prompt) {
        final_prompt.push_str(search_convention());
    } else {
        final_prompt.push_str(search_convention_lite());
    }
    final_prompt.push_str("\n\n---\n\n");

    // 2.15 分批长任务: 超长生成(60 页 PPT 这类)拆成有界批次, 每轮只建 ≤K 个 pending 单元,
    //      用 polaris.build.json 清单做 checkpoint, 断线从下一个 pending 续跑 ——
    //      规避单轮输出过长把流式连接拖死(socket closed → 进程坏死)。
    if args.batch_build {
        let bs = args.batch_size.unwrap_or(8).clamp(1, 50);
        final_prompt.push_str(&batch_build_directive(&art_dir, bs));
        final_prompt.push_str("\n\n---\n\n");
    }

    // 2.5 目标模式: 用户设了完成条件时, 注入「持续推进直到达成」指令
    if let Some(goal) = args
        .goal
        .as_deref()
        .map(str::trim)
        .filter(|g| !g.is_empty())
    {
        final_prompt.push_str(&goal_directive(goal));
        final_prompt.push_str("\n\n---\n\n");
    }

    // 2.65 动态编排: 把本轮当成多智能体编排, 用 Task 子代理并行扇出, 每条流水线
    //      实现 -> 对抗式校验 -> 修复, 最后汇总(详见 dynamic_workflow_directive)。
    if args.dynamic_workflow {
        final_prompt.push_str(&dynamic_workflow_directive());
        final_prompt.push_str("\n\n---\n\n");
    }

    // 2.68 专家团 / 智能匹配
    //   - auto-match（默认）: 每轮自动路由最合适的 1~3 位专家，注入「智能匹配·专家团」视角块。
    //     这是默认对话体验——一上来就用智能匹配专家团，无命中信号则不注入（闲聊不被套专家）。
    //   - expert-team: 显式专家团；检测到多专家任务时召集成队并注入分工。
    //   注意: 这里只**计算**专家块, 注入推迟到「## 用户问题」紧前(见步骤 4)——
    //   此前专家块排在第 6 段, 与用户问题之间隔着 KB 概览/召回/记忆地图/对话历史数千 token,
    //   专家准则在注意力上被淹没; 约束贴着问题放才最有效。
    // 专家团一律走内核桥(引擎未拼装时 expert_bridge()=None → 不注入, 语义同无命中)。
    let expert_block: Option<String> = match args.agent_mode.as_deref() {
        Some("expert-team") => super::bridges::expert_bridge().and_then(|eb| {
            if eb.detect_multi_expert_task(&args.prompt) {
                current_project_id.clone().and_then(|project_id| {
                    // 多专家召集: 注入每位主选专家的完整准则正文(而非只有名字+标签)
                    eb.team_block_spawn(project_id, args.prompt.clone())
                })
            } else {
                // 单专家任务也给个智能匹配视角，不必非要凑成多人团
                eb.route_block(&args.prompt)
            }
        }),
        // 默认（None 或 "auto-match"）走智能匹配；"single-agent" / "single-expert" 不在此注入。
        Some("auto-match") | None => {
            super::bridges::expert_bridge().and_then(|eb| eb.route_block(&args.prompt))
        }
        _ => None,
    };

    // 2.7 生图能力检测: 用户想生成图片, 但供应商坞里全是文本/代码大模型, 没有一个能真生图。
    //     注入「当前供应商 + 能否真生图」的事实, 让 image-gen 技能据此决定:
    //     不支持 → 用中文说清楚, 并改用「很有图片质感的 HTML」兜底。
    //     模型有时不遵守「开头摊牌」指令(会先说「已生成」), 所以由后端在回复最前面
    //     **确定性地**插入这句中文说明(见下方 image_notice), 保证用户一上来就看到。
    let image_notice: Option<String> = if skills::detect_image_intent(&args.prompt) {
        let (provider_name, supported) = crate::provider::image_gen_capability();
        final_prompt.push_str(&image_capability_directive(
            &provider_name,
            supported,
            &art_dir,
        ));
        final_prompt.push_str("\n\n---\n\n");
        if supported {
            None
        } else {
            Some(format!(
                "> ⚠️ **说明**：你当前使用的「{}」是文本大模型，**不支持生成真实图片**。下面用一张「HTML 模拟的画面」来替代；如需真实 AI 生图，请在「API 供应商」里配置支持文生图的图像接口。\n\n",
                provider_name
            ))
        }
    } else {
        None
    };

    // 3. CLAUDE.md 上下文 (KB 地图 + 项目人格)
    if !cm_ctx.is_empty() {
        final_prompt.push_str(&cm_ctx);
        final_prompt.push_str("\n\n---\n\n");
    }

    // fable 状态只查一次(打开 SQLite + COUNT): 此前家底概览与强制召回各自调一次
    // fable::status(), 现在共用同一份结果。
    let fable_st = super::bridges::kb_bridge().and_then(|b| b.fable_status());

    // 3.15 知识库家底概览(始终注入, 便宜): 让模型一开口就答得清「你的库在哪 / 有什么」,
    //      报全四层(妈妈库 wiki / raw / output / memory)家底, 不再只会复述 wiki 结构。
    {
        let ov = kb_overview_block(fable_st.as_ref());
        if !ov.is_empty() {
            final_prompt.push_str(&ov);
            final_prompt.push_str("\n\n---\n\n");
        }
    }

    // 3.2 双库强制召回: 后端先替模型查两个库(妈妈库 wiki 权威 + 外库 raw/output 混检), 命中
    //     片段直接喂进上下文。快速模式 kb_recall 恒真(本职就是「快速调用知识库」); 工作模式仅
    //     用户开了 KB 才查。创作模式跳过(素材已在 prompt 里)。
    if kb_recall {
        // 快速模式提速核心: 召回走「快档」—— 双车道(grep + 向量)融合但**跳过重排 API**, 把这步
        // 从网络主导的 ~1.8s 砍到 ~250ms(重排是 hybrid 唯一的慢源, 见 retrieve::search 的
        // mode=="hybrid" 闸)。预算也调小, 片段少 → 提示词更短、首 token 更快。
        // 工作模式(纯 Claude Code, 手动开 KB)走全质量 hybrid(带重排)。
        let fast_recall = !work_full;
        let recall_budget = if work_full {
            FORCED_RECALL_BUDGET
        } else {
            FAST_RECALL_BUDGET
        };
        let recall =
            forced_recall_block(&args.prompt, recall_budget, fast_recall, fable_st.as_ref());
        if !recall.is_empty() {
            final_prompt.push_str(&recall);
            final_prompt.push_str("\n\n---\n\n");
        }
    }

    // 3.4 回声层记忆地图: 「每日做梦」从历史对话蒸馏出的、关于主人本人的记忆(偏好/规则/
    //     纠正过的做法)。跨项目全局, 注地图不注全文(PRD v5 §6.3③) —— 让灵魂不仅记得盘里的
    //     往事, 也记得与主人相处的方式。memory/ 为空时返回空串、零开销。
    {
        let mmap = memory_map_block(MEMORY_MAP_BUDGET);
        if !mmap.is_empty() {
            final_prompt.push_str(&mmap);
            final_prompt.push_str("\n\n---\n\n");
        }
    }

    // 3.5 跨对话产物地图: 本项目其它对话生成过、仍在磁盘上的文件(绝对路径)。
    //     让模型可直接 Read「上次那个文件」, 用户不用重新拖拽。当前对话排除(它的文件
    //     已在下面的对话历史里出现)。
    if let Some(pid) = current_project_id.as_deref() {
        let amap =
            project_artifacts_block(pid, args.conversation_id.as_deref(), ARTIFACT_MAP_BUDGET);
        if !amap.is_empty() {
            final_prompt.push_str(&amap);
            final_prompt.push_str("\n\n---\n\n");
        }
    }

    // 3.6 对话历史: 本对话最近若干轮原文(预算封顶), 让同一对话能接上文 ——
    //     此前每轮都是无状态新进程, claude 看不到上一句, 这里补上。
    if let Some(cid) = args.conversation_id.as_deref() {
        // 快速模式历史预算调小: 秒级问答用不到大段历史/代码上下文, 少喂 → 输入 token 少 → 更快;
        // 工作模式要更全的上下文(多文件/多轮重构), 保留较大预算。
        let hist_budget = if work_full {
            HISTORY_CTX_BUDGET
        } else {
            FAST_HISTORY_BUDGET
        };
        let hist = history_block(cid, hist_budget);
        if !hist.is_empty() {
            final_prompt.push_str(&hist);
            final_prompt.push_str("\n\n---\n\n");
        }
    }

    // 3.7 用户手改产物提醒: 本对话的产物文件在上一轮之后被用户手动编辑过(右抽屉编辑器
    //     保存或外部工具改动)时, 明确告知模型「磁盘为准、改前必须 Read」—— 对齐 CLI 的
    //     文件真源体验, 防止模型凭对话历史里的旧版本整体重写、毁掉用户的手工修改。
    if let Some(cid) = args.conversation_id.as_deref() {
        let edited = user_edited_artifacts_block(cid);
        if !edited.is_empty() {
            final_prompt.push_str(&edited);
            final_prompt.push_str("\n\n---\n\n");
        }
    }

    // 3.9 专家块(智能匹配/专家团): 在 2.68 计算, 此处注入 —— 贴着用户问题, 准则不被
    //     KB/历史大段上下文稀释。
    if let Some(block) = expert_block {
        final_prompt.push_str(&block);
        final_prompt.push_str("\n\n---\n\n");
    }

    // 4. 用户原始问题
    final_prompt.push_str("## 用户问题\n\n");
    final_prompt.push_str(&args.prompt);

    let perm = args.permission_mode.cli_value();
    let conv_id_opt = args.conversation_id.clone();

    // 上下文预算自检: 估算本轮注入的总 token 并 emit 给前端(kind=meta) —— 分批编排据此
    // 自适应批量大小(input 越大则每批越小), 也让「自动检测上下文优化」有据可依。
    let est_tokens = estimate_tokens(&final_prompt);
    emit_event(
        app,
        ChatStreamEvent {
            req_id: req_id.to_string(),
            kind: "meta".into(),
            text: Some(est_tokens.to_string()),
            tool: None,
            conversation_id: conv_id_opt.clone(),
        },
    );

    // spawn 前再查一次「取消挂起」: 上面拼 prompt + KB 召回可能耗时数秒, 期间用户可能
    // 已点停止 —— 有标记就直接放弃, 连子进程都不起。
    if CHILDREN.take_cancel(req_id) {
        return Ok(());
    }

    // 默认走宿主机执行（沙箱可选，但默认关闭）；动态编排时放行 Task 子代理；
    // work_full 决定快速模式是否禁用冗余工具(disallowedTools)、是否传按模式的 --model。
    let mut child = spawn_on_host(
        &final_prompt,
        perm,
        &art_dir,
        args.dynamic_workflow,
        work_full,
        args.provider_id.as_deref(),
    )?;

    // prompt 经 stdin 喂给 claude (而非命令行参数): 大 prompt 不会撞 Windows 命令行
    // 长度上限, 也不会因 prompt 以 `-` 开头被当成 flag。spawn 后立刻写 + drop, claude 读到 EOF 就开始处理。
    // stdin 写放独立线程: 大 prompt 超过 OS 管道缓冲(~64KB)且 claude 尚未开始读时,
    // write_all 会阻塞 —— 放后台线程就不会卡住本 async 命令的执行线程(影响其它并发命令)。
    // 写完线程结束时 drop(stdin) 关管道 → claude 读到 EOF 开工。失败不致命(claude 有 fallback)。
    if let Some(mut stdin) = child.stdin.take() {
        let payload = std::mem::take(&mut final_prompt);
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = stdin.write_all(payload.as_bytes());
        });
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "claude 子进程没有 stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "claude 子进程没有 stderr".to_string())?;

    CHILDREN.insert(req_id.to_string(), child);

    // 「最近一次活动」时间戳: stdout/stderr 每产出一行就刷新(见下面两个 reader 线程)。
    // 看门狗据此判「空闲挂死」而非「绝对超时」—— 正在活跃流式输出的长任务(批量 PPT/
    // 长脚本等)不会被误杀, 只有真的长时间零输出(claude 子代理对 `/` 无界扫描卡住)才判挂死。
    let last_activity = Arc::new(Mutex::new(std::time::Instant::now()));

    // 看门狗(容器/服务端稳健性): 个别 prompt 会让 claude 触发子代理(`claude --print`,
    // 容器内其 cwd 落在 `/`)对文件系统做无界扫描而长时间不返回 —— 既拖死本轮, 又占住
    // OAuth 订阅的并发槽拖垮后续消息。判据分两层:
    // ① **连续空闲**超过阈值(而非一启动就倒计时)才进入嫌疑区(POLARIS_CHAT_TIMEOUT_SECS,
    //    桌面 600s / 容器 180s, 0=关);
    // ② 空闲超阈后先**深检进程树**: 还有活的子孙进程(claude 正在跑 Bash 工具里的构建/
    //    ffmpeg/下载等, 工具执行期整段零输出是常态), 或整树 CPU 时间仍在推进(claude 本体
    //    在算) —— 都算「静默但在干活」, 不杀, 转入 30s 低频复查; 只有**连续两次采样都
    //    完全静止**(零子孙 + CPU 零推进, 真挂死/网络吊死的特征)才杀整树。深检失败(平台
    //    探测不可用)退回旧的空闲即杀, 保住容器自愈。
    // 另有绝对硬顶 POLARIS_CHAT_HARD_CAP_SECS: 到点无条件收回(防失控子代理靠"有 CPU 活动"
    // 永久霸占并发槽)。桌面默认 0=不设(用户看得见, 有停止按钮, 长任务不设顶); 容器默认
    // 3600s。杀前 Child 已从 CHILDREN 摘出, 回收原因由看门狗自己 emit error 告知前端;
    // 杀掉后 claude stdout 随之关闭 → 下面 reader 线程照常收尾 emit done, 系统自愈。
    let watchdog_timeout = std::env::var("POLARIS_CHAT_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        // 桌面此前默认 0=不启用 → 挂死的 claude 子进程(及其 cwd=/ 子代理)永不超时,
        // 一年长跑里偶发网络故障累积出几十个吊死进程+阻塞线程,耗尽句柄/FD。默认常开,
        // 仍可经 POLARIS_CHAT_TIMEOUT_SECS 覆写(设 0 显式关闭)。
        .unwrap_or(if cfg!(feature = "desktop") { 600 } else { 180 });
    if watchdog_timeout > 0 {
        let wd_req = req_id.to_string();
        let wd_activity = last_activity.clone();
        let wd_app = app.clone();
        let wd_conv = conv_id_opt.clone();
        std::thread::spawn(move || {
            let timeout = std::time::Duration::from_secs(watchdog_timeout);
            let started = std::time::Instant::now();
            let hard_cap = std::env::var("POLARIS_CHAT_HARD_CAP_SECS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(if cfg!(feature = "desktop") { 0 } else { 3600 });
            // 检查节拍: 常态每 5s 看一次空闲; 进入「静默但在干活」延期区后放缓到 30s,
            // 深检(进程快照)只在延期区跑, 常态零开销。
            let base_tick = std::cmp::min(timeout, std::time::Duration::from_secs(5));
            let mut tick = base_tick;
            let mut last_cpu: Option<u64> = None;
            loop {
                std::thread::sleep(tick);
                // 先读空闲时长(不与 CHILDREN 锁同时持有, 避免锁序问题), 再持锁取 pid:
                // 取到 Some 才证明仍是本 req 的活进程; 取到 None = 已正常结束被 stdout
                // 线程 remove → 退出看门狗。深检可能耗几十 ms, 不在锁内做。
                let idle = wd_activity.lock().elapsed();
                let pid = {
                    let g = CHILDREN.lock();
                    let Some(c) = g.get(&wd_req) else { break };
                    c.id()
                };
                let over_cap =
                    hard_cap > 0 && started.elapsed() >= std::time::Duration::from_secs(hard_cap);
                if !over_cap && idle < timeout {
                    // 有输出在推进, 回到常态节拍并清掉 CPU 基线。
                    last_cpu = None;
                    tick = base_tick;
                    continue;
                }
                if !over_cap {
                    if let Some(s) = sample_tree(pid) {
                        // 首次越阈只建 CPU 基线不杀(cpu_advancing 视为 true), 下次采样再比对。
                        let cpu_advancing = last_cpu.is_none_or(|prev| s.cpu > prev);
                        last_cpu = Some(s.cpu);
                        if s.descendants > 0 || cpu_advancing {
                            tick = std::time::Duration::from_secs(30);
                            continue; // 静默但在干活: 不杀, 低频续看
                        }
                        eprintln!(
                            "[chat-watchdog] req={wd_req} 空闲 {}s 且进程树静止(0 子孙/CPU 无推进), 判挂死回收",
                            idle.as_secs()
                        );
                    } else {
                        eprintln!(
                            "[chat-watchdog] req={wd_req} 空闲 {}s, 进程树深检不可用, 按旧策略回收",
                            idle.as_secs()
                        );
                    }
                } else {
                    eprintln!("[chat-watchdog] req={wd_req} 总时长超硬顶 {hard_cap}s, 无条件回收");
                }
                // 重新确认仍是本 req 的同一进程再杀(防深检窗口内正常结束 + PID 复用误杀)。
                // 锁内只做「确认 + 摘出 Child」; kill_tree(Windows taskkill 同步等待常见
                // 数百 ms)与 kill/wait 全在锁外 —— CHILDREN 是全局锁, 锁内阻塞会把**所有
                // 对话**的 insert/remove(每次发送与收尾)一起钉住(范式同 chat_cancel /
                // ChildRegistry::kill)。摘出而非借用: 持有 Child 所有权 = reader 线程
                // 无法先 remove+reap → pid 在 kill 前绝不会被复用。
                let victim = {
                    let mut g = CHILDREN.lock();
                    let same_proc = g.get(&wd_req).is_some_and(|c| c.id() == pid);
                    if same_proc {
                        g.remove(&wd_req)
                    } else {
                        None
                    }
                };
                if let Some(mut c) = victim {
                    // 深检与摘出之间进程可能恰好正常结束:先 try_wait, 已退出就只 reap,
                    // 不把一次成功的请求误报成「被看门狗回收」。
                    if matches!(c.try_wait(), Ok(Some(_))) {
                        let _ = c.wait();
                    } else {
                        // 先杀干净**再**告知前端:反过来的话, 管道里已缓冲的 delta 会在
                        // error 之后继续上屏, 而移动端把 error 当终态立即放行下一问,
                        // 旧进程的尾巴会混进新一轮气泡。
                        kill_tree(pid); // 杀进程组: 一并带走 cwd=/ 的子代理
                        let _ = c.kill(); // 兜底杀本体 (taskkill /T 通常已带走它)
                        let _ = c.wait(); // reap, 防 Unix 僵尸进程泄漏
                        // Child 已摘出, reader 线程收尾时查不到 → 不再发「异常退出」error
                        // (同 chat_cancel 语义)。回收原因由这里告知前端, 不至于无声中断。
                        let reason = if over_cap {
                            format!("本轮总时长超过硬顶 {hard_cap}s, 已被看门狗强制回收")
                        } else {
                            format!(
                                "claude 进程空闲 {}s 无任何输出且进程树静止, 判定挂死, 已被看门狗回收",
                                idle.as_secs()
                            )
                        };
                        emit_event(
                            &wd_app,
                            ChatStreamEvent {
                                req_id: wd_req.clone(),
                                kind: "error".into(),
                                text: Some(reason),
                                tool: None,
                                conversation_id: wd_conv.clone(),
                            },
                        );
                    }
                }
                break;
            }
        });
    }

    // stderr 读线程: 任何 stderr 行都 emit 为 error 事件; 累积起来给 wait 用
    let app_err = app.clone();
    let req_err = req_id.to_string();
    let conv_id_err = conv_id_opt.clone();
    let stderr_buf = Arc::new(Mutex::new(String::new()));
    let stderr_buf_clone = stderr_buf.clone();
    let act_err = last_activity.clone();
    std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            let Ok(line) = line else { continue };
            if line.trim().is_empty() {
                continue;
            }
            *act_err.lock() = std::time::Instant::now(); // 刷新活动: 有产出就不算挂死
            {
                // 单次加锁 + 封顶: 异常时 stderr 也可能狂刷, 不让它无界累积。
                let mut buf = stderr_buf_clone.lock();
                if buf.len() < MAX_STDERR_BYTES {
                    buf.push_str(&line);
                    buf.push('\n');
                }
            }
            emit_event(
                &app_err,
                ChatStreamEvent {
                    req_id: req_err.clone(),
                    kind: "error".into(),
                    text: Some(format!("[stderr] {}", line)),
                    tool: None,
                    conversation_id: conv_id_err.clone(),
                },
            );
        }
    });

    // stdout 读线程: stream-json -> 事件; 累积 assistant 文本 + 产物路径
    let app_out = app.clone();
    let req_out = req_id.to_string();
    let conv_id_thread = conv_id_opt.clone();
    let stderr_buf_for_done = stderr_buf.clone();
    let art_dir_thread = art_dir.clone();
    let act_out = last_activity.clone();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut assistant_text = String::new();
        // 生图不支持时: 后端确定性地把中文说明作为**第一段**发出去并计入正文,
        // 不依赖模型遵守「开头摊牌」指令 → 用户一定先看到「当前模型不支持生图」。
        if let Some(notice) = image_notice {
            assistant_text.push_str(&notice);
            emit_event(
                &app_out,
                ChatStreamEvent {
                    req_id: req_out.clone(),
                    kind: "delta".into(),
                    text: Some(notice),
                    tool: None,
                    conversation_id: conv_id_thread.clone(),
                },
            );
        }
        // 本轮生成的成品文件 (绝对路径, 正斜杠), 既来自 Write/Edit 工具调用,
        // 也来自产物目录的前后快照 diff (覆盖 Bash/脚本生成的文件)
        let mut artifacts: Vec<String> = Vec::new();
        // 落库缓冲封顶: claude 若异常死循环狂打输出, 不让 assistant_text 无界增长撑爆内存。
        // 超限后改写入可丢弃的 scrap (实时 delta 仍照常 emit, 前端实时可见), 不再增长落库缓冲。
        let mut scrap = String::new();
        let mut capped = false;
        let mut partial = PartialStreamState::default();
        // delta 合批器: --include-partial-messages 下 CLI 每 token 吐一条 text_delta,
        // 逐条 emit = 每 token 一次跨 webview IPC。这里按 30ms 时间窗合并后再 emit;
        // 非 delta 事件到达前与流结束时必 flush(见 handle_stream_event 与循环末尾),
        // 事件顺序 / payload 结构完全不变。
        let mut batcher = DeltaBatcher::new();
        for line in reader.lines() {
            let Ok(line) = line else { continue };
            if line.trim().is_empty() {
                continue;
            }
            *act_out.lock() = std::time::Instant::now(); // 刷新活动: 流式产出即视为推进, 防误杀
            let target = if capped {
                &mut scrap
            } else {
                &mut assistant_text
            };
            match serde_json::from_str::<Value>(&line) {
                Ok(v) => handle_stream_event(
                    &app_out,
                    &req_out,
                    conv_id_thread.as_deref(),
                    &v,
                    target,
                    &mut artifacts,
                    &mut partial,
                    &mut batcher,
                ),
                Err(_) => {
                    // 非 JSON 行: 当作 delta 直接显示 (调试友好)。先 flush 挂起的合批
                    // delta, 保证屏上文本顺序与到达顺序一致。
                    batcher.flush(&app_out, &req_out, conv_id_thread.as_deref());
                    // 正文已进 accum 且已上屏 → 置记账位, 否则后续 result 携带同一内容时
                    // 会因 saw_assistant_text=false 再补一遍, 屏上/落库都成双份。
                    if !line.trim().is_empty() {
                        partial.saw_assistant_text = true;
                    }
                    target.push_str(&line);
                    target.push('\n');
                    emit_event(
                        &app_out,
                        ChatStreamEvent {
                            req_id: req_out.clone(),
                            kind: "delta".into(),
                            text: Some(line),
                            tool: None,
                            conversation_id: conv_id_thread.clone(),
                        },
                    );
                }
            }
            if capped {
                scrap.clear(); // scrap 只为让上面 emit 继续工作, 不能自己变成无界
            } else if assistant_text.len() > MAX_ASSISTANT_BYTES {
                assistant_text.push_str("\n\n[⚠️ 输出过长，后续内容已省略]");
                capped = true;
            }
        }
        // 流结束必 flush: 缓冲里最后一撮 delta 要先于 error/artifact/done 事件落地。
        batcher.flush(&app_out, &req_out, conv_id_thread.as_deref());

        // 等子进程退出, 检查 exit code (不能持锁 wait, 否则 chat_cancel 死锁)
        let child_opt = CHILDREN.remove(&req_out);
        let exit_msg: Option<String> = if let Some(mut child) = child_opt {
            // 有界等待: 卡死的孙进程 (占着管道不退) 绝不能把这个读线程永久钉住,
            // 否则一年里每次卡死都泄漏一个 ~2MB 栈的线程 → 终将 OOM。
            // 非阻塞 try_wait 轮询 + 硬死线, 到点强杀回收 (关管道) 再走异常退出路径。
            // 注意: 这里只补一道兜底, stdout/stderr 的读取与 emit、看门狗、事件载荷全不变。
            let wait_deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
            let waited: std::io::Result<Option<std::process::ExitStatus>> = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break Ok(Some(status)),
                    Ok(None) => {
                        if std::time::Instant::now() >= wait_deadline {
                            let _ = child.kill(); // 强杀回收: 关掉管道, 不让本线程泄漏
                            let _ = child.wait(); // 杀后做一次简短最终 reap
                                                  // 拿不到真实状态就当超时异常 (走下方 None 分支 → 同款错误事件)
                            break Ok(child.try_wait().ok().flatten());
                        }
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                    Err(e) => break Err(e),
                }
            };
            match waited {
                Ok(Some(status)) => {
                    if !status.success() {
                        let stderr_txt = stderr_buf_for_done.lock().clone();
                        Some(format!(
                            "claude 进程异常退出 (exit code={:?})\n--- stderr ---\n{}",
                            status.code(),
                            if stderr_txt.is_empty() {
                                "(stderr 为空)".to_string()
                            } else {
                                stderr_txt
                            }
                        ))
                    } else {
                        None
                    }
                }
                Ok(None) => {
                    let stderr_txt = stderr_buf_for_done.lock().clone();
                    Some(format!(
                        "claude 进程等待超时, 已强制回收\n--- stderr ---\n{}",
                        if stderr_txt.is_empty() {
                            "(stderr 为空)".to_string()
                        } else {
                            stderr_txt
                        }
                    ))
                }
                Err(e) => Some(format!("等待 claude 进程失败: {}", e)),
            }
        } else {
            None
        };

        if let Some(msg) = exit_msg {
            emit_event(
                &app_out,
                ChatStreamEvent {
                    req_id: req_out.clone(),
                    kind: "error".into(),
                    text: Some(msg),
                    tool: None,
                    conversation_id: conv_id_thread.clone(),
                },
            );
        }

        // 产物目录前后快照 diff: 捕获 Bash / 脚本 / Skill 生成的新增或改动文件。
        // 只上报常见成品格式; 打包应用 (含 polaris.project.json 的文件夹) 的内部文件
        // 不逐个上报, 整个应用归并成一个「应用文件夹」chip (路径带尾随 `/`)。
        let art_after = dir_snapshot(&art_dir_thread);
        for (path, mtime) in art_after.iter() {
            let changed = match art_before.get(path) {
                None => true,
                Some(old) => mtime > old,
            };
            if !changed {
                continue;
            }
            let s = if let Some(root) = packaged_project_root(path) {
                folder_artifact_repr(&root)
            } else {
                let s = path.to_string_lossy().replace('\\', "/");
                if !is_displayable_artifact(&s) {
                    continue; // 脚本 / 配置 / 临时文件等中间产物: 不进对话框
                }
                s
            };
            if !artifacts.contains(&s) {
                artifacts.push(s.clone());
                emit_event(
                    &app_out,
                    ChatStreamEvent {
                        req_id: req_out.clone(),
                        kind: "artifact".into(),
                        text: Some(s),
                        tool: None,
                        conversation_id: conv_id_thread.clone(),
                    },
                );
            }
        }

        // 落库前最后一道修剪: 实时阶段上报的文件可能事后被删 / 被归并进应用文件夹,
        // 不让「没有的文件」进历史记录 (重载历史时 chip 全部真实可点)。
        artifacts.retain(|p| {
            if let Some(dir) = p.strip_suffix('/') {
                return Path::new(dir).is_dir();
            }
            let pb = Path::new(p);
            pb.is_file() && is_displayable_artifact(p) && packaged_project_root(pb).is_none()
        });

        // 持久化 assistant 消息 (产物清单以注释 marker 形式存入正文, 重载历史时解析)
        if let Some(cid) = &conv_id_thread {
            let mut content = assistant_text.trim().to_string();
            if !artifacts.is_empty() {
                if let Ok(json) = serde_json::to_string(&artifacts) {
                    content.push_str(&format!("\n\n{}{}-->", ARTIFACT_MARKER_PREFIX, json));
                }
            }
            if !content.trim().is_empty() {
                let _ = conv::append_message(cid, "assistant", &content);
            }
        }

        emit_event(
            &app_out,
            ChatStreamEvent {
                req_id: req_out.clone(),
                kind: "done".into(),
                text: None,
                tool: None,
                conversation_id: conv_id_thread.clone(),
            },
        );
        // 本轮已终态: 清掉可能残留的「取消挂起」标记(如 stop 恰在收尾窗口内到达), 防积攒。
        CHILDREN.take_cancel(&req_out);
    });

    // stop 在「spawn 后、child 注册进 CHILDREN 前」的窄窗口内到达的兜底: 那一刻
    // chat_cancel 找不到 child 只能打标记, 这里(reader 线程已挂接后)补一次检查 ——
    // 有标记就按正常取消路径杀掉; stdout 随之关闭, reader 线程照常发 done 收尾,
    // 且 child 已先从 CHILDREN 摘除 → 不会发「异常退出」error(与 chat_cancel 同款语义)。
    if CHILDREN.take_cancel(req_id) {
        if let Some(mut c) = CHILDREN.remove(req_id) {
            kill_tree(c.id());
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    Ok(())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub fn chat_cancel(req_id: String) -> Result<(), String> {
    if let Some(mut child) = CHILDREN.remove(&req_id) {
        kill_tree(child.id()); // 先杀整树: claude 扇出的 python/node/dev server 等子孙
        let _ = child.kill(); // 再杀 claude 本体 (taskkill /T 通常已带走它, 这步兜底)
        let _ = child.wait(); // reap, 防 Unix 僵尸进程泄漏
    } else {
        // chat_send 已改为后台线程拼 prompt + spawn: stop 可能在 child 注册进 CHILDREN
        // 之前到达。打「取消挂起」标记, 后台管线在 spawn 前 / reader 挂接后各查一次,
        // 有标记即放弃 spawn 或立刻杀掉刚起的 child(见 chat_send_pipeline)。
        CHILDREN.mark_cancel(req_id);
    }
    Ok(())
}

/// 读取某会话的分批构建清单 `polaris.build.json`(分批长任务的断点/进度凭据)。
/// 前端编排循环每轮结束后读它, 算还剩几个 pending 来决定续不续、断了从哪接。
/// 不存在或解析失败返回 None(前端据此判定「还没规划」或「读不到, 当作未完成重试」)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn chat_build_manifest(conversation_id: Option<String>) -> Option<Value> {
    let path = artifacts_dir(conversation_id.as_deref()).join("polaris.build.json");
    let txt = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str::<Value>(&txt).ok()
}

/// 看门狗深检采样: root 进程树(含 root)的子孙数 + 整树累计 CPU 时间。
/// cpu 单位各平台不同(Windows 100ns / Linux jiffies / mac 秒), 只用于跨采样单调比较。
struct TreeSample {
    descendants: usize,
    cpu: u64,
}

/// 从 (pid, ppid) 全表收出以 root 为根的进程树(含 root)。`contains` 防 PID 复用造出的环。
fn collect_tree(root: u32, pairs: &[(u32, u32)]) -> Vec<u32> {
    let mut tree = vec![root];
    let mut i = 0;
    while i < tree.len() {
        let parent = tree[i];
        for &(pid, ppid) in pairs {
            if ppid == parent && pid != parent && !tree.contains(&pid) {
                tree.push(pid);
            }
        }
        i += 1;
    }
    tree
}

/// Windows: toolhelp 快照收 (pid, ppid), 再只对树内成员查 GetProcessTimes。
/// None = 快照失败(调用方退回空闲即杀)。
#[cfg(windows)]
fn sample_tree(root: u32) -> Option<TreeSample> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32First, Process32Next, PROCESSENTRY32, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let mut pairs: Vec<(u32, u32)> = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: PROCESSENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32>() as u32;
        if Process32First(snap, &mut entry) != 0 {
            loop {
                pairs.push((entry.th32ProcessID, entry.th32ParentProcessID));
                if Process32Next(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    let tree = collect_tree(root, &pairs);
    let mut cpu: u64 = 0;
    for &pid in &tree {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                continue; // 拿不到句柄(权限/刚退出)记 0: 只影响该成员的 CPU 票, 不影响单调性
            }
            let mut t: [FILETIME; 4] = std::mem::zeroed();
            if GetProcessTimes(h, &mut t[0], &mut t[1], &mut t[2], &mut t[3]) != 0 {
                for ft in [&t[2], &t[3]] {
                    cpu += ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
                }
            }
            CloseHandle(h);
        }
    }
    Some(TreeSample {
        descendants: tree.len().saturating_sub(1),
        cpu,
    })
}

/// Linux(容器/server): 单遍扫 /proc/<pid>/stat 同时拿 ppid 与 utime+stime。
/// stat 第 2 字段(comm)可含空格/括号, 一律从最后一个 ')' 之后再按空格切。
#[cfg(target_os = "linux")]
fn sample_tree(root: u32) -> Option<TreeSample> {
    let mut procs: Vec<(u32, u32, u64)> = Vec::new(); // (pid, ppid, cpu)
    for ent in std::fs::read_dir("/proc").ok()? {
        let Ok(ent) = ent else { continue };
        let name = ent.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(ent.path().join("stat")) else {
            continue;
        };
        let Some(rest) = stat.rfind(')').map(|i| &stat[i + 1..]) else {
            continue;
        };
        let f: Vec<&str> = rest.split_whitespace().collect();
        // rest 内 0-based: state=0, ppid=1, …, utime=11, stime=12
        let (Some(ppid), Some(ut), Some(st)) = (
            f.get(1).and_then(|s| s.parse::<u32>().ok()),
            f.get(11).and_then(|s| s.parse::<u64>().ok()),
            f.get(12).and_then(|s| s.parse::<u64>().ok()),
        ) else {
            continue;
        };
        procs.push((pid, ppid, ut + st));
    }
    let pairs: Vec<(u32, u32)> = procs.iter().map(|&(p, pp, _)| (p, pp)).collect();
    let tree = collect_tree(root, &pairs);
    let cpu = procs
        .iter()
        .filter(|(p, _, _)| tree.contains(p))
        .map(|&(_, _, c)| c)
        .sum();
    Some(TreeSample {
        descendants: tree.len().saturating_sub(1),
        cpu,
    })
}

/// macOS 及其它 unix: 一次 `ps -axo pid=,ppid=,cputime=` 全表。cputime 形如
/// "0:00.12" / "1:02:03" / "1-02:03:04", 解析成秒(只求单调可比)。
#[cfg(all(unix, not(target_os = "linux")))]
fn sample_tree(root: u32) -> Option<TreeSample> {
    fn parse_cputime(s: &str) -> u64 {
        let (days, rest) = match s.split_once('-') {
            Some((d, r)) => (d.parse::<u64>().unwrap_or(0), r),
            None => (0, s),
        };
        let mut secs = 0f64;
        for part in rest.split(':') {
            secs = secs * 60.0 + part.parse::<f64>().unwrap_or(0.0);
        }
        days * 86400 + secs as u64
    }
    let out = Command::new("ps")
        .args(["-axo", "pid=,ppid=,cputime="])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut procs: Vec<(u32, u32, u64)> = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let (Some(pid), Some(ppid)) = (
            f.first().and_then(|s| s.parse::<u32>().ok()),
            f.get(1).and_then(|s| s.parse::<u32>().ok()),
        ) else {
            continue;
        };
        procs.push((pid, ppid, f.get(2).map_or(0, |s| parse_cputime(s))));
    }
    let pairs: Vec<(u32, u32)> = procs.iter().map(|&(p, pp, _)| (p, pp)).collect();
    let tree = collect_tree(root, &pairs);
    let cpu = procs
        .iter()
        .filter(|(p, _, _)| tree.contains(p))
        .map(|&(_, _, c)| c)
        .sum();
    Some(TreeSample {
        descendants: tree.len().saturating_sub(1),
        cpu,
    })
}

// ───────────────────────── Internals ─────────────────────

/// token 级部分流(`--include-partial-messages`)开关,默认开。
/// `POLARIS_PARTIAL_STREAM=0` 关闭 —— 兼容不认识该 flag 的旧版 claude CLI(老 CLI 见到未知
/// 参数会直接拒跑);关掉后回到「整块 assistant 事件」粒度,除了不逐字外行为完全一致。
fn partial_stream_enabled() -> bool {
    std::env::var("POLARIS_PARTIAL_STREAM")
        .map(|v| v.trim() != "0")
        .unwrap_or(true)
}

/// 部分流状态:token 级 `stream_event` delta 与随后整块 `assistant` 事件之间的去重记账。
/// 落库口径始终是整块 assistant 事件(权威、含完整 content);delta 只负责「屏上逐字长出来」。
#[derive(Default)]
struct PartialStreamState {
    /// 当前消息已用 token delta 流出过文本 → 它的整块 assistant 事件只记账(进 accum)不再重复显示。
    msg_streamed: bool,
    /// 本请求曾流出过任何 token delta → result 兜底不再重复显示(正文已经在屏上)。
    ever_streamed: bool,
    /// 本请求曾收到过**整块 assistant 事件**的非空文本(权威记账口径)→ result 兜底不再补账。
    /// 不能复用「accum 非空」判断: image_notice 会把中文说明预置进 accum, 使它永远非空,
    /// 整块事件真缺席(进程中途崩)时 result 兜底被废、最终文本丢失 —— 预置 notice 不置本位。
    saw_assistant_text: bool,
}

/// delta 合批器: 纯 text_delta 文本先攒进缓冲, 距上次 emit ≥30ms 才发一条合并 delta
/// (payload 结构不变, 只是多条 delta 的 text 拼成一条); 任何非 delta 事件到达前、以及
/// 流结束时由调用方 flush —— 事件相对顺序与逐条 emit 完全一致, 只是 IPC 频率从
/// 「每 token 一次」降到「每 30ms 一次」。
struct DeltaBatcher {
    buf: String,
    last_emit: std::time::Instant,
}

/// 合批时间窗(毫秒): 距上次 emit 不足此值的 text_delta 先攒着。
const DELTA_BATCH_WINDOW_MS: u64 = 30;

impl DeltaBatcher {
    fn new() -> Self {
        Self {
            buf: String::new(),
            last_emit: std::time::Instant::now(),
        }
    }
    /// 累积一段 text_delta; 距上次 emit ≥ 时间窗即 flush。
    fn push(&mut self, app: &AppHandle, req_id: &str, conv_id: Option<&str>, txt: &str) {
        self.buf.push_str(txt);
        if self.last_emit.elapsed() >= std::time::Duration::from_millis(DELTA_BATCH_WINDOW_MS) {
            self.flush(app, req_id, conv_id);
        }
    }
    /// 把缓冲里的文本合成**一条** delta 事件发出(缓冲为空则什么都不发)。
    fn flush(&mut self, app: &AppHandle, req_id: &str, conv_id: Option<&str>) {
        if self.buf.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.buf);
        emit_event(
            app,
            ChatStreamEvent {
                req_id: req_id.into(),
                kind: "delta".into(),
                text: Some(text),
                tool: None,
                conversation_id: conv_id.map(|s| s.to_string()),
            },
        );
        self.last_emit = std::time::Instant::now();
    }
}

fn handle_stream_event(
    app: &AppHandle,
    req_id: &str,
    conv_id: Option<&str>,
    v: &Value,
    accum: &mut String,
    artifacts: &mut Vec<String>,
    ps: &mut PartialStreamState,
    batcher: &mut DeltaBatcher,
) {
    let t = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
    match t {
        // --include-partial-messages 打开后 CLI 吐的 token 级增量:钻进 content_block_delta 的
        // text_delta 逐字上屏(豆包/ChatGPT 式)。**不进 accum** —— 完整文本稍后随整块 assistant
        // 事件到达并记账,这里只管显示;若整块事件缺席(进程中途崩),result 兜底仍按 accum 口径落库。
        // 纯 text_delta 走合批器(30ms 窗); 其它 stream_event 子类型(content_block_stop/
        // message_stop 等)先 flush, 保证顺序不变。
        "stream_event" => {
            let ev = v.get("event");
            let et = ev
                .and_then(|e| e.get("type"))
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let mut pushed = false;
            if et == "content_block_delta" {
                if let Some(d) = ev.and_then(|e| e.get("delta")) {
                    if d.get("type").and_then(|x| x.as_str()) == Some("text_delta") {
                        if let Some(txt) = d.get("text").and_then(|x| x.as_str()) {
                            if !txt.is_empty() {
                                ps.msg_streamed = true;
                                ps.ever_streamed = true;
                                batcher.push(app, req_id, conv_id, txt);
                                pushed = true;
                            }
                        }
                    }
                }
            }
            if !pushed {
                batcher.flush(app, req_id, conv_id);
            }
        }
        "assistant" => {
            // 整块事件(text/tool_use)到达: 先 flush 挂起的合批 delta, 保证事件顺序不变。
            batcher.flush(app, req_id, conv_id);
            if let Some(content) = v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            {
                for block in content {
                    let bt = block.get("type").and_then(|x| x.as_str()).unwrap_or("");
                    match bt {
                        "text" => {
                            if let Some(txt) = block.get("text").and_then(|x| x.as_str()) {
                                accum.push_str(txt);
                                // 非空整块文本已记账 → result 兜底不必再补(见 saw_assistant_text 注释)。
                                if !txt.is_empty() {
                                    ps.saw_assistant_text = true;
                                }
                                // 本消息的文本已经逐字流出过 → 整块事件只记账,不再重复上屏。
                                if !ps.msg_streamed {
                                    emit_event(
                                        app,
                                        ChatStreamEvent {
                                            req_id: req_id.into(),
                                            kind: "delta".into(),
                                            text: Some(txt.to_string()),
                                            tool: None,
                                            conversation_id: conv_id.map(|s| s.to_string()),
                                        },
                                    );
                                }
                            }
                        }
                        "tool_use" => {
                            let name = block
                                .get("name")
                                .and_then(|x| x.as_str())
                                .unwrap_or("unknown");
                            // 输入摘要(命令/路径/检索词等一行) → 前端工具 pill 可展开看详情
                            let summary = block.get("input").and_then(tool_input_summary);
                            emit_event(
                                app,
                                ChatStreamEvent {
                                    req_id: req_id.into(),
                                    kind: "tool".into(),
                                    text: summary,
                                    tool: Some(name.to_string()),
                                    conversation_id: conv_id.map(|s| s.to_string()),
                                },
                            );
                            // 写文件类工具 → 记一个成品文件 (实时反馈)
                            if matches!(name, "Write" | "Edit" | "MultiEdit" | "NotebookEdit") {
                                let fp = block
                                    .get("input")
                                    .and_then(|i| {
                                        i.get("file_path").or_else(|| i.get("notebook_path"))
                                    })
                                    .and_then(|x| x.as_str());
                                if let Some(fp) = fp {
                                    let norm = fp.replace('\\', "/");
                                    // 只展示常见成品格式; 应用文件夹内部文件不单独展示
                                    // (收尾快照统一归并成一个文件夹 chip), 防中间产物刷屏
                                    if is_displayable_artifact(&norm)
                                        && packaged_project_root(Path::new(&norm)).is_none()
                                        && !artifacts.contains(&norm)
                                    {
                                        artifacts.push(norm.clone());
                                        emit_event(
                                            app,
                                            ChatStreamEvent {
                                                req_id: req_id.into(),
                                                kind: "artifact".into(),
                                                text: Some(norm),
                                                tool: None,
                                                conversation_id: conv_id.map(|s| s.to_string()),
                                            },
                                        );
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            // 一条消息一个整块事件:处理完毕即复位,下一条消息的 delta 重新记账。
            ps.msg_streamed = false;
        }
        "result" => {
            // 收尾事件: 先 flush 挂起的合批 delta。
            batcher.flush(app, req_id, conv_id);
            // result 事件: claude --print 模式收尾, result 字段是最终文本
            if let Some(txt) = v.get("result").and_then(|x| x.as_str()) {
                // 若前面已经有整块 assistant text, result 通常是同一内容的最终版, 不重复记账。
                // 判据是 saw_assistant_text 而非「accum 非空」: image_notice 预置会让 accum
                // 永远非空, 把这条「整块事件缺席」的兜底彻底废掉(最终文本静默丢失)。
                if !ps.saw_assistant_text {
                    accum.push_str(txt);
                    // 曾逐字流出过 → 正文已在屏上(只是整块事件缺席没记上账),补账不补屏。
                    if !ps.ever_streamed {
                        emit_event(
                            app,
                            ChatStreamEvent {
                                req_id: req_id.into(),
                                kind: "delta".into(),
                                text: Some(txt.to_string()),
                                tool: None,
                                conversation_id: conv_id.map(|s| s.to_string()),
                            },
                        );
                    }
                }
            }
            // error subtype
            if let Some(subtype) = v.get("subtype").and_then(|x| x.as_str()) {
                if subtype.starts_with("error") {
                    let msg = v
                        .get("result")
                        .and_then(|x| x.as_str())
                        .unwrap_or("(unknown error)")
                        .to_string();
                    emit_event(
                        app,
                        ChatStreamEvent {
                            req_id: req_id.into(),
                            kind: "error".into(),
                            text: Some(format!("[result error: {}] {}", subtype, msg)),
                            tool: None,
                            conversation_id: conv_id.map(|s| s.to_string()),
                        },
                    );
                }
            }
        }
        // 其它事件类型(system/user 回显等): 也算「非 delta 事件」, flush 兜底防缓冲滞留。
        _ => {
            batcher.flush(app, req_id, conv_id);
        }
    }
}

fn emit_event(app: &AppHandle, ev: ChatStreamEvent) {
    let _ = app.emit("chat:stream", ev);
}

// Docker-in-Docker 沙箱仅桌面构建可用 (依赖 polaris_sandbox crate)；
// server(容器内)本期降级，不编译此路径。
#[cfg(feature = "desktop")]
#[allow(dead_code)]
fn spawn_in_sandbox(prompt: &str, perm: &str) -> Result<Child, String> {
    let perm_flag = format!("--permission-mode={}", perm);
    // 联网 + (非只读档位)本地读写执行, 让成品能真正产出
    let allowed = allowed_tools_for(perm, false);
    // 沙箱内 KB 永远挂在 /kb (sandbox_start 时挂载),
    // 这里让 claude 把 /kb 也加进可读目录,并以 /workspace 为 cwd
    let mut cmd = Command::new("docker");
    cmd.args([
        "exec",
        "-i",
        "-w",
        "/workspace",
        // 与 polaris_sandbox::CONTAINER_NAME 同值。字面内联以斩断 kernel→sandbox 的
        // 向上依赖(红线 R3);本函数是 dead_code 保留路径,真启用时应经壳层桥接注入。
        "polaris-sandbox",
        "claude",
        "--print",
        "--output-format",
        "stream-json",
        "--verbose",
    ]);
    // token 级部分流(同 spawn_on_host);flag 必须在 prompt 位置参数之前。
    if partial_stream_enabled() {
        cmd.arg("--include-partial-messages");
    }
    cmd.args([
        "--add-dir",
        "/kb",
        "--allowedTools",
        &allowed,
        &perm_flag,
        prompt,
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    no_window(&mut cmd); // 隐藏式: 不弹控制台窗口
    let child = cmd
        .spawn()
        .map_err(|e| format!("在沙箱内调起 claude 失败: {}", e))?;
    Ok(child)
}

fn spawn_on_host(
    prompt: &str,
    perm: &str,
    art_dir: &Path,
    with_task: bool,
    work_full: bool,
    provider_id: Option<&str>,
) -> Result<Child, String> {
    let perm_flag = format!("--permission-mode={}", perm);
    // cwd = polaris-app 根 (env!("CARGO_MANIFEST_DIR") 的父级),
    // 这样 claude CLI 自动信任整棵 polaris-app/ 子树, 包括 PolarisKB/
    let cwd = claude_md::project_root().unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });

    // KB 根写一份 .ignore(幂等), 让 ripgrep(Grep 工具/rg)自动跳 output/二进制/大素材 → 检索更快
    ensure_kb_search_ignore();

    // 如果 KB root 不在 cwd 子树下(用户可能把 KB 移到别处), 用 --add-dir 显式放行
    let kb_root = std::path::PathBuf::from(
        super::bridges::kb_bridge()
            .map(|b| b.root())
            .unwrap_or_default(),
    );
    let mut extra_dirs: Vec<String> = Vec::new();
    if !kb_root.as_os_str().is_empty() && kb_root.exists() && !kb_root.starts_with(&cwd) {
        extra_dirs.push("--add-dir".into());
        extra_dirs.push(kb_root.to_string_lossy().to_string());
    }
    // 产物目录在 ~/Polaris 下, 不在 cwd 子树, 显式放行 claude 可写入
    if art_dir.exists() && !art_dir.starts_with(&cwd) {
        extra_dirs.push("--add-dir".into());
        extra_dirs.push(art_dir.to_string_lossy().to_string());
    }

    let mut args: Vec<String> = vec![
        "--print".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
    ];
    // token 级部分流:CLI 吐 content_block_delta 逐字增量 → 前端豆包式逐字上屏。
    // 默认开;POLARIS_PARTIAL_STREAM=0 关(兼容旧版 CLI,见 partial_stream_enabled)。
    if partial_stream_enabled() {
        args.push("--include-partial-messages".into());
    }
    args.extend(extra_dirs);
    // 联网工具默认放行; 非「拒绝授权」档位再叠加本地读写执行 (Bash/PowerShell/文件),
    // 否则 headless 下连 `python xxx.py` 都被拒, .pptx/.xlsx 这类成品根本产不出来。
    args.push("--allowedTools".into());
    args.push(allowed_tools_for(perm, with_task));
    // 快速模式: 弃用冗余工具(Task/NotebookEdit)。disallowedTools 优先级高于 allowedTools。
    if let Some(disallowed) = disallowed_tools_for(work_full, with_task) {
        args.push("--disallowedTools".into());
        args.push(disallowed);
    }
    // 模型档跟随模式(可选, 默认不启用): 多模型供应商上可让快速模式走快档(便宜快)、工作模式走强档。
    // 仅当对应环境变量显式设了 model id 才传 --model —— 单模型网关/未配置时**保持原样**(供应商
    // 钉死的模型), 绝不因传错 model 名把请求打挂。POLARIS_WORK_MODEL / POLARIS_FAST_MODEL。
    let model_env = if work_full {
        "POLARIS_WORK_MODEL"
    } else {
        "POLARIS_FAST_MODEL"
    };
    if let Some(m) = std::env::var(model_env)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        args.push("--model".into());
        args.push(m);
    }
    args.push(perm_flag);
    // ⚠️ prompt 不再塞 argv 末尾 —— 走 stdin。
    // Windows CreateProcessW 的 lpCommandLine 上限 32767 字符, 你 KB 全文 + 多轮对话历史
    // 拼一起轻松爆, 直接抛 206 ERROR_FILENAME_TOO_LONG 拒 spawn (实测 33k 字符就 100% 复现)。
    // 改 stdin 后 prompt 长度无限制。kb.rs 的 spawn_in_sandbox 早就这么干了 (注释在那)。
    let _ = prompt; // 函数签名仍保留 prompt 参数, 调用方写 stdin

    // 解析 claude 可执行文件的全路径再 spawn, 而非裸名 "claude":
    // npm 装只在 PATH 放 `claude.cmd`, 而 Windows CreateProcessW 解析裸名只补 `.exe`、不查 PATHEXT
    // → 裸名找不到 npm 装的 claude。resolve_claude_exe 会挖出真·原生 exe (原生装 / npm 装通吃);
    // 解析不到再回退裸名靠 PATH (兼容用户自行配好的环境)。
    let claude_bin: std::ffi::OsString = crate::doctor::resolve_claude_exe()
        .map(|p| p.into_os_string())
        .unwrap_or_else(|| "claude".into());
    let mut cmd = Command::new(&claude_bin);
    cmd.args(&args)
        .current_dir(&cwd)
        .stdin(Stdio::piped()) // 接 prompt 用, 调用方 spawn 后 write + drop
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Windows: claude 跑 Bash 工具要靠 Git Bash。启动期 prime 通常已设好 CLAUDE_CODE_GIT_BASH_PATH,
    // 但若 Git Bash 是 app 启动后才装的, 这里兜底显式喂给子进程 —— 免得 claude 扫不到 shell。
    #[cfg(windows)]
    if std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH").is_none() {
        if let Some(bash) = crate::doctor::detect_git_bash() {
            cmd.env("CLAUDE_CODE_GIT_BASH_PATH", bash);
        }
    }
    // 子进程环境净化: loopback 强制 NO_PROXY (切 Codex 时 claude 走 127.0.0.1 本地代理,
    // 系统代理会劫持回环 → 连不上) + 清 DEBUG/LD_PRELOAD。见 doctor::harden_child_env。
    crate::doctor::harden_child_env(&mut cmd);
    // 隔离模式跑第三方 → CLAUDE_CONFIG_DIR 指私有目录, 会话账本不进 ~/.claude/projects,
    // cc-switch 等外部监控不再看见 Polaris 自动任务的第三方会话。
    // provider_id 指定了本对话这家 → 逐命令注入它的 env(真隔离, 与全局开关/其它对话解耦);
    // None/"auto" → 回落全局当前供应商(Auto 档)。统一走这一个入口。
    crate::provider::scope_child_claude_by_id(&mut cmd, provider_id);
    no_window(&mut cmd); // 隐藏式: 每次发消息不再弹出黑色终端窗口

    // Linux/容器: 让 claude 成为新进程组的组长 (setpgid)。这样 kill_tree 的
    // `kill -TERM -<pid>` 能一次带走 claude 扇出的 python/node/dev-server 整棵子孙树,
    // 不留孤儿占端口/CPU —— 对容器内长稳运行(>3h, 反复发消息)至关重要。
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    cmd.spawn().map_err(|e| {
        // 错误只在 spawn 本身失败 (e.g. exe 找不到), 不再是 prompt 太长
        format!("调起宿主机 claude CLI 失败: {}", e)
    })
}

/// 从 tool_use 的 input JSON 里提一行人能看懂的摘要(命令/文件路径/检索词)。
fn tool_input_summary(input: &serde_json::Value) -> Option<String> {
    const KEYS: [&str; 10] = [
        "command",
        "file_path",
        "notebook_path",
        "pattern",
        "query",
        "url",
        "description",
        "prompt",
        "path",
        "skill",
    ];
    for k in KEYS {
        if let Some(s) = input.get(k).and_then(|x| x.as_str()) {
            let s = s.trim();
            if s.is_empty() {
                continue;
            }
            let one_line = s.lines().next().unwrap_or(s);
            let mut out: String = one_line.chars().take(120).collect();
            if one_line.chars().count() > 120 || s.lines().count() > 1 {
                out.push('…');
            }
            return Some(out);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 看门狗深检:collect_tree 收全子孙、不越界、PID 复用成环不死循环。
    #[test]
    fn collect_tree_gathers_descendants_and_survives_cycles() {
        // 100 → 200 → 300; 400 无关; 500 是 100 的另一子
        let pairs = [(200, 100), (300, 200), (400, 999), (500, 100)];
        let mut tree = collect_tree(100, &pairs);
        tree.sort();
        assert_eq!(tree, vec![100, 200, 300, 500]);
        // PID 复用把根的祖先又标成树内成员的孩子(100 ← 300 成环): 必须终止且不重复
        let cyclic = [(200, 100), (300, 200), (100, 300)];
        let mut t2 = collect_tree(100, &cyclic);
        t2.sort();
        assert_eq!(t2, vec![100, 200, 300]);
    }

    /// 真行为验证:cmd 拉起 ping 子进程后, sample_tree 必须看到 ≥1 个子孙
    /// (这是「静默但在干活的长任务不被看门狗误杀」的判据本体)。
    #[cfg(windows)]
    #[test]
    fn sample_tree_sees_live_descendants() {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", "ping -n 6 127.0.0.1 >NUL"]);
        no_window(&mut cmd);
        let mut child = cmd.spawn().expect("spawn cmd");
        std::thread::sleep(std::time::Duration::from_millis(800)); // 等 ping 起来
        let s = sample_tree(child.id()).expect("windows toolhelp 快照应可用");
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            s.descendants >= 1,
            "cmd 的 ping 子进程应被数进树, 实得 {}",
            s.descendants
        );
    }
}
