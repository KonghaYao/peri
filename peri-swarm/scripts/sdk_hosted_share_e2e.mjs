#!/usr/bin/env node
// S9a3b1 end-to-end driver: one `openai_hosted` sandbox shared by two sessions.
//
// A: create with `environment.type=openai_hosted` (no id), write `a.txt`.
// B: create with a fresh anonymous environment; `a.txt` must be absent there,
//    then write `b.txt`.
// C: create with `environment.environment_id = A.environment.id`; `cat a.txt`
//    must return A's content. A and C share `sandboxId`/`workspacePath`/`runId`
//    and the same SRT process group; B is a different sandbox.
//
// The model stub selects the Bash command from the input (`[[tool:<name>]]`
// resolved through `PERI_STUB_COMMANDS`), so every turn is a real tool call
// through the Bun workspace MCP into the SRT sandbox.
//
// The driver also runs A and C concurrently (both `/events` calls in flight,
// since each POST resolves only after its turn), aborts C's SSE subscription
// mid-turn, and then cancels a second C turn through the `kind=control` cancel
// input. Each session's items must stay its own, and A must finish normally in
// both phases while C keeps sharing the same sandbox.
//
// Requirements (same harness as S9a2/S9a3a):
//   - Peri Loom (9497) with the session and state databases,
//   - Bun swarm service (8081) with `PERI_PATH` pointing at the Peri binary,
//   - the swarm gateway (18080) with the Bun bridge and the model stub provider,
//   - `node peri-swarm/scripts/local_model_stub.mjs` (9496) started with the
//     same `PERI_STUB_COMMANDS` JSON,
//   - `npm install --prefix /tmp/peri-swarm-s9/sdk openai@7.62.0`.
//
// Environment:
//   PERI_SWARM_BASE_URL        default http://127.0.0.1:18080/v1
//   PERI_SWARM_API_KEY         gateway bearer key (required)
//   PERI_SWARM_MODEL           default stub-model
//   PERI_MODEL_STUB_URL        default http://127.0.0.1:9496
//   PERI_SWARM_BUN_URL         default http://127.0.0.1:8081
//   PERI_SWARM_BUN_TOKEN       Bun outer bearer token (required)
//   PERI_SWARM_SANDBOXES_ROOT  default ~/.peri-swarm/sandboxes
//   PERI_STUB_COMMANDS         JSON object of directive -> shell command (required)
//   PERI_E2E_OUT               optional path for the JSON evidence record
//   PERI_E2E_TIMEOUT_MS        default 180000
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
const TURN_TIMEOUT_MS = Number(process.env.PERI_E2E_TURN_TIMEOUT_MS ?? '120000');
const OUT_PATH = process.env.PERI_E2E_OUT ?? '';
const CONTRACT_HEADERS = { 'x-peri-swarm-expect': '1', 'OpenAI-Beta': 'agents=v1' };
const EXPECTED_DIRECTIVES = [
  'write-a',
  'write-b',
  'check-b',
  'read-a',
  'slow-a',
  'slow-c',
  'cancel-a',
  'cancel-c',
];
/** `cancel-c` is cancelled mid-tool, so it never reaches its final model call. */
const CANCELLED_DIRECTIVE = 'cancel-c';
const INSTRUCTIONS =
  'S9a3b1 shared-sandbox verification agent. Run the workspace Bash command for this step exactly as described, then reply with the stub text unchanged.';

if (!API_KEY) {
  console.error('PERI_SWARM_API_KEY is required');
  process.exit(2);
}
if (!BUN_TOKEN) {
  console.error('PERI_SWARM_BUN_TOKEN is required');
  process.exit(2);
}

/** `PERI_STUB_COMMANDS` mirrored to the stub: one source of truth per run. */
function parseCommands(raw) {
  if (raw === undefined || raw.trim().length === 0) {
    console.error('PERI_STUB_COMMANDS is required (JSON object of directive -> shell command)');
    process.exit(2);
  }
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    console.error(`PERI_STUB_COMMANDS is not JSON: ${error.message}`);
    process.exit(2);
  }
  if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
    console.error('PERI_STUB_COMMANDS must be a JSON object');
    process.exit(2);
  }
  for (const name of EXPECTED_DIRECTIVES) {
    if (typeof parsed[name] !== 'string' || parsed[name].trim().length === 0) {
      console.error(`PERI_STUB_COMMANDS.${name} must be a non-empty string`);
      process.exit(2);
    }
  }
  return parsed;
}

const COMMANDS = parseCommands(process.env.PERI_STUB_COMMANDS);

const checks = [];
function check(name, condition, detail) {
  checks.push({ name, ok: Boolean(condition), ...(detail === undefined ? {} : { detail }) });
  if (!condition) throw new Error(`assertion failed: ${name}${detail ? ` (${JSON.stringify(detail).slice(0, 800)})` : ''}`);
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
  const output = execFileSync('ps', ['-ax', '-ww', '-o', 'pid=,ppid=,pgid=,command='], { encoding: 'utf8' });
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
    lstart: execFileSync('ps', ['-p', String(row.pid), '-o', 'lstart='], { encoding: 'utf8' }).trim(),
    command: row.command,
  }));
}

function matchingProcesses(envId, workspacePath, sandboxId) {
  const rows = sandboxProcesses(workspacePath, sandboxId);
  const groups = [...new Set(rows.map((row) => row.pgid))];
  return {
    envId,
    processCount: rows.length,
    processGroups: groups,
    processGroupCount: groups.length,
    srtCliCount: rows.filter((row) => row.command.includes('@anthropic-ai/sandbox-runtime/dist/cli.js')).length,
    periProviderCount: rows.filter(
      (row) => row.command.includes('mcp-start') && !row.command.includes('sandbox-runtime/dist/cli.js'),
    ).length,
    processes: processEvidence(rows),
  };
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

/** Public binding fields only: no token is ever stored in bindings.json. */
function bindingsSnapshot() {
  const file = path.join(SANDBOXES_ROOT, 'bindings.json');
  if (!fs.existsSync(file)) return { path: file, entries: {} };
  return { path: file, entries: JSON.parse(fs.readFileSync(file, 'utf8')).bindings ?? {} };
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

function outputText(value) {
  return typeof value === 'string' ? value : JSON.stringify(value ?? '');
}

async function itemsOf(client, sessionId) {
  const page = await client.beta.agents.sessions.items.list(
    sessionId,
    { order: 'asc', limit: 500 },
    { headers: CONTRACT_HEADERS },
  );
  return page.body?.items ?? [];
}

async function createSession(client, environment, label, createdEnvironments) {
  const started = Date.now();
  const created = await client.beta.agents.sessions.create(
    { environment, agent: { model: MODEL, instructions: INSTRUCTIONS } },
    { headers: CONTRACT_HEADERS },
  );
  const summary = {
    label,
    requestedEnvironment: environment,
    createMs: Date.now() - started,
    id: created.id,
    logicalId: created.logicalId,
    rootSessionId: created.rootSessionId,
    state: created.state,
    lastItemSeq: created.lastItemSeq,
    cwd: created.cwd,
    environment: created.environment,
    sandbox: created.sandbox ?? null,
  };
  // Record the environment before the assertions: a failed check must still
  // leave its reservation in the teardown list.
  if (typeof created.environment?.id === 'string') {
    createdEnvironments.push({ label, envId: created.environment.id });
  }
  check(`${label}: create returned a session id`, typeof created.id === 'string' && created.id.length > 0);
  check(`${label}: create reports created=true`, created.created === true);
  check(
    `${label}: environment.type=openai_hosted`,
    created.environment?.type === 'openai_hosted',
    created.environment,
  );
  check(
    `${label}: environment id is the requested one or a fresh env_*`,
    typeof created.environment?.id === 'string' && created.environment.id.startsWith('env_'),
    created.environment,
  );
  check(`${label}: create left the session idle`, created.state === 'idle', created.state);
  check(`${label}: create with no input persisted no turn`, created.lastItemSeq === 0, created.lastItemSeq);
  return { created, summary };
}

/**
 * Open the events stream and collect events until a terminal turn state.
 * The terminal `turn` state is live-only (not persisted), so a stream opened
 * after the turn finished only replays item events.
 */
async function openStream(client, sessionId, afterSeq) {
  const stream = await client.beta.agents.sessions.events.stream(
    sessionId,
    { after: afterSeq },
    { headers: CONTRACT_HEADERS },
  );
  const state = { events: [], error: null, outcome: null, openedAt: Date.now() };
  let resolveDone;
  const done = new Promise((resolve) => {
    resolveDone = resolve;
  });
  (async () => {
    try {
      for await (const event of stream) {
        state.events.push(event);
        if (event?.state === 'completed' || event?.state === 'failed') {
          state.outcome = 'terminal';
          resolveDone('terminal');
          return;
        }
      }
      state.outcome = 'closed';
      resolveDone('closed');
    } catch (error) {
      state.error = error?.message ?? String(error);
      state.outcome = 'error';
      resolveDone('error');
    }
  })();
  return {
    state,
    done,
    abort() {
      try {
        stream.controller.abort();
      } catch {
        // best effort
      }
    },
  };
}

async function postTurn(client, sessionId, text) {
  const inputId = crypto.randomUUID();
  const started = Date.now();
  const accepted = await client.post(`/agents/sessions/${sessionId}/events`, {
    body: { inputId, kind: 'user', mode: 'turn', text },
    headers: CONTRACT_HEADERS,
  });
  check('events POST accepted the input', accepted?.accepted === true, accepted);
  check('events POST echoes the input id', accepted?.inputId === inputId, accepted);
  check('events POST disposition inserted', accepted?.disposition === 'inserted', accepted);
  return { inputId, postMs: Date.now() - started, at: started };
}

/**
 * Fire a turn without awaiting it. `/events` resolves only after the turn has
 * finished, so overlapping sessions need both requests in flight at once.
 */
function startTurn(client, sessionId, text) {
  const inputId = crypto.randomUUID();
  const startedAt = Date.now();
  const promise = client.post(`/agents/sessions/${sessionId}/events`, {
    body: { inputId, kind: 'user', mode: 'turn', text },
    headers: CONTRACT_HEADERS,
  });
  return { inputId, startedAt, promise };
}

function checkAccepted(label, accepted, inputId) {
  check(`${label}: events POST accepted the input`, accepted?.accepted === true, accepted);
  check(`${label}: events POST echoes the input id`, accepted?.inputId === inputId, accepted);
  check(`${label}: events POST disposition inserted`, accepted?.disposition === 'inserted', accepted);
}

async function lastSeq(client, sessionId) {
  return (await itemsOf(client, sessionId)).at(-1)?.seq ?? 0;
}

async function waitForToolCallItem(client, sessionId, baselineSeq, directive, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const items = await itemsOf(client, sessionId);
    const call = items.find(
      (item) =>
        item.seq > baselineSeq &&
        item.kind === 'tool_call' &&
        item.payload?.input?.command === COMMANDS[directive],
    );
    if (call !== undefined) return call;
    await sleep(100);
  }
  throw new Error(`tool call for ${directive} never appeared in the item log`);
}

function turnStates(handle) {
  return handle.state.events
    .filter((event) => event && typeof event.state === 'string')
    .map((event) => ({ state: event.state, stopReason: event.stopReason ?? null, requestId: event.requestId ?? null }));
}

function turnText(label, directive) {
  return `S9a3b1 scenario=${label} [[tool:${directive}]] Run the workspace Bash command for ${directive}, then reply with the stub text unchanged.`;
}

/**
 * Wait until this turn's tool call, its result and the final message are in the
 * item log. Turns are attributed by the exact command the stub emitted.
 *
 * The completion item normally carries the whole message. A late
 * `agent_message_chunk` can still land after it (the gateway dispatch race is
 * recorded, not fixed, by S9a3b1), so the returned `messageText` appends any
 * later deltas of the same message id and `lateDeltas` keeps that evidence.
 */
async function waitForTurn(client, sessionId, baselineSeq, directive, timeoutMs = TURN_TIMEOUT_MS) {
  const deadline = Date.now() + timeoutMs;
  let observed = null;
  while (Date.now() < deadline) {
    const items = await itemsOf(client, sessionId);
    const fresh = items.filter((item) => item.seq > baselineSeq);
    const toolCall = fresh.find(
      (item) => item.kind === 'tool_call' && item.payload?.input?.command === COMMANDS[directive],
    );
    const toolResult = toolCall === undefined
      ? undefined
      : items.find(
          (item) => item.kind === 'tool_result' && item.payload?.toolCallId === toolCall.payload?.toolCallId,
        );
    const completes = fresh.filter((item) => item.payload?.complete === true);
    observed = { fresh, toolCall, toolResult, complete: completes[completes.length - 1] };
    if (toolCall !== undefined && toolResult !== undefined && observed.complete !== undefined) {
      const messageId = observed.complete.payload?.messageId;
      // Let a trailing delta land before reading the final text.
      await sleep(300);
      const settled = await itemsOf(client, sessionId);
      const settledFresh = settled.filter((item) => item.seq > baselineSeq);
      const lateDeltas = settledFresh.filter(
        (item) =>
          item.kind === 'message' &&
          item.payload?.complete !== true &&
          item.payload?.messageId === messageId &&
          item.seq > observed.complete.seq,
      );
      const messageText = [observed.complete.payload?.text ?? '', ...lateDeltas.map((item) => item.payload?.text ?? '')].join('');
      return { ...observed, fresh: settledFresh, messageText, lateDeltas };
    }
    await sleep(100);
  }
  throw new Error(
    `turn ${directive} did not complete: ${JSON.stringify(summarizeItems(observed?.fresh ?? []))}`,
  );
}

async function main() {
  const summary = {
    sdk: { package: 'openai', version: sdkVersion() },
    baseUrl: BASE_URL,
    bunUrl: BUN_URL,
    model: MODEL,
    stubUrl: STUB_URL,
    sandboxesRoot: SANDBOXES_ROOT,
    directives: EXPECTED_DIRECTIVES,
    startedAt: new Date().toISOString(),
    timings: {},
  };

  const client = new OpenAI({ baseURL: BASE_URL, apiKey: API_KEY, maxRetries: 0, timeout: TIMEOUT_MS });
  const envs = [];
  let failure = null;

  const reset = await fetch(`${STUB_URL}/__reset`, { method: 'POST' });
  check('model stub reset', reset.ok, { status: reset.status });
  summary.baseline = {
    stub: await stubStats(),
    sandboxDirectories: rootDirectories(),
    bindings: bindingsSnapshot(),
  };
  check(
    'model stub exposes the expected directives',
    JSON.stringify([...(summary.baseline.stub.directives ?? [])].sort()) ===
      JSON.stringify([...EXPECTED_DIRECTIVES].sort()),
    summary.baseline.stub.directives,
  );

  try {
    // ---------------------------------------------------------------- A ----
    const a = await createSession(client, { type: 'openai_hosted' }, 'A', envs);
    const envA = a.created.environment.id;
    summary.sessions = { A: a.summary };

    const aReserved = await bunView(envA);
    const aReservation = aReserved.body?.sandbox;
    summary.A = {
      session: a.summary,
      reservation: aReservation,
      startCountAtCreate: matchingProcesses(envA, aReservation?.workspacePath, aReservation?.sandboxId)
        .processGroupCount,
    };
    check('A: create is a lazy reservation', aReservation?.state === 'reserved', aReservation);
    check('A: reservation has no runId', aReservation?.runId === null, aReservation);
    check(
      'A: reservation has a sandboxId',
      /^sbx_[a-f0-9]{32}$/.test(aReservation?.sandboxId ?? ''),
      aReservation?.sandboxId,
    );
    check(
      'A: reservation has an absolute workspacePath',
      typeof aReservation?.workspacePath === 'string' && aReservation.workspacePath.startsWith('/'),
      aReservation?.workspacePath,
    );
    check('A: creation started no SRT process', summary.A.startCountAtCreate === 0, summary.A.startCountAtCreate);
    check(
      'A: sandbox directory does not exist yet',
      !fs.existsSync(path.join(SANDBOXES_ROOT, aReservation.sandboxId)),
      aReservation.sandboxId,
    );

    const aStream1 = await openStream(client, a.created.id, 0);
    const aPost1 = await postTurn(client, a.created.id, turnText('A', 'write-a'));
    const aOutcome1 = await Promise.race([aStream1.done, sleep(TIMEOUT_MS).then(() => 'timeout')]);
    if (aOutcome1 === 'timeout') aStream1.abort();
    check('A: first turn reached a terminal SSE state', aOutcome1 === 'terminal', {
      outcome: aOutcome1,
      error: aStream1.state.error,
    });
    const aTurn1 = await waitForTurn(client, a.created.id, 0, 'write-a');
    const aFile = path.join(aReservation.workspacePath, 'a.txt');
    check('A: workspace a.txt exists', fs.existsSync(aFile), aFile);
    const aContent = fs.readFileSync(aFile, 'utf8');
    check('A: a.txt content is a-content', aContent === 'a-content', aContent);
    check(
      'A: tool result reports the a.txt round-trip',
      outputText(aTurn1.toolResult.payload?.output).includes('a-file=a-content'),
      aTurn1.toolResult.payload?.output,
    );
    const aView1 = await bunView(envA);
    const aSandbox = aView1.body?.sandbox;
    const aProc1 = matchingProcesses(envA, aSandbox?.workspacePath, aSandbox?.sandboxId);
    summary.A.firstTurn = {
      postMs: aPost1.postMs,
      items: summarizeItems(aTurn1.fresh),
      toolResult: aTurn1.toolResult.payload,
      finalText: aTurn1.messageText,
      lateDeltas: aTurn1.lateDeltas.length,
      sseEventCount: aStream1.state.events.length,
      sandbox: aSandbox,
      startCount: aProc1.processGroupCount,
      process: aProc1,
    };
    check('A: sandbox is ready after the first tool call', aSandbox?.state === 'ready', aSandbox);
    check('A: exactly one SRT process group', aProc1.processGroupCount === 1, aProc1);
    check('A: exactly one SRT CLI process', aProc1.srtCliCount === 1, aProc1);
    check('A: exactly one Peri workspace provider', aProc1.periProviderCount === 1, aProc1);
    check('A: sandbox exposes a runId', typeof aSandbox?.runId === 'string' && aSandbox.runId.startsWith('run_'), aSandbox?.runId);
    check(
      'A: sandbox keeps the reserved workspacePath',
      aSandbox?.workspacePath === aReservation.workspacePath,
      { reserved: aReservation.workspacePath, ready: aSandbox?.workspacePath },
    );

    // ---------------------------------------------------------------- B ----
    const b = await createSession(client, { type: 'openai_hosted' }, 'B', envs);
    const envB = b.created.environment.id;
    summary.sessions.B = b.summary;
    const bViewReserved = await bunView(envB);
    const bReservation = bViewReserved.body?.sandbox;
    summary.B = {
      session: b.summary,
      reservation: bReservation,
      startCountAtCreate: matchingProcesses(envB, bReservation?.workspacePath, bReservation?.sandboxId)
        .processGroupCount,
    };
    check('B: create is a lazy reservation', bReservation?.state === 'reserved', bReservation);
    check('B: environment id differs from A', envB !== envA, { envA, envB });
    check('B: sandboxId differs from A', bReservation?.sandboxId !== aSandbox?.sandboxId, {
      a: aSandbox?.sandboxId,
      b: bReservation?.sandboxId,
    });
    check('B: workspacePath differs from A', bReservation?.workspacePath !== aSandbox?.workspacePath, {
      a: aSandbox?.workspacePath,
      b: bReservation?.workspacePath,
    });
    check('B: creation started no SRT process', summary.B.startCountAtCreate === 0);

    const bStream1 = await openStream(client, b.created.id, 0);
    await postTurn(client, b.created.id, turnText('B', 'write-b'));
    const bOutcome1 = await Promise.race([bStream1.done, sleep(TIMEOUT_MS).then(() => 'timeout')]);
    if (bOutcome1 === 'timeout') bStream1.abort();
    check('B: first turn reached a terminal SSE state', bOutcome1 === 'terminal', {
      outcome: bOutcome1,
      error: bStream1.state.error,
    });
    const bTurn1 = await waitForTurn(client, b.created.id, 0, 'write-b');
    const bOutput = outputText(bTurn1.toolResult.payload?.output);
    check('B: a.txt is absent in the fresh sandbox', bOutput.includes('a-absent'), bOutput);
    check('B: tool result wrote b.txt', bOutput.includes('b-file=b-content'), bOutput);
    check('B: a.txt does not exist in B workspace', !fs.existsSync(path.join(bReservation.workspacePath, 'a.txt')));
    const bFile = path.join(bReservation.workspacePath, 'b.txt');
    check('B: b.txt exists in B workspace', fs.existsSync(bFile), bFile);
    check('B: b.txt content is b-content', fs.readFileSync(bFile, 'utf8') === 'b-content');
    const bView1 = await bunView(envB);
    const bSandbox = bView1.body?.sandbox;
    const bProc1 = matchingProcesses(envB, bSandbox?.workspacePath, bSandbox?.sandboxId);
    summary.B.firstTurn = {
      items: summarizeItems(bTurn1.fresh),
      toolResult: bTurn1.toolResult.payload,
      finalText: bTurn1.messageText,
      lateDeltas: bTurn1.lateDeltas.length,
      sandbox: bSandbox,
      startCount: bProc1.processGroupCount,
      process: bProc1,
    };
    check('B: sandbox is ready after the first tool call', bSandbox?.state === 'ready', bSandbox);
    check('B: exactly one SRT process group', bProc1.processGroupCount === 1, bProc1);
    check('B: A and B never report the same runId', bSandbox?.runId !== aSandbox?.runId, {
      a: aSandbox?.runId,
      b: bSandbox?.runId,
    });

    // A must not see B's file, even after B wrote it.
    const aBaseline2 = (await itemsOf(client, a.created.id)).at(-1)?.seq ?? 0;
    const aStream2 = await openStream(client, a.created.id, aBaseline2);
    await postTurn(client, a.created.id, turnText('A-check-b', 'check-b'));
    const aOutcome2 = await Promise.race([aStream2.done, sleep(TIMEOUT_MS).then(() => 'timeout')]);
    if (aOutcome2 === 'timeout') aStream2.abort();
    check('A: isolation turn reached a terminal SSE state', aOutcome2 === 'terminal', {
      outcome: aOutcome2,
      error: aStream2.state.error,
    });
    const aTurn2 = await waitForTurn(client, a.created.id, aBaseline2, 'check-b');
    const aCheckOutput = outputText(aTurn2.toolResult.payload?.output);
    check('A: A cannot see B b.txt', aCheckOutput.includes('b-absent'), aCheckOutput);
    check('A: A workspace listing has no b.txt', !aCheckOutput.includes('b.txt'), aCheckOutput);
    check(
      'A: host view of A workspace has no b.txt',
      !fs.existsSync(path.join(aSandbox.workspacePath, 'b.txt')),
      aSandbox.workspacePath,
    );
    summary.A.isolation = {
      items: summarizeItems(aTurn2.fresh),
      toolResult: aTurn2.toolResult.payload,
      finalText: aTurn2.messageText,
      lateDeltas: aTurn2.lateDeltas.length,
      hostWorkspaceEntries: fs.readdirSync(aSandbox.workspacePath).sort(),
    };

    // ---------------------------------------------------------------- C ----
    const c = await createSession(
      client,
      { type: 'openai_hosted', environment_id: envA },
      'C',
      envs,
    );
    summary.sessions.C = c.summary;
    check('C: environment.id is A environment id', c.created.environment.id === envA, {
      requested: envA,
      got: c.created.environment.id,
    });
    const cViewReserved = await bunView(envA);
    const cReservation = cViewReserved.body?.sandbox;
    const procBeforeC = matchingProcesses(envA, cReservation?.workspacePath, cReservation?.sandboxId);
    summary.C = {
      session: c.summary,
      sharedReservation: cReservation,
      processBeforeFirstCall: procBeforeC,
    };
    check('C: shared sandbox is already ready', cReservation?.state === 'ready', cReservation);
    check('C: shared sandboxId equals A', cReservation?.sandboxId === aSandbox?.sandboxId, {
      a: aSandbox?.sandboxId,
      c: cReservation?.sandboxId,
    });
    check('C: shared workspacePath equals A', cReservation?.workspacePath === aSandbox?.workspacePath, {
      a: aSandbox?.workspacePath,
      c: cReservation?.workspacePath,
    });
    check('C: shared runId equals A', cReservation?.runId === aSandbox?.runId, {
      a: aSandbox?.runId,
      c: cReservation?.runId,
    });
    check('C: no extra SRT process before its first call', procBeforeC.processGroupCount === 1, procBeforeC);

    const cStream1 = await openStream(client, c.created.id, 0);
    await postTurn(client, c.created.id, turnText('C', 'read-a'));
    const cOutcome1 = await Promise.race([cStream1.done, sleep(TIMEOUT_MS).then(() => 'timeout')]);
    if (cOutcome1 === 'timeout') cStream1.abort();
    check('C: first turn reached a terminal SSE state', cOutcome1 === 'terminal', {
      outcome: cOutcome1,
      error: cStream1.state.error,
    });
    const cTurn1 = await waitForTurn(client, c.created.id, 0, 'read-a');
    const cReadOutput = outputText(cTurn1.toolResult.payload?.output);
    check('C: cat a.txt returns a-content', cReadOutput.includes('c-read=a-content'), cReadOutput);
    const cViewAfterFirst = await bunView(envA);
    const cSandboxAfterFirst = cViewAfterFirst.body?.sandbox;
    const procAfterC = matchingProcesses(envA, cSandboxAfterFirst?.workspacePath, cSandboxAfterFirst?.sandboxId);
    summary.C.firstTurn = {
      items: summarizeItems(cTurn1.fresh),
      toolResult: cTurn1.toolResult.payload,
      finalText: cTurn1.messageText,
      lateDeltas: cTurn1.lateDeltas.length,
      sandbox: cSandboxAfterFirst,
      startCount: procAfterC.processGroupCount,
      process: procAfterC,
    };
    check('C: first call did not start another SRT process group', procAfterC.processGroupCount === 1, procAfterC);
    check('C: first call kept the runId', cSandboxAfterFirst?.runId === aSandbox?.runId, {
      before: aSandbox?.runId,
      after: cSandboxAfterFirst?.runId,
    });
    check(
      'C: the SRT process start time is unchanged',
      JSON.stringify(procAfterC.processes.map((row) => row.lstart)) ===
        JSON.stringify(procBeforeC.processes.map((row) => row.lstart)),
      { before: procBeforeC.processes, after: procAfterC.processes },
    );
    check(
      'C: SRT process ids are unchanged',
      JSON.stringify(procAfterC.processes.map((row) => row.pid)) ===
        JSON.stringify(procBeforeC.processes.map((row) => row.pid)),
      { before: procBeforeC.processes.map((row) => row.pid), after: procAfterC.processes.map((row) => row.pid) },
    );
    const sharedBindings = bindingsSnapshot();
    const bindingA = sharedBindings.entries[envA] ?? null;
    summary.C.binding = bindingA;
    check('C: binding records A runId for the shared env', bindingA?.lastRunId === aSandbox?.runId, bindingA);
    check('C: binding maps the shared env to the same sandboxId', bindingA?.sandboxId === aSandbox?.sandboxId, bindingA);

    // -------------------------------- Phase 1: A and C run concurrently ----
    const timeline = [];
    const concurrencyStart = Date.now();
    const mark = (event) =>
      timeline.push({ offsetMs: Date.now() - concurrencyStart, event, at: new Date().toISOString() });

    const aBaseline3 = await lastSeq(client, a.created.id);
    const cBaselineSeq = await lastSeq(client, c.created.id);
    const aStream3 = await openStream(client, a.created.id, aBaseline3);
    const cStream2 = await openStream(client, c.created.id, cBaselineSeq);
    mark('phase1: streams opened');
    const aReq3 = startTurn(client, a.created.id, turnText('A-concurrent', 'slow-a'));
    const cReq2 = startTurn(client, c.created.id, turnText('C-concurrent', 'slow-c'));
    mark('phase1: A and C inputs fired');
    check(
      'concurrent: both inputs were fired within 1s',
      Math.abs(cReq2.startedAt - aReq3.startedAt) < 1_000,
      { a: aReq3.startedAt, c: cReq2.startedAt },
    );
    // Abort C's subscription while its turn is in flight: the turn must finish
    // and its items must remain readable; A's stream must be unaffected.
    const abortDeadline = Date.now() + 600;
    while (Date.now() < abortDeadline && cStream2.state.events.length === 0) await sleep(20);
    cStream2.abort();
    mark('phase1: C stream aborted');
    const cAbortOutcome = await Promise.race([cStream2.done, sleep(5_000).then(() => 'no-outcome')]);
    mark(`phase1: C stream settled: ${cAbortOutcome}`);
    const aOutcome3 = await Promise.race([aStream3.done, sleep(TIMEOUT_MS).then(() => 'timeout')]);
    const aTerminalAt = Date.now();
    if (aOutcome3 === 'timeout') aStream3.abort();
    mark('phase1: A SSE terminal');
    check('concurrent: A turn reached a terminal SSE state', aOutcome3 === 'terminal', {
      outcome: aOutcome3,
      error: aStream3.state.error,
    });
    check(
      'concurrent: A terminal is a completed turn',
      turnStates(aStream3).at(-1)?.state === 'completed',
      turnStates(aStream3),
    );
    const [aAccepted3, cAccepted3] = await Promise.all([aReq3.promise, cReq2.promise]);
    mark('phase1: both turn requests returned');
    checkAccepted('concurrent A', aAccepted3, aReq3.inputId);
    checkAccepted('concurrent C', cAccepted3, cReq2.inputId);
    const aTurn3 = await waitForTurn(client, a.created.id, aBaseline3, 'slow-a');
    const cTurn2 = await waitForTurn(client, c.created.id, cBaselineSeq, 'slow-c');
    mark('phase1: C items complete');
    const cItemsAfterAbort = await itemsOf(client, c.created.id);
    const aItems = await itemsOf(client, a.created.id);
    const aAfter3 = outputText(aTurn3.toolResult.payload?.output);
    const cAfter2 = outputText(cTurn2.toolResult.payload?.output);
    check('concurrent: A tool result completed while C was running', aAfter3.includes('a-done='), aAfter3);
    check('concurrent: C turn ran through its own subscription', cAfter2.includes('c-start=a-content'), cAfter2);
    check(
      'concurrent: A item log carries no C output',
      !['c-start=', 'c-end=', 'c-read='].some((needle) => JSON.stringify(aItems).includes(needle)),
      summarizeItems(aItems.filter((item) => item.seq > aBaseline3)),
    );
    check(
      'concurrent: C item log carries no A output',
      !['a-done=', 'a-file=', 'a-tail='].some((needle) => JSON.stringify(cItemsAfterAbort).includes(needle)),
      summarizeItems(cItemsAfterAbort.filter((item) => item.seq > cBaselineSeq)),
    );
    check(
      'concurrent: A turn completed with its own final text',
      aTurn3.messageText?.startsWith('stub-echo:'),
      aTurn3.messageText,
    );
    check(
      'concurrent: C turn completed after its subscription was aborted',
      cTurn2.messageText?.startsWith('stub-echo:'),
      cTurn2.messageText,
    );
    const aFileAfterPhase1 = fs.readFileSync(path.join(aSandbox.workspacePath, 'a.txt'), 'utf8');
    check(
      'concurrent: a.txt holds both writes exactly once',
      aFileAfterPhase1 === 'a-content\na-concurrent',
      aFileAfterPhase1,
    );

    const phase1Stats = await stubStats();
    const phase1Turns = (phase1Stats.turns ?? []).filter(
      (turn) => turn.directive === 'slow-a' || turn.directive === 'slow-c',
    );
    const turnAt = (directive, kind) =>
      phase1Turns.find((turn) => turn.directive === directive && turn.kind === kind)?.at ?? null;
    const interleaving = {
      aToolAt: turnAt('slow-a', 'tool_call'),
      aFinalAt: turnAt('slow-a', 'final'),
      cToolAt: turnAt('slow-c', 'tool_call'),
      cFinalAt: turnAt('slow-c', 'final'),
    };
    check(
      'concurrent: the two sessions interleaved main calls',
      interleaving.aToolAt !== null &&
        interleaving.cToolAt !== null &&
        interleaving.aFinalAt !== null &&
        interleaving.cFinalAt !== null &&
        Date.parse(interleaving.cToolAt) < Date.parse(interleaving.aFinalAt) &&
        Date.parse(interleaving.aToolAt) < Date.parse(interleaving.cFinalAt),
      interleaving,
    );

    // Persisted items are readable again after the abort (no live turn state).
    const cLastSeen = cStream2.state.events
      .filter((event) => typeof event?.seq === 'number')
      .at(-1)?.seq ?? cBaselineSeq;
    const cReplay = await openStream(client, c.created.id, cLastSeen);
    await sleep(1_000);
    cReplay.abort();
    await Promise.race([cReplay.done, sleep(2_000)]);
    const cReplayItemSeqs = cReplay.state.events
      .filter((event) => typeof event?.seq === 'number')
      .map((event) => event.seq);
    check(
      'concurrent: C replay events are persisted items at or after the cursor',
      cReplay.state.events.every((event) => typeof event?.seq === 'number' && event.seq > cLastSeen),
      cReplay.state.events,
    );

    // --------------------------- Phase 2: cancel C, A must be unaffected ----
    const aBaseline4 = await lastSeq(client, a.created.id);
    const cBaseline3 = await lastSeq(client, c.created.id);
    const aStream4 = await openStream(client, a.created.id, aBaseline4);
    const cStream3 = await openStream(client, c.created.id, cBaseline3);
    mark('phase2: streams opened');
    const aReq4 = startTurn(client, a.created.id, turnText('A-cancel-phase', 'cancel-a'));
    const cReq4 = startTurn(client, c.created.id, turnText('C-cancel-phase', 'cancel-c'));
    mark('phase2: A and C inputs fired');
    const cToolCall4 = await waitForToolCallItem(client, c.created.id, cBaseline3, 'cancel-c');
    mark('phase2: C tool call persisted');
    const cancelResponse = await client.post(`/agents/sessions/${c.created.id}/events`, {
      body: { inputId: crypto.randomUUID(), kind: 'control', mode: 'turn', text: 'cancel' },
      headers: CONTRACT_HEADERS,
    });
    mark('phase2: C cancel returned');
    check('cancel: control input accepted', cancelResponse?.accepted === true, cancelResponse);
    check('cancel: control input inserted', cancelResponse?.disposition === 'inserted', cancelResponse);
    const aOutcome4 = await Promise.race([aStream4.done, sleep(TIMEOUT_MS).then(() => 'timeout')]);
    const aTerminalAt4 = Date.now();
    if (aOutcome4 === 'timeout') aStream4.abort();
    mark('phase2: A SSE terminal');
    const cOutcome4 = await Promise.race([cStream3.done, sleep(60_000).then(() => 'timeout')]);
    const cTerminalAt4 = Date.now();
    if (cOutcome4 === 'timeout') cStream3.abort();
    mark('phase2: C SSE terminal');
    const [aAccepted4, cAccepted4] = await Promise.all([aReq4.promise, cReq4.promise]);
    mark('phase2: both turn requests returned');
    checkAccepted('cancel A', aAccepted4, aReq4.inputId);
    checkAccepted('cancel C', cAccepted4, cReq4.inputId);
    check('cancel: A turn reached a terminal SSE state', aOutcome4 === 'terminal', {
      outcome: aOutcome4,
      error: aStream4.state.error,
    });
    check('cancel: C turn reached a terminal SSE state', cOutcome4 === 'terminal', {
      outcome: cOutcome4,
      error: cStream3.state.error,
    });
    const aStates4 = turnStates(aStream4);
    const cStates4 = turnStates(cStream3);
    const cStopReason = cStates4.at(-1)?.stopReason ?? null;
    check('cancel: C turn stopReason is cancelled', cStopReason === 'cancelled', cStates4);
    check(
      'cancel: A turn stopReason is not cancelled',
      (aStates4.at(-1)?.stopReason ?? '') !== 'cancelled',
      aStates4,
    );
    const aTurn4 = await waitForTurn(client, a.created.id, aBaseline4, 'cancel-a');
    const aAfter4 = outputText(aTurn4.toolResult.payload?.output);
    check('cancel: A tool result completed', aAfter4.includes('a-cancel-done='), aAfter4);
    const cItems4 = await itemsOf(client, c.created.id);
    const cFresh4 = cItems4.filter((item) => item.seq > cBaseline3);
    check(
      'cancel: C turn has no final message after the cancel',
      !cFresh4.some((item) => item.payload?.complete === true),
      summarizeItems(cFresh4),
    );
    check(
      'cancel: C item log carries the cancelled tool call',
      cFresh4.some((item) => item.payload?.toolCallId === cToolCall4.payload?.toolCallId),
      summarizeItems(cFresh4),
    );
    check(
      'cancel: C item log carries no A output',
      !JSON.stringify(cFresh4).includes('a-cancel-done='),
      summarizeItems(cFresh4),
    );
    const aFileAfterPhase2 = fs.readFileSync(path.join(aSandbox.workspacePath, 'a.txt'), 'utf8');
    check(
      'cancel: A workspace write landed after the C cancel',
      aFileAfterPhase2 === 'a-content\na-concurrent\na-cancel-phase',
      aFileAfterPhase2,
    );

    const phase2Stats = await stubStats();
    const phase2Turns = (phase2Stats.turns ?? []).filter(
      (turn) => turn.directive === 'cancel-a' || turn.directive === 'cancel-c',
    );
    const cancelAt = (directive, kind) =>
      phase2Turns.find((turn) => turn.directive === directive && turn.kind === kind)?.at ?? null;
    const cancelInterleaving = {
      aToolAt: cancelAt('cancel-a', 'tool_call'),
      aFinalAt: cancelAt('cancel-a', 'final'),
      cToolAt: cancelAt('cancel-c', 'tool_call'),
    };
    check(
      'cancel: A was still running when C entered its cancelled tool call',
      cancelInterleaving.aToolAt !== null &&
        cancelInterleaving.cToolAt !== null &&
        cancelInterleaving.aFinalAt !== null &&
        Date.parse(cancelInterleaving.cToolAt) < Date.parse(cancelInterleaving.aFinalAt),
      cancelInterleaving,
    );
    check(
      'cancel: the control input was accepted after C tool call item',
      (cancelResponse?.lastItemSeq ?? 0) >= cToolCall4.seq,
      { lastItemSeq: cancelResponse?.lastItemSeq, cToolCallSeq: cToolCall4.seq },
    );

    const finalAView = await bunView(envA);
    const finalBView = await bunView(envB);
    const finalProcA = matchingProcesses(
      envA,
      finalAView.body?.sandbox?.workspacePath,
      finalAView.body?.sandbox?.sandboxId,
    );
    summary.concurrency = {
      timeline,
      phase1: {
        aStartedAt: aReq3.startedAt,
        cStartedAt: cReq2.startedAt,
        aTerminalAt,
        cAbortOutcome,
        aStates: turnStates(aStream3),
        aSseEventCount: aStream3.state.events.length,
        cSseEventsBeforeAbort: cStream2.state.events.length,
        cSseErrorAfterAbort: cStream2.state.error,
        aItems: summarizeItems(aTurn3.fresh),
        cItems: summarizeItems(cTurn2.fresh),
        aToolOutput: aAfter3,
        cToolOutput: cAfter2,
        aFinalText: aTurn3.messageText,
        cFinalText: cTurn2.messageText,
        aLateDeltas: aTurn3.lateDeltas.length,
        cLateDeltas: cTurn2.lateDeltas.length,
        interleaving,
        cReplay: { after: cLastSeen, itemSeqs: cReplayItemSeqs, error: cReplay.state.error },
        aWorkspaceContent: aFileAfterPhase1,
      },
      phase2: {
        aStartedAt: aReq4.startedAt,
        cStartedAt: cReq4.startedAt,
        cancelResponse,
        cToolCallSeq: cToolCall4.seq,
        aTerminalAt: aTerminalAt4,
        cTerminalAt: cTerminalAt4,
        aStates: aStates4,
        cStates: cStates4,
        cStopReason,
        aItems: summarizeItems(aTurn4.fresh),
        cItems: summarizeItems(cFresh4),
        aToolOutput: aAfter4,
        aFinalText: aTurn4.messageText,
        aLateDeltas: aTurn4.lateDeltas.length,
        interleaving: cancelInterleaving,
        aWorkspaceContent: aFileAfterPhase2,
      },
      startCount: finalProcA.processGroupCount,
      process: finalProcA,
      runId: finalAView.body?.sandbox?.runId,
      bRunId: finalBView.body?.sandbox?.runId,
    };
    check('concurrent: one SRT process group still backs A and C', finalProcA.processGroupCount === 1, finalProcA);
    check('concurrent: the shared runId is unchanged', finalAView.body?.sandbox?.runId === aSandbox?.runId);


    // --------------------------------------------------------------- stub ----
    const stats = await stubStats();
    const turns = Array.isArray(stats.turns) ? stats.turns : [];
    const turnsByDirective = {};
    for (const turn of turns) {
      const key = `${turn.directive}:${turn.kind}`;
      turnsByDirective[key] = (turnsByDirective[key] ?? 0) + 1;
    }
    for (const directive of EXPECTED_DIRECTIVES) {
      check(`stub: one tool turn for ${directive}`, turnsByDirective[`${directive}:tool_call`] === 1, turnsByDirective);
      if (directive === CANCELLED_DIRECTIVE) {
        check(
          `stub: ${directive} has no final turn (cancelled in the tool call)`,
          turnsByDirective[`${directive}:final`] === undefined,
          turnsByDirective,
        );
      } else {
        check(`stub: one final turn for ${directive}`, turnsByDirective[`${directive}:final`] === 1, turnsByDirective);
      }
    }
    check('stub: every tool call named Bash', turns.filter((t) => t.kind === 'tool_call').every((t) => t.toolName === 'Bash'));
    check(
      'stub: no directive was rejected or ignored',
      (stats.otherRequests ?? []).every((request) => !String(request?.reason ?? '').includes('directive')),
      stats.otherRequests,
    );
    const finalTurns = turns.filter((turn) => turn.kind === 'final');
    check(
      'stub: main calls are one tool turn plus one final turn per completed turn',
      stats.mainCalls === EXPECTED_DIRECTIVES.length + finalTurns.length,
      { mainCalls: stats.mainCalls, tool: EXPECTED_DIRECTIVES.length, final: finalTurns.length },
    );
    const finalTexts = Object.fromEntries(finalTurns.map((t) => [t.directive, t.reply]));
    summary.stub = {
      mainCalls: stats.mainCalls,
      helperCalls: stats.helperCalls,
      otherCalls: stats.otherCalls,
      toolCalls: stats.toolCalls,
      turns: turns.map((turn) => ({
        n: turn.n,
        kind: turn.kind,
        directive: turn.directive,
        toolName: turn.toolName,
        arguments: turn.arguments,
        reply: turn.reply,
        toolResultSeen: turn.toolResultSeen,
        at: turn.at,
      })),
    };
    check(
      'stub: final texts match the item logs',
      finalTexts['slow-a'] === aTurn3.messageText &&
        finalTexts['slow-c'] === cTurn2.messageText &&
        finalTexts['cancel-a'] === aTurn4.messageText &&
        finalTexts['read-a'] === cTurn1.messageText &&
        finalTexts['write-a'] === aTurn1.messageText,
      {
        'slow-a': { stub: finalTexts['slow-a'], item: aTurn3.messageText },
        'slow-c': { stub: finalTexts['slow-c'], item: cTurn2.messageText },
        'cancel-a': { stub: finalTexts['cancel-a'], item: aTurn4.messageText },
        'read-a': { stub: finalTexts['read-a'], item: cTurn1.messageText },
        'write-a': { stub: finalTexts['write-a'], item: aTurn1.messageText },
      },
    );

    summary.identities = {
      A: {
        envId: envA,
        sandboxId: aSandbox?.sandboxId,
        workspacePath: aSandbox?.workspacePath,
        runId: aSandbox?.runId,
        sessionId: a.created.id,
      },
      B: {
        envId: envB,
        sandboxId: bSandbox?.sandboxId,
        workspacePath: bSandbox?.workspacePath,
        runId: bSandbox?.runId,
        sessionId: b.created.id,
      },
      C: {
        envId: c.created.environment.id,
        sandboxId: cSandboxAfterFirst?.sandboxId,
        workspacePath: cSandboxAfterFirst?.workspacePath,
        runId: cSandboxAfterFirst?.runId,
        sessionId: c.created.id,
      },
    };
    check('identities: A and C share sandboxId', summary.identities.A.sandboxId === summary.identities.C.sandboxId);
    check('identities: A and B differ in sandboxId', summary.identities.A.sandboxId !== summary.identities.B.sandboxId);
    check('identities: A and C share workspacePath', summary.identities.A.workspacePath === summary.identities.C.workspacePath);
    check('identities: A and B differ in workspacePath', summary.identities.A.workspacePath !== summary.identities.B.workspacePath);
  } catch (error) {
    failure = error instanceof Error ? error.stack ?? error.message : String(error);
  } finally {
    // teardown: only the environments this run created
    const teardown = {
      envs: [],
      rootBeforeCreate: summary.baseline.sandboxDirectories,
      bindingsBaseline: summary.baseline.bindings,
      bindingsPreTeardown: bindingsSnapshot(),
    };
    const record = (name, ok, detail) => {
      checks.push({ name, ok: Boolean(ok), ...(detail === undefined ? {} : { detail }) });
      if (!ok && failure === null) failure = `teardown assertion failed: ${name}`;
    };
    const deleted = new Set();
    for (const { label, envId } of envs) {
      if (deleted.has(envId)) continue;
      deleted.add(envId);
      try {
        const view = await bunView(envId);
        const sandboxId = view.body?.sandbox?.sandboxId ?? null;
        const workspacePath = view.body?.sandbox?.workspacePath ?? null;
        const response = await bunRequest('DELETE', `/v1/sandboxes/${encodeURIComponent(envId)}?deleteData=true`);
        let after = null;
        const deadline = Date.now() + 30_000;
        while (Date.now() < deadline) {
          after = await bunView(envId);
          if (after.status === 404) break;
          await sleep(100);
        }
        const processes = sandboxId === null ? null : matchingProcesses(envId, workspacePath, sandboxId);
        teardown.envs.push({
          label,
          envId,
          sandboxId,
          workspacePath,
          deleteStatus: response.status,
          deleteBody: response.body,
          afterDeleteStatus: after?.status ?? null,
          sandboxDirectoryExists: sandboxId !== null && fs.existsSync(path.join(SANDBOXES_ROOT, sandboxId)),
          processGroupCount: processes?.processGroupCount ?? null,
          processes: processes?.processes ?? [],
        });
      } catch (error) {
        // Keep tearing down the remaining environments; the record shows why.
        teardown.envs.push({
          label,
          envId,
          error: error instanceof Error ? error.message : String(error),
        });
        if (failure === null) failure = `teardown ${label} failed: ${error instanceof Error ? error.message : error}`;
      }
    }
    teardown.rootAfter = rootDirectories();
    teardown.bindingsAfter = bindingsSnapshot();
    teardown.untouchedBindings = Object.keys(teardown.bindingsAfter.entries).sort();
    summary.teardown = teardown;
    for (const entry of teardown.envs) {
      record(`teardown ${entry.label}: DELETE returned 200`, entry.deleteStatus === 200, entry);
      record(`teardown ${entry.label}: GET after DELETE is 404`, entry.afterDeleteStatus === 404, entry);
      record(`teardown ${entry.label}: sandbox directory removed`, entry.sandboxDirectoryExists === false, entry);
      record(`teardown ${entry.label}: no SRT process remains`, entry.processGroupCount === 0, entry);
      record(
        `teardown ${entry.label}: binding removed`,
        teardown.bindingsAfter.entries[entry.envId] === undefined,
        teardown.bindingsAfter.entries[entry.envId],
      );
    }
    record(
      'teardown: the sandbox root returns to its baseline',
      JSON.stringify(teardown.rootAfter) === JSON.stringify(summary.baseline.sandboxDirectories),
      { before: summary.baseline.sandboxDirectories, after: teardown.rootAfter },
    );
    summary.checks = checks;
    summary.checkCount = checks.length;
    summary.finishedAt = new Date().toISOString();
    const serialized = JSON.stringify(failure === null ? summary : { ...summary, failure }, null, 2);
    if (OUT_PATH) fs.writeFileSync(OUT_PATH, `${serialized}\n`);
    // The SDK keeps keep-alive sockets open; exit explicitly once the record is
    // written so a finished run cannot linger as a stray process.
    process.stdout.write(`${serialized}\n`, () => {
      process.exit(failure === null ? 0 : 1);
    });
  }
}

main().catch((error) => {
  console.error(error instanceof Error ? error.stack ?? error.message : String(error));
  process.exit(1);
});
