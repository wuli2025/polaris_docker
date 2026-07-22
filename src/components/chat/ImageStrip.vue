<script setup lang="ts">
// ImageStrip —— 本轮生成图片的横排缩略图画廊(LUMI 式一排)。
// 图片产物不再混在文件行里当图标,而是以真缩略图横排铺开:加载中 shimmer 骨架,
// 点击进右抽屉预览;超过封顶数收进「+N」块引导去文件夹,永不把超长画廊塞进对话。
import { ref, computed, watch } from "vue";
import { Eye, ImageOff } from "@lucide/vue";
import { fileName, loadImageThumb } from "./shared";
import { useArtifactsStore } from "../../stores/artifacts";

const props = defineProps<{
  /** 本轮全部图片产物路径(已按生成顺序) */
  paths: string[];
  /** 「打开文件夹」目标 —— 缩略图超出封顶时 +N 块的兜底入口 */
  folder?: string;
}>();
const emit = defineEmits<{
  (e: "open", path: string): void;
  (e: "open-folder", path: string): void;
}>();

const artifactsStore = useArtifactsStore();

// 一排最多 12 张,余量收进「+N」
const MAX_THUMBS = 12;
const shown = computed(() => props.paths.slice(0, MAX_THUMBS));
const moreCount = computed(() => Math.max(0, props.paths.length - MAX_THUMBS));

// 路径 → src:undefined=加载中(shimmer),null=读失败(兜底图标),string=可显示。
// 底层缓存在 shared.loadImageThumb,这里只是本组件的响应式投影。
const srcs = ref<Record<string, string | null>>({});
watch(
  () => props.paths,
  (list) => {
    for (const p of list.slice(0, MAX_THUMBS)) {
      if (p in srcs.value) continue;
      loadImageThumb(p).then((u) => {
        srcs.value[p] = u;
      });
    }
  },
  { immediate: true }
);

function openMore() {
  if (props.folder) emit("open-folder", props.folder);
  else emit("open", props.paths[MAX_THUMBS]);
}
</script>

<template>
  <div class="img-strip" role="list">
    <button
      v-for="(p, i) in shown"
      :key="p"
      class="im-thumb"
      :class="{ active: artifactsStore.current?.path === p }"
      :title="fileName(p)"
      role="listitem"
      @click="emit('open', p)"
    >
      <img
        v-if="srcs[p]"
        :src="srcs[p]!"
        :alt="fileName(p)"
        loading="lazy"
        decoding="async"
      />
      <span v-else-if="srcs[p] === null" class="im-fallback">
        <ImageOff :size="18" :stroke-width="1.6" />
      </span>
      <span v-else class="im-skeleton"></span>
      <span class="im-veil">
        <Eye :size="13" :stroke-width="2" />
        预览
      </span>
      <span
        v-if="i === shown.length - 1 && !moreCount && paths.length > 1"
        class="im-count"
        >图 {{ paths.length }}</span
      >
    </button>
    <button v-if="moreCount" class="im-thumb im-more" @click="openMore">
      +{{ moreCount }}
    </button>
  </div>
</template>

<style scoped>
/* ── 横排画廊:等宽方块一排铺开,溢出横向滚动 ── */
.img-strip {
  display: flex;
  gap: 9px;
  overflow-x: auto;
  padding: 2px 2px 8px;
  margin-bottom: 4px;
  scrollbar-width: thin;
  animation: strip-rise 0.35s var(--ease-out) both;
}
@keyframes strip-rise {
  from {
    opacity: 0;
    transform: translateY(6px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}
@media (prefers-reduced-motion: reduce) {
  .img-strip {
    animation: none;
  }
}

.im-thumb {
  position: relative;
  flex: 0 0 auto;
  width: 112px;
  height: 112px;
  padding: 0;
  border: 1px solid var(--border-soft);
  border-radius: 12px;
  background: var(--bg-soft);
  overflow: hidden;
  cursor: pointer;
  transition: border-color 0.18s, box-shadow 0.25s var(--ease-out),
    transform 0.25s var(--ease-spring);
}
.im-thumb:hover {
  border-color: var(--primary);
  transform: translateY(-2px);
  box-shadow: 0 8px 22px var(--primary-soft), 0 2px 6px rgba(0, 0, 0, 0.07);
}
.im-thumb.active {
  border-color: var(--primary);
  box-shadow: 0 0 0 2px var(--primary-soft);
}
.im-thumb img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
  transition: transform 0.4s var(--ease-out);
}
.im-thumb:hover img {
  transform: scale(1.06);
}

/* 加载骨架:流光 shimmer,生成中/读盘中都用它占位 */
.im-skeleton {
  position: absolute;
  inset: 0;
  background: linear-gradient(
    100deg,
    var(--bg-soft) 38%,
    rgba(255, 255, 255, 0.65) 50%,
    var(--bg-soft) 62%
  );
  background-size: 200% 100%;
  animation: im-shimmer 1.3s ease-in-out infinite;
}
html[data-theme="dark"] .im-skeleton {
  background: linear-gradient(
    100deg,
    var(--bg-soft) 38%,
    rgba(255, 255, 255, 0.08) 50%,
    var(--bg-soft) 62%
  );
  background-size: 200% 100%;
}
@keyframes im-shimmer {
  from {
    background-position: 160% 0;
  }
  to {
    background-position: -60% 0;
  }
}
@media (prefers-reduced-motion: reduce) {
  .im-skeleton {
    animation: none;
  }
}
.im-fallback {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--dim);
}

/* 悬停浮出的玻璃感「预览」标 */
.im-veil {
  position: absolute;
  left: 50%;
  top: 50%;
  transform: translate(-50%, -50%) scale(0.92);
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 4px 10px;
  border-radius: 999px;
  background: rgba(15, 23, 42, 0.58);
  backdrop-filter: blur(4px);
  color: #fff;
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.4px;
  opacity: 0;
  pointer-events: none;
  transition: opacity 0.18s, transform 0.25s var(--ease-spring);
}
.im-thumb:hover .im-veil {
  opacity: 1;
  transform: translate(-50%, -50%) scale(1);
}

/* 张数徽标(末张右下角,LUMI 式) */
.im-count {
  position: absolute;
  right: 6px;
  bottom: 6px;
  padding: 1px 7px;
  border-radius: 999px;
  background: rgba(15, 23, 42, 0.55);
  backdrop-filter: blur(4px);
  color: rgba(255, 255, 255, 0.92);
  font-size: 10px;
  letter-spacing: 0.3px;
  pointer-events: none;
}

/* +N 兜底块 */
.im-more {
  display: flex;
  align-items: center;
  justify-content: center;
  border-style: dashed;
  color: var(--muted);
  font-size: 15px;
  font-weight: 600;
}
.im-more:hover {
  color: var(--primary);
  background: var(--primary-soft);
}
</style>
