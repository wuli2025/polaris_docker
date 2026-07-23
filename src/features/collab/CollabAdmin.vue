<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import {
  TicketPlus,
  Copy,
  LoaderCircle,
  RefreshCw,
  Server,
  Users,
  UserPlus,
  MonitorSmartphone,
  ShieldOff,
} from "@lucide/vue";
import {
  collabApi,
  fmtTime,
  type AccountInfo,
  type AdminDevice,
  type AdminUser,
  type Ticket,
} from "./api";
import { isTauri } from "../../tauri";
import { useCollabStore } from "./stores/collab";
import { toast } from "../../composables/useToast";

const collab = useCollabStore();

// ── 邀请票据 ──
const ticketRole = ref("member");
const ticketNote = ref("");
const ticket = ref<Ticket | null>(null);
const issuing = ref(false);
async function issueTicket() {
  issuing.value = true;
  try {
    ticket.value = await collabApi.adminTicket({
      role: ticketRole.value,
      note: ticketNote.value.trim(),
    });
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    issuing.value = false;
  }
}
async function copyCode() {
  if (!ticket.value) return;
  try {
    // 优先复制分享码(带主机地址,对方零填写);老主机没有 share 字段则退回裸码。
    await navigator.clipboard.writeText(ticket.value.share || ticket.value.code);
    toast.info("配对码已复制,发给要入伙的同事吧");
  } catch {
    toast.error("复制失败,请手动选中复制");
  }
}

// ── 本机主机(桌面版一键当主机的管理卡) ──
async function stopHost() {
  if (!confirm("停止本机协作主机?同事将立即连不上,下次启动 App 也不再自动开启。")) return;
  try {
    await collab.hostStop();
    toast.info("主机已停止");
  } catch (e) {
    toast.error((e as Error).message);
  }
}

// ── 账号体系自述:决定这台机器上能做多少账号操作 ──
// authority(账号中心)/ local(老的本机账号)= 密码邮箱都归本机管,全套能改;
// delegated(成员主机)= 这里的账号行只是本机成员资格,密码邮箱在云端,只能改角色/停用/移除。
const acct = ref<AccountInfo | null>(null);
const isDelegated = computed(() => acct.value?.mode === "delegated");
const isAuthority = computed(() => acct.value?.mode === "authority");
/** 这一行是不是「云端账号」:密码邮箱归账号中心管,本机改不动(本机应急账号不算) */
function isCloud(u: AdminUser) {
  return isDelegated.value && !!u.uid;
}

// ── 用户 ──
const users = ref<AdminUser[]>([]);
const usersLoading = ref(false);
async function loadUsers() {
  usersLoading.value = true;
  try {
    users.value = await collabApi.adminUsers();
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    usersLoading.value = false;
  }
}

// ── 建号(远程开户:不必再 SSH 进服务器跑脚本)──
const nu = ref({ username: "", password: "", displayName: "", email: "", role: "collaborator" });
const creating = ref(false);
async function createAccount() {
  if (nu.value.username.trim().length < 3) return toast.error("用户名至少 3 个字符");
  if (nu.value.password.length < 8) return toast.error("密码至少 8 位");
  creating.value = true;
  try {
    const r = await collabApi.adminAccountCreate({
      username: nu.value.username.trim(),
      password: nu.value.password,
      displayName: nu.value.displayName.trim(),
      email: nu.value.email.trim(),
      role: nu.value.role,
    });
    toast.info(
      r.uid
        ? `已建全局账号「${nu.value.username.trim()}」,他可以在任何一台主机上登录`
        : `已建本机账号「${nu.value.username.trim()}」`,
    );
    nu.value = { username: "", password: "", displayName: "", email: "", role: "collaborator" };
    await loadUsers();
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    creating.value = false;
  }
}

// ── 行内编辑(昵称/邮箱/角色)──
const editingId = ref<number | null>(null);
const draft = ref({ displayName: "", email: "", role: "" });
const saving = ref(false);
function startEdit(u: AdminUser) {
  editingId.value = u.id;
  draft.value = { displayName: u.display_name || "", email: u.email || "", role: u.role };
}
async function saveEdit(u: AdminUser) {
  saving.value = true;
  try {
    const args: { userId: number; displayName?: string; email?: string; role?: string } = {
      userId: u.id,
    };
    // 只提交真正改过的字段:后端「字段缺席 = 不动这一项」,顺带避开成员主机上改邮箱的 403。
    if (draft.value.displayName.trim() !== (u.display_name || "")) {
      args.displayName = draft.value.displayName.trim();
    }
    if (!isCloud(u) && draft.value.email.trim() !== (u.email || "")) {
      args.email = draft.value.email.trim();
    }
    if (draft.value.role !== u.role) args.role = draft.value.role;
    if (Object.keys(args).length === 1) {
      editingId.value = null;
      return;
    }
    await collabApi.adminAccountUpdate(args);
    editingId.value = null;
    toast.info("已保存");
    await loadUsers();
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    saving.value = false;
  }
}
async function removeAccount(u: AdminUser) {
  const what = isDelegated.value
    ? `把「${u.username}」移出本机?他在云端的账号不受影响,但将无法再访问这台主机。`
    : `永久删除账号「${u.username}」?其会话、设备、项目成员关系一并清除,不可撤销。`;
  if (!confirm(what)) return;
  try {
    await collabApi.adminAccountDelete(u.id);
    toast.info(isDelegated.value ? "已移出本机" : "账号已删除");
    await loadUsers();
  } catch (e) {
    toast.error((e as Error).message);
  }
}
/** 老账号(v2.5.0 之前建的)没有全局 uid,登不了别的主机,给它补签一个 */
async function backfillUid(u: AdminUser) {
  try {
    const r = await collabApi.adminAccountUidBackfill(u.id);
    u.uid = r.uid;
    toast.info(`已给「${u.username}」补签全局身份,现在他能在所有主机上登录了`);
  } catch (e) {
    toast.error((e as Error).message);
  }
}
async function toggleUser(u: AdminUser) {
  try {
    await collabApi.adminUserDisable(u.id, !u.disabled);
    u.disabled = !u.disabled;
    toast.info(`已${u.disabled ? "停用" : "启用"}「${u.username}」`);
  } catch (e) {
    toast.error((e as Error).message);
  }
}
/** owner 兜底改密:成员没绑邮箱/邮件服务没配时,当面给一把新密码 */
async function resetPassword(u: AdminUser) {
  const pw = prompt(`给「${u.username}」设一个新密码(至少 8 位)。\n改完对方所有旧登录会被踢下线。`);
  if (pw == null) return;
  if (pw.length < 8) {
    toast.error("新密码至少 8 位");
    return;
  }
  try {
    await collabApi.adminUserResetPassword(u.id, pw);
    toast.info(`「${u.username}」的密码已重置,把新密码告诉对方吧`);
  } catch (e) {
    toast.error((e as Error).message);
  }
}

// ── 设备 ──
const devices = ref<AdminDevice[]>([]);
const devicesLoading = ref(false);
async function loadDevices() {
  devicesLoading.value = true;
  try {
    devices.value = await collabApi.adminDevices();
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    devicesLoading.value = false;
  }
}
async function revoke(d: AdminDevice) {
  if (!confirm(`吊销设备「${d.name || d.id}」?该设备将无法再访问协作服务。`))
    return;
  try {
    await collabApi.adminDeviceRevoke(d.id);
    toast.info("已吊销");
    await loadDevices();
  } catch (e) {
    toast.error((e as Error).message);
  }
}

onMounted(async () => {
  try {
    acct.value = await collabApi.accountInfo();
  } catch {
    // 老主机没有 /api/account/info:当本地权威处理(全套可改),与升级前行为一致
  }
  void loadUsers();
  void loadDevices();
  if (isTauri) void collab.hostStatus();
});
</script>

<template>
  <div class="admin">
    <!-- 本机主机(桌面版在当主机时显示) -->
    <section v-if="isTauri && collab.hostInfo?.running" class="card">
      <h3><Server :size="15" :stroke-width="1.8" /> 本机主机</h3>
      <div class="row">
        <span class="dim">端口 {{ collab.hostInfo.port }}</span>
        <span v-for="u in collab.hostInfo.urls" :key="u" class="mono dim">{{ u }}</span>
        <button class="btn danger sm" style="margin-left:auto" @click="stopHost">停止主机</button>
      </div>
      <p class="tip" style="margin:8px 0 0">协作数据都存在这台机器上;它关机或停止主机,同事就连不上了。</p>
    </section>

    <!-- 邀请票据 -->
    <section class="card">
      <h3><TicketPlus :size="15" :stroke-width="1.8" /> 生成邀请票据</h3>
      <p class="tip">生成一次性配对码,同事在登录页「票据入伙」里输入即可加入团队。</p>
      <div class="row">
        <label class="lb">角色</label>
        <select v-model="ticketRole" class="sel">
          <option value="member">成员(member)</option>
          <option value="owner">管理者(owner)</option>
        </select>
        <input v-model="ticketNote" class="inp" placeholder="备注(给谁用,可选)" />
        <button class="btn solid" :disabled="issuing" @click="issueTicket">
          <LoaderCircle v-if="issuing" :size="13" class="spin" /> 生成
        </button>
      </div>
      <div v-if="ticket" class="ticket">
        <div class="tk-code">{{ ticket.code }}</div>
        <div v-if="ticket.share" class="tk-share">{{ ticket.share }}</div>
        <div class="tk-meta">
          <span>角色:{{ ticket.role }}</span>
          <span>有效期至:{{ fmtTime(ticket.expires_at) }}</span>
          <button class="btn ghost sm" @click="copyCode"><Copy :size="12" /> 复制配对码</button>
        </div>
        <p v-if="ticket.share" class="tip" style="margin:6px 0 0">
          复制的是整串配对码(含主机地址)——同事粘贴进「票据入伙」即可,不用填任何地址。
        </p>
      </div>
    </section>

    <!-- 新建账号(远程开户) -->
    <section v-if="!isDelegated" class="card">
      <h3><UserPlus :size="15" :stroke-width="1.8" /> 新建账号</h3>
      <p class="tip">
        {{
          isAuthority
            ? "这台机器是账号中心:在这儿建的账号会拿到全局身份,同一套用户名密码可以登任何一台主机。"
            : "直接给同事开一个本机账号(不必让对方走票据入伙)。"
        }}
      </p>
      <div class="row">
        <input v-model="nu.username" class="inp" placeholder="用户名(3–32 位,字母数字 _ . -)" />
        <input v-model="nu.password" class="inp" type="password" placeholder="初始密码(至少 8 位)" />
      </div>
      <div class="row" style="margin-top:8px">
        <input v-model="nu.displayName" class="inp" placeholder="昵称(可选,默认同用户名)" />
        <input v-model="nu.email" class="inp" placeholder="邮箱(可选,绑了才能自助找回密码)" />
        <select v-model="nu.role" class="sel">
          <option value="collaborator">成员(collaborator)</option>
          <option value="visitor">访客(visitor)</option>
          <option value="owner">管理者(owner)</option>
        </select>
        <button class="btn solid" :disabled="creating" @click="createAccount">
          <LoaderCircle v-if="creating" :size="13" class="spin" /> 建账号
        </button>
      </div>
      <p class="tip" style="margin:10px 0 0">
        管理员开户不发验证码,邮箱由你填对即可;把初始密码当面给对方,让他登录后自行改密。
      </p>
    </section>

    <!-- 用户列表 -->
    <section class="card">
      <h3>
        <Users :size="15" :stroke-width="1.8" /> 用户
        <button class="refresh" title="刷新" @click="loadUsers"><RefreshCw :size="13" /></button>
      </h3>
      <p v-if="isDelegated" class="tip">
        账号在云端账号中心({{ acct?.authorityUrl }})管理:这里只能改本机角色、停用或移出本机;
        改密码/邮箱请到账号中心。
      </p>
      <div v-if="usersLoading" class="dim"><LoaderCircle :size="13" class="spin" /> 加载中…</div>
      <div v-else-if="!users.length" class="dim">还没有其他用户,先建个账号或生成票据邀请同事吧</div>
      <table v-else class="tbl">
        <thead>
          <tr><th>用户名</th><th>昵称</th><th>邮箱</th><th>角色</th><th>状态</th><th></th></tr>
        </thead>
        <tbody>
          <tr v-for="u in users" :key="u.id" :class="{ off: u.disabled }">
            <td>
              {{ u.username }}
              <span
                v-if="isAuthority && !u.uid"
                class="badge-warn"
                title="没有全局身份,登不了其他主机 —— 点右侧「补签身份」"
                >本机限定</span
              >
            </td>
            <template v-if="editingId === u.id">
              <td><input v-model="draft.displayName" class="inp cell" placeholder="昵称" /></td>
              <td>
                <input
                  v-model="draft.email"
                  class="inp cell"
                  :disabled="isCloud(u)"
                  :title="isCloud(u) ? '云端账号的邮箱请到账号中心改' : ''"
                  placeholder="邮箱(留空=解绑)"
                />
              </td>
              <td>
                <select v-model="draft.role" class="sel">
                  <option value="collaborator">collaborator</option>
                  <option value="visitor">visitor</option>
                  <option value="owner">owner</option>
                </select>
              </td>
              <td></td>
              <td class="ta-r">
                <button class="btn solid sm" :disabled="saving" @click="saveEdit(u)">保存</button>
                <button class="btn ghost sm" @click="editingId = null">取消</button>
              </td>
            </template>
            <template v-else>
              <td>{{ u.display_name || "—" }}</td>
              <td :title="u.email || '未绑定邮箱,不能自助找回密码'">{{ u.email || "未绑定" }}</td>
              <td>{{ u.role }}</td>
              <td>
                <span class="dot" :class="{ ok: !u.disabled }"></span>
                {{ u.disabled ? "已停用" : "正常" }}
              </td>
              <td class="ta-r">
                <button class="btn ghost sm" @click="startEdit(u)">编辑</button>
                <button
                  v-if="isAuthority && !u.uid"
                  class="btn ghost sm"
                  @click="backfillUid(u)"
                >
                  补签身份
                </button>
                <button v-if="!isCloud(u)" class="btn ghost sm" @click="resetPassword(u)">
                  重置密码
                </button>
                <button class="btn ghost sm" @click="toggleUser(u)">
                  {{ u.disabled ? "启用" : "停用" }}
                </button>
                <button class="btn danger sm" @click="removeAccount(u)">
                  {{ isDelegated ? "移出" : "删除" }}
                </button>
              </td>
            </template>
          </tr>
        </tbody>
      </table>
    </section>

    <!-- 设备白名单 -->
    <section class="card">
      <h3>
        <MonitorSmartphone :size="15" :stroke-width="1.8" /> 设备白名单
        <button class="refresh" title="刷新" @click="loadDevices"><RefreshCw :size="13" /></button>
      </h3>
      <div v-if="devicesLoading" class="dim"><LoaderCircle :size="13" class="spin" /> 加载中…</div>
      <div v-else-if="!devices.length" class="dim">暂无已登记设备</div>
      <table v-else class="tbl">
        <thead>
          <tr><th>设备</th><th>用户</th><th>节点</th><th></th></tr>
        </thead>
        <tbody>
          <tr v-for="d in devices" :key="d.id" :class="{ off: d.revoked }">
            <td>
              <span v-if="d.is_host" class="badge-host">主机</span>
              {{ d.name || d.node_id || d.id }}
            </td>
            <td>{{ d.username || `#${d.user_id}` }}</td>
            <td class="mono">{{ d.node_id || "—" }}</td>
            <td class="ta-r">
              <span v-if="d.revoked" class="dim">已吊销</span>
              <button v-else class="btn danger sm" @click="revoke(d)">
                <ShieldOff :size="12" /> 吊销
              </button>
            </td>
          </tr>
        </tbody>
      </table>
    </section>
  </div>
</template>

<style scoped>
.admin { flex: 1; overflow-y: auto; padding: 16px; display: flex; flex-direction: column; gap: 14px; }
.card {
  border: 1px solid var(--border-soft); border-radius: 12px;
  background: var(--panel); padding: 16px 18px;
}
.card h3 {
  display: flex; align-items: center; gap: 7px;
  margin: 0 0 8px; font-size: 13.5px; font-weight: 600;
  color: var(--ink); letter-spacing: 1px;
}
.refresh { margin-left: auto; border: none; background: none; color: var(--muted); cursor: pointer; display: inline-flex; padding: 4px; border-radius: 6px; }
.refresh:hover { color: var(--ink); background: var(--selection-bg); }
.tip { margin: 0 0 12px; font-size: 12px; color: var(--text-2); line-height: 1.7; }
.row { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
.lb { font-size: 12px; color: var(--text-2); }
.sel, .inp {
  border: 1px solid var(--border); border-radius: 8px;
  background: var(--bg); color: var(--ink);
  font-size: 12.5px; padding: 7px 10px;
}
.inp { flex: 1; min-width: 140px; }
.sel { cursor: pointer; }
.btn {
  display: inline-flex; align-items: center; gap: 5px;
  border: none; cursor: pointer;
  font-size: 12.5px; padding: 7px 13px; border-radius: 8px;
}
.btn:disabled { opacity: 0.55; cursor: not-allowed; }
.btn.solid { background: var(--btn-solid-bg); color: var(--btn-solid-text); }
.btn.solid:hover:not(:disabled) { background: var(--primary); }
.btn.ghost { background: transparent; color: var(--text-2); border: 1px solid var(--border); }
.btn.ghost:hover { color: var(--ink); border-color: var(--ink); }
.btn.danger { background: transparent; color: var(--vermilion); border: 1px solid var(--border); }
.btn.danger:hover { border-color: var(--vermilion); }
.btn.sm { padding: 4px 10px; font-size: 11.5px; }

.ticket {
  margin-top: 14px; padding: 16px;
  border: 1px dashed var(--border); border-radius: 12px;
  background: var(--bg-soft, var(--selection-bg));
  text-align: center;
}
.tk-code {
  font-family: var(--mono); font-size: 28px; font-weight: 700;
  letter-spacing: 5px; color: var(--ink);
  word-break: break-all; user-select: all;
}
.tk-meta { margin-top: 10px; display: flex; flex-wrap: wrap; justify-content: center; align-items: center; gap: 12px; font-size: 11.5px; color: var(--muted); }
.tk-share {
  margin-top: 8px; font-family: var(--mono); font-size: 11px; line-height: 1.6;
  color: var(--muted); word-break: break-all; user-select: all;
}
.inp.cell { min-width: 110px; width: 100%; padding: 5px 8px; font-size: 12px; }
.badge-warn {
  font-size: 10px; font-weight: 700; color: var(--vermilion);
  background: color-mix(in srgb, var(--vermilion) 12%, transparent);
  border-radius: 4px; padding: 1px 6px; margin-left: 6px; vertical-align: 1px;
}
.badge-host {
  font-size: 10px; font-weight: 700; color: #b8860b;
  background: color-mix(in srgb, #b8860b 14%, transparent);
  border-radius: 4px; padding: 1px 6px; margin-right: 6px; vertical-align: 1px;
}

.dim { display: inline-flex; align-items: center; gap: 6px; font-size: 12px; color: var(--dim); font-style: italic; padding: 8px 0; }
.tbl { width: 100%; border-collapse: collapse; font-size: 12.5px; }
.tbl th {
  text-align: left; font-weight: 500; color: var(--muted); font-size: 11.5px;
  padding: 6px 8px; border-bottom: 1px solid var(--border-soft);
}
.tbl td { padding: 8px; border-bottom: 1px solid var(--border-soft); color: var(--text); }
.tbl tr:last-child td { border-bottom: none; }
.tbl tr.off td { opacity: 0.55; }
.ta-r { text-align: right; }
.mono { font-family: var(--mono); font-size: 11px; color: var(--muted); }
.dot { display: inline-block; width: 7px; height: 7px; border-radius: 50%; background: var(--muted); margin-right: 5px; }
.dot.ok { background: #1f9d55; }
.spin { animation: spin 0.9s linear infinite; }
@keyframes spin { to { transform: rotate(360deg); } }
</style>
