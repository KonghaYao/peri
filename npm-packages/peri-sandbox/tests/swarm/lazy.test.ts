import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import { deriveSandboxId } from '../../src/instance/options.js';
import { SandboxError } from '../../src/instance/types.js';
import { SwarmManager } from '../../src/swarm/manager.js';
import { createSwarmRequestHandler } from '../../src/swarm/server.js';
import { createFakeStarter, temporarySwarmRoot } from './harness.js';

const BASE = 'http://127.0.0.1:8081';

interface SandboxPayload {
  sandbox: {
    logicalEnvId: string;
    sandboxId: string;
    state: string;
    runId: string | null;
    workspacePath: string;
    mcpUrl: string | null;
    lastUsedAt: string;
    lastError: { code: string; message: string } | null;
  };
}

function reserveRequest(logicalEnvId: string, lazy?: boolean): Request {
  return new Request(`${BASE}/v1/sandboxes`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: 'Bearer outer-secret' },
    body: JSON.stringify({ logicalEnvId, ...(lazy === undefined ? {} : { lazy }) }),
  });
}

function mcpRequest(logicalEnvId: string, method = 'POST'): Request {
  return new Request(`${BASE}/sandboxes/${logicalEnvId}/mcp`, {
    method,
    headers: { authorization: 'Bearer outer-secret', 'content-type': 'application/json' },
    ...(method === 'POST'
      ? {
          body: '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"Read","arguments":{"file_path":"a.txt"}}}',
        }
      : {}),
  });
}

async function startInnerMcp(): Promise<{ url: string; close: () => Promise<void> }> {
  const server = createServer((request, response) => {
    request.resume();
    request.on('end', () => {
      response.writeHead(200, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ ok: true, method: request.method }));
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
  if (address === null || typeof address === 'string') throw new Error('inner MCP did not bind');
  return {
    url: `http://127.0.0.1:${address.port}/mcp`,
    close: () =>
      new Promise<void>((resolve, reject) => {
        server.closeAllConnections();
        server.close((error) => (error ? reject(error) : resolve()));
      }),
  };
}

test('lazy reservation allocates identity without starting the sandbox', async (t) => {
  const root = temporarySwarmRoot(t);
  const fake = createFakeStarter();
  const manager = new SwarmManager({ root, startSandbox: fake.start });
  const handler = createSwarmRequestHandler({ manager, token: 'outer-secret' });
  try {
    const response = await handler(reserveRequest('env-lazy', true));
    assert.equal(response.status, 200);
    const payload = (await response.json()) as SandboxPayload;
    assert.equal(fake.calls.length, 0);
    assert.equal(manager.get('env-lazy'), undefined);
    assert.equal(payload.sandbox.state, 'reserved');
    assert.equal(payload.sandbox.sandboxId, deriveSandboxId('env-lazy'));
    assert.equal(payload.sandbox.workspacePath, `${manager.root}/${payload.sandbox.sandboxId}/workspace`);
    assert.equal(payload.sandbox.mcpUrl, `${BASE}/sandboxes/env-lazy/mcp`);
    assert.equal(payload.sandbox.runId, null);

    const again = (await (await handler(reserveRequest('env-lazy', true))).json()) as SandboxPayload;
    assert.equal(again.sandbox.sandboxId, payload.sandbox.sandboxId);
    assert.equal(again.sandbox.workspacePath, payload.sandbox.workspacePath);
    assert.equal(fake.calls.length, 0);

    const fetched = (await (
      await handler(
        new Request(`${BASE}/v1/sandboxes/env-lazy`, {
          headers: { authorization: 'Bearer outer-secret' },
        }),
      )
    ).json()) as SandboxPayload;
    assert.equal(fetched.sandbox.state, 'reserved');
    assert.equal(fetched.sandbox.mcpUrl, `${BASE}/sandboxes/env-lazy/mcp`);
  } finally {
    await manager.closeAll();
  }
});

test('the first MCP request starts once and is forwarded', async (t) => {
  const root = temporarySwarmRoot(t);
  const inner = await startInnerMcp();
  t.after(() => inner.close());
  const fake = createFakeStarter({ mcpUrl: () => inner.url });
  const manager = new SwarmManager({ root, startSandbox: fake.start });
  const handler = createSwarmRequestHandler({ manager, token: 'outer-secret' });
  try {
    await handler(reserveRequest('env-first', true));
    assert.equal(fake.calls.length, 0);

    const response = await handler(mcpRequest('env-first'));
    assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), { ok: true, method: 'POST' });
    assert.equal(fake.calls.length, 1);

    const second = await handler(mcpRequest('env-first'));
    assert.equal(second.status, 200);
    assert.equal(fake.calls.length, 1);

    const view = (await (
      await handler(
        new Request(`${BASE}/v1/sandboxes/env-first`, {
          headers: { authorization: 'Bearer outer-secret' },
        }),
      )
    ).json()) as SandboxPayload;
    assert.equal(view.sandbox.state, 'ready');
    assert.equal(view.sandbox.runId, 'run_fake_1');
  } finally {
    await manager.closeAll();
  }
});

test('concurrent first requests share one start', async (t) => {
  const root = temporarySwarmRoot(t);
  const inner = await startInnerMcp();
  t.after(() => inner.close());
  const fake = createFakeStarter({ delayMs: 25, mcpUrl: () => inner.url });
  const manager = new SwarmManager({ root, startSandbox: fake.start });
  const handler = createSwarmRequestHandler({ manager, token: 'outer-secret' });
  try {
    await handler(reserveRequest('env-race', true));
    const [first, second] = await Promise.all([handler(mcpRequest('env-race')), handler(mcpRequest('env-race'))]);
    assert.equal(first.status, 200);
    assert.equal(second.status, 200);
    assert.equal(fake.calls.length, 1);
  } finally {
    await manager.closeAll();
  }
});

test('GET reports reserved, starting, ready and failed', async (t) => {
  const root = temporarySwarmRoot(t);
  const fake = createFakeStarter({ delayMs: 25 });
  const manager = new SwarmManager({ root, startSandbox: fake.start });
  try {
    await manager.reserve('env-states');
    assert.equal(manager.describe('env-states')?.state, 'reserved');

    const starting = manager.ensure('env-states');
    assert.equal(manager.describe('env-states')?.state, 'starting');
    await starting;
    assert.equal(manager.describe('env-states')?.state, 'ready');
  } finally {
    await manager.closeAll();
  }

  const failingRoot = temporarySwarmRoot(t);
  const failing = new SwarmManager({
    root: failingRoot,
    startSandbox: async () => {
      throw new SandboxError('start_failed', 'fake start failed');
    },
  });
  try {
    await failing.reserve('env-failed');
    await assert.rejects(failing.ensure('env-failed'), /fake start failed/);
    const description = failing.describe('env-failed');
    assert.equal(description?.state, 'failed');
    assert.equal(description?.lastError?.code, 'start_failed');
    assert.equal(description?.sandboxId, deriveSandboxId('env-failed'));
  } finally {
    await failing.closeAll();
  }
});

test('DELETE on a reservation keeps data unless deleteData=true', async (t) => {
  const root = temporarySwarmRoot(t);
  const manager = new SwarmManager({ root, startSandbox: createFakeStarter().start });
  const handler = createSwarmRequestHandler({ manager, token: 'outer-secret' });
  try {
    await handler(reserveRequest('env-delete', true));
    const kept = await handler(
      new Request(`${BASE}/v1/sandboxes/env-delete`, {
        method: 'DELETE',
        headers: { authorization: 'Bearer outer-secret' },
      }),
    );
    assert.equal(kept.status, 200);
    assert.deepEqual(await kept.json(), { logicalEnvId: 'env-delete', closed: false, deletedData: false });
    assert.equal(manager.describe('env-delete')?.state, 'reserved');
    assert.equal(manager.describe('env-delete')?.runId, null);

    await handler(reserveRequest('env-delete', true));
    const deleted = await handler(
      new Request(`${BASE}/v1/sandboxes/env-delete?deleteData=true`, {
        method: 'DELETE',
        headers: { authorization: 'Bearer outer-secret' },
      }),
    );
    assert.deepEqual(await deleted.json(), { logicalEnvId: 'env-delete', closed: false, deletedData: true });
    assert.equal(manager.describe('env-delete'), undefined);
  } finally {
    await manager.closeAll();
  }
});

test('a reserved binding survives a restart and still starts on the first request', async (t) => {
  const root = temporarySwarmRoot(t);
  const first = new SwarmManager({ root, startSandbox: createFakeStarter().start });
  await first.reserve('env-restart');
  await first.closeAll();

  const inner = await startInnerMcp();
  t.after(() => inner.close());
  const fake = createFakeStarter({ mcpUrl: () => inner.url });
  const restarted = new SwarmManager({ root, startSandbox: fake.start });
  const handler = createSwarmRequestHandler({ manager: restarted, token: 'outer-secret' });
  try {
    const view = (await (
      await handler(
        new Request(`${BASE}/v1/sandboxes/env-restart`, {
          headers: { authorization: 'Bearer outer-secret' },
        }),
      )
    ).json()) as SandboxPayload;
    assert.equal(view.sandbox.state, 'reserved');
    assert.equal(fake.calls.length, 0);

    const response = await handler(mcpRequest('env-restart'));
    assert.equal(response.status, 200);
    assert.equal(fake.calls.length, 1);
    assert.equal(restarted.describe('env-restart')?.state, 'ready');
  } finally {
    await restarted.closeAll();
  }
});

test('lazy reserve reopens an environment that was closed before', async (t) => {
  const root = temporarySwarmRoot(t);
  const inner = await startInnerMcp();
  t.after(() => inner.close());
  const fake = createFakeStarter({ mcpUrl: () => inner.url });
  const manager = new SwarmManager({ root, startSandbox: fake.start });
  try {
    const first = await manager.ensure('env-reopen');
    await manager.close('env-reopen', { deleteData: false });
    assert.equal(manager.describe('env-reopen')?.state, 'stopped');

    const reserved = await manager.reserve('env-reopen');
    assert.equal(reserved.state, 'reserved');
    assert.equal(reserved.sandboxId, first.sandboxId);
    assert.equal(reserved.workspacePath, first.workspacePath);
    assert.equal(fake.calls.length, 1);

    const entry = await manager.ensure('env-reopen');
    assert.equal(entry.sandboxId, first.sandboxId);
    assert.notEqual(entry.runId, first.runId);
    assert.equal(fake.calls.length, 2);
  } finally {
    await manager.closeAll();
  }
});

test('lazy:false keeps the S7 eager start behavior', async (t) => {
  const root = temporarySwarmRoot(t);
  const fake = createFakeStarter();
  const manager = new SwarmManager({ root, startSandbox: fake.start });
  const handler = createSwarmRequestHandler({ manager, token: 'outer-secret' });
  try {
    const response = await handler(reserveRequest('env-eager', false));
    assert.equal(response.status, 200);
    assert.equal(fake.calls.length, 1);
    const payload = (await response.json()) as SandboxPayload;
    assert.equal(payload.sandbox.state, 'ready');
  } finally {
    await manager.closeAll();
  }
});

test('shared idle workspace wakes once at its existing MCP URL and respects explicit stop and delete', async (t) => {
  let now = 0;
  const inner = await startInnerMcp();
  t.after(() => inner.close());
  const fake = createFakeStarter({ delayMs: 15, mcpUrl: () => inner.url });
  const manager = new SwarmManager({
    root: temporarySwarmRoot(t),
    startSandbox: fake.start,
    now: () => new Date(now),
    idleMs: 100,
  });
  const handler = createSwarmRequestHandler({ manager, token: 'outer-secret' });
  const id = 'env-shared';
  const stop = () =>
    handler(
      new Request(`${BASE}/v1/sandboxes/${id}`, {
        method: 'DELETE',
        headers: { authorization: 'Bearer outer-secret' },
      }),
    );
  try {
    const first = await manager.ensure(id);
    const marker = join(first.workspacePath, 'saved.txt');
    writeFileSync(marker, 'before reclaim');
    now = 100;
    assert.deepEqual((await manager.sweepIdle()).reclaimed, [id]);
    const responses = await Promise.all([handler(mcpRequest(id)), handler(mcpRequest(id))]);
    for (const response of responses) {
      assert.equal(response.status, 200);
      await response.json();
    }
    const awake = manager.get(id)!;
    assert.notEqual(awake.runId, first.runId);
    assert.equal(awake.workspacePath, first.workspacePath);
    assert.equal(awake.sandboxId, first.sandboxId);
    assert.equal(readFileSync(marker, 'utf8'), 'before reclaim');
    assert.equal(fake.calls.length, 2);
    now += 100;
    await manager.sweepIdle();
    assert.equal((await stop()).status, 200);
    assert.equal(manager.describe(id)?.state, 'stopped');
    const stopped = await handler(mcpRequest(id));
    assert.equal(stopped.status, 404);
    await stopped.text();
    await manager.ensure(id);
    await manager.close(id, { deleteData: true });
    const deleted = await handler(mcpRequest(id));
    assert.equal(deleted.status, 404);
    await deleted.text();
    assert.equal(fake.calls.length, 3);
  } finally {
    await manager.closeAll();
  }
});

test('deletion while a cold tool request body is arriving cannot recreate the environment', async (t) => {
  const fake = createFakeStarter();
  const manager = new SwarmManager({ root: temporarySwarmRoot(t), startSandbox: fake.start });
  const handler = createSwarmRequestHandler({ manager });
  await manager.reserve('env-slow');
  let body!: ReadableStreamDefaultController<Uint8Array>;
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      body = controller;
    },
  });
  const request = new Request(`${BASE}/sandboxes/env-slow/mcp`, {
    method: 'POST',
    body: stream,
    duplex: 'half',
  } as RequestInit);
  const waiting = handler(request);
  await new Promise((resolve) => setImmediate(resolve));
  await manager.close('env-slow', { deleteData: true });
  body.enqueue(new TextEncoder().encode('{"jsonrpc":"2.0","id":1,"method":"tools/call"}'));
  body.close();
  const response = await waiting;
  assert.equal(response.status, 404);
  await response.text();
  assert.equal(fake.calls.length, 0);
  assert.equal(manager.binding('env-slow'), undefined);
});
