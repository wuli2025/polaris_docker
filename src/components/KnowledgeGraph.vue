<script setup lang="ts">
import { ref, onMounted, onUnmounted, onActivated, onDeactivated, nextTick } from "vue";
import cytoscape, { type Core } from "cytoscape";
// @ts-ignore — cytoscape-fcose 无类型声明
import fcose from "cytoscape-fcose";
import { kb, files as filesApi, type KbGraph, type KbNode } from "../tauri";

// KeepAlive 的 include 按组件 name 匹配 → 显式命名，确保本视图被缓存
defineOptions({ name: "KnowledgeGraph" });
// source: 'kb'=知识库 wiki 图谱(默认);'files'=文件中心星图(语义簇+文件星点)。
// embedded: 嵌进向导/弹层时隐掉自带大标题栏(宿主自带标题)。
const props = withDefaults(
  defineProps<{ source?: "kb" | "files"; embedded?: boolean }>(),
  { source: "kb", embedded: false },
);
// 布局稳定(或空/兜底)时通知 App 收起加载条
const emit = defineEmits<{ ready: [] }>();
const isFiles = props.source === "files";

// fcose 力导向引擎: 中心引力 → 有机圆盘云团 (银河感), 自动打包孤立分量
try {
  cytoscape.use(fcose);
} catch {
  /* HMR 下重复注册会抛错, 忽略 */
}

const container = ref<HTMLDivElement | null>(null);
const stats = ref({ docs: 0, folders: 0, edges: 0, memories: 0 });
const empty = ref(false);
const showFolders = ref(true);
const spinning = ref(true); // 初始即自动旋转
const query = ref("");
const selected = ref<{
  title: string;
  kind: KbNode["kind"];
  path: string;
  deg: number;
  summary?: string;
} | null>(null);

let cy: Core | null = null;
let graphData: KbGraph | null = null;
let rafId = 0;
let spinLastT = 0;
let spinCx = 0;
let spinCy = 0;

// ── 恒星色谱 (核心亮色 → 外缘辉光色) ────────────────────────
type Star = { grad: string; glow: string };
const PAL: Record<string, Star> = {
  root: { grad: "#fff6e0 #efa838", glow: "#f0b24a" }, // 星系核心 · 暖金
  folder: { grad: "#e6f2ff #5fa8e6", glow: "#5fa8e6" }, // 亮蓝巨星
  doc: { grad: "#dbe7ff #4f7fd0", glow: "#4f7fd0" }, // 蓝白主序星
  feedback: { grad: "#ffe0ec #f0567f", glow: "#f0567f" }, // 玫红 · 回声记忆(对话沉淀)
  概念: { grad: "#d6e6ff #5577d8", glow: "#5577d8" },
  课程: { grad: "#fff0cf #e0a23c", glow: "#e0a23c" },
  人物: { grad: "#ecdcff #9a6fe0", glow: "#9a6fe0" },
  事件: { grad: "#ffd9cf #e0654a", glow: "#e0654a" },
  方法: { grad: "#d6ecff #52a6d6", glow: "#52a6d6" }, // 天蓝, 避开突兀的绿
};
function palKey(n: KbNode): string {
  if (n.kind === "root") return "root";
  if (n.kind === "folder") return "folder";
  if (n.kind === "feedback") return "feedback"; // 回声记忆走玫红,不按 category
  return PAL[n.category] ? n.category : "doc";
}
function nodeSize(kind: KbNode["kind"], deg: number): number {
  if (kind === "root") return 50;
  if (kind === "folder") return Math.min(40, Math.max(22, 20 + deg * 1.4));
  return Math.min(32, Math.max(13, 11 + deg * 3));
}
// 辉光: 基础偏弱, 随星体大小放大 (大节点更亮), 再叠 ±不规则抖动 → 自然不机械
function glowOpacity(kind: KbNode["kind"], size: number): number {
  const base = kind === "root" ? 0.3 : kind === "folder" ? 0.16 : 0.075;
  const sizeBoost = (Math.max(0, size - 13) / 37) * 0.1;
  const jitter = 0.82 + Math.random() * 0.36;
  return Math.min(0.42, (base + sizeBoost) * jitter);
}
function glowPad(size: number): number {
  const jitter = 0.8 + Math.random() * 0.4;
  return Math.max(3, Math.round(size * 0.42 * jitter));
}

// fcose 力导向布局是同步重计算:numIter 轮迭代全在主线程上跑,几千节点 × 2500 轮可达秒级卡顿
// (代码里曾兜底 3.5s 才收加载条,等于自认会卡)。改成「按节点数自适应」:节点越多,迭代越少、
// 质量降档(fcose 的 quality:"draft" 比 "default" 快数倍),把单次布局的主线程占用压在百毫秒量级 ——
// 节点多时画面精度略降(肉眼几乎无感),但绝不再让「打开/重排星图」把整个 UI 卡死。
function layoutFor(nodeCount: number): any {
  const big = nodeCount > 1200;
  const mid = nodeCount > 300;
  return {
    name: "fcose",
    quality: big ? "draft" : "default",
    animate: !big, // 大图不做入场动画,直接定位,省一段主线程动画开销
    animationDuration: 800,
    randomize: true,
    fit: true,
    padding: 60,
    nodeRepulsion: 6500,
    idealEdgeLength: 72,
    edgeElasticity: 0.45,
    gravity: 0.6, // 向心 → 收成圆盘
    gravityRange: 3.6,
    gravityCompound: 1.2,
    gravityRangeCompound: 1.5,
    // 迭代数随规模递减:小图 2500(精)、中图 1000、大图 400(快)
    numIter: big ? 400 : mid ? 1000 : 2500,
    packComponents: true,
    nodeSeparation: 80,
    nodeDimensionsIncludeLabels: false,
  };
}
// 安全上限:超过此节点数只渲染「连接最多的前 N 个」,避免 20 万节点的病态库把布局/渲染卡死。
const MAX_GRAPH_NODES = 2500;

// 颜色按字面量 selector 下发 (避免 data() 颜色映射的运行时风险)
const palSelectors = Object.entries(PAL).map(([k, v]) => ({
  selector: `node[pal = "${k}"]`,
  style: {
    "background-gradient-stop-colors": v.grad,
    "background-color": v.glow, // 渐变不支持时的兜底
    "underlay-color": v.glow,
  },
}));

// ── 构建 / 重建图谱 ─────────────────────────────────────────
function render() {
  if (!graphData || !container.value) return;
  cancelSpinLoop();
  if (cy) {
    cy.destroy();
    cy = null;
  }

  const keepFolders = showFolders.value;
  // 文档与回声记忆始终显示;目录/根节点仅在「目录结构」开启时显示
  let nodes = graphData.nodes.filter(
    (n) => keepFolders || n.kind === "doc" || n.kind === "feedback"
  );
  let keepIds = new Set(nodes.map((n) => n.id));
  let edges = graphData.edges.filter(
    (e) => keepIds.has(e.source) && keepIds.has(e.target)
  );

  // 连接数 (节点度) → 星体大小, 链接越多的节点越亮越大
  const deg: Record<string, number> = {};
  for (const e of edges) {
    deg[e.source] = (deg[e.source] || 0) + 1;
    deg[e.target] = (deg[e.target] || 0) + 1;
  }

  // 安全上限:超大图(几万~几十万节点)布局/渲染会卡死主线程。只保留「连接最多的前 N 个」
  // (度数=语义/结构枢纽,留它们最能代表全局),并据此重算保留集,把规模钉在可流畅渲染的量级。
  if (nodes.length > MAX_GRAPH_NODES) {
    nodes = [...nodes]
      .sort((a, b) => (deg[b.id] || 0) - (deg[a.id] || 0))
      .slice(0, MAX_GRAPH_NODES);
    keepIds = new Set(nodes.map((n) => n.id));
    edges = edges.filter((e) => keepIds.has(e.source) && keepIds.has(e.target));
  }

  stats.value = {
    docs: nodes.filter((n) => n.kind === "doc").length,
    folders: nodes.filter((n) => n.kind === "folder").length,
    edges: edges.length,
    memories: nodes.filter((n) => n.kind === "feedback").length,
  };

  cy = cytoscape({
    container: container.value,
    minZoom: 0.1,
    maxZoom: 3,
    // 滚轮缩放灵敏度:用户反馈原 0.85 太肉 → 提到 2 倍以上,滚一下缩放更跟手。
    wheelSensitivity: 1.8,
    elements: [
      ...nodes.map((n) => {
        const size = nodeSize(n.kind, deg[n.id] || 0);
        const data: any = {
          id: n.id,
          label: n.title,
          kind: n.kind,
          pal: palKey(n),
          size,
          upad: glowPad(size),
          uopa: glowOpacity(n.kind, size),
          deg: deg[n.id] || 0,
          path: n.kind === "doc" || n.kind === "feedback" ? n.id : "",
          summary: n.summary || "",
        };
        // files 源:category 携带所属语义簇的颜色(#hex)→ 按簇着色(root 仍用金核),
        // 让画面一眼分出几个语义聚类(同簇同色)。
        if (isFiles && n.kind !== "root" && n.category && n.category.startsWith("#")) {
          data.gcolor = n.category;
        }
        return { data };
      }),
      ...edges.map((e, i) => ({
        // rel 只在「簇间语义关系边」上有(AI 推断:同源/进阶/方法论…);层级/双链边不带 → 不匹配 edge[rel]。
        data: e.rel
          ? { id: `e${i}`, source: e.source, target: e.target, rel: e.rel }
          : { id: `e${i}`, source: e.source, target: e.target },
      })),
    ],
    style: [
      {
        selector: "node",
        style: {
          "background-fill": "radial-gradient",
          "background-gradient-stop-positions": "0% 100%" as any,
          width: "data(size)",
          height: "data(size)",
          "border-width": 0.6,
          "border-color": "rgba(255,255,255,0.55)",
          "underlay-shape": "ellipse",
          "underlay-padding": "data(upad)" as any,
          "underlay-opacity": "data(uopa)" as any,
          label: "data(label)",
          color: "rgba(220,232,255,0.9)",
          "font-family": "Source Han Serif SC, serif",
          "font-size": 10,
          "text-valign": "bottom",
          "text-margin-y": 4,
          "text-outline-color": "#070b16",
          "text-outline-width": 2,
          "text-outline-opacity": 0.75,
          "text-opacity": 0, // 文档标签默认隐藏, 缩放/悬停时浮现
          "min-zoomed-font-size": 7,
        },
      },
      ...palSelectors,
      // files 源:按语义簇色实底着色(覆盖 pal 渐变)+ 同色辉光 → 各簇颜色分明
      {
        selector: "node[gcolor]",
        style: {
          "background-fill": "solid",
          "background-color": "data(gcolor)",
          "underlay-color": "data(gcolor)",
        },
      },
      {
        selector: 'node[kind = "folder"]',
        style: { "text-opacity": 1, "font-size": 11, color: "#d8eaff" },
      },
      {
        selector: 'node[kind = "root"]',
        style: {
          "text-opacity": 1,
          "font-size": 15,
          color: "#ffe9bf",
          "font-weight": 700,
          "border-width": 1.2,
          "border-color": "rgba(255,240,200,0.85)",
        },
      },
      { selector: "node.show-label", style: { "text-opacity": 1 } },
      {
        selector: "node.hl",
        style: {
          "underlay-opacity": 0.5,
          "border-width": 1.4,
          "border-color": "#ffffff",
          "text-opacity": 1,
          "z-index": 99,
        },
      },
      { selector: "node.faded", style: { opacity: 0.07 } },
      {
        selector: "node:selected",
        style: {
          "border-width": 2,
          "border-color": "#ffffff",
          "underlay-opacity": 0.55,
        },
      },
      {
        selector: "edge",
        style: {
          width: 0.8,
          "line-color": "#7f9fd8",
          "curve-style": "straight",
          opacity: 0.18,
        },
      },
      { selector: "edge.faded", style: { opacity: 0.03 } },
      {
        selector: "edge.hl",
        style: { "line-color": "#cfe2ff", width: 1.4, opacity: 0.85 },
      },
      // 簇间语义关系边(AI 推断):鎏金虚线 + 箭头 + 关系标签(放大/悬停才显字),让星图成真·关系图谱
      {
        selector: "edge[rel]",
        style: {
          width: 1.3,
          "line-color": "#d4b06a",
          "line-style": "dashed",
          "curve-style": "bezier",
          opacity: 0.5,
          "target-arrow-shape": "triangle",
          "target-arrow-color": "#d4b06a",
          "arrow-scale": 0.7,
          label: "data(rel)",
          "font-family": "var(--sans)",
          "font-size": 8,
          color: "#f0dcae",
          "text-outline-color": "#070b16",
          "text-outline-width": 2,
          "text-outline-opacity": 0.85,
          "text-rotation": "autorotate" as any,
          "text-opacity": 0,
          "min-zoomed-font-size": 6,
        },
      },
      { selector: "edge[rel].show-label", style: { "text-opacity": 0.92 } },
      {
        selector: "edge[rel].hl",
        style: { opacity: 0.95, width: 2, "text-opacity": 1, "line-color": "#e8c878" },
      },
    ],
    layout: layoutFor(nodes.length),
  });

  wireInteractions(cy);
  // 等入场布局动画结束再起转, 避免与 fcose 动画打架；同时通知 App 收起加载条(此时图已稳定)
  cy.one("layoutstop", () => {
    // 拉远镜头:整张图缩在视野中央、四周大量留白,像从远处看一片星海 —— 更震撼、更有「我的
    // 数据宇宙」的体量感。files(用户数据库)拉得更远;远景下标签自动隐去(只在放大时浮现),
    // 正好只见星点不见字。随后建索引/归类越完善,簇结构越贴合真实数据,这片星海也越像你本人。
    try {
      const c = cy!;
      c.fit(undefined, isFiles ? 120 : 80);
      const center = { x: c.width() / 2, y: c.height() / 2 };
      c.zoom({ level: c.zoom() * (isFiles ? 0.55 : 0.78), renderedPosition: center });
    } catch {
      /* 容错:fit/zoom 失败不阻断 */
    }
    startSpinLoop();
    emit("ready");
  });
}

// ── 交互: 悬停高亮邻居 / 选中信息 / 缩放显隐标签 ─────────────
function wireInteractions(c: Core) {
  let labelsShown = false;
  const syncLabels = () => {
    const show = c.zoom() >= 0.9;
    if (show !== labelsShown) {
      labelsShown = show;
      c.batch(() => {
        c.nodes('[kind = "doc"], [kind = "feedback"]').toggleClass("show-label", show);
        c.edges("[rel]").toggleClass("show-label", show); // 关系标签同步显隐
      });
    }
  };
  c.on("zoom", syncLabels);
  c.ready(() => syncLabels());

  c.on("mouseover", "node", (evt) => {
    const hood = evt.target.closedNeighborhood();
    c.batch(() => {
      c.elements().addClass("faded");
      hood.removeClass("faded").addClass("hl");
    });
  });
  c.on("mouseout", "node", () => {
    c.batch(() => c.elements().removeClass("faded hl"));
  });

  c.on("tap", "node", (evt) => {
    const d = evt.target.data();
    selected.value = { title: d.label, kind: d.kind, path: d.path, deg: d.deg, summary: d.summary };
  });
  c.on("tap", (evt) => {
    if (evt.target === c) selected.value = null;
  });

  // 拖拽节点时暂停自转, 松手续转 (startSpinLoop 内部按 spinning 意图判断)
  c.on("grab", "node", () => cancelSpinLoop());
  c.on("free", "node", () => startSpinLoop());
}

// ── 工具栏动作 ──────────────────────────────────────────────
function relayout() {
  if (!cy) return;
  cy.layout(layoutFor(cy.nodes().length)).run();
}
function fit() {
  if (cy) cy.animate({ fit: { eles: cy.elements(), padding: 60 }, duration: 350 });
}

// 缓慢自转 (约 100s 一圈): 绕固定圆心刚体旋转节点位置, 标签保持正向。
// 仅在开启时跑 requestAnimationFrame, 默认关 → 不增加常驻开销。
function spinStep(t: number) {
  if (!cy || !spinning.value) return;
  if (!spinLastT) {
    spinLastT = t;
    rafId = requestAnimationFrame(spinStep);
    return;
  }
  const elapsed = t - spinLastT;
  // 节流到 ~30fps：慢速自转下与 60fps 视觉无异，但每帧重绘全图的开销减半，更顺
  if (elapsed < 30) {
    rafId = requestAnimationFrame(spinStep);
    return;
  }
  const dt = Math.min(0.05, elapsed / 1000); // 标签页切回时不跳变
  spinLastT = t;
  const d = 0.06 * dt;
  const cos = Math.cos(d);
  const sin = Math.sin(d);
  cy.batch(() => {
    cy!.nodes().forEach((n) => {
      const p = n.position();
      const dx = p.x - spinCx;
      const dy = p.y - spinCy;
      n.position({ x: spinCx + dx * cos - dy * sin, y: spinCy + dx * sin + dy * cos });
    });
  });
  rafId = requestAnimationFrame(spinStep);
}
// 启动自转循环 (幂等): 重算圆心后起 RAF; 仅在 spinning 意图为真时生效
function startSpinLoop() {
  if (!cy || !spinning.value) return;
  cancelAnimationFrame(rafId); // 先收掉旧循环, 防止叠加
  const bb = cy.nodes().boundingBox();
  spinCx = (bb.x1 + bb.x2) / 2;
  spinCy = (bb.y1 + bb.y2) / 2;
  spinLastT = 0;
  rafId = requestAnimationFrame(spinStep);
}
// 只停循环, 不改变用户意图 (用于重建/拖拽期间临时暂停)
function cancelSpinLoop() {
  cancelAnimationFrame(rafId);
  spinLastT = 0;
}
function toggleSpin() {
  spinning.value = !spinning.value;
  if (spinning.value) startSpinLoop();
  else cancelSpinLoop();
}
function toggleFolders() {
  showFolders.value = !showFolders.value;
  render();
}
function runSearch() {
  if (!cy) return;
  const q = query.value.trim().toLowerCase();
  cy.batch(() => {
    cy!.elements().removeClass("faded hl");
    if (!q) return;
    const hits = cy!
      .nodes()
      .filter((n) => String(n.data("label")).toLowerCase().includes(q));
    if (hits.length === 0) return;
    cy!.elements().addClass("faded");
    hits.removeClass("faded").addClass("hl");
    hits.connectedEdges().removeClass("faded");
    hits.neighborhood("node").removeClass("faded");
  });
}

onMounted(async () => {
  graphData = isFiles ? await filesApi.graph() : await kb.graph();
  empty.value = graphData.nodes.length === 0;
  stats.value = {
    docs: graphData.nodes.filter((n) => n.kind === "doc").length,
    folders: graphData.nodes.filter((n) => n.kind === "folder").length,
    edges: graphData.edges.length,
    memories: graphData.nodes.filter((n) => n.kind === "feedback").length,
  };
  if (empty.value) {
    emit("ready");
    return;
  }
  await nextTick();
  render(); // render 内 layoutstop 时 emit('ready')，App 据此收起加载条
  // 兜底：极端情况下 layoutstop 未触发，也不让加载条一直卡住
  setTimeout(() => emit("ready"), 3500);
});

// KeepAlive：切回本视图时恢复自转；切走时暂停自转 raf + 星场 CSS 动画
// (animation-play-state 显式暂停,确保缓存期间合成器零开销)
const bgPaused = ref(false);
onActivated(() => {
  bgPaused.value = false;
  if (!cy) return;
  cy.resize(); // KeepAlive 重新挂载后画布尺寸可能需校正
  if (spinning.value) startSpinLoop();
});
onDeactivated(() => {
  cancelSpinLoop();
  bgPaused.value = true;
});

onUnmounted(() => {
  cancelAnimationFrame(rafId);
  if (cy) {
    cy.destroy();
    cy = null;
  }
});
</script>

<template>
  <div class="graph" :class="{ 'bg-paused': bgPaused }">
    <div v-if="!embedded" class="head">
      <div class="title">知识图谱</div>
      <div class="tools" v-if="!empty">
        <input
          class="search"
          v-model="query"
          @input="runSearch"
          placeholder="搜索节点…"
          spellcheck="false"
        />
        <button class="btn" @click="relayout" title="重新布局">重新布局</button>
        <button class="btn" @click="fit" title="适应窗口">适应</button>
        <button
          class="btn"
          :class="{ on: spinning }"
          @click="toggleSpin"
          title="缓慢自转"
        >
          {{ spinning ? "停止" : "旋转" }}
        </button>
        <button
          class="btn"
          :class="{ on: showFolders }"
          @click="toggleFolders"
          title="按文件夹层级显示中枢结构"
        >
          目录结构
        </button>
        <div class="stats">
          文档 <strong>{{ stats.docs }}</strong>
          <template v-if="stats.memories"
            >· <span class="echo-stat">记忆 {{ stats.memories }}</span></template
          >
          <template v-if="showFolders"
            >· 目录 <strong>{{ stats.folders }}</strong></template
          >
          · 关系 <strong>{{ stats.edges }}</strong>
        </div>
      </div>
    </div>

    <div v-if="empty" class="empty">
      <div class="empty-glyph">◈</div>
      <div>当前 KB 没有可视化节点</div>
      <div class="empty-hint">
        把 Markdown 文件放进知识库的 <code>raw/</code> 任意子目录,刷新本页即可。
        图谱会按文件夹层级自动连成结构;若想加横向关联,可在正文写
        <code>[[wiki-link]]</code> 双链或 <code>[文字](相对路径.md)</code> 链接。
      </div>
    </div>

    <div v-else class="stage">
      <!-- 深空背景层 (全部 pointer-events:none, 不挡交互) -->
      <div class="galaxy-bg"></div>
      <div class="nebula"></div>
      <div class="stars s1"></div>
      <div class="stars s2"></div>
      <div class="stars s3"></div>

      <div ref="container" class="cy"></div>

      <div class="vignette"></div>

      <div class="legend">
        <span><i class="dot" style="--c: #f0b24a"></i>{{ isFiles ? "我的资料" : "知识库" }}</span>
        <span><i class="dot sq" style="--c: #5fa8e6"></i>{{ isFiles ? "主题" : "目录" }}</span>
        <span><i class="dot" style="--c: #4f7fd0"></i>{{ isFiles ? "文件" : "文档" }}</span>
        <span v-if="stats.memories"
          ><i class="dot" style="--c: #f0567f"></i>记忆</span
        >
      </div>

      <transition name="fade">
        <div v-if="selected" class="card">
          <div class="card-kind">
            {{
              selected.kind === "root"
                ? isFiles
                  ? "星系核心 · 我的资料"
                  : "星系核心 · 知识库"
                : selected.kind === "folder"
                ? isFiles
                  ? "主题 · 一类资料"
                  : "目录中枢"
                : selected.kind === "feedback"
                ? "回声 · 记忆"
                : isFiles
                ? "文件"
                : "文档"
            }}
          </div>
          <div class="card-title">{{ selected.title }}</div>
          <div v-if="selected.summary" class="card-summary">{{ selected.summary }}</div>
          <div v-if="selected.path" class="card-path">{{ selected.path }}</div>
          <div class="card-meta">连接数 {{ selected.deg }}</div>
        </div>
      </transition>
    </div>
  </div>
</template>

<style scoped>
.graph {
  position: relative;
  display: flex;
  flex-direction: column;
  height: 100%;
  background: var(--bg);
}
.head {
  padding: 14px 24px;
  border-bottom: 1px solid var(--hairline);
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
}
.title {
  font-family: var(--serif);
  font-size: 18px;
  letter-spacing: 2px;
  color: var(--ink);
  white-space: nowrap;
}
.tools {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  justify-content: flex-end;
}
.search {
  width: 150px;
  padding: 5px 10px;
  font-size: 12px;
  font-family: var(--sans);
  color: var(--ink);
  background: var(--bg-soft);
  border: 1px solid var(--hairline);
  border-radius: 4px;
  outline: none;
}
.search:focus {
  border-color: var(--border-strong);
}
.btn {
  padding: 5px 11px;
  font-size: 12px;
  font-family: var(--sans);
  color: var(--muted);
  background: var(--bg-soft);
  border: 1px solid var(--hairline);
  border-radius: 4px;
  cursor: pointer;
  transition: all 0.15s;
}
.btn:hover {
  color: var(--ink);
  border-color: var(--border-strong);
}
.btn.on {
  color: #2a4a6e;
  border-color: #9bbce0;
  background: #eef4fb;
}
.stats {
  font-size: 12px;
  color: var(--muted);
  margin-left: 4px;
  white-space: nowrap;
}
.stats strong {
  color: var(--ink);
  font-family: var(--mono);
}
.stats .echo-stat {
  color: #f0567f;
  font-weight: 700;
  font-family: var(--mono);
}

/* ── 星河舞台 ───────────────────────────────────────────── */
.stage {
  position: relative;
  flex: 1;
  min-height: 0;
  overflow: hidden;
  background: #04060e;
}
.galaxy-bg,
.nebula,
.stars,
.vignette {
  position: absolute;
  inset: 0;
  pointer-events: none;
}
.galaxy-bg {
  background: radial-gradient(
    ellipse at 50% 44%,
    #18233f 0%,
    #0b1124 48%,
    #05070f 100%
  );
}
.nebula {
  background:
    radial-gradient(42% 38% at 34% 40%, rgba(78, 104, 200, 0.2), transparent 70%),
    radial-gradient(46% 40% at 70% 66%, rgba(140, 92, 200, 0.15), transparent 70%),
    radial-gradient(34% 30% at 60% 24%, rgba(92, 122, 210, 0.12), transparent 70%);
  filter: blur(8px);
  animation: nebula 22s ease-in-out infinite alternate;
}
/* KeepAlive 切走时显式暂停星场/星云动画,合成器零开销 */
.graph.bg-paused .nebula,
.graph.bg-paused .stars {
  animation-play-state: paused;
}
/* 多层星场: 渐变平铺成星点, transform/opacity 走合成层 → 轻量 */
.stars {
  inset: -60px;
  background-repeat: repeat;
  will-change: transform, opacity;
}
.stars.s1 {
  background-image:
    radial-gradient(1.3px 1.3px at 18% 28%, rgba(255, 255, 255, 0.95), transparent),
    radial-gradient(1.5px 1.5px at 84% 18%, rgba(255, 246, 222, 0.95), transparent),
    radial-gradient(1px 1px at 42% 80%, rgba(220, 235, 255, 0.8), transparent);
  background-size: 300px 300px;
  animation: drift1 70s linear infinite alternate, tw 5.5s ease-in-out infinite alternate;
}
.stars.s2 {
  background-image:
    radial-gradient(1px 1px at 60% 50%, rgba(200, 220, 255, 0.85), transparent),
    radial-gradient(1.2px 1.2px at 12% 66%, rgba(255, 255, 255, 0.8), transparent),
    radial-gradient(1px 1px at 90% 78%, rgba(210, 230, 255, 0.7), transparent);
  background-size: 220px 220px;
  opacity: 0.85;
  animation: drift2 95s linear infinite alternate, tw 7s ease-in-out infinite alternate;
}
.stars.s3 {
  background-image:
    radial-gradient(0.8px 0.8px at 30% 40%, rgba(255, 255, 255, 0.6), transparent),
    radial-gradient(0.8px 0.8px at 75% 60%, rgba(200, 220, 255, 0.55), transparent);
  background-size: 160px 160px;
  opacity: 0.6;
  animation: drift1 130s linear infinite, tw 9s ease-in-out infinite alternate;
}
.cy {
  position: absolute;
  inset: 0;
  z-index: 2;
  background: transparent;
}
.vignette {
  z-index: 3;
  background: radial-gradient(
    ellipse at center,
    transparent 52%,
    rgba(2, 3, 9, 0.7) 100%
  );
}
@keyframes drift1 {
  from {
    transform: translate3d(0, 0, 0);
  }
  to {
    transform: translate3d(-26px, 18px, 0);
  }
}
@keyframes drift2 {
  from {
    transform: translate3d(0, 0, 0);
  }
  to {
    transform: translate3d(22px, -16px, 0);
  }
}
@keyframes tw {
  from {
    opacity: 0.45;
  }
  to {
    opacity: 1;
  }
}
@keyframes nebula {
  from {
    opacity: 0.7;
    transform: scale(1);
  }
  to {
    opacity: 1;
    transform: scale(1.06);
  }
}

/* ── 玻璃态浮层 ─────────────────────────────────────────── */
.legend {
  position: absolute;
  left: 16px;
  bottom: 14px;
  z-index: 4;
  display: flex;
  gap: 14px;
  font-size: 11px;
  font-family: var(--sans);
  color: rgba(214, 226, 255, 0.82);
  background: rgba(10, 14, 28, 0.55);
  border: 1px solid rgba(150, 180, 255, 0.18);
  border-radius: 8px;
  padding: 6px 12px;
  backdrop-filter: blur(8px);
}
.legend span {
  display: flex;
  align-items: center;
  gap: 5px;
}
.legend .dot {
  width: 9px;
  height: 9px;
  border-radius: 50%;
  display: inline-block;
  background: var(--c);
  box-shadow: 0 0 7px 1px var(--c);
}
.legend .dot.sq {
  border-radius: 2px;
}
.card {
  position: absolute;
  right: 16px;
  top: 16px;
  z-index: 4;
  width: 244px;
  background: rgba(10, 14, 28, 0.72);
  border: 1px solid rgba(150, 180, 255, 0.2);
  border-left: 3px solid #6fb3ff;
  border-radius: 8px;
  padding: 12px 14px;
  box-shadow: 0 10px 34px rgba(0, 0, 0, 0.5);
  backdrop-filter: blur(10px);
}
.card-kind {
  font-size: 10px;
  letter-spacing: 1px;
  color: rgba(160, 185, 235, 0.85);
  font-family: var(--sans);
}
.card-title {
  font-family: var(--serif);
  font-size: 16px;
  color: #f1f5ff;
  margin: 3px 0 6px;
  word-break: break-word;
}
.card-summary {
  font-size: 12.5px;
  color: #f0dcae;
  line-height: 1.6;
  margin: 0 0 7px;
  word-break: break-word;
}
.card-path {
  font-family: var(--mono);
  font-size: 11px;
  color: rgba(180, 200, 240, 0.7);
  word-break: break-all;
  line-height: 1.5;
}
.card-meta {
  margin-top: 7px;
  font-size: 11px;
  color: rgba(150, 175, 225, 0.75);
  font-family: var(--sans);
}
.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.18s;
}
.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}

.empty {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  color: var(--muted);
  font-family: var(--serif);
  letter-spacing: 1px;
  gap: 6px;
}
.empty-glyph {
  font-size: 56px;
  color: var(--border-strong);
  margin-bottom: 8px;
}
.empty-hint {
  font-family: var(--sans);
  font-size: 12px;
  color: var(--dim);
  max-width: 460px;
  text-align: center;
  letter-spacing: 0;
  margin-top: 8px;
  line-height: 1.7;
}
.empty-hint code {
  background: var(--code-bg);
  padding: 1px 6px;
  border-radius: 2px;
  font-family: var(--mono);
  font-size: 11px;
}
</style>
