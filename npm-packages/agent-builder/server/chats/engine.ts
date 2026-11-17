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
import type { ServerEnv } from '../env';
import { AppError, diagnosticCode } from '../errors';
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
  /** Optional lazy recovery hook: engines that can re-probe the Store implement it. */
  ensureReady?(): Promise<void>;
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
      open: (target) => handle.open(target),
      execute: (action) => handle.execute(action),
      facts: () => handle.facts(),
      close: () => handle.close(),
      onFact: async (listener) => {
        const connection = handle.connect();
        const detach = connection.on('chatFact', listener);
        return async () => {
          detach();
          await connection.dispose();
        };
      },
    };
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
      console.error('[agent-builder] rivet engine failed to start', { code: diagnosticCode(error) });
      throw new AppError('engine_unavailable', 'Chat runtime failed to start', {
        details: { reasons: [...this.status.reasons] },
        retryable: true,
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
 * later is reported (and can recover) without a process restart. Capabilities and
 * `POST /api/chats` therefore agree on the current state.
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
