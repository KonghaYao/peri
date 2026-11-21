#!/usr/bin/env node
// S9a4b end-to-end driver: the persisted-session boundary after only the Rust
// host (`peri-swarm`) is killed and restarted. Loom, Bun, the model stub and
// the Rivet engine keep running.
//
// Modes (first CLI argument, default `prepare`):
//
//   prepare       create one `openai_hosted` session, run one workspace tool
//                 turn (`[[tool:once]]` appends one line to `ran.log`), then
//                 snapshot the session view, every item page, the Bun sandbox
//                 view, the SRT process group, `ran.log` (bytes/sha256/mtime)
//                 and the model-stub counters. No input is sent in this mode
//                 beyond the single tool turn.
//
//   after-restart (gateway restarted, same Loom/Bun/stub) verify:
//                 - `GET /agents/sessions/{id}` is the persisted session
//                   (same id, rootSessionId, state, lastItemSeq, environment);
//                 - `GET /agents/sessions/{id}/items` pages reproduce the same
//                   seq/itemId/kind order and the same page cursors;
//                 - the sandbox `runId`, SRT process group, `ran.log`
//                   (content + mtime) and model-stub counters are unchanged,
//                   i.e. reading history did not replay the old command;
//                 - one new text-only `POST /events`: either the reattached
//                   turn completes (`reattached=true`) or the call fails with
//                   `503 session_not_live` (`reattached=false`). Both outcomes
//                   are recorded, never converted into each other.
//
//   probe         print the current gateway/Bun/Loom/stub PIDs, the gateways
//                 seen in `ps`, and the sandbox/process view (kill/restart
//                 timeline helper; needs the state file).
//
// Requirements (same harness as S9a2/S9a3/S9a4a):
//   - Peri Loom (9497) with the session and state databases,
//   - Bun swarm service (8081) with `PERI_PATH`,
//   - the swarm gateway (`target/debug/peri-swarm`, 18080) with the Bun bridge,
//   - `node peri-swarm/scripts/local_model_stub.mjs` (9496) started with
//     `PERI_STUB_COMMANDS={"once":"printf 'once\\n' >> ran.log; ..."}`.
//
// Environment:
//   PERI_SWARM_BASE_URL        default http://127.0.0.1:18080/v1
//   PERI_SWARM_API_KEY         gateway bearer key (required)
//   PERI_SWARM_MODEL           default stub-model
//   PERI_MODEL_STUB_URL        default http://127.0.0.1:9496
//   PERI_SWARM_BUN_URL         default http://127.0.0.1:8081
//   PERI_SWARM_BUN_TOKEN       Bun outer bearer token (required)
//   PERI_SWARM_SANDBOXES_ROOT  default ~/.peri-swarm/sandboxes
//   PERI_E2E_STATE             state file (default /private/tmp/peri-swarm-s9a4b/state.json)
//   PERI_E2E_BUN_LOG           optional Bun log: its fingerprint must be unchanged
//                              by the history GETs after the restart
//   PERI_E2E_OUT               optional path for the JSON evidence record
//   PERI_E2E_PAGE_SIZE         items page size (default 3)
//   PERI_E2E_TIMEOUT_MS        default 120000
//
// Exit code 0 only when every assertion of the selected mode passes. The JSON
// summary on stdout is the evidence record; no credential is ever printed.

import crypto from 'node:crypto';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';

const MODE = process.argv[2] ?? 'prepare';
const BASE_URL = process.env.PERI_SWARM_BASE_URL ?? 'http://127.0.0.1:18080/v1';
const API_KEY = process.env.PERI_SWARM_API_KEY ?? '';
const MODEL = process.env.PERI_SWARM_MODEL ?? 'stub-model';
const STUB_URL = process.env.PERI_MODEL_STUB_URL ?? 'http://127.0.0.1:9496';
const BUN_URL = (process.env.PERI_SWARM_BUN_URL ?? 'http://127.0.0.1:8081').replace(/\/$/, '');
const BUN_TOKEN = process.env.PERI_SWARM_BUN_TOKEN ?? '';
const SANDBOXES_ROOT = process.env.PERI_SWARM_SANDBOXES_ROOT ?? path.join(homedir(), '.peri-swarm', 'sandboxes');
const STATE_PATH = process.env.PERI_E2E_STATE ?? '/private/tmp/peri-swarm-s9a4b/state.json';
const BUN_LOG = process.env.PERI_E2E_BUN_LOG ?? '';
const OUT_PATH = process.env.PERI_E2E_OUT ?? '';
const PAGE_SIZE = Number(process.env.PERI_E2E_PAGE_SIZE ?? '3');
const TIMEOUT_MS = Number(process.env.PERI_E2E_TIMEOUT_MS ?? '120000');
const CONTRACT_HEADERS = { 'x-peri-swarm-expect': '1', 'OpenAI-Beta': 'agents=v1' };

const ONCE_DIRECTIVE = 'once';
const ONCE_COMMAND = "printf 'once\\n' >> ran.log; printf 'once\\n' > once.txt";
const INSTRUCTIONS =
  'S9a4b restart-boundary verification agent. Run the workspace Bash command for the directive, then reply with the stub text unchanged.';

if (!API_KEY) {
  console.error('[s9a4b] PERI_SWARM_API_KEY is required');
  process.exit(2);
}
if (!BUN_TOKEN) {
  console.error('[s9a4b] PERI_SWARM_BUN_TOKEN is required');
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
function sha256(text) {
  return crypto.createHash('sha256').update(text).digest('hex');
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

// ------------------------------------------------------------- items ----

// Reads every page with `after` (the seq cursor) and records the page shape so
// a restart can be compared on seq/itemId/kind/order and `nextCursor`.
async function readAllItems(sessionId) {
  const pages = [];
  const items = [];
  let after = 0;
  for (let guard = 0; guard < 200; guard += 1) {
    const query = new URLSearchParams({ order: 'asc', limit: String(PAGE_SIZE) });
    if (after > 0) query.set('after', String(after));
    const response = await api('GET', `/agents/sessions/${sessionId}/items?${query.toString()}`);
    check(`items page after=${after} returns 200`, response.status === 200, summarizeError(response));
    const data = response.body ?? {};
    const pageItems = Array.isArray(data.items) ? data.items : [];
    pages.push({
      after,
      status: response.status,
      itemSeqs: pageItems.map((item) => item.seq),
      itemIds: pageItems.map((item) => item.itemId ?? null),
      firstCursor: data.firstCursor ?? null,
      nextCursor: data.nextCursor ?? null,
    });
    for (const item of pageItems) {
      items.push({ seq: item.seq, itemId: item.itemId ?? null, kind: item.kind ?? null });
    }
    if (data.nextCursor === null || data.nextCursor === undefined) break;
    const parsed = Number(data.nextCursor);
    if (!Number.isFinite(parsed) || parsed <= after) break;
    after = parsed;
  }
  return { pages, items };
}

function compareItems(before, after) {
  const keys = (entries) => entries.map((entry) => `${entry.seq}:${entry.itemId}:${entry.kind}`).join('|');
  const pages = (entries) => entries.map((entry) => `${entry.after}->${entry.nextCursor}[${entry.itemSeqs.join(',')}]`).join('|');
  return {
    sameItemCount: before.items.length === after.items.length,
    sameItemSequence: keys(before.items) === keys(after.items),
    samePageCursors: pages(before.pages) === pages(after.pages),
    beforeCount: before.items.length,
    afterCount: after.items.length,
  };
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
          const frame = parseSseFrame(raw);
          if (frame !== null) state.events.push(frame);
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

// The gateway, Bun service, Loom project service, Rivet engine and model stub
// are identified by command line only; no PID is hard-coded.
function serviceSnapshot() {
  const rows = psSnapshot();
  const find = (predicate) => rows.filter(predicate).map((row) => ({ pid: row.pid, ppid: row.ppid, command: row.command }));
  return {
    periSwarm: find((row) => /(^|\/)peri-swarm$/.test(row.command.split(' ')[0]) || row.command.includes('target/debug/peri-swarm')),
    bunSwarm: find((row) => row.command.includes('src/swarm/main.ts')),
    modelStub: find((row) => row.command.includes('local_model_stub.mjs')),
    loom: find((row) => row.command.includes('prototypes/v1/platform/dev.py')),
    rivetEngine: find((row) => /(^|\/)rivet-engine/.test(row.command) || row.command.includes('rivet-engine')),
  };
}

function pidOf(entries) {
  return entries.length > 0 ? entries.map((entry) => entry.pid) : [];
}

// ------------------------------------------------------------- filesystem ----

function fileEvidence(file) {
  if (!fs.existsSync(file)) return { path: file, exists: false };
  const text = fs.readFileSync(file, 'utf8');
  const stat = fs.statSync(file);
  return {
    path: file,
    exists: true,
    bytes: stat.size,
    mtimeMs: stat.mtimeMs,
    lines: text.split('\n').filter((line) => line.length > 0).length,
    sha256: sha256(text),
    text: text.length > 200 ? `${text.slice(0, 200)}…` : text,
  };
}

function writeState(state) {
  fs.mkdirSync(path.dirname(STATE_PATH), { recursive: true, mode: 0o700 });
  fs.writeFileSync(STATE_PATH, `${JSON.stringify(state, null, 2)}\n`, { mode: 0o600 });
}

// Post-turn probes (post-start resource re-discovery, git attribution) can
// finish just after the turn terminal. Snapshotting the Bun log too early would
// attribute those writes to the later history GETs, so the baseline waits until
// the file stops growing.
async function waitForQuiescence(file, stableMs = 800, timeoutMs = 8000) {
  if (file.length === 0) return { waited: true, waitedMs: 0 };
  const started = Date.now();
  let lastSize = -1;
  let stableSince = Date.now();
  while (Date.now() - started < timeoutMs) {
    let size = -1;
    try {
      size = fs.statSync(file).size;
    } catch {
      size = -1;
    }
    if (size !== lastSize) {
      lastSize = size;
      stableSince = Date.now();
    } else if (Date.now() - stableSince >= stableMs) {
      return { waited: true, waitedMs: Date.now() - started, bytes: size };
    }
    await sleep(50);
  }
  return { waited: false, waitedMs: Date.now() - started, bytes: lastSize };
}

function readState() {
  if (!fs.existsSync(STATE_PATH)) throw new Error(`state file not found: ${STATE_PATH}`);
  return readJson(STATE_PATH);
}

// ------------------------------------------------------------ prepare mode ----

const ONCE_TEXT =
  `S9a4b prepare [[tool:${ONCE_DIRECTIVE}]] Run the workspace Bash command for ` +
  `${ONCE_DIRECTIVE} exactly once, then reply with the stub text unchanged.`;

const NEW_TURN_TEXT =
  'S9a4b after restart: reply with the stub text only. Do not run any tool.';

async function prepareMode() {
  const summary = { mode: MODE, startedAt: nowIso(), session: null, sandbox: null, stub: null, items: null, files: null, services: null };
  const reset = await fetch(`${STUB_URL}/__reset`, { method: 'POST' });
  check('model stub reset', reset.ok, { status: reset.status });
  mark('prepare_gateway_snapshot', pidOf(serviceSnapshot().periSwarm));

  const created = await createSession({ type: 'openai_hosted' }, 'prepare');
  const session = created.created;
  const envId = session.environment?.id;
  const sessionId = session.id;

  const view = await bunView(envId);
  const reservation = view.body?.sandbox;
  const workspacePath = reservation?.workspacePath;
  check('prepare: sandbox binding exists', Boolean(reservation), view.body);

  const stream = openSse(sessionId, { after: session.lastItemSeq ?? 0 });
  const accepted = await api('POST', `/agents/sessions/${sessionId}/events`, {
    body: { kind: 'user', mode: 'turn', text: ONCE_TEXT },
    headers: { 'idempotency-key': crypto.randomUUID() },
  });
  check('prepare: events POST accepted', accepted.status === 202 && accepted.body?.accepted === true, accepted);
  const terminal = await waitForTerminal(stream, TIMEOUT_MS);
  stream.abort();
  check('prepare: turn completed', turnStateOf(terminal) === 'completed', terminal ?? null);

  const afterView = await bunView(envId);
  const sandbox = afterView.body?.sandbox;
  const quiescence = await waitForQuiescence(BUN_LOG);
  mark('prepare_bun_log_quiescent', quiescence);
  const processes = matchingProcesses(workspacePath ?? '', sandbox?.sandboxId);
  const stats = await stubStats();
  const ranLog = fileEvidence(path.join(workspacePath ?? '', 'ran.log'));
  const onceFile = fileEvidence(path.join(workspacePath ?? '', 'once.txt'));
  const bunLog = BUN_LOG.length > 0 ? fileEvidence(BUN_LOG) : null;
  const items = await readAllItems(sessionId);
  const sessionView = await api('GET', `/agents/sessions/${sessionId}`);

  summary.session = {
    id: sessionId,
    logicalId: session.logicalId ?? null,
    environment: session.environment ?? null,
    preKillView: { status: sessionView.status, body: sessionView.body },
  };
  summary.sandbox = {
    envId,
    sandboxId: sandbox?.sandboxId ?? null,
    runId: sandbox?.runId ?? null,
    state: sandbox?.state ?? null,
    workspacePath: sandbox?.workspacePath ?? null,
    providerVersion: sandbox?.providerVersion ?? null,
    processes,
  };
  summary.stub = { mainCalls: stats.mainCalls, toolCalls: stats.toolCalls, turns: Array.isArray(stats.turns) ? stats.turns.length : null };
  summary.items = items;
  summary.files = { ranLog, onceFile };
  summary.bunLog = bunLog;
  summary.quiescence = quiescence;
  summary.services = serviceSnapshot();
  summary.servicesPids = {
    gateway: pidOf(summary.services.periSwarm),
    bun: pidOf(summary.services.bunSwarm),
    stub: pidOf(summary.services.modelStub),
    loom: pidOf(summary.services.loom),
    rivet: pidOf(summary.services.rivetEngine),
  };

  check('prepare: sandbox is ready', sandbox?.state === 'ready', sandbox);
  check('prepare: exactly one SRT process group', processes.processGroupCount === 1, processes);
  check('prepare: ran.log has one line', ranLog.lines === 1, ranLog);
  check('prepare: once.txt exists', onceFile.exists, onceFile);
  check('prepare: items were persisted', items.items.length > 0, { count: items.items.length });
  check('prepare: session view is 200', sessionView.status === 200, summarizeError(sessionView));
  mark('prepare_done', { sessionId, envId, runId: sandbox?.runId ?? null, items: items.items.length });

  const state = {
    sessionId,
    envId,
    logicalId: session.logicalId ?? null,
    sandboxId: sandbox?.sandboxId ?? null,
    runId: sandbox?.runId ?? null,
    workspacePath: sandbox?.workspacePath ?? null,
    sessionView: sessionView.body,
    items,
    files: { ranLog, onceFile },
    stub: summary.stub,
    bunLog,
    processes,
    servicesPids: summary.servicesPids,
    preparedAt: nowIso(),
  };
  writeState(state);
  summary.statePath = STATE_PATH;
  return summary;
}

// ------------------------------------------------------------- pre-kill mode ----

// Last observation while the first host is still alive: the Bun log baseline
// and the sandbox/process/stub facts the restart must not change. This is what
// separates "activity before the kill" from "history GETs after the restart".
async function preKillMode() {
  const state = readState();
  const view = await bunView(state.envId);
  const sandbox = view.body?.sandbox;
  const processes = matchingProcesses(state.workspacePath ?? '', state.sandboxId);
  const summary = {
    mode: MODE,
    at: nowIso(),
    services: serviceSnapshot(),
    sandbox: { status: view.status, body: view.body },
    processes,
    stub: await stubStats(),
    files: {
      ranLog: fileEvidence(path.join(state.workspacePath ?? '', 'ran.log')),
      onceFile: fileEvidence(path.join(state.workspacePath ?? '', 'once.txt')),
    },
    bunLog: BUN_LOG.length > 0 ? fileEvidence(BUN_LOG) : null,
  };
  summary.servicesPids = {
    gateway: pidOf(summary.services.periSwarm),
    bun: pidOf(summary.services.bunSwarm),
    stub: pidOf(summary.services.modelStub),
    loom: pidOf(summary.services.loom),
    rivet: pidOf(summary.services.rivetEngine),
  };
  check('pre-kill: sandbox runId is still the prepared one', sandbox?.runId === state.runId, {
    before: state.runId,
    after: sandbox?.runId ?? null,
  });
  check(
    'pre-kill: same SRT process group as prepare',
    processes.processGroups.join(',') === state.processes.processGroups.join(','),
    { before: state.processes.processGroups, after: processes.processGroups },
  );
  state.bunLogAtKill = summary.bunLog;
  state.processesAtKill = processes;
  state.stubAtKill = summary.stub;
  writeState(state);
  return summary;
}

// ------------------------------------------------------ after-restart mode ----

async function afterRestartMode() {
  const state = readState();
  const summary = {
    mode: MODE,
    startedAt: nowIso(),
    state,
    services: serviceSnapshot(),
    session: null,
    items: null,
    sandbox: null,
    stub: null,
    files: null,
    input: null,
  };
  summary.servicesPids = {
    gateway: pidOf(summary.services.periSwarm),
    bun: pidOf(summary.services.bunSwarm),
    stub: pidOf(summary.services.modelStub),
    loom: pidOf(summary.services.loom),
    rivet: pidOf(summary.services.rivetEngine),
  };
  mark('after_restart_gateway_snapshot', summary.servicesPids.gateway);
  // Baseline taken after the new host is listening but before any session GET:
  // a difference before the first POST can only come from the history reads.
  const bunLogAtRestart = BUN_LOG.length > 0 ? fileEvidence(BUN_LOG) : null;
  summary.bunLogAtRestart = bunLogAtRestart;
  check(
    'restart: no gateway PID from prepare survived',
    summary.servicesPids.gateway.every((pid) => !(state.servicesPids.gateway ?? []).includes(pid)),
    { before: state.servicesPids.gateway ?? [], now: summary.servicesPids.gateway },
  );

  // 1. Persisted session view.
  const view = await api('GET', `/agents/sessions/${state.sessionId}`);
  const viewBody = view.body ?? {};
  summary.session = { status: view.status, body: viewBody };
  check('restart: GET session returns 200', view.status === 200, summarizeError(view));
  check('restart: same session id', viewBody.id === state.sessionId, { id: viewBody.id });
  check(
    'restart: same rootSessionId',
    viewBody.rootSessionId === state.sessionView.rootSessionId,
    { before: state.sessionView.rootSessionId, after: viewBody.rootSessionId },
  );
  check('restart: same environment id', viewBody.environment?.id === state.envId, viewBody.environment);
  check(
    'restart: lastItemSeq is the persisted maximum',
    viewBody.lastItemSeq === state.sessionView.lastItemSeq,
    { before: state.sessionView.lastItemSeq, after: viewBody.lastItemSeq },
  );
  mark('get_session_after_restart', { status: view.status, state: viewBody.state ?? null });

  // 2. Items: same order, same item ids, same page cursors.
  const items = await readAllItems(state.sessionId);
  const comparison = compareItems(state.items, items);
  summary.items = { ...items, comparison };
  check('restart: item count is unchanged', comparison.sameItemCount, comparison);
  check('restart: item seq/itemId/kind order is unchanged', comparison.sameItemSequence, comparison);
  check('restart: page cursors are continuous and unchanged', comparison.samePageCursors, comparison);
  mark('items_after_restart', comparison);

  // 3. No external command replay: same sandbox run, same process group, same
  //    workspace file bytes/mtime, same model-stub counters.
  const bunViewAfter = await bunView(state.envId);
  const sandbox = bunViewAfter.body?.sandbox;
  const processes = matchingProcesses(state.workspacePath ?? '', state.sandboxId);
  const stats = await stubStats();
  const ranLog = fileEvidence(path.join(state.workspacePath ?? '', 'ran.log'));
  const onceFile = fileEvidence(path.join(state.workspacePath ?? '', 'once.txt'));
  summary.sandbox = { envId: state.envId, status: bunViewAfter.status, body: bunViewAfter.body, processes };
  summary.stub = {
    mainCalls: stats.mainCalls,
    toolCalls: stats.toolCalls,
    turns: Array.isArray(stats.turns) ? stats.turns.length : null,
  };
  summary.files = { ranLog, onceFile };
  check('restart: sandbox runId is unchanged', sandbox?.runId === state.runId, {
    before: state.runId,
    after: sandbox?.runId ?? null,
  });
  check('restart: sandbox sandboxId is unchanged', sandbox?.sandboxId === state.sandboxId, {
    before: state.sandboxId,
    after: sandbox?.sandboxId ?? null,
  });
  check(
    'restart: same SRT process group/PIDs',
    processes.processGroupCount === state.processes.processGroupCount &&
      processes.processGroups.join(',') === state.processes.processGroups.join(',') &&
      processes.processes.map((entry) => entry.pid).join(',') === state.processes.processes.map((entry) => entry.pid).join(','),
    { before: state.processes, after: processes },
  );
  check('restart: ran.log bytes/sha256/mtime unchanged', ranLog.sha256 === state.files.ranLog.sha256 && ranLog.mtimeMs === state.files.ranLog.mtimeMs, {
    before: state.files.ranLog,
    after: ranLog,
  });
  check('restart: ran.log still has exactly one line', ranLog.lines === 1, ranLog);
  check('restart: once.txt bytes/sha256/mtime unchanged', onceFile.sha256 === state.files.onceFile.sha256 && onceFile.mtimeMs === state.files.onceFile.mtimeMs, {
    before: state.files.onceFile,
    after: onceFile,
  });
  check('restart: model stub counters unchanged before the new input', stats.mainCalls === state.stub.mainCalls && stats.toolCalls === state.stub.toolCalls, {
    before: state.stub,
    after: summary.stub,
  });
  const bunLogBeforeInput = BUN_LOG.length > 0 ? fileEvidence(BUN_LOG) : null;
  summary.bunLogBeforeInput = bunLogBeforeInput;
  const bunLogBaseline = bunLogAtRestart ?? state.bunLogAtKill ?? state.bunLog;
  if (bunLogBeforeInput !== null && bunLogBaseline !== null && bunLogBaseline !== undefined) {
    check(
      'restart: history GETs did not reach the Bun bridge (log unchanged)',
      bunLogBeforeInput.sha256 === bunLogBaseline.sha256 &&
        bunLogBeforeInput.bytes === bunLogBaseline.bytes,
      { before: bunLogBaseline, after: bunLogBeforeInput },
    );
  }
  mark('no_replay_evidence', {
    runId: sandbox?.runId ?? null,
    processGroups: processes.processGroups,
    ranLogSha256: ranLog.sha256,
    stubMainCalls: stats.mainCalls,
  });

  // 4. One new text-only input: either the reattached turn completes or the
  //    call is rejected as `503 session_not_live`. Both are recorded as-is.
  const stream = openSse(state.sessionId, { after: viewBody.lastItemSeq ?? state.sessionView.lastItemSeq ?? 0 });
  const inputId = crypto.randomUUID();
  const accepted = await api('POST', `/agents/sessions/${state.sessionId}/events`, {
    body: { kind: 'user', mode: 'turn', text: NEW_TURN_TEXT },
    headers: { 'idempotency-key': inputId },
  });
  mark('post_events_after_restart', { status: accepted.status, body: accepted.body });
  if (accepted.status === 202 && accepted.body?.accepted === true) {
    const terminal = await waitForTerminal(stream, TIMEOUT_MS);
    stream.abort();
    summary.input = {
      idempotencyKey: inputId,
      status: accepted.status,
      accepted: true,
      terminal: terminal ?? null,
      afterItems: await readAllItems(state.sessionId),
      afterStub: await stubStats(),
      afterRanLog: fileEvidence(path.join(state.workspacePath ?? '', 'ran.log')),
      afterProcesses: matchingProcesses(state.workspacePath ?? '', state.sandboxId),
      bunLogAfterInput: BUN_LOG.length > 0 ? fileEvidence(BUN_LOG) : null,
      reattached: true,
    };
    check('restart: new turn completed', turnStateOf(terminal) === 'completed', terminal ?? null);
    check(
      'restart: text-only turn did not run the old command',
      summary.input.afterRanLog.sha256 === ranLog.sha256 && summary.input.afterRanLog.lines === 1,
      { before: ranLog, after: summary.input.afterRanLog },
    );
    check(
      'restart: text-only turn added no new SRT process group',
      summary.input.afterProcesses.processGroups.join(',') === processes.processGroups.join(','),
      { before: processes, after: summary.input.afterProcesses },
    );
    const prefixKeys = (entries) => entries.map((entry) => `${entry.seq}:${entry.itemId}:${entry.kind}`).join('|');
    check(
      'restart: pre-kill items stayed a prefix (load replay did not duplicate them)',
      summary.input.afterItems.items.length > state.items.items.length &&
        prefixKeys(summary.input.afterItems.items.slice(0, state.items.items.length)) ===
          prefixKeys(state.items.items),
      {
        beforeCount: state.items.items.length,
        afterCount: summary.input.afterItems.items.length,
      },
    );
    check(
      'restart: the reattached turn made exactly one new main model call',
      summary.input.afterStub.mainCalls === stats.mainCalls + 1 &&
        summary.input.afterStub.toolCalls === stats.toolCalls,
      { before: summary.stub, after: summary.input.afterStub },
    );
    mark('new_turn_completed', {
      mainCalls: summary.input.afterStub.mainCalls,
      itemSeq: summary.input.afterItems.items.at(-1)?.seq ?? null,
    });
  } else {
    stream.abort();
    summary.input = {
      idempotencyKey: inputId,
      status: accepted.status,
      accepted: false,
      error: summarizeError(accepted),
      reattached: false,
    };
    check(
      'restart: rejected input is the documented session_not_live boundary',
      (accepted.status === 502 || accepted.status === 503) &&
        accepted.body?.error?.details?.operation === 'session_not_live',
      summarizeError(accepted),
    );
    mark('new_turn_rejected', summarizeError(accepted));
  }
  summary.reattached = summary.input.reattached;

  return summary;
}

// -------------------------------------------------------------- probe mode ----

async function probeMode() {
  const state = readState();
  const services = serviceSnapshot();
  const bunViewAfter = await bunView(state.envId);
  const sandbox = bunViewAfter.body?.sandbox;
  const processes = matchingProcesses(state.workspacePath ?? '', state.sandboxId);
  const summary = {
    mode: MODE,
    at: nowIso(),
    services,
    servicesPids: {
      gateway: pidOf(services.periSwarm),
      bun: pidOf(services.bunSwarm),
      stub: pidOf(services.modelStub),
      loom: pidOf(services.loom),
      rivet: pidOf(services.rivetEngine),
    },
    sandbox: { status: bunViewAfter.status, body: bunViewAfter.body },
    processes,
    files: { ranLog: fileEvidence(path.join(state.workspacePath ?? '', 'ran.log')) },
    stub: await stubStats(),
  };
  check('probe: sandbox view is 200', bunViewAfter.status === 200, summarizeError(bunViewAfter));
  check('probe: sandbox runId is unchanged', sandbox?.runId === state.runId, {
    before: state.runId,
    after: sandbox?.runId ?? null,
  });
  return summary;
}

// ------------------------------------------------------------------ main ----

const MODES = { prepare: prepareMode, 'pre-kill': preKillMode, 'after-restart': afterRestartMode, probe: probeMode };
const runMode = MODES[MODE];
if (!runMode) {
  console.error(`[s9a4b] unknown mode ${MODE}; expected ${Object.keys(MODES).join(' / ')}`);
  process.exit(2);
}

let summary;
let exitCode = 0;
try {
  summary = await runMode();
} catch (error) {
  summary = { mode: MODE, error: error?.stack ?? String(error), timeline };
  exitCode = 1;
}
const failures = failedChecks();
const record = {
  ...summary,
  at: nowIso(),
  checks,
  failures,
  timeline,
};
if (OUT_PATH.length > 0) {
  fs.mkdirSync(path.dirname(OUT_PATH), { recursive: true, mode: 0o700 });
  fs.writeFileSync(OUT_PATH, `${JSON.stringify(record, null, 2)}\n`, { mode: 0o600 });
}
console.log(JSON.stringify(record, null, 2));
if (exitCode === 0 && failures.length > 0) exitCode = 1;
if (exitCode !== 0) {
  console.error(`[s9a4b] failed checks: ${failures.join(', ') || 'see error'}`);
}
process.exit(exitCode);
