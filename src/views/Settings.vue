<script setup lang="ts">
/** 设置页：工作目录、Maven 目录、每个服务的 JDK 绑定 + 启动模式 + 端口 + JVM 堆参数 + 分组管理。
 *  服务配置按工作目录分组，展开后单独配置；高级参数（堆/profiles/额外参数）折叠隐藏。 */
import { onBeforeUnmount, onMounted, ref } from 'vue';
import {
  Save,
  FolderOpen,
  RefreshCw,
  ChevronDown,
  ChevronRight,
  Circle,
  Settings2,
  Plus,
  Trash2,
  ChevronUp,
  Wand2,
} from 'lucide-vue-next';
import { useSettingsStore } from '@/stores/settings';
import { useServicesStore } from '@/stores/services';
import { pickDirectory, saveConfig, scanMvn, scanServices } from '@/api/tauri';
import type { HeapRule, ServiceConfig, ServiceInfo } from '@/types';

const settings = useSettingsStore();
const services = useServicesStore();

/* ================ 基础设置：工作目录 ================ */

async function chooseWorkspace() {
  const dir = await pickDirectory();
  if (dir) {
    await settings.setWorkspace(dir);
    await services.scan();
  }
}

/* ================ Maven 目录 ================ */
const mvnCandidates = ref<string[]>([]);
const mvnScanning = ref(false);
const mvnDropdownOpen = ref(false);

async function scanMvnDir() {
  mvnScanning.value = true;
  try {
    const found = await scanMvn();
    mvnCandidates.value = found;
    if (found.length === 0) {
      services.setMessage('info', '未自动找到 Maven，请手动选择 Maven 的 bin 目录');
    } else if (found.length === 1 && !settings.mvnPath) {
      await settings.setMvnPath(found[0]);
      services.setMessage('success', `已自动设置 Maven：${found[0]}`);
    } else {
      mvnDropdownOpen.value = true;
      services.setMessage('info', `找到 ${found.length} 个 Maven，请选择`);
    }
  } catch (e) {
    services.setMessage('error', `Maven 扫描失败：${e}`);
  } finally {
    mvnScanning.value = false;
  }
}

async function pickMvnDir() {
  const dir = await pickDirectory();
  if (dir) {
    await settings.setMvnPath(dir.replace(/\\/g, '/'));
    services.setMessage('success', '已保存 Maven 目录');
  }
}

async function selectMvn(path: string) {
  await settings.setMvnPath(path);
  mvnDropdownOpen.value = false;
  services.setMessage('success', `已设置 Maven：${path}`);
}

function onDocClick(e: MouseEvent) {
  const target = e.target as HTMLElement;
  if (!target.closest('[data-mvn-dropdown]')) {
    mvnDropdownOpen.value = false;
  }
}
onMounted(() => document.addEventListener('click', onDocClick));
onBeforeUnmount(() => document.removeEventListener('click', onDocClick));

async function onMvnBlur() {
  await settings.setMvnPath(settings.mvnPath);
}

/* ================ 服务配置：按工作目录分组 ================ */

/** 单个服务的编辑副本（含 JVM 堆参数） */
interface SvcEdit {
  jdk_id: string;
  mode: 'mvn' | 'jar';
  port: number;
  enabled: boolean;
  extra_args: string;
  mvn_profiles: string;
  jvm_xms: string;
  jvm_xmx: string;
  jvm_metaspace: string;
  jvm_extra: string;
}

/** 每个工作目录的编辑状态：wsPath -> { edits, scanned, scanning } */
interface WsState {
  /** service_id -> edit */
  edits: Record<string, SvcEdit>;
  /** 扫描到的服务信息 */
  scanned: ServiceInfo[];
  /** 是否正在扫描 */
  scanning: boolean;
  /** 是否已扫描过 */
  scannedOnce: boolean;
}

const wsStates = ref<Record<string, WsState>>({});
/** 展开的工作目录集合 */
const expandedWs = ref<Set<string>>(new Set());
/** 展开高级参数的服务集合（key: wsPath::serviceId） */
const expandedAdvanced = ref<Set<string>>(new Set());

/** 生成高级参数折叠区的 key */
function advKey(wsPath: string, sid: string): string {
  return `${wsPath}::${sid}`;
}

/** 切换某服务高级参数折叠 */
function toggleAdvanced(wsPath: string, sid: string) {
  const k = advKey(wsPath, sid);
  if (expandedAdvanced.value.has(k)) {
    expandedAdvanced.value.delete(k);
  } else {
    expandedAdvanced.value.add(k);
  }
}

/** 确保某个工作目录的状态对象存在 */
function ensureWsState(wsPath: string): WsState {
  if (!wsStates.value[wsPath]) {
    const savedConfigs = settings.getWorkspaceServices(wsPath);
    const edits: Record<string, SvcEdit> = {};
    // 为已保存配置初始化编辑条目（扫描后会补充缺失的）
    for (const [sid, cfg] of Object.entries(savedConfigs)) {
      edits[sid] = svcEditFromConfig(cfg);
    }
    wsStates.value[wsPath] = {
      edits,
      scanned: [],
      scanning: false,
      scannedOnce: false,
    };
  }
  return wsStates.value[wsPath];
}

/** 从已保存 ServiceConfig 构造编辑副本（含 JVM 字段） */
function svcEditFromConfig(cfg: ServiceConfig): SvcEdit {
  return {
    jdk_id: cfg.jdk_id || '',
    mode: cfg.mode || 'mvn',
    port: cfg.port || 0,
    enabled: cfg.enabled ?? true,
    extra_args: cfg.extra_args || '',
    mvn_profiles: cfg.mvn_profiles || '',
    jvm_xms: cfg.jvm_xms || '',
    jvm_xmx: cfg.jvm_xmx || '',
    jvm_metaspace: cfg.jvm_metaspace || '',
    jvm_extra: cfg.jvm_extra || '',
  };
}

/** 从规则套用 JVM 参数到编辑副本（仅填充空字段） */
function applyRuleToEdit(e: SvcEdit, rule?: HeapRule) {
  if (!rule) return;
  if (!e.jvm_xms) e.jvm_xms = rule.jvm_xms;
  if (!e.jvm_xmx) e.jvm_xmx = rule.jvm_xmx;
  if (!e.jvm_metaspace) e.jvm_metaspace = rule.jvm_metaspace;
}

/** 从路径中提取短名称 */
function shortName(path: string): string {
  const parts = path.replace(/\\/g, '/').split('/').filter(Boolean);
  return parts[parts.length - 1] || path;
}

/** 切换工作目录展开/折叠 */
async function toggleWs(wsPath: string) {
  if (expandedWs.value.has(wsPath)) {
    expandedWs.value.delete(wsPath);
    return;
  }
  expandedWs.value.add(wsPath);
  // 首次展开时扫描服务
  const st = ensureWsState(wsPath);
  if (!st.scannedOnce && !st.scanning) {
    await scanWs(wsPath);
  }
}

/** 扫描某个工作目录的服务 */
async function scanWs(wsPath: string) {
  const st = ensureWsState(wsPath);
  st.scanning = true;
  try {
    const found = await scanServices(wsPath);
    st.scanned = found;
    st.scannedOnce = true;
    // 为扫描到的服务补充编辑条目（保留已有配置，缺失的用默认值）
    for (const s of found) {
      if (!st.edits[s.id]) {
        const saved = settings.getServiceConfig(wsPath, s.id);
        const edit = svcEditFromConfig(saved || {
          jdk_id: '',
          mode: 'mvn',
          port: s.port || 0,
          enabled: true,
          extra_args: '',
          mvn_profiles: '',
          jvm_xms: '',
          jvm_xmx: '',
          jvm_metaspace: '',
          jvm_extra: '',
        });
        // 规则启用且服务未配过堆参数 → 自动套用
        if (settings.heapRulesEnabled && !saved?.jvm_xmx) {
          applyRuleToEdit(edit, settings.matchHeapRule(s.artifact_id));
        }
        st.edits[s.id] = edit;
      }
    }
    // 移除已不在该工作目录的服务的编辑条目
    const foundIds = new Set(found.map((s) => s.id));
    for (const sid of Object.keys(st.edits)) {
      if (!foundIds.has(sid)) {
        delete st.edits[sid];
      }
    }
  } catch (e) {
    services.setMessage('error', `扫描 ${shortName(wsPath)} 失败：${e}`);
  } finally {
    st.scanning = false;
  }
}

/** 获取某个工作目录的服务列表（优先用已扫描的，否则空） */
function wsServices(wsPath: string): ServiceInfo[] {
  return wsStates.value[wsPath]?.scanned || [];
}

/** 某个工作目录下有多少个服务已启用 */
function wsEnabledCount(wsPath: string): number {
  const st = wsStates.value[wsPath];
  if (!st) return 0;
  return Object.values(st.edits).filter((e) => e.enabled).length;
}

/** 从编辑副本构造持久化 ServiceConfig */
function svcConfigFromEdit(e: SvcEdit): ServiceConfig {
  return {
    jdk_id: e.jdk_id,
    mode: e.mode,
    port: Number(e.port) || 0,
    enabled: e.enabled,
    extra_args: e.extra_args,
    mvn_profiles: e.mvn_profiles,
    jvm_xms: e.jvm_xms,
    jvm_xmx: e.jvm_xmx,
    jvm_metaspace: e.jvm_metaspace,
    jvm_extra: e.jvm_extra,
  };
}

/** 保存全部工作目录的服务配置 */
async function saveAllServices() {
  // 构建嵌套的 services 映射
  const servicesMap: Record<string, Record<string, ServiceConfig>> = {};
  // 包含已展开扫描过的工作目录
  for (const [wsPath, st] of Object.entries(wsStates.value)) {
    const wsMap: Record<string, ServiceConfig> = {};
    for (const [sid, e] of Object.entries(st.edits)) {
      wsMap[sid] = svcConfigFromEdit(e);
    }
    // 只保留有配置的工作目录
    if (Object.keys(wsMap).length > 0) {
      servicesMap[wsPath] = wsMap;
    }
  }
  // 同时保留未展开但有已有配置的工作目录
  for (const wsPath of settings.workspaces) {
    if (!servicesMap[wsPath]) {
      const saved = settings.getWorkspaceServices(wsPath);
      if (Object.keys(saved).length > 0) {
        servicesMap[wsPath] = saved;
      }
    }
  }
  await saveConfig({
    workspace: settings.workspace,
    workspaces: settings.workspaces,
    default_jdk_id: settings.defaultJdkId,
    workspace_jdks: settings.workspaceJdks,
    workspace_selections: settings.workspaceSelections,
    workspace_pins: settings.workspacePins,
    workspace_orders: settings.workspaceOrders,
    mvn_path: settings.mvnPath,
    jdks: settings.jdks,
    services: servicesMap as never,
    groups: settings.groups,
    heap_rules: settings.heapRules,
    heap_rules_enabled: settings.heapRulesEnabled,
    terminals: settings.terminals,
  });
  // 同步更新内存中的 services ref
  settings.services = servicesMap;
  services.setMessage('success', '已保存全部服务配置');
}

/* ================ 堆参数下拉控件辅助 ================ */

/** 堆大小预设档位 */
const HEAP_PRESETS = ['', '256m', '384m', '512m', '768m', '1g', '1.5g', '2g', '__custom__'];
const HEAP_PRESET_LABELS: Record<string, string> = {
  '': '不限制',
  __custom__: '自定义…',
};

/** 判断某值是否是预设值（非自定义） */
function isPreset(val: string): boolean {
  return val === '' || HEAP_PRESETS.includes(val);
}

/** 下拉 change：选 __custom__ 时不清空（保留原值供编辑） */
function onHeapSelect(e: SvcEdit, field: 'jvm_xms' | 'jvm_xmx' | 'jvm_metaspace', val: string) {
  if (val !== '__custom__') {
    e[field] = val;
  }
}

/** 当前选中值在下拉里的显示（非预设时显示自定义） */
function heapSelectValue(val: string): string {
  return isPreset(val) ? val : '__custom__';
}

/* ================ 堆内存默认规则管理 ================ */

const newRuleName = ref('');
const newRuleKeywords = ref('');

/** 新增规则 */
async function addRule() {
  if (!newRuleName.value.trim()) {
    services.setMessage('error', '请填写规则名称');
    return;
  }
  await settings.addHeapRule({
    id: crypto.randomUUID(),
    name: newRuleName.value.trim(),
    keywords: newRuleKeywords.value.trim(),
    jvm_xms: '256m',
    jvm_xmx: '768m',
    jvm_metaspace: '256m',
    builtin: false,
  });
  newRuleName.value = '';
  newRuleKeywords.value = '';
  services.setMessage('success', '已新增规则，请编辑参数');
}

/** 「应用到所有未配置服务」：把规则匹配结果填入所有未配 xmx 的服务 */
function applyRulesToAll() {
  let count = 0;
  for (const [, st] of Object.entries(wsStates.value)) {
    for (const [sid, e] of Object.entries(st.edits)) {
      if (!e.jvm_xmx) {
        const info = st.scanned.find((s) => s.id === sid);
        const rule = settings.matchHeapRule(info?.artifact_id || sid);
        if (rule) {
          applyRuleToEdit(e, rule);
          count++;
        }
      }
    }
  }
  services.setMessage(
    count > 0 ? 'success' : 'info',
    count > 0 ? `已为 ${count} 个服务套用默认规则` : '没有需要套用的服务（均已配置或无规则）',
  );
}

/** 切换规则启用开关 */
async function toggleRules() {
  await settings.setHeapRulesEnabled(!settings.heapRulesEnabled);
}

/** 初始化：自动展开当前工作目录 */
onMounted(async () => {
  if (settings.workspace) {
    await toggleWs(settings.workspace);
  }
});
</script>

<template>
  <div class="space-y-5 p-5">
    <!-- 基础设置 -->
    <section class="rounded-xl border border-slate-800 bg-slate-900/50 p-4">
      <h2 class="mb-3 text-sm font-semibold text-slate-200">基础</h2>
      <div class="space-y-3">
        <div>
          <label class="mb-1 block text-xs text-slate-500">工作目录</label>
          <div class="flex gap-2">
            <input
              :value="settings.workspace"
              readonly
              class="flex-1 rounded-md border border-slate-700 bg-slate-950 px-3 py-2 font-mono text-xs text-slate-400"
            />
            <button
              class="flex items-center gap-1 rounded-md bg-indigo-500 px-3 py-2 text-sm text-white hover:bg-indigo-600"
              @click="chooseWorkspace"
            >
              <FolderOpen class="h-4 w-4" /> 选择
            </button>
          </div>
        </div>
        <div>
          <label class="mb-1 block text-xs text-slate-500">
            Maven 目录（bin 所在目录，如 D:/apache-maven-3.6.3/bin）
          </label>
          <div class="flex gap-2">
            <input
              v-model="settings.mvnPath"
              class="flex-1 rounded-md border border-slate-700 bg-slate-950 px-3 py-2 font-mono text-xs text-slate-300 focus:border-indigo-500 focus:outline-none"
              placeholder="留空则用 PATH 中的 mvn；或点「扫描」/「选择」指定"
              @blur="onMvnBlur"
            />
            <!-- 扫描 + 下拉选择 -->
            <div class="relative" data-mvn-dropdown>
              <button
                class="flex items-center gap-1 rounded-md bg-indigo-500 px-3 py-2 text-sm text-white hover:bg-indigo-600 disabled:opacity-40"
                :disabled="mvnScanning"
                @click="scanMvnDir"
              >
                <RefreshCw class="h-4 w-4" :class="{ 'animate-spin': mvnScanning }" />
                扫描
              </button>
              <div
                v-if="mvnDropdownOpen && mvnCandidates.length > 0"
                class="absolute right-0 top-full z-30 mt-1 max-h-60 w-80 overflow-auto rounded-md border border-slate-700 bg-slate-900 py-1 shadow-xl"
              >
                <button
                  v-for="p in mvnCandidates"
                  :key="p"
                  class="block w-full truncate px-3 py-2 text-left font-mono text-xs text-slate-300 hover:bg-slate-800"
                  :title="p"
                  @click="selectMvn(p)"
                >
                  {{ p }}
                </button>
              </div>
            </div>
            <button
              class="flex items-center gap-1 rounded-md border border-slate-700 px-3 py-2 text-sm text-slate-300 hover:bg-slate-800"
              @click="pickMvnDir"
            >
              <FolderOpen class="h-4 w-4" /> 选择
            </button>
          </div>
          <div class="mt-1 flex items-center gap-1 text-[11px] text-slate-600">
            <ChevronDown class="h-3 w-3" />
            留空时使用系统 PATH 中的 mvn；配置目录后启动更可靠（避免 PATH 未配）
          </div>
        </div>
      </div>
    </section>

    <!-- 服务级配置：按工作目录分组 -->
    <section class="rounded-xl border border-slate-800 bg-slate-900/50 p-4">
      <div class="mb-3 flex items-center justify-between">
        <h2 class="text-sm font-semibold text-slate-200">
          服务配置
          <span class="ml-1 text-xs font-normal text-slate-500">
            （按工作目录分组，展开后单独配置；点 ⚙ 展开高级参数）
          </span>
        </h2>
        <button
          class="flex items-center gap-1 rounded-md bg-indigo-500 px-3 py-1.5 text-xs text-white hover:bg-indigo-600"
          @click="saveAllServices"
        >
          <Save class="h-3.5 w-3.5" /> 保存全部
        </button>
      </div>

      <div
        v-if="settings.workspaces.length === 0"
        class="py-8 text-center text-sm text-slate-600"
      >
        请先在「总览」页选择工作目录
      </div>

      <div v-else class="space-y-2">
        <div
          v-for="ws in [...settings.workspaces].reverse()"
          :key="ws"
          class="overflow-hidden rounded-md border border-slate-800 bg-slate-950/40"
        >
          <!-- 工作目录折叠头 -->
          <button
            class="flex w-full items-center gap-2 px-3 py-2.5 text-left transition hover:bg-slate-800/40"
            :class="{ 'bg-indigo-500/5': expandedWs.has(ws) }"
            @click="toggleWs(ws)"
          >
            <ChevronRight
              class="h-4 w-4 shrink-0 text-slate-500 transition"
              :class="{ 'rotate-90': expandedWs.has(ws) }"
            />
            <Circle
              class="h-2.5 w-2.5 shrink-0"
              :class="ws === settings.workspace ? 'text-indigo-400' : 'text-slate-600'"
              :fill="'currentColor'"
            />
            <div class="min-w-0 flex-1">
              <div class="flex items-center gap-2">
                <span class="truncate text-sm font-medium text-slate-300">
                  {{ shortName(ws) }}
                </span>
                <span
                  v-if="ws === settings.workspace"
                  class="shrink-0 rounded bg-indigo-500/20 px-1.5 py-0.5 text-[10px] text-indigo-300"
                >
                  当前
                </span>
                <span v-if="wsStates[ws]?.scannedOnce" class="shrink-0 text-[11px] text-slate-500">
                  {{ wsServices(ws).length }} 个服务，{{ wsEnabledCount(ws) }} 个启用
                </span>
                <span v-else-if="wsStates[ws]?.scanning" class="shrink-0 text-[11px] text-amber-400">
                  扫描中…
                </span>
              </div>
              <div class="truncate text-[11px] text-slate-600" :title="ws">{{ ws }}</div>
            </div>
            <!-- 重新扫描按钮 -->
            <button
              v-if="expandedWs.has(ws) && wsStates[ws]?.scannedOnce"
              class="flex shrink-0 items-center gap-1 rounded px-2 py-1 text-[11px] text-slate-400 hover:bg-slate-700/50 hover:text-slate-200"
              @click.stop="scanWs(ws)"
            >
              <RefreshCw class="h-3 w-3" :class="{ 'animate-spin': wsStates[ws]?.scanning }" />
              重新扫描
            </button>
          </button>

          <!-- 展开内容：服务列表 -->
          <div v-if="expandedWs.has(ws)" class="border-t border-slate-800/60">
            <!-- 扫描中 -->
            <div v-if="wsStates[ws]?.scanning" class="py-6 text-center text-xs text-slate-600">
              正在扫描服务…
            </div>
            <!-- 无服务 -->
            <div
              v-else-if="wsServices(ws).length === 0"
              class="py-6 text-center text-xs text-slate-600"
            >
              该工作目录下未扫描到可运行服务
            </div>
            <!-- 服务列表：表头 -->
            <div v-else>
              <div
                class="flex items-center gap-3 border-b border-slate-800/60 px-4 py-2 text-[11px] font-medium text-slate-500"
              >
                <div class="w-48 shrink-0">服务</div>
                <div class="w-40 shrink-0">JDK</div>
                <div class="w-24 shrink-0">模式</div>
                <div class="flex-1"></div>
                <div class="w-20 shrink-0 text-right">高级参数</div>
              </div>
              <!-- 服务行 -->
              <div
                v-for="s in wsServices(ws)"
                :key="s.id"
                class="border-b border-slate-800/40 last:border-0"
              >
                <!-- 核心行（始终可见） -->
                <div class="flex items-center gap-3 px-4 py-2">
                  <!-- 启用 + 服务名 -->
                  <div class="flex w-48 shrink-0 items-center gap-2 overflow-hidden">
                    <input
                      type="checkbox"
                      class="accent-indigo-500"
                      v-model="wsStates[ws].edits[s.id].enabled"
                    />
                    <span class="truncate font-mono text-xs text-slate-300" :title="s.id">
                      {{ s.artifact_id }}
                    </span>
                  </div>
                  <!-- JDK -->
                  <div class="w-40 shrink-0">
                    <select
                      v-model="wsStates[ws].edits[s.id].jdk_id"
                      class="w-full rounded border border-slate-700 bg-slate-950 px-2 py-1 text-xs text-slate-300"
                    >
                      <option value="">默认 JDK</option>
                      <option v-for="j in settings.jdks" :key="j.id" :value="j.id">
                        JDK {{ j.version }} ({{ j.vendor }})
                      </option>
                    </select>
                  </div>
                  <!-- 模式 -->
                  <div class="w-24 shrink-0">
                    <select
                      v-model="wsStates[ws].edits[s.id].mode"
                      class="w-full rounded border border-slate-700 bg-slate-950 px-2 py-1 text-xs text-slate-300"
                    >
                      <option value="mvn">mvn</option>
                      <option value="jar">java -jar</option>
                    </select>
                  </div>
                  <!-- 已配置的堆参数摘要（提示用） -->
                  <div class="min-w-0 flex-1 truncate text-[11px] text-slate-500">
                    <span v-if="wsStates[ws].edits[s.id].jvm_xmx" class="font-mono text-emerald-400/80">
                      -Xmx{{ wsStates[ws].edits[s.id].jvm_xmx }}
                    </span>
                    <span v-else class="text-slate-600">未设堆上限</span>
                  </div>
                  <!-- 高级参数切换 -->
                  <div class="w-20 shrink-0 text-right">
                    <button
                      class="inline-flex items-center gap-1 rounded px-2 py-1 text-[11px] transition"
                      :class="
                        expandedAdvanced.has(advKey(ws, s.id))
                          ? 'bg-indigo-500/20 text-indigo-300'
                          : 'text-slate-400 hover:bg-slate-700/50 hover:text-slate-200'
                      "
                      :title="expandedAdvanced.has(advKey(ws, s.id)) ? '收起高级参数' : '展开 JVM 参数等高级配置'"
                      @click="toggleAdvanced(ws, s.id)"
                    >
                      <Settings2 class="h-3.5 w-3.5" />
                    </button>
                  </div>
                </div>

                <!-- 高级参数折叠区 -->
                <div
                  v-if="expandedAdvanced.has(advKey(ws, s.id))"
                  class="bg-slate-950/50 px-4 py-3"
                >
                  <!-- JVM 内存参数 -->
                  <div class="mb-3">
                    <div class="mb-2 text-[11px] font-medium uppercase tracking-wide text-slate-500">
                      JVM 内存参数
                    </div>
                    <div class="grid grid-cols-2 gap-x-6 gap-y-3">
                      <!-- 初始堆 -Xms -->
                      <div>
                        <label class="mb-1 flex items-center gap-1.5 text-xs text-slate-400">
                          <span class="font-mono text-indigo-300">-Xms</span>
                          初始堆
                          <span class="text-[10px] text-slate-600">JVM 启动时预分配</span>
                        </label>
                        <div class="flex gap-2">
                          <select
                            :value="heapSelectValue(wsStates[ws].edits[s.id].jvm_xms)"
                            class="w-32 rounded border border-slate-700 bg-slate-950 px-2 py-1 text-xs text-slate-300"
                            @change="onHeapSelect(wsStates[ws].edits[s.id], 'jvm_xms', ($event.target as HTMLSelectElement).value)"
                          >
                            <option v-for="p in HEAP_PRESETS" :key="p" :value="p">
                              {{ HEAP_PRESET_LABELS[p] || p }}
                            </option>
                          </select>
                          <input
                            v-if="!isPreset(wsStates[ws].edits[s.id].jvm_xms)"
                            v-model="wsStates[ws].edits[s.id].jvm_xms"
                            class="w-24 rounded border border-slate-700 bg-slate-950 px-2 py-1 font-mono text-xs text-amber-300 focus:border-indigo-500 focus:outline-none"
                            placeholder="如 640m"
                          />
                        </div>
                      </div>
                      <!-- 最大堆 -Xmx -->
                      <div>
                        <label class="mb-1 flex items-center gap-1.5 text-xs text-slate-400">
                          <span class="font-mono text-indigo-300">-Xmx</span>
                          最大堆
                          <span class="text-[10px] text-amber-400/70">堆内存上限（最关键）</span>
                        </label>
                        <div class="flex gap-2">
                          <select
                            :value="heapSelectValue(wsStates[ws].edits[s.id].jvm_xmx)"
                            class="w-32 rounded border border-slate-700 bg-slate-950 px-2 py-1 text-xs text-slate-300"
                            @change="onHeapSelect(wsStates[ws].edits[s.id], 'jvm_xmx', ($event.target as HTMLSelectElement).value)"
                          >
                            <option v-for="p in HEAP_PRESETS" :key="p" :value="p">
                              {{ HEAP_PRESET_LABELS[p] || p }}
                            </option>
                          </select>
                          <input
                            v-if="!isPreset(wsStates[ws].edits[s.id].jvm_xmx)"
                            v-model="wsStates[ws].edits[s.id].jvm_xmx"
                            class="w-24 rounded border border-slate-700 bg-slate-950 px-2 py-1 font-mono text-xs text-amber-300 focus:border-indigo-500 focus:outline-none"
                            placeholder="如 1280m"
                          />
                        </div>
                      </div>
                      <!-- 元空间 -->
                      <div>
                        <label class="mb-1 flex items-center gap-1.5 text-xs text-slate-400">
                          <span class="font-mono text-indigo-300">MaxMetaspaceSize</span>
                          元空间
                          <span class="text-[10px] text-slate-600">JDK8+ 类元数据区</span>
                        </label>
                        <div class="flex gap-2">
                          <select
                            :value="heapSelectValue(wsStates[ws].edits[s.id].jvm_metaspace)"
                            class="w-32 rounded border border-slate-700 bg-slate-950 px-2 py-1 text-xs text-slate-300"
                            @change="onHeapSelect(wsStates[ws].edits[s.id], 'jvm_metaspace', ($event.target as HTMLSelectElement).value)"
                          >
                            <option v-for="p in HEAP_PRESETS" :key="p" :value="p">
                              {{ HEAP_PRESET_LABELS[p] || p }}
                            </option>
                          </select>
                          <input
                            v-if="!isPreset(wsStates[ws].edits[s.id].jvm_metaspace)"
                            v-model="wsStates[ws].edits[s.id].jvm_metaspace"
                            class="w-24 rounded border border-slate-700 bg-slate-950 px-2 py-1 font-mono text-xs text-amber-300 focus:border-indigo-500 focus:outline-none"
                            placeholder="如 320m"
                          />
                        </div>
                      </div>
                      <!-- 其他 JVM 参数 -->
                      <div>
                        <label class="mb-1 flex items-center gap-1.5 text-xs text-slate-400">
                          <span class="font-mono text-indigo-300">其他 JVM 参数</span>
                          <span class="text-[10px] text-slate-600">如 -XX:+UseG1GC</span>
                        </label>
                        <input
                          v-model="wsStates[ws].edits[s.id].jvm_extra"
                          class="w-full rounded border border-slate-700 bg-slate-950 px-2 py-1 font-mono text-xs text-slate-300 focus:border-indigo-500 focus:outline-none"
                          placeholder="如 -XX:+UseG1GC -XX:MaxDirectMemorySize=256m"
                        />
                      </div>
                    </div>
                  </div>

                  <!-- 启动参数 -->
                  <div>
                    <div class="mb-2 text-[11px] font-medium uppercase tracking-wide text-slate-500">
                      启动参数
                    </div>
                    <div class="grid grid-cols-3 gap-x-6 gap-y-3">
                      <!-- 端口 -->
                      <div>
                        <label class="mb-1 block text-xs text-slate-400">端口</label>
                        <input
                          v-model.number="wsStates[ws].edits[s.id].port"
                          type="number"
                          class="w-full rounded border border-slate-700 bg-slate-950 px-2 py-1 text-xs text-slate-300"
                          placeholder="端口"
                        />
                      </div>
                      <!-- Maven Profiles -->
                      <div>
                        <label class="mb-1 block text-xs text-slate-400">
                          Maven Profiles
                          <span class="text-[10px] text-slate-600">仅 mvn</span>
                        </label>
                        <input
                          v-model="wsStates[ws].edits[s.id].mvn_profiles"
                          class="w-full rounded border border-slate-700 bg-slate-950 px-2 py-1 text-xs text-slate-300"
                          :disabled="wsStates[ws].edits[s.id].mode !== 'mvn'"
                          :class="{
                            'cursor-not-allowed opacity-40':
                              wsStates[ws].edits[s.id].mode !== 'mvn',
                          }"
                          placeholder="如 SpringCloud"
                        />
                      </div>
                      <!-- Spring 应用参数 -->
                      <div>
                        <label class="mb-1 flex items-center gap-1 text-xs text-slate-400">
                          Spring 应用参数
                          <span class="text-[10px] text-slate-600">如 --server.port</span>
                        </label>
                        <input
                          v-model="wsStates[ws].edits[s.id].extra_args"
                          class="w-full rounded border border-slate-700 bg-slate-950 px-2 py-1 text-xs text-slate-300"
                          placeholder="如 --server.port=8001"
                        />
                      </div>
                    </div>
                  </div>
                </div>
              </div>
            </div>
          </div>
        </div>
      </div>
    </section>

    <!-- 堆内存默认规则 -->
    <section class="rounded-xl border border-slate-800 bg-slate-900/50 p-4">
      <div class="mb-3 flex items-center justify-between">
        <h2 class="text-sm font-semibold text-slate-200">
          堆内存默认规则
          <span class="ml-1 text-xs font-normal text-slate-500">
            （新扫描到的服务按服务名关键词自动套用 JVM 堆参数）
          </span>
        </h2>
        <div class="flex items-center gap-3">
          <button
            class="flex items-center gap-1 rounded-md border border-indigo-500/40 px-2.5 py-1.5 text-xs text-indigo-300 hover:bg-indigo-500/10"
            title="把规则匹配结果填入所有未配置堆参数的服务"
            @click="applyRulesToAll"
          >
            <Wand2 class="h-3.5 w-3.5" /> 应用到所有未配置服务
          </button>
          <label class="flex cursor-pointer items-center gap-1.5 text-xs text-slate-400">
            <input
              type="checkbox"
              class="accent-indigo-500"
              :checked="settings.heapRulesEnabled"
              @change="toggleRules"
            />
            启用自动套用
          </label>
        </div>
      </div>

      <!-- 规则列表 -->
      <div class="space-y-1.5">
        <div
          v-for="(r, idx) in settings.heapRules"
          :key="r.id"
          class="flex items-center gap-2 rounded-md border border-slate-800/60 bg-slate-950/40 px-3 py-2"
        >
          <!-- 优先级排序 -->
          <div class="flex shrink-0 flex-col">
            <button
              class="text-slate-600 hover:text-slate-300 disabled:opacity-20"
              :disabled="idx === 0"
              title="上移（提高优先级）"
              @click="settings.moveHeapRule(r.id, -1)"
            >
              <ChevronUp class="h-3.5 w-3.5" />
            </button>
            <button
              class="text-slate-600 hover:text-slate-300 disabled:opacity-20"
              :disabled="idx === settings.heapRules.length - 1"
              title="下移"
              @click="settings.moveHeapRule(r.id, 1)"
            >
              <ChevronDown class="h-3.5 w-3.5" />
            </button>
          </div>
          <!-- 名称 -->
          <div class="w-24 shrink-0">
            <input
              v-model="r.name"
              class="w-full rounded border border-transparent bg-transparent px-1 py-0.5 text-xs font-medium text-slate-300 hover:border-slate-700 focus:border-indigo-500 focus:bg-slate-950 focus:outline-none"
              :readonly="r.builtin && !r.keywords"
            />
          </div>
          <!-- 关键词 -->
          <div class="w-48 shrink-0">
            <input
              v-model="r.keywords"
              class="w-full rounded border border-transparent bg-transparent px-1 py-0.5 font-mono text-xs text-slate-400 hover:border-slate-700 focus:border-indigo-500 focus:bg-slate-950 focus:outline-none"
              :placeholder="r.keywords === '' ? '兜底（永远匹配）' : '逗号分隔'"
              :readonly="r.builtin && !r.keywords"
            />
          </div>
          <!-- 套用参数 -->
          <div class="flex shrink-0 items-center gap-1.5 font-mono text-[11px]">
            <span class="text-slate-600">Xms</span>
            <input
              v-model="r.jvm_xms"
              class="w-14 rounded border border-transparent bg-transparent px-1 py-0.5 text-xs text-emerald-400/90 hover:border-slate-700 focus:border-indigo-500 focus:bg-slate-950 focus:outline-none"
            />
            <span class="text-slate-600">Xmx</span>
            <input
              v-model="r.jvm_xmx"
              class="w-14 rounded border border-transparent bg-transparent px-1 py-0.5 text-xs text-emerald-400/90 hover:border-slate-700 focus:border-indigo-500 focus:bg-slate-950 focus:outline-none"
            />
            <span class="text-slate-600">Meta</span>
            <input
              v-model="r.jvm_metaspace"
              class="w-14 rounded border border-transparent bg-transparent px-1 py-0.5 text-xs text-emerald-400/90 hover:border-slate-700 focus:border-indigo-500 focus:bg-slate-950 focus:outline-none"
            />
          </div>
          <!-- 内置标签 -->
          <div class="flex-1"></div>
          <span
            v-if="r.builtin"
            class="shrink-0 rounded bg-slate-700/40 px-1.5 py-0.5 text-[10px] text-slate-500"
          >
            内置
          </span>
          <span
            v-if="!r.keywords"
            class="shrink-0 rounded bg-amber-500/10 px-1.5 py-0.5 text-[10px] text-amber-400/80"
          >
            兜底
          </span>
          <!-- 删除（内置不可删） -->
          <button
            v-if="!r.builtin"
            class="shrink-0 rounded p-1 text-slate-500 hover:bg-red-500/10 hover:text-red-400"
            title="删除规则"
            @click="settings.removeHeapRule(r.id)"
          >
            <Trash2 class="h-3.5 w-3.5" />
          </button>
        </div>
      </div>

      <!-- 新增规则 -->
      <div class="mt-3 flex items-center gap-2 border-t border-slate-800/60 pt-3">
        <input
          v-model="newRuleName"
          class="w-28 rounded border border-slate-700 bg-slate-950 px-2 py-1.5 text-xs text-slate-300 focus:border-indigo-500 focus:outline-none"
          placeholder="规则名"
          @keyup.enter="addRule"
        />
        <input
          v-model="newRuleKeywords"
          class="flex-1 rounded border border-slate-700 bg-slate-950 px-2 py-1.5 font-mono text-xs text-slate-300 focus:border-indigo-500 focus:outline-none"
          placeholder="关键词（逗号分隔，如 job,task）"
          @keyup.enter="addRule"
        />
        <button
          class="flex items-center gap-1 rounded-md bg-indigo-500 px-3 py-1.5 text-xs text-white hover:bg-indigo-600"
          @click="addRule"
        >
          <Plus class="h-3.5 w-3.5" /> 新增
        </button>
      </div>
      <div class="mt-2 text-[11px] text-slate-600">
        匹配规则：按列表从上到下，第一个命中的规则生效（关键词包含在服务名里即命中）；空关键词为兜底，永远命中。
      </div>
    </section>
  </div>
</template>
