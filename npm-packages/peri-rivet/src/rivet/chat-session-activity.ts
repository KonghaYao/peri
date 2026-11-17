import type {
  ChatActivitySource,
  ChatFactEnvelope,
  ChatService as ChatServiceContract,
} from '@peri-code/sdk';
import type { ChatObservation } from '@peri-code/sdk/observe';
import type { RuntimeActivity } from '../host/activity.js';
import { ChatActivityLedger } from '../host/chat-activity.js';
import { KeepAwakeCoordinator, type KeepAwakePort } from '../host/keep-awake.js';

export type ChatActivityDiagnosticType =
  | 'activity_source_failed'
  | 'fact_observer_failed'
  | 'keep_awake_failed';

export interface ChatActivationActivityOptions {
  readonly chat: ChatServiceContract;
  /** Complete activity source; without it the ledger stays incomplete and never releases. */
  readonly activity?: ChatActivitySource;
  /** Rivet's `c.keepAwake` bridge; omitted in tests and non-Rivet hosts. */
  readonly keepAwake?: KeepAwakePort;
  /** Receives live facts for realtime delivery; replay facts are not repeated here. */
  readonly onFact?: (envelope: ChatFactEnvelope) => void;
  readonly onDiagnostic?: (type: ChatActivityDiagnosticType, error: unknown) => void;
}

/**
 * Keep-alive projection for one chat activation.
 *
 * The SDK owns fact semantics; this adapter only feeds facts and observations into the activity
 * ledger and releases Rivet's keep-awake hold once the ledger is quiescent. It never closes the
 * SDK handle: the actor owns close ordering, so a failed chat close still stops this projection.
 */
export class ChatActivationActivity {
  readonly #chat: ChatServiceContract;
  readonly #ledger: ChatActivityLedger;
  readonly #coordinator: KeepAwakeCoordinator | undefined;
  readonly #onFact: ((envelope: ChatFactEnvelope) => void) | undefined;
  readonly #onDiagnostic: ChatActivationActivityOptions['onDiagnostic'];
  #unsubscribeFacts: (() => void) | undefined;
  #started = false;
  #stopped = false;

  constructor(options: ChatActivationActivityOptions) {
    this.#chat = options.chat;
    this.#onFact = options.onFact;
    this.#onDiagnostic = options.onDiagnostic;
    this.#ledger = new ChatActivityLedger({
      ...(options.activity === undefined ? {} : { source: options.activity }),
      onObserverError: (error) => this.#diagnose('activity_source_failed', error),
    });
    this.#coordinator =
      options.keepAwake === undefined
        ? undefined
        : new KeepAwakeCoordinator(this.#ledger, options.keepAwake, {
            onError: (error) => this.#diagnose('keep_awake_failed', error),
          });
  }

  activity(): RuntimeActivity {
    return this.#ledger.activity;
  }

  observe(): ChatObservation | null {
    try {
      return this.#chat.observe?.() ?? null;
    } catch (error) {
      this.#diagnose('activity_source_failed', error);
      return null;
    }
  }

  start(): void {
    if (this.#started || this.#stopped) {
      return;
    }
    this.#started = true;
    this.#unsubscribeFacts = this.#chat.subscribeFacts({
      onBinding: () => {
        this.refresh();
      },
      onFact: (envelope) => {
        this.#ledger.applyFact(envelope.fact);
        this.#publish(envelope);
        this.refresh();
      },
      onClosed: () => {
        // The SDK closed on its own: there is nothing left to drain, so release immediately.
        this.#ledger.markClosed();
        this.#coordinator?.stop();
      },
    });
    this.#ledger.start();
    this.#coordinator?.start();
    this.refresh();
  }

  /** Runs one action while pinning the activation, then re-reads the counters it may have moved. */
  async run<T>(operation: () => Promise<T>): Promise<T> {
    if (!this.#started || this.#stopped) {
      return await operation();
    }
    const lease = this.#ledger.beginLocalOperation();
    try {
      return await operation();
    } finally {
      lease.release();
      this.refresh();
    }
  }

  refresh(): void {
    if (this.#stopped) {
      return;
    }
    this.#ledger.applyObservation(this.observe());
  }

  /** Stops observation and releases the keep-awake hold; call after the SDK handle is closed. */
  stop(): void {
    if (this.#stopped) {
      return;
    }
    this.#stopped = true;
    this.#unsubscribeFacts?.();
    this.#unsubscribeFacts = undefined;
    this.#ledger.dispose();
    this.#coordinator?.stop();
  }

  #publish(envelope: ChatFactEnvelope): void {
    if (this.#onFact === undefined) {
      return;
    }
    try {
      this.#onFact(envelope);
    } catch (error) {
      this.#diagnose('fact_observer_failed', error);
    }
  }

  #diagnose(type: ChatActivityDiagnosticType, error: unknown): void {
    try {
      this.#onDiagnostic?.(type, error);
    } catch {
      // Diagnostics are observational; they must not affect the ledger or keep-awake state.
    }
  }
}
