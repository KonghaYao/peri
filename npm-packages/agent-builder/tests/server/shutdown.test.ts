import { describe, expect, test } from 'bun:test';
import { inspect } from 'node:util';
import { createShutdown } from '../../server/shutdown';

function gate() {
  let resolve = () => {};
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function setup(mode: 'complete' | 'chats' | 'host' | 'sandbox' | 'engine') {
  const events: string[] = [];
  const wait = gate();
  let confirmed = mode === 'complete';
  const cleanup = createShutdown({
    stop: () => {
      events.push('stop');
    },
    chats: {
      async closeAllChats() {
        events.push('chats');
        return { complete: confirmed || mode !== 'chats', errors: [] };
      },
    },
    owner: {
      beginShutdown() {
        events.push('owner.begin');
      },
      async closeAll() {
        events.push('owner.close');
        return { complete: confirmed || mode !== 'host', pending: confirmed ? 0 : 1, failures: [] };
      },
    },
    sandbox: {
      runtime: {
        async closeAll() {
          events.push('sandbox');
          if (mode === 'sandbox') await wait.promise;
        },
      },
    },
    engine: {
      async shutdown() {
        events.push('engine');
        if (mode === 'engine') await wait.promise;
      },
    },
    timeoutMs: 2,
    hostTimeoutMs: 2,
  });
  return {
    cleanup,
    events,
    confirm() {
      confirmed = true;
      wait.resolve();
    },
  };
}

describe('ABV-06 confirmed shutdown', () => {
  test('complete cleanup confirms every dependency in order, exactly once', async () => {
    const { cleanup, events } = setup('complete');
    expect(await cleanup.run()).toEqual({ complete: true, pending: [] });
    expect(events).toEqual(['stop', 'chats', 'owner.begin', 'owner.close', 'sandbox', 'engine']);
    await cleanup.run();
    expect(events.filter((event) => event === 'sandbox')).toHaveLength(1);
    expect(events.filter((event) => event === 'engine')).toHaveLength(1);
  });

  test.each(['chats', 'host', 'sandbox', 'engine'] as const)(
    '%s incomplete is not success and retains cleanup for retry',
    async (mode) => {
      const { cleanup, events, confirm } = setup(mode);
      const lines: string[] = [];
      const originalLog = console.log;
      const originalError = console.error;
      console.log = (...args) => {
        lines.push(inspect(args));
      };
      console.error = (...args) => {
        lines.push(inspect(args));
      };
      try {
        const first = cleanup.run();
        expect(cleanup.run()).toBe(first);
        const result = await first;
        expect(result).toEqual({ complete: false, pending: [mode] });
        expect(result.complete ? 0 : 1).toBe(1);
        if (mode !== 'engine')
          expect(lines.some((line) => line.includes('sandbox runtime closed'))).toBe(false);
        if (mode === 'chats' || mode === 'host') expect(events).not.toContain('sandbox');
        if (mode !== 'engine') expect(events).not.toContain('engine');
        if (mode === 'sandbox' || mode === 'engine') expect(cleanup.pending).toEqual([mode]);
        confirm();
        expect((await cleanup.run()).complete).toBe(true);
        expect(cleanup.pending).toEqual([]);
        expect(events.filter((event) => event === 'sandbox')).toHaveLength(1);
        expect(events.filter((event) => event === 'engine')).toHaveLength(1);
      } finally {
        confirm();
        console.log = originalLog;
        console.error = originalError;
      }
    },
  );

  test('random failure message/name/stack/cause stay internal; real rejection can retry', async () => {
    const secret = crypto.randomUUID() + crypto.randomUUID();
    const failure = Object.assign(new Error(secret, { cause: new Error(secret) }), {
      name: secret,
      code: secret,
    });
    let fail = true;
    const cleanup = createShutdown({
      stop() {},
      chats: {
        async closeAllChats() {
          return { complete: true, errors: [] };
        },
      },
      owner: {
        beginShutdown() {},
        async closeAll() {
          return { complete: true, pending: 0, failures: [] };
        },
      },
      sandbox: {
        runtime: {
          async closeAll() {
            if (fail) throw failure;
          },
        },
      },
      engine: { async shutdown() {} },
    });
    const original = console.error;
    const lines: string[] = [];
    console.error = (...args) => {
      lines.push(inspect(args, { depth: 10 }));
    };
    try {
      expect((await cleanup.run()).complete).toBe(false);
      expect(lines.some((line) => line.includes(secret))).toBe(false);
      fail = false;
      expect((await cleanup.run()).complete).toBe(true);
    } finally {
      console.error = original;
    }
  });
});
