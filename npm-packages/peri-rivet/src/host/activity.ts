import { ActorError } from '../errors.js';
import { createDeferred, type Deferred } from '../util/deferred.js';

export const ACTIVITY_COUNTERS = [
  'activeInternalTasks',
  'queuedInputs',
  'pendingDeliveries',
  'pendingInteractions',
  'localOperations',
] as const;

export type ActivityCounter = (typeof ACTIVITY_COUNTERS)[number];

export type ActivityCategory = 'root' | ActivityCounter;

/**
 * A complete observation of one activation's execution state.
 *
 * `complete` is explicit because a real Peri adapter may not be able to observe every counter yet.
 * Consumers must treat an incomplete observation as "still needed" instead of releasing the unit.
 */
export type RuntimeActivity = Readonly<{
  revision: number;
  complete: boolean;
  rootRunning: boolean;
  activeInternalTasks: number;
  queuedInputs: number;
  pendingDeliveries: number;
  pendingInteractions: number;
  localOperations: number;
  closing: boolean;
}>;

export interface RuntimeActivitySource {
  snapshot(): RuntimeActivity;
  subscribe(listener: (activity: RuntimeActivity) => void): () => void;
}

export interface ActivityLease {
  readonly category: ActivityCategory;
  readonly released: boolean;
  release(): void;
}

export interface ActivityLedgerOptions {
  /**
   * Observer exceptions are reported here instead of unwinding through a counter mutation.
   *
   * Without a sink they are re-thrown from a microtask so the defect remains visible without
   * corrupting a lease that a caller is about to receive.
   */
  readonly onObserverError?: (error: unknown) => void;
}

/**
 * The single authority for an activation's activity counters.
 *
 * Invariants:
 * - an admission increments `queuedInputs` before `submit` can return `accepted`;
 * - a root is counted before the queued slot is released, so the total never touches zero mid-handoff;
 * - closing rejects new queued work but never hides work that is already running;
 * - every mutation publishes one immutable snapshot with a strictly increasing revision.
 */
export class ActivityLedger implements RuntimeActivitySource {
  readonly #counts: Record<ActivityCounter, number> = {
    activeInternalTasks: 0,
    queuedInputs: 0,
    pendingDeliveries: 0,
    pendingInteractions: 0,
    localOperations: 0,
  };
  #rootRunning = false;
  #closing = false;
  #complete = true;
  #revision = 0;
  readonly #listeners = new Set<(activity: RuntimeActivity) => void>();
  readonly #waiters: Deferred<RuntimeActivity>[] = [];
  readonly #onObserverError: ((error: unknown) => void) | undefined;

  constructor(options: ActivityLedgerOptions = {}) {
    this.#onObserverError = options.onObserverError;
  }

  get closing(): boolean {
    return this.#closing;
  }

  beginRoot(): ActivityLease {
    if (this.#rootRunning) {
      throw new ActorError('activity_invariant', 'a root execution is already running for this activation');
    }
    this.#rootRunning = true;
    this.#publish();
    return this.#lease('root');
  }

  beginInternalTask(): ActivityLease {
    return this.#beginCounter('activeInternalTasks');
  }

  beginQueuedInput(): ActivityLease {
    if (this.#closing) {
      throw new ActorError('runtime_closing', 'the runtime unit is closing and no longer accepts input');
    }
    return this.#beginCounter('queuedInputs');
  }

  beginDelivery(): ActivityLease {
    return this.#beginCounter('pendingDeliveries');
  }

  beginInteraction(): ActivityLease {
    return this.#beginCounter('pendingInteractions');
  }

  beginLocalOperation(): ActivityLease {
    return this.#beginCounter('localOperations');
  }

  /**
   * Stops new external admission while allowing already-accepted work to finish.
   *
   * The flag stays set for the rest of the activation; a closed unit is never reopened in place.
   */
  beginClose(): void {
    if (this.#closing) {
      return;
    }
    this.#closing = true;
    this.#publish();
  }

  /**
   * Records whether the source can observe every counter.
   *
   * A partial source (for example the SDK's phase-limited chat observation) must leave this
   * `false`; `canReleaseActivation` then refuses to release the activation even when the visible
   * counters are zero.
   */
  setComplete(complete: boolean): void {
    if (this.#complete === complete) {
      return;
    }
    this.#complete = complete;
    this.#publish();
  }

  snapshot(): RuntimeActivity {
    return this.#createSnapshot();
  }

  subscribe(listener: (activity: RuntimeActivity) => void): () => void {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  }

  waitForQuiescence(): Promise<RuntimeActivity> {
    const current = this.#createSnapshot();
    if (isQuiescent(current)) {
      return Promise.resolve(current);
    }
    const waiter = createDeferred<RuntimeActivity>();
    this.#waiters.push(waiter);
    return waiter.promise;
  }

  #beginCounter(category: ActivityCounter): ActivityLease {
    this.#counts[category] += 1;
    this.#publish();
    return this.#lease(category);
  }

  #lease(category: ActivityCategory): ActivityLease {
    return new Lease(category, () => {
      this.#release(category);
    });
  }

  #release(category: ActivityCategory): void {
    if (category === 'root') {
      if (!this.#rootRunning) {
        throw new ActorError(
          'activity_invariant',
          'root activity was released without a matching acquisition',
        );
      }
      this.#rootRunning = false;
    } else {
      const current = this.#counts[category];
      if (current <= 0) {
        throw new ActorError('activity_invariant', `${category} was released without a matching acquisition`);
      }
      this.#counts[category] = current - 1;
    }
    this.#publish();
  }

  #publish(): void {
    this.#revision += 1;
    const activity = this.#createSnapshot();

    if (isQuiescent(activity)) {
      for (const waiter of this.#waiters.splice(0)) {
        waiter.resolve(activity);
      }
    }

    for (const listener of [...this.#listeners]) {
      try {
        listener(activity);
      } catch (error) {
        this.#reportObserverError(error);
      }
    }
  }

  #reportObserverError(error: unknown): void {
    if (this.#onObserverError !== undefined) {
      this.#onObserverError(error);
      return;
    }
    queueMicrotask(() => {
      throw error;
    });
  }

  #createSnapshot(): RuntimeActivity {
    return Object.freeze({
      revision: this.#revision,
      complete: this.#complete,
      rootRunning: this.#rootRunning,
      activeInternalTasks: this.#counts.activeInternalTasks,
      queuedInputs: this.#counts.queuedInputs,
      pendingDeliveries: this.#counts.pendingDeliveries,
      pendingInteractions: this.#counts.pendingInteractions,
      localOperations: this.#counts.localOperations,
      closing: this.#closing,
    });
  }
}

export function isQuiescent(activity: RuntimeActivity): boolean {
  return (
    !activity.rootRunning &&
    activity.activeInternalTasks === 0 &&
    activity.queuedInputs === 0 &&
    activity.pendingDeliveries === 0 &&
    activity.pendingInteractions === 0 &&
    activity.localOperations === 0
  );
}

/**
 * Whether the activation must keep its execution context resident.
 *
 * Unfinished deliveries, pending user interaction and an incomplete observation all pin the unit:
 * sleeping in any of those states would make the next event unreachable.
 */
export function requiresActivationContext(activity: RuntimeActivity): boolean {
  return !activity.complete || activity.closing || !isQuiescent(activity);
}

export function canReleaseActivation(activity: RuntimeActivity): boolean {
  return activity.complete && !activity.closing && isQuiescent(activity);
}

class Lease implements ActivityLease {
  readonly category: ActivityCategory;
  #released = false;
  readonly #onRelease: () => void;

  constructor(category: ActivityCategory, onRelease: () => void) {
    this.category = category;
    this.#onRelease = onRelease;
  }

  get released(): boolean {
    return this.#released;
  }

  release(): void {
    if (this.#released) {
      return;
    }
    this.#released = true;
    this.#onRelease();
  }
}
