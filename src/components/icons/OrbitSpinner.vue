<template>
  <span class="orbit" :style="{ '--sz': size + 'px' }" aria-hidden="true">
    <span class="ring"></span>
    <span class="core"></span>
  </span>
</template>

<script setup lang="ts">
// 高级感加载环(替代 lucide LoaderCircle 的匀速细线圈):
// 锥形渐变光弧 + 每圈非线性转速(慢起快收, 游戏里"蓄力→释放"的节奏感) + 中心呼吸核。
// 纯 CSS, 跟随 --primary 主题色; prefers-reduced-motion 下降级为匀速慢转。
withDefaults(defineProps<{ size?: number }>(), { size: 14 });
</script>

<style scoped>
.orbit {
  position: relative;
  display: inline-block;
  width: var(--sz);
  height: var(--sz);
  flex-shrink: 0;
}
.ring {
  position: absolute;
  inset: 0;
  border-radius: 50%;
  background: conic-gradient(
    from 0deg,
    transparent 10%,
    var(--primary) 90%,
    transparent 95%
  );
  -webkit-mask: radial-gradient(
    farthest-side,
    transparent calc(100% - 2.5px),
    #000 calc(100% - 2px)
  );
  mask: radial-gradient(
    farthest-side,
    transparent calc(100% - 2.5px),
    #000 calc(100% - 2px)
  );
  filter: drop-shadow(0 0 3px var(--primary-soft));
  animation: orbit-turn 0.85s cubic-bezier(0.55, 0.12, 0.45, 0.88) infinite;
}
.core {
  position: absolute;
  inset: 35%;
  border-radius: 50%;
  background: var(--primary);
  animation: orbit-pulse 1.7s ease-in-out infinite;
}
@keyframes orbit-turn {
  to {
    transform: rotate(360deg);
  }
}
@keyframes orbit-pulse {
  0%,
  100% {
    transform: scale(0.7);
    opacity: 0.18;
  }
  50% {
    transform: scale(1);
    opacity: 0.45;
  }
}
@media (prefers-reduced-motion: reduce) {
  .ring {
    animation: orbit-turn 1.8s linear infinite;
  }
  .core {
    animation: none;
    opacity: 0.25;
  }
}
</style>
