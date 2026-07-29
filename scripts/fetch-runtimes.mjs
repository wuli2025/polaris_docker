// 把「随安装包分发的运行时」抓到 src-tauri/runtime/,供 tauri.conf 的 bundle.resources 打进包里。
// 作为 tauri.conf.json 的 build.beforeBuildCommand 运行(`tauri build` 前),幂等:
// 已按 .stamp.json 里记的版本抓齐就秒退,不重复下载。
//
// 抓什么(对应「不再强制用户装 uv / PowerShell 7」):
//   · uv        —— Python 脚本的统一托管者(一个二进制管解释器+依赖)
//   · Python    —— python-build-standalone 便携解释器,`uv run` 免联网即可跑
//   · Git Bash  —— 仅 Windows。Claude Code 在 Windows 上跑 Bash 工具必须有 bash;
//                  内置一份 MinGit 后,用户既不用装 PowerShell 7 也不用装 Git。
//
// ★ MinGit 的两个坑(2026-07-23 实测,别想当然):
//   ① MinGit **没有 bash.exe**,只有 `usr/bin/sh.exe` —— 而它就是 bash 5.3 本体
//      (`sh.exe --version` 自报 GNU bash)。故这里显式复制出 `usr/bin/bash.exe` 与
//      `bin/bash.exe`,让 CLAUDE_CODE_GIT_BASH_PATH 指得过去。
//   ② 非登录 bash(`bash -c`)不读 /etc/profile → PATH 里没有 /usr/bin,
//      `ls/cat/grep/sed/awk` 全部找不到,更坏的是 `find`/`sort` 会**静默命中
//      System32 的同名 exe**(语义完全不同)。这个坑在 Rust 侧堵:见
//      doctor/bundled.rs 把 usr/bin、mingw64/bin 前插进 claude 子进程的 PATH。
//
// 下载源顺序与安装脚本(doctor/install.rs)同一套规矩:自家 R2 打头(发版时传上去、
// 字节数核对过、出站免费),再公共 GitHub 代理,最后 GitHub 直连。每个源都校验文件头
// 魔数 —— 代理挂掉时常回「200 + HTML 错误页」,只看「文件非空」会把错误页当安装包解压。

import {
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
  copyFileSync,
  rmSync,
  renameSync,
  openSync,
  readSync,
  closeSync,
  statSync,
} from "node:fs";
import { writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const srcTauri = resolve(here, "..", "src-tauri");
const outRoot = join(srcTauri, "runtime");

const isWin = process.platform === "win32";
const isMac = process.platform === "darwin";
if (!isWin && !isMac) {
  console.log(`[fetch-runtimes] 平台 ${process.platform} 不打包运行时,跳过`);
  process.exit(0);
}

// ── 版本锁 ────────────────────────────────────────────────────────────
// 改任何一个都必须把对应文件传进 R2 的 deps/(否则 R2 那跳 404,白白退化到公共代理):
//   wrangler r2 object put polaris-downloads/deps/<文件名> --file <本地路径> --remote
// uv 版本刻意与 doctor/install.rs 的 UV_VER 保持一致 —— 两处共用 R2 里同一份包。
const UV_VER = "0.11.29";
const PY_VER = "3.13.14";
const PY_TAG = "20260718"; // python-build-standalone release tag
// Git for Windows 的 tag 与资产版本号**命名不同**:tag 是 v2.55.0.windows.3,
// 而资产叫 MinGit-2.55.0.3-64-bit.zip。两个都得写,拼不出来。
const GIT_VER = "2.55.0.3"; // MinGit 资产版本号
const GIT_TAG = "v2.55.0.windows.3"; // 对应的 release tag

const DEPS_BASE = "https://llmwiki.cloud/downloads/deps";
const ZIP_MAGIC = "504b"; // PK
const GZ_MAGIC = "1f8b";

const arch = process.arch === "arm64" ? "arm64" : "x64";

/** 本次平台要抓的组件清单。dir=解压目标(runtime/ 下),strip=解压后要不要提层。 */
function plan() {
  const items = [];

  // uv —— 单文件二进制,zip 里就是 uv.exe/uvx.exe(或 uv/uvx),解压到 runtime/uv
  const uvAsset = isWin
    ? `uv-${arch === "arm64" ? "aarch64" : "x86_64"}-pc-windows-msvc.zip`
    : `uv-${arch === "arm64" ? "aarch64" : "x86_64"}-apple-darwin.tar.gz`;
  items.push({
    name: "uv",
    dir: "uv",
    asset: uvAsset,
    magic: isWin ? ZIP_MAGIC : GZ_MAGIC,
    minBytes: 5 * 1024 * 1024,
    urls: ghUrls("astral-sh/uv", UV_VER, uvAsset),
    // mac 的 tar.gz 内含一层 uv-<triple>/ 目录,要提层
    strip: isMac ? 1 : 0,
    check: isWin ? "uv.exe" : "uv",
  });

  // Python —— python-build-standalone,install_only 版解压出一层 python/
  const pyTriple = isWin
    ? `${arch === "arm64" ? "aarch64" : "x86_64"}-pc-windows-msvc`
    : `${arch === "arm64" ? "aarch64" : "x86_64"}-apple-darwin`;
  const pyAsset = `cpython-${PY_VER}+${PY_TAG}-${pyTriple}-install_only_stripped.tar.gz`;
  items.push({
    name: "python",
    dir: "python",
    asset: pyAsset,
    magic: GZ_MAGIC,
    minBytes: 10 * 1024 * 1024,
    urls: ghUrls("astral-sh/python-build-standalone", PY_TAG, pyAsset),
    strip: 1, // 去掉包内的 python/ 这一层,直接落到 runtime/python
    check: isWin ? "python.exe" : "bin/python3",
  });

  // Git Bash —— 仅 Windows。MinGit 解压即根目录含 cmd/ mingw64/ usr/
  if (isWin) {
    const gitAsset = `MinGit-${GIT_VER}-${arch === "arm64" ? "arm64" : "64-bit"}.zip`;
    items.push({
      name: "git",
      dir: "git",
      asset: gitAsset,
      magic: ZIP_MAGIC,
      minBytes: 20 * 1024 * 1024,
      urls: ghUrls("git-for-windows/git", GIT_TAG, gitAsset),
      strip: 0,
      check: "usr/bin/sh.exe",
    });
  }
  return items;
}

/** 自家 R2 打头,公共代理居中,GitHub 直连兜底 —— 与 doctor/install.rs 同一套顺序。 */
function ghUrls(repo, tag, asset) {
  const rel = `https://github.com/${repo}/releases/download/${tag}/${asset}`;
  return [
    `${DEPS_BASE}/${asset}`,
    `https://ghfast.top/${rel}`,
    `https://ghproxy.net/${rel}`,
    rel,
  ];
}

/** 读文件头 n 字节的十六进制 —— 用来识破「200 + HTML 错误页」。 */
function headHex(path, n = 2) {
  const fd = openSync(path, "r");
  try {
    const buf = Buffer.alloc(n);
    readSync(fd, buf, 0, n, 0);
    return buf.toString("hex");
  } finally {
    closeSync(fd);
  }
}

async function download(item, dest) {
  for (const url of item.urls) {
    try {
      console.log(`[fetch-runtimes] ${item.name} ← ${url}`);
      const res = await fetch(url, { redirect: "follow" });
      if (!res.ok) {
        console.log(`[fetch-runtimes]   HTTP ${res.status},换下一个源`);
        continue;
      }
      await writeFile(dest, Buffer.from(await res.arrayBuffer()));
      const size = statSync(dest).size;
      if (size < item.minBytes) {
        console.log(
          `[fetch-runtimes]   只有 ${(size / 1048576).toFixed(1)}MB(期望 ≥${(
            item.minBytes / 1048576
          ).toFixed(0)}MB),换下一个源`
        );
        continue;
      }
      const magic = headHex(dest);
      if (!magic.startsWith(item.magic)) {
        console.log(
          `[fetch-runtimes]   文件头 ${magic} 不是期望的 ${item.magic}(多半是代理的错误页),换下一个源`
        );
        continue;
      }
      console.log(`[fetch-runtimes]   ✓ ${(size / 1048576).toFixed(1)}MB`);
      return true;
    } catch (e) {
      console.log(`[fetch-runtimes]   失败: ${e.message},换下一个源`);
    }
  }
  return false;
}

/** 解压。Windows 的 tar.exe(libarchive)zip/tar.gz 通吃,比 Expand-Archive 快得多。 */
function extract(archivePath, destDir, strip) {
  mkdirSync(destDir, { recursive: true });
  const args = ["-xf", archivePath, "-C", destDir];
  if (strip > 0) args.push(`--strip-components=${strip}`);
  execFileSync("tar", args, { stdio: "inherit" });
}

/**
 * MinGit 的 sh.exe 就是 bash 本体,但没叫 bash.exe。
 * Claude Code 认的是 CLAUDE_CODE_GIT_BASH_PATH 指向的 bash —— 这里补出来。
 *
 * ★ 只能放在 `usr/bin/`,**不要**再往 `bin/` 放一份(2026-07-23 实测):
 *   msys 的 bash 依赖同目录的 `msys-2.0.dll`,而 `bin/` 下没有这个 dll →
 *   `bin/bash.exe` 裸跑直接 0xC0000135 (DLL_NOT_FOUND) 挂掉,只有在我们恰好把
 *   `usr/bin` 注入进 PATH 时才活得下来。那是个随时会塌的假象,不留。
 */
function materializeBash(gitDir) {
  const sh = join(gitDir, "usr", "bin", "sh.exe");
  if (!existsSync(sh)) {
    throw new Error(`MinGit 里找不到 usr/bin/sh.exe(${sh})—— 包结构变了?`);
  }
  const bash = join(gitDir, "usr", "bin", "bash.exe");
  copyFileSync(sh, bash);
  // 自证:复制出来的 bash **在不改 PATH 的前提下**能跑,且自报 bash
  const out = execFileSync(bash, ["--version"], { encoding: "utf8" });
  if (!/GNU bash/.test(out)) {
    throw new Error(`复制出的 bash.exe 不是 bash:${out.split("\n")[0]}`);
  }
  console.log(`[fetch-runtimes]   ✓ bash: ${out.split("\n")[0].trim()}`);
}

// ── 主流程 ────────────────────────────────────────────────────────────
const items = plan();
const stampPath = join(outRoot, ".stamp.json");
const want = {
  platform: process.platform,
  arch,
  uv: UV_VER,
  python: `${PY_VER}+${PY_TAG}`,
  ...(isWin ? { git: GIT_VER } : {}),
};

// 已抓齐且版本一致 → 秒退
if (existsSync(stampPath)) {
  try {
    const have = JSON.parse(readFileSync(stampPath, "utf8"));
    const same = JSON.stringify(have) === JSON.stringify(want);
    const intact = items.every((it) =>
      existsSync(join(outRoot, it.dir, it.check))
    );
    if (same && intact) {
      console.log("[fetch-runtimes] 运行时已就绪(版本一致),跳过下载");
      process.exit(0);
    }
  } catch {
    /* stamp 坏了当作没抓过 */
  }
}

mkdirSync(outRoot, { recursive: true });
const tmpDir = join(outRoot, ".tmp");
rmSync(tmpDir, { recursive: true, force: true });
mkdirSync(tmpDir, { recursive: true });

for (const item of items) {
  const target = join(outRoot, item.dir);
  if (existsSync(join(target, item.check))) {
    console.log(`[fetch-runtimes] ${item.name} 已在,跳过`);
    continue;
  }
  const archivePath = join(tmpDir, item.asset);
  if (!(await download(item, archivePath))) {
    console.error(
      `[fetch-runtimes] ${item.name} 所有下载源都失败。\n` +
        `  运行时缺失会让安装包退回到「让用户自己装」,这里直接失败,不出残包。\n` +
        `  可检查网络/代理后重跑:node scripts/fetch-runtimes.mjs`
    );
    process.exit(1);
  }
  const staging = join(tmpDir, `x-${item.dir}`);
  rmSync(staging, { recursive: true, force: true });
  extract(archivePath, staging, item.strip);
  if (!existsSync(join(staging, item.check))) {
    console.error(
      `[fetch-runtimes] ${item.name} 解压后缺 ${item.check} —— 包结构与预期不符,拒绝出残包。`
    );
    process.exit(1);
  }
  rmSync(target, { recursive: true, force: true });
  renameSync(staging, target);
  if (item.name === "git") materializeBash(target);
  console.log(`[fetch-runtimes] ${item.name} → ${target}`);
}

rmSync(tmpDir, { recursive: true, force: true });
writeFileSync(stampPath, JSON.stringify(want, null, 2));
console.log(`[fetch-runtimes] 全部就绪 → ${outRoot}`);
