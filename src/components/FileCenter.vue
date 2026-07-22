<script setup lang="ts">
/**
 * 文件中心 —— 知识库内的可视化文件库(《文件中心-PRD》落地)。
 *
 * 三视图:网格画廊(缩略图/首帧/类型图标占大位)· 聚类星图(语义簇)· 列表。
 * 琉璃质感 + 苹果式透明:毛玻璃面板、accent 光环、悬浮升起、缩略图懒加载。
 * 数据全部复用检索枢纽 fable.db,聚类复用已存向量(零新增嵌入),缩略图磁盘缓存。
 *
 * 结构:本文件是编排层(跨视图共享状态 + 数据加载 + 任务编排),
 * 各视图/浮层拆在 ./filecenter/ 子组件里(共享类型/纯函数见 ./filecenter/shared.ts);
 * 非默认视图(列表/分类树/远程源/核心层)全部懒加载,首开只解析默认的画廊。
 */
import { ref, reactive, computed, onMounted, onBeforeUnmount, nextTick, watch, defineAsyncComponent } from "vue";
import {
  Radar,
  FolderSearch,
  X,
  Wand2,
  KeyRound,
  Brain,
  Sparkles,
  WifiOff,
} from "@lucide/vue";
import OrbitSpinner from "./icons/OrbitSpinner.vue";
import {
  files as fc,
  artifacts as artifactsApi,
  invoke,
  type FileOverview,
  type FileCard,
  type FcCluster,
} from "../tauri";
import { useAppStore } from "../stores/app";
import { useWizardStore } from "../stores/wizard";
import { useFileTasksStore } from "../stores/fileTasks";
import { loadRemoteSources, type RemoteSource } from "../features/interconnect/remoteSources";
import { prewarmScan, invalidateScanPrewarm } from "../lib/scanPrewarm";
import { GLYPHS, nameOf, type ViewKind } from "./filecenter/shared";
// 常驻子组件(默认画廊视图链路,同步解析)
import FcBanner from "./filecenter/FcBanner.vue";
import FcToolbar from "./filecenter/FcToolbar.vue";
import FilterPanel from "./filecenter/FilterPanel.vue";
import GalleryPane from "./filecenter/GalleryPane.vue";
import DetailDrawer from "./filecenter/DetailDrawer.vue";
import FolderPickerModal from "./filecenter/FolderPickerModal.vue";
import GalaxyOverlay from "./filecenter/GalaxyOverlay.vue";
// 非默认视图按需加载:首开只解析画廊,切到对应视图才拉代码。
const ListPane = defineAsyncComponent(() => import("./filecenter/ListPane.vue"));
const ClusterTreePane = defineAsyncComponent(() => import("./filecenter/ClusterTreePane.vue"));
const RemotePane = defineAsyncComponent(() => import("./filecenter/RemotePane.vue"));
// 核心层 = 整套知识库(llmwiki)本体:点「核心层」tab 直接内嵌完整知识库,体验与独立知识库一致。
// 按需加载,只在切到核心层时才挂载、初始化。
const WikiBrowse = defineAsyncComponent(() => import("./WikiBrowse.vue"));
// 盘管理:NAS 网络盘的记忆与一键映射,按需加载(只在点开时才挂载)。
const NasManager = defineAsyncComponent(() => import("./NasManager.vue"));
const nasOpen = ref(false);

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
const view = ref<ViewKind>("gallery");
// 列表/分类树懒挂载:首开不解析,第一次切到该视图才挂;之后 v-show 保活(滚动位置/展开态不丢)。
const listVisited = ref(false);
const treeVisited = ref(false);
watch(
  view,
  (v) => {
    if (v === "list") listVisited.value = true;
    if (v === "clusters") treeVisited.value = true;
  },
  { immediate: true },
);

// ── 远程源(经 iroh 隧道接入的 NAS/主机):像本机一样浏览、下载它的盘 ──
// 浏览态(当前路径/条目)下沉到 RemotePane;这里只记「选了哪台」+ 打开计数(同源再点也回根目录)。
const remoteSources = ref<RemoteSource[]>(loadRemoteSources());
const remoteSel = ref<RemoteSource | null>(null);
const remoteTick = ref(0);
function pickRemote(s: RemoteSource) {
  remoteSources.value = loadRemoteSources();
  remoteSel.value = s;
  remoteTick.value += 1;
  view.value = "remote";
}

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
void loadCore; void toggleSchema; void ontoSchemasWithData; void openFullKb; void ontoLoading; void ontoTotal; // 旧核心层视图暂未挂载,保留能力

const overview = ref<FileOverview | null>(null);
const cards = ref<FileCard[]>([]);
const page = ref(0);
const total = ref(0);
const loading = ref(false);
const exhausted = ref(false);
// reset 请求撞上飞行中的分页加载时,不能直接丢弃(否则归类/盘点完成的 watch 调 loadGrid(true)
// 会被早退,filter 已清空但网格残留旧数据)。先记下,等当前加载收尾立刻补跑一次重载。
const pendingReset = ref(false);

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
// 操作轻提示统一走 flashOp:9s 自动消隐。opMsg 若常驻,要么被常驻的任务文案永久遮蔽
// (旧模板 buildMsg||scanMsg||opMsg,盘点跑过一次 scanMsg 恒非空,打开失败/检索失败全看不见),
// 要么反过来永久遮蔽任务进度 —— 只有「短暂置顶」才两全。
let opMsgTimer: ReturnType<typeof setTimeout> | null = null;
function flashOp(msg: string) {
  opMsg.value = msg;
  if (opMsgTimer) clearTimeout(opMsgTimer);
  opMsgTimer = setTimeout(() => (opMsg.value = ""), 9000);
}
// 盘点完成但有根「连不上、已跳过」时,这里存这些路径 → 弹温和提示框(见模板 .fc-alert)。
const unreachableNotice = ref<string[]>([]);
function dismissUnreachable() {
  unreachableNotice.value = [];
}
function retryUnreachable() {
  const roots = [...unreachableNotice.value];
  unreachableNotice.value = [];
  if (roots.length) doScan(roots, []);
}

// ── 「盘点」= 先扫一眼文件夹结构 → 勾选要盘点的目录(不限知识库)→ 建库 ──
// 选择器本体(扫描/勾选/懒加载下钻)整个下沉到 FolderPickerModal;这里只管开关 + 接「开始盘点」。
const pickerOpen = ref(false);
const pickerBusy = ref(false);
function openFolderPicker() {
  if (pickerBusy.value || scanning.value) return;
  pickerOpen.value = true;
}
function closeFolderPicker() {
  pickerOpen.value = false;
}
function onPickerStart(roots: string[], exclude: string[], full: boolean) {
  pickerOpen.value = false;
  doScan(roots, exclude, full);
}

// 归类(离线纯数学)/ AI 语义归类 / 建索引 —— 全读 store(运行态 + 进度文案)。
const clustering = computed(() => tasks.running.cluster);
const clusterMsg = computed(() => tasks.detail.cluster);
const building = computed(() => tasks.running.index);
const buildMsg = computed(() => tasks.detail.index);
const llmClustering = computed(() => tasks.running.clusterLlm);
const llmMsg = computed(() => tasks.detail.clusterLlm);
// 星图重挂载键:归类收尾后 key 变 → KnowledgeGraph 重载新数据,星图原地升级。
// **归类进行中不推进**:tier 一档一跳,每跳都整图销毁重拉重排(大图秒级 fcose),
// 星图开着会反复闪烁抖动;等 done(clustering=false)后一次性升级。
const graphRefreshKey = ref(0);
watch(
  () => tasks.doneTick.cluster + tasks.doneTick.clusterLlm,
  () => {
    if (!tasks.clustering) graphRefreshKey.value++;
  },
);

// 语义文件夹下钻路径(目前两级:[] = 顶层主题;[topId] = 某主题内看子主题)
const folderPath = ref<number[]>([]);

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

// 选中详情(速览/大图的拉取下沉到 DetailDrawer,这里只记选中卡 + 缓存里已有的缩略图种子)
const selected = ref<FileCard | null>(null);
const detailSeed = ref<string | null>(null);

// 缩略图缓存:abspath → dataURL('' = 已尝试但无图)。
// 关键内存治理:每条 value 是 360px 的 base64 data URL(~30–150KB),旧实现是无上限
// reactive Map 且永不清理 —— 滚一个十万图的库就把几 GB base64 永久钉在 WebView 堆里
// (Mac WKWebView 回收又懒),是桌面内存膨胀的主因之一。改成带上限的 LRU:
// Map 的迭代序即插入序,fetchThumb 命中缺失才重取并重新插到队尾 → 插入序≈最近使用序,
// 超 CAP 时从队首淘汰最久未用的。被淘汰项再进视口会重取(服务端已落盘 jpg,极快),
// 以「偶尔重取」换「内存恒定」。CAP 取 600:足够覆盖几屏视口 + 缓冲,峰值约几十 MB。
const THUMB_CACHE_CAP = 600;
const thumbCache = reactive(new Map<string, string>());
const thumbPending = new Set<string>();
function thumbCacheSet(key: string, val: string) {
  // 重取已存在的键:先删再插,使其移到迭代序队尾(刷新为「最近使用」)。
  if (thumbCache.has(key)) thumbCache.delete(key);
  thumbCache.set(key, val);
  // 超额淘汰:从队首(最久未用)删,直到回到上限。
  while (thumbCache.size > THUMB_CACHE_CAP) {
    const oldest = thumbCache.keys().next().value;
    if (oldest === undefined) break;
    thumbCache.delete(oldest);
  }
}

let searchTimer: ReturnType<typeof setTimeout> | null = null;

// ── 虚拟滚动:画廊/列表只在 DOM 里保留视口内的几十张卡片,几十万文件也不卡 ──
// 旧实现把 cards 全量 v-for 进 DOM(无限滚动一直 concat),470K 库滚几屏就上千节点、
// 每张卡片读响应式 thumbCache + 内联 :ref,任何一次缩略图回填都触发整片重渲染 → 卡死。
// 改成 RecycleScroller:画廊走 grid 模式(列数随宽度算)、列表走定高行,常数级 DOM。
const bodyEl = ref<HTMLElement | null>(null);
const bodyW = ref(0);
let bodyRO: ResizeObserver | null = null;
const GALLERY_MIN_CELL = 196; // 单元格(含间距)最小宽度
const GALLERY_CELL_H = 208; // 单元格(含间距)固定高度:缩略图 + 元信息 + 间距
// 可用内容宽:扣掉滚动条 padding 余量(~20px),避免 cols×cellW 溢出反而催出横向滚动条
const galleryInnerW = computed(() => Math.max(GALLERY_MIN_CELL, bodyW.value - 20));
const galleryCols = computed(() => Math.max(1, Math.floor(galleryInnerW.value / GALLERY_MIN_CELL)));
const galleryCellW = computed(() => Math.floor(galleryInnerW.value / galleryCols.value));

// 视口内(含缓冲)范围的缩略图按需预取;只对当前渲染的几十张发起,且 fetchThumb 自带去重。
function onGalleryRange(start: number, end: number) {
  for (let i = start; i < end && i < cards.value.length; i++) {
    const c = cards.value[i];
    if (c && c.thumbable && !thumbCache.has(c.abspath)) fetchThumb(c);
  }
}
// 滚到底(RecycleScroller 在末项进入缓冲区时触发,配合 buffer 提前预载)→ 续翻下一页。
function onGridEnd() {
  if (view.value === "gallery" || view.value === "list") loadGrid(false);
}
// 内容区(.fc-body)挂载/卸载时挂上/摘下 ResizeObserver:量宽度供画廊算列数。
// .fc-body 在空库时不渲染,故不能只在 onMounted 挂一次——用 watch 跟随其出现。
watch(bodyEl, (el) => {
  bodyRO?.disconnect();
  bodyRO = null;
  if (!el) return;
  bodyW.value = el.clientWidth;
  bodyRO = new ResizeObserver((entries) => {
    const w = entries[0]?.contentRect.width ?? 0;
    if (w > 0) bodyW.value = w;
  });
  bodyRO.observe(el);
});

// 任务完成(store doneTick 自增)→ 刷新文件中心数据,让新盘点/新索引/新归类结果显示出来。
// 监听全局 store,故即便任务是在别的视图发起、本组件后挂载,回来也会因 tick 变化而刷新。
watch(() => tasks.doneTick.inventory, () => {
  loadOverview(); loadGrid(true); ensureLangBackfill();
  invalidateScanPrewarm(); // 盘点后结构/大小变了,预热缓存作废重拉
  // 本轮盘点里「连不上、已跳过」的根(群晖 NAS / 拔掉的外置盘)→ 弹个温和提示框,提醒这些没扫到,
  // 别让用户误以为「盘点完成 = 全都扫到了」。能连上时数组为空,提示框不出现。
  unreachableNotice.value = [...tasks.lastUnreachable];
});
watch(() => tasks.doneTick.index, () => { loadOverview(); });
watch(
  () => tasks.doneTick.cluster + tasks.doneTick.clusterLlm,
  () => {
    // v3 智能归类每完成一档(骨架/AI/语义)都 tier→tick 自增:任务**还在跑**时只刷
    // 侧栏总览,既不清导航、也不 loadGrid(true) —— 后者会清空画廊并把虚拟滚动拽回顶部,
    // 归类期间用户正翻着的画廊每档被拽回第一屏。收尾那次才全量重载。
    if (!tasks.clustering) {
      folderPath.value = [];
      activeCluster.value = null;
      loadGrid(true);
    }
    loadOverview();
  },
);
watch(() => tasks.doneTick.titles, () => { loadGrid(true); });

// 簇 id → 簇(颜色/名称),传给画廊/列表/详情做染色与归类标签。
const clusterColor = computed<Record<number, FcCluster>>(() => {
  const m: Record<number, FcCluster> = {};
  for (const c of overview.value?.clusters ?? []) m[c.id] = c;
  return m;
});

// ───────────────────────── 语义文件夹(父层只留筛选编排要用的部分) ─────────────────────────
const allClusters = computed<FcCluster[]>(() => overview.value?.clusters ?? []);
function hasChildren(id: number): boolean {
  return allClusters.value.some((c) => c.parent === id);
}

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
  if (loading.value) {
    // 飞行中又来了重载请求:记下,等收尾补跑(分页 loadGrid(false) 撞上不算,无需重载)。
    if (reset) pendingReset.value = true;
    return;
  }
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
    // 加载期间有被搁置的重载请求(filter 已变)→ 用最新 filter 重跑,覆盖刚拼进来的旧数据。
    if (pendingReset.value) {
      pendingReset.value = false;
      loadGrid(true);
    }
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
async function doScan(roots: string[], exclude: string[], full = false) {
  // 真身在全局 store:后台线程跑、事件 App 级监听。切走文件中心也不中断,完成由
  // watch(doneTick.inventory) 刷新本页;全局任务中心浮层全程显示进度。
  // full=false(默认)走智能增量(只摸 mtime 变过的子树,重扫快一个数量级);full=true 逐目录完整重扫。
  await tasks.startInventory(roots, exclude, full);
}

// 以下任务全部委托给全局 store(后台线程 + App 级事件监听):切走文件中心也不中断,
// 完成由顶部 watch(doneTick.*) 刷新本页;全局任务中心浮层全程显示「还在跑 + 进度」。

// 构建向量索引(文本 → 硅基 BGE-M3 嵌入),让「智能归类」从词法升级到语义。
async function buildIndex() {
  if (!overview.value?.hasEmbedProvider) {
    app.setView("sense_api"); // 没配嵌入 key → 跳设置页配硅基(免费)
    return;
  }
  await tasks.startIndex();
}

// 合并后的统一「智能归类」入口:
//  · 配了硅基流动嵌入 key(hasEmbedProvider)→ AI 语义归类(复用已有向量);
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
    flashOp("已撤销 AI 名称,回落到本地清洗名");
    loadGrid(true);
  } catch (e: any) {
    flashOp(`撤销失败:${e?.message ?? e}`);
  }
}

// ───────────────────────── 语义检索 ─────────────────────────
async function runSemantic() {
  if (semBusy.value) return; // 防并发:快速连按会让先发的慢响应后到、覆盖新词的结果
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
    flashOp(`检索失败:${e?.message ?? e}`);
  } finally {
    semBusy.value = false;
  }
}
function clearSemantic() {
  semActive.value = false;
  semHits.value = [];
}
// 工具条搜索框「×」清空:清词 + 收语义带 + 回普通筛选。
function onClearSearch() {
  clearSemantic();
  applyFilters();
}

// ───────────────────────── 缩略图懒加载 ─────────────────────────
async function fetchThumb(card: FileCard) {
  if (!card.thumbable) return;
  if (thumbCache.has(card.abspath) || thumbPending.has(card.abspath)) return;
  thumbPending.add(card.abspath);
  try {
    const url = await fc.thumb(card.abspath, 360);
    thumbCacheSet(card.abspath, url ?? "");
  } catch {
    thumbCacheSet(card.abspath, "");
  } finally {
    thumbPending.delete(card.abspath);
  }
}

// 首屏/翻页后批量预热缩略图(服务端多线程生成 jpg,后续 fc.thumb 命中即快)。
function warmVisible() {
  const slice = cards.value.filter((c) => c.thumbable && !thumbCache.has(c.abspath)).slice(0, 24);
  if (slice.length) fc.warmThumbs(slice.map((c) => c.abspath), 360).catch(() => {});
}

// ───────────────────────── 详情 ─────────────────────────
function openDetail(card: FileCard) {
  detailSeed.value = thumbCache.get(card.abspath) || null;
  selected.value = card;
}
function closeDetail() {
  selected.value = null;
}
async function openPath(abspath: string) {
  try {
    await artifactsApi.openExternal(abspath);
  } catch (e: any) {
    flashOp(`打开失败:${e?.message ?? e}`);
  }
}

// ───────────────────────── 辅助 ─────────────────────────
const hasFiles = computed(() => (overview.value?.totalFiles ?? 0) > 0);

onMounted(async () => {
  remoteSources.value = loadRemoteSources(); // 互联页新接入的远程源同步进来
  // 互联页设备卡点了「使用盘」→ 带着目标远程源直接落进远程浏览
  const want = sessionStorage.getItem("polaris.fc.openRemote");
  if (want) {
    sessionStorage.removeItem("polaris.fc.openRemote");
    const rs = remoteSources.value.find((s) => s.id === want);
    if (rs) pickRemote(rs);
  }
  await loadOverview();
  await loadGrid(true);
  // 后台补齐文稿自然语言(不阻塞首屏;代码/媒体的按语言归类已即时可用)
  ensureLangBackfill();
  await nextTick();
  // 进入文件中心且库还是空的 → 立刻拉起「让 AI 更懂你」引导,用户一点进来就被带着走完
  // 盘点 → 语义归类 → 图谱 → 建索引。库一旦有内容(说明引导过/手动盘过)就不再打扰。
  if ((overview.value?.totalFiles ?? 0) === 0) openWizard();
  // 空闲预热盘点选择器(扫根+第一层+低并发预算大小),点「盘点」秒开(scanPrewarm)。
  const idle = (window as any).requestIdleCallback ?? ((f: () => void) => setTimeout(f, 300));
  idle(() => prewarmScan());
});

onBeforeUnmount(() => {
  bodyRO?.disconnect();
  bodyRO = null;
  // 任务事件监听已托管给全局 fileTasks store(App 级注册一次,永不在此退订)——
  // 这正是「切走文件中心,盘点/建索引/归类仍在后台跑、进度不丢」的关键。
  if (tipTimer) clearTimeout(tipTimer);
  if (opMsgTimer) clearTimeout(opMsgTimer);
  // 内存治理:切走文件中心即主动清空缩略图缓存(几百条 base64 data URL,可达几十 MB),
  // 让 WebView 立刻能回收,而不是等组件实例被 GC 才释放(Mac WKWebView 回收偏懒)。
  // 回到文件中心时视口内缩略图会按需重取(服务端已落盘 jpg,极快)。
  thumbCache.clear();
  thumbPending.clear();
  cards.value = [];
});
</script>

<template>
  <div class="fc">
    <!-- 顶部琉璃横幅(可收起)—— 核心层是知识库本体,不要文件库的横幅统计 -->
    <FcBanner v-show="view !== 'core'" :overview="overview" />

    <!-- 工具条 -->
    <FcToolbar
      v-model:view="view"
      v-model:search="searchText"
      :overview="overview"
      :remote-sources="remoteSources"
      :remote-sel-id="remoteSel?.id ?? null"
      :sort="sort"
      :sem-busy="semBusy"
      :scanning="scanning"
      :picker-busy="pickerBusy"
      :clustering="clustering"
      :llm-clustering="llmClustering"
      :building="building"
      :llm-titling="llmTitling"
      @pick-remote="pickRemote"
      @search-input="onSearchInput"
      @clear-search="onClearSearch"
      @semantic="runSemantic"
      @set-sort="setSort"
      @open-wizard="openWizard"
      @open-galaxy="galaxyOpen = true"
      @open-picker="openFolderPicker"
      @smart-cluster="doSmartCluster"
      @build-index="buildIndex"
      @titles="doTitlesLlm"
      @open-nas="nasOpen = true"
    />

    <!-- 盘管理:NAS 网络盘的记忆与一键映射 -->
    <NasManager v-if="nasOpen" @close="nasOpen = false" />

    <!-- AI 整理名称进度 -->
    <div v-if="titleMsg && view !== 'core'" class="fc-llm">
      <Sparkles :size="14" :stroke-width="1.8" class="llm-ic" />
      <span class="llm-text">{{ titleMsg }}</span>
      <button v-if="titleDone" class="link-btn" @click="resetTitles">撤销 AI 名称</button>
    </div>

    <!-- AI 归类进度(旧向导 file:cluster_llm 路径) -->
    <div v-if="llmMsg && view !== 'core'" class="fc-llm">
      <Brain :size="14" :stroke-width="1.8" class="llm-ic" />
      <span class="llm-text">{{ llmMsg }}</span>
    </div>

    <!-- 智能归类(v3 渐进式:骨架→AI 命名→后台语义精修)进度 -->
    <div v-if="clustering && !llmMsg && view !== 'core'" class="fc-llm">
      <Brain :size="14" :stroke-width="1.8" class="llm-ic" />
      <span class="llm-text">{{ clusterMsg || "智能归类已就绪" }}</span>
    </div>

    <!-- 归类小提示:默认不常驻,只在点「智能归类 / AI 归类」时弹一下解释能力 -->
    <transition name="tip">
      <div v-if="tip.show && hasFiles && view !== 'core'" class="fc-tip">
        <button class="tip-x" title="关闭" @click="closeTip"><X :size="12" :stroke-width="2" /></button>
        <template v-if="tip.kind === 'cluster'">
          <Wand2 :size="14" :stroke-width="1.8" class="tip-ic" />
          <span class="tip-body">
            <template v-if="overview?.hasEmbedProvider">
              <b>三步走</b>:先<b>秒级</b>按结构出星图骨架 → AI 读懂你的资料、起<b>亲切名字</b>并理清主题间关系 → 后台把<b>全部资料向量化</b>后再按<b>内容语义</b>精修一次(可关页面)。
            </template>
            <template v-else>
              先<b>秒级</b>按结构归好、AI 起<b>亲切名字</b>。到设置页配硅基 key(<b>免费</b>)后,还会在后台把全部资料向量化、按<b>内容语义</b>再精修一次。
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
            <OrbitSpinner v-if="building" :size="12" />
            <Radar v-else :size="12" :stroke-width="1.8" />
            {{ overview.embeddedFiles > 0 ? "续建索引" : "建索引" }}
          </button>
        </template>
        <template v-else>
          <Brain :size="14" :stroke-width="1.8" class="tip-ic ai" />
          <span class="tip-body">
            先<b>秒级</b>出星图骨架 → AI 读懂你的资料、起<b>亲切名字</b>并理清关系 → 后台向量化全部资料后再按<b>语义</b>精修一次。
          </span>
        </template>
      </div>
    </transition>

    <!-- 即时操作提示(9s 自动消隐)短暂置顶;平时显示任务进度文案 -->
    <div v-if="(scanMsg || buildMsg || opMsg) && view !== 'core'" class="fc-note">{{ opMsg || buildMsg || scanMsg }}</div>

    <!-- 筛选面板:语义文件夹(两级下钻)+ 按类型 + 按语言 -->
    <FilterPanel
      v-if="view !== 'core'"
      :overview="overview"
      :has-files="hasFiles"
      :folder-path="folderPath"
      :active-cluster="activeCluster"
      :active-kind="activeKind"
      :active-lang="activeLang"
      @open-folder="openFolder"
      @view-whole-folder="viewWholeFolder"
      @folder-home="folderHome"
      @pick-kind="pickKind"
      @pick-lang="pickLang"
    />

    <!-- 核心层 = 知识库本体:直接内嵌整套 llmwiki 知识库(就是你资料的大脑),
         体验与独立「知识库」完全一致,只是住在文件中心里。空库与否都照常进知识库。 -->
    <WikiBrowse v-if="view === 'core'" class="fc-core-wiki" />

    <!-- 远程源:经 iroh 隧道浏览对端(NAS/主机)的盘 -->
    <RemotePane v-else-if="view === 'remote' && remoteSel" :source="remoteSel" :open-tick="remoteTick" />

    <!-- 空库引导 -->
    <div v-else-if="!hasFiles" class="fc-empty glass">
      <div class="empty-orb"><FolderSearch :size="30" :stroke-width="1.3" /></div>
      <div class="empty-title">文件中心还是空的</div>
      <div class="empty-sub">点「盘点」先扫一眼文件夹,勾选要盘点的目录(可选知识库之外的盘符/文件夹),建成可视化文件库;<br />已嵌入文本可再点「智能归类」把相似数据自动放在一起。</div>
      <button class="empty-cta" :disabled="scanning || pickerBusy" @click="openFolderPicker">
        <OrbitSpinner v-if="scanning || pickerBusy" :size="15" />
        <FolderSearch v-else :size="15" :stroke-width="1.8" />
        <span>{{ scanning ? "盘点中…" : "立即盘点" }}</span>
      </button>
    </div>

    <!-- 内容区 -->
    <div v-else ref="bodyEl" class="fc-body">
      <!-- 语义检索结果带 -->
      <div v-if="semActive" class="sem-strip">
        <div class="sem-head">
          <Radar :size="14" :stroke-width="1.8" />
          <span>语义检索:「{{ searchText }}」</span>
          <button class="sem-close" @click="clearSemantic"><X :size="13" :stroke-width="2" /> 收起</button>
        </div>
        <div v-if="semBusy" class="sem-loading"><OrbitSpinner :size="16" /> 检索中…</div>
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

      <!-- 画廊(默认视图,常驻同步解析;v-show 保活滚动位置)-->
      <GalleryPane
        v-show="view === 'gallery'"
        :cards="cards"
        :cell-h="GALLERY_CELL_H"
        :cols="galleryCols"
        :cell-w="galleryCellW"
        :thumb-cache="thumbCache"
        :cluster-by-id="clusterColor"
        :loading="loading"
        :exhausted="exhausted"
        :total="total"
        @range="onGalleryRange"
        @end="onGridEnd"
        @open="openDetail"
      />

      <!-- 分类树状图(懒挂载:第一次切到才解析,之后 v-show 保活展开态)-->
      <ClusterTreePane
        v-if="treeVisited"
        v-show="view === 'clusters'"
        :clusters="overview?.clusters ?? []"
        :active-cluster="activeCluster"
        :clustering="clustering"
        :llm-clustering="llmClustering"
        :can-cluster="!!overview?.totalFiles"
        @view-whole-folder="viewWholeFolder"
        @smart-cluster="doSmartCluster"
      />

      <!-- 列表(懒挂载:第一次切到才解析,之后 v-show 保活滚动位置)-->
      <ListPane
        v-if="listVisited"
        v-show="view === 'list'"
        :cards="cards"
        :cluster-by-id="clusterColor"
        :loading="loading"
        @end="onGridEnd"
        @open="openDetail"
      />
    </div>

    <!-- 详情抽屉(含遮罩;速览/大图在组件内按需拉取) -->
    <DetailDrawer
      :card="selected"
      :cluster-by-id="clusterColor"
      :seed-thumb="detailSeed"
      @close="closeDetail"
      @msg="opMsg = $event"
    />

    <!-- 连不上的根(群晖 NAS / 拔掉的外置盘)温和提示框:盘点完成后弹出,可重试 / 知道了 -->
    <transition name="fade">
      <div v-if="unreachableNotice.length" class="fc-alert-scrim" @click="dismissUnreachable">
        <div class="fc-alert glass" @click.stop>
          <div class="fc-alert-head">
            <WifiOff :size="18" :stroke-width="1.8" class="fc-alert-ic" />
            <span>有 {{ unreachableNotice.length }} 个位置这次没连上</span>
          </div>
          <div class="fc-alert-body">
            下面这些位置盘点时连接不上,已自动跳过(其它盘已正常盘点、不受影响):
            <ul class="fc-alert-list">
              <li v-for="p in unreachableNotice" :key="p">{{ p }}</li>
            </ul>
            常见原因:群晖 NAS 没开机 / Tailscale 没连上 / 外置盘没插好。处理后点「重试这些」即可补扫。
          </div>
          <div class="fc-alert-foot">
            <button class="fc-alert-btn ghost" @click="dismissUnreachable">知道了</button>
            <button class="fc-alert-btn solid" @click="retryUnreachable">重试这些</button>
          </div>
        </div>
      </div>
    </transition>

    <!-- 文件夹选择器:盘点前先扫一眼,取消勾选不要的文件夹 -->
    <transition name="fade">
      <FolderPickerModal
        v-if="pickerOpen"
        @close="closeFolderPicker"
        @busy="pickerBusy = $event"
        @start="onPickerStart"
      />
    </transition>

    <!-- 星图浮层:文件库 → 星河图谱(全屏沉浸) -->
    <transition name="fade">
      <GalaxyOverlay v-if="galaxyOpen" :refresh-key="graphRefreshKey" @close="galaxyOpen = false" />
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

/* ── 主体 ── */
.fc-body {
  flex: 1;
  min-height: 0;
  /* 改为 flex 列 + 自身不滚:由内部各视图(画廊/列表虚拟滚动、分类树)各自滚动,
     这样虚拟滚动容器才有确定高度,DOM 只保留视口内的卡片。 */
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

/* 核心层 = 内嵌整套知识库(WikiBrowse):占满文件中心剩余高度,
   覆盖 .wiki 自带的 height:100%(否则会顶出工具条溢出)。 */
.fc-core-wiki {
  flex: 1 1 auto;
  min-height: 0;
  height: auto;
}

/* 语义带 */
.sem-strip {
  flex: none;
  background: color-mix(in srgb, var(--primary) 7%, var(--panel));
  border: 1px solid color-mix(in srgb, var(--primary) 22%, transparent);
  border-radius: 14px;
  padding: 12px 14px;
  margin: 4px 6px 12px;
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

/* 字形通用(语义带用;完整版见画廊/列表/详情组件) */
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

/* 连不上的根:温和提示框(比 picker 更靠上,z-index 40) */
.fc-alert-scrim {
  position: absolute;
  inset: 0;
  z-index: 40;
  display: flex;
  align-items: center;
  justify-content: center;
  background: color-mix(in srgb, var(--bg) 50%, transparent);
  -webkit-backdrop-filter: blur(3px);
  backdrop-filter: blur(3px);
  padding: 24px;
}
.fc-alert {
  width: min(440px, 100%);
  padding: 18px 20px 14px;
  border-radius: 16px;
  box-shadow: var(--shadow-lg, 0 24px 60px -20px rgba(0, 0, 0, 0.45));
}
.fc-alert-head {
  display: flex;
  align-items: center;
  gap: 9px;
  font-family: var(--serif);
  font-size: 15.5px;
  letter-spacing: 0.5px;
  color: var(--ink);
}
.fc-alert-ic { color: var(--gold, #d4b06a); flex: none; }
.fc-alert-body { margin: 11px 0 4px; font-size: 12.8px; line-height: 1.7; color: var(--muted); }
.fc-alert-list {
  margin: 8px 0;
  padding: 9px 12px;
  list-style: none;
  border-radius: 10px;
  background: color-mix(in srgb, var(--panel) 55%, transparent);
  border: 1px solid var(--border-soft);
  max-height: 168px;
  overflow: auto;
}
.fc-alert-list li {
  font-family: var(--mono, monospace);
  font-size: 12px;
  color: var(--text);
  word-break: break-all;
  padding: 2px 0;
}
.fc-alert-foot { display: flex; justify-content: flex-end; gap: 9px; margin-top: 14px; }
.fc-alert-btn {
  padding: 7px 16px;
  border-radius: 9px;
  font-size: 13px;
  cursor: pointer;
  border: 1px solid var(--border);
  transition: background 0.15s, border-color 0.15s, transform 0.12s;
}
.fc-alert-btn:active { transform: translateY(1px); }
.fc-alert-btn.ghost { background: transparent; color: var(--muted); }
.fc-alert-btn.ghost:hover { color: var(--text); background: var(--selection-bg); }
.fc-alert-btn.solid {
  background: var(--btn-solid-bg, var(--gold, #d4b06a));
  color: var(--btn-solid-text, #1a1a1a);
  border-color: transparent;
}
.fc-alert-btn.solid:hover { filter: brightness(1.06); }

/* ── 动效 ── */
.spin { animation: spin 0.9s linear infinite; }
@keyframes spin { to { transform: rotate(360deg); } }
.fade-enter-active, .fade-leave-active { transition: opacity 0.26s; }
.fade-enter-from, .fade-leave-to { opacity: 0; }
</style>
