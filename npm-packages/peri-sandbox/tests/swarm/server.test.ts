import assert from 'node:assert/strict';
import { existsSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import { SwarmError } from '../../src/swarm/errors.js';
import { SwarmManager } from '../../src/swarm/manager.js';
import { createSwarmRequestHandler } from '../../src/swarm/server.js';
import { createFakeStarter, temporarySwarmRoot } from './harness.js';

const BASE = 'http://127.0.0.1:8081';

interface SandboxPayload {
  sandbox: {
    logicalEnvId: string;
    sandboxId: string;
    state: string;
    runId: string;
    workspacePath: string;
    mcpUrl: string | null;
    lastUsedAt: string;
  };
}

function ensureRequest(logicalEnvId: string, token?: string): Request {
  return new Request(`${BASE}/v1/sandboxes`, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      ...(token === undefined ? {} : { authorization: `Bearer ${token}` }),
    },
    body: JSON.stringify({ logicalEnvId }),
  });
}

function authorized(url: string, init: RequestInit = {}): Request {
  return new Request(url, {
    ...init,
    headers: { ...(init.headers as Record<string, string> | undefined), authorization: 'Bearer outer-secret' },
  });
}

test('without a configured token the API is open', async (t) => {
  const root = temporarySwarmRoot(t);
  const fake = createFakeStarter();
  const manager = new SwarmManager({ root, startSandbox: fake.start });
  const handler = createSwarmRequestHandler({ manager });
  try {
    const health = await handler(new Request(`${BASE}/healthz`));
    assert.equal(health.status, 200);
    assert.deepEqual(await health.json(), { status: 'ok' });

    const created = await handler(ensureRequest('env-open'));
    assert.equal(created.status, 200);
    const payload = (await created.json()) as SandboxPayload;
    const entry = manager.get('env-open');
    assert.ok(entry !== undefined);
    assert.equal(payload.sandbox.logicalEnvId, 'env-open');
    assert.equal(payload.sandbox.sandboxId, entry.sandboxId);
    assert.equal(payload.sandbox.state, 'ready');
    assert.equal(payload.sandbox.runId, entry.runId);
    assert.equal(payload.sandbox.workspacePath, entry.workspacePath);
    assert.equal(payload.sandbox.mcpUrl, `${BASE}/sandboxes/env-open/mcp`);
    assert.equal(JSON.stringify(payload).includes(entry.innerToken), false);
    assert.equal('innerToken' in payload.sandbox, false);
  } finally {
    await manager.closeAll();
  }
});

test('a configured token guards /v1 and /sandboxes but not /healthz', async (t) => {
  const root = temporarySwarmRoot(t);
  const fake = createFakeStarter();
  const manager = new SwarmManager({ root, startSandbox: fake.start });
  const handler = createSwarmRequestHandler({ manager, token: 'outer-secret' });
  try {
    const health = await handler(new Request(`${BASE}/healthz`));
    assert.equal(health.status, 200);

    const missing = await handler(ensureRequest('env-guarded'));
    assert.equal(missing.status, 401);
    assert.deepEqual(await missing.json(), {
      error: { code: 'unauthorized', message: 'missing or invalid bearer token.' },
    });

    const wrong = await handler(ensureRequest('env-guarded', 'wrong-token'));
    assert.equal(wrong.status, 401);

    const proxy = await handler(new Request(`${BASE}/sandboxes/env-guarded/mcp`, { method: 'POST' }));
    assert.equal(proxy.status, 401);
    assert.equal(fake.calls.length, 0);

    const allowed = await handler(ensureRequest('env-guarded', 'outer-secret'));
    assert.equal(allowed.status, 200);
    assert.equal(fake.calls.length, 1);

    const unknown = await handler(authorized(`${BASE}/v1/sandboxes/env-missing`));
    assert.equal(unknown.status, 404);
    const body = (await unknown.json()) as { error: { code: string } };
    assert.equal(body.error.code, 'sandbox_not_found');
  } finally {
    await manager.closeAll();
  }
});

test('DELETE keeps the workspace unless deleteData=true, and re-ensure starts a new run', async (t) => {
  const root = temporarySwarmRoot(t);
  const fake = createFakeStarter();
  const manager = new SwarmManager({ root, startSandbox: fake.start });
  const handler = createSwarmRequestHandler({ manager, token: 'outer-secret' });
  try {
    const created = (await (await handler(ensureRequest('env-lifecycle', 'outer-secret'))).json()) as SandboxPayload;
    const first = manager.get('env-lifecycle');
    assert.ok(first !== undefined);
    const marker = join(first.workspacePath, 'keep.txt');
    writeFileSync(marker, 'kept');

    const invalidQuery = await handler(
      authorized(`${BASE}/v1/sandboxes/env-lifecycle?deleteData=maybe`, { method: 'DELETE' }),
    );
    assert.equal(invalidQuery.status, 400);

    const stopped = await handler(authorized(`${BASE}/v1/sandboxes/env-lifecycle`, { method: 'DELETE' }));
    assert.equal(stopped.status, 200);
    assert.deepEqual(await stopped.json(), { logicalEnvId: 'env-lifecycle', closed: true, deletedData: false });
    assert.equal(existsSync(first.instance.sandboxRoot), true);
    assert.equal(existsSync(marker), true);

    const stoppedView = (await (
      await handler(authorized(`${BASE}/v1/sandboxes/env-lifecycle`))
    ).json()) as SandboxPayload;
    assert.equal(stoppedView.sandbox.state, 'stopped');
    assert.equal(stoppedView.sandbox.mcpUrl, null);
    assert.equal(stoppedView.sandbox.runId, created.sandbox.runId);
    assert.equal(stoppedView.sandbox.workspacePath, created.sandbox.workspacePath);

    const restarted = (await (await handler(ensureRequest('env-lifecycle', 'outer-secret'))).json()) as SandboxPayload;
    assert.notEqual(restarted.sandbox.runId, created.sandbox.runId);
    assert.equal(restarted.sandbox.sandboxId, created.sandbox.sandboxId);
    assert.equal(restarted.sandbox.workspacePath, created.sandbox.workspacePath);
    assert.equal(existsSync(marker), true);

    const deleted = await handler(
      authorized(`${BASE}/v1/sandboxes/env-lifecycle?deleteData=true`, { method: 'DELETE' }),
    );
    assert.equal(deleted.status, 200);
    assert.deepEqual(await deleted.json(), { logicalEnvId: 'env-lifecycle', closed: true, deletedData: true });
    assert.equal(existsSync(first.instance.sandboxRoot), false);
    assert.equal(manager.binding('env-lifecycle'), undefined);

    const gone = await handler(authorized(`${BASE}/v1/sandboxes/env-lifecycle`));
    assert.equal(gone.status, 404);
  } finally {
    await manager.closeAll();
  }
});

test('invalid requests and routes return stable JSON errors', async (t) => {
  const root = temporarySwarmRoot(t);
  const manager = new SwarmManager({ root, startSandbox: createFakeStarter().start });
  const handler = createSwarmRequestHandler({ manager });
  try {
    const notJson = await handler(
      new Request(`${BASE}/v1/sandboxes`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: '{',
      }),
    );
    assert.equal(notJson.status, 400);
    assert.equal(((await notJson.json()) as { error: { code: string } }).error.code, 'invalid_request');

    const missingId = await handler(
      new Request(`${BASE}/v1/sandboxes`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({}),
      }),
    );
    assert.equal(missingId.status, 400);

    const invalidId = await handler(ensureRequest('../escape'));
    assert.equal(invalidId.status, 400);
    assert.equal(((await invalidId.json()) as { error: { code: string } }).error.code, 'invalid_logical_env_id');

    const unknownRoute = await handler(new Request(`${BASE}/v1/unknown`));
    assert.equal(unknownRoute.status, 404);
    assert.equal(((await unknownRoute.json()) as { error: { code: string } }).error.code, 'not_found');

    const wrongMethod = await handler(new Request(`${BASE}/v1/sandboxes`, { method: 'GET' }));
    assert.equal(wrongMethod.status, 405);
    assert.equal(wrongMethod.headers.get('allow'), 'POST');

    const healthWrongMethod = await handler(new Request(`${BASE}/healthz`, { method: 'POST' }));
    assert.equal(healthWrongMethod.status, 405);
  } finally {
    await manager.closeAll();
  }
});

test('an empty token configuration is rejected', (t) => {
  const root = temporarySwarmRoot(t);
  const manager = new SwarmManager({ root, startSandbox: createFakeStarter().start });
  assert.throws(
    () => createSwarmRequestHandler({ manager, token: '' }),
    (error: unknown) => error instanceof SwarmError && error.code === 'invalid_config',
  );
});
