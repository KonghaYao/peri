import { isPeriError, PeriError } from '../contracts/errors.js';
import { createDeferred, type Deferred } from '../util/deferred.js';

/**
 * Host-held owner for resources that must outlive one Rivet activation.
 *
 * Rivet 3.1.1 disposes activation vars in its hook `finally`, even when `onSleep`/`onDestroy`
 * reported an unconfirmed close. Without an owner outside the activation, an SDK/database close
 * that failed once became unreachable. This owner is created by the composition root, injected into
 * the actor factories, and kept for the process lifetime; it holds only process-local in-memory
 * records (no persistence and no replayable execution state).
 *
 * Keys are the existing actor keys verbatim. `activationRef` is only an object identity used to tell
 * one activation's callbacks from another; it is not a new business identity and never crosses the
 * wire.
 */

export type HostResourceKey = readonly string[];

/** A resource the host can close; the resource retains its own close semantics. */
export interface HostClosable {
  close(): Promise<void>;
}

/** One record that could not confirm its close. The error is for the host, never for serialization. */
export interface HostCloseFailure {
  readonly key: HostResourceKey;
  /** Opaque host handle; it carries no vars, target, headers or credentials. */
  readonly activationRef: object;
  readonly error: unknown;
}

export interface HostCloseOptions {
  /** Total deadline for `closeAll`; a timed-out close promise is retained, not cancelled. */
  readonly timeoutMs: number;
  /** Close rounds, default 2. Only explicitly retryable incomplete failures are retried. */
  readonly maxAttempts?: number;
}

export interface HostCloseReport {
  readonly complete: boolean;
  readonly pending: number;
  /** Raw failures stay process-local; callers must not serialize them into logs or responses. */
  readonly failures: readonly HostCloseFailure[];
}

export interface HostPendingResource {
  readonly key: HostResourceKey;
  /** Opaque host handle; it carries no vars, target, headers or credentials. */
  readonly activationRef: object;
}

export interface HostResourceLease<T> {
  readonly value: T;
  /** True once observers/keep-awake were detached and this lease no longer admits work. */
  readonly retired: boolean;
  close(): Promise<void>;
  /** Detach activation-bound observers; runs immediately when the record is already retired. */
  onRetire(detach: () => void): void;
}

/** Registers a closable with the record. Lower `order` closes first; chat uses 0, database 1. */
export type HostResourceTrack = (resource: HostClosable, order?: number) => void;

export interface HostResourceOwner {
  readonly shuttingDown: boolean;
  /**
   * Registers the key/activation synchronously, then runs `create` while holding the key's
   * single-flight slot. An existing record for the same key is closed first; an unconfirmed close
   * rejects this open instead of stacking a second activation.
   */
  open<T>(
    key: HostResourceKey,
    activationRef: object,
    create: (track: HostResourceTrack) => Promise<T>,
  ): Promise<HostResourceLease<T>>;
  lease<T>(activationRef: object): HostResourceLease<T> | undefined;
  pending(): readonly HostPendingResource[];
  pendingFor(key: HostResourceKey): HostPendingResource | undefined;
  /**
   * Ends the activation identity, whether or not a record exists. The mark outlives the record a
   * confirmed close removes, and no later `open` may reuse that activation.
   */
  retire(activationRef: object): void;
  beginShutdown(): void;
  closeAll(options: HostCloseOptions): Promise<HostCloseReport>;
}

interface ResourceSlot {
  readonly order: number;
  readonly resource: HostClosable;
  closed: boolean;
  closing: Promise<void> | null;
}

class ResourceRecord {
  readonly key: HostResourceKey;
  readonly encoded: string;
  /** Internal activation identity used to match repeated opens and retire calls. */
  readonly activationRef: object;
  /** Opaque handle exposed to the host; intentionally contains no activation data. */
  readonly ref: object;
  readonly #slots: ResourceSlot[] = [];
  readonly #createSettled: Deferred<void> = createDeferred<void>();
  #detach: (() => void) | null = null;
  #closePromise: Promise<void> | null = null;
  value: unknown;
  retired = false;

  constructor(key: HostResourceKey, encoded: string, activationRef: object, ref: object) {
    this.key = [...key];
    this.encoded = encoded;
    this.activationRef = activationRef;
    this.ref = ref;
  }

  track(resource: HostClosable, order: number): void {
    this.#slots.push({ order, resource, closed: false, closing: null });
    this.#slots.sort((left, right) => left.order - right.order);
  }

  onRetire(detach: () => void): void {
    if (this.retired) {
      runDetach(detach);
      return;
    }
    const previous = this.#detach;
    this.#detach =
      previous === null
        ? detach
        : () => {
            runDetach(previous);
            runDetach(detach);
          };
  }

  retire(): void {
    if (this.retired) {
      return;
    }
    this.retired = true;
    const detach = this.#detach;
    this.#detach = null;
    if (detach !== null) {
      runDetach(detach);
    }
  }

  finishCreate(): void {
    this.#createSettled.resolve();
  }

  close(): Promise<void> {
    if (this.#closePromise === null) {
      this.retire();
      this.#closePromise = this.#runClose().catch((error: unknown) => {
        // A settled failure may be retried; a still-pending close keeps its single promise.
        this.#closePromise = null;
        throw error;
      });
    }
    return this.#closePromise;
  }

  async #runClose(): Promise<void> {
    // Never close a resource while its creator is still using it: the chat database is registered
    // before `createChatSession` returns, and a shutdown can start during that await.
    await this.#createSettled.promise;
    for (const slot of this.#slots) {
      if (slot.closed) {
        continue;
      }
      if (slot.closing === null) {
        slot.closing = slot.resource.close().then(
          () => {
            slot.closed = true;
          },
          (error: unknown) => {
            slot.closing = null;
            throw error;
          },
        );
      }
      await slot.closing;
    }
  }
}

function runDetach(detach: () => void): void {
  try {
    detach();
  } catch {
    // Detaching observations is not a resource close; a failing observer must not keep the
    // resource record open or hide the real close result.
  }
}

function encodeKey(key: HostResourceKey): string {
  return JSON.stringify(key);
}

function shutdownError(): PeriError {
  return new PeriError({
    code: 'closed',
    phase: 'internal',
    message: 'the host resource owner is shutting down',
    details: { reason: 'shutting_down' },
  });
}

function retiredError(): PeriError {
  return new PeriError({
    code: 'closed',
    phase: 'internal',
    message: 'the activation lease was retired',
    details: { reason: 'retired' },
  });
}

function deadlineError(): PeriError {
  return new PeriError({
    code: 'close_incomplete',
    phase: 'internal',
    message: 'the host resource close exceeded its deadline',
    details: { reason: 'deadline' },
  });
}

function invalidOption(message: string): PeriError {
  return new PeriError({ code: 'invalid_input', phase: 'input', message });
}

/**
 * Only errors that explicitly mean "close not confirmed" may be retried. Unknown failures are
 * surfaced immediately: replaying them would hide the root cause and can duplicate side effects.
 */
function isRetryableIncomplete(error: unknown): boolean {
  if (isPeriError(error)) {
    return error.code === 'close_incomplete' || error.code === 'sandbox_close_incomplete';
  }
  if (!(error instanceof Error)) {
    return false;
  }
  return (
    error.name === 'ChatCloseIncompleteError' ||
    error.name === 'PeriInstanceCloseIncompleteError' ||
    error.name === 'PeriHostCleanupError' ||
    error.name === 'SessionCloseIncompleteError' ||
    error.name === 'DeadlineError'
  );
}

function raceDeadline<T>(promise: Promise<T>, timeoutMs: number): Promise<T> {
  if (timeoutMs === Number.POSITIVE_INFINITY) {
    return promise;
  }
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<never>((_resolve, reject) => {
    timer = setTimeout(() => reject(deadlineError()), timeoutMs);
  });
  return Promise.race([promise, timeout]).finally(() => {
    if (timer !== undefined) {
      clearTimeout(timer);
    }
  });
}

class Owner implements HostResourceOwner {
  readonly #records = new Map<string, ResourceRecord>();
  readonly #byToken = new WeakMap<object, ResourceRecord>();
  readonly #tokens = new WeakMap<object, object>();
  /**
   * Activation identities that ended. This must outlive the record: a confirmed close removes the
   * record, and without a sticky mark the same vars would start a second runtime after teardown.
   */
  readonly #retiredRefs = new WeakSet<object>();
  readonly #queues = new Map<string, Promise<void>>();
  #shuttingDown = false;

  get shuttingDown(): boolean {
    return this.#shuttingDown;
  }

  async open<T>(
    key: HostResourceKey,
    activationRef: object,
    create: (track: HostResourceTrack) => Promise<T>,
  ): Promise<HostResourceLease<T>> {
    if (!Array.isArray(key) || key.length === 0) {
      throw invalidOption('a host resource key must be a non-empty string array');
    }
    const encoded = encodeKey(key);
    return await this.#serialize(encoded, async () => {
      this.#requireAdmission();
      // Retirement is the host's own decision, so it cannot depend on a surviving lease or record.
      // Check before the existing record is closed: a retired activation must never disturb the
      // replacement record that a newer activation already owns.
      if (this.#retiredRefs.has(activationRef)) {
        throw retiredError();
      }
      const existing = this.#records.get(encoded);
      if (existing !== undefined) {
        if (existing.activationRef === activationRef) {
          if (!existing.retired) {
            return this.#leaseFor<T>(existing);
          }
          // A retired activation must not rebuild its own resources: teardown already handed the
          // record to the host close path.
          throw retiredError();
        }
        // The old lease must confirm its close before a new activation may open a database or
        // runtime. An incomplete close throws the original failure and leaves no new record.
        await this.#closeRecord(existing);
        // `beginShutdown` may linearize while the old close is awaited. Registration and `create`
        // below must be one admitted transaction: closeAll already snapshotted the records it saw,
        // so starting a factory/ownership reset/DB after that point would create an unowned resource.
        this.#requireAdmission();
      }

      // Register synchronously before the first await so pending()/closeAll can see the record
      // even when construction fails halfway through. The public ref is an empty opaque object:
      // the host can retry it without retaining target/storage credentials inside actor vars.
      const ref = {};
      this.#tokens.set(activationRef, ref);
      const record = new ResourceRecord(key, encoded, activationRef, ref);
      this.#records.set(encoded, record);
      this.#byToken.set(ref, record);

      let value: T;
      try {
        value = await create((resource, order = 0) => record.track(resource, order));
      } catch (error) {
        record.finishCreate();
        try {
          await this.#closeRecord(record);
        } catch {
          // The record stays owned for host closeAll/pending; the original construction failure is
          // what the caller must see, not a replacement close error.
        }
        throw error;
      }
      record.finishCreate();

      if (this.#shuttingDown || record.retired) {
        // A close that cannot confirm stays recorded; exposing its error is more actionable than
        // pretending the shutdown closed the resources.
        await this.#closeRecord(record);
        throw this.#shuttingDown ? shutdownError() : retiredError();
      }

      record.value = value;
      return this.#leaseFor<T>(record);
    });
  }

  lease<T>(activationRef: object): HostResourceLease<T> | undefined {
    const record = this.#recordFor(activationRef);
    if (record === undefined) {
      return undefined;
    }
    return this.#leaseFor<T>(record);
  }

  pending(): readonly HostPendingResource[] {
    return [...this.#records.values()].map((record) => ({
      key: [...record.key],
      activationRef: record.ref,
    }));
  }

  pendingFor(key: HostResourceKey): HostPendingResource | undefined {
    const record = this.#records.get(encodeKey(key));
    return record === undefined ? undefined : { key: [...record.key], activationRef: record.ref };
  }

  retire(activationRef: object): void {
    // Recorded even when no record exists yet: retirement is terminal for this activation, so a
    // later open either fails here or, if teardown already closed the record, still fails here.
    this.#retiredRefs.add(activationRef);
    this.#recordFor(activationRef)?.retire();
  }

  beginShutdown(): void {
    this.#shuttingDown = true;
    for (const record of this.#records.values()) {
      record.retire();
    }
  }

  async closeAll(options: HostCloseOptions): Promise<HostCloseReport> {
    if (!Number.isSafeInteger(options.timeoutMs) || options.timeoutMs < 0) {
      throw invalidOption('closeAll.timeoutMs must be a non-negative integer');
    }
    const maxAttempts = options.maxAttempts ?? 2;
    if (!Number.isSafeInteger(maxAttempts) || maxAttempts < 1) {
      throw invalidOption('closeAll.maxAttempts must be a positive integer');
    }
    this.beginShutdown();

    const deadline = Date.now() + options.timeoutMs;
    const failures = new Map<ResourceRecord, unknown>();
    const nonRetryable = new Set<ResourceRecord>();

    for (let attempt = 0; attempt < maxAttempts; attempt += 1) {
      const records = [...this.#records.values()];
      if (records.length === 0) {
        break;
      }
      let retryable = false;
      for (const record of records) {
        if (this.#records.get(record.encoded) !== record) {
          failures.delete(record);
          continue;
        }
        if (nonRetryable.has(record)) {
          continue;
        }
        const remaining = deadline - Date.now();
        if (remaining <= 0) {
          failures.set(record, deadlineError());
          continue;
        }
        try {
          await raceDeadline(record.close(), remaining);
          this.#removeIfCurrent(record);
          failures.delete(record);
        } catch (error) {
          failures.set(record, error);
          if (isRetryableIncomplete(error)) {
            retryable = true;
          } else {
            nonRetryable.add(record);
          }
        }
      }
      if (!retryable || Date.now() >= deadline) {
        break;
      }
    }

    const pending = [...this.#records.values()];
    return {
      complete: pending.length === 0,
      pending: pending.length,
      failures: pending.map((record) => ({
        key: [...record.key],
        activationRef: record.ref,
        error: failures.get(record) ?? deadlineError(),
      })),
    };
  }

  #leaseFor<T>(record: ResourceRecord): HostResourceLease<T> {
    return {
      get value(): T {
        return record.value as T;
      },
      get retired(): boolean {
        return record.retired;
      },
      close: async () => {
        await this.#closeRecord(record);
      },
      onRetire: (detach) => {
        record.onRetire(detach);
      },
    };
  }

  /** Called at every admission point after an awaited transition; no create may follow shutdown. */
  #requireAdmission(): void {
    if (this.#shuttingDown) {
      throw shutdownError();
    }
  }

  async #closeRecord(record: ResourceRecord): Promise<void> {
    await record.close();
    this.#removeIfCurrent(record);
  }

  #recordFor(ref: object): ResourceRecord | undefined {
    const direct = this.#byToken.get(ref);
    if (direct !== undefined) {
      return direct;
    }
    const token = this.#tokens.get(ref);
    return token === undefined ? undefined : this.#byToken.get(token);
  }

  #removeIfCurrent(record: ResourceRecord): void {
    // A late close from an old lease must never delete the replacement record for the same key.
    if (this.#records.get(record.encoded) === record) {
      this.#records.delete(record.encoded);
    }
    if (this.#byToken.get(record.ref) === record) {
      this.#byToken.delete(record.ref);
    }
    if (this.#tokens.get(record.activationRef) === record.ref) {
      this.#tokens.delete(record.activationRef);
    }
  }

  async #serialize<T>(encoded: string, run: () => Promise<T>): Promise<T> {
    const previous = this.#queues.get(encoded) ?? Promise.resolve();
    let settle: (() => void) | undefined;
    const current = new Promise<void>((resolve) => {
      settle = resolve;
    });
    const tail = previous.then(() => current);
    this.#queues.set(encoded, tail);
    await previous;
    try {
      return await run();
    } finally {
      settle?.();
      if (this.#queues.get(encoded) === tail) {
        this.#queues.delete(encoded);
      }
    }
  }
}

/** Creates the explicit host owner; the composition root must hold it for the process lifetime. */
export function createHostResourceOwner(): HostResourceOwner {
  return new Owner();
}
