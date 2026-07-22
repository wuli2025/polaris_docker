import { defineStore } from "pinia";
import { ref, computed } from "vue";
import {
  convApi,
  isTauri,
  invoke,
  type Conversation,
  type Project,
} from "../tauri";
import { useChatStore } from "./chat";
import { toast } from "../composables/useToast";
import { humanizeError } from "../lib/humanizeError";

/** 右抽屉的三种宽度形态：默认抽屉 / 成品预览 / 放大编辑 */
export type DrawerWidthMode = "default" | "preview" | "expand";

export type ViewKey =
  | "chat"
  | "wiki"
  | "file_center"
  | "graph"
  | "automation"
  | "sandbox"
  | "claude_md"
  | "skill_center"
  | "env_doctor"
  | "mcp"
  | "update"
  | "feishu"
  | "settings"
  | "sense_api"
  | "voice_input"
  | "video_course"
  | "media_ops"
  | "deck"
  | "web_studio"
  | "collab"
  | "interconnect"
  | "collab_project";

export const useAppStore = defineStore("app", () => {
  const view = ref<ViewKey>("chat");
  const sidebarCollapsed = ref(false);
  // 右抽屉(成品预览)默认收起 → 把横向空间让给主区;点对话里的成品 chip 或顶栏抽屉按钮即展开
  const drawerCollapsed = ref(true);

  // 置顶对话：仅前端持久化（localStorage），侧栏排序时置顶优先
  const PINNED_KEY = "polaris.pinnedConvs.v1";
  function loadPinned(): Set<string> {
    try {
      const raw = localStorage.getItem(PINNED_KEY);
      if (raw) return new Set(JSON.parse(raw) as string[]);
    } catch {
      /* ignore corrupt storage */
    }
    return new Set();
  }
  const pinnedConvs = ref<Set<string>>(loadPinned());
  function persistPinned() {
    try {
      localStorage.setItem(PINNED_KEY, JSON.stringify([...pinnedConvs.value]));
    } catch {
      /* storage may be unavailable */
    }
  }
  function isPinned(convId: string | null | undefined): boolean {
    return !!convId && pinnedConvs.value.has(convId);
  }
  function togglePin(convId: string) {
    if (!convId) return;
    const s = new Set(pinnedConvs.value);
    if (s.has(convId)) s.delete(convId);
    else s.add(convId);
    pinnedConvs.value = s;
    persistPinned();
  }

  // 主题：浅色（默认·暖白水墨）/ 黑夜（深空玻璃，抄自智能选股版）。
  // 挂到 <html data-theme="dark"> 上由 style.css 的 token 覆盖块全局换肤。
  const THEME_KEY = "polaris.theme.v1";
  // light=软白水墨 / dark=墨黑 / aurora-light=软白+极光画框 / aurora-dark=墨黑+灰画框
  type Theme = "light" | "dark" | "aurora-light" | "aurora-dark";
  function loadTheme(): Theme {
    try {
      const t = localStorage.getItem(THEME_KEY);
      if (
        t === "light" ||
        t === "dark" ||
        t === "aurora-light" ||
        t === "aurora-dark"
      )
        return t;
      if (t === "nougat") return "aurora-light"; // 旧键迁移
      return "aurora-light"; // 未选择过 → 默认极光琉璃画框(软白)
    } catch {
      return "aurora-light";
    }
  }
  const theme = ref<Theme>(loadTheme());
  function applyTheme() {
    // light = 默认（无属性）；其余挂 data-theme 由 style.css token 块换肤
    if (theme.value === "light") {
      document.documentElement.removeAttribute("data-theme");
    } else {
      document.documentElement.setAttribute("data-theme", theme.value);
    }
    // 原生标题栏跟随主题染成画框色（仅桌面端；Win11 生效，Win10 静默跳过）
    if (isTauri) {
      const titlebar: Record<Theme, { caption: string; text: string }> = {
        light: { caption: "#f3f2eb", text: "#1a1a1c" }, // 暖米框面，与侧栏无色差
        dark: { caption: "#1f1f1f", text: "#ececea" }, // 石墨框面
        "aurora-light": { caption: "#eef1fa", text: "#232436" }, // 珠光浅画框
        "aurora-dark": { caption: "#1c1d20", text: "#ececea" }, // 墨黑+灰画框
      };
      invoke("set_titlebar_color", titlebar[theme.value]).catch(() => {});
    }
  }
  function setTheme(t: Theme) {
    theme.value = t;
    try {
      localStorage.setItem(THEME_KEY, t);
    } catch {
      /* storage may be unavailable */
    }
    applyTheme();
  }
  applyTheme(); // store 初始化（App 启动）时立即生效，避免闪白

  // 任务完成但用户未查看的会话集合 → 侧栏显示墨蓝色未读点
  const unreadConvs = ref<Set<string>>(new Set());
  function markUnread(convId: string) {
    if (!convId) return;
    // 正在查看的对话不标记
    if (convId === currentConvId.value) return;
    unreadConvs.value = new Set(unreadConvs.value).add(convId);
  }
  function clearUnread(convId: string) {
    if (!unreadConvs.value.has(convId)) return;
    const s = new Set(unreadConvs.value);
    s.delete(convId);
    unreadConvs.value = s;
  }

  // 项目 + 对话
  const projects = ref<Project[]>([]);
  const expandedProjects = ref<Set<string>>(new Set());
  const conversationsByProject = ref<Record<string, Conversation[]>>({});
  const currentConvId = ref<string | null>(null);
  const currentProjectId = ref<string | null>(null);

  // 「召唤专家」跨视图通道:「召唤其它专家」跳到「专家团」页,在那里点某张卡的「召唤」时,
  // 经此把 (kind,id) 投递给对话区(ChatPanel 挂载时消费)去真正入驻 + 记入最近召唤。
  // 带 nonce:连续召唤同一个也能触发消费。
  const pendingSummon = ref<{ kind: "expert" | "team"; id: string; nonce: number } | null>(null);

  function setView(v: ViewKey) {
    view.value = v;
  }
  // 在「专家团」页点「召唤」→ 投递召唤意图并切回对话区,由 ChatPanel 落地。
  function requestSummon(kind: "expert" | "team", id: string) {
    pendingSummon.value = { kind, id, nonce: (pendingSummon.value?.nonce ?? 0) + 1 };
    view.value = "chat";
  }
  function toggleSidebar() {
    sidebarCollapsed.value = !sidebarCollapsed.value;
  }
  function toggleDrawer() {
    drawerCollapsed.value = !drawerCollapsed.value;
  }

  // 侧栏宽度可拖拽调节(200–420px),记住选择
  const SIDEBAR_W_KEY = "polaris.sidebarWidth.v1";
  const sidebarUserWidth = ref(
    Math.min(420, Math.max(200, parseInt(localStorage.getItem(SIDEBAR_W_KEY) || "260") || 260))
  );
  // persist=false：拖拽中每帧调用,只更新内存值(避免 60fps 同步写盘卡顿);
  // 松手时再 persist=true 落一次盘。
  function setSidebarWidth(w: number, persist = true) {
    sidebarUserWidth.value = Math.min(420, Math.max(200, Math.round(w)));
    if (!persist) return;
    try {
      localStorage.setItem(SIDEBAR_W_KEY, String(sidebarUserWidth.value));
    } catch {
      /* storage 不可用 */
    }
  }
  const sidebarWidth = computed(() =>
    sidebarCollapsed.value ? 48 : sidebarUserWidth.value
  );
  // ── 右抽屉宽度可拖拽调节（WorkBuddy 式收缩框）──
  // 三种形态各记各的宽：默认抽屉 / 成品预览 / 放大编辑。拖一次就记住，
  // 下次进入同一形态直接复原；没拖过(null)则走 App.vue 里的自适应默认档位。
  const DRAWER_W_KEYS: Record<DrawerWidthMode, string> = {
    default: "polaris.drawerWidth.default.v1",
    preview: "polaris.drawerWidth.preview.v1",
    expand: "polaris.drawerWidth.expand.v1",
  };
  const DRAWER_LIMITS: Record<DrawerWidthMode, { min: number; max: () => number }> = {
    default: { min: 240, max: () => Math.max(320, Math.round(window.innerWidth * 0.5)) },
    preview: { min: 320, max: () => Math.max(420, Math.round(window.innerWidth * 0.8)) },
    expand: { min: 520, max: () => Math.max(640, Math.round(window.innerWidth * 0.92)) },
  };
  function loadDrawerW(mode: DrawerWidthMode): number | null {
    try {
      const n = parseInt(localStorage.getItem(DRAWER_W_KEYS[mode]) || "");
      return Number.isFinite(n) && n >= 200 ? n : null;
    } catch {
      return null;
    }
  }
  const drawerWidths = ref<Record<DrawerWidthMode, number | null>>({
    default: loadDrawerW("default"),
    preview: loadDrawerW("preview"),
    expand: loadDrawerW("expand"),
  });
  // 拖拽中：App.vue 据此关掉 grid 列宽过渡，避免跟手延迟
  const drawerResizing = ref(false);
  function clampDrawerW(mode: DrawerWidthMode, w: number): number {
    const L = DRAWER_LIMITS[mode];
    return Math.min(L.max(), Math.max(L.min, Math.round(w)));
  }
  // persist=false：拖拽中每帧只更新内存值；松手时再 persist=true 落一次盘（同侧栏）
  function setDrawerWidth(mode: DrawerWidthMode, w: number, persist = true) {
    const v = clampDrawerW(mode, w);
    drawerWidths.value = { ...drawerWidths.value, [mode]: v };
    if (!persist) return;
    try {
      localStorage.setItem(DRAWER_W_KEYS[mode], String(v));
    } catch {
      /* storage 不可用 */
    }
  }
  /** 双击分隔条：恢复该形态的自适应默认宽 */
  function resetDrawerWidth(mode: DrawerWidthMode) {
    drawerWidths.value = { ...drawerWidths.value, [mode]: null };
    try {
      localStorage.removeItem(DRAWER_W_KEYS[mode]);
    } catch {
      /* storage 不可用 */
    }
  }
  // 收起后右抽屉完全消失（0 宽，不留小框/导轨）；需要时点对话顶栏的抽屉按钮或生成产物自动展开
  const drawerWidth = computed(() =>
    drawerCollapsed.value ? 0 : drawerWidths.value.default ?? 300
  );

  // MCP 配置弹窗（全局状态，Sidebar 与 App 共用）
  const showMcpModal = ref(false);

  async function refreshProjects() {
    try {
      projects.value = await convApi.listProjects();
    } catch (e) {
      // 静默失败=侧栏空白没人知道为什么;报出去并保留旧列表
      toast.error(`项目列表加载失败:${humanizeError(e)}`);
      return;
    }
    if (!currentProjectId.value && projects.value.length) {
      currentProjectId.value = projects.value[0].id;
      expandedProjects.value.add(currentProjectId.value);
    }
    // 首屏只「等」当前项目的对话到位即可让侧栏渲染;其余项目的对话在后台并发补齐。
    // 侧栏项目排序虽依赖各项目对话的活跃时间,但那些时间戳「后到」无妨——先把界面画
    // 出来(不被项目数 × 一次 invoke 的串扇出阻塞首帧),批次到齐后一次性响应重排。
    // 旧版 `await Promise.all(所有项目)` 在项目多时会把首屏卡成 O(项目数)。
    const cur = currentProjectId.value;
    if (cur) await refreshConversations(cur);
    const rest = projects.value.filter((p) => p.id !== cur).map((p) => p.id);
    if (rest.length) void loadConversationsBatch(rest);
  }

  /** 后台批量拉多个项目的对话,全部到齐后做「一次」响应更新(避免 N 次 spread 抖动)。 */
  async function loadConversationsBatch(projectIds: string[]) {
    const results = await Promise.all(
      projectIds.map(async (id): Promise<[string, Conversation[]] | null> => {
        try {
          return [id, await convApi.listConversations(id)];
        } catch {
          return null; // 单个项目失败不连累其余;侧栏该项目暂空,下次刷新再补
        }
      })
    );
    const next = { ...conversationsByProject.value };
    for (const r of results) if (r) next[r[0]] = r[1];
    conversationsByProject.value = next;
  }

  async function refreshConversations(projectId: string) {
    try {
      conversationsByProject.value[projectId] =
        await convApi.listConversations(projectId);
    } catch (e) {
      toast.error(`对话列表加载失败:${humanizeError(e)}`);
      return;
    }
    // Vue 3 reactive: 替换 ref 触发更新
    conversationsByProject.value = { ...conversationsByProject.value };
  }

  async function toggleProject(projectId: string) {
    if (expandedProjects.value.has(projectId)) {
      expandedProjects.value.delete(projectId);
    } else {
      expandedProjects.value.add(projectId);
      if (!conversationsByProject.value[projectId]) {
        await refreshConversations(projectId);
      }
    }
    expandedProjects.value = new Set(expandedProjects.value);
  }

  async function createProject(name: string) {
    const p = await convApi.createProject(name);
    projects.value = [...projects.value, p];
    expandedProjects.value = new Set([...expandedProjects.value, p.id]);
    currentProjectId.value = p.id;
    conversationsByProject.value = { ...conversationsByProject.value, [p.id]: [] };
    return p;
  }

  /** 本地项目 ↔ 协作项目绑定(团队项目主页/侧栏联动之桥) */
  async function bindProjectToCollab(
    projectId: string,
    collabProjectId: number,
    collabHost: string
  ) {
    const p = await convApi.bindProjectCollab(projectId, collabProjectId, collabHost);
    const i = projects.value.findIndex((x) => x.id === p.id);
    if (i >= 0) {
      const next = [...projects.value];
      next[i] = p;
      projects.value = next;
    }
    return p;
  }

  /** 按协作项目 id 反查绑定的本地项目(无则 undefined) */
  function projectByCollabId(collabId: number) {
    return projects.value.find((p) => p.collabProjectId === collabId);
  }

  // 归档项目 = 从活动列表移除(后端只置 archived 标记, 对话/消息保留, 不做硬删除)
  async function archiveProject(projectId: string) {
    await convApi.archiveProject(projectId);
    projects.value = projects.value.filter((p) => p.id !== projectId);
    const next = { ...conversationsByProject.value };
    delete next[projectId];
    conversationsByProject.value = next;
    if (expandedProjects.value.has(projectId)) {
      expandedProjects.value.delete(projectId);
      expandedProjects.value = new Set(expandedProjects.value);
    }
    // 当前项目被归档 → 回退到第一个剩余项目
    if (currentProjectId.value === projectId) {
      currentProjectId.value = projects.value[0]?.id ?? null;
    }
  }

  // 在系统文件管理器中打开该项目的工作目录
  async function openProjectDir(projectId: string) {
    await convApi.openProjectDir(projectId);
  }

  /** 设置(或清除)项目的工作目录：本项目下所有对话以此为 claude cwd（终端 cd 进 repo 同款）。
   *  workDir 传 null/空 = 解绑回落默认。成功后就地更新本地项目的 workDir。 */
  async function setProjectWorkDir(projectId: string, workDir: string | null) {
    await convApi.setWorkDir(projectId, workDir);
    const i = projects.value.findIndex((x) => x.id === projectId);
    if (i >= 0) {
      const next = [...projects.value];
      next[i] = { ...next[i], workDir: workDir && workDir.trim() ? workDir : null };
      projects.value = next;
    }
  }

  /**
   * @param navigate 是否切到 chat 视图。默认 true(侧栏/对话面板新建即跳进对话)。
   *   工坊类组件(Deck/Web 等)自己管理视图、就地展示预览, 必须传 false ——
   *   否则 setView('chat') 会卸载工坊组件、连带销毁其状态机/预览/「继续修改」。
   */
  async function createConversation(projectId: string, navigate = true) {
    const c = await convApi.createConversation(projectId);
    const cur = conversationsByProject.value[projectId] ?? [];
    conversationsByProject.value = {
      ...conversationsByProject.value,
      [projectId]: [c, ...cur],
    };
    expandedProjects.value = new Set([...expandedProjects.value, projectId]);
    currentConvId.value = c.id;
    // 同步标记这条新对话为「历史已加载(空)」——必须紧跟 currentConvId 赋值、其间不能有
    // await。否则 currentConvId 变更触发的 loadHistory(微任务)会在首条消息推入后用空历史
    // 把它覆盖掉(现象:第一次给对话发消息经常被「吃掉」)。覆盖所有「新建即发送」入口。
    useChatStore().markFresh(c.id);
    currentProjectId.value = projectId;
    if (navigate) setView("chat");
    // 新建对话即预热: 新对话必然还没有常驻进程, 正是预热收益最大的时刻
    // (用户接下来要打的就是首条消息)。
    useChatStore().prewarm(c.id);
    return c;
  }

  async function deleteConversation(conv: Conversation) {
    await convApi.deleteConversation(conv.id);
    const cur = conversationsByProject.value[conv.projectId] ?? [];
    conversationsByProject.value = {
      ...conversationsByProject.value,
      [conv.projectId]: cur.filter((c) => c.id !== conv.id),
    };
    if (currentConvId.value === conv.id) {
      currentConvId.value = null;
    }
    // 删除后顺手清掉置顶标记，避免遗留垃圾
    if (pinnedConvs.value.has(conv.id)) togglePin(conv.id);
  }

  /** 回声层:归档对话 —— 从列表移除(消息保留在磁盘,可逆),不删数据。 */
  async function archiveConversation(conv: Conversation) {
    await convApi.archiveConversation(conv.id, true);
    const cur = conversationsByProject.value[conv.projectId] ?? [];
    conversationsByProject.value = {
      ...conversationsByProject.value,
      [conv.projectId]: cur.filter((c) => c.id !== conv.id),
    };
    if (currentConvId.value === conv.id) {
      currentConvId.value = null;
    }
  }

  async function renameConversation(conv: Conversation, title: string) {
    const t = title.trim();
    if (!t || t === conv.title) return;
    await convApi.renameConversation(conv.id, t);
    const cur = conversationsByProject.value[conv.projectId] ?? [];
    conversationsByProject.value = {
      ...conversationsByProject.value,
      [conv.projectId]: cur.map((c) => (c.id === conv.id ? { ...c, title: t } : c)),
    };
  }

  function selectConversation(conv: Conversation) {
    currentConvId.value = conv.id;
    currentProjectId.value = conv.projectId;
    clearUnread(conv.id);
    setView("chat");
    // 打开对话即预热常驻 claude 进程(fire-and-forget): 用户看历史/打字的几秒里
    // 把 CLI ~6.4s 自举跑完, 首条消息首响 ~10s → ~3s。失败静默, 不影响任何流程。
    useChatStore().prewarm(conv.id);
  }

  /** 按 id 找对话标题(任务中心给后台 AI 任务起可读名用);找不到返回空串。 */
  function convTitle(convId: string | null): string {
    if (!convId) return "";
    for (const list of Object.values(conversationsByProject.value)) {
      const c = list.find((x) => x.id === convId);
      if (c) return c.title;
    }
    return "";
  }
  /** 按 id 跳转到某对话(任务中心点击后台任务用)。 */
  function openConversationById(convId: string) {
    for (const list of Object.values(conversationsByProject.value)) {
      const c = list.find((x) => x.id === convId);
      if (c) {
        selectConversation(c);
        return;
      }
    }
  }

  return {
    // ui
    view,
    sidebarCollapsed,
    drawerCollapsed,
    sidebarWidth,
    setSidebarWidth,
    drawerWidth,
    drawerWidths,
    drawerResizing,
    setDrawerWidth,
    resetDrawerWidth,
    showMcpModal,
    theme,
    setTheme,
    setView,
    pendingSummon,
    requestSummon,
    toggleSidebar,
    toggleDrawer,
    unreadConvs,
    markUnread,
    clearUnread,
    // pin
    pinnedConvs,
    isPinned,
    togglePin,
    // conv
    projects,
    expandedProjects,
    conversationsByProject,
    currentConvId,
    currentProjectId,
    refreshProjects,
    refreshConversations,
    toggleProject,
    createProject,
    bindProjectToCollab,
    projectByCollabId,
    archiveProject,
    openProjectDir,
    setProjectWorkDir,
    createConversation,
    deleteConversation,
    archiveConversation,
    renameConversation,
    selectConversation,
    convTitle,
    openConversationById,
  };
});
