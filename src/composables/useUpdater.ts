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
// ★ DOCKER 分叉起点:以下整套「容器自更新」在主仓不存在,同步主仓时必须整体保住。
import { isTauri, authHeaders } from "../tauri";

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

// ★ DOCKER 分叉:/api/version 响应（容器模式独享;桌面模式 fallback 用 getVersion()）。
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
// ★ DOCKER 分叉:docker_check_update(update.sh --check 的结构化输出)。
//   不需要 docker.sock —— 所以「有没有新版」和「能不能一键装」是两件独立的事。
interface DockerCheck {
  ok: boolean;
  has_update: boolean;
  current?: string;
  latest?: string;
  image?: string;
  file?: string;
  size?: string;
  source?: string;
  error?: string;
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
// ★ DOCKER 分叉:容器模式额外位
export const isDockerMode = ref<boolean>(!isTauri); // 容器/浏览器预览 = true;桌面 Tauri = false
export const dockerUpdaterEnabled = ref<boolean>(false); // POLARIS_DOCKER_SOCKET=1 && socket 已挂
export const dockerStatus = ref<DockerStatus | null>(null); // 最近一次 docker_status 响应
export const dockerLastApply = ref<DockerUpdateResult | null>(null); // 最近一次 docker_update 响应
export const dockerApplying = ref<boolean>(false); // 更新中（按钮转圈 + 禁用）
export const dockerChecking = ref<boolean>(false); // 检查中（容器线自己的转圈位，桌面的 checking 在容器里恒 false）
export const dockerCheckInfo = ref<DockerCheck | null>(null); // 最近一次 docker_check_update 响应
export const dockerHasUpdate = computed(() => !!dockerCheckInfo.value?.has_update);
export const dockerLatest = computed(() => dockerCheckInfo.value?.latest || "");
/** 没挂 docker.sock 时给用户的兜底命令：SSH 到 NAS 粘一行，等价于网页上点更新。 */
export const dockerSshFallback =
  "curl -fsSL https://llmwiki.cloud/docker/install-r2.sh | sudo bash";

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

// ★ DOCKER 分叉:容器里没有 Tauri IPC,版本号只能从后端 /api/version 读。
async function fetchDockerVersion(): Promise<VersionInfo | null> {
  try {
    const r = await fetch("/api/version", {
      cache: "no-store",
      headers: { ...authHeaders() },
    });
    if (!r.ok) return null;
    return (await r.json()) as VersionInfo;
  } catch {
    return null;
  }
}

async function ensureCurrentVersion(): Promise<void> {
  if (currentVersion.value) return;
  // ★ DOCKER 分叉:容器模式走 /api/version（否则页面显示 v—）
  if (isDockerMode.value) {
    const info = await fetchDockerVersion();
    if (info) {
      currentVersion.value = info.version;
      dockerUpdaterEnabled.value = !!info.updater_enabled && !!info.socket_present;
    }
    return;
  }
  try {
    currentVersion.value = await getVersion();
  } catch {
    /* 非 Tauri 运行时（纯浏览器预览）拿不到，忽略 */
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

/**
 * 启动时调用一次：订阅 + 触发后端检查，发现新版即由 UpdateBanner 自动弹出。
 *
 * **冷启动重试**：开机那一刻网络常还没就绪 → 首次检查直接失败(error)，中央弹窗就不弹了，
 * 用户只能手动去「更新」页才看到。这里改成「渐进退避重试」——只要还没拿到确定结论
 * （发现新版 / 已最新），就隔几秒再试，直到网络恢复，保证「点开 app 就会弹」。
 */
export async function checkForUpdate(): Promise<void> {
  if (autoChecked) return;
  autoChecked = true;
  await ensureCurrentVersion();
  // ★ DOCKER 分叉:容器里没有桌面 updater 状态机(updater_check 是 Tauri IPC,必失败)。
  //   走容器自己的一条线:启动时也真去问一次远端版本,这样「有新版」不用等用户
  //   主动翻到更新页才知道(老版本这里直接 return,页面永远静默)。
  if (isDockerMode.value) {
    await dockerCheck();
    return;
  }
  await ensureSubscribed();
  // 首查错峰推迟 5s（避开首帧 IPC 突发——启动检查更新不抢开屏后的第一波命令），
  // 随后 4s/12s/30s 退避重试（覆盖冷启动到网络就绪的常见窗口）。
  const delays = [5000, 4000, 12000, 30000];
  for (const wait of delays) {
    if (wait) await new Promise((r) => setTimeout(r, wait));
    try {
      const st = await invoke<UpdaterState>("updater_check");
      lastCheckedAt.value = Date.now();
      // 已有确定结论(available=有更新会触发弹窗 / up-to-date=已最新)即收手；
      // 仅「检查失败」才继续退避重试。downloading/installing 也视为已在推进、收手。
      if (st.status !== "error") return;
    } catch (e) {
      console.warn("[updater] auto check failed, will retry:", e);
    }
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

// ── ★ DOCKER 分叉:容器版独有动作(主仓无此段,同步时整体保住)───────────────

/** /api/invoke 的薄封装：设了 POLARIS_AUTH_TOKEN 时全量鉴权,裸 fetch 不带 token 必 401。 */
async function apiInvoke<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  const r = await fetch("/api/invoke", {
    method: "POST",
    headers: { "Content-Type": "application/json", ...authHeaders() },
    body: JSON.stringify({ cmd, args }),
  });
  const j = (await r.json()) as T & { error?: string };
  if (j && (j as { error?: string }).error) throw new Error((j as { error?: string }).error);
  return j;
}

/**
 * 容器模式：「检查更新」按钮。
 *
 * 两件独立的事，一次问清楚：
 *   · docker_status       —— 这台机器**能不能**在网页上一键换镜像（docker.sock 挂没挂）；
 *   · docker_check_update —— 远端**有没有**新版（逐个镜像源拉 manifest，不需要 sock）。
 * ★ 老版本只问前者,于是页面永远说不出「有新版 x.y.z」,用户看到的就是一个没反应的按钮。
 */
export async function dockerCheck(): Promise<void> {
  if (dockerChecking.value) return;
  dockerChecking.value = true;
  await ensureCurrentVersion();
  dialogDismissed.value = false;
  try {
    const st = await apiInvoke<DockerStatus>("docker_status");
    dockerStatus.value = st;
    dockerUpdaterEnabled.value = !!st.updater_enabled;
  } catch (e) {
    console.warn("[updater] docker_status failed:", e);
    dockerLastApply.value = { error: `检查失败：${(e as Error).message || e}` };
  }
  try {
    const ck = await apiInvoke<DockerCheck>("docker_check_update");
    dockerCheckInfo.value = ck;
    if (ck.current) currentVersion.value = ck.current;
    lastCheckedAt.value = Date.now();
  } catch (e) {
    console.warn("[updater] docker_check_update failed:", e);
    dockerCheckInfo.value = {
      ok: false,
      has_update: false,
      error: `查不到最新版本：${(e as Error).message || e}`,
    };
  } finally {
    dockerChecking.value = false;
  }
}

/**
 * 容器模式：「立即更新」按钮——调 /api/invoke docker_update。后端 spawn update.sh，
 * 由它派一个替身容器去下载/校验/换镜像（当前容器会被替换掉，所以这里拿到的只是
 * 「替身已出发」，不是「已装好」）。
 * @param force 版本号相同也重装（用于「重装一遍试试」）。
 */
export async function dockerApply(force = false): Promise<void> {
  if (dockerApplying.value) return;
  dockerApplying.value = true;
  dockerLastApply.value = null;
  try {
    const r = await fetch("/api/invoke", {
      method: "POST",
      headers: { "Content-Type": "application/json", ...authHeaders() },
      body: JSON.stringify({ cmd: "docker_update", args: { confirm: true, force } }),
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
