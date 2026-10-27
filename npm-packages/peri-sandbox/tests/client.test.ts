import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:net';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { connectSandboxControl } from '../src/index.js';

test('control client rejects a closed or malformed management response', async () => {
  const root = mkdtempSync(join(tmpdir(), 'peri-client-'));
  const socketPath = join(root, 'control.sock');
  let malformed = false;
  const server = createServer((connection) => {
    connection.once('data', () => {
      if (malformed) connection.end('{invalid-json}\n');
      else connection.end();
    });
  });
  try {
    await new Promise<void>((resolve) => server.listen(socketPath, resolve));
    const client = connectSandboxControl({ socketPath });
    await assert.rejects(client.list(), /closed before response/);
    malformed = true;
    await assert.rejects(client.list(), /JSON|Unexpected token|property name/i);
  } finally {
    await new Promise<void>((resolve) => server.close(() => resolve()));
    rmSync(root, { recursive: true, force: true });
  }
});
