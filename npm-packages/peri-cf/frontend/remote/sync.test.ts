import { describe, expect, test } from 'bun:test';
import {
  Y, decodeAckFrame, decodeHelloFrame, encodeSyncFrame, type DocSnapshot,
} from '@peri-code/sdk/view';
import { ChatProjection } from './projection';
import { ChatSyncProvider, SyncBackpressureError, WebSocketChatChannel, type WebSocketLike } from './sync';

const chat = { id: 'session-1', title: '同步测试', updatedAt: '2026-10-20T00:00:00Z' };

class FakeSocket implements WebSocketLike {
  binaryType = '';
  readyState = 0;
  bufferedAmount = 0;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onerror: ((event: unknown) => void) | null = null;
  onclose: ((event: { code: number; reason: string }) => void) | null = null;
  readonly sent: Uint8Array[] = [];
  closeCode: number | undefined;

  send(data: Uint8Array): void { this.sent.push(data); }
  close(code?: number): void {
    if (this.readyState === 3) return;
    this.readyState = 3;
    this.closeCode = code;
    this.onclose?.({ code: code ?? 1000, reason: '' });
  }
  open(): void { this.readyState = 1; this.onopen?.(); }
  message(data: unknown): void { this.onmessage?.({ data }); }
  drop(code = 1006): void {
    this.readyState = 3;
    this.closeCode = code;
    this.onclose?.({ code, reason: 'network' });
  }
}

function docs(options: { sessionId?: string; running?: boolean } = {}) {
  const chatDoc = new Y.Doc();
  const sessionDoc = new Y.Doc();
  chatDoc.getMap('root').set('schemaVersion', 3);
  chatDoc.getMap('root').set('entries', new Y.Map());
  chatDoc.getMap('root').set('entryOrder', new Y.Array());
  const root = sessionDoc.getMap('root');
  root.set('schemaVersion', 2);
  const info = new Y.Map();
  info.set('sessionId', options.sessionId ?? 'session-1');
  info.set('bindingRevision', 1);
  info.set('running', options.running ?? false);
  root.set('session', info);
  return { chatDoc, sessionDoc };
}

function snapshotFrame(options: { generation?: string; sequence?: number; sessionId?: string } = {}): Uint8Array {
  const pair = docs({ sessionId: options.sessionId });
  const snapshot: DocSnapshot = {
    protocol: 2, generation: options.generation ?? 'g1', sequence: options.sequence ?? 0, mode: 'snapshot',
    chat: Y.encodeStateAsUpdateV2(pair.chatDoc), session: Y.encodeStateAsUpdateV2(pair.sessionDoc),
  };
  return encodeSyncFrame({ type: 'snapshot', delivery: 1, snapshot });
}

async function settle(): Promise<void> {
  for (let index = 0; index < 6; index += 1) await Promise.resolve();
}

function provider(sockets: FakeSocket[], options: { reconnect?: boolean; maxAttempts?: number } = {}) {
  const projection = new ChatProjection(chat);
  const statuses: string[] = [];
  const sync = new ChatSyncProvider({
    url: 'ws://example.test/api/chats/chat-1/sync',
    projection,
    webSocketFactory: () => {
      const socket = new FakeSocket();
      sockets.push(socket);
      return socket;
    },
    reconnect: options.reconnect ?? false,
    reconnectDelayMs: 0,
    ...(options.maxAttempts === undefined ? {} : { maxAttempts: options.maxAttempts }),
    onStatus: (status) => statuses.push(status),
  });
  sync.start();
  return { projection, sync, statuses };
}

describe('WebSocketChatChannel framing', () => {
  test('queues HELLO until open and sends it as binary', async () => {
    const sockets: FakeSocket[] = [];
    const { sync } = provider(sockets);
    const socket = sockets[0]!;
    expect(socket.sent).toHaveLength(0);
    socket.open();
    expect(socket.sent).toHaveLength(1);
    expect(decodeHelloFrame(socket.sent[0]!)).toEqual({ type: 'hello' });
    await sync.close();
  });

  test('client applies SNAPSHOT and replies ACK, never writes a document update', async () => {
    const sockets: FakeSocket[] = [];
    const { projection, sync } = provider(sockets);
    const socket = sockets[0]!;
    socket.open();
    socket.message(snapshotFrame());
    await settle();
    expect(projection.state()).toMatchObject({ readiness: 'ready', sessionId: 'session-1' });
    const ack = socket.sent.at(-1)!;
    expect(decodeAckFrame(ack)).toEqual({ type: 'ack', delivery: 1 });
    expect(socket.sent.every(frame => frame[0] === 0x01 || frame[0] === 0x04)).toBe(true);
    await sync.close();
  });

  test('reconnect sends HELLO with the existing state vector for delta resume', async () => {
    const sockets: FakeSocket[] = [];
    const { sync } = provider(sockets, { reconnect: true });
    sockets[0]!.open();
    sockets[0]!.message(snapshotFrame());
    await settle();
    sockets[0]!.drop();
    await new Promise((resolve) => setTimeout(resolve, 5));
    expect(sockets).toHaveLength(2);
    sockets[1]!.open();
    const hello = decodeHelloFrame(sockets[1]!.sent[0]!);
    expect(hello.type).toBe('hello');
    expect(hello.resume).toBeDefined();
    expect(hello.resume?.chat.byteLength).toBeGreaterThan(0);
    await sync.close();
  });
});

describe('WebSocketChatChannel explicit failures', () => {  test('text frames are rejected and the connection fails explicitly', async () => {
    const sockets: FakeSocket[] = [];
    const { sync, statuses } = provider(sockets);
    sockets[0]!.open();
    sockets[0]!.message('not-binary');
    await settle();
    expect(statuses).toContain('failed');
    expect(sockets[0]!.closeCode).toBe(1011);
    await sync.close();
  });

  test('slow socket backpressure fails without reconnecting', async () => {
    const sockets: FakeSocket[] = [];
    const { sync } = provider(sockets, { reconnect: true });
    const socket = sockets[0]!;
    socket.bufferedAmount = 2_000_000;
    socket.open();
    await settle();
    expect(socket.closeCode).toBe(1011);
    await new Promise((resolve) => setTimeout(resolve, 5));
    expect(sockets).toHaveLength(1);
    expect(socket.sent).toHaveLength(0);
    await sync.close();
  });

  test('a closed channel refuses further sends instead of dropping frames', async () => {
    const sockets: FakeSocket[] = [];
    const channel = new WebSocketChatChannel('ws://example.test', () => {
      const socket = new FakeSocket();
      sockets.push(socket);
      return socket;
    });
    channel.close();
    expect(() => channel.send(new Uint8Array([1]))).toThrow();
  });

  test('backpressure error exposes the buffered byte count', () => {
    const error = new SyncBackpressureError(2_000_000);
    expect(error.name).toBe('SyncBackpressureError');
    expect(error.bufferedBytes).toBe(2_000_000);
  });
});

describe('ChatSyncProvider reconnect budget', () => {
  test('a successful resync resets the budget so long sessions keep reconnecting', async () => {
    const sockets: FakeSocket[] = [];
    const { sync } = provider(sockets, { reconnect: true, maxAttempts: 2 });
    // 5 次断线都发生在成功同步之后：每次都算新的连续失败序列。
    for (let cycle = 0; cycle < 5; cycle += 1) {
      const socket = sockets.at(-1)!;
      socket.open();
      socket.message(snapshotFrame({ sequence: cycle }));
      await settle();
      socket.drop();
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
    expect(sockets).toHaveLength(6);
    await sync.close();
  });
});
