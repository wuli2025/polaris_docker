<script setup lang="ts">
/**
 * 文件中心筛选面板:语义文件夹(两级下钻)+ 按类型筛选 + 按语言归类。
 * 自 FileCenter.vue 原样搬出;筛选选中态在父层,展开/收起态在本组件内持久化。
 */
import { ref, computed, watch } from "vue";
import { ChevronDown, ChevronRight, ChevronLeft, Folder, SlidersHorizontal, Languages, Layers } from "@lucide/vue";
import type { FileOverview, FcCluster } from "../../tauri";
import { KIND_COLOR, KIND_LABEL, langColor, loadFc, saveFc } from "./shared";

const props = defineProps<{
  overview: FileOverview | null;
  hasFiles: boolean;
  folderPath: number[];
  activeCluster: number | null;
  activeKind: string | null;
  activeLang: string | null;
}>();

const emit = defineEmits<{
  (e: "open-folder", c: FcCluster): void;
  (e: "view-whole-folder", c: FcCluster): void;
  (e: "folder-home"): void;
  (e: "pick-kind", k: string | null): void;
  (e: "pick-lang", l: string | null): void;
}>();

// 折叠状态(均持久化记住用户选择):语义分类 · 类型筛选 · 语言归类。
const foldersOpen = ref(loadFc("polaris.fc.folders", true));
const kindOpen = ref(loadFc("polaris.fc.kinds", false));
const langOpen = ref(loadFc("polaris.fc.langs", true));
watch(foldersOpen, (v) => saveFc("polaris.fc.folders", v));
watch(kindOpen, (v) => saveFc("polaris.fc.kinds", v));
watch(langOpen, (v) => saveFc("polaris.fc.langs", v));

// ───────────────────────── 语义文件夹层级(两级) ─────────────────────────
const allClusters = computed<FcCluster[]>(() => props.overview?.clusters ?? []);
const topFolders = computed<FcCluster[]>(() => allClusters.value.filter((c) => c.parent === 0));
function childrenOf(id: number): FcCluster[] {
  return allClusters.value.filter((c) => c.parent === id);
}
function hasChildren(id: number): boolean {
  return allClusters.value.some((c) => c.parent === id);
}
const hasClusters = computed(() => allClusters.value.length > 0);
// 当前文件夹层要显示的卡片:根 → 顶层主题;进入某主题 → 其子主题
const folderCards = computed<FcCluster[]>(() =>
  props.folderPath.length ? childrenOf(props.folderPath[0]) : topFolders.value,
);
const currentFolder = computed<FcCluster | null>(() =>
  props.folderPath.length ? allClusters.value.find((c) => c.id === props.folderPath[0]) ?? null : null,
);
</script>

<template>
  <!-- 语义文件夹:同主题归一格,点开看子主题,再点筛选画廊 -->
  <div v-if="hasFiles && hasClusters" class="fc-folders">
    <div class="fld-bar">
      <button class="fld-toggle" :class="{ open: foldersOpen }" :title="foldersOpen ? '收起分类' : '展开分类'" @click="foldersOpen = !foldersOpen">
        <ChevronDown :size="14" :stroke-width="1.8" :class="{ flip: !foldersOpen }" />
        <span>分类</span>
        <span class="fld-toggle-n">{{ folderCards.length }}</span>
      </button>
      <button class="crumb" :class="{ on: folderPath.length === 0 && activeCluster === null }" @click="emit('folder-home')">
        <Layers :size="13" :stroke-width="1.8" /> 全部主题
      </button>
      <template v-if="currentFolder">
        <ChevronRight :size="13" :stroke-width="2" class="crumb-sep" />
        <span class="crumb cur" :style="{ '--c': currentFolder.color }">
          <span class="crumb-dot" />{{ currentFolder.label }}
        </span>
        <button class="crumb-all" @click="emit('view-whole-folder', currentFolder)">看全部 {{ currentFolder.size }} 个</button>
      </template>
    </div>
    <div v-show="foldersOpen" class="fld-grid">
      <button
        v-if="folderPath.length"
        class="fld-card back"
        @click="emit('folder-home')"
      >
        <span class="fld-ic"><ChevronLeft :size="18" :stroke-width="1.8" /></span>
        <span class="fld-main"><span class="fld-name">返回全部主题</span></span>
      </button>
      <button
        v-for="c in folderCards"
        :key="'f' + c.id"
        class="fld-card"
        :class="{ on: activeCluster === c.id, drill: c.parent === 0 && hasChildren(c.id) }"
        :style="{ '--c': c.color }"
        :title="c.label"
        @click="emit('open-folder', c)"
      >
        <span class="fld-ic">
          <Folder :size="19" :stroke-width="1.5" />
          <span v-if="hasChildren(c.id)" class="fld-stack" />
        </span>
        <span class="fld-main">
          <span class="fld-name">{{ c.label }}</span>
          <span class="fld-meta">
            {{ c.size }} 个<template v-if="hasChildren(c.id)"> · {{ childrenOf(c.id).length }} 类</template>
          </span>
        </span>
        <ChevronRight v-if="c.parent === 0 && hasChildren(c.id)" :size="15" :stroke-width="1.8" class="fld-arrow" />
      </button>
    </div>
  </div>
  <div v-else-if="hasFiles" class="fld-hint">
    <Layers :size="14" :stroke-width="1.7" />
    <span>还没按主题归类。点上方 <b>「智能归类」</b>,文件会按内容主题自动归进文件夹(配了 API key 走语义 AI 归类,没配则按文件夹 / 名称离线归)。</span>
  </div>

  <!-- 按类型筛选(可收起,默认收起,腾出下方空间) -->
  <div v-if="hasFiles" class="fc-kinds">
    <button class="kinds-toggle" :class="{ open: kindOpen }" @click="kindOpen = !kindOpen">
      <SlidersHorizontal :size="13" :stroke-width="1.8" />
      <span>按类型筛选</span>
      <span v-if="activeKind" class="kinds-active" :style="{ '--c': KIND_COLOR[activeKind] || KIND_COLOR.other }">
        <span class="chip-dot" />{{ KIND_LABEL[activeKind] || activeKind }}
      </span>
      <ChevronDown :size="14" :stroke-width="1.8" class="kinds-chev" :class="{ flip: kindOpen }" />
    </button>
    <div v-if="kindOpen" class="fc-chips">
      <button class="chip" :class="{ on: activeKind === null }" @click="emit('pick-kind', null)">
        全部类型
      </button>
      <button
        v-for="kc in overview?.byKind ?? []"
        :key="kc.kind"
        class="chip"
        :class="{ on: activeKind === kc.kind }"
        :style="{ '--chip': KIND_COLOR[kc.kind] || KIND_COLOR.other }"
        @click="emit('pick-kind', kc.kind)"
      >
        <span class="chip-dot" />{{ KIND_LABEL[kc.kind] || kc.kind }}
        <span class="chip-n">{{ kc.count }}</span>
      </button>
    </div>
  </div>

  <!-- 按语言归类(编程语言 / 自然语言 / 媒体大类)—— 比「按类型」更细,按语言分门别类 -->
  <div v-if="hasFiles" class="fc-kinds">
    <button class="kinds-toggle" :class="{ open: langOpen }" @click="langOpen = !langOpen">
      <Languages :size="13" :stroke-width="1.8" />
      <span>按语言归类</span>
      <span v-if="activeLang" class="kinds-active" :style="{ '--c': langColor(activeLang) }">
        <span class="chip-dot" />{{ activeLang }}
      </span>
      <ChevronDown :size="14" :stroke-width="1.8" class="kinds-chev" :class="{ flip: langOpen }" />
    </button>
    <div v-if="langOpen" class="fc-chips">
      <button class="chip" :class="{ on: activeLang === null }" @click="emit('pick-lang', null)">
        全部语言
      </button>
      <button
        v-for="lc in overview?.byLang ?? []"
        :key="lc.lang"
        class="chip"
        :class="{ on: activeLang === lc.lang }"
        :style="{ '--chip': langColor(lc.lang) }"
        @click="emit('pick-lang', lc.lang)"
      >
        <span class="chip-dot" />{{ lc.lang }}
        <span class="chip-n">{{ lc.count }}</span>
      </button>
    </div>
  </div>
</template>

<style scoped>
/* ── 语义文件夹 ── */
.fc-folders {
  margin: 0 2px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.fld-bar {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
  padding: 0 4px;
}
.fld-toggle {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  height: 26px;
  padding: 0 10px;
  border: 1px solid var(--border-soft);
  background: color-mix(in srgb, var(--panel) 55%, transparent);
  color: var(--text-2);
  border-radius: 8px;
  font-size: 12px;
  cursor: pointer;
  transition: color 0.16s, border-color 0.16s;
}
.fld-toggle:hover, .fld-toggle.open { color: var(--text); border-color: var(--border-strong); }
.fld-toggle :deep(svg) { transition: transform 0.2s; color: var(--dim); }
.fld-toggle .flip { transform: rotate(-90deg); }
.fld-toggle-n {
  font-size: 11px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}
.crumb {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  height: 26px;
  padding: 0 10px;
  border: 1px solid var(--border-soft);
  background: color-mix(in srgb, var(--panel) 55%, transparent);
  color: var(--text-2);
  border-radius: 8px;
  font-size: 12px;
  cursor: pointer;
}
.crumb:hover { color: var(--text); }
.crumb.on { color: var(--text); border-color: var(--border-strong); }
.crumb.cur {
  --c: var(--muted);
  cursor: default;
  color: var(--text);
  border-color: color-mix(in srgb, var(--c) 45%, transparent);
  background: color-mix(in srgb, var(--c) 12%, transparent);
}
.crumb-dot { width: 7px; height: 7px; border-radius: 50%; background: var(--c); }
.crumb-sep { color: var(--dim); flex: none; }
.crumb-all {
  margin-left: 2px;
  border: none;
  background: transparent;
  color: var(--primary);
  font-size: 12px;
  cursor: pointer;
  padding: 0 4px;
}
.crumb-all:hover { text-decoration: underline; }
.fld-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(190px, 1fr));
  gap: 10px;
}
.fld-card {
  --c: var(--muted);
  display: flex;
  align-items: center;
  gap: 11px;
  padding: 11px 12px;
  border: 1px solid var(--border-soft);
  background: color-mix(in srgb, var(--panel) 60%, transparent);
  border-radius: 13px;
  cursor: pointer;
  text-align: left;
  transition: transform 0.16s, border-color 0.16s, box-shadow 0.16s, background 0.16s;
  -webkit-backdrop-filter: blur(8px);
  backdrop-filter: blur(8px);
}
.fld-card:hover {
  transform: translateY(-2px);
  border-color: color-mix(in srgb, var(--c) 55%, transparent);
  box-shadow: 0 10px 24px -14px color-mix(in srgb, var(--c) 70%, transparent);
}
.fld-card.on {
  border-color: color-mix(in srgb, var(--c) 75%, transparent);
  background: color-mix(in srgb, var(--c) 13%, transparent);
}
.fld-card.back { --c: var(--muted); color: var(--muted); }
.fld-ic {
  position: relative;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 38px;
  height: 38px;
  flex: none;
  border-radius: 10px;
  color: var(--c);
  background: color-mix(in srgb, var(--c) 16%, transparent);
}
.fld-stack {
  position: absolute;
  right: 5px;
  bottom: 5px;
  width: 8px;
  height: 8px;
  border-radius: 2px;
  background: var(--c);
  box-shadow: -3px -3px 0 -1px color-mix(in srgb, var(--c) 45%, transparent);
}
.fld-main { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; }
.fld-name {
  font-size: 13px;
  font-weight: 600;
  color: var(--text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.fld-meta { font-size: 11px; color: var(--muted); font-variant-numeric: tabular-nums; }
.fld-arrow { color: var(--dim); flex: none; }
.fld-card:hover .fld-arrow { color: var(--c); }
.fld-hint {
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 0 4px;
  padding: 9px 14px;
  border-radius: 12px;
  border: 1px dashed var(--border-soft);
  color: var(--muted);
  font-size: 12.5px;
  line-height: 1.6;
}
.fld-hint b { color: var(--text); }

/* ── 类型筛选(可收起) ── */
.fc-kinds { margin: 0 2px; }
.kinds-toggle {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  height: 30px;
  padding: 0 12px;
  border: 1px solid var(--border-soft);
  background: color-mix(in srgb, var(--panel) 55%, transparent);
  color: var(--text-2);
  border-radius: 9px;
  font-size: 12.5px;
  cursor: pointer;
  transition: color 0.16s, border-color 0.16s;
}
.kinds-toggle:hover, .kinds-toggle.open { color: var(--text); border-color: var(--border-strong); }
.kinds-active {
  --c: var(--muted);
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 1px 8px;
  border-radius: 99px;
  background: color-mix(in srgb, var(--c) 14%, transparent);
  color: var(--text);
  font-size: 11.5px;
}
.kinds-chev { color: var(--dim); transition: transform 0.2s; }
.kinds-chev.flip { transform: rotate(180deg); }
.fc-kinds .fc-chips { margin-top: 8px; padding: 0 4px; }

/* ── 过滤胶囊 ── */
.fc-chips {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  padding: 0 6px;
}
.chip {
  --chip: var(--muted);
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 28px;
  padding: 0 11px;
  border: 1px solid var(--border-soft);
  background: color-mix(in srgb, var(--panel) 55%, transparent);
  color: var(--text-2);
  border-radius: 99px;
  font-size: 12px;
  cursor: pointer;
  transition: all 0.16s;
  -webkit-backdrop-filter: blur(8px);
  backdrop-filter: blur(8px);
}
.chip:hover { border-color: color-mix(in srgb, var(--chip) 55%, transparent); color: var(--text); }
.chip.on {
  border-color: color-mix(in srgb, var(--chip) 70%, transparent);
  background: color-mix(in srgb, var(--chip) 15%, transparent);
  color: var(--text);
}
.chip-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--chip);
  box-shadow: 0 0 6px color-mix(in srgb, var(--chip) 70%, transparent);
}
.chip-n {
  font-size: 11px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}
</style>
