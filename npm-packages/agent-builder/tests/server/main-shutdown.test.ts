import { expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { createShutdown } from '../../server/shutdown';

// 执行 main 的真实装配/退出代码，仅替换外部端口与进程退出，避免启动大 engine。
test.each(['chats', 'host', 'sandbox', 'engine', 'blocked', 'stop-throw'] as const)(
  'ABV-06 actual main shutdown exits 1 for %s',
  async (mode) => {
    const source = readFileSync(new URL('../../server/main.ts', import.meta.url), 'utf8');
    const body = source.slice(
      source.indexOf('const cleanup = createShutdown('),
      source.indexOf("process.on('SIGINT'"),
    );
    const js = new Bun.Transpiler({ loader: 'ts', target: 'bun' }).transformSync(
      `${body}\nawait shutdown('SIGTERM');`,
    );
    const AsyncFunction = Object.getPrototypeOf(async () => {}).constructor;
    const fn = new AsyncFunction(
      'createShutdown',
      'server',
      'chatService',
      'owner',
      'sandboxRuntime',
      'engine',
      'secureDatabaseFiles',
      'env',
      'db',
      'process',
      'console',
      'diagnosticCode',
      js,
    );
    let release = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    let cleanup: ReturnType<typeof createShutdown> | undefined;
    let exit = -1;
    let confirmed = mode === 'blocked' || mode === 'stop-throw';
    const calls: string[] = [];
    const logs: string[] = [];
    try {
      await fn(
        (options: Parameters<typeof createShutdown>[0]) => {
          cleanup = createShutdown({ ...options, timeoutMs: 2, hostTimeoutMs: 2 });
          return cleanup;
        },
        {
          stop() {
            calls.push('stop');
            if (mode === 'stop-throw') throw new Error('injected server stop failure');
          },
        },
        {
          async closeAllChats() {
            return { complete: confirmed || mode !== 'chats', errors: [] };
          },
        },
        {
          beginShutdown() {},
          async closeAll() {
            return { complete: confirmed || mode !== 'host', pending: 0, failures: [] };
          },
        },
        {
          runtime: {
            async closeAll() {
              calls.push('sandbox');
              if (mode === 'sandbox') await gate;
            },
          },
        },
        {
          async shutdown() {
            calls.push('engine');
            if (mode === 'engine') await gate;
          },
          async forceStop() {
            calls.push('engine.forceStop');
            return mode === 'blocked'
              ? { status: 'blocked' as const, pids: [123] }
              : { status: 'none' as const, pids: [] };
          },
        },
        () => {},
        { databasePath: 'unused' },
        {
          close() {
            calls.push('db.close');
          },
        },
        {
          exit(code: number) {
            exit = code;
          },
        },
        {
          log(text: string) {
            logs.push(text);
          },
          error(text: string, detail?: unknown) {
            logs.push(`${text} ${detail === undefined ? '' : JSON.stringify(detail)}`.trim());
          },
        },
        () => 'internal_error',
      );
      expect(exit).toBe(1);
      expect(calls).toContain('db.close');
      // A failed step must not skip the terminal engine cleanup: process exit does not reap it.
      expect(calls.indexOf('engine.forceStop')).toBeGreaterThan(-1);
      expect(calls.indexOf('engine.forceStop')).toBeLessThan(calls.indexOf('db.close'));
      if (mode === 'chats' || mode === 'host') expect(calls).not.toContain('sandbox');
    } finally {
      confirmed = true;
      release();
      if (cleanup !== undefined) {
        expect((await cleanup.run()).complete).toBe(true);
        expect(cleanup.pending).toEqual([]);
      }
    }
  },
);

// A second SIGTERM during shutdown used to reach the default handler and exit before the engine
// child was stopped. The handler must stay installed for the whole shutdown.
test('ABV-06 signals stay handled while shutdown is running', () => {
  const source = readFileSync(new URL('../../server/main.ts', import.meta.url), 'utf8');
  expect(source).toContain("process.on('SIGINT'");
  expect(source).toContain("process.on('SIGTERM'");
  expect(source).not.toContain('process.once(');
});
