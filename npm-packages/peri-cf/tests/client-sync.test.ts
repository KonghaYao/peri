import { afterEach, describe, expect, spyOn, test } from 'bun:test';
import { Y, SessionDocSync, type DocStateVector } from '@peri-code/sdk/view';
import { decodeAuthFrame, decodeAckFrame, encodeSyncFrame, MAX_SYNC_FRAME_CHARS, type SyncState } from '../shared/sync';
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
  sent: string[] = [];
  closed = 0;

  constructor(readonly url: string) {}
  send(data: string) { this.sent.push(data); }
  close() { this.closed++; }
  open() { this.onopen?.call(this as unknown as WebSocket, new Event('open')); }
  message(data: unknown) {
    this.onmessage?.call(this as unknown as WebSocket, { data } as MessageEvent);
  }
  disconnect(code = 1006) {
    this.onclose?.call(this as unknown as WebSocket, { code } as CloseEvent);
  }
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
  metadata.set('state', { chat: target, running: false, executionBlocked: false,
    entryMetadata: { entry: { id: 'answer', createdAt: target.updatedAt } } });
  const sync = new SessionDocSync(chatDoc, sessionDoc, { flushIntervalMs: 100_000 });
  const updates: Parameters<typeof sync.subscribe>[0] extends (update: infer Update) => unknown ? Update[] : never = [];
  const subscription = sync.subscribe(update => { updates.push(update); });
  cleanup.push(async () => { subscription.unsubscribe(); await sync.close(); chatDoc.destroy(); sessionDoc.destroy(); });
  return {
    chatDoc, sessionDoc, entry, text,
    snapshot(resume?: DocStateVector) {
      const peer = sync.subscribe(() => {}, { resume });
      peer.unsubscribe();
      return encodeSyncFrame({ type: 'snapshot', snapshot: peer.snapshot });
    },
    update(change: () => void) {
      change();
      sync.flush();
      return encodeSyncFrame({ type: 'update', update: updates.at(-1)! });
    },
    state(change: Record<string, unknown>) {
      metadata.set('state', { ...metadata.get('state') as object, ...change });
    },
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
    socket.message(JSON.stringify({ ...JSON.parse(server.snapshot()), delivery: 1 }));
    expect(decodeAckFrame(socket.sent[1])).toEqual({ type: 'ack', delivery: 1 });
    socket.message(JSON.stringify({ ...JSON.parse(server.update(() => server.text.insert(server.text.length, '!'))), delivery: 2 }));
    expect(client.states.at(-1)?.messages[0].content).toBe('partial!');
    expect(decodeAckFrame(socket.sent[2])).toEqual({ type: 'ack', delivery: 2 });
    socket.message(JSON.stringify({ ...JSON.parse(server.update(() => server.state({ running: true }))), delivery: 4 }));
    expect(socket.sent).toHaveLength(3);
    expect(client.statuses.at(-1)).toBe('failed');
  });
  test('authenticates in the first frame, never in URL or subprotocol, and reads projected state', () => {
    const server = producer();
    const client = replica();
    const socket = client.sockets[0];
    expect(socket.url).toBe('wss://example.test/api/chats/chat-1/sync');
    expect(socket.sent).toEqual([]);
    socket.open();
    expect(decodeAuthFrame(socket.sent[0])).toEqual({ type: 'auth', token: 'fixture-token' });
    socket.message(server.snapshot());
    expect(client.statuses.at(-1)).toBe('synced');
    expect(client.states.at(-1)?.messages[0]).toEqual({ id: 'answer', role: 'assistant', content: 'partial',
      createdAt: chat.updatedAt, status: 'completed' });
    expect(socket.sent).toHaveLength(1);
  });

  test('observes chat-only text updates and session-only running/blocked/terminal changes without writing back', () => {
    const server = producer();
    const client = replica();
    const socket = client.sockets[0];
    socket.open();
    socket.message(server.snapshot());
    socket.message(server.update(() => server.text.insert(server.text.length, ' answer')));
    expect(client.states.at(-1)?.messages[0].content).toBe('partial answer');
    socket.message(server.update(() => server.state({ running: true })));
    expect(client.states.at(-1)?.running).toBe(true);
    socket.message(server.update(() => server.state({ running: false, executionBlocked: true, error: 'stop failed' })));
    expect(client.states.at(-1)).toMatchObject({ running: false, executionBlocked: true, error: 'stop failed' });
    expect(socket.sent).toHaveLength(1);
  });

  test('replaces both documents across generations and rebinds their observers', () => {
    const first = producer(chat, 'old');
    const second = producer(chat, 'replacement');
    const client = replica();
    const socket = client.sockets[0];
    socket.open();
    socket.message(first.snapshot());
    socket.message(second.snapshot());
    expect(client.states.at(-1)?.messages[0].content).toBe('replacement');
    socket.message(second.update(() => second.text.insert(second.text.length, '!')));
    expect(client.states.at(-1)?.messages[0].content).toBe('replacement!');
    socket.message(second.update(() => second.state({ running: true })));
    expect(client.states.at(-1)?.running).toBe(true);
    expect(socket.sent).toHaveLength(1);
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
    expect(JSON.parse(snapshot).snapshot.mode).toBe('delta');
    second.message(snapshot);
    expect(client.states.at(-1)?.messages[0].content).toBe('partial missed');
    expect(client.statuses.at(-1)).toBe('synced');
    expect(first.closed).toBe(1);
    expect(second.sent).toHaveLength(1);
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
    expect(client.states.at(-1)?.messages[0].content).toBe('partial');
    expect(client.statuses.at(-1)).toBe('disconnected');
    await until(() => client.sockets.length === 2);
    const second = client.sockets[1];
    second.open();
    second.message(server.snapshot(decodeAuthFrame(second.sent[0]).resume));
    const published = client.states.length;
    late.call(first as unknown as WebSocket, { data: gap } as MessageEvent);
    expect(client.states).toHaveLength(published);
    expect(client.states.at(-1)?.messages[0].content).toBe('partial skipped next');
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
    expect(JSON.parse(snapshot).snapshot.mode).toBe('snapshot');
    socket.message(snapshot);
    socket.message(restarted.update(() => restarted.text.insert(restarted.text.length, '!')));
    expect(client.states.at(-1)?.messages[0].content).toBe('new generation!');
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
    expect(client.states.at(-1)?.messages[0].content).toBe('partial!');
    expect(socket.sent).toHaveLength(1);
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

  test.each(['invalid-json', '{}', new Uint8Array([1]), ' '.repeat(MAX_SYNC_FRAME_CHARS + 1)])(
    'blocks invalid, binary or oversized frames', data => {
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
    late.call(socket as unknown as WebSocket, { data: server.snapshot() } as MessageEvent);
    await new Promise(resolve => setTimeout(resolve, 8));
    expect(client.sockets).toHaveLength(1);
    expect(client.states).toHaveLength(1);
    expect(socket.onopen).toBeNull();
    expect(socket.onmessage).toBeNull();
    expect(socket.onclose).toBeNull();
    expect(socket.onerror).toBeNull();
  });
});

function workspace(overrides: Partial<ConstructorParameters<typeof ChatSession>[0]> = {}) {
  const connection = transport();
  const commands: string[] = [];
  const stops: string[] = [];
  const views: ChatView[] = [];
  const queries = createChatQueries({ list: async () => [chat], detail: async chatId => ({
    chat: { ...chat, id: chatId }, messages: [{ id: 'history', role: 'assistant', content: 'HTTP history', createdAt: chat.updatedAt }],
  }) });
  const session = new ChatSession({
    create: async () => chat,
    send: async (_id, content) => { commands.push(content); },
    cancel: async chatId => { stops.push(chatId); },
    ...overrides,
  }, queries, 'fixture-token', view => views.push(view), connection);
  cleanup.push(() => queries.dispose());
  cleanup.push(() => session.dispose());
  return { ...connection, session, commands, stops, queries, views };
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
    expect(client.session.view.messages[0].content).toBe('HTTP history');
    expect(client.session.view.historyReady).toBe(false);
    expect(await client.session.send('too soon')).toBe(false);
    client.sockets[0].open();
    client.sockets[0].message(server.snapshot());
    expect(await ready).toBe(true);
    expect(client.session.view.historyReady).toBe(true);
    expect(client.queries.client.getQueryData(client.queries.history(chat.id).queryKey)).toMatchObject({
      messages: [{ content: 'partial' }],
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

  test('202 command acceptance is not completion, duplicate sends are blocked, server terminal state wins', async () => {
    const { client, server } = await openWorkspace();
    expect(await client.session.send('hello')).toBe(true);
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
    expect(client.session.view.messages[0]).toMatchObject({ content: 'partial final', status: 'completed' });
    expect(client.commands).toEqual(['hello']);
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
    expect(client.commands).toEqual(['hello']);
    expect(client.stops).toEqual([]);
    expect(socket.sent).toHaveLength(1);
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
    expect(client.commands).toEqual(['synced']);
  });

  test('cancel waits for actual server stop, failed stop blocks sends and can only be retried explicitly', async () => {
    const cancel = deferred<void>();
    let attempts = 0;
    const client = workspace({ cancel: () => { attempts++; return cancel.promise; } });
    const { server } = await openWorkspace(producer(), client);
    client.sockets[0].message(server.update(() => server.state({ running: true })));
    let settled = false;
    const stop = client.session.stop().then(result => { settled = true; return result; });
    await until(() => attempts === 1);
    expect(settled).toBe(false);
    expect(client.session.view.phase).toBe('cancelling');
    expect(await client.session.stop()).toBe(false);
    cancel.reject(new Error('stop unconfirmed'));
    expect(await stop).toBe(false);
    expect(client.session.view.phase).toBe('blocked');
    expect(await client.session.send('unsafe')).toBe(false);
    expect(await client.session.select({ ...chat, id: 'other' })).toBe(false);
    expect(attempts).toBe(2);
    expect(client.sockets).toHaveLength(1);
  });

  test('waits for pending command acknowledgement before exact-target cancel and retains cancelling until response', async () => {
    const command = deferred<void>();
    const cancel = deferred<void>();
    const events: string[] = [];
    const client = workspace({ send: () => { events.push('send'); return command.promise; },
      cancel: id => { events.push(`cancel:${id}`); return cancel.promise; } });
    await openWorkspace(producer(), client);
    const send = client.session.send('hello');
    const stop = client.session.stop();
    await Promise.resolve();
    expect(events).toEqual(['send']);
    command.resolve();
    await send;
    await until(() => events.length === 2);
    expect(events).toEqual(['send', `cancel:${chat.id}`]);
    expect(client.session.view.phase).toBe('cancelling');
    cancel.resolve();
    expect(await stop).toBe(true);
  });

  test('uncertain command failure stays blocked and is never retried on reconnect', async () => {
    let attempts = 0;
    const client = workspace({ send: async () => { attempts++; throw new Error('lost response'); } });
    const { server } = await openWorkspace(producer(), client);
    expect(await client.session.send('hello')).toBe(false);
    expect(client.session.view.phase).toBe('blocked');
    client.sockets[0].disconnect();
    await until(() => client.sockets.length === 2);
    client.sockets[1].open();
    client.sockets[1].message(server.snapshot(decodeAuthFrame(client.sockets[1].sent[0]).resume));
    expect(client.session.view.phase).toBe('blocked');
    expect(await client.session.send('duplicate')).toBe(false);
    expect(attempts).toBe(1);
    expect(await client.session.stop()).toBe(true);
    expect(client.session.view.phase).toBe('idle');
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
    late.call(old as unknown as WebSocket, { data: producer().snapshot() } as MessageEvent);
    expect(client.session.view.selected?.id).toBe('other');
    expect(client.session.view.messages[0].content).toBe('other answer');
    expect(old.closed).toBe(1);
    expect(decodeAuthFrame(client.sockets[1].sent[0]).resume).toBeUndefined();
  });

  test('superseded HTTP navigation cannot install a socket or publish stale history', async () => {
    const slow = deferred<{ chat: Chat; messages: [] }>();
    let historyStarted = false;
    const connection = transport();
    const queries = createChatQueries({ list: async () => [], detail: id => {
      if (id === chat.id) { historyStarted = true; return slow.promise; }
      return Promise.resolve({ chat: { ...chat, id }, messages: [] });
    } });
    const session = new ChatSession({ create: async () => chat, send: async () => {}, cancel: async () => {} },
      queries, 'fixture-token', () => {}, connection);
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
    slow.resolve({ chat, messages: [] });
    await Promise.resolve();
    expect(session.view.selected?.id).toBe('target');
    expect(connection.sockets).toHaveLength(1);
  });

  test('initial new-chat send waits for HTTP history and snapshot before issuing exactly one command', async () => {
    const client = workspace();
    const server = producer();
    const send = client.session.send('first prompt');
    await until(() => client.sockets.length === 1);
    expect(client.commands).toEqual([]);
    expect(await client.session.send('duplicate')).toBe(false);
    client.sockets[0].open();
    client.sockets[0].message(server.snapshot());
    expect(await send).toBe(true);
    expect(client.commands).toEqual(['first prompt']);
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
    expect(client.commands).toEqual([]);
  });

  test('workspace disposal closes sync and clears private caches without cancelling the server generation', async () => {
    const { client, server } = await openWorkspace();
    client.sockets[0].message(server.update(() => server.state({ running: true })));
    const published = client.views.length;
    client.session.dispose();
    await client.queries.dispose();
    expect(client.sockets[0].closed).toBe(1);
    expect(client.stops).toEqual([]);
    expect(client.queries.client.getQueryCache().getAll()).toHaveLength(0);
    client.sockets[0].message(server.snapshot());
    expect(client.views).toHaveLength(published);
  });
});
