#!/usr/bin/env node
// S9a1 end-to-end driver: the official `openai@7.62.0` Agents API against the
// local `peri-swarm` gateway (real Loom session/state databases, real Rivet
// engine, local deterministic model stub).
//
// Five session methods:
//   1. create   POST /v1/agents/sessions            client.beta.agents.sessions.create
//   2. get      GET  /v1/agents/sessions/{id}       client.beta.agents.sessions.retrieve
//   3. event    POST /v1/agents/sessions/{id}/events client.beta.agents.sessions.events.create
//      (the reference request; see `highLevelEvent` in the output)
//   4. events   GET  /v1/agents/sessions/{id}/events client.beta.agents.sessions.events.stream
//   5. items    GET  /v1/agents/sessions/{id}/items  client.beta.agents.sessions.items.list
//
// The SDK's `events.create` body is the OpenAI Agents API shape
// (`{events: [...]}`). The Swarm gateway takes one submission per request
// (`{inputId, kind, mode, text}`), so method 3 is issued with the same official
// client through its low-level `client.post`; the high-level request is still
// sent first and its wire answer is recorded, never faked.
//
// Requirements (no dependency is vendored in this repository):
//   npm install --prefix /tmp/peri-s9a1-sdk openai@7.62.0
//   NODE_PATH=/tmp/peri-s9a1-sdk/node_modules node peri-swarm/scripts/sdk_sessions_e2e.mjs
//
// Environment:
//   PERI_SWARM_BASE_URL   default http://127.0.0.1:18080/v1
//   PERI_SWARM_API_KEY    deployment bearer key (required)
//   PERI_SWARM_MODEL      default stub-model
//   PERI_MODEL_STUB_URL   default http://127.0.0.1:9496
//   PERI_E2E_TIMEOUT_MS   default 180000
//
// Exit code 0 only when every assertion in `assertions` passes. The JSON
// summary on stdout is the evidence record; no credential is ever printed.

import crypto from 'node:crypto';
import fs from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';

const require = createRequire(import.meta.url);
const OpenAI = require('openai');

const BASE_URL = process.env.PERI_SWARM_BASE_URL ?? 'http://127.0.0.1:18080/v1';
const API_KEY = process.env.PERI_SWARM_API_KEY ?? '';
const MODEL = process.env.PERI_SWARM_MODEL ?? 'stub-model';
const STUB_URL = process.env.PERI_MODEL_STUB_URL ?? 'http://127.0.0.1:9496';
const TIMEOUT_MS = Number(process.env.PERI_E2E_TIMEOUT_MS ?? '180000');
const CONTRACT_HEADERS = { 'x-peri-swarm-expect': '1', 'OpenAI-Beta': 'agents=v1' };

if (!API_KEY) {
  console.error('PERI_SWARM_API_KEY is required');
  process.exit(2);
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

function sleep(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

async function stubStats() {
  const response = await fetch(`${STUB_URL}/__stats`);
  if (!response.ok) throw new Error(`model stub /__stats returned ${response.status}`);
  return response.json();
}

function findStubTurn(stats, expectedText) {
  const turns = Array.isArray(stats.turns) ? stats.turns : [];
  const exact = [...turns].reverse().find((turn) => turn.lastUserText.trim() === expectedText.trim());
  if (exact) return exact;
  return [...turns].reverse().find((turn) => turn.lastUserText.includes(expectedText.trim()));
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
    text: typeof item.payload?.text === 'string' ? item.payload.text : undefined,
  }));
}

async function main() {
  const version = sdkVersion();
  const summary = {
    sdk: { package: 'openai', version },
    baseUrl: BASE_URL,
    model: MODEL,
    stubUrl: STUB_URL,
    startedAt: new Date().toISOString(),
    timings: {},
  };

  const client = new OpenAI({
    baseURL: BASE_URL,
    apiKey: API_KEY,
    maxRetries: 0,
    timeout: TIMEOUT_MS,
  });

  // A fresh stub counter makes the deterministic reply index repeatable.
  const reset = await fetch(`${STUB_URL}/__reset`, { method: 'POST' });
  check('model stub reset', reset.ok, { status: reset.status });

  const initialInput = 'S9a1 initial turn: reply with the deterministic stub text.';
  const followUp = 'S9a1 follow-up turn: this answer must land in the item log.';

  // 1. create ---------------------------------------------------------------
  const createStarted = Date.now();
  const created = await client.beta.agents.sessions.create(
    {
      environment: { type: 'none' },
      agent: {
        model: MODEL,
        instructions: 'S9a1 local verification agent. Return the stub text unchanged.',
      },
      input: initialInput,
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
    agent: created.agent,
  };
  check('create returned a session id', typeof created.id === 'string' && created.id.length > 0);
  check('create object is agent.session', created.object === 'agent.session', created.object);
  check('create reports created=true', created.created === true);
  check('create environment.type=none', created.environment?.type === 'none', created.environment);
  check('create agent.model is the requested model', created.agent?.model === MODEL);
  check('create carried the instructions', String(created.agent?.instructions ?? '').includes('S9a1'));
  check('create left the session idle', created.state === 'idle', created.state);
  check('create persisted the first turn', created.lastItemSeq >= 2, created.lastItemSeq);
  check('create id equals logicalId', created.id === created.logicalId);

  // 2. get ------------------------------------------------------------------
  const getStarted = Date.now();
  const fetched = await client.beta.agents.sessions.retrieve(created.id, {
    headers: CONTRACT_HEADERS,
  });
  summary.timings.retrieveMs = Date.now() - getStarted;
  summary.retrieved = {
    id: fetched.id,
    rootSessionId: fetched.rootSessionId,
    state: fetched.state,
    lastItemSeq: fetched.lastItemSeq,
    cwd: fetched.cwd,
    createdAt: fetched.createdAt,
    lastActivityAt: fetched.lastActivityAt,
    environment: fetched.environment,
  };
  check('get returns the same session', fetched.id === created.id, { got: fetched.id });
  check('get keeps the root session id', fetched.rootSessionId === created.rootSessionId);
  check('get reports idle', fetched.state === 'idle', fetched.state);
  check('get sees the create-turn items', fetched.lastItemSeq === created.lastItemSeq);
  check('get keeps the requested model', fetched.agent?.model === MODEL);

  // Initial items (asc) prove the first answer is in the log.
  const initialPage = await client.beta.agents.sessions.items.list(
    created.id,
    { order: 'asc', limit: 100 },
    { headers: CONTRACT_HEADERS },
  );
  check('items page object is list', initialPage.body?.object === 'list', initialPage.body?.object);
  check('items page carries the logical id', initialPage.body?.logicalId === created.id);
  const initialItems = initialPage.body?.items ?? [];
  const initialComplete = initialItems.find((item) => item.payload?.complete === true);
  check('first answer is a complete message item', Boolean(initialComplete), summarizeItems(initialItems));
  const initialStats = await stubStats();
  const initialTurn = findStubTurn(initialStats, initialInput);
  check('model stub saw the initial input', Boolean(initialTurn), initialStats.turns);
  check('first answer text equals the stub reply', initialComplete.payload.text === initialTurn.reply, {
    item: initialComplete.payload.text,
    stub: initialTurn.reply,
  });
  summary.initialTurn = {
    stubReply: initialTurn.reply,
    itemSeq: initialComplete.seq,
    itemId: initialComplete.itemId,
    deltaCount: initialItems.filter((item) => item.payload?.delta === true).length,
    items: summarizeItems(initialItems),
  };

  // 3. events POST (high-level probe, then the gateway's one-event body) -----
  let highLevelEvent = null;
  try {
    await client.beta.agents.sessions.events.create(
      created.id,
      {
        events: [
          {
            type: 'agent.session.input.message',
            input: [{ role: 'user', content: [{ type: 'input_text', text: followUp }] }],
          },
        ],
      },
      { headers: CONTRACT_HEADERS },
    );
    highLevelEvent = { accepted: true };
  } catch (error) {
    highLevelEvent = {
      accepted: false,
      status: error?.status,
      name: error?.name,
      message: error?.message,
      body: error?.error ?? null,
    };
  }
  summary.highLevelEvent = highLevelEvent;
  check(
    'high-level events.create is rejected by the one-event gateway contract (recorded, not reused)',
    highLevelEvent.accepted === false && highLevelEvent.status === 400,
    highLevelEvent,
  );

  // 4. events GET (SSE) opened BEFORE the follow-up so the deltas are live.
  const streamStarted = Date.now();
  const stream = await client.beta.agents.sessions.events.stream(
    created.id,
    { after: created.lastItemSeq },
    { headers: CONTRACT_HEADERS },
  );
  const streamOpenedAt = Date.now();
  summary.timings.sseOpenMs = streamOpenedAt - streamStarted;
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
  await sleep(200);

  const inputId = crypto.randomUUID();
  const postStarted = Date.now();
  summary.timings.ssePostGapMs = postStarted - streamOpenedAt;
  const accepted = await client.post(`/agents/sessions/${created.id}/events`, {
    body: { inputId, kind: 'user', mode: 'turn', text: followUp },
    headers: CONTRACT_HEADERS,
  });
  summary.timings.eventsPostMs = Date.now() - postStarted;
  summary.accepted = accepted;
  check('events POST accepted the input', accepted?.accepted === true, accepted);
  check('events POST echoes the input id', accepted?.inputId === inputId, accepted);
  check('events POST disposition inserted', accepted?.disposition === 'inserted', accepted);

  const streamOutcome = await Promise.race([
    sseDone,
    sleep(TIMEOUT_MS).then(() => 'timeout'),
  ]);
  summary.timings.sseMs = Date.now() - streamStarted;
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

  const sseItems = sse.events.filter((event) => event && typeof event.itemId === 'string' && typeof event.seq === 'number');
  const sseTurns = sse.events.filter((event) => event && typeof event.state === 'string');
  const sseOther = sse.events.filter((event) => !sseItems.includes(event) && !sseTurns.includes(event));
  summary.sse = {
    eventCount: sse.events.length,
    turnStates: sseTurns.map((event) => event.state),
    seqs: sseItems.map((item) => item.seq),
    items: summarizeItems(sseItems),
    other: sseOther.map((event) => {
      if (event?.fromSeq !== undefined) return { gap: event };
      if (event?.lastItemSeq !== undefined) return { heartbeat: event };
      return { unknown: event };
    }),
  };

  check('SSE reported exactly one turn', sseTurns.length === 2, sseTurns);
  check(
    'SSE turn is started then completed',
    sseTurns[0]?.state === 'started' && sseTurns[1]?.state === 'completed',
    sseTurns.map((event) => event.state),
  );
  check(
    'SSE turn events share one request id',
    sseTurns[0]?.requestId === sseTurns[1]?.requestId,
    sseTurns,
  );
  check('SSE carried no gap event', !sse.events.some((event) => event?.fromSeq !== undefined));
  check('SSE item seq starts right after create', sseItems[0]?.seq === created.lastItemSeq + 1, sseItems[0]?.seq);
  for (let index = 1; index < sseItems.length; index += 1) {
    if (sseItems[index].seq !== sseItems[index - 1].seq + 1) {
      check('SSE item seq is contiguous', false, {
        previous: sseItems[index - 1].seq,
        current: sseItems[index].seq,
      });
    }
  }
  const sseDeltas = sseItems.filter((item) => item.payload?.delta === true);
  const sseComplete = sseItems.find((item) => item.payload?.complete === true);
  const sseDeltaText = sseDeltas.map((item) => item.payload?.text ?? '').join('');
  check('SSE streamed at least two text deltas', sseDeltas.length >= 2, sseDeltas.length);
  check('SSE ended with a complete message', Boolean(sseComplete), summarizeItems(sseItems));
  check('SSE deltas concatenate to the complete text', sseDeltaText === sseComplete?.payload?.text, {
    delta: sseDeltaText,
    complete: sseComplete?.payload?.text,
  });
  check(
    'SSE deltas and the complete item share one item id',
    sseDeltas.every((item) => item.itemId === sseComplete?.itemId),
    { complete: sseComplete?.itemId, deltas: sseDeltas.map((item) => item.itemId) },
  );

  const finalStats = await stubStats();
  const followUpTurn = findStubTurn(finalStats, followUp);
  check('model stub saw the follow-up turn', Boolean(followUpTurn), finalStats.turns);
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

  const afterFollowUp = await client.beta.agents.sessions.retrieve(created.id, {
    headers: CONTRACT_HEADERS,
  });
  summary.afterFollowUp = {
    state: afterFollowUp.state,
    lastItemSeq: afterFollowUp.lastItemSeq,
    rootSessionId: afterFollowUp.rootSessionId,
  };
  check('follow-up turn left the session idle', afterFollowUp.state === 'idle', afterFollowUp.state);
  check(
    'follow-up advanced lastItemSeq to the SSE tail',
    afterFollowUp.lastItemSeq === sseItems[sseItems.length - 1]?.seq,
    { session: afterFollowUp.lastItemSeq, sse: sseItems[sseItems.length - 1]?.seq },
  );
  check('same session across every call', afterFollowUp.id === created.id);

  // 5. items GET (asc and desc, cursor paging) ------------------------------
  const itemsStarted = Date.now();
  const paginate = async (order, limit) => {
    const pages = [];
    let after;
    for (;;) {
      const query = { order, limit };
      if (after !== undefined) query.after = after;
      const page = await client.beta.agents.sessions.items.list(created.id, query, {
        headers: CONTRACT_HEADERS,
      });
      const body = page.body ?? {};
      pages.push({
        query,
        count: (body.items ?? []).length,
        nextCursor: body.nextCursor,
        hasMore: body.hasMore,
        lastItemSeq: body.lastItemSeq,
        order: body.order,
        sdkDataLength: page.data?.length,
        sdkHasMore: page.has_more,
      });
      check(`items ${order} page object is list`, body.object === 'list', body.object);
      check(`items ${order} page logical id`, body.logicalId === created.id, body.logicalId);
      check(`items ${order} page order`, body.order === order, body.order);
      check(`items ${order} page respects limit`, (body.items ?? []).length <= limit, body.items?.length);
      check(`items ${order} page lastItemSeq`, body.lastItemSeq === afterFollowUp.lastItemSeq, body.lastItemSeq);
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

  const ascPages = await paginate('asc', 2);
  const descPages = await paginate('desc', 2);
  summary.timings.itemsMs = Date.now() - itemsStarted;
  summary.items = { ascPages, descPages };

  const ascItems = [];
  const descItems = [];
  // Re-read the bodies by replaying the same windows is unnecessary: the pages
  // above already carry counts; fetch the full ascending set once for the
  // content assertions and compare it with the paged windows.
  const fullAsc = await client.beta.agents.sessions.items.list(
    created.id,
    { order: 'asc', limit: 500 },
    { headers: CONTRACT_HEADERS },
  );
  const fullItems = fullAsc.body?.items ?? [];
  summary.items.full = {
    count: fullItems.length,
    lastItemSeq: fullAsc.body?.lastItemSeq,
    hasMore: fullAsc.body?.hasMore,
    nextCursor: fullAsc.body?.nextCursor,
    items: summarizeItems(fullItems),
  };
  check('full asc read has no next page', fullAsc.body?.hasMore === false);
  check('full asc read covers every seq', fullItems.length === afterFollowUp.lastItemSeq, {
    count: fullItems.length,
    lastItemSeq: afterFollowUp.lastItemSeq,
  });
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
    const page = await client.beta.agents.sessions.items.list(created.id, query, {
      headers: CONTRACT_HEADERS,
    });
    return page.body;
  };
  for (const order of ['asc', 'desc']) {
    const pages = order === 'asc' ? ascPages : descPages;
    const collected = [];
    for (const page of pages) {
      const body = await readWindow(order, 2, page.query.after);
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
  check('items complete item matches the SSE item', loggedFollowUp?.itemId === sseComplete?.itemId, {
    items: loggedFollowUp?.itemId,
    sse: sseComplete?.itemId,
  });
  check('items complete item seq matches the SSE seq', loggedFollowUp?.seq === sseComplete?.seq);

  // The official CursorPage reads OpenAI's `data`/`has_more`; the Swarm
  // contract answers `items`/`hasMore`. Record both, assert only the contract.
  const firstPage = ascPages[0];
  summary.sdkPageShape = {
    note: 'openai@7.62.0 CursorPage reads body.data/body.has_more; peri-swarm answers body.items/body.hasMore',
    dataLength: firstPage.sdkDataLength,
    hasMore: firstPage.sdkHasMore,
  };
  check('SDK CursorPage did not auto-paginate a Swarm page', firstPage.sdkDataLength === 0, firstPage);

  summary.assertions = checks;
  summary.finishedAt = new Date().toISOString();
  return summary;
}

main()
  .then((summary) => {
    process.stdout.write(`${JSON.stringify(summary, null, 2)}\n`, () => process.exit(0));
  })
  .catch((error) => {
    const failure = `${JSON.stringify(
      {
        ok: false,
        error: error?.message ?? String(error),
        checks,
      },
      null,
      2,
    )}\n`;
    process.stdout.write(failure, () => process.exit(1));
  });
