//! collab/workset.rs —— 项目同步器 + 任务开工器(v8 方案 2.2 与 6.3⑤「领卡即就位」)。
//!
//! 完整端的"就位"三件套:
//! 1. 部分克隆(blob:none + sparse-checkout cone),大仓库也秒级就位;
//! 2. 领卡开工:自动开分支、按 scope 扩稀疏集、落后检测;
//! 3. 先拉后推 + 对话回传断线缓存(本地 outbox 小库,幂等键防重复补传)。
//! 全部经 git CLI(风格同 mergectl.rs),不引第三方 git 库。
use std::path::Path;
use std::process::Command;

/// 任务开工报告(task_setup 的返回值,给上层展示"就位情况")。
#[derive(serde::Serialize, Clone, Debug)]
pub struct SetupReport {
    /// 已切到的任务分支名。
    pub branch: String,
    /// 落后 origin/main 的提交数(离线时为 0,不可信)。
    pub behind_main: u64,
    /// true=fetch 失败,本次按离线模式继续(本地引用可能过期)。
    pub offline: bool,
    /// 本次按 scope 加入稀疏集的目录列表。
    pub sparse_dirs: Vec<String>,
}

/// 在 repo 目录下跑一条 git 命令,只接受 0 退出码(错误带 stderr 便于回溯)。
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

/// 宽松版:返回 (stdout, 是否成功),起不来进程才算 Err。
fn git(repo: &Path, args: &[&str]) -> Result<(String, bool), String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("git 启动失败: {e}"))?;
    Ok((
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    ))
}

/// 判断 dest 是否已是 git 仓库(有 .git 目录/文件即算)。
fn is_git_repo(dest: &Path) -> bool {
    dest.join(".git").exists()
}

/// 项目同步器:部分克隆(blob:none)+ 锥形稀疏检出,只落指定目录。
/// dest 已存在且是 git 仓库 → 跳过 clone,只更新稀疏集(幂等,可反复调)。
pub fn clone_partial(remote_url: &str, dest: &Path, sparse_dirs: &[&str]) -> Result<(), String> {
    if !is_git_repo(dest) {
        // --no-checkout:先不落文件,等稀疏集设好再 checkout,避免全量落盘一瞬间。
        let out = Command::new("git")
            .args(["clone", "--filter=blob:none", "--no-checkout", remote_url])
            .arg(dest)
            .output()
            .map_err(|e| format!("git 启动失败: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "部分克隆失败: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        git_ok(dest, &["sparse-checkout", "init", "--cone"])?;
        if !sparse_dirs.is_empty() {
            let mut args = vec!["sparse-checkout", "set"];
            args.extend_from_slice(sparse_dirs);
            git_ok(dest, &args)?;
        }
        git_ok(dest, &["checkout", "main"])?;
    } else {
        // 已就位:只把稀疏集对齐到目标目录(cone 模式,set 是全量替换)。
        git_ok(dest, &["sparse-checkout", "init", "--cone"])?;
        if !sparse_dirs.is_empty() {
            let mut args = vec!["sparse-checkout", "set"];
            args.extend_from_slice(sparse_dirs);
            git_ok(dest, &args)?;
        }
    }
    Ok(())
}

/// 任务开工器「领卡即就位」:fetch → 开/切分支 → 按 scope 扩稀疏集 → 落后检测。
/// scope_csv:逗号分隔的目录前缀(任务卡圈定的地盘)。
pub fn task_setup(repo: &Path, branch: &str, scope_csv: &str) -> Result<SetupReport, String> {
    // ① fetch 失败不挡路——记 offline,继续用本地引用干活(v8:离线可开工)。
    let offline = !git(repo, &["fetch", "origin"])?.1;

    // ② 本地有该分支就切过去,没有就从 origin/main(离线退化到本地 main)新开。
    let has_branch = git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )?
    .1;
    if has_branch {
        git_ok(repo, &["switch", branch])?;
    } else {
        let base = if git(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/remotes/origin/main",
            ],
        )?
        .1
        {
            "origin/main"
        } else {
            "main"
        };
        git_ok(repo, &["switch", "-c", branch, base])?;
    }

    // ③ scope 目录并入稀疏集(add 是增量,不动别的卡已就位的目录)。
    let dirs: Vec<String> = scope_csv
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if !dirs.is_empty() {
        let mut args: Vec<&str> = vec!["sparse-checkout", "add"];
        args.extend(dirs.iter().map(String::as_str));
        git_ok(repo, &args)?;
    }

    // ④ 落后检测:HEAD 与 origin/main 差了几个提交(离线/无远端引用时算 0)。
    let behind_main = git(repo, &["rev-list", "--count", "HEAD..origin/main"])?
        .0
        .trim()
        .parse::<u64>()
        .unwrap_or(0);

    Ok(SetupReport {
        branch: branch.to_string(),
        behind_main,
        offline,
        sparse_dirs: dirs,
    })
}

/// 网络类 git 操作重试:3 次指数退避(1s/3s/9s)。只重试传入闭包返回的 Err;
/// 调用方保证闭包幂等(fetch/push 天然幂等,merge 不进这里)。
fn retry_net<T>(mut f: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    let mut last = String::new();
    for (i, wait) in [0u64, 1, 3, 9].iter().enumerate() {
        if *wait > 0 {
            std::thread::sleep(std::time::Duration::from_secs(*wait));
        }
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                // REJECT: 前缀 = 语义性拒绝(保护分支/需先同步),重试无意义,立刻返回。
                if e.starts_with("REJECT:") {
                    return Err(e);
                }
                if i < 3 {
                    eprintln!("[workset] 网络操作失败({e}),重试 {}/3", i + 1);
                }
                last = e;
            }
        }
    }
    Err(last)
}

/// 先拉后推之「拉」:fetch + merge origin/main。fetch 带 3 次退避重试(网络抖动兜底)。
/// 冲突时列出冲突文件、立即 abort(不把半截冲突留在工作区),返回 Err 给上层去走冲突流程。
pub fn sync_main(repo: &Path) -> Result<String, String> {
    retry_net(|| git_ok(repo, &["fetch", "origin"]))
        .map_err(|e| format!("拉取失败(离线?): {e}"))?;
    let (out, ok) = git(repo, &["merge", "origin/main", "--no-edit"])?;
    if ok {
        return Ok(out.trim().to_string());
    }
    let files = git(repo, &["diff", "--name-only", "--diff-filter=U"])?.0;
    let _ = git(repo, &["merge", "--abort"]);
    Err(format!(
        "与 origin/main 合并有冲突,已回退。冲突文件:{}",
        files.split_whitespace().collect::<Vec<_>>().join(", ")
    ))
}

/// 先拉后推之「推」:push -u origin <branch>。网络类失败带 3 次退避重试;
/// 被拒时给明确中文提示(常见两种:落后需先 sync,保护分支不许直推)——被拒不重试。
pub fn push_branch(repo: &Path, branch: &str) -> Result<(), String> {
    retry_net(|| {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["push", "-u", "origin", branch])
            .output()
            .map_err(|e| format!("git 启动失败: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("non-fast-forward") || err.contains("fetch first") || err.contains("behind")
        {
            return Err(format!(
                "REJECT:推送被拒:远端分支已前进,请先「先拉后推」同步再推。({})",
                err.trim()
            ));
        }
        if err.contains("protected")
            || err.contains("pre-receive hook declined")
            || err.contains("GH006")
        {
            return Err(format!(
                "REJECT:推送被拒:{branch} 是保护分支,不允许直推,请走合并闸门。({})",
                err.trim()
            ));
        }
        Err(format!("推送失败: {}", err.trim()))
    })
    .map_err(|e| e.trim_start_matches("REJECT:").to_string())
}

/// scope 越界检测:任务分支相对 origin/main(无远端引用退化 main)动过的文件里,
/// 不落在任何 scope 前缀下的清单。scope 为空 = 不设限,返回空。软约束的「照妖镜」:
/// 推送前给协作者明确提示;主机侧检查闸另有同名硬闸。
pub fn out_of_scope_files(repo: &Path, scope_csv: &str) -> Result<Vec<String>, String> {
    let prefixes: Vec<String> = scope_csv
        .split(',')
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if prefixes.is_empty() {
        return Ok(Vec::new());
    }
    let base = if git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "refs/remotes/origin/main",
        ],
    )?
    .1
    {
        "origin/main...HEAD"
    } else {
        "main...HEAD"
    };
    let out = git_ok(repo, &["diff", "--name-only", base])?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .filter(|f| {
            !prefixes
                .iter()
                .any(|p| *f == p.as_str() || f.starts_with(&format!("{p}/")))
        })
        .map(String::from)
        .collect())
}

/// scope 就位状态:本地稀疏集实际清单 vs 任务 scope,给前端展示「已就位/缺失」。
#[derive(serde::Serialize, Clone, Debug)]
pub struct ScopeStatus {
    /// 本地 sparse-checkout 当前包含的目录。
    pub sparse: Vec<String>,
    /// scope 里声明但本地稀疏集缺失的目录(需要再跑一次任务开工器)。
    pub missing: Vec<String>,
}

pub fn scope_status(repo: &Path, scope_csv: &str) -> Result<ScopeStatus, String> {
    let sparse: Vec<String> = git_ok(repo, &["sparse-checkout", "list"])?
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let missing = scope_csv
        .split(',')
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .filter(|d| !sparse.iter().any(|s| s == d))
        .collect();
    Ok(ScopeStatus { sparse, missing })
}

// ---------- 对话回传断线缓存(v8 2.2:本地缓存、重连按序补传、幂等键防重复) ----------

/// 打开(必要时建表)outbox 小库。独立于 collab.db,坏了也不连累主库。
fn outbox_conn(dir: &Path) -> Result<rusqlite::Connection, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败: {e}"))?;
    let conn = rusqlite::Connection::open(dir.join("outbox.db"))
        .map_err(|e| format!("打开 outbox.db 失败: {e}"))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS outbox(
            idem_key   TEXT PRIMARY KEY,
            payload    TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            sent_at    INTEGER NULL
        );",
    )
    .map_err(|e| format!("建表失败: {e}"))?;
    Ok(conn)
}

/// 随机 16 字节 hex 幂等键(时间纳秒+进程号+计数器混合哈希,无需引随机库)。
fn gen_idem_key() -> String {
    use std::hash::{Hash, Hasher};
    static CNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut out = String::with_capacity(32);
    for salt in 0u64..2 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .hash(&mut h);
        std::process::id().hash(&mut h);
        CNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .hash(&mut h);
        salt.hash(&mut h);
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

/// 入队一条待回传消息,返回幂等键(调用方发送时带上,服务端按键去重)。
pub fn queue_message(dir: &Path, payload_json: &str) -> Result<String, String> {
    let conn = outbox_conn(dir)?;
    let key = gen_idem_key();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    conn.execute(
        "INSERT INTO outbox(idem_key, payload, created_at, sent_at) VALUES(?1, ?2, ?3, NULL)",
        rusqlite::params![key, payload_json, now],
    )
    .map_err(|e| format!("入队失败: {e}"))?;
    Ok(key)
}

/// 列出所有未发送的消息(按入队顺序,重连时按序补传)。返回 (idem_key, payload)。
pub fn pending_messages(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let conn = outbox_conn(dir)?;
    let mut stmt = conn
        .prepare(
            "SELECT idem_key, payload FROM outbox WHERE sent_at IS NULL ORDER BY created_at, rowid",
        )
        .map_err(|e| format!("查询失败: {e}"))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| format!("查询失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取失败: {e}"))?;
    Ok(rows)
}

/// 标记某条消息已发送(补传成功后调,幂等:重复标记无害)。
pub fn mark_sent(dir: &Path, idem_key: &str) -> Result<(), String> {
    let conn = outbox_conn(dir)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    conn.execute(
        "UPDATE outbox SET sent_at = ?1 WHERE idem_key = ?2 AND sent_at IS NULL",
        rusqlite::params![now, idem_key],
    )
    .map_err(|e| format!("标记失败: {e}"))?;
    Ok(())
}

/// outbox 补传报告。
#[derive(serde::Serialize, Clone, Debug, Default)]
pub struct FlushReport {
    /// 本轮成功送达(含服务端幂等去重命中)的条数。
    pub sent: u64,
    /// 服务端明确拒绝(4xx,如任务不存在/无权限)的条数——同样出队,不再无限重试。
    pub rejected: u64,
    /// 仍留队等下次补传(网络/5xx)的条数。
    pub remaining: u64,
}

/// 把 outbox 里积压的消息按序补传到主机(v8 2.2 的「重连按序补传」落地)。
/// payload 约定为 `{"taskId":N,"body":"..."}`;POST {base}/api/collab/tasks/{taskId}/messages,
/// 带幂等键,服务端按键去重(重复补传无害)。网络失败即停(保持按序),留队下次再来。
pub fn flush_outbox(dir: &Path, base_url: &str, token: &str) -> Result<FlushReport, String> {
    let base = base_url.trim_end_matches('/');
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(10))
        .build();
    let mut report = FlushReport::default();
    let pending = pending_messages(dir)?;
    let total = pending.len() as u64;
    for (key, payload) in pending {
        let v: serde_json::Value = match serde_json::from_str(&payload) {
            Ok(v) => v,
            Err(_) => {
                // 坏载荷永远发不出去,出队并计拒绝,防堵死后续消息。
                mark_sent(dir, &key)?;
                report.rejected += 1;
                continue;
            }
        };
        let Some(task_id) = v.get("taskId").and_then(|t| t.as_i64()) else {
            mark_sent(dir, &key)?;
            report.rejected += 1;
            continue;
        };
        let body = v.get("body").and_then(|b| b.as_str()).unwrap_or_default();
        let resp = agent
            .post(&format!("{base}/api/collab/tasks/{task_id}/messages"))
            .set("Authorization", &format!("Bearer {token}"))
            .send_json(serde_json::json!({ "body": body, "idemKey": key }));
        match resp {
            Ok(_) => {
                mark_sent(dir, &key)?;
                report.sent += 1;
            }
            // 409 = 幂等键已存在(上次发成功但没来得及标记)→ 视为送达。
            Err(ureq::Error::Status(409, _)) => {
                mark_sent(dir, &key)?;
                report.sent += 1;
            }
            // 其他 4xx = 服务端明确拒绝(无权限/任务没了),重试无意义,出队。
            Err(ureq::Error::Status(code, r)) if (400..500).contains(&code) => {
                eprintln!(
                    "[outbox] 消息被拒(HTTP {code}): {}",
                    r.into_string().unwrap_or_default()
                );
                mark_sent(dir, &key)?;
                report.rejected += 1;
            }
            // 网络/5xx:停止本轮(保持按序),剩余留队。
            Err(e) => {
                eprintln!("[outbox] 补传中断,留队待重试: {e}");
                break;
            }
        }
    }
    report.remaining = total - report.sent - report.rejected;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "workset-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn run(repo: &Path, args: &[&str]) {
        let st = Command::new("git")
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

    fn write_commit(repo: &Path, file: &str, content: &str, msg: &str) {
        let p = repo.join(file);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
        run(repo, &["add", "."]);
        run(repo, &["commit", "-m", msg]);
    }

    /// 造:裸远端(允许 partial clone)+ 种子仓库推入 main。返回 (bare_url, seed_repo)。
    fn mk_remote() -> (String, PathBuf) {
        let bare = tmp("bare");
        run(&bare, &["init", "--bare", "-b", "main"]);
        // file:// 协议 partial clone 需要服务端允许 filter。
        run(&bare, &["config", "uploadpack.allowFilter", "true"]);

        let seed = tmp("seed");
        run(&seed, &["init", "-b", "main"]);
        run(&seed, &["config", "user.name", "t"]);
        run(&seed, &["config", "user.email", "t@t"]);
        write_commit(&seed, "docs/readme.md", "docs\n", "c1");
        write_commit(&seed, "src/main.rs", "fn main(){}\n", "c2");
        write_commit(&seed, "assets/logo.txt", "logo\n", "c3");
        run(&seed, &["remote", "add", "origin", bare.to_str().unwrap()]);
        run(&seed, &["push", "-u", "origin", "main"]);
        (format!("file://{}", bare.to_str().unwrap()), seed)
    }

    #[test]
    fn partial_clone_setup_sync_push() {
        let (url, seed) = mk_remote();

        // 1) 部分克隆:只要 docs/,稀疏检出不该落 src/、assets/。
        let work = tmp("work");
        let dest = work.join("proj");
        clone_partial(&url, &dest, &["docs"]).unwrap();
        assert!(dest.join("docs/readme.md").exists());
        assert!(!dest.join("src").exists());
        assert!(!dest.join("assets").exists());
        run(&dest, &["config", "user.name", "t"]);
        run(&dest, &["config", "user.email", "t@t"]);

        // 已存在仓库再调:跳过 clone,只更新稀疏集(加上 assets)。
        clone_partial(&url, &dest, &["docs", "assets"]).unwrap();
        assert!(dest.join("assets/logo.txt").exists());
        assert!(!dest.join("src").exists());

        // 2) 领卡开工:新分支 + scope 把 src 并入稀疏集(刚 fetch 完,不落后)。
        let rep = task_setup(&dest, "task/t-001", "src").unwrap();
        assert_eq!(rep.branch, "task/t-001");
        assert!(!rep.offline);
        assert_eq!(rep.behind_main, 0);
        assert_eq!(rep.sparse_dirs, vec!["src".to_string()]);
        assert!(dest.join("src/main.rs").exists());

        // 远端 main 前进一格(种子仓库再推一个提交)→ 再开工时应检出落后 1。
        write_commit(&seed, "docs/new.md", "new\n", "c4: 远端前进");
        run(&seed, &["push", "origin", "main"]);
        let rep2 = task_setup(&dest, "task/t-001", "").unwrap();
        assert_eq!(rep2.branch, "task/t-001");
        assert_eq!(rep2.behind_main, 1);
        assert!(rep2.sparse_dirs.is_empty());

        // 3) 先拉后推:干净合并 origin/main,落后归零。
        sync_main(&dest).unwrap();
        assert!(dest.join("docs/new.md").exists());
        let rep3 = task_setup(&dest, "task/t-001", "").unwrap();
        assert_eq!(rep3.behind_main, 0);

        // 4) 推分支上去,远端应出现该引用。
        write_commit(&dest, "src/lib.rs", "// work\n", "任务提交");
        push_branch(&dest, "task/t-001").unwrap();
        let ls = Command::new("git")
            .args(["ls-remote", "--heads"])
            .arg(url.trim_start_matches("file://"))
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&ls.stdout).contains("refs/heads/task/t-001"));
    }

    #[test]
    fn outbox_queue_roundtrip() {
        let dir = tmp("outbox");
        // 入队两条,键各不同。
        let k1 = queue_message(&dir, r#"{"msg":"第一条"}"#).unwrap();
        let k2 = queue_message(&dir, r#"{"msg":"第二条"}"#).unwrap();
        assert_eq!(k1.len(), 32);
        assert_ne!(k1, k2);

        // 未发列表按序两条。
        let pending = pending_messages(&dir).unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].0, k1);
        assert_eq!(pending[1].0, k2);

        // 标记第一条已发 → 只剩第二条;重复标记幂等。
        mark_sent(&dir, &k1).unwrap();
        mark_sent(&dir, &k1).unwrap();
        let pending = pending_messages(&dir).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, k2);

        mark_sent(&dir, &k2).unwrap();
        assert!(pending_messages(&dir).unwrap().is_empty());
    }
}
