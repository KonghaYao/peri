// 必须在 query-core 求值前安装 window：其 `isServer` 在模块加载时求值，
// 否则 Bun 测试环境会被判定为 server，refetchInterval 定时器不会调度。
import '../../test/install-dom';
import { afterEach, describe, expect, jest, test } from 'bun:test';
import {
  LIST_POLL_IDLE_INTERVAL_MS, LIST_POLL_INTERVAL_MS, createChatQueries,
  listRefetchInterval,
} from './queries';
import type { ChatSummary } from './types';

function summary(sessionId: string, title = sessionId, updatedAt = '2026-10-06T00:00:00Z'): ChatSummary {
  return {
    id: sessionId, sessionId, title, updatedAt,
    cwd: '/workspace', messageCount: 0, createdAt: '2026-10-05T00:00:00Z',
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
