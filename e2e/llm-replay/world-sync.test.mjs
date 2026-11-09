import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, readdir, rename, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { replaceWorldDirectory } from '../scripts/world-sync-swap.mjs';

async function temporary(t) {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'peri-world-swap-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  return directory;
}

async function fixture(t) {
  const directory = await temporary(t);
  const world = path.join(directory, 'world');
  const staged = path.join(directory, 'staged');
  await mkdir(world);
  await mkdir(staged);
  await writeFile(path.join(world, 'marker'), 'old');
  await writeFile(path.join(staged, 'marker'), 'new');
  return { directory, world, staged };
}

const marker = (directory) => readFile(path.join(directory, 'marker'), 'utf8');

test('world:sync 替换在校验通过后才删除旧目录', async (t) => {
  const { directory, world, staged } = await fixture(t);
  const result = await replaceWorldDirectory(world, staged, {
    validate: async (target) => assert.equal(await marker(target), 'new'),
  });
  assert.equal(await marker(world), 'new');
  assert.equal(result.removed, true);
  assert.deepEqual((await readdir(directory)).filter((name) => name.includes('previous')), []);
});

test('第二次 rename 失败时回滚旧 world，staging 保留', async (t) => {
  const { directory, world, staged } = await fixture(t);
  let calls = 0;
  const renameImpl = async (from, to) => {
    calls += 1;
    if (calls === 2) throw Object.assign(new Error('injected rename failure'), { code: 'EIO' });
    await rename(from, to);
  };
  await assert.rejects(replaceWorldDirectory(world, staged, { renameImpl }), /injected rename failure/u);
  assert.equal(await marker(world), 'old');
  assert.equal(await marker(staged), 'new');
  assert.deepEqual((await readdir(directory)).filter((name) => name.includes('previous')), []);
});

test('替换后校验失败时回滚旧 world', async (t) => {
  const { directory, world, staged } = await fixture(t);
  await assert.rejects(
    replaceWorldDirectory(world, staged, { validate: async () => { throw new Error('invalid staged world'); } }),
    /invalid staged world/u,
  );
  assert.equal(await marker(world), 'old');
  assert.deepEqual((await readdir(directory)).filter((name) => name.includes('previous') || name.includes('failed')), []);
});

test('回滚失败时保留旧 world 并在错误中给出位置', async (t) => {
  const { directory, world, staged } = await fixture(t);
  let calls = 0;
  const renameImpl = async (from, to) => {
    calls += 1;
    if (calls === 1) { await rename(from, to); return; }
    throw Object.assign(new Error('boom'), { code: 'EIO' });
  };
  await assert.rejects(
    replaceWorldDirectory(world, staged, { renameImpl }),
    (error) => error instanceof AggregateError && /旧 world 保留在/u.test(error.message),
  );
  const previous = (await readdir(directory)).find((name) => name.startsWith('world.previous-'));
  assert.ok(previous, '旧 world 必须保留在 previous 目录');
  assert.equal(await marker(path.join(directory, previous)), 'old');
});
