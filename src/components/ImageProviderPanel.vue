<script setup lang="ts">
/**
 * 生图模型配置面板 —— 挂在「API 供应商」坞里的独立分区。
 *
 * 为什么单独一个组件而不是并进 ProviderDock:
 * 后端就是**两张独立的表**(见 provider/image_store.rs 文件头 —— 聊天表的 switch/detect 会把
 * 任何带 base_url 的条目当聊天家、把地址套进 ANTHROPIC_BASE_URL, 生图 endpoint 混进去会把
 * 聊天整条链路搞挂)。前端跟着分开, 两边的状态/方法物理隔离, 不会有人手滑串用。
 */
import { computed, onMounted, reactive, ref } from "vue";
import { Eye, EyeOff, Pencil, Plus, Trash2 } from "@lucide/vue";
import {
  imageProvider,
  isTauri,
  type ImageFlavor,
  type ImagePresetView,
  type ImageProviderListResult,
  type ImageProviderView,
} from "../tauri";

const data = ref<ImageProviderListResult | null>(null);
const loading = ref(false);
const err = ref<string | null>(null);
const ok = ref<string | null>(null);
const editing = ref(false);
const revealKey = ref(false);
const testing = ref(false);

const form = reactive({
  id: "",
  name: "",
  flavor: "minimax" as ImageFlavor,
  endpoint: "",
  model: "",
  apiKey: "",
  note: "",
});

const items = computed<ImageProviderView[]>(() => data.value?.items ?? []);
const presets = computed<ImagePresetView[]>(() => data.value?.presets ?? []);
const currentId = computed(() => data.value?.currentId ?? "");
/** 一家都没配 = 生图能力关闭(与后端 current_image_config() 回 None 同口径) */
const empty = computed(() => items.value.length === 0);

async function refresh() {
  loading.value = true;
  err.value = null;
  try {
    data.value = await imageProvider.list();
  } catch (e) {
    err.value = String(e);
  } finally {
    loading.value = false;
  }
}
onMounted(refresh);

function startNew(p?: ImagePresetView) {
  Object.assign(form, {
    id: "",
    name: p?.name ?? "",
    flavor: p?.flavor ?? "minimax",
    endpoint: p?.endpoint ?? "",
    model: p?.model ?? "",
    apiKey: "",
    note: p?.note ?? "",
  });
  revealKey.value = false;
  editing.value = true;
  err.value = null;
  ok.value = null;
}

function startEdit(it: ImageProviderView) {
  // apiKey 留空 = 后端保持原 key 不变(后端不回明文, 这里也不假装能回显)
  Object.assign(form, { ...it, apiKey: "" });
  revealKey.value = false;
  editing.value = true;
  err.value = null;
  ok.value = null;
}

async function submit() {
  err.value = null;
  ok.value = null;
  try {
    data.value = await imageProvider.save({
      id: form.id || undefined,
      name: form.name,
      flavor: form.flavor,
      endpoint: form.endpoint,
      model: form.model,
      apiKey: form.apiKey || undefined,
      note: form.note,
    });
    editing.value = false;
    ok.value = "已保存。";
  } catch (e) {
    err.value = String(e);
  }
}

async function remove(it: ImageProviderView) {
  if (!confirm(`删除生图供应商「${it.name}」？`)) return;
  err.value = null;
  try {
    data.value = await imageProvider.delete(it.id);
  } catch (e) {
    err.value = String(e);
  }
}

async function use(it: ImageProviderView) {
  err.value = null;
  try {
    data.value = await imageProvider.switch(it.id);
    ok.value = `已切到「${it.name}」。`;
  } catch (e) {
    err.value = String(e);
  }
}

/** 真打一次上游 —— 配错地址/Key/模型名是最常见的坑, 让用户当场知道而不是等生图时才炸 */
async function testCurrent() {
  if (testing.value || !currentId.value) return;
  testing.value = true;
  err.value = null;
  ok.value = null;
  try {
    const out = `${crypto.randomUUID?.() ?? Date.now()}-imgtest.png`;
    const r = await imageProvider.generate("a simple red circle on white background", out, "1:1");
    ok.value = `连通 ✓ ${r.provider} 出图 ${(r.bytes / 1024).toFixed(0)} KB（${r.format}，试了 ${r.attempts} 次）`;
  } catch (e) {
    err.value = `测试失败：${String(e)}`;
  } finally {
    testing.value = false;
  }
}

const flavorHint = computed(() =>
  form.flavor === "minimax"
    ? "MiniMax 形状：画幅走 aspect_ratio，取图 data.image_urls[0]"
    : "OpenAI 形状：画幅走 size，取图 data[0].url 或 data[0].b64_json（豆包方舟等兼容网关同此）"
);
</script>

<template>
  <div class="img-sec">
    <div class="sec-head">
      <span class="t">生图模型</span>
      <span class="sub">文生图独立配置，与上面的聊天供应商互不影响</span>
    </div>

    <div v-if="loading" class="muted">加载中…</div>

    <!-- 空态：直接把预设摆出来，点一下就带出表单（免手输地址/模型名） -->
    <template v-else-if="empty && !editing">
      <div class="empty">
        还没配生图模型。配好后，对话里说「画一张…」就会出真图，而不是 HTML 模拟。
      </div>
      <div class="preset-row">
        <button v-for="p in presets" :key="p.id" class="preset" :title="p.note" @click="startNew(p)">
          <Plus :size="12" :stroke-width="2.2" /> {{ p.name }}
        </button>
        <button class="preset ghost" @click="startNew()">自定义…</button>
      </div>
    </template>

    <!-- 已配列表 -->
    <template v-else-if="!editing">
      <div v-for="it in items" :key="it.id" class="row" :class="{ cur: it.isCurrent }">
        <button
          class="dot"
          :class="{ on: it.isCurrent }"
          :title="it.isCurrent ? '当前使用' : '点击启用'"
          @click="use(it)"
        />
        <div class="main">
          <div class="l1">
            <span class="nm">{{ it.name }}</span>
            <span class="badge">{{ it.flavor === "minimax" ? "MiniMax 形状" : "OpenAI 形状" }}</span>
            <span v-if="!it.hasKey" class="badge warn">未填 Key</span>
          </div>
          <div class="l2">{{ it.model }} · {{ it.endpoint }}</div>
        </div>
        <div class="acts">
          <button class="ico" title="编辑" @click="startEdit(it)"><Pencil :size="12" /></button>
          <button class="ico" title="删除" @click="remove(it)"><Trash2 :size="12" /></button>
        </div>
      </div>
      <div class="preset-row">
        <button v-for="p in presets" :key="p.id" class="preset" :title="p.note" @click="startNew(p)">
          <Plus :size="12" :stroke-width="2.2" /> {{ p.name }}
        </button>
        <button class="preset ghost" @click="startNew()">自定义…</button>
        <button
          v-if="currentId && isTauri"
          class="preset test"
          :disabled="testing"
          title="真打一次上游，确认地址/Key/模型名都对"
          @click="testCurrent"
        >
          {{ testing ? "测试中…（生图慢，20–60s）" : "测试连通" }}
        </button>
      </div>
    </template>

    <!-- 表单 -->
    <div v-else class="form">
      <label class="f">
        <span>名称</span>
        <input v-model="form.name" placeholder="如：MiniMax 图像" />
      </label>
      <label class="f">
        <span>接口形状</span>
        <select v-model="form.flavor">
          <option value="minimax">MiniMax</option>
          <option value="openai">OpenAI 兼容（含豆包方舟）</option>
        </select>
      </label>
      <div class="hint">{{ flavorHint }}</div>
      <label class="f">
        <span>请求地址</span>
        <input v-model="form.endpoint" placeholder="https://…/v1/image_generation" />
      </label>
      <label class="f">
        <span>模型名</span>
        <input v-model="form.model" list="img-model-presets" placeholder="如：image-01" />
        <datalist id="img-model-presets">
          <option v-for="p in presets" :key="p.id" :value="p.model">{{ p.name }}</option>
        </datalist>
      </label>
      <label class="f">
        <span>API Key</span>
        <span class="keywrap">
          <input
            v-model="form.apiKey"
            :type="revealKey ? 'text' : 'password'"
            :placeholder="form.id ? '留空 = 不修改已存的 Key' : '粘贴 Key'"
          />
          <button class="ico" type="button" @click="revealKey = !revealKey">
            <component :is="revealKey ? EyeOff : Eye" :size="12" />
          </button>
        </span>
      </label>
      <div v-if="err" class="msg err">{{ err }}</div>
      <div class="form-acts">
        <button class="btn" @click="editing = false">取消</button>
        <button class="btn primary" @click="submit">保存</button>
      </div>
    </div>

    <div v-if="err && !editing" class="msg err">{{ err }}</div>
    <div v-if="ok" class="msg ok">{{ ok }}</div>
  </div>
</template>

<style scoped>
.img-sec { padding: 10px 12px 12px; border-top: 1px solid var(--border-soft); }
.sec-head { display: flex; align-items: baseline; gap: 8px; margin-bottom: 8px; }
.sec-head .t { font-size: 12.5px; color: var(--ink); font-weight: 600; }
.sec-head .sub { font-size: 10.5px; color: var(--dim); }
.muted { font-size: 11.5px; color: var(--muted); padding: 6px 0; }
.empty { font-size: 11.5px; color: var(--muted); line-height: 1.7; margin-bottom: 8px; }

.row { display: flex; align-items: center; gap: 9px; padding: 7px 6px; border-radius: 4px; }
.row.cur { background: var(--selection-bg); }
.dot { width: 9px; height: 9px; border-radius: 50%; border: 1px solid var(--border); background: transparent; cursor: pointer; flex-shrink: 0; padding: 0; }
.dot.on { background: #4a8f6d; border-color: #4a8f6d; box-shadow: 0 0 0 3px rgba(74,143,109,.15); }
.main { flex: 1; min-width: 0; }
.l1 { display: flex; align-items: center; gap: 6px; }
.nm { font-size: 12.5px; color: var(--ink); }
.badge { font-size: 9.5px; padding: 1px 5px; border-radius: 2px; background: var(--bg-soft); color: var(--dim); border: 1px solid var(--border-soft); }
.badge.warn { color: #c08a3e; border-color: rgba(192,138,62,.4); }
.l2 { font-size: 10.5px; color: var(--dim); font-family: var(--mono); margin-top: 2px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.acts { display: flex; gap: 3px; flex-shrink: 0; }
.ico { background: transparent; border: none; color: var(--muted); cursor: pointer; padding: 3px; border-radius: 3px; display: inline-flex; }
.ico:hover { color: var(--ink); background: var(--bg-soft); }

.preset-row { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 8px; }
.preset { display: inline-flex; align-items: center; gap: 4px; font-size: 11px; padding: 4px 9px; border-radius: 3px; border: 1px solid var(--border); background: transparent; color: var(--text-2); cursor: pointer; }
.preset:hover:not(:disabled) { border-color: var(--ink); color: var(--ink); }
.preset.ghost { border-style: dashed; }
.preset.test { margin-left: auto; }
.preset:disabled { opacity: .5; cursor: not-allowed; }

.form { display: flex; flex-direction: column; gap: 8px; padding-top: 4px; }
.f { display: flex; flex-direction: column; gap: 3px; }
.f > span:first-child { font-size: 10.5px; color: var(--muted); }
.f input, .f select { width: 100%; padding: 5px 8px; font-size: 12px; border: 1px solid var(--border); border-radius: 3px; background: var(--panel); color: var(--ink); }
.keywrap { display: flex; gap: 4px; align-items: center; }
.hint { font-size: 10px; color: var(--dim); line-height: 1.6; }
.form-acts { display: flex; justify-content: flex-end; gap: 8px; margin-top: 4px; }
.btn { padding: 5px 13px; font-size: 12px; border-radius: 3px; border: 1px solid var(--border); background: transparent; color: var(--text-2); cursor: pointer; }
.btn.primary { background: var(--btn-solid-bg); color: var(--btn-solid-text); border-color: var(--btn-solid-bg); }

.msg { margin-top: 8px; padding: 6px 9px; border-radius: 3px; font-size: 11.5px; line-height: 1.6; white-space: pre-wrap; }
.msg.err { background: var(--vermilion-soft); color: var(--vermilion); border-left: 2px solid var(--vermilion); }
.msg.ok { background: var(--primary-soft); color: var(--primary-deep); border-left: 2px solid var(--primary); }
</style>
