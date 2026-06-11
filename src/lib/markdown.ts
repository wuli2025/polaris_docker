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
  /** false = 流式中的活跃消息:跳过异步增强排队(等定稿后再高亮),省 CPU */
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
  const html = sanitizeHtml(marked.parse(mathPlaceholders(key)) as string);
  if (cache.size >= CACHE_CAP) {
    cache.clear();
    enhanceQueued.clear();
  }
  cache.set(key, html);
  if (opts?.enhance !== false) scheduleEnhance(key, html);
  return html;
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
let shikiMod: Promise<typeof import("shiki")> | null = null;
function getShiki() {
  if (!shikiMod) shikiMod = import("shiki");
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
    const { codeToHtml } = await getShiki();
    const blocks = tpl.content.querySelectorAll(".code-block");
    for (const blk of Array.from(blocks)) {
      const codeEl = blk.querySelector("pre > code");
      const pre = blk.querySelector("pre");
      if (!codeEl || !pre) continue;
      const lang = (blk.getAttribute("data-lang") || "").toLowerCase();
      if (!lang || lang === "text" || lang === "plain") continue;
      try {
        const out = await codeToHtml(codeEl.textContent || "", {
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
