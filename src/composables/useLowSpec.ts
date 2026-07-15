/**
 * 低配机器探测（纯前端，零后端往返）。
 *
 * WebView2(Win) 与 Docker 浏览器都是 Chromium 内核，`navigator.deviceMemory`
 * 会给出物理内存的近似档位（GB，隐私考量下取 4/8 等粗粒度）；WKWebView(Mac)
 * 无此 API 时回落到核数与系统「减少动态效果」偏好。命中任一即判低配：
 * 据此把开屏/极光等装饰动画降级、聊天历史折叠阈值调小，让弱机也不卡。
 *
 * 只在模块首次加载时算一次（机器配置一次会话内不变），各处直接读常量。
 */
function detect(): boolean {
  try {
    if (typeof navigator === "undefined") return false;
    const dm = (navigator as unknown as { deviceMemory?: number }).deviceMemory;
    if (typeof dm === "number" && dm > 0 && dm <= 4) return true; // ≤4GB
    const cores = navigator.hardwareConcurrency || 4;
    if (cores <= 2) return true; // 双核及以下
    if (
      typeof window !== "undefined" &&
      window.matchMedia?.("(prefers-reduced-motion: reduce)").matches
    )
      return true;
    return false;
  } catch {
    return false;
  }
}

/** 本机是否为「低配」（内存小 / 核少 / 已开减少动效）。会话内恒定。 */
export const isLowSpec = detect();
