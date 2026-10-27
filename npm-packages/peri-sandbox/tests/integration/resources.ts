import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';
import { z } from 'zod';
import { connectSandboxControl, loadConfig, type SandboxRecord } from '../../src/index.js';

const configFile = process.argv[2];
if (!configFile) throw new Error('Usage: node dist/tests/integration/resources.js /absolute/config.json');
const config = loadConfig(configFile);
for (const profile of ['workspace-egress', 'workspace-limited', 'workspace-memory'])
  assert(config.profiles[profile], `missing acceptance profile: ${profile}`);
const control = connectSandboxControl({ socketPath: `${config.controlRoot}/control.sock` });
const token = readFileSync(config.auth.tokenFile, 'utf8').trim();
const records: SandboxRecord[] = [];
const clients: Client[] = [];
const passed: string[] = [];
const requestId = () => randomUUID();
const schema = z.record(z.string(), z.unknown());

async function connect(record: SandboxRecord): Promise<Client> {
  const client = new Client(
    { name: 'peri-sandbox-resources-acceptance', version: '1' },
    {
      versionNegotiation: { mode: { pin: '2026-07-28' } },
      capabilities: { extensions: { 'io.modelcontextprotocol/tasks': {} } },
    },
  );
  await client.connect(
    new StreamableHTTPClientTransport(new URL(record.mcpUrl), {
      requestInit: { headers: { Authorization: `Bearer ${token}` } },
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

function cgroup(record: SandboxRecord): string {
  const unit = `${config.provider.unitPrefix}${record.id}-${record.generation}.service`;
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
  const egress = await control.create({ profile: 'workspace-egress' }, { requestId: requestId() });
  records.push(egress);
  const permitted = await connect(await control.start(egress.id, { requestId: requestId() }));
  const allowed = await bash(permitted, 'curl -fsS --max-time 10 https://example.com >/dev/null');
  assert.equal(allowed.isError, false, `allowlisted HTTPS failed: ${JSON.stringify(allowed)}`);
  const denied = await bash(permitted, 'curl -fsS --max-time 3 https://www.wikipedia.org >/dev/null');
  assert.equal(denied.isError, true, `unexpected unrestricted HTTPS: ${JSON.stringify(denied)}`);
  passed.push('A06: allowlisted HTTPS succeeds; other HTTPS denied');

  const limited = await control.create({ profile: 'workspace-limited' }, { requestId: requestId() });
  records.push(limited);
  const running = await control.start(limited.id, { requestId: requestId() });
  const client = await connect(running);
  const root = cgroup(running);
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

  await control.stop(limited.id, { requestId: requestId() });
  const memorySandbox = await control.create({ profile: 'workspace-memory' }, { requestId: requestId() });
  records.push(memorySandbox);
  const memoryClient = await connect(await control.start(memorySandbox.id, { requestId: requestId() }));
  const memoryRoot = cgroup(await control.inspect(memorySandbox.id));
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
    `memory limit did not kill allocation: ${JSON.stringify(oversized)}; ${readFileSync(`${memoryRoot}/memory.events`, 'utf8')}; pids=${readFileSync(`${memoryRoot}/pids.current`, 'utf8')}; memory=${readFileSync(`${memoryRoot}/memory.current`, 'utf8')}`,
  );
  passed.push('A07: MemoryMax kills an oversized child allocation');

  console.log(JSON.stringify({ passed }, null, 2));
} catch (error) {
  console.error('resource acceptance failed after:', passed, error);
  throw error;
} finally {
  await Promise.allSettled(clients.map((client) => client.close()));
  const cleanupFailures: unknown[] = [];
  for (const record of records.reverse()) {
    try {
      await control.destroy(record.id, { requestId: requestId(), deleteData: true });
    } catch (error) {
      console.error(`resource acceptance cleanup incomplete: ${record.id}`, error);
      cleanupFailures.push(error);
    }
  }
  if (cleanupFailures.length) throw new AggregateError(cleanupFailures, 'resource acceptance cleanup incomplete');
}
