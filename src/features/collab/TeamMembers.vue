<script setup lang="ts">
/**
 * 团队成员页(GitHub 组织成员风格):
 * - 成员列表 + 角色徽标(owner/member)
 * - 拉人:输入用户名即时搜索(防抖 300ms)下拉补全,点选即加入
 * - 移除:对自己显示「退出团队」;移除他人需团队 owner
 */
import { computed, onBeforeUnmount, ref, watch } from "vue";
import {
  Crown,
  LoaderCircle,
  LogOut,
  UserMinus,
  UserPlus,
  Users,
} from "@lucide/vue";
import { collabApi, type UserSearchHit } from "./api";
import { useCollabStore } from "./stores/collab";
import { toast } from "../../composables/useToast";

const collab = useCollabStore();

const myUsername = computed(() => collab.user?.username ?? "");
/** 我能管人:全局 owner 或本团队 owner */
const canAdmin = computed(() => collab.canManage);

// ── 拉人:即时搜索补全 ──
const query = ref("");
const role = ref<"member" | "owner">("member");
const hits = ref<UserSearchHit[]>([]);
const searching = ref(false);
const dropOpen = ref(false);
const adding = ref(false);
let debounceTimer: ReturnType<typeof setTimeout> | null = null;

watch(query, (q) => {
  if (debounceTimer) clearTimeout(debounceTimer);
  const s = q.trim();
  if (!s) {
    hits.value = [];
    dropOpen.value = false;
    return;
  }
  debounceTimer = setTimeout(() => {
    void doSearch(s);
  }, 300);
});

async function doSearch(s: string) {
  searching.value = true;
  try {
    hits.value = await collabApi.searchUsers(s);
    dropOpen.value = true;
  } catch {
    hits.value = [];
  } finally {
    searching.value = false;
  }
}

async function pick(u: UserSearchHit) {
  dropOpen.value = false;
  adding.value = true;
  try {
    await collab.addTeamMember(u.username, role.value);
    toast.info(`已把「${u.display_name || u.username}」拉进团队`);
    query.value = "";
    hits.value = [];
  } catch (e) {
    toast.error((e as Error).message);
  } finally {
    adding.value = false;
  }
}

async function remove(userId: number, username: string) {
  const isSelf = username === myUsername.value;
  const ask = isSelf
    ? "退出这个团队?退出后将看不到团队项目。"
    : `把「${username}」移出团队?`;
  if (!confirm(ask)) return;
  try {
    await collab.removeTeamMember(userId);
    toast.info(isSelf ? "已退出团队" : "已移出");
  } catch (e) {
    toast.error((e as Error).message);
  }
}

onBeforeUnmount(() => {
  if (debounceTimer) clearTimeout(debounceTimer);
});
</script>

<template>
  <div class="tm">
    <section class="card">
      <h3>
        <Users :size="15" :stroke-width="1.8" />
        {{ collab.currentTeam?.name || "团队" }} · 成员
        <span class="cnt">{{ collab.teamMembers.length }} 人</span>
      </h3>

      <!-- 拉人(团队 owner 可见) -->
      <div v-if="canAdmin" class="invite">
        <div class="inv-box">
          <input
            v-model="query"
            class="inp"
            placeholder="输入用户名拉人,支持模糊搜索…"
            @focus="hits.length && (dropOpen = true)"
            @blur="dropOpen = false"
          />
          <div v-if="dropOpen && (hits.length || searching)" class="drop">
            <div v-if="searching" class="drop-dim">
              <LoaderCircle :size="12" class="spin" /> 搜索中…
            </div>
            <button
              v-for="u in hits"
              :key="u.id"
              class="drop-item"
              @mousedown.prevent="pick(u)"
            >
              <span class="di-name">{{ u.display_name || u.username }}</span>
              <span class="di-user">@{{ u.username }}</span>
            </button>
            <div v-if="!searching && !hits.length" class="drop-dim">没找到这个用户</div>
          </div>
        </div>
        <select v-model="role" class="sel">
          <option value="member">成员</option>
          <option value="owner">团队管理者</option>
        </select>
        <span v-if="adding" class="dim"><LoaderCircle :size="13" class="spin" /></span>
      </div>
      <p v-else class="tip">只有团队管理者能拉人/移人;你可以随时退出团队。</p>

      <!-- 成员列表 -->
      <div v-if="!collab.teamMembers.length" class="dim it">团队还没有成员</div>
      <div v-else class="list">
        <div v-for="m in collab.teamMembers" :key="m.user_id" class="row">
          <span class="name">{{ m.display_name || m.username }}</span>
          <span class="user">@{{ m.username }}</span>
          <span class="badge" :class="{ owner: m.role === 'owner' }">
            <Crown v-if="m.role === 'owner'" :size="10" :stroke-width="2" />
            {{ m.role === "owner" ? "管理者" : "成员" }}
          </span>
          <template v-if="m.username === myUsername">
            <button class="btn danger sm" @click="remove(m.user_id, m.username)">
              <LogOut :size="12" /> 退出团队
            </button>
          </template>
          <template v-else-if="canAdmin">
            <button class="btn ghost sm" @click="remove(m.user_id, m.username)">
              <UserMinus :size="12" /> 移除
            </button>
          </template>
        </div>
      </div>

      <p v-if="canAdmin" class="tip foot">
        <UserPlus :size="12" :stroke-width="1.8" />
        对方需要先在登录页「注册」建号,才能被搜索到。
      </p>
    </section>
  </div>
</template>

<style scoped>
.tm { flex: 1; overflow-y: auto; padding: 16px; }
.card {
  border: 1px solid var(--border-soft); border-radius: 12px;
  background: var(--panel); padding: 16px 18px;
}
.card h3 {
  display: flex; align-items: center; gap: 7px;
  margin: 0 0 12px; font-size: 13.5px; font-weight: 600;
  color: var(--ink); letter-spacing: 1px;
}
.cnt { margin-left: auto; font-size: 11.5px; font-weight: 400; color: var(--muted); }

.invite { display: flex; align-items: center; gap: 8px; margin-bottom: 14px; }
.inv-box { position: relative; flex: 1; min-width: 0; }
.inp {
  width: 100%; box-sizing: border-box;
  border: 1px solid var(--border); border-radius: 8px;
  background: var(--bg); color: var(--ink);
  font-size: 12.5px; padding: 8px 10px; font-family: inherit;
}
.inp:focus { outline: none; border-color: var(--primary, var(--ink)); }
.sel {
  border: 1px solid var(--border); border-radius: 8px;
  background: var(--bg); color: var(--ink);
  font-size: 12.5px; padding: 7px 10px; cursor: pointer;
}
.drop {
  position: absolute; left: 0; right: 0; top: calc(100% + 4px); z-index: 30;
  background: var(--panel); border: 1px solid var(--border);
  border-radius: 10px; box-shadow: var(--shadow-lg, var(--shadow));
  max-height: 220px; overflow-y: auto; padding: 4px;
}
.drop-item {
  width: 100%; text-align: left; border: none; cursor: pointer;
  background: transparent; border-radius: 7px; padding: 7px 9px;
  display: flex; align-items: baseline; gap: 8px;
}
.drop-item:hover { background: var(--selection-bg); }
.di-name { font-size: 12.5px; color: var(--ink); font-weight: 500; }
.di-user { font-size: 11px; color: var(--muted); font-family: var(--mono); }
.drop-dim { display: flex; align-items: center; gap: 6px; padding: 8px 9px; font-size: 12px; color: var(--dim); font-style: italic; }

.list { display: flex; flex-direction: column; }
.row {
  display: flex; align-items: center; gap: 9px;
  padding: 9px 4px; border-bottom: 1px solid var(--border-soft);
}
.row:last-child { border-bottom: none; }
.name { font-size: 13px; color: var(--ink); font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.user { font-size: 11px; color: var(--muted); font-family: var(--mono); }
.badge {
  display: inline-flex; align-items: center; gap: 3px;
  font-size: 10.5px; color: var(--muted);
  background: var(--selection-bg); padding: 2px 8px; border-radius: 20px;
  margin-left: auto;
}
.badge.owner { color: #b8860b; background: color-mix(in srgb, #b8860b 12%, transparent); }

.btn {
  display: inline-flex; align-items: center; gap: 4px;
  border: none; cursor: pointer;
  font-size: 11.5px; padding: 4px 10px; border-radius: 8px;
}
.btn.ghost { background: transparent; color: var(--text-2); border: 1px solid var(--border); }
.btn.ghost:hover { color: var(--ink); border-color: var(--ink); }
.btn.danger { background: transparent; color: var(--vermilion); border: 1px solid var(--border); }
.btn.danger:hover { border-color: var(--vermilion); }

.tip { display: flex; align-items: center; gap: 5px; margin: 0 0 12px; font-size: 12px; color: var(--text-2); line-height: 1.7; }
.tip.foot { margin: 12px 0 0; color: var(--dim); }
.dim { display: inline-flex; align-items: center; gap: 6px; font-size: 12px; color: var(--dim); }
.dim.it { font-style: italic; padding: 8px 0; }
.spin { animation: spin 0.9s linear infinite; }
@keyframes spin { to { transform: rotate(360deg); } }
</style>
