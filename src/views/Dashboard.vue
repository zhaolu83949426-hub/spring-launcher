<script setup lang="ts">
/** 总览页：选工作目录 → 服务列表 → 一键启动
 *  选中状态按工作目录持久化，切换工作目录自动恢复上次选中
 *  服务列表支持长按拖拽自定义排序（顺序同样按工作目录持久化） */
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue';
import {
  FolderOpen,
  Square,
  RefreshCw,
  Rocket,
  Circle,
  ChevronDown,
  Trash2,
  Check,
  Pin,
  Play,
  GripVertical,
} from 'lucide-vue-next';
import { useSettingsStore } from '@/stores/settings';
import { useServicesStore } from '@/stores/services';
import { pickDirectory } from '@/api/tauri';
import { formatMemory } from '@/utils';
import type { ServiceStatus } from '@/types';

const settings = useSettingsStore();
const services = useServicesStore();

const tab = defineModel<TabType>('tab');
type TabType = 'dashboard' | 'services' | 'logs' | 'jdk' | 'settings';

/** 当前工作目录的选中服务 id 集合（从 settings store 读取，按工作目录持久化） */
const selectedIds = computed(
  () => new Set(settings.getSelection(settings.workspace)),
);

/** 当前工作目录的置顶服务 id 集合（按工作目录持久化） */
const pinnedIds = computed(
  () => new Set(settings.getPins(settings.workspace)),
);

/** 工作目录下拉是否展开 */
const wsDropdownOpen = ref(false);

async function chooseWorkspace() {
  const dir = await pickDirectory();
  if (dir) {
    await settings.setWorkspace(dir);
    await services.scan();
  }
}

/** 切换到历史工作目录（不停止旧工作目录的服务） */
async function switchToWorkspace(path: string) {
  wsDropdownOpen.value = false;
  if (path === settings.workspace) return;
  await settings.switchWorkspace(path);
  await services.scan();
}

/** 删除历史工作目录记录 */
async function removeWorkspace(path: string, e: Event) {
  e.stopPropagation();
  await settings.removeWorkspace(path);
}

/** 点击外部关闭下拉 */
function onDocClick(e: MouseEvent) {
  const target = e.target as HTMLElement;
  if (!target.closest('[data-ws-dropdown]')) {
    wsDropdownOpen.value = false;
  }
}
onMounted(() => {
  document.addEventListener('click', onDocClick);
  document.addEventListener('pointermove', onPointerMove);
  document.addEventListener('pointerup', onPointerUp);
  document.addEventListener('pointercancel', endDrag);
  window.addEventListener('keydown', onKeyDown);
  window.addEventListener('blur', endDrag);
});
onBeforeUnmount(() => {
  document.removeEventListener('click', onDocClick);
  document.removeEventListener('pointermove', onPointerMove);
  document.removeEventListener('pointerup', onPointerUp);
  document.removeEventListener('pointercancel', endDrag);
  window.removeEventListener('keydown', onKeyDown);
  window.removeEventListener('blur', endDrag);
  endDrag();
});

async function rescan() {
  await services.scan();
}

/** 全部可启动服务（扫描到的） */
const allServices = computed(() => services.list());
const checkedCount = computed(() => selectedIds.value.size);

/** 其他工作目录中仍在运行的服务 */
const otherRunning = computed(() => services.otherWorkspaceRunning());

async function toggle(id: string) {
  const s = new Set(selectedIds.value);
  if (s.has(id)) s.delete(id);
  else s.add(id);
  await settings.setSelection(settings.workspace, Array.from(s));
}

async function selectAll() {
  await settings.setSelection(
    settings.workspace,
    allServices.value.map((s) => s.id),
  );
}
async function clearAll() {
  await settings.setSelection(settings.workspace, []);
}

/** 取某服务的运行状态，未扫到则 stopped */
function statusOf(id: string): ServiceStatus {
  return services.runtimes[id]?.status ?? 'stopped';
}

/** 运行状态对应的圆点颜色 + 文案 */
function statusDot(status: ServiceStatus): {
  color: string;
  label: string;
} {
  switch (status) {
    case 'running':
      return { color: 'text-emerald-400', label: '运行中' };
    case 'starting':
      return { color: 'text-amber-400', label: '启动中' };
    case 'stopping':
      return { color: 'text-amber-400', label: '停止中' };
    case 'crashed':
      return { color: 'text-red-400', label: '已崩溃' };
    default:
      return { color: 'text-slate-600', label: '已停止' };
  }
}

const stats = computed(() => {
  // 只统计当前工作目录扫描到的服务
  const arr = allServices.value.map((s) => services.runtimes[s.id]).filter(Boolean);
  return {
    total: allServices.value.length,
    running: arr.filter((r) => r.status === 'running').length,
    starting: arr.filter((r) => r.status === 'starting').length,
    crashed: arr.filter((r) => r.status === 'crashed').length,
  };
});

/** 当前工作目录所有已启动服务的内存总和（字节） */
const totalMemory = computed(() =>
  allServices.value.reduce((sum, s) => sum + (services.memory[s.id] ?? 0), 0),
);

/** 当前工作目录的自定义排序 id 列表（空数组表示未自定义） */
const orderIds = computed(() => settings.getOrder(settings.workspace));

/** id -> 自定义排序序号 */
const orderIndex = computed(() => {
  const map = new Map<string, number>();
  orderIds.value.forEach((id, i) => map.set(id, i));
  return map;
});

/** 服务列表排序：置顶的排前面；同组内按自定义顺序，未自定义的保持扫描顺序（稳定排序） */
const sortedServices = computed(() => {
  const pins = pinnedIds.value;
  const order = orderIndex.value;
  return [...allServices.value].sort((a, b) => {
    const aPin = pins.has(a.id) ? 0 : 1;
    const bPin = pins.has(b.id) ? 0 : 1;
    if (aPin !== bPin) return aPin - bPin;
    const ai = order.get(a.id) ?? Number.MAX_SAFE_INTEGER;
    const bi = order.get(b.id) ?? Number.MAX_SAFE_INTEGER;
    return ai - bi;
  });
});

/** 取某服务的内存占用（字节），未运行返回 0 */
function memOf(id: string): number {
  return services.memory[id] ?? 0;
}

async function startSelected() {
  // 只启动当前工作目录存在的选中服务
  const existingIds = new Set(allServices.value.map((s) => s.id));
  const ids = Array.from(selectedIds.value).filter((id) => existingIds.has(id));
  await services.startSelected(ids);
  tab.value = 'services';
}

async function stopAll() {
  // 只停止当前工作目录下的运行中服务，不影响其他工作目录的服务
  await services.stopWorkspace(settings.workspace);
}

/** 切换单个服务的置顶状态 */
async function togglePin(id: string) {
  await settings.togglePin(settings.workspace, id);
}

/** 启动单个服务 */
async function startOne(id: string) {
  await services.start(id);
}

/** 停止单个服务 */
async function stopOne(id: string) {
  await services.stop(id);
}

/** 从路径中提取短名称用于显示 */
function shortName(path: string): string {
  const parts = path.replace(/\\/g, '/').split('/').filter(Boolean);
  return parts[parts.length - 1] || path;
}

/* ==================== 服务列表：长按拖拽排序 ==================== */

/** 长按判定时长（毫秒）。按住超过该时长才进入拖拽，避免与点击/选文冲突 */
const LONG_PRESS_MS = 180;
/** 长按判定期间允许的最大位移（像素），越界视为滚动或选文本，取消拖拽 */
const MOVE_TOLERANCE_PX = 8;

/** 列表容器，用于测量各行的位置 */
const listRef = ref<HTMLElement | null>(null);

/** 正在拖拽的服务 id，空串表示未拖拽 */
const dragId = ref('');
/** 拖拽落点：插入槽位下标（0..n，n 表示追加到末尾） */
const dragOverIndex = ref(-1);
/** 跟随鼠标的浮层坐标 */
const ghostPos = reactive({ x: 0, y: 0 });
/** 是否处于拖拽态 */
const isDragging = computed(() => dragId.value !== '');
/** 拖拽浮层展示的服务名 */
const dragLabel = computed(
  () => allServices.value.find((s) => s.id === dragId.value)?.artifact_id ?? '',
);

/** 长按计时器与按下点（进入拖拽前的临时状态） */
let pressTimer = 0;
let pressId = '';
let pressX = 0;
let pressY = 0;

/** 按下：手柄立即进入拖拽；整行则需长按（避免与点击/选文冲突） */
function onRowPointerDown(e: PointerEvent, id: string) {
  clearPress();
  if (e.button !== 0) return;
  const el = e.target as HTMLElement;
  if (el.closest('button, input, a, select, textarea')) return;
  pressId = id;
  pressX = e.clientX;
  pressY = e.clientY;
  // 手柄是明确的拖拽意图，按下即拖，无需长按
  if (el.closest('[data-drag-handle]')) {
    beginDrag();
    return;
  }
  pressTimer = window.setTimeout(beginDrag, LONG_PRESS_MS);
}

/** 清理长按计时器与临时按下状态 */
function clearPress() {
  if (pressTimer) {
    window.clearTimeout(pressTimer);
    pressTimer = 0;
  }
  pressId = '';
}

/** 长按到点 → 进入拖拽态 */
function beginDrag() {
  pressTimer = 0;
  if (!pressId) return;
  dragId.value = pressId;
  ghostPos.x = pressX;
  ghostPos.y = pressY;
  updateDropIndex(pressY);
  // 拖拽期间禁止文本选中，并清掉长按期间可能已产生的选区
  document.body.style.userSelect = 'none';
  document.body.style.cursor = 'grabbing';
  window.getSelection()?.removeAllRanges();
}

/** 结束/取消拖拽：复位全部临时状态（不提交排序） */
function endDrag() {
  clearPress();
  dragId.value = '';
  dragOverIndex.value = -1;
  document.body.style.userSelect = '';
  document.body.style.cursor = '';
}

/** 指针移动：待拖拽时越界即取消长按；拖拽中更新浮层与落点 */
function onPointerMove(e: PointerEvent) {
  if (!isDragging.value) {
    if (
      pressTimer &&
      (Math.abs(e.clientX - pressX) > MOVE_TOLERANCE_PX ||
        Math.abs(e.clientY - pressY) > MOVE_TOLERANCE_PX)
    ) {
      clearPress();
    }
    return;
  }
  ghostPos.x = e.clientX;
  ghostPos.y = e.clientY;
  updateDropIndex(e.clientY);
}

/** 松手：若已进入拖拽态则提交排序 */
function onPointerUp() {
  if (!isDragging.value) {
    clearPress();
    return;
  }
  const fromIndex = sortedServices.value.findIndex((s) => s.id === dragId.value);
  const slot = dragOverIndex.value;
  endDrag();
  if (fromIndex >= 0 && slot >= 0) void commitReorder(fromIndex, slot);
}

/** Esc 取消拖拽 */
function onKeyDown(e: KeyboardEvent) {
  if (e.key === 'Escape') endDrag();
}

/**
 * 根据鼠标纵向位置计算插入槽位。
 * 置顶分组与普通分组各自独立，落点被夹在自己所属分组的区间内，
 * 因此拖拽不会改变服务的置顶状态。
 */
function updateDropIndex(clientY: number) {
  const fromIndex = sortedServices.value.findIndex((s) => s.id === dragId.value);
  if (fromIndex < 0 || !listRef.value) return;
  const rows = Array.from(
    listRef.value.querySelectorAll<HTMLElement>('[data-drag-row]'),
  );
  let slot = rows.length;
  for (let i = 0; i < rows.length; i++) {
    const rect = rows[i].getBoundingClientRect();
    if (clientY < rect.top + rect.height / 2) {
      slot = i;
      break;
    }
  }
  dragOverIndex.value = clampToGroup(fromIndex, slot);
}

/** 把插入槽位夹到 fromIndex 所属分组内，返回合法的槽位区间 [lo, hi] */
function clampToGroup(fromIndex: number, slot: number): number {
  const pins = pinnedIds.value;
  const list = sortedServices.value;
  const fromPinned = pins.has(list[fromIndex]?.id ?? '');
  let lo = 0;
  let hi = list.length;
  if (fromPinned) {
    // 置顶组：[0, 首个非置顶下标]，可插到组末尾
    const firstUnpinned = list.findIndex((s) => !pins.has(s.id));
    if (firstUnpinned >= 0) hi = firstUnpinned;
  } else {
    // 普通组：[首个非置顶下标, 末尾]
    const firstUnpinned = list.findIndex((s) => !pins.has(s.id));
    lo = firstUnpinned < 0 ? list.length : firstUnpinned;
  }
  return Math.min(Math.max(slot, lo), hi);
}

/** 应用排序结果并持久化。slot 为插入槽位（插入到该下标之前） */
async function commitReorder(fromIndex: number, slot: number) {
  const before = sortedServices.value.map((s) => s.id);
  const ids = [...before];
  const [moved] = ids.splice(fromIndex, 1);
  // 移除被拖项后，其后的下标整体前移一位
  ids.splice(slot > fromIndex ? slot - 1 : slot, 0, moved);
  // 顺序未变化时不写配置
  if (before.every((id, i) => id === ids[i])) return;
  await settings.setOrder(settings.workspace, ids);
}
</script>

<template>
  <div class="space-y-5 p-5">
    <!-- 工作目录选择 -->
    <section
      class="rounded-xl border border-slate-800 bg-slate-900/50 p-4"
    >
      <div class="mb-2 flex items-center justify-between">
        <h2 class="text-sm font-semibold text-slate-200">工作目录</h2>
        <button
          v-if="settings.workspace"
          class="flex items-center gap-1 rounded-md px-2 py-1 text-xs text-slate-400 hover:bg-slate-800 hover:text-slate-200"
          @click="rescan"
        >
          <RefreshCw class="h-3.5 w-3.5" /> 重新扫描
        </button>
      </div>
      <div class="flex items-center gap-2">
        <!-- 工作目录下拉 + 历史切换 -->
        <div class="relative flex-1" data-ws-dropdown>
          <button
            class="flex w-full items-center gap-2 rounded-md border border-slate-700 bg-slate-950 px-3 py-2 text-left font-mono text-xs text-slate-400 hover:border-slate-600"
            @click="wsDropdownOpen = !wsDropdownOpen"
          >
            <span class="flex-1 truncate" :title="settings.workspace">
              {{ settings.workspace || '未选择工作目录' }}
            </span>
            <ChevronDown
              class="h-3.5 w-3.5 shrink-0 text-slate-500 transition"
              :class="{ 'rotate-180': wsDropdownOpen }"
            />
          </button>
          <!-- 历史工作目录下拉 -->
          <div
            v-if="wsDropdownOpen"
            class="absolute left-0 top-full z-30 mt-1 max-h-72 w-full overflow-auto rounded-md border border-slate-700 bg-slate-900 py-1 shadow-xl"
          >
            <div
              v-if="settings.workspaces.length === 0"
              class="px-3 py-4 text-center text-xs text-slate-600"
            >
              暂无历史工作目录
            </div>
            <div
              v-for="ws in [...settings.workspaces].reverse()"
              :key="ws"
              class="group flex cursor-pointer items-center gap-2 px-3 py-2 hover:bg-slate-800"
              :class="{
                'bg-indigo-500/10 text-indigo-300':
                  ws === settings.workspace,
              }"
              @click="switchToWorkspace(ws)"
            >
              <Check
                v-if="ws === settings.workspace"
                class="h-3.5 w-3.5 shrink-0 text-indigo-400"
              />
              <Circle
                v-else
                class="h-3.5 w-3.5 shrink-0 text-slate-600"
                :fill="'currentColor'"
              />
              <div class="min-w-0 flex-1">
                <div class="truncate text-xs text-slate-300" :title="ws">
                  {{ shortName(ws) }}
                </div>
                <div class="truncate text-[10px] text-slate-600" :title="ws">
                  {{ ws }}
                </div>
              </div>
              <button
                class="shrink-0 rounded p-1 text-slate-500 opacity-0 hover:bg-red-500/20 hover:text-red-400 group-hover:opacity-100"
                title="删除历史记录"
                @click="removeWorkspace(ws, $event)"
              >
                <Trash2 class="h-3.5 w-3.5" />
              </button>
            </div>
          </div>
        </div>
        <button
          class="flex items-center gap-1.5 rounded-md bg-indigo-500 px-3 py-2 text-sm font-medium text-white hover:bg-indigo-600"
          @click="chooseWorkspace"
        >
          <FolderOpen class="h-4 w-4" /> 选择
        </button>
      </div>
      <!-- 其他工作目录运行中服务提示 -->
      <div
        v-if="otherRunning.length > 0"
        class="mt-2 flex items-center gap-1.5 rounded-md border border-amber-500/30 bg-amber-500/10 px-3 py-1.5 text-xs text-amber-300"
      >
        <Circle class="h-2.5 w-2.5 shrink-0" :fill="'currentColor'" />
        <span>
          有 {{ otherRunning.length }} 个服务来自其他工作目录仍在运行，切换工作目录不会停止它们
        </span>
      </div>
    </section>

    <!-- 统计卡片 -->
    <section class="grid grid-cols-5 gap-3">
      <div class="rounded-xl border border-slate-800 bg-slate-900/50 p-4">
        <div class="text-2xl font-bold text-slate-100">{{ stats.total }}</div>
        <div class="text-xs text-slate-500">总服务数</div>
      </div>
      <div class="rounded-xl border border-slate-800 bg-slate-900/50 p-4">
        <div class="text-2xl font-bold text-sky-400">
          {{ formatMemory(totalMemory) }}
        </div>
        <div class="text-xs text-slate-500">内存占用</div>
      </div>
      <div class="rounded-xl border border-slate-800 bg-slate-900/50 p-4">
        <div class="text-2xl font-bold text-emerald-400">{{ stats.running }}</div>
        <div class="text-xs text-slate-500">运行中</div>
      </div>
      <div class="rounded-xl border border-slate-800 bg-slate-900/50 p-4">
        <div class="text-2xl font-bold text-amber-400">{{ stats.starting }}</div>
        <div class="text-xs text-slate-500">启动中</div>
      </div>
      <div class="rounded-xl border border-slate-800 bg-slate-900/50 p-4">
        <div class="text-2xl font-bold text-red-400">{{ stats.crashed }}</div>
        <div class="text-xs text-slate-500">已崩溃</div>
      </div>
    </section>

    <!-- 当前工作目录服务列表 + 操作栏 -->
    <section
      class="rounded-xl border border-slate-800 bg-slate-900/50 p-4"
    >
      <div class="mb-3 flex items-center justify-between">
        <h2 class="text-sm font-semibold text-slate-200">
          服务列表
          <span class="ml-1 text-xs font-normal text-slate-500">
            （当前工作目录扫描到 {{ allServices.length }} 个）
          </span>
          <span
            v-if="allServices.length > 1"
            class="ml-1 text-xs font-normal text-slate-600"
          >
            · 长按拖拽可调整顺序
          </span>
        </h2>
        <!-- 操作栏 -->
        <div class="flex items-center gap-3">
          <button
            class="flex items-center gap-2 rounded-md bg-emerald-500 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-600 disabled:opacity-40"
            :disabled="checkedCount === 0 || services.busy"
            @click="startSelected"
          >
            <Rocket class="h-4 w-4" /> 启动选中 ({{ checkedCount }})
          </button>
          <button
            class="flex items-center gap-2 rounded-md bg-red-500/80 px-4 py-1.5 text-sm font-medium text-white hover:bg-red-600 disabled:opacity-40"
            :disabled="stats.running === 0 || services.busy"
            @click="stopAll"
          >
            <Square class="h-4 w-4" /> 全部停止
          </button>
          <div class="flex gap-2 text-xs">
            <button class="text-indigo-300 hover:underline" @click="selectAll">
              全选
            </button>
            <span class="text-slate-600">|</span>
            <button class="text-slate-400 hover:underline" @click="clearAll">
              清空
            </button>
          </div>
        </div>
      </div>
      <div
        v-if="allServices.length === 0"
        class="py-6 text-center text-sm text-slate-600"
      >
        暂无服务，请选择工作目录或重新扫描
      </div>
      <div v-else ref="listRef" class="space-y-1">
        <div
          v-for="(s, i) in sortedServices"
          :key="s.id"
          data-drag-row
          class="relative flex items-center gap-2 rounded-md px-2 py-1.5 text-xs hover:bg-slate-800/40"
          :class="{
            'bg-slate-800/60 opacity-40 ring-1 ring-inset ring-indigo-400/60':
              dragId === s.id,
          }"
          @pointerdown="onRowPointerDown($event, s.id)"
        >
          <!-- 插入位置指示线 -->
          <span
            v-if="dragOverIndex === i"
            class="pointer-events-none absolute inset-x-0 -top-1 h-0.5 rounded-full bg-indigo-400"
          ></span>
          <!-- 拖拽手柄：按下即拖；整行长按亦可拖拽 -->
          <span
            data-drag-handle
            class="flex shrink-0 cursor-grab items-center px-0.5 text-slate-600 hover:text-slate-400"
            title="拖拽排序（整行长按亦可）"
          >
            <GripVertical class="h-3.5 w-3.5" />
          </span>
          <input
            type="checkbox"
            class="accent-indigo-500"
            :checked="selectedIds.has(s.id)"
            @change="toggle(s.id)"
          />
          <Circle
            class="h-2.5 w-2.5 shrink-0"
            :class="statusDot(statusOf(s.id)).color"
            :fill="'currentColor'"
          />
          <Pin
            v-if="pinnedIds.has(s.id)"
            class="h-3 w-3 shrink-0 text-indigo-400"
            :fill="'currentColor'"
          />
          <span
            class="flex-1 cursor-grab truncate font-mono text-slate-300"
            :title="s.main_class"
          >
            {{ s.artifact_id }}
          </span>
          <span class="shrink-0 text-[11px] text-slate-600">
            {{ statusDot(statusOf(s.id)).label }}
          </span>
          <span
            class="shrink-0 font-mono text-[11px]"
            :class="memOf(s.id) > 0 ? 'text-sky-400' : 'text-slate-600'"
          >
            {{ formatMemory(memOf(s.id)) }}
          </span>
          <span class="shrink-0 w-10 text-right font-mono text-[11px] text-slate-600">
            {{ s.port || '-' }}
          </span>
          <!-- 操作列：置顶 / 开启 / 停止 -->
          <span class="flex shrink-0 items-center gap-0.5">
            <button
              class="rounded p-1 hover:bg-slate-700/50 disabled:opacity-30"
              :class="pinnedIds.has(s.id) ? 'text-indigo-400' : 'text-slate-500'"
              :title="pinnedIds.has(s.id) ? '取消置顶' : '置顶'"
              @click="togglePin(s.id)"
            >
              <Pin class="h-3.5 w-3.5" :fill="pinnedIds.has(s.id) ? 'currentColor' : 'none'" />
            </button>
            <button
              class="rounded p-1 text-emerald-400 hover:bg-emerald-500/10 disabled:opacity-30"
              :disabled="(statusOf(s.id) !== 'stopped' && statusOf(s.id) !== 'crashed') || services.busy"
              title="启动"
              @click="startOne(s.id)"
            >
              <Play class="h-3.5 w-3.5" />
            </button>
            <button
              class="rounded p-1 text-red-400 hover:bg-red-500/10 disabled:opacity-30"
              :disabled="statusOf(s.id) === 'stopped' || statusOf(s.id) === 'crashed' || services.busy"
              title="停止"
              @click="stopOne(s.id)"
            >
              <Square class="h-3.5 w-3.5" />
            </button>
          </span>
        </div>
        <!-- 拖到列表末尾时的指示线 -->
        <div
          v-if="isDragging && dragOverIndex === sortedServices.length"
          class="pointer-events-none h-0.5 rounded-full bg-indigo-400"
        ></div>
      </div>
    </section>

    <!-- 拖拽浮层：跟随鼠标显示被拖服务名 -->
    <Teleport to="body">
      <div
        v-if="isDragging"
        class="pointer-events-none fixed z-50 flex items-center gap-2 rounded-md border border-indigo-400/60 bg-slate-900 px-3 py-1.5 font-mono text-xs text-slate-100 shadow-xl shadow-black/40"
        :style="{ left: ghostPos.x + 14 + 'px', top: ghostPos.y + 14 + 'px' }"
      >
        <GripVertical class="h-3.5 w-3.5 shrink-0 text-indigo-400" />
        <span class="max-w-[16rem] truncate">{{ dragLabel }}</span>
      </div>
    </Teleport>
  </div>
</template>
