import { describe, expect, test } from 'bun:test';
import {
  AgentResource,
  AUTHORIZATION,
  PlatformErrorBody,
  postJson,
  seedProvider,
  setupAgentApp,
} from './helpers';

function storedAgent(ctx: ReturnType<typeof setupAgentApp>, id: string) {
  const row = ctx.db
    .query<{ config: string; provider_id: string | null; name: string; wire_name: string | null }, [string]>(
      'SELECT config, provider_id, name, wire_name FROM agents WHERE id = ?',
    )
    .get(id);
  if (row === null) throw new Error(`Agent ${id} was not stored`);
  return { ...row, config: JSON.parse(row.config) as Record<string, unknown> };
}

describe('agent capability mapping', () => {
  test('resolves provider/model by provider name or id and leaves unknown providers unbound', async () => {
    const ctx = setupAgentApp();
    const providerId = seedProvider(ctx, 'acme');

    const byNameResponse = await ctx.app.request(
      '/v1/agents',
      postJson({ name: 'By name', model: 'acme/model-one' }),
    );
    expect(byNameResponse.status).toBe(201);
    const byName = AgentResource.parse(await byNameResponse.json());
    expect(byName.model).toBe('acme/model-one');
    const byNameRow = storedAgent(ctx, byName.id);
    expect(byNameRow.provider_id).toBe(providerId);
    expect(byNameRow.config.provider).toEqual({ model: 'model-one' });

    const byIdResponse = await ctx.app.request(
      '/v1/agents',
      postJson({ name: 'By id', model: `${providerId}/org/model-two` }),
    );
    const byId = AgentResource.parse(await byIdResponse.json());
    expect(byId.model).toBe(`${providerId}/org/model-two`);
    const byIdRow = storedAgent(ctx, byId.id);
    expect(byIdRow.provider_id).toBe(providerId);
    expect(byIdRow.config.provider).toEqual({ model: 'org/model-two' });

    const unknownResponse = await ctx.app.request(
      '/v1/agents',
      postJson({ name: 'Unknown', model: 'missing/model-three' }),
    );
    expect(unknownResponse.status).toBe(201);
    const unknown = AgentResource.parse(await unknownResponse.json());
    expect(unknown.model).toBe('missing/model-three');
    const unknownRow = storedAgent(ctx, unknown.id);
    expect(unknownRow.provider_id).toBeNull();
    expect(unknownRow.config.provider).toBeNull();

    const bareResponse = await ctx.app.request('/v1/agents', postJson({ name: 'Bare', model: 'bare-model' }));
    const bare = AgentResource.parse(await bareResponse.json());
    expect(bare.model).toBe('bare-model');
    expect(storedAgent(ctx, bare.id).provider_id).toBeNull();
  });

  test('maps tools and multi_agent onto the shared agent config', async () => {
    const ctx = setupAgentApp();
    const response = await ctx.app.request(
      '/v1/agents',
      postJson({
        name: 'Mapped',
        model: 'model-1',
        tools: [
          {
            type: 'mcp',
            server_label: 'local_tools',
            transport: {
              type: 'stdio',
              command: 'node',
              args: ['server.js'],
              cwd: '/tmp/tools',
              env: { TOKEN: 'value' },
              env_vars: ['HOME'],
            },
          },
          { type: 'tool_search' },
          { type: 'web_search' },
        ],
        multi_agent: { enabled: true, max_concurrent_subagents: 3 },
      }),
    );
    expect(response.status).toBe(201);
    const agent = AgentResource.parse(await response.json());
    expect(agent.tools).toEqual([
      {
        type: 'mcp',
        server_label: 'local_tools',
        credential_id: null,
        transport: {
          type: 'stdio',
          command: 'node',
          args: ['server.js'],
          cwd: '/tmp/tools',
          env_vars: ['HOME'],
        },
        request_metadata: {},
        allowed_tools: null,
        required: false,
        connection_origin: 'service',
      },
      { type: 'tool_search' },
      {
        type: 'web_search',
        mode: 'live',
        context_size: 'medium',
        allowed_domains: null,
        location: null,
      },
    ]);
    expect(agent.multi_agent).toEqual({ enabled: true, max_concurrent_subagents: 3 });

    const row = storedAgent(ctx, agent.id);
    expect(row.config.metaHarness).toEqual({
      McpMiddleware: true,
      ToolSearch: true,
      WebMiddleware: true,
      SubAgentMiddleware: true,
      BuiltInSubagents: true,
    });
    expect(row.config.mcpServers).toEqual({
      local_tools: {
        type: 'stdio',
        command: 'node',
        args: ['server.js'],
        env: { TOKEN: 'value' },
        cwd: '/tmp/tools',
        env_vars: ['HOME'],
      },
    });
  });

  test('persists HTTP authorization only in the MCP server config, never in the resource', async () => {
    const ctx = setupAgentApp();
    const secret = 'mcp-authorization-secret-4a1c';
    const response = await ctx.app.request(
      '/v1/agents',
      postJson({
        name: 'Authorized MCP',
        model: 'model-1',
        tools: [
          {
            type: 'mcp',
            server_label: 'search',
            transport: {
              type: 'http',
              server_url: 'http://127.0.0.1:9999/mcp',
              authorization: `Bearer ${secret}`,
            },
          },
        ],
      }),
    );
    expect(response.status).toBe(201);
    const raw = await response.text();
    expect(raw).not.toContain(secret);
    const agent = AgentResource.parse(JSON.parse(raw));
    expect(agent.tools).toEqual([
      {
        type: 'mcp',
        server_label: 'search',
        credential_id: null,
        transport: { type: 'http', server_url: 'http://127.0.0.1:9999/mcp' },
        request_metadata: {},
        allowed_tools: null,
        required: false,
        connection_origin: 'service',
      },
    ]);

    const row = storedAgent(ctx, agent.id);
    expect(row.config.mcpServers).toEqual({
      search: {
        type: 'http',
        url: 'http://127.0.0.1:9999/mcp',
        authorization: `Bearer ${secret}`,
      },
    });
  });

  test('keeps extended MCP fields readable through the /api agent resource', async () => {
    const ctx = setupAgentApp();
    const secret = 'mcp-authorization-secret-9f2a';
    const response = await ctx.app.request(
      '/v1/agents',
      postJson({
        name: 'Extended MCP',
        model: 'model-1',
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
      }),
    );
    expect(response.status).toBe(201);
    const agent = AgentResource.parse(await response.json());

    const apiResponse = await ctx.app.request(`/api/agents/${agent.id}`);
    expect(apiResponse.status).toBe(200);
    const apiAgent = (await apiResponse.json()) as {
      agent: { config: { mcpServers: Record<string, unknown> } };
    };
    expect(apiAgent.agent.config.mcpServers).toEqual({
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
    });
  });

  test('keeps multi-agent capabilities explicitly disabled when omitted', async () => {
    const ctx = setupAgentApp();
    const response = await ctx.app.request(
      '/v1/agents',
      postJson({
        name: 'Solo',
        model: 'model-1',
        tools: [{ type: 'function', name: 'f', description: 'd', parameters: {} }],
      }),
    );
    const agent = AgentResource.parse(await response.json());
    expect(agent.multi_agent).toEqual({ enabled: false, max_concurrent_subagents: null });

    const row = storedAgent(ctx, agent.id);
    expect(row.config.metaHarness).toEqual({
      SubAgentMiddleware: false,
      BuiltInSubagents: false,
    });
    expect(row.config.mcpServers).toEqual({});
  });

  test('degrades unsupported tool types without attaching a local capability', async () => {
    const ctx = setupAgentApp();
    const response = await ctx.app.request(
      '/v1/agents',
      postJson({
        name: 'Degraded',
        model: 'model-1',
        tools: [
          { type: 'computer_use', include_screenshots: true },
          { type: 'function', name: 'f', description: 'd', parameters: {} },
        ],
      }),
    );
    expect(response.status).toBe(201);
    expect(response.headers.get('x-platform-degraded')?.split(',').sort()).toEqual([
      'computer_use',
      'function',
    ]);
    const row = storedAgent(ctx, AgentResource.parse(await response.json()).id);
    expect(row.config.metaHarness).toEqual({
      SubAgentMiddleware: false,
      BuiltInSubagents: false,
    });
  });

  test('preserves exact model names and keeps the full wire name beside the 80-character /api view', async () => {
    const ctx = setupAgentApp();
    const longName = 'n'.repeat(128);
    const response = await ctx.app.request('/v1/agents', postJson({ name: longName, model: 'model-1' }));
    expect(response.status).toBe(201);
    const agent = AgentResource.parse(await response.json());
    expect(agent.name).toBe(longName);
    const stored = storedAgent(ctx, agent.id);
    expect(stored.name).toBe('n'.repeat(80));
    expect(stored.wire_name).toBe(longName);

    const apiResponse = await ctx.app.request(`/api/agents/${agent.id}`);
    expect(apiResponse.status).toBe(200);
    const apiAgent = (await apiResponse.json()) as { agent: { name: string } };
    expect(apiAgent.agent.name).toBe('n'.repeat(80));

    const unnamedResponse = await ctx.app.request('/v1/agents', postJson({ name: null, model: 'model-1' }));
    const unnamed = AgentResource.parse(await unnamedResponse.json());
    expect(unnamed.name).toBeNull();
    const unnamedRow = storedAgent(ctx, unnamed.id);
    expect(unnamedRow.name).toBe('');
    expect(unnamedRow.wire_name).toBeNull();
  });

  test('keeps omitted and empty instructions distinct and defaults metadata', async () => {
    const ctx = setupAgentApp();
    const omitted = AgentResource.parse(
      await (await ctx.app.request('/v1/agents', postJson({ model: 'model-1' }))).json(),
    );
    expect(omitted.instructions).toBeNull();
    expect(omitted.metadata).toEqual({});
    expect(omitted.compaction).toEqual({ async: false });
    expect(omitted.experimental).toEqual({ time_awareness: false });
    expect(omitted.service_tier).toBe('auto');
    expect(omitted.reasoning).toEqual({ effort: null, summary: null });
    expect(omitted.text).toEqual({ format: { type: 'text' }, verbosity: 'medium' });

    const explicit = AgentResource.parse(
      await (
        await ctx.app.request('/v1/agents', postJson({ model: 'model-1', instructions: '', metadata: null }))
      ).json(),
    );
    expect(explicit.instructions).toBe('');
    expect(explicit.metadata).toEqual({});
  });

  test('rejects MCP transports that the runtime cannot represent', async () => {
    const ctx = setupAgentApp();

    const invalidLabel = await ctx.app.request(
      '/v1/agents',
      postJson({
        model: 'model-1',
        tools: [
          {
            type: 'mcp',
            server_label: 'bad label',
            transport: { type: 'http', server_url: 'http://127.0.0.1:1/mcp' },
          },
        ],
      }),
    );
    expect(invalidLabel.status).toBe(400);
    expect(PlatformErrorBody.parse(await invalidLabel.json()).error.param).toBe('tools.0.server_label');

    const reservedLabel = await ctx.app.request(
      '/v1/agents',
      postJson({
        model: 'model-1',
        tools: [
          {
            type: 'mcp',
            server_label: 'peri-workspace-sandbox',
            transport: { type: 'http', server_url: 'http://127.0.0.1:1/mcp' },
          },
        ],
      }),
    );
    expect(reservedLabel.status).toBe(400);
    expect(PlatformErrorBody.parse(await reservedLabel.json()).error.param).toBe('tools.0.server_label');

    const invalidUrl = await ctx.app.request(
      '/v1/agents',
      postJson({
        model: 'model-1',
        tools: [
          {
            type: 'mcp',
            server_label: 'remote',
            transport: { type: 'http', server_url: 'ftp://127.0.0.1/mcp' },
          },
        ],
      }),
    );
    expect(invalidUrl.status).toBe(400);
    expect(PlatformErrorBody.parse(await invalidUrl.json()).error.param).toBe('tools.0.transport');
  });

  test('exposes degraded tools through the list header too', async () => {
    const ctx = setupAgentApp();
    await ctx.app.request(
      '/v1/agents',
      postJson({
        name: 'Degraded',
        model: 'model-1',
        tools: [{ type: 'computer_use' }],
      }),
    );
    const response = await ctx.app.request('/v1/agents', { headers: AUTHORIZATION });
    expect(response.status).toBe(200);
    expect(response.headers.get('x-platform-degraded')).toBe('computer_use');
  });
});
