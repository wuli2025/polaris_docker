<script setup lang="ts">
/**
 * 成品编辑器（仿豆包）—— 在右抽屉放大态里直接编辑生成的「网页 PPT / HTML 网页」。
 * - 左侧：页面缩略大纲（deck 模式按 .slide 分页）
 * - 中间：大画布 iframe，可视化模式下整页 contenteditable，双击文字即改
 * - 顶部：可视化/源码切换、翻页、主题、缩放、保存、退出
 * 保存 = 把编辑后的完整 HTML 写回原产物文件（artifact_write）。
 * 既支持多页 deck，也支持单页网页（无 .slide 时自动隐藏分页栏）。
 */
import { ref, computed, onMounted, onBeforeUnmount, nextTick, watch } from "vue";
import {
  Code2, Eye, ChevronLeft, ChevronRight, Plus, Copy, Trash2,
  Save, X, Loader, Palette, ZoomIn, ZoomOut, Maximize, FileType2,
  Bold, Italic, Underline, AlignLeft, AlignCenter, AlignRight, Minus, BringToFront,
  MousePointer2, RotateCw, Type, Square, Circle, Image as ImageIcon, SendToBack,
} from "@lucide/vue";
import { useArtifactsStore } from "../stores/artifacts";
import { DECK_THEMES } from "../lib/deckThemes";

const artifacts = useArtifactsStore();

type Mode = "visual" | "code";
const mode = ref<Mode>("visual");

// 画布 / 源码各自的工作副本
const html = ref<string>(artifacts.payload?.text ?? "");
const frameSrc = ref<string>(html.value); // 显式控制 iframe 重载，避免源码每键回灌
const frame = ref<HTMLIFrameElement | null>(null);
const canvasEl = ref<HTMLElement | null>(null);
const stageEl = ref<HTMLElement | null>(null);

// ── 对象编辑（像 PPT：选中→拖动→缩放→右侧面板改格式）──
const selEl = ref<HTMLElement | null>(null);
const selBox = ref<{ x: number; y: number; w: number; h: number } | null>(null);
const selStyle = ref<{ bold: boolean; italic: boolean; underline: boolean; align: string; size: number; color: string }>(
  { bold: false, italic: false, underline: false, align: "", size: 0, color: "#000000" }
);
// 位置/大小/旋转（相对当前页，1280×720 坐标系）+ 段落
const selGeom = ref<{ x: number; y: number; w: number; h: number; rot: number }>({ x: 0, y: 0, w: 0, h: 0, rot: 0 });
const selPara = ref<{ lh: number; ls: number }>({ lh: 0, ls: 0 });
// 填充 / 描边 / 圆角（让形状框像 WPS 一样可改底色边框）
const selFill = ref<{ bg: string; hasBg: boolean; border: string; bw: number; radius: number }>(
  { bg: "#4a86ff", hasBg: false, border: "#ffffff", bw: 0, radius: 0 }
);
const fileInput = ref<HTMLInputElement | null>(null);
const HANDLES = ["nw", "n", "ne", "e", "se", "s", "sw", "w"];

// deck 信息
const isDeck = ref(false);
const slides = ref<{ title: string; accent: string }[]>([]);
const cur = ref(0);
const total = computed(() => slides.value.length);
// 每页一张真实缩略（静态、无脚本的自包含 srcdoc，只显示该页）
const thumbs = ref<string[]>([]);

// 主题
const themes = DECK_THEMES;
const theme = ref<string>("");

// 缩放
const zoom = ref(1); // 用户倍率
const fitScale = ref(1);
const scale = computed(() => +(fitScale.value * zoom.value).toFixed(3));
const stageStyle = computed(() => ({
  width: "1280px",
  height: "720px",
  transform: `scale(${scale.value})`,
}));

const dirty = computed(() => artifacts.dirty);
const saving = computed(() => artifacts.saving);
const justSaved = ref(false);

// ───────── iframe 接入 ─────────
function win(): any { return frame.value?.contentWindow as any; }
function doc(): Document | null { return frame.value?.contentDocument ?? null; }

let inputHandler: (() => void) | null = null;
let keyGuard: ((e: KeyboardEvent) => void) | null = null;
let clickGuard: ((e: MouseEvent) => void) | null = null;

function detectDeck() {
  const d = doc();
  if (!d) return;
  const secs = Array.from(d.querySelectorAll<HTMLElement>(".slide"));
  isDeck.value = !!d.querySelector(".deck") && secs.length > 0;
  slides.value = secs.map((s, i) => ({
    title:
      s.getAttribute("data-title") ||
      (s.querySelector("h1,h2,h3,.h1,.h2,.h3") as HTMLElement | null)?.textContent?.trim()?.slice(0, 40) ||
      `第 ${i + 1} 页`,
    accent: "",
  }));
  theme.value = d.documentElement.getAttribute("data-theme") || "";
  const w = win();
  cur.value = (w?.__deck?.current?.() as number) ?? 0;
}

// 把当前文档克隆成「只显示第 n 页」的静态缩略 HTML（去脚本/编辑物/动画/页脚，秒开无 JS）
function buildThumb(n: number): string {
  const d = doc();
  if (!d) return "";
  const root = d.documentElement.cloneNode(true) as HTMLElement;
  root.querySelectorAll("#__ed, #__obj, #__objcss, script").forEach((e) => e.remove());
  root.querySelectorAll("[contenteditable]").forEach((e) => e.removeAttribute("contenteditable"));
  root.querySelectorAll(".__hov").forEach((e) => e.classList.remove("__hov"));
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
function rebuildThumbs() {
  if (!isDeck.value) { thumbs.value = []; return; }
  const arr: string[] = [];
  for (let i = 0; i < total.value; i++) arr.push(buildThumb(i));
  thumbs.value = arr;
}

function applyEditable() {
  const d = doc();
  if (!d) return;
  d.querySelectorAll("[contenteditable]").forEach((e) => e.removeAttribute("contenteditable"));
  if (mode.value !== "visual") return;
  // 普通网页：整页可编辑文字；deck：走对象编辑（双击进文字编辑），不整页 contenteditable
  if (!isDeck.value) d.body?.setAttribute("contenteditable", "true");
}

function injectEditorStyle() {
  const d = doc();
  if (!d) return;
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

function onFrameLoad() {
  const d = doc();
  if (!d) return;
  injectEditorStyle();
  detectDeck();
  applyEditable();
  rebuildThumbs();
  ensureOverlay();
  clearSel();
  // 输入即脏
  inputHandler = () => { if (!artifacts.dirty) artifacts.markDirty(true); refreshSel(); };
  d.addEventListener("input", inputHandler, true);
  // 编辑时拦掉 deck 自带的 ←/→ 翻页；Esc 退选/退出文字编辑；Delete 删元素
  keyGuard = (e: KeyboardEvent) => {
    if (mode.value !== "visual") return;
    const ae = d.activeElement as HTMLElement | null;
    if (ae && (ae as any).isContentEditable) {
      e.stopPropagation();
      if (e.key === "Escape") { ae.removeAttribute("contenteditable"); ae.blur?.(); refreshSel(); }
      return;
    }
    if ((e.ctrlKey || e.metaKey) && (e.key === "z" || e.key === "Z")) {
      // 非文字编辑态的 Ctrl+Z = 恢复最近删除的元素
      if (undoDelete()) {
        e.preventDefault();
        e.stopPropagation();
      }
      return;
    }
    if (selEl.value) {
      if (e.key === "Escape") { clearSel(); e.stopPropagation(); }
      else if (e.key === "Delete") { fmtDelete(); e.stopPropagation(); }
    }
  };
  d.addEventListener("keydown", keyGuard, true);
  // 阻止 deck 自带点击翻页（编辑时点字不该翻页）
  clickGuard = (e: MouseEvent) => {
    if (mode.value === "visual" && (e.target as HTMLElement)?.closest(".slide")) e.stopPropagation();
  };
  d.addEventListener("click", clickGuard, true);
  // 对象编辑监听
  d.addEventListener("pointerdown", onDocPointerDown as any, true);
  d.addEventListener("pointermove", onDocPointerMove as any, true);
  d.addEventListener("pointerup", onDocPointerUp as any, true);
  d.addEventListener("dblclick", onDocDblClick as any, true);
  d.addEventListener("mouseover", onDocMouseOver as any, true);
  computeFit();
  if (pendingGo != null) { const g = pendingGo; pendingGo = null; goSlide(g); }
}

// ───────── 导航 ─────────
let pendingGo: number | null = null;
function goSlide(n: number) {
  const w = win();
  if (!isDeck.value || !w?.__deck) return;
  const i = Math.max(0, Math.min(total.value - 1, n));
  const leaving = cur.value;
  clearSel();
  w.__deck.go(i);
  cur.value = i;
  applyEditable();
  // 刷新刚离开那页的缩略，让可视化编辑即时反映到左栏
  if (leaving !== i && thumbs.value.length) thumbs.value[leaving] = buildThumb(leaving);
}
function prev() { goSlide(cur.value - 1); }
function next() { goSlide(cur.value + 1); }

// ───────── 主题 ─────────
function setTheme(id: string) {
  const d = doc();
  if (!d) return;
  d.documentElement.setAttribute("data-theme", id);
  theme.value = id;
  artifacts.markDirty(true);
  rebuildThumbs(); // 缩略跟着换肤
}

// ───────── 对象编辑：选中 / 拖动 / 缩放 / 改格式 ─────────
const BLOCK_SEL =
  "h1,h2,h3,h4,h5,h6,p,li,img,ul,ol,table,blockquote,pre,.card,.pill,.kicker,.lede,.eyebrow,.h1,.h2,.h3,.big-num,.gradient-text,.divider-accent,.row,.grid";

function activeSlide(): HTMLElement | null {
  const d = doc();
  return d ? (d.querySelector<HTMLElement>(".slide.is-active") || d.body) : null;
}
function selectableFrom(t: HTMLElement): HTMLElement | null {
  const slide = activeSlide();
  if (!slide || !slide.contains(t) || t === slide) return null;
  let el: HTMLElement | null = t;
  // 从命中点向上找到「一个有意义的盒子」：自身或最近的块级/卡片，止于 slide 的直接子级
  while (el && el !== slide) {
    if (el.matches(BLOCK_SEL) || el.parentElement === slide) return el;
    el = el.parentElement;
  }
  return t;
}

function getTranslate(el: HTMLElement): { tx: number; ty: number } {
  const m = /translate\(\s*([-0-9.]+)px\s*,\s*([-0-9.]+)px/.exec(el.style.transform || "");
  return m ? { tx: parseFloat(m[1]), ty: parseFloat(m[2]) } : { tx: 0, ty: 0 };
}
function getRotate(el: HTMLElement): number {
  const m = /rotate\(\s*([-0-9.]+)deg/.exec(el.style.transform || "");
  return m ? Math.round(parseFloat(m[1])) : 0;
}
function applyTransform(el: HTMLElement, tx: number, ty: number, rot: number) {
  el.style.transform = `translate(${Math.round(tx)}px, ${Math.round(ty)}px) rotate(${rot || 0}deg)`;
}
function setTranslate(el: HTMLElement, tx: number, ty: number) {
  applyTransform(el, tx, ty, getRotate(el));
}

function ensureOverlay(): HTMLElement | null {
  const d = doc();
  if (!d) return null;
  if (!d.getElementById("__objcss")) {
    const st = d.createElement("style");
    st.id = "__objcss";
    st.textContent = `
      #__obj{position:fixed;z-index:2147483600;pointer-events:none;border:1.5px solid #4a86ff;box-shadow:0 0 0 1px rgba(255,255,255,.6);}
      #__obj .__h{position:absolute;width:11px;height:11px;background:#fff;border:1.5px solid #4a86ff;border-radius:2px;pointer-events:auto;}
      #__obj .__h:hover{background:#4a86ff;}
      .__hov{outline:1.5px dashed rgba(74,134,255,.55)!important;outline-offset:2px;cursor:move;}
    `;
    d.head?.appendChild(st);
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
      h.addEventListener("pointerdown", (e) => startResize(e as PointerEvent, dir));
      box.appendChild(h);
    }
    d.body?.appendChild(box);
  }
  return box;
}

function positionOverlay() {
  const box = doc()?.getElementById("__obj") as HTMLElement | null;
  if (!box) return;
  if (!selEl.value || !selBox.value) { box.style.display = "none"; return; }
  const b = selBox.value;
  box.style.display = "block";
  box.style.left = b.x + "px";
  box.style.top = b.y + "px";
  box.style.width = b.w + "px";
  box.style.height = b.h + "px";
}
function rgbToHex(c: string): string {
  const m = /rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)/.exec(c);
  if (!m) return c.startsWith("#") ? c : "#000000";
  const h = (n: string) => (+n).toString(16).padStart(2, "0");
  return "#" + h(m[1]) + h(m[2]) + h(m[3]);
}
function readSelStyle() {
  const el = selEl.value;
  if (!el) return;
  const cs = getComputedStyle(el);
  selStyle.value = {
    bold: parseInt(cs.fontWeight) >= 600,
    italic: cs.fontStyle === "italic",
    underline: cs.textDecorationLine.includes("underline"),
    align: cs.textAlign,
    size: Math.round(parseFloat(cs.fontSize) || 0),
    color: rgbToHex(el.style.color || cs.color),
  };
  selPara.value = {
    lh: +(parseFloat(cs.lineHeight) / (parseFloat(cs.fontSize) || 1)).toFixed(2) || 0,
    ls: Math.round(parseFloat(cs.letterSpacing) || 0),
  };
  const bg = cs.backgroundColor;
  const hasBg = !!bg && bg !== "transparent" && bg !== "rgba(0, 0, 0, 0)";
  const bw = Math.round(parseFloat(cs.borderTopWidth) || 0);
  selFill.value = {
    bg: hasBg ? rgbToHex(bg) : "#4a86ff",
    hasBg,
    border: rgbToHex(cs.borderTopColor) || "#ffffff",
    bw,
    radius: Math.round(parseFloat(cs.borderTopLeftRadius) || 0),
  };
}
function refreshSel() {
  const el = selEl.value;
  if (!el) { selBox.value = null; positionOverlay(); return; }
  const r = el.getBoundingClientRect();
  selBox.value = { x: r.left, y: r.top, w: r.width, h: r.height };
  positionOverlay();
  readSelStyle();
  // 相对当前页的 X/Y/W/H/旋转，供右侧面板显示
  const slide = activeSlide();
  if (slide) {
    const sr = slide.getBoundingClientRect();
    selGeom.value = {
      x: Math.round(r.left - sr.left), y: Math.round(r.top - sr.top),
      w: Math.round(r.width), h: Math.round(r.height), rot: getRotate(el),
    };
  }
}
function selectEl(el: HTMLElement | null) {
  // 退出上一个文字编辑
  if (selEl.value && selEl.value !== el) selEl.value.removeAttribute("contenteditable");
  selEl.value = el;
  refreshSel();
}
function clearSel() { selectEl(null); }

// 拖动 / 缩放
let drag: null | {
  kind: "move" | "resize"; dir: string;
  sx: number; sy: number; tx0: number; ty0: number; w0: number; h0: number;
} = null;

function startMove(e: PointerEvent) {
  const el = selEl.value;
  if (!el) return;
  const t = getTranslate(el);
  const r = el.getBoundingClientRect();
  drag = { kind: "move", dir: "", sx: e.clientX, sy: e.clientY, tx0: t.tx, ty0: t.ty, w0: r.width, h0: r.height };
  (e.target as HTMLElement).setPointerCapture?.(e.pointerId);
  e.preventDefault();
}
function startResize(e: PointerEvent, dir: string) {
  const el = selEl.value;
  if (!el) return;
  e.stopPropagation();
  e.preventDefault();
  el.style.boxSizing = "border-box";
  const t = getTranslate(el);
  const r = el.getBoundingClientRect();
  el.style.width = r.width + "px";
  el.style.height = r.height + "px";
  drag = { kind: "resize", dir, sx: e.clientX, sy: e.clientY, tx0: t.tx, ty0: t.ty, w0: r.width, h0: r.height };
  (e.target as HTMLElement).setPointerCapture?.(e.pointerId);
}
function onDocPointerMove(e: PointerEvent) {
  if (!drag || !selEl.value) return;
  const el = selEl.value;
  const dx = e.clientX - drag.sx, dy = e.clientY - drag.sy;
  if (drag.kind === "move") {
    setTranslate(el, drag.tx0 + dx, drag.ty0 + dy);
  } else {
    let { tx0: tx, ty0: ty } = drag;
    let w = drag.w0, h = drag.h0;
    const d = drag.dir;
    if (d.includes("e")) w = drag.w0 + dx;
    if (d.includes("s")) h = drag.h0 + dy;
    if (d.includes("w")) { w = drag.w0 - dx; tx = drag.tx0 + dx; }
    if (d.includes("n")) { h = drag.h0 - dy; ty = drag.ty0 + dy; }
    el.style.width = Math.max(24, w) + "px";
    el.style.height = Math.max(20, h) + "px";
    setTranslate(el, tx, ty);
  }
  refreshSel();
}
function onDocPointerUp() {
  if (drag) { drag = null; artifacts.markDirty(true); refreshSel(); }
}

function onDocPointerDown(e: PointerEvent) {
  if (mode.value !== "visual" || !isDeck.value) return;
  const t = e.target as HTMLElement;
  if (t.closest("#__obj")) return; // 点到手柄 → 各自处理
  const pick = selectableFrom(t);
  if (pick) {
    if (pick !== selEl.value) selectEl(pick);
    // 已选中且不是正在文字编辑 → 开始拖动
    if (!(pick as any).isContentEditable) startMove(e);
  } else {
    clearSel();
  }
}
function onDocDblClick(e: PointerEvent) {
  if (mode.value !== "visual" || !isDeck.value) return;
  const el = selEl.value || selectableFrom(e.target as HTMLElement);
  if (!el) return;
  selectEl(el);
  el.setAttribute("contenteditable", "true");
  (el as HTMLElement).focus?.();
}
function onDocMouseOver(e: MouseEvent) {
  if (mode.value !== "visual" || !isDeck.value) return;
  const d = doc(); if (!d) return;
  d.querySelectorAll(".__hov").forEach((x) => x.classList.remove("__hov"));
  const pick = selectableFrom(e.target as HTMLElement);
  if (pick && pick !== selEl.value) pick.classList.add("__hov");
}

// 格式工具栏动作
function fmtBold() { const el = selEl.value; if (!el) return; el.style.fontWeight = selStyle.value.bold ? "400" : "800"; afterFmt(); }
function fmtItalic() { const el = selEl.value; if (!el) return; el.style.fontStyle = selStyle.value.italic ? "normal" : "italic"; afterFmt(); }
function fmtUnderline() { const el = selEl.value; if (!el) return; el.style.textDecoration = selStyle.value.underline ? "none" : "underline"; afterFmt(); }
function fmtAlign(a: string) { const el = selEl.value; if (!el) return; el.style.textAlign = a; afterFmt(); }
function fmtFont(delta: number) { const el = selEl.value; if (!el) return; el.style.fontSize = Math.max(8, selStyle.value.size + delta) + "px"; afterFmt(); }
function fmtColor(e: Event) { const el = selEl.value; if (!el) return; const c = (e.target as HTMLInputElement).value; el.style.color = c; (el.style as any).webkitTextFillColor = c; afterFmt(); }
// 删除可撤销:记住被删元素与原位置,Ctrl+Z 原位恢复(误删不再只能改源码找回)
const deletedStack: Array<{ el: Element; parent: Element; next: Node | null }> = [];
function fmtDelete() {
  const el = selEl.value;
  if (!el) return;
  if (el.parentElement) {
    deletedStack.push({ el, parent: el.parentElement, next: el.nextSibling });
    if (deletedStack.length > 30) deletedStack.shift();
  }
  el.remove();
  clearSel();
  artifacts.markDirty(true);
  rebuildThumbs();
}
function undoDelete(): boolean {
  const d = deletedStack.pop();
  if (!d) return false;
  const anchor = d.next && d.next.parentNode === d.parent ? d.next : null;
  d.parent.insertBefore(d.el, anchor);
  artifacts.markDirty(true);
  rebuildThumbs();
  return true;
}
function fmtFront() { const el = selEl.value; if (!el) return; if (getComputedStyle(el).position === "static") el.style.position = "relative"; el.style.zIndex = "60"; afterFmt(); }
function fmtBack() { const el = selEl.value; if (!el) return; if (getComputedStyle(el).position === "static") el.style.position = "relative"; el.style.zIndex = "1"; afterFmt(); }
// 填充 / 描边 / 圆角
function fmtFill(e: Event) { const el = selEl.value; if (!el) return; el.style.backgroundColor = (e.target as HTMLInputElement).value; afterFmt(); }
function fmtFillClear() { const el = selEl.value; if (!el) return; el.style.backgroundColor = "transparent"; afterFmt(); }
function fmtBorderColor(e: Event) { const el = selEl.value; if (!el) return; const c = (e.target as HTMLInputElement).value; el.style.borderColor = c; if (!parseFloat(getComputedStyle(el).borderTopWidth)) el.style.borderWidth = "2px", el.style.borderStyle = "solid"; afterFmt(); }
function fmtBorderWidth(v: number) { const el = selEl.value; if (!el) return; const w = Math.max(0, v); el.style.borderWidth = w + "px"; el.style.borderStyle = w ? "solid" : "none"; afterFmt(); }
function fmtRadius(v: number) { const el = selEl.value; if (!el) return; el.style.borderRadius = Math.max(0, v) + "px"; afterFmt(); }

// ───────── 插入元素（仿豆包顶栏：文本框 / 形状 / 线条 / 图片 = 自由浮动的 WPS 式框）─────────
function insertNode(el: HTMLElement, w: number, h: number | null) {
  const d = doc();
  const slide = activeSlide();
  if (!d || !slide) return;
  if (getComputedStyle(slide).position === "static") slide.style.position = "relative";
  el.classList.add("__ins");
  el.style.position = "absolute";
  el.style.boxSizing = "border-box";
  el.style.left = Math.round((1280 - w) / 2) + "px";
  el.style.top = Math.round((720 - (h ?? 80)) / 2) + "px";
  el.style.width = w + "px";
  if (h != null) el.style.height = h + "px";
  el.style.zIndex = "40";
  slide.appendChild(el);
  selectEl(el);
  artifacts.markDirty(true);
  rebuildThumbs();
}
function insertEl(kind: "text" | "rect" | "ellipse" | "line") {
  const d = doc();
  if (!d) return;
  const el = d.createElement("div");
  if (kind === "text") {
    el.textContent = "双击编辑文字";
    el.style.cssText = "font-size:40px;font-weight:700;line-height:1.25;color:#ffffff;text-shadow:0 1px 6px rgba(0,0,0,.25);";
    insertNode(el, 480, null);
  } else if (kind === "rect") {
    el.style.cssText = "background:rgba(74,134,255,.85);border-radius:10px;";
    insertNode(el, 280, 180);
  } else if (kind === "ellipse") {
    el.style.cssText = "background:rgba(74,134,255,.85);border-radius:50%;";
    insertNode(el, 200, 200);
  } else {
    el.style.cssText = "background:#ffffff;border-radius:2px;";
    insertNode(el, 420, 4);
  }
}
function pickImage() { fileInput.value?.click(); }
function onImagePicked(e: Event) {
  const f = (e.target as HTMLInputElement).files?.[0];
  (e.target as HTMLInputElement).value = "";
  if (!f) return;
  const rd = new FileReader();
  rd.onload = () => {
    const src = rd.result as string;
    const d = doc();
    if (!d || !src) return;
    const img = d.createElement("img");
    img.src = src; // 内嵌为 base64，随 HTML 一起保存
    img.style.cssText = "display:block;border-radius:8px;object-fit:contain;";
    // 按图片原始比例给个合适初始宽度
    const probe = new Image();
    probe.onload = () => {
      const ratio = probe.naturalHeight / (probe.naturalWidth || 1);
      const w = 360;
      insertNode(img, w, Math.round(w * ratio) || 240);
    };
    probe.onerror = () => insertNode(img, 360, 240);
    probe.src = src;
  };
  rd.readAsDataURL(f);
}
function afterFmt() { artifacts.markDirty(true); refreshSel(); }

// 右侧面板：位置/大小/旋转
function setGeom(field: "x" | "y" | "w" | "h" | "rot", v: number) {
  const el = selEl.value;
  const slide = activeSlide();
  if (!el || !slide || isNaN(v)) return;
  const r = el.getBoundingClientRect();
  const sr = slide.getBoundingClientRect();
  const t = getTranslate(el);
  const rot = getRotate(el);
  if (field === "x") applyTransform(el, t.tx + (v - (r.left - sr.left)), t.ty, rot);
  else if (field === "y") applyTransform(el, t.tx, t.ty + (v - (r.top - sr.top)), rot);
  else if (field === "rot") applyTransform(el, t.tx, t.ty, v);
  else { el.style.boxSizing = "border-box"; el.style[field === "w" ? "width" : "height"] = Math.max(8, v) + "px"; }
  afterFmt();
}
// 右侧面板：段落
function setPara(field: "lh" | "ls", v: number) {
  const el = selEl.value;
  if (!el || isNaN(v)) return;
  if (field === "lh") el.style.lineHeight = String(v);
  else el.style.letterSpacing = v + "px";
  afterFmt();
}

// ───────── 结构编辑（加页/复制/删页）：改 DOM → 序列化 → 重载 ─────────
function reloadFrom(serialized: string, go: number | null) {
  html.value = serialized;
  pendingGo = go;
  frameSrc.value = serialized; // 触发 iframe 重载 → onFrameLoad
}
function addSlide(duplicate = false) {
  const d = doc();
  if (!d || !isDeck.value) return;
  const active = d.querySelector<HTMLElement>(".slide.is-active") || d.querySelector<HTMLElement>(".slide");
  if (!active) return;
  const clone = active.cloneNode(true) as HTMLElement;
  clone.classList.remove("is-active", "is-prev");
  if (!duplicate) {
    // 留下结构、清掉正文，做一张「空白同款」
    clone.querySelectorAll("h1,h2,h3,h4,p,li,span,.lede,.kicker").forEach((el) => {
      if (!el.querySelector("*")) (el as HTMLElement).textContent = "点击编辑…";
    });
  }
  active.after(clone);
  reloadFrom(serialize(), cur.value + 1);
}
function deleteSlide() {
  const d = doc();
  if (!d || !isDeck.value || total.value <= 1) return;
  const active = d.querySelector<HTMLElement>(".slide.is-active");
  if (!active) return;
  const idx = cur.value;
  active.remove();
  reloadFrom(serialize(), Math.max(0, idx - 1));
}

// ───────── 序列化（去掉编辑器注入物）─────────
function serialize(): string {
  const d = doc();
  if (!d) return html.value;
  const root = d.documentElement.cloneNode(true) as HTMLElement;
  root.querySelectorAll("#__ed, #__obj, #__objcss").forEach((e) => e.remove());
  root.querySelectorAll("[contenteditable]").forEach((e) => e.removeAttribute("contenteditable"));
  root.querySelectorAll(".__hov").forEach((e) => e.classList.remove("__hov"));
  root.querySelectorAll(".is-active,.is-prev").forEach((e) => {
    e.classList.remove("is-active", "is-prev");
    if (!e.getAttribute("class")) e.removeAttribute("class");
  });
  return "<!doctype html>\n" + root.outerHTML;
}

// ───────── 模式切换 ─────────
function toCode() {
  if (mode.value === "code") return;
  clearSel();
  html.value = serialize();   // 把可视化编辑同步进源码
  mode.value = "code";
}
function toVisual() {
  if (mode.value === "visual") return;
  clearSel();
  mode.value = "visual";
  if (frameSrc.value !== html.value) {
    pendingGo = cur.value;
    frameSrc.value = html.value; // 用源码改动重载画布
  } else {
    nextTick(applyEditable);
  }
}

// ───────── 保存 / 退出 ─────────
async function save(): Promise<boolean> {
  const out = mode.value === "visual" ? serialize() : html.value;
  html.value = out;
  const ok = await artifacts.saveContent(out);
  if (ok) {
    justSaved.value = true;
    setTimeout(() => (justSaved.value = false), 1800);
  }
  return ok;
}

// ── 伴生 .pptx：从「编辑此 PPT」进来时，保存后一键重导出覆盖 ──
const justExported = ref(false);
async function updatePptx() {
  if (artifacts.exporting || artifacts.saving) return;
  if (artifacts.dirty && !(await save())) return; // 先把改动落盘，导出读的是磁盘上的 html
  const ok = await artifacts.exportPptx();
  if (ok) {
    justExported.value = true;
    setTimeout(() => (justExported.value = false), 2600);
  }
}
function exit() {
  if (artifacts.dirty && !confirm("有未保存的修改，确定退出编辑？")) return;
  artifacts.exitEdit();
}

// ───────── 缩放 ─────────
function computeFit() {
  const el = canvasEl.value;
  if (!el) return;
  const pad = 48;
  const fw = (el.clientWidth - pad) / 1280;
  const fh = (el.clientHeight - pad) / 720;
  fitScale.value = Math.max(0.15, Math.min(fw, fh));
}
function zoomIn() { zoom.value = Math.min(2.5, +(zoom.value + 0.1).toFixed(2)); }
function zoomOut() { zoom.value = Math.max(0.3, +(zoom.value - 0.1).toFixed(2)); }
function zoomFit() { zoom.value = 1; computeFit(); }

let ro: ResizeObserver | null = null;
function onKey(e: KeyboardEvent) {
  if ((e.ctrlKey || e.metaKey) && (e.key === "s" || e.key === "S")) {
    e.preventDefault();
    save();
  }
}
onMounted(() => {
  window.addEventListener("keydown", onKey);
  ro = new ResizeObserver(() => computeFit());
  if (canvasEl.value) ro.observe(canvasEl.value);
});
onBeforeUnmount(() => {
  window.removeEventListener("keydown", onKey);
  ro?.disconnect();
  const d = doc();
  if (d) {
    if (inputHandler) d.removeEventListener("input", inputHandler, true);
    if (keyGuard) d.removeEventListener("keydown", keyGuard, true);
    if (clickGuard) d.removeEventListener("click", clickGuard, true);
  }
});

// 若外部重新打开了别的文件，刷新工作副本
watch(
  () => artifacts.payload?.path,
  () => {
    html.value = artifacts.payload?.text ?? "";
    frameSrc.value = html.value;
    mode.value = "visual";
  }
);
</script>

<template>
  <div class="ed">
    <!-- 顶部工具栏 -->
    <div class="ed-bar">
      <div class="ed-seg">
        <button :class="{ on: mode === 'visual' }" title="可视化编辑" @click="toVisual"><Eye :size="13" /> 可视化</button>
        <button :class="{ on: mode === 'code' }" title="源码编辑" @click="toCode"><Code2 :size="13" /> 源码</button>
      </div>

      <template v-if="isDeck && mode === 'visual'">
        <div class="ed-nav">
          <button class="ed-ic" :disabled="cur <= 0" title="上一页" @click="prev"><ChevronLeft :size="15" /></button>
          <span class="ed-page">{{ cur + 1 }} / {{ total }}</span>
          <button class="ed-ic" :disabled="cur >= total - 1" title="下一页" @click="next"><ChevronRight :size="15" /></button>
        </div>
        <label class="ed-theme" title="切换主题">
          <Palette :size="14" />
          <select :value="theme" @change="setTheme(($event.target as HTMLSelectElement).value)">
            <option v-for="t in themes" :key="t.id" :value="t.id">{{ t.name }}</option>
          </select>
        </label>
      </template>

      <div v-if="mode === 'visual'" class="ed-zoom">
        <button class="ed-ic" title="缩小" @click="zoomOut"><ZoomOut :size="14" /></button>
        <button class="ed-pct" title="适应窗口" @click="zoomFit">{{ Math.round(scale * 100) }}%</button>
        <button class="ed-ic" title="放大" @click="zoomIn"><ZoomIn :size="14" /></button>
      </div>

      <div class="ed-spacer" />

      <span v-if="artifacts.saveError" class="ed-err" :title="artifacts.saveError">保存失败</span>
      <span v-else-if="artifacts.exportError" class="ed-err" :title="artifacts.exportError">导出失败</span>
      <span v-else-if="justExported" class="ed-ok">PPT 已更新 ✓</span>
      <span v-else-if="justSaved" class="ed-ok">已保存 ✓</span>
      <span v-else-if="dirty" class="ed-dirty">未保存</span>

      <button class="ed-save" :disabled="saving || (!dirty && !justSaved)" @click="save">
        <Loader v-if="saving" :size="14" class="spin" /><Save v-else :size="14" />
        {{ saving ? "保存中" : "保存" }}
      </button>
      <button
        v-if="artifacts.companionPptx"
        class="ed-save pptx"
        :disabled="saving || artifacts.exporting"
        title="保存当前修改，并把网页版重新导出覆盖 .pptx（逐页截图，可能要几十秒）"
        @click="updatePptx"
      >
        <Loader v-if="artifacts.exporting" :size="14" class="spin" /><FileType2 v-else :size="14" />
        {{ artifacts.exporting ? "导出中…" : "更新 PPT" }}
      </button>
      <button class="ed-exit" title="退出编辑" @click="exit"><X :size="15" /></button>
    </div>

    <!-- 插入条（仿豆包顶栏：自由浮动的 WPS 式框，可拖动/拉大拉小） -->
    <div v-if="isDeck && mode === 'visual'" class="ed-insert">
      <span class="ed-insert-label">插入</span>
      <button @click="insertEl('text')"><Type :size="14" /> 文本框</button>
      <button @click="insertEl('rect')"><Square :size="14" /> 矩形</button>
      <button @click="insertEl('ellipse')"><Circle :size="14" /> 圆形</button>
      <button @click="insertEl('line')"><Minus :size="14" /> 线条</button>
      <button @click="pickImage"><ImageIcon :size="14" /> 图片</button>
      <span class="ed-insert-tip">插入后拖动移动、拖角拉大拉小，双击文本框改字</span>
    </div>
    <input ref="fileInput" type="file" accept="image/*" style="display:none" @change="onImagePicked" />

    <!-- 主体 -->
    <div class="ed-body">
      <!-- 缩略大纲（deck + 可视化） -->
      <aside v-if="isDeck && mode === 'visual'" class="ed-rail">
        <button
          v-for="(s, i) in slides"
          :key="i"
          class="ed-thumb"
          :class="{ on: i === cur }"
          :title="s.title"
          @click="goSlide(i)"
        >
          <span class="ed-thumb-n">{{ i + 1 }}</span>
          <span class="ed-thumb-prev">
            <iframe
              v-if="thumbs[i]"
              class="ed-thumb-frame"
              :srcdoc="thumbs[i]"
              sandbox=""
              scrolling="no"
              tabindex="-1"
              aria-hidden="true"
            />
            <span v-else class="ed-thumb-ph">{{ s.title }}</span>
          </span>
        </button>
        <div class="ed-rail-acts">
          <button class="ed-rail-btn" title="新增一页（空白同款）" @click="addSlide(false)"><Plus :size="13" /> 加页</button>
          <button class="ed-rail-btn" title="复制当前页" @click="addSlide(true)"><Copy :size="12" /></button>
          <button class="ed-rail-btn danger" title="删除当前页" :disabled="total <= 1" @click="deleteSlide"><Trash2 :size="12" /></button>
        </div>
      </aside>

      <!-- 画布 -->
      <div v-show="mode === 'visual'" ref="canvasEl" class="ed-canvas">
        <div ref="stageEl" class="ed-stage" :style="stageStyle">
          <iframe
            ref="frame"
            class="ed-frame"
            :srcdoc="frameSrc"
            sandbox="allow-scripts allow-same-origin"
            @load="onFrameLoad"
          />
        </div>
        <div class="ed-hint">
          <Maximize :size="12" />
          {{ isDeck ? "单击选中元素 · 拖动移动 · 拖角缩放 · 双击改文字 · Del 删除" : "整页可编辑文字" }} · Ctrl+S 保存
        </div>
      </div>

      <!-- 源码 -->
      <div v-show="mode === 'code'" class="ed-code">
        <textarea
          v-model="html"
          class="ed-code-area"
          spellcheck="false"
          @input="artifacts.markDirty(true)"
        />
      </div>

      <!-- 右侧属性面板（仿豆包「格式」模块） -->
      <aside v-if="isDeck && mode === 'visual'" class="ed-panel">
        <template v-if="selEl">
          <div class="ep-head"><span>元素格式</span><button class="ep-x" title="取消选中" @click="clearSel"><X :size="14" /></button></div>

          <div class="ep-sec">
            <div class="ep-label">对齐</div>
            <div class="ep-btns">
              <button :class="{ on: selStyle.align === 'left' }" title="左对齐" @click="fmtAlign('left')"><AlignLeft :size="15" /></button>
              <button :class="{ on: selStyle.align === 'center' }" title="居中" @click="fmtAlign('center')"><AlignCenter :size="15" /></button>
              <button :class="{ on: selStyle.align === 'right' }" title="右对齐" @click="fmtAlign('right')"><AlignRight :size="15" /></button>
            </div>
          </div>

          <div class="ep-sec">
            <div class="ep-label">位置与大小</div>
            <div class="ep-grid">
              <label class="ep-field"><span>W</span><input type="number" :value="selGeom.w" @change="setGeom('w', +($event.target as HTMLInputElement).value)" /></label>
              <label class="ep-field"><span>H</span><input type="number" :value="selGeom.h" @change="setGeom('h', +($event.target as HTMLInputElement).value)" /></label>
              <label class="ep-field"><span>X</span><input type="number" :value="selGeom.x" @change="setGeom('x', +($event.target as HTMLInputElement).value)" /></label>
              <label class="ep-field"><span>Y</span><input type="number" :value="selGeom.y" @change="setGeom('y', +($event.target as HTMLInputElement).value)" /></label>
              <label class="ep-field"><RotateCw :size="12" /><input type="number" :value="selGeom.rot" @change="setGeom('rot', +($event.target as HTMLInputElement).value)" /></label>
            </div>
          </div>

          <div class="ep-sec">
            <div class="ep-label">文字</div>
            <div class="ep-row">
              <div class="ep-stepper">
                <button title="减小字号" @click="fmtFont(-2)"><Minus :size="13" /></button>
                <span>{{ selStyle.size || "–" }}</span>
                <button title="增大字号" @click="fmtFont(2)"><Plus :size="13" /></button>
              </div>
              <div class="ep-btns">
                <button :class="{ on: selStyle.bold }" title="加粗" @click="fmtBold"><Bold :size="15" /></button>
                <button :class="{ on: selStyle.italic }" title="斜体" @click="fmtItalic"><Italic :size="15" /></button>
                <button :class="{ on: selStyle.underline }" title="下划线" @click="fmtUnderline"><Underline :size="15" /></button>
              </div>
            </div>
            <label class="ep-color">
              <span>文字颜色</span>
              <span class="ep-color-sw" :style="{ background: selStyle.color }"><input type="color" :value="selStyle.color" @input="fmtColor" /></span>
            </label>
          </div>

          <div class="ep-sec">
            <div class="ep-label">段落</div>
            <div class="ep-grid">
              <label class="ep-field"><span>行高</span><input type="number" step="0.1" :value="selPara.lh" @change="setPara('lh', +($event.target as HTMLInputElement).value)" /></label>
              <label class="ep-field"><span>字距</span><input type="number" :value="selPara.ls" @change="setPara('ls', +($event.target as HTMLInputElement).value)" /></label>
            </div>
          </div>

          <div class="ep-sec">
            <div class="ep-label">填充与描边</div>
            <label class="ep-color">
              <span>填充{{ selFill.hasBg ? "" : "（无）" }}</span>
              <span class="ep-fill-end">
                <span class="ep-color-sw" :style="{ background: selFill.hasBg ? selFill.bg : 'transparent' }"><input type="color" :value="selFill.bg" @input="fmtFill" /></span>
                <button v-if="selFill.hasBg" class="ep-clear" title="清除填充" @click.prevent="fmtFillClear"><X :size="12" /></button>
              </span>
            </label>
            <label class="ep-color">
              <span>描边</span>
              <span class="ep-color-sw" :style="{ background: selFill.border }"><input type="color" :value="selFill.border" @input="fmtBorderColor" /></span>
            </label>
            <div class="ep-grid">
              <label class="ep-field"><span>边宽</span><input type="number" :value="selFill.bw" @change="fmtBorderWidth(+($event.target as HTMLInputElement).value)" /></label>
              <label class="ep-field"><span>圆角</span><input type="number" :value="selFill.radius" @change="fmtRadius(+($event.target as HTMLInputElement).value)" /></label>
            </div>
          </div>

          <div class="ep-sec">
            <div class="ep-label">层级</div>
            <div class="ep-row">
              <button class="ep-layer" @click="fmtFront"><BringToFront :size="14" /> 置顶层</button>
              <button class="ep-layer" @click="fmtBack"><SendToBack :size="14" /> 置底层</button>
            </div>
          </div>

          <div class="ep-acts">
            <button class="danger" @click="fmtDelete"><Trash2 :size="14" /> 删除元素</button>
          </div>
        </template>

        <div v-else class="ep-empty">
          <MousePointer2 :size="22" :stroke-width="1.6" />
          <div class="ep-empty-t">单击画布里的文字或卡片</div>
          <div class="ep-empty-s">选中后在这里改大小 / 位置 / 字号 / 颜色 / 对齐，<br>拖动移动、拖角缩放，双击改文字</div>
        </div>
      </aside>
    </div>
  </div>
</template>

<style scoped>
.ed { position: absolute; inset: 0; display: flex; flex-direction: column; background: var(--bg-soft); z-index: 5; }

/* 工具栏 */
.ed-bar { display: flex; align-items: center; gap: 10px; padding: 8px 12px; border-bottom: 1px solid var(--border-soft); background: var(--panel); flex-wrap: wrap; }
.ed-seg { display: inline-flex; padding: 2px; gap: 2px; background: var(--bg-soft); border: 1px solid var(--border-soft); border-radius: 8px; }
.ed-seg button { display: inline-flex; align-items: center; gap: 5px; padding: 5px 11px; border: none; background: transparent; color: var(--muted); font-size: 12.5px; font-weight: 600; border-radius: 6px; cursor: pointer; }
.ed-seg button.on { background: var(--primary); color: #fff; }
.ed-nav { display: inline-flex; align-items: center; gap: 4px; }
.ed-page { font-size: 12.5px; color: var(--text-2); min-width: 46px; text-align: center; font-variant-numeric: tabular-nums; }
.ed-ic { width: 28px; height: 28px; display: inline-flex; align-items: center; justify-content: center; border: 1px solid var(--border); border-radius: 7px; background: var(--bg); color: var(--text-2); cursor: pointer; }
.ed-ic:hover:not(:disabled) { border-color: var(--primary); color: var(--primary); }
.ed-ic:disabled { opacity: .4; cursor: default; }
.ed-theme { display: inline-flex; align-items: center; gap: 5px; color: var(--muted); }
.ed-theme select { border: 1px solid var(--border); border-radius: 7px; background: var(--bg); color: var(--text); font-size: 12px; padding: 5px 6px; cursor: pointer; max-width: 120px; }
.ed-zoom { display: inline-flex; align-items: center; gap: 3px; }
.ed-pct { min-width: 50px; padding: 5px 6px; border: 1px solid var(--border); border-radius: 7px; background: var(--bg); color: var(--text-2); font-size: 12px; cursor: pointer; font-variant-numeric: tabular-nums; }
.ed-pct:hover { border-color: var(--primary); color: var(--primary); }
.ed-spacer { flex: 1; }
.ed-dirty { font-size: 11.5px; color: var(--warn, #c98500); }
.ed-ok { font-size: 11.5px; color: var(--good, #1aaf6c); }
.ed-err { font-size: 11.5px; color: var(--vermilion); }
.ed-save { display: inline-flex; align-items: center; gap: 6px; padding: 7px 16px; border: none; border-radius: 8px; background: var(--primary); color: #fff; font-size: 13px; font-weight: 600; cursor: pointer; }
.ed-save:hover:not(:disabled) { filter: brightness(1.07); }
.ed-save:disabled { opacity: .5; cursor: default; }
/* 「更新 PPT」：描边样式与实底「保存」区分主次 */
.ed-save.pptx { background: var(--primary-soft); color: var(--primary-deep); border: 1px solid var(--primary); }
.ed-exit { width: 30px; height: 30px; display: inline-flex; align-items: center; justify-content: center; border: 1px solid var(--border); border-radius: 8px; background: var(--bg); color: var(--muted); cursor: pointer; }
.ed-exit:hover { border-color: var(--vermilion); color: var(--vermilion); }

/* 插入条 */
.ed-insert { display: flex; align-items: center; gap: 6px; padding: 6px 12px; border-bottom: 1px solid var(--border-soft); background: var(--bg-soft); flex-wrap: wrap; }
.ed-insert-label { font-size: 11px; font-weight: 700; letter-spacing: .06em; color: var(--dim); margin-right: 2px; }
.ed-insert button { display: inline-flex; align-items: center; gap: 5px; padding: 5px 11px; border: 1px solid var(--border); border-radius: 7px; background: var(--bg); color: var(--text-2); font-size: 12.5px; font-weight: 500; cursor: pointer; }
.ed-insert button:hover { border-color: var(--primary); color: var(--primary); }
.ed-insert-tip { margin-left: auto; font-size: 11px; color: var(--dim); }

/* 主体 */
.ed-body { flex: 1; display: flex; min-height: 0; overflow: hidden; }

/* 缩略大纲 */
.ed-rail { width: 200px; flex-shrink: 0; overflow-y: auto; border-right: 1px solid var(--border-soft); background: var(--panel); padding: 10px; display: flex; flex-direction: column; gap: 9px; }
.ed-thumb { position: relative; display: flex; align-items: stretch; gap: 9px; padding: 0; border: none; background: transparent; cursor: pointer; }
/* 序号在缩略左侧 */
.ed-thumb-n { flex-shrink: 0; width: 18px; align-self: center; text-align: right; color: var(--muted); font-size: 11px; font-weight: 700; font-variant-numeric: tabular-nums; }
.ed-thumb.on .ed-thumb-n { color: var(--primary); }
/* 真实缩略：16:9 盒子里放等比缩放的 iframe */
.ed-thumb-prev { position: relative; flex: 1; aspect-ratio: 16 / 9; border-radius: 7px; overflow: hidden; background: #fff; border: 1.5px solid var(--border-soft); box-shadow: var(--shadow-sm, 0 1px 3px rgba(0,0,0,.08)); transition: border-color .15s, box-shadow .15s; }
.ed-thumb:hover .ed-thumb-prev { border-color: var(--border-strong); }
.ed-thumb.on .ed-thumb-prev { border-color: var(--primary); box-shadow: 0 0 0 2px var(--primary-soft); }
.ed-thumb-frame { position: absolute; top: 0; left: 0; width: 1280px; height: 720px; border: 0; transform-origin: top left; transform: scale(0.119); pointer-events: none; background: #fff; }
.ed-thumb-ph { position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; padding: 4px; font-size: 10px; color: var(--muted); text-align: center; }
.ed-rail-acts { display: flex; gap: 5px; margin-top: 4px; position: sticky; bottom: 0; }
.ed-rail-btn { display: inline-flex; align-items: center; gap: 4px; padding: 7px 9px; border: 1px dashed var(--border-strong); border-radius: 8px; background: var(--bg); color: var(--text-2); font-size: 12px; cursor: pointer; }
.ed-rail-btn:hover:not(:disabled) { border-color: var(--primary); color: var(--primary); }
.ed-rail-btn:disabled { opacity: .4; cursor: default; }
.ed-rail-btn.danger:hover:not(:disabled) { border-color: var(--vermilion); color: var(--vermilion); }
.ed-rail-btn:first-child { flex: 1; border-style: solid; justify-content: center; }

/* 画布：纯白高级感（无格子，仅极淡顶光） */
.ed-canvas { flex: 1; min-width: 0; position: relative; overflow: auto; display: flex; align-items: center; justify-content: center;
  background: radial-gradient(120% 90% at 50% 0%, #fbfcfd 0%, #ffffff 55%); }
.ed-stage { flex-shrink: 0; transform-origin: center center; box-shadow: 0 18px 50px rgba(20,30,50,.16), 0 3px 10px rgba(20,30,50,.08); border-radius: 3px; overflow: hidden; background: #fff; }
.ed-frame { width: 1280px; height: 720px; border: none; display: block; background: #fff; }
.ed-hint { position: absolute; left: 50%; bottom: 12px; transform: translateX(-50%); display: inline-flex; align-items: center; gap: 6px; padding: 5px 12px; border-radius: 999px; background: color-mix(in srgb, var(--ink, #111) 82%, transparent); color: #fff; font-size: 11.5px; white-space: nowrap; pointer-events: none; }

/* 源码 */
.ed-code { flex: 1; min-width: 0; display: flex; }
.ed-code-area { flex: 1; resize: none; border: none; padding: 16px 18px; background: #0f1115; color: #d6deeb; font-family: var(--mono); font-size: 12.5px; line-height: 1.6; tab-size: 2; outline: none; white-space: pre; overflow: auto; }

/* 右侧属性面板（仿豆包格式模块） */
.ed-panel { width: 232px; flex-shrink: 0; overflow-y: auto; border-left: 1px solid var(--border-soft); background: var(--panel); padding: 0 0 16px; }
.ep-head { position: sticky; top: 0; z-index: 1; display: flex; align-items: center; justify-content: space-between; padding: 12px 14px; border-bottom: 1px solid var(--border-soft); background: var(--panel); font-size: 13px; font-weight: 600; color: var(--text); }
.ep-x { width: 24px; height: 24px; display: inline-flex; align-items: center; justify-content: center; border: none; background: transparent; color: var(--muted); border-radius: 6px; cursor: pointer; }
.ep-x:hover { background: var(--bg-soft); color: var(--text); }
.ep-sec { padding: 12px 14px; border-bottom: 1px solid var(--border-soft); display: flex; flex-direction: column; gap: 9px; }
.ep-label { font-size: 11px; font-weight: 700; letter-spacing: .08em; text-transform: uppercase; color: var(--dim); }
.ep-row { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
.ep-btns { display: inline-flex; gap: 3px; padding: 2px; background: var(--bg-soft); border-radius: 8px; }
.ep-btns button { width: 30px; height: 28px; display: inline-flex; align-items: center; justify-content: center; border: none; background: transparent; color: var(--text-2); border-radius: 6px; cursor: pointer; }
.ep-btns button:hover { background: var(--panel); color: var(--text); }
.ep-btns button.on { background: var(--primary); color: #fff; }
.ep-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 7px; }
.ep-field { display: flex; align-items: center; gap: 5px; padding: 5px 8px; border: 1px solid var(--border); border-radius: 7px; background: var(--bg); }
.ep-field span { font-size: 11px; color: var(--muted); flex-shrink: 0; min-width: 12px; }
.ep-field input { width: 100%; min-width: 0; border: none; background: transparent; color: var(--text); font-size: 12.5px; outline: none; font-variant-numeric: tabular-nums; }
.ep-field input::-webkit-inner-spin-button { opacity: .4; }
.ep-stepper { display: inline-flex; align-items: center; gap: 2px; padding: 2px; background: var(--bg-soft); border-radius: 8px; }
.ep-stepper button { width: 26px; height: 28px; display: inline-flex; align-items: center; justify-content: center; border: none; background: transparent; color: var(--text-2); border-radius: 6px; cursor: pointer; }
.ep-stepper button:hover { background: var(--panel); color: var(--text); }
.ep-stepper span { min-width: 26px; text-align: center; font-size: 12.5px; color: var(--text); font-variant-numeric: tabular-nums; }
.ep-color { display: flex; align-items: center; justify-content: space-between; padding: 7px 10px; border: 1px solid var(--border); border-radius: 8px; background: var(--bg); cursor: pointer; font-size: 12.5px; color: var(--text-2); }
.ep-color-sw { position: relative; width: 30px; height: 18px; border-radius: 5px; border: 1px solid var(--border-strong); overflow: hidden; background-image: linear-gradient(45deg, #ddd 25%, transparent 25%), linear-gradient(-45deg, #ddd 25%, transparent 25%), linear-gradient(45deg, transparent 75%, #ddd 75%), linear-gradient(-45deg, transparent 75%, #ddd 75%); background-size: 8px 8px; background-position: 0 0, 0 4px, 4px -4px, -4px 0; }
.ep-color-sw input { position: absolute; inset: -4px; width: 200%; height: 200%; opacity: 0; cursor: pointer; }
.ep-fill-end { display: inline-flex; align-items: center; gap: 6px; }
.ep-clear { width: 22px; height: 18px; display: inline-flex; align-items: center; justify-content: center; border: 1px solid var(--border); border-radius: 5px; background: var(--bg); color: var(--muted); cursor: pointer; }
.ep-clear:hover { border-color: var(--vermilion); color: var(--vermilion); }
.ep-layer { flex: 1; display: inline-flex; align-items: center; justify-content: center; gap: 5px; padding: 7px; border: 1px solid var(--border); border-radius: 8px; background: var(--bg); color: var(--text-2); font-size: 12px; cursor: pointer; }
.ep-layer:hover { border-color: var(--primary); color: var(--primary); }
.ep-acts { display: flex; gap: 8px; padding: 12px 14px; }
.ep-acts button { flex: 1; display: inline-flex; align-items: center; justify-content: center; gap: 5px; padding: 8px; border: 1px solid var(--border); border-radius: 8px; background: var(--bg); color: var(--text-2); font-size: 12.5px; cursor: pointer; }
.ep-acts button:hover { border-color: var(--primary); color: var(--primary); }
.ep-acts button.danger:hover { border-color: var(--vermilion); color: var(--vermilion); background: var(--vermilion-soft); }
.ep-empty { display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 8px; height: 100%; padding: 40px 22px; text-align: center; color: var(--muted); }
.ep-empty-t { font-size: 13px; color: var(--text-2); font-weight: 500; }
.ep-empty-s { font-size: 11.5px; color: var(--dim); line-height: 1.6; }

.spin { animation: ed-spin .9s linear infinite; }
@keyframes ed-spin { to { transform: rotate(360deg); } }
</style>
