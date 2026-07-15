// 统一 markdown 渲染管线(聊天回复等所有 v-html 的来源):
// 1) 同步:marked(自定义 code/link 渲染) + DOMPurify → 立即可显示的 HTML,按原文缓存
//    —— 流式期间每 token 只为「活跃那条」做一次解析,历史回合全部命中缓存,不再全量重算。
// 2) 异步增强:shiki 代码高亮 + KaTeX 数学公式(都懒加载,首次用到才拉 chunk),
//    完成后更新缓存并 bump mdVersion,组件读它实现响应式刷新。
import { marked } from "marked";
import { ref } from "vue";
import { sanitizeHtml } from "./sanitize";

export const mdVersion = ref(0);

const cache = new Map<string, string>();
const enhanceQueued = new Set<string>();
const CACHE_CAP = 500;

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

// ── marked 全局配置:代码块包壳(语言标签 + 复制钮 + 超长折叠) ──
const COLLAPSE_LINES = 28;
marked.use({
  gfm: true,
  breaks: true,
  renderer: {
    code({ text, lang }: { text: string; lang?: string }) {
      const language = (lang || "").trim().split(/\s+/)[0];
      const lines = text.split("\n").length;
      const collapsed = lines > COLLAPSE_LINES ? " collapsed" : "";
      const langLabel = language || "text";
      return (
        `<div class="code-block${collapsed}" data-lang="${escapeHtml(language)}">` +
        `<div class="code-head"><span class="code-lang">${escapeHtml(langLabel)}</span>` +
        `<span class="code-actions">` +
        (collapsed
          ? `<button type="button" class="code-expand">展开 ${lines} 行</button>`
          : "") +
        `<button type="button" class="code-copy">复制</button></span></div>` +
        `<pre><code class="language-${escapeHtml(language)}">${escapeHtml(text)}</code></pre>` +
        `</div>`
      );
    },
  },
});

// ── 数学公式:fence 外把 $$…$$ / \[…\] / \(…\) 换成占位节点,异步 KaTeX 渲染 ──
const MATH_HINT = /\$\$|\\\[|\\\(/;

function mathPlaceholders(src: string): string {
  if (!MATH_HINT.test(src)) return src;
  // 按代码 fence 切段,只在 fence 外替换(行内 `code` 里出现 $$ 的概率低,接受)
  const parts = src.split(/(```[\s\S]*?(?:```|$))/);
  return parts
    .map((seg, i) => {
      if (i % 2 === 1) return seg; // fence 内原样
      return seg
        .replace(
          /\$\$([\s\S]+?)\$\$/g,
          (_m, tex) =>
            `<div class="math-block" data-tex="${escapeHtml(tex.trim())}"></div>`
        )
        .replace(
          /\\\[([\s\S]+?)\\\]/g,
          (_m, tex) =>
            `<div class="math-block" data-tex="${escapeHtml(tex.trim())}"></div>`
        )
        .replace(
          /\\\((.+?)\\\)/g,
          (_m, tex) =>
            `<span class="math-inline" data-tex="${escapeHtml(tex.trim())}"></span>`
        );
    })
    .join("");
}

export interface RenderOpts {
  /** false = 流式中的活跃消息:跳过异步增强排队(等定稿后再高亮),省 CPU,
   *  且走「稳定前缀 + 活跃尾巴」增量解析路径(见 renderMarkdownStreaming) */
  enhance?: boolean;
}

export function renderMarkdown(text: string, opts?: RenderOpts): string {
  const key = text || "";
  const hit = cache.get(key);
  if (hit !== undefined) {
    // 已有基础版但还没排过增强(此前是流式中渲染的) → 这次定稿了就补排
    if (opts?.enhance !== false) scheduleEnhance(key, hit);
    return hit;
  }
  // 流式中的活跃消息:增量路径 —— 只影响流式中间帧;定稿后(enhance!==false)
  // 走下面的完整 parse + 缓存 + 增强,最终渲染与旧行为完全一致。
  if (opts?.enhance === false) return renderMarkdownStreaming(key);
  const html = sanitizeHtml(marked.parse(mathPlaceholders(key)) as string);
  if (cache.size >= CACHE_CAP) {
    cache.clear();
    enhanceQueued.clear();
  }
  cache.set(key, html);
  scheduleEnhance(key, html); // 走到这里必是定稿路径(enhance=false 已在上面分流)
  return html;
}

// ── 流式增量渲染:稳定前缀 + 活跃尾巴,消 O(n²) ──
// 流式中活跃气泡文本每 ~40ms 增长一截,若整段作 cache key 则每帧 cache miss →
// marked.parse(全文) 重跑,回答越长每帧越贵(累计 O(n²));且增量 key 会把全局 LRU
// 撑满触发整体 clear() 抖动。这里把文本在最后一个段落边界(\n\n)切成两段:
// 稳定前缀(多数帧不变,按前缀字符串独立小缓存命中)+ 活跃尾巴(永远很短,每帧 parse)。
// 中间产物一律不写全局 LRU。定稿后走完整路径,最终 HTML 与旧路径逐字一致。
const streamPrefixCache = new Map<string, string>();
const STREAM_CACHE_CAP = 32; // 多个对话并发流式也够用;满了整清,代价只是重 parse 一次前缀

function countOccurrences(s: string, needle: string): number {
  let n = 0;
  let i = 0;
  while ((i = s.indexOf(needle, i)) !== -1) {
    n++;
    i += needle.length;
  }
  return n;
}

/** 把流式全文切成「稳定前缀 + 活跃尾巴」。切点选最后一个 \n\n(段落边界),
 *  且保证不落在代码栅栏内部 / 跨段落的 $$ 公式块内部 —— 否则两段各自 parse 会把
 *  同一个块拆碎。检测到切点在块内就退到该块开始之前的上一个段落边界重试。 */
function splitStreamText(text: string): [string, string] {
  let idx = text.lastIndexOf("\n\n");
  while (idx > 0) {
    const prefix = text.slice(0, idx + 2);
    // 栅栏计数为奇数 = 切点在代码块内 → 退到栅栏开始之前
    if (countOccurrences(prefix, "```") % 2 === 1) {
      idx = text.lastIndexOf("\n\n", prefix.lastIndexOf("```") - 1);
      continue;
    }
    // $$ 计数(只数栅栏外,与 mathPlaceholders 同一套 fence 切分)为奇数 =
    // 切点可能在跨段落的 $$…$$ 块内 → 同样后退,保住数学占位语义
    if (MATH_HINT.test(prefix)) {
      const parts = prefix.split(/(```[\s\S]*?(?:```|$))/);
      let dollars = 0;
      for (let i = 0; i < parts.length; i += 2)
        dollars += countOccurrences(parts[i], "$$");
      if (dollars % 2 === 1) {
        idx = text.lastIndexOf("\n\n", prefix.lastIndexOf("$$") - 1);
        continue;
      }
    }
    return [prefix, text.slice(idx + 2)];
  }
  // 没有安全切点(单段长文/整段都在栅栏里) → 整段当尾巴,行为同旧路径单帧
  return ["", text];
}

function parseChunk(src: string): string {
  return sanitizeHtml(marked.parse(mathPlaceholders(src)) as string);
}

function renderMarkdownStreaming(text: string): string {
  const [prefix, tail] = splitStreamText(text);
  let head = "";
  if (prefix) {
    const hit = streamPrefixCache.get(prefix);
    if (hit !== undefined) {
      head = hit;
    } else {
      head = parseChunk(prefix);
      if (streamPrefixCache.size >= STREAM_CACHE_CAP) streamPrefixCache.clear();
      streamPrefixCache.set(prefix, head);
    }
  }
  return tail ? head + parseChunk(tail) : head;
}

function scheduleEnhance(key: string, html: string) {
  if (enhanceQueued.has(key)) return;
  const needCode = html.includes('class="code-block');
  const needMath = html.includes('data-tex="');
  if (!needCode && !needMath) {
    enhanceQueued.add(key); // 标记免重复检查
    return;
  }
  enhanceQueued.add(key);
  // 空闲时再做,别跟流式渲染抢主线程
  const run = () => {
    enhanceHtml(html, needCode, needMath)
      .then((out) => {
        if (out && cache.get(key) === html) {
          cache.set(key, out);
          mdVersion.value++;
        }
      })
      .catch(() => {});
  };
  if ("requestIdleCallback" in window) {
    (window as any).requestIdleCallback(run, { timeout: 800 });
  } else {
    setTimeout(run, 60);
  }
}

// ── 懒加载 shiki / katex ──
// shiki 走细粒度 core:只注册常用语言 + 单主题(one-dark-pro),用 JS 正则引擎免 wasm。
// 全量 bundle(import("shiki"))会把 200+ 语言 + 全部主题打进 dist,体积不可接受。
let shikiMod: Promise<import("shiki/core").HighlighterCore> | null = null;
function getShiki() {
  if (!shikiMod) {
    shikiMod = Promise.all([
      import("shiki/core"),
      import("shiki/engine/javascript"),
    ]).then(([core, js]) =>
      core.createHighlighterCore({
        // forgiving:个别语法含 JS 正则不支持的 pattern 时跳过该规则,不整体报错
        engine: js.createJavaScriptRegexEngine({ forgiving: true }),
        themes: [import("shiki/themes/one-dark-pro.mjs")],
        langs: [
          import("shiki/langs/typescript.mjs"), // 含 ts 别名
          import("shiki/langs/tsx.mjs"),
          import("shiki/langs/javascript.mjs"), // 含 js 别名
          import("shiki/langs/jsx.mjs"),
          import("shiki/langs/json.mjs"),
          import("shiki/langs/python.mjs"), // 含 py 别名
          import("shiki/langs/rust.mjs"), // 含 rs 别名
          import("shiki/langs/go.mjs"),
          import("shiki/langs/shellscript.mjs"), // 含 bash/sh/shell/zsh 别名
          import("shiki/langs/html.mjs"),
          import("shiki/langs/css.mjs"),
          import("shiki/langs/vue.mjs"),
          import("shiki/langs/markdown.mjs"), // 含 md 别名
          import("shiki/langs/sql.mjs"),
          import("shiki/langs/yaml.mjs"), // 含 yml 别名
          import("shiki/langs/toml.mjs"),
          import("shiki/langs/diff.mjs"),
          import("shiki/langs/dockerfile.mjs"), // 含 docker 别名
        ],
      })
    );
  }
  return shikiMod;
}
let katexMod: Promise<any> | null = null;
function getKatex() {
  if (!katexMod) {
    katexMod = Promise.all([
      import("katex"),
      // CSS 随首次使用注入
      import("katex/dist/katex.min.css" as any),
    ]).then(([m]) => (m as any).default ?? m);
  }
  return katexMod;
}

async function enhanceHtml(
  html: string,
  needCode: boolean,
  needMath: boolean
): Promise<string | null> {
  const tpl = document.createElement("template");
  tpl.innerHTML = html;
  let changed = false;

  if (needCode) {
    const highlighter = await getShiki();
    const loadedLangs = new Set(highlighter.getLoadedLanguages());
    const blocks = tpl.content.querySelectorAll(".code-block");
    for (const blk of Array.from(blocks)) {
      const codeEl = blk.querySelector("pre > code");
      const pre = blk.querySelector("pre");
      if (!codeEl || !pre) continue;
      const lang = (blk.getAttribute("data-lang") || "").toLowerCase();
      if (!lang || lang === "text" || lang === "plain") continue;
      // 未注册语言:core 版 codeToHtml 会直接抛错,预判后保留无高亮原样
      if (!loadedLangs.has(lang)) continue;
      try {
        const out = highlighter.codeToHtml(codeEl.textContent || "", {
          lang,
          theme: "one-dark-pro",
        });
        const t2 = document.createElement("template");
        t2.innerHTML = out;
        const shikiPre = t2.content.querySelector("pre");
        if (shikiPre) {
          pre.replaceWith(shikiPre);
          changed = true;
        }
      } catch {
        /* 未知语言:保留无高亮原样 */
      }
    }
  }

  if (needMath) {
    const katex = await getKatex();
    const nodes = tpl.content.querySelectorAll(".math-block[data-tex], .math-inline[data-tex]");
    for (const n of Array.from(nodes)) {
      const tex = n.getAttribute("data-tex") || "";
      if (!tex) continue;
      try {
        n.innerHTML = katex.renderToString(tex, {
          throwOnError: false,
          displayMode: n.classList.contains("math-block"),
          output: "html",
        });
        n.removeAttribute("data-tex");
        changed = true;
      } catch {
        n.textContent = tex;
      }
    }
  }

  return changed ? tpl.innerHTML : null;
}

/**
 * 给渲染 markdown 的容器装事件委托(复制代码/展开折叠/外链系统浏览器打开)。
 * 挂在 App 根上一次即可,所有 v-html 区域全覆盖。返回卸载函数。
 */
export function installMarkdownDelegation(
  root: HTMLElement | Document,
  openExternal: (url: string) => void
): () => void {
  const handler = (e: Event) => {
    const target = e.target as HTMLElement | null;
    if (!target) return;
    const copyBtn = target.closest(".code-copy");
    if (copyBtn) {
      const blk = copyBtn.closest(".code-block");
      const code = blk?.querySelector("pre code, pre")?.textContent ?? "";
      navigator.clipboard
        .writeText(code)
        .then(() => {
          copyBtn.textContent = "已复制 ✓";
          setTimeout(() => (copyBtn.textContent = "复制"), 1400);
        })
        .catch(() => {});
      return;
    }
    const expandBtn = target.closest(".code-expand");
    if (expandBtn) {
      const blk = expandBtn.closest(".code-block");
      if (blk) {
        blk.classList.remove("collapsed");
        expandBtn.remove();
      }
      return;
    }
    const a = target.closest("a[href]") as HTMLAnchorElement | null;
    if (a && /^https?:\/\//i.test(a.getAttribute("href") || "")) {
      // 外链一律交给系统浏览器,别在 webview 里导航走丢
      e.preventDefault();
      openExternal(a.getAttribute("href")!);
    }
  };
  root.addEventListener("click", handler);
  return () => root.removeEventListener("click", handler);
}
