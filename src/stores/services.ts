/**
 * 服务 store —— 扫描结果 + 运行态 + 日志缓冲。
 * 日志按服务 id 分桶存放，设上限避免内存爆炸。
 */
import { defineStore } from 'pinia';
import { reactive, ref } from 'vue';
import * as api from '@/api/tauri';
import { useSettingsStore } from './settings';
import type {
  ServiceInfo,
  LogEvent,
  ServiceRuntime,
  ServiceStatus,
  StartMode,
} from '@/types';

/** 单个服务日志最多保留行数 */
const LOG_MAX_LINES = 2000;
/** 单行最大长度（截断超长堆栈） */
const LOG_MAX_LEN = 2000;

/** 前端日志缓冲中的单行数据。 */
export interface LogLine {
  id: number;
  text: string;
  error: boolean;
  ts: number;
}

let nextLogId = 0;

export const useServicesStore = defineStore('services', () => {
  const settings = useSettingsStore();

  /** 扫描到的服务静态信息（按 workspace -> id 索引）。
   *  与 runtimes 不同，infos 只保存"扫过的"工作目录的静态信息；
   *  当前工作目录 scan 后写入，其他历史目录由 scanWorkspace 按需写入。 */
  const infos = ref<Record<string, Record<string, ServiceInfo>>>({});
  /** 运行态（按 id 索引） */
  const runtimes = reactive<Record<string, ServiceRuntime>>({});
  /** 日志缓冲（按 id 索引） */
  const logs = reactive<Record<string, LogLine[]>>({});
  /** 内存占用（按 id 索引，字节）；仅运行中服务有值 */
  const memory = reactive<Record<string, number>>({});
  /** 全局操作进行中标记（禁用按钮） */
  const busy = ref(false);
  /** 全局错误/提示消息 */
  const message = ref<{ type: 'error' | 'success' | 'info'; text: string } | null>(
    null,
  );

  function setMessage(
    type: 'error' | 'success' | 'info',
    text: string,
    timeout = 4000,
  ) {
    message.value = { type, text };
    if (timeout > 0) {
      window.setTimeout(() => {
        if (message.value?.text === text) message.value = null;
      }, timeout);
    }
  }

  /** 批量追加日志，一次更新响应式数组并裁剪历史数据。 */
  function pushLogBatch(id: string, entries: LogEvent[]) {
    const timestamp = Date.now();
    const incoming = entries.map((entry) => ({
      id: nextLogId++,
      text:
        entry.line.length > LOG_MAX_LEN
          ? entry.line.slice(0, LOG_MAX_LEN) + '…'
          : entry.line,
      error: entry.is_error,
      ts: timestamp,
    }));
    logs[id] = [...(logs[id] ?? []), ...incoming].slice(-LOG_MAX_LINES);
  }

  function clearLog(id: string) {
    logs[id] = [];
  }

  /** 查某个服务在某工作目录下的静态信息（精确匹配）。 */
  function getInfo(id: string, workspace?: string): ServiceInfo | undefined {
    if (workspace && infos.value[workspace]?.[id]) {
      return infos.value[workspace][id];
    }
    // 兜底：遍历所有工作目录，命中第一个
    for (const ws of Object.keys(infos.value)) {
      if (infos.value[ws]?.[id]) return infos.value[ws][id];
    }
    return undefined;
  }

  /** 列表视图：当前工作目录的所有服务静态信息（infos + runtime 合并的旧入口）。
   *  Dashboard 仍按"当前目录"语义使用。 */
  function list(): ServiceInfo[] {
    const ws = infos.value[settings.workspace];
    return ws ? Object.values(ws) : [];
  }

  /** 列出所有有静态信息的工作目录（已扫过的）。 */
  function scannedWorkspaces(): string[] {
    return Object.keys(infos.value).filter(
      (ws) => Object.keys(infos.value[ws]).length > 0,
    );
  }

  /** 某工作目录下是否至少有一个服务处于运行/启动/停止中。 */
  function workspaceHasActive(ws: string): boolean {
    const map = infos.value[ws];
    if (!map) return false;
    return Object.keys(map).some((id) => {
      const st = runtimes[id]?.status;
      return st === 'running' || st === 'starting' || st === 'stopping';
    });
  }

  /** 其他工作目录中仍在运行的服务（不在当前扫描结果中但处于运行/启动/停止中状态）。
   *  现在每个 runtime 带 workspace，归属更准确。Dashboard 仍用此提示。 */
  function otherWorkspaceRunning(): ServiceRuntime[] {
    const cur = settings.workspace;
    return Object.values(runtimes).filter(
      (r) =>
        r.workspace !== cur &&
        (r.status === 'running' ||
          r.status === 'starting' ||
          r.status === 'stopping'),
    );
  }

  /** 扫描当前工作目录下的服务。结果写入 infos[当前workspace]，不清空其他工作目录缓存。 */
  async function scan() {
    if (!settings.workspace) {
      setMessage('error', '请先选择工作目录');
      return;
    }
    await scanWorkspace(settings.workspace, true);
  }

  /** 扫描指定工作目录并缓存其静态信息。
   *  showAll=false 时（默认，给「显示全部」用）：仅扫描并缓存，不动运行态、不弹消息。 */
  async function scanWorkspace(ws: string, announce = false) {
    if (!ws) {
      if (announce) setMessage('error', '请先选择工作目录');
      return;
    }
    if (announce) busy.value = true;
    try {
      const found = await api.scanServices(ws);
      const newMap: Record<string, ServiceInfo> = {};
      for (const s of found) newMap[s.id] = s;
      // 仅替换该工作目录的静态信息，保留其他工作目录缓存
      infos.value = { ...infos.value, [ws]: newMap };
      // 为新扫到的服务初始化默认运行态（保留已有运行态）
      for (const s of found) {
        if (!runtimes[s.id]) {
          runtimes[s.id] = {
            id: s.id,
            status: 'stopped',
            pid: 0,
            started_at: 0,
            jdk_id: '',
            workspace: ws,
          };
        }
      }
      // 收编后台存量服务：上次会话关闭前遗留（或外部启动）的进程，
      // 按启动类/jar 路径识别回来并更新为运行中。
      // 收编成功后立即同步一次快照，补全真实的 workspace / pid / started_at
      // （状态事件先到时这些字段还是占位值）。
      try {
        const adoptedIds = await api.adoptRunningServices(ws, found);
        if (adoptedIds.length > 0) {
          await syncStatus();
        }
      } catch {
        // 收编失败不影响扫描流程（仅状态显示回退为未启动）
      }
      if (announce) {
        // 清理已不在当前工作目录且未运行的服务运行态记录
        // 保留运行中的服务（可能来自其他工作目录，切换后维持现状）
        const newIds = new Set(found.map((s) => s.id));
        for (const id of Object.keys(runtimes)) {
          if (!newIds.has(id)) {
            const rt = runtimes[id];
            if (rt.workspace === ws || !rt.workspace) {
              if (rt.status === 'stopped' || rt.status === 'crashed') {
                delete runtimes[id];
                delete logs[id];
              }
            }
          }
        }
        // 同步后端真实运行态（包含其他工作目录仍运行的服务）
        await syncStatus();
        setMessage('success', `扫描到 ${found.length} 个可运行服务`);
      }
    } catch (e) {
      if (announce) setMessage('error', `扫描失败：${e}`);
    } finally {
      if (announce) busy.value = false;
    }
  }

  /** 从后端同步一次真实运行态（启动/应用加载时调用） */
  async function syncStatus() {
    const list = await api.getServiceStatus();
    for (const r of list) runtimes[r.id] = r;
  }

  /** 从后端刷新各运行中服务的内存占用。无运行中服务时清空。 */
  async function refreshMemory() {
    const hasRunning = Object.values(runtimes).some(
      (r) => r.status === 'running' || r.status === 'starting',
    );
    if (!hasRunning) {
      for (const k of Object.keys(memory)) delete memory[k];
      return;
    }
    try {
      const list = await api.getServiceMemory();
      for (const k of Object.keys(memory)) delete memory[k];
      for (const m of list) memory[m.id] = m.memory_bytes;
    } catch {
      // 内存查询失败时静默忽略，保留旧值
    }
  }

  /** 本地更新状态（响应事件，不等后端） */
  function applyStatus(id: string, status: ServiceStatus, pid: number) {
    const prev = runtimes[id]?.status;
    const r = runtimes[id];
    if (r) {
      r.status = status;
      r.pid = pid;
      r.started_at =
        status === 'running' || status === 'starting'
          ? r.started_at || Date.now()
          : 0;
    } else {
      runtimes[id] = {
        id,
        status,
        pid,
        started_at:
          status === 'running' || status === 'starting' ? Date.now() : 0,
        jdk_id: '',
        // workspace 由后端 syncStatus 补全；事件先到时暂留空串，
        // 后续 syncStatus 会用真实值覆盖。
        workspace: '',
      };
    }
    // 状态变为 crashed 时弹错误提示（启动失败/异常退出）
    if (status === 'crashed' && prev !== 'crashed') {
      setMessage('error', `${id} 启动失败或异常退出，请查看日志`);
    }
    // 终态清理：不属于当前工作目录且已停止/崩溃的服务，
    // 从运行态记录中移除，使其从对应工作目录分组里消失。
    // （当前工作目录的服务保留记录，列表里仍要显示为已停止。）
    if (status === 'stopped' || status === 'crashed') {
      const rtWs = runtimes[id]?.workspace ?? '';
      const inCurrent = rtWs === settings.workspace || !getInfo(id);
      if (!inCurrent) {
        delete runtimes[id];
        delete logs[id];
      }
    }
  }

  function resolveMode(id: string): StartMode {
    // 按工作目录分组的服务配置：services[workspace][id].mode
    const wsServices = settings.services[settings.workspace];
    return wsServices?.[id]?.mode ?? 'mvn';
  }

  async function start(id: string) {
    busy.value = true;
    try {
      await api.startService(id, resolveMode(id));
      applyStatus(id, 'starting', runtimes[id]?.pid ?? 0);
      setMessage('info', `正在启动 ${id}…`);
    } catch (e) {
      setMessage('error', `启动失败：${e}`);
    } finally {
      busy.value = false;
    }
  }

  async function stop(id: string) {
    busy.value = true;
    // 乐观更新：必须在 await 之前标记 stopping。
    // 后端 stop() 会同步 emit Stopped 事件（stop 进程后立即广播），
    // 若在 await 之后才设 stopping，会把已到达的 Stopped 状态覆盖回去，
    // 导致状态卡在 stopping，"其他工作目录运行中"列表里的条目不消失。
    applyStatus(id, 'stopping', runtimes[id]?.pid ?? 0);
    try {
      await api.stopService(id);
      setMessage('info', `正在停止 ${id}…`);
    } catch (e) {
      setMessage('error', `停止失败：${e}`);
    } finally {
      busy.value = false;
    }
  }

  async function restart(id: string) {
    busy.value = true;
    try {
      await api.restartService(id);
      setMessage('info', `正在重启 ${id}…`);
    } catch (e) {
      setMessage('error', `重启失败：${e}`);
    } finally {
      busy.value = false;
    }
  }

  async function startByGroup(groupName: string) {
    busy.value = true;
    try {
      await api.startGroup(groupName);
      setMessage('info', `分组 [${groupName}] 开始按顺序启动`);
    } catch (e) {
      setMessage('error', `分组启动失败：${e}`);
    } finally {
      busy.value = false;
    }
  }

  async function startSelected(ids: string[]) {
    busy.value = true;
    try {
      await api.startAll(ids);
      setMessage('info', `开始启动 ${ids.length} 个服务`);
    } catch (e) {
      setMessage('error', `启动失败：${e}`);
    } finally {
      busy.value = false;
    }
  }

  async function stopAll() {
    busy.value = true;
    try {
      await api.stopAll();
      setMessage('info', '正在停止所有服务…');
    } catch (e) {
      setMessage('error', `停止失败：${e}`);
    } finally {
      busy.value = false;
    }
  }

  /** 停止某个工作目录下所有处于运行/启动/停止中的服务。
   *  依据每个 runtime 记录的 workspace 归属过滤，不动其他工作目录的服务。 */
  async function stopWorkspace(ws: string) {
    const ids = Object.values(runtimes)
      .filter(
        (r) =>
          r.workspace === ws &&
          (r.status === 'running' ||
            r.status === 'starting' ||
            r.status === 'stopping'),
      )
      .map((r) => r.id);
    if (ids.length === 0) return;
    busy.value = true;
    try {
      await Promise.all(ids.map((id) => api.stopService(id)));
      setMessage('info', `正在停止 ${ws} 下 ${ids.length} 个服务…`);
    } catch (e) {
      setMessage('error', `停止失败：${e}`);
    } finally {
      busy.value = false;
    }
  }

  return {
    infos,
    runtimes,
    logs,
    memory,
    busy,
    message,
    list,
    getInfo,
    scannedWorkspaces,
    workspaceHasActive,
    otherWorkspaceRunning,
    scan,
    scanWorkspace,
    syncStatus,
    refreshMemory,
    applyStatus,
    pushLogBatch,
    clearLog,
    start,
    stop,
    restart,
    startByGroup,
    startSelected,
    stopAll,
    stopWorkspace,
    setMessage,
  };
});
