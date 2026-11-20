import { SSE_HEARTBEAT_MS } from '../../chats/keepalive';
import type { PlatformEventRow } from '../repositories/sessions';
import { formatEventFrame } from './events';

export interface SessionEventSource {
  replay(sessionId: string): PlatformEventRow[];
  subscribe(sessionId: string, listener: (row: PlatformEventRow) => void): () => void;
}

export interface SessionEventTap {
  /** Starts the SSE body; buffered rows are flushed first, then live rows follow. */
  response(status: number): Response;
  /** Releases the subscription when the caller never returns the response. */
  dispose(): void;
}

const SSE_HEADERS = {
  'content-type': 'text/event-stream; charset=utf-8',
  'cache-control': 'no-cache, no-transform',
  connection: 'keep-alive',
  'x-accel-buffering': 'no',
} as const;

/**
 * Subscribes before the caller performs any work that produces events.
 *
 * Every connection starts from the durable log: the replay and the live subscription are merged in
 * the same buffer ordered by log position. Subscribing first is what makes "create session, then
 * submit input" lossless: events produced between creation and the first SSE byte are already
 * captured.
 */
export function openSessionEventTap(
  source: SessionEventSource,
  sessionId: string,
  requestSignal: AbortSignal,
): SessionEventTap {
  const buffered: PlatformEventRow[] = [];
  let lastSeq = 0;
  let started = false;
  let disposed = false;
  let deliver: ((row: PlatformEventRow) => void) | null = null;

  const unsubscribe = source.subscribe(sessionId, (row) => {
    if (disposed) return;
    if (!started) {
      buffered.push(row);
      return;
    }
    deliver?.(row);
  });
  for (const row of source.replay(sessionId)) buffered.push(row);

  const dispose = () => {
    if (disposed) return;
    disposed = true;
    unsubscribe();
  };

  return {
    dispose,
    response(status: number): Response {
      const abort = new AbortController();
      const onAbort = () => abort.abort();
      if (requestSignal.aborted) abort.abort();
      requestSignal.addEventListener('abort', onAbort);

      const encoder = new TextEncoder();
      let done = false;
      let finish = () => {};

      const stream = new ReadableStream<Uint8Array>({
        start(controller) {
          const heartbeat = setInterval(() => {
            if (done) return;
            try {
              controller.enqueue(encoder.encode(': ping\n\n'));
            } catch {
              finish();
            }
          }, SSE_HEARTBEAT_MS);

          const write = (chunk: string) => {
            if (done) return;
            try {
              controller.enqueue(encoder.encode(chunk));
            } catch {
              finish();
            }
          };

          finish = () => {
            if (done) return;
            done = true;
            started = false;
            deliver = null;
            clearInterval(heartbeat);
            requestSignal.removeEventListener('abort', onAbort);
            abort.signal.removeEventListener('abort', finish);
            dispose();
            try {
              controller.close();
            } catch {
              // Already closed by the client; nothing to release.
            }
          };
          abort.signal.addEventListener('abort', finish);

          deliver = (row) => {
            if (row.seq <= lastSeq) return;
            lastSeq = row.seq;
            write(formatEventFrame(row));
          };
          started = true;

          const pending = buffered.splice(0).sort((left, right) => left.seq - right.seq);
          for (const row of pending) deliver(row);
          if (abort.signal.aborted) finish();
        },
        cancel() {
          abort.abort();
        },
      });

      return new Response(stream, { status, headers: SSE_HEADERS });
    },
  };
}
