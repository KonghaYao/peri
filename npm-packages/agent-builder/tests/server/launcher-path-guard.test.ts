import { afterEach, describe, expect, test } from 'bun:test';
import { spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import {
  chmodSync,
  cpSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  realpathSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const LAUNCHER = fileURLToPath(new URL('../../bin/agent-builder.js', import.meta.url));
const GUARD = fileURLToPath(new URL('../../bin/path-guard.mjs', import.meta.url));

const roots: string[] = [];

afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

/** Canonical fixture root: realpath removes the host's /var alias without depending on it. */
function canonicalRoot(): string {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), 'ab-launcher-path-')));
  chmodSync(root, 0o700);
  roots.push(root);
  return root;
}

function directory(parent: string, name: string, mode = 0o700): string {
  const target = path.join(parent, name);
  mkdirSync(target, { mode });
  chmodSync(target, mode);
  return target;
}

/** Stages the real launcher with a fake dist entry so no existing build is imported or changed. */
function stagedLauncher(root: string): string {
  const packageDir = directory(root, 'package');
  const binDir = directory(packageDir, 'bin');
  cpSync(LAUNCHER, path.join(binDir, 'agent-builder.js'));
  cpSync(GUARD, path.join(binDir, 'path-guard.mjs'));
  const distDir = directory(packageDir, 'dist');
  const serverDir = directory(distDir, 'server');
  writeFileSync(path.join(serverDir, 'main.js'), "console.log('fake-main-loaded');\n");
  return path.join(binDir, 'agent-builder.js');
}

function runLauncher(launcher: string, cwd: string, databasePath: string) {
  const result = spawnSync(process.execPath, [launcher, 'doctor'], {
    cwd,
    env: {
      HOME: path.join(cwd, 'home'),
      PATH: process.env.PATH ?? '/usr/bin:/bin',
      TMPDIR: cwd,
      DATABASE_PATH: databasePath,
    },
    encoding: 'utf8',
    timeout: 15_000,
  });
  return { status: result.status, output: `${result.stdout ?? ''}\n${result.stderr ?? ''}` };
}

function modeOf(target: string): string {
  const entry = lstatSync(target, { throwIfNoEntry: false });
  if (entry === undefined) return 'absent';
  return entry.isSymbolicLink() ? 'symlink' : (entry.mode & 0o777).toString(8);
}

describe('packaged launcher private paths', () => {
  test('rejects a symlink DATABASE_PATH ancestor before creating data or storage directories', () => {
    const root = canonicalRoot();
    const real = directory(root, 'real');
    const privateDir = directory(real, 'private');
    const secret = `sk-launcher-${randomBytes(16).toString('hex')}`;
    const alias = path.join(root, `alias-${secret}`);
    symlinkSync(real, alias);
    const launcher = stagedLauncher(root);
    const runtime = directory(root, 'runtime');

    const result = runLauncher(launcher, runtime, path.join(alias, 'private', 'config.db'));

    expect(result.status).toBe(1);
    expect(result.output).toContain('realpath');
    expect(result.output).toContain('符号链接');
    expect(result.output).not.toContain(secret);
    expect(result.output).not.toContain(alias);
    expect(result.output).not.toContain('fake-main-loaded');
    expect(readdirSync(privateDir)).toEqual([]);
    expect(modeOf(real)).toBe('700');
  });

  test('still creates canonical missing parents and imports the server entry', () => {
    const root = canonicalRoot();
    const launcher = stagedLauncher(root);
    const runtime = directory(root, 'runtime');
    const dataDir = path.join(runtime, 'data');

    const result = runLauncher(launcher, runtime, path.join(dataDir, 'config.db'));

    expect(result.status).toBe(0);
    expect(result.output).toContain('fake-main-loaded');
    expect(modeOf(dataDir)).toBe('700');
    expect(modeOf(path.join(dataDir, 'rivet'))).toBe('700');
    expect(existsSync(path.join(dataDir, 'config.db'))).toBe(false);
  });
});
