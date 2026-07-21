<script setup lang="ts">
/**
 * 列表视图(虚拟滚动:定高行,表头固定,DOM 只保留视口内几十行)。
 * 自 FileCenter.vue 原样搬出;cards/分页在父层,这里只渲染 + 事件上抛。
 */
import { RecycleScroller } from "vue-virtual-scroller";
import "vue-virtual-scroller/dist/vue-virtual-scroller.css";
import OrbitSpinner from "../icons/OrbitSpinner.vue";
import type { FileCard, FcCluster } from "../../tauri";
import { GLYPHS, KIND_LABEL, accentFor, glyphFor, fmtTime } from "./shared";

defineProps<{
  cards: FileCard[];
  clusterById: Record<number, FcCluster>;
  loading: boolean;
}>();

const emit = defineEmits<{
  (e: "end"): void;
  (e: "open", card: FileCard): void;
}>();
</script>

<template>
  <div class="listview">
    <div class="lv-head">
      <span class="lv-c-name">名称</span>
      <span class="lv-c-cluster">归类</span>
      <span class="lv-c-kind">类型</span>
      <span class="lv-c-size">大小</span>
      <span class="lv-c-time">修改</span>
    </div>
    <RecycleScroller
      class="lv-scroll"
      :items="cards"
      :item-size="42"
      key-field="id"
      :buffer="900"
      @scroll-end="emit('end')"
      v-slot="{ item: card }"
    >
      <div
        class="lv-row"
        :style="{ '--accent': accentFor(card, clusterById) }"
        @click="emit('open', card)"
      >
        <span class="lv-c-name">
          <svg viewBox="0 0 48 48" class="glyph lv-glyph" v-html="GLYPHS[glyphFor(card)]" />
          <span class="lv-name" :title="card.name">{{ card.title || card.name }}</span>
        </span>
        <span class="lv-c-cluster">
          <span v-if="card.clusterId > 0 && clusterById[card.clusterId]" class="lv-tag" :style="{ '--c': clusterById[card.clusterId].color }">
            {{ clusterById[card.clusterId].label }}
          </span>
          <span v-else class="lv-dim">—</span>
        </span>
        <span class="lv-c-kind">{{ KIND_LABEL[card.kind] || card.kind }}</span>
        <span class="lv-c-size">{{ card.sizeH }}</span>
        <span class="lv-c-time">{{ fmtTime(card.mtime) }}</span>
      </div>
    </RecycleScroller>
    <div v-if="loading" class="grid-status"><span class="grid-loading"><OrbitSpinner :size="18" /> 加载中…</span></div>
  </div>
</template>

<style scoped>
/* 字形通用(与画廊一致):thin 单线 + accent 高光。放最前,保证 .lv-glyph 的 1.8 线宽能覆盖 */
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

/* ── 列表 ── */
.listview { display: flex; flex-direction: column; flex: 1; min-height: 0; padding: 4px 6px; }
.lv-scroll { flex: 1; min-height: 0; overflow-y: auto; }
.lv-head, .lv-row {
  display: grid;
  grid-template-columns: 1fr 160px 80px 90px 130px;
  gap: 10px;
  align-items: center;
}
.lv-head {
  flex: none;
  padding: 9px 12px;
  font-size: 11px;
  color: var(--muted);
  letter-spacing: 0.5px;
  border-bottom: 1px solid var(--hairline);
  background: color-mix(in srgb, var(--bg) 80%, transparent);
}
.lv-row {
  /* 定高行:必须与 RecycleScroller 的 item-size(42)一致,否则会错位/重叠 */
  height: 42px;
  box-sizing: border-box;
  padding: 0 12px;
  border-radius: 10px;
  cursor: pointer;
  font-size: 12.5px;
  color: var(--text-2);
  transition: background 0.14s;
}
.lv-row:hover { background: var(--selection-bg); }
.lv-c-name { display: flex; align-items: center; gap: 9px; min-width: 0; }
.lv-glyph { width: 22px; height: 22px; flex: none; color: var(--accent); }
.lv-glyph :deep(*) { stroke-width: 1.8; }
.lv-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--text); }
.lv-tag {
  --c: var(--muted);
  font-size: 11px;
  padding: 2px 9px;
  border-radius: 99px;
  color: var(--c);
  background: color-mix(in srgb, var(--c) 14%, transparent);
  border: 1px solid color-mix(in srgb, var(--c) 30%, transparent);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  display: inline-block;
  max-width: 100%;
}
.lv-dim { color: var(--dim); }
.lv-c-size, .lv-c-time { font-variant-numeric: tabular-nums; color: var(--muted); font-size: 11.5px; }

/* 底部状态条(与画廊一致) */
.grid-status {
  flex: none;
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: 26px;
  padding: 6px 12px 12px;
}
.grid-loading {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  color: var(--muted);
  font-size: 12.5px;
}
</style>
