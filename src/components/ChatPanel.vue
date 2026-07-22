<script setup lang="ts">
// ChatPanel 主文件:只保留「消息区」(回合构建/渲染/滚动/历史折叠)与跨块共享的粘合。
// 其余大块已拆到 src/components/chat/ 子组件(逻辑原样搬移):
//   ChatTopBar   —— 顶栏(标题/重命名/更多菜单/复制反馈/抽屉开关)
//   ChatHero     —— 空对话页(彩蛋/问候/工作流推荐)
//   ChatComposer —— 输入区(输入卡片/工具条各弹层/授权栏/语音/附件/发送)
//   BriefingCenter(懒加载,挂在 Composer 授权栏)—— 今日建议胶囊 + 大弹窗
// 共享类型与纯函数在 chat/shared.ts。
import { ref, computed, onMounted, onBeforeUnmount, nextTick, watch } from "vue";
import ExpertTeamStudio from "./ExpertTeamStudio.vue";
import { ChevronDown, Paperclip } from "@lucide/vue";
import ChatTopBar from "./chat/ChatTopBar.vue";
import ChatHero from "./chat/ChatHero.vue";
import ChatComposer from "./chat/ChatComposer.vue";
import TurnItem from "./chat/TurnItem.vue";
import { renderMd, buildTurnsSlice, type Turn } from "./chat/shared";
import { mdVersion } from "../lib/markdown";
import type { AgentMode } from "./chat/shared";
import { expert, type ExpertAgentStatus, type SuggestedFlow } from "../tauri";
import { useAppStore } from "../stores/app";
import { useArtifactsStore } from "../stores/artifacts";
import { useChatStore, type Bubble } from "../stores/chat";
import { useFileDrop } from "../composables/useFileDrop";
import { isLowSpec } from "../composables/useLowSpec";

const app = useAppStore();
const artifactsStore = useArtifactsStore();
const chatStore = useChatStore();

// 子组件句柄:顶栏(复制反馈)与输入区(填入输入框/附件/发送选项)
const topBarRef = ref<InstanceType<typeof ChatTopBar> | null>(null);
const composerRef = ref<InstanceType<typeof ChatComposer> | null>(null);

/** 点击成品文件 chip → 展开右侧抽屉并预览；应用文件夹 → 直接在文件管理器打开 */
function openArtifact(path: string) {
  if (path.endsWith("/")) {
    artifactsStore.openFolder(path);
    return;
  }
  app.drawerCollapsed = false;
  artifactsStore.open(path);
}

/** 「打开文件夹」入口 → 直接在系统文件管理器里打开本轮产物所在目录(不进右抽屉预览)。 */
function openFolderCard(path: string) {
  artifactsStore.openFolder(path);
}

// 豆包化:对话里模型一落出演示 spec(polaris.slides.json),右抽屉立刻自动打开成播放器
// —— 配合抽屉的宽容解析轮询,页面逐页点亮,用户不必等生成结束、也不必自己去点产物 chip。
// 只在「抽屉没在看别的东西」时抢焦点;同一个路径一个会话只自动开一次。
// 流式(artifact 事件)这条链路已由事件源头 stores/chat.ts 命令式处理(作者注释也说
// 源头这条更可靠,且带「陈旧让位」判断)——此处不再重复。但组件 watch 原先还兜着
// 「历史恢复」场景:loadHistory 整体替换气泡数组时,历史里的 spec 也会触发自动打开。
// 为保住这个覆盖面又不付每帧代价,把 watch 源从「flatMap 全气泡 + 正则全扫」(流式
// 每 40ms 一帧都要 O(全对话) 重算)改成气泡**数组引用**这个轻签名:流式只在既有数组
// 上 push/就地改(引用不变,watch 不触发,由 store 源头链路负责);只有切对话 /
// loadHistory 重载才换新数组(引用变,才做一次全扫)。
const autoOpenedSpecs = new Set<string>();
watch(
  () => chatStore.bubblesFor(app.currentConvId),
  (list) => {
    const specs = (list ?? [])
      .flatMap((b: any) => b.artifacts ?? [])
      .filter((p: string) => /polaris\.slides\.json$/i.test(p));
    for (const p of specs) {
      if (autoOpenedSpecs.has(p)) continue;
      autoOpenedSpecs.add(p);
      const cur = artifactsStore.current?.path;
      if (!cur || cur === p) {
        app.drawerCollapsed = false;
        artifactsStore.open(p);
      }
    }
  }
);

// 豆包式「参考文件」已在 buildTurnsSlice 里一次算好挂在 turn.refs 上(见 chat/shared.ts),
// 点开预览走 openArtifact 同一条右侧抽屉链路;这里不再保留每帧内联重算的函数版。

// 产物文件夹入口的展开态(按回合 key, 默认折叠 —— Kimi 式默认只露主预览卡 + 文件夹一行摘要,
// 不再把一堆小文件铺满对话框; 只在会话内存活, 不持久化)
const filesExpanded = ref<Record<number, boolean>>({});
function toggleFiles(k: number) {
  filesExpanded.value = { ...filesExpanded.value, [k]: !filesExpanded.value[k] };
}

// 多开：当前对话的气泡 / 运行态来自 chat store（按对话 id 维护，切走不丢、后台续流）
const bubbles = computed(() => chatStore.bubblesFor(app.currentConvId));
const sending = computed(() => chatStore.isSending(app.currentConvId));

const scrollEl = ref<HTMLDivElement | null>(null);

// ── 已定稿回合缓存:流式 delta 帧(每 ~40ms)只有末回合在长,之前全量重建全部回合
// 纯属浪费(长对话时逐帧字符串拼接 + 对象分配)。切分点 = 最后一条 user 气泡(user 恒
// 开新回合,故其之前的气泡构成的回合已定稿)。前缀按「逐气泡引用 + 产物数」签名比对
// (O(n) 引用比较,远便宜于重建),命中则复用上次前缀回合;任何结构变化 —— 切换对话 /
// 重发 / 删除 / loadHistory 重载(整个数组换新对象)—— 签名必不匹配 → 整体重建,
// 保守失效,绝不渲染错乱。产物数纳入签名是防「artifact 事件就地 push 进前缀气泡」的
// 边角(本回合尚无 assistant 正文时后端产物会挂到上一回合的 assistant 气泡上)。
interface TurnsPrefixCache {
  sig: { b: Bubble; artLen: number }[];
  turns: Turn[];
  /** 预渲染 html 时的 mdVersion:异步高亮/公式完成后失效重渲,前缀 html 才不会停在未增强版 */
  mdVer: number;
}
let turnsPrefixCache: TurnsPrefixCache | null = null;
const renderTurns = computed<Turn[]>(() => {
  const list = bubbles.value;
  const mdVer = mdVersion.value; // 注册依赖:增强完成 → 前缀缓存失效 → html 重挂
  // 末回合起点 = 最后一条 user 气泡;没有 user 气泡则整段都算活跃回合
  let split = 0;
  for (let i = list.length - 1; i >= 0; i--) {
    if (list[i].role === "user") {
      split = i;
      break;
    }
  }
  let prefixTurns: Turn[];
  const c = turnsPrefixCache;
  let hit = c !== null && c.sig.length === split && c.mdVer === mdVer;
  if (hit && c) {
    for (let i = 0; i < split; i++) {
      const s = c.sig[i];
      // 引用比较即可:前缀气泡唯一的就地变更是 artifacts push(见上),用产物数兜住
      if (s.b !== list[i] || s.artLen !== (list[i].artifacts?.length ?? 0)) {
        hit = false;
        break;
      }
    }
  }
  if (hit && c) {
    prefixTurns = c.turns;
  } else {
    prefixTurns = buildTurnsSlice(list.slice(0, split), 0);
    // 定稿回合的正文 html 在此一次预渲染挂到 turn 上(renderMarkdown 有按原文的内部
    // 缓存,这里主要省掉的是每帧的 TL;DR 正则 + ANSI 清洗前处理与字符串拼接),模板
    // (TurnItem)直接 v-html turn.html;活跃末回合不挂,由 TurnItem 每帧现场渲染。
    for (const t of prefixTurns) {
      if (t.text) t.html = renderMd(t.text, true);
    }
    turnsPrefixCache = {
      sig: list
        .slice(0, split)
        .map((b) => ({ b, artLen: b.artifacts?.length ?? 0 })),
      turns: prefixTurns,
      mdVer,
    };
  }
  // 活跃末回合每帧重建(它在流式变化中);key 顺延保证与整段构建时完全一致
  const tailTurns = buildTurnsSlice(list.slice(split), prefixTurns.length);
  return prefixTurns.length ? prefixTurns.concat(tailTurns) : tailTurns;
});
function isPending(t: Turn): boolean {
  return sending.value && t === renderTurns.value[renderTurns.value.length - 1];
}

// ── 历史折叠:长对话只渲染最近 N 回合,顶部「加载更早」逐段放开 ──
// 低配机起步只渲染 15 回合(单回合含大量工具/产物时 DOM 也重),弱机滚动更顺;
// 「加载更早」仍按同一步长逐段放开,不影响回看完整历史。
const FOLD_STEP = isLowSpec ? 15 : 30;
const visibleLimit = ref(FOLD_STEP);
const hiddenCount = computed(() =>
  Math.max(0, renderTurns.value.length - visibleLimit.value)
);
const visibleTurns = computed(() =>
  hiddenCount.value > 0 ? renderTurns.value.slice(hiddenCount.value) : renderTurns.value
);
function showEarlier() {
  const el = scrollEl.value;
  const prevH = el?.scrollHeight ?? 0;
  const prevTop = el?.scrollTop ?? 0;
  visibleLimit.value += FOLD_STEP;
  // 维持视口锚定,别跳
  nextTick(() => {
    if (el) el.scrollTop = prevTop + (el.scrollHeight - prevH);
  });
}

// ── 工具 pill 展开详情 ──
const expandedTool = ref<string | null>(null);
function toggleTool(turnKey: number, idx: number) {
  const k = `${turnKey}:${idx}`;
  expandedTool.value = expandedTool.value === k ? null : k;
}

// ── 重新生成 / 编辑重发 ──
// 发送选项(权限/技能/知识库/智能体/工作模式/供应商)由输入区(Composer)统一给出,
// 与主发送共用同一份形状。
async function regenerate(t: Turn) {
  if (!t.user || sending.value) return;
  const convId = app.currentConvId;
  if (!convId) return;
  const text = t.user.text || "";
  const files = t.user.files;
  let prompt = text || "请查看我上传的附件。";
  if (files && files.length) {
    const lines = files.map((a) => `- ${a.path}`).join("\n");
    prompt += `\n\n---\n[附件]（用户拖拽上传，可用 Read 等工具读取）：\n${lines}`;
  }
  const opts = composerRef.value?.sendOptions(convId);
  if (!opts) return;
  await chatStore.send(convId, prompt, text || "（仅附件）", files, opts);
}
function editTurn(t: Turn) {
  if (!t.user?.text) return;
  composerRef.value?.setInput(t.user.text);
}

// 复制某一回合的回答正文（回答下方的「复制」按钮）——反馈提示借顶栏的复制小提示
async function copyTurn(t: Turn) {
  if (!t.text) return;
  try {
    await navigator.clipboard.writeText(t.text);
    topBarRef.value?.flash("已复制回答");
  } catch {
    topBarRef.value?.flash("复制失败");
  }
}

// 空白页工作流建议:点一条把整条工作流提示词填进输入框(可改可发)
function applyFlow(f: SuggestedFlow) {
  composerRef.value?.setInput(f.prompt);
}

// ─────────── 百人专家团模式(与输入区 v-model 共享,消息区据此显示工作台) ───────────
const agentMode = ref<AgentMode>("auto-match");

// ─────────── 专家团实时状态轮询 ──────────
// 自适应退避:状态有变化 → 回到 2s 快节奏(用户正盯着看进度);连续稳定 → 逐步拉长
// 到 15s(空转就别每 3s 打一次后端);窗口失焦 → 直接慢到上限(用户没在看)。
// 旧版固定 3s setInterval,活跃时对后端是恒定压力、且失焦仍照打。
const teamAgentsStatus = ref<ExpertAgentStatus[]>([]);
const AGENTS_POLL_MIN = 2000;
const AGENTS_POLL_MAX = 15000;
let agentsPollTimer: ReturnType<typeof setTimeout> | null = null;
let agentsPollDelay = AGENTS_POLL_MIN;
let agentsPolling = false;
let agentsSnapCache: string | null = null; // 上一轮状态快照,免去每轮双份 stringify

async function pollAgentsStatus() {
  const pid = app.currentProjectId;
  if (!pid) return;
  try {
    teamAgentsStatus.value = await expert.agentsStatus(pid);
  } catch {
    /* ignore */
  }
}

function scheduleAgentsPoll() {
  if (!agentsPolling) return;
  agentsPollTimer = setTimeout(async () => {
    if (!agentsPolling) return;
    const before = agentsSnapCache ?? JSON.stringify(teamAgentsStatus.value);
    await pollAgentsStatus();
    const after = JSON.stringify(teamAgentsStatus.value);
    agentsSnapCache = after;
    const hidden =
      typeof document !== "undefined" && document.visibilityState === "hidden";
    if (before !== after) {
      agentsPollDelay = AGENTS_POLL_MIN; // 有进展→回快节奏
    } else if (hidden) {
      agentsPollDelay = AGENTS_POLL_MAX; // 用户没在看→直接慢到底
    } else {
      agentsPollDelay = Math.min(Math.round(agentsPollDelay * 1.6), AGENTS_POLL_MAX);
    }
    scheduleAgentsPoll();
  }, agentsPollDelay);
}

function startAgentsPoll() {
  if (agentsPolling) return;
  agentsPolling = true;
  agentsPollDelay = AGENTS_POLL_MIN;
  void pollAgentsStatus(); // 立即拉一次,别等第一个间隔
  scheduleAgentsPoll();
}

function stopAgentsPoll() {
  agentsPolling = false;
  if (agentsPollTimer) {
    clearTimeout(agentsPollTimer);
    agentsPollTimer = null;
  }
}

// 当切换到专家团模式时启动轮询，切换走时停止
watch(agentMode, (m) => {
  if (m === "expert-team") {
    startAgentsPoll();
  } else {
    stopAgentsPoll();
    teamAgentsStatus.value = [];
  }
});

onBeforeUnmount(() => {
  stopAgentsPoll(); // 切走视图即停表,别把退避轮询定时器泄漏到卸载后
});

// ─────────── 拖拽上传附件到当前对话(覆盖层在本层,附件状态在输入区) ───────────
const { isOver: dropOver } = useFileDrop({
  active: () => app.view === "chat",
  onDrop: (paths: string[]) => {
    void composerRef.value?.attachPaths(paths);
  },
});

function scrollToBottom() {
  nextTick(() => {
    if (scrollEl.value) scrollEl.value.scrollTop = scrollEl.value.scrollHeight;
    atBottom.value = true;
  });
}

// ── 滚动跟随:只有用户本就在底部才跟;上翻后浮出「回到底部」钮,不再硬拽 ──
const atBottom = ref(true);
function onMessagesScroll() {
  const el = scrollEl.value;
  if (!el) return;
  atBottom.value = el.scrollHeight - el.scrollTop - el.clientHeight < 90;
}

// 历史加载中/失败状态(骨架屏 + 重试入口)
const historyLoading = ref(false);
const historyErr = computed(() => chatStore.historyError(app.currentConvId));
async function retryHistory() {
  historyLoading.value = true;
  try {
    await chatStore.loadHistory(app.currentConvId, true);
  } finally {
    historyLoading.value = false;
  }
  scrollToBottom();
}

// 切换对话：加载该对话历史（运行中的对话不会被历史覆盖），滚到底
// (草稿/输入历史召回的按对话隔离在输入区 ChatComposer 内各自 watch 处理)
watch(
  () => app.currentConvId,
  async (cid) => {
    visibleLimit.value = FOLD_STEP;
    expandedTool.value = null;
    historyLoading.value = true;
    try {
      await chatStore.loadHistory(cid);
    } finally {
      historyLoading.value = false;
    }
    scrollToBottom();
  }
);

// 当前对话气泡变化（含流式增量）时跟随滚动 —— 只看「条数 + 末条长度」这个轻签名,
// 替代昂贵的 deep watch;且仅当用户在底部时才跟。
const tailSig = computed(() => {
  const arr = bubbles.value;
  const last = arr[arr.length - 1];
  return (
    arr.length * 1e9 +
    (last ? last.text.length + (last.artifacts?.length ?? 0) * 7 : 0)
  );
});
watch(tailSig, () => {
  if (atBottom.value) scrollToBottom();
});

onMounted(async () => {
  await chatStore.init(); // app 级流式监听只注册一次，按 conversationId 路由
  await chatStore.loadHistory(app.currentConvId);
  scrollToBottom();
});
</script>

<template>
  <div class="chat" :class="{ 'drag-active': dropOver }">
    <!-- 拖拽上传覆盖层 -->
    <div v-if="dropOver" class="drop-overlay">
      <div class="drop-card">
        <Paperclip :size="30" :stroke-width="1.4" />
        <div class="drop-title">松开以上传到当前对话</div>
        <div class="drop-sub">文件作为附件，发送时供 Claude 读取</div>
      </div>
    </div>

    <!-- 顶栏：标题 / 重命名 / 更多菜单 / 复制反馈 / 抽屉开关 -->
    <ChatTopBar ref="topBarRef" />

    <div class="messages" ref="scrollEl" @scroll.passive="onMessagesScroll">
      <!-- 历史加载骨架 -->
      <div v-if="historyLoading && renderTurns.length === 0" class="hist-skeleton">
        <div class="sk-row user"></div>
        <div class="sk-row"></div>
        <div class="sk-row short"></div>
      </div>
      <!-- 历史加载失败:不假装是空对话 -->
      <div v-else-if="historyErr && renderTurns.length === 0" class="hist-error">
        <span>历史加载失败:{{ historyErr }}</span>
        <button class="hist-retry" @click="retryHistory">重试</button>
      </div>
      <!-- 空对话页：彩蛋 / 问候 / 工作流推荐 -->
      <ChatHero v-else-if="renderTurns.length === 0" @apply="applyFlow" />

      <!-- 专家团工作台：入驻了团/专家（expert-team / single-expert）时显示在消息区下方 -->
      <div v-if="(agentMode === 'expert-team' || agentMode === 'single-expert') && app.currentProjectId" class="expert-team-studio-wrap">
        <ExpertTeamStudio
          :project-id="app.currentProjectId"
          :agents-status="teamAgentsStatus"
        />
      </div>

      <!-- 历史折叠:更早的回合不渲染,点击逐段放开 -->
      <div v-if="hiddenCount > 0" class="earlier-wrap">
        <button class="earlier-btn" @click="showEarlier">
          加载更早的 {{ hiddenCount }} 个回合
        </button>
      </div>

      <!-- 单回合抽成 TurnItem 子组件:流式中 visibleTurns 虽然每帧是新数组,
           但前缀(已定稿)回合的 turn 对象引用不变(前缀缓存保证),且这里的事件
           处理器全是稳定的方法引用(不写内联箭头,否则每帧生成新函数 props 就
           永远不相等)—— Vue 对子组件 props 做浅比较,全等则跳过整棵子树的
           re-render 与 DOM patch,历史回合在流式期间零开销。 -->
      <TurnItem
        v-for="t in visibleTurns"
        :key="t.key"
        :turn="t"
        :pending="isPending(t)"
        :sending="sending"
        :expanded-tool="expandedTool"
        :files-collapsed="!filesExpanded[t.key]"
        @toggle-tool="toggleTool"
        @toggle-files="toggleFiles"
        @open-artifact="openArtifact"
        @open-folder="openFolderCard"
        @edit="editTurn"
        @copy="copyTurn"
        @regenerate="regenerate"
      />

      <!-- 回到底部(上翻后浮现,流式不再硬拽) -->
      <Transition name="copy-fade">
        <button
          v-if="!atBottom && renderTurns.length"
          class="to-bottom"
          title="回到底部"
          @click="scrollToBottom()"
        >
          <ChevronDown :size="16" :stroke-width="2" />
        </button>
      </Transition>
    </div>

    <!-- 输入区域：输入卡片 / 工具条弹层 / 授权栏 / 今日建议(懒加载) -->
    <ChatComposer ref="composerRef" v-model:agent-mode="agentMode" />
  </div>
</template>

<style scoped>
.chat {
  display: flex;
  flex-direction: column;
  height: 100%;
  position: relative;
}

.messages {
  flex: 1;
  overflow-y: auto;
  /* 底部留出输入玻璃卡的悬浮空间：消息从液态玻璃下穿过 */
  padding: 40px 32px 210px;
}

/* copy-fade 过渡也用于「回到底部」浮钮(与顶栏复制提示同款,scoped 各留一份) */
.copy-fade-enter-active,
.copy-fade-leave-active {
  transition: opacity 0.2s ease, transform 0.2s ease;
}
.copy-fade-enter-from,
.copy-fade-leave-to {
  opacity: 0;
  transform: translate(-50%, -4px);
}

/* 对话回合本体的样式(用户/助手气泡、工具 pill、markdown、产物卡片、参考文件等)
   随模板一起搬去 chat/TurnItem.vue,此处只留消息区骨架/折叠/浮钮等容器级样式 */

/* ── 历史骨架 / 加载失败 / 折叠 ── */
.hist-skeleton {
  max-width: 880px;
  margin: 30px auto;
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.sk-row {
  height: 44px;
  border-radius: 12px;
  background: linear-gradient(
    90deg,
    var(--bg-soft) 25%,
    var(--border-soft) 50%,
    var(--bg-soft) 75%
  );
  background-size: 200% 100%;
  animation: sk-shimmer 1.4s ease infinite;
}
.sk-row.user {
  width: 40%;
  align-self: flex-end;
}
.sk-row.short {
  width: 65%;
}
@keyframes sk-shimmer {
  to {
    background-position: -200% 0;
  }
}
.hist-error {
  max-width: 880px;
  margin: 30px auto;
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px 16px;
  border-radius: 10px;
  background: var(--vermilion-soft);
  color: var(--vermilion);
  font-size: 12.5px;
}
.hist-retry {
  padding: 4px 14px;
  border: 1px solid var(--vermilion);
  background: transparent;
  color: var(--vermilion);
  border-radius: 7px;
  font-size: 12px;
  cursor: pointer;
  flex-shrink: 0;
}
.hist-retry:hover {
  background: var(--vermilion);
  color: #fff;
}
.earlier-wrap {
  max-width: 880px;
  margin: 0 auto 18px;
  text-align: center;
}
.earlier-btn {
  padding: 5px 16px;
  border: 1px solid var(--border-soft);
  background: var(--panel);
  color: var(--muted);
  border-radius: 999px;
  font-size: 11.5px;
  cursor: pointer;
  transition: color 0.15s, border-color 0.15s;
}
.earlier-btn:hover {
  color: var(--text);
  border-color: var(--border);
}

/* 回到底部悬浮钮(sticky 钉在滚动容器视口底部) */
.to-bottom {
  position: sticky;
  bottom: 8px;
  left: calc(100% - 60px);
  z-index: 11;
  width: 34px;
  height: 34px;
  border-radius: 50%;
  border: 1px solid var(--border);
  background: var(--panel);
  color: var(--text-2);
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  box-shadow: var(--shadow-lg);
}
.to-bottom:hover {
  color: var(--primary);
  border-color: var(--primary);
}

/* ─────────── 拖拽上传覆盖层 ─────────── */
.drop-overlay {
  position: absolute;
  inset: 10px;
  z-index: 50;
  background: rgba(44, 70, 97, 0.06);
  border: 2px dashed var(--primary);
  border-radius: 14px;
  display: flex;
  align-items: center;
  justify-content: center;
  backdrop-filter: blur(1px);
  pointer-events: none;
}
.drop-card {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  color: var(--primary);
}
.drop-title {
  font-family: var(--serif);
  font-size: 16px;
  font-weight: 600;
  letter-spacing: 1px;
}
.drop-sub {
  font-size: 12px;
  color: var(--muted);
}
</style>
