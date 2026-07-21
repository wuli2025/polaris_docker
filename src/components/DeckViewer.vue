<script setup lang="ts">
// 豆包式演示播放器:左缩略图栏 + 大舞台 + 翻页。DeckStudio 与 RightDrawer 共用。
//
// 为什么是组件而不是 srcdoc iframe:Tauri 主文档 CSP 会被 srcdoc 继承,内联脚本
// 一律被拦,iframe 里的播放器 runtime 根本不执行(排查过:壳在、页全空)。组件由
// Vue 管翻页状态,页面 HTML 走 v-html —— slidesSpec 渲染的标记全部经 esc() 转义,
// 不含任何脚本,安全由构造保证。附带红利:轮询更新 spec 时只是 pages 数组变化,
// 不再有 iframe 整体重载,页码天然保持。
import { computed, ref, watch, onBeforeUnmount } from "vue";
import { ChevronLeft, ChevronRight, Loader, PencilLine, Check } from "@lucide/vue";
import { specSlidesRender, setSpecText, getSpecText, type SlideSpec } from "../lib/slidesSpec";

const props = defineProps<{
  /** 已把图片换成 dataURL 的 spec 对象(resolveSpecImages 之后)。 */
  spec: SlideSpec | null;
  /** 生成中:缩略图尾部脉动占位 + 无人翻页时自动跟随最新页。 */
  generating?: boolean;
  /** 允许点字直改(生成中不给)。 */
  editable?: boolean;
}>();
// 用户改完一处文字 → 抛给父组件写盘(父持有 specPath)。
const emit = defineEmits<{ (e: "edit", slide: number, path: string, value: string): void }>();

const page = ref(0);
const userNav = ref(false);

const rendered = computed(() => (props.spec ? specSlidesRender(props.spec) : null));
const pages = computed(() => rendered.value?.pages ?? []);

// 生成中逐页点亮:用户没翻过页就跟随最新页;页数缩水(重生成)时钳回界内
watch(
  () => pages.value.length,
  (n) => {
    if (!n) return;
    if (props.generating && !userNav.value) page.value = n - 1;
    else if (page.value > n - 1) page.value = n - 1;
  },
  { immediate: true }
);
// 生成结束且用户全程没翻页 → 回封面,从头看成品
watch(
  () => props.generating,
  (now, was) => {
    if (was && !now && !userNav.value) page.value = 0;
  }
);

function go(i: number, user = false) {
  const n = pages.value.length;
  if (!n) return;
  page.value = Math.max(0, Math.min(i, n - 1));
  if (user) userNav.value = true;
}
function onKey(e: KeyboardEvent) {
  if ([" ", "ArrowRight", "ArrowDown", "PageDown"].includes(e.key)) {
    e.preventDefault();
    go(page.value + 1, true);
  } else if (["ArrowLeft", "ArrowUp", "PageUp"].includes(e.key)) {
    e.preventDefault();
    go(page.value - 1, true);
  } else if (e.key === "Home") {
    e.preventDefault();
    go(0, true);
  } else if (e.key === "End") {
    e.preventDefault();
    go(pages.value.length - 1, true);
  }
}

// ───────── 点字直改 ─────────
// 只改文字,不动版式 —— autofit 仍然生效,所以用户**改不坏排版**(这正是"版式态"的红利:
// 豆包没有重排引擎、改字就溢出,我们改完自动重算字号)。
// 舞台里带 data-e="<字段路径>" 的元素点一下变 contenteditable,失焦/Enter 落盘。
const editing = ref(false); // 编辑模式开关(工具条按钮)
const dirty = ref(false); // 有未落盘的改动(纯提示)
const stageEl = ref<HTMLElement | null>(null);

function onStageClick(e: MouseEvent) {
  if (!editing.value) {
    go(page.value + 1, true); // 非编辑态:点击翻页(原行为)
    return;
  }
  const el = (e.target as HTMLElement)?.closest?.("[data-e]") as HTMLElement | null;
  if (!el || el.isContentEditable) return;
  e.stopPropagation();
  beginEdit(el);
}

function beginEdit(el: HTMLElement) {
  el.contentEditable = "true";
  el.spellcheck = false;
  el.focus();
  // 光标落到点击处即可(不全选),改错别字更顺手
  const path = el.dataset.e!;
  const before = props.spec ? getSpecText((props.spec as any).slides?.[page.value], path) : "";
  const finish = () => {
    el.contentEditable = "false";
    el.removeEventListener("blur", finish);
    el.removeEventListener("keydown", onKeyEdit);
    // innerText 保留换行(compare.body / timeline.body 是整块多行文本)
    const now = (el.innerText ?? "").replace(/ /g, " ").replace(/\n{2,}/g, "\n").trim();
    if (now === before) return; // 没改就不写盘
    emit("edit", page.value, path, now);
    dirty.value = true;
  };
  const onKeyEdit = (ev: KeyboardEvent) => {
    ev.stopPropagation(); // 别让 ←→ 冒泡去翻页
    if (ev.key === "Escape") {
      el.innerText = before; // 撤销本次
      el.blur();
    } else if (ev.key === "Enter" && !ev.shiftKey) {
      ev.preventDefault();
      el.blur(); // Enter 落盘;Shift+Enter 才换行
    }
  };
  el.addEventListener("blur", finish, { once: true });
  el.addEventListener("keydown", onKeyEdit);
}

// 退出编辑模式前先把正在编辑的那处收尾(否则改了字却没落盘)
function toggleEdit() {
  if (editing.value) {
    (stageEl.value?.querySelector("[contenteditable='true']") as HTMLElement | null)?.blur();
  }
  editing.value = !editing.value;
}
// 生成中不许编辑(spec 每 3s 被重写,改了也会被覆盖)
watch(
  () => props.generating,
  (g) => {
    if (g) editing.value = false;
  }
);

// 选中缩略图滚进视野
const railEl = ref<HTMLElement | null>(null);
watch(page, () => {
  const th = railEl.value?.children[page.value] as HTMLElement | undefined;
  th?.scrollIntoView?.({ block: "nearest" });
});

// ── 色板 CSS:注入 document.head,所有选择器加实例级 scope 前缀,卸载时移除 ──
// slideBaseCss 输出 .sl/.pts/.card… 这类通用选择器,直接进全局必然污染 App 样式。
const SCOPE = `dkv-${Math.random().toString(36).slice(2, 8)}`;
function scopeCss(css: string, scope: string): string {
  return css
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/(^|\})([^@{}]+)\{/g, (_m, close: string, sel: string) =>
      `${close}${sel
        .split(",")
        .map((s) => `${scope} ${s.trim()}`)
        .filter((s) => s.trim())
        .join(",")}{`
    );
}
let styleEl: HTMLStyleElement | null = null;
watch(
  () => rendered.value?.css,
  (css) => {
    if (!css) return;
    if (!styleEl) {
      styleEl = document.createElement("style");
      styleEl.dataset.dkv = SCOPE;
      document.head.appendChild(styleEl);
    }
    styleEl.textContent = scopeCss(css, `.${SCOPE}`);
  },
  { immediate: true }
);
onBeforeUnmount(() => {
  styleEl?.remove();
  styleEl = null;
});
</script>

<template>
  <div class="dkv" :class="SCOPE" tabindex="0" @keydown="onKey">
    <aside ref="railEl" class="dkv-rail">
      <button
        v-for="(h, i) in pages"
        :key="i"
        class="dkv-th"
        :class="{ on: i === page }"
        :title="`第 ${i + 1} 页`"
        @click="go(i, true)"
      >
        <span class="dkv-n">{{ i + 1 }}</span>
        <div class="dkv-fit" v-html="h"></div>
      </button>
      <div v-if="generating" class="dkv-pending">下一页生成中…</div>
    </aside>
    <main class="dkv-main">
      <div
        ref="stageEl"
        class="dkv-stage"
        :class="{ editing }"
        :title="editing ? '点任意文字即可修改 · Enter 保存 · Esc 撤销' : '点击翻下一页 · ←→ 翻页'"
        @click="onStageClick"
      >
        <div class="dkv-fit stage" v-html="pages[page] ?? ''"></div>
      </div>
      <div class="dkv-bar">
        <button class="dkv-btn" :disabled="page <= 0" @click.stop="go(page - 1, true)">
          <ChevronLeft :size="14" /> 上一页
        </button>
        <span class="dkv-num">{{ pages.length ? page + 1 : 0 }} / {{ pages.length }}</span>
        <button class="dkv-btn" :disabled="page >= pages.length - 1" @click.stop="go(page + 1, true)">
          下一页 <ChevronRight :size="14" />
        </button>
        <button
          v-if="editable && !generating"
          class="dkv-btn edit"
          :class="{ on: editing }"
          :title="editing ? '完成编辑' : '改字：点任意文字直接改，排版自动重算'"
          @click.stop="toggleEdit"
        >
          <component :is="editing ? Check : PencilLine" :size="13" />
          {{ editing ? "完成" : "改字" }}
        </button>
        <span v-if="editing" class="dkv-tip">点文字直接改 · Enter 保存 · Esc 撤销</span>
        <span v-else-if="dirty" class="dkv-tip ok">已保存改动</span>
        <span v-if="generating" class="dkv-gen"><Loader :size="12" class="dkv-spin" /> 生成中…</span>
      </div>
    </main>
  </div>
</template>

<style scoped>
.dkv { display: flex; height: 100%; min-height: 0; background: #26262b; border-radius: 10px; overflow: hidden; outline: none; }
.dkv-rail { width: 150px; flex-shrink: 0; overflow-y: auto; padding: 12px 10px; display: flex; flex-direction: column; gap: 10px; background: rgba(0, 0, 0, 0.22); }
.dkv-rail::-webkit-scrollbar { width: 6px; }
.dkv-rail::-webkit-scrollbar-thumb { background: rgba(255, 255, 255, 0.18); border-radius: 3px; }
.dkv-th { position: relative; cursor: pointer; border-radius: 8px; outline: 2px solid transparent; outline-offset: 1px; border: none; background: transparent; padding: 0; transition: outline-color 0.15s; flex-shrink: 0; text-align: left; }
.dkv-th:hover { outline-color: rgba(255, 255, 255, 0.35); }
.dkv-th.on { outline-color: var(--primary, #7fa8d4); }
.dkv-n { position: absolute; left: 5px; top: 5px; z-index: 2; font-size: 10px; line-height: 1; padding: 3px 6px; border-radius: 4px; background: rgba(0, 0, 0, 0.55); color: #fff; font-weight: 600; }
.dkv-pending { aspect-ratio: 16/9; border-radius: 6px; border: 1.5px dashed rgba(255, 255, 255, 0.32); display: flex; align-items: center; justify-content: center; color: rgba(255, 255, 255, 0.6); font-size: 11px; animation: dkv-pulse 1.25s ease-in-out infinite; flex-shrink: 0; }
@keyframes dkv-pulse { 50% { opacity: 0.4; } }
.dkv-main { flex: 1; display: flex; flex-direction: column; min-width: 0; }
/* container-type:size → 100cqh 可用容器高算出 16:9 下的最大宽,宽高双约束下不溢出 */
.dkv-stage { flex: 1; min-height: 0; cursor: pointer; container-type: size; display: grid; place-items: center; padding: 16px; }
.dkv-stage .dkv-fit { width: min(100%, calc((100cqh - 32px) * 1.77778)); }
/* 抽屉窄时也绝不换行:按钮一律 nowrap + 不收缩,提示语可省略号截断 */
.dkv-bar { height: 44px; flex-shrink: 0; display: flex; align-items: center; justify-content: center; gap: 10px; padding: 0 10px; color: #c9c9cf; font-size: 12.5px; user-select: none; overflow: hidden; }
.dkv-btn { display: inline-flex; align-items: center; gap: 3px; border: 1px solid rgba(255, 255, 255, 0.22); background: rgba(255, 255, 255, 0.06); color: #e4e4e8; border-radius: 7px; padding: 5px 12px; font-size: 12px; cursor: pointer; white-space: nowrap; flex-shrink: 0; }
.dkv-btn:hover:not(:disabled) { background: rgba(255, 255, 255, 0.14); }
.dkv-btn:disabled { opacity: 0.35; cursor: default; }
.dkv-num { min-width: 52px; text-align: center; white-space: nowrap; flex-shrink: 0; }
.dkv-gen { display: inline-flex; align-items: center; gap: 5px; color: var(--primary, #7fa8d4); font-weight: 600; white-space: nowrap; flex-shrink: 0; }
.dkv-btn.edit { display: inline-flex; align-items: center; gap: 4px; }
.dkv-btn.edit.on { background: var(--primary, #7fa8d4); border-color: var(--primary, #7fa8d4); color: #fff; }
/* 提示语是唯一可牺牲的:空间不够就截断,不许把按钮挤换行 */
.dkv-tip { font-size: 11.5px; color: #9a9aa2; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; min-width: 0; }
.dkv-tip.ok { color: #6cbf8f; font-weight: 600; }
/* 编辑态:可改的文字给虚线底提示,hover 高亮 —— 让「哪里能点」一眼可见 */
.dkv-stage.editing { cursor: default; }
.dkv-stage.editing .dkv-fit :deep([data-e]) {
  outline: 1px dashed rgba(127, 168, 212, 0.55);
  outline-offset: 2px;
  cursor: text;
  border-radius: 2px;
}
.dkv-stage.editing .dkv-fit :deep([data-e]:hover) { outline-color: var(--primary, #7fa8d4); background: rgba(127, 168, 212, 0.1); }
.dkv-stage.editing .dkv-fit :deep([contenteditable="true"]) {
  outline: 2px solid var(--primary, #7fa8d4);
  background: rgba(127, 168, 212, 0.14);
}
.dkv-spin { animation: dkv-rot 0.9s linear infinite; }
@keyframes dkv-rot { to { transform: rotate(360deg); } }
/* v-html 内容里的 .sl 尺寸约束(须 :deep 穿透) */
.dkv-fit { width: 100%; }
.dkv-fit :deep(.sl) { width: 100%; }
.dkv-th .dkv-fit :deep(.sl) { border-radius: 6px; pointer-events: none; }
.dkv-stage .dkv-fit :deep(.sl) { box-shadow: 0 12px 36px rgba(0, 0, 0, 0.45); }
</style>
