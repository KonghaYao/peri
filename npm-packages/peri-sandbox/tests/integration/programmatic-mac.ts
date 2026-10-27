import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { createServer } from 'node:net';
import { join } from 'node:path';
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';
import { z } from 'zod';
import { connectSandboxControl, startPeriSandbox } from '../../src/index.js';

if (process.platform !== 'darwin') throw new Error('macOS acceptance runs only on macOS');
const root = realpathSync(mkdtempSync(join(tmpdir(), 'peri-programmatic-')));
const configPath = join(root, 'control', 'config.json');
let session: Awaited<ReturnType<typeof startPeriSandbox>> | undefined;
try {
  session = await startPeriSandbox({ configPath });
  assert.equal(session.configPath, configPath);
  assert(existsSync(session.tokenFile));
  const control = connectSandboxControl({ socketPath: join(root, 'control', 'control.sock') });
  assert.equal((await control.inspect(session.id)).state, 'ready');
  await assert.rejects(startPeriSandbox({ configPath }), { code: 'conflict' });
  assert.equal((await control.inspect(session.id)).state, 'ready');
  const client = new Client(
    { name: 'programmatic-test', version: '1' },
    {
      versionNegotiation: { mode: { pin: '2026-07-28' } },
      capabilities: { extensions: { 'io.modelcontextprotocol/tasks': {} } },
    },
  );
  try {
    await client.connect(
      new StreamableHTTPClientTransport(new URL(session.mcpUrl), {
        requestInit: { headers: { Authorization: `Bearer ${readFileSync(session.tokenFile, 'utf8').trim()}` } },
      }),
    );
    assert.deepEqual((await client.listTools()).tools.map((tool) => tool.name).sort(), [
      'Bash',
      'Edit',
      'Glob',
      'Grep',
      'Read',
      'Write',
      'folder_operations',
    ]);
    const glob = await client.request(
      { method: 'tools/call', params: { name: 'Glob', arguments: { pattern: '*.does-not-exist' } } },
      z.record(z.string(), z.unknown()),
    );
    assert.equal(glob.isError, false, JSON.stringify(glob));
    const bash = await client.request(
      {
        method: 'tools/call',
        params: {
          name: 'Bash',
          arguments: {
            command:
              'stat /bin/ls && command -v ls && ls -d . && if test -x /opt/homebrew/bin/brew; then command -v brew; fi',
          },
        },
      },
      z.record(z.string(), z.unknown()),
    );
    assert.equal(bash.isError, false, JSON.stringify(bash));
    assert(JSON.stringify(bash.content).includes('/bin/ls'));
    if (existsSync('/opt/homebrew/bin/brew')) assert(JSON.stringify(bash.content).includes('/opt/homebrew/bin/brew'));
  } finally {
    await client.close();
  }
  const original = { id: session.id, mcpUrl: session.mcpUrl };
  await session.close();
  await session.close();
  session = undefined;
  assert.equal(existsSync(join(root, 'sandboxes', original.id, 'workspace')), true);
  const occupiedPort = Number(new URL(original.mcpUrl).port);
  const occupant = createServer();
  await new Promise<void>((resolve, reject) => {
    occupant.once('error', reject);
    occupant.listen(occupiedPort, '127.0.0.1', resolve);
  });
  try {
    session = await startPeriSandbox({ configPath });
    assert.equal(session.id, original.id);
    assert.notEqual(session.mcpUrl, original.mcpUrl);
    assert.equal(
      (await connectSandboxControl({ socketPath: join(root, 'control', 'control.sock') }).inspect(session.id)).mcpUrl,
      session.mcpUrl,
    );
    const response = await fetch(session.mcpUrl);
    assert.equal(response.status, 401);
    await session.close();
    session = undefined;
  } finally {
    await new Promise<void>((resolve) => occupant.close(() => resolve()));
  }
  session = await startPeriSandbox({ configPath });
  assert.equal(session.id, original.id);
  assert.equal(session.mcpUrl, original.mcpUrl);
  console.log('macOS: programmatic start, MCP, occupied-port fallback and preferred-port recovery passed');
} finally {
  await session?.close();
  rmSync(root, { recursive: true, force: true });
}
