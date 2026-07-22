<script setup lang="ts">
// 输入区(自 ChatPanel 拆出,逻辑原样搬移):输入卡片 + 工具条(工作模式/技能/模式/
// 智能体/供应商弹层) + 授权栏 + 语音听写 + 附件/贴图 + 发送/停止/清空上下文。
// 状态尽量下沉本组件;与父层(消息区)共享的只有 agentMode(v-model,消息区要显示
// 专家团工作台)与三个 expose:setInput / attachPaths / sendOptions。
import { ref, computed, onMounted, onBeforeUnmount, nextTick, watch, defineAsyncComponent } from "vue";
import {
  Puzzle,
  ChevronDown,
  ChevronRight,
  X,
  ArrowRight,
  Square,
  Sparkles,
  Globe,
  Wrench,
  FileText,
  Table,
  AudioLines,
  Clapperboard,
  Image as ImageIcon,
  Ghost,
  FileCode,
  Target,
  Workflow,
  BookOpen,
  Layers,
  Hand,
  Mic,
  SlidersHorizontal,
  Zap,
  Code2,
  Eraser,
  Check,
} from "@lucide/vue";
import SearchGlass from "../icons/SearchGlass.vue";
import OrbitSpinner from "../icons/OrbitSpinner.vue";
import {
  chat,
  skills as skillsApi,
  expert,
  avatarSlot,
  invoke,
  listen,
  isTauri,
  uploadToBackend,
  type PermissionMode,
  type Skill,
  type AttachedFile,
  type ExpertCard,
  type ExpertTeam as ExpertTeamCard,
} from "../../tauri";
import { WebVoiceRecorder } from "../../lib/webVoice";
import { toast } from "../../composables/useToast";
import { humanizeError } from "../../lib/humanizeError";
import { useAppStore } from "../../stores/app";
import { useSkillsStore } from "../../stores/skills";
import { useChatStore } from "../../stores/chat";
import { useProvidersStore } from "../../stores/providers";
import { useWorkflowsStore } from "../../stores/workflows";
import { useLongTaskStore, detectLongTask } from "../../stores/longtask";
import {
  attachIcon,
  humanSize,
  useIdleRunner,
  type AgentMode,
  type WorkMode,
  type ChatSendOptions,
} from "./shared";

// 「今日建议」整块非首屏必需(空闲帧才拉数据) → 懒加载,别进 ChatPanel 首开 chunk
const BriefingCenter = defineAsyncComponent(() => import("./BriefingCenter.vue"));

const app = useAppStore();
const skillsStore = useSkillsStore();
const chatStore = useChatStore();
const workflowsStore = useWorkflowsStore();
const longTaskStore = useLongTaskStore();

// 与父层共享的智能体模式(消息区据此显示专家团工作台并轮询状态)
const agentMode = defineModel<AgentMode>("agentMode", { required: true });

const input = ref("");
// 每个对话各自的未发送草稿:切走/切回都保留本对话的草稿,且绝不把 A 的半句话
// 带进 B(全局单 ref 会串台、还可能误发到别的对话)。键用 convId,新对话(null)用 ""。
const drafts = new Map<string, string>();
// 多开：当前对话的气泡 / 运行态来自 chat store（按对话 id 维护，切走不丢、后台续流）
const bubbles = computed(() => chatStore.bubblesFor(app.currentConvId));
const sending = computed(() => chatStore.isSending(app.currentConvId));

const showPermDropdown = ref(false);
const permMode = ref<PermissionMode>("manual");
const showSkillPanel = ref(false);
const skillSearch = ref("");
const skillsList = ref<Skill[]>([]);

// ─────────── 目标模式 (Claude Code goal) ───────────
// 开启后，主输入框里写的内容即「完成条件」：Claude 会持续推进直到达成，
// 不中途收尾、不反问。开关随会话持续生效（贴近 session-scoped /goal），手动关闭。
const goalMode = ref(false);
const inputEl = ref<HTMLTextAreaElement | null>(null);

// 输入框高度随内容自动增长（仿豆包）：先归零再按 scrollHeight 撑高，到 CSS max-height 后内部滚动。
function autoGrow() {
  const el = inputEl.value;
  if (!el) return;
  el.style.height = "auto";
  el.style.height = `${el.scrollHeight}px`;
}
// 内容变化（手输 / 程序填入 / 发送清空）都重算高度
watch(input, () => nextTick(autoGrow));
onMounted(() => nextTick(autoGrow));

/** 供父层(编辑重发 / 空白页工作流建议)把整段文字填进输入框并聚焦 */
function setInput(text: string) {
  input.value = text;
  nextTick(() => {
    inputEl.value?.focus();
    autoGrow();
  });
}

// ─────────── 语音听写（输入框麦克风 · 仿豆包/Codex）───────────
// 点麦克风 / 按右 Alt 开始说话，说话时文字流式长进输入框，再点 / 再按右 Alt 结束。
// 后端 voice_dictate_start/stop 录音转写 + 防污染，文字经 voice:dictation 事件回填。
const dictating = ref(false);
const voiceBusy = ref(false); // 浏览器路径:停录后上传+识别的 ~1s,期间禁重复点击
let dictateBase = ""; // 听写开始时输入框已有内容，新转写续在其后
const voiceUnlisteners: Array<() => void> = [];
let webRec: WebVoiceRecorder | null = null;

async function toggleDictate() {
  // 浏览器/Docker:后端无麦克风,采集在客户端做,停录后上传 WAV 走 voice_transcribe_file。
  if (!isTauri) return toggleDictateWeb();
  // 桌面:后端 cpal 录音 + 防污染,文字经 voice:partial/voice:dictation 事件回填。
  try {
    if (!dictating.value) {
      dictateBase = input.value ? input.value.replace(/\s+$/, "") + " " : "";
      await invoke("voice_dictate_start");
      dictating.value = true;
    } else {
      dictating.value = false;
      await invoke("voice_dictate_stop");
    }
  } catch (e) {
    dictating.value = false;
    toast.error(`语音输入：${humanizeError(e)}`);
  }
}

// 浏览器整段批处理:点开始 getUserMedia 录音 → 再点停止 → 16k WAV 上传 → 识别回填。
async function toggleDictateWeb() {
  if (voiceBusy.value) return; // 识别中,忽略点击
  if (!dictating.value) {
    try {
      dictateBase = input.value ? input.value.replace(/\s+$/, "") + " " : "";
      webRec = new WebVoiceRecorder();
      await webRec.start();
      dictating.value = true;
    } catch (e) {
      dictating.value = false;
      webRec = null;
      toast.error(`语音输入：${humanizeError(e)}`);
    }
    return;
  }
  // 停录 → 上传 → 识别
  dictating.value = false;
  const rec = webRec;
  webRec = null;
  voiceBusy.value = true;
  try {
    const wav = await rec?.stop();
    if (!wav) return; // 太短/误触
    const [up] = await uploadToBackend([wav]);
    if (!up?.path) throw new Error("音频上传失败");
    const r = await invoke<{ text?: string; error?: string }>("voice_transcribe_file", {
      path: up.path,
    });
    if (r?.text) {
      input.value = dictateBase + r.text;
      nextTick(() => {
        autoGrow();
        inputEl.value?.focus();
      });
    }
  } catch (e) {
    toast.error(`语音输入：${humanizeError(e)}`);
  } finally {
    voiceBusy.value = false;
  }
}

function onGlobalKeydown(e: KeyboardEvent) {
  // 右 Alt 快捷开关听写（仅本窗口获焦时）。AltGr 在 Win 也以 AltRight 触发。
  if (e.code === "AltRight") {
    e.preventDefault();
    if (!e.repeat) void toggleDictate();
  }
}

// 首帧非关键加载推迟(见 shared.ts useIdleRunner)
const idle = useIdleRunner();

onMounted(async () => {
  window.addEventListener("keydown", onGlobalKeydown);
  // 从「专家团」页点「召唤」后会切回本视图(ChatPanel 重新挂载)→ 在此消费召唤意图。
  consumePendingSummon();
  // 若在别的视图点了工作流包「使用」才切来对话，挂载时补消费一次
  applyInsert(workflowsStore.insertRequest);
  // 技能清单 / 供应商清单 / codex 授权态都只服务于点开面板后的展示,
  // 不影响首屏聊天区渲染 → 推迟到空闲帧再打 IPC
  idle.runWhenIdle(() => {
    void loadSkills();
    providersStore.refresh();
    providersStore.refreshCodex();
  });
  // 流式：说话中把当前转写实时续到输入框（从听写起点之后替换）
  voiceUnlisteners.push(
    await listen<{ text?: string }>("voice:partial", (p) => {
      if (dictating.value && p && typeof p.text === "string") {
        input.value = dictateBase + p.text;
        nextTick(autoGrow);
      }
    })
  );
  // 结束：终稿（防污染后）落定到输入框
  voiceUnlisteners.push(
    await listen<{ text?: string; error?: string; cancelled?: boolean }>("voice:dictation", (f) => {
      dictating.value = false;
      if (f?.error) {
        toast.error(`语音输入：${f.error}`);
        return;
      }
      if (f?.cancelled) return;
      if (typeof f?.text === "string" && f.text) {
        input.value = dictateBase + f.text;
        nextTick(() => {
          autoGrow();
          inputEl.value?.focus();
        });
      }
    })
  );
});

onBeforeUnmount(() => {
  idle.dispose(); // 让尚未执行的空闲回调作废
  window.removeEventListener("keydown", onGlobalKeydown);
  for (const u of voiceUnlisteners) u();
  if (webRec) {
    webRec.cancel();
    webRec = null;
  } else if (dictating.value) {
    void invoke("voice_dictate_stop").catch(() => {});
  }
});

function toggleGoal() {
  goalMode.value = !goalMode.value;
  if (goalMode.value) nextTick(() => inputEl.value?.focus());
}

// ─────────── 动态编排（多智能体）模式开关 ───────────
// 激活后，本条请求按「编排器扇出 N 个独立子任务，每条 实现→对抗式校验→修复，最后汇总」
// 的多智能体方式跑（后端放行 Task 子代理并注入编排指令）。适合可拆分 + 可验证的任务。
const orchestrateMode = ref(false);
function toggleOrchestrate() {
  orchestrateMode.value = !orchestrateMode.value;
  if (orchestrateMode.value) nextTick(() => inputEl.value?.focus());
}

// ─────────── 知识库模式开关（双库强制召回）───────────
// 默认开启：让用户一开箱就体验到知识库便利。开启后后端会替模型先查两个库
//（妈妈库 wiki 权威 + 外库 raw/output 混检 40→重排取优）并把命中片段喂进上下文，
// 同时注入结构化 wiki 导航。关闭则只留极简根路径提示，省 token。
const kbMode = ref(true);
function toggleKb() {
  kbMode.value = !kbMode.value;
  if (kbMode.value) nextTick(() => inputEl.value?.focus());
}

// ─────────── 分批长任务（Batch Build）模式开关 ───────────
// 超长生成（如 60 页 PPT）强制走分批：先规划成清单，每轮只建一小批，断线从清单续跑，
// 避免单轮输出过长把流式连接拖死。关时也会按「N 页/张/章」启发式自动判定长任务。
const batchMode = ref(false);
function toggleBatch() {
  batchMode.value = !batchMode.value;
  if (batchMode.value) nextTick(() => inputEl.value?.focus());
}

// ─────────── 百人专家团模式 ──────────
const expertModeLabels: Record<AgentMode, string> = {
  "single-agent": "单Agent",
  "single-expert": "单专家",
  "expert-team": "专家团",
  "auto-match": "智能匹配",
};

// 「智能体」切换器：基础回答模式（智能匹配 / 单 Agent），与「召唤专家」互斥
const showAgentPanel = ref(false);
const agentModeOptions: { mode: AgentMode; name: string; desc: string; icon: string }[] = [
  {
    mode: "auto-match",
    name: "智能匹配专家团",
    desc: "每轮自动召集最合适的专家，并说明为什么是 TA",
    icon: `<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3v3M12 18v3M3 12h3M18 12h3"/><circle cx="12" cy="12" r="4"/><path d="m5 5 2 2M17 17l2 2M19 5l-2 2M7 17l-2 2"/></svg>`,
  },
  {
    mode: "single-agent",
    name: "单 Agent",
    desc: "关闭专家加成，通用助手直接答，最省",
    icon: `<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="11" width="18" height="10" rx="2"/><circle cx="12" cy="5" r="3"/><path d="M12 8v3"/><path d="M8 15h0M16 15h0"/></svg>`,
  },
];
// ── 「召唤专家」统一入口（仿 WorkBuddy）──
// 单专家与专家团协作合并成同一个动作:「召唤」。召唤一位专家 → 单专家模式;
// 召唤一支业务团 → 专家团协作模式。最近召唤的列在弹层里可一键再召唤,
// 「召唤其它专家」跳转到完整专家团画廊去挑（画廊里每张卡都有「召唤」键）。
const rosterTeams = ref<ExpertTeamCard[]>([]);
const rosterExperts = ref<ExpertCard[]>([]);
const rosterLoaded = ref(false);
const rosterLoading = ref(false);
const avatarSlots = ref<string[]>([]);
const selectedTeamId = ref<string>("");
const selectedExpertId = ref<string>("");

type SummonKind = "expert" | "team";
interface SummonedEntry {
  kind: SummonKind;
  id: string;
  name: string;
  desc: string;
  icon: string;
}

const selectedTeam = computed(
  () => rosterTeams.value.find((t) => t.id === selectedTeamId.value) || null
);
const selectedExpert = computed(
  () => rosterExperts.value.find((e) => e.id === selectedExpertId.value) || null
);

// 工具栏按钮文案：召唤了具体团/专家就显示其名字，否则回退到模式名
const agentModeLabel = computed(() => {
  if (agentMode.value === "expert-team")
    return selectedTeam.value?.name || activeSummonName.value || "专家团";
  if (agentMode.value === "single-expert")
    return selectedExpert.value?.name || activeSummonName.value || "单专家";
  return expertModeLabels[agentMode.value];
});

// ── 最近召唤（持久化到 localStorage，跨会话保留）──
const RECENT_KEY = "polaris.recentSummoned";
const recentSummoned = ref<SummonedEntry[]>([]);
try {
  const raw = localStorage.getItem(RECENT_KEY);
  if (raw) recentSummoned.value = JSON.parse(raw);
} catch {
  /* ignore */
}
function saveRecent() {
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify(recentSummoned.value.slice(0, 8)));
  } catch {
    /* ignore */
  }
}

// 当前被召唤实体是否就是某条最近记录（用于打勾 / 文案兜底）
function isSummonActive(e: SummonedEntry): boolean {
  if (e.kind === "team")
    return agentMode.value === "expert-team" && selectedTeamId.value === e.id;
  return agentMode.value === "single-expert" && selectedExpertId.value === e.id;
}
const activeSummonName = computed(() => {
  const hit = recentSummoned.value.find((e) => isSummonActive(e));
  return hit?.name || "";
});

async function ensureRoster() {
  if (rosterLoaded.value || rosterLoading.value) return;
  rosterLoading.value = true;
  try {
    const [ts, es, slots] = await Promise.all([
      expert.teams(),
      expert.list(),
      expert.avatarSlots(),
    ]);
    rosterTeams.value = ts;
    rosterExperts.value = es;
    avatarSlots.value = slots ?? [];
    rosterLoaded.value = true;
  } catch (e) {
    console.error("加载专家团花名册失败", e);
  } finally {
    rosterLoading.value = false;
  }
}

// id → 头像（本地映射，零额外 IPC）；未就绪返回空串落 emoji 占位
function summonAvatar(e: SummonedEntry): string {
  const slots = avatarSlots.value;
  return slots.length ? slots[avatarSlot(e.id)] ?? "" : "";
}

function toggleAgentPanel() {
  showAgentPanel.value = !showAgentPanel.value;
  if (showAgentPanel.value) ensureRoster();
}

// 选「基础模式」(智能匹配 / 单 Agent)：清掉已召唤的专家，即时生效并收起
function pickAgentMode(m: AgentMode) {
  agentMode.value = m;
  selectedTeamId.value = "";
  selectedExpertId.value = "";
  showAgentPanel.value = false;
}

// 统一「召唤」：召唤专家 → 单专家模式；召唤业务团 → 专家团协作模式
async function summon(kind: SummonKind, id: string) {
  await ensureRoster();
  const pid = app.currentProjectId;
  let entry: SummonedEntry;
  if (kind === "team") {
    const t = rosterTeams.value.find((x) => x.id === id);
    entry = {
      kind,
      id,
      name: t?.name || id,
      desc: t?.tagline || t?.description || "",
      icon: t?.icon || "🧭",
    };
    selectedTeamId.value = id;
    selectedExpertId.value = "";
    agentMode.value = "expert-team";
    if (pid) {
      try {
        await expert.teamApply(pid, id, true);
      } catch (e) {
        console.error("team.apply 失败", e);
      }
    }
  } else {
    const ex = rosterExperts.value.find((x) => x.id === id);
    entry = {
      kind,
      id,
      name: ex?.name || id,
      desc: ex?.role || ex?.description || "",
      icon: ex?.icon || "👤",
    };
    selectedExpertId.value = id;
    selectedTeamId.value = "";
    agentMode.value = "single-expert";
    if (pid) {
      try {
        await expert.apply(pid, id, true);
      } catch (e) {
        console.error("expert.apply 失败", e);
      }
    }
  }
  // 置顶到最近召唤
  recentSummoned.value = [
    entry,
    ...recentSummoned.value.filter((r) => !(r.kind === kind && r.id === id)),
  ].slice(0, 8);
  saveRecent();
  showAgentPanel.value = false;
}

// 「召唤其它专家」→ 收起弹层、跳转到左侧「专家团」功能页(在那里挑卡片召唤);
// 不再就地弹半透明浮层。专家团页点「召唤」会经 app.requestSummon 切回这里由
// consumePendingSummon() 落地。
function openExpertGallery() {
  showAgentPanel.value = false;
  app.setView("claude_md");
}

// 消费「专家团」页投递来的召唤意图(每次挂载即检查一次,消费后清空)。
function consumePendingSummon() {
  const p = app.pendingSummon;
  if (!p) return;
  app.pendingSummon = null;
  summon(p.kind, p.id);
}

// ─────────── 「模式」合并键 ───────────
// 把 目标 / 动态编排 / 知识库 / 分批长任务 四个开关收进一枚「模式」键的弹出面板，
// 减少工具栏拥挤。底层 4 个 ref 与发送逻辑保持不变，这里只是统一的开关入口。
const showModePanel = ref(false);
const activeModeCount = computed(
  () =>
    (goalMode.value ? 1 : 0) +
    (orchestrateMode.value ? 1 : 0) +
    (kbMode.value ? 1 : 0) +
    (batchMode.value ? 1 : 0)
);
const activeModeSummary = computed(() => {
  const on: string[] = [];
  if (goalMode.value) on.push("目标");
  if (orchestrateMode.value) on.push("编排");
  if (kbMode.value) on.push("知识库");
  if (batchMode.value) on.push("分批");
  return on.join(" · ");
});

// ─────────── 工作模式: 快速 / 工作 ───────────
// 快速模式(默认): 「快速调用知识库 + 快速回答」。强制走知识库召回的「快档」(双车道融合但跳过
//   重排 API, ~1.8s→~0.25s)+ 工具精简(弃 Task/NotebookEdit)+ 提示词瘦身(跳「可运行项目」「长
//   任务」约定)+ 上下文预算调小 + 默认自动批准 —— 一切为秒级查库、秒级回答。
// 工作模式: 纯 Claude Code —— 放开全套工具 + 注入全部约定(可运行项目/长任务)+ 全质量召回(带
//   重排)+ 手动授权, 面向写代码 / 跑项目 / 产出复杂成品。随设备记忆(localStorage), 默认快速。
const workMode = ref<WorkMode>(
  localStorage.getItem("polaris.workMode") === "work" ? "work" : "fast"
);
const showWorkModePanel = ref(false);
watch(workMode, (m) => localStorage.setItem("polaris.workMode", m));
const workModeLabel = computed(() =>
  workMode.value === "work" ? "工作模式" : "快速模式"
);
const workModeOptions: { mode: WorkMode; name: string; desc: string }[] = [
  {
    mode: "fast",
    name: "快速模式",
    desc: "快速查库 + 快速回答：召回走快档(跳重排)、工具精简、提示词瘦身、自动批准；日常问答/找资料/速览首选",
  },
  {
    mode: "work",
    name: "工作模式",
    desc: "纯 Claude Code：全套工具 + 全部约定 + 全质量召回 + 手动授权；写代码·跑项目·产复杂成品时切到这",
  },
];
// 切换工作模式即套用该模式的聪明默认(用户随后仍可手动覆盖):
//   快速 = 自动批准编辑(少弹窗) + 默认开知识库(本模式本职就是快速调用知识库);
//   工作 = 手动授权(纯 Claude Code, 改东西逐步确认) + 默认关知识库(要查再开)。
function applyModeDefaults(m: WorkMode) {
  permMode.value = m === "work" ? "manual" : "auto_current";
  kbMode.value = m === "fast";
}
function pickWorkMode(m: WorkMode) {
  workMode.value = m;
  applyModeDefaults(m);
  showWorkModePanel.value = false;
}
// 初始按记忆的模式套一次默认(权限/知识库本就每次挂载重置, 这里只是按模式给更合适的初值)
applyModeDefaults(workMode.value);

// ─────────── API/模型切换:每个对话各用各的供应商(真隔离) ───────────
// 选项**自动来自左下角「API 供应商」中心**(只列已配好 Key / 已授权的那些)。每个对话各记一份
// 选择,发消息时透传 providerId,后端逐命令注入该家 env → 多对话并发也互不串台。
// "auto" = 沿用应用全局当前供应商(新对话默认)。空白页(还没建对话)选的暂存到 pending,
// 首次发送创建对话后迁移给它。
const providersStore = useProvidersStore();
const showProviderPanel = ref(false);
const PROVIDER_BIND_KEY = "polaris.convProvider.v1";
function loadConvProvider(): Record<string, string> {
  try {
    return JSON.parse(localStorage.getItem(PROVIDER_BIND_KEY) || "{}") || {};
  } catch {
    return {};
  }
}
// convId → providerId 绑定表(持久化, 切对话/重启都记得)
const convProvider = ref<Record<string, string>>(loadConvProvider());
watch(
  convProvider,
  (v) => localStorage.setItem(PROVIDER_BIND_KEY, JSON.stringify(v)),
  { deep: true }
);
// 空白页(无 currentConvId)时用户先选的供应商, 首次发送时落到新对话
const pendingProvider = ref<string>("auto");

function providerForConv(convId: string | null | undefined): string {
  if (!convId) return pendingProvider.value;
  return convProvider.value[convId] || "auto";
}
function hostOf(url: string): string {
  if (!url) return "";
  try {
    return new URL(url).host;
  } catch {
    return url.replace(/^https?:\/\//, "").replace(/\/.*$/, "");
  }
}
function providerSub(p: { kind: string; baseUrl: string }): string {
  if (p.kind === "official") return "Claude 官方订阅";
  if (p.kind === "codex") return "ChatGPT · GPT-5.5";
  return hostOf(p.baseUrl) || p.kind;
}
// 自动识别:只列已配 Key / 可用的供应商(official 恒可用;key 类需 hasKey;codex 需已授权)
const availableProviders = computed(() =>
  providersStore.providers.filter(
    (p) => p.hasKey || (p.kind === "codex" && providersStore.codex?.loggedIn)
  )
);
// 切换器选项 = Auto + 已配供应商
const providerOptions = computed(() => [
  { id: "auto", name: "Auto", sub: "跟随左下角当前默认供应商", auto: true },
  ...availableProviders.value.map((p) => ({
    id: p.id,
    name: p.name,
    sub: providerSub(p),
    auto: false,
  })),
]);
const currentProviderId = computed(() => providerForConv(app.currentConvId));
const currentProviderName = computed(() => {
  const id = currentProviderId.value;
  if (id === "auto") return "Auto";
  return providersStore.providers.find((x) => x.id === id)?.name || "Auto";
});
function pickProvider(id: string) {
  const cid = app.currentConvId;
  if (cid) {
    convProvider.value = { ...convProvider.value, [cid]: id };
  } else {
    pendingProvider.value = id;
  }
  showProviderPanel.value = false;
}

/** 当前工具条各开关拼出的发送选项(父层「重新生成」/「今日建议」与主发送共用同一份形状) */
function sendOptions(convId?: string | null): ChatSendOptions {
  return {
    permissionMode: permMode.value,
    skillIds: Array.from(skillsStore.enabledSkills),
    useKb: kbMode.value || undefined,
    agentMode: agentMode.value,
    workMode: workMode.value,
    providerId: providerForConv(convId),
  };
}

// ─────────── 工作流包「使用」→ 填入输入框 ───────────
// 右侧「工作流包」点「使用」时，store 发来拼装好的提示词：已有内容则追加，否则填入；
// 随后聚焦并把光标移到末尾。带 nonce 以便重复使用同一包也能触发。
function applyInsert(req: { text: string; n: number } | null | undefined) {
  if (!req || !req.text) return;
  const cur = input.value.trimEnd();
  input.value = cur ? `${cur}\n\n${req.text}` : req.text;
  workflowsStore.clearInsert();
  nextTick(() => {
    const el = inputEl.value;
    if (!el) return;
    el.focus();
    el.selectionStart = el.selectionEnd = el.value.length;
    el.scrollTop = el.scrollHeight;
  });
}
watch(() => workflowsStore.insertRequest, applyInsert);

// ─────────── 拖拽上传附件到当前对话 ───────────
const attachments = ref<AttachedFile[]>([]);
/** 上传中的占位（大文件复制需要时间，显示转圈） */
const pendingAttach = ref<{ name: string }[]>([]);

async function onDropFiles(paths: string[]) {
  const convId = await ensureConversation();
  const placeholders = paths.map((p) => ({
    name: p.split(/[\\/]/).pop() || p,
  }));
  pendingAttach.value.push(...placeholders);
  try {
    const res = await chat.attachFiles(convId ?? undefined, paths);
    for (const r of res) {
      if (r.ok) attachments.value.push(r);
      else if (convId)
        chatStore.pushBubble(convId, {
          role: "assistant",
          text: `[附件失败] ${r.name}:${r.error ?? ""}`,
        });
    }
  } catch (e: any) {
    if (convId)
      chatStore.pushBubble(convId, {
        role: "assistant",
        text: `[附件失败] ${e?.message ?? e}`,
      });
  } finally {
    for (const ph of placeholders) {
      const idx = pendingAttach.value.indexOf(ph);
      if (idx >= 0) pendingAttach.value.splice(idx, 1);
    }
  }
}

function removeAttachment(i: number) {
  attachments.value.splice(i, 1);
}

// ─────────── 剪贴板贴图(截图 → Ctrl+V 直接成附件) ───────────
function fileToBase64(f: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result).split(",")[1] ?? "");
    r.onerror = () => reject(r.error);
    r.readAsDataURL(f);
  });
}

async function onPaste(e: ClipboardEvent) {
  const items = e.clipboardData?.items;
  if (!items) return;
  const imgs: File[] = [];
  for (const it of Array.from(items)) {
    if (it.kind === "file" && it.type.startsWith("image/")) {
      const f = it.getAsFile();
      if (f) imgs.push(f);
    }
  }
  if (!imgs.length) return; // 纯文本粘贴走默认行为
  e.preventDefault();
  const convId = await ensureConversation();
  for (const f of imgs) {
    const ext = (f.type.split("/")[1] || "png").replace("jpeg", "jpg");
    const name =
      f.name && f.name !== "image.png"
        ? f.name
        : `粘贴图片-${new Date().toISOString().slice(11, 19).replace(/:/g, "")}.${ext}`;
    const ph = { name };
    pendingAttach.value.push(ph);
    try {
      const b64 = await fileToBase64(f);
      const res = await chat.attachImage(convId ?? undefined, name, b64);
      if (res?.ok) attachments.value.push(res);
      else toast.error(`贴图失败:${res?.error ?? "未知错误"}`);
    } catch (err) {
      toast.error(`贴图失败:${humanizeError(err)}`);
    } finally {
      const idx = pendingAttach.value.indexOf(ph);
      if (idx >= 0) pendingAttach.value.splice(idx, 1);
    }
  }
}

const permLabel: Record<PermissionMode, string> = {
  manual: "手动授权",
  auto_current: "自动 · 仅当前会话",
  auto_all: "自动 · 完全放行",
  deny: "拒绝授权",
};

// Load skills for panel
async function loadSkills() {
  try {
    skillsList.value = await skillsApi.list();
  } catch {
    skillsList.value = [
      {
        id: "deep-research",
        name: "深度搜索",
        description:
          "使用 LLM 大规模联网搜索相关内容，自动检索、汇总、交叉验证多来源信息",
        source: "third-party",
      },
      {
        id: "skill-creator",
        name: "Skill 创建向导",
        description: "引导用户创建自定义 Skill，自动生成模板和配置文件",
        source: "official",
      },
    ];
  }
}

function filteredSkills() {
  if (!skillSearch.value.trim()) return skillsList.value;
  const q = skillSearch.value.toLowerCase();
  return skillsList.value.filter(
    (s) =>
      s.name.toLowerCase().includes(q) ||
      s.description.toLowerCase().includes(q)
  );
}

function skillIcon(id: string) {
  const map: Record<string, any> = {
    "deep-research": Globe,
    "skill-creator": Wrench,
    pdf: FileText,
    xlsx: Table,
    "edge-tts": AudioLines,
    hyperframes: Clapperboard,
    "web-search": SearchGlass,
    "image-gen": ImageIcon,
    "cloak-browser": Ghost,
  };
  return map[id] ?? Sparkles;
}

function goToSkillCenter() {
  showSkillPanel.value = false;
  app.setView("skill_center");
}

function toggleSkill(id: string) {
  skillsStore.toggle(id);
  showSkillPanel.value = false;
}

function clearActiveSkill(id: string) {
  skillsStore.remove(id);
}

// 切换对话：草稿按对话隔离(先存上一对话的草稿,再载入新对话的草稿,没有则空);
// 输入历史召回索引也跟着对话走,别串台。(消息区的历史加载/滚动在父层各自处理)
watch(
  () => app.currentConvId,
  (cid, prev) => {
    drafts.set(prev ?? "", input.value);
    input.value = drafts.get(cid ?? "") ?? "";
    histIdx = -1;
    nextTick(autoGrow); // 草稿可能多行,水合后重算高度
  }
);

async function ensureConversation(): Promise<string | null> {
  if (app.currentConvId) return app.currentConvId;
  let pid = app.currentProjectId;
  if (!pid) {
    await app.refreshProjects();
    pid = app.currentProjectId;
  }
  if (!pid) {
    const p = await app.createProject("默认项目");
    pid = p.id;
  }
  const c = await app.createConversation(pid);
  return c.id;
}

async function send() {
  const text = input.value.trim();
  const attached = attachments.value.slice();
  const hasAttach = attached.length > 0;
  // 多开：只拦「当前对话」正在发送，不阻止在别的对话并行发起
  if ((!text && !hasAttach) || sending.value) return;

  // 先清空输入/草稿,再 ensureConversation（它创建新对话会切换 currentConvId、
  // 触发上面的草稿水合）—— 否则刚打的字会被当成新对话的草稿残留。失败再还回去。
  drafts.delete(app.currentConvId ?? "");
  input.value = "";
  attachments.value = [];
  histIdx = -1;

  const convId = await ensureConversation();
  if (!convId) {
    input.value = text; // 创建对话失败:把文字还给用户,别让人白打一通
    return;
  }

  // 空白页时选的供应商(pending)落到这条新对话, 之后它就记住自己用哪家;随后复位 pending。
  if (pendingProvider.value !== "auto" && !convProvider.value[convId]) {
    convProvider.value = { ...convProvider.value, [convId]: pendingProvider.value };
    pendingProvider.value = "auto";
  }
  const sendProviderId = providerForConv(convId);

  // 把附件绝对路径拼进 prompt，让 claude 能用 Read 等工具读取
  let prompt = text || "请查看我上传的附件。";
  if (hasAttach) {
    const lines = attached.map((a) => `- ${a.path}`).join("\n");
    prompt += `\n\n---\n[附件]（用户拖拽上传，可用 Read 等工具读取）：\n${lines}`;
  }

  const display = text || "（仅附件）";

  // 分批长任务：显式开关 或 启发式判定（「N 页/张/章」且 N ≥ 阈值）→ 走分批编排循环，
  // 先规划成清单再每轮只建一小批，断线从清单续跑，规避单轮过长把连接拖死。
  // （目标等专用模式优先，不与分批叠加。）
  const wantBatch =
    !goalMode.value &&
    !orchestrateMode.value &&
    (batchMode.value || detectLongTask(prompt));
  if (wantBatch) {
    await longTaskStore.runBatchBuild(convId, prompt, display, {
      permissionMode: permMode.value,
      skillIds: Array.from(skillsStore.enabledSkills),
      useKb: kbMode.value || undefined,
      providerId: sendProviderId,
    });
    return;
  }

  // 交给 chat store：推 user 气泡 + 调后端 + 记录 reqId/sending（按对话 id，多开）
  await chatStore.send(convId, prompt, display, attached, {
    permissionMode: permMode.value,
    skillIds: Array.from(skillsStore.enabledSkills),
    // 目标模式下，本条输入框内容即完成条件
    goal: goalMode.value && text ? text : undefined,
    dynamicWorkflow: orchestrateMode.value || undefined,
    useKb: kbMode.value || undefined,
    agentMode: agentMode.value,
    workMode: workMode.value,
    providerId: sendProviderId,
  });
}

async function cancel() {
  // 先停掉分批编排循环（否则它会在本轮 done 后又发下一批），再取消在飞的子进程
  if (app.currentConvId) longTaskStore.stop(app.currentConvId);
  await chatStore.cancel(app.currentConvId);
}

// ── 清空上下文（右下角橡皮擦）：消息清零避免上下文过长;旧内容后台自动沉淀入记忆库 ──
const clearingCtx = ref(false);
async function clearContext() {
  const cid = app.currentConvId;
  if (!cid || clearingCtx.value) return;
  if (
    !confirm(
      "清空本对话的全部历史上下文？\n\n有价值的内容（反馈、偏好、决策）会自动沉淀进记忆库，生成的文件不受影响。"
    )
  )
    return;
  clearingCtx.value = true;
  try {
    await chatStore.clearContext(cid);
    toast.success("上下文已清空，旧对话正在后台沉淀入记忆库");
  } catch (e: any) {
    toast.error(`清空失败：${humanizeError(e)}`);
  } finally {
    clearingCtx.value = false;
  }
}

function pickPerm(m: PermissionMode) {
  permMode.value = m;
  showPermDropdown.value = false;
}

// ── 输入历史召回:空输入框时 ↑ 召回上一条发过的消息,↓ 往回走/清空 ──
let histIdx = -1;
const userTexts = computed(() =>
  bubbles.value.filter((b) => b.role === "user" && b.text).map((b) => b.text)
);
function recallHistory(dir: 1 | -1): boolean {
  const hist = userTexts.value;
  if (!hist.length) return false;
  if (dir === 1) {
    // 往更早走
    if (histIdx === -1 && input.value.trim()) return false; // 有草稿不打断
    histIdx = Math.min(histIdx + 1, hist.length - 1);
  } else {
    if (histIdx <= 0) {
      histIdx = -1;
      input.value = "";
      return true;
    }
    histIdx--;
  }
  input.value = hist[hist.length - 1 - histIdx] ?? "";
  nextTick(() => {
    const el = inputEl.value;
    if (el) el.selectionStart = el.selectionEnd = el.value.length;
  });
  return true;
}

// ─────────── 常驻 claude 进程预热(输入区触发) ───────────
// 输入框首次聚焦/首个键入 = 「马上要发消息」的最强信号: 立刻预热常驻进程, 让 CLI
// ~6.4s 的自举在用户打字期间跑完, 首条消息首响 ~10s → ~3s。每对话只发一次(切对话
// 后对新对话再发; store 内另有 60s 去抖兜底), 传输入区**当下真实档位**(权限/工作
// 模式/供应商) —— 比「切对话」触发点的 localStorage 推断更准, 指纹与真发送对齐。
const prewarmedConv = ref<string | null>(null);
function prewarmOnType() {
  const cid = app.currentConvId;
  if (!cid || prewarmedConv.value === cid) return;
  prewarmedConv.value = cid;
  chatStore.prewarm(cid, {
    permissionMode: permMode.value,
    workMode: workMode.value,
    providerId: providerForConv(cid),
    // 编排模式进指纹: 与真发送(dynamicWorkflow: orchestrateMode.value)对齐, 否则预热白做。
    dynamicWorkflow: orchestrateMode.value || undefined,
  });
}

function onKeydown(e: KeyboardEvent) {
  prewarmOnType(); // 首个键入即预热(每对话一次, 之后零成本)
  if (e.isComposing || (e as any).keyCode === 229) return;
  if (e.key === "ArrowUp" && !e.shiftKey && !e.ctrlKey && !e.metaKey) {
    const el = inputEl.value;
    if (el && el.selectionStart === 0 && el.selectionEnd === 0) {
      if (recallHistory(1)) {
        e.preventDefault();
        return;
      }
    }
  }
  if (e.key === "ArrowDown" && histIdx >= 0 && !e.shiftKey) {
    const el = inputEl.value;
    if (el && el.selectionEnd === el.value.length) {
      if (recallHistory(-1)) {
        e.preventDefault();
        return;
      }
    }
  }
  // Esc 中断本轮生成 —— 对齐 CLI 肌肉记忆:不用挪鼠标去点停止按钮。
  if (e.key === "Escape" && sending.value) {
    e.preventDefault();
    cancel();
    return;
  }
  if (e.key !== "Enter") return;
  // Shift+Enter 仍然换行
  if (e.shiftKey) return;
  e.preventDefault();
  send();
}

async function newChat() {
  let pid = app.currentProjectId;
  if (!pid) {
    await app.refreshProjects();
    pid = app.currentProjectId;
  }
  if (!pid) {
    const p = await app.createProject("默认项目");
    pid = p.id;
  }
  await app.createConversation(pid);
}
// (newChat 暂无模板入口,保留与拆分前一致的行为与能力)
void newChat;

// 父层需要的三个入口:编辑重发/工作流建议填入输入框、拖拽附件、发送选项
defineExpose({ setInput, attachPaths: onDropFiles, sendOptions });
</script>

<template>
  <!-- 输入区域 -->
  <div class="input-area">
    <!-- 技能选择弹窗 -->
    <div v-if="showSkillPanel" class="skill-panel">
      <div class="skill-panel-head">
        <span class="skill-panel-title">选择技能</span>
        <button class="skill-panel-close" @click="showSkillPanel = false">
          <X :size="14" :stroke-width="2" />
        </button>
      </div>
      <div class="skill-panel-search">
        <SearchGlass :size="14" :stroke-width="1.8" class="sp-search-icon" />
        <input v-model="skillSearch" placeholder="搜索技能..." type="text" />
      </div>
      <div class="skill-panel-list">
        <div
          v-for="s in filteredSkills()"
          :key="s.id"
          class="skill-panel-item"
          :class="{ active: skillsStore.has(s.id) }"
          @click="toggleSkill(s.id)"
        >
          <component
            :is="skillIcon(s.id)"
            :size="16"
            :stroke-width="1.6"
            class="sp-item-icon"
          />
          <div class="sp-item-info">
            <div class="sp-item-name">{{ s.name }}</div>
            <div class="sp-item-desc">{{ s.description }}</div>
          </div>
        </div>
      </div>
      <div class="skill-panel-foot">
        <button class="sp-manage" @click="goToSkillCenter">
          <ArrowRight :size="12" :stroke-width="2" />
          <span>探索和管理技能</span>
        </button>
      </div>
    </div>

    <!-- 「模式」弹窗：目标 / 动态编排 / 知识库 / 分批长任务 合并到一处 -->
    <div v-if="showModePanel" class="mode-panel">
      <div class="skill-panel-head">
        <span class="skill-panel-title">模式</span>
        <button class="skill-panel-close" @click="showModePanel = false">
          <X :size="14" :stroke-width="2" />
        </button>
      </div>
      <div class="mode-list">
        <button class="mode-row" :class="{ on: goalMode }" @click="toggleGoal">
          <Target :size="16" :stroke-width="1.7" class="mr-ic" />
          <span class="mr-tx">
            <span class="mr-nm">目标模式</span>
            <span class="mr-ds">设一个完成条件，持续推进直到达成，不中途收尾、不反问</span>
          </span>
          <span class="mr-sw" :class="{ on: goalMode }"></span>
        </button>
        <button class="mode-row" :class="{ on: orchestrateMode }" @click="toggleOrchestrate">
          <Workflow :size="16" :stroke-width="1.7" class="mr-ic" />
          <span class="mr-tx">
            <span class="mr-nm">动态编排（多智能体）</span>
            <span class="mr-ds">拆成多个独立子任务并行干，每条 实现→校验→修复；可拆分+可验证才用，更贵</span>
          </span>
          <span class="mr-sw" :class="{ on: orchestrateMode }"></span>
        </button>
        <button class="mode-row" :class="{ on: kbMode }" @click="toggleKb">
          <BookOpen :size="16" :stroke-width="1.7" class="mr-ic" />
          <span class="mr-tx">
            <span class="mr-nm">知识库</span>
            <span class="mr-ds">注入完整 KB 结构化 wiki + 双链地图（消耗较多 token，默认关）</span>
          </span>
          <span class="mr-sw" :class="{ on: kbMode }"></span>
        </button>
        <button class="mode-row" :class="{ on: batchMode }" @click="toggleBatch">
          <Layers :size="16" :stroke-width="1.7" class="mr-ic" />
          <span class="mr-tx">
            <span class="mr-nm">分批长任务</span>
            <span class="mr-ds">超长生成先规划成清单，每轮只建一小批，断线从断点续跑</span>
          </span>
          <span class="mr-sw" :class="{ on: batchMode }"></span>
        </button>
      </div>
    </div>

    <!-- 「模式」切换器：快速 / 工作 两套预设 -->
    <div v-if="showWorkModePanel" class="mode-panel work-mode-panel">
      <div class="skill-panel-head">
        <span class="skill-panel-title">模式</span>
        <button class="skill-panel-close" @click="showWorkModePanel = false">
          <X :size="14" :stroke-width="2" />
        </button>
      </div>
      <div class="mode-list">
        <button
          v-for="opt in workModeOptions"
          :key="opt.mode"
          class="mode-row exclusive"
          :class="{ on: workMode === opt.mode }"
          @click="pickWorkMode(opt.mode)"
        >
          <span class="mr-ic">
            <Zap v-if="opt.mode === 'fast'" :size="17" :stroke-width="1.8" />
            <Code2 v-else :size="17" :stroke-width="1.8" />
          </span>
          <span class="mr-tx">
            <span class="mr-nm">{{ opt.name }}<span v-if="opt.mode === 'fast'" class="mr-default">默认</span></span>
            <span class="mr-ds">{{ opt.desc }}</span>
          </span>
          <span class="mr-radio" :class="{ on: workMode === opt.mode }"></span>
        </button>
      </div>
    </div>

    <!-- 「智能体」切换器：基础模式 + 召唤专家（单专家 / 专家团合并，仿 WorkBuddy） -->
    <div v-if="showAgentPanel" class="mode-panel agent-panel">
      <div class="skill-panel-head">
        <span class="skill-panel-title">智能体 · 谁来回答</span>
        <button class="skill-panel-close" @click="showAgentPanel = false">
          <X :size="14" :stroke-width="2" />
        </button>
      </div>
      <div class="mode-list">
        <!-- 基础回答模式：智能匹配 / 单 Agent（互斥） -->
        <button
          v-for="opt in agentModeOptions"
          :key="opt.mode"
          class="mode-row exclusive"
          :class="{ on: agentMode === opt.mode }"
          @click="pickAgentMode(opt.mode)"
        >
          <span class="mr-ic" v-html="opt.icon"></span>
          <span class="mr-tx">
            <span class="mr-nm">{{ opt.name }}<span v-if="opt.mode === 'auto-match'" class="mr-default">默认</span></span>
            <span class="mr-ds">{{ opt.desc }}</span>
          </span>
          <span class="mr-radio" :class="{ on: agentMode === opt.mode }"></span>
        </button>

        <!-- 召唤专家：单专家 + 专家团合并成一个动作 -->
        <div class="summon-sec">
          <div class="summon-head">最近召唤专家</div>
          <div v-if="recentSummoned.length" class="summon-list">
            <button
              v-for="e in recentSummoned"
              :key="e.kind + ':' + e.id"
              class="summon-row"
              :class="{ on: isSummonActive(e) }"
              @click="summon(e.kind, e.id)"
            >
              <img
                v-if="summonAvatar(e)"
                class="summon-av"
                decoding="async"
                :src="summonAvatar(e)"
                :alt="e.name"
              />
              <span v-else class="summon-ic">{{ e.icon }}</span>
              <span class="summon-tx">
                <span class="summon-nm">
                  {{ e.name }}
                  <span class="summon-kind">{{ e.kind === 'team' ? '专家团' : '专家' }}</span>
                </span>
                <span class="summon-ds">{{ e.desc }}</span>
              </span>
              <Check v-if="isSummonActive(e)" :size="15" :stroke-width="2.4" class="summon-check" />
            </button>
          </div>
          <div v-else class="summon-empty">
            还没召唤过专家 · 点下方「召唤其它专家」挑一位专家或一支业务团
          </div>
          <button class="summon-more" @click="openExpertGallery">
            <span class="sm-ic">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M22 10 12 5 2 10l10 5 10-5Z"/><path d="M6 12v5c0 1.5 2.7 3 6 3s6-1.5 6-3v-5"/></svg>
            </span>
            召唤其它专家
            <ChevronRight :size="15" :stroke-width="2" class="sm-arrow" />
          </button>
        </div>
      </div>
    </div>


    <!-- 「API / 模型」切换器：每个对话各用各的供应商(选项自动来自左下角 API 中心) -->
    <div v-if="showProviderPanel" class="mode-panel provider-panel">
      <div class="skill-panel-head">
        <span class="skill-panel-title">自动模式</span>
        <button class="skill-panel-close" @click="showProviderPanel = false">
          <X :size="14" :stroke-width="2" />
        </button>
      </div>
      <div class="prov-hint">这条对话用哪个 API · 每个对话各记各的、互不串台</div>
      <div class="mode-list">
        <button
          v-for="opt in providerOptions"
          :key="opt.id"
          class="mode-row exclusive"
          :class="{ on: currentProviderId === opt.id }"
          @click="pickProvider(opt.id)"
        >
          <span class="mr-tx">
            <span class="mr-nm">
              {{ opt.name }}
              <span v-if="opt.auto" class="mr-default">默认</span>
            </span>
            <span class="mr-ds">{{ opt.sub }}</span>
          </span>
          <span class="mr-radio" :class="{ on: currentProviderId === opt.id }"></span>
        </button>
      </div>
      <button class="prov-add" @click="providersStore.openAdd(null)">
        ＋ 配置 / 添加供应商
      </button>
    </div>


    <!-- 输入卡片 -->
    <div class="input-card" :class="{ 'goal-on': goalMode }">
      <!-- Skill 标签 -->
      <div v-if="skillsStore.enabledSkills.size > 0" class="skill-tags">
        <div
          v-for="s in skillsList.filter((x) => skillsStore.has(x.id))"
          :key="s.id"
          class="skill-tag"
          @click="clearActiveSkill(s.id)"
        >
          <component :is="skillIcon(s.id)" :size="12" :stroke-width="1.8" />
          <span>{{ s.name }}</span>
          <X :size="10" :stroke-width="2" class="tag-close" />
        </div>
      </div>
      <!-- 待发送附件 -->
      <div
        v-if="attachments.length || pendingAttach.length"
        class="attach-chips"
      >
        <div
          v-for="(f, i) in attachments"
          :key="f.path"
          class="attach-chip"
          :title="f.path"
        >
          <component :is="attachIcon(f.kind)" :size="14" :stroke-width="1.7" />
          <span class="ac-name">{{ f.name }}</span>
          <span class="ac-size">{{ humanSize(f.size) }}</span>
          <button class="ac-remove" title="移除" @click="removeAttachment(i)">
            <X :size="11" :stroke-width="2" />
          </button>
        </div>
        <div
          v-for="(p, i) in pendingAttach"
          :key="'pending-' + i"
          class="attach-chip pending"
          :title="p.name"
        >
          <OrbitSpinner :size="14" />
          <span class="ac-name">{{ p.name }}</span>
        </div>
      </div>
      <textarea
        ref="inputEl"
        v-model="input"
        :placeholder="
          sending
            ? '生成中 …（按 Esc 或点 ■ 停止本轮）'
            : goalMode
            ? '目标模式：在此写下完成条件，Claude 会持续推进直到达成 (Enter 发送) …'
            : '请输入消息 (Enter 发送 · Shift + Enter 换行，可拖文件进来作为附件) …'
        "
        rows="2"
        @keydown="onKeydown"
        @focus="prewarmOnType"
        @input="autoGrow"
        @paste="onPaste"
      ></textarea>
      <div class="toolbar">
        <div class="toolbar-left">
          <button
            class="toolbar-btn work-mode-btn"
            :class="{ active: showWorkModePanel, work: workMode === 'work' }"
            @click="showWorkModePanel = !showWorkModePanel"
          >
            <Zap v-if="workMode === 'fast'" :size="14" :stroke-width="1.8" />
            <Code2 v-else :size="14" :stroke-width="1.8" />
            <span>{{ workModeLabel }}</span>
            <div class="btn-tooltip">
              <div class="btn-tooltip-inner">
                快速 / 工作 两套预设
                <div class="btn-tooltip-sub">
                  快速模式：强制快速查库 + 快速回答（精简工具、跳重排、瘦身提示词、自动批准）；工作模式：纯 Claude Code 全套
                </div>
              </div>
            </div>
          </button>
          <button
            class="toolbar-btn"
            :class="{ active: showSkillPanel }"
            @click="showSkillPanel = !showSkillPanel"
          >
            <Puzzle :size="14" :stroke-width="1.8" />
            <span>技能</span>
          </button>
          <button
            class="toolbar-btn"
            :class="{ active: skillsStore.has('deep-research') }"
            @click="toggleSkill('deep-research')"
          >
            <SearchGlass :size="14" :stroke-width="1.8" />
            <span>深度搜索</span>
            <div class="btn-tooltip">
              <div class="btn-tooltip-inner">
                使用 LLM 大规模联网搜索相关内容
                <div class="btn-tooltip-sub">
                  激活后 Claude 会自动检索多来源信息并交叉验证
                </div>
              </div>
            </div>
          </button>
          <button
            class="toolbar-btn"
            :class="{ active: activeModeCount > 0 || showModePanel }"
            @click="showModePanel = !showModePanel"
          >
            <SlidersHorizontal :size="14" :stroke-width="1.8" />
            <span>{{ activeModeCount > 0 ? `模式 · ${activeModeSummary}` : "模式" }}</span>
            <div class="btn-tooltip">
              <div class="btn-tooltip-inner">
                在一处统一开关：目标模式 / 动态编排 / 知识库 / 分批长任务
                <div class="btn-tooltip-sub">
                  默认全关（单 agent 直接答）；按这件事的需要逐项打开，可叠加
                </div>
              </div>
            </div>
          </button>
          <button
            class="toolbar-btn agent-toggle"
            :class="{ active: agentMode !== 'single-agent' || showAgentPanel }"
            @click="toggleAgentPanel"
          >
            <Sparkles :size="14" :stroke-width="1.8" />
            <span>{{ agentModeLabel }}</span>
            <div class="btn-tooltip">
              <div class="btn-tooltip-inner">
                谁来回答这条消息
                <div class="btn-tooltip-sub">
                  默认「智能匹配」自动召集最合适的专家；也可召唤指定专家 / 专家团，或切回单 Agent
                </div>
              </div>
            </div>
          </button>
          <button
            class="toolbar-btn provider-btn"
            :class="{ active: showProviderPanel || currentProviderId !== 'auto' }"
            @click="showProviderPanel = !showProviderPanel"
          >
            <Layers :size="14" :stroke-width="1.8" />
            <span>{{ currentProviderName }}</span>
            <ChevronDown
              :size="12"
              :stroke-width="2"
              class="prov-caret"
              :class="{ flip: showProviderPanel }"
            />
            <div class="btn-tooltip">
              <div class="btn-tooltip-inner">
                这条对话用哪个 API / 模型
                <div class="btn-tooltip-sub">
                  选项自动来自左下角「API 供应商」里已配的那些；每个对话各记各的、互不串台。Auto = 用当前默认供应商
                </div>
              </div>
            </div>
          </button>
        </div>
        <div class="toolbar-right">
          <button
            v-if="bubbles.length && !sending"
            class="clear-ctx-btn"
            :disabled="clearingCtx"
            title="清空上下文：清空本对话历史避免上下文过长；有价值内容自动沉淀进记忆库，文件不受影响"
            @click="clearContext"
          >
            <Eraser :size="15" :stroke-width="1.9" />
          </button>
          <button
            class="mic-btn"
            :class="{ live: dictating, busy: voiceBusy }"
            :disabled="voiceBusy"
            :title="voiceBusy ? '识别中…' : dictating ? '正在听写 · 点击 / 右 Alt 结束' : '语音输入 · 点击 / 按右 Alt 开始，再按一下结束'"
            @click="toggleDictate"
          >
            <Mic :size="15" :stroke-width="1.9" />
            <span v-if="dictating || voiceBusy" class="mic-ping"></span>
            <div class="mic-tip">
              语音输入 · 按 <b>右 Alt</b> 快捷开关
              <div class="mic-tip-sub">说话时文字实时长进输入框，再按一下结束</div>
            </div>
          </button>
          <button
            v-if="sending"
            class="send-btn stop"
            title="停止 (Esc)"
            @click="cancel"
          >
            <Square :size="14" :stroke-width="2" fill="currentColor" />
          </button>
          <button
            v-else
            class="send-btn"
            title="发送 (Enter)"
            :disabled="!input.trim() && !attachments.length"
            @click="send()"
          >
            <ArrowRight :size="16" :stroke-width="2" />
          </button>
        </div>
      </div>
    </div>

    <!-- 底部授权栏 -->
    <div class="auth-bar">
      <!-- 今日建议（每日任务）：底部留一枚小胶囊随时重开；正文是居中大弹窗(懒加载) -->
      <BriefingCenter :send-options="(cid: string) => sendOptions(cid)" />
      <div class="perm-wrap" style="margin-right: 48px;">
        <button
          class="auth-btn"
          :class="{ deny: permMode === 'deny' }"
          @click="showPermDropdown = !showPermDropdown"
        >
          <Hand
            v-if="permMode !== 'deny'"
            :size="13"
            :stroke-width="1.6"
            class="auth-hand"
          />
          <span v-else class="auth-deny">⊘</span>
          <span class="auth-label">{{ permLabel[permMode] }}</span>
          <ChevronDown :size="12" :stroke-width="2" />
        </button>
        <div v-if="showPermDropdown" class="dropdown">
          <div
            v-for="m in [
              { k: 'manual', l: '手动授权', d: '每次工具调用前确认' },
              {
                k: 'auto_current',
                l: '自动 · 仅当前会话',
                d: '本会话放行非高危操作',
              },
              {
                k: 'auto_all',
                l: '自动 · 完全放行',
                d: '全部工具免确认,体验等同终端 CLI(脚本/联网/执行不设限)',
              },
              {
                k: 'deny',
                l: '拒绝授权(只读)',
                d: '禁止写入/执行,只允许 Read/Grep/Glob',
              },
            ]"
            :key="m.k"
            class="perm-row"
            :class="{
              active: permMode === m.k,
              deny: m.k === 'deny',
            }"
            @click="pickPerm(m.k as PermissionMode)"
          >
            <div class="title">{{ m.l }}</div>
            <div class="desc">{{ m.d }}</div>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* ─────────── 输入区域 ─────────── */
/* 输入区悬浮在消息流上方（苹果 Liquid Glass 范式）：
   消息滚动时从玻璃卡下方穿过，透明感才真正可见。
   容器自身不挡点击，只有卡片/按钮等子元素可交互 */
.input-area {
  position: absolute;
  left: 0;
  right: 0;
  bottom: 0;
  z-index: 12;
  padding: 12px 32px 16px;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  pointer-events: none;
}
.input-area > * {
  pointer-events: auto;
}

/* 技能选择弹窗 */
.skill-panel {
  position: absolute;
  bottom: calc(100% - 8px);
  left: 32px;
  width: 360px;
  max-height: 420px;
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: 12px;
  box-shadow: var(--shadow-lg);
  z-index: 30;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
.skill-panel-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px 14px 8px;
  border-bottom: 1px solid var(--border-soft);
}
.skill-panel-title {
  font-size: 14px;
  font-weight: 600;
  color: var(--text);
}
.skill-panel-close {
  width: 24px;
  height: 24px;
  border: none;
  background: transparent;
  color: var(--muted);
  border-radius: 4px;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
}
.skill-panel-close:hover {
  background: var(--bg-soft);
  color: var(--text);
}
.skill-panel-search {
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 10px 14px;
  padding: 6px 10px;
  background: var(--bg-soft);
  border: 1px solid var(--border-soft);
  border-radius: 6px;
}
.sp-search-icon {
  color: var(--muted);
  flex-shrink: 0;
}
.skill-panel-search input {
  border: none;
  outline: none;
  background: transparent;
  font-size: 12.5px;
  color: var(--text);
  width: 100%;
}
.skill-panel-search input::placeholder {
  color: var(--dim);
}
.skill-panel-list {
  flex: 1;
  overflow-y: auto;
  padding: 0 6px;
}
.skill-panel-item {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  padding: 8px 10px;
  border-radius: 6px;
  cursor: pointer;
}
.skill-panel-item:hover {
  background: var(--bg-soft);
}
.skill-panel-item.active {
  background: var(--primary-soft);
}
.sp-item-icon {
  color: var(--primary);
  margin-top: 1px;
  flex-shrink: 0;
}
.sp-item-info {
  flex: 1;
  min-width: 0;
}
.sp-item-name {
  font-size: 13px;
  font-weight: 500;
  color: var(--text);
}
.sp-item-desc {
  font-size: 11px;
  color: var(--muted);
  margin-top: 2px;
  line-height: 1.4;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
}
.skill-panel-foot {
  padding: 8px 14px;
  border-top: 1px solid var(--border-soft);
}
.sp-manage {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 6px 12px;
  background: transparent;
  border: none;
  color: var(--primary);
  font-size: 12.5px;
  border-radius: 4px;
  cursor: pointer;
}
.sp-manage:hover {
  background: var(--primary-soft);
}

/* 「模式」合并键弹窗 + 角标 */
.mode-badge {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 15px;
  height: 15px;
  padding: 0 4px;
  margin-left: 1px;
  font-size: 10px;
  font-weight: 700;
  line-height: 1;
  border-radius: 999px;
  background: var(--primary);
  color: #fff;
}
.mode-panel {
  position: absolute;
  bottom: calc(100% - 8px);
  left: 32px;
  width: 360px;
  max-height: 420px;
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: 12px;
  box-shadow: var(--shadow-lg);
  z-index: 30;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
.mode-list {
  padding: 6px;
  overflow-y: auto;
}
/* API/模型切换器 */
.prov-caret {
  color: var(--muted);
  transition: transform 0.18s ease;
}
.prov-caret.flip {
  transform: rotate(180deg);
}
.provider-panel {
  width: 320px;
}
.prov-hint {
  padding: 8px 14px 2px;
  font-size: 11px;
  color: var(--dim);
}
.prov-add {
  margin: 2px 8px 8px;
  padding: 9px;
  border: 1px dashed var(--border-strong);
  border-radius: 8px;
  background: transparent;
  color: var(--muted);
  font-size: 12px;
  cursor: pointer;
  transition: border-color 0.12s ease, color 0.12s ease, background 0.12s ease;
}
.prov-add:hover {
  border-color: var(--primary);
  color: var(--primary);
  background: var(--primary-soft);
}
.mode-row {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  width: 100%;
  padding: 10px;
  border: none;
  background: transparent;
  border-radius: 8px;
  text-align: left;
  cursor: pointer;
}
.mode-row:hover {
  background: var(--bg-soft);
}
.mode-row.on {
  background: var(--primary-soft);
}
.mr-ic {
  color: var(--muted);
  margin-top: 1px;
  flex-shrink: 0;
}
.mode-row.on .mr-ic {
  color: var(--primary);
}
.mr-tx {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.mr-nm {
  font-size: 13px;
  font-weight: 500;
  color: var(--text);
}
.mr-ds {
  font-size: 11px;
  color: var(--muted);
  line-height: 1.45;
}
.mr-sw {
  position: relative;
  width: 30px;
  height: 17px;
  flex-shrink: 0;
  margin-top: 2px;
  border-radius: 999px;
  background: var(--border);
  transition: background 0.15s ease;
}
.mr-sw::after {
  content: "";
  position: absolute;
  top: 2px;
  left: 2px;
  width: 13px;
  height: 13px;
  border-radius: 50%;
  background: #fff;
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.25);
  transition: transform 0.15s ease;
}
.mr-sw.on {
  background: var(--primary);
}
.mr-sw.on::after {
  transform: translateX(13px);
}

/* 专家模式分隔线 */
.mode-sep {
  text-align: center;
  font-size: 11px;
  color: var(--muted);
  padding: 4px 8px;
  letter-spacing: 0.5px;
  opacity: 0.7;
}
.mode-row.agent-mode { gap: 8px; }

/* 「智能体」互斥切换器 */
.agent-panel { left: auto; right: 8px; width: 320px; }
.mode-row.exclusive { align-items: center; }
/* 工作模式时给模式键一抹冷色, 与快速(暖金/默认)区分, 一眼可辨当前预设 */
.work-mode-btn.work:not(.active) {
  color: #2563eb;
}
html[data-theme="dark"] .work-mode-btn.work:not(.active),
html[data-theme="aurora-dark"] .work-mode-btn.work:not(.active) {
  color: #7aa2ff;
}
.mr-default {
  display: inline-block;
  margin-left: 6px;
  font-size: 9.5px;
  font-weight: 700;
  color: var(--btn-solid-text);
  background: var(--primary);
  border-radius: 999px;
  padding: 0 6px;
  vertical-align: middle;
}
.mr-radio {
  position: relative;
  width: 16px;
  height: 16px;
  flex-shrink: 0;
  border-radius: 50%;
  border: 1.6px solid var(--border);
  transition: border-color 0.15s ease;
}
.mr-radio.on {
  border-color: var(--primary);
}
.mr-radio.on::after {
  content: "";
  position: absolute;
  inset: 3px;
  border-radius: 50%;
  background: var(--primary);
}
.agent-panel-foot {
  font-size: 11px;
  color: var(--muted);
  line-height: 1.5;
  padding: 8px 10px 4px;
  border-top: 1px solid var(--border-soft);
  margin-top: 4px;
}
.toolbar-btn.agent-toggle.active {
  color: var(--primary);
}

/* 召唤专家：最近召唤 + 召唤其它专家（仿 WorkBuddy 二级菜单） */
.summon-sec {
  margin-top: 4px;
  padding-top: 6px;
  border-top: 1px solid var(--border-soft);
}
.summon-head {
  font-size: 11px;
  color: var(--muted);
  padding: 4px 10px 6px;
  letter-spacing: 0.3px;
}
.summon-list {
  display: flex;
  flex-direction: column;
  gap: 2px;
  max-height: 230px;
  overflow-y: auto;
}
.summon-row {
  display: flex;
  align-items: center;
  gap: 9px;
  width: 100%;
  padding: 7px 10px;
  border: none;
  background: transparent;
  border-radius: 8px;
  text-align: left;
  cursor: pointer;
  transition: background 0.14s;
}
.summon-row:hover {
  background: var(--bg-soft);
}
.summon-row.on {
  background: var(--primary-soft);
}
.summon-av {
  width: 26px;
  height: 26px;
  border-radius: 50%;
  object-fit: cover;
  flex-shrink: 0;
}
.summon-ic {
  width: 26px;
  height: 26px;
  flex-shrink: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 15px;
  border-radius: 50%;
  background: var(--bg-soft);
}
.summon-tx {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 1px;
}
.summon-nm {
  font-size: 12.5px;
  font-weight: 500;
  color: var(--text);
  display: flex;
  align-items: center;
  gap: 6px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.summon-kind {
  font-size: 9.5px;
  font-weight: 600;
  color: var(--muted);
  border: 1px solid var(--border);
  border-radius: 4px;
  padding: 0 4px;
  flex-shrink: 0;
}
.summon-row.on .summon-kind {
  color: var(--primary);
  border-color: var(--primary);
}
.summon-ds {
  font-size: 10.5px;
  color: var(--muted);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.summon-check {
  color: var(--primary);
  flex-shrink: 0;
}
.summon-empty {
  font-size: 11px;
  color: var(--muted);
  padding: 6px 10px 8px;
  line-height: 1.5;
}
.summon-more {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  margin-top: 2px;
  padding: 9px 10px;
  border: none;
  background: transparent;
  border-radius: 8px;
  color: var(--primary);
  font-size: 12.5px;
  font-weight: 600;
  cursor: pointer;
  transition: background 0.14s;
}
.summon-more:hover {
  background: var(--primary-soft);
}
.summon-more .sm-ic {
  display: flex;
  flex-shrink: 0;
}
.summon-more .sm-arrow {
  margin-left: auto;
  opacity: 0.7;
}

/* 面板内内联挑选：业务团 / 专家 列表 */
.roster-picker {
  margin: 2px 4px 6px 34px;
  padding: 4px;
  border-left: 2px solid var(--border-soft);
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.roster-search {
  width: 100%;
  box-sizing: border-box;
  margin: 2px 0 4px;
  padding: 6px 9px;
  font-size: 12px;
  color: var(--text);
  background: var(--bg-soft);
  border: 1px solid var(--border);
  border-radius: 7px;
  outline: none;
}
.roster-search:focus {
  border-color: var(--primary);
}
.roster-scroll {
  max-height: 196px;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.roster-row {
  display: flex;
  align-items: center;
  gap: 9px;
  width: 100%;
  padding: 7px 9px;
  border: none;
  background: transparent;
  border-radius: 7px;
  text-align: left;
  cursor: pointer;
}
.roster-row:hover {
  background: var(--bg-soft);
}
.roster-row.on {
  background: var(--primary-soft);
}
.roster-ic {
  flex-shrink: 0;
  font-size: 15px;
  line-height: 1;
  width: 18px;
  text-align: center;
}
.roster-tx {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 1px;
}
.roster-nm {
  font-size: 12.5px;
  font-weight: 500;
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.roster-ds {
  font-size: 10.5px;
  color: var(--muted);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.roster-check {
  flex-shrink: 0;
  color: var(--primary);
  font-size: 13px;
  font-weight: 700;
}
.roster-empty {
  font-size: 11.5px;
  color: var(--muted);
  padding: 8px 9px;
}

/* 输入卡片 —— 宽度仿豆包（输入多了高度自动撑大）；
   形态仿 Codex 圆润边框 + 苹果 Liquid Glass 透明琉璃：
   半透明渐变面 + 大半径背景模糊（消息从卡下穿过时透出朦胧色），
   鼠标进入边框以暖金调亮起，聚焦再亮一档（只变色，不位移） */
.input-card {
  width: 100%;
  max-width: 1394px;
  background: linear-gradient(
    180deg,
    rgba(255, 255, 255, 0.72),
    rgba(252, 251, 246, 0.52)
  );
  backdrop-filter: blur(24px) saturate(1.6);
  border: 1px solid rgba(190, 182, 162, 0.5);
  border-radius: 22px;
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.85),
    inset 0 -1px 0 rgba(255, 255, 255, 0.25), 0 8px 32px rgba(120, 100, 60, 0.1);
  padding: 16px 20px;
  transition: border-color 0.2s ease, box-shadow 0.2s ease;
}
.input-card:hover {
  border-color: rgba(167, 140, 79, 0.85);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.9),
    inset 0 -1px 0 rgba(255, 255, 255, 0.25),
    0 0 0 1px rgba(167, 140, 79, 0.2), 0 8px 32px rgba(120, 100, 60, 0.14);
}
.input-card:focus-within {
  border-color: rgba(151, 122, 60, 1);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.9),
    inset 0 -1px 0 rgba(255, 255, 255, 0.25),
    0 0 0 1px rgba(167, 140, 79, 0.32), 0 10px 36px rgba(120, 100, 60, 0.2);
}
textarea {
  width: 100%;
  border: none;
  outline: none;
  resize: none;
  font-size: 14.5px;
  background: transparent;
  color: var(--text);
  padding: 4px 2px;
  line-height: 1.75;
  /* 高度随内容自动增长（JS 控制），最多到上限后内部滚动 */
  min-height: 60px;
  max-height: 300px;
  overflow-y: auto;
}

/* 工具栏 */
.toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  margin-top: 8px;
  padding-top: 8px;
  border-top: 1px solid var(--border-soft);
}
.toolbar-left {
  display: flex;
  align-items: center;
  gap: 6px;
}
.toolbar-btn {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 5px 10px;
  border-radius: 6px;
  font-size: 12px;
  color: var(--text-2);
  border: none;
  background: transparent;
  cursor: pointer;
  position: relative;
}
.toolbar-btn:hover {
  background: var(--bg-soft);
  color: var(--text);
}
.toolbar-btn.active {
  background: var(--primary-soft);
  color: var(--primary);
}
/* Tooltip — 放在按钮下方，避免顶部穿模 */
.btn-tooltip {
  position: absolute;
  top: calc(100% + 6px);
  left: 50%;
  transform: translateX(-50%);
  z-index: 25;
  opacity: 0;
  pointer-events: none;
  transition: opacity 0.15s;
}
.toolbar-btn:hover .btn-tooltip {
  opacity: 1;
}
.btn-tooltip-inner {
  background: var(--ink);
  color: #fafaf7;
  padding: 8px 12px;
  border-radius: 8px;
  font-size: 12px;
  white-space: nowrap;
  line-height: 1.5;
}
.btn-tooltip-sub {
  font-size: 11px;
  color: var(--dim);
}

/* Skill 标签 — 蓝色链接样式 */
.skill-tags {
  display: flex;
  gap: 12px;
  margin-bottom: 8px;
  padding: 0 2px;
  flex-wrap: wrap;
}
.skill-tag {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 12.5px;
  color: var(--primary);
  cursor: pointer;
  transition: opacity 0.15s;
}
.skill-tag:hover {
  opacity: 0.7;
  text-decoration: underline;
}
.tag-close {
  opacity: 0.5;
  width: 12px;
  height: 12px;
}

/* 目标模式激活时，输入卡片描边提示「这一框内容即完成条件」 */
.input-card.goal-on {
  border-color: var(--primary);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.85),
    0 0 0 1px var(--primary-soft), 0 8px 32px rgba(120, 100, 60, 0.1);
}

/* ───── 黑夜模式（深空玻璃）下的覆盖：暖白玻璃 → 深空玻璃，暖金 → 流光金 ───── */
html[data-theme="dark"] .input-card {
  /* 黑炭风格：实底近纯黑（≈ #0e0e0e，明显比主区 #181818 更黑），扁平不浮，
     读起来就是一块黑炭面 */
  background: linear-gradient(
    180deg,
    rgba(17, 17, 17, 1),
    rgba(10, 10, 10, 1)
  );
  border-color: rgba(255, 255, 255, 0.07);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.04),
    inset 0 -1px 0 rgba(255, 255, 255, 0.02), 0 8px 32px rgba(0, 0, 0, 0.4);
}
html[data-theme="dark"] .input-card:hover {
  border-color: rgba(212, 176, 106, 0.45);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.08),
    0 0 0 1px rgba(212, 176, 106, 0.1), 0 8px 32px rgba(0, 0, 0, 0.45);
}
html[data-theme="dark"] .input-card:focus-within {
  border-color: rgba(212, 176, 106, 0.7);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.08),
    0 0 0 1px rgba(212, 176, 106, 0.18), 0 10px 36px rgba(0, 0, 0, 0.5);
}
html[data-theme="dark"] .input-card.goal-on {
  border-color: var(--primary);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.07),
    0 0 0 1px var(--primary-soft), 0 8px 32px rgba(0, 0, 0, 0.45);
}
/* 深色下 --ink 变浅色：发送键/工具提示的反色文字需跟着翻转 */
html[data-theme="dark"] .send-btn {
  color: #1a1a1a;
}
html[data-theme="dark"] .send-btn:hover {
  color: #fff;
}
html[data-theme="dark"] .send-btn:disabled {
  color: var(--dim);
}
html[data-theme="dark"] .btn-tooltip-inner {
  background: #2a2a29;
  border: 1px solid rgba(255, 255, 255, 0.1);
}

.toolbar-right {
  display: flex;
  align-items: center;
  gap: 6px;
}
.send-btn {
  width: 32px;
  height: 32px;
  background: var(--ink);
  color: #fafaf7;
  border: none;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  transition: background 0.18s, transform 0.22s var(--ease-spring),
    box-shadow 0.22s var(--ease-out);
}
.send-btn:hover {
  background: var(--primary);
  transform: scale(1.06);
  box-shadow: var(--shadow);
}
.send-btn:not(:disabled):active {
  transform: scale(0.9);
  transition-duration: 0.05s;
}
.send-btn:disabled {
  background: var(--border);
  cursor: not-allowed;
}
.send-btn.stop {
  background: var(--vermilion);
}

/* 清空上下文（麦克风左侧的橡皮擦）：外观与 mic-btn 同族 */
.clear-ctx-btn {
  width: 32px;
  height: 32px;
  background: transparent;
  color: var(--text-2);
  border: 1px solid var(--border-soft);
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  transition: color 0.15s, border-color 0.15s, background 0.15s;
}
.clear-ctx-btn:hover:not(:disabled) {
  color: var(--vermilion);
  border-color: var(--vermilion);
  background: var(--vermilion-soft, rgba(220, 80, 50, 0.08));
}
.clear-ctx-btn:disabled {
  opacity: 0.5;
  cursor: default;
}

/* ─────────── 语音听写麦克风（发送键左侧 · 仿豆包/Codex）─────────── */
.mic-btn {
  position: relative;
  width: 32px;
  height: 32px;
  background: transparent;
  color: var(--text-2);
  border: 1px solid var(--border-soft);
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  transition: color 0.15s, border-color 0.15s, background 0.15s;
}
.mic-btn:hover {
  color: var(--ink);
  border-color: var(--border);
  background: var(--hover-soft, rgba(0, 0, 0, 0.04));
}
.mic-btn.live {
  background: var(--vermilion);
  border-color: var(--vermilion);
  color: #fff;
}
/* 浏览器路径：停录后上传+识别中，金色脉冲提示「在干活」 */
.mic-btn.busy {
  color: var(--gold, #d4b06a);
  border-color: var(--gold, #d4b06a);
  cursor: progress;
}
.mic-btn.busy .mic-ping {
  border-color: var(--gold, #d4b06a);
}
/* 录音中：外扩呼吸光环 */
.mic-ping {
  position: absolute;
  inset: -1px;
  border-radius: 50%;
  border: 2px solid var(--vermilion);
  animation: mic-ping 1.3s cubic-bezier(0, 0, 0.2, 1) infinite;
  pointer-events: none;
}
@keyframes mic-ping {
  0% {
    transform: scale(1);
    opacity: 0.7;
  }
  100% {
    transform: scale(1.8);
    opacity: 0;
  }
}
.mic-tip {
  position: absolute;
  bottom: calc(100% + 8px);
  right: 0;
  z-index: 25;
  opacity: 0;
  pointer-events: none;
  transition: opacity 0.15s;
  background: var(--ink);
  color: #fafaf7;
  padding: 7px 11px;
  border-radius: 8px;
  font-size: 12px;
  white-space: nowrap;
  line-height: 1.5;
}
.mic-tip b {
  color: var(--gold, #d4b06a);
}
.mic-tip-sub {
  font-size: 11px;
  color: var(--dim);
}
.mic-btn:hover .mic-tip {
  opacity: 1;
}

/* ─────────── 底部授权栏 ─────────── */
.auth-bar {
  width: 100%;
  max-width: 1394px;
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 8px;
}
.perm-wrap {
  position: relative;
}
.auth-btn {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 4px 10px;
  border-radius: 6px;
  font-size: 12px;
  color: var(--text-2);
  border: 1px solid var(--border-soft);
  background: transparent;
  cursor: pointer;
}
.auth-btn:hover {
  border-color: var(--border);
  color: var(--text);
}
.auth-btn.deny {
  color: var(--vermilion);
  border-color: rgba(192, 57, 43, 0.2);
}
/* 授权手图标：跟随按钮文字色（浅色=近黑墨色，深色=浅灰），不再用金黄 */
.auth-hand {
  color: currentColor;
  opacity: 0.9;
  flex-shrink: 0;
}
.auth-deny {
  color: var(--vermilion);
}
.auth-label {
  margin-right: 2px;
}

/* 授权下拉菜单 — 向上展开 */
.dropdown {
  position: absolute;
  right: 0;
  bottom: calc(100% + 6px);
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: 8px;
  box-shadow: var(--shadow-lg);
  width: 280px;
  padding: 6px;
  z-index: 20;
}
.perm-row {
  padding: 8px 10px;
  border-radius: 6px;
  cursor: pointer;
}
.perm-row:hover {
  background: var(--bg-soft);
}
.perm-row.active {
  background: var(--primary-soft);
}
.perm-row.deny .title {
  color: var(--vermilion);
}
.perm-row .title {
  font-size: 13px;
  color: var(--text);
  font-weight: 600;
}
.perm-row .desc {
  font-size: 11.5px;
  color: var(--muted);
  margin-top: 2px;
  line-height: 1.5;
}

/* ─────────── 附件 chips（基础样式与 ChatPanel 消息气泡内的只读 chips 各留一份） ─────────── */
.attach-chips {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin-bottom: 8px;
}
.attach-chip {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  max-width: 260px;
  padding: 4px 8px;
  background: var(--bg-soft);
  border: 1px solid var(--border);
  border-radius: 8px;
  font-size: 12px;
  color: var(--text-2);
}
.attach-chip .ac-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-weight: 500;
  color: var(--text);
}
.attach-chip .ac-size {
  color: var(--dim);
  font-size: 11px;
  flex-shrink: 0;
}
.attach-chip.pending {
  color: var(--muted);
}
.ac-remove {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 16px;
  height: 16px;
  border: none;
  background: transparent;
  color: var(--muted);
  border-radius: 4px;
  cursor: pointer;
  flex-shrink: 0;
}
.ac-remove:hover {
  background: var(--border);
  color: var(--text);
}
</style>
