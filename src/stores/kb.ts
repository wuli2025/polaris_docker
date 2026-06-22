import { defineStore } from "pinia";
import { ref } from "vue";
import {
  kb,
  listen,
  type KbCompileEvent,
  type KbMaintainEvent,
  type KbLintReport,
  type KbThreatReport,
} from "../tauri";

// 「构建知识网」全局状态。
// 后端 kb_compile 本就是独立线程 + 全局事件,离开 wiki 视图进程不会停;
// 之前进度锁在 WikiBrowse 组件里,组件一卸载就退订+清零,看起来像停了。
// 把状态+监听抬到这里 → 监听只注册一次、脱离任何组件生命周期,
// 切走切回甚至关掉 wiki 视图,进度都在后台继续累积,回来即见。
export const useKbStore = defineStore("kb", () => {
  const compiling = ref(false);
  const compileLog = ref<string[]>([]);
  // 内存治理:日志数组封顶 200 行,且单条裁到 2000 字 —— 否则异常堆栈 / 整页文本回显这类
  // 超长单条 × 200 行仍能堆出几十 MB。集中走这个 helper,两个监听器(compile / enrich)共用。
  function pushCompileLog(line: string) {
    compileLog.value.push(line.length > 2000 ? line.slice(0, 2000) + " …(已截断)" : line);
    if (compileLog.value.length > 200)
      compileLog.value.splice(0, compileLog.value.length - 200);
  }
  const compileMsg = ref("");
  const compileRunId = ref("");
  // 编译后重扫的文档总数(done 时回填),供 WikiBrowse 更新计数
  const lastDocCount = ref<number | null>(null);
  // 每次编译完成自增 → WikiBrowse watch 它来刷新文件列表
  const doneTick = ref(0);

  // ── 一键流水线: 构建(编译) → 自动补双链 → 智能去重 ──
  // 三步本是互补的同一件事: 编译产出新页, 补双链把新旧页连通,
  // 去重收掉编译可能产生的重复页。每阶段是独立后端 run, done 时串起下一阶段。
  const pipelineStage = ref<"" | "compile" | "enrich" | "dedup">("");

  let unlisten: (() => void) | null = null;
  // 同步「正在挂载」闸:guard 若只看 unlisten,它要等 await listen() 兑现后才被赋值,
  // 两个并发调用会在 await 之前都通过 guard → 重复订阅(同一事件回调触发两次)。在 await 前
  // 同步置真才真正幂等(fileTasks.ts 的 wired 同款做法)。
  let wiringCompile = false;

  // 全局只注册一次 kb:compile 监听
  async function ensureListener() {
    if (unlisten || wiringCompile) return;
    wiringCompile = true;
    unlisten = await listen<KbCompileEvent>("kb:compile", (ev) => {
      // invoke 回执(run_id)可能比后端首个事件晚到 → 此刻 compileRunId 仍空, 早到的事件(含
      // done)会被下面的 runId 过滤丢弃, 致 compiling 永卡 true。运行中且尚未拿到 id 时,
      // 采纳首个事件的 runId(同一时刻只可能有一个 run, 有 compiling 与后端忙标志双重串行化)。
      if (!compileRunId.value && compiling.value) compileRunId.value = ev.runId;
      if (ev.runId !== compileRunId.value) return;
      const t = ev.text ?? "";
      if (ev.kind === "done") {
        if (typeof ev.docCount === "number") lastDocCount.value = ev.docCount;
        doneTick.value++;
        if (pipelineStage.value === "compile") {
          advancePipeline("enrich", t);
          return;
        }
        compiling.value = false;
        compileMsg.value = t || "完成";
        return;
      }
      const icon =
        ev.kind === "error"
          ? "⚠ "
          : ev.kind === "page"
            ? "📄 "
            : ev.kind === "phase"
              ? "▸ "
              : "· ";
      pushCompileLog(icon + t);
    });
  }

  // 启动一次构建知识网。进行中重复调用直接忽略(后端进程仍在跑)。
  async function startCompile() {
    if (compiling.value) return;
    compiling.value = true;
    compileMsg.value = "";
    compileLog.value = [];
    lastDocCount.value = null;
    pipelineStage.value = "";
    await ensureListener();
    try {
      compileRunId.value = ""; // 清空: 让 await 期间早到的事件能被 adopt(见监听器)
      compileRunId.value = await kb.compile();
    } catch (e: any) {
      compiling.value = false;
      compileMsg.value = "启动失败:" + (e?.message ?? e);
    }
  }

  // 一键流水线入口: 编译 → 补双链 → 去重, 共用同一块进度日志。
  async function startBuildAll() {
    if (compiling.value) return;
    compiling.value = true;
    compileMsg.value = "";
    compileLog.value = ["▸ 第 1/3 步:构建知识网(摄入即编译)…"];
    lastDocCount.value = null;
    pipelineStage.value = "compile";
    await ensureListener();
    await ensureMaintainListener();
    try {
      compileRunId.value = "";
      compileRunId.value = await kb.compile();
    } catch (e: any) {
      pipelineStage.value = "";
      compiling.value = false;
      compileMsg.value = "启动失败:" + (e?.message ?? e);
    }
  }

  // 上一阶段 done → 记录其结果并启动下一阶段; 启动失败则就地终止整条流水线。
  async function advancePipeline(next: "enrich" | "dedup", prevMsg: string) {
    if (prevMsg) compileLog.value.push("· " + prevMsg);
    compileLog.value.push(
      next === "enrich" ? "▸ 第 2/3 步:自动补双链…" : "▸ 第 3/3 步:智能去重…"
    );
    pipelineStage.value = next;
    try {
      compileRunId.value = "";
      compileRunId.value =
        next === "enrich" ? await kb.enrichLinks() : await kb.dedup();
    } catch (e: any) {
      pipelineStage.value = "";
      compiling.value = false;
      compileMsg.value = "流水线中断:" + (e?.message ?? e);
    }
  }

  // ── 维护知识网: 自动补双链 (enrich) / 智能去重 (dedup) ──
  // 借鉴 llm_wiki「AI 出决策、代码执行」。复用上面的进度日志 UI (同时只跑一个维护操作)。
  let unlistenMaintain: (() => void)[] = [];
  // 同步幂等闸(理由同 wiringCompile):数组要等两个 await listen() 都兑现才非空,
  // 并发调用会在此之前都通过 length 判断 → enrich/dedup 各被订阅两次。
  let wiringMaintain = false;
  async function ensureMaintainListener() {
    if (unlistenMaintain.length || wiringMaintain) return;
    wiringMaintain = true;
    const handle = (ev: KbMaintainEvent) => {
      // 同 kb:compile: 回执晚到时采纳首个事件的 runId, 防早到的 done 被丢、卡死 compiling。
      if (!compileRunId.value && compiling.value) compileRunId.value = ev.runId;
      if (ev.runId !== compileRunId.value) return;
      const t = ev.text ?? "";
      if (ev.kind === "done") {
        doneTick.value++;
        if (pipelineStage.value === "enrich") {
          advancePipeline("dedup", t);
          return;
        }
        if (pipelineStage.value === "dedup") {
          pipelineStage.value = "";
          compiling.value = false;
          compileMsg.value =
            "全部完成:编译 → 补双链 → 去重" + (t ? `(${t})` : "");
          compileLog.value.push("· " + (t || "去重完成"));
          return;
        }
        compiling.value = false;
        compileMsg.value = t || "完成";
        return;
      }
      const icon =
        ev.kind === "error" ? "⚠ " : ev.kind === "phase" ? "▸ " : "· ";
      pushCompileLog(icon + t);
    };
    unlistenMaintain.push(await listen<KbMaintainEvent>("kb:enrich", handle));
    unlistenMaintain.push(await listen<KbMaintainEvent>("kb:dedup", handle));
  }

  async function startMaintain(kind: "enrich" | "dedup") {
    if (compiling.value) return;
    compiling.value = true;
    compileMsg.value = "";
    compileLog.value = [kind === "enrich" ? "▸ 自动补双链…" : "▸ 智能去重…"];
    lastDocCount.value = null;
    await ensureMaintainListener();
    try {
      compileRunId.value = "";
      compileRunId.value =
        kind === "enrich" ? await kb.enrichLinks() : await kb.dedup();
    } catch (e: any) {
      compiling.value = false;
      compileMsg.value = "启动失败:" + (e?.message ?? e);
    }
  }

  // ── wiki 质量检查 (lint): 同步返回报告 ──
  const lintReport = ref<KbLintReport | null>(null);
  const linting = ref(false);
  async function runLint() {
    linting.value = true;
    try {
      lintReport.value = await kb.lint();
    } finally {
      linting.value = false;
    }
  }

  // ── 信源安全扫描 (提示词注入痕迹): 同步返回报告 + 隔离动作 ──
  const threatReport = ref<KbThreatReport | null>(null);
  const scanning = ref(false);
  async function runScan() {
    scanning.value = true;
    try {
      threatReport.value = await kb.scanSources();
    } finally {
      scanning.value = false;
    }
  }
  async function quarantine(relPath: string) {
    await kb.quarantine(relPath);
    // 隔离后重扫，命中清单即时刷新
    await runScan();
  }

  return {
    compiling,
    compileLog,
    compileMsg,
    compileRunId,
    pipelineStage,
    lastDocCount,
    doneTick,
    ensureListener,
    startCompile,
    startBuildAll,
    startMaintain,
    lintReport,
    linting,
    runLint,
    threatReport,
    scanning,
    runScan,
    quarantine,
  };
});
