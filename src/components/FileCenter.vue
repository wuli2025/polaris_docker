<script setup lang="ts">
/**
 * 文件中心 —— 知识库内的可视化文件库(《文件中心-PRD》落地)。
 *
 * 三视图:网格画廊(缩略图/首帧/类型图标占大位)· 聚类星图(语义簇)· 列表。
 * 琉璃质感 + 苹果式透明:毛玻璃面板、accent 光环、悬浮升起、缩略图懒加载。
 * 数据全部复用检索枢纽 fable.db,聚类复用已存向量(零新增嵌入),缩略图磁盘缓存。
 */
import { ref, reactive, computed, onMounted, onBeforeUnmount, nextTick, watch } from "vue";
import {
  Search,
  LayoutGrid,
  List as ListIcon,
  Orbit,
  Sparkles,
  Radar,
  FolderSearch,
  ExternalLink,
  FolderOpen,
  X,
  Wand2,
  LoaderCircle,
  ArrowDownWideNarrow,
} from "@lucide/vue";
import {
  files as fc,
  artifacts as artifactsApi,
  listen,
  type FileOverview,
  type FileCard,
  type FcCluster,
} from "../tauri";

// ───────────────────────── 状态 ─────────────────────────
type ViewKind = "gallery" | "clusters" | "list";
const view = ref<ViewKind>("gallery");
const overview = ref<FileOverview | null>(null);
const cards = ref<FileCard[]>([]);
const page = ref(0);
const total = ref(0);
const loading = ref(false);
const exhausted = ref(false);

const activeKind = ref<string | null>(null);
const activeCluster = ref<number | null>(null);
const sort = ref<"recent" | "name" | "size" | "kind">("recent");
const searchText = ref("");

const scanning = ref(false);
const scanMsg = ref("");
const clustering = ref(false);
const clusterMsg = ref("");

// 语义检索结果(独立于网格的一条结果带)
interface SemHit {
  path: string;
  abspath: string;
  snippet: string;
  score: number;
  lanes: string[];
}
const semHits = ref<SemHit[]>([]);
const semBusy = ref(false);
const semActive = ref(false);

// 选中详情
const selected = ref<FileCard | null>(null);
const detailGist = ref("");
const detailThumb = ref<string | null>(null);

// 缩略图缓存:abspath → dataURL('' = 已尝试但无图)
const thumbCache = reactive(new Map<string, string>());
const thumbPending = new Set<string>();

let thumbObs: IntersectionObserver | null = null;
let moreObs: IntersectionObserver | null = null;
const sentinel = ref<HTMLElement | null>(null);
const elCard = new WeakMap<Element, FileCard>();
let unlistenScan: (() => void) | null = null;
let searchTimer: ReturnType<typeof setTimeout> | null = null;

// ───────────────────────── 配色 / 字形 ─────────────────────────
const KIND_COLOR: Record<string, string> = {
  text: "#5fa8e6",
  doc: "#8b6cff",
  image: "#6fcf97",
  audio: "#e0a24b",
  video: "#e0736b",
  archive: "#93a0b4",
  other: "#8a8f98",
};
const KIND_LABEL: Record<string, string> = {
  text: "文本",
  doc: "文档",
  image: "图片",
  audio: "音频",
  video: "视频",
  archive: "压缩包",
  other: "其它",
};
const CODE_EXTS = new Set([
  "rs", "py", "js", "ts", "tsx", "jsx", "mjs", "vue", "go", "java", "c", "cpp", "h", "hpp",
  "rb", "php", "json", "jsonl", "html", "htm", "css", "sh", "ps1", "bat", "sql", "toml",
]);
const TEXTY_EXTS = new Set(["md", "txt", "rst", "org", "tex", "log", "yaml", "yml", "xml", "ini", "cfg", "srt", "vtt"]);

const clusterColor = computed<Record<number, FcCluster>>(() => {
  const m: Record<number, FcCluster> = {};
  for (const c of overview.value?.clusters ?? []) m[c.id] = c;
  return m;
});

function accentFor(card: FileCard): string {
  if (card.clusterId > 0 && clusterColor.value[card.clusterId]) {
    return clusterColor.value[card.clusterId].color;
  }
  return KIND_COLOR[card.kind] ?? KIND_COLOR.other;
}

function glyphFor(card: FileCard): string {
  const k = card.kind;
  const e = card.ext.toLowerCase();
  if (k === "image") return "image";
  if (k === "video") return "video";
  if (k === "audio") return "audio";
  if (k === "archive") return "archive";
  if (e === "pdf") return "pdf";
  if (["xls", "xlsx", "csv", "tsv", "ods"].includes(e)) return "sheet";
  if (["ppt", "pptx"].includes(e)) return "slide";
  if (["doc", "docx"].includes(e)) return "doc";
  if (CODE_EXTS.has(e)) return "code";
  if (TEXTY_EXTS.has(e) || k === "text") return "text";
  if (k === "doc") return "doc";
  return "other";
}

// 自研科技感线性字形(thin 单线 + accent 高光,不落俗套)
const GLYPHS: Record<string, string> = {
  text: `<path class="soft" d="M30 6 L38 14 H30 Z"/><path d="M16 6 H30 L38 14 V42 H16 Z"/><path d="M30 6 V14 H38"/><path d="M21 23 H33 M21 29 H33 M21 35 H28"/>`,
  doc: `<path class="soft" d="M30 6 L38 14 H30 Z"/><path d="M16 6 H30 L38 14 V42 H16 Z"/><path d="M30 6 V14 H38"/><path d="M21 24 H33 M21 30 H33 M21 36 H29"/>`,
  code: `<rect class="soft" x="8" y="11" width="32" height="26" rx="5"/><path d="M18 19 L12 24 L18 29"/><path d="M30 19 L36 24 L30 29"/><path class="acc" d="M27 16 L21 32"/>`,
  pdf: `<path class="soft" d="M30 6 L38 14 H30 Z"/><path d="M16 6 H30 L38 14 V42 H16 Z"/><path d="M30 6 V14 H38"/><rect class="fill" x="15" y="29" width="20" height="8" rx="2.5"/>`,
  sheet: `<rect class="soft" x="9" y="10" width="30" height="9" rx="3.5"/><rect x="9" y="10" width="30" height="28" rx="3.5"/><path d="M9 19 H39 M9 28.5 H39 M19 10 V38 M29 10 V38"/>`,
  slide: `<rect class="soft" x="8" y="10" width="32" height="22" rx="3.5"/><rect x="8" y="10" width="32" height="22" rx="3.5"/><path class="acc" d="M15 26 V22 M21 26 V17 M27 26 V20 M33 26 V14"/><path d="M19 32 L17 38 M29 32 L31 38 M16 38 H32"/>`,
  image: `<rect x="8" y="10" width="32" height="28" rx="3.5"/><circle cx="18" cy="19" r="3"/><path class="soft" d="M9 33 L18 25 L25 31 L31 24 L39 32 V35 a3 3 0 0 1-3 3 H12 a3 3 0 0 1-3-3 Z"/><path d="M9 33 L18 25 L25 31 L31 24 L39 32"/>`,
  video: `<rect class="soft" x="8" y="11" width="32" height="26" rx="5"/><rect x="8" y="11" width="32" height="26" rx="5"/><path class="fill" d="M21 18.5 L31 24 L21 29.5 Z"/>`,
  audio: `<path d="M11 22 V26" stroke-width="2.4"/><path d="M16 18 V30" stroke-width="2.4"/><path class="acc" d="M21 12 V36" stroke-width="2.4"/><path d="M26 16 V32" stroke-width="2.4"/><path class="acc" d="M31 13 V35" stroke-width="2.4"/><path d="M36 20 V28" stroke-width="2.4"/>`,
  archive: `<path class="soft" d="M24 9 L38 16 L24 23 L10 16 Z"/><path d="M24 9 L38 16 V32 L24 39 L10 32 V16 Z"/><path d="M10 16 L24 23 L38 16 M24 23 V39"/>`,
  other: `<path class="soft" d="M24 8 L38 16 V32 L24 40 L10 32 V16 Z"/><path d="M24 8 L38 16 V32 L24 40 L10 32 V16 Z"/><circle cx="24" cy="24" r="4"/>`,
};

// ───────────────────────── 加载 ─────────────────────────
async function loadOverview() {
  try {
    overview.value = await fc.overview(null);
  } catch {
    overview.value = null;
  }
}

async function loadGrid(reset = false) {
  if (loading.value) return;
  if (reset) {
    page.value = 0;
    cards.value = [];
    exhausted.value = false;
  }
  if (exhausted.value) return;
  loading.value = true;
  try {
    const res = await fc.grid({
      root: null,
      clusterId: activeCluster.value,
      kind: activeKind.value,
      sort: sort.value,
      query: searchText.value.trim() || null,
      page: page.value,
      pageSize: 60,
    });
    total.value = res.total;
    cards.value = reset ? res.items : cards.value.concat(res.items);
    if (res.items.length < res.pageSize || cards.value.length >= res.total) {
      exhausted.value = true;
    } else {
      page.value += 1;
    }
    warmVisible();
  } catch {
    /* 静默:空库时网格为空 */
  } finally {
    loading.value = false;
  }
}

function applyFilters() {
  semActive.value = false;
  loadGrid(true);
}

// 过滤切换
function pickKind(k: string | null) {
  activeKind.value = activeKind.value === k ? null : k;
  applyFilters();
}
function pickCluster(id: number | null) {
  activeCluster.value = activeCluster.value === id ? null : id;
  if (activeCluster.value !== null) view.value = "gallery";
  applyFilters();
}
function setSort(s: typeof sort.value) {
  sort.value = s;
  applyFilters();
}
function onSearchInput() {
  if (searchTimer) clearTimeout(searchTimer);
  searchTimer = setTimeout(() => applyFilters(), 240);
}

// ───────────────────────── 盘点 / 聚类 ─────────────────────────
async function doScan() {
  if (scanning.value) return;
  scanning.value = true;
  scanMsg.value = "正在盘点磁盘…";
  try {
    if (!unlistenScan) {
      unlistenScan = await listen<{ kind: string; files?: number; message?: string }>(
        "fable:inventory",
        (p) => {
          if (p.kind === "progress") scanMsg.value = `已盘点 ${p.files ?? 0} 个文件…`;
          else if (p.kind === "done") {
            scanMsg.value = `盘点完成 · ${p.files ?? 0} 个文件`;
            scanning.value = false;
            loadOverview();
            loadGrid(true);
          } else if (p.kind === "error") {
            scanMsg.value = `盘点失败:${p.message ?? ""}`;
            scanning.value = false;
          }
        },
      );
    }
    await fc.inventoryStart(null);
  } catch (e: any) {
    scanMsg.value = `盘点失败:${e?.message ?? e}`;
    scanning.value = false;
  }
}

async function doCluster() {
  if (clustering.value) return;
  clustering.value = true;
  clusterMsg.value = "正在按语义归类(复用已有向量,零新增嵌入)…";
  try {
    const r = await fc.clusterBuild(null);
    clusterMsg.value = r.note;
    await loadOverview();
    await loadGrid(true);
    if ((overview.value?.clusters.length ?? 0) > 0) view.value = "clusters";
  } catch (e: any) {
    clusterMsg.value = `归类失败:${e?.message ?? e}`;
  } finally {
    clustering.value = false;
  }
}

// ───────────────────────── 语义检索 ─────────────────────────
async function runSemantic() {
  const q = searchText.value.trim();
  if (!q) {
    semActive.value = false;
    semHits.value = [];
    return;
  }
  semBusy.value = true;
  semActive.value = true;
  try {
    const r = await fc.search(q, 24, "hybrid");
    // 去重到「文件」粒度(同文件多 chunk 命中只留最高分一条)
    const byPath = new Map<string, SemHit>();
    for (const h of r.hits) {
      const ex = byPath.get(h.path);
      if (!ex || h.score > ex.score) {
        byPath.set(h.path, {
          path: h.path,
          abspath: h.abspath,
          snippet: h.snippet,
          score: h.score,
          lanes: h.lanes,
        });
      }
    }
    semHits.value = Array.from(byPath.values()).slice(0, 16);
  } catch (e: any) {
    semHits.value = [];
    clusterMsg.value = `检索失败:${e?.message ?? e}`;
  } finally {
    semBusy.value = false;
  }
}
function clearSemantic() {
  semActive.value = false;
  semHits.value = [];
}

// ───────────────────────── 缩略图懒加载 ─────────────────────────
async function fetchThumb(card: FileCard) {
  if (!card.thumbable) return;
  if (thumbCache.has(card.abspath) || thumbPending.has(card.abspath)) return;
  thumbPending.add(card.abspath);
  try {
    const url = await fc.thumb(card.abspath, 360);
    thumbCache.set(card.abspath, url ?? "");
  } catch {
    thumbCache.set(card.abspath, "");
  } finally {
    thumbPending.delete(card.abspath);
  }
}

function setupObservers() {
  thumbObs = new IntersectionObserver(
    (entries) => {
      for (const en of entries) {
        if (en.isIntersecting) {
          const card = elCard.get(en.target);
          if (card) fetchThumb(card);
          thumbObs?.unobserve(en.target);
        }
      }
    },
    { rootMargin: "300px" },
  );
  moreObs = new IntersectionObserver(
    (entries) => {
      if (
        entries.some((e) => e.isIntersecting) &&
        (view.value === "gallery" || view.value === "list") &&
        !semActive.value
      ) {
        loadGrid(false);
      }
    },
    { rootMargin: "600px" },
  );
}

function registerTile(el: Element | null, card: FileCard) {
  if (!el || !thumbObs) return;
  elCard.set(el, card);
  if (card.thumbable && !thumbCache.has(card.abspath)) thumbObs.observe(el);
}

// 视口内可见的卡片批量预热(进入网格/翻页后调用)
function warmVisible() {
  const slice = cards.value.filter((c) => c.thumbable && !thumbCache.has(c.abspath)).slice(0, 24);
  if (slice.length) fc.warmThumbs(slice.map((c) => c.abspath), 360).catch(() => {});
}

watch(sentinel, (el) => {
  if (el && moreObs) moreObs.observe(el);
});

// ───────────────────────── 详情 ─────────────────────────
async function openDetail(card: FileCard) {
  selected.value = card;
  detailGist.value = "";
  detailThumb.value = thumbCache.get(card.abspath) || null;
  // 速览(按需 + 缓存)
  fc.gist(card.abspath).then((g) => {
    if (selected.value?.abspath === card.abspath) detailGist.value = g;
  });
  if (card.thumbable && !detailThumb.value) {
    fc.thumb(card.abspath, 640).then((u) => {
      if (selected.value?.abspath === card.abspath) detailThumb.value = u;
    });
  }
}
function closeDetail() {
  selected.value = null;
}
async function openExternal(card: FileCard) {
  await openPath(card.abspath);
}
async function openPath(abspath: string) {
  try {
    await artifactsApi.openExternal(abspath);
  } catch (e: any) {
    clusterMsg.value = `打开失败:${e?.message ?? e}`;
  }
}
async function revealCard(card: FileCard) {
  try {
    await artifactsApi.reveal(card.abspath);
  } catch (e: any) {
    clusterMsg.value = `定位失败:${e?.message ?? e}`;
  }
}

// ───────────────────────── 星图布局(phyllotaxis) ─────────────────────────
const starOrbs = computed(() => {
  const cl = (overview.value?.clusters ?? []).slice(0, 40);
  if (!cl.length) return [];
  const maxSize = Math.max(...cl.map((c) => c.size), 1);
  const golden = Math.PI * (3 - Math.sqrt(5));
  return cl.map((c, i) => {
    const r = 40 * Math.sqrt(i + 0.6);
    const a = i * golden;
    const x = 50 + (r * Math.cos(a)) / 6.2;
    const y = 50 + (r * Math.sin(a)) / 6.2;
    const d = 34 + 70 * Math.sqrt(c.size / maxSize);
    return { ...c, x, y, d };
  });
});

// ───────────────────────── 辅助 ─────────────────────────
function fmtTime(sec: number): string {
  if (!sec) return "";
  const d = new Date(sec * 1000);
  const now = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  const hm = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  if (d.toDateString() === now.toDateString()) return `今天 ${hm}`;
  return `${d.getFullYear() === now.getFullYear() ? "" : d.getFullYear() + "/"}${pad(d.getMonth() + 1)}/${pad(d.getDate())} ${hm}`;
}
function fmtBytes(b: number): string {
  const u = ["B", "KB", "MB", "GB", "TB"];
  let v = b,
    i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return i === 0 ? `${b} B` : `${v.toFixed(1)} ${u[i]}`;
}
function nameOf(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}
const hasFiles = computed(() => (overview.value?.totalFiles ?? 0) > 0);
const headerStats = computed(() => {
  const o = overview.value;
  if (!o) return [];
  return [
    { label: "文件", value: o.totalFiles.toLocaleString() },
    { label: "总量", value: fmtBytes(o.totalBytes) },
    { label: "语义簇", value: String(o.clusters.length) },
    { label: "已嵌入", value: `${o.embeddedFiles}/${o.textFiles}` },
  ];
});

onMounted(async () => {
  setupObservers();
  await loadOverview();
  await loadGrid(true);
  await nextTick();
});

onBeforeUnmount(() => {
  thumbObs?.disconnect();
  moreObs?.disconnect();
  if (unlistenScan) unlistenScan();
});
</script>

<template>
  <div class="fc">
    <!-- 顶部琉璃横幅 -->
    <div class="fc-banner glass">
      <div class="fc-title-wrap">
        <div class="fc-title"><Orbit :size="17" :stroke-width="1.6" /> 文件中心</div>
        <div class="fc-sub">同类数据自动归在一起 · 缩略图 / 首帧 / 类型图标 · 智能检索</div>
      </div>
      <div class="fc-stats">
        <div v-for="s in headerStats" :key="s.label" class="stat">
          <div class="stat-val">{{ s.value }}</div>
          <div class="stat-lab">{{ s.label }}</div>
        </div>
      </div>
    </div>

    <!-- 工具条 -->
    <div class="fc-toolbar glass">
      <div class="seg">
        <button class="seg-btn" :class="{ on: view === 'gallery' }" @click="view = 'gallery'" title="网格画廊">
          <LayoutGrid :size="15" :stroke-width="1.7" />
        </button>
        <button class="seg-btn" :class="{ on: view === 'clusters' }" @click="view = 'clusters'" title="聚类星图">
          <Orbit :size="15" :stroke-width="1.7" />
        </button>
        <button class="seg-btn" :class="{ on: view === 'list' }" @click="view = 'list'" title="列表">
          <ListIcon :size="15" :stroke-width="1.7" />
        </button>
      </div>

      <div class="search">
        <Search :size="15" :stroke-width="1.8" class="search-ic" />
        <input
          v-model="searchText"
          placeholder="搜索文件名 · 回车做语义检索"
          @input="onSearchInput"
          @keydown.enter="runSemantic"
        />
        <button v-if="searchText" class="search-clear" @click="searchText = ''; clearSemantic(); applyFilters()">
          <X :size="13" :stroke-width="2" />
        </button>
        <button class="sem-btn" :disabled="semBusy || !searchText.trim()" title="语义检索(grep ∥ 向量)" @click="runSemantic">
          <LoaderCircle v-if="semBusy" :size="14" class="spin" />
          <Radar v-else :size="14" :stroke-width="1.8" />
          <span>语义</span>
        </button>
      </div>

      <div class="sortwrap">
        <ArrowDownWideNarrow :size="14" :stroke-width="1.7" class="sort-ic" />
        <select :value="sort" @change="setSort(($event.target as HTMLSelectElement).value as any)">
          <option value="recent">最近修改</option>
          <option value="name">名称</option>
          <option value="size">大小</option>
          <option value="kind">类型</option>
        </select>
      </div>

      <div class="actions">
        <button class="tool-btn" :disabled="scanning" title="扫描磁盘建立文件索引" @click="doScan">
          <LoaderCircle v-if="scanning" :size="14" class="spin" />
          <FolderSearch v-else :size="14" :stroke-width="1.8" />
          <span>{{ scanning ? "盘点中" : "盘点" }}</span>
        </button>
        <button class="tool-btn accent" :disabled="clustering || !overview?.embeddedFiles" title="按语义把相似文件归类(复用已有向量)" @click="doCluster">
          <LoaderCircle v-if="clustering" :size="14" class="spin" />
          <Wand2 v-else :size="14" :stroke-width="1.8" />
          <span>{{ clustering ? "归类中" : "智能归类" }}</span>
        </button>
      </div>
    </div>

    <div v-if="scanMsg || clusterMsg" class="fc-note">{{ clusterMsg || scanMsg }}</div>

    <!-- 过滤胶囊 -->
    <div v-if="hasFiles" class="fc-chips">
      <button class="chip" :class="{ on: activeKind === null && activeCluster === null }" @click="activeKind = null; activeCluster = null; applyFilters()">
        全部
      </button>
      <button
        v-for="kc in overview?.byKind ?? []"
        :key="kc.kind"
        class="chip"
        :class="{ on: activeKind === kc.kind }"
        :style="{ '--chip': KIND_COLOR[kc.kind] || KIND_COLOR.other }"
        @click="pickKind(kc.kind)"
      >
        <span class="chip-dot" />{{ KIND_LABEL[kc.kind] || kc.kind }}
        <span class="chip-n">{{ kc.count }}</span>
      </button>
      <span v-if="(overview?.clusters.length ?? 0) > 0" class="chip-div" />
      <button
        v-for="c in (overview?.clusters ?? []).slice(0, 12)"
        :key="'c' + c.id"
        class="chip cluster"
        :class="{ on: activeCluster === c.id }"
        :style="{ '--chip': c.color }"
        @click="pickCluster(c.id)"
      >
        <span class="chip-dot" />{{ c.label }}
        <span class="chip-n">{{ c.size }}</span>
      </button>
    </div>

    <!-- 空库引导 -->
    <div v-if="!hasFiles" class="fc-empty glass">
      <div class="empty-orb"><FolderSearch :size="30" :stroke-width="1.3" /></div>
      <div class="empty-title">文件中心还是空的</div>
      <div class="empty-sub">点「盘点」扫描知识库根目录,把磁盘上的文件建成可视化文件库;<br />已嵌入文本可再点「智能归类」把相似数据自动放在一起。</div>
      <button class="empty-cta" :disabled="scanning" @click="doScan">
        <LoaderCircle v-if="scanning" :size="15" class="spin" />
        <FolderSearch v-else :size="15" :stroke-width="1.8" />
        <span>{{ scanning ? "盘点中…" : "立即盘点" }}</span>
      </button>
    </div>

    <!-- 内容区 -->
    <div v-else class="fc-body">
      <!-- 语义检索结果带 -->
      <div v-if="semActive" class="sem-strip">
        <div class="sem-head">
          <Radar :size="14" :stroke-width="1.8" />
          <span>语义检索:「{{ searchText }}」</span>
          <button class="sem-close" @click="clearSemantic"><X :size="13" :stroke-width="2" /> 收起</button>
        </div>
        <div v-if="semBusy" class="sem-loading"><LoaderCircle :size="16" class="spin" /> 检索中…</div>
        <div v-else-if="!semHits.length" class="sem-empty">没有命中。试试更短的关键词,或先在「检索枢纽」构建向量索引。</div>
        <div v-else class="sem-list">
          <div v-for="h in semHits" :key="h.path" class="sem-row" @click="openPath(h.abspath)">
            <svg viewBox="0 0 48 48" class="glyph sem-glyph" v-html="GLYPHS.text" />
            <div class="sem-main">
              <div class="sem-name">{{ nameOf(h.path) }}</div>
              <div class="sem-snip">{{ h.snippet }}</div>
            </div>
            <div class="sem-score">
              <span v-for="l in h.lanes" :key="l" class="lane" :class="l">{{ l === 'vector' ? '向量' : 'grep' }}</span>
            </div>
          </div>
        </div>
      </div>

      <!-- 画廊 -->
      <div v-show="view === 'gallery'" class="gallery">
        <div
          v-for="card in cards"
          :key="card.id"
          class="tile glass"
          :style="{ '--accent': accentFor(card) }"
          :ref="(el) => registerTile(el as Element, card)"
          @click="openDetail(card)"
        >
          <div class="thumb">
            <img
              v-if="card.thumbable && thumbCache.get(card.abspath)"
              :src="thumbCache.get(card.abspath)"
              class="thumb-img"
              loading="lazy"
              alt=""
            />
            <div v-else class="thumb-glyph">
              <div class="glyph-halo" />
              <svg viewBox="0 0 48 48" class="glyph" v-html="GLYPHS[glyphFor(card)]" />
              <div v-if="card.thumbable && !thumbCache.has(card.abspath)" class="shimmer" />
            </div>
            <span class="ext-badge">{{ card.ext || card.kind }}</span>
            <span v-if="card.kind === 'video'" class="play-badge">▶</span>
          </div>
          <div class="tile-meta">
            <div class="tile-name" :title="card.name">{{ card.name }}</div>
            <div class="tile-sub">
              <span v-if="card.clusterId > 0 && clusterColor[card.clusterId]" class="tile-cluster" :style="{ color: clusterColor[card.clusterId].color }">
                {{ clusterColor[card.clusterId].label }}
              </span>
              <span v-else class="tile-kind">{{ KIND_LABEL[card.kind] || card.kind }}</span>
              <span class="tile-size">{{ card.sizeH }}</span>
            </div>
          </div>
        </div>
        <div v-if="loading" class="grid-loading"><LoaderCircle :size="18" class="spin" /> 加载中…</div>
        <div v-if="!cards.length && !loading" class="grid-empty">该筛选下没有文件</div>
      </div>

      <!-- 星图 -->
      <div v-show="view === 'clusters'" class="starmap glass">
        <div v-if="!overview?.clusters.length" class="star-empty">
          <Sparkles :size="26" :stroke-width="1.4" />
          <div>还没有语义簇</div>
          <div class="star-hint">点工具条「智能归类」,把已嵌入的文本按相似度归成一张星图(复用已有向量,不花新钱)。</div>
          <button class="empty-cta" :disabled="clustering || !overview?.embeddedFiles" @click="doCluster">
            <Wand2 :size="15" :stroke-width="1.8" /><span>智能归类</span>
          </button>
        </div>
        <div v-else class="star-field">
          <div
            v-for="o in starOrbs"
            :key="o.id"
            class="orb"
            :class="{ on: activeCluster === o.id }"
            :style="{ left: o.x + '%', top: o.y + '%', '--d': o.d + 'px', '--c': o.color }"
            @click="pickCluster(o.id)"
          >
            <div class="orb-core" />
            <div class="orb-label">
              <span class="orb-name">{{ o.label }}</span>
              <span class="orb-n">{{ o.size }}</span>
            </div>
          </div>
        </div>
      </div>

      <!-- 列表 -->
      <div v-show="view === 'list'" class="listview">
        <div class="lv-head">
          <span class="lv-c-name">名称</span>
          <span class="lv-c-cluster">归类</span>
          <span class="lv-c-kind">类型</span>
          <span class="lv-c-size">大小</span>
          <span class="lv-c-time">修改</span>
        </div>
        <div
          v-for="card in cards"
          :key="card.id"
          class="lv-row"
          :style="{ '--accent': accentFor(card) }"
          @click="openDetail(card)"
        >
          <span class="lv-c-name">
            <svg viewBox="0 0 48 48" class="glyph lv-glyph" v-html="GLYPHS[glyphFor(card)]" />
            <span class="lv-name" :title="card.name">{{ card.name }}</span>
          </span>
          <span class="lv-c-cluster">
            <span v-if="card.clusterId > 0 && clusterColor[card.clusterId]" class="lv-tag" :style="{ '--c': clusterColor[card.clusterId].color }">
              {{ clusterColor[card.clusterId].label }}
            </span>
            <span v-else class="lv-dim">—</span>
          </span>
          <span class="lv-c-kind">{{ KIND_LABEL[card.kind] || card.kind }}</span>
          <span class="lv-c-size">{{ card.sizeH }}</span>
          <span class="lv-c-time">{{ fmtTime(card.mtime) }}</span>
        </div>
        <div v-if="loading" class="grid-loading"><LoaderCircle :size="18" class="spin" /> 加载中…</div>
      </div>

      <div ref="sentinel" class="sentinel" />
    </div>

    <!-- 详情抽屉 -->
    <transition name="drawer">
      <div v-if="selected" class="detail glass" :style="{ '--accent': accentFor(selected) }">
        <button class="detail-close" @click="closeDetail"><X :size="16" :stroke-width="2" /></button>
        <div class="detail-hero">
          <img v-if="detailThumb" :src="detailThumb" class="detail-img" alt="" />
          <div v-else class="detail-glyph">
            <div class="glyph-halo big" />
            <svg viewBox="0 0 48 48" class="glyph" v-html="GLYPHS[glyphFor(selected)]" />
          </div>
        </div>
        <div class="detail-name">{{ selected.name }}</div>
        <div class="detail-path">{{ selected.path }}</div>
        <div class="detail-tags">
          <span class="dtag">{{ KIND_LABEL[selected.kind] || selected.kind }}</span>
          <span class="dtag">{{ selected.sizeH }}</span>
          <span v-if="selected.clusterId > 0 && clusterColor[selected.clusterId]" class="dtag cluster" :style="{ '--c': clusterColor[selected.clusterId].color }">
            {{ clusterColor[selected.clusterId].label }}
          </span>
          <span class="dtag dim">{{ fmtTime(selected.mtime) }}</span>
        </div>
        <div class="detail-gist">
          <div class="gist-head"><Sparkles :size="13" :stroke-width="1.7" /> 内容速览</div>
          <div v-if="detailGist" class="gist-body">{{ detailGist }}</div>
          <div v-else class="gist-body loading"><LoaderCircle :size="13" class="spin" /> 生成中…</div>
        </div>
        <div class="detail-actions">
          <button class="detail-btn primary" @click="openExternal(selected)"><ExternalLink :size="14" :stroke-width="1.8" /> 打开</button>
          <button class="detail-btn" @click="revealCard(selected)"><FolderOpen :size="14" :stroke-width="1.8" /> 在文件夹中显示</button>
        </div>
      </div>
    </transition>
    <transition name="fade">
      <div v-if="selected" class="detail-scrim" @click="closeDetail" />
    </transition>
  </div>
</template>

<style scoped>
.fc {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  gap: 12px;
  padding: 4px 4px 0;
  position: relative;
}

/* ── 琉璃通用 ── */
.glass {
  background: color-mix(in srgb, var(--panel) 68%, transparent);
  -webkit-backdrop-filter: blur(22px) saturate(1.5);
  backdrop-filter: blur(22px) saturate(1.5);
  border: 1px solid var(--border-soft);
  border-radius: 16px;
}

/* ── 横幅 ── */
.fc-banner {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 16px 22px;
  position: relative;
  overflow: hidden;
}
.fc-banner::before {
  content: "";
  position: absolute;
  inset: 0;
  background:
    radial-gradient(120% 140% at 0% 0%, color-mix(in srgb, var(--primary) 16%, transparent), transparent 55%),
    radial-gradient(120% 140% at 100% 100%, color-mix(in srgb, var(--gold) 14%, transparent), transparent 55%);
  pointer-events: none;
}
.fc-title-wrap { position: relative; }
.fc-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-family: var(--serif);
  font-size: 18px;
  letter-spacing: 1.5px;
  color: var(--ink);
}
.fc-sub {
  margin-top: 5px;
  font-size: 12px;
  color: var(--muted);
  letter-spacing: 0.3px;
}
.fc-stats {
  display: flex;
  gap: 26px;
  position: relative;
}
.stat { text-align: right; }
.stat-val {
  font-size: 19px;
  font-weight: 650;
  color: var(--text);
  font-variant-numeric: tabular-nums;
}
.stat-lab {
  font-size: 11px;
  color: var(--muted);
  margin-top: 2px;
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
.actions { display: flex; gap: 8px; }
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
.tool-btn:disabled { opacity: 0.5; cursor: default; }

.fc-note {
  font-size: 12px;
  color: var(--muted);
  padding: 0 8px;
  margin-top: -4px;
}

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
.chip-div {
  width: 1px;
  height: 16px;
  background: var(--border);
  margin: 0 2px;
}

/* ── 主体 ── */
.fc-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 4px 6px 28px;
}

/* 语义带 */
.sem-strip {
  background: color-mix(in srgb, var(--primary) 7%, var(--panel));
  border: 1px solid color-mix(in srgb, var(--primary) 22%, transparent);
  border-radius: 14px;
  padding: 12px 14px;
  margin-bottom: 16px;
}
.sem-head {
  display: flex;
  align-items: center;
  gap: 7px;
  font-size: 12.5px;
  color: var(--primary);
  margin-bottom: 8px;
}
.sem-close {
  margin-left: auto;
  display: inline-flex;
  align-items: center;
  gap: 3px;
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 11.5px;
  cursor: pointer;
}
.sem-loading, .sem-empty { font-size: 12.5px; color: var(--muted); display: flex; align-items: center; gap: 6px; }
.sem-list { display: flex; flex-direction: column; gap: 2px; }
.sem-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 7px 8px;
  border-radius: 9px;
  cursor: pointer;
  transition: background 0.14s;
}
.sem-row:hover { background: var(--selection-bg); }
.sem-glyph { width: 24px; height: 24px; flex: none; color: var(--primary); }
.sem-main { flex: 1; min-width: 0; }
.sem-name { font-size: 13px; color: var(--text); font-weight: 550; }
.sem-snip {
  font-size: 11.5px;
  color: var(--muted);
  margin-top: 1px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.sem-score { display: flex; gap: 4px; flex: none; }
.lane {
  font-size: 10px;
  padding: 1px 6px;
  border-radius: 6px;
  background: var(--selection-bg);
  color: var(--muted);
}
.lane.vector { background: color-mix(in srgb, var(--primary) 16%, transparent); color: var(--primary); }

/* ── 画廊 ── */
.gallery {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(184px, 1fr));
  gap: 16px;
}
.tile {
  border-radius: 16px;
  overflow: hidden;
  cursor: pointer;
  transition: transform 0.2s cubic-bezier(0.2, 0.7, 0.3, 1), box-shadow 0.2s, border-color 0.2s;
  border: 1px solid var(--border-soft);
}
.tile:hover {
  transform: translateY(-4px);
  border-color: color-mix(in srgb, var(--accent) 55%, transparent);
  box-shadow:
    0 14px 34px -14px color-mix(in srgb, var(--accent) 55%, transparent),
    var(--shadow-lg);
}
.thumb {
  position: relative;
  aspect-ratio: 4 / 3;
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
.glyph-halo.big { width: 150px; height: 150px; }
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
.tile-meta { padding: 10px 12px 12px; }
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
.sentinel { grid-column: 1 / -1; height: 1px; }
.grid-loading, .grid-empty {
  grid-column: 1 / -1;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  padding: 22px;
  color: var(--muted);
  font-size: 12.5px;
}

/* ── 星图 ── */
.starmap {
  height: calc(100vh - 320px);
  min-height: 420px;
  position: relative;
  overflow: hidden;
  background:
    radial-gradient(80% 80% at 50% 40%, color-mix(in srgb, var(--primary) 8%, transparent), transparent 70%),
    color-mix(in srgb, var(--panel) 50%, transparent);
}
.star-field {
  position: absolute;
  inset: 0;
}
.orb {
  position: absolute;
  width: var(--d);
  height: var(--d);
  transform: translate(-50%, -50%);
  cursor: pointer;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  transition: transform 0.25s;
  animation: float 6s ease-in-out infinite;
}
.orb:nth-child(even) { animation-duration: 7.5s; }
.orb:nth-child(3n) { animation-duration: 9s; }
@keyframes float {
  0%, 100% { margin-top: 0; }
  50% { margin-top: -8px; }
}
.orb-core {
  position: absolute;
  inset: 0;
  border-radius: 50%;
  background: radial-gradient(circle at 38% 32%, color-mix(in srgb, var(--c) 90%, #fff 30%), var(--c) 60%, color-mix(in srgb, var(--c) 40%, transparent));
  box-shadow:
    0 0 0 1px color-mix(in srgb, var(--c) 50%, transparent),
    0 8px 28px -6px color-mix(in srgb, var(--c) 75%, transparent),
    inset 0 0 22px color-mix(in srgb, #fff 18%, transparent);
  transition: box-shadow 0.25s, transform 0.25s;
}
.orb:hover { transform: translate(-50%, -50%) scale(1.08); z-index: 5; }
.orb:hover .orb-core {
  box-shadow:
    0 0 0 2px color-mix(in srgb, var(--c) 80%, transparent),
    0 14px 40px -6px color-mix(in srgb, var(--c) 90%, transparent),
    inset 0 0 26px color-mix(in srgb, #fff 28%, transparent);
}
.orb.on .orb-core {
  box-shadow:
    0 0 0 3px var(--c),
    0 14px 44px -4px color-mix(in srgb, var(--c) 95%, transparent);
}
.orb-label {
  position: relative;
  text-align: center;
  pointer-events: none;
  padding: 0 4px;
  max-width: calc(var(--d) + 30px);
}
.orb-name {
  display: block;
  font-size: 11.5px;
  font-weight: 600;
  color: #fff;
  text-shadow: 0 1px 4px rgba(0, 0, 0, 0.5);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.orb-n {
  font-size: 10px;
  color: rgba(255, 255, 255, 0.85);
  text-shadow: 0 1px 3px rgba(0, 0, 0, 0.5);
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

/* ── 列表 ── */
.listview { display: flex; flex-direction: column; }
.lv-head, .lv-row {
  display: grid;
  grid-template-columns: 1fr 160px 80px 90px 130px;
  gap: 10px;
  align-items: center;
  padding: 9px 12px;
}
.lv-head {
  font-size: 11px;
  color: var(--muted);
  letter-spacing: 0.5px;
  border-bottom: 1px solid var(--hairline);
  position: sticky;
  top: 0;
  background: color-mix(in srgb, var(--bg) 80%, transparent);
  -webkit-backdrop-filter: blur(8px);
  backdrop-filter: blur(8px);
  z-index: 2;
}
.lv-row {
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

/* ── 空库 ── */
.fc-empty {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 12px;
  text-align: center;
  margin: 0 2px 12px;
  padding: 40px;
}
.empty-orb {
  width: 76px;
  height: 76px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--primary);
  background: radial-gradient(circle, color-mix(in srgb, var(--primary) 18%, transparent), transparent 70%);
}
.empty-title { font-family: var(--serif); font-size: 17px; color: var(--text); letter-spacing: 1px; }
.empty-sub { font-size: 12.5px; color: var(--muted); line-height: 1.8; }
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
.spin { animation: spin 0.9s linear infinite; }
@keyframes spin { to { transform: rotate(360deg); } }
.drawer-enter-active, .drawer-leave-active { transition: transform 0.26s cubic-bezier(0.2, 0.7, 0.3, 1), opacity 0.26s; }
.drawer-enter-from, .drawer-leave-to { transform: translateX(20px); opacity: 0; }
.fade-enter-active, .fade-leave-active { transition: opacity 0.26s; }
.fade-enter-from, .fade-leave-to { opacity: 0; }
</style>
