<script setup lang="ts">
/**
 * 星图浮层:文件库 → 星河图谱(全屏沉浸)。自 FileCenter.vue 原样搬出。
 */
import { defineAsyncComponent } from "vue";
import { Orbit, X } from "@lucide/vue";
// 星图(星河图谱)按需加载:依赖 cytoscape(~562KB),只在点开「星图」时才拉。
const KnowledgeGraph = defineAsyncComponent(() => import("../KnowledgeGraph.vue"));

defineProps<{
  /** 归类档位刷新键:智能归类每完成一档就变化 → KnowledgeGraph 重挂载,星图原地升级。 */
  refreshKey: number;
}>();

const emit = defineEmits<{ (e: "close"): void }>();
</script>

<template>
  <div class="galaxy-overlay">
    <div class="galaxy-head">
      <span class="galaxy-title"><Orbit :size="16" :stroke-width="1.7" /> 我的资料 · 星图</span>
      <button class="galaxy-close" title="关闭" @click="emit('close')"><X :size="18" :stroke-width="2" /></button>
    </div>
    <!-- key 绑定归类档位:智能归类每完成一档(骨架/AI 初级/语义精修)就重挂载,星图「原地长准」。 -->
    <KnowledgeGraph :key="refreshKey" source="files" :embedded="true" />
  </div>
</template>

<style scoped>
/* 星图浮层 */
.galaxy-overlay {
  position: fixed;
  inset: 0;
  z-index: 70;
  display: flex;
  flex-direction: column;
  background: #04060e;
}
.galaxy-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px 18px;
  flex: none;
  z-index: 5;
}
.galaxy-title {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  color: #e8eefc;
  font-family: var(--serif);
  font-size: 15px;
  letter-spacing: 1px;
}
.galaxy-close {
  display: inline-flex;
  border: 1px solid rgba(150, 180, 255, 0.22);
  background: rgba(20, 26, 44, 0.6);
  color: #cfe0ff;
  border-radius: 9px;
  padding: 6px;
  cursor: pointer;
}
.galaxy-close:hover { background: rgba(40, 52, 84, 0.7); }
.galaxy-overlay :deep(.graph) { flex: 1; min-height: 0; }
</style>
