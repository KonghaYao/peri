import type { SwarmSandboxEntry } from './manager.js';

/** Hop-by-hop, outer-auth and browser-scoped headers that must not reach the inner MCP endpoint. */
const STRIPPED_REQUEST_HEADERS = [
  'authorization',
  'connection',
  'content-length',
  'cookie',
  'host',
  'keep-alive',
  'origin',
  'proxy-authenticate',
  'proxy-authorization',
  'te',
  'trailer',
  'transfer-encoding',
  'upgrade',
  'x-peri-sandbox-token',
  'x-peri-swarm-token',
  'x-rivet-token',
];

/**
 * Forward an outer HTTP MCP request to the instance's loopback endpoint.
 *
 * The outer `Authorization` belongs to the swarm server, so it is replaced by
 * the inner instance token. The response is returned as-is, preserving status,
 * headers and streaming bodies (including `subscriptions/listen` SSE).
 */
export async function forwardSandboxMcp(entry: SwarmSandboxEntry, request: Request): Promise<Response> {
  const headers = new Headers(request.headers);
  for (const name of STRIPPED_REQUEST_HEADERS) headers.delete(name);
  headers.set('authorization', `Bearer ${entry.innerToken}`);
  const bodyless = request.method === 'GET' || request.method === 'HEAD';
  const init: RequestInit & { duplex?: 'half' } = {
    method: request.method,
    headers,
    redirect: 'manual',
    signal: request.signal,
    ...(bodyless || request.body === null ? {} : { body: request.body, duplex: 'half' }),
  };
  return await fetch(entry.mcpUrl, init);
}
