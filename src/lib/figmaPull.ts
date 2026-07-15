/**
 * Figma 往返桥（回程转换器）：把 Figma REST `/v1/files/:key` 的节点树转成
 * 绝对定位 HTML —— 视觉级还原（位置/尺寸/填充/描边/圆角/阴影/文字/图片/矢量 SVG），
 * 不追求语义与动画（那些在去程 html.to.design 时就已经拍平了）。
 *
 * 策略：取第一页里面积最大的顶层 Frame 当页面；全部节点**拍平**成相对 Frame 原点的
 * 绝对定位盒子（文档顺序 = 叠放顺序，与 Figma 一致），容器只出自己的底盒不嵌套坐标，
 * 从根上避免逐层坐标换算出错。旋转的节点按外接框摆放（MVP 不还原旋转）。
 */

export interface FigmaPullData {
  doc: any;
  /** imageRef → data URI（Rust 侧已下载内嵌） */
  images: Record<string, string>;
}

/** 需要走 SVG 导出的矢量类节点（div 拼不出来的形状） */
const VECTORISH = new Set([
  "VECTOR",
  "BOOLEAN_OPERATION",
  "STAR",
  "REGULAR_POLYGON",
  "POLYGON",
  "ARROW",
]);
const CONTAINERISH = new Set([
  "FRAME",
  "GROUP",
  "COMPONENT",
  "COMPONENT_SET",
  "INSTANCE",
  "SECTION",
]);
const MAX_NODES = 1500;
const MAX_DEPTH = 8;

function walk(n: any, fn: (n: any) => void) {
  fn(n);
  for (const c of n?.children ?? []) walk(c, fn);
}

function pickFrame(doc: any): any | null {
  const canvas = (doc?.document?.children ?? []).find((c: any) => c.type === "CANVAS");
  const frames = (canvas?.children ?? []).filter(
    (c: any) => c.visible !== false && c.absoluteBoundingBox
  );
  if (!frames.length) return null;
  const area = (n: any) =>
    (n.absoluteBoundingBox?.width ?? 0) * (n.absoluteBoundingBox?.height ?? 0);
  return frames.reduce((a: any, b: any) => (area(b) > area(a) ? b : a));
}

/** 拉回前收集需要导出为 SVG 的矢量节点 id（交给 figma_export_svgs 批量导） */
export function collectVectorIds(doc: any): string[] {
  const frame = pickFrame(doc);
  if (!frame) return [];
  const out: string[] = [];
  walk(frame, (n) => {
    if (VECTORISH.has(n.type) && n.visible !== false && n.absoluteBoundingBox) out.push(n.id);
  });
  return out.slice(0, 60);
}

function rgba(c: any, extra = 1): string {
  if (!c) return "transparent";
  const r = Math.round((c.r ?? 0) * 255);
  const g = Math.round((c.g ?? 0) * 255);
  const b = Math.round((c.b ?? 0) * 255);
  const a = +(((c.a ?? 1) * extra).toFixed(3));
  return a >= 1 ? `rgb(${r},${g},${b})` : `rgba(${r},${g},${b},${a})`;
}

function esc(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/\n/g, "<br>");
}

function firstVisible(paints: any[] | undefined): any | null {
  return (paints ?? []).find((p: any) => p.visible !== false) ?? null;
}

/** 填充 → CSS 背景（IMAGE 填充单独出 <img>，这里返回 imgRef） */
function fillCss(n: any): { css: string; imgRef?: string; imgMode?: string } {
  const f = firstVisible(n.fills);
  if (!f) return { css: "" };
  if (f.type === "SOLID") return { css: `background:${rgba(f.color, f.opacity ?? 1)};` };
  if (f.type === "IMAGE") return { css: "", imgRef: f.imageRef, imgMode: f.scaleMode };
  if (String(f.type).startsWith("GRADIENT")) {
    const stops = (f.gradientStops ?? [])
      .map((s: any) => `${rgba(s.color, f.opacity ?? 1)} ${Math.round((s.position ?? 0) * 100)}%`)
      .join(",");
    if (!stops) return { css: "" };
    if (f.type === "GRADIENT_LINEAR" && f.gradientHandlePositions?.length >= 2) {
      const [p0, p1] = f.gradientHandlePositions;
      const deg = Math.round(90 + (Math.atan2(p1.y - p0.y, p1.x - p0.x) * 180) / Math.PI);
      return { css: `background:linear-gradient(${deg}deg,${stops});` };
    }
    if (f.type === "GRADIENT_RADIAL")
      return { css: `background:radial-gradient(circle,${stops});` };
    return { css: `background:linear-gradient(180deg,${stops});` };
  }
  return { css: "" };
}

function strokeCss(n: any): string {
  const s = firstVisible(n.strokes);
  if (!s || s.type !== "SOLID") return "";
  const w = n.strokeWeight ?? 1;
  return `border:${w}px solid ${rgba(s.color, s.opacity ?? 1)};`;
}

function radiusCss(n: any): string {
  if (Array.isArray(n.rectangleCornerRadii) && n.rectangleCornerRadii.some((r: number) => r > 0))
    return `border-radius:${n.rectangleCornerRadii.map((r: number) => `${r}px`).join(" ")};`;
  if (n.cornerRadius > 0) return `border-radius:${n.cornerRadius}px;`;
  return "";
}

function shadowCss(n: any): string {
  const parts = (n.effects ?? [])
    .filter((e: any) => e.visible !== false && (e.type === "DROP_SHADOW" || e.type === "INNER_SHADOW"))
    .map(
      (e: any) =>
        `${e.type === "INNER_SHADOW" ? "inset " : ""}${Math.round(e.offset?.x ?? 0)}px ${Math.round(
          e.offset?.y ?? 0
        )}px ${Math.round(e.radius ?? 0)}px ${Math.round(e.spread ?? 0)}px ${rgba(e.color)}`
    );
  return parts.length ? `box-shadow:${parts.join(",")};` : "";
}

function textCss(n: any): string {
  const st = n.style ?? {};
  let css = "";
  if (st.fontFamily) css += `font-family:'${st.fontFamily}','Microsoft YaHei',sans-serif;`;
  if (st.fontSize) css += `font-size:${st.fontSize}px;`;
  if (st.fontWeight) css += `font-weight:${st.fontWeight};`;
  if (st.letterSpacing) css += `letter-spacing:${(+st.letterSpacing).toFixed(2)}px;`;
  if (st.lineHeightPx) css += `line-height:${st.lineHeightPx}px;`;
  if (st.italic) css += "font-style:italic;";
  if (st.textDecoration === "UNDERLINE") css += "text-decoration:underline;";
  else if (st.textDecoration === "STRIKETHROUGH") css += "text-decoration:line-through;";
  if (st.textCase === "UPPER") css += "text-transform:uppercase;";
  const alignH: Record<string, string> = { LEFT: "left", CENTER: "center", RIGHT: "right", JUSTIFIED: "justify" };
  if (alignH[st.textAlignHorizontal]) css += `text-align:${alignH[st.textAlignHorizontal]};`;
  // 垂直对齐用 flex 竖排实现（Figma 文本框有固定高度时才有意义）
  const alignV: Record<string, string> = { CENTER: "center", BOTTOM: "flex-end", TOP: "flex-start" };
  if (st.textAlignVertical && st.textAlignVertical !== "TOP")
    css += `display:flex;flex-direction:column;justify-content:${alignV[st.textAlignVertical]};`;
  const f = firstVisible(n.fills);
  if (f?.type === "SOLID") css += `color:${rgba(f.color, f.opacity ?? 1)};`;
  return css;
}

/** 把一个 Frame 转成整页 HTML（body = Frame 尺寸, 元素全部绝对定位拍平） */
export function figmaFrameToHtml(
  data: FigmaPullData,
  svgs: Record<string, string>
): { html: string; name: string; w: number; h: number } {
  const frame = pickFrame(data.doc);
  if (!frame) throw new Error("这个 Figma 文件里没有可转换的 Frame（第一页是空的？）");
  const fb = frame.absoluteBoundingBox;
  const w = Math.round(fb.width);
  const h = Math.round(fb.height);
  const parts: string[] = [];
  let count = 0;

  const render = (n: any, depth: number) => {
    if (n.visible === false || count > MAX_NODES || depth > MAX_DEPTH) return;
    const bb = n.absoluteBoundingBox;
    if (!bb) return;
    count++;
    const x = Math.round(bb.x - fb.x);
    const y = Math.round(bb.y - fb.y);
    const bw = Math.max(1, Math.round(bb.width));
    const bh = Math.max(1, Math.round(bb.height));
    const pos = `position:absolute;left:${x}px;top:${y}px;width:${bw}px;height:${bh}px;box-sizing:border-box;`;
    const op = n.opacity != null && n.opacity < 1 ? `opacity:${(+n.opacity).toFixed(3)};` : "";

    if (n.type === "TEXT") {
      parts.push(`<div style="${pos}${op}${textCss(n)}margin:0;">${esc(n.characters ?? "")}</div>`);
      return;
    }
    if (VECTORISH.has(n.type)) {
      if (svgs[n.id]) {
        parts.push(`<img src="${svgs[n.id]}" style="${pos}${op}display:block;" alt="">`);
      } else {
        // 没导出到 SVG 的矢量: 用第一填充/描边色的圆角盒近似
        const f = firstVisible(n.fills) ?? firstVisible(n.strokes);
        const c = f?.type === "SOLID" ? rgba(f.color, f.opacity ?? 1) : "rgba(0,0,0,.25)";
        parts.push(`<div style="${pos}${op}background:${c};border-radius:3px;"></div>`);
      }
      return;
    }

    const fill = fillCss(n);
    const box = `${pos}${op}${fill.css}${strokeCss(n)}${radiusCss(n)}${shadowCss(n)}`;
    if (n.type === "ELLIPSE") {
      parts.push(`<div style="${box}border-radius:50%;"></div>`);
      return;
    }
    if (n.type === "LINE") {
      const s = firstVisible(n.strokes);
      const c = s?.type === "SOLID" ? rgba(s.color, s.opacity ?? 1) : "#000";
      const lw = Math.max(1, Math.round(n.strokeWeight ?? 1));
      parts.push(
        `<div style="position:absolute;left:${x}px;top:${y}px;width:${bw}px;height:${lw}px;${op}background:${c};"></div>`
      );
      return;
    }
    // 图片填充: 独立 <img>（临时 URL 已内嵌为 data URI, 不会过期）
    if (fill.imgRef && data.images[fill.imgRef]) {
      const fit = fill.imgMode === "FIT" ? "contain" : "cover";
      parts.push(
        `<img src="${data.images[fill.imgRef]}" style="${pos}${op}${radiusCss(n)}${shadowCss(n)}object-fit:${fit};display:block;" alt="">`
      );
    } else if (n.type === "RECTANGLE" || CONTAINERISH.has(n.type)) {
      // 容器/矩形: 只出自己的底盒(有视觉属性才出), 孩子拍平续画在其上
      if (fill.css || strokeCss(n) || shadowCss(n)) parts.push(`<div style="${box}"></div>`);
    }
    if (CONTAINERISH.has(n.type)) for (const c of n.children ?? []) render(c, depth + 1);
  };

  // 页面背景 = Frame 自己的填充
  const frameFill = fillCss(frame);
  const bodyBg = frameFill.css || "background:#ffffff;";
  for (const c of frame.children ?? []) render(c, 0);

  const name = String(frame.name ?? "Figma 页面");
  const html =
    `<!doctype html>\n<html lang="zh"><head><meta charset="utf-8"><title>${esc(name)}</title>` +
    `<style>*{margin:0;box-sizing:border-box}html,body{width:${w}px;height:${h}px;overflow:hidden}</style></head>` +
    `<body style="position:relative;width:${w}px;height:${h}px;${bodyBg}font-family:'Microsoft YaHei',sans-serif;">\n` +
    parts.join("\n") +
    `\n</body></html>\n`;
  return { html, name, w, h };
}
