import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import type { AgentDraft } from '@shared/agent';
import { AgentDraft as AgentDraftSchema } from '@shared/agent';
import { AgentsRepository } from '../../../server/agents/repository';
import { NotConfiguredChatEngine, probeChatRuntime } from '../../../server/chats/engine';
import { ChatsRepository } from '../../../server/chats/repository';
import { ChatService } from '../../../server/chats/service';
import { openDatabase } from '../../../server/db/database';
import { migrate } from '../../../server/db/migrate';
import { testEnv, withStoredProvider } from '../helpers';
import { BINDING, FakeEngine, transportFailure, userInput } from './helpers';

let db: Database | null = null;

afterEach(() => {
  db?.close();
  db = null;
});

const providerDraft: AgentDraft = AgentDraftSchema.parse({
  name: 'Reviewer',
  description: '',
  config: {
    instructions: 'Be concise',
    provider: { id: 'anthropic-main', type: 'anthropic', model: 'claude-sonnet-4-5' },
    metaHarness: {},
    mcpServers: {},
  },
});

function setup(options: { engine?: FakeEngine; draft?: AgentDraft } = {}) {
  const database = openDatabase(':memory:');
  migrate(database);
  db = database;
  const agents = new AgentsRepository(database);
  const chats = new ChatsRepository(database);
  const agent = agents.create(withStoredProvider(database, options.draft ?? providerDraft));
  const engine = options.engine ?? new FakeEngine();
  let counter = 0;
  const service = new ChatService({
    env: testEnv(),
    engine,
    chats,
    agents,
    generateId: () => `id-${++counter}`,
  });
  return { service, engine, agent, chats };
}

describe('ChatService', () => {
  test('creates a chat row, opens the actor and lists it with the agent name', async () => {
    const { service, engine, agent } = setup();
    const chat = await service.create({ agentId: agent.id });

    expect(chat.agentId).toBe(agent.id);
    expect(chat.agentName).toBe('Reviewer');
    expect(chat.sessionId).toBeNull();
    expect(service.list()).toEqual([chat]);

    const actor = engine.actors.get(chat.chatId);
    expect(actor?.opened).toHaveLength(1);
    expect(actor?.opened[0]?.chatId).toBe(chat.chatId);
  });

  test('creates a session once and stores its id before sending', async () => {
    const { service, engine, agent, chats } = setup();
    const chat = await service.create({ agentId: agent.id });

    const response = await service.send(chat.chatId, 'request-1', 'hello');
    const actor = engine.actors.get(chat.chatId);
    expect(actor?.actions.map((action) => action.type)).toEqual(['create_new_session', 'send']);
    expect(chats.get(chat.chatId)?.sessionId).toBe('session-1');
    expect(response).toMatchObject({ requestId: 'request-1', sessionId: 'session-1' });
  });

  test('maps a refused session to the 502 session_creation_failed contract', async () => {
    const { service, engine, agent } = setup();
    const chat = await service.create({ agentId: agent.id });
    const actor = engine.actors.get(chat.chatId);
    if (!actor) throw new Error('actor missing');
    actor.failCreateSessionWith = Object.assign(new Error('Store refused the new session'), {
      code: 'session_creation_failed',
    });

    await expect(service.send(chat.chatId, 'request-1', 'hello')).rejects.toMatchObject({
      code: 'session_creation_failed',
      status: 502,
      retryable: true,
    });
  });

  test('returns the cached receipt for the same requestId and rejects reuse with other text', async () => {
    const { service, engine, agent } = setup();
    const chat = await service.create({ agentId: agent.id });
    const first = await service.send(chat.chatId, 'request-1', 'hello');
    const second = await service.send(chat.chatId, 'request-1', 'hello');

    expect(second).toEqual(first);
    expect(engine.actors.get(chat.chatId)?.actions.filter((a) => a.type === 'send')).toHaveLength(1);

    await expect(service.send(chat.chatId, 'request-1', 'different')).rejects.toMatchObject({
      code: 'request_conflict',
      status: 409,
    });
  });

  test('changes to the stored session after a restart and falls back to a new session', async () => {
    const { service, engine, agent } = setup();
    const chat = await service.create({ agentId: agent.id });
    await service.send(chat.chatId, 'request-1', 'hello');
    const actor = engine.actors.get(chat.chatId);
    if (!actor) throw new Error('actor missing');
    actor.actions.length = 0;
    actor.bound = false;

    await service.send(chat.chatId, 'request-2', 'again');
    expect(actor.actions.map((action) => action.type)).toEqual(['change_session', 'send']);

    actor.actions.length = 0;
    actor.bound = false;
    actor.failChangeSession = true;
    await service.send(chat.chatId, 'request-3', 'after prune');
    expect(actor.actions.map((action) => action.type)).toEqual([
      'change_session',
      'create_new_session',
      'send',
    ]);
  });

  test('reopens once after a binding conflict', async () => {
    const { service, engine, agent } = setup();
    const chat = await service.create({ agentId: agent.id });
    const actor = engine.actors.get(chat.chatId);
    if (!actor) throw new Error('actor missing');
    actor.failOpenWith = Object.assign(new Error('config changed'), {
      code: 'binding_conflict',
    });

    await service.send(chat.chatId, 'request-1', 'hello');
    expect(actor.closed).toBe(true);
    expect(actor.actions.map((action) => action.type)).toEqual(['create_new_session', 'send']);
  });

  test('reports close_incomplete without deleting the chat', async () => {
    const { service, engine, agent } = setup();
    const chat = await service.create({ agentId: agent.id });
    const actor = engine.actors.get(chat.chatId);
    if (!actor) throw new Error('actor missing');
    actor.failCloseWith = new Error('close timed out');

    const result = await service.close(chat.chatId);
    expect(result.status).toBe('close_incomplete');
    expect(result.errors).toHaveLength(1);
    expect(service.list()).toHaveLength(1);
  });

  test('closes and removes the host mapping', async () => {
    const { service, engine, agent } = setup();
    const chat = await service.create({ agentId: agent.id });

    expect(await service.close(chat.chatId)).toEqual({ status: 'closed', errors: [] });
    expect(engine.actors.get(chat.chatId)?.closed).toBe(true);

    await service.remove(chat.chatId);
    expect(service.list()).toEqual([]);
  });

  test('reopens a normally closed chat as a new run from the stored session', async () => {
    const { service, engine, agent } = setup();
    const chat = await service.create({ agentId: agent.id });
    await service.send(chat.chatId, 'request-1', '核验');
    const before = await service.prepareStream(chat.chatId);

    expect(await service.close(chat.chatId)).toEqual({ status: 'closed', errors: [] });
    expect(before.terminal).toBe(true);

    const actor = engine.actors.get(chat.chatId);
    if (!actor) throw new Error('actor missing');
    actor.actions.length = 0;
    // A confirmed close drops the activation; the next open starts from Store history.
    const binding = { ...BINDING, bindingRevision: 2 };
    actor.factsResult = {
      binding,
      envelopes: [{ ...userInput('history-1', '核验成功', 1), ...binding }],
      overflow: false,
    };

    const reopened = await service.prepareStream(chat.chatId);
    expect(reopened).not.toBe(before);
    expect(actor.actions.map((action) => action.type)).toEqual(['change_session']);
    expect(actor.actions[0]).toMatchObject({ payload: { sessionId: 'session-1' } });

    const events: unknown[] = [];
    const unsubscribe = await reopened.subscribe(undefined, (event) => events.push(event));
    unsubscribe();
    expect(events[0]).toMatchObject({
      event: 'snapshot',
      data: { sessionId: 'session-1', messages: [expect.objectContaining({ text: '核验成功' })] },
    });
    await reopened.dispose();
  });

  test('shares one new activation when a closed chat is entered concurrently', async () => {
    const { service, engine, agent } = setup();
    const chat = await service.create({ agentId: agent.id });
    await service.send(chat.chatId, 'request-1', 'hello');
    expect((await service.close(chat.chatId)).status).toBe('closed');

    const actor = engine.actors.get(chat.chatId);
    if (!actor) throw new Error('actor missing');
    actor.actions.length = 0;
    const [first, second] = await Promise.all([
      service.prepareStream(chat.chatId),
      service.prepareStream(chat.chatId),
    ]);
    expect(second).toBe(first);
    expect(actor.actions.filter((action) => action.type === 'change_session')).toHaveLength(1);
    await first.dispose();
  });

  test('keeps normal entry blocked until an incomplete close is confirmed', async () => {
    const { service, engine, agent } = setup();
    const chat = await service.create({ agentId: agent.id });
    const actor = engine.actors.get(chat.chatId);
    if (!actor) throw new Error('actor missing');
    actor.failCloseWith = new Error('close timed out');

    expect((await service.close(chat.chatId)).status).toBe('close_incomplete');
    expect(() => service.prepareStream(chat.chatId)).toThrow('Chat is closing or closed');

    actor.failCloseWith = null;
    expect((await service.close(chat.chatId)).status).toBe('closed');
    const reopened = await service.prepareStream(chat.chatId);
    await reopened.dispose();
  });

  test('does not resurrect a deleted chat through the normal entry', async () => {
    const { service, agent } = setup();
    const chat = await service.create({ agentId: agent.id });
    expect((await service.close(chat.chatId)).status).toBe('closed');
    await service.remove(chat.chatId);

    await expect(service.prepareStream(chat.chatId)).rejects.toMatchObject({ code: 'not_found' });
    expect(service.list()).toEqual([]);
  });

  test('refuses chats when the runtime is not configured, naming the reasons', async () => {
    const env = testEnv();
    const engine = new NotConfiguredChatEngine(probeChatRuntime(env));
    const database = openDatabase(':memory:');
    migrate(database);
    db = database;
    const agents = new AgentsRepository(database);
    const agent = agents.create(withStoredProvider(database, providerDraft));
    const service = new ChatService({
      env,
      engine,
      chats: new ChatsRepository(database),
      agents,
    });

    await expect(service.create({ agentId: agent.id })).rejects.toMatchObject({
      code: 'engine_unavailable',
      details: { reasons: expect.arrayContaining(['store_token_missing']) },
    });
  });

  test('does not silently replace history when loading the stored session fails for a real error', async () => {
    const { service, engine, agent, chats } = setup();
    const chat = await service.create({ agentId: agent.id });
    const actor = engine.actors.get(chat.chatId);
    if (!actor) throw new Error('actor missing');
    chats.setSessionId(chat.chatId, 'stored-session');
    actor.failChangeSessionWith = Object.assign(new Error('engine failed'), {
      code: 'internal_error',
    });

    await expect(service.send(chat.chatId, 'request-1', 'hello')).rejects.toMatchObject({
      code: 'internal_error',
      cause: expect.objectContaining({ message: 'engine failed' }),
    });
    expect(actor.actions.map((action) => action.type)).not.toContain('create_new_session');
  });

  test('normalizes untyped transport failures to engine_unavailable while the engine is not ready', async () => {
    const { service, engine, agent } = setup();
    const chat = await service.create({ agentId: agent.id });
    const actor = engine.actors.get(chat.chatId);
    if (!actor) throw new Error('actor missing');
    actor.failOpenAlwaysWith = transportFailure();
    engine.healthStatus = 'unavailable';

    for (const call of [
      () => service.send(chat.chatId, 'request-1', 'hello'),
      () => service.cancel(chat.chatId),
      () => service.prepareStream(chat.chatId),
    ]) {
      await expect(call()).rejects.toMatchObject({
        code: 'engine_unavailable',
        status: 503,
        details: { reasons: [] },
        retryable: false,
      });
    }

    // The same transport error on an engine that reports ready stays a real 500.
    engine.healthStatus = 'ready';
    await expect(service.send(chat.chatId, 'request-2', 'hello')).rejects.toMatchObject({
      code: 'internal_error',
      status: 500,
    });
  });

  test('prepareStream maps a failed session creation like send', async () => {
    const { service, engine, agent } = setup();
    const chat = await service.create({ agentId: agent.id });
    const actor = engine.actors.get(chat.chatId);
    if (!actor) throw new Error('actor missing');
    actor.failCreateSessionWith = transportFailure();
    engine.healthStatus = 'unavailable';

    await expect(service.prepareStream(chat.chatId)).rejects.toMatchObject({
      code: 'engine_unavailable',
      status: 503,
    });

    actor.failCreateSessionWith = Object.assign(new Error('bad session'), { code: 'internal_error' });
    engine.healthStatus = 'ready';
    await expect(service.prepareStream(chat.chatId)).rejects.toMatchObject({
      code: 'internal_error',
      status: 500,
    });
  });

  test('a raw engine start failure is a typed 503 before any chat row exists', async () => {
    const engine = new FakeEngine();
    engine.start = async () => {
      engine.started += 1;
      throw transportFailure();
    };
    engine.healthStatus = 'unavailable';
    const { service, agent } = setup({ engine });

    await expect(service.create({ agentId: agent.id })).rejects.toMatchObject({
      code: 'engine_unavailable',
      status: 503,
      details: { reasons: [] },
      retryable: false,
    });
    expect(service.list()).toEqual([]);
  });

  test('a create that never obtained an actor handle removes its chat row', async () => {
    const engine = new FakeEngine();
    engine.actorsAvailable = false;
    engine.healthStatus = 'starting';
    const { service, agent } = setup({ engine });

    await expect(service.create({ agentId: agent.id })).rejects.toMatchObject({
      code: 'engine_unavailable',
      status: 503,
    });
    expect(service.list()).toEqual([]);
  });

  test('falls back to a new session only when the stored session is typed not_found', async () => {
    const { service, engine, agent, chats } = setup();
    const chat = await service.create({ agentId: agent.id });
    const actor = engine.actors.get(chat.chatId);
    if (!actor) throw new Error('actor missing');
    chats.setSessionId(chat.chatId, 'pruned-session');
    actor.failChangeSession = true;

    await service.send(chat.chatId, 'request-1', 'hello');
    expect(actor.actions.map((action) => action.type)).toEqual([
      'change_session',
      'create_new_session',
      'send',
    ]);
  });

  test('refuses an agent without a provider or without the matching key', async () => {
    const { service, agent } = setup({
      draft: AgentDraftSchema.parse({
        name: 'No provider',
        config: { instructions: '', provider: null, metaHarness: {}, mcpServers: {} },
      }),
    });
    await expect(service.create({ agentId: agent.id })).rejects.toMatchObject({
      code: 'not_ready',
      details: { reason: 'provider_missing' },
    });

    const database = db;
    if (!database) throw new Error('database missing');
    const agents = new AgentsRepository(database);
    const noKey = agents.create(withStoredProvider(database, providerDraft, null));
    const engine = new FakeEngine();
    const serviceWithoutKey = new ChatService({
      env: testEnv(),
      engine,
      chats: new ChatsRepository(database),
      agents,
    });
    await expect(serviceWithoutKey.create({ agentId: noKey.id })).rejects.toMatchObject({
      code: 'not_ready',
      details: { reason: 'api_key_missing' },
    });
  });
});
