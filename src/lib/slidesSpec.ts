/**
 * polaris.slides.json(传统PPT spec)→ 预览 HTML 的确定性渲染器。
 *
 * 与 Rust 端 forge_pptx_native.rs 同源同构:6 色板 / 9 版式一一对应,
 * 预览即导出(结构同源,不会预览一个样导出一个样)。纯函数,无副作用,
 * 产出完整 HTML 文档字符串喂 iframe srcdoc(sandbox=allow-scripts 下无脚本也可渲染)。
 */

export interface SlideSpec {
  version?: number;
  theme?: string;
  slides: SlidePage[];
}
export interface SlidePage {
  layout?: string;
  title?: string;
  subtitle?: string;
  kicker?: string;
  points?: (string | { text?: string; sub?: string[] })[];
  left?: SpecCol;
  right?: SpecCol;
  items?: {
    head?: string;
    body?: string;
    points?: (string | { text?: string; sub?: string[] })[];
    value?: string;
    label?: string;
    desc?: string;
  }[];
  steps?: { head?: string; body?: string }[];
  text?: string;
  by?: string;
  notes?: string;
}
export interface SpecCol {
  head?: string;
  points?: (string | { text?: string; sub?: string[] })[];
}

interface Palette {
  bg1: string; bg2: string; ink: string; muted: string;
  accent: string; card: string; cardLine: string;
}

/** 与 forge_pptx_native.rs 的 PALETTES 保持同步(色值一致)。 */
const PALETTES: Record<string, Palette> = {
  "ink-gold":     { bg1: "#16181D", bg2: "#1F232B", ink: "#F2F0E9", muted: "#A8A49A", accent: "#D4B06A", card: "#20242C", cardLine: "#2E333D" },
  "deep-space":   { bg1: "#0B0F1A", bg2: "#131A2A", ink: "#E8ECF6", muted: "#93A0B8", accent: "#7AA2F7", card: "#16203A", cardLine: "#263250" },
  "warm-paper":   { bg1: "#FAF6EE", bg2: "#F3EDE0", ink: "#3A2F25", muted: "#8A7E6F", accent: "#B3672A", card: "#FFFFFF", cardLine: "#E5DCCB" },
  "forest":       { bg1: "#F4F7F2", bg2: "#E9F0E7", ink: "#1E2A22", muted: "#6B7A6F", accent: "#2F7A4F", card: "#FFFFFF", cardLine: "#D7E2D6" },
  "tech-blue":    { bg1: "#FFFFFF", bg2: "#EEF3FA", ink: "#16324F", muted: "#5D7187", accent: "#1F6FD6", card: "#FFFFFF", cardLine: "#D8E2EE" },
  "minimal-white":{ bg1: "#FFFFFF", bg2: "#F6F5F0", ink: "#1F1F1F", muted: "#6B6B6B", accent: "#A07520", card: "#FFFFFF", cardLine: "#E6E3D8" },
};

/** spec 可用的原生色板 id 列表(给提示词/选择器用)。 */
export const NATIVE_THEMES = Object.keys(PALETTES);

function esc(s: unknown): string {
  return String(s ?? "")
    .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

function pointsHtml(points: SlidePage["points"], pal: Palette): string {
  if (!Array.isArray(points)) return "";
  const li: string[] = [];
  for (const p of points) {
    if (typeof p === "string") {
      li.push(`<li>${esc(p)}</li>`);
    } else if (p && typeof p === "object") {
      const subs = Array.isArray(p.sub) && p.sub.length
        ? `<ul class="sub">${p.sub.map((s) => `<li>${esc(s)}</li>`).join("")}</ul>`
        : "";
      li.push(`<li>${esc(p.text ?? "")}${subs}</li>`);
    }
  }
  return li.length ? `<ul class="pts" style="--acc:${pal.accent}">${li.join("")}</ul>` : "";
}

function headerHtml(title?: string): string {
  if (!title) return "";
  return `<h2 class="hd">${esc(title)}</h2><div class="rule"></div>`;
}

function slideHtml(sl: SlidePage, pal: Palette): string {
  const layout = sl.layout ?? "bullets";
  let inner = "";
  switch (layout) {
    case "title":
    case "closing": {
      const title = sl.title || (layout === "closing" ? "谢谢" : "");
      inner = `<div class="center">
        ${sl.kicker ? `<div class="kick">${esc(sl.kicker)}</div>` : ""}
        <h1>${esc(title)}</h1><div class="rule mid"></div>
        ${sl.subtitle ? `<p class="sub">${esc(sl.subtitle)}</p>` : ""}
      </div>`;
      break;
    }
    case "section":
      inner = `<div class="sect"><div class="bar"></div><div>
        ${sl.kicker ? `<div class="kick">${esc(sl.kicker)}</div>` : ""}
        <h1 class="sec-t">${esc(sl.title ?? "")}</h1></div></div>`;
      break;
    case "two-col": {
      const col = (c?: SpecCol) =>
        c ? `<div class="card">${c.head ? `<div class="chead">${esc(c.head)}</div>` : ""}${pointsHtml(c.points, pal)}</div>` : "";
      inner = `${headerHtml(sl.title)}<div class="cols">${col(sl.left)}${col(sl.right)}</div>`;
      break;
    }
    case "compare": {
      const items = Array.isArray(sl.items) ? sl.items.slice(0, 4) : [];
      const cards = items
        .map((it) => {
          const body = (it.body ?? "")
            .split("\n").filter((l) => l.trim())
            .map((l) => `<p>${esc(l.trim())}</p>`).join("");
          return `<div class="card">${it.head ? `<div class="chead">${esc(it.head)}</div>` : ""}${body}${pointsHtml(it.points, pal)}</div>`;
        })
        .join("");
      inner = `${headerHtml(sl.title)}<div class="cmp" style="--n:${items.length || 1}">${cards}</div>`;
      break;
    }
    case "stats": {
      const items = Array.isArray(sl.items) ? sl.items.slice(0, 4) : [];
      const cards = items
        .map(
          (it) => `<div class="card stat">
            ${it.value ? `<div class="num">${esc(it.value)}</div>` : ""}
            ${it.label ? `<div class="nlabel">${esc(it.label)}</div>` : ""}
            ${it.desc ? `<div class="ndesc">${esc(it.desc)}</div>` : ""}
          </div>`,
        )
        .join("");
      inner = `${headerHtml(sl.title)}<div class="cmp stats" style="--n:${items.length || 1}">${cards}</div>`;
      break;
    }
    case "timeline": {
      const steps = Array.isArray(sl.steps) ? sl.steps.slice(0, 5) : [];
      const cells = steps
        .map(
          (st, i) => `<div class="step"><div class="dot">${i + 1}</div>
            ${st.head ? `<div class="shead">${esc(st.head)}</div>` : ""}
            ${st.body
              ? `<div class="sbody">${st.body.split("\n").filter((l) => l.trim()).map((l) => `<p>${esc(l.trim())}</p>`).join("")}</div>`
              : ""}
          </div>`,
        )
        .join("");
      inner = `${headerHtml(sl.title)}<div class="tl" style="--n:${steps.length || 1}">${cells}</div>`;
      break;
    }
    case "quote":
      inner = `<div class="quote"><div class="qmark">“</div>
        <p class="qtext">${esc(sl.text ?? "")}</p>
        ${sl.by ? `<p class="qby">—— ${esc(sl.by)}</p>` : ""}</div>`;
      break;
    default:
      inner = `${headerHtml(sl.title)}${pointsHtml(sl.points, pal)}`;
  }
  return `<section class="sl">${inner}</section>`;
}

/** spec(对象或 JSON 字符串)→ 自包含预览 HTML。解析失败返回 null。 */
export function specPreviewHtml(spec: SlideSpec | string): string | null {
  let s: SlideSpec;
  try {
    s = typeof spec === "string" ? JSON.parse(spec) : spec;
  } catch {
    return null;
  }
  if (!s || !Array.isArray(s.slides) || !s.slides.length) return null;
  const pal = PALETTES[s.theme ?? ""] ?? PALETTES["minimal-white"];
  const slides = s.slides.map((sl) => slideHtml(sl, pal)).join("\n");
  return `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><style>
  *{box-sizing:border-box;margin:0}
  body{background:#3a3a3e;padding:18px;display:flex;flex-direction:column;gap:18px;
    font-family:"Segoe UI","Microsoft YaHei","PingFang SC",sans-serif}
  .sl{aspect-ratio:16/9;width:100%;max-width:980px;margin:0 auto;border-radius:8px;
    background:linear-gradient(180deg,${pal.bg1},${pal.bg2});color:${pal.ink};
    padding:4.4% 6.2%;overflow:hidden;position:relative;box-shadow:0 8px 26px rgba(0,0,0,.35)}
  .hd{font-size:clamp(17px,2.6vw,26px);font-weight:700}
  .rule{width:72px;height:4px;background:${pal.accent};margin:10px 0 16px}
  .rule.mid{margin:14px auto}
  .center{position:absolute;inset:0;display:flex;flex-direction:column;align-items:center;justify-content:center;text-align:center;padding:0 10%}
  .center h1{font-size:clamp(24px,4vw,40px);font-weight:800}
  .kick{color:${pal.accent};font-weight:700;font-size:clamp(10px,1.4vw,14px);letter-spacing:.18em;text-transform:uppercase;margin-bottom:12px}
  .sub{color:${pal.muted};font-size:clamp(12px,1.7vw,17px);margin-top:4px}
  .sect{position:absolute;inset:0;display:flex;align-items:center;gap:26px;padding:0 8%}
  .bar{width:8px;height:130px;background:${pal.accent};border-radius:2px;flex-shrink:0}
  .sec-t{font-size:clamp(22px,3.4vw,34px);font-weight:800;margin-top:6px}
  .pts{list-style:none;display:flex;flex-direction:column;gap:.55em;font-size:clamp(12px,1.7vw,17px)}
  .pts>li{padding-left:1.15em;position:relative}
  .pts>li::before{content:"•";color:var(--acc,${pal.accent});position:absolute;left:0;font-weight:700}
  .pts .sub{list-style:none;margin-top:.35em;display:flex;flex-direction:column;gap:.3em;color:${pal.muted};font-size:.86em}
  .pts .sub>li{padding-left:1.1em;position:relative}
  .pts .sub>li::before{content:"–";color:${pal.muted};position:absolute;left:0}
  .cols{display:grid;grid-template-columns:1fr 1fr;gap:3%}
  .cmp{display:grid;grid-template-columns:repeat(var(--n),1fr);gap:2.4%}
  .card{background:${pal.card};border:1px solid ${pal.cardLine};border-radius:10px;padding:5.5% 5%;min-height:0}
  .chead{color:${pal.accent};font-weight:700;font-size:clamp(12px,1.8vw,17px);margin-bottom:.6em}
  .card p{font-size:clamp(11px,1.5vw,14px);margin-bottom:.45em}
  .stats{margin-top:3%}
  .stat{display:flex;flex-direction:column;align-items:center;justify-content:center;text-align:center;padding:7% 4%}
  .num{color:${pal.accent};font-weight:800;font-size:clamp(24px,4.6vw,46px);line-height:1.1}
  .nlabel{font-weight:700;font-size:clamp(11px,1.7vw,16px);margin-top:.5em}
  .ndesc{color:${pal.muted};font-size:clamp(10px,1.4vw,12px);margin-top:.4em}
  .tl{display:grid;grid-template-columns:repeat(var(--n),1fr);gap:2.2%;position:relative;margin-top:4%}
  .tl::before{content:"";position:absolute;left:10%;right:10%;top:21px;height:3px;background:${pal.cardLine}}
  .step{display:flex;flex-direction:column;align-items:center;text-align:center;position:relative;z-index:1}
  .dot{width:44px;height:44px;border-radius:50%;background:${pal.accent};color:${pal.bg1};font-weight:800;font-size:18px;display:flex;align-items:center;justify-content:center}
  .shead{font-weight:700;font-size:clamp(11px,1.7vw,16px);margin-top:.7em}
  .sbody{color:${pal.muted};font-size:clamp(10px,1.4vw,13px);margin-top:.4em}
  .sbody p{margin-bottom:.3em}
  .quote{position:absolute;inset:0;display:flex;flex-direction:column;align-items:center;justify-content:center;text-align:center;padding:0 12%}
  .qmark{color:${pal.accent};font-size:clamp(48px,9vw,96px);font-weight:800;line-height:.6;align-self:flex-start;margin-left:-2%}
  .qtext{font-size:clamp(16px,2.6vw,26px);font-style:italic;margin-top:14px}
  .qby{color:${pal.muted};font-size:clamp(11px,1.5vw,15px);margin-top:18px}
  </style></head><body>${slides}</body></html>`;
}
