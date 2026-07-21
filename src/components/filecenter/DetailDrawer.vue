<script setup lang="ts">
/**
 * 文件详情抽屉:大图/字形 hero + 元信息 + 内容速览 + 打开/定位。
 * 自 FileCenter.vue 原样搬出;速览与大图的按需拉取下沉到本组件。
 */
import { ref, watch } from "vue";
import { X, Sparkles, ExternalLink, FolderOpen } from "@lucide/vue";
import OrbitSpinner from "../icons/OrbitSpinner.vue";
import { files as fc, artifacts as artifactsApi, type FileCard, type FcCluster } from "../../tauri";
import { GLYPHS, KIND_LABEL, accentFor, glyphFor, fmtTime } from "./shared";

const props = defineProps<{
  card: FileCard | null;
  clusterById: Record<number, FcCluster>;
  /** 打开时父层缩略图缓存里已有的图(有则立即显示,免二次拉取)。 */
  seedThumb: string | null;
}>();

const emit = defineEmits<{
  (e: "close"): void;
  /** 打开/定位失败等轻提示,上抛给父层的 opMsg 展示位。 */
  (e: "msg", text: string): void;
}>();

const detailGist = ref("");
const detailThumb = ref<string | null>(null);

// 选中卡片变化 → 拉速览(按需 + 缓存)与高清缩略图。
watch(
  () => props.card,
  (card) => {
    if (!card) return;
    detailGist.value = "";
    detailThumb.value = props.seedThumb || null;
    fc.gist(card.abspath).then((g) => {
      if (props.card?.abspath === card.abspath) detailGist.value = g;
    });
    if (card.thumbable && !detailThumb.value) {
      fc.thumb(card.abspath, 640).then((u) => {
        if (props.card?.abspath === card.abspath) detailThumb.value = u;
      });
    }
  },
  { immediate: true },
);

async function openExternal(card: FileCard) {
  try {
    await artifactsApi.openExternal(card.abspath);
  } catch (e: any) {
    emit("msg", `打开失败:${e?.message ?? e}`);
  }
}
async function revealCard(card: FileCard) {
  try {
    await artifactsApi.reveal(card.abspath);
  } catch (e: any) {
    emit("msg", `定位失败:${e?.message ?? e}`);
  }
}
</script>

<template>
  <transition name="drawer">
    <div v-if="card" class="detail glass" :style="{ '--accent': accentFor(card, clusterById) }">
      <button class="detail-close" @click="emit('close')"><X :size="16" :stroke-width="2" /></button>
      <div class="detail-hero">
        <img v-if="detailThumb" decoding="async" :src="detailThumb" class="detail-img" alt="" />
        <div v-else class="detail-glyph">
          <div class="glyph-halo big" />
          <svg viewBox="0 0 48 48" class="glyph" v-html="GLYPHS[glyphFor(card)]" />
        </div>
      </div>
      <div class="detail-name">{{ card.title || card.name }}</div>
      <div v-if="card.title && card.title !== card.name" class="detail-rawname" :title="card.name">原名：{{ card.name }}</div>
      <div class="detail-path">{{ card.path }}</div>
      <div class="detail-tags">
        <span class="dtag">{{ KIND_LABEL[card.kind] || card.kind }}</span>
        <span class="dtag">{{ card.sizeH }}</span>
        <span v-if="card.clusterId > 0 && clusterById[card.clusterId]" class="dtag cluster" :style="{ '--c': clusterById[card.clusterId].color }">
          {{ clusterById[card.clusterId].label }}
        </span>
        <span class="dtag dim">{{ fmtTime(card.mtime) }}</span>
      </div>
      <div class="detail-gist">
        <div class="gist-head"><Sparkles :size="13" :stroke-width="1.7" /> 内容速览</div>
        <div v-if="detailGist" class="gist-body">{{ detailGist }}</div>
        <div v-else class="gist-body loading"><OrbitSpinner :size="13" /> 生成中…</div>
      </div>
      <div class="detail-actions">
        <button class="detail-btn primary" @click="openExternal(card)"><ExternalLink :size="14" :stroke-width="1.8" /> 打开</button>
        <button class="detail-btn" @click="revealCard(card)"><FolderOpen :size="14" :stroke-width="1.8" /> 在文件夹中显示</button>
      </div>
    </div>
  </transition>
  <transition name="fade">
    <div v-if="card" class="detail-scrim" @click="emit('close')" />
  </transition>
</template>

<style scoped>
/* ── 琉璃通用 ── */
.glass {
  background: color-mix(in srgb, var(--panel) 68%, transparent);
  -webkit-backdrop-filter: blur(22px) saturate(1.5);
  backdrop-filter: blur(22px) saturate(1.5);
  border: 1px solid var(--border-soft);
  border-radius: 16px;
}

/* 字形通用(与画廊一致):thin 单线 + accent 高光 */
.glyph-halo {
  position: absolute;
  width: 96px;
  height: 96px;
  border-radius: 50%;
  background: radial-gradient(circle, color-mix(in srgb, var(--accent) 32%, transparent), transparent 68%);
  filter: blur(6px);
}
.glyph-halo.big { width: 150px; height: 150px; }
.glyph {
  position: relative;
  width: 46px;
  height: 46px;
  color: var(--text-2);
}
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

/* ── 详情抽屉 ── */
.detail-scrim {
  position: absolute;
  inset: 0;
  z-index: 60;
  background: var(--overlay);
  -webkit-backdrop-filter: blur(2px);
  backdrop-filter: blur(2px);
}
.detail {
  position: absolute;
  top: 8px;
  right: 8px;
  bottom: 8px;
  width: 360px;
  max-width: calc(100% - 16px);
  z-index: 61;
  display: flex;
  flex-direction: column;
  padding: 18px;
  overflow-y: auto;
  box-shadow: var(--shadow-lg);
}
.detail-close {
  position: absolute;
  top: 14px;
  right: 14px;
  display: inline-flex;
  border: none;
  background: var(--selection-bg);
  color: var(--muted);
  border-radius: 8px;
  padding: 5px;
  cursor: pointer;
}
.detail-close:hover { color: var(--text); background: var(--selection-bg-hover); }
.detail-hero {
  position: relative;
  aspect-ratio: 16 / 10;
  border-radius: 14px;
  overflow: hidden;
  background:
    radial-gradient(120% 120% at 50% 0%, color-mix(in srgb, var(--accent) 16%, transparent), transparent 70%),
    var(--bg-soft);
  display: flex;
  align-items: center;
  justify-content: center;
  margin-bottom: 14px;
}
.detail-img { width: 100%; height: 100%; object-fit: contain; }
.detail-glyph { position: relative; display: flex; align-items: center; justify-content: center; }
.detail-glyph .glyph { width: 76px; height: 76px; }
.detail-name {
  font-size: 15px;
  font-weight: 600;
  color: var(--text);
  word-break: break-all;
  line-height: 1.4;
}
.detail-rawname {
  font-size: 11px;
  color: var(--muted);
  margin-top: 3px;
  word-break: break-all;
  line-height: 1.4;
}
.detail-path {
  font-size: 11px;
  color: var(--dim);
  font-family: var(--mono);
  margin-top: 4px;
  word-break: break-all;
}
.detail-tags {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin-top: 12px;
}
.dtag {
  font-size: 11px;
  padding: 3px 10px;
  border-radius: 99px;
  background: var(--selection-bg);
  color: var(--text-2);
}
.dtag.dim { color: var(--muted); }
.dtag.cluster {
  --c: var(--muted);
  color: var(--c);
  background: color-mix(in srgb, var(--c) 14%, transparent);
  border: 1px solid color-mix(in srgb, var(--c) 30%, transparent);
}
.detail-gist {
  margin-top: 16px;
  padding: 12px 14px;
  border-radius: 12px;
  background: color-mix(in srgb, var(--accent) 6%, var(--bg-soft));
  border: 1px solid var(--border-soft);
}
.gist-head {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 11.5px;
  color: var(--accent);
  margin-bottom: 6px;
}
.gist-body { font-size: 12.5px; color: var(--text-2); line-height: 1.7; }
.gist-body.loading { display: flex; align-items: center; gap: 6px; color: var(--muted); }
.detail-actions {
  display: flex;
  gap: 8px;
  margin-top: auto;
  padding-top: 16px;
}
.detail-btn {
  flex: 1;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  height: 36px;
  border: 1px solid var(--border-soft);
  background: color-mix(in srgb, var(--panel) 70%, transparent);
  color: var(--text-2);
  border-radius: 10px;
  font-size: 12.5px;
  cursor: pointer;
  transition: all 0.16s;
}
.detail-btn:hover { color: var(--text); border-color: var(--border-strong); }
.detail-btn.primary {
  background: var(--btn-solid-bg);
  color: var(--btn-solid-text);
  border-color: transparent;
}
.detail-btn.primary:hover { opacity: 0.9; }

/* ── 动效 ── */
.drawer-enter-active, .drawer-leave-active { transition: transform 0.26s cubic-bezier(0.2, 0.7, 0.3, 1), opacity 0.26s; }
.drawer-enter-from, .drawer-leave-to { transform: translateX(20px); opacity: 0; }
.fade-enter-active, .fade-leave-active { transition: opacity 0.26s; }
.fade-enter-from, .fade-leave-to { opacity: 0; }
</style>
