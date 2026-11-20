import assert from 'node:assert/strict';
import { existsSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { SwarmError } from '../../src/swarm/errors.js';
import {
  defaultSwarmSandboxesRoot,
  prepareSwarmRoot,
  resolveSwarmSandboxesRoot,
  SwarmManager,
} from '../../src/swarm/manager.js';
import { createFakeStarter, temporarySwarmRoot } from './harness.js';

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
