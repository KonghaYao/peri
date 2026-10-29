import { describe, expect, test } from 'bun:test';
import { ChatHandleTable, type ChatHandleRecord } from './handles';

const chat = (id: string) => ({ id, title: id, updatedAt: '2026-10-20T00:00:00Z' });

function record(id: string, address = `handle-${id}`, handleId: string | null = null): ChatHandleRecord {
  return { address, handleId, chat: chat(id) };
}

describe('ChatHandleTable', () => {
  test('an empty table has no records', () => {
    const table = new ChatHandleTable();
    expect(table.get('session-1')).toBeUndefined();
  });

  test('set/get/delete/clear keep the table keyed by session id', () => {
    const table = new ChatHandleTable();
    table.set('session-1', record('session-1'));
    expect(table.get('session-1')?.address).toBe('handle-session-1');
    table.delete('session-1');
    expect(table.get('session-1')).toBeUndefined();

    table.set('session-1', record('session-1'));
    table.clear();
    expect(table.get('session-1')).toBeUndefined();
  });

  test('rebind moves the record only when the old entry points at the same address', () => {
    const table = new ChatHandleTable();
    table.set('session-1', record('session-1', 'handle-1', 'handle-1'));
    table.followRebind({
      previousChatId: 'session-1', address: 'handle-1', handleId: 'handle-1', chat: chat('session-2'),
    });
    expect(table.get('session-1')).toBeUndefined();
    expect(table.get('session-2')?.address).toBe('handle-1');
  });

  test('rebind keeps a differently addressed record for the previous id', () => {
    const table = new ChatHandleTable();
    table.set('session-1', record('session-1', 'handle-1', 'handle-1'));
    table.followRebind({
      previousChatId: 'session-1', address: 'handle-2', handleId: 'handle-2', chat: chat('session-2'),
    });
    // 旧记录属于另一条连接，不能被这次换绑误删。
    expect(table.get('session-1')?.address).toBe('handle-1');
    expect(table.get('session-2')?.address).toBe('handle-2');
  });

  test('rebind without a previous record still registers the new identity', () => {
    const table = new ChatHandleTable();
    table.followRebind({
      previousChatId: 'session-1', address: 'session-9', handleId: null, chat: chat('session-9'),
    });
    expect(table.get('session-9')).toMatchObject({ address: 'session-9', handleId: null });
  });
});
