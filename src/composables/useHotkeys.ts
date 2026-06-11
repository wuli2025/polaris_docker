// 全局快捷键(App 根挂一次):
//   Ctrl/Cmd+N 新对话 · Ctrl/Cmd+K 命令面板 · Ctrl/Cmd+B 收/展侧栏 · Esc 关命令面板
import { onMounted, onBeforeUnmount, ref } from "vue";
import { useAppStore } from "../stores/app";

/** 命令面板开关(模块级,CommandPalette 与热键共享) */
export const paletteOpen = ref(false);

export function useHotkeys() {
  const app = useAppStore();

  async function newConversation() {
    let pid = app.currentProjectId;
    if (!pid) {
      await app.refreshProjects();
      pid = app.currentProjectId;
    }
    if (!pid) pid = (await app.createProject("默认项目")).id;
    await app.createConversation(pid);
  }

  function onKeydown(e: KeyboardEvent) {
    const mod = e.ctrlKey || e.metaKey;
    if (mod && !e.shiftKey && !e.altKey) {
      const k = e.key.toLowerCase();
      if (k === "k") {
        e.preventDefault();
        paletteOpen.value = !paletteOpen.value;
        return;
      }
      if (k === "n") {
        e.preventDefault();
        newConversation();
        return;
      }
      if (k === "b") {
        e.preventDefault();
        app.toggleSidebar();
        return;
      }
    }
    if (e.key === "Escape" && paletteOpen.value) {
      e.preventDefault();
      paletteOpen.value = false;
    }
  }

  onMounted(() => window.addEventListener("keydown", onKeydown));
  onBeforeUnmount(() => window.removeEventListener("keydown", onKeydown));
}
