<script setup lang="ts">
/** 日志查看器：左侧服务 tab，右侧终端风格流式日志。
 *  tab 覆盖所有工作目录中已启动过（或有日志缓冲）的服务。 */
import { computed, nextTick, ref, watch } from 'vue';
import { Search, Pause, Trash2, ArrowDownToLine } from 'lucide-vue-next';
import { useServicesStore, type LogLine } from '@/stores/services';

const services = useServicesStore();

/** 当前选中的服务 id */
const activeId = ref<string>('');
const search = ref('');
const paused = ref(false);
const pausedLines = ref<LogLine[] | null>(null);
const containerRef = ref<HTMLElement | null>(null);

/** 页面最多渲染的日志行数，历史日志仍保留在 store 中。 */
const DISPLAY_MAX_LINES = 300;

interface LogSegment {
  text: string;
  className: string;
}

/** 取工作目录短名（最后一段），用于 tab 后缀区分不同目录的同名服务。 */
function workspaceShort(ws: string): string {
  if (!ws) return '';
  const parts = ws.replace(/\\/g, '/').split('/').filter(Boolean);
  return parts[parts.length - 1] || ws;
}

/** 可看日志的服务列表：遍历所有运行态（含其他工作目录），
 *  展示已启动过（运行中/启动中/停止中/已崩溃，或有日志缓冲）的服务。 */
const tabs = computed(() => {
  const result: { id: string; label: string; count: number; workspace: string }[] = [];
  const seen = new Set<string>();
  for (const [id, rt] of Object.entries(services.runtimes)) {
    const hasLogs = (services.logs[id]?.length ?? 0) > 0;
    // 已停止且从未有日志 → 完全未启动过，不展示
    if (rt.status === 'stopped' && !hasLogs) continue;
    if (seen.has(id)) continue;
    seen.add(id);
    // 静态信息可能来自任意工作目录，兜底查找
    const info = services.getInfo(id, rt.workspace || undefined);
    const wsShort = workspaceShort(rt.workspace);
    result.push({
      id,
      label: info?.artifact_id ?? id,
      count: services.logs[id]?.length ?? 0,
      workspace: wsShort,
    });
  }
  return result;
});

/** 初始化：从 window 取 ServiceList 传过来的 id，否则取第一个 */
const win = window as unknown as { __active_log_id?: string };
watch(
  tabs,
  (list) => {
    if (list.length === 0) return;
    const fromWindow = win.__active_log_id;
    if (fromWindow && list.some((t) => t.id === fromWindow)) {
      activeId.value = fromWindow;
    } else if (!activeId.value && list.length > 0) {
      activeId.value = list[0].id;
    }
  },
  { immediate: true },
);

const allLines = computed(() => {
  if (!activeId.value) return [];
  return services.logs[activeId.value] ?? [];
});

/** 搜索过滤后的行 */
const filteredLines = computed(() => {
  if (!search.value) return allLines.value;
  const kw = search.value.toLowerCase();
  return allLines.value.filter((l) => l.text.toLowerCase().includes(kw));
});

const visibleLines = computed(() => filteredLines.value.slice(-DISPLAY_MAX_LINES));

/** 暂停时固定当前页面快照，避免新日志继续触发日志区域重绘。 */
const displayedLines = computed(() => pausedLines.value ?? visibleLines.value);

/** 滚动到底部 */
function scrollToBottom() {
  const el = containerRef.value;
  if (el) el.scrollTop = el.scrollHeight;
}

/** 可见日志批次变化时自动滚动到底部。 */
watch(
  () => displayedLines.value[displayedLines.value.length - 1]?.id,
  scrollToBottom,
  { flush: 'post' },
);

/** 切换服务时自动滚动到底部（不受暂停影响，始终展示最新日志） */
watch(activeId, () => scrollToBottom(), { flush: 'post' });

function clearCurrent() {
  if (!activeId.value) return;
  services.clearLog(activeId.value);
  if (paused.value) pausedLines.value = [];
}

function switchTab(id: string) {
  activeId.value = id;
  if (paused.value) pausedLines.value = visibleLines.value.slice();
  win.__active_log_id = id;
}

/** 暂停时冻结当前日志快照，恢复后跳到最新日志。 */
function togglePause() {
  if (!paused.value) {
    pausedLines.value = visibleLines.value.slice();
    paused.value = true;
    return;
  }
  paused.value = false;
  pausedLines.value = null;
  void nextTick(scrollToBottom);
}

const LEVEL_CLASSES: Record<string, string> = {
  TRACE: 'text-slate-500',
  DEBUG: 'text-violet-300',
  INFO: 'font-medium text-sky-300',
  WARN: 'font-medium text-amber-300',
  WARNING: 'font-medium text-amber-300',
  ERROR: 'font-semibold text-red-400',
  FATAL: 'font-semibold text-rose-400',
};

/** 按日志结构拆分字段，保留纯文本渲染，避免引入 HTML 注入风险。 */
function logSegments(line: { text: string; error: boolean }): LogSegment[] {
  const sections = line.text.split('|');
  const segments: LogSegment[] = [];

  sections.forEach((section, index) => {
    segments.push(...colorizeSection(section, index, line));
    if (index < sections.length - 1) {
      segments.push({ text: '|', className: 'text-slate-700' });
    }
  });
  return segments;
}

function colorizeSection(
  text: string,
  section: number,
  line: { text: string; error: boolean },
): LogSegment[] {
  const pattern = /\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}(?:\.\d+)?|\b(?:TRACE|DEBUG|INFO|WARN|WARNING|ERROR|FATAL)\b|\[[^\]]+\]|\b(?:[A-Za-z_$][\w$]*\.){2,}[A-Za-z_$][\w$]*\b/g;
  const segments: LogSegment[] = [];
  let start = 0;

  for (const match of text.matchAll(pattern)) {
    const offset = match.index ?? 0;
    if (offset > start) {
      segments.push({
        text: text.slice(start, offset),
        className: sectionClass(section, line),
      });
    }
    segments.push({ text: match[0], className: tokenClass(match[0], section) });
    start = offset + match[0].length;
  }
  if (start < text.length) {
    segments.push({ text: text.slice(start), className: sectionClass(section, line) });
  }
  return segments;
}

function tokenClass(token: string, section: number) {
  const level = token.replace(/^\[|\]$/g, '');
  if (LEVEL_CLASSES[level]) return LEVEL_CLASSES[level];
  if (/^\d{4}-\d{2}-\d{2}/.test(token)) return 'text-slate-500';
  if (/^(?:[A-Za-z_$][\w$]*\.){2,}/.test(token)) return 'text-emerald-300';
  if (token.startsWith('[')) return 'text-amber-200';
  return section === 2 ? 'text-amber-200' : 'text-slate-300';
}

function sectionClass(section: number, line: { text: string; error: boolean }) {
  if (section === 0 && line.text.includes('|')) return 'text-slate-500';
  if (section === 1) return 'text-slate-400';
  if (section === 2) return 'text-amber-200';
  if (line.error || /\b(?:ERROR|FATAL)\b/.test(line.text)) return 'text-red-300';
  if (/Started .*Application|Started .*App /.test(line.text)) return 'text-emerald-300';
  return 'text-slate-200';
}

function lineClass(line: { text: string; error: boolean }) {
  if (line.error || /\b(?:ERROR|FATAL)\b/.test(line.text)) return 'bg-red-950/20';
  if (/\bWARN(?:ING)?\b/.test(line.text)) return 'bg-amber-950/15';
  if (/Started .*Application|Started .*App /.test(line.text)) return 'bg-emerald-950/15';
  return '';
}
</script>

<template>
  <div class="flex h-full flex-col">
    <!-- 服务 tab 条 -->
    <div
      class="flex shrink-0 gap-1 overflow-x-auto border-b border-slate-800 px-3 py-2"
    >
      <button
        v-for="t in tabs"
        :key="t.id"
        class="shrink-0 rounded-md px-3 py-1 text-xs transition"
        :class="
          activeId === t.id
            ? 'bg-indigo-500/20 text-indigo-300'
            : 'text-slate-500 hover:bg-slate-800 hover:text-slate-300'
        "
        @click="switchTab(t.id)"
      >
        {{ t.label }}
        <span v-if="t.workspace" class="ml-1 text-[10px] text-slate-600">
          ({{ t.workspace }})
        </span>
        <span v-if="t.count" class="ml-1 text-[10px] text-slate-600">
          {{ t.count }}
        </span>
      </button>
      <div
        v-if="tabs.length === 0"
        class="px-3 py-1 text-xs text-slate-600"
      >
        暂无服务日志
      </div>
    </div>

    <!-- 工具条 -->
    <div
      class="flex shrink-0 items-center gap-2 border-b border-slate-800 px-4 py-2"
    >
      <div class="relative flex-1 max-w-sm">
        <Search class="absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-slate-500" />
        <input
          v-model="search"
          placeholder="过滤日志…"
          class="w-full rounded-md border border-slate-700 bg-slate-950 py-1.5 pl-7 pr-2 text-xs text-slate-300 placeholder:text-slate-600 focus:border-indigo-500 focus:outline-none"
        />
      </div>
      <button
        class="flex items-center gap-1 rounded-md px-2 py-1.5 text-xs"
        :class="
          paused
            ? 'bg-amber-500/15 text-amber-300'
            : 'text-slate-400 hover:bg-slate-800'
        "
        @click="togglePause"
      >
        <Pause class="h-3.5 w-3.5" /> {{ paused ? '已暂停' : '暂停刷新' }}
      </button>
      <button
        class="flex items-center gap-1 rounded-md px-2 py-1.5 text-xs text-slate-400 hover:bg-slate-800"
        @click="scrollToBottom"
      >
        <ArrowDownToLine class="h-3.5 w-3.5" /> 底部
      </button>
      <button
        class="flex items-center gap-1 rounded-md px-2 py-1.5 text-xs text-slate-400 hover:bg-slate-800"
        @click="clearCurrent"
      >
        <Trash2 class="h-3.5 w-3.5" /> 清屏
      </button>
    </div>

    <!-- 日志内容 -->
    <div
      ref="containerRef"
      class="flex-1 overflow-auto bg-slate-950 px-3 py-2 font-mono text-xs leading-relaxed"
    >
      <div
        v-if="displayedLines.length === 0"
        class="flex h-full items-center justify-center text-slate-700"
      >
        {{ activeId ? '等待日志输出…' : '请选择一个服务' }}
      </div>
      <div
        v-for="line in displayedLines"
        :key="line.id"
        v-memo="[line.id]"
        class="-mx-1 whitespace-pre-wrap break-all rounded-sm px-1"
        :class="lineClass(line)"
      >
        <span
          v-for="(segment, segmentIndex) in logSegments(line)"
          :key="segmentIndex"
          :class="segment.className"
        >{{ segment.text }}</span>
      </div>
    </div>
  </div>
</template>
