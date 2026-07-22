/**
 * 多人协作 · REST 轻封装
 *
 * base + token 管理:
 * - 浏览器/Docker 模式:base 为空(同源),直接打 /api/collab/*
 * - 桌面模式(isTauri):连别人的主机 → 填地址或粘分享码(parseShareCode 自动填);
 *   自己当主机 → 「把这台电脑设为主机」(collab_host_start)后自动指向 127.0.0.1。
 * - base 与 token 都持久化到 localStorage,重启后自动恢复会话。
 *
 * 错误约定:后端错误响应形如 {error:string},统一抛成 Error(message)。
 */

const BASE_KEY = "polaris.collab.base.v1";
const TOKEN_KEY = "polaris.collab.token.v1";
const DEVICE_KEY = "polaris.collab.deviceId.v1";

// ── 类型 ──────────────────────────────────────────────

export interface CollabUser {
  id?: number;
  username: string;
  display_name?: string;
  displayName?: string;
  role: string; // owner | member | ...
}

export interface CollabTeam {
  id: number;
  name: string;
  created_at?: number;
  /** 我在这个团队里的角色:owner | member */
  my_role: string;
  member_count?: number;
}

export interface TeamMember {
  user_id: number;
  username: string;
  display_name: string;
  role: string;
}

export interface UserSearchHit {
  id: number;
  username: string;
  display_name?: string;
}

export interface CollabProject {
  id: number;
  name: string;
  repo: string;
  /** 所属团队;旧独立项目为 null(前端归入「未分组」) */
  team_id?: number | null;
  lead_expert_id?: string | null;
  charter_path?: string;
  created_at?: number;
  archived?: boolean;
  /** 进行中任务数(pending+in_progress)——侧栏徽章 */
  open_count?: number;
  /** 待验收任务数(review)——侧栏徽章 */
  review_count?: number;
  /** 管理者放行的全项目共享可见路径(CSV) */
  shared_scope?: string;
}

export interface ProjectMember {
  user_id: number;
  username: string;
  display_name: string;
  role: string;
}

/** 任务卡六态 */
export type TaskState =
  | "pending"
  | "in_progress"
  | "review"
  | "merged"
  | "archived"
  | "cancelled";

export interface TaskCard {
  id: number;
  project_id: number;
  title: string;
  body: string;
  scope: string;
  criteria: string;
  /** 负责人 user_id(后端 i64);显示名需用项目成员表映射 */
  assignee: number | null;
  state: TaskState;
  round: number;
  branch: string;
  pr_id: number | null;
  issue_no: number | null;
  created_at: number;
  updated_at: number;
}

/** 每轮验收记录(comments 是 JSON 字符串,渲染时再解析) */
export interface TaskRound {
  round: number;
  reviewer: string;
  verdict: string;
  comments: string;
  created_at: number;
}

export interface ReviewComment {
  item: string;
  note: string;
}

/** 检查工作流(CI-lite,GitHub status checks 式)单项结果 */
export interface CheckRun {
  name: string;
  status: "pass" | "fail" | "skipped" | "running";
  output: string;
  started_at: number;
  ended_at: number;
}

/** GET /api/collab/checks 响应:项目档位 + 检查技能 + 该卡当前轮次 + 各项结果 */
export interface ChecksResp {
  profile: string;
  /** 项目检查技能 id(后端回落默认 project-check-default) */
  checkSkill?: string;
  round: number;
  runs: CheckRun[];
}

/** 任务级对话消息(协作者↔负责人↔主 Agent) */
export interface TaskMessage {
  id: number;
  task_id: number;
  round: number;
  author_user_id: number;
  author_name: string;
  /** lead | assignee | member | ai */
  role: string;
  body: string;
  created_at: number;
}

export interface Ticket {
  code: string;
  role: string;
  expires_at: number;
  /** 分享码 PLRS1-*(裸码+主机地址),对方粘一串即入伙 */
  share?: string;
}

/** 项目动态时间线条目(GitHub activity feed 式)。kind: review|task */
export interface ActivityItem {
  kind: "review" | "task";
  actor: string;
  task_id: number;
  title: string;
  /** review: "pass/reject · 第N轮";task: 当前 state */
  detail: string;
  at: number;
}

export interface AdminUser {
  id: number;
  username: string;
  display_name: string;
  role: string;
  disabled: boolean;
  /** 绑定邮箱(空 = 未绑,不能自助找回,只能 owner 重置) */
  email?: string;
}

export interface AdminDevice {
  id: string;
  user_id: number;
  name: string;
  node_id: string;
  revoked: boolean;
  username?: string;
  /** 这台就是主机(node_id 命中 meta.host_node_id) */
  is_host?: boolean;
  /** 该设备最近上报的资源实况(遥测,字段按设备能力可缺:CPU/内存/磁盘/核数) */
  stats?: {
    cpu_pct?: number;
    mem_used?: number;
    mem_total?: number;
    disk_used?: number;
    disk_total?: number;
    cores?: number;
  };
  /** 上报时间(ms)。据此判断新鲜度,过期在 UI 上如实标灰。 */
  stats_at?: number;
}

/** 一条审计事件(「正在发生」活动流)。 */
export interface AuditRow {
  actor: string;
  action: string;
  target: string;
  detail: string;
  /** Unix 秒 */
  at: number;
}

// ── 合并闸门(冲突裁决台) ──

export interface ConflictBlock {
  ours: string;
  base: string;
  theirs: string;
  /** `<<<<<<<` 所在行号(1 起) */
  start_line: number;
  /** `>>>>>>>` 所在行号(1 起) */
  end_line: number;
}

export interface MergeTrial {
  clean: boolean;
  conflictFiles: string[];
  behind: number;
  ahead: number;
  /** 分支改动越出卡上 scope 的文件 */
  scopeOverlap: string[];
  conflictBlocks: Record<string, ConflictBlock[]>;
}

/** 单个冲突块的处置决定(裁决台三处置之「采纳某侧/融合草案」) */
export interface BlockResolution {
  choice: "ours" | "theirs" | "manual";
  /** manual(人工或 AI 融合草案)时的最终文本 */
  text?: string;
}

// ── 主 Agent 授权表 ──

export interface LeadGrants {
  can_merge: boolean;
  can_reassign: boolean;
  auto_dispatch: boolean;
  token_budget: number;
}

/** 主 Agent 模型配置(OpenAI 兼容端点;api_key 服务端脱敏为 •••) */
export interface LeadModelCfg {
  enabled: boolean;
  base_url: string;
  api_key: string;
  model: string;
}

/** AI 拆解产出的任务卡草案(未落库) */
export interface CardDraft {
  title: string;
  body: string;
  scope: string;
  criteria: string;
}

/** AI 验收意见草稿(不落状态机,人确认后才落) */
export interface ReviewDraft {
  pass: boolean;
  summary: string;
  comments: { item: number; note: string }[];
}

// ── 晨报 ──

export interface MorningReport {
  project_id: number;
  merged_yesterday: TaskCard[];
  rejected_open: TaskCard[];
  review_queue: TaskCard[];
  stale: TaskCard[];
  unclaimed: TaskCard[];
  escalated: TaskCard[];
}

// ── base / token / deviceId 持久化 ────────────────────

export function getBase(): string {
  try {
    return localStorage.getItem(BASE_KEY) ?? "";
  } catch {
    return "";
  }
}

export function setBase(base: string): void {
  const b = base.trim().replace(/\/+$/, "");
  try {
    if (b) localStorage.setItem(BASE_KEY, b);
    else localStorage.removeItem(BASE_KEY);
  } catch {
    /* storage 不可用 */
  }
}

export function getToken(): string {
  try {
    return localStorage.getItem(TOKEN_KEY) ?? "";
  } catch {
    return "";
  }
}

export function setToken(token: string): void {
  try {
    if (token) localStorage.setItem(TOKEN_KEY, token);
    else localStorage.removeItem(TOKEN_KEY);
  } catch {
    /* storage 不可用 */
  }
}

/** 本机设备指纹:首次生成后固定,登录/入伙都带上 */
export function deviceId(): string {
  try {
    let id = localStorage.getItem(DEVICE_KEY);
    if (!id) {
      id =
        typeof crypto !== "undefined" && "randomUUID" in crypto
          ? crypto.randomUUID()
          : `dev-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
      localStorage.setItem(DEVICE_KEY, id);
    }
    return id;
  } catch {
    return "dev-anonymous";
  }
}

// ── fetch 封装 ────────────────────────────────────────

async function req<T>(
  path: string,
  init?: { method?: string; body?: unknown; token?: string }
): Promise<T> {
  const headers: Record<string, string> = {};
  // init.token:一次性覆写凭据(bootstrap 用主机访问口令,而非会话 token)
  const token = init?.token ?? getToken();
  if (token) headers["Authorization"] = `Bearer ${token}`;
  if (init?.body !== undefined) headers["Content-Type"] = "application/json";
  let res: Response;
  try {
    res = await fetch(getBase() + path, {
      method: init?.method ?? "GET",
      headers,
      body: init?.body !== undefined ? JSON.stringify(init.body) : undefined,
    });
  } catch {
    throw new Error("无法连接协作主机,请检查地址与网络");
  }
  const text = await res.text();
  let data: unknown = null;
  let parseFailed = false;
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      parseFailed = true;
    }
  }
  // 2xx 却返回了非 JSON(最常见:主机地址其实指向了普通网页/SPA,或压根没连上
  // Polaris 协作服务)。此前这里静默返回 null,调用方读 r.user 就炸成天书报错。
  if (res.ok && parseFailed) {
    throw new Error(
      "主机返回了非 JSON 响应 —— 请确认「协作主机地址」指向的是 Polaris 协作服务(而非普通网页)"
    );
  }
  if (!res.ok) {
    const msg =
      (data as { error?: string } | null)?.error ||
      (res.status === 401
        ? "登录已过期,请重新登录"
        : `请求失败(HTTP ${res.status})`);
    const err = new Error(msg) as Error & { status?: number };
    err.status = res.status;
    throw err;
  }
  return data as T;
}

const get = <T>(path: string) => req<T>(path);
const post = <T>(path: string, body?: unknown) =>
  req<T>(path, { method: "POST", body: body ?? {} });

/**
 * 打**账号权威**(云端账号中心),而不是当前协作主机 —— 这是登录链路里唯一一处
 * 绝对地址请求:注册/改密/换身份断言都发生在云端,主机根本没有密码可验。
 * 不带会话 token(权威不认主机的会话,凭据是密码本身)。
 */
async function authorityReq<T>(
  authorityUrl: string,
  path: string,
  body?: unknown
): Promise<T> {
  const base = authorityUrl.trim().replace(/\/+$/, "");
  if (!base) throw new Error("还没有配置云端账号中心地址");
  let res: Response;
  try {
    res = await fetch(base + path, {
      method: body === undefined ? "GET" : "POST",
      headers: body === undefined ? {} : { "Content-Type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  } catch {
    throw new Error("连不上云端账号中心,请检查网络");
  }
  const text = await res.text();
  let data: unknown = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch {
    throw new Error("云端账号中心返回了非 JSON —— 请确认地址填对了");
  }
  if (!res.ok) {
    const err = new Error(
      (data as { error?: string } | null)?.error ?? `账号中心请求失败(HTTP ${res.status})`
    ) as Error & { status?: number };
    err.status = res.status;
    throw err;
  }
  return data as T;
}

// ── 会话 ──────────────────────────────────────────────

export interface AuthResult {
  user: CollabUser;
  token: string;
}

/** 邮箱服务状态(公开):configured=能发信,signupOpen=开放邮箱自助注册 */
export interface EmailStatus {
  configured: boolean;
  signupOpen: boolean;
}

/** owner 邮箱服务配置(SMTP 授权码永不回显,只有 passSet) */
export interface EmailConfig {
  configured: boolean;
  host: string;
  port: number;
  user: string;
  from: string;
  passSet: boolean;
  signupOpen: boolean;
}

/** 主机的账号体系自述:决定登录页往哪儿打。 */
export interface AccountInfo {
  /** authority=本机就是账号中心;delegated=账号在云端(authorityUrl);local=老的本机账号 */
  mode: "authority" | "delegated" | "local";
  authorityUrl?: string;
  /** delegated:本机是否已信任(钉住)该账号中心的公钥 */
  trusted?: boolean;
  /** 公钥指纹,换云机时给人眼核对 */
  kid?: string;
  /** 恒 false —— 邮箱是可选的(绑了才有自助找回) */
  emailRequired: boolean;
  /** authority:这台账号中心发得出验证码吗(发不出就别让人填邮箱空等) */
  emailCapable?: boolean;
}

/** 云端账号中心签发的身份断言(5 分钟有效,进主机即换成主机会话) */
export interface AssertionResult {
  assertion: string;
  uid: string;
  user?: { username: string; displayName?: string; email?: string };
}

export const collabApi = {
  // ── 云端账号中心(账号权威)──
  /** 本机的账号体系自述(公开,免登录) */
  accountInfo(): Promise<AccountInfo> {
    return get("/api/account/info");
  },
  /** 云端注册。email/code 可留空 = 不绑邮箱(不绑就没有自助找回)。成功即回身份断言 */
  authoritySignup(
    authorityUrl: string,
    args: {
      email?: string;
      code?: string;
      username: string;
      password: string;
      displayName: string;
    }
  ): Promise<AssertionResult> {
    return authorityReq(authorityUrl, "/api/account/signup", args);
  },
  /** 云端账号 + 本机邀请码 → 成为这台主机的成员(联邦模式下的入伙路径) */
  joinWithTicket(args: {
    assertion: string;
    code: string;
    deviceName: string;
  }): Promise<AuthResult> {
    return post("/api/collab/join", { ...args, nodeId: deviceId() });
  },
  /** 云端登录:用户名**或邮箱** + 密码 → 身份断言 */
  authorityLogin(
    authorityUrl: string,
    args: { username: string; password: string }
  ): Promise<AssertionResult> {
    return authorityReq(authorityUrl, "/api/account/login", args);
  },
  /** 云端发验证码(注册/找回共用,后端频控) */
  authoritySendCode(
    authorityUrl: string,
    email: string,
    purpose: "signup" | "reset"
  ): Promise<void> {
    return authorityReq(authorityUrl, "/api/collab/email/send_code", {
      email,
      purpose,
    });
  },
  /** 云端改密:改完**所有主机**同时生效(没有任何主机存着这个密码) */
  authorityReset(
    authorityUrl: string,
    args: { email: string; code: string; newPassword: string }
  ): Promise<{ ok: boolean; username?: string }> {
    return authorityReq(authorityUrl, "/api/account/reset", args);
  },
  /** 拿云端断言换**当前主机**的会话:主机纯本地验签,断网也认 */
  loginWithAssertion(assertion: string): Promise<AuthResult> {
    return post("/api/collab/login_assertion", {
      assertion,
      deviceId: deviceId(),
    });
  },
  /** owner 重新信任账号中心(换云机/轮换密钥);kid 非空则要求指纹对得上 */
  authorityRepin(kid?: string): Promise<{ ok: boolean; authorityUrl: string; kid: string }> {
    return post("/api/collab/authority/repin", { kid: kid ?? "" });
  },

  /** 仅零账号时可用:首次初始化,创建 owner。hostSelf=本机正当主机(设备页点亮主机徽标)。
   *  setupToken = 主机访问口令(collab_host_status.accessToken)—— 后端 bootstrap 现在
   *  强制校验它,不带必 401。 */
  bootstrap(args: {
    username: string;
    password: string;
    displayName: string;
    hostSelf?: boolean;
    setupToken?: string;
  }): Promise<AuthResult> {
    const { setupToken, ...rest } = args;
    return req("/api/collab/bootstrap", {
      method: "POST",
      body: { ...rest, deviceId: deviceId() },
      token: setupToken,
    });
  },

  login(args: { username: string; password: string }): Promise<AuthResult> {
    return post("/api/collab/login", { ...args, deviceId: deviceId() });
  },

  /** 开放注册(主机可关闭:403 时提示走票据入伙) */
  signup(args: {
    username: string;
    password: string;
    displayName: string;
  }): Promise<AuthResult> {
    return post("/api/collab/signup", { ...args, deviceId: deviceId() });
  },

  // ── 邮箱验证码:注册 / 找回密码 ──
  emailStatus(): Promise<EmailStatus> {
    return get("/api/collab/email/status");
  },
  /** 发验证码(60s 重发间隔 + 每小时 5 封,后端频控) */
  sendEmailCode(email: string, purpose: "signup" | "reset"): Promise<void> {
    return post("/api/collab/email/send_code", { email, purpose });
  },
  /** 邮箱验证码注册:验证通过即建号+绑邮箱+登录 */
  emailSignup(args: {
    email: string;
    code: string;
    username: string;
    password: string;
    displayName: string;
  }): Promise<AuthResult> {
    return post("/api/collab/email/signup", { ...args, deviceId: deviceId() });
  },
  /** 邮箱找回密码:改密并踢掉旧会话;回 username 提示用哪个名登录 */
  emailReset(args: {
    email: string;
    code: string;
    newPassword: string;
  }): Promise<{ ok: boolean; username?: string }> {
    return post("/api/collab/email/reset", args);
  },
  adminEmailConfig(): Promise<EmailConfig> {
    return get("/api/collab/admin/email_config");
  },
  /** pass 留空 = 保留旧授权码;testTo 非空则顺手发测试信 */
  adminEmailConfigSet(cfg: {
    host: string;
    port: number;
    user: string;
    pass: string;
    from: string;
    signupOpen: boolean;
    testTo?: string;
  }): Promise<{ ok: boolean; configured: boolean }> {
    return post("/api/collab/admin/email_config", cfg);
  },

  /** 用户名/昵称模糊搜索(登录后可用,限 20 条) */
  searchUsers(q: string): Promise<UserSearchHit[]> {
    return get(`/api/collab/users/search?q=${encodeURIComponent(q)}`);
  },

  // ── 团队 ──
  listTeams(): Promise<CollabTeam[]> {
    return get("/api/collab/teams");
  },
  createTeam(name: string): Promise<CollabTeam> {
    return post("/api/collab/teams", { name });
  },
  teamMembers(teamId: number): Promise<TeamMember[]> {
    return get(`/api/collab/team/members?teamId=${String(teamId)}`);
  },
  addTeamMember(
    teamId: number,
    username: string,
    role: "member" | "owner"
  ): Promise<void> {
    return post("/api/collab/team/members", { teamId, username, role });
  },
  removeTeamMember(teamId: number, userId: number): Promise<void> {
    return post("/api/collab/team/member_remove", { teamId, userId });
  },

  logout(): Promise<void> {
    return post("/api/collab/logout");
  },

  me(): Promise<{ username: string; role: string }> {
    return get("/api/collab/me");
  },

  /** 票据入伙:凭邀请码建账号并绑定本机 */
  redeem(args: {
    code: string;
    username: string;
    password: string;
    displayName: string;
    deviceName: string;
    nodeId: string;
  }): Promise<AuthResult> {
    return post("/api/collab/redeem", args);
  },

  // ── owner 管理 ──
  adminTicket(args: { role: string; note: string }): Promise<Ticket> {
    return post("/api/collab/admin/ticket", args);
  },
  adminUsers(): Promise<AdminUser[]> {
    return get("/api/collab/admin/users");
  },
  adminUserDisable(userId: number, disabled: boolean): Promise<void> {
    return post("/api/collab/admin/user_disable", { userId, disabled });
  },
  /** owner 直接重置某用户密码(邮箱没绑时的兜底通道,旧会话全部作废) */
  adminUserResetPassword(userId: number, newPassword: string): Promise<void> {
    return post("/api/collab/admin/user_reset_password", { userId, newPassword });
  },
  adminDevices(): Promise<AdminDevice[]> {
    return get("/api/collab/admin/devices");
  },
  adminAudit(limit = 50): Promise<AuditRow[]> {
    return get(`/api/collab/admin/audit?limit=${limit}`);
  },
  adminDeviceRevoke(devId: string): Promise<void> {
    return post("/api/collab/admin/device_revoke", { deviceId: devId });
  },

  // ── 项目 ──
  listProjects(): Promise<CollabProject[]> {
    return get("/api/collab/projects");
  },
  createProject(
    name: string,
    repo: string,
    teamId?: number | null
  ): Promise<CollabProject> {
    return post("/api/collab/projects", {
      name,
      repo,
      ...(teamId != null ? { teamId } : {}),
    });
  },
  projectMembers(projectId: number): Promise<ProjectMember[]> {
    return get(
      `/api/collab/project/members?projectId=${String(projectId)}`
    );
  },
  addMember(projectId: number, userId: number, role: string): Promise<void> {
    return post("/api/collab/project/members", { projectId, userId, role });
  },
  /** 按用户名拉项目成员(新后端支持) */
  addMemberByName(
    projectId: number,
    username: string,
    role: string
  ): Promise<void> {
    return post("/api/collab/project/members", { projectId, username, role });
  },
  setLead(projectId: number, expertId: string): Promise<void> {
    return post("/api/collab/project/lead", { projectId, expertId });
  },

  // ── 任务卡 ──
  listTasks(projectId: number): Promise<TaskCard[]> {
    return get(`/api/collab/tasks?projectId=${String(projectId)}`);
  },
  createTask(args: {
    projectId: number;
    title: string;
    body: string;
    scope: string;
    criteria: string;
  }): Promise<TaskCard> {
    return post("/api/collab/tasks", args);
  },
  claimTask(taskId: number): Promise<void> {
    return post("/api/collab/task/claim", { taskId });
  },
  submitTask(taskId: number, prId?: string): Promise<void> {
    return post(
      "/api/collab/task/submit",
      prId ? { taskId, prId } : { taskId }
    );
  },
  reviewTask(
    taskId: number,
    pass: boolean,
    comments: ReviewComment[],
    asLead = false
  ): Promise<void> {
    return post("/api/collab/task/review", { taskId, pass, comments, asLead });
  },
  archiveTask(taskId: number): Promise<void> {
    return post("/api/collab/task/archive", { taskId });
  },
  cancelTask(taskId: number): Promise<void> {
    return post("/api/collab/task/cancel", { taskId });
  },
  taskRounds(taskId: number): Promise<TaskRound[]> {
    return get(`/api/collab/task/rounds?taskId=${String(taskId)}`);
  },

  // ── 检查工作流(CI-lite:提交送验自动跑,结果亮在卡上) ──
  checks(taskId: number): Promise<ChecksResp> {
    return get(`/api/collab/checks?taskId=${String(taskId)}`);
  },
  checksRerun(taskId: number): Promise<{ ok: boolean }> {
    return post("/api/collab/checks/rerun", { taskId });
  },
  /** 设项目检查档位 code(全套)/creative(视频·游戏放宽)/off(管理者)。
   *  checkSkill 可选:同时设检查技能(空串=回到默认内置技能) */
  checksSetProfile(
    projectId: number,
    profile: string,
    checkSkill?: string
  ): Promise<{ ok: boolean }> {
    return post("/api/collab/checks/profile", {
      projectId,
      profile,
      ...(checkSkill !== undefined ? { checkSkill } : {}),
    });
  },
  /** 主机上可用作检查项的技能清单(检查设置下拉) */
  checksSkills(): Promise<{
    skills: { id: string; name: string }[];
    default: string;
  }> {
    return get("/api/collab/checks/skills");
  },

  // ── 任务级对话(多轮微调通道) ──
  taskMessages(taskId: number, afterId = 0): Promise<TaskMessage[]> {
    return get(
      `/api/collab/tasks/${String(taskId)}/messages?afterId=${String(afterId)}&limit=100`
    );
  },
  postTaskMessage(
    taskId: number,
    body: string,
    idemKey?: string
  ): Promise<TaskMessage> {
    return post(`/api/collab/tasks/${String(taskId)}/messages`, {
      body,
      ...(idemKey ? { idemKey } : {}),
    });
  },
  /** 主 Agent 在对话里回一条(管理者手动触发,控制 token 用量) */
  aiTaskReply(taskId: number): Promise<TaskMessage> {
    return post(`/api/collab/tasks/${String(taskId)}/ai-reply`);
  },

  /** 设项目共享可见路径(CSV,管理者):协作者开工时并入稀疏集 */
  setSharedScope(
    projectId: number,
    sharedScope: string
  ): Promise<{ ok: boolean }> {
    return post("/api/collab/projects/shared-scope", {
      projectId,
      sharedScope,
    });
  },

  // ── 合并闸门(冲突裁决台) ──
  mergeTrial(taskId: number): Promise<MergeTrial> {
    return post("/api/collab/merge/trial", { taskId });
  },
  /** 逐块裁决落地:全部冲突块处置齐才允许,落成任务分支上的合并提交 */
  mergeResolve(
    taskId: number,
    resolutions: Record<string, BlockResolution[]>
  ): Promise<{ ok: boolean; commit: string }> {
    return post("/api/collab/merge/resolve", { taskId, resolutions });
  },
  /** squash 放行;force=true 跳过检查闸强推(仅 owner,服务端留审计痕) */
  mergeSquash(
    taskId: number,
    force = false
  ): Promise<{ ok: boolean; commit: string; card: TaskCard }> {
    return post(
      "/api/collab/merge/squash",
      force ? { taskId, force: true } : { taskId }
    );
  },
  mergeRevert(
    projectId: number,
    commit: string
  ): Promise<{ ok: boolean; commit: string }> {
    return post("/api/collab/merge/revert", { projectId, commit });
  },

  // ── 主 Agent 授权 ──
  leadGrants(projectId: number): Promise<LeadGrants> {
    return get(`/api/collab/lead/grants?projectId=${String(projectId)}`);
  },
  setLeadGrants(projectId: number, grants: LeadGrants): Promise<void> {
    return post("/api/collab/lead/grants", { projectId, grants });
  },

  /** 主 Agent 改派(须 can_reassign 授权位,lead.rs 三问把关) */
  leadAssign(taskId: number, userId: number): Promise<TaskCard> {
    return post("/api/collab/lead/assign", { taskId, userId });
  },
  /** 主 Agent 催办:返回超期无动静的卡(默认 48h) */
  leadNudge(projectId: number, staleHours = 48): Promise<TaskCard[]> {
    return post("/api/collab/lead/nudge", { projectId, staleHours });
  },

  // ── 主 Agent AI(模型只产草案,落地全过闸) ──
  leadModel(): Promise<LeadModelCfg> {
    return get("/api/collab/lead/model");
  },
  setLeadModel(cfg: LeadModelCfg): Promise<void> {
    return post("/api/collab/lead/model", cfg);
  },
  /** AI 拆卡:目标 → 草案列表;dispatch=true 且有 auto_dispatch 授权时服务端直接建卡 */
  aiDecompose(
    projectId: number,
    goal: string,
    memberHint = "",
    dispatch = false
  ): Promise<{ drafts: CardDraft[]; created: TaskCard[] }> {
    return post("/api/collab/lead/ai/decompose", {
      projectId,
      goal,
      memberHint,
      dispatch,
    });
  },
  /** AI 验收:服务端自动取分支 diff 喂模型,返回意见草稿(不落状态机) */
  aiReview(taskId: number): Promise<ReviewDraft> {
    return post("/api/collab/lead/ai/review", { taskId });
  },
  /** AI 融合草案:对单个冲突块起草融合文本(只产草案,落地走 mergeResolve) */
  aiFuse(
    taskId: number,
    file: string,
    blockIndex: number
  ): Promise<{ text: string }> {
    return post("/api/collab/lead/ai/fuse", { taskId, file, blockIndex });
  },

  // ── 晨报 ──
  morning(projectId: number): Promise<MorningReport> {
    return get(`/api/collab/lead/morning?projectId=${String(projectId)}`);
  },

  /** 项目动态时间线(验收轮次+任务状态合成,项目主页概览用) */
  activity(projectId: number, limit = 30): Promise<ActivityItem[]> {
    return get(
      `/api/collab/activity?projectId=${String(projectId)}&limit=${String(limit)}`
    );
  },
};

/** unix 秒 → 本地可读时间 */
export function fmtTime(ts: number | null | undefined): string {
  if (!ts) return "—";
  try {
    return new Date(ts * 1000).toLocaleString("zh-CN", {
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return String(ts);
  }
}

// ── 分享码(配对码带主机地址,零手填入伙) ──────────────

/** 分享码 PLRS1-<base64url(json{c,a})> → {code, addrs};不是分享码返回 null(裸码走旧流程) */
export function parseShareCode(
  s: string
): { code: string; addrs: string[]; authority?: string; kid?: string } | null {
  const m = s.trim();
  if (!m.startsWith("PLRS1-")) return null;
  try {
    const b64 = m.slice(6).replace(/-/g, "+").replace(/_/g, "/");
    const pad = b64 + "=".repeat((4 - (b64.length % 4)) % 4);
    const v = JSON.parse(atob(pad));
    if (typeof v.c !== "string" || !Array.isArray(v.a)) return null;
    return {
      code: v.c,
      addrs: v.a.filter((x: unknown): x is string => typeof x === "string"),
      // u/k:主机的账号由云端账号中心统管时带上 —— 收码人据此知道去哪儿注册/登录,
      // 以及该信任哪把钥匙(kid 供人眼核对)。老码没有这两项,读到 undefined 即老行为。
      authority: typeof v.u === "string" && v.u ? v.u : undefined,
      kid: typeof v.k === "string" && v.k ? v.k : undefined,
    };
  } catch {
    return null;
  }
}

/** 连接码 PLRK1-<base64url(json{t:token, a:[addrs], n?:nodeId})> → {token, addrs, nodeId};
 *  非该格式返回 null。与手机端 net.ts 的 parseConnectCode 同一套:主机「互联」页把
 *  地址 + owner 令牌 + iroh NodeId 打包成一串,自己的设备粘一串即连,不用分别手填。 */
export function parseConnectCode(
  s: string
): {
  token: string;
  addrs: string[];
  nodeId?: string;
  authority?: string;
} | null {
  const m = s.trim();
  if (!m.startsWith("PLRK1-")) return null;
  try {
    const b64 = m.slice(6).replace(/-/g, "+").replace(/_/g, "/");
    const pad = b64 + "=".repeat((4 - (b64.length % 4)) % 4);
    const v = JSON.parse(atob(pad));
    if (typeof v.t !== "string" || !Array.isArray(v.a)) return null;
    return {
      token: v.t,
      addrs: v.a.filter((x: unknown): x is string => typeof x === "string"),
      nodeId: typeof v.n === "string" && v.n ? v.n : undefined,
      // u:这台主机的账号中心。令牌本身就是凭据、进门用不着它,
      // 但收码人换账号登录时得知道往哪儿打。
      authority: typeof v.u === "string" && v.u ? v.u : undefined,
    };
  } catch {
    return null;
  }
}

/** 逐个探活分享码里的地址,返回第一个 /api/health 能通的(单个 2.5s 超时)。
 *  分享码里可能混着连不上的地址(如代理 TUN 虚拟网卡的 IP),探活自动跳过。 */
export async function probeHost(addrs: string[]): Promise<string | null> {
  for (const a of addrs) {
    const base = a.replace(/\/+$/, "");
    try {
      const ctl = new AbortController();
      const t = setTimeout(() => ctl.abort(), 2500);
      const r = await fetch(base + "/api/health", { signal: ctl.signal });
      clearTimeout(t);
      if (r.ok) return base;
    } catch {
      /* 试下一个 */
    }
  }
  return null;
}
