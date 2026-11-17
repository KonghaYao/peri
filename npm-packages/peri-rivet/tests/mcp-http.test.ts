import { describe, expect, test } from 'bun:test';
import type { ResourceSnapshot } from '../src/contracts/client.js';
import { parseConfigRevision } from '../src/contracts/index.js';
import { type parseResourceId, parseResourceScope } from '../src/index.js';
import {
  createResourceMcpHttpHandler,
  RESOURCE_TOOL_NAMES,
  type ResourceMcpHttpHandler,
} from '../src/mcp/index.js';
import type { ResourceQueries } from '../src/resource/queries.js';

const ENDPOINT = 'http://localhost/mcp';

interface HttpRpcResult {
  readonly status: number;
  readonly body: Record<string, unknown>;
}

class TestQueries implements ResourceQueries {
  readonly calls: Array<{ scope: string; resourceId: string }> = [];
  readonly #entries = new Map<string, ResourceSnapshot>();

  put(scope: string, resourceId: string, value: ResourceSnapshot): void {
    this.#entries.set(`${scope}\u0000${resourceId}`, value);
  }

  async get(scope: ReturnType<typeof parseResourceScope>, resourceId: ReturnType<typeof parseResourceId>) {
    this.calls.push({ scope, resourceId });
    return this.#entries.get(`${scope}\u0000${resourceId}`) ?? null;
  }
}

function snapshot(): ResourceSnapshot {
  return {
    kind: 'agent_config',
    revision: parseConfigRevision('rev_1'),
    value: { model: 'peri' },
    observedAt: '2026-11-13T00:00:00.000Z',
  };
}

function expectRecord(value: unknown, label: string): Record<string, unknown> {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error(`${label} must be an object, received: ${JSON.stringify(value)}`);
  }
  return value as Record<string, unknown>;
}

function expectString(value: unknown, label: string): string {
  if (typeof value !== 'string') {
    throw new Error(`${label} must be a string, received: ${JSON.stringify(value)}`);
  }
  return value;
}

function structuredOf(body: Record<string, unknown>): Record<string, unknown> {
  return expectRecord(body.structuredContent, 'structuredContent');
}

function toolErrorCodeOf(body: Record<string, unknown>): string {
  const [block] = Array.isArray(body.content) ? body.content : [];
  const content = expectRecord(block, 'tool content block');
  const payload = expectRecord(JSON.parse(expectString(content.text, 'tool content text')), 'tool error');
  return expectString(expectRecord(payload.error, 'error payload').code, 'error.code');
}

async function sendTool(
  handler: ResourceMcpHttpHandler,
  id: number,
  name: string,
  args: Record<string, unknown>,
  scope: string | null,
): Promise<HttpRpcResult> {
  const headers: Record<string, string> = {
    'content-type': 'application/json',
    accept: 'application/json, text/event-stream',
  };
  if (scope !== null) {
    headers['x-test-scope'] = scope;
  }

  const response = await handler.fetch(
    new Request(ENDPOINT, {
      method: 'POST',
      headers,
      body: JSON.stringify({ jsonrpc: '2.0', id, method: 'tools/call', params: { name, arguments: args } }),
    }),
  );
  const text = await response.text();
  if ((response.headers.get('content-type') ?? '').includes('application/json')) {
    return { status: response.status, body: expectRecord(JSON.parse(text), 'JSON response') };
  }

  const dataLine = text.split('\n').find((line) => line.startsWith('data: '));
  if (dataLine === undefined) {
    throw new Error(`expected an SSE data line, received: ${text}`);
  }
  const message = expectRecord(JSON.parse(dataLine.slice('data: '.length)), 'JSON-RPC message');
  if (message.error !== undefined) {
    throw new Error(`unexpected JSON-RPC error: ${JSON.stringify(message.error)}`);
  }
  return { status: response.status, body: expectRecord(message.result, 'JSON-RPC result') };
}

function scopeFromHeader(request: Request): ReturnType<typeof parseResourceScope> | null {
  const raw = request.headers.get('x-test-scope');
  return raw === null ? null : parseResourceScope(raw);
}

describe('resource MCP HTTP handler', () => {
  test('requests that do not resolve to a scope receive a 401 JSON error', async () => {
    const queries = new TestQueries();
    const handler = createResourceMcpHttpHandler({ queries, resolveScope: () => null });
    try {
      const result = await sendTool(handler, 1, RESOURCE_TOOL_NAMES.get, { resourceId: 'res_1' }, null);
      expect(result.status).toBe(401);
      expect(expectRecord(result.body.error, 'error payload').code).toBe('mcp_scope_required');
      expect(queries.calls).toHaveLength(0);
    } finally {
      await handler.close();
    }
  });

  test('the authenticated scope reaches queries and cannot be overridden by arguments', async () => {
    const queries = new TestQueries();
    queries.put('scope_a', 'res_1', snapshot());
    const handler = createResourceMcpHttpHandler({ queries, resolveScope: scopeFromHeader });
    try {
      const found = await sendTool(
        handler,
        1,
        RESOURCE_TOOL_NAMES.get,
        { resourceId: 'res_1', scope: 'scope_b' },
        'scope_a',
      );
      expect(found.status).toBe(200);
      expect(structuredOf(found.body)).toMatchObject({
        kind: 'agent_config',
        revision: 'rev_1',
        value: { model: 'peri' },
        observedAt: '2026-11-13T00:00:00.000Z',
      });
      expect(queries.calls).toEqual([{ scope: 'scope_a', resourceId: 'res_1' }]);

      const crossScope = await sendTool(
        handler,
        2,
        RESOURCE_TOOL_NAMES.get,
        { resourceId: 'res_1' },
        'scope_b',
      );
      expect(crossScope.body.isError).toBe(true);
      expect(toolErrorCodeOf(crossScope.body)).toBe('resource_not_found');
    } finally {
      await handler.close();
    }
  });

  test('missing resources are reported as resource_not_found tool errors', async () => {
    const queries = new TestQueries();
    const handler = createResourceMcpHttpHandler({
      queries,
      resolveScope: () => parseResourceScope('scope_a'),
    });
    try {
      const result = await sendTool(handler, 1, RESOURCE_TOOL_NAMES.get, { resourceId: 'res_missing' }, null);
      expect(result.status).toBe(200);
      expect(result.body.isError).toBe(true);
      expect(toolErrorCodeOf(result.body)).toBe('resource_not_found');
    } finally {
      await handler.close();
    }
  });

  test('invalid tool arguments are rejected as tool errors', async () => {
    const queries = new TestQueries();
    const handler = createResourceMcpHttpHandler({
      queries,
      resolveScope: () => parseResourceScope('scope_a'),
    });
    try {
      const result = await sendTool(handler, 1, RESOURCE_TOOL_NAMES.get, { resourceId: '' }, null);
      expect(result.body.isError).toBe(true);
      expect(queries.calls).toHaveLength(0);
    } finally {
      await handler.close();
    }
  });

  test('a request body over the configured limit is rejected before dispatch', async () => {
    const queries = new TestQueries();
    const handler = createResourceMcpHttpHandler({
      queries,
      resolveScope: () => parseResourceScope('scope_a'),
      maxRequestBodySize: 256,
    });
    try {
      const result = await sendTool(
        handler,
        1,
        RESOURCE_TOOL_NAMES.get,
        { resourceId: 'res_1', pad: 'x'.repeat(1024) },
        null,
      );
      expect(result.status).toBe(413);
      expect(expectString(expectRecord(result.body.error, 'error payload').message, 'message')).toContain(
        '256',
      );
      expect(queries.calls).toHaveLength(0);
    } finally {
      await handler.close();
    }
  });
});
