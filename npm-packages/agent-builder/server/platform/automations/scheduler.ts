import { diagnosticCode } from '../../errors';
import type { AutomationService } from './service';

export type AutomationTick = () => Promise<void>;

export interface AutomationSchedulerOptions {
  /** Defaults to one second; tests drive `runOnce` or inject a service clock instead. */
  readonly intervalMs?: number;
  readonly tick?: AutomationTick;
  readonly onError?: (error: unknown) => void;
}

/**
 * Owns the process-local timer. Ticks never overlap each other, while deliveries of different
 * automations stay independent through the service's per-automation single-flight set.
 */
export class AutomationScheduler {
  readonly #intervalMs: number;
  readonly #tick: AutomationTick;
  readonly #onError: (error: unknown) => void;
  #timer: ReturnType<typeof setInterval> | null = null;
  #running: Promise<void> | null = null;
  #stopped = false;

  constructor(service: AutomationService, options: AutomationSchedulerOptions = {}) {
    this.#intervalMs = options.intervalMs ?? 1_000;
    this.#tick = options.tick ?? (() => service.tick());
    this.#onError =
      options.onError ??
      ((error) => {
        console.error('[automation] scheduler tick failed', { code: diagnosticCode(error) });
      });
  }

  start(): void {
    if (this.#stopped || this.#timer !== null) return;
    this.#timer = setInterval(() => {
      void this.runOnce();
    }, this.#intervalMs);
  }

  /** Runs one tick unless the previous one is still in flight. */
  async runOnce(): Promise<void> {
    if (this.#running !== null) return;
    const run = this.#tick().catch((error: unknown) => {
      this.#onError(error);
    });
    this.#running = run;
    try {
      await run;
    } finally {
      if (this.#running === run) this.#running = null;
    }
  }

  /** Clears the timer synchronously, then waits for an in-flight tick to settle. */
  async stop(): Promise<void> {
    this.#stopped = true;
    if (this.#timer !== null) {
      clearInterval(this.#timer);
      this.#timer = null;
    }
    const running = this.#running;
    if (running !== null) await running;
  }
}
