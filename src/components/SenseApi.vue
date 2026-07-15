<script setup lang="ts">
// 寓言计划 · 感官 API 管理页(设置 → 寓言计划 API)
// 形态对标参考图:按能力分组的服务商卡片墙;本地模型卡带下载进度条;
// 点卡片开配置弹窗(启用开关 / API Key + 获取链接 / 模型列表设默认 / 一键探活)。
// 页尾:回声层「每日做梦」(定时收录对话蒸馏成记忆)。
import { onMounted, onUnmounted, ref, computed, watch } from "vue";
import { invoke, listen, openUrl } from "../tauri";
import { useAppStore } from "../stores/app";
import { useFileTasksStore } from "../stores/fileTasks";

defineOptions({ name: "SenseApi" });

const app = useAppStore();
// 盘点/建索引托管给全局任务 store(App 级监听,脱离本页生命周期)——
// 这样在感官页点了盘点/建索引，切到别的界面也照常后台跑、全局任务中心可见。
const tasks = useFileTasksStore();

interface SenseModel {
  id: string;
  name: string;
  note: string;
  recommended: boolean;
}
interface SenseProviderView {
  id: string;
  name: string;
  sense: string;
  kind: string; // api | local
  enabled: boolean;
  base_url: string;
  api_key_masked: string;
  key_ready: boolean;
  key_source: string;
  default_model: string;
  models: SenseModel[];
  get_key_url: string;
  docs_url: string;
  note: string;
  free: boolean;
  optional: boolean;
  recommended: boolean;
  pack_id: string | null;
  installed: boolean;
}
interface SenseGroupView {
  sense: string;
  label: string;
  desc: string;
  providers: SenseProviderView[];
}
interface SensePackView {
  id: string;
  name: string;
  note: string;
  approx_mb: number;
  installed: boolean;
  size_mb: number;
  downloading: boolean;
}
interface SenseSwitches {
  cloud_enabled: boolean;
  audio_egress: boolean;
  image_egress: boolean;
  budget_monthly_cny: number;
}
interface SenseOverview {
  groups: SenseGroupView[];
  switches: SenseSwitches;
  packs: SensePackView[];
  models_dir: string;
}
interface FableRootView {
  path: string;
  files: number;
  bytes: number;
  scanned_at: number;
}
interface FableStatus {
  db_path: string;
  roots: FableRootView[];
  files_total: number;
  text_files: number;
  chunks_total: number;
  embedded_files: number;
  pending_files: number;
  scanning: boolean;
  indexing: boolean;
  embed_provider: string | null;
  cli_path: string | null;
}
interface FableHit {
  path: string;
  abspath: string;
  location: string;
  snippet: string;
  score: number;
  lanes: string[];
}
interface FableSearchResult {
  query: string;
  mode: string;
  hits: FableHit[];
  grep_hits: number;
  vector_hits: number;
  reranked: boolean;
  grep_truncated: boolean;
  ms: number;
}
interface DreamLog {
  ts: number;
  day: string;
  episodes: number;
  summary: string;
}
interface EchoStatus {
  enabled: boolean;
  hour: number;
  last_dream_day: string;
  dreaming: boolean;
  memory_count: number;
  log: DreamLog[];
}

const overview = ref<SenseOverview | null>(null);
const echo = ref<EchoStatus | null>(null);
const loadErr = ref("");

// 包下载进度:pack_id → {pct, file, msg}
const packProgress = ref<Record<string, { pct: number; file: string; msg: string }>>({});
// 做梦实时流(最近若干行)
const dreamLines = ref<string[]>([]);
const dreamErr = ref("");

// 配置弹窗
const editing = ref<SenseProviderView | null>(null);
const draftKey = ref("");
const draftBase = ref("");
const showKey = ref(false);
const testBusy = ref(false);
const testResult = ref<{ ok: boolean; latency_ms: number; message: string } | null>(null);
const saveBusy = ref(false);

const unlisteners: Array<() => void> = [];

async function refresh() {
  try {
    overview.value = await invoke<SenseOverview>("sense_list");
    echo.value = await invoke<EchoStatus>("echo_status");
    fable.value = await invoke<FableStatus>("fable_status");
    loadErr.value = "";
  } catch (e) {
    loadErr.value = String(e);
  }
  loadClusterModel();
  loadLocalEmbed();
}

// ── AI 归类模型(可选,独立于对话供应商;不配=用主对话 API)──
interface ClusterModelView {
  enabled: boolean;
  baseUrl: string;
  model: string;
  keySet: boolean;
}
const cm = ref<ClusterModelView | null>(null);
const cmForm = ref({ enabled: false, baseUrl: "", model: "", apiKey: "" });
const cmSaving = ref(false);
const cmMsg = ref("");
async function loadClusterModel() {
  try {
    const c = await invoke<ClusterModelView>("file_cluster_model_get");
    cm.value = c;
    cmForm.value = {
      enabled: c.enabled,
      baseUrl: c.baseUrl || "https://api.siliconflow.cn",
      model: c.model || "Qwen/Qwen2.5-7B-Instruct",
      apiKey: "",
    };
  } catch {
    /* 浏览器/降级模式忽略 */
  }
}
async function saveClusterModel() {
  if (cmSaving.value) return;
  cmSaving.value = true;
  cmMsg.value = "";
  try {
    const c = await invoke<ClusterModelView>("file_cluster_model_set", {
      enabled: cmForm.value.enabled,
      baseUrl: cmForm.value.baseUrl,
      model: cmForm.value.model,
      apiKey: cmForm.value.apiKey,
    });
    cm.value = c;
    cmForm.value.apiKey = "";
    cmMsg.value = c.enabled
      ? c.keySet
        ? `已启用独立归类模型:${c.model}`
        : "已开启,但还没填 key —— 填上才会生效"
      : "已关闭,AI 归类沿用你的主对话 API";
  } catch (e: any) {
    cmMsg.value = `保存失败:${e?.message ?? e}`;
  } finally {
    cmSaving.value = false;
  }
}

// ── 检索枢纽(神经层)──
const fable = ref<FableStatus | null>(null);
const fableErr = ref("");
const scanRoot = ref("");
// 进度行读全局 store —— 切走再回来仍能立刻看到当前进度(本页不再各自监听 fable 事件)。
const scanLine = computed(() => tasks.detail.inventory);
const indexLine = computed(() => tasks.detail.index);
const searchQ = ref("");
const searchBusy = ref(false);
const searchRes = ref<FableSearchResult | null>(null);

function fmtBytes(n: number): string {
  if (n >= 1e12) return (n / 1e12).toFixed(2) + " TB";
  if (n >= 1e9) return (n / 1e9).toFixed(2) + " GB";
  if (n >= 1e6) return (n / 1e6).toFixed(1) + " MB";
  return (n / 1e3).toFixed(0) + " KB";
}

async function startInventory() {
  fableErr.value = "";
  try {
    // 托管给全局任务 store:后台跑 + 全局任务中心可见 + 切界面不丢进度。
    await tasks.startInventory(scanRoot.value.trim() ? [scanRoot.value.trim()] : [], []);
    if (fable.value) fable.value = { ...fable.value, scanning: true };
  } catch (e) {
    fableErr.value = String(e);
  }
}

async function startIndex() {
  fableErr.value = "";
  try {
    // 托管给全局任务 store:后台跑 + 全局任务中心可见 + 切界面不丢进度。
    await tasks.startIndex();
    if (fable.value) fable.value = { ...fable.value, indexing: true };
  } catch (e) {
    fableErr.value = String(e);
  }
}

async function cancelFable() {
  try {
    await invoke("fable_cancel");
  } catch (e) {
    fableErr.value = String(e);
  }
}

async function runSearch(ai = false) {
  const q = searchQ.value.trim();
  if (!q || searchBusy.value) return;
  searchBusy.value = true;
  fableErr.value = "";
  try {
    searchRes.value = ai
      ? await invoke<FableSearchResult>("fable_search_ai", { query: q })
      : await invoke<FableSearchResult>("fable_search", { query: q });
  } catch (e) {
    fableErr.value = String(e);
  } finally {
    searchBusy.value = false;
  }
}

// ── 本地嵌入引擎(提速:绕开云 API 限速,治「35/秒、61.9 万要 6-28h」)──
interface LocalEmbedStatus {
  compiled: boolean;
  ready: boolean;
  enabled: boolean;
  dir: string;
}
const localEmbed = ref<LocalEmbedStatus | null>(null);
const leMsg = ref("");
const leBusy = ref(false);
async function loadLocalEmbed() {
  try {
    localEmbed.value = await invoke<LocalEmbedStatus>("fable_local_embed_status");
  } catch {
    /* 浏览器/降级模式忽略 */
  }
}
async function downloadLocalEmbed() {
  if (leBusy.value) return;
  leBusy.value = true;
  leMsg.value = "正在准备下载…";
  try {
    await invoke("fable_local_embed_download");
  } catch (e: any) {
    leMsg.value = `下载失败:${e?.message ?? e}`;
    leBusy.value = false;
  }
}
async function toggleLocalEmbed(on: boolean) {
  try {
    localEmbed.value = await invoke<LocalEmbedStatus>("fable_local_embed_set_enabled", { on });
    leMsg.value = on
      ? "已启用本地嵌入:之后的检索/建索引走本地,不再受云限速"
      : "已切回云 API 嵌入";
  } catch (e: any) {
    leMsg.value = `${e?.message ?? e}`;
    void loadLocalEmbed();
  }
}

onMounted(async () => {
  await refresh();
  unlisteners.push(
    await listen<{ id: string; kind: string; file?: string; pct?: number; message?: string }>(
      "sense:pack",
      (p) => {
        if (p.kind === "progress") {
          packProgress.value = {
            ...packProgress.value,
            [p.id]: { pct: p.pct ?? 0, file: p.file ?? "", msg: "" },
          };
        } else if (p.kind === "phase") {
          packProgress.value = {
            ...packProgress.value,
            [p.id]: {
              pct: packProgress.value[p.id]?.pct ?? 0,
              file: p.file ?? "",
              msg: p.message ?? "",
            },
          };
        } else if (p.kind === "done" || p.kind === "error") {
          const next = { ...packProgress.value };
          delete next[p.id];
          packProgress.value = next;
          if (p.kind === "error") loadErr.value = `感官包下载失败:${p.message ?? ""}`;
          void refresh();
        }
      }
    )
  );
  // 盘点 / 建索引的进度与监听已托管全局 fileTasks store(App 级常驻)。
  // 这里只在它们「完成」时刷新一次本页 FableStatus(家底数字)。
  watch(
    () => [tasks.doneTick.inventory, tasks.doneTick.index],
    () => void refresh(),
  );
  unlisteners.push(
    await listen<{ kind: string; message?: string }>("fable:localembed", (p) => {
      if (p.kind === "phase") {
        leMsg.value = p.message ?? "下载中…";
      } else if (p.kind === "done") {
        leMsg.value = p.message ?? "本地模型已就位";
        leBusy.value = false;
        void loadLocalEmbed();
      } else if (p.kind === "error") {
        leMsg.value = `下载失败:${p.message ?? ""}`;
        leBusy.value = false;
      }
    })
  );
  unlisteners.push(
    await listen<{ kind: string; text?: string; episodes?: number }>("echo:dream", (p) => {
      if (p.kind === "phase" || p.kind === "delta") {
        if (p.kind === "phase" && p.text) {
          dreamLines.value = [...dreamLines.value.slice(-40), p.text];
        }
      } else if (p.kind === "done") {
        dreamLines.value = [...dreamLines.value.slice(-40), `✓ ${p.text ?? "完成"}`];
        void refresh();
      } else if (p.kind === "error") {
        dreamErr.value = p.text ?? "做梦失败";
        void refresh();
      }
    })
  );
});

onUnmounted(() => {
  for (const u of unlisteners) u();
});

function openConfig(p: SenseProviderView) {
  editing.value = p;
  draftKey.value = "";
  draftBase.value = p.base_url;
  showKey.value = false;
  testResult.value = null;
}

async function saveConfig() {
  if (!editing.value || saveBusy.value) return;
  saveBusy.value = true;
  try {
    const args: Record<string, unknown> = { id: editing.value.id };
    if (draftKey.value.trim() !== "") args.apiKey = draftKey.value.trim();
    if (draftBase.value.trim() !== editing.value.base_url) args.baseUrl = draftBase.value.trim();
    overview.value = await invoke<SenseOverview>("sense_set", args);
    const updated = findProvider(editing.value.id);
    if (updated) editing.value = updated;
    draftKey.value = "";
  } catch (e) {
    loadErr.value = String(e);
  } finally {
    saveBusy.value = false;
  }
}

async function toggleEnabled(p: SenseProviderView, on: boolean) {
  try {
    overview.value = await invoke<SenseOverview>("sense_set", { id: p.id, enabled: on });
    if (editing.value?.id === p.id) {
      const updated = findProvider(p.id);
      if (updated) editing.value = updated;
    }
  } catch (e) {
    loadErr.value = String(e);
  }
}

async function setDefaultModel(p: SenseProviderView, modelId: string) {
  try {
    overview.value = await invoke<SenseOverview>("sense_set", { id: p.id, defaultModel: modelId });
    const updated = findProvider(p.id);
    if (updated && editing.value?.id === p.id) editing.value = updated;
  } catch (e) {
    loadErr.value = String(e);
  }
}

function findProvider(id: string): SenseProviderView | null {
  for (const g of overview.value?.groups ?? []) {
    const hit = g.providers.find((p) => p.id === id);
    if (hit) return hit;
  }
  return null;
}

async function runTest() {
  if (!editing.value || testBusy.value) return;
  // 未保存的 key 先落盘再测,贴近用户预期
  if (draftKey.value.trim() !== "") await saveConfig();
  testBusy.value = true;
  testResult.value = null;
  try {
    testResult.value = await invoke<{ ok: boolean; latency_ms: number; message: string }>(
      "sense_test",
      { id: editing.value.id }
    );
  } catch (e) {
    testResult.value = { ok: false, latency_ms: 0, message: String(e) };
  } finally {
    testBusy.value = false;
  }
}

async function installPack(packId: string) {
  try {
    packProgress.value = { ...packProgress.value, [packId]: { pct: 0, file: "", msg: "准备下载…" } };
    await invoke("sense_pack_install", { id: packId });
  } catch (e) {
    const next = { ...packProgress.value };
    delete next[packId];
    packProgress.value = next;
    loadErr.value = String(e);
  }
}

async function removePack(packId: string) {
  try {
    await invoke("sense_pack_remove", { id: packId });
    await refresh();
  } catch (e) {
    loadErr.value = String(e);
  }
}

function packOf(p: SenseProviderView): SensePackView | null {
  if (!p.pack_id) return null;
  return overview.value?.packs.find((k) => k.id === p.pack_id) ?? null;
}

// 用统一的 openUrl(../tauri):桌面走系统浏览器,Docker/Web 走 window.open(在用户浏览器开),
// 不再自己 invoke("open_url") —— 那在 Docker 里是让无头容器去开链接,用户这边什么都不会发生。

async function setSwitch(key: "cloudEnabled" | "audioEgress" | "imageEgress", v: boolean) {
  try {
    overview.value = await invoke<SenseOverview>("sense_switches_set", { [key]: v });
  } catch (e) {
    loadErr.value = String(e);
  }
}

// ── 回声层 ──
const dreamHours = Array.from({ length: 24 }, (_, i) => i);

async function setEcho(enabled?: boolean, hour?: number) {
  try {
    const args: Record<string, unknown> = {};
    if (enabled !== undefined) args.enabled = enabled;
    if (hour !== undefined) args.hour = hour;
    echo.value = await invoke<EchoStatus>("echo_set", args);
  } catch (e) {
    loadErr.value = String(e);
  }
}

async function dreamNow() {
  dreamErr.value = "";
  dreamLines.value = [];
  try {
    await invoke("echo_dream_now");
    if (echo.value) echo.value = { ...echo.value, dreaming: true };
  } catch (e) {
    dreamErr.value = String(e);
  }
}

const switches = computed(() => overview.value?.switches ?? null);

function statusDot(p: SenseProviderView): string {
  if (p.kind === "local") return p.installed ? "on" : "off";
  return p.enabled && p.key_ready ? "on" : p.enabled ? "warn" : "off";
}
</script>

<template>
  <div class="sense">
    <header class="head">
      <div>
        <h1>寓言计划 · 感官 API</h1>
        <p class="sub">
          听 / 看 / 嵌入 / 重排 / 读 —— 本地模型按需下载,云服务商填 key 即用。原文件永不出域,出域的只有采样件。
        </p>
      </div>
      <button class="btn" @click="app.setView('settings')">← 返回设置</button>
    </header>

    <div v-if="loadErr" class="err-line">{{ loadErr }}</div>

    <!-- 隐私三开关 -->
    <section v-if="switches" class="switches">
      <label class="sw">
        <input
          type="checkbox"
          :checked="switches.cloud_enabled"
          @change="setSwitch('cloudEnabled', ($event.target as HTMLInputElement).checked)"
        />
        云感官总闸
      </label>
      <label class="sw">
        <input
          type="checkbox"
          :checked="switches.audio_egress"
          @change="setSwitch('audioEgress', ($event.target as HTMLInputElement).checked)"
        />
        音频出域 <span class="sw-note">(转写已全本地化,建议保持关闭)</span>
      </label>
      <label class="sw">
        <input
          type="checkbox"
          :checked="switches.image_egress"
          @change="setSwitch('imageEgress', ($event.target as HTMLInputElement).checked)"
        />
        缩略图出域 <span class="sw-note">(看图需要)</span>
      </label>
      <span class="models-dir" :title="overview?.models_dir">模型目录:{{ overview?.models_dir }}</span>
    </section>

    <!-- 分组卡片墙 -->
    <section v-for="g in overview?.groups ?? []" :key="g.sense" class="group">
      <div class="g-head">
        <h2>{{ g.label }}</h2>
        <span class="g-desc">{{ g.desc }}</span>
      </div>
      <div class="cards">
        <div
          v-for="p in g.providers"
          :key="p.id"
          class="card"
          :class="{ dim: p.optional && !p.enabled && !p.key_ready }"
        >
          <div class="c-top">
            <div class="c-name">
              <span class="nm">{{ p.name }}</span>
              <span v-if="p.kind === 'local'" class="badge local">本地</span>
              <span v-if="p.free" class="badge free">免费</span>
              <span v-if="p.recommended" class="badge rec">推荐</span>
              <span v-if="p.optional" class="badge opt">选填</span>
              <span
                v-if="p.pack_id && packProgress[p.pack_id]"
                class="badge dl"
              >下载中 {{ packProgress[p.pack_id].pct }}%</span>
            </div>
            <span class="dot" :class="statusDot(p)"></span>
          </div>
          <div class="c-note">{{ p.note }}</div>

          <!-- 本地模型卡:下载/进度/删除 -->
          <template v-if="p.kind === 'local'">
            <div v-if="p.pack_id && packProgress[p.pack_id]" class="prog">
              <div class="prog-bar">
                <div class="prog-fill" :style="{ width: packProgress[p.pack_id].pct + '%' }"></div>
              </div>
              <span class="prog-txt">
                {{ packProgress[p.pack_id].msg || packProgress[p.pack_id].file }}
                {{ packProgress[p.pack_id].pct }}%
              </span>
            </div>
            <div v-else class="c-bottom">
              <template v-if="p.installed">
                <span class="ok-txt">✓ 已就位({{ (packOf(p)?.size_mb ?? 0).toFixed(0) }} MB)</span>
                <button class="link-danger" @click="p.pack_id && removePack(p.pack_id)">删除</button>
              </template>
              <template v-else>
                <span class="muted-txt">未下载(约 {{ (packOf(p)?.approx_mb ?? 0).toFixed(0) }} MB)</span>
                <button class="btn sm primary" @click="p.pack_id && installPack(p.pack_id)">下载</button>
              </template>
            </div>
          </template>

          <!-- 云服务商卡:点击配置 -->
          <template v-else>
            <button class="c-config" @click="openConfig(p)">
              <span>{{ p.key_ready ? (p.key_source || "已配置") : "点击配置" }}</span>
              <span class="chev">›</span>
            </button>
          </template>
        </div>
      </div>
    </section>

    <!-- 本地嵌入引擎(提速:绕开云 API 限速) -->
    <section v-if="localEmbed" class="group">
      <div class="g-head">
        <h2>嵌入 · 本地引擎(提速)</h2>
        <span class="g-desc">下载本地 BGE-M3 后,建索引/检索不走云、不受限速 —— 治「35/秒、几十万份要十几小时」</span>
      </div>
      <div class="echo-card">
        <!-- 本构建未编入引擎(如带语音的 Windows 桌面版):如实说明 + 指路 -->
        <template v-if="!localEmbed.compiled">
          <div class="le-row">
            <span class="badge local">本地</span>
            <span class="muted-txt">
              此版本未内置本地嵌入引擎(桌面语音引擎与它互斥)。本地提速请用
              <b>Docker / NAS 版</b>(那里这颗按钮即下即用),或用 <code>--features local-embed</code> 构建的桌面版。
            </span>
          </div>
        </template>
        <!-- 已编入:下载 / 启用 -->
        <template v-else>
          <div class="le-row">
            <span class="le-stat">
              引擎:<b class="ok-txt">已内置</b>
              · 模型:<b :class="localEmbed.ready ? 'ok-txt' : 'muted-txt'">{{ localEmbed.ready ? "已就位" : "未下载(约 1.2GB)" }}</b>
              · 状态:<b :class="localEmbed.enabled ? 'ok-txt' : 'muted-txt'">{{ localEmbed.enabled ? "已启用(走本地)" : "未启用(走云)" }}</b>
            </span>
          </div>
          <div class="le-row" style="margin-top: 10px">
            <button
              v-if="!localEmbed.ready"
              class="btn sm primary"
              :disabled="leBusy"
              @click="downloadLocalEmbed"
            >
              {{ leBusy ? "下载中…" : "下载本地引擎" }}
            </button>
            <label v-if="localEmbed.ready" class="sw">
              <input
                type="checkbox"
                :checked="localEmbed.enabled"
                @change="toggleLocalEmbed(($event.target as HTMLInputElement).checked)"
              />
              启用本地嵌入(重启仍生效)
            </label>
            <span class="muted-txt" v-if="localEmbed.dir" :title="localEmbed.dir">模型目录:{{ localEmbed.dir }}</span>
          </div>
        </template>
        <div v-if="leMsg" class="dream-line" style="margin-top: 8px">{{ leMsg }}</div>
      </div>
    </section>

    <!-- 检索枢纽 · 神经层 -->
    <section v-if="fable" class="group">
      <div class="g-head">
        <h2>神经 · 检索枢纽</h2>
        <span class="g-desc">全盘盘点 + 向量索引;对话里的 AI 会把 grep 与语义检索并行编排取证</span>
      </div>
      <div class="echo-card">
        <div class="fable-stats">
          <span class="fb-stat"><b>{{ fable.files_total.toLocaleString() }}</b> 文件已盘点</span>
          <span class="fb-stat"><b>{{ fable.text_files.toLocaleString() }}</b> 文本文件</span>
          <span class="fb-stat"><b>{{ fable.chunks_total.toLocaleString() }}</b> 向量 chunk</span>
          <span class="fb-stat" v-if="fable.pending_files > 0">
            <b>{{ fable.pending_files.toLocaleString() }}</b> 待嵌入
          </span>
          <span class="muted-txt">
            嵌入:{{ fable.embed_provider ?? "未配置(去上面给硅基流动填 key)" }}
            · CLI:{{ fable.cli_path ? "已就位" : "未安装(对话仍可用内置工具检索)" }}
          </span>
        </div>
        <div v-if="fable.roots.length" class="fable-roots">
          <div v-for="r in fable.roots" :key="r.path" class="hist-line">
            <span class="hist-day">{{ fmtBytes(r.bytes) }}</span>{{ r.path }}({{ r.files.toLocaleString() }} 文件)
          </div>
        </div>
        <div class="echo-row" style="margin-top: 10px">
          <input
            v-model="scanRoot"
            class="fable-in"
            placeholder="盘点根目录(留空 = 知识库根;可填 NAS 数据盘路径)"
          />
          <button class="btn sm primary" :disabled="fable.scanning" @click="startInventory">
            {{ fable.scanning ? "盘点中…" : "开始盘点" }}
          </button>
          <button class="btn sm" :disabled="fable.indexing || fable.pending_files === 0" @click="startIndex">
            {{ fable.indexing ? "构建中…" : "构建向量索引" }}
          </button>
          <button v-if="fable.scanning || fable.indexing" class="btn sm" @click="cancelFable">取消</button>
        </div>
        <div v-if="scanLine" class="dream-line">{{ scanLine }}</div>
        <div v-if="indexLine" class="dream-line">{{ indexLine }}</div>
        <div v-if="fableErr" class="err-line">{{ fableErr }}</div>

        <div class="echo-row" style="margin-top: 10px">
          <input
            v-model="searchQ"
            class="fable-in"
            placeholder="测一下混合检索(grep ∥ 向量并行)…"
            @keydown.enter="runSearch()"
          />
          <button class="btn sm" :disabled="searchBusy || !searchQ.trim()" @click="runSearch()">
            {{ searchBusy ? "检索中…" : "检索" }}
          </button>
          <button
            class="btn sm"
            :disabled="searchBusy || !searchQ.trim()"
            title="AI 深度检索:让 AI 把查询多路扩写后并行召回再融合,提升模糊/关键词查询的召回与精度(数秒级)"
            @click="runSearch(true)"
          >
            {{ searchBusy ? "…" : "✨ AI 深度" }}
          </button>
        </div>
        <div v-if="searchRes" class="fable-results">
          <div class="muted-txt" style="margin-bottom: 6px">
            {{ searchRes.ms }}ms · grep {{ searchRes.grep_hits }} 命中 · 向量 {{ searchRes.vector_hits }} 命中
            <template v-if="searchRes.reranked"> · 已重排</template>
            <template v-if="searchRes.mode && searchRes.mode.startsWith('ai')"> · {{ searchRes.mode }}</template>
            <template v-if="searchRes.grep_truncated"> · grep 预算截断</template>
          </div>
          <div v-for="(h, i) in searchRes.hits" :key="i" class="fable-hit">
            <div class="fb-path">
              {{ h.path }} <span class="hist-day">{{ h.location }}</span>
              <span v-for="l in h.lanes" :key="l" class="badge" :class="l === 'vector' ? 'local' : 'free'">{{
                l === "vector" ? "语义" : "grep"
              }}</span>
            </div>
            <div class="fb-snippet">{{ h.snippet }}</div>
          </div>
          <div v-if="!searchRes.hits.length" class="muted-txt">没有命中(向量索引未建时只有 grep 车道)</div>
        </div>
      </div>
    </section>

    <!-- 归类 · AI 归类模型(文件中心「AI 归类」用) -->
    <section class="group">
      <div class="g-head">
        <h2>归类 · AI 归类模型</h2>
        <span class="g-desc">
          文件中心「AI 归类」用哪个模型。<b>默认用你的主对话 API</b>(开箱即用);也可在此另配一个便宜/免费模型(如硅基免费对话模型),归类就不烧对话 API 的钱
        </span>
      </div>
      <div class="echo-card">
        <label class="sw">
          <input v-model="cmForm.enabled" type="checkbox" />
          启用独立归类模型(不勾 = 默认用主对话 API)
        </label>
        <div class="cm-grid" :class="{ dim: !cmForm.enabled }">
          <label class="cm-field">
            <span>接口地址(OpenAI 兼容 /v1/chat/completions)</span>
            <input v-model="cmForm.baseUrl" :disabled="!cmForm.enabled" class="fable-in" placeholder="https://api.siliconflow.cn" />
          </label>
          <label class="cm-field">
            <span>模型名</span>
            <input v-model="cmForm.model" :disabled="!cmForm.enabled" class="fable-in" placeholder="Qwen/Qwen2.5-7B-Instruct" />
          </label>
          <label class="cm-field">
            <span>API Key {{ cm?.keySet ? "(已配置,留空不改)" : "" }}</span>
            <input
              v-model="cmForm.apiKey"
              :disabled="!cmForm.enabled"
              class="fable-in"
              type="password"
              :placeholder="cm?.keySet ? '●●●●●● 已保存' : 'sk-...'"
            />
          </label>
        </div>
        <div class="echo-row" style="margin-top: 10px">
          <button class="btn sm primary" :disabled="cmSaving" @click="saveClusterModel">
            {{ cmSaving ? "保存中…" : "保存" }}
          </button>
          <span v-if="cmMsg" class="muted-txt">{{ cmMsg }}</span>
        </div>
        <div class="muted-txt" style="margin-top: 8px">
          提示:硅基流动的 key 跟上面 BGE-M3 是<b>同一个</b>;填它的免费对话模型(如 Qwen2.5-7B),归类几乎零成本。
        </div>
      </div>
    </section>

    <!-- 回声层 · 每日做梦 -->
    <section v-if="echo" class="group">
      <div class="g-head">
        <h2>回声 · 每日做梦</h2>
        <span class="g-desc">每天定时收录当天对话,蒸馏成长期记忆(memory/ 车道),让它越用越懂你</span>
      </div>
      <div class="echo-card">
        <div class="echo-row">
          <label class="sw">
            <input
              type="checkbox"
              :checked="echo.enabled"
              @change="setEcho(($event.target as HTMLInputElement).checked, undefined)"
            />
            启用每日做梦
          </label>
          <label class="sw">
            做梦时间
            <select
              :value="echo.hour"
              @change="setEcho(undefined, Number(($event.target as HTMLSelectElement).value))"
            >
              <option v-for="h in dreamHours" :key="h" :value="h">{{ String(h).padStart(2, "0") }}:00</option>
            </select>
          </label>
          <span class="muted-txt">
            已沉淀 {{ echo.memory_count }} 条记忆
            <template v-if="echo.last_dream_day">· 上次做梦 {{ echo.last_dream_day }}</template>
          </span>
          <button class="btn sm" :disabled="echo.dreaming" @click="dreamNow">
            {{ echo.dreaming ? "做梦中…" : "现在做一次梦" }}
          </button>
        </div>
        <div v-if="dreamErr" class="err-line">{{ dreamErr }}</div>
        <div v-if="dreamLines.length" class="dream-log">
          <div v-for="(l, i) in dreamLines" :key="i" class="dream-line">{{ l }}</div>
        </div>
        <div v-if="echo.log.length" class="echo-history">
          <div v-for="(l, i) in echo.log.slice(0, 5)" :key="i" class="hist-line">
            <span class="hist-day">{{ l.day }}</span> {{ l.summary }}
          </div>
        </div>
      </div>
    </section>

    <!-- 配置弹窗 -->
    <div v-if="editing" class="modal-mask" @click.self="editing = null">
      <div class="modal">
        <div class="m-head">
          <div>
            <div class="m-title">
              {{ editing.name }} 设置
              <a v-if="editing.docs_url" class="m-doc" @click.prevent="openUrl(editing.docs_url)">接入文档 ↗</a>
            </div>
            <div class="m-sub">{{ editing.note }}</div>
          </div>
          <div class="m-actions">
            <label class="sw">
              <input
                type="checkbox"
                :checked="editing.enabled"
                @change="toggleEnabled(editing, ($event.target as HTMLInputElement).checked)"
              />
              {{ editing.enabled ? "已启用" : "已停用" }}
            </label>
            <button class="x" @click="editing = null">×</button>
          </div>
        </div>

        <div class="m-field">
          <label>API Key</label>
          <div class="key-row">
            <input
              :type="showKey ? 'text' : 'password'"
              v-model="draftKey"
              :placeholder="editing.api_key_masked || '请输入 API Key'"
            />
            <button class="eye" @click="showKey = !showKey">{{ showKey ? "🙈" : "👁" }}</button>
          </div>
          <a v-if="editing.get_key_url" class="get-key" @click.prevent="openUrl(editing.get_key_url)">
            点击这里获取 API Key
          </a>
          <div v-if="editing.key_source" class="key-source">{{ editing.key_source }}</div>
        </div>

        <div class="m-field">
          <label>接口地址</label>
          <input v-model="draftBase" class="base-in" placeholder="https://…" />
        </div>

        <div class="m-field" v-if="editing.models.length">
          <label>模型</label>
          <div v-for="mo in editing.models" :key="mo.id" class="model-row">
            <div class="model-info">
              <span class="model-name">
                {{ mo.name }}
                <span v-if="mo.recommended" class="badge rec">推荐</span>
              </span>
              <span class="model-note">{{ mo.note }}</span>
            </div>
            <button
              class="btn sm"
              :class="{ primary: editing.default_model === mo.id }"
              @click="setDefaultModel(editing, mo.id)"
            >
              {{ editing.default_model === mo.id ? "默认模型" : "设为默认" }}
            </button>
          </div>
        </div>

        <div class="m-foot">
          <div class="test-area">
            <button class="btn" :disabled="testBusy" @click="runTest">
              {{ testBusy ? "测试中…" : "测一下" }}
            </button>
            <span
              v-if="testResult"
              class="test-result"
              :class="{ ok: testResult.ok, bad: !testResult.ok }"
            >
              {{ testResult.ok ? "✓" : "✗" }} {{ testResult.message }}
            </span>
          </div>
          <button class="btn primary" :disabled="saveBusy" @click="saveConfig">
            {{ saveBusy ? "保存中…" : "保存" }}
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.sense {
  flex: 1;
  overflow-y: auto;
  padding: 40px 56px 80px;
  max-width: 1080px;
  margin: 0 auto;
  width: 100%;
}
.head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
  border-bottom: 1px solid var(--hairline);
  padding-bottom: 18px;
  margin-bottom: 24px;
}
.head h1 {
  font-family: var(--serif);
  font-size: 22px;
  font-weight: 500;
  letter-spacing: 2px;
  margin: 0 0 8px;
  color: var(--ink);
}
.head .sub {
  font-size: 12.5px;
  color: var(--muted);
  margin: 0;
  letter-spacing: 0.4px;
}
.err-line {
  color: #c0392b;
  font-size: 12.5px;
  margin: 8px 0;
}

.switches {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 18px;
  background: var(--panel);
  border: 1px solid var(--hairline);
  border-radius: 2px;
  padding: 12px 16px;
  margin-bottom: 26px;
  font-size: 12.5px;
  color: var(--text-2);
}
.sw {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  cursor: pointer;
  white-space: nowrap;
}
.sw select {
  background: var(--panel);
  color: var(--text);
  border: 1px solid var(--border);
  border-radius: 2px;
  padding: 3px 6px;
  font-size: 12px;
}
.sw-note {
  color: var(--dim);
  font-size: 11.5px;
}
.models-dir {
  margin-left: auto;
  color: var(--dim);
  font-size: 11px;
  font-family: var(--mono);
  max-width: 320px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.group {
  margin-bottom: 30px;
}
.g-head {
  display: flex;
  align-items: baseline;
  gap: 12px;
  margin-bottom: 12px;
}
.g-head h2 {
  font-family: var(--serif);
  font-size: 15.5px;
  font-weight: 600;
  letter-spacing: 1.2px;
  color: var(--ink);
  margin: 0;
}
.g-desc {
  font-size: 11.5px;
  color: var(--dim);
}
.cards {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(330px, 1fr));
  gap: 12px;
}
.card {
  background: var(--panel);
  border: 1px solid var(--hairline);
  border-radius: 2px;
  padding: 14px 16px;
  box-shadow: var(--shadow-sm);
}
.card.dim {
  opacity: 0.62;
}
.c-top {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}
.c-name {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}
.nm {
  font-size: 13.5px;
  font-weight: 600;
  color: var(--ink);
}
.badge {
  font-size: 10px;
  padding: 1px 6px;
  border-radius: 2px;
  border: 1px solid var(--border);
  color: var(--text-2);
  letter-spacing: 0.5px;
}
.badge.local {
  color: #7c5cd9;
  border-color: #7c5cd966;
}
.badge.free {
  color: #2e8b57;
  border-color: #2e8b5766;
}
.badge.rec {
  color: #b8860b;
  border-color: #b8860b66;
}
.badge.opt {
  color: var(--dim);
}
.badge.dl {
  color: #2e8b57;
  border-color: #2e8b5766;
}
.dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--border);
  flex: none;
}
.dot.on {
  background: #2e8b57;
}
.dot.warn {
  background: #b8860b;
}
.c-note {
  font-size: 11.5px;
  color: var(--text-2);
  line-height: 1.7;
  margin: 8px 0 10px;
  min-height: 32px;
}
.c-config {
  width: 100%;
  display: flex;
  align-items: center;
  justify-content: space-between;
  background: transparent;
  border: none;
  border-top: 1px solid var(--hairline);
  padding: 9px 0 0;
  color: var(--text-2);
  font-size: 12.5px;
  cursor: pointer;
}
.c-config:hover {
  color: var(--ink);
}
.chev {
  color: var(--dim);
}
.c-bottom {
  display: flex;
  align-items: center;
  justify-content: space-between;
  border-top: 1px solid var(--hairline);
  padding-top: 9px;
}
.ok-txt {
  color: #2e8b57;
  font-size: 12px;
}
.muted-txt {
  color: var(--dim);
  font-size: 12px;
}
.link-danger {
  background: none;
  border: none;
  color: #c0392b;
  font-size: 12px;
  cursor: pointer;
  opacity: 0.75;
}
.link-danger:hover {
  opacity: 1;
}
.prog {
  border-top: 1px solid var(--hairline);
  padding-top: 10px;
}
.prog-bar {
  height: 5px;
  background: var(--bg-soft);
  border-radius: 3px;
  overflow: hidden;
}
.prog-fill {
  height: 100%;
  background: #2e8b57;
  transition: width 0.3s;
}
.prog-txt {
  display: block;
  margin-top: 5px;
  font-size: 11px;
  color: var(--dim);
  font-family: var(--mono);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.btn {
  padding: 7px 14px;
  background: transparent;
  border: 1px solid var(--border);
  border-radius: 2px;
  color: var(--text-2);
  font-size: 12.5px;
  cursor: pointer;
  white-space: nowrap;
}
.btn:hover:not(:disabled) {
  border-color: var(--ink);
  color: var(--ink);
}
.btn:disabled {
  opacity: 0.5;
  cursor: default;
}
.btn.sm {
  padding: 4px 12px;
  font-size: 12px;
}
.btn.primary {
  border-color: var(--primary);
  color: var(--primary);
}

/* 本地嵌入引擎卡 */
.le-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 14px;
  font-size: 12.5px;
  color: var(--text-2);
}
.le-stat {
  font-size: 12.5px;
  color: var(--text-2);
}
.le-stat b {
  margin: 0 2px;
}

/* 回声层 */
.echo-card {
  background: var(--panel);
  border: 1px solid var(--hairline);
  border-radius: 2px;
  padding: 16px 18px;
  box-shadow: var(--shadow-sm);
}
.echo-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 18px;
  font-size: 12.5px;
  color: var(--text-2);
}
.dream-log {
  margin-top: 12px;
  border-top: 1px solid var(--hairline);
  padding-top: 10px;
  max-height: 160px;
  overflow-y: auto;
}
.dream-line {
  font-size: 11.5px;
  color: var(--text-2);
  font-family: var(--mono);
  line-height: 1.8;
}
.echo-history {
  margin-top: 10px;
  border-top: 1px solid var(--hairline);
  padding-top: 8px;
}
.hist-line {
  font-size: 11.5px;
  color: var(--dim);
  line-height: 1.9;
}
.hist-day {
  font-family: var(--mono);
  color: var(--muted);
  margin-right: 8px;
}

/* 检索枢纽 */
.fable-stats {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 16px;
  font-size: 12.5px;
  color: var(--text-2);
}
.fb-stat b {
  color: var(--ink);
  font-size: 14px;
  margin-right: 3px;
}
.fable-roots {
  margin-top: 8px;
  border-top: 1px solid var(--hairline);
  padding-top: 6px;
}
.fable-in {
  flex: 1;
  min-width: 220px;
  padding: 7px 10px;
  border: 1px solid var(--border);
  border-radius: 2px;
  font-size: 12px;
  background: var(--panel);
  color: var(--text);
}
.fable-in:focus {
  outline: none;
  border-color: var(--primary);
}
.cm-grid {
  display: grid;
  grid-template-columns: 1.3fr 1.3fr 1fr;
  gap: 10px;
  margin-top: 10px;
}
.cm-grid.dim {
  opacity: 0.5;
}
@media (max-width: 760px) {
  .cm-grid {
    grid-template-columns: 1fr;
  }
}
.cm-field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 0;
}
.cm-field > span {
  font-size: 11px;
  color: var(--muted);
}
.cm-field .fable-in {
  flex: none;
  min-width: 0;
  width: 100%;
}
.fable-results {
  margin-top: 10px;
  border-top: 1px solid var(--hairline);
  padding-top: 10px;
  max-height: 320px;
  overflow-y: auto;
}
.fable-hit {
  padding: 6px 0;
  border-bottom: 1px dashed var(--hairline);
}
.fb-path {
  font-size: 12px;
  color: var(--ink);
  font-family: var(--mono);
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}
.fb-snippet {
  font-size: 11.5px;
  color: var(--text-2);
  line-height: 1.7;
  margin-top: 2px;
  word-break: break-all;
}

/* 配置弹窗 */
.modal-mask {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.45);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 60;
}
.modal {
  width: min(560px, 92vw);
  max-height: 86vh;
  overflow-y: auto;
  background: var(--panel);
  border: 1px solid var(--hairline);
  border-radius: 4px;
  padding: 22px 24px;
  box-shadow: 0 18px 60px rgba(0, 0, 0, 0.35);
}
.m-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 18px;
}
.m-title {
  font-family: var(--serif);
  font-size: 16px;
  font-weight: 600;
  color: var(--ink);
  display: flex;
  align-items: center;
  gap: 10px;
}
.m-doc {
  font-size: 11.5px;
  color: var(--primary);
  cursor: pointer;
  font-weight: 400;
}
.m-sub {
  font-size: 11.5px;
  color: var(--dim);
  margin-top: 4px;
  line-height: 1.7;
}
.m-actions {
  display: flex;
  align-items: center;
  gap: 12px;
  flex: none;
}
.x {
  background: none;
  border: none;
  font-size: 20px;
  color: var(--dim);
  cursor: pointer;
  line-height: 1;
}
.x:hover {
  color: var(--ink);
}
.m-field {
  margin-bottom: 16px;
}
.m-field > label {
  display: block;
  font-size: 12px;
  color: var(--muted);
  letter-spacing: 1px;
  margin-bottom: 6px;
  font-family: var(--serif);
}
.key-row {
  display: flex;
  gap: 6px;
}
.key-row input,
.base-in {
  flex: 1;
  width: 100%;
  padding: 8px 10px;
  border: 1px solid var(--border);
  border-radius: 2px;
  font-family: var(--mono);
  font-size: 12px;
  background: var(--panel);
  color: var(--text);
}
.key-row input:focus,
.base-in:focus {
  outline: none;
  border-color: var(--primary);
}
.eye {
  background: transparent;
  border: 1px solid var(--border);
  border-radius: 2px;
  cursor: pointer;
  padding: 0 10px;
}
.get-key {
  display: inline-block;
  margin-top: 6px;
  font-size: 12px;
  color: var(--primary);
  cursor: pointer;
}
.key-source {
  margin-top: 5px;
  font-size: 11.5px;
  color: #2e8b57;
}
.model-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  border: 1px solid var(--hairline);
  border-radius: 2px;
  padding: 10px 12px;
  margin-bottom: 8px;
}
.model-info {
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.model-name {
  font-size: 13px;
  color: var(--ink);
  display: flex;
  align-items: center;
  gap: 6px;
}
.model-note {
  font-size: 11px;
  color: var(--dim);
}
.m-foot {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  border-top: 1px solid var(--hairline);
  padding-top: 14px;
}
.test-area {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
}
.test-result {
  font-size: 12px;
  overflow: hidden;
  text-overflow: ellipsis;
}
.test-result.ok {
  color: #2e8b57;
}
.test-result.bad {
  color: #c0392b;
}
</style>
