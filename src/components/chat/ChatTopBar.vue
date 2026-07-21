<script setup lang="ts">
// 顶栏(自 ChatPanel 拆出,逻辑原样搬移):对话标题 / 就地重命名 / 「更多」菜单 /
// 复制反馈小提示 / 文件抽屉开关。状态全部下沉本组件;父层只经 expose 的 flash()
// 借用复制反馈提示(如「复制回答」)。
import { ref, computed, nextTick, onMounted, onBeforeUnmount } from "vue";
import {
  Pin,
  PinOff,
  Ellipsis,
  PencilLine,
  Copy,
  FileText,
  Trash2,
  Check,
  PanelRightOpen,
  PanelRightClose,
} from "@lucide/vue";
import { convApi, type Message } from "../../tauri";
import { useAppStore } from "../../stores/app";
import { useChatStore } from "../../stores/chat";

const app = useAppStore();
const chatStore = useChatStore();

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

// 父层(如消息区「复制回答」)借用同一个复制反馈提示
defineExpose({ flash: flashCopied });
</script>

<template>
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
</template>

<style scoped>
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
</style>
