import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, rmSync, existsSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { EventEmitter } from 'node:events';
import { Registry } from '../src/control/registry.js';
import { Control } from '../src/control/control.js';
import { checkDirectory, policyFor } from '../src/runtime/policy.js';
import type { Config } from '../src/config.js';
import type { SystemdProvider } from '../src/runtime/provider.js';

function fixture() {
  const root = mkdtempSync(join(tmpdir(), 'peri-sandbox-test-'));
  const config = {
    controlRoot: join(root, 'control'),
    sandboxesRoot: join(root, 'sandboxes'),
    publicBaseUrl: 'http://127.0.0.1:8787',
    profiles: {
      minimal: { allowedDomains: [], runtimeReadPaths: ['/usr'], memoryMax: '1G', tasksMax: 10, cpuQuota: '100%' },
    },
    limits: { maxSandboxes: 4, startupMs: 1000, stopMs: 1000, bashCalls: 10 },
    provider: { unitPrefix: 'test-', apiUnit: 'test-api.service' },
  } as unknown as Config;
  mkdirSync(config.controlRoot);
  mkdirSync(config.sandboxesRoot);
  const registry = new Registry(join(config.controlRoot, 'state.db'));
  let starts = 0,
    stops = 0;
  const provider = {
    unit: (id: string, generation: string) => `test-${id}-${generation}.service`,
    start: async () => {
      starts++;
      return { unit: 'unit', process: new EventEmitter() };
    },
    stop: async () => {
      stops++;
    },
    listOwned: async () => [],
  } as unknown as SystemdProvider;
  const control = new Control(config, registry, provider, async () => ({ close: async () => {} }));
  return {
    root,
    config,
    registry,
    provider,
    control,
    counts: () => ({ starts, stops }),
    close: () => {
      registry.close();
      rmSync(root, { recursive: true, force: true });
    },
  };
}

test('create, start, stop, restart, tombstone and operation receipts', async () => {
  const setup = fixture();
  try {
    const first = await setup.control.create({ profile: 'minimal' }, { requestId: 'create-one' });
    assert.equal((await setup.control.create({ profile: 'minimal' }, { requestId: 'create-one' })).id, first.id);
    await assert.rejects(
      () => setup.control.create({ profile: 'missing' }, { requestId: 'create-one' }),
      /different input/,
    );
    const running = await setup.control.start(first.id, { requestId: 'start-one' });
    assert.equal(running.state, 'ready');
    assert.equal(setup.counts().starts, 1);
    await setup.control.stop(first.id, { requestId: 'stop-one' });
    assert.equal((await setup.control.start(first.id, { requestId: 'start-one' })).generation, running.generation);
    assert.equal((await setup.control.inspect(first.id)).state, 'stopped');
    const restarted = await setup.control.start(first.id, { requestId: 'start-two' });
    assert.notEqual(restarted.generation, running.generation);
    await setup.control.destroy(first.id, { requestId: 'destroy-one', deleteData: true });
    assert.equal((await setup.control.inspect(first.id)).state, 'deleted');
    assert.deepEqual(await setup.control.list(), []);
    assert.equal(setup.counts().starts, 2);
    assert.equal(setup.counts().stops, 2);
  } finally {
    setup.close();
  }
});

test('policy denies root and path checks reject symlinks and traversal', () => {
  const setup = fixture();
  try {
    const id = `sbx_${'a'.repeat(32)}`;
    const { json } = policyFor(setup.config, id, 'minimal');
    assert.deepEqual(JSON.parse(json).filesystem.denyRead, ['/']);
    assert.throws(() => checkDirectory(join(setup.root, 'elsewhere'), setup.config.sandboxesRoot), /outside/);
  } finally {
    setup.close();
  }
});

test('start refuses a full data filesystem before allocating a generation', async () => {
  const setup = fixture();
  try {
    const created = await setup.control.create({ profile: 'minimal' }, { requestId: 'create' });
    setup.config.limits.minFreeBytes = Number.MAX_SAFE_INTEGER;
    await assert.rejects(() => setup.control.start(created.id, { requestId: 'start' }), /free-space watermark/);
    assert.equal((await setup.control.inspect(created.id)).state, 'stopped');
    assert.equal(setup.counts().starts, 0);
  } finally {
    setup.close();
  }
});

test('registry refuses to recreate an initialized database that went missing', () => {
  const setup = fixture();
  try {
    setup.registry.close();
    rmSync(join(setup.config.controlRoot, 'state.db'));
    assert.throws(() => new Registry(join(setup.config.controlRoot, 'state.db')), /registry database missing/);
  } finally {
    rmSync(setup.root, { recursive: true, force: true });
  }
});

test('a stalled provider launch times out and attempts unit cleanup', async () => {
  const setup = fixture();
  try {
    setup.config.limits.startupMs = 20;
    setup.provider.start = async () => new Promise(() => {});
    const created = await setup.control.create({ profile: 'minimal' }, { requestId: 'create' });
    await assert.rejects(() => setup.control.start(created.id, { requestId: 'start' }), /provider launch timeout/);
    assert.equal(setup.counts().stops, 1);
    assert.equal((await setup.control.inspect(created.id)).state, 'failed');
  } finally {
    setup.close();
  }
});

test('registry failure while provider launches stops its late writer', async () => {
  const setup = fixture();
  try {
    let release!: () => void;
    setup.provider.start = async () => {
      await new Promise<void>((done) => {
        release = done;
      });
      return { unit: 'unit', process: new EventEmitter() } as never;
    };
    const created = await setup.control.create({ profile: 'minimal' }, { requestId: 'create' });
    const started = setup.control.start(created.id, { requestId: 'start' });
    await new Promise((done) => setTimeout(done, 10));
    setup.registry.unhealthy = true;
    release();
    await assert.rejects(started, /registry unavailable/);
    assert.equal(setup.counts().stops, 1);
  } finally {
    setup.close();
  }
});

test('two concurrent stop requests share cleanup and cannot erase a later start', async () => {
  const setup = fixture();
  try {
    const created = await setup.control.create({ profile: 'minimal' }, { requestId: 'create' });
    await setup.control.start(created.id, { requestId: 'start' });
    let release!: () => void;
    const suspended = new Promise<void>((resolve) => {
      release = resolve;
    });
    const originalStop = setup.provider.stop.bind(setup.provider);
    setup.provider.stop = async (unit) => {
      await suspended;
      return originalStop(unit);
    };
    const first = setup.control.stop(created.id, { requestId: 'stop-1' });
    const second = setup.control.stop(created.id, { requestId: 'stop-2' });
    release();
    await Promise.all([first, second]);
    const restarted = await setup.control.start(created.id, { requestId: 'start-again' });
    assert.equal((await setup.control.inspect(created.id)).generation, restarted.generation);
    assert.equal(setup.counts().stops, 1);
  } finally {
    setup.close();
  }
});

test('database failure does not prevent process cleanup', async () => {
  const setup = fixture();
  try {
    const created = await setup.control.create({ profile: 'minimal' }, { requestId: 'create' });
    await setup.control.start(created.id, { requestId: 'start' });
    const originalPut = setup.registry.put.bind(setup.registry);
    setup.registry.put = (record) => {
      if (record.state === 'stopping') throw new Error('SQLITE_FULL');
      originalPut(record);
    };
    await assert.rejects(() => setup.control.stop(created.id, { requestId: 'stop' }), /database unavailable/);
    assert.equal(setup.counts().stops, 1);
  } finally {
    setup.close();
  }
});

test('startup failure cleans unit and leaves restartable record', async () => {
  const setup = fixture();
  try {
    const created = await setup.control.create({ profile: 'minimal' }, { requestId: 'create' });
    setup.provider.start = async () => {
      throw new Error('cannot start');
    };
    await assert.rejects(() => setup.control.start(created.id, { requestId: 'start' }), /cannot start/);
    const failed = await setup.control.inspect(created.id);
    assert.equal(failed.state, 'failed');
    assert.equal(failed.generation, undefined);
    assert.equal(setup.counts().stops, 1);
    assert.equal(
      (await setup.control.destroy(created.id, { requestId: 'destroy', deleteData: true })).state,
      'deleted',
    );
  } finally {
    setup.close();
  }
});

test('stop revokes an unfinished startup and cleans its unit', async () => {
  const setup = fixture();
  try {
    const created = await setup.control.create({ profile: 'minimal' }, { requestId: 'create' });
    const starting = setup.control.start(created.id, { requestId: 'start' });
    const stopped = setup.control.stop(created.id, { requestId: 'stop' });
    await assert.rejects(starting, /cancelled/);
    assert.equal((await stopped).state, 'stopped');
    assert.equal((await setup.control.inspect(created.id)).state, 'stopped');
    assert.ok(setup.counts().stops >= setup.counts().starts);
  } finally {
    setup.close();
  }
});

test('start cannot overtake a pending stop or destroy', async () => {
  const setup = fixture();
  try {
    const { id } = await setup.control.create({ profile: 'minimal' }, { requestId: 'create' });
    await setup.control.start(id, { requestId: 'start' });
    let release!: () => void;
    const blocked = new Promise<void>((resolve) => {
      release = resolve;
    });
    const originalStop = setup.provider.stop.bind(setup.provider);
    setup.provider.stop = async (unit) => {
      await blocked;
      return originalStop(unit);
    };
    const stopping = setup.control.stop(id, { requestId: 'stop' });
    await assert.rejects(
      () => setup.control.start(id, { requestId: 'racing-start' }),
      /cancelled|cannot start from stopping/,
    );
    release();
    await stopping;
    const destroying = setup.control.destroy(id, { requestId: 'destroy', deleteData: true });
    await assert.rejects(() => setup.control.start(id, { requestId: 'racing-destroy' }), /destroy in progress|deleted/);
    await destroying;
  } finally {
    setup.close();
  }
});

test('failed unit cleanup blocks restart and deletion until confirmed', async () => {
  const setup = fixture();
  try {
    const { id } = await setup.control.create({ profile: 'minimal' }, { requestId: 'create' });
    await setup.control.start(id, { requestId: 'start' });
    const originalStop = setup.provider.stop.bind(setup.provider);
    setup.provider.stop = async () => {
      throw new Error('cgroup populated');
    };
    await assert.rejects(() => setup.control.stop(id, { requestId: 'stop' }), /cgroup populated/);
    assert.equal((await setup.control.inspect(id)).state, 'stop_incomplete');
    await assert.rejects(() => setup.control.start(id, { requestId: 'restart' }), /cannot start/);
    await assert.rejects(
      () => setup.control.destroy(id, { requestId: 'destroy', deleteData: true }),
      /cgroup populated/,
    );
    assert.ok(existsSync(join(setup.config.sandboxesRoot, id)));
    setup.provider.stop = originalStop;
    await setup.control.stop(id, { requestId: 'retry-stop' });
    assert.equal((await setup.control.inspect(id)).state, 'stopped');
  } finally {
    setup.close();
  }
});

test('failed ready write closes backend and confirms unit cleanup', async () => {
  const setup = fixture();
  try {
    const { id } = await setup.control.create({ profile: 'minimal' }, { requestId: 'create' });
    let closed = 0;
    const control = new Control(setup.config, setup.registry, setup.provider, async () => ({
      close: async () => {
        closed++;
      },
    }));
    const originalPut = setup.registry.put.bind(setup.registry);
    setup.registry.put = (record) => {
      if (record.state === 'ready') throw new Error('SQLITE_FULL');
      originalPut(record);
    };
    await assert.rejects(() => control.start(id, { requestId: 'start' }), /SQLITE_FULL/);
    assert.equal(closed, 1);
    assert.equal(setup.counts().stops, 1);
    assert.equal((await control.inspect(id)).state, 'failed');
  } finally {
    setup.close();
  }
});

test('recovery cleans orphan units even when database read fails', async () => {
  const setup = fixture();
  try {
    const cleaned: string[] = [];
    setup.provider.listOwned = async () => ['test-orphan.service'];
    setup.provider.stop = async (unit) => {
      cleaned.push(unit);
    };
    setup.registry.list = () => {
      throw new Error('SQLITE_CORRUPT');
    };
    await assert.rejects(() => setup.control.recover(), /cannot reconcile/);
    assert.deepEqual(cleaned, ['test-orphan.service']);
  } finally {
    setup.close();
  }
});

test('recovery preserves uncleaned generation and interrupts pending receipt', async () => {
  const setup = fixture();
  try {
    const { id } = await setup.control.create({ profile: 'minimal' }, { requestId: 'create' });
    const running = await setup.control.start(id, { requestId: 'start' });
    setup.registry.begin('lost', 'start', { id }, id);
    setup.provider.stop = async () => {
      throw new Error('still populated');
    };
    await assert.rejects(() => setup.control.recover(), /could not clean/);
    assert.equal((await setup.control.inspect(id)).state, 'stop_incomplete');
    assert.equal((await setup.control.inspect(id)).generation, running.generation);
    assert.equal(setup.registry.receipt('lost')?.status, 'interrupted');
  } finally {
    setup.close();
  }
});

test('destroy rejects symlinked sandbox without deleting its target', async () => {
  const setup = fixture();
  try {
    const { id } = await setup.control.create({ profile: 'minimal' }, { requestId: 'create' });
    const target = join(setup.root, 'outside');
    mkdirSync(target);
    rmSync(join(setup.config.sandboxesRoot, id), { recursive: true });
    symlinkSync(target, join(setup.config.sandboxesRoot, id));
    await assert.rejects(() => setup.control.destroy(id, { requestId: 'destroy', deleteData: true }), /symlink/);
    assert.ok(existsSync(target));
    assert.equal((await setup.control.inspect(id)).state, 'destroying');
  } finally {
    setup.close();
  }
});

test('startup timeout cleans unit and closes backend that arrives late', async () => {
  const setup = fixture();
  try {
    const { id } = await setup.control.create({ profile: 'minimal' }, { requestId: 'create' });
    setup.config.limits.startupMs = 5;
    let resolveBackend!: (backend: { close(): Promise<void> }) => void;
    let closed = 0;
    const control = new Control(
      setup.config,
      setup.registry,
      setup.provider,
      async () =>
        new Promise((resolve) => {
          resolveBackend = resolve;
        }),
    );
    await assert.rejects(() => control.start(id, { requestId: 'start' }), /timeout/);
    assert.equal((await control.inspect(id)).state, 'failed');
    assert.equal(setup.counts().stops, 1);
    resolveBackend({
      close: async () => {
        closed++;
      },
    });
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(closed, 1);
  } finally {
    setup.close();
  }
});

test('process exit before backend readiness never publishes ready', async () => {
  const setup = fixture();
  try {
    const { id } = await setup.control.create({ profile: 'minimal' }, { requestId: 'create' });
    setup.provider.start = async () => {
      const process = new EventEmitter();
      setImmediate(() => process.emit('close'));
      return { unit: 'unit', process } as Awaited<ReturnType<SystemdProvider['start']>>;
    };
    const control = new Control(setup.config, setup.registry, setup.provider, async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
      return { close: async () => {} };
    });
    await assert.rejects(() => control.start(id, { requestId: 'start' }), /exited before readiness/);
    assert.equal((await control.inspect(id)).state, 'failed');
  } finally {
    setup.close();
  }
});

test('concurrent starts share one generation and preserve each receipt', async () => {
  const setup = fixture();
  try {
    const { id } = await setup.control.create({ profile: 'minimal' }, { requestId: 'create' });
    let release!: () => void;
    const blocked = new Promise<void>((resolve) => {
      release = resolve;
    });
    const control = new Control(setup.config, setup.registry, setup.provider, async () => {
      await blocked;
      return { close: async () => {} };
    });
    const first = control.start(id, { requestId: 'start-1' });
    const second = control.start(id, { requestId: 'start-2' });
    release();
    const [one, two] = await Promise.all([first, second]);
    assert.equal(one.generation, two.generation);
    assert.equal(setup.counts().starts, 1);
    await control.stop(id, { requestId: 'stop' });
    assert.equal((await control.start(id, { requestId: 'start-1' })).generation, one.generation);
  } finally {
    setup.close();
  }
});

test('recovery stops all units before attempting any failed database write', async () => {
  const setup = fixture();
  try {
    const first = await setup.control.create({ profile: 'minimal' }, { requestId: 'create-1' });
    const second = await setup.control.create({ profile: 'minimal' }, { requestId: 'create-2' });
    await setup.control.start(first.id, { requestId: 'start-1' });
    await setup.control.start(second.id, { requestId: 'start-2' });
    const cleaned: string[] = [];
    setup.provider.listOwned = async () => ['test-orphan.service'];
    setup.provider.stop = async (unit) => {
      cleaned.push(unit);
    };
    setup.registry.put = () => {
      throw new Error('SQLITE_IOERR');
    };
    await assert.rejects(() => setup.control.recover(), /SQLITE_IOERR/);
    assert.equal(cleaned.length, 3);
  } finally {
    setup.close();
  }
});
