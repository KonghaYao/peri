import {
  type ChatActivitySnapshot,
  type ChatActivitySource,
  ChatActivityTracker,
  type ChatFact,
} from '@peri-code/sdk';
import type { ChatObservation } from '@peri-code/sdk/observe';
import {
  type ActivityCounter,
  type ActivityLease,
  ActivityLedger,
  type RuntimeActivity,
} from './activity.js';

export interface ChatActivityLedgerOptions {
  readonly source?: ChatActivitySource;
  readonly onObserverError?: (error: unknown) => void;
}

/**
 * Maps the SDK's chat activity projection onto the activation ledger.
 *
 * The SDK owns fact semantics and completeness; this ledger owns Rivet lifetime decisions. Counters
 * are applied as absolute targets, and completeness is cleared before any counter drops so a
 * listener never observes a releasable snapshot mid-update.
 */
export class ChatActivityLedger {
  readonly #tracker: ChatActivityTracker;
  readonly #ledger: ActivityLedger;
  readonly #leases: Record<ActivityCounter, ActivityLease[]> = {
    activeInternalTasks: [],
    queuedInputs: [],
    pendingDeliveries: [],
    pendingInteractions: [],
    localOperations: [],
  };
  readonly #unsubscribe: () => void;
  #rootLease: ActivityLease | null = null;

  constructor(options: ChatActivityLedgerOptions = {}) {
    this.#ledger = new ActivityLedger({
      ...(options.onObserverError === undefined ? {} : { onObserverError: options.onObserverError }),
    });
    this.#tracker = new ChatActivityTracker({
      ...(options.source === undefined ? {} : { source: options.source }),
      ...(options.onObserverError === undefined ? {} : { onSourceError: options.onObserverError }),
    });
    this.#unsubscribe = this.#tracker.subscribe((snapshot) => this.#apply(snapshot));
    // The SDK observation is phase-limited; a ledger without a complete source must never release.
    this.#ledger.setComplete(false);
  }

  get activity(): RuntimeActivity {
    return this.#ledger.snapshot();
  }

  snapshot(): RuntimeActivity {
    return this.#ledger.snapshot();
  }

  start(): void {
    this.#tracker.start();
  }

  subscribe(listener: (activity: RuntimeActivity) => void): () => void {
    return this.#ledger.subscribe(listener);
  }

  beginLocalOperation(): { release(): void } {
    const release = this.#tracker.beginLocalOperation();
    return { release };
  }

  beginClose(): void {
    this.#ledger.beginClose();
  }

  applyObservation(observation: ChatObservation | null): void {
    this.#tracker.applyObservation(observation);
  }

  applyFact(fact: ChatFact): void {
    this.#tracker.applyFact(fact);
  }

  markClosed(): void {
    this.#tracker.markClosed();
  }

  dispose(): void {
    this.#unsubscribe();
    this.#tracker.dispose();
    this.#clear();
    this.#ledger.beginClose();
  }

  #apply(next: ChatActivitySnapshot): void {
    if (
      next.rootRunning === (this.#rootLease !== null) &&
      next.activeInternalTasks === this.#leases.activeInternalTasks.length &&
      next.queuedInputs === this.#leases.queuedInputs.length &&
      next.pendingDeliveries === this.#leases.pendingDeliveries.length &&
      next.pendingInteractions === this.#leases.pendingInteractions.length &&
      next.localOperations === this.#leases.localOperations.length &&
      next.complete === this.#ledger.snapshot().complete &&
      next.closing === this.#ledger.closing
    ) {
      return;
    }

    if (next.closing) {
      this.#ledger.beginClose();
    }
    this.#ledger.setComplete(false);
    this.#setRoot(next.rootRunning);
    this.#setCount('queuedInputs', next.queuedInputs);
    this.#setCount('pendingInteractions', next.pendingInteractions);
    this.#setCount('activeInternalTasks', next.activeInternalTasks);
    this.#setCount('pendingDeliveries', next.pendingDeliveries);
    this.#setCount('localOperations', next.localOperations);
    if (next.complete) {
      this.#ledger.setComplete(true);
    }
  }

  #clear(): void {
    this.#ledger.setComplete(false);
    this.#setRoot(false);
    for (const category of Object.keys(this.#leases) as ActivityCounter[]) {
      this.#setCount(category, 0);
    }
  }

  #setRoot(running: boolean): void {
    if (running && this.#rootLease === null) {
      this.#rootLease = this.#ledger.beginRoot();
      return;
    }
    if (!running && this.#rootLease !== null) {
      const lease = this.#rootLease;
      this.#rootLease = null;
      lease.release();
    }
  }

  #setCount(category: ActivityCounter, target: number): void {
    const leases = this.#leases[category];
    while (leases.length < target) {
      leases.push(this.#beginCounter(category));
    }
    while (leases.length > target) {
      leases.pop()?.release();
    }
  }

  #beginCounter(category: ActivityCounter): ActivityLease {
    switch (category) {
      case 'activeInternalTasks':
        return this.#ledger.beginInternalTask();
      case 'queuedInputs':
        return this.#ledger.beginQueuedInput();
      case 'pendingDeliveries':
        return this.#ledger.beginDelivery();
      case 'pendingInteractions':
        return this.#ledger.beginInteraction();
      case 'localOperations':
        return this.#ledger.beginLocalOperation();
    }
  }
}
