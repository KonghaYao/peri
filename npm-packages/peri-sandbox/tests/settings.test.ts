import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import test from 'node:test';
import {
  defaultProfile,
  defaultSettingsPath,
  initSettings,
  loadSettings,
  resolveSandboxSettings,
  verifyPeriRuntime,
} from '../src/instance/settings.js';
import { SandboxError } from '../src/instance/types.js';

function withPeriPath<T>(path: string, operation: () => Promise<T>): Promise<T> {
  const previous = process.env.PERI_PATH;
  process.env.PERI_PATH = path;
  return operation().finally(() => {
    if (previous === undefined) delete process.env.PERI_PATH;
    else process.env.PERI_PATH = previous;
  });
}

test('init writes private settings and a private sandboxes root', async () => {
  const root = mkdtempSync(join(tmpdir(), 'peri-settings-'));
  try {
    const file = join(root, 'settings.json');
    const created = await withPeriPath(process.execPath, () => initSettings(file));
    assert.equal(created, file);
    assert.equal(statSync(file).mode & 0o077, 0);
    const settings = loadSettings(file);
    assert.equal(settings.periPath, process.execPath);
    assert.equal(settings.periSha256, createHash('sha256').update(readFileSync(process.execPath)).digest('hex'));
    assert.equal(settings.sandboxesRoot, join(root, 'sandboxes'));
    assert.equal(statSync(settings.sandboxesRoot).mode & 0o077, 0);
    assert.equal(settings.defaultProfile, 'workspace-default');
    assert(settings.profiles['workspace-default']);
    assert(!readFileSync(file, 'utf8').includes('token'));
    await withPeriPath(process.execPath, () =>
      assert.rejects(
        () => initSettings(file),
        (error: unknown) => {
          assert(error instanceof SandboxError);
          assert.equal(error.code, 'conflict');
          return true;
        },
      ),
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('an explicit missing settings file is rejected', () => {
  const root = mkdtempSync(join(tmpdir(), 'peri-settings-'));
  try {
    assert.throws(() => loadSettings(join(root, 'missing.json')), /settings not found/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('default settings stay in memory when the default file is absent', () => {
  const root = mkdtempSync(join(tmpdir(), 'peri-settings-'));
  const previous = process.env.PERI_SANDBOX_SETTINGS;
  try {
    const file = join(root, 'missing', 'settings.json');
    process.env.PERI_SANDBOX_SETTINGS = file;
    assert.equal(defaultSettingsPath(), file);
    const settings = loadSettings();
    assert.equal(settings.sandboxesRoot, join(process.env.HOME ?? '', '.peri-sandbox', 'sandboxes'));
    assert(!existsSync(file));
    const resolved = resolveSandboxSettings();
    assert.equal(resolved.path, file);
    assert.equal(resolved.source, 'defaults');
    assert.equal(resolved.profileName, 'workspace-default');
  } finally {
    if (previous === undefined) delete process.env.PERI_SANDBOX_SETTINGS;
    else process.env.PERI_SANDBOX_SETTINGS = previous;
    rmSync(root, { recursive: true, force: true });
  }
});

test('settings description mirrors runtime file and profile rules', () => {
  const root = mkdtempSync(join(tmpdir(), 'peri-settings-'));
  try {
    const file = join(root, 'settings.json');
    const beta = { allowedDomains: [], runtimeReadPaths: ['/usr'], memoryMax: '2G', tasksMax: 4, cpuQuota: '50%' };
    writeFileSync(
      file,
      JSON.stringify({
        sandboxesRoot: join(root, 'sandboxes'),
        periPath: process.execPath,
        periSha256: 'a'.repeat(64),
        profiles: { beta },
      }),
    );
    assert.deepEqual(resolveSandboxSettings({ settingsPath: file }), {
      path: file,
      source: 'file',
      profileName: 'beta',
      profile: beta,
    });
    assert.throws(
      () => resolveSandboxSettings({ settingsPath: join(root, 'missing.json') }),
      /Sandbox settings not found/,
    );
    assert.throws(
      () => resolveSandboxSettings({ settingsPath: file, profileName: 'missing' }),
      /Unknown sandbox profile/,
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('public or symlinked sandboxes roots are rejected', () => {
  const root = mkdtempSync(join(tmpdir(), 'peri-settings-'));
  try {
    const sandboxesRoot = join(root, 'sandboxes');
    mkdirSync(sandboxesRoot, { mode: 0o755 });
    chmodSync(sandboxesRoot, 0o755);
    const file = join(root, 'settings.json');
    writeFileSync(
      file,
      JSON.stringify({
        sandboxesRoot,
        periPath: process.execPath,
        periSha256: 'a'.repeat(64),
        profiles: {
          default: { allowedDomains: [], runtimeReadPaths: ['/usr'], memoryMax: '1G', tasksMax: 8, cpuQuota: '100%' },
        },
      }),
    );
    assert.throws(() => loadSettings(file), /must be private/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('default profile reads runtime paths and package registries', () => {
  const profile = defaultProfile(process.execPath);
  assert(profile.allowedDomains.includes('registry.npmjs.org'));
  assert(profile.allowedDomains.includes('pypi.org'));
  assert(profile.allowedDomains.includes('crates.io'));
  assert(profile.runtimeReadPaths.includes(dirname(process.execPath)));
  if (process.platform === 'darwin') assert(profile.runtimeReadPaths.includes('/private/var/select'));
});

test('peri runtime verification rejects missing binaries and hash mismatches', () => {
  const root = mkdtempSync(join(tmpdir(), 'peri-settings-'));
  try {
    assert.throws(() => verifyPeriRuntime(join(root, 'missing'), 'a'.repeat(64)), /unavailable/);
    assert.throws(() => verifyPeriRuntime(process.execPath, 'a'.repeat(64)), /hash mismatch/);
    assert.doesNotThrow(() =>
      verifyPeriRuntime(process.execPath, createHash('sha256').update(readFileSync(process.execPath)).digest('hex')),
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
