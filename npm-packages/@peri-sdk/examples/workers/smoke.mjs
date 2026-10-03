import { once } from 'node:events';
import { createServer, connect } from 'node:net';
import { mkdtemp, open, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';

const here = import.meta.dir;
const sdkRoot = resolve(here, '../..');
const scratch = await mkdtemp(resolve(tmpdir(), 'peri-wrangler-smoke-'));
let sqld;
let wrangler;
let model;
let log;

async function freePort() {
  const server = createServer();
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const port = server.address().port;
  await new Promise((done) => server.close(done));
  return port;
}

async function waitForPort(port, process) {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (process.exitCode !== null) throw new Error(`sqld exited: ${process.exitCode}`);
    try {
      const socket = connect(port, '127.0.0.1');
      await once(socket, 'connect');
      socket.destroy();
      return;
    } catch {
      await Bun.sleep(50);
    }
  }
  throw new Error('sqld did not listen');
}

async function probe(url) {
  const response = await fetch(url, { signal: AbortSignal.timeout(30_000) });
  const body = await response.json();
  if (!response.ok) throw new Error(`Worker returned ${response.status}: ${JSON.stringify(body)}`);
  return body;
}

try {
  const build = Bun.spawn(['bun', 'run', 'build:workers'], {
    cwd: sdkRoot, stdin: 'inherit', stdout: 'inherit', stderr: 'inherit',
  });
  if (await build.exited !== 0) throw new Error('Workers probe build failed');

  const sqlPort = await freePort();
  sqld = Bun.spawn([
    'mise', 'exec', '--', 'sqld', '--no-welcome', '--http-listen-addr',
    `127.0.0.1:${sqlPort}`, '--db-path', resolve(scratch, 'sessions'),
  ], { cwd: sdkRoot, stdout: 'ignore', stderr: 'ignore' });
  await waitForPort(sqlPort, sqld);

  let modelCalls = 0;
  const modelPaths = [];
  model = Bun.serve({ hostname: '127.0.0.1', port: 0, async fetch(request) {
    const body = await request.json();
    if (!JSON.stringify(body).includes('Reply once')) return new Response('unexpected prompt', { status: 400 });
    modelPaths.push(new URL(request.url).pathname);
    modelCalls++;
    return new Response(
      'data: {"choices":[{"index":0,"delta":{"content":"WRANGLER_REPLY"},"finish_reason":null}]}\n\n' +
      'data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\n' +
      'data: [DONE]\n\n',
      { headers: { 'content-type': 'text/event-stream' } },
    );
  } });

  const workerPort = await freePort();
  log = await open(resolve(scratch, 'wrangler.log'), 'w');
  wrangler = Bun.spawn([
    'bun', 'run', 'wrangler', 'dev', '--config', resolve(here, 'wrangler.toml'),
    '--local', '--ip', '127.0.0.1',
    '--port', String(workerPort),
    '--persist-to', resolve(scratch, 'wrangler-state'),
    '--var', `STORAGE_URL:http://127.0.0.1:${sqlPort}`,
    '--var', `MODEL_URL:http://127.0.0.1:${model.port}/v1`,
  ], { cwd: sdkRoot, stdout: log.fd, stderr: log.fd });
  const url = `http://127.0.0.1:${workerPort}`;
  let ready = false;
  for (let attempt = 0; attempt < 100; attempt++) {
    if (wrangler.exitCode !== null) throw new Error(`Wrangler exited: ${wrangler.exitCode}`);
    try {
      const response = await fetch(`${url}/cdn-cgi/local/explorer/api/local/workers`, {
        signal: AbortSignal.timeout(1_000),
      });
      if (response.ok) { ready = true; break; }
    } catch {}
    await Bun.sleep(100);
  }
  if (!ready) throw new Error('Wrangler did not start');

  const run = await probe(url);
  if (!run.sessionId || run.stopReason !== 'end_turn' || run.notifications === 0 ||
      modelCalls === 0 || modelPaths.some((path) => path !== '/v1/chat/completions')) {
    throw new Error(`ACP prompt mismatch: ${JSON.stringify({ run, modelCalls, modelPaths })}`);
  }
  const restored = await probe(`${url}/?sessionId=${encodeURIComponent(run.sessionId)}`);
  if (!restored.listed || !restored.loaded) throw new Error(`ACP restore mismatch: ${JSON.stringify(restored)}`);
  console.log(JSON.stringify({ passed: true, sessionId: run.sessionId, modelCalls, persistedAcrossRestart: true }));
} catch (error) {
  if (log) {
    const tail = (await Bun.file(resolve(scratch, 'wrangler.log')).text()).slice(-5000);
    console.error(tail);
  }
  throw error;
} finally {
  if (wrangler) { wrangler.kill('SIGINT'); await wrangler.exited; }
  if (sqld) { sqld.kill('SIGINT'); await sqld.exited; }
  model?.stop(true);
  await log?.close();
  await rm(scratch, { recursive: true, force: true });
}
