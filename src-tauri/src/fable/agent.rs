//! agent 编排层 —— 以 claude code agent 为根基编排检索。
//!
//! 用户拍板的架构:**所有检索方式都是 agent 的工具**,编排权交给模型 ——
//! - 内置工具:Read / Glob / Grep(定向、路径已知时最准);
//! - 检索枢纽 CLI:`polaris-forge fable search`(grep 多核 ∥ RAG 向量并行混检,
//!   覆盖「不知道在哪个文件」的全盘模糊/语义检索);
//! - 模型可在一条 Bash 里多查询并行(CPU 多,放心发)。
//!
//! 本文件只产出「注入块」与 CLI 路径探测;真正的编排者是 headless claude 本身。

use std::path::PathBuf;

/// 探测 agent 可调用的 polaris-forge CLI。
/// 顺序:~/Polaris/bin(桌面播种位)→ PATH → /usr/local/bin(Docker 镜像内置位)。
pub fn resolve_cli() -> Option<String> {
    let exe = if cfg!(windows) { "polaris-forge.exe" } else { "polaris-forge" };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(u) = directories::UserDirs::new() {
        candidates.push(u.home_dir().join("Polaris").join("bin").join(exe));
    }
    candidates.push(PathBuf::from("/usr/local/bin/polaris-forge"));
    for c in &candidates {
        if c.is_file() {
            return Some(c.to_string_lossy().into_owned());
        }
    }
    // PATH 上有也算(Docker/开发机)
    let path = std::env::var("PATH").unwrap_or_default();
    let sep = if cfg!(windows) { ';' } else { ':' };
    for dir in path.split(sep) {
        if dir.is_empty() {
            continue;
        }
        let p = PathBuf::from(dir).join(exe);
        if p.is_file() {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    None
}

/// chat 注入块:检索枢纽就绪(盘点过)才注入;否则返回空串零开销。
/// `full=false`(知识库开关关着)只给一行提示;`full=true` 给完整编排指令。
pub fn fable_context_block(full: bool) -> String {
    let Ok(st) = super::status() else { return String::new() };
    if st.files_total == 0 {
        return String::new();
    }
    let cli = resolve_cli();
    let vector_ready = st.chunks_total > 0 && st.embed_provider.is_some();

    if !full {
        // 极简提示(<60 token):告诉模型枢纽存在,需要时自取
        return match &cli {
            Some(c) => format!(
                "检索枢纽: `\"{c}\" fable search --q=\"<查询>\" --json`(全盘混检,已盘点 {} 文件)\n\n",
                st.files_total
            ),
            None => String::new(),
        };
    }

    let mut s = String::new();
    s.push_str("### [检索枢纽 · 寓言计划]\n\n");
    s.push_str(&format!(
        "全盘清单已就绪:{} 个文件(文本 {} 个,向量化 chunk {} 条)。**你是检索的编排者**,所有检索方式都是你的工具,按需组合、并行使用:\n\n",
        st.files_total, st.text_files, st.chunks_total
    ));
    s.push_str("1. **定向查找**(知道大概路径/文件名/精确关键词):用内置 Glob / Grep / Read 工具,最快最准;\n");
    if let Some(c) = &cli {
        s.push_str(&format!(
            "2. **全盘混检**(不知道在哪、语义模糊、跨文件):运行 `\"{c}\" fable search --q=\"<查询>\" --top=12`,内部 grep 多核车道与 RAG 向量车道并行 + 重排,返回 JSON 命中(path/abspath/location/snippet);\n"
        ));
        s.push_str(&format!(
            "   - 多角度检索可一次并行发多条查询(机器核多):换 2-3 种说法各查一次再汇总;\n   - `--mode=grep` 纯字面 / `--mode=vector` 纯语义{};\n   - `--scope=wiki` 只搜妈妈库(人工确认的权威知识)/ `--scope=!wiki` 只搜外面的原始资料库;\n",
            if vector_ready { "" } else { "(向量索引还没建,当前实际只有 grep 车道)" }
        ));
    } else {
        s.push_str("2. (检索枢纽 CLI 未安装,全盘检索退化为内置 Grep 工具逐目录扫)\n");
    }
    s.push_str("3. **取证铁律**:混检命中只是线索,务必用 Read 打开 abspath 核对原文再引用;引用报相对路径。\n\n");
    s
}
