/**
 * 通用工具函数。
 */

/**
 * 格式化内存占用：≥ 1GB 用 G（保留两位小数），否则用 M（取整）。
 * 0 或负数返回 '-'。
 */
export function formatMemory(bytes: number): string {
  if (!bytes || bytes <= 0) return '-';
  const mb = bytes / (1024 * 1024);
  if (mb >= 1024) {
    return `${(mb / 1024).toFixed(2)} G`;
  }
  return `${Math.round(mb)} M`;
}
