import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { resolveOptions } from '../src/instance/options.js';
import type { SandboxProfile, SandboxSettings } from '../src/instance/types.js';

const id = `sbx_${'d'.repeat(32)}`;
const profile: SandboxProfile = {
  allowedDomains: [],
  runtimeReadPaths: ['/usr'],
  memoryMax: '1G',
  tasksMax: 32,
  cpuQuota: '100%',
};

function fixture(periPath: string): { root: string; settings: SandboxSettings } {
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'peri-options-')));
  const sandboxesRoot = join(root, 'sandboxes');
  mkdirSync(sandboxesRoot, { recursive: true, mode: 0o700 });
  return {
    root,
    settings: {
      sandboxesRoot,
      periPath,
      periSha256: 'a'.repeat(64),
      defaultProfile: 'default',
      profiles: { default: profile },
    },
  };
}

test('resolveOptions keeps the shared policy scope without an explicit opt-in', () => {
  const { root, settings } = fixture(process.execPath);
  try {
    const resolved = resolveOptions({ sandboxId: id }, settings);
    assert.equal(resolved.periPath, process.execPath);
    assert.deepEqual(resolved.profile.runtimeReadPaths, ['/usr']);
    assert(!resolved.profile.runtimeReadPaths.includes(dirname(process.execPath)));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('allowPeriBinaryDirectory merges the peri binary directory into the profile', () => {
  const { root, settings } = fixture(process.execPath);
  const bin = dirname(process.execPath);
  try {
    const resolved = resolveOptions({ sandboxId: id, allowPeriBinaryDirectory: true }, settings);
    assert.deepEqual(resolved.profile.runtimeReadPaths, ['/usr', bin]);
    assert.equal(resolved.profile.memoryMax, '1G');

    const alreadyListed = resolveOptions(
      {
        sandboxId: id,
        profile: { ...profile, runtimeReadPaths: ['/usr', bin] },
        allowPeriBinaryDirectory: true,
      },
      settings,
    );
    assert.deepEqual(alreadyListed.profile.runtimeReadPaths, ['/usr', bin]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
