//! 临时诊断用: 单独拉起 Codex 翻译代理并常驻, 打印端口。
//! 等价于坞里点「用 GPT 对话」时 cfg_for_view 走的 ensure_running() 那一步。
//! 跑法: cargo run -p polaris-kernel --example codex_proxy_standalone

fn main() {
    match polaris_kernel::integrations::codex_proxy::ensure_running() {
        Ok(port) => {
            println!("PROXY_PORT={port}");
            println!("ANTHROPIC_BASE_URL=http://127.0.0.1:{port}");
            loop {
                std::thread::sleep(std::time::Duration::from_secs(5));
                let e = polaris_kernel::integrations::codex_proxy::last_error();
                if !e.is_empty() {
                    println!("LAST_ERROR={e}");
                }
            }
        }
        Err(e) => {
            eprintln!("代理启动失败: {e}");
            std::process::exit(1);
        }
    }
}
