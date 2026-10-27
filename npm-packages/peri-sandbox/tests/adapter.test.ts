import assert from 'node:assert/strict';
import test from 'node:test';
import { createPeriSandboxAdapterWith, type PeriSandboxAdapterDependencies } from '../src/adapter.js';
import type { SandboxInstance, StartSandboxOptions } from '../src/instance/types.js';

const id = `sbx_${'c'.repeat(32)}`;
const token = 'adapter-test-token-123';

function fakeInstance(options: StartSandboxOptions, closeCalls: { count: number }): SandboxInstance {
  const workspacePath = `/tmp/peri-sandbox/sandboxes/${options.sandboxId}/workspace`;
  return {
    id: options.sandboxId as string,
    runId: 'run_adapter',
    state: 'ready',
    mcpUrl: 'http://127.0.0.1:41000/mcp',
    token,
    tokenFile: null,
    workspacePath,
    sandboxRoot: `/tmp/peri-sandbox/sandboxes/${options.sandboxId}`,
    runtime: { provider: 'macos-process-group', unit: null, pid: 4242 },
    status() {
      return {
        id: options.sandboxId as string,
        runId: 'run_adapter',
        state: 'ready',
        mcpUrl: 'http://127.0.0.1:41000/mcp',
        workspacePath,
        sandboxRoot: `/tmp/peri-sandbox/sandboxes/${options.sandboxId}`,
        runtime: { provider: 'macos-process-group', unit: null, pid: 4242 },
        lastError: null,
      };
    },
    async close() {
      closeCalls.count += 1;
    },
  };
}

function harness(overrides: PeriSandboxAdapterDependencies = {}): {
  deps: PeriSandboxAdapterDependencies;
  starts: StartSandboxOptions[];
  deletes: Array<{ sandboxId: string; sandboxesRoot?: string; settingsPath?: string }>;
  closes: { count: number };
} {
  const starts: StartSandboxOptions[] = [];
  const deletes: Array<{ sandboxId: string; sandboxesRoot?: string; settingsPath?: string }> = [];
  const closes = { count: 0 };
  const deps: PeriSandboxAdapterDependencies = {
    startSandbox: async (options) => {
      starts.push(options);
      return fakeInstance(options, closes);
    },
    deleteSandboxData: async (options) => {
      deletes.push(options);
    },
    ...overrides,
  };
  return { deps, starts, deletes, closes };
}

test('adapter maps start arguments and exposes the token only in mcpServer', async () => {
  const { deps, starts, closes } = harness();
  const adapter = createPeriSandboxAdapterWith({ sandboxesRoot: '/tmp/sandboxes' }, deps);
  const controller = new AbortController();

  const instance = await adapter.start({
    sandboxId: id,
    signal: controller.signal,
    options: {
      profileName: 'workspace-default',
      profile: {
        allowedDomains: ['example.com'],
        runtimeReadPaths: ['/usr'],
        memoryMax: '1G',
        tasksMax: 128,
        cpuQuota: '100%',
      },
      port: 41000,
      bashCalls: 42,
      minFreeBytes: 1024,
      startupTimeoutMs: 60_000,
      stopTimeoutMs: 10_000,
    },
  });

  assert.equal(starts.length, 1);
  assert.deepEqual(starts[0], {
    sandboxId: id,
    sandboxesRoot: '/tmp/sandboxes',
    profileName: 'workspace-default',
    profile: {
      allowedDomains: ['example.com'],
      runtimeReadPaths: ['/usr'],
      memoryMax: '1G',
      tasksMax: 128,
      cpuQuota: '100%',
    },
    port: 41000,
    bashCalls: 42,
    minFreeBytes: 1024,
    startupTimeoutMs: 60_000,
    stopTimeoutMs: 10_000,
    signal: controller.signal,
  });
  assert.equal(instance.sandboxId, id);
  assert.equal(instance.workspacePath, `/tmp/peri-sandbox/sandboxes/${id}/workspace`);
  assert.deepEqual(instance.mcpServer, {
    type: 'http',
    url: 'http://127.0.0.1:41000/mcp',
    headers: { Authorization: `Bearer ${token}` },
  });
  const { mcpServer, ...visible } = instance;
  assert.equal(JSON.stringify(visible).includes(token), false);
  assert.equal(mcpServer.headers.Authorization, `Bearer ${token}`);

  await instance.close();
  assert.equal(closes.count, 1);
});

test('adapter omits undefined per-start options and passes settingsPath', async () => {
  const { deps, starts } = harness();
  const adapter = createPeriSandboxAdapterWith({ settingsPath: '/tmp/settings.json' }, deps);

  await adapter.start({ sandboxId: id });

  assert.deepEqual(starts[0], { sandboxId: id, settingsPath: '/tmp/settings.json' });
});

test('adapter forwards deleteData with deployment roots', async () => {
  const { deps, deletes } = harness();
  const adapter = createPeriSandboxAdapterWith(
    { settingsPath: '/tmp/settings.json', sandboxesRoot: '/tmp/sandboxes' },
    deps,
  );

  await adapter.deleteData?.(id);

  assert.deepEqual(deletes, [{ sandboxId: id, sandboxesRoot: '/tmp/sandboxes', settingsPath: '/tmp/settings.json' }]);
});

test('adapter propagates start failures unchanged', async () => {
  const failure = new Error('sandbox refused to start');
  const { deps } = harness({
    startSandbox: async () => {
      throw failure;
    },
  });
  const adapter = createPeriSandboxAdapterWith({}, deps);

  await assert.rejects(adapter.start({ sandboxId: id }), failure);
});
