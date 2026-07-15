<script setup lang="ts">
import { onMounted, ref } from "vue";
import {
  TicketPlus,
  Copy,
  LoaderCircle,
  RefreshCw,
  Server,
  Users,
  MonitorSmartphone,
  ShieldOff,
} from "@lucide/vue";
import {
  collabApi,
  fmtTime,
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
async function toggleUser(u: AdminUser) {
  try {
    await collabApi.adminUserDisable(u.id, !u.disabled);
    u.disabled = !u.disabled;
    toast.info(`已${u.disabled ? "停用" : "启用"}「${u.username}」`);
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

onMounted(() => {
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

    <!-- 用户列表 -->
    <section class="card">
      <h3>
        <Users :size="15" :stroke-width="1.8" /> 用户
        <button class="refresh" title="刷新" @click="loadUsers"><RefreshCw :size="13" /></button>
      </h3>
      <div v-if="usersLoading" class="dim"><LoaderCircle :size="13" class="spin" /> 加载中…</div>
      <div v-else-if="!users.length" class="dim">还没有其他用户,先生成票据邀请同事吧</div>
      <table v-else class="tbl">
        <thead>
          <tr><th>用户名</th><th>昵称</th><th>角色</th><th>状态</th><th></th></tr>
        </thead>
        <tbody>
          <tr v-for="u in users" :key="u.id" :class="{ off: u.disabled }">
            <td>{{ u.username }}</td>
            <td>{{ u.display_name || "—" }}</td>
            <td>{{ u.role }}</td>
            <td>
              <span class="dot" :class="{ ok: !u.disabled }"></span>
              {{ u.disabled ? "已停用" : "正常" }}
            </td>
            <td class="ta-r">
              <button class="btn ghost sm" @click="toggleUser(u)">
                {{ u.disabled ? "启用" : "停用" }}
              </button>
            </td>
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
