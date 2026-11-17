import type { EngineHealth } from '@shared/api';
import type { ChatRuntimeReason, ChatRuntimeStatus } from '@shared/chat';
import { isRetryableRuntime } from '@shared/chat';
import { AppError } from '../errors';
import type { SandboxActorPort } from '../sandbox/coordinator';
import type { ChatActorPort, ChatEngine } from './engine';
import type { StoreProbeResult } from './store-probe';

/** A Store outage that recovers must not require a process restart. */
export const STORE_RETRY_AFTER_MS = 5_000;

export interface StoreProbeChatEngineOptions {
  /** Typed probe result; auth/database failures are not retried on the TTL. */
  readonly probe: () => Promise<StoreProbeResult>;
  /** Builds the real engine after a successful probe. It must not start the engine. */
  readonly createReadyEngine: () => ChatEngine;
  /** Engine built after a successful startup probe; its runtime is `wasm`. */
  readonly initialEngine?: ChatEngine | null;
  readonly retryAfterMs?: number;
  readonly now?: () => number;
  readonly initialReasons?: readonly ChatRuntimeReason[];
}

/**
 * Keeps a wasm runtime honest about the Store: it re-probes on the same TTL whether the engine
 * started healthy or degraded. Runtime-guarded requests call `ensureReady()`; concurrent callers
 * share one probe, and a failed probe degrades the wrapper with the concrete reason so a later
 * outage is reported instead of silently serving on a dead engine. Only `store_unreachable`
 * retries on the TTL; credential/database failures keep their non-retryable reason.
 */
export class StoreProbeChatEngine implements ChatEngine {
  readonly #probe: () => Promise<StoreProbeResult>;
  readonly #createReadyEngine: () => ChatEngine;
  readonly #retryAfterMs: number;
  readonly #now: () => number;
  #reasons: ChatRuntimeReason[];
  #ready: ChatEngine | null;
  #degraded: boolean;
  #lastProbeAt: number;
  #pending: Promise<void> | null = null;
  #closing = false;

  constructor(options: StoreProbeChatEngineOptions) {
    this.#probe = options.probe;
    this.#createReadyEngine = options.createReadyEngine;
    this.#retryAfterMs = options.retryAfterMs ?? STORE_RETRY_AFTER_MS;
    this.#now = options.now ?? Date.now;
    this.#ready = options.initialEngine ?? null;
    this.#degraded = this.#ready === null;
    this.#reasons = this.#ready === null ? [...(options.initialReasons ?? ['store_unreachable'])] : [];
    // The startup probe has just run; the first retry waits for the TTL like every later one.
    this.#lastProbeAt = this.#now();
  }

  get status(): ChatRuntimeStatus {
    if (this.#ready !== null && !this.#degraded) return { mode: 'wasm', reasons: [] };
    return { mode: 'not_configured', reasons: [...this.#reasons] };
  }

  /**
   * Probe while the Store is the blocker, and keep checking after recovery so a later Store loss is
   * reported. Other reasons (missing token/key/assets) cannot change while the process runs.
   */
  #shouldProbe(): boolean {
    if (this.#closing) return false;
    if (this.#ready !== null && !this.#degraded) return true;
    return this.#reasons.length > 0 && this.#reasons.every((reason) => reason === 'store_unreachable');
  }

  /** Lazy, TTL-gated, single-flight re-probe. A failed probe degrades instead of throwing. */
  async ensureReady(): Promise<void> {
    if (!this.#shouldProbe()) return;
    if (this.#pending !== null) {
      await this.#pending;
      return;
    }
    if (this.#now() - this.#lastProbeAt < this.#retryAfterMs) return;

    this.#lastProbeAt = this.#now();
    this.#pending = this.#refresh().finally(() => {
      this.#pending = null;
    });
    await this.#pending;
  }

  async start(): Promise<void> {
    return this.#running().start();
  }

  health(): EngineHealth {
    // A degraded wrapper must not keep reporting the inner engine's last known health.
    if (this.#ready === null || this.#degraded) {
      return { mode: 'not_configured', status: 'unknown', reasons: [...this.#reasons] };
    }
    return this.#ready.health();
  }

  get endpoint(): string | null {
    return this.#ready?.endpoint ?? null;
  }

  actor(chatId: string): ChatActorPort {
    return this.#running().actor(chatId);
  }

  sandbox(workspaceUri: string): SandboxActorPort | null {
    // Release/retry must stay reachable during a Store outage; new acquires are guarded by the
    // Chat runtime check before the coordinator asks for a holder.
    return this.#ready?.sandbox(workspaceUri) ?? null;
  }

  async shutdown(): Promise<void> {
    this.#closing = true;
    const pending = this.#pending;
    if (pending !== null) await pending.catch(() => undefined);
    const ready = this.#ready;
    this.#degraded = true;
    if (ready !== null) await ready.shutdown();
    this.#ready = null;
  }

  async #refresh(): Promise<void> {
    const result = await this.#probe();
    if (this.#closing) return;

    if (!result.reachable) {
      // A previously healthy engine stays alive for open streams, but new work must see the truth.
      if (this.#ready !== null) this.#degraded = true;
      this.#reasons = [result.reason];
      return;
    }

    if (this.#ready === null) {
      const engine = this.#createReadyEngine();
      if (engine.status.mode !== 'wasm') {
        // Assets can disappear between the config probe and now; keep the specific reasons.
        this.#degraded = true;
        this.#reasons = [...engine.status.reasons];
        await engine.shutdown();
        return;
      }
      this.#ready = engine;
    }
    this.#degraded = false;
    this.#reasons = [];
  }

  #running(): ChatEngine {
    if (this.#closing) {
      throw new AppError('engine_unavailable', 'Chat runtime is shutting down', { retryable: false });
    }
    if (this.#ready === null || this.#degraded) {
      throw new AppError('engine_unavailable', 'Chat runtime is not configured', {
        details: { reasons: [...this.#reasons] },
        retryable: isRetryableRuntime(this.#reasons),
      });
    }
    return this.#ready;
  }
}
