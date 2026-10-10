// Run after: PERI_WASM_PROFILE=release scripts/cargo-wasm.sh build --locked -p peri-wasm --target wasm32-unknown-emscripten
// Exercises the real ACP Host and Turso store; only the LLM endpoint is simulated.
import { once } from 'node:events';
import { createServer } from 'node:http';
import { createServer as createTcpServer, connect } from 'node:net';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { spawn } from 'node:child_process';

const output = new URL('../target/wasm32-unknown-emscripten/release/', import.meta.url);
const sdkDirectory = new URL('../npm-packages/@peri-sdk/', import.meta.url);
const root = await mkdtemp(resolve(tmpdir(), 'peri-wasm-acp-lifecycle-'));
const timeoutMs = 30_000;
const modelRequests = [];
let sqlProcess;
let model;
let acp;
let wire;
let sqlError = '';
let slowRequestArrived;
const slowRequest = new Promise((resolve) => { slowRequestArrived = resolve; });

function assert(value, message) {
  if (!value) throw new Error(message);
}

function withTimeout(promise, label, ms = timeoutMs) {
  let timer;
  return Promise.race([
    promise,
    new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${label} timed out after ${ms}ms`)), ms);
    }),
  ]).finally(() => clearTimeout(timer));
}

async function unusedPort() {
  const server = createTcpServer();
  server.listen(0, '127.0.0.1');
  await withTimeout(once(server, 'listening'), 'reserve sqld port');
  const port = server.address().port;
  await new Promise((done) => server.close(done));
  return port;
}

async function waitForPort(port, child) {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (child.exitCode !== null) throw new Error(`sqld exited early (${child.exitCode})`);
    try {
      const socket = connect(port, '127.0.0.1');
      await withTimeout(once(socket, 'connect'), 'connect sqld', 250);
      socket.destroy();
      return;
    } catch {
      await new Promise((done) => setTimeout(done, 50));
    }
  }
  throw new Error('sqld did not listen within five seconds');
}

// This is only JSON-RPC frame correlation; method behavior stays in peri-acp.
function rpc(port) {
  let nextId = 0;
  let closed = false;
  const pending = new Map();
  const notifications = [];
  const reader = (async () => {
    try {
      while (true) {
        const raw = await port.recv();
        if (raw == null) break;
        const frame = JSON.parse(raw);
        if (frame.id == null) {
          notifications.push(frame);
        } else {
          const waiter = pending.get(frame.id);
          assert(waiter, `unexpected ACP response: ${raw}`);
          pending.delete(frame.id);
          waiter.resolve(frame);
        }
      }
    } catch (error) {
      for (const waiter of pending.values()) waiter.reject(error);
      pending.clear();
      throw error;
    } finally {
      closed = true;
      for (const waiter of pending.values()) waiter.reject(new Error('ACP port closed'));
      pending.clear();
    }
  })();
  // The owning test awaits reader after close. Avoid an unhandled rejection meanwhile.
  reader.catch(() => {});
  return {
    notifications,
    reader,
    async request(method, params) {
      assert(!closed, `ACP port closed before ${method}`);
      const id = ++nextId;
      let resolveResponse;
      let rejectResponse;
      const response = new Promise((resolve, reject) => {
        resolveResponse = resolve;
        rejectResponse = reject;
      });
      pending.set(id, { resolve: resolveResponse, reject: rejectResponse });
      try {
        await withTimeout(port.send(JSON.stringify({ jsonrpc: '2.0', id, method, params })), `send ${method}`);
        return await withTimeout(response, `recv ${method}`);
      } finally {
        pending.delete(id);
      }
    },
    async notify(method, params) {
      await withTimeout(port.send(JSON.stringify({ jsonrpc: '2.0', method, params })), `notify ${method}`);
    },
  };
}

async function checkedRequest(client, method, params) {
  const frame = await client.request(method, params);
  assert(frame.result !== undefined && !frame.error, `${method} failed: ${JSON.stringify(frame)}`);
  return frame.result;
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
    try {
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks).toString());
      const serialized = JSON.stringify(body);
      const slow = serialized.includes('LIFECYCLE_SLOW_PROMPT');
      const fast = serialized.includes('LIFECYCLE_FAST_PROMPT');
      modelRequests.push({ slow, fast, url: request.url, authorization: request.headers.authorization });
      assert(slow || fast, `unexpected model input: ${serialized}`);
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      if (slow) {
        response.flushHeaders();
        slowRequestArrived();
        // Keep this turn in flight until the ACP cancellation aborts its HTTP call.
        return;
      }
      response.write('data: {"choices":[{"index":0,"delta":{"content":"LIFECYCLE_FAST_REPLY"},"finish_reason":null}]}\n\n');
      response.write('data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\n');
      response.end('data: [DONE]\n\n');
    } catch (error) {
      response.destroy(error);
    }
  });
  model.listen(0, '127.0.0.1');
  await withTimeout(once(model, 'listening'), 'listen model');

  await writeFile(new URL('package.json', output), '{"type":"module"}\n');
  const { default: Module } = await import(new URL('peri-wasm.js', output));
  const wasm = await withTimeout(Module(), 'load release peri-wasm');
  assert(wasm.PeriWasmAcp?.start, 'release artifact has no PeriWasmAcp export; rebuild it');
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
    machineId: '00000000-0000-4000-8000-000000000003',
  });

  acp = await withTimeout(wasm.PeriWasmAcp.start(config), 'start ACP Host');
  wire = rpc(acp);
  const init = await checkedRequest(wire, 'initialize', { protocolVersion: 1, clientCapabilities: {} });
  assert(init.protocolVersion === 1, `unexpected protocol: ${JSON.stringify(init)}`);
  const first = await checkedRequest(wire, 'session/new', { cwd: '/workspace', mcpServers: [] });
  const second = await checkedRequest(wire, 'session/new', { cwd: '/workspace', mcpServers: [] });
  const cancelledId = first.sessionId;
  const completedId = second.sessionId;
  assert(cancelledId && completedId && cancelledId !== completedId, 'two distinct sessions were not created');

  const interrupted = checkedRequest(wire, 'session/prompt', {
    sessionId: cancelledId, prompt: [{ type: 'text', text: 'LIFECYCLE_SLOW_PROMPT' }],
  });
  await withTimeout(slowRequest, 'slow LLM request');
  const continuing = checkedRequest(wire, 'session/prompt', {
    sessionId: completedId, prompt: [{ type: 'text', text: 'LIFECYCLE_FAST_PROMPT' }],
  });
  await wire.notify('session/cancel', { sessionId: cancelledId });
  const cancelled = await withTimeout(interrupted, 'cancelled prompt response');
  const completed = await withTimeout(continuing, 'independent prompt response');
  assert(cancelled.stopReason === 'cancelled', `cancelled prompt returned: ${JSON.stringify(cancelled)}`);
  assert(completed.stopReason === 'end_turn', `independent prompt returned: ${JSON.stringify(completed)}`);
  assert(modelRequests.length >= 2 && modelRequests.some((request) => request.slow) &&
    modelRequests.some((request) => request.fast), `model calls: ${JSON.stringify(modelRequests)}`);
  assert(modelRequests.every((request) => request.url === '/v1/chat/completions' &&
    request.authorization === 'Bearer fixture-key'), 'LLM HTTP request path or authorization differs');
  assert(wire.notifications.some((frame) => frame.method === 'session/update' &&
    JSON.stringify(frame).includes('LIFECYCLE_FAST_REPLY')), 'successful session answer was not emitted');

  const beforeClose = await checkedRequest(wire, 'session/list', { cwd: '/workspace' });
  assert(beforeClose.sessions?.some((entry) => entry.sessionId === completedId),
    `completed session missing before close: ${JSON.stringify(beforeClose)}`);
  await withTimeout(acp.close(), 'close ACP Host');
  await withTimeout(wire.reader, 'drain ACP reader');
  acp.free();
  acp = undefined;
  wire = undefined;

  acp = await withTimeout(wasm.PeriWasmAcp.start(config), 'restart ACP Host');
  wire = rpc(acp);
  await checkedRequest(wire, 'initialize', { protocolVersion: 1, clientCapabilities: {} });
  const listed = await checkedRequest(wire, 'session/list', { cwd: '/workspace' });
  assert(listed.sessions?.some((entry) => entry.sessionId === completedId),
    `completed session lost across restart: ${JSON.stringify(listed)}`);
  await checkedRequest(wire, 'session/load', { sessionId: completedId, cwd: '/workspace', mcpServers: [] });
  assert(wire.notifications.some((frame) => frame.method === 'session/update' &&
    JSON.stringify(frame).includes('LIFECYCLE_FAST_REPLY')),
  'completed answer missing from Turso history replay');
  console.log(JSON.stringify({ passed: true, sessions: [cancelledId, completedId],
    cancelledStopReason: cancelled.stopReason, completedStopReason: completed.stopReason,
    modelCalls: modelRequests.length, persistedAcrossRestart: true }));
} catch (error) {
  console.error(JSON.stringify({ passed: false, modelCalls: modelRequests.length,
    sqldExitCode: sqlProcess?.exitCode, sqldStderr: sqlError.slice(-4000) }));
  throw error;
} finally {
  if (acp) {
    await withTimeout(acp.close(), 'cleanup ACP Host').catch(() => {});
    await withTimeout(wire?.reader, 'cleanup ACP reader').catch(() => {});
    acp.free();
  }
  if (model) {
    model.closeAllConnections();
    await withTimeout(new Promise((done) => model.close(done)), 'close model', 5_000).catch(() => {});
  }
  if (sqlProcess && sqlProcess.exitCode === null) {
    sqlProcess.kill('SIGTERM');
    await Promise.race([once(sqlProcess, 'exit'), new Promise((done) => setTimeout(done, 2_000))]);
    if (sqlProcess.exitCode === null) sqlProcess.kill('SIGKILL');
  }
  await rm(root, { recursive: true, force: true });
}
