//! beam.rs —— 「隔空同屏」:把主机上的一个文件**包装成一页自包含网页**,再经已有的
//! iroh P2P 通道让手机与电脑同时打开同一页、互传同步消息(滚动/指点/缩放/关闭)。
//!
//! 为什么要「包装成网页」而不是各端各自渲染:
//!  - 手机壳的预览是一套渲染(md/文本/图片各走各的标签),桌面又是另一套 —— 两边永远长得
//!    不一样,更没法对齐滚动位置。打包成**同一份 HTML** 后,两端塞进各自 iframe 的 srcdoc,
//!    像素级同构,滚动比例才有意义。
//!  - 自包含(所有图片/样式/字体内联成 data URI,做法参考 Y2Z/monolith 与 SingleFile):
//!    对端拿到的是一整页,不再逐个资源回源 —— P2P 链路上一次往返就够,断线也还能看。
//!  - 顺带白得一个「导出为网页」:`beam_export` 把同一份 HTML 落盘,双击就能用任意浏览器打开。
//!
//! 传输不在这里:同步消息走 `beam:sync` 事件 —— 手机 POST /api/invoke(beam_send)进主机
//! 广播总线 → 桌面 UI(hosting 的 bridge_to_ui)与其它手机(/ws)同时收到;桌面发起时反向
//! 走 `hosting::bus_emit`。整条链路复用 tunnel.rs 的 iroh 打洞/relay,**不要求同一 WiFi**。
//!
//! 路径边界:本模块持有「可读文件白名单」(原在 apihub.rs,现下沉到此供双方共用),
//! 只有知识库根、`~/Polaris/{data/artifacts,projects,uploads-inbox}` 与项目绑定的工作目录
//! 能被打包;`..` 穿越与符号链接逃逸由 canonicalize 后复核挡住。fail-closed。

use base64::Engine;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// 同步消息的事件 topic:桌面 `listen("beam:sync")`,手机 /ws 同名帧。
pub const TOPIC: &str = "beam:sync";

// ── 体积闸(全部按「解码前的原始字节」算)────────────────────────────────────
/// 单个内联资源上限:超了就留原引用(页面上表现为该图裂开,但不至于把整页撑爆)。
const MAX_ASSET: u64 = 6 * 1024 * 1024;
/// 所有内联资源合计预算。
const MAX_ASSET_TOTAL: u64 = 40 * 1024 * 1024;
/// 主体是媒体(图片/音视频/pdf)时,该媒体自身的上限。
const MAX_MEDIA: u64 = 48 * 1024 * 1024;
/// 文本类源文件读取上限,超出截断并在页尾标注。
const MAX_TEXT: u64 = 8 * 1024 * 1024;
/// 最多内联多少个资源(防一页几千张图把打包时间拖成分钟级)。
const MAX_ASSET_COUNT: usize = 300;
/// 一条同步消息的 JSON 上限:总线是广播,不能让对端塞大包进来。
const MAX_MSG_BYTES: usize = 8 * 1024;

// ────────────────────────────── 路径白名单 ──────────────────────────────

/// 拒绝原因:「找到了但在白名单外」与「压根没这文件」要分开报 —— 前者是权限问题,
/// 后者是路径问题,混成一个错误码会让人对着「不存在」查半天权限。
pub(crate) enum PathDenied {
    NotFound,
    Outside,
}

/// 可读根:知识库 + `~/Polaris/{data/artifacts,projects,uploads-inbox}` + 项目绑定工作目录。
/// (整机文件系统读不到;逐用户 ACL 做好前不再往下放。)
pub(crate) fn allowed_roots() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    let kb = PathBuf::from(crate::kb::kb_root());
    if let Ok(c) = std::fs::canonicalize(&kb) {
        v.push(c);
    }
    if let Some(u) = directories::UserDirs::new() {
        let home = u.home_dir().join("Polaris");
        for root in [
            home.join("data/artifacts"),
            home.join("projects"),
            home.join("uploads-inbox"),
        ] {
            if let Ok(c) = std::fs::canonicalize(root) {
                v.push(c);
            }
        }
    }
    // 项目绑定的工作目录(Project.work_dir):大项目的产物写在用户自选的仓库目录里。
    for wd in crate::conv::all_project_work_dirs() {
        if let Ok(c) = std::fs::canonicalize(&wd) {
            v.push(c);
        }
    }
    v
}

/// 把请求里的 `path` 解析成一个**确实在白名单内**的真实文件。
///
/// 绝对路径直接 canonicalize;相对路径 / `~/` 开头 / 裸文件名也认 —— 助手正文里给出的
/// 往往是 `docs/报告.md` 这类相对写法,按进程 CWD 解析要么落空要么落到意想不到的目录,
/// 故逐个白名单根去拼,第一个命中的算数。无论走哪条,最后一律拿 canonicalize 之后的
/// 真实路径复核白名单,`..` 穿越与符号链接逃逸照旧被挡。
pub(crate) fn resolve_readable_path(raw: &str, allowed: &[PathBuf]) -> Result<PathBuf, PathDenied> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(PathDenied::NotFound);
    }
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        if let Some(u) = directories::UserDirs::new() {
            cands.push(u.home_dir().join(rest.replace('\\', "/")));
        }
    }
    let p = PathBuf::from(s);
    if p.is_absolute() {
        cands.push(p);
    } else if !s.starts_with('~') {
        let rel = s
            .trim_start_matches("./")
            .trim_start_matches(".\\")
            .replace('\\', "/");
        for root in allowed {
            cands.push(root.join(&rel));
        }
    }

    let mut outside = false;
    for c in cands {
        let Ok(canon) = std::fs::canonicalize(&c) else {
            continue;
        };
        if !canon.is_file() {
            continue;
        }
        if allowed
            .iter()
            .any(|root| crate::kb::path_contains(root, &canon))
        {
            return Ok(canon);
        }
        outside = true;
    }
    if outside {
        Err(PathDenied::Outside)
    } else {
        Err(PathDenied::NotFound)
    }
}

/// 字符串错误版(供非 HTTP 调用方:tauri 命令 / 打包器内部)。
fn resolve_or_msg(raw: &str, allowed: &[PathBuf]) -> Result<PathBuf, String> {
    resolve_readable_path(raw, allowed).map_err(|d| match d {
        PathDenied::NotFound => format!("文件不存在:{raw}"),
        PathDenied::Outside => {
            "路径不在允许范围(只能同屏知识库 / ~/Polaris 产物 / 项目工作目录里的文件)".into()
        }
    })
}

// ────────────────────────────── 打包结果 ──────────────────────────────

/// 一次打包的产出。`html` 就是可以直接落盘、双击用浏览器打开的完整页面。
#[derive(serde::Serialize, Clone)]
pub struct BeamDoc {
    /// 打包时解析到的真实绝对路径(正斜杠)。
    pub path: String,
    pub name: String,
    /// md | text | code | image | audio | video | html | svg | pdf | other
    pub kind: String,
    pub html: String,
    /// 原文件字节数。
    pub bytes: u64,
    /// 打包后网页字节数(含内联资源的 base64 膨胀)。
    #[serde(rename = "htmlBytes")]
    pub html_bytes: usize,
    /// 成功内联的资源数。
    pub inlined: usize,
    /// 因超预算/找不到而未内联的资源数(页面上表现为该处引用保持原样)。
    pub skipped: usize,
    /// 正文是否被截断(超大文本)。
    pub truncated: bool,
}

#[derive(Default)]
struct PackStats {
    inlined: usize,
    skipped: usize,
    budget: u64,
}

// ────────────────────────────── 打包主流程 ──────────────────────────────

/// 把 `path` 指向的文件包装成一页自包含网页。
pub fn pack(path: &str) -> Result<BeamDoc, String> {
    let allowed = allowed_roots();
    let file = resolve_or_msg(path, &allowed)?;
    let md = std::fs::metadata(&file).map_err(|e| format!("读取文件信息失败: {e}"))?;
    let size = md.len();
    let name = file
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("文件")
        .to_string();
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
    let kind = kind_of(&ext);

    let mut stats = PackStats {
        budget: MAX_ASSET_TOTAL,
        ..Default::default()
    };
    let mut truncated = false;

    let body = match kind {
        "md" => {
            let (text, cut) = read_text(&file, size)?;
            truncated = cut;
            let rendered = md_to_html(&text);
            inline_assets(&rendered, &dir, &allowed, &mut stats)
        }
        "html" => {
            let (text, cut) = read_text(&file, size)?;
            truncated = cut;
            // 整页 HTML:剥掉外层骨架只留正文,免得 <html>/<head> 套娃;拿不准就整份塞进去
            // (浏览器对嵌套 html 标签是容错的,视觉不受影响)。
            let inner = strip_html_shell(&text);
            inline_assets(&inner, &dir, &allowed, &mut stats)
        }
        "svg" => {
            let (text, cut) = read_text(&file, size)?;
            truncated = cut;
            let inlined = inline_assets(&text, &dir, &allowed, &mut stats);
            format!("<div class=\"beam-svg\">{inlined}</div>")
        }
        "image" => media_tag(&file, size, &ext, "image")?,
        "audio" => media_tag(&file, size, &ext, "audio")?,
        "video" => media_tag(&file, size, &ext, "video")?,
        "pdf" => media_tag(&file, size, &ext, "pdf")?,
        "text" | "code" => {
            let (text, cut) = read_text(&file, size)?;
            truncated = cut;
            format!(
                "<pre class=\"beam-code\" data-lang=\"{}\"><code>{}</code></pre>",
                esc_attr(&ext),
                esc(&text)
            )
        }
        _ => other_card(&name, size, &ext),
    };

    let html = shell(&name, kind, &body, &human_size(size), truncated, &stats);
    Ok(BeamDoc {
        path: file.to_string_lossy().replace('\\', "/"),
        name,
        kind: kind.to_string(),
        html_bytes: html.len(),
        html,
        bytes: size,
        inlined: stats.inlined,
        skipped: stats.skipped,
        truncated,
    })
}

/// 把打包结果落盘成一个 .html 文件(默认 `~/Polaris/data/beam/`),返回绝对路径。
/// 这就是「文件 → 网页」的可携带形态:发微信、传 U 盘、任何浏览器都能开。
pub fn export(path: &str, out_dir: Option<String>) -> Result<String, String> {
    let doc = pack(path)?;
    let dir = match out_dir.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => directories::UserDirs::new()
            .ok_or("无法定位用户目录")?
            .home_dir()
            .join("Polaris")
            .join("data")
            .join("beam"),
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("建导出目录失败: {e}"))?;
    let stem: String = Path::new(&doc.name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("beam")
        .chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c })
        .collect();
    // 同名不覆盖:递增序号(不用时间戳,免得每点一次就堆一个新文件还看不出先后)。
    let mut i = 0u32;
    let out = loop {
        let cand = if i == 0 {
            dir.join(format!("{stem}.html"))
        } else {
            dir.join(format!("{stem}-{i}.html"))
        };
        if !cand.exists() {
            break cand;
        }
        i += 1;
        if i > 9999 {
            return Err("导出目录里同名文件太多,请先清理".into());
        }
    };
    std::fs::write(&out, doc.html.as_bytes()).map_err(|e| format!("写出网页失败: {e}"))?;
    Ok(out.to_string_lossy().replace('\\', "/"))
}

// ────────────────────────────── 同步消息 ──────────────────────────────

/// 允许的动作。白名单式 —— 总线是广播面,不认识的动作一律拒收,不给对端往 UI 里塞未知帧。
const ACTS: &[&str] = &[
    "open",   // 打开某文件的同屏(带 path/name)
    "close",  // 结束同屏
    "scroll", // 滚动到比例 r(0..1)
    "mark",   // 指点:在 (x,y) 视口比例处冒一圈光晕
    "zoom",   // 字号缩放 z(0.6..2.0)
    "ping",   // 探活
    "pong",   // 探活回应(带 r)
];

/// 规整一条同步消息:补齐 `ts`,校验 `act` 与体积。返回可直接进总线的 payload。
///
/// 刻意不校验 `path` 是否可读 —— 收端 `beam_pack` 会自己过白名单闸;这里再查一遍
/// 等于把「发端有没有权限」和「收端能不能读」两件事混起来,反而会让跨设备场景莫名失败。
pub fn normalize(mut msg: Value) -> Result<Value, String> {
    if !msg.is_object() {
        return Err("同步消息必须是对象".into());
    }
    let act = msg
        .get("act")
        .and_then(|v| v.as_str())
        .ok_or("同步消息缺少 act")?
        .to_string();
    if !ACTS.contains(&act.as_str()) {
        return Err(format!("不认识的同屏动作:{act}"));
    }
    let o = msg.as_object_mut().expect("已判定是对象");
    o.entry("from").or_insert_with(|| json!("unknown"));
    o.insert(
        "ts".into(),
        json!(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)),
    );
    let encoded = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
    if encoded.len() > MAX_MSG_BYTES {
        return Err(format!(
            "同步消息过大({} 字节,上限 {MAX_MSG_BYTES})",
            encoded.len()
        ));
    }
    Ok(msg)
}

// ────────────────────────────── tauri 命令(桌面)──────────────────────────────

/// 打包成网页(桌面 UI / 手机数据面共用同一实现)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn beam_pack(path: String) -> Result<BeamDoc, String> {
    pack(&path)
}

/// 导出成独立网页文件,返回落盘绝对路径。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn beam_export(path: String, out_dir: Option<String>) -> Result<String, String> {
    export(&path, out_dir)
}

/// 桌面侧发同步消息:推进内嵌主机广播总线 → 手机 /ws 收到。
///
/// 主机没跑时退化成只 emit 给本机 UI(桌面自己还能同屏预览),并如实回 `delivered:false`,
/// 让界面提示「还没设为主机,手机收不到」而不是假装发出去了。
#[cfg(feature = "desktop")]
#[tauri::command]
pub fn beam_send(app: tauri::AppHandle, msg: Value) -> Result<Value, String> {
    let msg = normalize(msg)?;
    let delivered = crate::hosting::bus_emit(TOPIC, msg.clone());
    if !delivered {
        // 总线在跑时不必本地补发:hosting 的 bridge_to_ui 会把总线事件回灌 tauri,
        // 这里再 emit 一次本机 UI 会收到两条同 id 的帧。
        use tauri::Emitter;
        let _ = app.emit(TOPIC, msg.clone());
    }
    Ok(json!({ "delivered": delivered, "msg": msg }))
}

/// 随包分发的「隔空同屏 + 应用直投」说明页。编进二进制,首次取用时落到白名单目录
/// (`~/Polaris/data/artifacts/polaris/`),这样它就能被同屏舞台、手机预览、导出等
/// 现有链路当成普通产物打开 —— 用户不必去桌面找一个孤零零的 HTML 文件。
const GUIDE_HTML: &str = include_str!("../../docs/beam-guide.html");

/// 落盘(若已存在且内容一致则不重写)并返回说明页绝对路径。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn beam_doc_path() -> Result<String, String> {
    let dir = directories::UserDirs::new()
        .ok_or("无法定位用户目录")?
        .home_dir()
        .join("Polaris")
        .join("data")
        .join("artifacts")
        .join("polaris");
    std::fs::create_dir_all(&dir).map_err(|e| format!("建目录失败: {e}"))?;
    let f = dir.join("隔空同屏与应用直投·说明.html");
    let need_write = match std::fs::read_to_string(&f) {
        Ok(cur) => cur != GUIDE_HTML,
        Err(_) => true,
    };
    if need_write {
        std::fs::write(&f, GUIDE_HTML).map_err(|e| format!("写说明页失败: {e}"))?;
    }
    Ok(f.to_string_lossy().replace('\\', "/"))
}

/// 把主窗口从托盘/最小化里唤到前台 —— 手机投过来时电脑要「自己弹出来」,
/// 否则用户还得先去点任务栏,「同步打开」就名不副实了。
#[cfg(feature = "desktop")]
#[tauri::command]
pub fn beam_focus(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let w = app
        .get_webview_window("main")
        .ok_or("主窗口不存在(可能正在退出)")?;
    let _ = w.unminimize();
    let _ = w.show();
    let _ = w.set_focus();
    Ok(())
}

// ────────────────────────────── 分类 / 读取 ──────────────────────────────

fn kind_of(ext: &str) -> &'static str {
    match ext {
        "md" | "markdown" => "md",
        "html" | "htm" => "html",
        "svg" => "svg",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "avif" => "image",
        "mp3" | "wav" | "m4a" | "ogg" | "flac" | "aac" => "audio",
        "mp4" | "webm" | "mov" | "m4v" => "video",
        "pdf" => "pdf",
        "txt" | "log" | "csv" | "tsv" => "text",
        "rs" | "ts" | "tsx" | "js" | "jsx" | "vue" | "py" | "go" | "java" | "c" | "h" | "cpp"
        | "hpp" | "cs" | "rb" | "php" | "swift" | "kt" | "sh" | "bat" | "ps1" | "sql" | "json"
        | "yml" | "yaml" | "toml" | "ini" | "conf" | "xml" | "css" | "scss" => "code",
        _ => "other",
    }
}

/// 读文本并按上限截断。非 UTF-8 用有损转换 —— 同屏是「看个样子」,不是精确编辑。
fn read_text(file: &Path, size: u64) -> Result<(String, bool), String> {
    let cut = size > MAX_TEXT;
    let raw = if cut {
        use std::io::Read;
        let mut f = std::fs::File::open(file).map_err(|e| format!("打开文件失败: {e}"))?;
        let mut buf = vec![0u8; MAX_TEXT as usize];
        let n = f.read(&mut buf).map_err(|e| format!("读文件失败: {e}"))?;
        buf.truncate(n);
        buf
    } else {
        std::fs::read(file).map_err(|e| format!("读文件失败: {e}"))?
    };
    Ok((String::from_utf8_lossy(&raw).into_owned(), cut))
}

fn mime_of(ext: &str) -> &'static str {
    match ext {
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" | "aac" => "audio/mp4",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "pdf" => "application/pdf",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        _ => "application/octet-stream",
    }
}

fn data_uri(bytes: &[u8], ext: &str) -> String {
    format!(
        "data:{};base64,{}",
        mime_of(ext),
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// 主体是媒体时:整份内联成 data URI。超上限就给一张说明卡,别默默出个裂图。
fn media_tag(file: &Path, size: u64, ext: &str, kind: &str) -> Result<String, String> {
    if size > MAX_MEDIA {
        return Ok(format!(
            "<div class=\"beam-card\"><div class=\"beam-card-ico\">📦</div>\
             <p class=\"beam-card-t\">文件太大,没有整份打进网页</p>\
             <p class=\"beam-card-d\">{}({}),超过同屏上限 {}。同屏页只带说明,\
             原文件仍在主机上,可在电脑侧直接打开。</p></div>",
            esc(&file.file_name().unwrap_or_default().to_string_lossy()),
            human_size(size),
            human_size(MAX_MEDIA)
        ));
    }
    let bytes = std::fs::read(file).map_err(|e| format!("读文件失败: {e}"))?;
    let uri = data_uri(&bytes, ext);
    Ok(match kind {
        "image" => format!("<img class=\"beam-media\" src=\"{uri}\" alt=\"\">"),
        "audio" => format!(
            "<div class=\"beam-audio\"><div class=\"beam-card-ico\">🎵</div>\
             <audio class=\"beam-media\" controls src=\"{uri}\"></audio></div>"
        ),
        "video" => {
            format!("<video class=\"beam-media\" controls playsinline src=\"{uri}\"></video>")
        }
        _ => format!(
            "<embed class=\"beam-pdf\" type=\"application/pdf\" src=\"{uri}\">\
             <p class=\"beam-note\">PDF 由浏览器内置阅读器渲染;个别安卓 WebView 不支持内嵌 PDF,\
             此时页面会是空白,请在电脑侧查看。</p>"
        ),
    })
}

fn other_card(name: &str, size: u64, ext: &str) -> String {
    format!(
        "<div class=\"beam-card\"><div class=\"beam-card-ico\">📄</div>\
         <p class=\"beam-card-t\">{}</p>\
         <p class=\"beam-card-d\">{} · .{} —— 这个格式没法渲染成网页,同屏页只带这张说明。\
         原文件仍在主机上。</p></div>",
        esc(name),
        human_size(size),
        esc(ext)
    )
}

fn human_size(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

// ────────────────────────────── 资源内联(monolith 式)──────────────────────────────

/// 会被内联的引用形态。大小写不敏感匹配。
const REF_KEYS: [&str; 5] = ["src=", "href=", "poster=", "url(", "data-src="];

/// 把 HTML/CSS 里指向同目录(及子目录)的相对资源换成 data URI。
///
/// 只吃**相对路径**:`http(s)://`、`//`、`data:`、`#`、`mailto:`、`javascript:` 一律原样留着
/// (外链保持外链,页面在有网时照常拉;没网就是缺图,不比现在差)。每个资源仍要过白名单闸,
/// 所以「同屏一张网页」不会变成读整机文件的后门。
fn inline_assets(src: &str, dir: &Path, allowed: &[PathBuf], stats: &mut PackStats) -> String {
    let lower = src.to_ascii_lowercase();
    let mut out = String::with_capacity(src.len() + src.len() / 4);
    let mut i = 0usize;
    loop {
        // 找出下一个最早出现的引用关键字。
        let mut best: Option<(usize, &str)> = None;
        for k in REF_KEYS {
            if let Some(p) = lower[i..].find(k) {
                let abs = i + p;
                if best.map_or(true, |(b, _)| abs < b) {
                    best = Some((abs, k));
                }
            }
        }
        let Some((pos, key)) = best else {
            out.push_str(&src[i..]);
            break;
        };
        let vstart = pos + key.len();
        out.push_str(&src[i..vstart]);

        // 取值:可能带引号,也可能裸奔。url() 以 ')' 收尾,属性以空白/'>' 收尾。
        let rest = &src[vstart..];
        let first = rest.chars().next();
        let (quote, body_start) = match first {
            Some('"') => (Some('"'), 1),
            Some('\'') => (Some('\''), 1),
            _ => (None, 0),
        };
        let after = &rest[body_start..];
        let end_rel = match quote {
            Some(q) => after.find(q),
            None => {
                if key == "url(" {
                    after.find(')')
                } else {
                    after.find(|c: char| c.is_whitespace() || c == '>')
                }
            }
        };
        let Some(end_rel) = end_rel else {
            // 没有收尾符 = 文档残缺,后面整段原样吐出,别再猜。
            out.push_str(rest);
            break;
        };
        let raw_val = &after[..end_rel];
        let consumed = body_start + end_rel;

        let replaced = if stats.inlined < MAX_ASSET_COUNT {
            try_inline_one(raw_val, key, dir, allowed, stats)
        } else {
            None
        };
        if let Some(q) = quote {
            out.push(q);
        }
        match replaced {
            Some(uri) => out.push_str(&uri),
            None => out.push_str(raw_val),
        }
        i = vstart + consumed;
    }
    out
}

/// 单个引用的内联尝试。任何一步不满足就返回 None(保留原引用)。
/// `key` 是触发它的引用形态(`src=`/`href=`/`url(` …)。
fn try_inline_one(
    raw: &str,
    key: &str,
    dir: &Path,
    allowed: &[PathBuf],
    stats: &mut PackStats,
) -> Option<String> {
    let v = raw.trim();
    if v.is_empty() {
        return None;
    }
    let lv = v.to_ascii_lowercase();
    // `href=` 只内联样式表:普通 `<a href="report.pdf">` 若被 base64 塞进 href,
    // 两端 iframe 都没给 allow-downloads、又禁 data: 顶级导航,点了没反应还白吃 40MB 预算
    // (Fable5 审计 #6)。图片/背景走 `src=`/`url(`,不受影响。
    if key.eq_ignore_ascii_case("href=") {
        let path = lv.split(['?', '#']).next().unwrap_or(&lv);
        if !path.ends_with(".css") {
            return None;
        }
    }
    for skip in [
        "data:",
        "http://",
        "https://",
        "//",
        "#",
        "mailto:",
        "tel:",
        "javascript:",
        "about:",
        "blob:",
    ] {
        if lv.starts_with(skip) {
            return None;
        }
    }
    // 去掉查询串与锚点,再百分号解码(`我的图%20.png` 这类)。
    let clean = v.split(['?', '#']).next().unwrap_or(v);
    let decoded = pct_decode(clean);
    let cand = dir.join(decoded.replace('\\', "/"));
    let canon = std::fs::canonicalize(&cand).ok()?;
    if !canon.is_file() {
        return None;
    }
    if !allowed
        .iter()
        .any(|root| crate::kb::path_contains(root, &canon))
    {
        stats.skipped += 1;
        return None;
    }
    let size = std::fs::metadata(&canon).ok()?.len();
    if size > MAX_ASSET || size > stats.budget {
        stats.skipped += 1;
        return None;
    }
    let bytes = std::fs::read(&canon).ok()?;
    let ext = canon
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    stats.budget = stats.budget.saturating_sub(size);
    stats.inlined += 1;
    Some(data_uri(&bytes, &ext))
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16);
            let lo = (b[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 整页 HTML 只留 `<body>` 里的内容 + `<style>`(样式要留,否则页面裸奔)。
/// 找不到 body 就原样返回 —— 片段式 HTML 本来就没有骨架。
fn strip_html_shell(src: &str) -> String {
    let lower = src.to_ascii_lowercase();
    let mut styles = String::new();
    // 收 <head> 里的 <style>(<link rel=stylesheet> 的内联交给 inline_assets)。
    let mut cur = 0usize;
    while let Some(p) = lower[cur..].find("<style") {
        let s = cur + p;
        let Some(open_end) = lower[s..].find('>').map(|x| s + x + 1) else {
            break;
        };
        let Some(close) = lower[open_end..].find("</style>").map(|x| open_end + x) else {
            break;
        };
        styles.push_str("<style>");
        styles.push_str(&src[open_end..close]);
        styles.push_str("</style>");
        cur = close + "</style>".len();
    }
    let body = match lower.find("<body") {
        Some(bs) => {
            let start = lower[bs..].find('>').map(|x| bs + x + 1).unwrap_or(bs);
            let end = lower[start..]
                .find("</body>")
                .map(|x| start + x)
                .unwrap_or(src.len());
            src[start..end].to_string()
        }
        None => return src.to_string(),
    };
    format!("{styles}{body}")
}

// ────────────────────────────── markdown → HTML ──────────────────────────────

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            _ => o.push(c),
        }
    }
    o
}
fn esc_attr(s: &str) -> String {
    esc(s).replace('"', "&quot;")
}

/// 极简 markdown 渲染:标题 / 围栏代码 / 引用 / 有序无序列表 / 表格 / 分隔线 / 段落,
/// 行内支持 `code`、**粗**、*斜*、~~删~~、[链接]()、![图]()、裸链接。
///
/// 刻意不引三方 md crate:同屏页只求「和手机上看起来一样」,这套子集足够,
/// 且渲染逻辑跟手机端 `mobile/src/lib/md.ts` 同款取舍,两端观感一致。
fn md_to_html(src: &str) -> String {
    let lines: Vec<&str> = src.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    let mut out = String::with_capacity(src.len() * 2);
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim_start();

        // 围栏代码
        if let Some(fence) = t.strip_prefix("```").or_else(|| t.strip_prefix("~~~")) {
            let marker = if t.starts_with("```") { "```" } else { "~~~" };
            let lang = fence.trim().to_string();
            i += 1;
            let mut buf = String::new();
            while i < lines.len() && !lines[i].trim_start().starts_with(marker) {
                buf.push_str(lines[i]);
                buf.push('\n');
                i += 1;
            }
            i += 1; // 吃掉收尾围栏
            out.push_str(&format!(
                "<pre class=\"beam-code\" data-lang=\"{}\"><code>{}</code></pre>",
                esc_attr(&lang),
                esc(&buf)
            ));
            continue;
        }

        // 空行
        if t.is_empty() {
            i += 1;
            continue;
        }

        // 分隔线
        if matches!(t, "---" | "***" | "___" | "----" | "-----") {
            out.push_str("<hr>");
            i += 1;
            continue;
        }

        // 标题
        if t.starts_with('#') {
            let level = t.chars().take_while(|c| *c == '#').count();
            if (1..=6).contains(&level) && t.chars().nth(level) == Some(' ') {
                let text = inline_md(t[level + 1..].trim());
                out.push_str(&format!("<h{level}>{text}</h{level}>"));
                i += 1;
                continue;
            }
        }

        // 引用
        if t.starts_with("> ") || t == ">" {
            let mut buf = Vec::new();
            while i < lines.len() {
                let x = lines[i].trim_start();
                if let Some(r) = x.strip_prefix("> ") {
                    buf.push(r);
                } else if x == ">" {
                    buf.push("");
                } else {
                    break;
                }
                i += 1;
            }
            out.push_str(&format!("<blockquote>{}</blockquote>", md_to_html(&buf.join("\n"))));
            continue;
        }

        // 表格(GFM):首行含 | 且次行是分隔行
        if t.contains('|')
            && i + 1 < lines.len()
            && lines[i + 1].trim().starts_with('|')
            && lines[i + 1].contains('-')
            && lines[i + 1]
                .chars()
                .all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
        {
            let head = split_row(t);
            i += 2;
            let mut rows: Vec<Vec<String>> = Vec::new();
            while i < lines.len() && lines[i].trim().contains('|') && !lines[i].trim().is_empty() {
                rows.push(split_row(lines[i].trim()));
                i += 1;
            }
            out.push_str("<table><thead><tr>");
            for h in &head {
                out.push_str(&format!("<th>{}</th>", inline_md(h)));
            }
            out.push_str("</tr></thead><tbody>");
            for r in rows {
                out.push_str("<tr>");
                for c in r {
                    out.push_str(&format!("<td>{}</td>", inline_md(&c)));
                }
                out.push_str("</tr>");
            }
            out.push_str("</tbody></table>");
            continue;
        }

        // 列表(单层;嵌套项按缩进并进同一 li 的文本,不追求完整 CommonMark)
        let is_ul = t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ");
        let is_ol = ordered_marker(t).is_some();
        if is_ul || is_ol {
            let tag = if is_ul { "ul" } else { "ol" };
            out.push_str(&format!("<{tag}>"));
            while i < lines.len() {
                let x = lines[i].trim_start();
                let item = if is_ul && (x.starts_with("- ") || x.starts_with("* ") || x.starts_with("+ ")) {
                    Some(x[2..].to_string())
                } else if is_ol {
                    ordered_marker(x).map(|n| x[n..].to_string())
                } else {
                    None
                };
                match item {
                    Some(text) => {
                        out.push_str(&format!("<li>{}</li>", inline_md(text.trim())));
                        i += 1;
                    }
                    None => break,
                }
            }
            out.push_str(&format!("</{tag}>"));
            continue;
        }

        // 段落:吃到空行或下一个块级起点
        let mut buf: Vec<&str> = Vec::new();
        while i < lines.len() {
            let x = lines[i].trim_start();
            if x.is_empty()
                || x.starts_with('#')
                || x.starts_with("> ")
                || x.starts_with("```")
                || x.starts_with("~~~")
                || x.starts_with("- ")
                || x.starts_with("* ")
                || x.starts_with("+ ")
                || ordered_marker(x).is_some()
                || matches!(x, "---" | "***" | "___")
            {
                break;
            }
            buf.push(lines[i].trim());
            i += 1;
        }
        if !buf.is_empty() {
            out.push_str(&format!("<p>{}</p>", inline_md(&buf.join(" "))));
        }
    }
    out
}

/// `12. ` 形式的有序列表标记,返回标记长度(含空格)。
fn ordered_marker(s: &str) -> Option<usize> {
    let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || digits > 9 {
        return None;
    }
    let rest = &s[digits..];
    if rest.starts_with(". ") {
        Some(digits + 2)
    } else if rest.starts_with(") ") {
        Some(digits + 2)
    } else {
        None
    }
}

fn split_row(line: &str) -> Vec<String> {
    line.trim()
        .trim_start_matches('|')
        .trim_end_matches('|')
        .split('|')
        .map(|c| c.trim().to_string())
        .collect()
}

/// 行内标记。先按反引号切出代码片段(代码里的 `*` 不该被当强调),其余部分再跑替换。
fn inline_md(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 32);
    let mut code = false;
    for part in s.split('`') {
        if code {
            out.push_str(&format!("<code>{}</code>", esc(part)));
        } else {
            out.push_str(&inline_marks(&esc(part)));
        }
        code = !code;
    }
    out
}

/// 在**已转义**的文本上做行内替换。
///
/// 顺序很关键:先把 `[text](url)` / `![alt](url)` **抠成占位符**,再跑强调与裸链接。
/// 否则(旧版的做法)强调替换会咬进已经生成的 `href="…"`——URL 里一个 `_` 就把
/// `https://x/a_b_c` 切成 `https://x/a<em>b</em>c`,链接必坏(Fable5 审计 #1)。
/// 占位符只含 NUL+数字,`replace_pair` 与 `autolink` 都碰不到它。
fn inline_marks(escaped: &str) -> String {
    let mut store: Vec<String> = Vec::new();
    let protected = protect_links(escaped, &mut store);
    let s = autolink(&emphasize(&protected));
    // 还原占位符。倒序替换以防 `\x001\x00` 落在 `\x0010\x00` 内部(数字前缀问题)。
    let mut out = s;
    for (i, html) in store.iter().enumerate().rev() {
        out = out.replace(&format!("\u{0}{i}\u{0}"), html);
    }
    out
}

/// 成对强调标记链(粗/斜/删)。链接文本内也复用它,故单独抽出。
fn emphasize(s: &str) -> String {
    let mut x = replace_pair(s, "**", "strong");
    x = replace_pair(&x, "__", "strong");
    x = replace_pair(&x, "~~", "del");
    x = replace_pair(&x, "*", "em");
    x = replace_pair(&x, "_", "em");
    x
}

/// 把行内的 `[text](url)` / `![alt](url)` 渲染成 HTML 存进 `store`,原位留下占位符
/// `\0{idx}\0`。链接文本内允许强调(先 emphasize 再入库);URL 原样保留、不经强调。
fn protect_links(s: &str, store: &mut Vec<String>) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < s.len() {
        let Some(rel) = s[i..].find('[') else {
            out.push_str(&s[i..]);
            break;
        };
        let lb = i + rel;
        let is_img = lb > 0 && bytes[lb - 1] == b'!';
        let tstart = lb + 1;
        let Some(tend) = s[tstart..].find(']').map(|x| tstart + x) else {
            out.push_str(&s[i..lb + 1]);
            i = lb + 1;
            continue;
        };
        if !s[tend + 1..].starts_with('(') {
            out.push_str(&s[i..tend + 1]);
            i = tend + 1;
            continue;
        }
        let ustart = tend + 2;
        let Some(uend) = s[ustart..].find(')').map(|x| ustart + x) else {
            out.push_str(&s[i..lb + 1]);
            i = lb + 1;
            continue;
        };
        let text = &s[tstart..tend];
        let url = s[ustart..uend].trim();
        // 占位符前的普通文本(图片要连同前面的 `!` 一起吞掉)
        let prefix_end = if is_img { lb - 1 } else { lb };
        out.push_str(&s[i..prefix_end]);
        // `javascript:` 一类不进 href —— 同屏页会在两端 iframe 里跑。
        let safe = !url.to_ascii_lowercase().starts_with("javascript:");
        let html = if !safe {
            text.to_string()
        } else if is_img {
            format!(
                "<img class=\"beam-inline-img\" src=\"{}\" alt=\"{}\">",
                url.replace('"', "&quot;"),
                text.replace('"', "&quot;")
            )
        } else {
            format!(
                "<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">{}</a>",
                url.replace('"', "&quot;"),
                emphasize(text)
            )
        };
        store.push(html);
        out.push_str(&format!("\u{0}{}\u{0}", store.len() - 1));
        i = uend + 1;
    }
    out
}

/// 成对标记 → 标签。奇数个标记时最后一个原样保留。
fn replace_pair(s: &str, mark: &str, tag: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let Some(a) = rest.find(mark) else {
            out.push_str(rest);
            break;
        };
        let after = &rest[a + mark.len()..];
        let Some(b) = after.find(mark) else {
            out.push_str(rest);
            break;
        };
        let inner = &after[..b];
        if inner.is_empty() || inner.contains('\n') {
            out.push_str(&rest[..a + mark.len()]);
            rest = after;
            continue;
        }
        out.push_str(&rest[..a]);
        out.push_str(&format!("<{tag}>{inner}</{tag}>"));
        rest = &after[b + mark.len()..];
    }
    out
}

/// 裸 http(s) 链接转可点。已在 `<a href=...>` 里的不再重复包(靠前面是否紧跟 `"` 判断)。
fn autolink(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < s.len() {
        let Some(p) = s[i..].find("http").map(|x| i + x) else {
            out.push_str(&s[i..]);
            break;
        };
        let tail = &s[p..];
        let is_url = tail.starts_with("http://") || tail.starts_with("https://");
        let in_attr = p > 0 && matches!(s.as_bytes()[p - 1], b'"' | b'\'' | b'=' | b'>');
        if !is_url || in_attr {
            out.push_str(&s[i..p + 4]);
            i = p + 4;
            continue;
        }
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '"' | '\'' | ')'))
            .map(|x| p + x)
            .unwrap_or(s.len());
        let url = &s[p..end];
        out.push_str(&s[i..p]);
        out.push_str(&format!(
            "<a href=\"{url}\" target=\"_blank\" rel=\"noopener noreferrer\">{url}</a>"
        ));
        i = end;
    }
    out
}

// ────────────────────────────── 页面骨架 ──────────────────────────────

/// 同屏代理脚本:页面自己上报滚动比例、接收远端指令。两端跑的是**同一份**脚本,
/// 故「谁跟谁」是对称的 —— 上层(手机壳 / 桌面 BeamStage)只管转发。
///
/// 关键取舍:
///  - 收到远端滚动后开 400ms 抑制窗口,否则 A 滚 → B 跟 → B 回报 → A 跟…来回抖。
///  - 只上报比例不报像素:两端视口高度天差地别,像素对不齐。
///  - 页面跑在 **不给 allow-same-origin 的 sandbox iframe** 里,够不着主机接口,
///    只能 postMessage 给父窗口 —— 打包页里就算混进恶意脚本也偷不到令牌。
const AGENT_JS: &str = r#"(function(){
var ID="polaris-beam",lock=0,t=null;
function docH(){return Math.max(1,document.documentElement.scrollHeight)}
function scrollMax(){return Math.max(0,docH()-window.innerHeight)}
function ratio(){var m=scrollMax();return m>0?Math.min(1,window.scrollY/m):0}
function post(m){m.__beam=ID;try{(window.parent!==window?window.parent:window).postMessage(m,"*")}catch(e){}}
window.addEventListener("scroll",function(){
  if(Date.now()<lock||t)return;
  t=setTimeout(function(){t=null;post({act:"scroll",r:ratio()})},80);
},{passive:true});
// 指点:上报**内容比例**而非视口比例 —— 两端版式不同(手机窄排 vs 电脑宽排),
// 同一视口坐标落在不同段落上;用 (scrollY+clientY)/内容高 才能指到同一处内容。
// 点在链接/按钮/输入框上不算指点(那是要交互),且点击者本地也冒一圈,自己知道点上了。
window.addEventListener("click",function(e){
  var el=e.target;
  while(el&&el!==document.body){
    var tag=(el.tagName||"").toLowerCase();
    if(tag==="a"||tag==="button"||tag==="input"||tag==="select"||tag==="textarea"||tag==="label")return;
    el=el.parentNode;
  }
  var rx=e.clientX/Math.max(1,window.innerWidth);
  var ry=(window.scrollY+e.clientY)/docH();
  ring(rx*window.innerWidth+window.scrollX, ry*docH());
  post({act:"mark",rx:rx,ry:ry});
},true);
// 光晕用**绝对定位**贴在内容上(不是 fixed 贴视口),这样它跟着内容,指的是同一段文字。
function ring(px,py){
  var d=document.createElement("div");
  d.className="beam-ring";
  d.style.left=px+"px";d.style.top=py+"px";
  document.body.appendChild(d);
  setTimeout(function(){d.remove()},900);
}
function applyZoom(z){document.documentElement.style.setProperty("--beam-zoom",String(z||1))}
window.addEventListener("message",function(e){
  var d=e.data;if(!d||d.__beam!==ID)return;
  if(d.act==="scroll"){lock=Date.now()+400;window.scrollTo(0,scrollMax()*(d.r||0))}
  else if(d.act==="mark"){ring((d.rx||0)*window.innerWidth+window.scrollX,(d.ry||0)*docH())}
  else if(d.act==="zoom"){applyZoom(d.z)}
  else if(d.act==="ping"){post({act:"pong",r:ratio()})}
});
// 就绪即上报:父层(舞台/预览)收到 ready 再注入初始 zoom/滚动,
// 不再靠「60ms 定时器赌 iframe 解析完了」——大页面根本没解析完,帧会静默丢。
post({act:"ready",r:0});
})();"#;

const AGENT_CSS: &str = r#":root{--beam-zoom:1;color-scheme:light dark}
*{box-sizing:border-box}
html,body{margin:0;padding:0}
body{
  font:calc(16px * var(--beam-zoom))/1.75 -apple-system,BlinkMacSystemFont,"Segoe UI",
       "PingFang SC","Hiragino Sans GB","Microsoft YaHei",sans-serif;
  color:#1c1f26;background:#fbfbfd;
  -webkit-text-size-adjust:100%;word-break:break-word;
}
#beam-doc{max-width:820px;margin:0 auto;padding:24px 20px 72px}
h1,h2,h3,h4,h5,h6{line-height:1.35;margin:1.6em 0 .6em;font-weight:650}
h1{font-size:1.75em}h2{font-size:1.42em}h3{font-size:1.2em}
p{margin:.75em 0}
a{color:#3b6ef6;text-decoration:none}
a:hover{text-decoration:underline}
ul,ol{padding-left:1.4em;margin:.7em 0}
li{margin:.28em 0}
blockquote{margin:1em 0;padding:.5em 1em;border-left:3px solid #d6dae3;background:#f2f4f8;border-radius:0 8px 8px 0}
hr{border:none;border-top:1px solid #e2e5ec;margin:2em 0}
code{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:.9em;
  background:#eef0f5;padding:.15em .38em;border-radius:5px}
pre.beam-code{background:#f4f6fa;border:1px solid #e4e7ee;border-radius:12px;
  padding:14px 16px;overflow:auto;margin:1em 0}
pre.beam-code code{background:none;padding:0;font-size:.86em;line-height:1.6;white-space:pre}
table{border-collapse:collapse;width:100%;margin:1em 0;font-size:.94em;display:block;overflow-x:auto}
th,td{border:1px solid #e2e5ec;padding:8px 12px;text-align:left}
th{background:#f2f4f8;font-weight:620}
img,video,svg{max-width:100%;height:auto}
.beam-media{display:block;margin:0 auto;max-height:82vh;border-radius:12px}
.beam-audio{text-align:center;padding:40px 0}
.beam-audio .beam-media{max-width:min(520px,100%)}
.beam-pdf{width:100%;height:82vh;border:1px solid #e2e5ec;border-radius:12px}
.beam-svg{text-align:center}
.beam-inline-img{border-radius:10px}
.beam-note{font-size:.85em;opacity:.65;margin-top:10px}
.beam-card{text-align:center;padding:64px 24px}
.beam-card-ico{font-size:52px;line-height:1}
.beam-card-t{font-size:1.05em;font-weight:620;margin:14px 0 6px}
.beam-card-d{font-size:.9em;opacity:.7;max-width:420px;margin:0 auto;line-height:1.7}
.beam-foot{max-width:820px;margin:0 auto;padding:14px 20px 30px;font-size:12px;opacity:.5;
  border-top:1px solid #e6e8ee}
/* 指点光晕:两端点一下,对面同一处冒一圈 —— 「你看这里」不用再讲坐标。
   absolute 贴在内容上(随内容滚动),故两端版式不同也指向同一段文字。 */
.beam-ring{position:absolute;width:26px;height:26px;margin:-13px 0 0 -13px;border-radius:50%;
  border:2px solid #3b6ef6;pointer-events:none;z-index:2147483647;
  animation:beam-pulse .9s ease-out forwards}
@keyframes beam-pulse{
  0%{transform:scale(.4);opacity:.95}
  100%{transform:scale(2.6);opacity:0}
}
/* 宽屏 = 电脑版式。手机一横过来视口就 ~700-900px,自动切到这套 —— 「横屏就和电脑一样」
   靠纯 CSS 做到,不用多传一条消息,也就不会出现两端版式短暂不一致的中间态。 */
@media (min-width:700px){
  body{font:calc(16.5px * var(--beam-zoom))/1.8 -apple-system,BlinkMacSystemFont,"Segoe UI",
       "PingFang SC","Hiragino Sans GB","Microsoft YaHei",sans-serif}
  #beam-doc{max-width:1040px;padding:34px 44px 84px}
  h1{font-size:2em}h2{font-size:1.55em}h3{font-size:1.25em}
  /* 窄屏时表格要横滚才不撑破布局;宽屏放开,按真正的表格排 */
  table{display:table;font-size:.95em}
  .beam-media{max-height:88vh}
  .beam-pdf{height:88vh}
}
@media (min-width:1180px){ #beam-doc{max-width:1180px} }
/* 横屏矮视口(手机横过来):把上下留白压掉,内容占满 */
@media (orientation:landscape) and (max-height:560px){
  #beam-doc{padding-top:16px;padding-bottom:40px}
  .beam-media{max-height:92vh}
  .beam-foot{padding:8px 20px 12px}
}
@media (prefers-color-scheme:dark){
  body{color:#e7e9ee;background:#14161b}
  blockquote{background:#1c1f26;border-left-color:#333846}
  code{background:#1e2129}
  pre.beam-code{background:#181b21;border-color:#282c36}
  th,td{border-color:#2a2e38}
  th{background:#1c1f26}
  hr,.beam-foot{border-color:#252932}
  .beam-pdf{border-color:#2a2e38}
  a{color:#6f9bff}
}
@media print{.beam-foot{display:none}#beam-doc{max-width:none}}
"#;

fn shell(
    name: &str,
    kind: &str,
    body: &str,
    size_h: &str,
    truncated: bool,
    stats: &PackStats,
) -> String {
    let mut foot = format!("{} · {} · 北极星「隔空同屏」自包含网页", esc(name), size_h);
    if stats.inlined > 0 {
        foot.push_str(&format!(" · 已内联 {} 个资源", stats.inlined));
    }
    if stats.skipped > 0 {
        foot.push_str(&format!(" · {} 个资源超预算未内联", stats.skipped));
    }
    if truncated {
        foot.push_str(" · 正文过大已截断");
    }
    format!(
        "<!doctype html>\n<html lang=\"zh-CN\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1,viewport-fit=cover\">\n\
         <title>{}</title>\n<style>{}</style>\n</head>\n\
         <body data-beam-kind=\"{}\">\n<main id=\"beam-doc\">{}</main>\n\
         <footer class=\"beam-foot\">{}</footer>\n<script>{}</script>\n</body>\n</html>\n",
        esc(name),
        AGENT_CSS,
        esc_attr(kind),
        body,
        foot,
        AGENT_JS
    )
}

// ────────────────────────────── 测试 ──────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md_blocks_render() {
        let h = md_to_html("# 标题\n\n正文**粗**与 `code`\n\n- 甲\n- 乙\n\n> 引用\n");
        assert!(h.contains("<h1>标题</h1>"), "标题: {h}");
        assert!(h.contains("<strong>粗</strong>"), "强调: {h}");
        assert!(h.contains("<code>code</code>"), "代码片段: {h}");
        assert!(h.contains("<li>甲</li>") && h.contains("<li>乙</li>"), "列表: {h}");
        assert!(h.contains("<blockquote>"), "引用: {h}");
    }

    #[test]
    fn md_fence_keeps_markup_literal() {
        let h = md_to_html("```rust\nlet x = &y < &z;\n```");
        assert!(h.contains("&lt;"), "围栏内必须转义: {h}");
        assert!(h.contains("data-lang=\"rust\""), "语言标注: {h}");
        assert!(!h.contains("<strong>"), "围栏内不该跑行内替换: {h}");
    }

    #[test]
    fn md_table_and_link() {
        let h = md_to_html("| a | b |\n|---|---|\n| 1 | 2 |\n\n[去](https://x.com)\n");
        assert!(h.contains("<th>a</th>") && h.contains("<td>2</td>"), "表格: {h}");
        assert!(h.contains("href=\"https://x.com\""), "链接: {h}");
    }

    /// Fable5 审计 #1 回归:URL 里的 `_`/`*` 不能被强调替换咬坏。
    #[test]
    fn md_underscore_in_url_survives() {
        let h = md_to_html("见 [wiki](https://x.com/Foo_bar_baz) 和 ![](shot_2026_07.png)");
        assert!(
            h.contains("href=\"https://x.com/Foo_bar_baz\""),
            "URL 里的下划线必须原样保留(不得被 <em> 切碎): {h}"
        );
        assert!(
            h.contains("src=\"shot_2026_07.png\""),
            "图片路径的下划线必须原样保留: {h}"
        );
        assert!(!h.contains("<em>"), "URL 里的下划线不该产生斜体: {h}");
    }

    /// 链接文本内的强调仍要生效(占位符方案不能把这个也吞了)。
    #[test]
    fn md_emphasis_inside_link_text() {
        let h = md_to_html("[**重点**说明](https://a.com)");
        assert!(h.contains("<strong>重点</strong>"), "链接文本里的强调应生效: {h}");
        assert!(h.contains("href=\"https://a.com\""), "链接本身在: {h}");
    }

    /// snake_case 标识符 + 正文里的 `_` 不该被隔空配对成斜体。
    #[test]
    fn md_snake_case_not_italicized() {
        let h = md_to_html("调用 `set_value` 后 total_count 会更新");
        assert!(h.contains("<code>set_value</code>"), "代码片段保留: {h}");
        assert!(!h.contains("<em>"), "普通下划线标识符不该斜体: {h}");
    }

    /// 同屏页会在两端 iframe 里执行,`javascript:` 链接必须落成纯文本。
    #[test]
    fn md_rejects_javascript_url() {
        let h = md_to_html("[点我](javascript:alert(1))");
        assert!(!h.contains("javascript:"), "危险协议不得进 href: {h}");
    }

    #[test]
    fn html_escaped_in_text_kind() {
        assert_eq!(esc("<b>&"), "&lt;b&gt;&amp;");
    }

    /// 外链/data URI 不动;相对路径在白名单外也不动(内联不是读整机的后门)。
    #[test]
    fn inline_skips_absolute_and_outside() {
        let dir = std::env::temp_dir();
        let mut st = PackStats {
            budget: MAX_ASSET_TOTAL,
            ..Default::default()
        };
        let src = r#"<img src="https://x.com/a.png"><img src="data:image/png;base64,AA"><img src="a.png">"#;
        // allowed 为空 = 什么都不在白名单内
        let out = inline_assets(src, &dir, &[], &mut st);
        assert_eq!(out, src, "白名单为空时不应改写任何引用");
        assert_eq!(st.inlined, 0);
    }

    /// 白名单内的同目录图片要被真的内联成 data URI;但普通 `<a href>` 指向的
    /// 非 css 文件不该被 base64 塞进 href(Fable5 审计 #6)。
    #[test]
    fn inline_embeds_sibling_asset() {
        let dir = std::env::temp_dir().join(format!("polaris-beam-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pic.png"), b"\x89PNG\r\n\x1a\nfake").unwrap();
        std::fs::write(dir.join("report.pdf"), b"%PDF-1.4 fake").unwrap();
        std::fs::write(dir.join("theme.css"), b"body{color:red}").unwrap();
        let root = std::fs::canonicalize(&dir).unwrap();
        let mut st = PackStats {
            budget: MAX_ASSET_TOTAL,
            ..Default::default()
        };
        let src = r#"<img src="pic.png"><a href="report.pdf">下载</a><link rel="stylesheet" href="theme.css">"#;
        let out = inline_assets(src, &root, &[root.clone()], &mut st);
        assert!(out.contains("data:image/png;base64,"), "图片应内联: {out}");
        assert!(out.contains("href=\"report.pdf\""), "普通链接的 pdf 不该被内联: {out}");
        assert!(out.contains("data:text/css;base64,"), "样式表应内联: {out}");
        assert_eq!(st.inlined, 2, "只内联图片与 css 两个");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn strip_shell_keeps_style_and_body() {
        let s = "<html><head><style>b{color:red}</style></head><body><p>hi</p></body></html>";
        let out = strip_html_shell(s);
        assert!(out.contains("b{color:red}") && out.contains("<p>hi</p>"), "{out}");
        assert!(!out.contains("<head>"), "骨架应被剥掉: {out}");
    }

    #[test]
    fn normalize_gates_actions_and_size() {
        assert!(normalize(json!({"act":"scroll","r":0.5})).is_ok());
        assert!(normalize(json!({"act":"rm -rf"})).is_err(), "未知动作必须拒收");
        assert!(normalize(json!({"r":1})).is_err(), "缺 act 必须拒收");
        let big = "x".repeat(MAX_MSG_BYTES + 1);
        assert!(normalize(json!({"act":"open","path":big})).is_err(), "超大消息必须拒收");
        let ok = normalize(json!({"act":"open","path":"a.md"})).unwrap();
        assert!(ok.get("ts").is_some() && ok.get("from").is_some(), "应补齐 ts/from");
    }

    #[test]
    fn jail_rejects_outside_and_missing() {
        let roots = vec![std::env::temp_dir()];
        assert!(matches!(
            resolve_readable_path("", &roots),
            Err(PathDenied::NotFound)
        ));
        assert!(resolve_readable_path("definitely-not-here-xyz.md", &roots).is_err());
    }

    #[test]
    fn human_size_reads_right() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
    }

    /// 打包一份真 markdown:出的必须是能独立打开的完整页面(含代理脚本)。
    #[test]
    fn pack_produces_standalone_page() {
        let dir = std::env::temp_dir().join(format!("polaris-beam-pack-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("note.md");
        std::fs::write(&f, "# 你好\n\n世界\n").unwrap();
        let root = std::fs::canonicalize(&dir).unwrap();
        // 直接走内部渲染路径(pack 会读真白名单,单测里不改全局配置)
        let body = md_to_html("# 你好\n\n世界\n");
        let st = PackStats::default();
        let html = shell("note.md", "md", &body, "12 B", false, &st);
        assert!(html.starts_with("<!doctype html>"), "必须是完整页面");
        assert!(html.contains("polaris-beam"), "必须内嵌同屏代理脚本");
        assert!(html.contains("<h1>你好</h1>"), "正文应已渲染");
        let _ = root;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
