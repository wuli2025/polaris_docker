<script setup lang="ts">
import { ref, computed, onMounted, onBeforeUnmount, nextTick, watch } from "vue";
import {
  Puzzle,
  ChevronDown,
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
  LoaderCircle,
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
} from "@lucide/vue";
import SearchGlass from "./icons/SearchGlass.vue";
import {
  chat,
  convApi,
  skills as skillsApi,
  invoke,
  listen,
  isTauri,
  uploadToBackend,
  type PermissionMode,
  type Skill,
  type AttachedFile,
  type Message,
} from "../tauri";
import { WebVoiceRecorder } from "../lib/webVoice";
import { renderMarkdown, mdVersion } from "../lib/markdown";
import { toast } from "../composables/useToast";
import { humanizeError } from "../lib/humanizeError";
import { useAppStore } from "../stores/app";
import { useSkillsStore } from "../stores/skills";
import { useArtifactsStore } from "../stores/artifacts";
import { useChatStore, type Bubble } from "../stores/chat";
import { useWorkflowsStore } from "../stores/workflows";
import { useLongTaskStore, detectLongTask } from "../stores/longtask";
import { useFileDrop } from "../composables/useFileDrop";

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

const input = ref("");
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
function renderMd(text: string, enhance = true): string {
  void mdVersion.value; // 注册响应式依赖:增强完成后刷新
  const clean = (text || "").replace(ANSI_RE, "");
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
const renderTurns = computed<Turn[]>(() => {
  const out: Turn[] = [];
  let cur: Turn | undefined;
  let k = 0;
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
    return turn;
  };
  for (const b of bubbles.value) {
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
        for (const a of b.artifacts) if (!t.artifacts.includes(a)) t.artifacts.push(a);
      }
    }
  }
  return out;
});
function isPending(t: Turn): boolean {
  return sending.value && t === renderTurns.value[renderTurns.value.length - 1];
}

// ── 历史折叠:长对话只渲染最近 N 回合,顶部「加载更早」逐段放开 ──
const FOLD_STEP = 30;
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

onMounted(async () => {
  window.addEventListener("keydown", onGlobalKeydown);
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

// ─────────── 知识库严格搜索（KB）模式开关 ───────────
// 默认关闭：不注入大段 KB 导航，节省 token。打开后注入结构化 wiki + 双链地图。
const kbMode = ref(false);
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
  await loadSkills();
  scrollToBottom();
  // 若在别的视图点了工作流包「使用」才切来对话，挂载时补消费一次
  applyInsert(workflowsStore.insertRequest);
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

  const convId = await ensureConversation();
  if (!convId) return;

  // 把附件绝对路径拼进 prompt，让 claude 能用 Read 等工具读取
  let prompt = text || "请查看我上传的附件。";
  if (hasAttach) {
    const lines = attached.map((a) => `- ${a.path}`).join("\n");
    prompt += `\n\n---\n[附件]（用户拖拽上传，可用 Read 等工具读取）：\n${lines}`;
  }

  const display = text || "（仅附件）";

  input.value = "";
  attachments.value = [];
  histIdx = -1;

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
  });
}

async function cancel() {
  // 先停掉分批编排循环（否则它会在本轮 done 后又发下一批），再取消在飞的子进程
  if (app.currentConvId) longTaskStore.stop(app.currentConvId);
  await chatStore.cancel(app.currentConvId);
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
        </template>
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

          <!-- 生成的文件：统一收在回答末尾 -->
          <div v-if="t.artifacts.length" class="files">
            <div class="files-head">生成的文件 · {{ t.artifacts.length }}</div>
            <div class="files-list">
              <button
                v-for="a in t.artifacts"
                :key="a"
                class="artifact-chip"
                :class="{ active: artifactsStore.current?.path === a }"
                :title="a"
                @click="openArtifact(a)"
              >
                <component
                  :is="artifactIcon(a)"
                  :size="15"
                  :stroke-width="1.7"
                />
                <span class="af-name">{{ fileName(a) }}</span>
                <ExternalLink :size="12" :stroke-width="1.8" class="af-open" />
              </button>
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
            <LoaderCircle :size="14" :stroke-width="2" class="spin" />
            <span class="ac-name">{{ p.name }}</span>
          </div>
        </div>
        <textarea
          ref="inputEl"
          v-model="input"
          :placeholder="
            goalMode
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
              :class="{ active: goalMode }"
              @click="toggleGoal"
            >
              <Target :size="14" :stroke-width="1.8" />
              <span>目标模式</span>
              <div class="btn-tooltip">
                <div class="btn-tooltip-inner">
                  设定一个完成条件，Claude 会持续推进直到达成
                  <div class="btn-tooltip-sub">
                    条件满足前不中途收尾、不反问，自行规划下一步
                  </div>
                </div>
              </div>
            </button>
            <button
              class="toolbar-btn"
              :class="{ active: orchestrateMode }"
              @click="toggleOrchestrate"
            >
              <Workflow :size="14" :stroke-width="1.8" />
              <span>动态编排</span>
              <div class="btn-tooltip">
                <div class="btn-tooltip-inner">
                  多智能体编排：拆成多个独立子任务并行干，每条 实现→校验→修复
                  <div class="btn-tooltip-sub">
                    适合可拆分 + 可验证的任务（批量改写 / 多维审查 / 调研）· 比单轮更贵
                  </div>
                </div>
              </div>
            </button>
            <button
              class="toolbar-btn"
              :class="{ active: kbMode }"
              @click="toggleKb"
            >
              <BookOpen :size="14" :stroke-width="1.8" />
              <span>知识库</span>
              <div class="btn-tooltip">
                <div class="btn-tooltip-inner">
                  打开后注入完整 KB 结构化 wiki + 双链地图（消耗大量 token）
                  <div class="btn-tooltip-sub">
                    默认关闭以节省上下文；只在需要严格搜索知识库时打开
                  </div>
                </div>
              </div>
            </button>
            <button
              class="toolbar-btn"
              :class="{ active: batchMode }"
              @click="toggleBatch"
            >
              <Layers :size="14" :stroke-width="1.8" />
              <span>分批长任务</span>
              <div class="btn-tooltip">
                <div class="btn-tooltip-inner">
                  超长生成（如 60 页 PPT）先规划成清单，每轮只建一小批，断线从断点续跑
                  <div class="btn-tooltip-sub">
                    规避单轮输出过长把连接拖死；关时也会按「N 页/张/章」自动判定
                  </div>
                </div>
              </div>
            </button>
          </div>
          <div class="toolbar-right">
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
              title="停止"
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
  animation: typing-bounce 1.2s ease-in-out infinite;
}
.typing span:nth-child(2) {
  animation-delay: 0.18s;
}
.typing span:nth-child(3) {
  animation-delay: 0.36s;
}
@keyframes typing-bounce {
  0%, 80%, 100% {
    transform: translateY(0);
    opacity: 0.4;
  }
  40% {
    transform: translateY(-4px);
    opacity: 1;
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
.files-head {
  display: flex;
  align-items: center;
  gap: 5px;
  font-size: 11px;
  letter-spacing: 0.5px;
  color: var(--muted);
  margin-bottom: 8px;
}
.files-head :deep(svg) {
  color: var(--gold);
}
.files-list {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
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
  transition: border-color 0.15s, color 0.15s, background 0.15s;
}
.ta-btn:hover {
  border-color: var(--border);
  color: var(--text);
  background: var(--bg-soft);
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
.artifact-chip {
  position: relative;
  display: inline-flex;
  align-items: center;
  gap: 7px;
  max-width: 320px;
  padding: 6px 10px;
  background: var(--panel);
  border: 1px solid transparent;
  border-radius: 8px;
  color: var(--primary);
  font-size: 12.5px;
  cursor: pointer;
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.9), var(--shadow-sm);
  transition: border-color 0.15s, background 0.15s;
}
/* 琉璃流光描边：mask 镂空只留 1px 边缘（hover/active 时让位给实色反馈） */
.artifact-chip::before {
  content: "";
  position: absolute;
  inset: 0;
  border-radius: 8px;
  padding: 1px;
  background: var(--liuli-edge);
  -webkit-mask: linear-gradient(#000 0 0) content-box, linear-gradient(#000 0 0);
  -webkit-mask-composite: xor;
  mask-composite: exclude;
  pointer-events: none;
}
.artifact-chip:hover {
  border-color: var(--primary);
  background: var(--primary-soft);
}
.artifact-chip.active {
  border-color: var(--primary);
  background: var(--primary-soft);
}
.artifact-chip:hover::before,
.artifact-chip.active::before {
  display: none;
}
.artifact-chip .af-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-weight: 500;
}
.artifact-chip .af-open {
  opacity: 0.5;
  flex-shrink: 0;
}
.artifact-chip:hover .af-open {
  opacity: 0.9;
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
html[data-theme="dark"] .artifact-chip {
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
}
.send-btn:hover {
  background: var(--primary);
}
.send-btn:disabled {
  background: var(--border);
  cursor: not-allowed;
}
.send-btn.stop {
  background: var(--vermilion);
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
  justify-content: flex-end;
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
.spin {
  animation: spin 0.9s linear infinite;
}
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}
</style>
