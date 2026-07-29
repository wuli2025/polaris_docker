/**
 * 隔空同屏的本机触发口。
 *
 * BeamStage 挂在 App.vue 根部（全局覆盖层），而发起入口散在各处（互联页、文件中心…）。
 * 与其到处传组件 ref，不如留一个模块级信号：谁想发起就 `requestBeamOpen(path)`，
 * BeamStage 监听到就打包、本地打开并广播给手机。
 *
 * 带 `n`（递增序号）是因为「连续投同一个文件」也要能触发 —— 只比 path 的话第二次
 * 值没变，watch 不响应。
 */
import { ref } from "vue";

export const beamOpenRequest = ref<{ path: string; n: number } | null>(null);

let seq = 0;
export function requestBeamOpen(path: string): void {
  beamOpenRequest.value = { path, n: ++seq };
}
