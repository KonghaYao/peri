import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import test from 'node:test';
import {
  COLD_SNAPSHOT_META_KEY,
  COLD_WORKSPACE_PROVIDER_VERSION,
  COLD_WORKSPACE_SNAPSHOT,
} from '../../src/swarm/cold-tools.js';
import { SwarmManager } from '../../src/swarm/manager.js';
import { createSwarmRequestHandler } from '../../src/swarm/server.js';
import { createFakeStarter, temporarySwarmRoot } from './harness.js';

const BASE = 'http://127.0.0.1:8081';
const EXPECTED_TOOL_NAMES = ['Read', 'Write', 'Edit', 'Glob', 'Grep', 'folder_operations', 'Bash'];

interface JsonRpcResult {
  jsonrpc: string;
  id: unknown;
  result: Record<string, unknown>;
}

interface CapturedRequest {
  readonly url: string;
  readonly body: string;
  readonly authorization: string | undefined;
}

interface InnerMcp {
  readonly url: string;
  readonly requests: CapturedRequest[];
  close(): Promise<void>;
}

interface InnerMcpOptions {
  readonly providerVersion?: string;
  readonly tools?: readonly Record<string, unknown>[];
}

function jsonRpc(id: unknown, result: Record<string, unknown>): string {
  return JSON.stringify({ jsonrpc: '2.0', id, result });
}

function parseMethod(body: string): string | null {
  try {
    const parsed = JSON.parse(body) as { method?: unknown };
    return typeof parsed.method === 'string' ? parsed.method : null;
  } catch {
    return null;
  }
}

/** Fake inner provider: answers discover/tools-list and echoes every other call. */
async function startInnerMcp(options: InnerMcpOptions = {}): Promise<InnerMcp> {
  const requests: CapturedRequest[] = [];
  const server = createServer((request, response) => {
    const chunks: Buffer[] = [];
    request.on('data', (chunk: Buffer) => chunks.push(chunk));
    request.on('end', () => {
      const body = Buffer.concat(chunks).toString('utf8');
      requests.push({ url: request.url ?? '', body, authorization: request.headers.authorization });
      const parsed = JSON.parse(body) as { id?: unknown; method?: unknown };
      const id = parsed.id ?? null;
      if (parsed.method === 'server/discover' && options.providerVersion !== undefined) {
        response.writeHead(200, { 'content-type': 'application/json' });
        response.end(
          jsonRpc(id, {
            resultType: 'complete',
            supportedVersions: ['2026-07-28'],
            capabilities: { tools: {} },
            ttlMs: 0,
            cacheScope: 'private',
            _meta: {
              'io.modelcontextprotocol/serverInfo': { name: 'peri', version: options.providerVersion },
            },
          }),
        );
        return;
      }
      if (parsed.method === 'tools/list' && options.tools !== undefined) {
        response.writeHead(200, { 'content-type': 'application/json' });
        response.end(jsonRpc(id, { tools: options.tools }));
        return;
      }
      if (parsed.method === 'tools/list') {
        response.writeHead(200, { 'content-type': 'application/json' });
        response.end(jsonRpc(id, { tools: [] }));
        return;
      }
      response.writeHead(200, { 'content-type': 'application/json' });
      response.end(jsonRpc(id, { content: [{ type: 'text', text: 'forwarded from inner' }] }));
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
    requests,
    close: () =>
      new Promise<void>((resolve, reject) => {
        server.closeAllConnections();
        server.close((error) => (error ? reject(error) : resolve()));
      }),
  };
}

function authorized(logicalEnvId: string, body?: unknown, method = 'POST'): Request {
  return new Request(`${BASE}/sandboxes/${logicalEnvId}/mcp`, {
    method,
    headers: { authorization: 'Bearer outer-secret', 'content-type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
}

function reserveRequest(logicalEnvId: string): Request {
  return new Request(`${BASE}/v1/sandboxes`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: 'Bearer outer-secret' },
    body: JSON.stringify({ logicalEnvId, lazy: true }),
  });
}

function diagnosticsOf(result: Record<string, unknown>): Record<string, unknown> {
  const meta = result._meta;
  assert.ok(meta !== null && typeof meta === 'object' && !Array.isArray(meta), 'result._meta is missing');
  const diagnostics = (meta as Record<string, unknown>)['peri.swarmDiagnostics'];
  assert.ok(
    diagnostics !== null && typeof diagnostics === 'object' && !Array.isArray(diagnostics),
    'peri.swarmDiagnostics is missing',
  );
  return diagnostics as Record<string, unknown>;
}

test('cold initialize, discover, ping and tools/list never start the sandbox', async (t) => {
  const root = temporarySwarmRoot(t);
  const fake = createFakeStarter();
  const manager = new SwarmManager({ root, startSandbox: fake.start });
  const handler = createSwarmRequestHandler({ manager, token: 'outer-secret' });
  try {
    const reserved = await handler(reserveRequest('env-cold'));
    assert.equal(reserved.status, 200);
    assert.equal(fake.calls.length, 0);

    const initialize = await handler(
      authorized('env-cold', {
        jsonrpc: '2.0',
        id: 1,
        method: 'initialize',
        params: {
          protocolVersion: '2026-07-28',
          capabilities: {},
          clientInfo: { name: 'cold-test', version: '0.0.0' },
        },
      }),
    );
    assert.equal(initialize.status, 200);
    assert.equal(initialize.headers.get('mcp-protocol-version'), '2026-07-28');
    const initialized = (await initialize.json()) as JsonRpcResult;
    assert.equal(initialized.result.protocolVersion, '2026-07-28');
    assert.deepEqual(initialized.result.capabilities, { tools: { listChanged: false } });
    assert.deepEqual(initialized.result.serverInfo, {
      name: COLD_WORKSPACE_SNAPSHOT.providerName,
      version: COLD_WORKSPACE_PROVIDER_VERSION,
    });
    const initializeMeta = initialized.result._meta as Record<string, unknown>;
    assert.equal(initializeMeta[COLD_SNAPSHOT_META_KEY], true);
    const initializeDiagnostics = diagnosticsOf(initialized.result);
    assert.deepEqual(initializeDiagnostics.providerVersion, {
      expected: COLD_WORKSPACE_PROVIDER_VERSION,
      actual: COLD_WORKSPACE_PROVIDER_VERSION,
      consistent: true,
    });

    const notification = await handler(authorized('env-cold', { jsonrpc: '2.0', method: 'notifications/initialized' }));
    assert.equal(notification.status, 202);
    assert.equal(await notification.text(), '');

    const discover = (await (
      await handler(authorized('env-cold', { jsonrpc: '2.0', id: 2, method: 'server/discover', params: {} }))
    ).json()) as JsonRpcResult;
    assert.deepEqual(discover.result.supportedVersions, ['2026-07-28']);
    // 2026-07-28 `DiscoverResult` fields; rmcp rejects the result as an unknown
    // `CustomResult` when any of them is absent.
    assert.equal(discover.result.resultType, 'complete');
    assert.equal(discover.result.ttlMs, 0);
    assert.equal(discover.result.cacheScope, 'private');
    assert.deepEqual(discover.result.capabilities, { tools: { listChanged: false } });
    assert.equal((discover.result._meta as Record<string, unknown>)[COLD_SNAPSHOT_META_KEY], true);

    const ping = (await (
      await handler(authorized('env-cold', { jsonrpc: '2.0', id: 3, method: 'ping' }))
    ).json()) as JsonRpcResult;
    assert.deepEqual(ping.result, { resultType: 'complete' });

    const list = (await (
      await handler(authorized('env-cold', { jsonrpc: '2.0', id: 4, method: 'tools/list', params: {} }))
    ).json()) as JsonRpcResult;
    assert.equal(list.result.resultType, 'complete');
    assert.equal(list.result.ttlMs, 0);
    assert.equal(list.result.cacheScope, 'private');
    const tools = list.result.tools as Array<Record<string, unknown>>;
    assert.equal(tools.length, 7);
    assert.deepEqual(
      tools.map((tool) => tool.name),
      EXPECTED_TOOL_NAMES,
    );
    const listMeta = list.result._meta as Record<string, unknown>;
    assert.equal(listMeta[COLD_SNAPSHOT_META_KEY], true);
    assert.equal(listMeta.providerVersion, COLD_WORKSPACE_PROVIDER_VERSION);
    assert.equal(listMeta['peri.swarmProviderVersion'], COLD_WORKSPACE_PROVIDER_VERSION);
    assert.deepEqual(diagnosticsOf(list.result).providerVersion, {
      expected: COLD_WORKSPACE_PROVIDER_VERSION,
      actual: COLD_WORKSPACE_PROVIDER_VERSION,
      consistent: true,
    });

    assert.equal(fake.calls.length, 0);
    assert.equal(manager.get('env-cold'), undefined);
    assert.equal(manager.describe('env-cold')?.state, 'reserved');
  } finally {
    await manager.closeAll();
  }
});

test('the first tools/call starts exactly once and is forwarded', async (t) => {
  const root = temporarySwarmRoot(t);
  const inner = await startInnerMcp({ providerVersion: COLD_WORKSPACE_PROVIDER_VERSION });
  t.after(() => inner.close());
  const fake = createFakeStarter({ mcpUrl: () => inner.url });
  const manager = new SwarmManager({ root, startSandbox: fake.start });
  const handler = createSwarmRequestHandler({ manager, token: 'outer-secret' });
  try {
    await handler(reserveRequest('env-call'));
    assert.equal(fake.calls.length, 0);

    const response = await handler(
      authorized('env-call', {
        jsonrpc: '2.0',
        id: 8,
        method: 'tools/call',
        params: { name: 'Read', arguments: { file_path: 'hello.txt' } },
      }),
    );
    assert.equal(response.status, 200);
    const payload = (await response.json()) as JsonRpcResult;
    assert.deepEqual(payload.result.content, [{ type: 'text', text: 'forwarded from inner' }]);
    assert.equal(fake.calls.length, 1);

    const forwarded = inner.requests.filter((request) => parseMethod(request.body) === 'tools/call');
    assert.equal(forwarded.length, 1);
    assert.match(forwarded[0]?.body ?? '', /"name":"Read"/);
    assert.equal(forwarded[0]?.authorization, 'Bearer inner-token-1');

    const view = (await (
      await handler(new Request(`${BASE}/v1/sandboxes/env-call`, { headers: { authorization: 'Bearer outer-secret' } }))
    ).json()) as { sandbox: { state: string; providerVersion: string | null } };
    assert.equal(view.sandbox.state, 'ready');
    assert.equal(view.sandbox.providerVersion, COLD_WORKSPACE_PROVIDER_VERSION);

    // After the first start, tools/list is served by the real instance so hot
    // clients never observe a stale snapshot.
    const hotList = (await (
      await handler(authorized('env-call', { jsonrpc: '2.0', id: 9, method: 'tools/list', params: {} }))
    ).json()) as JsonRpcResult;
    assert.deepEqual(hotList.result.tools, []);
    assert.equal(hotList.result._meta, undefined);
    assert.equal(inner.requests.filter((request) => parseMethod(request.body) === 'tools/list').length, 1);
  } finally {
    await manager.closeAll();
  }
});

test('concurrent tools/call requests share one start and one version probe', async (t) => {
  const root = temporarySwarmRoot(t);
  const inner = await startInnerMcp({ providerVersion: COLD_WORKSPACE_PROVIDER_VERSION });
  t.after(() => inner.close());
  const fake = createFakeStarter({ delayMs: 25, mcpUrl: () => inner.url });
  const manager = new SwarmManager({ root, startSandbox: fake.start });
  const handler = createSwarmRequestHandler({ manager, token: 'outer-secret' });
  try {
    await handler(reserveRequest('env-race'));
    const call = (id: number) =>
      handler(
        authorized('env-race', {
          jsonrpc: '2.0',
          id,
          method: 'tools/call',
          params: { name: 'Bash', arguments: { command: 'true' } },
        }),
      );
    const [first, second] = await Promise.all([call(1), call(2)]);
    assert.equal(first.status, 200);
    assert.equal(second.status, 200);
    assert.equal(fake.calls.length, 1);
    assert.equal(inner.requests.filter((request) => parseMethod(request.body) === 'server/discover').length, 1);
  } finally {
    await manager.closeAll();
  }
});

test('the generated snapshot contains exactly seven tools with non-empty schemas', () => {
  assert.equal(COLD_WORKSPACE_SNAPSHOT.tools.length, 7);
  assert.deepEqual(
    COLD_WORKSPACE_SNAPSHOT.tools.map((tool) => tool.name),
    EXPECTED_TOOL_NAMES,
  );
  assert.ok(COLD_WORKSPACE_PROVIDER_VERSION.length > 0);
  assert.match(COLD_WORKSPACE_SNAPSHOT.sourceCommand, /peri mcp-start .*--capability workspace/);
  assert.match(COLD_WORKSPACE_SNAPSHOT.toolsDigest, /^sha256:[0-9a-f]{64}$/);
  for (const tool of COLD_WORKSPACE_SNAPSHOT.tools) {
    assert.ok(tool.name.length > 0);
    assert.ok(tool.description.trim().length > 0, `${tool.name} has an empty description`);
    assert.equal(tool.inputSchema.type, 'object', `${tool.name} inputSchema.type`);
    const properties = tool.inputSchema.properties;
    assert.ok(
      properties !== null && typeof properties === 'object' && Object.keys(properties).length > 0,
      `${tool.name} has no inputSchema properties`,
    );
  }
});

test('version diagnostics report expected and actual without rejecting the tool surface', async (t) => {
  const warnings: string[] = [];
  t.mock.method(console, 'warn', (...args: unknown[]) => {
    warnings.push(args.map((value) => (typeof value === 'string' ? value : JSON.stringify(value))).join(' '));
  });
  const root = temporarySwarmRoot(t);
  const manager = new SwarmManager({ root, startSandbox: createFakeStarter().start });
  const handler = createSwarmRequestHandler({
    manager,
    token: 'outer-secret',
    workspaceProviderVersion: '0.0.0-other',
  });
  try {
    await handler(reserveRequest('env-mismatch'));
    const response = (await (
      await handler(authorized('env-mismatch', { jsonrpc: '2.0', id: 1, method: 'tools/list', params: {} }))
    ).json()) as JsonRpcResult;
    assert.equal((response.result.tools as unknown[]).length, 7);
    assert.deepEqual(diagnosticsOf(response.result).providerVersion, {
      expected: '0.0.0-other',
      actual: COLD_WORKSPACE_PROVIDER_VERSION,
      consistent: false,
    });
    assert.ok(warnings.length > 0);
    for (const warning of warnings) {
      assert.equal(warning.includes('outer-secret'), false);
      assert.equal(warning.includes('http://'), false);
      assert.equal(warning.includes('/sandboxes/'), false);
    }
  } finally {
    await manager.closeAll();
  }
});

test('cold GET and DELETE do not start the sandbox', async (t) => {
  const root = temporarySwarmRoot(t);
  const fake = createFakeStarter();
  const manager = new SwarmManager({ root, startSandbox: fake.start });
  const handler = createSwarmRequestHandler({ manager, token: 'outer-secret' });
  try {
    await handler(reserveRequest('env-stream-cold'));
    for (const method of ['GET', 'DELETE']) {
      const response = await handler(authorized('env-stream-cold', undefined, method));
      assert.equal(response.status, 405);
      assert.equal(response.headers.get('allow'), 'POST');
      assert.equal(((await response.json()) as { error: { code: string } }).error.code, 'sandbox_cold');
    }
    assert.equal(fake.calls.length, 0);
  } finally {
    await manager.closeAll();
  }
});

function resolvePeriBinary(): string | null {
  const candidates = [
    process.env.PERI_SNAPSHOT_PERI_BIN,
    resolve(process.cwd(), '../../target/debug/peri'),
    resolve(process.cwd(), '../../target/release/peri'),
    resolve(process.cwd(), 'target/debug/peri'),
  ].filter((candidate): candidate is string => candidate !== undefined);
  for (const candidate of candidates) if (existsSync(candidate)) return candidate;
  return null;
}

/** Real provider over stdio; the same crate the sandbox serves, without SRT. */
function listRealProviderTools(
  periPath: string,
  workspace: string,
): Promise<{
  serverInfo: Record<string, unknown>;
  tools: Array<Record<string, unknown>>;
}> {
  return new Promise((resolvePromise, reject) => {
    const child = spawn(
      periPath,
      ['mcp-start', 'peri', '--capability', 'workspace', '--stdio', '--workspace', workspace],
      {
        stdio: ['pipe', 'pipe', 'pipe'],
      },
    );
    let buffer = '';
    let stderr = '';
    let settled = false;
    let serverInfo: Record<string, unknown> = {};
    const timer = setTimeout(() => finish(new Error(`real provider timed out: ${stderr.slice(-1000)}`)), 20_000);
    timer.unref();
    function finish(
      error: Error | undefined,
      value?: { serverInfo: Record<string, unknown>; tools: Array<Record<string, unknown>> },
    ) {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (!child.killed) child.kill('SIGKILL');
      if (error === undefined && value !== undefined) resolvePromise(value);
      else reject(error);
    }
    const send = (message: unknown) => child.stdin.write(`${JSON.stringify(message)}\n`);
    child.once('error', (error) => finish(error));
    child.once('exit', (code, signal) => {
      if (!settled)
        finish(new Error(`real provider exited early (code=${code}, signal=${signal}): ${stderr.slice(-1000)}`));
    });
    child.stderr.on('data', (chunk) => {
      stderr += chunk.toString('utf8');
    });
    child.stdout.on('data', (chunk) => {
      buffer += chunk.toString('utf8');
      for (let newline = buffer.indexOf('\n'); newline >= 0; newline = buffer.indexOf('\n')) {
        const line = buffer.slice(0, newline).trim();
        buffer = buffer.slice(newline + 1);
        if (line.length === 0) continue;
        let message: { id?: unknown; result?: Record<string, unknown>; error?: unknown };
        try {
          message = JSON.parse(line);
        } catch {
          continue;
        }
        if (message.id === 1 && message.result !== undefined) {
          serverInfo = (message.result.serverInfo as Record<string, unknown>) ?? {};
          send({ jsonrpc: '2.0', method: 'notifications/initialized' });
          send({ jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} });
        } else if (message.id === 2) {
          if (message.error !== undefined) finish(new Error(`tools/list failed: ${JSON.stringify(message.error)}`));
          else
            finish(undefined, { serverInfo, tools: (message.result?.tools as Array<Record<string, unknown>>) ?? [] });
        }
      }
    });
    send({
      jsonrpc: '2.0',
      id: 1,
      method: 'initialize',
      params: {
        protocolVersion: '2026-07-28',
        capabilities: {},
        clientInfo: { name: 'cold-hot-consistency', version: '0.0.0' },
      },
    });
  });
}

test('cold snapshot agrees with a real hot workspace provider', async (t) => {
  const periPath = resolvePeriBinary();
  if (periPath === null) {
    t.skip('no peri binary; build one with ./scripts/cargo-rmcp-patched.sh build -p peri-tui --bin peri');
    return;
  }
  const root = temporarySwarmRoot(t);
  const manager = new SwarmManager({ root, startSandbox: createFakeStarter().start });
  const handler = createSwarmRequestHandler({ manager, token: 'outer-secret' });
  try {
    await handler(reserveRequest('env-agree'));
    const cold = (await (
      await handler(authorized('env-agree', { jsonrpc: '2.0', id: 1, method: 'tools/list', params: {} }))
    ).json()) as JsonRpcResult;
    const hot = await listRealProviderTools(periPath, resolve(process.cwd(), '../../mcp-packages/workspace'));
    assert.equal(hot.tools.length, 7);
    assert.equal(hot.serverInfo.version, COLD_WORKSPACE_PROVIDER_VERSION);
    const normalize = (tools: Array<Record<string, unknown>>) =>
      tools.map((tool) => ({ name: tool.name, description: tool.description, inputSchema: tool.inputSchema }));
    assert.deepEqual(normalize(cold.result.tools as Array<Record<string, unknown>>), normalize(hot.tools));
  } finally {
    await manager.closeAll();
  }
});
