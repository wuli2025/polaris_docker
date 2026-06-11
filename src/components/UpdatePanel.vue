<script setup lang="ts">
// 「更新」板块：显示当前版本、手动检查更新、一键更新。
// 桌面 Tauri 走 updater.rs 状态机；容器模式走 /api/invoke docker_update（容器内 spawn update.sh）。
// 与中央对话框(UpdateBanner)共享 useUpdater 的状态——启动自动检测，
// 这里则给用户一个随时主动检查的入口。
import { onMounted, computed } from "vue";
import { getVersion } from "@tauri-apps/api/app";
import {
  RefreshCw,
  Sparkles,
  CheckCircle2,
  LoaderCircle,
  Rocket,
  Container,
} from "@lucide/vue";
import {
  currentVersion,
  updateVersion,
  updateNotes,
  updating,
  updateProgress,
  updateError,
  checking,
  upToDate,
  checkFailed,
  lastCheckedAt,
  manualCheck,
  applyUpdate,
  isDockerMode,
  dockerUpdaterEnabled,
  dockerStatus,
  dockerLastApply,
  dockerApplying,
  dockerCheck,
  dockerApply,
} from "../composables/useUpdater";

onMounted(async () => {
  if (!currentVersion.value) {
    if (isDockerMode.value) {
      // 容器模式：useUpdater 内部的 ensureCurrentVersion 会调 /api/version
      await manualCheck();
    } else {
      try {
        currentVersion.value = await getVersion();
      } catch {
        /* 浏览器预览态拿不到版本，忽略 */
      }
    }
  }
});

const lastChecked = computed(() => {
  if (!lastCheckedAt.value) return "";
  const d = new Date(lastCheckedAt.value);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}`;
});

// 容器模式:UpdatePanel 的「检查/立即更新」按钮转调 dockerCheck / dockerApply。
async function onCheck() {
  if (isDockerMode.value) return dockerCheck();
  return manualCheck();
}
async function onApply() {
  if (isDockerMode.value) return dockerApply();
  return applyUpdate();
}
</script>

<template>
  <div class="up-panel">
    <header class="up-header">
      <h1>更新</h1>
      <p class="up-sub">保持 Polaris 为最新版本</p>
    </header>

    <div class="up-body">
      <!-- 当前版本 -->
      <div class="ver-card">
        <img class="ver-logo" src="../assets/logo.png" alt="北极星" />
        <div class="ver-meta">
          <div class="ver-name">北极星 · Polaris</div>
          <div class="ver-num">当前版本 v{{ currentVersion || "—" }}</div>
          <div v-if="isDockerMode" class="ver-mode">
            <Container :size="12" :stroke-width="2" />
            <span>Docker 版{{ dockerStatus?.current_tag ? ` · ${dockerStatus.current_tag}` : "" }}</span>
          </div>
        </div>
        <button
          class="ck-btn"
          :disabled="checking || dockerApplying"
          :title="isDockerMode && !dockerUpdaterEnabled
            ? '请在 docker-compose 取消注释 POLARIS_DOCKER_SOCKET=1 并挂载 /var/run/docker.sock'
            : ''"
          @click="onCheck"
        >
          <LoaderCircle
            v-if="checking || dockerApplying"
            :size="15"
            :stroke-width="2"
            class="spin"
          />
          <RefreshCw v-else :size="15" :stroke-width="2" />
          <span>{{ checking ? "检查中…" : "检查更新" }}</span>
        </button>
      </div>

      <!-- 状态 / 更新区 -->
      <div class="state">
        <!-- 发现新版本 -->
        <div v-if="updateVersion" class="found">
          <div class="found-top">
            <span class="found-badge"><Sparkles :size="18" :stroke-width="1.7" /></span>
            <div>
              <div class="found-title">
                发现新版本 <b>v{{ updateVersion }}</b>
              </div>
              <div class="found-hint">
                {{ updating ? "正在下载，完成后自动重启生效" : "点「立即更新」后台下载安装，自动重启即用" }}
              </div>
            </div>
          </div>

          <div v-if="updateNotes && !updating" class="found-notes">{{ updateNotes }}</div>

          <div v-if="updating" class="bar">
            <div class="bar-fill" :style="{ width: updateProgress + '%' }"></div>
          </div>

          <button class="go-btn" :disabled="updating" @click="applyUpdate">
            <LoaderCircle
              v-if="updating"
              :size="15"
              :stroke-width="2"
              class="spin"
            />
            <Rocket v-else :size="15" :stroke-width="1.9" />
            <span>{{ updating ? `更新中 ${updateProgress}%` : "立即更新" }}</span>
          </button>
        </div>

        <!-- 已是最新 -->
        <div v-else-if="upToDate" class="ok">
          <CheckCircle2 :size="18" :stroke-width="1.8" />
          <span>已是最新版本</span>
        </div>

        <!-- Docker 模式专属面板：一键更新到 GHCR 最新 -->
        <div v-else-if="isDockerMode" class="docker-panel">
          <div class="dp-top">
            <span class="dp-badge"><Container :size="18" :stroke-width="1.7" /></span>
            <div>
              <div class="dp-title">Docker 容器版</div>
              <div class="dp-hint">
                <template v-if="dockerUpdaterEnabled">
                  点「立即更新」会后台拉取新镜像并自动重建容器（数据卷保留）。
                </template>
                <template v-else>
                  <b>Web 一键更新未启用</b>。请在 <code>docker-compose.synology.yml</code> 取消注释
                  <code>POLARIS_DOCKER_SOCKET="1"</code> 与
                  <code>/var/run/docker.sock</code> 挂载,重建容器后此处变可点。
                  <br />或者在终端跑 <code>./update.sh</code>(仓库自带) 手动更新。
                </template>
              </div>
            </div>
          </div>

          <div v-if="dockerLastApply" class="dp-result" :class="{ ok: dockerLastApply.success, err: !dockerLastApply.success }">
            <div v-if="dockerLastApply.error">❌ {{ dockerLastApply.error }}</div>
            <div v-else-if="dockerLastApply.success">
              ✅ update.sh 已执行(exit={{ dockerLastApply.exit_code }})。容器马上会被替换,
              HTTP 短暂断开,稍等几秒刷新即可。
            </div>
            <pre v-if="dockerLastApply.stdout" class="dp-stdout">{{ dockerLastApply.stdout }}</pre>
            <pre v-if="dockerLastApply.stderr" class="dp-stderr">{{ dockerLastApply.stderr }}</pre>
          </div>

          <button
            class="go-btn"
            :disabled="!dockerUpdaterEnabled || dockerApplying"
            :title="!dockerUpdaterEnabled
              ? '未启用 docker 一键更新'
              : '拉取新镜像并重建容器'"
            @click="onApply"
          >
            <LoaderCircle
              v-if="dockerApplying"
              :size="15"
              :stroke-width="2"
              class="spin"
            />
            <Rocket v-else :size="15" :stroke-width="1.9" />
            <span>{{ dockerApplying ? "更新中…" : "立即更新" }}</span>
          </button>
        </div>

        <!-- 自动检查失败（非静默，引导用户手动检查） -->
        <div v-else-if="checkFailed && !updateVersion" class="err">
          <div>自动检查更新失败: {{ updateError || "网络或服务端异常" }}</div>
          <div style="margin-top:4px;font-size:11px;color:var(--dim)">
            可点击上方「检查更新」重试，或前往
            <a href="https://github.com/wuli2025/polaris_coworker/releases" target="_blank" style="color:var(--primary)">GitHub Releases</a>
            手动下载
          </div>
        </div>

        <!-- 错误 -->
        <div v-else-if="updateError" class="err">{{ updateError }}</div>

        <!-- 空闲 -->
        <div v-else class="idle">Polaris 启动时会自动检查更新</div>

        <div v-if="lastChecked" class="last">上次检查 {{ lastChecked }}</div>
      </div>

      <!-- 工作原理 -->
      <div class="how">
        <div class="how-title">{{ isDockerMode ? "Docker 版如何更新" : "更新是怎么工作的" }}</div>
        <ol v-if="!isDockerMode">
          <li>启动时自动检查 GitHub 上有没有新版本</li>
          <li>发现新版会在屏幕中央弹一个轻提示，点「立即更新」即可</li>
          <li>后台静默下载并安装，<b>自动重启</b>到新版 —— 无需手动重装</li>
        </ol>
        <ol v-else>
          <li>本镜像由 GitHub Actions 自动构建并推送到 <b>ghcr.io/wuli2025/polaris</b></li>
          <li>启用 docker 一键更新后,点「立即更新」会执行 <code>update.sh</code>:
              <code>docker compose pull</code> + <code>up -d</code></li>
          <li>容器重建会保留数据卷(<code>polaris-data/claude/config</code>);HTTP 短暂断开,几秒后刷新即可</li>
          <li>未启用一键更新?在终端跑 <code>./update.sh</code> 也行,或看 <a href="https://github.com/wuli2025/polaris_docker" target="_blank">仓库说明</a></li>
        </ol>
      </div>
    </div>
  </div>
</template>

<style scoped>
.up-panel {
  height: 100%;
  overflow-y: auto;
  background: var(--bg);
  padding: 28px 32px 40px;
}
.up-header {
  margin-bottom: 22px;
}
.up-header h1 {
  margin: 0;
  font-family: var(--serif);
  font-size: 22px;
  font-weight: 600;
  color: var(--ink);
  letter-spacing: 2px;
}
.up-sub {
  margin: 4px 0 0;
  font-size: 12.5px;
  color: var(--muted);
}
.up-body {
  max-width: 560px;
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.ver-card {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 16px 18px;
  background: var(--panel);
  border: 1px solid var(--border-soft);
  border-radius: 14px;
}
.ver-logo {
  width: 40px;
  height: 40px;
  border-radius: 10px;
  object-fit: contain;
  flex-shrink: 0;
}
.ver-meta {
  flex: 1;
  min-width: 0;
}
.ver-name {
  font-family: var(--serif);
  font-size: 14px;
  font-weight: 600;
  color: var(--text);
  letter-spacing: 1px;
}
.ver-num {
  margin-top: 2px;
  font-size: 12px;
  color: var(--muted);
}
.ck-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 8px 14px;
  border: 1px solid var(--border);
  border-radius: 10px;
  background: var(--bg-soft);
  color: var(--text);
  font-size: 12.5px;
  font-weight: 500;
  flex-shrink: 0;
}
.ck-btn:hover:not(:disabled) {
  border-color: var(--primary);
  color: var(--primary);
}
.ck-btn:disabled {
  opacity: 0.65;
  cursor: default;
}

.state {
  padding: 4px 2px;
}
.found {
  padding: 16px;
  background: var(--primary-soft);
  border: 1px solid color-mix(in srgb, var(--primary) 28%, transparent);
  border-radius: 14px;
}
.found-top {
  display: flex;
  gap: 12px;
  align-items: flex-start;
}
.found-badge {
  width: 34px;
  height: 34px;
  border-radius: 9px;
  background: var(--panel);
  color: var(--primary);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
}
.found-title {
  font-size: 14px;
  color: var(--text);
  font-weight: 500;
}
.found-title b {
  color: var(--primary);
}
.found-hint {
  margin-top: 3px;
  font-size: 11.5px;
  color: var(--muted);
}
.found-notes {
  margin-top: 12px;
  max-height: 120px;
  overflow-y: auto;
  padding: 10px 12px;
  background: var(--panel);
  border-radius: 10px;
  font-size: 11.5px;
  line-height: 1.6;
  color: var(--text-2);
  white-space: pre-wrap;
}
.bar {
  margin-top: 14px;
  height: 6px;
  border-radius: 3px;
  background: var(--panel);
  overflow: hidden;
}
.bar-fill {
  height: 100%;
  background: var(--primary);
  border-radius: 3px;
  transition: width 0.2s ease;
}
.go-btn {
  margin-top: 14px;
  width: 100%;
  padding: 11px 0;
  border: none;
  border-radius: 11px;
  background: var(--btn-solid-bg);
  color: var(--btn-solid-text);
  font-size: 13.5px;
  font-weight: 600;
  letter-spacing: 1px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 7px;
}
.go-btn:hover:not(:disabled) {
  background: var(--primary);
}
.go-btn:disabled {
  opacity: 0.85;
  cursor: default;
}
.ok {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
  color: var(--primary);
  font-weight: 500;
}
.err {
  font-size: 12.5px;
  color: var(--vermilion);
  line-height: 1.6;
}
.idle {
  font-size: 12.5px;
  color: var(--muted);
}
.last {
  margin-top: 8px;
  font-size: 11px;
  color: var(--dim);
}

.how {
  margin-top: 4px;
  padding: 16px 18px;
  background: var(--bg-soft);
  border: 1px solid var(--border-soft);
  border-radius: 14px;
}
.how-title {
  font-family: var(--serif);
  font-size: 12.5px;
  letter-spacing: 1.5px;
  color: var(--text-2);
  margin-bottom: 8px;
}
.how ol {
  margin: 0;
  padding-left: 18px;
}
.how li {
  font-size: 12px;
  color: var(--muted);
  line-height: 1.9;
}
.how li b {
  color: var(--text-2);
}
.spin {
  animation: up-spin 0.9s linear infinite;
}
@keyframes up-spin {
  to {
    transform: rotate(360deg);
  }
}

/* ── Docker 模式专属样式 ───────────────────── */
.ver-mode {
  margin-top: 3px;
  font-size: 11px;
  color: var(--muted);
  display: inline-flex;
  align-items: center;
  gap: 4px;
}
.docker-panel {
  padding: 16px;
  background: var(--bg-soft);
  border: 1px solid var(--border-soft);
  border-radius: 14px;
}
.dp-top {
  display: flex;
  gap: 12px;
  align-items: flex-start;
}
.dp-badge {
  width: 34px;
  height: 34px;
  border-radius: 9px;
  background: var(--panel);
  color: var(--primary);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
}
.dp-title {
  font-size: 14px;
  font-weight: 500;
  color: var(--text);
}
.dp-hint {
  margin-top: 3px;
  font-size: 11.5px;
  color: var(--muted);
  line-height: 1.6;
}
.dp-hint code {
  background: var(--panel);
  padding: 1px 5px;
  border-radius: 4px;
  font-size: 11px;
}
.dp-result {
  margin-top: 12px;
  padding: 10px 12px;
  border-radius: 10px;
  font-size: 12px;
  line-height: 1.6;
}
.dp-result.ok {
  background: color-mix(in srgb, var(--primary) 12%, transparent);
  color: var(--text);
}
.dp-result.err {
  background: color-mix(in srgb, var(--vermilion) 12%, transparent);
  color: var(--vermilion);
}
.dp-stdout,
.dp-stderr {
  margin: 8px 0 0;
  max-height: 140px;
  overflow: auto;
  padding: 8px 10px;
  background: var(--panel);
  border-radius: 6px;
  font-size: 11px;
  line-height: 1.55;
  font-family: ui-monospace, "Cascadia Code", monospace;
  white-space: pre-wrap;
  word-break: break-all;
}
.dp-stderr {
  color: var(--vermilion);
}
</style>
