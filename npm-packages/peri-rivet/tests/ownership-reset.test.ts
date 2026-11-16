import { describe, expect, test } from 'bun:test';
import { clearSessionOwnership, readSessionOwnership } from '@peri-code/sdk';
import type { ChatOwnership } from '../src/rivet/chat-ports.js';
import { resetOwnership } from '../src/rivet/ownership-reset.js';

interface RecordingOwnership {
  readonly ownership: ChatOwnership;
  readonly reads: string[];
  readonly removals: string[];
  readonly values: Map<string, unknown>;
}

/** Storage that records which keys were touched; `values` starts from the caller's seed. */
function recordingOwnership(values: Map<string, unknown> = new Map()): RecordingOwnership {
  const reads: string[] = [];
  const removals: string[] = [];
  const ownership: ChatOwnership = {
    async getItem(key: string) {
      reads.push(key);
      return values.get(key) ?? null;
    },
    async setItem(key: string, value: unknown) {
      values.set(key, value);
    },
    async removeItem(key: string) {
      removals.push(key);
      values.delete(key);
    },
  };
  return { ownership, reads, removals, values };
}

const SESSION_ID = 'session_alpha';

/**
 * Discovers the SDK's ownership key for a session without duplicating its encoding.
 *
 * The SDK's own reader is the authority for the key, so the test seeds a value through a storage
 * whose `getItem` remembers which key it was asked for.
 */
async function ownershipKey(sessionId: string): Promise<string> {
  const seen: string[] = [];
  const probe: ChatOwnership = {
    async getItem(key: string) {
      seen.push(key);
      return null;
    },
    async setItem() {},
    async removeItem() {},
  };
  await readSessionOwnership(probe, sessionId);
  const [key] = seen;
  if (key === undefined) {
    throw new Error('readSessionOwnership read no key');
  }
  return key;
}

describe('ownership reset', () => {
  test('clears a stale record and reports it as cleared', async () => {
    const key = await ownershipKey(SESSION_ID);
    const storage = recordingOwnership(new Map([[key, { sessionId: SESSION_ID, owner: 'owner_previous' }]]));

    const outcome = await resetOwnership(storage.ownership, SESSION_ID);

    expect(outcome).toBe('cleared');
    expect(storage.reads).toEqual([key]);
    expect(storage.removals).toEqual([key]);
    expect(storage.values.size).toBe(0);
    expect(await readSessionOwnership(storage.ownership, SESSION_ID)).toBeNull();
  });

  test('reports an absent record without removing anything', async () => {
    const storage = recordingOwnership();

    const outcome = await resetOwnership(storage.ownership, SESSION_ID);

    expect(outcome).toBe('absent');
    expect(storage.reads).toHaveLength(1);
    expect(storage.removals).toHaveLength(0);
  });

  test('resets only the requested session', async () => {
    const mine = await ownershipKey(SESSION_ID);
    const other = await ownershipKey('session_beta');
    const storage = recordingOwnership(
      new Map([
        [mine, { sessionId: SESSION_ID, owner: 'owner_previous' }],
        [other, { sessionId: 'session_beta', owner: 'owner_other' }],
      ]),
    );

    await resetOwnership(storage.ownership, SESSION_ID);

    expect([...storage.values.keys()]).toEqual([other]);
  });

  test('a malformed record fails closed instead of being treated as absent', async () => {
    const key = await ownershipKey(SESSION_ID);
    const storage = recordingOwnership(
      new Map([[key, { sessionId: 'someone_else', owner: 'owner_previous' }]]),
    );

    await expect(resetOwnership(storage.ownership, SESSION_ID)).rejects.toThrow('sessionId mismatch');
    expect(storage.removals).toHaveLength(0);
    expect(storage.values.size).toBe(1);
  });

  test('a failing read rejects without removing', async () => {
    const storage = recordingOwnership();
    const failing: ChatOwnership = {
      ...storage.ownership,
      getItem: async () => {
        throw new Error('store unavailable');
      },
    };

    await expect(resetOwnership(failing, SESSION_ID)).rejects.toThrow('store unavailable');
    expect(storage.removals).toHaveLength(0);
  });

  test('clearing uses the same key the reader returned', async () => {
    const key = await ownershipKey(SESSION_ID);
    const storage = recordingOwnership(new Map([[key, { sessionId: SESSION_ID, owner: 'owner_previous' }]]));

    await resetOwnership(storage.ownership, SESSION_ID);
    await clearSessionOwnership(storage.ownership, SESSION_ID);

    expect(storage.removals).toEqual([key, key]);
  });
});
