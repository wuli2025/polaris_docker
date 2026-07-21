<script setup lang="ts">
/**
 * 右键上下文菜单（Figma 式）。从 ArtifactEditor.vue 原样搬移；
 * 定位（ctx 坐标）由父层计算，动作走 api 回调，点击后先关菜单再执行（等价原 ctxDo）。
 */
const props = defineProps<{
  ctx: { x: number; y: number };
  hasSel: boolean;
  multi: boolean;
  canUngroup: boolean;
  canPaste: boolean;
  api: {
    copyEl: () => void;
    pasteEl: () => void;
    duplicateEl: () => void;
    cutEl: () => void;
    zStep: (upward: boolean) => void;
    groupSel: () => void;
    ungroupSel: () => void;
    lockSel: () => void;
    hideSel: () => void;
    fmtDelete: () => void;
    selectAllEls: () => void;
  };
}>();

const emit = defineEmits<{ (e: "close"): void }>();

function ctxDo(fn: () => void) {
  emit("close");
  fn();
}
</script>

<template>
  <div class="ed-ctx" :style="{ left: ctx.x + 'px', top: ctx.y + 'px' }" @pointerdown.stop>
    <template v-if="hasSel">
      <button @click="ctxDo(api.copyEl)"><span>复制</span><kbd>Ctrl C</kbd></button>
      <button :disabled="!canPaste" @click="ctxDo(api.pasteEl)"><span>粘贴</span><kbd>Ctrl V</kbd></button>
      <button @click="ctxDo(api.duplicateEl)"><span>创建副本</span><kbd>Ctrl D</kbd></button>
      <button @click="ctxDo(api.cutEl)"><span>剪切</span><kbd>Ctrl X</kbd></button>
      <div class="sep" />
      <button @click="ctxDo(() => api.zStep(true))"><span>上移一层</span><kbd>]</kbd></button>
      <button @click="ctxDo(() => api.zStep(false))"><span>下移一层</span><kbd>[</kbd></button>
      <div v-if="multi || canUngroup" class="sep" />
      <button v-if="multi" @click="ctxDo(api.groupSel)"><span>编组</span><kbd>Ctrl G</kbd></button>
      <button v-if="canUngroup" @click="ctxDo(api.ungroupSel)"><span>取消编组</span><kbd>Ctrl⇧G</kbd></button>
      <div class="sep" />
      <button @click="ctxDo(api.lockSel)"><span>锁定</span></button>
      <button @click="ctxDo(api.hideSel)"><span>隐藏</span></button>
      <div class="sep" />
      <button class="danger" @click="ctxDo(api.fmtDelete)"><span>删除</span><kbd>Del</kbd></button>
    </template>
    <template v-else>
      <button :disabled="!canPaste" @click="ctxDo(api.pasteEl)"><span>粘贴</span><kbd>Ctrl V</kbd></button>
      <button @click="ctxDo(api.selectAllEls)"><span>全选</span><kbd>Ctrl A</kbd></button>
    </template>
  </div>
</template>

<style scoped>
/* 右键上下文菜单（Figma 式, 玻璃质感） */
.ed-ctx { position: absolute; z-index: 60; min-width: 188px; padding: 5px;
  border-radius: 12px; border: 1px solid var(--hairline, var(--border-soft));
  background: color-mix(in srgb, var(--panel) 94%, transparent);
  backdrop-filter: blur(18px) saturate(1.4); -webkit-backdrop-filter: blur(18px) saturate(1.4);
  box-shadow: 0 14px 44px rgba(15, 25, 45, .22), 0 3px 10px rgba(15, 25, 45, .12);
  animation: ed-ctx-in .1s ease; }
@keyframes ed-ctx-in { from { opacity: 0; transform: scale(.97); } }
.ed-ctx button { width: 100%; display: flex; align-items: center; justify-content: space-between; gap: 18px;
  padding: 6px 10px; border: none; background: transparent; color: var(--text); font-size: 12.5px;
  border-radius: 7px; cursor: pointer; text-align: left; }
.ed-ctx button:hover:not(:disabled) { background: var(--primary); color: #fff; }
.ed-ctx button:hover:not(:disabled) kbd { color: rgba(255, 255, 255, .75); }
.ed-ctx button:disabled { opacity: .4; cursor: default; }
.ed-ctx button.danger:hover { background: var(--vermilion); }
.ed-ctx kbd { font-size: 10.5px; color: var(--dim); font-family: inherit; }
.ed-ctx .sep { height: 1px; margin: 4px 8px; background: var(--border-soft); }
</style>
