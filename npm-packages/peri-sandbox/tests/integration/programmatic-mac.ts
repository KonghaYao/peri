import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { createServer } from 'node:net';
import { join } from 'node:path';
import { initSettings, SandboxError, startSandbox, type SandboxInstance } from '../../src/index.js';

if (process.platform !== 'darwin') throw new Error('macOS acceptance runs only on macOS');
const root = realpathSync(mkdtempSync(join(tmpdir(), 'peri-programmatic-')));
const settingsPath = join(root, 'settings.json');
const sandboxId = `sbx_${'cd'.repeat(16)}`;
let instance: SandboxInstance | undefined;

async function expectCode(promise: Promise<unknown>, code: string): Promise<void> {
  await assert.rejects(promise, (error: unknown) => {
    assert(error instanceof SandboxError, String(error));
    assert.equal(error.code, code);
    return true;
  });
}

try {
  await initSettings(settingsPath);
  instance = await startSandbox({ settingsPath, sandboxId, minFreeBytes: 0 });
  assert.equal(instance.id, sandboxId);
  assert.equal(instance.workspacePath, join(root, 'sandboxes', sandboxId, 'workspace'));
  assert.match(instance.token, /^[a-f0-9]{64}$/);

  await expectCode(startSandbox({ settingsPath, sandboxId, minFreeBytes: 0 }), 'conflict');

  const occupied = createServer();
  await new Promise<void>((resolve) => occupied.listen(0, '127.0.0.1', resolve));
  const address = occupied.address();
  assert(address && typeof address !== 'string');
  try {
    await expectCode(
      startSandbox({
        settingsPath,
        sandboxId: `sbx_${'ef'.repeat(16)}`,
        port: address.port,
        minFreeBytes: 0,
      }),
      'conflict',
    );
  } finally {
    await new Promise<void>((resolve) => occupied.close(() => resolve()));
  }

  await instance.close();
  await instance.close();
  assert.equal(instance.state, 'closed');
  assert.equal(existsSync(instance.workspacePath), true);
  await instance.close({ deleteData: true });
  assert.equal(existsSync(join(root, 'sandboxes', sandboxId)), false);
  instance = undefined;
  instance = await startSandbox({ settingsPath, sandboxId, minFreeBytes: 0 });
  console.log('macOS: programmatic start, same-id conflict, occupied port, idempotent close and deleteData passed');
} finally {
  await instance?.close({ deleteData: true });
  rmSync(root, { recursive: true, force: true });
}
