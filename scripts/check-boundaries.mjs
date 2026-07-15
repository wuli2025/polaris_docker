#!/usr/bin/env node
// 分仓规划 v2 · 红线断言(Phase 3/4 收口件)。任何一条越线 → 退出码 1,CI 直接红。
//  R1 前端包源码形态: packages/*/package.json 一旦存在(editor/wiki 组件包拆出时),
//     main 必须指向 .ts/.vue 源码且 vue 必须在 peerDependencies —— 预构建 dist 会
//     打散 tree-shaking、复制 Vue 实例(v2 文档 §2 的唯一前端性能红线)。
//  R2 巨石组件懒挂载: ArtifactEditor 必须经 defineAsyncComponent 挂载(独立 chunk
//     出首屏, Phase 0 实测 gzip 省 33KB),禁止退回同步 import。
//  R3 引擎同层禁互引(纵深防御): crates/ 各引擎 Cargo.toml 不得依赖同层引擎。
//     编译器已经保证(path 依赖不存在则编不过),这里防的是"未来有人加上这行依赖"。
import { readFileSync, readdirSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const fails = [];
const ok = (m) => console.log(`  PASS ${m}`);
const bad = (m) => { fails.push(m); console.error(`  FAIL ${m}`); };

// ── R1 前端包源码形态 ──
const pkgsDir = join(root, "packages");
if (existsSync(pkgsDir)) {
  for (const name of readdirSync(pkgsDir)) {
    const pj = join(pkgsDir, name, "package.json");
    if (!existsSync(pj)) continue;
    const pkg = JSON.parse(readFileSync(pj, "utf8"));
    const entry = pkg.main ?? pkg.module ?? "";
    if (/\.(ts|vue)$/.test(entry) && /src\//.test(entry)) ok(`R1 ${name}: 入口为源码形态 (${entry})`);
    else bad(`R1 ${name}: 入口必须指向 src/ 下 .ts/.vue 源码,现为 "${entry}"`);
    if (pkg.peerDependencies?.vue) ok(`R1 ${name}: vue 在 peerDependencies`);
    else bad(`R1 ${name}: vue 必须声明为 peerDependency(防双实例)`);
  }
} else {
  ok("R1 packages/ 尚未拆出组件包(拆出即自动生效)");
}

// ── R2 巨石组件懒挂载 ──
const src = join(root, "src");
let lazyHit = false, syncHit = [];
const walk = (dir) => {
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) { walk(p); continue; }
    if (!/\.(vue|ts)$/.test(e.name)) continue;
    const c = readFileSync(p, "utf8");
    if (/defineAsyncComponent\([\s\S]{0,120}?ArtifactEditor\.vue/.test(c)) lazyHit = true;
    if (/^\s*import\s+ArtifactEditor\s+from/m.test(c)) syncHit.push(p.slice(root.length + 1));
  }
};
walk(src);
if (lazyHit && syncHit.length === 0) ok("R2 ArtifactEditor 仅经 defineAsyncComponent 懒挂载");
else if (!lazyHit) bad("R2 找不到 ArtifactEditor 的 defineAsyncComponent 挂载点");
if (syncHit.length) bad(`R2 出现同步 import ArtifactEditor(会拖回首屏): ${syncHit.join(", ")}`);

// ── R3 引擎同层禁互引 ──
const engines = ["polaris-fable", "polaris-forge", "polaris-collab", "polaris-sandbox"];
const allowUp = { "polaris-wiki": ["polaris-fable"] }; // 3→2 唯一豁免
const cratesDir = join(root, "src-tauri", "crates");
for (const c of readdirSync(cratesDir)) {
  const toml = join(cratesDir, c, "Cargo.toml");
  if (!existsSync(toml)) continue;
  const deps = readFileSync(toml, "utf8");
  for (const peer of engines) {
    if (peer === c) continue;
    if (new RegExp(`^${peer}\\s*=`, "m").test(deps)) {
      if ((allowUp[c] ?? []).includes(peer)) ok(`R3 ${c} → ${peer}(3→2 获批豁免)`);
      else bad(`R3 ${c} 依赖了同层引擎 ${peer} —— 引擎联动必须由壳(polaris-app)编排`);
    }
  }
}
if (!fails.some((f) => f.startsWith("R3"))) ok("R3 引擎同层零互引");

console.log(fails.length ? `\n红线检查: ${fails.length} 处越线` : "\n红线检查: 全部通过");
process.exit(fails.length ? 1 : 0);
