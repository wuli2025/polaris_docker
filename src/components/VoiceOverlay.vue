<script setup lang="ts">
// 语音输入浮窗:听后端 voice:listening / voice:partial / voice:final 事件,
// 说话时画声纹 + 流式上字,松手短暂显示终稿后淡出。
// PRD v3 §5「流式上字」。注:作 app 内置浮层,Polaris 获焦时可见;
// 跨应用打字的实时预览需独立置顶窗(后续 polish),终稿注入由后端 enigo 完成。
import { onMounted, onUnmounted, ref } from "vue";
import { listen } from "../tauri";

defineOptions({ name: "VoiceOverlay" });

const visible = ref(false);
const listening = ref(false);
const text = ref("");
const note = ref("");
let hideTimer: number | undefined;
const unlisteners: Array<() => void> = [];

onMounted(async () => {
  unlisteners.push(
    await listen<boolean>("voice:listening", (on) => {
      clearTimeout(hideTimer);
      listening.value = on;
      if (on) {
        visible.value = true;
        text.value = "";
        note.value = "";
      } else {
        // 听写结束(无 voice:final)时也淡出
        hideTimer = window.setTimeout(() => {
          visible.value = false;
        }, 1200);
      }
    })
  );
  unlisteners.push(
    await listen<{ text?: string }>("voice:partial", (p) => {
      if (p && typeof p.text === "string") text.value = p.text;
    })
  );
  unlisteners.push(
    await listen<{ text?: string; error?: string; cancelled?: boolean }>("voice:final", (f) => {
      listening.value = false;
      if (f?.error) {
        note.value = "✗ " + f.error;
      } else if (f?.cancelled) {
        note.value = "（太短，已取消）";
      } else {
        text.value = f?.text ?? text.value;
        note.value = "✓ 已上屏";
      }
      // 终稿短暂展示后淡出
      clearTimeout(hideTimer);
      hideTimer = window.setTimeout(() => {
        visible.value = false;
      }, 1400);
    })
  );
});

onUnmounted(() => {
  for (const u of unlisteners) u();
  clearTimeout(hideTimer);
});
</script>

<template>
  <Transition name="vov">
    <div v-if="visible" class="vov" :class="{ live: listening }">
      <div v-if="listening" class="wave">
        <i v-for="n in 6" :key="n" :style="{ animationDelay: n * 0.08 + 's' }"></i>
      </div>
      <span v-else class="dot">●</span>
      <span class="txt">
        {{ text || (listening ? "在听…" : "") }}
        <span v-if="listening" class="cur"></span>
      </span>
      <span v-if="note" class="note">{{ note }}</span>
    </div>
  </Transition>
</template>

<style scoped>
.vov {
  position: fixed;
  left: 50%;
  bottom: 36px;
  transform: translateX(-50%);
  z-index: 9999;
  display: flex;
  align-items: center;
  gap: 12px;
  max-width: min(70vw, 720px);
  padding: 10px 20px;
  background: rgba(31, 31, 31, 0.96);
  border: 1px solid var(--gold, #d4b06a);
  border-radius: 30px;
  box-shadow: 0 10px 36px rgba(0, 0, 0, 0.5);
  backdrop-filter: blur(8px);
}
.wave {
  display: flex;
  align-items: center;
  gap: 3px;
  height: 20px;
}
.wave i {
  width: 3px;
  height: 8px;
  background: var(--gold, #d4b06a);
  border-radius: 2px;
  animation: vov-w 0.9s ease-in-out infinite;
}
@keyframes vov-w {
  0%,
  100% {
    transform: scaleY(0.5);
  }
  50% {
    transform: scaleY(2.4);
  }
}
.dot {
  color: #5fae7e;
  font-size: 12px;
}
.txt {
  font-size: 14px;
  color: #e8e8e6;
  line-height: 1.4;
  word-break: break-word;
}
.cur {
  display: inline-block;
  width: 7px;
  height: 15px;
  background: var(--gold, #d4b06a);
  margin-left: 2px;
  vertical-align: middle;
  animation: vov-blink 1s step-end infinite;
}
@keyframes vov-blink {
  50% {
    opacity: 0;
  }
}
.note {
  font-size: 12px;
  color: #8a8a85;
  white-space: nowrap;
}
.vov-enter-active,
.vov-leave-active {
  transition: opacity 0.25s, transform 0.25s;
}
.vov-enter-from,
.vov-leave-to {
  opacity: 0;
  transform: translateX(-50%) translateY(10px);
}
</style>
