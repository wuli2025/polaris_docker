<script setup lang="ts">
/**
 * ExpertCenter — 专家团中心（顶层「专家团」入口，原「人格」升级）
 *
 * 让 AI 更懂你：
 *  ① 按当前项目的知识库反推「该配哪支专家团」，一键入驻；
 *  ② 浏览全部专家 + 业务团（介绍 / 头像 / 下载 / 匹配测试）；
 *  ③ 高级：直接编辑本项目的人格档案 CLAUDE.md。
 */
import { ref, computed, onMounted, watch } from "vue";
import ExpertTeam from "./ExpertTeam.vue";
import ClaudeMdPanel from "./ClaudeMdPanel.vue";
import { expert, avatarSlot, type KbRecommendation } from "../tauri";
import { useAppStore } from "../stores/app";
import { toast } from "../composables/useToast";

const app = useAppStore();

type Tab = "market" | "advanced";
const tab = ref<Tab>("market");

// 当前配置的项目（默认当前项目）
const projectId = ref<string | null>(app.currentProjectId);
const projects = computed(() => app.projects);
const activeProject = computed(() => projects.value.find((p) => p.id === projectId.value) ?? null);

// KB 推荐
const rec = ref<KbRecommendation | null>(null);
const recLoading = ref(false);
const avatarSlots = ref<string[]>([]);
const leadAvatar = computed(() =>
  rec.value?.team && avatarSlots.value.length
    ? avatarSlots.value[avatarSlot(rec.value.team.leadId)]
    : "",
);

async function loadRecommendation() {
  rec.value = null;
  recLoading.value = true;
  try {
    const scope = activeProject.value?.kbScope || undefined;
    rec.value = await expert.recommendFromKb(scope);
  } catch (e) {
    console.error("KB 推荐失败", e);
  } finally {
    recLoading.value = false;
  }
}

onMounted(async () => {
  if (!projectId.value && projects.value.length) projectId.value = projects.value[0].id;
  try { avatarSlots.value = (await expert.avatarSlots()) ?? []; } catch { /* ignore */ }
  loadRecommendation();
});
watch(projectId, loadRecommendation);

// 召唤 = 把这支团/这位专家入驻到当前项目并切回对话区(由 ChatPanel 统一落地:
// expert.apply/teamApply + 设模式 + 记最近召唤 + 显示专家团工作台)。没有当前项目则提示。
function summonTeam(id: string) {
  if (!app.currentProjectId) { toast.error("请先在对话区选择或新建一个项目，再召唤专家团"); return; }
  app.requestSummon("team", id);
}
function summonExpert(id: string) {
  if (!app.currentProjectId) { toast.error("请先在对话区选择或新建一个项目，再召唤专家"); return; }
  app.requestSummon("expert", id);
}
function applyRecommended() {
  const team = rec.value?.team;
  if (!team) return;
  summonTeam(team.id);
}

function onSelectTeam(id: string) {
  summonTeam(id);
}
function onSelectExpert(id: string) {
  summonExpert(id);
}
</script>

<template>
  <div class="ec-root">
    <!-- 头部 -->
    <header class="ec-head">
      <div class="ec-title-wrap">
        <div class="ec-title">🧭 专家团 · 让 AI 更懂你</div>
        <div class="ec-sub">
          按你的知识库与偏好，自动给你配好对应业务的专家团；也可手动浏览全部专家入驻本项目。
        </div>
      </div>
      <div class="ec-proj">
        <label>配置项目</label>
        <select v-model="projectId" class="ec-proj-sel">
          <option v-for="p in projects" :key="p.id" :value="p.id">{{ p.name }}</option>
        </select>
      </div>
    </header>

    <!-- KB 智能推荐横幅 -->
    <section class="ec-rec" :class="{ has: rec?.team }">
      <div v-if="recLoading" class="ec-rec-loading">正在按知识库反推最适合你的专家团…</div>
      <template v-else-if="rec?.team">
        <img v-if="leadAvatar" :src="leadAvatar" class="ec-rec-avatar" :alt="rec.team.name" />
        <div class="ec-rec-body">
          <div class="ec-rec-head">
            <span class="ec-rec-badge">按你的知识库推荐</span>
            <span class="ec-rec-name">{{ rec.team.icon }} {{ rec.team.name }}</span>
          </div>
          <div class="ec-rec-reason">{{ rec.reason }}</div>
          <div v-if="rec.matchedTopics.length" class="ec-rec-topics">
            <span v-for="t in rec.matchedTopics.slice(0, 6)" :key="t" class="ec-topic">{{ t }}</span>
          </div>
        </div>
        <button class="ec-rec-apply" @click="applyRecommended">一键入驻</button>
      </template>
      <template v-else>
        <div class="ec-rec-body">
          <div class="ec-rec-head"><span class="ec-rec-badge dim">智能匹配 · 默认</span></div>
          <div class="ec-rec-reason">{{ rec?.reason ?? "正在分析你的知识库…" }}</div>
        </div>
      </template>
    </section>

    <!-- Tab -->
    <div class="ec-tabs">
      <button class="ec-tab" :class="{ on: tab === 'market' }" @click="tab = 'market'">专家市场</button>
      <button class="ec-tab" :class="{ on: tab === 'advanced' }" @click="tab = 'advanced'">人格档案 · 高级</button>
    </div>

    <!-- 内容 -->
    <div class="ec-body">
      <div v-show="tab === 'market'" class="ec-market">
        <ExpertTeam @select-team="onSelectTeam" @select-expert="onSelectExpert" />
      </div>
      <div v-if="tab === 'advanced'" class="ec-advanced">
        <ClaudeMdPanel />
      </div>
    </div>
  </div>
</template>

<style scoped>
.ec-root {
  flex: 1;
  display: flex;
  flex-direction: column;
  min-height: 0;
  background: var(--bg);
}
.ec-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
  padding: 16px 20px 12px;
  border-bottom: 1px solid var(--border-soft);
}
.ec-title { font-size: 17px; font-weight: 700; color: var(--ink, var(--text)); letter-spacing: 1px; }
.ec-sub { font-size: 12px; color: var(--muted); margin-top: 4px; max-width: 640px; line-height: 1.5; }
.ec-proj { display: flex; flex-direction: column; gap: 4px; align-items: flex-end; }
.ec-proj label { font-size: 11px; color: var(--dim); }
.ec-proj-sel {
  padding: 5px 10px;
  border-radius: 8px;
  border: 1px solid var(--border);
  background: var(--panel);
  color: var(--text);
  font-size: 12.5px;
  min-width: 160px;
}

.ec-rec {
  display: flex;
  align-items: center;
  gap: 14px;
  margin: 14px 20px 0;
  padding: 14px 16px;
  border-radius: 14px;
  border: 1px solid var(--border-soft);
  background: var(--bg-soft);
}
.ec-rec.has {
  border-color: rgba(212, 176, 106, 0.4);
  background: linear-gradient(100deg, rgba(212, 176, 106, 0.1), rgba(176, 123, 255, 0.06));
}
.ec-rec-loading { font-size: 13px; color: var(--muted); }
.ec-rec-avatar { width: 56px; height: 56px; border-radius: 14px; object-fit: cover; flex-shrink: 0; }
.ec-rec-body { flex: 1; min-width: 0; }
.ec-rec-head { display: flex; align-items: center; gap: 10px; }
.ec-rec-badge {
  font-size: 10.5px; font-weight: 700; color: var(--btn-solid-text);
  background: var(--primary); border-radius: 999px; padding: 1px 9px;
}
.ec-rec-badge.dim { background: var(--border); color: var(--muted); }
.ec-rec-name { font-size: 15px; font-weight: 700; color: var(--text); }
.ec-rec-reason { font-size: 12.5px; color: var(--text-2, var(--muted)); margin-top: 5px; line-height: 1.55; }
.ec-rec-topics { display: flex; flex-wrap: wrap; gap: 5px; margin-top: 7px; }
.ec-topic {
  font-size: 10.5px; color: var(--muted);
  border: 1px solid var(--border); border-radius: 5px; padding: 1px 7px;
}
.ec-rec-apply {
  flex-shrink: 0;
  padding: 9px 18px;
  border-radius: 10px;
  border: none;
  background: var(--btn-solid-bg);
  color: var(--btn-solid-text);
  font-size: 13px;
  font-weight: 600;
  cursor: pointer;
  transition: filter 0.14s;
}
.ec-rec-apply:hover { filter: brightness(1.08); }

.ec-tabs { display: flex; gap: 4px; padding: 12px 20px 0; }
.ec-tab {
  padding: 7px 16px;
  border: none;
  background: transparent;
  color: var(--muted);
  font-size: 13px;
  border-radius: 8px 8px 0 0;
  cursor: pointer;
  border-bottom: 2px solid transparent;
}
.ec-tab:hover { color: var(--text); }
.ec-tab.on { color: var(--text); font-weight: 600; border-bottom-color: var(--primary); }

.ec-body { flex: 1; min-height: 0; display: flex; flex-direction: column; }
.ec-market { flex: 1; min-height: 0; overflow-y: auto; padding: 12px 20px 20px; }
.ec-advanced { flex: 1; min-height: 0; display: flex; flex-direction: column; }
</style>
