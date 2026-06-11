// 共享轮询:替代各工坊手写的 setInterval —— 统一带上
//   ① 页面不可见自动停、可见恢复并立即拉一次(后台不再白烧 invoke)
//   ② 组件卸载自动清理
//   ③ start/stop 幂等
import { onUnmounted } from "vue";

export interface PollingHandle {
  start: () => void;
  stop: () => void;
  /** 当前是否在跑(含「页面隐藏暂停但逻辑上开启」) */
  readonly active: boolean;
}

export function usePolling(fn: () => void, intervalMs: number): PollingHandle {
  let timer: ReturnType<typeof setInterval> | null = null;
  let wanted = false; // 逻辑开关(页面隐藏时 timer 停但 wanted 仍真)

  function spin() {
    if (timer || document.hidden) return;
    timer = setInterval(fn, intervalMs);
  }
  function halt() {
    if (timer) {
      clearInterval(timer);
      timer = null;
    }
  }
  function onVisibility() {
    if (!wanted) return;
    if (document.hidden) {
      halt();
    } else {
      fn(); // 回前台立刻补一次,别等下个周期
      spin();
    }
  }

  document.addEventListener("visibilitychange", onVisibility);

  const handle: PollingHandle = {
    start() {
      if (wanted) return;
      wanted = true;
      fn();
      spin();
    },
    stop() {
      wanted = false;
      halt();
    },
    get active() {
      return wanted;
    },
  };

  onUnmounted(() => {
    handle.stop();
    document.removeEventListener("visibilitychange", onVisibility);
  });

  return handle;
}
