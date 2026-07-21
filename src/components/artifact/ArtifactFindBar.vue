<script setup lang="ts">
/**
 * 查找替换条（源码 / Markdown / 纯文本，Ctrl+F）。从 ArtifactEditor.vue 原样搬移。
 * 组件常驻挂载、用 visible 控制显隐（等价原 v-if 外加：查找词跨开合保留，与原父层状态一致）。
 * 作用对象（textarea 与工作副本 html）通过 api 存取，不直接持有。
 */
import { ref, computed, nextTick } from "vue";
import { Search, X } from "@lucide/vue";

const props = defineProps<{
  visible: boolean;
  api: {
    /** 当前作用的 textarea（文档模式=左侧编辑区；否则=源码区） */
    getTa: () => HTMLTextAreaElement | null;
    getHtml: () => string;
    setHtml: (v: string) => void;
    markDirty: () => void;
  };
}>();
const emit = defineEmits<{ (e: "close"): void }>();

const findQ = ref("");
const replQ = ref("");
const findInput = ref<HTMLInputElement | null>(null);
const findCount = computed(() =>
  findQ.value ? props.api.getHtml().split(findQ.value).length - 1 : 0
);

/** 打开时聚焦查找框（父层在置 visible 后调用） */
function focusInput() {
  nextTick(() => findInput.value?.focus());
}
function findNext() {
  const ta = props.api.getTa();
  if (!ta || !findQ.value) return;
  const v = ta.value;
  let i = v.indexOf(findQ.value, ta.selectionEnd || 0);
  if (i < 0) i = v.indexOf(findQ.value); // 到底回绕
  if (i < 0) return;
  ta.focus();
  ta.setSelectionRange(i, i + findQ.value.length);
}
function replaceOne() {
  const ta = props.api.getTa();
  if (!ta || !findQ.value) return;
  const s = ta.selectionStart, e2 = ta.selectionEnd;
  if (ta.value.slice(s, e2) === findQ.value) {
    props.api.setHtml(ta.value.slice(0, s) + replQ.value + ta.value.slice(e2));
    props.api.markDirty();
    nextTick(() => {
      ta.setSelectionRange(s + replQ.value.length, s + replQ.value.length);
      findNext();
    });
  } else {
    findNext(); // 先定位到第一处，再点一次替换
  }
}
function replaceAll() {
  if (!findQ.value || !findCount.value) return;
  props.api.setHtml(props.api.getHtml().split(findQ.value).join(replQ.value));
  props.api.markDirty();
}

defineExpose({ focusInput });
</script>

<template>
  <div v-show="visible" class="ed-find">
    <Search :size="13" class="ed-find-ic" />
    <input
      ref="findInput"
      v-model="findQ"
      class="ed-find-in"
      placeholder="查找…"
      @keydown.enter.prevent="findNext"
      @keydown.esc="emit('close')"
    />
    <span class="ed-find-n">{{ findCount }} 处</span>
    <input
      v-model="replQ"
      class="ed-find-in"
      placeholder="替换为…"
      @keydown.enter.prevent="replaceOne"
      @keydown.esc="emit('close')"
    />
    <button class="ed-find-btn" :disabled="!findCount" @click="findNext">下一个</button>
    <button class="ed-find-btn" :disabled="!findCount" @click="replaceOne">替换</button>
    <button class="ed-find-btn" :disabled="!findCount" @click="replaceAll">全部替换</button>
    <button class="ed-ic" title="关闭" @click="emit('close')"><X :size="13" /></button>
  </div>
</template>

<style scoped>
/* 查找替换条 */
.ed-find { display: flex; align-items: center; gap: 6px; padding: 5px 12px; border-bottom: 1px solid var(--border-soft); background: var(--bg-soft); }
.ed-find-ic { color: var(--muted); flex-shrink: 0; }
.ed-find-in { width: 170px; padding: 4px 8px; border: 1px solid var(--border); border-radius: 7px; background: var(--bg); color: var(--text); font-size: 12.5px; outline: none; }
.ed-find-in:focus { border-color: var(--primary); }
.ed-find-n { font-size: 11.5px; color: var(--muted); min-width: 34px; font-variant-numeric: tabular-nums; }
.ed-find-btn { padding: 4px 10px; border: 1px solid var(--border); border-radius: 7px; background: var(--bg); color: var(--text-2); font-size: 12px; cursor: pointer; }
.ed-find-btn:hover:not(:disabled) { border-color: var(--primary); color: var(--primary); }
.ed-find-btn:disabled { opacity: .4; cursor: default; }
.ed-ic { width: 28px; height: 28px; display: inline-flex; align-items: center; justify-content: center; border: 1px solid var(--border); border-radius: 7px; background: var(--bg); color: var(--text-2); cursor: pointer; }
.ed-ic:hover:not(:disabled) { border-color: var(--primary); color: var(--primary); }
</style>
