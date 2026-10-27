import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { configSchema, type Config } from '../src/config.js';
import { policyFor } from '../src/runtime/policy.js';
import { SystemdProvider } from '../src/runtime/provider.js';
import { transientUnitProperties, verifyProbeCgroup } from '../src/runtime/doctor.js';
import { srtRuntime } from '../src/runtime/srt.js';

test('SRT CLI resolves from the installed npm dependency', () => {
  const runtime = srtRuntime();
  assert(runtime.cli.startsWith(runtime.packageRoot + '/'));
  assert.equal(JSON.parse(readFileSync(join(runtime.packageRoot, 'package.json'), 'utf8')).version, '0.0.81');
  assert(existsSync(runtime.cli));
});

test('sandbox reads only the required SRT npm package, not its parent project', () => {
  const runtime = srtRuntime();
  const config = {
    controlRoot: '/var/lib/peri/control',
    sandboxesRoot: '/var/lib/peri/sandboxes',
    profiles: { minimal: { allowedDomains: [], runtimeReadPaths: ['/usr'] } },
  } as unknown as Config;
  const policy = JSON.parse(policyFor(config, `sbx_${'a'.repeat(32)}`, 'minimal').json);
  assert(policy.filesystem.allowRead.includes(runtime.packageRoot));
  assert(!policy.filesystem.allowRead.includes(dirname(runtime.packageRoot)));
});

test('probe and production launch use the same isolation and resource limits', () => {
  const config = { provider: { apiUnit: 'peri-api.service' } } as Config;
  const profile = { memoryMax: '512M', tasksMax: 24, cpuQuota: '50%' } as Config['profiles'][string];
  const bound = transientUnitProperties(config, profile, true);
  assert.deepEqual(bound, [
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
    'BindsTo=peri-api.service',
    '-p',
    'After=peri-api.service',
    '-p',
    'MemoryMax=512M',
    '-p',
    'TasksMax=24',
    '-p',
    'CPUQuota=50%',
  ]);
  assert.deepEqual(transientUnitProperties(config, profile, false), [
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
    'MemoryMax=512M',
    '-p',
    'TasksMax=24',
    '-p',
    'CPUQuota=50%',
  ]);
});

test('probe rejects absent, unlimited and mismatched cgroup controllers', () => {
  const root = mkdtempSync(join(tmpdir(), 'peri-cgroup-'));
  const profile = { memoryMax: '512M', tasksMax: 24, cpuQuota: '50%' } as Config['profiles'][string];
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

test('configuration rejects unbounded network and resource policies', () => {
  const config = {
    controlRoot: '/var/lib/peri/control',
    sandboxesRoot: '/var/lib/peri/sandboxes',
    listen: { host: '127.0.0.1', port: 9999 },
    publicBaseUrl: 'http://127.0.0.1:9999',
    auth: { tokenFile: '/var/lib/peri/token' },
    http: { allowedHosts: ['localhost'], allowedOrigins: [] },
    runtime: { periPath: '/usr/bin/peri', periSha256: 'a'.repeat(64) },
    provider: { apiUnit: 'peri-api.service', unitPrefix: 'peri-' },
    limits: { maxSandboxes: 1 },
    profiles: {
      minimal: { allowedDomains: [], runtimeReadPaths: ['/usr'], memoryMax: '512M', tasksMax: 24, cpuQuota: '50%' },
    },
  };
  assert.doesNotThrow(() => configSchema.parse(config));
  assert.throws(() => configSchema.parse({ ...config, runtime: { ...config.runtime, srtPath: '/usr/bin/srt' } }));
  for (const allowedDomains of [['*'], ['https://example.com'], ['example.com:443'], ['a/../b']]) {
    assert.throws(() =>
      configSchema.parse({ ...config, profiles: { minimal: { ...config.profiles.minimal, allowedDomains } } }),
    );
  }
  assert.throws(() => configSchema.parse({ ...config, profiles: {} }));
  assert.throws(() =>
    configSchema.parse({
      ...config,
      profiles: { minimal: { ...config.profiles.minimal, runtimeReadPaths: ['/var/lib/peri'] } },
    }),
  );
  assert.throws(() =>
    configSchema.parse({ ...config, profiles: { minimal: { ...config.profiles.minimal, memoryMax: '0M' } } }),
  );
  assert.throws(() =>
    configSchema.parse({ ...config, profiles: { minimal: { ...config.profiles.minimal, cpuQuota: '0%' } } }),
  );
  assert.throws(() =>
    configSchema.parse({
      ...config,
      profiles: { minimal: { ...config.profiles.minimal, tasksMax: Number.MAX_SAFE_INTEGER + 1 } },
    }),
  );
});

test('policy denies symlinked runtime reads into managed directories', () => {
  const root = mkdtempSync(join(tmpdir(), 'peri-policy-'));
  try {
    const controlRoot = join(root, 'control'),
      sandboxesRoot = join(root, 'sandboxes');
    mkdirSync(controlRoot);
    mkdirSync(sandboxesRoot);
    const alias = join(root, 'control-alias');
    symlinkSync(controlRoot, alias);
    const config = {
      controlRoot,
      sandboxesRoot,
      profiles: {
        minimal: {
          runtimeReadPaths: [alias],
          allowedDomains: [],
          memoryMax: '512M',
          tasksMax: 24,
          cpuQuota: '50%',
        },
      },
    } as unknown as Config;
    assert.throws(() => policyFor(config, `sbx_${'a'.repeat(32)}`, 'minimal'), /overlaps managed state/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('provider refuses to stop units outside a complete owned generation', async () => {
  const provider = new SystemdProvider({ provider: { unitPrefix: 'peri-' } } as Config);
  await assert.rejects(provider.stop('peri-other.service'), /outside owned namespace/);
  await assert.rejects(provider.stop(`peri-sbx_${'a'.repeat(32)}-run_short.service`), /outside owned namespace/);
});
