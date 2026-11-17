import {
  type ChatConfig,
  type ChatFactEnvelope,
  type JsonRpcNotification,
  type ReverseRequestHandler,
  RpcError,
  type Transport,
} from '@peri-code/sdk';
import {
  type ChatActorDiagnostic,
  type ChatFactsResult,
  type ChatOwnership,
  createHostResourceOwner,
  createMemoryChatOwnership,
  createNoopChatDatabase,
  createPeriChatActor,
  type HostResourceOwner,
  type OpenChatDatabase,
  WorkspaceURI,
} from '../../src/rivet/index.js';
import { sleep } from '../helpers.js';

export const STUB_MACHINE_ID = '00000000-0000-4000-8000-000000000001';

/**
 * The smallest transport a chat actor accepts: one in-memory queue and one text reply per input.
 *
 * It exists only to exercise the actor loop without a process, Store or model; nothing here is
 * exported from the package entry points.
 */

const INVALID_PARAMS = -32602;
const METHOD_NOT_FOUND = -32601;
const DEFAULT_REPLY_PREFIX = 'reply: ';

export interface StubChatRuntimeOptions {
  /** Reply text for one input; defaults to `reply: <text>`. */
  readonly reply?: (text: string) => string;
  /** Makes `session/close` reject so the close_incomplete path can be asserted. */
  readonly failClose?: boolean;
  /** Fails the next N `session/close` requests, then succeeds; for serialized retry tests. */
  readonly failCloseCount?: number;
}

export interface StubChatRuntime {
  readonly runtime: ChatConfig['runtime'];
  /** Every request/notification method the SDK sent, in order. */
  readonly requests: readonly string[];
}

interface StubSession {
  readonly id: string;
  readonly generation: string;
  revision: number;
  readonly items: Array<{ inputId: string; state: string }>;
}

export function createStubChatRuntime(options: StubChatRuntimeOptions = {}): StubChatRuntime {
  const requests: string[] = [];
  return {
    runtime: { kind: 'transport', factory: () => new StubTransport(requests, options) },
    requests,
  };
}

class StubTransport implements Transport {
  readonly #requests: string[];
  readonly #options: StubChatRuntimeOptions;
  readonly #sessions = new Map<string, StubSession>();
  readonly #listeners = new Set<(notification: JsonRpcNotification) => void>();
  readonly #timers = new Set<ReturnType<typeof setTimeout>>();
  #failCloseRemaining: number;
  #closed = false;

  constructor(requests: string[], options: StubChatRuntimeOptions) {
    this.#requests = requests;
    this.#options = options;
    this.#failCloseRemaining = options.failCloseCount ?? 0;
  }

  async request<T = unknown>(method: string, params?: unknown): Promise<T> {
    this.#requests.push(method);
    return this.#dispatch(method, params) as T;
  }

  async sendRequest<T = unknown>(method: string, params?: unknown): Promise<{ response: Promise<T> }> {
    return { response: this.request<T>(method, params) };
  }

  async notify(method: string, _params?: unknown): Promise<void> {
    this.#requests.push(method);
  }

  subscribe(listener: (notification: JsonRpcNotification) => void): () => void {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  }

  async *events(): AsyncIterableIterator<JsonRpcNotification> {}

  setRequestHandler(_handler: ReverseRequestHandler): void {}

  async close(): Promise<void> {
    this.#closed = true;
    for (const timer of this.#timers) {
      clearTimeout(timer);
    }
    this.#timers.clear();
    this.#listeners.clear();
  }

  #dispatch(method: string, params: unknown): unknown {
    switch (method) {
      case 'initialize':
        return { protocolVersion: 1, agentCapabilities: {}, authMethods: [] };
      case 'session/new': {
        asRecord(params);
        const session: StubSession = {
          id: `stub_session_${crypto.randomUUID()}`,
          generation: `stub_generation_${crypto.randomUUID()}`,
          revision: 0,
          items: [],
        };
        this.#sessions.set(session.id, session);
        return { sessionId: session.id, configOptions: [] };
      }
      case 'session/input/snapshot':
        return this.#snapshot(this.#requireSession(params));
      case 'session/input/enqueue':
        return this.#enqueue(params);
      case 'session/rename':
        this.#requireSession(params);
        return {};
      case 'session/close': {
        const failOnce = this.#failCloseRemaining > 0;
        if (failOnce) {
          this.#failCloseRemaining -= 1;
        }
        if (this.#options.failClose === true || failOnce) {
          throw new Error('session/close is unavailable');
        }
        this.#sessions.delete(requireString(asRecord(params), 'sessionId'));
        return {};
      }
      default:
        throw new RpcError(METHOD_NOT_FOUND, `the stub transport does not implement ${method}`);
    }
  }

  #enqueue(params: unknown): unknown {
    const record = asRecord(params);
    const session = this.#requireSession(params);
    const inputId = requireString(record, 'inputId');
    const text = requireString(record, 'content');
    if (record.generation !== session.generation) {
      throw new RpcError(INVALID_PARAMS, 'stub input queue generation is stale', {
        rejected: true,
        snapshot: this.#snapshot(session),
      });
    }
    if (!session.items.some((item) => item.inputId === inputId)) {
      session.items.push({ inputId, state: 'queued' });
      session.revision += 1;
      this.#startTurn(session, inputId, text);
    }
    const state = session.items.find((item) => item.inputId === inputId)?.state ?? 'unknown';
    return { results: [{ inputId, state }], snapshot: this.#snapshot(session) };
  }

  #startTurn(session: StubSession, inputId: string, text: string): void {
    const requestId = `stub_request_${crypto.randomUUID()}`;
    this.#emitAgentEvent(session.id, 'user_input_run_started', {
      generation: session.generation,
      request_id: requestId,
    });
    const timer = setTimeout(() => {
      this.#timers.delete(timer);
      this.#runTurn(session, requestId, inputId, text);
    }, 0);
    this.#timers.add(timer);
  }

  #runTurn(session: StubSession, requestId: string, inputId: string, text: string): void {
    if (this.#closed) {
      return;
    }
    const item = session.items.find((entry) => entry.inputId === inputId);
    if (item !== undefined) {
      item.state = 'delivered';
    }
    session.revision += 1;
    this.#emit('session/update', {
      sessionId: session.id,
      update: { sessionUpdate: 'user_message_chunk', messageId: inputId, content: { type: 'text', text } },
    });
    this.#emitAgentEvent(session.id, 'user_input_delivered', {
      generation: session.generation,
      input_id: inputId,
    });
    this.#emit('session/update', {
      sessionId: session.id,
      update: {
        sessionUpdate: 'agent_message_chunk',
        messageId: `stub_reply_${requestId}`,
        content: { type: 'text', text: this.#options.reply?.(text) ?? `${DEFAULT_REPLY_PREFIX}${text}` },
      },
    });
    this.#emit('peri/agent_event_done', { sessionId: session.id, requestId, stopReason: 'end_turn' });
  }

  #emitAgentEvent(sessionId: string, type: string, value: Record<string, unknown>): void {
    this.#emit('peri/agent_event', { sessionId, event_json: JSON.stringify({ type, value }) });
  }

  #emit(method: string, params: unknown): void {
    if (this.#closed) {
      return;
    }
    const notification: JsonRpcNotification = { jsonrpc: '2.0', method, params };
    for (const listener of [...this.#listeners]) {
      listener(notification);
    }
  }

  #requireSession(params: unknown): StubSession {
    const sessionId = requireString(asRecord(params), 'sessionId');
    const session = this.#sessions.get(sessionId);
    if (session === undefined) {
      throw new RpcError(INVALID_PARAMS, `the stub transport does not know session ${sessionId}`);
    }
    return session;
  }

  #snapshot(session: StubSession): unknown {
    return {
      generation: session.generation,
      revision: session.revision,
      items: session.items.map((item) => ({ ...item })),
    };
  }
}

function asRecord(params: unknown): Record<string, unknown> {
  if (params === null || typeof params !== 'object' || Array.isArray(params)) {
    throw new RpcError(INVALID_PARAMS, 'the stub transport requires an object params payload');
  }
  return params as Record<string, unknown>;
}

function requireString(record: Record<string, unknown>, key: string): string {
  const value = record[key];
  if (typeof value !== 'string' || value.length === 0) {
    throw new RpcError(INVALID_PARAMS, `the stub transport requires a non-empty ${key}`);
  }
  return value;
}

/** Actor-side harness: real actions with a fake Rivet context, no engine. */
export interface StubActorContext {
  readonly actorId: string;
  readonly key: string[];
  readonly keepAwake: <T>(promise: Promise<T>) => Promise<T>;
  readonly broadcast: (name: string, data: unknown) => void;
  vars: { chat: unknown; database: unknown; open: unknown };
}

export type StubAction = (context: unknown, ...args: unknown[]) => Promise<unknown>;

export interface StubChatActions {
  readonly open: StubAction;
  readonly execute: StubAction;
  readonly facts: StubAction;
  readonly close: StubAction;
}

export interface ChatHarness {
  readonly actions: StubChatActions;
  readonly context: StubActorContext;
  readonly owner: HostResourceOwner;
  readonly databaseOpens: () => number;
  readonly databaseCloses: () => number;
  readonly diagnostics: readonly ChatActorDiagnostic[];
  /** The definition's real lifecycle hook, for sleep/destroy close-contract tests. */
  readonly sleep: () => Promise<void>;
  readonly destroy: () => Promise<void>;
}

export interface ChatHarnessOptions {
  /** Makes the first N database closes reject. */
  readonly databaseCloseFailures?: number;
  /** Overrides the host owner so a test can inspect/close the retained record. */
  readonly owner?: HostResourceOwner;
  /** Overrides ownership so a test can count reset reads before the database opens. */
  readonly ownership?: ChatOwnership;
  /** Keeps the SDK's session/close retry window short for incomplete-close tests. */
  readonly closeDrainTimeoutMs?: number;
}

export function createChatHarness(
  runtime: ChatConfig['runtime'],
  chatId = 'chat_stub_1',
  options: ChatHarnessOptions = {},
): ChatHarness {
  const diagnostics: ChatActorDiagnostic[] = [];
  const owner = options.owner ?? createHostResourceOwner();
  let opens = 0;
  let closes = 0;
  let databaseFailures = options.databaseCloseFailures ?? 0;
  const openDatabase: OpenChatDatabase = () => {
    opens += 1;
    const database = createNoopChatDatabase();
    return {
      ...database,
      close: async () => {
        closes += 1;
        if (databaseFailures > 0) {
          databaseFailures -= 1;
          throw new Error('database close failed');
        }
      },
    };
  };
  const definition = createPeriChatActor({
    owner,
    openDatabase,
    ownership: options.ownership ?? createMemoryChatOwnership(),
    machineId: STUB_MACHINE_ID,
    chatConfig: {
      runtime,
      ...(options.closeDrainTimeoutMs === undefined
        ? {}
        : { lifecycle: { closeDrainTimeoutMs: options.closeDrainTimeoutMs } }),
    },
    onDiagnostic: (diagnostic) => {
      diagnostics.push(diagnostic);
    },
  });
  const context: StubActorContext = {
    actorId: `actor_${chatId}`,
    key: [chatId],
    keepAwake: (untilReleased) => untilReleased,
    broadcast: () => {},
    vars: { chat: null, database: null, open: null },
  };
  const hook = (name: 'onSleep' | 'onDestroy') => async (): Promise<void> => {
    const lifecycle = definition.config[name] as ((c: unknown) => Promise<void>) | undefined;
    if (lifecycle === undefined) {
      throw new Error(`the chat definition has no ${name} hook`);
    }
    await lifecycle(context);
  };
  return {
    actions: definition.config.actions as unknown as StubChatActions,
    context,
    owner,
    databaseOpens: () => opens,
    databaseCloses: () => closes,
    diagnostics,
    sleep: hook('onSleep'),
    destroy: hook('onDestroy'),
  };
}

export function chatTarget(chatId = 'chat_stub_1') {
  return {
    chatId,
    workspace: new WorkspaceURI({
      machineId: STUB_MACHINE_ID,
      runtimePath: '/tmp/peri-stub-workspace',
    }).toString(),
    agent: {},
    storage: { url: 'memory://stub' },
  };
}

export async function waitForFacts(
  actions: StubChatActions,
  context: StubActorContext,
  predicate: (facts: ChatFactsResult) => boolean,
  description: string,
): Promise<ChatFactsResult> {
  const deadline = Date.now() + 4_000;
  while (Date.now() < deadline) {
    const facts = (await actions.facts(context)) as ChatFactsResult;
    if (predicate(facts)) {
      return facts;
    }
    await sleep(5);
  }
  throw new Error(`timed out waiting for ${description}`);
}

export function agentChunkTexts(facts: ChatFactsResult): string[] {
  return facts.envelopes.flatMap(({ fact }) => {
    if (fact.kind !== 'notice' || fact.notification.method !== 'session/update') {
      return [];
    }
    const update = (
      fact.notification.params as
        | { update?: { sessionUpdate?: string; content?: { type?: string; text?: unknown } } }
        | undefined
    )?.update;
    if (update?.sessionUpdate !== 'agent_message_chunk' || update.content?.type !== 'text') {
      return [];
    }
    return typeof update.content.text === 'string' ? [update.content.text] : [];
  });
}

export function noticeMethods(envelopes: readonly ChatFactEnvelope[]): string[] {
  return envelopes.flatMap(({ fact }) => (fact.kind === 'notice' ? [fact.notification.method] : []));
}
