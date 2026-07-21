<script setup lang="ts">
/**
 * 浮动气泡工具栏（现代编辑器式）：选中画布里的文字时浮现在选区上方。
 * 从 ArtifactEditor.vue 原样搬移；execCommand 系列作用于父层传入的 iframe 文档。
 */
import {
  Bold, Italic, Underline, Strikethrough, Type, Highlighter,
  AlignLeft, AlignCenter, AlignRight, List, ListOrdered, Link2, RemoveFormatting,
} from "@lucide/vue";

const props = defineProps<{
  /** 画布容器坐标（父层根据选区计算） */
  bubble: { x: number; y: number };
  api: {
    doc: () => Document | null;
    markDirty: () => void;
    refreshSel: () => void;
    pushHistory: (debounceMs?: number) => void;
  };
}>();

// ───────── 富文本工具栏（豆包式，作用于画布里 contenteditable 的当前选区）─────────
// 网页模式整页可编辑，选中即可用；deck 模式双击文本进入文字编辑后可用。
function exec(cmd: string, val?: string) {
  const d = props.api.doc();
  if (!d) return;
  try { d.execCommand("styleWithCSS", false, "true"); } catch { /* 老命令，个别实现会抛 */ }
  d.execCommand(cmd, false, val);
  props.api.markDirty();
  props.api.refreshSel();
  props.api.pushHistory(500);
}
function execBlock(e: Event) {
  const sel = e.target as HTMLSelectElement;
  if (sel.value) exec("formatBlock", `<${sel.value}>`);
  sel.value = ""; // 复位成占位项，下次选同一项也能触发 change
}
function execFontSize(e: Event) {
  const sel = e.target as HTMLSelectElement;
  const px = parseInt(sel.value);
  sel.value = "";
  const d = props.api.doc();
  if (!d || !px) return;
  // execCommand 的 fontSize 只有 1–7 粗档：先打上 7 号标记，再把标记换成精确 px
  try { d.execCommand("styleWithCSS", false, "true"); } catch { /* 同上 */ }
  d.execCommand("fontSize", false, "7");
  d.querySelectorAll('font[size="7"]').forEach((f) => {
    const s = d.createElement("span");
    s.style.fontSize = px + "px";
    while (f.firstChild) s.appendChild(f.firstChild);
    f.replaceWith(s);
  });
  d.querySelectorAll<HTMLElement>('span[style*="font-size"]').forEach((sp) => {
    if (sp.style.fontSize === "xxx-large") sp.style.fontSize = px + "px";
  });
  props.api.markDirty();
  props.api.pushHistory(500);
}
function execLink() {
  const url = window.prompt("链接地址", "https://");
  if (url && url !== "https://") exec("createLink", url);
}
// 点工具条按钮不能抢走 iframe 里的文字选区焦点（否则命令落到空选区上）。
// select 下拉除外——它需要 mousedown 默认行为才能弹开。
function onFmtBarDown(e: MouseEvent) {
  const t = e.target as HTMLElement;
  if (!t.closest("select")) e.preventDefault();
}
function execColor(e: Event) { exec("foreColor", (e.target as HTMLInputElement).value); }
function execHilite(e: Event) { exec("hiliteColor", (e.target as HTMLInputElement).value); }
</script>

<template>
  <div
    class="ed-bubble"
    :style="{ left: bubble.x + 'px', top: bubble.y + 'px' }"
    @mousedown="onFmtBarDown"
  >
    <select class="ed-fmt-sel" title="段落格式" @change="execBlock">
      <option value="" disabled selected>正文</option>
      <option value="p">正文</option>
      <option value="h1">标题 1</option>
      <option value="h2">标题 2</option>
      <option value="h3">标题 3</option>
      <option value="blockquote">引用</option>
    </select>
    <select class="ed-fmt-sel" title="字号" @change="execFontSize">
      <option value="" disabled selected>字号</option>
      <option v-for="s in [12, 14, 16, 18, 20, 24, 28, 32, 40, 48, 64]" :key="s" :value="s">{{ s }}</option>
    </select>
    <span class="ed-fmt-sep" />
    <button class="ed-ic" title="加粗" @click="exec('bold')"><Bold :size="14" /></button>
    <button class="ed-ic" title="斜体" @click="exec('italic')"><Italic :size="14" /></button>
    <button class="ed-ic" title="下划线" @click="exec('underline')"><Underline :size="14" /></button>
    <button class="ed-ic" title="删除线" @click="exec('strikeThrough')"><Strikethrough :size="14" /></button>
    <label class="ed-fmt-color" title="文字颜色">
      <Type :size="13" />
      <input type="color" @input="execColor" />
    </label>
    <label class="ed-fmt-color" title="高亮标记">
      <Highlighter :size="13" />
      <input type="color" value="#fff59d" @input="execHilite" />
    </label>
    <span class="ed-fmt-sep" />
    <button class="ed-ic" title="左对齐" @click="exec('justifyLeft')"><AlignLeft :size="14" /></button>
    <button class="ed-ic" title="居中" @click="exec('justifyCenter')"><AlignCenter :size="14" /></button>
    <button class="ed-ic" title="右对齐" @click="exec('justifyRight')"><AlignRight :size="14" /></button>
    <span class="ed-fmt-sep" />
    <button class="ed-ic" title="无序列表" @click="exec('insertUnorderedList')"><List :size="14" /></button>
    <button class="ed-ic" title="有序列表" @click="exec('insertOrderedList')"><ListOrdered :size="14" /></button>
    <button class="ed-ic" title="插入链接" @click="execLink"><Link2 :size="14" /></button>
    <button class="ed-ic" title="清除格式" @click="exec('removeFormat')"><RemoveFormatting :size="14" /></button>
  </div>
</template>

<style scoped>
/* 浮动气泡工具栏：跟随文字选区，玻璃质感 */
.ed-bubble {
  position: absolute;
  z-index: 40;
  transform: translate(-50%, calc(-100% - 10px));
  display: flex;
  align-items: center;
  gap: 2px;
  padding: 5px 7px;
  border-radius: 11px;
  border: 1px solid var(--hairline, var(--border-soft));
  background: color-mix(in srgb, var(--panel) 86%, transparent);
  backdrop-filter: blur(14px) saturate(1.3);
  -webkit-backdrop-filter: blur(14px) saturate(1.3);
  box-shadow: 0 10px 34px rgba(15, 25, 45, .18), 0 2px 8px rgba(15, 25, 45, .1);
  animation: ed-bubble-in .14s ease;
  white-space: nowrap;
}
@keyframes ed-bubble-in { from { opacity: 0; transform: translate(-50%, calc(-100% - 4px)); } }
.ed-bubble .ed-ic { border: none; background: transparent; }
.ed-bubble .ed-ic:hover:not(:disabled) { background: var(--bg-soft); }
.ed-ic { width: 28px; height: 28px; display: inline-flex; align-items: center; justify-content: center; border: 1px solid var(--border); border-radius: 7px; background: var(--bg); color: var(--text-2); cursor: pointer; }
.ed-ic:hover:not(:disabled) { border-color: var(--primary); color: var(--primary); }
.ed-ic:disabled { opacity: .4; cursor: default; }
.ed-fmt-sep { width: 1px; height: 18px; background: var(--border-soft); margin: 0 5px; flex-shrink: 0; }
.ed-fmt-sel { border: 1px solid var(--border); border-radius: 7px; background: var(--bg); color: var(--text-2); font-size: 12px; padding: 4px 5px; cursor: pointer; max-width: 96px; }
.ed-fmt-sel:hover { border-color: var(--primary); color: var(--primary); }
.ed-fmt-color { position: relative; width: 30px; height: 28px; display: inline-flex; flex-direction: column; align-items: center; justify-content: center; gap: 1px; border-radius: 6px; cursor: pointer; color: var(--text-2); }
.ed-fmt-color:hover { background: var(--bg-soft); }
.ed-fmt-color::after { content: ""; width: 14px; height: 3px; border-radius: 2px; background: currentColor; opacity: .5; }
.ed-fmt-color input { position: absolute; inset: 0; opacity: 0; cursor: pointer; }
</style>
