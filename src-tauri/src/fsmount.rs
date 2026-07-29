//! fsmount.rs —— 把互联对端的共享目录挂成**本机系统盘符**(Tailscale 式「连上即多一块盘」)。
//!
//! 链路:资源管理器 Z: → Windows 自带 WebDAV 客户端(WebClient) → 本模块的 loopback
//! WebDAV 桥(127.0.0.1:随机口/随机密钥路径) → iroh 隧道本地口(127.0.0.1:18620+,带
//! owner Bearer) → 对端 fsface(路径关押,只读)。全程用户态,不装任何驱动。
//!
//! 为什么走 WebDAV 而不是 SMB/驱动:445 端口被系统占死、Dokan/WinFsp 要装内核驱动;
//! WebDAV 客户端 Windows/macOS 自带,`net use Z: http://…` 一条命令即挂,mac 用
//! mount_webdav。代价是只读 + Windows WebClient 默认单文件 50MB 上限(注册表可调,
//! 大文件仍可走「文件中心 · 远程源」在线下载,上限 512MB)。
//!
//! 稳定性(「只要两端应用开着就一直在」):
//!  - iroh 隧道自身带断线重连(10s 巡检 + 1s→30s 退避);
//!  - 本模块看门狗每 15s:幂等重发隧道连接 + 探活对端 + 盘符掉了自动重挂;
//!  - 首挂时对端未就绪(iroh 握手要几秒)不算失败:登记后由看门狗接力,通了自动挂上。
//!
//! 读写:盘是**读写还是只读,由对端说了算** —— 挂载时探一次 `/api/fs/caps`,看门狗每拍
//! 复核(对方在互联页里给某个目录打开写位后,不用重挂盘就生效)。对端说只读时,桥把
//! PUT/DELETE/MKCOL/MOVE 就地 403,不让请求白跑一趟隧道;对端说可写才放行,真正的
//! 逐目录写位仍由对端 fsface 把守 —— 桥只是不做多余的拦,不替对端发放权限。
//!
//! Windows 写盘的隐性要求(踩过才知道):WebDAV 重定向器只对 **DAV class 2**(带
//! LOCK/UNLOCK)的服务端开放写;而资源管理器复制文件时还会发 PROPPATCH 设时间戳,
//! 那一步失败会让整个复制回滚。所以这里实现了「假锁」(不做真互斥:盘的并发写由对端
//! 文件系统兜)与「PROPPATCH 一律回 200」—— 缺任何一个,拖文件进盘都会失败。
//!
//! 安全:桥只绑 127.0.0.1,路径带 128bit 随机密钥(本机其它用户猜不着);
//! 读写权限、路径关押全在对端 fsface 把守。
#![cfg(feature = "collab-host")]

use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 看门狗节拍:隧道探活 + 盘符复核。
const WATCH_TICK: Duration = Duration::from_secs(15);
/// 首挂时等对端就绪的预算(iroh 打洞握手一般 2~6s)。
const FIRST_PROBE_TRIES: u32 = 8;
/// 目录清单短缓存的寿命。资源管理器打开一个目录会连发好几轮 PROPFIND(自身、父级、
/// 每个子项),同一目录在这个窗口内只过一次隧道 —— 经中继时这是「点开文件夹要转圈」
/// 和「秒开」的分界。写操作会立刻清缓存,故看不到自己刚写的东西这种事不会发生。
/// `POLARIS_FS_DIR_TTL_MS` 可调:对端目录被别的机器改动时,最坏要等这么久才看到。
const DIR_CACHE_TTL_MS: u64 = 5_000;

fn dir_cache_ttl() -> Duration {
    Duration::from_millis(
        std::env::var("POLARIS_FS_DIR_TTL_MS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(DIR_CACHE_TTL_MS),
    )
}

// ────────────────────────────── 顺序预读 ──────────────────────────────
// Windows 重定向器读盘是**一块一块串着来**的:发一个 `Range: bytes=X-Y`,等它整块
// 回来,再发下一个。每块之间都空等一个往返 —— 实测这条链路上单流整拉能跑 3.4MB/s,
// 而同样的字节数用 1MB 串行块读只有 1.3MB/s,**六成带宽白白耗在等待上**。
//
// 修法:第一块请求到达时,向对端开一个**开口的** `bytes=X-` 请求,让数据在后台一直流,
// 灌进一个有界队列;下一块请求来了直接从队列取,零往返。用内存换掉往返,这也是这台
// 云机(1.7G 内存常年只用 300M)最该拿出来花的资源。
//
// 内存上限是明确的:`每流队列上限 × 并发流数`,默认 6MB × 4 = 24MB;队列满了泵线程
// 就地阻塞,不会因为没人读而无限吞流量。

/// 单块传输单位。与对端 `stream_file_body` 的块大小一致,省一次重新分片。
const PREFETCH_CHUNK: usize = 256 * 1024;
/// 每条预读流的队列上限(MB),即「最多提前拉多少」。
const PREFETCH_MB: usize = 6;
/// 同时保留几条预读流(多文件并行拷贝时每个文件一条)。
const PREFETCH_STREAMS: usize = 4;
/// 超过这个长度的单次 Range 不走预读:那是「一次要一大坨」的调用方(下载器/播放器),
/// 它自己就能把管子喂满,再垫一层缓冲只是白占内存。
const PREFETCH_MAX_SERVE: u64 = 8 * 1024 * 1024;
/// 预读流闲置这么久就回收(对端那边还挂着一个文件句柄和一条 QUIC 流)。
const PREFETCH_IDLE: Duration = Duration::from_secs(30);

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

/// 一条正在流的顺序预读。`next` = 本流下一个能直接供给的字节偏移。
struct Prefetch {
    rel: String,
    next: u64,
    /// 文件总长(从上游 Content-Range 里解出来),回 206 时要用。
    total: u64,
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    /// 上一块没吃完的残余。
    cur: Vec<u8>,
    cur_pos: usize,
    /// 置位后泵线程收工。接收端被丢弃时 send 也会失败退出,这个是主动关的那条路。
    stop: Arc<AtomicBool>,
    last_used: Instant,
}

impl Prefetch {
    /// 从流里取最多 want 字节。返回空 = 到文件尾或流断了。
    fn take(&mut self, want: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(want.min(PREFETCH_CHUNK * 2));
        while out.len() < want {
            if self.cur_pos >= self.cur.len() {
                // 等下一块。给一个宽限:对端慢没关系,但不许无限期挂着。
                match self.rx.recv_timeout(Duration::from_secs(60)) {
                    Ok(b) if !b.is_empty() => {
                        self.cur = b;
                        self.cur_pos = 0;
                    }
                    _ => break, // EOF / 泵退出 / 超时
                }
            }
            let n = (self.cur.len() - self.cur_pos).min(want - out.len());
            out.extend_from_slice(&self.cur[self.cur_pos..self.cur_pos + n]);
            self.cur_pos += n;
        }
        self.next += out.len() as u64;
        self.last_used = Instant::now();
        out
    }
}

impl Drop for Prefetch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// 解析进来的 `Range: bytes=100-999` → (起点, 要多少字节)。
/// **只认闭区间**:开口的 `bytes=100-`(要到文件尾)和后缀式 `bytes=-500` 都返回 None,
/// 让它们走直通流式 —— 那两种形态本来就是「一次要一大坨」,直通已经是最优解。
fn parse_range_req(h: &str) -> Option<(u64, u64)> {
    let spec = h.trim().strip_prefix("bytes=")?.split(',').next()?.trim();
    let (a, b) = spec.split_once('-')?;
    let start = a.trim().parse::<u64>().ok()?;
    let end = b.trim().parse::<u64>().ok()?;
    (end >= start).then(|| (start, end - start + 1))
}

/// 解析上游的 `Content-Range: bytes 100-999/12345` → (起, 总长)。
fn parse_content_range(v: &str) -> Option<(u64, u64)> {
    let rest = v.trim().strip_prefix("bytes")?.trim();
    let (span, total) = rest.split_once('/')?;
    let start = span.split('-').next()?.trim().parse::<u64>().ok()?;
    let total = total.trim().parse::<u64>().ok()?;
    Some((start, total))
}

/// 同上,但取的是**这一段有多少字节**(`bytes a-b/total` → b-a+1)。
fn parse_content_range_span(v: &str) -> Option<u64> {
    let rest = v.trim().strip_prefix("bytes")?.trim();
    let span = rest.split('/').next()?;
    let (a, b) = span.split_once('-')?;
    let (a, b) = (a.trim().parse::<u64>().ok()?, b.trim().parse::<u64>().ok()?);
    (b >= a).then(|| b - a + 1)
}

// ────────────────────────────── 挂载注册表 ──────────────────────────────

struct Mount {
    source_id: String,
    name: String,
    /// 看门狗幂等重连用;无 collab-net 的 server 壳挂不了隧道,字段闲置属正常。
    #[cfg_attr(not(feature = "collab-net"), allow(dead_code))]
    node_id: String,
    /// iroh 隧道在本机的代理口(fsapi 同款)。
    upstream_port: u16,
    token: String,
    /// WebDAV 桥的路径密钥:`http://127.0.0.1:{dav_port}/{secret}/…`。
    secret: String,
    dav_port: u16,
    /// 已挂的盘符("Z:")或 mac 挂载点路径;空 = 还没挂上(看门狗在追)。
    drive: Mutex<String>,
    alive: Arc<AtomicBool>,
    /// 最近一次对端探活成功的 unix 秒;0 = 从没通过。
    last_ok: AtomicU64,
    last_err: Mutex<String>,
    shutdown: Arc<tokio::sync::Notify>,
    /// 对端至少有一个共享根开了写。false = 这块盘挂成只读。
    writable: AtomicBool,
    /// 桌面快捷方式的完整路径;空 = 没建成/已删。
    shortcut: Mutex<String>,
    /// 目录清单短缓存(见 [`dir_cache_ttl`])。key = 根内相对路径。
    dir_cache: Mutex<HashMap<String, (Instant, Vec<UpEntry>)>>,
    /// 顺序预读流池(见「顺序预读」一节)。取用时整条摘出来,还回去时再插入 ——
    /// 同一条流永远只有一个使用者,不必再为流内状态加锁。
    prefetch: Mutex<Vec<Prefetch>>,
}

impl Mount {
    fn cache_get(&self, rel: &str) -> Option<Vec<UpEntry>> {
        let ttl = dir_cache_ttl();
        let c = self.dir_cache.lock().unwrap();
        c.get(rel).and_then(|(at, v)| (at.elapsed() < ttl).then(|| v.clone()))
    }
    fn cache_put(&self, rel: &str, v: &[UpEntry]) {
        let mut c = self.dir_cache.lock().unwrap();
        // 无界增长防护:目录多到这个量级说明用户在盘里深度浏览,整体清掉重新热身即可。
        if c.len() > 256 {
            c.clear();
        }
        c.insert(rel.to_string(), (Instant::now(), v.to_vec()));
    }
    /// 任何写操作后清空:宁可多过一次隧道,也不能让用户看不见自己刚拖进去的文件。
    /// 预读流一并作废 —— 文件可能刚被改写,手里那半截缓冲已经不算数了。
    fn cache_clear(&self) {
        self.dir_cache.lock().unwrap().clear();
        self.prefetch.lock().unwrap().clear();
    }

    /// 从子项名反查父目录的热缓存。资源管理器打开目录后会**逐个子项**再发一次
    /// PROPFIND,而这些属性刚在父目录那一趟里全拿回来过 —— 200 个文件就是 200 次
    /// 白跑的往返(这条链路上一次 44ms,合计近 9 秒)。
    fn cached_child(&self, rel: &str) -> Option<UpEntry> {
        let (parent, name) = match rel.rfind('/') {
            Some(i) => (&rel[..i], &rel[i + 1..]),
            None => ("", rel),
        };
        self.cache_get(parent)?.into_iter().find(|e| e.name == name)
    }

    /// 摘一条「下一个字节正好是 start」的预读流。摘不到 = 随机读/新文件,调用方新开一条。
    /// 顺手回收闲置太久的流(对端那头挂着文件句柄)。
    fn prefetch_take(&self, rel: &str, start: u64) -> Option<Prefetch> {
        let mut pool = self.prefetch.lock().unwrap();
        pool.retain(|p| p.last_used.elapsed() < PREFETCH_IDLE);
        let i = pool
            .iter()
            .position(|p| p.rel == rel && p.next == start)?;
        Some(pool.remove(i))
    }

    /// 还回预读流。池满就丢掉最久没用的那条(Drop 里会通知泵线程收工)。
    fn prefetch_put(&self, p: Prefetch) {
        let cap = env_usize("POLARIS_FS_READAHEAD_STREAMS", PREFETCH_STREAMS);
        let mut pool = self.prefetch.lock().unwrap();
        while pool.len() >= cap {
            let oldest = pool
                .iter()
                .enumerate()
                .min_by_key(|(_, p)| p.last_used)
                .map(|(i, _)| i);
            match oldest {
                Some(i) => drop(pool.remove(i)),
                None => break,
            }
        }
        pool.push(p);
    }
}

fn mounts() -> &'static Mutex<HashMap<String, Arc<Mount>>> {
    static M: OnceLock<Mutex<HashMap<String, Arc<Mount>>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// fsmount 专用小 tokio 运行时(DAV 桥 + 看门狗)。不借 tauri::async_runtime:
/// server 壳没有 tauri,且桥的生命周期独立于窗口。
fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("fsmount-dav")
            .enable_all()
            .build()
            .expect("fsmount runtime")
    })
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn view(m: &Mount) -> Value {
    let ok_at = m.last_ok.load(Ordering::Relaxed);
    json!({
        "sourceId": m.source_id,
        "name": m.name,
        "drive": m.drive.lock().unwrap().clone(),
        "davPort": m.dav_port,
        "ok": ok_at > 0 && now_secs().saturating_sub(ok_at) < 45,
        "lastOkAt": ok_at,
        "error": m.last_err.lock().unwrap().clone(),
        "writable": m.writable.load(Ordering::Relaxed),
        "shortcut": m.shortcut.lock().unwrap().clone(),
    })
}

// ────────────────────────────── 对端调用(经隧道) ──────────────────────────────

#[derive(Deserialize, Clone)]
struct UpEntry {
    name: String,
    is_dir: bool,
    size: u64,
    mtime: u64,
}

fn agent_short() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(4))
        .timeout_read(Duration::from_secs(20))
        .build()
}

/// 读文件用:不设总超时,只设空闲读超时 —— 大文件经中继可以慢,但不许卡死。
fn agent_read() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(4))
        .timeout_read(Duration::from_secs(60))
        .build()
}

fn up_url(m: &Mount, api: &str, rel: &str) -> String {
    format!(
        "http://127.0.0.1:{}{}?path={}",
        m.upstream_port,
        api,
        enc_comp(rel)
    )
}

fn up_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let msg = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
                .unwrap_or(body);
            format!("HTTP {code}:{}", msg.chars().take(200).collect::<String>())
        }
        ureq::Error::Transport(t) => format!("隧道不通:{t}"),
    }
}

/// 列对端目录。rel "" = 根(多共享目录时对端合成虚拟顶层)。
fn up_list(m: &Mount, rel: &str) -> Result<Vec<UpEntry>, String> {
    let resp = agent_short()
        .get(&up_url(m, "/api/fs/list", rel))
        .set("Authorization", &format!("Bearer {}", m.token))
        .call()
        .map_err(up_err)?;
    let v: Value = resp.into_json().map_err(|e| format!("响应非 JSON:{e}"))?;
    serde_json::from_value(v.get("entries").cloned().unwrap_or(json!([])))
        .map_err(|e| format!("entries 解析失败:{e}"))
}

/// 带短缓存的列目录(PROPFIND 热路径专用)。写操作会清缓存,见 [`Mount::cache_clear`]。
fn up_list_cached(m: &Mount, rel: &str) -> Result<Vec<UpEntry>, String> {
    if let Some(v) = m.cache_get(rel) {
        return Ok(v);
    }
    let v = up_list(m, rel)?;
    m.cache_put(rel, &v);
    Ok(v)
}

/// 单条属性。老版本对端没有这个端点(404/405)→ 回落「列父目录里找」。
fn up_stat(m: &Mount, rel: &str) -> Result<UpEntry, String> {
    let resp = agent_short()
        .get(&up_url(m, "/api/fs/stat", rel))
        .set("Authorization", &format!("Bearer {}", m.token))
        .call()
        .map_err(up_err)?;
    let v: Value = resp.into_json().map_err(|e| format!("响应非 JSON:{e}"))?;
    serde_json::from_value(v.get("entry").cloned().unwrap_or(json!(null)))
        .map_err(|e| format!("entry 解析失败:{e}"))
}

/// 对端开放的能力。老版本对端没有这个端点 → 当只读(它本来也没有写端点)。
fn up_caps(m: &Mount) -> (bool, bool) {
    let out = agent_short()
        .get(&format!("http://127.0.0.1:{}/api/fs/caps", m.upstream_port))
        .set("Authorization", &format!("Bearer {}", m.token))
        .call();
    match out {
        Ok(resp) => {
            let v: Value = resp.into_json().unwrap_or(json!({}));
            (
                v.get("read").and_then(|x| x.as_bool()).unwrap_or(true),
                v.get("write").and_then(|x| x.as_bool()).unwrap_or(false),
            )
        }
        Err(_) => (true, false),
    }
}

/// 触发对端的一个文件操作(mkdir/delete/move/copy)。
fn up_op(m: &Mount, op: &str, rel: &str, dest: Option<&str>) -> Result<(), String> {
    let mut url = format!(
        "http://127.0.0.1:{}/api/fs/op/{op}?path={}",
        m.upstream_port,
        enc_comp(rel)
    );
    if let Some(d) = dest {
        url.push_str(&format!("&dest={}", enc_comp(d)));
    }
    agent_short()
        .post(&url)
        .set("Authorization", &format!("Bearer {}", m.token))
        .set("Content-Length", "0")
        .call()
        .map_err(up_err)?;
    m.cache_clear();
    Ok(())
}

/// 把一个同步 Read 流 PUT 给对端。`len` 已知时带 Content-Length(省掉分块编码)。
fn up_write(
    m: &Mount,
    rel: &str,
    body: impl std::io::Read + Send + 'static,
    len: Option<u64>,
) -> Result<(), String> {
    // 上传不设总超时:大文件经中继可以慢。空闲读超时由 agent_read 兜(不许卡死)。
    let mut req = agent_read()
        .put(&up_url(m, "/api/fs/write", rel))
        .set("Authorization", &format!("Bearer {}", m.token));
    if let Some(n) = len {
        req = req.set("Content-Length", &n.to_string());
    }
    req.send(body).map_err(up_err)?;
    m.cache_clear();
    Ok(())
}

/// async 请求体 → 同步 `Read`(喂给 ureq)。桥内存恒定:只握着在飞的那几块。
struct ChanReader {
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}

impl std::io::Read for ChanReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        // 空块要继续等下一块,不能当 EOF(否则上传会莫名截断)。
        while self.pos >= self.buf.len() {
            match self.rx.recv() {
                Ok(b) => {
                    self.buf = b;
                    self.pos = 0;
                }
                Err(_) => return Ok(0), // 发送端已关 = 真 EOF
            }
        }
        let n = (self.buf.len() - self.pos).min(out.len());
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// 发起对端文件读(可带 Range),返回原始 ureq 响应供流式透传。
/// 对端新版流式无大小上限;老版本对端忽略 Range 回整文件,同样兼容。
///
/// `accept_zstd` = 允许对端压缩后再传。这条链路的带宽只有 3MB/s 出头,而两头的 CPU
/// 都闲着 —— 文本/代码/日志压掉三五倍等于把管子撑粗几倍。对端不支持(老版本)时
/// 它只会当没看见这个头,照常回原文,故无需协商。
fn up_read_stream(
    m: &Mount,
    rel: &str,
    range: Option<&str>,
    accept_zstd: bool,
) -> Result<ureq::Response, String> {
    let mut req = agent_read()
        .get(&up_url(m, "/api/fs/read", rel))
        .set("Authorization", &format!("Bearer {}", m.token));
    if let Some(r) = range {
        req = req.set("Range", r);
    }
    if accept_zstd && compress_enabled() {
        // 私有标记头(不是 Accept-Encoding):对端只对**懂这套区间记账的自己人**开压缩,
        // 见对端 fs_read_api 里的说明。
        req = req.set("X-Polaris-Zstd", "1");
    }
    req.call().map_err(up_err)
}

/// 传输压缩总开关。`POLARIS_FS_COMPRESS=0` 关掉 —— 千兆内网上压缩反而可能成为瓶颈。
fn compress_enabled() -> bool {
    std::env::var("POLARIS_FS_COMPRESS").map(|v| v.trim() != "0").unwrap_or(true)
}

/// 上游读的统一形态:压缩与否对调用方透明。
struct UpStream {
    status: u16,
    /// **未压缩语义**的区间头,原样转给重定向器。
    content_range: Option<String>,
    last_modified: Option<String>,
    /// 解码后的真实字节数;None = 未知(那就不发 Content-Length,走分块)。
    len: Option<u64>,
    reader: Box<dyn std::io::Read + Send + 'static>,
}

/// 发起上游读并按需接上解压器。压缩流的 Content-Length 是压缩后的长度,对下游毫无意义,
/// 这里换算成**未压缩**的真实长度(区间头里有起止/总长,算得出来),这样重定向器拿到的
/// Content-Length 依旧准确 —— 少了它资源管理器的复制进度条会瞎跳。
fn up_read_decoded(
    m: &Mount,
    rel: &str,
    range: Option<&str>,
    accept_zstd: bool,
) -> Result<UpStream, String> {
    let resp = up_read_stream(m, rel, range, accept_zstd)?;
    let status = resp.status();
    let content_range = resp.header("Content-Range").map(String::from);
    let last_modified = resp.header("Last-Modified").map(String::from);
    let zstd_on = resp
        .header("Content-Encoding")
        .map(|v| v.to_ascii_lowercase().contains("zstd"))
        .unwrap_or(false);
    let plain_len = resp
        .header("Content-Length")
        .and_then(|v| v.trim().parse::<u64>().ok());
    let len = if zstd_on {
        // 压缩流:长度只能从区间头推。`bytes a-b/total` → b-a+1。
        content_range
            .as_deref()
            .and_then(parse_content_range_span)
    } else {
        plain_len
    };
    let raw = resp.into_reader();
    let reader: Box<dyn std::io::Read + Send + 'static> = if zstd_on {
        Box::new(zstd::stream::Decoder::new(raw).map_err(|e| format!("解压器创建失败:{e}"))?)
    } else {
        Box::new(raw)
    };
    Ok(UpStream { status, content_range, last_modified, len, reader })
}

/// 新开一条预读流:向对端发**开口的** `bytes=start-`,后台泵线程把 body 灌进有界队列。
///
/// 两处不肯将就:
///  · `start > 0` 时对端必须回 206 —— 老版本对端会忽略 Range 直接回整文件,那样按
///    偏移记账就全错了(读出来的内容会静默错位)。见到 200 一律拒绝走预读,回落直通。
///  · 队列容量是**内存硬上限**:泵线程 send 阻塞 = 天然背压,没人读就不再往下拉。
fn prefetch_open(m: &Mount, rel: &str, start: u64) -> Result<Prefetch, String> {
    let up = up_read_decoded(m, rel, Some(&format!("bytes={start}-")), true)?;
    let total = match up.content_range.as_deref().and_then(parse_content_range) {
        Some((got_start, total)) => {
            if got_start != start {
                return Err(format!("对端回的区间起点 {got_start} 与请求 {start} 不符"));
            }
            total
        }
        None => {
            if start > 0 || up.status != 200 {
                return Err("对端不支持 Range(缺 Content-Range),不走预读".into());
            }
            // 整文件 200:总长看长度头,没有就当未知(0 = 回 200 而非 206)。
            up.len.unwrap_or(0)
        }
    };

    let mb = env_usize("POLARIS_FS_READAHEAD_MB", PREFETCH_MB);
    let cap = (mb * 1024 * 1024 / PREFETCH_CHUNK).max(2);
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(cap);
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    std::thread::Builder::new()
        .name("polaris-fs-prefetch".into())
        .spawn(move || {
            let mut reader = up.reader;
            let mut buf = vec![0u8; PREFETCH_CHUNK];
            loop {
                if stop2.load(Ordering::Relaxed) {
                    break;
                }
                match reader.read(&mut buf) {
                    Ok(0) => break, // 读完了
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break; // 使用者已走
                        }
                    }
                    Err(_) => break, // 链路断了:使用者会在 recv 拿到 EOF,按短读处理
                }
            }
        })
        .map_err(|e| format!("预读线程启动失败:{e}"))?;

    Ok(Prefetch {
        rel: rel.to_string(),
        next: start,
        total,
        rx,
        cur: Vec::new(),
        cur_pos: 0,
        stop,
        last_used: Instant::now(),
    })
}

/// 用预读流供一段 Range。回 `Ok(None)` = 这次不适合走预读(调用方回落直通),
/// 回 `Ok(Some((数据, 总长)))` = 数据已就位。
fn prefetch_serve(
    m: &Mount,
    rel: &str,
    start: u64,
    want: u64,
) -> Result<Option<(Vec<u8>, u64)>, String> {
    // `POLARIS_FS_READAHEAD=0` 整个关掉:出问题时能一键回到「每块各跑一趟」的老行为,
    // 也用来做同条件 A/B(这条链路的带宽随时在飘,不同时段互相比是不作数的)。
    if want == 0
        || want > PREFETCH_MAX_SERVE
        || std::env::var("POLARIS_FS_READAHEAD").map(|v| v.trim() == "0").unwrap_or(false)
    {
        return Ok(None);
    }
    let mut p = match m.prefetch_take(rel, start) {
        Some(p) => p,
        None => match prefetch_open(m, rel, start) {
            Ok(p) => p,
            // 对端不支持 Range 之类:不是错误,回落直通即可。
            Err(_) => return Ok(None),
        },
    };
    let data = p.take(want as usize);
    let total = p.total;
    if data.is_empty() {
        // 一个字节都没拿到:多半是流断了。别把这条坏流放回池里。
        return Ok(None);
    }
    m.prefetch_put(p);
    Ok(Some((data, total)))
}

/// 同步 Read → 流式 Body:spawn_blocking 分块泵 → mpsc → Stream。
/// 桥内存恒定(256KB × 通道容量),文件多大都不再整读。
fn stream_reader_body(mut reader: impl std::io::Read + Send + 'static) -> Body {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(4);
    tokio::task::spawn_blocking(move || {
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx
                        .blocking_send(Ok(axum::body::Bytes::copy_from_slice(&buf[..n])))
                        .is_err()
                    {
                        break; // 下游(WebClient/播放器)断开
                    }
                }
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    break;
                }
            }
        }
    });
    Body::from_stream(futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    }))
}

// ────────────────────────────── 编码小工具 ──────────────────────────────

/// URL 组件编码(连 `/` 也编,query 参数与单段路径共用)。
fn enc_comp(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// href 用:按 `/` 分段编码再拼回。
fn enc_path(rel: &str) -> String {
    rel.split('/')
        .filter(|s| !s.is_empty())
        .map(enc_comp)
        .collect::<Vec<_>>()
        .join("/")
}

/// 百分号解码(路径语境:不把 `+` 当空格)。非法序列原样保留。
fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if let (Some(h), Some(l)) = (
                (b.get(i + 1).and_then(|c| (*c as char).to_digit(16))),
                (b.get(i + 2).and_then(|c| (*c as char).to_digit(16))),
            ) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// unix 秒 → (y, m, d, hh, mm, ss, weekday 0=Sun)。Howard Hinnant civil-from-days。
fn civil(secs: u64) -> (i64, u32, u32, u32, u32, u32, u32) {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let wd = ((days + 4).rem_euclid(7)) as u32; // 1970-01-01 是周四
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = yoe as i64 + era * 400 + if m <= 2 { 1 } else { 0 };
    (
        y,
        m,
        d,
        (rem / 3600) as u32,
        (rem % 3600 / 60) as u32,
        (rem % 60) as u32,
        wd,
    )
}

/// RFC1123(`Sat, 24 Jul 2026 12:00:00 GMT`)—— WebDAV getlastmodified 要求的格式。
fn http_date(secs: u64) -> String {
    const WD: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MO: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (y, m, d, hh, mm, ss, wd) = civil(secs);
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        WD[wd as usize],
        d,
        MO[(m - 1) as usize],
        y,
        hh,
        mm,
        ss
    )
}

fn iso_date(secs: u64) -> String {
    let (y, m, d, hh, mm, ss, _) = civil(secs);
    format!("{y}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

// ────────────────────────────── WebDAV 桥(axum) ──────────────────────────────

use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Router,
};
use futures_util::StreamExt;

/// OPTIONS 是 Windows 判断「这盘能不能写」的那一问。`DAV: 1, 2` 里的 **2 = 支持锁**,
/// 少了它重定向器直接把盘当只读挂,后面 PUT 根本不会发出来。
fn options_resp(writable: bool) -> Response {
    let allow = if writable {
        "OPTIONS, GET, HEAD, PROPFIND, PROPPATCH, PUT, DELETE, MKCOL, MOVE, COPY, LOCK, UNLOCK"
    } else {
        "OPTIONS, GET, HEAD, PROPFIND"
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("Allow", allow)
        .header("DAV", "1, 2")
        .header("MS-Author-Via", "DAV")
        .body(Body::empty())
        .unwrap()
}

/// 假锁:回一张形式合法的锁票据,不做真互斥。
///
/// 为什么不做真锁:这块盘的并发写最终落在对端的文件系统上,由它裁决;桥自己维护一套
/// 锁表只会在崩溃/断线后留下锁不掉的死锁(WebDAV 的经典坑)。而 Windows 只是要「有人
/// 应这一声」才肯往下走 —— 应了就行。
fn lock_resp(href: &str) -> Response {
    let token = {
        let mut b = [0u8; 16];
        let _ = getrandom::getrandom(&mut b);
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    };
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <D:prop xmlns:D=\"DAV:\"><D:lockdiscovery><D:activelock>\
         <D:locktype><D:write/></D:locktype><D:lockscope><D:exclusive/></D:lockscope>\
         <D:depth>infinity</D:depth><D:timeout>Second-3600</D:timeout>\
         <D:locktoken><D:href>opaquelocktoken:{token}</D:href></D:locktoken>\
         <D:lockroot><D:href>{}</D:href></D:lockroot>\
         </D:activelock></D:lockdiscovery></D:prop>",
        xml_escape(href)
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/xml; charset=utf-8")
        .header("Lock-Token", format!("<opaquelocktoken:{token}>"))
        .body(Body::from(body))
        .unwrap()
}

/// PROPPATCH:一律回「设好了」。资源管理器复制文件的最后一步会来设 Win32 时间戳,
/// 这一步报错会让它把整次复制回滚 —— 而时间戳设不设成,对远程盘的语义无关紧要。
fn proppatch_resp(href: &str) -> Response {
    xml_resp(format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <D:multistatus xmlns:D=\"DAV:\"><D:response><D:href>{}</D:href>\
         <D:propstat><D:prop/><D:status>HTTP/1.1 200 OK</D:status></D:propstat>\
         </D:response></D:multistatus>",
        xml_escape(href)
    ))
}

fn xml_resp(body: String) -> Response {
    Response::builder()
        .status(StatusCode::MULTI_STATUS)
        .header(header::CONTENT_TYPE, "application/xml; charset=utf-8")
        .body(Body::from(body))
        .unwrap()
}

fn plain(status: StatusCode, msg: &str) -> Response {
    (status, msg.to_string()).into_response()
}

/// 单个资源的 `<D:response>` 块。dir 的 href 带尾斜杠(Windows 认这个区分文件夹)。
fn prop_block(
    href_prefix: &str,
    rel: &str,
    display: &str,
    is_dir: bool,
    size: u64,
    mtime: u64,
) -> String {
    let enc = enc_path(rel);
    let href = if enc.is_empty() {
        format!("{href_prefix}/")
    } else if is_dir {
        format!("{href_prefix}/{enc}/")
    } else {
        format!("{href_prefix}/{enc}")
    };
    let restype = if is_dir { "<D:collection/>" } else { "" };
    let len = if is_dir {
        String::new()
    } else {
        format!("<D:getcontentlength>{size}</D:getcontentlength>")
    };
    format!(
        "<D:response><D:href>{href}</D:href><D:propstat><D:prop>\
         <D:displayname>{}</D:displayname>\
         <D:resourcetype>{restype}</D:resourcetype>{len}\
         <D:getlastmodified>{}</D:getlastmodified>\
         <D:creationdate>{}</D:creationdate>\
         </D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>",
        xml_escape(display),
        http_date(mtime),
        iso_date(mtime)
    )
}

/// 取单条属性(PROPFIND/HEAD 文件时用)。先问对端的 `/api/fs/stat`(一次调用、常数载荷),
/// 老版本对端没这个端点才回落「列父目录再找」—— 后者在几千项的目录里是纯浪费。
/// 只走「热缓存 + 对端 stat」两步的快查:命中缓存 = 零往返,凉了 = 一次常数载荷调用。
/// 拿不到不代表不存在(可能是老对端没有 stat 端点),调用方自己决定要不要再列目录。
fn stat_fast(m: &Mount, rel: &str) -> Option<UpEntry> {
    // 父目录清单还热着就地取:资源管理器打开目录后逐个子项再问一遍属性,那些值刚
    // 在列目录那一趟里全拿回来过,没必要每个再过一次隧道。
    m.cached_child(rel).or_else(|| up_stat(m, rel).ok())
}

fn stat_one(m: &Mount, rel: &str) -> Result<UpEntry, String> {
    if let Some(e) = stat_fast(m, rel) {
        return Ok(e);
    }
    let (parent, name) = match rel.rfind('/') {
        Some(i) => (&rel[..i], &rel[i + 1..]),
        None => ("", rel),
    };
    let entries = up_list_cached(m, parent)?;
    entries
        .into_iter()
        .find(|e| e.name == name)
        .ok_or_else(|| "不存在".into())
}

/// `Destination: http://127.0.0.1:port/{secret}/a/b` → 根内相对路径 `a/b`。
/// MOVE/COPY 的目标就藏在这个头里。取不出来 = 头缺失或指向别的桥 → None(调用方回 400)。
fn dest_rel(m: &Mount, headers: &axum::http::HeaderMap) -> Option<String> {
    let raw = headers.get("destination")?.to_str().ok()?;
    // 去掉 scheme://host 部分(有些客户端发相对路径,那就原样用)。
    let path = match raw.find("://") {
        Some(i) => {
            let after = &raw[i + 3..];
            let slash = after.find('/')?;
            &after[slash..]
        }
        None => raw,
    };
    let rest = path.strip_prefix(&format!("/{}", m.secret))?;
    let rel = pct_decode(rest.trim_matches('/'));
    if rel.split('/').any(|s| s == "..") {
        return None;
    }
    Some(rel)
}

async fn dav(State(m): State<Arc<Mount>>, req: Request) -> Response {
    let method = req.method().as_str().to_uppercase();
    let writable = m.writable.load(Ordering::Relaxed);
    // OPTIONS 不看路径:WebDAV 重定向器会先探根路径的能力。
    if method == "OPTIONS" {
        return options_resp(writable);
    }
    let raw = req.uri().path().to_string();
    let prefix = format!("/{}", m.secret);
    let rest = match raw.strip_prefix(&prefix) {
        Some(r) => r,
        None => return plain(StatusCode::NOT_FOUND, "not found"),
    };
    let rel = pct_decode(rest.trim_matches('/'));
    if rel.split('/').any(|s| s == "..") {
        return plain(StatusCode::FORBIDDEN, "路径非法");
    }
    // 只读盘上的写方法就地拒:不让请求白跑一趟隧道再被对端拒。
    if !writable
        && matches!(
            method.as_str(),
            "PUT" | "DELETE" | "MKCOL" | "MOVE" | "COPY" | "PROPPATCH" | "LOCK" | "UNLOCK"
        )
    {
        return plain(
            StatusCode::FORBIDDEN,
            "此盘为只读 —— 在对端「互联 · 我共享的盘」里给目录打开写权限后即可写入",
        );
    }
    let depth1 = req
        .headers()
        .get("depth")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim() != "0")
        .unwrap_or(true);
    let mc = m.clone();
    match method.as_str() {
        "PROPFIND" => {
            let out = tokio::task::spawn_blocking(move || propfind_blocking(&mc, &rel, depth1))
                .await
                .unwrap_or_else(|e| Err(format!("内部任务失败:{e}")));
            match out {
                Ok(xml) => xml_resp(xml),
                Err(e) => plain(StatusCode::NOT_FOUND, &e),
            }
        }
        "GET" | "HEAD" => {
            let head_only = method == "HEAD";
            let range = req
                .headers()
                .get(header::RANGE)
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let out =
                tokio::task::spawn_blocking(move || get_blocking(&mc, &rel, head_only, range))
                    .await
                    .unwrap_or_else(|e| Err(format!("内部任务失败:{e}")));
            match out {
                Ok(r) => r,
                Err(e) => plain(StatusCode::NOT_FOUND, &e),
            }
        }
        // ── 写面(对端说可写才走到这里)──
        "PUT" => {
            let len = req
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            // 通道容量 4 块:上传边收边发,桥不缓存整个文件。
            let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(4);
            let reader = ChanReader { rx, buf: Vec::new(), pos: 0 };
            let mc2 = m.clone();
            let rel2 = rel.clone();
            let up = tokio::task::spawn_blocking(move || up_write(&mc2, &rel2, reader, len));
            let mut stream = req.into_body().into_data_stream();
            let mut recv_err: Option<String> = None;
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(b) => {
                        if tx.send(b.to_vec()).is_err() {
                            break; // 上传侧已出错退出
                        }
                    }
                    Err(e) => {
                        recv_err = Some(format!("读请求体中断:{e}"));
                        break;
                    }
                }
            }
            drop(tx);
            match up.await.unwrap_or_else(|e| Err(format!("上传任务失败:{e}"))) {
                Ok(()) if recv_err.is_none() => {
                    // 201 Created 是 WebDAV PUT 的标准成功码(资源管理器认 200/201/204)。
                    plain(StatusCode::CREATED, "")
                }
                Ok(()) => plain(StatusCode::BAD_REQUEST, &recv_err.unwrap_or_default()),
                Err(e) => plain(StatusCode::FORBIDDEN, &e),
            }
        }
        "MKCOL" | "DELETE" => {
            let op = if method == "MKCOL" { "mkdir" } else { "delete" };
            let out = tokio::task::spawn_blocking(move || up_op(&mc, op, &rel, None))
                .await
                .unwrap_or_else(|e| Err(format!("内部任务失败:{e}")));
            match out {
                Ok(()) => plain(StatusCode::NO_CONTENT, ""),
                Err(e) => plain(StatusCode::FORBIDDEN, &e),
            }
        }
        "MOVE" | "COPY" => {
            let Some(dest) = dest_rel(&m, req.headers()) else {
                return plain(StatusCode::BAD_REQUEST, "Destination 头缺失或非法");
            };
            let op = if method == "MOVE" { "move" } else { "copy" };
            let out = tokio::task::spawn_blocking(move || up_op(&mc, op, &rel, Some(&dest)))
                .await
                .unwrap_or_else(|e| Err(format!("内部任务失败:{e}")));
            match out {
                Ok(()) => plain(StatusCode::NO_CONTENT, ""),
                Err(e) => plain(StatusCode::FORBIDDEN, &e),
            }
        }
        // 锁与属性:见 lock_resp / proppatch_resp 的注释 —— Windows 写盘的两个必答题。
        "LOCK" => lock_resp(&raw),
        "UNLOCK" => plain(StatusCode::NO_CONTENT, ""),
        "PROPPATCH" => proppatch_resp(&raw),
        _ => plain(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"),
    }
}

fn multistatus(blocks: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus xmlns:D=\"DAV:\">{blocks}</D:multistatus>"
    )
}

fn propfind_blocking(m: &Mount, rel: &str, depth1: bool) -> Result<String, String> {
    let prefix = format!("/{}", m.secret);
    let mut blocks = String::new();
    // 先判类型:对**文件**的 PROPFIND 就不必先试一次「列目录」再等它失败了
    //(资源管理器每次打开目录都会对每个子项各发一次 PROPFIND,那趟白跑是最贵的一笔)。
    // 走 stat_fast:父目录清单还热着就是零往返,凉了才问对端;老对端没有 stat 端点
    // → 回落成原来的「先列、失败再当文件」两步(这里**不**带 stat_one 那条「列父目录」
    // 的回落,否则目录型 PROPFIND 会白列一次父目录才轮到列自己)。
    if let Some(e) = stat_fast(m, rel).filter(|e| !e.is_dir && !rel.is_empty()) {
        blocks.push_str(&prop_block(&prefix, rel, &e.name, false, e.size, e.mtime));
        return Ok(multistatus(&blocks));
    }
    match up_list_cached(m, rel) {
        Ok(entries) => {
            // 目录:自身 + (depth 1)子项。自身 mtime 用子项最大值兜底(对端不回目录自身戳)。
            let self_mtime = entries.iter().map(|e| e.mtime).max().unwrap_or(0);
            let display = if rel.is_empty() {
                m.name.clone()
            } else {
                rel.rsplit('/').next().unwrap_or(rel).to_string()
            };
            blocks.push_str(&prop_block(&prefix, rel, &display, true, 0, self_mtime));
            if depth1 {
                for e in entries {
                    let child_rel = if rel.is_empty() {
                        e.name.clone()
                    } else {
                        format!("{rel}/{}", e.name)
                    };
                    blocks.push_str(&prop_block(
                        &prefix, &child_rel, &e.name, e.is_dir, e.size, e.mtime,
                    ));
                }
            }
        }
        Err(list_err) => {
            // 不是目录:按文件取属性;连属性也取不到才算真错。
            let e = stat_one(m, rel).map_err(|_| list_err)?;
            blocks.push_str(&prop_block(&prefix, rel, &e.name, e.is_dir, e.size, e.mtime));
        }
    }
    Ok(multistatus(&blocks))
}

fn get_blocking(
    m: &Mount,
    rel: &str,
    head_only: bool,
    range: Option<String>,
) -> Result<Response, String> {
    // 先按文件读;失败再看是不是目录(给浏览器一个极简索引页,顺手当调试口)。
    if head_only {
        let e = match stat_one(m, rel) {
            Ok(e) if !e.is_dir => e,
            _ => {
                // 目录或根:HEAD 只回 200。
                up_list_cached(m, rel)?;
                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                    .body(Body::empty())
                    .unwrap());
            }
        };
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::CONTENT_LENGTH, e.size)
            .header(header::LAST_MODIFIED, http_date(e.mtime))
            .header(header::ACCEPT_RANGES, "bytes")
            .body(Body::empty())
            .unwrap());
    }
    // 顺序读走预读流:重定向器一块一块串行要数据,这里直接从已经在流的缓冲里切给它,
    // 不再每块空等一个往返。不适用的形态(无 Range/超大单块/对端不支持 Range)自动回落。
    if let Some((start, want)) = range.as_deref().and_then(parse_range_req) {
        match prefetch_serve(m, rel, start, want) {
            Ok(Some((data, total))) => {
                let end = start + data.len() as u64 - 1;
                let mut b = Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(header::CONTENT_TYPE, "application/octet-stream")
                    .header(header::ACCEPT_RANGES, "bytes")
                    .header(header::CONTENT_LENGTH, data.len());
                if total > 0 {
                    b = b.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{total}"));
                }
                return Ok(b.body(Body::from(data)).unwrap());
            }
            Ok(None) => {} // 回落直通
            Err(e) => return Err(e),
        }
    }
    match up_read_decoded(m, rel, range.as_deref(), true) {
        Ok(up) => {
            // 状态与区间头原样透传(对端 206/Content-Range → 播放器 seek 直接可用),
            // body 分块泵过去,桥不再把整个文件揣在内存里。压缩已在 up_read_decoded
            // 里解掉,长度也换算成了未压缩值 —— 下游看到的与从前一模一样。
            let status = StatusCode::from_u16(up.status).unwrap_or(StatusCode::OK);
            let mut b = Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .header(header::ACCEPT_RANGES, "bytes");
            if let Some(n) = up.len {
                b = b.header(header::CONTENT_LENGTH, n);
            }
            if let Some(v) = &up.content_range {
                b = b.header(header::CONTENT_RANGE, v.clone());
            }
            if let Some(v) = &up.last_modified {
                b = b.header(header::LAST_MODIFIED, v.clone());
            }
            Ok(b.body(stream_reader_body(up.reader)).unwrap())
        }
        Err(read_err) => {
            let entries = up_list(m, rel).map_err(|_| read_err)?;
            let mut html = String::from("<meta charset=utf-8><ul>");
            for e in entries {
                let slash = if e.is_dir { "/" } else { "" };
                html.push_str(&format!(
                    "<li><a href=\"/{}/{}{slash}\">{}{slash}</a></li>",
                    m.secret,
                    enc_path(&if rel.is_empty() {
                        e.name.clone()
                    } else {
                        format!("{rel}/{}", e.name)
                    }),
                    xml_escape(&e.name)
                ));
            }
            html.push_str("</ul>");
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                .body(Body::from(html))
                .unwrap())
        }
    }
}

// ────────────────────────────── 系统挂载(net use / mount_webdav) ──────────────────────────────

/// 静默跑一条系统命令,回 (成功?, 合并输出)。Windows 下不闪控制台黑窗。
/// 只有 Windows/mac 会挂盘与建桌面入口,Linux 上没有调用方(编进去就是死代码)。
#[cfg(any(windows, target_os = "macos"))]
fn run_hidden(prog: &str, args: &[&str]) -> (bool, String) {
    let mut c = std::process::Command::new(prog);
    c.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    match c.output() {
        Ok(o) => {
            let mut text = String::from_utf8_lossy(&o.stdout).to_string();
            text.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.success(), text.trim().to_string())
        }
        Err(e) => (false, format!("无法执行 {prog}:{e}")),
    }
}

#[cfg(windows)]
fn used_letter_mask() -> u32 {
    unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() }
}

/// 盘符是否仍在系统盘位图里("Z:" → bit 25)。
#[cfg(windows)]
fn letter_present(drive: &str) -> bool {
    drive
        .chars()
        .next()
        .map(|c| used_letter_mask() & (1 << (c.to_ascii_uppercase() as u8 - b'A')) != 0)
        .unwrap_or(false)
}

/// 清掉上一次进程留下的死映射:`net use` 清单里指向 127.0.0.1@ 且端口不是任何在册
/// 桥端口的 WebDAV 盘,一律删。崩溃退出后盘符不再越积越多。
#[cfg(windows)]
fn cleanup_stale_mounts() {
    let (_, listing) = run_hidden("net", &["use"]);
    let live: Vec<String> = mounts()
        .lock()
        .unwrap()
        .values()
        .map(|m| format!("@{}", m.dav_port))
        .collect();
    for line in listing.lines() {
        if !line.contains("\\\\127.0.0.1@") {
            continue;
        }
        let toks: Vec<&str> = line.split_whitespace().collect();
        let letter = toks
            .iter()
            .find(|t| t.len() == 2 && t.ends_with(':') && t.chars().next().unwrap().is_ascii_alphabetic());
        let remote_dead = toks
            .iter()
            .any(|t| t.contains("\\\\127.0.0.1@") && !live.iter().any(|p| t.contains(p.as_str())));
        if let (Some(l), true) = (letter, remote_dead) {
            let _ = run_hidden("net", &["use", l, "/delete", "/y"]);
        }
    }
}

/// 挑空闲盘符从 Z 往前(Z、Y、X…E),挂 WebDAV。preferred = 掉线重挂时优先原盘符。
#[cfg(windows)]
fn mount_system(m: &Mount, preferred: Option<char>) -> Result<String, String> {
    cleanup_stale_mounts();
    let url = format!("http://127.0.0.1:{}/{}", m.dav_port, m.secret);
    let mask = used_letter_mask();
    let mut cands: Vec<char> = Vec::new();
    if let Some(p) = preferred {
        if mask & (1 << (p as u8 - b'A')) == 0 {
            cands.push(p);
        }
    }
    for c in ('E'..='Z').rev() {
        if mask & (1 << (c as u8 - b'A')) == 0 && !cands.contains(&c) {
            cands.push(c);
        }
    }
    if cands.is_empty() {
        return Err("E: 到 Z: 没有空闲盘符".into());
    }
    let mut last = String::new();
    for c in cands.into_iter().take(4) {
        let drive = format!("{c}:");
        let (ok, out) = run_hidden("net", &["use", &drive, &url, "/persistent:no"]);
        if ok {
            return Ok(drive);
        }
        last = out;
    }
    Err(format!(
        "net use 挂载失败:{}(若反复失败,检查 Windows「WebClient」服务是否被禁用)",
        last.chars().take(300).collect::<String>()
    ))
}

#[cfg(target_os = "macos")]
fn mount_system(m: &Mount, _preferred: Option<char>) -> Result<String, String> {
    let safe: String = m
        .name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let mp = directories::UserDirs::new()
        .ok_or("找不到用户目录")?
        .home_dir()
        .join("Polaris")
        .join("远程盘")
        .join(if safe.is_empty() { "remote" } else { &safe });
    std::fs::create_dir_all(&mp).map_err(|e| format!("建挂载点失败:{e}"))?;
    let url = format!("http://127.0.0.1:{}/{}", m.dav_port, m.secret);
    let mps = mp.to_string_lossy().to_string();
    let (ok, out) = run_hidden("/sbin/mount_webdav", &["-s", &url, &mps]);
    if ok {
        Ok(mps)
    } else {
        Err(format!("mount_webdav 失败:{out}"))
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn mount_system(_m: &Mount, _preferred: Option<char>) -> Result<String, String> {
    Err("此平台暂不支持自动挂盘,请在「文件中心 · 远程源」浏览".into())
}

fn unmount_system(drive: &str) {
    if drive.is_empty() {
        return;
    }
    #[cfg(windows)]
    let _ = run_hidden("net", &["use", drive, "/delete", "/y"]);
    #[cfg(target_os = "macos")]
    let _ = run_hidden("/sbin/umount", &[drive]);
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = drive;
}

/// 盘符还挂着吗(看门狗复核用)。mac 上挂载点存在即视为在。
fn drive_alive(drive: &str) -> bool {
    if drive.is_empty() {
        return false;
    }
    #[cfg(windows)]
    {
        letter_present(drive)
    }
    #[cfg(target_os = "macos")]
    {
        std::path::Path::new(drive).join(".").exists()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        false
    }
}

// ────────────────────────────── 桌面入口 ──────────────────────────────
// 光有盘符不够显眼 —— 「连上就在桌面多一块盘」这件事,用户看的是桌面上那个图标。
// Windows 建 .lnk(WScript.Shell,不需要 UAC),mac 建软链。断开即删,不留死图标。

/// 只有会真建入口的平台用得上(Linux 分支不建桌面入口,编进去会是死代码)。
#[cfg(any(windows, target_os = "macos"))]
fn desktop_dir() -> Option<std::path::PathBuf> {
    directories::UserDirs::new().and_then(|u| u.desktop_dir().map(|p| p.to_path_buf()))
}

/// 文件名里不合法的字符换成 `-`(共享名是用户自己起的,可能带 `:` `/`)。
#[cfg(any(windows, target_os = "macos"))]
fn safe_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) { '-' } else { c })
        .collect();
    let t = s.trim();
    if t.is_empty() {
        "远程盘".into()
    } else {
        t.to_string()
    }
}

#[cfg(windows)]
fn make_shortcut(name: &str, target: &str) -> Result<String, String> {
    let dir = desktop_dir().ok_or("找不到桌面目录")?;
    let path = dir.join(format!("{} (远程盘).lnk", safe_name(name)));
    // PowerShell 单引号字符串里转义单引号 = 写两个。路径来自系统与用户命名,不进 shell 解析。
    let ps_path = path.to_string_lossy().replace('\'', "''");
    let ps_target = target.replace('\'', "''");
    let script = format!(
        "$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{ps_path}');\
         $s.TargetPath='{ps_target}';$s.Description='Polaris 互联 · 远程盘';$s.Save()"
    );
    let (ok, out) = run_hidden(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", &script],
    );
    if ok && path.exists() {
        Ok(path.to_string_lossy().to_string())
    } else {
        Err(format!("建桌面快捷方式失败:{out}"))
    }
}

#[cfg(target_os = "macos")]
fn make_shortcut(name: &str, target: &str) -> Result<String, String> {
    let dir = desktop_dir().ok_or("找不到桌面目录")?;
    let path = dir.join(format!("{} (远程盘)", safe_name(name)));
    let _ = std::fs::remove_file(&path);
    std::os::unix::fs::symlink(target, &path).map_err(|e| format!("建桌面软链失败:{e}"))?;
    Ok(path.to_string_lossy().to_string())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn make_shortcut(_name: &str, _target: &str) -> Result<String, String> {
    Err("此平台不建桌面入口".into())
}

fn drop_shortcut(p: &str) {
    if !p.is_empty() {
        let _ = std::fs::remove_file(p);
    }
}

/// 挂上之后补桌面入口。幂等(已有且还在就不动);失败只打日志 —— 盘本身照样能用,
/// 不该因为桌面上少个图标就让整次挂载算失败。
fn ensure_shortcut(m: &Mount, drive: &str) {
    let cur = m.shortcut.lock().unwrap().clone();
    if !cur.is_empty() && std::path::Path::new(&cur).exists() {
        return;
    }
    match make_shortcut(&m.name, drive) {
        Ok(p) => {
            eprintln!("[fsmount] 桌面入口:{p} → {drive}");
            *m.shortcut.lock().unwrap() = p;
        }
        Err(e) => eprintln!("[fsmount] 桌面入口没建成({e}),盘符仍可用"),
    }
}

// ────────────────────────────── 看门狗 ──────────────────────────────

/// 每 15s:幂等重发 iroh 隧道连接 → 探活对端 → 盘符掉了重挂。
/// 首挂没通(对端未就绪)也由这里接力 —— 一旦通了自动挂上,用户全程无感。
fn spawn_watchdog(m: Arc<Mount>) {
    rt().spawn(async move {
        loop {
            tokio::time::sleep(WATCH_TICK).await;
            if !m.alive.load(Ordering::SeqCst) {
                break;
            }
            // 1) 隧道保活:connect_client 幂等(在跑 = no-op),断了立即重建。
            #[cfg(feature = "collab-net")]
            {
                let node = m.node_id.clone();
                let port = m.upstream_port;
                let _ = tokio::task::spawn_blocking(move || {
                    crate::collab::tunnel::connect_client(&node, port)
                })
                .await;
            }
            // 2) 对端探活。
            let mc = m.clone();
            let probe = tokio::task::spawn_blocking(move || up_list(&mc, ""))
                .await
                .unwrap_or_else(|e| Err(format!("探活任务失败:{e}")));
            match probe {
                Ok(_) => {
                    m.last_ok.store(now_secs(), Ordering::Relaxed);
                    m.last_err.lock().unwrap().clear();
                    // 3) 写权限复核:对方在互联页里给目录开/关了写位,这里跟着变 ——
                    //    用户不必卸盘重挂(重挂会换盘符,正在用的窗口全断)。
                    let mc = m.clone();
                    if let Ok((_, w)) = tokio::task::spawn_blocking(move || up_caps(&mc)).await {
                        let was = m.writable.swap(w, Ordering::Relaxed);
                        if was != w {
                            eprintln!(
                                "[fsmount] 「{}」写权限变为{}",
                                m.name,
                                if w { "可写" } else { "只读" }
                            );
                        }
                    }
                    // 4) 盘符复核:没挂上/被系统掉了 → 重挂(优先原盘符)。
                    let cur = m.drive.lock().unwrap().clone();
                    if !drive_alive(&cur) {
                        let preferred = cur.chars().next().filter(|c| c.is_ascii_alphabetic());
                        let mc = m.clone();
                        let mounted =
                            tokio::task::spawn_blocking(move || mount_system(&mc, preferred))
                                .await
                                .unwrap_or_else(|e| Err(format!("挂载任务失败:{e}")));
                        match mounted {
                            Ok(d) => {
                                eprintln!("[fsmount] 「{}」已挂载为 {d}", m.name);
                                let mc = m.clone();
                                let d2 = d.clone();
                                let _ = tokio::task::spawn_blocking(move || {
                                    ensure_shortcut(&mc, &d2)
                                })
                                .await;
                                *m.drive.lock().unwrap() = d;
                            }
                            Err(e) => *m.last_err.lock().unwrap() = e,
                        }
                    }
                }
                Err(e) => {
                    *m.last_err.lock().unwrap() = e;
                }
            }
        }
    });
}

// ────────────────────────────── 命令(tauri/apihub 壳) ──────────────────────────────

/// 把一台已连隧道的远程源挂成本机盘符。幂等:同 sourceId 重复调 = 返回现状。
/// 对端未就绪不算失败:登记 + 起桥 + 看门狗接力,通了自动挂上。
/// async 命令:首挂要等 iroh 握手(秒级),不能拿 thread::sleep 钉死 tokio worker。
#[cfg_attr(feature = "desktop", tauri::command)]
#[allow(non_snake_case)]
pub async fn fs_mount(
    sourceId: String,
    name: String,
    nodeId: String,
    upstreamPort: u16,
    token: String,
) -> Result<Value, String> {
    // 已在册:确保盘还挂着,返回现状(看门狗自己会补挂)。
    if let Some(m) = mounts().lock().unwrap().get(&sourceId).cloned() {
        return Ok(view(&m));
    }
    // 起 loopback WebDAV 桥(随机口 + 随机路径密钥)。std 同步 bind 拿端口,
    // 进 rt() 任务里再转 tokio(from_std 须在 reactor 上下文内)。
    let mut sec = [0u8; 16];
    getrandom::getrandom(&mut sec).map_err(|e| format!("取随机数失败:{e}"))?;
    let secret: String = sec.iter().map(|b| format!("{b:02x}")).collect();
    let std_listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| format!("绑定 loopback 失败:{e}"))?;
    std_listener
        .set_nonblocking(true)
        .map_err(|e| format!("设非阻塞失败:{e}"))?;
    let dav_port = std_listener
        .local_addr()
        .map_err(|e| format!("取端口失败:{e}"))?
        .port();
    let m = Arc::new(Mount {
        source_id: sourceId.clone(),
        name: if name.trim().is_empty() {
            "远程盘".into()
        } else {
            name.trim().to_string()
        },
        node_id: nodeId,
        upstream_port: upstreamPort,
        token,
        secret,
        dav_port,
        drive: Mutex::new(String::new()),
        alive: Arc::new(AtomicBool::new(true)),
        last_ok: AtomicU64::new(0),
        last_err: Mutex::new(String::new()),
        shutdown: Arc::new(tokio::sync::Notify::new()),
        // 先按只读起(还没问过对端)。首挂探到 caps 前 Windows 就已经发 OPTIONS 了,
        // 所以下面探完 caps 才真正 net use —— 顺序颠倒会把可写盘挂成只读。
        writable: AtomicBool::new(false),
        shortcut: Mutex::new(String::new()),
        dir_cache: Mutex::new(HashMap::new()),
        prefetch: Mutex::new(Vec::new()),
    });
    let router: Router = Router::new().fallback(dav).with_state(m.clone());
    let shut = m.shutdown.clone();
    rt().spawn(async move {
        let listener = match tokio::net::TcpListener::from_std(std_listener) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[fsmount] 桥监听器转 tokio 失败:{e}");
                return;
            }
        };
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { shut.notified().await })
            .await;
    });
    mounts().lock().unwrap().insert(sourceId, m.clone());

    // 首挂:等对端就绪(iroh 握手要几秒),等到就立刻挂;等不到交给看门狗接力。
    // 探活/挂载都是阻塞活,全挪 spawn_blocking,别钉住调用方的 tokio worker。
    let mut ready = false;
    for i in 0..FIRST_PROBE_TRIES {
        let mc = m.clone();
        let ok = tokio::task::spawn_blocking(move || up_list(&mc, "").is_ok())
            .await
            .unwrap_or(false);
        if ok {
            ready = true;
            break;
        }
        if i + 1 < FIRST_PROBE_TRIES {
            tokio::time::sleep(Duration::from_millis(900)).await;
        }
    }
    if ready {
        m.last_ok.store(now_secs(), Ordering::Relaxed);
        // **先**问清对端开不开放写,再 net use。Windows 挂载的第一件事是 OPTIONS 探能力,
        // 那时 writable 必须已经是终值 —— 晚一步,可写的盘就被系统当成只读挂上去了。
        let mc = m.clone();
        if let Ok((_, w)) = tokio::task::spawn_blocking(move || up_caps(&mc)).await {
            m.writable.store(w, Ordering::Relaxed);
        }
        let mc = m.clone();
        let mounted = tokio::task::spawn_blocking(move || mount_system(&mc, None))
            .await
            .unwrap_or_else(|e| Err(format!("挂载任务失败:{e}")));
        match mounted {
            Ok(d) => {
                eprintln!(
                    "[fsmount] 「{}」已挂载为 {d}({})",
                    m.name,
                    if m.writable.load(Ordering::Relaxed) {
                        "可读写"
                    } else {
                        "只读"
                    }
                );
                let mc = m.clone();
                let d2 = d.clone();
                let _ = tokio::task::spawn_blocking(move || ensure_shortcut(&mc, &d2)).await;
                *m.drive.lock().unwrap() = d;
            }
            Err(e) => *m.last_err.lock().unwrap() = e,
        }
    } else {
        *m.last_err.lock().unwrap() = "对端还没就绪,已交给后台自动重试(通了自动挂上)".into();
    }
    spawn_watchdog(m.clone());
    Ok(view(&m))
}

/// 卸载一块远程盘(拆桥 + 删盘符)。幂等。
#[cfg_attr(feature = "desktop", tauri::command)]
#[allow(non_snake_case)]
pub async fn fs_unmount(sourceId: String) -> Result<(), String> {
    let m = mounts().lock().unwrap().remove(&sourceId);
    if let Some(m) = m {
        m.alive.store(false, Ordering::SeqCst);
        m.shutdown.notify_waiters();
        let drive = m.drive.lock().unwrap().clone();
        let sc = m.shortcut.lock().unwrap().clone();
        let _ = tokio::task::spawn_blocking(move || {
            unmount_system(&drive);
            drop_shortcut(&sc); // 断开就把桌面图标收走,不留一个点开报错的死图标
        })
        .await;
    }
    Ok(())
}

/// 全部挂载的现状(前端徽标轮询用)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fs_mount_status() -> Vec<Value> {
    let list: Vec<Arc<Mount>> = mounts().lock().unwrap().values().cloned().collect();
    let mut out: Vec<Value> = list.iter().map(|m| view(m)).collect();
    out.sort_by(|a, b| {
        a["sourceId"]
            .as_str()
            .unwrap_or("")
            .cmp(b["sourceId"].as_str().unwrap_or(""))
    });
    out
}

// ────────────────────────────── 大文件下载(流写磁盘 + 断点续传) ──────────────────────────────
// 文件中心「下载」的桌面路径:浏览器 blob 会把整个文件揣进 webview 内存,大文件必死;
// 这里 Rust 侧直接流写 .part,Range 续传,进度走 tauri 事件。任意大小。

/// 在飞下载的取消旗:key = 目标路径。fs_fetch 每块之间查一次。
#[cfg(feature = "desktop")]
fn cancels() -> &'static Mutex<std::collections::HashSet<String>> {
    static C: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// 从对端把一个文件下载到本机磁盘。断点续传:同目标再次调用从 .part 长度接着拉。
/// expectedSize 给了就先比对,已完整则秒回(重复点下载不重拉)。
#[cfg(feature = "desktop")]
#[tauri::command]
#[allow(non_snake_case)]
pub async fn fs_fetch(
    app: tauri::AppHandle,
    upstreamPort: u16,
    token: String,
    rel: String,
    destPath: String,
    expectedSize: Option<u64>,
) -> Result<Value, String> {
    cancels().lock().unwrap().remove(&destPath);
    let dest = destPath.clone();
    tokio::task::spawn_blocking(move || {
        fetch_blocking(app, upstreamPort, &token, &rel, &dest, expectedSize)
    })
    .await
    .map_err(|e| format!("下载任务失败:{e}"))?
}

/// 取消一个在飞下载(保留 .part 断点,下次接着拉)。幂等。
#[cfg(feature = "desktop")]
#[tauri::command]
#[allow(non_snake_case)]
pub fn fs_fetch_cancel(destPath: String) -> Result<(), String> {
    cancels().lock().unwrap().insert(destPath);
    Ok(())
}

#[cfg(feature = "desktop")]
fn fetch_blocking(
    app: tauri::AppHandle,
    port: u16,
    token: &str,
    rel: &str,
    dest: &str,
    expected: Option<u64>,
) -> Result<Value, String> {
    use std::io::{Read, Write};
    use tauri::Emitter;
    let part = format!("{dest}.part");
    let mut start = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if let Some(total) = expected {
        if start == total && total > 0 {
            // 断点已是完整文件:只差改名。
            finalize_part(&part, dest)?;
            let _ = app.emit("fsfetch:progress", json!({ "dest": dest, "got": total, "total": total, "done": true }));
            return Ok(json!({ "path": dest, "bytes": total, "resumed": true }));
        }
        if start > total {
            // 断点比对端文件还大 = 对端换了内容,推倒重来。
            let _ = std::fs::remove_file(&part);
            start = 0;
        }
    }
    let mut req = agent_read()
        .get(&format!(
            "http://127.0.0.1:{port}/api/fs/read?path={}",
            enc_comp(rel)
        ))
        .set("Authorization", &format!("Bearer {token}"));
    if start > 0 {
        req = req.set("Range", &format!("bytes={start}-"));
    }
    let resp = req.call().map_err(up_err)?;
    // 206 = 对端认了断点,接着写;200 = 整文件(老版本对端/断点无效),推倒重写。
    if resp.status() != 206 {
        start = 0;
    }
    let total: Option<u64> = if resp.status() == 206 {
        resp.header("Content-Range")
            .and_then(|v| v.rsplit('/').next())
            .and_then(|s| s.parse().ok())
    } else {
        resp.header("Content-Length").and_then(|v| v.parse().ok())
    };
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(start == 0)
        .append(start > 0)
        .open(&part)
        .map_err(|e| format!("打开断点文件失败:{e}"))?;
    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; 256 * 1024];
    let mut got = start;
    let mut last_emit = std::time::Instant::now();
    loop {
        if cancels().lock().unwrap().remove(dest) {
            return Err("已取消(断点已保留,重新下载会接着拉)".into());
        }
        let n = reader.read(&mut buf).map_err(|e| format!("读流中断:{e}(断点已保留,可重试续传)"))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| format!("写盘失败:{e}"))?;
        got += n as u64;
        if last_emit.elapsed() >= Duration::from_millis(300) {
            last_emit = std::time::Instant::now();
            let _ = app.emit(
                "fsfetch:progress",
                json!({ "dest": dest, "got": got, "total": total, "done": false }),
            );
        }
    }
    out.flush().map_err(|e| format!("落盘失败:{e}"))?;
    drop(out);
    if let Some(t) = total {
        if got != t {
            return Err(format!(
                "下载不完整({got}/{t} 字节),断点已保留 —— 再点一次下载会从断点续传"
            ));
        }
    }
    finalize_part(&part, dest)?;
    let _ = app.emit(
        "fsfetch:progress",
        json!({ "dest": dest, "got": got, "total": total, "done": true }),
    );
    Ok(json!({ "path": dest, "bytes": got, "resumed": start > 0 }))
}

/// .part → 正式文件(同名旧文件先挪开再覆盖,Windows rename 不跨越已存在目标)。
#[cfg(feature = "desktop")]
fn finalize_part(part: &str, dest: &str) -> Result<(), String> {
    if std::path::Path::new(dest).exists() {
        std::fs::remove_file(dest).map_err(|e| format!("旧文件占位删不掉:{e}"))?;
    }
    std::fs::rename(part, dest).map_err(|e| format!("改名落定失败:{e}"))
}

// ────────────────────────────── WebClient 大文件解锁(Windows) ──────────────────────────────
// 挂载盘符经 Windows 自带 WebDAV 客户端(WebClient),其单文件上限默认 50MB
//(FileSizeLimitInBytes,HKLM)。这里提供「读现状 + 一次 UAC 解锁到 4GB」。
// 4GB 是 WebDAV 重定向器的协议顶格;更大的文件走「文件中心 · 远程源」下载(无上限)。

/// 读当前 WebClient 单文件上限。键不存在 = Windows 默认 50000000 字节。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fs_webdav_limit() -> Result<Value, String> {
    #[cfg(windows)]
    {
        let (ok, out) = run_hidden(
            "reg",
            &[
                "query",
                r"HKLM\SYSTEM\CurrentControlSet\Services\WebClient\Parameters",
                "/v",
                "FileSizeLimitInBytes",
            ],
        );
        let mut limit: u64 = 50_000_000;
        if ok {
            // 行如:FileSizeLimitInBytes    REG_DWORD    0x2faf080
            if let Some(tok) = out.split_whitespace().find(|t| t.starts_with("0x")) {
                if let Ok(v) = u64::from_str_radix(&tok[2..], 16) {
                    limit = v;
                }
            }
        }
        Ok(json!({ "limit": limit, "unlocked": limit >= u32::MAX as u64 }))
    }
    #[cfg(not(windows))]
    {
        Ok(json!({ "limit": 0u64, "unlocked": true })) // 非 Windows 无此限制
    }
}

/// 一次 UAC:把上限改成 4GB 并重启 WebClient 服务。用户在 UAC 点了取消 = 报错返回。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn fs_webdav_unlock() -> Result<Value, String> {
    #[cfg(windows)]
    {
        tokio::task::spawn_blocking(|| {
            let script = "@echo off\r\n\
                reg add \"HKLM\\SYSTEM\\CurrentControlSet\\Services\\WebClient\\Parameters\" /v FileSizeLimitInBytes /t REG_DWORD /d 4294967295 /f\r\n\
                net stop WebClient /y\r\n\
                net start WebClient\r\n";
            let path = std::env::temp_dir().join("polaris-webdav-unlock.cmd");
            std::fs::write(&path, script).map_err(|e| format!("写解锁脚本失败:{e}"))?;
            // -Wait:等提权进程跑完再复核;用户取消 UAC → Start-Process 抛错 → ok=false。
            let ps = format!(
                "Start-Process -FilePath '{}' -Verb RunAs -WindowStyle Hidden -Wait",
                path.display()
            );
            let (ok, out) = run_hidden("powershell", &["-NoProfile", "-Command", &ps]);
            let _ = std::fs::remove_file(&path);
            if !ok {
                return Err(format!("解锁未完成(UAC 被取消或执行失败):{out}"));
            }
            fs_webdav_limit()
        })
        .await
        .map_err(|e| format!("解锁任务失败:{e}"))?
    }
    #[cfg(not(windows))]
    {
        Err("仅 Windows 需要解锁(其它平台无此限制)".into())
    }
}

/// 应用真退出时的清理:删掉所有盘符 + 停桥,别给系统留死映射。
pub fn unmount_all() {
    let all: Vec<Arc<Mount>> = mounts().lock().unwrap().drain().map(|(_, m)| m).collect();
    for m in all {
        m.alive.store(false, Ordering::SeqCst);
        m.shutdown.notify_waiters();
        let drive = m.drive.lock().unwrap().clone();
        unmount_system(&drive);
        drop_shortcut(&m.shortcut.lock().unwrap().clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 进来的 Range 只有**闭区间**才走预读:开口/后缀形态是「一次要一大坨」,
    /// 直通流式才是最优解,误判成预读会把大文件整段搬进内存。
    #[test]
    fn range_req_only_closed_spans() {
        assert_eq!(parse_range_req("bytes=0-65535"), Some((0, 65536)));
        assert_eq!(parse_range_req(" bytes=100-199 "), Some((100, 100)));
        assert_eq!(parse_range_req("bytes=5-5"), Some((5, 1)), "单字节区间也算闭区间");
        assert_eq!(parse_range_req("bytes=100-"), None, "开口区间走直通");
        assert_eq!(parse_range_req("bytes=-500"), None, "后缀区间走直通");
        assert_eq!(parse_range_req("bytes=200-100"), None, "倒置区间不认");
        assert_eq!(parse_range_req("items=0-1"), None, "非字节单位不认");
    }

    /// 预读的**行为**验证:拿一个本地桩当「对端」,数它收到几次请求。
    ///
    /// 这条比跑分重要 —— 跑分会被链路波动和 Windows 重定向器自己的整份缓存糊掉,
    /// 而这里问的是三件确定的事:
    ///  ① 连续的顺序块只开**一次**上游请求(否则预读等于没做);
    ///  ② 取出来的字节**位置正确**(错位是静默的,只会表现成「文件内容坏了」);
    ///  ③ 往回跳(倒着读)必须另开一条流,不能拿错位的缓冲糊弄过去。
    #[test]
    fn prefetch_pipelines_sequential_reads() {
        // 桩:只认 GET /api/fs/read,按 Range 回 206。文件内容 = 位置的低 8 位,
        // 这样任何错位都能被立刻看出来。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicU64::new(0));
        let total: u64 = 512 * 1024;
        {
            let hits = hits.clone();
            std::thread::spawn(move || {
                use std::io::{BufRead, BufReader, Write};
                for stream in listener.incoming() {
                    let Ok(mut s) = stream else { break };
                    let mut req = String::new();
                    let mut start = 0u64;
                    {
                        let mut r = BufReader::new(s.try_clone().unwrap());
                        loop {
                            let mut line = String::new();
                            if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                                break;
                            }
                            if let Some(v) = line.to_ascii_lowercase().strip_prefix("range:") {
                                start = v
                                    .trim()
                                    .trim_start_matches("bytes=")
                                    .split('-')
                                    .next()
                                    .and_then(|x| x.trim().parse().ok())
                                    .unwrap_or(0);
                            }
                            req.push_str(&line);
                        }
                    }
                    if !req.starts_with("GET /api/fs/read") {
                        let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                        continue;
                    }
                    hits.fetch_add(1, Ordering::SeqCst);
                    let body: Vec<u8> = (start..total).map(|i| (i % 251) as u8).collect();
                    let head = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\n\
                         Content-Range: bytes {}-{}/{}\r\nAccept-Ranges: bytes\r\n\r\n",
                        body.len(),
                        start,
                        total - 1,
                        total
                    );
                    let _ = s.write_all(head.as_bytes());
                    let _ = s.write_all(&body);
                }
            });
        }

        let m = Mount {
            source_id: "t".into(),
            name: "t".into(),
            node_id: String::new(),
            upstream_port: port,
            token: "tok".into(),
            secret: "s".into(),
            dav_port: 0,
            drive: Mutex::new(String::new()),
            alive: Arc::new(AtomicBool::new(true)),
            last_ok: AtomicU64::new(0),
            last_err: Mutex::new(String::new()),
            shutdown: Arc::new(tokio::sync::Notify::new()),
            writable: AtomicBool::new(false),
            shortcut: Mutex::new(String::new()),
            dir_cache: Mutex::new(HashMap::new()),
            prefetch: Mutex::new(Vec::new()),
        };

        // ① + ②:顺序四块,每块 64KB。
        let block = 64 * 1024u64;
        for i in 0..4u64 {
            let (data, got_total) = prefetch_serve(&m, "f.bin", i * block, block)
                .expect("预读不该报错")
                .expect("顺序块必须走预读");
            assert_eq!(got_total, total, "总长须来自上游区间头");
            assert_eq!(data.len(), block as usize, "第 {i} 块长度不对");
            let base = i * block;
            assert_eq!(data[0], (base % 251) as u8, "第 {i} 块起点错位");
            assert_eq!(
                data[data.len() - 1],
                ((base + block - 1) % 251) as u8,
                "第 {i} 块尾字节错位"
            );
        }
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "四个连续块必须复用同一条上游流,只开一次请求"
        );

        // ③:往回跳 → 手里那条流对不上,必须另开一条。
        let (data, _) = prefetch_serve(&m, "f.bin", 0, block)
            .expect("回跳不该报错")
            .expect("回跳仍应拿到数据");
        assert_eq!(data[0], 0, "回跳后必须从头给,不能续着上一条流");
        assert_eq!(hits.load(Ordering::SeqCst), 2, "回跳须新开一条上游流");

        // 换个文件也必须新开(别把 A 文件的缓冲喂给 B)。
        let (data, _) = prefetch_serve(&m, "g.bin", 0, block).unwrap().unwrap();
        assert_eq!(data.len(), block as usize);
        assert_eq!(hits.load(Ordering::SeqCst), 3, "换文件须新开上游流");
    }

    /// 子项属性必须由**父目录的热清单**就地供给,不许再过一次隧道。
    ///
    /// 这是「点开文件夹」快慢的结构性差别:资源管理器列完目录后,会对**每个子项**
    /// 再发一次 PROPFIND。200 个文件就是 200 次往返 —— 这条链路上一次几十毫秒,
    /// 合起来能到十秒级。改完之后这 200 次是零往返。
    /// 用桩计数验证,不受链路快慢影响(真机跑分会被链路波动糊掉)。
    #[test]
    fn child_stat_served_from_parent_listing() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let list_hits = Arc::new(AtomicU64::new(0));
        let stat_hits = Arc::new(AtomicU64::new(0));
        {
            let (lh, sh) = (list_hits.clone(), stat_hits.clone());
            std::thread::spawn(move || {
                use std::io::{BufRead, BufReader, Write};
                for stream in listener.incoming() {
                    let Ok(mut s) = stream else { break };
                    let mut first = String::new();
                    {
                        let mut r = BufReader::new(s.try_clone().unwrap());
                        let _ = r.read_line(&mut first);
                        loop {
                            let mut line = String::new();
                            if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                                break;
                            }
                        }
                    }
                    let body = if first.starts_with("GET /api/fs/list") {
                        lh.fetch_add(1, Ordering::SeqCst);
                        r#"{"entries":[{"name":"a.txt","is_dir":false,"size":11,"mtime":7},
                            {"name":"b.txt","is_dir":false,"size":22,"mtime":8}]}"#
                            .to_string()
                    } else {
                        sh.fetch_add(1, Ordering::SeqCst);
                        r#"{"entry":{"name":"a.txt","is_dir":false,"size":11,"mtime":7}}"#
                            .to_string()
                    };
                    let _ = s.write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .as_bytes(),
                    );
                }
            });
        }

        let m = Mount {
            source_id: "t".into(),
            name: "t".into(),
            node_id: String::new(),
            upstream_port: port,
            token: "tok".into(),
            secret: "s".into(),
            dav_port: 0,
            drive: Mutex::new(String::new()),
            alive: Arc::new(AtomicBool::new(true)),
            last_ok: AtomicU64::new(0),
            last_err: Mutex::new(String::new()),
            shutdown: Arc::new(tokio::sync::Notify::new()),
            writable: AtomicBool::new(false),
            shortcut: Mutex::new(String::new()),
            dir_cache: Mutex::new(HashMap::new()),
            prefetch: Mutex::new(Vec::new()),
        };

        // 列一次父目录(1 次往返),随后逐个子项取属性 —— 应当**一次都不再打**对端。
        let entries = up_list_cached(&m, "dir").expect("列目录");
        assert_eq!(entries.len(), 2);
        for _ in 0..50 {
            let a = stat_fast(&m, "dir/a.txt").expect("子项属性应命中父目录清单");
            assert_eq!((a.size, a.mtime), (11, 7), "取到的属性须与清单一致");
            let b = stat_fast(&m, "dir/b.txt").expect("子项属性应命中父目录清单");
            assert_eq!(b.size, 22);
        }
        assert_eq!(list_hits.load(Ordering::SeqCst), 1, "父目录只该列一次");
        assert_eq!(
            stat_hits.load(Ordering::SeqCst),
            0,
            "100 次子项属性必须全部由缓存供给,一次都不许过隧道"
        );

        // 缓存不认识的名字才允许落到对端。
        let _ = stat_fast(&m, "dir/不在清单里.txt");
        assert_eq!(stat_hits.load(Ordering::SeqCst), 1, "未知子项才问对端");

        // 写操作后缓存立即作废 —— 不能让用户看不见自己刚拖进去的文件。
        m.cache_clear();
        let _ = stat_fast(&m, "dir/a.txt");
        assert_eq!(stat_hits.load(Ordering::SeqCst), 2, "清缓存后须重新问对端");
    }

    /// 上游区间头的两种取法:起点+总长(记账用)、本段长度(压缩流的 Content-Length 用)。
    #[test]
    fn content_range_parsing() {
        assert_eq!(parse_content_range("bytes 100-999/12345"), Some((100, 12345)));
        assert_eq!(parse_content_range_span("bytes 100-999/12345"), Some(900));
        assert_eq!(parse_content_range_span("bytes 0-0/1"), Some(1));
        assert_eq!(parse_content_range("bytes */12345"), None, "不可满足区间没有起点");
        assert_eq!(parse_content_range_span("bogus"), None);
    }

    #[test]
    fn dates_and_encoding() {
        // 2026-07-24 是周五;0 秒 = 1970-01-01 周四。
        assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(iso_date(0), "1970-01-01T00:00:00Z");
        // 已知锚点:2020-01-01 00:00:00 UTC = 1577836800,周三。
        assert_eq!(http_date(1_577_836_800), "Wed, 01 Jan 2020 00:00:00 GMT");
        assert_eq!(enc_comp("a b/中"), "a%20b%2F%E4%B8%AD");
        assert_eq!(pct_decode("a%20b%2F%E4%B8%AD"), "a b/中");
        assert_eq!(enc_path("文档/子 目录"), "%E6%96%87%E6%A1%A3/%E5%AD%90%20%E7%9B%AE%E5%BD%95");
        assert_eq!(xml_escape("a<b>&\"c"), "a&lt;b&gt;&amp;&quot;c");
    }

    #[test]
    fn prop_block_shapes() {
        let b = prop_block("/s3cr3t", "dir/f.txt", "f.txt", false, 42, 0);
        assert!(b.contains("<D:href>/s3cr3t/dir/f.txt</D:href>"));
        assert!(b.contains("<D:getcontentlength>42</D:getcontentlength>"));
        assert!(!b.contains("<D:collection/>"));
        let d = prop_block("/s3cr3t", "dir", "dir", true, 0, 0);
        assert!(d.contains("<D:href>/s3cr3t/dir/</D:href>"), "目录 href 须带尾斜杠");
        assert!(d.contains("<D:collection/>"));
        let root = prop_block("/s3cr3t", "", "远程盘", true, 0, 0);
        assert!(root.contains("<D:href>/s3cr3t/</D:href>"));
    }
}
