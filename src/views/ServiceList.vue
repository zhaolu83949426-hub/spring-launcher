<script setup lang="ts">
/** 服务列表：按工作目录分组，表格形式展示，每行可启动/停止/重启/查看日志。
 * 默认展示所有工作目录中运行中/启动中/停止中的服务；打开「显示全部」可查看并启动
 * 所有历史工作目录的已停止服务（首次勾选时按需扫描各历史目录）。 */
import { computed, ref, watch } from 'vue';
import { Play, Square, RotateCw, FileText } from 'lucide-vue-next';
import { useServicesStore } from '@/stores/services';
import { useJdkStore } from '@/stores/jdk';
import { useSettingsStore } from '@/stores/settings';
import StatusBadge from '@/components/StatusBadge.vue';
import type { ServiceInfo, ServiceStatus } from '@/types';

const services = useServicesStore();
const jdk = useJdkStore();
const settings = useSettingsStore();

const tab = defineModel<TabType>('tab');
type TabType = 'dashboard' | 'services' | 'logs' | 'jdk' | 'settings';

/** 是否展示全部服务（含所有历史工作目录的已停止服务）。默认 false：只看运行中。 */
const showAll = ref(false);

/** 历史工作目录中尚未扫过的目录（用于「显示全部」时按需扫描）。 */
const unscannedHistory = computed(() =>
  settings.workspaces.filter(
    (ws) => !services.infos[ws] || Object.keys(services.infos[ws]).length === 0,
  ),
);

/** 勾选「显示全部」时，按需扫描尚未缓存的历史工作目录。 */
watch(
  [showAll, unscannedHistory],
  async ([on, pending]) => {
    if (!on || pending.length === 0) return;
    // 并行扫描所有未缓存的历史目录（扫描结果会写入 services.infos）
    await Promise.all(pending.map((ws) => services.scanWorkspace(ws)));
  },
  { immediate: true },
);

/** 全局运行中计数（含所有工作目录） */
const globalRunningCount = computed(
  () =>
    Object.values(services.runtimes).filter(
      (r) => r.status === 'running',
    ).length,
);

interface ServiceRow {
  info: ServiceInfo;
  status: ServiceStatus;
  pid: number;
  jdkVersion: string;
  uptime: string;
}

interface WorkspaceGroup {
  workspace: string;
  isCurrent: boolean;
  rows: ServiceRow[];
}

/** 组装单个工作目录下要展示的行。
 *  includeStopped：是否含已停止/已崩溃的服务（「显示全部」或当前目录默认含已停止）。 */
function buildRows(ws: string, includeStopped: boolean): ServiceRow[] {
  const map = services.infos[ws] ?? {};
  // 静态信息为主，补上该目录下有运行态但无静态信息的孤儿服务（兜底）
  const ids = new Set<string>(Object.keys(map));
  for (const id of Object.keys(services.runtimes)) {
    if (services.runtimes[id]?.workspace === ws) ids.add(id);
  }
  const rows: ServiceRow[] = [];
  for (const id of ids) {
    const info = map[id] ?? services.getInfo(id, ws);
    if (!info) continue;
    const rt = services.runtimes[id];
    const status = rt?.status ?? 'stopped';
    if (!includeStopped) {
      // 默认只展示运行中/启动中/停止中
      if (status !== 'running' && status !== 'starting' && status !== 'stopping')
        continue;
    }
    const jdkInfo = rt ? jdk.findById(rt.jdk_id) : undefined;
    rows.push({
      info,
      status,
      pid: rt?.pid ?? 0,
      jdkVersion: jdkInfo?.version ?? '-',
      uptime: rt?.started_at ? formatUptime(Date.now() - rt.started_at) : '-',
    });
  }
  return rows;
}

/** 分组后的列表：每个工作目录一组，含标题信息 + 行。 */
const groupedRows = computed<WorkspaceGroup[]>(() => {
  const groups: WorkspaceGroup[] = [];
  const seen = new Set<string>();
  const curWs = settings.workspace;

  if (showAll.value) {
    // 显示全部：所有历史工作目录，当前目录排第一
    const ordered = [
      curWs,
      ...settings.workspaces.filter((ws) => ws !== curWs),
    ].filter((ws, i, arr) => ws && arr.indexOf(ws) === i);
    for (const ws of ordered) {
      if (seen.has(ws)) continue;
      seen.add(ws);
      const rows = buildRows(ws, true);
      if (rows.length === 0) continue;
      groups.push({ workspace: ws, isCurrent: ws === curWs, rows });
    }
    return groups;
  }

  // 默认：所有目录都只展示运行中/启动中/停止中，已停止/已崩溃不展示。
  // 1) 当前目录
  if (curWs && (services.infos[curWs] || services.workspaceHasActive(curWs))) {
    seen.add(curWs);
    const rows = buildRows(curWs, false);
    if (rows.length > 0)
      groups.push({ workspace: curWs, isCurrent: true, rows });
  }
  // 2) 其他目录
  const otherWs = new Set<string>();
  for (const r of Object.values(services.runtimes)) {
    if (r.workspace && !seen.has(r.workspace)) otherWs.add(r.workspace);
  }
  for (const ws of otherWs) {
    const rows = buildRows(ws, false);
    if (rows.length === 0) continue;
    groups.push({ workspace: ws, isCurrent: false, rows });
  }
  return groups;
});

const isEmpty = computed(
  () => groupedRows.value.length === 0 || groupedRows.value.every((g) => g.rows.length === 0),
);

function formatUptime(ms: number): string {
  const sec = Math.floor(ms / 1000);
  if (sec < 60) return `${sec}s`;
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}m${sec % 60}s`;
  const hr = Math.floor(min / 60);
  return `${hr}h${min % 60}m`;
}

function isRunning(id: string) {
  return services.runtimes[id]?.status === 'running';
}

/** 组内运行中计数（用于标题展示） */
function runningCountIn(rows: ServiceRow[]): number {
  return rows.filter((r) => r.status === 'running').length;
}

/** 取工作目录的短名（最后一段），便于展示；完整路径放 title。 */
function workspaceShort(ws: string): string {
  const parts = ws.replace(/\\/g, '/').split('/').filter(Boolean);
  return parts[parts.length - 1] || ws;
}

async function viewLog(id: string) {
  // 把目标服务 id 存到 window 上，LogViewer 读取
  (window as unknown as { __active_log_id?: string }).__active_log_id = id;
  tab.value = 'logs';
}

async function stopAll() {
  await services.stopAll();
}
</script>

<template>
  <div class="p-5">
    <!-- 顶部工具条：运行中计数 + 显示全部开关 + 停止全部 -->
    <div class="mb-3 flex items-center justify-between">
      <div class="flex items-center gap-3 text-xs text-slate-500">
        <span>运行中 {{ globalRunningCount }} 个服务</span>
      </div>
      <div class="flex items-center gap-3">
        <label class="flex cursor-pointer items-center gap-2 text-xs text-slate-400">
          <input
            type="checkbox"
            class="accent-indigo-500"
            v-model="showAll"
          />
          显示全部
        </label>
        <button
          class="flex items-center gap-1.5 rounded-md bg-red-500/80 px-3 py-1.5 text-xs font-medium text-white hover:bg-red-600 disabled:opacity-40"
          :disabled="globalRunningCount === 0 || services.busy"
          @click="stopAll"
        >
          <Square class="h-3.5 w-3.5" /> 停止全部服务
        </button>
      </div>
    </div>

    <div
      v-if="isEmpty"
      class="rounded-xl border border-dashed border-slate-700 p-12 text-center text-slate-500"
    >
      <template v-if="showAll">
        暂无服务，请先在「总览」页选择工作目录并扫描
      </template>
      <template v-else>
        当前没有运行中的服务，勾选上方「显示全部」可查看并启动已停止的服务
      </template>
    </div>

    <!-- 按工作目录分组展示 -->
    <div v-else class="space-y-6">
      <div v-for="group in groupedRows" :key="group.workspace">
        <!-- 分组标题 -->
        <div class="mb-2 flex items-center gap-2">
          <h3
            class="text-sm font-medium text-slate-200"
            :title="group.workspace"
          >
            {{ workspaceShort(group.workspace) }}
          </h3>
          <span
            v-if="group.isCurrent"
            class="rounded bg-indigo-500/20 px-1.5 py-0.5 text-[10px] font-medium text-indigo-300"
          >
            当前
          </span>
          <span class="text-[11px] text-slate-500">
            运行中 {{ runningCountIn(group.rows) }} / {{ group.rows.length }}
          </span>
          <span class="ml-auto hidden truncate font-mono text-[10px] text-slate-600 sm:block">
            {{ group.workspace }}
          </span>
        </div>

        <!-- 该组服务表格 -->
        <div
          class="overflow-hidden rounded-xl border border-slate-800 bg-slate-900/50"
        >
          <table class="w-full text-sm">
            <thead>
              <tr class="border-b border-slate-800 text-left text-xs text-slate-500">
                <th class="px-4 py-3 font-medium">服务</th>
                <th class="px-4 py-3 font-medium">状态</th>
                <th class="px-4 py-3 font-medium">PID</th>
                <th class="px-4 py-3 font-medium">端口</th>
                <th class="px-4 py-3 font-medium">JDK</th>
                <th class="px-4 py-3 font-medium">运行时长</th>
                <th class="px-4 py-3 text-right font-medium">操作</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="row in group.rows"
                :key="row.info.id"
                class="border-b border-slate-800/60 last:border-0 hover:bg-slate-800/30"
              >
                <td class="px-4 py-3">
                  <div class="font-mono text-xs text-slate-200">
                    {{ row.info.artifact_id }}
                  </div>
                  <div class="truncate text-[11px] text-slate-600" :title="row.info.main_class">
                    {{ row.info.main_class }}
                  </div>
                </td>
                <td class="px-4 py-3">
                  <StatusBadge :status="row.status" />
                </td>
                <td class="px-4 py-3 font-mono text-xs text-slate-400">
                  {{ row.pid || '-' }}
                </td>
                <td class="px-4 py-3 font-mono text-xs text-slate-400">
                  {{ row.info.port || '-' }}
                </td>
                <td class="px-4 py-3 font-mono text-xs text-slate-400">
                  {{ row.jdkVersion }}
                </td>
                <td class="px-4 py-3 font-mono text-xs text-slate-400">
                  {{ row.uptime }}
                </td>
                <td class="px-4 py-3">
                  <div class="flex justify-end gap-1">
                    <button
                      class="rounded p-1.5 text-emerald-400 hover:bg-emerald-500/10 disabled:opacity-30"
                      :disabled="isRunning(row.info.id) || services.busy"
                      title="启动"
                      @click="services.start(row.info.id)"
                    >
                      <Play class="h-4 w-4" />
                    </button>
                    <button
                      class="rounded p-1.5 text-red-400 hover:bg-red-500/10 disabled:opacity-30"
                      :disabled="!isRunning(row.info.id) || services.busy"
                      title="停止"
                      @click="services.stop(row.info.id)"
                    >
                      <Square class="h-4 w-4" />
                    </button>
                    <button
                      class="rounded p-1.5 text-amber-400 hover:bg-amber-500/10 disabled:opacity-30"
                      :disabled="services.busy"
                      title="重启"
                      @click="services.restart(row.info.id)"
                    >
                      <RotateCw class="h-4 w-4" />
                    </button>
                    <button
                      class="rounded p-1.5 text-slate-400 hover:bg-slate-700/50"
                      title="查看日志"
                      @click="viewLog(row.info.id)"
                    >
                      <FileText class="h-4 w-4" />
                    </button>
                  </div>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
    </div>
  </div>
</template>
