import { afterEach, describe, expect, test } from 'bun:test';
import { isCancelledError, QueryObserver } from '@tanstack/react-query';
import { createChatQueries } from '../web/chat/queries';
import type { ChatReadApi } from '../web/chat/queries';
import type { Chat, ChatDetail } from '../web/chat/types';

const first: Chat = { id: 'first', title: '第一段思考', updatedAt: '2026-10-06T00:00:00Z' };
const second: Chat = { id: 'second', title: '新的思考', updatedAt: '2026-10-06T01:00:00Z' };
const scopes: ReturnType<typeof createChatQueries>[] = [];

function deferred<Value>() {
  let resolve!: (value: Value) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<Value>((accept, fail) => { resolve = accept; reject = fail; });
  return { promise, resolve, reject };
}

function scope(overrides: Partial<ChatReadApi> = {}) {
  const queries = createChatQueries({
    list: async () => [first],
    detail: async () => ({ chat: first, messages: [] }),
    ...overrides,
  });
  scopes.push(queries);
  return queries;
}

afterEach(async () => {
  await Promise.all(scopes.splice(0).map(queries => queries.dispose()));
});

describe('chat QueryClient read ownership', () => {
  test('replica state replaces authoritative detail cache without HTTP refetch or delta accumulation', async () => {
    let reads = 0;
    const queries = scope({ detail: async () => { reads++; return { chat: first, messages: [] }; } });
    await queries.readHistory(first.id);
    const detail: ChatDetail = { chat: { ...first, title: '同步标题' }, messages: [{ id: 'answer', role: 'assistant',
      content: '完整投影', createdAt: first.updatedAt, status: 'completed' }] };
    queries.setHistory(detail);
    queries.mergeChat(detail.chat);
    expect(await queries.readHistory(first.id, undefined, false)).toEqual(detail);
    expect(reads).toBe(1);
    expect(queries.client.getQueryData<Chat[]>(queries.list.queryKey)).toEqual([detail.chat]);
  });
  test('deduplicates concurrent list requests and sorts the server result', async () => {
    const response = deferred<Chat[]>();
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
    response.resolve([first, second]);
    expect(await firstRead).toEqual([second, first]);
    expect(await concurrentRead).toEqual([second, first]);
  });

  test('merges metadata in the query cache without duplicating identities', async () => {
    const queries = scope();
    await queries.client.fetchQuery(queries.list);
    queries.mergeChat(second);
    queries.mergeChat({ ...first, title: '更新的标题', updatedAt: '2026-10-06T02:00:00Z' });
    expect(queries.client.getQueryData<Chat[]>(queries.list.queryKey)).toEqual([
      { ...first, title: '更新的标题', updatedAt: '2026-10-06T02:00:00Z' }, second,
    ]);
  });

  test('notifies a list observer of loading, server data and merged metadata', async () => {
    const response = deferred<Chat[]>();
    const queries = scope({ list: () => response.promise });
    const observer = new QueryObserver(queries.client, queries.list);
    const states: { loading: boolean; titles: string[] }[] = [];
    const unsubscribe = observer.subscribe(result => {
      states.push({ loading: result.isFetching, titles: result.data?.map(chat => chat.title) || [] });
    });
    try {
      expect(observer.getCurrentResult().isFetching).toBe(true);
      response.resolve([first]);
      await queries.client.fetchQuery(queries.list);
      queries.mergeChat(second);
      expect(observer.getCurrentResult().data).toEqual([second, first]);
      expect(states.some(state => state.loading)).toBe(true);
      expect(states.at(-1)?.titles).toEqual([second.title, first.title]);
    } finally { unsubscribe(); }
  });

  test('refresh after merge cancels a pre-mutation list read and cannot publish its late result', async () => {
    const oldResponse = deferred<Chat[]>();
    const newResponse = deferred<Chat[]>();
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
      expect(observer.getCurrentResult().data).toEqual([second]);
      const refresh = observer.refetch({ cancelRefetch: true });
      expect(attempts).toBe(2);
      expect(oldSignal?.aborted).toBe(true);
      expect(observer.getCurrentResult().data).toEqual([second]);
      const updated = { ...second, title: '服务端最新标题', updatedAt: '2026-10-06T02:00:00Z' };
      newResponse.resolve([first, updated]);
      await refresh;
      expect(observer.getCurrentResult().data).toEqual([updated, first]);
      oldResponse.resolve([first]);
      await Promise.resolve();
      await Promise.resolve();
      expect(observer.getCurrentResult().data).toEqual([updated, first]);
      expect(queries.client.getQueryData<Chat[]>(queries.list.queryKey)).toEqual([updated, first]);
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
    const response = deferred<ChatDetail>();
    let attempts = 0;
    const queries = scope({ detail: () => { attempts++; return response.promise; } });
    const firstRead = queries.readHistory(first.id);
    const concurrentRead = queries.readHistory(first.id);
    expect(attempts).toBe(1);
    response.resolve({ chat: first, messages: [] });
    expect(await firstRead).toEqual(await concurrentRead);
  });

  test('passes navigation cancellation through QueryClient to the API signal', async () => {
    const response = deferred<ChatDetail>();
    let receivedSignal: AbortSignal | undefined;
    const queries = scope({ detail: (_chatId, signal) => { receivedSignal = signal; return response.promise; } });
    const navigation = new AbortController();
    const read = queries.readHistory(first.id, navigation.signal).catch(error => error);
    navigation.abort();
    expect(receivedSignal?.aborted).toBe(true);
    expect(isCancelledError(await read)).toBe(true);
    response.resolve({ chat: first, messages: [] });
    await Promise.resolve();
    expect(queries.client.getQueryData(queries.history(first.id).queryKey)).toBeUndefined();
  });

  test('does not issue reads for already aborted navigation', async () => {
    let attempts = 0;
    const queries = scope({ detail: async () => { attempts++; return { chat: first, messages: [] }; } });
    const navigation = new AbortController();
    navigation.abort();
    await expect(queries.readHistory(first.id, navigation.signal)).rejects.toThrow();
    expect(attempts).toBe(0);
  });

  test('cancelling one chat leaves a concurrent read of another chat intact', async () => {
    const response = deferred<ChatDetail>();
    const queries = scope({ detail: chatId => chatId === first.id ? response.promise
      : Promise.resolve({ chat: second, messages: [] }) });
    const navigation = new AbortController();
    const abandoned = queries.readHistory(first.id, navigation.signal).catch(error => error);
    const target = queries.readHistory(second.id);
    navigation.abort();
    expect(isCancelledError(await abandoned)).toBe(true);
    expect((await target).chat).toEqual(second);
    response.resolve({ chat: first, messages: [] });
  });

  test.each(['completed', 'cancelled'] as const)('forces fresh authoritative history after %s', async status => {
    let reads = 0;
    const queries = scope({ detail: async () => ({
      chat: first,
      messages: [{ id: 'answer', role: 'assistant', content: ++reads === 1 ? '旧历史' : '服务端最新内容',
        createdAt: first.updatedAt, status }],
    }) });
    expect((await queries.readHistory(first.id, undefined, false)).messages[0].content).toBe('旧历史');
    expect((await queries.readHistory(first.id, undefined, false)).messages[0].content).toBe('旧历史');
    expect(reads).toBe(1);
    expect((await queries.readHistory(first.id)).messages[0]).toMatchObject({ content: '服务端最新内容', status });
    expect(reads).toBe(2);
  });

  test('fresh history failure rejects without replacing cached authoritative messages or retrying', async () => {
    let reads = 0;
    const queries = scope({ detail: async () => {
      if (++reads > 1) throw new Error('历史暂不可用');
      return { chat: first, messages: [] };
    } });
    await queries.readHistory(first.id);
    await expect(queries.readHistory(first.id)).rejects.toThrow('历史暂不可用');
    expect(reads).toBe(2);
    expect(queries.client.getQueryData<ChatDetail>(queries.history(first.id).queryKey)).toEqual({ chat: first, messages: [] });
  });

  test('isolates auth scopes even when chat query keys are identical', async () => {
    const oldScope = scope();
    const newScope = scope({ list: async () => [second], detail: async () => ({ chat: second, messages: [] }) });
    expect(oldScope.client).not.toBe(newScope.client);
    await oldScope.client.fetchQuery(oldScope.list);
    expect(newScope.client.getQueryData(newScope.list.queryKey)).toBeUndefined();
    await newScope.client.fetchQuery(newScope.list);
    expect(oldScope.client.getQueryData<Chat[]>(oldScope.list.queryKey)).toEqual([first]);
    expect(newScope.client.getQueryData<Chat[]>(newScope.list.queryKey)).toEqual([second]);
    expect(oldScope.list.queryKey).toEqual(newScope.list.queryKey);
    expect(newScope.client.getQueryCache().getAll().map(query => query.queryKey)).toEqual([['chats', 'list']]);
  });

  test('retiring an auth scope aborts reads and clears both caches despite late API replies', async () => {
    const response = deferred<ChatDetail>();
    let receivedSignal: AbortSignal | undefined;
    const oldScope = scope({ detail: (_chatId, signal) => { receivedSignal = signal; return response.promise; } });
    await oldScope.client.fetchQuery(oldScope.list);
    const staleRead = oldScope.readHistory(first.id).catch(error => error);
    await oldScope.dispose();
    expect(receivedSignal?.aborted).toBe(true);
    expect(isCancelledError(await staleRead)).toBe(true);
    const newScope = scope({ list: async () => [second] });
    await newScope.client.fetchQuery(newScope.list);
    response.resolve({ chat: first, messages: [] });
    await Promise.resolve();
    expect(oldScope.client.getQueryCache().getAll()).toHaveLength(0);
    expect(newScope.client.getQueryData<Chat[]>(newScope.list.queryKey)).toEqual([second]);
  });
});
