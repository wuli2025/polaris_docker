<script setup lang="ts">
/**
 * 全局任务中心 —— 常驻右下角的后台任务浮层(苹果琉璃材质)。
 *
 * 解决「点了盘点/建索引/智能归类/构建知识网,或让 AI 在后台生成 PPT,一切走那个页面
 * 就好像停了、也看不到进度」:这些任务的真身都是后端后台线程 / 多开对话(见
 * stores/fileTasks.ts、stores/kb.ts、stores/chat.ts),本组件常驻挂在 App.vue,
 * 从三个全局 store 读运行态 → 无论当前在哪个视图,只要有任务在跑就在这里显示
 * 「还在跑 + 实时进度」,点一下即可跳回对应页面 / 对话查看。空闲时自动隐藏,零干扰。
 *
 * 完成提示:任务从「在跑」列表消失(done / error)时,统一弹一条 toast 告知结果
 * (见下方 watch(rows))——这样切走也能在右上角收到「XX 已完成」。
 *
 * 材质:半透明琉璃(translucent + backdrop blur),透出底下内容、避免遮挡,
 * 兼顾高级感;深浅主题各自换 tint(见 style 末尾的 data-theme 覆盖)。
 */
import { computed, ref, watch } from "vue";
import { LoaderCircle, ChevronDown, Activity, Sparkles, X } from "@lucide/vue";
import OrbitSpinner from "./icons/OrbitSpinner.vue";
import { useFileTasksStore } from "../stores/fileTasks";
import { useKbStore } from "../stores/kb";
import { useChatStore } from "../stores/chat";
import { useAppStore, type ViewKey } from "../stores/app";
import { toast } from "../composables/useToast";

const tasks = useFileTasksStore();
const kb = useKbStore();
const chat = useChatStore();
const app = useAppStore();

const expanded = ref(true);

interface Row {
  key: string;
  label: string;
  detail: string;
  view: ViewKey;
  /** AI 后台任务:跳转到对应对话而非仅切视图 */
  convId?: string;
  /** 是 AI 生成类任务(图标用 Sparkles) */
  ai?: boolean;
  /** 可停止/关闭(文件任务走 fable_cancel;AI 对话走 chat.cancel)。构建知识网暂无取消入口。 */
  stoppable?: boolean;
}

// 汇总三个 store 里所有「正在跑」的任务成统一列表。
// 顺序:AI 生成(用户最关心的「我刚让它做的」)→ 文件任务 → 构建知识网。
const rows = computed<Row[]>(() => {
  const out: Row[] = [];
  for (const cid of chat.runningConvIds) {
    // 你正盯着看的这条对话不算「后台」任务——普通对话边聊边生成属于前台,不该弹浮层叨扰。
    // 只有当你切走了(去别的视图 / 别的对话),它还在生成,才算真正的后台任务显示出来。
    if (app.view === "chat" && app.currentConvId === cid) continue;
    out.push({
      key: "chat:" + cid,
      label: app.convTitle(cid) || "AI 对话",
      detail: "正在生成…",
      view: "chat",
      convId: cid,
      ai: true,
      stoppable: true,
    });
  }
  for (const t of tasks.activeList) {
    out.push({
      key: "ft:" + t.id,
      label: t.label,
      detail: t.detail,
      view: "file_center",
      stoppable: true,
    });
  }
  if (kb.compiling) {
    const last = kb.compileLog.length ? kb.compileLog[kb.compileLog.length - 1] : "";
    out.push({
      key: "kb:compile",
      label: "构建知识网",
      detail: (kb.compileMsg || last || "进行中…").replace(/^[▸·📄⚠]\s*/, ""),
      view: "wiki",
    });
  }
  return out;
});

const show = computed(() => rows.value.length > 0);
const count = computed(() => rows.value.length);

// ── 完成提示 ──────────────────────────────────────────────
// 监听「在跑」列表:某任务 key 从列表里消失 = 它结束了(done / error)→ 弹一条 toast。
// 集中在这一处,自动覆盖三类任务,无需各 store 各写一遍通知。
const seen = new Map<string, string>(); // key -> label(消失时拿来组装文案)
watch(
  rows,
  (cur) => {
    const curKeys = new Set(cur.map((r) => r.key));
    for (const [key, label] of seen) {
      if (!curKeys.has(key)) notifyDone(key, label);
    }
    seen.clear();
    for (const r of cur) seen.set(r.key, r.label);
  },
  { flush: "post" },
);

function notifyDone(key: string, label: string) {
  if (key.startsWith("ft:")) {
    const id = key.slice(3) as keyof typeof tasks.detail;
    const failed = tasks.failed[id];
    const msg = tasks.detail[id] || `${label} 已完成`;
    failed ? toast.error(msg) : toast.success(msg);
  } else if (key === "kb:compile") {
    const msg = kb.compileMsg || "构建知识网完成";
    /失败|中断/.test(msg) ? toast.error(msg) : toast.success(msg);
  } else if (key.startsWith("chat:")) {
    const cid = key.slice(5);
    // 正盯着这个对话看 = 已经看到结果,不再叨扰;切走了才提示。
    const viewing = app.view === "chat" && app.currentConvId === cid;
    if (!viewing) toast.success(`${label} · 已生成完成`);
  }
}

function goto(r: Row) {
  if (r.convId) app.openConversationById(r.convId);
  else app.setView(r.view);
}
// 停止/关闭某任务:文件任务走协作式取消(盘点/索引真停,索引可再点继续);AI 对话走 chat.cancel。
function stop(r: Row) {
  if (r.key.startsWith("ft:")) tasks.cancel(r.key.slice(3) as Parameters<typeof tasks.cancel>[0]);
  else if (r.key.startsWith("chat:") && r.convId) chat.cancel(r.convId);
}
</script>

<template>
  <transition name="tc">
    <div v-if="show" class="task-center" :class="{ collapsed: !expanded }">
      <button class="tc-head" @click="expanded = !expanded">
        <span class="tc-pulse"><Activity :size="14" :stroke-width="2" /></span>
        <span class="tc-title">后台任务</span>
        <span class="tc-count">{{ count }}</span>
        <ChevronDown class="tc-chev" :class="{ flip: !expanded }" :size="15" :stroke-width="2" />
      </button>
      <div v-show="expanded" class="tc-body">
        <div v-for="r in rows" :key="r.key" class="tc-row" @click="goto(r)" :title="'点击查看 · ' + r.label">
          <component :is="r.ai ? Sparkles : LoaderCircle" :size="15" class="tc-spin" :class="{ ai: r.ai }" />
          <div class="tc-main">
            <div class="tc-label">{{ r.label }}</div>
            <div class="tc-detail">{{ r.detail }}</div>
          </div>
          <button
            v-if="r.stoppable"
            class="tc-stop"
            :title="r.ai ? '停止生成' : '停止任务(盘点/索引可再点继续)'"
            @click.stop="stop(r)"
          >
            <X :size="13" :stroke-width="2.2" />
          </button>
        </div>
      </div>
      <!-- 收起态:只显示一行最紧凑的胶囊(数量 + 第一个任务名) -->
      <div v-show="!expanded" class="tc-mini" @click="expanded = true">
        <OrbitSpinner :size="13" />
        <span>{{ rows[0]?.label }}{{ count > 1 ? ` 等 ${count} 项` : "" }}</span>
      </div>
    </div>
  </transition>
</template>

<style scoped>
.task-center {
  /* 琉璃材质变量(默认浅色主题);深色主题在文件末尾按 data-theme 覆盖。 */
  --glass-tint: rgba(252, 251, 247, 0.55);
  --glass-edge: rgba(120, 108, 86, 0.2);
  --glass-hi: rgba(255, 255, 255, 0.7);
  --glass-shadow: 0 18px 50px rgba(60, 50, 30, 0.18);

  position: fixed;
  right: 18px;
  bottom: 18px;
  z-index: 9990;
  width: 290px;
  /* 半透明 tint + 顶部高光斜射 → 玻璃质感;blur 把底下内容糊成毛玻璃、避免遮挡 */
  background:
    linear-gradient(155deg, var(--glass-hi) 0%, transparent 42%),
    var(--glass-tint);
  border: 1px solid var(--glass-edge);
  border-radius: 17px;
  /* 外阴影撑起悬浮感 + 内顶高光做出玻璃「唇边」 */
  box-shadow: var(--glass-shadow), inset 0 1px 0 var(--glass-hi);
  overflow: hidden;
  backdrop-filter: blur(30px) saturate(180%);
  -webkit-backdrop-filter: blur(30px) saturate(180%);
}
.task-center.collapsed {
  width: auto;
}
.tc-head {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 10px 13px;
  background: transparent;
  border: none;
  cursor: pointer;
  color: var(--muted, #a8a8a4);
  font-size: 12.5px;
  letter-spacing: 0.04em;
}
.collapsed .tc-head {
  display: none;
}
.tc-pulse {
  display: inline-flex;
  color: var(--primary, #d4b06a);
  animation: tc-breathe 2s ease-in-out infinite;
}
@keyframes tc-breathe {
  0%, 100% { opacity: 0.55; }
  50% { opacity: 1; }
}
.tc-title {
  font-weight: 650;
  color: var(--ink, #e8e8e6);
}
.tc-count {
  min-width: 18px;
  height: 18px;
  padding: 0 5px;
  border-radius: 9px;
  background: var(--primary, #d4b06a);
  color: #1a1a1a;
  font-size: 11px;
  font-weight: 700;
  display: inline-flex;
  align-items: center;
  justify-content: center;
}
.tc-chev {
  margin-left: auto;
  color: var(--muted, #888);
  transition: transform 0.2s ease;
}
.tc-chev.flip {
  transform: rotate(180deg);
}
.tc-body {
  padding: 2px 8px 8px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.tc-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 10px;
  border-radius: 11px;
  cursor: pointer;
  transition: background 0.15s ease;
}
.tc-row:hover {
  background: var(--primary-soft, rgba(212, 176, 106, 0.14));
}
.tc-main {
  min-width: 0;
  flex: 1;
}
.tc-label {
  font-size: 12.5px;
  font-weight: 600;
  color: var(--ink, #e8e8e6);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.tc-detail {
  font-size: 11px;
  color: var(--muted, #999);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  margin-top: 1px;
}
.tc-spin {
  color: var(--primary, #d4b06a);
  animation: tc-rot 0.9s linear infinite;
  flex-shrink: 0;
}
/* AI 生成任务:用呼吸闪烁的火花替代旋转,区分出「模型在想」 */
.tc-spin.ai {
  animation: tc-breathe 1.6s ease-in-out infinite;
}
@keyframes tc-rot {
  to { transform: rotate(360deg); }
}
/* 停止/关闭任务:平时低调,hover 转危险红,明确「这会停掉它」 */
.tc-stop {
  flex-shrink: 0;
  background: transparent;
  border: 1px solid var(--glass-edge);
  border-radius: 8px;
  padding: 5px;
  color: var(--muted, #999);
  cursor: pointer;
  display: inline-flex;
  transition: color 0.15s ease, border-color 0.15s ease, background 0.15s ease;
}
.tc-stop:hover {
  color: #e5484d;
  border-color: #e5484d;
  background: rgba(229, 72, 77, 0.1);
}
.tc-mini {
  display: flex;
  align-items: center;
  gap: 7px;
  padding: 9px 14px;
  cursor: pointer;
  color: var(--ink, #e8e8e6);
  font-size: 12px;
  font-weight: 600;
  white-space: nowrap;
}
.tc-enter-active,
.tc-leave-active {
  transition: opacity 0.25s ease, transform 0.25s ease;
}
.tc-enter-from,
.tc-leave-to {
  opacity: 0;
  transform: translateY(10px);
}

/* 深色主题(石墨 / 极光墨黑):换成深色半透明玻璃,顶高光收弱避免发灰。 */
:global(html[data-theme="dark"]) .task-center,
:global(html[data-theme="aurora-dark"]) .task-center {
  --glass-tint: rgba(32, 32, 36, 0.5);
  --glass-edge: rgba(255, 255, 255, 0.12);
  --glass-hi: rgba(255, 255, 255, 0.1);
  --glass-shadow: 0 18px 50px rgba(0, 0, 0, 0.5);
}
</style>
