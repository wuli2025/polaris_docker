// ChatPanel 拆分(src/components/chat/)后共享的类型与纯函数 —— 逻辑原样搬移自 ChatPanel.vue
import {
  FolderOpen,
  FileCode,
  Image as ImageIcon,
  Clapperboard,
  AudioLines,
  Table,
  FileText,
  File as FileIcon,
} from "@lucide/vue";
import { renderMarkdown, mdVersion } from "../../lib/markdown";
import {
  artifacts as artifactsApi,
  isTauri,
  backendFileUrl,
  type PermissionMode,
} from "../../tauri";
import type { Bubble } from "../../stores/chat";

export function fileName(path: string): string {
  return path.replace(/\/+$/, "").split("/").pop() || path;
}

export function fileExt(path: string): string {
  const n = fileName(path);
  const i = n.lastIndexOf(".");
  return i >= 0 ? n.slice(i + 1).toLowerCase() : "";
}

/** 尾随 `/` = 后端归并上报的「应用文件夹」产物（整个应用一个 chip） */
export function isFolderArtifact(path: string): boolean {
  return path.endsWith("/");
}

// ── Kimi 式「一文件一预览」：本轮产物里挑一个「主可打开件」当预览，其余收进文件夹入口 ──
// 预览优先级(数字小=更该当主预览):能在右抽屉里「打开看」的排前面,零碎数据文件垫底。
// html/htm 最优(前后端联动或纯前端页面),其次演示 spec / pdf / 图片 / 视频 / office / 文档。
const PREVIEW_RANK: Record<string, number> = {
  html: 0,
  htm: 0,
  svg: 2,
  pdf: 3,
  png: 4,
  jpg: 4,
  jpeg: 4,
  gif: 4,
  webp: 4,
  avif: 4,
  mp4: 5,
  mov: 5,
  webm: 5,
  pptx: 6,
  docx: 6,
  xlsx: 6,
  md: 7,
  markdown: 7,
  txt: 8,
  csv: 8,
};
/** 从本轮产物里选出「主预览件」:排除文件夹产物,按 PREVIEW_RANK 取最优;
 *  演示 spec(polaris.slides.json)当作最高优先——它就是要在右抽屉自动播放的那个。 */
export function pickPreview(arts: string[]): string | undefined {
  let best: string | undefined;
  let bestRank = Infinity;
  for (const a of arts) {
    if (isFolderArtifact(a)) continue;
    const rank = /polaris\.slides\.json$/i.test(a)
      ? -1
      : PREVIEW_RANK[fileExt(a)] ?? 50;
    if (rank < bestRank) {
      bestRank = rank;
      best = a;
    }
  }
  return best;
}
/** 本轮产物的「打开文件夹」目标:有后端归并的文件夹产物就用它,否则取所有文件的最长公共目录。
 *  返回不带尾随 `/` 的目录路径;取不到则 undefined(不显示文件夹入口)。 */
export function commonFolder(arts: string[]): string | undefined {
  const folder = arts.find(isFolderArtifact);
  if (folder) return folder.replace(/\/+$/, "");
  const files = arts
    .filter((a) => !isFolderArtifact(a))
    .map((a) => a.replace(/\\/g, "/"));
  if (!files.length) return undefined;
  const dirs = files.map((f) => {
    const i = f.lastIndexOf("/");
    return i >= 0 ? f.slice(0, i) : "";
  });
  // 按路径段求最长公共前缀
  let prefix = dirs[0].split("/");
  for (let i = 1; i < dirs.length; i++) {
    const segs = dirs[i].split("/");
    let j = 0;
    while (j < prefix.length && j < segs.length && prefix[j] === segs[j]) j++;
    prefix = prefix.slice(0, j);
  }
  const dir = prefix.join("/");
  return dir || undefined;
}

// ── 对话内横排图片画廊(LUMI 式一排缩略图) ──
// 只认位图:svg 桌面端 artifact_read 走文本通道不出 dataUrl,继续按普通文件走卡片。
const STRIP_IMAGE_EXTS = new Set([
  "png",
  "apng",
  "jpg",
  "jpeg",
  "gif",
  "webp",
  "bmp",
  "avif",
]);
export function isImageArtifact(path: string): boolean {
  return !isFolderArtifact(path) && STRIP_IMAGE_EXTS.has(fileExt(path));
}

// 缩略图数据面:桌面版走 artifact_read 的 dataUrl(≤25MB),网页版直接用带 token 的
// 后端文件 URL(浏览器自己缓存)。模块级缓存按路径去重 —— 同图重渲染/多回合只读一次盘;
// 上限淘最旧,防大图 base64 无限常驻内存。
const THUMB_CACHE = new Map<string, Promise<string | null>>();
const THUMB_CACHE_MAX = 24;
export function loadImageThumb(path: string): Promise<string | null> {
  let p = THUMB_CACHE.get(path);
  if (!p) {
    p = (async () => {
      try {
        if (!isTauri) return backendFileUrl(path);
        return (await artifactsApi.read(path)).dataUrl ?? null;
      } catch {
        return null;
      }
    })();
    if (THUMB_CACHE.size >= THUMB_CACHE_MAX) {
      const oldest = THUMB_CACHE.keys().next().value;
      if (oldest !== undefined) THUMB_CACHE.delete(oldest);
    }
    THUMB_CACHE.set(path, p);
  }
  return p;
}

export function artifactIcon(path: string) {
  if (isFolderArtifact(path)) return FolderOpen;
  const ext = fileExt(path);
  if (["html", "htm", "svg", "js", "ts", "css", "json", "xml"].includes(ext))
    return FileCode;
  if (["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "avif"].includes(ext))
    return ImageIcon;
  if (["mp4", "mov", "webm", "mkv", "avi"].includes(ext)) return Clapperboard;
  if (["mp3", "wav", "m4a", "aac", "flac", "ogg"].includes(ext))
    return AudioLines;
  if (["csv", "tsv", "xlsx", "xls"].includes(ext)) return Table;
  if (["md", "markdown", "txt", "pdf"].includes(ext)) return FileText;
  return FileIcon;
}

export function attachIcon(kind: string) {
  if (kind === "image") return ImageIcon;
  if (kind === "pdf") return FileText;
  if (kind === "office") return Table;
  if (kind === "text") return FileCode;
  return FileIcon;
}

export function humanSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

// 工具名 → 友好中文（对话里以优雅 pill 呈现，不再是终端灰块）
export const TOOL_LABELS: Record<string, string> = {
  Bash: "运行命令",
  Read: "读取文件",
  Write: "写入文件",
  Edit: "编辑文件",
  MultiEdit: "批量编辑",
  NotebookEdit: "编辑笔记本",
  Glob: "查找文件",
  Grep: "搜索内容",
  WebSearch: "联网搜索",
  WebFetch: "抓取网页",
  Task: "子任务",
  TodoWrite: "更新清单",
};
export function toolLabel(n: string): string {
  return TOOL_LABELS[n] ?? n;
}

// ── 回合时间 ──
export function fmtTime(at?: number): string {
  if (!at) return "";
  const d = new Date(at);
  const today = new Date();
  const sameDay = d.toDateString() === today.toDateString();
  const hm = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  return sameDay ? hm : `${d.getMonth() + 1}/${d.getDate()} ${hm}`;
}

// ─────────── 回复渲染：统一 markdown 管线(lib/markdown) ───────────
// 已完成回合按原文命中缓存(流式期间不再全量重算);shiki/KaTeX 异步增强,
// 完成后 mdVersion 变化触发重读缓存。流式中的活跃回合传 enhance=false 省 CPU。
const ANSI_RE = /\x1b\[[0-9;?]*[ -/]*[@-~]/g;
// 系统提示词约定长回答第一行写 `TL;DR: 一句话结论`(见后端 reply_style_directive),
// 这里把它从正文里摘出来渲染成置顶速览卡, 正文从第二段起正常走 markdown 管线。
const TLDR_RE = /^\s*(?:>\s*)?(?:\*\*)?\s*TL;?\s?DR\s*(?:\*\*)?\s*[::]\s*(.+?)\s*$/i;
export function renderMd(text: string, enhance = true): string {
  void mdVersion.value; // 注册响应式依赖:增强完成后刷新
  const clean = (text || "").replace(ANSI_RE, "");
  const nl = clean.indexOf("\n");
  const firstLine = nl >= 0 ? clean.slice(0, nl) : clean;
  const m = firstLine.match(TLDR_RE);
  if (m) {
    const rest = nl >= 0 ? clean.slice(nl + 1).replace(/^\s*\n/, "") : "";
    return (
      `<div class="tldr"><span class="tldr-tag">TL;DR</span><div class="tldr-body">` +
      renderMarkdown(m[1], { enhance }) +
      `</div></div>` +
      (rest ? renderMarkdown(rest, { enhance }) : "")
    );
  }
  return renderMarkdown(clean, { enhance });
}

// 一个「回合」= 一条用户消息 + 其后的助手正文/工具/产物，直到下一条用户消息。
// 助手多段文本拼成一块 markdown；工具折叠成 pill；所有生成文件聚合到回合末尾。
export interface TurnTool {
  name: string;
  /** 连续同名合并的次数 */
  count: number;
  /** 各次调用的输入摘要(命令/路径/检索词) */
  details: string[];
}
export interface Turn {
  key: number;
  user?: Bubble;
  text: string;
  tools: TurnTool[];
  artifacts: string[];
  errors: string[];
  hasAssistant: boolean;
  /** 回合时间(用户消息时刻,无则首条气泡时刻) */
  at?: number;
  /** 「参考文件」列表 —— 构建回合时一次算好挂在对象上,模板只读。
   *  此前是模板里内联函数 refFiles(t):流式每帧(~40ms)每回合被调 3 次
   *  (v-if / .length / v-for 各一次),每次都重建 Set 全量扫工具,纯属浪费。 */
  refs: string[];
  /** Kimi 式「主预览件」:本轮产物里挑一个最该「打开看」的(html/spec/pdf…),
   *  渲染成置顶预览大卡,点开走右抽屉。无可预览件则 undefined。 */
  preview?: string;
  /** Kimi 式「文件夹」入口目标:本轮产物的公共目录(不带尾随 /)。点它在文件管理器打开,
   *  不再把一堆小文件铺满对话框。无产物则 undefined。 */
  folder?: string;
  /** 定稿回合预渲染好的正文 html(ChatPanel renderTurns 里挂上,随前缀缓存复用;
   *  活跃末回合缺省 → 由 TurnItem 现场 renderMd,流式中逐帧更新)。 */
  html?: string;
}
export const ERR_RE = /^\[(错误|发送失败|result error)/;

/** 豆包式「参考文件」: 本回合 Read 过的文件, 去重、剔除本回合产物与被截断的摘要。
 *  (原 ChatPanel.refFiles 原样搬入, 改为在 buildTurnsSlice 收尾时按回合算一次) */
function buildRefFiles(t: Turn): string[] {
  const arts = new Set(t.artifacts);
  const seen = new Set<string>();
  const out: string[] = [];
  for (const tl of t.tools) {
    if (tl.name !== "Read") continue;
    for (const d of tl.details) {
      const p = d.trim().replace(/\\/g, "/");
      // 摘要被截断(尾随 …)或不像路径的跳过, 宁缺勿错
      if (!p || p.endsWith("…") || !p.includes("/")) continue;
      if (seen.has(p) || arts.has(p)) continue;
      seen.add(p);
      out.push(p);
    }
  }
  return out.slice(0, 8);
}
/** 把一段气泡切片构建成回合模型(原 renderTurns 主体原样提炼,key 从 startKey 递增)。
 *  切片须在回合边界上:要么从头开始,要么以一条 user 气泡开头(user 恒开新回合)。 */
export function buildTurnsSlice(list: Bubble[], startKey: number): Turn[] {
  const out: Turn[] = [];
  let cur: Turn | undefined;
  let k = startKey;
  // 当前回合已收录产物的去重集:产物只往「当前回合」追加,故单个随回合重置的 Set 即可。
  // 把原先 `artifacts.includes(a)` 的 O(N) 线性查改成 O(1) 命中,整轮去重从 O(N²) 降到 O(N) ——
  // 长对话 + 多产物时不再越聊越顿。
  let curArtSet = new Set<string>();
  const startTurn = (user?: Bubble): Turn => {
    const turn: Turn = {
      key: k++,
      user,
      text: "",
      tools: [],
      artifacts: [],
      errors: [],
      hasAssistant: false,
      at: user?.at,
      refs: [],
    };
    out.push(turn);
    cur = turn;
    curArtSet = new Set<string>();
    return turn;
  };
  for (const b of list) {
    if (b.role === "user") {
      startTurn(b);
      continue;
    }
    const t: Turn = cur ?? startTurn(undefined);
    if (t.at === undefined && b.at !== undefined) t.at = b.at;
    if (b.role === "tool") {
      const name = b.tool || "工具";
      // 合并连续同名工具，避免刷屏;输入摘要逐条留底供展开查看
      const last = t.tools[t.tools.length - 1];
      if (last?.name === name) {
        last.count++;
        if (b.toolDetail) last.details.push(b.toolDetail);
      } else {
        t.tools.push({
          name,
          count: 1,
          details: b.toolDetail ? [b.toolDetail] : [],
        });
      }
    } else {
      const txt = b.text || "";
      if (ERR_RE.test(txt.trim())) {
        t.errors.push(txt);
      } else if (txt) {
        t.text += (t.text ? "\n\n" : "") + txt;
        t.hasAssistant = true;
      }
      if (b.artifacts) {
        for (const a of b.artifacts)
          if (!curArtSet.has(a)) {
            curArtSet.add(a);
            t.artifacts.push(a);
          }
      }
    }
  }
  // 参考文件在回合构建完成后一次算好(tools/artifacts 已齐);流式中活跃回合每帧
  // 重建时也只算这一次,而不是模板每帧内联调 3 次。
  for (const t of out) {
    t.refs = buildRefFiles(t);
    if (t.artifacts.length) {
      t.preview = pickPreview(t.artifacts);
      t.folder = commonFolder(t.artifacts);
    }
  }
  return out;
}

// ─────────── 每日晨报建议（回声层做梦产物）───────────
// 「让 AI 更懂你」：后台做梦据你新加入的内容产出工程化建议，展示在对话框顶部，
// 点「让我去做」= 把建议的 action 当 prompt 直接发起一轮对话。
export interface Suggestion {
  id: string;
  title: string;
  // 类别:progress(新进展)/ wrapup(收尾)/ workflow(可复用流程)/ organize(整理)
  kind?: string;
  // 依据来源标签（某段对话 / 某份文件 / 某个老项目名）——「懂你」的落点
  source?: string;
  why: string;
  how: string;
  action: string;
}

// ─────────── 百人专家团模式 ──────────
// 单 agent / 单专家 / 专家团 / 智能匹配（默认），这四个是互斥的，只选一个
export type AgentMode = "single-agent" | "single-expert" | "expert-team" | "auto-match";

// ─────────── 工作模式: 快速 / 工作 ───────────
export type WorkMode = "fast" | "work";

/** 发送选项(重新生成 / 今日建议 与主发送共用同一份形状) */
export interface ChatSendOptions {
  permissionMode: PermissionMode;
  skillIds: string[];
  useKb?: boolean;
  agentMode: AgentMode;
  workMode: WorkMode;
  providerId: string;
}

// ── 首帧非关键加载推迟 ──
// ChatPanel 挂载时多个 onMounted 并发打出与首屏渲染无关的 IPC(晨报/技能清单/供应商/
// codex 状态),与首屏聊天区渲染抢主线程和后端。统一包进空闲回调:浏览器空闲(或到点
// 兜底)再执行;组件已卸载则放弃,防止延迟回调在卸载后注册监听器/改状态。
export function useIdleRunner() {
  let disposed = false;
  function runWhenIdle(fn: () => void) {
    const run = () => {
      if (!disposed) fn();
    };
    if (typeof (window as any).requestIdleCallback === "function") {
      (window as any).requestIdleCallback(run, { timeout: 600 });
    } else {
      setTimeout(run, 600);
    }
  }
  /** 组件卸载时调用:让尚未执行的空闲回调作废 */
  function dispose() {
    disposed = true;
  }
  const isDisposed = () => disposed;
  return { runWhenIdle, dispose, isDisposed };
}
