import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { loadConfig } from '../src/config.js';
import { initMacConfig } from '../src/runtime/mac-config.js';
import { policyFor } from '../src/runtime/policy.js';

test('macOS initializes a private localhost service using the existing Peri executable', {
  skip: process.platform !== 'darwin',
}, async () => {
  const root = mkdtempSync(join(tmpdir(), 'peri-mac-config-'));
  const file = join(root, 'control', 'config.json');
  const oldPath = process.env.PERI_PATH;
  try {
    process.env.PERI_PATH = process.execPath;
    await initMacConfig(file);
    const config = loadConfig(file);
    assert.equal(config.runtime.periPath, process.execPath);
    assert.equal(config.listen.host, '127.0.0.1');
    assert(config.listen.port > 0);
    assert(config.profiles['workspace-default'].runtimeReadPaths.includes('/bin'));
    assert(config.profiles['workspace-default'].runtimeReadPaths.includes('/sbin'));
    assert(config.profiles['workspace-default'].runtimeReadPaths.includes('/usr/local'));
    assert(config.profiles['workspace-default'].runtimeReadPaths.includes('/opt/homebrew'));
    const policy = JSON.parse(policyFor(config, `sbx_${'a'.repeat(32)}`, 'workspace-default').json);
    assert(policy.filesystem.allowRead.includes('/opt/homebrew'));
    assert(policy.network.allowedDomains.includes('registry.npmjs.org'));
    assert.equal(policy.network.strictAllowlist, true);
    assert.deepEqual(policy.filesystem.denyRead, ['/']);
    assert.equal(statSync(config.auth.tokenFile).mode & 0o077, 0);
    await assert.rejects(() => initMacConfig(file), /already exists/);
  } finally {
    if (oldPath === undefined) delete process.env.PERI_PATH;
    else process.env.PERI_PATH = oldPath;
    rmSync(root, { recursive: true, force: true });
  }
});
