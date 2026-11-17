import { describe, expect, test } from 'bun:test';
import { createMemoryChatOwnership } from '../src/rivet/index.js';

describe('createMemoryChatOwnership', () => {
  test('round-trips values and returns null for missing keys', async () => {
    const ownership = createMemoryChatOwnership();
    const record = { owner: 'chat:rt_1', revision: 3 };

    expect(await ownership.getItem('session/1')).toBeNull();
    await ownership.setItem('session/1', record);
    expect(await ownership.getItem('session/1')).toEqual(record);
  });

  test('isolates stored and returned values with structuredClone', async () => {
    const ownership = createMemoryChatOwnership();
    const written = { owner: 'chat:rt_1', nested: { revision: 1 } };
    await ownership.setItem('session/1', written);

    written.nested.revision = 99;
    expect(await ownership.getItem('session/1')).toEqual({
      owner: 'chat:rt_1',
      nested: { revision: 1 },
    });

    const read = (await ownership.getItem('session/1')) as { nested: { revision: number } };
    read.nested.revision = 42;
    expect(await ownership.getItem('session/1')).toEqual({
      owner: 'chat:rt_1',
      nested: { revision: 1 },
    });
  });

  test('removeItem deletes the record', async () => {
    const ownership = createMemoryChatOwnership();
    await ownership.setItem('session/1', { owner: 'chat:rt_1' });
    await ownership.removeItem('session/1');
    expect(await ownership.getItem('session/1')).toBeNull();
    // 删除不存在的 key 保持幂等。
    await ownership.removeItem('session/1');
  });

  test('each instance keeps its own records', async () => {
    const first = createMemoryChatOwnership();
    const second = createMemoryChatOwnership();
    await first.setItem('session/1', { owner: 'chat:rt_1' });
    expect(await second.getItem('session/1')).toBeNull();
  });
});
