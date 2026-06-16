<script setup lang="ts">
/**
 * ExpertTeamStudio — 专家团工作台（统一工作区 + 可视化）
 *
 * 读项目 personaId：
 *  - 若是业务团 id → 展示整支队伍（领衔 + 成员）的阵容板
 *  - 若是单专家 id → 展示单专家卡
 * 叠加实时状态（agentsStatus 轮询）：idle / working / done 徽标。
 */
import { ref, computed, onMounted, watch } from "vue";
import { expert, avatarSlot, convApi, type ExpertCard, type ExpertTeam, type ExpertAgentStatus } from "../tauri";

const props = defineProps<{
  projectId: string;
  agentsStatus?: ExpertAgentStatus[];
}>();

const activeTeam = ref<ExpertTeam | null>(null);
const lead = ref<ExpertCard | null>(null);
const members = ref<ExpertCard[]>([]);
const soloExpert = ref<ExpertCard | null>(null);
const loading = ref(true);
const avatarSlots = ref<string[]>([]);

onMounted(async () => {
  try { avatarSlots.value = (await expert.avatarSlots()) ?? []; } catch { /* ignore */ }
  loadActive();
});
watch(() => props.projectId, loadActive);

async function loadActive() {
  loading.value = true;
  activeTeam.value = null;
  lead.value = null;
  members.value = [];
  soloExpert.value = null;
  try {
    const projects = await convApi.listProjects();
    const proj = projects.find((p) => p.id === props.projectId);
    const pid = proj?.personaId;
    if (!pid) return;

    // 先看是不是业务团
    const team = await expert.teamGet(pid);
    if (team) {
      activeTeam.value = team;
      const [ld, ...ms] = await Promise.all([
        expert.get(team.leadId),
        ...team.memberIds.map((m) => expert.get(m)),
      ]);
      lead.value = ld;
      members.value = ms.filter((m): m is ExpertCard => !!m);
      return;
    }
    // 否则按单专家
    soloExpert.value = await expert.get(pid);
  } catch (e) {
    console.error("加载专家团失败", e);
  } finally {
    loading.value = false;
  }
}

// 阵容（领衔在前）
const roster = computed<ExpertCard[]>(() => {
  if (activeTeam.value && lead.value) return [lead.value, ...members.value];
  if (soloExpert.value) return [soloExpert.value];
  return [];
});

function statusOf(id: string): string {
  return props.agentsStatus?.find((a) => a.expertId === id)?.status ?? "idle";
}
const statusLabel: Record<string, string> = { idle: "待命", working: "工作中", done: "完成" };
const statusColor: Record<string, string> = { idle: "#6f6e7e", working: "#6ea8ff", done: "#5fd39a" };

const tierLabel: Record<number, string> = { 1: "便宜路由", 2: "中档专业", 3: "深度推理" };
const tierColor: Record<number, string> = { 1: "#5fd39a", 2: "#e6c984", 3: "#c79cff" };

function avatarUrl(id: string, icon: string): string {
  const slots = avatarSlots.value;
  if (slots.length) return slots[avatarSlot(id)] ?? "";
  const colors = ["#d4b06a", "#b07bff", "#5fd39a", "#6ea8ff", "#e6c984", "#c79cff"];
  const c = colors[icon.charCodeAt(0) % colors.length];
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="56" height="56" viewBox="0 0 56 56"><circle cx="28" cy="28" r="28" fill="${c}"/><text x="50%" y="56%" font-size="22" text-anchor="middle" dominant-baseline="middle">${icon}</text></svg>`;
  return `data:image/svg+xml,${encodeURIComponent(svg)}`;
}
</script>

<template>
  <div class="studio">
    <div class="studio-head">
      <span class="studio-title">🧭 专家团工作台</span>
      <span v-if="activeTeam" class="studio-badge">{{ activeTeam.icon }} {{ activeTeam.name }}</span>
      <span v-else-if="soloExpert" class="studio-badge">{{ soloExpert.icon }} {{ soloExpert.name }}</span>
      <span v-else class="studio-empty-hint">智能匹配中</span>
    </div>

    <div v-if="loading" class="studio-loading">
      <span class="loading-dot" /><span class="loading-dot" /><span class="loading-dot" />
    </div>

    <div v-else-if="!roster.length" class="studio-empty">
      <div class="empty-icon">🧭</div>
      <p class="empty-title">默认已启用「智能匹配专家团」</p>
      <p class="empty-sub">直接说出你的需求即可——系统会自动召集最合适的专家。也可在「专家团」里手动入驻一支业务团。</p>
    </div>

    <div v-else class="team-board">
      <div
        v-for="(m, idx) in roster"
        :key="m.id"
        class="member-card"
        :style="{ '--tier-left': tierColor[m.costTier] }"
      >
        <div class="member-avatar-wrap">
          <img :src="avatarUrl(m.id, m.icon)" :alt="m.name" class="member-avatar" />
          <div v-if="activeTeam && idx === 0" class="orchestrator-badge">🧭</div>
        </div>
        <div class="member-info">
          <div class="member-name-row">
            <span class="member-name">{{ m.icon }} {{ m.name }}</span>
            <span v-if="activeTeam && idx === 0" class="lead-tag">领衔</span>
            <span class="member-tier" :style="{ color: tierColor[m.costTier], borderColor: tierColor[m.costTier] + '55' }">
              {{ tierLabel[m.costTier] }}
            </span>
            <span class="status-dot" :style="{ background: statusColor[statusOf(m.id)] }" :title="statusLabel[statusOf(m.id)]" />
          </div>
          <div class="member-role">{{ m.role }}</div>
          <div class="member-reason">
            <span v-for="s in m.triggerSignals.slice(0, 4)" :key="s" class="reason-chip">{{ s }}</span>
            <span v-if="m.complements" class="reason-comp">补「{{ m.complements }}」</span>
          </div>
        </div>
      </div>

      <div class="orchestrate-note">
        <div class="note-icon">💡</div>
        <p>领衔者按情况临时组阵：简单任务单人直接干，确需分工且并行有收益时才多人协作。对话中可随时追加 / 换人。</p>
      </div>
    </div>
  </div>
</template>

<style scoped>
.studio { padding: 14px 16px; background: var(--panel, rgba(24, 24, 34, 0.66)); border-radius: 12px; border: 1px solid var(--line, rgba(255, 255, 255, 0.08)); min-height: 140px; }
.studio-head { display: flex; align-items: center; gap: 10px; margin-bottom: 14px; }
.studio-title { font-size: 14px; font-weight: 700; color: var(--ink, #e9e8ee); }
.studio-badge { font-size: 12px; background: rgba(212, 176, 106, 0.15); border: 1px solid rgba(212, 176, 106, 0.3); color: var(--gold, #d4b06a); padding: 2px 10px; border-radius: 20px; }
.studio-empty-hint { font-size: 12px; color: #6ea8ff; }

.studio-loading { display: flex; justify-content: center; gap: 6px; padding: 28px; }
.loading-dot { width: 8px; height: 8px; border-radius: 50%; background: var(--gold, #d4b06a); animation: dotPulse 1.2s ease-in-out infinite; }
.loading-dot:nth-child(2) { animation-delay: 0.2s; }
.loading-dot:nth-child(3) { animation-delay: 0.4s; }
@keyframes dotPulse { 0%, 80%, 100% { opacity: 0.3; transform: scale(0.8); } 40% { opacity: 1; transform: scale(1.1); } }

.studio-empty { display: flex; flex-direction: column; align-items: center; padding: 18px; text-align: center; }
.empty-icon { font-size: 34px; margin-bottom: 8px; opacity: 0.5; }
.empty-title { font-size: 14px; font-weight: 600; color: var(--ink); margin: 0 0 4px; }
.empty-sub { font-size: 12px; color: var(--dim, #a7a6b4); line-height: 1.5; max-width: 280px; margin: 0; }

.team-board { display: flex; flex-direction: column; gap: 10px; }
.member-card { display: flex; gap: 12px; padding: 11px 13px; border-radius: 12px; border: 1px solid var(--line); border-left: 3px solid var(--tier-left, var(--line)); background: var(--panel2, rgba(32, 32, 46, 0.5)); position: relative; }
.member-avatar-wrap { position: relative; flex: 0 0 56px; }
.member-avatar { width: 56px; height: 56px; border-radius: 50%; object-fit: cover; border: 2px solid rgba(255, 255, 255, 0.1); }
.orchestrator-badge { position: absolute; bottom: -2px; right: -2px; font-size: 14px; background: var(--panel, rgba(24, 24, 34, 0.9)); border-radius: 50%; width: 22px; height: 22px; display: flex; align-items: center; justify-content: center; border: 1px solid rgba(212, 176, 106, 0.4); }
.member-info { flex: 1; display: flex; flex-direction: column; gap: 3px; min-width: 0; }
.member-name-row { display: flex; align-items: center; gap: 7px; }
.member-name { font-size: 14px; font-weight: 700; color: var(--ink); }
.lead-tag { font-size: 10px; color: var(--gold, #d4b06a); border: 1px solid rgba(212, 176, 106, 0.4); border-radius: 4px; padding: 0 5px; }
.member-tier { font-size: 10px; padding: 0 6px; border-radius: 4px; border: 1px solid; }
.status-dot { width: 8px; height: 8px; border-radius: 50%; margin-left: auto; }
.member-role { font-size: 12px; color: var(--dim); }
.member-reason { display: flex; flex-wrap: wrap; gap: 4px; align-items: center; margin-top: 2px; }
.reason-chip { font-size: 10px; background: rgba(110, 168, 255, 0.15); border: 1px solid rgba(110, 168, 255, 0.3); color: #6ea8ff; padding: 1px 6px; border-radius: 4px; }
.reason-comp { font-size: 11px; color: var(--faint); }
.orchestrate-note { display: flex; gap: 8px; padding: 9px 11px; border-radius: 8px; background: rgba(212, 176, 106, 0.06); border: 1px solid rgba(212, 176, 106, 0.15); }
.note-icon { font-size: 14px; flex: 0 0 20px; }
.orchestrate-note p { font-size: 12px; color: var(--dim); margin: 0; line-height: 1.5; }
</style>
