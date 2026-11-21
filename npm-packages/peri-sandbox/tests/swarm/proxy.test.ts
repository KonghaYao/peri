import assert from 'node:assert/strict';
import { createServer, type IncomingHttpHeaders } from 'node:http';
import test from 'node:test';
import { SwarmManager } from '../../src/swarm/manager.js';
import { createSwarmRequestHandler } from '../../src/swarm/server.js';
import { createFakeStarter, temporarySwarmRoot } from './harness.js';

const BASE = 'http://127.0.0.1:8081';

interface CapturedRequest {
  readonly method: string;
  readonly url: string;
  readonly headers: IncomingHttpHeaders;
  readonly body: string;
}

interface InnerMcp {
  readonly url: string;
  readonly requests: CapturedRequest[];
  releaseStream(): void;
  close(): Promise<void>;
}

async function startInnerMcp(): Promise<InnerMcp> {
  const requests: CapturedRequest[] = [];
  let release: () => void = () => undefined;
  const streamGate = new Promise<void>((resolve) => {
    release = resolve;
  });
  const server = createServer((request, response) => {
    const chunks: Buffer[] = [];
    request.on('data', (chunk: Buffer) => chunks.push(chunk));
    request.on('end', () => {
      requests.push({
        method: request.method ?? '',
        url: request.url ?? '',
        headers: request.headers,
        body: Buffer.concat(chunks).toString('utf8'),
      });
      if (request.method === 'GET') {
        response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
        response.write('event: message\ndata: {"seq":1}\n\n');
        void streamGate.then(() => {
          response.write('event: message\ndata: {"seq":2}\n\n');
          response.end();
        });
        return;
      }
      response.writeHead(200, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ jsonrpc: '2.0', id: 1, method: request.method }));
    });
  });
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      server.off('error', reject);
      resolve();
    });
  });
  const address = server.address();
  if (address === null || typeof address === 'string') throw new Error('inner MCP server did not bind a port');
  return {
    url: `http://127.0.0.1:${address.port}/mcp`,
    requests,
    releaseStream: () => release(),
    close: () =>
      new Promise<void>((resolve, reject) => {
        server.closeAllConnections();
        server.close((error) => (error ? reject(error) : resolve()));
      }),
  };
}

function authorized(url: string, init: RequestInit = {}): Request {
  return new Request(url, {
    ...init,
    headers: { ...(init.headers as Record<string, string> | undefined), authorization: 'Bearer outer-secret' },
  });
}

function decode(value: Uint8Array | undefined): string {
  return value === undefined ? '' : new TextDecoder().decode(value);
}

function withTimeout<T>(promise: Promise<T>, milliseconds: number): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('timed out waiting for a stream chunk')), milliseconds);
    timer.unref();
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}

test('proxy replaces the outer Authorization and preserves method, path and body', async (t) => {
  const root = temporarySwarmRoot(t);
  const inner = await startInnerMcp();
  t.after(() => inner.close());
  const manager = new SwarmManager({
    root,
    startSandbox: createFakeStarter({ mcpUrl: () => inner.url }).start,
  });
  const handler = createSwarmRequestHandler({ manager, token: 'outer-secret' });
  try {
    const entry = await manager.ensure('env-post');
    const requestBody = '{"jsonrpc":"2.0","id":7,"method":"tools/list"}';
    const response = await handler(
      authorized(`${BASE}/sandboxes/env-post/mcp`, {
        method: 'POST',
        headers: { 'content-type': 'application/json', 'x-trace-id': 'trace-1' },
        body: requestBody,
      }),
    );
    assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), { jsonrpc: '2.0', id: 1, method: 'POST' });

    const seen = inner.requests.at(-1);
    assert.ok(seen !== undefined);
    assert.equal(seen.method, 'POST');
    assert.equal(seen.url, '/mcp');
    assert.equal(seen.body, requestBody);
    assert.equal(seen.headers.authorization, `Bearer ${entry.innerToken}`);
    assert.notEqual(seen.headers.authorization, 'Bearer outer-secret');
    assert.equal(seen.headers['x-trace-id'], 'trace-1');
    assert.equal(seen.headers.host, `127.0.0.1:${new URL(inner.url).port}`);

    const deleted = await handler(authorized(`${BASE}/sandboxes/env-post/mcp`, { method: 'DELETE' }));
    assert.equal(deleted.status, 200);
    const deleteSeen = inner.requests.at(-1);
    assert.ok(deleteSeen !== undefined);
    assert.equal(deleteSeen.method, 'DELETE');
    assert.equal(deleteSeen.headers.authorization, `Bearer ${entry.innerToken}`);
  } finally {
    await manager.closeAll();
  }
});

test('proxy streams the inner SSE body before the upstream response ends', async (t) => {
  const root = temporarySwarmRoot(t);
  const inner = await startInnerMcp();
  t.after(() => inner.close());
  const manager = new SwarmManager({
    root,
    startSandbox: createFakeStarter({ mcpUrl: () => inner.url }).start,
  });
  const handler = createSwarmRequestHandler({ manager, token: 'outer-secret' });
  try {
    await manager.ensure('env-stream');
    const response = await handler(
      authorized(`${BASE}/sandboxes/env-stream/mcp`, {
        method: 'GET',
        headers: { accept: 'text/event-stream' },
      }),
    );
    assert.equal(response.status, 200);
    assert.equal(response.headers.get('content-type'), 'text/event-stream');
    assert.ok(response.body !== null);
    const reader = response.body.getReader();
    const first = await withTimeout(reader.read(), 5_000);
    const firstText = decode(first.value);
    assert.match(firstText, /"seq":1/);
    assert.equal(firstText.includes('"seq":2'), false);

    inner.releaseStream();
    let rest = '';
    for (;;) {
      const next = await withTimeout(reader.read(), 5_000);
      if (next.done) break;
      rest += decode(next.value);
    }
    assert.match(rest, /"seq":2/);
  } finally {
    await manager.closeAll();
  }
});

test('proxy returns stable errors for missing sandboxes, methods and dead upstreams', async (t) => {
  const root = temporarySwarmRoot(t);
  const inner = await startInnerMcp();
  const innerUrl = inner.url;
  await inner.close();
  const manager = new SwarmManager({
    root,
    startSandbox: createFakeStarter({ mcpUrl: () => innerUrl }).start,
  });
  const handler = createSwarmRequestHandler({ manager, token: 'outer-secret' });
  try {
    const missing = await handler(authorized(`${BASE}/sandboxes/env-missing/mcp`, { method: 'POST' }));
    assert.equal(missing.status, 404);
    assert.equal(((await missing.json()) as { error: { code: string } }).error.code, 'sandbox_not_found');

    const method = await handler(authorized(`${BASE}/sandboxes/env-missing/mcp`, { method: 'PUT' }));
    assert.equal(method.status, 405);
    assert.equal(method.headers.get('allow'), 'POST, GET, DELETE');

    await manager.ensure('env-dead');
    const failed = await handler(
      authorized(`${BASE}/sandboxes/env-dead/mcp`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: '{}',
      }),
    );
    assert.equal(failed.status, 502);
    assert.equal(((await failed.json()) as { error: { code: string } }).error.code, 'mcp_unavailable');
  } finally {
    await manager.closeAll();
  }
});
