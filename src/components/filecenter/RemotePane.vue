<script setup lang="ts">
/**
 * 远程源浏览(经 iroh 隧道接入的 NAS/主机):像本机一样浏览、下载它的盘。
 * 自 FileCenter.vue 原样搬出;浏览态(当前路径/条目)是纯局部状态,下沉到本组件。
 * 父层点远程 chip → source/openTick 变化 → 本组件从根目录重新打开(与旧 pickRemote 行为一致)。
 */
import { ref, computed, watch } from "vue";
import { Server, ChevronRight, ChevronLeft, RefreshCw, Folder, ArrowDownWideNarrow, WifiOff } from "@lucide/vue";
import OrbitSpinner from "../icons/OrbitSpinner.vue";
import type { RemoteSource } from "../../features/interconnect/remoteSources";
import { fsList, fsDownload, joinPath, type FsEntry } from "../../features/collab/fsapi";

const props = defineProps<{
  source: RemoteSource;
  /** 每次点远程 chip 自增:即便还是同一个源,也重新回到根目录(保持旧行为)。 */
  openTick: number;
}>();

const rcwd = ref(""); // 当前相对路径
const rentries = ref<FsEntry[]>([]);
const rloading = ref(false);
const rerr = ref("");
const rcrumbs = computed(() => rcwd.value.split("/").filter(Boolean));

async function openRemote(rel = "") {
  rcwd.value = rel;
  rerr.value = "";
  rloading.value = true;
  try {
    rentries.value = await fsList(props.source, rel);
  } catch (e) {
    rerr.value = (e as Error).message;
    rentries.value = [];
  } finally {
    rloading.value = false;
  }
}
function enterRemote(e: FsEntry) {
  if (e.is_dir) openRemote(joinPath(rcwd.value, e.name));
}
function remoteUp() {
  const parts = rcrumbs.value.slice(0, -1);
  openRemote(parts.join("/"));
}
function remoteCrumbTo(i: number) {
  openRemote(rcrumbs.value.slice(0, i + 1).join("/"));
}
async function downloadRemote(e: FsEntry) {
  if (e.is_dir) return;
  try {
    const blob = await fsDownload(props.source, joinPath(rcwd.value, e.name));
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = e.name;
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 4000);
  } catch (err) {
    rerr.value = (err as Error).message;
  }
}
function fmtRemoteSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1048576) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1073741824) return `${(n / 1048576).toFixed(1)} MB`;
  return `${(n / 1073741824).toFixed(2)} GB`;
}

// 源变化 / 再次点 chip → 从根目录重新打开。
watch(
  () => [props.source.id, props.openTick] as const,
  () => openRemote(""),
  { immediate: true },
);
</script>

<template>
  <div class="fc-remote glass">
    <div class="rm-bar">
      <span class="rm-src"><Server :size="14" :stroke-width="1.8" /> {{ source.name || "远程源" }}</span>
      <div class="rm-crumb">
        <button class="rm-cr" @click="openRemote('')">根</button>
        <template v-for="(c, i) in rcrumbs" :key="i">
          <ChevronRight :size="12" class="rm-sep" />
          <button class="rm-cr" @click="remoteCrumbTo(i)">{{ c }}</button>
        </template>
      </div>
      <button class="rm-btn" :disabled="!rcwd" title="上一级" @click="remoteUp"><ChevronLeft :size="14" /> 上级</button>
      <button class="rm-btn" title="刷新" @click="openRemote(rcwd)"><RefreshCw :size="14" /></button>
    </div>

    <div v-if="rloading" class="rm-state"><OrbitSpinner :size="18" /> 读取远程目录…</div>
    <div v-else-if="rerr" class="rm-state rm-err"><WifiOff :size="16" /> {{ rerr }}</div>
    <div v-else-if="!rentries.length" class="rm-state">这个目录是空的。</div>
    <div v-else class="rm-list">
      <div
        v-for="e in rentries"
        :key="e.name"
        class="rm-row"
        :class="{ dir: e.is_dir }"
        @dblclick="enterRemote(e)"
      >
        <span class="rm-ic"><Folder v-if="e.is_dir" :size="16" :stroke-width="1.8" /><Server v-else :size="15" :stroke-width="1.7" /></span>
        <span class="rm-name" @click="enterRemote(e)">{{ e.name }}</span>
        <span class="rm-sz">{{ e.is_dir ? "" : fmtRemoteSize(e.size) }}</span>
        <button v-if="!e.is_dir" class="rm-dl" @click.stop="downloadRemote(e)"><ArrowDownWideNarrow :size="13" /> 下载</button>
        <button v-else class="rm-dl ghost" @click.stop="enterRemote(e)"><ChevronRight :size="13" /> 进入</button>
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

/* ── 远程源浏览(NAS/主机的盘,经 iroh 隧道) ── */
.fc-remote { flex: 1 1 auto; min-height: 0; display: flex; flex-direction: column; margin: 12px; padding: 0; overflow: hidden; }
.rm-bar { display: flex; align-items: center; gap: 10px; padding: 11px 14px; border-bottom: 1px solid var(--hairline); flex-wrap: wrap; }
.rm-src { display: inline-flex; align-items: center; gap: 6px; font-weight: 600; font-size: 13.5px; color: var(--ink); }
.rm-crumb { display: flex; align-items: center; gap: 3px; flex: 1; min-width: 0; overflow-x: auto; }
.rm-cr { border: none; background: none; color: var(--muted); cursor: pointer; font-size: 12.5px; padding: 3px 6px; border-radius: 6px; white-space: nowrap; }
.rm-cr:hover { color: var(--ink); background: var(--selection-bg); }
.rm-sep { color: var(--dim); flex: none; }
.rm-btn { display: inline-flex; align-items: center; gap: 4px; border: 1px solid var(--border); background: var(--panel); color: var(--text); cursor: pointer; font-size: 12px; padding: 5px 9px; border-radius: 8px; }
.rm-btn:hover:not(:disabled) { border-color: var(--primary); color: var(--ink); }
.rm-btn:disabled { opacity: .45; cursor: not-allowed; }
.rm-state { display: flex; align-items: center; justify-content: center; gap: 8px; padding: 40px; color: var(--muted); font-size: 13px; }
.rm-state.rm-err { color: var(--vermilion); }
.rm-list { flex: 1; overflow-y: auto; padding: 6px 8px; }
.rm-row { display: flex; align-items: center; gap: 10px; padding: 8px 10px; border-radius: 9px; cursor: default; }
.rm-row:hover { background: var(--selection-bg); }
.rm-row.dir { cursor: pointer; }
.rm-ic { flex: none; width: 24px; display: flex; align-items: center; justify-content: center; color: var(--muted); }
.rm-row.dir .rm-ic { color: var(--primary); }
.rm-name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 13px; color: var(--text); cursor: pointer; }
.rm-row.dir .rm-name { font-weight: 600; color: var(--ink); }
.rm-sz { flex: none; font-family: var(--mono); font-size: 11.5px; color: var(--muted); min-width: 68px; text-align: right; }
.rm-dl { flex: none; display: inline-flex; align-items: center; gap: 4px; border: 1px solid var(--border); background: var(--panel); color: var(--text); cursor: pointer; font-size: 11.5px; padding: 4px 9px; border-radius: 7px; }
.rm-dl:hover { border-color: var(--primary); color: var(--primary); }
.rm-dl.ghost { color: var(--muted); }
</style>
