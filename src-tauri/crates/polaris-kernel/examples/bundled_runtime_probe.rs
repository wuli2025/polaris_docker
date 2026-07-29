//! 随包运行时 (uv / Python / Git Bash) 的**端到端真机探针**。
//!
//! `cargo run -p polaris-kernel --example bundled_runtime_probe`
//!
//! 单测只能证明「路径拼得对」,证明不了「claude 子进程里 bash 真能用」。这个探针按
//! 真实链路走一遍并**自证**:
//!   ① 运行时根找得到, uv/python/bash 三件套解析出来的路径真实存在;
//!   ② env_check 报 shell_ready(即干净 Windows 也不必装 PowerShell 7);
//!   ③ 用 harden_child_env 造出的**真实子进程环境**跑内置 bash, 检查
//!      `ls/cat/grep/sed/awk/head/tail/wc/cut/tr/find/sort/xargs` 是否**全部解析到内置
//!      运行时目录内** —— 这一条专门盯死那个静默坑: 非登录 bash 不读 /etc/profile 时
//!      `find`/`sort` 会命中 System32 的同名 exe, 语义完全不同却不报错。
//!   ④ 同一套环境里 `uv --version` / `python --version` 能跑。
//!
//! 退出码非 0 = 有项没过, 可直接挂进发版前的冒烟。

#[cfg(not(feature = "desktop"))]
fn main() {
    use polaris_kernel::doctor::{self, bundled};
    use std::process::Command;

    let mut fail = 0usize;
    let mut check = |ok: bool, what: &str| {
        println!("{} {}", if ok { "PASS" } else { "FAIL" }, what);
        if !ok {
            fail += 1;
        }
    };

    // ① 运行时根 + 三件套
    let Some(root) = bundled::runtime_root() else {
        eprintln!(
            "FAIL 找不到随包运行时根目录。\n  \
             开发期请先抓一次: node scripts/fetch-runtimes.mjs"
        );
        std::process::exit(1);
    };
    println!("runtime_root = {}", root.display());
    let uv = bundled::uv_exe();
    let py = bundled::python_exe();
    let bash = bundled::git_bash();
    println!("  uv     = {:?}", uv);
    println!("  python = {:?}", py);
    println!("  bash   = {:?}", bash);
    check(uv.as_ref().is_some_and(|p| p.exists()), "内置 uv 存在");
    check(py.as_ref().is_some_and(|p| p.exists()), "内置 Python 存在");
    if cfg!(windows) {
        check(
            bash.as_ref().is_some_and(|p| p.exists()),
            "内置 Git Bash 存在",
        );
    }

    // ② env_check: 干净机器也应 shell_ready
    let r = doctor::env_check();
    println!(
        "env_check: ready={} shell_ready={} shell_bundled={} uv.found={} uv.bundled={} python.found={}",
        r.ready, r.shell_ready, r.shell_bundled, r.uv.found, r.uv.bundled, r.python.found
    );
    check(r.shell_ready, "shell_ready = true(无需再装 PowerShell 7)");
    check(r.uv.found, "env_check 能发现 uv");
    check(r.python.found, "env_check 能发现 Python");

    // ③ 真实子进程环境里的 unix 工具解析 —— 本探针的重点
    if cfg!(windows) {
        let Some(bash) = bash else {
            eprintln!("FAIL 没有内置 bash, 跳过子进程环境验证");
            std::process::exit(1);
        };

        // ③.0 内置 bash 必须**不靠我们注入 PATH 也能起来** —— msys 的 bash 要同目录的
        // msys-2.0.dll, 放错目录的副本会 0xC0000135 挂掉, 且只在注入 PATH 时才看不出来。
        // Claude Code 自己拉这个 bash 时未必带我们的环境, 故单独守一道。
        let standalone = Command::new(&bash).args(["-c", "exit 0"]).status();
        check(
            standalone.map(|s| s.success()).unwrap_or(false),
            "内置 bash 裸跑可用(不依赖我们注入的 PATH)",
        );
        // 造一条与 chat 里 spawn claude 同样被加固过的命令, 把它的 env 复制到 bash 上
        let mut probe = Command::new(&bash);
        doctor::harden_child_env(&mut probe);
        let tools = "ls cat grep sed awk head tail wc cut tr find sort xargs";
        probe.args([
            "-c",
            &format!("for c in {tools}; do printf '%s\\t%s\\n' \"$c\" \"$(command -v $c || echo MISSING)\"; done"),
        ]);
        let out = probe.output().expect("跑内置 bash 失败");
        let stdout = String::from_utf8_lossy(&out.stdout);
        // 判定口径: bash 是从 runtime/git 里拉起的, msys 会把该目录挂成自己的 `/`,
        // 故内置工具在 bash 里显示为 `/usr/bin/xxx`(即 runtime/git/usr/bin/xxx),
        // **不会**显示成 Windows 绝对路径。要守的是两件坏事:
        //   · MISSING            —— 非登录 bash 没有 /usr/bin 时的表现
        //   · /c/WINDOWS/...     —— find/sort 静默命中 System32 同名 exe(语义不同却不报错)
        let mut bad = Vec::new();
        for line in stdout.lines() {
            let (tool, path) = line.split_once('\t').unwrap_or((line, ""));
            let good = (path.starts_with("/usr/bin/") || path.starts_with("/mingw64/bin/"))
                && !path.to_ascii_lowercase().contains("/windows/");
            println!("    {tool:<6} -> {path}");
            if !good {
                bad.push(format!("{tool} -> {path}"));
            }
        }
        check(
            bad.is_empty(),
            &format!(
                "13 个 unix 工具全部走内置 msys(非 System32){}",
                if bad.is_empty() {
                    String::new()
                } else {
                    format!(" —— 越界: {}", bad.join(", "))
                }
            ),
        );

        // ④ 同一环境里 uv / python 可跑
        for (bin, label) in [("uv --version", "uv"), ("python --version", "python")] {
            let mut c = Command::new(&bash);
            doctor::harden_child_env(&mut c);
            c.args(["-c", bin]);
            let o = c.output().expect("spawn 失败");
            let txt = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            println!("    {label}: {}", txt.trim());
            check(o.status.success(), &format!("bash 里能跑 {label}"));
        }
    }

    println!("\n{}", if fail == 0 { "全部通过" } else { "有失败项" });
    std::process::exit(if fail == 0 { 0 } else { 1 });
}

#[cfg(feature = "desktop")]
fn main() {
    eprintln!(
        "本样例需在默认 feature 下跑(desktop 下 env_check 是 async 的):\n  \
         cargo run -p polaris-kernel --example bundled_runtime_probe"
    );
}
