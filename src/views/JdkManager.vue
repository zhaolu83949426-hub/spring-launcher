<script setup lang="ts">
/** JDK 管理页：列表 + 重新扫描 + 手动添加 + 设默认 + 删除
 *  支持两级默认：全局默认 JDK + 工作目录级默认 JDK */
import { ref, computed } from 'vue';
import { RefreshCw, Plus, Star, Trash2, FolderOpen, Globe, FolderTree } from 'lucide-vue-next';
import { useJdkStore } from '@/stores/jdk';
import { useSettingsStore } from '@/stores/settings';
import { pickDirectory } from '@/api/tauri';

const jdk = useJdkStore();
const settings = useSettingsStore();

const adding = ref(false);

async function scan() {
  adding.value = true;
  try {
    await jdk.scan();
  } finally {
    adding.value = false;
  }
}

async function addManual() {
  const dir = await pickDirectory();
  if (dir) {
    adding.value = true;
    try {
      await jdk.add(dir);
    } finally {
      adding.value = false;
    }
  }
}

async function setDefault(id: string) {
  await settings.setDefaultJdk(id);
}

async function remove(id: string) {
  await jdk.remove(id);
}

/** 从版本号推算主版本，便于一眼区分 8 / 17 */
function major(v: string): string {
  // 1.8.0_xxx -> 8，17.0.9 -> 17
  if (v.startsWith('1.8')) return '8';
  return v.split('.')[0];
}

/* ---- 工作目录级默认 JDK ---- */

/** 带工作目录信息的列表（最近使用的排前面） */
const workspaceRows = computed(() => {
  return [...settings.workspaces].reverse().map((path) => {
    const jdkId = settings.workspaceJdks[path] || '';
    const jdkInfo = jdkId ? settings.jdks.find((j) => j.id === jdkId) : undefined;
    return {
      path,
      jdkId,
      jdkVersion: jdkInfo?.version ?? '',
      isCurrent: path === settings.workspace,
    };
  });
});

async function setWorkspaceJdk(path: string, jdkId: string) {
  await settings.setWorkspaceJdk(path, jdkId);
}

/** 从路径中提取短名称 */
function shortName(path: string): string {
  const parts = path.replace(/\\/g, '/').split('/').filter(Boolean);
  return parts[parts.length - 1] || path;
}
</script>

<template>
  <div class="space-y-4 p-5">
    <div class="flex items-center justify-between">
      <div>
        <h2 class="text-base font-semibold text-slate-200">JDK 管理</h2>
        <p class="text-xs text-slate-500">
          自动扫描系统已安装的 JDK，也可手动添加（如 IDEA 托管的 .jdks 目录）
        </p>
      </div>
      <div class="flex gap-2">
        <button
          class="flex items-center gap-1.5 rounded-md bg-indigo-500 px-3 py-2 text-sm text-white hover:bg-indigo-600 disabled:opacity-40"
          :disabled="adding"
          @click="scan"
        >
          <RefreshCw class="h-4 w-4" :class="{ 'animate-spin': adding }" />
          重新扫描
        </button>
        <button
          class="flex items-center gap-1.5 rounded-md border border-slate-700 px-3 py-2 text-sm text-slate-300 hover:bg-slate-800"
          @click="addManual"
        >
          <Plus class="h-4 w-4" /> 手动添加
        </button>
      </div>
    </div>

    <div
      v-if="settings.jdks.length === 0"
      class="rounded-xl border border-dashed border-slate-700 p-12 text-center text-slate-500"
    >
      尚未发现任何 JDK，点击「重新扫描」自动查找，或「手动添加」指定路径
    </div>

    <div
      v-else
      class="overflow-hidden rounded-xl border border-slate-800 bg-slate-900/50"
    >
      <table class="w-full text-sm">
        <thead>
          <tr class="border-b border-slate-800 text-left text-xs text-slate-500">
            <th class="px-4 py-3 font-medium">主版本</th>
            <th class="px-4 py-3 font-medium">完整版本</th>
            <th class="px-4 py-3 font-medium">厂商</th>
            <th class="px-4 py-3 font-medium">路径</th>
            <th class="px-4 py-3 font-medium">来源</th>
            <th class="px-4 py-3 text-right font-medium">操作</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="j in settings.jdks"
            :key="j.id"
            class="border-b border-slate-800/60 last:border-0 hover:bg-slate-800/30"
          >
            <td class="px-4 py-3">
              <span
                class="inline-flex h-7 w-7 items-center justify-center rounded-md bg-indigo-500/15 text-xs font-bold text-indigo-300"
              >
                {{ major(j.version) }}
              </span>
            </td>
            <td class="px-4 py-3 font-mono text-xs text-slate-300">
              {{ j.version }}
            </td>
            <td class="px-4 py-3 text-xs text-slate-400">{{ j.vendor }}</td>
            <td class="px-4 py-3 font-mono text-[11px] text-slate-500">
              <span class="truncate inline-block max-w-md align-middle" :title="j.path">
                {{ j.path }}
              </span>
            </td>
            <td class="px-4 py-3 text-xs">
              <span
                :class="
                  j.manual
                    ? 'text-sky-300'
                    : 'text-slate-500'
                "
              >
                {{ j.manual ? '手动' : '扫描' }}
              </span>
            </td>
            <td class="px-4 py-3">
              <div class="flex justify-end gap-1">
                <button
                  class="rounded p-1.5 hover:bg-slate-700/50"
                  :class="
                    settings.defaultJdkId === j.id
                      ? 'text-amber-300'
                      : 'text-slate-500'
                  "
                  :title="
                    settings.defaultJdkId === j.id
                      ? '当前默认'
                      : '设为默认'
                  "
                  @click="setDefault(j.id)"
                >
                  <Star
                    class="h-4 w-4"
                    :fill="settings.defaultJdkId === j.id ? 'currentColor' : 'none'"
                  />
                </button>
                <button
                  class="rounded p-1.5 text-red-400 hover:bg-red-500/10"
                  title="删除"
                  @click="remove(j.id)"
                >
                  <Trash2 class="h-4 w-4" />
                </button>
              </div>
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <!-- 默认 JDK 提示 -->
    <div
      v-if="settings.defaultJdkId"
      class="flex items-center gap-2 rounded-md border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-xs text-amber-200/80"
    >
      <FolderOpen class="h-4 w-4" />
      当前默认 JDK：{{ settings.jdks.find((j) => j.id === settings.defaultJdkId)?.path }}
      （未单独配置 JDK 的服务将使用它）
    </div>

    <!-- 工作目录级默认 JDK -->
    <div class="rounded-xl border border-slate-800 bg-slate-900/50 p-4">
      <div class="mb-3 flex items-center gap-2">
        <FolderTree class="h-4 w-4 text-indigo-400" />
        <h3 class="text-sm font-semibold text-slate-200">工作目录默认 JDK</h3>
      </div>
      <p class="mb-3 text-xs text-slate-500">
        为每个工作目录单独指定默认 JDK。启动服务时优先级：
        <span class="text-slate-400">服务级 &gt; 工作目录级 &gt; 全局默认</span>
      </p>

      <div
        v-if="workspaceRows.length === 0"
        class="py-6 text-center text-sm text-slate-600"
      >
        暂无工作目录，请先在「总览」页选择工作目录
      </div>

      <div v-else class="space-y-2">
        <div
          v-for="row in workspaceRows"
          :key="row.path"
          class="flex items-center gap-3 rounded-md border px-3 py-2"
          :class="
            row.isCurrent
              ? 'border-indigo-500/40 bg-indigo-500/5'
              : 'border-slate-800 bg-slate-950/40'
          "
        >
          <div class="min-w-0 flex-1">
            <div class="flex items-center gap-1.5">
              <span class="truncate text-xs font-medium text-slate-300" :title="row.path">
                {{ shortName(row.path) }}
              </span>
              <span
                v-if="row.isCurrent"
                class="rounded bg-indigo-500/20 px-1.5 py-0.5 text-[10px] text-indigo-300"
              >
                当前
              </span>
            </div>
            <div class="truncate font-mono text-[10px] text-slate-600" :title="row.path">
              {{ row.path }}
            </div>
          </div>
          <select
            :value="row.jdkId"
            class="w-56 rounded border border-slate-700 bg-slate-950 px-2 py-1 text-xs text-slate-300 focus:border-indigo-500 focus:outline-none"
            @change="setWorkspaceJdk(row.path, ($event.target as HTMLSelectElement).value)"
          >
            <option value="">跟随全局默认</option>
            <option
              v-for="j in settings.jdks"
              :key="j.id"
              :value="j.id"
            >
              JDK {{ j.version }} ({{ j.vendor }})
            </option>
          </select>
        </div>
      </div>

      <!-- 当前工作目录生效 JDK 汇总 -->
      <div
        v-if="settings.workspace"
        class="mt-3 flex items-center gap-2 rounded-md border border-slate-700/50 bg-slate-950/40 px-3 py-2 text-xs text-slate-400"
      >
        <Globe class="h-3.5 w-3.5 text-slate-500" />
        <span>
          当前工作目录生效 JDK：
          <span class="text-slate-200">
            {{
              (settings.workspaceJdks[settings.workspace] &&
                settings.jdks.find((j) => j.id === settings.workspaceJdks[settings.workspace])?.version) ||
              (settings.defaultJdkId &&
                settings.jdks.find((j) => j.id === settings.defaultJdkId)?.version) ||
              '未配置'
            }}
          </span>
        </span>
      </div>
    </div>
  </div>
</template>
