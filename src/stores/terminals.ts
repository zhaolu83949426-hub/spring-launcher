/**
 * 终端 store —— 内嵌控制台会话的配置列表 + 运行态。
 *
 * 持久化分工：
 * - 配置列表（name/cwd/shell）随 config.json 保存，由本 store 调 saveTerminals（后端 config::mutate 局部更新）
 * - 运行态（running/pid）不持久化：PTY 子进程无法跨应用重启存活，重启后仅恢复列表
 *
 * 视图层（Console.vue）负责 xterm 实例与 IPC 订阅；本 store 只管配置元数据与 running 标记。
 */
import { defineStore } from 'pinia';
import { computed, ref } from 'vue';
import * as api from '@/api/tauri';
import type { TerminalConfig, TerminalInfo, TerminalShell } from '@/types';

export const useTerminalsStore = defineStore('terminals', () => {
  /** 持久化的终端配置列表 */
  const configs = ref<TerminalConfig[]>([]);
  /** 运行态快照：terminal_id -> TerminalInfo */
  const infos = ref<Record<string, TerminalInfo>>({});
  /** 当前激活的终端 id */
  const activeId = ref<string>('');

  /** 运行中的终端数（侧边栏徽章用） */
  const runningCount = computed(
    () => Object.values(infos.value).filter((i) => i.running).length,
  );

  /** 从后端加载配置列表 + 同步运行态 */
  async function load() {
    const cfg = await api.getConfig();
    configs.value = (cfg.terminals || []).map(normalizeConfig);
    await syncInfo();
  }

  /** 同步运行态快照（把后端 list_terminals 合并进 infos） */
  async function syncInfo() {
    const list = await api.listTerminals();
    const next: Record<string, TerminalInfo> = {};
    // 先标记所有已知 id 为未运行
    for (const c of configs.value) {
      next[c.id] = {
        id: c.id,
        name: c.name,
        cwd: c.cwd,
        shell: c.shell,
        running: false,
        pid: 0,
        started_at: 0,
      };
    }
    // 用后端实际运行态覆盖
    for (const info of list) {
      next[info.id] = info;
    }
    infos.value = next;
  }

  /**
   * 创建并启动一个终端会话。
   * @param cwd 工作目录
   * @param shell Shell 选择
   * @param name 终端名（可空，空则取默认）
   * @returns 新 terminal id
   */
  async function create(
    cwd: string,
    shell: TerminalShell = 'auto',
    name = '',
  ): Promise<string> {
    const id = await api.createTerminal(cwd, shell);
    const config: TerminalConfig = { id, name: name.trim(), cwd, shell };
    configs.value = [...configs.value, config];
    await persistConfigs();
    infos.value = {
      ...infos.value,
      [id]: {
        id,
        name: config.name,
        cwd,
        shell,
        running: true,
        pid: 0,
        started_at: Date.now(),
      },
    };
    activeId.value = id;
    return id;
  }

  /** 已有配置重新打开（重启应用后恢复用） */
  async function openExisting(id: string): Promise<void> {
    const cfg = configs.value.find((c) => c.id === id);
    if (!cfg) return;
    // 后端用原 id 重建 PTY
    await api.createTerminal(cfg.cwd, cfg.shell, id);
    infos.value = {
      ...infos.value,
      [id]: {
        ...cfg,
        running: true,
        pid: 0,
        started_at: Date.now(),
      },
    };
    activeId.value = id;
  }

  /** 关闭终端（停止 PTY，保留配置） */
  async function close(id: string) {
    await api.closeTerminal(id);
    if (infos.value[id]) {
      infos.value = {
        ...infos.value,
        [id]: { ...infos.value[id], running: false, pid: 0, started_at: 0 },
      };
    }
  }

  /** 标记某终端已退出（读线程 EOF 时由视图层回调） */
  function markExited(id: string) {
    if (infos.value[id]) {
      infos.value = {
        ...infos.value,
        [id]: { ...infos.value[id], running: false, pid: 0 },
      };
    }
  }

  /** 重启终端：先关再开（保留原配置与 id） */
  async function restart(id: string) {
    await close(id);
    await openExisting(id);
  }

  /** 删除某终端配置（同时关闭运行中的 PTY） */
  async function removeConfig(id: string) {
    if (infos.value[id]?.running) {
      await api.closeTerminal(id);
    }
    configs.value = configs.value.filter((c) => c.id !== id);
    const nextInfos = { ...infos.value };
    delete nextInfos[id];
    infos.value = nextInfos;
    await persistConfigs();
    if (activeId.value === id) {
      activeId.value = configs.value[0]?.id ?? '';
    }
  }

  /** 重命名 */
  async function rename(id: string, name: string) {
    configs.value = configs.value.map((c) =>
      c.id === id ? { ...c, name: name.trim() } : c,
    );
    if (infos.value[id]) {
      infos.value = {
        ...infos.value,
        [id]: { ...infos.value[id], name: name.trim() },
      };
    }
    await persistConfigs();
  }

  /** 更新工作目录 / Shell（需重启终端生效） */
  async function updateMeta(
    id: string,
    meta: { cwd?: string; shell?: TerminalShell },
  ) {
    configs.value = configs.value.map((c) =>
      c.id === id
        ? {
            ...c,
            cwd: meta.cwd ?? c.cwd,
            shell: meta.shell ?? c.shell,
          }
        : c,
    );
    if (infos.value[id]) {
      infos.value = {
        ...infos.value,
        [id]: {
          ...infos.value[id],
          cwd: meta.cwd ?? infos.value[id].cwd,
          shell: meta.shell ?? infos.value[id].shell,
        },
      };
    }
    await persistConfigs();
  }

  /** 持久化配置列表（局部 mutate，不影响其他字段） */
  async function persistConfigs() {
    await api.saveTerminals(configs.value);
  }

  /** 取某个终端的显示名（空则「终端 N」） */
  function displayName(id: string, index: number): string {
    const cfg = configs.value.find((c) => c.id === id);
    return cfg?.name?.trim() || `终端 ${index + 1}`;
  }

  return {
    configs,
    infos,
    activeId,
    runningCount,
    load,
    syncInfo,
    create,
    openExisting,
    close,
    markExited,
    restart,
    removeConfig,
    rename,
    updateMeta,
    displayName,
  };
});

/** 规范化配置（兼容老数据缺字段） */
function normalizeConfig(c: TerminalConfig): TerminalConfig {
  return {
    id: c.id,
    name: c.name ?? '',
    cwd: c.cwd ?? '',
    shell: c.shell ?? 'auto',
  };
}
