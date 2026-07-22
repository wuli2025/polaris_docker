<script setup lang="ts">
// TurnItem —— 单个回合的渲染子组件(模板与样式自 ChatPanel 的 v-for 原样抽出)。
// 为什么抽:流式 delta 每 ~40ms 一帧,renderTurns 尾回合重建 → visibleTurns 新引用 →
// 父级 v-for 每帧全量走一遍最多 30 个回合的 vnode diff。抽成子组件后,前缀(已定稿)
// 回合的 turn 对象引用不变(ChatPanel 的前缀回合缓存保证),其余 props(布尔/字符串)
// 与事件处理器(父级方法引用,非内联箭头)在流式期间也全部稳定 —— Vue 对子组件做
// props 浅比较,全部相等则直接跳过整棵子树的 re-render 与 DOM patch:历史回合在
// 流式中零开销,只有活跃末回合(turn 引用每帧换新)真正重渲染。
import { computed } from "vue";
import {
  ChevronDown,
  Wrench,
  FolderOpen,
  ExternalLink,
  PencilLine,
  Copy,
  RotateCcw,
  Eye,
} from "@lucide/vue";
import {
  fileName,
  fileExt,
  artifactIcon,
  attachIcon,
  toolLabel,
  fmtTime,
  renderMd,
  isImageArtifact,
  type Turn,
} from "./shared";
import ImageStrip from "./ImageStrip.vue";
import { useArtifactsStore } from "../../stores/artifacts";

const props = defineProps<{
  turn: Turn;
  /** 是否为「生成中」的活跃末回合 */
  pending: boolean;
  /** 当前对话是否在发送(隐藏编辑/重发入口) */
  sending: boolean;
  /** 展开中的工具详情 key(`${turnKey}:${idx}`,全局单开,由父级持有) */
  expandedTool: string | null;
  /** 本回合产物文件夹卡片是否折叠 */
  filesCollapsed: boolean;
}>();

const emit = defineEmits<{
  (e: "toggle-tool", turnKey: number, idx: number): void;
  (e: "toggle-files", turnKey: number): void;
  (e: "open-artifact", path: string): void;
  (e: "open-folder", path: string): void;
  (e: "edit", t: Turn): void;
  (e: "copy", t: Turn): void;
  (e: "regenerate", t: Turn): void;
}>();

// 产物高亮直接读 store(与父级同一实例),不走 props —— 避免为它给所有回合多传一个
// 每次打开文件都变化的字符串 prop。
const artifactsStore = useArtifactsStore();

// 正文 html:定稿回合直接用 renderTurns 预渲染挂在 turn.html 上的结果(命中前缀
// 缓存时连字符串都复用);活跃回合才现场 renderMd(TL;DR 摘取 + ANSI 清洗每帧只在
// 这一个回合上跑)。computed 缓存在组件实例上,turn 引用不变就不重算;renderMd
// 内部读 mdVersion,异步高亮完成后父级重建前缀 → 新 turn.html 自动刷进来。
const html = computed(() =>
  props.turn.html ?? renderMd(props.turn.text, !props.pending)
);

// Kimi 式产物区:主预览件单独成卡,其余文件收进「文件夹」入口。
// 展开时也只内联渲染前 MAX_INLINE 行,溢出的引导去文件管理器看,永不铺满对话框卡死。
const MAX_INLINE = 8;
// 文件夹入口里要列的文件 = 本轮全部产物里排除文件夹产物本身(仍含主预览件,方便一处看全)
const folderFiles = computed(() =>
  props.turn.artifacts.filter((a) => !a.endsWith("/"))
);
// 生成图片单拎出来走横排缩略图画廊(LUMI 式),不再混进文件行当图标
const imageFiles = computed(() => folderFiles.value.filter(isImageArtifact));
const docFiles = computed(
  () => folderFiles.value.filter((a) => !isImageArtifact(a))
);
// 主预览大卡只留非图片主件 —— 图片已在画廊里以真缩略图呈现,再出一张大卡就重复了
const docPreview = computed(() =>
  props.turn.preview && !isImageArtifact(props.turn.preview)
    ? props.turn.preview
    : undefined
);
// 文件夹入口:有非图片产物才出现;唯一一个且已被预览大卡呈现时省掉(预览卡已够)
const showFolderCard = computed(
  () =>
    docFiles.value.length > 0 &&
    !(docFiles.value.length === 1 && docFiles.value[0] === docPreview.value)
);
const inlineFiles = computed(() => docFiles.value.slice(0, MAX_INLINE));
const overflowCount = computed(() =>
  Math.max(0, docFiles.value.length - MAX_INLINE)
);
</script>

<template>
  <div class="turn">
    <!-- 用户消息：右侧中性气泡，无头像 -->
    <div v-if="turn.user" class="msg user">
      <button
        v-if="turn.user.text && !sending"
        class="u-edit"
        title="编辑并重发"
        @click="emit('edit', turn)"
      >
        <PencilLine :size="13" :stroke-width="1.8" />
      </button>
      <div class="bubble-user">
        <div v-if="turn.user.text" class="u-text">{{ turn.user.text }}</div>
        <div
          v-if="turn.user.files && turn.user.files.length"
          class="attach-chips in-bubble"
        >
          <div
            v-for="f in turn.user.files"
            :key="f.path"
            class="attach-chip readonly"
            :title="f.path"
          >
            <component :is="attachIcon(f.kind)" :size="14" :stroke-width="1.7" />
            <span class="ac-name">{{ f.name }}</span>
          </div>
        </div>
      </div>
    </div>

    <!-- 助手回复：纯文本，无头像无边框（Codex 式） -->
    <div
      v-if="
        turn.hasAssistant ||
        turn.tools.length ||
        turn.artifacts.length ||
        turn.errors.length ||
        pending
      "
      class="msg ai"
    >
      <!-- 工具调用：低调 pill,点击展开输入摘要 -->
      <div v-if="turn.tools.length" class="tool-strip">
        <template v-for="(tl, j) in turn.tools" :key="j">
          <button
            class="tool-pill"
            :class="{
              open: expandedTool === `${turn.key}:${j}`,
              clickable: tl.details.length > 0,
            }"
            @click="tl.details.length && emit('toggle-tool', turn.key, j)"
          >
            <Wrench :size="11" :stroke-width="1.8" />
            {{ toolLabel(tl.name) }}
            <span v-if="tl.count > 1" class="tp-count">×{{ tl.count }}</span>
          </button>
        </template>
      </div>
      <div
        v-for="(tl, j) in turn.tools"
        :key="'d' + j"
        v-show="expandedTool === `${turn.key}:${j}`"
        class="tool-detail"
      >
        <div class="td-head">{{ toolLabel(tl.name) }} · 输入摘要</div>
        <div v-for="(d, x) in tl.details" :key="x" class="td-line">{{ d }}</div>
      </div>

      <!-- 参考文件：豆包式小胶囊, 收在回答最前面, 点开右侧预览
           (turn.refs 在构建回合时一次算好,这里只读,不再每帧内联重算) -->
      <div v-if="turn.text && turn.refs.length" class="ref-files">
        <span class="ref-label">参考 {{ turn.refs.length }} 个文件</span>
        <button
          v-for="p in turn.refs"
          :key="p"
          class="ref-pill"
          :title="p"
          @click="emit('open-artifact', p)"
        >
          <component :is="artifactIcon(p)" :size="12" :stroke-width="1.7" />
          <span class="ref-name">{{ fileName(p) }}</span>
        </button>
      </div>

      <!-- 正文：markdown 渲染(流式中的活跃回合跳过异步高亮排队) -->
      <div v-if="turn.text" class="md" v-html="html"></div>

      <!-- 生成中：三点呼吸 -->
      <div v-if="pending" class="typing">
        <span></span><span></span><span></span>
      </div>

      <!-- 错误行 -->
      <div v-for="(e, j) in turn.errors" :key="'e' + j" class="err-line">
        {{ e }}
      </div>

      <!-- 生成的文件(Kimi 式：一文件一预览)——
           ① 主预览大卡：本轮最该「打开看」的那个件(html/演示/pdf…)，点开右抽屉联动。
           ② 文件夹入口：其余产物统一收进一行，点开在文件管理器看，绝不把小文件铺满对话框。 -->
      <div v-if="turn.artifacts.length" class="files">
        <!-- ⓪ 生成图片：横排缩略图画廊(LUMI 式一排),点开右抽屉预览 -->
        <ImageStrip
          v-if="imageFiles.length"
          :paths="imageFiles"
          :folder="turn.folder"
          @open="emit('open-artifact', $event)"
          @open-folder="emit('open-folder', $event)"
        />

        <!-- ① 主预览大卡(非图片主件;图片已在画廊里) -->
        <button
          v-if="docPreview"
          class="preview-card"
          :class="{ active: artifactsStore.current?.path === docPreview }"
          :title="docPreview"
          @click="emit('open-artifact', docPreview)"
        >
          <div class="pv-ico">
            <component
              :is="artifactIcon(docPreview)"
              :size="22"
              :stroke-width="1.6"
            />
          </div>
          <div class="pv-meta">
            <span class="pv-name">{{ fileName(docPreview) }}</span>
            <span class="pv-sub">
              <span v-if="fileExt(docPreview)" class="pv-ext">{{
                fileExt(docPreview)
              }}</span>
              点击预览
            </span>
          </div>
          <span class="pv-open">
            <Eye :size="15" :stroke-width="1.8" />
            {{ artifactsStore.current?.path === docPreview ? "预览中" : "预览" }}
          </span>
        </button>

        <!-- ② 文件夹入口：折叠态只显示一行摘要 + 打开文件夹；展开也封顶,不铺满。
             图片走上面的画廊,这里只收非图片产物;唯一一个且已被预览大卡呈现时省掉。 -->
        <div v-if="showFolderCard" class="folder-card">
          <div class="folder-head">
            <button
              class="folder-title-btn"
              @click="emit('toggle-files', turn.key)"
            >
              <FolderOpen :size="15" :stroke-width="1.7" class="folder-ico" />
              <span class="folder-title">本轮产物</span>
              <span class="folder-count">{{ docFiles.length }}</span>
              <ChevronDown
                :size="14"
                :stroke-width="2"
                class="folder-chev"
                :class="{ closed: filesCollapsed }"
              />
            </button>
            <button
              v-if="turn.folder"
              class="folder-open-btn"
              title="在文件管理器中打开文件夹"
              @click="emit('open-folder', turn.folder)"
            >
              <ExternalLink :size="13" :stroke-width="1.8" />
              打开文件夹
            </button>
          </div>
          <div v-if="!filesCollapsed" class="folder-body">
            <button
              v-for="a in inlineFiles"
              :key="a"
              class="file-row"
              :class="{ active: artifactsStore.current?.path === a }"
              :title="a"
              @click="emit('open-artifact', a)"
            >
              <component
                :is="artifactIcon(a)"
                :size="15"
                :stroke-width="1.7"
                class="fr-ico"
              />
              <span class="fr-name">{{ fileName(a) }}</span>
              <span v-if="fileExt(a)" class="fr-ext">{{ fileExt(a) }}</span>
              <ExternalLink :size="12" :stroke-width="1.8" class="fr-open" />
            </button>
            <button
              v-if="overflowCount > 0 && turn.folder"
              class="file-row overflow"
              @click="emit('open-folder', turn.folder)"
            >
              <FolderOpen :size="15" :stroke-width="1.7" class="fr-ico" />
              <span class="fr-name">还有 {{ overflowCount }} 个文件 · 打开文件夹查看全部</span>
              <ExternalLink :size="12" :stroke-width="1.8" class="fr-open" />
            </button>
          </div>
        </div>
      </div>

      <!-- 回答下方操作：复制 / 重新生成 / 时间 -->
      <div
        v-if="turn.hasAssistant && turn.text && !pending"
        class="turn-actions"
      >
        <button class="ta-btn" title="复制回答" @click="emit('copy', turn)">
          <Copy :size="13" :stroke-width="1.8" />
          <span>复制</span>
        </button>
        <button
          v-if="turn.user && !sending"
          class="ta-btn"
          title="用同样的问题再生成一次"
          @click="emit('regenerate', turn)"
        >
          <RotateCcw :size="13" :stroke-width="1.8" />
          <span>重新生成</span>
        </button>
        <span v-if="turn.at" class="ta-time">{{ fmtTime(turn.at) }}</span>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* ═══════════ 对话渲染 (Codex 式：纯对话，无头像) ═══════════
   样式自 ChatPanel.vue 原样搬入,保持原注释与级联顺序 */
.turn {
  max-width: 880px;
  margin: 0 auto 22px;
  animation: card-rise 0.32s var(--ease-out) both;
  /* 布局隔离(借鉴有戏剧场):单回合内容变化(流式追加)不再触发整列 layout 重算。
     不用 contain:paint/content-visibility —— 悬浮操作条会被裁剪、快滚会闪白。 */
  contain: layout style;
}
@media (prefers-reduced-motion: reduce) {
  .turn,
  .folder-card,
  .ref-files {
    animation: none;
  }
}

/* 用户：右对齐中性灰气泡，无头像 */
.msg.user {
  display: flex;
  justify-content: flex-end;
  align-items: center;
  gap: 8px;
  margin-bottom: 18px;
}
/* 编辑并重发(悬停气泡时浮现) */
.u-edit {
  width: 26px;
  height: 26px;
  border: none;
  border-radius: 7px;
  background: transparent;
  color: var(--muted);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  opacity: 0;
  transition: opacity 0.15s, background 0.15s;
  cursor: pointer;
  flex-shrink: 0;
}
.msg.user:hover .u-edit {
  opacity: 1;
}
.u-edit:hover {
  background: var(--bg-soft);
  color: var(--text);
}
.bubble-user {
  max-width: 82%;
  background: var(--bg-soft);
  border: 1px solid var(--border-soft);
  border-radius: 16px;
  padding: 9px 15px;
}
.u-text {
  white-space: pre-wrap;
  word-break: break-word;
  font-size: 13.5px;
  line-height: 1.65;
  color: var(--text);
}

/* 助手：纯文本，无头像无边框（Codex 式） */
.msg.ai {
  min-width: 0;
}

/* 工具调用 pill */
.tool-strip {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin-bottom: 10px;
}
.tool-pill {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 11px;
  color: var(--text-2);
  background: var(--bg-soft);
  border: 1px solid var(--border-soft);
  padding: 3px 9px;
  border-radius: 20px;
  cursor: default;
}
.tool-pill.clickable {
  cursor: pointer;
}
.tool-pill.clickable:hover,
.tool-pill.open {
  border-color: var(--primary);
  color: var(--primary-deep);
  background: var(--primary-soft);
}
.tool-pill :deep(svg) {
  color: var(--primary);
}
.tp-count {
  font-size: 10px;
  color: var(--muted);
}
/* 工具输入摘要(pill 点开) */
.tool-detail {
  margin: -4px 0 10px;
  padding: 8px 12px;
  border-radius: 9px;
  background: var(--bg-soft);
  border: 1px solid var(--border-soft);
}
.td-head {
  font-size: 10.5px;
  letter-spacing: 0.4px;
  color: var(--muted);
  margin-bottom: 4px;
}
.td-line {
  font-family: var(--mono);
  font-size: 11.5px;
  color: var(--text-2);
  padding: 1px 0;
  word-break: break-all;
}

/* 生成中三点 */
.typing {
  display: flex;
  gap: 4px;
  padding: 4px 0 2px;
}
.typing span {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--primary);
  opacity: 0.5;
  /* 游戏式弹跳: 顶点带 squash & stretch(压扁-拉伸)与光晕, 比匀速正弦更有"落地反弹"的实感 */
  animation: typing-bounce 1.1s cubic-bezier(0.36, 0, 0.64, 1) infinite;
}
.typing span:nth-child(2) {
  animation-delay: 0.15s;
}
.typing span:nth-child(3) {
  animation-delay: 0.3s;
}
@keyframes typing-bounce {
  0%,
  70%,
  100% {
    transform: translateY(0) scale(1, 0.92);
    opacity: 0.35;
    box-shadow: 0 0 0 rgba(0, 0, 0, 0);
  }
  35% {
    transform: translateY(-5px) scale(0.92, 1.1);
    opacity: 1;
    box-shadow: 0 2px 6px var(--primary-soft), 0 0 6px var(--primary-soft);
  }
  55% {
    transform: translateY(0) scale(1.15, 0.8);
    opacity: 0.7;
  }
}
@media (prefers-reduced-motion: reduce) {
  .typing span {
    animation: none;
    opacity: 0.6;
  }
}

.err-line {
  font-family: var(--mono);
  font-size: 12px;
  color: var(--vermilion);
  background: var(--vermilion-soft);
  border-radius: 6px;
  padding: 6px 10px;
  margin-top: 8px;
  white-space: pre-wrap;
  word-break: break-word;
}

/* 生成的文件：回答末尾 */
.files {
  margin-top: 12px;
  padding-top: 11px;
  border-top: 1px dashed var(--border);
}

/* 回答下方操作行（复制） —— 平时淡出，悬停回答时浮现 */
.turn-actions {
  margin-top: 10px;
  display: flex;
  gap: 6px;
  opacity: 0;
  transition: opacity 0.15s;
}
.msg.ai:hover .turn-actions {
  opacity: 1;
}
.ta-btn {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 4px 9px;
  border: 1px solid var(--border-soft);
  background: var(--panel);
  color: var(--muted);
  font-size: 11.5px;
  border-radius: 7px;
  transition: border-color 0.15s, color 0.15s, background 0.15s,
    transform 0.22s var(--ease-spring), box-shadow 0.22s var(--ease-out);
}
.ta-btn:hover {
  border-color: var(--border);
  color: var(--text);
  background: var(--bg-soft);
  transform: translateY(-1px);
  box-shadow: var(--shadow-sm);
}
.ta-time {
  align-self: center;
  font-size: 10.5px;
  color: var(--dim);
  margin-left: 4px;
}

/* ── markdown 正文排版 ── */
.md {
  font-size: 13.5px;
  line-height: 1.72;
  color: var(--text);
  word-break: break-word;
}
.md :deep(> *:first-child) {
  margin-top: 0;
}
.md :deep(> *:last-child) {
  margin-bottom: 0;
}
.md :deep(h1),
.md :deep(h2),
.md :deep(h3),
.md :deep(h4) {
  font-family: var(--serif);
  line-height: 1.35;
  margin: 1.15em 0 0.5em;
  color: var(--ink);
}
/* 结构化小节标题:左侧渐变光条,信息层级一眼可辨 */
.md :deep(h2),
.md :deep(h3) {
  position: relative;
  padding-left: 13px;
}
.md :deep(h2)::before,
.md :deep(h3)::before {
  content: "";
  position: absolute;
  left: 0;
  top: 0.16em;
  bottom: 0.16em;
  width: 3.5px;
  border-radius: 4px;
  background: linear-gradient(180deg, var(--primary), var(--primary-soft));
}
.md :deep(h1) {
  font-size: 1.5em;
}
.md :deep(h2) {
  font-size: 1.3em;
}
.md :deep(h3) {
  font-size: 1.12em;
}
.md :deep(h4) {
  font-size: 1em;
}
.md :deep(p) {
  margin: 0.55em 0;
}
.md :deep(ul),
.md :deep(ol) {
  margin: 0.55em 0;
  padding-left: 1.5em;
}
.md :deep(li) {
  margin: 0.25em 0;
}
.md :deep(li::marker) {
  color: var(--primary);
}
.md :deep(a) {
  color: var(--primary);
  text-decoration: none;
  border-bottom: 1px solid var(--primary-soft);
}
.md :deep(a:hover) {
  border-bottom-color: var(--primary);
}
.md :deep(strong) {
  color: var(--ink);
  font-weight: 600;
}
.md :deep(hr) {
  border: none;
  border-top: 1px solid var(--border);
  margin: 1.1em 0;
}
.md :deep(blockquote) {
  margin: 0.7em 0;
  padding: 0.4em 0.9em;
  border-left: 3px solid var(--primary);
  background: var(--primary-soft);
  border-radius: 0 6px 6px 0;
  color: var(--text-2);
}
.md :deep(blockquote p) {
  margin: 0.2em 0;
}
/* 行内代码 */
.md :deep(:not(pre) > code) {
  font-family: var(--mono);
  font-size: 0.88em;
  background: var(--code-bg);
  color: var(--primary-deep);
  padding: 0.12em 0.4em;
  border-radius: 5px;
  border: 1px solid var(--border-soft);
}
/* 代码块：深色卡片，横向滚动，盒绘对齐 */
.md :deep(pre) {
  background: #0f1b2d;
  color: #dbe6f5;
  border-radius: 10px;
  padding: 13px 15px;
  overflow-x: auto;
  margin: 0.8em 0;
  line-height: 1.55;
}
.md :deep(pre code) {
  font-family: var(--mono);
  font-size: 12.4px;
  background: none;
  border: none;
  padding: 0;
  color: inherit;
  white-space: pre;
}
/* 表格 */
.md :deep(table) {
  border-collapse: collapse;
  width: 100%;
  margin: 0.8em 0;
  font-size: 12.8px;
  display: block;
  overflow-x: auto;
}
.md :deep(th),
.md :deep(td) {
  border: 1px solid var(--border);
  padding: 6px 11px;
  text-align: left;
}
.md :deep(thead th) {
  background: var(--bg-soft);
  font-weight: 600;
  color: var(--text);
}
/* 正文内嵌图:卡片化(圆角 + 细边 + 浅影),与画廊缩略图同一气质 */
.md :deep(img) {
  max-width: 100%;
  border-radius: 10px;
  border: 1px solid var(--border-soft);
  box-shadow: var(--shadow-sm);
}

/* 成品文件 chips —— 回答末尾的可点击文件 */
/* ── 产物文件夹卡片(Kimi 式)：头部可折叠, 文件按行排列, 点行右侧预览 ── */
.folder-card {
  max-width: 420px;
  border: 1px solid var(--border-soft);
  border-radius: 10px;
  background: var(--panel);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.9), var(--shadow-sm);
  overflow: hidden;
  animation: card-rise 0.35s var(--ease-out) both;
  transition: box-shadow 0.25s var(--ease-out), border-color 0.25s;
}
.folder-card:hover {
  border-color: var(--border);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.9), var(--shadow);
}
@keyframes card-rise {
  from {
    opacity: 0;
    transform: translateY(6px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}
/* 深色下折叠卡阴影跟着翻转(原黑夜模式覆盖块的一员,其余覆盖随输入区搬去 ChatComposer) */
html[data-theme="dark"] .folder-card {
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.06), var(--shadow-sm);
}
/* ── 主预览大卡(Kimi 式)：本轮最该「打开看」的那个件, 单独醒目呈现 ── */
.preview-card {
  display: flex;
  align-items: center;
  gap: 11px;
  max-width: 420px;
  width: 100%;
  margin-bottom: 9px;
  padding: 11px 13px;
  border: 1px solid var(--border-soft);
  border-radius: 12px;
  background: linear-gradient(
    135deg,
    var(--primary-soft) 0%,
    var(--panel) 60%
  );
  cursor: pointer;
  text-align: left;
  animation: card-rise 0.35s var(--ease-out) both;
  transition: border-color 0.2s, box-shadow 0.22s var(--ease-out),
    transform 0.22s var(--ease-spring);
}
.preview-card:hover,
.preview-card.active {
  border-color: var(--primary);
  box-shadow: var(--shadow-sm);
  transform: translateY(-1px);
}
.pv-ico {
  flex-shrink: 0;
  width: 40px;
  height: 40px;
  border-radius: 9px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: var(--panel);
  border: 1px solid var(--border-soft);
  color: var(--primary);
}
.pv-meta {
  min-width: 0;
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.pv-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 13px;
  font-weight: 600;
  color: var(--ink);
}
.pv-sub {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 10.5px;
  color: var(--muted);
}
.pv-ext {
  padding: 0 5px;
  border-radius: 5px;
  background: var(--bg-soft);
  color: var(--muted);
  line-height: 15px;
  text-transform: uppercase;
  letter-spacing: 0.4px;
}
.pv-open {
  flex-shrink: 0;
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 5px 10px;
  border-radius: 8px;
  background: var(--primary);
  color: #fff;
  font-size: 11.5px;
  font-weight: 600;
}

.folder-head {
  display: flex;
  align-items: center;
  gap: 4px;
  width: 100%;
  padding: 3px 5px 3px 6px;
  font-size: 12px;
  color: var(--text);
}
.folder-title-btn {
  display: flex;
  align-items: center;
  gap: 7px;
  flex: 1;
  min-width: 0;
  padding: 5px 6px;
  border: none;
  border-radius: 7px;
  background: transparent;
  color: var(--text);
  font-size: 12px;
  cursor: pointer;
  text-align: left;
}
.folder-title-btn:hover {
  background: var(--bg-soft);
}
.folder-open-btn {
  flex-shrink: 0;
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 5px 9px;
  border: 1px solid var(--border-soft);
  border-radius: 7px;
  background: var(--panel);
  color: var(--muted);
  font-size: 11px;
  cursor: pointer;
  transition: color 0.12s, border-color 0.12s, background 0.12s;
}
.folder-open-btn:hover {
  color: var(--primary);
  border-color: var(--primary);
  background: var(--primary-soft);
}
.file-row.overflow .fr-name {
  color: var(--muted);
  font-weight: 500;
}
.folder-ico {
  color: var(--primary);
  flex-shrink: 0;
}
.folder-title {
  font-weight: 600;
  letter-spacing: 0.3px;
}
.folder-count {
  padding: 0 6px;
  border-radius: 8px;
  background: var(--primary-soft);
  color: var(--primary);
  font-size: 10.5px;
  line-height: 16px;
}
.folder-chev {
  margin-left: auto;
  color: var(--muted);
  transition: transform 0.15s;
}
.folder-chev.closed {
  transform: rotate(-90deg);
}
.folder-body {
  border-top: 1px solid var(--border-soft);
  padding: 4px;
  display: flex;
  flex-direction: column;
}
.file-row {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 6px 8px;
  border: none;
  border-radius: 7px;
  background: transparent;
  color: var(--text);
  font-size: 12.5px;
  cursor: pointer;
  text-align: left;
  transition: background 0.12s, color 0.12s,
    transform 0.22s var(--ease-spring);
}
.file-row:hover,
.file-row.active {
  background: var(--primary-soft);
  color: var(--primary);
  transform: translateX(2px);
}
.file-row .fr-ico {
  color: var(--primary);
  flex-shrink: 0;
}
.file-row .fr-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-weight: 500;
  min-width: 0;
}
.file-row .fr-ext {
  flex-shrink: 0;
  padding: 0 5px;
  border-radius: 5px;
  background: var(--bg-soft);
  color: var(--muted);
  font-size: 10px;
  line-height: 15px;
  text-transform: uppercase;
  letter-spacing: 0.4px;
}
.file-row .fr-open {
  margin-left: auto;
  flex-shrink: 0;
  opacity: 0;
  transition: opacity 0.12s;
}
.file-row:hover .fr-open,
.file-row.active .fr-open {
  opacity: 0.8;
}

/* ── TL;DR 速览行：回答开头一句话结论(renderMd 从正文摘出)。
   刻意低调(豆包式): 只是加粗一行 + 细虚线分隔, 不做彩色卡片。 ── */
.md :deep(.tldr) {
  display: flex;
  align-items: baseline;
  gap: 8px;
  margin: 0 0 10px;
  padding: 0 0 10px;
  border-bottom: 1px dashed var(--border-soft);
}
.md :deep(.tldr .tldr-tag) {
  flex-shrink: 0;
  padding: 0 5px;
  border: 1px solid var(--border-soft);
  border-radius: 5px;
  font-size: 9.5px;
  font-weight: 600;
  letter-spacing: 0.6px;
  line-height: 15px;
  color: var(--muted);
}
.md :deep(.tldr .tldr-body) {
  min-width: 0;
  font-size: 13.5px;
  font-weight: 600;
  line-height: 1.6;
}
.md :deep(.tldr .tldr-body p) {
  margin: 0;
  display: inline;
}

/* ── 参考文件胶囊：回答最前面一行小 pill, 点开右侧预览 ── */
.ref-files {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 6px;
  margin-bottom: 9px;
  animation: card-rise 0.3s var(--ease-out) both;
}
.ref-label {
  font-size: 10.5px;
  color: var(--dim);
  letter-spacing: 0.3px;
  margin-right: 2px;
}
.ref-pill {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  max-width: 200px;
  padding: 2px 8px;
  border: 1px solid var(--border-soft);
  border-radius: 999px;
  background: var(--bg-soft);
  color: var(--muted);
  font-size: 11px;
  cursor: pointer;
  transition: color 0.12s, border-color 0.12s, background 0.12s,
    transform 0.22s var(--ease-spring), box-shadow 0.22s var(--ease-out);
}
.ref-pill:hover {
  color: var(--primary);
  border-color: var(--primary);
  background: var(--primary-soft);
  transform: translateY(-1px);
  box-shadow: var(--shadow-sm);
}
.ref-pill .ref-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* ─────────── 附件 chips(消息气泡内的只读款;可编辑款随输入区在 ChatComposer) ─────────── */
.attach-chips {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin-bottom: 8px;
}
.attach-chips.in-bubble {
  margin-top: 8px;
  margin-bottom: 0;
}
.attach-chip {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  max-width: 260px;
  padding: 4px 8px;
  background: var(--bg-soft);
  border: 1px solid var(--border);
  border-radius: 8px;
  font-size: 12px;
  color: var(--text-2);
}
.attach-chip .ac-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-weight: 500;
  color: var(--text);
}
.attach-chip.readonly {
  background: transparent;
  color: var(--primary-deep);
}
</style>
