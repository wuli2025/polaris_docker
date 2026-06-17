/* polaris-web-studio :: motion.js — 高级动效运行时(可选,零依赖)
 *
 * 蒸馏自 Kimi 落地页 design.md 的动效系统,与 motion.css 配套。
 *   - <html data-motion> → 全局神经网络 Canvas 背景 + 鼠标跟随光晕 + 顶部滚动进度条
 *   - 任意元素 data-kinetic → 逐字标题(进入即错峰滑入)
 *   - 任意元素 data-count="5000000" [data-suffix="%"] → 进视口时从 0 滚到目标值
 * 主色由 CSS 变量 --motion-accent / --motion-glow 决定(主题可覆盖)。
 * 自动降级:prefers-reduced-motion 时停 Canvas、动画直接落终值。
 * 与 runtime.js 的 .reveal→.in 滚动揭示互补(本文件不重复实现揭示)。
 */
(function () {
  "use strict";
  var root = document.documentElement;
  var reduce = window.matchMedia && matchMedia('(prefers-reduced-motion: reduce)').matches;
  function accent() {
    return (getComputedStyle(root).getPropertyValue('--motion-accent') || '#00ff41').trim() || '#00ff41';
  }
  function rgbaAccent(a) {
    // 解析 --motion-accent(#rrggbb)→rgba;失败回退矩阵绿
    var c = accent(), r = 0, g = 255, b = 65;
    var m = /^#?([0-9a-f]{6})$/i.exec(c);
    if (m) { var n = parseInt(m[1], 16); r = (n >> 16) & 255; g = (n >> 8) & 255; b = n & 255; }
    return 'rgba(' + r + ',' + g + ',' + b + ',' + a + ')';
  }
  function ready(fn) {
    if (document.readyState !== 'loading') fn();
    else document.addEventListener('DOMContentLoaded', fn);
  }

  ready(function () {
    var motionOn = root.hasAttribute('data-motion');

    /* ── 全局层:进度条 + 鼠标光晕 + 神经网络背景 ── */
    if (motionOn) {
      var bar = document.createElement('div'); bar.id = 'px-progress'; document.body.appendChild(bar);
      var glow = document.createElement('div'); glow.id = 'px-glow'; document.body.appendChild(glow);
      window.addEventListener('scroll', function () {
        var st = root.scrollTop || document.body.scrollTop;
        var h = root.scrollHeight - innerHeight;
        bar.style.width = (h > 0 ? st / h * 100 : 0) + '%';
      }, { passive: true });
      window.addEventListener('mousemove', function (e) {
        glow.style.setProperty('--mx', e.clientX + 'px');
        glow.style.setProperty('--my', e.clientY + 'px');
      }, { passive: true });

      if (!reduce) neuralBackground();
    }

    /* ── 逐字标题 ── */
    var kinetics = document.querySelectorAll('[data-kinetic]');
    kinetics.forEach(function (n) {
      if (n.dataset.pxDone) return; n.dataset.pxDone = '1';
      var txt = n.textContent, frag = '';
      for (var i = 0; i < txt.length; i++) {
        var ch = txt[i] === ' ' ? '&nbsp;' : txt[i];
        frag += '<span class="px-char">' + ch + '</span>';
      }
      n.innerHTML = frag;
      var chars = n.querySelectorAll('.px-char');
      if (reduce) { chars.forEach(function (c) { c.classList.add('in'); }); return; }
      chars.forEach(function (c, i) {
        c.style.transitionDelay = (0.05 + i * 0.03) + 's';
        requestAnimationFrame(function () { requestAnimationFrame(function () { c.classList.add('in'); }); });
      });
    });

    /* ── 数字滚动 count-up(进视口触发一次) ── */
    var counters = document.querySelectorAll('[data-count]');
    function runCount(el) {
      if (el.dataset.pxc) return; el.dataset.pxc = '1';
      var target = parseFloat(el.getAttribute('data-count')) || 0;
      var suffix = el.getAttribute('data-suffix') || '';
      var dur = 2000, start = null;
      if (reduce) { el.textContent = format(target) + suffix; return; }
      function format(v) { return v >= 10000 ? Math.round(v).toLocaleString() : Math.round(v); }
      function tick(ts) {
        if (!start) start = ts;
        var p = Math.min((ts - start) / dur, 1), e = 1 - Math.pow(2, -10 * p); // expo.out
        el.textContent = format(target * e) + suffix;
        if (p < 1) requestAnimationFrame(tick); else el.textContent = format(target) + suffix;
      }
      requestAnimationFrame(tick);
    }
    if ('IntersectionObserver' in window) {
      var io = new IntersectionObserver(function (es) {
        es.forEach(function (e) { if (e.isIntersecting) { runCount(e.target); io.unobserve(e.target); } });
      }, { threshold: 0.6 });
      counters.forEach(function (c) { io.observe(c); });
    } else counters.forEach(runCount);
  });

  /* ── 神经网络粒子背景(按宽度分档,鼠标吸引,随机脉冲) ── */
  function neuralBackground() {
    var c = document.createElement('canvas'); c.id = 'px-bg'; document.body.appendChild(c);
    var ctx = c.getContext('2d'); if (!ctx) return;
    var dpr = Math.min(window.devicePixelRatio || 1, 2);
    var w, h, parts = [], mouse = { x: -1e4, y: -1e4 }, pulses = [], frame = 0;
    function resize() {
      w = innerWidth; h = innerHeight; c.width = w * dpr; c.height = h * dpr;
      c.style.width = w + 'px'; c.style.height = h + 'px'; ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    }
    resize();
    var N = w < 768 ? 80 : (w < 1280 ? 120 : 180), CD = 150, MR = 250;
    for (var i = 0; i < N; i++) {
      var sp = 0.3 + Math.random() * 0.5, a = Math.random() * Math.PI * 2, x = Math.random() * w, y = Math.random() * h;
      parts.push({ x: x, y: y, bx: x, by: y, vx: Math.cos(a) * sp, vy: Math.sin(a) * sp, s: 1 + Math.random() * 2, o: 0.3 + Math.random() * 0.4 });
    }
    addEventListener('mousemove', function (e) { mouse.x = e.clientX; mouse.y = e.clientY; }, { passive: true });
    addEventListener('mouseout', function () { mouse.x = -1e4; mouse.y = -1e4; });
    addEventListener('resize', resize);
    (function draw() {
      ctx.clearRect(0, 0, w, h); frame++;
      if (frame % 90 === 0 && Math.random() > 0.3) {
        var p = parts[(Math.random() * parts.length) | 0];
        pulses.push({ x: p.x, y: p.y, t: 0, i: 0.6 + Math.random() * 0.4 });
      }
      for (var k = pulses.length - 1; k >= 0; k--) { if (++pulses[k].t > 120) pulses.splice(k, 1); }
      for (var i = 0; i < parts.length; i++) {
        var p = parts[i], dx = mouse.x - p.x, dy = mouse.y - p.y, d = Math.sqrt(dx * dx + dy * dy);
        if (d < MR && d > 1) { p.vx += dx / d * 0.02; p.vy += dy / d * 0.02; }
        p.x += p.vx; p.y += p.vy;
        p.vx += (p.bx - p.x) * 0.0005; p.vy += (p.by - p.y) * 0.0005; p.vx *= 0.995; p.vy *= 0.995;
        if (p.x < -10) p.x = w + 10; if (p.x > w + 10) p.x = -10;
        if (p.y < -10) p.y = h + 10; if (p.y > h + 10) p.y = -10;
      }
      for (var i = 0; i < parts.length; i++) for (var j = i + 1; j < parts.length; j++) {
        var a = parts[i], b = parts[j], dx = a.x - b.x, dy = a.y - b.y, d = Math.sqrt(dx * dx + dy * dy);
        if (d < CD) {
          var al = (1 - d / CD) * 0.12;
          for (var q = 0; q < pulses.length; q++) {
            var pu = pulses[q], px = (a.x + b.x) / 2 - pu.x, py = (a.y + b.y) / 2 - pu.y, pd = Math.sqrt(px * px + py * py);
            if (pd < 100) al = Math.max(al, (1 - pd / 100) * pu.i * (1 - pu.t / 120));
          }
          var mx = (a.x + b.x) / 2 - mouse.x, my = (a.y + b.y) / 2 - mouse.y, md = Math.sqrt(mx * mx + my * my);
          if (md < 200) al = Math.max(al, (1 - md / 200) * 0.3);
          ctx.beginPath(); ctx.moveTo(a.x, a.y); ctx.lineTo(b.x, b.y);
          ctx.strokeStyle = rgbaAccent(al); ctx.lineWidth = 1; ctx.stroke();
        }
      }
      for (var i = 0; i < parts.length; i++) {
        var p = parts[i]; ctx.beginPath(); ctx.arc(p.x, p.y, p.s, 0, 6.283);
        ctx.fillStyle = rgbaAccent(p.o * 0.6); ctx.fill();
      }
      requestAnimationFrame(draw);
    })();
  }
})();
