// 必须在 query-core 求值前安装 window：其 `isServer` 在模块加载时求值，
// 否则 Bun 测试环境会被判定为 server，refetchInterval 定时器不会调度。
import '../../test/install-dom';
import { afterEach, describe, expect, jest, test } from 'bun:test';
import {
  LIST_POLL_IDLE_INTERVAL_MS, LIST_POLL_INTERVAL_MS, createChatQueries, historyKey,
  listRefetchInterval,
} from './queries';
import type { ChatDisplayState, ChatSummary } from './types';

function summary(sessionId: string, title = sessionId, updatedAt = '2026-10-06T00:00:00Z'): ChatSummary {
  return {
    id: sessionId, sessionId, title, updatedAt,
    cwd: '/workspace', messageCount: 0, createdAt: '2026-10-05T00:00:00Z',
  };
}

function state(sessionId: string, bindingRevision: number, extra: Partial<ChatDisplayState> = {}): ChatDisplayState {
  return {
    chat: { id: sessionId, title: sessionId, updatedAt: '2026-10-06T00:00:00Z' },
    sessionId, bindingRevision, running: false, executionBlocked: false, readiness: 'ready',
    entries: [], toolCalls: [], turns: [], activeTurn: { turnId: null, status: null },
    session: {}, tasks: [], plans: { current: [], byTurn: {} },
    interactions: [], pendingInteractions: [], inputQueue: null,
    ...extra,
  };
}

const scopes: ReturnType<typeof createChatQueries>[] = [];

function scope(list = async () => [summary('first')]) {
  const queries = createChatQueries({ list });
  scopes.push(queries);
  return queries;
}

afterEach(async () => {
  jest.useRealTimers();
  await Promise.all(scopes.splice(0).map(queries => queries.dispose()));
});

describe('chat directory cache', () => {
  test('list query only carries directory facts and sorts by updatedAt', async () => {
    const queries = scope(async () => [
      summary('first', '第一段思考', '2026-10-06T00:00:00Z'),
      summary('second', '新的思考', '2026-10-06T01:00:00Z'),
    ]);
    const chats = await queries.client.fetchQuery(queries.list);
    expect(chats.map(chat => chat.id)).toEqual(['second', 'first']);
    // 运行态字段不得出现在目录条目上。
    expect(chats[0]).not.toHaveProperty('running');
    expect(chats[0]).not.toHaveProperty('pendingInteractionCount');
  });

  test('list request deduplicates concurrent reads', async () => {
    let requests = 0;
    let resolve!: (value: ChatSummary[]) => void;
    const pending = new Promise<ChatSummary[]>(accept => { resolve = accept; });
    const queries = scope(() => { requests += 1; return pending; });
    const firstRead = queries.client.fetchQuery(queries.list);
    const concurrentRead = queries.client.fetchQuery(queries.list);
    expect(requests).toBe(1);
    resolve([summary('first')]);
    expect(await firstRead).toEqual([summary('first')]);
    expect(await concurrentRead).toEqual([summary('first')]);
  });
});

describe('replica cache by chatId + bindingRevision', () => {
  test('writes the snapshot under the binding generation and reads it back', () => {
    const queries = scope();
    const value = state('session-1', 3);
    queries.writeHistory(value);
    expect(queries.readHistory('session-1', 3)).toEqual(value);
    expect(queries.readHistory('session-1', 2)).toBeUndefined();
  });

  test('a new binding generation drops the previous generation cache', () => {
    const queries = scope();
    queries.writeHistory(state('session-1', 1));
    queries.writeHistory(state('session-1', 2));
    expect(queries.readHistory('session-1', 1)).toBeUndefined();
    expect(queries.readHistory('session-1', 2)?.bindingRevision).toBe(2);
  });

  test('unsupported readiness is cached as a distinct state, not an empty ready state', () => {
    const queries = scope();
    const unsupported = state('session-1', 1, {
      readiness: 'unsupported', entries: [], running: false,
    });
    queries.writeHistory(unsupported);
    const cached = queries.readHistory('session-1', 1);
    expect(cached?.readiness).toBe('unsupported');
    expect(cached?.entries).toEqual([]);
  });

  test('history key includes the binding revision', () => {
    expect(historyKey('session-1', 4)).toEqual(['chats', 'history', 'session-1', 4]);
  });
});

describe('list polling', () => {
  test('hidden tabs do not poll; visible tabs use idle cadence without running fields', () => {
    expect(listRefetchInterval([summary('a')], { visible: false })).toBe(false);
    expect(listRefetchInterval([summary('a')], { visible: true })).toBe(LIST_POLL_IDLE_INTERVAL_MS);
  });

  test('local activity raises the cadence for a bounded window', () => {
    expect(listRefetchInterval([], { visible: true, activeUntil: 2_000, now: 1_000 })).toBe(LIST_POLL_INTERVAL_MS);
    expect(listRefetchInterval([], { visible: true, activeUntil: 500, now: 1_000 })).toBe(LIST_POLL_IDLE_INTERVAL_MS);
  });
});
