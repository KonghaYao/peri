import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { tsImport } from 'tsx/esm/api';

const { startStdioAcpFixture } = await tsImport('../helpers/stdio-acp-fixture.ts', import.meta.url);
const bridge = fileURLToPath(new URL('../helpers/llm-replay-pty.py', import.meta.url));
const probeSource = `#!/usr/bin/env python3
import json,os,pathlib,signal,subprocess,sys,time
root=pathlib.Path(os.environ['PROBE_ROOT'])
mode=os.environ['PROBE_MODE']
signal.signal(signal.SIGHUP,signal.SIG_IGN)
child=None
if mode=='leak':
    source="import os,pathlib,time;pathlib.Path(os.environ['PROBE_ROOT'],'child-ready').write_text(str(os.getpid()));time.sleep(60)"
    child=subprocess.Popen([sys.executable,'-c',source],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    deadline=time.monotonic()+5
    while not (root/'child-ready').exists():
        if time.monotonic()>deadline: raise RuntimeError('child not ready')
        time.sleep(.005)
(root/'ready.json').write_text(json.dumps({'pid':os.getpid(),'pgid':os.getpgrp(),'child':child.pid if child else None}))
if mode=='crash': time.sleep(60)
else: sys.stdin.readline()
`;

async function world(mode) {
  const root = await mkdtemp(path.join(os.tmpdir(), 'peri-cleanup-oracle-'));
  const home = path.join(root, 'home'), binary = path.join(root, 'probe.py');
  await mkdir(home);
  await writeFile(binary, probeSource, { mode: 0o700 });
  return { root, home, binary, env: { HOME: home, PATH: process.env.PATH ?? '/usr/bin:/bin', PROBE_ROOT: root, PROBE_MODE: mode } };
}

async function ready(root) {
  const deadline = Date.now() + 5_000;
  while (Date.now() < deadline) {
    try {
      const value = JSON.parse(await readFile(path.join(root, 'ready.json'), 'utf8'));
      assert.equal(value.pid, value.pgid);
      if (value.child) process.kill(value.child, 0);
      return value;
    } catch (error) { if (error.code !== 'ENOENT') throw error; }
    await new Promise(resolve => setTimeout(resolve, 5));
  }
  throw new Error('cleanup oracle did not become ready');
}

function killGroup(pid) {
  if (!pid) return;
  try { process.kill(-pid, 'SIGKILL'); }
  catch (error) { if (error.code !== 'ESRCH') throw error; }
}

for (const mode of ['normal', 'leak']) {
  test(`ACP exit 0 ${mode === 'normal' ? 'with an empty group is clean' : 'with a surviving child is rejected'}`, { timeout: 10_000 }, async () => {
    const fixtureWorld = await world(mode);
    let state, fixture;
    try {
      fixture = await startStdioAcpFixture({ binary: fixtureWorld.binary, cwd: fixtureWorld.root, home: fixtureWorld.home, env: fixtureWorld.env,
        onNotification() {}, onServerRequest() {}, handleServerRequest() { throw new Error('unexpected RPC'); } });
      state = await ready(fixtureWorld.root);
      if (mode === 'normal') assert.equal(await fixture.close(), 0);
      else await assert.rejects(fixture.close(), /ACP fixture required forced process cleanup/);
      const logs = path.join(fixtureWorld.home, '.peri/logs');
      const files = (await readdir(logs)).filter(name => name.startsWith('stdio-process-'));
      assert.equal(files.length, 1);
      const outcome = JSON.parse(await readFile(path.join(logs, files[0]), 'utf8'));
      assert.equal(outcome.exitCode, 0);
      assert.equal(outcome.signal, null);
      assert.equal(outcome.forcedCleanup, mode === 'leak');
    } finally {
      await fixture?.close().catch(() => {});
      killGroup(state?.pgid);
      await rm(fixtureWorld.root, { recursive: true, force: true });
    }
  });
}

for (const mode of ['normal', 'leak', 'crash']) {
  test(`PTY cleanup reports ${mode} after the entire group is checked`, { timeout: 10_000 }, async () => {
    const fixtureWorld = await world(mode);
    const child = spawn(process.env.PERI_E2E_PYTHON ?? 'python3', [bridge], { stdio: ['pipe', 'pipe', 'pipe'] });
    const events = []; let stderr = '', state, timer;
    child.stderr.on('data', value => stderr += value);
    const lines = createInterface({ input: child.stdout });
    lines.on('line', value => events.push(JSON.parse(value)));
    const closed = new Promise(resolve => child.once('close', (code, signal) => resolve({ code, signal })));
    try {
      child.stdin.write(`${JSON.stringify({ command: [fixtureWorld.binary], cwd: fixtureWorld.root, rows: 24, cols: 80, env: fixtureWorld.env })}\n`);
      state = await ready(fixtureWorld.root);
      child.stdin.write(`${JSON.stringify(mode === 'crash' ? { type: 'crash' } : { type: 'input', data: Buffer.from('done\n').toString('base64') })}\n`);
      const exit = await Promise.race([closed, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('PTY bridge did not close')), 5_000); })]);
      assert.deepEqual(exit, { code: 0, signal: null });
      assert.equal(stderr, '');
      const reports = events.filter(event => event.type === 'exit');
      assert.equal(reports.length, 1);
      assert.equal(reports[0].code, mode === 'crash' ? -9 : 0);
      assert.equal(reports[0].forcedKill, mode !== 'normal');
    } finally {
      clearTimeout(timer);
      killGroup(state?.pgid);
      child.kill('SIGKILL');
      await closed;
      lines.close();
      await rm(fixtureWorld.root, { recursive: true, force: true });
    }
  });
}
