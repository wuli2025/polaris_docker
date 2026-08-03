//! 本机跑一遍环境医生的**深度校验**, 把报告原样打出来 —— 开发期核对用。
//!
//! ```powershell
//! cargo run -p polaris-kernel --example env_verify_probe          # 快速 (不发请求)
//! cargo run -p polaris-kernel --example env_verify_probe -- deep  # 含端到端冒烟
//! ```

fn main() {
    let deep = std::env::args().any(|a| a == "deep");
    let t = std::time::Instant::now();
    // 走同步版本体:桌面 flavor 下 env_verify 是 async(交给 Tauri 运行时 spawn_blocking),
    // example 里没有运行时,直接调会拿到 Future。两者跑的是同一份 env_verify_sync。
    let r = polaris_kernel::doctor::env_verify_sync(deep);
    println!("os={}  ok={}  耗时 {:?}", r.os, r.ok, t.elapsed());
    println!("应用内可跑={}  终端里可跑={}", r.app_runnable, r.terminal_runnable);
    println!("结论: {}\n", r.summary);

    println!("── 实跑验证 ──");
    for s in &r.steps {
        println!("[{}] {} ({} ms)\n    {}", s.status, s.name, s.ms, s.detail);
        if let Some(c) = &s.command {
            println!("    $ {c}");
        }
        if let Some(o) = &s.output {
            for line in o.lines() {
                println!("    | {line}");
            }
        }
    }

    println!("\n── 安装冲突 ({}) ──", r.conflicts.len());
    for c in &r.conflicts {
        println!("[{}] {}\n    {}", c.severity, c.title, c.detail);
        for p in &c.paths {
            println!("    · {p}");
        }
    }
}
