/**
 * ArtifactEditor 拆分出的共享类型 / 常量 / 纯函数 / 无状态 DOM 工具。
 * 全部从 ArtifactEditor.vue 原样搬移，仅把对模块级 ref 的依赖改成显式入参，行为不变。
 */
import { Image as ImageIcon, Type, List, Table, Frame } from "@lucide/vue";

export type Mode = "visual" | "code";
// 工具（Figma 式底部工具条）：select=点选/框选, hand=抓手, 其余=拖拽画出元素
export type Tool = "select" | "hand" | "text" | "rect" | "ellipse" | "line";

// 图层树（Figma 左栏）：扁平行 + 深度缩进，折叠靠 parentId 链过滤
export type LayerNode = {
  id: number; el: HTMLElement; parentId: number | null;
  tag: string; label: string; depth: number; kids: number;
  hidden: boolean; locked: boolean;
};

export const HANDLES = ["nw", "n", "ne", "e", "se", "s", "sw", "w"];

export const BLOCK_SEL =
  "h1,h2,h3,h4,h5,h6,p,li,img,ul,ol,table,blockquote,pre,.card,.pill,.kicker,.lede,.eyebrow,.h1,.h2,.h3,.big-num,.gradient-text,.divider-accent,.row,.grid";

// 效果：不透明度 / 阴影
export const SHADOWS = [
  { n: "无阴影", v: "none" },
  { n: "轻", v: "0 2px 8px rgba(0,0,0,.14)" },
  { n: "中", v: "0 6px 20px rgba(0,0,0,.22)" },
  { n: "重", v: "0 14px 40px rgba(0,0,0,.32)" },
];

export const FONTS = [
  { n: "微软雅黑", v: "Microsoft YaHei" },
  { n: "宋体", v: "SimSun" },
  { n: "黑体", v: "SimHei" },
  { n: "楷体", v: "KaiTi" },
  { n: "仿宋", v: "FangSong" },
  { n: "Arial", v: "Arial" },
  { n: "Georgia", v: "Georgia" },
  { n: "Times", v: "Times New Roman" },
  { n: "等宽 Consolas", v: "Consolas" },
];

export const SKIP_TAGS = new Set(["SCRIPT", "STYLE", "LINK", "META", "TITLE", "BR", "TEMPLATE"]);
export const VOID_TAGS = new Set(["img", "hr", "br", "input", "source", "svg", "video", "audio", "iframe", "canvas"]);

export function isEditorNode(el: Element): boolean {
  return (el.id || "").startsWith("__");
}

/** 图层行的类型图标（Figma 式: 文本 T / 图片 / 列表 / 表格 / 容器框） */
export function layerIcon(tag: string) {
  if (["img", "svg", "picture", "video", "canvas"].includes(tag)) return ImageIcon;
  if (["h1", "h2", "h3", "h4", "h5", "h6", "p", "span", "a", "li", "blockquote", "strong", "em", "label", "b", "i"].includes(tag)) return Type;
  if (tag === "ul" || tag === "ol") return List;
  if (tag === "table") return Table;
  return Frame;
}

export function layerLabelFor(el: HTMLElement): string {
  const t = el.tagName.toLowerCase();
  if (t === "img") return "图片";
  const txt = (el.textContent || "").trim().replace(/\s+/g, " ");
  if (txt && el.childElementCount === 0) return txt.slice(0, 24);
  const cls = (el.getAttribute("class") || "")
    .split(/\s+/)
    .filter((c) => c && !c.startsWith("__") && c !== "is-active" && c !== "is-prev")[0];
  if (cls) return `.${cls}`;
  return txt ? txt.slice(0, 24) : t;
}

export function rgbToHex(c: string): string {
  const m = /rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)/.exec(c);
  if (!m) return c.startsWith("#") ? c : "#000000";
  const h = (n: string) => (+n).toString(16).padStart(2, "0");
  return "#" + h(m[1]) + h(m[2]) + h(m[3]);
}

// ── 检查器 hex 色值输入 ──
export function normHex(v: string): string | null {
  const m = /^#?([0-9a-fA-F]{6})$/.exec(v.trim());
  return m ? "#" + m[1].toLowerCase() : null;
}

export function getTranslate(el: HTMLElement): { tx: number; ty: number } {
  const m = /translate\(\s*([-0-9.]+)px\s*,\s*([-0-9.]+)px/.exec(el.style.transform || "");
  return m ? { tx: parseFloat(m[1]), ty: parseFloat(m[2]) } : { tx: 0, ty: 0 };
}
export function getRotate(el: HTMLElement): number {
  const m = /rotate\(\s*([-0-9.]+)deg/.exec(el.style.transform || "");
  return m ? Math.round(parseFloat(m[1])) : 0;
}
export function applyTransform(el: HTMLElement, tx: number, ty: number, rot: number) {
  el.style.transform = `translate(${Math.round(tx)}px, ${Math.round(ty)}px) rotate(${rot || 0}deg)`;
}
export function setTranslate(el: HTMLElement, tx: number, ty: number) {
  applyTransform(el, tx, ty, getRotate(el));
}

// ───────── 序列化（去掉编辑器注入物）─────────
export function serializeDoc(d: Document): string {
  const root = d.documentElement.cloneNode(true) as HTMLElement;
  root.querySelectorAll("#__ed, #__obj, #__objcss, #__gv, #__gh, #__rb, #__meas").forEach((e) => e.remove());
  root.querySelectorAll("[contenteditable]").forEach((e) => e.removeAttribute("contenteditable"));
  root.querySelectorAll(".__hov, .__msel").forEach((e) => {
    e.classList.remove("__hov", "__msel");
    if (!e.getAttribute("class")) e.removeAttribute("class");
  });
  root.querySelectorAll(".is-active,.is-prev").forEach((e) => {
    e.classList.remove("is-active", "is-prev");
    if (!e.getAttribute("class")) e.removeAttribute("class");
  });
  return "<!doctype html>\n" + root.outerHTML;
}

// 把当前文档克隆成「只显示第 n 页」的静态缩略 HTML（去脚本/编辑物/动画/页脚，秒开无 JS）
export function buildThumbDoc(d: Document, n: number): string {
  const root = d.documentElement.cloneNode(true) as HTMLElement;
  root.querySelectorAll("#__ed, #__obj, #__objcss, #__gv, #__gh, #__rb, #__meas, script").forEach((e) => e.remove());
  root.querySelectorAll("[contenteditable]").forEach((e) => e.removeAttribute("contenteditable"));
  root.querySelectorAll(".__hov, .__msel").forEach((e) => e.classList.remove("__hov", "__msel"));
  root.querySelectorAll<HTMLElement>(".slide").forEach((s, i) => {
    s.classList.remove("is-prev", "is-active");
    if (i === n) s.classList.add("is-active");
  });
  const head = root.querySelector("head");
  if (head) {
    const st = document.createElement("style");
    st.textContent =
      "*{animation:none!important;transition:none!important}" +
      ".slide.is-active [data-anim],.slide.is-active [class*=anim-],.slide.is-active .anim-stagger-list>*{opacity:1!important;transform:none!important}" +
      ".progress-bar,.deck-footer,.deck-header{display:none!important}";
    head.appendChild(st);
  }
  return "<!doctype html>\n" + root.outerHTML;
}

export function injectEditorStyle(d: Document) {
  if (d.getElementById("__ed")) return;
  const st = d.createElement("style");
  st.id = "__ed";
  st.textContent = `
    [contenteditable]{outline:none!important;}
    [contenteditable] h1:hover,[contenteditable] h2:hover,[contenteditable] h3:hover,
    [contenteditable] h4:hover,[contenteditable] p:hover,[contenteditable] li:hover,
    [contenteditable] span:hover,[contenteditable] .kicker:hover,[contenteditable] .lede:hover{
      outline:1px dashed rgba(125,150,255,.55);outline-offset:4px;border-radius:3px;cursor:text;}
    [contenteditable] ::selection{background:rgba(120,160,255,.4);}
  `;
  d.head?.appendChild(st);
}

/** 编辑器覆盖层（选中框/手柄/参考线/框选橡皮筋/测距）注入；onHandleDown = 手柄按下开始缩放 */
export function ensureOverlay(
  d: Document,
  onHandleDown: (e: PointerEvent, dir: string) => void
): HTMLElement | null {
  if (!d.getElementById("__objcss")) {
    const st = d.createElement("style");
    st.id = "__objcss";
    /* Figma 视觉语言: #0d99ff 选中蓝 / #f24822 智能参考线红;
       所有描边与手柄尺寸除以 --edscale(画布缩放), 保证任何缩放下都是屏幕 1px/8px 观感 */
    st.textContent = `
      :root{--edscale:1;}
      #__obj{position:fixed;z-index:2147483600;pointer-events:none;border:calc(1.25px/var(--edscale)) solid #0d99ff;}
      #__obj .__h{position:absolute;width:calc(9px/var(--edscale));height:calc(9px/var(--edscale));background:#fff;border:calc(1.25px/var(--edscale)) solid #0d99ff;border-radius:calc(1.5px/var(--edscale));pointer-events:auto;box-shadow:0 0 calc(3px/var(--edscale)) rgba(0,0,0,.18);}
      #__obj .__h:hover{background:#0d99ff;}
      #__sz{position:absolute;left:50%;top:100%;transform:translate(-50%,calc(6px/var(--edscale)));background:#0d99ff;color:#fff;font:500 calc(11px/var(--edscale))/1.5 -apple-system,'Segoe UI',sans-serif;padding:calc(2px/var(--edscale)) calc(7px/var(--edscale));border-radius:calc(4px/var(--edscale));white-space:nowrap;pointer-events:none;font-variant-numeric:tabular-nums;}
      .__hov{outline:calc(2px/var(--edscale)) solid rgba(13,153,255,.65)!important;outline-offset:0;cursor:move;}
      #__gv{position:fixed;top:0;bottom:0;width:0;border-left:calc(1px/var(--edscale)) solid #f24822;z-index:2147483599;pointer-events:none;display:none;}
      #__gh{position:fixed;left:0;right:0;height:0;border-top:calc(1px/var(--edscale)) solid #f24822;z-index:2147483599;pointer-events:none;display:none;}
      .__msel{outline:calc(1.5px/var(--edscale)) solid #0d99ff!important;outline-offset:0;}
      #__rb{position:fixed;border:calc(1px/var(--edscale)) solid rgba(13,153,255,.9);background:rgba(13,153,255,.08);z-index:2147483601;pointer-events:none;display:none;}
      #__meas{position:fixed;inset:0;pointer-events:none;z-index:2147483602;display:none;}
      #__meas .__mlh{position:absolute;height:calc(1px/var(--edscale));background:#f24822;display:none;}
      #__meas .__mlv{position:absolute;width:calc(1px/var(--edscale));background:#f24822;display:none;}
      #__meas .__mt{position:absolute;transform:translate(-50%,-50%);background:#f24822;color:#fff;font:500 calc(10.5px/var(--edscale))/1.6 -apple-system,'Segoe UI',sans-serif;padding:calc(1px/var(--edscale)) calc(5px/var(--edscale));border-radius:calc(3px/var(--edscale));white-space:nowrap;display:none;font-variant-numeric:tabular-nums;}
    `;
    d.head?.appendChild(st);
  }
  // 磁吸对齐参考线（拖动元素贴近页面中线时亮起）+ 框选橡皮筋
  for (const gid of ["__gv", "__gh", "__rb"]) {
    if (!d.getElementById(gid)) {
      const g = d.createElement("div");
      g.id = gid;
      d.body?.appendChild(g);
    }
  }
  // Alt+悬停间距标注: 两条红线 + 两个数字标签（Figma 式测距）
  if (!d.getElementById("__meas")) {
    const m = d.createElement("div");
    m.id = "__meas";
    m.innerHTML =
      '<div class="__mlh"></div><div class="__mlv"></div><span class="__mt"></span><span class="__mt"></span>';
    d.body?.appendChild(m);
  }
  let box = d.getElementById("__obj");
  if (!box) {
    box = d.createElement("div");
    box.id = "__obj";
    box.style.display = "none";
    for (const dir of HANDLES) {
      const h = d.createElement("div");
      h.className = "__h __h-" + dir;
      h.setAttribute("data-dir", dir);
      const pos: Record<string, string> = {
        nw: "top:-6px;left:-6px;cursor:nwse-resize",
        n: "top:-6px;left:calc(50% - 5px);cursor:ns-resize",
        ne: "top:-6px;right:-6px;cursor:nesw-resize",
        e: "top:calc(50% - 5px);right:-6px;cursor:ew-resize",
        se: "bottom:-6px;right:-6px;cursor:nwse-resize",
        s: "bottom:-6px;left:calc(50% - 5px);cursor:ns-resize",
        sw: "bottom:-6px;left:-6px;cursor:nesw-resize",
        w: "top:calc(50% - 5px);left:-6px;cursor:ew-resize",
      };
      h.setAttribute("style", pos[dir]);
      h.addEventListener("pointerdown", (e) => onHandleDown(e as PointerEvent, dir));
      box.appendChild(h);
    }
    // Figma 式尺寸徽标: 选中框正下方的蓝色 W × H 胶囊
    const sz = d.createElement("div");
    sz.id = "__sz";
    box.appendChild(sz);
    d.body?.appendChild(box);
  }
  return box;
}

/** 选中框定位；box 传 null = 隐藏（多选时各元素用 __msel 描边，只有单选才有 8 手柄） */
export function positionOverlayDom(
  d: Document,
  box: { x: number; y: number; w: number; h: number } | null,
  scale: number
) {
  const el = d.getElementById("__obj") as HTMLElement | null;
  if (!el) return;
  // 编辑物尺寸随画布缩放反向补偿(线宽/手柄/徽标在屏幕上恒定大小, Figma 同款)
  d.documentElement.style.setProperty("--edscale", String(scale || 1));
  if (!box) { el.style.display = "none"; return; }
  el.style.display = "block";
  el.style.left = box.x + "px";
  el.style.top = box.y + "px";
  el.style.width = box.w + "px";
  el.style.height = box.h + "px";
  const sz = d.getElementById("__sz");
  if (sz) sz.textContent = `${Math.round(box.w)} × ${Math.round(box.h)}`;
}

/** 磁吸参考线开关（页面中线；x/y 为 iframe 视口坐标） */
export function showGuides(d: Document, v: boolean, h: boolean, x = 0, y = 0) {
  const gv = d.getElementById("__gv") as HTMLElement | null;
  const gh = d.getElementById("__gh") as HTMLElement | null;
  if (gv) { gv.style.display = v ? "block" : "none"; if (v) gv.style.left = x + "px"; }
  if (gh) { gh.style.display = h ? "block" : "none"; if (h) gh.style.top = y + "px"; }
}

// ── Alt+悬停间距标注（Figma 测距）: 选中元素 ↔ 悬停元素之间的水平/垂直净距 ──
export function clearMeasureDom(d: Document | null) {
  const m = d?.getElementById("__meas") as HTMLElement | null;
  if (m) m.style.display = "none";
}
export function drawMeasureDom(d: Document, scale: number, sel: DOMRect, hov: DOMRect) {
  const m = d.getElementById("__meas") as HTMLElement | null;
  if (!m) return;
  const [lh, lv, th, tv] = Array.from(m.children) as HTMLElement[];
  // 水平净距: 两框在 x 轴上不相交时, 量最近两条竖边
  let hx0 = 0, hx1 = 0, hOn = false;
  if (sel.right <= hov.left) { hx0 = sel.right; hx1 = hov.left; hOn = true; }
  else if (hov.right <= sel.left) { hx0 = hov.right; hx1 = sel.left; hOn = true; }
  // 垂直净距同理
  let vy0 = 0, vy1 = 0, vOn = false;
  if (sel.bottom <= hov.top) { vy0 = sel.bottom; vy1 = hov.top; vOn = true; }
  else if (hov.bottom <= sel.top) { vy0 = hov.bottom; vy1 = sel.top; vOn = true; }
  // 线画在两框重叠区中线上(无重叠用选中框中线)
  const oy0 = Math.max(sel.top, hov.top), oy1 = Math.min(sel.bottom, hov.bottom);
  const cy = oy0 < oy1 ? (oy0 + oy1) / 2 : (sel.top + sel.bottom) / 2;
  const ox0 = Math.max(sel.left, hov.left), ox1 = Math.min(sel.right, hov.right);
  const cx = ox0 < ox1 ? (ox0 + ox1) / 2 : (sel.left + sel.right) / 2;
  const k = scale || 1;
  if (hOn && hx1 - hx0 >= 1) {
    lh.style.display = "block";
    lh.style.left = hx0 + "px"; lh.style.top = cy + "px"; lh.style.width = hx1 - hx0 + "px";
    th.style.display = "block";
    th.style.left = (hx0 + hx1) / 2 + "px"; th.style.top = cy - 12 / k + "px";
    th.textContent = String(Math.round(hx1 - hx0));
  } else { lh.style.display = "none"; th.style.display = "none"; }
  if (vOn && vy1 - vy0 >= 1) {
    lv.style.display = "block";
    lv.style.left = cx + "px"; lv.style.top = vy0 + "px"; lv.style.height = vy1 - vy0 + "px";
    tv.style.display = "block";
    tv.style.left = cx + 14 / k + "px"; tv.style.top = (vy0 + vy1) / 2 + "px";
    tv.textContent = String(Math.round(vy1 - vy0));
  } else { lv.style.display = "none"; tv.style.display = "none"; }
  m.style.display = hOn || vOn ? "block" : "none";
}
