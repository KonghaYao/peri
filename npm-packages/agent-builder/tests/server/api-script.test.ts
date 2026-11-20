import { afterEach, describe, expect, test } from 'bun:test';
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  realpathSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const SCRIPT = fileURLToPath(new URL('../../scripts/api.sh', import.meta.url));
const FAKE_BUN = '#!/usr/bin/env bash\nprintf \'%s\\n\' "$RIVETKIT_STORAGE_PATH"\n';

let root: string | null = null;

afterEach(() => {
  if (root !== null) {
    rmSync(root, { recursive: true, force: true });
    root = null;
  }
});

function setup(envFile?: string): string {
  const dir = mkdtempSync(path.join(tmpdir(), 'ab-api-sh-'));
  root = dir;
  mkdirSync(path.join(dir, 'scripts'));
  mkdirSync(path.join(dir, 'bin'));
  copyFileSync(SCRIPT, path.join(dir, 'scripts', 'api.sh'));
  chmodSync(path.join(dir, 'scripts', 'api.sh'), 0o755);
  writeFileSync(path.join(dir, 'bin', 'bun'), FAKE_BUN);
  chmodSync(path.join(dir, 'bin', 'bun'), 0o755);
  if (envFile !== undefined) writeFileSync(path.join(dir, '.env'), envFile);
  return dir;
}

function run(shellValue?: string): string {
  if (root === null) throw new Error('setup first');
  const env: Record<string, string | undefined> = {
    ...process.env,
    PATH: `${path.join(root, 'bin')}:${process.env.PATH ?? ''}`,
    RIVETKIT_STORAGE_PATH: shellValue,
  };
  const result = Bun.spawnSync(['bash', 'scripts/api.sh'], { cwd: root, env });
  expect(result.exitCode).toBe(0);
  return result.stdout.toString().trim();
}

describe('scripts/api.sh storage path precedence', () => {
  test('defaults to .data/rivet without a shell value or .env key', () => {
    const dir = setup('OTHER=value\n');
    expect(run()).toBe(path.join(realpathSync(dir), '.data', 'rivet'));
  });

  test('reads a single-quoted export with a trailing comment from .env', () => {
    const dir = setup("export RIVETKIT_STORAGE_PATH='from-env-single' # comment\n");
    expect(run()).toBe(path.join(realpathSync(dir), 'from-env-single'));
  });

  test('reads a double-quoted value with a trailing comment', () => {
    const dir = setup('RIVETKIT_STORAGE_PATH="from-env-double" # comment\n');
    expect(run()).toBe(path.join(realpathSync(dir), 'from-env-double'));
  });

  test('reads an unquoted value with a trailing comment', () => {
    const dir = setup('RIVETKIT_STORAGE_PATH=from-unquoted # comment\n');
    expect(run()).toBe(path.join(realpathSync(dir), 'from-unquoted'));
  });

  test('keeps a shell value ahead of .env', () => {
    const dir = setup('RIVETKIT_STORAGE_PATH=from-env\n');
    // A shell value is used verbatim; only .env defaults are resolved against the repository root.
    expect(run(path.join(dir, 'from-shell'))).toBe(path.join(dir, 'from-shell'));
  });

  test('ignores unsupported lines instead of guessing', () => {
    const dir = setup('RIVETKIT_STORAGE_PATH_EXTRA=x\nRIVETKIT_STORAGE_PATH="unterminated\n');
    expect(run()).toBe(path.join(realpathSync(dir), '.data', 'rivet'));
  });

  test('keeps an absolute path and creates it privately', () => {
    const target = path.join(tmpdir(), `ab-api-abs-${process.pid}-${Date.now()}`);
    setup(`RIVETKIT_STORAGE_PATH=${target}\n`);
    expect(run()).toBe(target);
    expect(statSync(target).mode & 0o777).toBe(0o700);
    rmSync(target, { recursive: true, force: true });
  });

  test('lets the last assignment win, like Bun .env loading', () => {
    const dir = setup('RIVETKIT_STORAGE_PATH=first\nRIVETKIT_STORAGE_PATH=second\n');
    expect(run()).toBe(path.join(realpathSync(dir), 'second'));
    expect(existsSync(path.join(dir, 'first'))).toBe(false);
  });
});
