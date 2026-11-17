import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import { mkdtempSync, readFileSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { AgentListResponse, AgentPreviewResponse, AgentResponse } from '@shared/agent';
import { ApiErrorBody } from '@shared/api';
import { ChatListResponse } from '@shared/chat';
import { ProviderResponse } from '@shared/provider';
import type { Hono } from 'hono';
import { createApp } from '../../server/app';
import { openDatabase } from '../../server/db/database';
import { migrate } from '../../server/db/migrate';
import { ProvidersRepository } from '../../server/providers/repository';
import { testEnv } from './helpers';

const README = readFileSync(new URL('../../README.md', import.meta.url), 'utf8');
const CONTRACT = /<!-- api-contract:([a-z-]+) -->\n```json\n([\s\S]*?)\n```/g;
const MARKERS = [
  'agent-todo',
  'agent-reviewer',
  'provider-create',
  'provider-update',
  'chat-create',
] as const;
type Marker = (typeof MARKERS)[number];
type AgentMarker = Extract<Marker, 'agent-todo' | 'agent-reviewer'>;

const PLACEHOLDERS: Record<string, string> = {
  '<ANTHROPIC_PROVIDER_UUID>': '00000000-0000-4000-8000-000000000001',
  '<OPENAI_PROVIDER_UUID>': '00000000-0000-4000-8000-000000000002',
  '<ANTHROPIC_MODEL_ID>': 'test-anthropic-model',
  '<OPENAI_MODEL_ID>': 'test-openai-model',
};

let db: Database | null = null;
let temporaryDirectory: string | null = null;

afterEach(() => {
  db?.close();
  db = null;
  if (temporaryDirectory !== null) {
    rmSync(temporaryDirectory, { recursive: true, force: true });
    temporaryDirectory = null;
  }
});

function contracts(): Map<string, string> {
  const found = new Map<string, string>();
  for (const match of README.matchAll(CONTRACT)) {
    const [, marker, body] = match;
    if (marker === undefined || body === undefined) throw new Error('malformed README contract marker');
    found.set(marker, body);
  }
  expect([...found.keys()].sort()).toEqual([...MARKERS].sort());
  return found;
}

function contractBody(marker: Marker, replacements: Record<string, string> = {}): string {
  const body = contracts().get(marker);
  if (body === undefined) throw new Error(`README is missing the ${marker} contract`);
  return Object.entries({ ...PLACEHOLDERS, ...replacements }).reduce(
    (text, [placeholder, value]) => text.split(placeholder).join(value),
    body,
  );
}

function draft(marker: AgentMarker): unknown {
  return JSON.parse(contractBody(marker));
}

function seedProviders(database: Database): void {
  const repository = new ProvidersRepository(database);
  for (const [placeholder, type] of [
    ['<ANTHROPIC_PROVIDER_UUID>', 'anthropic'],
    ['<OPENAI_PROVIDER_UUID>', 'openai'],
  ] as const) {
    repository.create({
      id: PLACEHOLDERS[placeholder] ?? '',
      name: type,
      type,
      baseUrl: null,
      credentialSource: 'local',
      apiKey: null,
      revision: 1,
    });
  }
}

function setup(): Hono {
  const database = openDatabase(':memory:');
  migrate(database);
  seedProviders(database);
  db = database;
  return createApp({ env: testEnv(), db: database });
}

async function post(app: Hono, url: string, body: unknown): Promise<Response> {
  return app.request(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
}

async function createAgent(app: Hono, marker: AgentMarker) {
  const response = await post(app, '/api/agents', draft(marker));
  expect(response.status).toBe(201);
  return AgentResponse.parse(await response.json()).agent;
}

describe('README API contract', () => {
  test('documented SQLite Provider requests require If-Match and reject legacy env source safely', async () => {
    const app = setup();
    const created = await post(app, '/api/providers', JSON.parse(contractBody('provider-create')));
    expect(created.status).toBe(201);
    const { provider } = ProviderResponse.parse(await created.json());
    expect(provider.credentialSource).toBe('local');
    expect(provider.hasCredential).toBe(false);
    const body = contractBody('provider-update');
    const put = (headers: Record<string, string>) =>
      app.request(`/api/providers/${provider.id}`, {
        method: 'PUT',
        headers: { 'content-type': 'application/json', ...headers },
        body,
      });
    expect((await put({})).status).toBe(409);
    expect((await put({ 'If-Match': '"1"' })).status).toBe(200);
    expect((await put({ 'If-Match': '"1"' })).status).toBe(409);
    const secret = crypto.randomUUID();
    const invalid = await post(app, '/api/providers', {
      ...JSON.parse(contractBody('provider-create')),
      credentialSource: 'env',
      credential: { action: 'replace', value: secret },
    });
    expect(invalid.status).toBe(400);
    expect((await invalid.text()).includes(secret)).toBe(false);
  });

  test('both documented agent examples are accepted as-is after placeholder replacement', async () => {
    const app = setup();
    const todo = await createAgent(app, 'agent-todo');
    const reviewer = await createAgent(app, 'agent-reviewer');

    expect(todo.name).toBe('Todo Implementer');
    expect(todo.config.provider).toEqual({
      kind: 'reference',
      providerId: PLACEHOLDERS['<ANTHROPIC_PROVIDER_UUID>'] ?? '',
      model: 'test-anthropic-model',
    });
    expect(todo.config.metaHarness.McpMiddleware).toBe(true);
    expect(reviewer.name).toBe('Code Reviewer');
    expect(reviewer.config.provider).toEqual({
      kind: 'reference',
      providerId: PLACEHOLDERS['<OPENAI_PROVIDER_UUID>'] ?? '',
      model: 'test-openai-model',
    });

    const list = AgentListResponse.parse(await (await app.request('/api/agents')).json());
    expect(list.agents).toHaveLength(2);
  });

  test('the documented preview body works without Store or provider keys and persists nothing', async () => {
    const app = setup();
    const response = await post(app, '/api/agents/preview', draft('agent-todo'));

    expect(response.status).toBe(200);
    const preview = AgentPreviewResponse.parse(await response.json());
    expect(preview.target.chatId).toBe('preview');
    expect(preview.warnings.map((warning) => warning.code)).toEqual(
      expect.arrayContaining(['api_key_missing', 'history_not_persisted']),
    );

    const list = AgentListResponse.parse(await (await app.request('/api/agents')).json());
    expect(list.agents).toHaveLength(0);
  });

  test('the documented chat body reaches the published runtime prerequisite boundary', async () => {
    const app = setup();
    const agent = await createAgent(app, 'agent-todo');
    const body = JSON.parse(contractBody('chat-create', { '<AGENT_ID>': agent.id }));

    const response = await post(app, '/api/chats', body);

    expect(response.status).toBe(503);
    const error = ApiErrorBody.parse(await response.json());
    expect(error.error.code).toBe('engine_unavailable');
    expect(error.error.details.reasons).toEqual(expect.arrayContaining(['store_token_missing']));

    const chats = ChatListResponse.parse(await (await app.request('/api/chats')).json());
    expect(chats.chats).toHaveLength(0);
  });

  test('both documented agents remain after reopening the same database', async () => {
    temporaryDirectory = realpathSync(mkdtempSync(path.join(tmpdir(), 'agent-builder-readme-')));
    const databasePath = path.join(temporaryDirectory, 'agent-builder.db');
    const first = openDatabase(databasePath);
    migrate(first);
    seedProviders(first);
    const firstApp = createApp({ env: testEnv(), db: first });
    await createAgent(firstApp, 'agent-todo');
    await createAgent(firstApp, 'agent-reviewer');
    first.close();

    db = openDatabase(databasePath);
    migrate(db);
    const secondApp = createApp({ env: testEnv(), db });
    const list = AgentListResponse.parse(await (await secondApp.request('/api/agents')).json());

    expect(list.agents.map((agent) => agent.name).sort()).toEqual(['Code Reviewer', 'Todo Implementer']);
  });
});
