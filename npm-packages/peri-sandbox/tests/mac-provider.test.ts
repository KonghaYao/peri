import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, realpathSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import { MacProvider, macSpawnOptions } from '../src/runtime/mac-provider.js';
import type { ProviderStartOptions } from '../src/runtime/provider.js';

const options: ProviderStartOptions = {
  sandboxId: `sbx_${'a'.repeat(32)}`,
  runId: `run_${'b'.repeat(24)}`,
  sandboxesRoot: '/tmp/peri-sandbox-test',
  sandboxRoot: `/tmp/peri-sandbox-test/sbx_${'a'.repeat(32)}`,
  workspacePath: `/tmp/peri-sandbox-test/sbx_${'a'.repeat(32)}/workspace`,
  homePath: `/tmp/peri-sandbox-test/sbx_${'a'.repeat(32)}/home`,
  policyPath: `/tmp/peri-sandbox-test/sbx_${'a'.repeat(32)}/policy.json`,
  profile: { allowedDomains: [], runtimeReadPaths: [], memoryMax: '1G', tasksMax: 16, cpuQuota: '100%' },
  periPath: '/usr/bin/true',
};

test('macOS provider refuses to stop an unverified process group', {
  skip: process.platform !== 'darwin',
}, async () => {
  const provider = new MacProvider({ stopTimeoutMs: 100 });
  const stop = (
    provider as unknown as {
      stop(pid: number, started: string, options: ProviderStartOptions): Promise<void>;
    }
  ).stop.bind(provider);
  const group = Number(
    execFileSync('/bin/ps', ['-p', String(process.pid), '-o', 'pgid='], { encoding: 'utf8' }).trim(),
  );
  await assert.rejects(() => stop(group, 'wrong process', options), /cannot confirm owned macOS process group/);
  await assert.rejects(() => stop(1, 'wrong process', options), /invalid process group/);
  process.kill(process.pid, 0);
});

test('macOS provider pins SRT cwd and drops host secrets from the child env', () => {
  const spec = macSpawnOptions(options);
  const env = spec.env as Record<string, string | undefined>;
  assert.equal(spec.cwd, options.sandboxRoot);
  assert.equal(env.HOME, options.homePath);
  assert.equal(env.PERI_MODEL_API_KEY, undefined);
  assert.equal(env.PERI_STORAGE_TOKEN, undefined);
  assert.equal(env.HTTP_PROXY, undefined);
  assert.equal(env.HTTPS_PROXY, undefined);
  assert.equal(env.GIT_TEMPLATE_DIR, join(options.homePath, '.git-template'));
  if (process.platform === 'darwin' && existsSync('/private/var/select/developer_dir'))
    assert.equal(env.DEVELOPER_DIR, realpathSync('/private/var/select/developer_dir'));
});

test('macOS provider reports its runtime kind without touching the host', () => {
  const provider = new MacProvider();
  assert.equal(provider.kind, 'macos-process-group');
});
