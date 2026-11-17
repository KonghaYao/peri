import { describe, expect, test } from 'bun:test';
import { connect, createServer, type Socket } from 'node:net';
import { SERVER_IDLE_TIMEOUT_MS, SSE_HEARTBEAT_MS } from '../../../server/chats/keepalive';

const encoder = new TextEncoder();

function respondWith(stream: ReadableStream<Uint8Array>): Response {
  return new Response(stream, { headers: { 'content-type': 'text/event-stream' } });
}

function openEventStream(port: number, path: string): Promise<Socket> {
  return new Promise((resolve, reject) => {
    const socket = connect({ host: '127.0.0.1', port });
    socket.once('error', reject);
    socket.once('connect', () => {
      socket.off('error', reject);
      socket.write(`GET ${path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: keep-alive\r\n\r\n`);
      resolve(socket);
    });
  });
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

describe('SSE keepalive configuration', () => {
  test('the heartbeat fires at least twice inside the server idle timeout', () => {
    expect(SSE_HEARTBEAT_MS).toBeGreaterThan(0);
    expect(SERVER_IDLE_TIMEOUT_MS).toBeGreaterThan(SSE_HEARTBEAT_MS);
    expect(SSE_HEARTBEAT_MS * 2).toBeLessThanOrEqual(SERVER_IDLE_TIMEOUT_MS);
  });
});

// Real Bun.serve behaviour, probed over TCP so the test does not depend on a global fetch stub.
// The window is long because Bun 1.4 caps streamed responses at ~4s when idleTimeout <= 4
// (oven-sh/bun#46211), so the relation can only be exercised at normal timeout values.
describe('Bun.serve idle timeout', () => {
  test('a heartbeat keeps an SSE stream open while a silent stream is closed', async () => {
    const timers = new Set<ReturnType<typeof setInterval>>();
    const server = Bun.serve({
      hostname: '127.0.0.1',
      port: 0,
      idleTimeout: 10,
      fetch: (request) => {
        let streamTimer: ReturnType<typeof setInterval> | undefined;
        const silent = new URL(request.url).pathname === '/silent';
        return respondWith(
          new ReadableStream<Uint8Array>({
            start(controller) {
              controller.enqueue(encoder.encode(': open\n\n'));
              if (silent) return;
              streamTimer = setInterval(() => {
                controller.enqueue(encoder.encode(': ping\n\n'));
              }, 1_000);
              timers.add(streamTimer);
            },
            cancel() {
              if (streamTimer !== undefined) clearInterval(streamTimer);
            },
          }),
        );
      },
    });

    let live: Socket | null = null;
    let silent: Socket | null = null;
    try {
      const port = server.port;
      if (port === undefined) throw new Error('server did not bind a port');
      live = await openEventStream(port, '/live');
      silent = await openEventStream(port, '/silent');

      let received = '';
      live.on('data', (chunk: Buffer) => {
        received += chunk.toString('utf8');
      });
      let silentClosed = false;
      silent.on('data', () => {});
      silent.once('close', () => {
        silentClosed = true;
      });
      silent.once('error', () => {
        silentClosed = true;
      });

      await sleep(14_000);
      expect(silentClosed).toBe(true);
      expect(live.destroyed).toBe(false);
      expect(received.split(': ping').length - 1).toBeGreaterThanOrEqual(10);
    } finally {
      live?.destroy();
      silent?.destroy();
      for (const timer of timers) clearInterval(timer);
      await server.stop(true);
      const probe = createServer();
      try {
        await new Promise<void>((resolve, reject) => {
          probe.once('error', reject);
          probe.listen(server.port, '127.0.0.1', () => resolve());
        });
        console.info('[agent-builder test] owned keepalive port rebound', probe.address());
      } finally {
        await new Promise<void>((resolve, reject) =>
          probe.close((error) => (error ? reject(error) : resolve())),
        );
      }
    }
  }, 25_000);
});
