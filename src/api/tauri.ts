/**
 * 对 Rust 后端 #[tauri::command] 的薄封装。
 * 统一错误处理：Rust 侧返回 Err(String)，这里抛出 Error 交给 store/UI 处理。
 */
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import type {
  AppConfig,
  JdkInfo,
  LogBatchEvent,
  LogEvent,
  ServiceInfo,
  ServiceMemory,
  ServiceRuntime,
  StartMode,
  TerminalConfig,
  TerminalInfo,
  TerminalShell,
} from '@/types';

/** 包装 invoke，统一把 Rust 返回的 Err(string) 转成 JS Error */
async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return invoke<T>(cmd, args);
}

/* ---------- JDK ---------- */

export const scanJdks = (): Promise<JdkInfo[]> => call('scan_jdks');

export const addJdk = (path: string): Promise<JdkInfo> =>
  call('add_jdk', { path });

export const removeJdk = (id: string): Promise<void> =>
  call('remove_jdk', { id });

/* ---------- 项目扫描 ---------- */

export const scanServices = (workspace: string): Promise<ServiceInfo[]> =>
  call('scan_services', { workspace });

/* ---------- Maven ---------- */

/** 扫描系统已安装的 Maven，返回 bin 目录路径列表 */
export const scanMvn = (): Promise<string[]> => call('scan_mvn');

/* ---------- 进程管理 ---------- */

export const startService = (
  serviceId: string,
  mode: StartMode,
): Promise<void> => call('start_service', { serviceId, mode });

export const stopService = (serviceId: string): Promise<void> =>
  call('stop_service', { serviceId });

export const restartService = (serviceId: string): Promise<void> =>
  call('restart_service', { serviceId });

export const startGroup = (groupName: string): Promise<void> =>
  call('start_group', { groupName });

export const startAll = (serviceIds: string[]): Promise<void> =>
  call('start_all', { serviceIds });

export const stopAll = (): Promise<void> => call('stop_all');

export const getServiceStatus = (): Promise<ServiceRuntime[]> =>
  call('get_service_status');

export const getServiceMemory = (): Promise<ServiceMemory[]> =>
  call('get_service_memory');

/**
 * 收编后台存量服务：把扫描出的服务与系统 java 进程表按启动类 / jar 路径匹配，
 * 命中的更新为运行中并接管状态管理（覆盖"关闭程序但服务仍在后台跑"的场景）。
 * 返回收编的服务 id 列表。
 */
export const adoptRunningServices = (
  workspace: string,
  services: ServiceInfo[],
): Promise<string[]> =>
  call('adopt_running_services', { workspace, services });

/* ---------- 配置 ---------- */

export const getConfig = (): Promise<AppConfig> => call('get_config');

export const saveConfig = (config: AppConfig): Promise<void> =>
  call('save_config', { config });

/** 设置某个工作目录的服务选中状态（后端用 config::mutate，不覆盖其他字段） */
export const setWorkspaceSelection = (
  workspace: string,
  serviceIds: string[],
): Promise<void> =>
  call('set_workspace_selection', { workspace, serviceIds });

/** 设置某个工作目录的置顶服务列表（后端用 config::mutate，不覆盖其他字段） */
export const setWorkspacePin = (
  workspace: string,
  serviceIds: string[],
): Promise<void> => call('set_workspace_pin', { workspace, serviceIds });

/** 设置某个工作目录的服务自定义排序（后端用 config::mutate，不覆盖其他字段） */
export const setWorkspaceOrder = (
  workspace: string,
  serviceIds: string[],
): Promise<void> => call('set_workspace_order', { workspace, serviceIds });

/* ---------- 事件订阅 ---------- */

/**
 * 订阅某个服务的实时日志。
 * @returns 取消订阅的函数
 */
export function onLog(
  serviceId: string,
  cb: (lines: LogEvent[]) => void,
): Promise<UnlistenFn> {
  return listen<LogBatchEvent>(`log:${serviceId}`, (e) => cb(e.payload.lines));
}

/** 订阅服务状态变更 */
export function onStatus(
  cb: (id: string, status: string, pid: number) => void,
): Promise<UnlistenFn> {
  return listen<{ id: string; status: string; pid: number }>(
    'service:status',
    (e) => cb(e.payload.id, e.payload.status, e.payload.pid),
  );
}

/* ---------- 控制台（内嵌终端） ---------- */

/** 创建并启动一个交互式终端，返回新 terminal id */
export const createTerminal = (
  cwd: string,
  shell?: TerminalShell,
  id?: string,
): Promise<string> => call('create_terminal', { cwd, shell, id });

/** 向终端写入按键数据 */
export const terminalWrite = (id: string, data: string): Promise<void> =>
  call('terminal_write', { id, data });

/** 调整终端尺寸（列 × 行） */
export const terminalResize = (
  id: string,
  cols: number,
  rows: number,
): Promise<void> => call('terminal_resize', { id, cols, rows });

/** 关闭终端（drop PTY，子进程随之退出） */
export const closeTerminal = (id: string): Promise<void> =>
  call('close_terminal', { id });

/** 当前所有活跃终端的运行态快照 */
export const listTerminals = (): Promise<TerminalInfo[]> =>
  call('list_terminals');

/** 持久化终端配置列表（局部 mutate，不覆盖其他字段） */
export const saveTerminals = (list: TerminalConfig[]): Promise<void> =>
  call('save_terminals', { list });

/** 订阅某个终端的实时输出（前端 xterm write） */
export function onTerminalData(
  terminalId: string,
  cb: (data: string) => void,
): Promise<UnlistenFn> {
  return listen<{ data: string }>(`terminal:data:${terminalId}`, (e) =>
    cb(e.payload.data),
  );
}

/** 订阅某个终端的退出事件 */
export function onTerminalExit(
  terminalId: string,
  cb: (code: number | null) => void,
): Promise<UnlistenFn> {
  return listen<{ code: number | null }>(`terminal:exit:${terminalId}`, (e) =>
    cb(e.payload.code),
  );
}

/* ---------- 文件夹选择对话框 ---------- */

export const pickDirectory = (): Promise<string | null> =>
  open({ directory: true, multiple: false });
