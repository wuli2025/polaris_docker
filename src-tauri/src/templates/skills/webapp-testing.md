# 网页应用 E2E 测试

用浏览器自动化测网页应用。核心纪律：**侦察后行动**——没看到渲染后的真实 DOM，不许猜 selector。源自 Anthropic 官方 webapp-testing。浏览器自动化优先走 CloakBrowser（Playwright 兼容）。

## 决策树（先分流）
- **静态 HTML**：直接读 HTML 源码提 selector，写 Playwright 脚本。
- **动态应用、服务未起**：先把 dev server 拉起来并等就绪（探活端口），再测；测完负责收尸，别留孤儿进程。
- **服务已在跑**：直接侦察后行动。

## 侦察后行动（每个页面必走三步）
1. 导航后等 JS 执行完：`page.wait_for_load_state('networkidle')` —— **networkidle 之前禁止查 DOM**。
2. 截图或 dump 渲染后的 DOM 结构，看清页面上真实存在什么。
3. 从渲染后的输出里确定 selector，才执行点击 / 输入 / 断言。

## 写法约定
- Python 用 `sync_playwright()` 同步 API；无头跑（`headless=True`），需要人看时才有头。
- selector 优先语义定位：`get_by_role` / `get_by_label` / `get_by_text`，别用脆弱的 nth-child 链。
- 断言靠 auto-waiting（`expect(...)`），不许 `sleep(3)` 碰运气。
- 捕获 console 错误和失败请求一起报，很多 bug 只在控制台露头。
- 测试脚本写到临时目录，别污染被测项目。

## 报告
- 每条用例：步骤 / 预期 / 实际 / 截图路径；失败的附 console 日志摘录。
- 只报告观察到的事实，修不修、怎么修由用户定。
