import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';
import { z } from 'zod';
import { connectSandboxControl, loadConfig } from '../../src/index.js';
import { macConfigPath } from '../../src/runtime/mac-config.js';

if (process.platform !== 'darwin') throw new Error('macOS acceptance runs only on macOS');
const config = loadConfig(process.argv[2] ?? macConfigPath());
const control = connectSandboxControl({ socketPath: `${config.controlRoot}/control.sock` });
const token = readFileSync(config.auth.tokenFile, 'utf8').trim();
const requestId = () => randomUUID();
const created = await control.create(
  { name: 'mac-acceptance', profile: 'workspace-default' },
  { requestId: requestId() },
);
let client: Client | undefined;
try {
  const running = await control.start(created.id, { requestId: requestId() });
  assert.equal(running.state, 'ready');
  client = new Client(
    { name: 'peri-sandbox-mac-acceptance', version: '1' },
    {
      versionNegotiation: { mode: { pin: '2026-07-28' } },
      capabilities: { extensions: { 'io.modelcontextprotocol/tasks': {} } },
    },
  );
  await client.connect(
    new StreamableHTTPClientTransport(new URL(running.mcpUrl), {
      requestInit: { headers: { Authorization: `Bearer ${token}` } },
    }),
  );
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
  const resultSchema = z.record(z.string(), z.unknown());
  const call = (name: string, args: Record<string, unknown>) =>
    client!.request(
      {
        method: 'tools/call',
        params: { name, arguments: args, _meta: { 'peri/taskOwner': 'mac-acceptance' } },
      },
      resultSchema,
    );
  const target = `${config.sandboxesRoot}/${created.id}/workspace/mac-smoke.txt`;
  assert.equal((await call('Write', { file_path: target, content: 'mac-smoke-ok' })).isError, false);
  assert(JSON.stringify((await call('Read', { file_path: target })).content).includes('mac-smoke-ok'));
  const blocked = await call('Read', { file_path: config.auth.tokenFile });
  assert.equal(blocked.isError, true, 'Sandbox read the deployment token');
  const bash = await call('Bash', { command: 'pwd' });
  assert.equal(bash.isError, false, JSON.stringify(bash));
  assert(JSON.stringify(bash.content).includes(`${config.sandboxesRoot}/${created.id}/workspace`));
  const systemCommands = await call('Bash', { command: 'stat /bin/ls && command -v ls && ls -d .' });
  assert.equal(systemCommands.isError, false, JSON.stringify(systemCommands));
  assert(JSON.stringify(systemCommands.content).includes('/bin/ls'));
  await client.close();
  client = undefined;
  const stopped = await control.stop(created.id, { requestId: requestId() });
  assert.equal(stopped.state, 'stopped');
  const restarted = await control.start(created.id, { requestId: requestId() });
  assert.equal(restarted.mcpUrl, running.mcpUrl);
  console.log('macOS: MCP Write/Read/Bash, stop and restart passed');
} finally {
  await client?.close();
  await control.destroy(created.id, { requestId: requestId(), deleteData: true });
}
