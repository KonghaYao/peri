import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import type { AgentDraft } from '@shared/agent';
import { AgentDraft as AgentDraftSchema } from '@shared/agent';
import { AgentsRepository } from '../../../server/agents/repository';
import { ChatsRepository } from '../../../server/chats/repository';
import { ChatService } from '../../../server/chats/service';
import { openDatabase } from '../../../server/db/database';
import { migrate } from '../../../server/db/migrate';
import { SandboxCoordinator } from '../../../server/sandbox/coordinator';
import { testEnv, withStoredProvider } from '../helpers';
import { type FakeActor, FakeEngine } from './helpers';

let db: Database | null = null;
afterEach(() => {
  db?.close();
  db = null;
});

const TOKEN = 'sandbox-actor-token-0123456789';

const providerDraft: AgentDraft = AgentDraftSchema.parse({
  name: 'Sandbox Agent',
  description: '',
  config: {
    instructions: 'Use the workspace',
    provider: { id: 'anthropic-main', type: 'anthropic', model: 'claude-sonnet-4-5' },
    metaHarness: {},
    mcpServers: { docs: { type: 'http', url: 'https://example.com/mcp' } },
  },
});

function sandboxEnv() {
  return testEnv({
    sandbox: {
      enabled: true,
      mode: 'ready',
      reasons: [],
      periPath: '/tmp/peri',
      settingsPath: undefined,
      sandboxesRoot: '/tmp/agent-builder-sandboxes',
      profileName: 'default',
      token: TOKEN,
      gatewayToken: 'default',
      startupTimeoutMs: 90_000,
      stopTimeoutMs: 20_000,
      dataDir: '/tmp',
      workspacePathPrivate: true,
    },
  });
}

function setup(draft: AgentDraft = providerDraft) {
  const database = openDatabase(':memory:');
  migrate(database);
  db = database;
  const agents = new AgentsRepository(database);
  const chats = new ChatsRepository(database);
  const agent = agents.create(withStoredProvider(database, draft));
  const engine = new FakeEngine();
  const env = sandboxEnv();
  const sandbox = new SandboxCoordinator(env, {
    endpoint: engine.endpoint,
    actor: (uri) => engine.sandbox(uri),
  });
  let counter = 0;
  const service = new ChatService({
    env,
    engine,
    chats,
    agents,
    sandbox,
    generateId: () => `id-${++counter}`,
  });
  return { service, engine, sandbox, agent, chats, env };
}

function gate() {
  let resolve = () => {};
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function targetOf(engine: FakeEngine, chatId: string) {
  const target = engine.actors.get(chatId)?.opened[0];
  if (!target) throw new Error('actor was not opened');
  return target;
}

describe('ChatService sandbox lifecycle', () => {
  test('acquires before open and injects the reserved MCP entry with server-only headers', async () => {
    const { service, engine, agent, env } = setup();
    const chat = await service.create({ agentId: agent.id });

    const target = targetOf(engine, chat.chatId);
    expect(target.workspace).toBe(env.workspaceUri);
    const servers = target.session?.mcpServers as Record<string, unknown>;
    const injected = servers['peri-workspace-sandbox'] as {
      type: string;
      url: string;
      headers: Record<string, string>;
    };
    expect(injected.type).toBe('http');
    expect(injected.url).toContain('/gateway/actor-1/request/mcp');
    expect(injected.headers['x-peri-sandbox-token']).toBe(TOKEN);
    expect(injected.headers['x-rivet-token']).toBe('default');
    expect(servers.docs).toEqual({ type: 'http', url: 'https://example.com/mcp' });
    expect(
      (target.agent as { settings: { config: { meta_harness?: unknown } } }).settings.config.meta_harness,
    ).toEqual({ McpMiddleware: true });
    expect(chat.sandbox).toEqual({ state: 'ready', reason: null });
    // The token never appears in the chat boundary.
    expect(JSON.stringify(chat)).not.toContain(TOKEN);
  });

  test('an explicit McpMiddleware:false fails before holding the sandbox', async () => {
    const draft = AgentDraftSchema.parse({
      ...providerDraft,
      config: { ...providerDraft.config, metaHarness: { McpMiddleware: false } },
    });
    const { service, engine, agent, chats } = setup(draft);

    await expect(service.create({ agentId: agent.id })).rejects.toMatchObject({
      code: 'not_ready',
      details: { reason: 'sandbox_mcp_disabled' },
    });
    const sandboxActor = [...engine.sandboxes.values()][0];
    expect(sandboxActor?.acquireCalls ?? []).toEqual([]);
    expect(chats.list()).toEqual([]);
  });

  test('releases the holder and rolls back the row when Chat open fails', async () => {
    const { service, engine, agent, chats } = setup();
    const actorEngine = engine as FakeEngine;
    const originalActor = actorEngine.actor.bind(actorEngine);
    actorEngine.actor = ((chatId: string) => {
      const actor = originalActor(chatId) as FakeActor;
      actor.failOpenAlwaysWith = Object.assign(new Error('binding failed'), { code: 'binding_conflict' });
      return actor;
    }) as typeof actorEngine.actor;

    await expect(service.create({ agentId: agent.id })).rejects.toMatchObject({
      code: 'binding_conflict',
    });
    const sandboxActor = [...engine.sandboxes.values()][0];
    expect(sandboxActor?.releaseCalls).toHaveLength(1);
    expect(chats.list()).toEqual([]);
  });

  test('keeps the row and holder when a failed create cannot close its actor', async () => {
    const { service, engine, agent, chats, sandbox } = setup();
    const actorEngine = engine as FakeEngine;
    const originalActor = actorEngine.actor.bind(actorEngine);
    actorEngine.actor = ((chatId: string) => {
      const actor = originalActor(chatId) as FakeActor;
      actor.failOpenAlwaysWith = Object.assign(new Error('binding failed'), { code: 'binding_conflict' });
      actor.failCloseWith = Object.assign(new Error('close failed'), { code: 'internal_error' });
      return actor;
    }) as typeof actorEngine.actor;

    await expect(service.create({ agentId: agent.id })).rejects.toMatchObject({
      code: 'internal_error',
      cause: expect.objectContaining({ message: 'close failed' }),
    });
    expect(chats.list()).toHaveLength(1);
    const held = chats.list()[0];
    if (!held) return;
    const sandboxActor = [...engine.sandboxes.values()][0];
    expect(sandboxActor?.releaseCalls).toHaveLength(0);
    expect(sandbox.state(held.chatId).state).toBe('ready');
  });

  test('a failed Chat close does not release the sandbox holder', async () => {
    const { service, engine, sandbox, agent } = setup();
    const chat = await service.create({ agentId: agent.id });
    const actor = engine.actors.get(chat.chatId);
    if (!actor) throw new Error('actor missing');
    actor.failCloseWith = Object.assign(new Error('close timeout'), { code: 'internal_error' });

    const closed = await service.close(chat.chatId);
    expect(closed.status).toBe('close_incomplete');
    const sandboxActor = [...engine.sandboxes.values()][0];
    expect(sandboxActor?.releaseCalls).toHaveLength(0);
    expect(sandbox.state(chat.chatId).state).toBe('ready');
  });

  test('a confirmed Chat close with an unconfirmed sandbox release reports incomplete', async () => {
    const { service, engine, agent } = setup();
    const chat = await service.create({ agentId: agent.id });
    const sandboxActor = [...engine.sandboxes.values()][0];
    if (!sandboxActor) throw new Error('sandbox actor missing');
    sandboxActor.failRelease = { code: 'sandbox_close_incomplete', message: 'drain timeout' };

    const closed = await service.close(chat.chatId);
    expect(closed).toEqual({
      status: 'close_incomplete',
      errors: ['sandbox_close_incomplete'],
    });
  });

  test('remove keeps the row until both Chat and sandbox closes are confirmed', async () => {
    const { service, engine, chats, agent } = setup();
    const chat = await service.create({ agentId: agent.id });
    const sandboxActor = [...engine.sandboxes.values()][0];
    if (!sandboxActor) throw new Error('sandbox actor missing');
    sandboxActor.failRelease = { code: 'sandbox_close_incomplete', message: 'drain timeout' };

    await expect(service.remove(chat.chatId)).rejects.toMatchObject({
      code: 'close_incomplete',
      retryable: true,
    });
    expect(chats.get(chat.chatId)).not.toBeNull();

    // Retry succeeds because the actor failure is consumed and the holder is still known.
    await service.remove(chat.chatId);
    expect(chats.get(chat.chatId)).toBeNull();
  });

  test('ABV-01 delete retains both row and holder after actor close failure, then retries', async () => {
    const { service, engine, sandbox, chats, agent, env } = setup();
    const chat = await service.create({ agentId: agent.id });
    const actor = engine.actors.get(chat.chatId);
    if (!actor) throw new Error('actor missing');
    actor.failCloseWith = new Error('not confirmed');
    await expect(service.remove(chat.chatId)).rejects.toMatchObject({ code: 'close_incomplete' });
    expect(chats.get(chat.chatId)).not.toBeNull();
    expect(sandbox.state(chat.chatId).state).toBe('ready');
    expect(engine.sandbox(env.workspaceUri).releaseCalls).toHaveLength(0);
    actor.failCloseWith = null;
    await service.remove(chat.chatId);
    expect(chats.get(chat.chatId)).toBeNull();
    expect(engine.sandbox(env.workspaceUri).releaseCalls).toHaveLength(1);
  });

  test('ABV-02 stop releases only confirmed chats and retries the failed holder', async () => {
    const { service, engine, sandbox, agent, env } = setup();
    const first = await service.create({ agentId: agent.id });
    const second = await service.create({ agentId: agent.id });
    const actor = engine.actors.get(first.chatId);
    if (!actor) throw new Error('actor missing');
    actor.failCloseWith = new Error('not confirmed');
    expect((await service.closeAllChats()).complete).toBe(false);
    expect(engine.sandbox(env.workspaceUri).releaseCalls.map((call) => call.holder)).toEqual([second.chatId]);
    expect(sandbox.state(first.chatId).state).toBe('ready');
    actor.failCloseWith = null;
    expect((await service.closeAllChats()).complete).toBe(true);
    expect(engine.sandbox(env.workspaceUri).releaseCalls).toHaveLength(2);
  });

  test.each(['open', 'execute'] as const)(
    'ABV-03 close waits for admitted send at %s and rejects later work',
    async (phase) => {
      const { service, engine, sandbox, agent, env } = setup();
      const chat = await service.create({ agentId: agent.id });
      const actor = engine.actors.get(chat.chatId);
      if (!actor) throw new Error('actor missing');
      const entered = gate();
      const resume = gate();
      const originalOpen = actor.open.bind(actor);
      const originalExecute = actor.execute.bind(actor);
      actor.open = async (target) => {
        if (phase === 'open') {
          entered.resolve();
          await resume.promise;
        }
        actor.closed = false;
        return originalOpen(target);
      };
      actor.execute = async (action) => {
        if (phase === 'execute' && action.type === 'send') {
          entered.resolve();
          await resume.promise;
        }
        return originalExecute(action);
      };
      const send = service.send(chat.chatId, 'before-close', 'hello');
      await entered.promise;
      let closeFinished = false;
      const close = service.close(chat.chatId).then((result) => {
        closeFinished = true;
        return result;
      });
      const doubleClose = service.close(chat.chatId);
      await expect(service.send(chat.chatId, 'after-close', 'no')).rejects.toMatchObject({ code: 'closed' });
      expect(closeFinished).toBe(false);
      expect(sandbox.state(chat.chatId).state).toBe('ready');
      resume.resolve();
      await send;
      expect((await close).status).toBe('closed');
      await doubleClose;
      expect(actor.closed).toBe(true);
      expect(engine.sandbox(env.workspaceUri).releaseCalls).toHaveLength(1);
    },
  );

  test.each(['close', 'remove', 'stop'] as const)(
    'ABV-03 create/acquire is ordered before %s',
    async (operation) => {
      const { service, engine, chats, agent, env } = setup();
      const actor = engine.sandbox(env.workspaceUri);
      const entered = gate();
      const resume = gate();
      const acquire = actor.acquire.bind(actor);
      actor.acquire = async (...args) => {
        entered.resolve();
        await resume.promise;
        return acquire(...args);
      };
      const creating = service.create({ agentId: agent.id });
      await entered.promise;
      const created = chats.list()[0];
      if (!created) throw new Error('chat missing');
      const id = created.chatId;
      const closing =
        operation === 'stop'
          ? service.closeAllChats()
          : operation === 'remove'
            ? service.remove(id)
            : service.close(id);
      expect(actor.releaseCalls).toHaveLength(0);
      resume.resolve();
      await creating;
      await closing;
      expect(engine.actors.get(id)?.closed).toBe(true);
      expect(actor.releaseCalls).toHaveLength(1);
      if (operation === 'stop')
        await expect(service.create({ agentId: agent.id })).rejects.toMatchObject({ code: 'closed' });
    },
  );

  test.each(['remove', 'stop'] as const)(
    'ABV-03 %s drains a paused send before release',
    async (operation) => {
      const { service, engine, agent, env } = setup();
      const chat = await service.create({ agentId: agent.id });
      const actor = engine.actors.get(chat.chatId);
      if (!actor) throw new Error('actor missing');
      const entered = gate();
      const resume = gate();
      const execute = actor.execute.bind(actor);
      actor.execute = async (action) => {
        if (action.type === 'send') {
          entered.resolve();
          await resume.promise;
        }
        return execute(action);
      };
      const send = service.send(chat.chatId, 'send', 'hello');
      await entered.promise;
      const closing = operation === 'remove' ? service.remove(chat.chatId) : service.closeAllChats();
      expect(engine.sandbox(env.workspaceUri).releaseCalls).toHaveLength(0);
      await expect(service.send(chat.chatId, 'late', 'no')).rejects.toMatchObject({ code: 'closed' });
      resume.resolve();
      await send;
      await closing;
      expect(actor.closed).toBe(true);
      expect(engine.sandbox(env.workspaceUri).releaseCalls).toHaveLength(1);
    },
  );

  test('ABV-03 stop during readiness does not create a late row or activation', async () => {
    const { service, engine, chats, agent } = setup();
    const entered = gate();
    const resume = gate();
    engine.start = async () => {
      entered.resolve();
      await resume.promise;
    };
    const creating = service.create({ agentId: agent.id });
    await entered.promise;
    expect((await service.closeAllChats()).complete).toBe(true);
    resume.resolve();
    await expect(creating).rejects.toMatchObject({ code: 'closed' });
    expect(chats.list()).toEqual([]);
    expect(engine.actors.size).toBe(0);
  });

  test('two chats on one workspace hold independent holders and release independently', async () => {
    const { service, engine, sandbox, agent } = setup();
    const first = await service.create({ agentId: agent.id });
    const second = await service.create({ agentId: agent.id });
    const sandboxActor = [...engine.sandboxes.values()][0];
    if (!sandboxActor) throw new Error('sandbox actor missing');
    expect(sandboxActor.acquireCalls.map((call) => call.holder)).toEqual([first.chatId, second.chatId]);

    expect((await service.close(first.chatId)).status).toBe('closed');
    expect(sandbox.state(second.chatId).state).toBe('ready');
    expect((await service.close(second.chatId)).status).toBe('closed');
    expect(sandboxActor.releaseCalls.map((call) => call.holder)).toEqual([first.chatId, second.chatId]);
  });
});
