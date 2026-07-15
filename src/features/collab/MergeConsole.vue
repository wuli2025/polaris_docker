<script setup lang="ts">
/**
 * 冲突裁决台:合并前试算(无副作用) → 干净则 owner 可 squash 放行;
 * 有冲突则逐文件逐块并排展示 main 侧/分支侧(diff3 base 可展开),
 * 逐块三处置(v8 C4):采纳 main 侧 / 采纳分支侧 / 融合草案(可让 AI 起草,人改定)。
 * 全部块处置齐 → 一次性落成任务分支上的合并提交(永不直写 main);
 * 方向性分歧则「整单打回」(复用打回流程,由父层 TaskBoard 提供)。
 */
import { computed, ref, watch } from "vue";
import {
  Check,
  ChevronDown,
  GitMerge,
  LoaderCircle,
  RefreshCw,
  Sparkles,
  TriangleAlert,
  Undo2,
  RotateCcw,
} from "@lucide/vue";
import {
  collabApi,
  type BlockResolution,
  type MergeTrial,
  type TaskCard,
} from "./api";
import { useCollabStore } from "./stores/collab";
import { toast } from "../../composables/useToast";

const props = defineProps<{ task: TaskCard }>();
const emit = defineEmits<{
  /** 整单打回快捷入口:交回父层走既有打回流程 */
  (e: "reject"): void;
}>();

const collab = useCollabStore();
const trial = ref<MergeTrial | null>(null);
const trialErr = ref("");
const loading = ref(false);
const merging = ref(false);
/** squash 成功后的 commit(会话内记住,merged 态显示短哈希+回滚) */
const commit = ref("");
/** 展开的 diff3 base 块(file:index) */
const openBase = ref<Set<string>>(new Set());

// ── 逐块处置状态(file → 每块一条决定) ──
type BlockPick = {
  choice: "" | "ours" | "theirs" | "manual";
  text: string;
  fusing: boolean;
};
const picks = ref<Record<string, BlockPick[]>>({});

async function runTrial() {
  loading.value = true;
  trialErr.value = "";
  try {
    trial.value = await collabApi.mergeTrial(props.task.id);
    // 试算刷新 → 处置重置(冲突态势可能已变化,不保留旧决定)
    const next: Record<string, BlockPick[]> = {};
    if (trial.value && !trial.value.clean) {
      for (const f of trial.value.conflictFiles) {
        next[f] = (trial.value.conflictBlocks[f] ?? []).map(() => ({
          choice: "",
          text: "",
          fusing: false,
        }));
      }
    }
    picks.value = next;
  } catch (e) {
    trial.value = null;
    trialErr.value = (e as Error).message;
  } finally {
    loading.value = false;
  }
}

const totalBlocks = computed(() =>
  Object.values(picks.value).reduce((n, arr) => n + arr.length, 0)
);
const decidedBlocks = computed(() =>
  Object.values(picks.value).reduce(
    (n, arr) =>
      n +
      arr.filter((p) => p.choice && (p.choice !== "manual" || p.text.trim()))
        .length,
    0
  )
);
/** 冲突块解析不可用的文件(块数为 0)无法逐块裁决,只能整单打回 */
const undecidable = computed(
  () =>
    !!trial.value &&
    !trial.value.clean &&
    trial.value.conflictFiles.some(
      (f) => !(trial.value!.conflictBlocks[f] ?? []).length
    )
);
const allDecided = computed(
  () =>
    totalBlocks.value > 0 &&
    decidedBlocks.value === totalBlocks.value &&
    !undecidable.value
);

function pick(file: string, i: number, choice: BlockPick["choice"]) {
  const p = picks.value[file]?.[i];
  if (p) p.choice = p.choice === choice && choice !== "manual" ? "" : choice;
}

/** AI 融合草案:只产文本,填进可编辑草稿框,人确认(落地)才生效 */
async function fuseBlock(file: string, i: number) {
  const p = picks.value[file]?.[i];
  if (!p || p.fusing) return;
  p.fusing = true;
  try {
    const r = await collabApi.aiFuse(props.task.id, file, i);
    p.text = r.text;
    p.choice = "manual";
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    p.fusing = false;
  }
}

const resolving = ref(false);
/** 落地裁决:全部块处置齐 → 落成任务分支上的合并提交 → 重跑试算(应变干净) */
async function applyResolutions() {
  if (!allDecided.value || resolving.value) return;
  resolving.value = true;
  try {
    const resolutions: Record<string, BlockResolution[]> = {};
    for (const [file, arr] of Object.entries(picks.value)) {
      resolutions[file] = arr.map((p) =>
        p.choice === "manual"
          ? { choice: "manual", text: p.text }
          : { choice: p.choice as "ours" | "theirs" }
      );
    }
    const r = await collabApi.mergeResolve(props.task.id, resolutions);
    toast.info(`裁决已落到分支(${r.commit.slice(0, 8)}),重新试算…`);
    await runTrial();
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    resolving.value = false;
  }
}

async function doSquash() {
  merging.value = true;
  try {
    const r = await collabApi.mergeSquash(props.task.id);
    commit.value = r.commit;
    toast.info(`已 squash 合并进 main(${r.commit.slice(0, 8)})`);
    await collab.refreshTasks();
  } catch (e) {
    // 400 = 三级闸门拒绝理由,原样示人
    toast.error((e as Error).message);
  } finally {
    merging.value = false;
  }
}

const revertBusy = ref(false);
const revertInput = ref("");
async function doRevert() {
  const oid = (commit.value || revertInput.value).trim();
  if (!oid) {
    toast.error("请填写要回滚的合并 commit 哈希");
    return;
  }
  if (!confirm(`以卡回滚:在 main 上生成一笔反做提交,撤销 ${oid.slice(0, 8)} 的改动?`))
    return;
  revertBusy.value = true;
  try {
    const r = await collabApi.mergeRevert(props.task.project_id, oid);
    toast.info(`已回滚(新提交 ${r.commit.slice(0, 8)})`);
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    revertBusy.value = false;
  }
}

function toggleBase(key: string) {
  const s = new Set(openBase.value);
  if (s.has(key)) s.delete(key);
  else s.add(key);
  openBase.value = s;
}

// 换卡时重置并自动试算(review 态才挂本组件)
watch(
  () => props.task.id,
  () => {
    trial.value = null;
    commit.value = "";
    openBase.value = new Set();
    picks.value = {};
    if (props.task.state === "review") void runTrial();
  },
  { immediate: true }
);
</script>

<template>
  <section class="mc">
    <h3 class="mc-title">
      <GitMerge :size="13" :stroke-width="1.9" /> 冲突裁决台
      <button class="mc-refresh" title="重新试算" :disabled="loading" @click="runTrial">
        <RefreshCw :size="13" :class="{ spin: loading }" />
      </button>
    </h3>

    <!-- review 态:试算 + 放行 -->
    <template v-if="task.state === 'review'">
      <div v-if="loading && !trial" class="mc-dim">
        <LoaderCircle :size="13" class="spin" /> 正在试算 main ← {{ task.branch }} …
      </div>
      <div v-else-if="trialErr" class="mc-err">{{ trialErr }}</div>

      <template v-else-if="trial">
        <!-- scope 越界预警 -->
        <div v-if="trial.scopeOverlap.length" class="mc-warn scope">
          <TriangleAlert :size="13" :stroke-width="2" />
          <div>
            <b>范围预警</b>:以下改动越出卡上 scope,合并前请确认
            <ul>
              <li v-for="f in trial.scopeOverlap" :key="f">{{ f }}</li>
            </ul>
          </div>
        </div>

        <!-- 干净 -->
        <div v-if="trial.clean" class="mc-clean">
          <div class="mc-clean-line">
            <Check :size="15" :stroke-width="2.2" /> 可干净合并
            <span class="mc-ba">领先 main {{ trial.ahead }} 提交 · 落后 {{ trial.behind }} 提交</span>
          </div>
          <button
            v-if="collab.canManage"
            class="btn solid"
            :disabled="merging"
            @click="doSquash"
          >
            <LoaderCircle v-if="merging" :size="13" class="spin" />
            <GitMerge v-else :size="14" :stroke-width="1.9" />
            squash 合并进 main
          </button>
          <p v-else class="mc-dim">等待 owner 放行合并(三级闸门:机器闸 → 验收闸 → 放行闸)。</p>
        </div>

        <!-- 冲突:逐块三处置(采纳某侧/融合草案)或整单打回 -->
        <div v-else class="mc-conflict">
          <div class="mc-warn">
            <TriangleAlert :size="13" :stroke-width="2" />
            <b>{{ trial.conflictFiles.length }} 个文件有冲突</b>。逐块处置(采纳某侧或融合草案),全部处置完可一键落地;方向性分歧则整单打回。
          </div>

          <div v-for="file in trial.conflictFiles" :key="file" class="cf">
            <div class="cf-name">{{ file }}</div>
            <div
              v-for="(b, i) in trial.conflictBlocks[file] ?? []"
              :key="i"
              class="blk"
              :class="{ decided: picks[file]?.[i]?.choice }"
            >
              <div class="blk-loc">第 {{ b.start_line }}–{{ b.end_line }} 行</div>
              <div class="blk-cols">
                <div class="col ours" :class="{ picked: picks[file]?.[i]?.choice === 'ours' }">
                  <div class="col-h">main 侧</div>
                  <pre>{{ b.ours || "(空)" }}</pre>
                </div>
                <div class="col theirs" :class="{ picked: picks[file]?.[i]?.choice === 'theirs' }">
                  <div class="col-h">分支侧({{ task.branch }})</div>
                  <pre>{{ b.theirs || "(空)" }}</pre>
                </div>
              </div>
              <!-- 三处置:块级操作只有管理者可见(裁决动作过授权表在服务端) -->
              <div v-if="collab.canManage && picks[file]?.[i]" class="blk-acts">
                <button
                  class="pk"
                  :class="{ on: picks[file][i].choice === 'ours' }"
                  @click="pick(file, i, 'ours')"
                >采纳 main 侧</button>
                <button
                  class="pk"
                  :class="{ on: picks[file][i].choice === 'theirs' }"
                  @click="pick(file, i, 'theirs')"
                >采纳分支侧</button>
                <button
                  class="pk"
                  :class="{ on: picks[file][i].choice === 'manual' }"
                  @click="pick(file, i, 'manual')"
                >融合草案</button>
                <button
                  class="pk ai"
                  :disabled="picks[file][i].fusing"
                  title="让主 Agent 起草融合版本(只产草案,可改,落地须人确认)"
                  @click="fuseBlock(file, i)"
                >
                  <LoaderCircle v-if="picks[file][i].fusing" :size="11" class="spin" />
                  <Sparkles v-else :size="11" :stroke-width="1.9" />
                  AI 起草
                </button>
              </div>
              <textarea
                v-if="picks[file]?.[i]?.choice === 'manual'"
                v-model="picks[file][i].text"
                class="fuse-draft"
                rows="4"
                placeholder="融合后的最终文本(可让 AI 起草后修改)"
              ></textarea>
              <button class="base-toggle" @click="toggleBase(`${file}:${i}`)">
                <ChevronDown :size="12" :class="{ up: openBase.has(`${file}:${i}`) }" />
                共同祖先(base)
              </button>
              <pre v-if="openBase.has(`${file}:${i}`)" class="base-pre">{{ b.base || "(空)" }}</pre>
            </div>
            <div v-if="!(trial.conflictBlocks[file] ?? []).length" class="mc-dim">
              (冲突块解析不可用,该文件无法逐块裁决——请整单打回或到仓库处理)
            </div>
          </div>

          <div class="resolve-bar">
            <button
              v-if="collab.canManage"
              class="btn solid"
              :disabled="!allDecided || resolving"
              :title="undecidable ? '存在无法逐块解析的文件,只能整单打回' : ''"
              @click="applyResolutions"
            >
              <LoaderCircle v-if="resolving" :size="13" class="spin" />
              <Check v-else :size="13" :stroke-width="2" />
              落地裁决({{ decidedBlocks }}/{{ totalBlocks }} 块)
            </button>
            <span v-if="collab.canManage" class="resolve-hint">
              落到分支 {{ task.branch }},不碰 main;之后重跑试算即可放行
            </span>
            <button class="btn warn nomb" @click="emit('reject')">
              <Undo2 :size="13" :stroke-width="1.9" /> 整单打回(带意见)
            </button>
          </div>
        </div>
      </template>
    </template>

    <!-- merged 态:commit 短哈希 + 以卡回滚 -->
    <template v-else-if="task.state === 'merged'">
      <div class="mc-clean-line ok">
        <Check :size="15" :stroke-width="2.2" /> 已合并进 main
        <code v-if="commit" class="oid">{{ commit.slice(0, 8) }}</code>
      </div>
      <div v-if="collab.canManage" class="revert-row">
        <input
          v-if="!commit"
          v-model="revertInput"
          class="oid-inp"
          placeholder="合并 commit 哈希(本会话未记录时填)"
        />
        <button class="btn danger" :disabled="revertBusy" @click="doRevert">
          <LoaderCircle v-if="revertBusy" :size="13" class="spin" />
          <RotateCcw v-else :size="13" :stroke-width="1.9" />
          以卡回滚
        </button>
      </div>
    </template>
  </section>
</template>

<style scoped>
.mc { margin-top: 14px; border: 1px solid var(--border-soft); border-radius: 12px; padding: 12px 14px; background: var(--bg-soft, var(--selection-bg)); }
.mc-title { display: flex; align-items: center; gap: 5px; margin: 0 0 8px; font-size: 12px; font-weight: 600; color: var(--text-2); letter-spacing: 1px; }
.mc-refresh { margin-left: auto; border: none; background: none; color: var(--muted); cursor: pointer; display: inline-flex; padding: 3px; border-radius: 6px; }
.mc-refresh:hover:not(:disabled) { color: var(--ink); background: var(--selection-bg); }
.mc-dim { display: inline-flex; align-items: center; gap: 6px; font-size: 12px; color: var(--dim); font-style: italic; margin: 0; }
.mc-err { font-size: 12px; color: var(--vermilion); }

.mc-warn {
  display: flex; gap: 8px; align-items: flex-start;
  font-size: 12px; color: var(--text); line-height: 1.7;
  border: 1px solid color-mix(in srgb, var(--vermilion) 45%, transparent);
  background: color-mix(in srgb, var(--vermilion) 7%, transparent);
  border-radius: 9px; padding: 8px 11px; margin-bottom: 8px;
}
.mc-warn svg { color: var(--vermilion); flex-shrink: 0; margin-top: 2px; }
.mc-warn ul { margin: 4px 0 0; padding-left: 16px; font-family: var(--mono); font-size: 11px; }
.mc-warn.scope { border-color: color-mix(in srgb, #b8860b 50%, transparent); background: color-mix(in srgb, #b8860b 8%, transparent); }
.mc-warn.scope svg { color: #b8860b; }

.mc-clean-line { display: flex; align-items: center; gap: 7px; font-size: 13px; font-weight: 600; color: #1f9d55; margin-bottom: 10px; }
.mc-clean-line.ok { margin-bottom: 8px; }
.mc-ba { font-size: 11px; font-weight: 400; color: var(--muted); }
.oid { font-family: var(--mono); font-size: 11.5px; color: var(--ink); background: var(--panel); border: 1px solid var(--border-soft); padding: 1px 7px; border-radius: 6px; }

.btn { display: inline-flex; align-items: center; gap: 5px; border: none; cursor: pointer; font-size: 12px; padding: 6px 12px; border-radius: 8px; }
.btn:disabled { opacity: 0.55; cursor: not-allowed; }
.btn.solid { background: var(--btn-solid-bg); color: var(--btn-solid-text); }
.btn.solid:hover:not(:disabled) { background: var(--primary); }
.btn.warn { background: color-mix(in srgb, var(--vermilion) 12%, transparent); color: var(--vermilion); border: 1px solid var(--vermilion); margin-bottom: 10px; }
.btn.danger { background: transparent; color: var(--vermilion); border: 1px solid var(--border); }
.btn.danger:hover:not(:disabled) { border-color: var(--vermilion); }
.revert-row { display: flex; gap: 7px; align-items: center; }
.oid-inp { flex: 1; min-width: 0; border: 1px solid var(--border); border-radius: 8px; background: var(--bg); color: var(--ink); font-size: 11.5px; padding: 6px 9px; font-family: var(--mono); }

.cf { margin-top: 10px; }
.cf-name { font-family: var(--mono); font-size: 11.5px; font-weight: 600; color: var(--ink); margin-bottom: 6px; word-break: break-all; }
.blk { border: 1px solid var(--border-soft); border-radius: 9px; background: var(--panel); padding: 8px; margin-bottom: 8px; }
.blk.decided { border-color: color-mix(in srgb, #1f9d55 45%, transparent); }
.col.picked { outline: 2px solid color-mix(in srgb, #1f9d55 55%, transparent); outline-offset: -1px; }
.blk-acts { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 7px; }
.pk {
  display: inline-flex; align-items: center; gap: 4px;
  border: 1px solid var(--border); background: transparent; cursor: pointer;
  font-size: 11px; color: var(--text-2); padding: 3px 9px; border-radius: 14px;
}
.pk:hover:not(:disabled) { color: var(--ink); border-color: var(--ink); }
.pk.on { color: #1f9d55; border-color: #1f9d55; background: color-mix(in srgb, #1f9d55 9%, transparent); font-weight: 600; }
.pk.ai { color: var(--primary, var(--ink)); }
.pk:disabled { opacity: 0.55; cursor: not-allowed; }
.fuse-draft {
  width: 100%; margin-top: 7px; box-sizing: border-box;
  border: 1px dashed var(--border); border-radius: 8px;
  background: var(--bg); color: var(--ink);
  font-family: var(--mono); font-size: 11px; line-height: 1.6;
  padding: 7px 9px; resize: vertical;
}
.fuse-draft:focus { outline: none; border-color: var(--primary, var(--ink)); }
.resolve-bar { display: flex; align-items: center; flex-wrap: wrap; gap: 10px; margin-top: 12px; }
.resolve-hint { font-size: 10.5px; color: var(--muted); }
.btn.warn.nomb { margin-bottom: 0; margin-left: auto; }
.blk-loc { font-size: 10.5px; color: var(--muted); margin-bottom: 6px; font-family: var(--mono); }
.blk-cols { display: grid; grid-template-columns: 1fr 1fr; gap: 6px; }
.col { min-width: 0; border-radius: 7px; overflow: hidden; border: 1px solid var(--border-soft); }
.col-h { font-size: 10.5px; font-weight: 600; padding: 3px 8px; letter-spacing: 0.5px; }
.col.ours .col-h { background: color-mix(in srgb, #2f6fed 10%, transparent); color: #2f6fed; }
.col.theirs .col-h { background: color-mix(in srgb, #1f9d55 10%, transparent); color: #1f9d55; }
.col pre, .base-pre {
  margin: 0; padding: 7px 9px;
  font-family: var(--mono); font-size: 11px; line-height: 1.6;
  color: var(--text); white-space: pre-wrap; word-break: break-all;
  max-height: 180px; overflow-y: auto;
}
.base-toggle {
  display: inline-flex; align-items: center; gap: 4px;
  margin-top: 6px; border: none; background: none; cursor: pointer;
  font-size: 10.5px; color: var(--muted); padding: 2px 0;
}
.base-toggle:hover { color: var(--ink); }
.base-toggle svg { transition: transform 0.15s; }
.base-toggle svg.up { transform: rotate(180deg); }
.base-pre { border: 1px dashed var(--border-soft); border-radius: 7px; margin-top: 4px; color: var(--muted); }
.spin { animation: spin 0.9s linear infinite; }
@keyframes spin { to { transform: rotate(360deg); } }
</style>
