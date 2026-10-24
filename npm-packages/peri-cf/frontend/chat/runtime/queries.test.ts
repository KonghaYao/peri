// 必须在 query-core 求值前安装 window：其 `isServer` 在模块加载时求值，
// 否则 Bun 测试环境会被判定为 server，refetchInterval 定时器不会调度。
import '../../test/install-dom';
import { afterEach, describe, expect, jest, test } from 'bun:test';
import { isCancelledError, QueryObserver } from '@tanstack/react-query';
import {
  LIST_POLL_IDLE_INTERVAL_MS, LIST_POLL_INTERVAL_MS, createChatQueries, listRefetchInterval,
  mergeChatSummary, summaryFacts,
} from './queries';
import type { ChatReadApi } from './queries';
import type { Chat, ChatSummary, SyncState } from './types';

type FixtureEntry = { id: string; role: 'user' | 'assistant'; content: string; status?: string; error?: { code: string; message: string } };
function state(chat: Chat, entries: FixtureEntry[] = [], extra: Partial<SyncState> = {}): SyncState {
  return {
    chat, running: false, executionBlocked: false, readiness: 'ready',
    revision: 0,
    entries: entries.map((entry) => ({ entryId: `entry:${entry.id}`, id: entry.id, kind: 'message', turnId: 'turn:1',
      role: entry.role, status: entry.status ?? 'completed', createdAt: chat.updatedAt, createdAtSource: 'observed',
      completedAt: null, error: entry.error ?? null,
      blocks: entry.content ? [{ blockId: 'text:0', type: 'text', text: entry.content }] : [] })),
    toolCalls: [], turns: [], activeTurn: { turnId: null, status: null }, session: {}, tasks: [],
    plans: { current: [], byTurn: {} }, interactions: [], pendingInteractions: [], inputQueue: null,
    ...extra,
  };
}
function firstText(value: SyncState): string {
  const block = value.entries[0]?.blocks[0];
  return block?.type === 'text' ? block.text : '';
}

const first: Chat = { id: 'first', title: '第一段思考', updatedAt: '2026-10-06T00:00:00Z' };
const second: Chat = { id: 'second', title: '新的思考', updatedAt: '2026-10-06T01:00:00Z' };
/** 列表读面的事实字段；测试默认与新建会话一致。 */
function summary(chat: Chat): ChatSummary {
  return {
    ...chat, messageCount: 0, running: false, pendingInteractionCount: 0,
    lastTurnStatus: null, lastCompletedAt: null,
  };
}
const firstSummary = summary(first);
const secondSummary = summary(second);
const scopes: ReturnType<typeof createChatQueries>[] = [];

function deferred<Value>() {
  let resolve!: (value: Value) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<Value>((accept, fail) => { resolve = accept; reject = fail; });
  return { promise, resolve, reject };
}

function scope(overrides: Partial<ChatReadApi> = {}) {
  const queries = createChatQueries({
    list: async () => [firstSummary],
    detail: async () => state(first),
    ...overrides,
  });
  scopes.push(queries);
  return queries;
}

afterEach(async () => {
  jest.useRealTimers();
  await Promise.all(scopes.splice(0).map(queries => queries.dispose()));
});

/** 推进一轮微任务，让 QueryObserver 的 fetch 结算。 */
async function flushMicrotasks(): Promise<void> {
  for (let index = 0; index < 8; index += 1) await Promise.resolve();
}

describe('chat QueryClient read ownership', () => {
  test('replica state replaces authoritative detail cache without HTTP refetch or delta accumulation', async () => {
    let reads = 0;
    const queries = scope({ detail: async () => { reads++; return state(first); } });
    await queries.readHistory(first.id);
    const detail: SyncState = state({ ...first, title: '同步标题' },
      [{ id: 'answer', role: 'assistant', content: '完整投影' }]);
    queries.setHistory(detail);
    queries.mergeChat(detail.chat);
    expect(await queries.readHistory(first.id, undefined, false)).toEqual(detail);
    expect(reads).toBe(1);
    expect(queries.client.getQueryData<ChatSummary[]>(queries.list.queryKey)).toEqual([summary(detail.chat)]);
  });
  test('deduplicates concurrent list requests and sorts the server result', async () => {
    const response = deferred<ChatSummary[]>();
    let requests = 0;
    let receivedSignal: AbortSignal | undefined;
    const queries = scope({ list: signal => {
      requests++;
      receivedSignal = signal;
      return response.promise;
    } });
    const firstRead = queries.client.fetchQuery(queries.list);
    const concurrentRead = queries.client.fetchQuery(queries.list);
    expect(requests).toBe(1);
    expect(receivedSignal).toBeInstanceOf(AbortSignal);
    response.resolve([firstSummary, secondSummary]);
    expect(await firstRead).toEqual([secondSummary, firstSummary]);
    expect(await concurrentRead).toEqual([secondSummary, firstSummary]);
  });

  test('merges metadata in the query cache without duplicating identities', async () => {
    const queries = scope();
    await queries.client.fetchQuery(queries.list);
    queries.mergeChat(second);
    expect(queries.client.getQueryData<ChatSummary[]>(queries.list.queryKey)).toEqual([
      secondSummary, firstSummary,
    ]);
  });

  test('list title and updatedAt win over stale runtime metadata', async () => {
    const queries = scope();
    await queries.client.fetchQuery(queries.list);
    queries.mergeChat({ ...first, title: '运行期旧标题', updatedAt: '2026-10-06T02:00:00Z' });
    expect(queries.client.getQueryData<ChatSummary[]>(queries.list.queryKey)).toEqual([firstSummary]);

    const refreshed = { ...firstSummary, title: '列表新标题', updatedAt: '2026-10-06T03:00:00Z' };
    queries.client.setQueryData(queries.list.queryKey, [refreshed]);
    queries.mergeChat({ ...first, title: '运行期旧标题', updatedAt: '2026-10-06T02:00:00Z' });
    expect(queries.client.getQueryData<ChatSummary[]>(queries.list.queryKey)).toEqual([refreshed]);
  });

  test('merge authority stays in one rule: list owns title/time, live facts own runtime facts', () => {
    const existing: ChatSummary = {
      ...firstSummary, title: '列表标题', updatedAt: '2026-10-06T05:00:00Z',
      messageCount: 4, running: true, pendingInteractionCount: 2,
      lastTurnStatus: 'streaming', lastCompletedAt: null,
    };
    const merged = mergeChatSummary(existing, { ...first, title: '运行期旧标题', updatedAt: '2026-10-06T00:00:00Z' }, {
      messageCount: 2, running: false, pendingInteractionCount: 0,
      lastTurnStatus: 'completed', lastCompletedAt: '2026-10-06T04:00:00Z',
    });
    expect(merged).toMatchObject({
      title: '列表标题', updatedAt: '2026-10-06T05:00:00Z', messageCount: 4,
      running: false, pendingInteractionCount: 0, lastTurnStatus: 'completed',
    });
    // 无实时事实时保留列表运行态；有新会话时使用新会话标题与时间。
    expect(mergeChatSummary(existing, { ...first, title: '新会话', updatedAt: '2026-10-06T06:00:00Z' }))
      .toMatchObject({ title: '列表标题', updatedAt: '2026-10-06T05:00:00Z', running: true });
    expect(mergeChatSummary(undefined, second)).toMatchObject({
      id: second.id, title: second.title, messageCount: 0, running: false,
    });
  });

  test('polling is 15s when idle and 4s while a background chat is running', async () => {
    const running = { ...secondSummary, running: true };
    expect(listRefetchInterval(undefined)).toBe(LIST_POLL_IDLE_INTERVAL_MS);
    expect(listRefetchInterval([firstSummary])).toBe(LIST_POLL_IDLE_INTERVAL_MS);
    expect(listRefetchInterval([firstSummary, running])).toBe(LIST_POLL_INTERVAL_MS);
    let polls = 0;
    const queries = scope({ list: async () => {
      polls += 1;
      return polls === 1 ? [firstSummary, running] : [firstSummary, secondSummary];
    } });
    await queries.client.fetchQuery(queries.list);
    expect(queries.client.getQueryData<ChatSummary[]>(queries.list.queryKey))
      .toEqual([running, firstSummary]);
    expect(listRefetchInterval(queries.client.getQueryData(queries.list.queryKey))).toBe(LIST_POLL_INTERVAL_MS);

    await queries.client.refetchQueries({ queryKey: queries.list.queryKey });
    expect(queries.client.getQueryData<ChatSummary[]>(queries.list.queryKey))
      .toEqual([secondSummary, firstSummary]);
    // 后台完成后回到 15s 低频轮询，而不是完全停止。
    expect(listRefetchInterval(queries.client.getQueryData(queries.list.queryKey)))
      .toBe(LIST_POLL_IDLE_INTERVAL_MS);
  });

  test('a send followed by an immediate switch keeps the fast interval even before the list cache says running', () => {
    // 发送后立即切走：列表缓存仍是旧的 running=false，但选中会话的实时阶段已运行。
    expect(listRefetchInterval([firstSummary], { liveRunning: true })).toBe(LIST_POLL_INTERVAL_MS);
    // 命令在途/最近活动窗口内同样保持快速轮询，等待服务端事实。
    expect(listRefetchInterval([firstSummary], { activeUntil: 20_000, now: 10_000 })).toBe(LIST_POLL_INTERVAL_MS);
    expect(listRefetchInterval([firstSummary], { activeUntil: 5_000, now: 10_000 }))
      .toBe(LIST_POLL_IDLE_INTERVAL_MS);
  });

  test('hidden tabs suspend polling instead of claiming background discovery', () => {
    expect(listRefetchInterval([firstSummary], { visible: false })).toBe(false);
    expect(listRefetchInterval([{ ...firstSummary, running: true }], { visible: false })).toBe(false);
  });

  test('an external client start is discovered by the idle poll without local activity', async () => {
    jest.useFakeTimers();
    let reads = 0;
    const queries = scope({ list: async () => {
      reads += 1;
      return reads === 1 ? [firstSummary] : [{ ...secondSummary, running: true }, firstSummary];
    } });
    const observer = new QueryObserver(queries.client, {
      ...queries.list,
      refetchInterval: query => listRefetchInterval(query.state.data),
    });
    const unsubscribe = observer.subscribe(() => undefined);
    try {
      await flushMicrotasks();
      expect(reads).toBe(1);
      expect(observer.getCurrentResult().data).toEqual([firstSummary]);

      // 空闲 15s 内不请求；到点后才发现外部客户端启动。
      jest.advanceTimersByTime(LIST_POLL_IDLE_INTERVAL_MS - 1);
      await flushMicrotasks();
      expect(reads).toBe(1);
      jest.advanceTimersByTime(1);
      await flushMicrotasks();
      expect(reads).toBe(2);
      expect(observer.getCurrentResult().data?.[0]).toMatchObject({ id: second.id, running: true });
    } finally {
      unsubscribe();
      jest.useRealTimers();
    }
  });

  test('a cached running chat polls at 4s instead of 15s', async () => {
    jest.useFakeTimers();
    let reads = 0;
    const queries = scope({ list: async () => {
      reads += 1;
      return [{ ...secondSummary, running: true }];
    } });
    const observer = new QueryObserver(queries.client, {
      ...queries.list,
      refetchInterval: query => listRefetchInterval(query.state.data),
    });
    const unsubscribe = observer.subscribe(() => undefined);
    try {
      await flushMicrotasks();
      expect(reads).toBe(1);
      jest.advanceTimersByTime(LIST_POLL_INTERVAL_MS - 1);
      await flushMicrotasks();
      expect(reads).toBe(1);
      jest.advanceTimersByTime(1);
      await flushMicrotasks();
      expect(reads).toBe(2);
    } finally {
      unsubscribe();
      jest.useRealTimers();
    }
  });

  test('optimistic running is visible immediately and live facts clear it after settle', () => {
    const queries = scope();
    queries.client.setQueryData(queries.list.queryKey, [firstSummary]);
    queries.markRunning(first.id);
    expect(queries.client.getQueryData<ChatSummary[]>(queries.list.queryKey)?.[0]?.running).toBe(true);
    queries.mergeChat(first, {
      messageCount: 0, running: false, pendingInteractionCount: 0,
      lastTurnStatus: 'completed', lastCompletedAt: '2026-10-06T00:00:01Z',
    });
    expect(queries.client.getQueryData<ChatSummary[]>(queries.list.queryKey)?.[0]?.running).toBe(false);
  });

  test('notifies a list observer of loading, server data and merged metadata', async () => {
    const response = deferred<ChatSummary[]>();
    const queries = scope({ list: () => response.promise });
    const observer = new QueryObserver(queries.client, queries.list);
    const states: { loading: boolean; titles: string[] }[] = [];
    const unsubscribe = observer.subscribe(result => {
      states.push({ loading: result.isFetching, titles: result.data?.map(chat => chat.title) || [] });
    });
    try {
      expect(observer.getCurrentResult().isFetching).toBe(true);
      response.resolve([firstSummary]);
      await queries.client.fetchQuery(queries.list);
      queries.mergeChat(second);
      expect(observer.getCurrentResult().data).toEqual([secondSummary, firstSummary]);
      expect(states.some(state => state.loading)).toBe(true);
      expect(states.at(-1)?.titles).toEqual([second.title, first.title]);
    } finally { unsubscribe(); }
  });

  test('refresh after merge cancels a pre-mutation list read and cannot publish its late result', async () => {
    const oldResponse = deferred<ChatSummary[]>();
    const newResponse = deferred<ChatSummary[]>();
    let attempts = 0;
    let oldSignal: AbortSignal | undefined;
    const queries = scope({ list: signal => {
      if (++attempts === 1) {
        oldSignal = signal;
        return oldResponse.promise;
      }
      return newResponse.promise;
    } });
    const observer = new QueryObserver(queries.client, queries.list);
    const unsubscribe = observer.subscribe(() => undefined);
    try {
      queries.mergeChat(second);
      expect(observer.getCurrentResult().data).toEqual([secondSummary]);
      const refresh = observer.refetch({ cancelRefetch: true });
      expect(attempts).toBe(2);
      expect(oldSignal?.aborted).toBe(true);
      expect(observer.getCurrentResult().data).toEqual([secondSummary]);
      const updated = { ...secondSummary, title: '服务端最新标题', updatedAt: '2026-10-06T02:00:00Z' };
      newResponse.resolve([firstSummary, updated]);
      await refresh;
      expect(observer.getCurrentResult().data).toEqual([updated, firstSummary]);
      oldResponse.resolve([firstSummary]);
      await Promise.resolve();
      await Promise.resolve();
      expect(observer.getCurrentResult().data).toEqual([updated, firstSummary]);
      expect(queries.client.getQueryData<ChatSummary[]>(queries.list.queryKey)).toEqual([updated, firstSummary]);
    } finally { unsubscribe(); }
  });

  test('does not retry auth failures and allows only explicit refetch', async () => {
    let attempts = 0;
    const queries = scope({ list: async () => { attempts++; throw new Error('身份验证失败'); } });
    const observer = new QueryObserver(queries.client, queries.list);
    const unsubscribe = observer.subscribe(() => undefined);
    try {
      await expect(queries.client.fetchQuery(queries.list)).rejects.toThrow('身份验证失败');
      expect(attempts).toBe(1);
      expect(observer.getCurrentResult().isError).toBe(true);
      expect(observer.getCurrentResult().isFetching).toBe(false);
      await observer.refetch();
      expect(attempts).toBe(2);
    } finally { unsubscribe(); }
  });

  test('disables focus/reconnect/mount refetch and automatic retries', () => {
    const defaults = scope().client.getDefaultOptions();
    expect(defaults.queries).toMatchObject({
      retry: false, retryOnMount: false, refetchOnWindowFocus: false,
      refetchOnReconnect: false, refetchOnMount: false,
    });
    expect(defaults.mutations?.retry).toBe(false);
  });

  test('deduplicates concurrent fresh history reads', async () => {
    const response = deferred<SyncState>();
    let attempts = 0;
    const queries = scope({ detail: () => { attempts++; return response.promise; } });
    const firstRead = queries.readHistory(first.id);
    const concurrentRead = queries.readHistory(first.id);
    expect(attempts).toBe(1);
    response.resolve(state(first));
    expect(await firstRead).toEqual(await concurrentRead);
  });

  test('passes navigation cancellation through QueryClient to the API signal', async () => {
    const response = deferred<SyncState>();
    let receivedSignal: AbortSignal | undefined;
    const queries = scope({ detail: (_chatId, signal) => { receivedSignal = signal; return response.promise; } });
    const navigation = new AbortController();
    const read = queries.readHistory(first.id, navigation.signal).catch(error => error);
    navigation.abort();
    expect(receivedSignal?.aborted).toBe(true);
    expect(isCancelledError(await read)).toBe(true);
    response.resolve(state(first));
    await Promise.resolve();
    expect(queries.client.getQueryData(queries.history(first.id).queryKey)).toBeUndefined();
  });

  test('does not issue reads for already aborted navigation', async () => {
    let attempts = 0;
    const queries = scope({ detail: async () => { attempts++; return state(first); } });
    const navigation = new AbortController();
    navigation.abort();
    await expect(queries.readHistory(first.id, navigation.signal)).rejects.toThrow();
    expect(attempts).toBe(0);
  });

  test('cancelling one chat leaves a concurrent read of another chat intact', async () => {
    const response = deferred<SyncState>();
    const queries = scope({ detail: chatId => chatId === first.id ? response.promise
      : Promise.resolve(state(second)) });
    const navigation = new AbortController();
    const abandoned = queries.readHistory(first.id, navigation.signal).catch(error => error);
    const target = queries.readHistory(second.id);
    navigation.abort();
    expect(isCancelledError(await abandoned)).toBe(true);
    expect((await target).chat).toEqual(second);
    response.resolve(state(first));
  });

  test.each(['completed', 'cancelled'] as const)('forces fresh authoritative history after %s', async status => {
    let reads = 0;
    const queries = scope({ detail: async () => state(first,
      [{ id: 'answer', role: 'assistant', content: ++reads === 1 ? '旧历史' : '服务端最新内容', status }]) });
    expect(firstText(await queries.readHistory(first.id, undefined, false))).toBe('旧历史');
    expect(firstText(await queries.readHistory(first.id, undefined, false))).toBe('旧历史');
    expect(reads).toBe(1);
    const fresh = await queries.readHistory(first.id);
    expect(fresh).toMatchObject({ entries: [{ status }] });
    expect(firstText(fresh)).toBe('服务端最新内容');
    expect(reads).toBe(2);
  });

  test('fresh history failure rejects without replacing cached authoritative messages or retrying', async () => {
    let reads = 0;
    const queries = scope({ detail: async () => {
      if (++reads > 1) throw new Error('历史暂不可用');
      return state(first);
    } });
    await queries.readHistory(first.id);
    await expect(queries.readHistory(first.id)).rejects.toThrow('历史暂不可用');
    expect(reads).toBe(2);
    expect(queries.client.getQueryData<SyncState>(queries.history(first.id).queryKey)).toEqual(state(first));
  });

  test('isolates auth scopes even when chat query keys are identical', async () => {
    const oldScope = scope();
    const newScope = scope({ list: async () => [secondSummary], detail: async () => state(second) });
    expect(oldScope.client).not.toBe(newScope.client);
    await oldScope.client.fetchQuery(oldScope.list);
    expect(newScope.client.getQueryData(newScope.list.queryKey)).toBeUndefined();
    await newScope.client.fetchQuery(newScope.list);
    expect(oldScope.client.getQueryData<ChatSummary[]>(oldScope.list.queryKey)).toEqual([firstSummary]);
    expect(newScope.client.getQueryData<ChatSummary[]>(newScope.list.queryKey)).toEqual([secondSummary]);
    expect(oldScope.list.queryKey).toEqual(newScope.list.queryKey);
    expect(newScope.client.getQueryCache().getAll().map(query => query.queryKey)).toEqual([['chats', 'list']]);
  });

  test('retiring an auth scope aborts reads and clears both caches despite late API replies', async () => {
    const response = deferred<SyncState>();
    let receivedSignal: AbortSignal | undefined;
    const oldScope = scope({ detail: (_chatId, signal) => { receivedSignal = signal; return response.promise; } });
    await oldScope.client.fetchQuery(oldScope.list);
    const staleRead = oldScope.readHistory(first.id).catch(error => error);
    await oldScope.dispose();
    expect(receivedSignal?.aborted).toBe(true);
    expect(isCancelledError(await staleRead)).toBe(true);
    const newScope = scope({ list: async () => [secondSummary] });
    await newScope.client.fetchQuery(newScope.list);
    response.resolve(state(first));
    await Promise.resolve();
    expect(oldScope.client.getQueryCache().getAll()).toHaveLength(0);
    expect(newScope.client.getQueryData<ChatSummary[]>(newScope.list.queryKey)).toEqual([secondSummary]);
  });

  test('live facts overwrite stale list facts and clear running when the turn ends', async () => {
    const stale: ChatSummary = {
      ...firstSummary, running: true, pendingInteractionCount: 1,
      lastTurnStatus: 'streaming', lastCompletedAt: null,
    };
    const queries = scope({ list: async () => [stale] });
    await queries.client.fetchQuery(queries.list);

    queries.mergeChat(first, {
      messageCount: 2, running: true, pendingInteractionCount: 1,
      lastTurnStatus: 'streaming', lastCompletedAt: null,
    });
    expect(queries.client.getQueryData<ChatSummary[]>(queries.list.queryKey)).toEqual([
      { ...stale, messageCount: 2 },
    ]);

    queries.mergeChat(first, {
      messageCount: 2, running: false, pendingInteractionCount: 0,
      lastTurnStatus: 'completed', lastCompletedAt: '2026-10-06T02:00:00Z',
    });
    expect(queries.client.getQueryData<ChatSummary[]>(queries.list.queryKey)).toEqual([{
      ...firstSummary, messageCount: 2, running: false, pendingInteractionCount: 0,
      lastTurnStatus: 'completed', lastCompletedAt: '2026-10-06T02:00:00Z',
    }]);
  });

  test('summaryFacts reads live running and the last successfully completed turn', () => {
    const live = state(
      first,
      [{ id: 'u', role: 'user', content: '问题' }, { id: 'a', role: 'assistant', content: '回答' }],
      {
        running: true,
        activeTurn: { turnId: 'turn:2', status: 'accepting' },
        turns: [
          {
            turnId: 'turn:1', status: 'completed', startedAt: '2026-10-06T00:00:00Z',
            completedAt: '2026-10-06T00:01:00Z', error: null, interactionStatus: 'none',
          },
          {
            turnId: 'turn:2', status: 'accepting', startedAt: '2026-10-06T00:02:00Z',
            completedAt: null, error: null, interactionStatus: 'none',
          },
        ],
      },
    );
    expect(summaryFacts(live)).toEqual({
      messageCount: 2, running: true, pendingInteractionCount: 0,
      lastTurnStatus: 'accepting', lastCompletedAt: '2026-10-06T00:01:00Z',
    });

    live.running = false;
    live.turns[1] = { ...live.turns[1]!, status: 'completed', completedAt: '2026-10-06T00:03:00Z' };
    live.activeTurn = { turnId: null, status: null };
    expect(summaryFacts(live)).toMatchObject({
      running: false, lastTurnStatus: 'completed', lastCompletedAt: '2026-10-06T00:03:00Z',
    });
  });
});
