import type { SandboxActorResult } from '@peri-code/rivet/rivet';
import { SANDBOX_MCP_SERVER_NAME } from '@shared/agent';
import { publicDiagnosticCode } from '@shared/api';
import type { ChatSandboxState, SandboxStatus } from '@shared/chat';
import type { ServerEnv } from '../env';
import { AppError } from '../errors';

/** The only values the Chat target receives for the injected MCP server. */
export interface SandboxInjection {
  readonly serverName: typeof SANDBOX_MCP_SERVER_NAME;
  readonly url: string;
  readonly headers: Readonly<Record<string, string>>;
}

/** Actor handle as exposed by the rivet engine; actions carry the deployment credential. */
export interface SandboxActorPort {
  acquire(holder: string, credential: string): Promise<SandboxActorResult<unknown>>;
  release(holder: string, credential: string): Promise<SandboxActorResult<{ readonly released: true }>>;
  resolve(): Promise<string>;
}

/** Engine seam: `actor` is keyed by the canonical workspace URI, never by a hash. */
export interface SandboxEnginePort {
  readonly endpoint: string;
  actor(workspaceUri: string): SandboxActorPort;
}

export interface SandboxReleaseResult {
  readonly released: boolean;
  /** 固定白名单码；原始异常只保留在内部。 */
  readonly errors: string[];
}

interface Entry {
  readonly chatId: string;
  state: ChatSandboxState['state'];
  reason: string | null;
  held: boolean;
  injection: SandboxInjection | null;
  pending: Promise<SandboxInjection> | null;
  failure: unknown;
}

const DISABLED: ChatSandboxState = { state: 'disabled', reason: null };

function newEntry(chatId: string): Entry {
  return {
    chatId,
    state: 'closed',
    reason: null,
    held: false,
    injection: null,
    pending: null,
    failure: null,
  };
}

function safeCode(error: { readonly code?: string } | undefined): string {
  return publicDiagnosticCode(error?.code, 'sandbox_failed');
}

function sandboxActorError(code: string, cause: unknown): never {
  switch (code) {
    case 'invalid_identity':
      throw new AppError('invalid_identity', 'Sandbox workspace identity was rejected', { cause });
    case 'closed':
      throw new AppError('closed', 'Sandbox environment is already closed', { cause });
    case 'sandbox_close_incomplete':
      throw new AppError('close_incomplete', 'Sandbox environment close was not confirmed', { cause });
    default:
      throw new AppError('engine_unavailable', 'Sandbox environment is unavailable', {
        details: { reason: code },
        retryable: true,
        cause,
      });
  }
}

/**
 * Per-Chat sandbox holder.
 *
 * The actor owns the workspace refcount; this coordinator owns one holder name per chat and the
 * exact MCP target injected into that chat. A failed Chat close never releases the holder, because
 * the still-running activation may be using the environment. Multiple chats on one workspace share
 * the environment through the actor's refcount; only the last confirmed release stops it.
 */
export class SandboxCoordinator {
  readonly #env: ServerEnv;
  readonly #engine: SandboxEnginePort | null;
  readonly #entries = new Map<string, Entry>();

  constructor(env: ServerEnv, engine: SandboxEnginePort | null) {
    this.#env = env;
    this.#engine = engine;
  }

  get status(): SandboxStatus {
    return { mode: this.#env.sandbox.mode, reasons: [...this.#env.sandbox.reasons] };
  }

  get enabled(): boolean {
    return this.#env.sandbox.enabled;
  }

  state(chatId: string): ChatSandboxState {
    if (!this.#env.sandbox.enabled) return DISABLED;
    const entry = this.#entries.get(chatId);
    if (!entry) return { state: 'closed', reason: null };
    return { state: entry.state, reason: entry.reason };
  }

  /**
   * Ensures the environment is ready and returns the exact HTTP MCP target. Acquire is
   * single-flight per chat; a previous incomplete release is retried before holding again.
   */
  acquire(chatId: string): Promise<SandboxInjection | null> {
    if (!this.#env.sandbox.enabled) return Promise.resolve(null);
    if (this.#env.sandbox.mode !== 'ready') {
      throw new AppError('not_ready', 'Sandbox is enabled but not configured', {
        details: { reasons: [...this.#env.sandbox.reasons] },
        retryable: false,
      });
    }
    const engine = this.#engine;
    const token = this.#env.sandbox.token;
    if (engine === null) {
      throw new AppError('engine_unavailable', 'Sandbox engine is not available', {
        details: { reason: 'sandbox_engine_missing' },
        retryable: true,
      });
    }
    if (token === undefined) {
      throw new AppError('not_ready', 'Sandbox credential is not configured', {
        details: { reason: 'sandbox_token_missing' },
        retryable: false,
      });
    }

    const entry = this.#entries.get(chatId) ?? newEntry(chatId);
    this.#entries.set(chatId, entry);
    if (entry.injection !== null) return Promise.resolve(entry.injection);
    if (entry.pending !== null) return entry.pending;

    entry.state = 'starting';
    entry.reason = null;

    const run = async (): Promise<SandboxInjection> => {
      try {
        if (entry.held) {
          const released = await this.#releaseHeld(entry);
          if (!released) {
            throw new AppError('not_ready', 'Previous sandbox release was not confirmed', {
              details: { reason: 'sandbox_release_incomplete' },
              retryable: true,
            });
          }
        }

        const actor = engine.actor(this.#env.workspaceUri);
        const acquired = await actor.acquire(chatId, token);
        if (!acquired.ok) {
          entry.failure = acquired.error;
          entry.state = 'error';
          entry.reason = safeCode(acquired.error);
          sandboxActorError(entry.reason, acquired.error);
        }
        entry.held = true;
        const actorId = await actor.resolve();
        const url = new URL(`/gateway/${encodeURIComponent(actorId)}/request/mcp`, engine.endpoint).href;
        const injection: SandboxInjection = {
          serverName: SANDBOX_MCP_SERVER_NAME,
          url,
          headers: {
            // The local RivetKit gateway credential is separate from the actor credential.
            'x-rivet-token': this.#env.sandbox.gatewayToken,
            'x-peri-sandbox-token': token,
          },
        };
        entry.injection = injection;
        entry.failure = null;
        entry.state = 'ready';
        return injection;
      } catch (error) {
        if (entry.state === 'starting') {
          entry.state = 'error';
          entry.reason = error instanceof AppError ? error.code : 'sandbox_start_failed';
        }
        throw error;
      } finally {
        entry.pending = null;
      }
    };

    const pending = run();
    entry.pending = pending;
    return pending;
  }

  /** Idempotent; never throws for an actor-level failure so close/delete can report progress. */
  async release(chatId: string): Promise<SandboxReleaseResult> {
    if (!this.#env.sandbox.enabled) return { released: true, errors: [] };
    const entry = this.#entries.get(chatId);
    if (!entry) return { released: true, errors: [] };
    const pending = entry.pending;
    if (pending !== null) await pending.catch(() => undefined);
    return this.#releaseEntry(entry);
  }

  /** Releases every holder; `released:false` means at least one close was not confirmed. */
  async closeAll(): Promise<SandboxReleaseResult> {
    const errors: string[] = [];
    let released = true;
    for (const entry of [...this.#entries.values()]) {
      const pending = entry.pending;
      if (pending !== null) await pending.catch(() => undefined);
      const result = await this.#releaseEntry(entry);
      if (!result.released) {
        released = false;
        errors.push(...result.errors);
      }
    }
    return { released, errors };
  }

  async #releaseEntry(entry: Entry): Promise<SandboxReleaseResult> {
    if (!entry.held) {
      entry.injection = null;
      entry.state = entry.state === 'error' ? 'error' : 'closed';
      return { released: true, errors: [] };
    }
    entry.state = 'releasing';
    // The old target must not be handed to a new open: it may belong to an instance that is
    // closing. If the release fails, acquire retries the release before holding again.
    entry.injection = null;
    const released = await this.#releaseHeld(entry);
    return released
      ? { released: true, errors: [] }
      : { released: false, errors: [entry.reason ?? 'sandbox_release_failed'] };
  }

  async #releaseHeld(entry: Entry): Promise<boolean> {
    const engine = this.#engine;
    const token = this.#env.sandbox.token;
    if (engine === null || token === undefined) {
      entry.state = 'incomplete';
      entry.reason = 'sandbox_engine_missing';
      return false;
    }
    try {
      const actor = engine.actor(this.#env.workspaceUri);
      const result = await actor.release(entry.chatId, token);
      if (!result.ok) {
        entry.failure = result.error;
        entry.state = 'incomplete';
        entry.reason = safeCode(result.error);
        return false;
      }
      entry.failure = null;
      entry.held = false;
      entry.injection = null;
      entry.state = 'closed';
      entry.reason = null;
      return true;
    } catch (error) {
      entry.failure = error;
      entry.state = 'incomplete';
      entry.reason = error instanceof AppError ? error.code : 'sandbox_release_failed';
      return false;
    }
  }
}
