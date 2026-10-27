import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { randomBytes, randomUUID } from 'node:crypto';
import { existsSync, readFileSync, symlinkSync, writeFileSync } from 'node:fs';
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';
import { z } from 'zod';
import { loadSettings, startSandbox, type SandboxInstance } from '../../src/index.js';

const settingsPath = process.argv[2];
if (!settingsPath) throw new Error('Usage: node dist/tests/integration/linux.js /absolute/settings.json');
const settings = loadSettings(settingsPath);
const owner = `vm-${randomUUID()}`;
const resultSchema = z.record(z.string(), z.unknown());
const instances: SandboxInstance[] = [];
const clients: Client[] = [];
const report: string[] = [];

function sandboxId(): string {
  return `sbx_${randomBytes(16).toString('hex')}`;
}

async function connect(instance: SandboxInstance, tasks = true): Promise<Client> {
  const client = new Client(
    { name: 'peri-sandbox-linux-acceptance', version: '1' },
    {
      versionNegotiation: { mode: { pin: '2026-07-28' } },
      capabilities: tasks ? { extensions: { 'io.modelcontextprotocol/tasks': {} } } : {},
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

function call(client: Client, method: string, params: Record<string, unknown>): Promise<Record<string, unknown>> {
  return client.request({ method, params: { ...params, _meta: { 'peri/taskOwner': owner } } }, resultSchema);
}

async function taskUntilTerminal(client: Client, taskId: string, timeout = 12000): Promise<Record<string, unknown>> {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const state = await call(client, 'tasks/get', { taskId });
    if (['completed', 'failed', 'cancelled'].includes(String(state.status))) return state;
    await new Promise((done) => setTimeout(done, 150));
  }
  throw new Error(`task did not reach terminal state: ${taskId}`);
}

async function waitFor(check: () => boolean, timeout = 15000): Promise<void> {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (check()) return;
    await new Promise((done) => setTimeout(done, 100));
  }
  throw new Error('condition not reached before timeout');
}

function cgroupRoot(instance: SandboxInstance): string {
  const unit = instance.runtime.unit;
  assert(unit, 'Linux instance must expose a transient unit');
  assert.equal(instance.runtime.provider, 'systemd-user-unit');
  const group = execFileSync('systemctl', ['--user', 'show', unit, '-p', 'ControlGroup', '--value'], {
    encoding: 'utf8',
  }).trim();
  assert(group.startsWith('/user.slice/') && group.endsWith(`/${unit}`), group);
  return `/sys/fs/cgroup${group}`;
}

try {
  const first = await startSandbox({ settingsPath, sandboxId: sandboxId(), minFreeBytes: 0 });
  const second = await startSandbox({ settingsPath, sandboxId: sandboxId(), minFreeBytes: 0 });
  instances.push(first, second);
  assert.equal(first.state, 'ready');
  assert.notEqual(first.mcpUrl, second.mcpUrl);

  const firstRoot = cgroupRoot(first);
  const processes = readFileSync(`${firstRoot}/cgroup.procs`, 'utf8').trim().split('\n');
  const commands = processes.map((pid) => [pid, readFileSync(`/proc/${pid}/cmdline`, 'utf8').replaceAll('\0', ' ')]);
  const periProcesses = commands.filter(([, command]) => command.startsWith(`${settings.periPath} mcp-start `));
  assert.equal(periProcesses.length, 1, `expected one Peri backend per unit: ${JSON.stringify(commands)}`);
  report.push('A02: exactly one live Peri backend in the instance cgroup');

  const client = await connect(first);
  const tools = await client.listTools();
  for (const tool of ['Read', 'Write', 'Bash'])
    assert(
      tools.tools.some((item) => item.name === tool),
      `missing tool ${tool}`,
    );
  const path = `${first.workspacePath}/acceptance.txt`;
  writeFileSync(`${second.workspacePath}/secret.txt`, 'other-sandbox-secret');
  assert.equal(
    (
      await call(client, 'tools/call', {
        name: 'Write',
        arguments: { file_path: path, content: 'linux-acceptance' },
      })
    ).isError,
    false,
  );
  assert(
    JSON.stringify(
      (await call(client, 'tools/call', { name: 'Read', arguments: { file_path: path } })).content,
    ).includes('linux-acceptance'),
  );
  const bash = await call(client, 'tools/call', { name: 'Bash', arguments: { command: 'pwd' } });
  assert.equal(bash.isError, false, JSON.stringify(bash));
  assert(JSON.stringify(bash.content).includes(first.workspacePath));
  report.push('A01: real HTTP → SRT → Peri Write/Read/Bash');

  for (const forbidden of [
    `${second.workspacePath}/secret.txt`,
    `${first.sandboxRoot}/policy.json`,
    `/run/user/${process.getuid?.()}/bus`,
  ]) {
    const denied = await call(client, 'tools/call', { name: 'Read', arguments: { file_path: forbidden } });
    assert.equal(denied.isError, true, `protected read succeeded: ${forbidden}`);
  }
  symlinkSync(`${second.workspacePath}/secret.txt`, `${first.workspacePath}/cross-sandbox-link`);
  assert.equal(
    (
      await call(client, 'tools/call', {
        name: 'Read',
        arguments: { file_path: `${first.workspacePath}/cross-sandbox-link` },
      })
    ).isError,
    true,
  );
  await call(client, 'tools/call', {
    name: 'Write',
    arguments: { file_path: `${second.workspacePath}/overwrite.txt`, content: 'cross-write' },
  });
  assert.equal(existsSync(`${second.workspacePath}/overwrite.txt`), false, 'Write escaped into another sandbox');
  report.push('A04/A05: cross-sandbox reads, policy file and host bus denied');

  assert.equal((await fetch(first.mcpUrl)).status, 401);
  const legacy = await fetch(first.mcpUrl, {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${first.token}`,
      Accept: 'application/json, text/event-stream',
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({
      jsonrpc: '2.0',
      id: 1,
      method: 'initialize',
      params: { protocolVersion: '2025-03-26', capabilities: {}, clientInfo: { name: 'legacy', version: '1' } },
    }),
  });
  assert(legacy.status >= 400 || (await legacy.text()).includes('error'), 'legacy initialize was accepted');
  const oversize = await fetch(first.mcpUrl, {
    method: 'POST',
    headers: { Authorization: `Bearer ${first.token}`, 'Content-Type': 'application/json' },
    body: 'x'.repeat(1024 * 1024 + 1),
  });
  assert.equal(oversize.status, 413);
  report.push('A15/A16: unauthenticated, legacy and oversize requests rejected');

  const background = await call(client, 'tools/call', {
    name: 'Bash',
    arguments: { command: 'sleep 1; exit 7', run_in_background: true },
  });
  assert.equal(background.resultType, 'task', JSON.stringify(background));
  const finished = await taskUntilTerminal(client, String(background.taskId));
  assert.equal((finished.result as { isError: boolean })?.isError, true);
  await assert.rejects(
    client.request(
      { method: 'tasks/get', params: { taskId: background.taskId, _meta: { 'peri/taskOwner': `${owner}-other` } } },
      resultSchema,
    ),
    /unknown|task/i,
  );
  report.push('A08: Tasks mapping and owner boundary');

  const firstRun = first.runId;
  await client.close();
  await first.close();
  assert.equal(existsSync(path), true);
  const restarted = await startSandbox({ settingsPath, sandboxId: first.id, minFreeBytes: 0 });
  instances.push(restarted);
  assert.notEqual(restarted.runId, firstRun);
  assert.equal(restarted.workspacePath, first.workspacePath);
  assert(existsSync(path));
  const nextClient = await connect(restarted);
  await assert.rejects(call(nextClient, 'tasks/get', { taskId: background.taskId }), /unknown|task/i);
  report.push('A03: workspace persists, new runId, stale task rejected, URL may change');

  const restartedRoot = cgroupRoot(restarted);
  const pids = readFileSync(`${restartedRoot}/cgroup.procs`, 'utf8').trim().split('\n');
  const periPid = pids.find((pid) =>
    readFileSync(`/proc/${pid}/cmdline`, 'utf8').replaceAll('\0', ' ').startsWith(`${settings.periPath} mcp-start `),
  );
  assert(periPid, 'Peri process missing before crash probe');
  process.kill(Number(periPid), 'SIGKILL');
  await waitFor(() => restarted.state === 'failed');
  assert.equal(restarted.status().lastError?.code, 'backend_exited');
  await waitFor(() => !existsSync(restartedRoot));
  await restarted.close();
  report.push('A12: SIGKILL of Peri marks the instance failed and the cgroup disappears');

  await second.close();
  await second.close({ deleteData: true });
  assert.equal(existsSync(`${settings.sandboxesRoot}/${second.id}`), false);
  report.push('A17: deleteData removes only the managed sandbox directory');

  console.log(
    JSON.stringify(
      { passed: report, environment: { platform: process.platform, srt: '0.0.81', mcpSdk: '2.4.0' } },
      null,
      2,
    ),
  );
} finally {
  await Promise.allSettled(clients.map((client) => client.close()));
  for (const instance of instances.reverse()) {
    try {
      await instance.close({ deleteData: true });
    } catch (error) {
      console.error(`linux acceptance cleanup incomplete: ${instance.id}`, error);
      throw error;
    }
  }
}
