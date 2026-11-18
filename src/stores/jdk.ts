/**
 * JDK 管理 store —— 扫描、增删、设默认。
 * JDK 列表本身存在 settings store 里（因为要持久化），这里封装操作行为。
 */
import { defineStore } from 'pinia';
import { useSettingsStore } from './settings';
import * as api from '@/api/tauri';
import type { JdkInfo } from '@/types';

export const useJdkStore = defineStore('jdk', () => {
  const settings = useSettingsStore();

  /** 触发一次系统扫描，合并到已有列表（去重） */
  async function scan() {
    const found = await api.scanJdks();
    // 按 path 去重：保留 manual 的、或保留版本号解析成功的
    const byPath = new Map<string, JdkInfo>();
    for (const j of settings.jdks) byPath.set(j.path, j);
    for (const j of found) {
      const exist = byPath.get(j.path);
      if (!exist) {
        byPath.set(j.path, j);
      } else if (exist.manual) {
        // 手动添加的优先，不覆盖
      } else {
        byPath.set(j.path, j);
      }
    }
    settings.jdks = Array.from(byPath.values()).sort((a, b) =>
      a.version.localeCompare(b.version, undefined, { numeric: true }),
    );
    await settings.persist();
  }

  async function add(path: string) {
    const jdk = await api.addJdk(path);
    // 避免重复
    if (!settings.jdks.some((j) => j.path === jdk.path)) {
      settings.jdks.push(jdk);
      await settings.persist();
    }
    return jdk;
  }

  async function remove(id: string) {
    const before = settings.jdks.length;
    settings.jdks = settings.jdks.filter((j) => j.id !== id);
    if (settings.defaultJdkId === id) settings.defaultJdkId = '';
    // 清理工作目录级默认中对该 JDK 的引用
    const wsNext: Record<string, string> = {};
    for (const [ws, jdkId] of Object.entries(settings.workspaceJdks)) {
      if (jdkId !== id) wsNext[ws] = jdkId;
    }
    settings.workspaceJdks = wsNext;
    if (settings.jdks.length !== before) await settings.persist();
  }

  /** 根据 id 查 JDK；空 id 返回当前工作目录默认 JDK，再回退到全局默认 */
  function findById(id: string): JdkInfo | undefined {
    if (id) return settings.jdks.find((j) => j.id === id);
    const wsJdkId = settings.currentWorkspaceJdkId();
    if (wsJdkId) {
      const wsJdk = settings.jdks.find((j) => j.id === wsJdkId);
      if (wsJdk) return wsJdk;
    }
    return settings.jdks.find((j) => j.id === settings.defaultJdkId);
  }

  return { scan, add, remove, findById };
});
