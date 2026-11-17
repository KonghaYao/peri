import { expect, test } from 'bun:test';
import { inspect } from 'node:util';
import { AgentsRepository } from '../../../server/agents/repository';
import { createApp } from '../../../server/app';
import { ChatsRepository } from '../../../server/chats/repository';
import { ChatService } from '../../../server/chats/service';
import { openDatabase } from '../../../server/db/database';
import { migrate } from '../../../server/db/migrate';
import { AppError } from '../../../server/errors';
import { testEnv, withStoredProvider } from '../helpers';
import { BINDING, envelope, FakeEngine, userInput } from './helpers';

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
  const service = new ChatService({ env, agents, chats: new ChatsRepository(db), engine });
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
  try {
    const chat = await service.create({ agentId: agent.id });
    const actor = engine.actors.get(chat.chatId);
    if (!actor) throw new Error('actor missing');
    actor.failCloseWith = failure('internal_error');
    const closed = await app.request(`/api/chats/${chat.chatId}/close`, { method: 'POST' });
    expect(closed.status).toBe(202);
    bodies.push(await closed.text());
    await expect(service.remove(chat.chatId)).rejects.toMatchObject({ cause: actor.failCloseWith });
    actor.failCloseWith = null;
    await service.remove(chat.chatId);

    const next = await service.create({ agentId: agent.id });
    const nextActor = engine.actors.get(next.chatId);
    if (!nextActor) throw new Error('actor missing');
    for (const [code, status] of [
      ['engine_unavailable', 503],
      ['internal_error', 500],
      [secret, 500],
    ] as const) {
      const error = failure(code);
      nextActor.failOpenAlwaysWith = error;
      await expect(service.send(next.chatId, crypto.randomUUID(), 'hello')).rejects.toMatchObject({
        cause: error,
      });
      const response = await app.request(`/api/chats/${next.chatId}/send`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ requestId: crypto.randomUUID(), text: 'hello' }),
      });
      expect(response.status).toBe(status);
      bodies.push(await response.text());
    }
    nextActor.failOpenAlwaysWith = null;
    nextActor.facts = async () => {
      throw failure('internal_error');
    };
    const failedStream = await app.request(`/api/chats/${next.chatId}/events`);
    expect(failedStream.status).toBe(500);
    bodies.push(await failedStream.text());
    nextActor.facts = async () => ({
      binding: { ...BINDING, chatId: next.chatId },
      envelopes: [{ ...userInput('q', 'safe text'), chatId: next.chatId }],
      overflow: false,
    });
    const stream = await app.request(`/api/chats/${next.chatId}/events`);
    const reader = stream.body?.getReader();
    if (!reader) throw new Error('stream missing');
    bodies.push(new TextDecoder().decode((await reader.read()).value));
    nextActor.emit({
      ...envelope(2, { kind: 'closed', reason: { code: secret, message: secret } } as never),
      chatId: next.chatId,
    });
    bodies.push(new TextDecoder().decode((await reader.read()).value));
    await reader.cancel();
    await service.remove(next.chatId);

    bodies.push(await (await app.request('/fault')).text());
    bodies.push(await (await app.request('/app-fault')).text());
    expect(bodies.some((body) => body.includes(secret))).toBe(false);
    expect(logs.some((line) => line.includes(secret))).toBe(false);
  } finally {
    console.error = original;
    db.close();
  }
});
