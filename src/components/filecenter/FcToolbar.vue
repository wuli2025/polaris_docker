<script setup lang="ts">
/**
 * 文件中心工具条:视图切换 seg · 搜索 · 排序 · 功能按钮(向导/星图/盘点/归类/索引/AI 名称/盘管理)。
 * 自 FileCenter.vue 原样搬出;状态全在父层,这里只负责展示 + 事件上抛。
 */
import {
  Search,
  LayoutGrid,
  List as ListIcon,
  Orbit,
  Sparkles,
  Radar,
  FolderSearch,
  X,
  Wand2,
  ArrowDownWideNarrow,
  FolderTree,
  Network,
  Server,
} from "@lucide/vue";
import OrbitSpinner from "../icons/OrbitSpinner.vue";
import type { FileOverview } from "../../tauri";
import type { RemoteSource } from "../../features/interconnect/remoteSources";
import type { ViewKind } from "./shared";

defineProps<{
  overview: FileOverview | null;
  remoteSources: RemoteSource[];
  remoteSelId: string | null;
  sort: "recent" | "name" | "size" | "kind";
  semBusy: boolean;
  scanning: boolean;
  pickerBusy: boolean;
  clustering: boolean;
  llmClustering: boolean;
  building: boolean;
  llmTitling: boolean;
}>();

const view = defineModel<ViewKind>("view", { required: true });
const searchText = defineModel<string>("search", { required: true });

const emit = defineEmits<{
  (e: "pick-remote", s: RemoteSource): void;
  (e: "search-input"): void;
  (e: "clear-search"): void;
  (e: "semantic"): void;
  (e: "set-sort", s: "recent" | "name" | "size" | "kind"): void;
  (e: "open-wizard"): void;
  (e: "open-galaxy"): void;
  (e: "open-picker"): void;
  (e: "smart-cluster"): void;
  (e: "build-index"): void;
  (e: "titles"): void;
  (e: "open-nas"): void;
}>();

function clearSearch() {
  searchText.value = "";
  emit("clear-search");
}
</script>

<template>
  <div class="fc-toolbar glass">
    <div class="seg">
      <button class="seg-btn" :class="{ on: view === 'gallery' }" @click="view = 'gallery'" title="网格画廊">
        <LayoutGrid :size="15" :stroke-width="1.7" />
      </button>
      <button class="seg-btn" :class="{ on: view === 'clusters' }" @click="view = 'clusters'" title="分类树状图">
        <FolderTree :size="15" :stroke-width="1.7" />
      </button>
      <button class="seg-btn" :class="{ on: view === 'list' }" @click="view = 'list'" title="列表">
        <ListIcon :size="15" :stroke-width="1.7" />
      </button>
      <button class="seg-btn core-seg" :class="{ on: view === 'core' }" @click="view = 'core'" title="核心层 · 知识体系">
        <Network :size="15" :stroke-width="1.7" />
        <span class="seg-lab">核心层</span>
      </button>
      <!-- 远程源:每台经 iroh 隧道接入的 NAS/主机一颗 chip -->
      <button
        v-for="rs in remoteSources"
        :key="rs.id"
        class="seg-btn core-seg remote-seg"
        :class="{ on: view === 'remote' && remoteSelId === rs.id }"
        :title="`远程源 · ${rs.name}（127.0.0.1:${rs.port}）`"
        @click="emit('pick-remote', rs)"
      >
        <Server :size="15" :stroke-width="1.7" />
        <span class="seg-lab">{{ rs.name }}</span>
      </button>
    </div>

    <div v-show="view !== 'core'" class="search">
      <Search :size="15" :stroke-width="1.8" class="search-ic" />
      <input
        v-model="searchText"
        placeholder="搜索文件名 · 回车做语义检索"
        @input="emit('search-input')"
        @keydown.enter="emit('semantic')"
      />
      <button v-if="searchText" class="search-clear" @click="clearSearch">
        <X :size="13" :stroke-width="2" />
      </button>
      <button class="sem-btn" :disabled="semBusy || !searchText.trim()" title="语义检索(grep ∥ 向量)" @click="emit('semantic')">
        <OrbitSpinner v-if="semBusy" :size="14" />
        <Radar v-else :size="14" :stroke-width="1.8" />
        <span>语义</span>
      </button>
    </div>

    <div v-show="view !== 'core'" class="sortwrap">
      <ArrowDownWideNarrow :size="14" :stroke-width="1.7" class="sort-ic" />
      <select :value="sort" @change="emit('set-sort', ($event.target as HTMLSelectElement).value as any)">
        <option value="recent">最近修改</option>
        <option value="name">名称</option>
        <option value="size">大小</option>
        <option value="kind">类型</option>
      </select>
    </div>

    <div v-show="view !== 'core'" class="actions">
      <button
        class="tool-btn wizard"
        title="让 AI 更懂你:盘点 → 智能归类 → 知识图谱 → 建索引 → 进对话,一条龙引导"
        @click="emit('open-wizard')"
      >
        <Sparkles :size="14" :stroke-width="1.8" />
        <span>智能向导</span>
      </button>
      <button
        class="tool-btn"
        :disabled="!overview?.totalFiles"
        title="星图:把你的文件库渲染成星河图谱(归过类更好看)——一眼看清你都有些什么"
        @click="emit('open-galaxy')"
      >
        <Orbit :size="14" :stroke-width="1.8" />
        <span>星图</span>
      </button>
      <button
        class="tool-btn"
        :disabled="scanning || pickerBusy"
        title="盘点:先扫一眼文件夹结构,勾选要盘点的目录(可选知识库之外的盘符/文件夹),再建库"
        @click="emit('open-picker')"
      >
        <OrbitSpinner v-if="scanning || pickerBusy" :size="14" />
        <FolderSearch v-else :size="14" :stroke-width="1.8" />
        <span>{{ scanning ? "盘点中" : "盘点" }}</span>
      </button>
      <button
        class="tool-btn accent"
        :disabled="clustering || llmClustering || !overview?.totalFiles"
        :title="overview?.hasEmbedProvider
          ? '智能归类:先秒级按结构出星图骨架 → AI 读懂你的资料、起亲切名字并理清关系 → 后台把全部资料向量化后再按语义精修一次(全程后台,可关页面)'
          : '智能归类:先秒级按结构出星图骨架 → AI 读懂并起名;到设置页配硅基 key(免费)后,还会在后台按内容语义精修一次'"
        @click="emit('smart-cluster')"
      >
        <OrbitSpinner v-if="clustering || llmClustering" :size="14" />
        <Wand2 v-else :size="14" :stroke-width="1.8" />
        <span>{{ clustering || llmClustering ? "归类中" : "智能归类" }}</span>
      </button>
      <button
        class="tool-btn"
        :disabled="building || !overview?.totalFiles"
        :title="overview?.hasEmbedProvider
          ? '为文本建/续建向量索引(硅基 BGE-M3,后台跑),建好后能按「意思」搜文件'
          : '建索引需要嵌入 key:点这里到设置页配硅基 key(免费),全文索引则照常后台建'"
        @click="emit('build-index')"
      >
        <OrbitSpinner v-if="building" :size="14" />
        <Radar v-else :size="14" :stroke-width="1.8" />
        <span>{{ building ? "建索引中" : overview && overview.embeddedFiles > 0 ? "续建索引" : "建索引" }}</span>
      </button>
      <button
        class="tool-btn ai"
        :disabled="llmTitling || !overview?.totalFiles"
        title="用大模型给乱码/杂乱的文件名起可读的中文标题(只改显示,不改磁盘文件名)"
        @click="emit('titles')"
      >
        <OrbitSpinner v-if="llmTitling" :size="14" />
        <Sparkles v-else :size="14" :stroke-width="1.8" />
        <span>{{ llmTitling ? "整理中" : "AI 整理名称" }}</span>
      </button>
      <button
        class="tool-btn"
        title="盘管理:记住你登陆过的 NAS(主机/共享/账号),一键映射成网络盘,挂上后就能被「盘点」扫到"
        @click="emit('open-nas')"
      >
        <Server :size="14" :stroke-width="1.8" />
        <span>盘管理</span>
      </button>
    </div>
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

/* ── 工具条 ── */
.fc-toolbar {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 9px 14px;
  flex-wrap: wrap;
}
.seg {
  display: flex;
  gap: 2px;
  padding: 3px;
  background: var(--selection-bg);
  border-radius: 11px;
}
.seg-btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 34px;
  height: 28px;
  border: none;
  background: transparent;
  color: var(--muted);
  border-radius: 8px;
  cursor: pointer;
  transition: all 0.16s;
}
.seg-btn:hover { color: var(--text); }
.seg-btn.on {
  background: var(--panel);
  color: var(--primary);
  box-shadow: var(--shadow-sm);
}
.search {
  flex: 1;
  min-width: 220px;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 0 8px 0 12px;
  height: 34px;
  background: color-mix(in srgb, var(--bg) 60%, transparent);
  border: 1px solid var(--border-soft);
  border-radius: 11px;
  transition: border-color 0.16s, box-shadow 0.16s;
}
.search:focus-within {
  border-color: color-mix(in srgb, var(--primary) 50%, transparent);
  box-shadow: 0 0 0 3px color-mix(in srgb, var(--primary) 12%, transparent);
}
.search-ic { color: var(--muted); flex: none; }
.search input {
  flex: 1;
  min-width: 0;
  border: none;
  background: transparent;
  color: var(--text);
  font-size: 13px;
  outline: none;
}
.search-clear {
  display: inline-flex;
  border: none;
  background: transparent;
  color: var(--dim);
  cursor: pointer;
  padding: 3px;
  border-radius: 6px;
}
.search-clear:hover { color: var(--text); background: var(--selection-bg); }
.sem-btn {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  height: 26px;
  padding: 0 10px;
  border: none;
  border-radius: 8px;
  background: color-mix(in srgb, var(--primary) 14%, transparent);
  color: var(--primary);
  font-size: 12px;
  cursor: pointer;
  flex: none;
}
.sem-btn:hover:not(:disabled) { background: color-mix(in srgb, var(--primary) 22%, transparent); }
.sem-btn:disabled { opacity: 0.5; cursor: default; }
.sortwrap {
  display: flex;
  align-items: center;
  gap: 5px;
  color: var(--muted);
}
.sortwrap select {
  border: 1px solid var(--border-soft);
  background: color-mix(in srgb, var(--bg) 60%, transparent);
  color: var(--text);
  font-size: 12.5px;
  border-radius: 9px;
  padding: 6px 8px;
  outline: none;
  cursor: pointer;
}
.actions { display: flex; flex-wrap: wrap; gap: 8px; }
.tool-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 32px;
  padding: 0 13px;
  border: 1px solid var(--border-soft);
  background: color-mix(in srgb, var(--panel) 70%, transparent);
  color: var(--text-2);
  border-radius: 10px;
  font-size: 12.5px;
  cursor: pointer;
  transition: all 0.16s;
}
.tool-btn:hover:not(:disabled) {
  border-color: color-mix(in srgb, var(--primary) 45%, transparent);
  color: var(--text);
}
.tool-btn.accent {
  border-color: color-mix(in srgb, var(--gold) 45%, transparent);
  color: var(--gold);
}
.tool-btn.accent:hover:not(:disabled) {
  background: color-mix(in srgb, var(--gold) 12%, transparent);
}
.tool-btn.ai {
  border-color: color-mix(in srgb, #8b6cff 50%, transparent);
  color: #8b6cff;
  background: color-mix(in srgb, #8b6cff 8%, transparent);
}
.tool-btn.ai:hover:not(:disabled) {
  background: color-mix(in srgb, #8b6cff 16%, transparent);
}
.tool-btn:disabled { opacity: 0.5; cursor: default; }
.tool-btn.wizard {
  border-color: var(--primary);
  background: var(--primary);
  color: #fff;
}
.tool-btn.wizard:hover:not(:disabled) { filter: brightness(1.08); color: #fff; }

/* ── 核心层 / 远程源 seg 加宽样式 ── */
.seg-btn.core-seg { width: auto; padding: 0 11px; gap: 6px; }
.seg-lab { font-size: 12px; font-weight: 600; }
.remote-seg .seg-lab { max-width: 120px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
