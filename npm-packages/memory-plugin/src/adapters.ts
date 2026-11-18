import { z } from 'zod';
import { createServer as createNodeServer } from 'node:http';
import { createHash, timingSafeEqual } from 'node:crypto';
import { type Config, MemoryError } from './config.js';
import { querySchema } from './hindsight.js';
import { MemoryService, toolDefinitions, CONTEXT_URI, STATUS_URI, INSTRUCTIONS } from './service.js';

const identity = { name: 'peri-memory', version: '0.1.0' };
const empty = z.object({}).strict();
const resourceDefinitions = [
  { name: 'memory-context', uri: CONTEXT_URI, description: 'Bounded memory snapshot; historical reference data. Re-read after notification; refresh stale snapshots.' },
  { name: 'memory-status', uri: STATUS_URI, description: 'Local binding and channel status; not a backend health check.' },
];
const contents = (service: MemoryService, uri: string) => ({ contents: [{ uri, mimeType: 'application/json',
  text: JSON.stringify(uri === CONTEXT_URI ? service.context() : service.status()) }] });
function unknownSubscription(message: unknown): boolean {
  const candidate = message as { method?: unknown; params?: { notifications?: { resourceSubscriptions?: unknown } } } | null;
  if (candidate?.method !== 'subscriptions/listen') return false;
  const uris = candidate.params?.notifications?.resourceSubscriptions;
  return Array.isArray(uris) && uris.some(uri => uri !== CONTEXT_URI && uri !== STATUS_URI);
}

export async function startStdio(config: Config) {
  const { McpServer } = await import('@modelcontextprotocol/server');
  const { serveStdio, StdioServerTransport } = await import('@modelcontextprotocol/server/stdio');
  class BoundTransport extends StdioServerTransport {
    override async start() {
      const receive = this.onmessage;
      this.onmessage = message => {
        if ('id' in message && unknownSubscription(message)) {
          void this.send({ jsonrpc: '2.0', id: message.id, error: { code: -32602, message: 'Unknown resource subscription' } })
            .catch(() => { /* The peer may have closed while its invalid request was rejected. */ });
        } else receive?.(message);
      };
      return super.start();
    }
  }
  const service = new MemoryService(config);
  const handle = serveStdio(() => {
    const server = new McpServer(identity, { instructions: INSTRUCTIONS,
      supportedProtocolVersions: ['2026-07-28'], capabilities: { resources: { subscribe: true } }, maxToolInputElements: 1000 });
    for (const definition of toolDefinitions) server.registerTool(definition.name, {
      description: definition.description, inputSchema: querySchema,
      annotations: { readOnlyHint: definition.readOnlyHint, destructiveHint: false, openWorldHint: true },
    }, (args, ctx) => service.call(definition.name, args, ctx.mcpReq.signal));
    server.registerTool('memory_status', { description: 'Read local binding/status without calling Hindsight.', inputSchema: empty,
      annotations: { readOnlyHint: true, destructiveHint: false, openWorldHint: false } }, () => service.call('memory_status', {}));
    for (const resource of resourceDefinitions) server.registerResource(resource.name, resource.uri,
      { description: resource.description, mimeType: 'application/json', cacheHint: { ttlMs: 0, cacheScope: 'private' } },
      () => contents(service, resource.uri));
    const remove = service.onChange(async () => {
      await server.server.sendResourceUpdated({ uri: CONTEXT_URI });
      await server.server.sendResourceUpdated({ uri: STATUS_URI });
    });
    server.server.onclose = remove;
    return server;
  }, { legacy: 'reject', maxSubscriptions: 8,
    transport: new BoundTransport(process.stdin, process.stdout, { maxBufferSize: 128 * 1024 }),
    onerror: () => console.error('memory-plugin: stdio protocol error') });
  const onEnd = () => service.close();
  process.stdin.once('end', onEnd);
  try { await service.start(); } catch (error) { await handle.close(); service.close(); throw error; }
  return { service, async close() { process.stdin.off('end', onEnd); service.close(); await handle.close(); } };
}

/** mcp-use 2.x exposes HTTP. There is deliberately no private #buildSdkServer access or fabricated stdio API. */
export async function createMemoryServer(config: Config, token: string) {
  if (!token || token.length < 32) throw new MemoryError('CONFIG', 'HTTP mode requires MEMORY_MCP_TOKEN of at least 32 characters');
  // The memory package never opts users into third-party usage reporting.
  // Use mcp-use's documented switch before its first module evaluation.
  process.env.MCP_USE_ANONYMIZED_TELEMETRY = 'false';
  const { MCPServer } = await import('mcp-use');
  const server = new MCPServer({ ...identity, instructions: INSTRUCTIONS, legacy: 'reject', skills: false,
    allowedHosts: ['localhost', '127.0.0.1', '[::1]'], allowedOrigins: [], logging: { enabled: false } });
  const service = new MemoryService(config);
  for (const definition of toolDefinitions) server.tool({ name: definition.name,
    description: definition.description, inputSchema: querySchema,
    annotations: { readOnlyHint: definition.readOnlyHint, destructiveHint: false, openWorldHint: true },
  }, (args, ctx) => service.call(definition.name, args, ctx.signal));
  server.tool({ name: 'memory_status', description: 'Read local binding/status without calling Hindsight.', inputSchema: empty,
    annotations: { readOnlyHint: true, destructiveHint: false, openWorldHint: false } }, () => service.call('memory_status', {}));
  for (const resource of resourceDefinitions) server.resource({ ...resource, mimeType: 'application/json' }, () => contents(service, resource.uri));
  const remove = service.onChange(async () => {
    await server.notifyResourceUpdated(CONTEXT_URI); await server.notifyResourceUpdated(STATUS_URI);
  });
  const hash = (value: string) => createHash('sha256').update(value).digest();
  const expected = hash(`Bearer ${token}`);
  await service.start();
  return {
    service,
    async fetch(request: Request) {
      if (new URL(request.url).pathname !== '/mcp') return new Response('Not found', { status: 404 });
      if (!timingSafeEqual(hash(request.headers.get('authorization') || ''), expected))
        return new Response('Unauthorized', { status: 401 });
      if (request.headers.has('origin')) return new Response('Browser origins are disabled', { status: 403 });
      if (request.body) {
        const reader = request.body.getReader(); const chunks: Uint8Array[] = []; let size = 0;
        try {
          while (true) {
            const chunk = await reader.read(); if (chunk.done) break;
            size += chunk.value.length;
            if (size > 128 * 1024) { await reader.cancel(); return new Response('Request too large', { status: 413 }); }
            chunks.push(chunk.value);
          }
        } finally { reader.releaseLock(); }
        const body = Buffer.concat(chunks).toString('utf8');
        let message: { id?: string | number } | undefined;
        try { message = JSON.parse(body); } catch { /* SDK owns malformed JSON errors. */ }
        if (message && unknownSubscription(message)) return Response.json({ jsonrpc: '2.0', id: message.id ?? null,
          error: { code: -32602, message: 'Unknown resource subscription' } });
        request = new Request(request.url, { method: request.method, headers: request.headers, body, signal: request.signal });
      }
      return server.fetch(request);
    },
    async close() { remove(); service.close(); await server.close(); },
  };
}
export async function startHttp(config: Config, token: string, port: number) {
  const handler = await createMemoryServer(config, token);
  const { toNodeHandler } = await import('mcp-use/node');
  const http = createNodeServer(toNodeHandler(handler));
  try { await new Promise<void>((resolve, reject) => { http.once('error', reject); http.listen(port, '127.0.0.1', resolve); }); }
  catch (error) { await handler.close(); throw error; }
  const address = http.address();
  return { port: typeof address === 'object' && address ? address.port : port,
    async close() { await handler.close(); http.closeAllConnections(); await new Promise<void>(resolve => http.close(() => resolve())); } };
}
