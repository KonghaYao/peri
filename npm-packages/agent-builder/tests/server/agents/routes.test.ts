import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import { type Agent, AgentListResponse, AgentResponse } from '@shared/agent';
import { ApiErrorBody, HealthResponse } from '@shared/api';
import type { Hono } from 'hono';
import { createApp } from '../../../server/app';
import { openDatabase } from '../../../server/db/database';
import { migrate } from '../../../server/db/migrate';
import { testEnv } from '../helpers';

let db: Database | null = null;

afterEach(() => {
  db?.close();
  db = null;
});

function setup(): { database: Database; app: Hono } {
  const database = openDatabase(':memory:');
  migrate(database);
  db = database;
  const app = createApp({
    env: testEnv(),
    db: database,
  });
  return { database, app };
}

const validDraft = {
  name: 'Reviewer',
  description: 'Checks changes',
  config: {
    instructions: 'Be concise',
    provider: { id: 'anthropic-main', type: 'anthropic', model: 'claude-sonnet-4-5' },
    metaHarness: { '01_intro': true },
    mcpServers: {
      docs: {
        type: 'http',
        url: 'https://example.com/mcp',
        headers: { Authorization: 'Bearer super-secret-value' },
      },
    },
  },
} as const;

async function createAgent(app: Hono): Promise<Agent> {
  const response = await app.request('/api/agents', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(validDraft),
  });
  expect(response.status).toBe(201);
  return AgentResponse.parse(await response.json()).agent;
}

describe('GET /api/health', () => {
  test('reports the engine as not configured without touching the database', async () => {
    const { app } = setup();
    const response = await app.request('/api/health');

    expect(response.status).toBe(200);
    const body = HealthResponse.parse(await response.json());
    expect(body.ok).toBe(true);
    expect(body.engine).toEqual({
      mode: 'not_configured',
      status: 'unknown',
      reasons: ['store_token_missing', 'rivet_storage_path_missing'],
    });
    expect(body.runtime.name).toBe('bun');
  });
});

describe('GET /api/agents', () => {
  test('returns an empty list initially', async () => {
    const { app } = setup();
    const response = await app.request('/api/agents');

    expect(response.status).toBe(200);
    expect(AgentListResponse.parse(await response.json())).toEqual({ agents: [] });
  });
});

describe('agent CRUD routes', () => {
  test('creates, reads, updates, lists and deletes one agent', async () => {
    const { app } = setup();
    const created = await createAgent(app);

    expect(created.id).toMatch(/^[0-9a-f-]{36}$/);

    const listResponse = await app.request('/api/agents');
    const list = AgentListResponse.parse(await listResponse.json());
    expect(list.agents).toHaveLength(1);
    expect(list.agents[0]?.id).toBe(created.id);
    expect(list.agents[0]).not.toHaveProperty('config');

    const getResponse = await app.request(`/api/agents/${created.id}`);
    expect(getResponse.status).toBe(200);
    expect(AgentResponse.parse(await getResponse.json()).agent.config.mcpServers).toEqual(
      validDraft.config.mcpServers,
    );

    const replacement = {
      ...validDraft,
      name: 'Reviewer v2',
      config: { ...validDraft.config, instructions: 'New instructions' },
    };
    const putResponse = await app.request(`/api/agents/${created.id}`, {
      method: 'PUT',
      headers: { 'content-type': 'application/json', 'If-Match': String(created.revision) },
      body: JSON.stringify(replacement),
    });
    expect(putResponse.status).toBe(200);
    const updated = AgentResponse.parse(await putResponse.json()).agent;
    expect(updated.name).toBe('Reviewer v2');
    expect(updated.config.instructions).toBe('New instructions');
    expect(updated.createdAt).toBe(created.createdAt);

    const deleteResponse = await app.request(`/api/agents/${created.id}`, {
      method: 'DELETE',
      headers: { 'If-Match': String(updated.revision) },
    });
    expect(deleteResponse.status).toBe(204);
    expect(await deleteResponse.text()).toBe('');

    const missingResponse = await app.request(`/api/agents/${created.id}`);
    expect(missingResponse.status).toBe(404);
    expect(ApiErrorBody.parse(await missingResponse.json()).error.code).toBe('not_found');
  });

  test('returns the shared error envelope for unknown routes', async () => {
    const { app } = setup();
    const response = await app.request('/api/missing');

    expect(response.status).toBe(404);
    const body = ApiErrorBody.parse(await response.json());
    expect(body.error.code).toBe('not_found');
    expect(body.error.details).toEqual({});
    expect(body.error.retryable).toBe(false);
    expect(JSON.stringify(body)).not.toContain('stack');
  });
});

describe('agent route validation', () => {
  test('rejects malformed JSON', async () => {
    const { app } = setup();
    const response = await app.request('/api/agents', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: '{',
    });

    expect(response.status).toBe(400);
    expect(ApiErrorBody.parse(await response.json()).error.code).toBe('invalid_input');
  });

  test('rejects unknown keys', async () => {
    const { app } = setup();
    const response = await app.request('/api/agents', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ ...validDraft, allowedTools: ['read'] }),
    });

    expect(response.status).toBe(400);
    expect(ApiErrorBody.parse(await response.json()).error.code).toBe('invalid_input');
  });

  test('rejects ACP MCP transports with a dedicated code', async () => {
    const { app } = setup();
    const response = await app.request('/api/agents', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        ...validDraft,
        config: { mcpServers: { live: { type: 'acp' } } },
      }),
    });

    expect(response.status).toBe(400);
    const body = ApiErrorBody.parse(await response.json());
    expect(body.error.code).toBe('unsupported_mcp_type');
    expect(body.error.details).toEqual({});
  });

  test('rejects non-http(s) MCP URLs and blank stdio commands', async () => {
    const { app } = setup();
    const servers = [
      { type: 'http', url: 'file:///etc/passwd' },
      { type: 'http', url: 'ftp://example.com/mcp' },
      { type: 'stdio', command: '   ' },
    ];

    for (const server of servers) {
      const response = await app.request('/api/agents', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ ...validDraft, config: { mcpServers: { custom: server } } }),
      });

      expect(response.status).toBe(400);
      expect(ApiErrorBody.parse(await response.json()).error.code).toBe('invalid_input');
    }
  });

  test('accepts http/https MCP URLs and normal stdio commands', async () => {
    const { app } = setup();
    const servers = [
      { type: 'http', url: 'https://example.com/mcp' },
      { type: 'http', url: 'http://127.0.0.1:3000/mcp' },
      { type: 'stdio', command: 'node' },
    ];

    for (const server of servers) {
      const response = await app.request('/api/agents', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ ...validDraft, config: { mcpServers: { custom: server } } }),
      });

      expect(response.status).toBe(201);
    }
  });

  test('does not echo secret MCP headers in validation errors', async () => {
    const { app } = setup();
    const response = await app.request('/api/agents', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ ...validDraft, name: '   ' }),
    });

    expect(response.status).toBe(400);
    const raw = await response.text();
    expect(raw).not.toContain('super-secret-value');
    expect(raw).not.toContain('stack');
    expect(ApiErrorBody.parse(JSON.parse(raw)).error.code).toBe('invalid_input');
  });

  test('returns 404 for missing agents on get, update and delete', async () => {
    const { app } = setup();
    const unknown = '00000000-0000-4000-8000-000000000000';

    for (const [method, path] of [
      ['GET', `/api/agents/${unknown}`],
      ['PUT', `/api/agents/${unknown}`],
      ['DELETE', `/api/agents/${unknown}`],
    ] as const) {
      const response = await app.request(path, {
        method,
        headers: { 'content-type': 'application/json', 'If-Match': '1' },
        ...(method === 'PUT' ? { body: JSON.stringify(validDraft) } : {}),
      });
      expect(response.status).toBe(404);
      expect(ApiErrorBody.parse(await response.json()).error.code).toBe('not_found');
    }
  });

  test('refuses to delete an agent that still has chats', async () => {
    const { app, database } = setup();
    const created = await createAgent(app);
    database
      .query('INSERT INTO chats (chat_id, agent_id, created_at, updated_at) VALUES (?, ?, ?, ?)')
      .run('chat-1', created.id, 1, 1);

    const response = await app.request(`/api/agents/${created.id}`, {
      method: 'DELETE',
      headers: { 'If-Match': String(created.revision) },
    });
    expect(response.status).toBe(409);
    const body = ApiErrorBody.parse(await response.json());
    expect(body.error.code).toBe('agent_has_chats');
    expect(body.error.retryable).toBe(false);

    const stillThere = await app.request(`/api/agents/${created.id}`);
    expect(stillThere.status).toBe(200);
  });
});
