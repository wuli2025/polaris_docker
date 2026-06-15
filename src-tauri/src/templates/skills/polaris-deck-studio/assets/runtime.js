/* polaris-deck-studio :: runtime.js — minimal, dependency-free deck engine
 *
 * Clean-room Polaris implementation (not vendored). Wires up a `.deck > .slide` deck:
 *   ← / → / Space / PgUp / PgDn / Home / End  navigate
 *   T  cycle theme (data-theme on <html>)      F  fullscreen
 *   O  overview grid (click a thumb to jump)   P  print (→ PDF)
 *   #/N deep-link to slide N (1-based)         (used by export-pptx.mjs)
 *
 * Export hooks (for headless screenshotting):
 *   window.__deck = { total, current(), go(n), next(), prev() }
 *   add ?export=1 to the URL (or <html class="no-anim">) to disable entrance animations.
 */
(function () {
  "use strict";

  // ── polaris-fx：确定性逐帧时钟（per-frame fx 视频的 deck 侧基础，无需 chromiumoxide）。
  //    __fx.seek(tMs) 把所有 CSS 动画设到绝对时刻 t（WAAPI），让逐帧截图得到真动画。
  //    URL ?fx_t=N（毫秒）→ 加载后 seek 到该帧（将来由持久浏览器逐帧驱动 + 截图编码）。
  window.__fx = {
    seek: function (tMs) {
      try {
        var anims = document.getAnimations ? document.getAnimations() : [];
        for (var i = 0; i < anims.length; i++) {
          try { anims[i].pause(); anims[i].currentTime = tMs; } catch (e) {}
        }
      } catch (e) {}
    },
    // spring 物理弹簧纯函数（Remotion 参数系）：给定 t(秒) 返回 0→1 的位移，可驱动 transform。
    spring: function (cfg) {
      cfg = cfg || {};
      var mass = cfg.mass || 1, stiffness = cfg.stiffness || 100, damping = cfg.damping || 10;
      var w0 = Math.sqrt(stiffness / mass);
      var zeta = damping / (2 * Math.sqrt(stiffness * mass));
      return function (tSec) {
        if (zeta < 1) {
          var wd = w0 * Math.sqrt(1 - zeta * zeta);
          return 1 - Math.exp(-zeta * w0 * tSec) * (Math.cos(wd * tSec) + (zeta * w0 / wd) * Math.sin(wd * tSec));
        }
        return 1 - Math.exp(-w0 * tSec) * (1 + w0 * tSec);
      };
    }
  };
  (function () {
    var m = /[?&]fx_t=(\d+(?:\.\d+)?)/.exec(location.search);
    if (m) {
      var seekNow = function () { window.__fx.seek(parseFloat(m[1])); };
      if (document.readyState === "complete") setTimeout(seekNow, 20);
      else window.addEventListener("load", function () { setTimeout(seekNow, 20); });
    }
  })();

  var THEMES = [
    "minimal-white", "editorial-serif", "swiss-grid", "magazine-bold",
    "japanese-minimal", "xiaohongshu-white", "academic-paper", "corporate-clean",
    "soft-pastel", "tokyo-night", "dracula", "nord", "cyberpunk-neon",
    "terminal-green", "blueprint", "glassmorphism", "neo-brutalism"
  ];

  function ready(fn) {
    if (document.readyState !== "loading") fn();
    else document.addEventListener("DOMContentLoaded", fn);
  }

  ready(function () {
    var deck = document.querySelector(".deck");
    var slides = Array.prototype.slice.call(document.querySelectorAll(".slide"));
    if (!slides.length) return;
    var total = slides.length;
    var idx = 0;

    // Export mode: kill entrance animations for clean, deterministic stills.
    if (/[?&]export=1/.test(location.search)) {
      document.documentElement.classList.add("no-anim");
    }

    // progress bar
    var prog = document.querySelector(".progress-bar > span");
    // slide-number chrome (any element with .slide-number gets data-current/total)
    var counters = Array.prototype.slice.call(document.querySelectorAll(".slide-number"));

    function clamp(n) { return Math.max(0, Math.min(total - 1, n)); }

    function render() {
      for (var i = 0; i < total; i++) {
        var s = slides[i];
        s.classList.toggle("is-active", i === idx);
        s.classList.toggle("is-prev", i < idx);
      }
      if (prog) prog.style.width = ((idx + 1) / total * 100) + "%";
      for (var c = 0; c < counters.length; c++) {
        counters[c].setAttribute("data-current", String(idx + 1));
        counters[c].setAttribute("data-total", String(total));
      }
      var hash = "#/" + (idx + 1);
      if (location.hash !== hash) {
        try { history.replaceState(null, "", hash); } catch (e) { location.hash = hash; }
      }
      document.title = document.title.replace(/\s+·\s+\d+\/\d+$/, "");
    }

    function go(n) { idx = clamp(n); render(); }
    function next() { if (idx < total - 1) go(idx + 1); }
    function prev() { if (idx > 0) go(idx - 1); }

    // ---- deep link from hash (#/3) ----
    function fromHash() {
      var m = /^#\/(\d+)/.exec(location.hash || "");
      if (m) { var n = parseInt(m[1], 10) - 1; if (!isNaN(n)) idx = clamp(n); }
    }
    fromHash();
    window.addEventListener("hashchange", function () {
      var m = /^#\/(\d+)/.exec(location.hash || "");
      if (m) { var n = parseInt(m[1], 10) - 1; if (!isNaN(n) && n !== idx) go(n); }
    });

    // ---- theme cycling ----
    function cycleTheme(dir) {
      var cur = document.documentElement.getAttribute("data-theme") || THEMES[0];
      var i = THEMES.indexOf(cur);
      i = (i + (dir || 1) + THEMES.length) % THEMES.length;
      document.documentElement.setAttribute("data-theme", THEMES[i]);
    }

    // ---- overview grid ----
    var overview = document.querySelector(".overview");
    function buildOverview() {
      if (!overview || overview.dataset.built) return;
      for (var i = 0; i < total; i++) {
        var t = document.createElement("div");
        t.className = "thumb";
        var title = slides[i].getAttribute("data-title") ||
          (slides[i].querySelector("h1,h2,.h1,.h2,h3") || {}).textContent || ("Slide " + (i + 1));
        t.innerHTML = '<span class="n">' + (i + 1) + '</span><span class="t"></span>';
        t.querySelector(".t").textContent = String(title).trim().slice(0, 60);
        (function (n) { t.addEventListener("click", function () { go(n); toggleOverview(false); }); })(i);
        overview.appendChild(t);
      }
      overview.dataset.built = "1";
    }
    function toggleOverview(force) {
      if (!overview) return;
      buildOverview();
      var open = force === undefined ? !overview.classList.contains("open") : force;
      overview.classList.toggle("open", open);
    }

    function toggleFullscreen() {
      if (!document.fullscreenElement) (document.documentElement.requestFullscreen || function () {}).call(document.documentElement);
      else (document.exitFullscreen || function () {}).call(document);
    }

    // ---- keyboard ----
    document.addEventListener("keydown", function (e) {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      switch (e.key) {
        case "ArrowRight": case "PageDown": case " ": case "Spacebar":
          e.preventDefault(); next(); break;
        case "ArrowLeft": case "PageUp":
          e.preventDefault(); prev(); break;
        case "Home": e.preventDefault(); go(0); break;
        case "End": e.preventDefault(); go(total - 1); break;
        case "t": case "T": cycleTheme(e.shiftKey ? -1 : 1); break;
        case "f": case "F": toggleFullscreen(); break;
        case "o": case "O": case "Escape": toggleOverview(e.key === "Escape" ? false : undefined); break;
        case "p": case "P": window.print(); break;
        default: break;
      }
    });

    // click navigation (right half = next, left quarter = prev)
    if (deck) {
      deck.addEventListener("click", function (e) {
        if (e.target.closest("a,button,input,textarea,.no-nav,.overview")) return;
        var x = e.clientX / window.innerWidth;
        if (x > 0.6) next(); else if (x < 0.25) prev();
      });
    }

    render();
    window.__deck = { total: total, current: function () { return idx; }, go: go, next: next, prev: prev };

    // ════ 可编辑 PPT 分层导出(路线 A)═══════════════════════════════
    // ?extract=1 → 抽活动页文本块包围盒+样式,写进 <script id=polaris-text-rects>(dump-dom 取走)。
    // ?notext=1  → 把同一批文本块 visibility:hidden,截出「无字背景图」。
    // 两个模式必须共用同一套选择逻辑:背景里隐藏的 = 文本框里重建的,一一对应才不重影/不丢字。
    var TEXT_SEL = "h1,h2,h3,h4,h5,h6,p,li,blockquote,dt,dd,td,th,figcaption,.kicker,.eyebrow,.pill,.label,.title,.subtitle,.lede,.caption";
    function isArtText(cs) {
      // 渐变字/镂空字/阴影字:PPT 文本框只能近似 → 保真优先,留在背景图里(不抽取也不隐藏)。
      if (cs.webkitBackgroundClip === "text" || cs.backgroundClip === "text") return true;
      if (cs.webkitTextFillColor === "transparent" || cs.webkitTextFillColor === "rgba(0, 0, 0, 0)") return true;
      if (cs.textShadow && cs.textShadow !== "none") return true;
      return false;
    }
    function collectTextNodes(root) {
      var nodes = Array.prototype.slice.call(root.querySelectorAll(TEXT_SEL));
      var out = [];
      nodes.forEach(function (el) {
        var t = (el.textContent || "").trim();
        if (!t) return;
        if (el.querySelector(TEXT_SEL)) return; // 取叶子文本块,避免嵌套重复
        var cs = window.getComputedStyle(el);
        if (isArtText(cs)) return;
        out.push({ el: el, cs: cs, text: t });
      });
      return out;
    }
    function cssColorToHex(c) {
      var m = /rgba?\((\d+)[,\s]+(\d+)[,\s]+(\d+)(?:[,\s/]+([\d.]+))?\)/.exec(c || "");
      if (!m) return null;
      if (m[4] !== undefined && parseFloat(m[4]) === 0) return null; // 全透明字不值得建框
      function hx(n) { return ("0" + (+n).toString(16)).slice(-2); }
      return (hx(m[1]) + hx(m[2]) + hx(m[3])).toUpperCase();
    }

    // ---- ?notext=1:同步隐藏(DOMContentLoaded 即生效,先于首帧渲染,screenshot 无竞态)----
    if (/[?&]notext=1/.test(location.search)) {
      slides.forEach(function (s) {
        collectTextNodes(s).forEach(function (n) { n.el.style.visibility = "hidden"; });
      });
    }

    // ---- ?extract=1:文本块包围盒+样式 → JSON(等字体/布局稳定后抽取)----
    if (/[?&]extract=1/.test(location.search)) {
      var extractRects = function () {
        var active = slides[idx];
        if (!active) return;
        var out = [];
        collectTextNodes(active).forEach(function (n) {
          var r = n.el.getBoundingClientRect();
          if (r.width <= 0 || r.height <= 0) return;
          var cs = n.cs;
          var text = n.text;
          // li 的项目符号是 ::marker 伪元素,textContent 不含 → 手动补回,否则可编辑版丢符号。
          if (n.el.tagName === "LI" && cs.listStyleType !== "none") text = "• " + text;
          var align = cs.textAlign === "center" ? "ctr"
            : cs.textAlign === "right" || cs.textAlign === "end" ? "r"
            : cs.textAlign === "justify" ? "just" : "l";
          out.push({
            text: text.slice(0, 2000),
            x: Math.round(r.left), y: Math.round(r.top),
            w: Math.round(r.width), h: Math.round(r.height),
            size: Math.round(parseFloat(cs.fontSize) || 16),
            bold: (parseInt(cs.fontWeight, 10) || 400) >= 600,
            italic: cs.fontStyle === "italic",
            color: cssColorToHex(cs.color) || "000000",
            align: align,
            serif: /serif/i.test(cs.fontFamily) && !/sans-serif/i.test(cs.fontFamily),
            lh: Math.round(parseFloat(cs.lineHeight) || 0)
          });
        });
        var s = document.getElementById("polaris-text-rects");
        if (!s) { s = document.createElement("script"); s.type = "application/json"; s.id = "polaris-text-rects"; document.body.appendChild(s); }
        // 能力标记:Rust 看到它才敢用「无字背景+可见文本框」;旧 deck 没有 → 自动降级隐形层。
        s.setAttribute("data-notext-capable", "1");
        s.textContent = JSON.stringify(out);
      };
      // 等字体/图片布局稳定后抽取。
      if (document.readyState === "complete") setTimeout(extractRects, 60);
      else window.addEventListener("load", function () { setTimeout(extractRects, 60); });
    }
  });
})();
