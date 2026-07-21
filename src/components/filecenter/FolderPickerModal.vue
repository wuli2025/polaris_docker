<script setup lang="ts">
/**
 * 文件夹选择器(盘点前先扫一眼):扫根 + 第一层 → 层级勾选/懒加载下钻 → 开始盘点。
 * 自 FileCenter.vue 原样搬出;整棵选择器状态(节点/勾选/展开/大小队列)都是纯局部状态,
 * 挂载即扫描(等价旧 openFolderPicker),「开始盘点」把 roots/exclude 上抛给父层执行。
 */
import { ref, reactive, computed, onMounted, onBeforeUnmount } from "vue";
import {
  FolderTree,
  X,
  ArrowDownWideNarrow,
  ChevronRight,
  Layers,
  Folder,
  Check,
  RotateCcw,
  RefreshCw,
  FolderSearch,
} from "@lucide/vue";
import OrbitSpinner from "../icons/OrbitSpinner.vue";
import { files as fc, type FolderNode, type ScanRootInfo } from "../../tauri";
import { fmtBytes } from "./shared";
import { takeScan, peekSizes, onPrewarmSize, stopSizePrewarm } from "../../lib/scanPrewarm";

const emit = defineEmits<{
  (e: "close"): void;
  /** 扫描忙碌态上抛(父层工具条的「盘点」按钮转圈用)。 */
  (e: "busy", v: boolean): void;
  /** 开始盘点:roots + exclude + 是否完整盘点(full=true 逐目录重扫)。 */
  (e: "start", roots: string[], exclude: string[], full: boolean): void;
}>();

const pickerLoading = ref(false);
const pickerErr = ref("");
const pickerTruncated = ref(false);
const scanRoots = ref<ScanRootInfo[]>([]);
// 已加载的全部节点(根的第一层 + 用户点开后懒加载的更深层)。path → 节点。
const allNodes = reactive(new Map<string, FolderNode>());
// parent 路径 → 其直属子文件夹(后端已按名排序)。
const childIndex = reactive(new Map<string, FolderNode[]>());
// 正在懒加载子目录的文件夹路径。
const childLoading = reactive(new Set<string>());
// 文件夹路径 → 递归总量{files,bytes}(按需限并发计算)。
const sizeCache = reactive(new Map<string, { files: number; bytes: number }>());
// 层级复选框:显式勾上 / 显式取消 的路径(根路径或文件夹路径)。
// 未显式标记的节点 → 继承最近祖先的标记;祖先都没标记 → 看所属根的 defaultOn。
const checked = reactive(new Set<string>());
const unchecked = reactive(new Set<string>());
// 展开了子目录的文件夹/根路径。
const expanded = reactive(new Set<string>());
// 选择器里同级文件夹的排序:size=按大小从大到小(默认,大文件夹先露脸)/ name=按名称。
const pickerSort = ref<"size" | "name">("size");

// ── 打开「盘点」:先扫文件夹结构(根+第一层),让用户勾选 / 逐层点开要盘点的目录 ──
function ingestNodes(nodes: FolderNode[]) {
  for (const n of nodes) {
    if (!allNodes.has(n.path)) allNodes.set(n.path, n);
    const arr = childIndex.get(n.parent);
    if (arr) {
      if (!arr.some((x) => x.path === n.path)) arr.push(n);
    } else {
      childIndex.set(n.parent, [n]);
    }
    requestSize(n.path); // 后台算这个文件夹有多大
  }
}
onMounted(async () => {
  pickerLoading.value = true;
  emit("busy", true);
  pickerErr.value = "";
  // 预热接管:后台低并发大小泵停掉(IO 让给本组件的 4 并发队列),
  // 已预算好的大小先秒回填,还在飞的完成后经订阅落进来。
  stopSizePrewarm();
  for (const [p, s] of peekSizes()) sizeCache.set(p, s);
  onPrewarmSize((p, s) => queueSizeResult(p, s));
  try {
    // 文件中心挂载时已空闲预扫(scanPrewarm),这里通常直接命中缓存,零等待。
    const res = await takeScan();
    scanRoots.value = res.roots;
    pickerTruncated.value = res.truncated;
    ingestNodes(res.folders);
    // 默认勾上的根自动展开,方便直接看到知识库的子目录。
    for (const r of res.roots) if (r.defaultOn) expanded.add(r.path);
  } catch (e: any) {
    pickerErr.value = `扫描失败:${e?.message ?? e}`;
    scanRoots.value = [];
  } finally {
    pickerLoading.value = false;
    emit("busy", false);
  }
});
onBeforeUnmount(() => {
  onPrewarmSize(null);
  if (sizeFlushRaf) cancelAnimationFrame(sizeFlushRaf);
});

const rootByPath = computed(() => {
  const m = new Map<string, ScanRootInfo>();
  for (const r of scanRoots.value) m.set(r.path, r);
  return m;
});

// 从某路径向上的祖先链(不含自身),最后到所属根路径。
function ancestorsOf(path: string): string[] {
  const out: string[] = [];
  let cur = allNodes.get(path)?.parent;
  while (cur) {
    out.push(cur);
    const pn = allNodes.get(cur);
    if (!pn) break; // cur 已是根路径(根没有 FolderNode)
    cur = pn.parent;
  }
  return out;
}
function rootOf(path: string): string {
  return allNodes.get(path)?.root ?? path;
}
// 节点(根或文件夹)是否「最终被勾选」= 盘点范围内。
function isIncluded(path: string): boolean {
  const chain = [path, ...ancestorsOf(path)];
  for (const p of chain) {
    if (checked.has(p)) return true;
    if (unchecked.has(p)) return false;
  }
  return rootByPath.value.get(rootOf(path))?.defaultOn ?? false;
}
// 父节点(根→无父,看根默认;文件夹→其 parent)的最终状态。
function parentIncluded(path: string): boolean {
  const node = allNodes.get(path);
  if (!node) return false; // 这是根:无父
  return isIncluded(node.parent);
}
function toggleNode(path: string) {
  const want = !isIncluded(path);
  // 清掉所有(已加载)后代的显式标记,让它们继承新状态。
  for (const key of allNodes.keys()) {
    if (key !== path && (key.startsWith(path + "\\") || key.startsWith(path + "/"))) {
      checked.delete(key);
      unchecked.delete(key);
    }
  }
  const node = allNodes.get(path);
  // 基准 = 父节点最终态(根没有父 → 看根 defaultOn)。
  const base = node ? isIncluded(node.parent) : (rootByPath.value.get(path)?.defaultOn ?? false);
  checked.delete(path);
  unchecked.delete(path);
  if (want !== base) (want ? checked : unchecked).add(path);
}
async function toggleExpand(path: string) {
  if (expanded.has(path)) {
    expanded.delete(path);
    return;
  }
  expanded.add(path);
  // 文件夹(非根)且还没加载过子目录 → 懒加载。
  const node = allNodes.get(path);
  if (node && node.hasChildren && !childIndex.has(path) && !childLoading.has(path)) {
    childLoading.add(path);
    try {
      const kids = await fc.scanFolderChildren(node.root, path);
      ingestNodes(kids);
      if (!childIndex.has(path)) childIndex.set(path, []); // 兜底:全被剪掉 → 空数组
    } catch {
      childIndex.set(path, []);
    } finally {
      childLoading.delete(path);
    }
  }
}
function resetPicker() {
  checked.clear();
  unchecked.clear();
}
// 已勾选盘点的文件夹数(用于底部计数;只统计已加载节点)。
const pickerSelected = computed(() => {
  let n = 0;
  for (const node of allNodes.values()) if (isIncluded(node.path)) n++;
  return n;
});
// 至少勾了一个根或文件夹?
const pickerHasSelection = computed(
  () => scanRoots.value.some((r) => isIncluded(r.path)) || pickerSelected.value > 0,
);

// ── 文件夹大小:限并发的后台计算队列 ──
const SIZE_CONCURRENCY = 4;
const sizeQueue: string[] = [];
const sizeInflight = new Set<string>();
let sizeActive = 0;
// rAF 合帧回填(借鉴有戏剧场):sizeCache 是响应式 Map,每 set 一次就触发
// visibleRows 整树重排序 + siblingStats 重算;结果先攒缓冲,每帧一次性落盘,
// 4 并发连续到达也只有每帧一轮 diff。
let sizeBuf: Array<[string, { files: number; bytes: number }]> = [];
let sizeFlushRaf = 0;
function queueSizeResult(path: string, size: { files: number; bytes: number }) {
  sizeBuf.push([path, size]);
  if (!sizeFlushRaf) {
    sizeFlushRaf = requestAnimationFrame(() => {
      sizeFlushRaf = 0;
      const buf = sizeBuf;
      sizeBuf = [];
      for (const [p, s] of buf) sizeCache.set(p, s);
    });
  }
}
function requestSize(path: string) {
  if (sizeCache.has(path) || sizeInflight.has(path) || sizeQueue.includes(path)) return;
  sizeQueue.push(path);
  pumpSize();
}
function pumpSize() {
  while (sizeActive < SIZE_CONCURRENCY && sizeQueue.length) {
    const p = sizeQueue.shift()!;
    sizeInflight.add(p);
    sizeActive++;
    fc.folderSize(p)
      .then((r) => queueSizeResult(p, r))
      .catch(() => queueSizeResult(p, { files: 0, bytes: 0 }))
      .finally(() => {
        sizeActive--;
        sizeInflight.delete(p);
        pumpSize();
      });
  }
}
// 仿 WizTree:每层同级里算占比 + 条形(占比=该文件夹/同级已知总和;条长=该文件夹/同级最大)。
const siblingStats = computed(() => {
  const m = new Map<string, { sum: number; max: number }>();
  for (const [parent, kids] of childIndex) {
    let sum = 0;
    let max = 1;
    for (const k of kids) {
      const s = sizeCache.get(k.path);
      if (s) {
        sum += s.bytes;
        if (s.bytes > max) max = s.bytes;
      }
    }
    m.set(parent, { sum, max });
  }
  return m;
});
function sizePct(node: FolderNode): number | null {
  const s = sizeCache.get(node.path);
  if (!s) return null;
  const st = siblingStats.value.get(node.parent);
  if (!st || st.sum <= 0) return 0;
  return (s.bytes / st.sum) * 100;
}
function sizeBar(node: FolderNode): number {
  const s = sizeCache.get(node.path);
  if (!s) return 0;
  const st = siblingStats.value.get(node.parent);
  if (!st || st.max <= 0) return 0;
  return s.bytes / st.max;
}

// ── 扁平化「当前可见行」(支持任意深度的展开)给模板渲染 ──
interface PickerRow {
  key: string;
  kind: "root" | "folder" | "loading" | "empty";
  level: number;
  root?: ScanRootInfo;
  node?: FolderNode;
}
const visibleRows = computed<PickerRow[]>(() => {
  const rows: PickerRow[] = [];
  const pushChildren = (parentPath: string, level: number) => {
    const kids = childIndex.get(parentPath);
    if (childLoading.has(parentPath) && (!kids || !kids.length)) {
      rows.push({ key: parentPath + "#loading", kind: "loading", level });
      return;
    }
    if (!kids || !kids.length) {
      rows.push({ key: parentPath + "#empty", kind: "empty", level });
      return;
    }
    // 同级排序:按大小从大到小(未知大小排末尾),或按名称(后端已按名排好)。
    const ordered =
      pickerSort.value === "size"
        ? [...kids].sort((a, b) => (sizeCache.get(b.path)?.bytes ?? -1) - (sizeCache.get(a.path)?.bytes ?? -1))
        : kids;
    for (const k of ordered) {
      rows.push({ key: k.path, kind: "folder", level, node: k });
      if (expanded.has(k.path)) pushChildren(k.path, level + 1);
    }
  };
  for (const r of scanRoots.value) {
    rows.push({ key: r.path, kind: "root", level: 0, root: r });
    if (expanded.has(r.path)) pushChildren(r.path, 1);
  }
  return rows;
});

// 计算要传给盘点的 roots(最顶层被勾选项)+ exclude(被勾范围内又取消的最顶层项)。
function collectInventoryArgs(): { roots: string[]; exclude: string[] } {
  const roots: string[] = [];
  const exclude: string[] = [];
  // 根:被勾选 → 成为盘点根。
  for (const r of scanRoots.value) {
    if (isIncluded(r.path)) roots.push(r.path);
  }
  // 文件夹:被勾但父未勾 → 顶层勾选项(成为根);未勾但父已勾 → 顶层排除项。
  for (const f of allNodes.values()) {
    const inc = isIncluded(f.path);
    const pinc = parentIncluded(f.path);
    if (inc && !pinc) roots.push(f.path);
    else if (!inc && pinc) exclude.push(f.path);
  }
  return { roots, exclude };
}
function startInventoryFromPicker(full = false) {
  const { roots, exclude } = collectInventoryArgs();
  emit("start", roots, exclude, full);
}
</script>

<template>
  <div class="picker-scrim" @click="emit('close')">
    <div class="picker glass" @click.stop>
      <div class="picker-head">
        <div class="picker-title">
          <FolderTree :size="17" :stroke-width="1.7" />
          <span>选择要盘点的文件夹</span>
        </div>
        <button class="picker-close" @click="emit('close')"><X :size="16" :stroke-width="2" /></button>
      </div>
      <div class="picker-sub">
        勾选<b>要盘点的目录</b>(可勾知识库之外的盘符 / 文件夹),再开始建库。
        所有盘 / 卷<b>默认已全部勾上</b>(系统、缓存目录自动跳过)—— 想缩小范围展开后取消即可。
      </div>
      <div v-if="!pickerLoading && scanRoots.length" class="picker-sortbar">
        <span class="ps-lab"><ArrowDownWideNarrow :size="13" :stroke-width="1.7" /> 同级排序</span>
        <button class="ps-btn" :class="{ on: pickerSort === 'size' }" @click="pickerSort = 'size'">大小(大→小)</button>
        <button class="ps-btn" :class="{ on: pickerSort === 'name' }" @click="pickerSort = 'name'">名称</button>
      </div>

      <div v-if="pickerLoading" class="picker-loading">
        <OrbitSpinner :size="20" /> 正在扫描文件夹结构…
      </div>
      <div v-else-if="pickerErr" class="picker-error">{{ pickerErr }}</div>
      <div v-else-if="!scanRoots.length" class="picker-error">没有可盘点的目录。</div>
      <div v-else class="picker-tree">
        <template v-for="row in visibleRows" :key="row.key">
          <!-- 根行(整盘/整库) -->
          <div
            v-if="row.kind === 'root'"
            class="picker-row root"
            :class="{ off: !isIncluded(row.root!.path) }"
            :style="{ paddingLeft: 8 + 'px' }"
          >
            <button class="pk-check" :class="{ on: isIncluded(row.root!.path) }" @click="toggleNode(row.root!.path)">
              <Check v-if="isIncluded(row.root!.path)" :size="12" :stroke-width="2.6" />
            </button>
            <button class="pk-expand vis" @click="toggleExpand(row.root!.path)">
              <ChevronRight :size="19" :stroke-width="2" :class="{ open: expanded.has(row.root!.path) }" />
            </button>
            <Layers :size="14" :stroke-width="1.8" class="pk-ic" />
            <span class="pk-name root-name" :title="row.root!.path">{{ row.root!.label }}</span>
            <span class="pk-meta">{{ row.root!.path }}</span>
          </div>
          <!-- 文件夹行(任意深度) -->
          <div
            v-else-if="row.kind === 'folder'"
            class="picker-row"
            :class="{ off: !isIncluded(row.node!.path) }"
            :style="{ paddingLeft: 8 + row.level * 20 + 'px' }"
          >
            <button class="pk-check" :class="{ on: isIncluded(row.node!.path) }" @click="toggleNode(row.node!.path)">
              <Check v-if="isIncluded(row.node!.path)" :size="12" :stroke-width="2.6" />
            </button>
            <button class="pk-expand" :class="{ vis: row.node!.hasChildren }" @click="toggleExpand(row.node!.path)">
              <ChevronRight :size="19" :stroke-width="2" :class="{ open: expanded.has(row.node!.path) }" />
            </button>
            <Folder :size="15" :stroke-width="1.6" class="pk-ic" />
            <span class="pk-name" :title="row.node!.path">{{ row.node!.name }}</span>
            <template v-if="sizeCache.get(row.node!.path)">
              <span class="pk-bar" :title="(sizePct(row.node!) ?? 0).toFixed(1) + '% 占同级'">
                <span class="pk-bar-fill" :style="{ width: Math.max(2, sizeBar(row.node!) * 100) + '%' }" />
              </span>
              <span class="pk-pct">{{ (sizePct(row.node!) ?? 0).toFixed(1) }}%</span>
              <span class="pk-size">{{ fmtBytes(sizeCache.get(row.node!.path)!.bytes) }}</span>
            </template>
            <span v-else class="pk-meta calc">计算大小…</span>
          </div>
          <!-- 懒加载占位 -->
          <div
            v-else-if="row.kind === 'loading'"
            class="picker-row sub-loading"
            :style="{ paddingLeft: 8 + row.level * 20 + 'px' }"
          >
            <OrbitSpinner :size="13" /> 加载子文件夹…
          </div>
          <!-- 空目录占位 -->
          <div
            v-else
            class="picker-row empty-row"
            :style="{ paddingLeft: 8 + row.level * 20 + 'px' }"
          >
            （无子文件夹）
          </div>
        </template>
        <div v-if="pickerTruncated" class="picker-trunc">顶层文件夹太多,列表已截断到前 5000 个。</div>
      </div>

      <div class="picker-foot">
        <button class="pk-reset" :disabled="!checked.size && !unchecked.size" title="恢复默认勾选" @click="resetPicker">
          <RotateCcw :size="13" :stroke-width="1.8" /> 恢复默认
        </button>
        <span class="pk-count">已选 <b>{{ pickerSelected }}</b> 个文件夹</span>
        <button
          class="pk-full"
          :disabled="pickerLoading || !pickerHasSelection"
          title="完整盘点:忽略目录缓存,逐个目录重扫一遍。比智能增量慢,但能补回极少数「原地追加写入、没改动目录」的文件"
          @click="startInventoryFromPicker(true)"
        >
          <RefreshCw :size="13" :stroke-width="1.8" /> 完整盘点
        </button>
        <button
          class="pk-go"
          :disabled="pickerLoading || !pickerHasSelection"
          title="智能增量:只重扫修改时间变过的子树,没变的整棵跳过。重扫快一个数量级"
          @click="startInventoryFromPicker(false)"
        >
          <FolderSearch :size="14" :stroke-width="1.8" /> 开始盘点
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* ── 琉璃通用 ── */
.glass {
  background: color-mix(in srgb, var(--panel) 68%, transparent);
  -webkit-backdrop-filter: blur(22px) saturate(1.5);
  backdrop-filter: blur(22px) saturate(1.5);
  border: 1px solid var(--border-soft);
  border-radius: 16px;
}

/* ── 文件夹选择器(盘点前先扫一眼) ── */
.picker-scrim {
  position: absolute;
  inset: 0;
  z-index: 30;
  display: flex;
  align-items: center;
  justify-content: center;
  background: color-mix(in srgb, var(--bg) 50%, transparent);
  -webkit-backdrop-filter: blur(3px);
  backdrop-filter: blur(3px);
  padding: 24px;
}
.picker {
  width: min(760px, 100%);
  max-height: min(82vh, 760px);
  display: flex;
  flex-direction: column;
  padding: 18px 20px 16px;
  box-shadow: var(--shadow-lg, 0 24px 60px -20px rgba(0, 0, 0, 0.45));
}
.picker-head { display: flex; align-items: center; justify-content: space-between; }
.picker-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-family: var(--serif);
  font-size: 16px;
  letter-spacing: 1px;
  color: var(--ink);
}
.picker-close {
  display: inline-flex;
  border: none;
  background: transparent;
  color: var(--dim);
  cursor: pointer;
  padding: 4px;
  border-radius: 7px;
}
.picker-close:hover { color: var(--text); background: var(--selection-bg); }
.picker-sub { margin: 6px 0 12px; font-size: 12.5px; line-height: 1.6; color: var(--muted); }
.picker-sub b { color: var(--text); }
.picker-sortbar { display: flex; align-items: center; gap: 7px; margin: 0 0 10px; }
.ps-lab { display: inline-flex; align-items: center; gap: 5px; font-size: 12px; color: var(--muted); margin-right: 2px; }
.ps-btn {
  height: 25px; padding: 0 10px; border-radius: 7px; font-size: 11.5px; cursor: pointer;
  border: 1px solid var(--border-soft); background: color-mix(in srgb, var(--panel) 55%, transparent); color: var(--text-2);
}
.ps-btn:hover { color: var(--text); border-color: var(--border-strong); }
.ps-btn.on { color: var(--primary); border-color: color-mix(in srgb, var(--primary) 45%, transparent); background: color-mix(in srgb, var(--primary) 10%, transparent); }
.picker-loading, .picker-error {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 26px 4px;
  color: var(--muted);
  font-size: 13px;
}
.picker-tree {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  border: 1px solid var(--border-soft);
  border-radius: 12px;
  padding: 6px;
  background: color-mix(in srgb, var(--panel) 40%, transparent);
}
.picker-root + .picker-root { margin-top: 8px; }
.picker-root-head {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 8px 4px;
  font-size: 11.5px;
  color: var(--muted);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.picker-row {
  display: flex;
  align-items: center;
  gap: 8px;
  height: 32px;
  padding: 0 8px;
  border-radius: 8px;
  /* 行级布局隔离:大小回填/勾选态翻转只重排本行,不外溢整树(定高行,安全)。 */
  contain: content;
}
.picker-row:hover { background: var(--selection-bg); }
.picker-row.root { margin-top: 4px; }
.picker-row.root .root-name { font-weight: 650; font-size: 13.5px; }
.picker-row.empty-row { font-size: 12px; color: var(--dim); height: 26px; }
.picker-row.sub-loading { font-size: 12px; color: var(--muted); height: 28px; gap: 7px; }
.picker-row.off .pk-name, .picker-row.off .pk-meta { opacity: 0.4; }
.picker-row.off .root-name { text-decoration: none; }
.pk-check {
  flex: none;
  width: 17px;
  height: 17px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border: 1.5px solid var(--border-strong);
  border-radius: 5px;
  background: transparent;
  color: var(--btn-solid-text);
  cursor: pointer;
  transition: background 0.14s, border-color 0.14s;
}
.pk-check.on { background: var(--primary); border-color: var(--primary); color: #fff; }
.pk-check:disabled { opacity: 0.35; cursor: default; }
.pk-expand {
  flex: none;
  width: 28px;
  height: 28px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border: none;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  visibility: hidden;
  border-radius: 8px;
  transition: background 0.14s, color 0.14s;
}
.pk-expand.vis { visibility: visible; }
.pk-expand.vis:hover { background: var(--selection-bg); color: var(--text); }
.pk-expand :deep(svg) { transition: transform 0.18s; }
.pk-expand :deep(svg.open) { transform: rotate(90deg); }
.pk-ic { color: var(--muted); flex: none; }
.pk-name {
  flex: 1;
  min-width: 0;
  font-size: 13px;
  color: var(--text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.pk-meta { flex: none; font-size: 11px; color: var(--muted); font-variant-numeric: tabular-nums; }
.pk-meta.calc { font-style: italic; opacity: 0.7; }
/* ── 仿 WizTree 占比条 + 百分比 + 大小 ── */
.pk-bar {
  flex: none;
  width: 120px;
  height: 9px;
  border-radius: 5px;
  background: color-mix(in srgb, var(--ink) 9%, transparent);
  overflow: hidden;
}
.pk-bar-fill {
  display: block;
  height: 100%;
  border-radius: 5px;
  background: linear-gradient(90deg, var(--primary), var(--gold));
}
.pk-pct {
  flex: none;
  width: 50px;
  text-align: right;
  font-size: 12px;
  font-weight: 600;
  color: var(--text-2);
  font-variant-numeric: tabular-nums;
}
.pk-size {
  flex: none;
  width: 78px;
  text-align: right;
  font-size: 11.5px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}
.picker-trunc { padding: 8px; font-size: 11.5px; color: var(--dim); text-align: center; }
.picker-foot {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-top: 14px;
}
.pk-reset {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  height: 30px;
  padding: 0 12px;
  border: 1px solid var(--border-soft);
  background: transparent;
  color: var(--text-2);
  border-radius: 9px;
  font-size: 12px;
  cursor: pointer;
}
.pk-reset:hover:not(:disabled) { border-color: var(--border-strong); color: var(--text); }
.pk-reset:disabled { opacity: 0.45; cursor: default; }
.pk-count { flex: 1; font-size: 12.5px; color: var(--muted); text-align: center; }
.pk-count b { color: var(--text); font-variant-numeric: tabular-nums; }
.pk-go {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  height: 34px;
  padding: 0 18px;
  border: none;
  border-radius: 10px;
  background: var(--btn-solid-bg);
  color: var(--btn-solid-text);
  font-size: 13px;
  cursor: pointer;
  transition: opacity 0.16s, transform 0.16s;
}
.pk-go:hover:not(:disabled) { transform: translateY(-1px); }
.pk-go:disabled { opacity: 0.55; cursor: default; }
/* 完整盘点:次级描边钮(智能增量才是默认主钮)。 */
.pk-full {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  height: 34px;
  padding: 0 13px;
  border: 1px solid var(--border-soft);
  background: transparent;
  color: var(--text-2);
  border-radius: 10px;
  font-size: 12.5px;
  cursor: pointer;
  transition: border-color 0.16s, color 0.16s;
}
.pk-full:hover:not(:disabled) { border-color: var(--border-strong); color: var(--text); }
.pk-full:disabled { opacity: 0.45; cursor: default; }
</style>
