<script setup lang="ts">
/**
 * 文件中心 —— 知识库内的可视化文件库(《文件中心-PRD》落地)。
 *
 * 三视图:网格画廊(缩略图/首帧/类型图标占大位)· 聚类星图(语义簇)· 列表。
 * 琉璃质感 + 苹果式透明:毛玻璃面板、accent 光环、悬浮升起、缩略图懒加载。
 * 数据全部复用检索枢纽 fable.db,聚类复用已存向量(零新增嵌入),缩略图磁盘缓存。
 */
import { ref, reactive, computed, onMounted, onBeforeUnmount, nextTick, watch, defineAsyncComponent } from "vue";
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
  KeyRound,
  Brain,
  FileText,
  LoaderCircle,
  ArrowDownWideNarrow,
  ChevronDown,
  ChevronRight,
  ChevronLeft,
  Folder,
  SlidersHorizontal,
  Languages,
  Layers,
  FolderTree,
  Check,
  RotateCcw,
  Network,
  ShieldCheck,
  BookOpen,
} from "@lucide/vue";
import {
  files as fc,
  artifacts as artifactsApi,
  invoke,
  type FileOverview,
  type FileCard,
  type FcCluster,
  type FolderNode,
  type ScanRootInfo,
} from "../tauri";
import { useAppStore } from "../stores/app";
import { useWizardStore } from "../stores/wizard";
import { useFileTasksStore } from "../stores/fileTasks";
// 星图(星河图谱)按需加载:依赖 cytoscape(~562KB),只在点开「星图」时才拉。
const KnowledgeGraph = defineAsyncComponent(() => import("./KnowledgeGraph.vue"));

const app = useAppStore();
const wiz = useWizardStore();

// ── 星图:直接把文件库渲染成星河图谱(复用 KnowledgeGraph,source=files),随时可看 ──
const galaxyOpen = ref(false);

// ── 智能向导「让 AI 更懂你」:盘点→归类→图谱→索引→进对话 一条龙 ──
// 向导本体常驻 App.vue(扫描/归类跑着时可转后台、切视图都不丢),这里只负责开它 + 关掉后刷新。
const WIZ_SEEN_KEY = "polaris.fc.wizard.v1";
function openWizard() {
  try {
    localStorage.setItem(WIZ_SEEN_KEY, "1");
  } catch {
    /* ignore */
  }
  wiz.openWizard();
}
// 向导收起(转后台/完成)后刷新文件中心,把新盘点/新归类的结果显示出来。
watch(
  () => wiz.open,
  (o, prev) => {
    if (!o && prev) {
      loadOverview();
      loadGrid(true);
    }
  },
);

// ───────────────────────── 状态 ─────────────────────────
type ViewKind = "gallery" | "clusters" | "list" | "core";
const view = ref<ViewKind>("gallery");

// ── 核心层(知识体系):个人=知识网/聚类,企业=Schema-Guided 抽出的实体关系三元组 ──
interface OntoTypeLite { id: string; name: string; hint: string }
interface OntoSchema {
  id: string;
  name: string;
  industry: string;
  source: string;
  desc: string;
  entities: OntoTypeLite[];
  relations: OntoTypeLite[];
  triples: number;
}
interface OntoTriple {
  subject: string;
  subjectType: string;
  predicate: string;
  object: string;
  objectType: string;
  confidence: number;
  sourceFile: string;
}
const ontoSchemas = ref<OntoSchema[]>([]);
const ontoTotal = ref(0);
const ontoLoading = ref(false);
const openSchema = ref<string>("");
const schemaTriples = ref<OntoTriple[]>([]);
const triplesLoading = ref(false);
async function loadCore() {
  ontoLoading.value = true;
  try {
    const ov: any = await invoke("ontology_overview");
    ontoSchemas.value = ov.schemas ?? [];
    ontoTotal.value = ov.totalTriples ?? 0;
  } catch {
    ontoSchemas.value = [];
    ontoTotal.value = 0;
  } finally {
    ontoLoading.value = false;
  }
}
async function toggleSchema(id: string) {
  if (openSchema.value === id) {
    openSchema.value = "";
    return;
  }
  openSchema.value = id;
  triplesLoading.value = true;
  schemaTriples.value = [];
  try {
    schemaTriples.value = await invoke<OntoTriple[]>("ontology_triples", { schemaId: id, limit: 200 });
  } catch {
    schemaTriples.value = [];
  } finally {
    triplesLoading.value = false;
  }
}
const ontoSchemasWithData = computed(() => ontoSchemas.value.filter((s) => s.triples > 0));
function openFullKb() {
  app.setView("wiki");
}
watch(view, (v) => {
  if (v === "core" && !ontoSchemas.value.length) void loadCore();
});
const overview = ref<FileOverview | null>(null);
const cards = ref<FileCard[]>([]);
const page = ref(0);
const total = ref(0);
const loading = ref(false);
const exhausted = ref(false);

const activeKind = ref<string | null>(null);
const activeLang = ref<string | null>(null);
const activeCluster = ref<number | null>(null);
const sort = ref<"recent" | "name" | "size" | "kind">("recent");
const searchText = ref("");

// 长任务(盘点/建索引/智能归类/AI 整理名称)状态全部托管到全局 fileTasks store。
// 关键:这些活儿的真身是后端后台线程 + 全局事件,旧实现把状态/监听锁在本组件里,
// 一切走文件中心(组件卸载)就退订 + 清零、看着像停了。改读 store 后 →
// 切走切回甚至从没打开过文件中心,进度都在后台持续累积、回来即见(全局任务中心浮层亦据此显示)。
const tasks = useFileTasksStore();
const scanning = computed(() => tasks.running.inventory);
const scanMsg = computed(() => tasks.detail.inventory);
// 即时操作(检索/打开/定位/撤销)的轻提示位,独立于上面的任务进度文案。
const opMsg = ref("");

// ── 「盘点」= 先扫一眼文件夹结构 → 勾选要盘点的目录(不限知识库)→ 建库 ──
const pickerOpen = ref(false);
const pickerLoading = ref(false);
const pickerErr = ref("");
const pickerTruncated = ref(false);
const scanRoots = ref<ScanRootInfo[]>([]);
// 已加载的全部节点(根的第一层 + 用户点开后懒加载的更深层)。path → 节点。
const allNodes = reactive(new Map<string, FolderNode>());
// parent 路径 → 其直属子文件夹(后端已按名排序)。
const childIndex = reactive(new Map<string, FolderNode[]>());
// 正在懒加载子目录的文件夹路径。
const childLoading = reactive(new Set<string>());
// 文件夹路径 → 递归总量{files,bytes}(按需限并发计算)。
const sizeCache = reactive(new Map<string, { files: number; bytes: number }>());
// 层级复选框:显式勾上 / 显式取消 的路径(根路径或文件夹路径)。
// 未显式标记的节点 → 继承最近祖先的标记;祖先都没标记 → 看所属根的 defaultOn。
const checked = reactive(new Set<string>());
const unchecked = reactive(new Set<string>());
// 展开了子目录的文件夹/根路径。
const expanded = reactive(new Set<string>());
// 选择器里同级文件夹的排序:size=按大小从大到小(默认,大文件夹先露脸)/ name=按名称。
const pickerSort = ref<"size" | "name">("size");

// 归类(离线纯数学)/ AI 语义归类 / 建索引 —— 全读 store(运行态 + 进度文案)。
const clustering = computed(() => tasks.running.cluster);
const clusterMsg = computed(() => tasks.detail.cluster);
const building = computed(() => tasks.running.index);
const buildMsg = computed(() => tasks.detail.index);
const llmClustering = computed(() => tasks.running.clusterLlm);
const llmMsg = computed(() => tasks.detail.clusterLlm);
const reportPath = computed(() => tasks.reportPath.clusterLlm);

// 折叠状态(均持久化记住用户选择):头部横幅(体积/数量/统计)· 语义分类 · 类型筛选。
// 默认把横幅收起 → 功能键整体上移、把纵向空间让给下方可观看的文件网格。
function loadFc(key: string, def: boolean): boolean {
  try {
    const v = localStorage.getItem(key);
    return v === null ? def : v === "1";
  } catch {
    return def;
  }
}
function saveFc(key: string, v: boolean) {
  try {
    localStorage.setItem(key, v ? "1" : "0");
  } catch {
    /* storage 不可用 */
  }
}
const bannerOpen = ref(loadFc("polaris.fc.banner", false));
const foldersOpen = ref(loadFc("polaris.fc.folders", true));
const kindOpen = ref(loadFc("polaris.fc.kinds", false));
const langOpen = ref(loadFc("polaris.fc.langs", true));
watch(bannerOpen, (v) => saveFc("polaris.fc.banner", v));
watch(foldersOpen, (v) => saveFc("polaris.fc.folders", v));
watch(kindOpen, (v) => saveFc("polaris.fc.kinds", v));
watch(langOpen, (v) => saveFc("polaris.fc.langs", v));
// 语义文件夹下钻路径(目前两级:[] = 顶层主题;[topId] = 某主题内看子主题)
const folderPath = ref<number[]>([]);

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

// 归类小提示(默认不常驻,只在点「智能归类 / AI 归类」时弹一下)
const tip = reactive({ show: false, kind: "" as "" | "cluster" | "ai" });
let tipTimer: ReturnType<typeof setTimeout> | null = null;
function flashTip(kind: "cluster" | "ai") {
  tip.kind = kind;
  tip.show = true;
  if (tipTimer) clearTimeout(tipTimer);
  tipTimer = setTimeout(() => (tip.show = false), 9000);
}
function closeTip() {
  tip.show = false;
  if (tipTimer) clearTimeout(tipTimer);
}

// AI 智能命名(给乱码/杂乱文件名起可读标题)—— 运行态/进度读 store。
const llmTitling = computed(() => tasks.running.titles);
const titleMsg = computed(() => tasks.detail.titles);
// 撤销后本地标记,把「撤销 AI 名称」按钮收起(store 的 doneTick 仍记着这次完成)。
const titlesReverted = ref(false);
const titleDone = computed(
  () => tasks.doneTick.titles > 0 && !tasks.running.titles && !tasks.failed.titles && !titlesReverted.value,
);

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
let searchTimer: ReturnType<typeof setTimeout> | null = null;

// 任务完成(store doneTick 自增)→ 刷新文件中心数据,让新盘点/新索引/新归类结果显示出来。
// 监听全局 store,故即便任务是在别的视图发起、本组件后挂载,回来也会因 tick 变化而刷新。
watch(() => tasks.doneTick.inventory, () => { loadOverview(); loadGrid(true); ensureLangBackfill(); });
watch(() => tasks.doneTick.index, () => { loadOverview(); });
watch(
  () => tasks.doneTick.cluster + tasks.doneTick.clusterLlm,
  () => { folderPath.value = []; activeCluster.value = null; loadOverview(); loadGrid(true); },
);
watch(() => tasks.doneTick.titles, () => { loadGrid(true); });

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

// 「按语言归类」配色:自然语言/媒体给固定色,编程语言按名字哈希到一组高级色,稳定且区分度高。
const LANG_FIXED: Record<string, string> = {
  中文: "#e0736b", 英文: "#5b8cff", 其他语种: "#9aa0e6", 未识别: "#8a8f98",
  图片: "#6fcf97", 视频: "#c264d6", 音频: "#e0a24b", 压缩包: "#93a0b4", 其他文件: "#8a8f98",
  "文档·待识别": "#b0b4bd",
};
const LANG_PALETTE = [
  "#8b6cff", "#42c8d4", "#e08aae", "#7ec8a0", "#d49a6a", "#6cc0c0", "#cf9fd6", "#7f9cf5",
  "#d4b06a", "#b487e0", "#5fa8e6", "#e6a4c4",
];
function langColor(lang: string): string {
  if (LANG_FIXED[lang]) return LANG_FIXED[lang];
  let h = 0;
  for (let i = 0; i < lang.length; i++) h = (h * 31 + lang.charCodeAt(i)) >>> 0;
  return LANG_PALETTE[h % LANG_PALETTE.length];
}

const clusterColor = computed<Record<number, FcCluster>>(() => {
  const m: Record<number, FcCluster> = {};
  for (const c of overview.value?.clusters ?? []) m[c.id] = c;
  return m;
});

// ───────────────────────── 语义文件夹层级(两级) ─────────────────────────
const allClusters = computed<FcCluster[]>(() => overview.value?.clusters ?? []);
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
  folderPath.value.length ? childrenOf(folderPath.value[0]) : topFolders.value,
);
const currentFolder = computed<FcCluster | null>(() =>
  folderPath.value.length ? clusterColor.value[folderPath.value[0]] ?? null : null,
);

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

// 后台把文稿的自然语言(中文/英文)补齐:代码/媒体的语言无需回填即可显示,文稿要读文件头嗅探。
// 单次调用封顶 ~16K 文件(不冻界面),循环到无待回填为止,中途刷新「按语言」分布。幂等、可重入。
let langBackfilling = false;
async function ensureLangBackfill() {
  if (langBackfilling) return;
  langBackfilling = true;
  try {
    for (let i = 0; i < 200; i++) {
      const n = await fc.backfillLang();
      if (i === 0 || n === 0) await loadOverview();
      if (n === 0) break;
    }
  } catch {
    /* 静默:回填失败不影响代码/媒体的按语言归类 */
  } finally {
    langBackfilling = false;
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
      lang: activeLang.value,
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
function pickLang(l: string | null) {
  activeLang.value = activeLang.value === l ? null : l;
  applyFilters();
}
function pickCluster(id: number | null) {
  activeCluster.value = activeCluster.value === id ? null : id;
  if (activeCluster.value !== null) view.value = "gallery";
  applyFilters();
}

// 点一张语义文件夹卡片:有子主题 → 下钻;否则(叶/无子)→ 选中筛选画廊。
function openFolder(c: FcCluster) {
  if (c.parent === 0 && hasChildren(c.id)) {
    folderPath.value = [c.id];
    activeCluster.value = null;
    activeKind.value = null;
  } else {
    activeKind.value = null;
    activeCluster.value = c.id;
    view.value = "gallery";
    applyFilters();
  }
}
// 看某主题下的全部文件(不再细分子主题)。
function viewWholeFolder(c: FcCluster) {
  activeKind.value = null;
  activeCluster.value = c.id;
  view.value = "gallery";
  applyFilters();
}
// 回到顶层主题。
function folderHome() {
  folderPath.value = [];
  activeCluster.value = null;
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

// ───────────────────────── 盘点(扫描 + 选目录 + 建库)─────────────────────────
// 「盘点」点开 → 扫一眼文件夹结构 → 勾选要盘点的目录 → 开始建库。
async function doScan(roots: string[], exclude: string[]) {
  // 真身在全局 store:后台线程跑、事件 App 级监听。切走文件中心也不中断,完成由
  // watch(doneTick.inventory) 刷新本页;全局任务中心浮层全程显示进度。
  await tasks.startInventory(roots, exclude);
}

// ── 打开「盘点」:先扫文件夹结构(根+第一层),让用户勾选 / 逐层点开要盘点的目录 ──
function ingestNodes(nodes: FolderNode[]) {
  for (const n of nodes) {
    if (!allNodes.has(n.path)) allNodes.set(n.path, n);
    const arr = childIndex.get(n.parent);
    if (arr) {
      if (!arr.some((x) => x.path === n.path)) arr.push(n);
    } else {
      childIndex.set(n.parent, [n]);
    }
    requestSize(n.path); // 后台算这个文件夹有多大
  }
}
async function openFolderPicker() {
  if (pickerLoading.value || scanning.value) return;
  pickerOpen.value = true;
  pickerLoading.value = true;
  pickerErr.value = "";
  try {
    allNodes.clear();
    childIndex.clear();
    childLoading.clear();
    sizeCache.clear();
    sizeQueue.length = 0;
    sizeInflight.clear();
    checked.clear();
    unchecked.clear();
    expanded.clear();
    const res = await fc.scanFolders(null);
    scanRoots.value = res.roots;
    pickerTruncated.value = res.truncated;
    ingestNodes(res.folders);
    // 默认勾上的根自动展开,方便直接看到知识库的子目录。
    for (const r of res.roots) if (r.defaultOn) expanded.add(r.path);
  } catch (e: any) {
    pickerErr.value = `扫描失败:${e?.message ?? e}`;
    scanRoots.value = [];
  } finally {
    pickerLoading.value = false;
  }
}
function closeFolderPicker() {
  pickerOpen.value = false;
}

const rootByPath = computed(() => {
  const m = new Map<string, ScanRootInfo>();
  for (const r of scanRoots.value) m.set(r.path, r);
  return m;
});

// 从某路径向上的祖先链(不含自身),最后到所属根路径。
function ancestorsOf(path: string): string[] {
  const out: string[] = [];
  let cur = allNodes.get(path)?.parent;
  while (cur) {
    out.push(cur);
    const pn = allNodes.get(cur);
    if (!pn) break; // cur 已是根路径(根没有 FolderNode)
    cur = pn.parent;
  }
  return out;
}
function rootOf(path: string): string {
  return allNodes.get(path)?.root ?? path;
}
// 节点(根或文件夹)是否「最终被勾选」= 盘点范围内。
function isIncluded(path: string): boolean {
  const chain = [path, ...ancestorsOf(path)];
  for (const p of chain) {
    if (checked.has(p)) return true;
    if (unchecked.has(p)) return false;
  }
  return rootByPath.value.get(rootOf(path))?.defaultOn ?? false;
}
// 父节点(根→无父,看根默认;文件夹→其 parent)的最终状态。
function parentIncluded(path: string): boolean {
  const node = allNodes.get(path);
  if (!node) return false; // 这是根:无父
  return isIncluded(node.parent);
}
function toggleNode(path: string) {
  const want = !isIncluded(path);
  // 清掉所有(已加载)后代的显式标记,让它们继承新状态。
  for (const key of allNodes.keys()) {
    if (key !== path && (key.startsWith(path + "\\") || key.startsWith(path + "/"))) {
      checked.delete(key);
      unchecked.delete(key);
    }
  }
  const node = allNodes.get(path);
  // 基准 = 父节点最终态(根没有父 → 看根 defaultOn)。
  const base = node ? isIncluded(node.parent) : (rootByPath.value.get(path)?.defaultOn ?? false);
  checked.delete(path);
  unchecked.delete(path);
  if (want !== base) (want ? checked : unchecked).add(path);
}
async function toggleExpand(path: string) {
  if (expanded.has(path)) {
    expanded.delete(path);
    return;
  }
  expanded.add(path);
  // 文件夹(非根)且还没加载过子目录 → 懒加载。
  const node = allNodes.get(path);
  if (node && node.hasChildren && !childIndex.has(path) && !childLoading.has(path)) {
    childLoading.add(path);
    try {
      const kids = await fc.scanFolderChildren(node.root, path);
      ingestNodes(kids);
      if (!childIndex.has(path)) childIndex.set(path, []); // 兜底:全被剪掉 → 空数组
    } catch {
      childIndex.set(path, []);
    } finally {
      childLoading.delete(path);
    }
  }
}
function resetPicker() {
  checked.clear();
  unchecked.clear();
}
// 已勾选盘点的文件夹数(用于底部计数;只统计已加载节点)。
const pickerSelected = computed(() => {
  let n = 0;
  for (const node of allNodes.values()) if (isIncluded(node.path)) n++;
  return n;
});
// 至少勾了一个根或文件夹?
const pickerHasSelection = computed(
  () => scanRoots.value.some((r) => isIncluded(r.path)) || pickerSelected.value > 0,
);

// ── 文件夹大小:限并发的后台计算队列 ──
const SIZE_CONCURRENCY = 4;
const sizeQueue: string[] = [];
const sizeInflight = new Set<string>();
let sizeActive = 0;
function requestSize(path: string) {
  if (sizeCache.has(path) || sizeInflight.has(path) || sizeQueue.includes(path)) return;
  sizeQueue.push(path);
  pumpSize();
}
function pumpSize() {
  while (sizeActive < SIZE_CONCURRENCY && sizeQueue.length) {
    const p = sizeQueue.shift()!;
    sizeInflight.add(p);
    sizeActive++;
    fc.folderSize(p)
      .then((r) => sizeCache.set(p, r))
      .catch(() => sizeCache.set(p, { files: 0, bytes: 0 }))
      .finally(() => {
        sizeActive--;
        sizeInflight.delete(p);
        pumpSize();
      });
  }
}
// 仿 WizTree:每层同级里算占比 + 条形(占比=该文件夹/同级已知总和;条长=该文件夹/同级最大)。
const siblingStats = computed(() => {
  const m = new Map<string, { sum: number; max: number }>();
  for (const [parent, kids] of childIndex) {
    let sum = 0;
    let max = 1;
    for (const k of kids) {
      const s = sizeCache.get(k.path);
      if (s) {
        sum += s.bytes;
        if (s.bytes > max) max = s.bytes;
      }
    }
    m.set(parent, { sum, max });
  }
  return m;
});
function sizePct(node: FolderNode): number | null {
  const s = sizeCache.get(node.path);
  if (!s) return null;
  const st = siblingStats.value.get(node.parent);
  if (!st || st.sum <= 0) return 0;
  return (s.bytes / st.sum) * 100;
}
function sizeBar(node: FolderNode): number {
  const s = sizeCache.get(node.path);
  if (!s) return 0;
  const st = siblingStats.value.get(node.parent);
  if (!st || st.max <= 0) return 0;
  return s.bytes / st.max;
}

// ── 扁平化「当前可见行」(支持任意深度的展开)给模板渲染 ──
interface PickerRow {
  key: string;
  kind: "root" | "folder" | "loading" | "empty";
  level: number;
  root?: ScanRootInfo;
  node?: FolderNode;
}
const visibleRows = computed<PickerRow[]>(() => {
  const rows: PickerRow[] = [];
  const pushChildren = (parentPath: string, level: number) => {
    const kids = childIndex.get(parentPath);
    if (childLoading.has(parentPath) && (!kids || !kids.length)) {
      rows.push({ key: parentPath + "#loading", kind: "loading", level });
      return;
    }
    if (!kids || !kids.length) {
      rows.push({ key: parentPath + "#empty", kind: "empty", level });
      return;
    }
    // 同级排序:按大小从大到小(未知大小排末尾),或按名称(后端已按名排好)。
    const ordered =
      pickerSort.value === "size"
        ? [...kids].sort((a, b) => (sizeCache.get(b.path)?.bytes ?? -1) - (sizeCache.get(a.path)?.bytes ?? -1))
        : kids;
    for (const k of ordered) {
      rows.push({ key: k.path, kind: "folder", level, node: k });
      if (expanded.has(k.path)) pushChildren(k.path, level + 1);
    }
  };
  for (const r of scanRoots.value) {
    rows.push({ key: r.path, kind: "root", level: 0, root: r });
    if (expanded.has(r.path)) pushChildren(r.path, 1);
  }
  return rows;
});

// 计算要传给盘点的 roots(最顶层被勾选项)+ exclude(被勾范围内又取消的最顶层项)。
function collectInventoryArgs(): { roots: string[]; exclude: string[] } {
  const roots: string[] = [];
  const exclude: string[] = [];
  // 根:被勾选 → 成为盘点根。
  for (const r of scanRoots.value) {
    if (isIncluded(r.path)) roots.push(r.path);
  }
  // 文件夹:被勾但父未勾 → 顶层勾选项(成为根);未勾但父已勾 → 顶层排除项。
  for (const f of allNodes.values()) {
    const inc = isIncluded(f.path);
    const pinc = parentIncluded(f.path);
    if (inc && !pinc) roots.push(f.path);
    else if (!inc && pinc) exclude.push(f.path);
  }
  return { roots, exclude };
}
async function startInventoryFromPicker() {
  const { roots, exclude } = collectInventoryArgs();
  pickerOpen.value = false;
  await doScan(roots, exclude);
}

// 以下任务全部委托给全局 store(后台线程 + App 级事件监听):切走文件中心也不中断,
// 完成由顶部 watch(doneTick.*) 刷新本页;全局任务中心浮层全程显示「还在跑 + 进度」。
async function doCluster() {
  flashTip("cluster");
  await tasks.startCluster();
}

// 构建向量索引(文本 → 硅基 BGE-M3 嵌入),让「智能归类」从词法升级到语义。
async function buildIndex() {
  if (!overview.value?.hasEmbedProvider) {
    app.setView("sense_api"); // 没配嵌入 key → 跳设置页配硅基(免费)
    return;
  }
  await tasks.startIndex();
}

// 用已连接的大模型按语义归类(免嵌入 key)+ 桌面生成 HTML 报告。
async function doClusterLlm() {
  flashTip("ai");
  await tasks.startClusterLlm();
}
function openReport() {
  if (reportPath.value) openPath(reportPath.value);
}

// 合并后的统一「智能归类」入口:
//  · 配了硅基流动嵌入 key(hasEmbedProvider)→ AI 语义归类(复用已有向量并在桌面出报告);
//  · 没配 key → 离线「文件夹 / 名称」启发式归类。提示条据是否配 key 给出对应说明 / 配 key 入口。
async function doSmartCluster() {
  if (clustering.value || llmClustering.value) return;
  flashTip(overview.value?.hasEmbedProvider ? "ai" : "cluster");
  await tasks.startSmartCluster(!!overview.value?.hasEmbedProvider);
}

async function doTitlesLlm() {
  titlesReverted.value = false;
  await tasks.startTitles();
}
async function resetTitles() {
  try {
    await fc.titlesClear();
    titlesReverted.value = true;
    opMsg.value = "已撤销 AI 名称,回落到本地清洗名";
    loadGrid(true);
  } catch (e: any) {
    opMsg.value = `撤销失败:${e?.message ?? e}`;
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
    opMsg.value = `检索失败:${e?.message ?? e}`;
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
    opMsg.value = `打开失败:${e?.message ?? e}`;
  }
}
async function revealCard(card: FileCard) {
  try {
    await artifactsApi.reveal(card.abspath);
  } catch (e: any) {
    opMsg.value = `定位失败:${e?.message ?? e}`;
  }
}

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
  // 后台补齐文稿自然语言(不阻塞首屏;代码/媒体的按语言归类已即时可用)
  ensureLangBackfill();
  await nextTick();
  // 进入文件中心且库还是空的 → 立刻拉起「让 AI 更懂你」引导,用户一点进来就被带着走完
  // 盘点 → 语义归类 → 图谱 → 建索引。库一旦有内容(说明引导过/手动盘过)就不再打扰。
  if ((overview.value?.totalFiles ?? 0) === 0) openWizard();
});

onBeforeUnmount(() => {
  thumbObs?.disconnect();
  moreObs?.disconnect();
  // 任务事件监听已托管给全局 fileTasks store(App 级注册一次,永不在此退订)——
  // 这正是「切走文件中心,盘点/建索引/归类仍在后台跑、进度不丢」的关键。
  if (tipTimer) clearTimeout(tipTimer);
});
</script>

<template>
  <div class="fc">
    <!-- 顶部琉璃横幅(可收起:收起后只留一条,显示文件数/总量) -->
    <div class="fc-banner glass" :class="{ collapsed: !bannerOpen }">
      <div class="fc-title-wrap">
        <div class="fc-title"><Orbit :size="17" :stroke-width="1.6" /> 文件中心</div>
        <div v-if="bannerOpen" class="fc-sub">同类数据自动归在一起 · 缩略图 / 首帧 / 类型图标 · 智能检索</div>
        <div v-else class="fc-mini">
          {{ (overview?.totalFiles ?? 0).toLocaleString() }} 个文件 · {{ fmtBytes(overview?.totalBytes ?? 0) }}
        </div>
      </div>
      <div v-if="bannerOpen" class="fc-stats">
        <div v-for="s in headerStats" :key="s.label" class="stat">
          <div class="stat-val">{{ s.value }}</div>
          <div class="stat-lab">{{ s.label }}</div>
        </div>
      </div>
      <button class="fc-collapse" :title="bannerOpen ? '收起' : '展开'" @click="bannerOpen = !bannerOpen">
        <ChevronDown :size="16" :stroke-width="1.8" :class="{ flip: !bannerOpen }" />
      </button>
    </div>

    <!-- 工具条 -->
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
        <button
          class="tool-btn wizard"
          title="让 AI 更懂你:盘点 → 智能归类 → 知识图谱 → 建索引 → 进对话,一条龙引导"
          @click="openWizard"
        >
          <Sparkles :size="14" :stroke-width="1.8" />
          <span>智能向导</span>
        </button>
        <button
          class="tool-btn"
          :disabled="!overview?.totalFiles"
          title="星图:把你的文件库渲染成星河图谱(归过类更好看)——一眼看清你都有些什么"
          @click="galaxyOpen = true"
        >
          <Orbit :size="14" :stroke-width="1.8" />
          <span>星图</span>
        </button>
        <button
          class="tool-btn"
          :disabled="scanning || pickerLoading"
          title="盘点:先扫一眼文件夹结构,勾选要盘点的目录(可选知识库之外的盘符/文件夹),再建库"
          @click="openFolderPicker"
        >
          <LoaderCircle v-if="scanning || pickerLoading" :size="14" class="spin" />
          <FolderSearch v-else :size="14" :stroke-width="1.8" />
          <span>{{ scanning ? "盘点中" : "盘点" }}</span>
        </button>
        <button
          class="tool-btn accent"
          :disabled="clustering || llmClustering || !overview?.totalFiles"
          :title="overview?.hasEmbedProvider
            ? '已配嵌入 key:用语义模型自动归类(复用已有向量)并在桌面生成报告'
            : '未配 API key:按文件夹 / 名称离线归类;到设置页配硅基 key(免费)后自动升级为语义 AI 归类'"
          @click="doSmartCluster"
        >
          <LoaderCircle v-if="clustering || llmClustering" :size="14" class="spin" />
          <Wand2 v-else :size="14" :stroke-width="1.8" />
          <span>{{ clustering || llmClustering ? "归类中" : "智能归类" }}</span>
        </button>
        <button
          class="tool-btn"
          :disabled="building || !overview?.totalFiles"
          :title="overview?.hasEmbedProvider
            ? '为文本建/续建向量索引(硅基 BGE-M3,后台跑),建好后能按「意思」搜文件'
            : '建索引需要嵌入 key:点这里到设置页配硅基 key(免费),全文索引则照常后台建'"
          @click="buildIndex"
        >
          <LoaderCircle v-if="building" :size="14" class="spin" />
          <Radar v-else :size="14" :stroke-width="1.8" />
          <span>{{ building ? "建索引中" : overview && overview.embeddedFiles > 0 ? "续建索引" : "建索引" }}</span>
        </button>
        <button
          class="tool-btn ai"
          :disabled="llmTitling || !overview?.totalFiles"
          title="用大模型给乱码/杂乱的文件名起可读的中文标题(只改显示,不改磁盘文件名)"
          @click="doTitlesLlm"
        >
          <LoaderCircle v-if="llmTitling" :size="14" class="spin" />
          <Sparkles v-else :size="14" :stroke-width="1.8" />
          <span>{{ llmTitling ? "整理中" : "AI 整理名称" }}</span>
        </button>
      </div>
    </div>

    <!-- AI 整理名称进度 -->
    <div v-if="titleMsg" class="fc-llm">
      <Sparkles :size="14" :stroke-width="1.8" class="llm-ic" />
      <span class="llm-text">{{ titleMsg }}</span>
      <button v-if="titleDone" class="link-btn" @click="resetTitles">撤销 AI 名称</button>
    </div>

    <!-- AI 归类进度 / 报告 -->
    <div v-if="llmMsg" class="fc-llm">
      <Brain :size="14" :stroke-width="1.8" class="llm-ic" />
      <span class="llm-text">{{ llmMsg }}</span>
      <button v-if="reportPath" class="link-btn" @click="openReport">
        <FileText :size="13" :stroke-width="1.8" /> 打开桌面报告
      </button>
    </div>

    <!-- 归类小提示:默认不常驻,只在点「智能归类 / AI 归类」时弹一下解释能力 -->
    <transition name="tip">
      <div v-if="tip.show && hasFiles" class="fc-tip">
        <button class="tip-x" title="关闭" @click="closeTip"><X :size="12" :stroke-width="2" /></button>
        <template v-if="tip.kind === 'cluster'">
          <Wand2 :size="14" :stroke-width="1.8" class="tip-ic" />
          <span class="tip-body">
            <template v-if="(overview?.embeddedFiles ?? 0) > 0">
              按<b>内容语义</b>把相似文件归成两级主题树(复用已有向量,零新增嵌入)。
            </template>
            <template v-else>
              <b>未配置 API key</b>,当前按<b>文件夹 / 名称</b>离线归类。到设置页配硅基 key(<b>免费</b>)后,「智能归类」会自动升级为按<b>内容语义</b>的 AI 归类。
            </template>
          </span>
          <button v-if="overview && !overview.hasEmbedProvider" class="tip-act" @click="app.setView('sense_api')">
            <KeyRound :size="12" :stroke-width="1.8" /> 配 key
          </button>
          <button
            v-else-if="overview && overview.textFiles > overview.embeddedFiles"
            class="tip-act"
            :disabled="building"
            @click="buildIndex"
          >
            <LoaderCircle v-if="building" :size="12" class="spin" />
            <Radar v-else :size="12" :stroke-width="1.8" />
            {{ overview.embeddedFiles > 0 ? "续建索引" : "建索引" }}
          </button>
        </template>
        <template v-else>
          <Brain :size="14" :stroke-width="1.8" class="tip-ic ai" />
          <span class="tip-body">
            用已连接的大模型按<b>思想主题</b>归成两级树(<b>免 key</b>),完成后桌面出 HTML 报告。
          </span>
        </template>
      </div>
    </transition>

    <div v-if="scanMsg || clusterMsg || buildMsg || opMsg" class="fc-note">{{ buildMsg || clusterMsg || scanMsg || opMsg }}</div>

    <!-- 语义文件夹:同主题归一格,点开看子主题,再点筛选画廊 -->
    <div v-if="hasFiles && hasClusters" class="fc-folders">
      <div class="fld-bar">
        <button class="fld-toggle" :class="{ open: foldersOpen }" :title="foldersOpen ? '收起分类' : '展开分类'" @click="foldersOpen = !foldersOpen">
          <ChevronDown :size="14" :stroke-width="1.8" :class="{ flip: !foldersOpen }" />
          <span>分类</span>
          <span class="fld-toggle-n">{{ folderCards.length }}</span>
        </button>
        <button class="crumb" :class="{ on: folderPath.length === 0 && activeCluster === null }" @click="folderHome">
          <Layers :size="13" :stroke-width="1.8" /> 全部主题
        </button>
        <template v-if="currentFolder">
          <ChevronRight :size="13" :stroke-width="2" class="crumb-sep" />
          <span class="crumb cur" :style="{ '--c': currentFolder.color }">
            <span class="crumb-dot" />{{ currentFolder.label }}
          </span>
          <button class="crumb-all" @click="viewWholeFolder(currentFolder)">看全部 {{ currentFolder.size }} 个</button>
        </template>
      </div>
      <div v-show="foldersOpen" class="fld-grid">
        <button
          v-if="folderPath.length"
          class="fld-card back"
          @click="folderHome"
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
          @click="openFolder(c)"
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
        <button class="chip" :class="{ on: activeKind === null }" @click="activeKind = null; applyFilters()">
          全部类型
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
        <button class="chip" :class="{ on: activeLang === null }" @click="activeLang = null; applyFilters()">
          全部语言
        </button>
        <button
          v-for="lc in overview?.byLang ?? []"
          :key="lc.lang"
          class="chip"
          :class="{ on: activeLang === lc.lang }"
          :style="{ '--chip': langColor(lc.lang) }"
          @click="pickLang(lc.lang)"
        >
          <span class="chip-dot" />{{ lc.lang }}
          <span class="chip-n">{{ lc.count }}</span>
        </button>
      </div>
    </div>

    <!-- 空库引导 -->
    <div v-if="!hasFiles" class="fc-empty glass">
      <div class="empty-orb"><FolderSearch :size="30" :stroke-width="1.3" /></div>
      <div class="empty-title">文件中心还是空的</div>
      <div class="empty-sub">点「盘点」先扫一眼文件夹,勾选要盘点的目录(可选知识库之外的盘符/文件夹),建成可视化文件库;<br />已嵌入文本可再点「智能归类」把相似数据自动放在一起。</div>
      <button class="empty-cta" :disabled="scanning || pickerLoading" @click="openFolderPicker">
        <LoaderCircle v-if="scanning || pickerLoading" :size="15" class="spin" />
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
            <div class="tile-name" :title="card.name">{{ card.title || card.name }}</div>
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

      <!-- 分类树状图 -->
      <div v-show="view === 'clusters'" class="clustree glass">
        <div v-if="!overview?.clusters.length" class="star-empty">
          <Sparkles :size="26" :stroke-width="1.4" />
          <div>还没有归类</div>
          <div class="star-hint">
            点「智能归类」把相似文件归成一棵<b>分类树</b> —— 有向量索引走<b>语义</b>,没有就按<b>文件夹 / 名称</b>归类。
            每个大类点开能看它下面又分了哪些子类,点节点即可筛出该类的文件。
          </div>
          <button class="empty-cta" :disabled="clustering || llmClustering || !overview?.totalFiles" @click="doSmartCluster">
            <LoaderCircle v-if="clustering || llmClustering" :size="15" class="spin" />
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
                <button class="tn-body" :title="'查看「' + c.label + '」的全部文件'" @click="viewWholeFolder(c)">
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
                  <button class="tn-body" :title="'查看「' + ch.label + '」的全部文件'" @click="viewWholeFolder(ch)">
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
            <span class="lv-name" :title="card.name">{{ card.title || card.name }}</span>
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

      <!-- 核心层 · 知识体系(整套知识库系统的家) -->
      <div v-show="view === 'core'" class="coreview">
        <div class="core-hero glass">
          <div class="ch-ic"><Network :size="24" :stroke-width="1.5" /></div>
          <div class="ch-main">
            <div class="ch-t">核心层 · 知识体系</div>
            <div class="ch-d">
              这是你资料的「大脑」——散落的文件经过编译,连成一张互联的知识网。
              个人库自动归出你常用的主题;企业库在行业框内抽出可溯源的实体与关系。
            </div>
          </div>
          <button class="ch-open" @click="openFullKb"><BookOpen :size="14" :stroke-width="1.8" /> 打开完整知识库</button>
        </div>

        <div v-if="ontoLoading" class="grid-loading"><LoaderCircle :size="18" class="spin" /> 读取知识体系…</div>

        <!-- 企业:Schema-Guided 抽出的关系三元组(中文、可溯源) -->
        <template v-else-if="ontoSchemasWithData.length">
          <div class="core-section-h">
            <ShieldCheck :size="14" :stroke-width="1.8" />
            <b>已建立的关系网</b>
            <span class="csh-fine">共 {{ ontoTotal }} 条 · 每条都标了置信度与来源,可审计</span>
          </div>
          <div v-for="sc in ontoSchemasWithData" :key="sc.id" class="onto-card glass">
            <button class="oc-head" @click="toggleSchema(sc.id)">
              <span class="oc-name">{{ sc.name }}</span>
              <span class="oc-ind">{{ sc.industry }}</span>
              <span class="oc-cnt">{{ sc.triples }} 条关系</span>
              <ChevronDown class="oc-chev" :class="{ flip: openSchema === sc.id }" :size="15" :stroke-width="2" />
            </button>
            <div v-if="openSchema === sc.id" class="oc-body">
              <div v-if="triplesLoading" class="grid-loading"><LoaderCircle :size="16" class="spin" /> 加载关系…</div>
              <div v-else class="triple-list">
                <div v-for="(tp, i) in schemaTriples" :key="i" class="triple-row">
                  <span class="tr-sub">{{ tp.subject }}</span>
                  <span class="tr-rel">{{ tp.predicate }}</span>
                  <span class="tr-obj">{{ tp.object }}</span>
                  <span v-if="tp.sourceFile" class="tr-src" :title="tp.sourceFile">{{ tp.sourceFile.split('/').pop() }}</span>
                  <span class="tr-conf" :title="'置信度'">{{ Math.round(tp.confidence * 100) }}%</span>
                </div>
              </div>
            </div>
          </div>
        </template>

        <!-- 还没有知识体系:引导去构建 -->
        <div v-else class="core-empty">
          <Sparkles :size="26" :stroke-width="1.3" />
          <p>还没有构建知识体系。点「智能向导」,个人库自动归出常用主题,企业库在行业框内抽实体关系。</p>
          <button class="ch-open" @click="openWizard"><Wand2 :size="14" :stroke-width="1.8" /> 打开智能向导</button>
        </div>
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
        <div class="detail-name">{{ selected.title || selected.name }}</div>
        <div v-if="selected.title && selected.title !== selected.name" class="detail-rawname" :title="selected.name">原名：{{ selected.name }}</div>
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

    <!-- 文件夹选择器:盘点前先扫一眼,取消勾选不要的文件夹 -->
    <transition name="fade">
      <div v-if="pickerOpen" class="picker-scrim" @click="closeFolderPicker">
        <div class="picker glass" @click.stop>
          <div class="picker-head">
            <div class="picker-title">
              <FolderTree :size="17" :stroke-width="1.7" />
              <span>选择要盘点的文件夹</span>
            </div>
            <button class="picker-close" @click="closeFolderPicker"><X :size="16" :stroke-width="2" /></button>
          </div>
          <div class="picker-sub">
            勾选<b>要盘点的目录</b>(可勾知识库之外的盘符 / 文件夹),再开始建库。
            所有盘 / 卷<b>默认已全部勾上</b>(系统、缓存目录自动跳过)—— 想缩小范围展开后取消即可。
          </div>
          <div v-if="!pickerLoading && scanRoots.length" class="picker-sortbar">
            <span class="ps-lab"><ArrowDownWideNarrow :size="13" :stroke-width="1.7" /> 同级排序</span>
            <button class="ps-btn" :class="{ on: pickerSort === 'size' }" @click="pickerSort = 'size'">大小(大→小)</button>
            <button class="ps-btn" :class="{ on: pickerSort === 'name' }" @click="pickerSort = 'name'">名称</button>
          </div>

          <div v-if="pickerLoading" class="picker-loading">
            <LoaderCircle :size="20" class="spin" /> 正在扫描文件夹结构…
          </div>
          <div v-else-if="pickerErr" class="picker-error">{{ pickerErr }}</div>
          <div v-else-if="!scanRoots.length" class="picker-error">没有可盘点的目录。</div>
          <div v-else class="picker-tree">
            <template v-for="row in visibleRows" :key="row.key">
              <!-- 根行(整盘/整库) -->
              <div
                v-if="row.kind === 'root'"
                class="picker-row root"
                :class="{ off: !isIncluded(row.root!.path) }"
                :style="{ paddingLeft: 8 + 'px' }"
              >
                <button class="pk-check" :class="{ on: isIncluded(row.root!.path) }" @click="toggleNode(row.root!.path)">
                  <Check v-if="isIncluded(row.root!.path)" :size="12" :stroke-width="2.6" />
                </button>
                <button class="pk-expand vis" @click="toggleExpand(row.root!.path)">
                  <ChevronRight :size="19" :stroke-width="2" :class="{ open: expanded.has(row.root!.path) }" />
                </button>
                <Layers :size="14" :stroke-width="1.8" class="pk-ic" />
                <span class="pk-name root-name" :title="row.root!.path">{{ row.root!.label }}</span>
                <span class="pk-meta">{{ row.root!.path }}</span>
              </div>
              <!-- 文件夹行(任意深度) -->
              <div
                v-else-if="row.kind === 'folder'"
                class="picker-row"
                :class="{ off: !isIncluded(row.node!.path) }"
                :style="{ paddingLeft: 8 + row.level * 20 + 'px' }"
              >
                <button class="pk-check" :class="{ on: isIncluded(row.node!.path) }" @click="toggleNode(row.node!.path)">
                  <Check v-if="isIncluded(row.node!.path)" :size="12" :stroke-width="2.6" />
                </button>
                <button class="pk-expand" :class="{ vis: row.node!.hasChildren }" @click="toggleExpand(row.node!.path)">
                  <ChevronRight :size="19" :stroke-width="2" :class="{ open: expanded.has(row.node!.path) }" />
                </button>
                <Folder :size="15" :stroke-width="1.6" class="pk-ic" />
                <span class="pk-name" :title="row.node!.path">{{ row.node!.name }}</span>
                <template v-if="sizeCache.get(row.node!.path)">
                  <span class="pk-bar" :title="(sizePct(row.node!) ?? 0).toFixed(1) + '% 占同级'">
                    <span class="pk-bar-fill" :style="{ width: Math.max(2, sizeBar(row.node!) * 100) + '%' }" />
                  </span>
                  <span class="pk-pct">{{ (sizePct(row.node!) ?? 0).toFixed(1) }}%</span>
                  <span class="pk-size">{{ fmtBytes(sizeCache.get(row.node!.path)!.bytes) }}</span>
                </template>
                <span v-else class="pk-meta calc">计算大小…</span>
              </div>
              <!-- 懒加载占位 -->
              <div
                v-else-if="row.kind === 'loading'"
                class="picker-row sub-loading"
                :style="{ paddingLeft: 8 + row.level * 20 + 'px' }"
              >
                <LoaderCircle :size="13" class="spin" /> 加载子文件夹…
              </div>
              <!-- 空目录占位 -->
              <div
                v-else
                class="picker-row empty-row"
                :style="{ paddingLeft: 8 + row.level * 20 + 'px' }"
              >
                （无子文件夹）
              </div>
            </template>
            <div v-if="pickerTruncated" class="picker-trunc">顶层文件夹太多,列表已截断到前 5000 个。</div>
          </div>

          <div class="picker-foot">
            <button class="pk-reset" :disabled="!checked.size && !unchecked.size" title="恢复默认勾选" @click="resetPicker">
              <RotateCcw :size="13" :stroke-width="1.8" /> 恢复默认
            </button>
            <span class="pk-count">已选 <b>{{ pickerSelected }}</b> 个文件夹</span>
            <button class="pk-go" :disabled="pickerLoading || !pickerHasSelection" @click="startInventoryFromPicker">
              <FolderSearch :size="14" :stroke-width="1.8" /> 开始盘点
            </button>
          </div>
        </div>
      </div>
    </transition>

    <!-- 星图浮层:文件库 → 星河图谱(全屏沉浸) -->
    <transition name="fade">
      <div v-if="galaxyOpen" class="galaxy-overlay">
        <div class="galaxy-head">
          <span class="galaxy-title"><Orbit :size="16" :stroke-width="1.7" /> 我的资料 · 星图</span>
          <button class="galaxy-close" title="关闭" @click="galaxyOpen = false"><X :size="18" :stroke-width="2" /></button>
        </div>
        <KnowledgeGraph source="files" :embedded="true" />
      </div>
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

/* 横幅收起态:压成一条,只留标题 + 文件数/总量 */
.fc-banner { transition: padding 0.2s; }
.fc-banner.collapsed { padding: 9px 16px 9px 22px; }
.fc-mini {
  margin-top: 3px;
  font-size: 12px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}
.fc-collapse {
  position: relative;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  border: 1px solid var(--border-soft);
  background: color-mix(in srgb, var(--panel) 70%, transparent);
  color: var(--muted);
  border-radius: 8px;
  cursor: pointer;
  flex: none;
  transition: color 0.16s, border-color 0.16s;
}
.fc-collapse:hover { color: var(--text); border-color: var(--border-strong); }
.fc-collapse .flip { transform: rotate(180deg); }
.fc-collapse :deep(svg) { transition: transform 0.2s; }

/* ── 归类小提示(弹出) ── */
.fc-tip {
  position: relative;
  display: flex;
  align-items: center;
  gap: 9px;
  margin: 0 2px;
  padding: 9px 32px 9px 14px;
  border-radius: 12px;
  background: color-mix(in srgb, var(--primary) 8%, var(--panel));
  border: 1px solid color-mix(in srgb, var(--primary) 30%, transparent);
  font-size: 12.5px;
  color: var(--text-2);
  box-shadow: var(--shadow-sm);
}
.tip-ic { color: var(--gold); flex: none; }
.tip-ic.ai { color: #8b6cff; }
.tip-body { flex: 1; min-width: 0; line-height: 1.6; }
.tip-body b { color: var(--text); }
.tip-act {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  height: 26px;
  padding: 0 11px;
  border: 1px solid color-mix(in srgb, var(--primary) 45%, transparent);
  background: color-mix(in srgb, var(--primary) 12%, transparent);
  color: var(--primary);
  border-radius: 8px;
  font-size: 12px;
  cursor: pointer;
  flex: none;
}
.tip-act:hover:not(:disabled) { background: color-mix(in srgb, var(--primary) 20%, transparent); }
.tip-act:disabled { opacity: 0.55; cursor: default; }
.tip-x {
  position: absolute;
  top: 7px;
  right: 8px;
  display: inline-flex;
  border: none;
  background: transparent;
  color: var(--dim);
  cursor: pointer;
  padding: 2px;
  border-radius: 6px;
}
.tip-x:hover { color: var(--text); background: var(--selection-bg); }
.tip-enter-active, .tip-leave-active { transition: opacity 0.2s, transform 0.2s; }
.tip-enter-from, .tip-leave-to { opacity: 0; transform: translateY(-4px); }

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

/* AI 归类进度 / 报告条 */
.fc-llm {
  display: flex;
  align-items: center;
  gap: 9px;
  padding: 8px 14px;
  margin: 0 2px;
  border-radius: 12px;
  background: color-mix(in srgb, #8b6cff 8%, var(--panel));
  border: 1px solid color-mix(in srgb, #8b6cff 26%, transparent);
  font-size: 12.5px;
  color: var(--text-2);
}
.llm-ic { color: #8b6cff; flex: none; }
.llm-text { flex: 1; min-width: 0; }
.fc-llm .link-btn {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  height: 28px;
  padding: 0 12px;
  border: 1px solid color-mix(in srgb, #8b6cff 50%, transparent);
  background: color-mix(in srgb, #8b6cff 14%, transparent);
  color: #8b6cff;
  border-radius: 9px;
  font-size: 12px;
  cursor: pointer;
  flex: none;
}
.fc-llm .link-btn:hover { background: color-mix(in srgb, #8b6cff 22%, transparent); }

.fc-note {
  font-size: 12px;
  color: var(--muted);
  padding: 0 8px;
  margin-top: -4px;
}

/* ── 语义就绪度条 ── */
.fc-semantic {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
  padding: 9px 14px;
  margin: 0 2px;
  border-radius: 12px;
  background: color-mix(in srgb, var(--primary) 5%, var(--panel));
  border: 1px solid var(--border-soft);
  -webkit-backdrop-filter: blur(8px);
  backdrop-filter: blur(8px);
  font-size: 12.5px;
  color: var(--text-2);
}
.sem-ic {
  flex: none;
}
.sem-ic.warn {
  color: var(--gold);
}
.sem-ic.ok {
  color: var(--primary);
}
.sem-text {
  flex: 1;
  min-width: 220px;
  line-height: 1.65;
}
.sem-text b {
  color: var(--text);
}
.sem-ok {
  color: var(--ok);
}
.fc-semantic .link-btn {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  height: 28px;
  padding: 0 12px;
  border: 1px solid color-mix(in srgb, var(--primary) 45%, transparent);
  background: color-mix(in srgb, var(--primary) 12%, transparent);
  color: var(--primary);
  border-radius: 9px;
  font-size: 12px;
  cursor: pointer;
  flex: none;
  transition: all 0.16s;
}
.fc-semantic .link-btn:hover:not(:disabled) {
  background: color-mix(in srgb, var(--primary) 20%, transparent);
}
.fc-semantic .link-btn:disabled {
  opacity: 0.55;
  cursor: default;
}
.build-msg {
  flex-basis: 100%;
  font-size: 11.5px;
  color: var(--muted);
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

/* ── 分类树状图 ── */
.clustree {
  height: calc(100vh - 320px);
  min-height: 420px;
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
.empty-actions { display: flex; gap: 10px; align-items: center; }
.empty-cta.ghost {
  background: color-mix(in srgb, var(--panel) 70%, transparent);
  border: 1px solid var(--border-soft);
  color: var(--text);
}
.empty-cta.ghost:hover:not(:disabled) { border-color: var(--border-strong); }

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

/* ── 文件夹选择器(盘点前先扫一眼) ── */
.picker-scrim {
  position: absolute;
  inset: 0;
  z-index: 30;
  display: flex;
  align-items: center;
  justify-content: center;
  background: color-mix(in srgb, var(--bg) 50%, transparent);
  -webkit-backdrop-filter: blur(3px);
  backdrop-filter: blur(3px);
  padding: 24px;
}
.picker {
  width: min(760px, 100%);
  max-height: min(82vh, 760px);
  display: flex;
  flex-direction: column;
  padding: 18px 20px 16px;
  box-shadow: var(--shadow-lg, 0 24px 60px -20px rgba(0, 0, 0, 0.45));
}
.picker-head { display: flex; align-items: center; justify-content: space-between; }
.picker-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-family: var(--serif);
  font-size: 16px;
  letter-spacing: 1px;
  color: var(--ink);
}
.picker-close {
  display: inline-flex;
  border: none;
  background: transparent;
  color: var(--dim);
  cursor: pointer;
  padding: 4px;
  border-radius: 7px;
}
.picker-close:hover { color: var(--text); background: var(--selection-bg); }
.picker-sub { margin: 6px 0 12px; font-size: 12.5px; line-height: 1.6; color: var(--muted); }
.picker-sub b { color: var(--text); }
.picker-sortbar { display: flex; align-items: center; gap: 7px; margin: 0 0 10px; }
.ps-lab { display: inline-flex; align-items: center; gap: 5px; font-size: 12px; color: var(--muted); margin-right: 2px; }
.ps-btn {
  height: 25px; padding: 0 10px; border-radius: 7px; font-size: 11.5px; cursor: pointer;
  border: 1px solid var(--border-soft); background: color-mix(in srgb, var(--panel) 55%, transparent); color: var(--text-2);
}
.ps-btn:hover { color: var(--text); border-color: var(--border-strong); }
.ps-btn.on { color: var(--primary); border-color: color-mix(in srgb, var(--primary) 45%, transparent); background: color-mix(in srgb, var(--primary) 10%, transparent); }
.picker-loading, .picker-error {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 26px 4px;
  color: var(--muted);
  font-size: 13px;
}
.picker-tree {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  border: 1px solid var(--border-soft);
  border-radius: 12px;
  padding: 6px;
  background: color-mix(in srgb, var(--panel) 40%, transparent);
}
.picker-root + .picker-root { margin-top: 8px; }
.picker-root-head {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 8px 4px;
  font-size: 11.5px;
  color: var(--muted);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.picker-row {
  display: flex;
  align-items: center;
  gap: 8px;
  height: 32px;
  padding: 0 8px;
  border-radius: 8px;
}
.picker-row:hover { background: var(--selection-bg); }
.picker-row.root { margin-top: 4px; }
.picker-row.root .root-name { font-weight: 650; font-size: 13.5px; }
.picker-row.empty-row { font-size: 12px; color: var(--dim); height: 26px; }
.picker-row.sub-loading { font-size: 12px; color: var(--muted); height: 28px; gap: 7px; }
.picker-row.off .pk-name, .picker-row.off .pk-meta { opacity: 0.4; }
.picker-row.off .root-name { text-decoration: none; }
.pk-check {
  flex: none;
  width: 17px;
  height: 17px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border: 1.5px solid var(--border-strong);
  border-radius: 5px;
  background: transparent;
  color: var(--btn-solid-text);
  cursor: pointer;
  transition: background 0.14s, border-color 0.14s;
}
.pk-check.on { background: var(--primary); border-color: var(--primary); color: #fff; }
.pk-check:disabled { opacity: 0.35; cursor: default; }
.pk-expand {
  flex: none;
  width: 28px;
  height: 28px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border: none;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  visibility: hidden;
  border-radius: 8px;
  transition: background 0.14s, color 0.14s;
}
.pk-expand.vis { visibility: visible; }
.pk-expand.vis:hover { background: var(--selection-bg); color: var(--text); }
.pk-expand :deep(svg) { transition: transform 0.18s; }
.pk-expand :deep(svg.open) { transform: rotate(90deg); }
.pk-ic { color: var(--muted); flex: none; }
.pk-name {
  flex: 1;
  min-width: 0;
  font-size: 13px;
  color: var(--text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.pk-meta { flex: none; font-size: 11px; color: var(--muted); font-variant-numeric: tabular-nums; }
.pk-meta.calc { font-style: italic; opacity: 0.7; }
/* ── 仿 WizTree 占比条 + 百分比 + 大小 ── */
.pk-bar {
  flex: none;
  width: 120px;
  height: 9px;
  border-radius: 5px;
  background: color-mix(in srgb, var(--ink) 9%, transparent);
  overflow: hidden;
}
.pk-bar-fill {
  display: block;
  height: 100%;
  border-radius: 5px;
  background: linear-gradient(90deg, var(--primary), var(--gold));
}
.pk-pct {
  flex: none;
  width: 50px;
  text-align: right;
  font-size: 12px;
  font-weight: 600;
  color: var(--text-2);
  font-variant-numeric: tabular-nums;
}
.pk-size {
  flex: none;
  width: 78px;
  text-align: right;
  font-size: 11.5px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}
.picker-trunc { padding: 8px; font-size: 11.5px; color: var(--dim); text-align: center; }
.picker-foot {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-top: 14px;
}
.pk-reset {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  height: 30px;
  padding: 0 12px;
  border: 1px solid var(--border-soft);
  background: transparent;
  color: var(--text-2);
  border-radius: 9px;
  font-size: 12px;
  cursor: pointer;
}
.pk-reset:hover:not(:disabled) { border-color: var(--border-strong); color: var(--text); }
.pk-reset:disabled { opacity: 0.45; cursor: default; }
.pk-count { flex: 1; font-size: 12.5px; color: var(--muted); text-align: center; }
.pk-count b { color: var(--text); font-variant-numeric: tabular-nums; }
.pk-go {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  height: 34px;
  padding: 0 18px;
  border: none;
  border-radius: 10px;
  background: var(--btn-solid-bg);
  color: var(--btn-solid-text);
  font-size: 13px;
  cursor: pointer;
  transition: opacity 0.16s, transform 0.16s;
}
.pk-go:hover:not(:disabled) { transform: translateY(-1px); }
.pk-go:disabled { opacity: 0.55; cursor: default; }

/* ── 动效 ── */
.spin { animation: spin 0.9s linear infinite; }
@keyframes spin { to { transform: rotate(360deg); } }
.drawer-enter-active, .drawer-leave-active { transition: transform 0.26s cubic-bezier(0.2, 0.7, 0.3, 1), opacity 0.26s; }
.drawer-enter-from, .drawer-leave-to { transform: translateX(20px); opacity: 0; }
.fade-enter-active, .fade-leave-active { transition: opacity 0.26s; }
.fade-enter-from, .fade-leave-to { opacity: 0; }

/* ── 核心层 · 知识体系 ── */
.seg-btn.core-seg { width: auto; padding: 0 11px; gap: 6px; }
.seg-lab { font-size: 12px; font-weight: 600; }
.coreview { display: flex; flex-direction: column; gap: 14px; padding: 4px 2px 24px; }
.core-hero {
  display: flex; align-items: center; gap: 16px; padding: 18px 20px; border-radius: 16px;
  border: 1px solid var(--border-soft);
}
.ch-ic {
  width: 50px; height: 50px; border-radius: 14px; flex: none;
  display: flex; align-items: center; justify-content: center;
  color: var(--primary); background: color-mix(in srgb, var(--primary) 13%, transparent);
}
.ch-main { flex: 1; min-width: 0; }
.ch-t { font-size: 17px; font-weight: 660; color: var(--ink); }
.ch-d { font-size: 12.5px; color: var(--muted); line-height: 1.6; margin-top: 3px; }
.ch-open {
  flex: none; display: inline-flex; align-items: center; gap: 6px; height: 34px; padding: 0 14px;
  border-radius: 10px; border: 1px solid var(--primary); background: var(--primary); color: #fff;
  font-size: 13px; cursor: pointer; transition: filter 0.15s;
}
.ch-open:hover { filter: brightness(1.08); }
.core-section-h { display: flex; align-items: center; gap: 8px; font-size: 13px; color: var(--text); margin-top: 4px; }
.core-section-h :deep(svg) { color: var(--primary); flex: none; }
.csh-fine { margin-left: auto; font-size: 11px; color: var(--dim); }
.onto-card { border-radius: 13px; border: 1px solid var(--border-soft); overflow: hidden; }
.oc-head {
  display: flex; align-items: center; gap: 12px; width: 100%; padding: 13px 16px;
  background: transparent; border: none; cursor: pointer; text-align: left;
}
.oc-head:hover { background: color-mix(in srgb, var(--primary) 5%, transparent); }
.oc-name { font-size: 14px; font-weight: 640; color: var(--ink); }
.oc-ind { font-size: 11px; color: var(--primary); }
.oc-cnt { margin-left: auto; font-size: 12px; color: var(--muted); font-variant-numeric: tabular-nums; }
.oc-chev { color: var(--muted); transition: transform 0.2s; }
.oc-chev.flip { transform: rotate(180deg); }
.oc-body { padding: 4px 12px 12px; border-top: 1px solid var(--hairline); }
.triple-list { display: flex; flex-direction: column; }
.triple-row {
  display: flex; align-items: center; gap: 8px; padding: 8px 6px; font-size: 12.5px;
  border-bottom: 1px solid var(--hairline);
}
.triple-row:last-child { border-bottom: none; }
.tr-sub, .tr-obj { color: var(--ink); font-weight: 560; }
.tr-rel {
  font-size: 11px; color: var(--primary); padding: 1px 8px; border-radius: 99px;
  background: color-mix(in srgb, var(--primary) 11%, transparent); white-space: nowrap;
}
.tr-src {
  margin-left: auto; font-size: 10.5px; color: var(--dim);
  font-family: ui-monospace, Consolas, monospace; max-width: 160px; overflow: hidden;
  text-overflow: ellipsis; white-space: nowrap;
}
.tr-conf { font-size: 10.5px; color: var(--muted); font-variant-numeric: tabular-nums; flex: none; }
.core-empty {
  display: flex; flex-direction: column; align-items: center; gap: 12px; text-align: center;
  padding: 48px 40px; color: var(--muted);
}
.core-empty :deep(svg) { color: var(--primary); }
.core-empty p { font-size: 13px; max-width: 420px; line-height: 1.7; margin: 0; }
</style>