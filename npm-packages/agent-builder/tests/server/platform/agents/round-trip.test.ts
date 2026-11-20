import { describe, expect, test } from 'bun:test';
import { SESSION_INLINE_AGENT_ID } from '../../../../server/db/migrate';
import {
  AgentListResource,
  AgentResource,
  AUTHORIZATION,
  deleteRequest,
  PlatformErrorBody,
  postJson,
  seedProvider,
  setupAgentApp,
} from './helpers';

function secondsNow(): number {
  return Math.floor(Date.now() / 1000);
}

describe('agent resource round trip', () => {
  test('creates, reads, lists, replaces and deletes an agent', async () => {
    const ctx = setupAgentApp();
    const providerId = seedProvider(ctx, 'acme');

    const createBody = {
      name: 'Support agent',
      model: 'acme/model-test',
      instructions: 'Be brief.',
      metadata: { team: 'support' },
      reasoning: { effort: 'high', summary: 'auto' },
      text: { verbosity: 'low' },
      service_tier: 'priority',
      tools: [
        {
          type: 'mcp',
          server_label: 'search',
          transport: { type: 'http', server_url: 'http://127.0.0.1:9999/mcp' },
        },
        { type: 'tool_search' },
        { type: 'web_search', mode: 'live' },
      ],
      multi_agent: { enabled: true },
      compaction: { async: true },
      experimental: { time_awareness: true },
    };

    const createResponse = await ctx.app.request('/v1/agents', postJson(createBody));
    expect(createResponse.status).toBe(201);
    const created = AgentResource.parse(await createResponse.json());

    expect(created.object).toBe('agent');
    expect(created.name).toBe('Support agent');
    expect(created.instructions).toBe('Be brief.');
    expect(created.model).toBe('acme/model-test');
    expect(created.metadata).toEqual({ team: 'support' });
    expect(created.reasoning).toEqual({ effort: 'high', summary: 'auto' });
    expect(created.text).toEqual({ format: { type: 'text' }, verbosity: 'low' });
    expect(created.service_tier).toBe('priority');
    expect(created.multi_agent).toEqual({ enabled: true, max_concurrent_subagents: 6 });
    expect(created.compaction).toEqual({ async: true });
    expect(created.experimental).toEqual({ time_awareness: true });
    expect(Number.isInteger(created.created_at)).toBe(true);
    expect(Math.abs(created.created_at - secondsNow())).toBeLessThanOrEqual(5);
    expect(created.updated_at).toBe(created.created_at);

    expect(created.tools).toEqual([
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
      { type: 'tool_search' },
      {
        type: 'web_search',
        mode: 'live',
        context_size: 'medium',
        allowed_domains: null,
        location: null,
      },
    ]);

    const row = ctx.db
      .query<
        {
          config: string;
          provider_id: string | null;
          model: string;
          multi_agent: string;
        },
        [string]
      >('SELECT config, provider_id, model, multi_agent FROM agents WHERE id = ?')
      .get(created.id);
    expect(row).not.toBeNull();
    const config = JSON.parse(row?.config ?? '{}');
    expect(config.provider).toEqual({ model: 'model-test' });
    expect(config.metaHarness).toEqual({
      McpMiddleware: true,
      ToolSearch: true,
      WebMiddleware: true,
      SubAgentMiddleware: true,
      BuiltInSubagents: true,
    });
    expect(config.mcpServers).toEqual({
      search: { type: 'http', url: 'http://127.0.0.1:9999/mcp' },
    });
    expect(row?.provider_id).toBe(providerId);
    expect(row?.model).toBe('acme/model-test');
    expect(JSON.parse(row?.multi_agent ?? '{}')).toEqual({
      enabled: true,
      max_concurrent_subagents: 6,
    });

    const getResponse = await ctx.app.request(`/v1/agents/${created.id}`, { headers: AUTHORIZATION });
    expect(getResponse.status).toBe(200);
    expect(AgentResource.parse(await getResponse.json())).toEqual(created);

    const listResponse = await ctx.app.request('/v1/agents', { headers: AUTHORIZATION });
    expect(listResponse.status).toBe(200);
    expect(AgentListResource.parse(await listResponse.json())).toEqual({
      object: 'list',
      data: [created],
      first_id: created.id,
      last_id: created.id,
      has_more: false,
    });

    const updateBody = {
      name: 'Renamed agent',
      model: 'bare-model',
      instructions: 'New instructions.',
      metadata: { tier: 'gold' },
      reasoning: { effort: 'low', summary: null },
      text: { format: { type: 'json_schema', schema: { type: 'object' } }, verbosity: 'high' },
      service_tier: 'flex',
      tools: [{ type: 'web_search' }],
      multi_agent: { enabled: false },
      compaction: { async: false },
      experimental: { time_awareness: false },
    };
    const updateResponse = await ctx.app.request(`/v1/agents/${created.id}`, postJson(updateBody));
    expect(updateResponse.status).toBe(200);
    const updated = AgentResource.parse(await updateResponse.json());

    expect(updated.id).toBe(created.id);
    expect(updated.object).toBe('agent');
    expect(updated.created_at).toBe(created.created_at);
    expect(updated.updated_at).toBeGreaterThanOrEqual(created.updated_at);
    expect(updated.name).toBe('Renamed agent');
    expect(updated.model).toBe('bare-model');
    expect(updated.instructions).toBe('New instructions.');
    expect(updated.metadata).toEqual({ tier: 'gold' });
    expect(updated.reasoning).toEqual({ effort: 'low', summary: null });
    expect(updated.text).toEqual({
      format: { type: 'json_schema', schema: { type: 'object' } },
      verbosity: 'high',
    });
    expect(updated.tools).toEqual([
      {
        type: 'web_search',
        mode: 'live',
        context_size: 'medium',
        allowed_domains: null,
        location: null,
      },
    ]);
    expect(updated.multi_agent).toEqual({ enabled: false, max_concurrent_subagents: null });
    expect(updated.compaction).toEqual({ async: false });
    expect(updated.experimental).toEqual({ time_awareness: false });
    expect(updated.service_tier).toBe('flex');

    const updatedRow = ctx.db
      .query<{ config: string; provider_id: string | null }, [string]>(
        'SELECT config, provider_id FROM agents WHERE id = ?',
      )
      .get(created.id);
    expect(updatedRow?.provider_id).toBeNull();
    expect(JSON.parse(updatedRow?.config ?? '{}')).toMatchObject({
      provider: null,
      metaHarness: { WebMiddleware: true, SubAgentMiddleware: false, BuiltInSubagents: false },
      mcpServers: {},
    });

    const deleteResponse = await ctx.app.request(`/v1/agents/${created.id}`, deleteRequest());
    expect(deleteResponse.status).toBe(200);
    expect(await deleteResponse.json()).toEqual({
      id: created.id,
      object: 'agent.deleted',
      deleted: true,
    });

    const missingResponse = await ctx.app.request(`/v1/agents/${created.id}`, { headers: AUTHORIZATION });
    expect(missingResponse.status).toBe(404);
    const missing = PlatformErrorBody.parse(await missingResponse.json());
    expect(missing.error.code).toBe('not_found');
    expect(missing.error.type).toBe('not_found_error');
  });

  test('applies update as a patch and resets only the fields supplied as null', async () => {
    const ctx = setupAgentApp();
    const providerId = seedProvider(ctx, 'acme');
    const createResponse = await ctx.app.request(
      '/v1/agents',
      postJson({
        name: 'Patch base',
        model: 'acme/patch-model',
        instructions: 'Keep me',
        metadata: { team: 'support' },
        reasoning: { effort: 'high' },
        text: { verbosity: 'low' },
        service_tier: 'priority',
        tools: [{ type: 'web_search', mode: 'live' }],
        multi_agent: { enabled: true, max_concurrent_subagents: 2 },
        compaction: { async: true },
        experimental: { time_awareness: true },
      }),
    );
    const created = AgentResource.parse(await createResponse.json());

    const patchedResponse = await ctx.app.request(
      `/v1/agents/${created.id}`,
      postJson({ instructions: 'Patched instructions' }),
    );
    expect(patchedResponse.status).toBe(200);
    const patched = AgentResource.parse(await patchedResponse.json());
    expect(patched).toMatchObject({
      id: created.id,
      name: 'Patch base',
      model: 'acme/patch-model',
      instructions: 'Patched instructions',
      metadata: { team: 'support' },
      reasoning: { effort: 'high', summary: null },
      text: { format: { type: 'text' }, verbosity: 'low' },
      service_tier: 'priority',
      multi_agent: { enabled: true, max_concurrent_subagents: 2 },
      compaction: { async: true },
      experimental: { time_awareness: true },
      created_at: created.created_at,
    });
    expect(patched.tools).toEqual(created.tools);
    expect(patched.updated_at).toBeGreaterThanOrEqual(created.updated_at);

    const row = ctx.db
      .query<{ provider_id: string | null; config: string }, [string]>(
        'SELECT provider_id, config FROM agents WHERE id = ?',
      )
      .get(created.id);
    expect(row?.provider_id).toBe(providerId);
    expect(JSON.parse(row?.config ?? '{}')).toMatchObject({
      instructions: 'Patched instructions',
      provider: { model: 'patch-model' },
      mcpServers: {},
    });

    const multiAgentPatch = await ctx.app.request(
      `/v1/agents/${created.id}`,
      postJson({ multi_agent: { enabled: false } }),
    );
    expect(multiAgentPatch.status).toBe(200);
    expect(AgentResource.parse(await multiAgentPatch.json()).multi_agent).toEqual({
      enabled: false,
      max_concurrent_subagents: null,
    });
    const patchedRow = ctx.db
      .query<{ config: string; tools: string }, [string]>('SELECT config, tools FROM agents WHERE id = ?')
      .get(created.id);
    expect(JSON.parse(patchedRow?.config ?? '{}').metaHarness).toMatchObject({
      SubAgentMiddleware: false,
      BuiltInSubagents: false,
    });
    expect(JSON.parse(patchedRow?.tools ?? '[]')).toEqual(created.tools);

    const clearedResponse = await ctx.app.request(
      `/v1/agents/${created.id}`,
      postJson({ name: null, metadata: null, tools: null }),
    );
    expect(clearedResponse.status).toBe(200);
    const cleared = AgentResource.parse(await clearedResponse.json());
    expect(cleared.name).toBeNull();
    expect(cleared.metadata).toEqual({});
    expect(cleared.tools).toEqual([]);
    expect(cleared.model).toBe('acme/patch-model');
    expect(cleared.instructions).toBe('Patched instructions');

    const emptyResponse = await ctx.app.request(`/v1/agents/${created.id}`, postJson({}));
    expect(emptyResponse.status).toBe(200);
    expect(AgentResource.parse(await emptyResponse.json())).toMatchObject({
      ...cleared,
      updated_at: expect.any(Number),
    });
  });

  test('paginates in id order with a stable cursor', async () => {
    const ctx = setupAgentApp();
    const ids: string[] = [];
    for (const name of ['First', 'Second', 'Third']) {
      const response = await ctx.app.request('/v1/agents', postJson({ name, model: 'model-1' }));
      expect(response.status).toBe(201);
      ids.push(AgentResource.parse(await response.json()).id);
    }
    expect(ids).toHaveLength(3);
    const [first, second, third] = ids as [string, string, string];

    const pageOneResponse = await ctx.app.request('/v1/agents?limit=2&order=asc', {
      headers: AUTHORIZATION,
    });
    const pageOne = AgentListResource.parse(await pageOneResponse.json());
    expect(pageOne.object).toBe('list');
    expect(pageOne.data.map((agent) => agent.id)).toEqual([first, second]);
    expect(pageOne.first_id).toBe(first ?? null);
    expect(pageOne.last_id).toBe(second ?? null);
    expect(pageOne.has_more).toBe(true);

    const pageTwoResponse = await ctx.app.request(`/v1/agents?limit=2&order=asc&after=${pageOne.last_id}`, {
      headers: AUTHORIZATION,
    });
    const pageTwo = AgentListResource.parse(await pageTwoResponse.json());
    expect(pageTwo.data.map((agent) => agent.id)).toEqual([third]);
    expect(pageTwo.first_id).toBe(third ?? null);
    expect(pageTwo.last_id).toBe(third ?? null);
    expect(pageTwo.has_more).toBe(false);

    const descendingResponse = await ctx.app.request('/v1/agents?limit=2', {
      headers: AUTHORIZATION,
    });
    const descending = AgentListResource.parse(await descendingResponse.json());
    expect(descending.data.map((agent) => agent.id)).toEqual([third, second]);
    expect(descending.has_more).toBe(true);
  });

  test('deletes only agents without chats or sessions', async () => {
    const ctx = setupAgentApp();
    const createResponse = await ctx.app.request(
      '/v1/agents',
      postJson({ name: 'Referenced', model: 'model-1' }),
    );
    const agent = AgentResource.parse(await createResponse.json());

    ctx.db
      .query(
        `INSERT INTO chats (chat_id, agent_id, session_id, created_at, updated_at)
         VALUES (?, ?, NULL, ?, ?)`,
      )
      .run('chat-1', agent.id, Date.now(), Date.now());

    const chatResponse = await ctx.app.request(`/v1/agents/${agent.id}`, deleteRequest());
    expect(chatResponse.status).toBe(409);
    const chatError = PlatformErrorBody.parse(await chatResponse.json());
    expect(chatError.error.code).toBe('agent_has_sessions');
    expect(chatError.error.type).toBe('conflict_error');

    ctx.db.query('DELETE FROM chats WHERE chat_id = ?').run('chat-1');
    ctx.db
      .query(
        `INSERT INTO platform_sessions
          (id, agent_id, agent_config, environment_type, status, created_at, last_active_at)
         VALUES (?, ?, '{}', 'none', 'idle', ?, ?)`,
      )
      .run('session-1', agent.id, Date.now(), Date.now());

    const sessionResponse = await ctx.app.request(`/v1/agents/${agent.id}`, deleteRequest());
    expect(sessionResponse.status).toBe(409);
    expect(PlatformErrorBody.parse(await sessionResponse.json()).error.code).toBe('agent_has_sessions');

    ctx.db.query('DELETE FROM platform_sessions WHERE id = ?').run('session-1');
    const deletedResponse = await ctx.app.request(`/v1/agents/${agent.id}`, deleteRequest());
    expect(deletedResponse.status).toBe(200);
    expect(await deletedResponse.json()).toEqual({
      id: agent.id,
      object: 'agent.deleted',
      deleted: true,
    });
  });

  test('hides the reserved inline-session agent from the agent surface', async () => {
    const ctx = setupAgentApp();

    const platformList = AgentListResource.parse(
      await (await ctx.app.request('/v1/agents', { headers: AUTHORIZATION })).json(),
    );
    expect(platformList).toEqual({
      object: 'list',
      data: [],
      first_id: null,
      last_id: null,
      has_more: false,
    });

    const platformGet = await ctx.app.request(`/v1/agents/${SESSION_INLINE_AGENT_ID}`, {
      headers: AUTHORIZATION,
    });
    expect(platformGet.status).toBe(404);
    expect(PlatformErrorBody.parse(await platformGet.json()).error.code).toBe('not_found');

    const createResponse = await ctx.app.request(
      '/v1/agents',
      postJson({ name: 'Visible', model: 'model-1' }),
    );
    const created = AgentResource.parse(await createResponse.json());

    const firstPage = AgentListResource.parse(
      await (await ctx.app.request('/v1/agents?limit=1&order=asc', { headers: AUTHORIZATION })).json(),
    );
    expect(firstPage.data.map((agent) => agent.id)).toEqual([created.id]);
    expect(firstPage.has_more).toBe(false);

    const rowCount = ctx.db.query<{ count: number }, []>('SELECT COUNT(*) AS count FROM agents').get();
    expect(rowCount?.count).toBe(2);
  });
});
