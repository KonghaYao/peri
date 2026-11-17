import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import { type Agent, AgentCapabilities, AgentDraft, type AgentDraft as AgentDraftType } from '@shared/agent';
import { ApiErrorBody } from '@shared/api';
import { ChatCloseResponse, ChatListResponse, ChatResponse, ChatSendResponse } from '@shared/chat';
import type { Hono } from 'hono';
import { AgentsRepository } from '../../../server/agents/repository';
import { createApp } from '../../../server/app';
import {
  type ChatEngine,
  createChatEngine,
  createChatEngineWithStoreProbe,
} from '../../../server/chats/engine';
import { ChatsRepository } from '../../../server/chats/repository';
import { ChatService } from '../../../server/chats/service';
import { openDatabase } from '../../../server/db/database';
import { migrate } from '../../../server/db/migrate';
import { testEnv, withStoredProvider } from '../helpers';
import { agentChunk, BINDING, FakeEngine, userInput } from './helpers';

let db: Database | null = null;

const tick = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));

afterEach(() => {
  db?.close();
  db = null;
});

const draft: AgentDraftType = AgentDraft.parse({
  name: 'Reviewer',
  description: '',
  config: {
    instructions: 'Be concise',
    provider: { id: 'anthropic-main', type: 'anthropic', model: 'claude-sonnet-4-5' },
    metaHarness: {},
    mcpServers: {},
  },
});

function setup(options: { withRuntime?: boolean } = {}): {
  app: Hono;
  agent: Agent;
  engine: ChatEngine;
  fakeEngine: FakeEngine | null;
  service: ChatService;
} {
  const database = openDatabase(':memory:');
  migrate(database);
  db = database;
  const agents = new AgentsRepository(database);
  const agent = agents.create(withStoredProvider(database, draft));
  const env = testEnv(
    options.withRuntime === false ? {} : { storeToken: 'store-token', rivetStoragePath: '/tmp/rivet' },
  );
  // Without the runtime, let the app probe the environment so the route sees a real
  // `not_configured` engine instead of a fake that would mask the status.
  const engine = options.withRuntime === false ? createChatEngine(env) : new FakeEngine();
  const service = new ChatService({ env, engine, agents, chats: new ChatsRepository(database) });
  return {
    app: createApp({ env, db: database, engine, chatService: service }),
    agent,
    engine,
    fakeEngine: engine instanceof FakeEngine ? engine : null,
    service,
  };
}

async function createChat(app: Hono, agentId: string): Promise<ChatResponse> {
  const response = await app.request('/api/chats', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ agentId }),
  });
  expect(response.status).toBe(201);
  return ChatResponse.parse(await response.json());
}

async function firstChunk(response: Response): Promise<string> {
  const reader = response.body?.getReader();
  if (!reader) throw new Error('missing stream');
  const chunk = await reader.read();
  await reader.cancel();
  return new TextDecoder().decode(chunk.value);
}

describe('chat routes', () => {
  test('lists an empty collection', async () => {
    const { app } = setup();
    const response = await app.request('/api/chats');
    expect(response.status).toBe(200);
    expect(ChatListResponse.parse(await response.json())).toEqual({ chats: [] });
  });

  test('opening events creates a session before starting the projection', async () => {
    const { app, agent, fakeEngine } = setup();
    if (!fakeEngine) throw new Error('missing fake engine');
    const { chat } = await createChat(app, agent.id);
    const actor = fakeEngine.actors.get(chat.chatId);
    if (!actor) throw new Error('missing actor');

    const opening = await firstChunk(await app.request(`/api/chats/${chat.chatId}/events`));

    expect(opening).toContain('event: snapshot');
    expect(actor.actions.map((action) => action.type)).toContain('create_new_session');
    const list = ChatListResponse.parse(await (await app.request('/api/chats')).json());
    expect(list.chats.find((item) => item.chatId === chat.chatId)?.sessionId).toBe('session-1');
  });

  test('creates, sends, cancels, closes and removes a chat', async () => {
    const { app, agent } = setup();
    const { chat } = await createChat(app, agent.id);

    const send = await app.request(`/api/chats/${chat.chatId}/send`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ requestId: 'request-1', text: 'hello' }),
    });
    expect(send.status).toBe(200);
    expect(ChatSendResponse.parse(await send.json()).sessionId).toBe('session-1');

    const cancel = await app.request(`/api/chats/${chat.chatId}/cancel`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ requestId: 'cancel-1' }),
    });
    expect(cancel.status).toBe(200);

    const close = await app.request(`/api/chats/${chat.chatId}/close`, { method: 'POST' });
    expect(close.status).toBe(200);
    expect(ChatCloseResponse.parse(await close.json())).toEqual({ status: 'closed', errors: [] });

    const remove = await app.request(`/api/chats/${chat.chatId}`, { method: 'DELETE' });
    expect(remove.status).toBe(204);
    expect(ChatListResponse.parse(await (await app.request('/api/chats')).json()).chats).toEqual([]);
  });

  test('re-entering a normally closed chat streams the stored history', async () => {
    const { app, agent, fakeEngine } = setup();
    if (!fakeEngine) throw new Error('missing fake engine');
    const { chat } = await createChat(app, agent.id);
    const actor = fakeEngine.actors.get(chat.chatId);
    if (!actor) throw new Error('missing actor');

    const send = await app.request(`/api/chats/${chat.chatId}/send`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ requestId: 'request-1', text: '核验' }),
    });
    expect(send.status).toBe(200);

    const close = await app.request(`/api/chats/${chat.chatId}/close`, { method: 'POST' });
    expect(close.status).toBe(200);
    expect(ChatCloseResponse.parse(await close.json())).toEqual({ status: 'closed', errors: [] });

    const binding = { ...BINDING, bindingRevision: 2 };
    actor.actions.length = 0;
    actor.factsResult = {
      binding,
      envelopes: [{ ...userInput('history-1', '核验成功', 1), ...binding }],
      overflow: false,
    };

    const events = await app.request(`/api/chats/${chat.chatId}/events`);
    expect(events.status).toBe(200);
    const opening = await firstChunk(events);
    expect(opening).toContain('event: snapshot');
    expect(opening).toContain('核验成功');
    expect(actor.actions.map((action) => action.type)).toEqual(['change_session']);
  });

  test('validates send input and missing chats', async () => {
    const { app, agent } = setup();
    const { chat } = await createChat(app, agent.id);

    const blank = await app.request(`/api/chats/${chat.chatId}/send`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ requestId: 'request-1', text: '   ' }),
    });
    expect(blank.status).toBe(400);

    const missing = await app.request('/api/chats/missing/send', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ requestId: 'request-1', text: 'hello' }),
    });
    expect(missing.status).toBe(404);
    expect(ApiErrorBody.parse(await missing.json()).error.code).toBe('not_found');
  });

  test('reports not_configured with the missing reasons', async () => {
    const { app, agent } = setup({ withRuntime: false });
    const response = await app.request('/api/chats', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ agentId: agent.id }),
    });

    expect(response.status).toBe(503);
    const body = ApiErrorBody.parse(await response.json());
    expect(body.error.code).toBe('engine_unavailable');
    expect(body.error.details.reasons).toEqual(expect.arrayContaining(['store_token_missing']));
  });

  test('returns HTTP 502 for a refused session', async () => {
    const { app, agent, fakeEngine } = setup();
    if (!fakeEngine) throw new Error('missing fake engine');
    const { chat } = await createChat(app, agent.id);
    const actor = fakeEngine.actors.get(chat.chatId);
    if (!actor) throw new Error('missing actor');
    actor.failCreateSessionWith = Object.assign(new Error('Store refused the new session'), {
      code: 'session_creation_failed',
    });

    const response = await app.request(`/api/chats/${chat.chatId}/send`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ requestId: 'request-1', text: 'hello' }),
    });

    expect(response.status).toBe(502);
    const body = ApiErrorBody.parse(await response.json());
    expect(body.error.code).toBe('session_creation_failed');
    expect(body.error.retryable).toBe(true);
  });

  test('reports store_unreachable and refuses creation when the Store is down', async () => {
    const database = openDatabase(':memory:');
    migrate(database);
    db = database;
    const agent = new AgentsRepository(database).create(withStoredProvider(database, draft));
    const env = testEnv({
      storeToken: 'store-token',
      rivetStoragePath: '/tmp/rivet',
    });
    const engine = await createChatEngineWithStoreProbe(env, {
      probe: async () => {
        throw new Error('ECONNREFUSED');
      },
    });
    const app = createApp({ env, db: database, engine });

    const response = await app.request('/api/chats', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ agentId: agent.id }),
    });
    expect(response.status).toBe(503);
    const body = ApiErrorBody.parse(await response.json());
    expect(body.error.code).toBe('engine_unavailable');
    expect(body.error.details.reasons).toEqual(['store_unreachable']);

    const capabilitiesResponse = await app.request('/api/capabilities');
    const rawCapabilities = await capabilitiesResponse.text();
    expect(rawCapabilities).not.toContain('store-token');
    const capabilities = AgentCapabilities.parse(JSON.parse(rawCapabilities));
    expect(capabilities.chatRuntime).toBe('not_configured');
    expect(capabilities.chatRuntimeReasons).toEqual(['store_unreachable']);
    expect(capabilities.historyPersisted).toBe(false);
  });

  test('replays events after the Last-Event-ID header without a snapshot', async () => {
    const { app, agent, fakeEngine } = setup();
    if (!fakeEngine) throw new Error('missing fake engine');
    const { chat } = await createChat(app, agent.id);
    const actor = fakeEngine.actors.get(chat.chatId);
    if (!actor) throw new Error('missing actor');
    actor.factsResult = {
      binding: { chatId: chat.chatId, sessionId: 'session-1', bindingRevision: 1 },
      envelopes: [{ ...userInput('input-1', 'hello', 1), chatId: chat.chatId }],
      overflow: false,
    };

    const opening = await firstChunk(await app.request(`/api/chats/${chat.chatId}/events`));
    expect(opening).toContain('event: snapshot');

    const emit = (sequence: number, text: string) => {
      actor.emit({ ...agentChunk('message-1', text, sequence), chatId: chat.chatId });
    };
    emit(2, 'first');
    await tick();
    emit(3, 'second');
    await tick();

    const replay = await app.request(`/api/chats/${chat.chatId}/events`, {
      headers: { 'Last-Event-ID': '1' },
    });
    expect(replay.status).toBe(200);
    const body = await firstChunk(replay);
    expect(body).toContain('id: 2');
    expect(body).toContain('event: message');
    expect(body).toContain('second');
    expect(body).not.toContain('event: snapshot');
  });

  test('R02 真实 Hono 重连仍失败返回安全 503，恢复时并发请求 singleflight', async () => {
    const { app, agent, fakeEngine, service } = setup();
    if (!fakeEngine) throw new Error('missing fake engine');
    const { chat } = await createChat(app, agent.id);
    const actor = fakeEngine.actors.get(chat.chatId);
    if (!actor) throw new Error('missing actor');
    const facts = (revision: number) => {
      const binding = { chatId: chat.chatId, sessionId: `s${revision}`, bindingRevision: revision };
      return {
        binding,
        envelopes: [{ ...userInput(`m${revision}`, `history-${revision}`), ...binding }] as [
          ReturnType<typeof userInput>,
        ],
        overflow: false,
      };
    };
    let release = () => {};
    const pending = new Promise<void>((resolve) => {
      release = resolve;
    });
    let cancelInitial = async () => {};
    let reads = 0;
    let active = 0;
    let maxActive = 0;
    let reachable = false;
    const secret = crypto.randomUUID() + crypto.randomUUID();
    const originalConsole = console.error;
    const logs: unknown[][] = [];
    console.error = (...args) => {
      logs.push(args);
    };
    try {
      actor.factsResult = facts(1);
      const initial = await app.request(`/api/chats/${chat.chatId}/events`);
      const reader = initial.body?.getReader();
      if (!reader) throw new Error('missing stream');
      cancelInitial = () => reader.cancel();
      expect(new TextDecoder().decode((await reader.read()).value)).toContain('event: snapshot');
      actor.facts = async () => {
        reads += 1;
        active += 1;
        maxActive = Math.max(maxActive, active);
        try {
          if (!reachable) throw new Error(secret);
          await pending;
          return facts(3);
        } finally {
          active -= 1;
        }
      };
      actor.emit(facts(2).envelopes[0]);
      await tick();
      const reset = new TextDecoder().decode((await reader.read()).value);
      expect(reset).toContain('event: reset');
      const failed = await app.request(`/api/chats/${chat.chatId}/events`);
      expect(failed.status).toBe(503);
      expect(failed.headers.get('content-type')).toContain('application/json');
      const error = await failed.text();
      expect(ApiErrorBody.parse(JSON.parse(error)).error).toMatchObject({
        code: 'engine_unavailable',
        retryable: true,
      });
      expect(error).not.toContain(secret);
      expect((await reader.read()).done).toBe(true);
      reachable = true;
      const reconnects = Array.from({ length: 3 }, () => app.request(`/api/chats/${chat.chatId}/events`));
      await tick();
      expect(reads).toBe(3);
      actor.emit(facts(3).envelopes[0]);
      release();
      const responses = await Promise.all(reconnects);
      for (const response of responses) {
        expect(response.status).toBe(200);
        const body = await firstChunk(response);
        expect(body).toContain('event: snapshot');
        expect(body).toContain('history-3');
      }
      expect(reads).toBe(3);
      expect(maxActive).toBe(1);
      expect(JSON.stringify(logs)).not.toContain(secret);
    } finally {
      release();
      await cancelInitial();
      await service.closeAllChats();
      console.error = originalConsole;
    }
    expect(actor.listeners.size).toBe(0);
  });

  test('R02 prepare 后 subscribe 回读失败仍返回 503，不提交 200 空流', async () => {
    const { app, agent, fakeEngine, service } = setup();
    if (!fakeEngine) throw new Error('missing fake engine');
    const { chat } = await createChat(app, agent.id);
    const actor = fakeEngine.actors.get(chat.chatId);
    if (!actor) throw new Error('missing actor');
    actor.factsResult = {
      binding: { chatId: chat.chatId, sessionId: 'session-1', bindingRevision: 1 },
      envelopes: [{ ...userInput('m1', 'history'), chatId: chat.chatId }],
      overflow: false,
    };
    let reads = 0;
    try {
      const hub = await service.prepareStream(chat.chatId);
      const originalSubscribe = hub.subscribe.bind(hub);
      let inject = true;
      actor.facts = async () => {
        reads += 1;
        throw new Error('synthetic subscribe failure');
      };
      hub.subscribe = (...args) => {
        if (inject) {
          inject = false;
          actor.emit({
            ...userInput('m2', 'signal'),
            chatId: chat.chatId,
            sessionId: 's2',
            bindingRevision: 2,
          });
        }
        return originalSubscribe(...args);
      };
      const response = await app.request(`/api/chats/${chat.chatId}/events`);
      expect(response.status).toBe(503);
      expect(ApiErrorBody.parse(await response.json()).error).toMatchObject({
        code: 'engine_unavailable',
        retryable: true,
      });
      expect(reads).toBe(1);
      actor.facts = async () => actor.factsResult;
      const recovered = await app.request(`/api/chats/${chat.chatId}/events`);
      expect(recovered.status).toBe(200);
      expect(await firstChunk(recovered)).toContain('event: snapshot');
    } finally {
      await service.closeAllChats();
    }
    expect(actor.listeners.size).toBe(0);
  });

  test('streams a snapshot and closes the response on abort', async () => {
    const { app, agent } = setup();
    const { chat } = await createChat(app, agent.id);

    const response = await app.request(`/api/chats/${chat.chatId}/events`);
    expect(response.status).toBe(200);
    expect(response.headers.get('content-type')).toContain('text/event-stream');
    expect(response.body).not.toBeNull();

    const reader = response.body?.getReader();
    if (!reader) throw new Error('missing stream');
    const first = await reader.read();
    const text = new TextDecoder().decode(first.value);
    expect(text).toContain('event: snapshot');
    expect(text).toContain(`"chatId":"${chat.chatId}"`);
    await reader.cancel();
  });
});
