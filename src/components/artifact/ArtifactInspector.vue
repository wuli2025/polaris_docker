<script setup lang="ts">
/**
 * 右侧属性面板（Figma 式检查器：deck 和网页设计模式都有）。
 * 从 ArtifactEditor.vue 原样搬移：只在面板里用到的格式化函数（fmt* / setGeom / setPara /
 * alignToPage / alignSel / distributeSel …）随模板一起下沉；跨块动作（删除/编组/复制等）走 api 回调。
 */
import { computed } from "vue";
import {
  X, Copy, Trash2, Plus, Minus, RotateCw, Bold, Italic, Underline,
  AlignLeft, AlignCenter, AlignRight, BringToFront, SendToBack,
  MousePointer2, Image as ImageIcon, Group, Ungroup,
  AlignStartVertical, AlignCenterVertical, AlignEndVertical,
  AlignStartHorizontal, AlignCenterHorizontal, AlignEndHorizontal,
  AlignHorizontalDistributeCenter, AlignVerticalDistributeCenter,
} from "@lucide/vue";
import {
  SHADOWS, FONTS, normHex, getTranslate, getRotate, applyTransform, setTranslate,
} from "./shared";

const props = defineProps<{
  selEl: HTMLElement | null;
  /** 追加选的个数（>0 走多选面板） */
  extraCount: number;
  allSels: HTMLElement[];
  selGeom: { x: number; y: number; w: number; h: number; rot: number };
  selStyle: { bold: boolean; italic: boolean; underline: boolean; align: string; size: number; color: string };
  selPara: { lh: number; ls: number };
  selFill: { bg: string; hasBg: boolean; border: string; bw: number; radius: number };
  selEffects: { opacity: number; shadow: string };
  api: {
    /** markDirty + refreshSel + pushHistory(400) */
    afterFmt: () => void;
    activeSlide: () => HTMLElement | null;
    clearSel: () => void;
    fmtDelete: () => void;
    groupSel: () => void;
    ungroupSel: () => void;
    duplicateEl: () => void;
    replaceImage: () => void;
  };
}>();

const canUngroup = computed(() => !!props.selEl?.hasAttribute("data-group"));

// 格式工具栏动作
function fmtBold() { const el = props.selEl; if (!el) return; el.style.fontWeight = props.selStyle.bold ? "400" : "800"; props.api.afterFmt(); }
function fmtItalic() { const el = props.selEl; if (!el) return; el.style.fontStyle = props.selStyle.italic ? "normal" : "italic"; props.api.afterFmt(); }
function fmtUnderline() { const el = props.selEl; if (!el) return; el.style.textDecoration = props.selStyle.underline ? "none" : "underline"; props.api.afterFmt(); }
function fmtAlign(a: string) { const el = props.selEl; if (!el) return; el.style.textAlign = a; props.api.afterFmt(); }
function fmtFont(delta: number) { const el = props.selEl; if (!el) return; el.style.fontSize = Math.max(8, props.selStyle.size + delta) + "px"; props.api.afterFmt(); }
function fmtColor(e: Event) { const el = props.selEl; if (!el) return; const c = (e.target as HTMLInputElement).value; el.style.color = c; (el.style as any).webkitTextFillColor = c; props.api.afterFmt(); }
function fmtFront() { const el = props.selEl; if (!el) return; if (getComputedStyle(el).position === "static") el.style.position = "relative"; el.style.zIndex = "60"; props.api.afterFmt(); }
function fmtBack() { const el = props.selEl; if (!el) return; if (getComputedStyle(el).position === "static") el.style.position = "relative"; el.style.zIndex = "1"; props.api.afterFmt(); }
// 填充 / 描边 / 圆角
function fmtFill(e: Event) { const el = props.selEl; if (!el) return; el.style.backgroundColor = (e.target as HTMLInputElement).value; props.api.afterFmt(); }
function fmtFillClear() { const el = props.selEl; if (!el) return; el.style.backgroundColor = "transparent"; props.api.afterFmt(); }
function fmtBorderColor(e: Event) { const el = props.selEl; if (!el) return; const c = (e.target as HTMLInputElement).value; el.style.borderColor = c; if (!parseFloat(getComputedStyle(el).borderTopWidth)) el.style.borderWidth = "2px", el.style.borderStyle = "solid"; props.api.afterFmt(); }
function fmtBorderWidth(v: number) { const el = props.selEl; if (!el) return; const w = Math.max(0, v); el.style.borderWidth = w + "px"; el.style.borderStyle = w ? "solid" : "none"; props.api.afterFmt(); }
function fmtRadius(v: number) { const el = props.selEl; if (!el) return; el.style.borderRadius = Math.max(0, v) + "px"; props.api.afterFmt(); }

function fmtOpacity(v: number) {
  const el = props.selEl;
  if (!el || isNaN(v)) return;
  el.style.opacity = String(Math.max(0, Math.min(100, v)) / 100);
  props.api.afterFmt();
}
function fmtShadow(e: Event) {
  const el = props.selEl;
  if (!el) return;
  const v = (e.target as HTMLSelectElement).value;
  el.style.boxShadow = v === "none" ? "" : v;
  props.api.afterFmt();
}

function fmtFillHex(e: Event) {
  const h = normHex((e.target as HTMLInputElement).value);
  const el = props.selEl;
  if (!h || !el) return;
  el.style.backgroundColor = h;
  props.api.afterFmt();
}
function fmtColorHex(e: Event) {
  const h = normHex((e.target as HTMLInputElement).value);
  const el = props.selEl;
  if (!h || !el) return;
  el.style.color = h;
  (el.style as any).webkitTextFillColor = h;
  props.api.afterFmt();
}

// 右侧面板：整个选中元素换字体（PPT 习惯）
function fmtFontFamily(e: Event) {
  const el = props.selEl;
  const sel = e.target as HTMLSelectElement;
  const v = sel.value;
  sel.value = "";
  if (!el || !v) return;
  el.style.fontFamily = v;
  props.api.afterFmt();
}

// 右侧面板：位置/大小/旋转
function setGeom(field: "x" | "y" | "w" | "h" | "rot", v: number) {
  const el = props.selEl;
  const slide = props.api.activeSlide();
  if (!el || !slide || isNaN(v)) return;
  const r = el.getBoundingClientRect();
  const sr = slide.getBoundingClientRect();
  const t = getTranslate(el);
  const rot = getRotate(el);
  if (field === "x") applyTransform(el, t.tx + (v - (r.left - sr.left)), t.ty, rot);
  else if (field === "y") applyTransform(el, t.tx, t.ty + (v - (r.top - sr.top)), rot);
  else if (field === "rot") applyTransform(el, t.tx, t.ty, v);
  else { el.style.boxSizing = "border-box"; el.style[field === "w" ? "width" : "height"] = Math.max(8, v) + "px"; }
  props.api.afterFmt();
}
// 右侧面板：段落
function setPara(field: "lh" | "ls", v: number) {
  const el = props.selEl;
  if (!el || isNaN(v)) return;
  if (field === "lh") el.style.lineHeight = String(v);
  else el.style.letterSpacing = v + "px";
  props.api.afterFmt();
}

/** 一键对齐到页面（PPT 式：左/水平居中/右/顶/垂直居中/底） */
function alignToPage(which: "left" | "hcenter" | "right" | "top" | "vcenter" | "bottom") {
  const el = props.selEl;
  const slide = props.api.activeSlide();
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
  props.api.afterFmt();
}
/** 多选相互对齐（以选中集合的外接框为基准；单选时退化为对齐到页面） */
function alignSel(which: "left" | "hcenter" | "right" | "top" | "vcenter" | "bottom") {
  const els = props.allSels;
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
  props.api.afterFmt();
}
/** 等间距分布（≥3 个）：首尾不动，中间元素均摊间隙 */
function distributeSel(axis: "h" | "v") {
  const els = props.allSels;
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
  props.api.afterFmt();
}
</script>

<template>
  <aside class="ed-panel">
    <!-- 多选：相互对齐 / 等间距分布 / 批量删除 -->
    <template v-if="extraCount">
      <div class="ep-head"><span>已选 {{ allSels.length }} 个元素</span><button class="ep-x" title="取消选中" @click="api.clearSel()"><X :size="14" /></button></div>
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
        <button title="编组 (Ctrl+G)：包成一个整体一起拖" @click="api.groupSel()"><Group :size="14" /> 编组</button>
        <button class="danger" @click="api.fmtDelete()"><Trash2 :size="14" /> 删除全部</button>
      </div>
    </template>

    <template v-else-if="selEl">
      <div class="ep-head"><span>元素格式</span><button class="ep-x" title="取消选中" @click="api.clearSel()"><X :size="14" /></button></div>

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
        <button title="复制一份 (Ctrl+D)" @click="api.duplicateEl()"><Copy :size="14" /> 复制</button>
        <button v-if="canUngroup" title="取消编组 (Ctrl+Shift+G)" @click="api.ungroupSel()"><Ungroup :size="14" /> 解组</button>
        <button v-if="selEl.tagName === 'IMG'" title="换一张图（位置尺寸不变）" @click="api.replaceImage()"><ImageIcon :size="14" /> 换图</button>
        <button class="danger" @click="api.fmtDelete()"><Trash2 :size="14" /> 删除元素</button>
      </div>
    </template>

    <div v-else class="ep-empty">
      <MousePointer2 :size="22" :stroke-width="1.6" />
      <div class="ep-empty-t">单击画布里的文字或卡片</div>
      <div class="ep-empty-s">选中后在这里改大小 / 位置 / 字号 / 颜色 / 对齐，<br>拖动移动、拖角缩放，双击改文字；<br>Shift+点选或空白处框选可多选</div>
    </div>
  </aside>
</template>

<style scoped>
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
</style>
