<script setup lang="ts">
import { ref, computed, onMounted, onBeforeUnmount, nextTick, watch } from "vue";
import ExpertTeamStudio from "./ExpertTeamStudio.vue";
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
  File as FileIcon,
  FolderOpen,
  ExternalLink,
  Paperclip,
  Target,
  Ellipsis,
  PencilLine,
  Pin,
  PinOff,
  Copy,
  Trash2,
  Check,
  Workflow,
  PanelRightOpen,
  PanelRightClose,
  BookOpen,
  Layers,
  Hand,
  RotateCcw,
  Mic,
  SlidersHorizontal,
  Rocket,
  Flag,
  FolderTree,
  RefreshCw,
  Zap,
  Code2,
  Eraser,
} from "@lucide/vue";
import SearchGlass from "./icons/SearchGlass.vue";
import OrbitSpinner from "./icons/OrbitSpinner.vue";
import {
  chat,
  convApi,
  skills as skillsApi,
  expert,
  files as fc,
  avatarSlot,
  invoke,
  listen,
  isTauri,
  uploadToBackend,
  type PermissionMode,
  type Skill,
  type AttachedFile,
  type Message,
  type ExpertAgentStatus,
  type ExpertCard,
  type ExpertTeam as ExpertTeamCard,
  type SuggestedFlow,
} from "../tauri";
import { WebVoiceRecorder } from "../lib/webVoice";
import { renderMarkdown, mdVersion } from "../lib/markdown";
import { toast } from "../composables/useToast";
import { humanizeError } from "../lib/humanizeError";
import { useAppStore } from "../stores/app";
import { useSkillsStore } from "../stores/skills";
import { useArtifactsStore } from "../stores/artifacts";
import { useChatStore, type Bubble } from "../stores/chat";
import { useProvidersStore } from "../stores/providers";
import { useWorkflowsStore } from "../stores/workflows";
import { useLongTaskStore, detectLongTask } from "../stores/longtask";
import { useFileDrop } from "../composables/useFileDrop";
import { isLowSpec } from "../composables/useLowSpec";

function fileName(path: string): string {
  return path.replace(/\/+$/, "").split("/").pop() || path;
}

function fileExt(path: string): string {
  const n = fileName(path);
  const i = n.lastIndexOf(".");
  return i >= 0 ? n.slice(i + 1).toLowerCase() : "";
}

/** 尾随 `/` = 后端归并上报的「应用文件夹」产物（整个应用一个 chip） */
function isFolderArtifact(path: string): boolean {
  return path.endsWith("/");
}

function artifactIcon(path: string) {
  if (isFolderArtifact(path)) return FolderOpen;
  const ext = fileExt(path);
  if (["html", "htm", "svg", "js", "ts", "css", "json", "xml"].includes(ext))
    return FileCode;
  if (["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "avif"].includes(ext))
    return ImageIcon;
  if (["mp4", "mov", "webm", "mkv", "avi"].includes(ext)) return Clapperboard;
  if (["mp3", "wav", "m4a", "aac", "flac", "ogg"].includes(ext))
    return AudioLines;
  if (["csv", "tsv", "xlsx", "xls"].includes(ext)) return Table;
  if (["md", "markdown", "txt", "pdf"].includes(ext)) return FileText;
  return FileIcon;
}

const app = useAppStore();
const skillsStore = useSkillsStore();
const artifactsStore = useArtifactsStore();
const chatStore = useChatStore();
const workflowsStore = useWorkflowsStore();
const longTaskStore = useLongTaskStore();

/** 点击成品文件 chip → 展开右侧抽屉并预览；应用文件夹 → 直接在文件管理器打开 */
function openArtifact(path: string) {
  if (isFolderArtifact(path)) {
    artifactsStore.openFolder(path);
    return;
  }
  app.drawerCollapsed = false;
  artifactsStore.open(path);
}

/** 豆包式「参考文件」: 本回合 Read 过的文件, 去重、剔除本回合产物与被截断的摘要,
 *  收在回答最前面供点开预览(走 openArtifact 同一条右侧抽屉链路) */
function refFiles(t: Turn): string[] {
  const arts = new Set(t.artifacts);
  const seen = new Set<string>();
  const out: string[] = [];
  for (const tl of t.tools) {
    if (tl.name !== "Read") continue;
    for (const d of tl.details) {
      const p = d.trim().replace(/\\/g, "/");
      // 摘要被截断(尾随 …)或不像路径的跳过, 宁缺勿错
      if (!p || p.endsWith("…") || !p.includes("/")) continue;
      if (seen.has(p) || arts.has(p)) continue;
      seen.add(p);
      out.push(p);
    }
  }
  return out.slice(0, 8);
}

// 产物文件夹卡片的折叠态(按回合 key, 默认展开; 只在会话内存活, 不持久化)
const filesCollapsed = ref<Record<number, boolean>>({});
function toggleFiles(k: number) {
  filesCollapsed.value = { ...filesCollapsed.value, [k]: !filesCollapsed.value[k] };
}

const input = ref("");
// 每个对话各自的未发送草稿:切走/切回都保留本对话的草稿,且绝不把 A 的半句话
// 带进 B(全局单 ref 会串台、还可能误发到别的对话)。键用 convId,新对话(null)用 ""。
const drafts = new Map<string, string>();
// 多开：当前对话的气泡 / 运行态来自 chat store（按对话 id 维护，切走不丢、后台续流）
const bubbles = computed(() => chatStore.bubblesFor(app.currentConvId));
const sending = computed(() => chatStore.isSending(app.currentConvId));

// 当前项目是否为默认赠送的「毛主席」项目 —— 决定空状态彩蛋（与后端 MAO_PROJECT_NAME 一致）
const currentProjectName = computed(
  () => app.projects.find((p) => p.id === app.currentProjectId)?.name || ""
);
const isMaoProject = computed(() => currentProjectName.value === "毛主席");

// ─────────── 回复渲染：统一 markdown 管线(lib/markdown) ───────────
// 已完成回合按原文命中缓存(流式期间不再全量重算);shiki/KaTeX 异步增强,
// 完成后 mdVersion 变化触发重读缓存。流式中的活跃回合传 enhance=false 省 CPU。
const ANSI_RE = /\x1b\[[0-9;?]*[ -/]*[@-~]/g;
// 系统提示词约定长回答第一行写 `TL;DR: 一句话结论`(见后端 reply_style_directive),
// 这里把它从正文里摘出来渲染成置顶速览卡, 正文从第二段起正常走 markdown 管线。
const TLDR_RE = /^\s*(?:>\s*)?(?:\*\*)?\s*TL;?\s?DR\s*(?:\*\*)?\s*[::]\s*(.+?)\s*$/i;
function renderMd(text: string, enhance = true): string {
  void mdVersion.value; // 注册响应式依赖:增强完成后刷新
  const clean = (text || "").replace(ANSI_RE, "");
  const nl = clean.indexOf("\n");
  const firstLine = nl >= 0 ? clean.slice(0, nl) : clean;
  const m = firstLine.match(TLDR_RE);
  if (m) {
    const rest = nl >= 0 ? clean.slice(nl + 1).replace(/^\s*\n/, "") : "";
    return (
      `<div class="tldr"><span class="tldr-tag">TL;DR</span><div class="tldr-body">` +
      renderMarkdown(m[1], { enhance }) +
      `</div></div>` +
      (rest ? renderMarkdown(rest, { enhance }) : "")
    );
  }
  return renderMarkdown(clean, { enhance });
}

// 工具名 → 友好中文（对话里以优雅 pill 呈现，不再是终端灰块）
const TOOL_LABELS: Record<string, string> = {
  Bash: "运行命令",
  Read: "读取文件",
  Write: "写入文件",
  Edit: "编辑文件",
  MultiEdit: "批量编辑",
  NotebookEdit: "编辑笔记本",
  Glob: "查找文件",
  Grep: "搜索内容",
  WebSearch: "联网搜索",
  WebFetch: "抓取网页",
  Task: "子任务",
  TodoWrite: "更新清单",
};
function toolLabel(n: string): string {
  return TOOL_LABELS[n] ?? n;
}

// 一个「回合」= 一条用户消息 + 其后的助手正文/工具/产物，直到下一条用户消息。
// 助手多段文本拼成一块 markdown；工具折叠成 pill；所有生成文件聚合到回合末尾。
interface TurnTool {
  name: string;
  /** 连续同名合并的次数 */
  count: number;
  /** 各次调用的输入摘要(命令/路径/检索词) */
  details: string[];
}
interface Turn {
  key: number;
  user?: Bubble;
  text: string;
  tools: TurnTool[];
  artifacts: string[];
  errors: string[];
  hasAssistant: boolean;
  /** 回合时间(用户消息时刻,无则首条气泡时刻) */
  at?: number;
}
const ERR_RE = /^\[(错误|发送失败|result error)/;
/** 把一段气泡切片构建成回合模型(原 renderTurns 主体原样提炼,key 从 startKey 递增)。
 *  切片须在回合边界上:要么从头开始,要么以一条 user 气泡开头(user 恒开新回合)。 */
function buildTurnsSlice(list: Bubble[], startKey: number): Turn[] {
  const out: Turn[] = [];
  let cur: Turn | undefined;
  let k = startKey;
  // 当前回合已收录产物的去重集:产物只往「当前回合」追加,故单个随回合重置的 Set 即可。
  // 把原先 `artifacts.includes(a)` 的 O(N) 线性查改成 O(1) 命中,整轮去重从 O(N²) 降到 O(N) ——
  // 长对话 + 多产物时不再越聊越顿。
  let curArtSet = new Set<string>();
  const startTurn = (user?: Bubble): Turn => {
    const turn: Turn = {
      key: k++,
      user,
      text: "",
      tools: [],
      artifacts: [],
      errors: [],
      hasAssistant: false,
      at: user?.at,
    };
    out.push(turn);
    cur = turn;
    curArtSet = new Set<string>();
    return turn;
  };
  for (const b of list) {
    if (b.role === "user") {
      startTurn(b);
      continue;
    }
    const t: Turn = cur ?? startTurn(undefined);
    if (t.at === undefined && b.at !== undefined) t.at = b.at;
    if (b.role === "tool") {
      const name = b.tool || "工具";
      // 合并连续同名工具，避免刷屏;输入摘要逐条留底供展开查看
      const last = t.tools[t.tools.length - 1];
      if (last?.name === name) {
        last.count++;
        if (b.toolDetail) last.details.push(b.toolDetail);
      } else {
        t.tools.push({
          name,
          count: 1,
          details: b.toolDetail ? [b.toolDetail] : [],
        });
      }
    } else {
      const txt = b.text || "";
      if (ERR_RE.test(txt.trim())) {
        t.errors.push(txt);
      } else if (txt) {
        t.text += (t.text ? "\n\n" : "") + txt;
        t.hasAssistant = true;
      }
      if (b.artifacts) {
        for (const a of b.artifacts)
          if (!curArtSet.has(a)) {
            curArtSet.add(a);
            t.artifacts.push(a);
          }
      }
    }
  }
  return out;
}
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
}
let turnsPrefixCache: TurnsPrefixCache | null = null;
const renderTurns = computed<Turn[]>(() => {
  const list = bubbles.value;
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
  let hit = c !== null && c.sig.length === split;
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
    turnsPrefixCache = {
      sig: list
        .slice(0, split)
        .map((b) => ({ b, artLen: b.artifacts?.length ?? 0 })),
      turns: prefixTurns,
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

// ── 回合时间 / 本会话 token 估算 ──
function fmtTime(at?: number): string {
  if (!at) return "";
  const d = new Date(at);
  const today = new Date();
  const sameDay = d.toDateString() === today.toDateString();
  const hm = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  return sameDay ? hm : `${d.getMonth() + 1}/${d.getDate()} ${hm}`;
}

// ── 重新生成 / 编辑重发 ──
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
  await chatStore.send(convId, prompt, text || "（仅附件）", files, {
    permissionMode: permMode.value,
    skillIds: Array.from(skillsStore.enabledSkills),
    useKb: kbMode.value || undefined,
    agentMode: agentMode.value,
    workMode: workMode.value,
    providerId: providerForConv(convId),
  });
}
function editTurn(t: Turn) {
  if (!t.user?.text) return;
  input.value = t.user.text;
  nextTick(() => {
    inputEl.value?.focus();
    autoGrow();
  });
}

// 复制某一回合的回答正文（回答下方的「复制」按钮）
async function copyTurn(t: Turn) {
  if (!t.text) return;
  try {
    await navigator.clipboard.writeText(t.text);
    flashCopied("已复制回答");
  } catch {
    flashCopied("复制失败");
  }
}
const showPermDropdown = ref(false);
const permMode = ref<PermissionMode>("manual");
const showSkillPanel = ref(false);
const skillSearch = ref("");
const skillsList = ref<Skill[]>([]);
const scrollEl = ref<HTMLDivElement | null>(null);

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

// ── 首帧非关键加载推迟 ──
// ChatPanel 挂载时多个 onMounted 并发打出与首屏渲染无关的 IPC(晨报/技能清单/供应商/
// codex 状态),与首屏聊天区渲染抢主线程和后端。统一包进空闲回调:浏览器空闲(或到点
// 兜底)再执行;组件已卸载则放弃,防止延迟回调在卸载后注册监听器/改状态。
let disposed = false;
function runWhenIdle(fn: () => void) {
  const run = () => {
    if (!disposed) fn();
  };
  if (typeof (window as any).requestIdleCallback === "function") {
    (window as any).requestIdleCallback(run, { timeout: 600 });
  } else {
    setTimeout(run, 600);
  }
}

onMounted(async () => {
  window.addEventListener("keydown", onGlobalKeydown);
  // 从「专家团」页点「召唤」后会切回本视图(ChatPanel 重新挂载)→ 在此消费召唤意图。
  consumePendingSummon();
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
  disposed = true; // 让尚未执行的空闲回调作废
  window.removeEventListener("keydown", onGlobalKeydown);
  for (const u of voiceUnlisteners) u();
  stopAgentsPoll(); // 切走视图即停表,别把退避轮询定时器泄漏到卸载后
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

// ─────────── 每日晨报建议（回声层做梦产物）───────────
// 「让 AI 更懂你」：后台做梦据你新加入的内容产出工程化建议，展示在对话框顶部，
// 点「让我去做」= 把建议的 action 当 prompt 直接发起一轮对话。
interface Suggestion {
  id: string;
  title: string;
  // 类别:progress(新进展)/ wrapup(收尾)/ workflow(可复用流程)/ organize(整理)
  kind?: string;
  // 依据来源标签（某段对话 / 某份文件 / 某个老项目名）——「懂你」的落点
  source?: string;
  why: string;
  how: string;
  action: string;
}
// 类别 → 图标 / 文案 / 配色（data-kind 驱动 CSS）。让卡片一眼能分清「推进 / 收尾 / 固化流程 / 整理」。
const BRIEF_KINDS: Record<string, { icon: any; label: string }> = {
  progress: { icon: Rocket, label: "推进新进展" },
  wrapup: { icon: Flag, label: "收个尾" },
  workflow: { icon: Workflow, label: "固化为工作流" },
  organize: { icon: FolderTree, label: "整理资料" },
};
function briefKind(s: Suggestion) {
  return BRIEF_KINDS[s.kind || ""] || BRIEF_KINDS.progress;
}
const briefings = ref<Suggestion[]>([]);
// 今日建议改成「居中大弹窗」：briefOpen=true 时铺一层遮罩在屏幕正中展示。
const briefOpen = ref(false);
async function loadBriefings() {
  // 桌面(Tauri)与 Docker/Web(HTTP) 都要取:invoke 会自动按环境走原生 / HTTP /
  // 纯预览 stub(见 tauri.ts),无需在此处用 isTauri 把 Docker 一并误杀。
  try {
    briefings.value = (await invoke<Suggestion[]>("echo_briefing_today")) || [];
  } catch (e) {
    console.error("加载晨报失败", e);
  }
}
async function dismissBriefing(id: string) {
  try {
    briefings.value = await invoke<Suggestion[]>("echo_briefing_dismiss", { id });
    if (!briefings.value.length) briefOpen.value = false; // 全部处理完自动关
  } catch (e) {
    console.error("忽略建议失败", e);
  }
}
async function runBriefing(s: Suggestion) {
  const prompt = (s.action && s.action.trim()) || s.title;
  // 每条「今日建议」在各自独立的新对话里执行 —— 不挤占当前对话、彼此互不卡死。
  // 走 chat store 的 convId 多开 + App 级流式监听:连点几条会各自起一个对话，
  // 全在后台并行推进，切到别的界面也照常跑、回来仍能看到各自进度。
  let pid = app.currentProjectId;
  if (!pid) {
    await app.refreshProjects();
    pid = app.currentProjectId;
  }
  if (!pid) {
    const p = await app.createProject("默认项目");
    pid = p.id;
  }
  const c = await app.createConversation(pid); // 新建对话并切过去看它启动
  briefOpen.value = false; // 关掉弹窗，让用户看到这条建议在新对话里跑起来
  await dismissBriefing(s.id);
  await chatStore.send(c.id, prompt, s.title || prompt, undefined, {
    permissionMode: permMode.value,
    skillIds: Array.from(skillsStore.enabledSkills),
    useKb: kbMode.value || undefined,
    agentMode: agentMode.value,
    workMode: workMode.value,
    providerId: providerForConv(c.id),
  });
}
// ─────────── 空对话页的「下一步工作流」推荐(仿豆包的建议气泡)───────────
// 据用户真实知识库(主题/类型/语言 + 最近在动的文件夹)用大模型推几条「成体系的工作流」,
// 点一下把整条工作流提示词填进输入框(可改可发)。LLM 要数秒、要花 token,故按会话缓存,
// 只在首次进空白页时生成一次,顶部「换一批」可手动重算。
const workflowFlows = ref<SuggestedFlow[]>([]);
const flowsLoading = ref(false);
const flowsTried = ref(false); // 本会话已尝试过(无论成败),避免空白页反复触发 LLM
const FLOWS_CACHE_KEY = "polaris.flows.v1";
function readFlowsCache(): SuggestedFlow[] | null {
  try {
    const raw = sessionStorage.getItem(FLOWS_CACHE_KEY);
    if (!raw) return null;
    const arr = JSON.parse(raw);
    return Array.isArray(arr) && arr.length ? (arr as SuggestedFlow[]) : null;
  } catch {
    return null;
  }
}
async function loadWorkflowFlows(force = false) {
  if (flowsLoading.value) return;
  if (!force) {
    const cached = readFlowsCache();
    if (cached) {
      workflowFlows.value = cached;
      flowsTried.value = true;
      return;
    }
    if (flowsTried.value) return; // 本会话已试过且无结果 → 不再反复打扰
  }
  flowsLoading.value = true;
  try {
    const flows = await fc.suggestWorkflows(null);
    workflowFlows.value = flows || [];
    if (flows && flows.length) sessionStorage.setItem(FLOWS_CACHE_KEY, JSON.stringify(flows));
  } catch {
    // 库还空 / 模型不可用 → 安静留空,空白页只显示问候语,不报错打扰。
    workflowFlows.value = [];
  } finally {
    flowsLoading.value = false;
    flowsTried.value = true;
  }
}
// 点一条建议:把整条工作流提示词填进输入框并聚焦,让用户先看清(这些是成体系的长提示词)再发。
function applyFlow(f: SuggestedFlow) {
  input.value = f.prompt;
  nextTick(() => {
    autoGrow();
    inputEl.value?.focus();
  });
}
// 空白页(没有任何回合、且非毛主席彩蛋页)→ 拉一次工作流推荐;有缓存秒出,无缓存才走 LLM。
const showFlowSuggestions = computed(() => renderTurns.value.length === 0 && !isMaoProject.value);
watch(
  showFlowSuggestions,
  (empty) => {
    if (empty) loadWorkflowFlows();
  },
  { immediate: true },
);

onMounted(() => {
  // 晨报拉取 + 做梦监听都不影响首屏聊天区渲染 → 推迟到空闲帧,别与首帧 IPC 抢资源。
  // 弹窗因此最多晚 ~600ms,可接受。
  runWhenIdle(() => {
    void (async () => {
      await loadBriefings();
      if (disposed) return; // await 期间可能已卸载
      // 每次打开软件:只要有今日建议,就在屏幕正中自动弹出一次(本次启动仅弹一次,
      // 关掉后不再骚扰;底部胶囊随时可手动重开)。sessionStorage 随进程重启而清空,
      // 所以「下次打开软件」会再弹。
      if (briefings.value.length && !sessionStorage.getItem("polaris.brief.shown")) {
        briefOpen.value = true;
        sessionStorage.setItem("polaris.brief.shown", "1");
      }
      // 做梦/晨报生成完 → 刷新建议,并主动弹出让用户第一时间看到新内容。
      // 桌面走 Tauri 事件、Docker/Web 走 WS,两条路径的 listen 包装都直接回传 payload 本体
      // (见 tauri.ts),所以读 p.kind;旧代码读 p.payload.kind 多包一层、永远取不到。
      // 捕获 unlisten 并纳入 voiceUnlisteners(onBeforeUnmount 统一回收):此前未解绑,
      // KeepAlive 反复挂载会逐月累积上千个 echo:dream 监听器及其闭包 → 内存爬升。
      const un = await listen("echo:dream", async (p: any) => {
        if ((p?.kind ?? p?.payload?.kind) === "done") {
          await loadBriefings();
          if (briefings.value.length) briefOpen.value = true;
        }
      });
      if (disposed) un(); // 卸载后才注册完成:立刻解绑,别泄漏
      else voiceUnlisteners.push(un);
    })();
  });
});

// ─────────── 分批长任务（Batch Build）模式开关 ───────────
// 超长生成（如 60 页 PPT）强制走分批：先规划成清单，每轮只建一小批，断线从清单续跑，
// 避免单轮输出过长把流式连接拖死。关时也会按「N 页/张/章」启发式自动判定长任务。
const batchMode = ref(false);
function toggleBatch() {
  batchMode.value = !batchMode.value;
  if (batchMode.value) nextTick(() => inputEl.value?.focus());
}

// ─────────── 百人专家团模式 ──────────
// 单 agent / 单专家 / 专家团 / 智能匹配（默认），这四个是互斥的，只选一个
type AgentMode = "single-agent" | "single-expert" | "expert-team" | "auto-match";
const agentMode = ref<AgentMode>("auto-match");
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

// 消费「专家团」页投递来的召唤意图(ChatPanel 每次挂载即检查一次,消费后清空)。
function consumePendingSummon() {
  const p = app.pendingSummon;
  if (!p) return;
  app.pendingSummon = null;
  summon(p.kind, p.id);
}

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
    const before = JSON.stringify(teamAgentsStatus.value);
    await pollAgentsStatus();
    const after = JSON.stringify(teamAgentsStatus.value);
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
type WorkMode = "fast" | "work";
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

function attachIcon(kind: string) {
  if (kind === "image") return ImageIcon;
  if (kind === "pdf") return FileText;
  if (kind === "office") return Table;
  if (kind === "text") return FileCode;
  return FileIcon;
}

function humanSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

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

const { isOver: dropOver } = useFileDrop({
  active: () => app.view === "chat",
  onDrop: onDropFiles,
});

const permLabel: Record<PermissionMode, string> = {
  manual: "手动授权",
  auto_current: "自动 · 仅当前会话",
  auto_all: "自动 · 所有会话",
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
watch(
  () => app.currentConvId,
  async (cid, prev) => {
    // 草稿按对话隔离:先存上一对话的草稿,再载入新对话的草稿(没有则空)。
    drafts.set(prev ?? "", input.value);
    input.value = drafts.get(cid ?? "") ?? "";
    histIdx = -1; // 输入历史召回索引也跟着对话走,别串台
    nextTick(autoGrow); // 草稿可能多行,水合后重算高度
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
  // 若在别的视图点了工作流包「使用」才切来对话，挂载时补消费一次
  applyInsert(workflowsStore.insertRequest);
  // 技能清单 / 供应商清单 / codex 授权态都只服务于点开面板后的展示,
  // 不影响首屏聊天区渲染 → 推迟到空闲帧再打 IPC
  runWhenIdle(() => {
    void loadSkills();
    providersStore.refresh();
    providersStore.refreshCodex();
  });
});

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

function onKeydown(e: KeyboardEvent) {
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

// ─────────── 对话「更多」菜单（标题旁 ··· ） ───────────
// 当前对话对象（标题、置顶、复制、删除等操作的目标）
const currentConv = computed(() => {
  const list =
    app.conversationsByProject[app.currentProjectId || ""] || [];
  return list.find((c) => c.id === app.currentConvId) || null;
});

const showConvMenu = ref(false);
function toggleConvMenu() {
  showConvMenu.value = !showConvMenu.value;
}
function closeConvMenu() {
  showConvMenu.value = false;
}
// 点空白处关菜单（菜单与触发按钮内部点击都 .stop，不会误关）
onMounted(() => window.addEventListener("click", closeConvMenu));
onBeforeUnmount(() => window.removeEventListener("click", closeConvMenu));

// 复制反馈小提示（顶栏中央浮现 ~1.6s）
const copied = ref("");
let copiedTimer: ReturnType<typeof setTimeout> | undefined;
function flashCopied(msg: string) {
  copied.value = msg;
  if (copiedTimer) clearTimeout(copiedTimer);
  copiedTimer = setTimeout(() => (copied.value = ""), 1600);
}

// 重命名：标题就地变输入框，Enter 提交 / Esc 取消 / 失焦提交
const renaming = ref(false);
const renameText = ref("");
const renameInput = ref<HTMLInputElement | null>(null);
function openRename() {
  closeConvMenu();
  renameText.value = currentConv.value?.title ?? "";
  renaming.value = true;
  nextTick(() => {
    renameInput.value?.focus();
    renameInput.value?.select();
  });
}
async function commitRename() {
  if (!renaming.value) return;
  const conv = currentConv.value;
  renaming.value = false;
  if (conv) await app.renameConversation(conv, renameText.value);
}
function cancelRename() {
  renaming.value = false;
}

function togglePinCurrent() {
  closeConvMenu();
  if (app.currentConvId) app.togglePin(app.currentConvId);
}

async function copyConvId() {
  closeConvMenu();
  const id = app.currentConvId;
  if (!id) return;
  try {
    await navigator.clipboard.writeText(id);
    flashCopied("已复制会话 ID");
  } catch {
    flashCopied("复制失败");
  }
}

function conversationToMarkdown(title: string, msgs: Message[]): string {
  const lines: string[] = [`# ${title}`, ""];
  for (const msg of msgs) {
    if (msg.role === "tool") continue; // 工具调用噪声不进转写
    const who = msg.role === "user" ? "你" : "北极星";
    const body = (msg.content || "").trim();
    if (!body) continue;
    lines.push(`**${who}：**`, "", body, "");
  }
  return lines.join("\n").trim() + "\n";
}

async function copyAsMarkdown() {
  closeConvMenu();
  const conv = currentConv.value;
  if (!conv) return;
  try {
    const msgs = await convApi.getMessages(conv.id);
    await navigator.clipboard.writeText(
      conversationToMarkdown(conv.title, msgs)
    );
    flashCopied("已复制为 Markdown");
  } catch {
    flashCopied("复制失败");
  }
}

async function deleteCurrentConv() {
  closeConvMenu();
  const conv = currentConv.value;
  if (!conv) return;
  if (confirm(`删除对话「${conv.title}」？(消息也会被清空)`)) {
    await app.deleteConversation(conv);
  }
}
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
    <div class="chat-top">
      <div class="chat-title">
        <template v-if="app.currentConvId">
          <!-- 重命名：标题就地变输入框 -->
          <input
            v-if="renaming"
            ref="renameInput"
            v-model="renameText"
            class="t-rename"
            @keydown.enter.prevent="commitRename"
            @keydown.esc.prevent="cancelRename"
            @blur="commitRename"
            @click.stop
          />
          <template v-else>
            <Pin
              v-if="app.isPinned(app.currentConvId)"
              :size="12"
              :stroke-width="1.9"
              class="t-pin"
            />
            <span class="t-text">{{ currentConv?.title || "(对话)" }}</span>
          </template>

          <!-- 更多菜单 -->
          <div v-if="!renaming" class="conv-menu-wrap">
            <button
              class="conv-more"
              :class="{ active: showConvMenu }"
              title="更多"
              @click.stop="toggleConvMenu"
            >
              <Ellipsis :size="16" :stroke-width="2" />
            </button>
            <div v-if="showConvMenu" class="conv-menu" @click.stop>
              <button class="cm-item" @click="openRename">
                <PencilLine :size="14" :stroke-width="1.8" />
                <span>重命名对话</span>
              </button>
              <button class="cm-item" @click="togglePinCurrent">
                <component
                  :is="app.isPinned(app.currentConvId) ? PinOff : Pin"
                  :size="14"
                  :stroke-width="1.8"
                />
                <span>{{
                  app.isPinned(app.currentConvId) ? "取消置顶" : "置顶对话"
                }}</span>
              </button>
              <div class="cm-sep"></div>
              <button class="cm-item" @click="copyConvId">
                <Copy :size="14" :stroke-width="1.8" />
                <span>复制会话 ID</span>
              </button>
              <button class="cm-item" @click="copyAsMarkdown">
                <FileText :size="14" :stroke-width="1.8" />
                <span>复制为 Markdown</span>
              </button>
              <div class="cm-sep"></div>
              <button class="cm-item danger" @click="deleteCurrentConv">
                <Trash2 :size="14" :stroke-width="1.8" />
                <span>删除对话</span>
              </button>
              <div
                v-if="chatStore.inputTokens(app.currentConvId) > 0"
                class="cm-meta"
              >
                上轮注入 ≈
                {{ (chatStore.inputTokens(app.currentConvId) / 1000).toFixed(1) }}k
                tokens
              </div>
            </div>
          </div>
        </template>
        <template v-else>
          <span class="t-text muted">未选择对话</span>
        </template>
      </div>
      <Transition name="copy-fade">
        <div v-if="copied" class="copy-toast">
          <Check :size="13" :stroke-width="2.2" />
          <span>{{ copied }}</span>
        </div>
      </Transition>
      <button
        class="drawer-toggle"
        :title="app.drawerCollapsed ? '展开文件抽屉' : '收起文件抽屉'"
        @click="app.toggleDrawer()"
      >
        <component
          :is="app.drawerCollapsed ? PanelRightOpen : PanelRightClose"
          :size="17"
          :stroke-width="1.7"
        />
      </button>
    </div>

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
      <div v-else-if="renderTurns.length === 0" class="hero-wrap">
        <!-- 毛主席项目彩蛋：未对话前的空白中部 -->
        <template v-if="isMaoProject">
          <div class="mao-hero">小同志，你好。</div>
          <div class="mao-desc">
            这里是<strong>毛主席资料库</strong>。我已经把《毛泽东选集》《毛泽东全集》等
            资料装进了你本地的知识库 —— 你可以在「浏览」里随时翻看。有什么问题，尽管向我提；
            点对话框下的<strong>「请教毛主席」</strong>，我就用实事求是、矛盾分析的法子，
            给你客观地分析分析。
          </div>
          <div class="mao-slogan">为建设共产主义事业而奋斗</div>
        </template>
        <template v-else>
          <div class="hero">你说,北极星画</div>
          <!-- KB-first 的工作机制(沿双链取证/脚注溯源)是后台行为, 不在空对话页直接铺给用户;
               需要时挂在下面这行折叠摘要里, 默认收起。 -->
          <details class="hero-note">
            <summary>知识库优先 · 怎么工作的</summary>
            <div class="hero-sub">
              <strong>知识库优先</strong> · 先沿 <code>Read / Glob / Grep</code> 在 PolarisKB
              wiki 沿 <code>[[双链]]</code> 取证 · 命中标脚注来源 · 查不到才允许自由作答
            </div>
            <div class="hero-meta">
              <span class="hm-pill">📚 知识库写死优先</span>
              <span class="hm-pill">🔗 沿 <code>[[双链]]</code> 续读</span>
              <span class="hm-pill">📑 命中标脚注 <code>[^1]</code> 来源</span>
              <span class="hm-pill">⚠️ 查不到就标「资料不足」</span>
            </div>
          </details>

          <!-- 下一步工作流推荐(据你的知识库 + 最近在动的文件):点一条把整条工作流提示词填进输入框 -->
          <div v-if="flowsLoading || workflowFlows.length" class="flow-suggest">
            <div class="flow-head">
              <Sparkles :size="13" :stroke-width="1.8" />
              <span>据你最近的资料，下一步可以——</span>
              <button
                v-if="workflowFlows.length && !flowsLoading"
                class="flow-refresh"
                title="换一批建议"
                @click="loadWorkflowFlows(true)"
              >
                <RefreshCw :size="12" :stroke-width="2" /> 换一批
              </button>
            </div>
            <div v-if="flowsLoading && !workflowFlows.length" class="flow-chips">
              <span v-for="i in 4" :key="i" class="flow-chip skeleton"></span>
            </div>
            <div v-else class="flow-chips">
              <button
                v-for="(f, i) in workflowFlows"
                :key="i"
                class="flow-chip"
                :title="f.prompt"
                @click="applyFlow(f)"
              >
                {{ f.title }}
              </button>
            </div>
          </div>
        </template>
      </div>

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

      <div v-for="t in visibleTurns" :key="t.key" class="turn">
        <!-- 用户消息：右侧中性气泡，无头像 -->
        <div v-if="t.user" class="msg user">
          <button
            v-if="t.user.text && !sending"
            class="u-edit"
            title="编辑并重发"
            @click="editTurn(t)"
          >
            <PencilLine :size="13" :stroke-width="1.8" />
          </button>
          <div class="bubble-user">
            <div v-if="t.user.text" class="u-text">{{ t.user.text }}</div>
            <div
              v-if="t.user.files && t.user.files.length"
              class="attach-chips in-bubble"
            >
              <div
                v-for="f in t.user.files"
                :key="f.path"
                class="attach-chip readonly"
                :title="f.path"
              >
                <component :is="attachIcon(f.kind)" :size="14" :stroke-width="1.7" />
                <span class="ac-name">{{ f.name }}</span>
              </div>
            </div>
          </div>
        </div>

        <!-- 助手回复：纯文本，无头像无边框（Codex 式） -->
        <div
          v-if="
            t.hasAssistant ||
            t.tools.length ||
            t.artifacts.length ||
            t.errors.length ||
            isPending(t)
          "
          class="msg ai"
        >
          <!-- 工具调用：低调 pill,点击展开输入摘要 -->
          <div v-if="t.tools.length" class="tool-strip">
            <template v-for="(tl, j) in t.tools" :key="j">
              <button
                class="tool-pill"
                :class="{
                  open: expandedTool === `${t.key}:${j}`,
                  clickable: tl.details.length > 0,
                }"
                @click="tl.details.length && toggleTool(t.key, j)"
              >
                <Wrench :size="11" :stroke-width="1.8" />
                {{ toolLabel(tl.name) }}
                <span v-if="tl.count > 1" class="tp-count">×{{ tl.count }}</span>
              </button>
            </template>
          </div>
          <div
            v-for="(tl, j) in t.tools"
            :key="'d' + j"
            v-show="expandedTool === `${t.key}:${j}`"
            class="tool-detail"
          >
            <div class="td-head">{{ toolLabel(tl.name) }} · 输入摘要</div>
            <div v-for="(d, x) in tl.details" :key="x" class="td-line">{{ d }}</div>
          </div>

          <!-- 参考文件：豆包式小胶囊, 收在回答最前面, 点开右侧预览 -->
          <div v-if="t.text && refFiles(t).length" class="ref-files">
            <span class="ref-label">参考 {{ refFiles(t).length }} 个文件</span>
            <button
              v-for="p in refFiles(t)"
              :key="p"
              class="ref-pill"
              :title="p"
              @click="openArtifact(p)"
            >
              <component :is="artifactIcon(p)" :size="12" :stroke-width="1.7" />
              <span class="ref-name">{{ fileName(p) }}</span>
            </button>
          </div>

          <!-- 正文：markdown 渲染(流式中的活跃回合跳过异步高亮排队) -->
          <div v-if="t.text" class="md" v-html="renderMd(t.text, !isPending(t))"></div>

          <!-- 生成中：三点呼吸 -->
          <div v-if="isPending(t)" class="typing">
            <span></span><span></span><span></span>
          </div>

          <!-- 错误行 -->
          <div v-for="(e, j) in t.errors" :key="'e' + j" class="err-line">
            {{ e }}
          </div>

          <!-- 生成的文件：文件夹卡片统一收在回答末尾, 点文件行在右侧抽屉预览 -->
          <div v-if="t.artifacts.length" class="files">
            <div class="folder-card">
              <button class="folder-head" @click="toggleFiles(t.key)">
                <FolderOpen :size="15" :stroke-width="1.7" class="folder-ico" />
                <span class="folder-title">本轮产物</span>
                <span class="folder-count">{{ t.artifacts.length }}</span>
                <ChevronDown
                  :size="14"
                  :stroke-width="2"
                  class="folder-chev"
                  :class="{ closed: filesCollapsed[t.key] }"
                />
              </button>
              <div v-if="!filesCollapsed[t.key]" class="folder-body">
                <button
                  v-for="a in t.artifacts"
                  :key="a"
                  class="file-row"
                  :class="{ active: artifactsStore.current?.path === a }"
                  :title="a"
                  @click="openArtifact(a)"
                >
                  <component
                    :is="artifactIcon(a)"
                    :size="15"
                    :stroke-width="1.7"
                    class="fr-ico"
                  />
                  <span class="fr-name">{{ fileName(a) }}</span>
                  <span v-if="fileExt(a)" class="fr-ext">{{ fileExt(a) }}</span>
                  <ExternalLink :size="12" :stroke-width="1.8" class="fr-open" />
                </button>
              </div>
            </div>
          </div>

          <!-- 回答下方操作：复制 / 重新生成 / 时间 -->
          <div
            v-if="t.hasAssistant && t.text && !isPending(t)"
            class="turn-actions"
          >
            <button class="ta-btn" title="复制回答" @click="copyTurn(t)">
              <Copy :size="13" :stroke-width="1.8" />
              <span>复制</span>
            </button>
            <button
              v-if="t.user && !sending"
              class="ta-btn"
              title="用同样的问题再生成一次"
              @click="regenerate(t)"
            >
              <RotateCcw :size="13" :stroke-width="1.8" />
              <span>重新生成</span>
            </button>
            <span v-if="t.at" class="ta-time">{{ fmtTime(t.at) }}</span>
          </div>
        </div>
      </div>

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
        <!-- 今日建议（每日任务）：底部留一枚小胶囊随时重开；正文是居中大弹窗 -->
        <div v-if="briefings.length" class="brief-mini">
          <button
            class="brief-chip"
            :class="{ active: briefOpen }"
            @click="briefOpen = true"
          >
            <Sparkles :size="13" :stroke-width="1.9" class="bc-spark" />
            <span class="bc-text">今日建议</span>
            <span class="bc-count">{{ briefings.length }}</span>
          </button>
        </div>
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
                  l: '自动 · 所有会话',
                  d: '所有会话放行非高危操作(不绕过权限确认)',
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

    <!-- 今日建议 · 居中大弹窗（开软件自动弹一次，胶囊可重开） -->
    <Teleport to="body">
      <div
        v-if="briefOpen && briefings.length"
        class="brief-modal-scrim"
        @click.self="briefOpen = false"
      >
        <div class="brief-modal">
          <div class="bm-head">
            <span class="bm-ic"><Sparkles :size="18" :stroke-width="1.7" /></span>
            <div class="bm-tt">
              <span class="bm-title">为你准备的下一步</span>
              <span class="bm-sub"
                >读了你最近的对话、新资料和还没收尾的项目，我想到这几件可以替你做的事。</span
              >
            </div>
            <span class="bm-count">{{ briefings.length }}</span>
            <button class="bm-close" title="关闭" @click="briefOpen = false">
              <X :size="17" :stroke-width="1.9" />
            </button>
          </div>
          <div class="bm-cards">
            <div
              v-for="s in briefings"
              :key="s.id"
              class="bm-card"
              :data-kind="s.kind || 'progress'"
            >
              <div class="bmc-head">
                <span class="bmc-ic">
                  <component
                    :is="briefKind(s).icon"
                    :size="17"
                    :stroke-width="1.8"
                  />
                </span>
                <span class="bmc-kind">{{ briefKind(s).label }}</span>
                <span v-if="s.source" class="bmc-src" :title="'依据：' + s.source">
                  <BookOpen :size="11" :stroke-width="2" />
                  <span class="bmc-src-t">{{ s.source }}</span>
                </span>
              </div>
              <div class="bmc-title">{{ s.title }}</div>
              <div v-if="s.why" class="bmc-why">{{ s.why }}</div>
              <div v-if="s.how" class="bmc-how">
                <span class="bmc-how-tag">怎么做</span>{{ s.how }}
              </div>
              <div class="bmc-act">
                <button class="bmc-go" @click="runBriefing(s)">
                  <span>让我去做</span>
                  <ArrowRight :size="14" :stroke-width="2.1" />
                </button>
                <button class="bmc-dismiss" @click="dismissBriefing(s.id)">先放一放</button>
              </div>
            </div>
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>

<style scoped>
.chat {
  display: flex;
  flex-direction: column;
  height: 100%;
  position: relative;
}
.chat-top {
  position: relative;
  padding: 16px 30px;
  display: flex;
  align-items: center;
  gap: 12px;
  /* 顶栏与下方回答区无缝连成一片：透明背景、无分隔线，不再是单独的异色条；
     比原来略高更有呼吸感（仿豆包 / Coda） */
  border-bottom: none;
  background: transparent;
}
.chat-title {
  flex: 1;
  min-width: 0;
  display: flex;
  align-items: center;
  gap: 8px;
  font-family: var(--serif);
}
.t-text {
  font-size: 13px;
  font-weight: 600;
  color: var(--text);
}
.t-text.muted {
  font-weight: 400;
  color: var(--muted);
}
/* 文件抽屉开关（移到顶栏右侧；收起后右侧整列消失，靠它再展开） */
.drawer-toggle {
  width: 30px;
  height: 30px;
  border: none;
  border-radius: 7px;
  background: transparent;
  color: var(--muted);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  transition: background 0.15s, color 0.15s;
}
.drawer-toggle:hover {
  background: var(--selection-bg);
  color: var(--text);
}

/* 已置顶标记（标题前的小别针） */
.t-pin {
  color: var(--gold);
  transform: rotate(35deg);
  flex-shrink: 0;
}

/* 标题就地重命名输入框 */
.t-rename {
  flex: 1;
  min-width: 0;
  max-width: 420px;
  font-family: var(--serif);
  font-size: 13px;
  font-weight: 600;
  color: var(--text);
  padding: 3px 8px;
  border: 1px solid var(--primary);
  border-radius: 6px;
  background: var(--panel);
  outline: none;
  box-shadow: 0 0 0 3px var(--primary-soft);
}

/* ── 对话「更多」菜单 ── */
.conv-menu-wrap {
  position: relative;
  flex-shrink: 0;
}
.conv-more {
  width: 26px;
  height: 26px;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--muted);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  transition: background 0.15s, color 0.15s;
}
.conv-more:hover,
.conv-more.active {
  background: var(--selection-bg);
  color: var(--text);
}
.conv-menu {
  position: absolute;
  top: calc(100% + 6px);
  left: 0;
  z-index: 40;
  min-width: 184px;
  padding: 5px;
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: 10px;
  box-shadow: var(--shadow-lg);
  animation: cm-pop 130ms ease;
}
@keyframes cm-pop {
  from {
    opacity: 0;
    transform: translateY(-4px);
  }
}
.cm-item {
  display: flex;
  align-items: center;
  gap: 9px;
  width: 100%;
  padding: 8px 10px;
  border: none;
  background: transparent;
  color: var(--text-2);
  font-size: 12.5px;
  border-radius: 6px;
  text-align: left;
}
.cm-item:hover {
  background: var(--bg-soft);
  color: var(--text);
}
.cm-item.danger {
  color: var(--vermilion);
}
.cm-item.danger:hover {
  background: var(--vermilion-soft);
}
.cm-sep {
  height: 1px;
  margin: 5px 8px;
  background: var(--border-soft);
}
.cm-meta {
  padding: 6px 10px 4px;
  font-size: 10.5px;
  color: var(--dim);
  border-top: 1px solid var(--border-soft);
  margin-top: 5px;
}

/* 复制反馈小提示 */
.copy-toast {
  position: absolute;
  top: calc(100% + 8px);
  left: 50%;
  transform: translateX(-50%);
  z-index: 45;
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 6px 12px;
  background: var(--btn-solid-bg);
  color: var(--btn-solid-text);
  font-size: 12px;
  border-radius: 8px;
  box-shadow: var(--shadow-lg);
  pointer-events: none;
}
.copy-fade-enter-active,
.copy-fade-leave-active {
  transition: opacity 0.2s ease, transform 0.2s ease;
}
.copy-fade-enter-from,
.copy-fade-leave-to {
  opacity: 0;
  transform: translate(-50%, -4px);
}

.messages {
  flex: 1;
  overflow-y: auto;
  /* 底部留出输入玻璃卡的悬浮空间：消息从液态玻璃下穿过 */
  padding: 40px 32px 210px;
}
.hero-wrap {
  margin: 60px auto 40px;
  text-align: center;
  max-width: 720px;
}
.hero {
  font-family: var(--serif);
  font-size: 36px;
  font-weight: 600;
  letter-spacing: 4px;
  color: var(--ink);
}
.hero-sub {
  margin-top: 16px;
  color: var(--muted);
  font-size: 13px;
  letter-spacing: 0.5px;
}
.hero-sub strong {
  color: var(--primary);
  font-weight: 700;
}
.hero-sub code {
  font-family: var(--mono);
  font-size: 0.9em;
  color: var(--primary-deep);
  background: var(--bg-soft);
  border: 1px solid var(--border-soft);
  padding: 1px 6px;
  border-radius: 5px;
}
.hero-meta {
  margin-top: 22px;
  display: flex;
  flex-wrap: wrap;
  justify-content: center;
  gap: 8px;
}
.hm-pill {
  font-family: var(--mono);
  font-size: 11px;
  color: var(--primary-deep);
  background: var(--primary-soft);
  border: 1px solid var(--primary-soft);
  border-radius: 999px;
  padding: 5px 11px;
  letter-spacing: 0.02em;
  display: inline-flex;
  align-items: center;
  gap: 4px;
}
.hm-pill code {
  font-size: 0.92em;
  color: var(--primary-deep);
  background: transparent;
  border: none;
  padding: 0;
}
/* ── 下一步工作流推荐(空白页建议气泡,仿豆包)── */
.flow-suggest {
  margin: 30px auto 0;
  max-width: 680px;
}
.flow-head {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  font-size: 12.5px;
  color: var(--muted);
  letter-spacing: 0.3px;
}
.flow-head svg { color: var(--gold, #d4b06a); flex: none; }
.flow-refresh {
  display: inline-flex;
  align-items: center;
  gap: 3px;
  margin-left: 6px;
  padding: 2px 8px;
  font-size: 11.5px;
  color: var(--muted);
  background: transparent;
  border: 1px solid var(--border-soft);
  border-radius: 999px;
  cursor: pointer;
  transition: color 0.15s, border-color 0.15s, background 0.15s;
}
.flow-refresh:hover { color: var(--text); border-color: var(--border); background: var(--bg-soft); }
.flow-chips {
  margin-top: 14px;
  display: flex;
  flex-wrap: wrap;
  justify-content: center;
  gap: 9px;
}
.flow-chip {
  max-width: 100%;
  text-align: left;
  font-size: 13px;
  line-height: 1.4;
  color: var(--text);
  background: var(--panel, var(--bg-soft));
  border: 1px solid var(--border-soft);
  border-radius: 13px;
  padding: 9px 15px;
  cursor: pointer;
  white-space: normal;
  overflow: hidden;
  text-overflow: ellipsis;
  transition: transform 0.14s, border-color 0.14s, box-shadow 0.14s, background 0.14s;
}
.flow-chip:hover {
  transform: translateY(-2px);
  border-color: color-mix(in srgb, var(--gold, #d4b06a) 55%, transparent);
  background: color-mix(in srgb, var(--gold, #d4b06a) 8%, var(--panel, var(--bg-soft)));
  box-shadow: 0 8px 22px -14px color-mix(in srgb, var(--gold, #d4b06a) 80%, transparent);
}
.flow-chip:active { transform: translateY(0); }
.flow-chip.skeleton {
  width: 156px;
  height: 36px;
  cursor: default;
  pointer-events: none;
  background: linear-gradient(90deg, var(--bg-soft) 25%, var(--border-soft) 37%, var(--bg-soft) 63%);
  background-size: 400% 100%;
  animation: flow-sk 1.3s ease infinite;
  border-color: transparent;
}
@keyframes flow-sk {
  0% { background-position: 100% 0; }
  100% { background-position: 0 0; }
}
/* ── 毛主席项目彩蛋空状态 ── */
.mao-hero {
  font-family: var(--serif);
  font-size: 40px;
  font-weight: 600;
  letter-spacing: 6px;
  color: var(--vermilion);
}
.mao-desc {
  margin: 26px auto 0;
  max-width: 560px;
  font-size: 13.5px;
  line-height: 2;
  color: var(--text-2);
  text-align: center;
}
.mao-desc strong {
  color: var(--vermilion);
  font-weight: 600;
}
.mao-slogan {
  margin-top: 34px;
  font-family: var(--serif);
  font-size: 16px;
  letter-spacing: 3px;
  color: var(--vermilion);
  font-weight: 600;
}

/* ═══════════ 对话渲染 (Codex 式：纯对话，无头像) ═══════════ */
.turn {
  max-width: 880px;
  margin: 0 auto 22px;
  animation: card-rise 0.32s var(--ease-out) both;
}
@media (prefers-reduced-motion: reduce) {
  .turn,
  .folder-card,
  .ref-files {
    animation: none;
  }
}

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

/* 用户：右对齐中性灰气泡，无头像 */
.msg.user {
  display: flex;
  justify-content: flex-end;
  align-items: center;
  gap: 8px;
  margin-bottom: 18px;
}
/* 编辑并重发(悬停气泡时浮现) */
.u-edit {
  width: 26px;
  height: 26px;
  border: none;
  border-radius: 7px;
  background: transparent;
  color: var(--muted);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  opacity: 0;
  transition: opacity 0.15s, background 0.15s;
  cursor: pointer;
  flex-shrink: 0;
}
.msg.user:hover .u-edit {
  opacity: 1;
}
.u-edit:hover {
  background: var(--bg-soft);
  color: var(--text);
}
.bubble-user {
  max-width: 82%;
  background: var(--bg-soft);
  border: 1px solid var(--border-soft);
  border-radius: 16px;
  padding: 9px 15px;
}
.u-text {
  white-space: pre-wrap;
  word-break: break-word;
  font-size: 13.5px;
  line-height: 1.65;
  color: var(--text);
}

/* 助手：纯文本，无头像无边框（Codex 式） */
.msg.ai {
  min-width: 0;
}

/* 工具调用 pill */
.tool-strip {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin-bottom: 10px;
}
.tool-pill {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 11px;
  color: var(--text-2);
  background: var(--bg-soft);
  border: 1px solid var(--border-soft);
  padding: 3px 9px;
  border-radius: 20px;
  cursor: default;
}
.tool-pill.clickable {
  cursor: pointer;
}
.tool-pill.clickable:hover,
.tool-pill.open {
  border-color: var(--primary);
  color: var(--primary-deep);
  background: var(--primary-soft);
}
.tool-pill :deep(svg) {
  color: var(--primary);
}
.tp-count {
  font-size: 10px;
  color: var(--muted);
}
/* 工具输入摘要(pill 点开) */
.tool-detail {
  margin: -4px 0 10px;
  padding: 8px 12px;
  border-radius: 9px;
  background: var(--bg-soft);
  border: 1px solid var(--border-soft);
}
.td-head {
  font-size: 10.5px;
  letter-spacing: 0.4px;
  color: var(--muted);
  margin-bottom: 4px;
}
.td-line {
  font-family: var(--mono);
  font-size: 11.5px;
  color: var(--text-2);
  padding: 1px 0;
  word-break: break-all;
}

/* 生成中三点 */
.typing {
  display: flex;
  gap: 4px;
  padding: 4px 0 2px;
}
.typing span {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--primary);
  opacity: 0.5;
  /* 游戏式弹跳: 顶点带 squash & stretch(压扁-拉伸)与光晕, 比匀速正弦更有"落地反弹"的实感 */
  animation: typing-bounce 1.1s cubic-bezier(0.36, 0, 0.64, 1) infinite;
}
.typing span:nth-child(2) {
  animation-delay: 0.15s;
}
.typing span:nth-child(3) {
  animation-delay: 0.3s;
}
@keyframes typing-bounce {
  0%,
  70%,
  100% {
    transform: translateY(0) scale(1, 0.92);
    opacity: 0.35;
    box-shadow: 0 0 0 rgba(0, 0, 0, 0);
  }
  35% {
    transform: translateY(-5px) scale(0.92, 1.1);
    opacity: 1;
    box-shadow: 0 2px 6px var(--primary-soft), 0 0 6px var(--primary-soft);
  }
  55% {
    transform: translateY(0) scale(1.15, 0.8);
    opacity: 0.7;
  }
}
@media (prefers-reduced-motion: reduce) {
  .typing span {
    animation: none;
    opacity: 0.6;
  }
}

.err-line {
  font-family: var(--mono);
  font-size: 12px;
  color: var(--vermilion);
  background: var(--vermilion-soft);
  border-radius: 6px;
  padding: 6px 10px;
  margin-top: 8px;
  white-space: pre-wrap;
  word-break: break-word;
}

/* 生成的文件：回答末尾 */
.files {
  margin-top: 12px;
  padding-top: 11px;
  border-top: 1px dashed var(--border);
}

/* 回答下方操作行（复制） —— 平时淡出，悬停回答时浮现 */
.turn-actions {
  margin-top: 10px;
  display: flex;
  gap: 6px;
  opacity: 0;
  transition: opacity 0.15s;
}
.msg.ai:hover .turn-actions {
  opacity: 1;
}
.ta-btn {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 4px 9px;
  border: 1px solid var(--border-soft);
  background: var(--panel);
  color: var(--muted);
  font-size: 11.5px;
  border-radius: 7px;
  transition: border-color 0.15s, color 0.15s, background 0.15s,
    transform 0.22s var(--ease-spring), box-shadow 0.22s var(--ease-out);
}
.ta-btn:hover {
  border-color: var(--border);
  color: var(--text);
  background: var(--bg-soft);
  transform: translateY(-1px);
  box-shadow: var(--shadow-sm);
}
.ta-time {
  align-self: center;
  font-size: 10.5px;
  color: var(--dim);
  margin-left: 4px;
}

/* ── markdown 正文排版 ── */
.md {
  font-size: 13.5px;
  line-height: 1.72;
  color: var(--text);
  word-break: break-word;
}
.md :deep(> *:first-child) {
  margin-top: 0;
}
.md :deep(> *:last-child) {
  margin-bottom: 0;
}
.md :deep(h1),
.md :deep(h2),
.md :deep(h3),
.md :deep(h4) {
  font-family: var(--serif);
  line-height: 1.35;
  margin: 1.1em 0 0.5em;
  color: var(--ink);
}
.md :deep(h1) {
  font-size: 1.5em;
}
.md :deep(h2) {
  font-size: 1.3em;
}
.md :deep(h3) {
  font-size: 1.12em;
}
.md :deep(h4) {
  font-size: 1em;
}
.md :deep(p) {
  margin: 0.55em 0;
}
.md :deep(ul),
.md :deep(ol) {
  margin: 0.55em 0;
  padding-left: 1.5em;
}
.md :deep(li) {
  margin: 0.25em 0;
}
.md :deep(li::marker) {
  color: var(--muted);
}
.md :deep(a) {
  color: var(--primary);
  text-decoration: none;
  border-bottom: 1px solid var(--primary-soft);
}
.md :deep(a:hover) {
  border-bottom-color: var(--primary);
}
.md :deep(strong) {
  color: var(--ink);
  font-weight: 600;
}
.md :deep(hr) {
  border: none;
  border-top: 1px solid var(--border);
  margin: 1.1em 0;
}
.md :deep(blockquote) {
  margin: 0.7em 0;
  padding: 0.4em 0.9em;
  border-left: 3px solid var(--primary);
  background: var(--primary-soft);
  border-radius: 0 6px 6px 0;
  color: var(--text-2);
}
.md :deep(blockquote p) {
  margin: 0.2em 0;
}
/* 行内代码 */
.md :deep(:not(pre) > code) {
  font-family: var(--mono);
  font-size: 0.88em;
  background: var(--code-bg);
  color: var(--primary-deep);
  padding: 0.12em 0.4em;
  border-radius: 5px;
  border: 1px solid var(--border-soft);
}
/* 代码块：深色卡片，横向滚动，盒绘对齐 */
.md :deep(pre) {
  background: #0f1b2d;
  color: #dbe6f5;
  border-radius: 10px;
  padding: 13px 15px;
  overflow-x: auto;
  margin: 0.8em 0;
  line-height: 1.55;
}
.md :deep(pre code) {
  font-family: var(--mono);
  font-size: 12.4px;
  background: none;
  border: none;
  padding: 0;
  color: inherit;
  white-space: pre;
}
/* 表格 */
.md :deep(table) {
  border-collapse: collapse;
  width: 100%;
  margin: 0.8em 0;
  font-size: 12.8px;
  display: block;
  overflow-x: auto;
}
.md :deep(th),
.md :deep(td) {
  border: 1px solid var(--border);
  padding: 6px 11px;
  text-align: left;
}
.md :deep(thead th) {
  background: var(--bg-soft);
  font-weight: 600;
  color: var(--text);
}
.md :deep(img) {
  max-width: 100%;
  border-radius: 6px;
}

/* 成品文件 chips —— 回答末尾的可点击文件 */
/* ── 产物文件夹卡片(Kimi 式)：头部可折叠, 文件按行排列, 点行右侧预览 ── */
.folder-card {
  max-width: 420px;
  border: 1px solid var(--border-soft);
  border-radius: 10px;
  background: var(--panel);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.9), var(--shadow-sm);
  overflow: hidden;
  animation: card-rise 0.35s var(--ease-out) both;
  transition: box-shadow 0.25s var(--ease-out), border-color 0.25s;
}
.folder-card:hover {
  border-color: var(--border);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.9), var(--shadow);
}
@keyframes card-rise {
  from {
    opacity: 0;
    transform: translateY(6px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}
.folder-head {
  display: flex;
  align-items: center;
  gap: 7px;
  width: 100%;
  padding: 8px 11px;
  font-size: 12px;
  color: var(--text);
  cursor: pointer;
  background: transparent;
  border: none;
  text-align: left;
}
.folder-head:hover {
  background: var(--bg-soft);
}
.folder-ico {
  color: var(--primary);
  flex-shrink: 0;
}
.folder-title {
  font-weight: 600;
  letter-spacing: 0.3px;
}
.folder-count {
  padding: 0 6px;
  border-radius: 8px;
  background: var(--primary-soft);
  color: var(--primary);
  font-size: 10.5px;
  line-height: 16px;
}
.folder-chev {
  margin-left: auto;
  color: var(--muted);
  transition: transform 0.15s;
}
.folder-chev.closed {
  transform: rotate(-90deg);
}
.folder-body {
  border-top: 1px solid var(--border-soft);
  padding: 4px;
  display: flex;
  flex-direction: column;
}
.file-row {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 6px 8px;
  border: none;
  border-radius: 7px;
  background: transparent;
  color: var(--text);
  font-size: 12.5px;
  cursor: pointer;
  text-align: left;
  transition: background 0.12s, color 0.12s,
    transform 0.22s var(--ease-spring);
}
.file-row:hover,
.file-row.active {
  background: var(--primary-soft);
  color: var(--primary);
  transform: translateX(2px);
}
.file-row .fr-ico {
  color: var(--primary);
  flex-shrink: 0;
}
.file-row .fr-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-weight: 500;
  min-width: 0;
}
.file-row .fr-ext {
  flex-shrink: 0;
  padding: 0 5px;
  border-radius: 5px;
  background: var(--bg-soft);
  color: var(--muted);
  font-size: 10px;
  line-height: 15px;
  text-transform: uppercase;
  letter-spacing: 0.4px;
}
.file-row .fr-open {
  margin-left: auto;
  flex-shrink: 0;
  opacity: 0;
  transition: opacity 0.12s;
}
.file-row:hover .fr-open,
.file-row.active .fr-open {
  opacity: 0.8;
}

/* ── TL;DR 速览行：回答开头一句话结论(renderMd 从正文摘出)。
   刻意低调(豆包式): 只是加粗一行 + 细虚线分隔, 不做彩色卡片。 ── */
.md :deep(.tldr) {
  display: flex;
  align-items: baseline;
  gap: 8px;
  margin: 0 0 10px;
  padding: 0 0 10px;
  border-bottom: 1px dashed var(--border-soft);
}
.md :deep(.tldr .tldr-tag) {
  flex-shrink: 0;
  padding: 0 5px;
  border: 1px solid var(--border-soft);
  border-radius: 5px;
  font-size: 9.5px;
  font-weight: 600;
  letter-spacing: 0.6px;
  line-height: 15px;
  color: var(--muted);
}
.md :deep(.tldr .tldr-body) {
  min-width: 0;
  font-size: 13.5px;
  font-weight: 600;
  line-height: 1.6;
}
.md :deep(.tldr .tldr-body p) {
  margin: 0;
  display: inline;
}

/* ── 参考文件胶囊：回答最前面一行小 pill, 点开右侧预览 ── */
.ref-files {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 6px;
  margin-bottom: 9px;
  animation: card-rise 0.3s var(--ease-out) both;
}
.ref-label {
  font-size: 10.5px;
  color: var(--dim);
  letter-spacing: 0.3px;
  margin-right: 2px;
}
.ref-pill {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  max-width: 200px;
  padding: 2px 8px;
  border: 1px solid var(--border-soft);
  border-radius: 999px;
  background: var(--bg-soft);
  color: var(--muted);
  font-size: 11px;
  cursor: pointer;
  transition: color 0.12s, border-color 0.12s, background 0.12s,
    transform 0.22s var(--ease-spring), box-shadow 0.22s var(--ease-out);
}
.ref-pill:hover {
  color: var(--primary);
  border-color: var(--primary);
  background: var(--primary-soft);
  transform: translateY(-1px);
  box-shadow: var(--shadow-sm);
}
.ref-pill .ref-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

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

/* ── 今日建议（每日任务）：底部授权栏左侧的小胶囊 + 向上弹出面板 ── */
.brief-mini {
  position: relative;
  pointer-events: auto;
}
.brief-chip {
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
.brief-chip:hover { border-color: var(--border); color: var(--text); }
.brief-chip.active { border-color: var(--border); color: var(--ink); }
.bc-spark { color: var(--gold, #d4b06a); flex-shrink: 0; }
.bc-text { letter-spacing: 0.3px; }
.bc-count {
  font-size: 11px; color: var(--btn-solid-text, #fff);
  background: var(--btn-solid-bg); border-radius: 20px;
  padding: 0 6px; line-height: 16px; min-width: 16px; text-align: center;
}
/* ── 今日建议 · 居中大弹窗（苹果琉璃质感）── */
.brief-modal-scrim {
  position: fixed;
  inset: 0;
  z-index: 200;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
  /* 背景做成「磨砂玻璃门」：弱压暗 + 强模糊，让后面的界面虚化透出 */
  background: rgba(26, 24, 32, 0.32);
  backdrop-filter: blur(12px) saturate(118%);
  -webkit-backdrop-filter: blur(12px) saturate(118%);
  animation: bm-fade 0.22s ease;
}
@keyframes bm-fade { from { opacity: 0; } to { opacity: 1; } }
.brief-modal {
  width: 620px;
  max-width: 92vw;
  max-height: 84vh;
  display: flex;
  flex-direction: column;
  position: relative;
  /* 琉璃面板：近白高透叠强模糊 + 高光描边 + 投影，仿 macOS 通知中心 */
  background: linear-gradient(160deg, rgba(255, 255, 255, 0.82), rgba(255, 255, 255, 0.62));
  backdrop-filter: blur(44px) saturate(185%);
  -webkit-backdrop-filter: blur(44px) saturate(185%);
  border: 1px solid rgba(255, 255, 255, 0.72);
  border-radius: 22px;
  box-shadow:
    0 28px 80px -20px rgba(18, 16, 28, 0.5),
    0 2px 10px rgba(18, 16, 28, 0.12),
    inset 0 1px 0 rgba(255, 255, 255, 0.9);
  overflow: hidden;
  animation: bm-pop 0.26s cubic-bezier(0.2, 0.85, 0.3, 1);
}
html[data-theme="dark"] .brief-modal,
html[data-theme="aurora-dark"] .brief-modal {
  background: linear-gradient(160deg, rgba(48, 48, 52, 0.78), rgba(28, 28, 32, 0.6));
  border-color: rgba(255, 255, 255, 0.1);
  box-shadow:
    0 28px 80px -20px rgba(0, 0, 0, 0.72),
    0 2px 10px rgba(0, 0, 0, 0.4),
    inset 0 1px 0 rgba(255, 255, 255, 0.08);
}
@keyframes bm-pop {
  from { opacity: 0; transform: translateY(12px) scale(0.96); }
  to { opacity: 1; transform: translateY(0) scale(1); }
}
.bm-head {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 18px 20px 16px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.5);
}
html[data-theme="dark"] .bm-head,
html[data-theme="aurora-dark"] .bm-head {
  border-bottom-color: rgba(255, 255, 255, 0.08);
}
.bm-ic {
  width: 36px; height: 36px; border-radius: 11px; flex-shrink: 0;
  display: inline-flex; align-items: center; justify-content: center;
  color: #fff;
  background: linear-gradient(140deg, #6d8fb8, #2c4661);
  box-shadow: 0 5px 14px -4px rgba(44, 70, 97, 0.6), inset 0 1px 0 rgba(255, 255, 255, 0.32);
}
.bm-tt { display: flex; flex-direction: column; gap: 3px; min-width: 0; flex: 1; }
.bm-title { font-size: 16px; font-weight: 650; color: var(--ink); letter-spacing: 0.3px; }
.bm-sub { font-size: 12px; line-height: 1.6; color: var(--text-2); }
.bm-count {
  font-size: 12px; color: var(--ink);
  background: rgba(120, 120, 128, 0.16); border-radius: 20px;
  padding: 1px 9px; line-height: 19px; min-width: 21px; text-align: center;
  flex-shrink: 0;
}
.bm-close {
  flex-shrink: 0; border: none; background: transparent; color: var(--muted);
  display: inline-flex; padding: 6px; border-radius: 9px; cursor: pointer;
  transition: color 0.14s ease, background 0.14s ease;
}
.bm-close:hover { color: var(--ink); background: rgba(120, 120, 128, 0.16); }
.bm-cards {
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 16px 18px 20px;
  overflow-y: auto;
}
.bm-card {
  position: relative;
  border-radius: 16px;
  padding: 15px 17px;
  /* 卡片本身也是一层更浅的琉璃，悬浮微微上抬 */
  --accent: #2f6fed;
  --accent-soft: rgba(47, 111, 237, 0.12);
  background: rgba(255, 255, 255, 0.55);
  border: 1px solid rgba(255, 255, 255, 0.66);
  box-shadow: 0 6px 18px -11px rgba(18, 16, 28, 0.28), inset 0 1px 0 rgba(255, 255, 255, 0.7);
  transition: transform 0.18s ease, box-shadow 0.18s ease, background 0.18s ease;
}
.bm-card:hover {
  transform: translateY(-1px);
  background: rgba(255, 255, 255, 0.74);
  box-shadow: 0 14px 28px -12px rgba(18, 16, 28, 0.34), inset 0 1px 0 rgba(255, 255, 255, 0.85);
}
html[data-theme="dark"] .bm-card,
html[data-theme="aurora-dark"] .bm-card {
  background: rgba(255, 255, 255, 0.05);
  border-color: rgba(255, 255, 255, 0.09);
  box-shadow: 0 6px 18px -11px rgba(0, 0, 0, 0.5), inset 0 1px 0 rgba(255, 255, 255, 0.05);
}
html[data-theme="dark"] .bm-card:hover,
html[data-theme="aurora-dark"] .bm-card:hover {
  background: rgba(255, 255, 255, 0.09);
}
/* 四类建议各一抹克制的色：推进=蓝 / 收尾=琥珀 / 工作流=紫 / 整理=绿 */
.bm-card[data-kind="progress"] { --accent: #2f6fed; --accent-soft: rgba(47, 111, 237, 0.12); }
.bm-card[data-kind="wrapup"]   { --accent: #d98a16; --accent-soft: rgba(217, 138, 22, 0.14); }
.bm-card[data-kind="workflow"] { --accent: #7c5cd9; --accent-soft: rgba(124, 92, 217, 0.13); }
.bm-card[data-kind="organize"] { --accent: #0f9d6e; --accent-soft: rgba(15, 157, 110, 0.13); }
.bmc-head { display: flex; align-items: center; gap: 8px; margin-bottom: 10px; }
.bmc-ic {
  width: 28px; height: 28px; border-radius: 9px; flex-shrink: 0;
  display: inline-flex; align-items: center; justify-content: center;
  color: var(--accent);
  background: var(--accent-soft);
}
.bmc-kind {
  font-size: 11.5px; font-weight: 650; letter-spacing: 0.3px; color: var(--accent);
}
.bmc-src {
  margin-left: auto;
  display: inline-flex; align-items: center; gap: 4px;
  max-width: 48%;
  font-size: 11px; color: var(--text-2);
  background: rgba(120, 120, 128, 0.13);
  border-radius: 8px; padding: 3px 9px;
}
.bmc-src svg { flex-shrink: 0; opacity: 0.8; }
.bmc-src-t { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.bmc-title { font-size: 15px; font-weight: 650; color: var(--ink); line-height: 1.5; }
.bmc-why {
  font-size: 12.5px; color: var(--text-2); line-height: 1.7; margin-top: 7px;
}
.bmc-how {
  font-size: 12.5px; color: var(--muted); line-height: 1.7; margin-top: 6px;
}
.bmc-how-tag {
  display: inline-block; margin-right: 6px; vertical-align: 1px;
  font-size: 10.5px; font-weight: 600; color: var(--text-2);
  background: rgba(120, 120, 128, 0.13); border-radius: 6px; padding: 1px 7px;
}
.bmc-act { display: flex; align-items: center; gap: 10px; margin-top: 15px; }
.bmc-go {
  display: inline-flex; align-items: center; gap: 6px;
  border: none; cursor: pointer;
  background: linear-gradient(140deg, #38618c, #2c4661);
  color: #fff;
  font-size: 13px; font-weight: 600; letter-spacing: 0.4px;
  padding: 8px 16px; border-radius: 11px;
  box-shadow: 0 7px 18px -7px rgba(44, 70, 97, 0.62), inset 0 1px 0 rgba(255, 255, 255, 0.25);
  transition: transform 0.14s ease, filter 0.14s ease;
}
.bmc-go:hover { transform: translateY(-1px); filter: brightness(1.07); }
.bmc-go:active { transform: translateY(0); }
.bmc-dismiss {
  border: none; background: transparent; color: var(--muted);
  font-size: 12.5px; padding: 8px 12px; border-radius: 9px; cursor: pointer;
  transition: color 0.14s ease, background 0.14s ease;
}
.bmc-dismiss:hover { color: var(--ink); background: rgba(120, 120, 128, 0.13); }
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
html[data-theme="dark"] .folder-card {
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.06), var(--shadow-sm);
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

/* ─────────── 附件 chips ─────────── */
.attach-chips {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin-bottom: 8px;
}
.attach-chips.in-bubble {
  margin-top: 8px;
  margin-bottom: 0;
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
.attach-chip.readonly {
  background: transparent;
  color: var(--primary-deep);
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
