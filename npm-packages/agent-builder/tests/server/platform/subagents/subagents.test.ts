import { afterEach, describe, expect, test } from 'bun:test';
import {
  installSubagentHistoryReader,
  StoreHistoryUnavailableError,
  StoreSchemaUnsupportedError,
  type SubagentHistoryReader,
} from '../../../../server/platform/subagents/history';
import {
  createSession,
  eventTypes,
  get,
  type Harness,
  json,
  sessionEvents,
  setup,
} from '../sessions/helpers';
import { emitSubagentTask, newChildThreadId, seedItem, seedSubagent, seedTurn } from './helpers';

let harness: Harness | null = null;
const restores: (() => void)[] = [];

afterEach(async () => {
  for (const restore of restores.splice(0)) restore();
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

function installReader(h: Harness, reader: SubagentHistoryReader): void {
  restores.push(installSubagentHistoryReader(h.db, reader));
}

const SUBAGENT_FIELDS = [
  'closed_at',
  'id',
  'instructions',
  'name',
  'object',
  'opened_at',
  'parent_agent_id',
  'session_id',
  'status',
];

describe('subagent resources', () => {
  test('lists stored subagents in the documented shape and descending order', async () => {
    const h = start();
    const sessionId = await newSession(h);
    const now = Date.now();
    const older = seedSubagent(h, sessionId, {
      name: 'researcher',
      openedAt: now - 2_000,
      instructions: 'Investigate the failing test',
    });
    const newer = seedSubagent(h, sessionId, {
      name: 'reviewer',
      status: 'closed',
      openedAt: now - 1_000,
    });

    const response = await get(h, `/agents/sessions/${sessionId}/subagents`);
    expect(response.status).toBe(200);
    const body = await json<{ data: Record<string, unknown>[]; has_more: boolean }>(response);
    expect(body.data.map((item) => item.id)).toEqual([newer.id, older.id]);
    expect(body.has_more).toBe(false);

    const first = body.data[1];
    expect(first).toBeDefined();
    expect(Object.keys(first ?? {}).sort()).toEqual(SUBAGENT_FIELDS);
    expect(first).toMatchObject({
      object: 'agent.session.subagent',
      id: older.id,
      session_id: sessionId,
      name: 'researcher',
      instructions: [{ type: 'output_text', text: 'Investigate the failing test' }],
      parent_agent_id: 'agent-parent',
      status: 'active',
      closed_at: null,
    });
    expect(first?.opened_at).toBeNumber();
    expect(Math.abs(Number(first?.opened_at) - Math.floor(Date.now() / 1000))).toBeLessThan(5);

    const closed = body.data[0];
    expect(closed).toMatchObject({ id: newer.id, status: 'closed' });
    expect(closed?.closed_at).toBeNumber();
  });

  test('paginates by ID and validates the page parameters', async () => {
    const h = start();
    const sessionId = await newSession(h);
    seedSubagent(h, sessionId);
    seedSubagent(h, sessionId);

    const first = await get(h, `/agents/sessions/${sessionId}/subagents?limit=1&order=asc`);
    const firstPage = await json<{ data: { id: string }[]; has_more: boolean; last_id: string }>(first);
    expect(firstPage.data).toHaveLength(1);
    expect(firstPage.has_more).toBe(true);

    const second = await get(
      h,
      `/agents/sessions/${sessionId}/subagents?limit=1&order=asc&after=${firstPage.last_id}`,
    );
    const secondPage = await json<{ data: { id: string }[]; has_more: boolean }>(second);
    expect(secondPage.has_more).toBe(false);
    expect(secondPage.data[0]?.id).not.toBe(firstPage.data[0]?.id);

    const invalid = await get(h, `/agents/sessions/${sessionId}/subagents?limit=0`);
    expect(invalid.status).toBe(400);
    const error = await json<{ error: { code: string; param: string | null } }>(invalid);
    expect(error.error).toMatchObject({ code: 'invalid_input', param: 'limit' });
  });

  test('retrieves one subagent and scopes it to its session', async () => {
    const h = start();
    const sessionId = await newSession(h);
    const otherSessionId = await newSession(h);
    const subagent = seedSubagent(h, sessionId, { name: 'researcher' });

    const found = await get(h, `/agents/sessions/${sessionId}/subagents/${subagent.id}`);
    expect(found.status).toBe(200);
    expect(await json(found)).toMatchObject({
      id: subagent.id,
      object: 'agent.session.subagent',
      name: 'researcher',
    });

    const wrongSession = await get(h, `/agents/sessions/${otherSessionId}/subagents/${subagent.id}`);
    expect(wrongSession.status).toBe(404);

    const unknown = await get(h, `/agents/sessions/${sessionId}/subagents/${newChildThreadId()}`);
    expect(unknown.status).toBe(404);
    expect(await json(unknown)).toMatchObject({
      error: { type: 'not_found_error', code: 'not_found', param: null },
    });
  });

  test('answers 404 for every subagent route when the session does not exist', async () => {
    const h = start();
    const sessionId = crypto.randomUUID();
    const subagentId = crypto.randomUUID();
    for (const path of [
      `/agents/sessions/${sessionId}/subagents`,
      `/agents/sessions/${sessionId}/subagents/${subagentId}`,
      `/agents/sessions/${sessionId}/subagents/${subagentId}/items`,
      `/agents/sessions/${sessionId}/subagents/${subagentId}/turns`,
      `/agents/sessions/${sessionId}/subagents/${subagentId}/turns/${crypto.randomUUID()}`,
      `/agents/sessions/${sessionId}/subagents/${subagentId}/turns/${crypto.randomUUID()}/items`,
    ]) {
      const response = await get(h, path);
      expect({ path, status: response.status }).toEqual({ path, status: 404 });
    }
  });
});

describe('subagent projection', () => {
  test('adopts a running subagent, then closes it on the terminal task state', async () => {
    const h = start();
    const sessionId = await newSession(h);
    const childId = newChildThreadId();

    await emitSubagentTask(h, sessionId, { instanceId: childId, name: 'researcher' });
    const active = h.repos.subagents.get(childId);
    expect(active).not.toBeNull();
    expect(active).toMatchObject({
      id: childId,
      session_id: sessionId,
      name: 'researcher',
      parent_agent_id: h.agentId,
      status: 'active',
      closed_at: null,
    });
    expect(active?.opened_at).toBeNumber();
    const created = eventTypes(h, sessionId);
    expect(created).toContain('agent.session.subagent.created');
    expect(created).toContain('agent.session.subagent.active');
    expect(created.indexOf('agent.session.subagent.created')).toBeLessThan(
      created.indexOf('agent.session.subagent.active'),
    );
    const createdEvent = sessionEvents(h, sessionId).find(
      (event) => event.type === 'agent.session.subagent.created',
    );
    expect(createdEvent?.data.subagent).toMatchObject({
      id: childId,
      object: 'agent.session.subagent',
      session_id: sessionId,
      name: 'researcher',
      status: 'active',
      closed_at: null,
    });

    const listed = await get(h, `/agents/sessions/${sessionId}/subagents`);
    expect((await json<{ data: { id: string; status: string }[] }>(listed)).data).toEqual([
      expect.objectContaining({ id: childId, status: 'active' }),
    ]);

    await emitSubagentTask(h, sessionId, { instanceId: childId, status: 'completed' });
    const closed = h.repos.subagents.get(childId);
    expect(closed?.status).toBe('closed');
    expect(closed?.closed_at).toBeNumber();
    expect(eventTypes(h, sessionId)).toContain('agent.session.subagent.closed');

    const types = eventTypes(h, sessionId);
    expect(types.filter((type) => type.startsWith('agent.session.turn.'))).toEqual([]);
  });

  test('adopts an already finished subagent silently instead of replaying its lifecycle', async () => {
    const h = start();
    const sessionId = await newSession(h);
    const childId = newChildThreadId();

    await emitSubagentTask(h, sessionId, { instanceId: childId, status: 'failed' });

    const row = h.repos.subagents.get(childId);
    expect(row?.status).toBe('closed');
    const types = eventTypes(h, sessionId);
    expect(types).not.toContain('agent.session.subagent.created');
    expect(types).not.toContain('agent.session.subagent.closed');
  });

  test('ignores subagent tasks whose instance is not a child thread UUID', async () => {
    const h = start();
    const sessionId = await newSession(h);

    await emitSubagentTask(h, sessionId, { instanceId: 'agent-99', name: 'researcher' });

    expect(h.repos.subagents.listBySession(sessionId)).toEqual([]);
    expect(eventTypes(h, sessionId).filter((type) => type.includes('subagent'))).toEqual([]);
  });
});

describe('subagent history', () => {
  test('serves turns and items from the platform tables', async () => {
    const h = start();
    const sessionId = await newSession(h);
    const subagent = seedSubagent(h, sessionId);
    const firstTurn = seedTurn(h, sessionId, subagent.id);
    const secondTurn = seedTurn(h, sessionId, subagent.id, { status: 'completed' });
    seedItem(h, sessionId, firstTurn.id, 0, 'first item');
    seedItem(h, sessionId, firstTurn.id, 1, 'second item');
    seedItem(h, sessionId, secondTurn.id, 0, 'third item');

    const turns = await get(h, `/agents/sessions/${sessionId}/subagents/${subagent.id}/turns`);
    expect(turns.status).toBe(200);
    const turnBody = await json<{
      data: Record<string, unknown>[];
      first_id: string;
      last_id: string;
      has_more: boolean;
    }>(turns);
    expect(turnBody.data.map((turn) => turn.id)).toEqual([secondTurn.id, firstTurn.id]);
    expect(turnBody.first_id).toBe(secondTurn.id);
    expect(turnBody.has_more).toBe(false);
    expect(turnBody.data[1]).toMatchObject({
      object: 'agent.session.turn',
      session_id: sessionId,
      subagent_id: subagent.id,
      status: 'completed',
    });
    expect(turnBody.data[1]?.created_at).toBeNumber();

    const turn = await get(h, `/agents/sessions/${sessionId}/subagents/${subagent.id}/turns/${firstTurn.id}`);
    expect(turn.status).toBe(200);
    expect(await json(turn)).toMatchObject({ id: firstTurn.id, subagent_id: subagent.id });

    const turnItems = await get(
      h,
      `/agents/sessions/${sessionId}/subagents/${subagent.id}/turns/${firstTurn.id}/items?order=asc`,
    );
    const turnItemBody = await json<{ data: { id: string; turn_id: string }[] }>(turnItems);
    expect(turnItemBody.data.map((item) => item.turn_id)).toEqual([firstTurn.id, firstTurn.id]);

    const allItems = await get(
      h,
      `/agents/sessions/${sessionId}/subagents/${subagent.id}/items?order=asc&limit=2`,
    );
    const itemBody = await json<{
      data: { id: string; turn_id: string }[];
      has_more: boolean;
      last_id: string;
    }>(allItems);
    expect(itemBody.data).toHaveLength(2);
    expect(itemBody.has_more).toBe(true);

    const nextItems = await get(
      h,
      `/agents/sessions/${sessionId}/subagents/${subagent.id}/items?order=asc&limit=2&after=${itemBody.last_id}`,
    );
    const nextBody = await json<{ data: { id: string }[]; has_more: boolean }>(nextItems);
    expect(nextBody.data).toHaveLength(1);
    expect(nextBody.has_more).toBe(false);

    const other = seedSubagent(h, sessionId);
    const foreignTurn = seedTurn(h, sessionId, other.id);
    const foreign = await get(
      h,
      `/agents/sessions/${sessionId}/subagents/${subagent.id}/turns/${foreignTurn.id}`,
    );
    expect(foreign.status).toBe(404);
  });

  test('answers 503 for every history read when the Store is unavailable', async () => {
    const h = start();
    const sessionId = await newSession(h);
    const subagent = seedSubagent(h, sessionId);
    const turn = seedTurn(h, sessionId, subagent.id);
    installReader(h, {
      probe: () => {
        throw new StoreHistoryUnavailableError('down');
      },
      read: async () => {
        throw new StoreHistoryUnavailableError('down');
      },
    });

    for (const path of [
      `/agents/sessions/${sessionId}/subagents/${subagent.id}/turns`,
      `/agents/sessions/${sessionId}/subagents/${subagent.id}/items`,
      `/agents/sessions/${sessionId}/subagents/${subagent.id}/turns/${turn.id}`,
      `/agents/sessions/${sessionId}/subagents/${subagent.id}/turns/${turn.id}/items`,
      `/agents/sessions/${sessionId}/subagents/${subagent.id}/turns/${crypto.randomUUID()}`,
    ]) {
      const response = await get(h, path);
      expect(response.status).toBe(503);
      expect(await json(response)).toStrictEqual({
        error: {
          type: 'server_error',
          code: 'subagent_history_unavailable',
          message: 'Subagent history is unavailable',
          param: null,
        },
      });
    }
  });

  test('answers 409 when the Store schema does not match the history contract', async () => {
    const h = start();
    const sessionId = await newSession(h);
    const subagent = seedSubagent(h, sessionId);
    installReader(h, {
      probe: () => {
        throw new StoreSchemaUnsupportedError('columns differ');
      },
      read: async () => ({ turns: [] }),
    });

    const response = await get(h, `/agents/sessions/${sessionId}/subagents/${subagent.id}/items`);
    expect(response.status).toBe(409);
    expect(await json(response)).toMatchObject({
      error: { code: 'store_schema_unsupported', type: 'conflict_error', param: null },
    });
  });

  test('maps child-thread Store turns and items into the documented resources', async () => {
    const h = start();
    const sessionId = await newSession(h);
    const subagent = seedSubagent(h, sessionId);
    installReader(h, {
      probe: () => {},
      read: async () => ({
        turns: [
          {
            id: 'store-turn-1',
            status: 'completed',
            created_at: 1_700_000_000,
            started_at: 1_700_000_001,
            completed_at: 1_700_000_002,
            items: [
              { id: 'store-item-1', type: 'message', text: 'one' },
              { id: 'store-item-2', type: 'message', text: 'two' },
            ],
          },
          {
            id: 'store-turn-2',
            status: 'failed',
            created_at: 1_700_000_010,
            started_at: null,
            completed_at: 1_700_000_011,
            items: [{ id: 'store-item-3', type: 'message', text: 'three' }],
          },
        ],
      }),
    });

    const turns = await get(h, `/agents/sessions/${sessionId}/subagents/${subagent.id}/turns?order=asc`);
    const turnBody = await json<{ data: Record<string, unknown>[] }>(turns);
    expect(turnBody.data.map((turn) => turn.id)).toEqual(['store-turn-1', 'store-turn-2']);
    expect(turnBody.data[0]).toMatchObject({
      object: 'agent.session.turn',
      session_id: sessionId,
      subagent_id: subagent.id,
      agent_id: subagent.parent_agent_id,
      status: 'completed',
      created_at: 1_700_000_000,
      started_at: 1_700_000_001,
      completed_at: 1_700_000_002,
    });

    const items = await get(h, `/agents/sessions/${sessionId}/subagents/${subagent.id}/items?order=asc`);
    const itemBody = await json<{ data: { id: string; turn_id: string }[] }>(items);
    expect(itemBody.data).toEqual([
      expect.objectContaining({ id: 'store-item-1', turn_id: 'store-turn-1' }),
      expect.objectContaining({ id: 'store-item-2', turn_id: 'store-turn-1' }),
      expect.objectContaining({ id: 'store-item-3', turn_id: 'store-turn-2' }),
    ]);

    const turnItems = await get(
      h,
      `/agents/sessions/${sessionId}/subagents/${subagent.id}/turns/store-turn-1/items`,
    );
    const turnItemBody = await json<{ data: { id: string }[] }>(turnItems);
    expect(turnItemBody.data.map((item) => item.id)).toEqual(['store-item-2', 'store-item-1']);

    const detail = await get(h, `/agents/sessions/${sessionId}/subagents/${subagent.id}/turns/store-turn-2`);
    expect(await json(detail)).toMatchObject({ id: 'store-turn-2', status: 'failed' });

    const unknown = await get(h, `/agents/sessions/${sessionId}/subagents/${subagent.id}/turns/missing`);
    expect(unknown.status).toBe(404);
  });
});
