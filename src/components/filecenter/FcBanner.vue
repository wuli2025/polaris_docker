<script setup lang="ts">
/**
 * 文件中心顶部琉璃横幅(可收起:收起后只留一条,显示文件数/总量)。
 * 自 FileCenter.vue 原样搬出;收起状态持久化在本组件内。
 */
import { ref, computed, watch } from "vue";
import { Orbit, ChevronDown, Info } from "@lucide/vue";
import type { FileOverview } from "../../tauri";
import { fmtBytes, loadFc, saveFc } from "./shared";

const props = defineProps<{ overview: FileOverview | null }>();

// 折叠状态(持久化记住用户选择)。默认把横幅收起 → 功能键整体上移、把纵向空间让给下方可观看的文件网格。
const bannerOpen = ref(loadFc("polaris.fc.banner", false));
watch(bannerOpen, (v) => saveFc("polaris.fc.banner", v));

const headerStats = computed<{ label: string; value: string; hint?: string }[]>(() => {
  const o = props.overview;
  if (!o) return [];
  return [
    { label: "文件", value: o.totalFiles.toLocaleString() },
    {
      label: "总量",
      value: fmtBytes(o.totalBytes),
      // 「总量」= 磁盘实占空间(与资源管理器「占用空间」同口径),不是文件声称的逻辑大小。
      // 虚拟磁盘(.vhdx)、虚拟机盘、稀疏/压缩文件的逻辑大小可虚高几十倍,这里按实占算才准 ——
      // 这条提示就是为了让人看到数字变小时不会误以为「少算了 / 文件丢了」(文件数才是真凭据)。
      hint: "磁盘实占空间,与资源管理器「占用空间」一致 · 不含虚拟磁盘/稀疏文件的虚高逻辑大小 · 文件数不受影响",
    },
    { label: "语义簇", value: String(o.clusters.length) },
    { label: "已嵌入", value: `${o.embeddedFiles}/${o.textFiles}` },
  ];
});
</script>

<template>
  <div class="fc-banner glass" :class="{ collapsed: !bannerOpen }">
    <div class="fc-title-wrap">
      <div class="fc-title"><Orbit :size="17" :stroke-width="1.6" /> 文件中心</div>
      <div v-if="bannerOpen" class="fc-sub">同类数据自动归在一起 · 缩略图 / 首帧 / 类型图标 · 智能检索</div>
      <div v-else class="fc-mini">
        {{ (overview?.totalFiles ?? 0).toLocaleString() }} 个文件 · {{ fmtBytes(overview?.totalBytes ?? 0) }}
      </div>
    </div>
    <div v-if="bannerOpen" class="fc-stats">
      <div v-for="s in headerStats" :key="s.label" class="stat" :class="{ 'has-hint': s.hint }" :title="s.hint || ''">
        <div class="stat-val">{{ s.value }}</div>
        <div class="stat-lab">
          {{ s.label }}<Info v-if="s.hint" :size="11" :stroke-width="2" class="stat-info" />
        </div>
      </div>
    </div>
    <button class="fc-collapse" :title="bannerOpen ? '收起' : '展开'" @click="bannerOpen = !bannerOpen">
      <ChevronDown :size="16" :stroke-width="1.8" :class="{ flip: !bannerOpen }" />
    </button>
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

/* ── 横幅 ── */
.fc-banner {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 16px 22px;
  position: relative;
  overflow: hidden;
}
.fc-banner::before {
  content: "";
  position: absolute;
  inset: 0;
  background:
    radial-gradient(120% 140% at 0% 0%, color-mix(in srgb, var(--primary) 16%, transparent), transparent 55%),
    radial-gradient(120% 140% at 100% 100%, color-mix(in srgb, var(--gold) 14%, transparent), transparent 55%);
  pointer-events: none;
}
.fc-title-wrap { position: relative; }
.fc-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-family: var(--serif);
  font-size: 18px;
  letter-spacing: 1.5px;
  color: var(--ink);
}
.fc-sub {
  margin-top: 5px;
  font-size: 12px;
  color: var(--muted);
  letter-spacing: 0.3px;
}
.fc-stats {
  display: flex;
  gap: 26px;
  position: relative;
}
.stat { text-align: right; }
.stat-val {
  font-size: 19px;
  font-weight: 650;
  color: var(--text);
  font-variant-numeric: tabular-nums;
}
.stat-lab {
  font-size: 11px;
  color: var(--muted);
  margin-top: 2px;
  display: inline-flex;
  align-items: center;
  gap: 3px;
}
.stat.has-hint { cursor: help; }
.stat-info {
  color: var(--muted);
  opacity: 0.6;
  vertical-align: -1px;
}
.stat.has-hint:hover .stat-info { opacity: 1; color: var(--gold, var(--text)); }

/* 横幅收起态:压成一条,只留标题 + 文件数/总量 */
.fc-banner { transition: padding 0.2s; }
.fc-banner.collapsed { padding: 9px 16px 9px 22px; }
.fc-mini {
  margin-top: 3px;
  font-size: 12px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
}
.fc-collapse {
  position: relative;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  border: 1px solid var(--border-soft);
  background: color-mix(in srgb, var(--panel) 70%, transparent);
  color: var(--muted);
  border-radius: 8px;
  cursor: pointer;
  flex: none;
  transition: color 0.16s, border-color 0.16s;
}
.fc-collapse:hover { color: var(--text); border-color: var(--border-strong); }
.fc-collapse .flip { transform: rotate(180deg); }
.fc-collapse :deep(svg) { transition: transform 0.2s; }
</style>
