<script setup lang="ts">
/**
 * 成品编辑器（仿豆包）—— 在右抽屉放大态里直接编辑生成的「网页 PPT / HTML 网页」。
 * - 左侧：页面缩略大纲（deck 模式按 .slide 分页）
 * - 中间：大画布 iframe，可视化模式下整页 contenteditable，双击文字即改
 * - 顶部：可视化/源码切换、翻页、主题、缩放、保存、退出
 * 保存 = 把编辑后的完整 HTML 写回原产物文件（artifact_write）。
 * 既支持多页 deck，也支持单页网页（无 .slide 时自动隐藏分页栏）。
 */
import { ref, shallowRef, computed, onMounted, onBeforeUnmount, nextTick, watch } from "vue";
import {
  Code2, Eye, ChevronLeft, ChevronRight, Plus, Copy, Trash2,
  Save, X, Loader, Palette, ZoomIn, ZoomOut, Maximize, FileType2,
  Bold, Italic, Underline, AlignLeft, AlignCenter, AlignRight, Minus, BringToFront,
  MousePointer2, RotateCw, Type, Square, Circle, Image as ImageIcon, SendToBack,
  Undo2, Redo2, Strikethrough, List, ListOrdered, Link2, Highlighter,
  RemoveFormatting, FileText, Heading1, Heading2, Heading3, TextQuote,
  Table, Search,
  AlignStartVertical, AlignCenterVertical, AlignEndVertical,
  AlignStartHorizontal, AlignCenterHorizontal, AlignEndHorizontal,
  AlignHorizontalDistributeCenter, AlignVerticalDistributeCenter,
  Layers, LayoutList, EyeOff, Lock, LockOpen, ChevronDown,
  Hand, Frame, Group, Ungroup,
} from "@lucide/vue";
import { marked } from "marked";
import { sanitizeHtml } from "../lib/sanitize";
import { useArtifactsStore } from "../stores/artifacts";
import { DECK_THEMES } from "../lib/deckThemes";
import { figmaApi, openUrl } from "../tauri";
import { collectVectorIds, figmaFrameToHtml } from "../lib/figmaPull";

const artifacts = useArtifactsStore();

type Mode = "visual" | "code";
const mode = ref<Mode>("visual");

// ── 文档类编辑（Markdown / 纯文本）：不走 iframe 画布，走「源码 + 实时预览」──
const docKind = computed(() => artifacts.payload?.kind ?? "html");
const isTextDoc = computed(
  () => docKind.value === "markdown" || docKind.value === "text"
);

// 画布 / 源码各自的工作副本
const html = ref<string>(artifacts.payload?.text ?? "");
const frameSrc = ref<string>(html.value); // 显式控制 iframe 重载，避免源码每键回灌
// Markdown 实时预览（与 RightDrawer 预览同一条 marked+sanitize 管线）
const mdPreview = computed(() =>
  docKind.value === "markdown" ? sanitizeHtml(marked.parse(html.value) as string) : ""
);
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

// ── Figma 式设计模式 ──
// deck 永远走对象编辑；普通网页默认「设计模式」（点选/拖拽/框选），可切回「文字模式」整页改字
const pageDesign = ref(true);
const designOn = computed(() => isDeck.value || pageDesign.value);
// 多选（Shift+点选 / 空白处框选）：selEl 是主选，extraSels 是追加选
const extraSels = ref<HTMLElement[]>([]);
const allSels = computed(() => (selEl.value ? [selEl.value, ...extraSels.value] : []));
// 图层树（Figma 左栏）：扁平行 + 深度缩进，折叠靠 parentId 链过滤
type LayerNode = {
  id: number; el: HTMLElement; parentId: number | null;
  tag: string; label: string; depth: number; kids: number;
  hidden: boolean; locked: boolean;
};
const layerRows = shallowRef<LayerNode[]>([]);
const layerCollapsed = ref<Set<number>>(new Set());
const railTab = ref<"pages" | "layers">("pages");
const lids = new WeakMap<HTMLElement, number>();
let lidSeq = 1;
const layerDragId = ref<number | null>(null);
const layerDropId = ref<number | null>(null);
const layerDropPos = ref<"before" | "after" | "into" | null>(null);
// 画布平移（按住空格 / 鼠标中键拖拽）
const panHeld = ref(false);
let panDrag: { sx: number; sy: number; sl: number; st: number } | null = null;
// 框选（marquee）
let marquee: { x0: number; y0: number } | null = null;
let marqueeMoved = false;
// 工具（Figma 式底部工具条）：select=点选/框选, hand=抓手, 其余=拖拽画出元素
type Tool = "select" | "hand" | "text" | "rect" | "ellipse" | "line";
const tool = ref<Tool>("select");
let draw: { x0: number; y0: number } | null = null;
// 右键上下文菜单（zone 坐标）
const ctx = ref<{ x: number; y: number } | null>(null);
const canUngroup = computed(() => !!selEl.value?.hasAttribute("data-group"));
const zoneEl = ref<HTMLElement | null>(null);
// Alt+悬停间距标注
let lastHover: HTMLElement | null = null;

// ── Figma 往返桥: 去程复制 HTML 走 html.to.design 插件, 回程 REST 拉回替换画布 ──
const figmaOpen = ref(false);
const figmaLink = ref<string>(localStorage.getItem("polaris.figma.link") ?? "");
// PAT 只留内存:localStorage 会被同源 artifact 脚本(iframe allow-scripts+allow-same-origin)读走外传
const figmaToken = ref<string>("");
const figmaBusy = ref(false);
const figmaErr = ref("");
const figmaCopied = ref(false);
async function figmaCopyHtml() {
  figmaErr.value = "";
  const out = !isTextDoc.value && mode.value === "visual" ? serialize() : html.value;
  try {
    await navigator.clipboard.writeText(out);
    figmaCopied.value = true;
    setTimeout(() => (figmaCopied.value = false), 2000);
  } catch {
    figmaErr.value = "复制失败：切到「源码」模式手动全选复制也一样";
  }
}
async function figmaPullBack() {
  figmaErr.value = "";
  const link = figmaLink.value.trim();
  const token = figmaToken.value.trim();
  if (!link) { figmaErr.value = "先粘贴 Figma 文件链接"; return; }
  if (!token) { figmaErr.value = "需要 Figma 访问令牌：Figma 头像 → Settings → Security → Personal access tokens"; return; }
  figmaBusy.value = true;
  try {
    localStorage.setItem("polaris.figma.link", link);
    // 不落盘 token(见上:同源 artifact 可读 localStorage)
    const data = await figmaApi.pull(link, token);
    const ids = collectVectorIds(data.doc);
    const svgs = ids.length ? await figmaApi.exportSvgs(link, ids, token) : {};
    const res = figmaFrameToHtml(data, svgs);
    // 替换画布工作副本（可 Ctrl+Z 回退到 Figma 化之前, Ctrl+S 才真正落盘）
    reloadFrom(res.html, null);
    artifacts.markDirty(true);
    figmaOpen.value = false;
  } catch (e: any) {
    figmaErr.value = e?.message ?? String(e);
  } finally {
    figmaBusy.value = false;
  }
}
// 效果（不透明度 / 阴影）
const selEffects = ref<{ opacity: number; shadow: string }>({ opacity: 100, shadow: "none" });

// 缩放
const zoom = ref(1); // 用户倍率
const fitScale = ref(1);
const scale = computed(() => +(fitScale.value * zoom.value).toFixed(3));
const stageStyle = computed(() => ({
  width: "1280px",
  height: "720px",
  transform: `scale(${scale.value})`,
}));
// 舞台外包一层「按缩放后尺寸占位」的壳: transform 不改变布局尺寸, 直接放 flex 里
// 放大后画布滚动区不会跟着变大(左上还够不着)。壳的宽高 = 视觉尺寸 → 滚动/居中都正确。
const stageWrapStyle = computed(() => ({
  width: Math.round(1280 * scale.value) + "px",
  height: Math.round(720 * scale.value) + "px",
}));

const dirty = computed(() => artifacts.dirty);
const saving = computed(() => artifacts.saving);
const justSaved = ref(false);

// ── 统一撤销/重做（快照式）：拖动/缩放/删除/插入/换主题/改格式全部可撤销 ──
// 每次结构性改动后把 serialize() 快照压栈；撤销=用旧快照重载画布。
// 正在打字（contenteditable 聚焦）时让浏览器原生 undo 接管，保光标。
const history = ref<{ html: string; slide: number }[]>([]);
const hIndex = ref(-1);
const canUndo = computed(() => hIndex.value > 0);
const canRedo = computed(() => hIndex.value < history.value.length - 1);
let historyTimer = 0;
let restoring = false;

// ── 元素剪贴板（Ctrl+C/X/V/D, 支持多选整组复制）: ref 是为了右键菜单「粘贴」禁用态响应 ──
const elClipboard = ref<string[] | null>(null);

// ── 查找替换（源码模式 / Markdown / 纯文本，Ctrl+F）──
const findOpen = ref(false);
const findQ = ref("");
const replQ = ref("");
const findInput = ref<HTMLInputElement | null>(null);
const findCount = computed(() =>
  findQ.value ? html.value.split(findQ.value).length - 1 : 0
);

// 文档编辑 / 源码编辑的 textarea 与预览引用
const docTa = ref<HTMLTextAreaElement | null>(null);
const codeTa = ref<HTMLTextAreaElement | null>(null);
const mdPrevEl = ref<HTMLElement | null>(null);

// 图片替换目标（非空时 fileInput 选图 = 换掉它的 src，而不是插入新图）
let imgReplaceTarget: HTMLImageElement | null = null;

// ── 浮动气泡工具栏（现代编辑器式）：选中画布里的文字时浮现在选区上方 ──
const bubble = ref<{ x: number; y: number } | null>(null);
let selChangeHandler: (() => void) | null = null;

// 底部操作提示：进来亮 9 秒自动淡出，不长期占视野
const hintOn = ref(true);
let hintTimer = 0;

// ───────── iframe 接入 ─────────
function win(): any { return frame.value?.contentWindow as any; }
function doc(): Document | null { return frame.value?.contentDocument ?? null; }

let inputHandler: (() => void) | null = null;
let keyGuard: ((e: KeyboardEvent) => void) | null = null;
let clickGuard: ((e: MouseEvent) => void) | null = null;
let scrollHandler: (() => void) | null = null;
let keyUpGuard: ((e: KeyboardEvent) => void) | null = null;
let wheelGuard: ((e: WheelEvent) => void) | null = null;
let ctxGuard: ((e: MouseEvent) => void) | null = null;
let pasteGuard: ((e: ClipboardEvent) => void) | null = null;

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
  // 普通网页文字模式：整页可编辑文字；deck / 网页设计模式：走对象编辑（双击进文字编辑）
  if (!isDeck.value && !pageDesign.value) d.body?.setAttribute("contenteditable", "true");
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
  inputHandler = () => { if (!artifacts.dirty) artifacts.markDirty(true); refreshSel(); pushHistory(600); };
  d.addEventListener("input", inputHandler, true);
  // 编辑时拦掉 deck 自带的 ←/→ 翻页；Esc 退选/退出文字编辑；Delete 删元素；
  // Ctrl+Z/Y 统一撤销重做；Ctrl+C/V/D 复制粘贴元素；方向键微调位置
  keyGuard = (e: KeyboardEvent) => {
    if (mode.value !== "visual") return;
    const ae = d.activeElement as HTMLElement | null;
    if (ae && (ae as any).isContentEditable) {
      e.stopPropagation(); // 文字编辑中：原生快捷键（含 Ctrl+Z 撤销打字）自然生效
      if (e.key === "Escape") { ae.removeAttribute("contenteditable"); ae.blur?.(); refreshSel(); }
      return;
    }
    if (e.code === "Space") { panHeld.value = true; e.preventDefault(); e.stopPropagation(); return; }
    // Alt 按住且正悬停在别的元素上 → 立刻显示间距标注（Figma 测距）
    if (e.key === "Alt") {
      if (selEl.value && lastHover && lastHover !== selEl.value) {
        drawMeasure(selEl.value.getBoundingClientRect(), lastHover.getBoundingClientRect());
      }
      e.preventDefault(); // 别让 Alt 把焦点带去菜单
      return;
    }
    // Figma 式工具快捷键: V 选择 / H 抓手 / T 文本 / R 矩形 / O 圆 / L 线
    if (!e.ctrlKey && !e.metaKey && !e.altKey && designOn.value) {
      const tk: Record<string, Tool> = { v: "select", h: "hand", t: "text", r: "rect", o: "ellipse", l: "line" };
      const tt = tk[e.key.toLowerCase()];
      if (tt) { setTool(tt); e.preventDefault(); e.stopPropagation(); return; }
      // ] / [ = 上移/下移一层（z 序微调）
      if (selEl.value && (e.key === "]" || e.key === "[")) {
        zStep(e.key === "]");
        e.preventDefault(); e.stopPropagation();
        return;
      }
    }
    if (e.key === "Escape" && ctx.value) { ctx.value = null; e.stopPropagation(); return; }
    if (e.key === "Escape" && tool.value !== "select") { setTool("select"); e.stopPropagation(); return; }
    const mod = e.ctrlKey || e.metaKey;
    const k = e.key.toLowerCase();
    if (mod && k === "a" && designOn.value) { selectAllEls(); e.preventDefault(); e.stopPropagation(); return; }
    // Ctrl+G 编组 / Ctrl+Shift+G 取消编组
    if (mod && k === "g" && designOn.value) {
      if (e.shiftKey) ungroupSel();
      else groupSel();
      e.preventDefault(); e.stopPropagation();
      return;
    }
    if (mod && k === "z") { if (e.shiftKey) redo(); else undo(); e.preventDefault(); e.stopPropagation(); return; }
    if (mod && k === "y") { redo(); e.preventDefault(); e.stopPropagation(); return; }
    if (selEl.value) {
      if (mod && k === "c") { copyEl(); e.preventDefault(); e.stopPropagation(); return; }
      if (mod && k === "x") { cutEl(); e.preventDefault(); e.stopPropagation(); return; }
      if (mod && k === "d") { duplicateEl(); e.preventDefault(); e.stopPropagation(); return; }
      if (e.key.startsWith("Arrow")) {
        const s = e.shiftKey ? 10 : 1;
        nudge(
          e.key === "ArrowLeft" ? -s : e.key === "ArrowRight" ? s : 0,
          e.key === "ArrowUp" ? -s : e.key === "ArrowDown" ? s : 0
        );
        e.preventDefault(); e.stopPropagation(); return;
      }
      if (e.key === "Escape") { clearSel(); e.stopPropagation(); return; }
      if (e.key === "Delete") { fmtDelete(); e.stopPropagation(); return; }
    }
    if (mod && k === "v") {
      // 有元素剪贴板 → 贴元素; 否则放行原生 paste（贴系统剪贴板里的截图/图片, 由 paste 监听接住）
      if (elClipboard.value?.length) { pasteEl(); e.preventDefault(); e.stopPropagation(); }
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
  // 选区变化 → 浮动气泡工具栏跟随（选中文字浮现、失焦隐去）
  bubble.value = null;
  selChangeHandler = () => updateBubble();
  d.addEventListener("selectionchange", selChangeHandler);
  // 长网页在 iframe 里滚动时，选中框（fixed 定位）要跟着刷新
  scrollHandler = () => { refreshSel(); updateBubble(); };
  d.addEventListener("scroll", scrollHandler, true);
  // 空格抓手松开（按下在 keyGuard 里）; Alt 松开收起测距标注
  keyUpGuard = (e: KeyboardEvent) => {
    if (e.code === "Space") panHeld.value = false;
    if (e.key === "Alt") clearMeasure();
  };
  d.addEventListener("keyup", keyUpGuard, true);
  // 右键 = Figma 式上下文菜单（复制/粘贴/层级/编组/锁定…）
  ctxGuard = (e: MouseEvent) => {
    if (mode.value !== "visual" || !designOn.value) return;
    e.preventDefault();
    e.stopPropagation();
    const pick = selectableFrom(e.target as HTMLElement);
    if (pick && !allSels.value.includes(pick)) selectEl(pick);
    else if (!pick) clearSel();
    openCtxAt(e.clientX, e.clientY);
  };
  d.addEventListener("contextmenu", ctxGuard, true);
  // 系统剪贴板贴图: 复制截图后 Ctrl+V 直接落到画布中央
  pasteGuard = (e: ClipboardEvent) => onPasteClipboard(e);
  d.addEventListener("paste", pasteGuard, true);
  // Ctrl+滚轮落在 iframe 里时不会冒泡到父画布 → 在 iframe 文档里接住, 换算回画布坐标做锚点缩放
  wheelGuard = (e: WheelEvent) => {
    if (!e.ctrlKey) return;
    e.preventDefault();
    e.stopPropagation();
    const c = canvasEl.value;
    const st = stageEl.value;
    if (!c || !st) return;
    const cr = c.getBoundingClientRect();
    const sr2 = st.getBoundingClientRect();
    zoomAt(
      sr2.left - cr.left + e.clientX * scale.value,
      sr2.top - cr.top + e.clientY * scale.value,
      e.deltaY < 0 ? 1.12 : 1 / 1.12
    );
  };
  d.addEventListener("wheel", wheelGuard, { passive: false, capture: true });
  syncToolCursor();
  buildLayers();
  computeFit();
  if (pendingGo != null) { const g = pendingGo; pendingGo = null; goSlide(g); }
  // 撤销/重做触发的重载不再压栈；其余（首载/加删页/换页序/切回可视化）落一版快照基线
  if (restoring) restoring = false;
  else pushHistory();
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
  buildLayers();
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
  pushHistory();
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
  if (t.closest("[data-plk]")) return null; // 图层面板锁定的元素不可选中

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
      h.addEventListener("pointerdown", (e) => startResize(e as PointerEvent, dir));
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

function positionOverlay() {
  const d = doc();
  const box = d?.getElementById("__obj") as HTMLElement | null;
  if (!d || !box) return;
  // 编辑物尺寸随画布缩放反向补偿(线宽/手柄/徽标在屏幕上恒定大小, Figma 同款)
  d.documentElement.style.setProperty("--edscale", String(scale.value || 1));
  // 多选时不显示手柄框（各元素用 __msel 描边），只有单选才有 8 手柄
  if (!selEl.value || !selBox.value || extraSels.value.length) { box.style.display = "none"; return; }
  const b = selBox.value;
  box.style.display = "block";
  box.style.left = b.x + "px";
  box.style.top = b.y + "px";
  box.style.width = b.w + "px";
  box.style.height = b.h + "px";
  const sz = d.getElementById("__sz");
  if (sz) sz.textContent = `${Math.round(b.w)} × ${Math.round(b.h)}`;
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
  // box-shadow 读回来会被浏览器规范化重写，按特征偏移量映射回预设值
  const bs = el.style.boxShadow;
  selEffects.value = {
    opacity: Math.round((parseFloat(cs.opacity) || 1) * 100),
    shadow: !bs || bs === "none" ? "none"
      : bs.includes("2px 8px") ? SHADOWS[1].v
      : bs.includes("6px 20px") ? SHADOWS[2].v
      : bs.includes("14px 40px") ? SHADOWS[3].v
      : bs,
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
/** 同步多选描边：>1 个选中时每个元素都描边（单选走 #__obj 手柄框） */
function syncSelClass() {
  const d = doc();
  if (!d) return;
  d.querySelectorAll(".__msel").forEach((x) => x.classList.remove("__msel"));
  if (allSels.value.length > 1) for (const el of allSels.value) el.classList.add("__msel");
}
function selectEl(el: HTMLElement | null, additive = false) {
  // Shift+点选：追加 / 反选
  if (additive && el && selEl.value) {
    if (el === selEl.value) {
      // 反选主选：让第一个追加选顶上来
      if (extraSels.value.length) selEl.value = extraSels.value.shift()!;
      else selEl.value = null;
    } else {
      const i = extraSels.value.indexOf(el);
      if (i >= 0) extraSels.value.splice(i, 1);
      else extraSels.value.push(el);
    }
    syncSelClass();
    refreshSel();
    return;
  }
  // 退出上一个文字编辑
  if (selEl.value && selEl.value !== el) selEl.value.removeAttribute("contenteditable");
  extraSels.value = [];
  selEl.value = el;
  syncSelClass();
  refreshSel();
}
function clearSel() { selectEl(null); }

// 拖动 / 缩放
let drag: null | {
  kind: "move" | "resize"; dir: string;
  sx: number; sy: number; tx0: number; ty0: number; w0: number; h0: number;
  // 磁吸用：拖动起点相对页面的位置 + 页面几何（仅 move 记录）
  rel0x?: number; rel0y?: number; slx?: number; sly?: number; sw?: number; sh?: number;
  // 多选群移：其余选中元素的初始位移
  others?: { el: HTMLElement; tx0: number; ty0: number }[];
  // Figma 式智能吸附参照(页面相对坐标): 页面三线 + 兄弟元素的边缘/中线
  snapX?: number[]; snapY?: number[];
} = null;

/** 磁吸参考线开关（页面中线；x/y 为 iframe 视口坐标） */
function showGuides(v: boolean, h: boolean, x = 0, y = 0) {
  const d = doc();
  if (!d) return;
  const gv = d.getElementById("__gv") as HTMLElement | null;
  const gh = d.getElementById("__gh") as HTMLElement | null;
  if (gv) { gv.style.display = v ? "block" : "none"; if (v) gv.style.left = x + "px"; }
  if (gh) { gh.style.display = h ? "block" : "none"; if (h) gh.style.top = y + "px"; }
}

function startMove(e: PointerEvent) {
  // Alt+拖 = 拖出副本（Figma 手感）: 先原位克隆整组选中, 把选中换成副本再开拖
  if (e.altKey && selEl.value) duplicateForAltDrag();
  const el = selEl.value;
  if (!el) return;
  const t = getTranslate(el);
  const r = el.getBoundingClientRect();
  const root = activeSlide();
  const sr = root?.getBoundingClientRect();
  // 吸附参照(页面相对坐标): 页面左/中/右 + 每个未选中兄弟元素的边缘与中线
  const snapX: number[] = [];
  const snapY: number[] = [];
  if (sr && root) {
    snapX.push(0, sr.width / 2, sr.width);
    snapY.push(0, sr.height / 2, sr.height);
    const sel = allSels.value;
    for (const sib of marqueeTargets(root)) {
      if (sel.includes(sib)) continue;
      const b = sib.getBoundingClientRect();
      snapX.push(b.left - sr.left, b.left - sr.left + b.width / 2, b.right - sr.left);
      snapY.push(b.top - sr.top, b.top - sr.top + b.height / 2, b.bottom - sr.top);
    }
  }
  drag = {
    kind: "move", dir: "", sx: e.clientX, sy: e.clientY, tx0: t.tx, ty0: t.ty, w0: r.width, h0: r.height,
    rel0x: sr ? r.left - sr.left : undefined, rel0y: sr ? r.top - sr.top : undefined,
    slx: sr?.left, sly: sr?.top, sw: sr?.width, sh: sr?.height,
    others: extraSels.value.map((x) => { const tt = getTranslate(x); return { el: x, tx0: tt.tx, ty0: tt.ty }; }),
    snapX, snapY,
  };
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
  // 抓手平移：用 screen 坐标（iframe 视口坐标会随滚动漂移）
  if (panDrag) {
    const c = canvasEl.value;
    if (c) {
      c.scrollLeft = panDrag.sl - (e.screenX - panDrag.sx);
      c.scrollTop = panDrag.st - (e.screenY - panDrag.sy);
    }
    return;
  }
  if (draw) { moveDraw(e); return; }
  if (marquee) { moveMarquee(e); return; }
  if (!drag || !selEl.value) return;
  const el = selEl.value;
  let dx = e.clientX - drag.sx, dy = e.clientY - drag.sy;
  if (drag.kind === "move") {
    // Shift+拖 = 锁定水平/垂直轴（按位移大的方向走直线）
    if (e.shiftKey) {
      if (Math.abs(dx) > Math.abs(dy)) dy = 0;
      else dx = 0;
    }
    // Figma 式智能吸附: 移动框的左/中/右(上/中/下)贴近任一参照线 ±5px 时吸附并亮红色参考线
    const SNAP = 5;
    let gx: number | null = null;
    let gy: number | null = null;
    if (drag.rel0x != null && drag.snapX?.length) {
      const cand = [drag.rel0x + dx, drag.rel0x + dx + drag.w0 / 2, drag.rel0x + dx + drag.w0];
      let best = SNAP + 1, at = 0;
      for (const s of drag.snapX)
        for (const c of cand) {
          const d2 = s - c;
          if (Math.abs(d2) < Math.abs(best)) { best = d2; at = s; }
        }
      if (Math.abs(best) <= SNAP) { dx += best; gx = at; }
    }
    if (drag.rel0y != null && drag.snapY?.length) {
      const cand = [drag.rel0y + dy, drag.rel0y + dy + drag.h0 / 2, drag.rel0y + dy + drag.h0];
      let best = SNAP + 1, at = 0;
      for (const s of drag.snapY)
        for (const c of cand) {
          const d2 = s - c;
          if (Math.abs(d2) < Math.abs(best)) { best = d2; at = s; }
        }
      if (Math.abs(best) <= SNAP) { dy += best; gy = at; }
    }
    showGuides(gx != null, gy != null, (drag.slx ?? 0) + (gx ?? 0), (drag.sly ?? 0) + (gy ?? 0));
    setTranslate(el, drag.tx0 + dx, drag.ty0 + dy);
    if (drag.others) for (const o of drag.others) setTranslate(o.el, o.tx0 + dx, o.ty0 + dy);
  } else {
    let w = drag.w0, h = drag.h0;
    const d = drag.dir;
    if (d.includes("e")) w = drag.w0 + dx;
    if (d.includes("s")) h = drag.h0 + dy;
    if (d.includes("w")) w = drag.w0 - dx;
    if (d.includes("n")) h = drag.h0 - dy;
    // Shift+角手柄 = 等比缩放（以变化幅度大的一边为准, Figma 手感）
    if (e.shiftKey && d.length === 2) {
      const kx = w / Math.max(1, drag.w0);
      const ky = h / Math.max(1, drag.h0);
      const k = Math.abs(kx - 1) > Math.abs(ky - 1) ? kx : ky;
      w = drag.w0 * k;
      h = drag.h0 * k;
    }
    w = Math.max(24, w);
    h = Math.max(20, h);
    // 西/北侧手柄: 位置随尺寸变化补偿, 保持对边不动
    const tx = d.includes("w") ? drag.tx0 + (drag.w0 - w) : drag.tx0;
    const ty = d.includes("n") ? drag.ty0 + (drag.h0 - h) : drag.ty0;
    el.style.width = Math.round(w) + "px";
    el.style.height = Math.round(h) + "px";
    setTranslate(el, tx, ty);
  }
  refreshSel();
}
function onDocPointerUp(e: PointerEvent) {
  if (panDrag) { panDrag = null; return; }
  if (draw) { endDraw(e); return; }
  if (marquee) { endMarquee(e); return; }
  if (drag) { drag = null; showGuides(false, false); artifacts.markDirty(true); refreshSel(); pushHistory(); }
}

function onDocPointerDown(e: PointerEvent) {
  if (mode.value !== "visual") return;
  // 空格按住 / 抓手工具 / 鼠标中键 = 抓手平移画布（文字模式也生效）
  if (panHeld.value || tool.value === "hand" || e.button === 1) {
    const c = canvasEl.value;
    if (c) {
      panDrag = { sx: e.screenX, sy: e.screenY, sl: c.scrollLeft, st: c.scrollTop };
      (e.target as HTMLElement).setPointerCapture?.(e.pointerId);
      e.preventDefault();
      e.stopPropagation();
    }
    return;
  }
  if (ctx.value) ctx.value = null; // 任何按下先收起右键菜单
  clearMeasure();
  if (!designOn.value) return;
  // 画形状工具: 按下即开始拖出元素（Figma 式 draw-to-create）
  if (tool.value !== "select") { startDraw(e); return; }
  const t = e.target as HTMLElement;
  if (t.closest("#__obj")) return; // 点到手柄 → 各自处理
  // Ctrl+点选 = 穿透到指针下最深的元素（Figma 深选）; 普通点选 = 语义块
  const pick = e.ctrlKey || e.metaKey ? deepFrom(t) : selectableFrom(t);
  if (pick) {
    if (e.shiftKey) { selectEl(pick, true); return; } // Shift 追加/反选，不进入拖动
    if (!allSels.value.includes(pick)) selectEl(pick);
    // 已选中且不是正在文字编辑 → 开始拖动（多选时整组一起动）
    if (!(pick as any).isContentEditable) startMove(e);
  } else {
    startMarquee(e); // 空白处按下 = 框选（没拖出面积就当点空白退选）
  }
}
function onDocDblClick(e: PointerEvent) {
  if (mode.value !== "visual" || !designOn.value) return;
  const el = selEl.value || selectableFrom(e.target as HTMLElement);
  if (!el) return;
  selectEl(el);
  // 双击图片 = 直接换图（图片没有文字可编辑）
  if (el.tagName === "IMG") { replaceImage(); return; }
  el.setAttribute("contenteditable", "true");
  (el as HTMLElement).focus?.();
}
function onDocMouseOver(e: MouseEvent) {
  if (mode.value !== "visual" || !designOn.value || marquee || drag || panDrag || draw) return;
  if (tool.value !== "select") return; // 画形状/抓手时不做悬停高亮
  const d = doc(); if (!d) return;
  d.querySelectorAll(".__hov").forEach((x) => x.classList.remove("__hov"));
  const pick = selectableFrom(e.target as HTMLElement);
  if (pick && pick !== selEl.value) pick.classList.add("__hov");
  // Alt+悬停 = 显示选中元素到悬停元素的间距标注（Figma 测距）
  lastHover = pick;
  if (e.altKey && selEl.value && pick && pick !== selEl.value) {
    drawMeasure(selEl.value.getBoundingClientRect(), pick.getBoundingClientRect());
  } else {
    clearMeasure();
  }
}

// 格式工具栏动作
function fmtBold() { const el = selEl.value; if (!el) return; el.style.fontWeight = selStyle.value.bold ? "400" : "800"; afterFmt(); }
function fmtItalic() { const el = selEl.value; if (!el) return; el.style.fontStyle = selStyle.value.italic ? "normal" : "italic"; afterFmt(); }
function fmtUnderline() { const el = selEl.value; if (!el) return; el.style.textDecoration = selStyle.value.underline ? "none" : "underline"; afterFmt(); }
function fmtAlign(a: string) { const el = selEl.value; if (!el) return; el.style.textAlign = a; afterFmt(); }
function fmtFont(delta: number) { const el = selEl.value; if (!el) return; el.style.fontSize = Math.max(8, selStyle.value.size + delta) + "px"; afterFmt(); }
function fmtColor(e: Event) { const el = selEl.value; if (!el) return; const c = (e.target as HTMLInputElement).value; el.style.color = c; (el.style as any).webkitTextFillColor = c; afterFmt(); }
// 删除走统一快照历史：误删 Ctrl+Z 一步找回（连带位置/格式一起还原）
function fmtDelete() {
  const els = allSels.value;
  if (!els.length) return;
  els.forEach((el) => el.remove());
  clearSel();
  artifacts.markDirty(true);
  rebuildThumbs();
  pushHistory();
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
  // 普通网页可能已滚动：插到当前可视区中心而不是页顶
  const scy = slide.tagName === "BODY" ? (win()?.scrollY || 0) : 0;
  el.style.left = Math.round((1280 - w) / 2) + "px";
  el.style.top = Math.round(scy + (720 - (h ?? 80)) / 2) + "px";
  el.style.width = w + "px";
  if (h != null) el.style.height = h + "px";
  el.style.zIndex = "40";
  slide.appendChild(el);
  selectEl(el);
  artifacts.markDirty(true);
  rebuildThumbs();
  pushHistory();
}
function pickImage() { fileInput.value?.click(); }
function onImagePicked(e: Event) {
  const f = (e.target as HTMLInputElement).files?.[0];
  (e.target as HTMLInputElement).value = "";
  if (!f) {
    imgReplaceTarget = null;
    return;
  }
  const rd = new FileReader();
  rd.onload = () => {
    const src = rd.result as string;
    const d = doc();
    if (!d || !src) return;
    // 替换模式：只换选中图片的 src，位置尺寸原样保留
    if (imgReplaceTarget) {
      imgReplaceTarget.src = src;
      imgReplaceTarget = null;
      artifacts.markDirty(true);
      rebuildThumbs();
      refreshSel();
      pushHistory();
      return;
    }
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
function afterFmt() { artifacts.markDirty(true); refreshSel(); pushHistory(400); }

// ───────── 统一撤销/重做 ─────────
function pushHistoryNow() {
  if (restoring || isTextDoc.value) return;
  buildLayers(); // 图层树跟着每次结构快照同步
  const snap = serialize();
  const top = history.value[hIndex.value];
  if (top && top.html === snap) return; // 没变不重复记
  history.value = history.value.slice(0, hIndex.value + 1);
  history.value.push({ html: snap, slide: cur.value });
  if (history.value.length > 50) history.value.shift();
  hIndex.value = history.value.length - 1;
}
/** debounceMs>0：连续输入/连点格式钮合并成一步 */
function pushHistory(debounceMs = 0) {
  clearTimeout(historyTimer);
  if (!debounceMs) { pushHistoryNow(); return; }
  historyTimer = window.setTimeout(pushHistoryNow, debounceMs);
}
function restoreHistory(idx: number) {
  const h = history.value[idx];
  if (!h) return;
  clearTimeout(historyTimer); // 丢掉待压栈的旧改动，防止撤销后又被回灌
  hIndex.value = idx;
  restoring = true;
  clearSel();
  reloadFrom(h.html, h.slide);
  artifacts.markDirty(true);
}
function undo() {
  // 文字编辑中优先走浏览器原生撤销（保光标）；否则快照回退
  const d = doc();
  const ae = d?.activeElement as HTMLElement | null;
  if (ae && (ae as any).isContentEditable) { d!.execCommand("undo"); artifacts.markDirty(true); return; }
  if (canUndo.value) restoreHistory(hIndex.value - 1);
}
function redo() {
  const d = doc();
  const ae = d?.activeElement as HTMLElement | null;
  if (ae && (ae as any).isContentEditable) { d!.execCommand("redo"); artifacts.markDirty(true); return; }
  if (canRedo.value) restoreHistory(hIndex.value + 1);
}

// ───────── 元素复制 / 粘贴 / 微调 / 一键对齐 ─────────
function copyEl() {
  if (allSels.value.length) elClipboard.value = allSels.value.map((el) => el.outerHTML);
}
function cutEl() {
  if (!allSels.value.length) return;
  copyEl();
  fmtDelete();
}
function pasteEl() {
  const d = doc();
  const slide = activeSlide();
  if (!d || !slide || !elClipboard.value?.length) return;
  const pasted: HTMLElement[] = [];
  for (const html2 of elClipboard.value) {
    const tpl = d.createElement("div");
    tpl.innerHTML = html2;
    const el = tpl.firstElementChild as HTMLElement | null;
    if (!el) continue;
    el.classList.remove("__hov", "__msel");
    el.removeAttribute("contenteditable");
    // 粘贴进当前页并整体偏移一点，避免与原件完全重叠看不出来
    const t = getTranslate(el);
    setTranslate(el, t.tx + 18, t.ty + 18);
    slide.appendChild(el);
    pasted.push(el);
  }
  if (!pasted.length) return;
  selEl.value = pasted[0];
  extraSels.value = pasted.slice(1);
  syncSelClass();
  refreshSel();
  artifacts.markDirty(true);
  rebuildThumbs();
  pushHistory();
}
function duplicateEl() { if (!selEl.value) return; copyEl(); pasteEl(); }
/** Alt+拖副本: 原位克隆整组选中并把选中换成副本（随后由 startMove 拖走） */
function duplicateForAltDrag() {
  if (!selEl.value) return;
  const map = new Map<HTMLElement, HTMLElement>();
  for (const el of allSels.value) {
    const c = el.cloneNode(true) as HTMLElement;
    c.classList.remove("__hov", "__msel");
    c.removeAttribute("contenteditable");
    el.after(c);
    map.set(el, c);
  }
  selEl.value = map.get(selEl.value)!;
  extraSels.value = extraSels.value.map((x) => map.get(x)!);
  syncSelClass();
  artifacts.markDirty(true);
}
/** 方向键微调：1px，Shift=10px（多选时整组一起动） */
function nudge(dx: number, dy: number) {
  if (!allSels.value.length || (!dx && !dy)) return;
  for (const el of allSels.value) {
    const t = getTranslate(el);
    setTranslate(el, t.tx + dx, t.ty + dy);
  }
  afterFmt();
}
/** 一键对齐到页面（PPT 式：左/水平居中/右/顶/垂直居中/底） */
function alignToPage(which: "left" | "hcenter" | "right" | "top" | "vcenter" | "bottom") {
  const el = selEl.value;
  const slide = activeSlide();
  if (!el || !slide) return;
  const r = el.getBoundingClientRect();
  const sr = slide.getBoundingClientRect();
  const t = getTranslate(el);
  const relX = r.left - sr.left;
  const relY = r.top - sr.top;
  if (which === "left") setTranslate(el, t.tx - relX, t.ty);
  else if (which === "hcenter") setTranslate(el, t.tx + (sr.width - r.width) / 2 - relX, t.ty);
  else if (which === "right") setTranslate(el, t.tx + (sr.width - r.width) - relX, t.ty);
  else if (which === "top") setTranslate(el, t.tx, t.ty - relY);
  else if (which === "vcenter") setTranslate(el, t.tx, t.ty + (sr.height - r.height) / 2 - relY);
  else setTranslate(el, t.tx, t.ty + (sr.height - r.height) - relY);
  afterFmt();
}
/** 选中的是图片 → 打开选图框换 src（保持原尺寸位置） */
function replaceImage() {
  const el = selEl.value;
  if (!el || el.tagName !== "IMG") return;
  imgReplaceTarget = el as HTMLImageElement;
  fileInput.value?.click();
}

// ───────── Figma 式设计模式：模式切换 / 多选对齐分布 / 框选 / 图层树 / 抓手平移 ─────────
function setPageDesign(v: boolean) {
  if (pageDesign.value === v) return;
  pageDesign.value = v;
  clearSel();
  applyEditable();
  buildLayers();
}
/** Ctrl+A：全选当前页的顶层元素 */
function selectAllEls() {
  const root = activeSlide();
  if (!root) return;
  const t = marqueeTargets(root);
  if (!t.length) return;
  selectEl(t[0]);
  extraSels.value = t.slice(1);
  syncSelClass();
  refreshSel();
}
/** 多选相互对齐（以选中集合的外接框为基准；单选时退化为对齐到页面） */
function alignSel(which: "left" | "hcenter" | "right" | "top" | "vcenter" | "bottom") {
  const els = allSels.value;
  if (els.length < 2) { alignToPage(which); return; }
  const rs = els.map((el) => ({ el, r: el.getBoundingClientRect(), t: getTranslate(el) }));
  const minL = Math.min(...rs.map((x) => x.r.left));
  const maxR = Math.max(...rs.map((x) => x.r.right));
  const minT = Math.min(...rs.map((x) => x.r.top));
  const maxB = Math.max(...rs.map((x) => x.r.bottom));
  for (const x of rs) {
    let dx = 0, dy = 0;
    if (which === "left") dx = minL - x.r.left;
    else if (which === "hcenter") dx = (minL + maxR) / 2 - (x.r.left + x.r.width / 2);
    else if (which === "right") dx = maxR - x.r.right;
    else if (which === "top") dy = minT - x.r.top;
    else if (which === "vcenter") dy = (minT + maxB) / 2 - (x.r.top + x.r.height / 2);
    else dy = maxB - x.r.bottom;
    setTranslate(x.el, x.t.tx + dx, x.t.ty + dy);
  }
  afterFmt();
}
/** 等间距分布（≥3 个）：首尾不动，中间元素均摊间隙 */
function distributeSel(axis: "h" | "v") {
  const els = allSels.value;
  if (els.length < 3) return;
  const rs = els
    .map((el) => ({ el, r: el.getBoundingClientRect(), t: getTranslate(el) }))
    .sort((a, b) => (axis === "h" ? a.r.left - b.r.left : a.r.top - b.r.top));
  const first = rs[0], last = rs[rs.length - 1];
  const totalSize = rs.reduce((s, x) => s + (axis === "h" ? x.r.width : x.r.height), 0);
  const span = axis === "h" ? last.r.right - first.r.left : last.r.bottom - first.r.top;
  const gap = (span - totalSize) / (rs.length - 1);
  let cursor = axis === "h" ? first.r.left : first.r.top;
  for (const x of rs) {
    const d2 = cursor - (axis === "h" ? x.r.left : x.r.top);
    if (axis === "h") setTranslate(x.el, x.t.tx + d2, x.t.ty);
    else setTranslate(x.el, x.t.tx, x.t.ty + d2);
    cursor += (axis === "h" ? x.r.width : x.r.height) + gap;
  }
  afterFmt();
}
// 效果：不透明度 / 阴影
const SHADOWS = [
  { n: "无阴影", v: "none" },
  { n: "轻", v: "0 2px 8px rgba(0,0,0,.14)" },
  { n: "中", v: "0 6px 20px rgba(0,0,0,.22)" },
  { n: "重", v: "0 14px 40px rgba(0,0,0,.32)" },
];
function fmtOpacity(v: number) {
  const el = selEl.value;
  if (!el || isNaN(v)) return;
  el.style.opacity = String(Math.max(0, Math.min(100, v)) / 100);
  afterFmt();
}
function fmtShadow(e: Event) {
  const el = selEl.value;
  if (!el) return;
  const v = (e.target as HTMLSelectElement).value;
  el.style.boxShadow = v === "none" ? "" : v;
  afterFmt();
}

// ── 框选（marquee）：空白处按下拖出矩形，命中当前页顶层元素 ──
function marqueeTargets(root: HTMLElement): HTMLElement[] {
  return (Array.from(root.children) as HTMLElement[]).filter(
    (c) => !SKIP_TAGS.has(c.tagName) && !isEditorNode(c) && !c.hasAttribute("data-plk") && c.style.display !== "none"
  );
}
function startMarquee(e: PointerEvent) {
  clearSel();
  marquee = { x0: e.clientX, y0: e.clientY };
  marqueeMoved = false;
  (e.target as HTMLElement).setPointerCapture?.(e.pointerId);
}
function marqueeRect(e: PointerEvent) {
  const x = Math.min(marquee!.x0, e.clientX);
  const y = Math.min(marquee!.y0, e.clientY);
  return { x, y, w: Math.abs(e.clientX - marquee!.x0), h: Math.abs(e.clientY - marquee!.y0) };
}
function marqueeHits(r: { x: number; y: number; w: number; h: number }): HTMLElement[] {
  const root = activeSlide();
  if (!root) return [];
  return marqueeTargets(root).filter((el) => {
    const b = el.getBoundingClientRect();
    return b.left < r.x + r.w && b.right > r.x && b.top < r.y + r.h && b.bottom > r.y;
  });
}
function moveMarquee(e: PointerEvent) {
  const d = doc();
  if (!d || !marquee) return;
  const r = marqueeRect(e);
  if (r.w > 3 || r.h > 3) marqueeMoved = true;
  const rb = d.getElementById("__rb") as HTMLElement | null;
  if (rb) {
    rb.style.display = marqueeMoved ? "block" : "none";
    rb.style.left = r.x + "px";
    rb.style.top = r.y + "px";
    rb.style.width = r.w + "px";
    rb.style.height = r.h + "px";
  }
  // 实时高亮命中元素
  d.querySelectorAll(".__hov").forEach((x) => x.classList.remove("__hov"));
  if (marqueeMoved) for (const el of marqueeHits(r)) el.classList.add("__hov");
}
function endMarquee(e: PointerEvent) {
  const d = doc();
  const rb = d?.getElementById("__rb") as HTMLElement | null;
  if (rb) rb.style.display = "none";
  if (d && marquee && marqueeMoved) {
    const hit = marqueeHits(marqueeRect(e));
    d.querySelectorAll(".__hov").forEach((x) => x.classList.remove("__hov"));
    if (hit.length) {
      selEl.value = hit[0];
      extraSels.value = hit.slice(1);
      syncSelClass();
      refreshSel();
    }
  }
  marquee = null;
  marqueeMoved = false;
}

// ── Figma 深选: Ctrl+点选穿透到指针下最深的元素 ──
function deepFrom(t: HTMLElement): HTMLElement | null {
  const slide = activeSlide();
  if (!slide || !slide.contains(t) || t === slide) return null;
  if (t.closest("[data-plk]")) return null;
  if (isEditorNode(t)) return null;
  return t;
}

// ── 编组 / 取消编组（Ctrl+G / Ctrl+Shift+G）──
// 组 = 一个绝对定位的 div[data-group] 包住选中集合的外接框; 成员转组内绝对定位, 视觉位置不变。
function groupSel() {
  const d = doc();
  const root = activeSlide();
  // 过滤掉互为祖先的选择(把祖先留下会连孩子一起挪两次)
  const els = allSels.value.filter((el) => !allSels.value.some((o) => o !== el && o.contains(el)));
  if (!d || !root || els.length < 2) return;
  if (getComputedStyle(root).position === "static") root.style.position = "relative";
  const rr = root.getBoundingClientRect();
  const rects = els.map((el) => el.getBoundingClientRect());
  const minL = Math.min(...rects.map((r) => r.left));
  const minT = Math.min(...rects.map((r) => r.top));
  const maxR = Math.max(...rects.map((r) => r.right));
  const maxB = Math.max(...rects.map((r) => r.bottom));
  const g = d.createElement("div");
  g.setAttribute("data-group", "1");
  g.style.cssText = `position:absolute;left:${Math.round(minL - rr.left)}px;top:${Math.round(minT - rr.top)}px;width:${Math.round(maxR - minL)}px;height:${Math.round(maxB - minT)}px;`;
  root.appendChild(g);
  for (let i = 0; i < els.length; i++) {
    const el = els[i];
    const r = rects[i];
    el.classList.remove("__msel", "__hov");
    g.appendChild(el);
    // translate 折算进组内 left/top, 冻结当前视觉几何
    el.style.position = "absolute";
    el.style.left = Math.round(r.left - minL) + "px";
    el.style.top = Math.round(r.top - minT) + "px";
    el.style.width = Math.round(r.width) + "px";
    el.style.height = Math.round(r.height) + "px";
    el.style.transform = "";
    el.style.margin = "0";
    el.style.boxSizing = "border-box";
  }
  selectEl(g);
  artifacts.markDirty(true);
  rebuildThumbs();
  pushHistory();
}
function ungroupSel() {
  const g = selEl.value;
  if (!g || !g.hasAttribute("data-group") || !g.parentElement) return;
  const t = getTranslate(g);
  const gl = (parseFloat(g.style.left) || 0) + t.tx;
  const gt = (parseFloat(g.style.top) || 0) + t.ty;
  const kids = Array.from(g.children) as HTMLElement[];
  for (const k of kids) {
    k.style.left = Math.round((parseFloat(k.style.left) || 0) + gl) + "px";
    k.style.top = Math.round((parseFloat(k.style.top) || 0) + gt) + "px";
    g.before(k);
  }
  g.remove();
  if (kids.length) {
    selEl.value = kids[0];
    extraSels.value = kids.slice(1);
    syncSelClass();
    refreshSel();
  } else {
    clearSel();
  }
  artifacts.markDirty(true);
  rebuildThumbs();
  pushHistory();
}

// ── 层级微调: ] 上移一层 / [ 下移一层（z-index 步进）──
function zStep(upward: boolean) {
  const el = selEl.value;
  if (!el) return;
  if (getComputedStyle(el).position === "static") el.style.position = "relative";
  const z = parseInt(el.style.zIndex || getComputedStyle(el).zIndex) || 0;
  el.style.zIndex = String(Math.max(0, z + (upward ? 1 : -1)));
  afterFmt();
}

// ── Alt+悬停间距标注（Figma 测距）: 选中元素 ↔ 悬停元素之间的水平/垂直净距 ──
function clearMeasure() {
  const m = doc()?.getElementById("__meas") as HTMLElement | null;
  if (m) m.style.display = "none";
}
function drawMeasure(sel: DOMRect, hov: DOMRect) {
  const d = doc();
  const m = d?.getElementById("__meas") as HTMLElement | null;
  if (!d || !m) return;
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
  const k = scale.value || 1;
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

// ── 右键上下文菜单 ──
function openCtxAt(fx: number, fy: number) {
  const st = stageEl.value;
  const z = zoneEl.value;
  if (!st || !z) return;
  const sr = st.getBoundingClientRect();
  const zr = z.getBoundingClientRect();
  const x = sr.left - zr.left + fx * scale.value;
  const y = sr.top - zr.top + fy * scale.value;
  ctx.value = {
    x: Math.max(4, Math.min(x, zr.width - 200)),
    y: Math.max(4, Math.min(y, zr.height - 360)),
  };
}
function ctxDo(fn: () => void) {
  ctx.value = null;
  fn();
}
function closeCtx() { ctx.value = null; }
/** 右键菜单: 锁定选中元素（解锁去图层面板点锁图标） */
function lockSel() {
  const el = selEl.value;
  if (!el) return;
  el.setAttribute("data-plk", "1");
  clearSel();
  artifacts.markDirty(true);
  pushHistory();
}
/** 右键菜单: 隐藏选中元素（重新显示去图层面板点眼睛） */
function hideSel() {
  const els = allSels.value;
  if (!els.length) return;
  els.forEach((el) => (el.style.display = "none"));
  clearSel();
  artifacts.markDirty(true);
  rebuildThumbs();
  pushHistory();
}

// ── 检查器 hex 色值输入 ──
function normHex(v: string): string | null {
  const m = /^#?([0-9a-fA-F]{6})$/.exec(v.trim());
  return m ? "#" + m[1].toLowerCase() : null;
}
function fmtFillHex(e: Event) {
  const h = normHex((e.target as HTMLInputElement).value);
  const el = selEl.value;
  if (!h || !el) return;
  el.style.backgroundColor = h;
  afterFmt();
}
function fmtColorHex(e: Event) {
  const h = normHex((e.target as HTMLInputElement).value);
  const el = selEl.value;
  if (!h || !el) return;
  el.style.color = h;
  (el.style as any).webkitTextFillColor = h;
  afterFmt();
}

// ── 系统剪贴板贴图: 复制截图/图片后 Ctrl+V 直接落画布（成 base64 内嵌 img）──
function onPasteClipboard(e: ClipboardEvent) {
  if (mode.value !== "visual" || isTextDoc.value || !designOn.value) return;
  const ae = doc()?.activeElement as HTMLElement | null;
  if (ae && (ae as any).isContentEditable) return; // 文字编辑中 → 原生贴字
  const pae = document.activeElement;
  if (pae && (pae.tagName === "INPUT" || pae.tagName === "TEXTAREA")) return; // 父页输入框 → 原生
  const items = e.clipboardData?.items;
  if (!items) return;
  for (const it of Array.from(items)) {
    if (!it.type.startsWith("image/")) continue;
    const f = it.getAsFile();
    if (!f) continue;
    e.preventDefault();
    const rd = new FileReader();
    rd.onload = () => {
      const src = rd.result as string;
      const dd = doc();
      if (!dd || !src) return;
      const img = dd.createElement("img");
      img.src = src;
      img.style.cssText = "display:block;border-radius:8px;object-fit:contain;";
      const probe = new Image();
      probe.onload = () => {
        const ratio = probe.naturalHeight / (probe.naturalWidth || 1);
        const w = 420;
        insertNode(img, w, Math.round(w * ratio) || 280);
      };
      probe.onerror = () => insertNode(img, 420, 280);
      probe.src = src;
    };
    rd.readAsDataURL(f);
    return;
  }
}

// ── 画形状工具（Figma 式 draw-to-create）: 按下拖出矩形 → 松手生成元素并选中 ──
function setTool(t: Tool) {
  tool.value = t;
  if (t !== "select") clearSel();
  syncToolCursor();
}
function syncToolCursor() {
  const d = doc();
  if (!d?.body) return;
  d.body.style.cursor =
    tool.value === "select" ? "" : tool.value === "hand" ? "grab" : "crosshair";
}
function startDraw(e: PointerEvent) {
  clearSel();
  draw = { x0: e.clientX, y0: e.clientY };
  (e.target as HTMLElement).setPointerCapture?.(e.pointerId);
  e.preventDefault();
}
function drawRectOf(e: PointerEvent) {
  const x = Math.min(draw!.x0, e.clientX);
  const y = Math.min(draw!.y0, e.clientY);
  return { x, y, w: Math.abs(e.clientX - draw!.x0), h: Math.abs(e.clientY - draw!.y0) };
}
function moveDraw(e: PointerEvent) {
  const d = doc();
  if (!d || !draw) return;
  const r = drawRectOf(e);
  const rb = d.getElementById("__rb") as HTMLElement | null;
  if (rb) {
    rb.style.display = r.w > 2 || r.h > 2 ? "block" : "none";
    rb.style.left = r.x + "px";
    rb.style.top = r.y + "px";
    rb.style.width = r.w + "px";
    rb.style.height = r.h + "px";
  }
}
function endDraw(e: PointerEvent) {
  const d = doc();
  const slide = activeSlide();
  const rb = d?.getElementById("__rb") as HTMLElement | null;
  if (rb) rb.style.display = "none";
  if (!d || !slide || !draw) { draw = null; return; }
  const r = drawRectOf(e);
  draw = null;
  const kind = tool.value;
  if (kind === "select" || kind === "hand") return;
  const sr = slide.getBoundingClientRect();
  if (getComputedStyle(slide).position === "static") slide.style.position = "relative";
  // 没拖出面积 = 单击: 用默认尺寸, 以点击点为左上角
  const clicked = r.w < 8 && r.h < 8;
  const defaults: Record<string, [number, number]> = {
    text: [320, 0], rect: [240, 160], ellipse: [160, 160], line: [320, 4],
  };
  const w = clicked ? defaults[kind][0] : Math.max(12, Math.round(r.w));
  const h = kind === "line" ? 4 : clicked ? defaults[kind][1] : Math.max(12, Math.round(r.h));
  const el = d.createElement("div");
  el.classList.add("__ins");
  if (kind === "text") {
    el.textContent = "输入文字";
    // 跟随页面文字颜色, 深浅底都可见
    const base = getComputedStyle(slide).color || "#1a1a1a";
    el.style.cssText = `font-size:32px;font-weight:600;line-height:1.3;color:${base};`;
  } else if (kind === "rect") {
    el.style.cssText = "background:#d9d9d9;";
  } else if (kind === "ellipse") {
    el.style.cssText = "background:#d9d9d9;border-radius:50%;";
  } else {
    el.style.cssText = "background:#d9d9d9;border-radius:2px;";
  }
  el.style.position = "absolute";
  el.style.boxSizing = "border-box";
  el.style.left = Math.round((clicked ? e.clientX : r.x) - sr.left) + "px";
  el.style.top = Math.round((clicked ? e.clientY : r.y) - sr.top) + "px";
  el.style.width = w + "px";
  if (kind !== "text" || (!clicked && r.h >= 24)) el.style.height = h + "px";
  el.style.zIndex = "40";
  slide.appendChild(el);
  setTool("select");
  selectEl(el);
  // 文本: 画完直接进入编辑态并全选占位文字, 打字即替换（Figma 手感）
  if (kind === "text") {
    el.setAttribute("contenteditable", "true");
    (el as HTMLElement).focus?.();
    const s = win()?.getSelection?.();
    if (s) {
      const range = d.createRange();
      range.selectNodeContents(el);
      s.removeAllRanges();
      s.addRange(range);
    }
  }
  artifacts.markDirty(true);
  rebuildThumbs();
  pushHistory();
}

// ── 抓手平移：画布灰底区域按下也能平移（iframe 内的在 onDocPointerDown 处理）──
function onCanvasPointerDown(e: PointerEvent) {
  if (!(panHeld.value || e.button === 1)) return;
  const c = canvasEl.value;
  if (!c) return;
  panDrag = { sx: e.screenX, sy: e.screenY, sl: c.scrollLeft, st: c.scrollTop };
  (e.target as HTMLElement).setPointerCapture?.(e.pointerId);
  e.preventDefault();
}
function onCanvasPointerMove(e: PointerEvent) {
  if (!panDrag) return;
  const c = canvasEl.value;
  if (!c) return;
  c.scrollLeft = panDrag.sl - (e.screenX - panDrag.sx);
  c.scrollTop = panDrag.st - (e.screenY - panDrag.sy);
}
function onCanvasPointerUp() { panDrag = null; }

// ── 图层树（Figma 左栏）──
const SKIP_TAGS = new Set(["SCRIPT", "STYLE", "LINK", "META", "TITLE", "BR", "TEMPLATE"]);
const VOID_TAGS = new Set(["img", "hr", "br", "input", "source", "svg", "video", "audio", "iframe", "canvas"]);
function isEditorNode(el: Element): boolean {
  return (el.id || "").startsWith("__");
}
/** 图层行的类型图标（Figma 式: 文本 T / 图片 / 列表 / 表格 / 容器框） */
function layerIcon(tag: string) {
  if (["img", "svg", "picture", "video", "canvas"].includes(tag)) return ImageIcon;
  if (["h1", "h2", "h3", "h4", "h5", "h6", "p", "span", "a", "li", "blockquote", "strong", "em", "label", "b", "i"].includes(tag)) return Type;
  if (tag === "ul" || tag === "ol") return List;
  if (tag === "table") return Table;
  return Frame;
}
function layerLabelFor(el: HTMLElement): string {
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
function buildLayers() {
  if (!designOn.value || mode.value !== "visual" || isTextDoc.value) { layerRows.value = []; return; }
  const root = activeSlide();
  if (!root) { layerRows.value = []; return; }
  const rows: LayerNode[] = [];
  const walk = (el: HTMLElement, depth: number, parentId: number | null) => {
    for (const c of Array.from(el.children) as HTMLElement[]) {
      if (SKIP_TAGS.has(c.tagName) || isEditorNode(c)) continue;
      let id = lids.get(c);
      if (!id) { id = lidSeq++; lids.set(c, id); }
      const kids = (Array.from(c.children) as HTMLElement[]).filter(
        (k) => !SKIP_TAGS.has(k.tagName) && !isEditorNode(k)
      ).length;
      rows.push({
        id, el: c, parentId,
        tag: c.tagName.toLowerCase(), label: layerLabelFor(c), depth, kids,
        hidden: c.style.display === "none", locked: c.hasAttribute("data-plk"),
      });
      if (depth < 7 && kids) walk(c, depth + 1, id);
    }
  };
  walk(root, 0, null);
  layerRows.value = rows;
}
/** 折叠过滤：祖先被折叠的行不显示 */
const visibleLayers = computed(() => {
  const hiddenUnder = new Set<number>();
  const out: LayerNode[] = [];
  for (const n of layerRows.value) {
    if (n.parentId != null && hiddenUnder.has(n.parentId)) { hiddenUnder.add(n.id); continue; }
    out.push(n);
    if (layerCollapsed.value.has(n.id)) hiddenUnder.add(n.id);
  }
  return out;
});
function layerHover(n: LayerNode | null) {
  const d = doc();
  if (!d) return;
  d.querySelectorAll(".__hov").forEach((x) => x.classList.remove("__hov"));
  if (n && !allSels.value.includes(n.el)) n.el.classList.add("__hov");
}
function layerClick(n: LayerNode, e: MouseEvent) {
  if (n.locked) return;
  selectEl(n.el, e.shiftKey);
  try { n.el.scrollIntoView({ block: "nearest", inline: "nearest" }); } catch { /* 部分实现不支持 */ }
  refreshSel();
}
function layerToggleCollapse(n: LayerNode) {
  const s = new Set(layerCollapsed.value);
  if (s.has(n.id)) s.delete(n.id);
  else s.add(n.id);
  layerCollapsed.value = s;
}
function layerToggleEye(n: LayerNode) {
  n.el.style.display = n.el.style.display === "none" ? "" : "none";
  if (n.el.style.display === "none" && allSels.value.includes(n.el)) clearSel();
  artifacts.markDirty(true);
  rebuildThumbs();
  refreshSel();
  pushHistory(); // 同步重建图层树
}
function layerToggleLock(n: LayerNode) {
  if (n.el.hasAttribute("data-plk")) n.el.removeAttribute("data-plk");
  else {
    n.el.setAttribute("data-plk", "1");
    if (allSels.value.includes(n.el)) clearSel();
  }
  artifacts.markDirty(true);
  pushHistory();
}
// 图层拖拽重排：上 1/3 = 插到前面，下 1/3 = 插到后面，中间 = 放进去（容器类）
function onLayerDragStart(n: LayerNode, e: DragEvent) {
  layerDragId.value = n.id;
  if (e.dataTransfer) e.dataTransfer.effectAllowed = "move";
}
function onLayerDragOver(n: LayerNode, e: DragEvent) {
  e.preventDefault();
  const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
  const y = e.clientY - r.top;
  layerDropId.value = n.id;
  const canInto = !VOID_TAGS.has(n.tag);
  layerDropPos.value =
    canInto && y > r.height * 0.3 && y < r.height * 0.7 ? "into" : y < r.height / 2 ? "before" : "after";
}
function onLayerDrop() {
  const fromId = layerDragId.value, toId = layerDropId.value, pos = layerDropPos.value;
  layerDragId.value = null;
  layerDropId.value = null;
  layerDropPos.value = null;
  if (fromId == null || toId == null || fromId === toId || !pos) return;
  const from = layerRows.value.find((r) => r.id === fromId);
  const to = layerRows.value.find((r) => r.id === toId);
  if (!from || !to || from.el.contains(to.el)) return; // 不能拖进自己的后代
  if (pos === "into") to.el.appendChild(from.el);
  else if (pos === "before") to.el.before(from.el);
  else to.el.after(from.el);
  artifacts.markDirty(true);
  rebuildThumbs();
  refreshSel();
  pushHistory(); // 同步重建图层树
}
function onLayerDragEnd() {
  layerDragId.value = null;
  layerDropId.value = null;
  layerDropPos.value = null;
}

// ───────── 富文本工具栏（豆包式，作用于画布里 contenteditable 的当前选区）─────────
// 网页模式整页可编辑，选中即可用；deck 模式双击文本进入文字编辑后可用。
function exec(cmd: string, val?: string) {
  const d = doc();
  if (!d) return;
  try { d.execCommand("styleWithCSS", false, "true"); } catch { /* 老命令，个别实现会抛 */ }
  d.execCommand(cmd, false, val);
  artifacts.markDirty(true);
  refreshSel();
  pushHistory(500);
}
function execBlock(e: Event) {
  const sel = e.target as HTMLSelectElement;
  if (sel.value) exec("formatBlock", `<${sel.value}>`);
  sel.value = ""; // 复位成占位项，下次选同一项也能触发 change
}
function execFontSize(e: Event) {
  const sel = e.target as HTMLSelectElement;
  const px = parseInt(sel.value);
  sel.value = "";
  const d = doc();
  if (!d || !px) return;
  // execCommand 的 fontSize 只有 1–7 粗档：先打上 7 号标记，再把标记换成精确 px
  try { d.execCommand("styleWithCSS", false, "true"); } catch { /* 同上 */ }
  d.execCommand("fontSize", false, "7");
  d.querySelectorAll('font[size="7"]').forEach((f) => {
    const s = d.createElement("span");
    s.style.fontSize = px + "px";
    while (f.firstChild) s.appendChild(f.firstChild);
    f.replaceWith(s);
  });
  d.querySelectorAll<HTMLElement>('span[style*="font-size"]').forEach((sp) => {
    if (sp.style.fontSize === "xxx-large") sp.style.fontSize = px + "px";
  });
  artifacts.markDirty(true);
  pushHistory(500);
}
const FONTS = [
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
function execLink() {
  const url = window.prompt("链接地址", "https://");
  if (url && url !== "https://") exec("createLink", url);
}
// 点工具条按钮不能抢走 iframe 里的文字选区焦点（否则命令落到空选区上）。
// select 下拉除外——它需要 mousedown 默认行为才能弹开。
function onFmtBarDown(e: MouseEvent) {
  const t = e.target as HTMLElement;
  if (!t.closest("select")) e.preventDefault();
}
function execColor(e: Event) { exec("foreColor", (e.target as HTMLInputElement).value); }
function execHilite(e: Event) { exec("hiliteColor", (e.target as HTMLInputElement).value); }

// ───────── 浮动气泡工具栏定位 ─────────
// 选区矩形（iframe 内 1280×720 坐标）→ 乘缩放映射到画布容器坐标，浮在选区上方。
function updateBubble() {
  if (mode.value !== "visual" || isTextDoc.value) { bubble.value = null; return; }
  const d = doc();
  const w = win();
  const stage = stageEl.value;
  const canvas = canvasEl.value;
  const s = w?.getSelection?.();
  if (!d || !s || !stage || !canvas || s.isCollapsed || !s.rangeCount) { bubble.value = null; return; }
  const ae = d.activeElement as HTMLElement | null;
  if (!ae || !(ae as any).isContentEditable) { bubble.value = null; return; }
  const r = s.getRangeAt(0).getBoundingClientRect();
  if (!r || (!r.width && !r.height)) { bubble.value = null; return; }
  const sr = stage.getBoundingClientRect();
  const cr = canvas.getBoundingClientRect();
  const k = scale.value;
  let x = sr.left - cr.left + canvas.scrollLeft + (r.left + r.width / 2) * k;
  let y = sr.top - cr.top + canvas.scrollTop + r.top * k;
  // 别飘出画布：左右留半个气泡宽，太靠顶就压到选区下方
  x = Math.min(Math.max(x, 230), Math.max(230, canvas.clientWidth + canvas.scrollLeft - 230));
  if (y - canvas.scrollTop < 56) y = sr.top - cr.top + canvas.scrollTop + (r.bottom + 12) * k + 44;
  bubble.value = { x, y };
}

// Ctrl+滚轮缩放画布 —— 以光标为锚点（Figma 手感）: 缩放前后光标下的点保持不动
function zoomAt(px: number, py: number, factor: number) {
  const c = canvasEl.value;
  if (!c) return;
  const old = scale.value;
  zoom.value = Math.min(4, Math.max(0.1, +(zoom.value * factor).toFixed(4)));
  nextTick(() => {
    const k = scale.value / old;
    if (k === 1) return;
    c.scrollLeft = (c.scrollLeft + px) * k - px;
    c.scrollTop = (c.scrollTop + py) * k - py;
  });
}
function onCanvasWheel(e: WheelEvent) {
  if (!e.ctrlKey) return;
  e.preventDefault();
  const c = canvasEl.value;
  if (!c) return;
  const r = c.getBoundingClientRect();
  zoomAt(e.clientX - r.left, e.clientY - r.top, e.deltaY < 0 ? 1.12 : 1 / 1.12);
}

// 右侧面板：整个选中元素换字体（PPT 习惯）
function fmtFontFamily(e: Event) {
  const el = selEl.value;
  const sel = e.target as HTMLSelectElement;
  const v = sel.value;
  sel.value = "";
  if (!el || !v) return;
  el.style.fontFamily = v;
  afterFmt();
}

// ───────── Markdown 工具栏（作用于左侧 textarea 的选区/光标）─────────
function mdWrap(before: string, after = before, placeholder = "文字") {
  const ta = docTa.value;
  if (!ta) return;
  const s = ta.selectionStart, e2 = ta.selectionEnd, v = ta.value;
  const sel = v.slice(s, e2) || placeholder;
  html.value = v.slice(0, s) + before + sel + after + v.slice(e2);
  artifacts.markDirty(true);
  nextTick(() => {
    ta.focus();
    ta.setSelectionRange(s + before.length, s + before.length + sel.length);
  });
}
/** 行首前缀开关（标题/引用/列表）：选中多行则逐行处理 */
function mdPrefix(prefix: string) {
  const ta = docTa.value;
  if (!ta) return;
  const s = ta.selectionStart, e2 = ta.selectionEnd, v = ta.value;
  const ls = v.lastIndexOf("\n", s - 1) + 1; // 扩到行首
  const seg = v.slice(ls, e2);
  const done = seg
    .split("\n")
    .map((l) => (l.startsWith(prefix) ? l.slice(prefix.length) : prefix + l))
    .join("\n");
  html.value = v.slice(0, ls) + done + v.slice(e2);
  artifacts.markDirty(true);
  nextTick(() => {
    ta.focus();
    ta.setSelectionRange(ls, ls + done.length);
  });
}
function mdInsertBlock(text: string) {
  const ta = docTa.value;
  if (!ta) return;
  const s = ta.selectionStart, v = ta.value;
  html.value = v.slice(0, s) + text + v.slice(s);
  artifacts.markDirty(true);
  nextTick(() => {
    ta.focus();
    ta.setSelectionRange(s + text.length, s + text.length);
  });
}
function mdInsertTable() {
  mdInsertBlock("\n| 列 1 | 列 2 | 列 3 |\n| --- | --- | --- |\n| 内容 | 内容 | 内容 |\n");
}
/** Tab 缩进两空格（否则 Tab 会把焦点带走） */
function onDocTaKey(e: KeyboardEvent) {
  if (e.key !== "Tab") return;
  e.preventDefault();
  const ta = e.target as HTMLTextAreaElement;
  const s = ta.selectionStart, e2 = ta.selectionEnd;
  html.value = ta.value.slice(0, s) + "  " + ta.value.slice(e2);
  artifacts.markDirty(true);
  nextTick(() => ta.setSelectionRange(s + 2, s + 2));
}
/** 左编辑右预览按比例同步滚动 */
function onDocTaScroll() {
  const ta = docTa.value, pv = mdPrevEl.value;
  if (!ta || !pv) return;
  const ratio = ta.scrollTop / Math.max(1, ta.scrollHeight - ta.clientHeight);
  pv.scrollTop = ratio * (pv.scrollHeight - pv.clientHeight);
}

// ───────── 查找替换（源码 / Markdown / 纯文本）─────────
function activeTa(): HTMLTextAreaElement | null {
  return isTextDoc.value ? docTa.value : codeTa.value;
}
function openFind() {
  findOpen.value = true;
  nextTick(() => findInput.value?.focus());
}
function findNext() {
  const ta = activeTa();
  if (!ta || !findQ.value) return;
  const v = ta.value;
  let i = v.indexOf(findQ.value, ta.selectionEnd || 0);
  if (i < 0) i = v.indexOf(findQ.value); // 到底回绕
  if (i < 0) return;
  ta.focus();
  ta.setSelectionRange(i, i + findQ.value.length);
}
function replaceOne() {
  const ta = activeTa();
  if (!ta || !findQ.value) return;
  const s = ta.selectionStart, e2 = ta.selectionEnd;
  if (ta.value.slice(s, e2) === findQ.value) {
    html.value = ta.value.slice(0, s) + replQ.value + ta.value.slice(e2);
    artifacts.markDirty(true);
    nextTick(() => {
      ta.setSelectionRange(s + replQ.value.length, s + replQ.value.length);
      findNext();
    });
  } else {
    findNext(); // 先定位到第一处，再点一次替换
  }
}
function replaceAll() {
  if (!findQ.value || !findCount.value) return;
  html.value = html.value.split(findQ.value).join(replQ.value);
  artifacts.markDirty(true);
}

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
// ── 缩略图拖拽换页序 ──
const thumbDragIdx = ref<number | null>(null);
const thumbDragOver = ref<number | null>(null);
function onThumbDragStart(i: number, e: DragEvent) {
  thumbDragIdx.value = i;
  if (e.dataTransfer) e.dataTransfer.effectAllowed = "move";
}
function onThumbDragOver(i: number, e: DragEvent) {
  e.preventDefault();
  thumbDragOver.value = i;
}
function onThumbDrop(i: number) {
  const from = thumbDragIdx.value;
  thumbDragIdx.value = null;
  thumbDragOver.value = null;
  if (from == null || from === i) return;
  const d = doc();
  if (!d) return;
  const secs = Array.from(d.querySelectorAll<HTMLElement>(".slide"));
  const a = secs[from], b = secs[i];
  if (!a || !b) return;
  if (from < i) b.after(a);
  else b.before(a);
  reloadFrom(serialize(), i); // onFrameLoad 会落快照，可撤销
}
function onThumbDragEnd() {
  thumbDragIdx.value = null;
  thumbDragOver.value = null;
}

// ───────── 序列化（去掉编辑器注入物）─────────
function serialize(): string {
  const d = doc();
  if (!d) return html.value;
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
  // 文档类（md/文本）没有画布，永远保存源文本
  const out = !isTextDoc.value && mode.value === "visual" ? serialize() : html.value;
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
  const pad = 100; // 画布 padding 44×2 + 呼吸空隙, 适应缩放时不出滚动条
  const fw = (el.clientWidth - pad) / 1280;
  const fh = (el.clientHeight - pad) / 720;
  fitScale.value = Math.max(0.15, Math.min(fw, fh));
}
function zoomIn() { zoom.value = Math.min(4, +(zoom.value * 1.25).toFixed(3)); }
function zoomOut() { zoom.value = Math.max(0.1, +(zoom.value / 1.25).toFixed(3)); }
function zoomFit() { zoom.value = 1; computeFit(); }

let ro: ResizeObserver | null = null;
function onKey(e: KeyboardEvent) {
  // 空格按住 = 抓手平移（焦点在父页时也生效；输入框里正常打空格）
  if (e.code === "Space" && mode.value === "visual" && !isTextDoc.value) {
    const t = document.activeElement as HTMLElement | null;
    if (!t || (t.tagName !== "TEXTAREA" && t.tagName !== "INPUT" && t.tagName !== "SELECT")) {
      panHeld.value = true;
      e.preventDefault();
    }
  }
  const mod = e.ctrlKey || e.metaKey;
  if (!mod) return;
  const k = e.key.toLowerCase();
  if (k === "s") {
    e.preventDefault();
    save();
    return;
  }
  if (k === "f" && (isTextDoc.value || mode.value === "code")) {
    e.preventDefault();
    openFind();
    return;
  }
  // 可视化模式下焦点落在父页（如刚点完工具栏）时也接住撤销/重做
  if (mode.value === "visual" && !isTextDoc.value) {
    const t = document.activeElement as HTMLElement | null;
    const typing = !!t && (t.tagName === "TEXTAREA" || t.tagName === "INPUT");
    if (typing) return;
    if (k === "z") { e.preventDefault(); if (e.shiftKey) redo(); else undo(); }
    else if (k === "y") { e.preventDefault(); redo(); }
  }
}
function onWinKeyUp(e: KeyboardEvent) {
  if (e.code === "Space") panHeld.value = false;
  if (e.key === "Alt") clearMeasure();
}
onMounted(() => {
  window.addEventListener("keydown", onKey);
  window.addEventListener("keyup", onWinKeyUp);
  window.addEventListener("paste", onPasteClipboard);
  window.addEventListener("pointerdown", closeCtx);
  ro = new ResizeObserver(() => computeFit());
  if (canvasEl.value) ro.observe(canvasEl.value);
  hintTimer = window.setTimeout(() => (hintOn.value = false), 9000);
});
onBeforeUnmount(() => {
  window.removeEventListener("keydown", onKey);
  window.removeEventListener("keyup", onWinKeyUp);
  window.removeEventListener("paste", onPasteClipboard);
  window.removeEventListener("pointerdown", closeCtx);
  ro?.disconnect();
  clearTimeout(hintTimer);
  const d = doc();
  if (d) {
    if (inputHandler) d.removeEventListener("input", inputHandler, true);
    if (keyGuard) d.removeEventListener("keydown", keyGuard, true);
    if (clickGuard) d.removeEventListener("click", clickGuard, true);
    if (selChangeHandler) d.removeEventListener("selectionchange", selChangeHandler);
    if (scrollHandler) d.removeEventListener("scroll", scrollHandler, true);
    if (keyUpGuard) d.removeEventListener("keyup", keyUpGuard, true);
    if (wheelGuard) d.removeEventListener("wheel", wheelGuard, true);
    if (ctxGuard) d.removeEventListener("contextmenu", ctxGuard, true);
    if (pasteGuard) d.removeEventListener("paste", pasteGuard, true);
  }
});

// 缩放变化时: 气泡重定位 + 选中框/参考线线宽反向补偿(--edscale)与尺寸徽标刷新
watch(scale, () => {
  if (bubble.value) updateBubble();
  refreshSel();
});

// 若外部重新打开了别的文件，刷新工作副本 + 清空历史/查找状态
watch(
  () => artifacts.payload?.path,
  () => {
    html.value = artifacts.payload?.text ?? "";
    frameSrc.value = html.value;
    mode.value = "visual";
    history.value = [];
    hIndex.value = -1;
    findOpen.value = false;
    elClipboard.value = null;
    imgReplaceTarget = null;
    ctx.value = null;
    tool.value = "select";
    extraSels.value = [];
    layerRows.value = [];
    layerCollapsed.value = new Set();
    railTab.value = "pages";
    pageDesign.value = true;
    panHeld.value = false;
  }
);
</script>

<template>
  <div class="ed">
    <!-- 顶部工具栏 -->
    <div class="ed-bar">
      <div v-if="!isTextDoc" class="ed-seg">
        <button :class="{ on: mode === 'visual' }" title="可视化编辑" @click="toVisual"><Eye :size="13" /> 可视化</button>
        <button :class="{ on: mode === 'code' }" title="源码编辑" @click="toCode"><Code2 :size="13" /> 源码</button>
      </div>
      <!-- 普通网页：设计（Figma 式点选拖拽）/ 文字（整页直接改字）双模式 -->
      <div v-if="!isTextDoc && mode === 'visual' && !isDeck" class="ed-seg">
        <button :class="{ on: pageDesign }" title="设计模式：像 Figma 一样点选 / 拖拽 / 框选元素" @click="setPageDesign(true)"><MousePointer2 :size="13" /> 设计</button>
        <button :class="{ on: !pageDesign }" title="文字模式：整页直接改字" @click="setPageDesign(false)"><Type :size="13" /> 文字</button>
      </div>
      <div v-else class="ed-doc-tag">
        <FileText :size="13" />
        {{ docKind === "markdown" ? "Markdown 文档 · 左改右看" : "纯文本编辑" }}
        <span class="ed-doc-count">{{ html.length.toLocaleString() }} 字符</span>
      </div>

      <template v-if="!isTextDoc && mode === 'visual'">
        <button class="ed-ic" title="撤销 (Ctrl+Z)：改字/拖动/删除/换主题都能撤" :disabled="!canUndo" @click="undo()"><Undo2 :size="14" /></button>
        <button class="ed-ic" title="重做 (Ctrl+Y)" :disabled="!canRedo" @click="redo()"><Redo2 :size="14" /></button>
      </template>

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


      <div v-if="!isTextDoc && mode === 'visual'" class="ed-zoom">
        <button class="ed-ic" title="缩小" @click="zoomOut"><ZoomOut :size="14" /></button>
        <button class="ed-pct" title="适应窗口" @click="zoomFit">{{ Math.round(scale * 100) }}%</button>
        <button class="ed-ic" title="放大" @click="zoomIn"><ZoomIn :size="14" /></button>
      </div>

      <!-- Figma 往返: 送去 Figma 编辑 / 改完拉回来 -->
      <button v-if="!isTextDoc" class="ed-pct ed-figma" title="接入 Figma 网页端：复制页面导入 Figma 自由编辑，改完一键拉回替换" @click="figmaOpen = true">
        Figma
      </button>

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

    <!-- Markdown / 纯文本工具条：作用于左侧编辑区选区 -->
    <div v-if="isTextDoc" class="ed-fmt">
      <template v-if="docKind === 'markdown'">
        <button class="ed-ic" title="加粗" @click="mdWrap('**')"><Bold :size="14" /></button>
        <button class="ed-ic" title="斜体" @click="mdWrap('*')"><Italic :size="14" /></button>
        <button class="ed-ic" title="删除线" @click="mdWrap('~~')"><Strikethrough :size="14" /></button>
        <button class="ed-ic" title="行内代码" @click="mdWrap('`', '`', '代码')"><Code2 :size="14" /></button>
        <span class="ed-fmt-sep" />
        <button class="ed-ic" title="一级标题" @click="mdPrefix('# ')"><Heading1 :size="14" /></button>
        <button class="ed-ic" title="二级标题" @click="mdPrefix('## ')"><Heading2 :size="14" /></button>
        <button class="ed-ic" title="三级标题" @click="mdPrefix('### ')"><Heading3 :size="14" /></button>
        <button class="ed-ic" title="引用" @click="mdPrefix('> ')"><TextQuote :size="14" /></button>
        <button class="ed-ic" title="无序列表" @click="mdPrefix('- ')"><List :size="14" /></button>
        <button class="ed-ic" title="有序列表" @click="mdPrefix('1. ')"><ListOrdered :size="14" /></button>
        <span class="ed-fmt-sep" />
        <button class="ed-ic" title="插入链接" @click="mdWrap('[', '](https://)', '链接文字')"><Link2 :size="14" /></button>
        <button class="ed-ic" title="插入表格" @click="mdInsertTable"><Table :size="14" /></button>
        <span class="ed-fmt-sep" />
      </template>
      <button class="ed-ic" title="查找替换 (Ctrl+F)" @click="openFind"><Search :size="14" /></button>
      <span class="ed-fmt-tip">{{ docKind === "markdown" ? "选中文字后点按钮加格式 · 左右同步滚动" : "Ctrl+F 查找替换 · Ctrl+S 保存" }}</span>
    </div>

    <!-- 查找替换条（源码 / Markdown / 纯文本） -->
    <div v-if="findOpen && (isTextDoc || mode === 'code')" class="ed-find">
      <Search :size="13" class="ed-find-ic" />
      <input
        ref="findInput"
        v-model="findQ"
        class="ed-find-in"
        placeholder="查找…"
        @keydown.enter.prevent="findNext"
        @keydown.esc="findOpen = false"
      />
      <span class="ed-find-n">{{ findCount }} 处</span>
      <input
        v-model="replQ"
        class="ed-find-in"
        placeholder="替换为…"
        @keydown.enter.prevent="replaceOne"
        @keydown.esc="findOpen = false"
      />
      <button class="ed-find-btn" :disabled="!findCount" @click="findNext">下一个</button>
      <button class="ed-find-btn" :disabled="!findCount" @click="replaceOne">替换</button>
      <button class="ed-find-btn" :disabled="!findCount" @click="replaceAll">全部替换</button>
      <button class="ed-ic" title="关闭" @click="findOpen = false"><X :size="13" /></button>
    </div>

    <input ref="fileInput" type="file" accept="image/*" style="display:none" @change="onImagePicked" />

    <!-- 主体 -->
    <div class="ed-body">
      <!-- 左栏：deck = 页面缩略 / 图层树 双页签；网页设计模式 = 图层树（Figma 式） -->
      <aside v-if="!isTextDoc && mode === 'visual' && (isDeck || pageDesign)" class="ed-rail">
        <div class="ed-rail-tabs">
          <button v-if="isDeck" :class="{ on: railTab === 'pages' }" @click="railTab = 'pages'"><LayoutList :size="12" /> 页面</button>
          <button v-if="isDeck" :class="{ on: railTab === 'layers' }" @click="railTab = 'layers'"><Layers :size="12" /> 图层</button>
          <span v-if="!isDeck" class="ed-rail-title"><Layers :size="12" /> 图层</span>
        </div>

        <template v-if="isDeck && railTab === 'pages'">
          <button
            v-for="(s, i) in slides"
            :key="i"
            class="ed-thumb"
            :class="{ on: i === cur, 'drag-over': thumbDragOver === i && thumbDragIdx !== i }"
            :title="s.title + '（可拖拽调整页序）'"
            draggable="true"
            @click="goSlide(i)"
            @dragstart="onThumbDragStart(i, $event)"
            @dragover="onThumbDragOver(i, $event)"
            @drop="onThumbDrop(i)"
            @dragend="onThumbDragEnd"
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
        </template>

        <div v-else class="ed-layers" @mouseleave="layerHover(null)">
          <div
            v-for="n in visibleLayers"
            :key="n.id"
            class="ed-layer"
            :class="{
              on: allSels.includes(n.el),
              hidden: n.hidden,
              'drop-before': layerDropId === n.id && layerDropPos === 'before',
              'drop-after': layerDropId === n.id && layerDropPos === 'after',
              'drop-into': layerDropId === n.id && layerDropPos === 'into',
            }"
            :style="{ paddingLeft: 6 + n.depth * 13 + 'px' }"
            draggable="true"
            @click="layerClick(n, $event)"
            @mouseenter="layerHover(n)"
            @dragstart="onLayerDragStart(n, $event)"
            @dragover="onLayerDragOver(n, $event)"
            @drop="onLayerDrop()"
            @dragend="onLayerDragEnd"
          >
            <button v-if="n.kids" class="ed-layer-caret" :title="layerCollapsed.has(n.id) ? '展开' : '折叠'" @click.stop="layerToggleCollapse(n)">
              <ChevronRight v-if="layerCollapsed.has(n.id)" :size="11" /><ChevronDown v-else :size="11" />
            </button>
            <span v-else class="ed-layer-caret ph" />
            <component :is="layerIcon(n.tag)" :size="11" class="ed-layer-ico" />
            <span class="ed-layer-name" :title="n.tag + ' · ' + n.label">{{ n.label }}</span>
            <button class="ed-layer-act" :class="{ act: n.locked }" :title="n.locked ? '解锁' : '锁定（画布上不可选中）'" @click.stop="layerToggleLock(n)">
              <Lock v-if="n.locked" :size="11" /><LockOpen v-else :size="11" />
            </button>
            <button class="ed-layer-act" :class="{ act: n.hidden }" :title="n.hidden ? '显示' : '隐藏'" @click.stop="layerToggleEye(n)">
              <EyeOff v-if="n.hidden" :size="11" /><Eye v-else :size="11" />
            </button>
          </div>
          <div v-if="!visibleLayers.length" class="ed-layers-empty">当前页没有可编辑元素</div>
        </div>
      </aside>

      <!-- 文档编辑（Markdown / 纯文本）：左源码右实时预览（纯文本只有左栏） -->
      <template v-if="isTextDoc">
        <div class="ed-split">
          <textarea
            ref="docTa"
            v-model="html"
            class="ed-code-area doc"
            spellcheck="false"
            :placeholder="docKind === 'markdown' ? '# 在这里写 Markdown，右侧实时预览' : ''"
            @input="artifacts.markDirty(true)"
            @keydown="onDocTaKey"
            @scroll="onDocTaScroll"
          />
          <div
            v-if="docKind === 'markdown'"
            ref="mdPrevEl"
            class="ed-md-preview markdown"
            v-html="mdPreview"
          />
        </div>
      </template>

      <!-- 画布区: 滚动画布 + 悬浮工具药丸/提示条（不随内容滚动） -->
      <div v-if="!isTextDoc" v-show="mode === 'visual'" ref="zoneEl" class="ed-canvas-zone">
      <div
        ref="canvasEl"
        class="ed-canvas"
        :class="{ pan: panHeld || tool === 'hand', draw: designOn && tool !== 'select' && tool !== 'hand' }"
        @scroll.passive="updateBubble"
        @wheel="onCanvasWheel"
        @pointerdown="onCanvasPointerDown"
        @pointermove="onCanvasPointerMove"
        @pointerup="onCanvasPointerUp"
      >
        <!-- 壳按缩放后尺寸占位, 里面的舞台从左上角等比缩放 → 放大后可滚到每个角落 -->
        <div class="ed-stage-wrap" :style="stageWrapStyle">
          <div ref="stageEl" class="ed-stage" :style="stageStyle">
            <iframe
              ref="frame"
              class="ed-frame"
              :srcdoc="frameSrc"
              sandbox="allow-scripts allow-same-origin"
              @load="onFrameLoad"
            />
          </div>
        </div>
        <!-- 浮动气泡工具栏（现代编辑器式）：选中画布文字时浮现在选区上方 -->
        <div
          v-if="bubble"
          class="ed-bubble"
          :style="{ left: bubble.x + 'px', top: bubble.y + 'px' }"
          @mousedown="onFmtBarDown"
        >
          <select class="ed-fmt-sel" title="段落格式" @change="execBlock">
            <option value="" disabled selected>正文</option>
            <option value="p">正文</option>
            <option value="h1">标题 1</option>
            <option value="h2">标题 2</option>
            <option value="h3">标题 3</option>
            <option value="blockquote">引用</option>
          </select>
          <select class="ed-fmt-sel" title="字号" @change="execFontSize">
            <option value="" disabled selected>字号</option>
            <option v-for="s in [12, 14, 16, 18, 20, 24, 28, 32, 40, 48, 64]" :key="s" :value="s">{{ s }}</option>
          </select>
          <span class="ed-fmt-sep" />
          <button class="ed-ic" title="加粗" @click="exec('bold')"><Bold :size="14" /></button>
          <button class="ed-ic" title="斜体" @click="exec('italic')"><Italic :size="14" /></button>
          <button class="ed-ic" title="下划线" @click="exec('underline')"><Underline :size="14" /></button>
          <button class="ed-ic" title="删除线" @click="exec('strikeThrough')"><Strikethrough :size="14" /></button>
          <label class="ed-fmt-color" title="文字颜色">
            <Type :size="13" />
            <input type="color" @input="execColor" />
          </label>
          <label class="ed-fmt-color" title="高亮标记">
            <Highlighter :size="13" />
            <input type="color" value="#fff59d" @input="execHilite" />
          </label>
          <span class="ed-fmt-sep" />
          <button class="ed-ic" title="左对齐" @click="exec('justifyLeft')"><AlignLeft :size="14" /></button>
          <button class="ed-ic" title="居中" @click="exec('justifyCenter')"><AlignCenter :size="14" /></button>
          <button class="ed-ic" title="右对齐" @click="exec('justifyRight')"><AlignRight :size="14" /></button>
          <span class="ed-fmt-sep" />
          <button class="ed-ic" title="无序列表" @click="exec('insertUnorderedList')"><List :size="14" /></button>
          <button class="ed-ic" title="有序列表" @click="exec('insertOrderedList')"><ListOrdered :size="14" /></button>
          <button class="ed-ic" title="插入链接" @click="execLink"><Link2 :size="14" /></button>
          <button class="ed-ic" title="清除格式" @click="exec('removeFormat')"><RemoveFormatting :size="14" /></button>
        </div>
      </div>

      <!-- 底部悬浮工具药丸（Figma UI3 式）: 选择/抓手 + 画文本/矩形/圆/线 + 图片 -->
      <div v-if="designOn" class="ed-tools">
        <button :class="{ on: tool === 'select' }" title="选择 (V)" @click="setTool('select')"><MousePointer2 :size="16" /></button>
        <button :class="{ on: tool === 'hand' }" title="抓手平移 (H · 按住空格也行)" @click="setTool('hand')"><Hand :size="16" /></button>
        <span class="ed-tools-sep" />
        <button :class="{ on: tool === 'text' }" title="文本 (T) · 在画布上点击或拖拽放置" @click="setTool('text')"><Type :size="16" /></button>
        <button :class="{ on: tool === 'rect' }" title="矩形 (R) · 拖拽画出" @click="setTool('rect')"><Square :size="16" /></button>
        <button :class="{ on: tool === 'ellipse' }" title="圆 (O) · 拖拽画出" @click="setTool('ellipse')"><Circle :size="16" /></button>
        <button :class="{ on: tool === 'line' }" title="线条 (L) · 拖拽画出" @click="setTool('line')"><Minus :size="16" /></button>
        <span class="ed-tools-sep" />
        <button title="插入图片" @click="pickImage"><ImageIcon :size="16" /></button>
      </div>

      <div class="ed-hint" :class="{ off: !hintOn }">
        <Maximize :size="12" />
        {{ designOn ? "单击选中 · Shift/框选多选 · Alt+拖=副本 · Alt+悬停=测距 · 右键菜单 · Ctrl+G 编组 · Ctrl+Z 撤销" : "整页可直接改字 · 选中文字用上方工具栏改样式" }} · Ctrl+S 保存
      </div>

      <!-- 右键上下文菜单（Figma 式） -->
      <div v-if="ctx" class="ed-ctx" :style="{ left: ctx.x + 'px', top: ctx.y + 'px' }" @pointerdown.stop>
        <template v-if="selEl">
          <button @click="ctxDo(copyEl)"><span>复制</span><kbd>Ctrl C</kbd></button>
          <button :disabled="!elClipboard?.length" @click="ctxDo(pasteEl)"><span>粘贴</span><kbd>Ctrl V</kbd></button>
          <button @click="ctxDo(duplicateEl)"><span>创建副本</span><kbd>Ctrl D</kbd></button>
          <button @click="ctxDo(cutEl)"><span>剪切</span><kbd>Ctrl X</kbd></button>
          <div class="sep" />
          <button @click="ctxDo(() => zStep(true))"><span>上移一层</span><kbd>]</kbd></button>
          <button @click="ctxDo(() => zStep(false))"><span>下移一层</span><kbd>[</kbd></button>
          <div v-if="allSels.length > 1 || canUngroup" class="sep" />
          <button v-if="allSels.length > 1" @click="ctxDo(groupSel)"><span>编组</span><kbd>Ctrl G</kbd></button>
          <button v-if="canUngroup" @click="ctxDo(ungroupSel)"><span>取消编组</span><kbd>Ctrl⇧G</kbd></button>
          <div class="sep" />
          <button @click="ctxDo(lockSel)"><span>锁定</span></button>
          <button @click="ctxDo(hideSel)"><span>隐藏</span></button>
          <div class="sep" />
          <button class="danger" @click="ctxDo(fmtDelete)"><span>删除</span><kbd>Del</kbd></button>
        </template>
        <template v-else>
          <button :disabled="!elClipboard?.length" @click="ctxDo(pasteEl)"><span>粘贴</span><kbd>Ctrl V</kbd></button>
          <button @click="ctxDo(selectAllEls)"><span>全选</span><kbd>Ctrl A</kbd></button>
        </template>
      </div>
      </div>

      <!-- 源码 -->
      <div v-if="!isTextDoc" v-show="mode === 'code'" class="ed-code">
        <textarea
          ref="codeTa"
          v-model="html"
          class="ed-code-area"
          spellcheck="false"
          @input="artifacts.markDirty(true)"
        />
      </div>

      <!-- 右侧属性面板（Figma 式检查器：deck 和网页设计模式都有） -->
      <aside v-if="!isTextDoc && mode === 'visual' && designOn" class="ed-panel">
        <!-- 多选：相互对齐 / 等间距分布 / 批量删除 -->
        <template v-if="extraSels.length">
          <div class="ep-head"><span>已选 {{ allSels.length }} 个元素</span><button class="ep-x" title="取消选中" @click="clearSel"><X :size="14" /></button></div>
          <div class="ep-sec">
            <div class="ep-label">相互对齐</div>
            <div class="ep-btns">
              <button title="左对齐" @click="alignSel('left')"><AlignStartVertical :size="15" /></button>
              <button title="水平居中" @click="alignSel('hcenter')"><AlignCenterVertical :size="15" /></button>
              <button title="右对齐" @click="alignSel('right')"><AlignEndVertical :size="15" /></button>
              <button title="顶对齐" @click="alignSel('top')"><AlignStartHorizontal :size="15" /></button>
              <button title="垂直居中" @click="alignSel('vcenter')"><AlignCenterHorizontal :size="15" /></button>
              <button title="底对齐" @click="alignSel('bottom')"><AlignEndHorizontal :size="15" /></button>
            </div>
          </div>
          <div class="ep-sec">
            <div class="ep-label">等间距分布</div>
            <div class="ep-btns">
              <button title="水平等距分布（需 ≥3 个）" :disabled="allSels.length < 3" @click="distributeSel('h')"><AlignHorizontalDistributeCenter :size="15" /></button>
              <button title="垂直等距分布（需 ≥3 个）" :disabled="allSels.length < 3" @click="distributeSel('v')"><AlignVerticalDistributeCenter :size="15" /></button>
            </div>
          </div>
          <div class="ep-acts">
            <button title="编组 (Ctrl+G)：包成一个整体一起拖" @click="groupSel"><Group :size="14" /> 编组</button>
            <button class="danger" @click="fmtDelete"><Trash2 :size="14" /> 删除全部</button>
          </div>
        </template>

        <template v-else-if="selEl">
          <div class="ep-head"><span>元素格式</span><button class="ep-x" title="取消选中" @click="clearSel"><X :size="14" /></button></div>

          <!-- Figma 式: 顶部一排对齐图标(对齐到页面) -->
          <div class="ep-sec ep-align-row">
            <div class="ep-btns wide">
              <button title="左对齐到页面" @click="alignToPage('left')"><AlignStartVertical :size="15" /></button>
              <button title="水平居中" @click="alignToPage('hcenter')"><AlignCenterVertical :size="15" /></button>
              <button title="右对齐到页面" @click="alignToPage('right')"><AlignEndVertical :size="15" /></button>
              <button title="顶对齐到页面" @click="alignToPage('top')"><AlignStartHorizontal :size="15" /></button>
              <button title="垂直居中" @click="alignToPage('vcenter')"><AlignCenterHorizontal :size="15" /></button>
              <button title="底对齐到页面" @click="alignToPage('bottom')"><AlignEndHorizontal :size="15" /></button>
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
            <div class="ep-label">外观</div>
            <div class="ep-grid">
              <label class="ep-field" title="不透明度 %"><span>透明</span><input type="number" min="0" max="100" :value="selEffects.opacity" @change="fmtOpacity(+($event.target as HTMLInputElement).value)" /></label>
              <label class="ep-field" title="圆角 px"><span>圆角</span><input type="number" :value="selFill.radius" @change="fmtRadius(+($event.target as HTMLInputElement).value)" /></label>
            </div>
          </div>

          <div class="ep-sec">
            <div class="ep-label">填充</div>
            <label class="ep-color">
              <span>{{ selFill.hasBg ? "背景色" : "无填充" }}</span>
              <span class="ep-fill-end">
                <input class="ep-hex" :value="selFill.hasBg ? selFill.bg : ''" placeholder="#RRGGBB" spellcheck="false" @change="fmtFillHex" @click.prevent />
                <span class="ep-color-sw" :style="{ background: selFill.hasBg ? selFill.bg : 'transparent' }"><input type="color" :value="selFill.bg" @input="fmtFill" /></span>
                <button v-if="selFill.hasBg" class="ep-clear" title="清除填充" @click.prevent="fmtFillClear"><X :size="12" /></button>
              </span>
            </label>
          </div>

          <div class="ep-sec">
            <div class="ep-label">描边</div>
            <div class="ep-grid">
              <label class="ep-color no-border">
                <span class="ep-color-sw" :style="{ background: selFill.border }"><input type="color" :value="selFill.border" @input="fmtBorderColor" /></span>
              </label>
              <label class="ep-field" title="描边宽度 px"><span>宽</span><input type="number" :value="selFill.bw" @change="fmtBorderWidth(+($event.target as HTMLInputElement).value)" /></label>
            </div>
          </div>

          <div class="ep-sec">
            <div class="ep-label">阴影</div>
            <select class="ep-font full" :value="selEffects.shadow" @change="fmtShadow">
              <option v-for="s in SHADOWS" :key="s.n" :value="s.v">{{ s.n }}</option>
            </select>
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
            <div class="ep-row">
              <div class="ep-btns">
                <button :class="{ on: selStyle.align === 'left' }" title="文字左对齐" @click="fmtAlign('left')"><AlignLeft :size="15" /></button>
                <button :class="{ on: selStyle.align === 'center' }" title="文字居中" @click="fmtAlign('center')"><AlignCenter :size="15" /></button>
                <button :class="{ on: selStyle.align === 'right' }" title="文字右对齐" @click="fmtAlign('right')"><AlignRight :size="15" /></button>
              </div>
              <span class="ep-color-sw" title="文字颜色" :style="{ background: selStyle.color }"><input type="color" :value="selStyle.color" @input="fmtColor" /></span>
              <input class="ep-hex" :value="selStyle.color" placeholder="#RRGGBB" spellcheck="false" @change="fmtColorHex" />
            </div>
            <label class="ep-color">
              <span>字体</span>
              <select class="ep-font" @change="fmtFontFamily">
                <option value="" disabled selected>选择字体</option>
                <option v-for="f in FONTS" :key="f.v" :value="f.v">{{ f.n }}</option>
              </select>
            </label>
            <div class="ep-grid">
              <label class="ep-field"><span>行高</span><input type="number" step="0.1" :value="selPara.lh" @change="setPara('lh', +($event.target as HTMLInputElement).value)" /></label>
              <label class="ep-field"><span>字距</span><input type="number" :value="selPara.ls" @change="setPara('ls', +($event.target as HTMLInputElement).value)" /></label>
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
            <button title="复制一份 (Ctrl+D)" @click="duplicateEl"><Copy :size="14" /> 复制</button>
            <button v-if="canUngroup" title="取消编组 (Ctrl+Shift+G)" @click="ungroupSel"><Ungroup :size="14" /> 解组</button>
            <button v-if="selEl.tagName === 'IMG'" title="换一张图（位置尺寸不变）" @click="replaceImage"><ImageIcon :size="14" /> 换图</button>
            <button class="danger" @click="fmtDelete"><Trash2 :size="14" /> 删除元素</button>
          </div>
        </template>

        <div v-else class="ep-empty">
          <MousePointer2 :size="22" :stroke-width="1.6" />
          <div class="ep-empty-t">单击画布里的文字或卡片</div>
          <div class="ep-empty-s">选中后在这里改大小 / 位置 / 字号 / 颜色 / 对齐，<br>拖动移动、拖角缩放，双击改文字；<br>Shift+点选或空白处框选可多选</div>
        </div>
      </aside>
    </div>

    <!-- Figma 往返对话框 -->
    <div v-if="figmaOpen" class="ed-figma-mask" @click.self="figmaOpen = false">
      <div class="ed-figma-dlg">
        <div class="fg-head">
          <span>接入 Figma 网页端</span>
          <button class="ep-x" title="关闭" @click="figmaOpen = false"><X :size="14" /></button>
        </div>

        <div class="fg-step">
          <div class="fg-t">① 送去 Figma 编辑</div>
          <p class="fg-p">
            点「复制页面 HTML」→ 到 Figma 里运行社区插件 <b>html.to.design</b>（免费，网页端可用）→
            选 <b>Paste HTML</b> 粘贴导入，页面就变成可自由编辑的 Figma 图层。
          </p>
          <div class="fg-row">
            <button class="fg-btn primary" @click="figmaCopyHtml">{{ figmaCopied ? "已复制 ✓" : "复制页面 HTML" }}</button>
            <button class="fg-btn" @click="openUrl('https://www.figma.com/community/plugin/1159123024924461424')">获取 html.to.design</button>
            <button class="fg-btn" @click="openUrl('https://www.figma.com')">打开 Figma</button>
          </div>
        </div>

        <div class="fg-step">
          <div class="fg-t">② 改完拉回来</div>
          <p class="fg-p">
            粘贴 Figma 文件链接 + 访问令牌（Figma 头像 → Settings → Security → <b>Personal access tokens</b>，
            只读权限即可，令牌只存在本机）。拉回会把文件里<b>面积最大的画框</b>转成页面替换当前画布——
            视觉级还原，可 <b>Ctrl+Z</b> 撤销，<b>Ctrl+S</b> 才落盘。
          </p>
          <input v-model="figmaLink" class="fg-in" placeholder="https://www.figma.com/design/…" spellcheck="false" />
          <input v-model="figmaToken" class="fg-in" type="password" placeholder="figd_… 访问令牌（自己的 Figma 账号生成，只存本机）" spellcheck="false" />
          <div class="fg-row">
            <button class="fg-btn primary" :disabled="figmaBusy" @click="figmaPullBack">
              <Loader v-if="figmaBusy" :size="13" class="spin" />
              {{ figmaBusy ? "拉取并转换中…（图片多会久一点）" : "拉回并替换画布" }}
            </button>
            <button class="fg-btn" title="打开 Figma 设置页 → Security → Personal access tokens → Generate new token" @click="openUrl('https://www.figma.com/settings')">去生成令牌</button>
          </div>
          <div v-if="figmaErr" class="fg-err">{{ figmaErr }}</div>
        </div>
      </div>
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

/* 文档标签（Markdown / 纯文本编辑时替代可视化/源码切换段） */
.ed-doc-tag { display: inline-flex; align-items: center; gap: 6px; padding: 5px 11px; border: 1px solid var(--border-soft); border-radius: 8px; background: var(--bg-soft); color: var(--text-2); font-size: 12.5px; font-weight: 600; }
.ed-doc-count { font-weight: 400; color: var(--dim); font-size: 11.5px; font-variant-numeric: tabular-nums; }

/* 富文本工具条（豆包式第二排） */
.ed-fmt { display: flex; align-items: center; gap: 3px; padding: 5px 12px; border-bottom: 1px solid var(--border-soft); background: var(--panel); flex-wrap: wrap; }
.ed-fmt .ed-ic { border: none; background: transparent; }
.ed-fmt .ed-ic:hover:not(:disabled) { background: var(--bg-soft); }
.ed-fmt-sep { width: 1px; height: 18px; background: var(--border-soft); margin: 0 5px; flex-shrink: 0; }
.ed-fmt-sel { border: 1px solid var(--border); border-radius: 7px; background: var(--bg); color: var(--text-2); font-size: 12px; padding: 4px 5px; cursor: pointer; max-width: 96px; }
.ed-fmt-sel:hover { border-color: var(--primary); color: var(--primary); }
.ed-fmt-color { position: relative; width: 30px; height: 28px; display: inline-flex; flex-direction: column; align-items: center; justify-content: center; gap: 1px; border-radius: 6px; cursor: pointer; color: var(--text-2); }
.ed-fmt-color:hover { background: var(--bg-soft); }
.ed-fmt-color::after { content: ""; width: 14px; height: 3px; border-radius: 2px; background: currentColor; opacity: .5; }
.ed-fmt-color input { position: absolute; inset: 0; opacity: 0; cursor: pointer; }
.ed-fmt-tip { margin-left: auto; font-size: 11px; color: var(--dim); }

/* 查找替换条 */
.ed-find { display: flex; align-items: center; gap: 6px; padding: 5px 12px; border-bottom: 1px solid var(--border-soft); background: var(--bg-soft); }
.ed-find-ic { color: var(--muted); flex-shrink: 0; }
.ed-find-in { width: 170px; padding: 4px 8px; border: 1px solid var(--border); border-radius: 7px; background: var(--bg); color: var(--text); font-size: 12.5px; outline: none; }
.ed-find-in:focus { border-color: var(--primary); }
.ed-find-n { font-size: 11.5px; color: var(--muted); min-width: 34px; font-variant-numeric: tabular-nums; }
.ed-find-btn { padding: 4px 10px; border: 1px solid var(--border); border-radius: 7px; background: var(--bg); color: var(--text-2); font-size: 12px; cursor: pointer; }
.ed-find-btn:hover:not(:disabled) { border-color: var(--primary); color: var(--primary); }
.ed-find-btn:disabled { opacity: .4; cursor: default; }

/* 浮动气泡工具栏：跟随文字选区，玻璃质感 */
.ed-bubble {
  position: absolute;
  z-index: 40;
  transform: translate(-50%, calc(-100% - 10px));
  display: flex;
  align-items: center;
  gap: 2px;
  padding: 5px 7px;
  border-radius: 11px;
  border: 1px solid var(--hairline, var(--border-soft));
  background: color-mix(in srgb, var(--panel) 86%, transparent);
  backdrop-filter: blur(14px) saturate(1.3);
  -webkit-backdrop-filter: blur(14px) saturate(1.3);
  box-shadow: 0 10px 34px rgba(15, 25, 45, .18), 0 2px 8px rgba(15, 25, 45, .1);
  animation: ed-bubble-in .14s ease;
  white-space: nowrap;
}
@keyframes ed-bubble-in { from { opacity: 0; transform: translate(-50%, calc(-100% - 4px)); } }
.ed-bubble .ed-ic { border: none; background: transparent; }
.ed-bubble .ed-ic:hover:not(:disabled) { background: var(--bg-soft); }

/* 主体 */
.ed-body { flex: 1; display: flex; min-height: 0; overflow: hidden; }

/* 缩略大纲 / 图层树 左栏 */
.ed-rail { width: 200px; flex-shrink: 0; overflow-y: auto; border-right: 1px solid var(--border-soft); background: var(--panel); padding: 10px; display: flex; flex-direction: column; gap: 9px; }
/* 左栏页签（deck：页面/图层；网页：图层标题） */
.ed-rail-tabs { display: flex; gap: 2px; padding: 2px; background: var(--bg-soft); border-radius: 8px; flex-shrink: 0; }
.ed-rail-tabs button { flex: 1; display: inline-flex; align-items: center; justify-content: center; gap: 4px; padding: 5px 0; border: none; background: transparent; color: var(--muted); font-size: 11.5px; font-weight: 600; border-radius: 6px; cursor: pointer; }
.ed-rail-tabs button.on { background: var(--panel); color: var(--primary); box-shadow: 0 1px 2px rgba(0, 0, 0, .08); }
.ed-rail-title { flex: 1; display: inline-flex; align-items: center; justify-content: center; gap: 4px; padding: 5px 0; color: var(--text-2); font-size: 11.5px; font-weight: 700; }

/* 图层树（Figma 式）：悬停高亮画布元素、点选联动、拖拽重排、眼睛/锁 */
.ed-layers { flex: 1; min-height: 0; overflow-y: auto; overflow-x: hidden; display: flex; flex-direction: column; gap: 1px; margin: -2px -4px 0; padding: 2px 4px 0; }
.ed-layer { position: relative; display: flex; align-items: center; gap: 4px; padding: 4px 6px; border-radius: 6px; cursor: pointer; user-select: none; min-height: 26px; }
.ed-layer:hover { background: var(--bg-soft); }
.ed-layer.on { background: var(--primary-soft); }
.ed-layer.on .ed-layer-name { color: var(--primary); font-weight: 600; }
.ed-layer.hidden .ed-layer-name, .ed-layer.hidden .ed-layer-tag { opacity: .4; }
.ed-layer.drop-before::before { content: ""; position: absolute; left: 4px; right: 4px; top: -1px; height: 2px; background: var(--primary); border-radius: 2px; }
.ed-layer.drop-after::after { content: ""; position: absolute; left: 4px; right: 4px; bottom: -1px; height: 2px; background: var(--primary); border-radius: 2px; }
.ed-layer.drop-into { box-shadow: inset 0 0 0 1.5px var(--primary); }
.ed-layer-caret { width: 14px; height: 14px; flex-shrink: 0; display: inline-flex; align-items: center; justify-content: center; border: none; background: transparent; color: var(--muted); cursor: pointer; padding: 0; border-radius: 3px; }
.ed-layer-caret:hover { background: var(--border-soft); }
.ed-layer-caret.ph { pointer-events: none; }
.ed-layer-ico { flex-shrink: 0; color: var(--muted); }
.ed-layer.on .ed-layer-ico { color: var(--primary); }
.ed-layer-name { flex: 1; min-width: 0; font-size: 11.5px; color: var(--text-2); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.ed-layer-act { width: 18px; height: 18px; flex-shrink: 0; display: none; align-items: center; justify-content: center; border: none; background: transparent; color: var(--muted); cursor: pointer; padding: 0; border-radius: 4px; }
.ed-layer:hover .ed-layer-act, .ed-layer-act.act { display: inline-flex; }
.ed-layer-act:hover { background: var(--border-soft); color: var(--text); }
.ed-layer-act.act { color: var(--primary); }
.ed-layers-empty { padding: 20px 10px; text-align: center; font-size: 11.5px; color: var(--dim); }
.ed-thumb { position: relative; display: flex; align-items: stretch; gap: 9px; padding: 0; border: none; background: transparent; cursor: pointer; }
/* 序号在缩略左侧 */
.ed-thumb-n { flex-shrink: 0; width: 18px; align-self: center; text-align: right; color: var(--muted); font-size: 11px; font-weight: 700; font-variant-numeric: tabular-nums; }
.ed-thumb.on .ed-thumb-n { color: var(--primary); }
/* 真实缩略：16:9 盒子里放等比缩放的 iframe */
.ed-thumb-prev { position: relative; flex: 1; aspect-ratio: 16 / 9; border-radius: 7px; overflow: hidden; background: #fff; border: 1.5px solid var(--border-soft); box-shadow: var(--shadow-sm, 0 1px 3px rgba(0,0,0,.08)); transition: border-color .15s, box-shadow .15s; }
.ed-thumb:hover .ed-thumb-prev { border-color: var(--border-strong); }
.ed-thumb.on .ed-thumb-prev { border-color: var(--primary); box-shadow: 0 0 0 2px var(--primary-soft); }
/* 拖拽换页序：落点页上沿亮一条主色插入线 */
.ed-thumb.drag-over .ed-thumb-prev { box-shadow: 0 -3px 0 0 var(--primary); }
.ed-thumb[draggable="true"] { cursor: grab; }
.ed-thumb-frame { position: absolute; top: 0; left: 0; width: 1280px; height: 720px; border: 0; transform-origin: top left; transform: scale(0.119); pointer-events: none; background: #fff; }
.ed-thumb-ph { position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; padding: 4px; font-size: 10px; color: var(--muted); text-align: center; }
.ed-rail-acts { display: flex; gap: 5px; margin-top: 4px; position: sticky; bottom: 0; }
.ed-rail-btn { display: inline-flex; align-items: center; gap: 4px; padding: 7px 9px; border: 1px dashed var(--border-strong); border-radius: 8px; background: var(--bg); color: var(--text-2); font-size: 12px; cursor: pointer; }
.ed-rail-btn:hover:not(:disabled) { border-color: var(--primary); color: var(--primary); }
.ed-rail-btn:disabled { opacity: .4; cursor: default; }
.ed-rail-btn.danger:hover:not(:disabled) { border-color: var(--vermilion); color: var(--vermilion); }
.ed-rail-btn:first-child { flex: 1; border-style: solid; justify-content: center; }

/* 画布区: 滚动画布 + 悬浮工具药丸（不随内容滚动） */
.ed-canvas-zone { flex: 1; min-width: 0; position: relative; display: flex; }
/* 画布：Figma 式中性平底 */
.ed-canvas { flex: 1; min-width: 0; position: relative; overflow: auto; display: flex; padding: 44px; background: #eef0f2; }
html[data-theme="dark"] .ed-canvas,
html[data-theme="aurora-dark"] .ed-canvas {
  background: #1b1c1f;
}
/* 空格/抓手工具：光标变抓手；画形状工具：十字准星 */
.ed-canvas.pan { cursor: grab; }
.ed-canvas.pan:active { cursor: grabbing; }
.ed-canvas.draw { cursor: crosshair; }
/* 壳=缩放后的视觉尺寸(margin auto 居中, 超出可四向滚动); 舞台在壳内从左上角缩放 */
.ed-stage-wrap { flex-shrink: 0; margin: auto; position: relative; overflow: hidden; border-radius: 4px;
  box-shadow: 0 0 0 1px rgba(15, 23, 42, .06), 0 14px 44px rgba(15, 23, 42, .14), 0 2px 8px rgba(15, 23, 42, .08); background: #fff; }
html[data-theme="dark"] .ed-stage-wrap,
html[data-theme="aurora-dark"] .ed-stage-wrap { box-shadow: 0 0 0 1px rgba(255, 255, 255, .07), 0 14px 44px rgba(0, 0, 0, .5); }
.ed-stage { transform-origin: top left; background: #fff; }
.ed-frame { width: 1280px; height: 720px; border: none; display: block; background: #fff; }

/* 底部悬浮工具药丸（Figma UI3 式）*/
.ed-tools { position: absolute; left: 50%; bottom: 16px; transform: translateX(-50%); z-index: 46;
  display: flex; align-items: center; gap: 2px; padding: 5px;
  border-radius: 14px; border: 1px solid var(--hairline, var(--border-soft));
  background: color-mix(in srgb, var(--panel) 90%, transparent);
  backdrop-filter: blur(18px) saturate(1.4); -webkit-backdrop-filter: blur(18px) saturate(1.4);
  box-shadow: 0 12px 36px rgba(15, 25, 45, .2), 0 2px 8px rgba(15, 25, 45, .1); }
.ed-tools button { width: 36px; height: 36px; display: inline-flex; align-items: center; justify-content: center;
  border: none; background: transparent; color: var(--text-2); border-radius: 10px; cursor: pointer; transition: background .12s, color .12s; }
.ed-tools button:hover { background: var(--bg-soft); color: var(--text); }
.ed-tools button.on { background: var(--primary); color: #fff; }
.ed-tools-sep { width: 1px; height: 20px; background: var(--border-soft); margin: 0 4px; }

.ed-hint { position: absolute; left: 50%; bottom: 78px; transform: translateX(-50%); display: inline-flex; align-items: center; gap: 6px; padding: 5px 12px; border-radius: 999px; background: color-mix(in srgb, var(--ink, #111) 82%, transparent); color: #fff; font-size: 11.5px; white-space: nowrap; pointer-events: none; transition: opacity .6s ease; z-index: 45; }

/* 右键上下文菜单（Figma 式, 玻璃质感） */
.ed-ctx { position: absolute; z-index: 60; min-width: 188px; padding: 5px;
  border-radius: 12px; border: 1px solid var(--hairline, var(--border-soft));
  background: color-mix(in srgb, var(--panel) 94%, transparent);
  backdrop-filter: blur(18px) saturate(1.4); -webkit-backdrop-filter: blur(18px) saturate(1.4);
  box-shadow: 0 14px 44px rgba(15, 25, 45, .22), 0 3px 10px rgba(15, 25, 45, .12);
  animation: ed-ctx-in .1s ease; }
@keyframes ed-ctx-in { from { opacity: 0; transform: scale(.97); } }
.ed-ctx button { width: 100%; display: flex; align-items: center; justify-content: space-between; gap: 18px;
  padding: 6px 10px; border: none; background: transparent; color: var(--text); font-size: 12.5px;
  border-radius: 7px; cursor: pointer; text-align: left; }
.ed-ctx button:hover:not(:disabled) { background: var(--primary); color: #fff; }
.ed-ctx button:hover:not(:disabled) kbd { color: rgba(255, 255, 255, .75); }
.ed-ctx button:disabled { opacity: .4; cursor: default; }
.ed-ctx button.danger:hover { background: var(--vermilion); }
.ed-ctx kbd { font-size: 10.5px; color: var(--dim); font-family: inherit; }
.ed-ctx .sep { height: 1px; margin: 4px 8px; background: var(--border-soft); }
/* 提示条 9 秒后自动淡出，不长期占画布视野 */
.ed-hint.off { opacity: 0; }

/* 源码 */
.ed-code { flex: 1; min-width: 0; display: flex; }
.ed-code-area { flex: 1; resize: none; border: none; padding: 16px 18px; background: #0f1115; color: #d6deeb; font-family: var(--mono); font-size: 12.5px; line-height: 1.6; tab-size: 2; outline: none; white-space: pre; overflow: auto; }

/* 文档编辑（Markdown / 纯文本）：左源码右预览 */
.ed-split { flex: 1; min-width: 0; display: flex; }
.ed-split .ed-code-area.doc { min-width: 0; white-space: pre-wrap; }
.ed-md-preview { flex: 1; min-width: 0; overflow: auto; padding: 28px 32px; border-left: 1px solid var(--border-soft); background: var(--panel); color: var(--text); font-size: 14px; line-height: 1.75; }

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
.ep-btns button:disabled { opacity: .35; cursor: default; }
/* Figma 式顶部对齐排: 六钮均分一整行 */
.ep-btns.wide { width: 100%; }
.ep-btns.wide button { flex: 1; }
.ep-align-row { padding-top: 10px; padding-bottom: 10px; }
.ep-font.full { width: 100%; max-width: none; padding: 6px 8px; }
.ep-color.no-border { border: none; background: transparent; padding: 4px 0; justify-content: center; }
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
.ep-font { max-width: 118px; border: 1px solid var(--border); border-radius: 6px; background: var(--bg); color: var(--text); font-size: 12px; padding: 3px 4px; cursor: pointer; }
.ep-fill-end { display: inline-flex; align-items: center; gap: 6px; }
/* hex 色值输入（Figma 式） */
.ep-hex { width: 66px; padding: 3px 6px; border: 1px solid var(--border); border-radius: 6px; background: var(--bg); color: var(--text); font-size: 11.5px; font-family: var(--mono); outline: none; }
.ep-hex:focus { border-color: var(--primary); }
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

/* Figma 往返按钮 + 对话框 */
.ed-figma { font-weight: 700; letter-spacing: .02em; }
.ed-figma-mask { position: absolute; inset: 0; z-index: 70; display: flex; align-items: center; justify-content: center; background: rgba(15, 20, 30, .38); backdrop-filter: blur(3px); }
.ed-figma-dlg { width: 520px; max-width: calc(100% - 48px); max-height: calc(100% - 48px); overflow-y: auto; border-radius: 16px; border: 1px solid var(--hairline, var(--border-soft)); background: var(--panel); box-shadow: 0 24px 80px rgba(10, 18, 35, .35); animation: ed-ctx-in .12s ease; }
.fg-head { position: sticky; top: 0; display: flex; align-items: center; justify-content: space-between; padding: 14px 18px; border-bottom: 1px solid var(--border-soft); background: var(--panel); font-size: 14px; font-weight: 700; color: var(--text); }
.fg-step { padding: 14px 18px; border-bottom: 1px solid var(--border-soft); }
.fg-step:last-child { border-bottom: none; }
.fg-t { font-size: 13px; font-weight: 700; color: var(--text); margin-bottom: 6px; }
.fg-p { font-size: 12.5px; color: var(--text-2); line-height: 1.7; margin: 0 0 10px; }
.fg-p b { color: var(--text); }
.fg-row { display: flex; gap: 8px; flex-wrap: wrap; }
.fg-btn { display: inline-flex; align-items: center; gap: 6px; padding: 7px 14px; border: 1px solid var(--border); border-radius: 8px; background: var(--bg); color: var(--text-2); font-size: 12.5px; cursor: pointer; }
.fg-btn:hover:not(:disabled) { border-color: var(--primary); color: var(--primary); }
.fg-btn.primary { background: var(--primary); border-color: var(--primary); color: #fff; font-weight: 600; }
.fg-btn.primary:hover:not(:disabled) { filter: brightness(1.07); color: #fff; }
.fg-btn:disabled { opacity: .55; cursor: default; }
.fg-in { width: 100%; margin-bottom: 8px; padding: 8px 10px; border: 1px solid var(--border); border-radius: 8px; background: var(--bg); color: var(--text); font-size: 12.5px; outline: none; }
.fg-in:focus { border-color: var(--primary); }
.fg-err { margin-top: 8px; padding: 8px 10px; border-radius: 8px; background: var(--vermilion-soft, rgba(220, 80, 50, .1)); color: var(--vermilion); font-size: 12px; line-height: 1.6; }

.spin { animation: ed-spin .9s linear infinite; }
@keyframes ed-spin { to { transform: rotate(360deg); } }
</style>
