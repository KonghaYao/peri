import { describe, expect, test } from 'bun:test';
import { PeriError } from '../src/contracts/errors.js';
import {
  createHostResourceOwner,
  type HostClosable,
  type HostResourceOwner,
} from '../src/rivet/host-resource-owner.js';

interface ControlledClose extends HostClosable {
  readonly count: number;
  resolve(): void;
  reject(error: unknown): void;
}

/** A close that stays pending until the test settles it, recording each real close attempt. */
function controlledClose(): ControlledClose {
  let attempts = 0;
  let settling: { resolve: () => void; reject: (error: unknown) => void } | undefined;
  const queue = () => {
    let resolve!: () => void;
    let reject!: (error: unknown) => void;
    const promise = new Promise<void>((res, rej) => {
      resolve = res;
      reject = rej;
    });
    settling = { resolve, reject };
    return promise;
  };
  let pending: Promise<void> | undefined;
  return {
    get count() {
      return attempts;
    },
    close: () => {
      attempts += 1;
      pending ??= queue();
      return pending;
    },
    resolve: () => {
      settling?.resolve();
      pending = undefined;
      settling = undefined;
    },
    reject: (error: unknown) => {
      settling?.reject(error);
      pending = undefined;
      settling = undefined;
    },
  };
}

async function openValue(
  owner: HostResourceOwner,
  key: readonly string[],
  ref: object,
  value: string,
  resource?: HostClosable,
): Promise<void> {
  await owner.open(key, ref, async (track) => {
    if (resource !== undefined) {
      track(resource);
    }
    return value;
  });
}

function incomplete(message = 'not confirmed'): PeriError {
  return new PeriError({ code: 'close_incomplete', phase: 'session', message });
}

describe('host resource owner', () => {
  test('reuses an active lease for the same activation and retires its observers once', async () => {
    const owner = createHostResourceOwner();
    const key = ['chat', 'one'];
    const ref = {};
    let creates = 0;
    let detaches = 0;

    const first = await owner.open(key, ref, async () => {
      creates += 1;
      return 'chat-handle';
    });
    const second = await owner.open(key, ref, async () => {
      creates += 1;
      return 'other-handle';
    });
    first.onRetire(() => {
      detaches += 1;
    });

    expect(second.value).toBe('chat-handle');
    expect(creates).toBe(1);
    owner.retire(ref);
    owner.retire(ref);
    expect(detaches).toBe(1);
    expect(first.retired).toBe(true);
    expect(owner.lease(ref)?.value).toBe('chat-handle');
    await first.close();
    expect(owner.lease(ref)).toBeUndefined();
  });

  test('a new activation waits for the old lease and a late old close cannot delete it', async () => {
    const owner = createHostResourceOwner();
    const key = ['chat', 'late'];
    const refA = {};
    const refB = {};
    const oldClose = controlledClose();
    const leaseA = await owner.open(key, refA, async (track) => {
      track(oldClose);
      return 'A';
    });

    let createdB = 0;
    const openingB = owner.open(key, refB, async () => {
      createdB += 1;
      return 'B';
    });
    await Bun.sleep(0);
    expect(createdB).toBe(0);

    oldClose.resolve();
    await leaseA.close();
    const leaseB = await openingB;
    expect(createdB).toBe(1);
    expect(leaseB.value).toBe('B');
    expect(owner.lease(refA)).toBeUndefined();

    // A stale lease close already completed; repeating it must not remove the replacement record.
    await leaseA.close();
    expect(owner.lease(refB)?.value).toBe('B');
    expect(owner.pending()).toHaveLength(1);
    const pending = owner.pending()[0];
    if (pending === undefined) throw new Error('the replacement activation is not pending');
    expect(pending.activationRef).not.toBe(refB);
    expect(owner.lease(pending.activationRef)?.value).toBe('B');
  });

  test('an unconfirmed old close blocks the new activation until the host retry succeeds', async () => {
    const owner = createHostResourceOwner();
    const key = ['chat', 'blocked'];
    const refA = {};
    const refB = {};
    let closeAttempts = 0;
    const resource: HostClosable = {
      close: async () => {
        closeAttempts += 1;
        if (closeAttempts === 1) {
          throw incomplete('sdk close failed');
        }
      },
    };
    await openValue(owner, key, refA, 'A', resource);

    let createdB = 0;
    const rejected = await owner
      .open(key, refB, async () => {
        createdB += 1;
        return 'B';
      })
      .catch((error: unknown) => error);
    expect(rejected).toBeInstanceOf(PeriError);
    expect(createdB).toBe(0);
    expect(owner.pending()).toHaveLength(1);
    const pending = owner.pending()[0];
    if (pending === undefined) throw new Error('the failed activation is not pending');
    expect(pending.activationRef).not.toBe(refA);
    // The host handle must not carry activation vars, targets or credentials.
    expect(JSON.stringify(pending.activationRef)).toBe('{}');

    const lease = owner.lease(pending.activationRef);
    if (lease === undefined) throw new Error('the pending host handle is not retryable');
    await lease.close();
    expect(owner.pending()).toHaveLength(0);
    expect(closeAttempts).toBe(2);

    const leaseB = await owner.open(key, refB, async () => {
      createdB += 1;
      return 'B';
    });
    expect(createdB).toBe(1);
    expect(leaseB.value).toBe('B');
  });

  test('closeAll single-flights one close and a timeout keeps the real pending promise', async () => {
    const owner = createHostResourceOwner();
    const key = ['chat', 'timeout'];
    const resource = controlledClose();
    await openValue(owner, key, {}, 'A', resource);

    const [first, second] = await Promise.all([
      owner.closeAll({ timeoutMs: 20 }),
      owner.closeAll({ timeoutMs: 20 }),
    ]);
    expect(resource.count).toBe(1);
    expect(first.complete).toBe(false);
    expect(second.complete).toBe(false);
    expect(first.pending).toBe(1);

    const third = await owner.closeAll({ timeoutMs: 5 });
    expect(third.complete).toBe(false);
    expect(third.failures[0]?.error).toBeInstanceOf(PeriError);
    expect(resource.count).toBe(1);

    resource.resolve();
    const retried = await owner.closeAll({ timeoutMs: 1_000 });
    expect(retried.complete).toBe(true);
    // The retry observed the same close promise instead of starting a second database close.
    expect(resource.count).toBe(1);
  });

  test('unknown close errors are surfaced once instead of retried', async () => {
    const owner = createHostResourceOwner();
    let attempts = 0;
    const failure = new Error('database exploded');
    await openValue(owner, ['chat', 'unknown'], {}, 'A', {
      close: async () => {
        attempts += 1;
        throw failure;
      },
    });

    const report = await owner.closeAll({ timeoutMs: 1_000, maxAttempts: 3 });
    expect(report.complete).toBe(false);
    expect(report.pending).toBe(1);
    expect(attempts).toBe(1);
    expect(report.failures).toHaveLength(1);
    expect(report.failures[0]?.error).toBe(failure);
  });

  test('retryable incomplete closes are bounded by maxAttempts', async () => {
    const owner = createHostResourceOwner();
    let attempts = 0;
    await openValue(owner, ['chat', 'retry-bound'], {}, 'A', {
      close: async () => {
        attempts += 1;
        throw incomplete(`attempt ${attempts}`);
      },
    });

    const report = await owner.closeAll({ timeoutMs: 1_000, maxAttempts: 2 });
    expect(report.complete).toBe(false);
    expect(attempts).toBe(2);
    expect(report.pending).toBe(1);
  });

  test('ordered close stops at a failed earlier resource and retries the rest', async () => {
    const owner = createHostResourceOwner();
    let chatAttempts = 0;
    let databaseAttempts = 0;
    const chat: HostClosable = {
      close: async () => {
        chatAttempts += 1;
        if (chatAttempts === 1) {
          throw incomplete('chat close failed');
        }
      },
    };
    const database: HostClosable = {
      close: async () => {
        databaseAttempts += 1;
      },
    };
    await owner.open(['chat', 'ordered'], {}, async (track) => {
      track(database, 1);
      track(chat, 0);
      return 'A';
    });

    const report = await owner.closeAll({ timeoutMs: 1_000, maxAttempts: 2 });
    expect(report.complete).toBe(true);
    expect(chatAttempts).toBe(2);
    // The first round failed at chat, so the database was not touched until chat confirmed.
    expect(databaseAttempts).toBe(1);
  });

  test('a failed construction keeps a tracked resource whose close also failed', async () => {
    const owner = createHostResourceOwner();
    let closes = 0;
    const construction = new Error('createChatSession failed');
    const rejected = await owner
      .open(['chat', 'construction'], {}, async (track) => {
        track({
          close: async () => {
            closes += 1;
            if (closes === 1) {
              throw new Error('database close failed');
            }
          },
        });
        throw construction;
      })
      .catch((error: unknown) => error);

    expect(rejected).toBe(construction);
    expect(owner.pending()).toHaveLength(1);
    const report = await owner.closeAll({ timeoutMs: 1_000, maxAttempts: 2 });
    expect(report.complete).toBe(true);
    expect(closes).toBe(2);
  });

  test('beginShutdown retires observers and rejects new opens', async () => {
    const owner = createHostResourceOwner();
    const ref = {};
    let detaches = 0;
    const lease = await owner.open(['chat', 'shutdown'], ref, async () => 'A');
    lease.onRetire(() => {
      detaches += 1;
    });

    owner.beginShutdown();
    expect(detaches).toBe(1);
    expect(lease.retired).toBe(true);
    expect(owner.shuttingDown).toBe(true);
    const rejected = await owner
      .open(['chat', 'shutdown'], {}, async () => 'B')
      .catch((error: unknown) => error);
    expect(rejected).toBeInstanceOf(PeriError);
    expect((rejected as PeriError).code).toBe('closed');
    expect(owner.pending()).toHaveLength(1);

    const report = await owner.closeAll({ timeoutMs: 1_000 });
    expect(report.complete).toBe(true);
  });

  test('rejects invalid closeAll bounds without touching records', async () => {
    const owner = createHostResourceOwner();
    await expect(owner.closeAll({ timeoutMs: -1 })).rejects.toBeInstanceOf(PeriError);
    await expect(owner.closeAll({ timeoutMs: 1, maxAttempts: 0 })).rejects.toBeInstanceOf(PeriError);
  });

  test('a retired activation cannot reopen after a confirmed close removed its record', async () => {
    const owner = createHostResourceOwner();
    const key = ['sbx', 'retired-identity'];
    const ref = {};
    await openValue(owner, key, ref, 'A');

    owner.retire(ref);
    const lease = owner.lease(ref);
    if (lease === undefined) throw new Error('the retired lease is not reachable before its close');
    await lease.close();
    expect(owner.pending()).toHaveLength(0);
    expect(owner.lease(ref)).toBeUndefined();

    let creates = 0;
    const rejected = await owner
      .open(key, ref, async () => {
        creates += 1;
        return 'B';
      })
      .catch((error: unknown) => error);
    expect(rejected).toBeInstanceOf(PeriError);
    expect((rejected as PeriError).code).toBe('closed');
    expect(creates).toBe(0);
    expect(owner.pending()).toHaveLength(0);
  });

  test('retiring an activation before its first open still rejects that open', async () => {
    const owner = createHostResourceOwner();
    const ref = {};
    owner.retire(ref);

    let creates = 0;
    const rejected = await owner
      .open(['sbx', 'never-opened'], ref, async () => {
        creates += 1;
        return 'A';
      })
      .catch((error: unknown) => error);
    expect(rejected).toBeInstanceOf(PeriError);
    expect((rejected as PeriError).code).toBe('closed');
    expect(creates).toBe(0);
    expect(owner.pending()).toHaveLength(0);
  });

  test('a retired activation leaves the replacement record for the same key untouched', async () => {
    const owner = createHostResourceOwner();
    const key = ['sbx', 'replacement'];
    const oldRef = {};
    const newRef = {};
    await openValue(owner, key, oldRef, 'old');

    owner.retire(oldRef);
    let creates = 0;
    const replacement = await owner.open(key, newRef, async () => {
      creates += 1;
      return 'new';
    });
    expect(replacement.value).toBe('new');
    expect(creates).toBe(1);
    expect(owner.pending()).toHaveLength(1);

    // Repeating the old retirement and reopening the old activation must not close or replace it.
    owner.retire(oldRef);
    const rejected = await owner
      .open(key, oldRef, async () => {
        creates += 1;
        return 'stale';
      })
      .catch((error: unknown) => error);
    expect(rejected).toBeInstanceOf(PeriError);
    expect((rejected as PeriError).code).toBe('closed');
    expect(creates).toBe(1);
    expect(owner.lease(newRef)?.value).toBe('new');
    expect(owner.lease(newRef)?.retired).toBe(false);
    expect(owner.pending()).toHaveLength(1);
  });
});
