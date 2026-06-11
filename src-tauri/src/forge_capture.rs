//! Polaris Forge · capture 板块(工业级化阶段 1)
//!
//! 替 `forge_video::render_deck_fx_video` / `forge_pptx::capture_slides` 里
//! 「每帧起 chromium CLI 进程」的 slow 路径。设计见 workspace/research-notes/task-a.md。
//!
//! 选型:
//!   chromiumoxide 0.9.1(主路,持久 CDP 6-10× 提速)
//!   headless_chrome 1.0.21(fallback tier2,desktop feature only)
//!   chrome-headless-shell(瘦 Chromium,~80-130MB vs 完整 250-300MB)
//!   完整 chromium CLI(--screenshot 单帧,fallback tier3)
//!   wry + WebView2(仅 Windows 兜底,failback tier4)
//!
//! 5 档 fallback 链(任务 a §8):
//!   1. chromiumoxide 持久 CDP 主路
//!   2. headless_chrome CDP 客户端(API 接近,迁移 < 1 天)
//!   3. chromium CLI --screenshot= 单帧(视频路径放弃 → 走文本分支)
//!   4. (Windows)wry + html2canvas 仅 PPT 兜底
//!   5. 报错 + 重试 3 次(指数退避 500ms/1s/2s) + 写 capture_errors.log
//!
//! 工业级必备(任务 a §1.6):
//!   - reconnect-on-stale wrapper:CDP WS 断自动 reconnect ≤ 3
//!   - target pool:4-8 Page 共享 1 Browser(避免每帧 new_page)
//!   - render-ready gate:Page.lifecycleEvent + __deckReady 标记防白屏
//!   - kill_tree 两段式(SIGTERM → 5s → SIGKILL)
//!
//! 本版骨架:结构 + 5 档 enum + 占位函数;真实 chromiumoxide 调用 P1.5 落。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureTier {
    /// 主路:chromiumoxide 持久 CDP
    CdpPersistent,
    /// tier2:headless_chrome CDP 客户端(desktop only)
    CdpHeadlessChrome,
    /// tier3:chromium CLI --screenshot 单帧(视频放弃,改 PPT 路径)
    CliSingleFrame,
    /// tier4:Windows wry + WebView2 + html2canvas(仅 PPT 兜底)
    WryWebView2,
    /// 全部失败
    Failed,
}

impl CaptureTier {
    pub fn name(self) -> &'static str {
        match self {
            Self::CdpPersistent => "chromiumoxide-cdp",
            Self::CdpHeadlessChrome => "headless_chrome-cdp",
            Self::CliSingleFrame => "chromium-cli",
            Self::WryWebView2 => "wry-webview2",
            Self::Failed => "failed",
        }
    }
}

/// capture 单帧结果
#[derive(Debug, Clone)]
pub struct CapturedFrame {
    pub png_bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub tier: CaptureTier,
    pub duration_ms: u64,
}

/// 浏览器实例配置
#[derive(Debug, Clone)]
pub struct BrowserConfig {
    pub headless: bool,             // 桌面/headless-shell → true;CloakBrowser 有头模式 → false
    pub disable_sandbox: bool,      // Docker 必须 true
    pub disable_dev_shm: bool,      // 兜底 shm_size 不足
    pub disable_gpu: bool,          // 容器内无 GPU 驱动
    pub user_data_dir: Option<PathBuf>,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            headless: true,
            disable_sandbox: cfg!(target_os = "linux"),  // Docker/linux 默认开
            disable_dev_shm: true,
            disable_gpu: cfg!(target_os = "linux"),
            user_data_dir: None,
        }
    }
}

/// 找浏览器二进制。优先级:POLARIS_CHROMIUM_HEADLESS_SHELL(Docker) > POLARIS_CHROMIUM > PATH
pub fn find_browser() -> Option<PathBuf> {
    for env in &["POLARIS_CHROMIUM_HEADLESS_SHELL", "POLARIS_CHROMIUM"] {
        if let Ok(p) = std::env::var(env) {
            if !p.is_empty() {
                let path = PathBuf::from(&p);
                if path.exists() { return Some(path); }
            }
        }
    }
    for name in &["chrome-headless-shell", "chromium", "chromium-browser", "chrome"] {
        if let Ok(out) = std::process::Command::new(name).arg("--version").output() {
            if out.status.success() { return Some(PathBuf::from(name)); }
        }
    }
    None
}

/// 5 档 fallback 链骨架(本期 stub:始终返 CliSingleFrame 等 forge_video 现有路径)
/// P1.5 真实实现:每档调对应 crate,失败自动降级
pub async fn capture_with_fallback(
    deck_path: &Path,
    out_png: &Path,
    width: u32,
    height: u32,
    full_page: bool,
) -> Result<CapturedFrame, String> {
    // tier1:chromiumoxide(本期 stub → 直接降 tier3)
    // P1.5(chromiumoxide 0.9.x 实际 API,非伪):
    //   use chromiumoxide::browser::{Browser, BrowserConfig};
    //   let (mut browser, mut handler) = Browser::launch(
    //       BrowserConfig::builder()
    //           .chrome_path(find_browser().ok_or(...)?)
    //           .args(["--no-sandbox", "--disable-dev-shm-usage", "--disable-gpu"])
    //           .build()
    //           .map_err(|e| format!("BrowserConfig: {e}"))?,
    //   ).await?;
    //   let page = browser.new_page("about:blank").await?;
    //   // 配套启 handler tokio 任务消费 Page.event 流
    //   tokio::spawn(async move { while let Some(_evt) = handler.next().await {} });
    //   let url = deck_url(deck_path);
    //   page.goto(url).await?;
    //   let png = page.screenshot(
    //       ScreenshotParams::builder()
    //           .format(ScreenshotFormat::Png)
    //           .build()
    //   ).await?;
    //   return Ok(CapturedFrame { png_bytes: png, ..., tier: CaptureTier::CdpPersistent });

    // tier3 fallback(本期直接走 forge_video 既有的 chromium CLI 路径)
    capture_cli_single_frame(deck_path, out_png, width, height, full_page).await
}

/// tier3:chromium CLI 单帧(本期走,与 forge_video::render_deck_fx_video 共享)
async fn capture_cli_single_frame(
    deck_path: &Path,
    out_png: &Path,
    width: u32,
    height: u32,
    _full_page: bool,
) -> Result<CapturedFrame, String> {
    let browser = find_browser().ok_or_else(|| "未找到 chromium/chrome".to_string())?;
    let start = std::time::Instant::now();
    let is_http = deck_path.to_string_lossy().starts_with("http");
    let file_base = if is_http {
        deck_path.to_string_lossy().to_string()
    } else {
        let abs = std::fs::canonicalize(deck_path)
            .map_err(|e| format!("找不到 deck {deck_path:?}: {e}"))?;
        format!("file://{}", abs.to_string_lossy().replace('\\', "/"))
    };
    let mut cmd = std::process::Command::new(&browser);
    cmd.args([
        "--headless=new",
        "--no-sandbox",
        "--disable-dev-shm-usage",
        "--disable-gpu",
        "--hide-scrollbars",
        &format!("--screenshot={}", out_png.to_string_lossy()),
        &format!("--window-size={width},{height}"),
        &format!("--virtual-time-budget=2000"),
    ]);
    cmd.arg(&file_base);
    let out = cmd.output().map_err(|e| format!("chromium 启动失败: {e}"))?;
    if !out.status.success() {
        return Err(format!("chromium exit {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).chars().take(400).collect::<String>()));
    }
    let bytes = std::fs::read(out_png).map_err(|e| format!("读截图失败: {e}"))?;
    Ok(CapturedFrame {
        png_bytes: bytes,
        width, height,
        tier: CaptureTier::CliSingleFrame,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

/// target pool 骨架(P1.5 落:4-8 Page 共享 1 Browser,round-robin 分片)
/// 本期 stub:1 Page 单跑
pub struct TargetPool {
    // P1.5:browser: chromiumoxide::Browser,
    // P1.5:pages: Vec<chromiumoxide::Page>,
    // P1.5:idx: usize,
}

impl TargetPool {
    /// 启动一个 Browser + N 个 Page 池
    pub fn new(_size: usize, _cfg: BrowserConfig) -> Result<Self, String> {
        // P1.5:Browser::launch(...).await + browser.new_page("about:blank").await × N
        Ok(Self {})
    }

    /// round-robin 拿一个 Page(用完放回)
    pub async fn next_page(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// 截一帧
    pub async fn screenshot(&mut self, _url: &str, _w: u32, _h: u32, _out: &Path) -> Result<CapturedFrame, String> {
        Err("TargetPool::screenshot P1.5 待实".into())
    }
}

/// kill_tree 两段式:SIGTERM → grace → SIGKILL
/// 用 libc::kilib 直接调 POSIX;依赖 unix-only,Windows 编译期跳过
#[cfg(unix)]
pub fn kill_tree(pid: u32, grace: Duration) -> std::io::Result<()> {
    use libc::{kill, killpg};
    unsafe {
        // SIGTERM 给整个进程组(假设子进程 setpgid 创建过)
        let _ = killpg(pid as i32, libc::SIGTERM);
        std::thread::sleep(grace);
        if kill(pid as i32, 0) == 0 {
            let _ = killpg(pid as i32, libc::SIGKILL);
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn kill_tree(_pid: u32, _grace: Duration) -> std::io::Result<()> {
    // Windows:taskkill /T /F 走 std::process::Command,本期 stub
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_browser_does_not_panic() {
        // 只探测,不假定结果
        let _ = find_browser();
    }

    #[test]
    fn capture_tier_name() {
        assert_eq!(CaptureTier::CdpPersistent.name(), "chromiumoxide-cdp");
        assert_eq!(CaptureTier::CliSingleFrame.name(), "chromium-cli");
        assert_eq!(CaptureTier::Failed.name(), "failed");
    }
}
