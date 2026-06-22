<script setup lang="ts">
// ── 故障舱壁(Bulkhead / Error Boundary)──
// 参照 VS Code 扩展隔离 + GitHub Turbo Frame:某个功能视图在「渲染 / 生命周期 / 同步 watcher」
// 阶段抛出未捕获异常时,onErrorCaptured 把它拦在这里,只让「当前出错的这一个视图」换成可重试
// 卡片,绝不让异常冒泡到 app 根导致整窗白屏 —— 侧栏、任务中心、右抽屉(都是 <main> 的兄弟节点)
// 与其它功能键照常可用。这正是「要卡也只卡一个键、其余完好」的结构保证。
//
// 注:onErrorCaptured 只能捕获组件生命周期内的同步错误,捕获不到事件回调里的 Promise rejection
// (那类由 main.ts 的全局 unhandledrejection 兜底)。两者一前端一后端式分工,合起来无死角。
import { ref, watch, onErrorCaptured } from "vue";

// 当前挂载的视图键。父层每次切视图都会传入新值 → 用它判定「错误属于哪个视图」并在切走时自愈。
const props = defineProps<{ viewKey?: string | number }>();

const err = ref<Error | null>(null);
const erroredView = ref<string | number | undefined>(undefined);

onErrorCaptured((e, _instance, info) => {
  // 记录出错视图与信息;返回 false 阻止继续向上冒泡(否则会触达 app 级 errorHandler / 白屏)。
  err.value = e instanceof Error ? e : new Error(String(e));
  erroredView.value = props.viewKey;
  // 控制台留痕,方便排查;不 rethrow。
  console.error(`[FaultBoundary] 视图「${props.viewKey}」渲染出错 (${info}):`, e);
  return false;
});

// 切到别的视图就自愈:清掉错误态,让新视图正常渲染;之后再切回老视图会重新尝试挂载(等同重试)。
watch(
  () => props.viewKey,
  (next) => {
    if (err.value && next !== erroredView.value) {
      err.value = null;
      erroredView.value = undefined;
    }
  }
);

function retry() {
  // 重试:清错 → 默认插槽重新渲染。若组件立刻又抛,会被再次捕获,不会失控。
  err.value = null;
  erroredView.value = undefined;
}
</script>

<template>
  <slot v-if="!err" />
  <div v-else class="fb">
    <div class="fb-card">
      <div class="fb-icon">⚠️</div>
      <h3 class="fb-title">这个功能暂时出错了</h3>
      <p class="fb-sub">其它功能不受影响 —— 左侧任意功能键都能照常使用。可点「重试」重新加载本功能。</p>
      <details class="fb-detail">
        <summary>错误详情</summary>
        <pre>{{ err.message }}</pre>
      </details>
      <button class="fb-btn" @click="retry">重试</button>
    </div>
  </div>
</template>

<style scoped>
.fb {
  flex: 1;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 32px;
}
.fb-card {
  max-width: 440px;
  width: 100%;
  text-align: center;
  padding: 34px 30px;
  border-radius: 18px;
  background: var(--bg-elev, rgba(255, 255, 255, 0.04));
  border: 1px solid var(--hairline, rgba(255, 255, 255, 0.1));
  box-shadow: var(--shadow, 0 10px 40px rgba(0, 0, 0, 0.25));
  backdrop-filter: saturate(150%) blur(18px);
}
.fb-icon {
  font-size: 38px;
  line-height: 1;
  margin-bottom: 14px;
}
.fb-title {
  margin: 0 0 8px;
  font-size: 18px;
  font-weight: 700;
  color: var(--text, #e7ecf3);
}
.fb-sub {
  margin: 0 0 18px;
  font-size: 13.5px;
  line-height: 1.7;
  color: var(--muted, #9aa7ba);
}
.fb-detail {
  text-align: left;
  margin: 0 0 18px;
  font-size: 12.5px;
  color: var(--muted, #9aa7ba);
}
.fb-detail summary {
  cursor: pointer;
  user-select: none;
}
.fb-detail pre {
  margin: 10px 0 0;
  padding: 12px;
  border-radius: 10px;
  background: rgba(0, 0, 0, 0.25);
  border: 1px solid var(--hairline, rgba(255, 255, 255, 0.08));
  white-space: pre-wrap;
  word-break: break-word;
  max-height: 180px;
  overflow: auto;
  color: #ffb4b4;
}
.fb-btn {
  display: inline-block;
  padding: 9px 26px;
  border-radius: 10px;
  border: 1px solid var(--primary, #6db5ff);
  background: var(--primary-soft, rgba(109, 181, 255, 0.12));
  color: var(--primary, #6db5ff);
  font-size: 14px;
  font-weight: 600;
  cursor: pointer;
  transition: background 0.15s ease;
}
.fb-btn:hover {
  background: var(--primary, #6db5ff);
  color: #fff;
}
</style>
