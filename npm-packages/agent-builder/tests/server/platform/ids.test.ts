import { describe, expect, test } from 'bun:test';
import { z } from 'zod';
import { SESSION_INLINE_AGENT_ID } from '../../../server/db/migrate';
import { isUuidV7, newPlatformId } from '../../../server/platform/ids';

describe('platform ids', () => {
  test('mints standard UUIDv7 values accepted by the public UUID contract', () => {
    const id = newPlatformId();

    expect(id).toHaveLength(36);
    expect(id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    expect(isUuidV7(id)).toBe(true);
    expect(z.uuid().safeParse(id).success).toBe(true);
  });

  test('orders ids by their timestamp prefix', () => {
    const earlier = newPlatformId(1_700_000_000_000);
    const later = newPlatformId(1_700_000_000_001);

    expect(later > earlier).toBe(true);
  });

  test('keeps ids minted in the same millisecond strictly increasing', () => {
    const ids = Array.from({ length: 256 }, () => newPlatformId(1_700_000_000_000));

    expect(new Set(ids).size).toBe(ids.length);
    expect([...ids].sort()).toEqual(ids);
  });

  test('recognizes the inline session agent and rejects prefixed ids', () => {
    expect(isUuidV7(SESSION_INLINE_AGENT_ID)).toBe(true);
    expect(isUuidV7('sess_1234')).toBe(false);
    expect(isUuidV7(crypto.randomUUID())).toBe(false);
  });
});
