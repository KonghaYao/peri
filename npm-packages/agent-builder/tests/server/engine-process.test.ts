import { afterEach, describe, expect, test } from 'bun:test';
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { enginePidFile, terminateEngineProcess } from '../../server/rivet/engine-process';

const originalStorage = process.env.RIVETKIT_STORAGE_PATH;
let root: string | null = null;

afterEach(() => {
  if (root !== null) {
    rmSync(root, { recursive: true, force: true });
    root = null;
  }
  if (originalStorage === undefined) delete process.env.RIVETKIT_STORAGE_PATH;
  else process.env.RIVETKIT_STORAGE_PATH = originalStorage;
});

function setup(): string {
  root = mkdtempSync(path.join(tmpdir(), 'ab-engine-pid-'));
  process.env.RIVETKIT_STORAGE_PATH = root;
  const file = enginePidFile();
  expect(file).not.toBeNull();
  if (file === null) throw new Error('unreachable');
  mkdirSync(path.dirname(file), { recursive: true });
  return file;
}

describe('engine child cleanup', () => {
  test('terminates a live child recorded by the wrapper and clears the pid file', async () => {
    const file = setup();
    const child = Bun.spawn(['sleep', '30']);
    writeFileSync(file, `${child.pid}\n`);

    const result = await terminateEngineProcess({ gracefulMs: 1_000, killMs: 500 });

    expect(['terminated', 'forced']).toContain(result.status);
    expect(result.pids).toContain(child.pid);
    await child.exited;
    expect(existsSync(file)).toBe(false);
  });

  test('never signals a PID that is not a direct child', async () => {
    const file = setup();
    // The test process is alive but is not its own child; a name-blind kill would end the run.
    writeFileSync(file, `${process.pid}\n`);

    const result = await terminateEngineProcess({ gracefulMs: 50, killMs: 50 });

    expect(result).toEqual({ status: 'none', pids: [] });
    expect(existsSync(file)).toBe(false);
  });

  test('treats a stale PID as already gone', async () => {
    const file = setup();
    writeFileSync(file, '999999\n');

    const result = await terminateEngineProcess({ gracefulMs: 50, killMs: 50 });

    expect(result).toEqual({ status: 'none', pids: [] });
    expect(existsSync(file)).toBe(false);
  });

  test('is a no-op without a pid file', async () => {
    setup();
    expect(await terminateEngineProcess()).toEqual({ status: 'none', pids: [] });
  });
});
