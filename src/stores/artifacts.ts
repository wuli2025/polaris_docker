import { defineStore } from "pinia";
import { ref } from "vue";
import { artifacts as api, type ArtifactPayload } from "../tauri";

/**
 * 右侧抽屉的「成品预览」状态。
 * - current: 当前正在预览的文件（path + 文件名）
 * - payload: 后端读回的内容（html/图片/文本…）
 * - expanded: 抽屉是否放大（让观看更好看）
 * ChatPanel 点击文件 chip → open(path)；RightDrawer 据此渲染预览。
 */
export const useArtifactsStore = defineStore("artifacts", () => {
  const current = ref<{ path: string; name: string } | null>(null);
  const payload = ref<ArtifactPayload | null>(null);
  const loading = ref(false);
  const error = ref<string | null>(null);
  const expanded = ref(false);
  // ── 编辑器（豆包式）──
  const editing = ref(false);
  const saving = ref(false);
  const dirty = ref(false);
  const saveError = ref<string | null>(null);
  const savedAt = ref(0); // 最近保存时间戳(ms)，用于「已保存」提示

  async function open(path: string) {
    const name = path.split("/").pop() || path;
    current.value = { path, name };
    loading.value = true;
    error.value = null;
    payload.value = null;
    try {
      payload.value = await api.read(path);
    } catch (e: any) {
      error.value = e?.message ?? String(e);
    } finally {
      loading.value = false;
    }
  }

  async function refresh() {
    if (current.value) await open(current.value.path);
  }

  function close() {
    current.value = null;
    payload.value = null;
    error.value = null;
    expanded.value = false;
    editing.value = false;
    dirty.value = false;
    saveError.value = null;
  }

  function toggleExpand() {
    expanded.value = !expanded.value;
  }

  /** 进入编辑器（自动放大到大尺寸，仿豆包） */
  function enterEdit() {
    editing.value = true;
    expanded.value = true;
    saveError.value = null;
  }
  /** 退出编辑器（回到只读预览，仍保持放大状态由调用方决定） */
  function exitEdit() {
    editing.value = false;
    dirty.value = false;
    saveError.value = null;
  }
  function markDirty(v = true) {
    dirty.value = v;
  }

  /** 把编辑后的完整文本写回当前产物文件 */
  async function saveContent(text: string): Promise<boolean> {
    const target = current.value;
    if (!target) return false;
    const path = target.path; // 固定写入目标, 防 await 期间用户切换/关闭后写错文件
    saving.value = true;
    saveError.value = null;
    try {
      await api.write(path, text);
      // await 期间可能已 close() 或 open() 了别的产物 —— 若已不是同一个目标,
      // 别再回写它的 payload/dirty/savedAt(否则会给新产物盖上旧文本的状态)。
      if (current.value === target) {
        if (payload.value) payload.value = { ...payload.value, text };
        dirty.value = false;
        savedAt.value = Date.now();
      }
      return true;
    } catch (e: any) {
      if (current.value === target) saveError.value = e?.message ?? String(e);
      return false;
    } finally {
      if (current.value === target) saving.value = false;
    }
  }

  /** 「应用文件夹」chip：不进预览，直接在系统文件管理器打开该文件夹 */
  async function openFolder(path: string) {
    try {
      await api.openExternal(path.replace(/\/+$/, ""));
    } catch (_) {
      /* 忽略：打开失败不影响对话 */
    }
  }

  async function openExternal() {
    if (current.value) {
      try {
        await api.openExternal(current.value.path);
      } catch (_) {
        /* 忽略：打开失败不影响预览 */
      }
    }
  }

  /** 在系统文件管理器中定位并选中当前预览的文件 */
  async function revealInFolder() {
    if (current.value) {
      try {
        await api.reveal(current.value.path);
      } catch (_) {
        /* 忽略：打开失败不影响预览 */
      }
    }
  }

  return {
    current,
    payload,
    loading,
    error,
    expanded,
    editing,
    saving,
    dirty,
    saveError,
    savedAt,
    open,
    refresh,
    close,
    toggleExpand,
    enterEdit,
    exitEdit,
    markDirty,
    saveContent,
    openFolder,
    openExternal,
    revealInFolder,
  };
});
