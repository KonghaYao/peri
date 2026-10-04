// Run after a release WASM build. The MCP server lives behind ACP; production
// parsing, connection management, discovery, and tool execution stay in Rust.
import { once } from 'node:events';
import { createServer } from 'node:http';
import { createServer as createTcpServer, connect } from 'node:net';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { spawn } from 'node:child_process';

const profile = process.env.PERI_WASM_PROFILE === 'debug' ? 'debug' : 'release';
const output = new URL(`../target/wasm32-unknown-emscripten/${profile}/`, import.meta.url);
const root = await mkdtemp(resolve(tmpdir(), 'peri-wasm-mcp-'));
let sqlProcess;
let model;
let acp;
let sqlError = '';
const modelRequests = [];
const mcpMethods = [];
const toolCalls = [];

function assert(value, message) {
  if (!value) throw new Error(message);
}

function withTimeout(promise, label, milliseconds = 30_000) {
  let timer;
  return Promise.race([
    promise,
    new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${label} timed out after ${milliseconds}ms`)), milliseconds);
    }),
  ]).finally(() => clearTimeout(timer));
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

async function waitUntil(label, predicate) {
  await withTimeout((async () => {
    while (!predicate()) await new Promise((done) => setTimeout(done, 20));
  })(), label);
}

function acpClient(port) {
  let nextId = 0;
  const pending = new Map();
  const notifications = [];
  const reader = (async () => {
    while (true) {
      const raw = await port.recv();
      if (raw == null) break;
      const frame = JSON.parse(raw);
      if (frame.method && frame.id != null) {
        let result;
        let error;
        const params = frame.params ?? {};
        if (frame.method === 'mcp/connect') {
          assert(params.serverId === 'fixture-server', `wrong MCP server ID: ${raw}`);
          result = { connectionId: 'fixture-connection' };
        } else if (frame.method === 'mcp/disconnect') {
          assert(params.connectionId === 'fixture-connection', `wrong MCP connection ID: ${raw}`);
          result = {};
        } else if (frame.method === 'mcp/message') {
          assert(params.connectionId === 'fixture-connection', `wrong MCP connection ID: ${raw}`);
          mcpMethods.push(params.method);
          if (params.method === 'initialize') {
            result = { protocolVersion: '2025-11-25', capabilities: { tools: {} },
              serverInfo: { name: 'wasm-acp-fixture', version: '1' } };
          } else if (params.method === 'tools/list') {
            result = { tools: [{ name: 'echo', description: 'Echo one string',
              inputSchema: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'] } }] };
          } else if (params.method === 'tools/call') {
            toolCalls.push(params.params);
            result = { content: [{ type: 'text', text: `MCP_ECHO:${params.params?.arguments?.text}` }] };
          } else {
            error = { code: -32601, message: `MCP method not found: ${params.method}` };
          }
        } else if (frame.method === 'session/request_permission') {
          const optionId = params.options?.find((option) => option.kind === 'allow_once')?.optionId
            ?? params.options?.[0]?.optionId;
          assert(optionId, `permission request has no option: ${raw}`);
          result = { outcome: { outcome: 'selected', optionId } };
        } else {
          error = { code: -32601, message: `ACP method not found: ${frame.method}` };
        }
        await port.send(JSON.stringify({ jsonrpc: '2.0', id: frame.id, ...(error ? { error } : { result }) }));
      } else if (frame.method) {
        notifications.push(frame);
        if (frame.method === 'mcp/message') mcpMethods.push(frame.params?.method);
      } else if (frame.id != null) {
        const settle = pending.get(frame.id);
        assert(settle, `unexpected ACP response: ${raw}`);
        pending.delete(frame.id);
        settle(frame);
      }
    }
    for (const settle of pending.values()) settle({ error: { message: 'ACP closed' } });
  })();
  return {
    notifications,
    reader,
    async request(method, params) {
      const id = ++nextId;
      const response = new Promise((resolveResponse) => pending.set(id, resolveResponse));
      await withTimeout(port.send(JSON.stringify({ jsonrpc: '2.0', id, method, params })), `send ${method}`);
      return withTimeout(response, `recv ${method}`);
    },
  };
}

function modelEvent(response, message, finishReason) {
  response.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: message, finish_reason: finishReason }] })}\n\n`);
  response.end('data: [DONE]\n\n');
}

try {
  const sqlPort = await unusedPort();
  sqlProcess = spawn('mise', ['exec', '--', 'sqld', '--no-welcome', '--http-listen-addr',
    `127.0.0.1:${sqlPort}`, '--db-path', resolve(root, 'sessions')],
  { cwd: new URL('../npm-packages/@peri-sdk/', import.meta.url), stdio: ['ignore', 'ignore', 'pipe'] });
  sqlProcess.stderr.on('data', (chunk) => { sqlError += chunk.toString(); });
  await waitForPort(sqlPort, sqlProcess);

  model = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString());
    modelRequests.push(body);
    response.writeHead(200, { 'content-type': 'text/event-stream' });
    const tools = body.tools?.map((tool) => tool.function?.name) ?? [];
    if (modelRequests.length === 1) {
      assert(tools.includes('SearchExtraTools') && tools.includes('ExecuteExtraTool'),
        `MCP discovery meta tools missing from model request: ${JSON.stringify(tools)}`);
      modelEvent(response, { tool_calls: [{ index: 0, id: 'search-1', type: 'function',
        function: { name: 'SearchExtraTools', arguments: JSON.stringify({ query: 'select:mcp__fixture__echo' }) } }] }, 'tool_calls');
    } else if (modelRequests.length === 2) {
      assert(JSON.stringify(body).includes('mcp__fixture__echo'),
        `deferred MCP tool was not discovered: ${JSON.stringify(body)}`);
      modelEvent(response, { tool_calls: [{ index: 0, id: 'call-1', type: 'function',
        function: { name: 'ExecuteExtraTool', arguments: JSON.stringify({
          tool_name: 'mcp__fixture__echo', params: { text: 'from-wasm' },
        }) } }] }, 'tool_calls');
    } else {
      assert(JSON.stringify(body).includes('MCP_ECHO:from-wasm'),
        `MCP tool result did not reach model: ${JSON.stringify(body)}`);
      modelEvent(response, { content: 'WASM_MCP_COMPLETE' }, 'stop');
    }
  });
  model.listen(0, '127.0.0.1');
  await once(model, 'listening');

  await writeFile(new URL('package.json', output), '{"type":"module"}\n');
  const { default: Module } = await import(new URL('peri-wasm.js', output));
  const wasm = await withTimeout(Module(), 'load peri-wasm');
  assert(wasm.PeriWasmAcp?.start, 'peri-wasm artifact has no PeriWasmAcp export; rebuild it');
  acp = await withTimeout(wasm.PeriWasmAcp.start(JSON.stringify({
    cwd: '/workspace',
    settings: { config: {
      active_alias: 'sonnet',
      providers: [{ id: 'fixture', type: 'openai', apiKey: 'fixture-key',
        baseUrl: `http://127.0.0.1:${model.address().port}/v1` }],
      profiles: { sonnet: { provider: 'fixture', model: 'fixture-model' } },
      meta_harness: { McpMiddleware: true, ToolSearch: true,
        PermissionMiddleware: false, HumanInTheLoopMiddleware: false, WorkspaceMiddleware: false },
    } },
    storage: { url: `http://127.0.0.1:${sqlPort}`, authToken: 'local-dev' },
    machineId: '00000000-0000-4000-8000-000000000003',
  })), 'start ACP Host');
  const wire = acpClient(acp);
  const initialized = await wire.request('initialize', { protocolVersion: 1, clientCapabilities: {} });
  assert(initialized.result?.agentCapabilities?.mcpCapabilities?.acp === true,
    `ACP MCP capability not advertised: ${JSON.stringify(initialized)}`);
  const created = await wire.request('session/new', { cwd: '/workspace',
    mcpServers: [{ type: 'acp', name: 'fixture', serverId: 'fixture-server' }] });
  const sessionId = created.result?.sessionId;
  assert(sessionId, `session/new failed: ${JSON.stringify(created)}`);
  await waitUntil('remote MCP tools/list', () => mcpMethods.includes('tools/list'));
  const prompted = await wire.request('session/prompt', { sessionId,
    prompt: [{ type: 'text', text: 'Use the fixture echo tool with from-wasm, then finish.' }] });
  assert(prompted.result?.stopReason === 'end_turn', `session/prompt failed: ${JSON.stringify(prompted)}`);
  assert(mcpMethods.includes('initialize') && mcpMethods.includes('tools/list'),
    `MCP handshake/discovery missing: ${JSON.stringify(mcpMethods)}`);
  assert(toolCalls.length === 1 && toolCalls[0]?.name === 'echo' &&
    toolCalls[0]?.arguments?.text === 'from-wasm',
  `MCP call did not reach fixture: ${JSON.stringify(toolCalls)}`);
  assert(modelRequests.length >= 3, `expected at least 3 model calls, got ${modelRequests.length}`);
  await withTimeout(acp.close(), 'close ACP Host');
  acp.free();
  acp = undefined;
  await withTimeout(wire.reader, 'close ACP reader');
  console.log(JSON.stringify({ passed: true, sessionId, mcpMethods, modelCalls: modelRequests.length,
    toolCalls: toolCalls.length, notificationCount: wire.notifications.length }));
} catch (error) {
  console.error(JSON.stringify({ passed: false, mcpMethods, modelCalls: modelRequests.length,
    toolCalls, sqldExitCode: sqlProcess?.exitCode, sqldStderr: sqlError.slice(-4000) }));
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
