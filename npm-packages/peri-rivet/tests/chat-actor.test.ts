import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import {
  type ChatBindingTarget,
  ChatCloseIncompleteError,
  type ChatDatabase,
  type ChatFactEnvelope,
  type ChatFactObserver,
  type ChatService as ChatServiceContract,
  type ChatUserAction,
  type SendReceipt,
} from '@peri-code/sdk';
import { UserError } from 'rivetkit';
import type { ChatActorDiagnostic, ChatOpenTarget, ChatOwnership } from '../src/rivet/chat-ports.js';
import { createHostResourceOwner } from '../src/rivet/host-resource-owner.js';
import { WorkspaceURI } from '../src/rivet/workspace-uri.js';
import { createDeferred } from '../src/util/deferred.js';
import { createChatObservation } from './helpers.js';

const realSdk = await import('@peri-code/sdk');
// Hold the original function reference: the module namespace itself is replaced below.
const realCreateChatSession = realSdk.createChatSession;
const factory: {
  calls: number;
  /** `null` delegates to the real SDK so other test files keep real construction. */
  behavior: ((input: unknown) => Promise<ChatServiceContract>) | null;
} = { calls: 0, behavior: null };

mock.module('@peri-code/sdk', () => ({
  ...realSdk,
  createChatSession: async (input: unknown) => {
    factory.calls += 1;
    return factory.behavior === null
      ? await realCreateChatSession(input as Parameters<typeof realCreateChatSession>[0])
      : await factory.behavior(input);
  },
}));

// Bun module mocks are process-wide; leave the real constructor in place for later files.
afterAll(() => {
  factory.behavior = null;
});

const { createPeriChatActor } = await import('../src/rivet/chat-actor.js');
const { readSessionOwnership } = realSdk;

const CHAT_ID = 'chat_actor_1';
const SESSION_ID = 'session_1';
const MACHINE_ID = '00000000-0000-4000-8000-00000000000a';
const OTHER_MACHINE_ID = '00000000-0000-4000-8000-00000000000b';
const TARGET: ChatBindingTarget = { chatId: 'chat_1', sessionId: SESSION_ID, bindingRevision: 1 };

function workspaceUri(runtimePath = '/workspace', machineId = MACHINE_ID): string {
  return new WorkspaceURI({ machineId, runtimePath }).toString();
}

interface FakeChatOptions {
  readonly binding?: ChatBindingTarget | null;
  readonly replay?: readonly ChatFactEnvelope[];
  readonly failClose?: number;
}

interface FakeChat {
  readonly chat: ChatServiceContract;
  readonly executed: ChatUserAction[];
  readonly closeCount: number;
}

function createReceipt(requestId: string, target: ChatBindingTarget): SendReceipt {
  const accepted = createDeferred<void>();
  accepted.resolve();
  return {
    requestId,
    inputId: `input_${requestId}`,
    target,
    accepted: accepted.promise,
    isSent: true,
    // biome-ignore lint/suspicious/noThenProperty: the SDK receipt is deliberately thenable.
    then: accepted.promise.then.bind(accepted.promise),
  };
}

function createFakeChat(options: FakeChatOptions = {}): FakeChat {
  let binding: ChatBindingTarget | null = options.binding ?? null;
  const replay = options.replay ?? [];
  const executed: ChatUserAction[] = [];
  let closeCount = 0;
  let closeFailures = options.failClose ?? 0;

  const chat = {
    id: 'chat_1',
    get sessionId(): string | null {
      return binding?.sessionId ?? null;
    },
    get bindingRevision(): number {
      return binding?.bindingRevision ?? 0;
    },
    get binding(): ChatBindingTarget | null {
      return binding;
    },
    execute: (action: ChatUserAction): unknown => {
      executed.push(action);
      return createReceipt(action.requestId, binding ?? TARGET);
    },
    readPayload: async () => ({ found: false, ref: {} }),
    close: async () => {
      closeCount += 1;
      if (closeFailures > 0) {
        closeFailures -= 1;
        throw new ChatCloseIncompleteError([new Error('sdk close failed')]);
      }
      binding = null;
    },
    subscribeFacts: (observer: ChatFactObserver) => {
      if (binding !== null) {
        observer.onBinding(binding, replay);
      }
      return () => {};
    },
    observe: () => createChatObservation(),
  };

  return {
    chat: chat as unknown as ChatServiceContract,
    executed,
    get closeCount() {
      return closeCount;
    },
  };
}

interface FakeDatabase {
  readonly database: ChatDatabase & { close(): Promise<void> };
  readonly closeCount: number;
}

function createFakeDatabase(failures = 0): FakeDatabase {
  let closes = 0;
  const database = {
    listSessions: async () => [],
    getSession: async () => null,
    close: async () => {
      closes += 1;
      if (failures > 0) {
        failures -= 1;
        throw new Error('database close failed');
      }
    },
  };
  return {
    database: database as unknown as ChatDatabase & { close(): Promise<void> },
    get closeCount() {
      return closes;
    },
  };
}

function ownershipStub(stored: { readonly sessionId: string; readonly owner: string } | null) {
  const reads: string[] = [];
  const removals: string[] = [];
  const ownership: ChatOwnership = {
    async getItem(key: string) {
      reads.push(key);
      return stored;
    },
    async setItem() {},
    async removeItem(key: string) {
      removals.push(key);
    },
  };
  return { ownership, reads, removals };
}

function envelope(sequence: number): ChatFactEnvelope {
  return {
    chatId: 'chat_1',
    sessionId: SESSION_ID,
    bindingRevision: TARGET.bindingRevision,
    sequence,
    fact: { kind: 'run-state', running: false, executionBlocked: false },
  };
}

function target(overrides: Partial<ChatOpenTarget> = {}): ChatOpenTarget {
  return {
    chatId: CHAT_ID,
    workspace: workspaceUri(),
    agent: { cwd: '/workspace' },
    storage: { url: 'http://store.local/db' },
    ...overrides,
  };
}

interface FakeContext {
  readonly actorId: string;
  readonly key: string[];
  readonly keepAwake: <T>(promise: Promise<T>) => Promise<T>;
  readonly broadcast: (name: string, data: unknown) => void;
  vars: { chat: unknown; database: unknown; open: unknown };
}

function actorContext(key: string[] = [CHAT_ID]): FakeContext {
  return {
    actorId: 'actor_chat',
    key,
    keepAwake: (untilReleased) => untilReleased,
    broadcast: () => {},
    vars: { chat: null, database: null, open: null },
  };
}

type ActionCall = (context: unknown, ...args: unknown[]) => Promise<unknown>;

interface ChatActions {
  readonly open: ActionCall;
  readonly execute: ActionCall;
  readonly readPayload: ActionCall;
  readonly observe: ActionCall;
  readonly activity: ActionCall;
  readonly facts: ActionCall;
  readonly close: ActionCall;
}

function actionNames(definition: { readonly config: { readonly actions?: object } }): string[] {
  return Object.keys(definition.config.actions ?? {}).sort();
}

function actionsOf(definition: { readonly config: { readonly actions?: object } }): ChatActions {
  return definition.config.actions as unknown as ChatActions;
}

function definitionFor(
  overrides: {
    readonly chat?: FakeChat;
    readonly database?: FakeDatabase;
    readonly ownership?: ChatOwnership;
    readonly onDiagnostic?: (diagnostic: ChatActorDiagnostic) => void;
    readonly onLifecycle?: (event: unknown) => void;
  } = {},
) {
  const opened: { storageUrl: string | null; openCount: number } = { storageUrl: null, openCount: 0 };
  const database = overrides.database ?? createFakeDatabase();
  const ownership = overrides.ownership ?? ownershipStub(null).ownership;
  const inputs: unknown[] = [];
  factory.calls = 0;
  factory.behavior = async (input) => {
    inputs.push(input);
    if (overrides.chat === undefined) {
      throw new Error('no fake chat configured');
    }
    return overrides.chat.chat;
  };
  const definition = createPeriChatActor({
    owner: createHostResourceOwner(),
    openDatabase: (storage) => {
      opened.openCount += 1;
      opened.storageUrl = storage.url;
      return database.database;
    },
    ownership,
    machineId: MACHINE_ID,
    chatConfig: {
      runtime: {
        kind: 'transport',
        factory: () => {
          throw new Error('runtime is not used in actor tests');
        },
      },
    },
    ...(overrides.onDiagnostic === undefined ? {} : { onDiagnostic: overrides.onDiagnostic }),
    ...(overrides.onLifecycle === undefined ? {} : { onLifecycle: overrides.onLifecycle }),
  });
  return { definition, database, opened, inputs };
}

beforeEach(() => {
  factory.calls = 0;
});

describe('chat actor action surface', () => {
  test('exposes exactly the chat control actions and the fact event', () => {
    const { definition } = definitionFor({ chat: createFakeChat() });

    expect(actionNames(definition)).toEqual([
      'activity',
      'close',
      'execute',
      'facts',
      'observe',
      'open',
      'readPayload',
    ]);
    expect(definition.config.events?.chatFact).toBeDefined();
    expect(typeof definition.config.onSleep).toBe('function');
    expect(typeof definition.config.onDestroy).toBe('function');
  });

  test('every action except open fails as closed before the actor is opened', async () => {
    const { definition } = definitionFor({ chat: createFakeChat({ binding: TARGET }) });
    const actions = actionsOf(definition);
    const context = actorContext();
    const send = { type: 'send', requestId: 'r', payload: { text: 'x' } };
    // observe/activity/facts are synchronous actions, so each call is wrapped to catch a throw.
    const calls: ReadonlyArray<() => unknown> = [
      () => actions.execute(context, send),
      () => actions.observe(context),
      () => actions.activity(context),
      () => actions.facts(context),
      () => actions.readPayload(context, { ref: {} }),
    ];

    for (const call of calls) {
      const raised = await (async () => call())().catch((error: unknown) => error);
      expect(raised).toBeInstanceOf(UserError);
      expect((raised as UserError).code).toBe('closed');
      expect((raised as UserError).metadata).toMatchObject({ details: { reason: 'not_open' } });
    }
    await expect(actions.close(context)).resolves.toEqual({ closed: true });
  });

  test('rejects an open target whose chatId does not match the actor key', async () => {
    const { definition } = definitionFor({ chat: createFakeChat() });
    const actions = actionsOf(definition);
    const context = actorContext([CHAT_ID]);

    const raised = await actions
      .open(context, target({ chatId: 'chat_other' }))
      .catch((error: unknown) => error);
    expect(raised).toBeInstanceOf(UserError);
    expect((raised as UserError).code).toBe('invalid_identity');
    expect((raised as UserError).metadata).toMatchObject({ details: { reason: 'actor_key_mismatch' } });
    expect(factory.calls).toBe(0);
  });

  test('rejects an actor key that is not exactly one chat id', async () => {
    const { definition } = definitionFor({ chat: createFakeChat() });
    const actions = actionsOf(definition);

    const raised = await actions
      .open(actorContext(['a', 'b']), target({ chatId: 'a' }))
      .catch((error: unknown) => error);
    expect((raised as UserError).code).toBe('invalid_identity');
    expect((raised as UserError).metadata).toMatchObject({ details: { reason: 'actor_key_shape' } });
  });

  test('rejects an actor key that violates the identity rules', async () => {
    const { definition } = definitionFor({ chat: createFakeChat() });
    const actions = actionsOf(definition);

    const raised = await actions
      .open(actorContext(['']), target({ chatId: '' }))
      .catch((error: unknown) => error);
    expect((raised as UserError).code).toBe('invalid_identity');
    expect((raised as UserError).metadata).toMatchObject({ details: { reason: 'actor_key_shape' } });
  });
});

describe('chat actor open', () => {
  test('opens the database from the target storage and builds the session once', async () => {
    const chat = createFakeChat({ binding: TARGET });
    const { definition, database, opened, inputs } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();

    const result = await actions.open(context, target());

    expect(result).toMatchObject({
      chatId: CHAT_ID,
      workspace: workspaceUri(),
      reused: false,
      binding: TARGET,
    });
    expect(opened.storageUrl).toBe('http://store.local/db');
    expect(factory.calls).toBe(1);
    expect(database.closeCount).toBe(0);
    expect(context.vars.chat).toBe(chat.chat);
    // 越界后 SDK 仍只看到内部 WorkspaceRef：id 是 canonical URI，不做哈希或路径拼接。
    expect(inputs[0]).toMatchObject({
      target: { workspace: { id: workspaceUri(), runtimePath: '/workspace' } },
      machineId: MACHINE_ID,
    });
  });

  test('a repeated open with the same identity reuses the handle without rebuilding', async () => {
    const chat = createFakeChat({ binding: TARGET });
    const { definition } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();

    const first = await actions.open(context, target());
    const second = await actions.open(context, target());

    expect(first).toMatchObject({ reused: false });
    expect(second).toMatchObject({ reused: true, binding: TARGET });
    expect(factory.calls).toBe(1);
    expect(chat.closeCount).toBe(0);
  });

  test('equivalent configuration with different key insertion order is reused', async () => {
    const chat = createFakeChat({ binding: TARGET });
    const { definition } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();
    const first = target({
      agent: { cwd: '/workspace', settings: { alpha: 1, beta: 2 } },
      session: { instructions: 'x', mcpServers: { local: { type: 'stdio', command: 'tool', args: ['a'] } } },
      lifecycle: { startupSettleTimeoutMs: 100 },
    });
    const second = target({
      agent: { settings: { beta: 2, alpha: 1 }, cwd: '/workspace' },
      session: { mcpServers: { local: { args: ['a'], command: 'tool', type: 'stdio' } }, instructions: 'x' },
      lifecycle: { startupSettleTimeoutMs: 100 },
    });

    await actions.open(context, first);
    const again = (await actions.open(context, second)) as { readonly reused: boolean };

    expect(again.reused).toBe(true);
    expect(factory.calls).toBe(1);
  });

  test('non-plain session dependencies compare by identity', async () => {
    const chat = createFakeChat({ binding: TARGET });
    const { definition } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();
    const server = { connect: async () => {}, close: async () => {} };

    await actions.open(context, target({ session: { mcpServers: { local: { type: 'acp', server } } } }));
    const same = (await actions.open(
      context,
      target({ session: { mcpServers: { local: { type: 'acp', server } } } }),
    )) as { readonly reused: boolean };
    expect(same.reused).toBe(true);

    const raised = await actions
      .open(
        context,
        target({
          session: {
            mcpServers: {
              local: { type: 'acp', server: { connect: async () => {}, close: async () => {} } },
            },
          },
        }),
      )
      .catch((error: unknown) => error);
    expect(raised).toBeInstanceOf(UserError);
    expect((raised as UserError).code).toBe('binding_conflict');
    expect(factory.calls).toBe(1);
  });

  test.each([
    ['agent provider', { agent: { cwd: '/workspace', settings: { model: 'other' } } }],
    ['session instructions', { session: { instructions: 'other' } }],
    [
      'session MCP servers',
      { session: { mcpServers: { extra: { type: 'http' as const, url: 'http://mcp' } } } },
    ],
    ['storage', { storage: { url: 'http://store.local/other', authToken: 'other-token' } }],
    ['lifecycle', { lifecycle: { startupSettleTimeoutMs: 200 } }],
  ])('a different %s is rejected instead of silently keeping the activation', async (_label, changed) => {
    const chat = createFakeChat({ binding: TARGET });
    const { definition } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();
    const secret = 'project-secret-token';

    await actions.open(
      context,
      target({
        storage: { url: 'http://store.local/db', authToken: secret },
        session: { instructions: 'original' },
      }),
    );
    const raised = await actions
      .open(
        context,
        target({
          storage: { url: 'http://store.local/db', authToken: secret },
          session: { instructions: 'original' },
          ...(changed as Partial<ChatOpenTarget>),
        }),
      )
      .catch((error: unknown) => error);

    expect(raised).toBeInstanceOf(UserError);
    expect((raised as UserError).code).toBe('binding_conflict');
    expect(factory.calls).toBe(1);
    expect(context.vars.chat).toBe(chat.chat);
    const metadata = JSON.stringify((raised as UserError).metadata);
    expect(metadata).not.toContain(secret);
    expect(metadata).not.toContain('original');
  });

  test('two chat actors keep their agent and session inputs separate', async () => {
    const chat = createFakeChat({ binding: TARGET });
    const { definition, inputs } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const agentA = { cwd: '/workspace', settings: { provider: 'anthropic', model: 'sonnet' } };
    const agentB = { cwd: '/workspace', settings: { provider: 'deepseek', model: 'flash' } };
    const sessionA = {
      instructions: 'A',
      mcpServers: { a: { type: 'http' as const, url: 'http://mcp-a' } },
    };
    const sessionB = {
      instructions: 'B',
      mcpServers: { b: { type: 'stdio' as const, command: 'mcp-b' } },
    };

    await actions.open(actorContext([CHAT_ID]), target({ agent: agentA, session: sessionA }));
    await actions.open(
      actorContext(['chat_actor_2']),
      target({
        chatId: 'chat_actor_2',
        agent: agentB,
        session: sessionB,
      }),
    );

    const captured = inputs as Array<{
      target: { sessionId: string; agent: unknown; session: unknown };
      machineId: string;
    }>;
    expect(captured[0]?.target).toMatchObject({ sessionId: CHAT_ID, agent: agentA, session: sessionA });
    expect(captured[1]?.target).toMatchObject({
      sessionId: 'chat_actor_2',
      agent: agentB,
      session: sessionB,
    });
    expect(captured.map((input) => input.machineId)).toEqual([MACHINE_ID, MACHINE_ID]);
    expect(captured[0]?.target.agent).not.toEqual(captured[1]?.target.agent);
    expect(captured[0]?.target.session).not.toEqual(captured[1]?.target.session);
  });

  test('concurrent open calls for the same target construct one activation', async () => {
    const chat = createFakeChat({ binding: TARGET });
    const { definition, database, opened } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();

    const results = await Promise.all([actions.open(context, target()), actions.open(context, target())]);
    expect(results[0]).toMatchObject({ reused: false });
    expect(results[1]).toMatchObject({ reused: true });
    expect(opened.openCount).toBe(1);
    expect(factory.calls).toBe(1);

    await actions.close(context);
    expect(database.closeCount).toBe(1);
  });

  test('concurrent open calls with different targets construct once and conflict', async () => {
    const chat = createFakeChat({ binding: TARGET });
    const { definition, opened } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();

    const results = await Promise.allSettled([
      actions.open(context, target()),
      actions.open(context, target({ workspace: workspaceUri('/other') })),
    ]);
    expect(results[0]?.status).toBe('fulfilled');
    expect(results[1]?.status).toBe('rejected');
    expect((results[1] as PromiseRejectedResult).reason).toMatchObject({ code: 'binding_conflict' });
    expect(opened.openCount).toBe(1);
    await actions.close(context);
  });

  test('a failed concurrent initialization leaves no database behind and a retry succeeds', async () => {
    const chat = createFakeChat({ binding: TARGET });
    const { definition, database, opened } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();
    const inner = factory.behavior;
    if (inner === null) throw new Error('expected a configured factory');
    let fail = true;
    factory.behavior = async (input) => {
      if (fail) {
        fail = false;
        throw new Error('init failed once');
      }
      return await inner(input);
    };

    const results = await Promise.allSettled([
      actions.open(context, target()),
      actions.open(context, target()),
    ]);
    expect(results[0]?.status).toBe('rejected');
    expect(results[1]?.status).toBe('fulfilled');
    expect(opened.openCount).toBe(2);
    expect(database.closeCount).toBe(1);
    expect(context.vars.chat).toBe(chat.chat);

    await actions.close(context);
    expect(database.closeCount).toBe(2);
  });

  test('a different workspace on the same actor is a binding conflict', async () => {
    const chat = createFakeChat({ binding: TARGET });
    const { definition } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();

    await actions.open(context, target());
    const raised = await actions
      .open(context, target({ workspace: workspaceUri('/other') }))
      .catch((error: unknown) => error);

    expect(raised).toBeInstanceOf(UserError);
    expect((raised as UserError).code).toBe('binding_conflict');
    expect(factory.calls).toBe(1);
    expect(context.vars.chat).toBe(chat.chat);
  });

  test('a foreign machine is rejected before ownership, database and runtime side effects', async () => {
    const stub = ownershipStub(null);
    const { definition, opened } = definitionFor({
      chat: createFakeChat({ binding: TARGET }),
      ownership: stub.ownership,
    });
    const actions = actionsOf(definition);
    const context = actorContext();

    const raised = await actions
      .open(context, target({ workspace: workspaceUri('/workspace', OTHER_MACHINE_ID) }))
      .catch((error: unknown) => error);

    expect(raised).toBeInstanceOf(UserError);
    expect((raised as UserError).code).toBe('invalid_identity');
    expect((raised as UserError).metadata).toMatchObject({ details: { reason: 'machine_mismatch' } });
    expect(stub.reads).toHaveLength(0);
    expect(opened.storageUrl).toBeNull();
    expect(factory.calls).toBe(0);
    expect(context.vars).toEqual({ chat: null, database: null, open: null });
  });

  test.each([
    ['a raw path', '/workspace'],
    ['a non-canonical scheme', `PERI+SANDBOX://${MACHINE_ID}/workspace`],
    ['an uppercase machine id', `peri+sandbox://${MACHINE_ID.toUpperCase()}/workspace`],
  ])('rejects %s without side effects', async (_label, workspace) => {
    const stub = ownershipStub(null);
    const { definition, opened } = definitionFor({
      chat: createFakeChat({ binding: TARGET }),
      ownership: stub.ownership,
    });
    const actions = actionsOf(definition);
    const context = actorContext();

    const raised = await actions.open(context, target({ workspace })).catch((error: unknown) => error);

    expect(raised).toBeInstanceOf(UserError);
    expect((raised as UserError).code).toBe('invalid_identity');
    expect(stub.reads).toHaveLength(0);
    expect(opened.storageUrl).toBeNull();
    expect(factory.calls).toBe(0);
    expect(context.vars).toEqual({ chat: null, database: null, open: null });
  });

  test('a failed construction closes the database and leaves no vars behind', async () => {
    const database = createFakeDatabase();
    const { definition } = definitionFor({ database });
    const actions = actionsOf(definition);
    const context = actorContext();

    await expect(actions.open(context, target())).rejects.toThrow('no fake chat configured');

    expect(database.closeCount).toBe(1);
    expect(context.vars).toEqual({ chat: null, database: null, open: null });
  });

  test('clears the previous activation ownership record and reads then removes one key', async () => {
    const stub = ownershipStub({ sessionId: CHAT_ID, owner: 'owner_previous' });
    const { definition } = definitionFor({
      chat: createFakeChat({ binding: TARGET }),
      ownership: stub.ownership,
    });

    await actionsOf(definition).open(actorContext(), target());

    expect(stub.reads).toHaveLength(1);
    expect(stub.removals).toEqual(stub.reads);
    expect(await readSessionOwnership(stub.ownership, CHAT_ID)).not.toBeNull();
  });

  test('an absent ownership record is not reported as a clear', async () => {
    const stub = ownershipStub(null);
    const { definition } = definitionFor({
      chat: createFakeChat({ binding: TARGET }),
      ownership: stub.ownership,
    });

    await actionsOf(definition).open(actorContext(), target());

    expect(stub.reads).toHaveLength(1);
    expect(stub.removals).toHaveLength(0);
  });

  test('a failing ownership reset aborts open before the database is touched', async () => {
    const diagnostics: ChatActorDiagnostic[] = [];
    const database = createFakeDatabase();
    const stub = ownershipStub(null);
    const ownership: ChatOwnership = {
      ...stub.ownership,
      getItem: async () => {
        throw new Error('store unavailable');
      },
    };
    const { definition, opened } = definitionFor({
      chat: createFakeChat({ binding: TARGET }),
      database,
      ownership,
      onDiagnostic: (diagnostic) => diagnostics.push(diagnostic),
    });

    await expect(actionsOf(definition).open(actorContext(), target())).rejects.toThrow('store unavailable');

    expect(opened.storageUrl).toBeNull();
    expect(factory.calls).toBe(0);
    expect(diagnostics.map((diagnostic) => diagnostic.type)).toEqual(['ownership_reset_failed']);
  });
});

describe('chat actor actions', () => {
  test('execute returns the send receipt fields and rejects a stale target', async () => {
    const chat = createFakeChat({ binding: TARGET });
    const { definition } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();
    await actions.open(context, target());

    const sent = await actions.execute(context, {
      type: 'send',
      requestId: 'req_1',
      payload: { text: 'hi' },
    });
    expect(sent).toMatchObject({ type: 'send', requestId: 'req_1', inputId: 'input_req_1' });

    await expect(
      actions.execute(
        context,
        { type: 'send', requestId: 'req_2', payload: { text: 'hi' } },
        {
          sessionId: SESSION_ID,
          bindingRevision: 9,
        },
      ),
    ).rejects.toMatchObject({ code: 'binding_conflict' });
    expect(chat.executed).toHaveLength(1);
  });

  test('facts replays the whole generation without a cursor', async () => {
    const replay = [envelope(1), envelope(2), envelope(3)];
    const chat = createFakeChat({ binding: TARGET, replay });
    const { definition } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();
    await actions.open(context, target());

    const first = await actions.facts(context);
    const second = await actions.facts(context);

    expect(first).toEqual({ binding: TARGET, envelopes: replay, overflow: false });
    expect(second).toEqual(first);
    expect(actionNames(definition)).not.toContain('cursor');
  });

  test('observe and activity read the SDK handle without mutating it', async () => {
    const chat = createFakeChat({ binding: TARGET });
    const { definition } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();
    await actions.open(context, target());

    expect(await actions.observe(context)).toMatchObject({ chatId: 'chat_1', phase: 'bound' });
    expect(await actions.activity(context)).toMatchObject({ closing: false });
  });
});

describe('chat actor close and sleep', () => {
  test('close is idempotent and closes the chat handle once', async () => {
    const chat = createFakeChat({ binding: TARGET });
    const { definition, database } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();
    await actions.open(context, target());

    await expect(actions.close(context)).resolves.toEqual({ closed: true });
    await expect(actions.close(context)).resolves.toEqual({ closed: true });

    expect(chat.closeCount).toBe(1);
    expect(database.closeCount).toBe(1);
    expect(context.vars.chat).toBeNull();
  });

  test('sleep reports close_incomplete, keeps the retryable owner and does not close the database', async () => {
    const diagnostics: ChatActorDiagnostic[] = [];
    const chat = createFakeChat({ binding: TARGET, failClose: 1 });
    const { definition, database } = definitionFor({
      chat,
      onDiagnostic: (diagnostic) => diagnostics.push(diagnostic),
    });
    const actions = actionsOf(definition);
    const context = actorContext();
    await actions.open(context, target());

    const raised = await (definition.config.onSleep as (c: unknown) => Promise<void>)(context).catch(
      (error: unknown) => error,
    );
    expect(raised).toBeInstanceOf(UserError);
    expect((raised as UserError).code).toBe('close_incomplete');
    expect(chat.closeCount).toBe(1);
    expect(database.closeCount).toBe(0);
    expect(context.vars.chat).toBe(chat.chat);
    expect(context.vars.database).not.toBeNull();
    expect(diagnostics.map((diagnostic) => diagnostic.type)).toEqual(['close_failed']);
    expect(diagnostics[0]).toMatchObject({ scope: 'chat' });

    // The failed hook keeps the progress and the owner; retrying the same hook closes for real.
    await (definition.config.onSleep as (c: unknown) => Promise<void>)(context);
    expect(chat.closeCount).toBe(2);
    expect(database.closeCount).toBe(1);
    expect(context.vars).toEqual({ chat: null, database: null, open: null });
  });

  test('destroy also refuses to clear an activation whose chat close is unconfirmed', async () => {
    const chat = createFakeChat({ binding: TARGET, failClose: 1 });
    const { definition, database } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();
    await actions.open(context, target());

    const raised = await (definition.config.onDestroy as (c: unknown) => Promise<void>)(context).catch(
      (error: unknown) => error,
    );
    expect(raised).toBeInstanceOf(UserError);
    expect((raised as UserError).code).toBe('close_incomplete');
    expect(chat.closeCount).toBe(1);
    expect(database.closeCount).toBe(0);
    expect(context.vars.chat).toBe(chat.chat);

    await (definition.config.onDestroy as (c: unknown) => Promise<void>)(context);
    expect(chat.closeCount).toBe(2);
    expect(database.closeCount).toBe(1);
    expect(context.vars).toEqual({ chat: null, database: null, open: null });
  });

  test('sleep keeps chat-closed progress and retries only the database after a db failure', async () => {
    const chat = createFakeChat({ binding: TARGET });
    const database = createFakeDatabase(1);
    const { definition } = definitionFor({ chat, database });
    const actions = actionsOf(definition);
    const context = actorContext();
    await actions.open(context, target());

    await expect((definition.config.onSleep as (c: unknown) => Promise<void>)(context)).rejects.toThrow(
      'database close failed',
    );
    expect(chat.closeCount).toBe(1);
    expect(database.closeCount).toBe(1);
    expect(context.vars.chat).toBe(chat.chat);
    expect(context.vars.database).not.toBeNull();

    await (definition.config.onSleep as (c: unknown) => Promise<void>)(context);
    expect(chat.closeCount).toBe(1);
    expect(database.closeCount).toBe(2);
    expect(context.vars).toEqual({ chat: null, database: null, open: null });
  });

  test('a failed sleep and a queued explicit close serialize without deadlocking', async () => {
    const chat = createFakeChat({ binding: TARGET, failClose: 1 });
    const { definition, database } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();
    await actions.open(context, target());

    const sleeping = (definition.config.onSleep as (c: unknown) => Promise<void>)(context).catch(
      (error: unknown) => error,
    );
    const closing = actions.close(context);
    const [sleepFailure, closed] = await Promise.all([sleeping, closing]);

    expect(sleepFailure).toBeInstanceOf(UserError);
    expect((sleepFailure as UserError).code).toBe('close_incomplete');
    expect(closed).toEqual({ closed: true });
    expect(chat.closeCount).toBe(2);
    expect(database.closeCount).toBe(1);
    expect(context.vars).toEqual({ chat: null, database: null, open: null });
    // The queue is still usable after a rejected hook task.
    await expect(actions.close(context)).resolves.toEqual({ closed: true });
  });

  test('a failed close retains the handle and a retry closes for real', async () => {
    const chat = createFakeChat({ binding: TARGET, failClose: 1 });
    const { definition, database } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();
    await actions.open(context, target());

    const raised = await actions.close(context).catch((error: unknown) => error);
    expect(raised).toBeInstanceOf(UserError);
    expect((raised as UserError).code).toBe('close_incomplete');
    expect(context.vars.chat).toBe(chat.chat);
    expect(context.vars.database).not.toBeNull();
    expect(context.vars.open).not.toBeNull();
    expect(chat.closeCount).toBe(1);
    expect(database.closeCount).toBe(0);

    await expect(actions.close(context)).resolves.toEqual({ closed: true });
    expect(chat.closeCount).toBe(2);
    expect(database.closeCount).toBe(1);
    expect(context.vars.chat).toBeNull();
  });

  test('a persistently failing close never reports closed', async () => {
    const chat = createFakeChat({ binding: TARGET, failClose: 10 });
    const { definition, database } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();
    await actions.open(context, target());

    for (let attempt = 0; attempt < 2; attempt += 1) {
      const raised = await actions.close(context).catch((error: unknown) => error);
      expect(raised).toBeInstanceOf(UserError);
      expect((raised as UserError).code).toBe('close_incomplete');
      expect(context.vars.chat).toBe(chat.chat);
    }
    expect(chat.closeCount).toBe(2);
    expect(database.closeCount).toBe(0);
  });

  test('a database close failure keeps the chat-closed progress and retries only the database', async () => {
    const chat = createFakeChat({ binding: TARGET });
    const database = createFakeDatabase(1);
    const { definition } = definitionFor({ chat, database });
    const actions = actionsOf(definition);
    const context = actorContext();
    await actions.open(context, target());

    await expect(actions.close(context)).rejects.toThrow('database close failed');
    expect(chat.closeCount).toBe(1);
    expect(database.closeCount).toBe(1);
    expect(context.vars.chat).toBe(chat.chat);

    await expect(actions.close(context)).resolves.toEqual({ closed: true });
    expect(chat.closeCount).toBe(1);
    expect(database.closeCount).toBe(2);
    expect(context.vars.chat).toBeNull();
  });

  test('a failed close is not reported as a reusable open and is retried first', async () => {
    const chat = createFakeChat({ binding: TARGET, failClose: 1 });
    const { definition } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();
    await actions.open(context, target());

    await actions.close(context).catch(() => undefined);
    await expect(actions.open(context, target())).resolves.toMatchObject({ reused: false });
    expect(chat.closeCount).toBe(2);
    expect(factory.calls).toBe(2);
    await actions.close(context);
  });

  test('concurrent close calls share one close and never double-close', async () => {
    const chat = createFakeChat({ binding: TARGET });
    const { definition, database } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();
    await actions.open(context, target());

    const results = await Promise.all([actions.close(context), actions.close(context)]);
    expect(results).toEqual([{ closed: true }, { closed: true }]);
    expect(chat.closeCount).toBe(1);
    expect(database.closeCount).toBe(1);
    expect(context.vars.chat).toBeNull();
  });

  test('destroy reports the lifecycle event after teardown', async () => {
    const events: unknown[] = [];
    const chat = createFakeChat({ binding: TARGET });
    const { definition, database } = definitionFor({ chat, onLifecycle: (event) => events.push(event) });
    const actions = actionsOf(definition);
    const context = actorContext();
    await actions.open(context, target());

    await (definition.config.onDestroy as (c: unknown) => Promise<void>)(context);

    expect(database.closeCount).toBe(1);
    expect(events).toEqual([
      { type: 'wake', actorId: 'actor_chat', key: [CHAT_ID] },
      { type: 'destroy', actorId: 'actor_chat', key: [CHAT_ID] },
    ]);
  });
});
