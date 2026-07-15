<script setup lang="ts">
/**
 * 任务级对话面板(挂在任务详情抽屉里)。
 *
 * 协作者↔负责人↔主 Agent 的多轮微调通道,区别于「提交-打回」验收轮次:
 * - 增量拉取(afterId)+ WS 实时追加(store.lastTaskMessage);
 * - 发送失败(离线/隧道断)时,桌面端写入本地 outbox 断线缓存,重连后补传;
 * - 管理者可点「AI 回复」让主 Agent 依据对话上下文回一条建议(手动触发,控 token)。
 */
import { computed, nextTick, onMounted, ref, watch } from "vue";
import { Bot, LoaderCircle, Send } from "@lucide/vue";
import { invoke, isTauri } from "../../tauri";
import { useCollabStore } from "./stores/collab";
import { collabApi, fmtTime, getBase, getToken, type TaskMessage } from "./api";
import { toast } from "../../composables/useToast";

const props = defineProps<{ taskId: number }>();
const collab = useCollabStore();

const messages = ref<TaskMessage[]>([]);
const loading = ref(false);
const sending = ref(false);
const aiBusy = ref(false);
const draft = ref("");
const pendingCount = ref(0);
const listEl = ref<HTMLElement | null>(null);

const lastId = computed(() =>
  messages.value.length ? messages.value[messages.value.length - 1].id : 0
);

function scrollToBottom() {
  void nextTick(() => {
    if (listEl.value) listEl.value.scrollTop = listEl.value.scrollHeight;
  });
}

async function load(initial = false) {
  if (initial) loading.value = true;
  try {
    const more = await collabApi.taskMessages(props.taskId, lastId.value);
    if (more.length) {
      messages.value = [...messages.value, ...more];
      scrollToBottom();
    }
  } catch {
    /* 主机旧版无此端点/离线 → 静默 */
  } finally {
    loading.value = false;
  }
}

/** 桌面端:把 outbox 积压补传给主机(连上/发送前都试一把,幂等)。 */
async function flushOutbox() {
  if (!isTauri) return;
  try {
    const r = await invoke<{ sent: number; remaining: number }>(
      "collab_outbox_flush",
      { dir: "", baseUrl: getBase(), token: getToken() }
    );
    pendingCount.value = r?.remaining ?? 0;
    if (r && r.sent > 0) void load();
  } catch {
    /* 全离线:留队 */
  }
}

async function send() {
  const body = draft.value.trim();
  if (!body || sending.value) return;
  sending.value = true;
  try {
    await flushOutbox(); // 先补传积压,保持顺序
    const m = await collabApi.postTaskMessage(props.taskId, body);
    messages.value = [...messages.value, m];
    draft.value = "";
    scrollToBottom();
  } catch (e) {
    if (isTauri) {
      // 断线缓存:入 outbox,重连补传(幂等键由后端生成)。
      try {
        await invoke("collab_outbox_queue", {
          dir: "",
          payload: JSON.stringify({ taskId: props.taskId, body }),
        });
        pendingCount.value += 1;
        draft.value = "";
        toast.info("主机暂不可达,消息已存入待发送队列,连上后自动补传");
      } catch {
        toast.error((e as Error).message);
      }
    } else {
      toast.error((e as Error).message);
    }
  } finally {
    sending.value = false;
  }
}

async function aiReply() {
  if (aiBusy.value) return;
  aiBusy.value = true;
  try {
    const m = await collabApi.aiTaskReply(props.taskId);
    if (!messages.value.some((x) => x.id === m.id)) {
      messages.value = [...messages.value, m];
      scrollToBottom();
    }
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    aiBusy.value = false;
  }
}

// WS 推送实时追加(自己 POST 的那条已本地追加,靠 id 去重)。
watch(
  () => collab.lastTaskMessage,
  (m) => {
    if (!m || m.task_id !== props.taskId) return;
    if (messages.value.some((x) => x.id === m.id)) return;
    messages.value = [...messages.value, m];
    scrollToBottom();
  }
);

// 切换任务时重置重拉。
watch(
  () => props.taskId,
  () => {
    messages.value = [];
    void load(true);
    void flushOutbox();
  }
);

function roleLabel(m: TaskMessage): string {
  if (m.role === "ai") return "主Agent";
  if (m.role === "lead") return "负责人";
  if (m.role === "assignee") return "执行人";
  return "成员";
}
const isMe = (m: TaskMessage) =>
  m.author_name === (collab.user?.username ?? "") && m.role !== "ai";

onMounted(() => {
  void load(true);
  void flushOutbox();
});
</script>

<template>
  <section class="task-chat">
    <header class="chat-head">
      <h4>任务对话</h4>
      <span v-if="pendingCount > 0" class="pending-badge"
        >{{ pendingCount }} 条待发送</span
      >
      <button
        v-if="collab.canManage"
        class="ai-btn"
        :disabled="aiBusy"
        title="让主 Agent 依据对话上下文回一条建议(只出建议,不动状态)"
        @click="aiReply"
      >
        <LoaderCircle v-if="aiBusy" :size="13" class="spin" />
        <Bot v-else :size="13" />
        AI 回复
      </button>
    </header>

    <div ref="listEl" class="chat-list">
      <p v-if="loading" class="chat-empty">加载中…</p>
      <p v-else-if="!messages.length" class="chat-empty">
        还没有消息——有问题、要微调,在这里和负责人/主 Agent 直接聊。
      </p>
      <div
        v-for="m in messages"
        :key="m.id"
        class="msg"
        :class="{ mine: isMe(m), ai: m.role === 'ai' }"
      >
        <div class="msg-meta">
          <span class="msg-author">{{ m.author_name || "?" }}</span>
          <span class="msg-role">{{ roleLabel(m) }}</span>
          <span class="msg-time">{{ fmtTime(m.created_at) }}</span>
        </div>
        <div class="msg-body">{{ m.body }}</div>
      </div>
    </div>

    <div class="chat-input">
      <textarea
        v-model="draft"
        rows="2"
        placeholder="给负责人/主 Agent 留言…(Ctrl+Enter 发送)"
        @keydown.ctrl.enter.prevent="send"
        @keydown.meta.enter.prevent="send"
      />
      <button class="send-btn" :disabled="sending || !draft.trim()" @click="send">
        <LoaderCircle v-if="sending" :size="14" class="spin" />
        <Send v-else :size="14" />
      </button>
    </div>
  </section>
</template>

<style scoped>
.task-chat { display: flex; flex-direction: column; gap: 8px; }
.chat-head { display: flex; align-items: center; gap: 8px; }
.chat-head h4 { margin: 0; font-size: 13px; }
.pending-badge {
  font-size: 11px; padding: 1px 6px; border-radius: 8px;
  background: var(--warning-bg, #fef3c7); color: var(--warning-fg, #92400e);
}
.ai-btn {
  margin-left: auto; display: inline-flex; align-items: center; gap: 4px;
  font-size: 12px; padding: 2px 8px; border-radius: 6px;
  border: 1px solid var(--border-color, #e2e2e2); background: transparent; cursor: pointer;
}
.ai-btn:disabled { opacity: 0.6; cursor: default; }
.chat-list {
  max-height: 260px; overflow-y: auto; display: flex; flex-direction: column;
  gap: 8px; padding: 4px 2px;
}
.chat-empty { font-size: 12px; color: var(--text-tertiary, #999); margin: 4px 0; }
.msg { border-radius: 8px; padding: 6px 8px; background: var(--bg-secondary, #f5f5f5); }
.msg.mine { background: var(--accent-soft, #eef4ff); }
.msg.ai { background: var(--bg-tertiary, #f0eefc); }
.msg-meta { display: flex; gap: 6px; align-items: baseline; margin-bottom: 2px; }
.msg-author { font-size: 12px; font-weight: 600; }
.msg-role { font-size: 11px; color: var(--text-tertiary, #999); }
.msg-time { font-size: 11px; color: var(--text-tertiary, #999); margin-left: auto; }
.msg-body { font-size: 13px; white-space: pre-wrap; word-break: break-word; }
.chat-input { display: flex; gap: 6px; align-items: flex-end; }
.chat-input textarea {
  flex: 1; resize: vertical; min-height: 40px; font-size: 13px;
  border: 1px solid var(--border-color, #e2e2e2); border-radius: 8px; padding: 6px 8px;
  background: var(--bg-primary, #fff); color: inherit;
}
.send-btn {
  display: inline-flex; align-items: center; justify-content: center;
  width: 34px; height: 34px; border-radius: 8px; border: none; cursor: pointer;
  background: var(--accent, #4a6cf7); color: #fff;
}
.send-btn:disabled { opacity: 0.5; cursor: default; }
.spin { animation: spin 0.9s linear infinite; }
@keyframes spin { to { transform: rotate(360deg); } }
</style>
