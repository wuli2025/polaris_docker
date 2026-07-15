// src/features/interconnect/remoteSources.ts
// 远程源:一台经 iroh 隧道接入的主机(NAS / 另一台电脑)。桌面侧持久化其连接参数,
// 供「互联」页展示与「文件中心」远程浏览共用。传输隐形,用户只认名字。
export interface RemoteSource {
  id: string; // 稳定 id(时间戳)
  name: string; // 展示名,如「群晖 NAS」
  nodeId: string; // iroh 主机 NodeId(z-base32)
  port: number; // 本地代理端口(127.0.0.1:port)
  token: string; // 连上该主机的 owner 令牌(fsface 鉴权)
  createdAt: number;
}

const KEY = "polaris.interconnect.remoteSources.v1";

export function loadRemoteSources(): RemoteSource[] {
  try {
    const raw = JSON.parse(localStorage.getItem(KEY) || "[]");
    return Array.isArray(raw) ? (raw as RemoteSource[]) : [];
  } catch {
    return [];
  }
}
export function saveRemoteSources(list: RemoteSource[]): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(list));
  } catch {
    /* storage 不可用:静默 */
  }
}
export function upsertRemoteSource(s: RemoteSource): RemoteSource[] {
  const list = loadRemoteSources().filter((x) => x.id !== s.id);
  list.push(s);
  saveRemoteSources(list);
  return list;
}
export function removeRemoteSource(id: string): RemoteSource[] {
  const list = loadRemoteSources().filter((x) => x.id !== id);
  saveRemoteSources(list);
  return list;
}
/** 本地代理基址;fsapi / collab api 用它当 base。 */
export function remoteBase(s: RemoteSource): string {
  return `http://127.0.0.1:${s.port}`;
}
