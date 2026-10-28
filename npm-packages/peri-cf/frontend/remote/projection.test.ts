import { describe, expect, test } from 'bun:test';
import {
  Y, encodeSyncFrame, type ChatYjsChannel, type DocSnapshot, type DocUpdate,
} from '@peri-code/sdk/view';
import { ChatProjection } from './projection';

const chat = { id: 'session-1', title: '投影测试', updatedAt: '2026-10-20T00:00:00Z' };

class LoopbackChannel implements ChatYjsChannel {
  closed = false;
  readonly frames: Uint8Array[] = [];
  private readonly messages = new Set<(frame: Uint8Array) => void>();
  private readonly closes = new Set<(error?: Error) => void>();

  send(frame: Uint8Array): void { this.frames.push(frame); }
  onMessage(listener: (frame: Uint8Array) => void): () => void {
    this.messages.add(listener);
    return () => this.messages.delete(listener);
  }
  onClose(listener: (error?: Error) => void): () => void {
    this.closes.add(listener);
    return () => this.closes.delete(listener);
  }
  close(reason?: Error): void {
    if (this.closed) return;
    this.closed = true;
    for (const listener of this.closes) listener(reason);
  }
  deliver(frame: Uint8Array): void { for (const listener of this.messages) listener(frame); }
}

function docs(options: { sessionId?: string; running?: boolean; schema?: number } = {}) {
  const chatDoc = new Y.Doc();
  const sessionDoc = new Y.Doc();
  const chatRoot = chatDoc.getMap('root');
  chatRoot.set('schemaVersion', 3);
  chatRoot.set('entries', new Y.Map());
  chatRoot.set('entryOrder', new Y.Array());
  const sessionRoot = sessionDoc.getMap('root');
  sessionRoot.set('schemaVersion', options.schema ?? 2);
  const info = new Y.Map();
  info.set('sessionId', options.sessionId ?? 'session-1');
  info.set('bindingRevision', 1);
  info.set('running', options.running ?? false);
  sessionRoot.set('session', info);
  return { chatDoc, sessionDoc, chatRoot, sessionRoot, info };
}

function snapshot(pair: ReturnType<typeof docs>, generation = 'g1', sequence = 0): DocSnapshot {
  return {
    protocol: 2, generation, sequence, mode: 'snapshot',
    chat: Y.encodeStateAsUpdateV2(pair.chatDoc), session: Y.encodeStateAsUpdateV2(pair.sessionDoc),
  };
}

function frame(snapshotValue: DocSnapshot, delivery = 1): Uint8Array {
  return encodeSyncFrame({ type: 'snapshot', delivery, snapshot: snapshotValue });
}

function update(pair: ReturnType<typeof docs>, generation = 'g1', sequence = 1): DocUpdate {
  return {
    protocol: 2, generation, sequence,
    chat: Y.encodeStateAsUpdateV2(pair.chatDoc), session: Y.encodeStateAsUpdateV2(pair.sessionDoc),
  };
}

async function settle(): Promise<void> {
  for (let index = 0; index < 4; index += 1) await Promise.resolve();
}

describe('ChatProjection over ChatReplica', () => {
  test('starts not-ready without fabricating entries or running state', () => {
    const projection = new ChatProjection(chat);
    expect(projection.state()).toMatchObject({
      chat, readiness: 'not-ready', sessionId: null, bindingRevision: null,
      running: false, entries: [], pendingInteractions: [],
    });
  });

  test('applies a snapshot, sends ACK, and exposes replica facts', async () => {
    const projection = new ChatProjection(chat);
    const channel = new LoopbackChannel();
    projection.connect(channel);
    const pair = docs({ running: true });
    channel.deliver(frame(snapshot(pair)));
    await settle();
    expect(projection.status).toBe('synced');
    expect(projection.state()).toMatchObject({
      readiness: 'ready', sessionId: 'session-1', bindingRevision: 1, running: true,
    });
    // 客户端只回 ACK，不写业务文档。
    expect(channel.frames).toHaveLength(2);
    expect(channel.frames[0]?.[0]).toBe(0x01);
    expect(channel.frames[1]?.[0]).toBe(0x04);
  });

  test('applies updates without replacing the binding identity', async () => {
    const projection = new ChatProjection(chat);
    const channel = new LoopbackChannel();
    projection.connect(channel);
    const pair = docs({ running: true });
    channel.deliver(frame(snapshot(pair)));
    await settle();
    pair.info.set('running', false);
    channel.deliver(encodeSyncFrame({ type: 'update', delivery: 2, update: update(pair) }));
    await settle();
    expect(projection.state()).toMatchObject({ running: false, sessionId: 'session-1', bindingRevision: 1 });
  });

  test('a rebind updates the displayed chat identity and session title', async () => {
    const projection = new ChatProjection(chat);
    const channel = new LoopbackChannel();
    projection.connect(channel);
    const pair = docs();
    pair.info.set('title', '旧标题');
    channel.deliver(frame(snapshot(pair)));
    await settle();
    expect(projection.state().chat).toMatchObject({ id: 'session-1', title: '旧标题' });

    pair.info.set('sessionId', 'session-2');
    pair.info.set('bindingRevision', 2);
    pair.info.set('title', '新标题');
    channel.deliver(encodeSyncFrame({ type: 'update', delivery: 2, update: update(pair) }));
    await settle();
    expect(projection.state()).toMatchObject({ sessionId: 'session-2', bindingRevision: 2 });
    expect(projection.state().chat).toMatchObject({ id: 'session-2', title: '新标题' });
  });

  test('marks unsupported schema without turning it into empty history', async () => {
    const projection = new ChatProjection(chat);
    const channel = new LoopbackChannel();
    projection.connect(channel);
    channel.deliver(frame(snapshot(docs({ schema: 99 }))));
    await settle();
    expect(projection.state()).toMatchObject({
      readiness: 'unsupported', entries: [], running: false,
    });
  });
});
