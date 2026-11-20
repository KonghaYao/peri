import { describe, expect, test } from 'bun:test';
import {
  AgentResource,
  AUTHORIZATION,
  deleteRequest,
  PlatformErrorBody,
  postJson,
  setupAgentApp,
} from './helpers';

function metadataPairs(count: number): Record<string, string> {
  return Object.fromEntries(Array.from({ length: count }, (_, index) => [`key-${index}`, 'value']));
}

describe('agent request validation', () => {
  test('rejects unknown top-level fields', async () => {
    const ctx = setupAgentApp();
    const response = await ctx.app.request('/v1/agents', postJson({ model: 'model-1', unexpected: true }));
    expect(response.status).toBe(400);
    const body = PlatformErrorBody.parse(await response.json());
    expect(body.error.type).toBe('invalid_request_error');
  });

  test('rejects unknown nested tool fields', async () => {
    const ctx = setupAgentApp();
    const response = await ctx.app.request(
      '/v1/agents',
      postJson({
        model: 'model-1',
        tools: [{ type: 'web_search', unexpected: true }],
      }),
    );
    expect(response.status).toBe(400);
    expect(PlatformErrorBody.parse(await response.json()).error.type).toBe('invalid_request_error');
  });

  test('enforces the downstream 64 KiB instruction limit', async () => {
    const ctx = setupAgentApp();
    const tooLong = await ctx.app.request(
      '/v1/agents',
      postJson({ model: 'model-1', instructions: 'a'.repeat(65_537) }),
    );
    expect(tooLong.status).toBe(400);
    const body = PlatformErrorBody.parse(await tooLong.json());
    expect(body.error.type).toBe('invalid_request_error');
    expect(body.error.param).toBe('instructions');

    const boundary = await ctx.app.request(
      '/v1/agents',
      postJson({ model: 'model-1', instructions: 'a'.repeat(65_536) }),
    );
    expect(boundary.status).toBe(201);
    expect(AgentResource.parse(await boundary.json()).instructions).toHaveLength(65_536);
  });

  test('enforces metadata pair, key and value bounds', async () => {
    const ctx = setupAgentApp();

    const tooManyPairs = await ctx.app.request(
      '/v1/agents',
      postJson({ model: 'model-1', metadata: metadataPairs(17) }),
    );
    expect(tooManyPairs.status).toBe(400);
    expect(PlatformErrorBody.parse(await tooManyPairs.json()).error.param).toContain('metadata');

    const longKey = await ctx.app.request(
      '/v1/agents',
      postJson({ model: 'model-1', metadata: { ['k'.repeat(65)]: 'value' } }),
    );
    expect(longKey.status).toBe(400);
    expect(PlatformErrorBody.parse(await longKey.json()).error.param).toContain('metadata');

    const longValue = await ctx.app.request(
      '/v1/agents',
      postJson({ model: 'model-1', metadata: { key: 'v'.repeat(513) } }),
    );
    expect(longValue.status).toBe(400);

    const nonString = await ctx.app.request(
      '/v1/agents',
      postJson({ model: 'model-1', metadata: { key: 42 } }),
    );
    expect(nonString.status).toBe(400);
  });

  test('rejects a name longer than the wire limit and a missing model', async () => {
    const ctx = setupAgentApp();

    const longName = await ctx.app.request(
      '/v1/agents',
      postJson({ name: 'n'.repeat(129), model: 'model-1' }),
    );
    expect(longName.status).toBe(400);

    const missingModel = await ctx.app.request('/v1/agents', postJson({ name: 'No model' }));
    expect(missingModel.status).toBe(400);
    expect(PlatformErrorBody.parse(await missingModel.json()).error.param).toBe('model');
  });

  test('rejects invalid enum values and non-object bodies', async () => {
    const ctx = setupAgentApp();

    const invalidReasoning = await ctx.app.request(
      '/v1/agents',
      postJson({ model: 'model-1', reasoning: { effort: 'extreme' } }),
    );
    expect(invalidReasoning.status).toBe(400);

    const arrayBody = await ctx.app.request('/v1/agents', postJson([]));
    expect(arrayBody.status).toBe(400);

    const wrongContentType = await ctx.app.request('/v1/agents', {
      method: 'POST',
      headers: AUTHORIZATION,
      body: JSON.stringify({ model: 'model-1' }),
    });
    expect(wrongContentType.status).toBe(400);
    expect(PlatformErrorBody.parse(await wrongContentType.json()).error.type).toBe('invalid_request_error');
  });

  test('validates list pagination parameters', async () => {
    const ctx = setupAgentApp();

    const zeroLimit = await ctx.app.request('/v1/agents?limit=0', { headers: AUTHORIZATION });
    expect(zeroLimit.status).toBe(400);
    expect(PlatformErrorBody.parse(await zeroLimit.json()).error.param).toBe('limit');

    const textLimit = await ctx.app.request('/v1/agents?limit=abc', { headers: AUTHORIZATION });
    expect(textLimit.status).toBe(400);

    const badOrder = await ctx.app.request('/v1/agents?order=sideways', { headers: AUTHORIZATION });
    expect(badOrder.status).toBe(400);
    expect(PlatformErrorBody.parse(await badOrder.json()).error.param).toBe('order');
  });

  test('returns 404 for updates and deletes of unknown agents', async () => {
    const ctx = setupAgentApp();

    const update = await ctx.app.request('/v1/agents/does-not-exist', postJson({ model: 'model-1' }));
    expect(update.status).toBe(404);
    expect(PlatformErrorBody.parse(await update.json()).error.code).toBe('not_found');

    const remove = await ctx.app.request('/v1/agents/does-not-exist', deleteRequest());
    expect(remove.status).toBe(404);
    expect(PlatformErrorBody.parse(await remove.json()).error.type).toBe('not_found_error');
  });
});
