#!/usr/bin/env node
// S9a4a end-to-end driver: idle reclaim of the Bun workspace sandbox, and the
// visible failures when the Bun bridge or the Loom store is unavailable.
//
// Modes (first CLI argument, default `idle`):
//
//   idle       full real chain: create an `openai_hosted` session, run a slow
//              Bash command (`sleep 4; echo slow > slow.txt`) through the model
//              stub, keep the proxy request in flight past `PERI_SWARM_IDLE_MS`,
//              observe that the sandbox is NOT reclaimed, then wait for the idle
//              reclaim (SRT gone, binding kept, workspace file kept), re-ensure
//              the same logical environment (new runId), read `slow.txt` back
//              through the Bun MCP proxy, and delete only this environment.
//
//   prepare    create one hosted session and run a quick tool turn so the actor
//              and sandbox are live; write `{sessionId, envId, ...}` to the
//              state file. Used before stopping the Bun bridge.
//
//   bun-down   (Bun stopped) a new `openai_hosted` session must fail with a
//              diagnosable upstream error, never fall back to a local agent; a
//              new input on the prepared session must surface a visible error.
//
//   cleanup-env (Bun restarted) delete the prepared environment with
//              `deleteData=true` and verify 404/no directory/no binding.
//
//   loom-down  (Loom stopped) create and items must fail with `502
//              upstream_unavailable`; the temporary HOME must not receive a
//              local `threads.db` (no second store).
//
// Requirements (same harness as S9a2/S9a3a/S9a3b1/S9a3b2):
//   - Peri Loom (9497) with the session and state databases,
//   - Bun swarm service (8081) with `PERI_PATH` and `PERI_SWARM_IDLE_MS`,
//   - the swarm gateway (18080) with the Bun bridge and the model stub provider,
//   - `node peri-swarm/scripts/local_model_stub.mjs` (9496) started with
//     `PERI_STUB_COMMANDS={"slow":"sleep 4; echo slow > slow.txt","quick":...}`.
//
// Environment:
//   PERI_SWARM_BASE_URL        default http://127.0.0.1:18080/v1
//   PERI_SWARM_API_KEY         gateway bearer key (required)
//   PERI_SWARM_MODEL           default stub-model
//   PERI_MODEL_STUB_URL        default http://127.0.0.1:9496
//   PERI_SWARM_BUN_URL         default http://127.0.0.1:8081
//   PERI_SWARM_BUN_TOKEN       Bun outer bearer token (idle/prepare/cleanup)
//   PERI_SWARM_SANDBOXES_ROOT  default ~/.peri-swarm/sandboxes
//   PERI_E2E_IDLE_MS           expected Bun idle threshold (default 2000)
//   PERI_E2E_STATE             state file (default /private/tmp/peri-swarm-s9a4a/state.json)
//   PERI_E2E_HOME              temporary HOME of the gateway (loom-down scan)
//   PERI_SWARM_WORKSPACE_ROOT  gateway host workspace root (local-fallback check)
//   PERI_E2E_BUN_LOG           optional Bun log for sweep/reclaim evidence
//   PERI_E2E_TIMEOUT_MS        default 180000
//   PERI_E2E_OUT               optional path for the JSON evidence record
//
// Exit code 0 only when every assertion of the selected mode passes. The JSON
// summary on stdout is the evidence record; no credential is ever printed.

import crypto from 'node:crypto';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';

const MODE = process.argv[2] ?? 'idle';
const BASE_URL = process.env.PERI_SWARM_BASE_URL ?? 'http://127.0.0.1:18080/v1';
const API_KEY = process.env.PERI_SWARM_API_KEY ?? '';
const MODEL = process.env.PERI_SWARM_MODEL ?? 'stub-model';
const STUB_URL = process.env.PERI_MODEL_STUB_URL ?? 'http://127.0.0.1:9496';
const BUN_URL = (process.env.PERI_SWARM_BUN_URL ?? 'http://127.0.0.1:8081').replace(/\/$/, '');
const BUN_TOKEN = process.env.PERI_SWARM_BUN_TOKEN ?? '';
const SANDBOXES_ROOT = process.env.PERI_SWARM_SANDBOXES_ROOT ?? path.join(homedir(), '.peri-swarm', 'sandboxes');
const IDLE_MS = Number(process.env.PERI_E2E_IDLE_MS ?? '2000');
const STATE_PATH = process.env.PERI_E2E_STATE ?? '/private/tmp/peri-swarm-s9a4a/state.json';
const HOST_HOME = process.env.PERI_E2E_HOME ?? '';
const HOST_WORKSPACE_ROOT = process.env.PERI_SWARM_WORKSPACE_ROOT ?? '';
const BUN_LOG = process.env.PERI_E2E_BUN_LOG ?? '';
const OUT_PATH = process.env.PERI_E2E_OUT ?? '';
const TIMEOUT_MS = Number(process.env.PERI_E2E_TIMEOUT_MS ?? '180000');
const CONTRACT_HEADERS = { 'x-peri-swarm-expect': '1', 'OpenAI-Beta': 'agents=v1' };

const SLOW_DIRECTIVE = 'slow';
const SLOW_COMMAND = 'sleep 4; echo slow > slow.txt';
const QUICK_DIRECTIVE = 'quick';
const QUICK_COMMAND = 'echo prepared > prepared.txt';
const INSTRUCTIONS =
  'S9a4a idle/unavailable verification agent. Run the workspace Bash command for the directive, then reply with the stub text unchanged.';

if (!API_KEY) {
  console.error('[s9a4a] PERI_SWARM_API_KEY is required');
  process.exit(2);
}
if (MODE !== 'bun-down' && MODE !== 'loom-down' && !BUN_TOKEN) {
  console.error('[s9a4a] PERI_SWARM_BUN_TOKEN is required');
  process.exit(2);
}

const checks = [];
const timeline = [];
function check(name, condition, detail) {
  const ok = Boolean(condition);
  checks.push({ name, ok, ...(detail === undefined ? {} : { detail }) });
  return ok;
}
function failedChecks() {
  return checks.filter((entry) => !entry.ok).map((entry) => entry.name);
}
function nowIso() {
  return new Date().toISOString();
}
function mark(event, detail) {
  timeline.push({ at: nowIso(), event, ...(detail === undefined ? {} : { detail }) });
  return timeline[timeline.length - 1];
}
function sleep(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}
function readJson(file) {
  return JSON.parse(fs.readFileSync(file, 'utf8'));
}
function truncate(value, limit = 600) {
  const text = typeof value === 'string' ? value : JSON.stringify(value);
  return text.length > limit ? `${text.slice(0, limit)}…` : text;
}

// ---------------------------------------------------------------- HTTP ----

async function api(method, pathname, { body, headers = {} } = {}) {
  const response = await fetch(`${BASE_URL}${pathname}`, {
    method,
    headers: {
      authorization: `Bearer ${API_KEY}`,
      ...CONTRACT_HEADERS,
      ...(body === undefined ? {} : { 'content-type': 'application/json' }),
      ...headers,
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  let parsed = null;
  if (text.length > 0) {
    try {
      parsed = JSON.parse(text);
    } catch {
      parsed = text;
    }
  }
  return { status: response.status, body: parsed };
}

function summarizeError(response) {
  const error = response?.body?.error;
  return {
    status: response?.status ?? null,
    code: error?.code ?? null,
    message: error?.message ?? null,
    details: error?.details ?? null,
  };
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
  let parsed = null;
  if (text.length > 0) {
    try {
      parsed = JSON.parse(text);
    } catch {
      parsed = text;
    }
  }
  return { status: response.status, body: parsed };
}

function bunView(envId) {
  return bunRequest('GET', `/v1/sandboxes/${encodeURIComponent(envId)}`);
}

async function createSession(environment, label) {
  const started = Date.now();
  const response = await api('POST', '/agents/sessions', {
    body: { agent: { model: MODEL, instructions: INSTRUCTIONS }, environment },
  });
  const created = response.body ?? {};
  const ok =
    response.status === 201 &&
    created.created === true &&
    typeof created.id === 'string' &&
    typeof created.environment?.id === 'string';
  check(`${label}: create returns a hosted session`, ok, {
    status: response.status,
    id: created.id ?? null,
    environment: created.environment ?? null,
    error: response.status === 201 ? null : summarizeError(response),
  });
  return { ...response, created, createMs: Date.now() - started };
}

async function stubStats() {
  const response = await fetch(`${STUB_URL}/__stats`);
  if (!response.ok) throw new Error(`model stub /__stats returned ${response.status}`);
  return response.json();
}

// ----------------------------------------------------------------- SSE ----

function parseSseFrame(raw) {
  let event = 'message';
  let id = null;
  const dataLines = [];
  for (const line of raw.split(/\r?\n/)) {
    if (line.startsWith(':')) continue;
    const separator = line.indexOf(':');
    if (separator < 0) continue;
    const field = line.slice(0, separator);
    let value = line.slice(separator + 1);
    if (value.startsWith(' ')) value = value.slice(1);
    if (field === 'event') event = value;
    else if (field === 'data') dataLines.push(value);
    else if (field === 'id') id = value;
  }
  if (dataLines.length === 0 && id === null) return null;
  const rawData = dataLines.join('\n');
  let data = null;
  if (rawData.length > 0) {
    try {
      data = JSON.parse(rawData);
    } catch {
      data = rawData;
    }
  }
  return { event, id, data, at: nowIso() };
}

function openSse(sessionId, { after } = {}) {
  const params = new URLSearchParams();
  if (after !== undefined) params.set('after', String(after));
  const query = params.toString();
  const controller = new AbortController();
  const state = {
    status: null,
    events: [],
    error: null,
    outcome: null,
    openedAt: Date.now(),
    closedAt: null,
  };
  const ready = (async () => {
    try {
      const response = await fetch(
        `${BASE_URL}/agents/sessions/${sessionId}/events${query ? `?${query}` : ''}`,
        {
          headers: {
            authorization: `Bearer ${API_KEY}`,
            ...CONTRACT_HEADERS,
            accept: 'text/event-stream',
          },
          signal: controller.signal,
        },
      );
      state.status = response.status;
      if (!response.ok) {
        state.error = `HTTP ${response.status}`;
        state.outcome = 'error';
        return 'error';
      }
      const reader = response.body.getReader();
      const decoder = new TextDecoder();
      let buffer = '';
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        let index;
        while ((index = buffer.indexOf('\n\n')) >= 0) {
          const raw = buffer.slice(0, index);
          buffer = buffer.slice(index + 2);
          const event = parseSseFrame(raw);
          if (event !== null) state.events.push(event);
        }
      }
      state.closedAt = Date.now();
      state.outcome = 'closed';
      return 'closed';
    } catch (error) {
      state.closedAt = Date.now();
      if (controller.signal.aborted) {
        state.outcome = 'aborted';
        return 'aborted';
      }
      state.error = error?.message ?? String(error);
      state.outcome = 'error';
      return 'error';
    }
  })();
  return {
    state,
    ready,
    abort() {
      controller.abort();
    },
  };
}

function turnStateOf(event) {
  return event?.data && typeof event.data === 'object' && typeof event.data.state === 'string'
    ? event.data.state
    : null;
}

async function waitForTerminal(stream, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const terminal = stream.state.events.find((event) => {
      const state = turnStateOf(event);
      return state === 'completed' || state === 'failed';
    });
    if (terminal) return terminal;
    if (stream.state.outcome === 'error' || stream.state.outcome === 'closed') break;
    await sleep(25);
  }
  return null;
}

// -------------------------------------------------------------- process ----

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
    .filter((row) => row !== null);
}

function matchingProcesses(workspacePath, sandboxId) {
  const rows = psSnapshot().filter(
    (row) => row.command.includes(workspacePath) || (sandboxId !== undefined && row.command.includes(sandboxId)),
  );
  const groups = [...new Set(rows.map((row) => row.pgid))];
  return {
    processCount: rows.length,
    processGroupCount: groups.length,
    processGroups: groups,
    srtCliCount: rows.filter((row) => row.command.includes('@anthropic-ai/sandbox-runtime/dist/cli.js')).length,
    periProviderCount: rows.filter(
      (row) => row.command.includes('mcp-start') && !row.command.includes('sandbox-runtime/dist/cli.js'),
    ).length,
    processes: rows.map((row) => ({ pid: row.pid, ppid: row.ppid, pgid: row.pgid, command: row.command })),
  };
}

// ------------------------------------------------------------- filesystem ----

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

function readBindings() {
  const file = path.join(SANDBOXES_ROOT, 'bindings.json');
  if (!fs.existsSync(file)) return { version: 1, bindings: {} };
  return readJson(file);
}

function readText(file) {
  try {
    return fs.readFileSync(file, 'utf8');
  } catch {
    return null;
  }
}

function findDatabaseFiles(root, limit = 50) {
  const found = [];
  const visit = (directory, depth) => {
    if (depth > 5 || found.length >= limit) return;
    let entries;
    try {
      entries = fs.readdirSync(directory, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      if (found.length >= limit) return;
      const full = path.join(directory, entry.name);
      if (entry.isDirectory()) visit(full, depth + 1);
      else if (/(^|\/)(threads\.db|.*\.sqlite3?|.*\.db)$/.test(full)) found.push(full);
    }
  };
  visit(root, 0);
  return found;
}

function writeState(state) {
  fs.mkdirSync(path.dirname(STATE_PATH), { recursive: true, mode: 0o700 });
  fs.writeFileSync(STATE_PATH, `${JSON.stringify(state, null, 2)}\n`, { mode: 0o600 });
}

function readState() {
  if (!fs.existsSync(STATE_PATH)) throw new Error(`state file not found: ${STATE_PATH}`);
  return readJson(STATE_PATH);
}

// ------------------------------------------------------------------ Bun log ----

function bunLogEvidence(envId) {
  if (BUN_LOG.length === 0 || !fs.existsSync(BUN_LOG)) return null;
  const lines = fs
    .readFileSync(BUN_LOG, 'utf8')
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
  const parse = (line) => {
    const start = line.indexOf('{');
    if (start < 0) return null;
    try {
      return JSON.parse(line.slice(start));
    } catch {
      return null;
    }
  };
  const records = lines.map(parse).filter((record) => record !== null);
  return {
    listening: records.find((record) => record.event === 'swarm_server_listening') ?? null,
    sweeps: records.filter(
      (record) =>
        record.event === 'swarm_idle_sweep' &&
        ((Array.isArray(record.reclaimed) && record.reclaimed.includes(envId)) ||
          (Array.isArray(record.inFlight) && record.inFlight.includes(envId))),
    ),
    reclaimed: records.filter((record) => record.event === 'swarm_sandbox_reclaimed' && record.logicalEnvId === envId),
  };
}

// ------------------------------------------------------------ idle mode ----

const SLOW_TEXT =
  `S9a4a idle reclaim [[tool:${SLOW_DIRECTIVE}]] Run the workspace Bash command for ` +
  `${SLOW_DIRECTIVE} exactly once, then reply with the stub text unchanged.`;

async function idleMode() {
  const summary = {
    mode: MODE,
    startedAt: nowIso(),
    idleMs: IDLE_MS,
    bunUrl: BUN_URL,
    stubUrl: STUB_URL,
    sandboxesRoot: SANDBOXES_ROOT,
  };
  const reset = await fetch(`${STUB_URL}/__reset`, { method: 'POST' });
  check('model stub reset', reset.ok, { status: reset.status });
  const baselineStats = await stubStats();
  const baselineDirectories = rootDirectories();
  const baselineBindings = Object.keys(readBindings().bindings ?? {}).sort();
  summary.baseline = {
    stubMainCalls: baselineStats.mainCalls,
    sandboxDirectories: baselineDirectories,
    bindings: baselineBindings,
  };

  const created = await createSession({ type: 'openai_hosted' }, 'idle');
  const session = created.created;
  const sessionId = session.id;
  const envId = session.environment?.id;
  summary.session = {
    id: sessionId,
    logicalId: session.logicalId ?? null,
    environment: session.environment ?? null,
    state: session.state ?? null,
    lastItemSeq: session.lastItemSeq ?? 0,
    cwd: session.cwd ?? null,
  };

  const reserved = await bunView(envId);
  const reservation = reserved.body?.sandbox;
  summary.reservation = reservation;
  check('Bun lookup succeeds', reserved.status === 200, reserved);
  check('session creation left the sandbox reserved', reservation?.state === 'reserved', reservation);
  check('reserved sandbox has no runId', reservation?.runId === null, reservation);
  const workspacePath = reservation?.workspacePath;
  const sandboxId = reservation?.sandboxId;
  check('reservation returns an absolute workspacePath', workspacePath?.startsWith('/') === true, workspacePath);
  check('reservation returns a sandboxId', /^sbx_[a-f0-9]{32}$/.test(sandboxId ?? ''), sandboxId);
  const slowPath = path.join(workspacePath ?? '', 'slow.txt');
  const sandboxRoot = path.join(SANDBOXES_ROOT, sandboxId ?? '');

  const stream = openSse(sessionId, { after: session.lastItemSeq ?? 0 });
  const inputId = crypto.randomUUID();
  const turnStart = Date.now();
  mark('turn_start', { envId, sessionId });
  let turnTerminal = null;
  let terminalAt = null;
  const terminalWatch = (async () => {
    turnTerminal = await waitForTerminal(stream, TIMEOUT_MS);
    terminalAt = Date.now();
  })();
  // The events POST returns after the turn finished (Rivet actor call), so it
  // runs concurrently with the probe loop: the slow command must be observed
  // while it is still executing.
  const acceptedPromise = api('POST', `/agents/sessions/${sessionId}/events`, {
    body: { kind: 'user', mode: 'turn', text: SLOW_TEXT },
    headers: { 'idempotency-key': inputId },
  });

  const probe = {
    transitions: [],
    readyAt: null,
    ready: null,
    readyProcesses: null,
    midFlight: null,
    stoppedAt: null,
    stopped: null,
    stoppedProcesses: null,
  };
  let lastState = null;
  let lastRunId = null;
  const probeLoop = (async () => {
    const deadline = Date.now() + TIMEOUT_MS;
    while (Date.now() < deadline) {
      const view = await bunView(envId);
      const sandbox = view.body?.sandbox;
      const elapsed = Date.now() - turnStart;
      if (sandbox?.state !== lastState || sandbox?.runId !== lastRunId) {
        const transition = {
          at: nowIso(),
          elapsedMs: elapsed,
          status: view.status,
          state: sandbox?.state ?? null,
          runId: sandbox?.runId ?? null,
        };
        probe.transitions.push(transition);
        lastState = sandbox?.state ?? null;
        lastRunId = sandbox?.runId ?? null;
      }
      if (sandbox?.state === 'ready') {
        if (probe.readyAt === null) {
          probe.readyAt = Date.now();
          probe.ready = sandbox;
          probe.readyProcesses = matchingProcesses(workspacePath, sandboxId);
          mark('sandbox_ready', { runId: sandbox.runId, elapsedMs: elapsed });
        }
        if (probe.midFlight === null && Date.now() - probe.readyAt >= IDLE_MS + 500) {
          probe.midFlight = {
            at: nowIso(),
            elapsedSinceReadyMs: Date.now() - probe.readyAt,
            elapsedSinceTurnStartMs: elapsed,
            view: sandbox,
            slowFileExists: fs.existsSync(slowPath),
            processes: matchingProcesses(workspacePath, sandboxId),
          };
          mark('mid_flight_probe', {
            runId: sandbox.runId,
            elapsedSinceReadyMs: probe.midFlight.elapsedSinceReadyMs,
            slowFileExists: probe.midFlight.slowFileExists,
          });
        }
      }
      if (terminalAt !== null && sandbox?.state === 'stopped' && probe.stoppedAt === null) {
        probe.stoppedAt = Date.now();
        probe.stopped = sandbox;
        probe.stoppedProcesses = matchingProcesses(workspacePath, sandboxId);
        mark('sandbox_stopped', { runId: sandbox.runId, elapsedSinceTerminalMs: probe.stoppedAt - terminalAt });
        break;
      }
      await sleep(100);
    }
  })();

  const accepted = await acceptedPromise;
  const turnPostMs = Date.now() - turnStart;
  summary.accepted = { status: accepted.status, body: accepted.body };
  check('events POST accepted the input', accepted.status === 202 && accepted.body?.accepted === true, accepted);

  await Promise.all([terminalWatch, (async () => {
    const deadline = Date.now() + TIMEOUT_MS;
    while (probe.stoppedAt === null && Date.now() < deadline) await sleep(100);
  })()]);
  stream.abort();
  const waitStopOutcome = probe.stoppedAt !== null ? 'stopped' : 'timeout';
  summary.timings = {
    turnPostMs,
    turnMs: terminalAt === null ? null : terminalAt - turnStart,
    readyAfterTurnStartMs: probe.readyAt === null ? null : probe.readyAt - turnStart,
    reclaimAfterTerminalMs: probe.stoppedAt === null || terminalAt === null ? null : probe.stoppedAt - terminalAt,
    waitStopOutcome,
  };
  summary.turn = {
    terminalState: turnStateOf(turnTerminal),
    terminalEvent: turnTerminal ?? null,
    events: stream.state.events.map((event) => ({
      at: event.at,
      event: event.event,
      id: event.id,
      state: turnStateOf(event),
    })),
  };
  summary.probe = {
    transitions: probe.transitions,
    ready: probe.ready,
    readyProcesses: probe.readyProcesses,
    midFlight: probe.midFlight,
    stopped: probe.stopped,
    stoppedProcesses: probe.stoppedProcesses,
  };

  check('turn reached a terminal state', turnTerminal !== null, {
    outcome: stream.state.outcome,
    error: stream.state.error,
    states: stream.state.events.map(turnStateOf).filter(Boolean),
  });
  check('turn completed', turnStateOf(turnTerminal) === 'completed', turnTerminal?.data ?? null);
  check('sandbox started for the turn', probe.readyAt !== null, probe.transitions);
  check('sandbox had exactly one SRT process group while ready', probe.readyProcesses?.processGroupCount === 1, probe.readyProcesses);
  check('mid-flight probe was taken past the idle threshold', probe.midFlight !== null, {
    idleMs: IDLE_MS,
    transitions: probe.transitions,
  });
  check(
    'sandbox stayed ready past the idle threshold while the Bash request was in flight',
    probe.midFlight?.view?.state === 'ready' && probe.midFlight?.processes?.processGroupCount === 1,
    probe.midFlight?.view ?? null,
  );
  check(
    'the slow command had not finished at the mid-flight probe',
    probe.midFlight?.slowFileExists === false,
    probe.midFlight?.slowFileExists,
  );
  check('sandbox was reclaimed after the idle window', probe.stoppedAt !== null, { waitStopOutcome });
  check('reclaimed view reports stopped', probe.stopped?.state === 'stopped', probe.stopped);
  check(
    'reclaim keeps the workspace path and the previous runId',
    probe.stopped?.workspacePath === workspacePath && probe.stopped?.runId === probe.ready?.runId,
    probe.stopped ?? null,
  );
  check('reclaim leaves no SRT process group', probe.stoppedProcesses?.processGroupCount === 0, probe.stoppedProcesses);

  const fileAfterReclaim = readText(slowPath);
  const bindingsAfterReclaim = readBindings();
  const bindingAfterReclaim = bindingsAfterReclaim.bindings?.[envId] ?? null;
  summary.afterReclaim = {
    view: probe.stopped ?? null,
    processes: probe.stoppedProcesses,
    workspacePath,
    sandboxRootExists: fs.existsSync(sandboxRoot),
    slowFile: {
      path: slowPath,
      exists: fs.existsSync(slowPath),
      raw: fileAfterReclaim,
      trimmed: fileAfterReclaim?.trim() ?? null,
    },
    binding: bindingAfterReclaim,
  };
  check('reclaim keeps the sandbox directory', fs.existsSync(sandboxRoot), sandboxRoot);
  check('reclaim keeps slow.txt with its content', fileAfterReclaim?.trim() === 'slow', fileAfterReclaim);
  check('reclaim keeps the binding for the same sandboxId', bindingAfterReclaim?.sandboxId === sandboxId, bindingAfterReclaim);
  check('reclaim keeps the previous runId in the binding', bindingAfterReclaim?.lastRunId === probe.ready?.runId, bindingAfterReclaim);

  const restartedAt = Date.now();
  const restarted = await bunRequest('POST', '/v1/sandboxes', { logicalEnvId: envId });
  const restartedView = restarted.body?.sandbox;
  const restartProcesses = matchingProcesses(workspacePath, sandboxId);
  summary.restart = {
    status: restarted.status,
    view: restartedView ?? restarted.body,
    elapsedMs: Date.now() - restartedAt,
    processes: restartProcesses,
  };
  check('re-ensure succeeds', restarted.status === 200, restarted);
  check('re-ensure reports ready', restartedView?.state === 'ready', restartedView);
  check('re-ensure allocates a new runId', typeof restartedView?.runId === 'string' && restartedView.runId !== probe.ready?.runId, {
    before: probe.ready?.runId ?? null,
    after: restartedView?.runId ?? null,
  });
  check('re-ensure keeps sandboxId and workspacePath', restartedView?.sandboxId === sandboxId && restartedView?.workspacePath === workspacePath, restartedView);
  check('re-ensure starts exactly one SRT process group', restartProcesses.processGroupCount === 1, restartProcesses);

  // Read the file through the real Bun MCP proxy after the restart. The host's
  // cold `initialize` was answered by Bun before the first start, so the proxy
  // forwards `tools/call` directly to the restarted provider.
  const mcpStarted = Date.now();
  const mcpResponse = await fetch(`${BUN_URL}/sandboxes/${encodeURIComponent(envId)}/mcp`, {
    method: 'POST',
    headers: {
      authorization: `Bearer ${BUN_TOKEN}`,
      'content-type': 'application/json',
      accept: 'application/json, text/event-stream',
      'mcp-protocol-version': '2026-07-28',
      'mcp-method': 'tools/call',
      'mcp-name': 'Bash',
    },
    body: JSON.stringify({
      jsonrpc: '2.0',
      id: 's9a4a-cat',
      method: 'tools/call',
      params: {
        name: 'Bash',
        arguments: { command: 'cat slow.txt' },
        _meta: {
          'io.modelcontextprotocol/protocolVersion': '2026-07-28',
          'io.modelcontextprotocol/clientCapabilities': {},
        },
      },
    }),
  });
  const mcpText = await mcpResponse.text();
  let mcpBody = null;
  try {
    mcpBody = JSON.parse(mcpText);
  } catch {
    mcpBody = null;
  }
  if (mcpBody === null) {
    const dataLines = mcpText
      .split(/\r?\n/)
      .filter((line) => line.startsWith('data:'))
      .map((line) => line.slice(5).trim());
    if (dataLines.length > 0) {
      try {
        mcpBody = JSON.parse(dataLines[dataLines.length - 1]);
      } catch {
        mcpBody = null;
      }
    }
  }
  const mcpOutput = JSON.stringify(mcpBody?.result ?? mcpBody ?? '');
  summary.mcpRead = {
    status: mcpResponse.status,
    elapsedMs: Date.now() - mcpStarted,
    body: mcpBody ?? truncate(mcpText),
    result: mcpBody?.result ?? null,
    isError: mcpBody?.error ?? null,
  };
  check('post-restart MCP tools/call succeeds', mcpResponse.status === 200 && mcpBody?.error === undefined, {
    status: mcpResponse.status,
    body: mcpBody ?? truncate(mcpText),
  });
  check('post-restart bash reads slow.txt through Bun', mcpOutput.includes('slow'), truncate(mcpOutput));

  if (BUN_LOG.length > 0) {
    const log = bunLogEvidence(envId);
    summary.bunLog = log;
    check('Bun log records the idle threshold', log?.listening?.idleMs === IDLE_MS, log?.listening ?? null);
    check(
      'Bun log shows a sweep that kept this environment for an in-flight request',
      (log?.sweeps ?? []).some((record) => Array.isArray(record.inFlight) && record.inFlight.includes(envId)),
      log?.sweeps ?? null,
    );
    check(
      'Bun log records the idle reclaim',
      (log?.reclaimed ?? []).length === 1 && log.reclaimed[0].closed === true,
      log?.reclaimed ?? null,
    );
  }

  const stats = await stubStats();
  summary.stub = {
    mainCalls: stats.mainCalls,
    toolCalls: stats.toolCalls,
    turns: (stats.turns ?? []).map((turn) => ({ n: turn.n, kind: turn.kind, toolName: turn.toolName, arguments: turn.arguments })),
  };
  check('model stub ran one tool turn and one final turn', stats.toolCalls === 1 && stats.mainCalls === 2, summary.stub);

  const deleted = await bunRequest('DELETE', `/v1/sandboxes/${encodeURIComponent(envId)}?deleteData=true`);
  let afterDelete = null;
  const deleteDeadline = Date.now() + 30_000;
  while (Date.now() < deleteDeadline) {
    afterDelete = await bunView(envId);
    if (afterDelete.status === 404) break;
    await sleep(100);
  }
  const teardownProcesses = matchingProcesses(workspacePath, sandboxId);
  const bindingsAfterDelete = readBindings();
  summary.teardown = {
    deleteStatus: deleted.status,
    deleteBody: deleted.body,
    afterDeleteStatus: afterDelete?.status ?? null,
    sandboxDirectoryExists: fs.existsSync(sandboxRoot),
    processes: teardownProcesses,
    binding: bindingsAfterDelete.bindings?.[envId] ?? null,
    otherDirectories: rootDirectories(),
  };
  check('DELETE returned 200 and removed this sandbox data', deleted.status === 200 && deleted.body?.deletedData === true, deleted);
  check('GET after DELETE is 404', afterDelete?.status === 404, afterDelete);
  check('sandbox directory was removed', !fs.existsSync(sandboxRoot), sandboxRoot);
  check('no SRT process remains after DELETE', teardownProcesses.processCount === 0, teardownProcesses);
  check('the binding was removed', bindingsAfterDelete.bindings?.[envId] === undefined, bindingsAfterDelete.bindings?.[envId]);
  check(
    'other sandbox directories were not touched',
    JSON.stringify(rootDirectories()) === JSON.stringify(baselineDirectories),
    { before: baselineDirectories, after: rootDirectories() },
  );
  return summary;
}

// --------------------------------------------------------- prepare mode ----

const QUICK_TEXT =
  `S9a4a prepare [[tool:${QUICK_DIRECTIVE}]] Run the workspace Bash command for ` +
  `${QUICK_DIRECTIVE} exactly once, then reply with the stub text unchanged.`;

async function prepareMode() {
  const summary = { mode: MODE, startedAt: nowIso(), session: null, sandbox: null, stub: null, workspaceRootBaseline: null };
  const reset = await fetch(`${STUB_URL}/__reset`, { method: 'POST' });
  check('model stub reset', reset.ok, { status: reset.status });
  const created = await createSession({ type: 'openai_hosted' }, 'prepare');
  const session = created.created;
  const envId = session.environment?.id;
  const sessionId = session.id;
  const reserved = await bunView(envId);
  const reservation = reserved.body?.sandbox;
  const workspacePath = reservation?.workspacePath;

  if (HOST_WORKSPACE_ROOT.length > 0)
    summary.workspaceRootBaseline = fs.existsSync(HOST_WORKSPACE_ROOT) ? fs.readdirSync(HOST_WORKSPACE_ROOT).sort() : [];

  const stream = openSse(sessionId, { after: session.lastItemSeq ?? 0 });
  const accepted = await api('POST', `/agents/sessions/${sessionId}/events`, {
    body: { kind: 'user', mode: 'turn', text: QUICK_TEXT },
    headers: { 'idempotency-key': crypto.randomUUID() },
  });
  check('prepare: events POST accepted', accepted.status === 202 && accepted.body?.accepted === true, accepted);
  const terminal = await waitForTerminal(stream, TIMEOUT_MS);
  stream.abort();
  check('prepare: turn completed', turnStateOf(terminal) === 'completed', terminal ?? null);

  const view = await bunView(envId);
  const sandbox = view.body?.sandbox;
  const preparedFile = path.join(workspacePath ?? '', 'prepared.txt');
  const stats = await stubStats();
  summary.session = { id: sessionId, logicalId: session.logicalId ?? null, environment: session.environment ?? null };
  summary.sandbox = { ...sandbox, preparedFile, preparedFileExists: fs.existsSync(preparedFile) };
  summary.stub = { mainCalls: stats.mainCalls, toolCalls: stats.toolCalls };
  check('prepare: sandbox is ready', sandbox?.state === 'ready', sandbox);
  check('prepare: quick command wrote prepared.txt', fs.existsSync(preparedFile), preparedFile);

  const state = {
    sessionId,
    envId,
    sandboxId: sandbox?.sandboxId,
    workspacePath: sandbox?.workspacePath,
    runId: sandbox?.runId,
    cwd: session.cwd ?? null,
    workspaceRootBaseline: summary.workspaceRootBaseline,
    preparedAt: nowIso(),
  };
  writeState(state);
  summary.statePath = STATE_PATH;
  summary.state = state;
  return summary;
}

// -------------------------------------------------------- bun-down mode ----

async function bunDownMode() {
  const state = readState();
  const summary = { mode: MODE, startedAt: nowIso(), state, createFailure: null, existingInput: null };
  const workspaceBaseline = HOST_WORKSPACE_ROOT.length > 0 && fs.existsSync(HOST_WORKSPACE_ROOT)
    ? fs.readdirSync(HOST_WORKSPACE_ROOT).sort()
    : state.workspaceRootBaseline;

  mark('bun_down_create_attempt');
  const createResponse = await api('POST', '/agents/sessions', {
    body: { agent: { model: MODEL, instructions: INSTRUCTIONS }, environment: { type: 'openai_hosted' } },
  });
  summary.createFailure = { status: createResponse.status, error: summarizeError(createResponse), body: createResponse.body };
  check('bun-down: create fails (not a local fallback)', createResponse.status >= 500, createResponse);
  check('bun-down: create error is upstream_unavailable', summarizeError(createResponse).code === 'upstream_unavailable', summarizeError(createResponse));
  check('bun-down: create error is a 502', createResponse.status === 502, createResponse.status);
  check(
    'bun-down: create error names the sandbox reservation and the unreachable bridge',
    summarizeError(createResponse).details?.operation === 'sandbox_reserve' &&
      summarizeError(createResponse).details?.reason === 'unreachable',
    summarizeError(createResponse).details,
  );

  const workspaceAfter = HOST_WORKSPACE_ROOT.length > 0 && fs.existsSync(HOST_WORKSPACE_ROOT)
    ? fs.readdirSync(HOST_WORKSPACE_ROOT).sort()
    : workspaceBaseline;
  const newWorkspaceDirectories = workspaceAfter.filter((name) => !workspaceBaseline.includes(name));
  const newWorkspaceDirectoriesEmpty = newWorkspaceDirectories.every(
    (name) => fs.readdirSync(path.join(HOST_WORKSPACE_ROOT, name)).length === 0,
  );
  summary.workspaceRoot = {
    before: workspaceBaseline,
    after: workspaceAfter,
    newDirectories: newWorkspaceDirectories,
    newDirectoriesEmpty: newWorkspaceDirectoriesEmpty,
  };
  // A failed hosted create may allocate the host `cwd` shell directory before
  // the reservation fails. What must not happen is local session state: no
  // files in that directory and no local `threads.db` in the temporary HOME.
  check(
    'bun-down: the failed create wrote no local session state',
    newWorkspaceDirectoriesEmpty,
    summary.workspaceRoot,
  );
  if (HOST_HOME.length > 0) {
    const databases = findDatabaseFiles(HOST_HOME);
    summary.hostHome = { path: HOST_HOME, databaseFiles: databases };
    check('bun-down: temporary HOME has no local threads.db', !databases.some((file) => file.endsWith('threads.db')), databases);
  }

  // Existing session: a new input must surface an error (transport or turn item).
  const stream = openSse(state.sessionId, {});
  const accepted = await api('POST', `/agents/sessions/${state.sessionId}/events`, {
    body: { kind: 'user', mode: 'turn', text: SLOW_TEXT },
    headers: { 'idempotency-key': crypto.randomUUID() },
  });
  summary.existingInput = { postStatus: accepted.status, postBody: accepted.body };
  let visibleError = false;
  let terminal = null;
  if (accepted.status === 202 && accepted.body?.accepted === true) {
    terminal = await waitForTerminal(stream, TIMEOUT_MS);
    const terminalState = turnStateOf(terminal);
    visibleError = terminalState === 'failed' || stream.state.events.some((event) => event.event === 'error');
    summary.existingInput.terminalState = terminalState;
    summary.existingInput.events = stream.state.events.map((event) => ({
      at: event.at,
      event: event.event,
      id: event.id,
      state: turnStateOf(event),
      data: event.data,
    }));
  } else {
    visibleError = accepted.status >= 500;
    summary.existingInput.error = summarizeError(accepted);
  }
  const itemsResponse = await api('GET', `/agents/sessions/${state.sessionId}/items?order=asc&limit=200`);
  summary.existingInput.itemsStatus = itemsResponse.status;
  const items = Array.isArray(itemsResponse.body?.items) ? itemsResponse.body.items : [];
  summary.existingInput.items = items.map((item) => ({
    seq: item.seq,
    kind: item.kind,
    status: item.payload?.status,
    error: item.payload?.error ?? undefined,
    output: typeof item.payload?.output === 'string' ? truncate(item.payload.output, 300) : undefined,
    text: typeof item.payload?.text === 'string' ? truncate(item.payload.text, 300) : undefined,
  }));
  const itemError = items.some(
    (item) =>
      (item.kind === 'tool_result' && item.payload?.status !== 'completed') ||
      (item.kind === 'error') ||
      (typeof item.payload?.error === 'string' && item.payload.error.length > 0),
  );
  summary.existingInput.itemError = itemError;
  check('bun-down: the existing session surfaces a visible error', visibleError || itemError, {
    terminalState: summary.existingInput.terminalState ?? null,
    postStatus: accepted.status,
    itemError,
  });
  stream.abort();
  return summary;
}

// ----------------------------------------------------- cleanup-env mode ----

async function cleanupEnvMode() {
  const state = readState();
  const summary = { mode: MODE, startedAt: nowIso(), state, before: null, delete: null, after: null };
  const before = await bunView(state.envId);
  summary.before = { status: before.status, body: before.body };
  const deleted = await bunRequest('DELETE', `/v1/sandboxes/${encodeURIComponent(state.envId)}?deleteData=true`);
  summary.delete = { status: deleted.status, body: deleted.body };
  check('cleanup: DELETE succeeds (binding-only is enough)', deleted.status === 200, deleted);
  let after = null;
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    after = await bunView(state.envId);
    if (after.status === 404) break;
    await sleep(100);
  }
  const bindings = readBindings();
  summary.after = {
    status: after?.status ?? null,
    sandboxDirectoryExists: fs.existsSync(path.join(SANDBOXES_ROOT, state.sandboxId ?? '')),
    binding: bindings.bindings?.[state.envId] ?? null,
    processes: matchingProcesses(state.workspacePath ?? '', state.sandboxId),
  };
  check('cleanup: GET after DELETE is 404', after?.status === 404, after);
  check('cleanup: sandbox directory removed', !summary.after.sandboxDirectoryExists, summary.after.sandboxDirectoryExists);
  check('cleanup: binding removed', summary.after.binding === null, summary.after.binding);
  check('cleanup: no SRT process remains', summary.after.processes.processCount === 0, summary.after.processes);
  return summary;
}

// ------------------------------------------------------- loom-down mode ----

async function loomDownMode() {
  const state = readState();
  const summary = { mode: MODE, startedAt: nowIso(), state, create: null, items: null, session: null, hostHome: null, hostWorkspace: null };
  const createResponse = await api('POST', '/agents/sessions', {
    body: { agent: { model: MODEL, instructions: INSTRUCTIONS }, environment: { type: 'openai_hosted' } },
  });
  summary.create = { status: createResponse.status, error: summarizeError(createResponse), body: createResponse.body };
  check('loom-down: create returns 502', createResponse.status === 502, createResponse.status);
  check('loom-down: create error is upstream_unavailable', summarizeError(createResponse).code === 'upstream_unavailable', summarizeError(createResponse));

  const itemsResponse = await api('GET', `/agents/sessions/${state.sessionId}/items?order=asc&limit=50`);
  summary.items = { status: itemsResponse.status, error: summarizeError(itemsResponse), body: itemsResponse.body };
  check('loom-down: items returns 502', itemsResponse.status === 502, itemsResponse.status);
  check('loom-down: items error is upstream_unavailable', summarizeError(itemsResponse).code === 'upstream_unavailable', summarizeError(itemsResponse));

  const sessionResponse = await api('GET', `/agents/sessions/${state.sessionId}`);
  summary.session = { status: sessionResponse.status, error: summarizeError(sessionResponse), body: sessionResponse.body };
  check(
    'loom-down: session lookup is also an upstream error (no local fallback)',
    sessionResponse.status === 502 && summarizeError(sessionResponse).code === 'upstream_unavailable',
    summary.session,
  );

  if (HOST_HOME.length > 0) {
    const databases = findDatabaseFiles(HOST_HOME);
    summary.hostHome = { path: HOST_HOME, databaseFiles: databases };
    check('loom-down: temporary HOME has no local threads.db', !databases.some((file) => file.endsWith('threads.db')), databases);
    check('loom-down: temporary HOME has no second sqlite store', databases.length === 0, databases);
  } else {
    summary.hostHome = { skipped: 'PERI_E2E_HOME is not set' };
  }
  if (HOST_WORKSPACE_ROOT.length > 0) {
    const directories = fs.existsSync(HOST_WORKSPACE_ROOT) ? fs.readdirSync(HOST_WORKSPACE_ROOT).sort() : [];
    const baseline = state.workspaceRootBaseline ?? directories;
    const newDirectories = directories.filter((name) => !baseline.includes(name));
    const newDirectoriesEmpty = newDirectories.every(
      (name) => fs.readdirSync(path.join(HOST_WORKSPACE_ROOT, name)).length === 0,
    );
    summary.hostWorkspace = { path: HOST_WORKSPACE_ROOT, before: baseline, after: directories, newDirectories };
    check('loom-down: the failed create wrote no local session state', newDirectoriesEmpty, summary.hostWorkspace);
  }
  return summary;
}

// ----------------------------------------------------------------- main ----

async function main() {
  const summary =
    MODE === 'idle'
      ? await idleMode()
      : MODE === 'prepare'
        ? await prepareMode()
        : MODE === 'bun-down'
          ? await bunDownMode()
          : MODE === 'cleanup-env'
            ? await cleanupEnvMode()
            : MODE === 'loom-down'
              ? await loomDownMode()
              : (() => {
                  throw new Error(`unknown mode: ${MODE}`);
                })();
  summary.checks = checks;
  summary.checkCount = checks.length;
  summary.failedChecks = failedChecks();
  summary.timeline = timeline;
  summary.finishedAt = nowIso();
  const text = `${JSON.stringify(summary, null, 2)}\n`;
  if (OUT_PATH.length > 0) {
    fs.mkdirSync(path.dirname(OUT_PATH), { recursive: true, mode: 0o700 });
    fs.writeFileSync(OUT_PATH, text, { mode: 0o600 });
  }
  process.stdout.write(text);
  if (summary.failedChecks.length > 0) process.exitCode = 1;
}

main().catch((error) => {
  console.error(error instanceof Error ? error.stack ?? error.message : String(error));
  process.exitCode = 1;
});
