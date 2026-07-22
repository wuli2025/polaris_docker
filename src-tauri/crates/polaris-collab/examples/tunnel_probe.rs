//! 一次性隧道探针(本地手测用,不进产品):
//! `cargo run -p polaris-collab --example tunnel_probe --features collab-net -- <NodeId> [port] [token]`
//! 拉起 127.0.0.1:port ↔ NodeId 的 iroh 隧道,轮询 /api/collab/me 验证连通与令牌。
use std::io::{Read, Write};

fn main() {
    let mut args = std::env::args().skip(1);
    let node = args
        .next()
        .expect("用法: tunnel_probe <NodeId> [port] [token]");
    let port: u16 = args
        .next()
        .unwrap_or_else(|| "18620".into())
        .parse()
        .expect("port 非法");
    let token = args.next().unwrap_or_default();

    polaris_collab::collab::tunnel::connect_client(&node, port).expect("connect_client 失败");
    eprintln!("[probe] 隧道已登记(127.0.0.1:{port} ↔ {node}),等待打洞…");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    let mut last_err = String::new();
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_secs(2));
        match std::net::TcpStream::connect(("127.0.0.1", port)) {
            Ok(mut s) => {
                s.set_read_timeout(Some(std::time::Duration::from_secs(10))).ok();
                let req = format!(
                    "GET /api/collab/me HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
                );
                if s.write_all(req.as_bytes()).is_err() {
                    last_err = "写请求失败".into();
                    continue;
                }
                let mut buf = String::new();
                if s.read_to_string(&mut buf).is_ok() && !buf.is_empty() {
                    let head = &buf[..buf.len().min(2000)];
                    println!("== 响应 ==\n{head}");
                    if buf.starts_with("HTTP/1.1 200") {
                        println!("[probe] 隧道连通且令牌有效 ✓");
                    } else {
                        println!("[probe] 隧道已连通,但接口返回非 200(见上)");
                    }
                    return;
                }
                last_err = "读响应为空(隧道未就绪)".into();
            }
            Err(e) => last_err = e.to_string(),
        }
    }
    eprintln!("[probe] 90s 超时未连通:{last_err}");
    std::process::exit(1);
}
