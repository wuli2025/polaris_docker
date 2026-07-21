<script setup lang="ts">
// 空对话页(自 ChatPanel 拆出,逻辑原样搬移):毛主席项目彩蛋 / 问候语 /
// 「下一步工作流」推荐。父层只在 renderTurns 为空时挂载本组件;
// 点一条建议经 apply 事件把整条工作流提示词交回父层填进输入框。
import { ref, computed, watch } from "vue";
import { Sparkles, RefreshCw } from "@lucide/vue";
import { files as fc, type SuggestedFlow } from "../../tauri";
import { useAppStore } from "../../stores/app";

const emit = defineEmits<{ (e: "apply", f: SuggestedFlow): void }>();

const app = useAppStore();

// 当前项目是否为默认赠送的「毛主席」项目 —— 决定空状态彩蛋（与后端 MAO_PROJECT_NAME 一致）
const currentProjectName = computed(
  () => app.projects.find((p) => p.id === app.currentProjectId)?.name || ""
);
const isMaoProject = computed(() => currentProjectName.value === "毛主席");

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
// 点一条建议:把整条工作流提示词交回父层填进输入框并聚焦,让用户先看清(这些是成体系的长提示词)再发。
function applyFlow(f: SuggestedFlow) {
  emit("apply", f);
}
// 空白页(本组件仅在没有任何回合时挂载、且非毛主席彩蛋页)→ 拉一次工作流推荐;
// 有缓存秒出,无缓存才走 LLM。
const showFlowSuggestions = computed(() => !isMaoProject.value);
watch(
  showFlowSuggestions,
  (empty) => {
    if (empty) loadWorkflowFlows();
  },
  { immediate: true },
);
</script>

<template>
  <div class="hero-wrap">
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
</template>

<style scoped>
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
</style>
