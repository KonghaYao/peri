import { afterEach, describe, expect, test } from 'bun:test';
import { SESSION_INLINE_AGENT_ID } from '../../../../server/db/migrate';
import { automationBody, createSessionRow, del, get, type Harness, post, setup } from './helpers';

interface ErrorBody {
  readonly error: { readonly code: string; readonly param: string | null; readonly message: string };
}

interface AutomationBody {
  readonly id: string;
  readonly object: string;
  readonly type: 'periodic' | 'once';
  readonly created_at: number;
  readonly name: string;
  readonly status: string;
  readonly metadata: Record<string, string>;
  readonly created_by: { readonly type: string };
  readonly pause_reason: string | null;
  readonly target: Record<string, unknown>;
  readonly expression?: string;
  readonly timezone?: string;
  readonly run_at?: number;
}

interface ListBody {
  readonly object: string;
  readonly data: readonly AutomationBody[];
  readonly first_id: string | null;
  readonly last_id: string | null;
  readonly has_more: boolean;
}

interface InvocationBody {
  readonly id: string;
  readonly object: string;
  readonly status: string;
  readonly scheduled_at: number | null;
  readonly source: { readonly type: string };
}

interface InvocationList {
  readonly data: readonly InvocationBody[];
  readonly first_id: string | null;
  readonly has_more: boolean;
}

let harness: Harness | null = null;

afterEach(() => {
  harness?.dispose();
  harness = null;
});

function start(options: Parameters<typeof setup>[0] = {}): Harness {
  harness = setup(options);
  return harness;
}

describe('automation resources', () => {
  test('creates, reads and lists a periodic automation in the documented shape', async () => {
    const h = start();
    const session = createSessionRow(h.repos);
    const created = await post<AutomationBody>(
      h,
      '/agents/automations',
      automationBody(session.id, { metadata: { team: 'platform' } }),
    );

    expect(created.status).toBe(201);
    expect(created.body.object).toBe('agent.automation');
    expect(created.body.type).toBe('periodic');
    expect(created.body.name).toBe('Daily review');
    expect(created.body.status).toBe('active');
    expect(created.body.expression).toBe('0 9 * * *');
    expect(created.body.timezone).toBe('UTC');
    expect(created.body.metadata).toEqual({ team: 'platform' });
    expect(created.body.created_by).toEqual({ type: 'developer' });
    expect(created.body.pause_reason).toBeNull();
    expect(created.body.created_at).toBeNumber();
    expect(created.body.run_at).toBeUndefined();
    expect(created.body.target).toEqual({
      type: 'existing_session',
      session_id: session.id,
      instructions: 'Run the review',
    });

    const fetched = await get<AutomationBody>(h, `/agents/automations/${created.body.id}`);
    expect(fetched.status).toBe(200);
    expect(fetched.body).toEqual(created.body);

    const list = await get<ListBody>(h, '/agents/automations');
    expect(list.status).toBe(200);
    expect(list.body.object).toBe('list');
    expect(list.body.data).toEqual([created.body]);
    expect(list.body.first_id).toBe(created.body.id);
    expect(list.body.last_id).toBe(created.body.id);
    expect(list.body.has_more).toBe(false);
  });

  test('creates a one-time automation and rejects periodic timing fields', async () => {
    const h = start();
    const session = createSessionRow(h.repos);
    const runAt = Math.floor(h.clock.now / 1000) + 600;
    const created = await post<AutomationBody>(h, '/agents/automations', {
      type: 'once',
      name: 'One shot',
      target: { type: 'existing_session', session_id: session.id, instructions: 'go' },
      run_at: runAt,
    });

    expect(created.status).toBe(201);
    expect(created.body.type).toBe('once');
    expect(created.body.run_at).toBe(runAt);
    expect(created.body.status).toBe('active');
    expect(created.body.expression).toBeUndefined();

    const rejected = await post<ErrorBody>(h, `/agents/automations/${created.body.id}`, {
      expression: '0 9 * * *',
    });
    expect(rejected.status).toBe(400);
    expect(rejected.body.error.param).toBe('expression');
  });

  test('normalizes an unserved new-session environment type to the local sandbox', async () => {
    const h = start();
    const runAt = Math.floor(h.clock.now / 1000) + 60;
    const created = await post<AutomationBody>(h, '/agents/automations', {
      type: 'once',
      name: 'Fresh session',
      target: {
        type: 'new_session',
        agent_id: SESSION_INLINE_AGENT_ID,
        environment: { type: 'unserved_hosting', workspace_directory: '/srv/work' },
        input: [{ role: 'user', content: [{ type: 'input_text', text: 'hello' }] }],
      },
      run_at: runAt,
    });

    expect(created.status).toBe(201);
    expect(created.headers.get('x-platform-degraded')).toContain('environment_type_mapped_to_local');
    expect(created.body.target).toMatchObject({
      type: 'new_session',
      environment: {
        type: 'self_hosted',
        environment_id: null,
        workspace_directory: '/srv/work',
        capability_directories: [],
      },
    });
    expect(JSON.stringify(created.body)).not.toContain('unserved_hosting');

    const fetched = await get<AutomationBody>(h, `/agents/automations/${created.body.id}`);
    expect(fetched.headers.get('x-platform-degraded')).toContain('environment_type_mapped_to_local');
    expect(JSON.stringify(fetched.body)).not.toContain('unserved_hosting');

    h.clock.now = runAt * 1000;
    await h.runtime.runtime.service.tick();
    expect(h.port.created[0]?.environment).toEqual({
      type: 'local',
      workspace_directory: '/srv/work',
    });
  });

  test('keeps a documented environment type without reporting degradation', async () => {
    const h = start();
    const created = await post<AutomationBody>(h, '/agents/automations', {
      type: 'once',
      name: 'Plain session',
      target: {
        type: 'new_session',
        agent_id: SESSION_INLINE_AGENT_ID,
        environment: { type: 'none' },
        input: [{ role: 'user', content: [{ type: 'input_text', text: 'hello' }] }],
      },
      run_at: Math.floor(h.clock.now / 1000) + 60,
    });

    expect(created.status).toBe(201);
    expect(created.headers.get('x-platform-degraded')).toBeNull();
    expect(created.body.target).toMatchObject({ environment: { type: 'none' } });
  });

  test('updates name, metadata and target without changing the schedule', async () => {
    const h = start();
    const session = createSessionRow(h.repos);
    const other = createSessionRow(h.repos);
    const created = await post<AutomationBody>(h, '/agents/automations', automationBody(session.id));

    const updated = await post<AutomationBody>(h, `/agents/automations/${created.body.id}`, {
      name: 'Renamed',
      metadata: { team: 'core' },
      target: { type: 'existing_session', session_id: other.id, instructions: 'new text' },
    });
    expect(updated.status).toBe(200);
    expect(updated.body.name).toBe('Renamed');
    expect(updated.body.metadata).toEqual({ team: 'core' });
    expect(updated.body.expression).toBe('0 9 * * *');
    expect(updated.body.target).toEqual({
      type: 'existing_session',
      session_id: other.id,
      instructions: 'new text',
    });

    const unknown = await post<ErrorBody>(h, `/agents/automations/${created.body.id}`, {
      nope: true,
    });
    expect(unknown.status).toBe(400);
  });

  test('rejects timing fields that do not match the automation type', async () => {
    const h = start();
    const session = createSessionRow(h.repos);
    const created = await post<AutomationBody>(h, '/agents/automations', automationBody(session.id));

    const runAtRejected = await post<ErrorBody>(h, `/agents/automations/${created.body.id}`, {
      run_at: Math.floor(h.clock.now / 1000) + 600,
    });
    expect(runAtRejected.status).toBe(400);
    expect(runAtRejected.body.error.param).toBe('run_at');
  });

  test('pauses and resumes a periodic automation', async () => {
    const h = start();
    const session = createSessionRow(h.repos);
    const created = await post<AutomationBody>(h, '/agents/automations', automationBody(session.id));

    const paused = await post<AutomationBody>(h, `/agents/automations/${created.body.id}/pause`);
    expect(paused.status).toBe(200);
    expect(paused.body.status).toBe('paused');
    expect(paused.body.pause_reason).toBeNull();

    const resumed = await post<AutomationBody>(h, `/agents/automations/${created.body.id}/resume`);
    expect(resumed.status).toBe(200);
    expect(resumed.body.status).toBe('active');
  });

  test('resuming a completed one-time automation without a future run_at conflicts', async () => {
    const h = start();
    const session = createSessionRow(h.repos);
    const runAt = Math.floor(h.clock.now / 1000) + 60;
    const created = await post<AutomationBody>(h, '/agents/automations', {
      type: 'once',
      name: 'One shot',
      target: { type: 'existing_session', session_id: session.id, instructions: 'go' },
      run_at: runAt,
    });

    h.clock.now = runAt * 1000;
    await h.runtime.runtime.service.tick();

    const completed = await get<AutomationBody>(h, `/agents/automations/${created.body.id}`);
    expect(completed.body.status).toBe('completed');

    const resumed = await post<ErrorBody>(h, `/agents/automations/${created.body.id}/resume`);
    expect(resumed.status).toBe(409);
    expect(resumed.body.error.code).toBe('run_at_required');
  });

  test('deletes an automation and its resource disappears', async () => {
    const h = start();
    const session = createSessionRow(h.repos);
    const created = await post<AutomationBody>(h, '/agents/automations', automationBody(session.id));

    const deleted = await del<{ id: string; object: string; deleted: boolean }>(
      h,
      `/agents/automations/${created.body.id}`,
    );
    expect(deleted.status).toBe(200);
    expect(deleted.body).toEqual({
      id: created.body.id,
      object: 'agent.automation.deleted',
      deleted: true,
    });

    const missing = await get<ErrorBody>(h, `/agents/automations/${created.body.id}`);
    expect(missing.status).toBe(404);
    expect(missing.body.error.code).toBe('not_found');
  });

  test('paginates automations with after cursors in descending order', async () => {
    const h = start();
    const session = createSessionRow(h.repos);
    const ids: string[] = [];
    for (const name of ['one', 'two', 'three']) {
      const created = await post<AutomationBody>(
        h,
        '/agents/automations',
        automationBody(session.id, { name }),
      );
      ids.push(created.body.id);
    }

    const first = await get<ListBody>(h, '/agents/automations?limit=2');
    expect(first.body.data.map((item) => item.name)).toEqual(['three', 'two']);
    expect(first.body.has_more).toBe(true);
    expect(first.body.first_id).toBe(ids[2] as string);
    expect(first.body.last_id).toBe(ids[1] as string);

    const second = await get<ListBody>(h, `/agents/automations?limit=2&after=${first.body.last_id ?? ''}`);
    expect(second.body.data.map((item) => item.name)).toEqual(['one']);
    expect(second.body.has_more).toBe(false);
  });

  test('filters automations by existing-session target', async () => {
    const h = start();
    const first = createSessionRow(h.repos);
    const second = createSessionRow(h.repos);
    await post<AutomationBody>(h, '/agents/automations', automationBody(first.id));
    await post<AutomationBody>(h, '/agents/automations', automationBody(second.id));

    const filtered = await get<ListBody>(h, `/agents/automations?session_id=${first.id}`);
    expect(filtered.body.data).toHaveLength(1);
    expect(filtered.body.data[0]?.target).toMatchObject({ session_id: first.id });
  });

  test('returns 404 for missing automation operations', async () => {
    const h = start();
    const getMissing = await get<ErrorBody>(h, '/agents/automations/missing');
    expect(getMissing.status).toBe(404);
    expect(getMissing.body.error.code).toBe('not_found');

    const updateMissing = await post<ErrorBody>(h, '/agents/automations/missing', { name: 'x' });
    expect(updateMissing.status).toBe(404);

    const pauseMissing = await post<ErrorBody>(h, '/agents/automations/missing/pause');
    expect(pauseMissing.status).toBe(404);

    const resumeMissing = await post<ErrorBody>(h, '/agents/automations/missing/resume');
    expect(resumeMissing.status).toBe(404);

    const deleteMissing = await del<ErrorBody>(h, '/agents/automations/missing');
    expect(deleteMissing.status).toBe(404);

    const invocationMissing = await get<ErrorBody>(h, '/agents/automations/missing/invocations/invocation');
    expect(invocationMissing.status).toBe(404);
  });

  test('validates cron expressions, time zones, targets and unknown fields', async () => {
    const h = start();
    const session = createSessionRow(h.repos);

    const badCron = await post<ErrorBody>(
      h,
      '/agents/automations',
      automationBody(session.id, { expression: 'not a cron' }),
    );
    expect(badCron.status).toBe(400);
    expect(badCron.body.error.param).toBe('expression');

    const badZone = await post<ErrorBody>(
      h,
      '/agents/automations',
      automationBody(session.id, { timezone: 'Not/AZone' }),
    );
    expect(badZone.status).toBe(400);
    expect(badZone.body.error.code).toBe('timezone_unsupported');
    expect(badZone.body.error.param).toBe('timezone');

    const missingSession = await post<ErrorBody>(h, '/agents/automations', automationBody('missing-session'));
    expect(missingSession.status).toBe(404);

    const unknown = await post<ErrorBody>(h, '/agents/automations', automationBody(session.id, { nope: 1 }));
    expect(unknown.status).toBe(400);

    const unknownTarget = await post<ErrorBody>(
      h,
      '/agents/automations',
      automationBody(session.id, {
        target: {
          type: 'existing_session',
          session_id: session.id,
          instructions: 'x',
          extra: true,
        },
      }),
    );
    expect(unknownTarget.status).toBe(400);

    const past = await post<ErrorBody>(h, '/agents/automations', {
      type: 'once',
      name: 'Past',
      target: { type: 'existing_session', session_id: session.id, instructions: 'go' },
      run_at: Math.floor(h.clock.now / 1000) - 1,
    });
    expect(past.status).toBe(400);
    expect(past.body.error.param).toBe('run_at');
  });

  test('returns 404 when an invocation does not belong to the automation', async () => {
    const h = start();
    const session = createSessionRow(h.repos);
    const created = await post<AutomationBody>(h, '/agents/automations', automationBody(session.id));
    const other = await post<AutomationBody>(h, '/agents/automations', automationBody(session.id));
    const invocation = await post<{ id: string }>(h, `/agents/automations/${created.body.id}/invocations`);

    const mismatched = await get<ErrorBody>(
      h,
      `/agents/automations/${other.body.id}/invocations/${invocation.body.id}`,
    );
    expect(mismatched.status).toBe(404);
  });

  test('lists and fetches invocations with pagination', async () => {
    const h = start();
    const session = createSessionRow(h.repos);
    const created = await post<AutomationBody>(h, '/agents/automations', automationBody(session.id));
    const first = await post<InvocationBody>(h, `/agents/automations/${created.body.id}/invocations`);
    const second = await post<InvocationBody>(h, `/agents/automations/${created.body.id}/invocations`);

    const page = await get<InvocationList>(
      h,
      `/agents/automations/${created.body.id}/invocations?limit=1&order=asc`,
    );
    expect(page.body.data).toHaveLength(1);
    expect(page.body.first_id).toBe(first.body.id);
    expect(page.body.has_more).toBe(true);

    const next = await get<InvocationList>(
      h,
      `/agents/automations/${created.body.id}/invocations?limit=1&order=asc&after=${first.body.id}`,
    );
    expect(next.body.data.map((item) => item.id)).toEqual([second.body.id]);

    await h.runtime.runtime.service.drain();
    const fetched = await get<InvocationBody>(
      h,
      `/agents/automations/${created.body.id}/invocations/${second.body.id}`,
    );
    expect(fetched.status).toBe(200);
    expect(fetched.body.id).toBe(second.body.id);
    expect(second.body.status).toBe('queued');
    expect(fetched.body.status).not.toBe('queued');
  });
});
