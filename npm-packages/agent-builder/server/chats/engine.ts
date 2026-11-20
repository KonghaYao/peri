import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import type {
  ChatExecuteResult,
  ChatFactsResult,
  ChatOpenResult,
  ChatOpenTarget,
  HostResourceOwner,
  PeriChatActorDefinition,
  PeriSandboxMcpActorDefinition,
  SandboxMcpRuntimePort,
} from '@peri-code/rivet/rivet';
import { createHostResourceOwner, createSandboxMcpActor } from '@peri-code/rivet/rivet';
import type { ChatFactEnvelope, ChatUserAction } from '@peri-code/sdk';
import type { EngineHealth } from '@shared/api';
import type { ChatRuntimeReason, ChatRuntimeStatus } from '@shared/chat';
import { isRetryableRuntime } from '@shared/chat';
import { RivetError } from 'rivetkit';
import type { ServerEnv } from '../env';
import { AppError } from '../errors';
import { type EngineTermination, terminateEngineProcess } from '../rivet/engine-process';
import { createChatHost } from '../rivet/host';
import { type StartedRegistry, type StartRegistryOptions, startChatRegistry } from '../rivet/registry';
import type { SandboxActorPort } from '../sandbox/coordinator';
import { probeStoreReachability, type StoreProbeOptions } from './store-probe';
import { StoreProbeChatEngine } from './store-recovery';

export interface ChatActorPort {
  open(target: ChatOpenTarget): Promise<ChatOpenResult>;
  execute(action: ChatUserAction): Promise<ChatExecuteResult>;
  facts(): Promise<ChatFactsResult>;
  close(): Promise<{ closed: true }>;
  /** Subscribes first, then the caller reads `facts()` to close the snapshot/live gap. */
  onFact(listener: (envelope: ChatFactEnvelope) => void): Promise<() => Promise<void>>;
}

/** Concrete sandbox actor seam; the coordinator owns holder names and retry semantics. */
export interface SandboxRuntimeHost {
  readonly runtime: SandboxMcpRuntimePort;
  readonly token: string;
  readonly machineId: string;
  readonly authorizedWorkspaces: readonly string[];
}

export interface ChatEngine {
  readonly status: ChatRuntimeStatus;
  start(): Promise<void>;
  health(): EngineHealth;
  actor(chatId: string): ChatActorPort;
  /** Null when this deployment has no sandbox actor; never a hidden in-process fallback. */
  sandbox(workspaceUri: string): SandboxActorPort | null;
  /** Loopback rivet endpoint used for gateway MCP URLs; null until the engine exists. */
  readonly endpoint: string | null;
  shutdown(): Promise<void>;
  /**
   * Terminal fallback for the native engine child. Called after shutdown, including when ordered
   * teardown stopped early; a no-op when the engine already exited.
   */
  forceStop?(): Promise<EngineTermination>;
  /** Optional lazy recovery hook: engines that can re-probe the Store implement it. */
  ensureReady?(): Promise<void>;
}

/** Rivet error groups that name the failing layer without echoing an untrusted code or message. */
const RIVET_FAILURE_GROUPS = new Set(['client', 'actor', 'engine', 'user', 'rivetkit']);

/**
 * Log-safe failure layer for a failed engine call. The group is a closed enum from rivetkit;
 * unknown shapes and groups are dropped so no message, stack or credential reaches the log.
 */
export function rivetFailureGroup(error: unknown): string | null {
  if (!RivetError.isRivetError(error)) return null;
  const group = (error as { group?: unknown }).group;
  return typeof group === 'string' && RIVET_FAILURE_GROUPS.has(group) ? group : null;
}

/**
 * A `client`-group Rivet failure means the engine child never answered (connection refused/lost).
 * Business failures (`user`/`actor`) keep the engine ready.
 */
function isClientTransportFailure(error: unknown): boolean {
  return RivetError.isRivetError(error) && (error as { group?: unknown }).group === 'client';
}

/** The engine answered the call, so its own health is unaffected even though the call failed. */
function isBusinessFailure(error: unknown): boolean {
  if (!RivetError.isRivetError(error)) return false;
  const group = (error as { group?: unknown }).group;
  return group === 'user' || group === 'actor';
}

/** Read-only loopback probe of the rivet engine, the same endpoint rivetkit health-checks. */
const ENGINE_HEALTH_PROBE_TIMEOUT_MS = 1_000;
const ENGINE_HEALTH_PATH = '/health';

/** Log-safe startup/engine diagnostic: whitelisted codes, health and ports only. */
export interface EngineStartupDiagnostic {
  readonly code: 'engine_unavailable';
  readonly status: EngineHealth['status'];
  readonly reasons: readonly ChatRuntimeReason[];
  readonly enginePort: number;
  readonly hint: string;
}

/**
 * Builds the log payload for a failed engine start. It takes no error object on purpose: a raw
 * message, stack or path can never be logged, and the hint names the actionable deployment knobs.
 */
export function engineStartupDiagnostic(
  health: EngineHealth,
  fallbackReasons: readonly ChatRuntimeReason[],
  enginePort: number,
): EngineStartupDiagnostic {
  return {
    code: 'engine_unavailable',
    status: health.status,
    reasons: [...(health.reasons ?? fallbackReasons)],
    enginePort,
    hint: `检查 RIVETKIT_STORAGE_PATH 是否存在，以及 PERI_ENGINE_PORT=${enginePort} 与端口 ${enginePort + 1} 是否被占用；修正后重启 API`,
  };
}

/** Where the SDK keeps the wasm runtime; checked before claiming the real mode works. */
function findWasmAssets(): string | null {
  try {
    const sdkEntry = import.meta.resolve('@peri-code/sdk');
    const moduleUrl = new URL('./wasm/peri-wasm.js', sdkEntry);
    const wasmUrl = new URL('./wasm/peri_wasm.wasm', sdkEntry);
    if (!existsSync(fileURLToPath(moduleUrl)) || !existsSync(fileURLToPath(wasmUrl))) return null;
    return moduleUrl.href;
  } catch {
    return null;
  }
}

/** Doctor/health seam: true when the installed SDK carries both wasm artifacts. */
export function wasmAssetsAvailable(): boolean {
  return findWasmAssets() !== null;
}

type ProbeEnv = Pick<ServerEnv, 'storeToken' | 'rivetStoragePath'>;

/**
 * Configuration-only half of the probe: it never opens a socket. `createChatEngineWithStoreProbe`
 * adds the read-only Store reachability check; both paths report through the same reason list.
 */
export function probeChatRuntime(env: ProbeEnv, assetsAvailable = wasmAssetsAvailable()): ChatRuntimeStatus {
  const reasons: ChatRuntimeReason[] = [];
  if (env.storeToken === undefined) reasons.push('store_token_missing');
  if (!assetsAvailable) reasons.push('wasm_assets_missing');
  if (env.rivetStoragePath === undefined) reasons.push('rivet_storage_path_missing');

  return { mode: reasons.length === 0 ? 'wasm' : 'not_configured', reasons };
}

export class NotConfiguredChatEngine implements ChatEngine {
  readonly status: ChatRuntimeStatus;
  readonly endpoint: string | null = null;

  constructor(status: ChatRuntimeStatus) {
    this.status = status;
  }

  start(): Promise<void> {
    return Promise.reject(this.#unavailable());
  }

  health(): EngineHealth {
    // Health carries the same reasons as capabilities so readiness is readable from one endpoint.
    return { mode: 'not_configured', status: 'unknown', reasons: [...this.status.reasons] };
  }

  actor(): ChatActorPort {
    throw this.#unavailable();
  }

  sandbox(): SandboxActorPort | null {
    return null;
  }

  shutdown(): Promise<void> {
    return Promise.resolve();
  }

  #unavailable(): AppError {
    // Only a transient Store outage can recover without a restart, so clients may retry.
    return new AppError('engine_unavailable', 'Chat runtime is not configured', {
      details: { reasons: [...this.status.reasons] },
      retryable: isRetryableRuntime(this.status.reasons),
    });
  }
}

interface RawConnection {
  on(event: 'chatFact', handler: (envelope: ChatFactEnvelope) => void): () => void;
  dispose(): Promise<void>;
}

interface RawChatHandle {
  open(target: ChatOpenTarget): Promise<ChatOpenResult>;
  execute(action: ChatUserAction): Promise<ChatExecuteResult>;
  facts(): Promise<ChatFactsResult>;
  close(): Promise<{ closed: true }>;
  connect(): RawConnection;
}

/** Injection point for tests that need to control start/shutdown ordering. */
export interface RivetChatEngineDeps {
  readonly startRegistry?: (options: StartRegistryOptions) => Promise<StartedRegistry>;
}

/** Embedded rivet engine: one local registry, one chat actor per chat id, one sandbox actor. */
export class RivetChatEngine implements ChatEngine {
  readonly status: ChatRuntimeStatus;

  readonly #env: ServerEnv;
  readonly #wasmModuleUrl: string;
  readonly #startRegistry: (options: StartRegistryOptions) => Promise<StartedRegistry>;
  readonly #owner: HostResourceOwner;
  readonly #sandbox: SandboxRuntimeHost | null;
  #startPromise: Promise<void> | null = null;
  #started: StartedRegistry | null = null;
  #health: EngineHealth['status'] = 'unknown';
  #closing = false;

  constructor(
    env: ServerEnv,
    status: ChatRuntimeStatus,
    wasmModuleUrl: string,
    owner: HostResourceOwner,
    sandbox: SandboxRuntimeHost | null,
    deps: RivetChatEngineDeps = {},
  ) {
    this.#env = env;
    this.status = status;
    this.#wasmModuleUrl = wasmModuleUrl;
    this.#owner = owner;
    this.#sandbox = sandbox;
    this.#startRegistry = deps.startRegistry ?? startChatRegistry;
  }

  get endpoint(): string | null {
    return this.#started?.endpoint ?? null;
  }

  async start(): Promise<void> {
    if (this.#closing) {
      throw new AppError('engine_unavailable', 'Chat runtime is shutting down', {
        details: { status: this.#health },
        retryable: false,
      });
    }
    if (this.#started !== null) return;
    this.#startPromise ??= this.#startOnce();
    return this.#startPromise;
  }

  health(): EngineHealth {
    return { mode: 'embedded', status: this.#health };
  }

  actor(chatId: string): ChatActorPort {
    if (this.#started === null) {
      throw new AppError('engine_unavailable', 'Chat runtime is still starting', {
        details: { status: this.#health },
        retryable: true,
      });
    }

    const handle = this.#started.client.chat.getOrCreate([chatId]) as unknown as RawChatHandle;
    return {
      open: (target) => this.#observe(() => handle.open(target)),
      execute: (action) => this.#observe(() => handle.execute(action)),
      facts: () => this.#observe(() => handle.facts()),
      close: () => this.#observe(() => handle.close()),
      onFact: async (listener) => {
        const connection = handle.connect();
        const detach = connection.on('chatFact', listener);
        return async () => {
          detach();
          await this.#observe(() => connection.dispose());
        };
      },
    };
  }

  /**
   * Keeps `health()` honest about a child that died after a successful start. A transport failure
   * flips it to `unavailable`; the next successful call restores `ready`. Business failures do not
   * change it, so a real defect on a healthy engine stays a 500 instead of becoming a 503.
   */
  #observe<T>(operation: () => Promise<T>): Promise<T> {
    return operation().then(
      (value) => {
        if (!this.#closing && this.#started !== null && this.#health === 'unavailable') {
          this.#health = 'ready';
        }
        return value;
      },
      (error: unknown) =>
        this.#recordFailure(error).then(() => {
          throw error;
        }),
    );
  }

  /**
   * Opaque failures are ambiguous: the actor's own untyped defect also arrives as an internal
   * error. Ask the child before changing health, so a real defect on a live engine keeps its 500.
   */
  async #recordFailure(error: unknown): Promise<void> {
    if (this.#closing || this.#started === null || isBusinessFailure(error)) return;
    if (isClientTransportFailure(error)) {
      this.#health = 'unavailable';
      return;
    }
    this.#health = (await this.#engineAnswers()) ? 'ready' : 'unavailable';
  }

  async #engineAnswers(): Promise<boolean> {
    const endpoint = this.#started?.endpoint;
    if (endpoint === undefined) return false;
    try {
      const response = await fetch(new URL(ENGINE_HEALTH_PATH, endpoint), {
        signal: AbortSignal.timeout(ENGINE_HEALTH_PROBE_TIMEOUT_MS),
      });
      return response.ok;
    } catch {
      return false;
    }
  }

  sandbox(workspaceUri: string): SandboxActorPort | null {
    if (this.#sandbox === null || this.#started === null) return null;
    const client = this.#started.client as unknown as {
      sandbox: {
        getOrCreate(key: [string, string]): {
          acquire(holder: string, credential: string): ReturnType<SandboxActorPort['acquire']>;
          release(holder: string, credential: string): ReturnType<SandboxActorPort['release']>;
          resolve(): Promise<string>;
        };
      };
    };
    const handle = client.sandbox.getOrCreate(['sbx', workspaceUri]);
    return {
      acquire: (holder, credential) => handle.acquire(holder, credential),
      release: (holder, credential) => handle.release(holder, credential),
      resolve: () => handle.resolve(),
    };
  }

  /** Terminal: the process is exiting, so a concurrent start must not publish a new registry. */
  async shutdown(): Promise<void> {
    this.#closing = true;
    const startup = this.#startPromise;
    // A start that is still running can publish `#started` after shutdown began; awaiting it makes
    // the registry stopped below the same one the engine just created.
    if (startup !== null) await startup.catch(() => undefined);
    const started = this.#started;
    if (started !== null) await started.shutdown();
    this.#started = null;
    this.#startPromise = null;
    this.#health = 'unknown';
    // The registry teardown is the graceful path; the recorded child is the invariant. This is a
    // no-op when the engine already exited and clears the stale pid file.
    await terminateEngineProcess();
  }

  /**
   * The engine is a native child process that `process.exit()` does not reap. This only signals
   * the PID recorded by our wrapper, so a failed or skipped graceful shutdown cannot orphan it.
   */
  forceStop(): Promise<EngineTermination> {
    this.#closing = true;
    return terminateEngineProcess();
  }

  async #startOnce(): Promise<void> {
    this.#health = 'starting';
    try {
      const chat: PeriChatActorDefinition = createChatHost(this.#env, this.#wasmModuleUrl, this.#owner);
      const sandbox: PeriSandboxMcpActorDefinition | undefined =
        this.#sandbox === null
          ? undefined
          : createSandboxMcpActor({
              owner: this.#owner,
              runtime: this.#sandbox.runtime,
              token: this.#sandbox.token,
              machineId: this.#sandbox.machineId,
              authorizedWorkspaces: this.#sandbox.authorizedWorkspaces,
              onDiagnostic: (diagnostic) => {
                // Only the kind is logged: errors may carry paths or environment detail.
                console.error('[agent-builder] sandbox actor diagnostic', diagnostic.type);
              },
            });
      const started = await this.#startRegistry({
        enginePort: this.#env.enginePort,
        chat,
        ...(sandbox === undefined ? {} : { sandbox }),
      });
      this.#started = started;
      if (this.#closing) return;
      this.#health = 'ready';
    } catch (error) {
      this.#health = 'unavailable';
      this.#startPromise = null;
      // Whitelisted fields only: the raw message/stack may carry paths or environment values.
      console.error('[agent-builder] rivet engine failed to start', {
        ...engineStartupDiagnostic(this.health(), this.status.reasons, this.#env.enginePort),
        group: rivetFailureGroup(error) ?? 'unknown',
      });
      throw new AppError('engine_unavailable', 'Chat runtime failed to start', {
        details: { reasons: [...this.status.reasons] },
        retryable: isRetryableRuntime(this.status.reasons),
        cause: error,
      });
    }
  }
}

/** Configuration-only factory for callers that cannot await (route tests). */
export function createChatEngine(env: ServerEnv, sandbox: SandboxRuntimeHost | null = null): ChatEngine {
  const status = probeChatRuntime(env);
  if (status.mode !== 'wasm') return new NotConfiguredChatEngine(status);
  const wasmModuleUrl = findWasmAssets();
  if (wasmModuleUrl === null) return new NotConfiguredChatEngine(status);
  return new RivetChatEngine(env, status, wasmModuleUrl, createHostResourceOwner(), sandbox);
}

export interface ChatEngineFactoryOptions extends StoreProbeOptions {
  readonly retryAfterMs?: number;
  readonly now?: () => number;
  readonly owner?: HostResourceOwner;
  readonly sandbox?: SandboxRuntimeHost | null;
  /** Test seam: build the real engine after the Store answers, without touching rivet. */
  readonly createReadyEngine?: () => ChatEngine;
}

/**
 * Production probe: configuration first, then one read-only Store round trip. Every wasm engine is
 * wrapped in a lazily re-probing wrapper, including the healthy-boot case, so a Store that dies
 * later is reported (and can recover) without a process restart. Session creation therefore
 * agrees with the health reflected by the engine.
 */
export async function createChatEngineWithStoreProbe(
  env: ServerEnv,
  options: ChatEngineFactoryOptions = {},
): Promise<ChatEngine> {
  const status = probeChatRuntime(env);
  if (status.mode !== 'wasm') return new NotConfiguredChatEngine(status);

  const owner = options.owner ?? createHostResourceOwner();
  const sandbox = options.sandbox ?? null;
  const createReadyEngine = options.createReadyEngine ?? (() => buildRivetEngine(env, owner, sandbox));
  const startup = await probeStoreReachability(env, options);
  const initialEngine = startup.reachable ? createReadyEngine() : null;
  // Assets can vanish between the configuration probe and this construction; that reason is not
  // recoverable by re-probing the Store, so report it directly instead of wrapping it.
  if (initialEngine !== null && initialEngine.status.mode !== 'wasm') return initialEngine;

  return new StoreProbeChatEngine({
    probe: () => probeStoreReachability(env, options),
    createReadyEngine,
    initialEngine,
    initialReasons: startup.reachable ? [] : [startup.reason],
    ...(options.retryAfterMs === undefined ? {} : { retryAfterMs: options.retryAfterMs }),
    ...(options.now === undefined ? {} : { now: options.now }),
  });
}

/** Re-checks assets because the configuration snapshot is from startup, not from this moment. */
function buildRivetEngine(
  env: ServerEnv,
  owner: HostResourceOwner,
  sandbox: SandboxRuntimeHost | null,
): ChatEngine {
  const status = probeChatRuntime(env);
  if (status.mode !== 'wasm') return new NotConfiguredChatEngine(status);

  const wasmModuleUrl = findWasmAssets();
  if (wasmModuleUrl === null) {
    // Configuration was complete at the first check; assets may still vanish mid-boot.
    return new NotConfiguredChatEngine({
      mode: 'not_configured',
      reasons: [...status.reasons, 'wasm_assets_missing'],
    });
  }
  return new RivetChatEngine(env, status, wasmModuleUrl, owner, sandbox);
}
