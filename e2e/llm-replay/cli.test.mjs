import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { createServer } from 'node:http';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { contextHeaders } from './proxy.mjs';

const cli = fileURLToPath(new URL('./cli.mjs', import.meta.url));
const execute = promisify(execFile);

async function launch(args) {
  const child = spawn(process.execPath, [cli, ...args], { stdio: ['ignore', 'pipe', 'pipe'] });
  let stderr = '';
  child.stderr.on('data', (bytes) => { stderr += bytes.toString(); });
  const exited = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
  const info = await new Promise((resolve, reject) => {
    let output = '';
    const timer = setTimeout(() => { child.kill('SIGKILL'); reject(new Error(`CLI startup timeout: ${stderr}`)); }, 5000);
    child.once('error', (error) => { clearTimeout(timer); reject(error); });
    child.once('exit', () => { clearTimeout(timer); reject(new Error(`CLI exited before ready: ${stderr}`)); });
    child.stdout.on('data', (bytes) => {
      output += bytes.toString();
      const end = output.indexOf('\n');
      if (end >= 0) {
        clearTimeout(timer);
        try { resolve(JSON.parse(output.slice(0, end))); } catch (error) { reject(error); }
      }
    });
  });
  return {
    info,
    async stop() {
      if (child.exitCode === null) child.kill('SIGTERM');
      let timer;
      try {
        const result = await Promise.race([exited, new Promise((_, reject) => {
          timer = setTimeout(() => { child.kill('SIGKILL'); reject(new Error(`CLI shutdown timeout: ${stderr}`)); }, 5000);
        })]);
        assert.equal(result.code, 0, stderr);
      } finally { clearTimeout(timer); }
    },
  };
}

test('CLI 可录制封存、关停上游、独立进程验证和离线重放，管理接口要求控制 token', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'peri-replay-cli-'));
  const tape = path.join(directory, 'tape');
  const responseBody = 'data: {"choices":[{"delta":{"content":"cli-result"}}]}\n\ndata: [DONE]\n\n';
  let upstreamRequests = 0;
  const upstream = createServer(async (request, response) => {
    for await (const _chunk of request) { /* drain the real request */ }
    upstreamRequests++;
    assert.equal(request.headers['x-peri-replay-context'], undefined);
    response.writeHead(200, { 'content-type': 'text/event-stream' });
    response.end(responseBody);
  });
  await new Promise((resolve) => upstream.listen(0, '127.0.0.1', resolve));
  const origin = `http://127.0.0.1:${upstream.address().port}`;
  let running;
  const stopUpstream = () => new Promise((resolve) => { upstream.closeAllConnections(); upstream.close(resolve); });
  try {
    for (const mode of ['record', 'replay']) {
      running = await launch([mode, '--cassette', tape, '--run', mode,
        ...(mode === 'record' ? ['--upstream', `https://fixture.invalid=${origin}`] : [])]);
      const { info } = running;
      assert.equal((await fetch(`${info.admin}/status`)).status, 403);
      const response = await fetch(`${info.proxy}/v1/chat/completions`, {
        method: 'POST',
        headers: { 'content-type': 'application/json', ...contextHeaders({
          run: mode, owner: 'root', execution: 'input-1', call: 'reason:1', attempt: 1, purpose: 'reason',
        }, 'https://fixture.invalid/v1/chat/completions') },
        body: JSON.stringify({ model: 'fixture', stream: true, messages: [{ role: 'user', content: 'same input' }] }),
      });
      assert.equal(response.status, 200);
      assert.equal(await response.text(), responseBody);
      const headers = { 'x-peri-replay-admin': info.adminToken };
      let summary;
      for (let attempt = 0; attempt < 100; attempt++) {
        summary = await (await fetch(`${info.admin}/status`, { headers })).json();
        if (summary.active === 0) break;
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      assert.equal(summary.active, 0);
      const completed = await fetch(`${info.admin}/complete`, { method: 'POST', headers });
      assert.equal(completed.status, 200);
      assert.equal((await completed.json()).passed, true);
      if (mode === 'record') {
        assert.equal((await fetch(`${info.admin}/seal`, { method: 'POST', headers })).status, 200);
        await stopUpstream();
      } else assert.equal(summary.upstreamCalls, 0);
      await running.stop(); running = undefined;
      const verified = await execute(process.execPath, [cli, 'verify', '--cassette', tape]);
      assert.equal(JSON.parse(verified.stdout).entries, 1);
    }
    assert.equal(upstreamRequests, 1);
  } finally {
    await running?.stop();
    await stopUpstream();
    await rm(directory, { recursive: true, force: true });
  }
});
