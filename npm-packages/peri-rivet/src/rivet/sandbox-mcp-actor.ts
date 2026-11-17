import { timingSafeEqual } from 'node:crypto';
import { type ActionContext, type ActorDefinition, type ActorOptionsInput, actor } from 'rivetkit';
import {
  isPeriError,
  PeriError,
  type PeriErrorCode,
  type PeriErrorDetails,
  type PeriErrorPayload,
  type PeriErrorPhase,
} from '../contracts/errors.js';
import type { HostResourceLease, HostResourceOwner, HostResourceTrack } from './host-resource-owner.js';
import { projectRuntimeRequest } from './runtime-request.js';
import {
  createSandboxVars,
  releaseSandboxHold,
  SandboxActorCore,
  type SandboxActorCoreDependencies,
  type SandboxActorDiagnostic,
  type SandboxActorResult,
  type SandboxActorVars,
  type SandboxRuntimePort,
  syncSandboxKeepAwake,
} from './sandbox-actor.js';
import { projectError } from './sandbox-error-projection.js';
import { parseMachineId, parseWorkspaceUri, type WorkspaceUri } from './workspace-uri.js';

/**
 * Sandbox MCP actor：一个 actor 同时持有 workspace 环境的引用计数与 `/mcp` HTTP。
 *
 * 引用计数仍由 `SandboxActorCore` 唯一维护；本模块只增加 workspace URI 身份、
 * 部署授权/凭据校验、HTTP 转发与关闭前的在途请求收束。
 */

/** 宿主的真实运行端口：额外把 `/mcp` 请求转给环境内 MCP。 */
export interface SandboxMcpRuntimePort extends SandboxRuntimePort {
  fetchMcp(workspaceUri: WorkspaceUri, request: Request): Promise<Response>;
}

export interface SandboxMcpAcquireView {
  readonly workspaceUri: WorkspaceUri;
  readonly state: 'ready';
}

export interface SandboxMcpStatusView {
  readonly workspaceUri: WorkspaceUri;
  readonly present: boolean;
  readonly state: string | null;
  readonly refs: number;
  readonly holders: readonly string[];
  readonly startedAtMs: number | null;
  readonly orphaned: boolean;
}

export type SandboxMcpActorDiagnostic =
  | { readonly type: 'runtime_missing'; readonly workspaceUri: WorkspaceUri }
  | {
      readonly type: 'start_failed' | 'close_failed' | 'status_failed' | 'action_failed' | 'drain_failed';
      readonly workspaceUri: WorkspaceUri;
      readonly error: unknown;
    }
  | { readonly type: 'keep_awake_failed'; readonly workspaceUri: WorkspaceUri; readonly error: unknown }
  | { readonly type: 'holders_lost_on_teardown'; readonly workspaceUri: WorkspaceUri; readonly refs: number }
  | {
      readonly type: 'request_drain_timeout';
      readonly workspaceUri: WorkspaceUri;
      readonly remaining: number;
    }
  | {
      readonly type: 'request_rejected';
      readonly workspaceUri: WorkspaceUri | null;
      readonly reason: 'credential' | 'key' | 'path' | 'unavailable' | 'not_held' | 'closing';
    }
  | { readonly type: 'request_failed'; readonly workspaceUri: WorkspaceUri; readonly error: unknown };

export interface SandboxMcpActorOptions {
  /**
   * Required host-owned registry for one workspace's gate/runtime record.
   *
   * The record keeps the request gate and the close closure after Rivet clears activation vars, so a
   * new activation cannot open a second environment while the old gate still owns in-flight work.
   */
  readonly owner: HostResourceOwner;
  /** 缺省表示未启用 sandbox；acquire 返回 sandbox_unavailable，HTTP 返回 503。 */
  readonly runtime?: SandboxMcpRuntimePort | undefined;
  /** 部署级共享凭据；缺省或非法时工厂直接拒绝，不启动开放服务。 */
  readonly token: string;
  /** 可信部署身份；URI 的 machineId 必须与它一致。 */
  readonly machineId: string;
  /** 显式授权的 canonical workspace URI；不来自请求。 */
  readonly authorizedWorkspaces: readonly string[];
  readonly now?: (() => number) | undefined;
  /** 关闭实例前等待在途请求结束的上限，默认 5000ms。 */
  readonly drainTimeoutMs?: number | undefined;
  readonly options?: ActorOptionsInput;
  readonly onDiagnostic?: ((diagnostic: SandboxMcpActorDiagnostic) => void) | undefined;
}

export interface SandboxMcpRequestContext {
  readonly key: readonly string[];
  readonly vars: SandboxActorVars;
  readonly request?: Request | undefined;
}

export type SandboxMcpActionContext = ActionContext<
  unknown,
  unknown,
  unknown,
  SandboxActorVars,
  unknown,
  undefined
>;

export type SandboxMcpActorActions = {
  acquire(
    c: SandboxMcpActionContext,
    holder: string,
    credential: string,
  ): Promise<SandboxActorResult<SandboxMcpAcquireView>>;
  release(
    c: SandboxMcpActionContext,
    holder: string,
    credential: string,
  ): Promise<SandboxActorResult<{ readonly released: true }>>;
  status(c: SandboxMcpActionContext, credential: string): Promise<SandboxActorResult<SandboxMcpStatusView>>;
  closeAll(
    c: SandboxMcpActionContext,
    credential: string,
  ): Promise<SandboxActorResult<{ readonly closed: true }>>;
};

/** Explicit so consumers and declaration emit never depend on Rivet's internal generic names. */
export type PeriSandboxMcpActorDefinition = ActorDefinition<
  unknown,
  unknown,
  unknown,
  SandboxActorVars,
  unknown,
  undefined,
  Record<never, never>,
  Record<never, never>,
  SandboxMcpActorActions
>;

const SANDBOX_KEY_PREFIX = 'sbx';
const MCP_PATH = '/mcp';
const GATEWAY_REQUEST_PATH = /^\/gateway\/[^/]+\/request\/mcp$/;
const DEFAULT_DRAIN_TIMEOUT_MS = 5_000;
const UNAVAILABLE_MESSAGE = 'Sandbox pool is not configured';
const CREDENTIAL_MESSAGE = 'sandbox actor credential is required or invalid';

interface PreparedOptions extends SandboxMcpActorOptions {
  readonly tokenBytes: Buffer;
  readonly authorized: ReadonlySet<WorkspaceUri>;
  readonly drainTimeoutMs: number;
}

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
  return isPeriError(error)
    ? { ok: false, error: error.toJSON() }
    : failure('internal_error', 'sandbox actor action failed', 'internal');
}

function diagnose(
  sink: {
    readonly token: string;
    readonly onDiagnostic?: ((diagnostic: SandboxMcpActorDiagnostic) => void) | undefined;
  },
  diagnostic: SandboxMcpActorDiagnostic,
): void {
  try {
    const safe =
      'error' in diagnostic
        ? { ...diagnostic, error: projectError(diagnostic.error, sink.token) }
        : diagnostic;
    sink.onDiagnostic?.(safe);
  } catch {
    // 观测不得影响动作结果。
  }
}

function validateToken(token: string): Buffer {
  if (typeof token !== 'string' || !/^[\x21-\x7e]{16,512}$/.test(token)) {
    throw new PeriError({
      code: 'invalid_input',
      phase: 'input',
      message: 'sandbox actor token must be 16..512 printable characters without whitespace',
    });
  }
  return Buffer.from(token, 'utf8');
}

function prepare(options: SandboxMcpActorOptions): PreparedOptions {
  const machineId = parseMachineId(options.machineId);
  const authorized = new Set<WorkspaceUri>();
  for (const value of options.authorizedWorkspaces) {
    const parsed = parseWorkspaceUri(value);
    if (parsed.machineId !== machineId) {
      throw new PeriError({
        code: 'invalid_identity',
        phase: 'input',
        message: 'authorized workspace belongs to another machine',
        details: { reason: 'machine_mismatch', field: 'authorizedWorkspaces' },
      });
    }
    authorized.add(parsed.uri);
  }
  const drainTimeoutMs = options.drainTimeoutMs ?? DEFAULT_DRAIN_TIMEOUT_MS;
  if (!Number.isSafeInteger(drainTimeoutMs) || drainTimeoutMs < 0) {
    throw new PeriError({
      code: 'invalid_input',
      phase: 'input',
      message: 'drainTimeoutMs must be a non-negative integer',
    });
  }
  return { ...options, machineId, tokenBytes: validateToken(options.token), authorized, drainTimeoutMs };
}

/** 只在部署配置里比对凭据；请求里的 token 不会写进错误或日志。 */
function credentialMatches(prepared: PreparedOptions, credential: unknown): boolean {
  if (typeof credential !== 'string') {
    return false;
  }
  const actual = Buffer.from(credential, 'utf8');
  return actual.length === prepared.tokenBytes.length && timingSafeEqual(actual, prepared.tokenBytes);
}

function credentialFailure(): SandboxActorResult<never> {
  return failure('invalid_input', CREDENTIAL_MESSAGE, 'input');
}

function resolveWorkspaceKey(
  c: SandboxMcpRequestContext,
  prepared: PreparedOptions,
): SandboxActorResult<WorkspaceUri> {
  if (c.key.length !== 2 || c.key[0] !== SANDBOX_KEY_PREFIX) {
    return failure('invalid_identity', 'sandbox actor key must be ["sbx", workspaceUri]', 'input', {
      reason: 'actor_key_shape',
      keyLength: c.key.length,
    });
  }
  const raw = c.key[1];
  if (raw === undefined || raw.length === 0) {
    return failure('invalid_identity', 'sandbox actor key must be ["sbx", workspaceUri]', 'input');
  }
  let parsed: ReturnType<typeof parseWorkspaceUri>;
  try {
    parsed = parseWorkspaceUri(raw);
  } catch (error) {
    return failureFrom(error);
  }
  if (parsed.machineId !== prepared.machineId) {
    return failure('invalid_identity', 'workspace URI belongs to another machine', 'input', {
      reason: 'machine_mismatch',
    });
  }
  if (!prepared.authorized.has(parsed.uri)) {
    return failure('invalid_identity', 'workspace is not authorized', 'input', {
      reason: 'workspace_not_authorized',
    });
  }
  return { ok: true, value: parsed.uri };
}

/* ------------------------------------------------------------------ */
/* 关闭前收束：gate 归宿主 owner；activation vars 丢失后旧 gate 仍可关闭。 */
/* ------------------------------------------------------------------ */

interface InflightRequest {
  readonly controller: AbortController;
  /**
   * Cancels the forwarded response body; resolves only after the upstream source acknowledged
   * cancellation and rejects when it did not. A rejection keeps this handle in flight.
   */
  cancelResponse: ((reason: unknown) => Promise<void>) | undefined;
  /** Set once close/client abort won the race; a late response must not be streamed. */
  cancelled: boolean;
}

interface RequestGate {
  closing: boolean;
  readonly inflight: Set<InflightRequest>;
  readonly waiters: Set<() => void>;
}

/** The host-owned part of one sandbox activation; the ref count itself stays in `vars`. */
interface SandboxLive {
  readonly gate: RequestGate;
}

function createGate(): RequestGate {
  return { closing: false, inflight: new Set(), waiters: new Set() };
}

function notifyDrained(gate: RequestGate): void {
  if (gate.inflight.size !== 0) {
    return;
  }
  for (const waiter of [...gate.waiters]) {
    gate.waiters.delete(waiter);
    waiter();
  }
}

function beginRequest(vars: SandboxActorVars, gate: RequestGate): InflightRequest | null {
  if (gate.closing || vars.refs === 0) {
    return null;
  }
  const handle: InflightRequest = {
    controller: new AbortController(),
    cancelResponse: undefined,
    cancelled: false,
  };
  gate.inflight.add(handle);
  return handle;
}

function finishRequest(gate: RequestGate, handle: InflightRequest): void {
  gate.inflight.delete(handle);
  notifyDrained(gate);
}

function openGate(gate: RequestGate): void {
  gate.closing = false;
}

async function cancelInflight(handle: InflightRequest, reason: unknown): Promise<void> {
  handle.cancelled = true;
  try {
    handle.controller.abort(reason);
  } catch {
    // AbortController.abort is synchronous and does not throw for a normal signal.
  }
  try {
    await handle.cancelResponse?.(reason);
  } catch {
    // The stream owner already recorded the failure and kept the request in flight; swallow
    // here so abort/drain callers do not turn a retained close into an unhandled rejection.
  }
}

async function drainRequests(gate: RequestGate, timeoutMs: number): Promise<number> {
  gate.closing = true;
  const handles = [...gate.inflight];
  for (const handle of handles) {
    void cancelInflight(handle, new Error('sandbox is closing'));
  }
  if (gate.inflight.size === 0 || timeoutMs === 0) {
    return gate.inflight.size;
  }
  let timer: ReturnType<typeof setTimeout> | undefined;
  let waiter: (() => void) | undefined;
  const timerPromise = new Promise<number>((resolve) => {
    timer = setTimeout(() => resolve(gate.inflight.size), timeoutMs);
  });
  const drained = new Promise<number>((resolve) => {
    waiter = () => resolve(gate.inflight.size);
    gate.waiters.add(waiter);
  });
  // abort/cancel may complete synchronously; confirm after registering the waiter.
  if (gate.inflight.size === 0 && waiter !== undefined) {
    waiter();
  }
  const remaining = await Promise.race([drained, timerPromise]);
  if (timer !== undefined) {
    clearTimeout(timer);
  }
  if (waiter !== undefined) {
    gate.waiters.delete(waiter);
  }
  return remaining;
}

function mapCoreDiagnostic(
  diagnostic: SandboxActorDiagnostic,
  workspaceUri: WorkspaceUri,
): SandboxMcpActorDiagnostic {
  if (diagnostic.type === 'holders_lost_on_teardown') {
    return { type: diagnostic.type, workspaceUri, refs: diagnostic.refs };
  }
  if (diagnostic.type === 'runtime_missing') {
    return { type: diagnostic.type, workspaceUri };
  }
  return { type: diagnostic.type, workspaceUri, error: diagnostic.error };
}

function coreDependencies(
  workspaceUri: WorkspaceUri,
  gate: RequestGate,
  prepared: PreparedOptions,
): SandboxActorCoreDependencies {
  return {
    runtime: prepared.runtime,
    now: prepared.now,
    onDiagnostic: (diagnostic) => diagnose(prepared, mapCoreDiagnostic(diagnostic, workspaceUri)),
    beforeClose: async () => {
      const remaining = await drainRequests(gate, prepared.drainTimeoutMs);
      if (remaining > 0) {
        diagnose(prepared, { type: 'request_drain_timeout', workspaceUri, remaining });
        // Closing the instance now would strand readers whose upstream source
        // never acknowledged cancellation. Keep the holders and report incomplete.
        throw new PeriError({
          code: 'sandbox_close_incomplete',
          phase: 'sandbox',
          message: 'Sandbox did not confirm that in-flight requests were drained',
          details: { reason: 'drain_failed', remaining },
        });
      }
    },
  };
}

/** Converts a returned failure payload back into an error the owner can retain and retry. */
function errorFromPayload(payload: PeriErrorPayload): PeriError {
  return new PeriError({
    code: payload.code,
    phase: payload.phase,
    message: payload.message,
    retryable: payload.retryable,
    ...(payload.details === undefined ? {} : { details: payload.details }),
  });
}

/** A retired lease belongs to the host close path; no action may touch the runtime through it. */
function closingError(): PeriError {
  return new PeriError({
    code: 'closed',
    phase: 'sandbox',
    message: 'sandbox is closing',
    details: { reason: 'retired' },
  });
}

function closingFailure(): SandboxActorResult<never> {
  return failure('closed', 'sandbox is closing', 'sandbox', { reason: 'retired' });
}

/** One detach per host record; the callback keeps vars only, never the Rivet context. */
const holdDetachBound = new WeakSet<SandboxLive>();

/**
 * Activation ends are sticky even after a confirmed close removes the owner record; an in-flight
 * action holding the old vars must not create a new record and start a runtime.
 */
const retiredActivations = new WeakSet<SandboxActorVars>();

function bindSandboxHoldDetach(vars: SandboxActorVars, lease: HostResourceLease<SandboxLive>): void {
  if (holdDetachBound.has(lease.value)) {
    return;
  }
  holdDetachBound.add(lease.value);
  lease.onRetire(() => releaseSandboxHold(vars));
}

/**
 * Creates the host-owned gate and its close closure for one activation.
 *
 * The closure captures only plain vars, the runtime port and the gate, never Rivet's action
 * context. It reuses `SandboxActorCore` so reference counting and runtime close stay in one place.
 */
function createSandboxLive(
  vars: SandboxActorVars,
  workspaceUri: WorkspaceUri,
  prepared: PreparedOptions,
  track: HostResourceTrack,
): SandboxLive {
  const gate = createGate();
  track(
    {
      close: async () => {
        const result = await new SandboxActorCore(
          workspaceUri,
          vars,
          coreDependencies(workspaceUri, gate, prepared),
        ).closeAll();
        if (!result.ok) {
          throw errorFromPayload(result.error);
        }
      },
    },
    0,
  );
  return { gate };
}

async function ensureSandboxLease(
  c: SandboxMcpActionContext,
  prepared: PreparedOptions,
  workspaceUri: WorkspaceUri,
  allowRetired = false,
): Promise<HostResourceLease<SandboxLive>> {
  if (retiredActivations.has(c.vars)) {
    // Teardown already confirmed (and may have removed) the record. Only a close retry may use the
    // surviving record; reopening the activation would start a runtime after its sleep/destroy.
    const retained = allowRetired ? prepared.owner.lease<SandboxLive>(c.vars) : undefined;
    if (retained !== undefined) {
      return retained;
    }
    throw closingError();
  }
  const current = prepared.owner.lease<SandboxLive>(c.vars);
  if (current?.retired) {
    if (!allowRetired) {
      // Retire is the host close boundary: reopening the same activation would start a runtime
      // whose close task may already have completed.
      throw closingError();
    }
    return current;
  }
  const lease = await prepared.owner.open<SandboxLive>(c.key, c.vars, async (track) =>
    createSandboxLive(c.vars, workspaceUri, prepared, track),
  );
  bindSandboxHoldDetach(c.vars, lease);
  return lease;
}

type ActionOutcome<T> =
  | { readonly ok: true; readonly workspaceUri: WorkspaceUri; readonly result: SandboxActorResult<T> }
  | { readonly ok: false; readonly result: SandboxActorResult<never> };

async function executeAction<T>(
  c: SandboxMcpActionContext,
  prepared: PreparedOptions,
  credential: unknown,
  run: (core: SandboxActorCore, lease: HostResourceLease<SandboxLive>) => Promise<SandboxActorResult<T>>,
  options: { readonly allowRetiredLease?: boolean } = {},
): Promise<ActionOutcome<T>> {
  if (!credentialMatches(prepared, credential)) {
    return { ok: false, result: credentialFailure() };
  }
  const resolved = resolveWorkspaceKey(c, prepared);
  if (!resolved.ok) {
    return { ok: false, result: resolved };
  }
  const workspaceUri = resolved.value;
  let lease: HostResourceLease<SandboxLive>;
  try {
    // The owner closes a previous activation's gate/runtime first. If that close is still
    // unconfirmed, this action fails with the original incomplete error and never touches runtime.
    lease = await ensureSandboxLease(c, prepared, workspaceUri, options.allowRetiredLease === true);
  } catch (error) {
    if (isPeriError(error)) {
      return { ok: false, result: failureFrom(error) };
    }
    diagnose(prepared, { type: 'action_failed', workspaceUri, error });
    return { ok: false, result: failure('internal_error', 'sandbox actor action failed', 'internal') };
  }
  const gate = lease.value.gate;
  let result: SandboxActorResult<T>;
  try {
    const core = new SandboxActorCore(workspaceUri, c.vars, {
      ...coreDependencies(workspaceUri, gate, prepared),
      // The owner can retire the lease after the pre-run check but before this queued core action
      // executes. The check inside the same ref-count queue is the effective admission boundary.
      admit: () => (lease.retired ? closingFailure() : null),
    });
    result = await run(core, lease);
  } catch (error) {
    diagnose(prepared, { type: 'action_failed', workspaceUri, error });
    result = failure('internal_error', 'sandbox actor action failed', 'internal');
  }
  // close 失败会保留 holder；重新开放入口，避免环境仍在却被永久拒绝。Retired 记录归 host
  // close 路径，重开 gate 会让 HTTP 在 owner 关闭完成后重新进入。
  if (!lease.retired && result.ok && c.vars.refs > 0) {
    openGate(gate);
  } else if (
    !lease.retired &&
    !result.ok &&
    result.error.code === 'sandbox_close_incomplete' &&
    c.vars.refs > 0
  ) {
    openGate(gate);
  }
  // A retired activation's hold was detached by retire; never create a new hold while the close
  // path still has refs to reset, or closeAll could complete with a live keep-awake.
  if (!lease.retired) {
    syncSandboxKeepAwake(
      c,
      { onDiagnostic: (diagnostic) => diagnose(prepared, mapCoreDiagnostic(diagnostic, workspaceUri)) },
      workspaceUri,
    );
  }
  return { ok: true, workspaceUri, result };
}

function mapAcquired(
  result: SandboxActorResult<{ readonly sandboxId: string; readonly state: 'ready' }>,
  workspaceUri: WorkspaceUri,
): SandboxActorResult<SandboxMcpAcquireView> {
  if (!result.ok) {
    return result;
  }
  return { ok: true, value: { workspaceUri, state: result.value.state } };
}

function mapStatus(
  result: SandboxActorResult<{
    readonly sandboxId: string;
    readonly present: boolean;
    readonly state: string | null;
    readonly refs: number;
    readonly holders: readonly string[];
    readonly startedAtMs: number | null;
    readonly orphaned: boolean;
  }>,
  workspaceUri: WorkspaceUri,
): SandboxActorResult<SandboxMcpStatusView> {
  if (!result.ok) {
    return result;
  }
  return {
    ok: true,
    value: {
      workspaceUri,
      present: result.value.present,
      state: result.value.state,
      refs: result.value.refs,
      holders: result.value.holders,
      startedAtMs: result.value.startedAtMs,
      orphaned: result.value.orphaned,
    },
  };
}

/* ------------------------------------------------------------------ */
/* HTTP：只接受 `/mcp`；凭据与 workspace 授权都在触达 runtime 前校验。   */
/* ------------------------------------------------------------------ */

function isMcpPath(url: string): boolean {
  let pathname: string;
  try {
    pathname = new URL(url).pathname;
  } catch {
    return false;
  }
  return pathname === MCP_PATH || GATEWAY_REQUEST_PATH.test(pathname);
}

function httpError(status: number, code: string, message: string): Response {
  return new Response(JSON.stringify({ error: { code, message } }), {
    status,
    headers: { 'content-type': 'application/json' },
  });
}

function responseHeaders(headers: Headers): Headers {
  const filtered = new Headers(headers);
  for (const name of [
    'connection',
    'content-encoding',
    'content-length',
    'keep-alive',
    'proxy-authenticate',
    'proxy-authorization',
    'set-cookie',
    'te',
    'trailer',
    'transfer-encoding',
    'upgrade',
  ]) {
    filtered.delete(name);
  }
  return filtered;
}

function streamResponse(
  response: Response,
  gate: RequestGate,
  handle: InflightRequest,
  prepared: PreparedOptions,
  workspaceUri: WorkspaceUri,
): Response {
  if (response.body === null) {
    finishRequest(gate, handle);
    return new Response(null, {
      status: response.status,
      statusText: response.statusText,
      headers: responseHeaders(response.headers),
    });
  }
  const reader = response.body.getReader();
  let finished = false;
  // Proof that the read side ended on its own (`done` or a source error observed before any
  // cancellation attempt). Only such proof may finish a request without a confirmed cancel.
  let settled = false;
  let cancellation: Promise<void> | null = null;
  let wrapper: ReadableStreamDefaultController<Uint8Array> | null = null;
  const closeWrapper = (): void => {
    try {
      wrapper?.close();
    } catch {
      // The consumer may have cancelled or errored the wrapper first.
    }
  };
  const end = (): void => {
    if (finished) {
      return;
    }
    finished = true;
    finishRequest(gate, handle);
  };
  const cancelUpstream = (reason: unknown): Promise<void> => {
    if (cancellation !== null) {
      return cancellation;
    }
    if (finished || settled) {
      // An already finished/errored stream has nothing left to cancel; asking it again would
      // manufacture an acknowledgement that does not prove any producer stopped.
      end();
      closeWrapper();
      return Promise.resolve();
    }
    // Resolves only after the underlying source acknowledged cancellation. A rejected source
    // cancel is NOT an acknowledgement: keep the request in flight so release/close reports
    // drain_failed and retains the holder instead of pretending the environment is idle.
    cancellation = reader.cancel(reason).then(
      () => {
        end();
        closeWrapper();
      },
      (error: unknown) => {
        diagnose(prepared, { type: 'drain_failed', workspaceUri, error });
        throw error;
      },
    );
    return cancellation;
  };
  handle.cancelResponse = cancelUpstream;
  const body = new ReadableStream<Uint8Array>({
    start(stream) {
      wrapper = stream;
    },
    async pull(stream) {
      if (finished) {
        try {
          stream.close();
        } catch {
          // The wrapper may already be closed by the consumer.
        }
        return;
      }
      try {
        const chunk = await reader.read();
        if (finished) {
          try {
            stream.close();
          } catch {
            // The wrapper may already be closed by cancellation.
          }
          return;
        }
        if (chunk.done) {
          if (cancellation === null) {
            settled = true;
            end();
            stream.close();
          }
          // A `done` that arrives while cancellation is live is produced by the cancel
          // attempt itself (pending reads are closed before the source cancel settles).
          // It therefore proves nothing about the producer; only a resolved cancel may end.
          return;
        }
        stream.enqueue(chunk.value);
      } catch (error) {
        if (cancellation === null) {
          // The source failed before any cancel attempt: that is proof of termination, not a
          // cancellation acknowledgement.
          settled = true;
          end();
        }
        // With cancellation live this error may be the failed cancel itself, which proves
        // nothing; the cancellation outcome decides whether the request may finish.
        try {
          stream.error(error);
        } catch {
          // The wrapper may already be closed by cancellation.
        }
      }
    },
    async cancel(reason) {
      await cancelUpstream(reason);
    },
  });
  return new Response(body, {
    status: response.status,
    statusText: response.statusText,
    headers: responseHeaders(response.headers),
  });
}

async function handleRequest(
  c: SandboxMcpRequestContext,
  request: Request,
  prepared: PreparedOptions,
): Promise<Response> {
  if (!credentialMatches(prepared, request.headers.get('x-peri-sandbox-token'))) {
    diagnose(prepared, { type: 'request_rejected', workspaceUri: null, reason: 'credential' });
    return httpError(401, 'unauthorized', CREDENTIAL_MESSAGE);
  }
  const resolved = resolveWorkspaceKey(c, prepared);
  if (!resolved.ok) {
    diagnose(prepared, { type: 'request_rejected', workspaceUri: null, reason: 'key' });
    return httpError(403, 'invalid_identity', 'workspace is not authorized');
  }
  const workspaceUri = resolved.value;
  if (!isMcpPath(request.url)) {
    diagnose(prepared, { type: 'request_rejected', workspaceUri, reason: 'path' });
    return httpError(404, 'not_found', 'not found');
  }
  const runtime = prepared.runtime;
  if (runtime === undefined) {
    diagnose(prepared, { type: 'request_rejected', workspaceUri, reason: 'unavailable' });
    return httpError(503, 'sandbox_unavailable', UNAVAILABLE_MESSAGE);
  }
  if (prepared.owner.shuttingDown) {
    diagnose(prepared, { type: 'request_rejected', workspaceUri, reason: 'closing' });
    return httpError(503, 'sandbox_closing', 'sandbox is closing');
  }
  const lease = prepared.owner.lease<SandboxLive>(c.vars);
  if (lease === undefined) {
    // A record for this key with another activationRef owns the old gate. Do not let a new
    // activation's HTTP path send work to the runtime while that close is still unconfirmed.
    if (prepared.owner.pendingFor(c.key) !== undefined) {
      diagnose(prepared, { type: 'request_rejected', workspaceUri, reason: 'closing' });
      return httpError(503, 'sandbox_closing', 'sandbox is closing');
    }
    diagnose(prepared, { type: 'request_rejected', workspaceUri, reason: 'not_held' });
    return httpError(503, 'sandbox_not_held', 'sandbox has no active holder');
  }
  const gate = lease.value.gate;
  if (lease.retired || gate.closing) {
    diagnose(prepared, { type: 'request_rejected', workspaceUri, reason: 'closing' });
    return httpError(503, 'sandbox_closing', 'sandbox is closing');
  }
  const handle = beginRequest(c.vars, gate);
  if (handle === null) {
    diagnose(prepared, { type: 'request_rejected', workspaceUri, reason: 'not_held' });
    return httpError(503, 'sandbox_not_held', 'sandbox has no active holder');
  }
  const signal =
    request.signal === handle.controller.signal
      ? handle.controller.signal
      : AbortSignal.any([request.signal, handle.controller.signal]);
  const onAbort = (): void => {
    void cancelInflight(handle, signal.reason);
  };
  if (signal.aborted) {
    onAbort();
  } else {
    signal.addEventListener('abort', onAbort, { once: true });
  }
  try {
    const forwarded = projectRuntimeRequest(request, signal);
    const response = await runtime.fetchMcp(workspaceUri, forwarded);
    if (handle.cancelled) {
      // The request was aborted while the runtime was resolving; never expose
      // a body whose upstream is no longer covered by the drain bookkeeping.
      signal.removeEventListener('abort', onAbort);
      const wrapped = streamResponse(response, gate, handle, prepared, workspaceUri);
      if (wrapped.body !== null) {
        try {
          await wrapped.body.cancel(signal.reason);
        } catch (error) {
          // The source did not acknowledge the cancel. Keep the handle in flight so release
          // still reports drain_failed instead of closing an environment with a live body.
          diagnose(prepared, { type: 'drain_failed', workspaceUri, error });
        }
      }
      return httpError(503, 'sandbox_closing', 'sandbox is closing');
    }
    return streamResponse(response, gate, handle, prepared, workspaceUri);
  } catch (error) {
    signal.removeEventListener('abort', onAbort);
    finishRequest(gate, handle);
    diagnose(prepared, { type: 'request_failed', workspaceUri, error });
    return httpError(502, 'sandbox_mcp_failed', 'sandbox MCP request failed');
  }
}

async function teardown(c: SandboxMcpRequestContext, prepared: PreparedOptions): Promise<void> {
  retiredActivations.add(c.vars);
  const resolved = resolveWorkspaceKey(c, prepared);
  if (resolved.ok) {
    const workspaceUri = resolved.value;
    const lease = prepared.owner.lease<SandboxLive>(c.vars);
    if (lease === undefined) {
      await new SandboxActorCore(
        workspaceUri,
        c.vars,
        coreDependencies(workspaceUri, createGate(), prepared),
      ).teardown();
    } else {
      if (c.vars.refs > 0) {
        diagnose(prepared, {
          type: 'holders_lost_on_teardown',
          workspaceUri,
          refs: c.vars.refs,
        });
      }
      // An unconfirmed close stays in the host owner after this hook returns; Rivet will clear
      // the activation vars, but the gate/runtime record remains enumerable for closeAll.
      try {
        await lease.close();
      } catch (error) {
        diagnose(prepared, { type: 'close_failed', workspaceUri, error });
      }
    }
  }
  releaseSandboxHold(c.vars);
}

function assertConnectionCredential(c: SandboxMcpRequestContext, prepared: PreparedOptions): void {
  const request = c.request;
  if (request === undefined) {
    // 无请求头可验证的内部调用不是用户入口；不在这里伪造拒绝。
    return;
  }
  // RivetKit 为原始 HTTP 请求也建立一个合成连接；/mcp 由 onRequest 统一返回 401/404。
  if (isMcpPath(request.url)) {
    return;
  }
  const credential = request.headers.get('x-peri-sandbox-token');
  if (!credentialMatches(prepared, credential)) {
    throw new PeriError({ code: 'invalid_input', phase: 'input', message: CREDENTIAL_MESSAGE });
  }
}

/**
 * 一个 actor 同时管理 workspace 环境与 `/mcp` HTTP。
 *
 * key 固定为 `['sbx', canonicalWorkspaceUri]`；`authorizedWorkspaces` 与 `token` 都来自部署，
 * 请求不能声明或扩大授权。动作需要同一凭据；HTTP 还要求当前有 active holder。
 */
export function createSandboxMcpActor(options: SandboxMcpActorOptions): PeriSandboxMcpActorDefinition {
  const prepared = prepare(options);
  return actor({
    ...(prepared.options === undefined ? {} : { options: prepared.options }),
    createVars: (): SandboxActorVars => createSandboxVars(),
    onBeforeConnect: async (c) => assertConnectionCredential(c, prepared),
    onRequest: async (c, request) => await handleRequest(c, request, prepared),
    onSleep: async (c) => await teardown(c, prepared),
    onDestroy: async (c) => await teardown(c, prepared),
    actions: {
      acquire: async (c, value: string, credential: string) => {
        const outcome = await executeAction(c, prepared, credential, (core) => core.acquire(value));
        return outcome.ok ? mapAcquired(outcome.result, outcome.workspaceUri) : outcome.result;
      },
      release: async (c, value: string, credential: string) => {
        const outcome = await executeAction(c, prepared, credential, (core) => core.release(value));
        return outcome.result;
      },
      status: async (c, credential: string) => {
        const outcome = await executeAction(c, prepared, credential, (core) => core.status());
        return outcome.ok ? mapStatus(outcome.result, outcome.workspaceUri) : outcome.result;
      },
      closeAll: async (c, credential: string) => {
        // Host close reuses the same owner record and core close path as sleep/destroy: the close
        // slot drains the host-owned gate first, then closes the runtime instance.
        const outcome = await executeAction<{ readonly closed: true }>(
          c,
          prepared,
          credential,
          async (_core, lease) => {
            try {
              await lease.close();
              return { ok: true, value: { closed: true } };
            } catch (error) {
              return failureFrom(error);
            }
          },
          // Host close is the one action allowed through a retired record: it continues the same
          // single-flight close instead of rebuilding a lease.
          { allowRetiredLease: true },
        );
        return outcome.result;
      },
    },
  });
}
