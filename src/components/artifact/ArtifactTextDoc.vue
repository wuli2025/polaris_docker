<script setup lang="ts">
/**
 * 文档类编辑（Markdown / 纯文本）：不走 iframe 画布，走「源码 + 实时预览」。
 * 从 ArtifactEditor.vue 原样搬移；marked 随本组件异步加载（父层 defineAsyncComponent）。
 * 工作副本 html 走 v-model；Markdown 工具条按钮在父层，通过 defineExpose 调这里的 md* 函数。
 */
import { ref, computed, nextTick } from "vue";
import { marked } from "marked";
import { sanitizeHtml } from "../../lib/sanitize";
import { useArtifactsStore } from "../../stores/artifacts";

const props = defineProps<{
  modelValue: string;
  docKind: string;
}>();
const emit = defineEmits<{ (e: "update:modelValue", v: string): void }>();

const artifacts = useArtifactsStore();

// Markdown 实时预览（与 RightDrawer 预览同一条 marked+sanitize 管线）
const mdPreview = computed(() =>
  props.docKind === "markdown" ? sanitizeHtml(marked.parse(props.modelValue) as string) : ""
);

// 文档编辑的 textarea 与预览引用
const docTa = ref<HTMLTextAreaElement | null>(null);
const mdPrevEl = ref<HTMLElement | null>(null);

function setHtml(v: string) {
  emit("update:modelValue", v);
}
function onInput(e: Event) {
  setHtml((e.target as HTMLTextAreaElement).value);
  artifacts.markDirty(true);
}

// ───────── Markdown 工具栏（作用于左侧 textarea 的选区/光标）─────────
function mdWrap(before: string, after = before, placeholder = "文字") {
  const ta = docTa.value;
  if (!ta) return;
  const s = ta.selectionStart, e2 = ta.selectionEnd, v = ta.value;
  const sel = v.slice(s, e2) || placeholder;
  setHtml(v.slice(0, s) + before + sel + after + v.slice(e2));
  artifacts.markDirty(true);
  nextTick(() => {
    ta.focus();
    ta.setSelectionRange(s + before.length, s + before.length + sel.length);
  });
}
/** 行首前缀开关（标题/引用/列表）：选中多行则逐行处理 */
function mdPrefix(prefix: string) {
  const ta = docTa.value;
  if (!ta) return;
  const s = ta.selectionStart, e2 = ta.selectionEnd, v = ta.value;
  const ls = v.lastIndexOf("\n", s - 1) + 1; // 扩到行首
  const seg = v.slice(ls, e2);
  const done = seg
    .split("\n")
    .map((l) => (l.startsWith(prefix) ? l.slice(prefix.length) : prefix + l))
    .join("\n");
  setHtml(v.slice(0, ls) + done + v.slice(e2));
  artifacts.markDirty(true);
  nextTick(() => {
    ta.focus();
    ta.setSelectionRange(ls, ls + done.length);
  });
}
function mdInsertBlock(text: string) {
  const ta = docTa.value;
  if (!ta) return;
  const s = ta.selectionStart, v = ta.value;
  setHtml(v.slice(0, s) + text + v.slice(s));
  artifacts.markDirty(true);
  nextTick(() => {
    ta.focus();
    ta.setSelectionRange(s + text.length, s + text.length);
  });
}
function mdInsertTable() {
  mdInsertBlock("\n| 列 1 | 列 2 | 列 3 |\n| --- | --- | --- |\n| 内容 | 内容 | 内容 |\n");
}
/** Tab 缩进两空格（否则 Tab 会把焦点带走） */
function onDocTaKey(e: KeyboardEvent) {
  if (e.key !== "Tab") return;
  e.preventDefault();
  const ta = e.target as HTMLTextAreaElement;
  const s = ta.selectionStart, e2 = ta.selectionEnd;
  setHtml(ta.value.slice(0, s) + "  " + ta.value.slice(e2));
  artifacts.markDirty(true);
  nextTick(() => ta.setSelectionRange(s + 2, s + 2));
}
/** 左编辑右预览按比例同步滚动 */
function onDocTaScroll() {
  const ta = docTa.value, pv = mdPrevEl.value;
  if (!ta || !pv) return;
  const ratio = ta.scrollTop / Math.max(1, ta.scrollHeight - ta.clientHeight);
  pv.scrollTop = ratio * (pv.scrollHeight - pv.clientHeight);
}

defineExpose({
  /** 查找替换要拿的左侧 textarea */
  ta: () => docTa.value,
  mdWrap,
  mdPrefix,
  mdInsertTable,
});
</script>

<template>
  <div class="ed-split">
    <textarea
      ref="docTa"
      :value="modelValue"
      class="ed-code-area doc"
      spellcheck="false"
      :placeholder="docKind === 'markdown' ? '# 在这里写 Markdown，右侧实时预览' : ''"
      @input="onInput"
      @keydown="onDocTaKey"
      @scroll="onDocTaScroll"
    />
    <div
      v-if="docKind === 'markdown'"
      ref="mdPrevEl"
      class="ed-md-preview markdown"
      v-html="mdPreview"
    />
  </div>
</template>

<style scoped>
/* 文档编辑（Markdown / 纯文本）：左源码右预览 */
.ed-split { flex: 1; min-width: 0; display: flex; }
.ed-code-area { flex: 1; resize: none; border: none; padding: 16px 18px; background: #0f1115; color: #d6deeb; font-family: var(--mono); font-size: 12.5px; line-height: 1.6; tab-size: 2; outline: none; white-space: pre; overflow: auto; }
.ed-split .ed-code-area.doc { min-width: 0; white-space: pre-wrap; }
.ed-md-preview { flex: 1; min-width: 0; overflow: auto; padding: 28px 32px; border-left: 1px solid var(--border-soft); background: var(--panel); color: var(--text); font-size: 14px; line-height: 1.75; }
</style>
