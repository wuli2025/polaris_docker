<script setup lang="ts">
import { CircleCheck, CircleAlert, Info, X } from "@lucide/vue";
import { useToastQueue, type ToastKind } from "../composables/useToast";

const { items, dismiss } = useToastQueue();

const icons: Record<ToastKind, any> = {
  success: CircleCheck,
  error: CircleAlert,
  info: Info,
};
</script>

<template>
  <div class="toast-host" aria-live="polite">
    <TransitionGroup name="toast">
      <div v-for="t in items" :key="t.id" class="toast" :class="t.kind">
        <component :is="icons[t.kind]" :size="15" :stroke-width="2" class="t-ic" />
        <span class="t-text">{{ t.text }}</span>
        <button class="t-close" @click="dismiss(t.id)">
          <X :size="12" :stroke-width="2.2" />
        </button>
      </div>
    </TransitionGroup>
  </div>
</template>

<style scoped>
.toast-host {
  position: fixed;
  top: 14px;
  left: 50%;
  transform: translateX(-50%);
  z-index: 9999;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  pointer-events: none;
}
.toast {
  pointer-events: auto;
  display: inline-flex;
  align-items: center;
  gap: 8px;
  max-width: min(560px, 80vw);
  padding: 9px 14px;
  border-radius: 10px;
  font-size: 12.5px;
  line-height: 1.5;
  background: var(--panel);
  border: 1px solid var(--border);
  color: var(--text);
  box-shadow: var(--shadow-lg);
}
.toast.success .t-ic {
  color: var(--ok, #3a9d6e);
}
.toast.error {
  border-color: rgba(192, 57, 43, 0.35);
}
.toast.error .t-ic {
  color: var(--vermilion);
}
.toast.info .t-ic {
  color: var(--primary);
}
.t-text {
  word-break: break-word;
}
.t-close {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  height: 18px;
  border: none;
  background: transparent;
  color: var(--muted);
  border-radius: 4px;
  cursor: pointer;
  flex-shrink: 0;
}
.t-close:hover {
  background: var(--bg-soft);
  color: var(--text);
}
.toast-enter-active,
.toast-leave-active {
  transition: opacity 0.18s ease, transform 0.18s ease;
}
.toast-enter-from,
.toast-leave-to {
  opacity: 0;
  transform: translateY(-6px);
}
</style>
