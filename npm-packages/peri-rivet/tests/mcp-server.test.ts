import { describe, expect, test } from 'bun:test';
import { type CallToolResult, Client, InMemoryTransport } from '@modelcontextprotocol/client';
import { createResourceMcpServer, RESOURCE_TOOL_NAMES } from '../src/mcp/index.js';
import type { ResourceQueries } from '../src/resource/queries.js';

let calls = 0;

const neverQueried: ResourceQueries = {
  async get() {
    calls += 1;
    throw new Error('queries must not be reached without an authenticated scope');
  },
};

async function withClient(run: (client: Client) => Promise<void>): Promise<void> {
  const server = createResourceMcpServer({ queries: neverQueried });
  const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
  await server.connect(serverTransport);
  const client = new Client({ name: 'peri-rivet-tests', version: '0.0.0' });
  await client.connect(clientTransport);
  try {
    await run(client);
  } finally {
    await client.close();
    await server.close();
  }
}

function firstText(result: CallToolResult): string {
  const [block] = result.content;
  if (block === undefined || block.type !== 'text') {
    throw new Error('expected a text content block');
  }
  return block.text;
}

function errorCode(result: CallToolResult): string {
  const parsed: unknown = JSON.parse(firstText(result));
  if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new Error(`expected an error payload, received: ${firstText(result)}`);
  }
  const error = (parsed as Record<string, unknown>).error;
  if (error === null || typeof error !== 'object' || Array.isArray(error)) {
    throw new Error(`expected an ActorError payload, received: ${firstText(result)}`);
  }
  const code = (error as Record<string, unknown>).code;
  if (typeof code !== 'string') {
    throw new Error(`expected an error code, received: ${firstText(result)}`);
  }
  return code;
}

describe('resource MCP server', () => {
  test('exposes only the resource_get tool', async () => {
    await withClient(async (client) => {
      const tools = (await client.listTools()).tools;
      expect(tools.map((tool) => tool.name)).toEqual([RESOURCE_TOOL_NAMES.get]);
      expect(tools[0]?.annotations).toMatchObject({ readOnlyHint: true, openWorldHint: false });
      expect(tools[0]?.inputSchema.required).toContain('resourceId');
    });
  });

  test('rejects calls that have no authenticated auth extra', async () => {
    await withClient(async (client) => {
      const result = await client.callTool({
        name: RESOURCE_TOOL_NAMES.get,
        arguments: { resourceId: 'res_1' },
      });
      expect(result.isError).toBe(true);
      expect(errorCode(result)).toBe('mcp_scope_required');
    });
  });

  test('does not take the scope from tool arguments', async () => {
    await withClient(async (client) => {
      const result = await client.callTool({
        name: RESOURCE_TOOL_NAMES.get,
        arguments: { resourceId: 'res_1', scope: 'tenant_a' },
      });
      expect(result.isError).toBe(true);
      expect(errorCode(result)).toBe('mcp_scope_required');
      expect(calls).toBe(0);
    });
  });

  test('rejects invalid tool arguments before the handler runs', async () => {
    await withClient(async (client) => {
      const missingId = await client.callTool({ name: RESOURCE_TOOL_NAMES.get, arguments: {} });
      expect(missingId.isError).toBe(true);

      const wrongType = await client.callTool({
        name: RESOURCE_TOOL_NAMES.get,
        arguments: { resourceId: 42 },
      });
      expect(wrongType.isError).toBe(true);
      expect(calls).toBe(0);
    });
  });
});
