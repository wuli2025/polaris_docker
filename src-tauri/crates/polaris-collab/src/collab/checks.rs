//! collab/checks.rs —— 任务卡检查工作流(CI-lite,GitHub status checks 式)。
//!
//! 原则:脚本说了算(用项目自己的开源工具链),AI 永不进 pass/fail 判定路径;
//! 工具缺失/超时 = skipped 而非 fail(不误伤);creative 档跳过构建类检查,
//! 只留密钥扫描+大文件闸(视频/游戏素材仓不为难)。
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::db::{now, open_db};
use rusqlite::params;

/// 单项检查超时(秒)。cargo check 冷缓存也该够;超时=skipped(timeout)。
const STEP_TIMEOUT: u64 = 600;
/// 输出只留尾部字节数(错误都在最后)。
const OUTPUT_TAIL: usize = 16 * 1024;

/// 全局串行锁:主机是台桌面机,N 个提交并发跑 N 个 cargo check 会把它轰趴;
/// 排队跑(提交线程本来就各自阻塞在这)。顺带天然互斥了同卡重跑的结果交错。
static RUN_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// 僵尸清理:上一次进程被杀/主机重启时卡在 running 的行,超过 30 分钟判「被中断」,
/// 否则那张卡的检查永远显示转圈、闸门永远不放行。开跑前 + 合并闸/查询前都清一遍(全局),
/// 免得「进程崩了没人再触发新检查」的卡片永久卡在 running。
pub fn sweep_stale_running() {
    if let Ok(conn) = open_db() {
        let _ = conn.execute(
            "UPDATE check_runs SET status='skipped', output='检查被中断(主机重启或进程被杀),请重跑', ended_at=?1
             WHERE status='running' AND started_at < ?2",
            params![now(), now() - 1800],
        );
    }
}

#[derive(serde::Serialize, Clone)]
pub struct CheckRun {
    pub name: String,
    pub status: String, // pass|fail|skipped|running
    pub output: String,
    pub started_at: i64,
    pub ended_at: i64,
}

/// 项目检查档位。
pub fn project_profile(project_id: i64) -> String {
    open_db()
        .ok()
        .and_then(|c| {
            c.query_row(
                "SELECT check_profile FROM projects WHERE id=?1",
                [project_id],
                |r| r.get::<_, String>(0),
            )
            .ok()
        })
        .unwrap_or_else(|| "code".into())
}

pub fn set_project_profile(project_id: i64, profile: &str) -> Result<(), String> {
    if !matches!(profile, "code" | "creative" | "off") {
        return Err("档位只能是 code/creative/off".into());
    }
    let conn = open_db()?;
    conn.execute(
        "UPDATE projects SET check_profile=?1 WHERE id=?2",
        params![profile, project_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 项目的检查技能 id。空 = 默认内置「项目检测」技能(project-check-default)。
pub fn project_check_skill(project_id: i64) -> String {
    let id: String = open_db()
        .ok()
        .and_then(|c| {
            c.query_row(
                "SELECT check_skill FROM projects WHERE id=?1",
                [project_id],
                |r| r.get::<_, String>(0),
            )
            .ok()
        })
        .unwrap_or_default();
    if id.trim().is_empty() {
        crate::skills::PROJECT_CHECK_ID.to_string()
    } else {
        id.trim().to_string()
    }
}

/// 设检查技能:必须是本机已安装且声明了检查协议的技能(或空=回到默认)。
pub fn set_project_check_skill(project_id: i64, skill_id: &str) -> Result<(), String> {
    let skill_id = skill_id.trim();
    if !skill_id.is_empty() {
        crate::skills::resolve_check_skill(skill_id)?; // 不合法/未安装直接拒,不留到跑检查才炸
    }
    let conn = open_db()?;
    conn.execute(
        "UPDATE projects SET check_skill=?1 WHERE id=?2",
        params![skill_id, project_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 读某轮检查结果。
pub fn list(task_id: i64, round: i64) -> Result<Vec<CheckRun>, String> {
    let conn = open_db()?;
    let mut stmt = conn
        .prepare("SELECT name,status,output,started_at,ended_at FROM check_runs WHERE task_id=?1 AND round=?2 ORDER BY id")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![task_id, round], |r| {
            Ok(CheckRun {
                name: r.get(0)?,
                status: r.get(1)?,
                output: r.get(2)?,
                started_at: r.get(3)?,
                ended_at: r.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// 该轮检查是否全绿(pass/skipped 都算过;fail/running 不算)。无记录=未跑,不算过。
pub fn all_green(task_id: i64, round: i64) -> Result<bool, String> {
    let runs = list(task_id, round)?;
    if runs.is_empty() {
        return Ok(false);
    }
    Ok(runs
        .iter()
        .all(|r| r.status == "pass" || r.status == "skipped"))
}

/// 最近一次跑检查的轮次(=最后一次 submit 的 round)。**关键**:review 通过会把
/// tasks.round +1(见 tasks::review),使 card.round 漂移到检查记录之上;合并闸/前端
/// 若按 card.round 查就会落空(空=未全绿)→ 干净卡永远合不进。故一律按「检查实际落库
/// 的最大轮次」判定,天然跨 reject→重提 周期正确(旧轮记录留着,max 恒为最后一次提交)。
pub fn latest_round(task_id: i64) -> Option<i64> {
    let conn = open_db().ok()?;
    conn.query_row(
        "SELECT MAX(round) FROM check_runs WHERE task_id=?1",
        params![task_id],
        |r| r.get::<_, Option<i64>>(0),
    )
    .ok()
    .flatten()
}

/// 该轮检查针对的分支提交(SHA)。合并闸用它对比当前分支头:检查过了之后又推
/// 新提交(GitHub 是按 commit 查,我们按轮查,这里补上防陈旧)→ 要求重跑。
pub fn round_sha(task_id: i64, round: i64) -> Option<String> {
    let conn = open_db().ok()?;
    conn.query_row(
        "SELECT sha FROM check_runs WHERE task_id=?1 AND round=?2 AND sha!='' LIMIT 1",
        params![task_id, round],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

fn record(task_id: i64, round: i64, r: &CheckRun) {
    if let Ok(conn) = open_db() {
        let _ = conn.execute(
            "INSERT INTO check_runs(task_id,round,name,status,output,started_at,ended_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![task_id, round, r.name, r.status, r.output, r.started_at, r.ended_at],
        );
    }
}

fn clear_round(task_id: i64, round: i64) {
    if let Ok(conn) = open_db() {
        let _ = conn.execute(
            "DELETE FROM check_runs WHERE task_id=?1 AND round=?2",
            params![task_id, round],
        );
    }
}

/// 同步跑完一轮检查(调用方决定放哪个线程)。emit 回调用于 collab:check 事件。
pub fn run_for_task(
    repo: &Path,
    branch: &str,
    task_id: i64,
    round: i64,
    profile: &str,
    emit: &dyn Fn(),
) -> Result<(), String> {
    if profile == "off" {
        return Ok(());
    }
    // 检查会执行仓库自带脚本/npm lifecycle/build.rs，本质是宿主 RCE。未接隔离 runner 前
    // 必须默认关闭；只有管理员明确接受风险并设置开关才运行。关闭态落一条 fail 记录，既让
    // UI 可见原因，也确保合并闸不会把「根本没检查」误当全绿。
    let explicitly_disabled = std::env::var("POLARIS_CHECKS_DISABLED")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let unsafe_host_enabled = !explicitly_disabled
        && std::env::var("POLARIS_CHECKS_UNSAFE_HOST")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
    if !unsafe_host_enabled {
        clear_round(task_id, round);
        record(
            task_id,
            round,
            &CheckRun {
                name: "安全隔离".into(),
                status: "fail".into(),
                output: "宿主执行检查默认关闭。请接入隔离 runner；仅明确接受不可信代码执行风险时，管理员才可设置 POLARIS_CHECKS_UNSAFE_HOST=1。".into(),
                started_at: now(),
                ended_at: now(),
            },
        );
        emit();
        return Err("宿主检查未启用（需要隔离 runner）".into());
    }
    // 全局排队:一次只跑一轮(主机不是 CI 农场);同卡重跑也被天然互斥。
    let _run = RUN_LOCK.lock();
    sweep_stale_running();
    clear_round(task_id, round);
    // 钉住本轮检查针对的提交:合并闸拿它对比分支头,防「检查后又推新提交」的陈旧窗口。
    let sha = run_cmd(repo, &["rev-parse", branch], STEP_TIMEOUT)
        .ok()
        .filter(|(ok, _)| *ok)
        .map(|(_, s)| s.trim().to_string())
        .unwrap_or_default();
    // 临时 worktree(检完即删;失败也尽力清)。
    let wt = std::env::temp_dir().join(format!(
        "polaris-check-{task_id}-{round}-{}",
        std::process::id()
    ));
    let wts = wt.to_string_lossy().to_string();
    let _ = run_cmd(repo, &["worktree", "prune"], STEP_TIMEOUT);
    let out = run_cmd(
        repo,
        &["worktree", "add", "--detach", &wts, branch],
        STEP_TIMEOUT,
    )?;
    if !out.0 {
        record(
            task_id,
            round,
            &CheckRun {
                name: "checkout".into(),
                status: "fail".into(),
                output: tail(&out.1),
                started_at: now(),
                ended_at: now(),
            },
        );
        emit();
        return Err("worktree 检出失败".into());
    }
    let result = run_steps(&wt, task_id, round, profile, emit);
    let _ = run_cmd(repo, &["worktree", "remove", "--force", &wts], STEP_TIMEOUT);
    let _ = std::fs::remove_dir_all(&wt);
    // 统一补 sha(running 行没有终态前闸门本来就不放行,末尾一次性盖章足够)。
    if !sha.is_empty() {
        if let Ok(conn) = open_db() {
            let _ = conn.execute(
                "UPDATE check_runs SET sha=?1 WHERE task_id=?2 AND round=?3",
                params![sha, task_id, round],
            );
        }
    }
    result
}

/// 分支相对 main 的改动文件表(GitHub 语义:只审「这个分支引入了什么」)。
/// 拿不到(如仓库没有 main)→ None = 回退全树扫描。
fn changed_files(wt: &Path) -> Option<Vec<String>> {
    // 必须用 -z(NUL 分隔、原始路径)!默认 `--name-only` 会把非 ASCII 路径 C-quote 成
    // "\347\247\230.txt" 这种转义串,wt.join() 找不到真实文件 → 中文名文件里的密钥/大文件
    // 全逃过扫描(独立审计实测的高危绕过)。-z 输出 UTF-8 原始字节,中文名原样保留。
    let (ok, out) = run_cmd(
        wt,
        &["diff", "-z", "--name-only", "main...HEAD"],
        STEP_TIMEOUT,
    )
    .ok()?;
    if !ok {
        return None;
    }
    Some(
        out.split('\0')
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    )
}

fn run_steps(
    wt: &Path,
    task_id: i64,
    round: i64,
    profile: &str,
    emit: &dyn Fn(),
) -> Result<(), String> {
    // 增量语义:密钥/大文件只审分支改动的文件 —— 老仓库 main 上的存量大素材/历史密钥
    // 不该挡住每一张卡(那会逼人人 force,闸门形同虚设)。构建类检查仍是全树(编译本来就是整体)。
    let diff = changed_files(wt);
    // ① 密钥扫描 + ② 大文件闸:所有档位都跑(creative 只是上限放宽)。不可关的前置硬闸。
    step(task_id, round, "密钥扫描", emit, || {
        secret_scan(wt, diff.as_deref())
    });
    let max_mb: u64 = if profile == "creative" { 500 } else { 50 };
    step(task_id, round, "大文件闸", emit, || {
        big_file_scan(wt, max_mb, diff.as_deref())
    });
    // ③ 地盘越界闸:改动必须落在任务卡 scope 内(所有档位;卡没圈 scope 则跳过)。
    let card = super::tasks::get(task_id).ok();
    let scope_csv = card.as_ref().map(|c| c.scope.clone()).unwrap_or_default();
    step(task_id, round, "地盘越界", emit, || {
        scope_gate(diff.as_deref(), &scope_csv)
    });
    if profile == "creative" {
        return Ok(()); // 视频/游戏素材仓:不跑构建/静态检查
    }
    // ④ 项目检测:执行项目指定的检查技能(默认内置 project-check-default,把原先
    // 硬编码的 cargo/npm/ruff 探测搬进了技能脚本)。技能缺失/坏协议 = fail,不静默放行。
    let project_id = card.as_ref().map(|c| c.project_id).unwrap_or(0);
    let skill_id = project_check_skill(project_id);
    step(task_id, round, "项目检测", emit, || {
        skill_check_step(wt, task_id, profile, &skill_id)
    });
    Ok(())
}

/// 地盘越界闸:分支改动文件 ∉ 任务 scope 前缀 → fail。scope 空=卡没圈地盘,跳过;
/// diff 拿不到(如无 main)也跳过(增量语义没得比,不误伤)。确定性路径比对,无 AI。
fn scope_gate(diff: Option<&[String]>, scope_csv: &str) -> (String, String) {
    let prefixes: Vec<String> = scope_csv
        .split(',')
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if prefixes.is_empty() {
        return ("skipped".into(), "任务卡未圈定 scope,跳过越界检查".into());
    }
    let Some(files) = diff else {
        return (
            "skipped".into(),
            "拿不到分支增量(仓库无 main?),跳过越界检查".into(),
        );
    };
    let outside: Vec<&String> = files
        .iter()
        .filter(|f| {
            !prefixes
                .iter()
                .any(|p| **f == *p || f.starts_with(&format!("{p}/")))
        })
        .take(20)
        .collect();
    if outside.is_empty() {
        (
            "pass".into(),
            format!("改动均在任务地盘内(scope: {})", prefixes.join(", ")),
        )
    } else {
        (
            "fail".into(),
            format!(
                "以下改动越出任务地盘(scope: {}):\n{}",
                prefixes.join(", "),
                outside
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )
    }
}

/// 项目检测:按检查协议执行技能入口脚本。退出码 0=pass 非 0=fail;超时=fail(防死循环
/// 绕过);技能缺失/协议坏=fail(闸门默认是拦,不静默放行)。脚本只从主机本机技能目录读。
fn skill_check_step(wt: &Path, task_id: i64, profile: &str, skill_id: &str) -> (String, String) {
    let entry = match crate::skills::resolve_check_skill(skill_id) {
        Ok(e) => e,
        Err(e) => return ("fail".into(), e),
    };
    let entry_s = entry.entry.to_string_lossy().to_string();
    let (prog, args): (&str, Vec<&str>) = if entry.windows {
        (
            "powershell",
            vec![
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                &entry_s,
            ],
        )
    } else {
        ("sh", vec![&entry_s])
    };
    let wt_s = wt.to_string_lossy().to_string();
    let tid = task_id.to_string();
    let envs: Vec<(&str, &str)> = vec![
        ("POLARIS_CHECK_DIR", wt_s.as_str()),
        ("POLARIS_CHECK_PROFILE", profile),
        ("POLARIS_TASK_ID", tid.as_str()),
    ];
    match run_prog_env(wt, prog, &args, &envs, entry.timeout_secs) {
        Ok((true, out)) => (
            "pass".into(),
            format!("技能 {skill_id} 通过\n{}", tail(&out)),
        ),
        Ok((false, out)) => (
            "fail".into(),
            format!("技能 {skill_id} 判不通过\n{}", tail(&out)),
        ),
        Err(e) if e.contains("timeout") => (
            "fail".into(),
            format!(
                "技能 {skill_id} 超时({}s)未跑完,判失败(防卡死绕过)",
                entry.timeout_secs
            ),
        ),
        Err(e) => ("fail".into(), format!("技能 {skill_id} 无法执行: {e}")),
    }
}

/// 单步骨架:先落 running(前端能看到进度),跑完覆写终态。
fn step(
    task_id: i64,
    round: i64,
    name: &str,
    emit: &dyn Fn(),
    f: impl FnOnce() -> (String, String),
) {
    let started = now();
    record(
        task_id,
        round,
        &CheckRun {
            name: name.into(),
            status: "running".into(),
            output: String::new(),
            started_at: started,
            ended_at: 0,
        },
    );
    emit();
    let (status, output) = f();
    if let Ok(conn) = open_db() {
        let _ = conn.execute(
            "UPDATE check_runs SET status=?1, output=?2, ended_at=?3 WHERE task_id=?4 AND round=?5 AND name=?6",
            params![status, output, now(), task_id, round, name],
        );
    }
    emit();
}

// 注:原 tool_present/shell_step/npm_script_exists(硬编码工具链探测)已整体搬进
// 内置「项目检测」技能的 check.ps1/check.sh —— 探测规则改脚本即可,不再改 Rust。
// 工具缺失=脚本内跳过该项(不误伤);超时/技能缺失=fail(闸门默认拦)语义保留在 skill_check_step。

/// git 命令(在 repo 目录)。返回 (成功?, 合并输出)。
fn run_cmd(repo: &Path, args: &[&str], timeout: u64) -> Result<(bool, String), String> {
    run_prog(repo, "git", args, timeout)
}

/// 后台线程排空一根管道(防子进程输出撑满 pipe 缓冲区死锁);非 UTF-8 有损转换。
fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut r) = pipe {
            let _ = r.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    })
}

fn run_prog(cwd: &Path, prog: &str, args: &[&str], timeout: u64) -> Result<(bool, String), String> {
    run_prog_env(cwd, prog, args, &[], timeout)
}

/// 跨平台起进程 + 超时 kill。Windows 上 npm/npx/ruff 多为 .cmd/.exe,统一走 cmd /C。
fn run_prog_env(
    cwd: &Path,
    prog: &str,
    args: &[&str],
    envs: &[(&str, &str)],
    timeout: u64,
) -> Result<(bool, String), String> {
    // Windows 上 npm/npx/ruff 多为 .cmd,须走 cmd /C 才找得到;git/cargo/powershell 是真 .exe 直调。
    let used_cmd = cfg!(windows) && prog != "git" && prog != "cargo" && prog != "powershell";
    let mut cmd = if used_cmd {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(prog).args(args);
        c
    } else {
        let mut c = Command::new(prog);
        c.args(args);
        c
    };
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{prog} not found / 启动失败: {e}"))?;
    // 边跑边排空 stdout/stderr:不排空的话输出超过 pipe 缓冲区子进程会写阻塞,永远等不到退出。
    let out_h = drain(child.stdout.take());
    let err_h = drain(child.stderr.take());
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => {
                let mut out = out_h.join().unwrap_or_default();
                out.push_str(&err_h.join().unwrap_or_default());
                // cmd /C 找不到命令 → 退出码 9009(命令不存在),判「工具缺失」而非 fail。
                // **只认 9009 这个精确码**,绝不匹配输出串:真 Rust 编译错误常含
                // "cannot find value/function",按串匹配会把真失败误判成跳过 → 假放行(更危险)。
                if used_cmd && status.code() == Some(9009) {
                    return Err(format!("{prog} not found (exit 9009)"));
                }
                return Ok((status.success(), out));
            }
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("timeout after {timeout}s"));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

fn tail(s: &str) -> String {
    if s.len() <= OUTPUT_TAIL {
        return s.to_string();
    }
    // 对齐到字符边界再切(工具输出常含中文,硬切字节会 panic)。
    let mut i = s.len() - OUTPUT_TAIL;
    while !s.is_char_boundary(i) {
        i += 1;
    }
    format!("…(截前略)…\n{}", &s[i..])
}

/// 待扫文件集:有 diff 表就只扫分支改动(GitHub 语义);拿不到 diff 回退全树。
/// 统一产出 (绝对路径, 相对展示名) 列表,已过滤目录/删除项/.git/node_modules/target。
fn scan_targets(wt: &Path, diff: Option<&[String]>) -> Vec<(std::path::PathBuf, String)> {
    let mut out = Vec::new();
    match diff {
        Some(files) => {
            for rel in files {
                let p = wt.join(rel);
                if p.is_file() {
                    out.push((p, rel.clone()));
                }
            }
        }
        None => {
            for entry in walkdir::WalkDir::new(wt)
                .into_iter()
                .filter_entry(|e| {
                    let n = e.file_name().to_string_lossy();
                    n != ".git" && n != "node_modules" && n != "target"
                })
                .flatten()
            {
                if entry.file_type().is_file() {
                    let rel = entry
                        .path()
                        .strip_prefix(wt)
                        .unwrap_or(entry.path())
                        .display()
                        .to_string();
                    out.push((entry.path().to_path_buf(), rel));
                }
            }
        }
    }
    out
}

/// 密钥扫描单文件上限:16MB。原来 2MB 太小 —— 密钥可藏进 3MB config.json 逃扫(实测)。
/// 16-50MB 的残余缝隙由大文件闸兜(>50MB 直接拦);对 16MB 内的文件全扫。
const SECRET_SCAN_CAP: u64 = 16 * 1024 * 1024;

/// 密钥扫描:轻量正则内置(开源 gitleaks 的常见模式子集,不引外部依赖)。
/// ≤16MB 的文件全扫;非 UTF-8 也按 lossy 转 ASCII 扫(密钥都是 ASCII token,一个坏字节
/// 不该让整个 .env 逃扫)。diff 模式只审分支改动。命中即 fail 并指出文件。
fn secret_scan(wt: &Path, diff: Option<&[String]>) -> (String, String) {
    let pats: &[(&str, &str)] = &[
        ("AWS AccessKey", r"AKIA[0-9A-Z]{16}"),
        ("私钥块", r"-----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY-----"),
        ("GitHub Token", r"ghp_[A-Za-z0-9]{36}"),
        ("Slack Token", r"xox[baprs]-[A-Za-z0-9-]{10,}"),
        (
            "通用 api_key 赋值",
            r#"(?i)(api[_-]?key|secret[_-]?key)\s*[:=]\s*['"][A-Za-z0-9_\-]{20,}['"]"#,
        ),
    ];
    let res: Vec<regex::Regex> = pats
        .iter()
        .filter_map(|(_, p)| regex::Regex::new(p).ok())
        .collect();
    let mut hits = Vec::new();
    'files: for (path, rel) in scan_targets(wt, diff) {
        let Ok(md) = path.metadata() else { continue };
        if md.len() > SECRET_SCAN_CAP {
            continue;
        }
        // 读字节后 lossy 转字符串:非 UTF-8 文件(.env 混一个坏字节)不再整体跳过,
        // ASCII 密钥 token 在 lossy 转换后原样保留,仍能被正则命中。
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        for (i, re) in res.iter().enumerate() {
            if re.is_match(&text) {
                hits.push(format!("{} → {}", pats[i].0, rel));
                if hits.len() >= 20 {
                    break 'files;
                }
            }
        }
    }
    let scope = if diff.is_some() {
        "分支改动"
    } else {
        "全仓"
    };
    if hits.is_empty() {
        ("pass".into(), format!("未发现疑似密钥(范围:{scope})"))
    } else {
        ("fail".into(), hits.join("\n"))
    }
}

/// 大文件闸:超过上限的文件列出来。creative 档上限放宽(素材仓);diff 模式只审分支改动
/// —— main 上的存量大素材不挡新卡。
fn big_file_scan(wt: &Path, max_mb: u64, diff: Option<&[String]>) -> (String, String) {
    let cap = max_mb * 1024 * 1024;
    let mut hits = Vec::new();
    for (path, rel) in scan_targets(wt, diff) {
        if let Ok(md) = path.metadata() {
            if md.len() > cap {
                hits.push(format!("{rel}({} MB)", md.len() / 1024 / 1024));
                if hits.len() >= 20 {
                    break;
                }
            }
        }
    }
    let scope = if diff.is_some() {
        "分支改动"
    } else {
        "全仓"
    };
    if hits.is_empty() {
        ("pass".into(), format!("无 >{max_mb}MB 文件(范围:{scope})"))
    } else {
        (
            "fail".into(),
            format!("超过 {max_mb}MB 上限:\n{}", hits.join("\n")),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::db::TEST_LOCK;

    fn git(repo: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git 启动失败");
        assert!(
            out.status.success(),
            "git {args:?} 失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// code 档:密钥扫描抓到假 AWS key=fail,大文件闸 pass,all_green=false;
    /// creative 档:只剩密钥扫描+大文件闸两项。
    /// 超时机制:短超时跑一个会睡很久的命令,应在 deadline 附近返回 Err(timeout),
    /// 且不会挂到 sleep 结束(证明 kill 生效)。shell_step 会把这个 Err 映射成 fail。
    #[test]
    fn run_prog_kills_on_timeout() {
        let dir = std::env::temp_dir();
        let t0 = std::time::Instant::now();
        // 跨平台睡 30s;超时设 2s。
        let r = if cfg!(windows) {
            // ping -n 31 ≈ 睡 30s(比 timeout.exe 在无 TTY 下更稳)
            super::run_prog(&dir, "ping", &["-n", "31", "127.0.0.1"], 2)
        } else {
            super::run_prog(&dir, "sleep", &["30"], 2)
        };
        let elapsed = t0.elapsed().as_secs();
        assert!(r.is_err(), "应超时返回 Err,实际 {r:?}");
        assert!(r.unwrap_err().contains("timeout"), "Err 应含 timeout");
        assert!(
            elapsed < 15,
            "应在 ~2s 超时附近返回,而非等满 30s(实际 {elapsed}s)"
        );
    }

    /// 地盘越界闸纯函数路径:界内 pass、越界 fail 且列出文件、空 scope / 无 diff 跳过。
    #[test]
    fn scope_gate_paths() {
        let files = vec!["src/a.rs".to_string(), "docs/b.md".to_string()];
        let (st, _) = super::scope_gate(Some(&files), "src, docs");
        assert_eq!(st, "pass");
        let (st, out) = super::scope_gate(Some(&files), "src");
        assert_eq!(st, "fail");
        assert!(out.contains("docs/b.md"), "应列出越界文件: {out}");
        // 前缀必须按路径段匹配:srcx/ 不算 src/ 界内。
        let tricky = vec!["srcx/evil.rs".to_string()];
        let (st, _) = super::scope_gate(Some(&tricky), "src");
        assert_eq!(st, "fail");
        assert_eq!(super::scope_gate(Some(&files), "").0, "skipped");
        assert_eq!(super::scope_gate(None, "src").0, "skipped");
    }

    #[test]
    fn checks_run_for_task_profiles() {
        let _g = TEST_LOCK.lock().unwrap();
        let tmpdb = std::env::temp_dir().join(format!("collab-checks-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&tmpdb);
        std::env::set_var("POLARIS_COLLAB_DB", &tmpdb);
        std::env::set_var("POLARIS_CHECKS_UNSAFE_HOST", "1");
        // check_runs 有 tasks 外键(foreign_keys=ON),先种上项目+卡。
        {
            let conn = open_db().unwrap();
            conn.execute(
                "INSERT INTO projects(id,name,repo,created_at) VALUES(1,'t','',0)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO tasks(id,project_id,title,created_at,updated_at) VALUES(1,1,'t',0,0)",
                [],
            )
            .unwrap();
        }
        // 临时 git 仓:main 干净,feat/t1 上有个带假 AWS key 的文件。
        let repo = std::env::temp_dir().join(format!("collab-checks-repo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]); // 显式 main:diff 增量语义按 main...HEAD 算
        git(&repo, &["config", "user.email", "t@test.local"]);
        git(&repo, &["config", "user.name", "tester"]);
        // 假 key 拼接构造,避免本仓库自己被密钥扫描类工具误报。
        let key = format!("{}{}", "AKIA", "ABCDEFGHIJKLMNOP");
        std::fs::write(repo.join("README.md"), "hello").unwrap();
        // main 上的「历史遗留密钥」:增量语义下不该挡新分支的卡(不是这张卡引入的)。
        std::fs::write(repo.join("legacy.txt"), format!("old = {key}\n")).unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "init"]);
        git(&repo, &["checkout", "-b", "feat/t1"]);
        std::fs::write(repo.join("leak.txt"), format!("aws_id = {key}\n")).unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "leak"]);

        // code 档(仓里没有 Cargo.toml/package.json/pyproject → 只有两项内置检查)。
        run_for_task(&repo, "feat/t1", 1, 0, "code", &|| {}).unwrap();
        let runs = list(1, 0).unwrap();
        let sec = runs
            .iter()
            .find(|r| r.name == "密钥扫描")
            .expect("缺密钥扫描项");
        assert_eq!(sec.status, "fail", "假 AWS key 应被抓到: {}", sec.output);
        assert!(
            sec.output.contains("leak.txt"),
            "输出应指出文件: {}",
            sec.output
        );
        assert!(
            !sec.output.contains("legacy.txt"),
            "main 上的历史密钥不该挡这张卡(增量语义): {}",
            sec.output
        );
        let big = runs
            .iter()
            .find(|r| r.name == "大文件闸")
            .expect("缺大文件闸项");
        assert_eq!(big.status, "pass");
        assert!(!all_green(1, 0).unwrap(), "有 fail 不该全绿");
        // SHA 钉住:本轮记录应带分支头提交。
        let sha = round_sha(1, 0).expect("本轮应记下 SHA");
        assert_eq!(sha.len(), 40, "应是完整 commit sha: {sha}");

        // 地盘越界:测试卡没圈 scope → 跳过(不误伤)。
        let gate = runs
            .iter()
            .find(|r| r.name == "地盘越界")
            .expect("缺地盘越界项");
        assert_eq!(gate.status, "skipped", "无 scope 应跳过: {}", gate.output);
        // 项目检测(code 档才有):技能存在与否都必须有终态记录,绝不静默消失。
        let sk = runs
            .iter()
            .find(|r| r.name == "项目检测")
            .expect("缺项目检测项");
        assert!(
            sk.status == "pass" || sk.status == "fail",
            "项目检测应有终态: {}",
            sk.status
        );

        // creative 档:clear_round 后重跑,只剩三项(不跑项目检测/构建类)。
        run_for_task(&repo, "feat/t1", 1, 0, "creative", &|| {}).unwrap();
        let runs = list(1, 0).unwrap();
        assert_eq!(runs.len(), 3, "creative 档只留密钥扫描+大文件闸+地盘越界");
        assert!(runs
            .iter()
            .all(|r| r.name == "密钥扫描" || r.name == "大文件闸" || r.name == "地盘越界"));

        // off 档:直接返回,不清也不写。
        run_for_task(&repo, "feat/t1", 1, 0, "off", &|| {}).unwrap();
        assert_eq!(list(1, 0).unwrap().len(), 3);

        // 档位读写 + 非法值拒绝。
        assert_eq!(project_profile(1), "code");
        set_project_profile(1, "creative").unwrap();
        assert_eq!(project_profile(1), "creative");
        assert!(set_project_profile(1, "yolo").is_err());

        std::env::remove_var("POLARIS_COLLAB_DB");
        std::env::remove_var("POLARIS_CHECKS_UNSAFE_HOST");
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_file(&tmpdb);
    }
}
