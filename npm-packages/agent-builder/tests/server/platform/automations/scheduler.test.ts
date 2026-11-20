import { afterEach, describe, expect, test } from 'bun:test';
import { SESSION_INLINE_AGENT_ID } from '../../../../server/db/migrate';
import { automationBody, createSessionRow, deferred, get, type Harness, post, START, setup } from './helpers';

interface AutomationBody {
  readonly id: string;
  readonly status: string;
  readonly pause_reason: string | null;
}

interface InvocationBody {
  readonly id: string;
  readonly status: string;
  readonly scheduled_at: number | null;
  readonly session_id: string | null;
  readonly skip_reason: string | null;
  readonly source: { readonly type: string };
}

interface InvocationList {
  readonly data: readonly InvocationBody[];
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

async function invocations(h: Harness, automationId: string): Promise<InvocationList> {
  const response = await get<InvocationList>(h, `/agents/automations/${automationId}/invocations?order=asc`);
  return response.body;
}

describe('automation scheduler ticks', () => {
  test('runs a due periodic occurrence once and advances the schedule', async () => {
    const h = start();
    const session = createSessionRow(h.repos);
    const created = await post<AutomationBody>(
      h,
      '/agents/automations',
      automationBody(session.id, { expression: '* * * * *' }),
    );

    h.clock.now = Date.UTC(2026, 0, 1, 12, 1, 0);
    await h.runtime.runtime.service.tick();

    expect(h.port.sends).toEqual([{ sessionId: session.id, text: 'Run the review' }]);
    const first = await invocations(h, created.body.id);
    expect(first.data).toHaveLength(1);
    expect(first.data[0]?.status).toBe('completed');
    expect(first.data[0]?.scheduled_at).toBe(Date.UTC(2026, 0, 1, 12, 1, 0) / 1000);
    expect(first.data[0]?.source.type).toBe('automation');

    await h.runtime.runtime.service.tick();
    expect(h.port.sends).toHaveLength(1);
    expect((await invocations(h, created.body.id)).data).toHaveLength(1);

    h.clock.now = Date.UTC(2026, 0, 1, 12, 2, 0);
    await h.runtime.runtime.service.tick();
    expect(h.port.sends).toHaveLength(2);
  });

  test('does not fire while paused', async () => {
    const h = start();
    const session = createSessionRow(h.repos);
    const created = await post<AutomationBody>(
      h,
      '/agents/automations',
      automationBody(session.id, { expression: '* * * * *' }),
    );
    await post(h, `/agents/automations/${created.body.id}/pause`);

    h.clock.now = Date.UTC(2026, 0, 1, 13, 0, 0);
    await h.runtime.runtime.service.tick();

    expect(h.port.sends).toHaveLength(0);
    expect((await invocations(h, created.body.id)).data).toHaveLength(0);
  });

  test('completes a one-time automation after its trigger and never fires again', async () => {
    const h = start();
    const session = createSessionRow(h.repos);
    const runAt = Math.floor(START / 1000) + 60;
    const created = await post<AutomationBody>(h, '/agents/automations', {
      type: 'once',
      name: 'One shot',
      target: { type: 'existing_session', session_id: session.id, instructions: 'go' },
      run_at: runAt,
    });

    h.clock.now = (runAt + 30) * 1000;
    await h.runtime.runtime.service.tick();

    const after = await get<AutomationBody>(h, `/agents/automations/${created.body.id}`);
    expect(after.body.status).toBe('completed');
    expect(h.port.sends).toHaveLength(1);
    const first = await invocations(h, created.body.id);
    expect(first.data).toHaveLength(1);
    expect(first.data[0]?.session_id).toBe(session.id);

    await h.runtime.runtime.service.tick();
    expect(h.port.sends).toHaveLength(1);
    expect((await invocations(h, created.body.id)).data).toHaveLength(1);
  });

  test('creates a session for a new_session target', async () => {
    const h = start();
    const runAt = Math.floor(START / 1000) + 60;
    const created = await post<AutomationBody>(h, '/agents/automations', {
      type: 'once',
      name: 'Fresh session',
      target: {
        type: 'new_session',
        agent_id: SESSION_INLINE_AGENT_ID,
        environment: { type: 'none' },
        input: [{ role: 'user', content: [{ type: 'input_text', text: 'hello' }] }],
      },
      run_at: runAt,
    });

    h.clock.now = runAt * 1000;
    await h.runtime.runtime.service.tick();

    expect(h.port.created).toHaveLength(1);
    expect(h.port.created[0]?.agent_id).toBe(SESSION_INLINE_AGENT_ID);
    expect(h.port.sends).toHaveLength(0);
    const list = await invocations(h, created.body.id);
    expect(list.data[0]?.status).toBe('completed');
    expect(list.data[0]?.session_id).toBe('session-1');
  });

  test('pauses the automation when the target session already failed', async () => {
    const h = start();
    const session = createSessionRow(h.repos, 'failed');
    const created = await post<AutomationBody>(
      h,
      '/agents/automations',
      automationBody(session.id, { expression: '* * * * *' }),
    );

    h.clock.now = Date.UTC(2026, 0, 1, 12, 1, 0);
    await h.runtime.runtime.service.tick();

    const after = await get<AutomationBody>(h, `/agents/automations/${created.body.id}`);
    expect(after.body.status).toBe('paused');
    expect(after.body.pause_reason).toBe('target_session_failed');
    expect(h.port.sends).toHaveLength(0);

    const list = await invocations(h, created.body.id);
    expect(list.data).toHaveLength(1);
    expect(list.data[0]?.status).toBe('skipped');
    expect(list.data[0]?.skip_reason).toBe('pending_delivery');
    const stored = h.repos.automationInvocations.listByAutomation(created.body.id);
    expect(stored[0]?.skip_reason).toBe('target_session_failed');
  });

  test('queues a manual invocation and delivers it after the response', async () => {
    const h = start();
    const session = createSessionRow(h.repos);
    const created = await post<AutomationBody>(
      h,
      '/agents/automations',
      automationBody(session.id, { expression: '* * * * *' }),
    );
    const before = h.repos.automations.get(created.body.id);
    const gate = deferred();
    h.port.sendGate = gate;

    const manual = await post<InvocationBody>(h, `/agents/automations/${created.body.id}/invocations`);
    expect(manual.status).toBe(200);
    expect(manual.body.source).toEqual({ type: 'manual' });
    expect(manual.body.scheduled_at).toBeNull();
    expect(manual.body.status).toBe('queued');
    expect(manual.body.session_id).toBeNull();

    await Bun.sleep(0);
    expect(h.port.sends).toHaveLength(1);
    expect(h.repos.automationInvocations.get(manual.body.id)?.status).toBe('queued');

    gate.release();
    await h.runtime.runtime.service.drain();

    const settled = await get<InvocationBody>(
      h,
      `/agents/automations/${created.body.id}/invocations/${manual.body.id}`,
    );
    expect(settled.body.status).toBe('completed');
    expect(settled.body.session_id).toBe(session.id);
    expect(h.port.sends).toEqual([{ sessionId: session.id, text: 'Run the review' }]);
    expect(manual.body.status).toBe('queued');

    const after = h.repos.automations.get(created.body.id);
    expect(after?.next_run_at).toBe(before?.next_run_at);
    expect(after?.status).toBe('active');
  });

  test('records a failed manual invocation when delivery throws', async () => {
    const h = start();
    const session = createSessionRow(h.repos);
    const created = await post<AutomationBody>(h, '/agents/automations', automationBody(session.id));
    h.port.sendError = new Error('delivery exploded');

    const manual = await post<InvocationBody>(h, `/agents/automations/${created.body.id}/invocations`);
    expect(manual.status).toBe(200);
    expect(manual.body.status).toBe('queued');

    await h.runtime.runtime.service.drain();

    const stored = h.repos.automationInvocations.get(manual.body.id);
    expect(stored?.status).toBe('failed');
    expect(stored?.error_code).toBe('delivery_failed');
    expect(h.repos.automations.get(created.body.id)?.status).toBe('active');
  });

  test('skips a manual invocation that overlaps an in-flight delivery', async () => {
    const h = start();
    const session = createSessionRow(h.repos);
    const created = await post<AutomationBody>(h, '/agents/automations', automationBody(session.id));
    const gate = deferred();
    h.port.sendGate = gate;

    const first = await post<InvocationBody>(h, `/agents/automations/${created.body.id}/invocations`);
    const second = await post<InvocationBody>(h, `/agents/automations/${created.body.id}/invocations`);
    expect(first.body.status).toBe('queued');
    expect(second.body.status).toBe('queued');

    await Bun.sleep(0);
    gate.release();
    await h.runtime.runtime.service.drain();

    const stored = h.repos.automationInvocations.listByAutomation(created.body.id);
    expect(stored).toHaveLength(2);
    expect(stored[0]?.status).toBe('completed');
    expect(stored[1]?.status).toBe('skipped');
    expect(stored[1]?.skip_reason).toBe('overlap');
    expect(h.port.sends).toHaveLength(1);
  });

  test('keeps a one-time automation completed when its target session already failed', async () => {
    const h = start();
    const session = createSessionRow(h.repos, 'failed');
    const runAt = Math.floor(START / 1000) + 60;
    const created = await post<AutomationBody>(h, '/agents/automations', {
      type: 'once',
      name: 'Doomed target',
      target: { type: 'existing_session', session_id: session.id, instructions: 'go' },
      run_at: runAt,
    });

    h.clock.now = runAt * 1000;
    await h.runtime.runtime.service.tick();

    const after = await get<AutomationBody>(h, `/agents/automations/${created.body.id}`);
    expect(after.body.status).toBe('completed');
    expect(after.body.pause_reason).toBeNull();
    expect(h.port.sends).toHaveLength(0);

    const stored = h.repos.automationInvocations.listByAutomation(created.body.id);
    expect(stored).toHaveLength(1);
    expect(stored[0]?.status).toBe('failed');
    expect(stored[0]?.error_code).toBe('target_session_failed');
  });

  test('skips an overlapping occurrence while the previous delivery is in flight', async () => {
    const h = start();
    const session = createSessionRow(h.repos);
    const created = await post<AutomationBody>(
      h,
      '/agents/automations',
      automationBody(session.id, { expression: '* * * * *' }),
    );

    const gate = deferred();
    h.port.sendGate = gate;
    h.clock.now = Date.UTC(2026, 0, 1, 12, 1, 0);
    const first = h.runtime.runtime.service.tick();

    h.clock.now = Date.UTC(2026, 0, 1, 12, 2, 0);
    await h.runtime.runtime.service.tick();
    gate.release();
    await first;

    const stored = h.repos.automationInvocations.listByAutomation(created.body.id);
    expect(stored).toHaveLength(2);
    expect(stored[0]?.status).toBe('completed');
    expect(stored[1]?.status).toBe('skipped');
    expect(stored[1]?.skip_reason).toBe('overlap');
    expect(h.port.sends).toHaveLength(1);
  });

  test('advances a missed periodic occurrence instead of backfilling it', async () => {
    const h = start();
    const session = createSessionRow(h.repos);
    const created = await post<AutomationBody>(
      h,
      '/agents/automations',
      automationBody(session.id, { expression: '* * * * *' }),
    );

    h.clock.now = START + 60 * 60_000;
    await h.runtime.runtime.service.tick();

    expect(h.port.sends).toHaveLength(0);
    expect((await invocations(h, created.body.id)).data).toHaveLength(0);
    const after = h.repos.automations.get(created.body.id);
    expect(after?.status).toBe('active');
    expect(after?.next_run_at).toBe(Date.UTC(2026, 0, 1, 13, 1, 0));
  });
});
