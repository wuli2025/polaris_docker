<script setup lang="ts">
/**
 * 隔空同屏 · 舞台（桌面侧）
 *
 * 手机在预览某个文件时点「投到电脑」→ 主机把文件**包装成一页自包含网页**（beam_pack）→
 * 同步消息经 iroh P2P 通道广播（beam:sync）→ 这里自动把电脑窗口唤到前台、打开同一页，
 * 之后两端滚动/指点/字号互相跟随，效果与手机上看到的完全一致。
 *
 * 三条设计决定：
 *  ① **两端渲染同一份 HTML**（不是各画各的），滚动比例才有意义、观感才真的一样。
 *  ② 页面塞进 **不给 allow-same-origin 的 sandbox iframe**：页内脚本跑在 opaque origin，
 *     够不着主机接口，就算文件里混了脚本也偷不到 owner 令牌。两端只经 postMessage 通话。
 *  ③ 回声靠 `from` 挡：自己发的消息会被主机总线原样广播回本机 UI（bridge_to_ui），
 *     不按设备 id 过滤就会「自己跟自己滚」。
 */
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import {
  MonitorSmartphone,
  X,
  Link2,
  Link2Off,
  Minus,
  Plus,
  Download,
  ExternalLink,
  LoaderCircle,
} from "@lucide/vue";
import { beam, artifacts, isTauri, type BeamDoc, type BeamMsg } from "../tauri";
import { toast } from "../composables/useToast";
import { beamOpenRequest } from "../lib/beamBus";

/** 同屏代理脚本在页内用的固定标记（与 src-tauri/src/beam.rs 的 AGENT_JS 对齐）。 */
const AGENT = "polaris-beam";
/** 本机设备 id：用来忽略总线回灌的自己的消息。 */
const ME = (() => {
  const K = "polaris.beam.deviceId";
  let v = localStorage.getItem(K);
  if (!v) {
    v = `desktop-${Math.random().toString(36).slice(2, 10)}`;
    localStorage.setItem(K, v);
  }
  return v;
})();

const open = ref(false);
const loading = ref(false);
const err = ref("");
const doc = ref<BeamDoc | null>(null);
const peer = ref("");
/** 跟随对方：关掉后对方的滚动不再拉动本地视图（自己想单独翻看时用）。 */
const follow = ref(true);
const zoom = ref(1);
const frame = ref<HTMLIFrameElement | null>(null);
/** 收到远端滚动后的抑制窗口：不设就会 A→B→A 来回抖。 */
let lockUntil = 0;
let lastSent = 0;
let unlisten: (() => void) | null = null;

const kindLabel = computed(() => {
  const m: Record<string, string> = {
    md: "文档", text: "文本", code: "代码", image: "图片",
    audio: "音频", video: "视频", html: "网页", svg: "矢量图", pdf: "PDF",
  };
  return m[doc.value?.kind ?? ""] ?? "文件";
});
const sizeLabel = computed(() => {
  const b = doc.value?.htmlBytes ?? 0;
  if (!b) return "";
  return b > 1024 * 1024 ? `${(b / 1024 / 1024).toFixed(1)} MB` : `${Math.round(b / 1024)} KB`;
});

// ── 收：主机总线 → 本地舞台 ─────────────────────────────────────────
async function onMsg(m: BeamMsg) {
  if (!m || m.from === ME) return; // 自己的回声
  peer.value = m.from || "对方设备";
  switch (m.act) {
    case "open":
      if (m.path) await load(m.path);
      break;
    case "close":
      close(false);
      break;
    case "scroll":
      if (follow.value) {
        lockUntil = Date.now() + 400;
        toFrame({ act: "scroll", r: m.r ?? 0 });
      }
      break;
    case "mark":
      // 指点用内容比例 rx/ry(见 AGENT_JS):两端版式不同也指同一段内容。
      toFrame({ act: "mark", rx: m.rx ?? 0, ry: m.ry ?? 0 });
      break;
    case "zoom":
      zoom.value = m.z ?? 1;
      toFrame({ act: "zoom", z: zoom.value });
      break;
    case "ping":
      void beam.send({ act: "pong", from: ME }).catch(() => {});
      break;
  }
}

// 打包代际:连投两个文件时,慢的那次(大图多)后返回会覆盖快的 —— 用代际号丢弃迟到结果。
let loadGen = 0;
async function load(path: string) {
  const gen = ++loadGen;
  loading.value = true;
  err.value = "";
  open.value = true;
  // 电脑可能正缩在托盘里 —— 「同步打开」得真的自己弹出来，否则用户还要去点任务栏。
  void beam.focus().catch(() => {});
  try {
    const packed = await beam.pack(path);
    if (gen !== loadGen) return; // 已被更晚的 open 取代,丢弃
    doc.value = packed;
    zoom.value = 1;
  } catch (e) {
    if (gen !== loadGen) return;
    err.value = (e as Error).message || "打包失败";
    doc.value = null;
  } finally {
    if (gen === loadGen) loading.value = false;
  }
}

// ── 发：本地 iframe → 主机总线 ──────────────────────────────────────
function toFrame(m: Record<string, unknown>) {
  const w = frame.value?.contentWindow;
  if (!w) return;
  try {
    w.postMessage({ ...m, __beam: AGENT }, "*");
  } catch {
    /* iframe 还没就绪，下一条会补上 */
  }
}

function onFrameMsg(e: MessageEvent) {
  const d = e.data as Record<string, unknown> | null;
  if (!d || d.__beam !== AGENT) return;
  // 无 frame 即丢弃(不是「有 frame 才校验」)—— 否则舞台关掉后,桌面别的 iframe
  // (比如预览里打开的一份导出网页,自带同款脚本)的滚动会借道上总线拽动手机。
  if (!frame.value || e.source !== frame.value.contentWindow) return;
  const act = d.act as string;
  if (act === "ready") {
    // 页面解析完才注入初始字号 —— 不再靠定时器赌 iframe 就绪(大页面赌不中)。
    toFrame({ act: "zoom", z: zoom.value });
    return;
  }
  // 「解除跟随」= 我要自己单独翻看,既不收也不发,免得把对方拽着走。
  if (!follow.value) return;
  if (act === "scroll") {
    if (Date.now() < lockUntil) return; // 刚被对方拉动过，别回弹
    const now = Date.now();
    if (now - lastSent < 90) return;
    lastSent = now;
    void beam.send({ act: "scroll", from: ME, r: Number(d.r) || 0 }).catch(() => {});
  } else if (act === "mark") {
    void beam
      .send({ act: "mark", from: ME, rx: Number(d.rx) || 0, ry: Number(d.ry) || 0 })
      .catch(() => {});
  }
}

// ── 工具条 ────────────────────────────────────────────────────────
function setZoom(delta: number) {
  zoom.value = Math.min(2, Math.max(0.6, Math.round((zoom.value + delta) * 10) / 10));
  toFrame({ act: "zoom", z: zoom.value });
  void beam.send({ act: "zoom", from: ME, z: zoom.value }).catch(() => {});
}

async function exportPage() {
  if (!doc.value) return;
  try {
    const out = await beam.export(doc.value.path);
    toast.info(`已导出网页：${out}`);
  } catch (e) {
    toast.error((e as Error).message);
  }
}

async function openExternal() {
  if (!doc.value) return;
  try {
    const out = await beam.export(doc.value.path);
    await artifacts.openExternal(out);
  } catch (e) {
    toast.error((e as Error).message);
  }
}

function close(notify = true) {
  open.value = false;
  doc.value = null;
  err.value = "";
  if (notify) void beam.send({ act: "close", from: ME }).catch(() => {});
}

/** 供外部（互联页「投到手机」）调用：本机发起同屏。 */
async function beamOut(path: string) {
  const r = await beam.send({ act: "open", from: ME, path });
  if (!r?.delivered) {
    toast.info("本机还没设为主机，手机暂时收不到 —— 已在本地打开");
  }
  await load(path);
}
defineExpose({ beamOut });

// 初始字号在页面 ready 时注入(见 onFrameMsg 的 act==="ready"),这里不再用定时器猜。

// 本机其它入口（互联页「投到手机」等）发起的同屏。
watch(beamOpenRequest, (r) => {
  if (r?.path) void beamOut(r.path).catch((e) => toast.error((e as Error).message));
});

onMounted(async () => {
  window.addEventListener("message", onFrameMsg);
  if (!isTauri) return; // 浏览器预览模式下总线不存在，别空转
  try {
    unlisten = await beam.on(onMsg);
  } catch {
    /* 事件面暂不可用：同屏只是收不到，不该炸整个 App */
  }
});
onUnmounted(() => {
  window.removeEventListener("message", onFrameMsg);
  unlisten?.();
});
</script>

<template>
  <transition name="beam-fade">
    <div v-if="open" class="beam" role="dialog" aria-label="隔空同屏">
      <header class="beam-bar">
        <span class="beam-badge"><MonitorSmartphone :size="14" /> 隔空同屏</span>
        <span class="beam-name" :title="doc?.path ?? ''">{{ doc?.name ?? "载入中…" }}</span>
        <span v-if="doc" class="beam-meta">{{ kindLabel }} · {{ sizeLabel }}</span>
        <span v-if="peer" class="beam-peer">来自 {{ peer }}</span>

        <div class="beam-acts">
          <button
            class="beam-btn"
            :class="{ on: follow }"
            :title="follow ? '正在跟随对方（点一下解除）' : '已解除跟随（点一下恢复）'"
            @click="follow = !follow"
          >
            <component :is="follow ? Link2 : Link2Off" :size="15" />
          </button>
          <button class="beam-btn" title="缩小字号" @click="setZoom(-0.1)"><Minus :size="15" /></button>
          <span class="beam-zoom">{{ Math.round(zoom * 100) }}%</span>
          <button class="beam-btn" title="放大字号" @click="setZoom(0.1)"><Plus :size="15" /></button>
          <button class="beam-btn" title="导出为独立网页文件" @click="exportPage"><Download :size="15" /></button>
          <button class="beam-btn" title="用系统浏览器打开" @click="openExternal"><ExternalLink :size="15" /></button>
          <button class="beam-btn danger" title="结束同屏" @click="close()"><X :size="16" /></button>
        </div>
      </header>

      <div class="beam-body">
        <div v-if="loading" class="beam-center">
          <LoaderCircle :size="26" class="beam-spin" />
          <p>正在把文件包装成网页…</p>
        </div>
        <div v-else-if="err" class="beam-center">
          <p class="beam-err">{{ err }}</p>
          <p class="beam-hint">只有知识库、~/Polaris 产物与项目工作目录里的文件能同屏。</p>
        </div>
        <iframe
          v-else-if="doc"
          ref="frame"
          class="beam-frame"
          :srcdoc="doc.html"
          sandbox="allow-scripts allow-popups allow-modals"
          allow="fullscreen"
        ></iframe>
      </div>

      <p class="beam-foot">
        两端看的是同一份自包含网页 · 滚动与指点实时同步 · 走 iroh P2P，不需要同一 WiFi
      </p>
    </div>
  </transition>
</template>

<style scoped>
.beam {
  position: fixed;
  inset: 0;
  z-index: 240;
  display: flex;
  flex-direction: column;
  background: var(--bg, #0f1117);
  backdrop-filter: blur(20px);
}
.beam-bar {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 14px;
  border-bottom: 1px solid var(--line, rgba(255, 255, 255, 0.08));
  background: var(--bg-elev, rgba(255, 255, 255, 0.03));
}
.beam-badge {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  font-size: 12px;
  font-weight: 600;
  padding: 4px 9px;
  border-radius: 999px;
  color: #fff;
  background: linear-gradient(135deg, #4c7dff, #7a5cff);
  flex-shrink: 0;
}
.beam-name {
  font-size: 14px;
  font-weight: 600;
  max-width: 40vw;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.beam-meta,
.beam-peer {
  font-size: 12px;
  opacity: 0.55;
  flex-shrink: 0;
}
.beam-acts {
  margin-left: auto;
  display: flex;
  align-items: center;
  gap: 4px;
}
.beam-btn {
  width: 30px;
  height: 30px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border: none;
  border-radius: 9px;
  background: var(--bg-elev2, rgba(255, 255, 255, 0.06));
  color: inherit;
  opacity: 0.75;
  cursor: pointer;
}
.beam-btn:hover {
  opacity: 1;
}
.beam-btn.on {
  background: #4c7dff;
  color: #fff;
  opacity: 1;
}
.beam-btn.danger:hover {
  background: #d9534f;
  color: #fff;
}
.beam-zoom {
  font-size: 11px;
  opacity: 0.6;
  min-width: 34px;
  text-align: center;
}
.beam-body {
  flex: 1;
  min-height: 0;
  display: flex;
}
.beam-frame {
  flex: 1;
  border: none;
  background: #fff;
}
.beam-center {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  opacity: 0.75;
  font-size: 14px;
}
.beam-err {
  color: #ff8f8f;
  max-width: 460px;
  text-align: center;
}
.beam-hint {
  font-size: 12px;
  opacity: 0.6;
}
.beam-spin {
  animation: beam-rot 1s linear infinite;
}
@keyframes beam-rot {
  to {
    transform: rotate(360deg);
  }
}
.beam-foot {
  margin: 0;
  padding: 7px 14px;
  font-size: 11px;
  opacity: 0.45;
  text-align: center;
  border-top: 1px solid var(--line, rgba(255, 255, 255, 0.08));
}
.beam-fade-enter-active,
.beam-fade-leave-active {
  transition: opacity 0.18s ease;
}
.beam-fade-enter-from,
.beam-fade-leave-to {
  opacity: 0;
}
</style>
