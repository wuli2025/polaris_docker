import { defineStore } from "pinia";
import { reactive, computed } from "vue";
import { files as fc, listen } from "../tauri";

// 文件中心长任务的全局状态枢纽。
//
// 沿用 [`useKbStore`]「构建知识网」的同一思路(见 stores/kb.ts 顶部注释):后端这些活儿
// 本就是独立后台线程 + 全局事件,离开文件中心视图进程不会停;但旧实现把进度/监听锁在
// FileCenter.vue 组件里,组件一卸载(切去别的页)就退订 + 清零,看起来像「停了」。
// 把状态 + 监听抬到这个 store →
//   ① 监听只注册一次、脱离任何组件生命周期 → 切走切回甚至从没打开过文件中心,事件都不丢;
//   ② 任意组件(文件中心 / 全局任务中心浮层)都能读同一份运行态 → 处处可见「还在跑」;
//   ③ done 时自增对应 doneTick → 关心的视图 watch 它来刷新数据。
//
// 后端事件契约:
//   盘点      fable:inventory   {kind: progress(files,bytes) / done(files,...) / error(message)}
//   建索引    fable:index       {kind: progress(files,chunks) / done(files,stopped) / error}
//   智能归类  file:cluster      {kind: phase(text) / done(clusters,files,note) / error}     ← 本轮改后台
//   AI 归类   file:cluster_llm  {kind: phase(text) / done(clusters,assigned,report) / error}
//   AI 整理名 file:title_llm    {kind: phase(text) / done(count) / error}

export type FileTaskId = "inventory" | "index" | "cluster" | "clusterLlm" | "titles";

const LABELS: Record<FileTaskId, string> = {
  inventory: "盘点磁盘",
  index: "建向量索引",
  cluster: "智能归类",
  clusterLlm: "AI 语义归类",
  titles: "AI 整理名称",
};

export const useFileTasksStore = defineStore("fileTasks", () => {
  const ids: FileTaskId[] = ["inventory", "index", "cluster", "clusterLlm", "titles"];
  const mk = <T,>(v: T) => Object.fromEntries(ids.map((k) => [k, v])) as Record<FileTaskId, T>;

  const running = reactive<Record<FileTaskId, boolean>>(mk(false));
  const detail = reactive<Record<FileTaskId, string>>(mk(""));
  const failed = reactive<Record<FileTaskId, boolean>>(mk(false));
  // done 自增 → 组件 watch 刷新对应数据(总览 / 网格)。
  const doneTick = reactive<Record<FileTaskId, number>>(mk(0));
  // AI 归类完成后的桌面报告路径。
  const reportPath = reactive<Record<FileTaskId, string>>(mk(""));

  function begin(id: FileTaskId, msg: string) {
    running[id] = true;
    failed[id] = false;
    detail[id] = msg;
    reportPath[id] = "";
  }
  function finish(id: FileTaskId, msg: string) {
    running[id] = false;
    detail[id] = msg;
    doneTick[id]++;
  }
  function fail(id: FileTaskId, msg: string) {
    running[id] = false;
    failed[id] = true;
    detail[id] = msg;
  }

  let wired = false;
  const unlisteners: Array<() => void> = [];
  // 全局只注册一次;脱离组件生命周期,App 启动时调一次,之后永久在线。
  async function ensureListeners() {
    if (wired) return;
    wired = true;
    unlisteners.push(
      await listen<{ kind: string; files?: number; message?: string }>("fable:inventory", (p) => {
        if (p.kind === "progress") detail.inventory = `已盘点 ${p.files ?? 0} 个文件…`;
        else if (p.kind === "done") finish("inventory", `盘点完成 · ${p.files ?? 0} 个文件`);
        else if (p.kind === "error") fail("inventory", `盘点失败:${p.message ?? ""}`);
      }),
    );
    unlisteners.push(
      await listen<{ kind: string; files?: number; chunks?: number; stopped?: string; message?: string }>(
        "fable:index",
        (p) => {
          if (p.kind === "progress") detail.index = `已嵌入 ${p.files ?? 0} 文件 · ${p.chunks ?? 0} chunk…`;
          else if (p.kind === "done") finish("index", `索引完成 · 本轮 ${p.files ?? 0} 文件 · ${p.stopped ?? ""}`);
          else if (p.kind === "error") fail("index", `索引失败:${p.message ?? ""}`);
        },
      ),
    );
    unlisteners.push(
      await listen<{ kind: string; text?: string; clusters?: number; note?: string; message?: string }>(
        "file:cluster",
        (p) => {
          if (p.kind === "phase") detail.cluster = p.text ?? "";
          else if (p.kind === "done") finish("cluster", p.note || "归类完成");
          else if (p.kind === "error") fail("cluster", `归类失败:${p.message ?? ""}`);
        },
      ),
    );
    unlisteners.push(
      await listen<{ kind: string; text?: string; clusters?: number; assigned?: number; report?: string; message?: string }>(
        "file:cluster_llm",
        (p) => {
          if (p.kind === "phase") detail.clusterLlm = p.text ?? "";
          else if (p.kind === "done") {
            reportPath.clusterLlm = p.report ?? "";
            finish(
              "clusterLlm",
              `AI 归类完成 · ${p.clusters ?? 0} 个子主题 · ${p.assigned ?? 0} 个文件已归类 · 报告已存桌面`,
            );
          } else if (p.kind === "error") fail("clusterLlm", `AI 归类失败:${p.message ?? ""}`);
        },
      ),
    );
    unlisteners.push(
      await listen<{ kind: string; text?: string; count?: number; message?: string }>("file:title_llm", (p) => {
        if (p.kind === "phase") detail.titles = p.text ?? "";
        else if (p.kind === "done") finish("titles", `AI 整理完成 · 已为 ${p.count ?? 0} 个文件生成智能标题`);
        else if (p.kind === "error") fail("titles", `AI 整理失败:${p.message ?? ""}`);
      }),
    );
  }

  // ── 启动各任务(进行中重复调用直接忽略,后端 FlagGuard 也会兜底拒绝双发)──
  async function startInventory(roots: string[], exclude: string[]) {
    if (running.inventory) return;
    await ensureListeners();
    begin("inventory", exclude.length ? `正在盘点(已跳过 ${exclude.length} 个文件夹)…` : "正在盘点磁盘…");
    try {
      await fc.inventoryStart(roots, exclude);
    } catch (e: any) {
      fail("inventory", `盘点失败:${e?.message ?? e}`);
    }
  }
  async function startIndex() {
    if (running.index) return;
    await ensureListeners();
    begin("index", "正在构建向量索引(硅基 BGE-M3 滴灌嵌入)…");
    try {
      await fc.indexStart();
    } catch (e: any) {
      fail("index", `索引失败:${e?.message ?? e}`);
    }
  }
  async function startCluster() {
    if (running.cluster) return;
    await ensureListeners();
    begin("cluster", "正在把相似文件归类…");
    try {
      await fc.clusterBuild(null);
    } catch (e: any) {
      fail("cluster", `归类失败:${e?.message ?? e}`);
    }
  }
  async function startClusterLlm() {
    if (running.clusterLlm) return;
    await ensureListeners();
    begin("clusterLlm", "正在用大模型按语义归类(读文件清单 → 主题分组)…");
    try {
      await fc.clusterLlm(null);
    } catch (e: any) {
      fail("clusterLlm", `AI 归类失败:${e?.message ?? e}`);
    }
  }
  // 统一「智能归类」入口:配了嵌入 key → AI 语义归类(出桌面报告);没配 → 离线纯数学归类。
  async function startSmartCluster(hasEmbedProvider: boolean) {
    if (running.cluster || running.clusterLlm) return;
    if (hasEmbedProvider) await startClusterLlm();
    else await startCluster();
  }
  async function startTitles() {
    if (running.titles) return;
    await ensureListeners();
    begin("titles", "正在用大模型给文件起可读标题(读文件清单 → 起名)…");
    try {
      await fc.titlesLlm(null);
    } catch (e: any) {
      fail("titles", `AI 整理失败:${e?.message ?? e}`);
    }
  }

  // 任一归类(离线 / AI)进行中。
  const clustering = computed(() => running.cluster || running.clusterLlm);
  const anyRunning = computed(() => ids.some((k) => running[k]));
  // 全局任务中心浮层用:正在跑的任务列表(带可读标签 + 实时进度文案)。
  const activeList = computed(() =>
    ids.filter((k) => running[k]).map((k) => ({ id: k, label: LABELS[k], detail: detail[k] })),
  );

  return {
    running,
    detail,
    failed,
    doneTick,
    reportPath,
    clustering,
    anyRunning,
    activeList,
    ensureListeners,
    startInventory,
    startIndex,
    startCluster,
    startClusterLlm,
    startSmartCluster,
    startTitles,
  };
});
