/* polaris :: find-browser.mjs
 *
 * 找一个本机已有的 Chromium 系浏览器给 Playwright 用 —— 不触发 Playwright 的自动下载。
 * 与 Rust 侧 forge.rs / forge_capture.rs 的 find_chromium 同一条优先级链，让 Node 脚本
 * 复用北极星已有的浏览器分发（app 通过 POLARIS_CHROMIUM* 注入 ureq 下载/自带的 headless-shell）：
 *
 *   1. 显式 env（app 注入；Docker 注入 headless-shell；用户也可手动指）
 *   2. 本机已装的 Edge / Chrome / Chromium 固定路径
 *   3. Playwright channel（按名字驱动系统 Edge/Chrome，仍不下载）
 *
 * 返回值直接展开进 chromium.launch(...)：要么 {executablePath}，要么 {channel}。
 * 全都没有时抛错（带可读指引），绝不退回"自动下载一个 chromium"。
 */
import { existsSync } from "node:fs";

// 容器/Linux(尤其 root) 下 chromium 必须关沙箱才能起；/dev/shm 常很小要绕开;无 GPU。
// 桌面 headless 截图带上也无害(标准 CI 参数)。
const SANDBOX_OFF = ["--no-sandbox", "--disable-dev-shm-usage", "--disable-gpu"];

export function findLocalBrowser() {
  const plat = process.platform;
  // 非 Win/Mac(即 Docker/Linux) 一律带关沙箱参数; Win/Mac 桌面不需要(也不削弱本机浏览器安全)。
  const argsFor = (fromEnv) => (fromEnv || plat === "linux" ? SANDBOX_OFF.slice() : []);

  // 1) 显式 env —— 优先 headless-shell（Docker / 自带），再通用 chromium 覆盖。
  //    env 提供的多半是容器/headless 浏览器(如 Docker 的 /usr/bin/chromium) → 必带关沙箱。
  for (const v of [
    process.env.POLARIS_CHROMIUM_HEADLESS_SHELL,
    process.env.POLARIS_CHROMIUM,
    process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH,
  ]) {
    if (v && existsSync(v)) return { executablePath: v, args: argsFor(true) };
  }

  // 2) 本机已装浏览器固定路径（与 Rust find_chromium 同名同路）
  const candidates =
    plat === "win32"
      ? [
          "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
          "C:/Program Files/Microsoft/Edge/Application/msedge.exe",
          "C:/Program Files/Google/Chrome/Application/chrome.exe",
          "C:/Program Files (x86)/Google/Chrome/Application/chrome.exe",
        ]
      : plat === "darwin"
        ? [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
          ]
        : [
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/microsoft-edge",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
          ];
  for (const p of candidates) {
    if (existsSync(p)) return { executablePath: p, args: argsFor(false) };
  }

  // 3) Playwright channel —— 让 Playwright 自己按名字找系统 Edge/Chrome（仍不下载二进制）
  return { channel: plat === "win32" ? "msedge" : "chrome", args: argsFor(false) };
}

/** 给日志用的一句话说明本次选了哪个浏览器。 */
export function describeBrowser(opt) {
  return opt.executablePath ? `本机浏览器 ${opt.executablePath}` : `系统 channel: ${opt.channel}`;
}
