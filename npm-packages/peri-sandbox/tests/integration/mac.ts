import assert from 'node:assert/strict';
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
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
const hostOnlySkill = join(homedir(), '.claude', 'skills', `peri-sandbox-host-only-${process.pid}`);
const hostOnlyFile = join(tmpdir(), `peri-sandbox-host-secret-${process.pid}`);

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
  const capabilities = client.getServerCapabilities();
  assert(capabilities?.resources, 'outer discover must mirror the inner resources capability');
  assert.equal(capabilities.resources.subscribe, true, 'resources.subscribe must be mirrored');
  assert(capabilities.extensions?.['io.modelcontextprotocol/skills'], 'skills extension must be mirrored');
  assert(capabilities.extensions?.['io.modelcontextprotocol/tasks'], 'tasks extension must be mirrored');

  const demoDir = join(instance.workspacePath, '.claude', 'skills', 'demo');
  mkdirSync(demoDir, { recursive: true });
  writeFileSync(join(demoDir, 'SKILL.md'), '---\nname: demo\ndescription: sandbox demo skill\n---\n\nMAC_DEMO_SKILL\n');
  mkdirSync(hostOnlySkill, { recursive: true });
  writeFileSync(
    join(hostOnlySkill, 'SKILL.md'),
    '---\nname: peri-sandbox-host-only\ndescription: must stay on the host\n---\n\nHOST_ONLY\n',
  );
  const skills = await client.request({ method: 'skills/list', params: {} }, resultSchema);
  const skillNames = (skills.skills as Array<{ frontmatter?: { name?: string } }>).map(
    (skill) => skill.frontmatter?.name,
  );
  assert(skillNames.includes('demo'), `sandbox skill missing: ${JSON.stringify(skillNames)}`);
  assert(
    !skillNames.includes('peri-sandbox-host-only'),
    `host-only skill leaked into the sandbox: ${JSON.stringify(skillNames)}`,
  );
  const skillRead = await client.request(
    { method: 'resources/read', params: { uri: 'skill://project/demo/SKILL.md' } },
    resultSchema,
  );
  assert(
    JSON.stringify(skillRead.contents).includes('MAC_DEMO_SKILL'),
    'resources/read must return the sandbox skill content',
  );
  const resources = await client.request({ method: 'resources/list' }, resultSchema);
  assert(
    (resources.resources as Array<{ uri: string }>).some(
      (resource) => resource.uri === 'skill://project/demo/SKILL.md',
    ),
    'resources/list must expose the sandbox skill',
  );
  assert.equal(
    (await call('Read', { file_path: '/etc/passwd' })).isError,
    false,
    'the default profile intentionally allows runtime paths such as /etc',
  );
  writeFileSync(hostOnlyFile, 'HOST_SECRET');
  assert.equal(
    (await call('Read', { file_path: hostOnlyFile })).isError,
    true,
    'Read escaped the sandbox into a host-only file',
  );
  assert.equal(
    (await call('Bash', { command: `cat ${hostOnlyFile}` })).isError,
    true,
    'Bash escaped the sandbox into a host-only file',
  );
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
  rmSync(hostOnlySkill, { recursive: true, force: true });
  rmSync(hostOnlyFile, { force: true });
  await client?.close();
  await instance?.close({ deleteData: true });
}
