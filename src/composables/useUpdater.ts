// ─────────────────────────────────────────────────────────────
// 自动更新（GitHub Releases 托管）—— 前端 = 后端状态机的「视图」
//
// 旧版是「纯前端、一堆离散 ref 各自维护」；现在更新逻辑收进 Rust 的唯一状态机
// （src-tauri/src/updater.rs，借鉴 OpenCode 桌面端 updater-controller）：
//   - 单飞：并发 check/apply 只跑一次，多次点击不重入；
//   - 可观测：后端每次状态流转 emit("updater://state")，这里 listen 订阅；
//   - 持久化 + 重启续提示：发现新版本落盘，下次启动离线也能先看到「有更新待装」。
//
// 本文件只做两件事：① 订阅后端状态 → 映射成下面这些「兼容旧名」的派生量
// （UpdateBanner / UpdatePanel 无需改动）；② 把用户动作转发成后端命令。
// 无网络 / 还没发布 release / 非 Tauri 运行时都会被静默吞掉，不打扰用户。
// ─────────────────────────────────────────────────────────────
import { computed, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import { isTauri } from "../tauri";

// 后端 updater.rs 的 UpdaterState（serde tag = "status"）。
type UpdaterState =
  | { status: "disabled" }
  | { status: "idle" }
  | { status: "checking" }
  | { status: "up-to-date" }
  | { status: "available"; version: string; notes: string }
  | { status: "downloading"; version: string; percent: number }
  | { status: "ready"; version: string }
  | { status: "installing"; version: string }
  | { status: "error"; message: string };

// /api/version 响应（容器模式独享;桌面模式 fallback 用 getVersion()）。
interface VersionInfo {
  version: string;
  flavor: "docker" | "desktop";
  updater_enabled: boolean;
  socket_present: boolean;
}
interface DockerStatus {
  updater_enabled: boolean;
  socket_present: boolean;
  current_tag: string;
  update_script: boolean;
}
interface DockerUpdateResult {
  success?: boolean;
  exit_code?: number | null;
  tag?: string;
  stdout?: string;
  stderr?: string;
  note?: string;
  dryRun?: boolean;
  wouldRun?: string;
  error?: string;
}

// 后端状态机的当前态（唯一真相源）。
const state = ref<UpdaterState>({ status: "idle" });

// ── 兼容旧契约：以下导出全部由 state 派生，消费组件（Banner/Panel）零改动 ──
export const currentVersion = ref<string>(""); // 当前已安装版本（前端取）
export const lastCheckedAt = ref<number | null>(null); // 上次检查时间戳(ms)
export const dialogDismissed = ref(false); // 中央对话框「以后再说」—— 纯前端态
// 容器模式额外位:
export const isDockerMode = ref<boolean>(!isTauri); // 容器/浏览器预览 = true;桌面 Tauri = false
export const dockerUpdaterEnabled = ref<boolean>(false); // POLARIS_DOCKER_SOCKET=1 && socket 已挂
export const dockerStatus = ref<DockerStatus | null>(null); // 最近一次 docker_status 响应
export const dockerLastApply = ref<DockerUpdateResult | null>(null); // 最近一次 docker_update 响应
export const dockerApplying = ref<boolean>(false); // 更新中（按钮转圈 + 禁用）

const versionOf = (s: UpdaterState): string | null =>
  "version" in s ? s.version : null;

export const updateVersion = computed<string | null>(() => versionOf(state.value)); // 有值=有更新
export const remoteVersion = updateVersion; // 远程最新版本号（语义同上）
export const updateNotes = computed<string>(() =>
  state.value.status === "available" ? state.value.notes : "",
);
export const updating = computed(
  () => state.value.status === "downloading" || state.value.status === "installing",
);
export const updateProgress = computed(() => {
  const s = state.value;
  if (s.status === "downloading") return s.percent;
  if (s.status === "installing" || s.status === "ready") return 100;
  return 0;
});
export const updateError = computed(() =>
  state.value.status === "error" ? state.value.message : "",
);
export const checking = computed(() => state.value.status === "checking");
export const upToDate = computed(() => state.value.status === "up-to-date");
export const checkFailed = computed(() => state.value.status === "error");

let subscribed = false;
let autoChecked = false;

async function fetchDockerVersion(): Promise<VersionInfo | null> {
  try {
    const r = await fetch("/api/version", { cache: "no-store" });
    if (!r.ok) return null;
    return (await r.json()) as VersionInfo;
  } catch {
    return null;
  }
}

async function ensureCurrentVersion(): Promise<void> {
  if (currentVersion.value) return;
  if (isDockerMode.value) {
    // 容器模式：去 /api/version 拉真实版本（之前页面显示 v— 的根因）
    const info = await fetchDockerVersion();
    if (info) {
      currentVersion.value = info.version;
      dockerUpdaterEnabled.value = !!info.updater_enabled && !!info.socket_present;
    }
  } else {
    try {
      currentVersion.value = await getVersion();
    } catch {
      /* 浏览器预览态拿不到，忽略 */
    }
  }
}

/** 订阅后端状态机：先拉一次快照，再 listen 增量。幂等。 */
async function ensureSubscribed(): Promise<void> {
  if (subscribed) return;
  subscribed = true;
  try {
    await listen<UpdaterState>("updater://state", (ev) => {
      state.value = ev.payload;
    });
    // 拉一次初始快照（可能在 listen 建立前就已被 init 设过 available）。
    state.value = await invoke<UpdaterState>("updater_get_state");
  } catch (e) {
    subscribed = false; // 非 Tauri 运行时：留待下次，静默
    console.warn("[updater] subscribe failed:", e);
  }
}

/** 启动时调用一次：订阅 + 触发一次后端检查（失败由状态机记为 error，不弹中央对话框）。 */
export async function checkForUpdate(): Promise<void> {
  if (autoChecked) return;
  autoChecked = true;
  await ensureCurrentVersion();
  await ensureSubscribed();
  try {
    await invoke("updater_check");
    lastCheckedAt.value = Date.now();
  } catch (e) {
    console.warn("[updater] auto check failed:", e);
  }
}

/** 用户在「更新」板块点「检查更新」：转发到后端（单飞），带 UI 反馈。 */
export async function manualCheck(): Promise<void> {
  await ensureCurrentVersion();
  await ensureSubscribed();
  dialogDismissed.value = false; // 手动检查后允许中央对话框再次出现
  try {
    await invoke("updater_check");
    lastCheckedAt.value = Date.now();
  } catch (e) {
    console.warn("[updater] manual check failed:", e);
  }
}

/** 用户点「立即更新」：后端下载 + 安装 + 自重启（进度由 updater://state 推送）。 */
export async function applyUpdate(): Promise<void> {
  if (updating.value) return;
  try {
    await invoke("updater_apply");
    // 正常路径里后端会自重启，不会走到这里。
  } catch (e) {
    console.warn("[updater] apply failed:", e);
  }
}

// ── Docker 版独有动作 ───────────────────────────────────────────

/** 容器模式：「检查更新」按钮——调 /api/invoke docker_status,返回 updater_enabled + socket_present。 */
export async function dockerCheck(): Promise<void> {
  await ensureCurrentVersion();
  dialogDismissed.value = false;
  try {
    const r = await fetch("/api/invoke", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ cmd: "docker_status", args: {} }),
    });
    const j = (await r.json()) as DockerStatus & { error?: string };
    if (j.error) throw new Error(j.error);
    dockerStatus.value = j;
    dockerUpdaterEnabled.value = !!j.updater_enabled && !!j.socket_present;
    lastCheckedAt.value = Date.now();
  } catch (e) {
    console.warn("[updater] docker_check failed:", e);
    // 状态里塞个 error 提示;UpdatePanel 兜底
    dockerLastApply.value = { error: `检查失败：${(e as Error).message || e}` };
  }
}

/** 容器模式：「立即更新」按钮——调 /api/invoke docker_update。后端 spawn update.sh。 */
export async function dockerApply(): Promise<void> {
  if (dockerApplying.value) return;
  dockerApplying.value = true;
  dockerLastApply.value = null;
  try {
    const r = await fetch("/api/invoke", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ cmd: "docker_update", args: { confirm: true } }),
    });
    const j = (await r.json()) as DockerUpdateResult;
    dockerLastApply.value = j;
  } catch (e) {
    dockerLastApply.value = { error: `请求失败：${(e as Error).message || e}` };
  } finally {
    dockerApplying.value = false;
  }
}

/** 「以后再说」：只关中央对话框，本次会话不再自动弹（板块入口仍在）。 */
export function dismissUpdate(): void {
  dialogDismissed.value = true;
}
