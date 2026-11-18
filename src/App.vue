<script setup lang="ts">
/**
 * 主布局：左侧侧边栏（导航 + 工作目录）+ 右侧主内容区。
 * 不用 vue-router 的单文件路由表，手动切 tab 更轻量。
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import {
  LayoutDashboard,
  Server,
  Terminal,
  TerminalSquare,
  Boxes,
  Settings as SettingsIcon,
  AlertCircle,
  CheckCircle2,
  Info,
  X,
} from 'lucide-vue-next';
import { useSettingsStore } from '@/stores/settings';
import { useServicesStore } from '@/stores/services';
import { useTerminalsStore } from '@/stores/terminals';
import { onLog, onStatus } from '@/api/tauri';
import Dashboard from '@/views/Dashboard.vue';
import ServiceList from '@/views/ServiceList.vue';
import LogViewer from '@/views/LogViewer.vue';
import Console from '@/views/Console.vue';
import JdkManager from '@/views/JdkManager.vue';
import SettingsView from '@/views/Settings.vue';

type Tab = 'dashboard' | 'services' | 'logs' | 'console' | 'jdk' | 'settings';

const tab = ref<Tab>('dashboard');

const settings = useSettingsStore();
const services = useServicesStore();
const terminals = useTerminalsStore();

const navItems = computed(() => [
  { key: 'dashboard' as const, label: '总览', icon: LayoutDashboard },
  { key: 'services' as const, label: '服务', icon: Server },
  { key: 'logs' as const, label: '日志', icon: Terminal },
  { key: 'console' as const, label: '控制台', icon: TerminalSquare },
  { key: 'jdk' as const, label: 'JDK', icon: Boxes },
  { key: 'settings' as const, label: '设置', icon: SettingsIcon },
]);

/** 运行中服务计数（侧边栏徽章） */
const runningCount = computed(
  () =>
    Object.values(services.runtimes).filter((r) => r.status === 'running')
      .length,
);

/** 运行中终端计数（侧边栏徽章） */
const runningTerminalCount = computed(() => terminals.runningCount);

/* ---- 生命周期：加载配置 + 订阅事件 ---- */
// 把 window 当成任意键值容器用（存动态 unlisten 句柄）
const win = window as unknown as Record<string, unknown>;

/** 内存占用定时刷新句柄（每 30 秒） */
let memTimer: ReturnType<typeof setInterval> | undefined;

onMounted(async () => {
  await settings.load();
  // 加载控制台配置列表（PTY 进程不自动重开，仅恢复配置）
  await terminals.load();
  // 订阅所有服务的日志：为已扫描服务各建监听
  await onStatus((id, status, pid) => {
    services.applyStatus(id, status as never, pid);
  });
  if (settings.workspace) {
    await services.scan();
  } else {
    // 没有工作目录也同步一次运行态（可能其他工作目录有服务在跑）
    await services.syncStatus();
  }
  // 内存占用统计：启动时刷新一次，之后每 30 秒刷新
  await services.refreshMemory();
  memTimer = setInterval(() => services.refreshMemory(), 30_000);
});

onBeforeUnmount(() => {
  if (memTimer) clearInterval(memTimer);
});

watch(
  () => Object.keys(services.runtimes),
  async (ids) => {
    // 为每个出现过运行态的服务订阅日志（含其他工作目录的服务），
    // 让日志页能展示所有工作目录的服务输出。
    for (const id of ids) {
      // 用闭包标识避免重复 listen：存到 window 上（够用）
      const key = `__log_unlisten_${id}`;
      if (!win[key]) {
        win[key] = await onLog(id, (lines) => {
          services.pushLogBatch(id, lines);
        });
      }
    }
  },
  { deep: false },
);

/** 消息条自动消失由 store 控制，这里只负责渲染 + 手动关闭 */
function dismissMessage() {
  services.message = null;
}

function messageIcon(type: string) {
  if (type === 'error') return AlertCircle;
  if (type === 'success') return CheckCircle2;
  return Info;
}
function messageClass(type: string) {
  if (type === 'error') return 'bg-red-500/15 text-red-300 border-red-500/40';
  if (type === 'success')
    return 'bg-emerald-500/15 text-emerald-300 border-emerald-500/40';
  return 'bg-sky-500/15 text-sky-300 border-sky-500/40';
}
</script>

<template>
  <div class="flex h-screen w-screen overflow-hidden bg-slate-900 text-slate-200">
    <!-- 侧边栏 -->
    <aside
      class="flex w-56 shrink-0 flex-col border-r border-slate-800 bg-slate-950/60"
    >
      <div class="flex items-center gap-2 px-4 py-4">
        <div
          class="flex h-9 w-9 items-center justify-center rounded-lg bg-indigo-500 font-bold text-white"
        >
          C
        </div>
        <div class="leading-tight">
          <div class="text-sm font-semibold text-white">Spring 启动器</div>
          <div class="text-[11px] text-slate-500">本地服务管理</div>
        </div>
      </div>

      <nav class="flex-1 space-y-1 px-2 py-2">
        <button
          v-for="item in navItems"
          :key="item.key"
          class="flex w-full items-center gap-3 rounded-md px-3 py-2 text-sm transition"
          :class="
            tab === item.key
              ? 'bg-indigo-500/15 text-indigo-300'
              : 'text-slate-400 hover:bg-slate-800/60 hover:text-slate-200'
          "
          @click="tab = item.key"
        >
          <component :is="item.icon" class="h-4 w-4" />
          <span class="flex-1 text-left">{{ item.label }}</span>
          <span
            v-if="item.key === 'services' && runningCount > 0"
            class="rounded-full bg-emerald-500/20 px-1.5 py-0.5 text-[10px] font-medium text-emerald-300"
          >
            {{ runningCount }}
          </span>
          <span
            v-if="item.key === 'console' && runningTerminalCount > 0"
            class="rounded-full bg-sky-500/20 px-1.5 py-0.5 text-[10px] font-medium text-sky-300"
          >
            {{ runningTerminalCount }}
          </span>
        </button>
      </nav>

      <div class="border-t border-slate-800 px-3 py-3">
        <div class="mb-1 flex items-center justify-between">
          <span class="text-[11px] text-slate-500">工作目录</span>
          <span
            v-if="settings.workspaces.length > 0"
            class="text-[10px] text-slate-600"
          >
            {{ settings.workspaces.length }} 个历史
          </span>
        </div>
        <div
          class="truncate text-xs text-slate-400"
          :title="settings.workspace"
        >
          {{ settings.workspace || '未选择' }}
        </div>
      </div>
    </aside>

    <!-- 主区 -->
    <main class="relative flex flex-1 flex-col overflow-hidden">
      <!-- 悬浮消息条（不占文档流，避免挤压内容） -->
      <Transition name="msg">
        <div
          v-if="services.message"
          class="pointer-events-auto absolute left-1/2 top-4 z-50 flex max-w-md -translate-x-1/2 items-center gap-2 rounded-md border px-3 py-2 text-sm shadow-lg"
          :class="messageClass(services.message.type)"
        >
          <component
            :is="messageIcon(services.message.type)"
            class="h-4 w-4 shrink-0"
          />
          <span class="flex-1">{{ services.message.text }}</span>
          <button class="opacity-60 hover:opacity-100" @click="dismissMessage">
            <X class="h-4 w-4" />
          </button>
        </div>
      </Transition>

      <div class="flex-1 overflow-auto">
        <Dashboard v-if="tab === 'dashboard'" v-model:tab="tab" />
        <ServiceList v-else-if="tab === 'services'" v-model:tab="tab" />
        <LogViewer v-else-if="tab === 'logs'" />
        <Console v-else-if="tab === 'console'" />
        <JdkManager v-else-if="tab === 'jdk'" />
        <SettingsView v-else-if="tab === 'settings'" />
      </div>
    </main>
  </div>
</template>

<style scoped>
.msg-enter-active,
.msg-leave-active {
  transition: all 0.3s ease;
}
.msg-enter-from,
.msg-leave-to {
  opacity: 0;
  transform: translateY(-12px);
}
</style>
