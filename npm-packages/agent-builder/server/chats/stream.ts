import type { ChatFactEnvelope } from '@peri-code/sdk';
import { ChatState, type ChatStateSnapshot } from '@peri-code/sdk/view';
import { publicDiagnosticCode } from '@shared/api';
import type {
  ChatClosedEvent,
  ChatMessage,
  ChatMessageEvent,
  ChatResetEvent,
  ChatSnapshotEvent,
  ChatStateEvent,
  ChatStreamEventName,
  ChatStreamState,
} from '@shared/chat';
import { AppError, diagnosticCode } from '../errors';
import type { ChatActorPort } from './engine';

export const DEFAULT_STREAM_BUFFER = 1000;

export interface StreamEvent {
  readonly seq: number;
  readonly event: ChatStreamEventName;
  readonly data: unknown;
}

/** Bounded replay log; `seq` is the SSE event id and survives reconnects. */
export class ChatEventLog {
  readonly #capacity: number;
  #events: StreamEvent[] = [];
  #seq = 0;

  constructor(capacity = DEFAULT_STREAM_BUFFER) {
    this.#capacity = capacity;
  }

  get lastSeq(): number {
    return this.#seq;
  }

  append(event: ChatStreamEventName, data: unknown): StreamEvent {
    this.#seq += 1;
    const item: StreamEvent = { seq: this.#seq, event, data };
    this.#events.push(item);
    if (this.#events.length > this.#capacity) this.#events.shift();
    return item;
  }

  /**
   * `gap` means the requested position fell out of the ring (or is ahead of it), so the caller
   * must rebuild from a snapshot instead of replaying a partial sequence.
   */
  since(seq: number): { events: StreamEvent[]; gap: boolean } {
    // Position 0 means "nothing replayed yet"; the ring may already start past 1, so replaying
    // from it cannot be guaranteed and the caller has to rebuild from a snapshot.
    if (seq <= 0 || seq > this.#seq) return { events: [], gap: true };
    const oldest = this.#events[0]?.seq;
    if (seq > 0 && oldest !== undefined && seq < oldest - 1) return { events: [], gap: true };
    if (seq > 0 && oldest === undefined) return { events: [], gap: true };
    return { events: this.#events.filter((event) => event.seq > seq), gap: false };
  }
}

type Binding = {
  readonly chatId: string;
  readonly sessionId: string;
  readonly bindingRevision: number;
};

function sameBinding(left: Binding | null, right: Binding | null): boolean {
  if (left === null || right === null) return left === right;
  return (
    left.chatId === right.chatId &&
    left.sessionId === right.sessionId &&
    left.bindingRevision === right.bindingRevision
  );
}

function messageFingerprint(message: ChatMessage): string {
  return JSON.stringify([message.role, message.status, message.createdAt, message.text]);
}

type Listener = (event: StreamEvent) => void;
type ProjectionListener = (snapshot: ChatStateSnapshot) => void;

/**
 * Transports the SDK's single projection over SSE.
 *
 * `ChatState` owns fact reduction and message merging; this hub only diffs the read-only snapshot
 * into replayable boundary events and keeps a bounded log for reconnect ordering. There is no
 * second reducer here, so history replay and live chunks cannot diverge.
 */
export class ChatStreamHub {
  readonly #chatId: string;
  readonly #actor: ChatActorPort;
  readonly #log: ChatEventLog;
  readonly #state = ChatState.empty();
  #unsubscribeState: (() => void) | null = null;
  #binding: Binding | null = null;
  /** SDK ChatService keys itself with a random id, not the app chat id; learn it from facts. */
  #actorChatId: string | null = null;
  #messages = new Map<string, { message: ChatMessage; fingerprint: string }>();
  #running = false;
  #closed = false;
  #overflow = false;
  #terminal = false;
  #startPromise: Promise<void> | null = null;
  #detach: (() => Promise<void>) | null = null;
  #listeners = new Map<Listener, () => void>();
  /** Read-only projection taps (platform recorder); they never enqueue or alter hub events. */
  #taps = new Set<ProjectionListener>();
  #live = false;
  #refresh: Promise<void> | null = null;
  #refreshBuffer: ChatFactEnvelope[] | null = null;
  #refreshRevision = 0;
  #refreshError: unknown = null;

  constructor(chatId: string, actor: ChatActorPort, capacity = DEFAULT_STREAM_BUFFER) {
    this.#chatId = chatId;
    this.#actor = actor;
    this.#log = new ChatEventLog(capacity);
    this.#unsubscribeState = this.#state.subscribe((snapshot) => this.#onSnapshot(snapshot));
  }

  get chatId(): string {
    return this.#chatId;
  }

  get terminal(): boolean {
    return this.#terminal;
  }

  get lastSeq(): number {
    return this.#log.lastSeq;
  }

  /** Live projection; taps read the same frozen snapshot the hub itself projects. */
  get snapshot(): ChatStateSnapshot {
    return this.#state.snapshot;
  }

  get pendingInteractions(): ChatStateSnapshot['pendingInteractions'] {
    return this.#state.snapshot.pendingInteractions;
  }

  /**
   * Observes every projection update after the hub has emitted its own boundary events.
   * With no taps the hub behaves exactly as before; taps never mutate hub state.
   */
  tap(listener: ProjectionListener): () => void {
    this.#taps.add(listener);
    return () => {
      this.#taps.delete(listener);
    };
  }

  start(): Promise<void> {
    this.#startPromise ??= this.#start();
    return this.#startPromise;
  }

  async ready(): Promise<void> {
    await this.start();
    if (this.#terminal) return;
    if (this.#refresh === null && this.#refreshError !== null) this.#beginRefresh();
    while (this.#refresh !== null) await this.#refresh;
    this.#assertReady();
  }

  #assertReady(): void {
    if (!this.#terminal && this.#refreshError !== null) {
      throw new AppError('engine_unavailable', 'Chat facts refresh failed', {
        retryable: true,
        cause: this.#refreshError,
      });
    }
  }

  async subscribe(
    sinceSeq: number | undefined,
    send: Listener,
    disconnected: () => void = () => {},
  ): Promise<() => void> {
    await this.ready();
    while (this.#refresh !== null) await this.#refresh;
    this.#assertReady();

    if (this.#terminal) {
      send(this.#snapshot());
      send(this.#closedEvent());
      return () => {};
    }

    if (sinceSeq === undefined || sinceSeq === 0) {
      send(this.#snapshot());
    } else {
      const replay = this.#log.since(sinceSeq);
      if (replay.gap) {
        send(this.#snapshot());
      } else {
        for (const event of replay.events) send(event);
      }
    }

    this.#listeners.set(send, disconnected);
    return () => {
      this.#listeners.delete(send);
    };
  }

  /** Emits one terminal `closed` event, then detaches every subscriber. */
  async close(reason: string | null = null): Promise<void> {
    if (this.#terminal) return;
    this.#terminal = true;
    this.#closed = true;
    this.#refreshBuffer = null;
    this.#refreshRevision = 0;
    this.#refreshError = null;
    const event = this.#log.append('closed', { reason } satisfies ChatClosedEvent);
    for (const listener of [...this.#listeners.keys()]) listener(event);
    this.#listeners.clear();
    const detach = this.#detach;
    this.#detach = null;
    if (detach) await detach().catch(() => {});
  }

  async dispose(): Promise<void> {
    this.#terminal = true;
    this.#refreshBuffer = null;
    this.#refreshRevision = 0;
    this.#refreshError = null;
    for (const disconnect of [...this.#listeners.values()]) disconnect();
    this.#listeners.clear();
    this.#taps.clear();
    this.#unsubscribeState?.();
    this.#unsubscribeState = null;
    const detach = this.#detach;
    this.#detach = null;
    if (detach) await detach().catch(() => {});
  }

  async #start(): Promise<void> {
    const buffered: ChatFactEnvelope[] = [];
    // Subscribe before reading facts: `facts()` is uncursored, so this ordering is the only way to
    // avoid losing a fact between the snapshot and the live stream.
    this.#detach = await this.#actor.onFact((envelope) => {
      if (this.#live) this.#applyLive(envelope);
      else buffered.push(envelope);
    });

    try {
      const snapshot = await this.#actor.facts();
      this.#applySnapshot(snapshot);
    } catch (error) {
      const detach = this.#detach;
      this.#detach = null;
      if (detach) await detach().catch(() => {});
      throw error;
    }

    // Let the replica publish the seeded projection silently: those messages belong in the
    // snapshot, not in the replay log, otherwise a reconnect would replay them twice.
    await Promise.resolve();
    this.#live = true;
    for (const envelope of buffered) this.#applyLive(envelope);
    if (this.#state.snapshot.closed && !this.#terminal) await this.close(null);
  }

  #applySnapshot(snapshot: {
    binding: Binding | null;
    envelopes: readonly ChatFactEnvelope[];
    overflow: boolean;
  }): void {
    // `actor.facts()` is authoritative and comes from this chat's actor handle. The SDK's
    // `binding.chatId` is a per-activation random id, so it cannot be compared with the app chat id.
    if (snapshot.binding !== null) this.#actorChatId = snapshot.binding.chatId;
    this.#overflow = snapshot.overflow;
    const bindingChanged = !sameBinding(this.#binding, snapshot.binding);
    this.#binding = snapshot.binding;

    if (snapshot.overflow) {
      this.#broadcast(this.#log.append('reset', { reason: 'overflow' } satisfies ChatResetEvent));
    }
    if (bindingChanged && this.#messages.size > 0) {
      this.#resetProjection('rebound');
    }

    if (snapshot.binding === null) {
      // A fresh chat has no binding yet; the SDK replica cannot attribute envelopes without one.
      this.#actorChatId = null;
      this.#state.apply({ binding: null, envelopes: [], overflow: snapshot.overflow });
      this.#messages.clear();
      this.#running = false;
      this.#closed = false;
      return;
    }
    this.#state.apply({
      binding: snapshot.binding,
      envelopes: [...snapshot.envelopes],
      overflow: snapshot.overflow,
    });
    // The initial projection is applied synchronously here; the subscription is not live yet, so
    // no message events precede the snapshot a subscriber receives.
  }

  #applyLive(envelope: ChatFactEnvelope): void {
    if (this.#terminal) return;
    // Until facts establish the actor identity, ask for an authoritative snapshot. Once known, a
    // different chat id is a foreign fact and must be dropped without touching the replay log.
    if (this.#actorChatId === null) {
      this.#beginRefresh();
      return;
    }
    if (envelope.chatId !== this.#actorChatId) return;
    this.#refreshRevision = Math.max(this.#refreshRevision, envelope.bindingRevision);
    if (this.#refreshBuffer !== null) {
      this.#refreshBuffer.push(envelope);
      return;
    }
    if (this.#refreshError !== null) return;
    // live fact 从不授予 binding 权威；SDK 自行拒绝旧代次，合法新代次回读 actor 快照。
    this.#state.apply(envelope);
    if (this.#binding === null || envelope.bindingRevision > this.#binding.bindingRevision) {
      this.#beginRefresh(envelope);
    }
  }

  #beginRefresh(envelope?: ChatFactEnvelope): void {
    if (this.#terminal || this.#refresh !== null) return;
    this.#refreshBuffer = envelope === undefined ? [] : [envelope];
    const revision =
      this.#refreshError === null ? this.#refreshRevision : (this.#binding?.bindingRevision ?? 0);
    this.#refresh = Promise.resolve().then(() => this.#refreshFacts(revision));
  }

  async #refreshFacts(requestedRevision: number): Promise<void> {
    try {
      while (!this.#terminal) {
        const snapshot = await this.#actor.facts();
        if (this.#terminal) return;
        const rebound = !sameBinding(this.#binding, snapshot.binding);
        this.#applySnapshot(snapshot);
        await Promise.resolve();
        // 持续接住投影微任务里的通知；只有 facts 快照可以确认新 binding。
        const buffered = this.#refreshBuffer?.splice(0) ?? [];
        for (const envelope of buffered) this.#state.apply(envelope);
        await Promise.resolve();
        if ((rebound || this.#refreshError !== null) && !this.#terminal) {
          this.#broadcast(this.#log.append('snapshot', this.#snapshot().data));
        }
        // 每个更高信号最多触发下一读；快照尚旧或同代次通知都不能驱动自旋。
        if (this.#refreshRevision <= Math.max(requestedRevision, this.#binding?.bindingRevision ?? 0)) break;
        requestedRevision = this.#refreshRevision;
      }
      this.#refreshError = null;
      this.#refreshRevision = this.#binding?.bindingRevision ?? 0;
    } catch (error) {
      if (this.#terminal) return;
      this.#refreshError = error;
      console.error('[agent-builder] chat facts refresh failed', { code: diagnosticCode(error) });
      this.#broadcast(this.#log.append('reset', { reason: 'overflow' } satisfies ChatResetEvent));
      for (const disconnect of [...this.#listeners.values()]) disconnect();
      this.#listeners.clear();
    } finally {
      const buffered = this.#refreshBuffer;
      this.#refreshBuffer = null;
      this.#refresh = null;
      if (!this.#terminal && this.#refreshError === null) {
        for (const envelope of buffered ?? []) this.#state.apply(envelope);
      }
    }
  }

  /** ChatState notifies once per microtask with the merged, frozen projection. */
  #onSnapshot(snapshot: ChatStateSnapshot): void {
    if (this.#terminal) return;
    this.#project(snapshot, this.#live);
  }

  #project(snapshot: ChatStateSnapshot, emit: boolean): void {
    const entries = new Map(snapshot.entries.map((entry) => [entry.entryId, entry]));
    for (const projected of snapshot.messages) {
      const entry = entries.get(projected.entryId);
      const message: ChatMessage = {
        id: projected.entryId,
        role: projected.role,
        text: projected.text,
        status: entry?.status ?? 'completed',
        createdAt: entry?.createdAt ?? null,
      };
      const fingerprint = messageFingerprint(message);
      const previous = this.#messages.get(message.id);
      if (previous?.fingerprint === fingerprint) continue;
      this.#messages.set(message.id, { message, fingerprint });
      if (emit) {
        this.#broadcast(this.#log.append('message', { message } satisfies ChatMessageEvent));
      }
    }

    const state: ChatStreamState = { running: snapshot.running, closed: snapshot.closed };
    if (emit && (state.running !== this.#running || state.closed !== this.#closed)) {
      this.#broadcast(this.#log.append('state', { state } satisfies ChatStateEvent));
    }
    this.#running = state.running;
    this.#closed = state.closed;

    if (emit && snapshot.closed && !this.#terminal) {
      const reason =
        snapshot.closedReason?.code === undefined ? null : publicDiagnosticCode(snapshot.closedReason.code);
      void this.close(reason);
    }

    // Taps run after the hub's own events so a recorder always observes post-boundary state.
    for (const tap of [...this.#taps]) tap(snapshot);
  }

  #resetProjection(reason: ChatResetEvent['reason']): void {
    this.#messages.clear();
    this.#running = false;
    this.#closed = false;
    this.#broadcast(this.#log.append('reset', { reason } satisfies ChatResetEvent));
  }

  #snapshot(): StreamEvent {
    const view = this.#state.snapshot;
    const messages = [...this.#messages.values()].map((entry) => entry.message);
    const data: ChatSnapshotEvent = {
      chatId: this.#chatId,
      sessionId: view.sessionId,
      bindingRevision: view.bindingRevision,
      messages,
      state: { running: this.#running, closed: this.#closed },
      lastSeq: this.#log.lastSeq,
      overflow: this.#overflow,
    };
    return { seq: this.#log.lastSeq, event: 'snapshot', data };
  }

  #closedEvent(): StreamEvent {
    return {
      seq: this.#log.lastSeq,
      event: 'closed',
      data: { reason: null } satisfies ChatClosedEvent,
    };
  }

  #broadcast(event: StreamEvent): void {
    for (const listener of this.#listeners.keys()) listener(event);
  }
}
