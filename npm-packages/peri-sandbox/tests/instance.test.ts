import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { createServer } from 'node:http';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { PassThrough } from 'node:stream';
import test from 'node:test';
import type { ChildProcessWithoutNullStreams } from 'node:child_process';
import { startSandboxWith, type BackendFactoryInput, type StartSandboxDependencies } from '../src/instance/start.js';
import {
  SandboxError,
  type McpBackendPort,
  type SandboxInstance,
  type StartSandboxOptions,
} from '../src/instance/types.js';
import type { Provider, ProviderHandle, ProviderStartOptions } from '../src/runtime/provider.js';
import type { SandboxHttpServer, SandboxHttpServerOptions } from '../src/server/http.js';

const id = `sbx_${'a'.repeat(32)}`;

class FakeProvider implements Provider {
  readonly kind = 'macos-process-group' as const;
  readonly process: ChildProcessWithoutNullStreams;
  readonly starts: ProviderStartOptions[] = [];
  stopCalls = 0;
  startError: Error | null = null;
  stopError: Error | null = null;
  activeUnit: string | null = null;

  constructor() {
    this.process = Object.assign(new EventEmitter(), {
      stdin: new PassThrough(),
      stdout: new PassThrough(),
      stderr: new PassThrough(),
      pid: 4242,
    }) as unknown as ChildProcessWithoutNullStreams;
  }

  async doctor(): Promise<void> {}

  async findActiveUnit(): Promise<string | null> {
    return this.activeUnit;
  }

  async start(options: ProviderStartOptions): Promise<ProviderHandle> {
    this.starts.push(options);
    if (this.startError) throw this.startError;
    return {
      provider: this.kind,
      unit: null,
      process: this.process,
      stop: async () => {
        this.stopCalls += 1;
        if (this.stopError) throw this.stopError;
      },
    };
  }
}

class FakeBackend implements McpBackendPort {
  readonly runId: string;
  closed = 0;
  constructor(runId: string) {
    this.runId = runId;
  }
  serverCapabilities() {
    return { tools: {} };
  }
  serverInfo() {
    return { name: 'fake', version: '1' };
  }
  instructions() {
    return undefined;
  }
  async request(): Promise<Record<string, unknown>> {
    return { resultType: 'complete' };
  }
  async notify(): Promise<void> {}
  async subscribe() {
    return {
      honoredFilter: {},
      close: async () => {},
      closed: Promise.resolve('local' as const),
    };
  }
  cancel(): void {}
  async close(): Promise<void> {
    this.closed += 1;
  }
}

interface Harness {
  provider: FakeProvider;
  deps: StartSandboxDependencies;
  exits: Array<(error: Error) => void>;
  httpClosed: () => number;
  backends: FakeBackend[];
}

function harness(overrides: Partial<StartSandboxDependencies> = {}): Harness {
  const provider = new FakeProvider();
  const exits: Array<(error: Error) => void> = [];
  const backends: FakeBackend[] = [];
  let httpCloseCalls = 0;
  const deps: StartSandboxDependencies = {
    provider,
    verifyRuntime: () => undefined,
    backendFactory: (input: BackendFactoryInput) => {
      exits.push(input.onExit);
      const backend = new FakeBackend(input.runId);
      backends.push(backend);
      return backend;
    },
    httpFactory: async (_options: SandboxHttpServerOptions): Promise<SandboxHttpServer> => ({
      url: 'http://127.0.0.1:41000/mcp',
      port: 41000,
      close: async () => {
        httpCloseCalls += 1;
      },
    }),
    ...overrides,
  };
  return { provider, deps, exits, httpClosed: () => httpCloseCalls, backends };
}

function settingsFile(root: string, sandboxesRoot = join(root, 'sandboxes')): string {
  const file = join(root, 'settings.json');
  writeFileSync(
    file,
    JSON.stringify({
      sandboxesRoot,
      periPath: process.execPath,
      periSha256: 'a'.repeat(64),
      defaultProfile: 'default',
      profiles: {
        default: {
          allowedDomains: [],
          runtimeReadPaths: ['/usr'],
          memoryMax: '1G',
          tasksMax: 32,
          cpuQuota: '100%',
        },
      },
    }),
  );
  return file;
}

function startOptions(root: string, overrides: StartSandboxOptions = {}): StartSandboxOptions {
  mkdirSync(join(root, 'sandboxes'), { recursive: true, mode: 0o700 });
  return {
    settingsPath: settingsFile(root),
    sandboxId: id,
    minFreeBytes: 0,
    ...overrides,
  };
}

async function withRoot(operation: (root: string) => Promise<void>): Promise<void> {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'peri-instance-')));
  try {
    await operation(root);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

test('startSandbox returns a ready instance with a private workspace and no leaked token', async () => {
  await withRoot(async (root) => {
    const { deps, provider, backends } = harness();
    const instance = await startSandboxWith(startOptions(root), deps);
    try {
      assert.equal(instance.id, id);
      assert.match(instance.runId, /^run_[a-f0-9]{24}$/);
      assert.equal(instance.state, 'ready');
      assert.equal(instance.workspacePath, join(root, 'sandboxes', id, 'workspace'));
      assert.equal(instance.tokenFile, null);
      assert.match(instance.token, /^[a-f0-9]{64}$/);
      assert.equal(provider.starts.length, 1);
      assert.equal(provider.starts[0].policyPath, join(root, 'sandboxes', id, 'policy.json'));
      assert.equal(backends.length, 1);
      assert.equal(existsSync(join(root, 'sandboxes', id, '.lock')), true);
      const status = JSON.stringify(instance.status());
      assert(!status.includes(instance.token), 'status leaked the token');
      assert(!('token' in instance.status()));
      assert.deepEqual(instance.runtime, { provider: 'macos-process-group', unit: null, pid: 4242 });
      assert.equal(instance.status().lastError, null);
    } finally {
      await instance.close();
    }
  });
});

test('the same sandbox id conflicts while running, and restarts keep the workspace', async () => {
  await withRoot(async (root) => {
    const { deps } = harness();
    const options = startOptions(root);
    const first = await startSandboxWith(options, deps);
    const workspaceFile = join(first.workspacePath, 'persisted.txt');
    writeFileSync(workspaceFile, 'kept');
    await assert.rejects(
      () => startSandboxWith(options, deps),
      (error: unknown) => error instanceof SandboxError && error.code === 'conflict',
    );
    const firstRun = first.runId;
    await first.close();
    assert.equal(existsSync(workspaceFile), true);

    const second = await startSandboxWith(options, deps);
    try {
      assert.equal(second.id, id);
      assert.notEqual(second.runId, firstRun);
      assert.equal(second.workspacePath, first.workspacePath);
      assert.equal(readFileSync(workspaceFile, 'utf8'), 'kept');
    } finally {
      await second.close();
    }
  });
});

test('an active residual unit blocks startup and does not leak the lock', async () => {
  await withRoot(async (root) => {
    const { deps, provider } = harness();
    provider.activeUnit = `peri-sbx-${id}-run_${'b'.repeat(24)}.service`;
    await assert.rejects(
      () => startSandboxWith(startOptions(root), deps),
      (error: unknown) => error instanceof SandboxError && error.code === 'conflict',
    );
    assert.equal(provider.starts.length, 0);
    assert.equal(existsSync(join(root, 'sandboxes', id, '.lock')), false);
  });
});

test('close is idempotent and releases the lock once', async () => {
  await withRoot(async (root) => {
    const { deps, provider, httpClosed, backends } = harness();
    const instance = await startSandboxWith(startOptions(root), deps);
    await instance.close();
    await instance.close();
    assert.equal(instance.state, 'closed');
    assert.equal(provider.stopCalls, 1);
    assert.equal(httpClosed(), 1);
    assert.equal(backends[0].closed, 1);
    assert.equal(existsSync(join(root, 'sandboxes', id, '.lock')), false);
    assert.equal(existsSync(join(root, 'sandboxes', id, 'workspace')), true);
  });
});

test('close({deleteData:true}) removes the sandbox root, also after a plain close', async () => {
  await withRoot(async (root) => {
    const { deps } = harness();
    const first = await startSandboxWith(startOptions(root), deps);
    await first.close({ deleteData: true });
    assert.equal(existsSync(join(root, 'sandboxes', id)), false);

    const second = await startSandboxWith(startOptions(root), deps);
    await second.close();
    assert.equal(existsSync(join(root, 'sandboxes', id)), true);
    await second.close({ deleteData: true });
    assert.equal(existsSync(join(root, 'sandboxes', id)), false);
  });
});

test('an unexpected backend exit marks the instance failed and close still cleans up', async () => {
  await withRoot(async (root) => {
    const { deps, exits, provider } = harness();
    const instance = await startSandboxWith(startOptions(root), deps);
    exits[0](new SandboxError('backend_exited', 'backend died'));
    assert.equal(instance.state, 'failed');
    assert.deepEqual(instance.status().lastError, { code: 'backend_exited', message: 'backend died' });
    await instance.close();
    assert.equal(instance.state, 'closed');
    assert.equal(provider.stopCalls, 1);
    assert.equal(existsSync(join(root, 'sandboxes', id, '.lock')), false);
  });
});

test('a backend that exits before ready fails startup and cleans the unit and lock', async () => {
  await withRoot(async (root) => {
    const { deps, provider } = harness({
      backendFactory: async () => {
        throw new SandboxError('backend_exited', 'backend exited before ready');
      },
    });
    await assert.rejects(
      () => startSandboxWith(startOptions(root), deps),
      (error: unknown) => error instanceof SandboxError && error.code === 'backend_exited',
    );
    assert.equal(provider.stopCalls, 1);
    assert.equal(existsSync(join(root, 'sandboxes', id, '.lock')), false);
  });
});

test('startup timeout rejects and stops the unit', async () => {
  await withRoot(async (root) => {
    const { deps, provider } = harness({
      backendFactory: () => new Promise<McpBackendPort>(() => undefined),
    });
    await assert.rejects(
      () => startSandboxWith(startOptions(root, { startupTimeoutMs: 30 }), deps),
      (error: unknown) => error instanceof SandboxError && error.code === 'start_failed',
    );
    assert.equal(provider.stopCalls, 1);
    assert.equal(existsSync(join(root, 'sandboxes', id, '.lock')), false);
  });
});

test('an aborted start is reported as start_cancelled', async () => {
  await withRoot(async (root) => {
    const { deps, provider } = harness();
    const controller = new AbortController();
    controller.abort();
    await assert.rejects(
      () => startSandboxWith(startOptions(root, { signal: controller.signal }), deps),
      (error: unknown) => error instanceof SandboxError && error.code === 'start_cancelled',
    );
    assert.equal(provider.starts.length, 0);
    assert.equal(existsSync(join(root, 'sandboxes', id, '.lock')), false);
  });
});

test('an explicit occupied port is a conflict and never falls back', async () => {
  await withRoot(async (root) => {
    const occupant = createServer();
    await new Promise<void>((resolve) => occupant.listen(0, '127.0.0.1', resolve));
    const address = occupant.address();
    assert(address && typeof address !== 'string');
    try {
      const { deps, provider } = harness({ httpFactory: undefined });
      await assert.rejects(
        () =>
          startSandboxWith(
            startOptions(root, { port: address.port, token: 'a'.repeat(64), sandboxId: `sbx_${'c'.repeat(32)}` }),
            deps,
          ),
        (error: unknown) => error instanceof SandboxError && error.code === 'conflict',
      );
      assert.equal(provider.starts.length, 0);
      assert.equal(existsSync(join(root, 'sandboxes', `sbx_${'c'.repeat(32)}`, '.lock')), false);
    } finally {
      await new Promise<void>((resolve) => occupant.close(() => resolve()));
    }
  });
});

test('stop_incomplete keeps the lock so close can be retried', async () => {
  await withRoot(async (root) => {
    const { deps, provider } = harness();
    const instance = await startSandboxWith(startOptions(root), deps);
    provider.stopError = new SandboxError('stop_incomplete', 'cannot confirm stop');
    await assert.rejects(
      () => instance.close(),
      (error: unknown) => error instanceof SandboxError && error.code === 'stop_incomplete',
    );
    assert.equal(instance.state, 'failed');
    assert.equal(existsSync(join(root, 'sandboxes', id, '.lock')), true);
    provider.stopError = null;
    await instance.close();
    assert.equal(instance.state, 'closed');
    assert.equal(existsSync(join(root, 'sandboxes', id, '.lock')), false);
  });
});

test('a token file is written inside the sandbox root and removed with deleteData', async () => {
  await withRoot(async (root) => {
    const { deps } = harness();
    const tokenFile = join(root, 'sandboxes', id, 'token');
    const instance = await startSandboxWith(startOptions(root, { tokenFile }), deps);
    assert.equal(instance.tokenFile, realpathSync(tokenFile));
    assert.equal(readFileSync(instance.tokenFile!, 'utf8'), instance.token);
    await instance.close({ deleteData: true });
    assert.equal(existsSync(tokenFile), false);
  });
});

test('sandboxesRoot under a symlinked ancestor is canonicalized', async () => {
  const raw = mkdtempSync(join(tmpdir(), 'peri-instance-'));
  try {
    const canonical = realpathSync(raw);
    const { deps } = harness();
    const instance = await startSandboxWith(
      {
        settingsPath: settingsFile(raw),
        sandboxId: `sbx_${'d'.repeat(32)}`,
        minFreeBytes: 0,
      },
      deps,
    );
    try {
      assert(instance.workspacePath.startsWith(`${canonical}/`), instance.workspacePath);
      assert.equal(instance.sandboxRoot, join(canonical, 'sandboxes', instance.id));
    } finally {
      await instance.close();
    }
  } finally {
    rmSync(raw, { recursive: true, force: true });
  }
});

test('instance exposes a status snapshot without mutable internal state', async () => {
  await withRoot(async (root) => {
    const { deps } = harness();
    const instance: SandboxInstance = await startSandboxWith(startOptions(root), deps);
    try {
      const status = instance.status();
      (status.runtime as { unit: string | null }).unit = 'mutated';
      assert.equal(instance.runtime.unit, null);
      assert.equal(Object.isFrozen(instance.runtime), true);
    } finally {
      await instance.close();
    }
  });
});
