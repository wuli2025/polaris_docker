<script setup lang="ts">
import { onMounted, ref } from "vue";
import { open } from "@tauri-apps/plugin-dialog";
import { kb, isTauri } from "../tauri";
import { useAppStore } from "../stores/app";

const app = useAppStore();

const currentRoot = ref("");
const defaultRoot = ref("");
const draft = ref("");
const busy = ref(false);
const message = ref<{ kind: "ok" | "err"; text: string } | null>(null);

async function refresh() {
  try {
    currentRoot.value = await kb.root();
    defaultRoot.value = await kb.defaultRoot();
    draft.value = currentRoot.value;
  } catch (e) {
    message.value = { kind: "err", text: `读取 KB 路径失败:${String(e)}` };
  }
}

onMounted(refresh);

async function pickFolder() {
  if (!isTauri) {
    message.value = { kind: "err", text: "浏览器模式不支持选择目录" };
    return;
  }
  const picked = await open({
    directory: true,
    multiple: false,
    title: "选择 KB 根目录",
  });
  if (typeof picked === "string" && picked) {
    draft.value = picked;
  }
}

async function save() {
  if (busy.value) return; // 防双发:重扫索引是重操作
  const v = draft.value.trim();
  if (!v) {
    message.value = { kind: "err", text: "路径不能为空" };
    return;
  }
  busy.value = true;
  message.value = null;
  try {
    const n = await kb.setRoot(v);
    await refresh();
    message.value = {
      kind: "ok",
      text: `已切换。重新扫描完成,索引 ${n} 篇文档。`,
    };
  } catch (e) {
    message.value = { kind: "err", text: String(e) };
  } finally {
    busy.value = false;
  }
}

function useDefault() {
  draft.value = defaultRoot.value;
}
</script>

<template>
  <div class="settings">
    <header class="head">
      <h1>设置</h1>
      <p class="sub">配置 Polaris 工作台的本地路径与运行参数。</p>
    </header>

    <section class="block">
      <div class="b-title">外观</div>
      <div class="b-desc">
        浅色为默认的暖白水墨主题；黑夜模式为石墨炭黑主题(中性深色 +
        石墨卡面 + 暖金点缀)，切换立即生效并记住选择。
      </div>
      <div class="theme-row">
        <button
          class="theme-card"
          :class="{ active: app.theme === 'light' }"
          @click="app.setTheme('light')"
        >
          <span class="theme-swatch light-sw">
            <span class="sw-side"></span>
            <span class="sw-main"><span class="sw-card"></span></span>
          </span>
          <span class="theme-name">浅色 · 暖白水墨</span>
        </button>
        <button
          class="theme-card"
          :class="{ active: app.theme === 'dark' }"
          @click="app.setTheme('dark')"
        >
          <span class="theme-swatch dark-sw">
            <span class="sw-side"></span>
            <span class="sw-main"><span class="sw-card"></span></span>
          </span>
          <span class="theme-name">黑夜 · 石墨炭黑</span>
        </button>
      </div>
    </section>

    <section class="block">
      <div class="b-title">知识库根目录(KB 根)</div>
      <div class="b-desc">
        Polaris 在此目录下维护
        <code>raw/</code> · <code>output/</code> · <code>wiki/</code>
        三层结构。修改后立即生效,索引自动重扫,旧目录不会被删除。
      </div>

      <div class="row labels">
        <span>当前</span>
      </div>
      <div class="row">
        <input class="path-ro" :value="currentRoot" readonly />
      </div>

      <div class="row labels">
        <span>新路径</span>
        <button class="link-btn" @click="useDefault" :disabled="busy">
          填入默认 ({{ defaultRoot }})
        </button>
      </div>
      <div class="row">
        <input
          class="path-in"
          v-model="draft"
          placeholder="例如 C:\Users\mi\Polaris\PolarisKB"
          :disabled="busy"
        />
        <button class="btn" @click="pickFolder" :disabled="busy">浏览…</button>
        <button
          class="btn primary"
          @click="save"
          :disabled="busy || draft.trim() === currentRoot"
        >
          {{ busy ? "正在切换…" : "保存并重扫" }}
        </button>
      </div>

      <div
        v-if="message"
        class="msg"
        :class="{ ok: message.kind === 'ok', err: message.kind === 'err' }"
      >
        {{ message.text }}
      </div>
    </section>

    <section class="block muted">
      <div class="b-title sm">即将开放</div>
      <ul class="todo">
        <li>Claude Code 二进制路径</li>
        <li>沙箱镜像名 / Docker socket</li>
      </ul>
    </section>
  </div>
</template>

<style scoped>
.settings {
  flex: 1;
  overflow-y: auto;
  padding: 40px 56px 80px;
  max-width: 820px;
  margin: 0 auto;
  width: 100%;
}
.head {
  border-bottom: 1px solid var(--hairline);
  padding-bottom: 18px;
  margin-bottom: 32px;
}
.head h1 {
  font-family: var(--serif);
  font-size: 22px;
  font-weight: 500;
  letter-spacing: 2px;
  margin: 0 0 8px;
  color: var(--ink);
}
.head .sub {
  font-size: 12.5px;
  color: var(--muted);
  margin: 0;
  letter-spacing: 0.4px;
}

.block {
  background: var(--panel);
  border: 1px solid var(--hairline);
  border-radius: 2px;
  padding: 22px 24px;
  margin-bottom: 22px;
  box-shadow: var(--shadow-sm);
}
.block.muted {
  background: transparent;
  box-shadow: none;
  border-color: var(--border-soft);
}
.b-title {
  font-family: var(--serif);
  font-size: 14.5px;
  font-weight: 600;
  color: var(--ink);
  letter-spacing: 1.2px;
  margin-bottom: 6px;
}
.b-title.sm {
  font-size: 12px;
  color: var(--muted);
  font-weight: 500;
}
.b-desc {
  font-size: 12.5px;
  color: var(--text-2);
  line-height: 1.85;
  margin-bottom: 18px;
}
.b-desc code {
  background: var(--code-bg);
  color: var(--code-text);
  padding: 1px 6px;
  border-radius: 2px;
  font-family: var(--mono);
  font-size: 11.5px;
}

.row {
  display: flex;
  gap: 8px;
  align-items: center;
  margin-bottom: 10px;
}
.row.labels {
  margin-bottom: 4px;
  font-size: 11.5px;
  color: var(--dim);
  letter-spacing: 1px;
  font-family: var(--serif);
  justify-content: space-between;
}
.path-ro,
.path-in {
  flex: 1;
  padding: 8px 10px;
  border: 1px solid var(--border);
  border-radius: 2px;
  font-family: var(--mono);
  font-size: 12px;
  background: var(--panel);
  color: var(--text);
}
.path-ro {
  background: var(--bg-soft);
  color: var(--muted);
}
.path-in:focus {
  outline: none;
  border-color: var(--primary);
}

.btn {
  padding: 8px 14px;
  background: transparent;
  border: 1px solid var(--border);
  border-radius: 2px;
  color: var(--text-2);
  font-size: 12.5px;
  letter-spacing: 0.5px;
  cursor: pointer;
}
.btn:hover:not(:disabled) {
  border-color: var(--ink);
  color: var(--ink);
}
.btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.btn.primary {
  background: var(--btn-solid-bg);
  color: var(--btn-solid-text);
  border-color: var(--btn-solid-bg);
}
.btn.primary:hover:not(:disabled) {
  background: var(--primary);
  border-color: var(--primary);
}

.link-btn {
  background: transparent;
  border: none;
  color: var(--primary);
  font-size: 11.5px;
  letter-spacing: 0.3px;
  cursor: pointer;
  padding: 0;
}
.link-btn:hover:not(:disabled) {
  text-decoration: underline;
}
.link-btn:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}

/* 主题切换卡：左侧栏+主区的微缩示意图，所见即所得 */
.theme-row {
  display: flex;
  gap: 14px;
}
.theme-card {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 10px;
  background: transparent;
  border: 1px solid var(--border);
  border-radius: 8px;
  cursor: pointer;
  transition: border-color 0.15s, box-shadow 0.15s;
}
.theme-card:hover {
  border-color: var(--border-strong);
}
.theme-card.active {
  border-color: var(--primary);
  box-shadow: 0 0 0 2px var(--primary-soft);
}
.theme-swatch {
  display: flex;
  width: 148px;
  height: 88px;
  border-radius: 5px;
  overflow: hidden;
  border: 1px solid var(--hairline);
}
.theme-swatch .sw-side {
  width: 34%;
}
.theme-swatch .sw-main {
  flex: 1;
  display: flex;
  align-items: flex-end;
  justify-content: center;
  padding: 8px;
}
.theme-swatch .sw-card {
  width: 86%;
  height: 26px;
  border-radius: 7px;
}
.light-sw {
  background: #ffffff;
}
.light-sw .sw-side {
  background: #ffffff;
}
.light-sw .sw-card {
  background: rgba(255, 255, 255, 0.85);
  border: 1px solid rgba(190, 182, 162, 0.6);
}
.dark-sw {
  background: #181818;
}
.dark-sw .sw-side {
  background: #1f1f1f;
}
.dark-sw .sw-card {
  background: rgba(43, 43, 41, 0.95);
  border: 1px solid rgba(255, 255, 255, 0.14);
}
.theme-name {
  font-size: 12px;
  color: var(--text-2);
  letter-spacing: 0.5px;
}
.theme-card.active .theme-name {
  color: var(--primary);
  font-weight: 600;
}

.msg {
  margin-top: 14px;
  padding: 8px 12px;
  border-radius: 2px;
  font-size: 12.5px;
  letter-spacing: 0.3px;
}
.msg.ok {
  background: var(--primary-soft);
  color: var(--primary-deep);
  border-left: 2px solid var(--primary);
}
.msg.err {
  background: var(--vermilion-soft);
  color: var(--vermilion);
  border-left: 2px solid var(--vermilion);
}

.todo {
  margin: 0;
  padding-left: 18px;
  font-size: 12px;
  color: var(--muted);
  line-height: 2;
}
</style>
