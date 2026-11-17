import { afterEach, describe, expect, test } from 'bun:test';
import {
  chmodSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { ConfigError, type ConfigErrorCode, configFailure } from '../../server/config-error';
import { loadEnv } from '../../server/env';
import { loadOrCreateMachineId } from '../../server/identity';

const roots: string[] = [];

function tempDir(mode = 0o700): string {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), 'agent-builder-env-')));
  chmodSync(root, mode);
  roots.push(root);
  return root;
}

afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

function baseSource(dir: string): Record<string, string | undefined> {
  return {
    DATABASE_PATH: path.join(dir, 'agent-builder.db'),
    PERI_SANDBOX_PERI_PATH: path.join(dir, 'no-peri'),
    PERI_WORKSPACE_PATH: process.cwd(),
    PERI_STORE_TOKEN: 'store-token',
    ANTHROPIC_API_KEY: 'sk-ant-test',
  };
}

function expectConfigCode(run: () => unknown, code: ConfigErrorCode): void {
  try {
    run();
  } catch (error) {
    expect(error).toBeInstanceOf(ConfigError);
    expect((error as ConfigError).code).toBe(code);
    return;
  }
  throw new Error(`expected loadEnv to reject with ${code}`);
}

describe('machine identity', () => {
  test('creates a 0600 identity file and reuses it across loads', () => {
    const dir = tempDir();
    const file = path.join(dir, 'machine-id');
    const first = loadOrCreateMachineId(dir);
    const second = loadOrCreateMachineId(dir);

    expect(first).toBe(second);
    expect(first).toMatch(/^[0-9a-f-]{36}$/);
    expect(statSync(file).mode & 0o777).toBe(0o600);
    expect(readFileSync(file, 'utf8').trim()).toBe(first);
  });

  test('keeps the env machine id stable across two loads instead of generating a new one', () => {
    const dir = tempDir();
    const first = loadEnv(baseSource(dir), { identityDir: dir });
    const second = loadEnv(baseSource(dir), { identityDir: dir });
    expect(first.machineId).toBe(second.machineId);
    expect(statSync(path.join(dir, 'machine-id')).mode & 0o777).toBe(0o600);
  });

  test('rejects an invalid explicit machine id without rewriting the identity file', () => {
    const dir = tempDir();
    expectConfigCode(
      () => loadEnv({ ...baseSource(dir), PERI_MACHINE_ID: 'not-a-uuid' }),
      'config_machine_id_invalid',
    );
  });

  test('rejects a group/world-readable identity file', () => {
    const dir = tempDir();
    const file = path.join(dir, 'machine-id');
    writeFileSync(file, '00000000-0000-4000-8000-000000000001\n', { mode: 0o644 });
    chmodSync(file, 0o644);
    expect(() => loadOrCreateMachineId(dir)).toThrow(/group\/world/);
  });
});

describe('workspace and sandbox configuration', () => {
  test('derives one canonical workspace URI from the explicit directory', () => {
    const dir = tempDir();
    const env = loadEnv(baseSource(dir), { identityDir: dir });
    expect(env.workspaceUri).toBe(`peri+sandbox://${env.machineId}${process.cwd()}`);
    expect(env.sandbox.mode).toBe('disabled');
    expect(env.sandbox.enabled).toBe(false);
  });

  test('rejects a symlink workspace instead of authorizing a second spelling', () => {
    const dir = tempDir();
    const real = tempDir();
    const link = path.join(dir, 'workspace-link');
    symlinkSync(real, link);
    expectConfigCode(
      () => loadEnv({ ...baseSource(dir), PERI_WORKSPACE_PATH: link }, { identityDir: dir }),
      'workspace_path_symlink',
    );
  });

  test('classifies every rejected workspace path with a stable config code', () => {
    const dir = tempDir();
    expectConfigCode(
      () =>
        loadEnv({ ...baseSource(dir), PERI_WORKSPACE_PATH: path.join(dir, 'missing') }, { identityDir: dir }),
      'workspace_path_missing',
    );

    const realParent = tempDir();
    const nested = path.join(realParent, 'workspace');
    mkdirSync(nested, { mode: 0o700 });
    const aliasParent = path.join(dir, 'alias-parent');
    symlinkSync(realParent, aliasParent);
    // 父目录是 symlink、最终目录不是：与 macOS /tmp 别名同类，必须拒绝而不是 realpath 归一化。
    expect(realpathSync(path.join(aliasParent, 'workspace'))).toBe(nested);
    expectConfigCode(
      () =>
        loadEnv(
          { ...baseSource(dir), PERI_WORKSPACE_PATH: path.join(aliasParent, 'workspace') },
          { identityDir: dir },
        ),
      'workspace_path_not_canonical',
    );

    const file = path.join(dir, 'workspace-file');
    writeFileSync(file, '');
    expectConfigCode(
      () => loadEnv({ ...baseSource(dir), PERI_WORKSPACE_PATH: file }, { identityDir: dir }),
      'workspace_path_not_directory',
    );

    expectConfigCode(
      () => loadEnv({ ...baseSource(dir), PERI_WORKSPACE_PATH: '/' }, { identityDir: dir }),
      'workspace_path_filesystem_root',
    );
  });

  test('names only schema fields when an environment value is invalid', () => {
    const dir = tempDir();
    try {
      loadEnv({ ...baseSource(dir), PORT: 'not-a-port' }, { identityDir: dir });
    } catch (error) {
      const failure = configFailure(error);
      expect(failure?.code).toBe('config_invalid');
      expect(failure?.hint).toContain('PORT');
      return;
    }
    throw new Error('expected loadEnv to reject an invalid PORT');
  });

  test('enables sandbox only when the workspace is 0700 and the peri binary exists', () => {
    const dir = tempDir();
    const workspace = tempDir(0o700);
    const peri = path.join(dir, 'peri');
    writeFileSync(peri, '#!/bin/sh\nexit 0\n', { mode: 0o755 });
    chmodSync(peri, 0o755);

    const env = loadEnv(
      {
        ...baseSource(dir),
        PERI_WORKSPACE_PATH: workspace,
        PERI_SANDBOX_ENABLED: 'true',
        PERI_SANDBOX_PERI_PATH: peri,
        PERI_SANDBOX_ROOT: path.join(dir, 'sandboxes'),
      },
      { identityDir: dir },
    );

    expect(env.sandbox.mode).toBe('ready');
    expect(env.sandbox.reasons).toEqual([]);
    expect(env.sandbox.token).toMatch(/^[0-9a-f]{64}$/);
    expect(statSync(path.join(dir, 'sandbox-token')).mode & 0o777).toBe(0o600);
    expect(env.sandbox.workspacePathPrivate).toBe(true);
  });

  test('reports a public workspace as a blocking sandbox reason', () => {
    const dir = tempDir();
    const workspace = tempDir(0o755);
    const peri = path.join(dir, 'peri');
    writeFileSync(peri, '#!/bin/sh\n', { mode: 0o755 });

    const env = loadEnv(
      {
        ...baseSource(dir),
        PERI_WORKSPACE_PATH: workspace,
        PERI_SANDBOX_ENABLED: 'true',
        PERI_SANDBOX_PERI_PATH: peri,
      },
      { identityDir: dir },
    );

    expect(env.sandbox.mode).toBe('misconfigured');
    expect(env.sandbox.reasons).toContain('sandbox_workspace_not_private');
  });

  test('reports a missing peri binary without starting a placeholder sandbox', () => {
    const dir = tempDir();
    const workspace = tempDir();
    const env = loadEnv(
      {
        ...baseSource(dir),
        PERI_WORKSPACE_PATH: workspace,
        PERI_SANDBOX_ENABLED: 'true',
        PERI_SANDBOX_PERI_PATH: path.join(dir, 'missing-peri'),
      },
      { identityDir: dir },
    );
    expect(env.sandbox.mode).toBe('misconfigured');
    expect(env.sandbox.reasons).toContain('sandbox_peri_missing');
  });

  test('parses extra allowed origins without accepting arbitrary broad values', () => {
    const dir = tempDir();
    const env = loadEnv(
      { ...baseSource(dir), PERI_ALLOWED_ORIGINS: 'http://127.0.0.1:5173/, http://localhost:9999' },
      { identityDir: dir },
    );
    expect(env.allowedOrigins).toContain('http://127.0.0.1:5173');
    expect(env.allowedOrigins).toContain('http://localhost:9999');
    expect(env.allowedOrigins).not.toContain('http://evil.example');
  });
});
