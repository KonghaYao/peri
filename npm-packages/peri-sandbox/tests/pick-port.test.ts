import assert from 'node:assert/strict';
import { createServer } from 'node:net';
import test from 'node:test';
import { pickPort } from '../src/server/pick-port.js';

test('picked port is already held by its server until close', async () => {
  const server = createServer();
  const port = await pickPort(server, '127.0.0.1', 0);
  const competitor = createServer();
  try {
    await assert.rejects(pickPort(competitor, '127.0.0.1', port), { code: 'EADDRINUSE' });
  } finally {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
  const replacement = createServer();
  try {
    assert.equal(await pickPort(replacement, '127.0.0.1', port), port);
  } finally {
    await new Promise<void>((resolve) => replacement.close(() => resolve()));
  }
});
