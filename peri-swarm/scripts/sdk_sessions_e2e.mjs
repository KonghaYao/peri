#!/usr/bin/env node
// S9a1 end-to-end driver: the official `openai@7.60.0` Agents API against the
// local `peri-swarm` gateway (real Loom session/state databases, real Rivet
// engine, local deterministic model stub).
//
// Covered official methods:
//   1. create   POST /v1/agents/sessions?stream=true   client.beta.agents.sessions.create
//   2. get      GET  /v1/agents/sessions/{id}          client.beta.agents.sessions.retrieve
//   3. event    POST /v1/agents/sessions/{id}/events   client.beta.agents.sessions.events.create
//   4. events   GET  /v1/agents/sessions/{id}/events   client.beta.agents.sessions.events.stream
//   5. items    GET  /v1/agents/sessions/{id}/items    client.beta.agents.sessions.items.list
//   6. turns    GET  /v1/agents/sessions/{id}/turns    client.beta.agents.sessions.turns.list
//
// The follow-up uses one official message event with an Idempotency-Key. The
// SDK's asResponse() exposes the HTTP 202 and the gateway receipt. A replay
// with the same key must leave both the item log and model call count stable.
// The answer must recall the code supplied only on the first turn. An SDK
// reconnect with Last-Event-ID then reads the follow-up from a saved item seq.
//
// Official event coverage asserted in the create stream and the follow-up
// stream: `agent.session.created` first frame, the
// `session.in_progress` -> `turn.created` -> `turn.in_progress` start order,
// assistant `turn.item.done` with `phase=final_answer` and non-empty
// `output_text`, `turn.completed`, `agent.session.idle`. A deterministic
// `agent.session.input.cancel` probe against a slow stub stream asserts that the
// cancellation request is accepted and the stream still reaches a terminal
// without throwing. Cancellation is folded into
// `turn.completed(status="cancelled")` by contract; the real host behavior is
// recorded under `differences` instead of being asserted as cancelled.
//
// Requirements (no dependency is vendored in this repository):
//   npm install --prefix /tmp/peri-s9a1-sdk openai@7.60.0
//   NODE_PATH=/tmp/peri-s9a1-sdk/node_modules node peri-swarm/scripts/sdk_sessions_e2e.mjs
//
// Environment:
//   PERI_SWARM_BASE_URL   default http://127.0.0.1:8080/v1
//   PERI_SWARM_API_KEY    deployment bearer key (required)
//   PERI_SWARM_MODEL      default stub-model
//   PERI_MODEL_STUB_URL   default http://127.0.0.1:9496
//   PERI_E2E_TIMEOUT_MS   default 180000
//   PERI_E2E_OUTPUT_PATH  optional: write the run JSON to this path
//   PERI_E2E_RAW_PATH     optional: write every raw SSE frame to this path
//   PERI_E2E_CONTRACT_SELF_TEST
//                         `clean` exits 0, `gap` exits 1; proves the raw-gap
//                         assertion fails the subprocess without a live stack
//
// Exit code 0 only when every assertion in `assertions` passes. The JSON
// summary on stdout is the evidence record; no credential is ever printed.
// Observed gaps (for example `usage` is always null) are recorded under
// `differences` and never asserted as correct.

import crypto from 'node:crypto';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { isDeepStrictEqual } from 'node:util';

const require = createRequire(import.meta.url);
const OpenAI = require('openai');

const BASE_URL = process.env.PERI_SWARM_BASE_URL ?? 'http://127.0.0.1:8080/v1';
const API_KEY = process.env.PERI_SWARM_API_KEY ?? '';
const MODEL = process.env.PERI_SWARM_MODEL ?? 'stub-model';
const STUB_URL = process.env.PERI_MODEL_STUB_URL ?? 'http://127.0.0.1:9496';
const TIMEOUT_MS = Number(process.env.PERI_E2E_TIMEOUT_MS ?? '180000');
const CONTRACT_HEADERS = { 'x-peri-swarm-expect': '1', 'OpenAI-Beta': 'agents=v1' };
const SCRIPT_PATH = fileURLToPath(import.meta.url);
const REPO_ROOT = fileURLToPath(new URL('../..', import.meta.url));

function scriptSha256() {
  return crypto.createHash('sha256').update(fs.readFileSync(SCRIPT_PATH)).digest('hex');
}

function gitParentCommit() {
  try {
    return execFileSync('git', ['rev-parse', 'HEAD'], {
      cwd: REPO_ROOT,
      encoding: 'utf8',
    }).trim();
  } catch {
    return null;
  }
}

function sdkVersion() {
  const entry = require.resolve('openai');
  const raw = fs.readFileSync(path.join(path.dirname(entry), 'package.json'), 'utf8');
  return JSON.parse(raw).version;
}

const checks = [];
function check(name, condition, detail) {
  checks.push({ name, ok: Boolean(condition), ...(detail === undefined ? {} : { detail }) });
  if (!condition) throw new Error(`assertion failed: ${name}${detail ? ` (${JSON.stringify(detail)})` : ''}`);
}

// Recorded differences are observations about known gaps. They are evidence,
// not passing assertions: the script must not claim the gap is correct.
const differences = [];
function recordDifference(name, observed, note) {
  differences.push({ name, observed, note, disposition: '记录差异（不视为通过）' });
}

// A gap frame has no `type`: the gateway sends `{fromSeq, toSeq}`. A frame with
// either boundary field is a gap and must be checked on the raw stream, before
// any per-turn filtering can drop it.
function isGapFrame(event) {
  return event?.type === 'gap'
    || typeof event?.fromSeq === 'number'
    || typeof event?.toSeq === 'number';
}

function assertNoRawGap(events, label) {
  const gaps = events.filter(isGapFrame);
  check(`${label}: raw frames carried no gap event`, gaps.length === 0, {
    gapFrames: gaps,
    rawEventTypes: countFrames(events),
  });
  return gaps;
}

function rawFrameSample(events, limit = 10) {
  return events.slice(0, limit).map((event, index) => ({
    index,
    label: frameLabel(event),
    type: event?.type ?? null,
    eventId: event?.event_id ?? null,
    turnId: isOfficialEvent(event) ? officialEventTurnId(event) : null,
    seq: event?.seq ?? null,
    state: event?.state ?? null,
    fromSeq: event?.fromSeq ?? null,
    toSeq: event?.toSeq ?? null,
  }));
}

function sleep(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

async function stubStats() {
  const response = await fetch(`${STUB_URL}/__stats`);
  if (!response.ok) throw new Error(`model stub /__stats returned ${response.status}`);
  return response.json();
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
    phase: item.payload?.phase,
    text: typeof item.payload?.text === 'string' ? item.payload.text : undefined,
  }));
}

// SSE frames mix the gateway's own `item`/`turn` events with the official
// `agent.session.*` family and the official `error` frame. Classify by shape:
// official events always carry a `type`, the gateway frames do not.
function isOfficialEvent(event) {
  return typeof event?.type === 'string'
    && (event.type.startsWith('agent.session.') || event.type === 'error');
}

function isItemFrame(event) {
  return typeof event?.itemId === 'string' && typeof event?.seq === 'number';
}

function isTurnFrame(event) {
  return typeof event?.state === 'string' && typeof event?.requestId === 'string';
}

function isSessionFrame(event) {
  return event?.object === 'agent.session'
    && typeof event?.logicalId === 'string'
    && typeof event?.created === 'boolean';
}

function isHeartbeat(event) {
  return typeof event?.lastItemSeq === 'number'
    && event?.itemId === undefined
    && event?.state === undefined
    && event?.type === undefined;
}

function frameLabel(event) {
  if (isGapFrame(event)) return 'gap';
  if (isOfficialEvent(event)) return event.type;
  if (isItemFrame(event)) return 'gateway.item';
  if (isTurnFrame(event)) return `gateway.turn.${event.state}`;
  if (isSessionFrame(event)) return 'gateway.session';
  if (isHeartbeat(event)) return 'heartbeat';
  return 'unknown';
}

function countFrames(events) {
  const counts = {};
  for (const event of events) {
    const label = frameLabel(event);
    counts[label] = (counts[label] ?? 0) + 1;
  }
  return counts;
}

function officialEvents(events) {
  return events.filter(isOfficialEvent);
}

function officialTypes(events) {
  return officialEvents(events).map((event) => event.type);
}

function findOfficial(events, type) {
  return events.find((event) => event?.type === type);
}

function officialIndex(events, type) {
  return events.findIndex((event) => event?.type === type);
}

/**
 * Turn identity carried by one official frame.
 *
 * `turn.created`/`item.*`/`turn.completed` carry `turn.id` or `turn_id`.
 * `session.in_progress`/`idle`/`failed` and `error` carry only an `event_id`
 * derived from the turn id, so recover it from there.
 */
function officialEventTurnId(event) {
  if (!isOfficialEvent(event)) return null;
  // `agent.session.created` is the session identity, not a turn. Its event id
  // ends in `_session_created`, which would otherwise be misread as `_created`.
  if (event.type === 'agent.session.created') return null;
  if (typeof event.turn_id === 'string' && event.turn_id.length > 0) return event.turn_id;
  if (typeof event.turn?.id === 'string' && event.turn.id.length > 0) return event.turn.id;
  if (typeof event.event_id !== 'string') return null;
  // Check the longer `session_*` suffixes first: `evt_<turn>_session_in_progress`
  // also ends with `_in_progress`, and a greedy regex would capture `_session`.
  for (const suffix of [
    '_session_in_progress',
    '_session_failed',
    '_created',
    '_in_progress',
    '_completed',
    '_failed',
    '_idle',
    '_error',
  ]) {
    if (event.event_id.startsWith('evt_') && event.event_id.endsWith(suffix)) {
      return event.event_id.slice('evt_'.length, -suffix.length);
    }
  }
  return null;
}

/**
 * Whether one SSE frame belongs to the turn this driver just submitted.
 *
 * Explicit cursor subscriptions can replay another turn's terminal before
 * the new turn starts. Read the turn id from each frame.
 */
function eventBelongsToTurn(event, turnId, firstItemSeq) {
  if (isItemFrame(event)) {
    return typeof firstItemSeq === 'number' && event.seq >= firstItemSeq;
  }
  if (isTurnFrame(event)) return event.turnId === turnId;
  if (isOfficialEvent(event)) return officialEventTurnId(event) === turnId;
  return false;
}

function turnTerminalReached(event, turnId) {
  if (event?.type !== 'agent.session.idle' && event?.type !== 'agent.session.failed') {
    return false;
  }
  // Use the terminal frame's own derived turn identity. Scanning all collected
  // events would let a replayed terminal from the previous turn end the stream
  // before the submitted turn starts.
  return officialEventTurnId(event) === turnId;
}

function outputTextOf(item) {
  return (item?.content ?? [])
    .filter((part) => part?.type === 'output_text')
    .map((part) => part.text ?? '')
    .join('');
}

function itemText(item) {
  return (item?.content ?? []).map((part) => part?.text ?? '').join('');
}

function assistantDoneEvents(events) {
  return events.filter(
    (event) => event?.type === 'agent.session.turn.item.done' && event.item?.role === 'assistant',
  );
}

/**
 * Consume one SSE stream until `terminal(event)`.
 *
 * The iteration promise settles only after the async iterator has been closed,
 * so a later `AgentSessionCreateStream.finalResult()` observes the collected
 * events. A timeout aborts the local request and returns `outcome=timeout`
 * without claiming a terminal was observed.
 */
async function collectStream(stream, terminal, timeoutMs, onEvent) {
  const events = [];
  let error = null;
  let timer;
  const timedOut = new Promise((resolve) => {
    timer = setTimeout(() => resolve('timeout'), timeoutMs);
  });
  try {
    const iteration = (async () => {
      try {
        for await (const event of stream) {
          events.push(event);
          onEvent?.(event, events);
          if (terminal(event, events)) return 'terminal';
        }
        return 'closed';
      } catch (streamError) {
        error = streamError;
        return 'error';
      }
    })();
    const outcome = await Promise.race([iteration, timedOut]);
    if (outcome === 'timeout') {
      try {
        stream.controller.abort();
      } catch {
        // best effort: the local request may already be closed
      }
    }
    return { events, outcome, error };
  } finally {
    clearTimeout(timer);
  }
}

/**
 * Poll `sessions.retrieve` until the projection settles.
 *
 * `agent.session.idle` is broadcast before the in-process turn lease is
 * dropped, so a retrieve issued immediately after the terminal can still see
 * `status: in_progress` while the persisted `state` is already `idle`. Polling
 * records that window instead of hiding it or asserting a lucky timing.
 */
async function waitForSession(client, sessionId, predicate, timeoutMs = 5000) {
  const started = Date.now();
  const observed = [];
  for (;;) {
    const session = await client.beta.agents.sessions.retrieve(sessionId, {
      headers: CONTRACT_HEADERS,
    });
    observed.push({
      atMs: Date.now() - started,
      state: session.state,
      status: session.status,
      lastItemSeq: session.lastItemSeq,
    });
    if (predicate(session)) {
      return { session, settleMs: Date.now() - started, observed };
    }
    if (Date.now() - started >= timeoutMs) {
      return { session, settleMs: null, observed };
    }
    await sleep(50);
  }
}

function assertTurnStartOrder(events, label) {
  const types = officialTypes(events);
  const sessionIndex = officialIndex(events, 'agent.session.in_progress');
  const createdIndex = officialIndex(events, 'agent.session.turn.created');
  const inProgressIndex = officialIndex(events, 'agent.session.turn.in_progress');
  check(
    `${label}: official turn start opens with session.in_progress`,
    sessionIndex >= 0 && createdIndex >= 0 && inProgressIndex >= 0,
    types,
  );
  check(
    `${label}: session.in_progress precedes turn.created`,
    sessionIndex < createdIndex,
    { sessionIndex, createdIndex, types },
  );
  check(
    `${label}: turn.created precedes turn.in_progress`,
    createdIndex < inProgressIndex,
    { createdIndex, inProgressIndex, types },
  );
}

function assertTurnIdentity(events, label) {
  const turnCreated = findOfficial(events, 'agent.session.turn.created');
  const turnId = turnCreated?.turn?.id;
  check(`${label}: turn.created carries a turn id`, typeof turnId === 'string' && turnId.length > 0, turnCreated);
  // `agent.session.created` is a session frame, not a turn frame. Every other
  // official frame must name the same turn; frames with no recoverable turn id
  // are a failure, not a silent skip.
  const named = officialEvents(events).filter((event) => event.type !== 'agent.session.created');
  const anonymous = named.filter((event) => officialEventTurnId(event) === null);
  check(
    `${label}: every official turn frame names a turn`,
    anonymous.length === 0,
    anonymous.map((event) => event.type),
  );
  const mismatched = named.filter((event) => officialEventTurnId(event) !== turnId);
  check(
    `${label}: every official turn frame shares one turn id`,
    mismatched.length === 0,
    mismatched.map((event) => ({ type: event.type, turnId: officialEventTurnId(event) })),
  );
  return turnId;
}

/// Terminal frames are the ones that end the stream. Assert their own payload
/// carries the expected turn id instead of relying on any earlier frame.
function assertTerminalTurnIds(events, turnId, label) {
  const terminalTypes = [
    'agent.session.turn.completed',
    'agent.session.turn.failed',
    'agent.session.idle',
    'agent.session.failed',
    'error',
  ];
  const observed = [];
  for (const type of terminalTypes) {
    for (const event of events.filter((candidate) => candidate?.type === type)) {
      const observedTurnId = officialEventTurnId(event);
      observed.push({ type, turnId: observedTurnId, eventId: event.event_id ?? null });
      check(
        `${label}: ${type} carries the expected turn id`,
        typeof observedTurnId === 'string' && observedTurnId === turnId,
        { expected: turnId, observed: observedTurnId, eventId: event.event_id ?? null },
      );
    }
  }
  check(`${label}: at least one terminal frame names the turn`, observed.length > 0, observed);
  return observed;
}

function assertAssistantFinal(events, label) {
  const done = assistantDoneEvents(events);
  check(`${label}: exactly one assistant turn.item.done`, done.length === 1, done.map((event) => event.item));
  const item = done[0]?.item;
  check(`${label}: assistant item.done phase is final_answer`, item?.phase === 'final_answer', item);
  check(`${label}: assistant item.done status is completed`, item?.status === 'completed', item?.status);
  check(`${label}: assistant item.done output_index is 0`, done[0]?.output_index === 0, done[0]?.output_index);
  const text = outputTextOf(item);
  check(`${label}: assistant item.done carries non-empty output_text`, text.length > 0, item);
  const outputDone = findOfficial(events, 'agent.session.turn.output_text.done');
  check(`${label}: output_text.done matches the item text`, outputDone?.text === text, {
    outputDone: outputDone?.text,
    item: text,
  });
  return { event: done[0], item, text };
}

function assertOfficialItems(body, label) {
  const rows = body?.items ?? [];
  const data = body?.data ?? [];
  const lastSeq = new Map();
  for (const row of rows) {
    lastSeq.set(row.itemId, Math.max(lastSeq.get(row.itemId) ?? 0, row.seq));
  }
  const ids = data.map((item) => item.id);
  const idsInRawPage = ids.filter((id) => lastSeq.has(id));
  check(`${label}: data contains distinct item ids from the raw page`,
    new Set(ids).size === data.length && ids.every((id) => lastSeq.has(id)),
    { ids, rawItems: lastSeq.size });
  const groupMaxima = [...lastSeq.values()];
  const tailSeq = body?.order === 'desc' ? Math.min(...groupMaxima) : Math.max(...groupMaxima);
  check(`${label}: nextCursor is the last group's maximum seq when more items exist`,
    body?.has_more ? Number.isFinite(tailSeq) && body.nextCursor === tailSeq : body?.nextCursor === null,
    { nextCursor: body?.nextCursor, tailSeq, hasMore: body?.has_more });
  for (const item of data) {
    const outputStatuses = ['in_progress', 'completed', 'incomplete'];
    const callStatuses = [...outputStatuses, 'failed'];
    let fieldsMatch = false;
    switch (item.type) {
      case 'message':
        fieldsMatch = ['user', 'assistant'].includes(item.role)
          && outputStatuses.includes(item.status)
          && (item.role === 'user' ? item.phase === null : [null, 'commentary', 'final_answer'].includes(item.phase))
          && Array.isArray(item.content) && item.content.length === 1
          && item.content[0].type === (item.role === 'user' ? 'input_text' : 'output_text')
          && typeof item.content[0].text === 'string';
        break;
      case 'reasoning':
        fieldsMatch = item.status === null && Array.isArray(item.summary) && item.summary.length === 1
          && item.summary[0].type === 'summary_text' && typeof item.summary[0].text === 'string';
        break;
      case 'function_call':
        fieldsMatch = typeof item.call_id === 'string' && typeof item.name === 'string'
          && 'arguments' in item && callStatuses.includes(item.status);
        break;
      case 'function_call_output':
        fieldsMatch = typeof item.call_id === 'string' && callStatuses.includes(item.status)
          && (item.error === null || typeof item.error === 'string')
          && (item.output === null || typeof item.output === 'string');
        break;
    }
    check(`${label}: official item fields`, typeof item.id === 'string' && typeof item.turn_id === 'string'
      && !('seq' in item) && fieldsMatch, { id: item.id, type: item.type });
  }
  if (idsInRawPage.length === ids.length) {
    const seqs = ids.map((id) => lastSeq.get(id));
    const ordered = body?.order === 'desc'
      ? seqs.every((seq, index) => index === 0 || seq < seqs[index - 1])
      : seqs.every((seq, index) => index === 0 || seq > seqs[index - 1]);
    check(`${label}: official items follow the requested order`, ordered, { order: body?.order, ids });
  }
  return { data, rawItemCount: lastSeq.size };
}

let summary = null;

async function main() {
  if (!API_KEY) {
    console.error('PERI_SWARM_API_KEY is required');
    process.exit(2);
  }
  const version = sdkVersion();
  const scriptShaAtStart = scriptSha256();
  const parentCommitAtStart = gitParentCommit();
  summary = {
    sdk: { package: 'openai', version },
    baseUrl: BASE_URL,
    model: MODEL,
    stubUrl: STUB_URL,
    startedAt: new Date().toISOString(),
    timings: {},
    script: {
      path: SCRIPT_PATH,
      sha256AtStart: scriptShaAtStart,
      sha256AtEnd: null,
      sha256Stable: null,
    },
    git: {
      parentCommitAtStart,
      parentCommitAtEnd: null,
      parentCommitStable: null,
    },
    evidence: {
      outputPath: process.env.PERI_E2E_OUTPUT_PATH ?? null,
      rawFramesPath: process.env.PERI_E2E_RAW_PATH ?? null,
    },
  };

  const clientOptions = {
    baseURL: BASE_URL,
    apiKey: API_KEY,
    maxRetries: 0,
    timeout: TIMEOUT_MS,
  };
  const client = new OpenAI(clientOptions);

  // A fresh stub counter makes the deterministic reply index repeatable.
  const reset = await fetch(`${STUB_URL}/__reset`, { method: 'POST' });
  check('model stub reset', reset.ok, { status: reset.status });
  const stubAtStart = await stubStats();
  recordDifference(
    '模型 stub 的失败注入能力',
    { directives: stubAtStart.directives },
    'stub 没有独立失败开关；未知 [[tool:<name>]] 指令会返回 400，但真实 host 将其折叠为 completed 空 turn（见末尾差异）。取消路径使用 agent.session.input.cancel。',
  );

  const verificationCode = crypto.randomBytes(16).toString('hex');
  const initialInput = `Remember the verification code ${verificationCode} for our next turn. Acknowledge receipt.`;
  const followUp = 'What verification code did I give you? Reply with the code only.';

  // 1. create (official streaming create) -----------------------------------
  const createStarted = Date.now();
  const createStream = await client.beta.agents.sessions.create(
    {
      environment: { type: 'none' },
      agent: {
        model: MODEL,
        instructions: 'S9a1 local verification agent. Return the stub text unchanged.',
      },
      input: initialInput,
      stream: true,
    },
    { headers: CONTRACT_HEADERS },
  );
  summary.timings.createOpenMs = Date.now() - createStarted;
  // Collecting the result requires enabling the collector before iteration.
  createStream.withResultCollection();
  const create = await collectStream(
    createStream,
    (event) => event?.type === 'agent.session.idle'
      || event?.type === 'agent.session.failed'
      || event?.type === 'error',
    TIMEOUT_MS,
  );
  summary.timings.createStreamMs = Date.now() - createStarted;
  summary.createStream = {
    eventCount: create.events.length,
    eventTypes: countFrames(create.events),
    officialTypes: officialTypes(create.events),
    outcome: create.outcome,
    error: create.error?.message ?? null,
  };
  check('create stream reached a terminal official frame', create.outcome === 'terminal', {
    outcome: create.outcome,
    error: create.error?.message ?? null,
  });
  const createRawGaps = assertNoRawGap(create.events, 'create stream (raw)');
  summary.createStream.rawGapCount = createRawGaps.length;
  summary.createStream.rawGapFrames = createRawGaps;
  summary.createStream.rawFrameSamples = rawFrameSample(create.events);

  // The official create stream's first frame is the session identity.
  const createdEvent = create.events[0];
  check(
    'create stream first frame is agent.session.created',
    createdEvent?.type === 'agent.session.created',
    { first: createdEvent, eventTypes: summary.createStream.eventTypes },
  );
  const createdSessionId = createdEvent?.session?.id;
  check(
    'agent.session.created carries the session id',
    typeof createdSessionId === 'string' && createdSessionId.length > 0,
    createdEvent?.session,
  );
  check(
    'agent.session.created object is agent.session',
    createdEvent?.session?.object === 'agent.session',
    createdEvent?.session?.object,
  );
  check(
    'agent.session.created created_at is Unix seconds',
    Number.isInteger(createdEvent?.session?.created_at) && createdEvent.session.created_at > 1_600_000_000,
    createdEvent?.session?.created_at,
  );
  check(
    'agent.session.created carries the requested environment',
    createdEvent?.session?.environment?.type === 'none',
    createdEvent?.session?.environment,
  );

  // The gateway's own `session` frame carries the persisted projection.
  const sessionFrame = create.events.find(isSessionFrame);
  check('create stream carries the custom session frame', Boolean(sessionFrame), summary.createStream.eventTypes);
  check('custom session frame reports created=true', sessionFrame?.created === true, sessionFrame?.created);
  check('custom session frame id matches the created event', sessionFrame?.id === createdSessionId, {
    frame: sessionFrame?.id,
    created: createdSessionId,
  });
  check('custom session frame logicalId matches the session id', sessionFrame?.logicalId === createdSessionId);
  check('create environment.type=none', sessionFrame?.environment?.type === 'none', sessionFrame?.environment);
  check('create agent.model is the requested model', sessionFrame?.agent?.model === MODEL, sessionFrame?.agent?.model);
  check(
    'create carried the instructions',
    String(sessionFrame?.agent?.instructions ?? '').includes('S9a1'),
    sessionFrame?.agent?.instructions,
  );
  check(
    'custom session frame exposes created_at as Unix seconds',
    Number.isInteger(sessionFrame?.created_at) && sessionFrame.created_at > 1_600_000_000,
    sessionFrame?.created_at,
  );
  check(
    'custom session frame exposes last_active_at as Unix seconds',
    Number.isInteger(sessionFrame?.last_active_at) && sessionFrame.last_active_at >= sessionFrame.created_at,
    { created_at: sessionFrame?.created_at, last_active_at: sessionFrame?.last_active_at },
  );
  check(
    'custom session frame omits the old camelCase time fields',
    sessionFrame?.createdAt === undefined && sessionFrame?.lastActivityAt === undefined,
    { createdAt: sessionFrame?.createdAt, lastActivityAt: sessionFrame?.lastActivityAt },
  );
  summary.session = {
    id: sessionFrame?.id,
    logicalId: sessionFrame?.logicalId,
    rootSessionId: sessionFrame?.rootSessionId,
    object: sessionFrame?.object,
    created: sessionFrame?.created,
    state: sessionFrame?.state,
    status: sessionFrame?.status,
    lastItemSeqAtCreate: sessionFrame?.lastItemSeq,
    created_at: sessionFrame?.created_at,
    last_active_at: sessionFrame?.last_active_at,
    environment: sessionFrame?.environment,
    agent: sessionFrame?.agent,
  };

  assertTurnStartOrder(create.events, 'create stream');
  const createTurnId = assertTurnIdentity(create.events, 'create stream');
  assertTerminalTurnIds(create.events, createTurnId, 'create stream');
  const createFinal = assertAssistantFinal(create.events, 'create stream');
  const createCompleted = findOfficial(create.events, 'agent.session.turn.completed');
  check(
    'create stream reports a completed turn',
    createCompleted?.turn?.status === 'completed',
    createCompleted?.turn,
  );
  check(
    'create stream completed turn id matches the start frames',
    createCompleted?.turn?.id === createTurnId,
    { completed: createCompleted?.turn?.id, created: createTurnId },
  );
  const createIdle = findOfficial(create.events, 'agent.session.idle');
  check('create stream ends with agent.session.idle', Boolean(createIdle), officialTypes(create.events));
  check('create idle session id matches', createIdle?.session?.id === createdSessionId);
  check(
    'agent.session.idle carries the requested environment',
    createIdle?.session?.environment?.type === 'none',
    createIdle?.session?.environment,
  );
  check('create stream emitted no failure frame', officialIndex(create.events, 'agent.session.failed') === -1);
  check('create stream emitted no error frame', officialIndex(create.events, 'error') === -1);

  // SDK result collection over the same create stream. `finalResult()` fails
  // when the expected terminal/idle order or the assistant item is missing.
  let createResult = null;
  let createResultError = null;
  if (createIdle !== undefined) {
    try {
      createResult = await createStream.finalResult();
    } catch (resultError) {
      createResultError = resultError;
    }
  }
  check('SDK finalResult() returned the create turn', Boolean(createResult), {
    error: createResultError?.message ?? null,
  });
  check(
    'SDK finalResult output_text matches the assistant item',
    createResult?.output_text === createFinal.text,
    { sdk: createResult?.output_text, item: createFinal.text },
  );
  check('SDK finalResult turn id matches', createResult?.turn_id === createTurnId, {
    sdk: createResult?.turn_id,
    events: createTurnId,
  });
  check('SDK finalResult collected one final message', (createResult?.messages ?? []).length === 1, {
    messages: createResult?.messages?.length,
  });
  summary.createStream.finalResult = createResult === null
    ? null
    : {
        output_text: createResult.output_text,
        turn_id: createResult.turn_id,
        status: createResult.turn?.status,
        messages: createResult.messages.length,
        message_ids: createResult.messages.map((message) => message.id),
      };
  summary.createStream.assistantItemDone = {
    itemId: createFinal.item?.id,
    turnId: createFinal.event?.turn_id,
    phase: createFinal.item?.phase,
    output_index: createFinal.event?.output_index,
    text: createFinal.text,
  };

  // 2. get ------------------------------------------------------------------
  const getStarted = Date.now();
  const fetchedWait = await waitForSession(
    client,
    createdSessionId,
    (session) => session.state === 'idle' && session.status === 'idle',
  );
  const fetched = fetchedWait.session;
  summary.timings.retrieveMs = Date.now() - getStarted;
  summary.statusSettle = {
    note: 'agent.session.idle 在 turn lease 释放前发出；紧随其后的 GET 可能短暂看到 in_progress',
    settleMs: fetchedWait.settleMs,
    observed: fetchedWait.observed,
  };
  check('get status settles to idle', fetchedWait.settleMs !== null, fetchedWait.observed);
  recordDifference(
    'agent.session.idle 与 GET status 的时序',
    { settleMs: fetchedWait.settleMs, observed: fetchedWait.observed },
    'idle 帧在 turn lease 释放前发出；紧随其后的 GET 可能短暂返回 in_progress。脚本轮询至稳定并记录窗口。',
  );
  summary.retrieved = {
    id: fetched.id,
    rootSessionId: fetched.rootSessionId,
    state: fetched.state,
    status: fetched.status,
    lastItemSeq: fetched.lastItemSeq,
    cwd: fetched.cwd,
    created_at: fetched.created_at,
    last_active_at: fetched.last_active_at,
    environment: fetched.environment,
  };
  check('get returns the same session', fetched.id === createdSessionId, { got: fetched.id });
  check(
    'get keeps the root session id',
    typeof fetched.rootSessionId === 'string'
      && fetched.rootSessionId.length > 0
      && fetched.rootSessionId === sessionFrame?.rootSessionId,
    { fetched: fetched.rootSessionId ?? null, sessionFrame: sessionFrame?.rootSessionId ?? null },
  );
  check('get reports idle', fetched.state === 'idle', fetched.state);
  check('get reports the official idle status', fetched.status === 'idle', fetched.status);
  check(
    'get sees the create-turn items',
    fetched.lastItemSeq >= (sessionFrame?.lastItemSeq ?? 0) + 1,
    { fetched: fetched.lastItemSeq, atCreate: sessionFrame?.lastItemSeq },
  );
  check('get keeps the requested model', fetched.agent?.model === MODEL);
  check(
    'get exposes created_at as Unix seconds',
    Number.isInteger(fetched.created_at) && fetched.created_at > 1_600_000_000,
    fetched.created_at,
  );
  check(
    'get exposes last_active_at as Unix seconds',
    Number.isInteger(fetched.last_active_at) && fetched.last_active_at >= fetched.created_at,
    { created_at: fetched.created_at, last_active_at: fetched.last_active_at },
  );
  check(
    'get omits the old camelCase time fields',
    fetched.createdAt === undefined && fetched.lastActivityAt === undefined,
    { createdAt: fetched.createdAt, lastActivityAt: fetched.lastActivityAt },
  );
  recordDifference(
    '会话 usage',
    { session_usage: fetched.usage ?? null },
    '网关不做 token 记账，官方 AgentSession.usage 恒为 null；这里是记录差异，不是通过断言。',
  );

  // Initial items (asc) prove the first answer is in the log. `data` is the
  // official CursorPage view: one last row per item.
  const initialPage = await client.beta.agents.sessions.items.list(
    createdSessionId,
    { order: 'asc', limit: 100 },
    { headers: CONTRACT_HEADERS },
  );
  check('items page object is list', initialPage.body?.object === 'list', initialPage.body?.object);
  check('items page carries the logical id', initialPage.body?.logicalId === createdSessionId);
  check(
    'items page exposes boolean has_more and hasMore',
    typeof initialPage.body?.has_more === 'boolean' && typeof initialPage.body?.hasMore === 'boolean',
    { has_more: initialPage.body?.has_more, hasMore: initialPage.body?.hasMore },
  );
  check(
    'items page answers has_more and hasMore consistently',
    initialPage.body.has_more === initialPage.body.hasMore,
    { has_more: initialPage.body.has_more, hasMore: initialPage.body.hasMore },
  );
  check(
    'SDK CursorPage parsed the official data rows',
    Array.isArray(initialPage.data) && initialPage.data.length > 0,
    { sdkData: initialPage.data?.length, bodyData: initialPage.body?.data?.length },
  );
  const initialItems = initialPage.body?.items ?? [];
  const initialDataShape = assertOfficialItems(initialPage.body, 'initial items');
  const initialComplete = initialItems.find((row) => row.kind === 'message' && row.payload?.complete === true);
  check('first answer is a complete message item', Boolean(initialComplete), summarizeItems(initialItems));
  const initialOfficial = initialDataShape.data.find((item) => item.id === initialComplete?.itemId);
  check('first answer projects into the same official turn as SSE', initialOfficial?.type === 'message'
    && initialOfficial.role === 'assistant' && initialOfficial.phase === 'final_answer'
    && initialOfficial.status === 'completed' && initialOfficial.turn_id === createFinal.event?.turn_id
    && initialOfficial.content[0]?.text === initialComplete?.payload?.text, initialOfficial);
  check(
    'first answer item is persisted as final_answer',
    initialComplete?.payload?.phase === 'final_answer',
    { phase: initialComplete?.payload?.phase, outputIndex: initialComplete?.payload?.outputIndex },
  );
  check('first answer item output_index is 0', initialComplete?.payload?.outputIndex === 0, initialComplete?.payload?.outputIndex);
  check(
    'item view id mirrors itemId',
    typeof initialComplete?.id === 'string'
      && typeof initialComplete?.itemId === 'string'
      && initialComplete.id === initialComplete.itemId,
    { id: initialComplete?.id, itemId: initialComplete?.itemId },
  );
  const initialStats = await stubStats();
  const initialTurn = initialStats.turns.find(
    (turn) => turn.kind === 'final' && turn.lastUserText === initialInput,
  );
  check('model stub saw the initial input', Boolean(initialTurn), initialStats.turns);
  check('first answer acknowledged the verification code', createFinal.text === 'stub-code-saved', createFinal.text);
  check('first answer text equals the stub reply', initialComplete?.payload?.text === initialTurn.reply, {
    item: initialComplete?.payload?.text,
    stub: initialTurn.reply,
  });
  summary.initialTurn = {
    stubReply: initialTurn.reply,
    stubLastUserText: initialTurn.lastUserText,
    itemSeq: initialComplete?.seq,
    itemId: initialComplete?.itemId,
    deltaCount: initialItems.filter((item) => item.payload?.delta === true).length,
    dataCount: initialDataShape.data.length,
    items: summarizeItems(initialItems),
  };

  // 3. Official batch envelope: rejected without creating a turn ------------
  const rejectStarted = Date.now();
  let batchProbe;
  try {
    await client.beta.agents.sessions.events.create(
      createdSessionId,
      {
        events: [
          {
            type: 'agent.session.input.message',
            input: [{ role: 'user', content: [{ type: 'input_text', text: 'S9a1 rejected batch A' }] }],
          },
          {
            type: 'agent.session.input.message',
            input: [{ role: 'user', content: [{ type: 'input_text', text: 'S9a1 rejected batch B' }] }],
          },
        ],
      },
      { headers: CONTRACT_HEADERS },
    );
    batchProbe = { accepted: true };
  } catch (error) {
    batchProbe = {
      accepted: false,
      status: error?.status,
      name: error?.name,
      message: error?.message,
      body: error?.error ?? null,
    };
  }
  summary.timings.batchRejectMs = Date.now() - rejectStarted;
  summary.batchEnvelopeProbe = batchProbe;
  check(
    'official multi-event batch is rejected (recorded, no turn)',
    batchProbe.accepted === false && batchProbe.status === 400,
    batchProbe,
  );
  await sleep(150);
  const statsAfterReject = await stubStats();
  const afterReject = await client.beta.agents.sessions.retrieve(createdSessionId, {
    headers: CONTRACT_HEADERS,
  });
  check(
    'rejected batch started no model call',
    typeof statsAfterReject.mainCalls === 'number'
      && typeof initialStats.mainCalls === 'number'
      && statsAfterReject.mainCalls === initialStats.mainCalls,
    { before: initialStats.mainCalls, after: statsAfterReject.mainCalls },
  );
  check(
    'rejected batch left lastItemSeq unchanged',
    typeof afterReject.lastItemSeq === 'number'
      && typeof fetched.lastItemSeq === 'number'
      && afterReject.lastItemSeq === fetched.lastItemSeq,
    { before: fetched.lastItemSeq, after: afterReject.lastItemSeq },
  );

  // 4. A fresh SDK client subscribes before posting to the existing session.
  const continuingClient = new OpenAI(clientOptions);
  const sessionEvents = continuingClient.beta.agents.sessions.events;
  const streamStarted = Date.now();
  const inputId = crypto.randomUUID();
  const expectedFollowUpTurnId = `turn_input_${inputId}`;
  const stream = await sessionEvents.stream(createdSessionId, { headers: CONTRACT_HEADERS });
  const streamOpenedAt = Date.now();
  summary.timings.sseOpenMs = streamOpenedAt - streamStarted;
  const followUpReader = collectStream(
    stream,
    (event) => turnTerminalReached(event, expectedFollowUpTurnId),
    TIMEOUT_MS,
  );
  const postStarted = Date.now();
  summary.timings.ssePostGapMs = postStarted - streamOpenedAt;
  const submitFollowUp = () => sessionEvents.create(
    createdSessionId,
    {
      'Idempotency-Key': inputId,
      events: [{
        type: 'agent.session.input.message',
        input: [{ type: 'message', role: 'user', content: [{ type: 'input_text', text: followUp }] }],
      }],
    },
    { headers: CONTRACT_HEADERS },
  ).asResponse();
  const acceptedResponse = await submitFollowUp();
  check('SDK events.create returned HTTP 202', acceptedResponse.status === 202, acceptedResponse.status);
  const accepted = await acceptedResponse.json();
  summary.timings.eventsPostMs = Date.now() - postStarted;
  summary.accepted = accepted;
  check('events POST accepted the input', accepted?.accepted === true, accepted);
  check('events POST echoes the input id', accepted?.inputId === inputId, accepted);
  check('events POST disposition inserted', accepted?.disposition === 'inserted', accepted);
  check('events POST reports turn mode', accepted?.mode === 'turn', accepted?.mode);
  check(
    'events POST receipt advances lastItemSeq by the user item',
    accepted?.lastItemSeq === fetched.lastItemSeq + 1,
    { receipt: accepted?.lastItemSeq, before: fetched.lastItemSeq },
  );

  const followUpRun = await followUpReader;
  const followUpRawGaps = assertNoRawGap(followUpRun.events, 'follow-up stream (raw)');
  const followUpEvents = followUpRun.events.filter(
    (event) => eventBelongsToTurn(event, expectedFollowUpTurnId, accepted.lastItemSeq),
  );
  const unexpectedTurnIds = [
    ...new Set(
      followUpRun.events
        .filter(isOfficialEvent)
        .map(officialEventTurnId)
        .filter((turnId) => turnId !== null && turnId !== expectedFollowUpTurnId),
    ),
  ];
  summary.timings.sseMs = Date.now() - streamStarted;
  summary.followUpStream = {
    rawEventCount: followUpRun.events.length,
    rawEventTypes: countFrames(followUpRun.events),
    unexpectedTurnIds,
    turnEventCount: followUpEvents.length,
    eventTypes: countFrames(followUpEvents),
    officialTypes: officialTypes(followUpEvents),
    outcome: followUpRun.outcome,
    error: followUpRun.error?.message ?? null,
  };
  check('SSE stream reached a terminal official frame', followUpRun.outcome === 'terminal', {
    outcome: followUpRun.outcome,
    error: followUpRun.error?.message ?? null,
  });
  check('follow-up stream emitted no error frame', officialIndex(followUpEvents, 'error') === -1);
  check('follow-up stream emitted no failure frame', officialIndex(followUpEvents, 'agent.session.failed') === -1);
  summary.followUpStream.rawGapCount = followUpRawGaps.length;
  summary.followUpStream.rawGapFrames = followUpRawGaps;
  summary.followUpStream.rawFrameSamples = rawFrameSample(followUpRun.events);
  check(
    'follow-up stream observed the submitted turn',
    followUpEvents.length > 0,
    { expectedTurnId: expectedFollowUpTurnId, rawEventTypes: summary.followUpStream.rawEventTypes },
  );
  check('SDK tail subscription skipped earlier turns', unexpectedTurnIds.length === 0, unexpectedTurnIds);

  assertTurnStartOrder(followUpEvents, 'follow-up stream');
  const followUpTurnId = assertTurnIdentity(followUpEvents, 'follow-up stream');
  assertTerminalTurnIds(followUpEvents, followUpTurnId, 'follow-up stream');
  check(
    'follow-up turn id derives from the submitted input id',
    followUpTurnId === expectedFollowUpTurnId,
    { observed: followUpTurnId, expected: expectedFollowUpTurnId },
  );
  const userAddedEvents = followUpEvents.filter(
    (event) => event?.type === 'agent.session.turn.item.added' && event.item?.role === 'user',
  );
  check('follow-up stream carries one user item', userAddedEvents.length === 1, officialTypes(followUpEvents));
  const [userAdded] = userAddedEvents;
  check('follow-up user item text is the submitted text', itemText(userAdded?.item) === followUp, {
    item: itemText(userAdded?.item),
    submitted: followUp,
  });
  check('follow-up user item shares the turn id', userAdded?.turn_id === followUpTurnId);
  const followUpFinal = assertAssistantFinal(followUpEvents, 'follow-up stream');
  check('follow-up recalled the verification code from the first turn', followUpFinal.text === verificationCode, {
    expected: verificationCode,
    actual: followUpFinal.text,
  });
  const followUpCompleted = findOfficial(followUpEvents, 'agent.session.turn.completed');
  check(
    'follow-up stream reports a completed turn',
    followUpCompleted?.turn?.status === 'completed',
    followUpCompleted?.turn,
  );
  const followUpIdle = findOfficial(followUpEvents, 'agent.session.idle');
  check('follow-up stream ends with agent.session.idle', Boolean(followUpIdle), officialTypes(followUpEvents));
  check('follow-up idle session id matches', followUpIdle?.session?.id === createdSessionId);
  check('follow-up idle status is idle', followUpIdle?.session?.status === 'idle', followUpIdle?.session?.status);

  const sseItems = followUpEvents.filter(isItemFrame);
  const sseTurns = followUpEvents.filter(isTurnFrame);
  const sseOther = followUpEvents.filter(
    (event) => !isItemFrame(event) && !isTurnFrame(event) && !isOfficialEvent(event),
  );
  summary.sse = {
    eventCount: followUpEvents.length,
    turnStates: sseTurns.map((event) => event.state),
    seqs: sseItems.map((item) => item.seq),
    items: summarizeItems(sseItems),
    other: sseOther.map((event) => {
      if (event?.fromSeq !== undefined) return { gap: event };
      if (isHeartbeat(event)) return { heartbeat: event };
      if (isSessionFrame(event)) return { session: { id: event.logicalId, created: event.created } };
      return { unknown: event };
    }),
  };

  check(
    'SSE reported the gateway turn started and completed',
    sseTurns.length === 2
      && sseTurns[0]?.state === 'started'
      && sseTurns[1]?.state === 'completed',
    sseTurns,
  );
  check(
    'SSE turn events share one request id',
    typeof sseTurns[0]?.requestId === 'string'
      && typeof sseTurns[1]?.requestId === 'string'
      && sseTurns[0].requestId === sseTurns[1].requestId,
    sseTurns,
  );
  check(
    'SSE item seq starts at the accepted user item',
    typeof sseItems[0]?.seq === 'number'
      && typeof accepted.lastItemSeq === 'number'
      && sseItems[0].seq === accepted.lastItemSeq,
    { first: sseItems[0]?.seq, receipt: accepted.lastItemSeq },
  );
  for (let index = 1; index < sseItems.length; index += 1) {
    if (sseItems[index].seq !== sseItems[index - 1].seq + 1) {
      check('SSE item seq is contiguous', false, {
        previous: sseItems[index - 1].seq,
        current: sseItems[index].seq,
      });
    }
  }
  // 实时 delta 没有持久游标；只有 item 行参与 seq 和回放比较。
  const sseDeltas = followUpEvents.filter((event) => event?.type === 'agent.session.turn.output_text.delta');
  const sseComplete = sseItems.find((item) => item.kind === 'message' && item.payload?.complete === true);
  const sseDeltaText = sseDeltas.map((event) => event.delta).join('');
  summary.sse.textDeltaCount = sseDeltas.length;
  check('SSE streamed at least two text deltas', sseDeltas.length >= 2, sseDeltas.length);
  check('SSE ended with a complete message', Boolean(sseComplete), summarizeItems(sseItems));
  check('SSE deltas concatenate to the complete text', sseDeltaText === sseComplete?.payload?.text, {
    delta: sseDeltaText,
    complete: sseComplete?.payload?.text,
  });
  check(
    'SSE deltas and the complete item share one item id',
    sseDeltas.every((event) => event.item_id === sseComplete?.itemId && event.turn_id === followUpTurnId),
    { complete: sseComplete?.itemId, deltas: sseDeltas.map((event) => event.item_id) },
  );
  check(
    'assistant item.done id matches the gateway complete item',
    typeof followUpFinal.item?.id === 'string'
      && typeof sseComplete?.itemId === 'string'
      && followUpFinal.item.id === sseComplete.itemId,
    { official: followUpFinal.item?.id, gateway: sseComplete?.itemId },
  );
  check(
    'assistant item.done seq matches the gateway complete row',
    followUpFinal.event?.event_id === `evt_item_${sseComplete?.seq}_done`,
    { event_id: followUpFinal.event?.event_id, seq: sseComplete?.seq },
  );
  check(
    'assistant item.done text matches the gateway complete text',
    followUpFinal.text === sseComplete?.payload?.text,
    { official: followUpFinal.text, gateway: sseComplete?.payload?.text },
  );

  const finalStats = await stubStats();
  const newModelCalls = finalStats.turns.filter((turn) => turn.n > initialTurn.n);
  check(
    'model stub received exactly one follow-up call',
    finalStats.mainCalls === initialStats.mainCalls + 1
      && newModelCalls.length === 1
      && newModelCalls[0].kind === 'final'
      && newModelCalls[0].lastUserText === followUp,
    newModelCalls,
  );
  const [followUpTurn] = newModelCalls;
  check('SSE complete text equals the stub reply', sseComplete?.payload?.text === followUpTurn.reply, {
    item: sseComplete?.payload?.text,
    stub: followUpTurn.reply,
  });
  summary.modelStub = {
    mainCalls: finalStats.mainCalls,
    helperCalls: finalStats.helperCalls,
    otherCalls: finalStats.otherCalls,
    turns: finalStats.turns,
  };
  recordDifference(
    'turn 用量',
    {
      item_done_usage: followUpFinal.event?.usage ?? null,
      turn_completed_usage: followUpCompleted?.usage ?? null,
      turn_usage: followUpCompleted?.turn?.usage ?? null,
    },
    '网关不做 token 记账，官方 turn 的 usage 恒为 null；这里是记录差异。',
  );
  recordDifference(
    'item.added.output_index',
    { output_index: userAdded?.output_index ?? null },
    '当前只有 turn.item.done 携带 output_index；item.added 仍为 null。',
  );
  recordDifference(
    '官方文本 delta 帧',
    { officialTypes: officialTypes(followUpEvents) },
    '回放为单条全文 delta，实时为逐 chunk delta；实时与回放 delta 的 event_id 不保证一致。',
  );

  const afterFollowUpWait = await waitForSession(
    client,
    createdSessionId,
    (session) => session.state === 'idle' && session.status === 'idle',
  );
  const afterFollowUp = afterFollowUpWait.session;
  summary.afterFollowUp = {
    state: afterFollowUp.state,
    status: afterFollowUp.status,
    statusSettleMs: afterFollowUpWait.settleMs,
    lastItemSeq: afterFollowUp.lastItemSeq,
    rootSessionId: afterFollowUp.rootSessionId,
    created_at: afterFollowUp.created_at,
    last_active_at: afterFollowUp.last_active_at,
  };
  check('follow-up status settles to idle', afterFollowUpWait.settleMs !== null, afterFollowUpWait.observed);
  check('follow-up turn left the session idle', afterFollowUp.state === 'idle', afterFollowUp.state);
  check('follow-up status is idle', afterFollowUp.status === 'idle', afterFollowUp.status);
  check(
    'follow-up advanced lastItemSeq to the SSE tail',
    typeof afterFollowUp.lastItemSeq === 'number'
      && typeof sseItems[sseItems.length - 1]?.seq === 'number'
      && afterFollowUp.lastItemSeq === sseItems[sseItems.length - 1].seq,
    { session: afterFollowUp.lastItemSeq, sse: sseItems[sseItems.length - 1]?.seq },
  );
  check('same session across every call', afterFollowUp.id === createdSessionId);
  check('follow-up kept the root session id', afterFollowUp.rootSessionId === fetched.rootSessionId, {
    before: fetched.rootSessionId,
    after: afterFollowUp.rootSessionId,
  });
  check(
    'follow-up advanced last_active_at',
    afterFollowUp.last_active_at >= fetched.last_active_at,
    { before: fetched.last_active_at, after: afterFollowUp.last_active_at },
  );

  // Resume from the live user item's seq, as a client that missed the answer would.
  const checkpoint = sseItems[0].seq;
  const replayStream = await sessionEvents.stream(createdSessionId, {
    headers: { ...CONTRACT_HEADERS, 'Last-Event-ID': String(checkpoint) },
  });
  const replayRun = await collectStream(
    replayStream,
    (event) => turnTerminalReached(event, followUpTurnId) || isGapFrame(event),
    TIMEOUT_MS,
  );
  const replayRawGaps = assertNoRawGap(replayRun.events, 'reconnect stream (raw)');
  const replayItems = replayRun.events.filter(isItemFrame);
  const replayDeltas = replayRun.events.filter((event) => event?.type === 'agent.session.turn.output_text.delta');
  const remainingItems = sseItems.slice(1);
  summary.reconnect = {
    lastEventId: checkpoint,
    outcome: replayRun.outcome,
    rawEventTypes: countFrames(replayRun.events),
    itemSeqs: replayItems.map((item) => item.seq),
  };
  check('SDK reconnect reached the follow-up terminal', replayRun.outcome === 'terminal', {
    ...summary.reconnect,
    error: replayRun.error?.message,
  });
  check(
    'SDK reconnect replayed exactly the rows after the checkpoint',
    isDeepStrictEqual(replayItems, remainingItems),
    { replayed: summarizeItems(replayItems), expected: summarizeItems(remainingItems) },
  );
  assertTerminalTurnIds(replayRun.events, followUpTurnId, 'reconnect stream');
  check(
    'SDK reconnect replayed exactly one full-text delta',
    replayDeltas.length === 1
      && replayDeltas[0].turn_id === followUpTurnId
      && replayDeltas[0].item_id === sseComplete?.itemId
      && replayDeltas[0].delta === sseComplete?.payload?.text,
    replayDeltas,
  );
  const replayFinal = assertAssistantFinal(replayRun.events, 'reconnect stream');
  check(
    'SDK reconnect replayed the same assistant item and answer',
    replayFinal.event.turn_id === followUpTurnId
      && replayFinal.event.event_id === followUpFinal.event.event_id
      && replayFinal.item.id === followUpFinal.item.id
      && replayFinal.text === verificationCode,
    replayFinal,
  );
  check(
    'SDK reconnect reached idle for the same session',
    findOfficial(replayRun.events, 'agent.session.idle')?.session?.id === createdSessionId,
    officialTypes(replayRun.events),
  );

  const retriedResponse = await submitFollowUp();
  check('repeated SDK events.create returned HTTP 202', retriedResponse.status === 202, retriedResponse.status);
  const retried = await retriedResponse.json();
  summary.retried = retried;
  check(
    'repeated SDK events.create preserved the receipt and the item cursor',
    retried?.accepted === true
      && retried.inputId === inputId
      && retried.disposition === 'existing'
      && retried.lastItemSeq === afterFollowUp.lastItemSeq,
    retried,
  );
  const afterRetry = await continuingClient.beta.agents.sessions.retrieve(createdSessionId, {
    headers: CONTRACT_HEADERS,
  });
  check(
    'repeated SDK events.create left the session idle and history unchanged',
    afterRetry.status === 'idle' && afterRetry.lastItemSeq === afterFollowUp.lastItemSeq,
    { status: afterRetry.status, lastItemSeq: afterRetry.lastItemSeq },
  );
  const statsAfterRetry = await stubStats();
  check(
    'SDK reconnect and repeated events.create started no additional model call',
    statsAfterRetry.mainCalls === finalStats.mainCalls,
    { before: finalStats.mainCalls, after: statsAfterRetry.mainCalls },
  );

  // 5. items GET (asc and desc, cursor paging) ------------------------------
  const itemsStarted = Date.now();
  const paginate = async (order, limit) => {
    const pages = [];
    let after;
    for (;;) {
      const query = { order, limit };
      if (after !== undefined) query.after = after;
      const page = await client.beta.agents.sessions.items.list(createdSessionId, query, {
        headers: CONTRACT_HEADERS,
      });
      const body = page.body ?? {};
      const dataShape = assertOfficialItems(body, `items ${order} page ${pages.length + 1}`);
      const sdkData = page.data ?? [];
      pages.push({
        query,
        count: (body.items ?? []).length,
        dataCount: dataShape.data.length,
        nextCursor: body.nextCursor,
        hasMore: body.has_more,
        hasMoreCamel: body.hasMore,
        sdkHasMore: page.has_more,
        sdkDataLength: sdkData.length,
        lastItemSeq: body.lastItemSeq,
        order: body.order,
        sdkDataIds: sdkData.map((item) => item.id),
      });
      check(`items ${order} page object is list`, body.object === 'list', body.object);
      check(`items ${order} page logical id`, body.logicalId === createdSessionId, body.logicalId);
      check(`items ${order} page order`, body.order === order, body.order);
      check(
        `items ${order} page respects the official item limit`,
        dataShape.data.length <= Math.min(limit, dataShape.rawItemCount),
        { data: dataShape.data.length, items: dataShape.rawItemCount, limit },
      );
      check(
        `items ${order} page lastItemSeq is a number`,
        typeof body.lastItemSeq === 'number' && body.lastItemSeq === afterFollowUp.lastItemSeq,
        { page: body.lastItemSeq, session: afterFollowUp.lastItemSeq },
      );
      check(
        `items ${order} body exposes boolean has_more and hasMore`,
        typeof body.has_more === 'boolean' && typeof body.hasMore === 'boolean',
        { has_more: body.has_more, hasMore: body.hasMore },
      );
      check(
        `items ${order} body answers has_more and hasMore consistently`,
        body.has_more === body.hasMore,
        { has_more: body.has_more, hasMore: body.hasMore },
      );
      check(
        `items ${order} SDK CursorPage has_more is boolean and matches the body`,
        typeof page.has_more === 'boolean' && page.has_more === body.has_more,
        { sdk: page.has_more, body: body.has_more },
      );
      check(
        `items ${order} SDK CursorPage data matches the body data`,
        sdkData.length === dataShape.data.length,
        { sdk: sdkData.length, body: dataShape.data.length },
      );
      for (const item of body.items ?? []) {
        if (item.seq === after) {
          check(`items ${order} cursor is exclusive`, false, { after });
        }
      }
      if (!body.hasMore) break;
      check(`items ${order} hasMore implies nextCursor`, typeof body.nextCursor === 'number', body.nextCursor);
      after = body.nextCursor;
      if (pages.length > 1000) throw new Error('items pagination did not terminate');
    }
    return pages;
  };

  const ascPages = await paginate('asc', 1);
  const descPages = await paginate('desc', 1);
  summary.timings.itemsMs = Date.now() - itemsStarted;
  summary.items = { ascPages, descPages };

  const ascItems = [];
  const descItems = [];
  // Fetch the full ascending set once for the content assertions and compare
  // it with the item-grouped windows above.
  const fullAsc = await client.beta.agents.sessions.items.list(
    createdSessionId,
    { order: 'asc', limit: 500 },
    { headers: CONTRACT_HEADERS },
  );
  const fullItems = fullAsc.body?.items ?? [];
  const fullData = assertOfficialItems(fullAsc.body, 'full asc items').data;
  summary.items.full = {
    count: fullItems.length,
    dataCount: fullData.length,
    lastItemSeq: fullAsc.body?.lastItemSeq,
    hasMore: fullAsc.body?.has_more,
    hasMoreCamel: fullAsc.body?.hasMore,
    sdkHasMore: fullAsc.has_more,
    sdkDataLength: (fullAsc.data ?? []).length,
    nextCursor: fullAsc.body?.nextCursor,
    data: fullData.map(({ id, type, turn_id }) => ({ id, type, turn_id })),
    items: summarizeItems(fullItems),
  };
  check('full asc read has no next page', fullAsc.body?.has_more === false, fullAsc.body?.has_more);
  check(
    'full asc read exposes boolean has_more and hasMore',
    typeof fullAsc.body?.has_more === 'boolean' && typeof fullAsc.body?.hasMore === 'boolean',
    { has_more: fullAsc.body?.has_more, hasMore: fullAsc.body?.hasMore },
  );
  check(
    'full asc read answers has_more and hasMore consistently',
    fullAsc.body.has_more === fullAsc.body.hasMore,
    { has_more: fullAsc.body.has_more, hasMore: fullAsc.body.hasMore },
  );
  check(
    'full asc SDK has_more is boolean and matches the body',
    typeof fullAsc.has_more === 'boolean' && fullAsc.has_more === fullAsc.body.has_more,
    { sdk: fullAsc.has_more, body: fullAsc.body.has_more },
  );
  check('full asc SDK data matches the body data', (fullAsc.data ?? []).length === fullData.length);
  check('full asc read covers every seq', fullItems.length === afterFollowUp.lastItemSeq, {
    count: fullItems.length,
    lastItemSeq: afterFollowUp.lastItemSeq,
  });
  check('full asc data is non-empty', fullData.length > 0, fullData.length);
  // 本场景只写根会话的用户消息和完成的助手消息，不含纯内部页。
  const expectedItemIds = new Set(fullItems.filter((row) => row.kind === 'message'
    && row.source?.agentId === 'root' && !row.truncated
    && (row.payload?.role === 'user' || row.payload?.complete === true)).map((row) => row.itemId));
  check('full asc data covers the mappable messages in this scenario',
    fullData.length === expectedItemIds.size && fullData.every((item) => expectedItemIds.has(item.id)),
    { data: fullData.map((item) => item.id), expected: [...expectedItemIds] });
  for (let index = 0; index < fullItems.length; index += 1) {
    if (fullItems[index].seq !== index + 1) {
      check('item seq is 1..N without gaps', false, { index, seq: fullItems[index].seq });
    }
  }
  const ascPageCursors = ascPages.map((page) => page.nextCursor);
  check('asc pagination produced multiple pages', ascPages.length >= 2, ascPages.length);
  check(
    'asc pagination reports hasMore until the end',
    ascPages.slice(0, -1).every((page) => page.hasMore === true) && ascPages.at(-1).hasMore === false,
    ascPages.map((page) => page.hasMore),
  );
  check('desc pagination produced multiple pages', descPages.length >= 2, descPages.length);
  check(
    'desc pagination reports hasMore until the end',
    descPages.slice(0, -1).every((page) => page.hasMore === true) && descPages.at(-1).hasMore === false,
    descPages.map((page) => page.hasMore),
  );

  // Rebuild the paged seq lists from fresh reads so the dedupe check is real.
  const readWindow = async (order, limit, after) => {
    const query = { order, limit };
    if (after !== undefined) query.after = after;
    const page = await client.beta.agents.sessions.items.list(createdSessionId, query, {
      headers: CONTRACT_HEADERS,
    });
    return page.body;
  };
  for (const order of ['asc', 'desc']) {
    const pages = order === 'asc' ? ascPages : descPages;
    const collected = [];
    for (const page of pages) {
      const body = await readWindow(order, page.query.limit, page.query.after);
      for (const item of body.items ?? []) collected.push(item.seq);
      check(`${order} page re-read matches its recorded count`, (body.items ?? []).length === page.count);
    }
    const unique = new Set(collected);
    check(`${order} paging has no duplicates`, unique.size === collected.length, collected);
    const expected = fullItems.map((item) => item.seq);
    const sorted = order === 'asc' ? [...expected] : [...expected].reverse();
    check(`${order} paging covers every item exactly once`, collected.join(',') === sorted.join(','), {
      got: collected,
      want: sorted,
    });
    if (order === 'asc') ascItems.push(...collected);
    else descItems.push(...collected);
  }
  check('asc and desc windows are reverses', ascItems.join(',') === [...descItems].reverse().join(','));

  const loggedFollowUp = fullItems.find(
    (item) => item.payload?.complete === true && item.payload?.text === followUpTurn.reply,
  );
  check('follow-up answer landed in items', Boolean(loggedFollowUp), summarizeItems(fullItems));
  const loggedConversation = fullData.filter((item) => item.type === 'message').map((item) => ({
    role: item.role,
    text: item.content[0]?.text,
    turn: item.turn_id,
  }));
  check(
    'official history contains both user and assistant turns exactly once, in order',
    isDeepStrictEqual(loggedConversation, [
      { role: 'user', text: initialInput, turn: createFinal.event?.turn_id },
      { role: 'assistant', text: createFinal.text, turn: createFinal.event?.turn_id },
      { role: 'user', text: followUp, turn: followUpTurnId },
      { role: 'assistant', text: verificationCode, turn: followUpTurnId },
    ]),
    loggedConversation,
  );
  check(
    'items complete item matches the SSE item',
    typeof loggedFollowUp?.itemId === 'string'
      && typeof sseComplete?.itemId === 'string'
      && loggedFollowUp.itemId === sseComplete.itemId,
    { items: loggedFollowUp?.itemId, sse: sseComplete?.itemId },
  );
  check(
    'items complete item seq matches the SSE seq',
    typeof loggedFollowUp?.seq === 'number'
      && typeof sseComplete?.seq === 'number'
      && loggedFollowUp.seq === sseComplete.seq,
    { items: loggedFollowUp?.seq, sse: sseComplete?.seq },
  );
  check(
    'items complete item persisted final_answer',
    loggedFollowUp?.payload?.phase === 'final_answer',
    loggedFollowUp?.payload?.phase,
  );

  // The official CursorPage now reads body.data/body.has_more; the gateway also
  // answers the Swarm `items`/`hasMore` view. Both are recorded and checked.
  const firstPage = ascPages[0];
  summary.sdkPageShape = {
    note: 'openai@7.60.0 CursorPage reads body.data/body.has_more; peri-swarm emits data plus items/hasMore/has_more',
    sdkDataLength: firstPage.sdkDataLength,
    bodyDataCount: firstPage.dataCount,
    sdkHasMore: firstPage.sdkHasMore,
    bodyHasMore: firstPage.hasMore,
    bodyHasMoreCamel: firstPage.hasMoreCamel,
  };
  check('SDK CursorPage read the official data rows', firstPage.sdkDataLength > 0, firstPage);
  check(
    'SDK CursorPage read has_more from the body',
    typeof firstPage.sdkHasMore === 'boolean' && firstPage.sdkHasMore === firstPage.hasMore,
    firstPage,
  );
  check(
    'body emits has_more and hasMore consistently',
    typeof firstPage.hasMore === 'boolean' && firstPage.hasMore === firstPage.hasMoreCamel,
    firstPage,
  );

  // The SDK auto-paginates with the last `data[].id` (`itim_...`) as `after`.
  const sdkPaged = [];
  for await (const item of client.beta.agents.sessions.items.list(
    createdSessionId,
    { order: 'asc', limit: 2 },
    { headers: CONTRACT_HEADERS },
  )) {
    sdkPaged.push(item);
  }
  const fullDataIds = fullData.map((item) => item.id);
  check(
    'SDK auto-pagination yields every item-level row once',
    sdkPaged.length === fullData.length,
    { sdk: sdkPaged.length, data: fullData.length },
  );
  check(
    'SDK auto-pagination follows the data ids in order',
    sdkPaged.map((item) => item.id).join(',') === fullDataIds.join(','),
    { sdk: sdkPaged.map((item) => item.id), data: fullDataIds },
  );
  summary.items.sdkAutoPagination = {
    note: 'client.beta.agents.sessions.items.list 的 for-await 自动用 data[last].id 作为 after',
    count: sdkPaged.length,
    ids: sdkPaged.map((item) => item.id),
  };

  // 6. deterministic cancellation path -------------------------------------
  // The stub streams slowly (`PERI_STUB_CHUNK_DELAY_MS`) so the driver can
  // cancel after the turn starts. The gateway folds a host cancel into
  // `turn.completed` with official status `cancelled`; the SDK reader must
  // observe the terminal instead of waiting forever or throwing.
  //
  // A provider 400 is not a deterministic failure here: an unknown
  // `[[tool:...]]` directive made the stub answer 400, and the ACP prompt still
  // returned a normal stop reason with no assistant output (observed while
  // developing this driver). The cancellation path is the contract-defined
  // terminal this script can drive without killing a process mid-run.
  const cancelStreamStarted = Date.now();
  const cancelInputId = crypto.randomUUID();
  const cancelControlId = crypto.randomUUID();
  const expectedCancelTurnId = `turn_input_${cancelInputId}`;
  const cancelText = `S9a1 cancellation probe ${cancelInputId}: stream slowly, then cancel.`;
  const cancelStream = await client.beta.agents.sessions.events.stream(
    createdSessionId,
    { after: afterFollowUp.lastItemSeq },
    { headers: CONTRACT_HEADERS },
  );
  let resolveCancelStarted;
  const cancelStarted = new Promise((resolve) => {
    resolveCancelStarted = resolve;
  });
  const cancelTiming = { firstFrames: [] };
  const cancelReader = collectStream(
    cancelStream,
    (event) => turnTerminalReached(event, expectedCancelTurnId),
    TIMEOUT_MS,
    (event) => {
      if (cancelTiming.firstFrames.length < 8) {
        cancelTiming.firstFrames.push({
          atMs: Date.now() - cancelStreamStarted,
          label: frameLabel(event),
          turnId: officialEventTurnId(event),
        });
      }
      if (event?.type === 'agent.session.turn.created' && event.turn?.id === expectedCancelTurnId) {
        cancelTiming.turnStartedAtMs = Date.now() - cancelStreamStarted;
        resolveCancelStarted(true);
      }
    },
  );
  await sleep(100);
  const cancelAccepted = await client.post(`/agents/sessions/${createdSessionId}/events`, {
    body: {
      inputId: cancelInputId,
      kind: 'user',
      mode: 'turn',
      text: cancelText,
    },
    headers: CONTRACT_HEADERS,
  });
  check('cancel probe input was accepted', cancelAccepted?.accepted === true, cancelAccepted);
  const cancelTurnStarted = await Promise.race([
    cancelStarted,
    sleep(5000).then(() => false),
  ]);
  check('cancel probe turn started before the cancel', cancelTurnStarted === true, {
    expectedTurnId: expectedCancelTurnId,
    input: cancelAccepted,
  });
  // turn.created 先于宿主开始 provider 请求；确认 stub 已收到该轮，才测试在途取消。
  let providerStarted = false;
  const providerDeadline = Date.now() + 5000;
  while (Date.now() < providerDeadline) {
    const stats = await stubStats();
    if (stats.turns.some((turn) => turn.lastUserText === cancelText)) {
      providerStarted = true;
      break;
    }
    await sleep(50);
  }
  check('cancel probe reached the model before cancellation', providerStarted);
  cancelTiming.controlSentAtMs = Date.now() - cancelStreamStarted;
  const cancelResponse = await sessionEvents.create(createdSessionId, {
    'Idempotency-Key': cancelControlId,
    events: [{ type: 'agent.session.input.cancel' }],
  }, { headers: CONTRACT_HEADERS }).asResponse();
  check('SDK cancel returned HTTP 202', cancelResponse.status === 202, cancelResponse.status);
  const cancelControl = await cancelResponse.json();
  cancelTiming.controlReturnedAtMs = Date.now() - cancelStreamStarted;
  summary.timings.cancelPostMs = Date.now() - cancelStreamStarted;
  summary.cancelAccepted = { input: cancelAccepted, control: cancelControl };
  check('control cancel was accepted', cancelControl?.accepted === true, cancelControl);
  check('control cancel echoes the control input id', cancelControl?.inputId === cancelControlId, cancelControl);
  const cancelled = await cancelReader;
  const cancelEvents = cancelled.events.filter(
    (event) => eventBelongsToTurn(event, expectedCancelTurnId, cancelAccepted.lastItemSeq),
  );
  cancelTiming.terminalAtMs = Date.now() - cancelStreamStarted;
  summary.timings.cancelStreamMs = Date.now() - cancelStreamStarted;
  summary.cancelTiming = cancelTiming;
  summary.cancelStream = {
    rawEventCount: cancelled.events.length,
    rawEventTypes: countFrames(cancelled.events),
    turnEventCount: cancelEvents.length,
    eventTypes: countFrames(cancelEvents),
    officialTypes: officialTypes(cancelEvents),
    outcome: cancelled.outcome,
    error: cancelled.error?.message ?? null,
  };
  check('cancel stream reached a terminal official frame', cancelled.outcome === 'terminal', {
    outcome: cancelled.outcome,
    error: cancelled.error?.message ?? null,
  });
  check('cancel stream did not throw in the SDK reader', cancelled.error === null, cancelled.error?.message);
  const cancelRawGaps = assertNoRawGap(cancelled.events, 'cancel stream (raw)');
  summary.cancelStream.rawGapCount = cancelRawGaps.length;
  summary.cancelStream.rawGapFrames = cancelRawGaps;
  summary.cancelStream.rawFrameSamples = rawFrameSample(cancelled.events);
  check(
    'cancel stream observed the submitted turn',
    cancelEvents.length > 0,
    { expectedTurnId: expectedCancelTurnId, rawEventTypes: summary.cancelStream.rawEventTypes },
  );
  const cancelCompleted = findOfficial(cancelEvents, 'agent.session.turn.completed');
  const cancelIdle = findOfficial(cancelEvents, 'agent.session.idle');
  const cancelGatewayTurn = cancelEvents.find(
    (event) => isTurnFrame(event) && event.state === 'completed',
  );
  check('cancel stream carries turn.completed', Boolean(cancelCompleted), officialTypes(cancelEvents));
  assertTerminalTurnIds(cancelEvents, expectedCancelTurnId, 'cancel stream');
  check(
    'cancel request reached a terminal state',
    typeof cancelCompleted?.turn?.status === 'string'
      && (cancelCompleted.turn.status === 'completed' || cancelCompleted.turn.status === 'cancelled'),
    cancelCompleted?.turn?.status,
  );
  check('cancel stream ends with agent.session.idle', Boolean(cancelIdle), officialTypes(cancelEvents));
  check('cancelled session is idle', cancelIdle?.session?.status === 'idle', cancelIdle?.session?.status);
  check('cancel stream emitted no error frame', officialIndex(cancelEvents, 'error') === -1);
  check('cancel stream emitted no failure frame', officialIndex(cancelEvents, 'agent.session.failed') === -1);
  summary.cancelFrames = {
    turn_id: cancelCompleted?.turn_id,
    official_status: cancelCompleted?.turn?.status,
    gateway_stop_reason: cancelGatewayTurn?.stopReason ?? null,
    session_status: cancelIdle?.session?.status,
  };
  if (cancelCompleted?.turn?.status !== 'cancelled') {
    recordDifference(
      '真实 provider 流上的官方取消未以 cancelled 终止',
      { ...cancelTiming, officialStatus: cancelCompleted?.turn?.status, gatewayStopReason: cancelGatewayTurn?.stopReason ?? null },
      '网关受理了取消，但宿主未报告 cancelled。脚本将宿主终态原样记录，不把它算作取消成功。',
    );
  }

  const afterCancelWait = await waitForSession(
    client,
    createdSessionId,
    (session) => session.state === 'idle' && session.status === 'idle',
  );
  const afterCancel = afterCancelWait.session;
  summary.afterCancel = {
    state: afterCancel.state,
    status: afterCancel.status,
    statusSettleMs: afterCancelWait.settleMs,
    lastItemSeq: afterCancel.lastItemSeq,
    error: afterCancel.error ?? null,
  };
  check('cancelled session status settles to idle', afterCancelWait.settleMs !== null, afterCancelWait.observed);
  check('cancelled turn left the session idle', afterCancel.state === 'idle', afterCancel.state);
  check('cancelled turn status is idle', afterCancel.status === 'idle', afterCancel.status);

  // 7. official turns list --------------------------------------------------
  const turnsStarted = Date.now();
  const turnsPage = await client.beta.agents.sessions.turns.list(
    createdSessionId,
    { order: 'asc', limit: 10 },
    { headers: CONTRACT_HEADERS },
  );
  summary.timings.turnsMs = Date.now() - turnsStarted;
  const turns = turnsPage.data ?? [];
  summary.turns = {
    count: turns.length,
    hasMore: turnsPage.has_more,
    bodyHasMore: turnsPage.body?.has_more,
    bodyHasMoreCamel: turnsPage.body?.hasMore,
    turns: turns.map((turn) => ({
      id: turn.id,
      status: turn.status,
      agent_id: turn.agent_id,
      subagent_id: turn.subagent_id,
      created_at: turn.created_at,
      completed_at: turn.completed_at,
      error: turn.error ?? null,
      usage: turn.usage ?? null,
    })),
  };
  check(
    'turns list exposes boolean has_more and hasMore',
    typeof turnsPage.body?.has_more === 'boolean' && typeof turnsPage.body?.hasMore === 'boolean',
    { has_more: turnsPage.body?.has_more, hasMore: turnsPage.body?.hasMore },
  );
  check(
    'turns list answers has_more and hasMore consistently',
    turnsPage.body.has_more === turnsPage.body.hasMore,
    { has_more: turnsPage.body.has_more, hasMore: turnsPage.body.hasMore },
  );
  check(
    'turns SDK has_more matches the body',
    typeof turnsPage.has_more === 'boolean' && turnsPage.has_more === turnsPage.body.has_more,
    { sdk: turnsPage.has_more, body: turnsPage.body.has_more },
  );
  check('turns list has three turns', turns.length === 3, turns.map((turn) => turn.id));
  const turnById = new Map(turns.map((turn) => [turn.id, turn]));
  check(
    'turn ids match the create, follow-up and cancel streams',
    turnById.has(createTurnId)
      && turnById.has(followUpTurnId)
      && turnById.has(expectedCancelTurnId),
    { createTurnId, followUpTurnId, cancelTurnId: expectedCancelTurnId, list: turns.map((turn) => turn.id) },
  );
  check(
    'create turn persisted as completed',
    turnById.get(createTurnId)?.status === 'completed',
    turnById.get(createTurnId)?.status,
  );
  check(
    'follow-up turn persisted as completed',
    turnById.get(followUpTurnId)?.status === 'completed',
    turnById.get(followUpTurnId)?.status,
  );
  const persistedCancelStatus = turnById.get(expectedCancelTurnId)?.status;
  check(
    'cancel turn persisted the stream terminal status',
    typeof persistedCancelStatus === 'string'
      && persistedCancelStatus === cancelCompleted?.turn?.status,
    {
      persisted: persistedCancelStatus,
      stream: cancelCompleted?.turn?.status,
    },
  );
  recordDifference(
    'turn 的子 agent 与 usage',
    { agent_id: turns.map((turn) => turn.agent_id), subagent_id: turns.map((turn) => turn.subagent_id) },
    '本地运行时只有 root agent；agent_id 恒 root、subagent_id 恒 null。',
  );
  recordDifference(
    '模型 400 与 turn 终态',
    {
      probe: 'unknown [[tool:...]] directive -> stub HTTP 400',
      observed: 'ACP prompt 返回正常 stop reason，turn 以 completed 结束且无 assistant 输出',
    },
    'provider 400 未被网关折叠为 turn.failed；这是开发期观察，脚本改用确定性的 control=cancel 覆盖取消路径。',
  );

  // Evidence chain: the same script blob and git HEAD must hold across the run.
  summary.script.sha256AtEnd = scriptSha256();
  summary.script.sha256Stable = summary.script.sha256AtEnd === summary.script.sha256AtStart;
  summary.git.parentCommitAtEnd = gitParentCommit();
  summary.git.parentCommitStable = typeof summary.git.parentCommitAtStart === 'string'
    && summary.git.parentCommitAtStart.length > 0
    && summary.git.parentCommitAtEnd === summary.git.parentCommitAtStart;
  check('script blob is unchanged during the run', summary.script.sha256Stable, {
    start: summary.script.sha256AtStart,
    end: summary.script.sha256AtEnd,
  });
  check('git parent commit is available and unchanged during the run', summary.git.parentCommitStable, {
    start: summary.git.parentCommitAtStart,
    end: summary.git.parentCommitAtEnd,
  });

  summary.ok = true;
  summary.exitCode = 0;
  summary.assertions = checks;
  summary.assertionCount = checks.length;
  summary.differences = differences;
  summary.rawFrameCounts = {
    create: create.events.length,
    followUp: followUpRun.events.length,
    reconnect: replayRun.events.length,
    cancel: cancelled.events.length,
  };
  summary.rawGapCounts = {
    create: createRawGaps.length,
    followUp: followUpRawGaps.length,
    reconnect: replayRawGaps.length,
    cancel: cancelRawGaps.length,
  };
  summary.finishedAt = new Date().toISOString();

  if (summary.evidence.rawFramesPath) {
    fs.writeFileSync(
      summary.evidence.rawFramesPath,
      `${JSON.stringify(
        {
          script: summary.script,
          git: summary.git,
          create: create.events,
          followUp: followUpRun.events,
          reconnect: replayRun.events,
          cancel: cancelled.events,
        },
        null,
        2,
      )}\n`,
    );
  }
  if (summary.evidence.outputPath) {
    fs.writeFileSync(summary.evidence.outputPath, `${JSON.stringify(summary, null, 2)}\n`);
  }
  return summary;
}

// Contract self-test for the raw-gap assertion. The e2e path uses the same
// helper; this mode proves a gap frame exits non-zero without needing a live
// stack or credentials. `clean` exits 0, `gap` exits 1.
const selfTestMode = process.env.PERI_E2E_CONTRACT_SELF_TEST;
if (selfTestMode !== undefined) {
  try {
    if (selfTestMode === 'clean') {
      assertNoRawGap([{ type: 'heartbeat', lastItemSeq: 1 }], 'self-test');
    } else if (selfTestMode === 'gap') {
      assertNoRawGap([{ fromSeq: 3, toSeq: 4 }], 'self-test');
    } else {
      throw new Error(`unknown self-test mode: ${selfTestMode}`);
    }
    process.stdout.write(`${JSON.stringify({ ok: true, mode: selfTestMode, checks })}\n`);
    process.exit(0);
  } catch (error) {
    process.stdout.write(
      `${JSON.stringify({ ok: false, mode: selfTestMode, error: error?.message ?? String(error), checks })}\n`,
    );
    process.exit(1);
  }
}

main()
  .then((summary) => {
    process.stdout.write(`${JSON.stringify(summary, null, 2)}\n`, () => process.exit(0));
  })
  .catch((error) => {
    const failure = `${JSON.stringify(
      {
        ok: false,
        exitCode: 1,
        error: error?.message ?? String(error),
        checks,
        differences,
        partialSummary: summary,
      },
      null,
      2,
    )}\n`;
    process.stdout.write(failure, () => process.exit(1));
  });
