import { describe, expect, test } from 'bun:test';
import type { ChatSnapshotEvent } from '@shared/chat';
import { ChatEventLog, ChatStreamHub, type StreamEvent } from '../../../server/chats/stream';
import { agentChunk, BINDING, envelope, FakeActor, userInput } from './helpers';

const tick = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));

function gate() {
  let resolve = () => {};
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function revisionFacts(revision: number) {
  const binding = { ...BINDING, sessionId: `session-${revision}`, bindingRevision: revision };
  return {
    binding,
    envelopes: [{ ...userInput(`i-${revision}`, `confirmed-${revision}`), ...binding }] as [
      ReturnType<typeof userInput>,
    ],
    overflow: false,
  };
}

function messages(events: StreamEvent[]): string[] {
  return events
    .filter((event) => event.event === 'message')
    .map((event) => (event.data as { message: { text: string } }).message.text);
}

describe('ChatEventLog', () => {
  test('replays from sinceSeq and reports a gap once the ring dropped the position', () => {
    const log = new ChatEventLog(3);
    log.append('message', { n: 1 });
    log.append('message', { n: 2 });
    log.append('message', { n: 3 });
    expect(log.since(1)).toEqual({
      events: [
        { seq: 2, event: 'message', data: { n: 2 } },
        { seq: 3, event: 'message', data: { n: 3 } },
      ],
      gap: false,
    });

    log.append('message', { n: 4 });
    expect(log.since(0).gap).toBe(true);
    // Event 1 fell out of the ring, but a client that already saw it can continue at 2.
    expect(log.since(1).gap).toBe(false);
    expect(log.since(2).gap).toBe(false);
    expect(log.since(3)).toEqual({
      events: [{ seq: 4, event: 'message', data: { n: 4 } }],
      gap: false,
    });
    expect(log.since(9)).toEqual({ events: [], gap: true });
  });
});

describe('ChatStreamHub', () => {
  test('sends a snapshot from facts, then one message event per chunk without duplicates', async () => {
    const actor = new FakeActor();
    actor.factsResult = {
      binding: { ...BINDING },
      envelopes: [userInput('input-1', 'hello', 1)],
      overflow: false,
    };
    const hub = new ChatStreamHub('chat-1', actor);

    const events: StreamEvent[] = [];
    await hub.subscribe(undefined, (event) => events.push(event));
    const snapshot = events[0];
    expect(snapshot?.event).toBe('snapshot');
    const snapshotData = snapshot === undefined ? undefined : (snapshot.data as ChatSnapshotEvent);
    expect(snapshotData?.messages.map((message) => message.text)).toEqual(['hello']);

    actor.emit(agentChunk('message-1', 'echo: ', 2));
    await tick();
    actor.emit(agentChunk('message-1', 'hello', 3));
    await tick();

    expect(messages(events)).toEqual(['echo: ', 'echo: hello']);
    const beforeDuplicate = events.length;
    actor.emit(agentChunk('message-1', 'hello', 3));
    await tick();
    expect(events).toHaveLength(beforeDuplicate);

    await hub.dispose();
  });

  test('accepts the SDK activation id when it differs from the app chat id', async () => {
    const actor = new FakeActor();
    const binding = { ...BINDING, chatId: 'sdk-activation-1' };
    actor.factsResult = {
      binding,
      envelopes: [{ ...userInput('input-1', 'hello', 1), chatId: binding.chatId }],
      overflow: false,
    };
    const hub = new ChatStreamHub('app-chat-id', actor);
    const events: StreamEvent[] = [];
    try {
      await hub.subscribe(undefined, (event) => events.push(event));
      expect(events[0]?.data).toMatchObject({
        sessionId: 'session-1',
        messages: [expect.objectContaining({ text: 'hello' })],
      });

      actor.emit({ ...agentChunk('message-1', 'reply', 2), chatId: binding.chatId });
      await tick();
      expect(messages(events)).toEqual(['reply']);
    } finally {
      await hub.dispose();
    }
  });

  test('replays only events after sinceSeq on reconnect', async () => {
    const actor = new FakeActor();
    actor.factsResult = { binding: { ...BINDING }, envelopes: [userInput('i-1', 'hi', 1)], overflow: false };
    const hub = new ChatStreamHub('chat-1', actor);

    const first: StreamEvent[] = [];
    await hub.subscribe(undefined, (event) => first.push(event));
    actor.emit(agentChunk('m-1', 'a', 2));
    await tick();
    actor.emit(agentChunk('m-1', 'b', 3));
    await tick();

    const resumeFrom = Math.max(1, hub.lastSeq - 1);
    const resumed: StreamEvent[] = [];
    await hub.subscribe(resumeFrom, (event) => resumed.push(event));
    expect(resumed.every((event) => event.seq > resumeFrom)).toBe(true);
    expect(resumed.some((event) => event.event === 'snapshot')).toBe(false);

    await hub.dispose();
  });

  test('falls back to a snapshot when the ring lost the requested position', async () => {
    const actor = new FakeActor();
    actor.factsResult = { binding: { ...BINDING }, envelopes: [userInput('i-1', 'hi', 1)], overflow: false };
    const hub = new ChatStreamHub('chat-1', actor, 3);

    await hub.subscribe(undefined, () => {});
    for (let sequence = 2; sequence <= 6; sequence += 1) {
      actor.emit(agentChunk('m-1', `chunk-${sequence}`, sequence));
      await tick();
    }

    const resumed: StreamEvent[] = [];
    await hub.subscribe(1, (event) => resumed.push(event));
    expect(resumed[0]?.event).toBe('snapshot');

    await hub.dispose();
  });

  test('surfaces Store overflow in the first snapshot and keeps projected messages', async () => {
    const actor = new FakeActor();
    actor.factsResult = {
      binding: { ...BINDING },
      envelopes: [userInput('i-1', 'kept', 1)],
      overflow: true,
    };
    const hub = new ChatStreamHub('chat-1', actor);

    const events: StreamEvent[] = [];
    await hub.subscribe(undefined, (event) => events.push(event));
    expect(events.map((event) => event.event)).toEqual(['snapshot']);
    const snapshot = events[0]?.data as ChatSnapshotEvent;
    expect(snapshot.overflow).toBe(true);
    expect(snapshot.messages.map((message) => message.text)).toEqual(['kept']);

    await hub.dispose();
  });

  test('emits a rebound reset and rebuilds messages when the binding changes', async () => {
    const actor = new FakeActor();
    actor.factsResult = {
      binding: { ...BINDING },
      envelopes: [userInput('i-1', 'old', 1)],
      overflow: false,
    };
    const hub = new ChatStreamHub('chat-1', actor);

    const events: StreamEvent[] = [];
    await hub.subscribe(undefined, (event) => events.push(event));
    events.length = 0;

    // The rebound drops the old turn; the new binding must accept its own input before chunks.
    const input = { ...userInput('i-2', 'new question', 2), sessionId: 'session-2', bindingRevision: 2 };
    actor.factsResult = {
      binding: { ...BINDING, sessionId: 'session-2', bindingRevision: 2 },
      envelopes: [input],
      overflow: false,
    };
    actor.emit(input);
    await tick();
    actor.emit({ ...agentChunk('m-2', 'new answer', 3), sessionId: 'session-2', bindingRevision: 2 });
    await tick();

    expect(events.map((event) => event.event)).toEqual(['reset', 'message', 'snapshot', 'message']);
    const reset = events[0]?.data as { reason: string } | undefined;
    expect(reset?.reason).toBe('rebound');

    const reopened: StreamEvent[] = [];
    await hub.subscribe(undefined, (event) => reopened.push(event));
    const snapshot = reopened[0]?.data as ChatSnapshotEvent;
    expect(snapshot.messages.map((message) => message.text)).toEqual(['new question', 'new answer']);
    actor.emit(agentChunk('late-rev1', 'stale', 50));
    await tick();
    const afterLate: StreamEvent[] = [];
    (await hub.subscribe(undefined, (event) => afterLate.push(event)))();
    expect(afterLate[0]?.data).toMatchObject({
      sessionId: 'session-2',
      bindingRevision: 2,
      messages: snapshot.messages,
    });

    await hub.dispose();
  });

  test('ABV-07 rev2 survives late rev1 and foreign Chat facts without rebound authority', async () => {
    const actor = new FakeActor();
    const binding = { ...BINDING, sessionId: 'session-2', bindingRevision: 2 };
    const input = { ...userInput('i-2', 'current', 1), ...binding };
    actor.factsResult = { binding, envelopes: [input], overflow: false };
    const hub = new ChatStreamHub(BINDING.chatId, actor);
    const events: StreamEvent[] = [];
    try {
      await hub.subscribe(undefined, (event) => events.push(event));
      const seq = hub.lastSeq;
      actor.emit(agentChunk('stale', 'old generation', 6));
      actor.emit({ ...userInput('foreign', 'foreign Chat'), chatId: 'other-chat', bindingRevision: 99 });
      actor.emit({ ...userInput('wrong-session', 'wrong session'), ...binding, sessionId: 'wrong-session' });
      await tick();
      const snapshots: StreamEvent[] = [];
      (await hub.subscribe(undefined, (event) => snapshots.push(event)))();
      expect(snapshots[0]?.data).toMatchObject({
        sessionId: 'session-2',
        bindingRevision: 2,
        messages: [expect.objectContaining({ text: 'current' })],
      });
      expect(hub.lastSeq).toBe(seq);
    } finally {
      await hub.dispose();
    }
    expect(actor.listeners.size).toBe(0);
  });

  test('ABV-07 unknown newer fact cannot rebound when actor facts still confirm current binding', async () => {
    const actor = new FakeActor();
    actor.factsResult = { binding: BINDING, envelopes: [userInput('i-1', 'current')], overflow: false };
    const hub = new ChatStreamHub(BINDING.chatId, actor);
    try {
      await hub.start();
      actor.emit({
        ...userInput('unconfirmed', 'not authoritative'),
        bindingRevision: 2,
        sessionId: 'new-session',
      });
      await tick();
      const events: StreamEvent[] = [];
      (await hub.subscribe(undefined, (event) => events.push(event)))();
      expect(events[0]?.data).toMatchObject({ bindingRevision: 1, sessionId: 'session-1' });
      const event = events[0];
      if (!event) throw new Error('snapshot missing');
      expect((event.data as ChatSnapshotEvent).messages.map((message) => message.text)).toEqual(['current']);
    } finally {
      await hub.dispose();
    }
  });

  test('R01 回读 pending rev2 时保留 rev3 信号，并重放权威 rebound', async () => {
    const actor = new FakeActor();
    actor.factsResult = revisionFacts(1);
    const hub = new ChatStreamHub(BINDING.chatId, actor);
    const pending = gate();
    let reads = 0;
    try {
      await hub.start();
      actor.facts = async () => {
        reads += 1;
        if (reads === 1) await pending.promise;
        return revisionFacts(reads + 1);
      };
      actor.emit({ ...userInput('seed', 'same revision', 2), ...revisionFacts(1).binding });
      await tick();
      const cursor = hub.lastSeq;
      actor.emit(revisionFacts(2).envelopes[0]);
      actor.emit(revisionFacts(3).envelopes[0]);
      actor.emit({ ...userInput('same', 'same revision'), ...revisionFacts(1).binding, sequence: 2 });
      actor.emit({ ...userInput('foreign', 'foreign'), chatId: 'other-chat', bindingRevision: 99 });
      pending.resolve();
      const events: StreamEvent[] = [];
      (await hub.subscribe(undefined, (event) => events.push(event)))();
      expect(reads).toBe(2);
      expect(events[0]?.data).toMatchObject({
        sessionId: 'session-3',
        bindingRevision: 3,
        messages: [expect.objectContaining({ text: 'confirmed-3' })],
      });
      const replay: StreamEvent[] = [];
      (await hub.subscribe(cursor, (event) => replay.push(event)))();
      expect(
        replay.some(
          (event) => event.event === 'snapshot' && (event.data as ChatSnapshotEvent).bindingRevision === 3,
        ),
      ).toBe(true);
      const lastSeq = hub.lastSeq;
      actor.emit(revisionFacts(2).envelopes[0]);
      actor.emit({ ...userInput('late', 'late other chat'), chatId: 'other-chat', bindingRevision: 100 });
      await tick();
      expect(hub.lastSeq).toBe(lastSeq);
      expect(reads).toBe(2);
    } finally {
      pending.resolve();
      await hub.dispose();
    }
  });

  test('R01 未确认的高 revision 不自旋，同代次通知不重复回读', async () => {
    const actor = new FakeActor();
    actor.factsResult = revisionFacts(1);
    const hub = new ChatStreamHub(BINDING.chatId, actor);
    const pending = gate();
    let reads = 0;
    try {
      await hub.start();
      actor.facts = async () => {
        reads += 1;
        await pending.promise;
        return revisionFacts(1);
      };
      actor.emit(revisionFacts(2).envelopes[0]);
      actor.emit(revisionFacts(2).envelopes[0]);
      pending.resolve();
      const events: StreamEvent[] = [];
      (await hub.subscribe(undefined, (event) => events.push(event)))();
      await tick();
      expect(reads).toBe(1);
      expect(events[0]?.data).toMatchObject({ bindingRevision: 1 });
    } finally {
      pending.resolve();
      await hub.dispose();
    }
  });

  test('R02 暂时失败后并发订阅共用一次恢复回读，不自动重试失败', async () => {
    const actor = new FakeActor();
    actor.factsResult = revisionFacts(1);
    const hub = new ChatStreamHub(BINDING.chatId, actor);
    const pending = gate();
    const failure = new Error('synthetic facts failure');
    let reads = 0;
    try {
      await hub.start();
      actor.facts = async () => {
        reads += 1;
        throw failure;
      };
      actor.emit(revisionFacts(2).envelopes[0]);
      actor.emit(revisionFacts(3).envelopes[0]);
      await tick();
      expect(reads).toBe(1);
      actor.facts = async () => {
        reads += 1;
        await pending.promise;
        return revisionFacts(3);
      };
      const events: StreamEvent[][] = [[], [], []];
      const reconnects = Promise.all(
        events.map((items) => hub.subscribe(undefined, (event) => items.push(event))),
      );
      void reconnects.catch(() => {});
      await tick();
      expect(reads).toBe(2);
      pending.resolve();
      for (const cancel of await reconnects) cancel();
      for (const items of events) expect(items[0]?.data).toMatchObject({ bindingRevision: 3 });
      expect(reads).toBe(2);
    } finally {
      pending.resolve();
      await hub.dispose();
    }
  });

  test('R02 并发失败只读一次，下一次重连可重试并补同 binding snapshot', async () => {
    const actor = new FakeActor();
    actor.factsResult = revisionFacts(1);
    const hub = new ChatStreamHub(BINDING.chatId, actor);
    const pending = gate();
    const failure = new Error('synthetic retry failure');
    let reads = 0;
    try {
      await hub.start();
      actor.facts = async () => {
        reads += 1;
        if (reads > 1) await pending.promise;
        throw failure;
      };
      actor.emit(revisionFacts(2).envelopes[0]);
      await tick();
      const reconnects = Promise.allSettled(
        Array.from({ length: 3 }, () => hub.subscribe(undefined, () => {})),
      );
      await tick();
      expect(reads).toBe(2);
      pending.resolve();
      for (const result of await reconnects) {
        expect(result.status).toBe('rejected');
        if (result.status === 'rejected') {
          expect(result.reason).toMatchObject({
            code: 'engine_unavailable',
            retryable: true,
            cause: failure,
          });
        }
      }
      await tick();
      expect(reads).toBe(2);
      const cursor = hub.lastSeq;
      actor.facts = async () => {
        reads += 1;
        return revisionFacts(1);
      };
      const events: StreamEvent[] = [];
      (await hub.subscribe(cursor, (event) => events.push(event)))();
      expect(
        events.some(
          (event) =>
            event.event === 'snapshot' &&
            (event.data as ChatSnapshotEvent).messages[0]?.text === 'confirmed-1',
        ),
      ).toBe(true);
      expect(reads).toBe(4);
    } finally {
      pending.resolve();
      await hub.dispose();
    }
  });

  test.each(['close', 'dispose'] as const)(
    'R01/R02 %s 清理 pending 更高信号，迟到失败不再发事件',
    async (action) => {
      const actor = new FakeActor();
      actor.factsResult = revisionFacts(1);
      const hub = new ChatStreamHub(BINDING.chatId, actor);
      const pending = gate();
      let reads = 0;
      try {
        const events: StreamEvent[] = [];
        await hub.subscribe(undefined, (event) => events.push(event));
        actor.facts = async () => {
          reads += 1;
          await pending.promise;
          throw new Error('synthetic late failure');
        };
        actor.emit(revisionFacts(2).envelopes[0]);
        await tick();
        actor.emit(revisionFacts(3).envelopes[0]);
        const ending = hub[action]();
        pending.resolve();
        await ending;
        const count = events.length;
        await tick();
        expect(events).toHaveLength(count);
        expect(reads).toBe(1);
        expect(actor.listeners.size).toBe(0);
        if (action === 'close') {
          const late: StreamEvent[] = [];
          (await hub.subscribe(undefined, (event) => late.push(event)))();
          expect(late.map((event) => event.event)).toEqual(['snapshot', 'closed']);
        }
      } finally {
        pending.resolve();
        await hub.dispose();
      }
    },
  );

  test('R01 同代次 closed 与 pending 更高信号交叉时不再回读', async () => {
    const actor = new FakeActor();
    actor.factsResult = revisionFacts(1);
    const hub = new ChatStreamHub(BINDING.chatId, actor);
    const pending = gate();
    let reads = 0;
    try {
      await hub.start();
      actor.facts = async () => {
        reads += 1;
        await pending.promise;
        return revisionFacts(1);
      };
      actor.emit(revisionFacts(2).envelopes[0]);
      await tick();
      actor.emit({ ...envelope(3, { kind: 'closed' }), ...revisionFacts(1).binding });
      actor.emit(revisionFacts(3).envelopes[0]);
      pending.resolve();
      const events: StreamEvent[] = [];
      (await hub.subscribe(undefined, (event) => events.push(event)))();
      expect(events.map((event) => event.event)).toEqual(['snapshot', 'closed']);
      expect(hub.terminal).toBe(true);
      expect(reads).toBe(1);
      expect(actor.listeners.size).toBe(0);
    } finally {
      pending.resolve();
      await hub.dispose();
    }
  });

  test('ends the stream with a closed event and reports it to late subscribers', async () => {
    const actor = new FakeActor();
    actor.factsResult = {
      binding: { ...BINDING },
      envelopes: [userInput('i-1', 'hi', 1)],
      overflow: false,
    };
    const hub = new ChatStreamHub('chat-1', actor);

    const events: StreamEvent[] = [];
    await hub.subscribe(undefined, (event) => events.push(event));
    actor.emit(envelope(2, { kind: 'closed' }));
    await tick();

    expect(events.at(-1)?.event).toBe('closed');
    expect(hub.terminal).toBe(true);

    const late: StreamEvent[] = [];
    await hub.subscribe(undefined, (event) => late.push(event));
    expect(late.map((event) => event.event)).toEqual(['snapshot', 'closed']);
  });
});
