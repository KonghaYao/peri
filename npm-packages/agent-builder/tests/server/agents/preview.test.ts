import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import { type AgentDraft, AgentListResponse, AgentPreviewResponse, PREVIEW_CHAT_ID } from '@shared/agent';
import { ApiErrorBody } from '@shared/api';
import type { Hono } from 'hono';
import { REDACTED_VALUE } from '../../../server/agents/open-target';
import { createApp } from '../../../server/app';
import { openDatabase } from '../../../server/db/database';
import { migrate } from '../../../server/db/migrate';
import { testEnv } from '../helpers';

let db: Database | null = null;

afterEach(() => {
  db?.close();
  db = null;
});

interface SetupResult {
  readonly app: Hono;
  readonly database: Database;
}

function setup(envOverrides: Parameters<typeof testEnv>[0] = {}): SetupResult {
  const database = openDatabase(':memory:');
  migrate(database);
  db = database;
  return { app: createApp({ env: testEnv(envOverrides), db: database }), database };
}

const secretDraft = {
  name: 'Preview Agent',
  description: 'Preview only',
  config: {
    instructions: 'Answer briefly',
    provider: {
      id: 'anthropic-main',
      type: 'anthropic',
      model: 'claude-sonnet-4-5',
      baseUrl: 'https://api.example.com',
    },
    metaHarness: { '01_intro': true, McpMiddleware: false },
    mcpServers: {
      docs: {
        type: 'http',
        url: 'https://example.com/mcp',
        headers: { Authorization: 'Bearer mcp-header-secret' },
      },
      local: {
        type: 'stdio',
        command: 'npx',
        args: ['-y', 'some-mcp'],
        env: { MCP_TOKEN: 'stdio-env-secret' },
      },
    },
  },
} satisfies AgentDraft;

async function preview(app: Hono, draft: unknown): Promise<Response> {
  return app.request('/api/agents/preview', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(draft),
  });
}

describe('POST /api/agents/preview', () => {
  test('returns a redacted target and warnings without persisting anything', async () => {
    const { app } = setup({
      storeToken: 'store-super-secret-value',
    });
    const response = await preview(app, secretDraft);
    const raw = await response.text();

    expect(response.status).toBe(200);
    expect(raw).not.toContain('super-secret-value');
    expect(raw).not.toContain('mcp-header-secret');
    expect(raw).not.toContain('stdio-env-secret');

    const body = AgentPreviewResponse.parse(JSON.parse(raw));
    expect(body.target.chatId).toBe(PREVIEW_CHAT_ID);
    expect(body.target.agent.settings.config.active_alias).toBe('sonnet');
    expect(body.target.agent.settings.config.providers[0]?.apiKey).toBeUndefined();
    expect(body.target.agent.settings.config.providers[0]?.apiKey).not.toBe('sk-ant-super-secret-value');
    expect(body.target.storage.authToken).toBe(REDACTED_VALUE);
    expect(body.target.session.mcpServers?.docs).toEqual({
      type: 'http',
      url: 'https://example.com/mcp',
      headers: { Authorization: REDACTED_VALUE },
    });
    expect(body.target.session.mcpServers?.local).toEqual({
      type: 'stdio',
      command: 'npx',
      args: ['-y', 'some-mcp'],
      env: { MCP_TOKEN: REDACTED_VALUE },
    });
    expect(body.target.session.instructions).toBe('Answer briefly');
    expect(body.warnings.map((warning) => warning.code)).toEqual([
      'provider_missing',
      'mcp_not_connected',
      'history_not_persisted',
    ]);

    const list = AgentListResponse.parse(await (await app.request('/api/agents')).json());
    expect(list.agents).toEqual([]);
  });

  test('keeps Manager-scoped keys out of the agent config', async () => {
    const { app } = setup();
    const body = AgentPreviewResponse.parse(await (await preview(app, secretDraft)).json());
    const agent = body.target.agent as Record<string, unknown>;
    const serialized = JSON.stringify(agent);

    expect(Object.keys(agent)).toEqual(['settings']);
    expect(serialized).not.toContain('"cwd"');
    expect(serialized).not.toContain('"storage"');
    expect(serialized).not.toContain('"machineId"');
    expect(body.target.workspace).toBe(testEnv().workspaceUri);
  });

  test('omits empty sections and reports missing provider and API key', async () => {
    const { app } = setup();
    const draft: AgentDraft = {
      name: 'Empty Agent',
      description: '',
      config: { instructions: '', provider: null, metaHarness: {}, mcpServers: {} },
    };

    const withProvider = AgentPreviewResponse.parse(
      await (
        await preview(app, {
          ...draft,
          config: { ...draft.config, provider: { id: 'oai', type: 'openai', model: 'gpt-5' } },
        })
      ).json(),
    );
    expect(withProvider.warnings.map((warning) => warning.code)).toContain('provider_missing');

    const empty = AgentPreviewResponse.parse(await (await preview(app, draft)).json());
    expect(empty.target.agent.settings.config.providers).toEqual([]);
    expect(empty.target.agent.settings.config.profiles).toEqual({});
    expect(empty.target.agent.settings.config.meta_harness).toBeUndefined();
    expect(empty.target.session.instructions).toBeUndefined();
    expect(empty.target.session.mcpServers).toBeUndefined();
    expect(empty.warnings.map((warning) => warning.code)).toEqual([
      'provider_missing',
      'history_not_persisted',
    ]);
  });

  test('rejects unknown metaHarness keys without echoing arbitrary names', async () => {
    const { app } = setup();
    const secret = crypto.randomUUID() + crypto.randomUUID();
    const response = await preview(app, {
      ...secretDraft,
      config: { ...secretDraft.config, metaHarness: { [secret]: true, another_fake: false } },
    });

    expect(response.status).toBe(400);
    const body = ApiErrorBody.parse(await response.json());
    expect(body.error.code).toBe('invalid_input');
    expect(body.error.details).toEqual({});
    expect(JSON.stringify(body).includes(secret)).toBe(false);
  });
});
