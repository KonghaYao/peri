#!/usr/bin/env node
// S9a3b2 end-to-end driver: SSE disconnect/reconnect and `Idempotency-Key`
// replay against the real gateway, Bun workspace and SRT sandbox.
//
// Reconnect scenario (hosted environment, slow Bash):
//   1. open `GET /events?after=0` and post one turn whose Bash command runs
//      for ~2.4 s (`slow-reconnect`);
//   2. read events until the `tool_call` item arrives, then abort the SSE
//      connection (record its SSE `id`);
//   3. while no SSE client exists, poll `GET /items` and observe the turn
//      continue and finish (`tool_result` and a full-text completion row);
//   4. reconnect with `Last-Event-ID: <aborted id>` and collect the remaining
//      item events; every seq must arrive exactly once, with no gap;
//   5. reconnect with `?afterSeq=<last-1>` and verify the query cursor form;
//   6. page `GET /items` with `limit=2` from the disconnect cursor and verify
//      `nextCursor`/`hasMore` cover every seq once;
//   7. match the single full-text replay delta and persisted completion row
//      against the model stub's reply.
//
// Idempotency scenario (environment `none`, no directive):
//   1. two concurrent `POST /events` with the same UUID `Idempotency-Key` and
//      identical text: both 202 with the same `inputId`, one inserted + one
//      existing, one turn in the stub and one `complete` item;
//   2. same key + different text: `409 input_conflict`, no model call and no
//      new item (never enqueued);
//   3. replay after completion: `202 existing`, no new turn;
//   4. `swarm_inputs` holds exactly one receipt row for the key (optional Loom
//      Hrana probe; the credential is read from the connection file and never
//      printed).
//
// Requirements (same harness as S9a2/S9a3a/S9a3b1):
//   - Peri Loom (9497) with the session and state databases,
//   - Bun swarm service (8081) with `PERI_PATH` pointing at the Peri binary,
//   - the swarm gateway (18080) with the Bun bridge and the model stub provider,
//   - `node peri-swarm/scripts/local_model_stub.mjs` (9496) started with the
//     same `PERI_STUB_COMMANDS` JSON,
//   - a local agent SDK checkout is not required: this driver speaks the HTTP
//     contract directly so it can read SSE `id` fields.
//
// Environment:
//   PERI_SWARM_BASE_URL        default http://127.0.0.1:18080/v1
//   PERI_SWARM_API_KEY         gateway bearer key (required)
//   PERI_SWARM_MODEL           default stub-model
//   PERI_MODEL_STUB_URL        default http://127.0.0.1:9496
//   PERI_SWARM_BUN_URL         default http://127.0.0.1:8081
//   PERI_SWARM_BUN_TOKEN       Bun outer bearer token (required)
//   PERI_SWARM_SANDBOXES_ROOT  default ~/.peri-swarm/sandboxes
//   PERI_LOOM_CONNECTION       optional Loom connection.json for the DB probe
//   PERI_E2E_OUT               optional path for the JSON evidence record
//   PERI_E2E_TIMEOUT_MS        default 180000
//
// Exit code 0 only when every assertion passes. The JSON summary on stdout is
// the evidence record; no credential is ever printed.

import crypto from 'node:crypto';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';

const BASE_URL = process.env.PERI_SWARM_BASE_URL ?? 'http://127.0.0.1:18080/v1';
const API_KEY = process.env.PERI_SWARM_API_KEY ?? '';
const MODEL = process.env.PERI_SWARM_MODEL ?? 'stub-model';
const STUB_URL = process.env.PERI_MODEL_STUB_URL ?? 'http://127.0.0.1:9496';
const BUN_URL = (process.env.PERI_SWARM_BUN_URL ?? 'http://127.0.0.1:8081').replace(/\/$/, '');
const BUN_TOKEN = process.env.PERI_SWARM_BUN_TOKEN ?? '';
const SANDBOXES_ROOT = process.env.PERI_SWARM_SANDBOXES_ROOT ?? path.join(homedir(), '.peri-swarm', 'sandboxes');
const LOOM_CONNECTION = process.env.PERI_LOOM_CONNECTION ?? '';
const TIMEOUT_MS = Number(process.env.PERI_E2E_TIMEOUT_MS ?? '180000');
const OUT_PATH = process.env.PERI_E2E_OUT ?? '';
const CONTRACT_HEADERS = { 'x-peri-swarm-expect': '1', 'OpenAI-Beta': 'agents=v1' };

const RECONNECT_DIRECTIVE = 'slow-reconnect';
const RECONNECT_COMMAND = 'for i in 1 2 3 4 5 6; do echo tick-$i; sleep 0.4; done; echo slow-done';
const RECONNECT_TEXT =
  `S9a3b2 reconnect [[tool:${RECONNECT_DIRECTIVE}]] Run the workspace Bash command for ` +
  `${RECONNECT_DIRECTIVE}, then reply with the stub text unchanged.`;
const INSTRUCTIONS =
  'S9a3b2 reconnect/idempotency verification agent. Run the workspace Bash command for this step exactly as described, then reply with the stub text unchanged.';

if (!API_KEY) {
  console.error('[s9a3b2] PERI_SWARM_API_KEY is required');
  process.exit(2);
}
if (!BUN_TOKEN) {
  console.error('[s9a3b2] PERI_SWARM_BUN_TOKEN is required');
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

async function stubStats() {
  const response = await fetch(`${STUB_URL}/__stats`);
  if (!response.ok) throw new Error(`model stub /__stats returned ${response.status}`);
  return response.json();
}

function summarizeItem(item) {
  const payload = item.payload ?? {};
  const label = payload.complete === true ? 'message:complete' : payload.delta === true ? 'message:delta' : item.kind;
  return {
    seq: item.seq,
    itemId: item.itemId,
    kind: item.kind,
    label,
    toolCallId: payload.toolCallId,
    status: payload.status,
    input: payload.input,
    output: typeof payload.output === 'string' ? payload.output : undefined,
    text: typeof payload.text === 'string' ? payload.text : undefined,
    messageId: payload.messageId,
  };
}

function summarizeItems(items) {
  return items.map(summarizeItem);
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

async function createSession(environment, label) {
  const started = Date.now();
  const response = await api('POST', '/agents/sessions', {
    body: { agent: { model: MODEL, instructions: INSTRUCTIONS }, environment },
  });
  const created = response.body ?? {};
  check(`${label}: create status 201`, response.status === 201, { status: response.status, body: created });
  check(`${label}: create reports created=true`, created.created === true, created.created);
  check(`${label}: create returns a session id`, typeof created.id === 'string' && created.id.length > 0, created.id);
  check(`${label}: create returns a logicalId`, typeof created.logicalId === 'string' && created.logicalId.length > 0, created.logicalId);
  return { created, createMs: Date.now() - started, status: response.status };
}

function startEvent(sessionId, inputId, text) {
  const startedAt = Date.now();
  const promise = api('POST', `/agents/sessions/${sessionId}/events`, {
    body: { kind: 'user', mode: 'turn', text },
    headers: { 'idempotency-key': inputId },
  }).then((response) => ({ ...response, elapsedMs: Date.now() - startedAt, finishedAt: nowIso() }));
  return { inputId, startedAt, promise };
}

async function itemsPage(sessionId, { after, afterSeq, limit = 500, order = 'asc' } = {}) {
  const params = new URLSearchParams({ order, limit: String(limit) });
  if (afterSeq !== undefined) params.set('afterSeq', String(afterSeq));
  if (after !== undefined) params.set('after', String(after));
  const response = await api('GET', `/agents/sessions/${sessionId}/items?${params.toString()}`);
  if (response.status !== 200) {
    throw new Error(`GET items returned ${response.status}: ${JSON.stringify(response.body).slice(0, 400)}`);
  }
  return response.body;
}

async function allItems(sessionId) {
  const page = await itemsPage(sessionId, { limit: 500 });
  return page.items ?? [];
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
  let data = null;
  const rawData = dataLines.join('\n');
  if (rawData.length > 0) {
    try {
      data = JSON.parse(rawData);
    } catch {
      data = rawData;
    }
  }
  return { event, id, data, at: nowIso() };
}

/**
 * Open one SSE response and parse frames as they arrive. `fetch` is used
 * directly (not an SDK) because the `id:` field is the resume cursor under
 * test. `state.events` keeps every frame in order; `state.itemIds` keeps only
 * item frames, which are the persisted seq values.
 */
function openSse(sessionId, { after, afterSeq, lastEventId, label } = {}) {
  const params = new URLSearchParams();
  if (after !== undefined) params.set('after', String(after));
  if (afterSeq !== undefined) params.set('afterSeq', String(afterSeq));
  const query = params.toString();
  const headers = { authorization: `Bearer ${API_KEY}`, ...CONTRACT_HEADERS, accept: 'text/event-stream' };
  if (lastEventId !== undefined) headers['last-event-id'] = String(lastEventId);
  const controller = new AbortController();
  const state = {
    label: label ?? 'sse',
    request: { after, afterSeq, lastEventId },
    status: null,
    events: [],
    itemIds: [],
    error: null,
    outcome: null,
    openedAt: Date.now(),
    closedAt: null,
  };
  const ready = (async () => {
    try {
      const response = await fetch(`${BASE_URL}/agents/sessions/${sessionId}/events${query ? `?${query}` : ''}`, {
        headers,
        signal: controller.signal,
      });
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
          if (event === null) continue;
          state.events.push(event);
          if (event.event === 'item' && event.id !== null) state.itemIds.push(Number(event.id));
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

async function waitForEvent(stream, predicate, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const found = stream.state.events.find(predicate);
    if (found) return found;
    if (stream.state.error !== null) throw new Error(`${stream.state.label}: ${stream.state.error}`);
    await sleep(25);
  }
  throw new Error(
    `${stream.state.label}: timed out waiting for ${label}; ` +
      `events=${JSON.stringify(stream.state.events.map((event) => [event.event, event.id]).slice(-10))}`,
  );
}

// -------------------------------------------------------------- Loom DB ----

let loomConnectionCache;
function loomConnection() {
  if (loomConnectionCache !== undefined) return loomConnectionCache;
  loomConnectionCache = null;
  if (LOOM_CONNECTION.length > 0 && fs.existsSync(LOOM_CONNECTION)) {
    loomConnectionCache = JSON.parse(fs.readFileSync(LOOM_CONNECTION, 'utf8'));
  }
  return loomConnectionCache;
}

/**
 * Run one statement on the Swarm state database through the Loom Hrana v3
 * pipeline. The project key is read from the connection file and only placed
 * in the Authorization header; it is never returned or logged.
 */
async function hranaQuery(sql, args = []) {
  const connection = loomConnection();
  if (connection === null) return { skipped: 'PERI_LOOM_CONNECTION is not set' };
  const namespace = connection.database?.namespace ?? {};
  const tenant = namespace.tenant ?? 'local_default';
  const branch = connection.database?.branch ?? 'main';
  const database = process.env.PERI_SWARM_STATE_DATABASE ?? 'swarm';
  const endpoint = String(connection.endpoint).replace(/\/$/, '');
  const url = `${endpoint}/db/${tenant}/${database}/${branch}/v3/pipeline`;
  const response = await fetch(url, {
    method: 'POST',
    headers: {
      authorization: `Bearer ${connection.project_api_key}`,
      'content-type': 'application/json',
    },
    body: JSON.stringify({
      baton: null,
      requests: [
        {
          type: 'execute',
          stmt: {
            sql,
            args: args.map((value) => ({ type: 'text', value: String(value) })),
            named_args: [],
            want_rows: true,
          },
        },
        { type: 'close' },
      ],
    }),
  });
  const payload = await response.json();
  if (!response.ok) return { error: `HTTP ${response.status}`, payload: JSON.stringify(payload).slice(0, 500) };
  const first = Array.isArray(payload.results) ? payload.results[0] : null;
  if (first?.type !== 'ok') return { error: 'pipeline failed', payload: JSON.stringify(first).slice(0, 500) };
  const rows = first.response?.result?.rows ?? [];
  return { rows: rows.map((row) => row.map((cell) => (cell === null ? null : cell?.value ?? null))) };
}

// ---------------------------------------------------------------- teardown ----

async function bunRequest(method, pathname, body) {
  const response = await fetch(`${BUN_URL}${pathname}`, {
    method,
    headers: {
      authorization: `Bearer ${BUN_TOKEN}`,
      ...(body === undefined ? {} : { 'content-type': 'application/json' }),
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

async function bunView(envId) {
  return bunRequest('GET', `/v1/sandboxes/${encodeURIComponent(envId)}`);
}

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
    processes: rows.map((row) => ({ pid: row.pid, ppid: row.ppid, pgid: row.pgid, command: row.command })),
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

async function deleteEnvironment(label, envId) {
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
  return {
    label,
    envId,
    sandboxId,
    workspacePath,
    deleteStatus: response.status,
    deleteBody: response.body,
    afterDeleteStatus: after?.status ?? null,
    sandboxDirectoryExists: sandboxId !== null && fs.existsSync(path.join(SANDBOXES_ROOT, sandboxId)),
    processGroupCount: processes?.processGroupCount ?? null,
    processCount: processes?.processCount ?? null,
    processes: processes?.processes ?? [],
  };
}

// ----------------------------------------------------------------- main ----

async function main() {
  const summary = {
    transport: 'http',
    baseUrl: BASE_URL,
    bunUrl: BUN_URL,
    model: MODEL,
    stubUrl: STUB_URL,
    sandboxesRoot: SANDBOXES_ROOT,
    loomConnectionFile: LOOM_CONNECTION,
    directive: RECONNECT_DIRECTIVE,
    command: RECONNECT_COMMAND,
    startedAt: nowIso(),
    timeline,
    checks: [],
    checkCount: 0,
  };
  const envs = [];
  let failure = null;

  const reset = await fetch(`${STUB_URL}/__reset`, { method: 'POST' });
  check('model stub reset', reset.ok, { status: reset.status });
  const baseline = {
    stub: await stubStats(),
    sandboxDirectories: rootDirectories(),
    bindings: bindingsSnapshot(),
  };
  summary.baseline = baseline;
  check(
    'model stub exposes the slow-reconnect directive',
    Array.isArray(baseline.stub.directives) && baseline.stub.directives.includes(RECONNECT_DIRECTIVE),
    baseline.stub.directives,
  );

  try {
    // ------------------------------------------------------- reconnect ----
    const reconnectSession = await createSession({ type: 'openai_hosted' }, 'R');
    const session = reconnectSession.created;
    if (typeof session.environment?.id === 'string') {
      envs.push({ label: 'R', envId: session.environment.id });
    }
    summary.reconnect = {
      createMs: reconnectSession.createMs,
      sessionId: session.id,
      logicalId: session.logicalId,
      environment: session.environment,
      state: session.state,
      lastItemSeq: session.lastItemSeq,
    };
    check('R: create reports environment.type=openai_hosted', session.environment?.type === 'openai_hosted', session.environment);
    check('R: create left the session idle', session.state === 'idle', session.state);

    const sse1 = openSse(session.id, { after: 0, label: 'sse1' });
    mark('R: sse1 opened with after=0');
    const turn = startEvent(session.id, crypto.randomUUID(), RECONNECT_TEXT);
    mark('R: turn posted', { inputId: turn.inputId, text: RECONNECT_TEXT });

    const toolCallEvent = await waitForEvent(
      sse1,
      (event) =>
        event.event === 'item' &&
        event.data?.kind === 'tool_call' &&
        event.data?.payload?.input?.command === RECONNECT_COMMAND,
      TIMEOUT_MS,
      'the slow tool_call item',
    );
    const disconnectMark = mark('R: tool_call item received; aborting sse1', {
      id: toolCallEvent.id,
      seq: toolCallEvent.data?.seq,
      toolCallId: toolCallEvent.data?.payload?.toolCallId,
    });
    const sse1Ids = [...sse1.state.itemIds];
    const lastEventId = sse1Ids.at(-1) ?? 0;
    const sse1Items = sse1.state.events
      .filter((event) => event.event === 'item' && event.id !== null)
      .map((event) => event.data);
    sse1.abort();
    const sse1Outcome = await sse1.ready;
    check('R: sse1 was aborted by the client', sse1Outcome === 'aborted', sse1Outcome);
    check(
      'R: sse1 received a strict prefix of the item log before the abort',
      sse1Ids.length >= 2 && sse1Ids.every((value, index) => value === index + 1),
      sse1Ids,
    );
    check('R: the disconnect cursor is the last delivered seq', Number.isInteger(lastEventId) && lastEventId >= 1, lastEventId);

    // The turn keeps running without any SSE subscriber; items are the only
    // observation channel until the reconnect.
    const atDisconnect = await itemsPage(session.id, { limit: 500 });
    const sessionAtDisconnect = await api('GET', `/agents/sessions/${session.id}`);
    mark('R: items read while disconnected', {
      seqs: atDisconnect.items.map((item) => item.seq),
      kinds: atDisconnect.items.map((item) => summarizeItem(item).label),
      sessionState: sessionAtDisconnect.body?.state,
    });
    check('R: the turn is still running while disconnected', sessionAtDisconnect.body?.state === 'running', sessionAtDisconnect.body?.state);
    check(
      'R: items at disconnect match the persisted SSE prefix ending in the tool call',
      JSON.stringify(atDisconnect.items) === JSON.stringify(sse1Items)
        && atDisconnect.items.at(-1)?.itemId === toolCallEvent.data?.itemId,
      summarizeItems(atDisconnect.items),
    );
    check(
      'R: the turn had not finished when the SSE client left',
      atDisconnect.items.every((item) => item.payload?.complete !== true),
      summarizeItems(atDisconnect.items),
    );

    const samples = [];
    let finalPage = null;
    let sessionAfter;
    const deadline = Date.now() + TIMEOUT_MS;
    while (Date.now() < deadline) {
      const page = await itemsPage(session.id, { limit: 500 });
      const rows = page.items ?? [];
      const lastSeq = rows.at(-1)?.seq ?? 0;
      if (samples.at(-1)?.lastSeq !== lastSeq) {
        samples.push({ at: nowIso(), lastSeq, labels: rows.map((item) => summarizeItem(item).label) });
      }
      if (rows.some((item) => item.kind === 'message' && item.payload?.complete === true)) {
        // 完成行先落库，再等会话对外变回 idle。
        sessionAfter = await api('GET', `/agents/sessions/${session.id}`);
        if (sessionAfter.body?.state === 'idle') {
          finalPage = page;
          break;
        }
      }
      await sleep(100);
    }
    if (finalPage === null) throw new Error('R: the turn never completed while no SSE client was attached');
    const finalItems = finalPage.items ?? [];
    const N = finalItems.at(-1)?.seq ?? 0;
    mark('R: turn completed while disconnected', {
      lastSeq: N,
      sessionState: sessionAfter.body?.state,
      items: summarizeItems(finalItems),
    });
    check('R: the session is idle after the turn', sessionAfter.body?.state === 'idle', sessionAfter.body?.state);
    check('R: the items log ends with a completion item', finalItems.at(-1)?.payload?.complete === true, summarizeItem(finalItems.at(-1) ?? {}));
    check('R: the disconnect happened before the tool result', finalItems.some((item) => item.kind === 'tool_result' && item.seq > lastEventId), summarizeItems(finalItems));

    const toolResult = finalItems.find((item) => item.kind === 'tool_result');
    check(
      'R: the slow command ran to completion in the sandbox',
      typeof toolResult?.payload?.output === 'string' && toolResult.payload.output.includes('slow-done'),
      toolResult?.payload?.output,
    );
    check(
      'R: the tool result shows every tick',
      typeof toolResult?.payload?.output === 'string' &&
        ['tick-1', 'tick-2', 'tick-3', 'tick-4', 'tick-5', 'tick-6'].every((tick) => toolResult.payload.output.includes(tick)),
      toolResult?.payload?.output,
    );

    // Reconnect with the SSE cursor (Last-Event-ID).
    const sse2 = openSse(session.id, { lastEventId, label: 'sse2' });
    mark('R: sse2 opened with Last-Event-ID', { lastEventId });
    await waitForEvent(
      sse2,
      (event) => event.event === 'agent.session.turn.item.done' && event.data?.event_id === `evt_item_${N}_done`,
      TIMEOUT_MS,
      `sse2 to replay the official frames for the completion row at seq ${N}`,
    );
    sse2.abort();
    const sse2Outcome = await sse2.ready;
    const sse2Ids = [...sse2.state.itemIds];
    check('R: sse2 replayed the remaining events and was then aborted', sse2Outcome === 'aborted', sse2Outcome);
    check('R: sse2 starts at the first unseen seq', sse2Ids[0] === lastEventId + 1, { first: sse2Ids[0], expected: lastEventId + 1 });
    check(
      'R: sse2 ids are contiguous with no duplicate',
      sse2Ids.length === N - lastEventId && sse2Ids.every((value, index) => value === lastEventId + 1 + index),
      { ids: sse2Ids, expected: `[${lastEventId + 1}..${N}]` },
    );
    check(
      'R: the union of sse1 and sse2 covers every item exactly once',
      [...sse1.state.itemIds, ...sse2Ids].length === N &&
        [...sse1.state.itemIds, ...sse2Ids].every((value, index) => value === index + 1),
      { sse1: sse1.state.itemIds, sse2: sse2Ids, N },
    );
    const finalBySeq = new Map(finalItems.map((item) => [item.seq, item]));
    const sse2MatchesItems = sse2.state.events
      .filter((event) => event.event === 'item' && event.id !== null)
      .every((event) => {
        const item = finalBySeq.get(Number(event.id));
        return item !== undefined && JSON.stringify(item) === JSON.stringify(event.data);
      });
    check('R: replayed SSE payloads equal the persisted items', sse2MatchesItems);
    const replayTurnEvents = sse2.state.events.filter((event) => event.event === 'turn');
    summary.reconnect.sse1 = {
      status: sse1.state.status,
      itemIds: sse1.state.itemIds,
      events: sse1.state.events.map((event) => ({ event: event.event, id: event.id, seq: event.data?.seq })),
    };
    summary.reconnect.disconnect = {
      at: disconnectMark.at,
      lastEventId,
      lastSeq: lastEventId,
      itemsAtDisconnect: summarizeItems(atDisconnect.items),
      sessionStateAtDisconnect: sessionAtDisconnect.body?.state,
    };
    summary.reconnect.disconnectedSamples = samples;
    summary.reconnect.sse2 = {
      status: sse2.state.status,
      request: sse2.state.request,
      itemIds: sse2Ids,
      turnEvents: replayTurnEvents.length,
      events: sse2.state.events.map((event) => ({ event: event.event, id: event.id, seq: event.data?.seq, kind: event.data?.kind })),
    };
    summary.reconnect.itemsAfterCompletion = summarizeItems(finalItems);

    // Reconnect with the query cursor form.
    const sse3 = openSse(session.id, { afterSeq: N - 1, label: 'sse3' });
    await waitForEvent(
      sse3,
      (event) => event.event === 'item' && Number(event.id) === N,
      TIMEOUT_MS,
      'sse3 to replay the final item',
    );
    await sleep(150);
    sse3.abort();
    await sse3.ready;
    check(
      'R: afterSeq resumes at exactly the configured seq',
      JSON.stringify(sse3.state.itemIds) === JSON.stringify([N]),
      sse3.state.itemIds,
    );
    summary.reconnect.sse3 = {
      request: sse3.state.request,
      itemIds: sse3.state.itemIds,
      events: sse3.state.events.map((event) => ({ event: event.event, id: event.id, seq: event.data?.seq })),
    };

    // Items pagination across the reconnect cursor.
    const pages = [];
    let cursor = lastEventId;
    let union = [];
    for (let index = 0; index < 64; index += 1) {
      const page = await itemsPage(session.id, { afterSeq: cursor, limit: 2 });
      const seqs = page.items.map((item) => item.seq);
      pages.push({
        afterSeq: cursor,
        seqs,
        nextCursor: page.nextCursor,
        hasMore: page.hasMore,
        lastItemSeq: page.lastItemSeq,
      });
      union = union.concat(seqs);
      if (page.hasMore !== true) break;
      check(
        `R: page ${index} nextCursor is its last seq`,
        page.nextCursor === seqs.at(-1),
        { nextCursor: page.nextCursor, seqs },
      );
      cursor = page.nextCursor;
    }
    check(
      'R: paging from the disconnect cursor covers every remaining item once',
      union.length === N - lastEventId && union.every((value, index) => value === lastEventId + 1 + index),
      { union, expected: `[${lastEventId + 1}..${N}]` },
    );
    check('R: the final page reports hasMore=false', pages.at(-1)?.hasMore === false, pages.at(-1));
    check('R: the final page has no nextCursor', pages.at(-1)?.nextCursor === null, pages.at(-1));
    check('R: lastItemSeq is the final seq on every page', pages.every((page) => page.lastItemSeq === N), pages.map((page) => page.lastItemSeq));
    summary.reconnect.itemsPaging = { pages, union, lastItemSeq: N };

    const afterItems = await itemsPage(session.id, { afterSeq: lastEventId, limit: 500 });
    check(
      'R: items after the disconnect cursor equal the replayed seqs',
      JSON.stringify(afterItems.items.map((item) => item.seq)) === JSON.stringify(sse2Ids),
      { items: afterItems.items.map((item) => item.seq), sse2: sse2Ids },
    );

    // 新 turn 只持久化一条 assistant 全文完成行；重连按这条行重建一条全文 delta。
    const assistantRows = (items) => items.filter((item) => item.kind === 'message' && item.payload?.role === 'assistant');
    const messages = assistantRows(finalItems);
    const [complete] = messages;
    const completeText = complete?.payload?.text;
    check(
      'R: the only persisted assistant row is a full-text completion',
      messages.length === 1 && complete?.payload?.complete === true && (complete?.payload?.delta ?? false) === false
        && typeof completeText === 'string' && completeText.length > 0,
      summarizeItems(messages),
    );
    const reread = assistantRows(await allItems(session.id));
    check('R: the completion item id is stable across reads', reread.length === 1 && reread[0].itemId === complete?.itemId,
      { first: complete?.itemId, second: reread[0]?.itemId });
    const replayDeltas = sse2.state.events
      .filter((event) => event.event === 'agent.session.turn.output_text.delta')
      .map((event) => event.data);
    check(
      'R: reconnect synthesized one full-text delta for the persisted item',
      replayDeltas.length === 1 && replayDeltas[0]?.item_id === complete?.itemId && replayDeltas[0]?.delta === completeText,
      replayDeltas,
    );

    const statsAfterReconnect = await stubStats();
    const reconnectTurns = (statsAfterReconnect.turns ?? []).filter((turn) => turn.directive === RECONNECT_DIRECTIVE);
    const reconnectFinal = reconnectTurns.find((turn) => turn.kind === 'final');
    mark('R: model stub reply observed', { reply: reconnectFinal?.reply, mainCalls: statsAfterReconnect.mainCalls });
    check('R: the stub ran exactly one tool turn', reconnectTurns.filter((turn) => turn.kind === 'tool_call').length === 1, reconnectTurns);
    check('R: the stub ran exactly one final turn', reconnectTurns.filter((turn) => turn.kind === 'final').length === 1, reconnectTurns);
    check('R: persisted full text equals the stub reply', reconnectFinal?.reply === completeText, { stub: reconnectFinal?.reply, completeText });
    summary.reconnect.message = {
      itemId: complete?.itemId ?? null,
      completeText: completeText ?? null,
      replayDeltaCount: replayDeltas.length,
      stubReply: reconnectFinal?.reply ?? null,
    };
    summary.reconnect.stub = {
      mainCalls: statsAfterReconnect.mainCalls,
      toolCalls: statsAfterReconnect.toolCalls,
      turns: reconnectTurns,
    };

    // ------------------------------------------------------ idempotency ----
    const idem = await createSession({ type: 'none' }, 'I');
    const idemSession = idem.created;
    summary.idempotency = {
      createMs: idem.createMs,
      sessionId: idemSession.id,
      logicalId: idemSession.logicalId,
      environment: idemSession.environment,
      state: idemSession.state,
      lastItemSeq: idemSession.lastItemSeq,
    };
    check('I: create reports environment.type=none', idemSession.environment?.type === 'none', idemSession.environment);
    check('I: create left the session idle', idemSession.state === 'idle', idemSession.state);

    const idemKey = crypto.randomUUID();
    const idemText = `S9a3b2 idempotency probe inputId=${idemKey}`;
    const idemBaseline = {
      stub: await stubStats(),
      items: await allItems(idemSession.id),
      at: nowIso(),
    };
    check('I: the session starts with no items', idemBaseline.items.length === 0, idemBaseline.items.length);

    const concurrentStart = Date.now();
    const first = startEvent(idemSession.id, idemKey, idemText);
    const second = startEvent(idemSession.id, idemKey, idemText);
    mark('I: two concurrent POST /events with one Idempotency-Key', { idempotencyKey: idemKey });
    const [firstResponse, secondResponse] = await Promise.all([first.promise, second.promise]);
    const dispositions = [firstResponse.body?.disposition, secondResponse.body?.disposition].sort();
    mark('I: both concurrent responses returned', {
      first: { status: firstResponse.status, disposition: firstResponse.body?.disposition, lastItemSeq: firstResponse.body?.lastItemSeq },
      second: { status: secondResponse.status, disposition: secondResponse.body?.disposition, lastItemSeq: secondResponse.body?.lastItemSeq },
    });
    check(
      'I: both concurrent submissions return 202',
      firstResponse.status === 202 && secondResponse.status === 202,
      { first: firstResponse.status, second: secondResponse.status },
    );
    check(
      'I: both concurrent submissions are accepted',
      firstResponse.body?.accepted === true && secondResponse.body?.accepted === true,
      { first: firstResponse.body, second: secondResponse.body },
    );
    check(
      'I: both responses echo the Idempotency-Key as inputId',
      firstResponse.body?.inputId === idemKey && secondResponse.body?.inputId === idemKey,
      { first: firstResponse.body?.inputId, second: secondResponse.body?.inputId, idemKey },
    );
    check(
      'I: exactly one submission was inserted and one was existing',
      JSON.stringify(dispositions) === JSON.stringify(['existing', 'inserted']),
      { first: firstResponse.body?.disposition, second: secondResponse.body?.disposition },
    );

    const afterPairItems = await allItems(idemSession.id);
    const afterPairStats = await stubStats();
    const pairCompletes = afterPairItems.filter((item) => item.payload?.complete === true);
    const pairFinals = (afterPairStats.turns ?? []).filter(
      (turn) => turn.kind === 'final' && String(turn.lastUserText ?? '').includes(idemKey),
    );
    check('I: the pair produced exactly one completion item', pairCompletes.length === 1, summarizeItems(afterPairItems));
    check(
      'I: the pair produced exactly one model final call',
      afterPairStats.mainCalls === idemBaseline.stub.mainCalls + 1,
      { before: idemBaseline.stub.mainCalls, after: afterPairStats.mainCalls },
    );
    check('I: the pair produced exactly one stub turn for this text', pairFinals.length === 1, pairFinals);

    const conflictText = `${idemText} different`;
    const conflict = await api('POST', `/agents/sessions/${idemSession.id}/events`, {
      body: { kind: 'user', mode: 'turn', text: conflictText },
      headers: { 'idempotency-key': idemKey },
    });
    const afterConflictItems = await allItems(idemSession.id);
    const afterConflictStats = await stubStats();
    mark('I: same key with different content rejected', {
      status: conflict.status,
      code: conflict.body?.error?.code,
    });
    check('I: same key + different content returns 409', conflict.status === 409, conflict);
    check('I: the conflict names input_conflict', conflict.body?.error?.code === 'input_conflict', conflict.body);
    check(
      'I: the conflicted input was never enqueued (no new items)',
      JSON.stringify(afterConflictItems.map((item) => item.seq)) === JSON.stringify(afterPairItems.map((item) => item.seq)),
      { before: afterPairItems.map((item) => item.seq), after: afterConflictItems.map((item) => item.seq) },
    );
    check(
      'I: the conflicted input caused no model call',
      afterConflictStats.mainCalls === afterPairStats.mainCalls,
      { before: afterPairStats.mainCalls, after: afterConflictStats.mainCalls },
    );
    check(
      'I: the conflicted input left the session idle',
      (await api('GET', `/agents/sessions/${idemSession.id}`)).body?.state === 'idle',
      (await api('GET', `/agents/sessions/${idemSession.id}`)).body?.state,
    );

    const replay = await api('POST', `/agents/sessions/${idemSession.id}/events`, {
      body: { kind: 'user', mode: 'turn', text: idemText },
      headers: { 'idempotency-key': idemKey },
    });
    const afterReplayItems = await allItems(idemSession.id);
    const afterReplayStats = await stubStats();
    mark('I: completed turn replayed with the same key', {
      status: replay.status,
      disposition: replay.body?.disposition,
      lastItemSeq: replay.body?.lastItemSeq,
    });
    check('I: replay after completion returns 202', replay.status === 202, replay);
    check('I: replay after completion is accepted', replay.body?.accepted === true, replay.body);
    check('I: replay reports disposition=existing', replay.body?.disposition === 'existing', replay.body);
    check('I: replay echoes the same inputId', replay.body?.inputId === idemKey, replay.body);
    check(
      'I: replay reports the completed log length',
      replay.body?.lastItemSeq === afterPairItems.at(-1)?.seq,
      { replay: replay.body?.lastItemSeq, items: afterPairItems.at(-1)?.seq },
    );
    check(
      'I: replay created no items',
      JSON.stringify(afterReplayItems.map((item) => item.seq)) === JSON.stringify(afterPairItems.map((item) => item.seq)),
      { before: afterPairItems.map((item) => item.seq), after: afterReplayItems.map((item) => item.seq) },
    );
    check(
      'I: replay caused no model call',
      afterReplayStats.mainCalls === afterPairStats.mainCalls,
      { before: afterPairStats.mainCalls, after: afterReplayStats.mainCalls },
    );

    const idemFinalView = await api('GET', `/agents/sessions/${idemSession.id}`);
    check('I: the session is idle after the replays', idemFinalView.body?.state === 'idle', idemFinalView.body?.state);

    const receiptRows = await hranaQuery(
      'SELECT input_id, state, fingerprint FROM swarm_inputs WHERE logical_id = ? ORDER BY input_id',
      [idemSession.logicalId],
    );
    const itemRows = await hranaQuery('SELECT COUNT(*) AS n FROM swarm_items WHERE logical_id = ?', [
      idemSession.logicalId,
    ]);
    summary.idempotency.db = { receiptRows, itemRows };
    if (receiptRows.rows !== undefined) {
      check('I: the state store holds exactly one input receipt', receiptRows.rows.length === 1, receiptRows.rows);
      check('I: the receipt is keyed by the Idempotency-Key', receiptRows.rows[0]?.[0] === idemKey, receiptRows.rows);
      check('I: the receipt state is accepted', receiptRows.rows[0]?.[1] === 'accepted', receiptRows.rows);
    } else {
      check(`I: DB receipt probe unavailable (${receiptRows.skipped ?? receiptRows.error})`, false, receiptRows);
    }
    if (itemRows.rows !== undefined) {
      check(
        'I: the state store item count equals the item API count',
        Number(itemRows.rows[0]?.[0]) === afterReplayItems.length,
        { db: itemRows.rows[0]?.[0], api: afterReplayItems.length },
      );
    } else {
      check(`I: DB item probe unavailable (${itemRows.skipped ?? itemRows.error})`, false, itemRows);
    }
    summary.idempotency.concurrent = {
      idempotencyKey: idemKey,
      text: idemText,
      elapsedMs: Date.now() - concurrentStart,
      first: { status: firstResponse.status, body: firstResponse.body, elapsedMs: firstResponse.elapsedMs },
      second: { status: secondResponse.status, body: secondResponse.body, elapsedMs: secondResponse.elapsedMs },
    };
    summary.idempotency.baseline = {
      stubMainCalls: idemBaseline.stub.mainCalls,
      itemCount: idemBaseline.items.length,
      at: idemBaseline.at,
    };
    summary.idempotency.items = {
      afterPair: summarizeItems(afterPairItems),
      afterConflictSeq: afterConflictItems.map((item) => item.seq),
      afterReplaySeq: afterReplayItems.map((item) => item.seq),
    };
    summary.idempotency.conflict = { status: conflict.status, body: conflict.body };
    summary.idempotency.replay = { status: replay.status, body: replay.body };
    summary.idempotency.stub = {
      before: idemBaseline.stub.mainCalls,
      afterPair: afterPairStats.mainCalls,
      afterConflict: afterConflictStats.mainCalls,
      afterReplay: afterReplayStats.mainCalls,
      finalTurns: pairFinals,
    };
    summary.idempotency.finalState = idemFinalView.body?.state;

    // 使用空闲会话的 cancel 探针，且不改变上面的 UUID 收据和模型调用断言。
    const opaqueKey = `cancel-retry/${crypto.randomUUID()}`;
    const postOpaque = () => api('POST', `/agents/sessions/${idemSession.id}/events`, {
      body: { events: [{ type: 'agent.session.input.cancel' }] },
      headers: { 'idempotency-key': opaqueKey },
    });
    const opaqueFirst = await postOpaque();
    const opaqueRetry = await postOpaque();
    const opaqueId = opaqueFirst.body?.inputId;
    check('I: opaque key inserts once and replays as existing', opaqueFirst.status === 202
      && opaqueFirst.body?.disposition === 'inserted' && opaqueRetry.status === 202
      && opaqueRetry.body?.disposition === 'existing', { first: opaqueFirst.status, retry: opaqueRetry.status });
    check('I: opaque key yields a stable UUIDv5 instead of echoing the raw key', typeof opaqueId === 'string'
      && /^[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(opaqueId)
      && opaqueRetry.body?.inputId === opaqueId, opaqueId);
    const afterOpaque = await allItems(idemSession.id);
    check('I: opaque-key cancel and retry add no items', afterOpaque.length === afterReplayItems.length,
      { before: afterReplayItems.length, after: afterOpaque.length });

    // -------------------------------------------------------- teardown ----
  } catch (error) {
    failure = error instanceof Error ? error.stack ?? error.message : String(error);
    mark('driver: aborted by an exception', { message: error instanceof Error ? error.message : String(error) });
  } finally {
    const teardown = {
      envs: [],
      rootBeforeCreate: summary.baseline.sandboxDirectories,
      bindingsBaseline: summary.baseline.bindings,
      bindingsPreTeardown: bindingsSnapshot(),
    };
    const deleted = new Set();
    for (const { label, envId } of envs) {
      if (deleted.has(envId)) continue;
      deleted.add(envId);
      try {
        const record = await deleteEnvironment(label, envId);
        teardown.envs.push(record);
        check(`teardown ${label}: DELETE returned 200`, record.deleteStatus === 200, record);
        check(`teardown ${label}: GET after DELETE is 404`, record.afterDeleteStatus === 404, record);
        check(`teardown ${label}: sandbox directory removed`, record.sandboxDirectoryExists === false, record);
        check(`teardown ${label}: no SRT process remains`, record.processGroupCount === 0, record);
      } catch (error) {
        teardown.envs.push({ label, envId, error: error instanceof Error ? error.message : String(error) });
        check(`teardown ${label}: completed`, false, teardown.envs.at(-1));
      }
    }
    teardown.rootAfter = rootDirectories();
    teardown.bindingsAfter = bindingsSnapshot();
    summary.teardown = teardown;
    check(
      'teardown: the sandbox root returns to its baseline',
      JSON.stringify(teardown.rootAfter) === JSON.stringify(summary.baseline.sandboxDirectories),
      { before: summary.baseline.sandboxDirectories, after: teardown.rootAfter },
    );
    check(
      'teardown: every binding created by this run is gone',
      Object.keys(teardown.bindingsAfter.entries).every((envId) => !envs.some((entry) => entry.envId === envId)),
      teardown.bindingsAfter.entries,
    );

    summary.checks = checks;
    summary.checkCount = checks.length;
    const failed = failedChecks();
    summary.failedChecks = failed;
    summary.finishedAt = nowIso();
    const payload = failure === null && failed.length === 0 ? summary : { ...summary, failure: failure ?? `failed checks: ${failed.join('; ')}` };
    const serialized = JSON.stringify(payload, null, 2);
    if (OUT_PATH) fs.writeFileSync(OUT_PATH, `${serialized}\n`);
    // Explicit exit so keep-alive sockets cannot keep a finished run alive.
    process.stdout.write(`${serialized}\n`, () => {
      process.exit(failure === null && failed.length === 0 ? 0 : 1);
    });
  }
}

main().catch((error) => {
  console.error(`[s9a3b2] fatal: ${error instanceof Error ? error.stack ?? error.message : String(error)}`);
  process.exit(1);
});
