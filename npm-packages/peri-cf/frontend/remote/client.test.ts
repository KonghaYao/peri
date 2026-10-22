import { describe, expect, test } from 'bun:test';
import { AgentManager } from './client';
import { ChatCommandError } from '@core/agent';
import type {
  Chat, ChatCommand, ChatCommandPayload, ChatRepository, ChatRuntimePort, SyncState,
} from '@core/agent';

const chat: Chat = { id: '11111111-1111-4111-8111-111111111111', title: '测试会话', updatedAt: '2026-10-20T00:00:00Z' };
const otherChat: Chat = { id: '22222222-2222-4222-8222-222222222222', title: '另一个会话', updatedAt: '2026-10-20T01:00:00Z' };
const chatSummary = {
  ...chat, messageCount: 0, running: false, pendingInteractionCount: 0,
  lastTurnStatus: null, lastCompletedAt: null,
};

function stateOf(target: Chat): SyncState {
  return {
    chat: target, running: false, executionBlocked: false, readiness: 'ready',
    revision: 0,
    entries: [], toolCalls: [], turns: [], activeTurn: { turnId: null, status: null },
    session: {}, tasks: [], plans: { current: [], byTurn: {} },
    interactions: [], pendingInteractions: [], inputQueue: null,
  };
}

class FakeRuntime implements ChatRuntimePort {
  readonly commands: ChatCommand[] = [];
  readonly listeners = new Set<(state: SyncState) => void>();
  readonly statusListeners = new Set<(status: 'connecting' | 'synced' | 'failed', error?: Error) => void>();
  hydrated = 0;
  readFailures = 0;
  watchFailures = 0;
  disposed = false;
  onCommand?: (command: ChatCommand) => void;
  commandImpl?: (command: ChatCommand) => Promise<ChatCommandPayload | undefined>;
  readonly value: SyncState;

  constructor(target: Chat) { this.value = stateOf(target); }

  async read(): Promise<SyncState> {
    if (this.readFailures > 0) {
      this.readFailures -= 1;
      throw new Error('历史读取失败');
    }
    this.hydrated += 1;
    return this.value;
  }
  async watch(onState: (state: SyncState) => void): Promise<{ initial: SyncState; close(): void }> {
    if (this.watchFailures > 0) {
      this.watchFailures -= 1;
      throw new Error('事件流首帧失败');
    }
    this.listeners.add(onState);
    const initial = await this.read();
    return { initial, close: () => { this.listeners.delete(onState); } };
  }
  subscribeStreamStatus(listener: (status: 'connecting' | 'synced' | 'failed', error?: Error) => void): () => void {
    this.statusListeners.add(listener);
    return () => { this.statusListeners.delete(listener); };
  }
  emitStreamStatus(status: 'connecting' | 'synced' | 'failed', error?: Error): void {
    for (const listener of [...this.statusListeners]) listener(status, error);
  }
  async command(command: ChatCommand): Promise<ChatCommandPayload | undefined> {
    this.commands.push(command);
    this.onCommand?.(command);
    if (this.commandImpl) return await this.commandImpl(command);
    if (command.op === 'payload') return { id: 'ref-1', bytes: 2, json: '{}' };
    return undefined;
  }
  dispose(): void { this.disposed = true; }

  /** 模拟服务端投影推送。 */
  publish(): void {
    for (const listener of [...this.listeners]) listener(this.value);
  }
}

function setup(options: { create?: () => Promise<Chat>; chats?: Chat[] } = {}) {
  const byId = new Map((options.chats ?? [chat]).map((item) => [item.id, item]));
  const runtimes = new Map<string, FakeRuntime>();
  const runtimeFor = (id: string): FakeRuntime => {
    let runtime = runtimes.get(id);
    if (!runtime) {
      runtime = new FakeRuntime(byId.get(id) ?? chat);
      runtimes.set(id, runtime);
    }
    return runtime;
  };
  const calls = { created: 0, listed: 0 };
  const repository: Pick<ChatRepository, 'list' | 'create'> = {
    async list() { calls.listed += 1; return [chatSummary]; },
    async create() { calls.created += 1; return options.create ? options.create() : chat; },
  };
  const manager = new AgentManager({
    repository,
    runtimeFor: (id) => runtimeFor(id),
  });
  return { manager, runtime: runtimeFor(chat.id), runtimeFor, calls };
}

describe('AgentManager client facade', () => {
  test('lists and creates chats without touching the runtime', async () => {
    const { manager, runtime, calls } = setup();
    expect(await manager.listChats()).toEqual([chatSummary]);
    expect(await manager.createChat()).toEqual(chat);
    expect(calls).toEqual({ created: 1, listed: 1 });
    expect(runtime.hydrated).toBe(0);
    manager.dispose();
  });

  test('opens a chat, hydrates it, and publishes a synced view', async () => {
    const { manager, runtime } = setup();
    const views: string[] = [];
    const stop = manager.subscribe(() => views.push(manager.getState().phase));

    expect(await manager.openChat(chat)).toBe(true);
    expect(manager.getState()).toMatchObject({
      selected: chat, phase: 'idle', historyReady: true, error: '',
    });
    expect(manager.getState().state?.chat.id).toBe(chat.id);
    expect(runtime.hydrated).toBeGreaterThanOrEqual(2);
    expect(views).toContain('loading');
    expect(views.at(-1)).toBe('idle');

    stop();
    manager.dispose();
  });

  test('sends prompts and reads payloads through the runtime port', async () => {
    const { manager, runtime } = setup();
    await manager.openChat(chat);

    expect(await manager.send('  你好  ')).toBe(chat.id);
    expect(runtime.commands.at(-1)).toEqual({ op: 'prompt', content: '你好' });

    expect(await manager.readPayload('ref-1')).toEqual({
      ok: true, payload: { id: 'ref-1', bytes: 2, json: '{}' },
    });
    expect(runtime.commands.at(-1)).toEqual({ op: 'payload', refId: 'ref-1' });

    manager.dispose();
  });

  test('opens a newly created chat when selection is null', async () => {
    const created: Chat = { ...chat, id: '22222222-2222-4222-8222-222222222222', title: 'New chat' };
    const { manager, calls } = setup({ create: async () => created, chats: [chat, created] });
    expect(await manager.openChat(null)).toBe(true);
    expect(calls.created).toBe(1);
    expect(manager.getState().selected).toEqual(created);
    manager.dispose();
  });

  test('an implicit send returns the created chat identity even after the view switches away', async () => {
    const created: Chat = { ...chat, id: '33333333-3333-4333-8333-333333333333', title: 'Implicit' };
    const { manager, runtime, runtimeFor } = setup({
      create: async () => created, chats: [chat, created, otherChat],
    });
    const createdRuntime = runtimeFor(created.id);
    let finishPrompt!: () => void;
    createdRuntime.commandImpl = () => new Promise<ChatCommandPayload | undefined>((resolve) => {
      finishPrompt = () => resolve(undefined);
    });

    const sending = manager.send('隐式创建');
    for (let attempt = 0; attempt < 30 && createdRuntime.commands.length === 0; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
    expect(createdRuntime.commands).toEqual([{ op: 'prompt', content: '隐式创建' }]);

    // 创建完成后、命令结算前切走：返回值仍必须是实际接收 prompt 的会话。
    expect(await manager.openChat(otherChat)).toBe(true);
    finishPrompt();
    expect(await sending).toBe(created.id);
    expect(manager.getState().selected).toEqual(otherChat);
    expect(runtime.commands.some((command) => command.op === 'prompt')).toBe(false);
    manager.dispose();
  });

  test('selecting the current chat is a no-op even while it is running', async () => {
    const { manager, runtime } = setup();
    await manager.openChat(chat);
    runtime.value.running = true;
    runtime.publish();
    expect(manager.getState().state?.running).toBe(true);

    const hydrated = runtime.hydrated;
    const listeners = runtime.listeners.size;
    expect(await manager.openChat(chat)).toBe(true);
    expect(runtime.hydrated).toBe(hydrated);
    expect(runtime.listeners.size).toBe(listeners);
    expect(runtime.commands).toEqual([]);
    manager.dispose();
  });

  test('switching chats closes the old subscription without cancelling its task', async () => {
    const { manager, runtime } = setup({ chats: [chat, otherChat] });
    await manager.openChat(chat);
    runtime.value.running = true;
    runtime.publish();

    expect(await manager.openChat(otherChat)).toBe(true);
    expect(manager.getState().selected).toEqual(otherChat);
    expect(manager.getState().state?.running).toBe(false);
    expect(runtime.listeners.size).toBe(0);
    expect(runtime.commands.some((command) => command.op === 'cancel')).toBe(false);

    // 重开旧会话时重新附着同一服务端任务，仍然不需要 cancel。
    expect(await manager.openChat(chat)).toBe(true);
    expect(manager.getState().state?.running).toBe(true);
    expect(runtime.listeners.size).toBe(1);
    expect(runtime.commands.some((command) => command.op === 'cancel')).toBe(false);
    manager.dispose();
  });

  test('creating a new chat leaves the current running task untouched', async () => {
    const created: Chat = { ...otherChat, title: 'New chat' };
    const { manager, runtime, calls } = setup({ chats: [chat, created], create: async () => created });
    await manager.openChat(chat);
    runtime.value.running = true;
    runtime.publish();

    expect(await manager.openChat(null)).toBe(true);
    expect(calls.created).toBe(1);
    expect(runtime.listeners.size).toBe(0);
    expect(runtime.commands.some((command) => command.op === 'cancel')).toBe(false);
    expect(manager.getState().selected).toEqual(created);
    manager.dispose();
  });

  test('a failed history read can be retried without a cancel or a stale selection', async () => {
    const { manager, runtime } = setup();
    runtime.readFailures = 1;

    expect(await manager.openChat(chat)).toBe(false);
    expect(manager.getState()).toMatchObject({ selected: chat, historyReady: false, loadState: 'failed' });
    expect(manager.getState().error).toContain('历史读取失败');
    expect(manager.getState().state).toBeNull();

    expect(await manager.openChat(chat)).toBe(true);
    expect(manager.getState()).toMatchObject({ selected: chat, historyReady: true, loadState: 'ready', error: '' });
    expect(runtime.commands).toEqual([]);
    manager.dispose();
  });

  test('dismissing the error keeps the failed load state for the timeline', async () => {
    const { manager, runtime } = setup();
    runtime.readFailures = 1;
    expect(await manager.openChat(chat)).toBe(false);
    expect(manager.getState().loadState).toBe('failed');

    manager.dismissError();
    expect(manager.getState().error).toBe('');
    expect(manager.getState().loadState).toBe('failed');
    manager.dispose();
  });

  test('a dismissed execution error is not re-shown by the next identical projection frame', async () => {
    const { manager, runtime } = setup();
    await manager.openChat(chat);
    runtime.value.error = '本轮执行失败';
    runtime.publish();
    expect(manager.getState().error).toContain('本轮执行失败');

    manager.dismissError();
    runtime.publish();
    expect(manager.getState().error).toBe('');

    // 新错误仍会展示。
    runtime.value.error = '另一个错误';
    runtime.publish();
    expect(manager.getState().error).toContain('另一个错误');
    manager.dispose();
  });

  test('content plus a first-frame stream failure keeps content, marks failed and stays retryable', async () => {
    const { manager, runtime } = setup();
    runtime.value.entries = [{
      entryId: 'turn:1:user', id: 'turn:1:user', kind: 'message', turnId: 'turn:1',
      role: 'user', status: 'completed', createdAt: chat.updatedAt, createdAtSource: 'observed',
      completedAt: null, error: null, blocks: [{ blockId: 'text:0', type: 'text', text: '已有内容' }],
    }];
    runtime.watchFailures = 1;

    expect(await manager.openChat(chat)).toBe(false);
    expect(manager.getState()).toMatchObject({
      selected: chat, loadState: 'failed', liveSync: 'failed', historyReady: false,
    });
    expect(manager.getState().state?.entries).toHaveLength(1);
    expect(manager.getState().error).toContain('事件流首帧失败');

    // 关闭错误文本后失败事实仍在：由 loadState 提供重新加载入口。
    manager.dismissError();
    expect(manager.getState().error).toBe('');
    expect(manager.getState().loadState).toBe('failed');

    expect(await manager.reloadChat()).toBe(true);
    expect(manager.getState()).toMatchObject({ loadState: 'ready', liveSync: 'synced', historyReady: true });
    manager.dispose();
  });

  test('a mid-stream disconnect keeps readable content and allows reconnecting while running', async () => {
    const { manager, runtime } = setup();
    await manager.openChat(chat);
    runtime.value.running = true;
    runtime.publish();
    runtime.emitStreamStatus('failed', new Error('事件流中断'));

    expect(manager.getState()).toMatchObject({
      loadState: 'ready', liveSync: 'failed', historyReady: true,
    });
    expect(manager.getState().error).toContain('事件流中断');
    expect(manager.getState().state).not.toBeNull();

    const hydrated = runtime.hydrated;
    expect(await manager.reloadChat()).toBe(true);
    expect(runtime.hydrated).toBeGreaterThan(hydrated);
    expect(manager.getState().liveSync).toBe('synced');
    expect(runtime.commands.some((command) => command.op === 'cancel')).toBe(false);

    // EventSource 自动重连成功时也恢复同步态，不需要用户再次操作。
    runtime.emitStreamStatus('failed', new Error('再次中断'));
    expect(manager.getState().liveSync).toBe('failed');
    runtime.emitStreamStatus('synced');
    expect(manager.getState()).toMatchObject({ liveSync: 'synced', loadState: 'ready', historyReady: true });
    manager.dispose();
  });

  test('a late successful send during a same-chat reload is not reported as unsent', async () => {
    const { manager, runtime } = setup();
    await manager.openChat(chat);
    let finishPrompt!: () => void;
    runtime.commandImpl = () => new Promise<ChatCommandPayload | undefined>((resolve) => {
      finishPrompt = () => resolve(undefined);
    });

    const sending = manager.send('可靠消息');
    await Promise.resolve();
    expect(await manager.openChat(chat, { force: true })).toBe(true);
    finishPrompt();

    expect(await sending).toBe(chat.id);
    expect(manager.getState().error).toBe('');
    expect(manager.getState().selected).toEqual(chat);
    manager.dispose();
  });

  test('an error on an already loaded chat forces a real re-read on same-id selection', async () => {
    const { manager, runtime } = setup();
    await manager.openChat(chat);
    runtime.value.error = '本轮执行失败';
    runtime.publish();
    expect(manager.getState().error).toContain('本轮执行失败');

    const hydrated = runtime.hydrated;
    expect(await manager.openChat(chat)).toBe(true);
    expect(runtime.hydrated).toBeGreaterThan(hydrated);
    expect(runtime.commands.some((command) => command.op === 'cancel')).toBe(false);
    manager.dispose();
  });

  test('reloadChat re-reads and re-watches without disposing or cancelling a running task', async () => {
    const { manager, runtime } = setup();
    await manager.openChat(chat);
    runtime.value.running = true;
    runtime.publish();

    const hydrated = runtime.hydrated;
    expect(await manager.reloadChat()).toBe(false);
    expect(runtime.hydrated).toBe(hydrated);
    expect(runtime.listeners.size).toBe(1);
    expect(runtime.disposed).toBe(false);
    expect(runtime.commands).toEqual([]);

    runtime.value.running = false;
    runtime.publish();
    expect(await manager.reloadChat()).toBe(true);
    expect(runtime.hydrated).toBeGreaterThan(hydrated);
    expect(runtime.listeners.size).toBe(1);
    expect(runtime.disposed).toBe(false);
    manager.dispose();
  });

  test('a failed new-chat creation can be retried through reloadChat', async () => {
    const created: Chat = { ...otherChat, title: 'New chat' };
    let attempts = 0;
    const { manager, calls } = setup({
      chats: [chat, created],
      create: async () => {
        if (++attempts === 1) throw new Error('创建失败');
        return created;
      },
    });

    expect(await manager.openChat(null)).toBe(false);
    expect(manager.getState()).toMatchObject({ selected: null, loadState: 'failed' });
    expect(manager.getState().error).toContain('创建失败');
    expect(calls.created).toBe(1);

    expect(await manager.reloadChat()).toBe(true);
    expect(calls.created).toBe(2);
    expect(manager.getState().selected).toEqual(created);
    manager.dispose();
  });

  test('a prompt settling after navigation cannot mutate the new chat view', async () => {
    const { manager, runtime } = setup({ chats: [chat, otherChat] });
    await manager.openChat(chat);
    let rejectPrompt!: (error: unknown) => void;
    runtime.command = async (command) => {
      runtime.commands.push(command);
      return await new Promise<ChatCommandPayload | undefined>((_resolve, reject) => {
        rejectPrompt = reject;
      });
    };

    const sending = manager.send('你好');
    await Promise.resolve();
    expect(await manager.openChat(otherChat)).toBe(true);
    rejectPrompt(new Error('旧会话迟到的失败'));

    expect(await sending).toBeNull();
    expect(manager.getState().selected).toEqual(otherChat);
    expect(manager.getState().error).toBe('');
    expect(manager.getState().phase).toBe('idle');
    manager.dispose();
  });

  test('a late successful send from the old chat cannot lock or pollute the new chat', async () => {
    const { manager, runtime, runtimeFor } = setup({ chats: [chat, otherChat] });
    await manager.openChat(chat);
    let finishPrompt!: () => void;
    runtime.commandImpl = () => new Promise<ChatCommandPayload | undefined>((resolve) => {
      finishPrompt = () => resolve(undefined);
    });

    const sending = manager.send('旧会话消息');
    await Promise.resolve();
    await manager.openChat(otherChat);

    // 切换后新会话可以立即发送；旧 in-flight 不锁住 Composer。
    expect(await manager.send('新会话消息')).toBe(otherChat.id);
    finishPrompt();
    // 迟到的成功仍是“已发送”，且身份是旧会话；但不得改写新会话视图。
    expect(await sending).toBe(chat.id);
    expect(manager.getState().selected).toEqual(otherChat);
    expect(manager.getState().error).toBe('');
    expect(runtimeFor(otherChat.id).commands).toEqual([{ op: 'prompt', content: '新会话消息' }]);
    manager.dispose();
  });

  test('stops publishing after dispose', async () => {
    const { manager } = setup();
    let published = 0;
    manager.subscribe(() => { published += 1; });
    await manager.openChat(chat);
    const before = published;
    manager.dispose();
    expect(manager.subscribe(() => { published += 1; })()).toBeUndefined();
    expect(await manager.openChat(chat)).toBe(false);
    expect(published).toBe(before);
  });
});

describe('AgentManager command dispositions', () => {
  test('rejected prompts show the refusal and return to idle without blocking', async () => {
    const { manager, runtime } = setup();
    await manager.openChat(chat);
    runtime.onCommand = () => { throw new ChatCommandError('busy', 'Chat is busy', 'rejected'); };

    expect(await manager.send('你好')).toBeNull();
    expect(manager.getState().phase).toBe('idle');
    expect(manager.getState().error).toContain('会话正在执行中');

    // 未被 uncertain 拦住：第二次发送仍会触达命令通道。
    expect(await manager.send('再试')).toBeNull();
    expect(runtime.commands.filter((command) => command.op === 'prompt')).toHaveLength(2);
    manager.dispose();
  });

  test('generic runtime failures are unknown and block sending', async () => {
    const { manager, runtime } = setup();
    await manager.openChat(chat);
    runtime.onCommand = () => { throw new TypeError('network down'); };

    expect(await manager.send('你好')).toBeNull();
    expect(manager.getState().phase).toBe('blocked');
    expect(manager.getState().error).toContain('尚未确认执行状态');
    manager.dispose();
  });

  test('internal_error with unknown disposition blocks sending', async () => {
    const { manager, runtime } = setup();
    await manager.openChat(chat);
    runtime.onCommand = () => { throw new ChatCommandError('internal_error', 'boom', 'unknown'); };

    expect(await manager.send('你好')).toBeNull();
    expect(manager.getState().phase).toBe('blocked');
    manager.dispose();
  });

  test('a successful cancel after unknown clears uncertainty', async () => {
    const { manager, runtime } = setup();
    await manager.openChat(chat);
    runtime.onCommand = (command) => {
      if (command.op === 'prompt') throw new ChatCommandError('internal_error', 'boom', 'unknown');
    };

    expect(await manager.send('你好')).toBeNull();
    expect(manager.getState().phase).toBe('blocked');

    runtime.onCommand = undefined;
    expect(await manager.stop()).toBe(true);
    expect(manager.getState().phase).toBe('idle');
    manager.dispose();
  });

  test('preflight failures are not_sent and do not lock the session', async () => {
    const { manager } = setup();

    expect(await manager.readPayload('ref-1')).toEqual({
      ok: false, code: 'unavailable', message: '会话尚未就绪，无法读取全文。', disposition: 'not_sent',
    });
    await expect(manager.answerInteraction('i-1', { kind: 'permission', decision: 'allow_once' }))
      .rejects.toThrow('回答未提交');
    expect(manager.getState().phase).toBe('idle');
    manager.dispose();
  });
});
