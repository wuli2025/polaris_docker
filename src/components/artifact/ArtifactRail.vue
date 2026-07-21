<script setup lang="ts">
/**
 * 左栏：deck = 页面缩略 / 图层树 双页签；网页设计模式 = 图层树（Figma 式）。
 * 从 ArtifactEditor.vue 原样搬移；页签/折叠/拖拽等纯局部状态下沉到本组件，
 * 对画布 DOM 的动作通过 api 回调走父层（父层用 :key=文件路径 重挂载即等价于旧的状态重置）。
 */
import { ref, computed } from "vue";
import {
  ChevronRight, ChevronDown, Plus, Copy, Trash2,
  Layers, LayoutList, Eye, EyeOff, Lock, LockOpen,
} from "@lucide/vue";
import { type LayerNode, layerIcon, VOID_TAGS } from "./shared";

const props = defineProps<{
  isDeck: boolean;
  slides: { title: string; accent: string }[];
  cur: number;
  total: number;
  thumbs: string[];
  layerRows: LayerNode[];
  allSels: HTMLElement[];
  api: {
    doc: () => Document | null;
    goSlide: (n: number) => void;
    addSlide: (duplicate: boolean) => void;
    deleteSlide: () => void;
    /** 缩略图拖拽换页序：父层做 DOM 重排 + 重载 */
    reorderSlides: (from: number, to: number) => void;
    selectEl: (el: HTMLElement | null, additive?: boolean) => void;
    clearSel: () => void;
    refreshSel: () => void;
    markDirty: () => void;
    rebuildThumbs: () => void;
    pushHistory: () => void;
  };
}>();

const railTab = ref<"pages" | "layers">("pages");
const layerCollapsed = ref<Set<number>>(new Set());
const layerDragId = ref<number | null>(null);
const layerDropId = ref<number | null>(null);
const layerDropPos = ref<"before" | "after" | "into" | null>(null);

/** 折叠过滤：祖先被折叠的行不显示 */
const visibleLayers = computed(() => {
  const hiddenUnder = new Set<number>();
  const out: LayerNode[] = [];
  for (const n of props.layerRows) {
    if (n.parentId != null && hiddenUnder.has(n.parentId)) { hiddenUnder.add(n.id); continue; }
    out.push(n);
    if (layerCollapsed.value.has(n.id)) hiddenUnder.add(n.id);
  }
  return out;
});
function layerHover(n: LayerNode | null) {
  const d = props.api.doc();
  if (!d) return;
  d.querySelectorAll(".__hov").forEach((x) => x.classList.remove("__hov"));
  if (n && !props.allSels.includes(n.el)) n.el.classList.add("__hov");
}
function layerClick(n: LayerNode, e: MouseEvent) {
  if (n.locked) return;
  props.api.selectEl(n.el, e.shiftKey);
  try { n.el.scrollIntoView({ block: "nearest", inline: "nearest" }); } catch { /* 部分实现不支持 */ }
  props.api.refreshSel();
}
function layerToggleCollapse(n: LayerNode) {
  const s = new Set(layerCollapsed.value);
  if (s.has(n.id)) s.delete(n.id);
  else s.add(n.id);
  layerCollapsed.value = s;
}
function layerToggleEye(n: LayerNode) {
  n.el.style.display = n.el.style.display === "none" ? "" : "none";
  if (n.el.style.display === "none" && props.allSels.includes(n.el)) props.api.clearSel();
  props.api.markDirty();
  props.api.rebuildThumbs();
  props.api.refreshSel();
  props.api.pushHistory(); // 同步重建图层树
}
function layerToggleLock(n: LayerNode) {
  if (n.el.hasAttribute("data-plk")) n.el.removeAttribute("data-plk");
  else {
    n.el.setAttribute("data-plk", "1");
    if (props.allSels.includes(n.el)) props.api.clearSel();
  }
  props.api.markDirty();
  props.api.pushHistory();
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
  const from = props.layerRows.find((r) => r.id === fromId);
  const to = props.layerRows.find((r) => r.id === toId);
  if (!from || !to || from.el.contains(to.el)) return; // 不能拖进自己的后代
  if (pos === "into") to.el.appendChild(from.el);
  else if (pos === "before") to.el.before(from.el);
  else to.el.after(from.el);
  props.api.markDirty();
  props.api.rebuildThumbs();
  props.api.refreshSel();
  props.api.pushHistory(); // 同步重建图层树
}
function onLayerDragEnd() {
  layerDragId.value = null;
  layerDropId.value = null;
  layerDropPos.value = null;
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
  props.api.reorderSlides(from, i);
}
function onThumbDragEnd() {
  thumbDragIdx.value = null;
  thumbDragOver.value = null;
}
</script>

<template>
  <aside class="ed-rail">
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
        @click="api.goSlide(i)"
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
        <button class="ed-rail-btn" title="新增一页（空白同款）" @click="api.addSlide(false)"><Plus :size="13" /> 加页</button>
        <button class="ed-rail-btn" title="复制当前页" @click="api.addSlide(true)"><Copy :size="12" /></button>
        <button class="ed-rail-btn danger" title="删除当前页" :disabled="total <= 1" @click="api.deleteSlide()"><Trash2 :size="12" /></button>
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
</template>

<style scoped>
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
</style>
