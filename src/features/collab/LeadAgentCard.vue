<script setup lang="ts">
/**
 * 主 Agent 设置卡(owner 专属):指定项目主 Agent 专家 + 授权表。
 * 铁律:主 Agent 只裁决内容,权限永远由授权表判定 —— UI 显著标注。
 */
import { ref, watch } from "vue";
import { Bot, ChevronDown, LoaderCircle, Save, ShieldAlert } from "@lucide/vue";
import { collabApi, type LeadGrants, type LeadModelCfg } from "./api";
import { useCollabStore } from "./stores/collab";
import { toast } from "../../composables/useToast";

const collab = useCollabStore();
const grants = ref<LeadGrants>({
  can_merge: false,
  can_reassign: false,
  auto_dispatch: false,
  token_budget: 200000,
});
const expertId = ref("");
const loading = ref(false);
const saving = ref(false);

// 主 Agent 模型(全主机一份,owner 专属;api_key 服务端脱敏为 •••,原样传回=保留)
const modelOpen = ref(false);
const model = ref<LeadModelCfg>({ enabled: false, base_url: "", api_key: "", model: "" });
const modelReadable = ref(false);

async function load() {
  const pid = collab.currentProjectId;
  if (!pid) return;
  loading.value = true;
  try {
    grants.value = await collabApi.leadGrants(pid);
  } catch {
    /* 无记录 → 保持默认(全关) */
  } finally {
    loading.value = false;
  }
  expertId.value = collab.currentProject?.lead_expert_id ?? "";
  try {
    model.value = await collabApi.leadModel();
    modelReadable.value = true;
  } catch {
    modelReadable.value = false; // 非全局 owner 看不到模型配置,不展示该区
  }
}

async function save() {
  const pid = collab.currentProjectId;
  if (!pid) return;
  saving.value = true;
  try {
    await collabApi.setLeadGrants(pid, {
      ...grants.value,
      token_budget: Math.max(0, Math.floor(Number(grants.value.token_budget) || 0)),
    });
    // 主 Agent 专家:空 = 纯人工模式
    await collabApi.setLead(pid, expertId.value.trim());
    if (modelReadable.value) {
      await collabApi.setLeadModel({
        ...model.value,
        base_url: model.value.base_url.trim(),
        model: model.value.model.trim(),
      });
    }
    toast.info("主 Agent 设置已保存");
    await collab.refreshProjects();
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    saving.value = false;
  }
}

watch(() => collab.currentProjectId, load, { immediate: true });
</script>

<template>
  <div class="lead-card">
    <div class="lc-head">
      <Bot :size="13" :stroke-width="1.8" /> 主 Agent
      <LoaderCircle v-if="loading" :size="12" class="spin" />
    </div>
    <div class="lc-rule">
      <ShieldAlert :size="12" :stroke-width="2" />
      <span>铁律:主 Agent 只裁决内容,<b>权限永远由授权表判定</b>。</span>
    </div>
    <label class="lc-fld">
      <span>专家 ID(空 = 纯人工)</span>
      <input v-model="expertId" placeholder="如 ai/ai-engineer" />
    </label>
    <label class="lc-row">
      <input v-model="grants.can_merge" type="checkbox" />
      <span>允许放行合并(can_merge)</span>
    </label>
    <label class="lc-row">
      <input v-model="grants.can_reassign" type="checkbox" />
      <span>允许改派任务(can_reassign)</span>
    </label>
    <label class="lc-row">
      <input v-model="grants.auto_dispatch" type="checkbox" />
      <span>允许自动派工(auto_dispatch)</span>
    </label>
    <label class="lc-fld">
      <span>每日 token 预算</span>
      <input v-model.number="grants.token_budget" type="number" min="0" step="10000" />
    </label>

    <!-- 主 Agent 模型(AI 拆卡/验收/融合草案的引擎;不配则纯人工照常) -->
    <template v-if="modelReadable">
      <button class="lc-model-toggle" @click="modelOpen = !modelOpen">
        <ChevronDown :size="12" :class="{ up: modelOpen }" />
        模型配置
        <span class="lc-model-state" :class="{ on: model.enabled }">
          {{ model.enabled ? "已启用" : "未启用(纯人工)" }}
        </span>
      </button>
      <template v-if="modelOpen">
        <label class="lc-row">
          <input v-model="model.enabled" type="checkbox" />
          <span>启用 AI(拆卡/验收/融合草案)</span>
        </label>
        <label class="lc-fld">
          <span>OpenAI 兼容端点</span>
          <input v-model="model.base_url" placeholder="如 https://api.moonshot.cn" />
        </label>
        <label class="lc-fld">
          <span>API Key(••• = 保留现有)</span>
          <input v-model="model.api_key" type="password" placeholder="sk-…" />
        </label>
        <label class="lc-fld">
          <span>模型名</span>
          <input v-model="model.model" placeholder="如 kimi-k2-0711-preview" />
        </label>
      </template>
    </template>

    <button class="lc-save" :disabled="saving || !collab.currentProjectId" @click="save">
      <LoaderCircle v-if="saving" :size="12" class="spin" />
      <Save v-else :size="12" :stroke-width="1.9" />
      保存
    </button>
  </div>
</template>

<style scoped>
.lead-card {
  margin: 8px 10px 0;
  border: 1px solid var(--border-soft); border-radius: 10px;
  background: var(--panel); padding: 10px 11px;
  display: flex; flex-direction: column; gap: 8px;
}
.lc-head { display: flex; align-items: center; gap: 5px; font-size: 11.5px; font-weight: 600; color: var(--text-2); letter-spacing: 1px; }
.lc-rule {
  display: flex; gap: 6px; align-items: flex-start;
  font-size: 10.5px; line-height: 1.6; color: #b8860b;
  border: 1px solid color-mix(in srgb, #b8860b 45%, transparent);
  background: color-mix(in srgb, #b8860b 8%, transparent);
  border-radius: 8px; padding: 6px 8px;
}
.lc-rule svg { flex-shrink: 0; margin-top: 1px; }
.lc-fld { display: flex; flex-direction: column; gap: 3px; }
.lc-fld span { font-size: 10.5px; color: var(--muted); }
.lc-fld input {
  border: 1px solid var(--border); border-radius: 7px;
  background: var(--bg); color: var(--ink);
  font-size: 11.5px; padding: 5px 8px;
}
.lc-fld input:focus { outline: none; border-color: var(--primary, var(--ink)); }
.lc-row { display: flex; align-items: center; gap: 7px; font-size: 11.5px; color: var(--text); cursor: pointer; }
.lc-row input { accent-color: var(--primary, var(--ink)); }
.lc-model-toggle {
  display: flex; align-items: center; gap: 5px;
  border: none; background: none; cursor: pointer; padding: 2px 0;
  font-size: 11px; font-weight: 600; color: var(--text-2); letter-spacing: 0.5px;
}
.lc-model-toggle:hover { color: var(--ink); }
.lc-model-toggle svg { transition: transform 0.15s; }
.lc-model-toggle svg.up { transform: rotate(180deg); }
.lc-model-state { margin-left: auto; font-weight: 400; font-size: 10px; color: var(--muted); }
.lc-model-state.on { color: #1f9d55; }
.lc-save {
  align-self: flex-end;
  display: inline-flex; align-items: center; gap: 5px;
  border: none; cursor: pointer;
  background: var(--btn-solid-bg); color: var(--btn-solid-text);
  font-size: 11.5px; padding: 5px 12px; border-radius: 7px;
}
.lc-save:hover:not(:disabled) { background: var(--primary); }
.lc-save:disabled { opacity: 0.55; cursor: not-allowed; }
.spin { animation: spin 0.9s linear infinite; }
@keyframes spin { to { transform: rotate(360deg); } }
</style>
