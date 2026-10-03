// Run after: scripts/cargo-wasm.sh build --locked -p peri-wasm --target wasm32-unknown-emscripten
// Real ACP Host + local sqld; only the OpenAI HTTP endpoint is simulated.
import { once } from 'node:events';
import { createServer } from 'node:http';
import { createServer as createTcpServer, connect } from 'node:net';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { spawn } from 'node:child_process';

const profile = process.env.PERI_WASM_PROFILE === 'release' ? 'release' : 'debug';
const output = new URL(`../target/wasm32-unknown-emscripten/${profile}/`, import.meta.url);
const sdkDirectory = new URL('../npm-packages/@peri-sdk/', import.meta.url);
const root = await mkdtemp(resolve(tmpdir(), 'peri-wasm-acp-'));
const modelRequests = [];
let sqlProcess;
let model;
let acp;
let sqlError = '';

function assert(value, message) {
  if (!value) throw new Error(message);
}

async function unusedPort() {
  const server = createTcpServer();
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const port = server.address().port;
  await new Promise((done) => server.close(done));
  return port;
}

async function waitForPort(port, child) {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (child.exitCode !== null) throw new Error(`sqld exited early (${child.exitCode})`);
    try {
      const socket = connect(port, '127.0.0.1');
      await once(socket, 'connect');
      socket.destroy();
      return;
    } catch {
      await new Promise((done) => setTimeout(done, 50));
    }
  }
  throw new Error('sqld did not listen within five seconds');
}

function withTimeout(promise, label) {
  let timer;
  return Promise.race([
    promise,
    new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${label} timed out after 30s`)), 30_000);
    }),
  ]).finally(() => clearTimeout(timer));
}

function rpc(port) {
  let nextId = 0;
  const notifications = [];
  return {
    notifications,
    async request(method, params) {
      const id = ++nextId;
      await withTimeout(port.send(JSON.stringify({ jsonrpc: '2.0', id, method, params })), `send ${method}`);
      while (true) {
        const raw = await withTimeout(port.recv(), `recv ${method}`);
        assert(raw != null, `ACP port closed during ${method}`);
        const frame = JSON.parse(raw);
        if (frame.id == null) {
          notifications.push(frame);
          continue;
        }
        assert(frame.id === id, `unexpected ACP response while awaiting ${method}: ${raw}`);
        return frame;
      }
    },
  };
}

try {
  const sqlPort = await unusedPort();
  sqlProcess = spawn('mise', [
    'exec', '--', 'sqld', '--no-welcome', '--http-listen-addr',
    `127.0.0.1:${sqlPort}`, '--db-path', resolve(root, 'sessions'),
  ], { cwd: sdkDirectory, stdio: ['ignore', 'ignore', 'pipe'] });
  sqlProcess.stderr.on('data', (chunk) => { sqlError += chunk.toString(); });
  await waitForPort(sqlPort, sqlProcess).catch((error) => {
    throw new Error(`${error.message}\n${sqlError}`);
  });

  model = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    modelRequests.push({
      url: request.url,
      authorization: request.headers.authorization,
      body: JSON.parse(Buffer.concat(chunks).toString()),
    });
    response.writeHead(200, { 'content-type': 'text/event-stream' });
    response.write('data: {"choices":[{"index":0,"delta":{"content":"WASM_ACP_REPLY"},"finish_reason":null}]}\n\n');
    response.write('data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\n');
    response.end('data: [DONE]\n\n');
  });
  model.listen(0, '127.0.0.1');
  await once(model, 'listening');

  await writeFile(new URL('package.json', output), '{"type":"module"}\n');
  const { default: Module } = await import(new URL('peri-wasm.js', output));
  const wasm = await withTimeout(Module(), 'load peri-wasm');
  assert(wasm.PeriWasmAcp?.start, 'peri-wasm artifact has no PeriWasmAcp export; rebuild it');
  const config = JSON.stringify({
    cwd: '/workspace',
    settings: {
      config: {
        active_alias: 'sonnet',
        providers: [{
          id: 'fixture', type: 'openai', apiKey: 'fixture-key',
          baseUrl: `http://127.0.0.1:${model.address().port}/v1`,
        }],
        profiles: { sonnet: { provider: 'fixture', model: 'fixture-model' } },
      },
    },
    storage: { url: `http://127.0.0.1:${sqlPort}`, authToken: 'local-dev' },
    machineId: '00000000-0000-4000-8000-000000000002',
  });

  acp = await withTimeout(wasm.PeriWasmAcp.start(config), 'start ACP Host');
  let wire = rpc(acp);
  const init = await wire.request('initialize', { protocolVersion: 1, clientCapabilities: {} });
  assert(init.result?.protocolVersion === 1, `initialize failed: ${JSON.stringify(init)}`);
  const created = await wire.request('session/new', { cwd: '/workspace', mcpServers: [] });
  const sessionId = created.result?.sessionId;
  assert(sessionId, `session/new failed: ${JSON.stringify(created)}`);
  const hidden = await wire.request('session/list', { cwd: '/workspace' });
  assert(Array.isArray(hidden.result?.sessions) && hidden.result.sessions.length === 0,
    `new empty session should be hidden: ${JSON.stringify(hidden)}`);
  const unknown = await wire.request('peri/unknown-method', {});
  assert(unknown.error?.code === -32601, `unknown method error mismatch: ${JSON.stringify(unknown)}`);
  const prompted = await wire.request('session/prompt', {
    sessionId, prompt: [{ type: 'text', text: 'Reply once' }],
  });
  assert(prompted.result?.stopReason === 'end_turn', `session/prompt failed: ${JSON.stringify(prompted)}`);
  assert(modelRequests.length === 1, `expected one real model HTTP call, got ${modelRequests.length}`);
  assert(modelRequests[0].url === '/v1/chat/completions' &&
    modelRequests[0].authorization === 'Bearer fixture-key' &&
    JSON.stringify(modelRequests[0].body).includes('Reply once'),
    `wrong model request: ${JSON.stringify(modelRequests[0])}`);
  const visible = await wire.request('session/list', { cwd: '/workspace' });
  assert(visible.result?.sessions?.some((entry) => entry.sessionId === sessionId),
    `prompted session is missing: ${JSON.stringify(visible)}`);
  assert(wire.notifications.some((frame) => frame.method === 'session/update'),
    `missing ACP session/update: ${JSON.stringify(wire.notifications)}`);
  await withTimeout(acp.close(), 'close ACP Host');
  acp.free();
  acp = undefined;

  acp = await withTimeout(wasm.PeriWasmAcp.start(config), 'restart ACP Host');
  wire = rpc(acp);
  const resumedInit = await wire.request('initialize', { protocolVersion: 1, clientCapabilities: {} });
  assert(resumedInit.result?.protocolVersion === 1, `restart initialize failed: ${JSON.stringify(resumedInit)}`);
  const listed = await wire.request('session/list', { cwd: '/workspace' });
  assert(listed.result?.sessions?.some((entry) => entry.sessionId === sessionId),
    `Turso session lost across Host restart: ${JSON.stringify(listed)}`);
  const loaded = await wire.request('session/load', { sessionId, cwd: '/workspace', mcpServers: [] });
  assert(loaded.result?.modes && Array.isArray(loaded.result.configOptions),
    `session/load failed: ${JSON.stringify(loaded)}`);
  console.log(JSON.stringify({ passed: true, sessionId, modelCalls: modelRequests.length,
    notificationCount: wire.notifications.length, persistedAcrossRestart: true }));
} catch (error) {
  console.error(JSON.stringify({ passed: false, modelCalls: modelRequests.length,
    sqldExitCode: sqlProcess?.exitCode, sqldStderr: sqlError.slice(-4000) }));
  throw error;
} finally {
  if (acp) {
    await withTimeout(acp.close(), 'cleanup ACP Host').catch(() => {});
    acp.free();
  }
  if (model) await new Promise((done) => model.close(done));
  if (sqlProcess && sqlProcess.exitCode === null) {
    sqlProcess.kill('SIGTERM');
    await Promise.race([once(sqlProcess, 'exit'), new Promise((done) => setTimeout(done, 2_000))]);
    if (sqlProcess.exitCode === null) sqlProcess.kill('SIGKILL');
  }
  await rm(root, { recursive: true, force: true });
}
