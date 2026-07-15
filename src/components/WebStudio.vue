<script setup lang="ts">
import { ref, computed, watch, onMounted } from "vue";
import { usePolling } from "../composables/usePolling";
import {
  Globe,
  FileText,
  Loader,
  Sparkles,
  Upload,
  X,
  Eye,
  FolderOpen,
  ExternalLink,
  Zap,
  Wand2,
  Type,
  RefreshCw,
} from "@lucide/vue";
import { useAppStore } from "../stores/app";
import { useChatStore } from "../stores/chat";
import { artifacts as artifactsApi, chat as chatApi, skills as skillsApi, type AttachedFile, type Skill } from "../tauri";
import { useFileDrop } from "../composables/useFileDrop";
import { groupedThemes, findTheme, type DeckTheme } from "../lib/deckThemes";

// KeepAlive 的 include 按组件 name 匹配 → 显式命名:切走再回来「继续修改」状态不丢
defineOptions({ name: "WebStudio" });

const app = useAppStore();
const chat = useChatStore();

const STUDIO_PROJECT_NAME = "网站工坊";
const VIEW_KEY = "web_studio";

type Phase = "config" | "generating" | "done";
const phase = ref<Phase>("config");
const error = ref<string | null>(null);
const convId = ref<string | null>(null);
const lastAction = ref<"create" | "revise">("create");

// ───────── 配置 ─────────
// 品牌名记住上次填写(重开/重置不用再敲)
const BRAND_KEY = "polaris.webstudio.brand.v1";
const brandName = ref(localStorage.getItem(BRAND_KEY) ?? "");
watch(brandName, (v) => {
  try {
    localStorage.setItem(BRAND_KEY, v);
  } catch {
    /* storage 不可用就算了 */
  }
});
const contentText = ref("");
const charCount = computed(() => contentText.value.length);
const uploads = ref<AttachedFile[]>([]);
const uploading = ref(false);

const selectedTheme = ref("auto"); // corporate-clean 等素白模板已从主题库下架,默认交给 AI 自由发挥
const groups = groupedThemes(true);
const curTheme = computed<DeckTheme>(() => findTheme(selectedTheme.value));

type SiteType = "landing" | "product" | "portfolio" | "blog" | "event";
const SITE_TYPES: { id: SiteType; label: string; hint: string }[] = [
  { id: "landing", label: "产品落地页", hint: "Hero + 功能 + 价格 + CTA，转化导向" },
  { id: "product", label: "SaaS 介绍", hint: "功能 bento + 数据 + 价格表 + FAQ" },
  { id: "portfolio", label: "个人作品集", hint: "简介 + 项目网格 + 经历 + 联系" },
  { id: "blog", label: "博客首页", hint: "文章卡片流 + 分类 + 订阅" },
  { id: "event", label: "活动页", hint: "主视觉 + 日程 + 嘉宾 + 报名 CTA" },
];
const siteType = ref<SiteType>("landing");
const siteTypeHint = computed(() => SITE_TYPES.find((s) => s.id === siteType.value)?.hint ?? "");

// 自定义风格：在所选主题基础上叠加
const customStyle = ref("");

// 可叠加的「增强技能」——与对话框同源:list_skills 全量技能库,点选后随对话一起注入。
// polaris-web-studio 本体恒注入,不在列表里重复展示。
const FALLBACK_SKILLS: Skill[] = [
  { id: "deep-research", name: "深度搜索", description: "先联网研究、把内容/文案补全", source: "official" },
  { id: "image-gen", name: "AI 配图", description: "为网站生成插图/配图", source: "official" },
  { id: "pdf", name: "读 PDF", description: "解析上传的 PDF 素材", source: "official" },
];
const skillsList = ref<Skill[]>([]);
const skillSearch = ref("");
async function loadSkills() {
  try {
    skillsList.value = await skillsApi.list();
  } catch {
    skillsList.value = FALLBACK_SKILLS;
  }
}
onMounted(loadSkills);
function filteredSkills(): Skill[] {
  const base = skillsList.value.filter((s) => s.id !== "polaris-web-studio");
  const q = skillSearch.value.trim().toLowerCase();
  if (!q) return base;
  return base.filter((s) => s.name.toLowerCase().includes(q) || s.description.toLowerCase().includes(q));
}
const extraSkills = ref<string[]>([]);
function toggleSkill(id: string) {
  const i = extraSkills.value.indexOf(id);
  if (i >= 0) extraSkills.value.splice(i, 1);
  else extraSkills.value.push(id);
}
const skillIds = computed(() => ["polaris-web-studio", ...extraSkills.value]);

const canGenerate = computed(
  () => (contentText.value.trim().length >= 10 || uploads.value.length > 0) && phase.value !== "generating"
);

// ───────── 上传 ─────────
async function addPaths(paths: string[]) {
  if (!paths.length) return;
  uploading.value = true;
  error.value = null;
  try {
    const res = await chatApi.attachFiles(convId.value ?? undefined, paths);
    for (const r of res) {
      if (r.ok && !uploads.value.some((u) => u.path === r.path)) uploads.value.push(r);
    }
  } catch (e: any) {
    error.value = e?.message ?? String(e);
  } finally {
    uploading.value = false;
  }
}
async function pickFiles() {
  try {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const sel = await open({
      multiple: true,
      filters: [{ name: "素材", extensions: ["md", "txt", "docx", "pdf", "html", "json", "csv"] }],
    });
    if (!sel) return;
    await addPaths(Array.isArray(sel) ? sel : [sel]);
  } catch (e: any) {
    error.value = e?.message ?? String(e);
  }
}
function removeUpload(i: number) {
  uploads.value.splice(i, 1);
}
const { isOver: dropOver } = useFileDrop({
  active: () => app.view === VIEW_KEY && phase.value !== "generating",
  onDrop: addPaths,
});

// ───────── prompt ─────────
function buildPrompt(): string {
  const themeLine =
    selectedTheme.value === "auto"
      ? "AI 自由发挥 —— 由你根据内容气质自挑最贴合的主题（从 skill 的 data-theme 主题库里选，或在其上自行配色），" +
        "必须基于 polaris-web-studio(open-design) 的主题体系制作，观感必须高级：讲究的版式层级、克制的配色、超大标题与留白，拒绝平庸的默认观感"
      : `${curTheme.value.name}（data-theme id=${selectedTheme.value}）`;
  const st = SITE_TYPES.find((s) => s.id === siteType.value)!;
  const lines = [
    "请使用 polaris-web-studio skill 制作一个网站。",
    "",
    "## 网站配置",
    `- 站点类型：${st.label}（${siteType.value}）—— ${st.hint}`,
    `- 主题：${themeLine}`,
    brandName.value.trim() ? `- 品牌名：${brandName.value.trim()}` : "- 品牌名：从内容里提炼一个",
  ];
  if (customStyle.value.trim()) {
    lines.push(`- 自定义风格补充：${customStyle.value.trim()}（在所选主题基础上按此调整，冲突时以此为准）`);
  }
  if (extraSkills.value.length) {
    const names = skillsList.value
      .filter((s) => extraSkills.value.includes(s.id))
      .map((s) => s.name)
      .join("、") || extraSkills.value.join("、");
    lines.push(`- 已启用增强技能：${names}——制作时按需调用（如先研究补全内容、为网站配图、解析素材）。`);
  }
  if (uploads.value.length) {
    lines.push("", "## 素材文件（先 Read 它们作为内容来源）");
    for (const u of uploads.value) lines.push(`- ${u.path}`);
  }
  lines.push("", "## 需求 / 内容");
  lines.push(contentText.value.trim() || "（见上方素材文件）");
  lines.push(
    "",
    "## 要求",
    "- 严格按 SKILL.md：用 site.css 组件 + 选定主题写一个**响应式**单页站点；把 site.css + themes.css 内联进 <style>、runtime.js 内联进 <script>，产出**自包含** .html 存到产物目录。",
    "- 文案要具体有信息量，别用占位 Lorem；配图用 emoji / CSS 渐变 / inline SVG，不要外链不存在的图。",
    "- 回答末尾用**绝对路径**列出最终 .html。",
  );
  return lines.join("\n");
}
function revisePrompt(text: string): string {
  return [
    "对刚才生成的这个网站做如下修改：",
    "",
    text.trim(),
    "",
    "## 要求",
    "- 直接在**原 .html 文件上修改**（文件名不变），改完覆盖保存；保持响应式与自包含。",
    "- 回答末尾用绝对路径列出更新后的 .html。",
  ].join("\n");
}

// ───────── 动作 ─────────
async function ensureConv(): Promise<string> {
  let project = app.projects.find((p) => p.name === STUDIO_PROJECT_NAME);
  let projectId: string | null = project?.id ?? null;
  if (!projectId) {
    await app.createProject(STUDIO_PROJECT_NAME);
    projectId = app.currentProjectId;
    if (!projectId) throw new Error("创建网站工坊项目失败");
  }
  // navigate=false: 留在网站工坊视图就地展示进度/预览, 不跳 chat(否则本组件被卸载)。
  const conv = await app.createConversation(projectId, false);
  return conv.id;
}
function preview(): string {
  if (brandName.value.trim()) return brandName.value.trim();
  const t = contentText.value.trim();
  if (t) return t.slice(0, 22) + (t.length > 22 ? "…" : "");
  if (uploads.value.length) return uploads.value[0].name;
  return "未命名";
}

async function start() {
  if (!canGenerate.value) return;
  error.value = null;
  try {
    const id = await ensureConv();
    convId.value = id;
    if (uploads.value.length) {
      try {
        const res = await chatApi.attachFiles(id, uploads.value.map((u) => u.path));
        uploads.value = res.filter((r) => r.ok);
      } catch {
        /* 已在目录则忽略 */
      }
    }
    lastAction.value = "create";
    phase.value = "generating";
    const display = `🌐 网站·${curTheme.value.name}：${preview()}`;
    await chat.send(id, buildPrompt(), display, undefined, {
      permissionMode: "auto_current",
      skillIds: skillIds.value,
      goal: `制作一个「${curTheme.value.name}」主题的${SITE_TYPES.find((s) => s.id === siteType.value)?.label}网站(.html)并保存到产物目录`,
    });
  } catch (e: any) {
    error.value = e?.message ?? String(e);
    phase.value = hasResult.value ? "done" : "config";
  }
}

const reviseText = ref("");
async function revise() {
  const text = reviseText.value.trim();
  if (!text || !convId.value) return;
  error.value = null;
  try {
    lastAction.value = "revise";
    phase.value = "generating";
    await chat.send(convId.value, revisePrompt(text), `✏️ 修改网站：${text.slice(0, 20)}`, undefined, {
      permissionMode: "auto_current",
      skillIds: skillIds.value,
      goal: "按要求修改已生成的网站并覆盖更新 .html",
    });
    reviseText.value = "";
  } catch (e: any) {
    error.value = e?.message ?? String(e);
    phase.value = "done";
  }
}

function reset() {
  phase.value = "config";
  convId.value = null;
  outputs.value = [];
  previewHtml.value = "";
  reviseText.value = "";
}

// ───────── 产物 + 实时预览 ─────────
const sending = computed(() => chat.isSending(convId.value));
// 生成遮罩上的「现在在干嘛」:取对话流最近一次工具调用(纯展示)
const lastToolHint = computed(() => {
  const arr = chat.bubblesFor(convId.value);
  for (let i = arr.length - 1; i >= 0; i--) {
    if (arr[i].role === "tool") return arr[i].toolDetail || arr[i].tool || "";
  }
  return "";
});
const outputs = ref<{ path: string; name: string }[]>([]);
const hasResult = computed(() => outputs.value.length > 0);
const previewHtml = ref<string>("");
const previewPath = ref<string>("");

async function loadOutputs() {
  if (!convId.value) return;
  try {
    const list = await artifactsApi.list(convId.value);
    outputs.value = list
      .filter((e) => /\.html?$/i.test(e.name))
      .map((e) => ({ path: e.path, name: e.name }));
    await loadPreview();
  } catch {
    /* ignore */
  }
}
// 修改是覆盖写原文件(文件名不变)→ 不能按路径短路,要重读内容、真变了才换 srcdoc
async function loadPreview() {
  const htmlOut = outputs.value[0];
  if (!htmlOut) return;
  try {
    const p = await artifactsApi.read(htmlOut.path);
    if (p?.text && (p.text !== previewHtml.value || htmlOut.path !== previewPath.value)) {
      previewHtml.value = p.text;
      previewPath.value = htmlOut.path;
    }
  } catch {
    /* ignore */
  }
}

watch(sending, async (now, before) => {
  if (before && !now && phase.value === "generating") {
    await loadOutputs();
    phase.value = "done";
  }
});
// 共享轮询:页面隐藏自动暂停、回前台立即补拉、卸载自动清理
const poller = usePolling(loadOutputs, 4000);
watch(phase, (p) => {
  if (p === "generating") poller.start();
  else poller.stop();
});

function openConv() {
  if (convId.value) app.setView("chat");
}
function openDir() {
  const proj = app.projects.find((p) => p.name === STUDIO_PROJECT_NAME);
  if (proj) app.openProjectDir(proj.id);
}
function openFile(path: string) {
  artifactsApi.openExternal(path);
}
function fillDemo() {
  brandName.value = "北极星 Polaris";
  contentText.value =
    "为 Polaris 做一个产品落地页。主张：把 AI 变成你的创作生产线。" +
    "三个卖点：① 对话即创作，文案/PPT/网站/视频一站出；② 知识库沉淀，越用越懂你；③ 全本地，数据不出门。" +
    "数据：10k+ 用户、99.9% 可用、4.9 星。价格：免费 / ¥39 月 / 团队定制。结尾 CTA：免费开始。";
}
</script>

<template>
  <div class="wb">
    <header class="wb-head">
      <Globe :size="19" :stroke-width="1.7" class="wb-icon" />
      <h1 class="wb-title">网站生成</h1>
      <span class="wb-sub">左侧配置 · 中间实时预览 · 底部继续修改</span>
    </header>

    <div class="wb-work">
      <!-- 左：配置 -->
      <aside class="wb-side">
        <div class="wb-side-sec">
          <div class="wb-side-title">站点类型</div>
          <div class="wb-types">
            <button v-for="s in SITE_TYPES" :key="s.id" class="wb-type" :class="{ on: siteType === s.id }" @click="siteType = s.id">{{ s.label }}</button>
          </div>
          <span class="wb-note">{{ siteTypeHint }}</span>
        </div>

        <div class="wb-side-sec">
          <div class="wb-side-title">品牌名</div>
          <div class="wb-brand"><Type :size="13" /><input v-model="brandName" class="wb-input" placeholder="可留空，AI 提炼" /></div>
        </div>

        <div class="wb-side-sec">
          <div class="wb-side-title">主题风格</div>
          <div class="wb-preview-mini" :style="{ background: curTheme.bg, color: curTheme.text }">
            <span :style="{ color: curTheme.accent, fontFamily: curTheme.font === 'serif' ? 'var(--serif)' : 'inherit' }">{{ curTheme.name }}</span>
          </div>
          <template v-for="g in groups" :key="g.group">
            <div class="wb-group-label">{{ g.group }}</div>
            <div class="wb-themes">
              <button v-for="t in g.items" :key="t.id" class="wb-theme" :class="{ active: selectedTheme === t.id }" :title="t.name" @click="selectedTheme = t.id">
                <span class="wb-theme-sw" :style="{ background: t.bg }">
                  <Sparkles v-if="t.id === 'auto'" :size="12" :style="{ color: t.accent }" />
                  <span v-else class="wb-theme-acc" :style="{ background: t.accent }"></span>
                </span>
                <span class="wb-theme-name">{{ t.name }}</span>
              </button>
            </div>
          </template>
        </div>

        <div class="wb-side-sec">
          <div class="wb-side-title">自定义风格 · 可选</div>
          <textarea
            v-model="customStyle"
            class="wb-custom"
            rows="2"
            placeholder="用自己的话补充风格：如「科技深色、玻璃拟物、霓虹强调」「杂志感、衬线大标题、留白」…叠加在所选主题上"
          />
        </div>

        <div class="wb-side-sec">
          <div class="wb-side-title">增强技能 · 可选</div>
          <input v-model="skillSearch" class="wb-skill-search" type="text" placeholder="搜索技能…" />
          <div class="wb-skill-list">
            <button
              v-for="s in filteredSkills()"
              :key="s.id"
              class="wb-skill-item"
              :class="{ on: extraSkills.includes(s.id) }"
              :title="s.description"
              @click="toggleSkill(s.id)"
            >
              <span class="wb-skill-name">{{ s.name }}</span>
              <span class="wb-skill-desc">{{ s.description }}</span>
            </button>
            <span v-if="!filteredSkills().length" class="wb-note">没有匹配的技能</span>
          </div>
          <span class="wb-note">与对话框同一个技能库。点选叠加，AI 制作时会按需调用（如先联网把内容补全、为网站配图）。</span>
        </div>

        <div v-if="hasResult" class="wb-side-sec">
          <div class="wb-side-title">产物</div>
          <button v-for="o in outputs" :key="o.path" class="wb-out" @click="openFile(o.path)">
            <Globe :size="13" /><span>{{ o.name }}</span><ExternalLink :size="11" />
          </button>
          <div class="wb-side-acts">
            <button class="wb-ghost" @click="openDir"><FolderOpen :size="12" /> 目录</button>
            <button class="wb-ghost" @click="openConv"><Eye :size="12" /> 对话</button>
            <button class="wb-ghost" @click="reset"><RefreshCw :size="12" /> 重来</button>
          </div>
        </div>
      </aside>

      <!-- 右：主区 + composer -->
      <main class="wb-main">
        <div class="wb-canvas" :class="{ drop: dropOver }">
          <div v-if="!hasResult" class="wb-input">
            <h3 class="wb-input-title"><FileText :size="16" :stroke-width="1.7" /> 需求 / 内容</h3>
            <textarea
              v-model="contentText"
              class="wb-textarea"
              placeholder="描述你要的网站：做给谁、主张是什么、要哪些板块/数据/价格，或上传文件作为素材，然后点下方「生成」…"
            />
            <div class="wb-input-foot">
              <span :class="{ warn: charCount < 10 && uploads.length === 0 }">
                {{ charCount }} 字{{ charCount < 10 && uploads.length === 0 ? " · 至少 10 字或上传文件" : "" }}
              </span>
              <div class="wb-input-btns">
                <button class="wb-ghost" @click="fillDemo">填入示例</button>
                <button class="wb-ghost" :disabled="uploading" @click="pickFiles">
                  <Loader v-if="uploading" :size="12" class="spin" /><Upload v-else :size="12" /> 上传
                </button>
              </div>
            </div>
            <div v-if="uploads.length" class="wb-files">
              <div v-for="(u, i) in uploads" :key="u.path" class="wb-file">
                <FileText :size="12" /><span class="wb-file-name">{{ u.name }}</span>
                <button class="wb-file-x" @click="removeUpload(i)"><X :size="12" /></button>
              </div>
            </div>
          </div>

          <div v-else class="wb-preview">
            <!-- 安全: 只给 allow-scripts, 不加 allow-same-origin(否则 srcdoc 脚本可自拆沙箱触达后端)。 -->
            <iframe v-if="previewHtml" class="wb-frame" :srcdoc="previewHtml" sandbox="allow-scripts"></iframe>
            <div v-else class="wb-frame-empty">
              <Globe :size="30" />
              <span>{{ phase === 'generating' ? '预览加载中…可在对话或目录查看' : '预览没有加载出来' }}</span>
              <button v-if="phase !== 'generating'" class="wb-ghost" @click="loadOutputs">重新加载预览</button>
            </div>
          </div>

          <div v-if="phase === 'generating'" class="wb-overlay">
            <Loader :size="30" class="spin" />
            <span>{{ lastAction === 'revise' ? '正在按修改重做…' : '正在制作网站…' }}</span>
            <span v-if="lastToolHint" class="wb-tool-hint">{{ lastToolHint }}</span>
            <button class="wb-ghost" @click="openConv">在对话里看进度 →</button>
          </div>
        </div>

        <div class="wb-composer">
          <div v-if="error" class="wb-error">{{ error }}</div>
          <template v-if="!hasResult">
            <button class="wb-primary" :disabled="!canGenerate || phase === 'generating'" @click="start">
              <Zap :size="16" :stroke-width="1.9" /> 一键生成网站
            </button>
            <span class="wb-note">在「网站工坊」项目下新建对话注入技能全自动制作。</span>
          </template>
          <template v-else>
            <Wand2 :size="16" :stroke-width="1.7" class="wb-comp-i" />
            <textarea
              v-model="reviseText"
              class="wb-comp-input"
              rows="1"
              placeholder="继续修改：换东京夜深色主题 / 价格改两档 / 加一段 FAQ / Hero 文案改成『…』 / 加嘉宾板块…"
              @keydown.enter.exact.prevent="revise"
            />
            <button class="wb-primary sm" :disabled="!reviseText.trim() || phase === 'generating'" @click="revise">
              <Wand2 :size="14" /> 应用修改
            </button>
          </template>
        </div>
      </main>
    </div>
  </div>
</template>

<style scoped>
.wb { height: 100%; display: flex; flex-direction: column; overflow: hidden; background: var(--bg); }
.wb-head { display: flex; align-items: center; gap: 10px; padding: 12px 20px; border-bottom: 1px solid var(--border-soft); background: var(--panel); }
.wb-icon { color: var(--primary); }
.wb-title { font-family: var(--serif); font-size: 16px; font-weight: 600; color: var(--text); }
.wb-sub { font-size: 12px; color: var(--muted); margin-left: 4px; }

.wb-work { flex: 1; display: grid; grid-template-columns: 252px 1fr; overflow: hidden; }
@media (max-width: 820px) { .wb-work { grid-template-columns: 200px 1fr; } }

.wb-side { overflow-y: auto; border-right: 1px solid var(--border-soft); padding: 14px; display: flex; flex-direction: column; gap: 18px; background: var(--bg-soft); }
.wb-side-sec { display: flex; flex-direction: column; gap: 8px; }
.wb-side-title { font-size: 11px; font-weight: 700; letter-spacing: .1em; text-transform: uppercase; color: var(--dim); }
.wb-types { display: flex; flex-wrap: wrap; gap: 5px; }
.wb-type { padding: 6px 11px; border: 1px solid var(--border); border-radius: 999px; background: var(--bg); color: var(--text-2); font-size: 11.5px; cursor: pointer; }
.wb-type.on { border-color: var(--primary); background: var(--primary-soft); color: var(--primary-deep); font-weight: 600; }
.wb-note { font-size: 10.5px; color: var(--muted); line-height: 1.5; }
.wb-custom { resize: none; padding: 8px 10px; border: 1px solid var(--border); border-radius: 7px; background: var(--bg); color: var(--text); font-size: 11.5px; line-height: 1.5; }
.wb-custom:focus { outline: none; border-color: var(--primary); }
.wb-skill-search { padding: 6px 9px; border: 1px solid var(--border); border-radius: 7px; background: var(--bg); color: var(--text); font-size: 11.5px; }
.wb-skill-search:focus { outline: none; border-color: var(--primary); }
.wb-skill-list { display: flex; flex-direction: column; gap: 5px; max-height: 220px; overflow-y: auto; }
.wb-skill-item { display: flex; flex-direction: column; gap: 2px; padding: 6px 9px; border: 1px solid var(--border); border-radius: 7px; background: var(--bg); cursor: pointer; text-align: left; }
.wb-skill-item:hover { border-color: var(--primary); }
.wb-skill-item.on { border-color: var(--primary); background: var(--primary-soft); }
.wb-skill-name { font-size: 11.5px; font-weight: 600; color: var(--text-2); }
.wb-skill-item.on .wb-skill-name { color: var(--primary-deep); }
.wb-skill-desc { font-size: 10px; color: var(--muted); line-height: 1.4; display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
.wb-brand { display: flex; align-items: center; gap: 7px; color: var(--muted); }
.wb-input { flex: 1; padding: 7px 10px; border: 1px solid var(--border); border-radius: 7px; background: var(--bg); color: var(--text); font-size: 12.5px; }
.wb-input:focus { outline: none; border-color: var(--primary); }

.wb-preview-mini { height: 48px; border-radius: 8px; border: 1px solid var(--border); display: flex; align-items: center; padding: 0 12px; font-size: 13px; font-weight: 800; }
.wb-group-label { font-size: 10.5px; color: var(--dim); margin-top: 2px; }
.wb-themes { display: grid; grid-template-columns: 1fr 1fr; gap: 6px; }
.wb-theme { display: flex; align-items: center; gap: 6px; padding: 5px 6px; border: 1px solid var(--border); border-radius: 7px; background: var(--bg); cursor: pointer; text-align: left; }
.wb-theme:hover { border-color: var(--primary); }
.wb-theme.active { border-color: var(--primary); background: var(--primary-soft); }
.wb-theme-sw { width: 20px; height: 20px; border-radius: 5px; flex-shrink: 0; border: 1px solid rgba(0,0,0,.08); position: relative; overflow: hidden; display: flex; align-items: center; justify-content: center; }
.wb-theme-acc { position: absolute; bottom: 0; left: 0; right: 0; height: 38%; }
.wb-theme-name { font-size: 11px; color: var(--text-2); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

.wb-out { display: flex; align-items: center; gap: 6px; padding: 7px 9px; border: 1px solid var(--primary); border-radius: 7px; background: var(--primary-soft); color: var(--primary-deep); font-size: 11.5px; font-weight: 600; cursor: pointer; }
.wb-out span { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.wb-side-acts { display: flex; gap: 5px; margin-top: 4px; }
.wb-ghost { display: inline-flex; align-items: center; gap: 4px; padding: 6px 9px; border: 1px solid var(--border); border-radius: 6px; background: transparent; color: var(--text-2); font-size: 11.5px; cursor: pointer; transition: border-color .15s, color .15s; }
.wb-ghost:hover:not(:disabled) { border-color: var(--primary); color: var(--primary); }
.wb-ghost:disabled { opacity: .5; cursor: default; }

.wb-main { display: flex; flex-direction: column; overflow: hidden; position: relative; }
.wb-canvas { flex: 1; overflow: auto; position: relative; padding: 18px; display: flex; }
.wb-canvas.drop { outline: 2px dashed var(--primary); outline-offset: -10px; }

.wb-input { flex: 1; display: flex; flex-direction: column; gap: 10px; max-width: 860px; margin: 0 auto; width: 100%; }
.wb-input-title { display: inline-flex; align-items: center; gap: 7px; font-size: 14px; font-weight: 600; color: var(--text); margin: 0; }
.wb-textarea { flex: 1; min-height: 300px; resize: none; padding: 14px 16px; border: 1px solid var(--border); border-radius: 10px; background: var(--panel); color: var(--text); font-size: 14px; line-height: 1.75; }
.wb-textarea:focus { outline: none; border-color: var(--primary); }
.wb-input-foot { display: flex; align-items: center; justify-content: space-between; font-size: 12px; color: var(--muted); }
.wb-input-foot .warn { color: var(--vermilion); }
.wb-input-btns { display: flex; gap: 6px; }
.wb-files { display: flex; flex-wrap: wrap; gap: 6px; }
.wb-file { display: flex; align-items: center; gap: 5px; padding: 4px 8px; background: var(--bg-soft); border-radius: 6px; font-size: 11.5px; color: var(--text-2); }
.wb-file-name { max-width: 180px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.wb-file-x { border: none; background: transparent; color: var(--muted); cursor: pointer; display: inline-flex; padding: 1px; }
.wb-file-x:hover { color: var(--vermilion); }

.wb-preview { flex: 1; display: flex; min-height: 0; }
.wb-frame { flex: 1; width: 100%; border: 1px solid var(--border); border-radius: 10px; background: #fff; box-shadow: var(--shadow, 0 6px 24px rgba(0,0,0,.08)); }
.wb-frame-empty { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 10px; color: var(--muted); border: 1px dashed var(--border); border-radius: 10px; }

.wb-overlay { position: absolute; inset: 18px; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 12px; background: color-mix(in srgb, var(--bg) 78%, transparent); backdrop-filter: blur(2px); border-radius: 10px; color: var(--text); font-size: 14px; font-weight: 600; }
.wb-tool-hint { max-width: 80%; font-family: var(--mono); font-size: 11px; font-weight: 400; color: var(--muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

.wb-composer { border-top: 1px solid var(--border-soft); background: var(--panel); padding: 12px 18px; display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.wb-comp-i { color: var(--primary); flex-shrink: 0; }
.wb-comp-input { flex: 1; min-width: 200px; resize: none; padding: 10px 12px; border: 1px solid var(--border); border-radius: 9px; background: var(--bg); color: var(--text); font-size: 13px; line-height: 1.5; max-height: 110px; }
.wb-comp-input:focus { outline: none; border-color: var(--primary); }
.wb-primary { display: inline-flex; align-items: center; justify-content: center; gap: 8px; padding: 11px 26px; border: none; border-radius: 10px; background: var(--primary); color: #fff; font-size: 14px; font-weight: 600; cursor: pointer; transition: filter .15s; }
.wb-primary.sm { padding: 10px 18px; font-size: 13px; flex-shrink: 0; }
.wb-primary:hover:not(:disabled) { filter: brightness(1.07); }
.wb-primary:disabled { opacity: .5; cursor: default; }
.wb-error { flex-basis: 100%; padding: 8px 11px; border-radius: 8px; background: var(--vermilion-soft); color: var(--vermilion); font-size: 12px; }

.spin { animation: wb-spin .9s linear infinite; }
@keyframes wb-spin { to { transform: rotate(360deg); } }
</style>
