# 动效执行配方库（recipes.md）——拷贝即用，任何执行模型都别自己发明

> **给执行模型的话**：下面每个配方都是「已定稿的成品代码」。你的工作是**抄进作品、换掉参数区的值**（颜色/时长/文案），不是重新发明。发明出来的动效十有八九违反 taste.md；抄配方的动效一定合规。
> 每个配方末尾有「验收断言」——交付前逐条自查，全过才算做对。
> 硬前提（所有配方内建，别删）：只动 `transform/opacity/filter`；进视口用 IntersectionObserver 一次性触发；滚动联动只用 CSS `animation-timeline`；**绝不 `addEventListener('scroll')`**；`prefers-reduced-motion` 收敛为可读静态终值。
> 版本 v1.0（2026-07-07）。配方编号 R-Mn 对应 motion-library.md 的 [Mn]。

---

## R0 · 共用底座（每个作品先抄这段，其它配方都依赖它）

```html
<script>
// ==== R0: 共用底座 ====
const REDUCED = matchMedia('(prefers-reduced-motion: reduce)').matches;
const FINE    = matchMedia('(pointer: fine)').matches; // 触屏=false → 关磁吸/倾斜/聚光跟随

// 进视口一次性揭示：给元素加 class="rv"（可选 style="--i:0/1/2..." 错峰）
const io = new IntersectionObserver(es => es.forEach(e => {
  if (e.isIntersecting) { e.target.classList.add('in'); io.unobserve(e.target); }
}), { threshold: .18 });
document.querySelectorAll('.rv').forEach(el => io.observe(el));
</script>
<style>
/* 揭示基调：未入场下移微隐 → 入场归位。REDUCED 时直接终值 */
.rv { opacity: 0; transform: translateY(12px); transition: opacity .6s cubic-bezier(.16,1,.3,1), transform .6s cubic-bezier(.16,1,.3,1); transition-delay: calc(var(--i, 0) * 60ms); }
.rv.in { opacity: 1; transform: none; }
@media (prefers-reduced-motion: reduce) { .rv { opacity: 1; transform: none; transition: none; } }
</style>
```
**验收断言**：① 全文搜不到 `addEventListener('scroll'`；② reduce-motion 下页面首屏即完整可读；③ `.rv` 只出现在需要揭示的块上，不是所有元素。

## R-M15 · 数字滚动 count-up

```html
<span class="num" data-count="36">0</span>
<script>
// 依赖 R0 的 io 思路：进视口一次性滚动，REDUCED 直接显终值
document.querySelectorAll('[data-count]').forEach(el => {
  const end = +el.dataset.count;
  if (REDUCED) { el.textContent = end; return; }
  new IntersectionObserver((es, ob) => es.forEach(e => {
    if (!e.isIntersecting) return; ob.disconnect();
    const t0 = performance.now(), D = 900;                    // 时长 0.9s
    (function tick(t) {
      const p = Math.min((t - t0) / D, 1), k = 1 - Math.pow(1 - p, 3); // easeOutCubic
      el.textContent = Math.round(end * k);
      if (p < 1) requestAnimationFrame(tick);
    })(t0);
  })).observe(el);
});
</script>
<style>.num { font-variant-numeric: tabular-nums; }</style>
```
**验收断言**：① `tabular-nums` 在；② REDUCED 直接显终值；③ 只对关键指标用（≤4 处/页）。

## R-M16 · 拆字入场（只给 Hero 一句）

```html
<h1 class="split" id="hero-title">让文字自己走进来</h1>
<script>
// 拆成逐字 span；REDUCED 不拆直接显整句
const h = document.getElementById('hero-title');
if (!REDUCED) {
  h.innerHTML = [...h.textContent].map((c, i) =>
    `<span class="ch" style="--i:${i}">${c === ' ' ? '&nbsp;' : c}</span>`).join('');
  requestAnimationFrame(() => requestAnimationFrame(() => h.classList.add('go')));
}
</script>
<style>
.split .ch { display: inline-block; opacity: 0; transform: translateY(.5em); filter: blur(6px);
  transition: opacity .6s cubic-bezier(.16,1,.3,1), transform .6s cubic-bezier(.16,1,.3,1), filter .6s cubic-bezier(.16,1,.3,1);
  transition-delay: calc(var(--i) * 28ms); }               /* 错峰 24–40ms */
.split.go .ch { opacity: 1; transform: none; filter: none; }
</style>
```
**验收断言**：① 全页只有 1 个 `.split`；② 正文/小字没被拆；③ REDUCED 显整句（JS 未执行拆分）。

## R-M17 · 磁吸指针（只给主 CTA / 品牌标，一页 ≤2 个）

```html
<a class="magnet" href="#">进入体系</a>
<script>
if (FINE && !REDUCED) document.querySelectorAll('.magnet').forEach(el => {
  const R = 120, PULL = .28, MAX = 10;                      // 感应半径/吸力/位移封顶 px
  el.parentElement.addEventListener('pointermove', ev => {
    const b = el.getBoundingClientRect(),
          dx = ev.clientX - (b.left + b.width / 2), dy = ev.clientY - (b.top + b.height / 2);
    const d = Math.hypot(dx, dy), f = d < R ? PULL : 0;
    el.style.setProperty('--mx', Math.max(-MAX, Math.min(MAX, dx * f)) + 'px');
    el.style.setProperty('--my', Math.max(-MAX, Math.min(MAX, dy * f)) + 'px');
  });
  el.parentElement.addEventListener('pointerleave', () => {
    el.style.setProperty('--mx', '0px'); el.style.setProperty('--my', '0px');
  });
});
</script>
<style>
.magnet { display: inline-block; transform: translate(var(--mx, 0), var(--my, 0));
  transition: transform .2s cubic-bezier(.16,1,.3,1); will-change: transform; }
</style>
```
**验收断言**：① 位移封顶 ≤10px；② 触屏与 REDUCED 不挂监听（`FINE && !REDUCED` 闸在）；③ `.magnet` 全页 ≤2 个。

## R-M18 · 聚光跟随便当（一组共享一套坐标）

```html
<div class="bento" id="wall">
  <article class="cell">…</article><article class="cell">…</article><!-- 更多格 -->
</div>
<script>
// 整面墙只挂一个监听，只写两个变量——每格自己算坐标是违纪
const wall = document.getElementById('wall');
if (FINE && !REDUCED) wall.addEventListener('pointermove', ev => {
  const b = wall.getBoundingClientRect();
  wall.style.setProperty('--x', ((ev.clientX - b.left) / b.width * 100) + '%');
  wall.style.setProperty('--y', ((ev.clientY - b.top) / b.height * 100) + '%');
});
</script>
<style>
.bento { display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 12px; }
.cell  { position: relative; background: var(--surface1); border: 1px solid rgba(255,255,255,.08);
         border-radius: 14px; overflow: hidden; }
/* 聚光：无彩白光、只加光不位移。260px 为光斑半径 */
.cell::before { content: ""; position: absolute; inset: -1px; pointer-events: none; opacity: 0;
  background: radial-gradient(260px at var(--x, 50%) var(--y, 50%), rgba(255,255,255,.10), transparent 70%);
  transition: opacity .3s; }
.bento:hover .cell::before { opacity: 1; }
/* 指针侧描边亮起：同一套坐标，mask 出靠近指针的一段边 */
.cell::after { content: ""; position: absolute; inset: 0; border-radius: inherit; pointer-events: none;
  border: 1px solid var(--accent); opacity: 0; transition: opacity .3s;
  -webkit-mask: radial-gradient(180px at var(--x, 50%) var(--y, 50%), #000, transparent 70%);
          mask: radial-gradient(180px at var(--x, 50%) var(--y, 50%), #000, transparent 70%); }
.bento:hover .cell::after { opacity: .5; }
/* 触屏/REDUCED：恒定静态微光，完整可读 */
@media (pointer: coarse), (prefers-reduced-motion: reduce) {
  .cell::before { opacity: .35; background: radial-gradient(200px at 50% 0%, rgba(255,255,255,.06), transparent 70%); }
  .cell::after { display: none; }
}
</style>
```
**验收断言**：① 只有 1 个 pointermove、只写 `--x/--y`；② 格子零位移（无 transform 位移、无 box-shadow）；③ 触屏/REDUCED 落静态微光；④ 高光下格内小字对比仍 ≥4.5:1。

## R-M19 · 微倾斜光泽卡（一页 ≤3 张主卡）

```html
<div class="tiltwrap"><article class="tilt"><div class="glare"></div>…卡内容…</article></div>
<script>
if (FINE && !REDUCED) document.querySelectorAll('.tiltwrap').forEach(w => {
  const card = w.querySelector('.tilt'), A = 6;             // 最大角度 ≤6°
  w.addEventListener('pointermove', ev => {
    const b = w.getBoundingClientRect(),
          px = (ev.clientX - b.left) / b.width - .5, py = (ev.clientY - b.top) / b.height - .5;
    card.style.setProperty('--rx', (-py * A) + 'deg');
    card.style.setProperty('--ry', ( px * A) + 'deg');
    card.style.setProperty('--gx', (px * 100 + 50) + '%');
  });
  w.addEventListener('pointerleave', () => ['--rx','--ry'].forEach(v => card.style.setProperty(v, '0deg')));
});
</script>
<style>
.tiltwrap { perspective: 800px; }
.tilt { position: relative; transform: rotateX(var(--rx, 0)) rotateY(var(--ry, 0));
        transition: transform .3s cubic-bezier(.16,1,.3,1); overflow: hidden; }
.glare { position: absolute; inset: 0; pointer-events: none; opacity: 0; transition: opacity .3s;
  background: linear-gradient(105deg, transparent 40%, rgba(255,255,255,.10) var(--gx, 50%), transparent 60%); }
.tiltwrap:hover .glare { opacity: 1; }
@media (pointer: coarse), (prefers-reduced-motion: reduce) { .tilt { transform: none !important; } .glare { display: none; } }
</style>
```
**验收断言**：① 角度 ≤6°；② 复位有 300ms 过渡；③ 触屏/REDUCED 落平；④ `.tiltwrap` ≤3。

## R-M20 · 遮罩揭幕（只给章节封面/主图）

```html
<figure class="wipe rv">…主图/色块…</figure>
<style>
/* 复用 R0 的 .rv/.in 触发：从左向右揭幕 */
.wipe { clip-path: inset(0 100% 0 0); transition: clip-path .8s cubic-bezier(.16,1,.3,1); }
.wipe.in { clip-path: inset(0 0 0 0); }
@media (prefers-reduced-motion: reduce) { .wipe { clip-path: none; transition: none; } }
</style>
```
**验收断言**：① 只用于章节封面/主图（≤1 处/章）；② REDUCED 直接全显；③ 时长 600–900ms。

## R-M21 · 金属流光字（只落 1 个大标题关键词）

```html
<h2 class="sheen">高级感</h2>
<style>
.sheen { display: inline-block; color: transparent; -webkit-background-clip: text; background-clip: text;
  /* 参数区：底色↔高光。底色必须自身可读（相当于素色字），高光只是加亮 */
  background-image: linear-gradient(100deg, #C9CDD4 0 42%, #FFFFFF 50%, #C9CDD4 58% 100%);
  background-size: 250% 100%; animation: sheen 8s linear infinite; }
@keyframes sheen { from { background-position: 120% 0; } to { background-position: -120% 0; } }
@media (prefers-reduced-motion: reduce) { .sheen { animation: none; background-position: 50% 0; } }
</style>
```
**验收断言**：① 全页 ≤1 处、只在大标题；② 底色两端与背景对比 ≥3:1（大字标准）；③ REDUCED 停在最亮帧附近静止。

## R-M22 · 解码打字（仅 Hero 一句；动画是增强非承重墙）

```html
<p class="decrypt" data-text="系统就绪，欢迎回来。">系统就绪，欢迎回来。</p>
<script>
// 注意：HTML 里已写终文（首帧/无 JS/REDUCED 都完整可读），JS 只是把它"重演"一遍
document.querySelectorAll('.decrypt').forEach(el => {
  if (REDUCED) return;
  const label = el.dataset.text, GLYPH = '█▓▒░<>/\\|#*+=';
  new IntersectionObserver((es, ob) => es.forEach(e => {
    if (!e.isIntersecting) return; ob.disconnect();
    let frame = 0; const total = label.length * 3;          // 每字约 3 帧解码
    (function tick() {
      frame++;
      el.textContent = [...label].map((c, i) =>
        i < frame / 3 ? c : GLYPH[(Math.random() * GLYPH.length) | 0]).join('');
      if (frame < total) requestAnimationFrame(tick); else el.textContent = label;
    })();
  })).observe(el);
});
</script>
```
**验收断言**：① HTML 源码里就是终文（禁 JS 也可读）；② REDUCED 完全不动；③ 全页 ≤1 处、等宽或 `tabular-nums` 防抖。

## R-M23 · 钉幕缩放（CSS scroll-timeline 优先，一页 ≤1 处）

```html
<section class="pinstage"><div class="pinvisual">…主视觉…</div></section>
<style>
/* 支持 animation-timeline 的浏览器：随滚动进度 0.94→1 缩放+提亮；不支持自动没有动画=静态可读 */
@supports (animation-timeline: view()) {
  .pinvisual { animation: pinzoom linear both; animation-timeline: view(); animation-range: entry 0% cover 45%; }
  @keyframes pinzoom { from { transform: scale(.94); opacity: .55; } to { transform: scale(1); opacity: 1; } }
}
@media (prefers-reduced-motion: reduce) { .pinvisual { animation: none; } }
</style>
```
**验收断言**：① 零 JS、零 scroll 监听；② 不支持的浏览器直接静态成立；③ scale 幅度 ≤0.94→1。

## R-M24 · 视差地层（CSS scroll-timeline，速差 ≤40px）

```html
<section class="strata"><div class="layer back">…背景层…</div><div class="layer front">…前景层…</div></section>
<style>
@supports (animation-timeline: view()) {
  .strata .back  { animation: drift-b linear both; animation-timeline: view(); }
  .strata .front { animation: drift-f linear both; animation-timeline: view(); }
  @keyframes drift-b { from { transform: translateY(24px); } to { transform: translateY(-24px); } }  /* 慢 */
  @keyframes drift-f { from { transform: translateY(40px); } to { transform: translateY(-40px); } }  /* 快 */
}
@media (prefers-reduced-motion: reduce), (max-width: 720px) { .strata .layer { animation: none; } }
</style>
```
**验收断言**：① 速差 ≤40px；② 移动端与 REDUCED 关视差仍完整；③ 零 JS。

## R-M25 · 弹性签名回位（全站唯一，只给品牌件）

```html
<div class="brandmark rv">◆</div>
<style>
/* 复用 .rv/.in：唯一允许超调的地方。超调 ≤6% 来自贝塞尔第二参 1.56 的收敛段 */
.brandmark { transition: transform .7s cubic-bezier(.34,1.56,.64,1), opacity .4s; transform: translateY(16px) scale(.9); opacity: 0; }
.brandmark.in { transform: none; opacity: 1; }
@media (prefers-reduced-motion: reduce) { .brandmark { transition: none; transform: none; opacity: 1; } }
</style>
```
**验收断言**：① 全站唯一一处弹性；② 周边功能 UI 全部 `cubic-bezier(.16,1,.3,1)` 类机械/有机缓动；③ REDUCED 直接落定。

## R-M26 · 融球导航（每页 ≤1 处，SVG goo 滤镜）

```html
<svg width="0" height="0" style="position:absolute"><filter id="goo">
  <feGaussianBlur in="SourceGraphic" stdDeviation="6" result="b"/>
  <feColorMatrix in="b" values="1 0 0 0 0  0 1 0 0 0  0 0 1 0 0  0 0 0 22 -11"/>
</filter></svg>
<nav class="goonav"><span class="blob" id="blob"></span>
  <a data-i="0" class="on">一</a><a data-i="1">二</a><a data-i="2">三</a>
</nav>
<script>
// 活动药丸平移穿过相邻项时因 goo 滤镜粘连再分离；REDUCED 直接跳变
const nav = document.querySelector('.goonav'), blob = document.getElementById('blob');
nav.querySelectorAll('a').forEach(a => a.addEventListener('click', ev => {
  nav.querySelector('.on')?.classList.remove('on'); a.classList.add('on');
  blob.style.transform = `translateX(${a.offsetLeft}px)`;
  blob.style.width = a.offsetWidth + 'px';
}));
</script>
<style>
.goonav { position: relative; display: inline-flex; gap: 4px; padding: 4px; border-radius: 999px;
  background: var(--surface1); filter: url(#goo); }        /* goo 只套指示层容器 */
.goonav a { position: relative; z-index: 1; padding: 8px 18px; border-radius: 999px; cursor: pointer; }
.blob { position: absolute; top: 4px; bottom: 4px; left: 0; border-radius: 999px; background: var(--accent);
  transition: transform .5s cubic-bezier(.16,1,.3,1), width .5s cubic-bezier(.16,1,.3,1); }
@media (prefers-reduced-motion: reduce) { .goonav { filter: none; } .blob { transition: none; } }
</style>
```
**验收断言**：① 每页 ≤1 处 goo；② REDUCED 无粘连直接跳变；③ 文字层 z-index 在 blob 之上、对比达标。

## R-M27 · 点阵光束场（氛围签名件，纯 CSS，绝不 WebGL）

```html
<div class="dotfield" aria-hidden="true"></div>
<style>
.dotfield { position: fixed; inset: 0; z-index: -1; pointer-events: none;
  background-image: radial-gradient(rgba(255,255,255,.07) 1px, transparent 1.5px);
  background-size: 26px 26px; animation: dotdrift 40s linear infinite; }
@keyframes dotdrift { from { background-position: 0 0; } to { background-position: 26px 52px; } }
/* 可选：一道柔光束（大模糊渐变条低速平移，透明度 ≤8%）*/
.dotfield::after { content: ""; position: absolute; inset: -20%; opacity: .07;
  background: linear-gradient(115deg, transparent 30%, var(--accent) 50%, transparent 70%);
  filter: blur(60px); animation: beam 26s ease-in-out infinite alternate; }
@keyframes beam { from { transform: translateX(-12%); } to { transform: translateX(12%); } }
@media (prefers-reduced-motion: reduce) { .dotfield, .dotfield::after { animation: none; } }
</style>
```
**验收断言**：① 透明度 ≤8%、周期 20s+；② 是全页唯一持续运动；③ REDUCED 静止成图；④ 无 canvas/WebGL。

## R-M6 · 跑马灯标点（每页 ≤1 个，唯一常驻运动）

```html
<div class="marquee" aria-hidden="true"><div class="track">
  <span>拆字 · 磁吸 · 聚光 · 视差 · 揭幕 ·&nbsp;</span><span>拆字 · 磁吸 · 聚光 · 视差 · 揭幕 ·&nbsp;</span>
</div></div>
<style>
/* 内容复制两份首尾相接；匀速线性；REDUCED 静止成图 */
.marquee { overflow: hidden; white-space: nowrap; }
.marquee .track { display: inline-flex; animation: mq 30s linear infinite; }
@keyframes mq { to { transform: translateX(-50%); } }
@media (prefers-reduced-motion: reduce) { .marquee .track { animation: none; } }
</style>
```
**验收断言**：① 每页 ≤1 个；② 匀速线性、20s+；③ REDUCED 静止；④ `aria-hidden` 在（装饰性内容）。

## R-M10 · 图像自身反馈（hover 改图不抬卡，双速时序）

```html
<a class="work"><img src="…" alt="…"><figcaption>标题</figcaption></a>
<style>
/* 文字 ~200ms、图片 ~400ms 双速造纵深；hover 只动 filter，不位移不阴影 */
.work img { filter: grayscale(60%) brightness(.85); transition: filter .4s cubic-bezier(.16,1,.3,1); }
.work figcaption { opacity: .7; transition: opacity .2s; }
.work:hover img { filter: none; }
.work:hover figcaption { opacity: 1; }
@media (prefers-reduced-motion: reduce) { .work img { filter: none; transition: none; } }
</style>
```
**验收断言**：① hover 零位移零阴影，只有 filter/opacity；② 双速（文字快图片慢）；③ REDUCED 直接显终态。

## R-M13 · 极光扫词 / R-M14 · 描边环转（补两个旧技法的定稿代码）

```html
<h1><span class="aurora-word">极光</span>时代</h1>
<button class="ring">开始</button>
<style>
/* M13：只落大标题关键词；相邻色相带，hue 呼吸 24s */
.aurora-word { color: transparent; -webkit-background-clip: text; background-clip: text;
  background-image: linear-gradient(120deg, #23D3A6, #3FA9E6, #C965D9);
  animation: hueb 24s ease-in-out infinite alternate; }
@keyframes hueb { to { filter: hue-rotate(28deg); } }      /* 相移 ≤30° */
/* M14：conic 描边环转，全站只给 1 个主元素 */
@property --a { syntax: "<angle>"; initial-value: 0deg; inherits: false; }
.ring { position: relative; border: none; border-radius: 12px; padding: 12px 28px; background: var(--surface1); }
.ring::before { content: ""; position: absolute; inset: -1px; z-index: -1; border-radius: 13px;
  background: conic-gradient(from var(--a), transparent 0 70%, var(--accent) 85%, transparent 100%);
  animation: turn 4s linear infinite; }
@keyframes turn { to { --a: 360deg; } }
@media (prefers-reduced-motion: reduce) { .aurora-word, .ring::before { animation: none; } }
</style>
```
**验收断言**：M13 只在大标题关键词、hue 相移 ≤30°；M14 全站 ≤1 个、低速低亮。

---

## 执行模型通用军规（写作品前默背）

1. **先抄 R0，再挑 ≤3 个配方**——设计师题词的「三招绝活」指定了用哪几个，别加菜。
2. **静音测试**：把所有动画想象成禁用，页面必须仍是完整、可读、好看的成品——动效是锦上添花，不是承重墙。
3. **对比度**：深底浅字/浅底深字；小字（≤14px）≥4.5:1，大字 ≥3:1；聚光/高光/渐变都不许把字冲淡到线下。
4. **配给**：签名件 1 个、文字动效 1 处（Hero）、交互动效只落焦点件、滚动编排 ≤1 处。数一遍，超了就砍。
5. **改参数不改骨架**：配方里的颜色/时长/半径按设计师色板换，`FINE/REDUCED` 闸、IO 触发、transform/opacity 限定这些骨架一律不动。
