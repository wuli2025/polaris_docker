//! 一次性远程盘探针(本地手测用,不进产品):
//! `cargo run -p polaris-collab --example fs_probe --features collab-net -- <NodeId> <token> [path] [port]`
//! 起 iroh 隧道后调对端 /api/fs/list,把目录内容打出来。
use std::io::{Read, Write};

fn get(port: u16, token: &str, rel: &str) -> Result<String, String> {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(30))).ok();
    let enc: String = rel
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    let req = format!(
        "GET /api/fs/list?path={enc} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = String::new();
    s.read_to_string(&mut buf).map_err(|e| e.to_string())?;
    if buf.is_empty() {
        return Err("响应为空(隧道未就绪)".into());
    }
    Ok(buf)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let node = args
        .next()
        .expect("用法: fs_probe <NodeId> <token> [path] [port]");
    let token = args.next().unwrap_or_default();
    let rel = args.next().unwrap_or_default();
    let port: u16 = args
        .next()
        .unwrap_or_else(|| "18630".into())
        .parse()
        .expect("port 非法");

    polaris_collab::collab::tunnel::connect_client(&node, port).expect("connect_client 失败");
    eprintln!("[probe] 隧道已登记(127.0.0.1:{port} ↔ {node}),等待打洞…");

    // path == "hold":只挂住隧道 10 分钟,外部用 curl 自由探端点。
    if rel == "hold" {
        eprintln!("[probe] hold 模式:隧道保持 600s,请用 http://127.0.0.1:{port} 访问");
        std::thread::sleep(std::time::Duration::from_secs(600));
        return;
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    loop {
        if std::time::Instant::now() > deadline {
            eprintln!("[probe] 90s 超时未连通");
            std::process::exit(1);
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
        match get(port, &token, &rel) {
            Ok(resp) => {
                println!("== /api/fs/list?path={rel} ==");
                println!("{}", &resp[..resp.len().min(8000)]);
                return;
            }
            Err(e) => eprintln!("[probe] 未就绪: {e}"),
        }
    }
}
