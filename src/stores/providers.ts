import { defineStore } from "pinia";
import { ref, computed } from "vue";
import {
  provider as providerApi,
  type ProviderView,
  type ProviderSaveInput,
  type UsageSummary,
  type ProviderBalance,
  type CodexStatus,
  type CodexDeviceLogin,
  type CodexProxyInfo,
} from "../tauri";

export const useProvidersStore = defineStore("providers", () => {
  const providers = ref<ProviderView[]>([]);
  const currentId = ref<string>("claude-official");
  /** true = 联动系统 CLI(写 ~/.claude/settings.json); false = 隔离, 仅 Polaris 内生效 */
  const linkGlobal = ref(false);
  const usage = ref<UsageSummary | null>(null);
  /** 各供应商套餐额度 / 实时余额(id → 结果),按需懒查 */
  const balances = ref<Record<string, ProviderBalance>>({});
  /** 正在查询额度的供应商 id 集合(驱动行内 spinner) */
  const balanceBusy = ref<Record<string, boolean>>({});
  const codex = ref<CodexStatus | null>(null);
  const codexProxy = ref<CodexProxyInfo | null>(null);
  const loading = ref(false);
  const switching = ref<string | null>(null);
  const error = ref<string | null>(null);

  // 浮层开关
  const showAddModal = ref(false);
  const addTarget = ref<ProviderView | null>(null); // 预填的预设/待编辑供应商;null = 空白新建
  const showUsageBoard = ref(false);

  const current = computed(
    () => providers.value.find((p) => p.id === currentId.value) ?? null
  );

  function openAdd(target: ProviderView | null = null) {
    addTarget.value = target;
    showAddModal.value = true;
  }
  function closeAdd() {
    showAddModal.value = false;
    addTarget.value = null;
  }
  function openUsage() {
    showUsageBoard.value = true;
    refreshUsage();
  }
  function closeUsage() {
    showUsageBoard.value = false;
  }

  async function refresh() {
    loading.value = true;
    try {
      const res = await providerApi.list();
      providers.value = res.providers;
      currentId.value = res.currentId || "claude-official";
      linkGlobal.value = !!res.linkGlobal;
    } catch (e) {
      error.value = String(e);
    } finally {
      loading.value = false;
    }
  }

  async function refreshUsage() {
    try {
      usage.value = await providerApi.usage();
    } catch (e) {
      error.value = String(e);
    }
  }

  /** 查询单个供应商的套餐额度 / 实时余额(失败也写回一个 error 结果, 便于 UI 展示) */
  async function refreshBalance(id: string): Promise<ProviderBalance | null> {
    if (!id) return null;
    balanceBusy.value = { ...balanceBusy.value, [id]: true };
    try {
      const b = await providerApi.balance(id);
      balances.value = { ...balances.value, [id]: b };
      return b;
    } catch (e) {
      const fail: ProviderBalance = {
        id,
        available: false,
        kind: "error",
        label: "查询失败",
        detail: String(e),
        consoleUrl: "",
      };
      balances.value = { ...balances.value, [id]: fail };
      return fail;
    } finally {
      const { [id]: _drop, ...rest } = balanceBusy.value;
      balanceBusy.value = rest;
    }
  }

  /** 批量查询所有「已配 key」供应商的额度(用量看板的套餐额度区用,串行避免一次性打满) */
  async function refreshConfiguredBalances() {
    const targets = providers.value.filter(
      (p) => p.hasKey && p.kind !== "codex" && p.kind !== "copilot"
    );
    for (const p of targets) {
      await refreshBalance(p.id);
    }
  }

  async function refreshCodex() {
    try {
      codex.value = await providerApi.codexStatus();
    } catch (e) {
      error.value = String(e);
    }
  }

  async function refreshCodexProxy() {
    try {
      codexProxy.value = await providerApi.codexProxyInfo();
    } catch (e) {
      error.value = String(e);
    }
  }

  /** ① 启动原生 Device Code 授权:后端会自动开浏览器,返回配对码供 UI 展示 */
  async function codexStartLogin(): Promise<CodexDeviceLogin | null> {
    error.value = null;
    try {
      return await providerApi.codexStartLogin();
    } catch (e) {
      error.value = String(e);
      return null;
    }
  }

  /** ② 轮询一次授权状态;成功(ok)时顺带刷新 codex 状态。抛错交给调用方处理 */
  async function codexPollLogin(
    deviceCode: string,
    userCode: string
  ): Promise<"pending" | "ok"> {
    const r = await providerApi.codexPollLogin(deviceCode, userCode);
    if (r.status === "ok") await refreshCodex();
    return r.status;
  }

  /** 切换「联动系统 CLI / 隔离」模式;开联动会把当前供应商写入全局 settings.json,
   *  关联动会把全局清回官方(终端 CLI 立刻恢复干净)、Polaris 内选择不变 */
  async function setLinkMode(link: boolean): Promise<boolean> {
    error.value = null;
    try {
      linkGlobal.value = await providerApi.setLinkMode(link);
      return true;
    } catch (e) {
      error.value = String(e);
      return false;
    }
  }

  /** 切换供应商；返回是否成功（失败时 error 已设置，常见为缺 key） */
  async function switchTo(id: string): Promise<boolean> {
    error.value = null;
    switching.value = id;
    try {
      await providerApi.switch(id);
      currentId.value = id;
      return true;
    } catch (e) {
      error.value = String(e);
      return false;
    } finally {
      switching.value = null;
    }
  }

  async function save(input: ProviderSaveInput): Promise<string | null> {
    error.value = null;
    try {
      const id = await providerApi.save(input);
      await refresh();
      return id;
    } catch (e) {
      error.value = String(e);
      return null;
    }
  }

  async function remove(id: string) {
    error.value = null;
    try {
      await providerApi.delete(id);
      await refresh();
    } catch (e) {
      error.value = String(e);
    }
  }

  return {
    providers,
    currentId,
    linkGlobal,
    usage,
    balances,
    balanceBusy,
    codex,
    codexProxy,
    loading,
    switching,
    error,
    showAddModal,
    addTarget,
    showUsageBoard,
    current,
    openAdd,
    closeAdd,
    openUsage,
    closeUsage,
    refresh,
    refreshUsage,
    refreshBalance,
    refreshConfiguredBalances,
    refreshCodex,
    refreshCodexProxy,
    codexStartLogin,
    codexPollLogin,
    setLinkMode,
    switchTo,
    save,
    remove,
  };
});
