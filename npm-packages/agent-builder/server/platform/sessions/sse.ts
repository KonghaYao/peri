import { SSE_HEARTBEAT_MS } from '../../chats/keepalive';
import type { PlatformEventRow } from '../repositories/sessions';
import { formatEventFrame } from './events';

export interface SessionEventSource {
  replay(sessionId: string, afterSeq: number): PlatformEventRow[];
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
 * Live rows are buffered until the response body starts, and the durable replay is merged in the
 * same buffer ordered by log position. Subscribing first is what makes "create session, then submit
 * input" lossless: events produced between creation and the first SSE byte are already captured.
 */
export function openSessionEventTap(
  source: SessionEventSource,
  sessionId: string,
  afterSeq: number,
  requestSignal: AbortSignal,
): SessionEventTap {
  const buffered: PlatformEventRow[] = [];
  let lastSeq = afterSeq;
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
  for (const row of source.replay(sessionId, afterSeq)) buffered.push(row);

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

/** Resolves the documented `Last-Event-ID` header: log position or public event id. */
export function resumePosition(
  source: { sequenceOf(sessionId: string, eventId: string): number | null },
  sessionId: string,
  header: string | undefined,
): number {
  if (header === undefined || header.length === 0) return 0;
  if (/^\d+$/.test(header)) return Number(header);
  return source.sequenceOf(sessionId, header) ?? 0;
}
