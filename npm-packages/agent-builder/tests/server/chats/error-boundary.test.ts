import { expect, test } from 'bun:test';
import { inspect } from 'node:util';
import type { Hono } from 'hono';
import { AgentsRepository } from '../../../server/agents/repository';
import { createApp } from '../../../server/app';
import { ChatsRepository } from '../../../server/chats/repository';
import { ChatService } from '../../../server/chats/service';
import { openDatabase } from '../../../server/db/database';
import { migrate } from '../../../server/db/migrate';
import { AppError } from '../../../server/errors';
import { createPlatformRepositories } from '../../../server/platform/repositories';
import { SessionRecorder } from '../../../server/platform/sessions/recorder';
import {
  createSessionRuntime,
  installSessionRuntime,
  sessionDraftFor,
} from '../../../server/platform/sessions/runtime';
import { testEnv, withStoredProvider } from '../helpers';
import { BINDING, envelope, FakeEngine, userInput } from './helpers';

const AUTHORIZATION = { authorization: 'Bearer test-token' } as const;

function inputEvent(text: string) {
  return {
    events: [
      {
        type: 'agent.session.input.message',
        input: [{ role: 'user', content: [{ type: 'input_text', text }] }],
      },
    ],
  };
}

async function createSession(app: Hono, agentId: string): Promise<string> {
  const response = await app.request('/v1/agents/sessions', {
    method: 'POST',
    headers: { ...AUTHORIZATION, 'content-type': 'application/json' },
    body: JSON.stringify({ environment: { type: 'none' }, agent_id: agentId, input: 'hello' }),
  });
  expect(response.status).toBe(201);
  return ((await response.json()) as { id: string }).id;
}

test('ABV-04 random secret in actor errors never reaches response/SSE/log; original cause survives', async () => {
  const secret = crypto.randomUUID() + crypto.randomUUID();
  const failure = (code: string) =>
    Object.assign(new Error(secret, { cause: new Error(secret) }), { code, name: secret });
  const db = openDatabase(':memory:');
  migrate(db);
  const agents = new AgentsRepository(db);
  const agent = agents.create(
    withStoredProvider(
      db,
      {
        name: 'test',
        description: '',
        config: {
          instructions: '',
          provider: { id: 'p', type: 'anthropic', model: 'm' },
          metaHarness: {},
          mcpServers: {},
        },
      },
      secret,
    ),
  );
  const env = testEnv({ storeToken: secret });
  const engine = new FakeEngine();
  const repos = createPlatformRepositories(db);
  const recorder = new SessionRecorder({ repos });
  const service = new ChatService({
    env,
    agents,
    chats: new ChatsRepository(db),
    engine,
    draftFor: sessionDraftFor(repos),
  });
  const restore = installSessionRuntime(createSessionRuntime({ repos, chats: service, recorder }));
  const app = createApp({ env, db, engine, chatService: service });
  app.get('/fault', () => {
    throw failure(secret);
  });
  app.get('/app-fault', () => {
    throw new AppError('engine_unavailable', secret, {
      cause: failure(secret),
      details: { reason: secret, reasons: [secret], secret },
    });
  });
  const logs: string[] = [];
  const original = console.error;
  console.error = (...args) => {
    logs.push(inspect(args, { depth: 10 }));
  };
  const bodies: string[] = [];
  const sseChunks: string[] = [];
  try {
    const chatId = await createSession(app, agent.id);
    const actor = engine.actors.get(chatId);
    if (!actor) throw new Error('actor missing');
    actor.failCloseWith = failure('internal_error');
    const closed = await app.request(`/v1/agents/sessions/${chatId}`, {
      method: 'DELETE',
      headers: AUTHORIZATION,
    });
    expect(closed.status).toBe(409);
    bodies.push(await closed.text());
    await expect(service.remove(chatId)).rejects.toMatchObject({ cause: actor.failCloseWith });
    actor.failCloseWith = null;
    const removed = await app.request(`/v1/agents/sessions/${chatId}`, {
      method: 'DELETE',
      headers: AUTHORIZATION,
    });
    expect(removed.status).toBe(200);
    bodies.push(await removed.text());

    const nextId = await createSession(app, agent.id);
    const nextActor = engine.actors.get(nextId);
    if (!nextActor) throw new Error('actor missing');
    for (const [code, status] of [
      ['engine_unavailable', 503],
      ['internal_error', 500],
      [secret, 500],
    ] as const) {
      const error = failure(code);
      nextActor.failOpenAlwaysWith = error;
      await expect(service.send(nextId, crypto.randomUUID(), 'hello')).rejects.toMatchObject({
        cause: error,
      });
      const response = await app.request(`/v1/agents/sessions/${nextId}/events`, {
        method: 'POST',
        headers: { ...AUTHORIZATION, 'content-type': 'application/json' },
        body: JSON.stringify(inputEvent('hello')),
      });
      expect(response.status).toBe(status);
      bodies.push(await response.text());
    }
    nextActor.failOpenAlwaysWith = null;
    expect((await service.close(nextId)).status).toBe('closed');
    nextActor.facts = async () => {
      throw failure('internal_error');
    };
    const failedStream = await app.request(`/v1/agents/sessions/${nextId}/events`, {
      headers: AUTHORIZATION,
    });
    expect(failedStream.status).toBe(500);
    bodies.push(await failedStream.text());
    nextActor.facts = async () => ({
      binding: { ...BINDING, chatId: nextId },
      envelopes: [{ ...userInput('q', 'safe text'), chatId: nextId }],
      overflow: false,
    });
    const stream = await app.request(`/v1/agents/sessions/${nextId}/events`, {
      headers: AUTHORIZATION,
    });
    expect(stream.status).toBe(200);
    const reader = stream.body?.getReader();
    if (!reader) throw new Error('stream missing');
    sseChunks.push(new TextDecoder().decode((await reader.read()).value));
    nextActor.emit({
      ...envelope(2, { kind: 'closed', reason: { code: secret, message: secret } } as never),
      chatId: nextId,
    });
    sseChunks.push(new TextDecoder().decode((await reader.read()).value));
    await reader.cancel();
    await service.remove(nextId);

    bodies.push(await (await app.request('/fault')).text());
    bodies.push(await (await app.request('/app-fault')).text());
    expect(sseChunks).toHaveLength(2);
    expect(sseChunks.every((chunk) => chunk.includes('event:'))).toBe(true);
    expect(bodies.some((body) => body.includes(secret))).toBe(false);
    expect(sseChunks.some((chunk) => chunk.includes(secret))).toBe(false);
    expect(logs.some((line) => line.includes(secret))).toBe(false);
  } finally {
    console.error = original;
    restore();
    db.close();
  }
});
