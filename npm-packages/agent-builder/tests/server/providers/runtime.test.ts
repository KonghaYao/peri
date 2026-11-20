import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import { randomBytes } from 'node:crypto';
import { mkdtempSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { AgentDraft, type AgentPreviewTarget } from '@shared/agent';
import { ProviderCreate } from '@shared/provider';
import { createPreviewResponse } from '../../../server/agents/open-target';
import { AgentsRepository } from '../../../server/agents/repository';
import { createApp } from '../../../server/app';
import { NotConfiguredChatEngine, probeChatRuntime } from '../../../server/chats/engine';
import { ChatsRepository } from '../../../server/chats/repository';
import { ChatService } from '../../../server/chats/service';
import { openDatabase } from '../../../server/db/database';
import { migrate } from '../../../server/db/migrate';
import { loadEnv } from '../../../server/env';
import { ProvidersRepository } from '../../../server/providers/repository';
import { ProviderService } from '../../../server/providers/service';
import { SandboxCoordinator } from '../../../server/sandbox/coordinator';
import { FakeActor, FakeEngine } from '../chats/helpers';
import { testEnv } from '../helpers';

let db: Database;
afterEach(() => db?.close());
function gate() {
  let resolve = () => {};
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
function setup(sandboxEnabled = false) {
  db = openDatabase(':memory:');
  migrate(db);
  const env = testEnv();
  const providers = new ProviderService(new ProvidersRepository(db));
  const keyA = randomBytes(32).toString('hex');
  const keyB = randomBytes(32).toString('hex');
  const a = providers.create(
    ProviderCreate.parse({
      name: 'A',
      type: 'anthropic',
      credentialSource: 'local',
      baseUrl: 'https://a.example/v1',
      credential: { action: 'replace', value: keyA },
    }),
  );
  const b = providers.create(
    ProviderCreate.parse({
      name: 'B',
      type: 'anthropic',
      credentialSource: 'local',
      baseUrl: 'https://b.example/v2',
      credential: { action: 'replace', value: keyB },
    }),
  );
  const agents = new AgentsRepository(db);
  const draft = AgentDraft.parse({
    name: 'A',
    config: {
      provider: { kind: 'reference', providerId: a.id, model: 'model-a' },
      instructions: 'instructions-a',
      metaHarness: { '01_intro': false },
      mcpServers: { docs: { type: 'http', url: 'https://docs.example/mcp' } },
    },
  });
  const agent = agents.create(draft);
  const engine = new FakeEngine();
  const sandboxEnv = {
    ...env,
    sandbox: {
      ...env.sandbox,
      enabled: true,
      mode: 'ready' as const,
      token: randomBytes(24).toString('hex'),
      workspacePathPrivate: true,
    },
  };
  const sandbox = sandboxEnabled
    ? new SandboxCoordinator(sandboxEnv, { endpoint: engine.endpoint, actor: (uri) => engine.sandbox(uri) })
    : undefined;
  const chats = new ChatsRepository(db);
  let id = 0;
  const service = new ChatService({
    env,
    agents,
    providers,
    chats,
    engine,
    ...(sandbox ? { sandbox } : {}),
    generateId: () => `chat-${++id}`,
  });
  return { env, providers, a, b, keyA, keyB, agents, draft, agent, engine, service, chats, sandbox };
}
function firstTarget(engine: FakeEngine, chatId: string): AgentPreviewTarget {
  const target = engine.actors.get(chatId)?.opened[0];
  if (!target) throw new Error('target missing');
  return target as unknown as AgentPreviewTarget;
}

describe('Provider frozen runtime isolation', () => {
  test.each(['anthropic', 'openai'] as const)(
    'env sentinels cannot configure %s, legacy env or inline; only explicit SQLite replace enables new targets',
    async (type) => {
      const root = realpathSync(mkdtempSync(path.join(tmpdir(), 'ab-sqlite-only-')));
      const sentinel = randomBytes(32).toString('hex');
      const key = randomBytes(32).toString('hex');
      db = openDatabase(':memory:');
      migrate(db);
      const env = loadEnv({
        DATABASE_PATH: path.join(root, 'config.db'),
        PERI_WORKSPACE_PATH: root,
        PERI_MACHINE_ID: '00000000-0000-4000-8000-000000000001',
        PERI_SANDBOX_PERI_PATH: path.join(root, 'no-peri'),
        ANTHROPIC_API_KEY: sentinel,
        OPENAI_API_KEY: sentinel,
      });
      const providers = new ProviderService(new ProvidersRepository(db));
      const agents = new AgentsRepository(db);
      const chats = new ChatsRepository(db);
      const engine = new FakeEngine();
      const service = new ChatService({ env, providers, agents, chats, engine });
      const app = createApp({ env, db, engine, chatService: service });
      try {
        const provider = providers.create(ProviderCreate.parse({ name: type, type }));
        const agent = agents.create(
          AgentDraft.parse({
            name: 'Ref',
            config: { provider: { kind: 'reference', providerId: provider.id, model: 'm' } },
          }),
        );
        const inline = agents.create(
          AgentDraft.parse({
            name: 'Inline',
            config: { provider: { id: provider.id, type, model: 'editable' } },
          }),
        );
        const empty = agents.create(AgentDraft.parse({ name: 'Null', config: {} }));
        const create = (agentId: string) => service.create({ agentId });
        expect(providers.get(provider.id).hasCredential).toBe(false);
        expect(probeChatRuntime(env, true).mode).toBe('not_configured');
        for (const legacy of [false, true]) {
          if (legacy)
            db.query("UPDATE providers SET credential_source = 'env' WHERE id = ?").run(provider.id);
          expect(providers.get(provider.id).hasCredential).toBe(false);
          await expect(create(agent.id)).rejects.toMatchObject({
            code: 'not_ready',
            details: { reason: 'api_key_missing' },
          });
          const preview = createPreviewResponse(env, agent, engine.status, undefined, providers);
          expect(preview.warnings.some((warning) => warning.code === 'api_key_missing')).toBe(true);
          expect(preview.target.agent.settings.config.providers[0]?.apiKey).toBeUndefined();
          expect(providers.get(provider.id).revision).toBe(1);
        }
        for (const item of [inline, empty]) {
          await expect(create(item.id)).rejects.toMatchObject({
            code: 'not_ready',
            details: { reason: 'provider_missing' },
          });
        }
        expect(engine.actors.size).toBe(0);
        expect(chats.list()).toHaveLength(0);
        providers.update(
          provider.id,
          {
            name: type,
            baseUrl: null,
            credentialSource: 'local',
            credential: { action: 'replace', value: key },
          },
          1,
        );
        await expect(create(inline.id)).rejects.toMatchObject({ code: 'not_ready' });
        const chat = await create(agent.id);
        const target = firstTarget(engine, chat.chatId);
        expect(target.agent.settings.config.providers[0]?.apiKey === key).toBe(true);
        providers.update(
          provider.id,
          { name: type, baseUrl: null, credentialSource: 'local', credential: { action: 'clear' } },
          2,
        );
        expect(providers.get(provider.id).hasCredential).toBe(false);
        await expect(create(agent.id)).rejects.toMatchObject({ code: 'not_ready' });
        await service.send(chat.chatId, 'frozen', 'safe');
        await service.cancel(chat.chatId);
        expect(
          engine.actors
            .get(chat.chatId)
            ?.opened.every((item) => JSON.stringify(item) === JSON.stringify(target)),
        ).toBe(true);
        for (const url of [
          '/v1/agents',
          `/v1/agents/${agent.id}`,
          '/v1/agents/environments',
          '/v1/vaults',
          '/v1/agents/sessions',
        ]) {
          const body = await (
            await app.request(url, { headers: { authorization: 'Bearer test-token' } })
          ).text();
          expect(body.includes(sentinel) || body.includes(key)).toBe(false);
        }
        expect(JSON.stringify(env).includes(sentinel)).toBe(false);
        const converted = agents.update(
          inline.id,
          AgentDraft.parse({
            name: inline.name,
            description: inline.description,
            config: {
              ...inline.config,
              provider: { kind: 'reference', providerId: provider.id, model: 'editable' },
            },
          }),
          1,
        );
        expect(converted?.config.provider).toEqual({
          kind: 'reference',
          providerId: provider.id,
          model: 'editable',
        });
      } finally {
        await service.shutdown();
        rmSync(root, { recursive: true, force: true });
      }
    },
  );

  test('two same-protocol Providers keep independent keys and per-Agent settings; previews match real open', async () => {
    const { service, agents, providers, agent, a, b, keyA, keyB, engine, env } = setup();
    const other = agents.create(
      AgentDraft.parse({
        name: 'B',
        config: {
          provider: { kind: 'reference', providerId: b.id, model: 'model-b' },
          instructions: 'instructions-b',
          metaHarness: { '01_intro': true },
          mcpServers: {},
        },
      }),
    );
    const chatA = await service.create({ agentId: agent.id });
    const chatB = await service.create({ agentId: other.id });
    const targetA = firstTarget(engine, chatA.chatId);
    const targetB = firstTarget(engine, chatB.chatId);
    expect(targetA.agent.settings.config.providers[0]?.apiKey === keyA).toBe(true);
    expect(targetB.agent.settings.config.providers[0]?.apiKey === keyB).toBe(true);
    expect(targetA.agent.settings.config.profiles.sonnet?.model).toBe('model-a');
    expect(targetB.agent.settings.config.profiles.sonnet?.model).toBe('model-b');
    expect(targetA.session.instructions).toBe('instructions-a');
    expect(targetB.session.instructions).toBe('instructions-b');
    expect(targetA.agent.settings.config.meta_harness).toEqual({ '01_intro': false });
    expect(targetB.agent.settings.config.meta_harness).toEqual({ '01_intro': true });
    expect(targetB.session.mcpServers).toBeUndefined();
    const preview = createPreviewResponse(env, agent, engine.status, undefined, providers);
    expect(preview.target.agent.settings.config.profiles).toEqual(targetA.agent.settings.config.profiles);
    expect(preview.target.agent.settings.config.providers[0]?.baseUrl).toBe(
      targetA.agent.settings.config.providers[0]?.baseUrl,
    );
    providers.update(
      a.id,
      { name: a.name, baseUrl: a.baseUrl, credentialSource: 'local', credential: { action: 'clear' } },
      1,
    );
    const after = createPreviewResponse(env, agent, engine.status, undefined, providers);
    expect(after.warnings.some((warning) => warning.code === 'api_key_missing')).toBe(true);
    await service.send(chatA.chatId, 'r1', 'hello');
    await service.cancel(chatA.chatId);
    await service.prepareStream(chatA.chatId);
    expect(
      engine.actors
        .get(chatA.chatId)
        ?.opened.every((target) => JSON.stringify(target) === JSON.stringify(targetA)),
    ).toBe(true);
    await expect(service.create({ agentId: agent.id })).rejects.toMatchObject({
      code: 'not_ready',
      details: { reason: 'api_key_missing' },
    });
    expect((await service.closeAllChats()).complete).toBe(true);
  });

  test.each(['acquire', 'open'] as const)(
    'freezes synchronously before pending %s; changing Agent does not allow deleting an old runtime Provider',
    async (pause) => {
      const { service, agents, providers, agent, draft, a, b, keyA, keyB, engine, env } = setup(true);
      const entered = gate();
      const resume = gate();
      if (pause === 'acquire') {
        const actor = engine.sandbox(env.workspaceUri);
        const acquire = actor.acquire.bind(actor);
        actor.acquire = async (...args) => {
          entered.resolve();
          await resume.promise;
          return acquire(...args);
        };
      } else {
        const actor = new FakeActor();
        const open = actor.open.bind(actor);
        actor.open = async (target) => {
          entered.resolve();
          await resume.promise;
          return open(target);
        };
        engine.actors.set('chat-1', actor);
      }
      const creating = service.create({ agentId: agent.id });
      await entered.promise;
      agents.update(
        agent.id,
        AgentDraft.parse({
          ...draft,
          config: {
            ...draft.config,
            instructions: 'changed',
            provider: { kind: 'reference', providerId: b.id, model: 'changed-model' },
          },
        }),
        1,
      );
      providers.update(
        a.id,
        {
          name: 'A changed',
          baseUrl: 'https://new.example',
          credentialSource: 'local',
          credential: { action: 'replace', value: keyB },
        },
        1,
      );
      expect(providers.get(a.id).runtimeCount).toBe(1);
      expect(() => providers.delete(a.id, 2)).toThrow();
      resume.resolve();
      const chat = await creating;
      const target = firstTarget(engine, chat.chatId);
      expect(target.agent.settings.config.providers[0]?.apiKey === keyA).toBe(true);
      expect(target.agent.settings.config.providers[0]?.baseUrl).toBe('https://a.example/v1');
      expect(target.session.instructions).toBe('instructions-a');
      await service.send(chat.chatId, 'send', 'hi');
      await service.cancel(chat.chatId);
      const actor = engine.actors.get(chat.chatId);
      if (!actor) throw new Error('actor missing');
      expect(actor.opened.every((item) => JSON.stringify(item) === JSON.stringify(target))).toBe(true);
      actor.failCloseWith = new Error('close unconfirmed');
      expect((await service.close(chat.chatId)).status).toBe('close_incomplete');
      expect(() => providers.delete(a.id, 2)).toThrow();
      actor.failCloseWith = null;
      expect((await service.close(chat.chatId)).status).toBe('closed');
      providers.delete(a.id, 2);
      expect(providers.list().map((item) => item.id)).toEqual([b.id]);
      const next = await service.create({ agentId: agent.id });
      expect(firstTarget(engine, next.chatId).agent.settings.config.providers[0]?.apiKey === keyB).toBe(true);
      await service.close(next.chatId);
    },
  );

  test('failed open plus unconfirmed rollback retains pending reference until real close succeeds', async () => {
    const { service, agent, agents, draft, providers, a, b, engine, chats } = setup();
    const actor = new FakeActor();
    actor.failOpenAlwaysWith = new Error('open failed');
    actor.failCloseWith = new Error('cleanup failed');
    engine.actors.set('chat-1', actor);
    await expect(service.create({ agentId: agent.id })).rejects.toMatchObject({ code: 'internal_error' });
    expect(chats.list()).toHaveLength(1);
    agents.update(
      agent.id,
      AgentDraft.parse({
        ...draft,
        config: { ...draft.config, provider: { kind: 'reference', providerId: b.id, model: 'b' } },
      }),
      1,
    );
    expect(() => providers.delete(a.id, 1)).toThrow();
    actor.failCloseWith = null;
    expect((await service.close('chat-1')).status).toBe('closed');
    providers.delete(a.id, 1);
  });

  test('confirmed actor close but failed Sandbox release retains old Provider reference until retry', async () => {
    const { service, agent, agents, draft, providers, a, b, engine, env } = setup(true);
    const chat = await service.create({ agentId: agent.id });
    agents.update(
      agent.id,
      AgentDraft.parse({
        ...draft,
        config: { ...draft.config, provider: { kind: 'reference', providerId: b.id, model: 'b' } },
      }),
      1,
    );
    engine.sandbox(env.workspaceUri).failRelease = {
      code: 'sandbox_close_incomplete',
      message: 'unconfirmed',
    };
    expect((await service.close(chat.chatId)).status).toBe('close_incomplete');
    expect(() => providers.delete(a.id, 1)).toThrow();
    expect((await service.close(chat.chatId)).status).toBe('closed');
    providers.delete(a.id, 1);
  });

  test('clearing a key during an admitted send and unconfirmed close preserves the frozen target and runtime reference', async () => {
    const { service, agents, draft, agent, providers, a, b, keyA, engine } = setup();
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
    const sending = service.send(chat.chatId, 'pending', 'safe');
    await entered.promise;
    providers.update(
      a.id,
      { name: a.name, baseUrl: a.baseUrl, credentialSource: 'local', credential: { action: 'clear' } },
      1,
    );
    expect(providers.get(a.id).hasCredential).toBe(false);
    await expect(service.create({ agentId: agent.id })).rejects.toMatchObject({
      code: 'not_ready',
      details: { reason: 'api_key_missing' },
    });
    agents.update(
      agent.id,
      AgentDraft.parse({
        ...draft,
        config: {
          ...draft.config,
          provider: { kind: 'reference', providerId: b.id, model: 'new' },
        },
      }),
      1,
    );
    actor.failCloseWith = new Error('unconfirmed');
    const closing = service.close(chat.chatId);
    try {
      expect(providers.get(a.id).runtimeCount).toBe(1);
      expect(() => providers.delete(a.id, 2)).toThrow();
    } finally {
      resume.resolve();
    }
    await sending;
    expect((await closing).status).toBe('close_incomplete');
    expect(
      actor.opened.every(
        (item) => (item as unknown as AgentPreviewTarget).agent.settings.config.providers[0]?.apiKey === keyA,
      ),
    ).toBe(true);
    expect(providers.get(a.id).runtimeCount).toBe(1);
    expect(() => providers.delete(a.id, 2)).toThrow();
    actor.failCloseWith = null;
    expect((await service.close(chat.chatId)).status).toBe('closed');
    expect(providers.get(a.id).runtimeCount).toBe(0);
    providers.delete(a.id, 2);
    await service.shutdown();
  });

  test('local key works without env but cannot bypass missing Store/WASM infrastructure', async () => {
    const { env, service, agent, providers, agents, chats, engine } = setup();
    expect(
      probeChatRuntime({ ...env, storeToken: 'synthetic', rivetStoragePath: '/unused' }).reasons.includes(
        'provider_key_missing',
      ),
    ).toBe(false);
    expect(probeChatRuntime({ ...env, storeToken: 'synthetic', rivetStoragePath: '/unused' }, false)).toEqual(
      { mode: 'not_configured', reasons: ['wasm_assets_missing'] },
    );
    const chat = await service.create({ agentId: agent.id });
    expect(firstTarget(engine, chat.chatId).agent.settings.config.providers).toHaveLength(1);
    await service.close(chat.chatId);
    const blocked = new ChatService({
      env,
      providers,
      agents,
      chats,
      engine: new NotConfiguredChatEngine({
        mode: 'not_configured',
        reasons: ['store_token_missing', 'wasm_assets_missing'],
      }),
    });
    await expect(blocked.create({ agentId: agent.id })).rejects.toMatchObject({
      code: 'engine_unavailable',
      details: { reasons: ['store_token_missing', 'wasm_assets_missing'] },
    });
  });
});
