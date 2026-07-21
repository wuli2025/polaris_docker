<script setup lang="ts">
/**
 * 网格画廊(虚拟滚动:grid 模式,列数随宽度算,DOM 只保留视口内几十张)。
 * 自 FileCenter.vue 原样搬出;cards/thumbCache/分页在父层,这里只渲染 + 事件上抛。
 */
import { RecycleScroller } from "vue-virtual-scroller";
import "vue-virtual-scroller/dist/vue-virtual-scroller.css";
import OrbitSpinner from "../icons/OrbitSpinner.vue";
import type { FileCard, FcCluster } from "../../tauri";
import { GLYPHS, KIND_LABEL, accentFor, glyphFor } from "./shared";

defineProps<{
  cards: FileCard[];
  cellH: number;
  cols: number;
  cellW: number;
  thumbCache: Map<string, string>;
  clusterById: Record<number, FcCluster>;
  loading: boolean;
  exhausted: boolean;
  total: number;
}>();

const emit = defineEmits<{
  (e: "range", start: number, end: number): void;
  (e: "end"): void;
  (e: "open", card: FileCard): void;
}>();

function onUpdate(start: number, end: number) {
  emit("range", start, end);
}
</script>

<template>
  <div class="gallery-pane">
    <RecycleScroller
      class="gallery-vs"
      :items="cards"
      :item-size="cellH"
      :grid-items="cols"
      :item-secondary-size="cellW"
      key-field="id"
      :buffer="900"
      :emit-update="true"
      @update="onUpdate"
      @scroll-end="emit('end')"
      v-slot="{ item: card }"
    >
      <div class="tile-cell" @click="emit('open', card)">
        <div class="tile glass" :style="{ '--accent': accentFor(card, clusterById) }">
          <div class="thumb">
            <img
              v-if="card.thumbable && thumbCache.get(card.abspath)"
              :src="thumbCache.get(card.abspath)"
              class="thumb-img"
              loading="lazy"
              decoding="async"
              alt=""
            />
            <div v-else class="thumb-glyph">
              <div class="glyph-halo" />
              <svg viewBox="0 0 48 48" class="glyph" v-html="GLYPHS[glyphFor(card)]" />
              <div v-if="card.thumbable && !thumbCache.has(card.abspath)" class="shimmer" />
            </div>
            <span class="ext-badge">{{ card.ext || card.kind }}</span>
            <span v-if="card.source" class="src-badge">{{ card.source }}</span>
            <span v-if="card.kind === 'video'" class="play-badge">▶</span>
          </div>
          <div class="tile-meta">
            <div class="tile-name" :title="card.name">{{ card.title || card.name }}</div>
            <div class="tile-sub">
              <span v-if="card.clusterId > 0 && clusterById[card.clusterId]" class="tile-cluster" :style="{ color: clusterById[card.clusterId].color }">
                {{ clusterById[card.clusterId].label }}
              </span>
              <span v-else class="tile-kind">{{ KIND_LABEL[card.kind] || card.kind }}</span>
              <span class="tile-size">{{ card.sizeH }}</span>
            </div>
          </div>
        </div>
      </div>
    </RecycleScroller>
    <div class="grid-status">
      <span v-if="loading" class="grid-loading"><OrbitSpinner :size="18" /> 加载中…</span>
      <span v-else-if="!cards.length" class="grid-empty">该筛选下没有文件</span>
      <span v-else-if="exhausted" class="grid-done">已显示全部 {{ total.toLocaleString() }} 个</span>
    </div>
  </div>
</template>

<style scoped>
/* 外层容器:替代原先「scroller + 状态条」两个兄弟节点,布局与 .fc-body 直挂等价 */
.gallery-pane {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
}

/* ── 琉璃通用 ── */
.glass {
  background: color-mix(in srgb, var(--panel) 68%, transparent);
  -webkit-backdrop-filter: blur(22px) saturate(1.5);
  backdrop-filter: blur(22px) saturate(1.5);
  border: 1px solid var(--border-soft);
  border-radius: 16px;
}

/* ── 画廊(虚拟滚动)── */
.gallery-vs {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 4px 6px;
}
/* 每个虚拟单元格:用 8px 内边距撑出 16px 卡片间距,卡片填满余下空间(定高) */
.tile-cell {
  box-sizing: border-box;
  width: 100%;
  height: 100%;
  padding: 8px;
}
.tile {
  display: flex;
  flex-direction: column;
  height: 100%;
  border-radius: 16px;
  overflow: hidden;
  cursor: pointer;
  transition: transform 0.2s cubic-bezier(0.2, 0.7, 0.3, 1), box-shadow 0.2s, border-color 0.2s;
  border: 1px solid var(--border-soft);
}
.tile:hover {
  transform: translateY(-3px);
  border-color: color-mix(in srgb, var(--accent) 55%, transparent);
  box-shadow:
    0 14px 34px -14px color-mix(in srgb, var(--accent) 55%, transparent),
    var(--shadow-lg);
}
.thumb {
  position: relative;
  flex: 1;
  min-height: 0;
  overflow: hidden;
  background:
    radial-gradient(110% 120% at 50% 0%, color-mix(in srgb, var(--accent) 14%, transparent), transparent 70%),
    color-mix(in srgb, var(--bg-soft) 60%, transparent);
}
.thumb-img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
  transition: transform 0.35s ease;
}
.tile:hover .thumb-img { transform: scale(1.05); }
.thumb-glyph {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
}
.glyph-halo {
  position: absolute;
  width: 96px;
  height: 96px;
  border-radius: 50%;
  background: radial-gradient(circle, color-mix(in srgb, var(--accent) 32%, transparent), transparent 68%);
  filter: blur(6px);
}
.glyph {
  position: relative;
  width: 46px;
  height: 46px;
  color: var(--text-2);
}
.tile:hover .glyph { color: var(--text); }
.glyph :deep(*) {
  fill: none;
  stroke: currentColor;
  stroke-width: 1.7;
  stroke-linecap: round;
  stroke-linejoin: round;
}
.glyph :deep(.soft) { fill: var(--accent); stroke: none; opacity: 0.16; }
.glyph :deep(.fill) { fill: var(--accent); stroke: none; opacity: 0.92; }
.glyph :deep(.acc) { stroke: var(--accent); }
.ext-badge {
  position: absolute;
  left: 9px;
  bottom: 9px;
  font-size: 9.5px;
  letter-spacing: 0.5px;
  text-transform: uppercase;
  font-family: var(--mono);
  padding: 2px 7px;
  border-radius: 6px;
  color: #fff;
  background: color-mix(in srgb, var(--accent) 82%, #000 10%);
  -webkit-backdrop-filter: blur(6px);
  backdrop-filter: blur(6px);
  box-shadow: 0 2px 8px -2px color-mix(in srgb, var(--accent) 70%, transparent);
}
.src-badge {
  position: absolute;
  left: 9px;
  top: 9px;
  font-size: 9.5px;
  letter-spacing: 0.3px;
  padding: 2px 7px;
  border-radius: 6px;
  color: #fff;
  background: color-mix(in srgb, #6fcf97 80%, #000 12%);
  -webkit-backdrop-filter: blur(6px);
  backdrop-filter: blur(6px);
  box-shadow: 0 2px 8px -2px color-mix(in srgb, #6fcf97 60%, transparent);
}
.play-badge {
  position: absolute;
  right: 9px;
  bottom: 9px;
  width: 24px;
  height: 24px;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 9px;
  color: #fff;
  background: rgba(0, 0, 0, 0.45);
  border-radius: 50%;
  -webkit-backdrop-filter: blur(4px);
  backdrop-filter: blur(4px);
}
.shimmer {
  position: absolute;
  inset: 0;
  background: linear-gradient(100deg, transparent 30%, color-mix(in srgb, var(--accent) 10%, transparent) 50%, transparent 70%);
  background-size: 220% 100%;
  animation: shimmer 1.4s infinite;
}
@keyframes shimmer { to { background-position: -220% 0; } }
.tile-meta { flex: none; padding: 10px 12px 12px; }
.tile-name {
  font-size: 12.5px;
  color: var(--text);
  font-weight: 550;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.tile-sub {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  margin-top: 4px;
}
.tile-cluster, .tile-kind {
  font-size: 11px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.tile-kind { color: var(--muted); }
.tile-size {
  font-size: 10.5px;
  color: var(--dim);
  font-variant-numeric: tabular-nums;
  flex: none;
}
/* 画廊底部状态条:加载中 / 空 / 已到底(虚拟滚动后不再是网格内的格子)。 */
.grid-status {
  flex: none;
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: 26px;
  padding: 6px 12px 12px;
}
.grid-loading, .grid-empty, .grid-done {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  color: var(--muted);
  font-size: 12.5px;
}
.grid-done { color: var(--dim); }
</style>
