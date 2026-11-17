import { createDeferred } from '../util/deferred.js';
import { type RuntimeActivitySource, requiresActivationContext } from './activity.js';

export interface KeepAwakeHold {
  release(): void;
}

/**
 * Acquires one hold per continuously-active epoch.
 *
 * A Rivet adapter can implement this with `keepAwake(deferred.promise)`: `acquire` starts the hold
 * and `release` resolves it. Normal execution lifetime is expressed here; `waitUntil` remains
 * reserved for bounded finalization and must not be used as a keep-alive substitute.
 */
export interface KeepAwakePort {
  acquire(): KeepAwakeHold;
}

export interface KeepAwakeCoordinatorOptions {
  /**
   * Receives acquisition or release failures.
   *
   * A failing port is a host incident: the coordinator cannot promise residency, but it must not
   * corrupt the activity ledger or lose a lease. The next activity change retries the transition.
   */
  readonly onError: (error: unknown) => void;
}

export class KeepAwakeCoordinator {
  readonly #source: RuntimeActivitySource;
  readonly #port: KeepAwakePort;
  readonly #onError: (error: unknown) => void;
  #hold: KeepAwakeHold | null = null;
  #unsubscribe: (() => void) | null = null;
  #started = false;

  constructor(source: RuntimeActivitySource, port: KeepAwakePort, options: KeepAwakeCoordinatorOptions) {
    this.#source = source;
    this.#port = port;
    this.#onError = options.onError;
  }

  get holding(): boolean {
    return this.#hold !== null;
  }

  start(): void {
    if (this.#started) {
      return;
    }
    this.#started = true;
    this.#unsubscribe = this.#source.subscribe(() => {
      this.#sync();
    });
    this.#sync();
  }

  stop(): void {
    if (!this.#started) {
      return;
    }
    this.#started = false;
    this.#unsubscribe?.();
    this.#unsubscribe = null;
    const hold = this.#hold;
    this.#hold = null;
    if (hold !== null) {
      try {
        hold.release();
      } catch (error) {
        this.#onError(error);
      }
    }
  }

  #sync(): void {
    const needed = requiresActivationContext(this.#source.snapshot());

    if (needed && this.#hold === null) {
      try {
        this.#hold = this.#port.acquire();
      } catch (error) {
        this.#onError(error);
      }
      return;
    }

    if (!needed && this.#hold !== null) {
      const hold = this.#hold;
      try {
        hold.release();
        this.#hold = null;
      } catch (error) {
        this.#onError(error);
      }
    }
  }
}

/**
 * Adapts the common "keep awake until this promise settles" primitive used by actor hosts.
 *
 * The callback is invoked once per active epoch, not once per activity increment.
 */
export function promiseKeepAwakePort(keepAwake: (untilReleased: Promise<void>) => void): KeepAwakePort {
  return {
    acquire() {
      const released = createDeferred<void>();
      keepAwake(released.promise);
      return {
        release() {
          released.resolve();
        },
      };
    },
  };
}
