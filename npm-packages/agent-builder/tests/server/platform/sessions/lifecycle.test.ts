import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import { mkdtempSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { openDatabase } from '../../../../server/db/database';
import { migrate, SESSION_INLINE_AGENT_ID } from '../../../../server/db/migrate';
import { createPlatformRepositories } from '../../../../server/platform/repositories';
import { SessionRecorder } from '../../../../server/platform/sessions/recorder';
import { testEnv } from '../../helpers';
import {
  AUTHORIZATION,
  createSession,
  del,
  get,
  type Harness,
  json,
  post,
  sessionEvents,
  setup,
  tick,
} from './helpers';

let harness: Harness | null = null;
let db: Database | null = null;
const tempDirs: string[] = [];

afterEach(async () => {
  await harness?.dispose();
  harness = null;
  db?.close();
  db = null;
  for (const dir of tempDirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

function tempDbPath(): string {
  const dir = realpathSync(mkdtempSync(path.join(tmpdir(), 'ab-session-restart-')));
  tempDirs.push(dir);
  return path.join(dir, 'test.sqlite');
}

describe('session resources', () => {
  test('creates a session from a saved agent and reports the documented shape', async () => {
    const h = setup();
    harness = h;
    const { body } = await createSession(h, {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'hello',
      metadata: { tenant: 'alpha' },
    });

    expect(body.object).toBe('agent.session');
    expect(body.id).toBeString();
    expect(body.status).toBeOneOf(['idle', 'in_progress']);
    expect(body.created_at).toBeNumber();
    expect(Math.abs(Number(body.created_at) - Math.floor(Date.now() / 1000))).toBeLessThan(5);
    expect(body.environment).toEqual({ type: 'none' });
    expect(body.metadata).toEqual({ tenant: 'alpha' });
    expect(body.vault_ids).toEqual([]);
    expect(body.usage).toBeNull();
    expect(body.required_actions).toEqual([]);
    expect(body.error).toBeNull();
    expect(body.agent).toMatchObject({
      id: h.agentId,
      name: 'Session Agent',
      model: 'claude-sonnet-4-5',
      instructions: 'Be brief',
    });
  });

  test('resolves an inline agent without a saved agent row', async () => {
    const h = setup();
    harness = h;
    const { id, body } = await createSession(h, {
      environment: { type: 'none' },
      agent: { model: 'claude-sonnet-4-5', instructions: 'inline text' },
      input: 'hi',
    });

    expect((body.agent as { instructions?: string }).instructions).toBe('inline text');
    expect((body.agent as { model?: string }).model).toBe('claude-sonnet-4-5');

    const row = h.db
      .query<{ agent_id: string }, [string]>('SELECT agent_id FROM chats WHERE chat_id = ?')
      .get(id);
    expect(row?.agent_id).toBe(SESSION_INLINE_AGENT_ID);

    // The durable snapshot keeps the draft so a restarted process freezes the same target.
    const stored = h.repos.sessions.get(id)?.agent_config ?? '';
    const parsed = JSON.parse(stored) as { draft?: { config?: { instructions?: string } } };
    expect(parsed.draft?.config?.instructions).toBe('inline text');
  });

  test('freezes extended MCP fields from a saved agent into the session draft and target', async () => {
    const h = setup();
    harness = h;
    const secret = 'session-mcp-secret-8c1d';
    const created = await post(h, '/agents', {
      name: 'MCP agent',
      model: 'primary/claude-sonnet-4-5',
      tools: [
        {
          type: 'mcp',
          server_label: 'remote',
          transport: {
            type: 'http',
            server_url: 'http://127.0.0.1:9999/mcp',
            authorization: `Bearer ${secret}`,
          },
        },
        {
          type: 'mcp',
          server_label: 'local',
          transport: {
            type: 'stdio',
            command: 'node',
            args: ['server.js'],
            cwd: '/tmp/tools',
            env_vars: ['HOME'],
          },
        },
      ],
    });
    expect(created.status).toBe(201);
    const agent = await json<{ id: string }>(created);

    const session = await createSession(h, {
      environment: { type: 'none' },
      agent_id: agent.id,
      input: 'hello',
    });

    const expected = {
      remote: {
        type: 'http',
        url: 'http://127.0.0.1:9999/mcp',
        authorization: `Bearer ${secret}`,
      },
      local: {
        type: 'stdio',
        command: 'node',
        args: ['server.js'],
        cwd: '/tmp/tools',
        env_vars: ['HOME'],
      },
    };
    const snapshot = JSON.parse(h.repos.sessions.get(session.id)?.agent_config ?? '{}') as {
      draft?: { config?: { mcpServers?: unknown } };
    };
    expect(snapshot.draft?.config?.mcpServers).toEqual(expected);

    const target = h.actor(session.id).openTargets[0] as { session?: { mcpServers?: unknown } } | undefined;
    expect(target?.session?.mcpServers).toEqual(expected);
  });

  test('does not acquire a sandbox for environment none and does for self_hosted', async () => {
    const h = setup({ sandbox: true });
    harness = h;
    const none = await createSession(h, {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'hello',
    });
    expect(h.sandbox?.acquired).toEqual([]);

    const hosted = await createSession(h, {
      environment: { type: 'self_hosted', workspace_directory: '/workspace' },
      agent_id: h.agentId,
      input: 'hello',
    });
    expect(h.sandbox?.acquired).toEqual([hosted.id]);

    const deleted = await del(h, `/agents/sessions/${hosted.id}`);
    expect(deleted.status).toBe(200);
    expect(h.sandbox?.released).toContain(hosted.id);
    void none;
  });

  test('rejects hosted and undocumented environment types instead of mapping them', async () => {
    const h = setup();
    harness = h;
    const hosted = await post(h, '/agents/sessions', {
      environment: { type: 'openai_hosted' },
      agent_id: h.agentId,
      input: 'hello',
    });
    expect(hosted.status).toBe(501);
    const hostedError = (await hosted.json()) as { error: { type: string; param: string | null } };
    expect(hostedError.error.type).toBe('not_implemented_error');
    expect(hostedError.error.param).toBe('environment.type');

    const unknown = await post(h, '/agents/sessions', {
      environment: { type: 'remote_managed', workspace_directory: '/workspace' },
      agent_id: h.agentId,
      input: 'hello',
    });
    expect(unknown.status).toBe(400);
    const unknownError = (await unknown.json()) as { error: { type: string; param: string | null } };
    expect(unknownError.error.type).toBe('invalid_request_error');
    expect(unknownError.error.param).toBe('environment.type');

    const local = await post(h, '/agents/sessions', {
      environment: { type: 'local', workspace_directory: '/workspace' },
      agent_id: h.agentId,
      input: 'hello',
    });
    expect(local.status).toBe(400);

    const stored = h.db.query<{ count: number }, []>('SELECT COUNT(*) AS count FROM platform_sessions').get();
    expect(stored?.count).toBe(0);
  });

  test('validates vault ids against the vault whitelist', async () => {
    const h = setup();
    harness = h;
    const vault = h.repos.vaults.create({ name: 'vault', metadata: '{}' });

    const accepted = await createSession(h, {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'hello',
      vault_ids: [vault.id],
    });
    expect(accepted.body.vault_ids).toEqual([vault.id]);

    const rejected = await post(h, '/agents/sessions', {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'hello',
      vault_ids: ['missing-vault'],
    });
    expect(rejected.status).toBe(400);
    expect((await json<{ error: { param: string | null } }>(rejected)).error.param).toBe('vault_ids');
  });

  test('requires input when the environment is none', async () => {
    const h = setup();
    harness = h;
    const response = await post(h, '/agents/sessions', {
      environment: { type: 'none' },
      agent_id: h.agentId,
    });
    expect(response.status).toBe(400);
    expect((await json<{ error: { param: string | null } }>(response)).error.param).toBe('input');
  });

  test('lists sessions with agent and metadata filters and keyset pagination', async () => {
    const h = setup();
    harness = h;
    await createSession(h, {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'one',
      metadata: { tenant: 'alpha' },
    });
    await createSession(h, {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'two',
      metadata: { tenant: 'trial' },
    });

    const all = await json<{ object: string; data: unknown[]; has_more: boolean }>(
      await get(h, '/agents/sessions'),
    );
    expect(all.object).toBe('list');
    expect(all.data).toHaveLength(2);

    const filtered = await json<{ data: { metadata: Record<string, string> }[] }>(
      await get(h, '/agents/sessions?metadata[tenant]=alpha'),
    );
    expect(filtered.data).toHaveLength(1);
    expect(filtered.data[0]?.metadata).toEqual({ tenant: 'alpha' });

    const page = await json<{ data: { id: string }[]; has_more: boolean; last_id: string | null }>(
      await get(h, '/agents/sessions?limit=1'),
    );
    expect(page.data).toHaveLength(1);
    expect(page.has_more).toBe(true);

    const next = await json<{ data: { id: string }[] }>(
      await get(h, `/agents/sessions?limit=1&after=${page.last_id}`),
    );
    expect(next.data).toHaveLength(1);
    expect(next.data[0]?.id).not.toBe(page.data[0]?.id);

    const byAgent = await json<{ data: unknown[] }>(await get(h, `/agents/sessions?agent_id=${h.agentId}`));
    expect(byAgent.data).toHaveLength(2);

    const invalid = await get(h, '/agents/sessions?limit=0');
    expect(invalid.status).toBe(400);
  });

  test('updates metadata and agent settings and rejects a running delete', async () => {
    const h = setup();
    harness = h;
    const { id } = await createSession(h, {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'hello',
    });

    const updated = await json<{ metadata: Record<string, string>; agent: { model?: string } }>(
      await post(h, `/agents/sessions/${id}`, {
        metadata: { stage: 'two' },
        agent: { model: 'claude-sonnet-4-5-new' },
      }),
    );
    expect(updated.metadata).toEqual({ stage: 'two' });
    expect(updated.agent.model).toBe('claude-sonnet-4-5-new');
    expect(h.actor(id).closed).toBe(true);

    // The update closed the frozen run; the next input reopens the projection.
    const submitted = await post(h, `/agents/sessions/${id}/events`, {
      events: [
        {
          type: 'agent.session.input.message',
          input: [{ role: 'user', content: [{ type: 'input_text', text: 'again' }] }],
        },
      ],
    });
    expect(submitted.status).toBe(202);

    const actor = h.actor(id);
    actor.runState(true);
    actor.input('in-1', 'hello');
    await tick();
    const running = await json<{ status: string }>(await get(h, `/agents/sessions/${id}`));
    expect(running.status).toBe('in_progress');
    const conflict = await del(h, `/agents/sessions/${id}`);
    expect(conflict.status).toBe(409);

    actor.turnComplete('completed');
    actor.runState(false);
    await tick();
    const removed = await json<{ id: string; object: string; deleted: boolean }>(
      await del(h, `/agents/sessions/${id}`),
    );
    expect(removed).toEqual({ id, object: 'agent.session.deleted', deleted: true });
    expect((await get(h, `/agents/sessions/${id}`)).status).toBe(404);
    expect(h.repos.events.listBySession(id)).toEqual([]);
  });

  test('returns canonical errors for unknown sessions', async () => {
    const h = setup();
    harness = h;
    const missing = await get(h, '/agents/sessions/not-a-session');
    expect(missing.status).toBe(404);
    expect((await json<{ error: { type: string } }>(missing)).error.type).toBe('not_found_error');

    const badBody = await post(h, '/agents/sessions', { environment: { type: 'none' } });
    expect(badBody.status).toBe(400);
  });
});

describe('application wiring', () => {
  test('serves sessions through the real app once the runtime is installed', async () => {
    const { createApp } = await import('../../../../server/app');
    const h = setup();
    harness = h;
    const app = createApp({ env: testEnv(), db: h.db, engine: h.engine, chatService: h.chats });
    const response = await app.request('/v1/agents/sessions', {
      method: 'POST',
      headers: { ...AUTHORIZATION, 'content-type': 'application/json' },
      body: JSON.stringify({ environment: { type: 'none' }, agent_id: h.agentId, input: 'hello' }),
    });
    expect(response.status).toBe(201);
    const resource = await json<{ id: string }>(response);
    const fetched = await app.request(`/v1/agents/sessions/${resource.id}`, { headers: AUTHORIZATION });
    expect(fetched.status).toBe(200);
    expect((await json<{ id: string }>(fetched)).id).toBe(resource.id);
  });
});

describe('startup convergence', () => {
  test('sweeps non-terminal turns and items after reopening the database', () => {
    const databasePath = tempDbPath();
    const first = openDatabase(databasePath);
    migrate(first);
    const repos = createPlatformRepositories(first);
    const session = repos.sessions.create({
      id: 'session-restart',
      agentId: SESSION_INLINE_AGENT_ID,
      agentConfig: '{"agent":{},"draft":{}}',
      environmentType: 'none',
      status: 'in_progress',
    });
    const turn = repos.turns.create({
      id: 'turn-restart',
      sessionId: session.id,
      agentId: SESSION_INLINE_AGENT_ID,
      runtimeTurnId: 'runtime-turn-1',
      bindingRevision: 1,
      status: 'in_progress',
      startedAt: Date.now(),
    });
    const item = repos.items.create({
      id: 'item-restart',
      sessionId: session.id,
      turnId: turn.id,
      outputIndex: 0,
      type: 'message',
      status: 'in_progress',
      payload: JSON.stringify({ type: 'message', role: 'assistant', content: [] }),
    });
    first.close();

    const reopened = openDatabase(databasePath);
    db = reopened;
    migrate(reopened);
    const next = createPlatformRepositories(reopened);
    const recorder = new SessionRecorder({ repos: next });
    expect(recorder.sweepInterrupted()).toEqual({ turns: 1, items: 1 });

    const sweptTurn = next.turns.get(turn.id);
    expect(sweptTurn?.status).toBe('failed');
    expect(sweptTurn?.error_code).toBe('interrupted');
    expect(sweptTurn?.completed_at).toBeNumber();

    const sweptItem = next.items.get(item.id);
    expect(sweptItem?.status).toBe('incomplete');
    expect(JSON.parse(sweptItem?.payload ?? '{}')).toMatchObject({ status: 'incomplete' });

    expect(next.sessions.get(session.id)).toMatchObject({ status: 'failed', error: 'interrupted' });

    const events = next.events.listBySession(session.id);
    const failed = events.find((event) => event.type === 'agent.session.turn.failed');
    expect(failed).toBeDefined();
    const data = JSON.parse(failed?.data ?? '{}') as {
      turn?: { error?: { code?: string } };
      usage?: unknown;
    };
    expect(data.turn?.error?.code).toBe('interrupted');
    expect(data.usage).toBeNull();
    expect(events.some((event) => event.type === 'agent.session.failed')).toBe(true);

    // The sweep never broadcasts: a fresh recorder has no subscriber and the log stays queryable.
    expect(sessionEvents({ repos: next } as unknown as Harness, session.id).length).toBe(events.length);
  });
});
