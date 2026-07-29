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
  Mail,
  Terminal,
  Pencil,
  Eye,
} from "@lucide/vue";
import RemoteTerminal from "./RemoteTerminal.vue";
import { invoke, isTauri } from "../../tauri";
import { useCollabStore } from "../collab/stores/collab";
import { useAppStore } from "../../stores/app";
import {
  collabApi,
  parseConnectCode,
  type AdminDevice,
  type AuditRow,
  type EmailStatus,
} from "../collab/api";
import { toast } from "../../composables/useToast";
import { requestBeamOpen } from "../../lib/beamBus";
import { apps as appsApi, fsmount, openUrl, type MountStatus, type PubApp, type PortHit } from "../../tauri";
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
const authForm = reactive({
  username: "",
  password: "",
  displayName: "",
  email: "",
  code: "",
  newPassword: "",
});
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
    // 登录前自保:本机主机没跑先拉起、地址漂了先校正(修「无法连接协作主机」)。
    await collab.ensureLocalHost();
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
    // 账号由云端账号中心统管时带上它的地址:收码人换账号登录时知道往哪儿打
    // (令牌本身就是凭据,进门本身用不着这一项)。
    if (collab.authorityUrl) payload.u = collab.authorityUrl;
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
      transport: diskTransport(s), // 后端上报的真实链路:打洞直连标 P2P,走中继如实标 relay
      cores: remoteStats.value[s.id]?.cores ?? null,
      stats: remoteStats.value[s.id] ?? null,
      stale: diskStale(s.id),
      icon: HardDrive,
      src: s,
    });
  }
  return out;
});

// ── 账号:登录 / 注册 / 忘记密码(桌面互联页也能切账号) ──
const showLogin = ref(false);
type AuthMode = "login" | "signup" | "reset";
const authMode = ref<AuthMode>("login");
/** 邮箱服务状态(主机侧):决定「注册」入口亮不亮、忘记密码能不能自助 */
const emailInfo = ref<EmailStatus | null>(null);
async function openLogin() {
  showLogin.value = true;
  authMode.value = "login";
  authErr.value = "";
  // 先保证本机主机在跑(修「无法连接协作主机」),再探邮箱服务状态。
  try {
    await collab.ensureLocalHost();
  } catch {
    /* 起不来时登录提交处会给出具体错误 */
  }
  try {
    emailInfo.value = await collabApi.emailStatus();
  } catch {
    emailInfo.value = null; // 旧版主机无此端点 → 只显示登录
  }
}
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

// ── 邮箱验证码:发送(60s 倒计时)/ 注册 / 找回密码 ──
const codeCooldown = ref(0);
let codeTimer: ReturnType<typeof setInterval> | null = null;
async function sendCode() {
  const email = authForm.email.trim();
  if (!email) {
    authErr.value = "请先填写邮箱";
    return;
  }
  if (codeCooldown.value > 0) return;
  authErr.value = "";
  try {
    await collab.ensureLocalHost();
    await collabApi.sendEmailCode(email, authMode.value === "reset" ? "reset" : "signup");
    toast.info(`验证码已发往 ${email},10 分钟内有效`);
    codeCooldown.value = 60;
    if (codeTimer) clearInterval(codeTimer);
    codeTimer = setInterval(() => {
      codeCooldown.value--;
      if (codeCooldown.value <= 0 && codeTimer) {
        clearInterval(codeTimer);
        codeTimer = null;
      }
    }, 1000);
  } catch (e) {
    authErr.value = (e as Error).message;
  }
}

async function doEmailSignup() {
  authErr.value = "";
  const u = authForm.username.trim();
  if (!authForm.email.trim() || !authForm.code.trim()) {
    authErr.value = "请填写邮箱并获取验证码";
    return;
  }
  if (!u || !authForm.password) {
    authErr.value = "请填写用户名和密码";
    return;
  }
  authBusy.value = true;
  try {
    await collab.ensureLocalHost();
    await collab.emailSignup({
      email: authForm.email.trim(),
      code: authForm.code.trim(),
      username: u,
      password: authForm.password,
      displayName: authForm.displayName.trim() || u,
    });
    authForm.password = "";
    authForm.code = "";
    toast.info("注册成功,已登录");
    await refreshAll();
  } catch (e) {
    authErr.value = (e as Error).message;
  } finally {
    authBusy.value = false;
  }
}

async function doEmailReset() {
  authErr.value = "";
  if (!authForm.email.trim() || !authForm.code.trim()) {
    authErr.value = "请填写邮箱并获取验证码";
    return;
  }
  if (!authForm.newPassword) {
    authErr.value = "请填写新密码(至少 8 位)";
    return;
  }
  authBusy.value = true;
  try {
    await collab.ensureLocalHost();
    const r = await collabApi.emailReset({
      email: authForm.email.trim(),
      code: authForm.code.trim(),
      newPassword: authForm.newPassword,
    });
    authForm.code = "";
    authForm.newPassword = "";
    if (r.username) authForm.username = r.username;
    authMode.value = "login";
    toast.info(`密码已重置${r.username ? `,请用「${r.username}」登录` : ",请重新登录"}`);
  } catch (e) {
    authErr.value = (e as Error).message;
  } finally {
    authBusy.value = false;
  }
}

async function submitLogin() {
  if (authMode.value === "signup") {
    await doEmailSignup();
  } else if (authMode.value === "reset") {
    await doEmailReset();
    return; // 重置成功后回到登录 tab,让用户用新密码登录
  } else {
    await doHostAuth();
  }
  if (!authErr.value) showLogin.value = false;
}

// ── owner:邮箱服务设置(SMTP 发信,注册/找回密码的邮件从这里发出) ──
const showMailCfg = ref(false);
const mailCfg = reactive({
  host: "smtp.qq.com",
  port: 465,
  user: "",
  pass: "",
  from: "",
  signupOpen: true,
  passSet: false,
  testTo: "",
});
const mailCfgBusy = ref(false);
const mailCfgErr = ref("");
async function openMailCfg() {
  showMailCfg.value = true;
  mailCfgErr.value = "";
  mailCfg.pass = "";
  try {
    const c = await collabApi.adminEmailConfig();
    mailCfg.host = c.host || "smtp.qq.com";
    mailCfg.port = c.port || 465;
    mailCfg.user = c.user;
    mailCfg.from = c.from;
    mailCfg.signupOpen = c.signupOpen;
    mailCfg.passSet = c.passSet;
  } catch (e) {
    mailCfgErr.value = (e as Error).message;
  }
}
async function saveMailCfg() {
  mailCfgErr.value = "";
  if (!mailCfg.user.trim()) {
    mailCfgErr.value = "请填发信邮箱(如 1799820934@qq.com)";
    return;
  }
  if (!mailCfg.passSet && !mailCfg.pass.trim()) {
    mailCfgErr.value = "请填 SMTP 授权码(QQ 邮箱 → 设置 → 账号 → 开启SMTP服务领取)";
    return;
  }
  mailCfgBusy.value = true;
  try {
    await collabApi.adminEmailConfigSet({
      host: mailCfg.host.trim() || "smtp.qq.com",
      port: Number(mailCfg.port) || 465,
      user: mailCfg.user.trim(),
      pass: mailCfg.pass,
      from: mailCfg.from.trim(),
      signupOpen: mailCfg.signupOpen,
      testTo: mailCfg.testTo.trim() || undefined,
    });
    mailCfg.passSet = true;
    mailCfg.pass = "";
    toast.info(
      mailCfg.testTo.trim()
        ? "已保存,测试邮件已发出 —— 去收件箱确认"
        : "邮箱服务已保存,注册/找回密码即刻可用"
    );
    showMailCfg.value = false;
    emailInfo.value = await collabApi.emailStatus().catch(() => emailInfo.value);
  } catch (e) {
    mailCfgErr.value = (e as Error).message;
  } finally {
    mailCfgBusy.value = false;
  }
}
// ── 受控远程执行(B 方案) ────────────────────────────────────────────────
// 主机侧:本机是否允许「互联上的对端在我这跑命令」。默认关,且 Shell 档位自带过期。
// 调用方:对某台远程设备开终端面板(RemoteTerminal),经隧道打它的 /api/exec。

/** 打开终端的目标远程盘;null=面板关闭。 */
const termTarget = ref<RemoteSource | null>(null);

const execPolicy = ref<{ enabled: boolean; mode: string; shell_until: number } | null>(null);
const execBusy = ref(false);

async function loadExecPolicy() {
  if (!isTauri) return;
  try {
    execPolicy.value = await invoke("exec_policy_get");
  } catch {
    /* 老版本/非桌面壳:开关不显示,不报错打扰 */
  }
}

/** 总开关。关掉时后端会顺手清掉 Shell 解锁,不留悬空授权。 */
async function toggleExec() {
  if (execBusy.value) return;
  const next = !execPolicy.value?.enabled;
  if (
    next &&
    !confirm(
      "开启后,任何经 iroh 隧道连上本机且持 owner 令牌的设备,都能在这台电脑上执行命令。\n\n" +
        "默认是白名单模式(仅在册命令、不过 shell)。确定开启?"
    )
  )
    return;
  execBusy.value = true;
  try {
    execPolicy.value = await invoke("exec_policy_set", { enabled: next, shellMinutes: null });
    toast.info(next ? "已开启远程执行 · 白名单模式" : "已关闭远程执行");
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    execBusy.value = false;
  }
}

/** 临时解锁 Shell 模式(带过期)。再点一次立即落回白名单。 */
async function toggleShellMode(minutes: number) {
  if (execBusy.value || !execPolicy.value?.enabled) return;
  const unlocking = execPolicy.value.mode !== "shell";
  if (
    unlocking &&
    !confirm(
      `Shell 模式 = 对端可在本机跑任意命令(管道/重定向可用),等同交出一个 shell。\n\n` +
        `将在 ${minutes} 分钟后自动落回白名单。确定解锁?`
    )
  )
    return;
  execBusy.value = true;
  try {
    execPolicy.value = await invoke("exec_policy_set", {
      enabled: true,
      shellMinutes: unlocking ? minutes : 0,
    });
    toast.info(unlocking ? `Shell 模式已解锁 · ${minutes} 分钟后自动回收` : "已落回白名单模式");
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    execBusy.value = false;
  }
}

/** Shell 解锁剩余分钟(向上取整);0=未解锁/已过期。 */
const shellLeftMin = computed(() => {
  const u = execPolicy.value?.shell_until ?? 0;
  if (!u) return 0;
  return Math.max(0, Math.ceil((u * 1000 - Date.now()) / 60000));
});

/** 使用盘:带着目标远程源直接跳文件中心的远程浏览(FileCenter onMounted 接棒)。 */
function browseDisk(s: RemoteSource) {
  sessionStorage.setItem("polaris.fc.openRemote", s.id);
  app.setView("file_center");
}

// ── 远程盘挂载:连上对端后自动把它挂成本机系统盘符(Z:/Y:…),Tailscale 式「连上即多一块盘」──
// 后端看门狗每 15s 保隧道 + 保盘符,断线自愈;这里只负责发起与展示。
const mountMap = ref<Record<string, MountStatus>>({});

async function loadMounts() {
  if (!isTauri) return;
  try {
    const list = await fsmount.status();
    const m: Record<string, MountStatus> = {};
    for (const st of list) m[st.sourceId] = st;
    mountMap.value = m;
  } catch {
    /* 老版本/非主机构建:无此命令,不打扰 */
  }
}

// Windows WebClient 单文件上限(挂载盘读大文件的系统闸,默认 50MB,可一次 UAC 解到 4GB)。
const webdavUnlocked = ref(true); // 默认 true:非 Windows / 老后端不弹横幅
const webdavBusy = ref(false);
const isWindows = navigator.userAgent.includes("Windows");

async function loadWebdavLimit() {
  if (!isTauri || !isWindows) return;
  try {
    webdavUnlocked.value = (await fsmount.webdavLimit()).unlocked;
  } catch {
    /* 老版本无此命令 */
  }
}

async function unlockWebdav() {
  webdavBusy.value = true;
  try {
    const r = await fsmount.webdavUnlock();
    webdavUnlocked.value = r.unlocked;
    toast.info(r.unlocked ? "已解锁:挂载盘单文件上限 4GB(更大的走文件中心下载)" : "解锁没生效,可重试");
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    webdavBusy.value = false;
  }
}

/** 挂载一个远程源(幂等)。quiet = 启动自动恢复时静默,别一开机弹一串 toast。 */
async function mountRemote(s: RemoteSource, quiet = false) {
  if (!isTauri) return;
  try {
    const st = await fsmount.mount(s);
    mountMap.value = { ...mountMap.value, [s.id]: st };
    if (quiet) return;
    if (st.drive) {
      const rw = st.writable ? "可读写" : "只读";
      const desk = st.shortcut ? ",桌面已放一个盘图标" : "";
      toast.info(`「${s.name}」已挂载为 ${st.drive} 盘(${rw})${desk}`);
    } else {
      toast.info(`「${s.name}」隧道握手中,通了会自动挂上盘符,无需再操作`);
    }
  } catch (e) {
    if (!quiet) toast.error(`挂盘失败:${(e as Error).message}`);
  }
}

// ── 同账号设备网:入网一次,自己账号的设备此后自动互连自动挂盘 ──
// 与「手工粘连接码」并存(不替代):粘码仍是接别人 NAS / 临时设备的路子,
// 设备网只管**自己账号**的机器。
interface MeshPeer {
  nodeId: string;
  name: string;
  port: number;
  connected: boolean;
  error: string;
  /** 对端本机会话 token(远程终端/浏览盘用)。由设备密钥自助换来,不是云端给的。 */
  token: string;
  drive: string;
  writable: boolean;
  ok: boolean;
}
interface MeshStatus {
  enrolled: boolean;
  url: string;
  uid: string;
  nodeId: string;
  name: string;
  peers: MeshPeer[];
}
const mesh = ref<MeshStatus>({ enrolled: false, url: "", uid: "", nodeId: "", name: "", peers: [] });
const meshForm = reactive({ open: false, url: "", username: "", password: "" });
const meshBusy = ref(false);
const meshMounted = computed(() => mesh.value.peers.filter((p) => !!p.drive).length);

async function loadMesh() {
  if (!isTauri) return;
  try {
    mesh.value = await invoke<MeshStatus>("mesh_status");
    // 已入网的机器不必再看那张表单(退网后会自己回来)。
    if (mesh.value.enrolled) meshForm.open = false;
  } catch {
    /* 老版本/非主机构建:整张卡不出现 */
  }
}

async function meshJoin() {
  if (meshBusy.value) return;
  if (!meshForm.url.trim() || !meshForm.username.trim() || !meshForm.password) {
    toast.error("云端账号中心地址、账号、密码都要填");
    return;
  }
  meshBusy.value = true;
  try {
    await invoke("mesh_join", {
      url: meshForm.url.trim(),
      username: meshForm.username.trim(),
      password: meshForm.password,
    });
    meshForm.password = ""; // 用完即弃,不留在内存里等着被截图
    toast.info("已入网 —— 正在后台自动连上同账号的其它设备");
    await loadMesh();
  } catch (e) {
    toast.error(`入网失败:${(e as Error).message}`);
  } finally {
    meshBusy.value = false;
  }
}

async function meshLeave() {
  if (meshBusy.value) return;
  if (!confirm("退出设备网?本机自动挂上的远程盘会一并卸掉(手工添加的远程源不受影响)。")) return;
  meshBusy.value = true;
  try {
    await invoke("mesh_leave");
    toast.info("已退网");
    await loadMesh();
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    meshBusy.value = false;
  }
}

/** 立刻对一次账(不等 60s 心跳)。 */
async function meshRefresh() {
  if (meshBusy.value) return;
  meshBusy.value = true;
  try {
    await invoke("mesh_sync");
    await loadMesh();
  } catch (e) {
    toast.error(`对账失败:${(e as Error).message}`);
  } finally {
    meshBusy.value = false;
  }
}

/** 把一台设备移出设备网(丢了电脑时用)。它需要重新登录账号才能再进来。 */
async function meshKick(p: MeshPeer) {
  if (meshBusy.value) return;
  if (!confirm(`把「${p.name}」移出设备网?\n\n它将无法再自动连入你的设备(重新登录账号可恢复)。`)) return;
  meshBusy.value = true;
  try {
    await invoke("mesh_kick", { nodeId: p.nodeId });
    toast.info("已移出设备网");
    await loadMesh();
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    meshBusy.value = false;
  }
}

// ── 我共享出去的盘:本机开放哪些目录给互联对端,以及**哪些允许写**(落库,桌面双击也生效) ──
/** 一个共享目录:路径 + 是否允许对端写入。写权限默认关,由用户逐个点开。 */
interface ShareItem {
  path: string;
  write: boolean;
}
const shareItems = ref<ShareItem[]>([]);
const shareBusy = ref(false);
const writableCount = computed(() => shareItems.value.filter((d) => d.write).length);

async function loadShareRoots() {
  if (!isTauri) return;
  try {
    shareItems.value = (await invoke<ShareItem[]>("fs_share_list")) ?? [];
  } catch {
    // 老后端只有 fs_share_get(纯路径 = 只读):退化读一次,别让页面空着。
    try {
      const paths = (await invoke<string[]>("fs_share_get")) ?? [];
      shareItems.value = paths.map((p) => ({ path: p, write: false }));
    } catch {
      /* 非主机构建:不显示,不打扰 */
    }
  }
}

/** 覆盖式保存;后端会校验每条都是已存在目录,失败整体不落。 */
async function saveShareItems(next: ShareItem[], note?: string) {
  shareBusy.value = true;
  try {
    shareItems.value = (await invoke<ShareItem[]>("fs_share_save", { items: next })) ?? [];
    toast.info(note ?? (shareItems.value.length ? "已更新共享目录" : "已停止共享(清单为空)"));
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    shareBusy.value = false;
  }
}

/** 弹目录选择器,追加一个共享目录(新加的一律先只读)。 */
async function addShareRoot() {
  if (shareBusy.value) return;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({
    directory: true,
    multiple: false,
    title: "选择要共享给互联设备的目录",
  });
  if (typeof picked !== "string" || !picked) return;
  if (shareItems.value.some((d) => d.path === picked)) {
    toast.info("这个目录已在共享清单里");
    return;
  }
  await saveShareItems([...shareItems.value, { path: picked, write: false }]);
}

/** 切换某个目录的写权限。开写是有后果的动作,故明确二次确认一次。 */
async function toggleShareWrite(item: ShareItem) {
  if (shareBusy.value) return;
  const next = !item.write;
  if (next && !confirm(`允许互联设备写入「${item.path}」?\n\n对端将能在这个目录里新建、修改和删除文件。`)) {
    return;
  }
  await saveShareItems(
    shareItems.value.map((d) => (d.path === item.path ? { ...d, write: next } : d)),
    next ? "已允许对端写入该目录" : "已改回只读",
  );
}

/** 从清单里移掉某个目录并保存。 */
async function removeShareRoot(dir: string) {
  if (shareBusy.value) return;
  await saveShareItems(shareItems.value.filter((d) => d.path !== dir));
}

// ── 应用直投:把本机在跑的 HTTP 应用发布给手机(电脑当服务器,手机只是视图) ──
const pubApps = ref<PubApp[]>([]);
const portHits = ref<PortHit[]>([]);
const appsBusy = ref(false);
const scanning = ref(false);

async function loadApps() {
  if (!isTauri) return;
  try {
    pubApps.value = (await appsApi.list()) ?? [];
  } catch {
    /* 老版本主机没这命令:不显示,不打扰 */
  }
}

/** 扫本机在说 HTTP 的端口。Tauri/Electron 生产包不开端口,扫不到很正常。 */
async function scanPorts() {
  if (scanning.value) return;
  scanning.value = true;
  try {
    portHits.value = (await appsApi.scan()) ?? [];
    if (!portHits.value.length) {
      toast.info("本机没扫到在说 HTTP 的端口 —— Tauri/Electron 生产包默认不开端口,投不了");
    }
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    scanning.value = false;
  }
}

async function saveApps(next: PubApp[]) {
  appsBusy.value = true;
  try {
    pubApps.value = (await appsApi.set(next)) ?? [];
    portHits.value = portHits.value.map((h) => ({
      ...h,
      published: pubApps.value.some((a) => a.port === h.port),
    }));
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    appsBusy.value = false;
  }
}

/** 把一个扫到的端口发布出去。短名按端口自动起,重名自动加序号。 */
async function publishPort(h: PortHit) {
  const base = (h.title || h.server || `app${h.port}`)
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 20);
  let slug = base || `app${h.port}`;
  let i = 2;
  while (pubApps.value.some((a) => a.slug === slug)) slug = `${base}-${i++}`;
  await saveApps([
    ...pubApps.value,
    { slug, name: h.title || `本机 ${h.port} 端口`, port: h.port, path: "/", note: h.server },
  ]);
  toast.info(`已发布「${slug}」—— 手机端「应用」页即可打开`);
}

async function unpublishApp(slug: string) {
  await saveApps(pubApps.value.filter((a) => a.slug !== slug));
}

/** 在本机系统浏览器里打开(验证代理确实通了,也方便自己在电脑上用第二个窗口)。 */
async function openAppLocally(a: PubApp) {
  const port = collab.hostInfo?.port;
  if (!port) {
    toast.error("本机主机没在跑,无法打开");
    return;
  }
  try {
    const { url } = await appsApi.open(a.slug);
    await openUrl(`http://127.0.0.1:${port}${url}`);
  } catch (e) {
    toast.error((e as Error).message);
  }
}

// ── 隔空同屏:本机挑一个文件,包装成网页推给手机,两端同看一页(走 iroh,不要求同一 WiFi) ──

/** 在 app 内直接读方案说明:后端把随包分发的说明页拷进白名单目录并返回路径,再走同屏舞台打开。 */
async function openBeamDoc() {
  try {
    const p = await invoke<string>("beam_doc_path");
    requestBeamOpen(p);
  } catch (e) {
    toast.error((e as Error).message);
  }
}

async function beamPick() {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({
    multiple: false,
    title: "选择要与手机同屏的文件",
    filters: [
      { name: "可同屏的文件", extensions: ["md", "html", "htm", "svg", "txt", "pdf", "png", "jpg", "jpeg", "webp", "gif", "mp4", "mp3", "json", "csv", "log"] },
      { name: "所有文件", extensions: ["*"] },
    ],
  });
  if (typeof picked !== "string" || !picked) return;
  // 真正的打包/广播在 BeamStage 里做(它同时是接收端,状态与工具条都在那儿)。
  requestBeamOpen(picked);
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

// ── 隧道真实链路:后端 collab_tunnel_status.tunnels 逐条给出 direct(打洞直连)/relay(中继) ──
const tunnelByPort = ref<Record<number, { state: string; path: string }>>({});
async function pollTunnelPaths() {
  if (!isTauri) return;
  try {
    const s = await invoke<{ tunnels?: { port: number; state: string; path: string }[] }>(
      "collab_tunnel_status"
    );
    const map: Record<number, { state: string; path: string }> = {};
    for (const t of s?.tunnels ?? []) map[t.port] = { state: t.state, path: t.path };
    tunnelByPort.value = map;
  } catch {
    /* 状态拉不到不影响功能,徽标保持上一帧 */
  }
}
/** 远程盘链路徽标:中继兜底如实标 relay,其余(直连/建立中)标 p2p。 */
function diskTransport(s: RemoteSource): Transport {
  return tunnelByPort.value[s.port]?.path === "relay" ? "relay" : "p2p";
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
  // NAS/远程盘链路按后端上报如实标:打洞直连=P2P(蓝),走中继=relay,不再写死。
  const disks: TopoEntity[] = remotes.value.map((s) => ({
    kind: "disk",
    name: s.name || "远程盘",
    emoji: "🗄",
    revoked: false,
    nodeId: s.nodeId || "",
    t: diskTransport(s),
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
  let nodeId = addForm.nodeId.trim();
  let token = addForm.token.trim();
  if (!nodeId) {
    toast.error("粘对方「互联」页的连接码(PLRK1-…),或填 iroh NodeId");
    return;
  }
  // 整串 PLRK1 连接码直接粘:解出 NodeId + owner 令牌,三端(手机/桌面/NAS)同一串码。
  const conn = parseConnectCode(nodeId);
  if (conn) {
    if (!conn.nodeId) {
      toast.error("这串连接码没带 P2P NodeId(对方 iroh 还没就绪)—— 让对方刷新「互联」页后重新复制");
      return;
    }
    nodeId = conn.nodeId;
    if (conn.token) token = conn.token; // 码里自带 owner 令牌,手填的可省
  } else if (nodeId.startsWith("PLRS1-")) {
    toast.error("这是邀请码(PLRS1,给别人入伙用)。接入远程主机请粘对方「互联」页的连接码(PLRK1)");
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
      token,
      createdAt: Date.now(),
    };
    remotes.value = upsertRemoteSource(src);
    addForm.open = false;
    addForm.nodeId = "";
    addForm.token = "";
    // 连上即自动挂盘:它的共享目录变成本机一块新盘符(结果由 mountRemote 弹 toast)。
    await mountRemote(src);
  } catch (e) {
    toast.error(`连接失败:${(e as Error).message}`);
  } finally {
    connBusy.value = false;
  }
}
function forgetRemote(s: RemoteSource) {
  if (!confirm(`断开并移除「${s.name}」?`)) return;
  // 先卸盘(删盘符 + 拆 WebDAV 桥),再断隧道:不然资源管理器里留一块死盘。
  if (isTauri) fsmount.unmount(s.id).catch(() => {});
  // 真断隧道:不然孤儿隧道在后台对着已移除的主机永远重连(占端口+空耗流量)。
  if (isTauri) invoke("collab_tunnel_disconnect", { listenPort: s.port }).catch(() => {});
  delete mountMap.value[s.id];
  remotes.value = removeRemoteSource(s.id);
  toast.info("已断开并移除远程源");
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
    // 盘符自动恢复:挂载命令幂等,后端看门狗接力等隧道通(静默,别一开机弹一串 toast)。
    mountRemote(s, true);
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
  loadExecPolicy(); // 远程执行开关档位(主机侧)
  loadShareRoots(); // 我共享出去的盘目录(主机侧)
  loadApps(); // 已发布给手机的本机应用
  loadMounts(); // 已挂载的远程盘盘符(徽标)
  loadWebdavLimit(); // 挂载盘大文件解锁横幅要不要出现
  loadMesh(); // 同账号设备网现状(入网了吗、名册上有谁、挂了哪些盘)
  pollRemoteStats(); // 首屏就拉一次 NAS/远程盘实况(它们在「我的设备」里)
  pollTunnelPaths(); // 首屏也拉一次隧道链路,星图别先画个写死的 P2P
  // 本机仪表每 4s 跳一帧。盘实况:启动后前 ~40s 每 4s 密集试(iroh 握手要几秒,
  // 首拉大概率没通,别让用户等 12s 才看到数字),之后回落到每 12s 一次。
  let tick = 0;
  let warmup = 10; // 前 10 拍(40s)密集拉
  statTimer = setInterval(() => {
    // 窗口最小化/切后台时整拍跳过:本机采样+远程 invoke 全省(回前台下一拍即恢复,
    // 最多迟 4s,仪表盘场景无感)。warmup 拍也不消耗,断网启动的用户回来仍享密集试。
    if (document.hidden) return;
    sampleLocal();
    tick++;
    const due = warmup > 0 ? true : tick % 3 === 0;
    if (warmup > 0) warmup--;
    if (due && (tab.value === "devices" || tab.value === "topo")) {
      pollRemoteStats(); // 盘实况(任何分区都拉:mine 默认显示它们)
      pollTunnelPaths(); // 隧道真实链路(直连/中继),卡片与星图共用
      loadMounts(); // 盘符徽标(本地查注册表,零网络开销)
      loadMesh(); // 设备网名册(纯本地读进程内状态,不打云端)
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
  if (codeTimer) clearInterval(codeTimer);
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

            <div v-if="isTauri && execPolicy" class="lan-toggle" :class="{ busy: execBusy }" @click="toggleExec">
              <div class="lt-txt">
                <span class="lt-title">允许互联设备在本机执行命令</span>
                <span class="lt-sub">{{ execPolicy.enabled
                  ? (execPolicy.mode === "shell"
                      ? `已开 · Shell 模式(${shellLeftMin} 分钟后自动落回白名单)`
                      : "已开 · 白名单模式,仅在册命令且不过 shell")
                  : "关 · 对端调 /api/exec 一律拒绝(默认)" }}</span>
              </div>
              <span class="switch" :class="{ on: execPolicy.enabled }"><i></i></span>
            </div>
            <div v-if="isTauri && execPolicy?.enabled" class="exec-shell">
              <button class="pill ghost" :class="{ hot: execPolicy.mode === 'shell' }" @click="toggleShellMode(30)">
                {{ execPolicy.mode === "shell" ? "立即落回白名单" : "临时解锁 Shell 模式(30 分钟)" }}
              </button>
              <span class="ex-note">
                白名单模式够日常用(git/npm/cargo/claude…)。需要管道、重定向或在册外的命令时才解锁 Shell,到点自动回收。
              </span>
            </div>

            <!-- 我共享出去的盘:开放哪些目录给互联对端,逐目录点选是否允许写入 -->
            <div v-if="isTauri" class="fs-share" :class="{ busy: shareBusy }">
              <div class="fss-head">
                <div class="lt-txt">
                  <span class="lt-title">共享给互联设备的目录</span>
                  <span class="lt-sub">{{ shareItems.length
                    ? `已开放 ${shareItems.length} 个目录(${writableCount} 个可写) · 对端连上即挂成一块盘`
                    : "未开放任何目录 · 对端挂上的盘是空的" }}</span>
                </div>
                <button class="pill" :disabled="shareBusy" @click="addShareRoot">
                  <FolderInput :size="13" /> 添加目录
                </button>
              </div>
              <ul v-if="shareItems.length" class="fss-list">
                <li v-for="d in shareItems" :key="d.path">
                  <HardDrive :size="13" class="fss-ic" />
                  <span class="fss-path" :title="d.path">{{ d.path }}</span>
                  <!-- 点选放开写:默认关。开了对端就能在这块盘里直接改/删/拖入 -->
                  <label class="fss-w" :class="{ on: d.write }"
                    :title="d.write
                      ? '对端可在此目录内新建/修改/删除文件 —— 点一下改回只读'
                      : '当前只读。点一下允许对端写入此目录'">
                    <input type="checkbox" :checked="d.write" :disabled="shareBusy"
                      @change="toggleShareWrite(d)" />
                    <component :is="d.write ? Pencil : Eye" :size="12" />
                    {{ d.write ? "可写" : "只读" }}
                  </label>
                  <button class="fss-x" title="停止共享此目录" :disabled="shareBusy" @click="removeShareRoot(d.path)">
                    <ShieldOff :size="13" />
                  </button>
                </li>
              </ul>
              <span class="ex-note">
                路径关押在所选目录内(拒 <code>..</code> 与符号链接逃逸),仅同账号/持 owner 令牌的设备可见。
                <b>写权限默认关</b> —— 打开后对端能删这个目录里的东西,只开你真需要共享编辑的目录。
              </span>
            </div>

            <!-- 隔空同屏:把一个文件包装成网页,两端同看一页(手机侧在预览页点投屏钮也能发起) -->
            <div v-if="isTauri" class="fs-share">
              <div class="fss-head">
                <div class="lt-txt">
                  <span class="lt-title">隔空同屏</span>
                  <span class="lt-sub">
                    把文件包装成一页自包含网页,手机与电脑同时打开<b>同一页</b>,滚动与指点实时互传 ——
                    走 iroh 打洞/中继,<b>不要求同一 WiFi</b>。
                  </span>
                </div>
                <button class="pill ghost" title="在应用内直接读这套方案说明" @click="openBeamDoc">
                  <GraduationCap :size="13" /> 看说明
                </button>
                <button class="pill" @click="beamPick">
                  <MonitorSmartphone :size="13" /> 选文件投屏
                </button>
              </div>
              <span class="ex-note">
                手机侧:文件预览页右上角的投屏钮。可同屏的文件与「文件预览」同一把白名单闸
                (知识库 / ~/Polaris 产物 / 项目工作目录)。<b>手机横过来即自动切电脑版式。</b>
              </span>
            </div>

            <!-- 应用直投:把本机在跑的 HTTP 应用发布给手机 —— 电脑当服务器,手机只跑视图 -->
            <div v-if="isTauri" class="fs-share" :class="{ busy: appsBusy }">
              <div class="fss-head">
                <div class="lt-txt">
                  <span class="lt-title">应用直投(把电脑当服务器)</span>
                  <span class="lt-sub">
                    发布本机在跑的应用,手机上操作的是<b>真实应用本体</b>,计算全在电脑跑 ——
                    传的是 HTTP 报文不是画面,比远程桌面清晰、省流、可选可搜、触摸原生。
                  </span>
                </div>
                <button class="pill" :disabled="scanning" @click="scanPorts">
                  <Radio :size="13" /> {{ scanning ? "扫描中…" : "扫描本机应用" }}
                </button>
              </div>

              <ul v-if="pubApps.length" class="fss-list">
                <li v-for="a in pubApps" :key="a.slug">
                  <Cpu :size="13" class="fss-ic" />
                  <span class="fss-path" :title="`127.0.0.1:${a.port}${a.path}`">
                    {{ a.name }} · <code>/{{ a.slug }}</code> → :{{ a.port }}
                  </span>
                  <button class="fss-x" title="在本机浏览器打开" :disabled="appsBusy" @click="openAppLocally(a)">
                    <Globe :size="13" />
                  </button>
                  <button class="fss-x" title="下架" :disabled="appsBusy" @click="unpublishApp(a.slug)">
                    <ShieldOff :size="13" />
                  </button>
                </li>
              </ul>

              <ul v-if="portHits.length" class="fss-list">
                <li v-for="h in portHits" :key="h.port">
                  <Zap :size="13" class="fss-ic" />
                  <span class="fss-path">
                    :{{ h.port }} · {{ h.title || h.server || "HTTP 服务" }}
                    <span class="faint">(HTTP {{ h.status }})</span>
                  </span>
                  <button
                    v-if="!h.published"
                    class="pill"
                    :disabled="appsBusy"
                    @click="publishPort(h)"
                  >
                    <Plus :size="12" /> 发布
                  </button>
                  <span v-else class="faint" style="font-size: 11px">已发布</span>
                </li>
              </ul>

              <span class="ex-note">
                只能投<b>开了 TCP HTTP 端口</b>的应用(开发服务器、带内置 HTTP 后端或 sidecar 的桌面应用)。
                <b>Tauri / Electron 生产包默认走 <code>tauri://</code> / <code>file://</code>,不开端口 → 投不了</b>,
                需要时开它的 dev server 再扫。发布出去的应用与 polaris 同源,只发布你自己信得过的。
              </span>
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
              <button v-if="owner" class="ra-btn" title="配置 SMTP,开通邮箱注册/找回密码" @click="openMailCfg"><Mail :size="13" /> 邮箱服务</button>
              <button class="ra-btn" @click="doLogout"><LogOut :size="13" /> 登出</button>
            </template>
            <button v-else class="ra-btn pri" @click="openLogin">登录 / 注册账号</button>
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
          <!-- 同账号设备网:登录一次云端账号中心,自己的设备此后自动互连自动挂盘 -->
          <section v-if="isTauri" class="glass mesh-card" :class="{ busy: meshBusy }">
            <div class="mesh-head">
              <span class="mesh-ic" :class="{ on: mesh.enrolled }"><Network :size="16" :stroke-width="2" /></span>
              <div class="lt-txt">
                <span class="lt-title">同账号设备网{{ mesh.enrolled ? "" : "(推荐)" }}</span>
                <span class="lt-sub">
                  <template v-if="mesh.enrolled">
                    已入网 · 账号 <code>{{ mesh.uid || "—" }}</code> ·
                    名册 {{ mesh.peers.length }} 台,已挂 {{ meshMounted }} 块盘 —— 开机自动接,不用再粘连接码
                  </template>
                  <template v-else>
                    填一次云端账号中心 + 账号密码,<b>此后同一账号的电脑/NAS 自己就成网</b>:
                    自动打洞、自动把对方共享目录挂成本机盘符。密码不落盘,换的是一把可单独吊销的设备密钥。
                  </template>
                </span>
              </div>
              <button v-if="mesh.enrolled" class="pill ghost" :disabled="meshBusy" @click="meshRefresh">
                <RefreshCw :size="13" /> 立即对账
              </button>
              <button v-if="mesh.enrolled" class="pill ghost" :disabled="meshBusy" @click="meshLeave">退网</button>
              <button v-else class="pill" :disabled="meshBusy" @click="meshForm.open = !meshForm.open">
                <Plus :size="13" /> 入网
              </button>
            </div>

            <!-- 入网表单:只在没入网时出现,填完就再也不用看见它 -->
            <div v-if="meshForm.open && !mesh.enrolled" class="mesh-form">
              <div class="add-fields">
                <input v-model="meshForm.url" class="af-inp" placeholder="云端账号中心地址(如 http://43.139.209.127:8080)" />
                <input v-model="meshForm.username" class="af-inp" placeholder="账号(用户名或邮箱)" />
                <input v-model="meshForm.password" class="af-inp" type="password" placeholder="密码" />
              </div>
              <button class="cta" style="margin-top:8px" :disabled="meshBusy" @click="meshJoin">
                <LoaderCircle v-if="meshBusy" :size="15" class="spin" /><Zap v-else :size="15" /> 入网并自动连上我的设备
              </button>
              <p class="foot-note" style="margin-top:6px">
                需要本机已「设为主机」(要有 P2P 身份)。对端也得用同一账号入网,且已把这个账号加成它的成员 ——
                云端只证明「你是谁」,进不进得去仍由每台机器自己说了算。
              </p>
            </div>

            <!-- 名册:每台设备连上没、挂成哪块盘、读写档位 -->
            <ul v-if="mesh.enrolled && mesh.peers.length" class="fss-list">
              <li v-for="p in mesh.peers" :key="p.nodeId">
                <component :is="p.connected ? HardDrive : LoaderCircle" :size="13" class="fss-ic"
                  :class="{ spin: !p.connected }" />
                <span class="fss-path" :title="p.nodeId">{{ p.name }}</span>
                <span v-if="p.drive" class="host-badge drive-badge" :class="{ dim: !p.ok }">{{ p.drive }} 盘</span>
                <span v-if="p.drive" class="host-badge rw-badge" :class="{ rw: p.writable }">
                  {{ p.writable ? "读写" : "只读" }}
                </span>
                <span v-else-if="p.error" class="mesh-err" :title="p.error">接入未成</span>
                <!-- 远程终端:在这台设备上跑命令(受对端自己的执行策略约束) -->
                <button
                  v-if="p.connected"
                  class="fss-x"
                  title="在这台设备上跑命令(受它自己的远程执行策略约束)"
                  @click="termTarget = { id: p.nodeId, name: p.name, nodeId: p.nodeId, port: p.port, token: p.token, createdAt: 0 }"
                >
                  <Terminal :size="13" />
                </button>
                <button class="fss-x" title="把这台设备移出设备网" :disabled="meshBusy" @click="meshKick(p)">
                  <ShieldOff :size="13" />
                </button>
              </li>
            </ul>
            <p v-if="mesh.enrolled && !mesh.peers.length" class="foot-note">
              名册里还只有本机。在你的另一台电脑 / NAS 上用<b>同一个账号</b>入网,这里就会自动出现它,并挂上它的盘。
            </p>
          </section>

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
                <p class="foot-note" style="margin:2px 0 8px">粘对方「互联」页复制的<b>连接码(PLRK1-…)</b>即可,NodeId 与 owner 令牌自动带出;也可手动分开填。自动打洞直连,打不通走中继。</p>
                <div class="add-fields">
                  <input v-model="addForm.name" class="af-inp" placeholder="名称(如:群晖 NAS)" />
                  <input v-model="addForm.nodeId" class="af-inp" placeholder="连接码(PLRK1-…)或 NodeId" />
                  <input v-model="addForm.token" class="af-inp" placeholder="owner 令牌(粘连接码可不填)" />
                </div>
                <button class="cta" style="margin-top:8px" :disabled="connBusy" @click="connectRemote">
                  <LoaderCircle v-if="connBusy" :size="15" class="spin" /><Zap v-else :size="15" /> 发起连接
                </button>
              </div>
            </div>
            <button class="ap-close pill ghost" @click="addForm.open = false">收起</button>
          </section>

          <!-- 挂载盘大文件解锁:Windows WebClient 默认单文件 50MB,一次 UAC 解到 4GB -->
          <div
            v-if="isTauri && isWindows && !webdavUnlocked && Object.keys(mountMap).length"
            class="fs-share"
            style="margin: 0 0 10px"
          >
            <div class="fss-head">
              <div class="lt-txt">
                <span class="lt-title">挂载盘大文件解锁</span>
                <span class="lt-sub">
                  远程盘符走 Windows 自带 WebDAV,单文件默认限 <b>50MB</b>。解锁到 4GB 只需一次管理员授权;
                  超过 4GB 的文件请在「文件中心 · 远程源」下载(无上限、断点续传)。
                </span>
              </div>
              <button class="pill" :disabled="webdavBusy" @click="unlockWebdav">
                <HardDrive :size="13" /> {{ webdavBusy ? "等待授权…" : "解锁到 4GB" }}
              </button>
            </div>
          </div>

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
                    <span
                      v-if="c.kind === 'disk' && c.src && mountMap[c.src.id]?.drive"
                      class="host-badge drive-badge"
                      :class="{ dim: !mountMap[c.src.id].ok }"
                      :title="mountMap[c.src.id].ok ? '已挂载为本机盘,资源管理器直接用' : '盘符还在,对端暂时失联,看门狗重连中'"
                    >{{ mountMap[c.src.id].drive }} 盘</span>
                    <!-- 读写档位:对端把目录改成可写后 15s 内这里会自己变 -->
                    <span
                      v-if="c.kind === 'disk' && c.src && mountMap[c.src.id]?.drive"
                      class="host-badge rw-badge"
                      :class="{ rw: mountMap[c.src.id].writable }"
                      :title="mountMap[c.src.id].writable
                        ? '这块盘可读写:直接在资源管理器里改/删/拖入'
                        : '这块盘只读 —— 让对端在「我共享的盘」里给目录打开写权限'"
                    >{{ mountMap[c.src.id].writable ? "读写" : "只读" }}</span>
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
                  <button
                    v-if="!mountMap[c.src!.id]?.drive"
                    class="b"
                    title="把它的共享目录挂成本机盘符(Z:/Y:…)"
                    @click="mountRemote(c.src!)"
                  ><HardDrive :size="13" /> 挂成盘符</button>
                  <button class="b" @click="browseDisk(c.src!)"><FolderInput :size="13" /> 浏览盘</button>
                  <button class="b" title="在它上面跑命令(受对端策略约束)" @click="termTarget = c.src!">
                    <Terminal :size="13" /> 终端
                  </button>
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
              <span class="ac-s">手机、桌面、NAS 都认同一串连接码</span>
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

    <!-- 登录 / 注册 / 忘记密码 弹层(桌面互联页切账号) -->
    <Teleport to="body">
      <div v-if="showLogin" class="login-mask" @click.self="showLogin = false">
        <div class="glass login-box">
          <div class="lb-head">
            <span class="lb-av" v-html="allianceAvatar"></span>
            <div>
              <div class="lb-title">{{ needsBootstrap ? "创建 owner 账号"
                : authMode === "signup" ? "注册账号"
                : authMode === "reset" ? "找回密码" : "登录账号" }}</div>
              <div class="lb-sub">{{ needsBootstrap ? "本机还没账号,建一个 owner"
                : authMode === "signup" ? "邮箱验证注册,注册即登录"
                : authMode === "reset" ? "验证码发到绑定邮箱,重设密码" : "登录后管理设备联盟" }}</div>
            </div>
            <button class="icobtn" @click="showLogin = false">✕</button>
          </div>

          <!-- 首建 owner:保持原单表单 -->
          <template v-if="needsBootstrap">
            <input v-model="authForm.username" class="af-inp" placeholder="用户名" autocapitalize="off" />
            <input v-model="authForm.password" type="password" class="af-inp" placeholder="密码(至少 8 位)" @keydown.enter="submitLogin" />
            <input v-model="authForm.displayName" class="af-inp" placeholder="昵称(可选)" />
          </template>

          <template v-else>
            <!-- 登录 / 注册 / 忘记密码 三 tab -->
            <div class="lb-tabs">
              <button class="lb-tab" :class="{ on: authMode === 'login' }" @click="authMode = 'login'; authErr = ''">登录</button>
              <button class="lb-tab" :class="{ on: authMode === 'signup' }" @click="authMode = 'signup'; authErr = ''">注册</button>
              <button class="lb-tab" :class="{ on: authMode === 'reset' }" @click="authMode = 'reset'; authErr = ''">忘记密码</button>
            </div>

            <template v-if="authMode === 'login'">
              <input v-model="authForm.username" class="af-inp" placeholder="用户名" autocapitalize="off" />
              <input v-model="authForm.password" type="password" class="af-inp" placeholder="密码" @keydown.enter="submitLogin" />
            </template>

            <template v-else-if="authMode === 'signup'">
              <template v-if="emailInfo?.signupOpen">
                <div class="lb-code-row">
                  <input v-model="authForm.email" class="af-inp" placeholder="邮箱(如 xxx@qq.com)" autocapitalize="off" />
                  <button class="pill code-btn" :disabled="codeCooldown > 0" @click="sendCode">
                    {{ codeCooldown > 0 ? `${codeCooldown}s` : "发验证码" }}
                  </button>
                </div>
                <input v-model="authForm.code" class="af-inp" placeholder="邮箱验证码(6 位)" inputmode="numeric" />
                <input v-model="authForm.username" class="af-inp" placeholder="用户名(3-32 位字母数字)" autocapitalize="off" />
                <input v-model="authForm.password" type="password" class="af-inp" placeholder="密码(至少 8 位)" @keydown.enter="submitLogin" />
                <input v-model="authForm.displayName" class="af-inp" placeholder="昵称(可选)" />
              </template>
              <p v-else class="lb-hint">
                {{ emailInfo && emailInfo.configured
                  ? "管理员已关闭邮箱自助注册 —— 请找管理员要一张邀请票据入伙。"
                  : "主机还没配置邮箱服务:管理员登录后点侧栏「邮箱服务」,填 QQ 邮箱 SMTP 授权码即可开通注册与找回密码。" }}
              </p>
            </template>

            <template v-else>
              <template v-if="emailInfo?.configured">
                <div class="lb-code-row">
                  <input v-model="authForm.email" class="af-inp" placeholder="注册时绑定的邮箱" autocapitalize="off" />
                  <button class="pill code-btn" :disabled="codeCooldown > 0" @click="sendCode">
                    {{ codeCooldown > 0 ? `${codeCooldown}s` : "发验证码" }}
                  </button>
                </div>
                <input v-model="authForm.code" class="af-inp" placeholder="邮箱验证码(6 位)" inputmode="numeric" />
                <input v-model="authForm.newPassword" type="password" class="af-inp" placeholder="新密码(至少 8 位)" @keydown.enter="submitLogin" />
              </template>
              <p v-else class="lb-hint">主机还没配置邮箱服务,暂不能自助找回 —— 请联系管理员重置密码。</p>
            </template>
          </template>

          <p v-if="authErr" class="lb-err">{{ authErr }}</p>
          <button
            v-if="needsBootstrap || authMode === 'login' || (authMode === 'signup' && emailInfo?.signupOpen) || (authMode === 'reset' && emailInfo?.configured)"
            class="cta full" :disabled="authBusy" @click="submitLogin">
            <LoaderCircle v-if="authBusy" :size="15" class="spin" />
            {{ needsBootstrap ? "创建并登录"
              : authMode === "signup" ? "注册并登录"
              : authMode === "reset" ? "重设密码" : "登录" }}
          </button>
        </div>
      </div>
    </Teleport>

    <!-- owner:邮箱服务设置(SMTP)弹层 -->
    <Teleport to="body">
      <div v-if="showMailCfg" class="login-mask" @click.self="showMailCfg = false">
        <div class="glass login-box mail-box">
          <div class="lb-head">
            <Mail :size="22" style="flex:none" />
            <div>
              <div class="lb-title">邮箱服务设置</div>
              <div class="lb-sub">注册 / 找回密码的验证码邮件从这个邮箱发出</div>
            </div>
            <button class="icobtn" @click="showMailCfg = false">✕</button>
          </div>
          <div class="lb-code-row">
            <input v-model="mailCfg.host" class="af-inp" placeholder="SMTP 服务器(smtp.qq.com)" />
            <input v-model.number="mailCfg.port" class="af-inp port-inp" placeholder="465" inputmode="numeric" />
          </div>
          <input v-model="mailCfg.user" class="af-inp" placeholder="发信邮箱(如 1799820934@qq.com)" autocapitalize="off" />
          <input v-model="mailCfg.pass" type="password" class="af-inp"
            :placeholder="mailCfg.passSet ? 'SMTP 授权码(已配置,留空不改)' : 'SMTP 授权码(QQ邮箱→设置→账号→开启SMTP领取)'" />
          <input v-model="mailCfg.from" class="af-inp" placeholder="发件人地址(可选,默认同发信邮箱)" autocapitalize="off" />
          <label class="lb-check">
            <input v-model="mailCfg.signupOpen" type="checkbox" /> 开放邮箱自助注册(关掉则只能凭邀请票据入伙)
          </label>
          <input v-model="mailCfg.testTo" class="af-inp" placeholder="测试收件邮箱(可选,保存时发一封测试信)" autocapitalize="off" />
          <p class="lb-hint">QQ 邮箱的「授权码」不是 QQ 密码:网页版 QQ 邮箱 → 设置 → 账号 → 「POP3/IMAP/SMTP 服务」开启后按提示短信验证领取。</p>
          <p v-if="mailCfgErr" class="lb-err">{{ mailCfgErr }}</p>
          <button class="cta full" :disabled="mailCfgBusy" @click="saveMailCfg">
            <LoaderCircle v-if="mailCfgBusy" :size="15" class="spin" />
            保存{{ mailCfg.testTo.trim() ? "并发测试邮件" : "" }}
          </button>
        </div>
      </div>
    </Teleport>

    <!-- 远程终端:对某台互联设备发受控执行请求。模式由**对端**决定,本页只如实显示。 -->
    <Teleport to="body">
      <RemoteTerminal
        v-if="termTarget"
        :name="termTarget.name"
        :port="termTarget.port"
        :token="termTarget.token"
        @close="termTarget = null"
      />
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

/* 远程执行:Shell 临时解锁那一行(总开关复用 .lan-toggle) */
.exec-shell { display: flex; align-items: center; gap: 11px; flex-wrap: wrap; margin-top: 9px; padding: 0 2px; }
.exec-shell .pill.hot { color: #fb923c; border-color: rgba(251,146,60,.45); background: rgba(251,146,60,.12); }
.exec-shell .ex-note { flex: 1; min-width: 220px; font-size: 11px; line-height: 1.6; color: var(--dim); opacity: .85; }

/* 共享出去的盘目录 */
.fs-share { margin-top: 11px; padding: 11px 12px; border: 1px solid var(--glass-brd); border-radius: 12px; background: color-mix(in srgb, var(--panel) 55%, transparent); }
.fs-share.busy { opacity: .6; pointer-events: none; }
.fss-head { display: flex; align-items: center; gap: 12px; }
.fss-head .lt-txt { flex: 1; min-width: 0; }
.fss-list { list-style: none; margin: 9px 0 6px; padding: 0; display: flex; flex-direction: column; gap: 6px; }
.fss-list li { display: flex; align-items: center; gap: 8px; padding: 6px 9px; border-radius: 8px; background: rgba(0,0,0,.16); }
.fss-ic { flex: none; color: var(--dim); }
.fss-path { flex: 1; min-width: 0; font-size: 12px; font-family: ui-monospace, Consolas, monospace; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.fss-x { flex: none; background: none; border: 0; color: var(--dim); cursor: pointer; padding: 3px; border-radius: 6px; display: grid; place-items: center; }
.fss-x:hover:not(:disabled) { color: #f87171; background: rgba(239,68,68,.12); }
.fss-x:disabled { opacity: .4; cursor: default; }
/* 写权限开关:只读是安静的灰,可写是显眼的琥珀 —— 一眼看出哪个目录别人能改 */
.fss-w { flex: none; display: inline-flex; align-items: center; gap: 4px; cursor: pointer; user-select: none;
  font-size: 11px; padding: 3px 7px; border-radius: 999px; color: var(--dim);
  border: 1px solid rgba(255,255,255,.14); background: rgba(255,255,255,.04); }
.fss-w input { position: absolute; opacity: 0; width: 0; height: 0; }
.fss-w:hover { border-color: rgba(255,255,255,.28); }
.fss-w.on { color: #fbbf24; border-color: rgba(251,191,36,.45); background: rgba(251,191,36,.13); }
.fs-share .ex-note { display: block; font-size: 11px; line-height: 1.6; color: var(--dim); opacity: .85; margin-top: 4px; }
.fs-share .ex-note code { font-family: ui-monospace, Consolas, monospace; }

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
/* 登录/注册/忘记密码 三 tab */
.lb-tabs { display: flex; gap: 4px; padding: 3px; border-radius: 11px; background: color-mix(in srgb, var(--text-1) 6%, transparent); }
.lb-tab { flex: 1; padding: 7px 0; border: none; border-radius: 9px; background: transparent; color: var(--muted); font-size: 12.5px; font-weight: 600; cursor: pointer; transition: all .15s; }
.lb-tab.on { background: var(--panel); color: var(--text); box-shadow: 0 1px 4px rgba(0,0,0,.12); }
.lb-code-row { display: flex; gap: 8px; }
.lb-code-row .af-inp { flex: 1; min-width: 0; }
.code-btn { flex: none; white-space: nowrap; align-self: stretch; border-radius: 10px; }
.lb-hint { font-size: 12px; color: var(--muted); line-height: 1.6; margin: 2px 0; }
.lb-check { display: flex; align-items: center; gap: 7px; font-size: 12.5px; color: var(--text-2); cursor: pointer; user-select: none; }
.mail-box { width: min(420px, 94vw); }
.port-inp { flex: none !important; width: 84px; }
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
/* 已挂为本机盘符的远程盘:绿 = 在线可用;dim = 盘符还在但对端暂时失联(看门狗重连中) */
.drive-badge { color: #22c07a; background: color-mix(in srgb, #22c07a 14%, transparent); font-family: ui-monospace, Consolas, monospace; }
.drive-badge.dim { color: var(--dim); background: color-mix(in srgb, var(--dim) 12%, transparent); }
.rw-badge { color: var(--dim); background: color-mix(in srgb, var(--dim) 12%, transparent); }
/* ── 同账号设备网卡 ── */
.mesh-card { padding: 14px 16px; margin-bottom: 12px; }
.mesh-card.busy { opacity: .7; pointer-events: none; }
.mesh-head { display: flex; align-items: flex-start; gap: 10px; }
.mesh-head .lt-txt { flex: 1; min-width: 0; }
.mesh-ic { flex: none; width: 30px; height: 30px; border-radius: 9px; display: grid; place-items: center;
  color: var(--dim); background: rgba(255,255,255,.06); border: 1px solid rgba(255,255,255,.12); }
.mesh-ic.on { color: #22c07a; background: color-mix(in srgb, #22c07a 14%, transparent); border-color: color-mix(in srgb, #22c07a 40%, transparent); }
.mesh-card .lt-sub code { font-family: ui-monospace, Consolas, monospace; font-size: .92em; opacity: .8; }
.mesh-form { margin-top: 10px; }
.mesh-err { flex: none; font-size: 11px; color: #f87171; padding: 2px 7px; border-radius: 999px;
  background: color-mix(in srgb, #f87171 13%, transparent); }
.rw-badge.rw { color: #fbbf24; background: color-mix(in srgb, #fbbf24 15%, transparent); }
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
