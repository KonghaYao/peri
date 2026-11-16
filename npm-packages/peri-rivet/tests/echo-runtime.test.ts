import { describe, expect, test } from 'bun:test';
import {
  type ChatConfig,
  type ChatFactEnvelope,
  ChatFactReplica,
  type JsonRpcNotification,
  RpcError,
  type Transport,
} from '@peri-code/sdk';
import { UserError } from 'rivetkit';
import {
  type ChatExecuteResult,
  type ChatFactsResult,
  type ChatOwnership,
  createEchoChatRuntime,
  createNoopChatDatabase,
  createPeriChatActor,
  ECHO_REPLY_PREFIX,
  type EchoChatRuntime,
} from '../src/rivet/index.js';
import { sleep } from './helpers.js';

const CHAT_ID = 'chat_echo_1';

interface FakeContext {
  readonly actorId: string;
  readonly key: string[];
  readonly keepAwake: <T>(promise: Promise<T>) => Promise<T>;
  readonly broadcast: (name: string, data: unknown) => void;
  vars: { chat: unknown; database: unknown; open: unknown };
}

type ActionCall = (context: unknown, ...args: unknown[]) => Promise<unknown>;

interface ChatActions {
  readonly open: ActionCall;
  readonly execute: ActionCall;
  readonly facts: ActionCall;
  readonly close: ActionCall;
}

interface Harness {
  readonly actions: ChatActions;
  readonly diagnostics: readonly { type: string }[];
  readonly databaseCloses: () => number;
}

function actorContext(): FakeContext {
  return {
    actorId: 'actor_chat_echo',
    key: [CHAT_ID],
    keepAwake: (untilReleased) => untilReleased,
    broadcast: () => {},
    vars: { chat: null, database: null, open: null },
  };
}

function createHarness(runtime: ChatConfig['runtime'] = createEchoChatRuntime()): Harness {
  const records = new Map<string, unknown>();
  const ownership: ChatOwnership = {
    getItem: async (key) => records.get(key) ?? null,
    setItem: async (key, value) => {
      records.set(key, value);
    },
    removeItem: async (key) => {
      records.delete(key);
    },
  };
  const diagnostics: { type: string }[] = [];
  let closes = 0;
  const definition = createPeriChatActor({
    openDatabase: () => {
      const database = createNoopChatDatabase();
      return {
        ...database,
        close: async () => {
          closes += 1;
        },
      };
    },
    ownership,
    machineId: 'machine_echo',
    chatConfig: { runtime },
    onDiagnostic: (diagnostic) => {
      diagnostics.push(diagnostic);
    },
  });
  return {
    actions: definition.config.actions as unknown as ChatActions,
    diagnostics,
    databaseCloses: () => closes,
  };
}

function target() {
  return {
    chatId: CHAT_ID,
    workspace: { id: 'ws_echo', runtimePath: '/tmp/peri-echo-workspace' },
    agent: {},
    storage: { url: 'memory://echo' },
  };
}

/** Wraps a runtime so a test can assert which transport requests the SDK actually made. */
function recordingRuntime(base: EchoChatRuntime = createEchoChatRuntime()): {
  readonly runtime: EchoChatRuntime;
  readonly methods: string[];
} {
  const methods: string[] = [];
  const runtime: EchoChatRuntime = {
    kind: 'transport',
    factory: async (path, options) => {
      const transport = await base.factory(path, options);
      const recording: Transport = {
        request: async <T = unknown>(method: string, params?: unknown): Promise<T> => {
          methods.push(method);
          return transport.request<T>(method, params);
        },
        sendRequest: (method, params) => transport.sendRequest(method, params),
        notify: async (method, params) => {
          methods.push(method);
          return transport.notify(method, params);
        },
        subscribe: (listener) => transport.subscribe(listener),
        events: () => transport.events(),
        setRequestHandler: (handler) => transport.setRequestHandler(handler),
        close: () => transport.close(),
      };
      return recording;
    },
  };
  return { runtime, methods };
}

async function openAndCreate(harness: Harness, context: FakeContext): Promise<string> {
  await harness.actions.open(context, target());
  const created = (await harness.actions.execute(context, {
    type: 'create_new_session',
    requestId: 'req_create',
    payload: {},
  })) as ChatExecuteResult;
  if (created.type !== 'create_new_session') {
    throw new Error(`expected create_new_session, received ${created.type}`);
  }
  return created.result.sessionId;
}

async function send(harness: Harness, context: FakeContext, requestId: string, text: string): Promise<void> {
  const result = (await harness.actions.execute(context, {
    type: 'send',
    requestId,
    payload: { text },
  })) as ChatExecuteResult;
  if (result.type !== 'send') {
    throw new Error(`expected send, received ${result.type}`);
  }
  expect(result.target.sessionId.length).toBeGreaterThan(0);
}

async function facts(harness: Harness, context: FakeContext): Promise<ChatFactsResult> {
  return (await harness.actions.facts(context)) as ChatFactsResult;
}

async function waitForFacts(
  harness: Harness,
  context: FakeContext,
  predicate: (facts: ChatFactsResult) => boolean,
  description: string,
): Promise<ChatFactsResult> {
  const deadline = Date.now() + 4_000;
  while (Date.now() < deadline) {
    const current = await facts(harness, context);
    if (predicate(current)) {
      return current;
    }
    await sleep(5);
  }
  throw new Error(`timed out waiting for ${description}`);
}

function notices(result: ChatFactsResult): JsonRpcNotification[] {
  return result.envelopes.flatMap((envelope: ChatFactEnvelope) =>
    envelope.fact.kind === 'notice' ? [envelope.fact.notification] : [],
  );
}

function agentEventTypes(result: ChatFactsResult): string[] {
  return notices(result).flatMap((notification) => {
    if (notification.method !== 'peri/agent_event') {
      return [];
    }
    const eventJson = (notification.params as { event_json?: unknown } | undefined)?.event_json;
    if (typeof eventJson !== 'string') {
      return [];
    }
    const decoded = JSON.parse(eventJson) as { type?: unknown };
    return typeof decoded.type === 'string' ? [decoded.type] : [];
  });
}

function chunkTexts(result: ChatFactsResult): string[] {
  return notices(result).flatMap((notification) => {
    if (notification.method !== 'session/update') {
      return [];
    }
    const update = (notification.params as { update?: { content?: { text?: unknown } } } | undefined)?.update;
    const text = update?.content?.text;
    return typeof text === 'string' ? [text] : [];
  });
}

async function projectTexts(result: ChatFactsResult): Promise<string[]> {
  const binding = result.binding;
  if (binding === null) {
    throw new Error('facts have no binding to project');
  }
  const replica = new ChatFactReplica({ binding });
  try {
    replica.applySnapshot({ binding, envelopes: result.envelopes, overflow: result.overflow });
    await sleep(0);
    const view = replica.read();
    expect(view.readiness).toBe('ready');
    return view.entries.flatMap((entry) =>
      entry.blocks.flatMap((block) => (block.type === 'text' ? [block.text] : [])),
    );
  } finally {
    replica.dispose();
  }
}

describe('echo chat runtime', () => {
  test('implements the documented request shapes and per-turn event order', async () => {
    const transport = await createEchoChatRuntime().factory('/tmp/peri-echo-workspace');
    const notifications: JsonRpcNotification[] = [];
    transport.subscribe((notification) => {
      notifications.push(notification);
    });

    await expect(
      transport.request('initialize', { protocolVersion: 1, clientCapabilities: {} }),
    ).resolves.toEqual({ protocolVersion: 1, agentCapabilities: {}, authMethods: [] });

    const created = (await transport.request('session/new', {
      cwd: '/tmp/peri-echo-workspace',
      mcpServers: [],
    })) as { sessionId: string; configOptions: unknown[] };
    expect(created.configOptions).toEqual([]);

    const snapshot = (await transport.request('session/input/snapshot', {
      sessionId: created.sessionId,
    })) as { generation: string; revision: number; items: unknown[] };
    expect(snapshot).toMatchObject({ revision: 0, items: [] });

    const enqueue = {
      sessionId: created.sessionId,
      generation: snapshot.generation,
      commandId: 'input_1:enqueue',
      inputId: 'input_1',
      content: 'hi',
      originalDraft: 'hi',
    };
    const receipt = await transport.request('session/input/enqueue', enqueue);
    expect(receipt).toMatchObject({ results: [{ inputId: 'input_1', state: 'queued' }] });

    const deadline = Date.now() + 2_000;
    while (notifications.length < 5 && Date.now() < deadline) {
      await sleep(2);
    }
    expect(notifications.map((notification) => notification.method)).toEqual([
      'peri/agent_event',
      'session/update',
      'peri/agent_event',
      'session/update',
      'peri/agent_event_done',
    ]);

    // `commandId` is idempotent: the same command replays its receipt without another turn.
    await expect(transport.request('session/input/enqueue', enqueue)).resolves.toEqual(receipt);
    await sleep(10);
    expect(notifications).toHaveLength(5);

    const takenBack = (await transport.request('session/input/takeback', {
      sessionId: created.sessionId,
      generation: snapshot.generation,
      commandId: 'input_1:takeback',
      inputId: 'input_1',
    })) as { results: Array<{ state: string }> };
    expect(takenBack.results[0]?.state).toBe('unknown');

    let stale: unknown;
    try {
      await transport.request('session/input/enqueue', {
        ...enqueue,
        commandId: 'input_2:enqueue',
        inputId: 'input_2',
        generation: 'stale_generation',
      });
    } catch (error) {
      stale = error;
    }
    expect(stale).toBeInstanceOf(RpcError);
    expect((stale as RpcError).code).toBe(-32602);
    expect((stale as RpcError).data).toMatchObject({ rejected: true });

    await expect(
      transport.request('session/load', {
        sessionId: 'echo_session_other',
        cwd: '/tmp/peri-echo-workspace',
        mcpServers: [],
        loadGeneration: 'load_generation_1',
      }),
    ).resolves.toEqual({
      sessionId: 'echo_session_other',
      snapshotToken: null,
      nextCursor: null,
      _meta: { periLoadGeneration: 'load_generation_1' },
    });

    await expect(transport.request('session/close', { sessionId: created.sessionId })).resolves.toEqual({});
    await expect(transport.request('session/close', { sessionId: created.sessionId })).resolves.toEqual({});
    await transport.close();
  });

  test('open → create_new_session → send → facts → close runs without an ACP backend', async () => {
    const recording = recordingRuntime();
    const harness = createHarness(recording.runtime);
    const context = actorContext();

    const sessionId = await openAndCreate(harness, context);
    expect(typeof sessionId).toBe('string');
    expect(sessionId.startsWith('echo_session_')).toBe(true);

    await send(harness, context, 'req_send_1', 'hello');
    const result = await waitForFacts(
      harness,
      context,
      (current) =>
        chunkTexts(current).includes(`${ECHO_REPLY_PREFIX}hello`) &&
        current.envelopes.some((envelope) => envelope.fact.kind === 'turn-complete'),
      'the first echo turn',
    );

    expect(notices(result).map((notification) => notification.method)).toEqual([
      'peri/agent_event',
      'session/update',
      'peri/agent_event',
      'session/update',
      'peri/agent_event_done',
    ]);
    expect(agentEventTypes(result)).toEqual(['user_input_run_started', 'user_input_delivered']);

    const ordered = notices(result);
    const started = ordered[0];
    const done = ordered[4];
    if (started === undefined || done === undefined) {
      throw new Error('expected five notifications for one echo turn');
    }
    const startedJson = JSON.parse((started.params as { event_json: string }).event_json) as {
      value: { request_id: string; generation: string };
    };
    expect(startedJson.value.generation.length).toBeGreaterThan(0);
    expect((done.params as { requestId: string }).requestId).toBe(startedJson.value.request_id);
    expect((done.params as { stopReason: string }).stopReason).toBe('end_turn');

    const texts = await projectTexts(result);
    expect(texts).toContain('hello');
    expect(texts).toContain(`${ECHO_REPLY_PREFIX}hello`);
    expect(texts.filter((text) => text === 'hello')).toHaveLength(1);

    await expect(harness.actions.close(context)).resolves.toEqual({ closed: true });
    expect(recording.methods).toContain('session/close');
    expect(harness.databaseCloses()).toBe(1);
    expect(harness.diagnostics).toEqual([]);
  });

  test('supports multiple turns and closes each session with one session/close', async () => {
    const recording = recordingRuntime();
    const harness = createHarness(recording.runtime);
    const context = actorContext();

    await openAndCreate(harness, context);
    await send(harness, context, 'req_send_1', 'first');
    await waitForFacts(
      harness,
      context,
      (current) => chunkTexts(current).includes(`${ECHO_REPLY_PREFIX}first`),
      'the first echo turn',
    );
    await send(harness, context, 'req_send_2', 'second');
    const result = await waitForFacts(
      harness,
      context,
      (current) =>
        chunkTexts(current).includes(`${ECHO_REPLY_PREFIX}second`) &&
        current.envelopes.filter((envelope) => envelope.fact.kind === 'turn-complete').length === 2,
      'the second echo turn',
    );

    const texts = await projectTexts(result);
    for (const expected of ['first', 'second', `${ECHO_REPLY_PREFIX}first`, `${ECHO_REPLY_PREFIX}second`]) {
      expect(texts).toContain(expected);
    }
    expect(result.envelopes.filter((envelope) => envelope.fact.kind === 'turn-complete')).toHaveLength(2);

    await expect(harness.actions.close(context)).resolves.toEqual({ closed: true });
    expect(recording.methods.filter((method) => method === 'session/close')).toHaveLength(1);
    expect(harness.diagnostics).toEqual([]);
  });

  test('close reports close_incomplete when the runtime rejects session/close', async () => {
    const base = createEchoChatRuntime();
    const runtime: EchoChatRuntime = {
      kind: 'transport',
      factory: async (path, options) => {
        const transport = await base.factory(path, options);
        const rejecting: Transport = {
          request: async <T = unknown>(method: string, params?: unknown): Promise<T> => {
            if (method === 'session/close') {
              throw new Error('session/close is not implemented');
            }
            return transport.request<T>(method, params);
          },
          sendRequest: (method, params) => transport.sendRequest(method, params),
          notify: (method, params) => transport.notify(method, params),
          subscribe: (listener) => transport.subscribe(listener),
          events: () => transport.events(),
          setRequestHandler: (handler) => transport.setRequestHandler(handler),
          close: () => transport.close(),
        };
        return rejecting;
      },
    };
    const harness = createHarness(runtime);
    const context = actorContext();

    await openAndCreate(harness, context);

    let failure: unknown;
    try {
      await harness.actions.close(context);
    } catch (error) {
      failure = error;
    }
    expect(failure).toBeInstanceOf(UserError);
    expect((failure as UserError).code).toBe('close_incomplete');
    expect((failure as UserError).metadata).toMatchObject({ incomplete: true });
  });
});
