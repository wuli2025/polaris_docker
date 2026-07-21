/**
 * polaris.slides.json(传统PPT spec)→ 预览 HTML 的确定性渲染器。
 *
 * 与 Rust 端 forge_pptx_native.rs 同源同构:6 色板 / 11 版式一一对应,
 * 预览即导出(结构同源,不会预览一个样导出一个样)。纯函数,无副作用,
 * 产出完整 HTML 文档字符串喂 iframe srcdoc(sandbox=allow-scripts 下无脚本也可渲染)。
 *
 * 配图(image-full / image-text):srcdoc 是不透明源,**加载不了 file:// 本地路径**,
 * 故 `image` 字段必须由调用方预先换成 data URL(见 DeckStudio.resolveSpecImages);
 * 换不到就渲染成占位框——宁可显式标「配图待载入」,也不要预览悄悄少一张图让人以为导出也没有。
 * 图用 `object-fit:cover`,与 Rust 端的 `a:srcRect` cover 裁切同语义(同构的关键一环)。
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
  /** image-full / image-text 专用。spec 里是本地绝对路径;预览前会被换成 data URL。 */
  image?: string;
  /** image-text:图在哪半边,"left"(默认) | "right"。 */
  side?: string;
  /** image-text 文字侧的小标题。 */
  head?: string;
  /** freeform 专用:自由摆放的盒子(1280×720 逻辑 px 绝对定位)。 */
  boxes?: FreeBox[];
}

/**
 * freeform 盒子。与 pptx_native.rs 的 freeform 分支逐字段对齐(9 类 / 17 个 type 取值)。
 * 注意 `size` **不走 autofit** —— 引擎给多少画多少(只 clamp 4–400),预览必须照做,
 * 否则「预览帮你缩了、导出溢出」比不预览还糟。
 */
export interface FreeBox {
  type?: string;
  x?: number; y?: number; w?: number; h?: number;
  /** line/arrow/axis 的终点(默认 x2=x+w, y2=y)。 */
  x2?: number; y2?: number;
  /** circle/point 的半径:给了就以 (x,y) 为圆心。point 默认 6。 */
  r?: number;
  /** polyline/curve/polygon 的点集,[[x,y],…] 或 [{x,y},…],需 ≥2 点。 */
  points?: unknown;
  closed?: boolean;
  arrow?: boolean;
  dash?: boolean;
  /** 描边/文字色(色板词或 #hex)。 */
  color?: string;
  /** 填充色(可选;不给则空心)。 */
  fill?: string;
  /** 线宽 1–40,默认 3。 */
  width?: number;
  /** text 盒:单行文本 或 lines 多行数组。 */
  text?: string;
  lines?: string[];
  size?: number;
  align?: string;
  anchor?: string;
  bold?: boolean;
  italic?: boolean;
  /** scrim 的不透明度 0–100,默认 50。 */
  alpha?: number;
  /** image 盒:预览前会被换成 data URL(同固定版式的 image 字段)。 */
  image?: string;
  cover?: boolean;
  rounded?: boolean;
  /** 第 N 次单击时淡入(0/缺省=随页显示)。预览渲染全部盒子(= 动画播完的终态)。 */
  click?: number;
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

/** 原生色板的展示元数据(换肤 UI 用):中文名 + 取色,与 PALETTES 同步。 */
export const NATIVE_THEME_META: { id: string; name: string; bg: string; accent: string; ink: string }[] = [
  { id: "minimal-white", name: "极简白", bg: PALETTES["minimal-white"].bg1, accent: PALETTES["minimal-white"].accent, ink: PALETTES["minimal-white"].ink },
  { id: "warm-paper", name: "暖纸", bg: PALETTES["warm-paper"].bg1, accent: PALETTES["warm-paper"].accent, ink: PALETTES["warm-paper"].ink },
  { id: "forest", name: "森林", bg: PALETTES["forest"].bg1, accent: PALETTES["forest"].accent, ink: PALETTES["forest"].ink },
  { id: "tech-blue", name: "科技蓝", bg: PALETTES["tech-blue"].bg1, accent: PALETTES["tech-blue"].accent, ink: PALETTES["tech-blue"].ink },
  { id: "ink-gold", name: "墨金", bg: PALETTES["ink-gold"].bg1, accent: PALETTES["ink-gold"].accent, ink: PALETTES["ink-gold"].ink },
  { id: "deep-space", name: "深空", bg: PALETTES["deep-space"].bg1, accent: PALETTES["deep-space"].accent, ink: PALETTES["deep-space"].ink },
];

/**
 * 宽容解析:模型边写边存时 spec 常是「半个 JSON」。严格 parse 失败就打捞 ——
 * 扫 slides 数组里**已经完整闭合**的页对象逐个 parse,坏一页跳一页。
 * 这是「生成中逐页点亮」的地基:文件每落一批页,预览就先亮一批,不必等全文合法。
 */
export function parseSpecLoose(text: string): { spec: SlideSpec | null; partial: boolean } {
  try {
    const s = JSON.parse(text);
    if (s && Array.isArray(s.slides)) return { spec: s, partial: false };
  } catch {
    /* fallthrough → 打捞 */
  }
  const themeM = /"theme"\s*:\s*"([^"]*)"/.exec(text);
  const head = text.search(/"slides"\s*:\s*\[/);
  if (head < 0) return { spec: null, partial: true };
  const arrStart = text.indexOf("[", head);
  const slides: SlidePage[] = [];
  let depth = 0, inStr = false, escaped = false, objStart = -1;
  for (let i = arrStart + 1; i < text.length; i++) {
    const ch = text[i];
    if (inStr) {
      if (escaped) escaped = false;
      else if (ch === "\\") escaped = true;
      else if (ch === '"') inStr = false;
      continue;
    }
    if (ch === '"') { inStr = true; continue; }
    if (ch === "{") { if (depth === 0) objStart = i; depth++; }
    else if (ch === "}") {
      depth--;
      if (depth === 0 && objStart >= 0) {
        try { slides.push(JSON.parse(text.slice(objStart, i + 1))); } catch { /* 坏页跳过 */ }
        objStart = -1;
      }
    } else if (ch === "]" && depth === 0) break;
  }
  if (!slides.length) return { spec: null, partial: true };
  return { spec: { theme: themeM?.[1], slides }, partial: true };
}

function esc(s: unknown): string {
  return String(s ?? "")
    .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

/**
 * 可编辑标记:给承载文字的元素打 `data-e="<字段路径>"`,DeckViewer 据此把用户改的
 * 文字回写进 spec 的对应字段(见 setSpecText)。路径**相对单页**(slides[i] 之内),
 * 因为同一页 HTML 会同时被缩略图与舞台复用,页号由组件自己知道。
 * 只标**纯文字叶子**:标题/副标题/kicker/bullet/卡片头尾/数字/引文/freeform 文本盒。
 */
function de(path: string): string {
  return ` data-e="${path}"`;
}

/** 按 `a.0.b` 形式的路径取值(数组下标用数字段)。 */
function getPath(obj: any, path: string): any {
  return path.split(".").reduce((o, k) => (o == null ? o : o[k]), obj);
}

/**
 * 把用户编辑的文字写回单页 spec 的 `path` 字段。返回是否真的改动了。
 * 只写字符串叶子;路径不存在/类型不符就拒绝(宁可不改,也不要把 spec 写坏)。
 */
export function setSpecText(slide: any, path: string, value: string): boolean {
  const keys = path.split(".");
  const last = keys.pop();
  if (!last) return false;
  const host = keys.reduce((o, k) => (o == null ? o : o[k]), slide);
  if (host == null || typeof host !== "object") return false;
  const old = host[last];
  // points[] 里的裸字符串项 与 各种 string 字段:都只接受 string→string
  if (old !== undefined && typeof old !== "string") return false;
  if (String(old ?? "") === value) return false;
  host[last] = value;
  return true;
}

/** 该路径当前的文字(给编辑器做「没变就不写盘」判断)。 */
export function getSpecText(slide: any, path: string): string {
  const v = getPath(slide, path);
  return typeof v === "string" ? v : "";
}

// ───────── 自适应字号(与 pptx_native.rs 的 autofit 逐行同构)─────────
// 引擎按每页内容量反算字号(行少撑大、行多才收)。预览必须用**同一套算法与同一批常量**,
// 否则预览一个字号、导出另一个字号 —— 这正是「预览即导出」要防的事。
// 改这里等于改导出:两边任何一处调了,另一处必须同步。

const PT2PX = 96 / 72;
const LINE_H = 1.32;
/** bullet 悬挂缩进(marL=285750 EMU = 30px),算可用宽要扣。 */
const BULLET_INDENT = 30;

/** em 宽度:CJK/全角 1em,拉丁/数字约 0.55em。 */
function emWidth(s: unknown): number {
  let w = 0;
  for (const ch of String(s ?? "")) {
    const u = ch.codePointAt(0)!;
    const cjk =
      (u >= 0x2e80 && u <= 0x9fff) ||
      (u >= 0xf900 && u <= 0xfaff) ||
      (u >= 0xff00 && u <= 0xffef) ||
      (u >= 0x3000 && u <= 0x303f);
    w += cjk ? 1 : 0.55;
  }
  return w;
}

interface FitLine { em: number; rel: number; after: number }

/** 在 [min,max] 里挑最大的、能在 (w,h) px 盒内放下全部行的字号(pt)。 */
function autofit(lines: FitLine[], w: number, h: number, min: number, max: number): number {
  if (!lines.length) return max;
  for (let size = max; size >= min; size--) {
    let total = 0;
    for (const l of lines) {
      const s = Math.max(6, size + l.rel);
      const perRow = Math.max(1, w / (s * PT2PX));
      const rows = Math.max(1, Math.ceil(l.em / perRow));
      total += rows * s * LINE_H * PT2PX + l.after * PT2PX;
    }
    if (total <= h) return size;
  }
  return min;
}

/** points → 度量行(规则与 Rust 的 point_fit_lines / pointsHtml 一一对应)。 */
function pointFitLines(points: SlidePage["points"], out: FitLine[]): void {
  if (!Array.isArray(points)) return;
  for (const p of points) {
    if (typeof p === "string") {
      out.push({ em: emWidth(p), rel: 0, after: 8 });
    } else if (p && typeof p === "object") {
      if (p.text) out.push({ em: emWidth(p.text), rel: 0, after: 4 });
      for (const s of p.sub ?? []) out.push({ em: emWidth(s), rel: -3, after: 4 });
    }
  }
}

/**
 * pt → CSS。用 `cqw`(容器宽度百分比)而非 px/clamp:幻灯片容器宽 = 1280 逻辑 px 画布,
 * 故 S pt = S×(96/72) px = S×1.3333/1280×100 cqw —— **与导出像素级同构**,且预览缩放到
 * 任何尺寸都保持比例。旧代码用 clamp(a,Bvw,c) 只是「差不多大」,换算不过去。
 */
function fs(pt: number): string {
  return `font-size:${((pt * PT2PX * 100) / 1280).toFixed(3)}cqw`;
}

/**
 * points → HTML。`size` 由调用方 autofit 算好后传入(与导出同字号)。
 * `base` = 该 points 数组在单页 spec 里的路径前缀(如 `points` / `left.points`),
 * 用来给每个 bullet 打可编辑标记;不传则不可编辑。
 */
function pointsHtml(points: SlidePage["points"], pal: Palette, size: number, base?: string): string {
  if (!Array.isArray(points)) return "";
  const li: string[] = [];
  points.forEach((p, i) => {
    if (typeof p === "string") {
      // 裸字符串项:路径就是 `<base>.<i>`
      li.push(`<li${base ? de(`${base}.${i}`) : ""}>${esc(p)}</li>`);
    } else if (p && typeof p === "object") {
      // 子条降 3pt(与 Rust 的 rel:-3 一致)
      const subs = Array.isArray(p.sub) && p.sub.length
        ? `<ul class="sub" style="${fs(size - 3)}">${p.sub
            .map((s, j) => `<li${base ? de(`${base}.${i}.sub.${j}`) : ""}>${esc(s)}</li>`)
            .join("")}</ul>`
        : "";
      // 有子条时外层 li 不能整体可编辑(会把子条文字一起吞进 text),
      // 故把 text 单独包一层 span 挂标记。
      const txt = `<span${base ? de(`${base}.${i}.text`) : ""}>${esc(p.text ?? "")}</span>`;
      li.push(`<li>${txt}${subs}</li>`);
    }
  });
  return li.length
    ? `<ul class="pts" style="--acc:${pal.accent};${fs(size)}">${li.join("")}</ul>`
    : "";
}

/** 页题头。标题 autofit [22,32],与 Rust 的 header() 同界。 */
function headerHtml(title?: string): string {
  if (!title) return "";
  const size = autofit([{ em: emWidth(title), rel: 0, after: 0 }], 1120, 64, 22, 32);
  return `<h2 class="hd" style="${fs(size)}"${de("title")}>${esc(title)}</h2><div class="rule"></div>`;
}

function slideHtml(sl: SlidePage, pal: Palette): string {
  const layout = sl.layout ?? "bullets";
  let inner = "";
  switch (layout) {
    case "title":
    case "closing": {
      const title = sl.title || (layout === "closing" ? "谢谢" : "");
      inner = `<div class="center">
        ${sl.kicker ? `<div class="kick" style="${fs(17)}"${de("kicker")}>${esc(sl.kicker)}</div>` : ""}
        <h1 style="${fs(coverTitleSize(title))}"${de("title")}>${esc(title)}</h1><div class="rule mid"></div>
        ${sl.subtitle ? `<p class="sub" style="${fs(coverSubSize(sl.subtitle))}"${de("subtitle")}>${esc(sl.subtitle)}</p>` : ""}
      </div>`;
      break;
    }
    case "section": {
      const t = sl.title ?? "";
      const size = autofit([{ em: emWidth(t), rel: 0, after: 0 }], 1040, 90, 26, 44);
      inner = `<div class="sect"><div class="bar"></div><div>
        ${sl.kicker ? `<div class="kick" style="${fs(17)}"${de("kicker")}>${esc(sl.kicker)}</div>` : ""}
        <h1 class="sec-t" style="${fs(size)}"${de("title")}>${esc(t)}</h1></div></div>`;
      break;
    }
    case "two-col": {
      // 两栏共用同一字号(各栏算一次取小),与 Rust 的 col_size 同逻辑。
      const size = Math.min(
        ...[sl.left, sl.right].filter(Boolean).map((c) => {
          const fl: FitLine[] = [];
          if (c!.head) fl.push({ em: emWidth(c!.head), rel: 2, after: 8 });
          pointFitLines(c!.points, fl);
          return autofit(fl, 488 - BULLET_INDENT, 414, 13, 24);
        }),
        24
      );
      const col = (c: SpecCol | undefined, side: "left" | "right") =>
        c
          ? `<div class="card">${c.head ? `<div class="chead" style="${fs(size + 2)}"${de(`${side}.head`)}>${esc(c.head)}</div>` : ""}${pointsHtml(c.points, pal, size, `${side}.points`)}</div>`
          : "";
      inner = `${headerHtml(sl.title)}<div class="cols">${col(sl.left, "left")}${col(sl.right, "right")}</div>`;
      break;
    }
    case "compare": {
      const items = Array.isArray(sl.items) ? sl.items.slice(0, 4) : [];
      const n = Math.max(1, items.length);
      // Math.floor 不能省:Rust 那边 `(1120 - gap*(n-1)) / n` 是**整除**(3 卡 → 354 而非
      // 354.667)。这 0.667px 的差足以让 autofit 差出 1pt —— 实测预览 59 / 导出 58。
      const cw = Math.floor((1120 - 28 * (n - 1)) / n);
      const size = Math.min(
        ...items.map((it) => {
          const fl: FitLine[] = [];
          if (it.head) fl.push({ em: emWidth(it.head), rel: 3, after: 8 });
          for (const l of (it.body ?? "").split("\n").filter((x) => x.trim()))
            fl.push({ em: emWidth(l.trim()), rel: 0, after: 6 });
          pointFitLines(it.points, fl);
          return autofit(fl, cw - 48 - BULLET_INDENT, 382, 11, 22);
        }),
        22
      );
      const cards = items
        .map((it, i) => {
          // body 是「\n 分行的整块文本」,拆行只为排版;编辑要回写整块 → 标记打在容器上,
          // 由 DeckViewer 用 innerText 取回换行(逐行标记会把一行写成整个 body)。
          const lines = (it.body ?? "").split("\n").filter((l) => l.trim());
          const body = lines.length
            ? `<div${de(`items.${i}.body`)}>${lines.map((l) => `<p style="${fs(size)}">${esc(l.trim())}</p>`).join("")}</div>`
            : "";
          return `<div class="card">${it.head ? `<div class="chead" style="${fs(size + 3)}"${de(`items.${i}.head`)}>${esc(it.head)}</div>` : ""}${body}${pointsHtml(it.points, pal, size, `items.${i}.points`)}</div>`;
        })
        .join("");
      inner = `${headerHtml(sl.title)}<div class="cmp" style="--n:${items.length || 1}">${cards}</div>`;
      break;
    }
    case "stats": {
      const items = Array.isArray(sl.items) ? sl.items.slice(0, 4) : [];
      const n = Math.max(1, items.length);
      const cw = Math.floor((1120 - 28 * (n - 1)) / n); // 整除,同 Rust
      const cards = items
        .map((it, i) => {
          // value / desc 各自 autofit(与 Rust 同界),label 固定 20pt
          const vs = it.value ? autofit([{ em: emWidth(it.value), rel: 0, after: 0 }], cw - 40, 120, 22, 60) : 0;
          const ds = it.desc ? autofit([{ em: emWidth(it.desc), rel: 0, after: 0 }], cw - 40, 90, 10, 16) : 0;
          return `<div class="card stat">
            ${it.value ? `<div class="num" style="${fs(vs)}"${de(`items.${i}.value`)}>${esc(it.value)}</div>` : ""}
            ${it.label ? `<div class="nlabel" style="${fs(20)}"${de(`items.${i}.label`)}>${esc(it.label)}</div>` : ""}
            ${it.desc ? `<div class="ndesc" style="${fs(ds)}"${de(`items.${i}.desc`)}>${esc(it.desc)}</div>` : ""}
          </div>`;
        })
        .join("");
      inner = `${headerHtml(sl.title)}<div class="cmp stats" style="--n:${items.length || 1}">${cards}</div>`;
      break;
    }
    case "timeline": {
      const steps = Array.isArray(sl.steps) ? sl.steps.slice(0, 5) : [];
      const n = Math.max(1, steps.length);
      const sw = Math.floor((1120 - 24 * (n - 1)) / n); // 整除,同 Rust
      const size = Math.min(
        ...steps.map((st) => {
          const fl: FitLine[] = [];
          if (st.head) fl.push({ em: emWidth(st.head), rel: 3, after: 6 });
          for (const l of (st.body ?? "").split("\n").filter((x) => x.trim()))
            fl.push({ em: emWidth(l.trim()), rel: 0, after: 4 });
          return autofit(fl, sw, 320, 10, 20);
        }),
        20
      );
      const cells = steps
        .map(
          (st, i) => `<div class="step"><div class="dot">${i + 1}</div>
            ${st.head ? `<div class="shead" style="${fs(size + 3)}"${de(`steps.${i}.head`)}>${esc(st.head)}</div>` : ""}
            ${st.body
              ? `<div class="sbody" style="${fs(size)}"${de(`steps.${i}.body`)}>${st.body.split("\n").filter((l) => l.trim()).map((l) => `<p>${esc(l.trim())}</p>`).join("")}</div>`
              : ""}
          </div>`,
        )
        .join("");
      inner = `${headerHtml(sl.title)}<div class="tl" style="--n:${steps.length || 1}">${cells}</div>`;
      break;
    }
    case "quote": {
      const qs = autofit([{ em: emWidth(sl.text), rel: 0, after: 0 }], 960, 220, 18, 40);
      inner = `<div class="quote"><div class="qmark">“</div>
        <p class="qtext" style="${fs(qs)}"${de("text")}>${esc(sl.text ?? "")}</p>
        ${sl.by ? `<p class="qby" style="${fs(18)}">—— <span${de("by")}>${esc(sl.by)}</span></p>` : ""}</div>`;
      break;
    }
    case "image-full": {
      // 与 Rust 同构:全幅图 + 50% 黑蒙版 + 居中白字(白字恒定,不随色板)。
      const t = sl.title ?? "";
      inner = `<div class="ifull">${imgHtml(sl.image)}<div class="scrim"></div>
        <div class="center on-img">
          ${sl.kicker ? `<div class="kick on-img" style="${fs(17)}"${de("kicker")}>${esc(sl.kicker)}</div>` : ""}
          <h1 style="${fs(coverTitleSize(t))}"${de("title")}>${esc(t)}</h1><div class="rule mid"></div>
          ${sl.subtitle ? `<p class="sub on-img" style="${fs(coverSubSize(sl.subtitle))}"${de("subtitle")}>${esc(sl.subtitle)}</p>` : ""}
        </div></div>`;
      break;
    }
    case "image-text": {
      const right = String(sl.side ?? "").toLowerCase() === "right";
      const fl: FitLine[] = [];
      if (sl.head) fl.push({ em: emWidth(sl.head), rel: 2, after: 10 });
      pointFitLines(sl.points, fl);
      const size = autofit(fl, 544 - BULLET_INDENT, 446, 13, 30);
      const media = `<div class="ihalf">${imgHtml(sl.image)}</div>`;
      const txt = `<div class="ihalf txt">
        ${sl.head ? `<div class="chead" style="${fs(size + 2)}"${de("head")}>${esc(sl.head)}</div>` : ""}
        ${pointsHtml(sl.points, pal, size, "points")}</div>`;
      inner = `${headerHtml(sl.title)}<div class="cols">${right ? txt + media : media + txt}</div>`;
      break;
    }
    case "freeform": {
      // 自由版式:绝对定位盒子,不走 autofit(引擎同此)。此前这里没有分支 → freeform 页
      // 掉进 default 当 bullets 渲染,而 freeform 页没有 points → 预览一片空白、导出却好好的。
      inner = freeformHtml(sl, pal);
      break;
    }
    default: {
      // bullets(含未知版式降级):内容少就撑大,与 Rust 同界 [16,36]。
      const fl: FitLine[] = [];
      pointFitLines(sl.points, fl);
      const size = autofit(fl, 1120 - BULLET_INDENT, 470, 16, 36);
      // .fillbox:与 Rust 的 anchor="ctr" 对应 —— 内容放不满时垂直居中,不在下方留死白。
      inner = `${headerHtml(sl.title)}<div class="fillbox">${pointsHtml(sl.points, pal, size, "points")}</div>`;
    }
  }
  return `<section class="sl">${inner}</section>`;
}

/** 封面/结尾/全幅图页的主标题字号(三处共用,与 Rust 同界 [30,50])。 */
function coverTitleSize(title: string): number {
  return autofit([{ em: emWidth(title), rel: 0, after: 0 }], 1120, 110, 30, 50);
}
/** 封面副标题(与 Rust 同界 [14,24])。 */
function coverSubSize(sub: string): number {
  return autofit([{ em: emWidth(sub), rel: 0, after: 0 }], 960, 70, 14, 24);
}

/**
 * 配图 <img>。只认 data:/http(s) —— 本地绝对路径在 srcdoc 的不透明源里必然加载失败,
 * 与其渲染一个碎图图标,不如显式画占位框说明「预览未载入」(导出仍会有图)。
 */
function imgHtml(src?: string): string {
  const s = String(src ?? "").trim();
  if (!s) return "";
  if (/^(data:|https?:)/i.test(s)) return `<img class="pic" src="${esc(s)}" alt=""/>`;
  const name = s.split(/[\\/]/).pop() ?? s;
  return `<div class="pic ph"><span>配图待载入<br/>${esc(name)}</span></div>`;
}

// ───────── freeform(与 pptx_native.rs 的 freeform 分支同构)─────────
// 这一段**没有排版数学** —— freeform 本来就是绝对定位、不走 autofit,所以预览只需
// 把盒子按 x/y/w/h 画到 1280×720 画布上。坐标一律换成百分比:.sl 是 16:9 容器,
// left=x/1280 与 top=y/720 落点一致,缩放到任何尺寸都不漂。

/** 与 Rust norm_color 同构:色板词 / #RRGGBB / #RGB → CSS 颜色;认不出退 fallback。 */
function normColor(raw: unknown, pal: Palette, fallback: string): string {
  const t = String(raw ?? "").trim();
  if (!t) return fallback;
  switch (t.toLowerCase()) {
    case "ink": case "text": return pal.ink;
    case "muted": return pal.muted;
    case "accent": return pal.accent;
    case "card": return pal.card;
    case "line": return pal.cardLine;
    case "bg": case "bg1": return pal.bg1;
    case "bg2": return pal.bg2;
    case "white": return "#FFFFFF";
    case "black": return "#000000";
  }
  const hex = t.replace(/^#/, "");
  if (/^[0-9a-f]{6}$/i.test(hex)) return `#${hex.toUpperCase()}`;
  if (/^[0-9a-f]{3}$/i.test(hex)) return `#${hex.split("").map((c) => c + c).join("").toUpperCase()}`;
  return fallback;
}

const num = (v: unknown, d: number): number => {
  const n = typeof v === "number" ? v : Number(v);
  return Number.isFinite(n) ? Math.round(n) : d;
};
const clamp = (n: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, n));
/** x→% (画布宽 1280) / y→% (画布高 720)。 */
const px = (n: number) => `${((n * 100) / 1280).toFixed(4)}%`;
const py = (n: number) => `${((n * 100) / 720).toFixed(4)}%`;

/** points 解析:[[x,y],…] / [{x,y},…] / "x,y x,y" 都吃(与 Rust parse_points 宽容度对齐)。 */
function parsePts(raw: unknown): [number, number][] {
  const out: [number, number][] = [];
  if (Array.isArray(raw)) {
    for (const p of raw) {
      if (Array.isArray(p) && p.length >= 2) out.push([Number(p[0]) || 0, Number(p[1]) || 0]);
      else if (p && typeof p === "object") {
        const o = p as { x?: unknown; y?: unknown };
        if (o.x !== undefined && o.y !== undefined) out.push([Number(o.x) || 0, Number(o.y) || 0]);
      }
    }
  } else if (typeof raw === "string") {
    for (const pair of raw.trim().split(/\s+/)) {
      const [a, b] = pair.split(",");
      if (a !== undefined && b !== undefined) out.push([Number(a) || 0, Number(b) || 0]);
    }
  }
  return out;
}

/** 一个覆盖整页画布的 SVG 层(每盒一个 → DOM 顺序 = 绘制顺序 = 与导出的 z 序一致)。 */
function svgLayer(inner: string, defs = ""): string {
  return `<svg class="ff-svg" viewBox="0 0 1280 720" preserveAspectRatio="none">${defs}${inner}</svg>`;
}

function freeBoxHtml(b: FreeBox, pal: Palette, i: number): string {
  const x = num(b.x, 0), y = num(b.y, 0);
  const w = Math.max(1, num(b.w, 100)), h = Math.max(1, num(b.h, 100));
  const stroke = normColor(b.color, pal, pal.ink);
  const sw = clamp(num(b.width, 3), 1, 40);
  const fill = b.fill && String(b.fill).length ? normColor(b.fill, pal, pal.accent) : null;
  const click = Math.max(0, num(b.click, 0));
  // data-click:预览渲染全部盒子(动画终态),但把分步信息留给将来的放映/分步预览。
  const dc = click > 0 ? ` data-click="${click}"` : "";
  const box = (style: string, inner = "") =>
    `<div class="ff-b" style="left:${px(x)};top:${py(y)};width:${px(w)};height:${py(h)};${style}"${dc}>${inner}</div>`;

  switch (String(b.type ?? "")) {
    case "line": case "arrow": case "axis": {
      const x2 = num(b.x2, x + w), y2 = num(b.y2, y);
      const arrow = b.type === "arrow" || b.type === "axis" || b.arrow === true;
      const id = `ah${i}`;
      const defs = arrow
        ? `<defs><marker id="${id}" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="5" markerHeight="5" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="${stroke}"/></marker></defs>`
        : "";
      const dash = b.dash === true ? ` stroke-dasharray="${sw * 3},${sw * 2}"` : "";
      const mk = arrow ? ` marker-end="url(#${id})"` : "";
      return `<div class="ff-b ff-full"${dc}>${svgLayer(
        `<line x1="${x}" y1="${y}" x2="${x2}" y2="${y2}" stroke="${stroke}" stroke-width="${sw}" stroke-linecap="round"${dash}${mk}/>`,
        defs
      )}</div>`;
    }
    case "polyline": case "curve": case "polygon": {
      const pts = parsePts(b.points);
      if (pts.length < 2) return ""; // 与引擎一致:点不足只跳过该盒,不毁整页
      const closed = b.type === "polygon" || b.closed === true;
      const d = pts.map(([a, c]) => `${a},${c}`).join(" ");
      const tag = closed ? "polygon" : "polyline";
      return `<div class="ff-b ff-full"${dc}>${svgLayer(
        `<${tag} points="${d}" fill="${closed && fill ? fill : "none"}" stroke="${stroke}" stroke-width="${sw}" stroke-linejoin="round" stroke-linecap="round"/>`
      )}</div>`;
    }
    case "ellipse": case "circle": {
      const r = num(b.r, 0);
      const [ex, ey, ew, eh] = r > 0 ? [x - r, y - r, 2 * r, 2 * r] : [x, y, w, h];
      return `<div class="ff-b ff-full"${dc}>${svgLayer(
        `<ellipse cx="${ex + ew / 2}" cy="${ey + eh / 2}" rx="${ew / 2}" ry="${eh / 2}" fill="${fill ?? "none"}" stroke="${stroke}" stroke-width="${sw}"/>`
      )}</div>`;
    }
    case "point": case "dot": {
      const r = Math.max(1, num(b.r, 6));
      const c = fill ?? stroke;
      return `<div class="ff-b ff-full"${dc}>${svgLayer(
        `<circle cx="${x}" cy="${y}" r="${r}" fill="${c}" stroke="${c}" stroke-width="1"/>`
      )}</div>`;
    }
    case "text": {
      const size = clamp(num(b.size, 18), 4, 400);
      const color = normColor(b.color, pal, pal.ink);
      const align =
        ["center", "ctr", "c"].includes(String(b.align)) ? "center"
        : ["right", "r", "end"].includes(String(b.align)) ? "right"
        : "left";
      const anchor =
        ["middle", "center", "ctr"].includes(String(b.anchor)) ? "center"
        : ["bottom", "b"].includes(String(b.anchor)) ? "flex-end"
        : "flex-start";
      // lines 多行:逐行可编辑(各是独立字段);text 单行:整盒可编辑
      const multi = Array.isArray(b.lines);
      const src = multi ? b.lines!.filter((l) => typeof l === "string") : [b.text];
      const body = src
        .filter((t) => t !== undefined && t !== null)
        .map((t, j) => `<p${multi ? de(`boxes.${i}.lines.${j}`) : de(`boxes.${i}.text`)}>${esc(t)}</p>`)
        .join("");
      if (!body) return "";
      return box(
        `${fs(size)};color:${color};text-align:${align};justify-content:${anchor};` +
          `${b.bold ? "font-weight:700;" : ""}${b.italic ? "font-style:italic;" : ""}`,
        body
      );
    }
    case "rect": case "bar":
      return box(`background:${normColor(b.color, pal, pal.accent)}`);
    case "card":
      return box(`background:${pal.card};border:1px solid ${pal.cardLine};border-radius:10px`);
    case "scrim": {
      const a = clamp(num(b.alpha, 50), 0, 100);
      return box(`background:${normColor(b.color, pal, "#000000")};opacity:${a / 100}`);
    }
    case "image": case "pic": {
      const s = String(b.image ?? "").trim();
      const rounded = b.rounded === true ? "border-radius:10px;" : "";
      // 与固定版式同一条约定:srcdoc 加载不了 file://,路径必须已被换成 data URL,
      // 否则显式画占位框(宁可标「待载入」,也不要预览悄悄少一张图)。
      if (/^(data:|https?:)/i.test(s)) {
        const fit = b.cover === false ? "fill" : "cover";
        return box(`${rounded}overflow:hidden`, `<img src="${esc(s)}" alt="" style="width:100%;height:100%;object-fit:${fit};display:block"/>`);
      }
      const name = s.split(/[\\/]/).pop() ?? "";
      return box(
        `${rounded}background:${pal.card};border:1px dashed ${pal.cardLine};color:${pal.muted};` +
          `display:flex;align-items:center;justify-content:center;text-align:center;font-size:1.25cqw;line-height:1.6`,
        `<span>配图待载入<br/>${esc(name)}</span>`
      );
    }
    default:
      return ""; // 未知 type:与引擎一致,跳过该盒
  }
}

function freeformHtml(sl: SlidePage, pal: Palette): string {
  const boxes = Array.isArray(sl.boxes) ? sl.boxes : [];
  return `<div class="ff">${boxes.map((b, i) => freeBoxHtml(b, pal, i)).join("")}</div>`;
}

/** spec 对象或 JSON 字符串 → 校验过的 SlideSpec(失败 null)。 */
function coerceSpec(spec: SlideSpec | string): SlideSpec | null {
  let s: SlideSpec;
  try {
    s = typeof spec === "string" ? JSON.parse(spec) : spec;
  } catch {
    return null;
  }
  if (!s || !Array.isArray(s.slides) || !s.slides.length) return null;
  return s;
}

/**
 * 单页内部的全部样式(预览/播放器/缩略图共用)。.sl 只写「页面本体」规则,
 * 不写宽度/阴影等排场规则 —— 那些由各 chrome(纵览/播放器)自己按场景补。
 * container-type:inline-size —— fs() 用 cqw 把 pt 精确换算成容器宽度百分比,
 * 使预览字号与导出像素级同构,且**同一份 HTML 缩到 150px 缩略图字号自动等比**。
 */
function slideBaseCss(pal: Palette): string {
  return `
  .sl{aspect-ratio:16/9;border-radius:8px;container-type:inline-size;
    background:linear-gradient(180deg,${pal.bg1},${pal.bg2});color:${pal.ink};
    padding:4.4% 6.2%;overflow:hidden;position:relative}
  /* 字号一律由内联 style 的 fs() 给(autofit 算出),此处只管字重/颜色/间距 */
  .hd{font-weight:700}
  .rule{width:72px;height:4px;background:${pal.accent};margin:10px 0 16px}
  .rule.mid{margin:14px auto}
  .center{position:absolute;inset:0;display:flex;flex-direction:column;align-items:center;justify-content:center;text-align:center;padding:0 10%}
  .center h1{font-weight:800}
  .kick{color:${pal.accent};font-weight:700;letter-spacing:.18em;text-transform:uppercase;margin-bottom:12px}
  .sub{color:${pal.muted};margin-top:4px}
  .sect{position:absolute;inset:0;display:flex;align-items:center;gap:26px;padding:0 8%}
  .bar{width:8px;height:130px;background:${pal.accent};border-radius:2px;flex-shrink:0}
  .sec-t{font-weight:800;margin-top:6px}
  .pts{list-style:none;display:flex;flex-direction:column;gap:.55em}
  .pts>li{padding-left:1.15em;position:relative}
  .pts>li::before{content:"•";color:var(--acc,${pal.accent});position:absolute;left:0;font-weight:700}
  /* 子条字号由内联 fs(size-3) 给(与 Rust 的 rel:-3 一致),这里不再用 .86em 相对缩放 */
  .pts .sub{list-style:none;margin-top:.35em;display:flex;flex-direction:column;gap:.3em;color:${pal.muted}}
  .pts .sub>li{padding-left:1.1em;position:relative}
  .pts .sub>li::before{content:"–";color:${pal.muted};position:absolute;left:0}
  .cols{display:grid;grid-template-columns:1fr 1fr;gap:3%}
  /* 与 Rust 的 anchor="ctr" 对应:autofit 之后仍放不满时垂直居中,不在下方留死白。
     bullets 文本框 y=176..646(470px)→ 占满题头以下的整块内容区。 */
  .fillbox{position:absolute;left:6.2%;right:6.2%;top:24.4%;height:65.3%;
    display:flex;flex-direction:column;justify-content:center}
  .card{justify-content:center}
  .ihalf.txt{justify-content:center}
  /* 卡片高度对齐 Rust 的固定画法(画布 1280 宽 = 100cqw,故 Npx = N/12.8 cqw):
     two-col 470px / compare 430px / stats 320px。不钉死的话卡片会跟着内容缩,
     预览成了「卡随字变」而导出是「卡固定、字居中」—— 两回事。 */
  .cols>.card{height:36.72cqw}
  .cmp>.card{height:33.59cqw}
  .cmp.stats>.card{height:25cqw}
  /* 配图:object-fit:cover ≡ Rust 端 a:srcRect 的 cover 裁切(等比填满+对称裁切,不变形) */
  .pic{width:100%;height:100%;object-fit:cover;display:block;border-radius:8px}
  .pic.ph{display:flex;align-items:center;justify-content:center;text-align:center;
    background:${pal.card};border:1px dashed ${pal.cardLine};color:${pal.muted};
    font-size:1.25cqw;line-height:1.6;border-radius:8px}
  .ifull{position:absolute;inset:0}
  .ifull .pic{height:100%;border-radius:0}
  .scrim{position:absolute;inset:0;background:rgba(0,0,0,.5)}
  .center.on-img h1{color:#fff}
  .kick.on-img{color:#fff}
  .sub.on-img{color:#E8E8E8}
  /* 图框几何对齐 Rust:544x470 px 图框(见 pptx_native.rs image-text 分支)。
     用 aspect-ratio 而非 height:100% —— grid 父容器没有确定高度,百分比高会塌成 0。 */
  .ihalf{min-height:0}
  .ihalf .pic{aspect-ratio:544/470;height:auto}
  .ihalf.txt{display:flex;flex-direction:column;padding-top:.4%}
  .cmp{display:grid;grid-template-columns:repeat(var(--n),1fr);gap:2.4%}
  .card{background:${pal.card};border:1px solid ${pal.cardLine};border-radius:10px;padding:5.5% 5%;min-height:0;
    display:flex;flex-direction:column}
  .chead{color:${pal.accent};font-weight:700;margin-bottom:.6em}
  .card p{margin-bottom:.45em}
  .stats{margin-top:3%}
  .stat{display:flex;flex-direction:column;align-items:center;justify-content:center;text-align:center;padding:7% 4%}
  .num{color:${pal.accent};font-weight:800;line-height:1.1}
  .nlabel{font-weight:700;margin-top:.5em}
  .ndesc{color:${pal.muted};margin-top:.4em}
  .tl{display:grid;grid-template-columns:repeat(var(--n),1fr);gap:2.2%;position:relative;margin-top:4%}
  .tl::before{content:"";position:absolute;left:10%;right:10%;top:21px;height:3px;background:${pal.cardLine}}
  .step{display:flex;flex-direction:column;align-items:center;text-align:center;position:relative;z-index:1}
  /* 圆点 44px @1280 画布 = 3.4375cqw;内数字 18pt(与 Rust circle_num 一致) */
  .dot{width:3.4375cqw;height:3.4375cqw;border-radius:50%;background:${pal.accent};color:${pal.bg1};font-weight:800;font-size:1.875cqw;display:flex;align-items:center;justify-content:center}
  .shead{font-weight:700;margin-top:.7em}
  .sbody{color:${pal.muted};margin-top:.4em}
  .sbody p{margin-bottom:.3em}
  .quote{position:absolute;inset:0;display:flex;flex-direction:column;align-items:center;justify-content:center;text-align:center;padding:0 12%}
  /* 引号装饰 96pt(与 Rust 一致,非 autofit 项);正文/出处字号走内联 fs() */
  .qmark{color:${pal.accent};font-size:10cqw;font-weight:800;line-height:.6;align-self:flex-start;margin-left:-2%}
  .qtext{font-style:italic;margin-top:14px}
  .qby{color:${pal.muted};margin-top:18px}
  /* ── freeform ── .sl 有 padding:4.4% 6.2%,而 freeform 的坐标是相对整张 1280×720 画布的,
     故 .ff 用 absolute inset:0 顶掉 padding,让 left/top 的百分比直接落在画布原点上。 */
  .ff{position:absolute;inset:0;overflow:hidden}
  .ff-b{position:absolute;display:flex;flex-direction:column}
  .ff-b>p{margin:0}
  /* 线/折线/圆/点走 SVG:viewBox 与画布同为 1280×720,preserveAspectRatio="none" 下
     容器也是 16:9 → 不会变形,且 stroke-width 的用户单位 = 画布 px,与引擎线宽同尺度。 */
  .ff-full{left:0;top:0;width:100%;height:100%;pointer-events:none}
  .ff-svg{width:100%;height:100%;overflow:visible;display:block}`;
}

/** spec(对象或 JSON 字符串)→ 自包含纵览 HTML(全部页竖排滚动)。解析失败返回 null。 */
export function specPreviewHtml(spec: SlideSpec | string): string | null {
  const s = coerceSpec(spec);
  if (!s) return null;
  const pal = PALETTES[s.theme ?? ""] ?? PALETTES["minimal-white"];
  const slides = s.slides.map((sl) => slideHtml(sl, pal)).join("\n");
  return `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><style>
  *{box-sizing:border-box;margin:0}
  body{background:#3a3a3e;padding:18px;display:flex;flex-direction:column;gap:18px;
    font-family:"Segoe UI","Microsoft YaHei","PingFang SC",sans-serif}
  .sl{width:100%;max-width:980px;margin:0 auto;box-shadow:0 8px 26px rgba(0,0,0,.35)}
  ${slideBaseCss(pal)}
  </style></head><body>${slides}</body></html>`;
}

/**
 * 逐页渲染(DeckViewer.vue 组件用):每页一段 <section class="sl">…</section> + 色板 CSS。
 * 之所以不用 specViewerHtml 的 iframe 方案:Tauri 主文档的 CSP 会被 srcdoc iframe
 * 继承,内联 <script> 一律被拦 —— 播放器 runtime 根本跑不起来。组件方案由 Vue 管
 * 状态,页面 HTML 走 v-html(内容全部经 esc() 转义,安全由构造保证),彻底绕开 CSP。
 */
export function specSlidesRender(
  spec: SlideSpec | string
): { pages: string[]; css: string; theme: string } | null {
  const s = coerceSpec(spec);
  if (!s) return null;
  const theme = PALETTES[s.theme ?? ""] ? (s.theme as string) : "minimal-white";
  const pal = PALETTES[theme];
  return { pages: s.slides.map((sl) => slideHtml(sl, pal)), css: slideBaseCss(pal), theme };
}

export interface DeckViewerOpts {
  /** 生成中:缩略图栏尾部加脉动占位「下一页生成中」。 */
  generating?: boolean;
  /** srcdoc 重建时恢复到的页码(0 起)。 */
  initialPage?: number;
  /** true=跳到最新一页(生成中逐页点亮的跟随感);false/缺省=停在 initialPage。 */
  follow?: boolean;
  /** postMessage 回传时带上的通道名 —— 工坊与右抽屉可能同屏各挂一个播放器,
   *  父窗口靠它分流页码,不然互相覆盖。缺省 ""。 */
  channel?: string;
}

/**
 * spec → 豆包式播放器 HTML:左缩略图栏 + 右大舞台 + 键盘/点击翻页 + 页码。
 * 自包含(内联 runtime,sandbox=allow-scripts 即可跑),同一份页面 HTML 渲一遍进
 * <template>,缩略图和舞台都从模板克隆 —— cqw 字号在小容器里自动等比,无需 transform。
 * 翻页时向父窗口 postMessage({type:"deck-page",page,user}) —— 父组件靠它在轮询重建
 * srcdoc 时恢复页码(iframe 是不透明源,父窗口读不了它的内部状态,只能靠消息)。
 */
export function specViewerHtml(spec: SlideSpec | string, opts: DeckViewerOpts = {}): string | null {
  const s = coerceSpec(spec);
  if (!s) return null;
  const pal = PALETTES[s.theme ?? ""] ?? PALETTES["minimal-white"];
  const slides = s.slides.map((sl) => slideHtml(sl, pal)).join("\n");
  const n = s.slides.length;
  const cfg = JSON.stringify({
    n,
    page: Math.max(0, Math.min(opts.initialPage ?? 0, n - 1)),
    follow: opts.follow === true,
    generating: opts.generating === true,
    channel: String(opts.channel ?? ""),
  }).replace(/</g, "\\u003c");
  return `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><style>
  *{box-sizing:border-box;margin:0}
  html,body{height:100%}
  body{display:flex;background:#26262b;overflow:hidden;
    font-family:"Segoe UI","Microsoft YaHei","PingFang SC",sans-serif}
  #rail{width:152px;flex-shrink:0;overflow-y:auto;padding:12px 10px;display:flex;
    flex-direction:column;gap:10px;background:rgba(0,0,0,.22)}
  #rail::-webkit-scrollbar{width:6px}
  #rail::-webkit-scrollbar-thumb{background:rgba(255,255,255,.18);border-radius:3px}
  .th{position:relative;cursor:pointer;border-radius:8px;outline:2px solid transparent;
    outline-offset:1px;transition:outline-color .15s;flex-shrink:0}
  .th:hover{outline-color:rgba(255,255,255,.35)}
  .th.on{outline-color:${pal.accent}}
  .th .sl{width:100%;border-radius:6px;pointer-events:none}
  .th-n{position:absolute;left:5px;top:5px;z-index:2;font-size:10px;line-height:1;
    padding:3px 6px;border-radius:4px;background:rgba(0,0,0,.55);color:#fff;font-weight:600}
  .th-pending{aspect-ratio:16/9;border-radius:6px;border:1.5px dashed rgba(255,255,255,.32);
    display:flex;align-items:center;justify-content:center;color:rgba(255,255,255,.6);
    font-size:11px;animation:dkpulse 1.25s ease-in-out infinite;flex-shrink:0}
  @keyframes dkpulse{50%{opacity:.4}}
  #main{flex:1;display:flex;flex-direction:column;min-width:0}
  #stage{flex:1;display:flex;align-items:center;justify-content:center;padding:20px;min-height:0;cursor:pointer}
  #stage .sl{width:min(100%,calc((100vh - 96px)*1.77778));box-shadow:0 12px 36px rgba(0,0,0,.45)}
  #bar{height:48px;flex-shrink:0;display:flex;align-items:center;justify-content:center;gap:16px;
    color:#c9c9cf;font-size:12.5px;user-select:none}
  #bar button{border:1px solid rgba(255,255,255,.22);background:rgba(255,255,255,.06);color:#e4e4e8;
    border-radius:7px;padding:5px 14px;font-size:12.5px;cursor:pointer}
  #bar button:hover{background:rgba(255,255,255,.14)}
  #bar button:disabled{opacity:.35;cursor:default}
  #bar .gen{color:${pal.accent};font-weight:600;animation:dkpulse 1.25s ease-in-out infinite}
  ${slideBaseCss(pal)}
  </style></head><body>
  <aside id="rail"></aside>
  <main id="main">
    <div id="stage" title="点击翻下一页 · 键盘 ←→ 翻页"></div>
    <div id="bar">
      <button id="prev">‹ 上一页</button>
      <span id="num"></span>
      <button id="next">下一页 ›</button>
      <span class="gen" id="genhint" hidden>生成中…</span>
    </div>
  </main>
  <template id="tpl">${slides}</template>
  <script>
  (function(){
    var CFG=${cfg};
    var tpl=document.getElementById("tpl");
    var slides=[].slice.call(tpl.content.children);
    var rail=document.getElementById("rail"),stage=document.getElementById("stage");
    var num=document.getElementById("num"),prev=document.getElementById("prev"),next=document.getElementById("next");
    if(CFG.generating)document.getElementById("genhint").hidden=false;
    var thumbs=slides.map(function(s,i){
      var t=document.createElement("div");t.className="th";
      var b=document.createElement("span");b.className="th-n";b.textContent=String(i+1);
      t.appendChild(b);t.appendChild(s.cloneNode(true));
      t.addEventListener("click",function(){go(i,true);});
      rail.appendChild(t);return t;
    });
    if(CFG.generating){
      var p=document.createElement("div");p.className="th-pending";
      p.textContent="下一页生成中…";rail.appendChild(p);
    }
    var page=-1;
    function go(i,user){
      var to=Math.max(0,Math.min(i,slides.length-1));
      if(to===page)return;
      page=to;
      stage.replaceChildren(slides[page].cloneNode(true));
      for(var k=0;k<thumbs.length;k++)thumbs[k].classList.toggle("on",k===page);
      num.textContent=(page+1)+" / "+slides.length;
      prev.disabled=page<=0;next.disabled=page>=slides.length-1;
      var th=thumbs[page];if(th&&th.scrollIntoView)th.scrollIntoView({block:"nearest"});
      try{parent.postMessage({type:"deck-page",channel:CFG.channel,page:page,user:!!user},"*");}catch(e){}
    }
    prev.addEventListener("click",function(){go(page-1,true);});
    next.addEventListener("click",function(){go(page+1,true);});
    stage.addEventListener("click",function(){go(page<slides.length-1?page+1:page,true);});
    document.addEventListener("keydown",function(e){
      if(e.key==="ArrowRight"||e.key==="ArrowDown"||e.key===" "||e.key==="PageDown"){e.preventDefault();go(page+1,true);}
      else if(e.key==="ArrowLeft"||e.key==="ArrowUp"||e.key==="PageUp"){e.preventDefault();go(page-1,true);}
      else if(e.key==="Home"){e.preventDefault();go(0,true);}
      else if(e.key==="End"){e.preventDefault();go(slides.length-1,true);}
    });
    go(CFG.follow?slides.length-1:CFG.page,false);
  })();
  <\/script></body></html>`;
}
