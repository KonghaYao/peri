/** 监控面板的纯格式化；不保留任何原始数据，只把已去敏数值转成可读文本。 */

const BYTE_UNITS = ["B", "KiB", "MiB", "GiB", "TiB"] as const;

function pad(value: number): string {
  return String(value).padStart(2, "0");
}

/** 字节数按 1024 进制展示；非法值显式显示 `—`。 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < BYTE_UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const rounded = unit === 0 ? String(Math.round(value)) : value.toFixed(1).replace(/\.0$/, "");
  return `${rounded} ${BYTE_UNITS[unit]}`;
}

/** 秒数 → 天/小时/分/秒；用于进程 uptime 与 sandbox 运行时长。 */
export function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "—";
  const total = Math.floor(seconds);
  const days = Math.floor(total / 86_400);
  const hours = Math.floor((total % 86_400) / 3_600);
  const minutes = Math.floor((total % 3_600) / 60);
  const rest = total % 60;
  const parts: string[] = [];
  if (days > 0) parts.push(`${days} 天`);
  if (hours > 0) parts.push(`${hours} 小时`);
  if (minutes > 0) parts.push(`${minutes} 分`);
  if (rest > 0 || parts.length === 0) parts.push(`${rest} 秒`);
  return parts.join(" ");
}

/** 两个 ISO 时间之间的运行时长；任一无法解析时返回 `—`。 */
export function formatElapsed(fromIso: string, toIso: string): string {
  const from = Date.parse(fromIso);
  const to = Date.parse(toIso);
  if (!Number.isFinite(from) || !Number.isFinite(to)) return "—";
  return formatDuration(Math.max(0, (to - from) / 1000));
}

/** 本地时钟 `HH:MM:SS`；用于「更新时间」。 */
export function formatClock(date: Date | null): string {
  if (date === null || Number.isNaN(date.getTime())) return "—";
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

/** 本地日期时间 `YYYY-MM-DD HH:MM:SS`；用于采样时间。 */
export function formatDateTime(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "—";
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`
    + ` ${formatClock(date)}`;
}
