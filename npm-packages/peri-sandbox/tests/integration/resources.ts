import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';
import { z } from 'zod';
import { loadSettings, startSandbox, type SandboxInstance } from '../../src/index.js';

const settingsPath = process.argv[2];
if (!settingsPath) throw new Error('Usage: node dist/tests/integration/resources.js /absolute/settings.json');
const settings = loadSettings(settingsPath);
for (const profile of ['workspace-egress', 'workspace-limited', 'workspace-memory'])
  assert(settings.profiles[profile], `missing acceptance profile: ${profile}`);
const schema = z.record(z.string(), z.unknown());
const instances: SandboxInstance[] = [];
const clients: Client[] = [];
const passed: string[] = [];

async function start(profileName: string): Promise<SandboxInstance> {
  const instance = await startSandbox({
    settingsPath,
    sandboxId: `sbx_${randomBytes(16).toString('hex')}`,
    profileName,
    minFreeBytes: 0,
  });
  instances.push(instance);
  return instance;
}

async function connect(instance: SandboxInstance): Promise<Client> {
  const client = new Client(
    { name: 'peri-sandbox-resources-acceptance', version: '1' },
    {
      versionNegotiation: { mode: { pin: '2026-07-28' } },
      capabilities: { extensions: { 'io.modelcontextprotocol/tasks': {} } },
    },
  );
  await client.connect(
    new StreamableHTTPClientTransport(new URL(instance.mcpUrl), {
      requestInit: { headers: { Authorization: `Bearer ${instance.token}` } },
    }),
  );
  clients.push(client);
  return client;
}

async function bash(client: Client, command: string): Promise<Record<string, unknown>> {
  return client.request(
    {
      method: 'tools/call',
      params: { name: 'Bash', arguments: { command }, _meta: { 'peri/taskOwner': 'resource-acceptance' } },
    },
    schema,
  );
}

async function settle(client: Client, result: Record<string, unknown>): Promise<Record<string, unknown>> {
  if (result.resultType !== 'task') return result;
  const deadline = Date.now() + 30000;
  while (Date.now() < deadline) {
    const task = await client.request(
      { method: 'tasks/get', params: { taskId: result.taskId, _meta: { 'peri/taskOwner': 'resource-acceptance' } } },
      schema,
    );
    if (['completed', 'cancelled', 'failed'].includes(String(task.status))) return task;
    await new Promise((done) => setTimeout(done, 200));
  }
  throw new Error(`resource task did not settle: ${result.taskId}`);
}

function cgroup(instance: SandboxInstance): string {
  const unit = instance.runtime.unit;
  assert(unit, 'Linux instance must expose a transient unit');
  const group = execFileSync('systemctl', ['--user', 'show', unit, '-p', 'ControlGroup', '--value'], {
    encoding: 'utf8',
  }).trim();
  assert(group.startsWith('/user.slice/') && group.endsWith(`/${unit}`));
  return `/sys/fs/cgroup${group}`;
}

function event(root: string, file: string, field: string): number {
  const match = new RegExp(`^${field} (\\d+)$`, 'm').exec(readFileSync(`${root}/${file}`, 'utf8'));
  assert(match, `missing ${file}:${field}`);
  return Number(match[1]);
}

try {
  const egress = await start('workspace-egress');
  const permitted = await connect(egress);
  const allowed = await bash(permitted, 'curl -fsS --max-time 10 https://example.com >/dev/null');
  assert.equal(allowed.isError, false, `allowlisted HTTPS failed: ${JSON.stringify(allowed)}`);
  const denied = await bash(permitted, 'curl -fsS --max-time 3 https://www.wikipedia.org >/dev/null');
  assert.equal(denied.isError, true, `unexpected unrestricted HTTPS: ${JSON.stringify(denied)}`);
  passed.push('A06: allowlisted HTTPS succeeds; other HTTPS denied');
  await egress.close({ deleteData: true });

  const limited = await start('workspace-limited');
  const client = await connect(limited);
  const root = cgroup(limited);
  assert.equal(readFileSync(`${root}/memory.max`, 'utf8').trim(), String(512 * 1024 * 1024));
  assert.equal(readFileSync(`${root}/pids.max`, 'utf8').trim(), '32');
  assert.equal(readFileSync(`${root}/cpu.max`, 'utf8').trim().split(' ')[0], '50000');

  const throttled = event(root, 'cpu.stat', 'nr_throttled');
  await settle(client, await bash(client, 'timeout 2 sh -c "while :; do :; done"'));
  assert(event(root, 'cpu.stat', 'nr_throttled') > throttled, 'CPU quota never throttled workload');
  passed.push('A07: CPU quota throttles a real Bash child');

  const exhausted = event(root, 'pids.events', 'max');
  await settle(client, await bash(client, 'for number in $(seq 1 80); do sleep 2 & done; wait'));
  assert(event(root, 'pids.events', 'max') > exhausted, 'PID limit was not exercised');
  passed.push('A07: TasksMax blocks excess child processes');

  const capabilities = client.getServerCapabilities();
  assert(capabilities?.resources, 'outer discover must mirror resources');
  assert.equal(capabilities.resources.subscribe, true, 'resources.subscribe must be mirrored');
  assert(capabilities.extensions?.['io.modelcontextprotocol/skills'], 'skills extension must be mirrored');
  const skillDir = `${limited.workspacePath}/.claude/skills/demo`;
  mkdirSync(skillDir, { recursive: true });
  writeFileSync(skillDir + '/SKILL.md', '---\nname: demo\ndescription: linux demo skill\n---\n\nLINUX_DEMO_SKILL\n');
  const skills = await client.request({ method: 'skills/list', params: {} }, schema);
  assert(
    (skills.skills as Array<{ frontmatter?: { name?: string } }>).some((skill) => skill.frontmatter?.name === 'demo'),
    `skills/list did not expose the sandbox skill: ${JSON.stringify(skills)}`,
  );
  const skillRead = await client.request(
    { method: 'resources/read', params: { uri: 'skill://project/demo/SKILL.md' } },
    schema,
  );
  assert(
    JSON.stringify(skillRead.contents).includes('LINUX_DEMO_SKILL'),
    'resources/read did not return the sandbox skill',
  );
  const resourceList = await client.request({ method: 'resources/list' }, schema);
  assert(
    (resourceList.resources as Array<{ uri: string }>).some(
      (resource) => resource.uri === 'skill://project/demo/SKILL.md',
    ),
    'resources/list did not expose the sandbox skill',
  );
  passed.push('A08: capabilities/resources/skills are mirrored and readable inside the sandbox');
  await limited.close({ deleteData: true });

  const memory = await start('workspace-memory');
  const memoryClient = await connect(memory);
  const memoryRoot = cgroup(memory);
  const killed = event(memoryRoot, 'memory.events', 'oom_kill');
  const oversized = await bash(
    memoryClient,
    `echo 1000 > /proc/self/oom_score_adj; exec /opt/peri-runtime/node/bin/node -e 'const buffer=Buffer.alloc(700*1024*1024); for(let offset=0;offset<buffer.length;offset+=4096) buffer[offset]=1'`,
  );
  const memoryDeadline = Date.now() + 10000;
  while (event(memoryRoot, 'memory.events', 'oom_kill') === killed && Date.now() < memoryDeadline)
    await new Promise((done) => setTimeout(done, 200));
  assert(
    event(memoryRoot, 'memory.events', 'oom_kill') > killed,
    `memory limit did not kill allocation: ${JSON.stringify(oversized)}; ${readFileSync(`${memoryRoot}/memory.events`, 'utf8')}`,
  );
  passed.push('A07: MemoryMax kills an oversized child allocation');
  await memory.close({ deleteData: true });

  console.log(JSON.stringify({ passed }, null, 2));
} catch (error) {
  console.error('resource acceptance failed after:', passed, error);
  throw error;
} finally {
  await Promise.allSettled(clients.map((client) => client.close()));
  for (const instance of instances.reverse()) {
    try {
      await instance.close({ deleteData: true });
    } catch (error) {
      console.error(`resource acceptance cleanup incomplete: ${instance.id}`, error);
      throw error;
    }
  }
}
