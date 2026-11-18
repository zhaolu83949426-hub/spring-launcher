<script setup lang="ts">
/**
 * 控制台视图：内嵌交互式终端管理器。
 *
 * - 顶部 tab 条：终端列表（名称 + 运行圆点）+「+ 新建」
 * - 主体：每个 terminal id 一个容器，各自挂一个 xterm Terminal 实例
 *   切 tab 用 v-show（保留 buffer，不销毁实例），仅活跃 tab 可见
 * - 双向流：xterm onData → terminalWrite；terminal:data → term.write
 * - 自适应列宽：FitAddon + ResizeObserver → terminalResize
 */
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import {
  computed,
  nextTick,
  onBeforeUnmount,
  onMounted,
  reactive,
  ref,
  watch,
} from 'vue';
import {
  Plus,
  Play,
  RotateCw,
  Pencil,
  Trash2,
  X,
  FolderOpen,
  Power,
} from 'lucide-vue-next';
import { useTerminalsStore } from '@/stores/terminals';
import { useSettingsStore } from '@/stores/settings';
import * as api from '@/api/tauri';
import type { TerminalShell } from '@/types';

const terminals = useTerminalsStore();
const settings = useSettingsStore();

/** 每个终端的 xterm 实例 + 附属资源 */
interface TermEntry {
  term: Terminal;
  fit: FitAddon;
  /** 取消 data 事件订阅 */
  unlistenData?: () => void;
  /** 取消 exit 事件订阅 */
  unlistenExit?: () => void;
  /** ResizeObserver */
  observer?: ResizeObserver;
  /** 是否已 open 到 DOM */
  opened: boolean;
  /** 容器 DOM 引用（set via ref callback） */
  el?: HTMLElement;
}

/** terminal_id -> TermEntry */
const entries = reactive<Record<string, TermEntry>>({});
/** 正在挂载中的终端 id（防止 mountTerm 并发重复，导致订阅/挂载多次） */
const mountingIds = new Set<string>();

/** 容器 ref 映射：template 里用 :ref 绑定函数收集 */
const containerRefs = ref<Record<string, HTMLElement | null>>({});
const bodyRef = ref<HTMLElement | null>(null);

/** 当前 tab 列表（基于 configs，带显示名） */
const tabs = computed(() =>
  terminals.configs.map((c, i) => ({
    id: c.id,
    name: terminals.displayName(c.id, i),
    running: terminals.infos[c.id]?.running ?? false,
  })),
);

const activeId = computed({
  get: () => terminals.activeId,
  set: (v) => {
    terminals.activeId = v;
  },
});

/* ---------- 新建终端弹窗 ---------- */
const showCreate = ref(false);
const form = reactive({
  name: '',
  cwd: '',
  shell: 'auto' as TerminalShell,
});

function openCreateDialog() {
  // 默认工作目录取当前 settings.workspace（你说的"默认到当前工作目录"）
  form.name = '';
  form.cwd = settings.workspace || '';
  form.shell = 'auto';
  showCreate.value = true;
}

async function pickCwd() {
  const dir = await api.pickDirectory();
  if (dir) form.cwd = dir;
}

async function submitCreate() {
  showCreate.value = false;
  const cwd = form.cwd.trim();
  if (!cwd) {
    message('请选择工作目录', 'error');
    return;
  }
  try {
    await terminals.create(cwd, form.shell, form.name);
    await nextTick();
    await mountActive();
  } catch (e) {
    message(String(e), 'error');
  }
}

/* ---------- 消息条（轻量复用 services store 风格，但本视图独立维护） ---------- */
const toast = ref<{ text: string; type: string } | null>(null);
let toastTimer: ReturnType<typeof setTimeout> | undefined;
function message(text: string, type = 'info') {
  toast.value = { text, type };
  if (toastTimer) clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (toast.value = null), 3000);
}

/* ================ xterm 实例管理 ================ */

/** 新建一个 xterm 实例（不立即 open，等容器就绪）。
 *  关键：事件订阅在此一次性注册，与 mountTerm 的调用次数/时序解耦，
 *  避免并发挂载导致订阅重复（按键回显翻倍的根因）。 */
async function createTermEntry(id: string): Promise<TermEntry> {
  const fit = new FitAddon();
  const term = new Terminal({
    fontSize: 13,
    fontFamily:
      '"Cascadia Code", "JetBrains Mono", Consolas, "Courier New", monospace',
    cursorBlink: true,
    scrollback: 5000,
    theme: {
      background: '#020617', // slate-950，与日志区一致
      foreground: '#e2e8f0', // slate-200
      cursor: '#e2e8f0',
      selectionBackground: '#33415580',
    },
  });
  term.loadAddon(fit);
  term.onData((d) => {
    api.terminalWrite(id, d).catch((e) => logErr('write', e));
  });
  const entry: TermEntry = { term, fit, opened: false };
  // 订阅输出 / 退出（仅此一次）
  entry.unlistenData = await api.onTerminalData(id, (data) => {
    // 始终写到当前 entry 的 term（即便重新 open 过 DOM，term 实例不变）
    entries[id]?.term.write(data);
  });
  entry.unlistenExit = await api.onTerminalExit(id, () => {
    entries[id]?.term.write('\r\n\x1b[31m[进程已退出]\x1b[0m\r\n');
    terminals.markExited(id);
  });
  return entry;
}

/** 把终端实例 open 到容器，启动 ResizeObserver。
 *  事件订阅已在 createTermEntry 时一次性完成，这里只管 DOM 挂载 / 焦点 / 尺寸。
 *  mountingIds 防止并发调用（新建后会同时触发 mountActive 与 watch(activeId)，
 *  两者并发会在 await createTermEntry 期间各注册一份订阅，导致回显翻倍）。 */
async function mountTerm(id: string) {
  const el = containerRefs.value[id];
  if (!el) return;

  // 并发守卫：已在挂载中则跳过本次
  if (mountingIds.has(id)) return;
  mountingIds.add(id);
  try {
    // 没有 entry 时创建（订阅在此一次性注册）
    let entry = entries[id];
    if (!entry) {
      entry = await createTermEntry(id);
      entries[id] = entry;
    }
    entry.el = el;

    if (!entry.opened) {
      // open 前清空容器（避免重复渲染残留）
      el.innerHTML = '';
      entry.term.open(el);
      entry.opened = true;
      // 首次 open 后容器布局可能尚未完成（尺寸为 0），延迟到下一帧再 fit + resize，
      // 否则 xterm 的 textarea 无法聚焦、输入失效。
      requestAnimationFrame(() => {
        const e = entries[id];
        if (!e || !e.opened) return;
        try {
          e.fit.fit();
          const dims = proposeDims(e);
          if (dims) {
            api.terminalResize(id, dims.cols, dims.rows).catch(() => {});
          }
        } catch {
          /* 容器未布局好，等 resize 观察器兜底 */
        }
        e.term.focus();
      });
    }

    // 容器尺寸观察（仅一次）
    if (!entry.observer) {
      const observer = new ResizeObserver(() => {
        const e = entries[id];
        if (!e || !e.opened) return;
        try {
          e.fit.fit();
          const dims = proposeDims(e);
          if (dims) {
            api.terminalResize(id, dims.cols, dims.rows).catch(() => {});
          }
        } catch {
          /* ignore */
        }
      });
      observer.observe(el);
      entry.observer = observer;
    }

    // 活跃 tab 聚焦 + 重算尺寸（切回已 open 的终端时需重新 focus 才能接收键盘）
    if (entry.opened) {
      try {
        entry.fit.fit();
      } catch {
        /* ignore */
      }
      entry.term.focus();
    }
  } finally {
    mountingIds.delete(id);
  }
}

/** 读取 fit 计算后的当前行列（fit() 会同步更新 term.cols / term.rows） */
function proposeDims(entry: TermEntry): { cols: number; rows: number } | null {
  const { cols, rows } = entry.term;
  if (cols > 0 && rows > 0) {
    return { cols, rows };
  }
  return null;
}

/** 挂载当前激活的终端 */
async function mountActive() {
  const id = activeId.value;
  if (!id) return;
  await nextTick();
  await mountTerm(id);
}

/** 销毁某个终端实例（释放 xterm + 订阅 + observer） */
function disposeTerm(id: string) {
  const entry = entries[id];
  if (!entry) return;
  entry.unlistenData?.();
  entry.unlistenExit?.();
  entry.observer?.disconnect();
  entry.term.dispose();
  delete entries[id];
}

/* ================ 交互操作 ================ */

async function switchTab(id: string) {
  activeId.value = id;
  await nextTick();
  await mountTerm(id);
}

async function openStopped(id: string) {
  try {
    await terminals.openExisting(id);
    await nextTick();
    await mountTerm(id);
  } catch (e) {
    message(String(e), 'error');
  }
}

async function restartTerm(id: string) {
  disposeTerm(id);
  try {
    await terminals.restart(id);
    await nextTick();
    await mountTerm(id);
  } catch (e) {
    message(String(e), 'error');
  }
}

async function closeTerm(id: string) {
  await terminals.close(id);
}

async function deleteTerm(id: string, index: number) {
  const name = terminals.displayName(id, index);
  if (!confirm(`确定删除终端「${name}」？配置将被移除。`)) return;
  disposeTerm(id);
  await terminals.removeConfig(id);
  // 切到第一个可用 tab
  await nextTick();
  const first = terminals.configs[0]?.id;
  if (first) await switchTab(first);
}

/* ---------- 行内重命名 ---------- */
const editingId = ref<string>('');
const editingName = ref('');

function startRename(id: string, current: string) {
  editingId.value = id;
  editingName.value = current;
}

async function commitRename() {
  const id = editingId.value;
  if (!id) return;
  await terminals.rename(id, editingName.value);
  editingId.value = '';
  editingName.value = '';
}

function cancelRename() {
  editingId.value = '';
  editingName.value = '';
}

/* ================ 生命周期 ================ */

onMounted(async () => {
  // 若没有终端，自动弹出新建对话框（首次使用引导）
  if (terminals.configs.length === 0) {
    openCreateDialog();
    return;
  }
  // 有配置则默认激活第一个并尝试挂载
  if (!activeId.value) {
    activeId.value = terminals.configs[0].id;
  }
  await nextTick();
  // 仅挂载已运行的终端（未运行的等用户点「打开」）
  const id = activeId.value;
  if (id && terminals.infos[id]?.running) {
    await mountTerm(id);
  }
});

onBeforeUnmount(() => {
  // 视图卸载时释放所有 xterm 实例（PTY 进程仍在后台运行，重新进入视图时重建）
  for (const id of Object.keys(entries)) {
    disposeTerm(id);
  }
});

// 活跃 tab 切换时自动挂载（覆盖 store 外部修改 activeId 的情况）
watch(activeId, async (id) => {
  if (!id) return;
  await nextTick();
  await mountTerm(id);
});

function logErr(tag: string, e: unknown) {
  // eslint-disable-next-line no-console
  console.error(`[console] ${tag} error:`, e);
}
</script>

<template>
  <div class="flex h-full flex-col">
    <!-- 顶部 tab 条 -->
    <div
      class="flex shrink-0 items-center gap-1 overflow-x-auto border-b border-slate-800 px-3 py-2"
    >
      <button
        v-for="(t, i) in tabs"
        :key="t.id"
        class="group flex shrink-0 items-center gap-2 rounded-md px-3 py-1 text-xs transition"
        :class="
          activeId === t.id
            ? 'bg-indigo-500/20 text-indigo-300'
            : 'text-slate-400 hover:bg-slate-800 hover:text-slate-200'
        "
        @click="switchTab(t.id)"
      >
        <span
          class="h-1.5 w-1.5 shrink-0 rounded-full"
          :class="t.running ? 'bg-emerald-400' : 'bg-slate-600'"
          :title="t.running ? '运行中' : '已停止'"
        />
        <!-- 名称 / 行内重命名 -->
        <input
          v-if="editingId === t.id"
          v-model="editingName"
          class="w-28 rounded border border-indigo-500 bg-slate-950 px-1 py-0.5 text-xs text-slate-200 focus:outline-none"
          @keyup.enter="commitRename"
          @keyup.esc="cancelRename"
          @blur="commitRename"
          @click.stop
        />
        <span v-else class="max-w-[10rem] truncate">{{ t.name }}</span>

        <!-- 行内操作按钮（hover 显示） -->
        <span class="ml-1 hidden items-center gap-0.5 group-hover:flex">
          <button
            v-if="!t.running"
            class="rounded p-0.5 text-slate-500 hover:text-emerald-400"
            title="打开"
            @click.stop="openStopped(t.id)"
          >
            <Play class="h-3 w-3" />
          </button>
          <button
            v-if="t.running"
            class="rounded p-0.5 text-slate-500 hover:text-amber-400"
            title="重启"
            @click.stop="restartTerm(t.id)"
          >
            <RotateCw class="h-3 w-3" />
          </button>
          <button
            v-if="t.running"
            class="rounded p-0.5 text-slate-500 hover:text-rose-400"
            title="停止"
            @click.stop="closeTerm(t.id)"
          >
            <Power class="h-3 w-3" />
          </button>
          <button
            class="rounded p-0.5 text-slate-500 hover:text-sky-400"
            title="重命名"
            @click.stop="startRename(t.id, t.name)"
          >
            <Pencil class="h-3 w-3" />
          </button>
          <button
            class="rounded p-0.5 text-slate-500 hover:text-rose-400"
            title="删除"
            @click.stop="deleteTerm(t.id, i)"
          >
            <Trash2 class="h-3 w-3" />
          </button>
        </span>
      </button>

      <button
        class="ml-1 flex shrink-0 items-center gap-1 rounded-md bg-indigo-500/15 px-2.5 py-1 text-xs text-indigo-300 transition hover:bg-indigo-500/25"
        @click="openCreateDialog"
      >
        <Plus class="h-3.5 w-3.5" /> 新建
      </button>

      <div v-if="tabs.length === 0" class="px-3 py-1 text-xs text-slate-600">
        暂无控制台，点击「新建」创建
      </div>
    </div>

    <!-- 终端主体区 -->
    <div ref="bodyRef" class="relative flex-1 overflow-hidden bg-slate-950">
      <!-- 每个终端一个容器，v-show 控制可见（保留 xterm buffer） -->
      <div
        v-for="t in tabs"
        :key="t.id"
        :ref="
          (el) => {
            containerRefs[t.id] = el as HTMLElement | null;
          }
        "
        class="h-full w-full p-1"
        :class="activeId === t.id ? 'block' : 'hidden'"
      >
        <!-- 未运行时的占位提示 -->
        <div
          v-if="!t.running"
          class="flex h-full flex-col items-center justify-center gap-3 text-slate-600"
        >
          <div class="text-sm">「{{ t.name }}」已停止</div>
          <button
            class="flex items-center gap-1 rounded-md bg-emerald-500/15 px-3 py-1.5 text-xs text-emerald-300 hover:bg-emerald-500/25"
            @click="openStopped(t.id)"
          >
            <Play class="h-3.5 w-3.5" /> 打开终端
          </button>
        </div>
      </div>

      <!-- 空状态 -->
      <div
        v-if="tabs.length === 0"
        class="flex h-full flex-col items-center justify-center gap-3 text-slate-600"
      >
        <div class="text-sm">暂无控制台会话</div>
        <button
          class="flex items-center gap-1 rounded-md bg-indigo-500/15 px-3 py-1.5 text-xs text-indigo-300 hover:bg-indigo-500/25"
          @click="openCreateDialog"
        >
          <Plus class="h-3.5 w-3.5" /> 新建控制台
        </button>
      </div>

      <!-- 浮动消息条 -->
      <Transition name="toast">
        <div
          v-if="toast"
          class="absolute bottom-4 left-1/2 z-50 flex max-w-md -translate-x-1/2 items-center gap-2 rounded-md border px-3 py-2 text-xs shadow-lg"
          :class="
            toast.type === 'error'
              ? 'border-rose-500/40 bg-rose-500/15 text-rose-300'
              : 'border-sky-500/40 bg-sky-500/15 text-sky-300'
          "
        >
          <span class="flex-1">{{ toast.text }}</span>
          <button class="opacity-60 hover:opacity-100" @click="toast = null">
            <X class="h-3.5 w-3.5" />
          </button>
        </div>
      </Transition>
    </div>

    <!-- 新建终端弹窗 -->
    <Teleport to="body">
      <div
        v-if="showCreate"
        class="fixed inset-0 z-[100] flex items-center justify-center bg-black/50"
        @click.self="showCreate = false"
      >
        <div
          class="w-[26rem] rounded-lg border border-slate-700 bg-slate-900 p-5 shadow-2xl"
        >
          <div class="mb-4 flex items-center justify-between">
            <h3 class="text-sm font-semibold text-slate-200">新建控制台</h3>
            <button
              class="text-slate-500 hover:text-slate-300"
              @click="showCreate = false"
            >
              <X class="h-4 w-4" />
            </button>
          </div>

          <div class="space-y-3">
            <!-- 名称 -->
            <div>
              <label class="mb-1 block text-xs text-slate-400">名称（可选）</label>
              <input
                v-model="form.name"
                placeholder="留空则自动编号"
                class="w-full rounded-md border border-slate-700 bg-slate-950 px-3 py-1.5 text-xs text-slate-200 placeholder:text-slate-600 focus:border-indigo-500 focus:outline-none"
                @keyup.enter="submitCreate"
              />
            </div>

            <!-- 工作目录 -->
            <div>
              <label class="mb-1 block text-xs text-slate-400">
                工作目录
                <span class="text-slate-600">（默认取当前工作目录）</span>
              </label>
              <div class="flex gap-2">
                <input
                  v-model="form.cwd"
                  placeholder="如 H:\自研项目\csgc\csgs-web-admin"
                  class="flex-1 rounded-md border border-slate-700 bg-slate-950 px-3 py-1.5 text-xs text-slate-200 placeholder:text-slate-600 focus:border-indigo-500 focus:outline-none"
                />
                <button
                  class="flex shrink-0 items-center gap-1 rounded-md border border-slate-700 bg-slate-800 px-2.5 py-1.5 text-xs text-slate-300 hover:bg-slate-700"
                  @click="pickCwd"
                >
                  <FolderOpen class="h-3.5 w-3.5" /> 浏览
                </button>
              </div>
            </div>

            <!-- Shell 选择 -->
            <div>
              <label class="mb-1 block text-xs text-slate-400">Shell</label>
              <select
                v-model="form.shell"
                class="w-full rounded-md border border-slate-700 bg-slate-950 px-3 py-1.5 text-xs text-slate-200 focus:border-indigo-500 focus:outline-none"
              >
                <option value="auto">自动（PowerShell 优先）</option>
                <option value="pwsh">PowerShell 7 (pwsh)</option>
                <option value="power_shell">Windows PowerShell</option>
                <option value="cmd">命令提示符 (cmd)</option>
              </select>
            </div>
          </div>

          <div class="mt-5 flex justify-end gap-2">
            <button
              class="rounded-md border border-slate-700 px-3 py-1.5 text-xs text-slate-400 hover:bg-slate-800"
              @click="showCreate = false"
            >
              取消
            </button>
            <button
              class="flex items-center gap-1 rounded-md bg-indigo-500 px-3 py-1.5 text-xs font-medium text-white hover:bg-indigo-600"
              @click="submitCreate"
            >
              <Plus class="h-3.5 w-3.5" /> 创建并启动
            </button>
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>

<style scoped>
/* xterm 容器需占满，深色背景 */
:deep(.xterm) {
  padding: 4px;
  height: 100%;
}
.toast-enter-active,
.toast-leave-active {
  transition: all 0.25s ease;
}
.toast-enter-from,
.toast-leave-to {
  opacity: 0;
  transform: translate(-50%, 8px);
}
</style>
