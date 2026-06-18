import { defineStore } from "pinia";
import { reactive, computed, ref } from "vue";
import { files as fc, listen, invoke } from "../tauri";

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
//   AI 归类   file:cluster_llm  {kind: phase(text) / done(clusters,assigned) / error}
//   AI 整理名 file:title_llm    {kind: phase(text) / done(count) / error}

export type FileTaskId = "inventory" | "index" | "cluster" | "clusterLlm" | "titles" | "ontology";

const LABELS: Record<FileTaskId, string> = {
  inventory: "盘点磁盘",
  index: "建向量索引",
  cluster: "智能归类",
  clusterLlm: "AI 语义归类",
  titles: "AI 整理名称",
  ontology: "构建知识体系",
};

export const useFileTasksStore = defineStore("fileTasks", () => {
  const ids: FileTaskId[] = ["inventory", "index", "cluster", "clusterLlm", "titles", "ontology"];
  const mk = <T,>(v: T) => Object.fromEntries(ids.map((k) => [k, v])) as Record<FileTaskId, T>;

  const running = reactive<Record<FileTaskId, boolean>>(mk(false));
  const detail = reactive<Record<FileTaskId, string>>(mk(""));
  const failed = reactive<Record<FileTaskId, boolean>>(mk(false));
  // done 自增 → 组件 watch 刷新对应数据(总览 / 网格)。
  const doneTick = reactive<Record<FileTaskId, number>>(mk(0));
  // 上一轮盘点中「连不上、已跳过」的根(群晖 NAS / 拔掉的外置盘等)。文件中心 watch doneTick.inventory
  // 后读它弹温和提示框,提醒用户「这些没扫到」,而不是误以为盘点完成 = 全都扫到了。
  const lastUnreachable = ref<string[]>([]);

  function begin(id: FileTaskId, msg: string) {
    running[id] = true;
    failed[id] = false;
    detail[id] = msg;
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
      await listen<{ kind: string; files?: number; message?: string; unreachable?: string[] }>(
        "fable:inventory",
        (p) => {
          if (p.kind === "progress") detail.inventory = `已盘点 ${p.files ?? 0} 个文件…`;
          else if (p.kind === "done") {
            lastUnreachable.value = p.unreachable ?? [];
            finish("inventory", `盘点完成 · ${p.files ?? 0} 个文件`);
          } else if (p.kind === "error") {
            // 用户主动取消时后端发的也是 error{message:"已取消"};这是「停止」不是「失败」,
            // 按 done 口径优雅收尾(否则任务中心红字报「盘点失败」,误导)。
            if ((p.message ?? "").includes("已取消")) finish("inventory", "盘点已停止");
            else fail("inventory", `盘点失败:${p.message ?? ""}`);
          }
        },
      ),
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
      await listen<{
        kind: string;
        text?: string;
        tier?: string;
        clusters?: number;
        note?: string;
        message?: string;
      }>("file:cluster", (p) => {
        if (p.kind === "phase") detail.cluster = p.text ?? detail.cluster;
        else if (p.kind === "tick") {
          /* 心跳(AI 输出中):保活,不改文案 */
        } else if (p.kind === "tier") {
          // v3 某一档(骨架/AI 初级/语义精修)完成 → 刷新星图/网格(doneTick),但任务继续。
          if (p.note) detail.cluster = p.note;
          doneTick.cluster++;
        } else if (p.kind === "done") {
          finish("cluster", p.note || "归类完成");
        } else if (p.kind === "error") fail("cluster", `归类失败:${p.message ?? ""}`);
      }),
    );
    unlisteners.push(
      await listen<{ kind: string; text?: string; clusters?: number; assigned?: number; message?: string }>(
        "file:cluster_llm",
        (p) => {
          if (p.kind === "phase") detail.clusterLlm = p.text ?? "";
          else if (p.kind === "done") {
            finish(
              "clusterLlm",
              `AI 归类完成 · ${p.clusters ?? 0} 个子主题 · ${p.assigned ?? 0} 个文件已归类`,
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
    // 框架派(D 方案)Schema-Guided 抽取:fable:ontology {phase / tick / done(kept,note) / error}。
    unlisteners.push(
      await listen<{ kind: string; text?: string; kept?: number; note?: string; message?: string }>(
        "fable:ontology",
        (p) => {
          if (p.kind === "phase") detail.ontology = p.text ?? "";
          else if (p.kind === "tick") detail.ontology = "模型正在框内抽取关系…";
          else if (p.kind === "done") finish("ontology", p.note || `已抽出 ${p.kept ?? 0} 条关系`);
          else if (p.kind === "error") fail("ontology", `构建失败:${p.message ?? ""}`);
        },
      ),
    );
  }

  // ── 启动各任务(进行中重复调用直接忽略,后端 FlagGuard 也会兜底拒绝双发)──
  async function startInventory(roots: string[], exclude: string[], full = false) {
    if (running.inventory) return;
    await ensureListeners();
    const how = full ? "完整盘点(逐目录重扫)" : "盘点磁盘";
    begin("inventory", exclude.length ? `正在${how}(已跳过 ${exclude.length} 个文件夹)…` : `正在${how}…`);
    try {
      await fc.inventoryStart(roots, exclude, full);
    } catch (e: any) {
      fail("inventory", `盘点失败:${e?.message ?? e}`);
    }
  }
  async function startIndex() {
    if (running.index) return;
    // 用户手动开建 → 解除「开机不自动续建」的暂停标记(见 App.vue 的开机续建)。
    try { localStorage.removeItem("polaris.indexAutoPaused"); } catch { /* ignore */ }
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
  // 统一「智能归类」入口(v3 渐进式):后端一条龙 T0 秒级骨架 → T1 AI 初级命名+关系 →
  // T2 全量向量化后语义重聚再命名,全程后台、进度走 file:cluster 事件(tier 分档)。
  // 配不配嵌入 key 都点得动:没配则止于结构骨架 + AI 命名;配了自动接 T2 精修。
  // (hasEmbedProvider 参数保留兼容旧调用,实际由后端按服务商在不在自行决定是否做 T2。)
  async function startSmartCluster(_hasEmbedProvider?: boolean) {
    if (running.cluster || running.clusterLlm) return;
    await ensureListeners();
    begin("cluster", "正在启动智能归类…");
    try {
      await fc.smartCluster(null);
    } catch (e: any) {
      fail("cluster", `归类失败:${e?.message ?? e}`);
    }
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
  // 框架派(D 方案):在某行业 schema 框内抽实体关系三元组(企业知识库构建)。
  async function startOntology(schemaId: string) {
    if (running.ontology) return;
    await ensureListeners();
    begin("ontology", "正在行业框内抽取实体与关系…");
    try {
      await invoke("ontology_extract", { schemaId });
    } catch (e: any) {
      fail("ontology", `构建失败:${e?.message ?? e}`);
    }
  }

  // 停止/关闭某后台任务。
  //   盘点 / 建索引:走协作式取消(fable_cancel —— 循环每几百 ms 轮询 CANCEL 优雅停)。
  //     索引是「可续建」的:停了再点一次即从断点继续 → 等价于「暂停 / 继续」。
  //   AI 归类 / 整理名 / 构建体系:后端暂不轮询取消(且多为一次性 LLM 调用),这里把它从
  //     面板乐观收起,后台那一轮会自然跑完,不影响已落库数据。
  async function cancel(id: FileTaskId) {
    if (!running[id]) return;
    detail[id] = "正在停止…";
    // 主动停索引 → 记住,下次开机不自动续建(否则关了又自己跑起来,等于关不掉)。
    // 手动再点「建索引」会清掉这个标记(见 startIndex)。
    if (id === "index") {
      try { localStorage.setItem("polaris.indexAutoPaused", "1"); } catch { /* ignore */ }
    }
    try {
      await fc.fableCancel();
    } catch {
      /* 取消是尽力而为,失败也不抛给用户 */
    }
    if (id !== "inventory" && id !== "index") finish(id, "已停止");
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
    lastUnreachable,
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
    startOntology,
    cancel,
  };
});
