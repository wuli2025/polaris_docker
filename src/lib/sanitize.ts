// 统一 HTML 消毒。marked 不消毒, 模型回复 / 被 WebFetch 抓回的网页内容里的原始 HTML
// 会原样进 v-html —— 在 Tauri webview 这个特权上下文里, `<img onerror=…>` / `<svg onload=…>`
// 可触达 __TAURI_INTERNALS__ 调后端。所有 marked.parse 的产物都必须过这里再喂 v-html。
import DOMPurify from "dompurify";

export function sanitizeHtml(html: string): string {
  // 默认配置已禁用 <script> 与 on* 事件属性、javascript: URL; 保留常规富文本/表格/代码块。
  return DOMPurify.sanitize(html, { USE_PROFILES: { html: true } });
}
