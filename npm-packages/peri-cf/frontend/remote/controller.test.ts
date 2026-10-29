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
  title?: string; executionBlocked?: boolean;
  activeTurn?: { turnId: string | null; status: string | null };
  inputQueue?: { generation?: string; revision?: number; items: { inputId: string; state: string }[] };
  userEntryIds?: string[];
  sequence?: number;
}): Uint8Array {
  const chatDoc = new Y.Doc();
  const sessionDoc = new Y.Doc();
  const chatRoot = chatDoc.getMap('root');
  chatRoot.set('schemaVersion', 3);
  const entries = new Y.Map();
  const entryOrder = new Y.Array();
  chatRoot.set('entries', entries);
  chatRoot.set('entryOrder', entryOrder);
  for (const entryId of options.userEntryIds ?? []) {
    const entry = new Y.Map();
    entry.set('entryId', entryId);
    entry.set('id', entryId);
    entry.set('kind', 'message');
    entry.set('turnId', entryId.split(':').slice(0, 2).join(':'));
    entry.set('role', 'user');
    entry.set('status', 'completed');
    entry.set('createdAt', '2026-10-20T00:00:00Z');
    entry.set('createdAtSource', 'observed');
    entry.set('completedAt', null);
    entry.set('error', null);
    entry.set('blocks', new Y.Map());
    entry.set('blockOrder', new Y.Array());
    entries.set(entryId, entry);
    entryOrder.push([entryId]);
  }
  const root = sessionDoc.getMap('root');
  root.set('schemaVersion', options.schema ?? 2);
  const info = new Y.Map();
  info.set('sessionId', options.sessionId);
  info.set('bindingRevision', options.bindingRevision ?? 1);
  info.set('running', options.running ?? false);
  info.set('executionBlocked', options.executionBlocked ?? false);
  if (options.title) info.set('title', options.title);
  if (options.activeTurn) {
    info.set('activeTurnId', options.activeTurn.turnId);
    info.set('activeTurnStatus', options.activeTurn.status);
  }
  if (options.inputQueue) {
    info.set('inputQueue', {
      generation: options.inputQueue.generation ?? 'g',
      revision: options.inputQueue.revision ?? 1,
      items: options.inputQueue.items,
    });
  }
  root.set('session', info);
  const snapshot: DocSnapshot = {
    protocol: 2, generation: 'g1', sequence: options.sequence ?? 0, mode: 'snapshot',
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
  close?: (address: string) => Promise<void>;
  initialSyncTimeoutMs?: number;
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
    async close(address) {
      log.push(`close:${address}`);
      await options.close?.(address);
    },
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
    ...(options.initialSyncTimeoutMs ? { initialSyncTimeoutMs: options.initialSyncTimeoutMs } : {}),
  });
  return { controller, log, sockets, actions, sentTargets };
}

async function openWithSnapshot(
  controller: ChatController,
  target: ChatSummary | null,
  sockets: FakeSocket[],
  options: Parameters<typeof snapshotFrame>[0],
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

  test('new session creation does not persist a placeholder title', async () => {
    const { controller, sockets, actions } = harness();
    const createPayloads: unknown[] = [];
    const execute = actions.execute;
    actions.execute = async (...args) => {
      const action = args[1];
      if (action.type === 'create_new_session') createPayloads.push(action.payload);
      return execute(...args);
    };

    expect(await openWithSnapshot(controller, null, sockets, { sessionId: 'created-session' })).toBe(true);
    // 标题由 Store 在首次 append_history 时从首条用户消息补齐；创建不得写入占位标题。
    expect(createPayloads).toEqual([{}]);
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
    expect(log).toContain('close:handle-1');
    expect(controller.getState().selected?.id).toBe('session-2');
    await controller.dispose();
  });

  test('an initial sync timeout reports failed without closing the server handle', async () => {
    const { controller, sockets, log } = harness({ initialSyncTimeoutMs: 5 });
    const opening = controller.openChat(summary('session-1'));
    await waitFor(() => sockets.length > 0);

    expect(await opening).toBe(false);
    expect(controller.getState()).toMatchObject({ loadState: 'failed', phase: 'idle' });
    expect(controller.getState().error).toContain('同步超时');
    // 超时只报告失败，不关闭服务端句柄。
    expect(log.some((entry) => entry.startsWith('close:'))).toBe(false);
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

async function tick(ms = 5): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

async function finishReload(
  reloading: Promise<boolean>,
  sockets: FakeSocket[],
  options: Parameters<typeof snapshotFrame>[0],
): Promise<boolean> {
  await waitFor(() => sockets.some((socket) => socket.readyState === 0));
  const socket = sockets.filter((candidate) => candidate.readyState === 0).at(-1)!;
  socket.open();
  socket.message(snapshotFrame(options));
  return await reloading;
}

describe('ChatController cancel intent', () => {
  test('a settled turn releases the cancel intent so the next send reaches the action API', async () => {
    const { controller, sockets, sentTargets } = harness();
    await openWithSnapshot(controller, summary('session-1'), sockets, {
      sessionId: 'session-1', running: true,
      activeTurn: { turnId: 'turn-1', status: 'running' },
    });
    expect(await controller.stop()).toBe(true);
    expect(controller.getState().phase).toBe('cancelling');
    // 取消只是 requested：任务仍在跑时输入不发。
    expect(await controller.dispatchSend('不应发送')).toBeNull();

    sockets.at(-1)!.message(snapshotFrame({
      sessionId: 'session-1', running: false, sequence: 1,
      activeTurn: { turnId: 'turn-1', status: 'completed' },
    }));
    await waitFor(() => controller.getState().phase === 'idle');

    const sent = await controller.dispatchSend('继续');
    expect(sent?.sessionId).toBe('session-1');
    expect(sentTargets.at(-1)).toEqual({ sessionId: 'session-1', bindingRevision: 1 });
    await controller.dispose();
  });

  test('a new turn invalidates the previous cancel intent even while running', async () => {
    const { controller, sockets } = harness();
    await openWithSnapshot(controller, summary('session-1'), sockets, {
      sessionId: 'session-1', running: true,
      activeTurn: { turnId: 'turn-1', status: 'running' },
    });
    await controller.stop();
    expect(controller.getState().phase).toBe('cancelling');
    sockets.at(-1)!.message(snapshotFrame({
      sessionId: 'session-1', running: true, sequence: 1,
      activeTurn: { turnId: 'turn-2', status: 'running' },
    }));
    await waitFor(() => controller.getState().phase === 'streaming');
    await controller.dispose();
  });
});

describe('ChatController action recovery', () => {
  test('an unknown action result exposes reload, and reopening clears the blocked state', async () => {
    let fail = true;
    const { controller, sockets, sentTargets } = harness({
      send: async () => {
        if (fail) throw new RemoteActionError('transport_error', '网络请求失败，动作执行状态未知。', 'unknown');
        return { ...receipt({ requestId: 'request-1', sessionId: 'session-1' }), inputId: 'input-1' };
      },
    });
    await openWithSnapshot(controller, summary('session-1'), sockets, { sessionId: 'session-1' });

    expect(await controller.dispatchSend('你好')).toBeNull();
    expect(controller.getState()).toMatchObject({ phase: 'blocked', recovery: 'reload' });
    expect(controller.getState().error).toContain('未确认');
    // 关闭文案不能把阻断态藏起来：仍然 blocked，但提示与恢复入口仍在。
    controller.dismissError();
    expect(controller.getState()).toMatchObject({ phase: 'blocked', error: '' });
    expect(await controller.dispatchSend('重试')).toBeNull();

    fail = false;
    const reloading = controller.reloadChat();
    expect(await finishReload(reloading, sockets, { sessionId: 'session-1', sequence: 1 })).toBe(true);
    expect(controller.getState()).toMatchObject({ phase: 'idle', recovery: 'none', error: '' });
    expect(await controller.dispatchSend('重试')).not.toBeNull();
    expect(sentTargets.length).toBeGreaterThan(0);
    await controller.dispose();
  });

  test('a 409 binding conflict on send exposes rebind and reuses the open-flow fallback', async () => {
    let sendAttempts = 0;
    const { controller, sockets, sentTargets, log } = harness({
      changeSession: async () => {
        throw new RemoteActionError('binding_conflict', 'Chat binding is stale', 'rejected', { status: 409 });
      },
      send: async () => {
        sendAttempts += 1;
        if (sendAttempts === 1) {
          throw new RemoteActionError('binding_conflict', 'Chat binding is stale', 'rejected', { status: 409 });
        }
        return { ...receipt({ requestId: 'request-1', sessionId: 'session-1' }), inputId: 'input-1' };
      },
    });
    await openWithSnapshot(controller, summary('session-1'), sockets, { sessionId: 'session-1' });

    expect(await controller.dispatchSend('你好')).toBeNull();
    expect(controller.getState()).toMatchObject({ recovery: 'reload', phase: 'blocked' });
    expect(controller.getState().error).toContain('绑定');
    expect(log.filter((entry) => entry.startsWith('sync:')).at(-1)).toBe('sync:session-1');

    const reloading = controller.reloadChat();
    expect(await finishReload(reloading, sockets, {
      sessionId: 'session-1', bindingRevision: 9, sequence: 1,
    })).toBe(true);
    expect(controller.getState().recovery).toBe('none');
    // 重新换绑仍 409：继续以 Session ID 寻址既有控制者。
    expect(log).toContain('close:handle-2');
    expect(log.filter((entry) => entry.startsWith('sync:')).at(-1)).toBe('sync:session-1');

    expect(await controller.dispatchSend('重试')).not.toBeNull();
    expect(sentTargets.at(-1)).toEqual({ sessionId: 'session-1', bindingRevision: 9 });
    await controller.dispose();
  });
});

describe('ChatController send delivery', () => {
  test('send waits for the replica delivery fact before reporting success', async () => {
    const { controller, sockets, sentTargets } = harness();
    await openWithSnapshot(controller, summary('session-1'), sockets, { sessionId: 'session-1' });

    let outcome: Awaited<ReturnType<typeof controller.send>> | undefined;
    const sending = controller.send('你好').then((value) => { outcome = value; return value; });
    await waitFor(() => sentTargets.length > 0);
    await tick();
    // 202 到手但副本还没结算：不能当作已送达。
    expect(outcome).toBeUndefined();

    sockets.at(-1)!.message(snapshotFrame({
      sessionId: 'session-1', sequence: 1,
      inputQueue: { items: [{ inputId: 'input-1', state: 'delivered' }] },
    }));
    expect(await sending).toEqual({ delivered: true, sessionId: 'session-1' });
    expect(controller.getState().error).toBe('');
    await controller.dispose();
  });

  test('a withdrawn input is reported as not delivered and keeps a visible error', async () => {
    const { controller, sockets, sentTargets } = harness();
    await openWithSnapshot(controller, summary('session-1'), sockets, { sessionId: 'session-1' });

    const sending = controller.send('你好');
    await waitFor(() => sentTargets.length > 0);
    await tick();
    sockets.at(-1)!.message(snapshotFrame({
      sessionId: 'session-1', sequence: 1,
      inputQueue: { items: [{ inputId: 'input-1', state: 'withdrawn' }] },
    }));
    const outcome = await sending;
    expect(outcome).toMatchObject({ delivered: false, sessionId: 'session-1' });
    expect(outcome.delivered ? '' : outcome.message).toContain('撤回');
    expect(controller.getState().error).toContain('撤回');
    await controller.dispose();
  });

  test('the delivered user entry confirms an input even without a queue snapshot', async () => {
    const { controller, sockets, sentTargets } = harness();
    await openWithSnapshot(controller, summary('session-1'), sockets, { sessionId: 'session-1' });

    const sending = controller.send('你好');
    await waitFor(() => sentTargets.length > 0);
    await tick();
    sockets.at(-1)!.message(snapshotFrame({
      sessionId: 'session-1', sequence: 1, userEntryIds: ['turn:input-1:user'],
    }));
    expect(await sending).toEqual({ delivered: true, sessionId: 'session-1' });
    await controller.dispose();
  });
});

describe('ChatController rebind and recovery flows', () => {
  test('selected chat follows the replica binding identity and session title', async () => {
    const { controller, sockets } = harness();
    await openWithSnapshot(controller, summary('session-1', '旧标题'), sockets, {
      sessionId: 'session-1', title: '旧标题',
    });
    expect(controller.getState().selected).toMatchObject({ id: 'session-1', title: '旧标题' });

    sockets.at(-1)!.message(snapshotFrame({
      sessionId: 'session-1', bindingRevision: 2, title: '服务端改名', sequence: 1,
    }));
    await waitFor(() => controller.getState().selected?.title === '服务端改名');
    expect(controller.getState().state?.chat).toMatchObject({ id: 'session-1', title: '服务端改名' });
    expect(controller.getState().state?.bindingRevision).toBe(2);

    sockets.at(-1)!.message(snapshotFrame({
      sessionId: 'session-2', bindingRevision: 3, title: '第二个会话', sequence: 2,
    }));
    await waitFor(() => controller.getState().selected?.id === 'session-2');
    expect(controller.currentChatId()).toBe('session-2');
    expect(controller.getState().state?.chat.id).toBe('session-2');
    await controller.dispose();
  });

  test('reload after a failed new-chat creation retries creation instead of returning false', async () => {
    let attempts = 0;
    const { controller, sockets } = harness({
      createSession: async () => {
        attempts += 1;
        if (attempts === 1) {
          throw new RemoteActionError('sandbox_start_failed', '沙箱启动失败', 'rejected', { status: 503 });
        }
        return receipt({ requestId: 'request-1', sessionId: 'recovered-session' });
      },
    });
    expect(await controller.openChat(null)).toBe(false);
    expect(controller.getState()).toMatchObject({ loadState: 'failed', selected: null });

    const reloading = controller.reloadChat();
    expect(await finishReload(reloading, sockets, { sessionId: 'recovered-session', sequence: 1 })).toBe(true);
    expect(controller.getState().selected?.id).toBe('recovered-session');
    await controller.dispose();
  });

  test('reload releases the active chat and truly reopens the same session', async () => {
    const { controller, sockets, log } = harness();
    await openWithSnapshot(controller, summary('session-1'), sockets, {
      sessionId: 'session-1', executionBlocked: true,
    });
    expect(controller.getState().phase).toBe('blocked');
    const previous = sockets.at(-1)!;
    const before = log.filter((entry) => entry === 'createHandle').length;

    const reloading = controller.reloadChat();
    expect(await finishReload(reloading, sockets, {
      sessionId: 'session-1', bindingRevision: 2, sequence: 1,
    })).toBe(true);
    expect(previous.readyState).toBe(3);
    expect(log.filter((entry) => entry === 'createHandle').length).toBe(before + 1);
    expect(controller.getState()).toMatchObject({ phase: 'idle', loadState: 'ready' });
    await controller.dispose();
  });

  test('rebuild closes the handle, surfaces close_incomplete and retries into a new chat', async () => {
    let closeAttempts = 0;
    const { controller, sockets, log } = harness({
      close: async () => {
        closeAttempts += 1;
        if (closeAttempts === 1) {
          throw new RemoteActionError(
            'close_incomplete', 'Chat is closed but cleanup is incomplete', 'unknown',
            { status: 500, incomplete: true },
          );
        }
      },
    });
    await openWithSnapshot(controller, summary('session-1'), sockets, {
      sessionId: 'session-1', executionBlocked: true,
    });
    expect(controller.getState().phase).toBe('blocked');

    expect(await controller.rebuildChat()).toBe(false);
    expect(controller.getState().error).toContain('关闭未完成');
    expect(controller.getState()).toMatchObject({ phase: 'blocked', recovery: 'reload' });
    // 关闭失败不能把本地会话提前释放。
    expect(controller.getState().selected?.id).toBe('session-1');

    const rebuilding = controller.rebuildChat();
    expect(await finishReload(rebuilding, sockets, { sessionId: 'created-session', sequence: 1 })).toBe(true);
    expect(controller.getState().selected?.id).toBe('created-session');
    expect(log).toContain('close:handle-1');
    await controller.dispose();
  });
});

describe('ChatController config options', () => {
  test('writes config options through the same bound target and action log', async () => {
    const { controller, log, sockets, sentTargets } = harness();
    await openWithSnapshot(controller, summary('session-1'), sockets, { sessionId: 'session-1' });

    const result = await controller.setConfigOption('model', 'fast');
    expect(result).toEqual({ ok: true });
    expect(log.at(-1)).toBe('action:set_config_option');
    expect(sentTargets.at(-1)).toEqual({ sessionId: 'session-1', bindingRevision: 1 });
    await controller.dispose();
  });

  test('an unbound controller rejects config writes without pretending success', async () => {
    const { controller } = harness();
    expect(await controller.setConfigOption('mode', 'auto')).toEqual({
      ok: false, message: '会话尚未就绪，配置未提交。',
    });
    await controller.dispose();
  });
});
