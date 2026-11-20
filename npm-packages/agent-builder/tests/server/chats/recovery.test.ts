import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import { createHostResourceOwner } from '@peri-code/rivet/rivet';
import { type Agent, type AgentDraft, AgentDraft as AgentDraftSchema } from '@shared/agent';
import type { ChatRuntimeStatus } from '@shared/chat';
import type { Hono } from 'hono';
import { AgentsRepository } from '../../../server/agents/repository';
import { createApp } from '../../../server/app';
import {
  type ChatEngine,
  type ChatEngineFactoryOptions,
  createChatEngineWithStoreProbe,
  RivetChatEngine,
} from '../../../server/chats/engine';
import { ChatsRepository } from '../../../server/chats/repository';
import { ChatService } from '../../../server/chats/service';
import { openDatabase } from '../../../server/db/database';
import { migrate } from '../../../server/db/migrate';
import type { ServerEnv } from '../../../server/env';
import { createPlatformRepositories } from '../../../server/platform/repositories';
import { SessionRecorder } from '../../../server/platform/sessions/recorder';
import {
  createSessionRuntime,
  installSessionRuntime,
  sessionDraftFor,
} from '../../../server/platform/sessions/runtime';
import type { StartedRegistry } from '../../../server/rivet/registry';
import { SandboxCoordinator } from '../../../server/sandbox/coordinator';
import { sandboxEnginePort } from '../../../server/sandbox/engine-port';
import { testEnv, withStoredProvider } from '../helpers';
import { FakeActor, FakeEngine, FakeSandboxActor } from './helpers';

const AUTHORIZATION = { authorization: 'Bearer test-token' } as const;

interface PlatformErrorBody {
  readonly error: {
    readonly type: string;
    readonly code: string;
    readonly message: string;
    readonly param: string | null;
  };
}

async function errorBody(response: Response): Promise<PlatformErrorBody> {
  return (await response.json()) as PlatformErrorBody;
}

/** Creates a chat through the `/v1` session surface, which is the only surviving chat entry. */
async function createChat(app: Hono, agentId: string): Promise<Response> {
  return app.request('/v1/agents/sessions', {
    method: 'POST',
    headers: { ...AUTHORIZATION, 'content-type': 'application/json' },
    body: JSON.stringify({ environment: { type: 'none' }, agent_id: agentId, input: 'hello' }),
  });
}

const READY_ENV = {
  storeToken: 'store-token',
  rivetStoragePath: '/tmp/agent-builder/.data/rivet',
} as const;

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

let db: Database | null = null;

afterEach(() => {
  db?.close();
  db = null;
});

test('ABV-09 boot Store outage recovers into first Chat + Sandbox with registry started before acquire', async () => {
  const base = testEnv({
    ...READY_ENV,
    storeToken: crypto.randomUUID(),
  });
  const env = {
    ...base,
    sandbox: { ...base.sandbox, enabled: true, mode: 'ready' as const, token: crypto.randomUUID() },
  };
  const owner = createHostResourceOwner();
  const actor = new FakeActor();
  const sandboxActor = new FakeSandboxActor(env.workspaceUri);
  let reachable = false;
  let clock = 0;
  let starts = 0;
  let registryClosed = 0;
  const acquire = sandboxActor.acquire.bind(sandboxActor);
  sandboxActor.acquire = async (...args) => {
    expect(starts).toBe(1);
    return acquire(...args);
  };
  const engine = await createChatEngineWithStoreProbe(env, {
    owner,
    now: () => clock,
    retryAfterMs: 1,
    probe: async () => {
      if (!reachable) throw new Error('Store unavailable');
    },
    createReadyEngine: () =>
      new RivetChatEngine(
        env,
        { mode: 'wasm', reasons: [] },
        'file:///unused.js',
        owner,
        {
          token: env.sandbox.token,
          machineId: env.machineId,
          authorizedWorkspaces: [env.workspaceUri],
          runtime: {
            async start() {},
            async close() {},
            stateOf() {
              return undefined;
            },
            async fetchMcp() {
              return new Response();
            },
          },
        },
        {
          startRegistry: async () => {
            starts += 1;
            return {
              endpoint: 'http://127.0.0.1:8999',
              client: { chat: { getOrCreate: () => actor }, sandbox: { getOrCreate: () => sandboxActor } },
              async shutdown() {
                registryClosed += 1;
              },
            } as unknown as StartedRegistry;
          },
        },
      ),
  });
  const database = openDatabase(':memory:');
  migrate(database);
  const agents = new AgentsRepository(database);
  const agent = agents.create(withStoredProvider(database, providerDraft));
  const repos = createPlatformRepositories(database);
  const recorder = new SessionRecorder({ repos });
  const sandbox = new SandboxCoordinator(env, sandboxEnginePort(engine));
  const service = new ChatService({
    env,
    engine,
    agents,
    chats: new ChatsRepository(database),
    sandbox,
    draftFor: sessionDraftFor(repos),
  });
  const restore = installSessionRuntime(createSessionRuntime({ repos, chats: service, recorder }));
  const app = createApp({ env, db: database, engine, chatService: service });
  const create = () =>
    app.request('/v1/agents/sessions', {
      method: 'POST',
      headers: { ...AUTHORIZATION, 'content-type': 'application/json' },
      body: JSON.stringify({
        environment: { type: 'self_hosted', workspace_directory: env.workspacePath },
        agent_id: agent.id,
        input: 'hello',
      }),
    });
  try {
    expect((await create()).status).toBe(503);
    expect(starts).toBe(0);
    reachable = true;
    clock = 2;
    const response = await create();
    expect(response.status).toBe(201);
    const session = (await response.json()) as { id: string };
    expect(session.id).toBeString();
    expect(sandboxActor.acquireCalls).toHaveLength(1);
    expect(actor.opened.length).toBeGreaterThan(0);
    expect(actor.opened.every((item) => JSON.stringify(item) === JSON.stringify(actor.opened[0]))).toBe(true);
    await service.remove(session.id);
  } finally {
    restore();
    owner.beginShutdown();
    expect((await owner.closeAll({ timeoutMs: 100 })).complete).toBe(true);
    await engine.shutdown();
    database.close();
  }
  expect(registryClosed).toBe(1);
});

interface Harness {
  readonly app: Hono;
  readonly agent: Agent;
  readonly engine: ChatEngine;
  readonly chats: ChatService;
  readonly probes: () => number;
  readonly readyEngineCreations: () => number;
  readonly clock: { value: number };
  readonly reachable: { value: boolean };
  readonly failure: { value: unknown };
  readonly dispose: () => void;
}

async function setup(
  options: {
    readonly probeWait?: Promise<void>;
    readonly createReadyEngine?: () => ChatEngine;
    readonly startupReachable?: boolean;
    readonly probeError?: unknown;
  } = {},
): Promise<Harness> {
  const env: ServerEnv = testEnv(READY_ENV);
  const database = openDatabase(':memory:');
  migrate(database);
  db = database;
  const agents = new AgentsRepository(database);
  const agent = agents.create(withStoredProvider(database, providerDraft));

  const clock = { value: 0 };
  const reachable = { value: options.startupReachable ?? false };
  const failure = { value: options.probeError ?? new Error('Store unreachable') };
  let probes = 0;
  let readyEngineCreations = 0;

  const probeOptions: ChatEngineFactoryOptions = {
    probe: async () => {
      probes += 1;
      // Gate probes after the startup one so concurrent requests can pile up on the same probe.
      if (probes > 1 && options.probeWait !== undefined) await options.probeWait;
      // The probe contract is "throw when unreachable"; the failure is classified by status.
      if (!reachable.value) throw failure.value;
    },
    createReadyEngine:
      options.createReadyEngine ??
      (() => {
        readyEngineCreations += 1;
        return new FakeEngine();
      }),
    retryAfterMs: 1_000,
    now: () => clock.value,
  };

  const engine = await createChatEngineWithStoreProbe(env, probeOptions);
  const repos = createPlatformRepositories(database);
  const recorder = new SessionRecorder({ repos });
  const chats = new ChatService({
    env,
    engine,
    chats: new ChatsRepository(database),
    agents,
    draftFor: sessionDraftFor(repos),
  });
  const restore = installSessionRuntime(createSessionRuntime({ repos, chats, recorder }));
  return {
    app: createApp({ env, db: database, engine, chatService: chats }),
    agent,
    engine,
    chats,
    probes: () => probes,
    readyEngineCreations: () => readyEngineCreations,
    clock,
    reachable,
    failure,
    dispose: restore,
  };
}

describe('Store recovery', () => {
  test('returns a retryable store_unreachable 503, then 201 after the TTL without a restart', async () => {
    const harness = await setup();
    // The startup probe ran once; the first request is inside the TTL and must not probe again.
    expect(harness.probes()).toBe(1);

    const refused = await createChat(harness.app, harness.agent.id);
    expect(refused.status).toBe(503);
    expect(refused.headers.has('x-platform-degraded')).toBe(false);
    expect((await errorBody(refused)).error).toMatchObject({
      code: 'engine_unavailable',
      type: 'server_error',
      param: null,
    });
    expect(harness.probes()).toBe(1);
    // The wire error hides reasons; the chat runtime still owns the concrete retry contract.
    await expect(harness.chats.create({ agentId: harness.agent.id })).rejects.toMatchObject({
      code: 'engine_unavailable',
      retryable: true,
      details: { reasons: ['store_unreachable'] },
    });

    harness.reachable.value = true;
    harness.clock.value = 1_001;

    const created = await createChat(harness.app, harness.agent.id);
    expect(created.status).toBe(201);
    const session = (await created.json()) as { agent: { id: string } };
    expect(session.agent.id).toBe(harness.agent.id);
    expect(harness.probes()).toBe(2);
    expect(harness.readyEngineCreations()).toBe(1);
    expect(harness.engine.status).toEqual<ChatRuntimeStatus>({ mode: 'wasm', reasons: [] });
    harness.dispose();
  });

  test('degrades a healthy boot on the first failed re-probe and recovers without a restart', async () => {
    const harness = await setup({ startupReachable: true });
    expect(harness.engine.status).toEqual<ChatRuntimeStatus>({ mode: 'wasm', reasons: [] });
    expect(harness.engine.health()).toMatchObject({ mode: 'embedded', status: 'ready' });
    expect(harness.probes()).toBe(1);
    expect(harness.readyEngineCreations()).toBe(1);

    harness.reachable.value = false;
    harness.clock.value = 1_001;
    const refused = await createChat(harness.app, harness.agent.id);
    expect(refused.status).toBe(503);
    expect((await errorBody(refused)).error.code).toBe('engine_unavailable');
    expect(harness.probes()).toBe(2);
    expect(harness.engine.status).toEqual<ChatRuntimeStatus>({
      mode: 'not_configured',
      reasons: ['store_unreachable'],
    });
    expect(harness.engine.health()).toMatchObject({
      mode: 'not_configured',
      status: 'unknown',
      reasons: ['store_unreachable'],
    });

    harness.reachable.value = true;
    harness.clock.value = 2_002;
    expect((await createChat(harness.app, harness.agent.id)).status).toBe(201);
    expect(harness.probes()).toBe(3);
    // The engine built for the healthy boot is reused after recovery, not rebuilt.
    expect(harness.readyEngineCreations()).toBe(1);
    expect(harness.engine.health()).toMatchObject({ mode: 'embedded', status: 'ready' });
    harness.dispose();
  });

  test('shares one probe between concurrent creates after the TTL', async () => {
    const gateControl: { release: (() => void) | null } = { release: null };
    const gate = new Promise<void>((resolve) => {
      gateControl.release = resolve;
    });
    const harness = await setup({ probeWait: gate });
    harness.reachable.value = true;
    harness.clock.value = 1_001;

    const pending = Promise.all(Array.from({ length: 3 }, () => createChat(harness.app, harness.agent.id)));
    // Let all three handlers enter `ensureReady` before the single probe resolves.
    await new Promise((resolve) => setTimeout(resolve, 0));
    if (gateControl.release === null) throw new Error('probe did not start');
    gateControl.release();

    const responses = await pending;
    expect(responses.map((response) => response.status)).toEqual([201, 201, 201]);
    expect(harness.probes()).toBe(2);
    expect(harness.readyEngineCreations()).toBe(1);
    harness.dispose();
  });

  test('degrades again when the Store stops answering after a recovery', async () => {
    const harness = await setup();
    harness.reachable.value = true;
    harness.clock.value = 1_001;
    expect((await createChat(harness.app, harness.agent.id)).status).toBe(201);

    harness.reachable.value = false;
    harness.clock.value = 2_002;
    const degraded = await createChat(harness.app, harness.agent.id);
    expect(degraded.status).toBe(503);
    expect((await errorBody(degraded)).error.code).toBe('engine_unavailable');
    expect(harness.engine.status.mode).toBe('not_configured');

    harness.reachable.value = true;
    harness.clock.value = 3_003;
    expect((await createChat(harness.app, harness.agent.id)).status).toBe(201);
    // The engine from the first recovery is reused instead of being rebuilt.
    expect(harness.readyEngineCreations()).toBe(1);
    harness.dispose();
  });

  test('ensureReady is a recovery trigger and reflects the probe result', async () => {
    const harness = await setup();
    expect(harness.engine.status).toEqual<ChatRuntimeStatus>({
      mode: 'not_configured',
      reasons: ['store_unreachable'],
    });

    harness.reachable.value = true;
    harness.clock.value = 1_001;
    await harness.engine.ensureReady?.();
    expect(harness.engine.status).toEqual<ChatRuntimeStatus>({ mode: 'wasm', reasons: [] });
    expect(harness.engine.health()).toMatchObject({ mode: 'embedded', status: 'ready' });
    expect(harness.probes()).toBe(2);
    harness.dispose();
  });

  test('recovers the SSE route without a restart', async () => {
    const harness = await setup();
    harness.reachable.value = true;
    harness.clock.value = 1_001;
    const created = await createChat(harness.app, harness.agent.id);
    expect(created.status).toBe(201);
    const sessionId = ((await created.json()) as { id: string }).id;

    harness.reachable.value = false;
    harness.clock.value = 2_002;
    const refused = await harness.app.request(`/v1/agents/sessions/${sessionId}/events`, {
      headers: AUTHORIZATION,
    });
    expect(refused.status).toBe(503);
    expect((await errorBody(refused)).error).toMatchObject({ code: 'engine_unavailable' });

    harness.reachable.value = true;
    harness.clock.value = 3_003;
    const stream = await harness.app.request(`/v1/agents/sessions/${sessionId}/events`, {
      headers: AUTHORIZATION,
    });
    expect(stream.status).toBe(200);
    const reader = stream.body?.getReader();
    expect(reader).toBeDefined();
    await reader?.read();
    await reader?.cancel();
    harness.dispose();
  });

  for (const status of [401, 403]) {
    test(`reports HTTP ${status} as non-retryable store_auth_failed without TTL re-probing`, async () => {
      const harness = await setup({
        probeError: Object.assign(new Error(`HTTP error! status: ${status}`), { status }),
      });
      expect(harness.engine.status).toEqual({
        mode: 'not_configured',
        reasons: ['store_auth_failed'],
      });

      const refused = await createChat(harness.app, harness.agent.id);
      expect(refused.status).toBe(503);
      expect((await errorBody(refused)).error).toMatchObject({
        code: 'engine_unavailable',
        type: 'server_error',
      });
      await expect(harness.chats.create({ agentId: harness.agent.id })).rejects.toMatchObject({
        code: 'engine_unavailable',
        retryable: false,
        details: { reasons: ['store_auth_failed'] },
      });

      // Credentials cannot change while the process runs: no probe even after the TTL.
      harness.clock.value = 10_000;
      const afterTtl = await createChat(harness.app, harness.agent.id);
      expect(afterTtl.status).toBe(503);
      await expect(harness.chats.create({ agentId: harness.agent.id })).rejects.toMatchObject({
        retryable: false,
      });
      expect(harness.probes()).toBe(1);
      expect(harness.readyEngineCreations()).toBe(0);
      harness.dispose();
    });
  }

  test('reports HTTP 404 as non-retryable store_database_missing without TTL re-probing', async () => {
    const harness = await setup({
      probeError: Object.assign(new Error('HTTP error! status: 404'), { status: 404 }),
    });
    expect(harness.engine.status).toEqual({
      mode: 'not_configured',
      reasons: ['store_database_missing'],
    });

    const refused = await createChat(harness.app, harness.agent.id);
    expect(refused.status).toBe(503);
    expect((await errorBody(refused)).error.code).toBe('engine_unavailable');
    await expect(harness.chats.create({ agentId: harness.agent.id })).rejects.toMatchObject({
      code: 'engine_unavailable',
      retryable: false,
      details: { reasons: ['store_database_missing'] },
    });

    harness.clock.value = 10_000;
    expect((await createChat(harness.app, harness.agent.id)).status).toBe(503);
    expect(harness.probes()).toBe(1);
    harness.dispose();
  });

  test('keeps a timeout retryable and re-probes after the TTL', async () => {
    const harness = await setup({
      probeError: Object.assign(new Error('Query timed out'), { name: 'TimeoutError' }),
    });
    expect(harness.engine.status).toEqual({
      mode: 'not_configured',
      reasons: ['store_unreachable'],
    });
    await expect(harness.chats.create({ agentId: harness.agent.id })).rejects.toMatchObject({
      code: 'engine_unavailable',
      retryable: true,
      details: { reasons: ['store_unreachable'] },
    });

    harness.reachable.value = true;
    harness.clock.value = 1_001;
    expect((await createChat(harness.app, harness.agent.id)).status).toBe(201);
    expect(harness.probes()).toBe(2);
    harness.dispose();
  });

  test('ensureReady probes the Store on the TTL, reports the outage and recovers', async () => {
    const harness = await setup({ startupReachable: true });
    expect(harness.engine.health()).toMatchObject({ mode: 'embedded', status: 'ready' });
    // Within the TTL readiness must not add another round trip.
    expect(harness.probes()).toBe(1);

    harness.reachable.value = false;
    harness.clock.value = 1_001;
    await harness.engine.ensureReady?.();
    expect(harness.engine.health()).toMatchObject({
      mode: 'not_configured',
      status: 'unknown',
      reasons: ['store_unreachable'],
    });
    expect(harness.probes()).toBe(2);

    // A second readiness call inside the TTL shares the last result instead of probing again.
    await harness.engine.ensureReady?.();
    expect(harness.probes()).toBe(2);

    harness.reachable.value = true;
    harness.clock.value = 2_002;
    await harness.engine.ensureReady?.();
    expect(harness.engine.health()).toMatchObject({ mode: 'embedded', status: 'ready' });
    expect(harness.probes()).toBe(3);
    expect(harness.readyEngineCreations()).toBe(1);
    harness.dispose();
  });

  test('ensureReady reports store_auth_failed after a healthy boot and stops probing', async () => {
    const harness = await setup({ startupReachable: true });
    expect(harness.engine.health()).toMatchObject({ status: 'ready' });

    harness.failure.value = Object.assign(new Error('HTTP error! status: 401'), { status: 401 });
    harness.reachable.value = false;
    harness.clock.value = 1_001;
    await harness.engine.ensureReady?.();
    expect(harness.engine.health()).toMatchObject({
      mode: 'not_configured',
      status: 'unknown',
      reasons: ['store_auth_failed'],
    });
    expect(harness.probes()).toBe(2);

    harness.clock.value = 10_000;
    await harness.engine.ensureReady?.();
    expect(harness.engine.health()).toMatchObject({ reasons: ['store_auth_failed'] });
    expect(harness.probes()).toBe(2);
    harness.dispose();
  });

  test('does not probe for configuration that cannot change at runtime', async () => {
    const env: ServerEnv = testEnv({ rivetStoragePath: '/tmp/rivet' });
    let probes = 0;
    const engine = await createChatEngineWithStoreProbe(env, {
      probe: async () => {
        probes += 1;
      },
    });

    await engine.ensureReady?.();
    expect(probes).toBe(0);
    expect(engine.status.reasons).toContain('store_token_missing');
  });
});
