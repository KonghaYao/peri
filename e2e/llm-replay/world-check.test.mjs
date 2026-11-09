import test from 'node:test';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { cp, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { promisify } from 'node:util';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { sha256 } from './matcher.mjs';

const execute = promisify(execFile);
const e2eRoot = fileURLToPath(new URL('..', import.meta.url));
const script = path.join(e2eRoot, 'scripts/refresh-cassettes.mjs');
const fixtures = path.join(e2eRoot, 'fixtures/replay-v2');
const recordedSeal = path.join(fixtures, 'S01/cassette/seal.json');

/** 复制真实 world 与一条 cassette seal，得到可自由重封的隔离 fixture。 */
async function fixture(t) {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'peri-world-check-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const world = path.join(directory, 'world');
  await cp(path.join(fixtures, 'world'), world, { recursive: true });
  const cassette = path.join(directory, 'S01/cassette');
  await mkdir(cassette, { recursive: true });
  await cp(recordedSeal, path.join(cassette, 'seal.json'));
  return { directory, world, sealFile: path.join(cassette, 'seal.json') };
}

/** 模拟“改 leaf 后手工重封 world seal”：loadWorld 仍通过，录制引用未变。 */
async function rewriteLeaf(world, id, content) {
  const manifest = JSON.parse(await readFile(path.join(world, 'manifest.json'), 'utf8'));
  await writeFile(path.join(world, manifest.leaves[id].file), content);
  const sealFile = path.join(world, 'seal.json');
  const seal = JSON.parse(await readFile(sealFile, 'utf8'));
  seal.leaves[id] = sha256(Buffer.from(content));
  await writeFile(sealFile, `${JSON.stringify(seal)}\n`);
}

async function run(args) {
  try {
    const { stdout } = await execute(process.execPath, [script, ...args], { cwd: e2eRoot });
    return { code: 0, report: JSON.parse(stdout) };
  } catch (error) {
    const stdout = error.stdout ?? '';
    return { code: typeof error.code === 'number' ? error.code : 1, report: stdout ? JSON.parse(stdout) : null };
  }
}

test('手工改写未映射 block0 并重封 world seal 时 world:check 非零且 stale', async (t) => {
  const { world } = await fixture(t);
  const original = await readFile(path.join(world, 'system/block0.main.txt'));
  await rewriteLeaf(world, 'sys.block0.main', Buffer.concat([original, Buffer.from('\nTAMPERED\n')]));

  const { code, report } = await run(['--check', '--world', world]);
  assert.equal(code, 1);
  assert.equal(report.stale, true);
  assert.deepEqual(report.staleReasons, ['cassette_unmapped_drift']);
  assert.deepEqual(report.recorded.unmappedDrift, ['sys.block0.main']);
  assert.deepEqual(report.recorded.mappedDrift, []);
  assert.deepEqual(report.updated, []);
});

test('干净基线 world:check 为 0 且录制引用全部一致', async (t) => {
  const { world } = await fixture(t);
  const { code, report } = await run(['--check', '--world', world]);
  assert.equal(code, 0);
  assert.equal(report.stale, false);
  assert.deepEqual(report.staleReasons, []);
  assert.equal(report.recorded.cassettes, 1);
  assert.ok(report.recorded.references > 0);
  assert.deepEqual(report.recorded.unmappedDrift, []);
  assert.deepEqual(report.recorded.missingLeaves, []);
});

test('mapped sys.skills 经 world:sync 后只报告待重录，不判为未映射损坏', async (t) => {
  const { world, sealFile } = await fixture(t);
  // 模拟 cassette 录制的是旧 skills：只有映射 leaf 的录制哈希落后于当前 world。
  const seal = JSON.parse(await readFile(sealFile, 'utf8'));
  const staleSkills = sha256(Buffer.from('recorded-old-skills'));
  let replaced = 0;
  for (const meta of Object.values(seal.requestMeta)) {
    for (const ref of meta.worldRefs) {
      if (ref.id !== 'sys.skills') continue;
      ref.sha256 = staleSkills;
      replaced += 1;
    }
  }
  assert.ok(replaced > 0, 'S01 seal 必须引用 sys.skills');
  await writeFile(sealFile, `${JSON.stringify(seal)}\n`);

  // 源已变化但 world 尚未同步：由源映射报 stale。
  await rewriteLeaf(world, 'sys.skills', 'OLD WORLD SKILLS');
  const before = await run(['--check', '--world', world]);
  assert.equal(before.code, 1);
  assert.ok(before.report.updated.includes('sys.skills'));

  const synced = await run(['--world', world]);
  assert.equal(synced.code, 0);
  assert.deepEqual(synced.report.updated, ['sys.skills']);
  assert.equal(synced.report.written, 1);

  const after = await run(['--check', '--world', world]);
  assert.equal(after.code, 0);
  assert.equal(after.report.stale, false);
  assert.deepEqual(after.report.staleReasons, []);
  assert.deepEqual(after.report.recorded.mappedDrift, ['sys.skills']);
  assert.deepEqual(after.report.recorded.unmappedDrift, []);
  assert.equal(after.report.recorded.requiresRerecord, true);
});
