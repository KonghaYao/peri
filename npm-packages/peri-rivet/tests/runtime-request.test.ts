import { describe, expect, test } from 'bun:test';
import {
  createHostResourceOwner,
  createSandboxMcpActor,
  type SandboxActorResult,
} from '../src/rivet/index.js';
import { projectRuntimeRequest } from '../src/rivet/runtime-request.js';
import type { SandboxActorContext, SandboxActorVars } from '../src/rivet/sandbox-actor.js';
import { createSandboxVars } from '../src/rivet/sandbox-actor.js';
import { formatWorkspaceUri } from '../src/rivet/workspace-uri.js';

const MACHINE_ID = '00000000-0000-4000-8000-000000000001';
const WORKSPACE = formatWorkspaceUri({ machineId: MACHINE_ID, runtimePath: '/Users/me/request-race' });
const HOLDER = 'sandbox-mcp:host';
const TOKEN = 'sandbox-request-token-0123456789';

interface Definition {
  config: {
    createVars(): SandboxActorVars;
    actions: {
      acquire(
        c: SandboxActorContext,
        holder: string,
        credential: string,
      ): Promise<SandboxActorResult<{ workspaceUri: string; state: 'ready' }>>;
    };
    onRequest(
      c: SandboxActorContext & { request?: Request | undefined },
      request: Request,
    ): Promise<Response>;
    onDestroy(c: SandboxActorContext): Promise<void>;
  };
}

function context(): SandboxActorContext {
  return { vars: createSandboxVars(), key: ['sbx', WORKSPACE], keepAwake: (promise) => promise };
}

function actor(fetchMcp: (request: Request) => Promise<Response>): Definition {
  let state: string | undefined;
  return createSandboxMcpActor({
    owner: createHostResourceOwner(),
    runtime: {
      async start() {
        state = 'ready';
      },
      async close() {
        state = undefined;
      },
      stateOf: () => state,
      fetchMcp: async (_workspace, request) => await fetchMcp(request),
    },
    token: TOKEN,
    machineId: MACHINE_ID,
    authorizedWorkspaces: [WORKSPACE],
  }) as unknown as Definition;
}

describe('sandbox runtime request projection', () => {
  test('GET with only credentials forwards no headers; Accept is preserved', () => {
    const credentialOnly = new Request('http://127.0.0.1/mcp', {
      headers: {
        'x-peri-sandbox-token': TOKEN,
        'x-rivet-token': 'synthetic-gateway',
        authorization: 'Bearer synthetic-outer',
        cookie: 'session=synthetic',
        'proxy-authorization': 'Basic synthetic',
      },
    });
    const empty = projectRuntimeRequest(credentialOnly, new AbortController().signal);
    expect([...empty.headers]).toEqual([]);
    expect(empty.method).toBe('GET');

    const withMcp = new Request('http://127.0.0.1/mcp', {
      headers: {
        'x-peri-sandbox-token': TOKEN,
        authorization: 'Bearer synthetic-outer',
        accept: 'text/event-stream',
        'mcp-session-id': 'synthetic-session',
        'last-event-id': 'synthetic-event',
      },
    });
    const kept = projectRuntimeRequest(withMcp, new AbortController().signal);
    expect([...kept.headers].sort()).toEqual([
      ['accept', 'text/event-stream'],
      ['last-event-id', 'synthetic-event'],
      ['mcp-session-id', 'synthetic-session'],
    ]);
    expect(kept.headers.has('authorization')).toBe(false);
    expect(kept.headers.has('x-peri-sandbox-token')).toBe(false);
  });

  test('POST keeps the streaming body and forwards abort without credentials', async () => {
    const payload = JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/list' });
    const controller = new AbortController();
    const source = new Request('http://127.0.0.1/mcp', {
      method: 'POST',
      headers: {
        'x-peri-sandbox-token': TOKEN,
        authorization: 'Bearer synthetic-outer',
        'content-type': 'application/json',
        accept: 'application/json, text/event-stream',
      },
      body: payload,
      signal: controller.signal,
    });
    const projected = projectRuntimeRequest(source, controller.signal);
    expect(projected.method).toBe('POST');
    expect(projected.headers.get('content-type')).toBe('application/json');
    expect(projected.headers.get('accept')).toBe('application/json, text/event-stream');
    expect(projected.headers.has('authorization')).toBe(false);
    expect(await projected.text()).toBe(payload);

    const abortController = new AbortController();
    const secondSource = new Request('http://127.0.0.1/mcp', { method: 'POST', body: 'x' });
    const second = projectRuntimeRequest(secondSource, abortController.signal);
    abortController.abort(new Error('synthetic abort'));
    expect(second.signal.aborted).toBe(true);
  });

  test('actor /mcp forwards GETs and POST bodies without outer credentials and propagates abort', async () => {
    const payload = JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/list' });
    const observed: Array<Record<string, unknown>> = [];
    let forwardedSignal: AbortSignal | undefined;
    const d = actor(async (request) => {
      observed.push({
        method: request.method,
        actorCredential: request.headers.has('x-peri-sandbox-token'),
        gatewayCredential: request.headers.has('x-rivet-token'),
        authorization: request.headers.has('authorization'),
        cookie: request.headers.has('cookie'),
        accept: request.headers.has('accept'),
        contentType: request.headers.get('content-type'),
        body: request.method === 'POST' ? await request.text() : null,
      });
      forwardedSignal = request.signal;
      return new Response(null);
    });
    const c = context();
    await d.config.actions.acquire(c, HOLDER, TOKEN);

    const base = {
      'x-peri-sandbox-token': TOKEN,
      'x-rivet-token': 'synthetic-gateway',
      authorization: 'Bearer synthetic-outer',
      cookie: 'session=synthetic',
    };
    await d.config.onRequest(c, new Request('http://127.0.0.1/mcp', { headers: base }));
    await d.config.onRequest(
      c,
      new Request('http://127.0.0.1/mcp', {
        headers: { ...base, accept: 'text/event-stream' },
      }),
    );
    const controller = new AbortController();
    await d.config.onRequest(
      c,
      new Request('http://127.0.0.1/mcp', {
        method: 'POST',
        headers: { ...base, 'content-type': 'application/json' },
        body: payload,
        signal: controller.signal,
      }),
    );
    controller.abort(new Error('synthetic abort'));

    expect(observed).toEqual([
      {
        method: 'GET',
        actorCredential: false,
        gatewayCredential: false,
        authorization: false,
        cookie: false,
        accept: false,
        contentType: null,
        body: null,
      },
      {
        method: 'GET',
        actorCredential: false,
        gatewayCredential: false,
        authorization: false,
        cookie: false,
        accept: true,
        contentType: null,
        body: null,
      },
      {
        method: 'POST',
        actorCredential: false,
        gatewayCredential: false,
        authorization: false,
        cookie: false,
        accept: false,
        contentType: 'application/json',
        body: payload,
      },
    ]);
    expect(forwardedSignal?.aborted).toBe(true);
    await d.config.onDestroy(c);
  });

  test('the same projection passes under Node 24', () => {
    const script = `
import assert from 'node:assert/strict';
const mod = await import(process.env.PERI_PROJECTION_URL);
const token = crypto.randomUUID();
const credentialOnly = new Request('http://127.0.0.1/mcp', { headers: {
  'x-peri-sandbox-token': token,
  'x-rivet-token': 'synthetic-gateway',
  authorization: 'Bearer ' + token,
  cookie: 'session=' + token,
  'proxy-authorization': 'Basic ' + token,
} });
const empty = mod.projectRuntimeRequest(credentialOnly, new AbortController().signal);
assert.equal([...empty.headers].length, 0);
const withMcp = new Request('http://127.0.0.1/mcp', { headers: {
  'x-peri-sandbox-token': token,
  accept: 'text/event-stream',
  'mcp-session-id': 'synthetic-session',
} });
const kept = mod.projectRuntimeRequest(withMcp, new AbortController().signal);
assert.equal(kept.headers.get('accept'), 'text/event-stream');
assert.equal(kept.headers.get('mcp-session-id'), 'synthetic-session');
assert.equal(kept.headers.has('x-peri-sandbox-token'), false);
const payload = JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/list' });
const post = new Request('http://127.0.0.1/mcp', { method: 'POST', headers: {
  'x-peri-sandbox-token': token,
  'content-type': 'application/json',
}, body: payload });
const projected = mod.projectRuntimeRequest(post, new AbortController().signal);
assert.equal(projected.method, 'POST');
assert.equal(await projected.text(), payload);
assert.equal(projected.headers.has('authorization'), false);
const controller = new AbortController();
const abortedSource = new Request('http://127.0.0.1/mcp', { method: 'POST', body: 'x' });
const aborted = mod.projectRuntimeRequest(abortedSource, controller.signal);
controller.abort(new Error('synthetic abort'));
assert.equal(aborted.signal.aborted, true);
console.log(JSON.stringify({ runtime: 'node', credentialHeaders: 0, mcpHeadersKept: 2, postBodyPreserved: true, abortForwarded: true }));
`;
    const run = Bun.spawnSync({
      cmd: ['node', '--input-type=module', '-e', script],
      env: {
        ...process.env,
        PERI_PROJECTION_URL: new URL('../src/rivet/runtime-request.ts', import.meta.url).href,
      },
      stdout: 'pipe',
      stderr: 'pipe',
    });
    expect(new TextDecoder().decode(run.stderr)).toBe('');
    expect(run.exitCode).toBe(0);
    expect(JSON.parse(new TextDecoder().decode(run.stdout).trim())).toEqual({
      runtime: 'node',
      credentialHeaders: 0,
      mcpHeadersKept: 2,
      postBodyPreserved: true,
      abortForwarded: true,
    });
  });
});
