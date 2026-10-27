import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { existsSync, readFileSync, symlinkSync, writeFileSync } from 'node:fs';
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';
import { z } from 'zod';
import { connectSandboxControl, loadConfig, type SandboxRecord } from '../../src/index.js';
import { srtRuntime } from '../../src/runtime/srt.js';

const file = process.argv[2];
if (!file) throw new Error('Usage: node dist/tests/integration/linux.js /absolute/config.json');
const config = loadConfig(file);
const control = connectSandboxControl({ socketPath: `${config.controlRoot}/control.sock` });
const token = readFileSync(config.auth.tokenFile, 'utf8').trim();
const owner = `vm-${randomUUID()}`;
const requestId = () => randomUUID();
const resultSchema = z.record(z.string(), z.unknown());
const created: SandboxRecord[] = [];
const clients: Client[] = [];
const report: string[] = [];

async function connect(record: SandboxRecord, tasks = true): Promise<Client> {
  const client = new Client(
    { name: 'peri-sandbox-linux-acceptance', version: '1' },
    {
      versionNegotiation: { mode: { pin: '2026-07-28' } },
      capabilities: tasks ? { extensions: { 'io.modelcontextprotocol/tasks': {} } } : {},
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

try {
  const first = await control.create(
    { name: 'linux-acceptance-a', profile: 'workspace-default' },
    { requestId: requestId() },
  );
  const second = await control.create(
    { name: 'linux-acceptance-b', profile: 'workspace-default' },
    { requestId: requestId() },
  );
  created.push(first, second);
  const running = await control.start(first.id, { requestId: requestId() });
  await control.start(second.id, { requestId: requestId() });
  assert.equal(running.state, 'ready');
  const firstUnit = `${config.provider.unitPrefix}${first.id}-${running.generation}.service`;
  const firstGroup = execFileSync('systemctl', ['--user', 'show', firstUnit, '-p', 'ControlGroup', '--value'], {
    encoding: 'utf8',
  }).trim();
  assert(firstGroup.startsWith('/user.slice/'));
  const processes = readFileSync(`/sys/fs/cgroup${firstGroup}/cgroup.procs`, 'utf8').trim().split('\n');
  const commands = processes.map((pid) => [pid, readFileSync(`/proc/${pid}/cmdline`, 'utf8').replaceAll('\0', ' ')]);
  const periProcesses = commands.filter(([, command]) => command.startsWith(`${config.runtime.periPath} mcp-start `));
  assert.equal(periProcesses.length, 1, `expected one Peri backend per generation: ${JSON.stringify(commands)}`);
  report.push('A02: exactly one live Peri backend in the generation cgroup');
  const client = await connect(running);
  const tools = await client.listTools();
  for (const tool of ['Read', 'Write', 'Bash']) assert(tools.tools.some((item) => item.name === tool));
  assert.deepEqual(tools.tools.map((tool) => tool.name).sort(), [
    'Bash',
    'Edit',
    'Glob',
    'Grep',
    'Read',
    'Write',
    'folder_operations',
  ]);
  const workspace = `${config.sandboxesRoot}/${first.id}/workspace`;
  const other = `${config.sandboxesRoot}/${second.id}/workspace`;
  const otherHome = `${config.sandboxesRoot}/${second.id}/home`;
  const path = `${workspace}/acceptance.txt`;
  writeFileSync(`${other}/secret.txt`, 'other-sandbox-secret');
  writeFileSync(`${otherHome}/secret.txt`, 'other-home-secret');
  assert.equal(
    (await call(client, 'tools/call', { name: 'Write', arguments: { file_path: path, content: 'mac-host-linux-vm' } }))
      .isError,
    false,
  );
  const read = await call(client, 'tools/call', { name: 'Read', arguments: { file_path: path } });
  assert(JSON.stringify(read.content).includes('mac-host-linux-vm'));
  const bash = await call(client, 'tools/call', { name: 'Bash', arguments: { command: 'pwd' } });
  assert.equal(bash.isError, false, JSON.stringify(bash));
  assert(JSON.stringify(bash.content).includes(workspace));
  report.push('A01: real HTTP → SRT → Peri Write/Read/Bash');

  const denied = await call(client, 'tools/call', { name: 'Read', arguments: { file_path: `${other}/secret.txt` } });
  assert.equal(denied.isError, true, JSON.stringify(denied));
  const controlRead = await call(client, 'tools/call', {
    name: 'Read',
    arguments: { file_path: config.auth.tokenFile },
  });
  assert.equal(controlRead.isError, true, JSON.stringify(controlRead));
  for (const forbidden of [
    `${otherHome}/secret.txt`,
    `${workspace}/../../${second.id}/workspace/secret.txt`,
    `${config.controlRoot}/config.json`,
    `/run/user/${process.getuid?.()}/bus`,
  ]) {
    const result = await call(client, 'tools/call', { name: 'Read', arguments: { file_path: forbidden } });
    assert.equal(result.isError, true, `protected read succeeded for ${forbidden}: ${JSON.stringify(result)}`);
  }
  symlinkSync(`${other}/secret.txt`, `${workspace}/cross-sandbox-link`);
  assert.equal(
    (await call(client, 'tools/call', { name: 'Read', arguments: { file_path: `${workspace}/cross-sandbox-link` } }))
      .isError,
    true,
  );
  await call(client, 'tools/call', {
    name: 'Write',
    arguments: { file_path: `${other}/overwrite.txt`, content: 'cross-write' },
  });
  assert.equal(existsSync(`${other}/overwrite.txt`), false, 'Write escaped into another sandbox');
  await call(client, 'tools/call', {
    name: 'Bash',
    arguments: { command: `printf cross-write > ${other}/shell-write.txt` },
  });
  assert.equal(existsSync(`${other}/shell-write.txt`), false, 'Bash escaped into another sandbox');
  const sibling = `${config.sandboxesRoot}/unowned-${randomUUID()}`;
  await call(client, 'tools/call', { name: 'Bash', arguments: { command: `mkdir ${sibling}` } });
  assert.equal(existsSync(sibling), false, 'sandbox created an unowned sibling directory');
  report.push('A04/A05: other workspace and control token denied');

  const secondClient = await connect(await control.inspect(second.id));
  const tempMarker = `acceptance-${randomUUID()}`;
  const temporary = await call(client, 'tools/call', {
    name: 'Bash',
    arguments: { command: `mkdir -p /tmp/claude; printf %s ${tempMarker} > /tmp/claude/cross-sandbox-marker` },
  });
  assert.equal(temporary.isError, false, JSON.stringify(temporary));
  const isolatedTmp = await call(secondClient, 'tools/call', {
    name: 'Bash',
    arguments: { command: 'test ! -e /tmp/claude/cross-sandbox-marker' },
  });
  assert.equal(isolatedTmp.isError, false, JSON.stringify(isolatedTmp));
  const blockedNetwork = await call(client, 'tools/call', {
    name: 'Bash',
    arguments: { command: 'curl -fsS --max-time 2 https://example.com >/dev/null' },
  });
  assert.equal(blockedNetwork.isError, true, JSON.stringify(blockedNetwork));
  report.push('A04/A05/A06: symlink, home, private tmp and default egress denial');

  const background = await call(client, 'tools/call', {
    name: 'Bash',
    arguments: { command: 'sleep 1; exit 7', run_in_background: true },
  });
  assert.equal(background.resultType, 'task', JSON.stringify(background));
  const taskId = String(background.taskId);
  assert.match(taskId, /^task_[0-9a-f]{48}$/);
  assert.equal(background.status, 'working', JSON.stringify(background));
  await call(client, 'tasks/update', { taskId });
  const finished = await taskUntilTerminal(client, taskId);
  assert.equal(finished.taskId, taskId);
  assert.equal(finished.status, 'completed');
  assert.equal((finished.result as { isError: boolean })?.isError, true, JSON.stringify(finished));
  await assert.rejects(
    client.request(
      { method: 'tasks/get', params: { taskId, _meta: { 'peri/taskOwner': `${owner}-other` } } },
      resultSchema,
    ),
    /unknown|task/i,
  );
  const normal = await call(client, 'tools/call', {
    name: 'Bash',
    arguments: { command: 'sleep 1; exit 0', run_in_background: true },
  });
  assert.equal(normal.resultType, 'task', JSON.stringify(normal));
  const normalResult = await taskUntilTerminal(client, String(normal.taskId));
  assert.equal(normalResult.status, 'completed');
  assert.equal((normalResult.result as { isError: boolean })?.isError, false, JSON.stringify(normalResult));
  report.push('A08: task creation, update acknowledgement, normal/nonzero results, owner boundary');

  const cancelledMarker = `${workspace}/cancelled-task-survived`;
  const waiting = await call(client, 'tools/call', {
    name: 'Bash',
    arguments: { command: `sleep 3; echo survived > ${cancelledMarker}`, run_in_background: true },
  });
  assert.equal(waiting.resultType, 'task', JSON.stringify(waiting));
  const cancelledTask = String(waiting.taskId);
  assert.match(cancelledTask, /^task_[0-9a-f]{48}$/);
  await call(client, 'tasks/cancel', { taskId: cancelledTask });
  const cancelled = await taskUntilTerminal(client, cancelledTask);
  assert.equal(cancelled.taskId, cancelledTask);
  assert.equal(cancelled.status, 'cancelled', JSON.stringify(cancelled));
  await new Promise((done) => setTimeout(done, 3500));
  assert.equal(existsSync(cancelledMarker), false, 'cancelled Bash kept executing after task reached cancelled state');
  report.push('A08: task cancellation reaches terminal state and shell stops');

  const parallel = await connect(running);
  const [firstRead, parallelRead] = await Promise.all([
    call(client, 'tools/call', { name: 'Read', arguments: { file_path: path } }),
    call(parallel, 'tools/call', { name: 'Read', arguments: { file_path: path } }),
  ]);
  assert.equal(firstRead.isError, false);
  assert.equal(parallelRead.isError, false);
  assert.equal((await call(parallel, 'tasks/get', { taskId })).status, 'completed');
  let longFinished = false;
  const longCall = call(client, 'tools/call', {
    name: 'Bash',
    arguments: { command: 'sleep 2; echo long-finished' },
  }).finally(() => {
    longFinished = true;
  });
  await new Promise((done) => setTimeout(done, 150));
  const duringLong = await call(parallel, 'tasks/get', { taskId });
  assert.equal(duringLong.status, 'completed');
  assert.equal(longFinished, false, 'task query waited behind a long Bash call');
  assert.equal((await longCall).isError, false);
  report.push('A10: two simultaneous HTTP connections and task query during long Bash');

  await client.close();
  const reconnected = await connect(running);
  assert.equal((await call(reconnected, 'tasks/get', { taskId })).status, 'completed');
  report.push('A09: task survives HTTP connection close within generation');

  const unauthenticated = await fetch(running.mcpUrl);
  assert.equal(unauthenticated.status, 401);
  const legacy = await fetch(running.mcpUrl, {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${token}`,
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
  const oversize = await fetch(running.mcpUrl, {
    method: 'POST',
    headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
    body: 'x'.repeat(1024 * 1024 + 1),
  });
  assert.equal(oversize.status, 413);
  const noTasks = await connect(running, false);
  await assert.rejects(
    call(noTasks, 'tools/call', { name: 'Bash', arguments: { command: 'pwd' } }),
    /Tasks|capability/i,
  );
  report.push('A15/A16: unauthenticated, legacy and missing Tasks rejected; >1MiB request rejected');

  await control.stop(first.id, { requestId: requestId() });
  const restarted = await control.start(first.id, { requestId: requestId() });
  assert.equal(restarted.mcpUrl, running.mcpUrl);
  assert.notEqual(restarted.generation, running.generation);
  const nextClient = await connect(restarted);
  assert(
    JSON.stringify(
      (await call(nextClient, 'tools/call', { name: 'Read', arguments: { file_path: path } })).content,
    ).includes('mac-host-linux-vm'),
  );
  await assert.rejects(call(nextClient, 'tasks/get', { taskId }), /unknown|task/i);
  await assert.rejects(call(nextClient, 'tasks/update', { taskId }), /unknown|task/i);
  await assert.rejects(call(nextClient, 'tasks/cancel', { taskId }), /unknown|task/i);
  report.push('A03: stable URL/file, fresh generation, stale task denied');

  const escaped = `${workspace}/descendant-escaped`;
  const orphan = await call(nextClient, 'tools/call', {
    name: 'Bash',
    arguments: {
      command: `setsid sh -c 'sh -c "sleep 3; echo escaped > ${escaped}" </dev/null >/dev/null 2>&1 &' </dev/null >/dev/null 2>&1 &`,
    },
  });
  assert(orphan.isError === false || orphan.resultType === 'task', JSON.stringify(orphan));
  const unit = `${config.provider.unitPrefix}${first.id}-${restarted.generation}.service`;
  const group = execFileSync('systemctl', ['--user', 'show', unit, '-p', 'ControlGroup', '--value'], {
    encoding: 'utf8',
  }).trim();
  assert(group.startsWith('/user.slice/'));
  assert(
    readFileSync(`/sys/fs/cgroup${group}/cgroup.procs`, 'utf8').trim().split('\n').length > 1,
    'no running descendant found in cgroup',
  );
  await control.stop(first.id, { requestId: requestId() });
  assert.equal((await control.inspect(first.id)).state, 'stopped');
  await new Promise((done) => setTimeout(done, 3500));
  assert.equal(existsSync(escaped), false, 'detached descendant kept running after stop');
  report.push('A12: double-forked setsid descendant terminated before stop reports success');

  const existing = await control.inspect(second.id);
  const secondWorkspace = `${config.sandboxesRoot}/${second.id}/workspace`;
  writeFileSync(`${secondWorkspace}/restart-retained.txt`, 'persist-across-service-restart');
  const restartUnit = execFileSync('systemctl', ['--user', 'restart', config.provider.apiUnit], {
    encoding: 'utf8',
    timeout: 30000,
  });
  assert.equal(restartUnit, '');
  const readyBy = Date.now() + 30000;
  while (!existsSync(`${config.controlRoot}/control.sock`) && Date.now() < readyBy)
    await new Promise((done) => setTimeout(done, 100));
  assert(existsSync(`${config.controlRoot}/control.sock`), 'API control socket did not return after restart');
  const afterRestart = await control.inspect(second.id);
  assert.equal(afterRestart.state, 'stopped');
  assert.equal(afterRestart.desiredState, 'stopped');
  assert.equal(afterRestart.mcpUrl, existing.mcpUrl);
  assert.equal(readFileSync(`${secondWorkspace}/restart-retained.txt`, 'utf8'), 'persist-across-service-restart');
  report.push('A14: real systemd API restart stops old unit without replay and preserves data');

  const crashSandbox = await control.create({ profile: 'workspace-default' }, { requestId: requestId() });
  created.push(crashSandbox);
  const crashRun = await control.start(crashSandbox.id, { requestId: requestId() });
  const crashUnit = `${config.provider.unitPrefix}${crashSandbox.id}-${crashRun.generation}.service`;
  const crashGroup = execFileSync('systemctl', ['--user', 'show', crashUnit, '-p', 'ControlGroup', '--value'], {
    encoding: 'utf8',
  }).trim();
  assert(crashGroup.startsWith('/user.slice/'));
  const crashPids = readFileSync(`/sys/fs/cgroup${crashGroup}/cgroup.procs`, 'utf8').trim().split('\n');
  const periPid = crashPids.find((pid) =>
    readFileSync(`/proc/${pid}/cmdline`, 'utf8')
      .replaceAll('\0', ' ')
      .startsWith(`${config.runtime.periPath} mcp-start `),
  );
  assert(periPid, 'Peri process missing before crash probe');
  process.kill(Number(periPid), 'SIGKILL');
  const crashedBy = Date.now() + 15000;
  while ((await control.inspect(crashSandbox.id)).generation && Date.now() < crashedBy)
    await new Promise((done) => setTimeout(done, 100));
  const afterCrash = await control.inspect(crashSandbox.id);
  assert.equal(afterCrash.state, 'failed', JSON.stringify(afterCrash));
  assert.equal(afterCrash.generation, undefined, 'dead backend left a registered generation');
  assert.equal(existsSync(`/sys/fs/cgroup${crashGroup}`), false, 'killed Peri left an active cgroup');
  report.push('A12: SIGKILL of Peri triggers full unit cleanup without replay');

  const srtRun = await control.start(crashSandbox.id, { requestId: requestId() });
  const srtUnit = `${config.provider.unitPrefix}${crashSandbox.id}-${srtRun.generation}.service`;
  const srtGroup = execFileSync('systemctl', ['--user', 'show', srtUnit, '-p', 'ControlGroup', '--value'], {
    encoding: 'utf8',
  }).trim();
  assert(srtGroup.startsWith('/user.slice/'));
  const srtPids = readFileSync(`/sys/fs/cgroup${srtGroup}/cgroup.procs`, 'utf8').trim().split('\n');
  const srtPid = srtPids.find((pid) => {
    const command = readFileSync(`/proc/${pid}/cmdline`, 'utf8').replaceAll('\0', ' ');
    return command.includes(srtRuntime().cli) && command.includes('--settings');
  });
  assert(
    srtPid,
    `SRT process missing: ${JSON.stringify(srtPids.map((pid) => readFileSync(`/proc/${pid}/cmdline`, 'utf8').replaceAll('\0', ' ')))}`,
  );
  process.kill(Number(srtPid), 'SIGKILL');
  const srtStoppedBy = Date.now() + 15000;
  while ((await control.inspect(crashSandbox.id)).generation && Date.now() < srtStoppedBy)
    await new Promise((done) => setTimeout(done, 100));
  assert.equal((await control.inspect(crashSandbox.id)).state, 'failed');
  assert.equal(existsSync(`/sys/fs/cgroup${srtGroup}`), false, 'killed SRT left an active cgroup');
  report.push('A12: SIGKILL of SRT cleans Peri and its cgroup');

  const apiVictim = await control.create({ profile: 'workspace-default' }, { requestId: requestId() });
  created.push(apiVictim);
  const apiRun = await control.start(apiVictim.id, { requestId: requestId() });
  const apiWorkspace = `${config.sandboxesRoot}/${apiVictim.id}/workspace`;
  writeFileSync(`${apiWorkspace}/api-kill-retained.txt`, 'retained');
  const apiUnit = `${config.provider.unitPrefix}${apiVictim.id}-${apiRun.generation}.service`;
  const apiGroup = execFileSync('systemctl', ['--user', 'show', apiUnit, '-p', 'ControlGroup', '--value'], {
    encoding: 'utf8',
  }).trim();
  execFileSync('systemctl', ['--user', 'kill', '--kill-whom=main', '--signal=SIGKILL', config.provider.apiUnit], {
    timeout: 15000,
  });
  const apiRecoveredBy = Date.now() + 30000;
  let afterApiKill: SandboxRecord | undefined;
  while (Date.now() < apiRecoveredBy) {
    try {
      afterApiKill = await control.inspect(apiVictim.id);
      if (afterApiKill.state === 'stopped' && !afterApiKill.generation) break;
    } catch {}
    await new Promise((done) => setTimeout(done, 200));
  }
  assert.equal(afterApiKill?.state, 'stopped', JSON.stringify(afterApiKill));
  assert.equal(afterApiKill.generation, undefined);
  assert.equal(existsSync(`/sys/fs/cgroup${apiGroup}`), false, 'killed API left an active child cgroup');
  assert.equal(readFileSync(`${apiWorkspace}/api-kill-retained.txt`, 'utf8'), 'retained');
  report.push('A12/A14: SIGKILL of API cleans child unit, recovers data without replay');

  await control.destroy(first.id, { requestId: requestId(), deleteData: true });
  assert.equal((await control.inspect(first.id)).state, 'deleted');
  assert.equal(existsSync(`${config.sandboxesRoot}/${first.id}`), false);
  assert.equal(
    (await control.list()).some((item) => item.id === first.id),
    false,
  );
  const deletedResponse = await fetch(running.mcpUrl, { headers: { Authorization: `Bearer ${token}` } });
  assert.equal(deletedResponse.status, 410);
  await assert.rejects(
    control.start(first.id, { requestId: requestId() }),
    (error) => (error as { code?: string }).code === 'deleted',
  );
  report.push('A17: tombstone prevents restart; only owned data removed; URL returns 410');

  console.log(
    JSON.stringify(
      { passed: report, environment: { platform: process.platform, srt: '0.0.81', mcpSdk: '2.4.0' } },
      null,
      2,
    ),
  );
} finally {
  await Promise.allSettled(clients.map((client) => client.close()));
  const readyBy = Date.now() + 30000;
  while (!existsSync(`${config.controlRoot}/control.sock`) && Date.now() < readyBy)
    await new Promise((done) => setTimeout(done, 100));
  for (const record of created.reverse()) {
    try {
      await control.destroy(record.id, { requestId: requestId(), deleteData: true });
    } catch (error) {
      console.error(`cleanup incomplete for ${record.id}`, error);
    }
  }
}
