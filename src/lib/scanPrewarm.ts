/**
 * 盘点预热(借鉴有戏剧场「空闲预热」):文件中心一挂载就在空闲期先把
 * 「扫根 + 第一层文件夹」拉回来缓存,并低并发预算各文件夹递归大小 ——
 * 用户点「盘点」时选择器零等待直接开,大小列也大多已就位。
 *
 * 设计:
 * - takeScan() 复用飞行中/已完成的请求;TTL 过期或盘点完成后失效重拉。
 * - 大小预算并发压到 2(选择器打开后是 4),避免后台抢 IO 让前台变卡。
 * - 选择器打开时 stopSizePrewarm() 停掉后台泵,由选择器自己的队列接管;
 *   已算好的经 peekSizes() 秒回填,在飞的完成后仍落入缓存并回调订阅者。
 */
import { files as fc, type FolderScan, type FolderSize } from "../tauri";

const TTL_MS = 60_000;

let scanPromise: Promise<FolderScan> | null = null;
let scanAt = 0;

const sizes = new Map<string, FolderSize>();
let sizeQueue: string[] = [];
let sizeActive = 0;
let sizePumping = true;
const SIZE_CONCURRENCY = 2;
type SizeCb = (path: string, size: FolderSize) => void;
let sizeCb: SizeCb | null = null;

function pumpSize() {
  while (sizePumping && sizeActive < SIZE_CONCURRENCY && sizeQueue.length) {
    const p = sizeQueue.shift()!;
    if (sizes.has(p)) continue;
    sizeActive++;
    fc.folderSize(p)
      .then((r) => {
        sizes.set(p, r);
        sizeCb?.(p, r);
      })
      .catch(() => {})
      .finally(() => {
        sizeActive--;
        pumpSize();
      });
  }
}

/** 空闲预热:拉扫描结构,完成后低并发预算第一层文件夹大小。幂等,可反复调。 */
export function prewarmScan(): void {
  void takeScan().then((res) => {
    sizePumping = true;
    for (const n of res.folders) {
      if (!sizes.has(n.path) && !sizeQueue.includes(n.path)) sizeQueue.push(n.path);
    }
    pumpSize();
  }).catch(() => {});
}

/** 取扫描结果(复用缓存/飞行中的请求;过期自动重拉)。 */
export function takeScan(): Promise<FolderScan> {
  const now = Date.now();
  if (scanPromise && now - scanAt < TTL_MS) return scanPromise;
  scanAt = now; // 先占位防并发重复发起;完成后再刷成完成时刻
  scanPromise = fc
    .scanFolders(null)
    .then((r) => {
      // TTL 从**完成时刻**起算:慢盘上 scanFolders 本身要几十秒,若从发起时刻计,
      // 解析完剩余有效期近 0,下一次 takeScan 立即判过期重拉,预热形同虚设。
      scanAt = Date.now();
      return r;
    })
    .catch((e) => {
      scanPromise = null; // 失败不缓存,下次重试
      throw e;
    });
  return scanPromise;
}

/** 已预算好的文件夹大小快照(选择器打开时秒回填)。 */
export function peekSizes(): ReadonlyMap<string, FolderSize> {
  return sizes;
}

/** 选择器接管后订阅在飞预算的落地;传 null 取消订阅。 */
export function onPrewarmSize(cb: SizeCb | null): void {
  sizeCb = cb;
}

/** 停掉后台大小泵(选择器打开时调,把 IO 让给前台自己的队列)。 */
export function stopSizePrewarm(): void {
  sizePumping = false;
  sizeQueue = [];
}

/** 盘点完成后结构/大小都可能变了 → 全部失效,下次预热重拉。 */
export function invalidateScanPrewarm(): void {
  scanPromise = null;
  scanAt = 0;
  sizes.clear();
  sizeQueue = [];
}
