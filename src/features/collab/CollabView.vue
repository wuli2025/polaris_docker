<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import {
  Handshake,
  LoaderCircle,
  LogOut,
  Plus,
  Plug,
  Kanban,
  Server,
  ShieldCheck,
  ChevronDown,
  Crown,
  FolderGit2,
  Users,
  UsersRound,
  X,
} from "@lucide/vue";
import { invoke, isTauri } from "../../tauri";
import { parseShareCode } from "./api";
import { useCollabStore } from "./stores/collab";
import TaskBoard from "./TaskBoard.vue";
import CollabAdmin from "./CollabAdmin.vue";
import LeadAgentCard from "./LeadAgentCard.vue";
import TeamMembers from "./TeamMembers.vue";
import { toast } from "../../composables/useToast";

const collab = useCollabStore();

onMounted(() => {
  void collab.init();
  if (isTauri) {
    void collab.hostStatus().then((s) => {
      // 主机自启续联:本机在当主机但 base 丢了(如清过缓存)→ 自动指回本机
      if (s?.running && !collab.base) {
        collab.applyBase(`http://127.0.0.1:${s.port}`);
      }
    });
  }
});

// ── P2P 隧道状态徽标(桌面版:轮询 collab_tunnel_status,断线/重连一目了然) ──
interface TunnelStatus {
  running: boolean;
  state?: string; // stopped|connecting|connected|reconnecting
  latency_ms?: number | null;
  last_error?: string;
  connections?: number;
}
const tunnel = ref<TunnelStatus | null>(null);
const TUNNEL_LABEL: Record<string, string> = {
  connected: "隧道已连",
  connecting: "隧道连接中",
  reconnecting: "隧道重连中",
  stopped: "隧道未启",
};
function pollTunnel() {
  if (!isTauri) return;
  void invoke<TunnelStatus>("collab_tunnel_status")
    .then((s) => {
      tunnel.value = s ?? null;
    })
    .catch(() => {
      tunnel.value = null;
    });
}
onMounted(() => {
  pollTunnel();
  if (isTauri) setInterval(pollTunnel, 10_000);
});

// ── 一键当主机(桌面版):本机起协作服务 → 直接进初始化/登录 ──
const hostBusy = ref(false);
async function makeHost() {
  hostBusy.value = true;
  try {
    const info = await collab.hostStart();
    tab.value = info.needsBootstrap ? "bootstrap" : "login";
    authErr.value = "";
    toast.info(
      info.needsBootstrap
        ? `主机已在本机启动(端口 ${info.port}),注册你的管理者账号吧`
        : `主机已在本机启动(端口 ${info.port}),直接登录`
    );
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    hostBusy.value = false;
  }
}

// ── 桌面模式:主机地址(折叠进「高级设置」,不再拦路) ──
const hostInput = ref(collab.base);
const hostOpen = ref(collab.needsHost); // 没保存过才默认展开
function saveHost() {
  const v = hostInput.value.trim();
  if (!v) {
    toast.error("请填写主机地址,如 http://192.168.1.5:8080");
    return;
  }
  if (!/^https?:\/\//.test(v)) {
    toast.error("地址需以 http:// 或 https:// 开头");
    return;
  }
  collab.applyBase(v);
  toast.info("已保存主机地址");
  hostOpen.value = false;
}

// ── 登录 / 注册 为主,票据入伙收进第三个小 tab ──
type AuthTab = "login" | "signup" | "redeem" | "bootstrap";
const tab = ref<AuthTab>("login");
const busy = ref(false);
const authErr = ref("");
const f = ref({
  username: "",
  password: "",
  displayName: "",
  code: "",
  deviceName: "",
});

// 连接失败时自动展开高级设置提醒检查主机地址
watch(authErr, (e) => {
  if (isTauri && e.includes("无法连接")) hostOpen.value = true;
});

async function doAuth() {
  authErr.value = "";
  const v = f.value;
  if (!v.username.trim() || !v.password) {
    authErr.value = "用户名和密码不能为空";
    return;
  }
  if (tab.value !== "login" && !v.displayName.trim()) {
    authErr.value = "请填写显示昵称";
    return;
  }
  if (tab.value === "redeem" && !v.code.trim()) {
    authErr.value = "请填写邀请配对码";
    return;
  }
  // 桌面版没连主机就登录 → 请求会落到应用自己身上(返回 SPA 网页)→ 天书报错。
  // 先挡下并把「高级设置」展开;分享码自带主机地址,豁免。
  const shareOk = tab.value === "redeem" && !!parseShareCode(v.code);
  if (collab.needsHost && !shareOk) {
    authErr.value =
      "先点上方「把这台电脑设为主机」,或让主机管理者发你配对码;也可在「高级设置」手填主机地址";
    hostOpen.value = true;
    return;
  }
  busy.value = true;
  try {
    if (tab.value === "login") {
      await collab.login(v.username.trim(), v.password);
    } else if (tab.value === "signup") {
      await collab.signup(v.username.trim(), v.password, v.displayName.trim());
    } else if (tab.value === "bootstrap") {
      await collab.bootstrap(
        v.username.trim(),
        v.password,
        v.displayName.trim(),
        // 本机正在当主机 → 自报主机设备,设备页点亮「主机」徽标
        !!collab.hostInfo?.running
      );
    } else {
      await collab.redeem({
        code: v.code.trim(),
        username: v.username.trim(),
        password: v.password,
        displayName: v.displayName.trim(),
        deviceName: v.deviceName.trim() || "我的电脑",
      });
    }
    toast.info(`欢迎,${collab.user?.display_name || collab.user?.displayName || collab.user?.username}`);
  } catch (e) {
    const err = e as Error & { status?: number };
    if (tab.value === "signup" && err.status === 403) {
      authErr.value = "主机已关闭开放注册,请向管理者要一张邀请票据,从「票据入伙」加入";
    } else {
      authErr.value = err.message;
    }
  } finally {
    busy.value = false;
  }
}

async function doLogout() {
  if (!confirm("退出协作登录?")) return;
  await collab.logout();
}

// ── 已登录:主界面(GitHub 式:团队 → 项目 → 看板) ──
const rightPane = ref<"board" | "team" | "admin">("board");

// 团队切换器(下拉里带「新建团队」)
const NEW_TEAM = -1;
const teamSel = computed({
  get: () => collab.currentTeamId ?? NEW_TEAM,
  set: (v: number) => {
    if (v === NEW_TEAM) {
      showNewTeam.value = true;
      return;
    }
    void collab.selectTeam(v);
  },
});
const showNewTeam = ref(false);
const ntName = ref("");
const ntBusy = ref(false);
async function submitNewTeam() {
  const name = ntName.value.trim();
  if (!name) {
    toast.error("给团队起个名字");
    return;
  }
  ntBusy.value = true;
  try {
    await collab.createTeam(name);
    showNewTeam.value = false;
    ntName.value = "";
    rightPane.value = "team";
    toast.info(`团队「${name}」已创建,去拉人吧`);
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    ntBusy.value = false;
  }
}

// 新建项目(挂当前团队)
const showNewProject = ref(false);
const npName = ref("");
const npRepo = ref("");
const npBusy = ref(false);
async function submitNewProject() {
  if (!npName.value.trim() || !npRepo.value.trim()) {
    toast.error("项目名与仓库地址都要填");
    return;
  }
  npBusy.value = true;
  try {
    await collab.createProject(
      npName.value.trim(),
      npRepo.value.trim(),
      collab.currentTeamId
    );
    showNewProject.value = false;
    npName.value = "";
    npRepo.value = "";
    rightPane.value = "board";
    toast.info("项目已创建");
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    npBusy.value = false;
  }
}

function pickProject(id: number) {
  rightPane.value = "board";
  void collab.selectProject(id);
}

const displayName = computed(
  () =>
    collab.user?.display_name ||
    collab.user?.displayName ||
    collab.user?.username ||
    ""
);

const MAIN_TABS: { key: AuthTab; label: string }[] = [
  { key: "login", label: "登录" },
  { key: "signup", label: "注册" },
];
</script>

<template>
  <div class="collab">
    <!-- ── 未登录 ── -->
    <div v-if="!collab.authed" class="auth-wrap">
      <div class="auth-card">
        <div class="auth-brand">
          <span class="ab-ic"><Handshake :size="20" :stroke-width="1.7" color="#fff" /></span>
          <h1>多人协作</h1>
        </div>
        <p class="auth-lead">像 GitHub 一样协作:注册账号 → 建团队拉人 → 团队里建项目跑任务看板。</p>

        <!-- 桌面版:一键把本机变成协作主机(零配置;同事凭配对码加入) -->
        <div v-if="isTauri" class="host-cta">
          <template v-if="!collab.hostInfo?.running">
            <button class="btn solid wide" :disabled="hostBusy" @click="makeHost">
              <LoaderCircle v-if="hostBusy" :size="14" class="spin" />
              <Server v-else :size="14" :stroke-width="1.8" />
              把这台电脑设为主机
            </button>
            <p class="cta-tip">本机启动协作服务,注册管理者账号;同事凭一个配对码加入,谁都不用填地址。</p>
          </template>
          <p v-else class="cta-on">
            <Server :size="13" :stroke-width="2" /> 本机主机运行中 · 端口 {{ collab.hostInfo.port }}
          </p>
          <div class="cta-divider"><span>或者连接别人的主机</span></div>
        </div>

        <div class="tabs">
          <button
            v-for="t in MAIN_TABS"
            :key="t.key"
            class="tab"
            :class="{ active: tab === t.key }"
            @click="tab = t.key; authErr = ''"
          >
            {{ t.label }}
          </button>
          <button
            class="tab minor"
            :class="{ active: tab === 'redeem' }"
            @click="tab = 'redeem'; authErr = ''"
          >
            票据入伙
          </button>
        </div>
        <p class="tab-hint">
          {{
            tab === "login"
              ? "已有账号,直接登录"
              : tab === "signup"
                ? "开放注册:用户名 + 密码 + 昵称,注册成功直接进入"
                : tab === "redeem"
                  ? "拿到管理者发的配对码?在这里建号加入"
                  : "全新协作服务:创建第一个管理者账号(仅零账号时可用)"
          }}
        </p>

        <form class="auth-form" @submit.prevent="doAuth">
          <input v-if="tab === 'redeem'" v-model="f.code" class="inp code" placeholder="邀请配对码(整串粘贴,自动找到主机)" />
          <input v-model="f.username" class="inp" placeholder="用户名" autocomplete="username" />
          <input
            v-model="f.password"
            class="inp"
            type="password"
            placeholder="密码"
            :autocomplete="tab === 'login' ? 'current-password' : 'new-password'"
          />
          <input v-if="tab !== 'login'" v-model="f.displayName" class="inp" placeholder="显示昵称(同事看到的名字)" />
          <input v-if="tab === 'redeem'" v-model="f.deviceName" class="inp" placeholder="本机备注名(如「小王的笔记本」,可选)" />
          <div v-if="authErr" class="auth-err">{{ authErr }}</div>
          <button class="btn solid wide" type="submit" :disabled="busy">
            <LoaderCircle v-if="busy" :size="14" class="spin" />
            {{
              tab === "login"
                ? "登录"
                : tab === "signup"
                  ? "注册并进入"
                  : tab === "redeem"
                    ? "入伙并登录"
                    : "初始化并登录"
            }}
          </button>
        </form>

        <button
          v-if="tab !== 'bootstrap'"
          class="link-btn"
          @click="tab = 'bootstrap'; authErr = ''"
        >
          全新主机?首次初始化 →
        </button>

        <!-- 桌面模式:主机地址收进「高级设置」,保存过默认收起 -->
        <div v-if="isTauri" class="adv">
          <button class="adv-toggle" @click="hostOpen = !hostOpen">
            <ChevronDown :size="13" class="chev" :class="{ open: hostOpen }" />
            高级设置
            <span v-if="collab.base" class="adv-cur">{{ collab.base }}</span>
          </button>
          <div v-if="hostOpen" class="host-box">
            <label class="host-lb"><Plug :size="13" :stroke-width="1.8" /> 协作主机地址</label>
            <div class="host-row">
              <input
                v-model="hostInput"
                class="inp"
                placeholder="如 http://192.168.1.5:8080"
                @keyup.enter="saveHost"
              />
              <button class="btn ghost" type="button" @click="saveHost">保存</button>
            </div>
            <p class="host-tip">
              {{ collab.needsHost
                ? "桌面版需要先填团队协作主机(Docker 部署的那台)的地址。"
                : "已保存,登录时自动复用;连接失败时再来这里检查。" }}
            </p>
          </div>
        </div>
      </div>
    </div>

    <!-- ── 已登录 ── -->
    <div v-else class="main">
      <!-- 左栏:团队切换 → 项目 → 成员 -->
      <aside class="left">
        <div class="me">
          <span class="me-name">{{ displayName }}</span>
          <span
            v-if="tunnel?.running"
            class="tun-badge"
            :class="tunnel.state || 'connected'"
            :title="
              (TUNNEL_LABEL[tunnel.state || ''] || '隧道') +
              (tunnel.latency_ms != null ? ` · ${tunnel.latency_ms}ms` : '') +
              (tunnel.last_error ? ` · ${tunnel.last_error}` : '')
            "
          >
            <Plug :size="10" :stroke-width="2" />
            {{ tunnel.latency_ms != null ? `${tunnel.latency_ms}ms` : (TUNNEL_LABEL[tunnel.state || ""] || "P2P") }}
          </span>
          <span class="me-role" :class="{ owner: collab.isOwner }">
            <Crown v-if="collab.isOwner" :size="11" :stroke-width="2" />
            {{ collab.isOwner ? "管理者" : "成员" }}
          </span>
          <button class="icon-btn" title="退出登录" @click="doLogout"><LogOut :size="14" /></button>
        </div>

        <!-- 团队切换器 -->
        <div class="team-switch">
          <UsersRound :size="14" :stroke-width="1.8" class="ts-ic" />
          <select v-if="collab.teams.length" v-model="teamSel" class="ts-sel">
            <option v-for="t in collab.teams" :key="t.id" :value="t.id">
              {{ t.name }}{{ t.my_role === "owner" ? " ♛" : "" }}
            </option>
            <option :value="NEW_TEAM">＋ 新建团队…</option>
          </select>
          <button v-else class="ts-new" @click="showNewTeam = true">
            <Plus :size="13" :stroke-width="2" /> 新建团队
          </button>
        </div>

        <!-- 没有团队:引导卡 -->
        <div v-if="!collab.teams.length" class="guide">
          <p>创建一个团队,像 GitHub 组织一样管理成员和项目。</p>
          <button class="btn solid sm" @click="showNewTeam = true">
            <Plus :size="13" :stroke-width="2" /> 创建团队
          </button>
        </div>

        <!-- 当前团队的项目 -->
        <template v-if="collab.currentTeamId != null">
          <div class="sec-head">
            <FolderGit2 :size="13" :stroke-width="1.8" /> 项目
            <button
              v-if="collab.canManage"
              class="icon-btn add"
              title="在当前团队新建项目"
              @click="showNewProject = true"
            >
              <Plus :size="14" :stroke-width="2" />
            </button>
          </div>
          <div class="proj-list">
            <button
              v-for="p in collab.teamProjects"
              :key="p.id"
              class="proj"
              :class="{ active: p.id === collab.currentProjectId }"
              @click="pickProject(p.id)"
            >
              <span class="proj-name">{{ p.name }}</span>
              <span class="proj-repo">{{ p.repo }}</span>
            </button>
            <div v-if="!collab.teamProjects.length" class="empty">
              团队还没有项目{{ collab.canManage ? ",点上方 + 创建" : ",等团队管理者创建" }}
            </div>
          </div>
        </template>

        <!-- 兼容旧独立项目 -->
        <template v-if="collab.ungroupedProjects.length">
          <div class="sec-head"><FolderGit2 :size="13" :stroke-width="1.8" /> 未分组</div>
          <div class="proj-list">
            <button
              v-for="p in collab.ungroupedProjects"
              :key="p.id"
              class="proj"
              :class="{ active: p.id === collab.currentProjectId }"
              @click="pickProject(p.id)"
            >
              <span class="proj-name">{{ p.name }}</span>
              <span class="proj-repo">{{ p.repo }}</span>
            </button>
          </div>
        </template>

        <!-- 当前项目成员(简表) -->
        <div class="sec-head"><Users :size="13" :stroke-width="1.8" /> 项目成员</div>
        <div class="member-list">
          <div v-for="m in collab.members" :key="m.user_id" class="member">
            <span class="m-name">{{ m.display_name || m.username || m.user_id }}</span>
            <span class="m-role">{{ m.role }}</span>
          </div>
          <div v-if="!collab.members.length" class="empty">当前项目暂无成员</div>
        </div>

        <!-- 主 Agent 设置卡(项目/团队 owner 可见) -->
        <LeadAgentCard v-if="collab.canManage && collab.currentProjectId" />

        <!-- 底部导航 -->
        <div class="left-foot">
          <button
            class="foot-btn"
            :class="{ active: rightPane === 'board' }"
            @click="rightPane = 'board'"
          >
            <Kanban :size="13" :stroke-width="1.8" /> 看板
          </button>
          <button
            v-if="collab.currentTeamId != null"
            class="foot-btn"
            :class="{ active: rightPane === 'team' }"
            @click="rightPane = 'team'"
          >
            <UsersRound :size="13" :stroke-width="1.8" /> 成员
          </button>
          <button
            v-if="collab.isOwner"
            class="foot-btn"
            :class="{ active: rightPane === 'admin' }"
            @click="rightPane = 'admin'"
          >
            <ShieldCheck :size="13" :stroke-width="1.8" /> 主机管理
          </button>
        </div>
      </aside>

      <!-- 右侧 -->
      <section class="right">
        <CollabAdmin v-if="rightPane === 'admin' && collab.isOwner" />
        <TeamMembers v-else-if="rightPane === 'team' && collab.currentTeamId != null" />
        <TaskBoard v-else-if="collab.currentProjectId" />
        <div v-else class="board-empty">
          <Handshake :size="34" :stroke-width="1.4" />
          <p v-if="!collab.teams.length">
            创建一个团队,像 GitHub 组织一样管理成员和项目;再在团队里建项目、发任务卡。
          </p>
          <p v-else>
            {{ collab.canManage
              ? "团队里还没有项目,在左侧点 + 创建一个,再建任务卡分派给成员。"
              : "团队里还没有项目,等团队管理者创建后即可开工。" }}
          </p>
          <button v-if="!collab.teams.length" class="btn solid" @click="showNewTeam = true">
            <Plus :size="14" :stroke-width="2" /> 创建团队
          </button>
        </div>
      </section>
    </div>

    <!-- 新建团队对话框 -->
    <div v-if="showNewTeam" class="mask" @click.self="showNewTeam = false">
      <div class="dialog">
        <div class="dlg-head">
          <span>新建团队</span>
          <button class="icon-btn" @click="showNewTeam = false"><X :size="16" /></button>
        </div>
        <p class="dlg-tip">团队 = GitHub 组织:成员和项目都挂在团队下,你是创建者即团队管理者。</p>
        <label class="fld"><span>团队名 *</span>
          <input v-model="ntName" class="inp" placeholder="如「前端组」「北极星小队」" @keyup.enter="submitNewTeam" />
        </label>
        <div class="dlg-act">
          <button class="btn ghost" @click="showNewTeam = false">取消</button>
          <button class="btn solid" :disabled="ntBusy" @click="submitNewTeam">
            <LoaderCircle v-if="ntBusy" :size="13" class="spin" /> 创建
          </button>
        </div>
      </div>
    </div>

    <!-- 新建项目对话框(挂当前团队) -->
    <div v-if="showNewProject" class="mask" @click.self="showNewProject = false">
      <div class="dialog">
        <div class="dlg-head">
          <span>新建协作项目</span>
          <button class="icon-btn" @click="showNewProject = false"><X :size="16" /></button>
        </div>
        <p v-if="collab.currentTeam" class="dlg-tip">将创建在团队「{{ collab.currentTeam.name }}」下。</p>
        <label class="fld"><span>项目名 *</span><input v-model="npName" class="inp" placeholder="如「官网重构」" /></label>
        <label class="fld"><span>仓库地址 *</span><input v-model="npRepo" class="inp" placeholder="如 git@github.com:team/site.git" /></label>
        <div class="dlg-act">
          <button class="btn ghost" @click="showNewProject = false">取消</button>
          <button class="btn solid" :disabled="npBusy" @click="submitNewProject">
            <LoaderCircle v-if="npBusy" :size="13" class="spin" /> 创建
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.collab { flex: 1; display: flex; min-height: 0; background: var(--bg); }

/* ── 登录页 ── */
.auth-wrap { flex: 1; display: flex; align-items: center; justify-content: center; padding: 30px; overflow-y: auto; }
.auth-card {
  width: min(420px, 94vw);
  border: 1px solid var(--border-soft); border-radius: 16px;
  background: var(--panel); padding: 28px 30px;
  box-shadow: var(--shadow);
}
.auth-brand { display: flex; align-items: center; gap: 11px; }
.ab-ic {
  width: 38px; height: 38px; border-radius: 11px;
  display: inline-flex; align-items: center; justify-content: center;
  background: linear-gradient(135deg, #2f6fed, #6aa4ff);
}
.auth-brand h1 { margin: 0; font-family: var(--serif); font-size: 20px; font-weight: 600; letter-spacing: 3px; color: var(--ink); }
.auth-lead { margin: 12px 0 18px; font-size: 12.5px; line-height: 1.8; color: var(--text-2); }

/* 一键当主机 CTA */
.host-cta { margin-bottom: 14px; }
.host-cta .btn { display: inline-flex; align-items: center; justify-content: center; gap: 6px; }
.cta-tip { font-size: 12px; color: var(--muted); margin: 8px 0 0; line-height: 1.6; }
.cta-on {
  display: inline-flex; align-items: center; gap: 6px; margin: 0;
  font-size: 12.5px; font-weight: 600; color: var(--primary, var(--ink));
}
.cta-divider {
  display: flex; align-items: center; gap: 10px; margin-top: 14px;
  color: var(--muted); font-size: 11px;
}
.cta-divider::before, .cta-divider::after {
  content: ""; flex: 1; height: 1px; background: var(--border-soft, currentColor); opacity: .6;
}

.tabs { display: flex; gap: 4px; border-bottom: 1px solid var(--border-soft); }
.tab {
  border: none; background: none; cursor: pointer;
  font-size: 13px; color: var(--muted); padding: 8px 12px;
  border-bottom: 2px solid transparent; margin-bottom: -1px;
}
.tab:hover { color: var(--ink); }
.tab.active { color: var(--ink); font-weight: 600; border-bottom-color: var(--ink); }
.tab.minor { margin-left: auto; font-size: 11.5px; }
.tab-hint { margin: 10px 0 12px; font-size: 11.5px; color: var(--muted); line-height: 1.7; }

.auth-form { display: flex; flex-direction: column; gap: 10px; }
.inp {
  border: 1px solid var(--border); border-radius: 8px;
  background: var(--bg); color: var(--ink);
  font-size: 13px; padding: 9px 11px; font-family: inherit;
}
.inp:focus { outline: none; border-color: var(--primary, var(--ink)); }
.inp.code { font-family: var(--mono); letter-spacing: 2px; }
.auth-err { color: var(--vermilion); font-size: 12px; line-height: 1.7; }

.link-btn {
  margin-top: 12px; border: none; background: none; cursor: pointer;
  font-size: 11.5px; color: var(--muted); padding: 0;
}
.link-btn:hover { color: var(--ink); text-decoration: underline; }

.adv { margin-top: 16px; border-top: 1px dashed var(--border-soft); padding-top: 10px; }
.adv-toggle {
  display: flex; align-items: center; gap: 5px; width: 100%;
  border: none; background: none; cursor: pointer; padding: 4px 0;
  font-size: 11.5px; color: var(--muted);
}
.adv-toggle:hover { color: var(--ink); }
.adv-cur { margin-left: auto; font-family: var(--mono); font-size: 10.5px; color: var(--dim); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 55%; }
.chev { transition: transform 0.15s; }
.chev.open { transform: rotate(180deg); }
.host-box { margin-top: 8px; padding: 12px; border: 1px dashed var(--border); border-radius: 10px; }
.host-lb { display: inline-flex; align-items: center; gap: 5px; font-size: 12px; color: var(--text-2); margin-bottom: 7px; }
.host-row { display: flex; gap: 7px; }
.host-row .inp { flex: 1; min-width: 0; }
.host-tip { margin: 8px 0 0; font-size: 11.5px; color: var(--muted); line-height: 1.7; }

.btn {
  display: inline-flex; align-items: center; justify-content: center; gap: 6px;
  border: none; cursor: pointer;
  font-size: 13px; padding: 9px 16px; border-radius: 8px;
}
.btn:disabled { opacity: 0.55; cursor: not-allowed; }
.btn.solid { background: var(--btn-solid-bg); color: var(--btn-solid-text); }
.btn.solid:hover:not(:disabled) { background: var(--primary); }
.btn.ghost { background: transparent; color: var(--text-2); border: 1px solid var(--border); }
.btn.ghost:hover { color: var(--ink); border-color: var(--ink); }
.btn.wide { width: 100%; margin-top: 4px; letter-spacing: 2px; }
.btn.sm { font-size: 12px; padding: 6px 12px; }

/* ── 主界面 ── */
.main { flex: 1; display: flex; min-height: 0; min-width: 0; }
.left {
  width: 230px; flex-shrink: 0;
  border-right: 1px solid var(--border-soft);
  background: var(--bg-soft, var(--panel));
  display: flex; flex-direction: column; min-height: 0;
}
.me {
  display: flex; align-items: center; gap: 7px;
  padding: 13px 13px 11px;
  border-bottom: 1px solid var(--border-soft);
}
.me-name { font-size: 13px; font-weight: 600; color: var(--ink); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.me-role {
  display: inline-flex; align-items: center; gap: 3px;
  font-size: 10.5px; color: var(--muted);
  background: var(--selection-bg); padding: 2px 7px; border-radius: 20px; flex-shrink: 0;
}
.me-role.owner { color: #b8860b; background: color-mix(in srgb, #b8860b 12%, transparent); }
.tun-badge {
  display: inline-flex; align-items: center; gap: 3px;
  font-size: 10.5px; padding: 2px 7px; border-radius: 20px; flex-shrink: 0;
  color: #15803d; background: color-mix(in srgb, #22c55e 14%, transparent);
}
.tun-badge.connecting, .tun-badge.reconnecting {
  color: #b45309; background: color-mix(in srgb, #f59e0b 14%, transparent);
}
.tun-badge.stopped { color: var(--muted); background: var(--selection-bg); }
.me .icon-btn { margin-left: auto; }
.icon-btn {
  border: none; background: none; color: var(--muted); cursor: pointer;
  display: inline-flex; padding: 4px; border-radius: 6px;
}
.icon-btn:hover { color: var(--ink); background: var(--selection-bg); }

/* 团队切换器 */
.team-switch {
  display: flex; align-items: center; gap: 7px;
  padding: 10px 13px; border-bottom: 1px solid var(--border-soft);
}
.ts-ic { color: var(--muted); flex-shrink: 0; }
.ts-sel {
  flex: 1; min-width: 0;
  border: 1px solid var(--border); border-radius: 8px;
  background: var(--bg); color: var(--ink);
  font-size: 12.5px; font-weight: 600; padding: 6px 8px; cursor: pointer;
}
.ts-new {
  flex: 1;
  display: inline-flex; align-items: center; justify-content: center; gap: 5px;
  border: 1px dashed var(--border); background: transparent;
  color: var(--text-2); cursor: pointer;
  font-size: 12px; padding: 6px; border-radius: 8px;
}
.ts-new:hover { color: var(--ink); border-color: var(--ink); }

.guide { margin: 12px 10px; padding: 12px; border: 1px dashed var(--border); border-radius: 10px; }
.guide p { margin: 0 0 10px; font-size: 12px; color: var(--text-2); line-height: 1.8; }

.sec-head {
  display: flex; align-items: center; gap: 6px;
  padding: 12px 13px 6px;
  font-size: 11.5px; font-weight: 600; color: var(--muted); letter-spacing: 1px;
}
.sec-head .add { margin-left: auto; }
.proj-list { padding: 0 8px; display: flex; flex-direction: column; gap: 3px; max-height: 32%; overflow-y: auto; }
.proj {
  text-align: left; border: none; cursor: pointer;
  background: transparent; border-radius: 8px; padding: 7px 9px;
  display: flex; flex-direction: column; gap: 2px;
}
.proj:hover { background: var(--selection-bg); }
.proj.active { background: var(--selection-bg); }
.proj-name { font-size: 12.5px; font-weight: 600; color: var(--ink); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.proj.active .proj-name { color: var(--primary, var(--ink)); }
.proj-repo { font-size: 10.5px; color: var(--dim); font-family: var(--mono); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.member-list { padding: 0 8px; flex: 1; overflow-y: auto; }
.member { display: flex; align-items: center; gap: 7px; padding: 5px 9px; }
.m-name { font-size: 12px; color: var(--text); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.m-role { margin-left: auto; font-size: 10.5px; color: var(--dim); }
.empty { font-size: 11.5px; color: var(--dim); font-style: italic; padding: 8px 9px; line-height: 1.7; }

.left-foot { display: flex; gap: 6px; padding: 10px; border-top: 1px solid var(--border-soft); }
.foot-btn {
  flex: 1;
  display: inline-flex; align-items: center; justify-content: center; gap: 5px;
  border: 1px solid var(--border-soft); background: transparent;
  color: var(--text-2); cursor: pointer;
  font-size: 11.5px; padding: 6px 4px; border-radius: 8px;
}
.foot-btn:hover { color: var(--ink); border-color: var(--border); }
.foot-btn.active { background: var(--selection-bg); color: var(--ink); border-color: var(--border); }

.right { flex: 1; display: flex; flex-direction: column; min-width: 0; min-height: 0; }
.board-empty {
  flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 14px;
  color: var(--dim);
}
.board-empty p { margin: 0; font-size: 13px; color: var(--muted); max-width: 340px; text-align: center; line-height: 1.8; }

/* 对话框 */
.mask {
  position: fixed; inset: 0; z-index: 60;
  background: rgba(0,0,0,0.35);
  display: flex; align-items: center; justify-content: center;
}
.dialog {
  width: min(440px, 92vw);
  background: var(--panel); border: 1px solid var(--border-soft);
  border-radius: 14px; padding: 18px 20px;
  box-shadow: var(--shadow-lg, var(--shadow));
}
.dlg-head { display: flex; align-items: center; justify-content: space-between; font-size: 15px; font-weight: 600; color: var(--ink); font-family: var(--serif); letter-spacing: 1px; margin-bottom: 12px; }
.dlg-tip { margin: 0 0 12px; font-size: 12px; color: var(--text-2); line-height: 1.7; }
.fld { display: flex; flex-direction: column; gap: 5px; margin-bottom: 12px; }
.fld span { font-size: 12px; color: var(--text-2); }
.dlg-act { display: flex; justify-content: flex-end; gap: 8px; }
.spin { animation: spin 0.9s linear infinite; }
@keyframes spin { to { transform: rotate(360deg); } }
</style>
