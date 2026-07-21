<script setup lang="ts">
/**
 * 分类树状图:顶层大类 + 可展开子类 + 右上角 minimap。
 * 自 FileCenter.vue 原样搬出;树的展开态是纯局部状态,下沉到本组件。
 */
import { ref, computed, reactive, nextTick } from "vue";
import { Sparkles, Wand2, Orbit, ChevronRight } from "@lucide/vue";
import OrbitSpinner from "../icons/OrbitSpinner.vue";
import type { FcCluster } from "../../tauri";

const props = defineProps<{
  clusters: FcCluster[];
  activeCluster: number | null;
  clustering: boolean;
  llmClustering: boolean;
  canCluster: boolean;
}>();

const emit = defineEmits<{
  (e: "view-whole-folder", c: FcCluster): void;
  (e: "smart-cluster"): void;
}>();

const topFolders = computed<FcCluster[]>(() => props.clusters.filter((c) => c.parent === 0));
function childrenOf(id: number): FcCluster[] {
  return props.clusters.filter((c) => c.parent === id);
}
function hasChildren(id: number): boolean {
  return props.clusters.some((c) => c.parent === id);
}

// ── 聚类树状图:展开的顶层节点集合(默认全部收起,只露顶层大类)──
const treeExpanded = reactive(new Set<number>());
function toggleTree(id: number) {
  if (treeExpanded.has(id)) treeExpanded.delete(id);
  else treeExpanded.add(id);
}
// 右上角 minimap 点某大类 → 展开该枝并滚动到位
const treeRef = ref<HTMLElement | null>(null);
function focusBranch(id: number) {
  treeExpanded.add(id);
  nextTick(() => {
    const el = treeRef.value?.querySelector(`[data-cl="${id}"]`) as HTMLElement | null;
    el?.scrollIntoView({ behavior: "smooth", block: "center" });
  });
}
</script>

<template>
  <div class="clustree glass">
    <div v-if="!clusters.length" class="star-empty">
      <Sparkles :size="26" :stroke-width="1.4" />
      <div>还没有归类</div>
      <div class="star-hint">
        点「智能归类」把相似文件归成一棵<b>分类树</b> —— 有向量索引走<b>语义</b>,没有就按<b>文件夹 / 名称</b>归类。
        每个大类点开能看它下面又分了哪些子类,点节点即可筛出该类的文件。
      </div>
      <button class="empty-cta" :disabled="clustering || llmClustering || !canCluster" @click="emit('smart-cluster')">
        <OrbitSpinner v-if="clustering || llmClustering" :size="15" />
        <Wand2 v-else :size="15" :stroke-width="1.8" /><span>{{ clustering || llmClustering ? "归类中…" : "智能归类" }}</span>
      </button>
    </div>
    <template v-else>
      <!-- 右上角缩略地图(看板边框内浮层) -->
      <div class="tree-map">
        <div class="tm-head"><Orbit :size="12" :stroke-width="1.7" /> 全景</div>
        <div class="tm-body">
          <button
            v-for="c in topFolders"
            :key="'tm' + c.id"
            class="tm-node"
            :class="{ on: activeCluster === c.id }"
            :style="{ '--c': c.color }"
            :title="c.label + ' · ' + c.size + ' 个'"
            @click="focusBranch(c.id)"
          >
            <span class="tm-dot" />
            <span class="tm-name">{{ c.label }}</span>
            <span v-if="hasChildren(c.id)" class="tm-sub">{{ childrenOf(c.id).length }}</span>
          </button>
        </div>
      </div>

      <!-- 树主体(默认全收起:只露顶层大类,点 ▸ 展开子类) -->
      <div ref="treeRef" class="tree-scroll">
        <div
          v-for="c in topFolders"
          :key="'t' + c.id"
          class="tree-branch"
          :data-cl="c.id"
        >
          <div class="tree-node top" :class="{ on: activeCluster === c.id }" :style="{ '--c': c.color }">
            <button class="tn-twist" :class="{ vis: hasChildren(c.id) }" :title="treeExpanded.has(c.id) ? '收起子类' : '展开子类'" @click="toggleTree(c.id)">
              <ChevronRight :size="14" :stroke-width="2" :class="{ open: treeExpanded.has(c.id) }" />
            </button>
            <button class="tn-body" :title="'查看「' + c.label + '」的全部文件'" @click="emit('view-whole-folder', c)">
              <span class="tn-dot" />
              <span class="tn-name">{{ c.label }}</span>
              <span v-if="hasChildren(c.id)" class="tn-kinds">{{ childrenOf(c.id).length }} 类</span>
              <span class="tn-count">{{ c.size }}</span>
            </button>
          </div>
          <!-- 子类 -->
          <div v-if="treeExpanded.has(c.id) && hasChildren(c.id)" class="tree-children">
            <div
              v-for="ch in childrenOf(c.id)"
              :key="'tc' + ch.id"
              class="tree-node sub"
              :class="{ on: activeCluster === ch.id }"
              :style="{ '--c': ch.color || c.color }"
            >
              <span class="tn-elbow" />
              <button class="tn-body" :title="'查看「' + ch.label + '」的全部文件'" @click="emit('view-whole-folder', ch)">
                <span class="tn-dot small" />
                <span class="tn-name">{{ ch.label }}</span>
                <span class="tn-count">{{ ch.size }}</span>
              </button>
            </div>
          </div>
        </div>
      </div>
    </template>
  </div>
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

/* ── 分类树状图 ── */
.clustree {
  /* 填满 .fc-body 剩余高度(其已是 flex 列);内部 .tree-scroll 绝对定位自滚 */
  flex: 1;
  min-height: 0;
  position: relative;
  overflow: hidden;
  background:
    radial-gradient(90% 80% at 28% 0%, color-mix(in srgb, var(--primary) 6%, transparent), transparent 62%),
    color-mix(in srgb, var(--panel) 50%, transparent);
}
.tree-scroll {
  position: absolute;
  inset: 0;
  overflow-y: auto;
  padding: 20px 22px 26px;
}
.tree-branch { position: relative; }

/* 节点(顶层 / 子类共用主体) */
.tree-node {
  --c: var(--muted);
  display: flex;
  align-items: center;
  gap: 2px;
  position: relative;
}
.tn-twist {
  flex: none;
  width: 22px;
  height: 38px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border: none;
  background: transparent;
  color: var(--dim);
  cursor: pointer;
  visibility: hidden;
}
.tn-twist.vis { visibility: visible; }
.tn-twist:hover { color: var(--text); }
.tn-twist :deep(svg) { transition: transform 0.18s; }
.tn-twist :deep(svg.open) { transform: rotate(90deg); }
.tn-body {
  flex: 1;
  min-width: 0;
  display: flex;
  align-items: center;
  gap: 10px;
  height: 40px;
  margin: 3px 0;
  padding: 0 14px;
  border: 1px solid var(--border-soft);
  border-radius: 11px;
  background: color-mix(in srgb, var(--panel) 60%, transparent);
  color: var(--text);
  cursor: pointer;
  text-align: left;
  transition: transform 0.16s, border-color 0.16s, background 0.16s, box-shadow 0.16s;
  -webkit-backdrop-filter: blur(8px);
  backdrop-filter: blur(8px);
}
.tree-node.top > .tn-body { height: 44px; }
.tn-body:hover {
  transform: translateX(2px);
  border-color: color-mix(in srgb, var(--c) 55%, transparent);
  box-shadow: 0 9px 22px -15px color-mix(in srgb, var(--c) 80%, transparent);
}
.tree-node.on > .tn-body {
  border-color: color-mix(in srgb, var(--c) 75%, transparent);
  background: color-mix(in srgb, var(--c) 13%, transparent);
}
.tn-dot {
  width: 12px;
  height: 12px;
  flex: none;
  border-radius: 50%;
  background: radial-gradient(circle at 35% 30%, color-mix(in srgb, var(--c) 92%, #fff 35%), var(--c) 70%);
  box-shadow:
    0 0 0 3px color-mix(in srgb, var(--c) 16%, transparent),
    0 0 8px color-mix(in srgb, var(--c) 55%, transparent);
}
.tn-dot.small {
  width: 9px;
  height: 9px;
  box-shadow: 0 0 0 2px color-mix(in srgb, var(--c) 16%, transparent);
}
.tn-name {
  flex: 1;
  min-width: 0;
  font-size: 13.5px;
  font-weight: 600;
  color: var(--text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.tree-node.sub .tn-name { font-size: 12.5px; font-weight: 500; color: var(--text-2); }
.tn-kinds {
  flex: none;
  font-size: 11px;
  color: var(--c);
  font-variant-numeric: tabular-nums;
}
.tn-count {
  flex: none;
  font-size: 11px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
  padding: 1px 9px;
  border-radius: 99px;
  background: color-mix(in srgb, var(--c) 13%, transparent);
}

/* 子类缩进 + L 形连接线 */
.tree-children {
  position: relative;
  margin-left: 32px;
  padding-left: 18px;
}
.tree-children::before {
  content: "";
  position: absolute;
  left: 0;
  top: -3px;
  bottom: 23px;
  width: 1.5px;
  background: color-mix(in srgb, var(--border-strong) 75%, transparent);
}
.tn-elbow {
  position: absolute;
  left: -18px;
  top: 50%;
  width: 16px;
  height: 1.5px;
  background: color-mix(in srgb, var(--border-strong) 75%, transparent);
}

/* 右上角缩略地图 minimap */
.tree-map {
  position: absolute;
  top: 12px;
  right: 12px;
  z-index: 4;
  width: 170px;
  max-height: calc(100% - 24px);
  display: flex;
  flex-direction: column;
  border: 1px solid var(--border-soft);
  border-radius: 12px;
  background: color-mix(in srgb, var(--panel) 84%, transparent);
  -webkit-backdrop-filter: blur(16px) saturate(1.4);
  backdrop-filter: blur(16px) saturate(1.4);
  box-shadow: 0 14px 34px -18px rgba(0, 0, 0, 0.55);
  overflow: hidden;
}
.tm-head {
  display: flex;
  align-items: center;
  gap: 5px;
  padding: 8px 11px 6px;
  font-size: 11px;
  letter-spacing: 0.5px;
  color: var(--muted);
  border-bottom: 1px solid var(--hairline);
}
.tm-body {
  overflow-y: auto;
  padding: 5px;
  display: flex;
  flex-direction: column;
  gap: 1px;
}
.tm-node {
  --c: var(--muted);
  display: flex;
  align-items: center;
  gap: 7px;
  padding: 4px 7px;
  border: none;
  background: transparent;
  border-radius: 7px;
  cursor: pointer;
  text-align: left;
  transition: background 0.14s;
}
.tm-node:hover { background: var(--selection-bg); }
.tm-node.on { background: color-mix(in srgb, var(--c) 16%, transparent); }
.tm-dot {
  width: 7px;
  height: 7px;
  flex: none;
  border-radius: 50%;
  background: var(--c);
  box-shadow: 0 0 5px color-mix(in srgb, var(--c) 70%, transparent);
}
.tm-name {
  flex: 1;
  min-width: 0;
  font-size: 11px;
  color: var(--text-2);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.tm-node.on .tm-name { color: var(--text); }
.tm-sub {
  flex: none;
  font-size: 10px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}
.star-empty {
  position: absolute;
  inset: 0;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  color: var(--muted);
  text-align: center;
  padding: 0 40px;
}
.star-hint { font-size: 12px; max-width: 360px; line-height: 1.7; }

/* 空态主按钮(与文件中心「空库」CTA 同款) */
.empty-cta {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  margin-top: 6px;
  height: 38px;
  padding: 0 20px;
  border: none;
  border-radius: 12px;
  background: var(--btn-solid-bg);
  color: var(--btn-solid-text);
  font-size: 13px;
  cursor: pointer;
  transition: opacity 0.16s, transform 0.16s;
}
.empty-cta:hover:not(:disabled) { transform: translateY(-1px); }
.empty-cta:disabled { opacity: 0.6; cursor: default; }
</style>
