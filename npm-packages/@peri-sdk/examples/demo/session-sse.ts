import type { SSEStreamingApi } from "hono/streaming";
import type { DocStateVector } from "../../src/sync/index";
import type { SessionDocStream } from "./session-doc-stream";
import type { SessionEventLog } from "./session-event-log";

/** Backpressure includes optional diagnostics and heartbeat writes, not just Yjs frames. */
export async function streamSessionDocuments(stream: SSEStreamingApi, signal: AbortSignal,
  selected: { docs: SessionDocStream; events: SessionEventLog },
  options: { after: number; resume?: DocStateVector; diagnostics: boolean }): Promise<void> {
  let stopped = false;
  let resolveClosed!: () => void;
  const closed = new Promise<void>((resolve) => { resolveClosed = resolve; });
  const stop = () => { stopped = true; resolveClosed(); stream.abort(); };
  stream.onAbort(stop);
  signal.addEventListener("abort", stop, { once: true });
  let writes = Promise.resolve();
  let queuedBytes = 0;
  const write = (event: string, data: string, id?: string, initial = false): Promise<void> => {
    const bytes = initial ? 0 : data.length * 2;
    if (stopped || queuedBytes + bytes > 4 * 1024 * 1024) {
      stop();
      return Promise.reject(new Error("SSE buffer exceeded; reconnect to resume documents"));
    }
    queuedBytes += bytes;
    const pending = writes.then(async () => {
      if (!stopped) await stream.writeSSE({ event, data, id });
    }).finally(() => { queuedBytes -= bytes; });
    writes = pending.catch(stop);
    return pending;
  };
  let unsubscribe = () => {};
  let unsubscribeEvents = () => {};
  let heartbeat: ReturnType<typeof setInterval> | undefined;
  try {
    const connection = selected.docs.subscribe((entry) =>
      write("docs:update", JSON.stringify(entry), String(entry.sequence)), { resume: options.resume, onError: stop });
    unsubscribe = connection.unsubscribe;
    void write("docs:snapshot", JSON.stringify(connection.snapshot), String(connection.snapshot.sequence), true).catch(stop);
    if (options.diagnostics) unsubscribeEvents = selected.events.subscribe(options.after, (entry) => {
      void write("notification", JSON.stringify(entry), String(entry.id)).catch(stop);
    });
    heartbeat = setInterval(() => { void write("ping", "{}").catch(stop); }, 15_000);
    if (signal.aborted || stream.aborted) stop();
    await closed;
  } finally {
    if (heartbeat) clearInterval(heartbeat);
    unsubscribe();
    unsubscribeEvents();
    signal.removeEventListener("abort", stop);
    await writes;
  }
}
