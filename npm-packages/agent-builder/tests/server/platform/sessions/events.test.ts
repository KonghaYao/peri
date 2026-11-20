import type { Database } from 'bun:sqlite';
import { afterEach, describe, expect, test } from 'bun:test';
import { createSession, eventTypes, get, type Harness, json, sessionEvents, setup, tick } from './helpers';

let harness: Harness | null = null;
let extra: Database | null = null;

afterEach(async () => {
  await harness?.dispose();
  harness = null;
  extra?.close();
  extra = null;
});

function isSubsequence(actual: readonly string[], expected: readonly string[]): boolean {
  let index = 0;
  for (const type of actual) {
    if (type === expected[index]) index += 1;
    if (index === expected.length) return true;
  }
  return index === expected.length;
}

function start(): Harness {
  harness = setup();
  return harness;
}

describe('session events', () => {
  test('records the documented lifecycle sequence for one text turn', async () => {
    const h = start();
    const { id } = await createSession(h, {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'hello',
    });
    const actor = h.actor(id);

    actor.runState(true);
    actor.input('in-1', 'hello');
    await tick();
    actor.chunk('Hel');
    await tick();
    actor.chunk('lo');
    await tick();
    actor.turnComplete('completed');
    await tick();
    actor.runState(false);
    await tick();

    const types = eventTypes(h, id);
    expect(
      isSubsequence(types, [
        'agent.session.created',
        'agent.session.in_progress',
        'agent.session.turn.created',
        'agent.session.turn.in_progress',
        'agent.session.turn.item.added',
        'agent.session.turn.content_part.added',
        'agent.session.turn.output_text.delta',
        'agent.session.turn.output_text.done',
        'agent.session.turn.item.done',
        'agent.session.turn.completed',
        'agent.session.idle',
      ]),
    ).toBe(true);

    const events = sessionEvents(h, id);
    const deltas = events
      .filter((event) => event.type === 'agent.session.turn.output_text.delta')
      .map((event) => event.data.delta);
    expect(deltas).toEqual(['Hel', 'lo']);

    const assistantDone = events.find(
      (event) =>
        event.type === 'agent.session.turn.item.done' &&
        (event.data.item as { role?: string }).role === 'assistant',
    );
    expect((assistantDone?.data.item as { content?: unknown[] } | undefined)?.content).toEqual([
      { type: 'output_text', text: 'Hello' },
    ]);

    // Event ids are unique and the log position is monotonic.
    const ids = events.map((event) => event.event_id);
    expect(new Set(ids).size).toBe(ids.length);
    expect(events.map((event) => event.seq)).toEqual(
      [...events.map((event) => event.seq)].sort((left, right) => left - right),
    );
    // The SSE event name always equals the payload discriminator.
    for (const event of events) expect(event.data.type).toBe(event.type);
  });

  test('streams a command execution item with output deltas', async () => {
    const h = start();
    const { id } = await createSession(h, {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'run it',
    });
    const actor = h.actor(id);

    actor.runState(true);
    actor.input('in-1', 'run it');
    await tick();
    actor.tool({
      toolCallId: 'call-1',
      title: 'Shell',
      kind: 'execute',
      status: 'in_progress',
      arguments: { command: 'printf hello', cwd: '/workspace' },
    });
    await tick();
    actor.tool({
      toolCallId: 'call-1',
      title: 'Shell',
      kind: 'execute',
      status: 'in_progress',
      arguments: { command: 'printf hello', cwd: '/workspace' },
      result: { output: 'hel' },
    });
    await tick();
    actor.tool({
      toolCallId: 'call-1',
      title: 'Shell',
      kind: 'execute',
      status: 'completed',
      arguments: { command: 'printf hello', cwd: '/workspace' },
      result: { output: 'hello' },
    });
    await tick();
    actor.turnComplete('completed');
    await tick();

    const events = sessionEvents(h, id);
    const commandItem = events.find(
      (event) =>
        event.type === 'agent.session.turn.item.added' &&
        (event.data.item as { type?: string }).type === 'command_execution',
    );
    expect(commandItem).toBeDefined();
    const item = commandItem?.data.item as Record<string, unknown>;
    expect(item.command).toBe('printf hello');
    expect(item.cwd).toBe('/workspace');

    const deltas = events
      .filter((event) => event.type === 'agent.output.command_execution_output.delta')
      .map((event) => event.data.delta);
    expect(deltas).toEqual(['hel', 'lo']);

    const done = events.find(
      (event) =>
        event.type === 'agent.session.turn.item.done' &&
        (event.data.item as { type?: string }).type === 'command_execution',
    );
    expect((done?.data.item as { status?: string } | undefined)?.status).toBe('completed');
  });

  test('records a function call item for a non-execute tool', async () => {
    const h = start();
    const { id } = await createSession(h, {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'search',
    });
    const actor = h.actor(id);

    actor.runState(true);
    actor.input('in-1', 'search');
    await tick();
    actor.tool({ toolCallId: 'call-9', title: 'WebSearch', kind: 'search', status: 'in_progress' });
    await tick();
    actor.tool({ toolCallId: 'call-9', title: 'WebSearch', kind: 'search', status: 'completed' });
    await tick();
    actor.turnComplete('completed');
    await tick();

    const item = sessionEvents(h, id).find(
      (event) =>
        event.type === 'agent.session.turn.item.added' &&
        (event.data.item as { type?: string }).type === 'function_call',
    );
    expect(item?.data.item).toMatchObject({ call_id: 'call-9', name: 'WebSearch' });
  });

  test('emits reasoning summary events for thought chunks', async () => {
    const h = start();
    const { id } = await createSession(h, {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'think',
    });
    const actor = h.actor(id);

    actor.runState(true);
    actor.input('in-1', 'think');
    await tick();
    actor.thought('pondering');
    await tick();
    actor.turnComplete('completed');
    await tick();

    const types = eventTypes(h, id);
    expect(types).toContain('agent.session.turn.reasoning_summary_part.added');
    expect(types).toContain('agent.session.turn.reasoning_summary_text.delta');
    expect(types).toContain('agent.session.turn.reasoning_summary_text.done');
    expect(types).toContain('agent.session.turn.reasoning_summary_part.done');

    const item = sessionEvents(h, id).find(
      (event) =>
        event.type === 'agent.session.turn.item.done' &&
        (event.data.item as { type?: string }).type === 'reasoning',
    );
    expect((item?.data.item as { summary?: unknown[] } | undefined)?.summary).toEqual([
      { type: 'summary_text', text: 'pondering' },
    ]);
  });

  test('closes the content part and opens a new one when a rebuild rewrites the text', async () => {
    const h = start();
    const { id } = await createSession(h, {
      environment: { type: 'none' },
      agent_id: h.agentId,
      input: 'hello',
    });
    const actor = h.actor(id);

    actor.runState(true);
    actor.input('in-1', 'hello');
    await tick();
    actor.chunk('Hello there');
    await tick();

    const before = sessionEvents(h, id).filter((event) => event.type.startsWith('agent.session.turn.'));
    expect(before.some((event) => event.type === 'agent.session.turn.content_part.added')).toBe(true);

    // A new binding generation replays the same turn with shorter text: the recorder must not emit
    // a negative delta, it closes the open part and starts another one.
    const revision = actor.rebind([]);
    actor.envelopes.push(
      {
        chatId: id,
        sessionId: actor.binding.sessionId,
        bindingRevision: revision,
        sequence: 1,
        fact: { kind: 'run-state', running: true, executionBlocked: false },
      },
      {
        chatId: id,
        sessionId: actor.binding.sessionId,
        bindingRevision: revision,
        sequence: 2,
        fact: { kind: 'input-delivered', inputId: 'in-1', text: 'hello' },
      },
      {
        chatId: id,
        sessionId: actor.binding.sessionId,
        bindingRevision: revision,
        sequence: 3,
        fact: {
          kind: 'notice',
          notification: {
            jsonrpc: '2.0',
            method: 'session/update',
            params: {
              sessionId: actor.binding.sessionId,
              update: {
                sessionUpdate: 'agent_message_chunk',
                messageId: 'm-1',
                content: { type: 'text', text: 'Hi' },
              },
            },
          } as never,
        },
      },
    );
    // Any live fact triggers the facts refresh; the rebuilt projection is the authority.
    actor.emit({ kind: 'run-state', running: true, executionBlocked: false });
    actor.envelopes = actor.envelopes.filter((envelope) => envelope.bindingRevision === revision);
    await tick();
    await tick();

    const events = sessionEvents(h, id);
    const added = events.filter((event) => event.type === 'agent.session.turn.content_part.added');
    const closed = events.filter((event) => event.type === 'agent.session.turn.output_text.done');
    expect(closed.length).toBeGreaterThanOrEqual(1);
    expect(added.length).toBeGreaterThanOrEqual(2);
    expect(
      events.some(
        (event) =>
          event.type === 'agent.session.turn.output_text.delta' &&
          typeof event.data.delta === 'string' &&
          event.data.delta.startsWith('-'),
      ),
    ).toBe(false);
  });

  test('records token usage on the turn and the session', async () => {
    const h = start();
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
    actor.usage(7, 5);
    await tick();
    actor.turnComplete('completed');
    await tick();

    const completed = sessionEvents(h, id).find((event) => event.type === 'agent.session.turn.completed');
    expect(completed?.data.usage).toMatchObject({ input_tokens: 7, output_tokens: 5, total_tokens: 12 });

    const session = await json<{ usage: { total_tokens: number } | null }>(
      await get(h, `/agents/sessions/${id}`),
    );
    expect(session.usage?.total_tokens).toBe(12);

    const turns = await json<{ data: { id: string }[] }>(await get(h, `/agents/sessions/${id}/turns`));
    const turn = await json<{ usage: { input_tokens: number } | null }>(
      await get(h, `/agents/sessions/${id}/turns/${turns.data[0]?.id}`),
    );
    expect(turn.usage?.input_tokens).toBe(7);
  });

  test('exposes turns and items through the resource routes', async () => {
    const h = start();
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

    const turns = await json<{ object: string; data: { id: string; object: string; status: string }[] }>(
      await get(h, `/agents/sessions/${id}/turns`),
    );
    expect(turns.object).toBe('list');
    expect(turns.data).toHaveLength(1);
    const turn = turns.data[0];
    expect(turn?.object).toBe('agent.session.turn');
    expect(turn?.status).toBe('completed');

    const single = await json<Record<string, unknown>>(
      await get(h, `/agents/sessions/${id}/turns/${turn?.id}`),
    );
    expect(single.id).toBe(turn?.id);
    expect(single.session_id).toBe(id);

    const items = await json<{ data: { id: string; type: string; turn_id: string }[] }>(
      await get(h, `/agents/sessions/${id}/items`),
    );
    expect(items.data.length).toBeGreaterThanOrEqual(2);
    expect(items.data.every((item) => item.turn_id === turn?.id)).toBe(true);
    expect(items.data.map((item) => item.type)).toContain('message');

    const turnItems = await json<{ data: { id: string }[] }>(
      await get(h, `/agents/sessions/${id}/turns/${turn?.id}/items`),
    );
    expect(turnItems.data.map((item) => item.id)).toEqual(items.data.map((item) => item.id));

    const missing = await get(h, `/agents/sessions/${id}/turns/unknown`);
    expect(missing.status).toBe(404);
  });
});
