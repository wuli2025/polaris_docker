<script setup lang="ts">
import {
  computed,
  ref,
  watch,
  onMounted,
  onBeforeUnmount,
  defineAsyncComponent,
} from "vue";
// ── 常驻 / 首屏关键：静态导入（启动即需，进启动主包）──
import Sidebar from "./components/Sidebar.vue";
import ViewLoader from "./components/ViewLoader.vue";
import RightDrawer from "./components/RightDrawer.vue";
import ChatPanel from "./components/ChatPanel.vue";
import SplashScreen from "./components/SplashScreen.vue";
import Onboarding from "./components/Onboarding.vue";
import EnvDoctor from "./components/EnvDoctor.vue"; // 既是视图也是启动 env 网关，留静态
import UpdateBanner from "./components/UpdateBanner.vue";
import ToastHost from "./components/ToastHost.vue";
import VoiceOverlay from "./components/VoiceOverlay.vue";
import CommandPalette from "./components/CommandPalette.vue";
import TaskCenter from "./components/TaskCenter.vue";
import { useHotkeys } from "./composables/useHotkeys";
import { installMarkdownDelegation } from "./lib/markdown";
import { openUrl, onWsStatus, isTauri, files as fc } from "./tauri";
import { toast } from "./composables/useToast";
// ── 重 / 非首屏视图：懒加载，切到对应视图时才拉各自 chunk ──
// 把 cytoscape(图谱) + 4 套工坊 + 各面板/弹层(合计上万行)从启动主包挪走 → 开窗快、首屏不卡。
// KnowledgeGraph / SandboxStatus / 四工坊都有 defineOptions({name})，懒加载后 KeepAlive 仍按 name 缓存；
// 其首次挂载本就被 ViewLoader 加载条盖住，chunk 拉取(本地 ms 级)一并被遮。
const KnowledgeGraph = defineAsyncComponent(() => import("./components/KnowledgeGraph.vue"));
const SandboxStatus = defineAsyncComponent(() => import("./features/sandbox/components/SandboxStatus.vue"));
const WikiBrowse = defineAsyncComponent(() => import("./components/WikiBrowse.vue"));
const FileCenter = defineAsyncComponent(() => import("./components/FileCenter.vue"));
const Automation = defineAsyncComponent(() => import("./components/Automation.vue"));
const AutomationModal = defineAsyncComponent(() => import("./components/AutomationModal.vue"));
const ExpertCenter = defineAsyncComponent(() => import("./components/ExpertCenter.vue"));
const Settings = defineAsyncComponent(() => import("./components/Settings.vue"));
const SenseApi = defineAsyncComponent(() => import("./components/SenseApi.vue"));
const VoiceSettings = defineAsyncComponent(() => import("./components/VoiceSettings.vue"));
const SkillCenter = defineAsyncComponent(() => import("./components/SkillCenter.vue"));
const AddProviderModal = defineAsyncComponent(() => import("./components/AddProviderModal.vue"));
const McpConfigModal = defineAsyncComponent(() => import("./components/McpConfigModal.vue"));
const WorkflowPackModal = defineAsyncComponent(() => import("./components/WorkflowPackModal.vue"));
const UsageBoard = defineAsyncComponent(() => import("./components/UsageBoard.vue"));
const UpdatePanel = defineAsyncComponent(() => import("./components/UpdatePanel.vue"));
const FeishuSettings = defineAsyncComponent(() => import("./components/FeishuSettings.vue"));
const VideoCourseStudio = defineAsyncComponent(() => import("./components/VideoCourseStudio.vue"));
const MediaOps = defineAsyncComponent(() => import("./components/MediaOps.vue"));
const DeckStudio = defineAsyncComponent(() => import("./components/DeckStudio.vue"));
const WebStudio = defineAsyncComponent(() => import("./components/WebStudio.vue"));
// 「让 AI 更懂你」向导常驻 App 级:首次打开才拉 chunk,之后保持挂载 → 扫描/归类跑着时
// 用户可转后台、切视图、最小化窗口都不丢进度(组件不卸载,事件监听与状态都还在)。
const OnboardingWizard = defineAsyncComponent(() => import("./components/OnboardingWizard.vue"));
import { checkForUpdate } from "./composables/useUpdater";
import { useAppStore, type ViewKey } from "./stores/app";
import { useArtifactsStore } from "./stores/artifacts";
import { useProvidersStore } from "./stores/providers";
import { useChatStore } from "./stores/chat";
import { useWorkflowsStore } from "./stores/workflows";
import { useAutomationStore } from "./stores/automation";
import { useWizardStore } from "./stores/wizard";
import { useFileTasksStore } from "./stores/fileTasks";

const app = useAppStore();
const artifacts = useArtifactsStore();
const providers = useProvidersStore();
const chatStore = useChatStore();
const workflows = useWorkflowsStore();
const automation = useAutomationStore();
const wiz = useWizardStore();
const tasks = useFileTasksStore();
// 首次打开向导后就保持挂载(不再卸载),让后台扫描/归类跨视图、跨最小化持续推进。
const wizMounted = ref(false);
watch(
  () => wiz.open,
  (o) => {
    if (o) wizMounted.value = true;
  },
);

// ─────────── 重视图切换的"点击即缓冲"加载条 ───────────
// 点击图谱/沙箱(且首次=未被 KeepAlive 暖过)时：先立刻亮加载条(此刻重组件尚未挂载，
// 能马上画出来) → 等两帧画出后再挂载重组件(建图 / 9 数字人挂载的卡顿被盖在条下) →
// 组件 ready(图谱布局稳定 / 沙箱画好)后淡出。已暖的重视图直接秒切，不再亮条。
const HEAVY: ViewKey[] = ["graph", "sandbox"];
const warmed = ref<Set<ViewKey>>(new Set());
const mountedView = ref<ViewKey>(app.view); // 真正挂载的视图（重视图冷启时滞后两帧）
const switchLoader = ref<ViewKey | null>(null); // 当前加载条覆盖的重视图
let loaderSafety: number | undefined;

watch(
  () => app.view,
  (next) => {
    if (HEAVY.includes(next) && !warmed.value.has(next)) {
      switchLoader.value = next; // 点击瞬间亮条
      clearTimeout(loaderSafety);
      loaderSafety = window.setTimeout(() => {
        if (switchLoader.value === next) switchLoader.value = null;
      }, 4500); // 兜底：ready 没来也不卡住
      requestAnimationFrame(() =>
        requestAnimationFrame(() => {
          if (app.view !== next) return; // 这两帧里用户又切走了
          mountedView.value = next; // 现在才挂载重视图
          warmed.value.add(next);
        })
      );
    } else {
      mountedView.value = next;
      switchLoader.value = null;
    }
  }
);

function onViewReady(v: ViewKey) {
  if (switchLoader.value === v) switchLoader.value = null;
}

// 开机静默续建索引:盘点过、还有没嵌完的文本(embeddedFiles < textFiles)才接着建,
// 走全局 fileTasks store(任务中心可见、跨视图后台跑)。给一句温和 toast 提醒。延后几秒
// 起,别和启动的其它活儿抢资源;没待办或读不到总览就安静退出。
async function autoBuildIndexOnStartup() {
  try {
    const ov = await fc.overview(null);
    if (ov.indexing || ov.scanning) return; // 已经在跑,别重复
    // 没配嵌入服务商时向量索引无可续建(FTS 倒排首轮已一遍建完),不必每次开机空跑 + 误报。
    if (!ov.hasEmbedProvider) return;
    const pending = (ov.textFiles ?? 0) - (ov.embeddedFiles ?? 0);
    if ((ov.totalFiles ?? 0) === 0 || pending <= 0) return; // 没盘点过 / 全嵌完 → 不打扰
    window.setTimeout(() => {
      void tasks.startIndex();
      toast.info(`索引正在后台构建(约 ${pending.toLocaleString()} 个文件待处理)· 完成后 AI 检索更聪明,可放着不管`);
    }, 3000);
  } catch {
    /* 浏览器/降级模式或总览读取失败 → 安静跳过 */
  }
}

// 多开核心：app 级注册一次流式监听，任意对话的事件都按 conversationId 路由进各自缓冲，
// 这样切走/未挂载 ChatPanel 时后台任务仍持续流式推进、完成有提醒。
let unMdDelegate: (() => void) | null = null;
let unWsStatus: (() => void) | null = null;
onMounted(() => {
  chatStore.init();
  // 文件中心长任务(盘点/建索引/智能归类/AI 整理名称)的全局事件监听:App 级注册一次,
  // 脱离任何视图生命周期 → 在文件中心点了任务后切走/关掉该视图,进度照常推进、回来即见,
  // 全局任务中心浮层也据此随处显示「还在跑」。
  tasks.ensureListeners();
  // 开机自动续建检索索引:只要盘点过、还有没嵌完的文本,就在后台静默接着建(幂等续跑,
  // 不动用户原文件——只把内容切块嵌进本地 SQLite)。给用户一句温和提醒「索引在后台构建」,
  // 让 AI 检索越用越聪明。没有待办(全嵌完/没盘点过)就什么都不做、不打扰。
  void autoBuildIndexOnStartup();
  // markdown 区域事件委托(代码复制/展开/外链系统浏览器打开),全 v-html 区域一次覆盖
  unMdDelegate = installMarkdownDelegation(document, (url) => {
    openUrl(url).catch(() => {});
  });
  // Docker/Web 模式:WS 断线 → 顶部细条提示(自动重连由 tauri.ts 负责)
  if (!isTauri) unWsStatus = onWsStatus((ok) => (wsDown.value = !ok));
});
onBeforeUnmount(() => {
  unMdDelegate?.();
  unWsStatus?.();
  clearTimeout(loaderSafety);
  window.removeEventListener("mousemove", onAuroraPointer);
  if (edgeRaf) cancelAnimationFrame(edgeRaf);
});

// ─────────── 极光主题：彩虹边框高光跟随鼠标方位游走 ───────────
// mousemove 用 rAF 合帧(一帧最多算一次)，把鼠标相对主面板中心的方位角写进
// CSS 变量 --edge-angle，style.css 里的 conic 亮带就锚在该方位 → 边框上那一段亮起。
// 仅极光两套主题挂监听；切走主题即摘掉，零常驻开销。
let mainEl: HTMLElement | null = null;
let edgeRaf = 0;
let edgePx = 0;
let edgePy = 0;
function flushEdge() {
  edgeRaf = 0;
  mainEl ||= document.querySelector(".main");
  if (!mainEl) return;
  const r = mainEl.getBoundingClientRect();
  const cx = r.left + r.width / 2;
  const cy = r.top + r.height / 2;
  // conic 0deg 指向正上、顺时针递增；atan2 0 指向右(屏幕 y 向下) → +90° 对齐
  const deg = (Math.atan2(edgePy - cy, edgePx - cx) * 180) / Math.PI + 90;
  mainEl.style.setProperty("--edge-angle", `${deg.toFixed(1)}deg`);
}
function onAuroraPointer(e: MouseEvent) {
  edgePx = e.clientX;
  edgePy = e.clientY;
  if (!edgeRaf) edgeRaf = requestAnimationFrame(flushEdge);
}
const isAurora = computed(
  () => app.theme === "aurora-light" || app.theme === "aurora-dark"
);
watch(
  isAurora,
  (on) => {
    if (on) {
      window.addEventListener("mousemove", onAuroraPointer, { passive: true });
    } else {
      window.removeEventListener("mousemove", onAuroraPointer);
      if (edgeRaf) {
        cancelAnimationFrame(edgeRaf);
        edgeRaf = 0;
      }
    }
  },
  { immediate: true }
);

// 全局快捷键:Ctrl+N 新对话 / Ctrl+K 命令面板 / Ctrl+B 收侧栏
useHotkeys();

const wsDown = ref(false);

// 启动流程：splash(每次) → onboarding(仅首次) → env(环境检测,健康则无感放行) → ready
const ONBOARDED_KEY = "polaris.onboarded.v1";
const phase = ref<"splash" | "onboarding" | "env" | "ready">("splash");

function onSplashDone() {
  const done = localStorage.getItem(ONBOARDED_KEY);
  phase.value = done ? "env" : "onboarding";
}
function onOnboardingDone() {
  phase.value = "env";
}
function onEnvDone() {
  phase.value = "ready";
  // splash → onboarding → env 全部完成后，再检查更新（避免弹窗被盖住）
  checkForUpdate();
}

// 预览成品文件时把右侧抽屉拓宽；展开模式更宽，让观看更好看
const drawerTrack = computed(() => {
  if (artifacts.current) {
    return artifacts.expanded ? "min(1040px, 72vw)" : "clamp(400px, 36vw, 560px)";
  }
  return `${app.drawerWidth}px`;
});

const layoutCols = computed(
  () => `${app.sidebarWidth}px 1fr ${drawerTrack.value}`
);

// ── 侧栏宽度拖拽(分隔条) ──
const sbDragging = ref(false);
function startSbDrag(e: MouseEvent) {
  if (app.sidebarCollapsed) return;
  sbDragging.value = true;
  const startX = e.clientX;
  const startW = app.sidebarWidth;
  // 用 rAF 合帧：mousemove 可能一帧来好几个,只在每帧画前应用最后一次宽度,
  // 把 grid 重排+侧栏 backdrop-filter 重算压到每帧一次；拖拽中不落盘(persist=false)。
  let pending = startW;
  let rafId = 0;
  const flush = () => {
    rafId = 0;
    app.setSidebarWidth(pending, false);
  };
  const move = (ev: MouseEvent) => {
    pending = startW + ev.clientX - startX;
    if (!rafId) rafId = requestAnimationFrame(flush);
  };
  const up = () => {
    sbDragging.value = false;
    if (rafId) cancelAnimationFrame(rafId);
    app.setSidebarWidth(pending, true); // 松手落一次盘
    window.removeEventListener("mousemove", move);
    window.removeEventListener("mouseup", up);
  };
  window.addEventListener("mousemove", move);
  window.addEventListener("mouseup", up);
}
</script>

<template>
  <div class="shell" :class="{ 'sb-drag': sbDragging }" :style="{ gridTemplateColumns: layoutCols }">
    <!-- 极光琉璃画框主题：虚幻极光 + 颗粒背景层（fixed，居于全部内容之下，
         内容面板不透明遮住中央 → 极光只在画框带透出；浅/深两版共用） -->
    <template v-if="app.theme === 'aurora-light' || app.theme === 'aurora-dark'">
      <div class="aurora" aria-hidden="true">
        <span class="a1"></span><span class="a2"></span><span class="a3"></span><span class="a4"></span><span class="a5"></span>
      </div>
      <div class="grain" aria-hidden="true"></div>
    </template>
    <Sidebar />
    <main class="main">
      <!-- 侧栏宽度拖拽分隔条 -->
      <div
        v-if="!app.sidebarCollapsed"
        class="sb-resizer"
        title="拖拽调节侧栏宽度"
        @mousedown.prevent="startSbDrag"
      ></div>
      <!-- 重视图(图谱/沙箱)用 KeepAlive 缓存：第一次进算一次，之后切走再回来瞬开，
           且离开时其动画/自转随 DOM 脱离自动暂停，不在后台空耗。其余视图照常按需挂载。
           四个工坊也缓存：生成/修改是多轮流程(phase/convId/产物预览都在组件态里)，
           切去对话看进度再切回来必须还能「继续修改」，销毁重建=流程报废。
           mountedView 让重视图冷启时滞后两帧挂载，先把加载条画出来再扛卡顿。 -->
      <KeepAlive :include="['KnowledgeGraph', 'SandboxStatus', 'DeckStudio', 'WebStudio', 'MediaOps', 'VideoCourseStudio']">
        <ChatPanel v-if="mountedView === 'chat'" />
        <WikiBrowse v-else-if="mountedView === 'wiki'" />
        <FileCenter v-else-if="mountedView === 'file_center'" />
        <Automation v-else-if="mountedView === 'automation'" />
        <KnowledgeGraph
          v-else-if="mountedView === 'graph'"
          @ready="onViewReady('graph')"
        />
        <SandboxStatus
          v-else-if="mountedView === 'sandbox'"
          @ready="onViewReady('sandbox')"
        />
        <ExpertCenter v-else-if="mountedView === 'claude_md'" />
        <SkillCenter v-else-if="mountedView === 'skill_center'" />
        <EnvDoctor v-else-if="mountedView === 'env_doctor'" />
        <UpdatePanel v-else-if="mountedView === 'update'" />
        <FeishuSettings v-else-if="mountedView === 'feishu'" />
        <McpConfigModal v-else-if="mountedView === 'mcp'" inline @close="app.setView('chat')" />
        <Settings v-else-if="mountedView === 'settings'" />
        <SenseApi v-else-if="mountedView === 'sense_api'" />
        <VoiceSettings v-else-if="mountedView === 'voice_input'" />
        <VideoCourseStudio v-else-if="mountedView === 'video_course'" />
        <MediaOps v-else-if="mountedView === 'media_ops'" />
        <DeckStudio v-else-if="mountedView === 'deck'" />
        <WebStudio v-else-if="mountedView === 'web_studio'" />
      </KeepAlive>

      <!-- 点击重视图即刻浮现的快速加载条（盖住挂载/建图卡顿） -->
      <Transition name="vl">
        <ViewLoader
          v-if="switchLoader"
          :dark="switchLoader === 'graph'"
          :label="switchLoader === 'graph' ? '星河生成中' : '沙箱加载中'"
        />
      </Transition>
    </main>
    <RightDrawer />

    <!-- 自动更新提示条（发现新版本时浮出） -->
    <UpdateBanner />

    <!-- 全局 toast(统一通知出口) + Ctrl+K 命令面板 -->
    <ToastHost />
    <VoiceOverlay />
    <CommandPalette />

    <!-- 全局任务中心:盘点/建索引/智能归类等后台任务,无论切到哪个视图都常驻可见、可点回去 -->
    <TaskCenter />

    <!-- Docker/Web 模式断线提示条 -->
    <div v-if="wsDown" class="ws-down">连接已断开,正在自动重连…</div>

    <!-- 「让 AI 更懂你」引导向导(常驻;首次打开后保持挂载,转后台/切视图不丢进度) -->
    <OnboardingWizard v-if="wizMounted" />

    <AddProviderModal v-if="providers.showAddModal" />
    <WorkflowPackModal v-if="workflows.editorOpen" />
    <AutomationModal v-if="automation.editorOpen" />
    <UsageBoard v-if="providers.showUsageBoard" />

    <!-- MCP 配置对话框（触发器已移到 Sidebar 导航栏下方） -->
    <McpConfigModal v-if="app.showMcpModal" @close="app.showMcpModal = false" />

    <!-- 启动流程覆盖层：splash → onboarding -->
    <Transition name="splash-fade">
      <SplashScreen v-if="phase === 'splash'" @done="onSplashDone" />
    </Transition>
    <Transition name="onboard-fade">
      <Onboarding v-if="phase === 'onboarding'" @done="onOnboardingDone" />
    </Transition>
    <Transition name="onboard-fade">
      <EnvDoctor v-if="phase === 'env'" gate @done="onEnvDone" />
    </Transition>
  </div>
</template>

<style scoped>
/* 连屏一体化边框（仿 Codex）：整个 shell 是一块连续的框面（侧栏+顶部+四周同色），
   主区/右抽屉是嵌在框里的圆角面板 —— 上边框与左边框在面板左上圆角处汇合 */
.shell {
  height: 100vh;
  display: grid;
  background: var(--bg-side);
  /* 外圈四角倒圆：消除左上角（内容与标题栏交汇处）的正方形棱角，整窗只剩圆角 */
  border-radius: 12px;
  overflow: hidden;
  transition: grid-template-columns 180ms ease;
}
/* 拖宽时关过渡,否则跟手延迟 */
.shell.sb-drag {
  transition: none;
  user-select: none;
  cursor: col-resize;
}
.sb-resizer {
  position: absolute;
  left: -3px;
  top: 0;
  bottom: 0;
  width: 6px;
  z-index: 30;
  cursor: col-resize;
}
.sb-resizer:hover {
  background: var(--primary-soft);
}
/* 黑夜模式（仿 Codex）：中性石墨框面，无辉光 */
html[data-theme="dark"] .shell {
  background: var(--bg-side);
}
.main {
  position: relative;
  overflow: hidden;
  background: var(--bg-chat);
  display: flex;
  flex-direction: column;
  /* 圆润嵌入面板：四周均匀留出框面缝隙（含左侧，避免与侧栏齐平相撞出菱角接缝），
     面板呈干净的悬浮圆角卡，与染色标题栏组成连屏框面 */
  margin: 8px;
  border: 1px solid var(--hairline);
  border-radius: 12px;
  box-shadow: var(--shadow);
}
.placeholder {
  flex: 1;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--muted);
  font-family: var(--serif);
  font-size: 14px;
  letter-spacing: 2px;
}
/* Docker/Web 模式 WS 断线提示条 */
.ws-down {
  position: fixed;
  top: 0;
  left: 50%;
  transform: translateX(-50%);
  z-index: 9998;
  padding: 4px 16px;
  border-radius: 0 0 9px 9px;
  background: var(--vermilion);
  color: #fff;
  font-size: 12px;
  letter-spacing: 0.5px;
  box-shadow: var(--shadow-lg);
}
</style>

<!-- 非 scoped：Transition 类名需作用在子组件根元素上 -->
<style>
.splash-fade-leave-active {
  transition: opacity 0.8s ease;
}
.splash-fade-leave-to {
  opacity: 0;
}
.onboard-fade-enter-active {
  transition: opacity 0.4s ease;
}
.onboard-fade-leave-active {
  transition: opacity 0.45s ease;
}
.onboard-fade-enter-from,
.onboard-fade-leave-to {
  opacity: 0;
}
</style>
