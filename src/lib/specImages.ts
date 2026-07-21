// spec 配图路径 → data URL 内联(DeckStudio 与 RightDrawer 共用)。
//
// iframe srcdoc 是不透明源,直接给 file:// 路径必然加载失败,必须把字节内联进来
// (artifact_read 对图片类会回 dataUrl)。读不到就把 image 留成原路径,slidesSpec 会
// 渲染成「配图待载入」占位框 —— 预览缺图不阻断,导出那边照样有图。
// 缓存按路径存,免得轮询每次都把几百 KB 的图重读一遍。

import { artifacts as artifactsApi } from "../tauri";

const imgCache = new Map<string, string>();

/** 就地把 spec 里所有本地图路径换成 data URL(固定版式 sl.image + freeform 盒 image/pic)。 */
export async function resolveSpecImages(spec: any): Promise<any | null> {
  if (!spec || !Array.isArray(spec.slides)) return spec;
  // 图字段有两处:固定版式的 sl.image,与 freeform 的 sl.boxes[].image(image/pic 盒)。
  // 统一收集成 {holder,key} 再解析:两条路共用一次读盘 + 同一份 imgCache。
  const targets: { holder: any; key: string }[] = [];
  for (const sl of spec.slides) {
    if (typeof sl?.image === "string") targets.push({ holder: sl, key: "image" });
    if (Array.isArray(sl?.boxes)) {
      for (const b of sl.boxes) {
        const ty = String(b?.type ?? "");
        if ((ty === "image" || ty === "pic") && typeof b?.image === "string") {
          targets.push({ holder: b, key: "image" });
        }
      }
    }
  }
  await Promise.all(
    targets.map(async ({ holder, key }) => {
      const path = String(holder[key] ?? "").trim();
      if (!path || /^(data:|https?:)/i.test(path)) return;
      const hit = imgCache.get(path);
      if (hit) {
        holder[key] = hit;
        return;
      }
      try {
        const r = await artifactsApi.read(path);
        if (r?.dataUrl) {
          imgCache.set(path, r.dataUrl);
          holder[key] = r.dataUrl;
        }
      } catch {
        /* 读不到就留原路径 → 占位框 */
      }
    })
  );
  return spec;
}
