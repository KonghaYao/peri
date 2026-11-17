import { describe, expect, test } from 'bun:test';
import {
  probeStoreOnce,
  probeStoreReachability,
  STORE_PROBE_TIMEOUT_MS,
} from '../../../server/chats/store-probe';

const TARGET = { storeUrl: 'http://127.0.0.1:9/db/x/y', storeToken: 'store-token-secret' };

/** Accepts TCP connections and never answers: only a cancelled request frees its socket. */
function blackHole(): { readonly server: Bun.TCPSocketListener; readonly active: Set<unknown> } {
  const active = new Set<unknown>();
  const server = Bun.listen({
    hostname: '127.0.0.1',
    port: 0,
    socket: {
      open(socket) {
        active.add(socket);
      },
      data: () => {},
      close(socket) {
        active.delete(socket);
      },
      error(socket) {
        active.delete(socket);
      },
    },
  });
  return { server, active };
}

async function waitFor(predicate: () => boolean, timeoutMs: number): Promise<void> {
  const started = Date.now();
  while (!predicate()) {
    if (Date.now() - started > timeoutMs) throw new Error('condition was not met in time');
    await Bun.sleep(10);
  }
}

describe('probeStoreReachability', () => {
  test('reports reachable only after the injected read probe succeeds', async () => {
    const seen: Array<{ url: string; authToken: string }> = [];
    const result = await probeStoreReachability(TARGET, {
      probe: async (target) => {
        seen.push(target);
      },
    });

    expect(result).toEqual({ reachable: true });
    expect(seen).toEqual([{ url: TARGET.storeUrl, authToken: TARGET.storeToken }]);
  });

  test('classifies an untyped rejection as store_unreachable and never exposes the token', async () => {
    const result = await probeStoreReachability(TARGET, {
      probe: async () => {
        throw new Error(`connect ECONNREFUSED with ${TARGET.storeToken}`);
      },
    });

    expect(result).toEqual({ reachable: false, reason: 'store_unreachable', kind: 'network' });
  });

  test('classifies a timeout as a retryable store_unreachable', async () => {
    const started = Date.now();
    const result = await probeStoreReachability(TARGET, {
      probe: () => new Promise<void>(() => {}),
      timeoutMs: 20,
    });

    expect(result).toEqual({ reachable: false, reason: 'store_unreachable', kind: 'timeout' });
    expect(Date.now() - started).toBeLessThan(1_000);
  });

  test('skips the probe without a token', async () => {
    let called = false;
    const result = await probeStoreReachability(
      { storeUrl: TARGET.storeUrl, storeToken: undefined },
      {
        probe: async () => {
          called = true;
        },
      },
    );

    expect(result).toEqual({ reachable: false, reason: 'store_token_missing', kind: 'unknown' });
    expect(called).toBe(false);
  });

  test('keeps the startup budget at two seconds or less', () => {
    expect(STORE_PROBE_TIMEOUT_MS).toBeLessThanOrEqual(2_000);
  });
});

describe('store probe error classification', () => {
  /** Real Store-shaped endpoint: the installed SDK turns the status into a DatabaseError. */
  function statusServer(status: number): {
    readonly server: ReturnType<typeof Bun.serve>;
    readonly url: string;
  } {
    const server = Bun.serve({
      hostname: '127.0.0.1',
      port: 0,
      fetch: () => new Response(JSON.stringify({ message: `Store answered ${status}` }), { status }),
    });
    return { server, url: `http://127.0.0.1:${server.port}` };
  }

  for (const status of [401, 403]) {
    test(`classifies a real HTTP ${status} as store_auth_failed`, async () => {
      const { server, url } = statusServer(status);
      try {
        const result = await probeStoreReachability({ storeUrl: url, storeToken: 'store-token-secret' });
        expect(result).toEqual({
          reachable: false,
          reason: 'store_auth_failed',
          kind: 'http',
          status,
        });
      } finally {
        server.stop(true);
      }
    });
  }

  test('classifies a real HTTP 404 as store_database_missing', async () => {
    const { server, url } = statusServer(404);
    try {
      const result = await probeStoreReachability({ storeUrl: url, storeToken: 'store-token-secret' });
      expect(result).toEqual({
        reachable: false,
        reason: 'store_database_missing',
        kind: 'http',
        status: 404,
      });
    } finally {
      server.stop(true);
    }
  });

  test('classifies a real HTTP 500 as a retryable store_unreachable', async () => {
    const { server, url } = statusServer(500);
    try {
      const result = await probeStoreReachability({ storeUrl: url, storeToken: 'store-token-secret' });
      expect(result).toEqual({
        reachable: false,
        reason: 'store_unreachable',
        kind: 'http',
        status: 500,
      });
    } finally {
      server.stop(true);
    }
  });

  test('keeps a 5xx retryable even when its message mentions a missing database', async () => {
    const result = await probeStoreReachability(TARGET, {
      probe: async () => {
        throw Object.assign(new Error('HTTP error! status: 503: database not found'), { status: 503 });
      },
    });

    expect(result).toEqual({
      reachable: false,
      reason: 'store_unreachable',
      kind: 'http',
      status: 503,
    });
  });

  test('reads the status from DatabaseError.message when the package does not expose it', async () => {
    const result = await probeStoreReachability(TARGET, {
      probe: async () => {
        throw new Error('HTTP error! status: 401');
      },
    });

    expect(result).toEqual({
      reachable: false,
      reason: 'store_auth_failed',
      kind: 'http',
      status: 401,
    });
  });

  test('classifies an explicit missing-database message without an HTTP status', async () => {
    const result = await probeStoreReachability(TARGET, {
      probe: async () => {
        throw Object.assign(new Error('Database does not exist: agent_builder'), {
          code: 'DATABASE_NOT_FOUND',
        });
      },
    });

    expect(result).toEqual({ reachable: false, reason: 'store_database_missing', kind: 'http' });
  });

  test('classifies connection refusal as store_unreachable', async () => {
    const result = await probeStoreReachability(
      { storeUrl: 'http://127.0.0.1:1/db/local/main', storeToken: 'store-token-secret' },
      { timeoutMs: 1_000 },
    );

    expect(result).toEqual({ reachable: false, reason: 'store_unreachable', kind: 'network' });
  });

  test('logs the category/status without the token or Store URL', async () => {
    const lines: string[] = [];
    const original = console.warn;
    console.warn = (...args: unknown[]) => {
      lines.push(args.map((value) => (typeof value === 'string' ? value : JSON.stringify(value))).join(' '));
    };
    try {
      await probeStoreReachability(TARGET, {
        probe: async () => {
          throw Object.assign(new Error(`HTTP error! status: 403 ${TARGET.storeUrl}`), { status: 403 });
        },
      });
    } finally {
      console.warn = original;
    }

    const output = lines.join('\n');
    expect(output).toContain('store_auth_failed');
    expect(output).toContain('403');
    expect(output).not.toContain(TARGET.storeToken);
    expect(output).not.toContain(TARGET.storeUrl);
  });
});

describe('probeStoreOnce cancellation', () => {
  test('aborts its own query within the budget and never echoes the token', async () => {
    const { server, active } = blackHole();
    try {
      const started = Date.now();
      const failure = await probeStoreOnce(
        { url: `http://127.0.0.1:${server.port}/db/local/main`, authToken: 'store-token-secret' },
        150,
      ).then(
        () => null,
        (reason: unknown) => reason,
      );

      expect(failure).toBeInstanceOf(Error);
      expect(failure).toMatchObject({ name: 'TimeoutError' });
      expect(String(failure)).not.toContain('store-token-secret');
      expect(Date.now() - started).toBeLessThan(STORE_PROBE_TIMEOUT_MS);
      await waitFor(() => active.size === 0, 2_000);
      expect(active.size).toBe(0);
    } finally {
      server.stop(true);
    }
  });

  test('repeated probes against a black-hole Store do not accumulate connections', async () => {
    const { server, active } = blackHole();
    const target = {
      storeUrl: `http://127.0.0.1:${server.port}/db/local/main`,
      storeToken: 'store-token-secret',
    };
    try {
      for (let attempt = 0; attempt < 3; attempt += 1) {
        const started = Date.now();
        expect(await probeStoreReachability(target, { timeoutMs: 120 })).toEqual({
          reachable: false,
          reason: 'store_unreachable',
          kind: 'timeout',
        });
        expect(Date.now() - started).toBeLessThan(STORE_PROBE_TIMEOUT_MS);
        // The abandoned socket must be gone before the next probe starts.
        await waitFor(() => active.size === 0, 2_000);
        expect(active.size).toBe(0);
      }
    } finally {
      server.stop(true);
    }
  });
});
