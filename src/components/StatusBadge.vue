<script setup lang="ts">
/** 状态徽章：圆点 + 文字，颜色随状态变化 */
import type { ServiceStatus } from '@/types';

const props = defineProps<{ status: ServiceStatus }>();

const map: Record<ServiceStatus, { dot: string; text: string; label: string }> = {
  stopped: { dot: 'bg-slate-500', text: 'text-slate-400', label: '已停止' },
  starting: {
    dot: 'bg-amber-400 animate-pulse',
    text: 'text-amber-300',
    label: '启动中',
  },
  running: { dot: 'bg-emerald-400', text: 'text-emerald-300', label: '运行中' },
  stopping: {
    dot: 'bg-orange-400 animate-pulse',
    text: 'text-orange-300',
    label: '停止中',
  },
  crashed: { dot: 'bg-red-500', text: 'text-red-400', label: '已崩溃' },
};
</script>

<template>
  <span class="inline-flex items-center gap-1.5 text-xs" :class="map[props.status].text">
    <span class="h-2 w-2 rounded-full" :class="map[props.status].dot"></span>
    {{ map[props.status].label }}
  </span>
</template>
