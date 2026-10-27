import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';
import { z } from 'zod';
import { startSandbox, type SandboxInstance } from '../../src/index.js';

if (process.platform !== 'darwin') throw new Error('macOS acceptance runs only on macOS');
const settingsPath = process.argv[2];
if (!settingsPath) throw new Error('Usage: node dist/tests/integration/mac.js /absolute/settings.json');

const sandboxId = `sbx_${'ab'.repeat(16)}`;
const resultSchema = z.record(z.string(), z.unknown());
let instance: SandboxInstance | undefined;
let client: Client | undefined;

async function connect(target: SandboxInstance): Promise<Client> {
  const connection = new Client(
    { name: 'peri-sandbox-mac-acceptance', version: '1' },
    {
      versionNegotiation: { mode: { pin: '2026-07-28' } },
      capabilities: { extensions: { 'io.modelcontextprotocol/tasks': {} } },
    },
  );
  await connection.connect(
    new StreamableHTTPClientTransport(new URL(target.mcpUrl), {
      requestInit: { headers: { Authorization: `Bearer ${target.token}` } },
    }),
  );
  return connection;
}

function call(name: string, args: Record<string, unknown>): Promise<Record<string, unknown>> {
  return client!.request(
    { method: 'tools/call', params: { name, arguments: args, _meta: { 'peri/taskOwner': 'mac-acceptance' } } },
    resultSchema,
  );
}

try {
  instance = await startSandbox({ settingsPath, sandboxId, token: undefined });
  assert.equal(instance.state, 'ready');
  client = await connect(instance);
  const tools = await client.listTools();
  assert.deepEqual(tools.tools.map((tool) => tool.name).sort(), [
    'Bash',
    'Edit',
    'Glob',
    'Grep',
    'Read',
    'Write',
    'folder_operations',
  ]);
  const target = join(instance.workspacePath, 'mac-smoke.txt');
  assert.equal((await call('Write', { file_path: target, content: 'mac-smoke-ok' })).isError, false);
  assert(JSON.stringify((await call('Read', { file_path: target })).content).includes('mac-smoke-ok'));
  const blocked = await call('Read', { file_path: join(instance.sandboxRoot, 'policy.json') });
  assert.equal(blocked.isError, true, 'sandbox read the policy file');
  const bash = await call('Bash', { command: 'pwd' });
  assert.equal(bash.isError, false, JSON.stringify(bash));
  assert(JSON.stringify(bash.content).includes(instance.workspacePath));
  const systemCommands = await call('Bash', { command: 'stat /bin/ls && command -v ls && ls -d .' });
  assert.equal(systemCommands.isError, false, JSON.stringify(systemCommands));
  assert.equal((await fetch(instance.mcpUrl)).status, 401);

  const firstRun = instance.runId;
  await client.close();
  client = undefined;
  await instance.close();
  assert.equal(existsSync(target), true);
  instance = await startSandbox({ settingsPath, sandboxId });
  assert.notEqual(instance.runId, firstRun);
  assert.equal(readFileSync(target, 'utf8'), 'mac-smoke-ok');
  assert.equal(instance.workspacePath, join(instance.sandboxRoot, 'workspace'));
  await instance.close();
  instance = undefined;
  console.log('macOS: MCP Write/Read/Bash, token rejection, restart with the same workspace passed');
} finally {
  await client?.close();
  await instance?.close({ deleteData: true });
}
