import { ActorError } from '../errors.js';
import type { JsonValue } from '../json.js';
import { errorMessage } from '../util/error.js';

export type EvidenceCheckStatus = 'passed' | 'failed' | 'skipped';

export interface EvidenceEnvironment {
  readonly node: string;
  readonly platform: string;
  readonly arch: string;
}

export interface EvidenceCheck {
  readonly id: string;
  readonly title: string;
  readonly status: EvidenceCheckStatus;
  readonly durationMs: number;
  readonly details?: JsonValue;
  readonly error?: string;
  readonly note?: string;
}

export interface EvidenceSummary {
  readonly passed: number;
  readonly failed: number;
  readonly skipped: number;
}

export interface EvidenceReport {
  readonly runId: string;
  readonly startedAt: number;
  readonly finishedAt: number;
  readonly durationMs: number;
  readonly environment: EvidenceEnvironment;
  readonly checks: readonly EvidenceCheck[];
  readonly summary: EvidenceSummary;
}

export interface EvidenceRecorderOptions {
  readonly runId?: string;
  readonly now?: () => number;
  readonly environment?: EvidenceEnvironment;
}

/**
 * Deterministic evidence collector for scenario suites.
 *
 * It intentionally does not write files or print. Tests decide where a report goes, which keeps the
 * package free of ambient I/O while still producing the raw result record the experiment plan
 * requires.
 */
export class EvidenceRecorder {
  readonly #checks = new Map<string, EvidenceCheck>();
  readonly #runId: string;
  readonly #now: () => number;
  readonly #environment: EvidenceEnvironment;
  readonly #startedAt: number;

  constructor(options: EvidenceRecorderOptions = {}) {
    this.#now = options.now ?? Date.now;
    this.#runId = options.runId ?? `run_${this.#now()}_${globalThis.crypto.randomUUID().slice(0, 8)}`;
    this.#environment =
      options.environment ??
      ({
        node: process.version,
        platform: process.platform,
        arch: process.arch,
      } satisfies EvidenceEnvironment);
    this.#startedAt = this.#now();
  }

  async check(
    id: string,
    title: string,
    execute: () => Promise<JsonValue | undefined>,
  ): Promise<EvidenceCheck> {
    this.#assertFresh(id);
    const startedAt = this.#now();
    try {
      const details = await execute();
      const check: EvidenceCheck = {
        id,
        title,
        status: 'passed',
        durationMs: this.#now() - startedAt,
        ...(details === undefined ? {} : { details }),
      };
      this.#checks.set(id, check);
      return check;
    } catch (error) {
      const check: EvidenceCheck = {
        id,
        title,
        status: 'failed',
        durationMs: this.#now() - startedAt,
        error: errorMessage(error),
      };
      this.#checks.set(id, check);
      return check;
    }
  }

  skip(id: string, title: string, reason: string): EvidenceCheck {
    this.#assertFresh(id);
    const check: EvidenceCheck = {
      id,
      title,
      status: 'skipped',
      durationMs: 0,
      note: reason,
    };
    this.#checks.set(id, check);
    return check;
  }

  report(): EvidenceReport {
    const checks = [...this.#checks.values()];
    const finishedAt = this.#now();
    return Object.freeze({
      runId: this.#runId,
      startedAt: this.#startedAt,
      finishedAt,
      durationMs: finishedAt - this.#startedAt,
      environment: this.#environment,
      checks,
      summary: Object.freeze({
        passed: checks.filter((check) => check.status === 'passed').length,
        failed: checks.filter((check) => check.status === 'failed').length,
        skipped: checks.filter((check) => check.status === 'skipped').length,
      }),
    });
  }

  #assertFresh(id: string): void {
    if (this.#checks.has(id)) {
      throw new ActorError('invalid_input', 'Duplicate evidence check id', { id });
    }
  }
}
