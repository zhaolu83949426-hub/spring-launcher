/**
 * 全局配置 store —— 工作目录、JDK 列表、默认 JDK、mvn 路径、启动分组。
 * 持久化由 Rust 侧 config.rs 负责，前端只持有内存副本。
 */
import { defineStore } from 'pinia';
import { ref } from 'vue';
import * as api from '@/api/tauri';
import type {
  AppConfig,
  HeapRule,
  ServiceConfig,
  StartGroup,
  TerminalConfig,
} from '@/types';

export const useSettingsStore = defineStore('settings', () => {
  const workspace = ref('');
  const workspaces = ref<string[]>([]);
  const mvnPath = ref('mvn');
  const defaultJdkId = ref('');
  const jdks = ref<import('@/types').JdkInfo[]>([]);
  /** 工作目录级默认 JDK：workspace 路径 -> JDK id */
  const workspaceJdks = ref<Record<string, string>>({});
  /** 工作目录级服务选中状态：workspace 路径 -> 选中的服务 id 列表 */
  const workspaceSelections = ref<Record<string, string[]>>({});
  /** 工作目录级置顶服务：workspace 路径 -> 置顶的服务 id 列表 */
  const workspacePins = ref<Record<string, string[]>>({});
  /** 工作目录级自定义排序：workspace 路径 -> 服务 id 顺序列表 */
  const workspaceOrders = ref<Record<string, string[]>>({});
  const groups = ref<StartGroup[]>([]);
  /** 按工作目录分组的每服务配置：workspace 路径 -> service_id -> ServiceConfig */
  const services = ref<Record<string, Record<string, ServiceConfig>>>({});
  /** 堆内存默认规则列表（扫描新服务时按 artifactId 关键词匹配套用） */
  const heapRules = ref<HeapRule[]>([]);
  /** 是否启用默认规则自动套用 */
  const heapRulesEnabled = ref(true);
  /** 内嵌控制台会话配置列表（持久化；运行态由 terminals store 管理） */
  const terminals = ref<TerminalConfig[]>([]);
  /** 是否已从后端加载过配置 */
  const loaded = ref(false);

  /** 默认分组（首次使用时预设，贴合 csgs-backend 启动链） */
  function defaultGroups(): StartGroup[] {
    return [
      {
        name: '1. 注册中心',
        services: ['cip-nacos-app'],
        delay_sec: 30,
      },
      {
        name: '2. 基础设施',
        services: [
          'cip-actuator-app',
          'cip-sentinel-app',
          'cip-xxl-job-app',
        ],
        delay_sec: 15,
      },
      {
        name: '3. 框架应用',
        services: ['cip-gateway-app', 'cip-web-app', 'cip-biz-app'],
        delay_sec: 15,
      },
      {
        name: '4. 业务服务',
        services: [
          'csgs-backend-customer-app',
          'csgs-backend-merchant-app',
          'csgs-backend-portal-app',
        ],
        delay_sec: 10,
      },
    ];
  }

  /** 从后端加载配置 */
  async function load() {
    const cfg = await api.getConfig();
    workspace.value = cfg.workspace;
    workspaces.value = cfg.workspaces || [];
    mvnPath.value = cfg.mvn_path || 'mvn';
    defaultJdkId.value = cfg.default_jdk_id;
    jdks.value = cfg.jdks || [];
    workspaceJdks.value = cfg.workspace_jdks || {};
    workspaceSelections.value = cfg.workspace_selections || {};
    workspacePins.value = cfg.workspace_pins || {};
    workspaceOrders.value = cfg.workspace_orders || {};
    groups.value =
      cfg.groups && cfg.groups.length > 0 ? cfg.groups : defaultGroups();
    services.value = cfg.services || {};
    heapRules.value = cfg.heap_rules || [];
    heapRulesEnabled.value = cfg.heap_rules_enabled ?? true;
    terminals.value = cfg.terminals || [];
    loaded.value = true;
  }

  /** 把当前内存状态序列化回后端持久化 */
  async function persist() {
    const cfg: AppConfig = {
      workspace: workspace.value,
      workspaces: workspaces.value,
      default_jdk_id: defaultJdkId.value,
      workspace_jdks: workspaceJdks.value,
      workspace_selections: workspaceSelections.value,
      workspace_pins: workspacePins.value,
      workspace_orders: workspaceOrders.value,
      mvn_path: mvnPath.value,
      jdks: jdks.value,
      services: services.value, // 按工作目录分组的服务配置
      groups: groups.value,
      heap_rules: heapRules.value,
      heap_rules_enabled: heapRulesEnabled.value,
      terminals: terminals.value,
    };
    await api.saveConfig(cfg);
  }

  /** 设置工作目录并加入历史列表 */
  async function setWorkspace(path: string) {
    workspace.value = path;
    // 加入历史列表（去重，移到末尾表示最近使用）
    workspaces.value = workspaces.value.filter((w) => w !== path);
    workspaces.value.push(path);
    await persist();
  }

  /** 切换到历史工作目录（不停止旧工作目录的服务） */
  async function switchWorkspace(path: string) {
    workspace.value = path;
    // 移到末尾表示最近使用
    workspaces.value = workspaces.value.filter((w) => w !== path);
    workspaces.value.push(path);
    await persist();
  }

  /** 删除历史工作目录记录（不停止该目录下正在运行的服务） */
  async function removeWorkspace(path: string) {
    workspaces.value = workspaces.value.filter((w) => w !== path);
    // 清理该工作目录的 JDK 映射
    const next = { ...workspaceJdks.value };
    delete next[path];
    workspaceJdks.value = next;
    // 清理该工作目录的服务配置
    const nextSvc = { ...services.value };
    delete nextSvc[path];
    services.value = nextSvc;
    // 清理该工作目录的选中状态
    const nextSel = { ...workspaceSelections.value };
    delete nextSel[path];
    workspaceSelections.value = nextSel;
    // 清理该工作目录的置顶状态
    const nextPins = { ...workspacePins.value };
    delete nextPins[path];
    workspacePins.value = nextPins;
    // 清理该工作目录的自定义排序
    const nextOrder = { ...workspaceOrders.value };
    delete nextOrder[path];
    workspaceOrders.value = nextOrder;
    await persist();
  }

  async function setMvnPath(path: string) {
    mvnPath.value = path;
    await persist();
  }

  async function setDefaultJdk(id: string) {
    defaultJdkId.value = id;
    await persist();
  }

  /** 设置某个工作目录的默认 JDK（空串表示清除，回退到全局默认） */
  async function setWorkspaceJdk(wsPath: string, jdkId: string) {
    const next = { ...workspaceJdks.value };
    if (jdkId) {
      next[wsPath] = jdkId;
    } else {
      delete next[wsPath];
    }
    workspaceJdks.value = next;
    await persist();
  }

  /** 获取当前工作目录的默认 JDK id（空串表示未配置，回退到全局） */
  function currentWorkspaceJdkId(): string {
    return workspaceJdks.value[workspace.value] || '';
  }

  /** 获取某个工作目录的服务选中列表（空数组表示无选中） */
  function getSelection(wsPath: string): string[] {
    return workspaceSelections.value[wsPath] || [];
  }

  /** 设置某个工作目录的服务选中状态（通过后端 config::mutate，不覆盖其他字段） */
  async function setSelection(wsPath: string, ids: string[]) {
    await api.setWorkspaceSelection(wsPath, ids);
    const next = { ...workspaceSelections.value };
    if (ids.length > 0) {
      next[wsPath] = ids;
    } else {
      delete next[wsPath];
    }
    workspaceSelections.value = next;
  }

  /** 获取某个工作目录的置顶服务 id 集合（无置顶返回空集合） */
  function getPins(wsPath: string): string[] {
    return workspacePins.value[wsPath] || [];
  }

  /** 切换某服务的置顶状态（已置顶则取消，未置顶则置顶） */
  async function togglePin(wsPath: string, serviceId: string) {
    const current = new Set(getPins(wsPath));
    if (current.has(serviceId)) {
      current.delete(serviceId);
    } else {
      current.add(serviceId);
    }
    const ids = Array.from(current);
    await api.setWorkspacePin(wsPath, ids);
    const next = { ...workspacePins.value };
    if (ids.length > 0) {
      next[wsPath] = ids;
    } else {
      delete next[wsPath];
    }
    workspacePins.value = next;
  }

  /** 获取某个工作目录的服务自定义排序（空数组表示未自定义，按扫描顺序） */
  function getOrder(wsPath: string): string[] {
    return workspaceOrders.value[wsPath] || [];
  }

  /** 保存某个工作目录的服务自定义排序（后端用 config::mutate，不覆盖其他字段） */
  async function setOrder(wsPath: string, ids: string[]) {
    await api.setWorkspaceOrder(wsPath, ids);
    const next = { ...workspaceOrders.value };
    if (ids.length > 0) {
      next[wsPath] = ids;
    } else {
      delete next[wsPath];
    }
    workspaceOrders.value = next;
  }

  /** 获取某个工作目录的服务配置映射（空对象表示无配置） */
  function getWorkspaceServices(wsPath: string): Record<string, ServiceConfig> {
    return services.value[wsPath] || {};
  }

  /** 获取某个工作目录下某个服务的配置（不存在返回 undefined） */
  function getServiceConfig(
    wsPath: string,
    serviceId: string,
  ): ServiceConfig | undefined {
    return services.value[wsPath]?.[serviceId];
  }

  /** 设置某个工作目录下某个服务的配置（同时持久化） */
  async function setServiceConfig(
    wsPath: string,
    serviceId: string,
    config: ServiceConfig,
  ) {
    if (!services.value[wsPath]) {
      services.value[wsPath] = {};
    }
    services.value[wsPath][serviceId] = config;
    await persist();
  }

  /** 批量替换某个工作目录下的全部服务配置（同时持久化） */
  async function setWorkspaceServices(
    wsPath: string,
    configs: Record<string, ServiceConfig>,
  ) {
    const next = { ...services.value };
    if (Object.keys(configs).length === 0) {
      delete next[wsPath];
    } else {
      next[wsPath] = configs;
    }
    services.value = next;
    await persist();
  }

  /** 删除某个工作目录的全部服务配置（同时持久化） */
  async function removeWorkspaceServices(wsPath: string) {
    const next = { ...services.value };
    delete next[wsPath];
    services.value = next;
    await persist();
  }

  /* ================ 堆内存默认规则管理 ================ */

  /** 切换规则启用状态 */
  async function setHeapRulesEnabled(enabled: boolean) {
    heapRulesEnabled.value = enabled;
    await persist();
  }

  /** 新增一条规则（返回新规则，便于 UI 定位） */
  async function addHeapRule(rule: HeapRule) {
    // 兜底规则（keywords 为空）必须排在最后：插到末尾前
    const hasFallback = heapRules.value.some((r) => !r.keywords);
    if (hasFallback) {
      const idx = heapRules.value.findIndex((r) => !r.keywords);
      heapRules.value.splice(idx, 0, rule);
    } else {
      heapRules.value.push(rule);
    }
    await persist();
  }

  /** 更新一条规则（按 id 匹配） */
  async function updateHeapRule(rule: HeapRule) {
    const idx = heapRules.value.findIndex((r) => r.id === rule.id);
    if (idx >= 0) {
      heapRules.value[idx] = { ...rule };
      await persist();
    }
  }

  /** 删除一条规则（内置规则不可删） */
  async function removeHeapRule(id: string) {
    const r = heapRules.value.find((x) => x.id === id);
    if (r?.builtin) return;
    heapRules.value = heapRules.value.filter((x) => x.id !== id);
    await persist();
  }

  /** 上移/下移规则（调整匹配优先级） */
  async function moveHeapRule(id: string, dir: -1 | 1) {
    const idx = heapRules.value.findIndex((r) => r.id === id);
    if (idx < 0) return;
    const target = idx + dir;
    if (target < 0 || target >= heapRules.value.length) return;
    // 不允许越过兜底规则（保持兜底在最后）
    const arr = [...heapRules.value];
    [arr[idx], arr[target]] = [arr[target], arr[idx]];
    heapRules.value = arr;
    await persist();
  }

  /**
   * 按 artifactId 匹配第一条命中的规则。
   * keywords 为空（兜底规则）永远命中；按列表顺序匹配。
   */
  function matchHeapRule(artifactId: string): HeapRule | undefined {
    const id = artifactId.toLowerCase();
    for (const r of heapRules.value) {
      if (!r.keywords) return r; // 兜底
      const kws = r.keywords
        .split(',')
        .map((k) => k.trim().toLowerCase())
        .filter(Boolean);
      if (kws.some((k) => id.includes(k))) return r;
    }
    return undefined;
  }

  return {
    workspace,
    workspaces,
    mvnPath,
    defaultJdkId,
    jdks,
    workspaceJdks,
    workspaceSelections,
    workspacePins,
    workspaceOrders,
    groups,
    services,
    heapRules,
    heapRulesEnabled,
    terminals,
    loaded,
    load,
    persist,
    setWorkspace,
    switchWorkspace,
    removeWorkspace,
    setMvnPath,
    setDefaultJdk,
    setWorkspaceJdk,
    currentWorkspaceJdkId,
    getSelection,
    setSelection,
    getPins,
    togglePin,
    getOrder,
    setOrder,
    getWorkspaceServices,
    getServiceConfig,
    setServiceConfig,
    setWorkspaceServices,
    removeWorkspaceServices,
    setHeapRulesEnabled,
    addHeapRule,
    updateHeapRule,
    removeHeapRule,
    moveHeapRule,
    matchHeapRule,
  };
});
