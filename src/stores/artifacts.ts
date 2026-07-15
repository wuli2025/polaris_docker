import { defineStore } from "pinia";
import { ref } from "vue";
import {
  artifacts as api,
  isTauri,
  backendFileUrl,
  type ArtifactPayload,
} from "../tauri";

/** 网页版：触发浏览器下载（后端 download=1 已带 Content-Disposition: attachment）。 */
function triggerDownload(url: string) {
  const a = document.createElement("a");
  a.href = url;
  a.rel = "noopener";
  document.body.appendChild(a);
  a.click();
  a.remove();
}

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
  // ── PPT 伴生导出：编辑的是 deck.html，保存后可一键重导出覆盖同源 .pptx ──
  const companionPptx = ref<string | null>(null);
  const exporting = ref(false);
  const exportError = ref<string | null>(null);

  async function open(path: string) {
    const name = path.split(/[\\/]/).pop() || path;
    current.value = { path, name };
    loading.value = true;
    error.value = null;
    payload.value = null;
    companionPptx.value = null; // 换文件即斩断旧伴生关系，防把别的 html 导出去覆盖 pptx
    exportError.value = null;
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
    companionPptx.value = null;
    exportError.value = null;
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

  /**
   * 「编辑 PPT」入口：.pptx 是逐页截图死图改不了，真正可编辑的是它的网页版源稿
   * deck.html —— 打开 html 进编辑器，并记住伴生 .pptx，保存后可一键重导出覆盖。
   */
  async function enterEditDeck(htmlPath: string, pptxPath: string): Promise<boolean> {
    await open(htmlPath); // open 会清 companionPptx，故先 open 再认亲
    if (error.value || !payload.value?.text) return false;
    enterEdit();
    companionPptx.value = pptxPath;
    return true;
  }

  /** 把当前编辑的 deck.html 重新导出覆盖伴生 .pptx（自研 forge 管线，可能要几十秒） */
  async function exportPptx(): Promise<boolean> {
    const deck = current.value?.path;
    const out = companionPptx.value;
    if (!deck || !out || exporting.value) return false;
    exporting.value = true;
    exportError.value = null;
    try {
      await api.deckToPptx(deck, out);
      return true;
    } catch (e: any) {
      exportError.value = e?.message ?? String(e);
      return false;
    } finally {
      exporting.value = false;
    }
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

  /** 桌面版：用系统默认程序打开；网页版：在新标签页打开后端文件 URL（HTML 直接渲染，pptx 等走下载）。 */
  async function openExternal() {
    if (!current.value) return;
    if (isTauri) {
      try {
        await api.openExternal(current.value.path);
      } catch (_) {
        /* 忽略：打开失败不影响预览 */
      }
    } else {
      window.open(backendFileUrl(current.value.path), "_blank", "noopener");
    }
  }

  /**
   * 桌面版：在系统文件管理器中定位并选中该文件。
   * 网页版：「文件夹」在浏览器里无对应概念 —— 改为下载该文件（用户点这个键的真实意图就是「拿到文件」）。
   */
  async function revealInFolder() {
    if (!current.value) return;
    if (isTauri) {
      try {
        await api.reveal(current.value.path);
      } catch (_) {
        /* 忽略：打开失败不影响预览 */
      }
    } else {
      triggerDownload(backendFileUrl(current.value.path, { download: true }));
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
    companionPptx,
    exporting,
    exportError,
    open,
    refresh,
    close,
    toggleExpand,
    enterEdit,
    enterEditDeck,
    exportPptx,
    exitEdit,
    markDirty,
    saveContent,
    openFolder,
    openExternal,
    revealInFolder,
  };
});
