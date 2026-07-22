/**
 * 多人协作 Pinia store:会话 + 项目 + 任务看板 + 实时刷新。
 *
 * 实时刷新两条路:
 * - 浏览器/Docker 模式:走现有 src/tauri.ts 的 listen()(共享同源 /ws)。
 * - 桌面模式(isTauri)已连接远端主机:桌面壳里 listen() 只收本机 Tauri 事件,
 *   收不到远端主机的 collab 广播 → store 自己对 base 主机开一条 WS。
 */
import { defineStore } from "pinia";
import { computed, ref } from "vue";
import { invoke, isTauri, listen } from "../../../tauri";
import {
  collabApi,
  getBase,
  getToken,
  setBase,
  setToken,
  deviceId,
  parseShareCode,
  probeHost,
  type ActivityItem,
  type CheckRun,
  type CollabProject,
  type CollabTeam,
  type CollabUser,
  type ProjectMember,
  type TeamMember,
  type AccountInfo,
  type MorningReport,
  type ReviewComment,
  type TaskCard,
  type TaskMessage,
} from "../api";

const USER_KEY = "polaris.collab.user.v1";
const TEAM_KEY = "polaris.collab.teamId.v1";

/** 桌面内嵌主机状态(collab_host_status 返回) */
export interface HostInfo {
  running: boolean;
  port: number;
  urls: string[];
  needsBootstrap: boolean;
  autostart: boolean;
  /** 允许局域网/Tailscale 直连(绑 0.0.0.0);关则只绑 127.0.0.1。手机走 WiFi 连需要它开。 */
  remoteAccess?: boolean;
  /** 本机 iroh 主机 NodeId —— 手机据此打洞 P2P 直连(连接码里带上它)。 */
  nodeId?: string;
  /** 主机访问口令:仅经 Tauri IPC 给本机前端,bootstrap(首次建 owner)必须带它。 */
  accessToken?: string;
}

function loadCachedUser(): CollabUser | null {
  try {
    const raw = localStorage.getItem(USER_KEY);
    return raw ? (JSON.parse(raw) as CollabUser) : null;
  } catch {
    return null;
  }
}

export const useCollabStore = defineStore("collab", () => {
  // ── 会话 ──
  const base = ref(getBase());
  const token = ref(getToken());
  const user = ref<CollabUser | null>(loadCachedUser());
  const authed = computed(() => !!token.value && !!user.value);
  const isOwner = computed(() => user.value?.role === "owner");
  /** 桌面模式必须先填主机地址才能发请求 */
  const needsHost = computed(() => isTauri && !base.value);

  // ── 团队(GitHub 组织式) ──
  const teams = ref<CollabTeam[]>([]);
  const currentTeamId = ref<number | null>(
    (() => {
      try {
        const raw = localStorage.getItem(TEAM_KEY);
        return raw ? Number(raw) : null;
      } catch {
        return null;
      }
    })()
  );
  const currentTeam = computed(
    () => teams.value.find((t) => t.id === currentTeamId.value) ?? null
  );
  /** 管理权:全局 owner,或当前团队里我是 owner */
  const canManage = computed(
    () => isOwner.value || currentTeam.value?.my_role === "owner"
  );
  const teamMembers = ref<TeamMember[]>([]);

  const projects = ref<CollabProject[]>([]);
  const currentProjectId = ref<number | null>(null);
  /** 当前团队下的项目 */
  const teamProjects = computed(() =>
    currentTeamId.value == null
      ? []
      : projects.value.filter((p) => p.team_id === currentTeamId.value)
  );
  /** 旧独立项目(没挂团队)→「未分组」段落 */
  const ungroupedProjects = computed(() =>
    projects.value.filter((p) => p.team_id == null)
  );
  const currentProject = computed(
    () => projects.value.find((p) => p.id === currentProjectId.value) ?? null
  );
  const members = ref<ProjectMember[]>([]);
  const tasks = ref<TaskCard[]>([]);
  const loadingTasks = ref(false);
  /** 打回熔断提醒(collab:escalate),看板顶部横幅展示 */
  const escalation = ref<string | null>(null);
  /** 晨报(collab:morning 推送或手动拉取),看板顶部折叠条展示 */
  const morning = ref<MorningReport | null>(null);

  /** 负责人 user_id → 显示名(项目成员表映射;查不到回退 #id) */
  function memberName(userId: number | null | undefined): string {
    if (userId == null) return "";
    const m = members.value.find((x) => x.user_id === userId);
    return m?.display_name || m?.username || `#${userId}`;
  }

  /** 登录/注册响应校验:主机没回账号或令牌就明确报错,别让下游读 null 崩掉 */
  function requireAuth(r: { user?: CollabUser; token?: string } | null) {
    if (!r || !r.user || !r.token) {
      throw new Error(
        "主机未返回账号信息 —— 请检查协作主机地址是否指向 Polaris 协作服务"
      );
    }
    return r as { user: CollabUser; token: string };
  }

  function persistSession(u: CollabUser | null, t: string) {
    user.value = u;
    token.value = t;
    setToken(t);
    try {
      if (u) localStorage.setItem(USER_KEY, JSON.stringify(u));
      else localStorage.removeItem(USER_KEY);
    } catch {
      /* storage 不可用 */
    }
  }

  function applyBase(b: string) {
    setBase(b);
    base.value = getBase();
  }

  // ── 桌面内嵌主机(一键当主机) ──
  const hostInfo = ref<HostInfo | null>(null);

  async function hostStatus(): Promise<HostInfo | null> {
    if (!isTauri) return null;
    try {
      hostInfo.value = await invoke<HostInfo>("collab_host_status");
    } catch {
      hostInfo.value = null;
    }
    return hostInfo.value;
  }

  /** 本机起协作服务并把 base 自动指向 127.0.0.1(避开 localhost 被 Docker 占 IPv6 的坑) */
  async function hostStart(): Promise<HostInfo> {
    const info = await invoke<HostInfo>("collab_host_start");
    hostInfo.value = info;
    applyBase(`http://127.0.0.1:${info.port}`);
    return info;
  }

  async function hostStop() {
    await invoke<HostInfo>("collab_host_stop");
    await hostStatus();
  }

  /** 桌面端登录/注册前自保:base 指向本机(或还没填)时,主机没跑先拉起、端口漂了先校正。
   *  base 指向远端主机(用户手动填过地址/粘过分享码)时绝不动它。
   *  修的就是「点登录直接报 无法连接协作主机」—— 此前提交前没人保证主机在跑。 */
  async function ensureLocalHost() {
    if (!isTauri) return;
    const isLocal = (b: string) =>
      !b || /^https?:\/\/(127\.0\.0\.1|localhost)(:\d+)?$/.test(b);
    if (!isLocal(base.value)) return;
    const info = await hostStatus();
    if (!info?.running) {
      await hostStart(); // 起主机并把 base 指到 127.0.0.1:port
    } else {
      applyBase(`http://127.0.0.1:${info.port}`); // base 空/端口漂移 → 校正
    }
  }

  /** 切换局域网直连(绑 0.0.0.0):后端会重启主机重绑地址,urls 随之带上局域网 IP。 */
  async function hostSetRemoteAccess(enabled: boolean): Promise<HostInfo> {
    const info = await invoke<HostInfo>("collab_host_set_remote_access", { enabled });
    hostInfo.value = info;
    if (info?.running && info.port) applyBase(`http://127.0.0.1:${info.port}`);
    return info;
  }

  // ── 账号体系(云端账号中心 / 本机账号)──
  const accountInfo = ref<AccountInfo | null>(null);
  /** 账号在云端统管(delegated):注册/改密都得去账号中心,本机只认它签的身份 */
  const federated = computed(() => accountInfo.value?.mode === "delegated");
  const authorityUrl = computed(() => accountInfo.value?.authorityUrl ?? "");

  /** 拉一次主机的账号体系自述(免登录)。失败按老的本机账号处理,不挡登录页。 */
  async function loadAccountInfo(): Promise<AccountInfo | null> {
    try {
      accountInfo.value = await collabApi.accountInfo();
    } catch {
      // 老版本主机没有这个端点 → 就是本机账号模式。
      accountInfo.value = { mode: "local", emailRequired: false };
    }
    return accountInfo.value;
  }

  /**
   * 云端账号登录:账号中心验密码签断言 → 当前主机验签换本机会话。
   * 客户端直连账号中心,不经主机中转 —— 密码不在主机眼前过一遍。
   */
  async function loginViaAuthority(username: string, password: string) {
    const url = authorityUrl.value;
    if (!url) throw new Error("本机没有配置云端账号中心地址");
    const { assertion } = await collabApi.authorityLogin(url, {
      username,
      password,
    });
    const r = requireAuth(await collabApi.loginWithAssertion(assertion));
    persistSession(r.user, r.token);
    await afterAuth();
  }

  /**
   * 云端注册(邮箱可选),成功即用返回的断言进当前主机。
   *
   * 注册出来的是「全局身份」,不自动等于「这台主机的成员」:主机已经有主人时,
   * 换会话那步会被拒并提示要邀请码 —— 这时把断言留着交给 joinViaTicket。
   */
  async function signupViaAuthority(args: {
    email?: string;
    code?: string;
    username: string;
    password: string;
    displayName: string;
    /** 有邀请码就直接连着入伙,省得注册完再被挡一次 */
    inviteCode?: string;
    deviceName?: string;
  }) {
    const url = authorityUrl.value;
    if (!url) throw new Error("本机没有配置云端账号中心地址");
    const { assertion } = await collabApi.authoritySignup(url, args);
    const r = requireAuth(
      args.inviteCode?.trim()
        ? await collabApi.joinWithTicket({
            assertion,
            code: args.inviteCode.trim(),
            deviceName: args.deviceName ?? "我的电脑",
          })
        : await collabApi.loginWithAssertion(assertion)
    );
    persistSession(r.user, r.token);
    await afterAuth();
  }

  /** 已有云端账号 + 本机邀请码 → 成为这台主机的成员。 */
  async function joinViaTicket(args: {
    username: string;
    password: string;
    code: string;
    deviceName?: string;
  }) {
    const url = authorityUrl.value;
    if (!url) throw new Error("本机没有配置云端账号中心地址");
    const { assertion } = await collabApi.authorityLogin(url, {
      username: args.username,
      password: args.password,
    });
    const r = requireAuth(
      await collabApi.joinWithTicket({
        assertion,
        code: args.code,
        deviceName: args.deviceName ?? "我的电脑",
      })
    );
    persistSession(r.user, r.token);
    await afterAuth();
  }

  // ── 登录三入口 ──
  /** 账号在云端时自动改走账号中心;本机账号模式保持老行为。 */
  async function login(username: string, password: string) {
    if (accountInfo.value === null) await loadAccountInfo();
    if (federated.value) return loginViaAuthority(username, password);
    const r = requireAuth(await collabApi.login({ username, password }));
    persistSession(r.user, r.token);
    await afterAuth();
  }

  /** 开放注册(403 = 主机关闭注册,提示走票据) */
  async function signup(
    username: string,
    password: string,
    displayName: string
  ) {
    const r = requireAuth(
      await collabApi.signup({ username, password, displayName })
    );
    persistSession(r.user, r.token);
    await afterAuth();
  }

  async function bootstrap(
    username: string,
    password: string,
    displayName: string,
    hostSelf = false
  ) {
    // 首次建 owner 需要主机访问口令(仅 Tauri IPC 可得);没带后端必 401。
    const setupToken = hostInfo.value?.accessToken || undefined;
    const r = requireAuth(
      await collabApi.bootstrap({
        username,
        password,
        displayName,
        hostSelf,
        setupToken,
      })
    );
    persistSession(r.user, r.token);
    await afterAuth();
  }

  /** 邮箱验证码注册(注册成功即登录) */
  async function emailSignup(args: {
    email: string;
    code: string;
    username: string;
    password: string;
    displayName: string;
  }) {
    const r = requireAuth(await collabApi.emailSignup(args));
    persistSession(r.user, r.token);
    await afterAuth();
  }

  async function redeem(args: {
    code: string;
    username: string;
    password: string;
    displayName: string;
    deviceName: string;
  }) {
    // 分享码路径:解出裸码+地址表 → 逐个探活 → 自动 setBase,零手填。
    // 裸码(8位)照旧走已保存的 base。
    let code = args.code.trim();
    const parsed = parseShareCode(code);
    if (parsed) {
      const found = await probeHost(parsed.addrs);
      if (!found) {
        throw new Error(
          "配对码里的主机地址都连不上 —— 确认主机开着,且你们在同一网络/VPN 里"
        );
      }
      applyBase(found);
      code = parsed.code;
    }
    const r = requireAuth(
      await collabApi.redeem({ ...args, code, nodeId: deviceId() })
    );
    persistSession(r.user, r.token);
    await afterAuth();
  }

  async function logout() {
    try {
      await collabApi.logout();
    } catch {
      /* 主机不可达也允许本地退出 */
    }
    persistSession(null, "");
    projects.value = [];
    tasks.value = [];
    members.value = [];
    teams.value = [];
    teamMembers.value = [];
    checksByTask.value = {};
    checkProfile.value = "";
    currentProjectId.value = null;
    teardownWs();
  }

  /** 启动时校验缓存会话是否仍有效(token 过期则清掉回登录页) */
  const validated = ref(false);
  async function init() {
    if (validated.value) return;
    validated.value = true;
    if (!token.value || needsHost.value) return;
    try {
      const me = await collabApi.me();
      // me 只回 username/role,合并进缓存的 user(displayName 等保留)
      user.value = {
        ...(user.value ?? { username: me.username }),
        username: me.username,
        role: me.role,
      };
      await afterAuth();
    } catch (e) {
      if ((e as { status?: number }).status === 401) persistSession(null, "");
      // 网络错误 → 保留会话,让用户看到错误后手动重试
    }
  }

  async function afterAuth() {
    await Promise.all([refreshTeams(), refreshProjects()]);
    void subscribe();
  }

  // ── 团队 ──
  function persistTeamId(id: number | null) {
    currentTeamId.value = id;
    try {
      if (id != null) localStorage.setItem(TEAM_KEY, String(id));
      else localStorage.removeItem(TEAM_KEY);
    } catch {
      /* storage 不可用 */
    }
  }

  async function refreshTeams() {
    try {
      teams.value = await collabApi.listTeams();
    } catch {
      teams.value = [];
    }
    if (
      currentTeamId.value != null &&
      !teams.value.some((t) => t.id === currentTeamId.value)
    ) {
      persistTeamId(teams.value[0]?.id ?? null);
    } else if (currentTeamId.value == null && teams.value.length) {
      persistTeamId(teams.value[0].id);
    }
    await refreshTeamMembers();
  }

  async function createTeam(name: string) {
    const t = await collabApi.createTeam(name);
    await selectTeam(t.id);
    // listTeams 回来才有准确 my_role/member_count
    teams.value = await collabApi.listTeams().catch(() => teams.value);
    return t;
  }

  async function selectTeam(id: number | null) {
    persistTeamId(id);
    teamMembers.value = [];
    // 切团队后:当前项目若不属于新团队,换到该团队第一个项目
    const pool =
      id == null ? ungroupedProjects.value : teamProjects.value;
    if (!pool.some((p) => p.id === currentProjectId.value)) {
      currentProjectId.value = pool[0]?.id ?? null;
      if (currentProjectId.value) await selectProject(currentProjectId.value);
      else {
        tasks.value = [];
        members.value = [];
      }
    }
    await refreshTeamMembers();
  }

  async function refreshTeamMembers() {
    const id = currentTeamId.value;
    if (id == null) {
      teamMembers.value = [];
      return;
    }
    try {
      teamMembers.value = await collabApi.teamMembers(id);
    } catch {
      teamMembers.value = [];
    }
  }

  async function addTeamMember(username: string, role: "member" | "owner") {
    const id = currentTeamId.value;
    if (id == null) throw new Error("请先选择团队");
    await collabApi.addTeamMember(id, username, role);
    await refreshTeamMembers();
    teams.value = await collabApi.listTeams().catch(() => teams.value);
  }

  async function removeTeamMember(userId: number) {
    const id = currentTeamId.value;
    if (id == null) throw new Error("请先选择团队");
    await collabApi.removeTeamMember(id, userId);
    // 退出的是自己 → 团队列表也会变,统一重刷
    await refreshTeams();
  }

  // ── 项目 ──
  async function refreshProjects() {
    projects.value = await collabApi.listProjects();
    // 优先留在当前团队可见的项目里
    const pool =
      currentTeamId.value == null
        ? projects.value
        : [...teamProjects.value, ...ungroupedProjects.value];
    if (
      !currentProjectId.value ||
      !pool.some((p) => p.id === currentProjectId.value)
    ) {
      currentProjectId.value = pool[0]?.id ?? null;
    }
    if (currentProjectId.value) await selectProject(currentProjectId.value);
  }

  async function createProject(
    name: string,
    repo: string,
    teamId?: number | null
  ) {
    const p = await collabApi.createProject(name, repo, teamId);
    projects.value = [...projects.value, p];
    await selectProject(p.id);
    return p;
  }

  async function selectProject(id: number) {
    currentProjectId.value = id;
    // 检查缓存/档位是按项目的,切项目即失效(懒加载,打开抽屉/收到事件再拉)
    checksByTask.value = {};
    checkProfile.value = "";
    await Promise.all([refreshTasks(), refreshMembers(), refreshActivity()]);
  }

  // ── 检查工作流(CI-lite status checks) ──
  /** 卡 id → 本轮检查结果(懒加载:抽屉打开/collab:check 事件才拉) */
  const checksByTask = ref<Record<number, CheckRun[]>>({});
  /** 当前项目检查档位(code/creative/off;空 = 尚未从任何 checks 响应取到) */
  const checkProfile = ref<string>("");
  /** 当前项目检查技能 id(空 = 未取到;后端回落默认 project-check-default) */
  const checkSkill = ref<string>("");
  async function refreshChecks(taskId: number) {
    try {
      const r = await collabApi.checks(taskId);
      checksByTask.value = { ...checksByTask.value, [taskId]: r.runs };
      if (r.profile) checkProfile.value = r.profile;
      if (r.checkSkill) checkSkill.value = r.checkSkill;
    } catch {
      /* 主机旧版无此端点/网络抖动 → 静默,不打扰看板 */
    }
  }
  async function setCheckProfile(profile: string, skill?: string) {
    const pid = currentProjectId.value;
    if (!pid) throw new Error("请先选择项目");
    await collabApi.checksSetProfile(pid, profile, skill);
    if (profile) checkProfile.value = profile; // 空串 = 本次只改技能
    if (skill !== undefined) checkSkill.value = skill;
  }

  // ── 任务级对话:最新一条推送(TaskChat 面板 watch 它增量追加) ──
  const lastTaskMessage = ref<TaskMessage | null>(null);
  function onTaskMessage(p: unknown) {
    const m = p as TaskMessage | null;
    if (m && typeof m.id === "number" && typeof m.task_id === "number")
      lastTaskMessage.value = m;
  }

  // ── 项目动态时间线(项目主页概览 tab) ──
  const activity = ref<ActivityItem[]>([]);
  async function refreshActivity() {
    const id = currentProjectId.value;
    if (!id) return;
    try {
      activity.value = await collabApi.activity(id);
    } catch {
      activity.value = [];
    }
  }

  async function refreshMembers() {
    const id = currentProjectId.value;
    if (!id) return;
    try {
      members.value = await collabApi.projectMembers(id);
    } catch {
      members.value = [];
    }
  }

  // ── 任务 ──
  async function refreshTasks() {
    const id = currentProjectId.value;
    if (!id) return;
    loadingTasks.value = true;
    try {
      tasks.value = await collabApi.listTasks(id);
    } finally {
      loadingTasks.value = false;
    }
  }

  async function createTask(args: {
    title: string;
    body: string;
    scope: string;
    criteria: string;
  }) {
    const id = currentProjectId.value;
    if (!id) throw new Error("请先选择项目");
    await collabApi.createTask({ projectId: id, ...args });
    await refreshTasks();
  }

  async function claim(taskId: number) {
    await collabApi.claimTask(taskId);
    await refreshTasks();
  }
  async function submit(taskId: number, prId?: string) {
    await collabApi.submitTask(taskId, prId);
    await refreshTasks();
  }
  async function review(taskId: number, pass: boolean, comments: ReviewComment[]) {
    await collabApi.reviewTask(taskId, pass, comments);
    await refreshTasks();
  }
  async function archive(taskId: number) {
    await collabApi.archiveTask(taskId);
    await refreshTasks();
  }
  async function cancel(taskId: number) {
    await collabApi.cancelTask(taskId);
    await refreshTasks();
  }

  // ── 实时订阅 ──
  let unTask: (() => void) | null = null;
  let unEsc: (() => void) | null = null;
  let unMorning: (() => void) | null = null;
  let unCheck: (() => void) | null = null;
  let unMsg: (() => void) | null = null;
  let directWs: WebSocket | null = null;
  let directWsTimer: ReturnType<typeof setTimeout> | null = null;
  let subscribed = false;

  /** collab:task 去抖定时器 —— 服务端 AI 拆卡是紧循环逐卡 emit,N 张卡若不去抖
   *  就是 N×(refreshTasks+refreshActivity) 共 2N 次全量网络往返打向主机。 */
  let taskRefreshTimer: ReturnType<typeof setTimeout> | null = null;
  function onTaskEvent() {
    // 卡片变更 → 300ms trailing 去抖:窗口内多条事件只在最后重拉一次看板 + 动态时间线
    if (taskRefreshTimer) clearTimeout(taskRefreshTimer);
    taskRefreshTimer = setTimeout(() => {
      taskRefreshTimer = null;
      void refreshTasks();
      void refreshActivity();
    }, 300);
  }
  function onMorning(p: unknown) {
    const r = p as MorningReport | null;
    // 只收当前项目的晨报;没选项目时也先存下(切到该项目即可见)
    if (r && (currentProjectId.value == null || r.project_id === currentProjectId.value))
      morning.value = r;
  }
  async function refreshMorning() {
    const id = currentProjectId.value;
    if (!id) return;
    morning.value = await collabApi.morning(id);
  }
  function onCheckEvent(p: unknown) {
    // 检查进度/结果推送({taskId, round})→ 只刷这张卡的检查数据
    const tid = (p as { taskId?: number } | null)?.taskId;
    if (typeof tid === "number") void refreshChecks(tid);
  }
  function onEscalate(p: unknown) {
    const msg =
      (p as { message?: string; title?: string } | null)?.message ??
      (p as { title?: string } | null)?.title ??
      "有任务多轮打回,已触发熔断,请负责人介入";
    escalation.value = String(msg);
  }

  async function subscribe() {
    if (subscribed) return;
    subscribed = true;
    if (isTauri && base.value) {
      connectDirectWs();
      return;
    }
    unTask = await listen("collab:task", onTaskEvent);
    unEsc = await listen("collab:escalate", onEscalate);
    unMorning = await listen("collab:morning", onMorning);
    unCheck = await listen("collab:check", onCheckEvent);
    unMsg = await listen("collab:task_message", onTaskMessage);
  }

  /** 桌面模式直连远端主机 /ws(带 token),断线 2s 自动重连 */
  function connectDirectWs() {
    if (!token.value || !base.value) return;
    if (
      directWs &&
      (directWs.readyState === WebSocket.OPEN ||
        directWs.readyState === WebSocket.CONNECTING)
    )
      return;
    try {
      const url =
        base.value.replace(/^http/, "ws") +
        `/ws?token=${encodeURIComponent(token.value)}`;
      directWs = new WebSocket(url);
      directWs.onmessage = (e) => {
        try {
          const { topic, payload } = JSON.parse(e.data);
          if (topic === "collab:task") onTaskEvent();
          else if (topic === "collab:escalate") onEscalate(payload);
          else if (topic === "collab:morning") onMorning(payload);
          else if (topic === "collab:check") onCheckEvent(payload);
          else if (topic === "collab:task_message") onTaskMessage(payload);
        } catch {
          /* 忽略坏帧 */
        }
      };
      directWs.onclose = () => {
        directWs = null;
        if (subscribed && token.value) {
          if (directWsTimer) clearTimeout(directWsTimer);
          directWsTimer = setTimeout(connectDirectWs, 2000);
        }
      };
      directWs.onerror = () => {
        try {
          directWs?.close();
        } catch {
          /* ignore */
        }
      };
    } catch {
      directWs = null;
    }
  }

  function teardownWs() {
    subscribed = false;
    unTask?.();
    unEsc?.();
    unMorning?.();
    unCheck?.();
    unMsg?.();
    unTask = unEsc = unMorning = unCheck = unMsg = null;
    // 登出/卸载时清掉挂起的去抖定时器,防 teardown 后仍触发一轮无主刷新
    if (taskRefreshTimer) {
      clearTimeout(taskRefreshTimer);
      taskRefreshTimer = null;
    }
    if (directWsTimer) clearTimeout(directWsTimer);
    try {
      directWs?.close();
    } catch {
      /* ignore */
    }
    directWs = null;
  }

  return {
    // 会话
    base,
    token,
    user,
    authed,
    isOwner,
    needsHost,
    applyBase,
    // 桌面内嵌主机(一键当主机)
    hostInfo,
    hostStatus,
    hostStart,
    hostStop,
    hostSetRemoteAccess,
    ensureLocalHost,
    login,
    signup,
    emailSignup,
    bootstrap,
    redeem,
    logout,
    init,
    // 云端账号中心(账号权威)
    accountInfo,
    federated,
    authorityUrl,
    loadAccountInfo,
    loginViaAuthority,
    signupViaAuthority,
    joinViaTicket,
    // 团队
    teams,
    currentTeamId,
    currentTeam,
    canManage,
    teamMembers,
    refreshTeams,
    createTeam,
    selectTeam,
    refreshTeamMembers,
    addTeamMember,
    removeTeamMember,
    // 项目
    projects,
    teamProjects,
    ungroupedProjects,
    currentProjectId,
    currentProject,
    members,
    refreshProjects,
    createProject,
    selectProject,
    refreshMembers,
    // 任务
    tasks,
    loadingTasks,
    escalation,
    morning,
    memberName,
    refreshMorning,
    refreshTasks,
    activity,
    refreshActivity,
    // 检查工作流
    checksByTask,
    checkProfile,
    checkSkill,
    refreshChecks,
    setCheckProfile,
    // 任务级对话
    lastTaskMessage,
    createTask,
    claim,
    submit,
    review,
    archive,
    cancel,
  };
});
