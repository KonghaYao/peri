import assert from 'node:assert/strict';
import { existsSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { SwarmError } from '../../src/swarm/errors.js';
import {
  DEFAULT_SWARM_IDLE_MS,
  defaultSwarmSandboxesRoot,
  idleSweepIntervalMs,
  prepareSwarmRoot,
  resolveSwarmIdleMs,
  resolveSwarmSandboxesRoot,
  SwarmManager,
} from '../../src/swarm/manager.js';
import { createFakeStarter, temporarySwarmRoot } from './harness.js';

function controllableClock(start = '2026-01-01T00:00:00.000Z'): {
  now: () => Date;
  advance: (milliseconds: number) => void;
} {
  let current = new Date(start);
  return {
    now: () => current,
    advance: (milliseconds) => {
      current = new Date(current.getTime() + milliseconds);
    },
  };
}

test('default persistence root is ~/.peri-swarm/sandboxes', () => {
  assert.equal(defaultSwarmSandboxesRoot(), join(homedir(), '.peri-swarm', 'sandboxes'));
  assert.equal(resolveSwarmSandboxesRoot(undefined), defaultSwarmSandboxesRoot());
  assert.equal(resolveSwarmSandboxesRoot(''), defaultSwarmSandboxesRoot());
});

test('an explicit root must be absolute', () => {
  assert.throws(
    () => resolveSwarmSandboxesRoot('relative/sandboxes'),
    (error: unknown) => error instanceof SwarmError && error.code === 'invalid_config',
  );
});

test('manager creates a private root', (t) => {
  const root = temporarySwarmRoot(t);
  const manager = new SwarmManager({ root, startSandbox: createFakeStarter().start });
  assert.equal(manager.root, prepareSwarmRoot(root));
  assert.equal(statSync(manager.root).mode & 0o777, 0o700);
  assert.equal(existsSync(join(manager.root, 'bindings.json')), false);
});

test('concurrent ensure starts the sandbox exactly once', async (t) => {
  const root = temporarySwarmRoot(t);
  const fake = createFakeStarter({ delayMs: 25 });
  const manager = new SwarmManager({ root, startSandbox: fake.start });
  try {
    const [first, second, third] = await Promise.all([
      manager.ensure('env-concurrent'),
      manager.ensure('env-concurrent'),
      manager.ensure('env-concurrent'),
    ]);
    assert.equal(fake.calls.length, 1);
    assert.equal(first, second);
    assert.equal(second, third);
    assert.equal(first?.runId, 'run_fake_1');
    assert.equal(first?.sandboxId, fake.calls[0]?.options.sandboxId);
    assert.equal(first?.workspacePath, join(manager.root, first?.sandboxId ?? '', 'workspace'));
    assert.equal(first?.instance.state, 'ready');
  } finally {
    await manager.closeAll();
  }
});

test('manager opts into the peri binary directory for sandbox starts', async (t) => {
  const root = temporarySwarmRoot(t);
  const fake = createFakeStarter();
  const manager = new SwarmManager({ root, startSandbox: fake.start });
  try {
    await manager.ensure('env-peri-binary');
    assert.equal(fake.calls.length, 1);
    assert.equal(fake.calls[0]?.options.allowPeriBinaryDirectory, true);
  } finally {
    await manager.closeAll();
  }
});

test('close keeps data and re-ensure reuses the directory with a new run', async (t) => {
  const root = temporarySwarmRoot(t);
  const fake = createFakeStarter();
  const manager = new SwarmManager({ root, startSandbox: fake.start });
  const first = await manager.ensure('env-persist');
  const marker = join(first.workspacePath, 'keep.txt');
  writeFileSync(marker, 'kept');

  const closed = await manager.close('env-persist', { deleteData: false });
  assert.deepEqual(closed, { found: true, closed: true, deletedData: false });
  assert.equal(existsSync(first.instance.sandboxRoot), true);
  assert.equal(readFileSync(marker, 'utf8'), 'kept');
  assert.equal(manager.get('env-persist'), undefined);
  assert.equal(manager.binding('env-persist')?.lastRunId, first.runId);

  const second = await manager.ensure('env-persist');
  assert.equal(fake.calls.length, 2);
  assert.notEqual(second.runId, first.runId);
  assert.equal(second.sandboxId, first.sandboxId);
  assert.equal(second.workspacePath, first.workspacePath);
  assert.equal(readFileSync(marker, 'utf8'), 'kept');
  assert.equal(manager.binding('env-persist')?.lastRunId, second.runId);

  const deleted = await manager.close('env-persist', { deleteData: true });
  assert.deepEqual(deleted, { found: true, closed: true, deletedData: true });
  assert.equal(existsSync(first.instance.sandboxRoot), false);
  assert.equal(manager.binding('env-persist'), undefined);
  assert.equal(manager.get('env-persist'), undefined);
});

test('close without data keeps bindings and removes data only when asked', async (t) => {
  const root = temporarySwarmRoot(t);
  const manager = new SwarmManager({ root, startSandbox: createFakeStarter().start });
  await manager.ensure('env-close');
  await manager.close('env-close', { deleteData: false });
  assert.ok(manager.binding('env-close') !== undefined);
  const missing = await manager.close('env-close', { deleteData: false });
  assert.deepEqual(missing, { found: true, closed: false, deletedData: false });
  const unknown = await manager.close('env-missing', { deleteData: false });
  assert.deepEqual(unknown, { found: false, closed: false, deletedData: false });
});

test('PERI_SWARM_IDLE_MS parsing and sweep cadence', () => {
  assert.equal(DEFAULT_SWARM_IDLE_MS, 30 * 60_000);
  assert.equal(resolveSwarmIdleMs(undefined), DEFAULT_SWARM_IDLE_MS);
  assert.equal(resolveSwarmIdleMs('  '), DEFAULT_SWARM_IDLE_MS);
  assert.equal(resolveSwarmIdleMs('0'), 0);
  assert.equal(resolveSwarmIdleMs('2000'), 2000);
  for (const invalid of ['-1', '1.5', 'soon', '99999999999999999999'])
    assert.throws(
      () => resolveSwarmIdleMs(invalid),
      (error: unknown) => error instanceof SwarmError && error.code === 'invalid_config',
      invalid,
    );
  assert.equal(idleSweepIntervalMs(0), 0);
  assert.equal(idleSweepIntervalMs(2000), 500);
  assert.equal(idleSweepIntervalMs(100), 250);
  assert.equal(idleSweepIntervalMs(30 * 60_000), 30_000);
});

test('an invalid idle threshold is rejected', (t) => {
  const root = temporarySwarmRoot(t);
  assert.throws(
    () => new SwarmManager({ root, idleMs: -1 }),
    (error: unknown) => error instanceof SwarmError && error.code === 'invalid_config',
  );
});

test('idle sweep reclaims a ready environment, keeps data and bindings, and re-ensures a new run', async (t) => {
  const root = temporarySwarmRoot(t);
  const fake = createFakeStarter();
  const clock = controllableClock();
  const manager = new SwarmManager({ root, startSandbox: fake.start, now: clock.now, idleMs: 1000 });
  try {
    const first = await manager.ensure('env-idle');
    const marker = join(first.workspacePath, 'slow.txt');
    writeFileSync(marker, 'slow\n');

    clock.advance(999);
    assert.deepEqual(await manager.sweepIdle(), { reclaimed: [], inFlight: [] });
    assert.equal(manager.get('env-idle')?.runId, first.runId);

    clock.advance(1);
    const swept = await manager.sweepIdle();
    assert.deepEqual(swept, { reclaimed: ['env-idle'], inFlight: [] });
    assert.equal(manager.get('env-idle'), undefined);
    const cold = manager.describe('env-idle');
    assert.equal(cold?.state, 'reserved');
    assert.equal(cold?.runId, first.runId);
    assert.equal(cold?.workspacePath, first.workspacePath);
    assert.equal(manager.binding('env-idle')?.lastRunId, first.runId);
    assert.equal(fake.instances[0]?.closedWith.at(-1)?.deleteData, false);
    assert.equal(readFileSync(marker, 'utf8'), 'slow\n');

    const second = await manager.ensure('env-idle');
    assert.equal(fake.calls.length, 2);
    assert.notEqual(second.runId, first.runId);
    assert.equal(second.sandboxId, first.sandboxId);
    assert.equal(second.workspacePath, first.workspacePath);
    assert.equal(readFileSync(marker, 'utf8'), 'slow\n');
    assert.equal(manager.binding('env-idle')?.lastRunId, second.runId);
  } finally {
    await manager.closeAll();
  }
});

test('idle sweep never reclaims while a proxied request is in flight', async (t) => {
  const root = temporarySwarmRoot(t);
  const fake = createFakeStarter();
  const clock = controllableClock();
  const manager = new SwarmManager({ root, startSandbox: fake.start, now: clock.now, idleMs: 1000 });
  try {
    await manager.ensure('env-busy');
    manager.beginRequest('env-busy');
    clock.advance(60_000);
    const blocked = await manager.sweepIdle();
    assert.deepEqual(blocked, { reclaimed: [], inFlight: ['env-busy'] });
    assert.equal(manager.get('env-busy')?.runId, 'run_fake_1');
    assert.equal(fake.instances[0]?.state, 'ready');

    // The end of the request restarts the idle window.
    manager.endRequest('env-busy');
    clock.advance(999);
    assert.deepEqual(await manager.sweepIdle(), { reclaimed: [], inFlight: [] });
    clock.advance(1);
    assert.deepEqual(await manager.sweepIdle(), { reclaimed: ['env-busy'], inFlight: [] });
    assert.equal(fake.instances[0]?.state, 'closed');
  } finally {
    await manager.closeAll();
  }
});

test('idleMs 0 disables reclaim', async (t) => {
  const root = temporarySwarmRoot(t);
  const fake = createFakeStarter();
  const clock = controllableClock();
  const manager = new SwarmManager({ root, startSandbox: fake.start, now: clock.now, idleMs: 0 });
  try {
    await manager.ensure('env-forever');
    clock.advance(24 * 60 * 60_000);
    assert.deepEqual(await manager.sweepIdle(), { reclaimed: [], inFlight: [] });
    assert.equal(manager.get('env-forever')?.runId, 'run_fake_1');
  } finally {
    await manager.closeAll();
  }
});

test('bindings.json is 0600, atomic and omits the inner token', async (t) => {
  const root = temporarySwarmRoot(t);
  const manager = new SwarmManager({ root, startSandbox: createFakeStarter().start });
  const entry = await manager.ensure('env-bind');
  try {
    const path = join(manager.root, 'bindings.json');
    assert.equal(statSync(path).mode & 0o777, 0o600);
    const text = readFileSync(path, 'utf8');
    assert.equal(text.includes(entry.innerToken), false);
    const parsed = JSON.parse(text) as {
      version: number;
      bindings: Record<string, Record<string, string>>;
    };
    assert.equal(parsed.version, 1);
    assert.deepEqual(Object.keys(parsed.bindings), ['env-bind']);
    const binding = parsed.bindings['env-bind'];
    assert.ok(binding !== undefined);
    assert.deepEqual(Object.keys(binding).sort(), ['lastRunId', 'lastUsedAt', 'sandboxId', 'workspacePath']);
    assert.equal(binding.sandboxId, entry.sandboxId);
    assert.equal(binding.workspacePath, entry.workspacePath);
    assert.equal(binding.lastRunId, entry.runId);
    assert.equal(Number.isNaN(Date.parse(binding.lastUsedAt)), false);
  } finally {
    await manager.closeAll();
  }
});

test('an explicit stop queued during idle closure wins over a waiting tool request', async (t) => {
  const fake = createFakeStarter();
  const clock = controllableClock();
  const manager = new SwarmManager({
    root: temporarySwarmRoot(t),
    startSandbox: fake.start,
    now: clock.now,
    idleMs: 1,
  });
  const entry = await manager.ensure('env-race');
  const close = entry.instance.close.bind(entry.instance);
  let finish!: () => void;
  const gate = new Promise<void>((resolve) => {
    finish = resolve;
  });
  entry.instance.close = async (options) => {
    await gate;
    return close(options);
  };
  clock.advance(1);
  const sweeping = manager.sweepIdle();
  const waiting = manager.ensureForRequest('env-race');
  const stopped = manager.close('env-race');
  finish();
  await Promise.all([sweeping, stopped]);
  assert.equal(await waiting, undefined);
  assert.equal(manager.describe('env-race')?.state, 'stopped');
  assert.equal(fake.calls.length, 1);
});

test('a failed close is not reported as an idle reclaim or marked wakeable', async (t) => {
  const clock = controllableClock();
  const manager = new SwarmManager({
    root: temporarySwarmRoot(t),
    startSandbox: createFakeStarter().start,
    now: clock.now,
    idleMs: 1,
  });
  const entry = await manager.ensure('env-fail-close');
  const close = entry.instance.close.bind(entry.instance);
  entry.instance.close = async () => {
    throw new Error('provider stop failed');
  };
  clock.advance(1);
  assert.deepEqual((await manager.sweepIdle()).reclaimed, []);
  assert.equal(manager.get('env-fail-close'), entry);
  assert.equal(manager.reservation('env-fail-close'), undefined);
  entry.instance.close = close;
  await manager.closeAll();
});
