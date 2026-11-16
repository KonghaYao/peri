import { describe, expect, test } from 'bun:test';
import {
  createHostResourceOwner,
  createSandboxMcpActor,
  type SandboxMcpActorDiagnostic,
  type SandboxMcpRuntimePort,
} from '../src/rivet/index.js';
import {
  createSandboxVars,
  type SandboxActorContext,
  type SandboxActorResult,
  type SandboxActorVars,
} from '../src/rivet/sandbox-actor.js';
import { formatWorkspaceUri, type WorkspaceUri } from '../src/rivet/workspace-uri.js';

const MACHINE_ID = '00000000-0000-4000-8000-000000000001';
const WORKSPACE = formatWorkspaceUri({ machineId: MACHINE_ID, runtimePath: '/Users/me/workspace' });
const OTHER_WORKSPACE = formatWorkspaceUri({ machineId: MACHINE_ID, runtimePath: '/Users/me/other' });
const FOREIGN_WORKSPACE = formatWorkspaceUri({
  machineId: '00000000-0000-4000-8000-000000000002',
  runtimePath: '/Users/me/workspace',
});
const TOKEN = 'sandbox-mcp-token-0123456789';
const HOLDER = 'sandbox-mcp:host';

class FakeRuntime implements SandboxMcpRuntimePort {
  readonly started: string[] = [];
  readonly closed: string[] = [];
  readonly states = new Map<string, string>();
  readonly fetched: Array<{ workspaceUri: WorkspaceUri; request: Request }> = [];
  failStart: unknown = undefined;
  failClose: unknown = undefined;
  fetchImpl: ((workspaceUri: WorkspaceUri, request: Request) => Promise<Response>) | undefined;

  async start(workspaceUri: string): Promise<unknown> {
    if (this.failStart !== undefined) {
      throw this.failStart;
    }
    this.started.push(workspaceUri);
    this.states.set(workspaceUri, 'ready');
    return { workspaceUri };
  }

  async close(workspaceUri: string): Promise<void> {
    if (this.failClose !== undefined) {
      throw this.failClose;
    }
    this.closed.push(workspaceUri);
    this.states.delete(workspaceUri);
  }

  stateOf(workspaceUri: string): string | undefined {
    return this.states.get(workspaceUri);
  }

  async fetchMcp(workspaceUri: WorkspaceUri, request: Request): Promise<Response> {
    this.fetched.push({ workspaceUri, request });
    if (this.fetchImpl !== undefined) {
      return await this.fetchImpl(workspaceUri, request);
    }
    return new Response(JSON.stringify({ ok: true }), {
      status: 200,
      headers: { 'content-type': 'application/json', 'mcp-session-id': 'session-1' },
    });
  }
}

interface RequestContext extends SandboxActorContext {
  readonly request?: Request | undefined;
}

interface Definition {
  config: {
    createVars(): SandboxActorVars;
    actions: {
      acquire(
        c: SandboxActorContext,
        holder: string,
        credential: string,
      ): Promise<SandboxActorResult<{ workspaceUri: WorkspaceUri; state: 'ready' }>>;
      release(
        c: SandboxActorContext,
        holder: string,
        credential: string,
      ): Promise<SandboxActorResult<{ released: true }>>;
      status(
        c: SandboxActorContext,
        credential: string,
      ): Promise<SandboxActorResult<{ workspaceUri: WorkspaceUri; refs: number; present: boolean }>>;
      closeAll(c: SandboxActorContext, credential: string): Promise<SandboxActorResult<{ closed: true }>>;
    };
    onRequest(c: RequestContext, request: Request): Promise<Response>;
    onSleep(c: SandboxActorContext): Promise<void>;
    onDestroy(c: SandboxActorContext): Promise<void>;
    onBeforeConnect(c: RequestContext): Promise<void>;
  };
}

function definition(
  runtime: SandboxMcpRuntimePort | undefined = new FakeRuntime(),
  overrides: Record<string, unknown> = {},
): Definition {
  return createSandboxMcpActor({
    owner: createHostResourceOwner(),
    runtime,
    token: TOKEN,
    machineId: MACHINE_ID,
    authorizedWorkspaces: [WORKSPACE],
    ...overrides,
  }) as unknown as Definition;
}

function context(
  vars: SandboxActorVars = createSandboxVars(),
  key: string[] = ['sbx', WORKSPACE],
  request?: Request,
): SandboxActorContext & { request?: Request | undefined } {
  const kept: Promise<unknown>[] = [];
  const c: RequestContext = {
    vars,
    key,
    request,
    keepAwake<T>(promise: Promise<T>): Promise<T> {
      kept.push(promise);
      return promise;
    },
  };
  return c;
}

function request(
  options: {
    token?: string | null;
    path?: string;
    method?: string;
    body?: string | undefined;
    headers?: Record<string, string>;
  } = {},
): Request {
  const headers = new Headers(options.headers ?? {});
  if (options.token !== null) {
    headers.set('x-peri-sandbox-token', options.token ?? TOKEN);
  }
  headers.set('content-type', 'application/json');
  return new Request(`http://127.0.0.1${options.path ?? '/mcp'}`, {
    method: options.method ?? 'POST',
    headers,
    ...(options.body === undefined ? {} : { body: options.body }),
  });
}

function diagnostics(): {
  events: SandboxMcpActorDiagnostic[];
  sink: (d: SandboxMcpActorDiagnostic) => void;
} {
  const events: SandboxMcpActorDiagnostic[] = [];
  return { events, sink: (diagnostic) => events.push(diagnostic) };
}

describe('createSandboxMcpActor definition guards', () => {
  test('rejects missing or weak tokens at factory time', () => {
    const runtime = new FakeRuntime();
    for (const token of ['', 'short', 'has whitespace 0123456789']) {
      expect(() =>
        createSandboxMcpActor({
          owner: createHostResourceOwner(),
          runtime,
          token,
          machineId: MACHINE_ID,
          authorizedWorkspaces: [WORKSPACE],
        }),
      ).toThrow(/token must be/);
    }
  });

  test('rejects invalid machine ids and foreign/non-canonical authorized workspaces', () => {
    const runtime = new FakeRuntime();
    expect(() =>
      createSandboxMcpActor({
        owner: createHostResourceOwner(),
        runtime,
        token: TOKEN,
        machineId: 'machine_1',
        authorizedWorkspaces: [WORKSPACE],
      }),
    ).toThrow(/invalid_machine/);
    expect(() =>
      createSandboxMcpActor({
        owner: createHostResourceOwner(),
        runtime,
        token: TOKEN,
        machineId: MACHINE_ID,
        authorizedWorkspaces: [FOREIGN_WORKSPACE],
      }),
    ).toThrow(/another machine/);
    expect(() =>
      createSandboxMcpActor({
        owner: createHostResourceOwner(),
        runtime,
        token: TOKEN,
        machineId: MACHINE_ID,
        authorizedWorkspaces: [WORKSPACE.toUpperCase()],
      }),
    ).toThrow(/workspace URI/);
  });
});

describe('createSandboxMcpActor management actions', () => {
  test('a missing runtime fails acquire and HTTP with sandbox_unavailable', async () => {
    const definitionValue = definition(new FakeRuntime(), { runtime: undefined });
    const c = context();
    const acquired = await definitionValue.config.actions.acquire(c, HOLDER, TOKEN);
    expect(acquired.ok).toBe(false);
    if (!acquired.ok) {
      expect(acquired.error.code).toBe('sandbox_unavailable');
    }
    expect((await definitionValue.config.onRequest(c, request())).status).toBe(503);
  });

  test('wrong credential is rejected before any runtime call', async () => {
    const runtime = new FakeRuntime();
    const definitionValue = definition(runtime);
    const c = context();

    const acquired = await definitionValue.config.actions.acquire(c, HOLDER, 'wrong-token-0123456789');
    expect(acquired.ok).toBe(false);
    if (!acquired.ok) {
      expect(acquired.error).toMatchObject({ code: 'invalid_input', phase: 'input' });
      expect(JSON.stringify(acquired.error)).not.toContain(TOKEN);
    }
    expect(runtime.started).toEqual([]);
    expect(runtime.states.size).toBe(0);
  });

  test('key shape, non-canonical URI, foreign machine and unauthorized URI fail before runtime', async () => {
    const runtime = new FakeRuntime();
    const definitionValue = definition(runtime);
    const cases: string[][] = [
      ['sbx'],
      ['wrong', WORKSPACE],
      ['sbx', WORKSPACE.toUpperCase()],
      ['sbx', FOREIGN_WORKSPACE],
      ['sbx', OTHER_WORKSPACE],
    ];

    for (const key of cases) {
      const acquired = await definitionValue.config.actions.acquire(
        context(createSandboxVars(), key),
        HOLDER,
        TOKEN,
      );
      expect(acquired.ok).toBe(false);
      if (!acquired.ok) {
        expect(acquired.error.code).toBe('invalid_identity');
      }
    }
    expect(runtime.started).toEqual([]);
    expect(runtime.states.size).toBe(0);
  });

  test('acquire is idempotent per holder and exposes the canonical workspace URI', async () => {
    const runtime = new FakeRuntime();
    const definitionValue = definition(runtime, { now: () => 42 });
    const c = context();

    expect(await definitionValue.config.actions.acquire(c, HOLDER, TOKEN)).toEqual({
      ok: true,
      value: { workspaceUri: WORKSPACE, state: 'ready' },
    });
    expect(c.vars).toMatchObject({ refs: 1, holders: [HOLDER], startedAtMs: 42 });

    expect(await definitionValue.config.actions.acquire(c, HOLDER, TOKEN)).toEqual({
      ok: true,
      value: { workspaceUri: WORKSPACE, state: 'ready' },
    });
    expect(c.vars.refs).toBe(1);
    expect(runtime.started).toEqual([WORKSPACE, WORKSPACE]);

    const status = await definitionValue.config.actions.status(c, TOKEN);
    expect(status.ok && status.value).toMatchObject({ workspaceUri: WORKSPACE, present: true, refs: 1 });

    expect(await definitionValue.config.actions.release(c, HOLDER, TOKEN)).toEqual({
      ok: true,
      value: { released: true },
    });
    expect(runtime.closed).toEqual([WORKSPACE]);
    expect(c.vars.refs).toBe(0);
  });

  test('multiple Chat holders share one environment; only the last release closes it', async () => {
    const runtime = new FakeRuntime();
    const definitionValue = definition(runtime);
    const c = context();
    await definitionValue.config.actions.acquire(c, 'chat:one', TOKEN);
    await definitionValue.config.actions.acquire(c, 'chat:two', TOKEN);
    expect(c.vars.refs).toBe(2);

    await definitionValue.config.actions.release(c, 'chat:one', TOKEN);
    expect(runtime.closed).toEqual([]);
    expect(c.vars.holders).toEqual(['chat:two']);

    await definitionValue.config.actions.release(c, 'chat:two', TOKEN);
    expect(runtime.closed).toEqual([WORKSPACE]);
    expect(c.vars.refs).toBe(0);
  });

  test('a failed last release keeps the holder and reopens the HTTP entry', async () => {
    const runtime = new FakeRuntime();
    const definitionValue = definition(runtime, { drainTimeoutMs: 200 });
    const c = context();
    await definitionValue.config.actions.acquire(c, HOLDER, TOKEN);

    runtime.failClose = new Error('close failed');
    const released = await definitionValue.config.actions.release(c, HOLDER, TOKEN);
    expect(released.ok).toBe(false);
    if (!released.ok) {
      expect(released.error.code).toBe('sandbox_close_incomplete');
      expect(JSON.stringify(released.error)).not.toContain('close failed');
    }
    expect(c.vars.holders).toEqual([HOLDER]);
    const reopened = await definitionValue.config.onRequest(c, request());
    expect(reopened.status).toBe(200);
    await reopened.text();

    runtime.failClose = undefined;
    expect(await definitionValue.config.actions.release(c, HOLDER, TOKEN)).toEqual({
      ok: true,
      value: { released: true },
    });
    expect(runtime.closed).toEqual([WORKSPACE]);
  });
});

describe('createSandboxMcpActor HTTP forwarding', () => {
  test('rejects missing/wrong credentials, non-/mcp paths and requests before any holder', async () => {
    const runtime = new FakeRuntime();
    const definitionValue = definition(runtime);
    const c = context();

    expect((await definitionValue.config.onRequest(c, request({ token: null }))).status).toBe(401);
    expect(
      (await definitionValue.config.onRequest(c, request({ token: 'wrong-token-0123456789' }))).status,
    ).toBe(401);
    await definitionValue.config.actions.acquire(c, HOLDER, TOKEN);
    expect((await definitionValue.config.onRequest(c, request({ path: '/not-mcp' }))).status).toBe(404);
    expect(runtime.fetched).toHaveLength(0);

    const unheld = context();
    expect((await definitionValue.config.onRequest(unheld, request())).status).toBe(503);
  });

  test('forwards method, body, headers and the gateway-shaped path without outer credentials', async () => {
    const runtime = new FakeRuntime();
    const definitionValue = definition(runtime);
    const c = context();
    await definitionValue.config.actions.acquire(c, HOLDER, TOKEN);

    const response = await definitionValue.config.onRequest(
      c,
      request({
        method: 'POST',
        body: '{"jsonrpc":"2.0","id":1,"method":"initialize"}',
        headers: { 'mcp-session-id': 'session-in', 'mcp-protocol-version': '2026-07-28' },
      }),
    );
    expect(response.status).toBe(200);
    expect(response.headers.get('mcp-session-id')).toBe('session-1');
    expect(await response.text()).toBe('{"ok":true}');

    const forwarded = runtime.fetched[0];
    if (forwarded === undefined) throw new Error('missing forwarded request');
    expect(forwarded.workspaceUri).toBe(WORKSPACE);
    expect(new URL(forwarded.request.url).pathname).toBe('/mcp');
    expect(forwarded.request.method).toBe('POST');
    expect(forwarded.request.headers.get('mcp-session-id')).toBe('session-in');
    expect(forwarded.request.headers.get('mcp-protocol-version')).toBe('2026-07-28');
    expect(forwarded.request.headers.get('x-peri-sandbox-token')).toBeNull();
    expect(forwarded.request.headers.get('authorization')).toBeNull();
    expect(await forwarded.request.text()).toBe('{"jsonrpc":"2.0","id":1,"method":"initialize"}');

    const gatewayShaped = await definitionValue.config.onRequest(
      c,
      request({ path: '/gateway/actor_123/request/mcp', method: 'GET', body: undefined }),
    );
    expect(gatewayShaped.status).toBe(200);
    expect(runtime.fetched).toHaveLength(2);
  });

  test('forwards the MCP transport methods without rewriting them', async () => {
    const runtime = new FakeRuntime();
    const definitionValue = definition(runtime);
    const c = context();
    await definitionValue.config.actions.acquire(c, HOLDER, TOKEN);

    for (const method of ['GET', 'POST', 'DELETE', 'HEAD', 'OPTIONS']) {
      const response = await definitionValue.config.onRequest(c, request({ method, body: undefined }));
      expect(response.status).toBe(200);
      await response.text();
    }
    expect(runtime.fetched.map(({ request: forwarded }) => forwarded.method)).toEqual([
      'GET',
      'POST',
      'DELETE',
      'HEAD',
      'OPTIONS',
    ]);
  });

  test('strips hop-by-hop response headers and keeps MCP headers', async () => {
    const runtime = new FakeRuntime();
    runtime.fetchImpl = async () =>
      new Response('payload', {
        status: 200,
        headers: {
          'content-type': 'text/plain',
          'content-length': '7',
          'mcp-session-id': 'session-2',
          connection: 'keep-alive',
          'set-cookie': 'secret=1',
        },
      });
    const definitionValue = definition(runtime);
    const c = context();
    await definitionValue.config.actions.acquire(c, HOLDER, TOKEN);

    const response = await definitionValue.config.onRequest(c, request());
    expect(response.headers.get('content-type')).toBe('text/plain');
    expect(response.headers.get('mcp-session-id')).toBe('session-2');
    expect(response.headers.get('content-length')).toBeNull();
    expect(response.headers.get('set-cookie')).toBeNull();
    expect(await response.text()).toBe('payload');
  });

  test('last release aborts an in-flight request and waits for it before closing', async () => {
    const runtime = new FakeRuntime();
    let aborted = false;
    runtime.fetchImpl = async (_workspaceUri, incoming) => {
      await new Promise<void>((resolve) => {
        incoming.signal.addEventListener(
          'abort',
          () => {
            aborted = true;
            resolve();
          },
          { once: true },
        );
      });
      throw new Error('aborted');
    };
    const definitionValue = definition(runtime, { drainTimeoutMs: 200 });
    const c = context();
    await definitionValue.config.actions.acquire(c, HOLDER, TOKEN);

    const pending = definitionValue.config.onRequest(c, request());
    await Bun.sleep(5);
    const released = await definitionValue.config.actions.release(c, HOLDER, TOKEN);
    expect(released).toEqual({ ok: true, value: { released: true } });
    expect(aborted).toBe(true);
    expect((await pending).status).toBe(502);
    expect(runtime.closed).toEqual([WORKSPACE]);
  });

  test('last release aborts a live response stream so close is not blocked', async () => {
    const runtime = new FakeRuntime();
    let aborted = false;
    runtime.fetchImpl = async (_workspaceUri, incoming) => {
      const body = new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(new TextEncoder().encode('first'));
          incoming.signal.addEventListener(
            'abort',
            () => {
              aborted = true;
              try {
                controller.close();
              } catch {
                // The actor cancels the reader after aborting; one path closes first.
              }
            },
            { once: true },
          );
        },
      });
      return new Response(body, { status: 200, headers: { 'content-type': 'text/event-stream' } });
    };
    const definitionValue = definition(runtime, { drainTimeoutMs: 200 });
    const c = context();
    await definitionValue.config.actions.acquire(c, HOLDER, TOKEN);

    const response = await definitionValue.config.onRequest(c, request({ method: 'GET', body: undefined }));
    const body = response.body;
    if (body === null) throw new Error('expected a response body');
    const reader = body.getReader();
    expect(new TextDecoder().decode((await reader.read()).value)).toBe('first');
    const next = reader.read();
    const released = await definitionValue.config.actions.release(c, HOLDER, TOKEN);
    const tail = await next;
    expect(tail.done).toBe(true);
    expect(released).toEqual({ ok: true, value: { released: true } });
    expect(aborted).toBe(true);
    expect(runtime.closed).toEqual([WORKSPACE]);
  });

  test('last release cancels a response body that was never consumed', async () => {
    const runtime = new FakeRuntime();
    let cancels = 0;
    runtime.fetchImpl = async () => {
      const body = new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(new TextEncoder().encode('first'));
        },
        cancel() {
          cancels += 1;
        },
      });
      return new Response(body, { status: 200, headers: { 'content-type': 'text/event-stream' } });
    };
    const definitionValue = definition(runtime, { drainTimeoutMs: 200 });
    const c = context();
    await definitionValue.config.actions.acquire(c, HOLDER, TOKEN);

    const response = await definitionValue.config.onRequest(c, request({ method: 'GET', body: undefined }));
    expect(response.body).not.toBeNull();
    // Deliberately do not read the body.
    expect(await definitionValue.config.actions.release(c, HOLDER, TOKEN)).toEqual({
      ok: true,
      value: { released: true },
    });
    expect(cancels).toBe(1);
    expect(runtime.closed).toEqual([WORKSPACE]);
    expect(c.vars.refs).toBe(0);
  });

  test('unsettled cancellation fails close_incomplete, keeps the holder, and is retryable', async () => {
    const runtime = new FakeRuntime();
    const { events, sink } = diagnostics();
    let cancels = 0;
    let resolveCancel: (() => void) | undefined;
    runtime.fetchImpl = async () => {
      const body = new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(new TextEncoder().encode('first'));
        },
        cancel() {
          cancels += 1;
          return new Promise<void>((resolve) => {
            resolveCancel = resolve;
          });
        },
      });
      return new Response(body, { status: 200, headers: { 'content-type': 'text/event-stream' } });
    };
    const definitionValue = definition(runtime, { drainTimeoutMs: 20, onDiagnostic: sink });
    const c = context();
    await definitionValue.config.actions.acquire(c, HOLDER, TOKEN);
    const response = await definitionValue.config.onRequest(c, request({ method: 'GET', body: undefined }));
    const reader = response.body?.getReader();
    if (reader === undefined) throw new Error('expected a response body');
    await reader.read();

    const failed = await definitionValue.config.actions.release(c, HOLDER, TOKEN);
    expect(failed.ok).toBe(false);
    if (!failed.ok) {
      expect(failed.error.code).toBe('sandbox_close_incomplete');
      expect(failed.error.details).toMatchObject({ reason: 'drain_failed', remaining: 1 });
    }
    expect(cancels).toBe(1);
    expect(c.vars.refs).toBe(1);
    expect(runtime.closed).toEqual([]);
    expect(events.some((event) => event.type === 'request_drain_timeout' && event.remaining === 1)).toBe(
      true,
    );
    expect(events.some((event) => event.type === 'drain_failed')).toBe(true);

    // A failed drain keeps the holder; the existing retry test covers reopening the entry.

    resolveCancel?.();
    await Bun.sleep(0);
    await reader.cancel().catch(() => undefined);
    expect(await definitionValue.config.actions.release(c, HOLDER, TOKEN)).toEqual({
      ok: true,
      value: { released: true },
    });
    expect(runtime.closed).toEqual([WORKSPACE]);
    expect(c.vars.refs).toBe(0);
  });

  test('a rejecting source cancel keeps the request in flight and never reports release success', async () => {
    const runtime = new FakeRuntime();
    const { events, sink } = diagnostics();
    const source = { active: true, cancels: 0 };
    let ticks = 0;
    const producer = setInterval(() => {
      ticks += 1;
    }, 1);
    runtime.fetchImpl = async () => {
      const body = new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(new TextEncoder().encode('first'));
        },
        cancel() {
          source.cancels += 1;
          // The source fails before it can stop the producer (which the test clears below).
          throw new Error('source cancellation failed before producer cleanup');
        },
      });
      return new Response(body, { status: 200 });
    };
    const definitionValue = definition(runtime, { drainTimeoutMs: 20, onDiagnostic: sink });
    const c = context();
    try {
      await definitionValue.config.actions.acquire(c, HOLDER, TOKEN);
      const response = await definitionValue.config.onRequest(c, request({ method: 'GET', body: undefined }));
      const reader = response.body?.getReader();
      if (reader === undefined) throw new Error('expected a response body');
      await reader.read();

      const failed = await definitionValue.config.actions.release(c, HOLDER, TOKEN);
      expect(failed.ok).toBe(false);
      if (!failed.ok) {
        expect(failed.error.code).toBe('sandbox_close_incomplete');
        expect(failed.error.details).toMatchObject({ reason: 'drain_failed', remaining: 1 });
      }
      expect(source.cancels).toBe(1);
      expect(source.active).toBe(true);
      expect(ticks).toBeGreaterThan(0);
      expect(c.vars.refs).toBe(1);
      expect(runtime.closed).toEqual([]);
      expect(events.some((event) => event.type === 'drain_failed')).toBe(true);
      expect(events.some((event) => event.type === 'request_drain_timeout')).toBe(true);

      // A retry must not manufacture success, and reacquiring the same holder must not
      // duplicate the reference count while the unconfirmed request still owns the actor.
      expect((await definitionValue.config.actions.release(c, HOLDER, TOKEN)).ok).toBe(false);
      expect((await definitionValue.config.actions.acquire(c, HOLDER, TOKEN)).ok).toBe(true);
      expect(c.vars.refs).toBe(1);
      expect(c.vars.holders).toEqual([HOLDER]);
      await reader.cancel().catch(() => undefined);
    } finally {
      clearInterval(producer);
    }
  });

  test('a late response whose source cancel rejects is not marked drained', async () => {
    const runtime = new FakeRuntime();
    const { events, sink } = diagnostics();
    let releaseFetch: (() => void) | undefined;
    let cancels = 0;
    runtime.fetchImpl = async () => {
      await new Promise<void>((resolve) => {
        releaseFetch = resolve;
      });
      const body = new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(new TextEncoder().encode('first'));
        },
        cancel() {
          cancels += 1;
          throw new Error('late source cancellation failed');
        },
      });
      return new Response(body, { status: 200 });
    };
    const definitionValue = definition(runtime, { drainTimeoutMs: 20, onDiagnostic: sink });
    const c = context();
    await definitionValue.config.actions.acquire(c, HOLDER, TOKEN);

    const controller = new AbortController();
    const pending = definitionValue.config.onRequest(
      c,
      new Request(request({ method: 'GET', body: undefined }), { signal: controller.signal }),
    );
    while (releaseFetch === undefined) {
      await Bun.sleep(0);
    }
    controller.abort();
    releaseFetch();
    expect((await pending).status).toBe(503);
    expect(cancels).toBe(1);

    const failed = await definitionValue.config.actions.release(c, HOLDER, TOKEN);
    expect(failed.ok).toBe(false);
    if (!failed.ok) {
      expect(failed.error.code).toBe('sandbox_close_incomplete');
      expect(failed.error.details).toMatchObject({ reason: 'drain_failed', remaining: 1 });
    }
    expect(c.vars.refs).toBe(1);
    expect(runtime.closed).toEqual([]);
    expect(events.some((event) => event.type === 'drain_failed')).toBe(true);
  });

  test('a pending cancel that later rejects still blocks release and keeps the holder', async () => {
    const runtime = new FakeRuntime();
    let cancels = 0;
    let rejectCancel: ((error: unknown) => void) | undefined;
    runtime.fetchImpl = async () => {
      const body = new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(new TextEncoder().encode('first'));
        },
        cancel() {
          cancels += 1;
          return new Promise<void>((_resolve, reject) => {
            rejectCancel = reject;
          });
        },
      });
      return new Response(body, { status: 200 });
    };
    const definitionValue = definition(runtime, { drainTimeoutMs: 50 });
    const c = context();
    await definitionValue.config.actions.acquire(c, HOLDER, TOKEN);
    const response = await definitionValue.config.onRequest(c, request({ method: 'GET', body: undefined }));
    const reader = response.body?.getReader();
    if (reader === undefined) throw new Error('expected a response body');
    await reader.read();

    const release = definitionValue.config.actions.release(c, HOLDER, TOKEN);
    await Bun.sleep(5);
    expect(cancels).toBe(1);
    rejectCancel?.(new Error('pending source cancellation failed'));
    const failed = await release;
    expect(failed.ok).toBe(false);
    if (!failed.ok) {
      expect(failed.error.code).toBe('sandbox_close_incomplete');
      expect(failed.error.details).toMatchObject({ reason: 'drain_failed', remaining: 1 });
    }
    expect(c.vars.refs).toBe(1);
    expect(runtime.closed).toEqual([]);
    await reader.cancel().catch(() => undefined);
  });

  test('client abort with a rejecting source cancel is not counted as a drained request', async () => {
    const runtime = new FakeRuntime();
    const { events, sink } = diagnostics();
    let cancels = 0;
    runtime.fetchImpl = async () => {
      const body = new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(new TextEncoder().encode('first'));
        },
        cancel() {
          cancels += 1;
          throw new Error('abort source cancellation failed');
        },
      });
      return new Response(body, { status: 200 });
    };
    const definitionValue = definition(runtime, { drainTimeoutMs: 20, onDiagnostic: sink });
    const c = context();
    await definitionValue.config.actions.acquire(c, HOLDER, TOKEN);
    const controller = new AbortController();
    const response = await definitionValue.config.onRequest(
      c,
      new Request(request({ method: 'GET', body: undefined }), { signal: controller.signal }),
    );
    const reader = response.body?.getReader();
    if (reader === undefined) throw new Error('expected a response body');
    await reader.read();

    controller.abort();
    await Bun.sleep(0);
    expect(cancels).toBe(1);

    const failed = await definitionValue.config.actions.release(c, HOLDER, TOKEN);
    expect(failed.ok).toBe(false);
    if (!failed.ok) {
      expect(failed.error.code).toBe('sandbox_close_incomplete');
      expect(failed.error.details).toMatchObject({ reason: 'drain_failed' });
    }
    expect(runtime.closed).toEqual([]);
    expect(c.vars.refs).toBe(1);
    expect(events.some((event) => event.type === 'drain_failed')).toBe(true);
    await reader.cancel().catch(() => undefined);
  });

  test('client abort cancels the upstream response stream', async () => {
    const runtime = new FakeRuntime();
    let cancels = 0;
    let aborted = false;
    runtime.fetchImpl = async (_workspaceUri, incoming) => {
      incoming.signal.addEventListener('abort', () => {
        aborted = true;
      });
      const body = new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(new TextEncoder().encode('first'));
        },
        cancel() {
          cancels += 1;
        },
      });
      return new Response(body, { status: 200 });
    };
    const definitionValue = definition(runtime);
    const c = context();
    await definitionValue.config.actions.acquire(c, HOLDER, TOKEN);
    const controller = new AbortController();
    const signal = controller.signal;
    const forwarded = request({ method: 'GET', body: undefined });
    const response = await definitionValue.config.onRequest(c, new Request(forwarded, { signal }));
    const reader = response.body?.getReader();
    if (reader === undefined) throw new Error('expected a response body');
    await reader.read();

    controller.abort();
    await Bun.sleep(0);
    expect(aborted).toBe(true);
    expect(cancels).toBe(1);

    expect(await definitionValue.config.actions.release(c, HOLDER, TOKEN)).toEqual({
      ok: true,
      value: { released: true },
    });
    expect(runtime.closed).toEqual([WORKSPACE]);
  });

  test('a new request is rejected once the last release starts draining', async () => {
    const runtime = new FakeRuntime();
    let resolveCancel: (() => void) | undefined;
    runtime.fetchImpl = async () => {
      const body = new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(new TextEncoder().encode('first'));
        },
        cancel() {
          return new Promise<void>((resolve) => {
            resolveCancel = resolve;
          });
        },
      });
      return new Response(body, { status: 200 });
    };
    const definitionValue = definition(runtime, { drainTimeoutMs: 20 });
    const c = context();
    await definitionValue.config.actions.acquire(c, HOLDER, TOKEN);
    const response = await definitionValue.config.onRequest(c, request({ method: 'GET', body: undefined }));
    const reader = response.body?.getReader();
    if (reader === undefined) throw new Error('expected a response body');
    await reader.read();

    const release = definitionValue.config.actions.release(c, HOLDER, TOKEN);
    await Bun.sleep(2);
    expect((await definitionValue.config.onRequest(c, request())).status).toBe(503);
    const failed = await release;
    expect(failed.ok).toBe(false);
    resolveCancel?.();
    await Bun.sleep(0);
    await reader.cancel().catch(() => undefined);
    await definitionValue.config.actions.release(c, HOLDER, TOKEN);
  });

  test('sleep and destroy drain requests and close the instance', async () => {
    const runtime = new FakeRuntime();
    const { events, sink } = diagnostics();
    const definitionValue = definition(runtime, { onDiagnostic: sink });
    const c = context();
    await definitionValue.config.actions.acquire(c, HOLDER, TOKEN);
    await definitionValue.config.onSleep(c);
    expect(runtime.closed).toEqual([WORKSPACE]);
    expect(c.vars.refs).toBe(0);
    expect(events.some((event) => event.type === 'holders_lost_on_teardown')).toBe(true);
  });

  test('runtime errors reaching diagnostics never carry the actor token in message, stack or cause', async () => {
    const token = `synthetic-token-${crypto.randomUUID()}`;
    const runtime = new FakeRuntime();
    const { events, sink } = diagnostics();
    const secretError = (): Error => {
      const inner = new Error(`inner token=${token}`, {
        cause: new Error(`deep token=${token}`),
      });
      const outer = new Error(`outer token=${token}`, { cause: inner });
      outer.stack = `Error: outer token=${token}\n    at ${token}`;
      return outer;
    };

    runtime.failStart = secretError();
    const definitionValue = definition(runtime, { token, onDiagnostic: sink });
    const c = context();
    const started = await definitionValue.config.actions.acquire(c, HOLDER, token);
    expect(started.ok).toBe(false);

    runtime.failStart = undefined;
    expect((await definitionValue.config.actions.acquire(c, HOLDER, token)).ok).toBe(true);

    runtime.fetchImpl = async () => {
      throw secretError();
    };
    const http = await definitionValue.config.onRequest(c, request({ token }));
    expect(http.status).toBe(502);
    const body = await http.text();
    expect(body).not.toContain(token);
    runtime.fetchImpl = undefined;

    runtime.failClose = secretError();
    const released = await definitionValue.config.actions.release(c, HOLDER, token);
    expect(released.ok).toBe(false);
    expect(JSON.stringify(released)).not.toContain(token);

    const serialized = JSON.stringify(events);
    expect(serialized).not.toContain(token);
    expect(serialized).toContain('[redacted]');
    for (const type of ['start_failed', 'request_failed', 'close_failed']) {
      expect(events.some((event) => event.type === type)).toBe(true);
    }
    // The safe projection keeps diagnostics useful instead of dropping the error.
    const startDiagnostic = events.find((event) => event.type === 'start_failed');
    if (startDiagnostic === undefined || !('error' in startDiagnostic)) {
      throw new Error('expected a start_failed diagnostic with an error');
    }
    expect(startDiagnostic.error).toMatchObject({ name: 'Error' });
    expect(JSON.stringify(startDiagnostic.error)).toContain('cause');

    runtime.failClose = undefined;
    expect(await definitionValue.config.actions.release(c, HOLDER, token)).toEqual({
      ok: true,
      value: { released: true },
    });
  });

  test('connection guard requires the deployment credential but leaves /mcp to onRequest', async () => {
    const definitionValue = definition(new FakeRuntime());
    await expect(definitionValue.config.onBeforeConnect(context())).resolves.toBeUndefined();
    // RivetKit creates a synthetic connection for raw HTTP; /mcp must reach onRequest's 401.
    const mcpWithoutToken = context(createSandboxVars(), ['sbx', WORKSPACE], request({ token: null }));
    await expect(definitionValue.config.onBeforeConnect(mcpWithoutToken)).resolves.toBeUndefined();
    const denied = context(createSandboxVars(), ['sbx', WORKSPACE], request({ token: null, path: '/ws' }));
    await expect(definitionValue.config.onBeforeConnect(denied)).rejects.toThrow(/credential/);
    const allowed = context(createSandboxVars(), ['sbx', WORKSPACE], request({ path: '/ws' }));
    await expect(definitionValue.config.onBeforeConnect(allowed)).resolves.toBeUndefined();
  });
});
