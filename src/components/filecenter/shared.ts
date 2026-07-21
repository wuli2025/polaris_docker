/**
 * 文件中心共享层:跨子组件复用的类型 / 常量 / 纯函数。
 * 全部自 FileCenter.vue 原样搬出,不改行为。
 */
import type { FileCard, FcCluster } from "../../tauri";

/** 文件中心的视图种类(画廊 / 分类树 / 列表 / 核心层 / 远程源)。 */
export type ViewKind = "gallery" | "clusters" | "list" | "core" | "remote";

// ───────────────────────── 配色 / 字形 ─────────────────────────
export const KIND_COLOR: Record<string, string> = {
  text: "#5fa8e6",
  doc: "#8b6cff",
  image: "#6fcf97",
  audio: "#e0a24b",
  video: "#e0736b",
  archive: "#93a0b4",
  other: "#8a8f98",
};
export const KIND_LABEL: Record<string, string> = {
  text: "文本",
  doc: "文档",
  image: "图片",
  audio: "音频",
  video: "视频",
  archive: "压缩包",
  other: "其它",
};
export const CODE_EXTS = new Set([
  "rs", "py", "js", "ts", "tsx", "jsx", "mjs", "vue", "go", "java", "c", "cpp", "h", "hpp",
  "rb", "php", "json", "jsonl", "html", "htm", "css", "sh", "ps1", "bat", "sql", "toml",
]);
export const TEXTY_EXTS = new Set(["md", "txt", "rst", "org", "tex", "log", "yaml", "yml", "xml", "ini", "cfg", "srt", "vtt"]);

// 「按语言归类」配色:自然语言/媒体给固定色,编程语言按名字哈希到一组高级色,稳定且区分度高。
export const LANG_FIXED: Record<string, string> = {
  中文: "#e0736b", 英文: "#5b8cff", 其他语种: "#9aa0e6", 未识别: "#8a8f98",
  图片: "#6fcf97", 视频: "#c264d6", 音频: "#e0a24b", 压缩包: "#93a0b4", 其他文件: "#8a8f98",
  "文档·待识别": "#b0b4bd",
};
export const LANG_PALETTE = [
  "#8b6cff", "#42c8d4", "#e08aae", "#7ec8a0", "#d49a6a", "#6cc0c0", "#cf9fd6", "#7f9cf5",
  "#d4b06a", "#b487e0", "#5fa8e6", "#e6a4c4",
];
export function langColor(lang: string): string {
  if (LANG_FIXED[lang]) return LANG_FIXED[lang];
  let h = 0;
  for (let i = 0; i < lang.length; i++) h = (h * 31 + lang.charCodeAt(i)) >>> 0;
  return LANG_PALETTE[h % LANG_PALETTE.length];
}

/** 卡片的 accent 色:优先取其语义簇的颜色,否则按类型配色。 */
export function accentFor(card: FileCard, clusterById: Record<number, FcCluster>): string {
  if (card.clusterId > 0 && clusterById[card.clusterId]) {
    return clusterById[card.clusterId].color;
  }
  return KIND_COLOR[card.kind] ?? KIND_COLOR.other;
}

export function glyphFor(card: FileCard): string {
  const k = card.kind;
  const e = card.ext.toLowerCase();
  if (k === "image") return "image";
  if (k === "video") return "video";
  if (k === "audio") return "audio";
  if (k === "archive") return "archive";
  if (e === "pdf") return "pdf";
  if (["xls", "xlsx", "csv", "tsv", "ods"].includes(e)) return "sheet";
  if (["ppt", "pptx"].includes(e)) return "slide";
  if (["doc", "docx"].includes(e)) return "doc";
  if (CODE_EXTS.has(e)) return "code";
  if (TEXTY_EXTS.has(e) || k === "text") return "text";
  if (k === "doc") return "doc";
  return "other";
}

// 自研科技感线性字形(thin 单线 + accent 高光,不落俗套)
export const GLYPHS: Record<string, string> = {
  text: `<path class="soft" d="M30 6 L38 14 H30 Z"/><path d="M16 6 H30 L38 14 V42 H16 Z"/><path d="M30 6 V14 H38"/><path d="M21 23 H33 M21 29 H33 M21 35 H28"/>`,
  doc: `<path class="soft" d="M30 6 L38 14 H30 Z"/><path d="M16 6 H30 L38 14 V42 H16 Z"/><path d="M30 6 V14 H38"/><path d="M21 24 H33 M21 30 H33 M21 36 H29"/>`,
  code: `<rect class="soft" x="8" y="11" width="32" height="26" rx="5"/><path d="M18 19 L12 24 L18 29"/><path d="M30 19 L36 24 L30 29"/><path class="acc" d="M27 16 L21 32"/>`,
  pdf: `<path class="soft" d="M30 6 L38 14 H30 Z"/><path d="M16 6 H30 L38 14 V42 H16 Z"/><path d="M30 6 V14 H38"/><rect class="fill" x="15" y="29" width="20" height="8" rx="2.5"/>`,
  sheet: `<rect class="soft" x="9" y="10" width="30" height="9" rx="3.5"/><rect x="9" y="10" width="30" height="28" rx="3.5"/><path d="M9 19 H39 M9 28.5 H39 M19 10 V38 M29 10 V38"/>`,
  slide: `<rect class="soft" x="8" y="10" width="32" height="22" rx="3.5"/><rect x="8" y="10" width="32" height="22" rx="3.5"/><path class="acc" d="M15 26 V22 M21 26 V17 M27 26 V20 M33 26 V14"/><path d="M19 32 L17 38 M29 32 L31 38 M16 38 H32"/>`,
  image: `<rect x="8" y="10" width="32" height="28" rx="3.5"/><circle cx="18" cy="19" r="3"/><path class="soft" d="M9 33 L18 25 L25 31 L31 24 L39 32 V35 a3 3 0 0 1-3 3 H12 a3 3 0 0 1-3-3 Z"/><path d="M9 33 L18 25 L25 31 L31 24 L39 32"/>`,
  video: `<rect class="soft" x="8" y="11" width="32" height="26" rx="5"/><rect x="8" y="11" width="32" height="26" rx="5"/><path class="fill" d="M21 18.5 L31 24 L21 29.5 Z"/>`,
  audio: `<path d="M11 22 V26" stroke-width="2.4"/><path d="M16 18 V30" stroke-width="2.4"/><path class="acc" d="M21 12 V36" stroke-width="2.4"/><path d="M26 16 V32" stroke-width="2.4"/><path class="acc" d="M31 13 V35" stroke-width="2.4"/><path d="M36 20 V28" stroke-width="2.4"/>`,
  archive: `<path class="soft" d="M24 9 L38 16 L24 23 L10 16 Z"/><path d="M24 9 L38 16 V32 L24 39 L10 32 V16 Z"/><path d="M10 16 L24 23 L38 16 M24 23 V39"/>`,
  other: `<path class="soft" d="M24 8 L38 16 V32 L24 40 L10 32 V16 Z"/><path d="M24 8 L38 16 V32 L24 40 L10 32 V16 Z"/><circle cx="24" cy="24" r="4"/>`,
};

// ───────────────────────── 辅助(纯函数) ─────────────────────────
export function fmtTime(sec: number): string {
  if (!sec) return "";
  const d = new Date(sec * 1000);
  const now = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  const hm = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  if (d.toDateString() === now.toDateString()) return `今天 ${hm}`;
  return `${d.getFullYear() === now.getFullYear() ? "" : d.getFullYear() + "/"}${pad(d.getMonth() + 1)}/${pad(d.getDate())} ${hm}`;
}
export function fmtBytes(b: number): string {
  const u = ["B", "KB", "MB", "GB", "TB"];
  let v = b,
    i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return i === 0 ? `${b} B` : `${v.toFixed(1)} ${u[i]}`;
}
export function nameOf(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

// 折叠状态持久化(横幅 / 分类 / 类型 / 语言的展开记忆)。
export function loadFc(key: string, def: boolean): boolean {
  try {
    const v = localStorage.getItem(key);
    return v === null ? def : v === "1";
  } catch {
    return def;
  }
}
export function saveFc(key: string, v: boolean) {
  try {
    localStorage.setItem(key, v ? "1" : "0");
  } catch {
    /* storage 不可用 */
  }
}
