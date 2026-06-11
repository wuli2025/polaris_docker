<script setup lang="ts">
// 自绘放大镜:lucide Search 的镜片占比过大(r=8/24),缩到 13px 手柄只剩 2px
// 小尺寸下读成"圆圈"。这里镜片缩小、手柄拉长,小图标也能一眼认出是搜索。
// props 对齐 lucide 组件(:size/:stroke-width),可原位替换。
withDefaults(defineProps<{ size?: number | string; strokeWidth?: number | string }>(), {
  size: 24,
  strokeWidth: 2,
});
</script>

<template>
  <svg
    :width="size"
    :height="size"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    :stroke-width="strokeWidth"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
  >
    <circle cx="10" cy="10" r="6.5" />
    <path d="m20.5 20.5-5.9-5.9" />
  </svg>
</template>
