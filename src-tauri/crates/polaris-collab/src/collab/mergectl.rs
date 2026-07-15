//! collab/mergectl.rs —— 分支合并管理(合并闸门,v8 方案)。
//!
//! 铁律:合并放行是确定性代码的事——先 merge-tree 无副作用试算,干净才准落盘;
//! 大模型只能看试算结果给意见,永远碰不到真正的 merge 按钮。
//! 全部经 git CLI(≥2.38,依赖 `git merge-tree --write-tree`),不引第三方 git 库。
use std::path::{Path, PathBuf};
use std::process::Command;

use super::db;

/// 一次无副作用的合并试算结果(git merge-tree --write-tree)。
#[derive(serde::Serialize, Clone, Debug)]
pub struct MergeTrial {
    /// true=可干净合并,false=有冲突。
    pub clean: bool,
    /// 试算出的树 OID(冲突时也有,含冲突标记的树)。
    pub tree_oid: String,
    /// 冲突文件列表(clean 时为空)。
    pub conflict_files: Vec<String>,
}

/// 单个冲突块:ours/base/theirs 三方文本 + 在合并结果中的行号区间(1 起,含标记行)。
#[derive(serde::Serialize, Clone, Debug)]
pub struct ConflictBlock {
    pub ours: String,
    pub base: String,
    pub theirs: String,
    /// `<<<<<<<` 所在行号(1 起)。
    pub start_line: usize,
    /// `>>>>>>>` 所在行号(1 起)。
    pub end_line: usize,
}

/// 单个冲突块的处置决定(裁决台三处置之「采纳某侧 / 融合草案」;整单打回走任务状态机)。
#[derive(serde::Deserialize, Clone, Debug)]
pub struct BlockResolution {
    /// "ours"(采纳 base/main 侧) | "theirs"(采纳分支侧) | "manual"(人工或 AI 融合草案,须带 text)。
    pub choice: String,
    #[serde(default)]
    pub text: Option<String>,
}

/// 在 repo 目录下跑一条 git 命令,返回 (stdout, 退出码)。
/// 起不来进程才算 Err;非零退出码由调用方按语义处理(merge-tree 的 1 是"有冲突"不是错)。
fn git(repo: &Path, args: &[&str]) -> Result<(String, i32), String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("git 启动失败: {e}"))?;
    Ok((
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    ))
}

/// 只接受 0 退出码的便捷封装(错误里带 stderr,便于回溯)。
fn git_ok(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("git 启动失败: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} 失败(code={}): {}",
            args.join(" "),
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 校验引用存在(rev-parse --verify),防手滑传错分支名。
fn ensure_ref(repo: &Path, r: &str) -> Result<(), String> {
    git_ok(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{r}^{{commit}}"),
        ],
    )
    .map(|_| ())
    .map_err(|_| format!("引用不存在: {r}"))
}

/// 合并试算(无副作用):`git merge-tree --write-tree --name-only base branch`。
/// 退出码 0=干净,1=冲突;输出第一行是树 OID,冲突文件在后续行(至空行止)。
pub fn merge_trial(repo: &Path, base: &str, branch: &str) -> Result<MergeTrial, String> {
    ensure_ref(repo, base)?;
    ensure_ref(repo, branch)?;
    let (out, code) = git(
        repo,
        &["merge-tree", "--write-tree", "--name-only", base, branch],
    )?;
    if code != 0 && code != 1 {
        return Err(format!("merge-tree 失败(code={code})"));
    }
    let mut lines = out.lines();
    let tree_oid = lines.next().unwrap_or("").trim().to_string();
    if tree_oid.is_empty() {
        return Err("merge-tree 未输出树 OID(git 版本需 ≥2.38)".into());
    }
    let mut conflict_files = Vec::new();
    if code == 1 {
        for l in lines {
            let l = l.trim();
            if l.is_empty() {
                break; // 空行之后是 informational messages,不要
            }
            conflict_files.push(l.to_string());
        }
    }
    Ok(MergeTrial {
        clean: code == 0,
        tree_oid,
        conflict_files,
    })
}

/// 落后/领先计数:`git rev-list --left-right --count base...branch` → (behind, ahead)。
/// behind=base 独有提交数(branch 落后多少),ahead=branch 独有提交数。
pub fn behind_count(repo: &Path, base: &str, branch: &str) -> Result<(u64, u64), String> {
    ensure_ref(repo, base)?;
    ensure_ref(repo, branch)?;
    let out = git_ok(
        repo,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{base}...{branch}"),
        ],
    )?;
    let mut it = out.split_whitespace();
    let behind = it
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or("rev-list 输出解析失败")?;
    let ahead = it
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or("rev-list 输出解析失败")?;
    Ok((behind, ahead))
}

/// scope 冲突预警:base 上自 merge-base 以来改动的文件,与任务卡 scope 前缀(逗号分隔)取交集。
/// 命中说明别人已经动了这张卡圈定的地盘,合并前该先 rebase/沟通。
pub fn scope_overlap(
    repo: &Path,
    base: &str,
    branch: &str,
    scope: &str,
) -> Result<Vec<String>, String> {
    ensure_ref(repo, base)?;
    ensure_ref(repo, branch)?;
    let mb = git_ok(repo, &["merge-base", base, branch])?
        .trim()
        .to_string();
    let out = git_ok(repo, &["diff", "--name-only", &mb, base])?;
    let prefixes: Vec<&str> = scope
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let hits = out
        .lines()
        .map(|l| l.trim())
        .filter(|f| !f.is_empty() && prefixes.iter().any(|p| f.starts_with(p)))
        .map(String::from)
        .collect();
    Ok(hits)
}

/// 拆单个冲突文件的冲突块:取三方 blob(merge-base:file / base:file / branch:file),
/// 用 `git merge-file -p --diff3` 产出带标记的合并体,再解析
/// `<<<<<<<` / `|||||||` / `=======` / `>>>>>>>` 四段标记。
/// 走 merge-file 而不抠 merge-tree 输出,是因为它对每个文件独立、行号可直接数,最稳妥。
pub fn conflict_blocks(
    repo: &Path,
    base: &str,
    branch: &str,
    file: &str,
) -> Result<Vec<ConflictBlock>, String> {
    parse_blocks(&merged_with_markers(repo, base, branch, file)?)
}

/// 单文件三方合并体(带 diff3 冲突标记)——conflict_blocks 与裁决落地共用同一份权威文本。
fn merged_with_markers(
    repo: &Path,
    base: &str,
    branch: &str,
    file: &str,
) -> Result<String, String> {
    ensure_ref(repo, base)?;
    ensure_ref(repo, branch)?;
    let mb = git_ok(repo, &["merge-base", base, branch])?
        .trim()
        .to_string();

    // 三方内容落临时文件(文件在某侧可能不存在 → 按空文件处理,对应增/删冲突)。
    let show = |rev: &str| -> String {
        git(repo, &["show", &format!("{rev}:{file}")])
            .ok()
            .filter(|(_, c)| *c == 0)
            .map(|(s, _)| s)
            .unwrap_or_default()
    };
    let dir = std::env::temp_dir();
    let tag = format!("{}-{}", std::process::id(), db::now());
    let mk = |name: &str, content: &str| -> Result<PathBuf, String> {
        let p = dir.join(format!("mergectl-{tag}-{name}"));
        std::fs::write(&p, content).map_err(|e| format!("写临时文件失败: {e}"))?;
        Ok(p)
    };
    let f_ours = mk("ours", &show(base))?;
    let f_base = mk("base", &show(&mb))?;
    let f_theirs = mk("theirs", &show(branch))?;

    // merge-file 退出码 = 冲突数(>0),负数才是错。
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "merge-file",
            "-p",
            "--diff3",
            "-L",
            "ours",
            "-L",
            "base",
            "-L",
            "theirs",
        ])
        .arg(&f_ours)
        .arg(&f_base)
        .arg(&f_theirs)
        .output()
        .map_err(|e| format!("git merge-file 启动失败: {e}"))?;
    for p in [&f_ours, &f_base, &f_theirs] {
        let _ = std::fs::remove_file(p);
    }
    if out.status.code().map(|c| c < 0).unwrap_or(true) {
        return Err(format!(
            "merge-file 失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 解析 diff3 标记。段落顺序:ours → ||||||| base → ======= → theirs。
fn parse_blocks(merged: &str) -> Result<Vec<ConflictBlock>, String> {
    let mut blocks = Vec::new();
    let (mut ours, mut basetxt, mut theirs) = (String::new(), String::new(), String::new());
    let mut state = 0u8; // 0=块外 1=ours 2=base 3=theirs
    let mut start = 0usize;
    for (i, line) in merged.lines().enumerate() {
        let n = i + 1;
        if line.starts_with("<<<<<<<") {
            state = 1;
            start = n;
            ours.clear();
            basetxt.clear();
            theirs.clear();
        } else if line.starts_with("|||||||") && state == 1 {
            state = 2;
        } else if line.starts_with("=======") && (state == 1 || state == 2) {
            state = 3;
        } else if line.starts_with(">>>>>>>") && state == 3 {
            blocks.push(ConflictBlock {
                ours: ours.clone(),
                base: basetxt.clone(),
                theirs: theirs.clone(),
                start_line: start,
                end_line: n,
            });
            state = 0;
        } else {
            match state {
                1 => {
                    ours.push_str(line);
                    ours.push('\n');
                }
                2 => {
                    basetxt.push_str(line);
                    basetxt.push('\n');
                }
                3 => {
                    theirs.push_str(line);
                    theirs.push('\n');
                }
                _ => {}
            }
        }
    }
    Ok(blocks)
}

/// 冲突裁决落地:把逐块处置结果落成**任务分支上的一笔合并提交**(父=分支+base),
/// 之后 merge_trial 即干净,再走正常放行闸。融合草案永远先落 PR 分支、永不直写 base。
/// 全程 git plumbing(hash-object/read-tree/commit-tree/update-ref),不碰工作区;
/// update-ref 带旧值 CAS,分支被并发推进则整单失败重来。
/// 硬性要求:一次性覆盖试算出的**全部**冲突文件与冲突块——不许留半截冲突进历史。
pub fn resolve_conflicts(
    repo: &Path,
    base: &str,
    branch: &str,
    resolutions: &std::collections::HashMap<String, Vec<BlockResolution>>,
    actor: &str,
) -> Result<String, String> {
    let trial = merge_trial(repo, base, branch)?;
    if trial.clean {
        return Err("当前试算已无冲突,直接走合并放行即可".into());
    }
    // 分支正被检出时 update-ref 会让工作区与 HEAD 脱节——主机权威仓库应停在 base 上。
    if let Ok(head) = git_ok(repo, &["symbolic-ref", "-q", "HEAD"]) {
        if head.trim() == format!("refs/heads/{branch}") {
            return Err(format!("分支 {branch} 正被检出,请先切回 {base} 再裁决"));
        }
    }
    let branch_oid = git_ok(repo, &["rev-parse", branch])?.trim().to_string();
    let base_oid = git_ok(repo, &["rev-parse", base])?.trim().to_string();

    // 临时索引:从试算树(干净文件已合好)出发,逐个替换冲突文件为裁决后内容。
    let index_path =
        std::env::temp_dir().join(format!("mergectl-idx-{}-{}", std::process::id(), db::now()));
    let git_idx = |args: &[&str], stdin: Option<&str>| -> Result<String, String> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(repo)
            .env("GIT_INDEX_FILE", &index_path)
            .args(args);
        if stdin.is_some() {
            cmd.stdin(std::process::Stdio::piped());
        }
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("git 启动失败: {e}"))?;
        if let Some(content) = stdin {
            use std::io::Write;
            child
                .stdin
                .take()
                .ok_or("无法写入 git stdin")?
                .write_all(content.as_bytes())
                .map_err(|e| format!("写入 git stdin 失败: {e}"))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("git 执行失败: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git {} 失败(code={}): {}",
                args.join(" "),
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let run = || -> Result<String, String> {
        git_idx(&["read-tree", &trial.tree_oid], None)?;
        let mut total_blocks = 0usize;
        for file in &trial.conflict_files {
            let res = resolutions
                .get(file)
                .ok_or_else(|| format!("文件 {file} 缺少裁决——全部冲突块处置完才能落地"))?;
            let blocks = conflict_blocks(repo, base, branch, file)?;
            if blocks.len() != res.len() {
                return Err(format!(
                    "文件 {file} 冲突态势已变化(现 {} 块,收到 {} 块裁决),请刷新重裁",
                    blocks.len(),
                    res.len()
                ));
            }
            let resolved = apply_resolutions(&merged_with_markers(repo, base, branch, file)?, res)?;
            total_blocks += res.len();
            let blob = git_idx(&["hash-object", "-w", "--stdin"], Some(&resolved))?
                .trim()
                .to_string();
            // 保留原文件模式(可执行位);试算树里没有(增删冲突)则按普通文件。
            let mode = git_ok(repo, &["ls-tree", &trial.tree_oid, "--", file])
                .ok()
                .and_then(|l| l.split_whitespace().next().map(String::from))
                .unwrap_or_else(|| "100644".into());
            git_idx(
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("{mode},{blob},{file}"),
                ],
                None,
            )?;
        }
        let tree = git_idx(&["write-tree"], None)?.trim().to_string();
        let msg = format!(
            "裁决合并 {base} 进 {branch}({} 文件 {total_blocks} 块,裁决人:{actor})",
            trial.conflict_files.len()
        );
        let commit = git_ok(
            repo,
            &[
                "-c",
                "user.name=polaris-collab",
                "-c",
                "user.email=collab@polaris.local",
                "commit-tree",
                &tree,
                "-p",
                &branch_oid,
                "-p",
                &base_oid,
                "-m",
                &msg,
            ],
        )?
        .trim()
        .to_string();
        // CAS:分支还停在裁决开始时的提交上才允许前进,否则报冲突态势变化。
        git_ok(
            repo,
            &[
                "update-ref",
                &format!("refs/heads/{branch}"),
                &commit,
                &branch_oid,
            ],
        )
        .map_err(|_| format!("分支 {branch} 在裁决期间被推进,请刷新重裁"))?;
        Ok(commit)
    };
    let result = run();
    let _ = std::fs::remove_file(&index_path);
    if let Ok(oid) = &result {
        db::audit(
            actor,
            "merge.adjudicate",
            branch,
            &format!(
                "{} → {}",
                &branch_oid[..8.min(branch_oid.len())],
                &oid[..8.min(oid.len())]
            ),
        );
    }
    result
}

/// 把逐块处置套回带标记的合并体,产出最终文本。块序与 conflict_blocks 一致(按出现顺序)。
fn apply_resolutions(merged: &str, res: &[BlockResolution]) -> Result<String, String> {
    let mut out = String::new();
    let mut idx = 0usize;
    let mut state = 0u8; // 0=块外 1=ours 2=base 3=theirs
    let (mut ours, mut theirs) = (String::new(), String::new());
    for line in merged.lines() {
        if line.starts_with("<<<<<<<") {
            state = 1;
            ours.clear();
            theirs.clear();
        } else if line.starts_with("|||||||") && state == 1 {
            state = 2;
        } else if line.starts_with("=======") && (state == 1 || state == 2) {
            state = 3;
        } else if line.starts_with(">>>>>>>") && state == 3 {
            let r = res.get(idx).ok_or("裁决块数不足")?;
            let chosen = match r.choice.as_str() {
                "ours" => ours.clone(),
                "theirs" => theirs.clone(),
                "manual" => {
                    let t = r
                        .text
                        .clone()
                        .ok_or_else(|| format!("第 {} 块选了融合草案却没有文本", idx + 1))?;
                    if t.is_empty() || t.ends_with('\n') {
                        t
                    } else {
                        format!("{t}\n")
                    }
                }
                other => return Err(format!("未知处置类型: {other}")),
            };
            out.push_str(&chosen);
            idx += 1;
            state = 0;
        } else {
            match state {
                1 => {
                    ours.push_str(line);
                    ours.push('\n');
                }
                2 => {} // base 段只作展示参考,不进任何一侧
                3 => {
                    theirs.push_str(line);
                    theirs.push('\n');
                }
                _ => {
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
    if idx != res.len() {
        return Err(format!(
            "裁决块数不匹配(文件 {idx} 块,收到 {} 块)",
            res.len()
        ));
    }
    Ok(out)
}

/// squash 合并进 base(放行闸最后一步)。
/// 合并前强制重跑 merge_trial——试算不干净一律拒绝,不给"带冲突硬合"留门。
/// 成功返回新提交 OID 并记审计。
pub fn squash_merge(
    repo: &Path,
    base: &str,
    branch: &str,
    title: &str,
    actor: &str,
) -> Result<String, String> {
    let trial = merge_trial(repo, base, branch)?;
    if !trial.clean {
        return Err(format!(
            "拒绝合并:试算存在冲突({} 个文件),先解决冲突再来",
            trial.conflict_files.len()
        ));
    }
    git_ok(repo, &["switch", base]).map_err(|e| format!("切到 {base} 失败: {e}"))?;
    git_ok(repo, &["merge", "--squash", branch])?;
    git_ok(repo, &["commit", "-m", title]).map_err(|e| {
        // 失败回滚暂存区,别把半截 squash 留在工作区。
        let _ = git(repo, &["reset", "--hard", "HEAD"]);
        format!("squash 提交失败: {e}")
    })?;
    let oid = git_ok(repo, &["rev-parse", "HEAD"])?.trim().to_string();
    db::audit(actor, "merge.squash", branch, title);
    Ok(oid)
}

/// 一键回滚某张卡的合并提交:`git revert --no-edit <oid>`,返回回滚提交 OID。
pub fn revert_card(repo: &Path, commit_oid: &str, actor: &str) -> Result<String, String> {
    ensure_ref(repo, commit_oid)?;
    git_ok(repo, &["revert", "--no-edit", commit_oid]).map_err(|e| {
        let _ = git(repo, &["revert", "--abort"]);
        format!("revert 失败: {e}")
    })?;
    let oid = git_ok(repo, &["rev-parse", "HEAD"])?.trim().to_string();
    db::audit(actor, "merge.revert", commit_oid, "");
    Ok(oid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个真实临时 git 仓库。返回仓库路径(测试结束不强求清理,temp 目录自会回收)。
    fn mk_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mergectl-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        run(&dir, &["init", "-b", "main"]);
        // 库本地写死身份,squash_merge/revert_card 内部 commit 也能用。
        run(&dir, &["config", "user.name", "t"]);
        run(&dir, &["config", "user.email", "t@t"]);
        dir
    }

    fn run(repo: &PathBuf, args: &[&str]) {
        let st = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            st.status.success(),
            "git {:?} 失败: {}",
            args,
            String::from_utf8_lossy(&st.stderr)
        );
    }

    fn write_commit(repo: &PathBuf, file: &str, content: &str, msg: &str) {
        std::fs::write(repo.join(file), content).unwrap();
        run(repo, &["add", "."]);
        run(repo, &["commit", "-m", msg]);
    }

    /// 切独立临时 collab.db,防审计写进真实库(串行锁防并行串库)。
    fn tmp_db() -> std::sync::MutexGuard<'static, ()> {
        let g = super::super::db::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let p = std::env::temp_dir().join(format!(
            "collab-mergectl-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("POLARIS_COLLAB_DB", p);
        g
    }

    #[test]
    fn trial_clean_and_counts_and_squash_revert() {
        let _g = tmp_db();
        let repo = mk_repo("clean");
        write_commit(&repo, "a.txt", "base\n", "c1");
        run(&repo, &["switch", "-c", "feat"]);
        write_commit(&repo, "b.txt", "new file\n", "feat: add b");
        run(&repo, &["switch", "main"]);
        write_commit(&repo, "c.txt", "main side\n", "main: add c");

        // 干净可合并
        let t = merge_trial(&repo, "main", "feat").unwrap();
        assert!(t.clean);
        assert!(!t.tree_oid.is_empty());
        assert!(t.conflict_files.is_empty());

        // main 领先 1(feat 落后 1),feat 领先 1
        assert_eq!(behind_count(&repo, "main", "feat").unwrap(), (1, 1));

        // scope 命中:main 自分叉后动了 c.txt
        let hits = scope_overlap(&repo, "main", "feat", "c.txt, src/").unwrap();
        assert_eq!(hits, vec!["c.txt"]);
        assert!(scope_overlap(&repo, "main", "feat", "docs/")
            .unwrap()
            .is_empty());

        // squash 合并
        let oid = squash_merge(&repo, "main", "feat", "合入 feat", "boss").unwrap();
        assert_eq!(oid.len(), 40);
        assert!(repo.join("b.txt").exists());

        // 回滚这张卡
        let r = revert_card(&repo, &oid, "boss").unwrap();
        assert_ne!(r, oid);
        assert!(!repo.join("b.txt").exists());

        // 引用不存在要报错
        assert!(merge_trial(&repo, "main", "no-such-branch").is_err());
    }

    #[test]
    fn trial_conflict_and_blocks() {
        let _g = tmp_db();
        let repo = mk_repo("conflict");
        write_commit(&repo, "a.txt", "line1\nline2\nline3\n", "c1");
        run(&repo, &["switch", "-c", "feat"]);
        write_commit(&repo, "a.txt", "line1\nfeat-change\nline3\n", "feat edit");
        run(&repo, &["switch", "main"]);
        write_commit(&repo, "a.txt", "line1\nmain-change\nline3\n", "main edit");

        let t = merge_trial(&repo, "main", "feat").unwrap();
        assert!(!t.clean);
        assert!(!t.tree_oid.is_empty());
        assert_eq!(t.conflict_files, vec!["a.txt"]);

        // 冲突时 squash 必须被拒
        assert!(squash_merge(&repo, "main", "feat", "硬合", "boss").is_err());

        // 冲突块解析
        let blocks = conflict_blocks(&repo, "main", "feat", "a.txt").unwrap();
        assert_eq!(blocks.len(), 1);
        let b = &blocks[0];
        assert_eq!(b.ours, "main-change\n");
        assert_eq!(b.base, "line2\n");
        assert_eq!(b.theirs, "feat-change\n");
        assert!(b.start_line >= 1 && b.end_line > b.start_line);
    }

    #[test]
    fn resolve_conflicts_lands_on_branch_then_trial_clean() {
        let _g = tmp_db();
        let repo = mk_repo("resolve");
        write_commit(&repo, "a.txt", "line1\nline2\nline3\n", "c1");
        run(&repo, &["switch", "-c", "feat"]);
        write_commit(&repo, "a.txt", "line1\nfeat-change\nline3\n", "feat edit");
        run(&repo, &["switch", "main"]);
        write_commit(&repo, "a.txt", "line1\nmain-change\nline3\n", "main edit");

        // 缺文件裁决 → 拒
        assert!(resolve_conflicts(&repo, "main", "feat", &Default::default(), "boss").is_err());
        // 块数不匹配 → 拒
        let mut wrong = std::collections::HashMap::new();
        wrong.insert("a.txt".to_string(), vec![]);
        assert!(resolve_conflicts(&repo, "main", "feat", &wrong, "boss").is_err());

        // 采纳分支侧(theirs)落地 → 分支上多一笔双亲合并提交,试算变干净
        let mut res = std::collections::HashMap::new();
        res.insert(
            "a.txt".to_string(),
            vec![BlockResolution {
                choice: "theirs".into(),
                text: None,
            }],
        );
        let oid = resolve_conflicts(&repo, "main", "feat", &res, "boss").unwrap();
        assert_eq!(oid.len(), 40);
        let content = git_ok(&repo, &["show", "feat:a.txt"]).unwrap();
        assert_eq!(content, "line1\nfeat-change\nline3\n");
        let t = merge_trial(&repo, "main", "feat").unwrap();
        assert!(t.clean, "裁决后试算应干净");
        // main 本身未被动过
        assert_eq!(
            git_ok(&repo, &["show", "main:a.txt"]).unwrap(),
            "line1\nmain-change\nline3\n"
        );
        // 干净后 squash 放行畅通
        assert!(squash_merge(&repo, "main", "feat", "合入 feat", "boss").is_ok());

        // 融合草案(manual)路径:再造一处冲突,用自定义文本落地
        run(&repo, &["switch", "-c", "feat2"]);
        write_commit(&repo, "a.txt", "line1\nfeat2\nline3\n", "feat2 edit");
        run(&repo, &["switch", "main"]);
        write_commit(&repo, "a.txt", "line1\nmain2\nline3\n", "main edit2");
        let mut res2 = std::collections::HashMap::new();
        res2.insert(
            "a.txt".to_string(),
            vec![BlockResolution {
                choice: "manual".into(),
                text: Some("融合双方".into()),
            }],
        );
        resolve_conflicts(&repo, "main", "feat2", &res2, "boss").unwrap();
        assert_eq!(
            git_ok(&repo, &["show", "feat2:a.txt"]).unwrap(),
            "line1\n融合双方\nline3\n"
        );
        assert!(merge_trial(&repo, "main", "feat2").unwrap().clean);
    }
}
