/**
 * Typed wrappers around Tauri commands.
 *
 * Designed so the renderer can still mount in a plain browser (npm run dev) by
 * detecting absence of __TAURI_INTERNALS__ and returning empty / stub data.
 */
import { invoke as rawInvoke } from "@tauri-apps/api/core";
import {
  listen as rawListen,
  emit as rawEmit,
  type UnlistenFn,
} from "@tauri-apps/api/event";

export const isTauri =
  typeof window !== "undefined" &&
  // @ts-ignore tauri injects this
  typeof (window as any).__TAURI_INTERNALS__ !== "undefined";

// ──────────────────────────────────────────────────────────────
// Docker/Web 后端适配层
// ──────────────────────────────────────────────────────────────
// 非 Tauri 环境下：若同源存在 polaris-server（Docker 版），所有 invoke/listen
// 改走 HTTP(/api/invoke) + WebSocket(/ws)；探测不到后端则回退 browserStub，
// 保留 `npm run dev` 纯前端预览体验。业务组件零改动。

type BackendMode = "http" | "stub";
let backendMode: BackendMode | null = null;
let probePromise: Promise<void> | null = null;

/** 访问口令：URL ?token= 优先落盘 localStorage，之后从 localStorage 读。 */
function authToken(): string | null {
  if (typeof window === "undefined") return null;
  try {
    const u = new URL(window.location.href);
    const fromUrl = u.searchParams.get("token");
    if (fromUrl) localStorage.setItem("POLARIS_AUTH_TOKEN", fromUrl);
    return localStorage.getItem("POLARIS_AUTH_TOKEN");
  } catch {
    return null;
  }
}

export function authHeaders(): Record<string, string> {
  const t = authToken();
  return t ? { authorization: `Bearer ${t}` } : {};
}

/**
 * 后端受限文件 URL（/api/file）。window.open/<a download> 等导航请求带不了
 * Authorization 头，故 token 走 query（与 /ws 同理）。download=true 让后端加
 * Content-Disposition: attachment 强制下载。
 */
export function backendFileUrl(
  path: string,
  opts?: { download?: boolean }
): string {
  const qs = new URLSearchParams({ path });
  const t = authToken();
  if (t) qs.set("token", t);
  if (opts?.download) qs.set("download", "1");
  return `/api/file?${qs.toString()}`;
}

async function ensureBackend(): Promise<void> {
  if (backendMode) return;
  if (!probePromise) {
    probePromise = (async () => {
      try {
        const r = await fetch("/api/health", { cache: "no-store" });
        backendMode = r.ok ? "http" : "stub";
      } catch {
        backendMode = "stub";
      }
    })();
  }
  await probePromise;
}

async function httpInvoke<T>(
  cmd: string,
  args?: Record<string, unknown>
): Promise<T> {
  const res = await fetch("/api/invoke", {
    method: "POST",
    headers: { "content-type": "application/json", ...authHeaders() },
    body: JSON.stringify({ cmd, args: args ?? {} }),
  });
  if (!res.ok) {
    let msg = `HTTP ${res.status}`;
    try {
      const j = await res.json();
      if (j?.error) msg = j.error;
    } catch {
      /* ignore */
    }
    throw new Error(msg);
  }
  const text = await res.text();
  return (text ? JSON.parse(text) : undefined) as T;
}

/** 浏览器拖拽/选择的文件 → 上传到服务端 → 返回服务端绝对路径（喂给 kb_upload_files/chat_attach_files）。 */
export async function uploadToBackend(
  files: File[] | FileList
): Promise<Array<{ name: string; path: string; size: number }>> {
  if (isTauri) throw new Error("Tauri 环境请用原生文件路径");
  await ensureBackend();
  if (backendMode !== "http") return [];
  const fd = new FormData();
  const arr = Array.from(files as ArrayLike<File>);
  for (const f of arr) fd.append("files", f, f.name);
  const res = await fetch("/api/upload", {
    method: "POST",
    headers: { ...authHeaders() },
    body: fd,
  });
  if (!res.ok) throw new Error(`上传失败 HTTP ${res.status}`);
  const j = await res.json();
  return j.files ?? [];
}

// ── WebSocket：把服务端 emit 的事件按 topic 分发给 listen 注册的回调 ──
let ws: WebSocket | null = null;
const wsListeners = new Map<string, Set<(p: unknown) => void>>();
let wsReconnectTimer: ReturnType<typeof setTimeout> | null = null;

/** WS 连接状态变化（仅 Docker/Web 模式有意义）：true=已连上, false=断开重连中 */
const wsStatusCbs = new Set<(connected: boolean) => void>();
function dispatchWsStatus(connected: boolean) {
  for (const cb of wsStatusCbs) cb(connected);
}
export function onWsStatus(cb: (connected: boolean) => void): () => void {
  wsStatusCbs.add(cb);
  return () => wsStatusCbs.delete(cb);
}

function ensureWs(): void {
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING))
    return;
  try {
    const proto = window.location.protocol === "https:" ? "wss" : "ws";
    const t = authToken();
    const url = `${proto}://${window.location.host}/ws${
      t ? `?token=${encodeURIComponent(t)}` : ""
    }`;
    ws = new WebSocket(url);
    ws.onopen = () => dispatchWsStatus(true);
    ws.onmessage = (e) => {
      try {
        const { topic, payload } = JSON.parse(e.data);
        const set = wsListeners.get(topic);
        if (set) for (const cb of set) cb(payload);
      } catch {
        /* ignore malformed frame */
      }
    };
    ws.onclose = () => {
      ws = null;
      dispatchWsStatus(false);
      if (wsReconnectTimer) clearTimeout(wsReconnectTimer);
      // 仍有监听者才自动重连（避免空连接刷日志）。
      if (wsListeners.size > 0) wsReconnectTimer = setTimeout(ensureWs, 1500);
    };
    ws.onerror = () => {
      try {
        ws?.close();
      } catch {
        /* ignore */
      }
    };
  } catch {
    ws = null;
  }
}

export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (isTauri) return rawInvoke<T>(cmd, args);
  await ensureBackend();
  if (backendMode === "http") return httpInvoke<T>(cmd, args);
  // 纯前端预览：返回 stub 数据让 UI 仍可浏览。
  return browserStub(cmd, args) as T;
}

export async function listen<T>(
  event: string,
  cb: (payload: T) => void
): Promise<UnlistenFn> {
  if (isTauri) return rawListen<T>(event, (e) => cb(e.payload));
  await ensureBackend();
  if (backendMode !== "http") return () => {};
  ensureWs();
  let set = wsListeners.get(event);
  if (!set) {
    set = new Set();
    wsListeners.set(event, set);
  }
  set.add(cb as (p: unknown) => void);
  return () => {
    set!.delete(cb as (p: unknown) => void);
    if (set!.size === 0) wsListeners.delete(event);
  };
}

export async function emit(event: string, payload?: unknown): Promise<void> {
  if (isTauri) {
    await rawEmit(event, payload);
  }
  // Docker/Web 模式：前端→后端无需 emit（事件单向 server→client）。
}

// ──────────────────────────────────────────────────────────────
// 飞书网关 module (板块⑭ 阶段 A)
// ──────────────────────────────────────────────────────────────
export interface FeishuConfig {
  enabled: boolean;
  appId: string;
  appSecret: string;
  /** "feishu"(国内) | "lark"(国际) */
  domain: string;
  /** App 启动时自动开启网关 */
  autoStart?: boolean;
  /** "open" | "allowlist" | "disabled" */
  dmPolicy: string;
  groupRequireMention: boolean;
  allowFrom: string[];
}
export interface FeishuTestResult {
  ok: boolean;
  botName: string;
  botOpenId: string;
  message: string;
}

export interface FeishuQrResult {
  /** 二维码 SVG（本地生成，可直接内联渲染） */
  svg: string;
  /** 二维码指向的飞书开放平台建应用 URL */
  url: string;
}

export interface WecomBotInfo {
  botId: string;
  secret: string;
}

export const feishu = {
  getConfig: () => invoke<FeishuConfig>("feishu_get_config"),
  setConfig: (config: FeishuConfig) =>
    invoke<void>("feishu_set_config", { config }),
  test: () => invoke<FeishuTestResult>("feishu_test_connection"),
  /** 「扫码创建机器人」：生成飞书建应用入口二维码 */
  createQr: () => invoke<FeishuQrResult>("feishu_create_qr"),
  /** 在系统浏览器打开飞书开放平台建应用页（扫码桌面兜底） */
  openConsole: () => invoke<void>("feishu_open_console"),
  /** 企业微信扫码自动配置（OAuth 回环：开系统浏览器扫码 → 自动回传 botId/secret） */
  wecomScanCreate: (source: string) =>
    invoke<WecomBotInfo>("wecom_scan_create", { source }),
  /** 飞书对话引擎：启动长连接网关（Node 桥 → headless claude → 回发） */
  gatewayStart: () => invoke<void>("feishu_gateway_start"),
  /** 停止网关 */
  gatewayStop: () => invoke<void>("feishu_gateway_stop"),
  /** 查询网关运行状态 */
  gatewayStatus: () => invoke<{ running: boolean }>("feishu_gateway_status"),
  /** 订阅网关日志（feishu://log） */
  onGatewayLog: (cb: (text: string) => void) => listen<string>("feishu://log", cb),
  /** 订阅网关状态（feishu://status: starting|installing|connected|stopped） */
  onGatewayStatus: (cb: (state: string) => void) => listen<string>("feishu://status", cb),
};

// ──────────────────────────────────────────────────────────────
// 自媒体「账号管理」
// ──────────────────────────────────────────────────────────────
export interface MediaAccountStatus {
  platform: "wechat" | "xhs";
  label: string;
  bound: boolean;
  profileDir: string;
  /** profile 最近活动时间（unix 秒）；未绑定为 null */
  lastActive: number | null;
  detail: string;
}
export const mediaAccounts = {
  /** 探测各平台登录态（读固定 profile 目录） */
  status: () => invoke<MediaAccountStatus[]>("media_accounts_status"),
  /** 解绑某平台：清除登录态 profile，强制下次重新扫码 */
  forget: (platform: "wechat" | "xhs") =>
    invoke<string>("media_account_forget", { platform }),
};

// ──────────────────────────────────────────────────────────────
// KB module
// ──────────────────────────────────────────────────────────────
export interface KbHit {
  path: string;
  title: string;
  snippet: string;
  score: number;
}
export interface KbNode {
  id: string;
  title: string;
  category: string;
  /** "doc" 文档 | "folder" 目录中枢 | "root" 知识库根 */
  kind: "doc" | "folder" | "root" | "feedback";
  /** 文件中心星图:簇的「一句话画像」(AI 命名时给的温暖概括),选中卡片展示 */
  summary?: string;
}
export interface KbEdge {
  source: string;
  target: string;
  /** 文件中心星图:簇间语义关系标签(如「方法论 / 进阶 / 同源」);层级/双链边无此字段 */
  rel?: string;
}
export interface KbGraph {
  nodes: KbNode[];
  edges: KbEdge[];
}
/** 「构建知识网」编译进度事件 (kb:compile) */
export interface KbCompileEvent {
  runId: string;
  /** phase | tool | page | delta | done | error */
  kind: string;
  text?: string;
  /** 仅 done: 编译后重扫的文档总数 */
  docCount?: number;
}

/** 知识库拖拽上传的逐文件结果 */
export interface KbUploadResult {
  name: string;
  relPath: string;
  ok: boolean;
  message: string;
}

/** 批量转换 md 文件 (kb_convert_batch) 的汇总报告 */
export interface KbConvertReport {
  /** 扫到的文件总数 */
  total: number;
  /** 成功转成 md 的数量 (含缓存命中复用) */
  converted: number;
  /** 视频类跳过数 */
  skippedVideo: number;
  /** 其它跳过数 (图片/音频/压缩包等不可抽文本) */
  skippedOther: number;
  /** 失败明细 "文件名: 原因" */
  failed: string[];
}

/** wiki 质量检查 (kb_lint) 单条问题 */
export interface KbLintIssue {
  /** dead-link | missing-type | orphan | unsafe-path */
  kind: string;
  path: string;
  detail: string;
}
/** wiki 质量检查报告 */
export interface KbLintReport {
  totalPages: number;
  deadLinks: number;
  missingType: number;
  orphans: number;
  unsafePaths: number;
  issues: KbLintIssue[];
}

/** 信源安全扫描 (kb_scan_sources) 单条命中 */
export interface KbThreatHit {
  /** high | medium | low */
  severity: string;
  /** instruction-override | role-hijack | tool-coercion | exfiltration | hidden-content | suspicious-link */
  category: string;
  path: string;
  line: number;
  matched: string;
  snippet: string;
}
/** 信源安全扫描报告 */
export interface KbThreatReport {
  scannedFiles: number;
  flaggedFiles: number;
  skippedFiles: number;
  high: number;
  medium: number;
  low: number;
  hits: KbThreatHit[];
}

/** 「维护知识网」(enrich / dedup) 进度事件 (kb:enrich / kb:dedup) */
export interface KbMaintainEvent {
  runId: string;
  /** phase | tool | delta | done | error */
  kind: string;
  text?: string;
  /** 仅 done: enrich=applied 补链数 / dedup=merged 合并数 */
  applied?: number;
  merged?: number;
}

/** 名人资料包：随安装包分发，点「下载」拷进自己的资料库并附带安装配套 skill */
export interface KbPack {
  id: string;
  name: string;
  description: string;
  skillId: string;
  installed: boolean;
}

export const kb = {
  scan: () => invoke<number>("kb_scan"),
  /** 名人资料包列表（含安装状态） */
  packList: () => invoke<KbPack[]>("kb_pack_list"),
  /** 安装资料包：资料拷入 raw/ + 配套 skill 装入技能目录，返回索引文件总数 */
  packInstall: (id: string) => invoke<number>("kb_pack_install", { id }),
  /** 移除资料包：删 raw/ 下该名人目录 + 卸配套 skill，返回索引文件总数 */
  packRemove: (id: string) => invoke<number>("kb_pack_remove", { id }),
  /** 构建知识网：跑一个有写权限的 wiki 维护者 agent，摄入即编译。返回 runId，进度走 kb:compile 事件 */
  compile: () => invoke<string>("kb_compile"),
  /** wiki 质量检查：死双链/缺 type/孤儿页/不安全路径，纯规则即时返回 */
  lint: () => invoke<KbLintReport>("kb_lint"),
  /** 信源安全扫描：遍历 KB 文本文件扫提示词注入痕迹，纯规则即时返回 */
  scanSources: () => invoke<KbThreatReport>("kb_scan_sources"),
  /** 隔离可疑文件：移出 raw/ 到 .quarantine/（模型不再读到），可逆，返回隔离后相对路径 */
  quarantine: (relPath: string) => invoke<string>("kb_quarantine", { relPath }),
  /** 自动补双链：只读 claude 出 {term,target} 建议，Rust 执行替换。返回 runId，进度走 kb:enrich */
  enrichLinks: () => invoke<string>("kb_enrich_links"),
  /** 智能去重：规则粗筛 + AI 细判 + 代码合并。返回 runId，进度走 kb:dedup */
  dedup: () => invoke<string>("kb_dedup"),
  search: (q: string, topK = 8) =>
    invoke<KbHit[]>("kb_search", { query: q, topK }),
  list: (subdir: string | null = null) =>
    invoke<string[]>("kb_list", { subdir }),
  read: (relPath: string) => invoke<string>("kb_read", { relPath }),
  /** 删除一份资料(浏览页 ×)，返回剩余文件数 */
  delete: (relPath: string) => invoke<number>("kb_delete", { relPath }),
  /** 清空资料库(管理页)，返回剩余文件数 */
  clear: () => invoke<number>("kb_clear"),
  ingest: (sourcePath: string) =>
    invoke<string>("kb_ingest", { sourcePath }),
  /** 批量转换 md:文件/文件夹下非视频类可抽文本的全转 md 入 raw/ 并索引,视频/图片等跳过 */
  convertBatch: (paths: string[]) =>
    invoke<KbConvertReport>("kb_convert_batch", { paths }),
  /** 拖拽上传：任意格式 → 转 markdown 入 raw/，返回逐文件结果 */
  uploadFiles: (paths: string[]) =>
    invoke<KbUploadResult[]>("kb_upload_files", { paths }),
  graph: () => invoke<KbGraph>("kb_graph"),
  root: () => invoke<string>("kb_root"),
  defaultRoot: () => invoke<string>("kb_default_root"),
  setRoot: (newPath: string) =>
    invoke<number>("kb_set_root", { newPath }),
};

// ──────────────────────────────────────────────────────────────
// 全盘资源归集 (Scan) — 扫描 C/D 盘/桌面 → 多维表格 → 归档资源库 / 摄入核心层
// 归档复用 kb.uploadFiles;摄入核心层 = uploadFiles 后接 kb.compile。
// ──────────────────────────────────────────────────────────────
export interface ScanRoot {
  id: string;
  label: string;
  path: string;
  /** desktop | drive | home | volume | mounted */
  kind: string;
  defaultOn: boolean;
}
export interface ScanRow {
  id: string;
  path: string;
  name: string;
  ext: string;
  /** doc | sheet | slide | data | image | audio | video | archive | code | text | other */
  kind: string;
  /** 大概内容(启发式) */
  preview: string;
  size: number;
  sizeH: string;
  mtime: number;
  /** 价值 1-5 */
  score: number;
  /** 建议去向: resource | resource+core | skip */
  suggest: string;
}
export interface ScanReport {
  rows: ScanRow[];
  totalSeen: number;
  hit: number;
  skipped: number;
  truncated: boolean;
}
export const scan = {
  /** 平台自适应的扫描根(Win 盘符 / mac 家目录+卷 / Docker 挂载卷) */
  roots: () => invoke<ScanRoot[]>("scan_roots"),
  /** 扫描给定根下的有用资源,返回多维表格行。只读。 */
  resources: (roots: string[], max?: number) =>
    invoke<ScanReport>("scan_resources", { roots, max }),
};

// ──────────────────────────────────────────────────────────────
// 文件中心 (File Center) — 可视化文件库:类型/语义聚类/缩略图/速览
// 复用检索枢纽 fable.db(盘点表 + 已存向量),不另起数据源。
// ──────────────────────────────────────────────────────────────
export interface FcRoot {
  id: number;
  path: string;
  files: number;
}
export interface FcKindCount {
  kind: string;
  count: number;
  bytes: number;
}
/** 按语言归类的一档:编程语言(Python/Rust…)/ 自然语言(中文/英文)/ 媒体大类(图片/视频…) */
export interface FcLangCount {
  lang: string;
  count: number;
  bytes: number;
}
export interface FcCluster {
  id: number;
  label: string;
  color: string;
  keywords: string;
  size: number;
  /** 0 = 顶层主题文件夹;否则为所属父主题簇 id(语义两级归类) */
  parent: number;
}
export interface FileOverview {
  roots: FcRoot[];
  activeRoot: string | null;
  totalFiles: number;
  totalBytes: number;
  byKind: FcKindCount[];
  /** 按语言分布(编程语言 / 自然语言 / 媒体大类) */
  byLang: FcLangCount[];
  clusters: FcCluster[];
  textFiles: number;
  embeddedFiles: number;
  hasEmbedProvider: boolean;
  clustered: boolean;
  scanning: boolean;
  indexing: boolean;
}
/** 智能向导收尾的一条建议:标题 + 注入对话框的用户第一人称提示词 */
export interface SuggestedFlow {
  title: string;
  prompt: string;
}
export interface FileCard {
  id: number;
  path: string;
  abspath: string;
  name: string;
  /** 智能显示标题:AI 起的名(若有)否则本地清洗文件名;卡片主标题用它,name 做副标题/悬停 */
  title: string;
  ext: string;
  /** text | doc | image | audio | video | archive | other */
  kind: string;
  size: number;
  sizeH: string;
  mtime: number;
  clusterId: number;
  thumbable: boolean;
}
export interface FileGridPage {
  items: FileCard[];
  total: number;
  page: number;
  pageSize: number;
}
export interface ClusterBuildSummary {
  clusters: number;
  files: number;
  seconds: number;
  note: string;
}
export interface ClusterModelView {
  enabled: boolean;
  baseUrl: string;
  model: string;
  keySet: boolean;
}
export interface ScanRootInfo {
  path: string;
  label: string;
  defaultOn: boolean;
}
export interface FolderNode {
  path: string;
  parent: string;
  name: string;
  root: string;
  depth: number;
  files: number;
  hasChildren: boolean;
}
export interface FolderScan {
  roots: ScanRootInfo[];
  folders: FolderNode[];
  truncated: boolean;
}
export interface FolderSize {
  files: number;
  bytes: number;
}
export interface FileGridParams {
  root?: string | null;
  clusterId?: number | null;
  kind?: string | null;
  /** 按语言过滤:编程语言(Python/Rust…)、自然语言(中文/英文)、媒体大类(图片/视频…) */
  lang?: string | null;
  sort?: "recent" | "name" | "size" | "kind";
  query?: string | null;
  page?: number;
  pageSize?: number;
}

/** 检索枢纽 + 文件中心命令封装 */
export const files = {
  /** 文件库总览:类型分布 + 语义簇 + 根列表 + 索引状态 */
  overview: (root?: string | null) =>
    invoke<FileOverview>("file_overview", { root: root ?? null }),
  /** 分页拉取文件卡片(可按簇/类型/文件名过滤、排序) */
  grid: (p: FileGridParams = {}) =>
    invoke<FileGridPage>("file_grid", {
      root: p.root ?? null,
      clusterId: p.clusterId ?? null,
      kind: p.kind ?? null,
      lang: p.lang ?? null,
      sort: p.sort ?? "recent",
      query: p.query ?? null,
      page: p.page ?? 0,
      pageSize: p.pageSize ?? 60,
    }),
  /** 给所有文件补「语言」归类标签(代码/媒体零 IO,文稿读头嗅探中文/英文);返回回填条数 */
  backfillLang: () => invoke<number>("fable_backfill_lang", {}),
  /** 缩略图/首帧 → data URL(失败返回 null,前端落类型图标);磁盘缓存 */
  thumb: (abspath: string, max = 360) =>
    invoke<string | null>("file_thumb", { abspath, max }),
  /** 按需内容速览(抽取式,零 token,带缓存) */
  gist: (abspath: string) => invoke<string>("file_gist", { abspath }),
  /** 重建语义聚类(复用已存向量,纯数学)。后台线程跑,进度走 file:cluster 事件(phase/done/error) */
  clusterBuild: (root?: string | null) =>
    invoke<void>("file_cluster_build", { root: root ?? null }),
  /** 文件中心 v3 渐进式智能归类:T0 秒级骨架 → T1 AI 初级命名+关系 → T2 全量向量化后语义重聚再命名。
   *  后台线程跑,进度/各档走 file:cluster 事件(phase/tick/tier/done/error) */
  smartCluster: (root?: string | null) =>
    invoke<void>("file_smart_cluster", { root: root ?? null }),
  /** 用已连接的大模型按语义归类(免嵌入 key)+ 桌面生成 HTML 报告;进度走 file:cluster_llm 事件 */
  clusterLlm: (root?: string | null) =>
    invoke<void>("file_cluster_llm", { root: root ?? null }),
  /** 「让 AI 更懂你」:据盘点统计确定性生成知识画像 HTML → 桌面,返回文件路径(同步,不调大模型) */
  profileHtml: (root?: string | null) =>
    invoke<string>("file_profile_html", { root: root ?? null }),
  /** 智能向导收尾建议:大模型据**真实知识库**智能匹配「我能立刻替你做的事」,失败自动回落确定性建议 */
  suggestWorkflows: (root?: string | null) =>
    invoke<SuggestedFlow[]>("file_suggest_workflows", { root: root ?? null }),
  /** 文件中心「星图」:语义簇 + 抽样文件 → 与知识图谱同构的 KbGraph(供星河渲染复用) */
  graph: (root?: string | null) =>
    invoke<KbGraph>("file_graph", { root: root ?? null }),
  /** AI 智能命名:给乱码/杂乱文件名起可读中文标题(只覆盖显示,不改磁盘);进度走 file:title_llm 事件 */
  titlesLlm: (root?: string | null) =>
    invoke<void>("file_titles_llm", { root: root ?? null }),
  /** 清空 AI 标题 → 回落本地清洗名 */
  titlesClear: () => invoke<number>("file_titles_clear"),
  /** 读「归类专用模型」配置(独立于对话供应商,可指便宜模型;key 只回是否已配) */
  clusterModelGet: () => invoke<ClusterModelView>("file_cluster_model_get"),
  /** 存「归类专用模型」配置(apiKey 传空=保留旧 key) */
  clusterModelSet: (p: {
    enabled?: boolean;
    baseUrl?: string;
    model?: string;
    apiKey?: string;
  }) => invoke<ClusterModelView>("file_cluster_model_set", p),
  /** 批量预热缩略图缓存(进入网格时后台调,滚动更顺);返回成功数 */
  warmThumbs: (paths: string[], max = 360) =>
    invoke<number>("file_warm_thumbs", { paths, max }),
  /** 盘点前先扫一眼文件夹结构(根 + 第一层);列知识库+盘符/桌面等可选根 */
  scanFolders: (root?: string | null) =>
    invoke<FolderScan>("fable_scan_folders", { root: root ?? null }),
  /** 懒加载:点开某文件夹时取它的直属子文件夹(支持往下钻到任意深度) */
  scanFolderChildren: (root: string, path: string) =>
    invoke<FolderNode[]>("fable_scan_folder_children", { root, path }),
  /** 某文件夹的递归总量(文件数 + 字节);选择器里按需限并发调用显示大小 */
  folderSize: (path: string) => invoke<FolderSize>("fable_folder_size", { path }),
  /** 开始盘点:roots=勾选要盘点的文件夹/盘符(空=默认知识库+NAS);exclude=范围内取消的子文件夹。进度走 fable:inventory 事件 */
  inventoryStart: (roots?: string[], exclude?: string[]) =>
    invoke<void>("fable_inventory_start", { roots: roots ?? [], exclude: exclude ?? [] }),
  /** 构建/续建向量索引(文本 chunk → 硅基 BGE-M3 嵌入),进度走 fable:index 事件 */
  indexStart: (maxChunks?: number) =>
    invoke<void>("fable_index_start", { maxChunks: maxChunks ?? null }),
  /** 检索枢纽混合检索(grep ∥ 向量 RRF) */
  search: (query: string, topK = 24, mode: "hybrid" | "grep" | "vector" = "hybrid") =>
    invoke<FableSearchResult>("fable_search", { query, topK, mode }),
  /** 取消当前盘点/索引任务(协作式:循环轮询 CANCEL,几百毫秒内优雅停;索引可再点继续续建) */
  fableCancel: () => invoke<void>("fable_cancel"),
};

export interface FableHit {
  path: string;
  abspath: string;
  location: string;
  snippet: string;
  score: number;
  lanes: string[];
}
export interface FableSearchResult {
  query: string;
  mode: string;
  hits: FableHit[];
  grepHits: number;
  vectorHits: number;
  reranked: boolean;
  grepTruncated: boolean;
  ms: number;
}

// ──────────────────────────────────────────────────────────────
// Sandbox module → 已迁出至 features/sandbox/api.ts (架构重构 Phase 1)
// 浏览器降级 stub 仍保留在本文件下方的 browserStub() 中。
// ──────────────────────────────────────────────────────────────

// ──────────────────────────────────────────────────────────────
// Chat module
// ──────────────────────────────────────────────────────────────
export type PermissionMode =
  | "manual"
  | "auto_current"
  | "auto_all"
  | "deny";

export interface ChatSendArgs {
  prompt: string;
  permissionMode: PermissionMode;
  useSandbox?: boolean;
  skillIds?: string[];
  conversationId?: string;
  /** 目标模式：完成条件。设置后 Claude 会持续推进直到达成，不中途收尾。 */
  goal?: string;
  /** 「请教毛主席」：注入毛选式客观分析指令，调用毛主席资料库，生成标注来源的 HTML。 */
  consultMao?: boolean;
  /** 「动态编排」：多智能体编排——编排器拆 N 个独立子任务，Task 子代理并行扇出，每条流水线 实现→对抗式校验→修复，最后汇总。 */
  dynamicWorkflow?: boolean;
  /** 「知识库严格搜索」：打开时才把 KB 结构化 wiki + 双链地图注入上下文。默认 false。 */
  useKb?: boolean;
  /** 「分批长任务」：把超长生成拆成多轮有界批次（注入 polaris.build.json 清单协议）。 */
  batchBuild?: boolean;
  /** 每批最多构建几个单元（页/章/文件）。 */
  batchSize?: number;
  /** 智能体模式："auto-match"(默认智能匹配) | "expert-team" | "single-expert" | "single-agent"。 */
  agentMode?: string;
}

export interface ChatStreamEvent {
  reqId: string;
  kind: "delta" | "tool" | "error" | "done" | "artifact" | "meta";
  text?: string;
  tool?: string;
  conversationId?: string;
}

/** 分批构建清单 polaris.build.json 的一个单元 */
export interface BuildUnit {
  id: string;
  title: string;
  status: "pending" | "done" | string;
  artifact?: string;
}

/** 分批构建清单（断点续传凭据） */
export interface BuildManifest {
  goal?: string;
  kind?: string;
  batch_size?: number;
  output?: string;
  units: BuildUnit[];
}

/** 对话拖拽上传的附件（复制进会话 uploads 目录） */
export interface AttachedFile {
  name: string;
  /** uploads 目录里的绝对路径（正斜杠） */
  path: string;
  /** text | image | pdf | office | binary */
  kind: "text" | "image" | "pdf" | "office" | "binary";
  size: number;
  ok: boolean;
  error?: string;
}

export const chat = {
  send: (args: ChatSendArgs) =>
    invoke<string>("chat_send", { args: args as unknown as Record<string, unknown> }),
  cancel: (reqId: string) => invoke<void>("chat_cancel", { reqId }),
  /** 读取分批构建清单 polaris.build.json（分批长任务的断点/进度凭据）。不存在返回 null。 */
  buildManifest: (conversationId: string | undefined) =>
    invoke<BuildManifest | null>("chat_build_manifest", {
      conversationId: conversationId ?? null,
    }),
  /** 拖拽上传：把文件复制进当前会话，返回附件清单 */
  attachFiles: (conversationId: string | undefined, paths: string[]) =>
    invoke<AttachedFile[]>("chat_attach_files", {
      conversationId: conversationId ?? null,
      paths,
    }),
  /** 剪贴板贴图：base64 落盘到会话 uploads，返回附件 */
  attachImage: (
    conversationId: string | undefined,
    name: string,
    dataBase64: string
  ) =>
    invoke<AttachedFile>("chat_attach_image", {
      conversationId: conversationId ?? null,
      name,
      dataBase64,
    }),
};

/** 在系统默认浏览器打开外部链接（回复正文里的 http/https 链接） */
export async function openUrl(url: string): Promise<void> {
  if (isTauri) {
    await invoke<void>("open_url", { url });
    return;
  }
  window.open(url, "_blank", "noopener,noreferrer");
}

// ──────────────────────────────────────────────────────────────
// Artifacts module — 对话生成的成品文件，右侧抽屉预览
// ──────────────────────────────────────────────────────────────
export type ArtifactKind =
  | "html"
  | "svg"
  | "image"
  | "markdown"
  | "text"
  | "binary";

export interface ArtifactPayload {
  path: string;
  name: string;
  ext: string;
  kind: ArtifactKind;
  /** 文本类(html/svg/markdown/text)内容 */
  text?: string;
  /** 图片类的 data URL */
  dataUrl?: string;
  size: number;
}

/** 「参考资料」文件夹视图的一条文件记录 */
export interface ArtifactEntry {
  path: string;
  name: string;
  ext: string;
  kind: ArtifactKind;
  size: number;
  /** 修改时间 Unix 秒 */
  modified: number;
}

export const artifacts = {
  read: (path: string) => invoke<ArtifactPayload>("artifact_read", { path }),
  /** 把编辑后的文本写回已存在的产物文件（成品编辑器保存用） */
  write: (path: string, content: string) =>
    invoke<void>("artifact_write", { path, content }),
  openExternal: (path: string) =>
    invoke<void>("artifact_open_external", { path }),
  /** 在系统文件管理器中定位并选中该文件（资源管理器 / 访达） */
  reveal: (path: string) => invoke<void>("artifact_reveal", { path }),
  /** 列出某会话产物文件，按修改时间倒序 */
  list: (conversationId?: string) =>
    invoke<ArtifactEntry[]>("artifact_list", {
      conversationId: conversationId ?? null,
    }),
  /** 跨所有对话检索历史产物文件（文件名 + 正文） */
  search: (query: string) =>
    invoke<ArtifactSearchHit[]>("artifact_search", { query }),
};

/** 跨对话产物搜索命中 */
export interface ArtifactSearchHit {
  path: string;
  name: string;
  kind: ArtifactKind;
  conversationId: string;
  snippet: string;
  modified: number;
  score: number;
}

// ──────────────────────────────────────────────────────────────
// Project module — 可运行项目（一键启动前后端 + 内嵌预览）
// ──────────────────────────────────────────────────────────────
export interface ProjectInfo {
  /** 项目根绝对路径（正斜杠）——唯一标识 */
  root: string;
  name: string;
  /** 预览 URL（前端起来后内嵌 iframe 加载） */
  open?: string | null;
  /** 是否正在运行 */
  running: boolean;
  /** 服务名列表（展示用） */
  services: string[];
}

export const project = {
  /** 列出某会话产物里的可运行项目（带 polaris.project.json 的文件夹） */
  list: (conversationId?: string) =>
    invoke<ProjectInfo[]>("project_list", {
      conversationId: conversationId ?? null,
    }),
  /** 该项目是否正在运行 */
  status: (root: string) => invoke<boolean>("project_status", { root }),
  /** 一键运行：装依赖 + 起前后端，进度走 project:log / project:ready / project:exit 事件 */
  run: (root: string) => invoke<void>("project_run", { root }),
  /** 停止：kill 整个进程树 */
  stop: (root: string) => invoke<void>("project_stop", { root }),
};

// ──────────────────────────────────────────────────────────────
// Skills module
// ──────────────────────────────────────────────────────────────
export interface Skill {
  id: string;
  name: string;
  description: string;
  source: string;
  /** 是否已拥有可用（预装 / 已安装 / 用户自建） */
  installed?: boolean;
  /** 是否可删除（物理存在于用户目录，可卸载） */
  removable?: boolean;
}

export const skills = {
  list: () => invoke<Skill[]>("list_skills"),
  get: (id: string) => invoke<Skill>("get_skill", { id }),
  create: (id: string, name: string, description: string, systemPrompt: string) =>
    invoke<void>("create_skill", { id, name, description, systemPrompt }),
  install: (id: string) => invoke<void>("install_skill", { id }),
  /** 从外部来源导入：本地 .md/.zip/目录 · 远程 .md/.zip · git 仓库 URL（返回导入的 id 列表） */
  import: (source: string) => invoke<string[]>("import_skill", { source }),
  delete: (id: string) => invoke<void>("delete_skill", { id }),
};

// ──────────────────────────────────────────────────────────────
// CLAUDE.md 主上下文 module
// 每个 conv 项目一份 + KB 共享一份
// ──────────────────────────────────────────────────────────────
export interface ProjectClaudeMd {
  projectId: string;
  projectName: string;
  absPath: string;
  exists: boolean;
  active: boolean;
  size: number;
}

export interface KbClaudeMd {
  absPath: string;
  exists: boolean;
  active: boolean;
  size: number;
}

export type ClaudeMdArea = "kb" | "project";

export const claudeMd = {
  listProjects: () => invoke<ProjectClaudeMd[]>("claude_md_list_projects"),
  kbInfo: () => invoke<KbClaudeMd>("claude_md_kb_info"),
  read: (area: ClaudeMdArea, projectId?: string) =>
    invoke<string>("claude_md_read", { area, projectId: projectId ?? null }),
  write: (area: ClaudeMdArea, projectId: string | undefined, content: string) =>
    invoke<void>("claude_md_write", {
      area,
      projectId: projectId ?? null,
      content,
    }),
};

// ──────────────────────────────────────────────────────────────
// Conv module (项目 + 对话历史)
// ──────────────────────────────────────────────────────────────
export interface Project {
  id: string;
  name: string;
  createdAt: number;
  archived: boolean;
  /** 板块⑫ 套用的预设人格 id（自定义为 null） */
  personaId?: string | null;
  /** 该人格绑定的专属知识库 scope（KB 根下相对子目录，null/空=全局） */
  kbScope?: string | null;
}

export interface Conversation {
  id: string;
  projectId: string;
  title: string;
  createdAt: number;
  updatedAt: number;
}

export interface Message {
  id: string;
  conversationId: string;
  role: "user" | "assistant" | "tool";
  content: string;
  createdAt: number;
}

// Rust 端用 snake_case, serde 默认行为, 这里手动映射回 camelCase
type RawProject = {
  id: string;
  name: string;
  created_at: number;
  archived: boolean;
  persona_id?: string | null;
  kb_scope?: string | null;
};
type RawConv = {
  id: string;
  project_id: string;
  title: string;
  created_at: number;
  updated_at: number;
};
type RawMsg = {
  id: string;
  conversation_id: string;
  role: string;
  content: string;
  created_at: number;
};

const p = (r: RawProject): Project => ({
  id: r.id,
  name: r.name,
  createdAt: r.created_at,
  archived: r.archived,
  personaId: r.persona_id ?? null,
  kbScope: r.kb_scope ?? null,
});
const c = (r: RawConv): Conversation => ({
  id: r.id,
  projectId: r.project_id,
  title: r.title,
  createdAt: r.created_at,
  updatedAt: r.updated_at,
});
const m = (r: RawMsg): Message => ({
  id: r.id,
  conversationId: r.conversation_id,
  role: r.role as Message["role"],
  content: r.content,
  createdAt: r.created_at,
});

export const convApi = {
  listProjects: async () => (await invoke<RawProject[]>("conv_list_projects")).map(p),
  createProject: async (name: string) =>
    p(await invoke<RawProject>("conv_create_project", { name })),
  archiveProject: (projectId: string) =>
    invoke<void>("conv_archive_project", { projectId }),
  openProjectDir: (projectId: string) =>
    invoke<void>("conv_open_project_dir", { projectId }),
  listConversations: async (projectId: string) =>
    (await invoke<RawConv[]>("conv_list_conversations", { projectId })).map(c),
  createConversation: async (projectId: string) =>
    c(await invoke<RawConv>("conv_create_conversation", { projectId })),
  deleteConversation: (conversationId: string) =>
    invoke<void>("conv_delete_conversation", { conversationId }),
  /** 回声层:归档/取消归档对话(纯状态位,移出列表但保留消息,可逆) */
  archiveConversation: (id: string, archived = true) =>
    invoke<void>("conv_archive_conversation", { id, archived }),
  /** 回声层:把单条对话立刻沉淀为记忆(后台跑,进度走 echo:dream 事件) */
  distillConversation: (convId: string) =>
    invoke<void>("echo_distill_conversation", { convId }),
  renameConversation: (conversationId: string, title: string) =>
    invoke<void>("conv_rename_conversation", { conversationId, title }),
  getMessages: async (conversationId: string) =>
    (await invoke<RawMsg[]>("conv_get_messages", { conversationId })).map(m),
  /** 板块⑫: 设置项目的知识库 scope（人格工坊下拉） */
  setKbScope: (projectId: string, kbScope: string | null) =>
    invoke<void>("conv_set_project_kb_scope", { projectId, kbScope }),
};

// ──────────────────────────────────────────────────────────────
// 人格模块 module (板块⑫) — 预设人格库 + 应用到项目
// ──────────────────────────────────────────────────────────────
export interface PersonaPreset {
  id: string;
  name: string;
  icon: string;
  description: string;
  /** 建议绑定的知识库 scope（KB 根下相对子目录，空=全局） */
  kbScope: string;
  /** 人格正文（写入项目 CLAUDE.md 的内容） */
  body: string;
  /** 种类: "single"=单专家 | "team"=专家团（战略师领衔的编排型 CLAUDE.md） */
  kind: string;
}

export const persona = {
  list: () => invoke<PersonaPreset[]>("persona_list"),
  /** 把预设人格应用到项目（写 CLAUDE.md + 绑定 scope）；已有内容需 overwrite=true */
  apply: (projectId: string, personaId: string, overwrite = false) =>
    invoke<void>("persona_apply", { projectId, personaId, overwrite }),
};

// ──────────────────────────────────────────────────────────────
// 百人专家团 module — 运行时动态召集 + 可解释路由
// ──────────────────────────────────────────────────────────────
export interface ExpertCard {
  id: string;
  name: string;
  icon: string;
  role: string;
  description: string;
  triggerSignals: string[];
  complements: string;
  keywords: string[];
  capabilities: string[];
  claudeMdRef: string;
  modelHint: string;
  costTier: number;
  exclusiveWith: string[];
  source: string;
  license: string;
  group: string;
}

export interface ExpertMatch {
  expert: ExpertCard;
  hitSignals: string[];
  similarity: number;
  complements: string;
  isPrimary: boolean;
}

export interface ExpertAgentStatus {
  expertId: string;
  name: string;
  status: string;
  lastActive: string;
}

export interface ExpertGroup {
  id: string;
  name: string;
  icon: string;
  count: number;
}

export interface RouteRequest {
  query: string;
  limit?: number;
  groupFilter?: string;
}

/** 业务专家团：领衔者 + 成员的成建制队伍 */
export interface ExpertTeam {
  id: string;
  name: string;
  icon: string;
  tagline: string;
  description: string;
  leadId: string;
  memberIds: string[];
  tags: string[];
}

/** 路由调试一行：每个候选专家的命中明细 */
export interface ExpertDebugRow {
  id: string;
  name: string;
  group: string;
  hitSignals: string[];
  similarity: number;
  wouldSelect: boolean;
}

/** 按知识库反推的专家团推荐 */
export interface KbRecommendation {
  team: ExpertTeam | null;
  reason: string;
  topExperts: ExpertCard[];
  matchedTopics: string[];
  corpusSize: number;
}

export const expert = {
  list: () => invoke<ExpertCard[]>("expert_list"),
  listByGroup: (group: string) => invoke<ExpertCard[]>("expert_list_by_group", { group }),
  groups: () => invoke<ExpertGroup[]>("expert_groups"),
  route: (req: RouteRequest) => invoke<ExpertMatch[]>("expert_route", { req }),
  get: (id: string) => invoke<ExpertCard | null>("expert_get", { id }),
  matchAuto: (query: string) => invoke<ExpertMatch[]>("expert_match_auto", { query }),
  /** 把专家的 CLAUDE.md 模板应用到项目（写 CLAUDE.md + 记录 persona_id）；已有内容需 overwrite=true */
  apply: (projectId: string, expertId: string, overwrite = false) =>
    invoke<void>("expert_apply", { projectId, expertId, overwrite }),
  /** 取专家/专家团头像 base64 data URL（失败返回 null，前端落 gradient 占位） */
  getAvatar: (id: string) => invoke<string | null>("expert_avatar", { id }),
  /** 一次性取全部 9 张头像（按槽位），配合 avatarSlot(id) 本地映射，避免逐卡 IPC 卡顿 */
  avatarSlots: () => invoke<string[]>("expert_avatar_slots"),
  /** 召集专家团：分析任务并返回推荐的专家列表（最多5个） */
  teamSpawn: (projectId: string, task: string) =>
    invoke<ExpertMatch[]>("expert_team_spawn", { projectId, taskDescription: task }),
  /** 查询项目当前专家团各专家的状态（idle|working|done） */
  agentsStatus: (projectId: string) =>
    invoke<ExpertAgentStatus[]>("expert_agents_status", { projectId }),
  /** 全部业务专家团 */
  teams: () => invoke<ExpertTeam[]>("expert_teams"),
  /** 取单个业务团 */
  teamGet: (id: string) => invoke<ExpertTeam | null>("expert_team_get", { id }),
  /** 把业务团应用到项目（组装战略师领衔的编排型 CLAUDE.md）；已有内容需 overwrite=true */
  teamApply: (projectId: string, teamId: string, overwrite = false) =>
    invoke<void>("team_apply", { projectId, teamId, overwrite }),
  /** 「下载」专家：返回其完整 CLAUDE.md 文本 */
  exportExpert: (id: string) => invoke<string>("expert_export", { id }),
  /** 「下载」业务团：返回其完整编排型 CLAUDE.md 文本 */
  exportTeam: (id: string) => invoke<string>("team_export", { id }),
  /** 调试某条查询的智能匹配，返回全部命中专家的打分明细 */
  routeDebug: (query: string) => invoke<ExpertDebugRow[]>("expert_route_debug", { query }),
  /** 按知识库反推该配哪支专家团（scope 可限定子目录，空=全库） */
  recommendFromKb: (scope?: string) =>
    invoke<KbRecommendation>("expert_recommend_from_kb", { scope: scope ?? null }),
};

/**
 * 头像槽位：与后端 avatars.rs 的 FNV-1a 一致，把任意 expert/team id 映射到 0..9。
 * 专家/团 id 都是 ASCII，charCodeAt 即字节，结果与 Rust 一致。
 * 用法：拉一次 expert.avatarSlots() 得到 9 张 dataURL，再用 slots[avatarSlot(id)] 取图，
 * 100+ 张卡片零额外 IPC。
 */
export function avatarSlot(id: string): number {
  let h = 2166136261 >>> 0;
  for (let i = 0; i < id.length; i++) {
    h = (h ^ id.charCodeAt(i)) >>> 0;
    h = Math.imul(h, 16777619) >>> 0;
  }
  return h % 9;
}

// ──────────────────────────────────────────────────────────────
// API 供应商坞 + 用量看板 module
// ──────────────────────────────────────────────────────────────
export interface ProviderView {
  id: string;
  name: string;
  note: string;
  baseUrl: string;
  tokenField: string;
  category: string; // official | cn_official | aggregator | third_party | cloud_provider | custom
  websiteUrl: string;
  color: string;
  kind: string; // official | key | codex | copilot | custom
  isPreset: boolean;
  hasKey: boolean;
  authToken: string;
  /** 完整 settings_config（env + includeCoAuthoredBy/attribution 等） */
  settingsConfig: any;
}
export interface ProviderListResult {
  providers: ProviderView[];
  currentId: string;
  /** true = 联动(切换写 ~/.claude/settings.json, 终端 CLI 跟着变); false = 隔离(仅 Polaris 内生效) */
  linkGlobal: boolean;
}
export interface ProviderSaveInput {
  id?: string;
  name: string;
  note?: string;
  websiteUrl?: string;
  tokenField?: string;
  /** 完整 settings_config（env 含 base_url + token + 开关） */
  settingsConfig: any;
}
export interface TokenBucket {
  input: number;
  output: number;
  cacheRead: number;
  cacheCreation: number;
  total: number;
  requests: number;
  cost: number;
}
export interface DailyUsage {
  date: string;
  label: string;
  total: number;
  cost: number;
}
export interface UsageSummary {
  available: boolean;
  today: TokenBucket;
  week: TokenBucket;
  month: TokenBucket;
  year: TokenBucket;
  daily: DailyUsage[];
}
export interface CodexStatus {
  installed: boolean;
  loggedIn: boolean;
  authPath: string;
}
export interface CodexDeviceLogin {
  deviceCode: string;
  userCode: string;
  verificationUri: string;
  interval: number;
  expiresIn: number;
}
export interface CodexPollResult {
  status: "pending" | "ok";
}
export interface CodexProxyInfo {
  running: boolean;
  port: number;
  lastError: string;
}

export const provider = {
  list: () => invoke<ProviderListResult>("provider_list"),
  switch: (id: string) => invoke<string>("provider_switch", { id }),
  setLinkMode: (link: boolean) =>
    invoke<boolean>("provider_set_link_mode", { link }),
  save: (input: ProviderSaveInput) =>
    invoke<string>("provider_save", { input }),
  delete: (id: string) => invoke<void>("provider_delete", { id }),
  usage: () => invoke<UsageSummary>("usage_summary"),
  codexStatus: () => invoke<CodexStatus>("codex_status"),
  codexStartLogin: () => invoke<CodexDeviceLogin>("codex_start_login"),
  codexPollLogin: (deviceCode: string, userCode: string) =>
    invoke<CodexPollResult>("codex_poll_login", { deviceCode, userCode }),
  codexProxyInfo: () => invoke<CodexProxyInfo>("codex_proxy_info"),
};

// ──────────────────────────────────────────────────────────────
// 环境医生 module — 新用户「环境监测 + 配置安装」(claude / pwsh / PATH)
// ──────────────────────────────────────────────────────────────
export interface ToolStatus {
  key: "claude" | "pwsh" | "node" | "npm";
  name: string;
  found: boolean;
  version: string | null;
  path: string | null;
  onPath: boolean;
  required: boolean;
  hint: string;
}
export interface EnvReport {
  os: string;
  claude: ToolStatus;
  pwsh: ToolStatus;
  node: ToolStatus;
  npm: ToolStatus;
  claudeDir: string | null;
  claudeDirOnUserPath: boolean;
  /** 是否有 claude 可用的 shell (真身 PowerShell 7 / Git Bash)；false ⇒ 对话会报缺 shell */
  shellReady: boolean;
  ready: boolean;
}
export interface PathFixResult {
  ok: boolean;
  dir: string | null;
  status: string;
  message: string;
}
export interface EnvStreamEvent {
  reqId: string;
  kind: "log" | "error" | "done";
  line?: string;
  ok?: boolean;
  message?: string;
}
/** Claude Code 更新检测结果 */
export interface ClaudeUpdateInfo {
  installed: boolean;
  current: string | null;
  latest: string | null;
  updateAvailable: boolean;
  checked: boolean;
  message: string;
}

export const envDoctor = {
  check: () => invoke<EnvReport>("env_check"),
  fixPath: () => invoke<PathFixResult>("env_fix_path"),
  /** 安装 Claude Code。method: "npm"(经国内镜像, 默认) | "native"(官方原生脚本, 兜底) */
  installClaude: (method: "npm" | "native" = "npm") =>
    invoke<string>("env_install_claude", { method }),
  /** 安装 Node.js LTS (winget) —— npm 安装方式的前置依赖 */
  installNode: () => invoke<string>("env_install_node"),
  installPwsh: () => invoke<string>("env_install_pwsh"),
  /** 检测 Claude Code 是否有新版本 (当前版本 vs npmmirror latest) */
  checkClaudeUpdate: () => invoke<ClaudeUpdateInfo>("env_claude_update_check"),
  /** 更新 Claude Code 到最新版 (走国内 npmmirror)，流式日志同安装 */
  updateClaude: () => invoke<string>("env_update_claude"),
  cancel: (reqId: string) => invoke<void>("env_cancel", { reqId }),
};

// ──────────────────────────────────────────────────────────────
// Browser stubs (when running in plain `npm run dev` without Tauri)
// ──────────────────────────────────────────────────────────────
function browserStub(cmd: string, _args?: Record<string, unknown>): unknown {
  switch (cmd) {
    case "kb_scan":
      return 0;
    case "kb_compile":
      return "kbc-stub";
    case "kb_search":
      return [];
    case "kb_list":
      return [];
    case "kb_read":
      return "_(browser stub)_  本文件需要 Tauri 后端读取。";
    case "kb_delete":
      return 0;
    case "kb_clear":
      return 0;
    case "kb_pack_list":
      return [];
    case "kb_pack_install":
    case "kb_pack_remove":
      return 0;
    case "kb_ingest":
      return "browser-stub";
    case "kb_convert_batch":
      return {
        total: 0,
        converted: 0,
        skippedVideo: 0,
        skippedOther: 0,
        failed: [],
      };
    case "kb_upload_files": {
      const paths = (_args?.paths as string[]) ?? [];
      return paths.map((p) => ({
        name: p.split(/[\\/]/).pop() || p,
        relPath: `raw/${p.split(/[\\/]/).pop() || p}`,
        ok: true,
        message: "(browser stub)",
      }));
    }
    case "chat_attach_image": {
      const name = String(_args?.name ?? "pasted.png");
      return { name, path: name, kind: "image", size: 0, ok: true };
    }
    case "open_url": {
      window.open(String(_args?.url ?? ""), "_blank", "noopener,noreferrer");
      return undefined;
    }
    case "chat_attach_files": {
      const paths = (_args?.paths as string[]) ?? [];
      return paths.map((p) => ({
        name: p.split(/[\\/]/).pop() || p,
        path: p,
        kind: "binary",
        size: 0,
        ok: true,
      }));
    }
    case "echo_briefing_today":
    case "echo_briefing_dismiss":
      return [];
    case "echo_dream_now":
    case "echo_briefing_run":
      return undefined;
    case "echo_status":
    case "echo_set":
      return {
        enabled: false,
        hour: 8,
        run_on_boot: true,
        last_dream_day: "",
        dreaming: false,
        memory_count: 0,
        briefing_today: 0,
        log: [],
      };
    case "kb_graph":
      return { nodes: [], edges: [] };
    case "kb_root":
      return "(browser-only, no fs access)";
    case "kb_default_root":
      return "(browser-only)";
    case "kb_set_root":
      return 0;
    case "sandbox_status":
      return {
        docker_installed: false,
        docker_running: false,
        image_built: false,
        image_name: "polaris-sandbox:alpine",
        container_running: false,
        container_name: "polaris-sandbox",
        notes: ["浏览器模式 - 仅 UI 预览,无 Docker 能力"],
      };
    case "sandbox_build_image":
    case "sandbox_start":
    case "sandbox_stop":
    case "sandbox_exec":
      return "(browser stub)";
    case "cube_config_get":
      return { backend: "docker", endpoint: "", apiKey: "" };
    case "cube_config_set":
      return (_args?.config as unknown) ?? { backend: "docker", endpoint: "", apiKey: "" };
    case "cube_status":
      return {
        backend: "docker",
        endpoint: "",
        configured: false,
        reachable: false,
        note: "浏览器模式 - 无后端探测",
      };
    case "chat_send":
      return "stub-req-id";
    case "artifact_read": {
      const path = String(_args?.path ?? "demo.html");
      return {
        path,
        name: path.split("/").pop() || path,
        ext: "html",
        kind: "html",
        text:
          "<!doctype html><html><body style='font-family:sans-serif;padding:40px;text-align:center'><h1>预览占位</h1><p>浏览器模式无后端，无法读取真实文件。</p></body></html>",
        size: 0,
      };
    }
    case "artifact_write":
      return undefined;
    case "artifact_open_external":
      return undefined;
    case "artifact_list":
      return [];
    case "artifact_search":
      return [];
    case "project_list":
      return [];
    case "project_status":
      return false;
    case "project_run":
    case "project_stop":
      return undefined;
    case "list_skills":
      return [
        { id: "deep-research", name: "深度搜索", description: "使用 LLM 大规模联网搜索相关内容，自动检索、汇总、交叉验证多来源信息", source: "third-party", installed: true, removable: false },
        { id: "skill-creator", name: "Skill 创建向导", description: "引导用户创建自定义 Skill，自动生成模板和配置文件", source: "official", installed: true, removable: false },
        { id: "pdf", name: "PDF 文档处理", description: "提取 / 生成 / 编辑 PDF：抽取文本表格、合并拆分、Markdown 转 PDF、表单与 OCR", source: "official", installed: false, removable: false },
        { id: "xlsx", name: "Excel 表格", description: "读取分析与生成 Excel：透视统计、公式、图表、多 sheet 报表", source: "official", installed: false, removable: false },
        { id: "pptx", name: "PPT 演示文稿", description: "把 PDF / 文档 / 数据转成有高级感的 PPT：母版配色、版式层级、图表，python-pptx 生成", source: "official", installed: false, removable: false },
        { id: "edge-tts", name: "语音合成 Edge-TTS", description: "把文本转成自然语音音频，多语言多音色，免费无需 key", source: "third-party", installed: false, removable: false },
        { id: "hyperframes", name: "视频动画 Hyperframes", description: "用逐帧 / 分镜方式生成短视频与动画，ffmpeg 合成，可配 Edge-TTS 旁白", source: "third-party", installed: false, removable: false },
        { id: "web-search", name: "联网搜索", description: "实时联网检索，基于 Tavily / Brave 等真实来源回答并交叉验证", source: "third-party", installed: false, removable: false },
        { id: "image-gen", name: "AI 生图 gpt-image-2", description: "用 OpenAI gpt-image-2 模型按描述生成图片，自动扩写提示词，支持多候选与改图", source: "third-party", installed: false, removable: false },
        { id: "cloak-browser", name: "CloakBrowser 浏览器", description: "Agent 默认浏览器：源码级隐身 Chromium，drop-in 替换 Playwright，过 Cloudflare / 反爬。可随时关闭移除", source: "third-party", installed: true, removable: false },
      ];
    case "get_skill":
      return { id: "deep-research", name: "深度搜索", description: "使用 LLM 大规模联网搜索相关内容", source: "third-party", installed: true, removable: false };
    case "import_skill":
      return ["browser-stub-skill"];
    case "create_skill":
    case "install_skill":
    case "delete_skill":
      return undefined;
    case "conv_list_projects":
      return [
        {
          id: "p-stub",
          name: "(浏览器) 示例项目",
          created_at: 0,
          archived: false,
        },
      ];
    case "conv_create_project":
      return {
        id: "p-stub-new",
        name: (_args?.name as string) || "新项目",
        created_at: 0,
        archived: false,
      };
    case "conv_list_conversations":
      return [];
    case "conv_create_conversation":
      return {
        id: "c-stub-new",
        project_id: _args?.projectId as string,
        title: "新对话",
        created_at: 0,
        updated_at: 0,
      };
    case "conv_get_messages":
      return [];
    case "conv_archive_project":
    case "conv_open_project_dir":
    case "conv_delete_conversation":
    case "conv_rename_conversation":
      return undefined;
    case "claude_md_list_projects":
      return [];
    case "claude_md_kb_info":
      return {
        absPath: "(browser-only)",
        exists: false,
        active: false,
        size: 0,
      };
    case "claude_md_read":
      return "_(browser stub)_  本文件需要 Tauri 后端读取。";
    case "claude_md_write":
      return undefined;
    case "conv_set_project_kb_scope":
    case "persona_apply":
      return undefined;
    case "feishu_get_config":
      return {
        enabled: false,
        appId: "",
        appSecret: "",
        autoStart: false,
        domain: "feishu",
        dmPolicy: "open",
        groupRequireMention: true,
        allowFrom: [],
      };
    case "feishu_set_config":
      return undefined;
    case "feishu_test_connection":
      return {
        ok: false,
        botName: "",
        botOpenId: "",
        message: "浏览器模式无法连接飞书，请在桌面应用中测试。",
      };
    case "feishu_create_qr":
      return {
        svg: "<svg xmlns='http://www.w3.org/2000/svg' width='240' height='240'><rect width='240' height='240' fill='#fff'/><text x='120' y='124' font-size='12' fill='#999' text-anchor='middle'>浏览器模式无二维码</text></svg>",
        url: "https://open.feishu.cn/app",
      };
    case "feishu_open_console":
      return undefined;
    case "wecom_scan_create":
      throw new Error("浏览器模式无法扫码创建，请在桌面应用中操作。");
    case "feishu_gateway_status":
      return { running: false };
    case "feishu_gateway_start":
      throw new Error("浏览器模式无法启动网关，请在桌面应用中操作。");
    case "feishu_gateway_stop":
      return undefined;
    case "persona_list":
      return [
        { id: "stock-expert", name: "股票助手", icon: "📈", description: "A 股深度分析 / 公告监控 / 行情查询。", kbScope: "raw/股票", body: "(browser stub)" },
        { id: "content-writer", name: "内容创作", icon: "✍️", description: "公众号/自媒体写手：选题、撰写、5 种风格。", kbScope: "raw/创作", body: "(browser stub)" },
        { id: "lesson-planner", name: "备课出卷", icon: "📚", description: "K12 教案/试卷/答案解析。", kbScope: "raw/教学", body: "(browser stub)" },
        { id: "content-summarizer", name: "内容总结", icon: "📋", description: "网页/文档/会议纪要结构化摘要。", kbScope: "", body: "(browser stub)" },
        { id: "health-interpreter", name: "医疗健康解读", icon: "🏥", description: "体检报告/化验单通俗解读。", kbScope: "raw/健康", body: "(browser stub)" },
        { id: "pet-care", name: "萌宠管家", icon: "🐾", description: "猫狗行为/健康/营养。", kbScope: "raw/萌宠", body: "(browser stub)" },
        { id: "mao", name: "毛主席", icon: "☭", description: "毛选式客观分析。", kbScope: "raw/毛主席", body: "(browser stub)" },
      ];
    case "provider_list": {
      const mk = (id: string, name: string, baseUrl: string, category: string, color: string, kind: string, hasKey: boolean, authToken = "") => ({
        id, name, note: "", baseUrl, tokenField: "ANTHROPIC_AUTH_TOKEN", category, websiteUrl: baseUrl, color, kind, isPreset: true, hasKey, authToken,
        settingsConfig: { env: baseUrl ? { ANTHROPIC_BASE_URL: baseUrl, ...(authToken ? { ANTHROPIC_AUTH_TOKEN: authToken } : {}) } : {} },
      });
      return {
        providers: [
          mk("claude-official", "Claude 官方", "", "official", "#D97757", "official", true),
          mk("zhipu-glm", "智谱 GLM", "https://open.bigmodel.cn/api/anthropic", "cn_official", "#2c6fff", "key", false),
          mk("kimi", "Kimi 月之暗面", "https://api.moonshot.cn/anthropic", "cn_official", "#2c6fff", "key", true, "sk-demo"),
          mk("deepseek", "DeepSeek 深度求索", "https://api.deepseek.com/anthropic", "cn_official", "#2c6fff", "key", false),
          mk("openrouter", "OpenRouter", "https://openrouter.ai/api", "aggregator", "#7c5cff", "key", false),
          mk("aihubmix", "AiHubMix", "https://aihubmix.com", "aggregator", "#7c5cff", "key", false),
          mk("packycode", "PackyCode", "https://www.packyapi.com", "third_party", "#e8833a", "key", false),
          mk("github-copilot", "GitHub Copilot", "https://api.githubcopilot.com", "third_party", "#e8833a", "copilot", false),
          mk("codex", "Codex (ChatGPT)", "https://chatgpt.com/backend-api/codex", "third_party", "#e8833a", "codex", false),
        ],
        currentId: "kimi",
        linkGlobal: false,
      };
    }
    case "provider_switch":
      return String(_args?.id ?? "claude-official");
    case "provider_set_link_mode":
      return Boolean(_args?.link);
    case "provider_save":
      return "custom-stub";
    case "provider_delete":
      return undefined;
    case "codex_status":
      return { installed: false, loggedIn: false, authPath: "(browser-only)" };
    case "codex_start_login":
      return {
        deviceCode: "stub-device",
        userCode: "WXYZ-1234",
        verificationUri: "https://auth.openai.com/codex/device",
        interval: 5,
        expiresIn: 900,
      };
    case "codex_poll_login":
      return { status: "ok" };
    case "codex_proxy_info":
      return { running: false, port: 0, lastError: "" };
    case "env_check": {
      const tool = (key: string, name: string, found: boolean, required = false): ToolStatus => ({
        key: key as ToolStatus["key"],
        name,
        found,
        version: found ? "(browser stub) v0.0.0" : null,
        path: found ? `/usr/local/bin/${key}` : null,
        onPath: found,
        required,
        hint: found ? "(browser stub) 已安装" : "未安装 —— 浏览器预览无法真实检测",
      });
      return {
        os: "browser",
        claude: tool("claude", "Claude Code", false, true),
        pwsh: tool("pwsh", "PowerShell 7", false),
        node: tool("node", "Node.js", true),
        npm: tool("npm", "npm", true),
        claudeDir: null,
        claudeDirOnUserPath: true,
        shellReady: false,
        ready: false,
      };
    }
    case "env_fix_path":
      return {
        ok: false,
        dir: null,
        status: "skipped",
        message: "浏览器预览模式无法修改环境变量。",
      };
    case "env_install_claude":
    case "env_install_node":
    case "env_install_pwsh":
    case "env_update_claude":
      return "env-stub-req";
    case "env_claude_update_check":
      return {
        installed: true,
        current: "1.0.0",
        latest: "1.0.1",
        updateAvailable: true,
        checked: true,
        message: "(browser stub) 发现新版本 1.0.1 (当前 1.0.0)。",
      };
    case "env_cancel":
      return undefined;
    case "usage_summary": {
      const daily = Array.from({ length: 14 }, (_, i) => {
        const d = new Date(Date.now() - (13 - i) * 86400000);
        const label = `${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
        return { date: label, label, total: Math.round(300000 + Math.random() * 1600000), cost: +(Math.random() * 6).toFixed(4) };
      });
      return {
        available: true,
        today: { input: 75600, output: 644800, cacheRead: 45506800, cacheCreation: 1637200, total: 720483 + 47144001, requests: 411, cost: 49.107 },
        week: { input: 280000, output: 64000, cacheRead: 6100000, cacheCreation: 410000, total: 6854000, requests: 248, cost: 112.4 },
        month: { input: 980000, output: 240000, cacheRead: 22000000, cacheCreation: 1400000, total: 24620000, requests: 940, cost: 421.8 },
        year: { input: 1900000, output: 520000, cacheRead: 44000000, cacheCreation: 2800000, total: 49220000, requests: 1894, cost: 980.5 },
        daily,
      };
    }
    case "expert_list":
    case "expert_groups":
    case "expert_match_auto":
    case "expert_route":
    case "expert_list_by_group":
    case "expert_team_spawn":
    case "expert_agents_status":
    case "expert_teams":
    case "expert_route_debug":
      return [];
    case "expert_avatar_slots":
      return [];
    case "expert_avatar":
    case "expert_get":
    case "expert_team_get":
      return null;
    case "expert_export":
    case "team_export":
      return "";
    case "expert_recommend_from_kb":
      return { team: null, reason: "(browser stub)", topExperts: [], matchedTopics: [], corpusSize: 0 };
    default:
      return null;
  }
}