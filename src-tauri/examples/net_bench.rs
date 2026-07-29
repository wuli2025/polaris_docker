//! 互联数据面基准探针:把「盘慢」拆成可归因的几个数,免得靠感觉调参。
//!
//! 量四件事(全部经真实 iroh 隧道打到对端 /api/fs/*):
//!   ① 元数据往返    —— list / stat 串行 N 次的单次耗时。挂载盘「点开文件夹要等」就是这个数。
//!   ② 单流吞吐      —— 一个 GET 拉完整个大文件。这是 QUIC 单流 + 转发缓冲的上限。
//!   ③ 并行分片吞吐  —— 同一文件切 N 段并行拉。**若它远高于 ①,说明瓶颈是「一来一回」
//!      而不是带宽** —— 那么预读/流水线才是正解,调窗口调缓冲都是隔靴搔痒。
//!   ④ 小块串行读    —— 64KB 一块、一块读完再读下一块,模拟 Windows 重定向器的老实行为。
//!
//! 跑法(先在对端共享根造好基准文件,见 --help 里的提示):
//!   cargo run --release --example net_bench --features collab-host,collab-net
//!
//! 环境变量:
//!   POLARIS_PROBE_NODE   对端 `NodeId` 或 `NodeId@IP:UDP口`(带地址=直连,不绕中继)
//!   POLARIS_PROBE_TOKEN  对端 owner 令牌
//!   POLARIS_BENCH_BLOB   大文件相对路径(默认 share/_bench/blob64.bin)
//!   POLARIS_BENCH_DIR    多文件目录(默认 share/_bench/many)
#![cfg(all(feature = "collab-host", feature = "collab-net"))]

use std::io::Read;
use std::time::{Duration, Instant};

const PORT: u16 = 18991;

/// 对端信息只从环境变量来,**代码里不留默认值** —— 这个仓是公开的,而 owner 令牌
/// 等于对端那台机器的钥匙(还带共享目录写权限)。日常用 `scripts/cloud-disk.ps1`
/// 会从 `%LOCALAPPDATA%\Polaris\mesh\peer.json` 自动补齐这几个变量。
fn env_required(key: &str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            eprintln!(
                "缺少环境变量 {key}。设 POLARIS_PROBE_NODE(<NodeId>@IP:端口)与 \
                 POLARIS_PROBE_TOKEN(对端 owner 令牌)后重跑;\
                 或把它们写进 %LOCALAPPDATA%\\Polaris\\mesh\\peer.json。"
            );
            std::process::exit(2);
        }
    }
}

fn base() -> String {
    format!("http://127.0.0.1:{PORT}")
}

/// 一次 GET,返回 (状态码, 收到的字节数, 耗时)。body 直接丢进计数器,不留内存。
fn get_bytes(url: &str, range: Option<&str>, token: &str) -> Result<(u16, u64, Duration), String> {
    get_bytes_enc(url, range, token, false)
}

/// 同上,`zstd=true` 时点名要压缩传输 —— 收到的字节数按**解压后**算,这样与不压的
/// 那一档可以直接比(比的是「多久拿到这么多有效数据」,不是「传了多少字节」)。
fn get_bytes_enc(
    url: &str,
    range: Option<&str>,
    token: &str,
    zstd_on: bool,
) -> Result<(u16, u64, Duration), String> {
    let agent = ureq::builder()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(120))
        .build();
    let t0 = Instant::now();
    let mut req = agent.get(url).set("Authorization", &format!("Bearer {token}"));
    if let Some(r) = range {
        req = req.set("Range", r);
    }
    if zstd_on {
        req = req.set("X-Polaris-Zstd", "1"); // 私有标记头,见对端 fs_read_api
    }
    let resp = req.call().map_err(|e| format!("{e}"))?;
    let status = resp.status();
    let compressed = resp
        .header("Content-Encoding")
        .map(|v| v.to_ascii_lowercase().contains("zstd"))
        .unwrap_or(false);
    let raw = resp.into_reader();
    let mut reader: Box<dyn Read + Send> = if compressed {
        Box::new(zstd::stream::Decoder::new(raw).map_err(|e| format!("解压失败:{e}"))?)
    } else {
        Box::new(raw)
    };
    let mut buf = vec![0u8; 256 * 1024];
    let mut total = 0u64;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => total += n as u64,
            Err(e) => return Err(format!("读 body 失败(已收 {total} 字节): {e}")),
        }
    }
    Ok((status, total, t0.elapsed()))
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn mbps(bytes: u64, d: Duration) -> f64 {
    (bytes as f64 / 1024.0 / 1024.0) / d.as_secs_f64().max(1e-9)
}

/// 串行打 n 次,回 (平均 ms, 中位 ms, 最小 ms)。
fn serial_latency(url: &str, token: &str, n: usize) -> Result<(f64, f64, f64), String> {
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let (st, _, d) = get_bytes(url, None, token)?;
        if st != 200 {
            return Err(format!("状态码 {st}"));
        }
        v.push(ms(d));
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let avg = v.iter().sum::<f64>() / v.len() as f64;
    Ok((avg, v[v.len() / 2], v[0]))
}

fn main() {
    // 身份落点与 cloud_disk_keep 一致,复用同一个 device.key(免得每次换设备身份)。
    let home = std::env::var("POLARIS_MESH_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into()))
                .join("Polaris")
                .join("mesh")
        });
    std::fs::create_dir_all(&home).expect("建身份目录");
    if std::env::var("POLARIS_DEVICE_KEY").is_err() {
        std::env::set_var("POLARIS_DEVICE_KEY", home.join("device.key"));
    }
    if std::env::var("POLARIS_COLLAB_DB").is_err() {
        std::env::set_var("POLARIS_COLLAB_DB", home.join("collab.db"));
    }

    let node = env_required("POLARIS_PROBE_NODE");
    let token = env_required("POLARIS_PROBE_TOKEN");
    let blob = std::env::var("POLARIS_BENCH_BLOB").unwrap_or_else(|_| "share/_bench/blob64.bin".into());
    let dir = std::env::var("POLARIS_BENCH_DIR").unwrap_or_else(|_| "share/_bench/many".into());

    println!("对端 {node}");
    if let Err(e) = polaris_app_lib::collab::tunnel::connect_client(&node, PORT) {
        eprintln!("隧道建立失败: {e}");
        std::process::exit(1);
    }

    // 等对端应答(打洞握手要几秒)
    let caps_url = format!("{}/api/fs/caps", base());
    let mut ready = false;
    for _ in 0..20 {
        if get_bytes(&caps_url, None, &token).is_ok() {
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(700));
    }
    if !ready {
        eprintln!("对端没应答(隧道没通或令牌不对)");
        std::process::exit(1);
    }
    let st = polaris_app_lib::collab::tunnel::status();
    let t0 = st["tunnels"].get(0).cloned().unwrap_or_default();
    println!(
        "链路 path={} rtt={}ms\n",
        t0["path"].as_str().unwrap_or("?"),
        t0["latency_ms"]
    );

    // ① 元数据往返
    let list_url = format!("{}/api/fs/list?path={dir}", base());
    let stat_url = format!("{}/api/fs/stat?path={blob}", base());
    match serial_latency(&list_url, &token, 20) {
        Ok((a, m, lo)) => println!("① list(200 项目录) x20  平均 {a:.0}ms  中位 {m:.0}ms  最快 {lo:.0}ms"),
        Err(e) => println!("① list 失败: {e}"),
    }
    match serial_latency(&stat_url, &token, 20) {
        Ok((a, m, lo)) => println!("① stat(单文件)   x20  平均 {a:.0}ms  中位 {m:.0}ms  最快 {lo:.0}ms"),
        Err(e) => println!("① stat 失败: {e}"),
    }

    // ② 单流吞吐
    let blob_url = format!("{}/api/fs/read?path={blob}", base());
    let total = match get_bytes(&blob_url, None, &token) {
        Ok((_, n, d)) => {
            println!("\n② 单流整拉  {:.1}MB / {:.1}s = {:.2} MB/s", n as f64 / 1048576.0, d.as_secs_f64(), mbps(n, d));
            n
        }
        Err(e) => {
            eprintln!("② 单流整拉失败: {e}");
            0
        }
    };

    // ③ 并行分片
    if total > 0 {
        for parts in [2usize, 4, 8] {
            let chunk = total / parts as u64;
            let t0 = Instant::now();
            let hs: Vec<_> = (0..parts)
                .map(|i| {
                    let start = chunk * i as u64;
                    let end = if i == parts - 1 { total - 1 } else { start + chunk - 1 };
                    let url = blob_url.clone();
                    let tk = token.clone();
                    std::thread::spawn(move || {
                        get_bytes(&url, Some(&format!("bytes={start}-{end}")), &tk).map(|(_, n, _)| n)
                    })
                })
                .collect();
            let got: u64 = hs.into_iter().filter_map(|h| h.join().ok()).filter_map(|r| r.ok()).sum();
            let d = t0.elapsed();
            println!("③ {parts} 路并行  {:.1}MB / {:.1}s = {:.2} MB/s", got as f64 / 1048576.0, d.as_secs_f64(), mbps(got, d));
        }
    }

    // ④ 小块串行读(模拟 Windows 重定向器)
    if total > 0 {
        for block in [64u64 * 1024, 1024 * 1024] {
            let rounds = if block < 1024 * 1024 { 64 } else { 16 };
            let t0 = Instant::now();
            let mut got = 0u64;
            for i in 0..rounds {
                let start = i * block;
                let end = start + block - 1;
                match get_bytes(&blob_url, Some(&format!("bytes={start}-{end}")), &token) {
                    Ok((_, n, _)) => got += n,
                    Err(e) => {
                        eprintln!("④ 第 {i} 块失败: {e}");
                        break;
                    }
                }
            }
            let d = t0.elapsed();
            println!(
                "④ 串行 {}KB 块 x{rounds}  {:.1}MB / {:.1}s = {:.2} MB/s  (单块 {:.0}ms)",
                block / 1024,
                got as f64 / 1048576.0,
                d.as_secs_f64(),
                mbps(got, d),
                ms(d) / rounds as f64
            );
        }
    }

    // ⑤ 压缩:同一个文件压与不压各拉一遍。随机数据压不动(验证「不白花 CPU」),
    //    文本/日志压得动(验证「用对端 CPU 换带宽」)。
    let textlog = std::env::var("POLARIS_BENCH_TEXT")
        .unwrap_or_else(|_| "share/_bench/text64.log".into());
    println!();
    for (name, path) in [("随机数据", blob.as_str()), ("日志文本", textlog.as_str())] {
        let url = format!("{}/api/fs/read?path={path}", base());
        for (label, z) in [("不压", false), ("zstd", true)] {
            match get_bytes_enc(&url, None, &token, z) {
                Ok((_, n, d)) => println!(
                    "⑤ {name} · {label}  {:.1}MB / {:.1}s = {:.2} MB/s",
                    n as f64 / 1048576.0,
                    d.as_secs_f64(),
                    mbps(n, d)
                ),
                Err(e) => println!("⑤ {name} · {label} 失败: {e}"),
            }
        }
    }

    // ⑥ 走真盘符:挂上之后用「一块一块顺序读」的方式读文件 —— 这正是资源管理器
    //    复制文件时的形态,也是预读要救的那条路。跳过挂载失败(没有 WebClient 服务等)。
    if std::env::var("POLARIS_BENCH_MOUNT").map(|v| v == "0").unwrap_or(false) {
        println!("\n⑥ 已按 POLARIS_BENCH_MOUNT=0 跳过挂盘测试");
    } else {
        bench_drive(&node, &token);
    }

    polaris_app_lib::collab::tunnel::stop();
    println!("\n完。");
}

/// 挂盘 → 顺序读 → 卸盘。测的是端到端真实体感(含 Windows 重定向器的行为)。
fn bench_drive(node: &str, token: &str) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("bench runtime");
    let mounted = rt.block_on(polaris_app_lib::fsmount::fs_mount(
        "bench-disk".into(),
        "基准盘".into(),
        node.to_string(),
        PORT,
        token.to_string(),
    ));
    let v = match mounted {
        Ok(v) => v,
        Err(e) => {
            println!("\n⑥ 挂盘失败(跳过): {e}");
            return;
        }
    };
    let drive = v["drive"].as_str().unwrap_or("").to_string();
    if drive.is_empty() {
        println!("\n⑥ 没拿到盘符(看门狗仍在补挂),跳过");
        let _ = rt.block_on(polaris_app_lib::fsmount::fs_unmount("bench-disk".into()));
        return;
    }
    println!("\n⑥ 已挂 {drive}");

    // 列目录:资源管理器点开文件夹的那一下。
    let dir = format!("{drive}\\{}", "share\\_bench\\many");
    let t0 = Instant::now();
    match std::fs::read_dir(&dir) {
        Ok(rd) => println!(
            "⑥ 列 200 项目录  {} 项 / {:.0}ms",
            rd.count(),
            ms(t0.elapsed())
        ),
        Err(e) => println!("⑥ 列目录失败: {e}"),
    }

    // 顺序读,1MB 一块 —— 与 ④ 同形态,但这次真的经过盘符(含 Windows 重定向器)。
    // 预读开/关各跑一遍:这条链路的带宽随时在飘,只有紧挨着的两次才有可比性。
    // 注意 Windows WebClient 默认只让读 50MB 以内的文件(超了报 os error 223),
    // 所以这里用小样本,别拿 64MB 那个。
    // **每档换一个文件**:Windows 重定向器会把读过的文件整份缓存在本地,同一个文件
    // 读第二遍就是纯内存,那个数好看但毫无意义。而且计时必须**从 open 开始** ——
    // 重定向器是在 open 那一刻把文件拉过来的,只掐读循环等于什么都没测到。
    let files = ["blob16a.bin", "blob16b.bin", "blob16c.bin"];
    for (i, (label, off)) in [("预读关", true), ("预读开", false), ("预读开(复跑)", false)]
        .into_iter()
        .enumerate()
    {
        if off {
            std::env::set_var("POLARIS_FS_READAHEAD", "0");
        } else {
            std::env::remove_var("POLARIS_FS_READAHEAD");
        }
        let f = format!("{drive}\\share\\_bench\\{}", files[i]);
        let t0 = Instant::now();
        match std::fs::File::open(&f) {
            Ok(mut fh) => {
                let mut buf = vec![0u8; 1024 * 1024];
                let mut got = 0u64;
                loop {
                    match fh.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => got += n as u64,
                        Err(e) => {
                            println!("⑥ {label} 读盘出错: {e}");
                            break;
                        }
                    }
                }
                let d = t0.elapsed();
                println!(
                    "⑥ 盘上顺序读 · {label}  {:.1}MB / {:.1}s = {:.2} MB/s",
                    got as f64 / 1048576.0,
                    d.as_secs_f64(),
                    mbps(got, d)
                );
            }
            Err(e) => println!("⑥ {label} 打开盘上文件失败: {e}"),
        }
    }
    std::env::remove_var("POLARIS_FS_READAHEAD");

    let _ = rt.block_on(polaris_app_lib::fsmount::fs_unmount("bench-disk".into()));
    println!("⑥ 已卸盘");
}
