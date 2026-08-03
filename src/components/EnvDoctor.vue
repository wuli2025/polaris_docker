<script setup lang="ts">
import { computed, defineAsyncComponent, onBeforeUnmount, onMounted, ref, watch } from "vue";
import {
  envDoctor,
  listen,
  isTauri,
  type ClaudeUpdateInfo,
  type EnvReport,
  type EnvStreamEvent,
  type ToolStatus,
  type UvCacheInfo,
  type VerifyReport,
} from "../tauri";
// 懒加载:EnvDoctor 是常驻组件,这里若静态 import 会把 McpConfigModal 拽回主包,
// 把 App.vue:49 的 defineAsyncComponent 架空。模板处已有 v-if 条件,异步化零行为差异。
const McpConfigModal = defineAsyncComponent(() => import("./McpConfigModal.vue"));

/**
 * 环境医生 — 新用户开箱的「环境监测 + 配置安装」。
 * - gate=true: 作为启动流程的一道关 (全屏覆盖), 健康则自动放行;
 * - gate=false: 作为侧栏「环境」页随时复检 / 重装。
 */
const props = withDefaults(defineProps<{ gate?: boolean }>(), { gate: false });
const emit = defineEmits<{ (e: "done"): void }>();

const READY_FLAG = "polaris.env.ready.v1";

type Phase = "checking" | "ready-skip" | "panel";
const phase = ref<Phase>("checking");
const report = ref<EnvReport | null>(null);

// 安装 / 修复 / 更新 的进行态
const busyKind = ref<
  "" | "claude" | "claude-update" | "node" | "pwsh" | "uv" | "path"
>("");
const installReqId = ref<string | null>(null);
const logs = ref<string[]>([]);
const banner = ref<{ kind: "ok" | "err" | "info"; text: string } | null>(null);
let unlisten: (() => void) | null = null;

// Claude Code 更新检测
const updateInfo = ref<ClaudeUpdateInfo | null>(null);
const checkingUpdate = ref(false);

// uv 缓存信息 (装了 uv 才查; 用于展示占用 + 一键清理)
const uvCache = ref<UvCacheInfo | null>(null);
const cleaningCache = ref(false);

// 深度校验: 安装冲突 + 实跑验证。
// 与上面那张工具清单的分工: 清单答「装没装」(查得到文件即绿), 这里答「真能不能跑、会不会打架」——
// 装重了两份、终端里其实跑的是另一份、Store 别名遮挡、配置文件损坏……都只有实跑才看得见。
const verify = ref<VerifyReport | null>(null);
const verifying = ref<"" | "quick" | "deep">("");

const busy = computed(() => busyKind.value !== "");

async function runCheck() {
  report.value = await envDoctor.check();
  return report.value;
}

// 注:这里曾有个 maybeAutoInstallShell() —— 启动关一发现缺 shell 就**自动替用户装
// PowerShell 7**(弹 UAC、走 winget/MSI、国内常拉不动)。现已删除:应用改为**随安装包内置
// Git Bash**(见 src-tauri/crates/polaris-kernel/src/doctor/bundled.rs), 干净 Windows
// 开箱就有 shell, 不需要装任何东西。PowerShell 7 降级为纯可选, 只留手动按钮。

onMounted(async () => {
  // 浏览器预览 / 非 Tauri: 启动关直接放行, 不打扰
  if (!isTauri) {
    if (props.gate) emit("done");
    else {
      report.value = await runCheck();
      phase.value = "panel";
    }
    return;
  }

  // 已通过一次环境检测 → 后续每次启动不再检测, 直接放行。
  // (用户要求: 只第一次打开时检测; 想复检走侧栏「环境」页 gate=false)
  if (props.gate && localStorage.getItem(READY_FLAG)) {
    emit("done");
    return;
  }

  unlisten = await listen<EnvStreamEvent>("env:stream", onStream);

  // env_check 一旦 reject,不能把启动门永远钉死在「正在检测…」(全屏转圈无出口):
  // gate 模式失败即放行进主界面;面板模式落到 panel 并给出错误横幅,可点「重新检测」。
  let r: EnvReport;
  try {
    r = await runCheck();
  } catch (e) {
    banner.value = {
      kind: "err",
      text: `环境检测失败：${String(e)}。可稍后在侧栏「环境」页重新检测。`,
    };
    if (props.gate) emit("done");
    else phase.value = "panel";
    return;
  }
  if (r.ready) localStorage.setItem(READY_FLAG, "1");
  phase.value = r.ready && props.gate ? "ready-skip" : "panel";
});

onBeforeUnmount(() => {
  if (unlisten) unlisten();
});

// 进入面板 / 跳过页且 Claude Code 已安装时, 自动静默检测一次更新
watch(phase, (p) => {
  if ((p === "panel" || p === "ready-skip") && isTauri) {
    if (report.value?.claude.found && !updateInfo.value && !checkingUpdate.value) {
      checkClaudeUpdate();
    }
    if (report.value?.uv.found && !uvCache.value) {
      loadUvCache();
    }
    // 快速校验 (不发请求, ~1-3s) 在侧栏「环境」页自动跑一次。
    // 启动关 (gate) 不自动跑: 那里要的是尽快放行, 不该为了一份诊断多等几秒。
    if (!props.gate && p === "panel" && !verify.value && !verifying.value) {
      runVerify(false);
    }
  }
});

/** 深度校验。deep=true 会真发一次最小请求 (消耗极少量额度), 故只由用户显式点。 */
async function runVerify(deep = false) {
  // 不挡浏览器预览: 那边有 stub, 点了会如实说「预览模式无法深度校验」——
  // 挡掉的话按钮就是个死键, 比给出诚实的否定回答更糟。
  if (verifying.value) return;
  verifying.value = deep ? "deep" : "quick";
  try {
    verify.value = await envDoctor.verify(deep);
  } catch (e) {
    banner.value = { kind: "err", text: `深度校验失败：${String(e)}` };
  } finally {
    verifying.value = "";
  }
}

function onStream(ev: EnvStreamEvent) {
  if (installReqId.value && ev.reqId !== installReqId.value) return;
  if (ev.kind === "log" && ev.line) {
    logs.value.push(ev.line);
    if (logs.value.length > 400) logs.value.splice(0, logs.value.length - 400);
  } else if (ev.kind === "done") {
    finishInstall(ev.ok ?? false, ev.message ?? "");
  }
}

async function finishInstall(ok: boolean, message: string) {
  const justFinished = busyKind.value; // 重置前先记住刚装完的是什么
  installReqId.value = null;
  busyKind.value = "";
  banner.value = { kind: ok ? "ok" : "err", text: message || (ok ? "完成。" : "未成功。") };
  const r = await runCheck();
  if (r.ready) localStorage.setItem(READY_FLAG, "1");
  // 装好 / 更新完后重新检测版本, 让「更新」按钮翻成「已是最新」
  updateInfo.value = null;
  if (r.claude.found) checkClaudeUpdate();
  // 刚装完 uv → 刷新缓存信息卡
  if (justFinished === "uv") loadUvCache();
  // 装/更新过之后旧的校验结论就作废了 (最典型: 刚装完那份可能与老的那份并存) → 重跑快速校验
  if (verify.value) {
    verify.value = null;
    runVerify(false);
  }
}

// 装 Claude Code。默认 "direct"：直抓平台包 tgz (R2 优先 → npmmirror → npmjs) 解压到
// ~/.local/bin，**不经 npm**（后者下 80MB optional 依赖常卡死）。"native" = 官方脚本兜底。
async function installClaude(method: "native" | "direct" = "direct") {
  if (busyKind.value) return; // 防双发:已有安装流程在跑
  banner.value = null;
  logs.value = [];
  busyKind.value = "claude";
  try {
    installReqId.value = await envDoctor.installClaude(method);
    logs.value.push(
      method === "native"
        ? isWin.value
          ? "$ irm https://claude.ai/install.ps1 | iex"
          : "$ curl -fsSL https://claude.ai/install.sh | bash"
        : "$ 下载 Claude Code 平台包 (R2 → npmmirror → yarn → npmjs，逐源自证) → 解压到 ~/.local/bin"
    );
  } catch (e) {
    busyKind.value = "";
    banner.value = { kind: "err", text: String(e) };
  }
}

async function installNode() {
  if (busyKind.value) return; // 防双发
  banner.value = null;
  logs.value = [];
  busyKind.value = "node";
  try {
    installReqId.value = await envDoctor.installNode();
    logs.value.push(
      isWin.value
        ? "$ winget install --id OpenJS.NodeJS.LTS -e  (或下载官方 MSI)"
        : "$ curl -fsSL https://cdn.npmmirror.com/binaries/node/... | tar xz  → ~/.local/polaris-node"
    );
  } catch (e) {
    busyKind.value = "";
    banner.value = { kind: "err", text: String(e) };
  }
}

async function installPwsh() {
  if (busyKind.value) return; // 防双发
  banner.value = null;
  logs.value = [];
  busyKind.value = "pwsh";
  try {
    installReqId.value = await envDoctor.installPwsh();
    logs.value.push("$ winget install --id Microsoft.PowerShell -e");
  } catch (e) {
    busyKind.value = "";
    banner.value = { kind: "err", text: String(e) };
  }
}

async function installUv() {
  if (busyKind.value) return; // 防双发
  banner.value = null;
  logs.value = [];
  busyKind.value = "uv";
  try {
    installReqId.value = await envDoctor.installUv();
    logs.value.push(
      "$ 下载 uv release (gh-proxy 加速) → 解压到 ~/.local/bin → 写国内镜像配置"
    );
  } catch (e) {
    busyKind.value = "";
    banner.value = { kind: "err", text: String(e) };
  }
}

// uv 缓存占用 (静默查; 仅在 uv 已装时有意义)
async function loadUvCache() {
  if (!isTauri || !report.value?.uv.found) {
    uvCache.value = null;
    return;
  }
  try {
    uvCache.value = await envDoctor.uvCacheInfo();
  } catch {
    uvCache.value = null;
  }
}

async function cleanUvCache() {
  if (cleaningCache.value || busy.value) return;
  cleaningCache.value = true;
  banner.value = null;
  try {
    const msg = await envDoctor.uvCacheClean();
    banner.value = { kind: "ok", text: msg };
    await loadUvCache();
  } catch (e) {
    banner.value = { kind: "err", text: String(e) };
  } finally {
    cleaningCache.value = false;
  }
}

// 检测 Claude Code 是否有新版本 (静默, 不打扰; 仅在已安装时有意义)
async function checkClaudeUpdate() {
  if (checkingUpdate.value || busy.value) return;
  if (!report.value?.claude.found) return;
  checkingUpdate.value = true;
  try {
    updateInfo.value = await envDoctor.checkClaudeUpdate();
  } catch {
    // 检测失败不报错横幅, 静默留待用户手动点「检查更新」重试
  } finally {
    checkingUpdate.value = false;
  }
}

// 一键更新 Claude Code (走国内 npmmirror), 复用安装的流式日志管线
async function updateClaude() {
  banner.value = null;
  logs.value = [];
  busyKind.value = "claude-update";
  try {
    installReqId.value = await envDoctor.updateClaude();
    // 具体跑哪条由后端按「当初怎么装的」决定 (原生装 → claude update 就地更新; npm 装 →
    // npm i -g @latest 走国内镜像), 这里不写死, 免得日志首行跟实际跑的对不上。
    logs.value.push("$ 正在更新 Claude Code（按安装方式自动选择）…");
  } catch (e) {
    busyKind.value = "";
    banner.value = { kind: "err", text: String(e) };
  }
}

async function fixPath() {
  banner.value = null;
  busyKind.value = "path";
  try {
    const res = await envDoctor.fixPath();
    banner.value = { kind: res.ok ? "ok" : "err", text: res.message };
    await runCheck();
    // 修 PATH 直接改变「终端里能不能跑」这一结论 → 立刻重验, 让用户当场看到翻绿
    if (res.ok && verify.value) {
      verify.value = null;
      await runVerify(false);
    }
  } catch (e) {
    banner.value = { kind: "err", text: String(e) };
  } finally {
    busyKind.value = "";
  }
}

async function cancelInstall() {
  if (installReqId.value) {
    await envDoctor.cancel(installReqId.value);
  }
}

async function recheck() {
  banner.value = null;
  phase.value = "checking";
  verify.value = null; // 旧结论作废; 回到 panel 后 watch 会自动重跑快速校验
  try {
    const r = await runCheck();
    if (r.ready) localStorage.setItem(READY_FLAG, "1");
  } catch (e) {
    // 复检失败同样不能卡死在 checking:回到面板报错,按钮可再试
    banner.value = { kind: "err", text: `环境检测失败：${String(e)}，请重试。` };
  }
  phase.value = "panel";
}

function enter() {
  emit("done");
}

// 工具状态 → 状态点级别
function level(t: ToolStatus): "ok" | "warn" | "bad" {
  // PowerShell 7 是纯可选的: 只要有可用 shell(应用已内置 Git Bash)就算就绪, 不再标黄催装
  if (t.key === "pwsh" && !t.found) return report.value?.shellReady ? "ok" : "warn";
  // uv 是纯可选的加速器: 跑 Python 脚本由内置解释器打头阵, 没有 uv 也不算问题, 不标黄
  if (t.key === "uv" && !t.found) return "ok";
  if (t.found) return "ok";
  return t.required ? "bad" : "warn";
}
function statusText(t: ToolStatus): string {
  // 随安装包内置的那份: 免安装、免管理员, 如实说明来源, 不显示成「用户装好了」
  if (t.found && t.bundled) return "已就绪 · 随应用内置";
  if (t.key === "pwsh" && !t.found) {
    return report.value?.shellReady ? "可选 · 已内置 Git Bash" : "未安装 · 建议";
  }
  // uv 缺席不是缺陷: Python 走内置解释器 + pip --user 即可, uv 只是更省事
  if (t.key === "uv" && !t.found) return "可选 · 未安装（不影响使用）";
  if (t.found) return t.onPath ? "已就绪" : "已安装 (不在 PATH)";
  return t.required ? "未安装 · 必需" : "未安装 · 建议";
}

// 校验步骤 / 冲突 → 状态点级别 (复用工具清单那三档配色)
function stepLevel(s: VerifyReport["steps"][number]): "ok" | "warn" | "bad" {
  if (s.status === "ok") return "ok";
  if (s.status === "fail") return "bad";
  return "warn"; // warn / skip
}
function severityLevel(sev: string): "ok" | "warn" | "bad" {
  return sev === "high" ? "bad" : sev === "medium" ? "warn" : "ok";
}
function severityText(sev: string): string {
  return sev === "high" ? "必须处理" : sev === "medium" ? "建议处理" : "留意";
}
const conflictCount = computed(() => verify.value?.conflicts.length ?? 0);
const highConflictCount = computed(
  () => verify.value?.conflicts.filter((c) => c.severity === "high").length ?? 0
);
// 有冲突能靠「修复 PATH」解决时，冲突卡里直接给按钮，省得用户找上面那块
const hasFixableConflict = computed(
  () => !!verify.value?.conflicts.some((c) => c.fixable)
);

// 当前系统 (后端 env_check 回传): "windows" | "macos" | "linux" | "browser"
const osName = computed(() => report.value?.os ?? "");
const isWin = computed(() => osName.value === "windows");

const tools = computed<ToolStatus[]>(() => {
  if (!report.value) return [];
  const r = report.value;
  const all = [r.claude, r.pwsh, r.node, r.npm, r.uv, r.python];
  // PowerShell 7 仅 Windows 上是 Claude 的可用 shell; mac/Linux 自带 sh/zsh, 不展示该行。
  return isWin.value ? all : all.filter((t) => t.key !== "pwsh");
});
const pathNeedsFix = computed(
  () =>
    !!report.value &&
    report.value.claude.found &&
    !report.value.claudeDirOnUserPath
);

// 运行环境由镜像托管(server/容器 flavor)。后端权威字段，不靠前端猜形态。
// 容器里 env_claude_update_check 是放行的(查得出新版)，但 env_install_* / env_update_claude
// 在 apihub 层被挡 → 必须据此不渲染那些按钮，否则就是一个点了必报错的入口。
const imageManaged = computed(() => !!report.value?.imageManaged);
const dockerUpdateHint = computed(() => {
  const cur = report.value?.claude.version || updateInfo.value?.current;
  const latest = updateInfo.value?.updateAvailable ? updateInfo.value.latest : null;
  const head = cur ? `镜像内置 Claude Code ${cur}` : "Claude Code 随镜像预装";
  return latest
    ? `${head}；上游已有 ${latest}，升级请拉新镜像：docker pull`
    : `${head}；升级请拉新镜像：docker pull`;
});
</script>

<template>
  <div :class="props.gate ? 'gate' : 'page'">
    <div class="card">
      <!-- 头 -->
      <div class="badge"><span class="star"></span></div>
      <h1 class="title">环境检测与配置</h1>
      <p class="lead">
        北极星依托 <strong>Claude Code</strong> 在你本机干活。跑 Python 脚本与命令所需的
        <strong>Python、Git Bash 已随应用内置</strong>——免安装、免管理员权限，装好即用（uv 只是可选加速器，
        不装也不影响）。Claude Code 会在<strong>后台自动装好</strong>，你不用点任何东西。
      </p>

      <!-- 检测中 -->
      <div v-if="phase === 'checking'" class="checking">
        <span class="spinner"></span> 正在检测本机环境…
      </div>

      <template v-else>
        <!-- 工具清单 -->
        <ul class="tools">
          <li v-for="t in tools" :key="t.key" class="tool">
            <span class="dot" :class="level(t)"></span>
            <div class="t-main">
              <div class="t-row">
                <span class="t-name">{{ t.name }}</span>
                <span class="t-status" :class="level(t)">{{ statusText(t) }}</span>
              </div>
              <div class="t-sub">
                <span v-if="t.version" class="t-ver">{{ t.version }}</span>
                <span v-else class="t-hint">{{ t.hint }}</span>
                <span v-if="t.path" class="t-path" :title="t.path">{{ t.path }}</span>
              </div>
            </div>
            <!-- 行内动作 -->
            <div class="t-act">
              <!-- ① 镜像托管(容器/server 版) 优先于一切: 所有「安装 / 更新」在 apihub 层就被
                   挡下(见 apihub.rs 的 env_install_* / env_update_claude 分支), 给按钮等于给
                   一个点了必然报错的入口。这里只陈述事实, 升级指向 docker pull。
                   必须排在最前 —— 否则容器里万一 claude 没探到, 会先命中下面的「一键安装」。 -->
              <template v-if="imageManaged">
                <span v-if="t.key === 'claude'" class="t-managed" :title="dockerUpdateHint">
                  镜像内置
                </span>
              </template>
              <template v-else-if="t.key === 'claude' && !t.found">
                <!-- 直抓平台包 tgz (R2 → npmmirror → yarn → npmjs) 解压到 ~/.local/bin, 不经 npm -->
                <button
                  class="btn primary"
                  :disabled="busy"
                  title="下载 Claude Code 平台包（R2 → 国内镜像 → yarn → 官方，逐源自证），解压到 ~/.local/bin，不经 npm（避免卡死）"
                  @click="installClaude('direct')"
                >
                  {{ busyKind === "claude" ? "安装中…" : "一键安装" }}
                </button>
              </template>
              <!-- 已装 Claude Code: 检查 / 一键更新 (走国内镜像) -->
              <template v-else-if="t.key === 'claude' && t.found">
                <button
                  v-if="updateInfo?.updateAvailable"
                  class="btn primary"
                  :disabled="busy"
                  :title="`更新到 ${updateInfo.latest}（当前 ${updateInfo.current}）`"
                  @click="updateClaude"
                >
                  {{ busyKind === "claude-update" ? "更新中…" : `更新到 ${updateInfo.latest}` }}
                </button>
                <button
                  v-else
                  class="btn"
                  :disabled="busy || checkingUpdate"
                  :title="updateInfo?.checked ? updateInfo.message : '检查 Claude Code 是否有新版本'"
                  @click="checkClaudeUpdate"
                >
                  {{
                    checkingUpdate
                      ? "检查中…"
                      : updateInfo?.checked
                        ? "已是最新"
                        : "检查更新"
                  }}
                </button>
              </template>
              <template v-else-if="t.key === 'node' && !t.found">
                <button class="btn" :disabled="busy" @click="installNode">
                  {{ busyKind === "node" ? "安装中…" : "安装" }}
                </button>
              </template>
              <!-- PowerShell 7: 纯可选。已有可用 shell(内置 Git Bash)时不再催装,
                   只留一个弱化入口给「就是想用 pwsh」的用户 -->
              <template v-else-if="t.key === 'pwsh' && !t.found">
                <button
                  class="btn"
                  :class="{ text: report?.shellReady }"
                  :disabled="busy"
                  title="可选：Claude 已可用内置 Git Bash，装 PowerShell 7 只是多一种 shell 选择"
                  @click="installPwsh"
                >
                  {{ busyKind === "pwsh" ? "安装中…" : report?.shellReady ? "可选安装" : "安装" }}
                </button>
              </template>
              <!-- uv: 纯可选加速器。Python 脚本已由内置解释器顶着，这里只留一个弱化入口 -->
              <template v-else-if="t.key === 'uv' && !t.found">
                <button
                  class="btn text"
                  :disabled="busy"
                  title="可选：不装也能跑 Python 脚本（内置解释器 + pip --user）。装了 uv 则多一种自动隔离环境、按脚本装依赖的省事方式"
                  @click="installUv"
                >
                  {{ busyKind === "uv" ? "安装中…" : "可选安装" }}
                </button>
              </template>
            </div>
          </li>
        </ul>

        <!-- 镜像托管(容器/server 版): 说清「怎么升级」，取代那个点了必报错的更新按钮 -->
        <p v-if="imageManaged" class="alt">
          运行组件随<strong>镜像预装</strong>，面板内不做安装/更新。
          <span v-if="updateInfo?.updateAvailable">
            上游已发布 <strong>{{ updateInfo.latest }}</strong>（当前 {{ updateInfo.current }}）——
          </span>
          升级请拉新镜像：<code>docker pull</code> 后重建容器。
        </p>

        <!-- 安装 Claude 的方式说明 + 兜底 (默认直抓平台包 tgz, R2 优先多源; 官方脚本仅境外网络兜底) -->
        <p v-else-if="report && !report.claude.found" class="alt">
          默认<strong>直接下载</strong>官方平台包（<code>@anthropic-ai/claude-code</code> 原生二进制），
          四源逐个自证 <strong>R2 → npmmirror(阿里) → yarn → npmjs</strong>（各源下载+解压+试跑一次），解压到 <code>~/.local/bin</code> 即用、<strong>不经 npm</strong>（避免下载卡死）。
          <button class="link" :disabled="busy" @click="installClaude('native')">
            或改用官方脚本（境外网络{{ isWin ? "" : " · install.sh" }}）
          </button>
        </p>

        <!-- 环境变量 (PATH) 体检 -->
        <div v-if="pathNeedsFix" class="path-warn">
          <div class="pw-text">
            检测到 <strong>Claude Code 已安装但其目录不在 PATH</strong> 里——
            终端 / 重启后可能找不到 <code>claude</code>。
            <span v-if="report?.claudeDir" class="pw-dir">{{ report.claudeDir }}</span>
          </div>
          <button class="btn primary" :disabled="busy" @click="fixPath">
            {{ busyKind === "path" ? "修复中…" : "修复 PATH" }}
          </button>
        </div>

        <!-- ══ 深度校验: 安装冲突 + 实跑验证 ══
             上面那张清单只答「装没装」; 这一块答「真能不能跑、会不会打架」——
             每一条都真的起了子进程去验, 且「电脑终端里可运行」是用**新开终端的 PATH 语义**测的
             (Windows 从注册表重建机器级+用户级 PATH), 所以它跟应用内那条是两个独立结论。 -->
        <div v-if="!props.gate" class="verify">
          <div class="v-head">
            <span class="v-title">深度校验</span>
            <span v-if="verifying" class="v-sub">
              <span class="spinner sm"></span>
              {{ verifying === "deep" ? "正在真发一次请求…" : "正在实跑验证…" }}
            </span>
            <span v-else-if="verify" class="v-sub" :class="verify.ok ? 'ok' : 'warn'">
              {{ verify.summary }}
            </span>
            <span v-else class="v-sub">检测安装冲突，并实测 Claude Code 是否真能跑起来</span>
            <div class="spacer"></div>
            <button class="btn sm" :disabled="busy || !!verifying" @click="runVerify(false)">
              {{ verify ? "重新校验" : "开始校验" }}
            </button>
            <button
              class="btn sm primary"
              :disabled="busy || !!verifying || !verify?.appRunnable"
              title="真的发一次最小请求（几十 token），确证「装好了 + 能连上 + 有凭据」整条链路通"
              @click="runVerify(true)"
            >
              深度检测
            </button>
          </div>

          <template v-if="verify">
            <!-- ① 安装冲突。零冲突要明说 (否则用户以为没检测), 但**只在真验成功时才敢说**——
                 claude 都没跑起来时冲突数当然是 0, 那时说「只有一份、两边一致」是在撒谎。 -->
            <div v-if="conflictCount === 0 && verify.appRunnable" class="v-clean">
              未发现安装冲突 —— 本机只有一份 Claude Code，终端与应用跑的是同一份。
            </div>
            <ul v-else class="conflicts">
              <li v-for="c in verify.conflicts" :key="c.key" class="cf">
                <span class="dot" :class="severityLevel(c.severity)"></span>
                <div class="cf-main">
                  <div class="cf-row">
                    <span class="cf-title">{{ c.title }}</span>
                    <span class="cf-sev" :class="severityLevel(c.severity)">
                      {{ severityText(c.severity) }}
                    </span>
                  </div>
                  <div class="cf-detail">{{ c.detail }}</div>
                  <div v-for="(p, i) in c.paths" :key="i" class="cf-path" :title="p">{{ p }}</div>
                </div>
                <button
                  v-if="c.fixable"
                  class="btn sm"
                  :disabled="busy || !!verifying"
                  @click="fixPath"
                >
                  {{ busyKind === "path" ? "修复中…" : "修复 PATH" }}
                </button>
              </li>
            </ul>

            <!-- ② 实跑验证逐项 -->
            <ul class="vsteps">
              <li v-for="s in verify.steps" :key="s.key" class="vs">
                <span class="dot" :class="stepLevel(s)"></span>
                <div class="vs-main">
                  <div class="vs-row">
                    <span class="vs-name">{{ s.name }}</span>
                    <span class="vs-ms">{{ s.ms }} ms</span>
                  </div>
                  <div class="vs-detail">{{ s.detail }}</div>
                  <div v-if="s.command" class="vs-cmd" :title="s.command">$ {{ s.command }}</div>
                  <pre v-if="s.output && s.status !== 'ok'" class="vs-out">{{ s.output }}</pre>
                </div>
              </li>
            </ul>

            <p
              v-if="!verify.deep || highConflictCount > 0 || hasFixableConflict"
              class="v-foot"
            >
              <template v-if="!verify.deep">
                「深度检测」会真发一次最小请求，是确认<strong>真正可用</strong>（凭据、网络、上游全通）的唯一硬证据。
              </template>
              <template v-else-if="highConflictCount > 0">
                建议先清理上面标红的冲突，再重新校验。
              </template>
              <template v-else-if="hasFixableConflict">
                标了「修复 PATH」的那条点一下即可，新开终端后生效。
              </template>
            </p>
          </template>
        </div>

        <!-- uv 缓存治理: 装了 uv 才显示。uv 缓存放任会涨到数 GB, 给一键清理 -->
        <div v-if="report?.uv.found && uvCache?.available" class="uv-cache">
          <div class="uc-text">
            <strong>uv 缓存</strong>占用 <span class="uc-size">{{ uvCache.human }}</span>
            <span v-if="uvCache.dir" class="uc-dir" :title="uvCache.dir">{{ uvCache.dir }}</span>
          </div>
          <button
            class="btn"
            :disabled="busy || cleaningCache || uvCache.bytes === 0"
            title="uv cache clean —— 清空已下载的 Python 解释器与依赖缓存"
            @click="cleanUvCache"
          >
            {{ cleaningCache ? "清理中…" : "清理缓存" }}
          </button>
        </div>

        <!-- 流式安装日志 -->
        <div v-if="busy && logs.length" class="logwrap">
          <div class="log-head">
            <span>安装日志</span>
            <button v-if="installReqId" class="link" @click="cancelInstall">取消</button>
          </div>
          <pre class="log"><code v-for="(l, i) in logs" :key="i">{{ l }}
</code></pre>
        </div>

        <!-- 结果横幅 -->
        <div v-if="banner" class="banner" :class="banner.kind">{{ banner.text }}</div>

        <!-- 底部动作 -->
        <div class="actions">
          <button class="btn ghost" :disabled="busy" @click="recheck">重新检测</button>
          <div class="spacer"></div>
          <template v-if="props.gate">
            <button v-if="!report?.ready" class="btn text" :disabled="busy" @click="enter">
              稍后再说，先进入
            </button>
            <button class="btn primary" :disabled="busy && !report?.ready" @click="enter">
              {{ report?.ready ? "环境就绪 · 进入北极星" : "仍要进入" }}
            </button>
          </template>
        </div>

        <!-- MCP 服务配置 -->
        <div v-if="!props.gate" class="mcp-section"
        >
          <McpConfigModal inline @close="() => {}" />
        </div>
      </template>
    </div>
  </div>
</template>

<style scoped>
.gate {
  position: fixed;
  inset: 0;
  z-index: 9997;
  display: flex;
  align-items: center;
  justify-content: center;
  background: radial-gradient(120% 80% at 50% -10%, #eef2f7 0%, var(--bg) 55%);
  padding: 40px;
  overflow-y: auto;
}
.page {
  flex: 1;
  overflow-y: auto;
  padding: 40px 56px 80px;
  width: 100%;
}
.card {
  width: 100%;
  max-width: 600px;
  margin: 0 auto;
  background: var(--panel);
  border: 1px solid var(--hairline);
  border-radius: 6px;
  box-shadow: var(--shadow-lg);
  padding: 36px 40px 30px;
  animation: cardIn 0.45s cubic-bezier(0.2, 0.7, 0.2, 1);
}
.page .card {
  box-shadow: var(--shadow-sm);
}
@keyframes cardIn {
  from { opacity: 0; transform: translateY(12px); }
  to { opacity: 1; transform: translateY(0); }
}

.badge {
  display: flex;
  justify-content: center;
  margin-bottom: 18px;
}
.star {
  position: relative;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--primary);
  box-shadow: 0 0 0 4px var(--primary-soft), 0 0 18px 4px rgba(44, 70, 97, 0.25);
}
.star::before,
.star::after {
  content: "";
  position: absolute;
  left: 50%;
  top: 50%;
  background: linear-gradient(var(--g, to right), transparent, var(--primary), transparent);
}
.star::before { width: 40px; height: 1.5px; transform: translate(-50%, -50%); }
.star::after { width: 1.5px; height: 40px; transform: translate(-50%, -50%); }

.title {
  font-family: var(--serif);
  font-size: 21px;
  font-weight: 600;
  letter-spacing: 2px;
  color: var(--ink);
  text-align: center;
  margin: 0 0 14px;
}
.lead {
  font-size: 13px;
  line-height: 1.95;
  color: var(--text-2);
  margin: 0 0 22px;
  letter-spacing: 0.3px;
}
.lead strong { color: var(--ink); font-weight: 600; }

.checking {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 10px;
  padding: 30px 0;
  color: var(--muted);
  font-size: 13px;
}
.spinner {
  width: 14px;
  height: 14px;
  border: 2px solid var(--border);
  border-top-color: var(--primary);
  border-radius: 50%;
  animation: spin 0.8s linear infinite;
}
@keyframes spin { to { transform: rotate(360deg); } }

.tools {
  list-style: none;
  margin: 0 0 4px;
  padding: 0;
  border: 1px solid var(--border-soft);
  border-radius: 4px;
  overflow: hidden;
}
.tool {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px 14px;
  border-bottom: 1px solid var(--border-soft);
}
.tool:last-child { border-bottom: none; }
.dot {
  width: 9px;
  height: 9px;
  border-radius: 50%;
  flex-shrink: 0;
}
.dot.ok { background: #4a8f6d; box-shadow: 0 0 0 3px rgba(74, 143, 109, 0.15); }
.dot.warn { background: #c08a3e; box-shadow: 0 0 0 3px rgba(192, 138, 62, 0.15); }
.dot.bad { background: var(--vermilion); box-shadow: 0 0 0 3px var(--vermilion-soft); }

.t-main { flex: 1; min-width: 0; }
.t-row { display: flex; align-items: baseline; gap: 10px; }
.t-name { font-size: 13.5px; color: var(--ink); font-weight: 500; }
.t-status { font-size: 11px; letter-spacing: 0.5px; }
.t-status.ok { color: #4a8f6d; }
.t-status.warn { color: #c08a3e; }
.t-status.bad { color: var(--vermilion); }
.t-sub {
  display: flex;
  gap: 10px;
  margin-top: 2px;
  font-size: 11px;
  color: var(--muted);
  overflow: hidden;
}
.t-ver { color: var(--text-2); }
.t-path {
  font-family: var(--mono);
  color: var(--dim);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.t-act { flex-shrink: 0; }
/* 镜像托管标记：容器版没有可点的安装/更新，只标明来源（不做成按钮样式，免得像能点） */
.t-managed {
  font-size: 12px;
  color: var(--dim);
  border: 1px solid var(--border-soft);
  border-radius: 999px;
  padding: 2px 8px;
  white-space: nowrap;
  cursor: default;
}

.alt {
  font-size: 11.5px;
  color: var(--muted);
  margin: 12px 2px 0;
  line-height: 1.8;
}
.alt code {
  background: var(--code-bg);
  color: var(--code-text);
  padding: 1px 5px;
  border-radius: 3px;
  font-family: var(--mono);
  font-size: 10.5px;
}

.path-warn {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-top: 16px;
  padding: 12px 14px;
  border-radius: 4px;
  background: rgba(192, 138, 62, 0.08);
  border-left: 2px solid #c08a3e;
}
.pw-text { flex: 1; font-size: 12px; line-height: 1.7; color: var(--text-2); }
.pw-text strong { color: var(--ink); }
.pw-dir {
  display: block;
  margin-top: 3px;
  font-family: var(--mono);
  font-size: 10.5px;
  color: var(--dim);
}
.path-warn code {
  background: var(--code-bg);
  color: var(--code-text);
  padding: 0 4px;
  border-radius: 2px;
  font-family: var(--mono);
}

.uv-cache {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-top: 14px;
  padding: 10px 14px;
  border-radius: 4px;
  background: var(--bg-soft);
  border: 1px solid var(--border-soft);
}
.uc-text { flex: 1; font-size: 12px; line-height: 1.6; color: var(--text-2); min-width: 0; }
.uc-text strong { color: var(--ink); font-weight: 600; }
.uc-size { color: var(--ink); font-family: var(--mono); }
.uc-dir {
  display: block;
  margin-top: 2px;
  font-family: var(--mono);
  font-size: 10.5px;
  color: var(--dim);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* ── 深度校验 ── */
.verify {
  margin-top: 18px;
  border: 1px solid var(--border-soft);
  border-radius: 4px;
  overflow: hidden;
}
.v-head {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 9px 12px;
  background: var(--bg-soft);
  border-bottom: 1px solid var(--border-soft);
  flex-wrap: wrap;
}
.v-title {
  font-family: var(--serif);
  font-size: 12.5px;
  letter-spacing: 1px;
  color: var(--ink);
  white-space: nowrap;
}
.v-sub {
  font-size: 11.5px;
  color: var(--muted);
  line-height: 1.6;
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
}
.v-sub.ok { color: #4a8f6d; }
.v-sub.warn { color: #c08a3e; }
.spinner.sm { width: 11px; height: 11px; border-width: 1.5px; }
.btn.sm { padding: 5px 11px; font-size: 11.5px; }

.v-clean {
  padding: 10px 14px;
  font-size: 12px;
  line-height: 1.7;
  color: #4a8f6d;
  border-bottom: 1px solid var(--border-soft);
}

.conflicts,
.vsteps {
  list-style: none;
  margin: 0;
  padding: 0;
}
.cf,
.vs {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  padding: 11px 14px;
  border-bottom: 1px solid var(--border-soft);
}
.vsteps .vs:last-child { border-bottom: none; }
.cf .dot,
.vs .dot { margin-top: 4px; }
.cf-main,
.vs-main { flex: 1; min-width: 0; }
.cf-row,
.vs-row { display: flex; align-items: baseline; gap: 10px; }
.cf-title { font-size: 13px; color: var(--ink); font-weight: 500; }
.cf-sev { font-size: 11px; letter-spacing: 0.5px; }
.cf-sev.ok { color: #4a8f6d; }
.cf-sev.warn { color: #c08a3e; }
.cf-sev.bad { color: var(--vermilion); }
.cf-detail,
.vs-detail {
  margin-top: 3px;
  font-size: 11.5px;
  line-height: 1.75;
  color: var(--text-2);
}
.cf-path,
.vs-cmd {
  margin-top: 3px;
  font-family: var(--mono);
  font-size: 10.5px;
  color: var(--dim);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.vs-name { font-size: 13px; color: var(--ink); font-weight: 500; }
.vs-ms { font-size: 10.5px; color: var(--dim); font-family: var(--mono); }
.vs-out {
  margin: 6px 0 0;
  padding: 7px 9px;
  border-radius: 3px;
  background: #0c1320;
  color: #c8d4e6;
  font-family: var(--mono);
  font-size: 10.5px;
  line-height: 1.6;
  max-height: 120px;
  overflow: auto;
  white-space: pre-wrap;
  word-break: break-all;
}
.v-foot {
  margin: 0;
  padding: 9px 14px;
  border-top: 1px solid var(--border-soft);
  background: var(--bg-soft);
  font-size: 11px;
  line-height: 1.75;
  color: var(--muted);
}
.v-foot strong { color: var(--ink); }

.logwrap {
  margin-top: 16px;
  border: 1px solid var(--border-soft);
  border-radius: 4px;
  overflow: hidden;
}
.log-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 6px 12px;
  background: var(--bg-soft);
  font-size: 11px;
  letter-spacing: 1px;
  color: var(--dim);
  font-family: var(--serif);
}
.log {
  margin: 0;
  padding: 10px 12px;
  max-height: 220px;
  overflow-y: auto;
  background: #0c1320;
  color: #c8d4e6;
  font-family: var(--mono);
  font-size: 11px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-all;
}
.log code { background: transparent; color: inherit; }

.banner {
  margin-top: 16px;
  padding: 9px 13px;
  border-radius: 3px;
  font-size: 12.5px;
  line-height: 1.7;
  white-space: pre-wrap;
}
.banner.ok {
  background: var(--primary-soft);
  color: var(--primary-deep);
  border-left: 2px solid var(--primary);
}
.banner.err {
  background: var(--vermilion-soft);
  color: var(--vermilion);
  border-left: 2px solid var(--vermilion);
}
.banner.info {
  background: var(--selection-bg);
  color: var(--text-2);
  border-left: 2px solid var(--border);
}

.actions {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-top: 26px;
}
.spacer { flex: 1; }

.btn {
  padding: 8px 16px;
  border-radius: 3px;
  font-size: 12.5px;
  letter-spacing: 0.5px;
  border: 1px solid var(--border);
  background: transparent;
  color: var(--text-2);
  cursor: pointer;
}
.btn:hover:not(:disabled) { border-color: var(--ink); color: var(--ink); }
.btn.primary { background: var(--btn-solid-bg); color: var(--btn-solid-text); border-color: var(--btn-solid-bg); }
.btn.primary:hover:not(:disabled) { background: var(--primary); border-color: var(--primary); }
.btn.ghost { color: var(--text-2); }
.btn.text { border-color: transparent; color: var(--muted); }
.btn.text:hover:not(:disabled) { color: var(--ink); }
.btn:disabled { opacity: 0.5; cursor: not-allowed; }

.link {
  background: transparent;
  border: none;
  color: var(--primary);
  font-size: 11.5px;
  cursor: pointer;
  padding: 0;
}
.link:hover:not(:disabled) { text-decoration: underline; }

.mcp-section {
  margin-top: 28px;
  padding-top: 20px;
  border-top: 1px solid var(--border-soft);
}
.link:disabled { opacity: 0.4; cursor: not-allowed; }
</style>
