//! 构建知识网 kb_compile —— 自原 kb/compile.rs 纯移动(Phase 1 落户 polaris-wiki 仓)。
//! headless 只读管线/JSON 提取已下沉 kernel::headless, 维护锁/原子落盘归位 kb::maintain。

use crate::kb::{acquire_kb_task, kb_reindex, kb_root_pathbuf, KB_COMPILE_COUNTER};
#[cfg(not(feature = "desktop"))]
use crate::host::AppHandle;
use serde::Serialize;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(feature = "desktop")]
use tauri::{AppHandle, Emitter};

// ───────────────────────── 构建知识网 (摄入即编译 Ingest=Compile) ─────────────────────────
//
// Karpathy LLM-Wiki 的核心是「写的那一半」: 摄入资料时让 LLM 读原文、抽实体/概念、
// 在 wiki/ 写页面、落 [[双链]]、记账 index/log —— 交叉引用「早就写好了」, 知识因此互联成网。
// 旧「构建索引」(kb_scan) 只重扫文件、刷新内存, 不产生任何新知识与新关联。
// kb_compile 就是补上的编译器: 复用 chat.rs 已验证的 headless `claude --print` 管线,
// 给一个带写权限(Read/Write/Edit/Glob/Grep)的 claude 进程当「wiki 维护者」, 让它自己
// Read 原文、Write wiki 页 —— 与现有架构天然契合, 不引入新的 LLM API / 向量依赖。

// 维护互斥(KB_TASK_BUSY/acquire_kb_task)与原子落盘(kb_atomic_write)已归位
// kb::maintain(enrich/dedup 同在 fable 仓共用), 本文件经 crate::kb 门面取用。

/// 编译进度事件 (前端「构建知识网」进度面板订阅 `kb:compile`)。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KbCompileEvent {
    pub run_id: String,
    /// phase | tool | page | delta | done | error
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// 仅 done 事件: 编译后重扫得到的文档总数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_count: Option<usize>,
}

pub(crate) fn emit_compile(app: &AppHandle, run_id: &str, kind: &str, text: Option<String>) {
    let _ = app.emit(
        "kb:compile",
        KbCompileEvent {
            run_id: run_id.into(),
            kind: kind.into(),
            text,
            doc_count: None,
        },
    );
}

/// 「wiki 维护者」system prompt —— Karpathy 式「摄入即编译」。clean-room 自写, 只学方法论。
pub(crate) fn compile_directive(root_disp: &str) -> String {
    format!(
        "# 角色：知识库 wiki 维护者 (Karpathy 式 LLM-Wiki)\n\n\
你是这个知识库的**维护者**。知识库根目录就在你的工作目录: `{root}`。\n\
它分三层:\n\
- `raw/` — 原始资料, **只读, 严禁写入或修改**。\n\
- `wiki/` — **由你全权拥有的知识层**: 摘要页 / 实体页 / 概念页 / 综合页。你在这里写。\n\
- `output/` — 生成的报告类成品。\n\n\
## 你这一轮的任务：摄入即编译 (Ingest = Compile)\n\n\
把 `raw/` 里的原始资料**编译**成一张互联的知识网, 而不是简单罗列。具体:\n\n\
1. **先读规则与现状**: 读 `CLAUDE.md`(若有) 了解约定; 读 `wiki/index.md` 和 `wiki/` 下已有页面, 知道已经有什么。\n\
2. **盘点资料**: 用 Glob/Grep 扫 `raw/`, 了解有哪些资料、主题是什么。**不要逐篇全文读**, 靠文件名和 Grep 抽样了解即可, 控制成本。\n\
3. **抽取并撰写知识 (核心)**: 识别贯穿资料的**实体**(人/地/组织/事件)与**概念/思想脉络**(反复出现的主题、论点)。\
概念页放 `wiki/概念/`、实体页放 `wiki/实体/`(没有就新建子目录); 在页面里**用 `[[页面标题]]` 双链**指向相关的其它 wiki 页, 并用 Grep 找出哪些 raw 篇目讲了它、列进 frontmatter 的 `sources` 并在正文引用。\
这一步的目的是**建立关联**: 原本互不相连的资料, 经由共同的概念页/实体页被串成网。\n\
4. **记账**: 更新 `wiki/index.md` (每个 wiki 页一行: `- [[标题]] — 一句话摘要`, 按类型分组);\
追加 `wiki/log.md` (一行: `## [今天日期] compile | 本轮做了什么`, 没有就新建)。\n\n\
## 页面格式 (每个新建/更新的 wiki 页都要带 frontmatter)\n\n\
```\n\
---\n\
title: 页面标题\n\
type: concept        # entity | concept | source | synthesis 之一\n\
sources: [\"raw/某资料.md\"]   # 这页依据的原始资料相对路径, 可多个\n\
---\n\
\n\
正文... 用 [[其它页面]] 互联, 用脚注/引用标注来源, 不要编造 raw/ 里没有的事实。\n\
```\n\n\
## 针对「语料型」知识库 (如大量同质篇目、彼此几乎无双链)\n\n\
不要逐篇浅摘就完事。**优先抽思想脉络的概念页**(例如把反复出现的主题各立一个概念页),\
在概念页里用 `[[…]]` 把相关篇目链接进来 —— 让原本散落的篇目经由概念层互联成脉络。\
这一轮重在**覆盖度与连接**(把散点连成网), 不必把每篇都深挖到底。\n\n\
## 硬约束\n\n\
- **绝不修改或写入 `raw/`**。只读它。\n\
- 不编造资料里没有的内容; 拿不准的事写进 `wiki/` 时标注「待核实」。\n\
- 双链统一用 `[[页面标题]]` 形式 (标题=对应 wiki 文件名去掉 .md)。\n\
- 全程用中文撰写 wiki 页。\n\n\
完成后, 用一两句话总结你**新建/更新了哪些 wiki 页**、建立了哪些关联。现在开始。",
        root = root_disp
    )
}

#[cfg_attr(not(windows), allow(unused_variables))]
pub(crate) fn compile_no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: GUI 进程 spawn 控制台子进程时不弹黑窗
        cmd.creation_flags(0x0800_0000);
    }
}

/// 「构建知识网」: 启动一个有写权限的 headless claude 当 wiki 维护者, 把 raw/ 编译进 wiki/。
/// 立即返回 run_id; 进度通过 `kb:compile` 事件流式推送, 完成时发 `done` (附重扫后的文档数)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn kb_compile(app: AppHandle) -> Result<String, String> {
    let root = kb_root_pathbuf();
    if root.as_os_str().is_empty() || !root.exists() {
        return Err("知识库根目录不存在, 请先在「管理」里设置".into());
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let c = KB_COMPILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let run_id = format!("kbc-{:x}-{:x}", ts, c);

    let claude_bin: std::ffi::OsString = crate::doctor::resolve_claude_exe()
        .map(|p| p.into_os_string())
        .unwrap_or_else(|| "claude".into());
    let root_disp = root.to_string_lossy().replace('\\', "/");
    let prompt = compile_directive(&root_disp);

    let _kb_task = acquire_kb_task()?;
    let run_id_thread = run_id.clone();
    std::thread::spawn(move || {
        let _kb_task = _kb_task; // 持锁直到本线程结束(Drop 释放)
        emit_compile(
            &app,
            &run_id_thread,
            "phase",
            Some("启动 wiki 维护者…".into()),
        );

        // prompt 经 stdin 喂给 claude (而非命令行参数): 大 prompt 不会撞 Windows 命令行
        // 长度上限, 也不会因 prompt 以 `-` 开头被当成 flag —— 实测 argv 路径在某些 shell 下
        // 会触发 claude 的「Input must be provided」直接退 1, stdin 管道稳。
        let mut cmd = Command::new(&claude_bin);
        cmd.args([
            "--print",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-mode=bypassPermissions",
            "--allowedTools",
            "Read,Write,Edit,Glob,Grep",
        ])
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        crate::doctor::harden_child_env(&mut cmd); // loopback NO_PROXY + 清干扰变量
        crate::provider::scope_child_claude(&mut cmd); // 隔离模式第三方 → 私有会话账本
        compile_no_window(&mut cmd);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                emit_compile(
                    &app,
                    &run_id_thread,
                    "error",
                    Some(format!("调起 claude 失败: {e}")),
                );
                let _ = app.emit(
                    "kb:compile",
                    KbCompileEvent {
                        run_id: run_id_thread.clone(),
                        kind: "done".into(),
                        text: Some("编译未启动".into()),
                        doc_count: None,
                    },
                );
                return;
            }
        };

        // 把 prompt 写进 stdin 并关闭 (drop 即关), claude 读到 EOF 后开始干活
        if let Some(mut si) = child.stdin.take() {
            use std::io::Write as _;
            let _ = si.write_all(prompt.as_bytes());
            // si 在此作用域结束时 drop → stdin 关闭, 触发 claude 开始处理
        }

        // stderr: 累积, 退出非零时给原因 (stream-json 模式下通常为空, 仅崩溃时有内容)
        let stderr_buf = std::sync::Arc::new(parking_lot::Mutex::new(String::new()));
        if let Some(se) = child.stderr.take() {
            let buf = stderr_buf.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(se)
                    .lines()
                    .map_while(std::result::Result::ok)
                {
                    if !line.trim().is_empty() {
                        buf.lock().push_str(&line);
                        buf.lock().push('\n');
                    }
                }
            });
        }

        // stdout 管道先摘出来,再把 child 本体登记进全局回收池:App 退出钩子 kill_all
        // 能收割它,不再是「关 App 后继续写知识库的孤儿」(违反子进程整树回收铁律)。
        let so_pipe = child.stdout.take();
        polaris_runtime::procs::CHILDREN.insert(run_id_thread.clone(), child);
        // 硬顶看门狗:claude 挂死时本线程会永远卡在 stdout 读循环,_kb_task 维护锁
        // 随之永不释放 → 所有 KB 维护任务(enrich/dedup/compile)被锁死。超时杀树后
        // stdout EOF,读循环自然解阻塞。默认 60 分钟,POLARIS_KB_COMPILE_CAP_SECS 可调。
        let cap_secs: u64 = std::env::var("POLARIS_KB_COMPILE_CAP_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&s| s > 0)
            .unwrap_or(3600);
        {
            let rid = run_id_thread.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(cap_secs));
                polaris_runtime::procs::CHILDREN.kill(&rid); // 已正常结束则 no-op
            });
        }

        // stdout: 解析 stream-json, 把工具调用 / 写页面 / 文本翻成进度
        let mut pages: Vec<String> = Vec::new();
        if let Some(so) = so_pipe {
            emit_compile(
                &app,
                &run_id_thread,
                "phase",
                Some("读取资料、抽取实体与概念…".into()),
            );
            for line in BufReader::new(so)
                .lines()
                .map_while(std::result::Result::ok)
            {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                if v.get("type").and_then(|x| x.as_str()) != Some("assistant") {
                    // result 事件的错误子类型 → 透传
                    if v.get("type").and_then(|x| x.as_str()) == Some("result") {
                        if let Some(st) = v.get("subtype").and_then(|x| x.as_str()) {
                            if st.starts_with("error") {
                                let msg = v
                                    .get("result")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("(unknown)")
                                    .to_string();
                                emit_compile(
                                    &app,
                                    &run_id_thread,
                                    "error",
                                    Some(format!("[{st}] {msg}")),
                                );
                            }
                        }
                    }
                    continue;
                }
                let Some(content) = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                else {
                    continue;
                };
                for block in content {
                    match block.get("type").and_then(|x| x.as_str()) {
                        Some("tool_use") => {
                            let name = block.get("name").and_then(|x| x.as_str()).unwrap_or("");
                            if matches!(name, "Write" | "Edit" | "MultiEdit") {
                                if let Some(fp) = block
                                    .get("input")
                                    .and_then(|i| i.get("file_path"))
                                    .and_then(|x| x.as_str())
                                {
                                    let norm = fp.replace('\\', "/");
                                    let short =
                                        norm.rsplit('/').next().unwrap_or(&norm).to_string();
                                    if !pages.contains(&norm) {
                                        pages.push(norm);
                                    }
                                    emit_compile(
                                        &app,
                                        &run_id_thread,
                                        "page",
                                        Some(format!("写入 {short}")),
                                    );
                                }
                            } else {
                                emit_compile(&app, &run_id_thread, "tool", Some(name.to_string()));
                            }
                        }
                        Some("text") => {
                            if let Some(t) = block.get("text").and_then(|x| x.as_str()) {
                                let t = t.trim();
                                if !t.is_empty() {
                                    emit_compile(
                                        &app,
                                        &run_id_thread,
                                        "delta",
                                        Some(t.to_string()),
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // 从回收池摘回并收割;None = 看门狗已超时杀掉(或 App 退出时被 kill_all 收走)。
        let status = match polaris_runtime::procs::CHILDREN.remove(&run_id_thread) {
            Some(mut c) => c.wait(),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("超过硬顶时限 {cap_secs}s 被看门狗终止"),
            )),
        };
        // 编译完成 → 重扫刷新内存索引 + 图谱
        let n = kb_reindex();

        let ok = matches!(&status, Ok(s) if s.success());
        if !ok {
            let code = match &status {
                Ok(s) => format!("{:?}", s.code()),
                Err(e) => e.to_string(), // 看门狗超时/等待失败的原因直接透传
            };
            let se = stderr_buf.lock().clone();
            emit_compile(
                &app,
                &run_id_thread,
                "error",
                Some(format!(
                    "claude 退出码 {code}{}",
                    if se.is_empty() {
                        String::new()
                    } else {
                        format!(" — {se}")
                    }
                )),
            );
        }
        let msg = if ok {
            format!(
                "编译完成: 新建/更新 {} 个页面, 知识库共 {} 篇",
                pages.len(),
                n
            )
        } else {
            "编译中断 (见上方原因), 已刷新索引".into()
        };
        let _ = app.emit(
            "kb:compile",
            KbCompileEvent {
                run_id: run_id_thread.clone(),
                kind: "done".into(),
                text: Some(msg),
                doc_count: Some(n),
            },
        );
    });

    Ok(run_id)
}

// ───────────────────────── 只读 claude → 收集 JSON ─────────────────────────
// 已下沉 polaris-kernel::headless(fable 与 wiki 共用, 引擎间禁止互引):
// run_claude_readonly / run_claude_readonly_timeout / extract_balanced_json。
// 旧路径 `kb::run_claude_readonly` 等由 fable 仓的 kb 门面转发保住。
