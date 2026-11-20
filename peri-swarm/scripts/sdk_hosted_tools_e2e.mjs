#!/usr/bin/env node
// S9a2 end-to-end driver: `environment.type=openai_hosted` with a real local
// SRT sandbox behind the Bun swarm service, driven through the official
// `openai@7.62.0` Agents API and the local deterministic model stub.
//
// Flow:
//   1. create   POST /v1/agents/sessions  environment.type=openai_hosted, no input
//      -> the gateway reserves a Bun sandbox; no SRT process may exist yet.
//   2. discover GET /v1/sandboxes/{envId} -> `reserved`, `runId:null`,
//      `<sandboxesRoot>/<sandboxId>` absent, zero matching SRT processes,
//      zero model calls (a reservation is not a turn).
//   3. events   GET (SSE) opened, then POST one user input.
//      -> the stub answers with a `Bash` tool call; Peri's workspace MCP call
//         passes through Bun, which starts SRT exactly once.
//   4. assertions: sandbox `ready` + `runId` set, one SRT process group,
//      `<workspacePath>/hello.txt` content, tool_call/tool_result items,
//      final text item, stub call accounting (helper calls excluded).
//   5. teardown: DELETE /v1/sandboxes/{envId}?deleteData=true (only this env),
//      then GET 404, no matching processes and no sandbox directory.
//
// Requirements:
//   npm install --prefix /tmp/peri-swarm-s9/sdk openai@7.62.0
//   NODE_PATH=/tmp/peri-swarm-s9/sdk/node_modules node peri-swarm/scripts/sdk_hosted_tools_e2e.mjs
//
// Environment:
//   PERI_SWARM_BASE_URL     default http://127.0.0.1:18080/v1
//   PERI_SWARM_API_KEY      gateway bearer key (required)
//   PERI_SWARM_MODEL        default stub-model
//   PERI_MODEL_STUB_URL     default http://127.0.0.1:9496
//   PERI_SWARM_BUN_URL      default http://127.0.0.1:8081
//   PERI_SWARM_BUN_TOKEN    Bun outer bearer token (required)
//   PERI_SWARM_SANDBOXES_ROOT default ~/.peri-swarm/sandboxes
//   PERI_E2E_TIMEOUT_MS     default 180000
//
// Exit code 0 only when every assertion passes. The JSON summary on stdout is
// the evidence record; no credential is ever printed.

import crypto from 'node:crypto';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import { createRequire } from 'node:module';
import { homedir } from 'node:os';
import path from 'node:path';

const require = createRequire(import.meta.url);
const OpenAI = require('openai');

const BASE_URL = process.env.PERI_SWARM_BASE_URL ?? 'http://127.0.0.1:18080/v1';
const API_KEY = process.env.PERI_SWARM_API_KEY ?? '';
const MODEL = process.env.PERI_SWARM_MODEL ?? 'stub-model';
const STUB_URL = process.env.PERI_MODEL_STUB_URL ?? 'http://127.0.0.1:9496';
const BUN_URL = (process.env.PERI_SWARM_BUN_URL ?? 'http://127.0.0.1:8081').replace(/\/$/, '');
const BUN_TOKEN = process.env.PERI_SWARM_BUN_TOKEN ?? '';
const SANDBOXES_ROOT = process.env.PERI_SWARM_SANDBOXES_ROOT ?? path.join(homedir(), '.peri-swarm', 'sandboxes');
const TIMEOUT_MS = Number(process.env.PERI_E2E_TIMEOUT_MS ?? '180000');
const CONTRACT_HEADERS = { 'x-peri-swarm-expect': '1', 'OpenAI-Beta': 'agents=v1' };
const EXPECTED_FILE_TEXT = 'hello-from-swarm';

if (!API_KEY) {
  console.error('PERI_SWARM_API_KEY is required');
  process.exit(2);
}
if (!BUN_TOKEN) {
  console.error('PERI_SWARM_BUN_TOKEN is required');
  process.exit(2);
}

const checks = [];
function check(name, condition, detail) {
  checks.push({ name, ok: Boolean(condition), ...(detail === undefined ? {} : { detail }) });
  if (!condition) throw new Error(`assertion failed: ${name}${detail ? ` (${JSON.stringify(detail)})` : ''}`);
}

function sleep(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function sdkVersion() {
  const entry = require.resolve('openai');
  const raw = fs.readFileSync(path.join(path.dirname(entry), 'package.json'), 'utf8');
  return JSON.parse(raw).version;
}

async function stubStats() {
  const response = await fetch(`${STUB_URL}/__stats`);
  if (!response.ok) throw new Error(`model stub /__stats returned ${response.status}`);
  return response.json();
}

async function bunRequest(method, pathname, body) {
  const response = await fetch(`${BUN_URL}${pathname}`, {
    method,
    headers: {
      authorization: `Bearer ${BUN_TOKEN}`,
      ...(body === undefined ? {} : { 'content-type': 'application/json' }),
    },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  const text = await response.text();
  let parsed;
  try {
    parsed = text.length === 0 ? null : JSON.parse(text);
  } catch {
    parsed = { raw: text };
  }
  return { status: response.status, body: parsed };
}

async function bunView(envId) {
  return bunRequest('GET', `/v1/sandboxes/${encodeURIComponent(envId)}`);
}

/** Full-width `ps` rows: pid, ppid, pgid, command. */
function psSnapshot() {
  const output = execFileSync('ps', ['-ax', '-ww', '-o', 'pid=,ppid=,pgid=,command='], {
    encoding: 'utf8',
  });
  return output
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
    .map((line) => {
      const match = /^(\d+)\s+(\d+)\s+(\d+)\s+(.*)$/.exec(line);
      if (!match) return null;
      return { pid: Number(match[1]), ppid: Number(match[2]), pgid: Number(match[3]), command: match[4] };
    })
    .filter(Boolean);
}

function sandboxProcesses(workspacePath, sandboxId) {
  return psSnapshot().filter(
    (row) => row.command.includes(workspacePath) || row.command.includes(sandboxId),
  );
}

function processEvidence(rows) {
  return rows.map((row) => ({
    pid: row.pid,
    ppid: row.ppid,
    pgid: row.pgid,
    command: row.command,
  }));
}

function matchingProcesses(envId, workspacePath, sandboxId) {
  const rows = sandboxProcesses(workspacePath, sandboxId);
  const groups = [...new Set(rows.map((row) => row.pgid))];
  const srtCli = rows.filter((row) => row.command.includes('@anthropic-ai/sandbox-runtime/dist/cli.js'));
  const periProvider = rows.filter(
    (row) => row.command.includes('mcp-start') && !row.command.includes('sandbox-runtime/dist/cli.js'),
  );
  const sandboxExec = rows.filter((row) => row.command.includes('/usr/bin/sandbox-exec'));
  return {
    envId,
    processCount: rows.length,
    processGroups: groups,
    processGroupCount: groups.length,
    srtCliCount: srtCli.length,
    sandboxExecCount: sandboxExec.length,
    periProviderCount: periProvider.length,
    processes: processEvidence(rows),
  };
}

/**
 * Environment of one process. `sandbox-exec` execs the target, so the provider
 * replaces the wrapper process; its inherited `HOME`/`TMPDIR` are the values
 * `macSpawnOptions()` sets for the sandbox, which is the observable proof that
 * it was launched through SRT rather than by the Bun service directly.
 */
function processSandboxEnv(pid) {
  const output = execFileSync('ps', ['eww', '-p', String(pid), '-o', 'command='], { encoding: 'utf8' });
  const home = /\bHOME=(\S+)/.exec(output)?.[1] ?? null;
  const tmpdir = /\bTMPDIR=(\S+)/.exec(output)?.[1] ?? null;
  return { home, tmpdir };
}

function rootDirectories() {
  try {
    return fs
      .readdirSync(SANDBOXES_ROOT, { withFileTypes: true })
      .filter((entry) => entry.isDirectory())
      .map((entry) => entry.name)
      .sort();
  } catch (error) {
    if (error?.code === 'ENOENT') return [];
    throw error;
  }
}

function itemLabel(item) {
  const payload = item.payload ?? {};
  if (payload.complete) return 'message:complete';
  if (payload.delta) return 'message:delta';
  return item.kind;
}

function summarizeItems(items) {
  return items.map((item) => ({
    seq: item.seq,
    itemId: item.itemId,
    kind: item.kind,
    label: itemLabel(item),
    toolCallId: item.payload?.toolCallId,
    status: item.payload?.status,
    title: item.payload?.title,
    input: item.payload?.input,
    output: item.payload?.output,
    text: typeof item.payload?.text === 'string' ? item.payload.text : undefined,
  }));
}

async function main() {
  const summary = {
    sdk: { package: 'openai', version: sdkVersion() },
    baseUrl: BASE_URL,
    bunUrl: BUN_URL,
    model: MODEL,
    stubUrl: STUB_URL,
    sandboxesRoot: SANDBOXES_ROOT,
    startedAt: new Date().toISOString(),
    timings: {},
  };

  const client = new OpenAI({ baseURL: BASE_URL, apiKey: API_KEY, maxRetries: 0, timeout: TIMEOUT_MS });

  const reset = await fetch(`${STUB_URL}/__reset`, { method: 'POST' });
  check('model stub reset', reset.ok, { status: reset.status });
  const baselineStats = await stubStats();
  const baselineDirectories = rootDirectories();
  summary.baseline = { stubMainCalls: baselineStats.mainCalls, sandboxDirectories: baselineDirectories };

  // 1a. Real Bun reservation only: lazy reservation must not start SRT.
  // The Peri session below also reserves lazily, but the ACP host issues a
  // host-only `shell_task_list` workspace tools/call while the session is being
  // assembled (peri-middlewares/src/mcp/client/workspace_watch.rs), and that
  // probe is a real tools/call. This direct probe isolates the reservation
  // contract from that host behaviour.
  const probeEnvId = `s9a2-lazy-${crypto.randomUUID()}`;
  const probeReserved = await bunRequest('POST', '/v1/sandboxes', { logicalEnvId: probeEnvId, lazy: true });
  const probeView = probeReserved.body?.sandbox;
  summary.lazyProbe = { logicalEnvId: probeEnvId, reservation: probeView };
  check('lazy reservation accepted', probeReserved.status === 200, probeReserved);
  check('lazy reservation is reserved', probeView?.state === 'reserved', probeView);
  check('lazy reservation has no runId', probeView?.runId === null, probeView);
  check('lazy reservation did not create the workspace', !fs.existsSync(probeView?.workspacePath));
  const probeProcesses = matchingProcesses(probeEnvId, probeView?.workspacePath, probeView?.sandboxId);
  summary.lazyProbe.startCount = probeProcesses.processGroupCount;
  summary.lazyProbe.process = probeProcesses;
  check('lazy reservation observed zero SRT processes', probeProcesses.processCount === 0, probeProcesses);
  const probeDeleted = await bunRequest(
    'DELETE',
    `/v1/sandboxes/${encodeURIComponent(probeEnvId)}?deleteData=true`,
  );
  summary.lazyProbe.delete = probeDeleted.body;
  check('lazy reservation probe was removed', probeDeleted.status === 200 && probeDeleted.body?.deletedData === true, probeDeleted);
  check(
    'lazy reservation probe left no sandbox directory',
    !fs.existsSync(path.join(SANDBOXES_ROOT, probeView?.sandboxId ?? '')),
  );

  // 1b. create without input: reservation only, no model turn -----------------
  const instructions =
    'S9a2 local verification agent. Use the workspace Bash tool exactly as instructed, then reply with the stub text unchanged.';
  const createStarted = Date.now();
  const created = await client.beta.agents.sessions.create(
    {
      environment: { type: 'openai_hosted' },
      agent: { model: MODEL, instructions },
    },
    { headers: CONTRACT_HEADERS },
  );
  summary.timings.createMs = Date.now() - createStarted;
  summary.session = {
    id: created.id,
    logicalId: created.logicalId,
    rootSessionId: created.rootSessionId,
    object: created.object,
    created: created.created,
    state: created.state,
    lastItemSeq: created.lastItemSeq,
    environment: created.environment,
    cwd: created.cwd,
    sandbox: created.sandbox ?? null,
  };
  check('create returned a session id', typeof created.id === 'string' && created.id.length > 0);
  check('create object is agent.session', created.object === 'agent.session', created.object);
  check('create reports created=true', created.created === true);
  check('create environment.type=openai_hosted', created.environment?.type === 'openai_hosted', created.environment);
  const envId = created.environment?.id;
  check('create returned a logical environment id', typeof envId === 'string' && envId.startsWith('env_'), envId);
  check('create carried the instructions', String(created.agent?.instructions ?? '').includes('S9a2'));
  check('create left the session idle', created.state === 'idle', created.state);
  check('create with no input persisted no turn', created.lastItemSeq === 0, created.lastItemSeq);

  // 2. discovery stage: reserved, no SRT -------------------------------------
  const stageStarted = Date.now();
  const reserved = await bunView(envId);
  summary.timings.discoveryMs = Date.now() - stageStarted;
  const reservation = reserved.body?.sandbox;
  summary.reservation = reservation;
  check('Bun sandbox lookup succeeds', reserved.status === 200, reserved);
  const workspacePath = reservation?.workspacePath;
  const sandboxId = reservation?.sandboxId;
  check('reservation returned an absolute workspacePath', typeof workspacePath === 'string' && workspacePath.startsWith('/'), workspacePath);
  check('reservation returned a sandboxId', typeof sandboxId === 'string' && /^sbx_[a-f0-9]{32}$/.test(sandboxId), sandboxId);
  check(
    'reservation mcpUrl targets the Bun proxy',
    reservation?.mcpUrl === `${BUN_URL}/sandboxes/${envId}/mcp`,
    reservation?.mcpUrl,
  );
  const creationStage = matchingProcesses(envId, workspacePath, sandboxId);
  const creationState = reservation?.state;
  summary.creationStage = {
    viewState: creationState,
    /** True when the ACP host's workspace probe already started SRT. */
    startedDuringCreate: creationState === 'ready',
    runId: reservation?.runId,
    workspaceExists: fs.existsSync(workspacePath),
    sandboxRootExists: fs.existsSync(path.join(SANDBOXES_ROOT, sandboxId)),
    modelMainCalls: (await stubStats()).mainCalls,
    startCount: creationStage.processGroupCount,
    process: creationStage,
  };
  if (creationState === 'reserved') {
    check('creation stage is reserved', true);
    check('reserved sandbox has no runId', reservation?.runId === null, reservation);
    check('workspace directory does not exist before the first tools/call', !fs.existsSync(workspacePath));
    check(
      'sandbox root does not exist before the first tools/call',
      !fs.existsSync(path.join(SANDBOXES_ROOT, sandboxId)),
    );
    check('creation stage observed zero SRT processes', creationStage.processCount === 0, creationStage);
    check('creation stage observed zero SRT process groups', creationStage.processGroupCount === 0, creationStage);
    check('reservation wrote no sandbox directory', !rootDirectories().includes(sandboxId), rootDirectories());
  } else {
    // Observed deviation: the ACP host calls the host-only `shell_task_list`
    // workspace tool while assembling the session, which is a real tools/call
    // and starts the reserved sandbox before any model turn. Record it instead
    // of pretending the reservation stayed cold.
    check('creation stage is ready after the host workspace probe', creationState === 'ready', reservation);
    check('creation-stage sandbox has a runId', typeof reservation?.runId === 'string' && reservation.runId.startsWith('run_'), reservation);
    check('exactly one SRT process group backs the creation-stage sandbox', creationStage.processGroupCount === 1, creationStage);
    check('creation-stage sandbox exists on disk', fs.existsSync(workspacePath));
    check('the creation-stage start was not model-driven', summary.creationStage.modelMainCalls === 0);
  }
  check('creation stage observed zero model main calls', summary.creationStage.modelMainCalls === 0);

  // `retrieve` must report the same environment; it never contacts Bun again.
  const fetched = await client.beta.agents.sessions.retrieve(created.id, { headers: CONTRACT_HEADERS });
  summary.retrieved = {
    id: fetched.id,
    rootSessionId: fetched.rootSessionId,
    state: fetched.state,
    lastItemSeq: fetched.lastItemSeq,
    cwd: fetched.cwd,
    environment: fetched.environment,
  };
  check('get returns the same session', fetched.id === created.id);
  check('get keeps the root session id', fetched.rootSessionId === created.rootSessionId);
  check('get keeps environment openai_hosted', fetched.environment?.type === 'openai_hosted', fetched.environment);
  check('get keeps the environment id', fetched.environment?.id === envId, fetched.environment);
  check('get reports idle before the input', fetched.state === 'idle', fetched.state);
  check('get cwd differs from the sandbox workspace', fetched.cwd !== workspacePath, {
    cwd: fetched.cwd,
    workspacePath,
  });

  const reservedAgain = await bunView(envId);
  check(
    'discovery after get reports the same sandbox state',
    reservedAgain.body?.sandbox?.state === creationState,
    { before: creationState, after: reservedAgain.body?.sandbox?.state },
  );
  summary.creationStage.secondLookupState = reservedAgain.body?.sandbox?.state;

  // 3. open SSE, submit one turn --------------------------------------------
  const stream = await client.beta.agents.sessions.events.stream(
    created.id,
    created.lastItemSeq > 0 ? { after: created.lastItemSeq } : {},
    { headers: CONTRACT_HEADERS },
  );
  const sse = { events: [], error: null };
  let resolveSse;
  const sseDone = new Promise((resolve) => {
    resolveSse = resolve;
  });
  (async () => {
    try {
      for await (const event of stream) {
        sse.events.push(event);
        if (event?.state === 'completed' || event?.state === 'failed') {
          resolveSse('terminal');
          return;
        }
      }
      resolveSse('closed');
    } catch (error) {
      sse.error = error?.message ?? String(error);
      resolveSse('error');
    }
  })();

  const inputId = crypto.randomUUID();
  const turnText =
    'S9a2 hosted sandbox turn: call the workspace Bash tool with the command the system described, then answer.';
  const postStarted = Date.now();
  const accepted = await client.post(`/agents/sessions/${created.id}/events`, {
    body: { inputId, kind: 'user', mode: 'turn', text: turnText },
    headers: CONTRACT_HEADERS,
  });
  summary.timings.eventsPostMs = Date.now() - postStarted;
  summary.accepted = accepted;
  check('events POST accepted the input', accepted?.accepted === true, accepted);
  check('events POST echoes the input id', accepted?.inputId === inputId, accepted);
  check('events POST disposition inserted', accepted?.disposition === 'inserted', accepted);

  // Poll the Bun view while the turn runs; the first `ready` is the SRT start.
  let readyAt = null;
  let readyView = null;
  const readyProbe = (async () => {
    const deadline = Date.now() + TIMEOUT_MS;
    while (Date.now() < deadline) {
      const view = await bunView(envId);
      const sandbox = view.body?.sandbox;
      if (view.status === 200 && sandbox?.state === 'ready') {
        readyAt = Date.now();
        readyView = sandbox;
        return;
      }
      if (view.status === 200 && sandbox?.state === 'failed') {
        readyView = sandbox;
        return;
      }
      await sleep(50);
    }
  })();

  const streamOutcome = await Promise.race([sseDone, sleep(TIMEOUT_MS).then(() => 'timeout')]);
  const sseFinishedAt = Date.now();
  summary.timings.sseMs = sseFinishedAt - postStarted;
  if (streamOutcome === 'timeout') {
    try {
      stream.controller.abort();
    } catch {
      // best effort
    }
  }
  check('SSE stream reached a terminal turn', streamOutcome === 'terminal', {
    outcome: streamOutcome,
    error: sse.error,
  });
  await Promise.race([readyProbe, sleep(10_000)]);
  check('sandbox reached ready', readyView?.state === 'ready', readyView);
  summary.timings.sandboxReadyMs = readyAt === null ? null : readyAt - postStarted;
  summary.ready = readyView;

  const sseItems = sse.events.filter(
    (event) => event && typeof event.itemId === 'string' && typeof event.seq === 'number',
  );
  const sseTurns = sse.events.filter((event) => event && typeof event.state === 'string');
  summary.sse = {
    eventCount: sse.events.length,
    turnStates: sseTurns.map((event) => event.state),
    itemSeqs: sseItems.map((event) => event.seq),
    items: summarizeItems(sseItems),
  };
  check(
    'SSE turn is started then completed',
    sseTurns[0]?.state === 'started' && sseTurns[sseTurns.length - 1]?.state === 'completed',
    sseTurns.map((event) => event.state),
  );

  // 4. started sandbox observations -----------------------------------------
  const afterView = await bunView(envId);
  const sandbox = afterView.body?.sandbox;
  check('sandbox is ready after the tool call', sandbox?.state === 'ready', afterView.body);
  check('sandbox exposes a runId', typeof sandbox?.runId === 'string' && sandbox.runId.startsWith('run_'), sandbox?.runId);
  check('sandbox keeps the reserved workspacePath', sandbox?.workspacePath === workspacePath);

  const startedStage = matchingProcesses(envId, workspacePath, sandboxId);
  summary.startedStage = {
    viewState: sandbox?.state,
    runId: sandbox?.runId,
    /** One SRT process group backs the one ready instance. */
    startCount: startedStage.processGroupCount,
    process: startedStage,
  };
  check('exactly one SRT process group backs the sandbox', startedStage.processGroupCount === 1, startedStage);
  check('exactly one sandbox-runtime CLI process exists', startedStage.srtCliCount === 1, startedStage);
  check('exactly one Peri workspace provider exists', startedStage.periProviderCount === 1, startedStage);
  const srtCli = startedStage.processes.find((row) => row.command.includes('sandbox-runtime/dist/cli.js'));
  const provider = startedStage.processes.find(
    (row) => row.command.includes('mcp-start') && !row.command.includes('sandbox-runtime/dist/cli.js'),
  );
  check('the provider is a child of the SRT CLI', provider?.ppid === srtCli?.pid, {
    providerPid: provider?.pid,
    providerPpid: provider?.ppid,
    srtPid: srtCli?.pid,
  });
  const providerEnv = provider === undefined ? null : processSandboxEnv(provider.pid);
  summary.provider = {
    srtCli,
    provider,
    providerEnv,
    /** SRT `sandbox-exec` execs the target, so it is not a separate live process. */
    sandboxExecProcessVisible: startedStage.sandboxExecCount,
  };
  check(
    'the provider runs with the sandbox HOME',
    providerEnv?.home === path.join(SANDBOXES_ROOT, sandboxId, 'home'),
    providerEnv,
  );
  check(
    'the provider runs with the sandbox TMPDIR',
    typeof providerEnv?.tmpdir === 'string' && providerEnv.tmpdir.startsWith('/private/tmp/peri-'),
    providerEnv,
  );

  // 5. workspace file ---------------------------------------------------------
  const filePath = path.join(workspacePath, 'hello.txt');
  const fileExists = fs.existsSync(filePath);
  check('workspace tool wrote hello.txt', fileExists, filePath);
  const rawFile = fs.readFileSync(filePath, 'utf8');
  summary.workspaceFile = {
    path: filePath,
    exists: true,
    bytes: Buffer.byteLength(rawFile),
    raw: rawFile,
    trimmed: rawFile.trim(),
    mode: (fs.statSync(filePath).mode & 0o777).toString(8),
    uid: fs.statSync(filePath).uid,
  };
  check('hello.txt content is hello-from-swarm', rawFile.trim() === EXPECTED_FILE_TEXT, rawFile);
  check(
    'the host session cwd did not receive hello.txt',
    !fs.existsSync(path.join(fetched.cwd, 'hello.txt')),
    fetched.cwd,
  );

  // 6. items: tool_call -> tool_result -> final message ---------------------
  const page = await client.beta.agents.sessions.items.list(
    created.id,
    { order: 'asc', limit: 200 },
    { headers: CONTRACT_HEADERS },
  );
  const items = page.body?.items ?? [];
  summary.items = summarizeItems(items);
  const toolCalls = items.filter(
    (item) => item.kind === 'tool_call' && JSON.stringify(item.payload?.input ?? {}).includes(EXPECTED_FILE_TEXT),
  );
  check('item log contains the Bash tool call', toolCalls.length === 1, summarizeItems(items));
  const toolCall = toolCalls[0];
  const toolCallId = toolCall.payload?.toolCallId;
  check('tool call has an id', typeof toolCallId === 'string' && toolCallId.length > 0, toolCall.payload);
  check(
    'tool call input carries the command',
    typeof toolCall.payload?.input?.command === 'string' &&
      toolCall.payload.input.command.includes(`echo ${EXPECTED_FILE_TEXT} > hello.txt`),
    toolCall.payload?.input,
  );
  check(
    'tool call command also probes a sandbox-denied host path',
    typeof toolCall.payload?.input?.command === 'string' &&
      toolCall.payload.input.command.includes('sandbox=denied'),
    toolCall.payload?.input,
  );
  check(
    'tool call title names the workspace Bash tool',
    typeof toolCall.payload?.title === 'string' && toolCall.payload.title.includes('Bash'),
    toolCall.payload?.title,
  );
  const toolResults = items.filter(
    (item) => item.kind === 'tool_result' && item.payload?.toolCallId === toolCallId,
  );
  check('item log contains the matching tool result', toolResults.length >= 1, summarizeItems(items));
  const toolResult = toolResults[toolResults.length - 1];
  check('tool result completed', toolResult.payload?.status === 'completed', toolResult.payload);
  check(
    'the tool result reports the denied host read',
    JSON.stringify(toolResult.payload?.output ?? '').includes('sandbox=denied'),
    toolResult.payload?.output,
  );
  summary.toolCall = {
    itemSeq: toolCall.seq,
    itemId: toolCall.itemId,
    toolCallId,
    title: toolCall.payload?.title,
    input: toolCall.payload?.input,
    resultItemSeq: toolResult.seq,
    resultItemId: toolResult.itemId,
    status: toolResult.payload?.status,
    output: toolResult.payload?.output,
  };

  const stats = await stubStats();
  const turns = Array.isArray(stats.turns) ? stats.turns : [];
  const toolTurn = turns.find((turn) => turn.kind === 'tool_call');
  const finalTurn = [...turns].reverse().find((turn) => turn.kind === 'final');
  check('stub emitted exactly one tool call', stats.toolCalls === 1, stats.toolCalls);
  check('stub tool turn names Bash', toolTurn?.toolName === 'Bash', toolTurn);
  check('stub tool turn uses the snapshot argument shape', toolTurn?.arguments?.includes('"command"'), toolTurn);
  check('stub final turn saw the tool result', finalTurn?.toolResultSeen === true, finalTurn);
  summary.stub = {
    mainCalls: stats.mainCalls,
    helperCalls: stats.helperCalls,
    otherCalls: stats.otherCalls,
    toolCalls: stats.toolCalls,
    turns: turns.map((turn) => ({
      n: turn.n,
      kind: turn.kind,
      toolName: turn.toolName,
      arguments: turn.arguments,
      reply: turn.reply,
      toolResultSeen: turn.toolResultSeen,
      toolNames: turn.toolNames,
    })),
  };

  const completes = items.filter((item) => item.payload?.complete === true);
  check('final text message is in the item log', completes.length >= 1, summarizeItems(items));
  const finalComplete = completes[completes.length - 1];
  check('final text equals the stub final reply', finalComplete.payload?.text === finalTurn?.reply, {
    item: finalComplete.payload?.text,
    stub: finalTurn?.reply,
  });
  check('stub main calls are exactly tool turn + final turn', stats.mainCalls === 2, stats.mainCalls);
  summary.finalText = { itemSeq: finalComplete.seq, itemId: finalComplete.itemId, text: finalComplete.payload?.text };

  // Policy evidence: the provider was started by SRT with this sandbox policy.
  const policyPath = path.join(SANDBOXES_ROOT, sandboxId, 'policy.json');
  const policy = fs.existsSync(policyPath) ? JSON.parse(fs.readFileSync(policyPath, 'utf8')) : null;
  const periPath = process.env.PERI_E2E_PERI_PATH;
  summary.policy = {
    path: policyPath,
    allowWrite: policy?.filesystem?.allowWrite ?? null,
    allowWorkspace: policy?.filesystem?.allowWrite?.includes(workspacePath) ?? false,
    allowPeriRead: periPath ? (policy?.filesystem?.allowRead?.includes(periPath) ?? false) : null,
  };
  check('sandbox policy allows writing the workspace', summary.policy.allowWorkspace === true, summary.policy);
  if (periPath) check('sandbox policy allows reading the Peri binary', summary.policy.allowPeriRead === true, summary.policy);

  // 7. teardown: only this environment --------------------------------------
  const teardownStarted = Date.now();
  const beforeDelete = await bunView(envId);
  const deleted = await bunRequest('DELETE', `/v1/sandboxes/${encodeURIComponent(envId)}?deleteData=true`);
  summary.teardown = {
    beforeDelete: beforeDelete.body?.sandbox ?? beforeDelete.body,
    deleteStatus: deleted.status,
    deleteBody: deleted.body,
    deleteMs: Date.now() - teardownStarted,
  };
  check('DELETE returned 200', deleted.status === 200, deleted);
  check('DELETE closed the instance', deleted.body?.closed === true, deleted.body);
  check('DELETE deleted this sandbox data', deleted.body?.deletedData === true, deleted.body);

  let afterDelete = null;
  const deleteDeadline = Date.now() + 30_000;
  while (Date.now() < deleteDeadline) {
    afterDelete = await bunView(envId);
    if (afterDelete.status === 404) break;
    await sleep(100);
  }
  summary.teardown.afterDeleteStatus = afterDelete?.status ?? null;
  summary.teardown.afterDeleteBody = afterDelete?.body ?? null;
  check('GET after DELETE is 404', afterDelete?.status === 404, afterDelete);
  check('sandbox directory was removed', !fs.existsSync(path.join(SANDBOXES_ROOT, sandboxId)));
  const afterTeardown = matchingProcesses(envId, workspacePath, sandboxId);
  summary.teardown.startCountAfterDelete = afterTeardown.processGroupCount;
  summary.teardown.processAfterDelete = afterTeardown;
  check('no SRT process remains after DELETE', afterTeardown.processCount === 0, afterTeardown);
  check(
    'other sandbox directories were not touched',
    JSON.stringify(rootDirectories()) === JSON.stringify(baselineDirectories),
    { before: baselineDirectories, after: rootDirectories() },
  );

  summary.checks = checks;
  summary.checkCount = checks.length;
  summary.finishedAt = new Date().toISOString();
  console.log(JSON.stringify(summary, null, 2));
}

main().catch((error) => {
  console.error(error instanceof Error ? error.stack ?? error.message : String(error));
  process.exitCode = 1;
});
