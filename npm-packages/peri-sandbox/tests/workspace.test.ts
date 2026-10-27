import assert from 'node:assert/strict';
import {
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import {
  acquireLock,
  deleteSandboxData,
  findStaleLocks,
  makeDirectories,
  resolveToken,
} from '../src/instance/workspace.js';
import { SandboxError } from '../src/instance/types.js';

const id = `sbx_${'a'.repeat(32)}`;
const deadPid = 2 ** 30;

function tempRoot(): string {
  return mkdtempSync(join(tmpdir(), 'peri-workspace-'));
}

test('makeDirectories creates private workspace and home directories', () => {
  const root = tempRoot();
  try {
    makeDirectories(root, id);
    for (const path of [join(root, id, 'workspace'), join(root, id, 'home'), join(root, id, 'home', '.config')]) {
      assert.equal(statSync(path).isDirectory(), true);
      assert.equal(statSync(path).mode & 0o077, 0);
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('workspace lock is exclusive, releasable and clears only dead owners', () => {
  const root = tempRoot();
  try {
    const base = join(root, id);
    mkdirSync(base);
    const lock = acquireLock(base);
    assert.equal(lstatSync(lock.path).mode & 0o077, 0);
    assert.throws(
      () => acquireLock(base),
      (error: unknown) => error instanceof SandboxError && error.code === 'conflict',
    );
    lock.release();
    assert.equal(existsSync(lock.path), false);
    const again = acquireLock(base);
    again.release();

    writeFileSync(lock.path, JSON.stringify({ ownerPid: deadPid }), { mode: 0o600 });
    const stale = findStaleLocks(root);
    assert.deepEqual(stale, [lock.path]);
    const recovered = acquireLock(base);
    recovered.release();
    assert.equal(existsSync(lock.path), false);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('live locks are reported as in use, not stale', () => {
  const root = tempRoot();
  try {
    const base = join(root, id);
    mkdirSync(base);
    const lock = acquireLock(base);
    assert.deepEqual(findStaleLocks(root), []);
    lock.release();
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('tokens default to memory, token files stay inside the sandbox root', () => {
  const root = tempRoot();
  try {
    const base = join(root, id);
    mkdirSync(base);
    assert.match(resolveToken({ sandboxRoot: base }).token, /^[a-f0-9]{64}$/);
    const tokenFile = join(base, 'token');
    const created = resolveToken({ sandboxRoot: base, tokenFile });
    assert.equal(created.tokenFile, tokenFile);
    assert.equal(statSync(tokenFile).mode & 0o077, 0);
    assert.equal(resolveToken({ sandboxRoot: base, tokenFile }).token, created.token);
    assert.throws(
      () => resolveToken({ sandboxRoot: base, tokenFile: join(root, 'outside-token') }),
      /outside managed root/,
    );
    assert.throws(
      () => resolveToken({ sandboxRoot: base, token: 'short' }),
      (error: unknown) => error instanceof SandboxError && error.code === 'invalid_options',
    );
    symlinkSync(tokenFile, join(base, 'token-link'));
    assert.throws(
      () => resolveToken({ sandboxRoot: base, tokenFile: join(base, 'token-link') }),
      (error: unknown) => error instanceof SandboxError && error.code === 'conflict',
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('deleteSandboxData removes only the managed sandbox directory', async () => {
  const root = tempRoot();
  try {
    makeDirectories(root, id);
    writeFileSync(join(root, id, 'workspace', 'kept.txt'), 'data');
    await deleteSandboxData({ sandboxId: id, sandboxesRoot: root });
    assert.equal(existsSync(join(root, id)), false);
    await deleteSandboxData({ sandboxId: id, sandboxesRoot: root });

    const outside = join(root, 'outside');
    mkdirSync(outside);
    writeFileSync(join(outside, 'keep.txt'), 'keep');
    symlinkSync(outside, join(root, id));
    await assert.rejects(
      () => deleteSandboxData({ sandboxId: id, sandboxesRoot: root }),
      (error: unknown) => error instanceof SandboxError && error.code === 'conflict',
    );
    assert.equal(readFileSync(join(outside, 'keep.txt'), 'utf8'), 'keep');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
