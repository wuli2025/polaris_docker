//! Headless claude 只读管线 + JSON 决策提取 —— 内核横切能力。
//!
//! 原居 kb/compile.rs(Wave B 基础设施), 分仓 Phase 1 下沉内核: fable(回声蒸馏/文件
//! 中心 LLM/本体抽取/检索 AI 扩写)与 wiki(enrich/dedup)都在用, 而引擎间禁止互引 ——
//! 它只依赖 doctor(定位 claude/加固子环境)与 provider(会话隔离), 天然属于 kernel。
//!
//! 模式(借鉴 llm_wiki「让 AI 只出决策数据, 代码执行改动」): 起一个**只读**
//! (allowedTools 仅 Read/Glob/Grep, 物理上无法写文件)的 headless claude, 让它读资料、
//! 输出一段 JSON 决策, 把全部 assistant 文本收集起来返回; 改文件由 Rust 做。

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

#[cfg_attr(not(windows), allow(unused_variables))]
fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: GUI 进程 spawn 控制台子进程时不弹黑窗
        cmd.creation_flags(0x0800_0000);
    }
}

/// 起一个只读 headless claude, 把 prompt 经 stdin 喂进去, 收集其全部 assistant 文本块返回。
/// `on_event(kind, text)`: kind ∈ {tool, delta} 用于向前端透传进度。阻塞直到进程退出。
pub fn run_claude_readonly<F: FnMut(&str, &str)>(
    root: &Path,
    prompt: &str,
    on_event: F,
) -> Result<String, String> {
    run_claude_readonly_inner(root, prompt, on_event, None)
}

/// 同 `run_claude_readonly`,但带墙钟超时:到点 kill 子进程并整树回收,返回 Err。
/// 用于检索 AI 扩写等「卡住必须能放手」的路径(阻塞线程池有限,不能被永久钉死)。
pub fn run_claude_readonly_timeout<F: FnMut(&str, &str)>(
    root: &Path,
    prompt: &str,
    on_event: F,
    timeout: std::time::Duration,
) -> Result<String, String> {
    run_claude_readonly_inner(root, prompt, on_event, Some(timeout))
}

fn run_claude_readonly_inner<F: FnMut(&str, &str)>(
    root: &Path,
    prompt: &str,
    mut on_event: F,
    timeout: Option<std::time::Duration>,
) -> Result<String, String> {
    let claude_bin: std::ffi::OsString = crate::doctor::resolve_claude_exe()
        .map(|p| p.into_os_string())
        .unwrap_or_else(|| "claude".into());
    let mut cmd = Command::new(&claude_bin);
    cmd.args([
        "--print",
        "--output-format",
        "stream-json",
        "--verbose",
        "--permission-mode=bypassPermissions",
        "--allowedTools",
        "Read,Glob,Grep", // 只读: 物理上不给 Write/Edit, 决策数据落地由 Rust 执行
    ])
    .current_dir(root)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    crate::doctor::harden_child_env(&mut cmd); // loopback NO_PROXY + 清干扰变量
    crate::provider::scope_child_claude(&mut cmd); // 隔离模式第三方 → 私有会话账本
    no_window(&mut cmd);

    let mut child = cmd.spawn().map_err(|e| format!("调起 claude 失败: {e}"))?;
    if let Some(mut si) = child.stdin.take() {
        use std::io::Write as _;
        let _ = si.write_all(prompt.as_bytes());
    }
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

    let stdout = child.stdout.take();

    // 墙钟看门狗:设了 timeout 时,到点 kill 子进程 —— stdout 随之关闭,下面的读循环自然结束。
    // 命令正常读完会 drop done_tx 让看门狗提前醒来(不空等满 timeout);None=不设超时(旧行为)。
    let child = std::sync::Arc::new(parking_lot::Mutex::new(child));
    let timed_out = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let watchdog = timeout.map(|dur| {
        let child_w = std::sync::Arc::clone(&child);
        let flag = std::sync::Arc::clone(&timed_out);
        std::thread::spawn(move || {
            if matches!(
                done_rx.recv_timeout(dur),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ) {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                let _ = child_w.lock().kill();
            }
        })
    });

    let mut collected = String::new();
    let mut result_err: Option<String> = None;
    if let Some(so) = stdout {
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
            let ty = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
            if ty == "result" {
                if let Some(st) = v.get("subtype").and_then(|x| x.as_str()) {
                    if st.starts_with("error") {
                        result_err = Some(format!("claude 返回错误: {st}"));
                        break;
                    }
                }
                continue;
            }
            if ty != "assistant" {
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
                        on_event("tool", name);
                    }
                    Some("text") => {
                        if let Some(t) = block.get("text").and_then(|x| x.as_str()) {
                            collected.push_str(t);
                            on_event("delta", t.trim());
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // 读到 EOF(或 error 提前 break):通知看门狗退出,再回收子进程(避免僵尸 + 线程泄漏)。
    drop(done_tx);
    if let Some(h) = watchdog {
        let _ = h.join();
    }
    let status = child.lock().wait();

    if timed_out.load(std::sync::atomic::Ordering::SeqCst) {
        let secs = timeout.map(|d| d.as_secs()).unwrap_or_default();
        return Err(format!("claude 超时({secs}s)已终止"));
    }
    if let Some(e) = result_err {
        return Err(e);
    }
    if !matches!(&status, Ok(s) if s.success()) {
        let se = stderr_buf.lock().clone();
        return Err(format!(
            "claude 异常退出{}",
            if se.is_empty() {
                String::new()
            } else {
                format!(": {se}")
            }
        ));
    }
    Ok(collected)
}

/// 从一段文本里抽出第一个**平衡**的 JSON (对象 `{...}` 或数组 `[...]`), 容忍前后包裹的
/// markdown 代码围栏与说明文字 (借鉴 llm_wiki 对 LLM 输出格式宽松解析)。
pub fn extract_balanced_json(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let start = s.find(['{', '['])?;
    let open = bytes[start];
    let close = if open == b'{' { b'}' } else { b']' };
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            x if x == open => depth += 1,
            x if x == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(s[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_tolerates_fences_and_prose() {
        let s = "好的, 结果如下:\n```json\n[{\"a\":1},{\"b\":\"]x\"}]\n```\n完毕";
        let j = extract_balanced_json(s).unwrap();
        assert_eq!(j, "[{\"a\":1},{\"b\":\"]x\"}]");
        let obj = extract_balanced_json("noise {\"k\": \"v}v\"} tail").unwrap();
        assert_eq!(obj, "{\"k\": \"v}v\"}");
    }
}
