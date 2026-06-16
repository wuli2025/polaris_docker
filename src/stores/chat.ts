import { defineStore } from "pinia";
import { ref, computed } from "vue";
import {
  chat as chatApi,
  convApi,
  listen,
  type ChatStreamEvent,
  type AttachedFile,
  type PermissionMode,
} from "../tauri";
import { useAppStore } from "./app";
import { useSessionsStore } from "../features/coworker/stores/sessions";

export interface Bubble {
  role: "user" | "assistant" | "tool";
  text: string;
  tool?: string;
  /** 工具输入摘要(命令/路径/检索词一行,后端 tool 事件给出) → pill 可展开看 */
  toolDetail?: string;
  /** 本条 assistant 消息生成的成品文件（绝对路径，正斜杠） */
  artifacts?: string[];
  /** 本条 user 消息携带的上传附件 */
  files?: AttachedFile[];
  /** 消息时间(ms);历史消息来自后端 created_at,实时消息为收到时刻 */
  at?: number;
}

/** 对话框只展示用户能直接打开的常见成品格式(与后端 chat.rs DISPLAY_EXTS 同步);
 *  脚本/配置等中间产物不展示。带尾随 `/` 的是「应用文件夹」chip, 一律保留。
 *  这道前端过滤主要兜底**旧历史**: 白名单上线前落库的 marker 里还混着中间文件。 */
const DISPLAY_EXTS = new Set([
  "md", "markdown", "txt", "pdf", "doc", "docx", "ppt", "pptx", "xls", "xlsx", "csv",
  "html", "htm",
  "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "avif", "ico",
  "mp4", "mov", "webm", "mkv", "avi", "mp3", "wav", "m4a", "aac", "flac", "ogg",
  "zip",
]);
function isDisplayableArtifact(path: string): boolean {
  if (path.endsWith("/")) return true; // 应用文件夹
  const name = path.split("/").pop() || path;
  const i = name.lastIndexOf(".");
  return i >= 0 && DISPLAY_EXTS.has(name.slice(i + 1).toLowerCase());
}

/** 解析正文里夹带的产物清单 marker，返回剥离 marker 后的纯文本 + 路径数组 */
export function parseArtifacts(content: string): {
  text: string;
  artifacts: string[];
} {
  const m = content.match(/<!--POLARIS_ARTIFACTS:(\[[\s\S]*?\])-->/);
  if (!m) return { text: content, artifacts: [] };
  let arr: string[] = [];
  try {
    arr = JSON.parse(m[1]);
  } catch {
    arr = [];
  }
  const text = content.replace(m[0], "").trimEnd();
  return { text, artifacts: arr.filter(isDisplayableArtifact) };
}

/**
 * 对话运行时 store —— 多开的核心。
 *
 * 每个对话各自维护 bubbles / sending / reqId；流式事件在 app 级监听一次，
 * 按 `conversationId` 路由进各自缓冲。这样切到任意对话都能看到它的实时进度，
 * 多个任务可同时在后台流式推进（互不干扰），切走也不会"停"。
 */
export const useChatStore = defineStore("chatRuntime", () => {
  const byConv = ref<Record<string, Bubble[]>>({});
  const reqByConv = ref<Record<string, string>>({});
  const sendingByConv = ref<Record<string, boolean>>({});
  const loadedByConv = ref<Record<string, boolean>>({});
  // 本地「最近活跃时间」：发送/结束时打点。后端 updatedAt 在会话内不变，
  // 用它让刚交互过的对话在侧栏冒泡到最上（仿 Codex 最近对话置顶）。
  const activeAtByConv = ref<Record<string, number>>({});
  // 最近一轮注入的估算 input token（后端 meta 事件给出）。分批编排据此自适应批量。
  const tokensByConv = ref<Record<string, number>>({});
  // 等待某对话「本轮 done」的 resolver 队列（分批编排循环逐轮 await）。
  const doneWaiters: Record<string, Array<() => void>> = {};
  // 流式监听的「就绪 promise」。缓存它(而非一个 started 布尔), 让所有调用方 await 的是
  // 「监听器真正挂上」这一刻 —— 而不是仅仅把标志位置真。否则首条消息的 delta 可能在
  // listen() 完成注册之前就到达 → 丢帧(现象: 第一次发消息看不到流式输出, 但后台照常运行)。
  let initPromise: Promise<void> | null = null;

  /** 等到指定对话「本轮跑完(done)」。当前不在发送态则立即兑现。 */
  function waitForDone(convId: string): Promise<void> {
    if (!sendingByConv.value[convId]) return Promise.resolve();
    return new Promise<void>((resolve) => {
      (doneWaiters[convId] ??= []).push(resolve);
    });
  }
  /** 唤醒并清空某对话的 done 等待队列。done / cancel / 发送失败都必须调用,
   *  否则正在 await waitForDone 的分批编排循环会永久挂起(进度条卡死)。 */
  function wakeWaiters(convId: string) {
    const waiters = doneWaiters[convId];
    if (waiters && waiters.length) {
      doneWaiters[convId] = [];
      for (const w of waiters) w();
    }
  }
  function inputTokens(convId: string | null): number {
    if (!convId) return 0;
    return tokensByConv.value[convId] ?? 0;
  }

  function bubblesFor(convId: string | null): Bubble[] {
    if (!convId) return [];
    return byConv.value[convId] ?? [];
  }
  function isSending(convId: string | null): boolean {
    return !!(convId && sendingByConv.value[convId]);
  }
  /** 当前所有「正在生成」的对话 id —— 全局任务中心据此把 AI 的后台生成
   *  (切走仍在跑的 PPT / 长任务等)挂到右下角浮层。 */
  const runningConvIds = computed(() =>
    Object.keys(sendingByConv.value).filter((id) => sendingByConv.value[id]),
  );
  function activityAt(convId: string | null): number {
    if (!convId) return 0;
    return activeAtByConv.value[convId] ?? 0;
  }
  function touchActivity(convId: string) {
    if (!convId) return;
    activeAtByConv.value[convId] = Date.now();
  }
  function ensureArr(convId: string): Bubble[] {
    if (!byConv.value[convId]) byConv.value[convId] = [];
    return byConv.value[convId];
  }
  function pushBubble(convId: string, b: Bubble) {
    ensureArr(convId).push(b);
  }

  // 历史加载失败的对话集合:别假装是空对话,对话区给「重试」入口
  const historyErrorByConv = ref<Record<string, string>>({});
  function historyError(convId: string | null): string | null {
    if (!convId) return null;
    return historyErrorByConv.value[convId] ?? null;
  }

  async function loadHistory(convId: string | null, force = false) {
    if (!convId) return;
    // 正在运行的对话别用历史覆盖实时气泡
    if (sendingByConv.value[convId]) return;
    if (loadedByConv.value[convId] && !force) return;
    try {
      const msgs = await convApi.getMessages(convId);
      byConv.value[convId] = msgs.map((m) => {
        const at = m.createdAt > 1e12 ? m.createdAt : m.createdAt * 1000;
        if (m.role === "assistant") {
          const { text, artifacts } = parseArtifacts(m.content);
          return { role: m.role, text, artifacts, at } as Bubble;
        }
        return { role: m.role, text: m.content, at } as Bubble;
      });
      loadedByConv.value[convId] = true;
      delete historyErrorByConv.value[convId];
    } catch (e: any) {
      byConv.value[convId] = [];
      historyErrorByConv.value[convId] = e?.message ?? String(e);
    }
  }

  /** 发送一条消息：推入 user 气泡 + 调后端，记录 reqId/sending（不阻塞，多开） */
  async function send(
    convId: string,
    prompt: string,
    displayText: string,
    files: AttachedFile[] | undefined,
    opts: {
      permissionMode: PermissionMode;
      skillIds: string[];
      goal?: string;
      dynamicWorkflow?: boolean;
      useKb?: boolean;
      batchBuild?: boolean;
      batchSize?: number;
      agentMode?: string;
    }
  ) {
    // 关键: 先确保流式监听已挂上, 否则本轮的 delta 可能早于监听器注册而丢失
    // —— 现象正是「第一次发消息看不到输出, 但后台仍在运行」。尤其是从「更多」各工坊
    // (Deck/Web/视频/自媒体/自动化)直接发起时, ChatPanel 尚未挂载、init 从未被调用。
    await init();
    const sessions = useSessionsStore();
    const arr = ensureArr(convId);
    arr.push({
      role: "user",
      text: displayText,
      files: files && files.length ? files : undefined,
      at: Date.now(),
    });
    sendingByConv.value[convId] = true;
    touchActivity(convId);
    sessions.start(convId, displayText.slice(0, 18));
    try {
      const reqId = await chatApi.send({
        prompt,
        permissionMode: opts.permissionMode,
        skillIds: opts.skillIds,
        goal: opts.goal,
        dynamicWorkflow: opts.dynamicWorkflow,
        useKb: opts.useKb,
        batchBuild: opts.batchBuild,
        batchSize: opts.batchSize,
        agentMode: opts.agentMode,
        conversationId: convId,
      });
      reqByConv.value[convId] = reqId;
    } catch (e: any) {
      const { humanizeError } = await import("../lib/humanizeError");
      arr.push({
        role: "assistant",
        text: `[发送失败] ${humanizeError(e)}`,
        at: Date.now(),
      });
      sendingByConv.value[convId] = false;
      sessions.finish(convId);
      wakeWaiters(convId); // 否则分批循环 await 永挂
    }
  }

  async function cancel(convId: string | null) {
    if (!convId) return;
    const sessions = useSessionsStore();
    const req = reqByConv.value[convId];
    if (req) {
      try {
        await chatApi.cancel(req);
      } catch {
        /* ignore */
      }
    }
    sendingByConv.value[convId] = false;
    delete reqByConv.value[convId];
    touchActivity(convId);
    sessions.finish(convId);
    wakeWaiters(convId); // 取消后唤醒分批循环, 让它看到 !isRunning 自行收尾
  }

  /** app 级初始化：注册一次流式监听，按 conversationId 路由进各自缓冲。
   *  返回缓存的就绪 promise：重复调用只注册一次，且每个调用方都能 await 到「监听已挂上」。 */
  function init(): Promise<void> {
    if (initPromise) return initPromise;
    initPromise = listen<ChatStreamEvent>("chat:stream", (ev) => {
      const cid = ev.conversationId;
      if (!cid) return; // 无会话归属的事件无法路由（理论上不会出现）
      const arr = ensureArr(cid);
      if (ev.kind === "delta") {
        const last = arr[arr.length - 1];
        if (last && last.role === "assistant") last.text += ev.text ?? "";
        else arr.push({ role: "assistant", text: ev.text ?? "", at: Date.now() });
      } else if (ev.kind === "tool") {
        arr.push({
          role: "tool",
          text: `调用工具:${ev.tool ?? "(unknown)"}`,
          tool: ev.tool,
          toolDetail: ev.text || undefined,
          at: Date.now(),
        });
      } else if (ev.kind === "artifact") {
        const path = ev.text;
        if (path) {
          let target: Bubble | undefined;
          for (let i = arr.length - 1; i >= 0; i--) {
            if (arr[i].role === "assistant") {
              target = arr[i];
              break;
            }
          }
          if (!target) {
            target = { role: "assistant", text: "", artifacts: [] };
            arr.push(target);
          }
          if (!target.artifacts) target.artifacts = [];
          if (!target.artifacts.includes(path)) target.artifacts.push(path);
        }
      } else if (ev.kind === "meta") {
        // 上下文预算自检：后端估算的本轮 input token 数（纯数字文本）
        const n = parseInt(ev.text ?? "", 10);
        if (!Number.isNaN(n)) tokensByConv.value[cid] = n;
      } else if (ev.kind === "error") {
        // stderr 行 / 退出错误：仅展示，不作为终态（终态由 done 处理）
        arr.push({ role: "assistant", text: `[错误] ${ev.text ?? ""}` });
      } else if (ev.kind === "done") {
        // 终态：结束运行态 + 工位会话；若用户不在看该对话则打墨蓝未读点
        sendingByConv.value[cid] = false;
        delete reqByConv.value[cid];
        touchActivity(cid);
        // 本轮的实时气泡(含 [错误] 等未持久化的合成气泡)即为该对话的权威视图,
        // 标记 loaded 防止之后切回时 loadHistory 用后端副本覆盖、丢掉这些气泡。
        loadedByConv.value[cid] = true;
        const app = useAppStore();
        const sessions = useSessionsStore();
        sessions.finish(cid);
        app.markUnread(cid);
        // 唤醒分批编排循环：本轮已结束，可读清单决定续不续
        wakeWaiters(cid);
      }
    }).then(() => undefined);
    return initPromise;
  }

  return {
    byConv,
    bubblesFor,
    isSending,
    runningConvIds,
    activityAt,
    pushBubble,
    loadHistory,
    historyError,
    send,
    cancel,
    init,
    waitForDone,
    inputTokens,
  };
});
