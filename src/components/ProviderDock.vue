<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch, nextTick } from "vue";
import ImageProviderPanel from "./ImageProviderPanel.vue";
import {
  Zap,
  ChevronUp,
  X,
  Plus,
  Check,
  RefreshCw,
  Pencil,
  Trash2,
  ExternalLink,
  Search,
  LogIn,
  ShieldCheck,
  BarChart3,
  KeyRound,
  Download,
  Upload,
  Sparkles,
  CircleAlert,
  Star,
  Wallet,
} from "@lucide/vue";
import { useProvidersStore } from "../stores/providers";
import type {
  ProviderView,
  TokenBucket,
  ProviderBalance,
  CodexDeviceLogin,
  ClaudeLoginStart,
} from "../tauri";

const props = defineProps<{ collapsed?: boolean }>();
const store = useProvidersStore();

const open = ref(false);
const filter = ref("");

// Codex 授权 (原生 Device Code OAuth)
const codexOpen = ref(false);
const codexDevice = ref<CodexDeviceLogin | null>(null);
const codexBusy = ref(false);
const codexErr = ref<string | null>(null);
const codexCopied = ref(false);
let codexTimer: number | null = null;
let codexExpireAt = 0;

// Claude 官方订阅授权 (PKCE OAuth · 回环一键为主, 手工回贴兜底)
const claudeOpen = ref(false);
const claudeLogin = ref<ClaudeLoginStart | null>(null);
const claudePasted = ref("");
const claudeBusy = ref(false);
const claudeErr = ref<string | null>(null);
let claudeTimer: number | null = null;
let claudeExpireAt = 0;

// 用量周期
type Period = "today" | "week" | "month" | "year";
const period = ref<Period>("today");
const periods: { key: Period; label: string }[] = [
  { key: "today", label: "今日" },
  { key: "week", label: "近 7 天" },
  { key: "month", label: "近 30 天" },
  { key: "year", label: "近 1 年" },
];

onMounted(() => {
  store.refresh();
  store.refreshUsage();
});

watch(open, (v) => {
  if (v) {
    store.refresh();
    store.refreshUsage();
    store.refreshCodex();
    store.refreshCodexProxy();
    store.refreshClaudeAuth();
    if (store.currentId) store.refreshBalance(store.currentId);
    nextTick(() => window.addEventListener("keydown", onEsc));
  } else {
    codexOpen.value = false;
    claudeOpen.value = false;
    resetCodexAuth();
    resetClaudeAuth();
    window.removeEventListener("keydown", onEsc);
  }
});
// 切换供应商后,自动拉取新当前供应商的额度(面板开着时才查,省请求)
watch(
  () => store.currentId,
  (id) => {
    if (open.value && id && !store.balances[id]) store.refreshBalance(id);
  }
);
onBeforeUnmount(() => {
  window.removeEventListener("keydown", onEsc);
  stopCodexPoll();
  stopClaudePoll();
});
function onEsc(e: KeyboardEvent) {
  if (e.key !== "Escape") return;
  if (claudeOpen.value) claudeOpen.value = false;
  else if (codexOpen.value) codexOpen.value = false;
  else open.value = false;
}

/** 联动/隔离切换;后端会顺带把全局 settings.json 写入当前供应商(开)或清回官方(关) */
async function toggleLink() {
  await store.setLinkMode(!store.linkGlobal);
  await store.refresh();
}

function fmt(n: number): string {
  if (n >= 1e9) return (n / 1e9).toFixed(2) + "B";
  if (n >= 1e6) return (n / 1e6).toFixed(2) + "M";
  if (n >= 1e4) return (n / 1e3).toFixed(0) + "K";
  if (n >= 1e3) return (n / 1e3).toFixed(1) + "K";
  return String(n);
}
function fmtCost(n: number): string {
  return "$" + n.toFixed(4);
}
function hostOf(url: string): string {
  if (!url) return "本地 / 订阅";
  try {
    return new URL(url).host;
  } catch {
    return url.replace(/^https?:\/\//, "");
  }
}

/** 当前供应商 + 顺手把任何已配过 key 的也提到常用前几位 (仿 WeSight 全量平铺,
 *  但加"常用"做轻分组;新用户一打开就知道哪些是开箱可用的) */
const recentList = computed(() => {
  const seen = new Set<string>();
  const out: ProviderView[] = [];
  // 1) 当前
  if (store.currentId) {
    const c = store.providers.find((p) => p.id === store.currentId);
    if (c) { out.push(c); seen.add(c.id); }
  }
  // 2) 已配过 key 的, 顺序按 providers 原序
  for (const p of store.providers) {
    if (out.length >= 5) break;
    if (seen.has(p.id)) continue;
    if (p.hasKey) { out.push(p); seen.add(p.id); }
  }
  // 3) codex 兜底(强调入口), 不重复加
  if (!seen.has("codex")) {
    const c = store.providers.find((p) => p.id === "codex");
    if (c) { out.push(c); seen.add(c.id); }
  }
  return out;
});

const restList = computed(() => {
  const ids = new Set(recentList.value.map((p) => p.id));
  return store.providers.filter((p) => !ids.has(p.id));
});

const filtered = computed(() => {
  const q = filter.value.trim().toLowerCase();
  if (!q) return store.providers;
  return store.providers.filter(
    (p) =>
      p.name.toLowerCase().includes(q) ||
      hostOf(p.baseUrl).toLowerCase().includes(q) ||
      p.id.toLowerCase().includes(q)
  );
});

const current = computed(() => store.current);
const todayTotal = computed(() => store.usage?.today.total ?? 0);
const currentBalance = computed<ProviderBalance | null>(
  () => store.balances[store.currentId] ?? null
);
/** 额度结果 → 数字颜色类 */
function balClass(b?: ProviderBalance | null): string {
  if (!b) return "muted";
  if (b.kind === "balance") return "ok";
  if (b.kind === "alive") return "alive";
  if (b.kind === "error") return "err";
  return "muted";
}
/** 仅 key/official 类供应商有「额度」概念(codex/copilot 走授权,无额度数字) */
const showBalance = computed(
  () => !!current.value && current.value.kind !== "codex" && current.value.kind !== "copilot"
);
const currentModel = computed(() => {
  const c = current.value;
  if (!c) return "";
  const env = c.settingsConfig?.env ?? {};
  return (
    env.ANTHROPIC_MODEL ||
    env.ANTHROPIC_DEFAULT_SONNET_MODEL ||
    env.ANTHROPIC_DEFAULT_HAIKU_MODEL ||
    ""
  );
});

function bucketOf(p: Period): TokenBucket | null {
  return store.usage ? store.usage[p] : null;
}
const activeBucket = computed(() => bucketOf(period.value));

async function onRowClick(p: ProviderView) {
  if (p.kind === "codex") {
    resetCodexAuth();
    await store.refreshCodex();
    store.refreshCodexProxy();
    if (store.codex?.loggedIn && p.id !== store.currentId) {
      await store.switchTo("codex");
    } else {
      // ★ 未授权也直接弹,不再吞掉点击
      codexOpen.value = true;
    }
    return;
  }
  if (p.kind === "copilot") {
    store.openAdd(p);
    open.value = false;
    return;
  }
  if (p.id === store.currentId) return;
  if (!p.hasKey) {
    store.openAdd(p);
    open.value = false;
    return;
  }
  await store.switchTo(p.id);
}

function editProvider(p: ProviderView) {
  store.openAdd(p);
  open.value = false;
}
function addCustom() {
  store.openAdd(null);
  open.value = false;
}
function openBoard() {
  store.openUsage();
  open.value = false;
}
function importExport() {
  // 占位:先给轻提示;下版接真 UI
  alert(
    "导入/导出:把 ~/.claude/settings.json 拖入或复制 env 块即可。\n" +
    "下一版提供完整 UI(目前供应商增删改已覆盖大部分场景)。"
  );
}
async function removeProvider(p: ProviderView) {
  const verb = p.isPreset ? "清除配置" : "删除";
  if (!confirm(`${verb}「${p.name}」?`)) return;
  await store.remove(p.id);
}
function openSite(url: string) {
  if (url) window.open(url, "_blank");
}

// ── Codex 授权 ─────────────────────────
async function startCodexAuth() {
  codexErr.value = null;
  codexCopied.value = false;
  codexBusy.value = true;
  const dev = await store.codexStartLogin();
  codexBusy.value = false;
  if (!dev) {
    codexErr.value = store.error || "发起授权失败";
    return;
  }
  codexDevice.value = dev;
  codexExpireAt = Date.now() + dev.expiresIn * 1000;
  // auto = 回环一键授权(浏览器点 Authorize 即完成); device = 设备码流程兜底
  if (dev.mode === "auto") startCodexAutoPoll();
  else startCodexPoll(dev);
}
function startCodexPoll(dev: CodexDeviceLogin) {
  stopCodexPoll();
  const intervalMs = Math.max(2, dev.interval) * 1000;
  codexTimer = window.setInterval(async () => {
    if (Date.now() > codexExpireAt) {
      stopCodexPoll();
      codexDevice.value = null;
      codexErr.value = "授权超时, 请重试";
      return;
    }
    try {
      const st = await store.codexPollLogin(dev.deviceCode, dev.userCode);
      if (st === "ok") {
        stopCodexPoll();
        codexDevice.value = null;
        await store.refreshCodex();
      }
    } catch (e) {
      stopCodexPoll();
      codexDevice.value = null;
      codexErr.value = String(e);
    }
  }, intervalMs);
}
/** auto 模式:轮询后端回环会话状态,授权码由浏览器重定向自动送达后端 */
function startCodexAutoPoll() {
  stopCodexPoll();
  codexTimer = window.setInterval(async () => {
    if (Date.now() > codexExpireAt) {
      resetCodexAuth();
      codexErr.value = "授权超时, 请重试";
      return;
    }
    try {
      const r = await store.codexLoginPoll();
      if (r.status === "pending") return;
      stopCodexPoll();
      codexDevice.value = null;
      if (r.status !== "ok") {
        codexErr.value = r.message || "授权未完成, 请重试";
      }
    } catch (e) {
      stopCodexPoll();
      codexDevice.value = null;
      codexErr.value = String(e);
    }
  }, 1500);
}
function stopCodexPoll() {
  if (codexTimer !== null) {
    clearInterval(codexTimer);
    codexTimer = null;
  }
}
function resetCodexAuth() {
  stopCodexPoll();
  if (codexDevice.value?.mode === "auto") store.codexLoginCancel();
  codexDevice.value = null;
  codexBusy.value = false;
  codexErr.value = null;
  codexCopied.value = false;
}
function openCodexVerify() {
  if (codexDevice.value) window.open(codexDevice.value.verificationUri, "_blank");
}
async function routeCodex() {
  codexErr.value = null;
  const ok = await store.switchTo("codex");
  await store.refreshCodexProxy();
  if (ok) codexOpen.value = false;
  else codexErr.value = store.error || "切换失败";
}
async function copyUserCode() {
  if (!codexDevice.value) return;
  try {
    await navigator.clipboard.writeText(codexDevice.value.userCode);
    codexCopied.value = true;
    setTimeout(() => (codexCopied.value = false), 1500);
  } catch {
    /* 剪贴板不可用时忽略 */
  }
}

// ── Claude 官方订阅授权 (回环一键 · 手工回贴兜底) ─────────────────────────
/** 点「授权」:桌面端默认回环一键(开浏览器点 Authorize 即完成);
 *  forceManual=true 强制手工回贴(回环失灵时的兜底入口) */
async function startClaudeAuth(forceManual = false) {
  stopClaudePoll();
  if (forceManual) store.claudeLoginCancel(); // 改走手工, 顺手释放 54545 监听
  claudeErr.value = null;
  claudePasted.value = "";
  claudeBusy.value = true;
  const login = await store.claudeStartLogin(forceManual);
  claudeBusy.value = false;
  if (!login) {
    claudeErr.value = store.error || "发起授权失败";
    return;
  }
  claudeLogin.value = login;
  if (login.mode === "auto") startClaudeAutoPoll();
}
/** auto 模式:轮询后端回环会话状态,授权码由浏览器重定向自动送达后端 */
function startClaudeAutoPoll() {
  stopClaudePoll();
  claudeExpireAt = Date.now() + 10 * 60 * 1000;
  claudeTimer = window.setInterval(async () => {
    if (Date.now() > claudeExpireAt) {
      resetClaudeAuth();
      claudeErr.value = "授权超时, 请重试";
      return;
    }
    try {
      const r = await store.claudeLoginPoll();
      if (r.status === "pending") return;
      stopClaudePoll();
      claudeLogin.value = null;
      if (r.status !== "ok") {
        claudeErr.value = r.message || "授权未完成, 请重试";
      }
    } catch (e) {
      stopClaudePoll();
      claudeLogin.value = null;
      claudeErr.value = String(e);
    }
  }, 1500);
}
function stopClaudePoll() {
  if (claudeTimer !== null) {
    clearInterval(claudeTimer);
    claudeTimer = null;
  }
}
function openClaudeAuthPage() {
  if (claudeLogin.value) window.open(claudeLogin.value.authorizeUrl, "_blank");
}
/** 回贴授权码(可含 #state)→ 换 token 落盘 */
async function submitClaudeCode() {
  if (!claudeLogin.value || !claudePasted.value.trim()) return;
  claudeErr.value = null;
  claudeBusy.value = true;
  try {
    const ok = await store.claudeFinishLogin(
      claudePasted.value,
      claudeLogin.value.verifier,
      claudeLogin.value.state
    );
    if (ok) {
      claudeLogin.value = null;
      claudePasted.value = "";
    } else {
      claudeErr.value = "授权未完成,请确认授权码完整";
    }
  } catch (e) {
    claudeErr.value = String(e);
  } finally {
    claudeBusy.value = false;
  }
}
function resetClaudeAuth() {
  stopClaudePoll();
  if (claudeLogin.value?.mode === "auto") store.claudeLoginCancel();
  claudeLogin.value = null;
  claudePasted.value = "";
  claudeBusy.value = false;
  claudeErr.value = null;
}

// ── 行内辅助:副标题 ─────────────────────────
function subtitleOf(p: ProviderView): string {
  if (p.kind === "codex") {
    return store.codex?.loggedIn ? "ChatGPT · 已授权 · 点即用" : "ChatGPT · 需先授权";
  }
  if (p.kind === "copilot") return "需 OAuth · 代理";
  return hostOf(p.baseUrl);
}
</script>

<template>
  <div class="dock-root">
    <!-- resting 药丸 -->
    <button
      class="pill"
      :class="{ rail: props.collapsed, active: open }"
      :title="current ? `当前: ${current.name}` : 'API 供应商'"
      @click="open = !open"
    >
      <span
        class="dot"
        :style="{
          background: '#2f6fd0',
          boxShadow: '0 0 0 3px #2f6fd029',
        }"
      />
      <template v-if="!props.collapsed">
        <span class="pill-main">
          <span class="pill-name">{{ current?.name || "选择供应商" }}</span>
          <span class="pill-sub">
            <Zap :size="9" :stroke-width="2.4" />
            {{ fmt(todayTotal) }} · 今日
          </span>
        </span>
        <ChevronUp class="chev" :class="{ flip: open }" :size="14" :stroke-width="2" />
      </template>
    </button>

    <Teleport to="body">
      <Transition name="dock-fade">
        <div v-if="open" class="dock-overlay" @click="open = false">
          <div class="panel" @click.stop>
            <div class="panel-accent" />

            <header class="panel-head">
              <div class="head-titles">
                <div class="title">API 供应商</div>
                <div class="subtitle">{{ store.linkGlobal ? "点选即切换 · 联动写入 ~/.claude/settings.json" : "点选即切换 · 仅 Polaris 内生效" }}</div>
              </div>
              <div class="head-actions">
                <button class="icon-btn" title="添加供应商" @click="addCustom">
                  <Plus :size="16" :stroke-width="2" />
                </button>
                <button class="icon-btn" title="关闭" @click="open = false">
                  <X :size="15" :stroke-width="1.8" />
                </button>
              </div>
            </header>

            <div class="panel-body">
              <!-- 搜索条 -->
              <div class="search-row">
                <Search :size="13" :stroke-width="1.8" class="s-ic" />
                <input v-model="filter" class="search-input" placeholder="搜索供应商 / 主机名…" />
                <button v-if="filter" class="icon-btn sm" @click="filter = ''">
                  <X :size="13" :stroke-width="1.8" />
                </button>
              </div>

              <!-- 联动/隔离开关:隔离(默认)只影响 Polaris 自己 spawn 的 claude,
                   联动才写 ~/.claude/settings.json 让终端 CLI 跟着切 -->
              <div class="link-row">
                <div class="link-info">
                  <span class="link-title">联动系统 CLI</span>
                  <span class="link-desc">{{
                    store.linkGlobal
                      ? "切换会写入 ~/.claude/settings.json,终端 claude 跟着变"
                      : "已隔离:配置与会话账本仅 Polaris 自用,终端与监控不受影响"
                  }}</span>
                </div>
                <button
                  class="link-switch"
                  :class="{ on: store.linkGlobal }"
                  role="switch"
                  :aria-checked="store.linkGlobal"
                  :title="store.linkGlobal ? '点击改为隔离(终端恢复官方/原配置)' : '点击开启联动(终端跟随 Polaris 切换)'"
                  @click="toggleLink"
                >
                  <span class="knob" />
                </button>
              </div>

              <!-- ★ 上段:当前供应商状态卡 (放大) -->
              <section v-if="current" class="now-card" :class="{ codex: current.kind === 'codex' }">
                <div class="now-row">
                  <span
                    class="now-dot"
                    :style="{ background: current.color, boxShadow: `0 0 0 3px ${current.color}29` }"
                  />
                  <div class="now-info">
                    <div class="now-name">{{ current.name }}</div>
                    <div class="now-host">
                      <template v-if="current.kind === 'codex'">
                        <span v-if="store.codex?.loggedIn">ChatGPT 已授权</span>
                        <span v-else class="need-auth">⚠ 需先授权 ChatGPT</span>
                      </template>
                      <template v-else-if="current.kind === 'copilot'">
                        GitHub Copilot · 暂未支持
                      </template>
                      <template v-else-if="current.kind === 'official'">
                        <span v-if="store.claudeAuth?.loggedIn">Claude 订阅 · 已登录</span>
                        <span v-else class="need-auth">未登录订阅 · 可用 API Key 或点下方授权</span>
                      </template>
                      <template v-else>
                        {{ hostOf(current.baseUrl) }}<span v-if="currentModel"> · {{ currentModel }}</span>
                      </template>
                    </div>
                  </div>
                  <div class="now-today">
                    <div class="now-num">{{ fmt(todayTotal) }}</div>
                    <div class="now-lab">今日 token</div>
                  </div>
                </div>

                <!-- 套餐额度 / 实时余额(当前供应商,自动查) -->
                <div v-if="showBalance" class="now-balance">
                  <Wallet :size="12" :stroke-width="1.9" class="nb-ic" />
                  <span class="nb-label" :class="balClass(currentBalance)">
                    {{ currentBalance?.label ?? "查询额度…" }}
                  </span>
                  <span v-if="currentBalance?.detail" class="nb-detail">{{ currentBalance.detail }}</span>
                  <button
                    v-if="currentBalance?.consoleUrl"
                    class="nb-console"
                    title="打开控制台"
                    @click.stop="openSite(currentBalance.consoleUrl)"
                  >
                    <ExternalLink :size="11" :stroke-width="1.8" />
                  </button>
                  <button class="nb-refresh" title="刷新额度" @click.stop="store.refreshBalance(current.id)">
                    <span v-if="store.balanceBusy[current.id]" class="spinner sm" />
                    <RefreshCw v-else :size="11" :stroke-width="1.8" />
                  </button>
                </div>

                <!-- codex 未授权时,大绿主操作(全卡可点) -->
                <button
                  v-if="current.kind === 'codex' && !store.codex?.loggedIn"
                  class="now-cta codex-cta"
                  @click="codexOpen = true"
                >
                  <LogIn :size="14" :stroke-width="2" />
                  ChatGPT 一键授权
                </button>
                <!-- Claude 官方未登录订阅时,主操作 = 授权登录 -->
                <button
                  v-else-if="current.kind === 'official' && !store.claudeAuth?.loggedIn"
                  class="now-cta claude-cta"
                  @click="claudeOpen = true; startClaudeAuth()"
                >
                  <LogIn :size="14" :stroke-width="2" />
                  授权登录 Claude 订阅
                </button>
                <button v-else class="now-cta" @click="openBoard">
                  <BarChart3 :size="13" :stroke-width="1.8" />
                  查看用量详情
                </button>
              </section>

              <!-- ★ 中段:供应商全量列表(WeSight 风) -->
              <div class="prov-section">
                <div class="section-head">
                  <span>供应商</span>
                  <span class="section-sub">{{ store.providers.length }} 个</span>
                </div>

                <!-- 搜索命中 -->
                <template v-if="filter.trim()">
                  <div class="prov-list">
                    <div
                      v-for="p in filtered"
                      :key="p.id"
                      class="prov-row"
                      :class="{ on: p.id === store.currentId, pending: store.switching === p.id }"
                      @click="onRowClick(p)"
                    >
                      <span class="row-bar" v-if="p.id === store.currentId" />
                      <span class="prov-dot" :style="{ background: p.color }" />
                      <span class="prov-info">
                        <span class="prov-name">
                          {{ p.name }}
                          <span v-if="p.kind === 'codex'" class="kcodex">GPT</span>
                        </span>
                        <span class="prov-host">{{ subtitleOf(p) }}</span>
                      </span>
                      <span class="prov-tail">
                        <span v-if="store.switching === p.id" class="spinner" />
                        <span v-else-if="p.id === store.currentId" class="badge-on">
                          <Check :size="11" :stroke-width="2.6" /> 使用中
                        </span>
                        <span v-else-if="p.kind === 'codex' || p.kind === 'copilot'" class="badge-oauth">授权</span>
                        <span v-else-if="!p.hasKey" class="badge-need">配置</span>

                        <span class="row-actions">
                          <button v-if="p.websiteUrl" class="mini-act" title="官网" @click.stop="openSite(p.websiteUrl)">
                            <ExternalLink :size="12" :stroke-width="1.8" />
                          </button>
                          <button
                            v-if="p.kind !== 'codex' && p.kind !== 'copilot'"
                            class="mini-act"
                            :title="p.isPreset ? '配置' : '编辑'"
                            @click.stop="editProvider(p)"
                          >
                            <Pencil :size="12" :stroke-width="1.8" />
                          </button>
                          <button
                            v-if="(p.isPreset && p.hasKey && p.kind === 'key') || p.kind === 'custom'"
                            class="mini-act danger"
                            :title="p.isPreset ? '清除配置' : '删除'"
                            @click.stop="removeProvider(p)"
                          >
                            <Trash2 :size="12" :stroke-width="1.8" />
                          </button>
                        </span>
                      </span>
                    </div>
                    <div v-if="filtered.length === 0" class="list-empty">无匹配供应商</div>
                  </div>
                </template>

                <!-- 默认:分组(常用 / 全部) -->
                <template v-else>
                  <div v-if="recentList.length" class="group">
                    <div class="group-head">
                      <Star :size="10" :stroke-width="2" /> 常用
                    </div>
                    <div class="prov-list">
                      <div
                        v-for="p in recentList"
                        :key="p.id"
                        class="prov-row"
                        :class="{ on: p.id === store.currentId, pending: store.switching === p.id }"
                        @click="onRowClick(p)"
                      >
                        <span class="row-bar" v-if="p.id === store.currentId" />
                        <span class="prov-dot" :style="{ background: p.color }" />
                        <span class="prov-info">
                          <span class="prov-name">
                            {{ p.name }}
                            <span v-if="p.kind === 'codex'" class="kcodex">GPT</span>
                          </span>
                          <span class="prov-host">{{ subtitleOf(p) }}</span>
                        </span>
                        <span class="prov-tail">
                          <span v-if="store.switching === p.id" class="spinner" />
                          <span v-else-if="p.id === store.currentId" class="badge-on">
                            <Check :size="11" :stroke-width="2.6" /> 使用中
                          </span>
                          <span v-else-if="p.kind === 'codex' || p.kind === 'copilot'" class="badge-oauth">授权</span>
                          <span v-else-if="!p.hasKey" class="badge-need">配置</span>

                          <span class="row-actions">
                            <button v-if="p.websiteUrl" class="mini-act" title="官网" @click.stop="openSite(p.websiteUrl)">
                              <ExternalLink :size="12" :stroke-width="1.8" />
                            </button>
                            <button
                              v-if="p.kind !== 'codex' && p.kind !== 'copilot'"
                              class="mini-act"
                              :title="p.isPreset ? '配置' : '编辑'"
                              @click.stop="editProvider(p)"
                            >
                              <Pencil :size="12" :stroke-width="1.8" />
                            </button>
                            <button
                              v-if="(p.isPreset && p.hasKey && p.kind === 'key') || p.kind === 'custom'"
                              class="mini-act danger"
                              :title="p.isPreset ? '清除配置' : '删除'"
                              @click.stop="removeProvider(p)"
                            >
                              <Trash2 :size="12" :stroke-width="1.8" />
                            </button>
                          </span>
                        </span>
                      </div>
                    </div>
                  </div>

                  <div class="group">
                    <div class="group-head">
                      <Sparkles :size="10" :stroke-width="2" /> 全部
                    </div>
                    <div class="prov-list">
                      <div
                        v-for="p in restList"
                        :key="p.id"
                        class="prov-row"
                        :class="{ on: p.id === store.currentId, pending: store.switching === p.id }"
                        @click="onRowClick(p)"
                      >
                        <span class="row-bar" v-if="p.id === store.currentId" />
                        <span class="prov-dot" :style="{ background: p.color }" />
                        <span class="prov-info">
                          <span class="prov-name">
                            {{ p.name }}
                            <span v-if="p.kind === 'codex'" class="kcodex">GPT</span>
                          </span>
                          <span class="prov-host">{{ subtitleOf(p) }}</span>
                        </span>
                        <span class="prov-tail">
                          <span v-if="store.switching === p.id" class="spinner" />
                          <span v-else-if="p.id === store.currentId" class="badge-on">
                            <Check :size="11" :stroke-width="2.6" /> 使用中
                          </span>
                          <span v-else-if="p.kind === 'codex' || p.kind === 'copilot'" class="badge-oauth">授权</span>
                          <span v-else-if="!p.hasKey" class="badge-need">配置</span>

                          <span class="row-actions">
                            <button v-if="p.websiteUrl" class="mini-act" title="官网" @click.stop="openSite(p.websiteUrl)">
                              <ExternalLink :size="12" :stroke-width="1.8" />
                            </button>
                            <button
                              v-if="p.kind !== 'codex' && p.kind !== 'copilot'"
                              class="mini-act"
                              :title="p.isPreset ? '配置' : '编辑'"
                              @click.stop="editProvider(p)"
                            >
                              <Pencil :size="12" :stroke-width="1.8" />
                            </button>
                            <button
                              v-if="(p.isPreset && p.hasKey && p.kind === 'key') || p.kind === 'custom'"
                              class="mini-act danger"
                              :title="p.isPreset ? '清除配置' : '删除'"
                              @click.stop="removeProvider(p)"
                            >
                              <Trash2 :size="12" :stroke-width="1.8" />
                            </button>
                          </span>
                        </span>
                      </div>
                    </div>
                  </div>

                  <button class="add-row" @click="addCustom">
                    <Plus :size="13" :stroke-width="2.2" /> 添加自定义供应商
                  </button>

                  <!-- 生图模型: 后端是**独立的一张表**(见 provider/image_store.rs 文件头) -->
                  <ImageProviderPanel />
                </template>
              </div>

              <!-- ★ Codex 授权大卡 (整张更醒目) -->
              <Transition name="ed-fade">
                <div v-if="codexOpen" class="codex-card">
                  <div class="codex-card-head">
                    <div class="ed-title">
                      <ShieldCheck :size="14" :stroke-width="2" />
                      Codex (ChatGPT) 授权
                    </div>
                    <button class="icon-btn sm" @click="codexOpen = false">
                      <X :size="13" />
                    </button>
                  </div>

                  <!-- 授权进行中 -->
                  <template v-if="codexDevice">
                    <!-- 回环一键:浏览器点 Authorize 即完成, 零核对零回贴 -->
                    <template v-if="codexDevice.mode === 'auto'">
                      <p class="codex-note">
                        已为你打开 ChatGPT 授权页。在浏览器里登录并点「<b>Authorize</b>」即可,
                        授权会自动送回 Polaris,无需核对配对码:
                      </p>
                    </template>
                    <!-- 设备码兜底:1455 端口被占时的旧流程 -->
                    <template v-else>
                      <p class="codex-note">
                        已为你打开 ChatGPT 授权页。在浏览器里确认设备码后回到这里,授权完成会自动识别:
                      </p>
                      <button
                        class="codex-code"
                        :title="codexCopied ? '已复制' : '点击复制'"
                        @click="copyUserCode"
                      >
                        {{ codexDevice.userCode }}
                        <span class="code-copy">{{ codexCopied ? "已复制" : "复制" }}</span>
                      </button>
                    </template>
                    <p class="codex-poll"><span class="spinner" /> 等待浏览器中完成授权…</p>
                    <div class="ed-actions">
                      <button class="ed-cancel" @click="resetCodexAuth">取消</button>
                      <button class="ed-save" @click="openCodexVerify">
                        <ExternalLink :size="13" :stroke-width="2" /> 重新打开授权页
                      </button>
                    </div>
                  </template>

                  <!-- 已授权 -->
                  <template v-else-if="store.codex && store.codex.loggedIn">
                    <p class="codex-ok">
                      <ShieldCheck :size="14" :stroke-width="2" /> 已授权 ChatGPT
                    </p>
                    <p v-if="store.currentId === 'codex'" class="codex-note">
                      Claude Code 正经本地翻译代理使用你的 ChatGPT 订阅(<code>gpt-5.5</code>)<template
                        v-if="store.codexProxy?.running"
                      > · 127.0.0.1:{{ store.codexProxy.port }}</template
                      >。
                    </p>
                    <p v-else class="codex-note">
                      凭据已写入 <code>~/.codex/auth.json</code>。点「用 GPT 对话」即让 Claude Code 经本地翻译代理用上 ChatGPT 订阅(<code>gpt-5.5</code>)。
                    </p>
                    <p v-if="store.codexProxy?.lastError" class="codex-fail">
                      代理上次报错:{{ store.codexProxy.lastError }}
                    </p>
                    <div class="ed-actions">
                      <button class="ed-cancel" @click="startCodexAuth" :disabled="codexBusy">
                        <RefreshCw :size="13" :stroke-width="2" /> 重新授权
                      </button>
                      <button
                        v-if="store.currentId !== 'codex'"
                        class="ed-save login"
                        @click="routeCodex"
                      >
                        <Zap :size="13" :stroke-width="2" /> 用 GPT 对话
                      </button>
                      <button v-else class="ed-save" @click="codexOpen = false">
                        <Check :size="13" :stroke-width="2" /> 完成
                      </button>
                    </div>
                  </template>

                  <!-- 未授权:显著大按钮 -->
                  <template v-else>
                    <p class="codex-note">
                      用 ChatGPT 账号授权(无需安装 codex CLI)。点击后将自动打开浏览器,
                      登录并点「Authorize」即自动完成,凭据写入
                      <code>~/.codex/auth.json</code>,授权后点「用 GPT 对话」即生效。
                    </p>
                    <div class="ed-actions">
                      <button class="ed-cancel" @click="codexOpen = false">关闭</button>
                      <button
                        class="ed-save login big"
                        @click="startCodexAuth"
                        :disabled="codexBusy"
                      >
                        <span v-if="codexBusy" class="spinner" />
                        <LogIn v-else :size="14" :stroke-width="2" />
                        {{ codexBusy ? "正在发起…" : "ChatGPT 一键授权" }}
                      </button>
                    </div>
                  </template>

                  <p v-if="codexErr" class="codex-fail">
                    <CircleAlert :size="12" :stroke-width="2" /> {{ codexErr }}
                  </p>
                </div>
              </Transition>

              <!-- ★ Claude 官方订阅授权大卡 (手工回贴授权码) -->
              <Transition name="ed-fade">
                <div v-if="claudeOpen" class="claude-card">
                  <div class="codex-card-head">
                    <div class="ed-title claude">
                      <ShieldCheck :size="14" :stroke-width="2" />
                      Claude 官方订阅授权
                    </div>
                    <button class="icon-btn sm" @click="claudeOpen = false">
                      <X :size="13" />
                    </button>
                  </div>

                  <!-- 已登录 -->
                  <template v-if="store.claudeAuth?.loggedIn && !claudeLogin">
                    <p class="codex-ok claude">
                      <ShieldCheck :size="14" :stroke-width="2" /> 已登录 Claude 订阅
                    </p>
                    <p class="codex-note">
                      凭据已写入 <code>~/.claude/.credentials.json</code>,Polaris 与终端
                      <code>claude</code> 都会复用这份订阅,无需在外壳里再登录。
                    </p>
                    <div class="ed-actions">
                      <button class="ed-cancel" @click="startClaudeAuth()" :disabled="claudeBusy">
                        <RefreshCw :size="13" :stroke-width="2" /> 重新授权
                      </button>
                      <button class="ed-save" @click="claudeOpen = false">
                        <Check :size="13" :stroke-width="2" /> 完成
                      </button>
                    </div>
                  </template>

                  <!-- 授权进行中 -->
                  <template v-else-if="claudeLogin">
                    <!-- 回环一键:浏览器点 Authorize 即完成, 零复制零回贴 -->
                    <template v-if="claudeLogin.mode === 'auto'">
                      <p class="codex-note">
                        已为你打开 Claude 登录页。登录并点「<b>Authorize</b>」即可,
                        授权会自动送回 Polaris,无需复制授权码:
                      </p>
                      <p class="codex-poll"><span class="spinner" /> 等待浏览器中完成授权…</p>
                      <div class="ed-actions">
                        <button class="ed-cancel" @click="resetClaudeAuth">取消</button>
                        <button class="ed-cancel" @click="startClaudeAuth(true)">
                          改用手工回贴
                        </button>
                        <button class="ed-save login claude" @click="openClaudeAuthPage">
                          <ExternalLink :size="13" :stroke-width="2" /> 重新打开登录页
                        </button>
                      </div>
                    </template>
                    <!-- 手工回贴兜底:54545 被占 / 用户主动选择 -->
                    <template v-else>
                      <p class="codex-note">
                        已为你打开 Claude 登录页。登录并点「Authorize」后,页面会给出一段授权码,
                        <b>整段复制</b>粘贴到下面(形如 <code>xxxx#yyyy</code>,带 # 一起贴):
                      </p>
                      <textarea
                        v-model="claudePasted"
                        class="claude-input"
                        rows="2"
                        placeholder="在此粘贴授权码…"
                        spellcheck="false"
                        @keydown.enter.prevent="submitClaudeCode"
                      />
                      <div class="ed-actions">
                        <button class="ed-cancel" @click="openClaudeAuthPage">
                          <ExternalLink :size="13" :stroke-width="2" /> 重新打开登录页
                        </button>
                        <button
                          class="ed-save login claude"
                          :disabled="claudeBusy || !claudePasted.trim()"
                          @click="submitClaudeCode"
                        >
                          <span v-if="claudeBusy" class="spinner" />
                          <Check v-else :size="13" :stroke-width="2" />
                          {{ claudeBusy ? "验证中…" : "完成授权" }}
                        </button>
                      </div>
                    </template>
                  </template>

                  <!-- 未授权:发起按钮 -->
                  <template v-else>
                    <p class="codex-note">
                      用 Claude 账号登录订阅(Pro / Max)。点击后将打开浏览器,
                      登录并点「Authorize」即自动完成授权,凭据写入
                      <code>~/.claude/.credentials.json</code>。
                    </p>
                    <div class="ed-actions">
                      <button class="ed-cancel" @click="claudeOpen = false">关闭</button>
                      <button
                        class="ed-save login claude big"
                        :disabled="claudeBusy"
                        @click="startClaudeAuth()"
                      >
                        <span v-if="claudeBusy" class="spinner" />
                        <LogIn v-else :size="14" :stroke-width="2" />
                        {{ claudeBusy ? "正在打开登录页…" : "授权登录 Claude 订阅" }}
                      </button>
                    </div>
                  </template>

                  <p v-if="claudeErr" class="codex-fail">
                    <CircleAlert :size="12" :stroke-width="2" /> {{ claudeErr }}
                  </p>
                </div>
              </Transition>

              <div v-if="store.error" class="err-line">{{ store.error }}</div>

              <!-- ★ 下段:用量 + 功能键 -->
              <section class="usage">
                <div class="usage-head">
                  <span class="u-title">Token 用量</span>
                  <div class="u-actions">
                    <button class="ghost" title="完整统计" @click="openBoard">
                      <BarChart3 :size="12" :stroke-width="1.8" /> 详细
                    </button>
                    <button class="icon-btn sm" title="刷新" @click="store.refreshUsage()">
                      <RefreshCw :size="12" :stroke-width="1.8" />
                    </button>
                  </div>
                </div>

                <template v-if="store.usage?.available">
                  <div class="period-chips">
                    <button
                      v-for="pd in periods"
                      :key="pd.key"
                      class="chip"
                      :class="{ on: period === pd.key }"
                      @click="period = pd.key"
                    >
                      <span class="chip-lab">{{ pd.label }}</span>
                      <span class="chip-num">{{ fmt(bucketOf(pd.key)?.total || 0) }}</span>
                    </button>
                  </div>
                  <div v-if="activeBucket" class="mini-foot">
                    <span>成本估算 <b>{{ fmtCost(activeBucket.cost) }}</b></span>
                    <span>输入 {{ fmt(activeBucket.input) }} · 输出 {{ fmt(activeBucket.output) }}</span>
                    <span>{{ activeBucket.requests }} 次</span>
                  </div>
                </template>
                <div v-else class="usage-empty">
                  暂无用量数据<br /><span>(尚未通过 Claude Code 产生会话)</span>
                </div>

                <div class="util-row">
                  <button class="util" title="管理供应商" @click="addCustom">
                    <KeyRound :size="12" :stroke-width="1.8" /> 管理
                  </button>
                  <button class="util" title="导入配置" @click="importExport">
                    <Download :size="12" :stroke-width="1.8" /> 导入
                  </button>
                  <button class="util" title="导出配置" @click="importExport">
                    <Upload :size="12" :stroke-width="1.8" /> 导出
                  </button>
                </div>
              </section>
            </div>
          </div>
        </div>
      </Transition>
    </Teleport>
  </div>
</template>

<style scoped>
.dock-root { width: 100%; }

.pill {
  width: 100%;
  display: flex;
  align-items: center;
  gap: 9px;
  padding: 7px 9px;
  background: linear-gradient(180deg, var(--panel) 0%, var(--bg-soft) 100%);
  border: 1px solid var(--border-soft);
  border-radius: 9px;
  text-align: left;
  transition: border-color 140ms ease, box-shadow 140ms ease;
  box-shadow: var(--shadow-sm);
}
.pill:hover { border-color: var(--border-strong); box-shadow: var(--shadow); }
.pill.active { border-color: var(--primary); box-shadow: 0 0 0 2px var(--primary-soft); }
.pill.rail { justify-content: center; padding: 8px 0; }
.dot { width: 8px; height: 8px; border-radius: 50%; flex-shrink: 0; transition: box-shadow 200ms ease; }
.pill-main { flex: 1; display: flex; flex-direction: column; min-width: 0; gap: 1px; }
.pill-name { font-size: 12.5px; color: var(--text); font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.pill-sub { font-size: 10px; color: var(--muted); font-family: var(--mono); display: inline-flex; align-items: center; gap: 3px; }
.chev { color: var(--muted); transition: transform 200ms ease; }
.chev.flip { transform: rotate(180deg); }

.dock-overlay { position: fixed; inset: 0; z-index: 200; }
.panel {
  position: fixed;
  left: 12px;
  bottom: 54px;
  width: 420px;
  max-height: min(82vh, 760px);
  display: flex;
  flex-direction: column;
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: 14px;
  box-shadow: var(--shadow-lg), 0 0 0 1px var(--hairline);
  overflow: hidden;
}
.panel-accent { height: 2px; background: linear-gradient(90deg, var(--primary) 0%, var(--gold) 55%, var(--vermilion) 100%); opacity: 0.85; }
.panel-head { display: flex; align-items: flex-start; justify-content: space-between; padding: 13px 12px 10px 14px; border-bottom: 1px solid var(--border-soft); }
.head-titles { display: flex; flex-direction: column; gap: 2px; }
.head-actions { display: flex; gap: 2px; }
.title { font-family: var(--serif); font-size: 14.5px; font-weight: 600; color: var(--ink); letter-spacing: 1.5px; }
.subtitle { font-size: 10px; color: var(--dim); font-family: var(--mono); }
.icon-btn { border: none; background: transparent; color: var(--muted); border-radius: 5px; width: 26px; height: 26px; display: inline-flex; align-items: center; justify-content: center; flex-shrink: 0; }
.icon-btn:hover { background: var(--selection-bg); color: var(--text); }
.icon-btn.sm { width: 22px; height: 22px; }
.panel-body { flex: 1; min-height: 0; overflow-y: auto; }

.search-row { display: flex; align-items: center; gap: 6px; margin: 9px 10px 2px; padding: 5px 9px; border: 1px solid var(--border); border-radius: 8px; background: var(--bg-soft); }
.search-row:focus-within { border-color: var(--primary); }
.s-ic { color: var(--muted); flex-shrink: 0; }
.search-input { flex: 1; border: none; background: transparent; font-size: 12px; color: var(--text); }
.search-input:focus { outline: none; }

/* ── 联动/隔离开关行 ───────────────────────── */
.link-row {
  display: flex; align-items: center; gap: 8px;
  margin: 7px 10px 0;
  padding: 7px 10px;
  border: 1px solid var(--border-soft);
  border-radius: 9px;
  background: var(--bg-soft);
}
.link-info { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 1px; }
.link-title { font-size: 11.5px; font-weight: 600; color: var(--text); }
.link-desc { font-size: 9.5px; color: var(--dim); font-family: var(--mono); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.link-switch {
  position: relative; flex-shrink: 0;
  width: 32px; height: 18px;
  border: 1px solid var(--border);
  border-radius: 999px;
  background: var(--panel);
  padding: 0; cursor: pointer;
  transition: background 0.18s ease, border-color 0.18s ease;
}
.link-switch .knob {
  position: absolute; top: 2px; left: 2px;
  width: 12px; height: 12px; border-radius: 50%;
  background: var(--muted);
  transition: transform 0.18s ease, background 0.18s ease;
}
.link-switch.on { background: var(--primary); border-color: var(--primary); }
.link-switch.on .knob { transform: translateX(14px); background: #fff; }

/* ── 当前状态卡 (新) ───────────────────────── */
.now-card {
  margin: 8px 10px 4px;
  padding: 10px 11px 9px;
  border: 1px solid var(--border);
  border-radius: 10px;
  background: linear-gradient(180deg, var(--panel) 0%, var(--bg-soft) 100%);
  display: flex; flex-direction: column; gap: 9px;
}
.now-card.codex { border-color: #10a37f55; background: #10a37f0c; }
.now-row { display: flex; align-items: center; gap: 9px; }
.now-dot { width: 10px; height: 10px; border-radius: 50%; flex-shrink: 0; }
.now-info { flex: 1; min-width: 0; }
.now-name { font-size: 13px; color: var(--text); font-weight: 600; }
.now-host { font-size: 10.5px; color: var(--muted); font-family: var(--mono); margin-top: 1px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.now-host .need-auth { color: #d97706; font-weight: 600; }
.now-today { text-align: right; flex-shrink: 0; }
.now-num { font-family: var(--mono); font-size: 14px; font-weight: 600; color: var(--primary-deep); letter-spacing: -0.3px; }
.now-lab { font-size: 9.5px; color: var(--dim); }
/* 当前供应商套餐额度行 */
.now-balance {
  display: flex; align-items: center; gap: 6px;
  padding: 6px 9px;
  border: 1px solid var(--border-soft);
  border-radius: 8px;
  background: var(--bg-soft);
}
.nb-ic { color: var(--muted); flex-shrink: 0; }
.nb-label { font-family: var(--mono); font-size: 12.5px; font-weight: 700; letter-spacing: -0.2px; flex-shrink: 0; }
.nb-label.ok { color: #16a34a; }
.nb-label.alive { color: var(--primary-deep); }
.nb-label.err { color: var(--vermilion); }
.nb-label.muted { color: var(--muted); }
.nb-detail { flex: 1; min-width: 0; font-size: 10px; color: var(--dim); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.nb-console, .nb-refresh {
  border: none; background: transparent; color: var(--muted);
  width: 22px; height: 22px; border-radius: 5px; flex-shrink: 0;
  display: inline-flex; align-items: center; justify-content: center;
}
.nb-console:hover, .nb-refresh:hover { background: var(--selection-bg); color: var(--primary); }
.spinner.sm { width: 11px; height: 11px; border-width: 2px; }

.now-cta {
  width: 100%;
  display: inline-flex; align-items: center; justify-content: center; gap: 5px;
  padding: 7px 10px;
  border: 1px solid var(--border);
  background: var(--panel);
  color: var(--text-2);
  font-size: 12px;
  border-radius: 7px;
  font-weight: 500;
  transition: border-color 120ms ease, background 120ms ease;
}
.now-cta:hover { border-color: var(--primary); color: var(--primary); background: var(--primary-soft); }
.now-cta.codex-cta {
  background: #10a37f; border-color: #10a37f; color: #fff; font-weight: 600;
  box-shadow: 0 1px 0 #10a37f33, 0 0 0 3px #10a37f14;
}
.now-cta.codex-cta:hover { background: #0d8a6c; border-color: #0d8a6c; color: #fff; }

/* ── 列表分组 ───────────────────────── */
.prov-section { padding: 6px 6px 4px; }
.section-head { display: flex; align-items: baseline; justify-content: space-between; padding: 6px 6px 3px; }
.section-head > span:first-child { font-family: var(--serif); font-size: 11px; letter-spacing: 1.2px; color: var(--dim); }
.section-sub { font-size: 9.5px; color: var(--dim); font-family: var(--mono); }

.group { margin-bottom: 4px; }
.group-head {
  display: inline-flex; align-items: center; gap: 4px;
  padding: 5px 9px 3px;
  font-size: 10px;
  color: var(--dim);
  font-family: var(--serif);
  letter-spacing: 1px;
}

.prov-list { padding: 2px 4px; }
.prov-row { position: relative; display: flex; align-items: center; gap: 9px; padding: 7px 9px; border-radius: 8px; cursor: pointer; transition: background 120ms ease; }
.prov-row:hover { background: var(--selection-bg); }
.prov-row.on { background: var(--primary-soft); }
.prov-row.pending { opacity: 0.6; }
.row-bar { position: absolute; left: 0; top: 6px; bottom: 6px; width: 2.5px; border-radius: 2px; background: var(--primary); }
.prov-dot { width: 9px; height: 9px; border-radius: 50%; flex-shrink: 0; }
.prov-info { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 1px; }
.prov-name { font-size: 12.5px; color: var(--text); font-weight: 500; display: inline-flex; align-items: center; gap: 5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.prov-name .kcodex { font-family: var(--mono); font-size: 8.5px; padding: 0 4px; border-radius: 3px; color: #10a37f; border: 1px solid #10a37f66; font-weight: 600; letter-spacing: 0.5px; }
.prov-host { font-size: 10px; color: var(--muted); font-family: var(--mono); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.prov-tail { display: flex; align-items: center; gap: 4px; flex-shrink: 0; }
.badge-on { display: inline-flex; align-items: center; gap: 3px; font-size: 10px; color: var(--primary-deep); font-weight: 600; }
.badge-need { font-size: 9.5px; color: var(--gold); border: 1px solid var(--gold); border-radius: 4px; padding: 1px 5px; opacity: 0.85; }
.badge-oauth { font-size: 9.5px; color: #10a37f; border: 1px solid #10a37f; border-radius: 4px; padding: 1px 5px; }
.row-actions { display: none; align-items: center; gap: 2px; }
.prov-row:hover .row-actions { display: inline-flex; }
.mini-act { border: none; background: transparent; color: var(--muted); width: 22px; height: 22px; border-radius: 5px; display: inline-flex; align-items: center; justify-content: center; }
.mini-act:hover { background: var(--border); color: var(--text); }
.mini-act.danger:hover { background: var(--vermilion-soft); color: var(--vermilion); }
.spinner { width: 12px; height: 12px; border: 2px solid var(--border); border-top-color: var(--primary); border-radius: 50%; animation: spin 0.7s linear infinite; display: inline-block; }
@keyframes spin { to { transform: rotate(360deg); } }
.list-empty { text-align: center; font-size: 11.5px; color: var(--dim); padding: 12px 0; }
.add-row { width: 100%; display: flex; align-items: center; justify-content: center; gap: 5px; padding: 8px; margin: 4px 0 0; border: 1px dashed var(--border-strong); border-radius: 8px; background: transparent; color: var(--muted); font-size: 12px; }
.add-row:hover { border-color: var(--primary); color: var(--primary); background: var(--primary-soft); }

/* ── Codex 授权大卡 ───────────────────────── */
.codex-card { margin: 6px 10px 8px; padding: 11px; border: 1px solid #10a37f55; border-radius: 10px; background: #10a37f0c; display: flex; flex-direction: column; gap: 7px; }
.codex-card-head { display: flex; align-items: center; justify-content: space-between; }
.ed-title { display: inline-flex; align-items: center; gap: 5px; font-size: 12px; font-weight: 600; color: #10a37f; font-family: var(--serif); letter-spacing: 0.5px; }
.ed-actions { display: flex; gap: 6px; justify-content: flex-end; margin-top: 1px; }
.ed-cancel, .ed-save { display: inline-flex; align-items: center; gap: 4px; border: 1px solid var(--border); background: var(--panel); color: var(--text-2); font-size: 11.5px; padding: 5px 12px; border-radius: 6px; }
.ed-cancel:hover { background: var(--selection-bg); }
.ed-save { background: var(--ink); color: #fff; border-color: var(--ink); }
.ed-save:hover { background: var(--primary); border-color: var(--primary); }
.ed-save.login { background: #10a37f; border-color: #10a37f; }
.ed-save.login:hover { background: #0d8a6c; }
.ed-save.big { padding: 8px 14px; font-size: 12.5px; font-weight: 600; }
.ed-save:disabled, .ed-cancel:disabled { opacity: 0.55; cursor: default; }

.codex-note { margin: 0; font-size: 11px; color: var(--text-2); line-height: 1.6; }
.codex-note code, .codex-cmd { font-family: var(--mono); font-size: 10.5px; background: var(--code-bg); color: var(--code-text); padding: 1px 5px; border-radius: 4px; }
.codex-cmd { display: block; padding: 6px 8px; user-select: all; }
.codex-ok { margin: 0; display: inline-flex; align-items: center; gap: 5px; font-size: 12px; font-weight: 600; color: #10a37f; }
.codex-code { display: flex; align-items: center; justify-content: space-between; gap: 8px; font-family: var(--mono); font-size: 17px; font-weight: 700; letter-spacing: 3px; color: #10a37f; background: #10a37f14; border: 1px dashed #10a37f66; border-radius: 7px; padding: 8px 12px; cursor: pointer; user-select: all; }
.codex-code:hover { background: #10a37f22; }
.code-copy { font-family: var(--sans); font-size: 10px; font-weight: 500; letter-spacing: 0; color: var(--muted); }
.codex-poll { margin: 0; display: inline-flex; align-items: center; gap: 6px; font-size: 11px; color: var(--text-2); }

/* ── Claude 官方订阅授权大卡 (暖橙) ───────────────────────── */
.now-cta.claude-cta { background: #cc785c; border-color: #cc785c; color: #fff; font-weight: 600; box-shadow: 0 1px 0 #cc785c33, 0 0 0 3px #cc785c14; }
.now-cta.claude-cta:hover { background: #b9664c; border-color: #b9664c; color: #fff; }
.claude-card { margin: 6px 10px 8px; padding: 11px; border: 1px solid #cc785c55; border-radius: 10px; background: #cc785c0c; display: flex; flex-direction: column; gap: 7px; }
.ed-title.claude { color: #b9664c; }
.codex-ok.claude { color: #b9664c; }
.ed-save.login.claude { background: #cc785c; border-color: #cc785c; }
.ed-save.login.claude:hover { background: #b9664c; border-color: #b9664c; }
.claude-input {
  width: 100%; resize: vertical; min-height: 38px;
  font-family: var(--mono); font-size: 11.5px; line-height: 1.5;
  color: var(--text); background: var(--bg-soft);
  border: 1px dashed #cc785c66; border-radius: 7px; padding: 7px 9px;
}
.claude-input:focus { outline: none; border-color: #cc785c; border-style: solid; }
.codex-fail { margin: 1px 0 0; display: inline-flex; align-items: center; gap: 4px; font-size: 11px; color: var(--vermilion); background: var(--vermilion-soft); border-radius: 6px; padding: 6px 9px; line-height: 1.5; }
.err-line { margin: 0 14px 9px; font-size: 11px; color: var(--vermilion); background: var(--vermilion-soft); border-radius: 6px; padding: 6px 9px; }

/* ── 用量 + 功能键 ───────────────────────── */
.usage { border-top: 1px solid var(--border-soft); padding: 12px 14px 14px; margin-top: 4px; }
.usage-head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 10px; }
.u-title { font-family: var(--serif); font-size: 11px; letter-spacing: 1.5px; color: var(--dim); }
.u-actions { display: flex; align-items: center; gap: 4px; }
.ghost { display: inline-flex; align-items: center; gap: 4px; border: 1px solid var(--border); background: var(--panel); color: var(--text-2); font-size: 10.5px; padding: 3px 8px; border-radius: 6px; }
.ghost:hover { border-color: var(--primary); color: var(--primary); }
.period-chips { display: grid; grid-template-columns: repeat(4, 1fr); gap: 7px; margin-bottom: 10px; }
.chip { display: flex; flex-direction: column; align-items: center; gap: 2px; padding: 8px 4px 7px; border: 1px solid var(--border-soft); border-radius: 9px; background: var(--bg-soft); transition: border-color 120ms ease, background 120ms ease; }
.chip:hover { border-color: var(--border-strong); }
.chip.on { border-color: var(--primary); background: var(--primary-soft); }
.chip-lab { font-size: 10px; color: var(--text-2); }
.chip-num { font-family: var(--mono); font-size: 13.5px; font-weight: 600; color: var(--primary-deep); letter-spacing: -0.3px; }
.chip.on .chip-lab { color: var(--primary-deep); }
.mini-foot { display: flex; flex-wrap: wrap; gap: 4px 12px; font-size: 10.5px; color: var(--muted); padding-top: 4px; }
.mini-foot b { color: var(--primary-deep); font-family: var(--mono); }
.usage-empty { text-align: center; font-size: 11.5px; color: var(--muted); padding: 16px 0; line-height: 1.7; }
.usage-empty span { font-size: 10px; color: var(--dim); }

.util-row { display: flex; gap: 6px; margin-top: 10px; padding-top: 10px; border-top: 1px dashed var(--border-soft); }
.util {
  flex: 1;
  display: inline-flex; align-items: center; justify-content: center; gap: 4px;
  border: 1px solid var(--border-soft);
  background: var(--bg-soft);
  color: var(--text-2);
  font-size: 11px;
  padding: 5px 6px;
  border-radius: 6px;
  transition: border-color 120ms ease, color 120ms ease, background 120ms ease;
}
.util:hover { border-color: var(--primary); color: var(--primary); background: var(--primary-soft); }

.dock-fade-enter-active, .dock-fade-leave-active { transition: opacity 180ms ease; }
.dock-fade-enter-active .panel, .dock-fade-leave-active .panel { transition: transform 220ms cubic-bezier(0.16, 1, 0.3, 1), opacity 180ms ease; transform-origin: bottom left; }
.dock-fade-enter-from, .dock-fade-leave-to { opacity: 0; }
.dock-fade-enter-from .panel, .dock-fade-leave-to .panel { opacity: 0; transform: translateY(10px) scale(0.97); }
.ed-fade-enter-active, .ed-fade-leave-active { transition: opacity 160ms ease, transform 160ms ease; }
.ed-fade-enter-from, .ed-fade-leave-to { opacity: 0; transform: translateY(-4px); }
</style>
