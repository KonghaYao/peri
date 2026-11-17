/**
 * 这里的 9 个 pin code 与 canSleep 公式是唯一权威，不再兼容旧稿。
 * pins 只是对同一批事实的解释；canSleep 仍独立校验 counters 与 closing，避免单一字段撒谎。
 */
export const ACTIVITY_PIN_CODES = [
  'root_running',
  'active_internal_tasks',
  'queued_inputs',
  'pending_deliveries',
  'pending_interactions',
  'local_operations',
  'observation_incomplete',
  'closing',
  'activity_source_missing',
] as const;

export type ActivityPinCode = (typeof ACTIVITY_PIN_CODES)[number];

/** sleep 是宿主生命周期决定，不属于活动观测，因此这里只有活跃/空闲两态。 */
export type ActivityState = 'active' | 'idle';

export interface ActivityCounters {
  readonly rootRunning: boolean;
  readonly activeInternalTasks: number;
  readonly queuedInputs: number;
  readonly pendingDeliveries: number;
  readonly pendingInteractions: number;
  readonly localOperations: number;
}

/** 当前持有的 keep-awake；`since` 用于判断 hold 是否被重复获取。 */
export interface ActivityHold {
  readonly since: string;
}

/**
 * 一次完整的活动观测。
 *
 * `complete=false` 表示数据源无法观测全部计数，此时无论计数是否为零都不能休眠；关闭中的实例
 * 同样必须驻留到已接受的工作收尾。时间字段均为 ISO 8601，仅用于诊断与展示。
 */
export interface ActivitySnapshot {
  readonly state: ActivityState;
  readonly complete: boolean;
  readonly closing: boolean;
  readonly counters: ActivityCounters;
  readonly pins: readonly ActivityPinCode[];
  readonly hold: ActivityHold | null;
  readonly observedAt: string;
  readonly activeSince: string | null;
  readonly idleSince: string | null;
}

export function isQuiescentCounters(counters: ActivityCounters): boolean {
  return (
    !counters.rootRunning &&
    counters.activeInternalTasks === 0 &&
    counters.queuedInputs === 0 &&
    counters.pendingDeliveries === 0 &&
    counters.pendingInteractions === 0 &&
    counters.localOperations === 0
  );
}

/**
 * 休眠的唯一判定入口。
 *
 * 与现有 activation 规则保持一致：观测不完整、正在关闭、仍持 keep-awake、存在 pin 或任一
 * 计数非零时都拒绝休眠；宁可多驻留一次，也不要在交接点丢掉尚未送达的工作。
 */
export function canSleep(snapshot: ActivitySnapshot): boolean {
  return (
    snapshot.complete &&
    !snapshot.closing &&
    snapshot.hold === null &&
    snapshot.pins.length === 0 &&
    snapshot.state === 'idle' &&
    isQuiescentCounters(snapshot.counters)
  );
}
