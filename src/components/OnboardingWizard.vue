<script setup lang="ts">
/**
 * 智能向导「让 AI 更懂你」—— 一条龙引导:盘点 → 配模型 → 归类 → 图谱 → 索引 → 进对话。
 *
 * 全程复用既有命令(零新后端轮子):
 *   盘点  fc.scanFolders / fc.inventoryStart(fable:inventory)
 *   归类  fc.smartCluster(quick: 全覆盖词法 + AI 命名, file:cluster) ‖ fc.clusterBuild(离线词法)
 *   索引  fc.indexStart(后台向量索引, fable:index)
 *   画像  fc.profileHtml(确定性桌面 HTML)
 * 收尾据文件构成给「建议工作流」卡片,点一下带着任务跳进对话框。
 */
import { ref, reactive, computed, onBeforeUnmount, defineAsyncComponent } from "vue";
import {
  Sparkles,
  FolderSearch,
  Wand2,
  Radar,
  Orbit,
  X,
  Check,
  KeyRound,
  Brain,
  FileText,
  LoaderCircle,
  ChevronRight,
  Layers,
  Folder,
  User,
  Building2,
  Network,
  ShieldCheck,
} from "@lucide/vue";
import OrbitSpinner from "./icons/OrbitSpinner.vue";
import {
  files as fc,
  artifacts as artifactsApi,
  listen,
  invoke,
  openUrl,
  type ScanRootInfo,
  type FileOverview,
} from "../tauri";
import { useAppStore } from "../stores/app";
import { useWorkflowsStore } from "../stores/workflows";
import { useWizardStore } from "../stores/wizard";
import { useFileTasksStore } from "../stores/fileTasks";
// 星河图谱依赖 cytoscape(~562KB),只在「知识图谱」步骤(v-if 网住)才渲染 → 按需加载。
const KnowledgeGraph = defineAsyncComponent(() => import("./KnowledgeGraph.vue"));

const app = useAppStore();
const workflows = useWorkflowsStore();
const wiz = useWizardStore();
const tasks = useFileTasksStore();

type Step = "intro" | "profile" | "scope" | "scan" | "model" | "organize" | "graph" | "finish";
// 进度条按画像走两套用词:个人=聚类「智能归类」,企业=框架「构建体系」。
const STEPS = computed<{ key: Step; label: string }[]>(() => {
  const enterprise = wiz.profile === "enterprise";
  return [
    { key: "scope", label: "盘点范围" },
    { key: "scan", label: "全盘扫描" },
    { key: "model", label: "配模型" },
    { key: "organize", label: enterprise ? "构建知识体系" : "智能归类" },
    ...(enterprise ? [] : [{ key: "graph" as Step, label: "知识图谱" }]),
    { key: "finish", label: "进对话" },
  ];
});
const step = ref<Step>("intro");
const stepIndex = computed(() => STEPS.value.findIndex((s) => s.key === step.value));

// ── 盘点范围 ──
const roots = ref<ScanRootInfo[]>([]);
const loadingRoots = ref(false);
const rootsErr = ref("");
const uncheckedRoots = reactive(new Set<string>());
const excludeKeywords = ref("");
function loadRoots() {
  loadingRoots.value = true;
  rootsErr.value = "";
  fc.scanFolders(null)
    .then((r) => {
      roots.value = r.roots;
      uncheckedRoots.clear();
      // 默认全勾(后端现在「一个不落」把所有盘/卷 defaultOn=true);只把 defaultOn=false 的预置为不勾。
      for (const x of r.roots) if (!x.defaultOn) uncheckedRoots.add(x.path);
      // 后台逐个算每个盘/目录的真实体量(限并发,跟文件中心·盘点选择器一致)。
      sizeCache.clear();
      sizeQueue.length = 0;
      sizeInflight.clear();
      for (const x of r.roots) requestSize(x.path);
    })
    .catch((e: any) => (rootsErr.value = `扫描失败:${e?.message ?? e}`))
    .finally(() => (loadingRoots.value = false));
}

// ── 体积:限并发的后台计算队列(复用后端 fable_folder_size,带 10s 死线兜底)──
const sizeCache = reactive(new Map<string, { files: number; bytes: number }>());
const sizeQueue: string[] = [];
const sizeInflight = new Set<string>();
let sizeActive = 0;
const SIZE_CONCURRENCY = 4;
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
// 仿 WizTree:每个盘/目录在「所有范围」里的占比 + 条长(占比=本项/已知总和;条长=本项/已知最大)。
const rootSizeStats = computed(() => {
  let sum = 0;
  let max = 1;
  for (const r of roots.value) {
    const s = sizeCache.get(r.path);
    if (s) {
      sum += s.bytes;
      if (s.bytes > max) max = s.bytes;
    }
  }
  return { sum, max };
});
function rootSizePct(r: ScanRootInfo): number {
  const s = sizeCache.get(r.path);
  if (!s || rootSizeStats.value.sum <= 0) return 0;
  return (s.bytes / rootSizeStats.value.sum) * 100;
}
function rootSizeBar(r: ScanRootInfo): number {
  const s = sizeCache.get(r.path);
  if (!s || rootSizeStats.value.max <= 0) return 0;
  return s.bytes / rootSizeStats.value.max;
}
function fmtBytes(b: number): string {
  if (b <= 0) return "0 B";
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  let n = b;
  while (n >= 1024 && i < u.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n >= 100 || i === 0 ? Math.round(n) : n.toFixed(1)} ${u[i]}`;
}
const keywordList = computed(() =>
  excludeKeywords.value
    .split(/[,，;；\s]+/)
    .map((s) => s.trim().toLowerCase())
    .filter(Boolean),
);
function matchesKeyword(r: ScanRootInfo): boolean {
  if (!keywordList.value.length) return false;
  const hay = (r.path + " " + r.label).toLowerCase();
  return keywordList.value.some((k) => hay.includes(k));
}
function isRootOn(r: ScanRootInfo): boolean {
  return !uncheckedRoots.has(r.path) && !matchesKeyword(r);
}
function toggleRoot(r: ScanRootInfo) {
  if (uncheckedRoots.has(r.path)) uncheckedRoots.delete(r.path);
  else uncheckedRoots.add(r.path);
}
function selectAll(on: boolean) {
  uncheckedRoots.clear();
  if (!on) for (const r of roots.value) uncheckedRoots.add(r.path);
}
const selectedRoots = computed(() => roots.value.filter(isRootOn).map((r) => r.path));

// ── 扫描 ──
const scanFiles = ref(0);
const scanMsg = ref("");
// 扫描被取消 / 出错的恢复态:置位后停掉转圈动画、亮出「重新扫描 / 返回」出口,
// 否则向导会永远停在 scan 这一步(只有 done 才会 afterScan 往下走)。
const scanFailed = ref(false);
const scanCancelled = ref(false);
let unScan: (() => void) | null = null;
async function startScan() {
  step.value = "scan";
  scanFiles.value = 0;
  scanFailed.value = false;
  scanCancelled.value = false;
  scanMsg.value = "正在扫描你电脑上的文件…";
  try {
    if (!unScan) {
      unScan = await listen<{ kind: string; files?: number; message?: string }>(
        "fable:inventory",
        (p) => {
          // 已离开 scan 步(转后台 / 已往下走)就不再让迟到的事件改动这屏 UI;
          // 但若后台真扫完了,仍把流程推进下去。
          if (step.value !== "scan") {
            if (p.kind === "done") void afterScan();
            return;
          }
          if (p.kind === "progress") {
            scanFailed.value = false;
            scanFiles.value = p.files ?? 0;
            scanMsg.value = `已扫描 ${scanFiles.value.toLocaleString()} 个文件…`;
          } else if (p.kind === "done") {
            scanFiles.value = p.files ?? scanFiles.value;
            scanMsg.value = `扫描完成 · 共 ${scanFiles.value.toLocaleString()} 个文件`;
            void afterScan();
          } else if (p.kind === "error") {
            // 后端取消时发的是 error{message:"已取消"};与真实错误区分文案,但都给恢复出口。
            scanCancelled.value = (p.message ?? "").includes("已取消");
            scanFailed.value = true;
            scanMsg.value = scanCancelled.value ? "扫描已取消" : `扫描失败:${p.message ?? ""}`;
          }
        },
      );
    }
    // 托管全局任务 store(全局任务中心可见 + 后台跑);向导自身的 fable:inventory 监听仍负责推进步骤。
    await tasks.startInventory(selectedRoots.value, []);
  } catch (e: any) {
    scanFailed.value = true;
    scanMsg.value = `扫描失败:${e?.message ?? e}`;
  }
}
// 失败 / 取消后「返回」重选盘点范围。
function backToScope() {
  scanFailed.value = false;
  scanCancelled.value = false;
  step.value = "scope";
}
async function afterScan() {
  await loadOverview();
  await loadSense();
  step.value = "model";
}

// ── 总览 ──
const overview = ref<FileOverview | null>(null);
async function loadOverview() {
  try {
    overview.value = await fc.overview(null);
  } catch {
    overview.value = null;
  }
}

// ── 模型 / 归类方式 ──
const mode = ref<"ai" | "offline">("ai");
function goConfigEmbed() {
  app.setView("sense_api");
  wiz.closeWizard();
}

// ── 前置「寓言计划」感官模型配置:嵌入 key(决定语义聚类/检索质量)+ 本地模型下载 ──
interface SenseProv {
  id: string;
  name: string;
  kind: string;
  key_ready: boolean;
  enabled: boolean;
  get_key_url: string;
  installed: boolean;
  pack_id: string | null;
  free: boolean;
}
const embedProv = ref<SenseProv | null>(null);
const localPacks = ref<SenseProv[]>([]);
const embedKey = ref("");
const embedSaving = ref(false);
const embedMsg = ref("");
const packPct = reactive<Record<string, number>>({});
let unPack: (() => void) | null = null;
async function loadSense() {
  try {
    const ov: any = await invoke("sense_list");
    const provs: SenseProv[] = (ov.groups || []).flatMap((g: any) => g.providers || []);
    embedProv.value = provs.find((p) => p.id === "siliconflow-embed") ?? null;
    localPacks.value = provs.filter((p) => p.kind === "local");
  } catch {
    /* 浏览器/降级模式忽略 */
  }
}
async function saveEmbedKey() {
  if (!embedProv.value || embedSaving.value || !embedKey.value.trim()) return;
  embedSaving.value = true;
  embedMsg.value = "";
  try {
    await invoke("sense_set", { id: embedProv.value.id, apiKey: embedKey.value.trim(), enabled: true });
    embedKey.value = "";
    await loadSense();
    await loadOverview();
    embedMsg.value = embedProv.value?.key_ready ? "已配置 ✓ 语义检索就绪" : "已保存";
  } catch (e: any) {
    embedMsg.value = `保存失败:${e?.message ?? e}`;
  } finally {
    embedSaving.value = false;
  }
}
async function downloadPack(p: SenseProv) {
  if (!p.pack_id) return;
  if (!unPack) {
    unPack = await listen<{ id: string; kind: string; pct?: number; message?: string }>(
      "sense:pack",
      (ev) => {
        if (ev.kind === "progress" || ev.kind === "phase") {
          packPct[ev.id] = ev.pct ?? packPct[ev.id] ?? 0;
        } else {
          delete packPct[ev.id];
          void loadSense();
        }
      },
    );
  }
  packPct[p.pack_id] = 0;
  invoke("sense_pack_install", { id: p.pack_id }).catch(() => {
    if (p.pack_id) delete packPct[p.pack_id];
  });
}
function openKeyUrl() {
  if (embedProv.value?.get_key_url) openUrl(embedProv.value.get_key_url).catch(() => {});
}

// ── 归类 ──
const organizing = ref(false);
const organizeMsg = ref("");
let unCluster: (() => void) | null = null;
// 归类(智能 smartCluster / 离线词法 clusterBuild)进度与完成都走 file:cluster 事件
// (phase/tier/done/error)。必须等 done 再进图谱,否则聚类还没落库、星图会是空的。
// 两条路共用这一个监听器,避免同一频道挂两个监听导致 afterOrganize 触发两次。
async function ensureClusterListener() {
  if (unCluster) return;
  unCluster = await listen<{ kind: string; text?: string; note?: string; message?: string }>(
    "file:cluster",
    (p) => {
      if (p.kind === "phase") organizeMsg.value = p.text ?? organizeMsg.value;
      else if (p.kind === "tier") organizeMsg.value = p.note ?? organizeMsg.value;
      else if (p.kind === "done") {
        organizeMsg.value = p.note ?? "归类完成";
        void afterOrganize();
      } else if (p.kind === "error") {
        organizeMsg.value = `归类失败:${p.message ?? ""}`;
        organizing.value = false;
      }
    },
  );
}
// 离线词法归类:file_cluster_build 现为后台事件式(fire-and-forget)。
async function runOfflineCluster() {
  await ensureClusterListener();
  await fc.clusterBuild(null);
}
// 企业路径(D 方案):Schema-Guided 在框内抽三元组,事件走 fable:ontology。
const ontoKept = ref(0);
let unOnto: (() => void) | null = null;
async function runSchemaExtract() {
  if (!unOnto) {
    unOnto = await listen<{ kind: string; text?: string; kept?: number; note?: string; message?: string }>(
      "fable:ontology",
      (p) => {
        if (p.kind === "phase") organizeMsg.value = p.text ?? organizeMsg.value;
        else if (p.kind === "tick") organizeMsg.value = "模型正在框内抽取关系…";
        else if (p.kind === "done") {
          ontoKept.value = p.kept ?? 0;
          organizeMsg.value = p.note ?? "知识体系构建完成";
          void afterOrganize();
        } else if (p.kind === "error") {
          organizeMsg.value = `构建失败:${p.message ?? ""}`;
          organizing.value = false;
        }
      },
    );
  }
  // 走全局任务 store → 抽取也出现在右下角后台任务浮球,切走/最小化仍可见、完成有提醒。
  await tasks.startOntology(wiz.schemaId);
}
async function startOrganize() {
  step.value = "organize";
  organizing.value = true;
  // 归类要跑几秒,趁这空当把星河图谱(cytoscape ~562KB)的 chunk 预下载好 →
  // 归类一完成切到「知识图谱」步时,图谱「啪」地直接渲染,不再等大包下载的白屏。
  void import("./KnowledgeGraph.vue").catch(() => {});
  // 企业 → D 方案 Schema-Guided;个人 → B 方案聚类。
  if (wiz.method === "schema") {
    organizeMsg.value = `正在按「${chosenSchema.value?.name ?? "行业框"}」抽取实体与关系…`;
    try {
      await runSchemaExtract();
    } catch (e: any) {
      organizeMsg.value = `构建失败:${e?.message ?? e}`;
      organizing.value = false;
    }
    return;
  }
  if (mode.value === "offline") {
    organizeMsg.value = "正在按文件夹 / 名称离线归类(无需联网)…";
    try {
      await runOfflineCluster();
    } catch (e: any) {
      organizeMsg.value = `归类失败:${e?.message ?? e}`;
      organizing.value = false;
    }
    return;
  }
  // 智能归类(v3 全覆盖):T0 词法把**全库每个文件**都归类(不再像旧 file_cluster_llm 只看最近 240 个)
  // + T1 大模型读「簇画像」起亲切名/理关系。quick=true 跳过耗时的 T2 全量向量化(向导收尾自己会
  // 后台建索引,且大库 T2 要几十分钟、会爆「几秒就好」的预期)。AI 命名失败也会优雅降级保留词法名、
  // 照常 done,不再需要单独回落离线。
  organizeMsg.value = "正在快速归类你的全部文件、再请 AI 读懂起名…";
  try {
    await ensureClusterListener();
    await fc.smartCluster(null, true);
  } catch (e: any) {
    organizeMsg.value = `归类失败:${e?.message ?? e}`;
    organizing.value = false;
  }
}
async function afterOrganize() {
  await loadOverview();
  organizing.value = false;
  // 企业(D 方案)没有文件聚类星图,直接进收尾;个人(B 方案)先看知识图谱。
  step.value = wiz.method === "schema" ? "finish" : "graph";
  if (step.value === "finish") void finishUp();
}

// ── 知识图谱:复用 KnowledgeGraph.vue 的星河渲染(source=files),数据来自后端 file_graph ──
const topThemeCount = computed(() => (overview.value?.clusters ?? []).filter((c) => c.parent === 0).length);

// ── 收尾:后台建索引 + 生成桌面画像 + 推荐工作流 ──
const finishing = ref(false);
const profilePath = ref("");
const indexKicked = ref(false);
async function finishUp() {
  step.value = "finish";
  finishing.value = true;
  // 1) 后台建向量索引,托管全局任务 store(全局任务中心可见 + 关掉向导/切界面照常后台跑)。
  tasks.startIndex().then(() => (indexKicked.value = true)).catch(() => {});
  // 2) 确定性生成桌面画像(秒级,不调大模型)。
  try {
    profilePath.value = await fc.profileHtml(null);
  } catch {
    profilePath.value = "";
  }
  finishing.value = false;
  // 3) 大模型据真实知识库智能匹配收尾建议(数秒,后台跑;不阻塞「完成」按钮)。
  void loadSuggestions();
}
function openProfile() {
  if (profilePath.value) artifactsApi.openExternal(profilePath.value).catch(() => {});
}

interface FlowCard {
  title: string;
  prompt: string;
}
// 收尾建议改成「大模型据真实知识库智能匹配」:不再是固定阈值套话。
// 先用本地确定性兜底秒填(保证立刻有卡片可点),LLM 结果回来再替换。
const suggested = ref<FlowCard[]>([]);
const suggesting = ref(false);

// 本地确定性兜底:据类型分布给几张通用卡(仅当后端 LLM 彻底不可用时兜底)。
function localFallbackFlows(): FlowCard[] {
  const ov = overview.value;
  const cnt = (k: string) => ov?.byKind.find((x) => x.kind === k)?.count ?? 0;
  const out: FlowCard[] = [];
  if (cnt("video") >= 5)
    out.push({
      title: "把影视素材做成作品集",
      prompt:
        "我电脑里有不少视频素材(已盘点进文件中心)。请帮我从中挑出代表作,配上简介与封面思路,生成一个可分享的作品集网页。先列出你打算收录哪些、为什么。",
    });
  if (cnt("doc") + cnt("text") >= 8)
    out.push({
      title: "为我的文档写一篇结构化总结",
      prompt:
        "我的文档/资料已经盘点进知识库。请沿知识库通读后,按主题归纳要点、待办与关键结论,产出一份结构化总览(Markdown)。",
    });
  if (cnt("image") >= 20)
    out.push({
      title: "整理图片成图集",
      prompt: "我有很多图片已盘点进文件中心。请按场景/时间帮我归类,挑出精选,排成一个图集页面。",
    });
  if (cnt("audio") >= 3)
    out.push({
      title: "把录音转写并归档",
      prompt: "我有一些录音/音频已盘点进文件中心。请帮我规划把它们转写成文字、提炼摘要并沉淀进知识库的流程。",
    });
  if (!out.length)
    out.push({
      title: "从一个问题开始",
      prompt: "看了我的文件,你觉得我最近在忙什么?请基于知识库给我三条具体的下一步建议。",
    });
  return out;
}

// 让大模型读真实知识库(主题/类型/语言 + 可抽查文件)智能匹配 3~5 条「我能立刻替你做的事」。
async function loadSuggestions() {
  suggesting.value = true;
  suggested.value = localFallbackFlows(); // 立刻有卡片,LLM 回来再替换
  try {
    const flows = await fc.suggestWorkflows(null);
    if (flows && flows.length) suggested.value = flows;
  } catch {
    /* 后端自身已会回落;真彻底失败就保留本地兜底卡 */
  } finally {
    suggesting.value = false;
  }
}
function useFlow(f: FlowCard) {
  app.setView("chat");
  // ChatPanel 挂载后其 insertRequest watch 才生效,稍延迟再注入。
  window.setTimeout(() => workflows.insertText(f.prompt), 180);
  finishDone();
}

// ── 画像:个人(B 聚类)/ 企业(D Schema-Guided)──
interface SchemaCard {
  id: string;
  name: string;
  industry: string;
  source: string;
  desc: string;
  entities: { id: string; name: string; hint: string }[];
  relations: { id: string; name: string; hint: string }[];
  triples: number;
}
const schemaList = ref<SchemaCard[]>([]);
const schemasLoading = ref(false);
async function loadSchemas() {
  if (schemaList.value.length) return;
  schemasLoading.value = true;
  try {
    schemaList.value = await invoke<SchemaCard[]>("ontology_schemas");
  } catch {
    schemaList.value = [];
  } finally {
    schemasLoading.value = false;
  }
}
function pickPersonal() {
  wiz.setProfile("personal");
  step.value = "scope";
  loadRoots();
}
function pickEnterprise() {
  wiz.setProfile("enterprise");
  void loadSchemas();
  // 留在 profile 步展开行业框选择;选定后再进 scope。
}
function pickSchema(id: string) {
  wiz.setSchema(id);
}
function profileNext() {
  // 企业必须先选一个行业框。
  if (wiz.profile === "enterprise" && !wiz.schemaId) return;
  step.value = "scope";
  loadRoots();
}
const chosenSchema = computed(() => schemaList.value.find((s) => s.id === wiz.schemaId) || null);

// ── 生命周期 ──
function begin() {
  step.value = "profile";
  if (wiz.profile === "enterprise") void loadSchemas();
}
// 转入后台 / X / 点遮罩:只隐藏浮层,**不动 step**,后台扫描/归类继续 —— 回来还在原地。
function close() {
  wiz.closeWizard();
}
// 「完成」/进对话:一段流程收尾,下次打开从头开始。
function finishDone() {
  wiz.closeWizard();
  step.value = "intro";
}
// 长任务进行中(扫描/归类),给「转入后台」一个更明确的语义入口。
const inLongStep = computed(() => step.value === "scan" || step.value === "organize");
// 整个组件常驻 App,不随视图切换卸载;onBeforeUnmount 基本只在 App 退出时触发。
onBeforeUnmount(() => {
  if (unScan) unScan();
  if (unCluster) unCluster();
  if (unPack) unPack();
  if (unOnto) unOnto();
});
</script>

<template>
  <transition name="wiz-fade">
    <div v-show="wiz.open" class="wiz-scrim" @click.self="close">
      <div class="wiz glass" :class="{ big: step === 'graph' }">
        <button class="wiz-x" :title="inLongStep ? '转入后台(任务继续跑)' : '关闭'" @click="close"><X :size="17" :stroke-width="2" /></button>

        <!-- 进度条(intro 不显示) -->
        <div v-if="step !== 'intro'" class="wiz-steps">
          <div
            v-for="(s, i) in STEPS"
            :key="s.key"
            class="ws"
            :class="{ on: i === stepIndex, done: i < stepIndex }"
          >
            <span class="ws-dot"><Check v-if="i < stepIndex" :size="11" :stroke-width="3" /><template v-else>{{ i + 1 }}</template></span>
            <span class="ws-lab">{{ s.label }}</span>
          </div>
        </div>

        <!-- 0 · 欢迎 -->
        <div v-if="step === 'intro'" class="wiz-body intro">
          <div class="intro-orb"><Sparkles :size="34" :stroke-width="1.3" /></div>
          <h2>让 AI 更懂你</h2>
          <p class="intro-sub">
            三步、几分钟,把你电脑里的文件盘点一遍,自动归类成知识图谱,
            建好可搜索的索引 —— 之后 AI 就能沿着你的资料替你干活。
          </p>
          <ol class="intro-flow">
            <li><FolderSearch :size="15" :stroke-width="1.7" /> 全盘扫描:一个文件都不落下</li>
            <li><Wand2 :size="15" :stroke-width="1.7" /> 智能归类:自动分主题、画图谱</li>
            <li><Radar :size="15" :stroke-width="1.7" /> 建索引 + 进对话:让 AI 懂你想干的事</li>
          </ol>
          <button class="wiz-go" @click="begin"><Sparkles :size="16" :stroke-width="1.8" /> 开始</button>
          <button class="wiz-skip" @click="close">跳过,稍后我自己来</button>
        </div>

        <!-- 0.5 · 选画像:个人(B 聚类)/ 企业(D Schema) -->
        <div v-else-if="step === 'profile'" class="wiz-body">
          <h3 class="wiz-h"><Sparkles :size="16" :stroke-width="1.7" /> 先告诉我:这是谁的知识库?</h3>
          <p class="wiz-tip">我会据此**自动匹配**最合适的构建方案 —— 不用你懂技术。</p>
          <div class="prof-cards">
            <button class="prof-card" :class="{ on: wiz.profile === 'personal' }" @click="pickPersonal">
              <div class="pc-ic"><User :size="22" :stroke-width="1.6" /></div>
              <span class="pc-t">个人知识库</span>
              <span class="pc-d">我的电脑、我的资料。自动发现「我常用的几摊事」,起好中文名,一眼认出。</span>
              <span class="pc-tag">聚类驱动 · 自动发现</span>
            </button>
            <button class="prof-card" :class="{ on: wiz.profile === 'enterprise' }" @click="pickEnterprise">
              <div class="pc-ic"><Building2 :size="22" :stroke-width="1.6" /></div>
              <span class="pc-t">企业知识库</span>
              <span class="pc-d">公司 / 行业资料。先选行业框,在框内抽出实体与关系,每条可溯源、可审计。</span>
              <span class="pc-tag">框架派 · 低幻觉可审计</span>
            </button>
          </div>

          <!-- 企业:选行业框(中文) -->
          <div v-if="wiz.profile === 'enterprise'" class="schema-box">
            <div class="schema-head"><Network :size="14" :stroke-width="1.7" /> <b>选一个行业框</b><span class="schema-fine">框里只用给定的实体/关系类型,模型「做选择题」→ 不乱编</span></div>
            <div v-if="schemasLoading" class="wiz-loading"><OrbitSpinner :size="16" /> 加载行业框…</div>
            <div v-else class="schema-grid">
              <button
                v-for="sc in schemaList"
                :key="sc.id"
                class="schema-card"
                :class="{ on: wiz.schemaId === sc.id }"
                @click="pickSchema(sc.id)"
              >
                <span class="sc-name">{{ sc.name }}</span>
                <span class="sc-ind">{{ sc.industry }}</span>
                <span class="sc-types">
                  {{ sc.entities.slice(0, 5).map((e) => e.name).join(' · ') }}
                </span>
                <span class="sc-src">源自 {{ sc.source }}</span>
              </button>
            </div>
            <div v-if="chosenSchema" class="schema-preview">
              <ShieldCheck :size="13" :stroke-width="1.8" />
              <span>「{{ chosenSchema.name }}」框内:<b>{{ chosenSchema.entities.length }}</b> 类实体、<b>{{ chosenSchema.relations.length }}</b> 类关系 —— {{ chosenSchema.relations.slice(0, 5).map((r) => r.name).join('、') }}</span>
            </div>
          </div>

          <div class="wiz-foot end">
            <button v-if="wiz.profile" class="wiz-go" :disabled="wiz.profile === 'enterprise' && !wiz.schemaId" @click="profileNext">
              <ChevronRight :size="15" :stroke-width="1.8" /> 下一步:盘点资料
            </button>
          </div>
        </div>

        <!-- 1 · 盘点范围 -->
        <div v-else-if="step === 'scope'" class="wiz-body">
          <h3 class="wiz-h">选择要盘点的范围</h3>
          <p class="wiz-tip">
            已<b>默认全选</b>你电脑上所有可访问的盘 / 目录(系统、缓存目录会自动跳过)。
            想缩小范围就取消勾选,或在下方填关键词排除。
          </p>
          <div v-if="loadingRoots" class="wiz-loading"><OrbitSpinner :size="18" /> 正在列出可盘点的盘…</div>
          <div v-else-if="rootsErr" class="wiz-err">{{ rootsErr }}</div>
          <template v-else>
            <div class="root-tools">
              <button class="mini" @click="selectAll(true)">全选</button>
              <button class="mini" @click="selectAll(false)">全不选</button>
              <input v-model="excludeKeywords" class="kw" placeholder="排除含这些词的目录(逗号分隔,如 备份, temp)" />
            </div>
            <div class="root-list">
              <button
                v-for="r in roots"
                :key="r.path"
                class="root-row"
                :class="{ off: !isRootOn(r) }"
                @click="toggleRoot(r)"
              >
                <span class="root-check" :class="{ on: isRootOn(r) }"><Check v-if="isRootOn(r)" :size="12" :stroke-width="2.6" /></span>
                <Layers v-if="r.defaultOn" :size="15" :stroke-width="1.7" class="root-ic" />
                <Folder v-else :size="15" :stroke-width="1.7" class="root-ic" />
                <span class="root-name">{{ r.label }}</span>
                <span class="root-path">{{ r.path }}</span>
                <template v-if="sizeCache.get(r.path)">
                  <span class="root-bar" :title="rootSizePct(r).toFixed(1) + '% 占总量'">
                    <span class="root-bar-fill" :style="{ width: Math.max(2, rootSizeBar(r) * 100) + '%' }" />
                  </span>
                  <span class="root-size">{{ fmtBytes(sizeCache.get(r.path)!.bytes) }}</span>
                </template>
                <span v-else class="root-calc"><OrbitSpinner :size="11" /> 计算体积…</span>
              </button>
            </div>
            <p class="wiz-fine">
              想逐个文件夹精挑?盘点后到「文件中心 · 盘点」里还能更细地选。
            </p>
          </template>
          <div class="wiz-foot">
            <span class="foot-count">已选 <b>{{ selectedRoots.length }}</b> 个范围</span>
            <button class="wiz-go" :disabled="loadingRoots || !selectedRoots.length" @click="startScan">
              <FolderSearch :size="15" :stroke-width="1.8" /> 开始盘点
            </button>
          </div>
        </div>

        <!-- 2 · 扫描中 / 已取消 / 失败 -->
        <div v-else-if="step === 'scan'" class="wiz-body center">
          <!-- 失败 / 取消:停掉转圈,给明确出口,不再让向导卡死在这屏 -->
          <template v-if="scanFailed">
            <div class="scan-orb stopped"><component :is="scanCancelled ? Layers : X" :size="30" :stroke-width="1.4" /></div>
            <div class="scan-lab big">{{ scanMsg }}</div>
            <div class="scan-fine">{{ scanCancelled ? "盘点已停止。你可以重新扫描,或返回调整盘点范围。" : "扫描中断了。可重试,或返回重新选择范围。" }}</div>
            <div class="scan-actions">
              <button class="wiz-go ghost" @click="backToScope"><ChevronRight :size="14" :stroke-width="1.8" class="flip" /> 返回选范围</button>
              <button class="wiz-go" @click="startScan"><FolderSearch :size="15" :stroke-width="1.8" /> 重新扫描</button>
            </div>
          </template>
          <!-- 进行中 -->
          <template v-else>
            <div class="scan-orb"><FolderSearch :size="30" :stroke-width="1.3" /><div class="scan-ring" /></div>
            <div class="scan-num">{{ scanFiles.toLocaleString() }}</div>
            <div class="scan-lab">{{ scanMsg }}</div>
            <button class="wiz-go ghost bg" @click="close"><Layers :size="14" :stroke-width="1.8" /> 转入后台 · 去逛别处</button>
            <div class="scan-fine">扫描在后台继续,可最小化窗口或去用别的功能;扫完再点「智能向导」回来。</div>
          </template>
        </div>

        <!-- 3 · 配模型 -->
        <div v-else-if="step === 'model'" class="wiz-body">
          <h3 class="wiz-h">让 AI 怎么理解你的文件</h3>
          <p class="wiz-tip">扫到 <b>{{ (overview?.totalFiles ?? 0).toLocaleString() }}</b> 个文件。{{ wiz.method === 'schema' ? '配好嵌入模型,下一步在行业框内抽取实体与关系。' : '选一种归类方式,马上分主题、画图谱。' }}</p>
          <div v-if="wiz.method !== 'schema'" class="mode-cards">
            <button class="mode-card" :class="{ on: mode === 'ai' }" @click="mode = 'ai'">
              <Brain :size="20" :stroke-width="1.6" />
              <span class="mc-t">AI 语义归类 <em>推荐</em></span>
              <span class="mc-d">用已连接的对话大模型读文件清单,按内容主题分两级。免嵌入 key,几分钟出结果。</span>
            </button>
            <button class="mode-card" :class="{ on: mode === 'offline' }" @click="mode = 'offline'">
              <Wand2 :size="20" :stroke-width="1.6" />
              <span class="mc-t">离线快速</span>
              <span class="mc-d">按文件夹 / 名称就地归类,不联网、秒出。之后随时能升级成 AI 归类。</span>
            </button>
          </div>
          <!-- 前置「寓言计划」感官模型:嵌入 key + 本地模型,决定语义聚类与检索质量 -->
          <div class="sense-box">
            <div class="sense-head">
              <Radar :size="14" :stroke-width="1.7" />
              <b>配模型(强烈建议)</b>
              <button class="sense-more" title="打开完整的「寓言计划 · 感官 API」设置页" @click="goConfigEmbed">完整设置↗</button>
              <span v-if="overview?.hasEmbedProvider" class="sense-ok"><Check :size="12" :stroke-width="2.6" /> 嵌入已就绪</span>
              <span v-else class="sense-warn">不配只能按文件名粗分 · 语义聚类 / 检索效果差很多</span>
            </div>
            <!-- 嵌入 key(硅基 BGE-M3,免费):语义聚类 + 向量检索的关键 -->
            <div v-if="embedProv" class="sense-row">
              <span class="sr-name">
                <span class="sr-dot" :class="{ on: embedProv.key_ready }" />
                {{ embedProv.name }}<em v-if="embedProv.free">免费</em>
              </span>
              <template v-if="embedProv.key_ready">
                <span class="sr-ready"><Check :size="12" :stroke-width="2.6" /> 已配置</span>
              </template>
              <template v-else>
                <input v-model="embedKey" type="password" class="sr-key" placeholder="粘贴硅基流动 API Key(sk-…)" />
                <button class="mini" @click="openKeyUrl"><KeyRound :size="12" :stroke-width="1.8" /> 获取 key↗</button>
                <button class="mini key" :disabled="embedSaving || !embedKey.trim()" @click="saveEmbedKey">
                  {{ embedSaving ? "保存中" : "保存" }}
                </button>
              </template>
              <span v-if="embedMsg" class="sr-msg">{{ embedMsg }}</span>
            </div>
            <!-- 本地模型下载(转写音视频 → 内容可被聚类/检索)-->
            <div v-for="p in localPacks" :key="p.id" class="sense-row">
              <span class="sr-name">
                <span class="sr-dot" :class="{ on: p.installed }" />
                {{ p.name }}<em>本地</em>
              </span>
              <span v-if="p.installed" class="sr-ready"><Check :size="12" :stroke-width="2.6" /> 已下载</span>
              <template v-else-if="p.pack_id && packPct[p.pack_id] !== undefined">
                <span class="sr-prog"><span class="sr-fill" :style="{ width: packPct[p.pack_id] + '%' }" /></span>
                <span class="sr-msg">{{ packPct[p.pack_id] }}%</span>
              </template>
              <button v-else class="mini" @click="downloadPack(p)"><FileText :size="12" :stroke-width="1.8" /> 下载模型</button>
            </div>
          </div>
          <div class="wiz-foot end">
            <button v-if="!overview?.hasEmbedProvider" class="wiz-go ghost bg" @click="startOrganize">先跳过,直接{{ wiz.method === 'schema' ? '构建' : '归类' }}</button>
            <button class="wiz-go" @click="startOrganize">
              <component :is="wiz.method === 'schema' ? Network : Wand2" :size="15" :stroke-width="1.8" />
              {{ wiz.method === 'schema' ? '开始构建知识体系' : '开始归类' }}
            </button>
          </div>
        </div>

        <!-- 4 · 归类中 / 构建中 -->
        <div v-else-if="step === 'organize'" class="wiz-body center">
          <div class="scan-orb"><component :is="wiz.method === 'schema' ? Network : Wand2" :size="28" :stroke-width="1.3" /><div class="scan-ring" /></div>
          <div class="scan-lab big">{{ organizeMsg }}</div>
          <div class="scan-fine">{{ wiz.method === 'schema' ? '模型正在行业框内抽实体与关系,只抽资料里写明的、可溯源的,稍候…' : 'AI 正在读你的文件清单分主题,稍候…' }}</div>
          <button class="wiz-go ghost bg" @click="close"><Layers :size="14" :stroke-width="1.8" /> 转入后台 · 去逛别处</button>
        </div>

        <!-- 5 · 知识图谱 -->
        <div v-else-if="step === 'graph'" class="wiz-body">
          <h3 class="wiz-h"><Orbit :size="16" :stroke-width="1.7" /> 这是 AI 眼里的你</h3>
          <p class="wiz-tip">把你的文件归成了 <b>{{ topThemeCount }}</b> 个大主题。拖一拖、缩放看看,点「继续」我会后台建好检索索引,带你进对话。</p>
          <div class="graph-wrap">
            <KnowledgeGraph source="files" :embedded="true" />
          </div>
          <div class="wiz-foot end">
            <button class="wiz-go" @click="finishUp"><ChevronRight :size="15" :stroke-width="1.8" /> 继续:建索引 & 进对话</button>
          </div>
        </div>

        <!-- 6 · 收尾 -->
        <div v-else-if="step === 'finish'" class="wiz-body">
          <h3 class="wiz-h"><Sparkles :size="16" :stroke-width="1.7" /> 全部就绪 · AI 已经懂你了</h3>
          <p v-if="wiz.method === 'schema'" class="wiz-tip">
            已在「<b>{{ chosenSchema?.name ?? '行业' }}</b>」框内抽出 <b>{{ ontoKept }}</b> 条可溯源的实体关系,
            存进了知识体系(可在「核心层」查看)。向量索引正在<b>后台</b>建。挑一件我能立刻替你做的事:
          </p>
          <p v-else class="wiz-tip">
            向量 / 全文索引正在<b>后台</b>建(可以放着不管)。已在桌面生成你的「知识画像」。
            挑一个我能立刻替你做的事开始吧:
          </p>
          <div class="flow-cards">
            <button v-for="(f, i) in suggested" :key="i" class="flow-card" @click="useFlow(f)">
              <span class="fc-t">{{ f.title }}</span>
              <ChevronRight :size="15" :stroke-width="1.8" class="fc-arr" />
            </button>
          </div>
          <div v-if="suggesting" class="suggest-busy">
            <OrbitSpinner :size="12" /> AI 正在读你的资料、为你量身想建议…(先点上面任意一条也行)
          </div>
          <button v-else class="suggest-redo" @click="loadSuggestions">
            <Sparkles :size="12" :stroke-width="1.8" /> 让 AI 再想几条
          </button>
          <div class="finish-row">
            <button class="mini" :disabled="!profilePath" @click="openProfile"><FileText :size="12" :stroke-width="1.8" /> 打开桌面画像</button>
            <span v-if="finishing" class="fine-busy"><OrbitSpinner :size="12" /> 生成画像中…</span>
            <span v-else-if="profilePath" class="fine-ok"><Check :size="12" :stroke-width="2.4" /> 画像已存桌面</span>
          </div>
          <div class="wiz-foot end">
            <button class="wiz-go ghost" @click="finishDone">完成</button>
          </div>
        </div>
      </div>
    </div>
  </transition>
</template>

<style scoped>
.wiz-scrim {
  position: fixed;
  inset: 0;
  z-index: 80;
  display: flex;
  align-items: center;
  justify-content: center;
  background: color-mix(in srgb, var(--bg) 62%, transparent);
  -webkit-backdrop-filter: blur(6px);
  backdrop-filter: blur(6px);
  padding: 24px;
}
.glass {
  background: color-mix(in srgb, var(--panel) 78%, transparent);
  -webkit-backdrop-filter: blur(26px) saturate(1.5);
  backdrop-filter: blur(26px) saturate(1.5);
  border: 1px solid var(--border-soft);
  border-radius: 18px;
}
.wiz {
  position: relative;
  width: min(1040px, 94vw);
  max-height: 92vh;
  overflow: hidden;
  display: flex;
  flex-direction: column;
  box-shadow: var(--shadow-lg, 0 24px 80px rgba(0, 0, 0, 0.4));
  transition: width 0.2s ease;
}
/* 知识图谱步:整块放大成「大屏」,星图铺满更震撼 */
.wiz.big { width: min(1280px, 97vw); }
.wiz-x {
  position: absolute;
  top: 14px;
  right: 14px;
  z-index: 3;
  display: inline-flex;
  border: none;
  background: transparent;
  color: var(--dim);
  cursor: pointer;
  padding: 4px;
  border-radius: 8px;
}
.wiz-x:hover { color: var(--text); background: var(--selection-bg); }

/* 步骤条 */
.wiz-steps {
  display: flex;
  gap: 4px;
  padding: 16px 22px 12px;
  border-bottom: 1px solid var(--border-soft);
  overflow-x: auto;
}
.ws { display: flex; align-items: center; gap: 7px; padding: 0 10px 0 0; opacity: 0.5; flex: none; }
.ws.on, .ws.done { opacity: 1; }
.ws-dot {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 22px;
  height: 22px;
  border-radius: 50%;
  border: 1px solid var(--border-strong);
  font-size: 11px;
  color: var(--muted);
  flex: none;
}
.ws.on .ws-dot { background: var(--primary); border-color: var(--primary); color: #fff; }
.ws.done .ws-dot { background: color-mix(in srgb, var(--primary) 22%, transparent); border-color: var(--primary); color: var(--primary); }
.ws-lab { font-size: 12px; color: var(--text-2); white-space: nowrap; }
.ws.on .ws-lab { color: var(--text); font-weight: 600; }

/* 主体 */
.wiz-body { padding: 22px 26px 24px; overflow-y: auto; }
.wiz-body.center { display: flex; flex-direction: column; align-items: center; justify-content: center; text-align: center; min-height: 320px; gap: 14px; }
.wiz-h { display: flex; align-items: center; gap: 8px; font-size: 16px; margin: 0 0 8px; color: var(--ink); }
.wiz-tip { font-size: 13px; color: var(--muted); line-height: 1.6; margin: 0 0 16px; }
.wiz-tip b, .wiz-fine b { color: var(--text); }
.wiz-fine { font-size: 12px; color: var(--dim); margin: 10px 0 0; }
.wiz-loading, .wiz-err { display: flex; align-items: center; gap: 8px; font-size: 13px; color: var(--muted); padding: 20px 0; }
.wiz-err { color: var(--danger, #e0736b); }

/* 欢迎 */
.intro { text-align: center; }
.intro-orb {
  width: 76px; height: 76px; margin: 6px auto 14px;
  display: flex; align-items: center; justify-content: center;
  border-radius: 50%; color: var(--gold);
  background: radial-gradient(circle, color-mix(in srgb, var(--gold) 22%, transparent), transparent 70%);
}
.intro h2 { font-family: var(--serif); font-size: 24px; margin: 0 0 10px; letter-spacing: 1px; color: var(--ink); }
.intro-sub { font-size: 13.5px; color: var(--muted); line-height: 1.7; max-width: 520px; margin: 0 auto 20px; }
.intro-flow { list-style: none; margin: 0 auto 24px; padding: 0; max-width: 380px; text-align: left; display: flex; flex-direction: column; gap: 11px; }
.intro-flow li { display: flex; align-items: center; gap: 10px; font-size: 13px; color: var(--text-2); }
.intro-flow li :deep(svg) { color: var(--primary); flex: none; }

/* 按钮 */
.wiz-go {
  display: inline-flex; align-items: center; gap: 7px;
  height: 38px; padding: 0 20px;
  border: 1px solid var(--primary);
  background: var(--primary); color: #fff;
  border-radius: 10px; font-size: 13.5px; cursor: pointer;
  transition: filter 0.16s, opacity 0.16s;
}
.wiz-go:hover:not(:disabled) { filter: brightness(1.08); }
.wiz-go:disabled { opacity: 0.5; cursor: default; }
.wiz-go.ghost { background: transparent; color: var(--text); border-color: var(--border-strong); }
.wiz-go.bg { height: 32px; font-size: 12.5px; margin-top: 6px; color: var(--text-2); }
.wiz-go.bg:hover { color: var(--text); }
/* intro「跳过」:克制的文字按钮,不与主 CTA 抢视觉 */
.wiz-skip {
  display: block; margin: 12px auto 0;
  border: none; background: transparent;
  color: var(--muted); font-size: 12.5px; cursor: pointer;
  transition: color 0.16s;
}
.wiz-skip:hover { color: var(--text-2); text-decoration: underline; }
.mini {
  display: inline-flex; align-items: center; gap: 5px;
  height: 28px; padding: 0 11px;
  border: 1px solid var(--border-soft);
  background: color-mix(in srgb, var(--panel) 60%, transparent);
  color: var(--text-2); border-radius: 8px; font-size: 12px; cursor: pointer;
}
.mini:hover:not(:disabled) { color: var(--text); border-color: var(--border-strong); }
.mini:disabled { opacity: 0.5; cursor: default; }
.mini.key { color: var(--primary); border-color: color-mix(in srgb, var(--primary) 40%, transparent); }

/* 盘点范围 */
.root-tools { display: flex; align-items: center; gap: 8px; margin-bottom: 10px; flex-wrap: wrap; }
.kw {
  flex: 1; min-width: 200px; height: 30px; padding: 0 11px;
  border: 1px solid var(--border-soft); border-radius: 8px;
  background: color-mix(in srgb, var(--panel) 55%, transparent);
  color: var(--text); font-size: 12.5px;
}
.kw:focus { outline: none; border-color: var(--primary); }
.root-list { display: flex; flex-direction: column; gap: 4px; max-height: 300px; overflow-y: auto; }
.root-row {
  display: flex; align-items: center; gap: 9px;
  padding: 8px 11px; border-radius: 9px;
  border: 1px solid var(--border-soft);
  background: color-mix(in srgb, var(--panel) 45%, transparent);
  cursor: pointer; text-align: left; width: 100%;
  transition: border-color 0.14s, opacity 0.14s;
}
.root-row:hover { border-color: var(--border-strong); }
.root-row.off { opacity: 0.42; }
.root-check {
  display: inline-flex; align-items: center; justify-content: center;
  width: 18px; height: 18px; border-radius: 5px; flex: none;
  border: 1.5px solid var(--border-strong); color: #fff;
}
.root-check.on { background: var(--primary); border-color: var(--primary); }
.root-ic { color: var(--muted); flex: none; }
.root-name { font-size: 13px; color: var(--text); flex: none; }
.root-path { flex: 1; min-width: 0; font-size: 11px; color: var(--dim); font-family: ui-monospace, Consolas, monospace; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
/* 仿 WizTree:占总量条 + 体积 */
.root-bar { flex: none; width: 64px; height: 6px; border-radius: 99px; overflow: hidden; background: color-mix(in srgb, var(--ink) 10%, transparent); }
.root-bar-fill { display: block; height: 100%; border-radius: 99px; background: color-mix(in srgb, var(--primary) 70%, var(--ink) 8%); }
.root-size { flex: none; min-width: 56px; text-align: right; font-size: 11.5px; font-variant-numeric: tabular-nums; color: var(--text-2); }
.root-calc { flex: none; display: inline-flex; align-items: center; gap: 4px; font-size: 11px; color: var(--dim); }

.wiz-foot { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-top: 20px; }
.wiz-foot.end { justify-content: flex-end; }
.foot-count { font-size: 12.5px; color: var(--muted); }
.foot-count b { color: var(--text); }

/* 扫描 / 归类动画 */
.scan-orb { position: relative; width: 84px; height: 84px; display: flex; align-items: center; justify-content: center; color: var(--primary); }
/* 失败 / 取消:静止的灰圈,不再转动,一眼能看出「停了」而非「卡了」 */
.scan-orb.stopped { color: var(--muted); border: 2px solid var(--border-strong); border-radius: 50%; }
.scan-ring { position: absolute; inset: 0; border: 2px solid color-mix(in srgb, var(--primary) 40%, transparent); border-top-color: transparent; border-radius: 50%; animation: spin 1s linear infinite; }
.scan-actions { display: flex; align-items: center; gap: 10px; margin-top: 6px; }
.scan-actions .flip { transform: rotate(180deg); }
.scan-num { font-size: 34px; font-weight: 680; font-variant-numeric: tabular-nums; color: var(--ink); }
.scan-lab { font-size: 13px; color: var(--muted); }
.scan-lab.big { font-size: 14px; color: var(--text); }
.scan-fine { font-size: 12px; color: var(--dim); }

/* 模型卡 */
.mode-cards { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
.mode-card {
  display: flex; flex-direction: column; gap: 6px; text-align: left;
  padding: 16px; border-radius: 13px; cursor: pointer;
  border: 1px solid var(--border-soft); background: color-mix(in srgb, var(--panel) 45%, transparent);
  color: var(--text-2); transition: border-color 0.14s, background 0.14s;
}
.mode-card:hover { border-color: var(--border-strong); }
.mode-card.on { border-color: var(--primary); background: color-mix(in srgb, var(--primary) 8%, transparent); }
.mode-card :deep(svg) { color: var(--primary); }
.mc-t { font-size: 14px; font-weight: 620; color: var(--ink); display: flex; align-items: center; gap: 7px; }
.mc-t em { font-style: normal; font-size: 10.5px; color: #fff; background: var(--primary); padding: 1px 7px; border-radius: 99px; }
.mc-d { font-size: 12px; color: var(--muted); line-height: 1.55; }
.embed-note {
  display: flex; align-items: center; gap: 9px; margin-top: 16px;
  padding: 11px 14px; border-radius: 11px; font-size: 12.5px; color: var(--text-2);
  background: color-mix(in srgb, var(--primary) 6%, var(--panel));
  border: 1px solid color-mix(in srgb, var(--primary) 22%, transparent);
}
.embed-note :deep(svg) { color: var(--primary); flex: none; }
.embed-note span { flex: 1; line-height: 1.5; }
.embed-note b { color: var(--text); }

/* 前置感官模型配置 */
.sense-box {
  margin-top: 16px;
  padding: 12px 14px;
  border-radius: 12px;
  background: color-mix(in srgb, var(--panel) 50%, transparent);
  border: 1px solid var(--border-soft);
}
.sense-head {
  display: flex; align-items: center; gap: 8px;
  font-size: 13px; color: var(--text); margin-bottom: 10px;
}
.sense-head :deep(svg) { color: var(--primary); flex: none; }
.sense-more { border: none; background: transparent; color: var(--primary); font-size: 11.5px; cursor: pointer; padding: 0; }
.sense-more:hover { text-decoration: underline; }
.sense-warn {
  margin-left: auto; font-size: 11.5px; color: #e0736b;
  display: inline-flex; align-items: center; gap: 4px;
}
.sense-ok {
  margin-left: auto; font-size: 11.5px; color: #6fcf97;
  display: inline-flex; align-items: center; gap: 4px;
}
.sense-row {
  display: flex; align-items: center; gap: 8px; flex-wrap: wrap;
  padding: 7px 0; border-top: 1px solid var(--border-soft);
}
.sr-name {
  display: inline-flex; align-items: center; gap: 7px;
  font-size: 12.5px; color: var(--text); min-width: 150px;
}
.sr-name em { font-style: normal; font-size: 10px; color: var(--primary); border: 1px solid color-mix(in srgb, var(--primary) 40%, transparent); border-radius: 99px; padding: 0 6px; margin-left: 4px; }
.sr-dot { width: 8px; height: 8px; border-radius: 50%; background: var(--border-strong); flex: none; }
.sr-dot.on { background: #6fcf97; box-shadow: 0 0 6px #6fcf97; }
.sr-key {
  flex: 1; min-width: 200px; height: 30px; padding: 0 11px;
  border: 1px solid var(--border-soft); border-radius: 8px;
  background: color-mix(in srgb, var(--panel) 60%, transparent); color: var(--text); font-size: 12.5px;
}
.sr-key:focus { outline: none; border-color: var(--primary); }
.sr-ready { display: inline-flex; align-items: center; gap: 4px; font-size: 12px; color: #6fcf97; }
.sr-msg { font-size: 11.5px; color: var(--muted); }
.sr-prog { flex: 1; min-width: 120px; height: 6px; background: color-mix(in srgb, var(--panel) 80%, transparent); border-radius: 99px; overflow: hidden; }
.sr-fill { display: block; height: 100%; background: var(--primary); border-radius: 99px; transition: width 0.3s; }

/* 图谱:嵌入 KnowledgeGraph 星河,给定高度即可铺满。graph 步整块放大成「大屏」。 */
.graph-wrap { position: relative; height: min(66vh, 640px); border-radius: 14px; overflow: hidden; border: 1px solid var(--border-soft); }
.graph-wrap :deep(.graph) { height: 100%; }

/* 收尾 */
.flow-cards { display: flex; flex-direction: column; gap: 9px; }
.flow-card {
  display: flex; align-items: center; justify-content: space-between; gap: 10px;
  padding: 14px 16px; border-radius: 12px; cursor: pointer; width: 100%;
  border: 1px solid var(--border-soft); background: color-mix(in srgb, var(--panel) 50%, transparent);
  transition: border-color 0.14s, background 0.14s;
}
.flow-card:hover { border-color: var(--primary); background: color-mix(in srgb, var(--primary) 7%, transparent); }
.fc-t { font-size: 13.5px; color: var(--ink); font-weight: 560; }
.fc-arr { color: var(--primary); flex: none; }
.suggest-busy { display: inline-flex; align-items: center; gap: 6px; margin-top: 10px; font-size: 12px; color: var(--muted); }
.suggest-redo {
  display: inline-flex; align-items: center; gap: 5px; margin-top: 10px;
  border: none; background: transparent; color: var(--primary); font-size: 12px; cursor: pointer; padding: 2px 0;
}
.suggest-redo:hover { text-decoration: underline; }
.finish-row { display: flex; align-items: center; gap: 12px; margin-top: 16px; }
.fine-busy, .fine-ok { display: inline-flex; align-items: center; gap: 5px; font-size: 12px; color: var(--muted); }
.fine-ok { color: var(--primary); }

/* 画像选择(个人 / 企业) */
.prof-cards { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; margin-bottom: 16px; }
.prof-card {
  display: flex; flex-direction: column; gap: 7px; text-align: left;
  padding: 20px 18px; border-radius: 15px; cursor: pointer;
  border: 1px solid var(--border-soft); background: color-mix(in srgb, var(--panel) 45%, transparent);
  color: var(--text-2); transition: border-color 0.14s, background 0.14s, transform 0.14s;
}
.prof-card:hover { border-color: var(--border-strong); transform: translateY(-2px); }
.prof-card.on { border-color: var(--primary); background: color-mix(in srgb, var(--primary) 9%, transparent); }
.pc-ic {
  width: 44px; height: 44px; border-radius: 12px; display: flex; align-items: center; justify-content: center;
  color: var(--primary); background: color-mix(in srgb, var(--primary) 13%, transparent); margin-bottom: 4px;
}
.pc-t { font-size: 16px; font-weight: 650; color: var(--ink); }
.pc-d { font-size: 12.5px; color: var(--muted); line-height: 1.6; }
.pc-tag { font-size: 11px; color: var(--primary); margin-top: 2px; }

/* 企业:行业框选择 */
.schema-box {
  margin-top: 4px; padding: 14px 16px; border-radius: 13px;
  background: color-mix(in srgb, var(--panel) 50%, transparent); border: 1px solid var(--border-soft);
}
.schema-head { display: flex; align-items: center; gap: 8px; font-size: 13px; color: var(--text); margin-bottom: 12px; }
.schema-head :deep(svg) { color: var(--primary); flex: none; }
.schema-fine { margin-left: auto; font-size: 11px; color: var(--dim); }
.schema-grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: 10px; }
.schema-card {
  display: flex; flex-direction: column; gap: 4px; text-align: left;
  padding: 13px 14px; border-radius: 12px; cursor: pointer;
  border: 1px solid var(--border-soft); background: color-mix(in srgb, var(--panel) 55%, transparent);
  color: var(--text-2); transition: border-color 0.14s, background 0.14s;
}
.schema-card:hover { border-color: var(--border-strong); }
.schema-card.on { border-color: var(--primary); background: color-mix(in srgb, var(--primary) 9%, transparent); }
.sc-name { font-size: 14px; font-weight: 640; color: var(--ink); }
.sc-ind { font-size: 11px; color: var(--primary); }
.sc-types { font-size: 11.5px; color: var(--muted); line-height: 1.5; }
.sc-src { font-size: 10.5px; color: var(--dim); font-family: ui-monospace, Consolas, monospace; }
.schema-preview {
  display: flex; align-items: center; gap: 8px; margin-top: 12px; padding: 10px 12px;
  border-radius: 10px; font-size: 12px; color: var(--text-2);
  background: color-mix(in srgb, var(--primary) 6%, var(--panel)); border: 1px solid color-mix(in srgb, var(--primary) 20%, transparent);
}
.schema-preview :deep(svg) { color: var(--primary); flex: none; }
.schema-preview b { color: var(--text); }
@media (max-width: 760px) {
  .prof-cards { grid-template-columns: 1fr; }
  .schema-grid { grid-template-columns: 1fr 1fr; }
}

.spin { animation: spin 0.9s linear infinite; }
@keyframes spin { to { transform: rotate(360deg); } }
.wiz-fade-enter-active, .wiz-fade-leave-active { transition: opacity 0.2s; }
.wiz-fade-enter-from, .wiz-fade-leave-to { opacity: 0; }
</style>
