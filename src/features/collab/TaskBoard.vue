<script setup lang="ts">
import { computed, ref, watch } from "vue";
import {
  ChevronDown,
  Newspaper,
  Plus,
  RefreshCw,
  X,
  Hand,
  Upload,
  Check,
  Undo2,
  Archive,
  Ban,
  GitBranch,
  LoaderCircle,
  RotateCcw,
  ListChecks,
  History,
  ShieldCheck,
} from "@lucide/vue";
import { useCollabStore } from "./stores/collab";
import MergeConsole from "./MergeConsole.vue";
import TaskChat from "./TaskChat.vue";
import {
  collabApi,
  fmtTime,
  type CardDraft,
  type CheckRun,
  type ReviewComment,
  type ReviewDraft,
  type TaskCard,
  type TaskRound,
  type TaskState,
} from "./api";
import { toast } from "../../composables/useToast";
import { BellRing, Sparkles } from "@lucide/vue";

const collab = useCollabStore();
/** 项目是否任命了主 Agent(空 = 纯人工模式,AI 指挥入口全部隐藏) */
const hasLead = computed(
  () => !!collab.currentProject?.lead_expert_id?.trim()
);

// ── 泳道:五列(已取消的卡并入「归档」列并打标) ──
const LANES: { key: TaskState; label: string; hint: string }[] = [
  { key: "pending", label: "待领取", hint: "还没有待领取的任务" },
  { key: "in_progress", label: "进行中", hint: "没有进行中的任务" },
  { key: "review", label: "待验收", hint: "没有等待验收的任务" },
  { key: "merged", label: "已合并", hint: "还没有已合并的任务" },
  { key: "archived", label: "归档", hint: "归档区是空的" },
];
function laneTasks(lane: TaskState): TaskCard[] {
  if (lane === "archived")
    return collab.tasks.filter(
      (t) => t.state === "archived" || t.state === "cancelled"
    );
  return collab.tasks.filter((t) => t.state === lane);
}

const myName = computed(() => collab.user?.username ?? "");
/** 我的 user_id:登录响应缓存里有就用,否则按用户名从成员表反查 */
const myId = computed<number | null>(() => {
  if (typeof collab.user?.id === "number") return collab.user.id;
  return (
    collab.members.find((m) => m.username === myName.value)?.user_id ?? null
  );
});
const isMine = (t: TaskCard) => t.assignee != null && t.assignee === myId.value;
const canReview = computed(() => collab.canManage); // 验收权:全局 owner 或当前团队 owner

// ── 晨报(collab:morning 推送 + 手动刷新) ──
const morningOpen = ref(false);
const morningBusy = ref(false);
async function refreshMorning() {
  morningBusy.value = true;
  try {
    await collab.refreshMorning();
    morningOpen.value = true;
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    morningBusy.value = false;
  }
}
const MORNING_SECTIONS: { key: keyof NonNullable<typeof collab.morning> & string; label: string }[] = [
  { key: "merged_yesterday", label: "昨日已合并" },
  { key: "review_queue", label: "待验收" },
  { key: "rejected_open", label: "被打回待续改" },
  { key: "stale", label: "超 48h 无动静" },
  { key: "unclaimed", label: "待领取" },
  { key: "escalated", label: "打回熔断" },
];
function morningList(key: string): TaskCard[] {
  const m = collab.morning as Record<string, unknown> | null;
  const v = m?.[key];
  return Array.isArray(v) ? (v as TaskCard[]) : [];
}

// ── 建卡对话框(四要素必填) ──
const showCreate = ref(false);
const form = ref({ title: "", body: "", scope: "", criteria: "" });
const formErr = ref("");
const creating = ref(false);
function openCreate() {
  form.value = { title: "", body: "", scope: "", criteria: "" };
  formErr.value = "";
  showCreate.value = true;
}
async function submitCreate() {
  const f = form.value;
  if (!f.title.trim() || !f.body.trim() || !f.scope.trim() || !f.criteria.trim()) {
    formErr.value = "四要素(标题/正文/范围/验收标准)都必须填写";
    return;
  }
  creating.value = true;
  try {
    await collab.createTask({
      title: f.title.trim(),
      body: f.body.trim(),
      scope: f.scope.trim(),
      criteria: f.criteria.trim(),
    });
    showCreate.value = false;
    toast.info("任务卡已创建");
  } catch (e) {
    formErr.value = (e as Error).message;
  } finally {
    creating.value = false;
  }
}

// ── 详情抽屉 ──
const detail = ref<TaskCard | null>(null);
const rounds = ref<TaskRound[]>([]);
const roundsLoading = ref(false);
const acting = ref(false);
function openDetail(t: TaskCard) {
  detail.value = t;
  reviewMode.value = false;
  aiDraft.value = null;
  openChecks.value = new Set();
  void loadRounds(t.id);
  // 抽屉打开必刷检查(懒加载入口之一;另一入口是 collab:check 推送)
  void collab.refreshChecks(t.id);
}
function closeDetail() {
  detail.value = null;
}
async function loadRounds(taskId: number) {
  roundsLoading.value = true;
  rounds.value = [];
  try {
    rounds.value = await collabApi.taskRounds(taskId);
  } catch {
    /* 无轮次记录或接口失败 → 留空 */
  } finally {
    roundsLoading.value = false;
  }
}
// 看板刷新后详情里的卡片同步到最新状态
watch(
  () => collab.tasks,
  (list) => {
    if (!detail.value) return;
    const fresh = list.find((t) => t.id === detail.value!.id);
    if (fresh) detail.value = fresh;
  }
);

/** 验收标准按行拆成清单(支持 - / 1. 前缀) */
function criteriaItems(t: TaskCard): string[] {
  return t.criteria
    .split(/\r?\n/)
    .map((l) => l.replace(/^\s*(?:[-*•]|\d+[.、)])\s*/, "").trim())
    .filter(Boolean);
}
/** 轮次意见 JSON → 待办清单 */
function roundComments(r: TaskRound): ReviewComment[] {
  try {
    const v = JSON.parse(r.comments);
    if (Array.isArray(v))
      return v.map((c) => ({
        item: String(c?.item ?? ""),
        note: String(c?.note ?? ""),
      }));
  } catch {
    /* 非 JSON → 整段当一条 */
  }
  return r.comments ? [{ item: r.comments, note: "" }] : [];
}

// ── 操作(按状态 × 角色) ──
async function act(fn: () => Promise<void>, okMsg: string) {
  acting.value = true;
  try {
    await fn();
    toast.info(okMsg);
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    acting.value = false;
  }
}
const submitPrId = ref("");
function doClaim(t: TaskCard) {
  void act(() => collab.claim(t.id), `已领取「${t.title}」`);
}
function doSubmit(t: TaskCard) {
  void act(
    () => collab.submit(t.id, submitPrId.value.trim() || undefined),
    "已提交,等待验收"
  );
}
function doArchive(t: TaskCard) {
  void act(() => collab.archive(t.id), "已归档");
}
function doCancel(t: TaskCard) {
  if (!confirm(`取消任务「${t.title}」?`)) return;
  void act(() => collab.cancel(t.id), "已取消");
}
function doPass(t: TaskCard) {
  void act(() => collab.review(t.id, true, []), "验收通过,已合并");
  reviewMode.value = false;
}

// 打回:逐条意见输入
const reviewMode = ref(false);
const reviewItems = ref<ReviewComment[]>([{ item: "", note: "" }]);
function startReject() {
  reviewMode.value = true;
  reviewItems.value = [{ item: "", note: "" }];
}
function addReviewItem() {
  reviewItems.value.push({ item: "", note: "" });
}
function rmReviewItem(i: number) {
  reviewItems.value.splice(i, 1);
}
function doReject(t: TaskCard) {
  const items = reviewItems.value
    .map((c) => ({ item: c.item.trim(), note: c.note.trim() }))
    .filter((c) => c.item);
  if (!items.length) {
    toast.error("打回至少要填一条整改意见");
    return;
  }
  void act(async () => {
    await collab.review(t.id, false, items);
    reviewMode.value = false;
    if (detail.value) void loadRounds(detail.value.id);
  }, "已打回,进入下一轮");
}

// ── AI 拆卡(主 Agent 指挥件①:目标 → 草案 → 人工确认建卡/授权直派) ──
const showAi = ref(false);
const aiGoal = ref("");
const aiDispatch = ref(false);
const aiBusy = ref(false);
const aiErr = ref("");
const aiDrafts = ref<CardDraft[]>([]);
function openAiDecompose() {
  aiGoal.value = "";
  aiErr.value = "";
  aiDrafts.value = [];
  aiDispatch.value = false;
  showAi.value = true;
}
async function runDecompose() {
  const pid = collab.currentProjectId;
  if (!pid || !aiGoal.value.trim()) {
    aiErr.value = "先写清目标,主 Agent 才能拆";
    return;
  }
  aiBusy.value = true;
  aiErr.value = "";
  try {
    const hint = collab.members
      .map((m) => m.display_name || m.username)
      .join("、");
    const r = await collabApi.aiDecompose(
      pid,
      aiGoal.value.trim(),
      hint,
      aiDispatch.value
    );
    aiDrafts.value = r.drafts;
    if (r.created.length) {
      toast.info(`主 Agent 已直接建卡 ${r.created.length} 张`);
      await collab.refreshTasks();
      // 已直派的从草案列表剔除,剩下的(含「待澄清」)留给人工处置
      const made = new Set(r.created.map((c) => c.title));
      aiDrafts.value = r.drafts.filter((d) => !made.has(d.title));
      if (!aiDrafts.value.length) showAi.value = false;
    }
  } catch (e) {
    aiErr.value = (e as Error).message;
  } finally {
    aiBusy.value = false;
  }
}
async function createDraft(d: CardDraft, idx: number) {
  try {
    await collab.createTask({
      title: d.title,
      body: d.body,
      scope: d.scope,
      criteria: d.criteria,
    });
    aiDrafts.value.splice(idx, 1);
    toast.info(`已建卡「${d.title}」`);
    if (!aiDrafts.value.length) showAi.value = false;
  } catch (e) {
    toast.error((e as Error).message);
  }
}

// ── 催办(主 Agent 指挥件⑥) ──
const nudgeBusy = ref(false);
async function doNudge() {
  const pid = collab.currentProjectId;
  if (!pid) return;
  nudgeBusy.value = true;
  try {
    const stale = await collabApi.leadNudge(pid);
    toast.info(
      stale.length
        ? `已催办 ${stale.length} 张超期卡:${stale.map((t) => t.title).join("、")}`
        : "没有超期无动静的卡,无需催办"
    );
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    nudgeBusy.value = false;
  }
}

// ── AI 验收(主 Agent 指挥件③:diff 对照验收标准 → 意见草稿 → 人确认落档) ──
const aiDraft = ref<ReviewDraft | null>(null);
const aiReviewBusy = ref(false);
async function runAiReview(t: TaskCard) {
  aiReviewBusy.value = true;
  aiDraft.value = null;
  try {
    aiDraft.value = await collabApi.aiReview(t.id);
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    aiReviewBusy.value = false;
  }
}
/** 草稿意见 → 逐条打回意见(item 序号映射回验收标准原文) */
function draftComments(t: TaskCard, d: ReviewDraft): ReviewComment[] {
  const items = criteriaItems(t);
  return d.comments.map((c) => ({
    item: items[c.item - 1] ?? `验收标准第 ${c.item} 条`,
    note: c.note,
  }));
}
/** 按草稿落验收:actor 记 lead:<expert>,服务端过 lead.rs 三问 */
function applyAiDraft(t: TaskCard) {
  const d = aiDraft.value;
  if (!d) return;
  void act(async () => {
    await collabApi.reviewTask(t.id, d.pass, draftComments(t, d), true);
    await collab.refreshTasks();
    aiDraft.value = null;
    void loadRounds(t.id);
  }, d.pass ? "主 Agent 验收通过" : "主 Agent 已打回,进入下一轮");
}
/** 转人工:草稿意见填进打回编辑区,由人改定后落档 */
function editAiDraft(t: TaskCard) {
  const d = aiDraft.value;
  if (!d) return;
  reviewItems.value = draftComments(t, d).map((c) => ({ ...c }));
  if (!reviewItems.value.length) reviewItems.value = [{ item: "", note: "" }];
  reviewMode.value = true;
  aiDraft.value = null;
}

// ── 检查工作流(CI-lite status checks:徽章 + 抽屉详情 + 重跑 + owner 强推) ──
const CHECK_LABEL: Record<CheckRun["status"], string> = {
  pass: "通过",
  fail: "未过",
  skipped: "跳过",
  running: "运行中",
};
const detailChecks = computed<CheckRun[]>(() =>
  detail.value ? (collab.checksByTask[detail.value.id] ?? []) : []
);
/** 卡片徽章聚合态:store 里没这张卡的数据就不显示(懒加载,不为每张卡发请求) */
function checkBadge(t: TaskCard): "pass" | "fail" | "running" | null {
  const runs = collab.checksByTask[t.id];
  if (!runs || !runs.length) return null;
  if (runs.some((r) => r.status === "running")) return "running";
  if (runs.some((r) => r.status === "fail")) return "fail";
  return "pass";
}
const BADGE_ICON: Record<string, string> = { pass: "✓", fail: "✗", running: "●" };
const BADGE_TIP: Record<string, string> = {
  pass: "检查全绿",
  fail: "检查有未过项",
  running: "检查运行中",
};
/** 展开的检查输出(按 name) */
const openChecks = ref<Set<string>>(new Set());
function toggleCheck(name: string) {
  const s = new Set(openChecks.value);
  if (s.has(name)) s.delete(name);
  else s.add(name);
  openChecks.value = s;
}
const rerunBusy = ref(false);
async function doRerunChecks(t: TaskCard) {
  rerunBusy.value = true;
  try {
    await collabApi.checksRerun(t.id);
    toast.info("已重新排队检查,结果会实时刷新");
    await collab.refreshChecks(t.id);
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    rerunBusy.value = false;
  }
}
/** owner 强推:检查未过时跳过检查闸 squash(服务端留审计痕) */
const forceMerging = ref(false);
async function doForceSquash(t: TaskCard) {
  if (
    !confirm(
      `本轮检查未全绿。确定跳过检查闸,强推 squash 合并「${t.title}」进 main?(会记入审计)`
    )
  )
    return;
  forceMerging.value = true;
  try {
    const r = await collabApi.mergeSquash(t.id, true);
    toast.info(`已强推合并进 main(${r.commit.slice(0, 8)})`);
    await collab.refreshTasks();
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    forceMerging.value = false;
  }
}

const STATE_LABEL: Record<TaskState, string> = {
  pending: "待领取",
  in_progress: "进行中",
  review: "待验收",
  merged: "已合并",
  archived: "已归档",
  cancelled: "已取消",
};
</script>

<template>
  <div class="board">
    <!-- 打回熔断横幅 -->
    <div v-if="collab.escalation" class="escalate">
      <RotateCcw :size="14" :stroke-width="2" />
      <span>{{ collab.escalation }}</span>
      <button class="esc-close" @click="collab.escalation = null"><X :size="14" /></button>
    </div>

    <!-- 晨报折叠条 -->
    <div v-if="collab.morning" class="morning" :class="{ open: morningOpen }">
      <button class="mo-head" @click="morningOpen = !morningOpen">
        <Newspaper :size="14" :stroke-width="1.8" />
        <span class="mo-title">晨报</span>
        <span class="mo-sum">
          <template v-for="sec in MORNING_SECTIONS" :key="sec.key">
            <span v-if="morningList(sec.key).length" class="mo-chip" :class="sec.key">
              {{ sec.label }} {{ morningList(sec.key).length }}
            </span>
          </template>
          <span v-if="MORNING_SECTIONS.every((s2) => !morningList(s2.key).length)" class="mo-chip">
            一切安好,没有待办
          </span>
        </span>
        <ChevronDown :size="14" class="mo-arrow" :class="{ up: morningOpen }" />
      </button>
      <div v-if="morningOpen" class="mo-body">
        <div v-for="sec in MORNING_SECTIONS" :key="sec.key" class="mo-sec">
          <template v-if="morningList(sec.key).length">
            <div class="mo-sec-name">{{ sec.label }}({{ morningList(sec.key).length }})</div>
            <button
              v-for="t in morningList(sec.key)"
              :key="t.id"
              class="mo-item"
              @click="openDetail(t)"
            >
              <span class="mo-item-title">{{ t.title }}</span>
              <span v-if="t.assignee != null" class="mo-item-who">{{ collab.memberName(t.assignee) }}</span>
              <span v-if="t.round >= 1" class="tc-round">⟲{{ t.round }}</span>
            </button>
          </template>
        </div>
      </div>
    </div>

    <div class="board-head">
      <span class="bh-title">任务看板</span>
      <span v-if="collab.loadingTasks" class="bh-loading">
        <LoaderCircle :size="13" :stroke-width="2" class="spin" /> 刷新中
      </span>
      <button class="bh-morning" title="拉取最新晨报" :disabled="morningBusy" @click="refreshMorning">
        <RefreshCw v-if="!morningBusy" :size="13" :stroke-width="1.9" />
        <LoaderCircle v-else :size="13" class="spin" />
        晨报
      </button>
      <button
        v-if="canReview && hasLead"
        class="bh-morning"
        title="主 Agent 盘点超期无动静的卡并留痕催办"
        :disabled="nudgeBusy"
        @click="doNudge"
      >
        <BellRing v-if="!nudgeBusy" :size="13" :stroke-width="1.9" />
        <LoaderCircle v-else :size="13" class="spin" />
        催办
      </button>
      <button
        v-if="canReview && hasLead"
        class="bh-morning ai"
        title="把目标交给主 Agent 拆成任务卡草案(建卡由你确认)"
        @click="openAiDecompose"
      >
        <Sparkles :size="13" :stroke-width="1.9" /> AI 拆卡
      </button>
      <button class="bh-new" @click="openCreate"><Plus :size="14" :stroke-width="2" /> 新建任务卡</button>
    </div>

    <div class="lanes">
      <div v-for="lane in LANES" :key="lane.key" class="lane">
        <div class="lane-head">
          <span class="lane-name">{{ lane.label }}</span>
          <span class="lane-count">{{ laneTasks(lane.key).length }}</span>
        </div>
        <div class="lane-body">
          <button
            v-for="t in laneTasks(lane.key)"
            :key="t.id"
            class="tcard"
            :class="{ cancelled: t.state === 'cancelled' }"
            @click="openDetail(t)"
          >
            <div class="tc-title">{{ t.title }}</div>
            <div class="tc-meta">
              <span v-if="t.assignee != null" class="tc-assignee" :class="{ me: isMine(t) }">
                {{ collab.memberName(t.assignee) }}{{ isMine(t) ? "(我)" : "" }}
              </span>
              <span v-else class="tc-assignee free">无人认领</span>
              <span v-if="t.round >= 1" class="tc-round" :title="`已打回 ${t.round} 轮`">⟲{{ t.round }}</span>
              <span
                v-if="(t.state === 'review' || t.state === 'in_progress') && checkBadge(t)"
                class="tc-check"
                :class="checkBadge(t) ?? ''"
                :title="BADGE_TIP[checkBadge(t) ?? ''] ?? ''"
              >{{ BADGE_ICON[checkBadge(t) ?? ""] ?? "" }}</span>
              <span v-if="t.state === 'cancelled'" class="tc-cancel-tag">已取消</span>
            </div>
            <div v-if="t.branch" class="tc-branch">
              <GitBranch :size="11" :stroke-width="1.8" /> {{ t.branch }}
            </div>
          </button>
          <div v-if="!laneTasks(lane.key).length" class="lane-empty">{{ lane.hint }}</div>
        </div>
      </div>
    </div>

    <!-- ── 建卡对话框 ── -->
    <div v-if="showCreate" class="mask" @click.self="showCreate = false">
      <div class="dialog">
        <div class="dlg-head">
          <span>新建任务卡</span>
          <button class="dlg-x" @click="showCreate = false"><X :size="16" /></button>
        </div>
        <p class="dlg-tip">四要素必填:说清「做什么、动哪里、怎么算完成」,协作才不扯皮。</p>
        <label class="fld">
          <span>标题 *</span>
          <input v-model="form.title" placeholder="一句话说清这张卡要做什么" />
        </label>
        <label class="fld">
          <span>正文 *</span>
          <textarea v-model="form.body" rows="3" placeholder="背景与具体要求"></textarea>
        </label>
        <label class="fld">
          <span>范围(scope) *</span>
          <textarea v-model="form.scope" rows="2" placeholder="允许改动的文件/模块边界,越界需先沟通"></textarea>
        </label>
        <label class="fld">
          <span>验收标准 *</span>
          <textarea v-model="form.criteria" rows="3" placeholder="每行一条,验收时逐条核对&#10;- 构建通过&#10;- xx 功能可用"></textarea>
        </label>
        <div v-if="formErr" class="dlg-err">{{ formErr }}</div>
        <div class="dlg-act">
          <button class="btn ghost" @click="showCreate = false">取消</button>
          <button class="btn solid" :disabled="creating" @click="submitCreate">
            <LoaderCircle v-if="creating" :size="13" class="spin" /> 创建
          </button>
        </div>
      </div>
    </div>

    <!-- ── AI 拆卡对话框(草案 → 人工确认建卡) ── -->
    <div v-if="showAi" class="mask" @click.self="showAi = false">
      <div class="dialog">
        <div class="dlg-head">
          <span><Sparkles :size="15" :stroke-width="1.9" /> AI 拆卡(主 Agent)</span>
          <button class="dlg-x" @click="showAi = false"><X :size="16" /></button>
        </div>
        <p class="dlg-tip">
          把目标写清楚,主 Agent 拆成四要素齐全的任务卡草案。写不出可判定验收标准的部分会拆成「待澄清」卡,不硬拆。
        </p>
        <label class="fld">
          <span>目标 *</span>
          <textarea v-model="aiGoal" rows="4" placeholder="例:本周把落地页改版上线——新首屏、接入统计、移动端适配"></textarea>
        </label>
        <label class="ai-dispatch">
          <input v-model="aiDispatch" type="checkbox" />
          <span>拆完直接建卡并派发(需 auto_dispatch 授权位,「待澄清」卡仍留人工)</span>
        </label>
        <div v-if="aiErr" class="dlg-err">{{ aiErr }}</div>
        <div class="dlg-act">
          <button class="btn ghost" @click="showAi = false">关闭</button>
          <button class="btn solid" :disabled="aiBusy" @click="runDecompose">
            <LoaderCircle v-if="aiBusy" :size="13" class="spin" />
            <Sparkles v-else :size="13" :stroke-width="1.9" />
            拆解
          </button>
        </div>
        <div v-if="aiDrafts.length" class="drafts">
          <div class="drafts-head">草案({{ aiDrafts.length }})——逐张确认建卡</div>
          <div v-for="(d, i) in aiDrafts" :key="`${d.title}-${i}`" class="draft">
            <div class="draft-title">{{ d.title }}</div>
            <div class="draft-body">{{ d.body }}</div>
            <div class="draft-meta"><b>范围</b>:{{ d.scope }}</div>
            <div class="draft-meta"><b>验收</b>:{{ d.criteria }}</div>
            <button class="btn solid sm" @click="createDraft(d, i)">
              <Plus :size="12" :stroke-width="2" /> 建卡
            </button>
          </div>
        </div>
      </div>
    </div>

    <!-- ── 详情抽屉 ── -->
    <Transition name="drawer">
      <div v-if="detail" class="drawer-mask" @click.self="closeDetail">
        <aside class="drawer">
          <div class="dr-head">
            <span class="dr-state" :class="detail.state">{{ STATE_LABEL[detail.state] }}</span>
            <span v-if="detail.round >= 1" class="tc-round">⟲{{ detail.round }}</span>
            <button class="dlg-x" @click="closeDetail"><X :size="16" /></button>
          </div>
          <h2 class="dr-title">{{ detail.title }}</h2>
          <div class="dr-meta">
            <span v-if="detail.assignee != null">负责人:{{ collab.memberName(detail.assignee) }}</span>
            <span v-else>暂无负责人</span>
            <span v-if="detail.branch" class="tc-branch"><GitBranch :size="11" /> {{ detail.branch }}</span>
            <span v-if="detail.pr_id">PR:{{ detail.pr_id }}</span>
          </div>

          <section class="dr-sec">
            <h3>正文</h3>
            <p class="dr-body">{{ detail.body }}</p>
          </section>
          <section class="dr-sec">
            <h3>范围</h3>
            <p class="dr-body">{{ detail.scope }}</p>
          </section>
          <section class="dr-sec">
            <h3><ListChecks :size="13" :stroke-width="1.9" /> 验收标准</h3>
            <ul class="dr-criteria">
              <li v-for="(c, i) in criteriaItems(detail)" :key="i">{{ c }}</li>
            </ul>
          </section>

          <!-- 检查(CI-lite status checks:提交送验自动跑,重跑/强推给管理者) -->
          <section class="dr-sec">
            <h3>
              <ShieldCheck :size="13" :stroke-width="1.9" /> 检查
              <button
                v-if="canReview"
                class="ck-rerun"
                title="重新跑本轮检查"
                :disabled="rerunBusy"
                @click="doRerunChecks(detail)"
              >
                <LoaderCircle v-if="rerunBusy" :size="12" class="spin" />
                <RefreshCw v-else :size="12" :stroke-width="1.9" />
                重跑
              </button>
            </h3>
            <div v-if="!detailChecks.length" class="dr-dim">
              还没有检查记录(提交送验时自动触发)
            </div>
            <div v-for="r in detailChecks" :key="r.name" class="ck">
              <button class="ck-head" @click="toggleCheck(r.name)">
                <span class="ck-dot" :class="r.status"></span>
                <span class="ck-name">{{ r.name }}</span>
                <span class="ck-status" :class="r.status">{{ CHECK_LABEL[r.status] }}</span>
                <ChevronDown :size="12" class="ck-arrow" :class="{ up: openChecks.has(r.name) }" />
              </button>
              <pre v-if="openChecks.has(r.name)" class="ck-out">{{ r.output || "(无输出)" }}</pre>
            </div>
            <div
              v-if="canReview && detail.state === 'review' && detailChecks.some((r) => r.status === 'fail')"
              class="ck-force"
            >
              <button class="btn danger" :disabled="forceMerging" @click="doForceSquash(detail)">
                <LoaderCircle v-if="forceMerging" :size="12" class="spin" />
                强推合并(跳过检查)
              </button>
              <span class="ck-force-hint">检查未过;owner 可强推,会记入审计</span>
            </div>
          </section>

          <!-- 任务级对话:协作者↔负责人↔主 Agent 的多轮微调通道 -->
          <section class="dr-sec">
            <TaskChat :task-id="detail.id" />
          </section>

          <section class="dr-sec">
            <h3><History :size="13" :stroke-width="1.9" /> 轮次历史</h3>
            <div v-if="roundsLoading" class="dr-dim"><LoaderCircle :size="13" class="spin" /> 加载中…</div>
            <div v-else-if="!rounds.length" class="dr-dim">还没有验收记录</div>
            <div v-for="r in rounds" :key="r.round" class="round">
              <div class="round-head">
                <span class="round-no">第 {{ r.round }} 轮</span>
                <span class="round-verdict" :class="{ pass: r.verdict === 'pass' }">
                  {{ r.verdict === "pass" ? "通过" : "打回" }}
                </span>
                <span class="round-by">{{ r.reviewer }} · {{ fmtTime(r.created_at) }}</span>
              </div>
              <ul v-if="roundComments(r).length" class="round-todos">
                <li v-for="(c, i) in roundComments(r)" :key="i">
                  <span class="todo-item">{{ c.item }}</span>
                  <span v-if="c.note" class="todo-note">{{ c.note }}</span>
                </li>
              </ul>
            </div>
          </section>

          <!-- 冲突裁决台(待验收:试算/放行;已合并:回滚) -->
          <MergeConsole
            v-if="detail.state === 'review' || detail.state === 'merged'"
            :task="detail"
            @reject="startReject"
          />

          <!-- AI 验收意见草稿(不落状态机,人确认才落档) -->
          <section v-if="aiDraft && detail.state === 'review'" class="dr-sec ai-draft">
            <h3><Sparkles :size="13" :stroke-width="1.9" /> 主 Agent 验收草稿</h3>
            <div class="ai-verdict" :class="{ pass: aiDraft.pass }">
              建议:{{ aiDraft.pass ? "通过" : "打回" }} —— {{ aiDraft.summary }}
            </div>
            <ul v-if="aiDraft.comments.length" class="round-todos">
              <li v-for="(c, i) in draftComments(detail, aiDraft)" :key="i">
                <span class="todo-item">{{ c.item }}</span>
                <span v-if="c.note" class="todo-note">{{ c.note }}</span>
              </li>
            </ul>
            <div class="ai-draft-acts">
              <button class="btn solid sm" :disabled="acting" @click="applyAiDraft(detail)">
                <Check :size="12" :stroke-width="2" />
                按草稿落验收({{ aiDraft.pass ? "通过" : "打回" }})
              </button>
              <button v-if="!aiDraft.pass" class="btn ghost sm" @click="editAiDraft(detail)">
                转人工编辑
              </button>
              <button class="btn ghost sm" @click="aiDraft = null">忽略</button>
            </div>
          </section>

          <!-- 打回:逐条意见 -->
          <section v-if="reviewMode" class="dr-sec reject-box">
            <h3>打回意见(逐条)</h3>
            <div v-for="(c, i) in reviewItems" :key="i" class="rj-row">
              <input v-model="c.item" placeholder="问题点(必填)" />
              <input v-model="c.note" placeholder="整改建议(可选)" />
              <button v-if="reviewItems.length > 1" class="dlg-x" title="删除这条" @click="rmReviewItem(i)"><X :size="14" /></button>
            </div>
            <button class="btn ghost sm" @click="addReviewItem"><Plus :size="13" /> 再加一条</button>
          </section>

          <!-- 操作区(按状态 × 角色) -->
          <div class="dr-actions">
            <template v-if="detail.state === 'pending'">
              <button class="btn solid" :disabled="acting" @click="doClaim(detail)">
                <Hand :size="14" :stroke-width="1.9" /> 领取任务
              </button>
            </template>
            <template v-else-if="detail.state === 'in_progress' && isMine(detail)">
              <input v-model="submitPrId" class="pr-input" placeholder="PR 编号(可选)" />
              <button class="btn solid" :disabled="acting" @click="doSubmit(detail)">
                <Upload :size="14" :stroke-width="1.9" /> 提交验收
              </button>
            </template>
            <template v-else-if="detail.state === 'review' && canReview">
              <template v-if="!reviewMode">
                <button class="btn solid" :disabled="acting" @click="doPass(detail)">
                  <Check :size="14" :stroke-width="2" /> 验收通过
                </button>
                <button class="btn warn" :disabled="acting" @click="startReject">
                  <Undo2 :size="14" :stroke-width="1.9" /> 打回
                </button>
                <button
                  v-if="hasLead"
                  class="btn ghost"
                  :disabled="aiReviewBusy"
                  title="主 Agent 取分支 diff,对照验收标准逐条出草稿(不落状态机)"
                  @click="runAiReview(detail)"
                >
                  <LoaderCircle v-if="aiReviewBusy" :size="13" class="spin" />
                  <Sparkles v-else :size="13" :stroke-width="1.9" />
                  AI 验收
                </button>
              </template>
              <template v-else>
                <button class="btn warn" :disabled="acting" @click="doReject(detail)">确认打回</button>
                <button class="btn ghost" @click="reviewMode = false">取消</button>
              </template>
            </template>
            <template v-if="detail.state === 'merged' && canReview">
              <button class="btn ghost" :disabled="acting" @click="doArchive(detail)">
                <Archive :size="14" :stroke-width="1.8" /> 归档
              </button>
            </template>
            <button
              v-if="canReview && detail.state !== 'archived' && detail.state !== 'cancelled' && detail.state !== 'merged'"
              class="btn danger"
              :disabled="acting"
              @click="doCancel(detail)"
            >
              <Ban :size="14" :stroke-width="1.8" /> 取消任务
            </button>
          </div>
        </aside>
      </div>
    </Transition>
  </div>
</template>

<style scoped>
.board { flex: 1; display: flex; flex-direction: column; min-height: 0; min-width: 0; }
.escalate {
  display: flex; align-items: center; gap: 8px;
  margin: 12px 16px 0; padding: 8px 12px;
  border: 1px solid var(--vermilion); border-radius: 9px;
  background: color-mix(in srgb, var(--vermilion) 8%, transparent);
  color: var(--vermilion); font-size: 12.5px;
}
.esc-close { margin-left: auto; border: none; background: none; color: inherit; cursor: pointer; display: inline-flex; }

.board-head {
  display: flex; align-items: center; gap: 12px;
  padding: 14px 16px 10px;
}
/* 晨报折叠条 */
.morning {
  margin: 12px 16px 0;
  border: 1px solid var(--border-soft); border-radius: 11px;
  background: var(--panel);
}
.mo-head {
  width: 100%;
  display: flex; align-items: center; gap: 8px;
  border: none; background: none; cursor: pointer;
  padding: 9px 13px; text-align: left;
  color: var(--ink); font-size: 12.5px; font-weight: 600;
}
.mo-title { letter-spacing: 2px; font-family: var(--serif); }
.mo-sum { display: flex; flex-wrap: wrap; gap: 6px; flex: 1; min-width: 0; }
.mo-chip {
  font-size: 10.5px; font-weight: 500; color: var(--text-2);
  background: var(--selection-bg); padding: 2px 8px; border-radius: 20px;
}
.mo-chip.escalated, .mo-chip.stale { color: var(--vermilion); background: color-mix(in srgb, var(--vermilion) 10%, transparent); }
.mo-chip.review_queue { color: var(--primary); background: var(--primary-soft); }
.mo-arrow { color: var(--muted); transition: transform 0.15s; flex-shrink: 0; }
.mo-arrow.up { transform: rotate(180deg); }
.mo-body { border-top: 1px solid var(--border-soft); padding: 10px 13px; display: flex; flex-direction: column; gap: 8px; max-height: 260px; overflow-y: auto; }
.mo-sec-name { font-size: 11px; font-weight: 600; color: var(--muted); letter-spacing: 1px; margin-bottom: 4px; }
.mo-item {
  width: 100%;
  display: flex; align-items: center; gap: 8px;
  border: none; background: none; cursor: pointer;
  padding: 4px 6px; border-radius: 7px; text-align: left;
  font-size: 12px; color: var(--text);
}
.mo-item:hover { background: var(--selection-bg); }
.mo-item-title { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.mo-item-who { margin-left: auto; flex-shrink: 0; font-size: 10.5px; color: var(--muted); }
.bh-morning {
  display: inline-flex; align-items: center; gap: 5px;
  border: 1px solid var(--border); background: transparent;
  color: var(--text-2); cursor: pointer;
  font-size: 12px; padding: 6px 11px; border-radius: 8px;
}
.bh-morning:hover:not(:disabled) { color: var(--ink); border-color: var(--ink); }
.bh-morning:disabled { opacity: 0.55; cursor: not-allowed; }
.bh-title { font-family: var(--serif); font-size: 15px; font-weight: 600; letter-spacing: 2px; color: var(--ink); }
.bh-loading { display: inline-flex; align-items: center; gap: 5px; font-size: 11.5px; color: var(--muted); }
.bh-new {
  margin-left: auto;
  display: inline-flex; align-items: center; gap: 5px;
  border: none; cursor: pointer;
  background: var(--btn-solid-bg); color: var(--btn-solid-text);
  font-size: 12.5px; padding: 7px 13px; border-radius: 8px;
}
.bh-new:hover { background: var(--primary); }

.lanes {
  flex: 1; min-height: 0;
  display: flex; gap: 10px;
  padding: 0 16px 16px;
  overflow-x: auto;
}
.lane {
  flex: 1 0 200px; min-width: 200px; max-width: 300px;
  display: flex; flex-direction: column; min-height: 0;
  background: var(--bg-soft, var(--selection-bg));
  border: 1px solid var(--border-soft);
  border-radius: 12px;
}
.lane-head {
  display: flex; align-items: center; gap: 7px;
  padding: 10px 12px;
  border-bottom: 1px solid var(--border-soft);
}
.lane-name { font-size: 12.5px; font-weight: 600; color: var(--ink); letter-spacing: 1px; }
.lane-count {
  font-size: 11px; color: var(--muted);
  background: var(--panel); border: 1px solid var(--border-soft);
  padding: 0 7px; border-radius: 10px;
}
.lane-body { flex: 1; overflow-y: auto; padding: 10px; display: flex; flex-direction: column; gap: 8px; }
.lane-empty { font-size: 11.5px; color: var(--dim); text-align: center; padding: 24px 6px; font-style: italic; }

.tcard {
  text-align: left; cursor: pointer;
  border: 1px solid var(--border-soft); border-radius: 10px;
  background: var(--panel); padding: 10px 11px;
  transition: border-color 0.15s, box-shadow 0.15s, transform 0.15s;
}
.tcard:hover { border-color: var(--border); box-shadow: 0 4px 14px rgba(0,0,0,0.06); transform: translateY(-1px); }
.tcard.cancelled { opacity: 0.6; }
.tc-title { font-size: 12.5px; font-weight: 600; color: var(--ink); line-height: 1.5; word-break: break-word; }
.tc-meta { display: flex; align-items: center; flex-wrap: wrap; gap: 6px; margin-top: 7px; }
.tc-assignee {
  font-size: 10.5px; color: var(--text-2);
  background: var(--selection-bg); padding: 2px 7px; border-radius: 20px;
}
.tc-assignee.me { color: var(--primary); background: var(--primary-soft); }
.tc-assignee.free { color: var(--dim); font-style: italic; background: transparent; }
.tc-round {
  font-size: 10.5px; font-weight: 700; color: var(--vermilion);
  background: color-mix(in srgb, var(--vermilion) 10%, transparent);
  padding: 1px 7px; border-radius: 20px;
}
.tc-cancel-tag { font-size: 10.5px; color: var(--muted); border: 1px solid var(--border); padding: 1px 6px; border-radius: 5px; }
/* 检查徽章(卡片角落) */
.tc-check {
  width: 16px; height: 16px; border-radius: 50%; flex-shrink: 0;
  display: inline-flex; align-items: center; justify-content: center;
  font-size: 10.5px; font-weight: 700; line-height: 1;
}
.tc-check.pass { color: #1f9d55; background: color-mix(in srgb, #1f9d55 12%, transparent); }
.tc-check.fail { color: var(--vermilion); background: color-mix(in srgb, var(--vermilion) 12%, transparent); }
.tc-check.running { color: #b8860b; background: color-mix(in srgb, #b8860b 12%, transparent); animation: breathe 1.6s ease-in-out infinite; }
@keyframes breathe { 0%, 100% { opacity: 1; } 50% { opacity: 0.4; } }
.tc-branch {
  display: inline-flex; align-items: center; gap: 4px;
  margin-top: 6px; font-size: 10.5px; color: var(--muted); font-family: var(--mono);
  word-break: break-all;
}

/* 弹窗 */
.mask {
  position: fixed; inset: 0; z-index: 60;
  background: rgba(0,0,0,0.35);
  display: flex; align-items: center; justify-content: center;
}
.dialog {
  width: min(520px, 92vw); max-height: 86vh; overflow-y: auto;
  background: var(--panel); border: 1px solid var(--border-soft);
  border-radius: 14px; padding: 18px 20px; box-shadow: var(--shadow-lg, var(--shadow));
}
.dlg-head { display: flex; align-items: center; justify-content: space-between; font-size: 15px; font-weight: 600; color: var(--ink); font-family: var(--serif); letter-spacing: 1px; }
.dlg-x { border: none; background: none; color: var(--muted); cursor: pointer; display: inline-flex; padding: 4px; border-radius: 6px; }
.dlg-x:hover { color: var(--ink); background: var(--selection-bg); }
.dlg-tip { margin: 8px 0 14px; font-size: 12px; color: var(--text-2); line-height: 1.7; }
.fld { display: flex; flex-direction: column; gap: 5px; margin-bottom: 12px; }
.fld span { font-size: 12px; color: var(--text-2); }
.fld input, .fld textarea, .pr-input, .rj-row input {
  border: 1px solid var(--border); border-radius: 8px;
  background: var(--bg); color: var(--ink);
  font-size: 13px; padding: 8px 10px; font-family: inherit;
  resize: vertical;
}
.fld input:focus, .fld textarea:focus, .pr-input:focus, .rj-row input:focus { outline: none; border-color: var(--primary, var(--ink)); }
.dlg-err { color: var(--vermilion); font-size: 12px; margin-bottom: 10px; }
.dlg-act { display: flex; justify-content: flex-end; gap: 8px; }

.btn {
  display: inline-flex; align-items: center; gap: 5px;
  border: none; cursor: pointer;
  font-size: 12.5px; padding: 7px 14px; border-radius: 8px;
}
.btn:disabled { opacity: 0.55; cursor: not-allowed; }
.btn.solid { background: var(--btn-solid-bg); color: var(--btn-solid-text); }
.btn.solid:hover:not(:disabled) { background: var(--primary); }
.btn.ghost { background: transparent; color: var(--text-2); border: 1px solid var(--border); }
.btn.ghost:hover:not(:disabled) { color: var(--ink); border-color: var(--ink); }
.btn.ghost.sm { padding: 5px 10px; font-size: 11.5px; }
.btn.warn { background: color-mix(in srgb, var(--vermilion) 12%, transparent); color: var(--vermilion); border: 1px solid var(--vermilion); }
.btn.danger { background: transparent; color: var(--vermilion); border: 1px solid var(--border); }
.btn.danger:hover:not(:disabled) { border-color: var(--vermilion); }

/* 详情抽屉 */
.drawer-mask { position: fixed; inset: 0; z-index: 60; background: rgba(0,0,0,0.3); display: flex; justify-content: flex-end; }
.drawer {
  width: min(480px, 94vw); height: 100%;
  background: var(--panel); border-left: 1px solid var(--border-soft);
  padding: 18px 22px 30px; overflow-y: auto;
  display: flex; flex-direction: column; gap: 4px;
}
.drawer-enter-active, .drawer-leave-active { transition: opacity 0.18s ease; }
.drawer-enter-active .drawer, .drawer-leave-active .drawer { transition: transform 0.2s ease; }
.drawer-enter-from, .drawer-leave-to { opacity: 0; }
.drawer-enter-from .drawer, .drawer-leave-to .drawer { transform: translateX(24px); }
.dr-head { display: flex; align-items: center; gap: 8px; }
.dr-head .dlg-x { margin-left: auto; }
.dr-state {
  font-size: 11px; font-weight: 600; letter-spacing: 1px;
  padding: 3px 9px; border-radius: 20px;
  background: var(--selection-bg); color: var(--text-2);
}
.dr-state.review { color: var(--primary); background: var(--primary-soft); }
.dr-state.merged { color: #1f9d55; background: color-mix(in srgb, #1f9d55 12%, transparent); }
.dr-state.cancelled, .dr-state.archived { color: var(--muted); }
.dr-title { margin: 10px 0 4px; font-size: 18px; font-weight: 600; color: var(--ink); font-family: var(--serif); line-height: 1.5; }
.dr-meta { display: flex; flex-wrap: wrap; gap: 10px; font-size: 11.5px; color: var(--muted); margin-bottom: 8px; }
.dr-sec { margin-top: 14px; }
.dr-sec h3 {
  display: flex; align-items: center; gap: 5px;
  margin: 0 0 6px; font-size: 12px; font-weight: 600;
  color: var(--text-2); letter-spacing: 1px;
}
.dr-body { margin: 0; font-size: 13px; line-height: 1.8; color: var(--text); white-space: pre-wrap; word-break: break-word; }
.dr-criteria { margin: 0; padding-left: 18px; font-size: 13px; line-height: 1.9; color: var(--text); }
.dr-dim { display: inline-flex; align-items: center; gap: 6px; font-size: 12px; color: var(--dim); font-style: italic; }
.round { border: 1px solid var(--border-soft); border-radius: 10px; padding: 9px 11px; margin-bottom: 8px; }
.round-head { display: flex; align-items: center; gap: 8px; font-size: 11.5px; }
.round-no { font-weight: 600; color: var(--ink); }
.round-verdict { color: var(--vermilion); font-weight: 600; }
.round-verdict.pass { color: #1f9d55; }
.round-by { margin-left: auto; color: var(--muted); }
.round-todos { margin: 7px 0 0; padding-left: 16px; font-size: 12px; line-height: 1.8; color: var(--text); }
.todo-note { color: var(--muted); margin-left: 6px; }
/* 检查段(抽屉) */
.ck { border: 1px solid var(--border-soft); border-radius: 9px; margin-bottom: 6px; overflow: hidden; }
.ck-head {
  width: 100%; display: flex; align-items: center; gap: 7px;
  border: none; background: none; cursor: pointer; text-align: left;
  padding: 7px 10px; font-size: 12px; color: var(--text);
}
.ck-head:hover { background: var(--selection-bg); }
.ck-dot { width: 8px; height: 8px; border-radius: 50%; flex-shrink: 0; }
.ck-dot.pass { background: #1f9d55; }
.ck-dot.fail { background: var(--vermilion); }
.ck-dot.skipped { background: var(--muted); }
.ck-dot.running { background: #b8860b; animation: breathe 1.6s ease-in-out infinite; }
.ck-name {
  flex: 1; min-width: 0;
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  font-family: var(--mono); font-size: 11.5px;
}
.ck-status { font-size: 10.5px; font-weight: 600; flex-shrink: 0; }
.ck-status.pass { color: #1f9d55; }
.ck-status.fail { color: var(--vermilion); }
.ck-status.skipped { color: var(--muted); }
.ck-status.running { color: #b8860b; }
.ck-arrow { color: var(--muted); transition: transform 0.15s; flex-shrink: 0; }
.ck-arrow.up { transform: rotate(180deg); }
.ck-out {
  margin: 0; padding: 8px 10px;
  border-top: 1px solid var(--border-soft); background: var(--bg);
  font-family: var(--mono); font-size: 11px; line-height: 1.6;
  color: var(--text); white-space: pre-wrap; word-break: break-all;
  max-height: 260px; overflow: auto;
}
.ck-rerun {
  margin-left: auto; display: inline-flex; align-items: center; gap: 4px;
  border: 1px solid var(--border); background: transparent; cursor: pointer;
  font-size: 10.5px; color: var(--text-2); padding: 2px 8px; border-radius: 12px;
}
.ck-rerun:hover:not(:disabled) { color: var(--ink); border-color: var(--ink); }
.ck-rerun:disabled { opacity: 0.55; cursor: not-allowed; }
.ck-force { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; margin-top: 8px; }
.ck-force .btn { padding: 5px 10px; font-size: 11.5px; }
.ck-force-hint { font-size: 10.5px; color: var(--muted); }

.reject-box { border: 1px dashed var(--vermilion); border-radius: 10px; padding: 10px 12px; }
.rj-row { display: flex; gap: 6px; margin-bottom: 7px; }
.rj-row input { flex: 1; min-width: 0; font-size: 12px; padding: 6px 9px; }
.dr-actions {
  margin-top: 18px; padding-top: 14px;
  border-top: 1px solid var(--border-soft);
  display: flex; flex-wrap: wrap; gap: 8px; align-items: center;
}
.pr-input { width: 130px; font-size: 12px; padding: 6px 9px; }

/* AI 拆卡 / AI 验收 */
.bh-morning.ai { color: var(--primary, var(--ink)); }
.ai-dispatch { display: flex; align-items: center; gap: 7px; font-size: 11.5px; color: var(--text-2); margin-bottom: 12px; cursor: pointer; }
.ai-dispatch input { accent-color: var(--primary, var(--ink)); }
.drafts { margin-top: 14px; border-top: 1px solid var(--border-soft); padding-top: 12px; }
.drafts-head { font-size: 11.5px; font-weight: 600; color: var(--text-2); letter-spacing: 1px; margin-bottom: 8px; }
.draft { border: 1px solid var(--border-soft); border-radius: 10px; padding: 10px 12px; margin-bottom: 8px; display: flex; flex-direction: column; gap: 4px; }
.draft-title { font-size: 13px; font-weight: 600; color: var(--ink); }
.draft-body { font-size: 12px; color: var(--text); line-height: 1.7; white-space: pre-wrap; }
.draft-meta { font-size: 11px; color: var(--muted); line-height: 1.6; white-space: pre-wrap; }
.draft .btn.sm { align-self: flex-end; padding: 4px 11px; font-size: 11.5px; }
.btn.solid.sm { padding: 4px 11px; font-size: 11.5px; }
.ai-draft { border: 1px dashed var(--primary, var(--border)); border-radius: 10px; padding: 10px 12px; }
.ai-verdict { font-size: 12.5px; font-weight: 600; color: var(--vermilion); margin-bottom: 6px; }
.ai-verdict.pass { color: #1f9d55; }
.ai-draft-acts { display: flex; flex-wrap: wrap; gap: 7px; margin-top: 9px; }

.spin { animation: spin 0.9s linear infinite; }
@keyframes spin { to { transform: rotate(360deg); } }
</style>
