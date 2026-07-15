# 高级动效技法库（motion-library.md）

> 从一线产品官网的公开设计系统中**归纳出的通用动效方法**，用本仓语言重写为可落地、可检查的工程规则。这里只收「原理 + 参数区间 + 落地方式」，是所有 refero 系设计师共用的动效词汇表。
> 硬约束继承 `_foundation/aesthetics.md` 与 `taste.md`：只动 `transform`/`opacity`、用 IntersectionObserver 不监听 scroll、自定义缓动、`prefers-reduced-motion` 收敛为可读静态终值。
> 版本 v2.0（2026-07-07）。v1.0 收 M1–M15（Fable 5 归纳自约 285 个公开设计系统）；v2.0 增 M16–M27 高级交互/滚动/文字动效，汲取自 reactbits.dev 组件库与 refero.design 风格目录，全部重铸为「纯 transform/opacity/滤镜 + IO/scroll-timeline、绝不上 WebGL/canvas」的可落地纪律。引用记 `[Mn]`。
> **高级感来自动效，但更来自克制**：v2.0 的新技法威力更大，越要守「一页只放一次、只给唯一焦点」——磁吸/聚光/倾斜只许落在那个焦点件上，满屏都在动 = 廉价，一处动得恰好 = 高级。

---

## 贯穿性心法：高级动效是「配给」出来的，不是「铺满」出来的

调研 285 个顶级产品站得到的最强共识：**克制本身就是高级的signature**。绝大多数一流站点主动放弃满屏动画，把「动」当成稀缺资源，只在唯一焦点处释放一次。平庸设计到处都在动，高级设计大部分时间静止、只在关键一刻发声。因此本库的默认态是「静」，每一处「动」都要能回答「它为观者做了什么」。

---

## M1 · 稀缺色标点（Color Rationing）
全站近单色/无彩，唯一的高饱和强调色或渐变**只在转化点、焦点、状态处点亮**。颜色即注意力，稀缺才有力。
- 落地：强调色只出现在 CTA、当前项、关键数字、hover 激活态；正文与装饰一律无彩或极低饱和。
- 交互态用「色的出现」而非「形的位移」：灰→亮、边框透明→实、下划线 0→100%。过渡 120–200ms。

## M2 · 无阴影托层（Shadowless Elevation）
用**色阶明度台阶 + 1px 发丝边 + 毛玻璃背滤**造层级与景深，而非 box-shadow 抬升。深度靠合成，不靠影子。
- 落地：surface 分 3–5 档，越高越亮（暗主题）或越白（亮主题）；卡片边界用 `1px solid rgba(255,255,255,.08~.15)`；浮层用 `backdrop-filter: blur() saturate()`。
- 若必须用影子：低漂移、大模糊、低透明度（光的证据），绝不黑色硬边。

## M3 · 快 hover / 慢入场双速（Two-Speed Tempo）
微交互瞬时响应（120–200ms）给「手感」，英雄入场拉长（800–2500ms）给「气场」。同一页两种时间尺度分工明确。
- 落地：hover/active/focus 用 120–200ms；hero 首屏揭示用 800ms+；两者都用缓出曲线，不共用一个时长。

## M4 · 有重量的自定义缓动，拒弹簧（Weighted Easing, No Spring）
绝不用默认 linear/ease-in-out，也不用弹跳超调。用自定义贝塞尔模拟「有质量的物体缓缓入位」。
- 推荐曲线：`cubic-bezier(0.16,1,0.3,1)`（果断收尾）、`cubic-bezier(0.22,1,0.36,1)`（锚定缓入）、`cubic-bezier(0.4,0,0.2,1)`（机械短促，终端系）、`cubic-bezier(0.165,0.84,0.44,1)`（大气缓落）。
- 例外：弹簧/回弹**只留给「品牌角色」**（吉祥物、头像、hero 品牌立方体），功能 UI 永远机械克制。

## M5 · 单一签名艺术品（One Signature Piece）
一个具名的核心动态装置独扛全页景深与动感，其余界面保持沉默：粒子星座 / WebGL 旋转体 / 棱镜微光 / 网络图脉动 / 低透明度星云漂移。
- 落地：签名件占「动效预算」的主体，一页至多一个；它是品牌隐喻锚点，不是随处点缀。背景场用极低透明度持续缓漂（20s+ 周期）。

## M6 · 跑马灯标点（Marquee as Punctuation）
无限横向匀速滚动（logo 墙 / 章节词 / 公告条）作为**唯一常驻运动**，给静止版面一个节奏心跳。
- 落地：匀速 `linear()` 或长 duration 线性；每页 ≤1 个；`prefers-reduced-motion` 时静止成图。

## M7 · 滚动揭示 + 粘性毛玻璃导航（Scroll-Reveal & Frosted Nav）
元素进视口时淡入上移（IntersectionObserver 一次性触发）；导航滚动后浮现发丝边 + 背滤磨砂，悬浮失重。
- 落地：reveal 用 `translateY(8–16px)+opacity`，200–600ms 缓出；导航 `position:sticky` + `backdrop-filter`；绝不 `window.addEventListener('scroll')`。

## M8 · 巨字尺度即动效（Type-Scale as Motion）
超大字号 + 负字距把标题压成「实心字墙」，用排版的尺度张力承担「动感」，而非补间动画。
- 落地：display 字号 `clamp()` 流式顶到视口级；≥40px 标题字距 −1%～−4%，行高压到 0.85–0.95；权威来自尺寸不来自字重。

## M9 · 硬色场翻页（Color-Field Hard Cut）
满幅色块/明暗底直接切换当「隐形分隔」，滚动即翻章，不做淡入淡出。
- 落地：相邻区块背景明暗/色相硬切；配 48–100px 大间距造「杂志跨页」呼吸；深度靠对比不靠过渡。

## M10 · 图像自身状态反馈（Image-Level Feedback）
hover 不抬升元素，而是改变图片本身的亮度/饱和/遮罩——双级时序（文字 ~200ms、图片 ~400ms）。
- 落地：`filter: brightness()/grayscale()` 过渡；文字层与图片层用不同 duration 制造纵深。

## M11 · 方向性微位移（Directional Nudge）
箭头/链接 hover 右移几 px、下划线从左往右生长、L 形箭头——用「指向」代替「抬升」。
- 落地：`translateX(2–6px)`；下划线 `transform: scaleX()` 由 `transform-origin:left` 生长；焦点环内嵌。

## M12 · 分层视差 / z 叠（Layered Parallax）
3D 产品/设备/家具渲染浮于文本之上，靠 z 层叠与轻微微旋（2–8°）造深度，取代阴影。
- 落地：滚动时前后层用不同 `translateY` 速率（幅度克制，防晕）；静态错位 + 微旋也可无动画造能量。

## M13 · 渐变裁切文字扫过（Gradient-Clip Sweep）
Hero 关键词做色相扫描渐变（`background-clip:text`），是品牌招牌华彩。
- 落地：只落大标题关键词，两端色相相邻或分裂互补，缓慢 hue 呼吸（20s+）；绝不落正文/小字/指标数字 [A1]。

## M14 · 描边环转（Border-Turn / Conic Stroke）
沿元素边缘旋转的 1px 渐变/描边（conic-gradient）作全站唯一戏剧点，静界面里的一处精密运动。
- 落地：`@property` + conic-gradient 遮罩转边；只给 1 个主元素（主 CTA / 特性卡）；低速、低亮度。

## M15 · 数字滚动（Count-Up）
关键指标进视口时从 0 滚到目标，tabular 数字防抖。
- 落地：IntersectionObserver 触发，一次性；`font-variant-numeric: tabular-nums`；`prefers-reduced-motion` 直接显终值。

---

# 进阶技法 M16–M27（v2.0 · 汲取 reactbits.dev + refero.design）

> 这批更「主动」——文字自己会动、界面会回应指针、滚动成为时间轴。威力越大，配给越严：**每条都默认「一页一次、只落唯一焦点」**，且全部只动 transform/opacity/filter、优先 CSS scroll-timeline 而非 JS scroll 监听、reduce-motion 一律收敛为可读静态终值。

## M16 · 拆字入场（Split-Text Reveal）
主标题按字/词切块，逐个 `translateY(0.5em)+blur(6px)→0 + opacity 0→1`，错峰 24–40ms 一字，整句像被逐字敲出来——「巨字扉页」[M8] 的动态版。
- 落地：JS 拆标题为逐字 `<span>`，`transition-delay: calc(var(--i)*28ms)`；IO 一次性；缓出 `cubic-bezier(0.16,1,0.3,1)` 500–700ms；**只给 Hero 一句主标题**，正文/小字永不拆；`prefers-reduced-motion` 直接显整句。

## M17 · 磁吸指针（Magnetic Pull）
唯一焦点件（主 CTA / 品牌标）在指针进入半径时被缓缓「吸」向指针几 px，离开归位——给焦点一点引力，其余全不动。
- 落地：`pointermove` 内算 `dx,dy × 0.2–0.35` 写入 `--mx/--my`，元素 `translate(var(--mx),var(--my))` + `transition:transform 200ms cubic-bezier(0.16,1,0.3,1)`；位移封顶 ≤10px；**一页 ≤1–2 个焦点件**；触屏/`reduce-motion` 关闭。

## M18 · 聚光跟随（Spotlight Follow）
卡片/便当格随指针浮现一团柔和径向高光并让描边在指针一侧亮起——静止的格子被「照亮」，无位移、纯光（refero「Magic Bento」气质）。
- 落地：`pointermove` 写 `--x/--y` 百分比；卡面伪元素 `radial-gradient(240px at var(--x) var(--y), rgba(255,255,255,.10), transparent)`；描边用同坐标 mask/conic 亮边；一组便当共用一套光坐标；`reduce-motion` 落静态微光。

## M19 · 微倾斜光泽（Tilt & Glare）
关键卡随指针做 ≤6° 的 `rotateX/rotateY`，一道斜向高光扫过表面——给主卡一点「实体在手」的立体，克制不晕。
- 落地：容器 `perspective(800px)`，指针映射 `rotateX/Y`（幅度 ≤6°）；glare 用 `linear-gradient` 高光层跟指针平移；复位 `transition 300ms`；**一页 ≤3 张主卡**；`reduce-motion` 落平、去 glare。

## M20 · 遮罩揭幕转场（Mask / Clip Wipe）
区块或主图用 `clip-path`/`mask` 从一侧揭幕，像布幕拉开或像素消隐——比淡入更有「揭示」的仪式感，配 M9 硬色场做章节切换。
- 落地：`clip-path: inset(0 100% 0 0→0)` 或 `mask-position` 100%→0，进视口 IO 触发，600–900ms 缓出；**只给章节封面/主图**；`reduce-motion` 直接全显。

## M21 · 金属流光字（Metallic Sheen Sweep）
大标题上一道细窄高光如金属反光缓缓横扫（`background-clip:text` + 移动高光渐变），奢华冷静——不是彩虹，是一束反光。
- 落地：文字 `linear-gradient(100deg, 底色, 高光 45–55%, 底色)` + `background-clip:text`，`background-position` 6–10s 线性横移；**只落 1 个大标题关键词**；对比不达标退素色 [A1]；正文永不用。

## M22 · 解码打字（Decrypt / Type）
标题字符从随机字形「解码」落定，或按打字节奏逐字加光标浮现——终端/科技系的招牌文字动效。
- 落地：JS 每字先高频刷随机字符 300–600ms 再落定（或逐字加块光标 `steps()`）；**仅 Hero 一句**；等宽/`tabular-nums` 防抖；动画是增强非必需——首帧与 `reduce-motion` 直接显终文，可读性不依赖它。

## M23 · 钉幕缩放（Scroll-Pin Stage）
一个 hero 段在滚动中被「钉住」，其间主视觉随滚动进度缩放/明暗渐变，滚过后释放——滚动成了时间轴而非位移。
- 落地：优先 CSS `animation-timeline: view()/scroll()` 驱动 `scale(0.94→1)/opacity`（零 JS scroll 监听）；不支持时降级为 IO 一次性揭示（不钉）；幅度克制；**一页 ≤1 处**；`reduce-motion` 落终值。

## M24 · 视差地层（Parallax Strata）
前中后景以不同速率随滚动轻移（`translateY` 速差 ≤40px），造纵深地层——克制幅度防晕，用运动差取代阴影堆叠 [M12 的滚动版]。
- 落地：优先 `animation-timeline: scroll()` 给各层不同 `translateY` 幅度；背景层最慢、前景最快；**绝不 `addEventListener('scroll')`**；移动端与 `reduce-motion` 关视差、落静态错位。

## M25 · 弹性签名回位（Elastic Brand Settle）
唯一的品牌角色（logo / 吉祥物 / hero 立方）入场以一次轻微超调回弹落位——全站唯一允许弹簧处，功能 UI 永远机械 [M4 的唯一例外]。
- 落地：`cubic-bezier(0.34,1.56,0.64,1)` 超调 ≤6%、一次性、**仅品牌件**；周边功能元素保持 M4 机械缓动；`reduce-motion` 去超调直接落定。

## M26 · 融球流体（Gooey Merge）
导航药丸/指示器/光标移动时以「融球」粘连再分离（SVG goo 滤镜），液态有机——只给 1 个具名交互件当签名，非满屏果冻。
- 落地：SVG `feGaussianBlur`+`feColorMatrix` goo 滤镜套在导航指示层；活动药丸 `transform` 平移穿过相邻块时粘连；**每页 ≤1 处**；纯 transform；`reduce-motion` 直接跳变、无粘连。

## M27 · 点阵光束场（Dot-Field & Light-Beams）
深底上一层极低对比点阵/网格随极慢波动，或几道柔光束缓移，作 M5 签名件的「氛围场」变体——CSS/SVG 造，**绝不上 WebGL/canvas**（reactbits 的 Beams/Threads/Dot-Grid 的克制 CSS 版）。
- 落地：点阵用 `radial-gradient` 平铺 + 极慢 `background-position`/`opacity` 波；光束用大模糊 `linear-gradient` 条低速平移；透明度 ≤8%、20s+ 周期；作全页唯一持续运动 [M5]；`reduce-motion` 静止成图。

---

## 缓动的二元性（机械 vs 有机）
- **机械手感**（终端/工业/仪表系）：短促、`cubic-bezier(0.4,0,0.2,1)`、120–200ms，像物理开关咔哒入位。
- **有机手感**（氛围/编辑/奢华系）：长时、`cubic-bezier(0.16~0.22,1,0.3~0.36,1)`、600–1200ms，像镜头缓缓对焦。
- 一位设计师**只选一种手感到底**，不在同一作品里混用两套物理。

## 动效预算（所有 refero 系设计师通用）
一页至多：1 个签名件（M5 / M27）+ 1 类入场基调（M7 / M16 / M20）+ 1 处跑马灯（M6）+ 无限量的「稀缺色标点」微交互（M1，因为它本质是静态的色变）。超出即砍。
- **交互动效（M17 磁吸 / M18 聚光 / M19 倾斜 / M26 融球）只落唯一焦点件**：整组便当可共享一套聚光坐标，但磁吸/倾斜一页 ≤1–2 个焦点，绝不给每张卡都上——满屏回应指针是廉价的第一信号。
- **文字动效（M16 拆字 / M21 流光 / M22 解码）只给 Hero 一句主标题**，一页 ≤1 处，正文与小字永远静止。
- **滚动编排（M23 钉幕 / M24 视差）一页 ≤1 处**，且优先 CSS scroll-timeline、必须能在不支持/`reduce-motion` 时无损降级为静态。
- **弹簧（M25）全站唯一一处，只给品牌角色**；功能 UI 一律 M4 机械缓动。
- 自检口诀：把页面静音截图，若「看起来仍是完整可读的成品」才算动效用对了——动效是锦上添花，不是承重墙。
