import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { settingsSchema, profileSchema } from '../src/instance/options.js';
import { policyFor } from '../src/runtime/policy.js';
import { SystemdProvider } from '../src/runtime/systemd-provider.js';
import { transientUnitProperties, verifyProbeCgroup } from '../src/runtime/doctor.js';
import { srtRuntime } from '../src/runtime/srt.js';
import type { SandboxProfile } from '../src/instance/types.js';

const sandboxId = `sbx_${'a'.repeat(32)}`;
const profile: SandboxProfile = {
  allowedDomains: [],
  runtimeReadPaths: ['/usr'],
  memoryMax: '512M',
  tasksMax: 24,
  cpuQuota: '50%',
};

test('SRT CLI resolves from the installed npm dependency', () => {
  const runtime = srtRuntime();
  assert(runtime.cli.startsWith(runtime.packageRoot + '/'));
  assert.equal(JSON.parse(readFileSync(join(runtime.packageRoot, 'package.json'), 'utf8')).version, '0.0.81');
  assert(existsSync(runtime.cli));
});

test('sandbox reads only the required SRT npm package, not its parent project', () => {
  const runtime = srtRuntime();
  const sandboxesRoot = '/var/lib/peri/sandboxes';
  const policy = JSON.parse(policyFor({ sandboxesRoot, sandboxId, profile, periPath: process.execPath }).json);
  assert(policy.filesystem.allowRead.includes(runtime.packageRoot));
  assert(!policy.filesystem.allowRead.includes(dirname(runtime.packageRoot)));
  assert(policy.filesystem.allowRead.includes(process.execPath));
  assert(!policy.filesystem.allowRead.includes(sandboxesRoot));
  assert.equal(policy.filesystem.allowGitConfig, true);
});

test('probe and production launch use the same isolation and resource limits', () => {
  assert.deepEqual(transientUnitProperties(profile), [
    '-p',
    'Type=exec',
    '-p',
    'Restart=no',
    '-p',
    'KillMode=control-group',
    '-p',
    'OOMPolicy=continue',
    '-p',
    'PrivateTmp=yes',
    '-p',
    'ExecStartPre=/usr/bin/mkdir -p /tmp/claude',
    '-p',
    'CollectMode=inactive-or-failed',
    '-p',
    'MemoryMax=512M',
    '-p',
    'TasksMax=24',
    '-p',
    'CPUQuota=50%',
  ]);
  assert(!transientUnitProperties(profile).some((value) => value.startsWith('BindsTo=') || value.startsWith('After=')));
});

test('probe rejects absent, unlimited and mismatched cgroup controllers', () => {
  const root = mkdtempSync(join(tmpdir(), 'peri-cgroup-'));
  const set = (file: string, content: string) => writeFileSync(join(root, file), content);
  try {
    set('cgroup.events', 'populated 1\n');
    set('cpu.max', '50000 100000\n');
    set('memory.max', '536870912\n');
    set('pids.max', '24\n');
    assert.doesNotThrow(() => verifyProbeCgroup(root, profile));
    for (const [file, value] of [
      ['cgroup.events', 'populated 0\n'],
      ['cpu.max', 'max 100000\n'],
      ['cpu.max', '100000 100000\n'],
      ['memory.max', 'max\n'],
      ['memory.max', '536870913\n'],
      ['pids.max', '25\n'],
    ]) {
      const original = {
        'cgroup.events': 'populated 1\n',
        'cpu.max': '50000 100000\n',
        'memory.max': '536870912\n',
        'pids.max': '24\n',
      }[file]!;
      set(file, value);
      assert.throws(() => verifyProbeCgroup(root, profile), /unsupported|ineffective|differ|controllers/);
      set(file, original);
    }
    rmSync(join(root, 'cpu.max'));
    assert.throws(() => verifyProbeCgroup(root, profile));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('settings reject unbounded network and resource policies', () => {
  const base = {
    sandboxesRoot: '/var/lib/peri/sandboxes',
    periPath: '/usr/bin/peri',
    periSha256: 'a'.repeat(64),
    profiles: {
      minimal: { allowedDomains: [], runtimeReadPaths: ['/usr'], memoryMax: '512M', tasksMax: 24, cpuQuota: '50%' },
    },
  };
  assert.doesNotThrow(() => settingsSchema.parse(base));
  assert.throws(() => settingsSchema.parse({ ...base, controlRoot: '/var/lib/peri/control' }));
  for (const allowedDomains of [['*'], ['https://example.com'], ['example.com:443'], ['a/../b']])
    assert.throws(() =>
      settingsSchema.parse({ ...base, profiles: { minimal: { ...base.profiles.minimal, allowedDomains } } }),
    );
  assert.throws(() => settingsSchema.parse({ ...base, profiles: {} }));
  assert.throws(() => settingsSchema.parse({ ...base, sandboxesRoot: 'relative/path' }));
  assert.throws(() => settingsSchema.parse({ ...base, periSha256: 'short' }));
  for (const memoryMax of ['0M', '1.5G', '1024']) assert.throws(() => profileSchema.parse({ ...profile, memoryMax }));
  for (const cpuQuota of ['0%', '100']) assert.throws(() => profileSchema.parse({ ...profile, cpuQuota }));
  assert.throws(() => profileSchema.parse({ ...profile, tasksMax: Number.MAX_SAFE_INTEGER + 1 }));
  assert.throws(() => profileSchema.parse({ ...profile, unknown: true }));
});

test('policy denies symlinked runtime reads into managed directories', () => {
  const root = mkdtempSync(join(tmpdir(), 'peri-policy-'));
  try {
    const sandboxesRoot = join(root, 'sandboxes');
    mkdirSync(sandboxesRoot);
    const alias = join(root, 'sandbox-alias');
    symlinkSync(sandboxesRoot, alias);
    assert.throws(
      () =>
        policyFor({
          sandboxesRoot,
          sandboxId,
          profile: { ...profile, runtimeReadPaths: [alias] },
          periPath: '/usr/bin/peri',
        }),
      /overlaps managed state/,
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('provider refuses to stop units outside a complete owned generation', async () => {
  const provider = new SystemdProvider({ unitPrefix: 'peri-' });
  await assert.rejects(provider.stop('peri-other.service'), /outside owned namespace/);
  await assert.rejects(provider.stop(`peri-${sandboxId}-run_short.service`), /outside owned namespace/);
});
