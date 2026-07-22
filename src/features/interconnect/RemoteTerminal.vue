<script setup lang="ts">
/**
 * 远程终端：对某台经 iroh 隧道互联的设备发受控执行请求（/api/exec）。
 *
 * 它不是 SSH，也不是真 TTY —— 每条命令是一次独立的请求/响应，没有持久 shell 会话，
 * 所以 `cd` 不会留在下一条命令里（改用上方的「工作目录」框）。这是刻意的：
 * 无状态执行才能让对端每条都过一遍闸门与审计。
 *
 * 对端的模式由**它自己**决定（白名单/Shell），本页只如实显示、不代为放宽。
 */
import { ref, reactive, computed, nextTick, onMounted } from "vue";
import { Terminal, X, CornerDownLeft, LoaderCircle, ShieldCheck, ShieldAlert } from "@lucide/vue";

const props = defineProps<{ name: string; port: number; token?: string }>();
const emit = defineEmits<{ close: [] }>();

interface Entry {
  cmd: string;
  cwd: string;
  ok: boolean;
  out: string;
  err: string;
  code: number | null;
  ms: number;
  truncated: boolean;
  timedOut: boolean;
  denied?: boolean;
}

const input = ref("");
const cwd = ref("");
const busy = ref(false);
const entries = ref<Entry[]>([]);
const scroller = ref<HTMLElement | null>(null);
/** 对端策略：null=还没探到（老版本对端没有 exec 端点也停在 null）。 */
const policy = ref<{ enabled: boolean; mode: string; whitelist: string[] } | null>(null);
const policyErr = ref("");
/** 上翻历史（只存成功发出去的命令行）。 */
const history: string[] = [];
let histIdx = -1;

const modeLabel = computed(() => {
  if (!policy.value) return policyErr.value || "探测中…";
  if (!policy.value.enabled) return "对端未开启远程执行";
  return policy.value.mode === "shell" ? "Shell 模式（临时解锁）" : "白名单模式";
});

function api(path: string) {
  return `http://127.0.0.1:${props.port}${path}`;
}
function headers() {
  return {
    "content-type": "application/json",
    ...(props.token ? { Authorization: `Bearer ${props.token}` } : {}),
  };
}

async function probe() {
  try {
    const r = await fetch(api("/api/invoke"), {
      method: "POST",
      headers: headers(),
      body: JSON.stringify({ cmd: "exec_policy_get", args: {} }),
    });
    if (!r.ok) throw new Error(String(r.status));
    policy.value = await r.json();
  } catch {
    // 对端是旧版本（无此命令）或隧道断了。不猜，如实说。
    policyErr.value = "对端版本较旧或连接不通,无法读取执行策略";
  }
}

/**
 * 把一行命令拆成 cmd + args，支持双引号包裹带空格的参数。
 * 白名单模式下对端只认结构化的 cmd/args；Shell 模式我们整行原样发过去让对端 shell 解释。
 */
function tokenize(line: string): string[] {
  const out: string[] = [];
  let cur = "";
  let q: '"' | "'" | null = null;
  for (const ch of line) {
    if (q) {
      if (ch === q) q = null;
      else cur += ch;
    } else if (ch === '"' || ch === "'") {
      q = ch;
    } else if (/\s/.test(ch)) {
      if (cur) out.push(cur), (cur = "");
    } else cur += ch;
  }
  if (cur) out.push(cur);
  return out;
}

async function run() {
  const line = input.value.trim();
  if (!line || busy.value) return;
  busy.value = true;
  input.value = "";
  history.push(line);
  histIdx = -1;

  const shellMode = policy.value?.mode === "shell";
  const toks = tokenize(line);
  const body = shellMode
    ? { line, cwd: cwd.value.trim() }
    : { cmd: toks[0] ?? "", args: toks.slice(1), cwd: cwd.value.trim() };

  const started = performance.now();
  try {
    const r = await fetch(api("/api/exec"), {
      method: "POST",
      headers: headers(),
      body: JSON.stringify(body),
    });
    const j = await r.json().catch(() => ({}));
    if (!r.ok) {
      entries.value.push({
        cmd: line,
        cwd: cwd.value.trim(),
        ok: false,
        out: "",
        err: j?.error || `HTTP ${r.status}`,
        code: null,
        ms: Math.round(performance.now() - started),
        truncated: false,
        timedOut: false,
        denied: r.status === 403,
      });
    } else {
      entries.value.push({
        cmd: j.command || line,
        cwd: cwd.value.trim(),
        ok: !!j.ok,
        out: j.stdout || "",
        err: j.stderr || "",
        code: j.exit_code ?? null,
        ms: j.duration_ms ?? 0,
        truncated: !!j.truncated,
        timedOut: !!j.timed_out,
      });
      // 每次执行后顺手刷新策略：对端可能刚解锁/刚关掉，别让面板显示陈旧档位。
      probe();
    }
  } catch (e) {
    entries.value.push({
      cmd: line,
      cwd: cwd.value.trim(),
      ok: false,
      out: "",
      err: `请求失败:${(e as Error).message}(隧道可能已断)`,
      code: null,
      ms: Math.round(performance.now() - started),
      truncated: false,
      timedOut: false,
    });
  } finally {
    busy.value = false;
    await nextTick();
    scroller.value?.scrollTo({ top: scroller.value.scrollHeight });
  }
}

/** ↑/↓ 翻历史命令。 */
function onKey(e: KeyboardEvent) {
  if (e.key === "ArrowUp") {
    if (!history.length) return;
    e.preventDefault();
    histIdx = histIdx < 0 ? history.length - 1 : Math.max(0, histIdx - 1);
    input.value = history[histIdx];
  } else if (e.key === "ArrowDown") {
    if (histIdx < 0) return;
    e.preventDefault();
    histIdx++;
    if (histIdx >= history.length) {
      histIdx = -1;
      input.value = "";
    } else input.value = history[histIdx];
  }
}

onMounted(probe);
</script>

<template>
  <div class="rt-mask" @click.self="emit('close')">
    <div class="rt">
      <div class="rt-head">
        <Terminal :size="16" :stroke-width="1.8" />
        <span class="rt-title">{{ props.name }}</span>
        <span
          class="rt-mode"
          :class="{ ok: policy?.enabled, warn: policy && !policy.enabled, shell: policy?.mode === 'shell' }"
        >
          <ShieldCheck v-if="policy?.enabled && policy.mode !== 'shell'" :size="11" />
          <ShieldAlert v-else-if="policy" :size="11" />
          {{ modeLabel }}
        </span>
        <button class="rt-x" @click="emit('close')"><X :size="15" /></button>
      </div>

      <div class="rt-cwd">
        <span class="cl">工作目录</span>
        <input v-model="cwd" placeholder="留空 = 对端默认目录,例:D:\repo 或 /srv/app" />
      </div>

      <div ref="scroller" class="rt-body">
        <div v-if="!entries.length" class="rt-empty">
          <p v-if="policy && !policy.enabled">
            对端没开「允许远程执行」—— 去那台设备的互联页打开开关。
          </p>
          <p v-else-if="policy?.mode === 'whitelist'">
            白名单模式:只能跑在册命令,且不过 shell（管道/重定向不可用）。
            <br /><span class="wl">{{ policy.whitelist.join("  ") }}</span>
          </p>
          <p v-else-if="policy?.mode === 'shell'">
            对端已临时解锁 Shell 模式 —— 整行命令交给它的 shell 解释，管道/重定向可用。
          </p>
          <p v-else class="dim">{{ policyErr || "正在读取对端执行策略…" }}</p>
        </div>

        <div v-for="(e, i) in entries" :key="i" class="rt-entry">
          <div class="rt-cmd">
            <span class="ps">›</span>{{ e.cmd }}
            <span v-if="e.cwd" class="at">@ {{ e.cwd }}</span>
          </div>
          <pre v-if="e.out" class="rt-out">{{ e.out }}</pre>
          <pre v-if="e.err" class="rt-err" :class="{ denied: e.denied }">{{ e.err }}</pre>
          <div class="rt-meta">
            <span :class="e.ok ? 'g' : 'r'">
              {{ e.timedOut ? "超时终止" : e.denied ? "被拒绝" : `退出 ${e.code ?? "-"}` }}
            </span>
            <span>{{ e.ms }}ms</span>
            <span v-if="e.truncated" class="tr">输出已截断(256KB)</span>
          </div>
        </div>
      </div>

      <div class="rt-input">
        <span class="ps">›</span>
        <input
          v-model="input"
          :disabled="busy || (policy ? !policy.enabled : false)"
          :placeholder="
            policy && !policy.enabled
              ? '对端未开启远程执行'
              : policy?.mode === 'shell'
                ? '整行命令(可用管道),回车执行'
                : '例:git status --short'
          "
          @keydown.enter="run"
          @keydown="onKey"
        />
        <button class="rt-go" :disabled="busy || !input.trim()" @click="run">
          <LoaderCircle v-if="busy" :size="14" class="spin" />
          <CornerDownLeft v-else :size="14" />
        </button>
      </div>
      <p class="rt-foot">
        每条命令都是独立请求(无持久 shell,<code>cd</code> 不跨条留存),对端逐条记审计。
      </p>
    </div>
  </div>
</template>

<style scoped>
.rt-mask {
  position: fixed; inset: 0; z-index: 60; display: grid; place-items: center;
  background: rgba(0, 0, 0, 0.42); backdrop-filter: blur(3px);
}
.rt {
  width: min(860px, 92vw); height: min(620px, 86vh); display: flex; flex-direction: column;
  border-radius: 16px; overflow: hidden;
  background: var(--panel, #14161a); border: 1px solid var(--glass-brd, rgba(255,255,255,.1));
  box-shadow: 0 24px 64px rgba(0,0,0,.5);
}
.rt-head {
  display: flex; align-items: center; gap: 9px; padding: 12px 14px;
  border-bottom: 1px solid var(--glass-brd, rgba(255,255,255,.08));
}
.rt-title { font-weight: 600; font-size: 13.5px; }
.rt-mode {
  display: inline-flex; align-items: center; gap: 4px; margin-left: 2px;
  font-size: 11px; padding: 2.5px 8px; border-radius: 20px;
  background: color-mix(in srgb, var(--fg, #fff) 8%, transparent); color: var(--dim, #9aa);
}
.rt-mode.ok { color: #4ade80; background: rgba(74,222,128,.12); }
.rt-mode.warn { color: #fbbf24; background: rgba(251,191,36,.12); }
.rt-mode.shell { color: #fb923c; background: rgba(251,146,60,.14); }
.rt-x { margin-left: auto; background: none; border: 0; color: var(--dim, #9aa); cursor: pointer; padding: 3px; border-radius: 6px; }
.rt-x:hover { background: rgba(255,255,255,.08); }

.rt-cwd { display: flex; align-items: center; gap: 9px; padding: 9px 14px; border-bottom: 1px solid var(--glass-brd, rgba(255,255,255,.06)); }
.rt-cwd .cl { font-size: 11.5px; color: var(--dim, #9aa); flex: none; }
.rt-cwd input {
  flex: 1; background: rgba(0,0,0,.25); border: 1px solid var(--glass-brd, rgba(255,255,255,.08));
  border-radius: 8px; padding: 5px 9px; color: inherit; font-size: 12px;
  font-family: ui-monospace, "Cascadia Code", Consolas, monospace;
}

.rt-body { flex: 1; overflow-y: auto; padding: 12px 14px; font-size: 12.5px; }
.rt-empty { color: var(--dim, #9aa); font-size: 12.5px; line-height: 1.75; }
.rt-empty .wl { font-family: ui-monospace, Consolas, monospace; font-size: 11px; opacity: .8; word-spacing: 3px; }
.rt-empty .dim { opacity: .6; }

.rt-entry { margin-bottom: 14px; }
.rt-cmd { font-family: ui-monospace, "Cascadia Code", Consolas, monospace; font-size: 12.5px; margin-bottom: 5px; }
.ps { color: #4ade80; margin-right: 7px; font-weight: 700; }
.at { color: var(--dim, #9aa); font-size: 11px; margin-left: 8px; }
.rt-out, .rt-err {
  margin: 0 0 4px; padding: 8px 10px; border-radius: 8px; white-space: pre-wrap; word-break: break-word;
  font-family: ui-monospace, "Cascadia Code", Consolas, monospace; font-size: 11.5px; line-height: 1.55;
  background: rgba(0,0,0,.28); max-height: 340px; overflow: auto;
}
.rt-err { color: #fca5a5; background: rgba(239,68,68,.09); }
.rt-err.denied { color: #fbbf24; background: rgba(251,191,36,.1); }
.rt-meta { display: flex; gap: 12px; font-size: 10.5px; color: var(--dim, #9aa); }
.rt-meta .g { color: #4ade80; }
.rt-meta .r { color: #f87171; }
.rt-meta .tr { color: #fbbf24; }

.rt-input { display: flex; align-items: center; gap: 8px; padding: 11px 14px; border-top: 1px solid var(--glass-brd, rgba(255,255,255,.08)); }
.rt-input input {
  flex: 1; background: rgba(0,0,0,.25); border: 1px solid var(--glass-brd, rgba(255,255,255,.08));
  border-radius: 9px; padding: 8px 11px; color: inherit; font-size: 12.5px;
  font-family: ui-monospace, "Cascadia Code", Consolas, monospace;
}
.rt-input input:disabled { opacity: .5; }
.rt-go {
  flex: none; width: 34px; height: 34px; border-radius: 9px; border: 0; cursor: pointer;
  background: var(--accent, #4c8dff); color: #fff; display: grid; place-items: center;
}
.rt-go:disabled { opacity: .4; cursor: default; }
.spin { animation: rtspin 1s linear infinite; }
@keyframes rtspin { to { transform: rotate(360deg); } }
.rt-foot { margin: 0; padding: 0 14px 11px; font-size: 10.5px; color: var(--dim, #9aa); opacity: .8; }
.rt-foot code { font-family: ui-monospace, Consolas, monospace; }
</style>
