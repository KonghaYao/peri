import { afterEach, describe, expect, test } from 'bun:test';
import { createSession, get, type Harness, json, setup } from '../sessions/helpers';

let harness: Harness | null = null;

afterEach(async () => {
  await harness?.dispose();
  harness = null;
});

function start(): Harness {
  harness = setup();
  return harness;
}

async function newSession(h: Harness): Promise<string> {
  const created = await createSession(h, {
    environment: { type: 'none' },
    agent_id: h.agentId,
    input: 'hello',
  });
  return created.id;
}

const EMPTY_PAGE = {
  object: 'list',
  data: [],
  first_id: null,
  last_id: null,
  has_more: false,
};

describe('session traces', () => {
  test('returns the empty documented page when no trace was published', async () => {
    const h = start();
    const sessionId = await newSession(h);

    const response = await get(h, `/agents/sessions/${sessionId}/traces`);
    expect(response.status).toBe(200);
    expect(await json(response)).toStrictEqual(EMPTY_PAGE);
  });

  test('answers 404 for an unknown session and 400 for invalid pages', async () => {
    const h = start();
    const unknown = await get(h, `/agents/sessions/${crypto.randomUUID()}/traces`);
    expect(unknown.status).toBe(404);
    expect(await json(unknown)).toMatchObject({ error: { code: 'not_found' } });

    const sessionId = await newSession(h);
    const invalid = await get(h, `/agents/sessions/${sessionId}/traces?limit=zero`);
    expect(invalid.status).toBe(400);
    expect(await json(invalid)).toMatchObject({
      error: { code: 'invalid_input', param: 'limit' },
    });
  });

  test('publishes stored OTLP documents keyed by root turn', async () => {
    const h = start();
    const sessionId = await newSession(h);
    const first = h.repos.traces.create({
      sessionId,
      turnId: 'turn-root-1',
      kind: 'model_call',
      data: JSON.stringify({ resourceSpans: [{ scopeSpans: [] }] }),
    });
    const second = h.repos.traces.create({
      sessionId,
      turnId: 'turn-root-2',
      kind: 'model_call',
      data: JSON.stringify({ resourceSpans: [] }),
    });

    const response = await get(h, `/agents/sessions/${sessionId}/traces?order=asc`);
    const body = await json<{ data: Record<string, unknown>[]; has_more: boolean }>(response);
    expect(body.has_more).toBe(false);
    expect(body.data.map((trace) => trace.id)).toEqual(['turn-root-1', 'turn-root-2']);

    const trace = body.data[0];
    expect(Object.keys(trace ?? {}).sort()).toEqual(['created_at', 'id', 'object', 'otlp', 'session_id']);
    expect(trace).toMatchObject({
      object: 'agent.session.trace',
      id: 'turn-root-1',
      session_id: sessionId,
      otlp: { resourceSpans: [{ scopeSpans: [] }] },
    });
    expect(trace?.created_at).toBeNumber();
    expect(Math.abs(Number(trace?.created_at) - Math.floor(Date.now() / 1000))).toBeLessThan(5);
    expect(first.id).not.toBe('turn-root-1');
    expect(second.id).not.toBe('turn-root-2');
  });

  test('falls back to the row ID without a turn and skips unpublished payloads', async () => {
    const h = start();
    const sessionId = await newSession(h);
    const untethered = h.repos.traces.create({
      sessionId,
      turnId: null,
      kind: 'model_call',
      data: '{"resourceSpans": []}',
    });
    h.repos.traces.create({ sessionId, turnId: 'turn-root-3', kind: 'model_call', data: 'not-json' });
    h.repos.traces.create({ sessionId, turnId: 'turn-root-3', kind: 'model_call', data: '[]' });
    h.repos.traces.create({
      sessionId,
      turnId: 'turn-root-4',
      kind: 'model_call',
      data: '{"resourceSpans": []}',
    });
    h.repos.traces.create({
      sessionId,
      turnId: 'turn-root-4',
      kind: 'model_call',
      data: '{"resourceSpans": [{"duplicate": true}]}',
    });

    const response = await get(h, `/agents/sessions/${sessionId}/traces?order=asc`);
    const body = await json<{ data: { id: string; otlp: Record<string, unknown> }[] }>(response);
    expect(body.data.map((trace) => trace.id)).toEqual([untethered.id, 'turn-root-4']);
    expect(body.data[1]?.otlp).toEqual({ resourceSpans: [] });
  });

  test('pages traces by ID in both directions', async () => {
    const h = start();
    const sessionId = await newSession(h);
    for (const turnId of ['turn-root-a', 'turn-root-b', 'turn-root-c']) {
      h.repos.traces.create({
        sessionId,
        turnId,
        kind: 'model_call',
        data: '{"resourceSpans": []}',
      });
    }

    const first = await get(h, `/agents/sessions/${sessionId}/traces?limit=2`);
    const firstPage = await json<{
      data: { id: string }[];
      has_more: boolean;
      last_id: string;
      first_id: string;
    }>(first);
    expect(firstPage.data.map((trace) => trace.id)).toEqual(['turn-root-c', 'turn-root-b']);
    expect(firstPage.first_id).toBe('turn-root-c');
    expect(firstPage.last_id).toBe('turn-root-b');
    expect(firstPage.has_more).toBe(true);

    const next = await get(h, `/agents/sessions/${sessionId}/traces?limit=2&after=${firstPage.last_id}`);
    const nextPage = await json<{ data: { id: string }[]; has_more: boolean }>(next);
    expect(nextPage.data.map((trace) => trace.id)).toEqual(['turn-root-a']);
    expect(nextPage.has_more).toBe(false);
  });
});
