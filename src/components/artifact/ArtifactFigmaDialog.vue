<script setup lang="ts">
/**
 * Figma 往返对话框: 去程复制 HTML 走 html.to.design 插件, 回程 REST 拉回替换画布。
 * 从 ArtifactEditor.vue 原样搬移；随父层 defineAsyncComponent 懒加载（figmaPull 转换器一并按需加载）。
 */
import { ref } from "vue";
import { Loader, X } from "@lucide/vue";
import { figmaApi, openUrl } from "../../tauri";
import { collectVectorIds, figmaFrameToHtml } from "../../lib/figmaPull";
import { useArtifactsStore } from "../../stores/artifacts";

const props = defineProps<{
  api: {
    /** 当前要送去 Figma 的完整 HTML（可视化模式=序列化画布，否则=源码） */
    getOutHtml: () => string;
    /** 拉回结果替换画布工作副本（可 Ctrl+Z 回退, Ctrl+S 才真正落盘） */
    applyHtml: (html: string) => void;
  };
}>();
const emit = defineEmits<{ (e: "close"): void }>();

const artifacts = useArtifactsStore();

const figmaLink = ref<string>(localStorage.getItem("polaris.figma.link") ?? "");
// PAT 只留内存:localStorage 会被同源 artifact 脚本(iframe allow-scripts+allow-same-origin)读走外传
const figmaToken = ref<string>("");
const figmaBusy = ref(false);
const figmaErr = ref("");
const figmaCopied = ref(false);
async function figmaCopyHtml() {
  figmaErr.value = "";
  const out = props.api.getOutHtml();
  try {
    await navigator.clipboard.writeText(out);
    figmaCopied.value = true;
    setTimeout(() => (figmaCopied.value = false), 2000);
  } catch {
    figmaErr.value = "复制失败：切到「源码」模式手动全选复制也一样";
  }
}
async function figmaPullBack() {
  figmaErr.value = "";
  const link = figmaLink.value.trim();
  const token = figmaToken.value.trim();
  if (!link) { figmaErr.value = "先粘贴 Figma 文件链接"; return; }
  if (!token) { figmaErr.value = "需要 Figma 访问令牌：Figma 头像 → Settings → Security → Personal access tokens"; return; }
  figmaBusy.value = true;
  try {
    localStorage.setItem("polaris.figma.link", link);
    // 不落盘 token(见上:同源 artifact 可读 localStorage)
    const data = await figmaApi.pull(link, token);
    const ids = collectVectorIds(data.doc);
    const svgs = ids.length ? await figmaApi.exportSvgs(link, ids, token) : {};
    const res = figmaFrameToHtml(data, svgs);
    // 替换画布工作副本（可 Ctrl+Z 回退到 Figma 化之前, Ctrl+S 才真正落盘）
    props.api.applyHtml(res.html);
    artifacts.markDirty(true);
    emit("close");
  } catch (e: any) {
    figmaErr.value = e?.message ?? String(e);
  } finally {
    figmaBusy.value = false;
  }
}
</script>

<template>
  <div class="ed-figma-mask" @click.self="emit('close')">
    <div class="ed-figma-dlg">
      <div class="fg-head">
        <span>接入 Figma 网页端</span>
        <button class="ep-x" title="关闭" @click="emit('close')"><X :size="14" /></button>
      </div>

      <div class="fg-step">
        <div class="fg-t">① 送去 Figma 编辑</div>
        <p class="fg-p">
          点「复制页面 HTML」→ 到 Figma 里运行社区插件 <b>html.to.design</b>（免费，网页端可用）→
          选 <b>Paste HTML</b> 粘贴导入，页面就变成可自由编辑的 Figma 图层。
        </p>
        <div class="fg-row">
          <button class="fg-btn primary" @click="figmaCopyHtml">{{ figmaCopied ? "已复制 ✓" : "复制页面 HTML" }}</button>
          <button class="fg-btn" @click="openUrl('https://www.figma.com/community/plugin/1159123024924461424')">获取 html.to.design</button>
          <button class="fg-btn" @click="openUrl('https://www.figma.com')">打开 Figma</button>
        </div>
      </div>

      <div class="fg-step">
        <div class="fg-t">② 改完拉回来</div>
        <p class="fg-p">
          粘贴 Figma 文件链接 + 访问令牌（Figma 头像 → Settings → Security → <b>Personal access tokens</b>，
          只读权限即可，令牌只存在本机）。拉回会把文件里<b>面积最大的画框</b>转成页面替换当前画布——
          视觉级还原，可 <b>Ctrl+Z</b> 撤销，<b>Ctrl+S</b> 才落盘。
        </p>
        <input v-model="figmaLink" class="fg-in" placeholder="https://www.figma.com/design/…" spellcheck="false" />
        <input v-model="figmaToken" class="fg-in" type="password" placeholder="figd_… 访问令牌（自己的 Figma 账号生成，只存本机）" spellcheck="false" />
        <div class="fg-row">
          <button class="fg-btn primary" :disabled="figmaBusy" @click="figmaPullBack">
            <Loader v-if="figmaBusy" :size="13" class="spin" />
            {{ figmaBusy ? "拉取并转换中…（图片多会久一点）" : "拉回并替换画布" }}
          </button>
          <button class="fg-btn" title="打开 Figma 设置页 → Security → Personal access tokens → Generate new token" @click="openUrl('https://www.figma.com/settings')">去生成令牌</button>
        </div>
        <div v-if="figmaErr" class="fg-err">{{ figmaErr }}</div>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* Figma 往返对话框 */
.ed-figma-mask { position: absolute; inset: 0; z-index: 70; display: flex; align-items: center; justify-content: center; background: rgba(15, 20, 30, .38); backdrop-filter: blur(3px); }
.ed-figma-dlg { width: 520px; max-width: calc(100% - 48px); max-height: calc(100% - 48px); overflow-y: auto; border-radius: 16px; border: 1px solid var(--hairline, var(--border-soft)); background: var(--panel); box-shadow: 0 24px 80px rgba(10, 18, 35, .35); animation: ed-fg-in .12s ease; }
@keyframes ed-fg-in { from { opacity: 0; transform: scale(.97); } }
.fg-head { position: sticky; top: 0; display: flex; align-items: center; justify-content: space-between; padding: 14px 18px; border-bottom: 1px solid var(--border-soft); background: var(--panel); font-size: 14px; font-weight: 700; color: var(--text); }
.ep-x { width: 24px; height: 24px; display: inline-flex; align-items: center; justify-content: center; border: none; background: transparent; color: var(--muted); border-radius: 6px; cursor: pointer; }
.ep-x:hover { background: var(--bg-soft); color: var(--text); }
.fg-step { padding: 14px 18px; border-bottom: 1px solid var(--border-soft); }
.fg-step:last-child { border-bottom: none; }
.fg-t { font-size: 13px; font-weight: 700; color: var(--text); margin-bottom: 6px; }
.fg-p { font-size: 12.5px; color: var(--text-2); line-height: 1.7; margin: 0 0 10px; }
.fg-p b { color: var(--text); }
.fg-row { display: flex; gap: 8px; flex-wrap: wrap; }
.fg-btn { display: inline-flex; align-items: center; gap: 6px; padding: 7px 14px; border: 1px solid var(--border); border-radius: 8px; background: var(--bg); color: var(--text-2); font-size: 12.5px; cursor: pointer; }
.fg-btn:hover:not(:disabled) { border-color: var(--primary); color: var(--primary); }
.fg-btn.primary { background: var(--primary); border-color: var(--primary); color: #fff; font-weight: 600; }
.fg-btn.primary:hover:not(:disabled) { filter: brightness(1.07); color: #fff; }
.fg-btn:disabled { opacity: .55; cursor: default; }
.fg-in { width: 100%; margin-bottom: 8px; padding: 8px 10px; border: 1px solid var(--border); border-radius: 8px; background: var(--bg); color: var(--text); font-size: 12.5px; outline: none; }
.fg-in:focus { border-color: var(--primary); }
.fg-err { margin-top: 8px; padding: 8px 10px; border-radius: 8px; background: var(--vermilion-soft, rgba(220, 80, 50, .1)); color: var(--vermilion); font-size: 12px; line-height: 1.6; }
.spin { animation: ed-spin .9s linear infinite; }
@keyframes ed-spin { to { transform: rotate(360deg); } }
</style>
