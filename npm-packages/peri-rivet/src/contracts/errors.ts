import type { JsonValue } from '../json.js';

/**
 * v5 全边界错误目录。
 *
 * 只收当前有真实产生路径、或客户端必须能区分的 code：`busy`（当前 turn 未结束）与
 * `binding_conflict`（action 目标与当前绑定不符）必须独立；`session_epoch_mismatch` 未收录，
 * 因为 Rust session_epoch 尚未上 wire，没有可执行的比较来源。legacy 错误集（ActorError）
 * 与新契约并存，M0 不把两边 code 合并成一个联合。
 */
export const PERI_ERROR_CODES = [
  'invalid_input',
  'invalid_identity',
  'busy',
  'closed',
  'close_incomplete',
  'wake_failed',
  'restore_failed',
  'sleep_failed',
  'keep_awake_failed',
  'activity_source_required',
  'session_not_found',
  'binding_conflict',
  'sandbox_start_failed',
  'sandbox_unavailable',
  'sandbox_lease_stale',
  'sandbox_close_incomplete',
  'catalog_conflict',
  'catalog_unavailable',
  'storage_unavailable',
  'internal_error',
] as const;

export type PeriErrorCode = (typeof PERI_ERROR_CODES)[number];

/** 失败发生在哪个组件；用来把同一 code 的故障定位到边界，而不是解析 message。 */
export const PERI_ERROR_PHASES = ['input', 'session', 'harness', 'sandbox', 'catalog', 'internal'] as const;

export type PeriErrorPhase = (typeof PERI_ERROR_PHASES)[number];

export type PeriErrorDetails = Readonly<Record<string, JsonValue>>;

/** 跨边界 cause 的元素：只有名字，携带 message 会把上游凭据转发给客户端与日志。 */
export interface PeriErrorCause {
  readonly name: string;
}

/**
 * 按 code 给出唯一默认重试建议，避免每个调用点各判一次。
 *
 * `internal_error` 刻意标为不可重试：未知故障的自动重放会掩盖根因，重试必须是调用方的显式决定。
 */
const RETRYABLE_BY_CODE: Readonly<Record<PeriErrorCode, boolean>> = {
  invalid_input: false,
  invalid_identity: false,
  busy: true,
  closed: false,
  close_incomplete: true,
  wake_failed: true,
  restore_failed: false,
  sleep_failed: true,
  keep_awake_failed: true,
  activity_source_required: false,
  session_not_found: false,
  binding_conflict: false,
  sandbox_start_failed: true,
  sandbox_unavailable: true,
  sandbox_lease_stale: false,
  sandbox_close_incomplete: true,
  catalog_conflict: false,
  catalog_unavailable: true,
  storage_unavailable: true,
  internal_error: false,
};

export interface PeriErrorInit {
  readonly code: PeriErrorCode;
  readonly phase: PeriErrorPhase;
  readonly message: string;
  readonly retryable?: boolean;
  readonly details?: PeriErrorDetails;
  readonly session_id?: string;
  readonly session_epoch?: number | null;
  readonly turn_id?: string | null;
  readonly attempt_id?: string | null;
  readonly cause?: unknown;
}

export interface PeriErrorPayload {
  readonly name: 'PeriError';
  readonly code: PeriErrorCode;
  readonly phase: PeriErrorPhase;
  readonly message: string;
  readonly retryable: boolean;
  readonly details?: PeriErrorDetails;
  readonly session_id?: string;
  readonly session_epoch?: number | null;
  readonly turn_id?: string | null;
  readonly attempt_id?: string | null;
  readonly cause?: readonly PeriErrorCause[];
}

/**
 * 跨边界只暴露结构化字段；cause 只保留有界的 name 链，避免上游 message 中的凭据被回放。
 */
function describeCause(error: unknown, depth = 0): PeriErrorCause[] | undefined {
  if (!(error instanceof Error)) {
    return undefined;
  }
  const chain: PeriErrorCause[] = [{ name: error.name }];
  if (depth < 3 && error.cause !== undefined) {
    const nested = describeCause(error.cause, depth + 1);
    if (nested !== undefined) {
      chain.push(...nested);
    }
  }
  return chain;
}

/**
 * 把 wire 上的 name 链还原成 Error cause，保证 decode 后再次 toJSON 形状不变；
 * message 不参与回放，因此这里只恢复 name。
 */
function decodeCause(value: unknown): Error | undefined {
  if (!Array.isArray(value)) {
    return undefined;
  }
  const names = value
    .slice(0, 4)
    .map((entry) => (isRecord(entry) && typeof entry.name === 'string' ? entry.name : undefined))
    .filter((name): name is string => name !== undefined);
  let chain: Error | undefined;
  for (const name of names.reverse()) {
    const error = new Error();
    error.name = name;
    if (chain !== undefined) {
      error.cause = chain;
    }
    chain = error;
  }
  return chain;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isPhase(value: unknown): value is PeriErrorPhase {
  return typeof value === 'string' && (PERI_ERROR_PHASES as readonly string[]).includes(value);
}

/**
 * 与 ActorError 并存的 v5 错误类型：ActorError 仍服务现有 actor，PeriError 只走新契约入口。
 */
export class PeriError extends Error {
  override readonly name = 'PeriError';
  readonly code: PeriErrorCode;
  readonly phase: PeriErrorPhase;
  readonly retryable: boolean;
  readonly details: PeriErrorDetails | undefined;
  readonly session_id: string | undefined;
  readonly session_epoch: number | null | undefined;
  readonly turn_id: string | null | undefined;
  readonly attempt_id: string | null | undefined;

  constructor(init: PeriErrorInit) {
    super(init.message, init.cause === undefined ? undefined : { cause: init.cause });
    this.code = init.code;
    this.phase = init.phase;
    this.retryable = init.retryable ?? RETRYABLE_BY_CODE[init.code];
    this.details = init.details;
    this.session_id = init.session_id;
    this.session_epoch = init.session_epoch;
    this.turn_id = init.turn_id;
    this.attempt_id = init.attempt_id;
  }

  toJSON(): PeriErrorPayload {
    const cause = describeCause(this.cause);
    return {
      name: 'PeriError',
      code: this.code,
      phase: this.phase,
      message: this.message,
      retryable: this.retryable,
      ...(this.details === undefined ? {} : { details: this.details }),
      ...(this.session_id === undefined ? {} : { session_id: this.session_id }),
      ...(this.session_epoch === undefined ? {} : { session_epoch: this.session_epoch }),
      ...(this.turn_id === undefined ? {} : { turn_id: this.turn_id }),
      ...(this.attempt_id === undefined ? {} : { attempt_id: this.attempt_id }),
      ...(cause === undefined ? {} : { cause }),
    };
  }
}

export function isPeriError(error: unknown): error is PeriError {
  return error instanceof PeriError;
}

function decodeDetails(value: unknown): PeriErrorDetails | undefined {
  if (!isRecord(value)) {
    return undefined;
  }
  const details: Record<string, JsonValue> = {};
  for (const [key, entry] of Object.entries(value)) {
    // 入参通常来自 JSON，但仍做浅层收窄，避免把函数或嵌套类实例写回 details。
    if (
      entry === null ||
      typeof entry === 'string' ||
      typeof entry === 'boolean' ||
      (typeof entry === 'number' && Number.isFinite(entry)) ||
      Array.isArray(entry) ||
      isRecord(entry)
    ) {
      details[key] = entry as JsonValue;
    }
  }
  return details;
}

function decodeString(value: unknown): string | undefined {
  return typeof value === 'string' ? value : undefined;
}

function decodeNullableString(value: unknown): string | null | undefined {
  if (value === null) {
    return null;
  }
  return typeof value === 'string' ? value : undefined;
}

function decodeNullableEpoch(value: unknown): number | null | undefined {
  if (value === null) {
    return null;
  }
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

/**
 * 未知 code 一律降级为 `internal_error` 并保留原始 code；调用方据此告警，不能猜语义分支。
 */
export function decodePeriError(value: unknown): PeriError {
  if (!isRecord(value)) {
    return new PeriError({
      code: 'internal_error',
      phase: 'internal',
      message: 'unrecognized PeriError payload',
    });
  }

  const originalCode = typeof value.code === 'string' ? value.code : 'unknown';
  const knownCode = PERI_ERROR_CODES.find((code) => code === originalCode);
  const decodedDetails = decodeDetails(value.details);
  const details =
    knownCode === undefined ? { ...(decodedDetails ?? {}), original_code: originalCode } : decodedDetails;
  const sessionId = decodeString(value.session_id);
  const sessionEpoch = decodeNullableEpoch(value.session_epoch);
  const turnId = decodeNullableString(value.turn_id);
  const attemptId = decodeNullableString(value.attempt_id);
  const cause = decodeCause(value.cause);
  const retryable =
    knownCode !== undefined && typeof value.retryable === 'boolean' ? value.retryable : undefined;

  return new PeriError({
    code: knownCode ?? 'internal_error',
    phase: isPhase(value.phase) ? value.phase : 'internal',
    message: typeof value.message === 'string' ? value.message : originalCode,
    ...(retryable === undefined ? {} : { retryable }),
    ...(details === undefined ? {} : { details }),
    ...(sessionId === undefined ? {} : { session_id: sessionId }),
    ...(sessionEpoch === undefined ? {} : { session_epoch: sessionEpoch }),
    ...(turnId === undefined ? {} : { turn_id: turnId }),
    ...(attemptId === undefined ? {} : { attempt_id: attemptId }),
    ...(cause === undefined ? {} : { cause }),
  });
}
