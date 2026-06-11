// 错误 → 人话。所有展示给用户的错误先过这里:常见模式映射成可行动的提示,
// 兜底保留原文(截断)但不再裸漏 Rust panic / HTTP 状态码。
export function humanizeError(err: unknown): string {
  const raw =
    err instanceof Error
      ? err.message
      : typeof err === "string"
        ? err
        : String((err as any)?.message ?? err ?? "");
  const s = raw.toLowerCase();

  if (
    s.includes("invalid api key") ||
    s.includes("authentication") ||
    s.includes("401") ||
    s.includes("invalid x-api-key")
  )
    return "API 密钥无效或已过期——请到「API 供应商」里检查当前供应商的密钥。";
  if (s.includes("credit") || s.includes("quota") || s.includes("insufficient"))
    return "额度不足——当前供应商余额/配额可能用完了,请到「API 供应商」检查或换一家。";
  if (s.includes("rate limit") || s.includes("429"))
    return "请求太频繁被限流了,稍等几秒再试。";
  if (
    s.includes("econnrefused") ||
    s.includes("connection refused") ||
    s.includes("failed to fetch") ||
    s.includes("network") ||
    s.includes("socket") ||
    s.includes("timed out") ||
    s.includes("timeout")
  )
    return "网络连接失败——请检查网络,或当前 API 服务暂时不可达,稍后重试。";
  if (/http 5\d\d|"?5\d\d"? (internal|bad gateway|service)/.test(s) || s.includes("overloaded"))
    return "服务端暂时不可用(5xx)——通常稍等片刻重试即可。";
  if (s.includes("program not found") || s.includes("claude") && s.includes("not found"))
    return "找不到 Claude Code——请到「环境」页检测并安装。";
  if (s.includes("permission denied") || s.includes("access is denied") || s.includes("拒绝访问"))
    return "文件被占用或没有权限——关闭占用该文件的程序后重试。";
  if (s.includes("no such file") || s.includes("not found") || s.includes("找不到"))
    return `文件或资源不存在:${raw.slice(0, 120)}`;

  const out = raw.trim() || "操作失败,请稍后重试。";
  return out.length > 200 ? out.slice(0, 200) + "…" : out;
}
