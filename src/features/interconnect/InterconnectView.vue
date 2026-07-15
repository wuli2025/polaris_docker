<script setup lang="ts">
/**
 * 互联 · 设备联盟 —— 与「协作(项目/任务)」彻底分开的独立板块。
 *
 * 升级方案 v3「互联层重塑」的前端落地:把散乱的连法收进三个 Tab,苹果玻璃琉璃质感。
 *   ① 教程   —— 接入三步 + 连接码 + 局域网直连 + 账号根口令(PRD 原型①)
 *   ② 设备与授权 —— 设备联盟卡(传输徽标 / 挂盘 / 派任务 / 吊销)(PRD 原型②④)
 *   ③ 网络拓扑 —— 本机为心、设备为星的关系图,连接路径只作可视化,不需用户选
 *
 * 传输隐形:用户只面对「连谁 / 用它的什么」,局域网/P2P/中继由系统自动选档,
 * 界面上只作徽标展示。点「派任务」= 先建立远程连接(自动选路)再下发。
 */
import { computed, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import {
  MonitorSmartphone,
  Server,
  Copy,
  RefreshCw,
  LoaderCircle,
  ShieldOff,
  Radio,
  Laptop,
  Smartphone,
  HardDrive,
  GraduationCap,
  Network,
  Send,
  Wifi,
  Zap,
  Globe,
  FolderInput,
  ShieldCheck,
  Cpu,
  Plus,
  LogOut,
} from "@lucide/vue";
import { invoke, isTauri } from "../../tauri";
import { useCollabStore } from "../collab/stores/collab";
import { useAppStore } from "../../stores/app";
import { collabApi, type AdminDevice, type AuditRow } from "../collab/api";
import { toast } from "../../composables/useToast";
import {
  loadRemoteSources,
  upsertRemoteSource,
  removeRemoteSource,
  type RemoteSource,
} from "./remoteSources";
// 联盟账号头像(codex 生成的可爱卡通吉祥物 SVG,内联进左栏)
import allianceAvatar from "./alliance-avatar.svg?raw";

const collab = useCollabStore();
const app = useAppStore();

const owner = computed(() => (collab.user?.role ?? "") === "owner");
const authed = computed(() => collab.authed);
const needsBootstrap = computed(() => !!collab.hostInfo?.needsBootstrap);
// 桌面(Tauri):本机是否已「设为主机」由 collab_host_status 决定。
// 浏览器 / Docker server 版:你本就在跟一台主机(server)对话,视作「已是主机」,
// 直接亮连接信息,而不是让用户去点「设为主机」。
const hostRunning = computed(() =>
  isTauri ? !!collab.hostInfo?.running : true
);
// 三态:①未设为主机(仅桌面) ②已是主机但未认证(仅桌面,需注册/登录管理者) ③可出连接码
const showBecomeHost = computed(() => isTauri && !hostRunning.value);
const showHostAuth = computed(() => isTauri && hostRunning.value && !authed.value);
const showConnect = computed(() => hostRunning.value && (authed.value || !isTauri));

// ── 顶部 Tab ──
type Tab = "guide" | "devices" | "topo";
const tab = ref<Tab>("guide");
const TABS: { key: Tab; label: string; icon: unknown }[] = [
  { key: "guide", label: "远程连接教程", icon: GraduationCap },
  { key: "devices", label: "设备与授权", icon: MonitorSmartphone },
  { key: "topo", label: "网络拓扑", icon: Network },
];

// ── 桌面:设为主机后注册/登录管理者(互联板块自足,不用去协作) ──
const authForm = reactive({ username: "", password: "", displayName: "" });
const authBusy = ref(false);
const authErr = ref("");
async function doHostAuth() {
  authErr.value = "";
  const u = authForm.username.trim();
  if (!u || !authForm.password) {
    authErr.value = "请填写用户名和密码";
    return;
  }
  authBusy.value = true;
  try {
    if (needsBootstrap.value) {
      await collab.bootstrap(u, authForm.password, authForm.displayName.trim() || u, true);
    } else {
      await collab.login(u, authForm.password);
    }
    authForm.password = "";
    await refreshAll();
  } catch (e) {
    authErr.value = (e as Error).message;
  } finally {
    authBusy.value = false;
  }
}

// ── 主机连接:owner 令牌(手机以完整权限连上本机的凭据) ──
const tokenRevealed = ref(false);
const tokenCopied = ref(false);
const maskedToken = computed(() => {
  const t = collab.token || "";
  return t
    ? t.slice(0, 4) + "•".repeat(Math.max(4, t.length - 8)) + t.slice(-4)
    : "";
});
async function copyToken() {
  const t = collab.token;
  if (!t) return;
  try {
    await navigator.clipboard.writeText(t);
    tokenCopied.value = true;
    setTimeout(() => (tokenCopied.value = false), 1500);
    toast.info("owner 令牌已复制");
  } catch {
    toast.error("复制失败,请手动选中");
  }
}

// PLRK1 连接码:把「本机地址 + owner 令牌」打包成一串(base64url),同账号自己的设备
// 粘这一串即以 owner 完整权限连上,不用分别填地址和令牌。仅供你自己的设备用。
const connectCode = computed(() => {
  const t = collab.token || "";
  const a = collab.hostInfo?.urls ?? [];
  const n = collab.hostInfo?.nodeId ?? ""; // iroh 主机 NodeId → 手机打洞 P2P 直连
  if (!t) return "";
  try {
    const payload: Record<string, unknown> = { t, a };
    if (n) payload.n = n;
    const b64 = btoa(JSON.stringify(payload))
      .replace(/\+/g, "-")
      .replace(/\//g, "_")
      .replace(/=+$/, "");
    return "PLRK1-" + b64;
  } catch {
    return "";
  }
});
const hasIroh = computed(() => !!collab.hostInfo?.nodeId);
const codeCopied = ref(false);
const showManual = ref(false);
async function copyConnectCode() {
  if (!connectCode.value) return;
  try {
    await navigator.clipboard.writeText(connectCode.value);
    codeCopied.value = true;
    setTimeout(() => (codeCopied.value = false), 1500);
    toast.info("连接码已复制,去手机 App 粘贴即可");
  } catch {
    toast.error("复制失败,请手动选中");
  }
}

// ── 局域网直连开关(手机走 WiFi 连的最后一环) ──
const remoteOn = computed(() => !!collab.hostInfo?.remoteAccess);
const lanBusy = ref(false);
async function toggleRemote() {
  if (!isTauri || lanBusy.value) return;
  const target = !remoteOn.value;
  lanBusy.value = true;
  try {
    await collab.hostSetRemoteAccess(target); // 后端重启主机重绑,urls 随之变
    await refreshAll();
    toast.info(target ? "已开启局域网直连:连接码已带上局域网 IP,同一 WiFi 的手机可连" : "已关闭:主机回到仅本机可连");
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    lanBusy.value = false;
  }
}

async function becomeHost() {
  try {
    await collab.hostStart();
    toast.info("本机已设为主机");
    await refreshAll();
  } catch (e) {
    toast.error(`设为主机失败:${(e as Error).message}`);
  }
}

// ── 设备看板 ──
const devices = ref<AdminDevice[]>([]);
const devicesLoading = ref(false);
async function loadDevices(silent = false) {
  if (!silent) devicesLoading.value = true;
  try {
    devices.value = await collabApi.adminDevices();
  } catch {
    if (!silent) devices.value = []; // 静默刷新失败保留旧数据,不闪空
  } finally {
    devicesLoading.value = false;
  }
}
async function revoke(d: AdminDevice) {
  if (!confirm(`吊销设备「${d.name || d.node_id || d.id}」?该设备将无法再连入。`)) return;
  try {
    await collabApi.adminDeviceRevoke(d.id);
    toast.info("已吊销");
    await loadDevices();
  } catch (e) {
    toast.error((e as Error).message);
  }
}
function devIcon(d: AdminDevice) {
  const n = (d.name || "").toLowerCase();
  if (d.is_host) return Server;
  if (/phone|手机|android|ios|mobile|pixel|iphone/.test(n)) return Smartphone;
  if (/nas|群晖|synology|server/.test(n)) return HardDrive;
  return Laptop;
}
function shortNode(id?: string) {
  if (!id) return "—";
  return id.length > 16 ? `${id.slice(0, 8)}…${id.slice(-6)}` : id;
}

// ── 传输选档(可视化):局域网直连 / P2P 打洞 / 中继兜底 ──
// 没有 per-device 心跳前,按本机当前实际连法推断徽标(host 有 iroh NodeId → 走 P2P;
// 仅开局域网 → LAN)。这是「系统自动选了哪一档」的如实展示,不是编造的实时指标。
type Transport = "local" | "lan" | "p2p" | "relay" | "disk";
const TRANSPORT: Record<Transport, { label: string; icon: unknown; cls: string }> = {
  local: { label: "本机", icon: Server, cls: "t-local" },
  lan: { label: "局域网直连", icon: Wifi, cls: "t-lan" },
  p2p: { label: "P2P 打洞", icon: Zap, cls: "t-p2p" },
  relay: { label: "中继兜底", icon: Globe, cls: "t-relay" },
  disk: { label: "远程盘", icon: HardDrive, cls: "t-disk" },
};
function devTransport(d: AdminDevice): Transport {
  if (d.is_host) return "local";
  if (d.revoked) return "relay";
  if (hasIroh.value) return "p2p";
  if (remoteOn.value) return "lan";
  return "relay";
}

const hostDevice = computed(() => devices.value.find((d) => d.is_host) || null);
const remoteDevices = computed(() => devices.value.filter((d) => !d.is_host));
const onlineCount = computed(() => devices.value.filter((d) => !d.revoked).length);

// ── 设备联盟:本机真实遥测(CPU/内存/磁盘)。远端设备的同款数据由各自 Polaris 上报(Phase2)。──
interface DeviceStats {
  cpu_pct: number;
  mem_used: number;
  mem_total: number;
  disk_used: number;
  disk_total: number;
  cores: number;
}
const localStats = ref<DeviceStats | null>(null);
let statTimer: ReturnType<typeof setInterval> | null = null;
async function sampleLocal() {
  if (!isTauri) return;
  try {
    localStats.value = await invoke<DeviceStats>("sys_stats");
  } catch {
    /* 采样失败静默,下次再采 */
  }
}
/** 设备的资源数据:本机=真实采样;远端=遥测最近一帧(手机等自己上报,字段按能力可缺)。 */
function statsFor(d: AdminDevice): Partial<DeviceStats> | null {
  if (d.is_host) return localStats.value;
  return d.stats ?? null;
}
/** 遥测新鲜度:超 90s 没上报视为过期(卡上标灰,不冒充实时)。 */
function statsStale(d: AdminDevice): boolean {
  if (d.is_host) return false;
  return !d.stats_at || Date.now() - d.stats_at > 90_000;
}
function pctOf(used: number, total: number): number {
  return total > 0 ? Math.min(100, Math.round((used / total) * 100)) : 0;
}
/** 字节 → 人类可读:≥1024G 用 T,否则 G(≥10 取整,否则 1 位小数)。 */
function fmtSize(bytes: number): string {
  const g = bytes / 1024 ** 3;
  if (g >= 1024) return (g / 1024).toFixed(1) + "T";
  return (g >= 10 ? Math.round(g).toString() : g.toFixed(1)) + "G";
}
/** 仪表条颜色:占用越高越暖(≥85% 红、≥60% 橙、否则蓝绿)。 */
function meterCls(p: number): string {
  return p >= 85 ? "m-hot" : p >= 60 ? "m-warm" : "m-cool";
}
/** 部分字段的 stats → 仪表行(设备能报什么画什么,缺的不编)。 */
function metersOf(s: Partial<DeviceStats> | null): { k: string; p: number; v: string }[] {
  if (!s) return [];
  const rows: { k: string; p: number; v: string }[] = [];
  if (typeof s.cpu_pct === "number") {
    const c = Math.round(s.cpu_pct);
    rows.push({ k: "CPU", p: c, v: c + "%" });
  }
  if (s.mem_total) {
    const used = s.mem_used ?? 0;
    rows.push({
      k: "内存",
      p: s.mem_used != null ? pctOf(used, s.mem_total) : 0,
      v: s.mem_used != null ? fmtSize(used) + "/" + fmtSize(s.mem_total) : fmtSize(s.mem_total),
    });
  }
  if (s.disk_total) {
    rows.push({
      k: "磁盘",
      p: pctOf(s.disk_used ?? 0, s.disk_total),
      v: fmtSize(s.disk_used ?? 0) + "/" + fmtSize(s.disk_total),
    });
  }
  return rows;
}
/** 设备卡三条仪表;无任何数据(远端未上报)返回 null。 */
function metersFor(d: AdminDevice): { k: string; p: number; v: string }[] | null {
  const rows = metersOf(statsFor(d));
  return rows.length ? rows : null;
}
function coresFor(d: AdminDevice): number | null {
  return statsFor(d)?.cores ?? null;
}

// ── 子导航:我的设备 / 我共享出去的 / 我能用的(出站盘)/ 正在发生 ──
type DevFilter = "mine" | "shared" | "usable" | "activity";
const devFilter = ref<DevFilter>("mine");
const DEV_FILTERS = computed(() => [
  // 我的设备 = 本机 + 已登记设备 + 已连接的 NAS/远程盘(用户要求 NAS 归到这里)
  { key: "mine" as DevFilter, label: "我的设备", icon: MonitorSmartphone, n: 1 + remoteDevices.value.length + remotes.value.length },
  { key: "shared" as DevFilter, label: "我共享出去的", icon: ShieldCheck, n: 0 },
  { key: "usable" as DevFilter, label: "我能用的", icon: HardDrive, n: 0 },
  { key: "activity" as DevFilter, label: "正在发生", icon: Radio, n: auditRows.value.length },
]);

// ── 「我的设备」统一卡片:本机 + 已登记设备 + 已连接 NAS/远程盘,一套状态卡 ──
interface UnifiedCard {
  key: string;
  kind: "host" | "device" | "disk";
  name: string;
  sub: string;
  transport: Transport;
  cores: number | null;
  stats: Partial<DeviceStats> | null;
  stale: boolean;
  icon: unknown;
  dev?: AdminDevice;
  src?: RemoteSource;
  revoked?: boolean;
}
const mineItems = computed<UnifiedCard[]>(() => {
  const out: UnifiedCard[] = [];
  // ① 本机(这台电脑),真实采样
  out.push({
    key: "host-self",
    kind: "host",
    name: hostDevice.value?.name || (collab.user?.username ? "@" + collab.user.username : "这台电脑"),
    sub: "本机 · 全权",
    transport: "local",
    cores: localStats.value?.cores ?? null,
    stats: localStats.value,
    stale: false,
    icon: Server,
  });
  // ② 已登记设备(手机等),遥测
  for (const d of remoteDevices.value) {
    out.push({
      key: "dev-" + d.id,
      kind: "device",
      name: d.name || shortNode(d.node_id),
      sub: d.username ? "@" + d.username : "用户 #" + d.user_id,
      transport: devTransport(d),
      cores: coresFor(d),
      stats: statsFor(d),
      stale: statsStale(d),
      icon: devIcon(d),
      dev: d,
      revoked: d.revoked,
    });
  }
  // ③ 已连接的 NAS/远程盘,经隧道拉的实况(拉不到时旧值标灰,不清空)
  for (const s of remotes.value) {
    out.push({
      key: "disk-" + s.id,
      kind: "disk",
      name: s.name || "远程盘",
      sub: "远程盘 · 127.0.0.1:" + s.port,
      transport: "p2p", // iroh 打洞直连,如实标 P2P(与拓扑一致)
      cores: remoteStats.value[s.id]?.cores ?? null,
      stats: remoteStats.value[s.id] ?? null,
      stale: diskStale(s.id),
      icon: HardDrive,
      src: s,
    });
  }
  return out;
});

// ── 账号:登录 / 登出(桌面互联页也能切账号) ──
const showLogin = ref(false);
async function doLogout() {
  if (!confirm("登出当前账号?本机主机继续运行,重新登录即可管理。")) return;
  try {
    await collab.logout();
    toast.info("已登出");
    await refreshAll();
  } catch (e) {
    toast.error((e as Error).message);
  }
}
async function submitLogin() {
  await doHostAuth();
  if (!authErr.value) showLogin.value = false;
}
/** 使用盘:带着目标远程源直接跳文件中心的远程浏览(FileCenter onMounted 接棒)。 */
function browseDisk(s: RemoteSource) {
  sessionStorage.setItem("polaris.fc.openRemote", s.id);
  app.setView("file_center");
}

// ── 远程盘(我能用的)实况:经 iroh 隧道调对端 sys_stats,真数据;对端老版本无此命令→缺项 ──
const remoteStats = ref<Record<string, Partial<DeviceStats>>>({});
/** 每块盘最近一次成功拉到实况的时间:决定「新鲜/标灰」,失败不清值只变灰。 */
const remoteStatsAt = ref<Record<string, number>>({});
let remotePolling = false;
async function pollRemoteStats() {
  if (remotePolling) return; // 单飞:tick 与手动可能重入,别叠(codex #4)
  remotePolling = true;
  // 修剪已移除远程盘的残留键,防 map 无限膨胀。
  const liveIds = new Set(remotes.value.map((s) => s.id));
  for (const k of Object.keys(remoteStats.value)) {
    if (!liveIds.has(k)) {
      delete remoteStats.value[k];
      delete remoteStatsAt.value[k];
    }
  }
  try {
    await Promise.all(
      remotes.value.map(async (s) => {
        const ctl = new AbortController();
        const t = setTimeout(() => ctl.abort(), 6000); // 隧道半死时别挂死(覆盖到 json() 完成)
        try {
          const r = await fetch(`http://127.0.0.1:${s.port}/api/invoke`, {
            method: "POST",
            headers: {
              "content-type": "application/json",
              ...(s.token ? { Authorization: `Bearer ${s.token}` } : {}),
            },
            body: JSON.stringify({ cmd: "sys_stats", args: {} }),
            signal: ctl.signal,
          });
          if (!r.ok) throw new Error(String(r.status));
          const stats = await r.json();
          remoteStats.value = { ...remoteStats.value, [s.id]: stats };
          remoteStatsAt.value = { ...remoteStatsAt.value, [s.id]: Date.now() };
        } catch {
          // 断线/隧道重连中:**保留旧值只标灰**(remoteStatsAt 不更新 → stale)。
          // 之前失败即删导致卡片频繁退回「待上报」,体验像坏了 —— 灰值+时间戳更如实。
        } finally {
          clearTimeout(t);
        }
      })
    );
  } finally {
    remotePolling = false;
  }
}
/** 盘实况是否过期(>40s 没拉到新帧,多半在隧道重连)。 */
function diskStale(id: string): boolean {
  return !!remoteStats.value[id] && Date.now() - (remoteStatsAt.value[id] ?? 0) > 40_000;
}

// ── 正在发生:audit 活动流(接入/上报/吊销/账号事件,主机留痕) ──
const auditRows = ref<AuditRow[]>([]);
const auditLoading = ref(false);
async function loadAudit(silent = false) {
  if (!silent) auditLoading.value = true;
  try {
    auditRows.value = await collabApi.adminAudit(50);
  } catch {
    if (!silent) auditRows.value = []; // 静默刷新失败保留旧数据
  } finally {
    auditLoading.value = false;
  }
}
const AUDIT_LABEL: Record<string, string> = {
  "auth.login": "设备登录接入",
  "user.create": "新账号创建",
  "user.disable": "账号停用",
  "device.telemetry": "设备开始上报资源",
  "device.revoke": "设备被吊销",
  "mirror.export": "账号镜像导出",
};
function auditLabel(a: AuditRow): string {
  return AUDIT_LABEL[a.action] ?? a.action;
}
/** Unix 秒 → 相对时间。 */
function relTime(atSec: number): string {
  const d = Date.now() - atSec * 1000;
  if (d < 60_000) return "刚刚";
  if (d < 3_600_000) return `${Math.floor(d / 60_000)} 分钟前`;
  if (d < 86_400_000) return `${Math.floor(d / 3_600_000)} 小时前`;
  return `${Math.floor(d / 86_400_000)} 天前`;
}

// 进对应子页即取数;设备页周期静默刷新让遥测仪表流动。
// NAS/远程盘现归「我的设备」,故 mine 也要拉远程盘实况。
watch(devFilter, (f) => {
  if (f === "activity") loadAudit();
  if (f === "mine") pollRemoteStats();
});
// 从教程/拓扑切回设备页:立刻补拉一轮(不等下个 tick),避免"切回来是空的"。
watch(tab, (t) => {
  if (t === "devices") {
    pollRemoteStats();
    loadDevices(true);
  }
});

// ── 派任务 = 左侧新建一个「标记了目标设备」的对话 ──
// 点派任务:主对话系统里 createConversation → 标题打上 @设备名 → 切到对话页,
// 你在里面下指令,就是发给那台机器的任务(真正跨设备下发执行走 Phase 3.5)。
const dispatchBusy = ref(false);
async function openDispatch(d: AdminDevice) {
  if (dispatchBusy.value) return;
  dispatchBusy.value = true;
  const label = d.name || shortNode(d.node_id);
  try {
    let pid = app.currentProjectId ?? app.projects[0]?.id ?? null;
    if (!pid) {
      const p = await app.createProject("设备任务");
      pid = p.id;
    }
    const conv = await app.createConversation(pid, true); // 切到对话视图
    await app.renameConversation(conv, `@${label} · 派任务`);
    toast.info(`已开对话「@${label}」—— 在这里下指令,就是派给它的任务`);
  } catch (e) {
    toast.error(`开任务对话失败:${(e as Error).message}`);
  } finally {
    dispatchBusy.value = false;
  }
}

// ── 挂盘(远程盘):P2 路线图,先给出连接与说明 ──
function mountDisk(d: AdminDevice) {
  toast.info(`正在把「${d.name || "远程设备"}」的盘挂为本机盘 —— 远程盘(WebDAV)接入中,先用「派任务」验证连接`);
}

// ── 网络拓扑几何:本机为心,远端设备/远程盘环绕成星 ──
// 两类节点:① 入站设备(手机等主动连进本机做主机);② 出站远程盘(本机拨出去连的
// NAS/远程源,走 iroh 隧道,登记在文件中心)。后者原来只在文件中心可见,这里也画进星图。
interface TopoEntity {
  kind: "device" | "disk";
  name: string;
  emoji: string;
  revoked: boolean;
  nodeId: string;
  t: Transport;
}
const topoEntities = computed<TopoEntity[]>(() => {
  const devs: TopoEntity[] = remoteDevices.value.map((d) => ({
    kind: "device",
    name: d.name || shortNode(d.node_id),
    emoji: /phone|手机|android|ios|iphone/i.test(d.name || "")
      ? "📱"
      : /nas|群晖|synology/i.test(d.name || "")
        ? "🗄"
        : "💻",
    revoked: !!d.revoked,
    nodeId: d.node_id || "",
    t: devTransport(d),
  }));
  // NAS/远程盘走的就是 iroh 打洞直连 → 链路如实标 P2P(蓝),不再单列「远程盘」色。
  const disks: TopoEntity[] = remotes.value.map((s) => ({
    kind: "disk",
    name: s.name || "远程盘",
    emoji: "🗄",
    revoked: false,
    nodeId: s.nodeId || "",
    t: "p2p",
  }));
  return [...devs, ...disks];
});
const topo = computed(() => {
  const W = 600;
  const H = 380;
  const cx = W / 2;
  const cy = H / 2;
  const R = 132;
  const list = topoEntities.value;
  const n = list.length;
  const nodes = list.map((e, i) => {
    // 从正上方起,均匀铺开;单个时略偏右上更自然
    const deg = n === 1 ? -50 : -90 + (i * 360) / n;
    const rad = (deg * Math.PI) / 180;
    return {
      ...e,
      x: cx + R * Math.cos(rad),
      y: cy + R * Math.sin(rad),
    };
  });
  return { W, H, cx, cy, R, nodes };
});

// ── 接入远程主机 / NAS(iroh P2P):填 NodeId + owner 令牌 → 起隧道 → 登记为远程源 ──
// 隧道在 127.0.0.1:port 起透明代理,「文件中心 · 远程源」经该端口浏览对端的盘。
const remotes = ref<RemoteSource[]>(loadRemoteSources());
const addForm = reactive({ name: "群晖 NAS", nodeId: "", token: "", open: false });
let portSeq = 18620; // 本地代理端口起点,逐个 +1 避冲突
const connBusy = ref(false);

async function connectRemote() {
  const nodeId = addForm.nodeId.trim();
  if (!nodeId) {
    toast.error("请填 NAS 的 iroh NodeId(或连接码)");
    return;
  }
  if (!isTauri) {
    toast.error("远程主机接入需在桌面 App 内(iroh 隧道)");
    return;
  }
  // 端口在已用远程源之上取,避免与既有隧道撞。
  const used = new Set(remotes.value.map((r) => r.port));
  while (used.has(portSeq)) portSeq++;
  const port = portSeq++;
  connBusy.value = true;
  try {
    await invoke("collab_tunnel_connect", { hostNodeId: nodeId, listenPort: port });
    const src: RemoteSource = {
      id: `rs-${Date.now()}`,
      name: addForm.name.trim() || "远程主机",
      nodeId,
      port,
      token: addForm.token.trim(),
      createdAt: Date.now(),
    };
    remotes.value = upsertRemoteSource(src);
    addForm.open = false;
    addForm.nodeId = "";
    addForm.token = "";
    toast.info(`已发起 iroh P2P 连接「${src.name}」—— 到「文件中心 · 远程源」浏览它的盘`);
  } catch (e) {
    toast.error(`连接失败:${(e as Error).message}`);
  } finally {
    connBusy.value = false;
  }
}
function forgetRemote(s: RemoteSource) {
  if (!confirm(`断开并移除「${s.name}」?`)) return;
  remotes.value = removeRemoteSource(s.id);
  toast.info("已移除远程源");
}

async function refreshAll() {
  remotes.value = loadRemoteSources(); // 文件中心新接入的远程盘同步进拓扑
  if (isTauri) await collab.hostStatus();
  await loadDevices();
}

// 「连接码只粘一次」:已保存的远程盘(NAS 等)在启动时自动重建 iroh 隧道,
// 用户无需再粘码。端口沿用保存时的值,失败静默(下次刷新/手动再连)。
async function autoReconnectRemotes() {
  if (!isTauri) return;
  for (const s of loadRemoteSources()) {
    try {
      await invoke("collab_tunnel_connect", { hostNodeId: s.nodeId, listenPort: s.port });
    } catch {
      /* 单台失败不阻断其它;拓扑里仍显示,浏览时按需重连 */
    }
  }
}

onMounted(async () => {
  await refreshAll();
  // 流程顺滑:已经有设备/远程盘的老用户,进来直接看设备看板,不用每次翻过教程。
  if (tab.value === "guide" && (remoteDevices.value.length || remotes.value.length)) {
    tab.value = "devices";
  }
  await autoReconnectRemotes();
  await sampleLocal();
  pollRemoteStats(); // 首屏就拉一次 NAS/远程盘实况(它们在「我的设备」里)
  // 本机仪表每 4s 跳一帧。盘实况:启动后前 ~40s 每 4s 密集试(iroh 握手要几秒,
  // 首拉大概率没通,别让用户等 12s 才看到数字),之后回落到每 12s 一次。
  let tick = 0;
  let warmup = 10; // 前 10 拍(40s)密集拉
  statTimer = setInterval(() => {
    sampleLocal();
    tick++;
    const due = warmup > 0 ? true : tick % 3 === 0;
    if (warmup > 0) warmup--;
    if (due && tab.value === "devices") {
      pollRemoteStats(); // 盘实况(任何分区都拉:mine 默认显示它们)
      if (tick % 3 === 0) {
        if (devFilter.value === "mine") loadDevices(true);
        else if (devFilter.value === "activity") loadAudit(true); // 停在活动页也持续刷新(codex #7)
      }
    }
    // 兜底:从没成功拉到过实况的盘(初始 collab_tunnel_connect 可能整个失败,比如
    // 启动时断网),每 ~36s 重新发起一次隧道(端口已被占=隧道其实活着,静默吃掉)。
    if (isTauri && tick % 9 === 0) {
      for (const s of remotes.value) {
        if (remoteStatsAt.value[s.id]) continue;
        invoke("collab_tunnel_connect", { hostNodeId: s.nodeId, listenPort: s.port }).catch(() => {});
      }
    }
  }, 4000);
});
onUnmounted(() => {
  if (statTimer) clearInterval(statTimer);
});
</script>

<template>
  <div class="interconnect">
    <header class="bar">
      <div class="ttl"><Radio :size="17" :stroke-width="1.8" /> 互联 · 设备联盟</div>
      <nav class="tabs">
        <button
          v-for="t in TABS"
          :key="t.key"
          class="tab"
          :class="{ on: tab === t.key }"
          @click="tab = t.key"
        >
          <component :is="t.icon" :size="14" :stroke-width="1.9" />
          <span class="tab-label">{{ t.label }}</span>
        </button>
      </nav>
      <button class="icobtn" title="刷新" @click="refreshAll"><RefreshCw :size="15" /></button>
    </header>

    <div class="scroll">
      <!-- ════════════ ① 远程连接教程 ════════════ -->
      <template v-if="tab === 'guide'">
        <!-- 接入三步(传输隐形宣言) -->
        <section class="glass steps-card">
          <div class="sc-head">
            <span class="sc-kick">接入联盟 · 三步</span>
            <h2 class="sc-title">一个口令,设备<span class="grad">自动入网</span></h2>
            <p class="sc-sub">你只选「连谁、用它的什么」。局域网 / P2P 打洞 / 中继兜底由系统自动选,你永不用管。</p>
          </div>
          <ol class="steps">
            <li>
              <span class="st-n">1</span>
              <div><b>这台电脑生成连接码</b><p>下方一串即是。桌面版会自动带上 iroh P2P 直连能力。</p></div>
            </li>
            <li>
              <span class="st-n">2</span>
              <div><b>新设备粘一下</b><p>手机 / 另一台电脑装好 Polaris,把连接码粘进去 —— 不用填地址、不用选连法。</p></div>
            </li>
            <li>
              <span class="st-n">3</span>
              <div><b>传输自动选路</b><p>同一 WiFi 走局域网直连;跨网自动 P2P 打洞;打不通中继兜底。连上就能挂盘、派任务。</p></div>
            </li>
          </ol>
        </section>

        <!-- 主机连接卡:app 该填什么,一次看清 -->
        <section class="glass hero">
          <div class="hero-head">
            <MonitorSmartphone :size="18" :stroke-width="1.8" />
            <span>让手机 / 其它设备连上这台电脑</span>
          </div>

          <template v-if="showConnect">
            <div class="hint">
              <b>手机连这台电脑,就一步</b>:手机 App 里<b>粘下面这串</b>就连上 —— 经 <b>iroh 打洞 P2P 直连</b>(打不通自动走中继兜底),以 owner 完整权限。装完 App 直接粘,不用登录、不用授权。
              <template v-if="!hasIroh"><br/>(本机 iroh 正在就绪,连接码稍后会自动带上 P2P 直连能力,刷新本页即可。)</template>
            </div>

            <div class="code-box" @click="copyConnectCode">
              <span v-if="connectCode" class="code">{{ connectCode }}</span>
              <span v-else class="code dim">未登录管理者 —— 先在下方注册/登录</span>
            </div>
            <div class="code-actions">
              <button class="pill" @click="copyConnectCode" :disabled="!connectCode">
                <Copy :size="13" /> {{ codeCopied ? "已复制 ✓" : "复制连接码" }}
              </button>
              <button class="pill ghost" @click="showManual = !showManual">
                {{ showManual ? "收起" : "手动填地址/令牌" }}
              </button>
            </div>

            <div v-if="showManual" class="manual">
              <div class="field">
                <div class="fl">本机地址</div>
                <div class="addr-list">
                  <template v-if="collab.hostInfo?.urls?.length">
                    <code v-for="u in collab.hostInfo.urls" :key="u">{{ u }}</code>
                  </template>
                  <span v-else class="dim">仅本机回环 —— 手机要连需开局域网(allow_lan)或走中继</span>
                  <span v-if="collab.hostInfo?.port" class="al">端口 {{ collab.hostInfo.port }}</span>
                </div>
              </div>
              <div class="field">
                <div class="fl">owner 令牌</div>
                <div class="code-box sm" @click="tokenRevealed = !tokenRevealed">
                  <span v-if="collab.token" class="code">{{ tokenRevealed ? collab.token : maskedToken }}</span>
                  <span v-else class="code dim">未登录</span>
                </div>
                <div class="code-actions">
                  <button class="pill ghost" @click="copyToken" :disabled="!collab.token">
                    <Copy :size="12" /> {{ tokenCopied ? "已复制 ✓" : "复制令牌" }}
                  </button>
                  <button class="pill ghost" @click="tokenRevealed = !tokenRevealed" :disabled="!collab.token">
                    {{ tokenRevealed ? "隐藏" : "显示" }}
                  </button>
                </div>
              </div>
            </div>

            <div v-if="isTauri" class="lan-toggle" :class="{ busy: lanBusy }" @click="toggleRemote">
              <div class="lt-txt">
                <span class="lt-title">允许手机走 WiFi 连(局域网直连)</span>
                <span class="lt-sub">{{ remoteOn
                  ? "已开 · 连接码含局域网 IP,同一 WiFi 的手机可连"
                  : "关 · 仅本机可连;手机连不上就打开这个" }}</span>
              </div>
              <span class="switch" :class="{ on: remoteOn }"><i></i></span>
            </div>

            <p class="foot-note">
              仅供你<b>自己的设备</b>用。想让<b>别人(不同账号)</b>加入?到「协作」生成邀请码(collaborator/visitor)。
              要手机从外网(不同 WiFi)连,需走中继/隧道。
            </p>
          </template>

          <template v-else-if="showHostAuth">
            <div class="hint">
              {{ needsBootstrap
                ? "本机主机已启动 —— 注册一个管理者账号,就能拿到手机连接码。"
                : "本机主机已启动 —— 登录管理者账号以取连接码。" }}
            </div>
            <div class="auth-form">
              <input v-model="authForm.username" class="af-inp" placeholder="用户名" autocomplete="username" />
              <input v-if="needsBootstrap" v-model="authForm.displayName" class="af-inp" placeholder="昵称(可选)" />
              <input v-model="authForm.password" type="password" class="af-inp" placeholder="密码" autocomplete="current-password" @keydown.enter="doHostAuth" />
              <button class="cta" :disabled="authBusy" @click="doHostAuth">
                <LoaderCircle v-if="authBusy" :size="15" class="spin" />
                {{ needsBootstrap ? "注册管理者" : "登录" }}
              </button>
              <p v-if="authErr" class="af-err">{{ authErr }}</p>
            </div>
          </template>

          <template v-else>
            <div class="hint">这台电脑还不是主机。设为主机后,手机等设备就能连进来用它的算力与文件。</div>
            <button class="cta" @click="becomeHost">
              <Server :size="16" /> 把这台电脑设为主机
            </button>
          </template>
        </section>
      </template>

      <!-- ════════════ ② 设备与授权 ════════════ -->
      <template v-else-if="tab === 'devices'">
        <div class="devices-tab">
        <!-- 左栏:联盟头像 + 垂直导航(mockup 形态) -->
        <aside class="dev-rail glass">
          <div class="rail-fed">
            <span class="rail-av" v-html="allianceAvatar"></span>
            <div class="rail-fed-txt">
              <div class="rail-name">{{ collab.user?.display_name || collab.user?.username || "未登录" }}</div>
              <div class="rail-sub"><span class="odot" :class="{ off: !authed }"></span> {{ authed ? `${onlineCount} 台在线` : "点下方登录" }}</div>
            </div>
          </div>

          <!-- 账号:登录 / 登出 -->
          <div class="rail-acct">
            <template v-if="authed">
              <span class="ra-role">{{ owner ? "owner · 全权" : (collab.user?.role || "成员") }}</span>
              <button class="ra-btn" @click="doLogout"><LogOut :size="13" /> 登出</button>
            </template>
            <button v-else class="ra-btn pri" @click="showLogin = true">登录 / 注册账号</button>
          </div>

          <nav class="rail-nav">
            <button
              v-for="f in DEV_FILTERS"
              :key="f.key"
              class="rail-item"
              :class="{ on: devFilter === f.key }"
              @click="devFilter = f.key"
            >
              <component :is="f.icon" :size="16" :stroke-width="1.9" />
              <span class="rail-lb">{{ f.label }}</span>
              <span v-if="f.n" class="rail-n">{{ f.n }}</span>
            </button>
          </nav>
          <button class="rail-refresh" title="刷新" @click="loadDevices()"><RefreshCw :size="13" /> 刷新</button>
        </aside>

        <!-- 右栏:当前分区内容 -->
        <div class="dev-content">
        <!-- 我的设备:本机 + 已登记设备 + 已连接 NAS/远程盘,统一状态卡 + 接入卡 -->
        <template v-if="devFilter === 'mine'">
          <!-- 接入面板(展开式):① 分享本机连接码 ② 接入对方 —— 一步到位,不用去教程 -->
          <section v-if="addForm.open" class="glass add-panel">
            <div class="ap-cols">
              <!-- ① 让别的设备连上这台 -->
              <div class="ap-block">
                <div class="ap-h"><Radio :size="14" :stroke-width="2" /> 让别的设备连上这台电脑</div>
                <p class="foot-note" style="margin:2px 0 8px">手机 / 另一台 Polaris 粘下面这串即以 owner 完整权限连上(iroh P2P 直连)。</p>
                <div class="ap-code" @click="copyConnectCode">{{ connectCode || "本机主机就绪中,稍候刷新…" }}</div>
                <div class="ap-acts">
                  <button class="pill" :disabled="!connectCode" @click="copyConnectCode">
                    <Copy :size="13" /> {{ codeCopied ? "已复制 ✓" : "复制连接码" }}
                  </button>
                  <button v-if="isTauri" class="pill ghost" :class="{ busy: lanBusy }" @click="toggleRemote">
                    {{ remoteOn ? "关闭局域网直连" : "开局域网直连" }}
                  </button>
                </div>
              </div>
              <!-- ② 这台连别的 NAS / 主机 -->
              <div class="ap-block">
                <div class="ap-h"><Network :size="14" :stroke-width="2" /> 接入一台 NAS / 远程主机</div>
                <p class="foot-note" style="margin:2px 0 8px">粘对方的 <b>NodeId / 连接码</b> 与 <b>owner 令牌</b>,自动打洞直连(打不通走中继)。</p>
                <div class="add-fields">
                  <input v-model="addForm.name" class="af-inp" placeholder="名称(如:群晖 NAS)" />
                  <input v-model="addForm.nodeId" class="af-inp" placeholder="NodeId / 连接码" />
                  <input v-model="addForm.token" class="af-inp" placeholder="owner 令牌" />
                </div>
                <button class="cta" style="margin-top:8px" :disabled="connBusy" @click="connectRemote">
                  <LoaderCircle v-if="connBusy" :size="15" class="spin" /><Zap v-else :size="15" /> 发起连接
                </button>
              </div>
            </div>
            <button class="ap-close pill ghost" @click="addForm.open = false">收起</button>
          </section>

          <div class="dev-grid">
            <article
              v-for="c in mineItems"
              :key="c.key"
              class="glass dev"
              :class="[TRANSPORT[c.transport].cls, { off: c.revoked }]"
            >
              <div class="dev-top">
                <span class="dev-ico"><component :is="c.icon" :size="19" :stroke-width="1.7" /></span>
                <div class="dev-id">
                  <div class="dev-name">
                    {{ c.name }}
                    <span v-if="c.kind === 'host'" class="host-badge">本机</span>
                  </div>
                  <div class="dev-owner">{{ c.sub }}</div>
                </div>
                <span class="dev-dot" :class="{ on: !c.revoked }"></span>
              </div>

              <div class="dev-line">
                <span class="conn" :class="TRANSPORT[c.transport].cls">
                  <component :is="TRANSPORT[c.transport].icon" :size="12" :stroke-width="2" />
                  {{ TRANSPORT[c.transport].label }}
                </span>
                <span v-if="c.cores" class="dev-cores">{{ c.cores }} 核</span>
              </div>

              <div class="dev-meters" :class="{ stale: c.stale }" v-if="metersOf(c.stats).length">
                <div class="mt" v-for="m in metersOf(c.stats)" :key="m.k">
                  <span class="mt-k">{{ m.k }}</span>
                  <span class="mt-bar"><i :class="meterCls(m.p)" :style="{ width: m.p + '%' }"></i></span>
                  <span class="mt-v">{{ m.v }}</span>
                </div>
                <div v-if="c.stale && c.dev" class="mt-stale">上次上报 {{ c.dev.stats_at ? relTime(Math.floor(c.dev.stats_at / 1000)) : "未知" }} · 已离线?</div>
                <div v-else-if="c.stale && c.src" class="mt-stale">实况更新于 {{ relTime(Math.floor((remoteStatsAt[c.src.id] ?? 0) / 1000)) }} · 隧道重连中…</div>
              </div>
              <div class="dev-meters-none" v-else-if="!c.revoked">
                <Cpu :size="12" :stroke-width="1.9" /> {{ c.kind === 'disk' ? '实况待上报(对端需新版镜像)' : '资源待上报(对方 App 登录后自动上报)' }}
              </div>

              <div class="dev-btns">
                <template v-if="c.kind === 'host'"><span class="b flat">本机 · 全权</span></template>
                <template v-else-if="c.kind === 'disk'">
                  <button class="b" @click="browseDisk(c.src!)"><FolderInput :size="13" /> 浏览盘</button>
                  <button class="b danger" title="断开" @click="forgetRemote(c.src!)"><ShieldOff :size="13" /></button>
                </template>
                <template v-else-if="!c.revoked">
                  <button class="b" @click="mountDisk(c.dev!)"><FolderInput :size="13" /> 挂它的盘</button>
                  <button class="b pri" @click="openDispatch(c.dev!)"><Send :size="13" /> 派任务</button>
                  <button v-if="owner" class="b danger" title="吊销" @click="revoke(c.dev!)"><ShieldOff :size="13" /></button>
                </template>
                <template v-else><span class="b flat dim">已下线</span></template>
              </div>
            </article>

            <!-- 接入卡(加号) -->
            <button class="dev add-card" @click="addForm.open = !addForm.open">
              <Plus :size="28" :stroke-width="1.8" />
              <span class="ac-t">接入设备 / NAS</span>
              <span class="ac-s">手机粘连接码,或填 NAS 的 NodeId</span>
            </button>
          </div>
        </template>

        <!-- 我共享出去的:开给别人的资源(走协作邀请码)+ 加号卡 -->
        <template v-else-if="devFilter === 'shared'">
          <div class="dev-grid">
            <button class="dev add-card" @click="toast.info('到「协作」用定向邀请码把盘/项目开给指定的人,精确到权限与期限')">
              <Plus :size="28" :stroke-width="1.8" />
              <span class="ac-t">开放资源给别人</span>
              <span class="ac-s">走协作邀请码,精确到权限与期限</span>
            </button>
          </div>
        </template>

        <!-- 我能用的:别人分享给我的(凭邀请码接入)+ 加号卡 -->
        <template v-else-if="devFilter === 'usable'">
          <div class="dev-grid">
            <button class="dev add-card" @click="toast.info('拿到别人的邀请码,到手机 App 或「协作」凭码入伙,即可用对方开放的盘/算力')">
              <Plus :size="28" :stroke-width="1.8" />
              <span class="ac-t">凭邀请码接入</span>
              <span class="ac-s">用别人分享的资源(盘 / 算力)</span>
            </button>
          </div>
        </template>

        <!-- 正在发生:audit 活动流(接入/上报/吊销/账号事件,主机留痕) -->
        <template v-else-if="devFilter === 'activity'">
          <div v-if="auditLoading" class="empty glass"><LoaderCircle :size="14" class="spin" /> 拉取活动流…</div>
          <div v-else-if="!auditRows.length" class="empty glass">
            还没有活动记录。设备登录、开始上报资源、被吊销等都会在这里留痕。
          </div>
          <section v-else class="glass act-card">
            <div v-for="(a, i) in auditRows" :key="i" class="act-row">
              <span class="act-dot" :class="'act-' + a.action.split('.')[0]"></span>
              <div class="act-main">
                <div class="act-title"><b>{{ a.actor }}</b> · {{ auditLabel(a) }}</div>
                <div class="act-sub">{{ a.target }}<template v-if="a.detail"> · {{ a.detail }}</template></div>
              </div>
              <span class="act-time">{{ relTime(a.at) }}</span>
            </div>
          </section>
        </template>
        </div><!-- /.dev-content -->
        </div><!-- /.devices-tab -->
      </template>

      <!-- ════════════ ③ 网络拓扑 ════════════ -->
      <template v-else>
        <section class="glass topo-card">
          <div class="topo-head">
            <div>
              <div class="th-title"><Network :size="16" :stroke-width="1.9" /> 网络拓扑</div>
              <div class="th-sub">本机为心、设备为星。连线是<b>系统自动选的传输路径</b>,你无需选。</div>
            </div>
            <div class="topo-legend">
              <span class="lg t-lan"><i></i>局域网</span>
              <span class="lg t-p2p"><i></i>P2P</span>
              <span class="lg t-relay"><i></i>中继</span>
            </div>
          </div>

          <div class="topo-stage">
            <svg :viewBox="`0 0 ${topo.W} ${topo.H}`" class="topo-svg" preserveAspectRatio="xMidYMid meet">
              <!-- 心跳光环 -->
              <circle :cx="topo.cx" :cy="topo.cy" :r="topo.R" class="ring" />
              <circle :cx="topo.cx" :cy="topo.cy" :r="topo.R * 0.62" class="ring faint" />

              <!-- 连线:流动虚线 = 数据在跑 -->
              <g v-for="(nd, i) in topo.nodes" :key="'edge' + i">
                <line
                  :x1="topo.cx" :y1="topo.cy" :x2="nd.x" :y2="nd.y"
                  class="edge" :class="TRANSPORT[nd.t].cls"
                />
                <line
                  :x1="topo.cx" :y1="topo.cy" :x2="nd.x" :y2="nd.y"
                  class="edge-flow" :class="TRANSPORT[nd.t].cls"
                />
              </g>

              <!-- 远端节点:入站设备 + 出站远程盘 -->
              <g v-for="(nd, i) in topo.nodes" :key="'node' + i" class="tnode" :class="{ off: nd.revoked }">
                <circle :cx="nd.x" :cy="nd.y" r="27" class="tn-halo" :class="TRANSPORT[nd.t].cls" />
                <circle :cx="nd.x" :cy="nd.y" r="21" class="tn-disc" />
                <text :x="nd.x" :y="nd.y - 34" class="tn-name" text-anchor="middle">{{ nd.name }}</text>
                <text :x="nd.x" :y="nd.y + 42" class="tn-badge" :class="TRANSPORT[nd.t].cls" text-anchor="middle">{{ TRANSPORT[nd.t].label }}</text>
                <text :x="nd.x" :y="nd.y + 5" class="tn-emoji" text-anchor="middle">{{ nd.emoji }}</text>
              </g>

              <!-- 中心:本机 -->
              <circle :cx="topo.cx" :cy="topo.cy" r="42" class="hub-halo" />
              <circle :cx="topo.cx" :cy="topo.cy" r="34" class="hub-disc" />
              <text :x="topo.cx" :y="topo.cy - 2" class="hub-emoji" text-anchor="middle">🖥</text>
              <text :x="topo.cx" :y="topo.cy + 15" class="hub-label" text-anchor="middle">本机</text>
              <text :x="topo.cx" :y="topo.cy + 58" class="hub-name" text-anchor="middle">{{ hostDevice?.name || (collab.user?.username ? '@' + collab.user.username : '这台电脑') }}</text>
            </svg>

            <div v-if="!topo.nodes.length" class="topo-empty">
              还没有远端节点。去「教程」用连接码把手机连进来,或在「文件中心 · 远程源」连一台 NAS —— 拓扑图上就会长出一颗星。
            </div>
          </div>

          <div class="topo-stats">
            <div class="ts"><span class="ts-n">{{ 1 + topo.nodes.length }}</span><span class="ts-l">节点</span></div>
            <div class="ts"><span class="ts-n">{{ remoteDevices.length }}</span><span class="ts-l">远端设备</span></div>
            <div class="ts"><span class="ts-n">{{ remotes.length }}</span><span class="ts-l">远程盘</span></div>
            <div class="ts"><span class="ts-n">{{ hasIroh ? 'P2P' : (remoteOn ? 'LAN' : '本机') }}</span><span class="ts-l">当前选档</span></div>
          </div>
        </section>
      </template>
    </div>

    <!-- 登录 / 注册账号弹层(桌面互联页切账号) -->
    <Teleport to="body">
      <div v-if="showLogin" class="login-mask" @click.self="showLogin = false">
        <div class="glass login-box">
          <div class="lb-head">
            <span class="lb-av" v-html="allianceAvatar"></span>
            <div>
              <div class="lb-title">{{ needsBootstrap ? "创建 owner 账号" : "登录账号" }}</div>
              <div class="lb-sub">{{ needsBootstrap ? "本机还没账号,建一个 owner" : "登录后管理设备联盟" }}</div>
            </div>
            <button class="icobtn" @click="showLogin = false">✕</button>
          </div>
          <input v-model="authForm.username" class="af-inp" placeholder="用户名" autocapitalize="off" />
          <input v-model="authForm.password" type="password" class="af-inp" placeholder="密码" @keydown.enter="submitLogin" />
          <input v-if="needsBootstrap" v-model="authForm.displayName" class="af-inp" placeholder="昵称(可选)" />
          <p v-if="authErr" class="lb-err">{{ authErr }}</p>
          <button class="cta full" :disabled="authBusy" @click="submitLogin">
            <LoaderCircle v-if="authBusy" :size="15" class="spin" />
            {{ needsBootstrap ? "创建并登录" : "登录" }}
          </button>
        </div>
      </div>
    </Teleport>
  </div>
</template>

<style scoped>
/* ═══════════ 玻璃琉璃基调 ═══════════
   苹果磨砂玻璃:半透明底 + backdrop blur + 顶缘高光 + 柔阴影。
   浅色顶缘白高光,深色压到几近无。 */
.interconnect {
  flex: 1;
  display: flex;
  flex-direction: column;
  min-height: 0;
  --glass-bg: color-mix(in srgb, var(--panel) 68%, transparent);
  --glass-hi: rgba(255, 255, 255, 0.55);
  --glass-brd: color-mix(in srgb, var(--border) 60%, transparent);
}
:root[data-theme="dark"] .interconnect,
:root[data-theme="aurora-dark"] .interconnect {
  --glass-bg: color-mix(in srgb, var(--panel) 58%, transparent);
  --glass-hi: rgba(255, 255, 255, 0.07);
  --glass-brd: rgba(255, 255, 255, 0.09);
}
.glass {
  background: var(--glass-bg);
  backdrop-filter: blur(22px) saturate(180%);
  -webkit-backdrop-filter: blur(22px) saturate(180%);
  border: 1px solid var(--glass-brd);
  border-radius: 18px;
  box-shadow: var(--shadow-lg), inset 0 1px 0 var(--glass-hi);
}

.bar {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 11px 16px;
  border-bottom: 1px solid var(--hairline);
  flex-wrap: wrap;
}
.ttl { display: flex; align-items: center; gap: 8px; font-weight: 600; font-size: 15px; letter-spacing: .5px; }

/* 玻璃分段 Tab(仿手机「云端工作 / 连接电脑」药丸) */
.tabs {
  display: inline-flex;
  gap: 3px;
  padding: 4px;
  border-radius: 13px;
  background: color-mix(in srgb, var(--selection-bg) 55%, transparent);
  backdrop-filter: blur(12px);
  -webkit-backdrop-filter: blur(12px);
  border: 1px solid var(--glass-brd);
}
.tab {
  display: inline-flex; align-items: center; gap: 6px;
  padding: 7px 14px; border: none; background: none; cursor: pointer;
  border-radius: 10px; font-size: 12.5px; font-weight: 500; color: var(--muted);
  transition: color .18s, background .18s, box-shadow .18s;
}
.tab:hover { color: var(--text-2); }
.tab.on {
  background: var(--panel); color: var(--ink); font-weight: 600;
  box-shadow: var(--shadow), inset 0 1px 0 var(--glass-hi);
}
.icobtn { margin-left: auto; border: none; background: none; color: var(--muted); cursor: pointer; padding: 6px; border-radius: 8px; display: inline-flex; }
.icobtn:hover { color: var(--ink); background: var(--selection-bg); }
.icobtn.sm { padding: 4px; margin-left: auto; }
.scroll { flex: 1; overflow-y: auto; padding: 20px; display: flex; flex-direction: column; gap: 16px; max-width: 820px; width: 100%; margin: 0 auto; }
/* 设备与授权是宽版双栏,别被 820 卡窄留大白边 */
.scroll:has(.devices-tab) { max-width: 1320px; }

@media (max-width: 640px) {
  .tab-label { display: none; }
  .tab { padding: 7px 11px; }
}

.grad { background: linear-gradient(120deg, #0d99ff, #7c4dff); -webkit-background-clip: text; background-clip: text; -webkit-text-fill-color: transparent; }

/* ── 接入三步卡 ── */
.steps-card { padding: 24px 26px; position: relative; overflow: visible; }
.steps-card::before {
  content: ""; position: absolute; inset: 0; pointer-events: none; z-index: 0; border-radius: inherit;
  background: radial-gradient(120% 90% at 100% 0%, color-mix(in srgb, #7c4dff 12%, transparent), transparent 55%);
}
.sc-head, .steps { z-index: 1; }
.sc-head { position: relative; }
.sc-kick { font-size: 11.5px; font-weight: 700; letter-spacing: .16em; text-transform: uppercase; color: #0d99ff; }
.sc-title { font-size: 24px; margin: 8px 0 6px; letter-spacing: -.4px; line-height: 1.2; }
.sc-sub { margin: 0 0 4px; color: var(--text-2); font-size: 13px; line-height: 1.65; }
.steps { list-style: none; margin: 18px 0 0; padding: 0; display: grid; gap: 12px; position: relative; }
.steps li { display: flex; gap: 13px; align-items: flex-start; }
.st-n {
  flex: none; width: 26px; height: 26px; border-radius: 9px; color: #fff; font-weight: 700; font-size: 13px;
  display: flex; align-items: center; justify-content: center;
  background: linear-gradient(135deg, #0d99ff, #7c4dff);
  box-shadow: 0 4px 12px rgba(13, 153, 255, .3);
}
.steps li b { font-size: 14px; }
.steps li p { margin: 3px 0 0; font-size: 12.5px; color: var(--text-2); line-height: 1.6; }
.steps li div { min-width: 0; }

/* ① 主机连接卡(hero) */
.hero { padding: 20px 22px; }
.hero-head { display: flex; align-items: center; gap: 9px; font-weight: 600; font-size: 15.5px; margin-bottom: 10px; }
.hint { font-size: 13px; color: var(--text-2); line-height: 1.7; margin-bottom: 14px; }
.hint b { color: var(--ink); }
.code-box {
  border: 1px solid var(--glass-brd); border-radius: 12px;
  background: color-mix(in srgb, var(--bg) 55%, transparent); padding: 16px 14px; text-align: center;
  cursor: pointer; min-height: 56px; display: flex; align-items: center; justify-content: center;
  transition: border-color .15s;
}
.code-box:hover { border-color: #0d99ff; }
.code {
  font-family: var(--mono); font-size: 15px; font-weight: 600; letter-spacing: 1px;
  color: var(--ink); word-break: break-all; user-select: all; line-height: 1.5;
}
.code.dim { color: var(--muted); font-weight: 400; }
.code-actions { display: flex; gap: 10px; margin-top: 12px; }
.al { color: var(--muted); font-size: 11.5px; }
.field { margin-bottom: 14px; }
.fl { font-size: 12px; color: var(--muted); margin-bottom: 7px; font-weight: 500; }
.addr-list { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
.addr-list code { font-family: var(--mono); font-size: 12px; background: var(--selection-bg); padding: 4px 9px; border-radius: 6px; color: var(--text); user-select: all; }
.manual { margin-top: 6px; padding-top: 14px; border-top: 1px dashed var(--border); }
.lan-toggle {
  display: flex; align-items: center; gap: 12px; cursor: pointer;
  margin-top: 16px; padding: 12px 14px; border-radius: 12px;
  background: color-mix(in srgb, var(--bg) 55%, transparent); border: 1px solid var(--glass-brd);
}
.lan-toggle:hover { border-color: #0d99ff; }
.lan-toggle.busy { opacity: .6; pointer-events: none; }
.lt-txt { flex: 1; display: flex; flex-direction: column; gap: 3px; min-width: 0; }
.lt-title { font-weight: 600; font-size: 13.5px; }
.lt-sub { font-size: 11.5px; color: var(--muted); }
.switch {
  width: 44px; height: 26px; border-radius: 13px; flex: none; position: relative;
  background: var(--selection-bg); border: 1px solid var(--border); transition: background .15s;
}
.switch i { position: absolute; top: 2px; left: 2px; width: 20px; height: 20px; border-radius: 50%; background: var(--muted); transition: transform .15s, background .15s; }
.switch.on { background: #0d99ff; border-color: #0d99ff; }
.switch.on i { transform: translateX(18px); background: #fff; }
.code-box.sm { min-height: 40px; padding: 10px 12px; }
.code-box.sm .code { font-size: 12.5px; }
.foot-note { margin: 12px 0 0; font-size: 11.5px; color: var(--muted); line-height: 1.6; }
.foot-note b { color: var(--text-2); }
.cta {
  display: inline-flex; align-items: center; gap: 8px; justify-content: center;
  padding: 11px 18px; border-radius: 12px; border: none; cursor: pointer;
  background: linear-gradient(135deg, #0d99ff, #7c4dff); color: #fff; font-weight: 600; font-size: 14px;
  box-shadow: 0 6px 18px rgba(13, 153, 255, .28);
}
.cta.full { width: 100%; margin-top: 12px; }
.cta:active { transform: scale(.97); }
.cta:disabled { opacity: .6; cursor: not-allowed; }
.auth-form { display: flex; flex-direction: column; gap: 10px; max-width: 320px; }
.af-inp {
  border: 1px solid var(--glass-brd); border-radius: 10px;
  background: color-mix(in srgb, var(--bg) 55%, transparent); color: var(--ink);
  font-size: 13.5px; padding: 10px 12px; outline: none;
}
.af-inp:focus { border-color: #0d99ff; }
.af-err { margin: 0; font-size: 12px; color: var(--vermilion); }

/* pill 按钮 */
.pill {
  display: inline-flex; align-items: center; gap: 6px; justify-content: center;
  flex: 1; padding: 9px; border-radius: 10px; cursor: pointer; font-size: 13px;
  border: 1px solid transparent; background: #0d99ff; color: #fff;
}
.pill.ghost { background: color-mix(in srgb, var(--bg) 50%, transparent); color: var(--text); border-color: var(--glass-brd); }
.pill.ghost:hover { border-color: var(--ink); color: var(--ink); }
.pill:disabled { opacity: .5; cursor: not-allowed; }

/* ② 账号根卡 */
.rootcard { padding: 16px 18px; }
.rc-head { display: flex; align-items: center; gap: 8px; margin-bottom: 12px; }
.rc-title { font-weight: 600; font-size: 14.5px; }
.rc-badge { margin-left: auto; font-size: 11px; font-weight: 700; color: #fff; background: linear-gradient(135deg, #16a34a, #0d99ff); padding: 3px 10px; border-radius: 999px; }
.rc-code {
  font-family: var(--mono); font-size: 17px; letter-spacing: 1.5px; text-align: center;
  padding: 13px 8px; border-radius: 11px; background: color-mix(in srgb, var(--bg) 55%, transparent); border: 1px solid var(--glass-brd);
  user-select: all; word-break: break-all; cursor: pointer;
}
.rc-actions { display: flex; gap: 10px; margin-top: 11px; }

/* ── 设备联盟 ── */
.fed-head { display: flex; align-items: center; gap: 12px; padding: 13px 18px; }
.fh-av { width: 34px; height: 34px; border-radius: 50%; flex: none; background: linear-gradient(135deg, #0d99ff, #7c4dff); box-shadow: 0 4px 12px rgba(124, 77, 255, .3); }
.fh-txt { flex: 1; min-width: 0; }
.fh-title { font-weight: 600; font-size: 14.5px; }
.fh-sub { font-size: 11.5px; color: var(--muted); display: flex; align-items: center; gap: 6px; }
.odot { width: 7px; height: 7px; border-radius: 50%; background: #16a34a; display: inline-block; box-shadow: 0 0 0 3px color-mix(in srgb, #16a34a 22%, transparent); }

.empty { display: flex; align-items: center; gap: 8px; font-size: 12.5px; color: var(--muted); padding: 22px; justify-content: center; }

/* ── 设备联盟:左栏 + 右内容 双栏 ── */
.devices-tab { display: flex; gap: 14px; align-items: flex-start; }
.dev-rail {
  flex: none; width: 216px; position: sticky; top: 0; padding: 16px 14px;
  display: flex; flex-direction: column; gap: 6px; border-radius: 18px;
}
.rail-fed { display: flex; flex-direction: column; align-items: center; text-align: center; gap: 8px; padding: 6px 6px 14px; border-bottom: 1px solid var(--glass-brd); margin-bottom: 8px; }
.rail-av { width: 80px; height: 80px; flex: none; display: block; border-radius: 50%; overflow: hidden; box-shadow: 0 6px 20px rgba(13, 153, 255, .3); }
.rail-av :deep(svg) { width: 100%; height: 100%; display: block; }
.rail-fed-txt { min-width: 0; width: 100%; }
.rail-name { font-weight: 700; font-size: 15px; line-height: 1.25; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.rail-sub { font-size: 12px; color: var(--muted); display: flex; align-items: center; gap: 5px; margin-top: 3px; justify-content: center; }
.rail-nav { display: flex; flex-direction: column; gap: 3px; }
.rail-item {
  display: flex; align-items: center; gap: 11px; padding: 11px 13px; border-radius: 12px;
  font-size: 14px; font-weight: 600; color: var(--text-2); background: transparent;
  border: none; cursor: pointer; text-align: left; transition: all .15s var(--ease-out);
}
.rail-item:hover { background: color-mix(in srgb, var(--text-1) 6%, transparent); color: var(--text-1); }
.rail-item.on { color: #fff; background: linear-gradient(135deg, #0d99ff, #7c4dff); box-shadow: 0 4px 12px rgba(13, 153, 255, .3); }
.rail-lb { flex: 1; min-width: 0; }
.rail-n { min-width: 18px; height: 18px; padding: 0 5px; border-radius: 9px; font-size: 10.5px; font-weight: 700; display: inline-flex; align-items: center; justify-content: center; background: color-mix(in srgb, currentColor 16%, transparent); }
.rail-item.on .rail-n { background: rgba(255,255,255,.28); }
.rail-refresh { margin-top: auto; display: inline-flex; align-items: center; justify-content: center; gap: 6px; padding: 8px; border-radius: 10px; font-size: 12px; color: var(--muted); background: transparent; }
.rail-refresh:hover { color: var(--text-1); background: color-mix(in srgb, var(--text-1) 6%, transparent); }
.rail-acct { display: flex; align-items: center; justify-content: center; gap: 8px; padding: 0 4px 10px; margin-bottom: 6px; border-bottom: 1px solid var(--glass-brd); }
.ra-role { font-size: 11px; color: var(--muted); font-weight: 600; }
.ra-btn { display: inline-flex; align-items: center; gap: 5px; font-size: 12px; font-weight: 600; padding: 6px 12px; border-radius: 999px; color: var(--text-2); border: 1px solid var(--glass-brd); background: transparent; }
.ra-btn:hover { color: var(--text-1); background: color-mix(in srgb, var(--text-1) 6%, transparent); }
.ra-btn.pri { color: #fff; border-color: transparent; background: linear-gradient(135deg, #0d99ff, #7c4dff); box-shadow: 0 3px 10px rgba(13,153,255,.28); width: 100%; justify-content: center; }
.odot.off { background: var(--muted); box-shadow: none; }
.dev-content { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 13px; }

/* 接入卡(加号)+ 接入表单 */
.add-card {
  display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 8px;
  min-height: 230px; border: 1.5px dashed var(--glass-brd); border-radius: 18px; cursor: pointer;
  color: var(--text-2); background: color-mix(in srgb, var(--text-1) 3%, transparent); transition: all .16s var(--ease-out);
}
.add-card:hover { border-color: #0d99ff; color: #0d99ff; background: color-mix(in srgb, #0d99ff 7%, transparent); }
.ac-t { font-size: 15px; font-weight: 700; }
.ac-s { font-size: 12.5px; color: var(--muted); }
.add-panel { padding: 16px 18px; }
.ap-cols { display: grid; grid-template-columns: 1fr 1fr; gap: 20px; }
@media (max-width: 780px) { .ap-cols { grid-template-columns: 1fr; } }
.ap-block { min-width: 0; }
.ap-h { display: flex; align-items: center; gap: 7px; font-size: 13px; font-weight: 700; margin-bottom: 2px; }
.ap-code { font-family: var(--mono); font-size: 11px; line-height: 1.5; word-break: break-all; user-select: all; cursor: pointer; padding: 10px 12px; border-radius: 10px; background: color-mix(in srgb, var(--text-1) 6%, transparent); border: 1px solid var(--glass-brd); max-height: 88px; overflow: auto; }
.ap-acts { display: flex; gap: 8px; margin-top: 9px; flex-wrap: wrap; }
.ap-close { margin-top: 14px; }
.add-fields { display: grid; gap: 8px; }
.add-acts { display: flex; gap: 10px; align-items: center; margin-top: 11px; }
.af-inp { width: 100%; padding: 9px 11px; border-radius: 10px; border: 1px solid var(--glass-brd); background: color-mix(in srgb, var(--bg) 40%, transparent); color: var(--text); outline: none; font-size: 13px; }
.af-inp:focus { border-color: #0d99ff; }

/* 登录弹层 */
.login-mask { position: fixed; inset: 0; z-index: 300; background: rgba(0,0,0,.4); display: flex; align-items: center; justify-content: center; padding: 20px; }
.login-box { width: min(360px, 92vw); padding: 20px; border-radius: 18px; display: flex; flex-direction: column; gap: 10px; }
.lb-head { display: flex; align-items: center; gap: 11px; margin-bottom: 4px; }
.lb-av { width: 44px; height: 44px; flex: none; border-radius: 50%; overflow: hidden; box-shadow: 0 4px 12px rgba(13,153,255,.28); }
.lb-av :deep(svg) { width: 100%; height: 100%; display: block; }
.lb-title { font-weight: 700; font-size: 15px; }
.lb-sub { font-size: 11.5px; color: var(--muted); }
.lb-head .icobtn { margin-left: auto; }
.lb-err { color: var(--vermilion); font-size: 12px; margin: 0; }
.cta.full { width: 100%; }
@media (max-width: 720px) {
  .devices-tab { flex-direction: column; }
  .dev-rail { width: 100%; position: static; }
  .rail-nav { flex-direction: row; flex-wrap: wrap; }
  .rail-item { flex: 1 1 auto; }
}

/* ── 设备卡:核数 + 资源仪表 ── */
.dev-cores { font-size: 12px; font-weight: 700; color: var(--text-2); padding: 2px 9px; border-radius: 7px; background: color-mix(in srgb, var(--text-1) 7%, transparent); }
.dev-meters { display: grid; gap: 10px; margin: 8px 0 12px; }
.mt { display: grid; grid-template-columns: 38px 1fr auto; align-items: center; gap: 10px; }
.mt-k { font-size: 12px; color: var(--muted); }
.mt-bar { height: 8px; border-radius: 5px; background: color-mix(in srgb, var(--text-1) 9%, transparent); overflow: hidden; }
.mt-bar i { display: block; height: 100%; border-radius: 5px; transition: width .5s var(--ease-out); }
.mt-bar i.m-cool { background: linear-gradient(90deg, #0d99ff, #2ec5ff); }
.mt-bar i.m-warm { background: linear-gradient(90deg, #d97706, #f0a53b); }
.mt-bar i.m-hot { background: linear-gradient(90deg, #dc2626, #f2603b); }
.mt-v { font-family: var(--mono); font-size: 11.5px; color: var(--text-2); min-width: 56px; text-align: right; }
.dev-meters-none { display: flex; align-items: center; gap: 6px; font-size: 12.5px; color: var(--muted); margin: 8px 0 12px; }
.dev-meters.stale { opacity: .45; }
.mt-stale { font-size: 10px; color: var(--muted); }
.remote-block { padding: 4px 0 8px; border-bottom: 1px dashed var(--glass-brd); }
.remote-block:last-of-type { border-bottom: none; }
.remote-block .dev-meters { max-width: 420px; margin: 8px 0 2px; }

/* ── 正在发生(活动流) ── */
.act-card { padding: 8px 16px; }
.act-row { display: flex; align-items: center; gap: 11px; padding: 10px 2px; border-bottom: 1px solid var(--glass-brd); }
.act-row:last-child { border-bottom: none; }
.act-dot { width: 8px; height: 8px; border-radius: 50%; background: #8a8f98; flex: none; }
.act-dot.act-auth { background: #16a34a; }
.act-dot.act-device { background: #0d99ff; }
.act-dot.act-user { background: #7c4dff; }
.act-dot.act-mirror { background: #d97706; }
.act-main { flex: 1; min-width: 0; }
.act-title { font-size: 13px; }
.act-sub { font-size: 11.5px; color: var(--muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.act-time { font-size: 11px; color: var(--muted); flex: none; }

.dev-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 16px; }
@media (max-width: 900px) { .dev-grid { grid-template-columns: 1fr; } }
.dev { padding: 22px 24px; position: relative; overflow: hidden; transition: transform .18s var(--ease-out), box-shadow .18s; }
.dev:hover { transform: translateY(-2px); box-shadow: var(--shadow-lg), inset 0 1px 0 var(--glass-hi), 0 14px 30px rgba(20, 20, 25, .08); }
.dev::after { content: ""; position: absolute; left: 0; right: 0; top: 0; height: 3px; opacity: .85; }
.dev.t-local::after { background: linear-gradient(90deg, #8a8f98, #b5b9c0); }
.dev.t-lan::after { background: linear-gradient(90deg, #16a34a, #37c76a); }
.dev.t-p2p::after { background: linear-gradient(90deg, #0d99ff, #2ec5ff); }
.dev.t-relay::after { background: linear-gradient(90deg, #d97706, #f0a53b); }
.dev.off { opacity: .5; }
.dev.off::after { background: var(--border); }
.dev-top { display: flex; align-items: center; gap: 12px; }
.dev-ico { width: 44px; height: 44px; border-radius: 12px; background: color-mix(in srgb, var(--selection-bg) 70%, transparent); display: flex; align-items: center; justify-content: center; color: var(--ink); flex: none; }
.dev-id { flex: 1; min-width: 0; }
.dev-name { font-weight: 700; font-size: 16px; display: flex; align-items: center; gap: 7px; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.dev-owner { font-size: 12.5px; color: var(--muted); margin-top: 1px; }
.host-badge { font-size: 10px; font-weight: 700; color: #b8860b; background: color-mix(in srgb, #b8860b 14%, transparent); border-radius: 4px; padding: 1px 6px; flex: none; }
.dev-dot { width: 8px; height: 8px; border-radius: 50%; background: var(--muted); flex: none; }
.dev-dot.on { background: #16a34a; box-shadow: 0 0 0 3px color-mix(in srgb, #16a34a 20%, transparent); }
.dev-line { display: flex; align-items: center; gap: 8px; margin: 13px 0 8px; }
.conn { display: inline-flex; align-items: center; gap: 5px; font-size: 12px; font-weight: 700; padding: 4px 11px; border-radius: 8px; }
.conn.t-local { background: color-mix(in srgb, #8a8f98 16%, transparent); color: var(--text-2); }
.conn.t-lan { background: color-mix(in srgb, #16a34a 15%, transparent); color: #16a34a; }
.conn.t-p2p { background: color-mix(in srgb, #0d99ff 15%, transparent); color: #0d99ff; }
.conn.t-relay { background: color-mix(in srgb, #d97706 16%, transparent); color: #d97706; }
.dev-node { margin-left: auto; font-family: var(--mono); font-size: 10.5px; color: var(--muted); }
.dev-grant { display: flex; align-items: center; gap: 5px; font-size: 11px; color: var(--text-2); margin-bottom: 12px; }
.dev-grant.revoked { color: var(--vermilion); }
.dev-btns { display: flex; gap: 10px; margin-top: 14px; }
.b {
  flex: 1; display: inline-flex; align-items: center; justify-content: center; gap: 6px;
  font-size: 13.5px; font-weight: 600; padding: 12px; border-radius: 11px; cursor: pointer;
  border: 1px solid var(--glass-brd); background: color-mix(in srgb, var(--bg) 45%, transparent); color: var(--text);
  transition: border-color .15s, background .15s;
}
.b.flat { background: transparent; color: var(--muted); font-weight: 500; }
.b:hover { border-color: var(--ink); }
.b.pri { background: linear-gradient(135deg, #0d99ff, #7c4dff); color: #fff; border-color: transparent; box-shadow: 0 4px 12px rgba(13, 153, 255, .26); }
.b.pri:hover { filter: brightness(1.05); }
.b.danger { flex: none; width: 34px; color: var(--vermilion); }
.b.danger:hover { border-color: var(--vermilion); background: var(--vermilion-soft); }
.b.flat { cursor: default; background: none; border-color: transparent; color: var(--muted); }
.b.flat.dim { color: var(--dim); }

.grant-note { padding: 16px 18px; }
.gn-head { display: flex; align-items: center; gap: 8px; font-weight: 600; font-size: 13.5px; margin-bottom: 10px; }
.gn-list { margin: 0; padding-left: 20px; display: grid; gap: 6px; }
.gn-list li { font-size: 12.5px; color: var(--text-2); line-height: 1.6; }
.gn-list b { color: var(--ink); }
.remote-line { margin: 10px 0 0; padding: 8px 10px; border-radius: 10px; background: color-mix(in srgb, var(--bg) 45%, transparent); border: 1px solid var(--glass-brd); }
.remote-line .b.danger { margin-left: 4px; }

/* ── 网络拓扑 ── */
.topo-card { padding: 18px 20px; }
.topo-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 14px; flex-wrap: wrap; margin-bottom: 8px; }
.th-title { display: flex; align-items: center; gap: 7px; font-weight: 600; font-size: 15px; }
.th-sub { font-size: 12px; color: var(--muted); margin-top: 3px; }
.th-sub b { color: var(--text-2); }
.topo-legend { display: flex; gap: 12px; }
.lg { display: inline-flex; align-items: center; gap: 5px; font-size: 11px; color: var(--muted); }
.lg i { width: 9px; height: 9px; border-radius: 2px; display: inline-block; }
.lg.t-lan i { background: #16a34a; } .lg.t-p2p i { background: #0d99ff; } .lg.t-relay i { background: #d97706; } .lg.t-disk i { background: #14b8a6; }

.topo-stage { position: relative; margin: 6px 0 4px; }
.topo-svg { width: 100%; height: auto; display: block; overflow: visible; }
.ring { fill: none; stroke: var(--glass-brd); stroke-width: 1; stroke-dasharray: 3 5; opacity: .7; }
.ring.faint { opacity: .4; }
.edge { stroke-width: 2; opacity: .35; }
.edge.t-lan { stroke: #16a34a; } .edge.t-p2p { stroke: #0d99ff; } .edge.t-relay { stroke: #d97706; } .edge.t-disk { stroke: #14b8a6; }
.edge-flow { stroke-width: 2.4; stroke-dasharray: 2 12; stroke-linecap: round; animation: flow 1.1s linear infinite; }
.edge-flow.t-lan { stroke: #16a34a; } .edge-flow.t-p2p { stroke: #0d99ff; } .edge-flow.t-relay { stroke: #d97706; } .edge-flow.t-disk { stroke: #14b8a6; }
@keyframes flow { to { stroke-dashoffset: -14; } }

.hub-halo { fill: color-mix(in srgb, #7c4dff 18%, transparent); animation: pulse 2.6s ease-in-out infinite; }
.hub-disc { fill: var(--panel); stroke: #7c4dff; stroke-width: 1.5; filter: drop-shadow(0 4px 12px rgba(124, 77, 255, .35)); }
.hub-emoji { font-size: 22px; }
.hub-label { font-size: 10px; font-weight: 700; fill: #7c4dff; }
.hub-name { font-size: 11px; fill: var(--muted); }
@keyframes pulse { 0%, 100% { transform: scale(1); opacity: .6; } 50% { transform: scale(1.14); opacity: .3; } }
.hub-halo { transform-box: fill-box; transform-origin: center; }

.tnode.off { opacity: .45; }
.tn-halo { fill: none; stroke-width: 1.5; opacity: .5; }
.tn-halo.t-lan { stroke: #16a34a; } .tn-halo.t-p2p { stroke: #0d99ff; } .tn-halo.t-relay { stroke: #d97706; } .tn-halo.t-disk { stroke: #14b8a6; }
.tn-disc { fill: var(--panel); stroke: var(--glass-brd); stroke-width: 1; filter: drop-shadow(0 3px 8px rgba(20, 20, 25, .12)); }
.tn-emoji { font-size: 17px; }
.tn-name { font-size: 11px; font-weight: 600; fill: var(--text); }
.tn-badge { font-size: 9.5px; font-weight: 700; }
.tn-badge.t-lan { fill: #16a34a; } .tn-badge.t-p2p { fill: #0d99ff; } .tn-badge.t-relay { fill: #d97706; } .tn-badge.t-disk { fill: #14b8a6; }

.topo-empty { position: absolute; left: 50%; bottom: 8px; transform: translateX(-50%); text-align: center; font-size: 12px; color: var(--muted); max-width: 320px; }

.topo-stats { display: grid; grid-template-columns: repeat(4, 1fr); gap: 10px; margin-top: 14px; padding-top: 14px; border-top: 1px solid var(--hairline); }
.ts { text-align: center; }
.ts-n { display: block; font-size: 19px; font-weight: 700; color: var(--ink); font-family: var(--mono); }
.ts-l { font-size: 11px; color: var(--muted); }

/* ── 派任务浮层 ── */
.dispatch-mask {
  position: fixed; inset: 0; z-index: 2000; display: flex; align-items: center; justify-content: center;
  background: var(--overlay); backdrop-filter: blur(3px); -webkit-backdrop-filter: blur(3px);
  animation: fade .18s ease;
}
@keyframes fade { from { opacity: 0; } }
.dispatch { width: min(420px, calc(100vw - 40px)); padding: 18px 20px; animation: pop .22s var(--ease-spring); }
@keyframes pop { from { transform: scale(.94); opacity: 0; } }
.dp-head { display: flex; align-items: center; gap: 11px; margin-bottom: 16px; }
.dp-ico { width: 38px; height: 38px; border-radius: 11px; background: color-mix(in srgb, var(--selection-bg) 70%, transparent); display: flex; align-items: center; justify-content: center; color: var(--ink); flex: none; }
.dp-title { font-weight: 600; font-size: 15px; }
.dp-sub { font-size: 11.5px; color: var(--muted); }
.dp-head .icobtn { font-size: 15px; }
.dp-connecting { display: flex; flex-direction: column; align-items: center; gap: 12px; padding: 26px 0; color: var(--text-2); }
.dp-c-txt { text-align: center; font-size: 13.5px; line-height: 1.5; }
.dp-c-txt span { font-size: 11.5px; color: var(--muted); }
.dp-conn-ok { display: flex; align-items: center; gap: 7px; font-size: 13px; color: #16a34a; font-weight: 600; margin-bottom: 12px; }
.dp-conn-ok .conn { font-weight: 700; }
.dp-input {
  width: 100%; border: 1px solid var(--glass-brd); border-radius: 12px; resize: vertical;
  background: color-mix(in srgb, var(--bg) 55%, transparent); color: var(--ink);
  font-family: inherit; font-size: 13.5px; padding: 12px 13px; outline: none; line-height: 1.6;
}
.dp-input:focus { border-color: #0d99ff; }
.dp-note { margin: 10px 0 0; font-size: 11px; color: var(--muted); line-height: 1.6; }
.dp-sent { display: flex; flex-direction: column; align-items: center; gap: 8px; padding: 24px 0; color: #16a34a; }
.dp-sent div { font-size: 15px; font-weight: 600; }
.dp-sent span { font-size: 11.5px; color: var(--muted); }

.dim { font-size: 11.5px; color: var(--dim); }
.spin { animation: spin .9s linear infinite; }
@keyframes spin { to { transform: rotate(360deg); } }
</style>
