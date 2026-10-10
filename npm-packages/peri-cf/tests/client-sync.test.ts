import { afterEach, describe, expect, spyOn, test } from 'bun:test';
import { Y, SessionDocSync, type DocStateVector } from '@peri-code/sdk/view';
import { decodeAuthFrame, decodeAckFrame, encodeSyncFrame, MAX_SYNC_FRAME_BYTES,
  type SyncFramePayload } from '@peri-code/sdk/view';
import type { SyncState } from '../shared/sync';
import { chatCommandSchema, type ChatCommand } from '../shared/sync';
import { ChatSync, type SyncStatus } from '../web/api/sync';
import { ChatSession, type ChatView } from '../web/chat/session';
import { createChatQueries } from '../web/chat/queries';
import type { Chat } from '../web/chat/types';

const chat: Chat = { id: 'chat-1', title: '思考', updatedAt: '2026-10-06T00:00:00Z' };
const cleanup: (() => void | Promise<unknown>)[] = [];

afterEach(async () => {
  for (const dispose of cleanup.splice(0).reverse()) await dispose();
});

class FakeWebSocket {
  onopen: WebSocket['onopen'] = null;
  onmessage: WebSocket['onmessage'] = null;
  onclose: WebSocket['onclose'] = null;
  onerror: WebSocket['onerror'] = null;
  sent: Uint8Array[] = [];
  /** 文本帧 = 应用控制帧；默认由 fake 立即确认，测试可关掉它自行回合。 */
  frames: string[] = [];
  commands: ChatCommand[] = [];
  autoAck = true;
  binaryType: BinaryType = 'arraybuffer';
  closed = 0;
  private delivery = 0;

  constructor(readonly url: string) {}
  send(data: string | Uint8Array) {
    if (typeof data === 'string') { this.receive(data); return; }
    this.sent.push(data);
  }
  close() { this.closed++; }
  open() { this.onopen?.call(this as unknown as WebSocket, new Event('open')); }
  // Frame payloads receive the delivery number a real server socket assigns; raw values pass through untouched.
  message(data: unknown, delivery?: number) {
    const payload = typeof data === 'object' && data !== null && !(data instanceof Uint8Array) && 'type' in data
      ? encodeSyncFrame({ ...(data as SyncFramePayload), delivery: delivery ?? ++this.delivery })
      : data;
    this.publish(payload);
  }
  /** 服务端命令结果：文本帧，与二进制文档帧走同一连接但不同通道。 */
  result(id: string, outcome: { ok?: boolean; error?: { code: string; message: string }; payload?: { id: string; bytes: number; json: string } } = {}) {
    this.publish(JSON.stringify({ type: 'command_result', id, ok: outcome.ok ?? true,
      ...(outcome.error ? { error: outcome.error } : {}), ...(outcome.payload ? { payload: outcome.payload } : {}) }));
  }
  disconnect(code = 1006) {
    this.onclose?.call(this as unknown as WebSocket, { code } as CloseEvent);
  }
  private receive(data: string) {
    // 用冻结 schema 解析：帧形状不符在这里直接失败，而不是被客户端悄悄接受。
    const command = chatCommandSchema.parse(JSON.parse(data));
    this.frames.push(data);
    this.commands.push(command);
    if (this.autoAck) this.result(command.id);
  }
  private publish(data: unknown) {
    this.onmessage?.call(this as unknown as WebSocket, { data } as MessageEvent);
  }
}

// Client-to-server traffic after the auth frame must be nothing but delivery acknowledgements.
function acknowledgements(socket: FakeWebSocket) {
  return socket.sent.slice(1).map(frame => decodeAckFrame(frame));
}

/** 会话命令只能出现在某条已建立连接的文本帧上；按 op 统计发给服务端的命令。 */
function commandOps(sockets: FakeWebSocket[], op: ChatCommand['op']): ChatCommand[] {
  return sockets.flatMap(socket => socket.commands).filter(command => command.op === op);
}

function promptContents(sockets: FakeWebSocket[]): string[] {
  return commandOps(sockets, 'prompt').flatMap(command => command.op === 'prompt' ? [command.content] : []);
}

function transport() {
  const sockets: FakeWebSocket[] = [];
  return {
    sockets,
    socket: (url: string) => {
      const socket = new FakeWebSocket(url);
      sockets.push(socket);
      return socket as unknown as WebSocket;
    },
    origin: 'https://example.test', reconnectDelayMs: 1,
  };
}

async function until(predicate: () => boolean) {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (predicate()) return;
    await new Promise(resolve => setTimeout(resolve, 2));
  }
  throw new Error('Expected lifecycle transition did not occur');
}

function deferred<Value>() {
  let resolve!: (value: Value) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<Value>((accept, fail) => { resolve = accept; reject = fail; });
  return { promise, resolve, reject };
}

function producer(target = chat, content = 'partial') {
  const chatDoc = new Y.Doc();
  const sessionDoc = new Y.Doc();
  const root = chatDoc.getMap('root');
  // 文档对必须带 schema 版本标记（真实生产端由 SDK DocModel 写入）。
  root.set('schemaVersion', 3);
  sessionDoc.getMap('root').set('schemaVersion', 2);
  const entry = new Y.Map<unknown>();
  const text = new Y.Text(content);
  const block = new Y.Map<unknown>();
  block.set('blockId', 'text');
  block.set('type', 'text');
  block.set('text', text);
  const blocks = new Y.Map<unknown>();
  blocks.set('text', block);
  const blockOrder = new Y.Array<string>();
  blockOrder.push(['text']);
  entry.set('entryId', 'entry');
  entry.set('turnId', 'turn');
  entry.set('role', 'assistant');
  entry.set('status', 'completed');
  entry.set('blocks', blocks);
  entry.set('blockOrder', blockOrder);
  const entries = new Y.Map<unknown>();
  entries.set('entry', entry);
  const order = new Y.Array<string>();
  order.push(['entry']);
  root.set('entries', entries);
  root.set('entryOrder', order);
  const metadata = sessionDoc.getMap('peri-cf');
  metadata.set('state', { chat: target, running: false, executionBlocked: false });
  const sync = new SessionDocSync(chatDoc, sessionDoc, { flushIntervalMs: 100_000 });
  const updates: Parameters<typeof sync.subscribe>[0] extends (update: infer Update) => unknown ? Update[] : never = [];
  const subscription = sync.subscribe(update => { updates.push(update); });
  cleanup.push(async () => { subscription.unsubscribe(); await sync.close(); chatDoc.destroy(); sessionDoc.destroy(); });
  return {
    chatDoc, sessionDoc, entry, text,
    snapshot(resume?: DocStateVector) {
      const peer = sync.subscribe(() => {}, { resume });
      peer.unsubscribe();
      return { type: 'snapshot', snapshot: peer.snapshot } satisfies SyncFramePayload;
    },
    update(change: () => void) {
      change();
      sync.flush();
      return { type: 'update', update: updates.at(-1)! } satisfies SyncFramePayload;
    },
    state(change: Record<string, unknown>) {
      metadata.set('state', { ...metadata.get('state') as object, ...change });
    },
  };
}

function entryText(state: SyncState | undefined): string {
  const entry = state?.entries[0];
  return entry ? entry.blocks.flatMap((block) => (block.type === 'text' ? [block.text] : [])).join('') : '';
}

function historyState(chatId: string, content = 'HTTP history'): SyncState {
  const target = { ...chat, id: chatId };
  return {
    chat: target, running: false, executionBlocked: false, readiness: 'ready',
    entries: content ? [{ entryId: 'history', id: 'history', kind: 'message', turnId: 'turn:history',
      role: 'assistant', status: 'completed', createdAt: target.updatedAt, createdAtSource: 'observed',
      completedAt: null, error: null, blocks: [{ blockId: 'text:0', type: 'text', text: content }] }] : [],
    toolCalls: [], turns: [], activeTurn: { turnId: null, status: null }, session: {}, tasks: [],
    plans: { current: [], byTurn: {} }, interactions: [], pendingInteractions: [], inputQueue: null,
  };
}

function replica() {
  const connection = transport();
  const states: SyncState[] = [];
  const statuses: SyncStatus[] = [];
  const client = new ChatSync({ ...connection, chatId: chat.id, token: 'fixture-token',
    onState: state => states.push(state), onStatus: status => statuses.push(status) });
  cleanup.push(() => client.dispose());
  return { ...connection, states, statuses, client };
}

describe('read-only SDK Yjs WebSocket replica', () => {
  test('ACKs only successfully applied and published delivery frames without writing document content', () => {
    const server = producer();
    const client = replica();
    const socket = client.sockets[0];
    socket.open();
    socket.message(server.snapshot(), 1);
    expect(decodeAckFrame(socket.sent[1])).toEqual({ type: 'ack', delivery: 1 });
    socket.message(server.update(() => server.text.insert(server.text.length, '!')), 2);
    expect(entryText(client.states.at(-1))).toBe('partial!');
    expect(decodeAckFrame(socket.sent[2])).toEqual({ type: 'ack', delivery: 2 });
    socket.message(server.update(() => server.state({ running: true })), 4);
    expect(socket.sent).toHaveLength(3);
    expect(client.statuses.at(-1)).toBe('failed');
  });
  test('authenticates in the first frame, never in URL or subprotocol, and reads projected state', () => {
    const server = producer();
    const client = replica();
    const socket = client.sockets[0];
    expect(socket.url).toBe('wss://example.test/api/v2/chats/chat-1/sync');
    expect(socket.sent).toEqual([]);
    socket.open();
    expect(decodeAuthFrame(socket.sent[0])).toEqual({ type: 'auth', token: 'fixture-token' });
    socket.message(server.snapshot());
    expect(client.statuses.at(-1)).toBe('synced');
    expect(client.states.at(-1)?.entries[0]).toEqual({
      entryId: 'entry', id: 'entry', kind: 'message', turnId: 'turn', role: 'assistant', status: 'completed',
      createdAt: null, createdAtSource: 'unknown', completedAt: null, error: null,
      blocks: [{ blockId: 'text', type: 'text', text: 'partial' }],
    });
    expect(acknowledgements(socket)).toEqual([{ type: 'ack', delivery: 1 }]);
  });

  test('observes chat-only text updates and session-only running/blocked/terminal changes without writing back', () => {
    const server = producer();
    const client = replica();
    const socket = client.sockets[0];
    socket.open();
    socket.message(server.snapshot());
    socket.message(server.update(() => server.text.insert(server.text.length, ' answer')));
    expect(entryText(client.states.at(-1))).toBe('partial answer');
    socket.message(server.update(() => server.state({ running: true })));
    expect(client.states.at(-1)?.running).toBe(true);
    socket.message(server.update(() => server.state({ running: false, executionBlocked: true, error: 'stop failed' })));
    expect(client.states.at(-1)).toMatchObject({ running: false, executionBlocked: true, error: 'stop failed' });
    expect(acknowledgements(socket)).toEqual([1, 2, 3, 4].map(delivery => ({ type: 'ack', delivery })));
  });

  test('replaces both documents across generations and rebinds their observers', () => {
    const first = producer(chat, 'old');
    const second = producer(chat, 'replacement');
    const client = replica();
    const socket = client.sockets[0];
    socket.open();
    socket.message(first.snapshot());
    socket.message(second.snapshot());
    expect(entryText(client.states.at(-1))).toBe('replacement');
    socket.message(second.update(() => second.text.insert(second.text.length, '!')));
    expect(entryText(client.states.at(-1))).toBe('replacement!');
    socket.message(second.update(() => second.state({ running: true })));
    expect(client.states.at(-1)?.running).toBe(true);
    expect(acknowledgements(socket)).toEqual([1, 2, 3, 4].map(delivery => ({ type: 'ack', delivery })));
  });

  test('actually detaches transaction observers from replaced docs and binds the new pair', () => {
    const on = spyOn(Y.Doc.prototype, 'on');
    const off = spyOn(Y.Doc.prototype, 'off');
    try {
      const first = producer();
      const second = producer(chat, 'replacement');
      const client = replica();
      const socket = client.sockets[0];
      socket.open();
      on.mockClear();
      off.mockClear();
      socket.message(first.snapshot());
      expect(on.mock.calls.filter(([event]) => event === 'afterTransaction')).toHaveLength(2);
      socket.message(second.snapshot());
      expect(off.mock.calls.filter(([event]) => event === 'afterTransaction')).toHaveLength(2);
      expect(on.mock.calls.filter(([event]) => event === 'afterTransaction')).toHaveLength(4);
      client.client.dispose();
      expect(off.mock.calls.filter(([event]) => event === 'afterTransaction')).toHaveLength(4);
    } finally {
      on.mockRestore();
      off.mockRestore();
    }
  });

  test('reconnects with typed state vectors and repairs missed updates by a delta snapshot', async () => {
    const server = producer();
    const client = replica();
    const first = client.sockets[0];
    first.open();
    first.message(server.snapshot());
    first.disconnect();
    expect(client.statuses.at(-1)).toBe('disconnected');
    server.update(() => server.text.insert(server.text.length, ' missed'));
    await until(() => client.sockets.length === 2);
    const second = client.sockets[1];
    second.open();
    const auth = decodeAuthFrame(second.sent[0]);
    expect(auth.resume?.protocol).toBe(2);
    expect(auth.resume?.chat).toBeInstanceOf(Uint8Array);
    const snapshot = server.snapshot(auth.resume);
    expect(snapshot.type === 'snapshot' && snapshot.snapshot.mode).toBe('delta');
    second.message(snapshot);
    expect(entryText(client.states.at(-1))).toBe('partial missed');
    expect(client.statuses.at(-1)).toBe('synced');
    expect(first.closed).toBe(1);
    expect(acknowledgements(second)).toEqual([{ type: 'ack', delivery: 1 }]);
  });

  test('rejects sequence gaps, ignores stale socket events, and resumes rather than applying out of order', async () => {
    const server = producer();
    const client = replica();
    const first = client.sockets[0];
    first.open();
    first.message(server.snapshot());
    const late = first.onmessage!;
    server.update(() => server.text.insert(server.text.length, ' skipped'));
    const gap = server.update(() => server.text.insert(server.text.length, ' next'));
    first.message(gap);
    expect(entryText(client.states.at(-1))).toBe('partial');
    expect(client.statuses.at(-1)).toBe('disconnected');
    await until(() => client.sockets.length === 2);
    const second = client.sockets[1];
    second.open();
    second.message(server.snapshot(decodeAuthFrame(second.sent[0]).resume));
    const published = client.states.length;
    late.call(first as unknown as WebSocket, { data: encodeSyncFrame({ ...gap, delivery: 3 }) } as MessageEvent);
    expect(client.states).toHaveLength(published);
    expect(entryText(client.states.at(-1))).toBe('partial skipped next');
  });

  test('resumes across a backend generation change by replacing documents with a full snapshot', async () => {
    const firstServer = producer(chat, 'old generation');
    const client = replica();
    client.sockets[0].open();
    client.sockets[0].message(firstServer.snapshot());
    client.sockets[0].disconnect();
    const restarted = producer(chat, 'new generation');
    await until(() => client.sockets.length === 2);
    const socket = client.sockets[1];
    socket.open();
    const snapshot = restarted.snapshot(decodeAuthFrame(socket.sent[0]).resume);
    expect(snapshot.type === 'snapshot' && snapshot.snapshot.mode).toBe('snapshot');
    socket.message(snapshot);
    socket.message(restarted.update(() => restarted.text.insert(restarted.text.length, '!')));
    expect(entryText(client.states.at(-1))).toBe('new generation!');
    expect(client.statuses.at(-1)).toBe('synced');
  });

  test('ignores duplicate sequence updates without duplicating text', () => {
    const server = producer();
    const client = replica();
    const socket = client.sockets[0];
    socket.open();
    socket.message(server.snapshot());
    const update = server.update(() => server.text.insert(server.text.length, '!'));
    socket.message(update);
    socket.message(update);
    expect(entryText(client.states.at(-1))).toBe('partial!');
    expect(acknowledgements(socket)).toEqual([1, 2, 3].map(delivery => ({ type: 'ack', delivery })));
  });

  test.each([1008, 1009, 4401, 4403, 4404])('does not retry permanent close code %s', async code => {
    const client = replica();
    client.sockets[0].open();
    client.sockets[0].disconnect(code);
    expect(client.statuses.at(-1)).toBe('failed');
    await new Promise(resolve => setTimeout(resolve, 8));
    expect(client.sockets).toHaveLength(1);
  });

  test('retries 1011 only up to the configured bound', async () => {
    const connection = transport();
    const statuses: SyncStatus[] = [];
    const client = new ChatSync({ ...connection, chatId: chat.id, token: 'token', maxReconnectAttempts: 2,
      onState() {}, onStatus: status => statuses.push(status) });
    cleanup.push(() => client.dispose());
    connection.sockets[0].disconnect(1011);
    await until(() => connection.sockets.length === 2);
    connection.sockets[1].disconnect(1011);
    await until(() => connection.sockets.length === 3);
    connection.sockets[2].disconnect(1011);
    expect(statuses.at(-1)).toBe('failed');
    await new Promise(resolve => setTimeout(resolve, 8));
    expect(connection.sockets).toHaveLength(3);
  });

  test('times out readiness instead of leaving a silent connection pending forever', async () => {
    const connection = transport();
    const statuses: SyncStatus[] = [];
    const client = new ChatSync({ ...connection, chatId: chat.id, token: 'token', readyTimeoutMs: 5,
      maxReconnectAttempts: 0, onState() {}, onStatus: status => statuses.push(status) });
    cleanup.push(() => client.dispose());
    connection.sockets[0].open();
    await until(() => statuses.at(-1) === 'failed');
    expect(connection.sockets[0].closed).toBe(1);
  });

  test.each(['invalid-json', '{}', new Uint8Array([1]), new Uint8Array(MAX_SYNC_FRAME_BYTES + 1)])(
    'blocks text, malformed and oversized frames', data => {
      const client = replica();
      client.sockets[0].message(data);
      expect(client.statuses.at(-1)).toBe('failed');
      expect(client.states).toHaveLength(0);
      expect(client.sockets[0].closed).toBe(1);
    });

  test('rejects wrong chat identity and invalid metadata without publishing UI state', () => {
    const server = producer({ ...chat, id: 'other-chat' });
    const client = replica();
    client.sockets[0].message(server.snapshot());
    expect(client.statuses.at(-1)).toBe('failed');
    expect(client.states).toHaveLength(0);
    const invalid = producer();
    invalid.state({ running: 'yes' });
    const other = replica();
    other.sockets[0].message(invalid.snapshot());
    expect(other.statuses.at(-1)).toBe('failed');
    expect(other.states).toHaveLength(0);
  });

  test('disposal clears handlers/timers and ignores late frames without reconnecting', async () => {
    const server = producer();
    const client = replica();
    const socket = client.sockets[0];
    const late = socket.onmessage!;
    socket.open();
    socket.message(server.snapshot());
    socket.disconnect();
    client.client.dispose();
    client.client.dispose();
    late.call(socket as unknown as WebSocket, { data: encodeSyncFrame({ ...server.snapshot(), delivery: 1 }) } as MessageEvent);
    await new Promise(resolve => setTimeout(resolve, 8));
    expect(client.sockets).toHaveLength(1);
    expect(client.states).toHaveLength(1);
    expect(socket.onopen).toBeNull();
    expect(socket.onmessage).toBeNull();
    expect(socket.onclose).toBeNull();
    expect(socket.onerror).toBeNull();
  });
});

describe('WS control-frame command channel', () => {
  async function synced() {
    const server = producer();
    const client = replica();
    const socket = client.sockets[0];
    socket.autoAck = false;
    socket.open();
    socket.message(server.snapshot());
    expect(client.statuses.at(-1)).toBe('synced');
    return { client, server, socket };
  }

  test('encodes prompt/cancel/interaction/payload as JSON text frames and matches results by id', async () => {
    const { client, socket } = await synced();
    const sends = [
      client.client.command({ type: 'command', id: 'c1', op: 'prompt', content: 'hello' }),
      client.client.command({ type: 'command', id: 'c2', op: 'cancel' }),
      client.client.command({ type: 'command', id: 'c3', op: 'interaction', interactionId: 'interaction:1',
        answer: { kind: 'permission', decision: 'allow_once' } }),
      client.client.command({ type: 'command', id: 'c4', op: 'payload', refId: 'gen:1' }),
    ];
    // 文本帧就是应用控制帧（严格 schema，无多余字段）；SDK 文档同步帧仍走二进制通道。
    expect(socket.frames.every(frame => typeof frame === 'string')).toBe(true);
    expect(socket.commands).toEqual([
      { type: 'command', id: 'c1', op: 'prompt', content: 'hello' },
      { type: 'command', id: 'c2', op: 'cancel' },
      { type: 'command', id: 'c3', op: 'interaction', interactionId: 'interaction:1', answer: { kind: 'permission', decision: 'allow_once' } },
      { type: 'command', id: 'c4', op: 'payload', refId: 'gen:1' },
    ]);
    let matched = false;
    void sends[2]!.then(() => { matched = true; });
    socket.result('other-id');
    await Promise.resolve();
    expect(matched).toBe(false);
    socket.result('c1');
    socket.result('c2', { ok: false, error: { code: 'settled', message: 'turn settled' } });
    socket.result('c3');
    socket.result('c4', { payload: { id: 'gen:1', bytes: 8, json: '{"a":1}' } });
    await expect(sends[0]).resolves.toEqual({ ok: true });
    await expect(sends[1]).resolves.toEqual({ ok: false, source: 'server', code: 'settled', message: 'turn settled' });
    await expect(sends[2]).resolves.toEqual({ ok: true });
    await expect(sends[3]).resolves.toEqual({ ok: true, payload: { id: 'gen:1', bytes: 8, json: '{"a":1}' } });
  });

  test('rejects commands until the replica is synced without writing frames', async () => {
    const client = replica();
    const socket = client.sockets[0];
    await expect(client.client.command({ type: 'command', id: 'c1', op: 'cancel' }))
      .resolves.toMatchObject({ ok: false, source: 'client', code: 'unavailable' });
    socket.open();
    await expect(client.client.command({ type: 'command', id: 'c2', op: 'cancel' }))
      .resolves.toMatchObject({ ok: false, source: 'client', code: 'unavailable' });
    expect(socket.frames).toEqual([]);
  });

  test('times out unanswered commands as unconfirmed and never retries them on the wire', async () => {
    const { client, server, socket } = await synced();
    const outcome = await client.client.command({ type: 'command', id: 'c1', op: 'prompt', content: 'hello' }, { timeoutMs: 5 });
    expect(outcome).toMatchObject({ ok: false, source: 'client', code: 'transport_error' });
    expect(outcome.ok ? '' : outcome.message).toContain('超时');
    // 迟到的结果不复活已作废的命令，也不影响后续文档流与确认。
    socket.result('c1');
    socket.message(server.update(() => server.text.insert(server.text.length, '!')), 2);
    expect(entryText(client.states.at(-1))).toBe('partial!');
    expect(acknowledgements(socket)).toEqual([{ type: 'ack', delivery: 1 }, { type: 'ack', delivery: 2 }]);
    expect(socket.commands).toHaveLength(1);
  });

  test('disconnect invalidates in-flight commands instead of replaying them on the next connection', async () => {
    const { client, server, socket } = await synced();
    const pending = client.client.command({ type: 'command', id: 'c1', op: 'prompt', content: 'hello' });
    socket.disconnect();
    await expect(pending).resolves.toMatchObject({ ok: false, source: 'client', code: 'transport_error' });
    await until(() => client.sockets.length === 2);
    const reconnected = client.sockets[1]!;
    reconnected.open();
    reconnected.message(server.snapshot(decodeAuthFrame(reconnected.sent[0]).resume));
    expect(reconnected.frames).toEqual([]);
    const cancel = client.client.command({ type: 'command', id: 'c2', op: 'cancel' });
    await until(() => reconnected.commands.length === 1);
    reconnected.result('c2');
    await expect(cancel).resolves.toEqual({ ok: true });
  });
});

function workspace(overrides: Partial<ConstructorParameters<typeof ChatSession>[0]> = {}, connection = transport()) {
  const views: ChatView[] = [];
  const queries = createChatQueries({ list: async () => [chat], detail: async chatId => historyState(chatId) });
  const session = new ChatSession({ create: async () => chat, ...overrides },
    queries, 'fixture-token', view => views.push(view), connection);
  cleanup.push(() => queries.dispose());
  cleanup.push(() => session.dispose());
  return { ...connection, session, queries, views };
}

async function openWorkspace(server = producer(), client = workspace()) {
  const ready = client.session.select(chat);
  await until(() => client.sockets.length === 1);
  client.sockets[0].open();
  client.sockets[0].message(server.snapshot());
  expect(await ready).toBe(true);
  return { client, server };
}

describe('chat commands and subscription lifecycle', () => {
  test('reads HTTP history first but blocks sends until a valid WS snapshot confirms readiness', async () => {
    const server = producer();
    const client = workspace();
    const ready = client.session.select(chat);
    await until(() => client.sockets.length === 1);
    expect(entryText(client.session.view.state ?? undefined)).toBe('HTTP history');
    expect(client.session.view.historyReady).toBe(false);
    expect(await client.session.send('too soon')).toBe(false);
    client.sockets[0].open();
    client.sockets[0].message(server.snapshot());
    expect(await ready).toBe(true);
    expect(client.session.view.historyReady).toBe(true);
    expect(client.queries.client.getQueryData(client.queries.history(chat.id).queryKey)).toMatchObject({
      entries: [{ blocks: [{ type: 'text', text: 'partial' }] }],
    });
  });

  test('permanent sync rejection ends navigation readiness and leaves idle UI available for token settings', async () => {
    const client = workspace();
    const ready = client.session.select(chat);
    await until(() => client.sockets.length === 1);
    client.sockets[0].open();
    client.sockets[0].disconnect(4401);
    expect(await ready).toBe(false);
    expect(client.session.view.phase).toBe('idle');
    expect(client.session.view.historyReady).toBe(false);
    expect(client.session.view.error).toContain('访问令牌');
    expect(await client.session.send('not authorized')).toBe(false);
  });

  test('control-frame acknowledgement is not completion, duplicate sends are blocked, server terminal state wins', async () => {
    const { client, server } = await openWorkspace();
    expect(await client.session.send('hello')).toBe(true);
    expect(client.sockets[0]!.commands).toEqual([{ type: 'command', id: expect.any(String), op: 'prompt', content: 'hello' }]);
    expect(client.session.view.phase).toBe('streaming');
    expect(await client.session.send('duplicate')).toBe(false);
    client.sockets[0].message(server.update(() => {
      server.entry.set('status', 'streaming'); server.state({ running: true });
    }));
    expect(client.session.view.phase).toBe('streaming');
    client.sockets[0].message(server.update(() => {
      server.text.insert(server.text.length, ' final');
      server.entry.set('status', 'completed'); server.state({ running: false });
    }));
    expect(client.session.view.phase).toBe('idle');
    expect(client.session.view.state?.entries[0]).toMatchObject({ status: 'completed', blocks: [{ type: 'text', text: 'partial final' }] });
    expect(promptContents(client.sockets)).toEqual(['hello']);
  });

  test('disconnect only restores subscription; it neither cancels execution nor replays the prompt', async () => {
    const { client, server } = await openWorkspace();
    await client.session.send('hello');
    client.sockets[0].message(server.update(() => server.state({ running: true })));
    client.sockets[0].disconnect();
    expect(client.session.view.historyReady).toBe(false);
    expect(await client.session.send('duplicate')).toBe(false);
    server.update(() => { server.text.insert(server.text.length, ' final'); server.state({ running: false }); });
    await until(() => client.sockets.length === 2);
    const socket = client.sockets[1];
    socket.open();
    socket.message(server.snapshot(decodeAuthFrame(socket.sent[0]).resume));
    expect(client.session.view.phase).toBe('idle');
    expect(client.session.view.historyReady).toBe(true);
    expect(promptContents(client.sockets)).toEqual(['hello']);
    expect(commandOps(client.sockets, 'cancel')).toEqual([]);
    expect(acknowledgements(socket)).toEqual([{ type: 'ack', delivery: 1 }]);
  });

  test('even an idle disconnected chat blocks new send until resynced', async () => {
    const { client, server } = await openWorkspace();
    client.sockets[0].disconnect();
    expect(await client.session.send('not synced')).toBe(false);
    await until(() => client.sockets.length === 2);
    const socket = client.sockets[1];
    socket.open();
    socket.message(server.snapshot(decodeAuthFrame(socket.sent[0]).resume));
    expect(await client.session.send('synced')).toBe(true);
    expect(promptContents(client.sockets)).toEqual(['synced']);
  });

  test('cancel waits for the server confirmation, refused stop blocks sends and can only be retried explicitly', async () => {
    const client = workspace();
    const { server } = await openWorkspace(producer(), client);
    const socket = client.sockets[0];
    socket.autoAck = false;
    socket.message(server.update(() => server.state({ running: true })));
    const cancels = () => commandOps(client.sockets, 'cancel');
    let settled = false;
    const stop = client.session.stop().then(result => { settled = true; return result; });
    await until(() => cancels().length === 1);
    expect(settled).toBe(false);
    expect(client.session.view.phase).toBe('cancelling');
    // 停止确认中：导航被阻断，且不重复下发取消。
    expect(await client.session.select({ ...chat, id: 'other' })).toBe(false);
    expect(await client.session.stop()).toBe(false);
    expect(cancels()).toHaveLength(1);
    socket.result(cancels()[0]!.id, { ok: false, error: { code: 'blocked', message: 'host not confirmed' } });
    expect(await stop).toBe(false);
    expect(client.session.view.phase).toBe('blocked');
    expect(client.session.view.error).toContain('尚未确认任务停止');
    expect(await client.session.send('unsafe')).toBe(false);
    // 只能显式重试：再次点击停止才发出新的取消命令，确认后解除阻断。
    const retry = client.session.stop();
    await until(() => cancels().length === 2);
    socket.result(cancels()[1]!.id);
    expect(await retry).toBe(true);
    // 阻断解除后相位由服务端状态决定（此处仍报执行中），错误提示清空。
    expect(client.session.view.phase).toBe('streaming');
    expect(client.session.view.error).toBe('');
    expect(promptContents(client.sockets)).toEqual([]);
    socket.message(server.update(() => server.state({ running: false })));
    expect(client.session.view.phase).toBe('idle');
    socket.autoAck = true;
    expect(await client.session.send('unsafe')).toBe(true);
    expect(promptContents(client.sockets)).toEqual(['unsafe']);
    expect(client.sockets).toHaveLength(1);
  });

  test('waits for prompt acknowledgement before issuing cancel and retains cancelling until confirmed', async () => {
    const client = workspace();
    const { server } = await openWorkspace(producer(), client);
    const socket = client.sockets[0];
    socket.autoAck = false;
    const send = client.session.send('hello');
    const stop = client.session.stop();
    await until(() => socket.commands.length === 1);
    // prompt 未确认前不发取消，避免取消先于命令到达执行宿主。
    expect(socket.commands.map(command => command.op)).toEqual(['prompt']);
    expect(client.session.view.phase).toBe('cancelling');
    socket.result(socket.commands[0]!.id);
    expect(await send).toBe(true);
    await until(() => socket.commands.length === 2);
    expect(socket.commands.map(command => command.op)).toEqual(['prompt', 'cancel']);
    expect(client.session.view.phase).toBe('cancelling');
    socket.result(socket.commands[1]!.id);
    expect(await stop).toBe(true);
    expect(client.session.view.phase).toBe('idle');
  });

  test('unconfirmed prompt stays blocked, is never replayed on reconnect, and clears only via confirmed stop', async () => {
    const client = workspace();
    const { server } = await openWorkspace(producer(), client);
    const socket = client.sockets[0]!;
    socket.autoAck = false;
    const send = client.session.send('hello');
    await until(() => socket.commands.length === 1);
    socket.disconnect();
    expect(await send).toBe(false);
    expect(client.session.view.phase).toBe('blocked');
    await until(() => client.sockets.length === 2);
    const reconnected = client.sockets[1]!;
    reconnected.open();
    reconnected.message(server.snapshot(decodeAuthFrame(reconnected.sent[0]).resume));
    expect(client.session.view.phase).toBe('blocked');
    expect(await client.session.send('duplicate')).toBe(false);
    expect(reconnected.commands).toEqual([]);
    expect(await client.session.stop()).toBe(true);
    expect(client.session.view.phase).toBe('idle');
  });

  test('server-refused prompt reports the reason without marking execution uncertain', async () => {
    const client = workspace();
    const { server } = await openWorkspace(producer(), client);
    const socket = client.sockets[0]!;
    socket.autoAck = false;
    const send = client.session.send('hello');
    await until(() => socket.commands.length === 1);
    socket.result(socket.commands[0]!.id, { ok: false, error: { code: 'busy', message: 'turn running' } });
    expect(await send).toBe(false);
    expect(client.session.view.error).toContain('会话正在执行中');
    expect(client.session.view.phase).toBe('idle');
    socket.message(server.update(() => server.state({ running: true })));
    expect(client.session.view.phase).toBe('streaming');
    expect(await client.session.send('again')).toBe(false);
    expect(promptContents(client.sockets)).toEqual(['hello']);
  });

  test('interaction answers travel as control frames with explicit settlement and auth reasons', async () => {
    const client = workspace();
    await openWorkspace(producer(), client);
    const socket = client.sockets[0]!;
    socket.autoAck = false;
    const settled = client.session.answerInteraction('interaction:1', { kind: 'permission', decision: 'allow_once' });
    await until(() => socket.commands.length === 1);
    expect(socket.commands[0]).toEqual({ type: 'command', id: expect.any(String), op: 'interaction',
      interactionId: 'interaction:1', answer: { kind: 'permission', decision: 'allow_once' } });
    socket.result(socket.commands[0]!.id, { ok: false, error: { code: 'settled', message: 'already settled' } });
    await expect(settled).rejects.toThrow('这个交互已经结算或失效，请刷新后重试。');
    const unauthorized = client.session.answerInteraction('interaction:2', { kind: 'permission', decision: 'reject_once' });
    await until(() => socket.commands.length === 2);
    socket.result(socket.commands[1]!.id, { ok: false, error: { code: 'unauthorized', message: 'bad token' } });
    await expect(unauthorized).rejects.toThrow('身份验证失败，请在设置中检查访问令牌。');
    const accepted = client.session.answerInteraction('interaction:3', { kind: 'elicitation', decision: { action: 'decline' } });
    await until(() => socket.commands.length === 3);
    socket.result(socket.commands[2]!.id);
    await expect(accepted).resolves.toBeUndefined();
  });

  test('payload full-text reads use the same connection command channel', async () => {
    const client = workspace();
    await openWorkspace(producer(), client);
    const socket = client.sockets[0]!;
    socket.autoAck = false;
    const read = client.session.readPayload('gen:1');
    await until(() => socket.commands.length === 1);
    expect(socket.commands[0]).toEqual({ type: 'command', id: expect.any(String), op: 'payload', refId: 'gen:1' });
    const payload = { id: 'gen:1', bytes: 8, json: '{"a":1}' };
    socket.result(socket.commands[0]!.id, { payload });
    await expect(read).resolves.toEqual({ ok: true, payload });
  });

  test('accepts server running/blocked state from navigation rather than assuming idle', async () => {
    const server = producer();
    server.state({ running: true, executionBlocked: true });
    const { client } = await openWorkspace(server);
    expect(client.session.view.phase).toBe('blocked');
    expect(await client.session.send('unsafe')).toBe(false);
  });

  test('navigation retires old replica identity and ignores late events from the previous chat', async () => {
    const { client } = await openWorkspace();
    const old = client.sockets[0];
    const late = old.onmessage!;
    const otherChat = { ...chat, id: 'other' };
    const other = producer(otherChat, 'other answer');
    const ready = client.session.select(otherChat);
    await until(() => client.sockets.length === 2);
    client.sockets[1].open();
    client.sockets[1].message(other.snapshot());
    expect(await ready).toBe(true);
    late.call(old as unknown as WebSocket, { data: encodeSyncFrame({ ...producer().snapshot(), delivery: 1 }) } as MessageEvent);
    expect(client.session.view.selected?.id).toBe('other');
    expect(entryText(client.session.view.state ?? undefined)).toBe('other answer');
    expect(old.closed).toBe(1);
    expect(decodeAuthFrame(client.sockets[1].sent[0]).resume).toBeUndefined();
  });

  test('superseded HTTP navigation cannot install a socket or publish stale history', async () => {
    const slow = deferred<SyncState>();
    let historyStarted = false;
    const connection = transport();
    const queries = createChatQueries({ list: async () => [], detail: id => {
      if (id === chat.id) { historyStarted = true; return slow.promise; }
      return Promise.resolve(historyState(id, ''));
    } });
    const session = new ChatSession({ create: async () => chat }, queries, 'fixture-token', () => {}, connection);
    cleanup.push(() => queries.dispose());
    cleanup.push(() => session.dispose());
    const old = session.select(chat);
    await until(() => historyStarted);
    const target = { ...chat, id: 'target' };
    const fresh = session.select(target);
    await until(() => connection.sockets.length === 1);
    expect(connection.sockets[0].url).toContain('/target/sync');
    connection.sockets[0].open();
    connection.sockets[0].message(producer(target).snapshot());
    expect(await fresh).toBe(true);
    expect(await old).toBe(false);
    slow.resolve(historyState(chat.id, ''));
    await Promise.resolve();
    expect(session.view.selected?.id).toBe('target');
    expect(connection.sockets).toHaveLength(1);
  });

  test('initial new-chat send waits for HTTP history and snapshot before issuing exactly one command', async () => {
    const client = workspace();
    const server = producer();
    const send = client.session.send('first prompt');
    await until(() => client.sockets.length === 1);
    expect(client.sockets[0].commands).toEqual([]);
    expect(await client.session.send('duplicate')).toBe(false);
    client.sockets[0].open();
    client.sockets[0].message(server.snapshot());
    expect(await send).toBe(true);
    expect(promptContents(client.sockets)).toEqual(['first prompt']);
    expect(client.sockets[0].commands).toHaveLength(1);
  });

  test('initial send also honors server running state received while creating a chat', async () => {
    const client = workspace();
    const server = producer();
    server.state({ running: true });
    const send = client.session.send('first prompt');
    await until(() => client.sockets.length === 1);
    client.sockets[0].open();
    client.sockets[0].message(server.snapshot());
    expect(await send).toBe(false);
    expect(client.session.view.phase).toBe('streaming');
    expect(client.sockets[0].commands).toEqual([]);
  });

  test('workspace disposal closes sync and clears private caches without cancelling the server generation', async () => {
    const { client, server } = await openWorkspace();
    client.sockets[0].message(server.update(() => server.state({ running: true })));
    const published = client.views.length;
    client.session.dispose();
    await client.queries.dispose();
    expect(client.sockets[0].closed).toBe(1);
    expect(commandOps(client.sockets, 'cancel')).toEqual([]);
    expect(client.queries.client.getQueryCache().getAll()).toHaveLength(0);
    client.sockets[0].message(server.snapshot());
    expect(client.views).toHaveLength(published);
  });
});
