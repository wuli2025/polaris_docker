import { defineStore } from "pinia";
import { ref, computed } from "vue";

/**
 * 「让 AI 更懂你」引导向导的全局开关 + 知识库画像(个人 / 企业)。
 *
 * 向导本体常驻挂在 App.vue(不随视图切换销毁),靠这个 store 控制显隐 —— 于是扫描/归类
 * 跑着时用户可以「转入后台」隐掉浮层、最小化窗口、去逛别的视图,后台线程与事件监听照常推进,
 * 再点「智能向导」回来还停在原来那一步(状态不丢)。
 *
 * 画像(profile)决定走哪套知识构建方案(见桌面《聚类派 B 方案与框架派 D 方案改造报告》):
 *   - personal(个人 / C 端)→ B 方案 · 聚类驱动:自动发现主题、起中文名,让你「这就是我常用的文件」;
 *   - enterprise(企业 / B 端)→ D 方案 · Schema-Guided:先选行业框,在框内抽显式三元组,低幻觉可审计。
 */
export type KbProfile = "personal" | "enterprise";

const PROFILE_KEY = "polaris.kbProfile.v1";
const SCHEMA_KEY = "polaris.kbSchema.v1";

function loadProfile(): KbProfile | null {
  try {
    const p = localStorage.getItem(PROFILE_KEY);
    if (p === "personal" || p === "enterprise") return p;
  } catch {
    /* storage 不可用 */
  }
  return null;
}

export const useWizardStore = defineStore("wizard", () => {
  const open = ref(false);
  // 已选画像(null = 还没选过 → 向导首步让用户选)。持久化:下次开软件记得用户是个人还是企业。
  const profile = ref<KbProfile | null>(loadProfile());
  // 企业路径选定的行业 schema id(如 finance / medical / ecommerce …)。
  const schemaId = ref<string>(localStorage.getItem(SCHEMA_KEY) || "");

  // 个人 → 聚类(B);企业 → 框架抽取(D)。
  const method = computed<"cluster" | "schema">(() =>
    profile.value === "enterprise" ? "schema" : "cluster",
  );

  function setProfile(p: KbProfile) {
    profile.value = p;
    try {
      localStorage.setItem(PROFILE_KEY, p);
    } catch {
      /* ignore */
    }
  }
  function setSchema(id: string) {
    schemaId.value = id;
    try {
      localStorage.setItem(SCHEMA_KEY, id);
    } catch {
      /* ignore */
    }
  }

  function openWizard() {
    open.value = true;
  }
  function closeWizard() {
    open.value = false;
  }
  return { open, profile, schemaId, method, setProfile, setSchema, openWizard, closeWizard };
});
