import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import {
  createHostResourceOwner,
  createSandboxMcpActor,
  type SandboxActorResult,
  type SandboxMcpRuntimePort,
} from '../src/rivet/index.js';
import {
  createSandboxVars,
  type SandboxActorContext,
  type SandboxActorVars,
} from '../src/rivet/sandbox-actor.js';
import type { WorkspaceUri } from '../src/rivet/workspace-uri.js';
import { formatWorkspaceUri } from '../src/rivet/workspace-uri.js';

const MACHINE_ID = '00000000-0000-4000-8000-000000000001';
const WORKSPACE = formatWorkspaceUri({ machineId: MACHINE_ID, runtimePath: '/Users/me/example-race' });
const HOLDER = 'sandbox-mcp:host';
const TOKEN = 'sandbox-example-token-0123456789';

type ExampleFinally = (...values: unknown[]) => Promise<void>;

const EXAMPLE_SOURCE = readFileSync(new URL('../examples/sandbox-mcp.mjs', import.meta.url), 'utf8');

function exampleFinally(): ExampleFinally {
  const parts = EXAMPLE_SOURCE.split('} finally {\n');
  const body = parts[1]?.split('\n}\n')[0];
  if (parts.length !== 2 || body === undefined) {
    throw new Error('examples/sandbox-mcp.mjs finally block shape changed');
  }
  const AsyncFunction = Object.getPrototypeOf(async () => {}).constructor as new (
    ...args: string[]
  ) => ExampleFinally;
  return new AsyncFunction(
    'held',
    'client',
    'holder',
    'token',
    'uri',
    'owner',
    'runtime',
    'registry',
    'temporary',
    'root',
    'rmSync',
    'process',
    'console',
    body,
  );
}

interface RuntimeWithCloseAll extends SandboxMcpRuntimePort {
  state: string | undefined;
  closeAllCalls: number;
  failCloseRemaining: number;
  failCloseAll: boolean;
  closeAll(): Promise<void>;
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
    };
    onRequest(
      c: SandboxActorContext & { request?: Request | undefined },
      request: Request,
    ): Promise<Response>;
    onDestroy(c: SandboxActorContext): Promise<void>;
  };
}

function runtime(ownerClosing: () => boolean): RuntimeWithCloseAll {
  const value: RuntimeWithCloseAll = {
    state: undefined,
    closeAllCalls: 0,
    failCloseRemaining: 0,
    failCloseAll: false,
    async start() {
      if (ownerClosing()) {
        throw new Error('started after owner shutdown');
      }
      value.state = 'ready';
      return { state: 'ready' };
    },
    async close() {
      if (value.failCloseRemaining > 0) {
        value.failCloseRemaining -= 1;
        throw new Error('synthetic runtime close failed');
      }
      value.state = undefined;
    },
    stateOf: () => value.state,
    async fetchMcp() {
      return new Response(null);
    },
    async closeAll() {
      value.closeAllCalls += 1;
      if (value.failCloseAll) {
        throw new Error('synthetic runtime closeAll failed');
      }
    },
  };
  return value;
}

function definition(
  owner: ReturnType<typeof createHostResourceOwner>,
  port: RuntimeWithCloseAll,
): Definition {
  return createSandboxMcpActor({
    owner,
    runtime: port,
    token: TOKEN,
    machineId: MACHINE_ID,
    authorizedWorkspaces: [WORKSPACE],
    drainTimeoutMs: 2,
  }) as unknown as Definition;
}

function context(vars: SandboxActorVars = createSandboxVars()): SandboxActorContext {
  return { vars, key: ['sbx', WORKSPACE], keepAwake: (promise) => promise };
}

function request(): Request {
  return new Request('http://127.0.0.1:8962/mcp', {
    headers: { 'x-peri-sandbox-token': TOKEN },
  });
}

function consoleStub(messages: string[]): { error: (value: unknown) => void } {
  return { error: (value: unknown) => messages.push(String(value)) };
}

describe('examples/sandbox-mcp.mjs shutdown gate', () => {
  test('an unconfirmed drain skips runtime.closeAll and keeps the live state directory', async () => {
    const owner = createHostResourceOwner();
    const port = runtime(() => owner.shuttingDown);
    const d = definition(owner, port);
    const c = context();
    await d.config.actions.acquire(c, HOLDER, TOKEN);

    let cancels = 0;
    port.fetchMcp = async () =>
      new Response(
        new ReadableStream<Uint8Array>({
          start(controller) {
            controller.enqueue(new Uint8Array([1]));
          },
          cancel() {
            cancels += 1;
            throw new Error('synthetic cancel not confirmed');
          },
        }),
      );

    const response = await d.config.onRequest(c, request());
    const reader = response.body?.getReader();
    if (reader === undefined) throw new Error('expected a response body');
    await reader.read();

    const messages: string[] = [];
    const processStub = { exitCode: 0 };
    let disposed = 0;
    let registryShutdown = 0;
    let rmCalls = 0;
    const client = {
      sandbox: {
        getOrCreate: () => ({
          release: (holder: string, token: string) => d.config.actions.release(c, holder, token),
        }),
      },
      dispose: async () => {
        disposed += 1;
      },
    };
    const registry = {
      shutdown: async () => {
        registryShutdown += 1;
      },
    };

    await exampleFinally()(
      true,
      client,
      HOLDER,
      TOKEN,
      WORKSPACE,
      owner,
      port,
      registry,
      true,
      '/tmp/synthetic-example-root',
      () => {
        rmCalls += 1;
      },
      processStub,
      consoleStub(messages),
    );

    expect(messages).toContain('release incomplete: sandbox_close_incomplete');
    expect(messages).toContain('host resources incomplete: 1');
    expect(messages).toContain('runtime closeAll deferred: host resources are not confirmed');
    expect(cancels).toBe(1);
    expect(port.closeAllCalls).toBe(0);
    expect(port.state).toBe('ready');
    expect(owner.pending()).toHaveLength(1);
    expect(c.vars.refs).toBe(1);
    expect(processStub.exitCode).toBe(1);
    expect(rmCalls).toBe(0);
    // Client/registry cleanup does not depend on the resource gate.
    expect(disposed).toBe(1);
    expect(registryShutdown).toBe(1);

    await d.config.onDestroy(c);
    await reader.cancel().catch(() => undefined);
  });

  test('a confirmed owner close permits runtime.closeAll and temporary directory cleanup', async () => {
    const owner = createHostResourceOwner();
    const port = runtime(() => owner.shuttingDown);
    const d = definition(owner, port);
    const c = context();
    await d.config.actions.acquire(c, HOLDER, TOKEN);

    const messages: string[] = [];
    const processStub = { exitCode: 0 };
    let disposed = 0;
    let registryShutdown = 0;
    let root: string | undefined;
    let rmCalls = 0;
    const client = {
      sandbox: {
        getOrCreate: () => ({
          release: (holder: string, token: string) => d.config.actions.release(c, holder, token),
        }),
      },
      dispose: async () => {
        disposed += 1;
      },
    };
    const registry = {
      shutdown: async () => {
        registryShutdown += 1;
      },
    };

    await exampleFinally()(
      true,
      client,
      HOLDER,
      TOKEN,
      WORKSPACE,
      owner,
      port,
      registry,
      true,
      '/tmp/synthetic-example-root',
      (path: string) => {
        rmCalls += 1;
        root = path;
      },
      processStub,
      consoleStub(messages),
    );

    expect(messages).toHaveLength(0);
    expect(port.closeAllCalls).toBe(1);
    expect(port.state).toBeUndefined();
    expect(owner.pending()).toHaveLength(0);
    expect(processStub.exitCode).toBe(0);
    expect(rmCalls).toBe(1);
    expect(root).toBe('/tmp/synthetic-example-root');
    expect(disposed).toBe(1);
    expect(registryShutdown).toBe(1);
  });

  test('a failing runtime.closeAll keeps the temporary directory and sets a failure exit', async () => {
    const owner = createHostResourceOwner();
    const port = runtime(() => owner.shuttingDown);
    port.failCloseAll = true;
    const d = definition(owner, port);
    const c = context();
    await d.config.actions.acquire(c, HOLDER, TOKEN);

    const messages: string[] = [];
    const processStub = { exitCode: 0 };
    let disposed = 0;
    let registryShutdown = 0;
    let rmCalls = 0;
    let root: string | undefined;
    const client = {
      sandbox: {
        getOrCreate: () => ({
          release: (holder: string, token: string) => d.config.actions.release(c, holder, token),
        }),
      },
      dispose: async () => {
        disposed += 1;
      },
    };
    const registry = {
      shutdown: async () => {
        registryShutdown += 1;
      },
    };

    await exampleFinally()(
      true,
      client,
      HOLDER,
      TOKEN,
      WORKSPACE,
      owner,
      port,
      registry,
      true,
      '/tmp/synthetic-example-root',
      (path: string) => {
        rmCalls += 1;
        root = path;
      },
      processStub,
      consoleStub(messages),
    );

    expect(messages).toContain('runtime closeAll failed: Error');
    expect(port.closeAllCalls).toBe(1);
    expect(owner.pending()).toHaveLength(0);
    expect(processStub.exitCode).toBe(1);
    expect(rmCalls).toBe(0);
    expect(root).toBeUndefined();
    expect(disposed).toBe(1);
    expect(registryShutdown).toBe(1);
  });

  test('a release failure confirmed by the owner retry still permits runtime.closeAll', async () => {
    const owner = createHostResourceOwner();
    const port = runtime(() => owner.shuttingDown);
    const d = definition(owner, port);
    const c = context();
    await d.config.actions.acquire(c, HOLDER, TOKEN);
    port.failCloseRemaining = 1;

    const messages: string[] = [];
    const processStub = { exitCode: 0 };
    let disposed = 0;
    let registryShutdown = 0;
    let rmCalls = 0;
    const client = {
      sandbox: {
        getOrCreate: () => ({
          release: (holder: string, token: string) => d.config.actions.release(c, holder, token),
        }),
      },
      dispose: async () => {
        disposed += 1;
      },
    };
    const registry = {
      shutdown: async () => {
        registryShutdown += 1;
      },
    };

    await exampleFinally()(
      true,
      client,
      HOLDER,
      TOKEN,
      WORKSPACE,
      owner,
      port,
      registry,
      true,
      '/tmp/synthetic-example-root',
      () => {
        rmCalls += 1;
      },
      processStub,
      consoleStub(messages),
    );

    expect(messages).toContain('release incomplete: sandbox_close_incomplete');
    expect(messages).not.toContain('host resources incomplete: 1');
    expect(port.closeAllCalls).toBe(1);
    expect(port.state).toBeUndefined();
    expect(owner.pending()).toHaveLength(0);
    expect(processStub.exitCode).toBe(1);
    expect(rmCalls).toBe(1);
    expect(disposed).toBe(1);
    expect(registryShutdown).toBe(1);
  });
});
