<script setup lang="ts">
/**
 * ExpertTeam — 专家团市场（仿 WorkBuddy 的壳）
 *
 * 两个通道：
 *  ① 业务专家团：成建制队伍，点一下整支入驻（emit select-team）
 *  ② 全部专家：按领域浏览 / 搜索，单独入驻（emit select-expert）
 * 附：每个专家/团可「下载」CLAUDE.md；「智能匹配测试」面板可调试路由命中。
 */
import { ref, onMounted } from "vue";
import {
  expert,
  avatarSlot,
  type ExpertCard,
  type ExpertGroup,
  type ExpertTeam,
  type ExpertDebugRow,
} from "../tauri";

const groups = ref<ExpertGroup[]>([]);
const allExperts = ref<ExpertCard[]>([]);
const filteredExperts = ref<ExpertCard[]>([]);
const teams = ref<ExpertTeam[]>([]);
const selectedGroup = ref<string | null>(null);
const searchQuery = ref("");

// 9 张头像拉一次，按 id 本地映射（避免 100+ 卡片逐个 IPC 取头像 → 卡顿）
const avatarSlots = ref<string[]>([]);

onMounted(async () => {
  try {
    const [g, exps, tms, slots] = await Promise.all([
      expert.groups(),
      expert.list(),
      expert.teams(),
      expert.avatarSlots(),
    ]);
    groups.value = g ?? [];
    allExperts.value = exps ?? [];
    filteredExperts.value = exps ?? [];
    teams.value = tms ?? [];
    avatarSlots.value = slots ?? [];
  } catch (e) {
    console.error("加载专家库失败", e);
  }
});

function selectGroup(g: string | null) {
  selectedGroup.value = g;
  applyFilter();
}

function applyFilter() {
  const q = searchQuery.value.trim().toLowerCase();
  let list = selectedGroup.value
    ? allExperts.value.filter((e) => e.group === selectedGroup.value)
    : allExperts.value;
  if (q) {
    list = list.filter(
      (e) =>
        e.name.toLowerCase().includes(q) ||
        e.role.toLowerCase().includes(q) ||
        e.triggerSignals.some((s) => s.toLowerCase().includes(q)) ||
        e.keywords.some((k) => k.toLowerCase().includes(q)) ||
        e.group.toLowerCase().includes(q),
    );
  }
  filteredExperts.value = list;
}

// id → 头像（本地映射，零额外 IPC）；未就绪时落 emoji+渐变占位
function avatarUrl(id: string, icon: string): string {
  const slots = avatarSlots.value;
  if (slots.length) return slots[avatarSlot(id)] ?? "";
  const colors = ["#d4b06a", "#b07bff", "#5fd39a", "#6ea8ff", "#e6c984", "#c79cff"];
  const color = colors[icon.charCodeAt(0) % colors.length];
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="56" height="56" viewBox="0 0 56 56"><circle cx="28" cy="28" r="28" fill="${color}"/><text x="50%" y="56%" font-size="22" text-anchor="middle" dominant-baseline="middle">${icon}</text></svg>`;
  return `data:image/svg+xml,${encodeURIComponent(svg)}`;
}

// 下载某专家/团的 CLAUDE.md
async function downloadDoc(kind: "expert" | "team", id: string, name: string, ev: Event) {
  ev.stopPropagation();
  try {
    const text =
      kind === "team" ? await expert.exportTeam(id) : await expert.exportExpert(id);
    const blob = new Blob([text], { type: "text/markdown;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${name}.CLAUDE.md`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  } catch (e) {
    console.error("下载失败", e);
  }
}

// ── 智能匹配测试 / 调试 ─────────────────────
const showDebug = ref(false);
const debugQuery = ref("");
const debugRows = ref<ExpertDebugRow[]>([]);
const debugBusy = ref(false);
async function runDebug() {
  const q = debugQuery.value.trim();
  if (!q) return;
  debugBusy.value = true;
  try {
    const rows = await expert.routeDebug(q);
    debugRows.value = (rows ?? []).slice(0, 12);
  } catch (e) {
    console.error("调试失败", e);
  } finally {
    debugBusy.value = false;
  }
}

const tierLabel: Record<number, string> = { 1: "便宜", 2: "中档", 3: "贵档" };
const tierLeft: Record<number, string> = { 1: "#5fd39a", 2: "#d4b06a", 3: "#c79cff" };

const emit = defineEmits<{
  (e: "select-expert", id: string): void;
  (e: "select-team", id: string): void;
}>();
</script>

<template>
  <div class="expert-market">
    <!-- 搜索 + 调试入口 -->
    <div class="market-top">
      <input
        v-model="searchQuery"
        class="rs-input"
        placeholder="搜索专家名字、角色、触发词…"
        @input="applyFilter"
      />
      <button class="dbg-btn" :class="{ on: showDebug }" @click="showDebug = !showDebug">
        🔬 匹配测试
      </button>
    </div>

    <!-- 智能匹配测试面板 -->
    <div v-if="showDebug" class="debug-panel">
      <div class="dbg-row">
        <input
          v-model="debugQuery"
          class="rs-input"
          placeholder="输入一句需求，看智能匹配会召集谁、为什么…"
          @keyup.enter="runDebug"
        />
        <button class="dbg-run" :disabled="debugBusy" @click="runDebug">
          {{ debugBusy ? "…" : "测试匹配" }}
        </button>
      </div>
      <div v-if="debugRows.length" class="dbg-results">
        <div v-for="r in debugRows" :key="r.id" class="dbg-item" :class="{ sel: r.wouldSelect }">
          <span class="dbg-name">{{ r.name }}</span>
          <span class="dbg-bar"><i :style="{ width: Math.min(100, r.similarity * 120) + '%' }" /></span>
          <span class="dbg-score">{{ r.similarity.toFixed(3) }}</span>
          <span v-if="r.wouldSelect" class="dbg-tag">召集</span>
          <span v-if="r.hitSignals.length" class="dbg-sig">命中: {{ r.hitSignals.join("、") }}</span>
        </div>
      </div>
      <p v-else class="dbg-hint">输入需求并测试，可看到每位专家的命中信号与相似度——用来校准触发词。</p>
    </div>

    <!-- ① 业务专家团 -->
    <div class="section-title">🧭 业务专家团 · 一句话组队</div>
    <div class="team-grid">
      <button v-for="tm in teams" :key="tm.id" class="team-card" @click="emit('select-team', tm.id)">
        <img class="team-avatar" :src="avatarUrl(tm.leadId, tm.icon)" :alt="tm.name" />
        <div class="team-body">
          <div class="team-name">{{ tm.icon }} {{ tm.name }}</div>
          <div class="team-tag">{{ tm.tagline }}</div>
          <div class="team-meta">{{ tm.memberIds.length + 1 }} 人 · {{ tm.tags.slice(0, 3).join(" / ") }}</div>
        </div>
        <div class="card-actions">
          <span class="dl-inline" title="下载该团 CLAUDE.md" @click.stop="downloadDoc('team', tm.id, tm.name, $event)">⬇</span>
          <span class="summon-pill" title="召唤这支团" @click.stop="emit('select-team', tm.id)">召唤</span>
        </div>
      </button>
    </div>

    <!-- ② 全部专家 -->
    <div class="section-title">全部专家 · {{ filteredExperts.length }} / {{ allExperts.length }} 位</div>
    <div class="group-bar">
      <button class="gb-btn" :class="{ on: !selectedGroup }" @click="selectGroup(null)">
        全部 <span class="gb-c">{{ allExperts.length }}</span>
      </button>
      <button
        v-for="g in groups"
        :key="g.id"
        class="gb-btn"
        :class="{ on: selectedGroup === g.id }"
        @click="selectGroup(g.id)"
      >
        <span class="gb-icon">{{ g.icon }}</span>{{ g.name }} <span class="gb-c">{{ g.count }}</span>
      </button>
    </div>

    <div class="exp-grid">
      <button
        v-for="exp in filteredExperts"
        :key="exp.id"
        class="exp-card"
        :style="{ '--tier-left': tierLeft[exp.costTier] }"
        :title="exp.description"
        @click="emit('select-expert', exp.id)"
      >
        <img class="exp-avatar" :src="avatarUrl(exp.id, exp.icon)" :alt="exp.name" />
        <div class="exp-info">
          <div class="exp-name-row">
            <span class="exp-name">{{ exp.name }}</span>
            <span class="exp-tier">{{ tierLabel[exp.costTier] }}</span>
          </div>
          <div class="exp-role">{{ exp.role }}</div>
        </div>
        <div class="card-actions">
          <span class="dl-inline" title="下载该专家 CLAUDE.md" @click.stop="downloadDoc('expert', exp.id, exp.name, $event)">⬇</span>
          <span class="summon-pill sm" title="召唤这位专家" @click.stop="emit('select-expert', exp.id)">召唤</span>
        </div>
      </button>
    </div>
  </div>
</template>

<style scoped>
.expert-market {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 0 2px 6px;
  max-height: 72vh;
  overflow-y: auto;
}
.market-top { display: flex; gap: 8px; }
.rs-input {
  flex: 1;
  padding: 8px 12px;
  border-radius: 10px;
  border: 1px solid var(--line, rgba(255, 255, 255, 0.08));
  background: rgba(255, 255, 255, 0.05);
  color: var(--ink, #e9e8ee);
  font-size: 13px;
  outline: none;
}
.rs-input:focus { border-color: rgba(212, 176, 106, 0.45); }
.rs-input::placeholder { color: var(--faint, #6f6e7e); }
.dbg-btn {
  padding: 0 12px;
  border-radius: 10px;
  border: 1px solid var(--line);
  background: transparent;
  color: var(--dim, #a7a6b4);
  font-size: 12.5px;
  cursor: pointer;
  white-space: nowrap;
}
.dbg-btn.on { color: #6ea8ff; border-color: rgba(110, 168, 255, 0.4); background: rgba(110, 168, 255, 0.08); }

.debug-panel {
  border: 1px solid rgba(110, 168, 255, 0.25);
  background: rgba(110, 168, 255, 0.05);
  border-radius: 11px;
  padding: 10px 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.dbg-row { display: flex; gap: 8px; }
.dbg-run {
  padding: 0 14px;
  border-radius: 9px;
  border: 1px solid rgba(110, 168, 255, 0.4);
  background: rgba(110, 168, 255, 0.12);
  color: #6ea8ff;
  font-size: 12.5px;
  cursor: pointer;
  white-space: nowrap;
}
.dbg-run:disabled { opacity: 0.5; }
.dbg-results { display: flex; flex-direction: column; gap: 4px; }
.dbg-item {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 11.5px;
  color: var(--dim);
  padding: 2px 0;
}
.dbg-item.sel .dbg-name { color: var(--ink); font-weight: 600; }
.dbg-name { width: 120px; flex-shrink: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.dbg-bar { flex: 1; height: 6px; border-radius: 3px; background: rgba(255, 255, 255, 0.08); overflow: hidden; }
.dbg-bar i { display: block; height: 100%; background: linear-gradient(90deg, #6ea8ff, #b07bff); }
.dbg-score { width: 44px; text-align: right; font-variant-numeric: tabular-nums; }
.dbg-tag { color: #5fd39a; font-size: 10px; border: 1px solid rgba(95, 211, 154, 0.4); border-radius: 4px; padding: 0 5px; }
.dbg-sig { color: var(--faint); font-size: 10.5px; }
.dbg-hint { font-size: 11.5px; color: var(--faint); margin: 0; }

.section-title { font-size: 12px; font-weight: 700; color: var(--gold2, #e6c984); margin-top: 4px; }

.team-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(240px, 1fr)); gap: 8px; }
.team-card {
  position: relative;
  display: flex;
  align-items: center;
  gap: 11px;
  padding: 10px 12px;
  border-radius: 12px;
  border: 1px solid var(--line);
  background: rgba(255, 255, 255, 0.025);
  cursor: pointer;
  text-align: left;
  transition: all 0.16s;
}
.team-card:hover { border-color: rgba(212, 176, 106, 0.45); background: rgba(212, 176, 106, 0.06); transform: translateY(-1px); }
.team-avatar { width: 44px; height: 44px; border-radius: 12px; object-fit: cover; flex-shrink: 0; }
.team-body { flex: 1; min-width: 0; }
.team-name { font-size: 13.5px; font-weight: 700; color: var(--ink); }
.team-tag { font-size: 11.5px; color: var(--dim); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.team-meta { font-size: 10.5px; color: var(--faint); margin-top: 2px; }

.group-bar { display: flex; gap: 5px; flex-wrap: wrap; }
.gb-btn {
  display: flex; align-items: center; gap: 3px;
  padding: 3px 9px; border-radius: 20px;
  border: 1px solid var(--line); background: transparent;
  color: var(--dim); font-size: 11.5px; cursor: pointer; transition: 0.14s;
}
.gb-btn:hover { color: var(--ink); border-color: rgba(212, 176, 106, 0.3); }
.gb-btn.on { background: linear-gradient(135deg, rgba(212, 176, 106, 0.15), rgba(176, 123, 255, 0.1)); border-color: rgba(212, 176, 106, 0.45); color: var(--ink); }
.gb-icon { font-size: 10px; }
.gb-c { opacity: 0.5; font-size: 11px; }

.exp-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(200px, 1fr)); gap: 6px; }
.exp-card {
  position: relative;
  display: flex; align-items: center; gap: 8px;
  padding: 7px 8px 7px 10px;
  border-radius: 9px;
  border: 1px solid var(--line);
  border-left: 3px solid var(--tier-left, var(--line));
  background: rgba(255, 255, 255, 0.025);
  cursor: pointer; text-align: left; transition: all 0.16s;
}
.exp-card:hover { border-color: rgba(212, 176, 106, 0.35); background: rgba(255, 255, 255, 0.05); transform: translateY(-1px); }
.exp-avatar { width: 38px; height: 38px; border-radius: 50%; object-fit: cover; flex-shrink: 0; }
.exp-info { display: flex; flex-direction: column; gap: 1px; min-width: 0; flex: 1; }
.exp-name-row { display: flex; align-items: center; gap: 4px; }
.exp-name { font-size: 12.5px; font-weight: 600; color: var(--ink); flex: 1; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.exp-tier { font-size: 10px; padding: 0 4px; border-radius: 4px; border: 1px solid var(--line); color: var(--faint); white-space: nowrap; }
.exp-role { font-size: 10.5px; color: var(--dim); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }

/* 卡片右侧动作区：下载（hover 浮现）+ 召唤（常驻、显眼） */
.card-actions {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-shrink: 0;
}
.dl-inline {
  font-size: 13px;
  color: var(--faint);
  opacity: 0;
  transition: opacity 0.14s;
  padding: 2px 5px;
  border-radius: 6px;
  cursor: pointer;
}
.team-card:hover .dl-inline, .exp-card:hover .dl-inline { opacity: 0.55; }
.dl-inline:hover { opacity: 1 !important; color: var(--gold, #d4b06a); background: rgba(255, 255, 255, 0.08); }

.summon-pill {
  flex-shrink: 0;
  padding: 5px 14px;
  border-radius: 8px;
  border: 1px solid rgba(212, 176, 106, 0.5);
  background: rgba(212, 176, 106, 0.12);
  color: var(--gold, #d4b06a);
  font-size: 12px;
  font-weight: 700;
  cursor: pointer;
  white-space: nowrap;
  transition: all 0.14s;
}
.summon-pill:hover {
  background: rgba(212, 176, 106, 0.25);
  border-color: rgba(212, 176, 106, 0.85);
  transform: translateY(-1px);
}
.summon-pill.sm { padding: 4px 11px; font-size: 11px; border-radius: 7px; }
</style>
