#!/usr/bin/env node
/* polaris-deck-studio :: install-deps.mjs
 *
 * Installs the (optional) PPTX-export toolchain into THIS skill folder:
 *   - playwright (chromium)  → headless screenshots of each slide
 *   - pptxgenjs              → assembles a .pptx with one full-bleed image per slide
 *
 * Idempotent + best-effort. If it fails (offline / no npm), the HTML deck still works
 * and you can fall back to "print → PDF" (Ctrl+P) or the python-pptx `pptx` skill.
 *
 * Usage:  node install-deps.mjs
 */
import { execSync } from "node:child_process";
import { existsSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { findLocalBrowser, describeBrowser } from "./find-browser.mjs";

const here = dirname(fileURLToPath(import.meta.url));

function run(cmd, opts = {}) {
  console.log("→ " + cmd);
  // 关键：彻底禁用 Playwright 安装时的浏览器自动下载。
  // 我们只装 JS 库（playwright 包本体很小），浏览器一律用本机已装的 / app 经 ureq 分发的，
  // 由 find-browser.mjs 在运行时定位（与 Rust 侧 find_chromium 同一条链）。
  const env = { ...process.env, PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD: "1" };
  execSync(cmd, { stdio: "inherit", cwd: here, env, ...opts });
}

try {
  // Local package.json so deps land inside the skill (not polluting the cwd project).
  const pkg = join(here, "package.json");
  if (!existsSync(pkg)) {
    mkdirSync(here, { recursive: true });
    writeFileSync(pkg, JSON.stringify({ name: "polaris-deck-export", private: true, type: "module" }, null, 2));
  }

  const haveNodeModules = existsSync(join(here, "node_modules", "pptxgenjs")) &&
    existsSync(join(here, "node_modules", "playwright"));
  if (!haveNodeModules) {
    // PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1（run() 已注入）→ 装库不下浏览器
    run("npm install pptxgenjs playwright --no-audit --no-fund --loglevel=error");
  } else {
    console.log("✓ node deps already present");
  }

  // 不再 `npx playwright install chromium`（那会下载 ~150MB 浏览器）。
  // 改为探测本机/自带浏览器并报告——运行时由 find-browser.mjs 真正定位。
  const b = findLocalBrowser();
  if (b.executablePath) {
    console.log("✓ 将使用本机浏览器：" + b.executablePath);
  } else {
    console.log("ℹ 未找到固定路径浏览器，运行时将尝试系统 channel：" + b.channel);
    console.log("  （若失败，请安装 Edge/Chrome，或让 app 通过 POLARIS_CHROMIUM 指定；本工具不会自动下载）");
  }

  console.log("✓ deck-studio 导出工具就绪");
} catch (e) {
  console.error("✗ 依赖安装失败（HTML 演示不受影响，可用打印 PDF 兜底）：", e.message);
  process.exit(1);
}
