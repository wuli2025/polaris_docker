import { defineStore } from "pinia";
import { ref } from "vue";

/**
 * 「让 AI 更懂你」引导向导的全局开关。
 *
 * 向导本体常驻挂在 App.vue(不随视图切换销毁),靠这个 store 控制显隐 —— 于是扫描/归类
 * 跑着时用户可以「转入后台」隐掉浮层、最小化窗口、去逛别的视图,后台线程与事件监听照常推进,
 * 再点「智能向导」回来还停在原来那一步(状态不丢)。
 */
export const useWizardStore = defineStore("wizard", () => {
  const open = ref(false);
  function openWizard() {
    open.value = true;
  }
  function closeWizard() {
    open.value = false;
  }
  return { open, openWizard, closeWizard };
});
