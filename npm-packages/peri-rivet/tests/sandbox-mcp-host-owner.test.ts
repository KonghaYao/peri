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
const WORKSPACE = formatWorkspaceUri({ machineId: MACHINE_ID, runtimePath: '/Users/me/owner-workspace' });
const TOKEN = 'sandbox-host-owner-token-0123456789';
const HOLDER = 'sandbox-mcp:host';

class FakeRuntime implements SandboxMcpRuntimePort {
  readonly started: string[] = [];
  readonly closed: string[] = [];
  readonly fetched: WorkspaceUri[] = [];
  readonly states = new Map<string, string>();
  fetchImpl: ((workspaceUri: WorkspaceUri, request: Request) => Promise<Response>) | undefined;

  async start(workspaceUri: string): Promise<unknown> {
    this.started.push(workspaceUri);
    this.states.set(workspaceUri, 'ready');
    return { workspaceUri };
  }

  async close(workspaceUri: string): Promise<void> {
    this.closed.push(workspaceUri);
    this.states.delete(workspaceUri);
  }

  stateOf(workspaceUri: string): string | undefined {
    return this.states.get(workspaceUri);
  }

  async fetchMcp(workspaceUri: WorkspaceUri, request: Request): Promise<Response> {
    this.fetched.push(workspaceUri);
    if (this.fetchImpl === undefined) {
      throw new Error('no fetch implementation');
    }
    return await this.fetchImpl(workspaceUri, request);
  }
}

interface Definition {
  config: {
    actions: {
      acquire(
        c: SandboxActorContext,
        holder: string,
        credential: string,
      ): Promise<SandboxActorResult<{ workspaceUri: WorkspaceUri; state: 'ready' }>>;
    };
    onRequest(
      c: SandboxActorContext & { request?: Request | undefined },
      request: Request,
    ): Promise<Response>;
    onSleep(c: SandboxActorContext): Promise<void>;
  };
}

function actor(
  runtime: FakeRuntime,
  owner: ReturnType<typeof createHostResourceOwner>,
  diagnostics: SandboxMcpActorDiagnostic[] = [],
) {
  return createSandboxMcpActor({
    owner,
    runtime,
    token: TOKEN,
    machineId: MACHINE_ID,
    authorizedWorkspaces: [WORKSPACE],
    drainTimeoutMs: 10,
    onDiagnostic: (diagnostic) => diagnostics.push(diagnostic),
  }) as unknown as Definition;
}

function context(vars: SandboxActorVars = createSandboxVars()): SandboxActorContext {
  return {
    vars,
    key: ['sbx', WORKSPACE],
    keepAwake: (promise) => promise,
  };
}

function request(): Request {
  return new Request('http://127.0.0.1:8962/mcp', {
    method: 'GET',
    headers: { 'x-peri-sandbox-token': TOKEN },
  });
}

function streamingSource(cancel: () => Promise<void>): Response {
  return new Response(
    new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode('first'));
      },
      cancel,
    }),
    { status: 200 },
  );
}

describe('sandbox MCP host owner', () => {
  test('a new activation cannot bypass an old gate whose cancel keeps rejecting', async () => {
    const runtime = new FakeRuntime();
    let cancels = 0;
    runtime.fetchImpl = async () =>
      streamingSource(async () => {
        cancels += 1;
        throw new Error('source cancellation failed');
      });
    const diagnostics: SandboxMcpActorDiagnostic[] = [];
    const owner = createHostResourceOwner();
    const definition = actor(runtime, owner, diagnostics);
    const first = context();
    await definition.config.actions.acquire(first, HOLDER, TOKEN);
    const response = await definition.config.onRequest(first, request());
    const reader = response.body?.getReader();
    if (reader === undefined) throw new Error('expected a response body');
    await reader.read();

    const oldVars = first.vars;
    await definition.config.onSleep(first);
    expect(cancels).toBe(1);
    expect(runtime.closed).toEqual([]);
    expect(oldVars.refs).toBe(1);
    const pending = owner.pending()[0];
    if (pending === undefined) throw new Error('the failed sleep did not retain the gate');
    expect(pending.activationRef).not.toBe(oldVars);
    expect(owner.lease(pending.activationRef)).toBeDefined();

    // RivetKit replaces vars after the hook returns. A fresh activation for the same key must not
    // start a second runtime or forward HTTP while the old gate is still in flight.
    const second = context();
    const blocked = await definition.config.actions.acquire(second, HOLDER, TOKEN);
    expect(blocked.ok).toBe(false);
    if (!blocked.ok) {
      expect(blocked.error.code).toBe('sandbox_close_incomplete');
      expect(blocked.error.details).toMatchObject({ reason: 'drain_failed' });
    }
    expect(runtime.started).toEqual([WORKSPACE]);
    expect(second.vars.refs).toBe(0);
    const fetched = runtime.fetched.length;
    const http = await definition.config.onRequest(second, request());
    expect(http.status).toBe(503);
    expect(runtime.fetched).toHaveLength(fetched);

    const report = await owner.closeAll({ timeoutMs: 50, maxAttempts: 2 });
    expect(report.complete).toBe(false);
    expect(report.pending).toBe(1);
    expect(runtime.closed).toEqual([]);
    expect(diagnostics.some((event) => event.type === 'drain_failed')).toBe(true);
    await reader.cancel().catch(() => undefined);
  });

  test('host closeAll retains the gate until a pending cancel settles, then closes the runtime', async () => {
    const runtime = new FakeRuntime();
    let resolveCancel: (() => void) | undefined;
    runtime.fetchImpl = async () =>
      streamingSource(
        () =>
          new Promise<void>((resolve) => {
            resolveCancel = resolve;
          }),
      );
    const owner = createHostResourceOwner();
    const definition = actor(runtime, owner);
    const first = context();
    await definition.config.actions.acquire(first, HOLDER, TOKEN);
    const response = await definition.config.onRequest(first, request());
    const reader = response.body?.getReader();
    if (reader === undefined) throw new Error('expected a response body');
    await reader.read();

    const incomplete = await owner.closeAll({ timeoutMs: 20, maxAttempts: 2 });
    expect(incomplete.complete).toBe(false);
    expect(incomplete.pending).toBe(1);
    expect(runtime.closed).toEqual([]);

    resolveCancel?.();
    await Bun.sleep(0);
    await reader.cancel().catch(() => undefined);
    const complete = await owner.closeAll({ timeoutMs: 1_000, maxAttempts: 2 });
    expect(complete.complete).toBe(true);
    expect(runtime.closed).toEqual([WORKSPACE]);
    expect(first.vars.refs).toBe(0);
  });
});
