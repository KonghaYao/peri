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
const TARGET: ChatBindingTarget = { chatId: 'chat_1', sessionId: SESSION_ID, bindingRevision: 1 };

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

function createFakeDatabase(failClose = false): FakeDatabase {
  let closes = 0;
  const database = {
    listSessions: async () => [],
    getSession: async () => null,
    close: async () => {
      closes += 1;
      if (failClose) {
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
    workspace: { id: 'ws_1', runtimePath: '/workspace' },
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
  const opened: { storageUrl: string | null } = { storageUrl: null };
  const database = overrides.database ?? createFakeDatabase();
  const ownership = overrides.ownership ?? ownershipStub(null).ownership;
  factory.calls = 0;
  factory.behavior = async () => {
    if (overrides.chat === undefined) {
      throw new Error('no fake chat configured');
    }
    return overrides.chat.chat;
  };
  const definition = createPeriChatActor({
    openDatabase: (storage) => {
      opened.storageUrl = storage.url;
      return database.database;
    },
    ownership,
    machineId: 'machine_1',
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
  return { definition, database, opened };
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
    const { definition, database, opened } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();

    const result = await actions.open(context, target());

    expect(result).toMatchObject({ chatId: CHAT_ID, reused: false, binding: TARGET });
    expect(opened.storageUrl).toBe('http://store.local/db');
    expect(factory.calls).toBe(1);
    expect(database.closeCount).toBe(0);
    expect(context.vars.chat).toBe(chat.chat);
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

  test('a different workspace on the same actor is a binding conflict', async () => {
    const chat = createFakeChat({ binding: TARGET });
    const { definition } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();

    await actions.open(context, target());
    const raised = await actions
      .open(context, target({ workspace: { id: 'ws_2', runtimePath: '/other' } }))
      .catch((error: unknown) => error);

    expect(raised).toBeInstanceOf(UserError);
    expect((raised as UserError).code).toBe('binding_conflict');
    expect(factory.calls).toBe(1);
    expect(context.vars.chat).toBe(chat.chat);
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

  test('sleep closes the database even when the chat close fails', async () => {
    const diagnostics: ChatActorDiagnostic[] = [];
    const chat = createFakeChat({ binding: TARGET, failClose: 1 });
    const { definition, database } = definitionFor({
      chat,
      onDiagnostic: (diagnostic) => diagnostics.push(diagnostic),
    });
    const actions = actionsOf(definition);
    const context = actorContext();
    await actions.open(context, target());

    await (definition.config.onSleep as (c: unknown) => Promise<void>)(context);

    expect(chat.closeCount).toBe(1);
    expect(database.closeCount).toBe(1);
    expect(context.vars).toEqual({ chat: null, database: null, open: null });
    expect(diagnostics.map((diagnostic) => diagnostic.type)).toEqual(['close_failed']);
    expect(diagnostics[0]).toMatchObject({ scope: 'chat' });
  });

  test('a failed close surfaces close_incomplete and can be retried', async () => {
    const chat = createFakeChat({ binding: TARGET, failClose: 1 });
    const { definition } = definitionFor({ chat });
    const actions = actionsOf(definition);
    const context = actorContext();
    await actions.open(context, target());

    const raised = await actions.close(context).catch((error: unknown) => error);
    expect(raised).toBeInstanceOf(UserError);
    expect((raised as UserError).code).toBe('close_incomplete');
    expect(context.vars.chat).toBeNull();

    // The handle was released before the failure surfaced, so the next open rebuilds cleanly.
    await expect(actions.open(context, target())).resolves.toMatchObject({ reused: false });
    expect(factory.calls).toBe(2);
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
