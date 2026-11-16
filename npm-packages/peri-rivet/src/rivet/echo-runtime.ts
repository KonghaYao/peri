import {
  type ChatRuntime,
  type JsonRpcNotification,
  type ReverseRequestHandler,
  RpcError,
  type Transport,
} from '@peri-code/sdk';

/**
 * A transport-backed chat runtime that answers without ACP or an LLM.
 *
 * It exists so the actor loop (open → create_new_session → send → facts → close) can run with no
 * process or model behind it; the protocol it implements is documented in `docs/custom-runtime.md`.
 */

/** The one branch of `ChatRuntime` this module builds: a host-provided `Transport` factory. */
export type EchoChatRuntime = Extract<ChatRuntime, { kind: 'transport' }>;

/** Assistant text is `prefix + user text`; exported so tests and docs assert the same literal. */
export const ECHO_REPLY_PREFIX = 'echo: ';

/** JSON-RPC-style codes; the SDK only special-cases the queue rejection and close retry codes. */
const ACP_PROTOCOL_VERSION = 1;
const METHOD_NOT_FOUND = -32601;
const INVALID_PARAMS = -32602;
const QUEUE_REJECTED = -32602;
/** Bounded idempotency table: long-lived transports must not cache receipts without a limit. */
const MAX_COMMAND_RECEIPTS = 1024;

export function createEchoChatRuntime(): EchoChatRuntime {
  return {
    kind: 'transport',
    // One transport per activation: session ids are never shared between activations.
    factory: () => new EchoTransport(),
  };
}

type QueueState = 'queued' | 'dispatching' | 'claimed' | 'delivered' | 'withdrawn';
/** Receipts may report `unknown`: the SDK treats it as a rejected command, not a state change. */
type ReceiptState = QueueState | 'unknown';
type QueueItem = { inputId: string; state: QueueState };
type QueueSnapshot = { generation: string; revision: number; items: QueueItem[] };
type QueueReceipt = { results: Array<{ inputId: string; state: ReceiptState }>; snapshot: QueueSnapshot };

type EchoTurn = {
  readonly requestId: string;
  readonly inputId: string;
  timer: ReturnType<typeof setTimeout> | null;
};

type EchoSession = {
  readonly id: string;
  readonly generation: string;
  revision: number;
  readonly items: QueueItem[];
  /** Inputs accepted but not yet echoed, in enqueue order. */
  pending: Array<{ inputId: string; text: string }>;
  turn: EchoTurn | null;
};

/** Broadcast queue for `Transport.events()`; the SDK itself uses `subscribe`. */
class NotificationStream implements AsyncIterable<JsonRpcNotification> {
  #queue: JsonRpcNotification[] = [];
  #waiters: Array<(result: IteratorResult<JsonRpcNotification>) => void> = [];
  #closed = false;

  push(notification: JsonRpcNotification): void {
    if (this.#closed) {
      return;
    }
    const waiter = this.#waiters.shift();
    if (waiter !== undefined) {
      waiter({ value: notification, done: false });
      return;
    }
    this.#queue.push(notification);
  }

  close(): void {
    if (this.#closed) {
      return;
    }
    this.#closed = true;
    for (const waiter of this.#waiters.splice(0)) {
      waiter({ value: undefined, done: true });
    }
  }

  [Symbol.asyncIterator](): AsyncIterator<JsonRpcNotification> {
    return {
      next: (): Promise<IteratorResult<JsonRpcNotification>> => {
        const queued = this.#queue.shift();
        if (queued !== undefined) {
          return Promise.resolve({ value: queued, done: false });
        }
        if (this.#closed) {
          return Promise.resolve({ value: undefined, done: true });
        }
        return new Promise((resolve) => {
          this.#waiters.push(resolve);
        });
      },
    };
  }
}

class EchoTransport implements Transport {
  #closed = false;
  #listeners = new Set<(notification: JsonRpcNotification) => void>();
  #sessions = new Map<string, EchoSession>();
  #receipts = new Map<string, QueueReceipt>();
  #stream = new NotificationStream();
  #turnSequence = 0;

  async request<T = unknown>(method: string, params?: unknown): Promise<T> {
    return this.#dispatch(method, params) as T;
  }

  async sendRequest<T = unknown>(method: string, params?: unknown): Promise<{ response: Promise<T> }> {
    return { response: this.request<T>(method, params) };
  }

  async notify(method: string, params?: unknown): Promise<void> {
    if (this.#closed) {
      return;
    }
    if (method === 'session/cancel') {
      this.#cancelTurn(this.#requireSession(params, method));
      return;
    }
    throw new RpcError(METHOD_NOT_FOUND, `the echo runtime does not implement notification ${method}`);
  }

  subscribe(listener: (notification: JsonRpcNotification) => void): () => void {
    if (this.#closed) {
      return () => {};
    }
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  }

  events(): AsyncIterable<JsonRpcNotification> {
    return this.#stream;
  }

  /** Echo never issues reverse requests (`mcp/message`, permissions, elicitation). */
  setRequestHandler(_handler: ReverseRequestHandler): void {}

  async close(): Promise<void> {
    if (this.#closed) {
      return;
    }
    this.#closed = true;
    for (const session of this.#sessions.values()) {
      this.#dropTurn(session);
    }
    this.#sessions.clear();
    this.#receipts.clear();
    this.#listeners.clear();
    this.#stream.close();
  }

  #dispatch(method: string, params: unknown): unknown {
    if (this.#closed) {
      throw new Error(`the echo runtime transport is closed (received ${method})`);
    }
    switch (method) {
      case 'initialize':
        return { protocolVersion: ACP_PROTOCOL_VERSION, agentCapabilities: {}, authMethods: [] };
      case 'session/new':
        return this.#newSession(params, method);
      case 'session/load':
        return this.#loadSession(params, method);
      case 'session/input/snapshot':
        return this.#snapshot(this.#requireSession(params, method));
      case 'session/input/enqueue':
        return this.#enqueue(params, method);
      case 'session/input/dispatch':
        return this.#dispatchInput(params, method);
      case 'session/input/takeback':
        return this.#takeBack(params, method);
      case 'session/rename':
        this.#requireSession(params, method);
        requireString(asRecord(params, method), 'title', method);
        return {};
      case 'session/close':
        this.#closeSession(params, method);
        return {};
      default:
        throw new RpcError(METHOD_NOT_FOUND, `the echo runtime does not implement ${method}`);
    }
  }

  #newSession(params: unknown, method: string): { sessionId: string; configOptions: unknown[] } {
    // `cwd` is the only field the SDK always sends; echo creates nothing on disk.
    requireNonEmptyString(asRecord(params, method), 'cwd', method);
    const id = `echo_session_${crypto.randomUUID()}`;
    this.#sessions.set(id, newEchoSession(id));
    return { sessionId: id, configOptions: [] };
  }

  #loadSession(params: unknown, method: string): Record<string, unknown> {
    const record = asRecord(params, method);
    const sessionId = requireNonEmptyString(record, 'sessionId', method);
    // Echo keeps no history: a load yields an empty queue and replays no notifications.
    if (!this.#sessions.has(sessionId)) {
      this.#sessions.set(sessionId, newEchoSession(sessionId));
    }
    const loadGeneration = optionalString(record, 'loadGeneration');
    return {
      sessionId,
      snapshotToken: null,
      nextCursor: null,
      ...(loadGeneration === null ? {} : { _meta: { periLoadGeneration: loadGeneration } }),
    };
  }

  #enqueue(params: unknown, method: string): QueueReceipt {
    const record = asRecord(params, method);
    const session = this.#requireSession(params, method);
    const inputId = requireNonEmptyString(record, 'inputId', method);
    const text = requireString(record, 'content', method);

    // `commandId` is the idempotency key: a replayed command returns its receipt before any check.
    const commandId = optionalString(record, 'commandId');
    const cached = commandId === null ? undefined : this.#receipts.get(commandId);
    if (cached !== undefined) {
      return cached;
    }
    this.#requireGeneration(session, record, method);

    let state: QueueState = 'queued';
    if (!session.items.some((item) => item.inputId === inputId)) {
      session.items.push({ inputId, state });
      session.revision += 1;
      session.pending.push({ inputId, text });
      this.#startNextTurn(session);
    } else {
      state = session.items.find((item) => item.inputId === inputId)?.state ?? 'withdrawn';
    }

    const receipt: QueueReceipt = { results: [{ inputId, state }], snapshot: this.#snapshot(session) };
    if (commandId !== null) {
      this.#remember(commandId, receipt);
    }
    return receipt;
  }

  #dispatchInput(params: unknown, method: string): QueueReceipt {
    const record = asRecord(params, method);
    const session = this.#requireSession(params, method);
    this.#requireGeneration(session, record, method);
    const inputIds = Array.isArray(record.inputIds)
      ? record.inputIds.filter((value): value is string => typeof value === 'string' && value.length > 0)
      : [];
    if (inputIds.length === 0) {
      throw new RpcError(INVALID_PARAMS, `${method} requires a non-empty inputIds array`);
    }
    // Echo runs on enqueue, so dispatch only reports the current mailbox state.
    const results: QueueReceipt['results'] = inputIds.map((inputId) => ({
      inputId,
      state: session.items.find((item) => item.inputId === inputId)?.state ?? 'unknown',
    }));
    return this.#receipt(record, { results, snapshot: this.#snapshot(session) });
  }

  #takeBack(params: unknown, method: string): QueueReceipt {
    const record = asRecord(params, method);
    const session = this.#requireSession(params, method);
    this.#requireGeneration(session, record, method);
    const inputId = requireNonEmptyString(record, 'inputId', method);

    const item = session.items.find((entry) => entry.inputId === inputId);
    if (item === undefined) {
      return this.#receipt(record, {
        results: [{ inputId, state: 'unknown' }],
        snapshot: this.#snapshot(session),
      });
    }
    // A delivered input is already in the transcript; the mailbox cannot withdraw it.
    if (item.state === 'delivered') {
      return this.#receipt(record, {
        results: [{ inputId, state: 'unknown' }],
        snapshot: this.#snapshot(session),
      });
    }
    if (item.state !== 'withdrawn') {
      item.state = 'withdrawn';
      session.revision += 1;
      session.pending = session.pending.filter((entry) => entry.inputId !== inputId);
    }
    if (session.turn?.inputId === inputId) {
      this.#cancelTurn(session);
    }
    return this.#receipt(record, {
      results: [{ inputId, state: 'withdrawn' }],
      snapshot: this.#snapshot(session),
    });
  }

  #closeSession(params: unknown, method: string): void {
    const record = asRecord(params, method);
    const sessionId = requireNonEmptyString(record, 'sessionId', method);
    const session = this.#sessions.get(sessionId);
    if (session === undefined) {
      // Closing an already-closed session is idempotent: the SDK may replay session/close.
      return;
    }
    this.#dropTurn(session);
    this.#sessions.delete(sessionId);
  }

  #requireSession(params: unknown, method: string): EchoSession {
    const sessionId = requireNonEmptyString(asRecord(params, method), 'sessionId', method);
    const session = this.#sessions.get(sessionId);
    if (session === undefined) {
      throw new RpcError(INVALID_PARAMS, `${method} received an unknown sessionId: ${sessionId}`);
    }
    return session;
  }

  #requireGeneration(session: EchoSession, record: Record<string, unknown>, method: string): void {
    const generation = requireNonEmptyString(record, 'generation', method);
    if (generation === session.generation) {
      return;
    }
    // The SDK treats a -32602 whose data carries a new snapshot as a stale queue and blocks execution.
    throw new RpcError(QUEUE_REJECTED, `${method} rejected: input queue generation is stale`, {
      rejected: true,
      snapshot: this.#snapshot(session),
    });
  }

  #snapshot(session: EchoSession): QueueSnapshot {
    return {
      generation: session.generation,
      revision: session.revision,
      items: session.items.map((item) => ({ ...item })),
    };
  }

  #receipt(record: Record<string, unknown>, receipt: QueueReceipt): QueueReceipt {
    const commandId = optionalString(record, 'commandId');
    if (commandId !== null) {
      this.#remember(commandId, receipt);
    }
    return receipt;
  }

  #remember(commandId: string, receipt: QueueReceipt): void {
    this.#receipts.set(commandId, receipt);
    while (this.#receipts.size > MAX_COMMAND_RECEIPTS) {
      const oldest = this.#receipts.keys().next().value;
      if (oldest === undefined) {
        break;
      }
      this.#receipts.delete(oldest);
    }
  }

  #startNextTurn(session: EchoSession): void {
    if (session.turn !== null) {
      return;
    }
    while (session.pending.length > 0) {
      const next = session.pending.shift();
      if (next === undefined) {
        return;
      }
      const item = session.items.find((entry) => entry.inputId === next.inputId);
      if (item === undefined || item.state === 'withdrawn') {
        continue;
      }
      this.#beginTurn(session, next.inputId, next.text);
      return;
    }
  }

  #beginTurn(session: EchoSession, inputId: string, text: string): void {
    this.#turnSequence += 1;
    const turn: EchoTurn = { requestId: `echo_request_${this.#turnSequence}`, inputId, timer: null };
    session.turn = turn;
    this.#emitAgentEvent(session.id, 'user_input_run_started', {
      generation: session.generation,
      request_id: turn.requestId,
    });
    turn.timer = setTimeout(() => {
      turn.timer = null;
      this.#runTurn(session, turn, text);
    }, 0);
  }

  #runTurn(session: EchoSession, turn: EchoTurn, text: string): void {
    if (this.#closed || session.turn !== turn) {
      return;
    }
    const item = session.items.find((entry) => entry.inputId === turn.inputId);
    if (item === undefined || item.state === 'withdrawn') {
      this.#finishTurn(session, turn, 'cancelled');
      return;
    }
    // The user chunk shares `messageId` with the delivered fact, so a client projects one entry.
    this.#emitSessionUpdate(session.id, {
      sessionUpdate: 'user_message_chunk',
      messageId: turn.inputId,
      content: { type: 'text', text },
    });
    item.state = 'delivered';
    session.revision += 1;
    this.#emitAgentEvent(session.id, 'user_input_delivered', {
      generation: session.generation,
      input_id: turn.inputId,
    });
    this.#emitSessionUpdate(session.id, {
      sessionUpdate: 'agent_message_chunk',
      messageId: `echo_reply_${turn.requestId}`,
      content: { type: 'text', text: `${ECHO_REPLY_PREFIX}${text}` },
    });
    this.#finishTurn(session, turn, 'end_turn');
  }

  #finishTurn(session: EchoSession, turn: EchoTurn, stopReason: string): void {
    if (session.turn === turn) {
      session.turn = null;
    }
    this.#emit('peri/agent_event_done', { sessionId: session.id, requestId: turn.requestId, stopReason });
    this.#startNextTurn(session);
  }

  #cancelTurn(session: EchoSession): void {
    const turn = session.turn;
    if (turn === null) {
      return;
    }
    const item = session.items.find((entry) => entry.inputId === turn.inputId);
    if (item !== undefined && item.state !== 'delivered' && item.state !== 'withdrawn') {
      item.state = 'withdrawn';
      session.revision += 1;
    }
    session.turn = null;
    if (turn.timer !== null) {
      clearTimeout(turn.timer);
      turn.timer = null;
    }
    this.#emit('peri/agent_event_done', {
      sessionId: session.id,
      requestId: turn.requestId,
      stopReason: 'cancelled',
    });
    this.#startNextTurn(session);
  }

  #dropTurn(session: EchoSession): void {
    const timer = session.turn?.timer;
    if (timer !== null && timer !== undefined) {
      clearTimeout(timer);
    }
    session.turn = null;
    session.pending = [];
  }

  #emitSessionUpdate(sessionId: string, update: Record<string, unknown>): void {
    this.#emit('session/update', { sessionId, update });
  }

  #emitAgentEvent(sessionId: string, type: string, value: Record<string, unknown>): void {
    this.#emit('peri/agent_event', { sessionId, event_json: JSON.stringify({ type, value }) });
  }

  #emit(method: string, params: unknown): void {
    if (this.#closed) {
      return;
    }
    const notification: JsonRpcNotification = { jsonrpc: '2.0', method, params };
    this.#stream.push(notification);
    for (const listener of [...this.#listeners]) {
      listener(notification);
    }
  }
}

function newEchoSession(id: string): EchoSession {
  return {
    id,
    generation: `echo_generation_${crypto.randomUUID()}`,
    revision: 0,
    items: [],
    pending: [],
    turn: null,
  };
}

function asRecord(params: unknown, method: string): Record<string, unknown> {
  if (params === null || typeof params !== 'object' || Array.isArray(params)) {
    throw new RpcError(INVALID_PARAMS, `${method} requires an object params payload`);
  }
  return params as Record<string, unknown>;
}

function requireString(record: Record<string, unknown>, key: string, method: string): string {
  const value = record[key];
  if (typeof value !== 'string') {
    throw new RpcError(INVALID_PARAMS, `${method} requires a string ${key}`);
  }
  return value;
}

function requireNonEmptyString(record: Record<string, unknown>, key: string, method: string): string {
  const value = requireString(record, key, method);
  if (value.length === 0) {
    throw new RpcError(INVALID_PARAMS, `${method} requires a non-empty ${key}`);
  }
  return value;
}

function optionalString(record: Record<string, unknown>, key: string): string | null {
  const value = record[key];
  return typeof value === 'string' ? value : null;
}
