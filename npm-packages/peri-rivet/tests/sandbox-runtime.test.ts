import { describe, expect, test } from 'bun:test';
import { chmodSync, mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync } from 'node:fs';
import { createServer, type IncomingHttpHeaders } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { SandboxInstance, StartSandboxOptions } from 'peri-sandbox';
import { formatWorkspaceUri, type WorkspaceUri } from '../src/rivet/workspace-uri.js';
import {
  createPeriSandboxMcpRuntimeWith,
  type PeriSandboxMcpRuntimeDependencies,
} from '../src/sandbox/index.js';

const MACHINE_ID = '00000000-0000-4000-8000-000000000001';
const OTHER_MACHINE = '00000000-0000-4000-8000-000000000002';
const INNER_TOKEN = 'inner-sandbox-token-0123456789';
const ACTOR_TOKEN = 'outer-actor-token-0123456789';

class FakeInstance {
  readonly id: string;
  readonly runId = 'run_fake';
  state = 'ready';
  mcpUrl = 'http://127.0.0.1:1/mcp';
  readonly token = INNER_TOKEN;
  readonly tokenFile = null;
  readonly workspacePath: string;
  readonly sandboxRoot: string;
  readonly runtime = { provider: 'macos-process-group' as const, unit: null, pid: null };
  closeCalls = 0;
  readonly closeOptions: Array<{ deleteData?: boolean } | undefined> = [];
  failClose = false;

  constructor(options: StartSandboxOptions) {
    this.id = `sbx_${'f'.repeat(32)}`;
    this.workspacePath = options.workspacePath ?? '/tmp/workspace';
    this.sandboxRoot = '/tmp/sandbox';
  }

  status() {
    return {
      id: this.id,
      runId: this.runId,
      state: this.state,
      mcpUrl: this.mcpUrl,
      workspacePath: this.workspacePath,
      sandboxRoot: this.sandboxRoot,
      runtime: this.runtime,
      lastError: null,
    };
  }

  async close(options?: { deleteData?: boolean }): Promise<void> {
    this.closeCalls += 1;
    this.closeOptions.push(options);
    if (this.failClose) {
      throw new Error('close failed');
    }
    this.state = 'closed';
  }
}

interface Harness {
  readonly calls: StartSandboxOptions[];
  readonly instances: FakeInstance[];
  readonly deps: PeriSandboxMcpRuntimeDependencies;
}

function harness(delayMs = 0): Harness {
  const calls: StartSandboxOptions[] = [];
  const instances: FakeInstance[] = [];
  const deps: PeriSandboxMcpRuntimeDependencies = {
    startSandbox: async (options) => {
      calls.push(options);
      if (delayMs > 0) {
        await Bun.sleep(delayMs);
      }
      const instance = new FakeInstance(options);
      instances.push(instance);
      return instance as unknown as SandboxInstance;
    },
  };
  return { calls, instances, deps };
}

interface Fixture {
  readonly root: string;
  readonly sandboxesRoot: string;
  readonly workspace: string;
  readonly other: string;
  readonly uri: WorkspaceUri;
  readonly otherUri: WorkspaceUri;
}

function privateDirectory(path: string): string {
  mkdirSync(path, { recursive: true, mode: 0o700 });
  chmodSync(path, 0o700);
  return realpathSync(path);
}

function fixture(): Fixture {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'peri-rivet-sandbox-')));
  const sandboxesRoot = privateDirectory(join(root, 'sandboxes'));
  const workspace = privateDirectory(join(root, 'workspace'));
  const other = privateDirectory(join(root, 'other'));
  return {
    root,
    sandboxesRoot,
    workspace,
    other,
    uri: formatWorkspaceUri({ machineId: MACHINE_ID, runtimePath: workspace }),
    otherUri: formatWorkspaceUri({ machineId: MACHINE_ID, runtimePath: other }),
  };
}

function runtime(fixtureValue: Fixture, deps: Harness['deps']) {
  return createPeriSandboxMcpRuntimeWith(
    {
      machineId: MACHINE_ID,
      workspaces: [{ uri: fixtureValue.uri, path: fixtureValue.workspace }],
      sandboxesRoot: fixtureValue.sandboxesRoot,
      settingsPath: join(fixtureValue.root, 'settings.json'),
    },
    deps,
  );
}

describe('createPeriSandboxMcpRuntime authorization', () => {
  test('rejects foreign machines and URI/path mismatches', () => {
    const f = fixture();
    try {
      const foreign = formatWorkspaceUri({ machineId: OTHER_MACHINE, runtimePath: f.workspace });
      expect(() =>
        createPeriSandboxMcpRuntimeWith(
          { machineId: MACHINE_ID, workspaces: [{ uri: foreign, path: f.workspace }] },
          harness().deps,
        ),
      ).toThrow(/another machine/);

      expect(() =>
        createPeriSandboxMcpRuntimeWith(
          { machineId: MACHINE_ID, workspaces: [{ uri: f.uri, path: f.other }] },
          harness().deps,
        ),
      ).toThrow(/does not match/);

      expect(() =>
        createPeriSandboxMcpRuntimeWith(
          { machineId: MACHINE_ID, workspaces: [{ uri: `${f.uri}/`, path: f.workspace }] },
          harness().deps,
        ),
      ).toThrow(/workspace URI/);
    } finally {
      rmSync(f.root, { recursive: true, force: true });
    }
  });

  test('rejects missing, symlinked, non-private, non-canonical and overlapping directories', () => {
    const f = fixture();
    try {
      expect(() =>
        createPeriSandboxMcpRuntimeWith(
          {
            machineId: MACHINE_ID,
            workspaces: [
              {
                uri: formatWorkspaceUri({ machineId: MACHINE_ID, runtimePath: join(f.root, 'missing') }),
                path: join(f.root, 'missing'),
              },
            ],
          },
          harness().deps,
        ),
      ).toThrow(/existing real directory/);

      const link = join(f.root, 'workspace-link');
      symlinkSync(f.workspace, link);
      expect(() =>
        createPeriSandboxMcpRuntimeWith(
          {
            machineId: MACHINE_ID,
            workspaces: [
              { uri: formatWorkspaceUri({ machineId: MACHINE_ID, runtimePath: link }), path: link },
            ],
          },
          harness().deps,
        ),
      ).toThrow(/existing real directory/);

      chmodSync(f.workspace, 0o755);
      expect(() =>
        createPeriSandboxMcpRuntimeWith(
          {
            machineId: MACHINE_ID,
            workspaces: [{ uri: f.uri, path: f.workspace }],
            sandboxesRoot: f.sandboxesRoot,
          },
          harness().deps,
        ),
      ).toThrow(/private/);
      chmodSync(f.workspace, 0o700);

      const inside = privateDirectory(join(f.sandboxesRoot, 'inside'));
      expect(() =>
        createPeriSandboxMcpRuntimeWith(
          {
            machineId: MACHINE_ID,
            workspaces: [
              { uri: formatWorkspaceUri({ machineId: MACHINE_ID, runtimePath: inside }), path: inside },
            ],
            sandboxesRoot: f.sandboxesRoot,
          },
          harness().deps,
        ),
      ).toThrow(/overlap/);

      expect(() =>
        createPeriSandboxMcpRuntimeWith(
          {
            machineId: MACHINE_ID,
            workspaces: [
              { uri: formatWorkspaceUri({ machineId: MACHINE_ID, runtimePath: f.root }), path: f.root },
            ],
            sandboxesRoot: f.sandboxesRoot,
          },
          harness().deps,
        ),
      ).toThrow(/overlap/);

      expect(() =>
        createPeriSandboxMcpRuntimeWith(
          {
            machineId: MACHINE_ID,
            workspaces: [{ uri: f.uri, path: `${f.workspace}/../workspace` }],
            sandboxesRoot: f.sandboxesRoot,
          },
          harness().deps,
        ),
      ).toThrow(/normalized absolute path/);
    } finally {
      rmSync(f.root, { recursive: true, force: true });
    }
  });

  test('rejects duplicate URI and duplicate real path grants', () => {
    const f = fixture();
    try {
      expect(() =>
        createPeriSandboxMcpRuntimeWith(
          {
            machineId: MACHINE_ID,
            workspaces: [
              { uri: f.uri, path: f.workspace },
              { uri: f.uri, path: f.workspace },
            ],
          },
          harness().deps,
        ),
      ).toThrow(/duplicated/);

      expect(() =>
        createPeriSandboxMcpRuntimeWith(
          {
            machineId: MACHINE_ID,
            workspaces: [
              { uri: f.uri, path: f.workspace },
              { uri: f.otherUri, path: f.workspace },
            ],
          },
          harness().deps,
        ),
      ).toThrow(/duplicated/);
    } finally {
      rmSync(f.root, { recursive: true, force: true });
    }
  });
});

describe('createPeriSandboxMcpRuntime lifecycle', () => {
  test('start uses the explicit workspace and no URI-derived runtime identity; repeated starts reuse the instance', async () => {
    const f = fixture();
    try {
      const h = harness();
      const rt = runtime(f, h.deps);
      await rt.start(f.uri);
      expect(h.calls).toHaveLength(1);
      expect(h.calls[0]).toMatchObject({
        workspacePath: f.workspace,
        host: '127.0.0.1',
        port: 0,
        sandboxesRoot: f.sandboxesRoot,
        settingsPath: join(f.root, 'settings.json'),
      });
      // The canonical URI is the only workspace identity. peri-sandbox may
      // create an opaque instance handle for its own state directory, but the
      // adapter must not derive it from the URI or pass one in.
      expect(h.calls[0]?.sessionKey).toBeUndefined();
      expect(h.calls[0]?.sandboxId).toBeUndefined();
      expect(JSON.stringify(h.calls[0])).not.toContain(f.uri);
      expect(JSON.stringify(h.calls[0])).not.toContain('workspace:');
      expect(rt.stateOf(f.uri)).toBe('ready');

      await rt.start(f.uri);
      expect(h.calls).toHaveLength(1);

      await rt.close(f.uri);
      expect(h.instances[0]?.closeCalls).toBe(1);
      expect(h.instances[0]?.closeOptions).toEqual([{ deleteData: true }]);
      expect(rt.stateOf(f.uri)).toBeUndefined();

      await rt.start(f.uri);
      expect(h.calls).toHaveLength(2);
      await rt.closeAll();
    } finally {
      rmSync(f.root, { recursive: true, force: true });
    }
  });

  test('rejects a URI that names a symlink alias of the authorized directory', () => {
    const f = fixture();
    try {
      const link = join(f.root, 'workspace-alias');
      symlinkSync(f.workspace, link);
      expect(() =>
        createPeriSandboxMcpRuntimeWith(
          {
            machineId: MACHINE_ID,
            workspaces: [
              { uri: formatWorkspaceUri({ machineId: MACHINE_ID, runtimePath: link }), path: f.workspace },
            ],
            sandboxesRoot: f.sandboxesRoot,
          },
          harness().deps,
        ),
      ).toThrow(/does not match/);
    } finally {
      rmSync(f.root, { recursive: true, force: true });
    }
  });

  test('concurrent starts share one spawn', async () => {
    const f = fixture();
    try {
      const h = harness(10);
      const rt = runtime(f, h.deps);
      await Promise.all([rt.start(f.uri), rt.start(f.uri)]);
      expect(h.calls).toHaveLength(1);
      expect(rt.stateOf(f.uri)).toBe('ready');
      await rt.closeAll();
    } finally {
      rmSync(f.root, { recursive: true, force: true });
    }
  });

  test('close failure keeps the instance for retry; closeAll aggregates failures', async () => {
    const f = fixture();
    try {
      const h = harness();
      const rt = createPeriSandboxMcpRuntimeWith(
        {
          machineId: MACHINE_ID,
          workspaces: [
            { uri: f.uri, path: f.workspace },
            { uri: f.otherUri, path: f.other },
          ],
          sandboxesRoot: f.sandboxesRoot,
        },
        h.deps,
      );
      await rt.start(f.uri);
      await rt.start(f.otherUri);
      const first = h.instances[0];
      if (first === undefined) throw new Error('missing first instance');
      first.failClose = true;
      await expect(rt.close(f.uri)).rejects.toThrow('close failed');
      expect(rt.stateOf(f.uri)).toBe('ready');

      await expect(rt.closeAll()).rejects.toThrow(AggregateError);
      expect(first.closeCalls).toBeGreaterThanOrEqual(2);
      expect(rt.stateOf(f.uri)).toBe('ready');
      expect(rt.stateOf(f.otherUri)).toBeUndefined();

      first.failClose = false;
      await rt.close(f.uri);
      expect(rt.stateOf(f.uri)).toBeUndefined();
    } finally {
      rmSync(f.root, { recursive: true, force: true });
    }
  });
});

describe('createPeriSandboxMcpRuntime fetchMcp', () => {
  test('forwards method/body/headers to the inner MCP with the inner token only', async () => {
    const f = fixture();
    const received: Array<{
      headers: IncomingHttpHeaders;
      method?: string | undefined;
      url?: string | undefined;
      body: string;
    }> = [];
    const server = createServer(async (req, res) => {
      const chunks: Buffer[] = [];
      for await (const chunk of req) {
        chunks.push(Buffer.from(chunk));
      }
      received.push({
        headers: req.headers,
        method: req.method,
        url: req.url,
        body: Buffer.concat(chunks).toString('utf8'),
      });
      res.writeHead(200, { 'content-type': 'application/json', 'mcp-session-id': 'inner-session' });
      res.end(JSON.stringify({ ok: true }));
    });
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    const address = server.address();
    if (address === null || typeof address === 'string') {
      throw new Error('test server has no port');
    }
    const h = harness();
    try {
      const rt = runtime(f, h.deps);
      expect((await rt.fetchMcp(f.uri, new Request('http://127.0.0.1/mcp'))).status).toBe(503);

      await rt.start(f.uri);
      const started = h.instances[0];
      if (started === undefined) throw new Error('missing started instance');
      started.mcpUrl = `http://127.0.0.1:${address.port}/mcp`;
      const response = await rt.fetchMcp(
        f.uri,
        new Request('http://127.0.0.1/mcp', {
          method: 'POST',
          headers: {
            'content-type': 'application/json',
            'mcp-session-id': 'outer-session',
            'x-peri-sandbox-token': ACTOR_TOKEN,
            'x-rivet-token': 'gateway-token',
            authorization: 'Bearer outer-token',
            cookie: 'session=secret',
          },
          body: '{"jsonrpc":"2.0","id":1,"method":"tools/list"}',
        }),
      );
      expect(response.status).toBe(200);
      expect(response.headers.get('mcp-session-id')).toBe('inner-session');
      expect(await response.text()).toBe('{"ok":true}');
      expect(received).toHaveLength(1);
      const request = received[0];
      if (request === undefined) throw new Error('missing forwarded request');
      expect(request.method).toBe('POST');
      expect(request.url).toBe('/mcp');
      expect(request.body).toBe('{"jsonrpc":"2.0","id":1,"method":"tools/list"}');
      expect(request.headers.authorization).toBe(`Bearer ${INNER_TOKEN}`);
      expect(request.headers['x-peri-sandbox-token']).toBeUndefined();
      expect(request.headers['x-rivet-token']).toBeUndefined();
      expect(request.headers.cookie).toBeUndefined();

      const failed = h.instances[0];
      if (failed === undefined) throw new Error('missing failed instance');
      failed.state = 'failed';
      expect((await rt.fetchMcp(f.uri, new Request('http://127.0.0.1/mcp'))).status).toBe(503);
      expect((await rt.fetchMcp(f.otherUri, new Request('http://127.0.0.1/mcp'))).status).toBe(403);
    } finally {
      await new Promise<void>((resolve) => server.close(() => resolve()));
      rmSync(f.root, { recursive: true, force: true });
    }
  });
});
