<script setup lang="ts">
import { computed, onMounted, onBeforeUnmount, ref } from "vue";
import {
  Clock,
  Plus,
  Play,
  SquarePen,
  Trash2,
  Newspaper,
  BookMarked,
  Tv,
  Sparkles,
  Clapperboard,
  Cpu,
  MessageCircle,
  Folder,
  Telescope,
  Repeat,
  LoaderCircle,
  X,
  CircleStop,
  Square,
  Moon,
  Power,
  ChevronDown,
} from "@lucide/vue";
import OrbitSpinner from "./icons/OrbitSpinner.vue";
import { useAutomationStore, type AutomationFlow } from "../stores/automation";
import { useAppStore } from "../stores/app";
import { useChatStore } from "../stores/chat";
import { invoke, listen } from "../tauri";

const auto = useAutomationStore();
const app = useAppStore();
const chat = useChatStore();

// ── 自动做梦 · 每日晨报（回声层）──
interface DreamLog { ts: number; day: string; episodes: number; summary: string }
interface EchoStatus {
  enabled: boolean;
  hour: number;
  run_on_boot: boolean;
  last_dream_day: string;
  dreaming: boolean;
  memory_count: number;
  briefing_today: number;
  log: DreamLog[];
}
const echo = ref<EchoStatus | null>(null);
// 卡片收起状态（纯前端，持久化）——不影响是否开启，只折叠这张卡的占位
const dreamCollapsed = ref(localStorage.getItem("polaris.dreamCollapsed") === "1");
function toggleDream() {
  dreamCollapsed.value = !dreamCollapsed.value;
  localStorage.setItem("polaris.dreamCollapsed", dreamCollapsed.value ? "1" : "0");
}
async function loadEcho() {
  try {
    echo.value = await invoke<EchoStatus>("echo_status");
  } catch (e) {
    console.error("加载回声层状态失败", e);
  }
}
async function setEcho(args: { enabled?: boolean; hour?: number; runOnBoot?: boolean }) {
  try {
    echo.value = await invoke<EchoStatus>("echo_set", args);
  } catch (e) {
    console.error("保存回声层设置失败", e);
  }
}
async function dreamNow() {
  try {
    await invoke("echo_dream_now");
    if (echo.value) echo.value.dreaming = true;
  } catch (e) {
    console.error(e);
  }
}
async function briefNow() {
  try {
    await invoke("echo_briefing_run");
    if (echo.value) echo.value.dreaming = true;
  } catch (e) {
    console.error(e);
  }
}
const dreamHours = Array.from({ length: 24 }, (_, i) => i);

// echo:dream 监听器的解绑句柄。此前整个组件无 onBeforeUnmount,每次进出本视图都新挂
// 一个监听且不解绑 → 监听器/回调逐周累积(Docker/Web 模式进无界 Set 更严重)。
let unlistenDream: (() => void) | null = null;

onMounted(async () => {
  if (!app.projects.length) app.refreshProjects();
  auto.startScheduler();
  loadEcho();
  // 桌面 / Docker 两条路径都直接回传 payload 本体(见 tauri.ts),读 p.kind;
  // 旧代码读 p.payload.kind 多包一层取不到,且 isTauri 闸把 Docker 一并误杀。
  unlistenDream = await listen("echo:dream", (p: any) => {
    const k = p?.kind ?? p?.payload?.kind;
    if (k === "done" || k === "error") loadEcho();
  });
});

onBeforeUnmount(() => {
  // 对称回收:解绑 echo:dream 监听,杜绝重复挂载累积。
  // 注意:**不**停 auto.stopScheduler() —— 调度定时器是 app 级后台设施(切走本视图后
  // 计划任务仍须按时触发),startScheduler 自身幂等不会叠加,故让单个 app 生命周期定时器
  // 常驻才是正确行为;停掉会导致离开本页后自动化静默失效。
  if (unlistenDream) {
    unlistenDream();
    unlistenDream = null;
  }
});

const ICONS: Record<string, any> = {
  newspaper: Newspaper,
  "book-marked": BookMarked,
  tv: Tv,
  sparkles: Sparkles,
  clapperboard: Clapperboard,
  cpu: Cpu,
  "message-circle": MessageCircle,
};
function iconOf(f: AutomationFlow) {
  return ICONS[f.icon] || Sparkles;
}

function scheduleLabel(f: AutomationFlow): string {
  const s = f.schedule;
  if (s.kind === "daily") return `每天 ${s.time}`;
  if (s.kind === "interval") return `每 ${s.everyHours} 小时`;
  return "手动触发";
}
function projectLabel(f: AutomationFlow): string {
  if (!f.projectId) return "当前项目";
  return app.projects.find((p) => p.id === f.projectId)?.name || "未知项目";
}
function running(f: AutomationFlow): boolean {
  return !!f.lastConvId && chat.isSending(f.lastConvId);
}

async function run(f: AutomationFlow) {
  await auto.runFlow(f);
}
function edit(f: AutomationFlow) {
  auto.openEdit(f);
}
function remove(f: AutomationFlow) {
  if (confirm(`删除自动化「${f.name}」？`)) auto.removeFlow(f.id);
}

// ── 缩小版对话框（运行进度）──
const activeBubbles = computed(() => chat.bubblesFor(auto.activeConvId));
const activeRunning = computed(
  () => !!auto.activeConvId && chat.isSending(auto.activeConvId)
);
function closePanel() {
  auto.activeConvId = null;
}
function stopRun() {
  if (auto.activeConvId) chat.cancel(auto.activeConvId);
}
function stopFlow(f: AutomationFlow) {
  if (f.lastConvId) chat.cancel(f.lastConvId);
}
</script>

<template>
  <div class="auto-wrap">
    <div class="auto-main" :class="{ 'with-panel': auto.activeConvId }">
      <!-- 头部 -->
      <header class="head">
        <div class="title-row">
          <Clock :size="20" :stroke-width="1.7" class="t-icon" />
          <h1>自动化</h1>
        </div>
        <p class="lead">
          把一段编排好的任务交给本机 Claude 定时/循环跑：选方向 → 深度搜索 →
          仿知识库风格成稿 → 多维评审 → 落到草稿箱（不自动发布，由你过目后再发）。
        </p>
      </header>

      <!-- 自动做梦 · 每日晨报（回声层）-->
      <section v-if="echo" class="dream-card" :class="{ collapsed: dreamCollapsed }">
        <div class="dc-head">
          <span class="dc-ic"><Moon :size="15" :stroke-width="1.8" color="#fff" /></span>
          <div class="dc-tt">
            <span class="dc-name">自动做梦 · 每日晨报</span>
            <span v-if="!dreamCollapsed" class="dc-sub"
              >每天自动整理你的对话与新资料，归类进记忆，并据新内容给出工程化建议 —— 别的 AI 把功能做得更强，我们让 AI 更懂你。</span
            >
          </div>
          <label class="dc-switch" :title="echo.enabled ? '已开启' : '已关闭'">
            <input
              type="checkbox"
              :checked="echo.enabled"
              @change="setEcho({ enabled: ($event.target as HTMLInputElement).checked })"
            />
            <span class="dc-track"></span>
          </label>
          <button
            class="dc-fold"
            :title="dreamCollapsed ? '展开' : '收起'"
            @click="toggleDream"
          >
            <ChevronDown :size="16" :stroke-width="2" :class="{ up: !dreamCollapsed }" />
          </button>
        </div>

        <div v-if="echo.enabled && !dreamCollapsed" class="dc-body">
          <div class="dc-row">
            <span class="dc-label"><Clock :size="13" :stroke-width="1.7" /> 每天执行时间</span>
            <select
              class="dc-select"
              :value="echo.hour"
              @change="setEcho({ hour: Number(($event.target as HTMLSelectElement).value) })"
            >
              <option v-for="h in dreamHours" :key="h" :value="h">
                {{ String(h).padStart(2, "0") }}:00
              </option>
            </select>
          </div>
          <div class="dc-row">
            <span class="dc-label"><Power :size="13" :stroke-width="1.7" /> 开机补做</span>
            <label class="dc-mini-switch" title="错过固定时间（如开机前）则开机后自动补一次">
              <input
                type="checkbox"
                :checked="echo.run_on_boot"
                @change="setEcho({ runOnBoot: ($event.target as HTMLInputElement).checked })"
              />
              <span class="dc-track sm"></span>
            </label>
          </div>
          <div class="dc-stats">
            <span class="dc-stat">记忆 <b>{{ echo.memory_count }}</b> 条</span>
            <span class="dc-stat">今日建议 <b>{{ echo.briefing_today }}</b> 条</span>
            <span v-if="echo.last_dream_day" class="dc-stat">上次 {{ echo.last_dream_day }}</span>
          </div>
          <div class="dc-act">
            <button class="dc-btn" :disabled="echo.dreaming" @click="dreamNow">
              <component
                :is="echo.dreaming ? LoaderCircle : Moon"
                :size="13"
                :stroke-width="1.9"
                :class="{ spin: echo.dreaming }"
              />
              {{ echo.dreaming ? "处理中…" : "现在做一次梦" }}
            </button>
            <button class="dc-btn ghost" :disabled="echo.dreaming" @click="briefNow">
              <Sparkles :size="13" :stroke-width="1.9" /> 现在生成晨报
            </button>
          </div>
          <div v-if="echo.log.length" class="dc-log">
            <div v-for="(l, i) in echo.log.slice(0, 3)" :key="i" class="dc-log-line">
              <span class="dll-day">{{ l.day }}</span>{{ l.summary }}
            </div>
          </div>
        </div>
      </section>

      <!-- 流程卡片 -->
      <div class="grid">
        <button class="card new" @click="auto.openCreate()">
          <span class="new-plus"><Plus :size="22" :stroke-width="1.8" /></span>
          <span class="new-text">新建自动化</span>
        </button>

        <div v-for="f in auto.flows" :key="f.id" class="card flow">
          <div class="c-head">
            <span class="c-icon" :style="{ background: f.color }">
              <component :is="iconOf(f)" :size="15" :stroke-width="1.8" color="#fff" />
            </span>
            <span class="c-name" :title="f.name">{{ f.name }}</span>
            <span v-if="running(f)" class="run-tag">
              <OrbitSpinner :size="12" /> 运行中
            </span>
          </div>
          <p class="c-desc">{{ f.description || "（无描述）" }}</p>
          <div class="c-meta">
            <span class="meta"><Folder :size="12" :stroke-width="1.6" /> {{ projectLabel(f) }}</span>
            <span class="meta"><Clock :size="12" :stroke-width="1.6" /> {{ scheduleLabel(f) }}</span>
            <span v-if="f.deepResearch" class="meta"><Telescope :size="12" :stroke-width="1.6" /> 深度</span>
            <span v-if="f.loopCount > 1" class="meta"><Repeat :size="12" :stroke-width="1.6" /> ×{{ f.loopCount }}</span>
          </div>
          <div class="c-act">
            <!-- 运行中:卡片上直接给停止入口(不依赖右侧面板还开着) -->
            <button v-if="running(f)" class="run-btn stop" @click="stopFlow(f)">
              <Square :size="12" :stroke-width="2" /> 停止
            </button>
            <button v-else class="run-btn" @click="run(f)">
              <Play :size="13" :stroke-width="2" /> 运行
            </button>
            <button class="mini-btn" title="编辑" @click="edit(f)">
              <SquarePen :size="14" :stroke-width="1.7" />
            </button>
            <button class="mini-btn danger" title="删除" @click="remove(f)">
              <Trash2 :size="14" :stroke-width="1.7" />
            </button>
          </div>
        </div>
      </div>
    </div>

    <!-- 缩小版对话框：运行进度 -->
    <aside v-if="auto.activeConvId" class="run-panel">
      <div class="rp-head">
        <span class="rp-title">
          <component :is="activeRunning ? LoaderCircle : Sparkles" :size="14" :stroke-width="1.9" :class="{ spin: activeRunning }" />
          运行进度
        </span>
        <div class="rp-act">
          <button v-if="activeRunning" class="rp-stop" title="停止" @click="stopRun">
            <CircleStop :size="15" :stroke-width="1.8" />
          </button>
          <button class="rp-close" title="收起" @click="closePanel">
            <X :size="16" :stroke-width="1.7" />
          </button>
        </div>
      </div>
      <div class="rp-body">
        <div
          v-for="(b, i) in activeBubbles"
          :key="i"
          class="bubble"
          :class="b.role"
        >
          <template v-if="b.role === 'tool'">
            <span class="tool-pill">{{ b.text }}</span>
          </template>
          <template v-else>
            <div class="b-text">{{ b.text }}</div>
            <div v-if="b.artifacts && b.artifacts.length" class="b-arts">
              <span v-for="(a, j) in b.artifacts" :key="j" class="art-pill">
                📄 {{ a.split('/').pop() }}
              </span>
            </div>
          </template>
        </div>
        <div v-if="activeRunning" class="typing"><span></span><span></span><span></span></div>
        <div v-if="!activeBubbles.length && !activeRunning" class="rp-empty">
          运行后这里会实时显示进度。
        </div>
      </div>
    </aside>
  </div>
</template>

<style scoped>
.auto-wrap {
  flex: 1;
  display: flex;
  min-height: 0;
  background: var(--bg);
}
.auto-main {
  flex: 1;
  overflow-y: auto;
  padding: 34px 44px 60px;
  min-width: 0;
}

.head { margin-bottom: 24px; }
.title-row { display: flex; align-items: center; gap: 10px; }
.t-icon { color: var(--ink); }
.head h1 {
  font-family: var(--serif);
  font-size: 22px;
  font-weight: 600;
  letter-spacing: 3px;
  color: var(--ink);
  margin: 0;
}
.lead {
  margin: 10px 0 0;
  font-size: 13px;
  line-height: 1.9;
  color: var(--text-2);
  max-width: 720px;
  letter-spacing: 0.3px;
}

/* ── 自动做梦 · 每日晨报卡 ── */
.dream-card {
  border: 1px solid var(--border-soft);
  border-radius: 14px;
  background: var(--panel);
  padding: 16px 18px;
  margin-bottom: 18px;
}
.dream-card.collapsed { padding: 11px 18px; }
.dc-head { display: flex; align-items: flex-start; gap: 12px; }
.dream-card.collapsed .dc-head { align-items: center; }
.dc-fold {
  flex-shrink: 0; display: inline-flex; align-items: center; justify-content: center;
  width: 26px; height: 26px; border-radius: 7px; cursor: pointer;
  border: 1px solid var(--border-soft); background: transparent; color: var(--muted);
  transition: background 0.15s, color 0.15s;
}
.dc-fold:hover { background: var(--selection-bg); color: var(--ink); }
.dc-fold svg { transition: transform 0.18s; }
.dc-fold svg.up { transform: rotate(180deg); }
.dc-ic {
  width: 30px; height: 30px; border-radius: 9px; flex-shrink: 0;
  display: inline-flex; align-items: center; justify-content: center;
  background: linear-gradient(135deg, #7c5cd9, #a78bfa);
}
.dc-tt { display: flex; flex-direction: column; gap: 3px; min-width: 0; flex: 1; }
.dc-name { font-size: 15px; font-weight: 600; color: var(--ink); letter-spacing: 0.5px; }
.dc-sub { font-size: 12px; line-height: 1.7; color: var(--text-2); max-width: 640px; }
.dc-switch { position: relative; width: 40px; height: 22px; flex-shrink: 0; cursor: pointer; }
.dc-switch input { opacity: 0; width: 0; height: 0; }
.dc-track {
  position: absolute; inset: 0; border-radius: 22px;
  background: var(--border); transition: background 0.18s;
}
.dc-track::before {
  content: ""; position: absolute; top: 3px; left: 3px;
  width: 16px; height: 16px; border-radius: 50%;
  background: #fff; transition: transform 0.18s;
}
.dc-switch input:checked + .dc-track { background: var(--primary, #7c5cd9); }
.dc-switch input:checked + .dc-track::before { transform: translateX(18px); }
.dc-track.sm { border-radius: 18px; }
.dc-mini-switch { position: relative; width: 34px; height: 19px; cursor: pointer; }
.dc-mini-switch input { opacity: 0; width: 0; height: 0; }
.dc-mini-switch .dc-track::before { width: 13px; height: 13px; }
.dc-mini-switch input:checked + .dc-track { background: var(--primary, #7c5cd9); }
.dc-mini-switch input:checked + .dc-track::before { transform: translateX(15px); }

.dc-body { margin-top: 14px; padding-top: 14px; border-top: 1px solid var(--border-soft); }
.dc-row { display: flex; align-items: center; gap: 12px; margin-bottom: 10px; }
.dc-label {
  display: inline-flex; align-items: center; gap: 6px;
  font-size: 12.5px; color: var(--text-2); min-width: 110px;
}
.dc-label svg { color: var(--muted); }
.dc-select {
  border: 1px solid var(--border); border-radius: 7px;
  background: var(--bg); color: var(--ink);
  font-size: 12.5px; padding: 5px 9px; cursor: pointer;
}
.dc-stats { display: flex; flex-wrap: wrap; gap: 8px; margin: 12px 0; }
.dc-stat {
  font-size: 11.5px; color: var(--muted);
  background: var(--selection-bg); padding: 4px 9px; border-radius: 6px;
}
.dc-stat b { color: var(--ink); font-weight: 600; }
.dc-act { display: flex; flex-wrap: wrap; gap: 8px; }
.dc-btn {
  display: inline-flex; align-items: center; gap: 5px;
  border: none; cursor: pointer;
  background: var(--btn-solid-bg); color: var(--btn-solid-text);
  font-size: 12.5px; letter-spacing: 0.5px;
  padding: 7px 13px; border-radius: 8px;
}
.dc-btn:hover:not(:disabled) { background: var(--primary, var(--btn-solid-bg)); }
.dc-btn:disabled { opacity: 0.55; cursor: not-allowed; }
.dc-btn.ghost {
  background: transparent; color: var(--text-2);
  border: 1px solid var(--border);
}
.dc-btn.ghost:hover:not(:disabled) { border-color: var(--ink); color: var(--ink); background: transparent; }
.dc-log {
  margin-top: 12px; padding-top: 10px;
  border-top: 1px dashed var(--border-soft);
  display: flex; flex-direction: column; gap: 5px;
}
.dc-log-line { font-size: 11.5px; color: var(--text-2); line-height: 1.6; }
.dll-day { color: var(--muted); margin-right: 8px; font-variant-numeric: tabular-nums; }

.grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
  gap: 14px;
}
.card {
  border: 1px solid var(--border-soft);
  border-radius: 12px;
  background: var(--panel);
  padding: 16px;
  text-align: left;
  transition: border-color 0.15s, box-shadow 0.15s, transform 0.15s;
}
.card.flow:hover {
  border-color: var(--border);
  box-shadow: 0 6px 20px rgba(0, 0, 0, 0.06);
  transform: translateY(-1px);
}

.card.new {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  min-height: 160px;
  border-style: dashed;
  border-color: var(--border);
  color: var(--muted);
  cursor: pointer;
}
.card.new:hover { border-color: var(--ink); color: var(--ink); background: var(--selection-bg); }
.new-plus {
  width: 40px; height: 40px;
  border-radius: 50%;
  display: inline-flex; align-items: center; justify-content: center;
  background: var(--selection-bg);
}
.card.new:hover .new-plus { background: var(--btn-solid-bg); color: var(--btn-solid-text); }
.new-text { font-size: 13px; letter-spacing: 1px; }

.c-head { display: flex; align-items: center; gap: 9px; }
.c-icon {
  width: 26px; height: 26px;
  border-radius: 8px;
  display: inline-flex; align-items: center; justify-content: center;
  flex-shrink: 0;
}
.c-name {
  font-size: 14px;
  font-weight: 600;
  color: var(--ink);
  flex: 1;
  min-width: 0;
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}
.run-tag {
  display: inline-flex; align-items: center; gap: 4px;
  font-size: 10.5px; color: var(--primary);
  background: var(--primary-soft);
  padding: 2px 7px; border-radius: 20px;
  flex-shrink: 0;
}
.c-desc {
  margin: 10px 0 12px;
  font-size: 12px;
  line-height: 1.7;
  color: var(--text-2);
  min-height: 40px;
}
.c-meta {
  display: flex; flex-wrap: wrap; gap: 6px;
  margin-bottom: 14px;
}
.meta {
  display: inline-flex; align-items: center; gap: 4px;
  font-size: 11px; color: var(--muted);
  background: var(--bg-soft, var(--selection-bg));
  padding: 3px 8px; border-radius: 6px;
}
.meta svg { color: var(--dim); }

.c-act { display: flex; align-items: center; gap: 8px; }
.run-btn {
  flex: 1;
  display: inline-flex; align-items: center; justify-content: center; gap: 5px;
  border: none;
  background: var(--btn-solid-bg); color: var(--btn-solid-text);
  font-size: 12.5px; letter-spacing: 1px;
  padding: 7px 12px; border-radius: 8px;
  cursor: pointer;
}
.run-btn:hover:not(:disabled) { background: var(--primary); }
.run-btn:disabled { opacity: 0.55; cursor: not-allowed; }
.run-btn.stop { background: var(--vermilion); color: #fff; }
.run-btn.stop:hover { background: var(--vermilion); opacity: 0.88; }
.mini-btn {
  border: 1px solid var(--border);
  background: transparent; color: var(--muted);
  width: 30px; height: 30px;
  border-radius: 8px;
  display: inline-flex; align-items: center; justify-content: center;
  cursor: pointer;
}
.mini-btn:hover { border-color: var(--ink); color: var(--ink); }
.mini-btn.danger:hover { border-color: var(--vermilion); color: var(--vermilion); }

/* ── 缩小版对话框 ── */
.run-panel {
  width: 360px;
  flex-shrink: 0;
  border-left: 1px solid var(--border-soft);
  background: var(--bg-soft, var(--panel));
  display: flex;
  flex-direction: column;
  min-height: 0;
}
.rp-head {
  display: flex; align-items: center; justify-content: space-between;
  padding: 12px 14px;
  border-bottom: 1px solid var(--border-soft);
}
.rp-title {
  display: inline-flex; align-items: center; gap: 7px;
  font-size: 13px; font-weight: 600; color: var(--ink);
  font-family: var(--serif); letter-spacing: 1px;
}
.rp-act { display: flex; gap: 4px; }
.rp-stop, .rp-close {
  border: none; background: transparent; color: var(--muted);
  display: inline-flex; padding: 4px; border-radius: 6px; cursor: pointer;
}
.rp-stop:hover { color: var(--vermilion); background: var(--selection-bg); }
.rp-close:hover { color: var(--text); background: var(--selection-bg); }
.rp-body {
  flex: 1;
  overflow-y: auto;
  padding: 14px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.bubble { font-size: 12.5px; line-height: 1.7; }
.bubble.user .b-text {
  background: var(--btn-solid-bg); color: var(--btn-solid-text);
  padding: 8px 11px; border-radius: 10px 10px 2px 10px;
  align-self: flex-end;
  white-space: pre-wrap; word-break: break-word;
}
.bubble.user { display: flex; justify-content: flex-end; }
.bubble.assistant .b-text {
  color: var(--text);
  white-space: pre-wrap; word-break: break-word;
}
.bubble.tool { }
.tool-pill {
  display: inline-block;
  font-size: 11px; color: var(--muted);
  background: var(--selection-bg);
  padding: 2px 8px; border-radius: 20px;
  font-family: var(--mono);
}
.b-arts { margin-top: 6px; display: flex; flex-wrap: wrap; gap: 5px; }
.art-pill {
  font-size: 11px; color: var(--primary-deep, var(--primary));
  background: var(--primary-soft);
  padding: 3px 8px; border-radius: 6px;
}
.rp-empty { font-size: 12px; color: var(--dim); font-style: italic; text-align: center; padding: 30px 0; }

.spin { animation: spin 0.9s linear infinite; }
@keyframes spin { to { transform: rotate(360deg); } }
.typing { display: flex; gap: 4px; padding: 4px 0; }
.typing span {
  width: 6px; height: 6px; border-radius: 50%;
  background: var(--muted);
  animation: blink 1.2s infinite both;
}
.typing span:nth-child(2) { animation-delay: 0.2s; }
.typing span:nth-child(3) { animation-delay: 0.4s; }
@keyframes blink { 0%, 80%, 100% { opacity: 0.25; } 40% { opacity: 1; } }
</style>
