<script setup lang="ts">
import { ref, onMounted, computed, watch } from "vue";
import { storeToRefs } from "pinia";
import { marked } from "marked";
import { sanitizeHtml } from "../lib/sanitize";
import { RecycleScroller } from "vue-virtual-scroller";
import "vue-virtual-scroller/dist/vue-virtual-scroller.css";
import {
  Upload,
  LoaderCircle,
  CheckCircle2,
  XCircle,
  X,
  Trash2,
  Download,
  Sparkles,
  Waypoints,
} from "@lucide/vue";
import {
  kb,
  scan,
  type KbHit,
  type KbPack,
  type ScanRoot,
  type ScanRow,
  artifacts as artifactsApi,
  type ArtifactSearchHit,
} from "../tauri";
import { useAppStore } from "../stores/app";
import { useArtifactsStore } from "../stores/artifacts";
import { useKbStore } from "../stores/kb";
import { useFileDrop } from "../composables/useFileDrop";

const app = useAppStore();
const artifactsStore = useArtifactsStore();
const kbStore = useKbStore();

type Tab = "overview" | "packs" | "browse" | "manage";
const tab = ref<Tab>("browse");
const files = ref<string[]>([]);
const selected = ref<string | null>(null);
const markdown = ref("");
// 知识库 .md 含 AI 生成 / 抓取的网页 / 导入文档(不可信来源)，必须过 DOMPurify，
// 否则 markdown 内嵌的 <img onerror> 等可在特权 webview 触发 XSS。
const rendered = computed(() =>
  markdown.value ? sanitizeHtml(marked.parse(markdown.value) as string) : "",
);
const query = ref("");
const hits = ref<KbHit[]>([]);
// 搜索结果首屏只渲染 100 条,「展示更多」逐段放开(大结果集不再一次性铺 DOM)
const hitCap = ref(100);
// 历史对话产物命中（搜索记忆把过往输出文件也算入）
const artHits = ref<ArtifactSearchHit[]>([]);
const rootPath = ref("");
const scanned = ref<number | null>(null);
const ingestPath = ref("");
const ingestMsg = ref("");

// ── 构建知识网 (摄入即编译) ──
// 进度/正在跑/日志全部落在全局 kbStore: 离开 wiki 视图(组件卸载)后台照常累积,
// 切回来即见进度,不会因组件销毁而清零或"看起来停了"。
const {
  compiling,
  compileLog,
  compileMsg,
  pipelineStage,
  lastDocCount,
  doneTick,
  linting,
  lintReport,
  scanning,
  threatReport,
} = storeToRefs(kbStore);

onMounted(async () => {
  rootPath.value = await kb.root();
  // 若离开期间已有正在运行/完成的编译,重挂时确保监听在位并同步计数
  await kbStore.ensureListener();
  if (lastDocCount.value != null) scanned.value = lastDocCount.value;
  await Promise.all([refreshList(), refreshPacks()]);
});

// ─────────── 名人资料包（下载到自己的资料库，附带配套 skill） ───────────
const packs = ref<KbPack[]>([]);
const packBusy = ref<string | null>(null); // 正在装/卸的 pack id
const packMsg = ref("");

async function refreshPacks() {
  try {
    packs.value = await kb.packList();
  } catch {
    packs.value = [];
  }
}

async function installPack(p: KbPack) {
  packBusy.value = p.id;
  packMsg.value = "";
  try {
    await kb.packInstall(p.id);
    packMsg.value = `「${p.name}」已装入资料库,配套技能「${p.skillId}」已同步安装`;
    await Promise.all([refreshPacks(), refreshList()]);
  } catch (e: any) {
    packMsg.value = `安装失败:${e?.message ?? e}`;
  } finally {
    packBusy.value = null;
  }
}

async function removePack(p: KbPack) {
  if (!confirm(`移除「${p.name}」资料包?\n会删除 raw/${p.name}/ 下全部资料,并卸载配套技能。`))
    return;
  packBusy.value = p.id;
  packMsg.value = "";
  try {
    await kb.packRemove(p.id);
    packMsg.value = `「${p.name}」已移除`;
    await Promise.all([refreshPacks(), refreshList()]);
  } catch (e: any) {
    packMsg.value = `移除失败:${e?.message ?? e}`;
  } finally {
    packBusy.value = null;
  }
}

// 编译完成(后台也可能在别的视图触发)→ 刷新文件列表与计数
watch(doneTick, () => {
  if (lastDocCount.value != null) scanned.value = lastDocCount.value;
  refreshList();
});

async function refreshList() {
  try {
    files.value = await kb.list(null);
  } catch (e: any) {
    files.value = [];
  }
}

// 虚拟滚动适配: string[] → 带稳定 id 的对象数组
const fileItems = computed(() =>
  files.value.map((f, i) => ({ id: i, path: f }))
);

async function openFile(p: string) {
  selected.value = p;
  try {
    markdown.value = await kb.read(p);
  } catch (e: any) {
    markdown.value = `_(读取失败:${e?.message ?? e})_`;
  }
}

async function doScan() {
  scanned.value = await kb.scan();
  await refreshList();
}

// 构建知识网: 一键流水线「编译 → 自动补双链 → 智能去重」, 委托全局 store
// (进度走 kb:compile / kb:enrich / kb:dedup 事件, 脱离本组件生命周期)
async function doCompile() {
  await kbStore.startBuildAll();
}
// 流水线阶段 → 按钮文案
const buildLabel = computed(() => {
  if (!compiling.value) return "构建知识网";
  switch (pipelineStage.value) {
    case "compile":
      return "1/3 编译知识网…";
    case "enrich":
      return "2/3 自动补双链…";
    case "dedup":
      return "3/3 智能去重…";
    default:
      return "正在构建知识网…";
  }
});
// wiki 质量检查 (纯规则, 同步返回报告)
async function doLint() {
  await kbStore.runLint();
}

// 信源安全扫描 (提示词注入痕迹, 纯规则)
async function doSecurityScan() {
  await kbStore.runScan();
}
const quarantining = ref<string | null>(null);
async function doQuarantine(path: string) {
  if (quarantining.value) return;
  quarantining.value = path;
  try {
    await kbStore.quarantine(path);
  } finally {
    quarantining.value = null;
  }
}
const sevLabel: Record<string, string> = { high: "高危", medium: "可疑", low: "留意" };
const catLabel: Record<string, string> = {
  "instruction-override": "指令覆盖",
  "role-hijack": "角色劫持",
  "tool-coercion": "诱导执行",
  exfiltration: "数据外泄",
  "hidden-content": "隐藏内容",
  "suspicious-link": "危险链接",
};

// ── 批量转换 md 文件 (原「快速重扫」位) ──
// 填文件/文件夹绝对路径 → 非视频类可抽文本的全转 md 入 raw/ 并索引;
// 视频跳过(留给将来 ASR), 图片等抽不出文本的也跳过、不原样复制。
const convertPath = ref("");
const converting = ref(false);
const convertMsg = ref("");
async function doConvertBatch() {
  const p = convertPath.value.trim();
  if (!p || converting.value) return;
  converting.value = true;
  convertMsg.value = "";
  try {
    const r = await kb.convertBatch([p]);
    let msg = `共 ${r.total} 个文件:转换 ${r.converted} · 跳过视频 ${r.skippedVideo} · 跳过其它 ${r.skippedOther}`;
    if (r.failed.length) msg += ` · 失败 ${r.failed.length}(${r.failed[0]})`;
    convertMsg.value = msg;
    await refreshList();
  } catch (e: any) {
    convertMsg.value = `失败:${e?.message ?? e}`;
  } finally {
    converting.value = false;
  }
}

async function doSearch() {
  const q = query.value.trim();
  hitCap.value = 100;
  if (!q) {
    hits.value = [];
    artHits.value = [];
    return;
  }
  [hits.value, artHits.value] = await Promise.all([
    kb.search(q),
    artifactsApi.search(q),
  ]);
}

// 点开历史产物 → 右侧抽屉预览
function openArtifact(path: string) {
  artifactsStore.open(path);
}

async function doIngest() {
  if (!ingestPath.value.trim()) return;
  try {
    const r = await kb.ingest(ingestPath.value.trim());
    ingestMsg.value = `已 ingest → ${r}`;
    await refreshList();
  } catch (e: any) {
    ingestMsg.value = `失败:${e?.message ?? e}`;
  }
}

// 删除单份资料（浏览页每行右侧 ×）
async function doDelete(rel: string) {
  if (!confirm(`删除这份资料？\n${rel}`)) return;
  try {
    await kb.delete(rel);
    if (selected.value === rel) {
      selected.value = null;
      markdown.value = "";
    }
    await refreshList();
  } catch (e: any) {
    alert(`删除失败:${e?.message ?? e}`);
  }
}

// 清空整个资料库（管理页）
const clearMsg = ref("");
async function doClear() {
  if (
    !confirm(
      "确定清空整个资料库吗?\n这会删除全部资料(含已安装的名人资料包),且不可撤销;资料包可在「名人资料包」里重新安装。"
    )
  )
    return;
  try {
    const n = await kb.clear();
    clearMsg.value = `已清空,剩余 ${n} 个文件`;
    selected.value = null;
    markdown.value = "";
    await refreshList();
  } catch (e: any) {
    clearMsg.value = `失败:${e?.message ?? e}`;
  }
}

// ─────────── 拖拽上传到知识库 ───────────
interface UploadItem {
  name: string;
  status: "loading" | "ok" | "err";
  detail: string;
}
const uploading = ref<UploadItem[]>([]);

async function onDropFiles(paths: string[]) {
  // 乐观占位（大文件转换 / 复制需要时间，逐个显示进度）
  uploading.value = paths.map((p) => ({
    name: p.split(/[\\/]/).pop() || p,
    status: "loading",
    detail: "",
  }));
  try {
    const res = await kb.uploadFiles(paths);
    uploading.value = res.map((r) => ({
      name: r.name,
      status: r.ok ? "ok" : "err",
      detail: r.ok ? r.relPath : r.message,
    }));
    await refreshList();
  } catch (e: any) {
    uploading.value = uploading.value.map((u) => ({
      ...u,
      status: "err",
      detail: e?.message ?? String(e),
    }));
  }
  // 成功项几秒后淡出，失败项保留以便查看
  window.setTimeout(() => {
    uploading.value = uploading.value.filter((u) => u.status === "err");
  }, 5000);
}

const { isOver: dropOver } = useFileDrop({
  active: () => app.view === "wiki",
  onDrop: onDropFiles,
});

// ═══════════════ 全盘资源归集（概览 tab） ═══════════════
// 扫描 C/D 盘/桌面 → 多维表格 → 归档到资源库(raw/) / 摄入核心层(构建知识网)。
// 归档复用 kb.uploadFiles;摄入核心层 = 归档后跑 kbStore.startBuildAll()。
interface ScanRowVM extends ScanRow {
  checked: boolean;
}
const scanRoots = ref<ScanRoot[]>([]);
const rootsLoaded = ref(false);
const resScanning = ref(false);
const rows = ref<ScanRowVM[]>([]);
const scanMeta = ref<{ totalSeen: number; hit: number; skipped: number; truncated: boolean } | null>(null);
const archiving = ref(false);
const archiveMsg = ref("");

async function loadScanRoots() {
  try {
    scanRoots.value = await scan.roots();
  } catch {
    scanRoots.value = [];
  }
  rootsLoaded.value = true;
}

// 进入概览首次拉取扫描根
watch(
  tab,
  (t) => {
    if (t === "overview" && !rootsLoaded.value) loadScanRoots();
  },
  { immediate: true },
);

function toggleRoot(r: ScanRoot) {
  r.defaultOn = !r.defaultOn;
}

async function doResourceScan() {
  const roots = scanRoots.value.filter((r) => r.defaultOn).map((r) => r.path);
  if (!roots.length) {
    archiveMsg.value = "请先勾选要扫描的范围";
    return;
  }
  resScanning.value = true;
  archiveMsg.value = "";
  rows.value = [];
  scanMeta.value = null;
  try {
    const rep = await scan.resources(roots);
    rows.value = rep.rows.map((r) => ({
      ...r,
      checked: r.suggest !== "skip",
    }));
    scanMeta.value = {
      totalSeen: rep.totalSeen,
      hit: rep.hit,
      skipped: rep.skipped,
      truncated: rep.truncated,
    };
  } catch (e: any) {
    archiveMsg.value = `扫描失败:${e?.message ?? e}`;
  } finally {
    resScanning.value = false;
  }
}

// 扫描结果全部交给虚拟滚动渲染(见模板 RecycleScroller):不再硬截断到 600 项,
// 几万条也只在 DOM 里保留视口内十几行,首屏渲染从「600 行铺满」降到常数级。
const checkedCount = computed(() => rows.value.filter((r) => r.checked).length);
const checkedBytes = computed(() =>
  rows.value.filter((r) => r.checked).reduce((s, r) => s + r.size, 0),
);
function humanBytes(b: number): string {
  const u = ["B", "KB", "MB", "GB", "TB"];
  let v = b,
    i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return i === 0 ? `${b} B` : `${v.toFixed(1)} ${u[i]}`;
}

function selectAll(on: boolean) {
  rows.value.forEach((r) => (r.checked = on));
}
const kindLabel: Record<string, string> = {
  doc: "文档",
  text: "文本",
  sheet: "表格",
  slide: "演示",
  data: "数据",
  image: "图片",
  audio: "音频",
  video: "视频",
  archive: "压缩包",
  code: "代码",
  other: "其它",
};

// 归入核心层(数据库):把勾选文件复制入 raw/ 并索引,再跑构建知识网(LLM 编译)。
// 暂时把整台电脑都当成数据库 —— 扫到的有用文件统一归入核心层这一个去向。
async function archiveToCore() {
  const paths = rows.value.filter((r) => r.checked).map((r) => r.path);
  if (!paths.length || archiving.value) return;
  archiving.value = true;
  archiveMsg.value = `正在把 ${paths.length} 个文件归入核心层(数据库)…`;
  try {
    await kb.uploadFiles(paths);
    await refreshList();
    await kbStore.startBuildAll(); // 构建知识网(进度走 kb:compile 事件,见「管理」tab)
    archiveMsg.value = `${paths.length} 个文件已归档,核心层编译已启动(见「管理」tab 进度) ✓`;
  } catch (e: any) {
    archiveMsg.value = `归入失败:${e?.message ?? e}`;
  } finally {
    archiving.value = false;
  }
}

// ── 表格对话框(自然语言指挥多维表格) ──
// 现阶段为本地规则解析,覆盖常用指令;LLM 语义理解后续接入(见 PRD P3)。
interface ChatTurn {
  role: "u" | "a";
  text: string;
}
const chatLog = ref<ChatTurn[]>([]);
const chatInput = ref("");
function applyTableCmd(raw: string): string {
  const t = raw.toLowerCase();
  const now = Date.now() / 1000;
  const has = (...ks: string[]) => ks.some((k) => raw.includes(k) || t.includes(k));
  // 执行类
  if (has("归档全部", "全部归档", "全部归入", "都归入")) {
    selectAll(true);
    archiveToCore();
    return `已勾选全部并开始把 ${rows.value.length} 项归入核心层(数据库)。`;
  }
  if (has("摄入", "归入", "核心层", "数据库", "维基")) {
    archiveToCore();
    return `开始把勾选的 ${checkedCount.value} 项归入核心层(数据库)。`;
  }
  // 选择类
  if (has("全选", "都选上", "选全部")) {
    selectAll(true);
    return `已全选 ${rows.value.length} 项。`;
  }
  if (has("全不选", "取消全部", "都不选", "清空选择")) {
    selectAll(false);
    return "已取消全部勾选。";
  }
  let changed = 0;
  if (has("图片")) {
    rows.value.forEach((r) => r.kind === "image" && r.checked && ((r.checked = false), changed++));
    return `已取消勾选 ${changed} 个图片。`;
  }
  if (has("视频")) {
    rows.value.forEach((r) => r.kind === "video" && r.checked && ((r.checked = false), changed++));
    return `已取消勾选 ${changed} 个视频。`;
  }
  if (has("压缩包", "压缩")) {
    rows.value.forEach((r) => r.kind === "archive" && r.checked && ((r.checked = false), changed++));
    return `已取消勾选 ${changed} 个压缩包。`;
  }
  if (has("两年", "2年", "很久", "旧的", "没动")) {
    const cut = now - 60 * 60 * 24 * 365 * 2;
    rows.value.forEach((r) => r.mtime < cut && r.checked && ((r.checked = false), changed++));
    return `已取消勾选 ${changed} 个两年以上未修改的文件。`;
  }
  if (has("低价值", "没用的", "噪音", "评分低")) {
    rows.value.forEach((r) => r.score <= 2 && r.checked && ((r.checked = false), changed++));
    return `已取消勾选 ${changed} 个低价值项。`;
  }
  if (has("近一年", "最近", "一年内", "近期")) {
    const cut = now - 60 * 60 * 24 * 365;
    rows.value.forEach((r) => (r.checked = r.mtime >= cut));
    return `已只保留近一年的 ${checkedCount.value} 项。`;
  }
  return "暂未理解(现支持:全选/全不选/取消图片|视频|压缩包/取消两年没动的/取消低价值/只留近一年/全部归入核心层)。LLM 语义理解将在后续接入。";
}
function sendTableCmd() {
  const v = chatInput.value.trim();
  if (!v) return;
  chatLog.value.push({ role: "u", text: v });
  const reply = applyTableCmd(v);
  chatLog.value.push({ role: "a", text: reply });
  chatInput.value = "";
}
</script>

<template>
  <div class="wiki" :class="{ 'drag-active': dropOver }">
    <!-- 拖拽上传覆盖层 -->
    <div v-if="dropOver" class="kb-drop-overlay">
      <div class="kb-drop-card">
        <Upload :size="34" :stroke-width="1.4" />
        <div class="kb-drop-title">松开以加入知识库</div>
        <div class="kb-drop-sub">
          自动转 Markdown 入库并索引 · 支持 PDF / Word / Excel / PPT / 文本 / 代码
        </div>
      </div>
    </div>

    <!-- 上传进度（逐文件） -->
    <div v-if="uploading.length" class="upload-panel">
      <div class="upload-head">上传到知识库</div>
      <div
        v-for="(u, i) in uploading"
        :key="i"
        class="upload-row"
        :class="u.status"
      >
        <LoaderCircle v-if="u.status === 'loading'" :size="15" class="spin" />
        <CheckCircle2 v-else-if="u.status === 'ok'" :size="15" />
        <XCircle v-else :size="15" />
        <span class="up-name" :title="u.name">{{ u.name }}</span>
        <span class="up-detail" :title="u.detail">{{ u.detail }}</span>
      </div>
    </div>

    <div class="head">
      <div class="title">名人知识库</div>
      <div class="tabs">
        <button
          v-for="t in [
            { k: 'overview', l: '概览' },
            { k: 'packs', l: '名人资料包' },
            { k: 'browse', l: '浏览' },
            { k: 'manage', l: '管理' },
          ]"
          :key="t.k"
          class="tab"
          :class="{ active: tab === t.k }"
          @click="tab = t.k as Tab"
        >
          {{ t.l }}
        </button>
        <!-- 图谱:并入知识库的小功能键,点开切到星河图谱视图(复用其全屏加载机) -->
        <button class="tab tab-graph" title="知识图谱·星河" @click="app.setView('graph')">
          <Waypoints :size="14" :stroke-width="1.8" />
          图谱
        </button>
      </div>
      <div class="root">
        <span class="root-label">KB 根:</span>
        <code>{{ rootPath }}</code>
      </div>
    </div>

    <div v-if="tab === 'overview'" class="body overview rg">
      <!-- 扫描入口 -->
      <div class="rg-hero glass">
        <div class="rg-hero-title">把电脑炼成你的数据库</div>
        <div class="rg-hero-sub">
          扫描盘里散落的有用文件 → 逐文件看清内容 → 一键<b class="b-core">归入核心层</b>(大模型把资料编译成可检索的知识灵魂)。暂时把整台电脑都当成你的数据库。
        </div>
        <div class="rg-roots">
          <button
            v-for="r in scanRoots"
            :key="r.id"
            class="rg-rootchip"
            :class="{ on: r.defaultOn }"
            @click="toggleRoot(r)"
          >
            <span class="dot"></span>{{ r.label }}
          </button>
          <span v-if="rootsLoaded && !scanRoots.length" class="muted">未发现可扫描的盘/目录</span>
        </div>
      </div>

      <!-- 多维表格 -->
      <div v-if="rows.length" class="rg-table glass">
        <div class="rg-ttop">
          <span class="rg-ttitle">扫描结果 · 已按价值预选</span>
          <span class="rg-tmeta">
            勾选 <b class="b-core">{{ checkedCount }}</b> · {{ humanBytes(checkedBytes) }}
          </span>
          <span class="rg-spacer"></span>
          <button class="rg-chip" @click="selectAll(true)">全选</button>
          <button class="rg-chip" @click="selectAll(false)">全不选</button>
          <button class="rg-chip gold" :disabled="archiving || !checkedCount" @click="archiveToCore">
            归入核心层 {{ checkedCount }}
          </button>
        </div>
        <div class="rg-row rg-hd">
          <span class="c-cb"></span>
          <span class="c-name">文件名</span>
          <span class="c-ty">类型</span>
          <span class="c-prev">大概内容</span>
          <span class="c-size">大小</span>
          <span class="c-score">价值</span>
        </div>
        <!-- 虚拟滚动:行等高 46px、各列 nowrap+ellipsis(见 .rg-row CSS),DOM 里只保留视口内
             十几行。于是几万条扫描结果也秒开、不再硬截断到 600;v-model 绑定的是数据对象
             item.checked(非 DOM 节点),配合 key-field=id,回收复用不会串值。 -->
        <RecycleScroller
          class="rg-list"
          :items="rows"
          :item-size="46"
          key-field="id"
          v-slot="{ item }"
        >
          <div
            class="rg-row"
            :class="{ dim: !item.checked }"
            @click="item.checked = !item.checked"
          >
            <span class="c-cb">
              <input type="checkbox" v-model="item.checked" @click.stop />
            </span>
            <span class="c-name" :title="item.path">{{ item.name }}</span>
            <span class="c-ty"><span class="ty" :class="'ty-' + item.kind">{{ kindLabel[item.kind] || item.kind }}</span></span>
            <span class="c-prev" :title="item.preview">{{ item.preview }}</span>
            <span class="c-size">{{ item.sizeH }}</span>
            <span class="c-score">{{ "★".repeat(item.score) }}</span>
          </div>
        </RecycleScroller>
        <div v-if="archiveMsg" class="rg-archmsg">{{ archiveMsg }}</div>
      </div>
      <div v-else-if="scanMeta && !resScanning" class="rg-empty muted">
        没扫到可归档的资源(命中 0)。换个扫描范围试试。
      </div>
      <div v-else-if="!resScanning" class="rg-empty muted">
        到「文件中心」点「盘点」扫描你的盘 —— 扫到的有用文件会归入核心层,在这里连成知识网。
      </div>

      <!-- 表格对话框 -->
      <div v-if="rows.length" class="rg-chat glass">
        <div class="rg-chat-head">✦ 和这张表说话</div>
        <div v-if="chatLog.length" class="rg-chat-body">
          <div
            v-for="(m, i) in chatLog"
            :key="i"
            class="rg-bubble"
            :class="m.role"
          >
            {{ m.text }}
          </div>
        </div>
        <div class="rg-chat-input">
          <input
            v-model="chatInput"
            placeholder="例:把图片都取消 / 取消两年没动的 / 只留近一年 / 全部归入核心层"
            @keydown.enter="sendTableCmd"
          />
          <button class="rg-primary sm" @click="sendTableCmd">发送</button>
        </div>
      </div>
    </div>

    <div v-if="tab === 'packs'" class="body packs">
      <div class="packs-intro">
        名人资料包:把名人的著作资料<strong>下载到自己的资料库</strong>(<code>raw/</code> 下),
        并附带安装配套技能 —— 技能里写好了这个资料库的使用方法,装完即可在对话里直接请教。
      </div>
      <div class="pack-grid">
        <div v-for="p in packs" :key="p.id" class="card pack-card">
          <div class="pack-head">
            <div class="card-title">{{ p.name }}</div>
            <span v-if="p.installed" class="pack-badge installed">已装入</span>
          </div>
          <div class="card-body">{{ p.description }}</div>
          <div class="pack-skill">
            <Sparkles :size="13" :stroke-width="1.8" />
            <span>配套技能:<code>{{ p.skillId }}</code></span>
          </div>
          <div class="pack-actions">
            <button
              v-if="!p.installed"
              class="primary-btn"
              :disabled="packBusy === p.id"
              @click="installPack(p)"
            >
              <LoaderCircle
                v-if="packBusy === p.id"
                :size="14"
                :stroke-width="1.8"
                class="spin"
              />
              <Download v-else :size="14" :stroke-width="1.8" />
              <span>{{ packBusy === p.id ? "正在装入…" : "下载到我的资料库" }}</span>
            </button>
            <button
              v-else
              class="btn"
              :disabled="packBusy === p.id"
              @click="removePack(p)"
            >
              {{ packBusy === p.id ? "正在移除…" : "移除" }}
            </button>
          </div>
        </div>
        <div v-if="!packs.length" class="muted empty">
          暂无可用资料包(浏览器模式或资源目录缺失)
        </div>
      </div>
      <div v-if="packMsg" class="muted pack-msg">{{ packMsg }}</div>
    </div>

    <div v-if="tab === 'browse'" class="body browse">
      <div class="left">
        <div class="search-row">
          <input
            v-model="query"
            placeholder="搜索 KB(标题/正文)"
            @keydown.enter="doSearch"
          />
          <button class="btn" @click="doSearch">搜</button>
        </div>
        <div v-if="hits.length" class="hit-list">
          <div class="section-title">搜索结果</div>
          <div
            v-for="h in hits.slice(0, hitCap)"
            :key="h.path"
            class="hit"
            @click="openFile(h.path)"
          >
            <div class="hit-title">{{ h.title }}</div>
            <div class="hit-snip">{{ h.snippet }}</div>
            <div class="hit-meta">score {{ h.score.toFixed(1) }} · {{ h.path }}</div>
          </div>
          <button v-if="hits.length > hitCap" class="btn more" @click="hitCap += 100">
            展示更多（还有 {{ hits.length - hitCap }} 条）
          </button>
        </div>
        <div v-if="artHits.length" class="hit-list">
          <div class="section-title">历史对话产物</div>
          <div
            v-for="a in artHits.slice(0, hitCap)"
            :key="a.path"
            class="hit"
            @click="openArtifact(a.path)"
          >
            <div class="hit-title">{{ a.name }}</div>
            <div v-if="a.snippet" class="hit-snip">{{ a.snippet }}</div>
            <div class="hit-meta">产物 · {{ a.kind }} · 点开右栏预览</div>
          </div>
        </div>
        <div class="section-title">所有文件</div>
        <RecycleScroller
          v-if="files.length"
          class="file-scroller"
          :items="fileItems"
          :item-size="28"
          key-field="id"
        >
          <template #default="{ item }">
            <div
              class="file"
              :class="{ active: selected === item.path }"
              @click="openFile(item.path)"
            >
              <span class="file-name">{{ item.path }}</span>
              <button
                class="file-del"
                title="删除这份资料"
                @click.stop="doDelete(item.path)"
              >
                <X :size="13" :stroke-width="2" />
              </button>
            </div>
          </template>
        </RecycleScroller>
        <div v-else class="muted empty">
          KB 为空 —— 把文件直接拖到本页面即可入库,或在「管理」tab 手动 ingest
        </div>
      </div>
      <div class="right">
        <div v-if="!selected" class="placeholder">
          <div class="ph-glyph">▥</div>
          <div>选择左侧文件浏览</div>
        </div>
        <div v-else class="md" v-html="rendered"></div>
      </div>
    </div>

    <div v-if="tab === 'manage'" class="body manage">
      <div class="card">
        <div class="card-title">Ingest 文件 → KB</div>
        <div class="card-body">
          直接把文件<strong>拖到本页面</strong>即可入库;也可填本机绝对路径手动 ingest。
          自动转 Markdown 入 <code>raw/</code> 并索引 —— 支持 PDF / Word(docx) /
          Excel(xlsx) / PPT(pptx) / 文本 / 代码;图片等不可转的原样保存。
        </div>
        <div class="ingest-row">
          <input v-model="ingestPath" placeholder="例:D:\案例文件夹\01_xxx.pdf" />
          <button class="primary-btn" @click="doIngest">Ingest</button>
        </div>
        <div v-if="ingestMsg" class="ingest-msg">{{ ingestMsg }}</div>
      </div>
      <div class="card accent-card">
        <div class="card-title">构建知识网 · 摄入即编译 + 自动维护</div>
        <div class="card-body">
          一键跑完三步:<strong>① 编译</strong>(wiki 维护者读 <code>raw/</code>
          原始资料,抽取实体与思想脉络,在 <code>wiki/</code> 写概念页并用
          <code>[[双链]]</code> 互联,把散落的资料<strong>编译成一张有关系的知识网</strong>)
          → <strong>② 补双链</strong>(只读 AI 找出该互联却漏链的词,<em>替换由代码执行,正文不乱改</em>)
          → <strong>③ 去重</strong>(规则粗筛同名页 → AI 判真重复 → 合并并重写全库双链)。
          原始资料只读不改。耗时分钟级。
        </div>
        <button class="primary-btn" :disabled="compiling" @click="doCompile">
          <LoaderCircle
            v-if="compiling"
            :size="14"
            :stroke-width="1.8"
            class="spin"
          />
          <span>{{ buildLabel }}</span>
        </button>
        <span v-if="compileMsg" class="muted clear-msg">{{ compileMsg }}</span>
        <div v-if="compileLog.length" class="compile-log">
          <div v-for="(l, i) in compileLog" :key="i" class="compile-line">
            {{ l }}
          </div>
        </div>
      </div>
      <div class="card">
        <div class="card-title">质量检查 · 纯规则秒级</div>
        <div class="card-body">
          给知识网体检:扫<strong>死链 / 缺 type / 孤儿页 / 危险路径</strong>,
          不改任何文件、即时出报告。构建完或手动改完 wiki 后随手查一下。
        </div>
        <div class="maintain-row">
          <button class="primary-btn" :disabled="linting" @click="doLint">
            <span>{{ linting ? "体检中…" : "质量检查" }}</span>
          </button>
        </div>
        <div v-if="lintReport" class="lint-report">
          <div class="lint-summary">
            共 {{ lintReport.totalPages }} 页 ·
            死链 <b :class="{ bad: lintReport.deadLinks }">{{ lintReport.deadLinks }}</b> ·
            缺type <b :class="{ bad: lintReport.missingType }">{{ lintReport.missingType }}</b> ·
            孤儿 <b :class="{ bad: lintReport.orphans }">{{ lintReport.orphans }}</b> ·
            危险路径 <b :class="{ bad: lintReport.unsafePaths }">{{ lintReport.unsafePaths }}</b>
          </div>
          <div v-if="lintReport.issues.length" class="lint-issues">
            <div
              v-for="(it, i) in lintReport.issues.slice(0, 50)"
              :key="i"
              class="lint-line"
            >
              <span class="lint-kind">{{ it.kind }}</span>
              <span class="lint-path">{{ it.path }}</span>
              <span class="lint-detail">{{ it.detail }}</span>
            </div>
          </div>
          <div v-else class="muted">未发现问题,知识网很健康 ✓</div>
        </div>
      </div>
      <div class="card">
        <div class="card-title">信源安全扫描 · 防提示词注入</div>
        <div class="card-body">
          扫 <code>raw/</code> 等外部资料里有没有<strong>试图操纵 AI 的隐藏指令</strong>(提示词注入):
          「忽略以上指令」「你现在是…」「运行以下命令」「把密钥发送到…」、零宽隐藏字符、危险链接等。
          模型答题时会主动 Read 这些资料,被注入的文档可能指挥它跑命令/外发数据 ——
          扫到可疑信源可<strong>一键隔离</strong>(移出 raw/,模型不再读到,可逆)。
        </div>
        <div class="maintain-row">
          <button class="primary-btn" :disabled="scanning" @click="doSecurityScan">
            <span>{{ scanning ? "扫描中…" : "安全扫描" }}</span>
          </button>
        </div>
        <div v-if="threatReport" class="lint-report">
          <div class="lint-summary">
            扫 {{ threatReport.scannedFiles }} 个文件 ·
            可疑文件 <b :class="{ bad: threatReport.flaggedFiles }">{{ threatReport.flaggedFiles }}</b> ·
            高危 <b :class="{ bad: threatReport.high }">{{ threatReport.high }}</b> ·
            可疑 <b :class="{ bad: threatReport.medium }">{{ threatReport.medium }}</b> ·
            留意 <b>{{ threatReport.low }}</b>
          </div>
          <div v-if="threatReport.hits.length" class="lint-issues">
            <div
              v-for="(h, i) in threatReport.hits.slice(0, 80)"
              :key="i"
              class="threat-line"
            >
              <span class="threat-sev" :class="'sev-' + h.severity">{{ sevLabel[h.severity] || h.severity }}</span>
              <span class="lint-kind">{{ catLabel[h.category] || h.category }}</span>
              <span class="threat-where">
                <span class="lint-path">{{ h.path }}</span>
                <span class="threat-snip">第 {{ h.line }} 行：{{ h.snippet }}</span>
              </span>
              <button
                class="quarantine-btn"
                :disabled="quarantining === h.path"
                title="移出 raw/ 到 .quarantine/，模型不再读到（可逆）"
                @click="doQuarantine(h.path)"
              >
                {{ quarantining === h.path ? "隔离中…" : "隔离" }}
              </button>
            </div>
          </div>
          <div v-else class="muted">未发现注入痕迹,信源干净 ✓</div>
        </div>
      </div>
      <div class="card">
        <div class="card-title">批量转换 md 文件 · 非视频类</div>
        <div class="card-body">
          填本机文件或文件夹绝对路径(文件夹递归展开),把里面的<strong>非视频类</strong>文件
          批量转换成 Markdown 入 <code>raw/</code> 并索引 —— 支持 PDF / Word / Excel /
          PPT / 文本 / 代码;<strong>视频跳过</strong>,图片等抽不出文本的也跳过(不复制原件)。
          同一文件内容没变时重跑会自动复用,不会重复转换。
        </div>
        <div class="ingest-row">
          <input
            v-model="convertPath"
            placeholder="例:D:\资料文件夹 或 D:\资料\报告.pdf"
            @keydown.enter="doConvertBatch"
          />
          <button
            class="primary-btn"
            :disabled="converting"
            @click="doConvertBatch"
          >
            <LoaderCircle
              v-if="converting"
              :size="14"
              :stroke-width="1.8"
              class="spin"
            />
            <span>{{ converting ? "转换中…" : "批量转换" }}</span>
          </button>
        </div>
        <div v-if="convertMsg" class="ingest-msg">{{ convertMsg }}</div>
      </div>
      <div class="card danger-card">
        <div class="card-title">清空资料库</div>
        <div class="card-body">
          删除 <code>raw/</code> 下的<strong>全部资料</strong>(含已安装的名人资料包),
          保留目录结构。此操作<strong>不可撤销</strong>;名人资料包可在「名人资料包」tab
          重新安装。也可在「浏览」里逐条点 × 删除单份资料。
        </div>
        <button class="danger-btn" @click="doClear">
          <Trash2 :size="14" :stroke-width="1.8" />
          <span>清空资料库</span>
        </button>
        <span v-if="clearMsg" class="muted clear-msg">{{ clearMsg }}</span>
      </div>
    </div>
  </div>
</template>

<style scoped>
.wiki {
  display: flex;
  flex-direction: column;
  height: 100%;
  position: relative;
}
.head {
  padding: 18px 28px 0;
  border-bottom: 1px solid var(--hairline);
}
.title {
  font-family: var(--serif);
  font-size: 18px;
  letter-spacing: 2px;
  color: var(--ink);
}
.tabs {
  margin-top: 14px;
  display: flex;
  gap: 18px;
}
.tab {
  background: transparent;
  border: none;
  padding: 8px 0;
  color: var(--muted);
  font-size: 13px;
}
.tab.active {
  color: var(--text);
  font-weight: 600;
  border-bottom: 2px solid var(--ink);
}
.tab-graph {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  margin-left: 4px;
  padding-left: 16px;
  border-left: 1px solid var(--border-soft);
  color: var(--primary);
}
.tab-graph:hover {
  color: var(--text);
}
.root {
  margin-top: 8px;
  font-size: 11px;
  color: var(--muted);
  padding-bottom: 8px;
}
.root-label {
  margin-right: 6px;
}
.root code {
  background: var(--code-bg);
  padding: 1px 6px;
  border-radius: 2px;
  font-family: var(--mono);
}

.body {
  flex: 1;
  overflow: hidden;
  padding: 18px 28px;
}
.body.overview {
  display: flex;
  flex-direction: column;
  gap: 18px;
}
.body.browse {
  display: grid;
  grid-template-columns: 320px 1fr;
  gap: 16px;
  height: calc(100vh - 130px);
}
.body.manage {
  display: flex;
  flex-direction: column;
  gap: 18px;
}
/* 文件中心:让琉璃文件库铺满,内部自管滚动 */
.body.files-body {
  padding: 12px 18px 8px;
  height: calc(100vh - 130px);
  overflow: hidden;
}
.body.packs {
  display: flex;
  flex-direction: column;
  gap: 16px;
  overflow-y: auto;
}
.packs-intro {
  font-size: 12.5px;
  color: var(--text-2);
  line-height: 1.7;
}
.pack-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
  gap: 14px;
}
.pack-card {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.pack-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.pack-head .card-title {
  margin-bottom: 0;
  font-size: 15px;
}
.pack-badge.installed {
  font-size: 11px;
  color: var(--ok);
  border: 1px solid var(--ok-soft);
  background: var(--ok-soft);
  border-radius: 10px;
  padding: 1px 8px;
}
.pack-skill {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 11.5px;
  color: var(--muted);
}
.pack-skill code {
  background: var(--code-bg);
  padding: 1px 6px;
  border-radius: 2px;
  font-family: var(--mono);
}
.pack-actions {
  margin-top: 4px;
}
.pack-actions .primary-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
.pack-msg {
  font-size: 12px;
}

.cards {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 14px;
}
.card {
  background: var(--panel);
  border: 1px solid var(--hairline);
  border-radius: 4px;
  padding: 16px 18px;
}
.card-title {
  font-family: var(--serif);
  font-weight: 600;
  font-size: 13.5px;
  color: var(--text);
  margin-bottom: 6px;
}
.card-body {
  font-size: 12.5px;
  color: var(--text-2);
  line-height: 1.7;
}

.primary-btn {
  align-self: flex-start;
  padding: 7px 16px;
  background: var(--btn-solid-bg);
  color: var(--btn-solid-text);
  border: none;
  border-radius: 4px;
  font-size: 12.5px;
}
.primary-btn:hover {
  background: var(--primary);
}
.muted {
  color: var(--muted);
  font-size: 12px;
}

.left {
  border: 1px solid var(--hairline);
  border-radius: 4px;
  padding: 10px;
  background: var(--panel);
  display: flex;
  flex-direction: column;
  min-height: 0;
}
.file-scroller {
  flex: 1;
  min-height: 0;
}
.right {
  border: 1px solid var(--hairline);
  border-radius: 4px;
  padding: 22px 28px;
  overflow-y: auto;
  background: var(--panel);
}
.search-row {
  display: flex;
  gap: 6px;
  margin-bottom: 10px;
}
.search-row input {
  flex: 1;
  padding: 6px 8px;
  border: 1px solid var(--border);
  border-radius: 3px;
  font-size: 12.5px;
  background: var(--bg);
}
.search-row input:focus {
  outline: none;
  border-color: var(--primary);
}
.btn {
  padding: 6px 12px;
  border: 1px solid var(--border);
  background: var(--panel);
  border-radius: 3px;
  font-size: 12.5px;
}
.btn:hover {
  border-color: var(--primary);
}

.section-title {
  font-family: var(--serif);
  font-size: 11px;
  letter-spacing: 1.5px;
  color: var(--dim);
  padding: 8px 4px 4px;
}
.hit-list {
  margin-bottom: 10px;
}
.hit {
  padding: 8px 10px;
  border-radius: 3px;
  cursor: pointer;
  margin-bottom: 2px;
}
.hit:hover {
  background: var(--selection-bg);
}
.hit-title {
  font-size: 13px;
  font-weight: 600;
  color: var(--text);
}
.hit-snip {
  font-size: 11.5px;
  color: var(--muted);
  margin-top: 2px;
  line-height: 1.5;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
}
.hit-meta {
  font-size: 10.5px;
  color: var(--dim);
  margin-top: 2px;
  font-family: var(--mono);
}

.file {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 5px 6px 5px 10px;
  font-size: 12.5px;
  color: var(--text-2);
  border-radius: 3px;
  cursor: pointer;
  font-family: var(--mono);
}
.file-name {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.file:hover {
  background: var(--selection-bg);
  color: var(--text);
}
.file.active {
  background: var(--selection-bg);
  color: var(--ink);
  font-weight: 500;
}
.file-del {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
  flex-shrink: 0;
  border: none;
  background: transparent;
  color: var(--dim);
  border-radius: 4px;
  cursor: pointer;
  opacity: 0;
  transition: opacity 0.12s, background 0.12s, color 0.12s;
}
.file:hover .file-del {
  opacity: 1;
}
.file-del:hover {
  background: var(--vermilion-soft);
  color: var(--vermilion);
}

/* 清空资料库 —— 危险操作卡片 */
.danger-card {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  border-color: rgba(192, 57, 43, 0.25);
}
.danger-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  align-self: flex-start;
  margin-top: 12px;
  padding: 7px 14px;
  background: var(--vermilion);
  color: #fff;
  border: none;
  border-radius: 4px;
  font-size: 12.5px;
  cursor: pointer;
}
.danger-btn:hover {
  opacity: 0.9;
}
.clear-msg {
  margin-top: 8px;
}
.accent-card {
  border-left: 3px solid var(--accent, #b5462f);
}
.primary-btn:disabled {
  opacity: 0.6;
  cursor: default;
}
.primary-btn .spin {
  margin-right: 6px;
  vertical-align: -2px;
}
.compile-log {
  margin-top: 12px;
  max-height: 200px;
  overflow-y: auto;
  padding: 10px 12px;
  background: var(--code-bg, rgba(0, 0, 0, 0.04));
  border-radius: 6px;
  font-family: var(--mono, monospace);
  font-size: 12px;
  line-height: 1.7;
}
.compile-line {
  color: var(--text-2, #4a4f57);
  white-space: pre-wrap;
  word-break: break-word;
}

.maintain-row {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
}
.lint-report {
  margin-top: 12px;
}
.lint-summary {
  font-size: 13px;
  color: var(--text-2, #4a4f57);
  margin-bottom: 8px;
}
.lint-summary b {
  color: var(--accent, #2f6df0);
}
.lint-summary b.bad {
  color: var(--vermilion);
}
.lint-issues {
  max-height: 200px;
  overflow-y: auto;
  padding: 8px 10px;
  background: var(--code-bg, rgba(0, 0, 0, 0.04));
  border-radius: 6px;
  font-size: 12px;
  line-height: 1.6;
}
.lint-line {
  display: flex;
  gap: 8px;
  white-space: nowrap;
  overflow: hidden;
}
.lint-kind {
  flex: none;
  color: var(--gold);
  font-family: var(--mono, monospace);
}
.lint-path {
  flex: none;
  color: var(--text);
  font-family: var(--mono, monospace);
}
.lint-detail {
  color: var(--dim, #888);
  overflow: hidden;
  text-overflow: ellipsis;
}

/* ── 信源安全扫描命中行 ── */
.threat-line {
  display: flex;
  gap: 8px;
  align-items: center;
  padding: 3px 0;
  border-bottom: 1px solid var(--border, rgba(0, 0, 0, 0.06));
}
.threat-line:last-child {
  border-bottom: none;
}
.threat-sev {
  flex: none;
  font-weight: 700;
  padding: 1px 7px;
  border-radius: 10px;
  font-size: 11px;
}
.threat-sev.sev-high {
  color: #fff;
  background: var(--vermilion, #e5484d);
}
.threat-sev.sev-medium {
  color: #7a4a00;
  background: rgba(255, 166, 77, 0.28);
}
.threat-sev.sev-low {
  color: var(--dim, #888);
  background: rgba(0, 0, 0, 0.06);
}
.threat-where {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
.threat-snip {
  color: var(--dim, #888);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.quarantine-btn {
  flex: none;
  font-size: 11px;
  padding: 2px 9px;
  border-radius: 6px;
  border: 1px solid var(--vermilion, #e5484d);
  color: var(--vermilion, #e5484d);
  background: transparent;
  cursor: pointer;
}
.quarantine-btn:hover:not(:disabled) {
  background: var(--vermilion, #e5484d);
  color: #fff;
}
.quarantine-btn:disabled {
  opacity: 0.5;
  cursor: default;
}

.placeholder {
  height: 100%;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  color: var(--dim);
  font-family: var(--serif);
  letter-spacing: 1px;
}
.ph-glyph {
  font-size: 40px;
  margin-bottom: 12px;
  color: var(--border-strong);
}

.md {
  font-size: 13.5px;
  line-height: 1.85;
  color: var(--text);
}
.md :deep(h1),
.md :deep(h2),
.md :deep(h3) {
  font-family: var(--serif);
  letter-spacing: 1px;
}
.md :deep(h1) {
  font-size: 22px;
  margin-top: 0;
}
.md :deep(h2) {
  font-size: 17px;
  border-bottom: 1px solid var(--hairline);
  padding-bottom: 6px;
}
.md :deep(code) {
  background: var(--code-bg);
  padding: 1.5px 6px;
  border-radius: 2px;
  font-family: var(--mono);
  font-size: 12px;
}
.md :deep(pre) {
  background: var(--bg-soft);
  border: 1px solid var(--hairline);
  padding: 14px 16px;
  border-radius: 3px;
  overflow-x: auto;
}
.md :deep(blockquote) {
  border-left: 2px solid var(--ink);
  padding-left: 14px;
  color: var(--text-2);
  margin-left: 0;
}
.md :deep(a) {
  color: var(--primary);
}

.ingest-row {
  display: flex;
  gap: 6px;
  margin-top: 12px;
}
.ingest-row input {
  flex: 1;
  padding: 7px 10px;
  border: 1px solid var(--border);
  border-radius: 3px;
  font-size: 12.5px;
  background: var(--bg);
  font-family: var(--mono);
}
.ingest-msg {
  margin-top: 8px;
  font-size: 12px;
  color: var(--muted);
}
.empty {
  padding: 20px 8px;
  font-style: italic;
}

/* ─────────── 拖拽上传覆盖层 ─────────── */
.kb-drop-overlay {
  position: absolute;
  inset: 10px;
  z-index: 50;
  background: rgba(44, 70, 97, 0.07);
  border: 2px dashed var(--primary);
  /* 黑夜模式见文件底部 html[data-theme="dark"] 覆盖 */
  border-radius: 14px;
  display: flex;
  align-items: center;
  justify-content: center;
  backdrop-filter: blur(1px);
  pointer-events: none;
}
.kb-drop-card {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 10px;
  color: var(--primary);
  text-align: center;
  padding: 0 24px;
}
.kb-drop-title {
  font-family: var(--serif);
  font-size: 18px;
  font-weight: 600;
  letter-spacing: 1px;
}
.kb-drop-sub {
  font-size: 12.5px;
  color: var(--muted);
}

/* ─────────── 上传进度面板 ─────────── */
.upload-panel {
  position: absolute;
  right: 18px;
  bottom: 18px;
  z-index: 40;
  width: 320px;
  max-height: 50vh;
  overflow-y: auto;
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: 10px;
  box-shadow: var(--shadow-lg);
  padding: 10px 12px;
}
.upload-head {
  font-size: 12px;
  font-weight: 600;
  color: var(--text);
  margin-bottom: 8px;
}
.upload-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 0;
  font-size: 12px;
}
.upload-row.loading {
  color: var(--muted);
}
.upload-row.ok {
  color: var(--ok);
}
.upload-row.err {
  color: var(--vermilion);
}
.up-name {
  font-weight: 500;
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 130px;
}
.up-detail {
  color: var(--dim);
  font-size: 11px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  flex: 1;
}
.spin {
  animation: spin 0.9s linear infinite;
}
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}

/* 黑夜模式：拖拽覆盖层的浅蓝雾在深空底上隐形，换成主色蓝雾 */
html[data-theme="dark"] .kb-drop-overlay {
  background: rgba(91, 140, 255, 0.12);
}

/* ═══════════ 全盘资源归集 · 琉璃牛轧糖(v4) ═══════════ */
.body.overview.rg {
  gap: 16px;
  overflow-y: auto;
  --rg-gold: #d4b06a;
  --rg-gold-soft: #f0dcae;
  --rg-blue: #7fa8d9;
  --rg-stroke: rgba(150, 140, 120, 0.18);
}
.rg .glass {
  position: relative;
  border-radius: 18px;
  border: 1px solid var(--rg-stroke);
  background: linear-gradient(
    155deg,
    rgba(246, 229, 198, 0.1),
    rgba(255, 250, 243, 0.04)
  );
  backdrop-filter: blur(22px) saturate(140%);
  -webkit-backdrop-filter: blur(22px) saturate(140%);
  box-shadow: 0 14px 40px -22px rgba(0, 0, 0, 0.55),
    inset 0 1px 0 rgba(255, 255, 255, 0.12);
}
html[data-theme="dark"] .rg .glass {
  background: linear-gradient(
    155deg,
    rgba(231, 201, 138, 0.08),
    rgba(255, 250, 243, 0.03)
  );
}

/* hero */
.rg-hero {
  padding: 22px 24px;
}
.rg-hero-title {
  font-family: var(--serif);
  font-size: 20px;
  font-weight: 600;
  color: var(--text);
  letter-spacing: 0.5px;
}
.rg-hero-sub {
  margin-top: 6px;
  font-size: 13px;
  color: var(--text-2);
  line-height: 1.7;
}
.b-res {
  color: var(--rg-blue);
}
.b-core {
  color: var(--rg-gold);
}
.rg-roots {
  margin-top: 16px;
  display: flex;
  flex-wrap: wrap;
  gap: 9px;
}
.rg-rootchip {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  padding: 6px 14px;
  border-radius: 999px;
  border: 1px solid var(--rg-stroke);
  background: rgba(255, 255, 255, 0.04);
  color: var(--text-2);
  font-size: 12.5px;
  cursor: pointer;
  transition: all 0.18s;
}
.rg-rootchip .dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--muted);
  transition: all 0.18s;
}
.rg-rootchip.on {
  border-color: var(--rg-gold);
  background: rgba(212, 176, 106, 0.14);
  color: var(--text);
}
.rg-rootchip.on .dot {
  background: var(--rg-gold);
  box-shadow: 0 0 8px rgba(212, 176, 106, 0.8);
}
.rg-scanrow {
  margin-top: 16px;
  display: flex;
  align-items: center;
  gap: 14px;
}
.rg-meta {
  font-size: 12px;
  color: var(--muted);
}
.rg-meta b {
  color: var(--rg-gold);
}
.rg-primary {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  padding: 9px 22px;
  border: none;
  border-radius: 12px;
  background: linear-gradient(160deg, var(--rg-gold-soft), var(--rg-gold));
  color: #2a2410;
  font-weight: 700;
  font-size: 13.5px;
  cursor: pointer;
  box-shadow: 0 8px 20px -9px rgba(212, 176, 106, 0.85);
}
.rg-primary.sm {
  padding: 8px 16px;
  font-size: 13px;
}
.rg-primary:disabled {
  opacity: 0.55;
  cursor: default;
}

/* 表格 */
.rg-table {
  display: flex;
  flex-direction: column;
  overflow: hidden;
  min-height: 280px;
}
.rg-ttop {
  display: flex;
  align-items: center;
  gap: 9px;
  padding: 12px 16px;
  border-bottom: 1px solid var(--rg-stroke);
  flex-wrap: wrap;
}
.rg-ttitle {
  font-weight: 600;
  color: var(--text);
  font-size: 13.5px;
}
.rg-tmeta {
  font-size: 12px;
  color: var(--muted);
}
.rg-spacer {
  flex: 1;
}
.rg-chip {
  font-size: 12px;
  padding: 6px 12px;
  border-radius: 9px;
  border: 1px solid var(--rg-stroke);
  background: rgba(255, 255, 255, 0.04);
  color: var(--text-2);
  cursor: pointer;
}
.rg-chip.blue {
  background: rgba(127, 168, 217, 0.16);
  color: var(--rg-blue);
  border-color: rgba(127, 168, 217, 0.34);
  font-weight: 700;
}
.rg-chip.gold {
  background: linear-gradient(160deg, var(--rg-gold-soft), var(--rg-gold));
  color: #2a2410;
  border-color: transparent;
  font-weight: 700;
}
.rg-chip:disabled {
  opacity: 0.45;
  cursor: default;
}
.rg-row {
  display: grid;
  grid-template-columns: 34px minmax(120px, 1.4fr) 56px minmax(160px, 2.2fr) 76px 70px;
  align-items: center;
  gap: 10px;
  padding: 0 16px;
  height: 46px;
  font-size: 12.5px;
  color: var(--text-2);
  border-bottom: 1px solid var(--hairline);
}
.rg-row.rg-hd {
  height: 38px;
  color: var(--muted);
  font-size: 11.5px;
  letter-spacing: 0.04em;
  border-bottom: 1px solid var(--rg-stroke);
}
.rg-row.dim {
  opacity: 0.5;
}
.rg-list {
  flex: 1;
  max-height: calc(100vh - 470px);
  min-height: 180px;
  overflow-y: auto;
}
.rg-list .rg-row {
  cursor: pointer;
}
.rg-list .rg-row:hover {
  background: rgba(255, 255, 255, 0.04);
}
.rg-more-note {
  padding: 12px 16px;
  font-size: 11.5px;
  color: var(--muted);
  text-align: center;
}
.rg-row .c-name {
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.rg-row .c-prev {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.rg-row .c-score {
  color: var(--rg-gold);
  letter-spacing: -1px;
  white-space: nowrap;
}
.rg-row .c-dest {
  cursor: pointer;
  font-size: 11px;
  white-space: nowrap;
}
.c-dest .d-res {
  color: var(--rg-blue);
}
.c-dest .d-core {
  color: var(--rg-gold);
  margin-left: 3px;
}
.c-dest .d-x {
  color: var(--muted);
}
.ty {
  display: inline-block;
  font-size: 11px;
  padding: 1px 7px;
  border-radius: 5px;
}
.ty-doc,
.ty-text {
  background: rgba(127, 168, 217, 0.16);
  color: var(--rg-blue);
}
.ty-sheet,
.ty-data {
  background: rgba(123, 191, 138, 0.16);
  color: #6bbf86;
}
.ty-slide {
  background: rgba(212, 176, 106, 0.16);
  color: var(--rg-gold);
}
.ty-image,
.ty-video,
.ty-audio {
  background: rgba(177, 150, 214, 0.16);
  color: #b196d6;
}
.ty-archive,
.ty-code,
.ty-other {
  background: rgba(150, 140, 120, 0.16);
  color: var(--text-2);
}
.rg-archmsg {
  padding: 10px 16px;
  font-size: 12.5px;
  color: var(--rg-gold);
  border-top: 1px solid var(--rg-stroke);
}
.rg-empty {
  padding: 28px;
  text-align: center;
  font-size: 13px;
}

/* 对话框 */
.rg-chat {
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
.rg-chat-head {
  padding: 11px 16px;
  font-size: 13px;
  font-weight: 700;
  color: var(--rg-gold);
  border-bottom: 1px solid var(--rg-stroke);
}
.rg-chat-body {
  padding: 12px 16px;
  max-height: 200px;
  overflow-y: auto;
}
.rg-bubble {
  margin: 7px 0;
  padding: 8px 13px;
  border-radius: 12px;
  max-width: 86%;
  font-size: 13px;
  line-height: 1.6;
}
.rg-bubble.u {
  margin-left: auto;
  background: linear-gradient(160deg, rgba(212, 176, 106, 0.22), rgba(212, 176, 106, 0.1));
  color: var(--text);
  border: 1px solid rgba(212, 176, 106, 0.22);
}
.rg-bubble.a {
  background: rgba(255, 255, 255, 0.05);
  color: var(--text-2);
  border: 1px solid var(--rg-stroke);
}
.rg-chat-input {
  display: flex;
  gap: 9px;
  padding: 11px 16px;
  border-top: 1px solid var(--rg-stroke);
}
.rg-chat-input input {
  flex: 1;
  background: rgba(0, 0, 0, 0.14);
  border: 1px solid var(--rg-stroke);
  border-radius: 11px;
  padding: 9px 14px;
  color: var(--text);
  font-size: 13px;
}
.rg-chat-input input::placeholder {
  color: var(--muted);
}
</style>
