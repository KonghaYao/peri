import { describe, expect, test } from 'bun:test';
import { Y, encodeSyncFrame, type DocSnapshot } from '@peri-code/sdk/view';
import type { ChatSummary } from './contracts';
import { ChatController } from './controller';
import { RemoteActionError, RemoteSendReceipt, type ActionReceipt, type RemoteActions } from './actions';
import type { WebSocketLike } from './sync';

class FakeSocket implements WebSocketLike {
  binaryType = '';
  readyState = 0;
  bufferedAmount = 0;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onerror: ((event: unknown) => void) | null = null;
  onclose: ((event: { code: number; reason: string }) => void) | null = null;
  readonly sent: Uint8Array[] = [];

  send(data: Uint8Array): void { this.sent.push(data); }
  close(code?: number): void {
    if (this.readyState === 3) return;
    this.readyState = 3;
    this.onclose?.({ code: code ?? 1000, reason: '' });
  }
  open(): void { this.readyState = 1; this.onopen?.(); }
  message(data: unknown): void { this.onmessage?.({ data }); }
}

function summary(sessionId: string, title = sessionId): ChatSummary {
  return {
    sessionId, title, cwd: '/workspace', messageCount: 1,
    createdAt: '2026-10-20T00:00:00Z', updatedAt: '2026-10-20T01:00:00Z',
  };
}

function snapshotFrame(options: {
  sessionId: string; bindingRevision?: number; running?: boolean; schema?: number;
}): Uint8Array {
  const chatDoc = new Y.Doc();
  const sessionDoc = new Y.Doc();
  chatDoc.getMap('root').set('schemaVersion', 3);
  chatDoc.getMap('root').set('entries', new Y.Map());
  chatDoc.getMap('root').set('entryOrder', new Y.Array());
  const root = sessionDoc.getMap('root');
  root.set('schemaVersion', options.schema ?? 2);
  const info = new Y.Map();
  info.set('sessionId', options.sessionId);
  info.set('bindingRevision', options.bindingRevision ?? 1);
  info.set('running', options.running ?? false);
  root.set('session', info);
  const snapshot: DocSnapshot = {
    protocol: 2, generation: 'g1', sequence: 0, mode: 'snapshot',
    chat: Y.encodeStateAsUpdateV2(chatDoc), session: Y.encodeStateAsUpdateV2(sessionDoc),
  };
  return encodeSyncFrame({ type: 'snapshot', delivery: 1, snapshot });
}

async function waitFor(condition: () => boolean, timeoutMs = 500): Promise<void> {
  const start = Date.now();
  while (!condition()) {
    if (Date.now() - start > timeoutMs) throw new Error('condition not met');
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

function receipt(partial: Partial<ActionReceipt> & { sessionId: string }): ActionReceipt {
  return {
    requestId: 'request-1',
    chatId: 'handle-1',
    bindingRevision: 1,
    raw: {
      ok: true, requestId: 'request-1', chatId: 'handle-1',
      sessionId: partial.sessionId, bindingRevision: 1,
    },
    ...partial,
  };
}

interface HarnessOptions {
  changeSession?: (sessionId: string) => Promise<ActionReceipt>;
  createSession?: () => Promise<ActionReceipt>;
  send?: () => Promise<ActionReceipt>;
  cancel?: () => Promise<ActionReceipt>;
}

function harness(options: HarnessOptions = {}) {
  const log: string[] = [];
  const sockets: FakeSocket[] = [];
  const sentTargets: unknown[] = [];
  const actions: RemoteActions = {
    async createHandle() {
      const id = `handle-${log.filter(entry => entry === 'createHandle').length + 1}`;
      log.push('createHandle', id);
      return { id, sessionId: null, bindingRevision: 0 };
    },
    async execute(_address, action, target) {
      log.push(`action:${action.type}`);
      sentTargets.push(target);
      if (action.type === 'change_session') {
        return options.changeSession?.(action.payload.sessionId)
          ?? receipt({ requestId: action.requestId, sessionId: action.payload.sessionId });
      }
      if (action.type === 'create_new_session') {
        return options.createSession?.()
          ?? receipt({ requestId: action.requestId, sessionId: 'created-session' });
      }
      if (action.type === 'send') {
        return options.send?.()
          ?? { ...receipt({ requestId: action.requestId, sessionId: 'session-1' }), inputId: 'input-1' };
      }
      if (action.type === 'cancel') {
        return options.cancel?.()
          ?? { ...receipt({ requestId: action.requestId, sessionId: 'session-1' }), status: 'requested' };
      }
      return receipt({ requestId: action.requestId, sessionId: action.type === 'answer' ? 'session-1' : 'session-1' });
    },
    async close(address) { log.push(`close:${address}`); },
    async readPayload() { return { id: 'ref-1', bytes: 2, json: '{}' }; },
  };
  const controller = new ChatController({
    actions,
    list: async () => [summary('session-1'), summary('session-2')],
    syncUrl: (address) => { log.push(`sync:${address}`); return `ws://example.test/${address}`; },
    webSocketFactory: (url) => {
      const socket = new FakeSocket();
      sockets.push(socket);
      log.push(`socket:${url}`);
      return socket;
    },
  });
  return { controller, log, sockets, actions, sentTargets };
}

async function openWithSnapshot(
  controller: ChatController,
  target: ChatSummary | null,
  sockets: FakeSocket[],
  options: { sessionId: string; bindingRevision?: number; running?: boolean; schema?: number },
): Promise<boolean> {
  const opening = controller.openChat(target);
  await waitFor(() => sockets.some(candidate => candidate.readyState === 0));
  const socket = sockets.filter(candidate => candidate.readyState === 0).at(-1)!;
  socket.open();
  socket.message(snapshotFrame(options));
  return await opening;
}

describe('ChatController history open', () => {
  test('creates a handle, sends change_session, then connects WS', async () => {
    const { controller, log, sockets } = harness();
    expect(await openWithSnapshot(controller, summary('session-1'), sockets, { sessionId: 'session-1' })).toBe(true);
    expect(log.slice(0, 5)).toEqual([
      'createHandle', 'handle-1', 'action:change_session', 'sync:handle-1', 'socket:ws://example.test/handle-1',
    ]);
    expect(controller.getState()).toMatchObject({
      selected: { id: 'session-1' }, loadState: 'ready', liveSync: 'synced', historyReady: true,
    });
    await controller.dispose();
  });

  test('empty handle creates a new session before connecting', async () => {
    const { controller, log, sockets } = harness();
    expect(await openWithSnapshot(controller, null, sockets, { sessionId: 'created-session' })).toBe(true);
    expect(log.slice(0, 4)).toEqual([
      'createHandle', 'handle-1', 'action:create_new_session', 'sync:handle-1',
    ]);
    expect(controller.getState().selected?.id).toBe('created-session');
    await controller.dispose();
  });

  test('binding conflict falls back to the Session-addressed existing handle', async () => {
    const { controller, log, sockets } = harness({
      changeSession: async () => { throw new RemoteActionError('binding_conflict', 'owned', 'rejected', { status: 409 }); },
    });
    expect(await openWithSnapshot(controller, summary('session-1'), sockets, { sessionId: 'session-1' })).toBe(true);
    expect(log).toContain('close:handle-1');
    expect(log).toContain('sync:session-1');
    expect(controller.getState().selected?.id).toBe('session-1');
    await controller.dispose();
  });

  test('a late history open cannot replace the newer selection', async () => {
    let finishChange!: () => void;
    const blocked = new Promise<void>((resolve) => { finishChange = resolve; });
    const { controller, sockets, log } = harness({
      changeSession: async (sessionId) => {
        if (sessionId === 'session-1') await blocked;
        return receipt({ sessionId });
      },
    });
    const first = controller.openChat(summary('session-1'));
    await waitFor(() => log.includes('action:change_session'));
    const second = controller.openChat(summary('session-2'));
    await waitFor(() => sockets.length > 0);
    sockets.at(-1)!.open();
    sockets.at(-1)!.message(snapshotFrame({ sessionId: 'session-2' }));
    expect(await second).toBe(true);
    finishChange();
    expect(await first).toBe(false);
    expect(controller.getState().selected?.id).toBe('session-2');
    await controller.dispose();
  });
});

describe('ChatController A/B isolation', () => {
  test('opening B releases the local connection without closing A or cancelling it', async () => {
    const { controller, log, sockets } = harness();
    await openWithSnapshot(controller, summary('session-1'), sockets, { sessionId: 'session-1', running: true });
    const socketA = sockets.at(-1)!;
    await openWithSnapshot(controller, summary('session-2'), sockets, { sessionId: 'session-2' });
    expect(socketA.readyState).toBe(3);
    expect(log.some(entry => entry === 'close:handle-1')).toBe(false);

    // 回到 A：复用已登记句柄，不再 create/change，只重连同步。
    const before = log.filter(entry => entry === 'createHandle').length;
    await openWithSnapshot(controller, summary('session-1'), sockets, { sessionId: 'session-1', running: true });
    expect(log.filter(entry => entry === 'createHandle').length).toBe(before);
    expect(log.filter(entry => entry.startsWith('sync:')).at(-1)).toBe('sync:handle-1');
    await controller.dispose();
  });
});

describe('ChatController send/cancel/unsupported', () => {
  test('send targets the replica binding revision and resolves delivery from inputQueue', async () => {
    const { controller, sockets, sentTargets } = harness();
    await openWithSnapshot(controller, summary('session-1'), sockets, {
      sessionId: 'session-1', bindingRevision: 7,
    });
    const sent = await controller.dispatchSend('  你好  ');
    expect(sent?.sessionId).toBe('session-1');
    expect(sentTargets.at(-1)).toEqual({ sessionId: 'session-1', bindingRevision: 7 });
    expect(sent?.receipt).toBeInstanceOf(RemoteSendReceipt);
    expect(sent?.receipt.target).toEqual({
      chatId: 'handle-1', sessionId: 'session-1', bindingRevision: 1,
    });
    expect(sent?.receipt.isSent).toBe(false);
    // 副本队列投递事实结算回执。
    const chatDoc = new Y.Doc();
    const sessionDoc = new Y.Doc();
    chatDoc.getMap('root').set('schemaVersion', 3);
    const root = sessionDoc.getMap('root');
    root.set('schemaVersion', 2);
    const info = new Y.Map();
    info.set('sessionId', 'session-1');
    info.set('bindingRevision', 7);
    info.set('running', true);
    info.set('inputQueue', {
      generation: 'g', revision: 1, items: [{ inputId: 'input-1', state: 'delivered' }],
    });
    root.set('session', info);
    const snapshot: DocSnapshot = {
      protocol: 2, generation: 'g1', sequence: 1, mode: 'snapshot',
      chat: Y.encodeStateAsUpdateV2(chatDoc), session: Y.encodeStateAsUpdateV2(sessionDoc),
    };
    sockets.at(-1)!.message(encodeSyncFrame({ type: 'snapshot', delivery: 2, snapshot }));
    await sent!.receipt;
    expect(sent?.receipt.isSent).toBe(true);
    await controller.dispose();
  });

  test('cancel is requested, not reported as stopped; running stays from replica', async () => {
    const { controller, sockets } = harness();
    await openWithSnapshot(controller, summary('session-1'), sockets, { sessionId: 'session-1', running: true });
    expect(await controller.stop()).toBe(true);
    expect(controller.getState().phase).toBe('cancelling');
    expect(controller.getState().state?.running).toBe(true);
    await controller.dispose();
  });

  test('unsupported schema is an explicit state, never an empty ready history', async () => {
    const { controller, sockets } = harness();
    expect(await openWithSnapshot(controller, summary('session-1'), sockets, {
      sessionId: 'session-1', schema: 99,
    })).toBe(true);
    expect(controller.getState()).toMatchObject({
      loadState: 'unsupported', historyReady: false, phase: 'idle',
    });
    expect(controller.getState().state?.entries).toEqual([]);
    expect(controller.getState().error).toContain('不受支持');
    await controller.dispose();
  });
});
