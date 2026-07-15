// src/features/collab/fsapi.ts —— 经 iroh 隧道代理端口调远程主机(NAS)的 fsface。
// remoteBase(s) = http://127.0.0.1:<隧道端口>,请求随隧道透传到对端 server 的 /api/fs/*。
import { type RemoteSource, remoteBase } from "../interconnect/remoteSources";

export interface FsEntry {
  name: string;
  is_dir: boolean;
  size: number;
  mtime: number;
}

function joinPath(cwd: string, name: string): string {
  return cwd ? `${cwd}/${name}` : name;
}

async function req(s: RemoteSource, path: string, rel: string): Promise<Response> {
  const u = `${remoteBase(s)}${path}?path=${encodeURIComponent(rel)}`;
  return fetch(u, {
    headers: s.token ? { Authorization: `Bearer ${s.token}` } : {},
  });
}

async function errText(r: Response): Promise<string> {
  try {
    const j = await r.json();
    if (j && typeof j.error === "string") return j.error;
  } catch {
    /* 非 JSON 错误体 */
  }
  return `HTTP ${r.status}`;
}

/** 列目录(rel 相对浏览根,"" = 根)。 */
export async function fsList(s: RemoteSource, rel: string): Promise<FsEntry[]> {
  const r = await req(s, "/api/fs/list", rel);
  if (!r.ok) throw new Error(`列目录失败:${await errText(r)}`);
  const j = await r.json();
  return (j.entries || []) as FsEntry[];
}

/** 下载文件字节(前端转 blob 存盘 / 预览)。 */
export async function fsDownload(s: RemoteSource, rel: string): Promise<Blob> {
  const r = await req(s, "/api/fs/read", rel);
  if (!r.ok) throw new Error(`下载失败:${await errText(r)}`);
  return await r.blob();
}

export { joinPath };
