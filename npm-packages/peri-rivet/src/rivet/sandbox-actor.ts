import { type ActionContext, type ActorDefinition, type ActorOptionsInput, actor } from 'rivetkit';
import {
  isPeriError,
  PeriError,
  type PeriErrorCode,
  type PeriErrorDetails,
  type PeriErrorPayload,
  type PeriErrorPhase,
} from '../contracts/errors.js';
import { parseIdentityText } from '../contracts/identity.js';
import { parseSandboxId } from '../contracts/server-identity.js';
import { createDeferred, type Deferred } from '../util/deferred.js';

/**
 * sandbox actor：单个运行实例引用计数的唯一权威。
 *
 * 引用计数只存在于 `vars.holders`（activation 级内存），端口只负责 spawn/close/只读状态。
 * 持有引用期间用 `keepAwake` 阻止空闲休眠；`vars` 仍可能随 sleep 丢失，因此
 * `status.orphaned` 与 teardown 时的 best-effort close 承担校正，宿主再用进程级
 * `closeAll` 兜底。
 */

/** 宿主进程内的运行端口：只做 spawn/close/只读状态，不持有引用计数。 */
export interface SandboxRuntimePort {
  start(sandboxId: string): Promise<unknown>;
  close(sandboxId: string): Promise<void>;
  /** 未登记返回 undefined；返回值只用于展示与漂移校正，不参与计数。 */
  stateOf(sandboxId: string): string | undefined;
}

/** 运行意图；不持久化，不跨重启恢复。 */
export interface SandboxActorVars {
  refs: number;
  holders: string[];
  startedAtMs: number | null;
}

export interface SandboxAcquireView {
  readonly sandboxId: string;
  readonly state: 'ready';
}

export interface SandboxStatusView {
  readonly sandboxId: string;
  readonly present: boolean;
  readonly state: string | null;
  readonly refs: number;
  readonly holders: readonly string[];
  readonly startedAtMs: number | null;
  /** refs 为 0 但实例仍在：holders 随 activation 丢失，需宿主显式 closeAll。 */
  readonly orphaned: boolean;
}

export type SandboxActorResult<T> =
  | { readonly ok: true; readonly value: T }
  | { readonly ok: false; readonly error: PeriErrorPayload };

export type SandboxActorDiagnostic =
  | { readonly type: 'runtime_missing'; readonly sandboxId: string }
  | {
      readonly type: 'start_failed' | 'close_failed' | 'status_failed' | 'action_failed';
      readonly sandboxId: string;
      readonly error: unknown;
    }
  | { readonly type: 'keep_awake_failed'; readonly sandboxId: string; readonly error: unknown }
  | { readonly type: 'holders_lost_on_teardown'; readonly sandboxId: string; readonly refs: number };

export interface SandboxActorOptions {
  /** 缺省表示未启用 sandbox；acquire 返回 sandbox_unavailable。 */
  readonly runtime?: SandboxRuntimePort | undefined;
  readonly now?: (() => number) | undefined;
  /** 缺省从 key[1] 取 sandboxId；key 仍必须先满足 `["sbx", sandboxId]`。返回值非法时 acquire 失败 invalid_identity。 */
  readonly sandboxIdFromKey?: ((key: readonly string[]) => string | undefined) | undefined;
  readonly options?: ActorOptionsInput;
  readonly onDiagnostic?: (diagnostic: SandboxActorDiagnostic) => void;
}

/** actor action / lifecycle 需要的最小结构；真实 `c` 由 rivetkit 注入。 */
export interface SandboxActorContext {
  readonly vars: SandboxActorVars;
  readonly key: readonly string[];
  keepAwake<T>(promise: Promise<T>): Promise<T>;
}

export type SandboxActorActions = {
  acquire(c: SandboxActionContext, holder: string): Promise<SandboxActorResult<SandboxAcquireView>>;
  release(c: SandboxActionContext, holder: string): Promise<SandboxActorResult<{ readonly released: true }>>;
  status(c: SandboxActionContext): Promise<SandboxActorResult<SandboxStatusView>>;
  closeAll(c: SandboxActionContext): Promise<SandboxActorResult<{ readonly closed: true }>>;
};

type SandboxActionContext = ActionContext<unknown, unknown, unknown, SandboxActorVars, unknown, undefined>;

/** Explicit so consumers and declaration emit never depend on Rivet's internal generic names. */
export type PeriSandboxActorDefinition = ActorDefinition<
  unknown,
  unknown,
  unknown,
  SandboxActorVars,
  unknown,
  undefined,
  Record<never, never>,
  Record<never, never>,
  SandboxActorActions
>;

const SANDBOX_UNAVAILABLE_MESSAGE = 'Sandbox pool is not configured';
const SANDBOX_KEY_PREFIX = 'sbx';

function failure(
  code: PeriErrorCode,
  message: string,
  phase: PeriErrorPhase = 'sandbox',
  details?: PeriErrorDetails,
): SandboxActorResult<never> {
  return {
    ok: false,
    error: new PeriError({ code, phase, message, ...(details === undefined ? {} : { details }) }).toJSON(),
  };
}

function failureFrom(error: unknown): SandboxActorResult<never> {
  // 只有 PeriError 携带可跨边界的 code；其余折叠为 internal_error，原文留给诊断。
  return isPeriError(error)
    ? { ok: false, error: error.toJSON() }
    : failure('internal_error', 'sandbox actor action failed', 'internal');
}

function invalidKey(details?: PeriErrorDetails): SandboxActorResult<never> {
  return failure('invalid_identity', 'sandbox actor key must be ["sbx", sandboxId]', 'input', details);
}

function diagnose(
  sink: { readonly onDiagnostic?: (diagnostic: SandboxActorDiagnostic) => void },
  diagnostic: SandboxActorDiagnostic,
): void {
  try {
    sink.onDiagnostic?.(diagnostic);
  } catch {
    // 观测不得影响动作结果。
  }
}

export function createSandboxVars(): SandboxActorVars {
  return { refs: 0, holders: [], startedAtMs: null };
}

function reset(vars: SandboxActorVars): void {
  vars.refs = 0;
  vars.holders = [];
  vars.startedAtMs = null;
}

/** 每个 vars 一个保活句柄；vars 随休眠更换，旧句柄由 GC 回收。 */
const holds = new WeakMap<SandboxActorVars, Deferred<void>>();

function releaseHold(vars: SandboxActorVars): void {
  const deferred = holds.get(vars);
  if (deferred === undefined) {
    return;
  }
  holds.delete(vars);
  deferred.resolve();
}

function syncKeepAwake(c: SandboxActorContext, options: SandboxActorOptions, sandboxId: string): void {
  if (c.vars.refs === 0) {
    releaseHold(c.vars);
    return;
  }
  if (holds.has(c.vars)) {
    return;
  }
  const deferred = createDeferred<void>();
  holds.set(c.vars, deferred);
  try {
    void c.keepAwake(deferred.promise).catch((error: unknown) => {
      if (holds.get(c.vars) === deferred) {
        holds.delete(c.vars);
      }
      diagnose(options, { type: 'keep_awake_failed', sandboxId, error });
    });
  } catch (error) {
    holds.delete(c.vars);
    diagnose(options, { type: 'keep_awake_failed', sandboxId, error });
  }
}

/**
 * 每个 vars 一条串行队列。
 *
 * 同一 actor 的多个 action 实际会并发执行；读改写 `vars` 必须排队，否则
 * release 的 close 尚未完成时 acquire 会先登记 holder，再被 release 的 reset 清空。
 */
const queues = new WeakMap<SandboxActorVars, Promise<void>>();

function serialize<T>(vars: SandboxActorVars, run: () => Promise<T> | T): Promise<T> {
  const previous = queues.get(vars) ?? Promise.resolve();
  const task = previous.then(run);
  queues.set(
    vars,
    task.then(
      () => undefined,
      () => undefined,
    ),
  );
  return task;
}

/** 纯逻辑核心：测试直接构造，不需要启动 engine，也不 import rivetkit。 */
export class SandboxActorCore {
  constructor(
    private readonly sandboxId: string,
    private readonly vars: SandboxActorVars,
    private readonly dependencies: Pick<SandboxActorOptions, 'runtime' | 'now' | 'onDiagnostic'> = {},
  ) {}

  async acquire(holder: string): Promise<SandboxActorResult<SandboxAcquireView>> {
    return await serialize(this.vars, async () => {
      const parsed = this.holder(holder);
      if (!parsed.ok) {
        return parsed;
      }
      const runtime = this.dependencies.runtime;
      if (runtime === undefined) {
        diagnose(this.dependencies, { type: 'runtime_missing', sandboxId: this.sandboxId });
        return failure('sandbox_unavailable', SANDBOX_UNAVAILABLE_MESSAGE);
      }
      this.reconcile();
      // 已持有且实例仍在：只做幂等的 start 确认，不重复计数。
      const known = this.vars.holders.includes(parsed.value);
      try {
        await runtime.start(this.sandboxId);
      } catch (error) {
        diagnose(this.dependencies, { type: 'start_failed', sandboxId: this.sandboxId, error });
        return failure('sandbox_start_failed', `Sandbox ${this.sandboxId} failed to start`);
      }
      if (!known) {
        this.vars.holders.push(parsed.value);
        this.vars.refs = this.vars.holders.length;
        this.vars.startedAtMs ??= this.dependencies.now?.() ?? Date.now();
      }
      return { ok: true, value: { sandboxId: this.sandboxId, state: 'ready' } };
    });
  }

  async release(holder: string): Promise<SandboxActorResult<{ readonly released: true }>> {
    return await serialize(this.vars, async () => {
      const parsed = this.holder(holder);
      if (!parsed.ok) {
        return parsed;
      }
      if (!this.vars.holders.includes(parsed.value)) {
        return { ok: true, value: { released: true } };
      }
      const remaining = this.vars.holders.filter((item) => item !== parsed.value);
      if (remaining.length > 0) {
        this.vars.holders = remaining;
        this.vars.refs = remaining.length;
        return { ok: true, value: { released: true } };
      }
      // 最后一个引用才销毁；close 失败时保留 holder 供重试。
      const closed = await this.closeInstance();
      if (!closed.ok) {
        return closed;
      }
      reset(this.vars);
      return { ok: true, value: { released: true } };
    });
  }

  /** 只读状态；读取时校正「引用仍在但实例已不在」的漂移，但不销毁孤儿。 */
  async status(): Promise<SandboxActorResult<SandboxStatusView>> {
    return await serialize(this.vars, () => {
      let state: string | undefined;
      try {
        state = this.dependencies.runtime?.stateOf(this.sandboxId);
      } catch (error) {
        diagnose(this.dependencies, { type: 'status_failed', sandboxId: this.sandboxId, error });
        return failure('internal_error', 'sandbox status is unavailable', 'internal');
      }
      if (this.vars.refs > 0 && state === undefined) {
        reset(this.vars);
      }
      return {
        ok: true,
        value: {
          sandboxId: this.sandboxId,
          present: state !== undefined,
          state: state ?? null,
          refs: this.vars.refs,
          holders: [...this.vars.holders],
          startedAtMs: this.vars.startedAtMs,
          orphaned: this.vars.refs === 0 && state !== undefined,
        },
      };
    });
  }

  /**
   * 只关闭本 sandboxId：actor 按 id 分片，进程级回收留给宿主的 `runtime.closeAll()`。
   *
   * 即使 refs 为 0 也调用 close，用于清理 sleep 丢失 holders 后留下的孤儿实例。
   */
  async closeAll(): Promise<SandboxActorResult<{ readonly closed: true }>> {
    return await serialize(this.vars, async () => {
      const closed = await this.closeInstance();
      if (!closed.ok) {
        return closed;
      }
      reset(this.vars);
      return { ok: true, value: { closed: true } };
    });
  }

  /** activation 结束：refs>0 说明 holders 将随 vars 丢失，先记诊断再 best-effort close。 */
  async teardown(): Promise<void> {
    await serialize(this.vars, async () => {
      if (this.vars.refs > 0) {
        diagnose(this.dependencies, {
          type: 'holders_lost_on_teardown',
          sandboxId: this.sandboxId,
          refs: this.vars.refs,
        });
      }
      const closed = await this.closeInstance();
      if (closed.ok) {
        reset(this.vars);
      }
    });
  }

  private holder(value: string): SandboxActorResult<string> {
    try {
      return { ok: true, value: parseIdentityText<string>('holder', value) };
    } catch (error) {
      return failureFrom(error);
    }
  }

  private reconcile(): void {
    if (this.vars.refs === 0) {
      return;
    }
    if (this.dependencies.runtime?.stateOf(this.sandboxId) === undefined) {
      reset(this.vars);
    }
  }

  /** close 失败只上报诊断，错误原文不进入返回信封。 */
  private async closeInstance(): Promise<SandboxActorResult<true>> {
    const runtime = this.dependencies.runtime;
    if (runtime === undefined) {
      return { ok: true, value: true };
    }
    try {
      await runtime.close(this.sandboxId);
      return { ok: true, value: true };
    } catch (error) {
      diagnose(this.dependencies, { type: 'close_failed', sandboxId: this.sandboxId, error });
      return failure('sandbox_close_incomplete', `Sandbox ${this.sandboxId} did not close`);
    }
  }
}

function resolveSandboxId(c: SandboxActorContext, options: SandboxActorOptions): SandboxActorResult<string> {
  // key 是 sandbox 身份的唯一声明：形状不符时若仍落到 key[1]，同一 sandboxId 会被多个 key 分走引用计数。
  if (c.key.length !== 2 || c.key[0] !== SANDBOX_KEY_PREFIX) {
    return invalidKey({ reason: 'actor_key_shape', keyLength: c.key.length });
  }
  const raw = options.sandboxIdFromKey === undefined ? c.key[1] : options.sandboxIdFromKey(c.key);
  if (raw === undefined || raw.length === 0) {
    return invalidKey();
  }
  try {
    return { ok: true, value: parseSandboxId(raw) };
  } catch (error) {
    return isPeriError(error) ? failureFrom(error) : invalidKey();
  }
}

async function executeAction<T>(
  c: SandboxActorContext,
  options: SandboxActorOptions,
  run: (core: SandboxActorCore) => Promise<SandboxActorResult<T>>,
): Promise<SandboxActorResult<T>> {
  const resolved = resolveSandboxId(c, options);
  if (!resolved.ok) {
    return resolved;
  }
  const sandboxId = resolved.value;
  let result: SandboxActorResult<T>;
  try {
    result = await run(new SandboxActorCore(sandboxId, c.vars, options));
  } catch (error) {
    diagnose(options, { type: 'action_failed', sandboxId, error });
    result = failure('internal_error', 'sandbox actor action failed', 'internal');
  }
  syncKeepAwake(c, options, sandboxId);
  return result;
}

async function teardown(c: SandboxActorContext, options: SandboxActorOptions): Promise<void> {
  const resolved = resolveSandboxId(c, options);
  if (resolved.ok) {
    await new SandboxActorCore(resolved.value, c.vars, options).teardown();
  }
  // activation 正在结束：不能再持有一个新的 keepAwake。
  releaseHold(c.vars);
}

/**
 * A Rivet actor owning the reference count for exactly one sandbox instance.
 *
 * `runtime` is optional so the actor can be registered without a sandbox backend; every acquire
 * then fails with `sandbox_unavailable` instead of pretending the instance exists.
 */
export function createSandboxActor(options: SandboxActorOptions = {}): PeriSandboxActorDefinition {
  return actor({
    ...(options.options === undefined ? {} : { options: options.options }),
    createVars: (): SandboxActorVars => createSandboxVars(),
    onSleep: async (c) => await teardown(c, options),
    onDestroy: async (c) => await teardown(c, options),
    actions: {
      acquire: async (c, holder) => await executeAction(c, options, (core) => core.acquire(holder)),
      release: async (c, holder) => await executeAction(c, options, (core) => core.release(holder)),
      status: async (c) => await executeAction(c, options, (core) => core.status()),
      closeAll: async (c) => await executeAction(c, options, (core) => core.closeAll()),
    },
  });
}
