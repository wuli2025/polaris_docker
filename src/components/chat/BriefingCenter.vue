<script setup lang="ts">
// 「今日建议」(自 ChatPanel 拆出,逻辑原样搬移):底部小胶囊 + 居中大弹窗 +
// 晨报拉取/忽略/执行 + echo:dream 做梦完成监听。发送选项经 sendOptions prop
// 从工具条(ChatComposer)取,保证「让我去做」与主发送用同一套模式/供应商配置。
import { ref, onMounted, onBeforeUnmount } from "vue";
import {
  Sparkles,
  X,
  ArrowRight,
  BookOpen,
  Rocket,
  Flag,
  Workflow,
  FolderTree,
} from "@lucide/vue";
import { invoke, listen } from "../../tauri";
import { useAppStore } from "../../stores/app";
import { useChatStore } from "../../stores/chat";
import { useIdleRunner, type ChatSendOptions, type Suggestion } from "./shared";

const props = defineProps<{
  /** 取当前工具条各开关拼出的发送选项(权限/技能/知识库/智能体/工作模式/供应商) */
  sendOptions: (convId: string) => ChatSendOptions;
}>();

const app = useAppStore();
const chatStore = useChatStore();

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
  await chatStore.send(c.id, prompt, s.title || prompt, undefined, props.sendOptions(c.id));
}

// 首帧非关键加载推迟(见 shared.ts useIdleRunner):卸载后作废
const idle = useIdleRunner();
const unlisteners: Array<() => void> = [];

onMounted(() => {
  // 晨报拉取 + 做梦监听都不影响首屏聊天区渲染 → 推迟到空闲帧,别与首帧 IPC 抢资源。
  // 弹窗因此最多晚 ~600ms,可接受。
  idle.runWhenIdle(() => {
    void (async () => {
      await loadBriefings();
      if (idle.isDisposed()) return; // await 期间可能已卸载
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
      // 捕获 unlisten 并统一回收(onBeforeUnmount):此前未解绑,
      // KeepAlive 反复挂载会逐月累积上千个 echo:dream 监听器及其闭包 → 内存爬升。
      const un = await listen("echo:dream", async (p: any) => {
        if ((p?.kind ?? p?.payload?.kind) === "done") {
          await loadBriefings();
          if (briefings.value.length) briefOpen.value = true;
        }
      });
      if (idle.isDisposed()) un(); // 卸载后才注册完成:立刻解绑,别泄漏
      else unlisteners.push(un);
    })();
  });
});

onBeforeUnmount(() => {
  idle.dispose(); // 让尚未执行的空闲回调作废
  for (const u of unlisteners) u();
});
</script>

<template>
  <!-- 今日建议（每日任务）：底部授权栏留一枚小胶囊随时重开；正文是居中大弹窗 -->
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
</template>

<style scoped>
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
</style>
