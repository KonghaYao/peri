import type { ChatInstanceObservation } from '../../shared/instances';
import type { WasmResourceSnapshot } from '../../shared/resources';
import { CF_WASM_MAX_MEMORY_BYTES } from '../../shared/runtime-limits';

export const phaseLabels: Record<WasmResourceSnapshot['phase'], string> = {
  loading: '加载中',
  starting: '启动中',
  ready: '就绪',
  closing: '关闭中',
  closed: '已关闭',
  failed: '启动失败',
  'close-unconfirmed': '关闭未确认',
};

export const failureLabels: Record<NonNullable<ChatInstanceObservation['failure']>, string> = {
  'host-unreachable': '无法读取该会话的实例观测：宿主未响应或不可达。',
  'invalid-observation': '该会话返回的实例观测不可信，已忽略。',
};

export interface StartupStage {
  key: 'module' | 'native' | 'acp';
  label: string;
  valueMs: number | null;
}

export function startupStages(instance: WasmResourceSnapshot): StartupStage[] {
  return [
    { key: 'module', label: '模块加载', valueMs: instance.startup.moduleReadyMs },
    { key: 'native', label: '原生启动', valueMs: instance.startup.nativeReadyMs },
    { key: 'acp', label: 'ACP 就绪', valueMs: instance.startup.acpReadyMs },
  ];
}

// 所有卡片共用一个刻度，跨实例的启动耗时才能互相比较；无观测时保留一个最小刻度。
export function startupScale(observations: ChatInstanceObservation[]): number {
  const values = observations.flatMap((entry) => entry.observation?.instance
    ? startupStages(entry.observation.instance).map((stage) => stage.valueMs ?? 0) : []);
  return Math.max(100, ...values);
}

export function formatStartupMs(valueMs: number | null): string {
  return valueMs === null ? '未记录' : `${valueMs} ms`;
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / 1024 / 1024).toFixed(2)} MiB`;
}

export function memoryPercent(bytes: number): number {
  return Math.min(100, Math.max(0, (bytes / CF_WASM_MAX_MEMORY_BYTES) * 100));
}

export function formatClock(iso: string): string {
  const timestamp = Date.parse(iso);
  if (Number.isNaN(timestamp)) return '—';
  return new Date(timestamp).toLocaleTimeString('zh-CN', { hour12: false });
}

export function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.max(0, Math.round(ms))} ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(seconds < 10 ? 1 : 0)} 秒`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} 分 ${Math.round(seconds % 60)} 秒`;
  return `${Math.floor(minutes / 60)} 小时 ${minutes % 60} 分`;
}

export function lifetimeMs(instance: WasmResourceSnapshot, now: number): number {
  const startedAt = Date.parse(instance.startedAt);
  const endedAt = instance.endedAt ? Date.parse(instance.endedAt) : now;
  if (Number.isNaN(startedAt) || Number.isNaN(endedAt)) return 0;
  return Math.max(0, endedAt - startedAt);
}

export function relativeTime(iso: string, now: number): string {
  const delta = now - Date.parse(iso);
  if (Number.isNaN(delta) || delta < 1000) return '刚刚';
  if (delta < 60_000) return `${Math.round(delta / 1000)} 秒前`;
  if (delta < 3_600_000) return `${Math.floor(delta / 60_000)} 分钟前`;
  if (delta < 86_400_000) return `${Math.floor(delta / 3_600_000)} 小时前`;
  return `${Math.floor(delta / 86_400_000)} 天前`;
}

export interface ObservationSummary {
  chats: number;
  observed: number;
  running: number;
  blocked: number;
  unreachable: number;
}

export function summarize(observations: ChatInstanceObservation[]): ObservationSummary {
  return {
    chats: observations.length,
    observed: observations.filter((entry) => entry.observation?.instance).length,
    running: observations.filter((entry) => entry.observation?.running).length,
    blocked: observations.filter((entry) => entry.observation?.executionBlocked).length,
    unreachable: observations.filter((entry) => entry.failure === 'host-unreachable').length,
  };
}
