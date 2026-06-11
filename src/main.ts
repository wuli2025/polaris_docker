import { createApp } from "vue";
import { createPinia } from "pinia";
import App from "./App.vue";
import "./style.css";
import { toast } from "./composables/useToast";
import { humanizeError } from "./lib/humanizeError";

const app = createApp(App);

// 全局兜底:任何未捕获异常不再白屏裸奔,可见 toast + 控制台留痕
app.config.errorHandler = (err, _instance, info) => {
  console.error("[vue error]", err, info);
  toast.error(humanizeError(err));
};
window.addEventListener("unhandledrejection", (e) => {
  console.error("[unhandled rejection]", e.reason);
  e.preventDefault();
  toast.error(humanizeError(e.reason));
});
window.addEventListener("error", (e) => {
  // 资源加载错误等没有 error 对象,只记日志不打扰用户
  if (e.error) {
    console.error("[window error]", e.error);
    toast.error(humanizeError(e.error));
  }
});

app.use(createPinia());
app.mount("#app");
