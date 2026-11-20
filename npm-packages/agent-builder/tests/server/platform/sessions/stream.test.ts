import { afterEach, describe, expect, test } from 'bun:test';
import {
  createSession,
  eventTypes,
  get,
  type Harness,
  json,
  post,
  readFrames,
  sessionEvents,
  setup,
  tick,
} from './helpers';

let harness: Harness | null = null;

afterEach(async () => {
  await harness?.dispose();
  harness = null;
});

describe('session event stream', () => {
  test('replays the durable log as SSE frames whose event name equals the payload type', async () => {
    const h = setup();
    harness = h;
    const { id } = await createSession(h, {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'hello',
    });
    const actor = h.actor(id);
    actor.runState(true);
    actor.input('in-1', 'hello');
    await tick();
    actor.chunk('Hi');
    await tick();
    actor.turnComplete('completed');
    await tick();

    const response = await get(h, `/agents/sessions/${id}/events`);
    expect(response.status).toBe(200);
    expect(response.headers.get('content-type')).toContain('text/event-stream');

    const frames = await readFrames(response, 6);
    const types = frames.map((frame) => frame.event);
    expect(types[0]).toBe('agent.session.created');
    expect(types).toContain('agent.session.turn.created');
    for (const frame of frames) {
      expect(frame.data.type).toBe(frame.event);
      expect(frame.id).not.toBeNull();
    }
  });

  test('resumes after Last-Event-ID without repeating earlier events', async () => {
    const h = setup();
    harness = h;
    const { id } = await createSession(h, {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'hello',
    });
    const actor = h.actor(id);
    actor.runState(true);
    actor.input('in-1', 'hello');
    await tick();
    actor.turnComplete('completed');
    await tick();

    const events = sessionEvents(h, id);
    expect(events.length).toBeGreaterThan(3);
    const resumeAt = events[2];
    if (resumeAt === undefined) throw new Error('missing event');

    const response = await get(h, `/agents/sessions/${id}/events`, {
      'Last-Event-ID': String(resumeAt.seq),
    });
    const frames = await readFrames(response, 2);
    expect(frames.map((frame) => frame.id)).toEqual([String(events[3]?.seq), String(events[4]?.seq)]);
    expect(new Set(frames.map((frame) => frame.id)).size).toBe(frames.length);
  });

  test('accepts a public event id as Last-Event-ID', async () => {
    const h = setup();
    harness = h;
    const { id } = await createSession(h, {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'hello',
    });
    const actor = h.actor(id);
    actor.runState(true);
    actor.input('in-1', 'hello');
    await tick();
    actor.turnComplete('completed');
    await tick();
    const events = sessionEvents(h, id);
    const anchor = events[1];
    if (anchor === undefined) throw new Error('missing event');

    // The id a client reads from the frame payload is the same id stored on the row, so
    // echoing it back resolves to the row's log position instead of replaying from zero.
    for (const event of events) {
      expect(event.data.event_id).toBe(event.event_id);
    }

    const response = await get(h, `/agents/sessions/${id}/events`, {
      'Last-Event-ID': anchor.event_id,
    });
    const frames = await readFrames(response, 1);
    expect(frames[0]?.id).toBe(String(events[2]?.seq));
  });

  test('streams live events after the replay and cleans up the subscription on cancel', async () => {
    const h = setup();
    harness = h;
    const { id } = await createSession(h, {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'hello',
    });
    const actor = h.actor(id);
    const before = sessionEvents(h, id).length;

    const response = await get(h, `/agents/sessions/${id}/events`);
    const replayed = await readFrames(response, before);
    expect(replayed).toHaveLength(before);

    // A second stream keeps the replay it has already seen; new facts arrive live.
    const live = await get(h, `/agents/sessions/${id}/events`, {
      'Last-Event-ID': String(sessionEvents(h, id).at(-1)?.seq ?? 0),
    });
    const framesPromise = readFrames(live, 1);
    await tick();
    actor.runState(true);
    await tick();
    const frames = await framesPromise;
    expect(frames[0]?.event).toBe('agent.session.in_progress');
  });

  test('creates a session as an event stream when stream=true', async () => {
    const h = setup();
    harness = h;
    const response = await post(h, '/agents/sessions', {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'hello',
      stream: true,
    });
    expect(response.status).toBe(201);
    expect(response.headers.get('content-type')).toContain('text/event-stream');
    const frames = await readFrames(response, 1);
    expect(frames[0]?.event).toBe('agent.session.created');
    const session = frames[0]?.data.session as { id?: string };
    expect(session.id).toBeString();
  });
});

describe('session inputs', () => {
  test('submits a message input and replays an identical Idempotency-Key once', async () => {
    const h = setup();
    harness = h;
    const { id } = await createSession(h, {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'first',
    });
    const actor = h.actor(id);
    const body = {
      events: [
        {
          type: 'agent.session.input.message',
          input: [{ role: 'user', content: [{ type: 'input_text', text: 'second' }] }],
        },
      ],
    };

    const first = await post(h, `/agents/sessions/${id}/events`, body, { 'Idempotency-Key': 'key-1' });
    expect(first.status).toBe(202);
    const sendsAfterFirst = actor.actions.filter((action) => action.type === 'send').length;

    const replay = await post(h, `/agents/sessions/${id}/events`, body, { 'Idempotency-Key': 'key-1' });
    expect(replay.status).toBe(202);
    expect(actor.actions.filter((action) => action.type === 'send').length).toBe(sendsAfterFirst);

    const conflict = await post(
      h,
      `/agents/sessions/${id}/events`,
      {
        events: [
          {
            type: 'agent.session.input.message',
            input: [{ role: 'user', content: [{ type: 'input_text', text: 'different' }] }],
          },
        ],
      },
      { 'Idempotency-Key': 'key-1' },
    );
    expect(conflict.status).toBe(409);
    expect((await json<{ error: { code: string } }>(conflict)).error.code).toBe('idempotency_conflict');
  });

  test('cancels a running turn', async () => {
    const h = setup();
    harness = h;
    const { id } = await createSession(h, {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'first',
    });
    const actor = h.actor(id);
    const response = await post(h, `/agents/sessions/${id}/events`, {
      events: [{ type: 'agent.session.input.cancel' }],
    });
    expect(response.status).toBe(202);
    expect(actor.actions.some((action) => action.type === 'cancel')).toBe(true);
  });

  test('answers a pending interaction through the session projection', async () => {
    const h = setup();
    harness = h;
    const { id } = await createSession(h, {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'first',
    });
    const actor = h.actor(id);
    actor.runState(true);
    actor.input('in-1', 'first');
    await tick();
    actor.interaction('per-1', 'permission', 'turn:in-1');
    await tick();

    const resource = await json<{ status: string; required_actions: { type: string }[] }>(
      await get(h, `/agents/sessions/${id}`),
    );
    expect(resource.status).toBe('requires_action');
    expect(resource.required_actions[0]?.type).toBe('function_call');
    expect(eventTypes(h, id)).toContain('agent.session.requires_action');

    const response = await post(h, `/agents/sessions/${id}/events`, {
      events: [
        {
          type: 'agent.session.input.mcp_approval_request_result',
          request_id: 'per-1',
          response: { type: 'mcp_tool_call', decision: 'approve' },
        },
      ],
    });
    expect(response.status).toBe(202);
    expect(actor.answers).toEqual([{ kind: 'permission', decision: 'allow_once' }]);
  });

  test('rejects unsupported input types and unknown interactions', async () => {
    const h = setup();
    harness = h;
    const { id } = await createSession(h, {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'first',
    });

    const unsupported = await post(h, `/agents/sessions/${id}/events`, {
      events: [{ type: 'agent.session.input.tool_result', turn_id: 't', call_id: 'c', success: true }],
    });
    expect(unsupported.status).toBe(400);

    const unknown = await post(h, `/agents/sessions/${id}/events`, {
      events: [
        {
          type: 'agent.session.input.mcp_approval_request_result',
          request_id: 'missing',
          response: { type: 'mcp_tool_call', decision: 'deny' },
        },
      ],
    });
    expect(unknown.status).toBe(404);
  });

  test('rejects a missing session and a bad idempotency key', async () => {
    const h = setup();
    harness = h;
    const missing = await post(h, '/agents/sessions/does-not-exist/events', {
      events: [{ type: 'agent.session.input.cancel' }],
    });
    expect(missing.status).toBe(404);

    const { id } = await createSession(h, {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'first',
    });
    const badKey = await post(
      h,
      `/agents/sessions/${id}/events`,
      { events: [{ type: 'agent.session.input.cancel' }] },
      { 'Idempotency-Key': 'x'.repeat(257) },
    );
    expect(badKey.status).toBe(400);
  });
});
