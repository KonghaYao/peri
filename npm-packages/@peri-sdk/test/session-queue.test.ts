import { expect, test } from "bun:test";
import { Session } from "../src/agent/session";
import { SessionQueueStaleError } from "../src/agent/session-queue";
import { RpcError } from "../src/transport/rpc-error";
import type { JsonRpcNotification, Transport } from "../src/transport/types";

type QueueSnapshot = { generation: string; revision: number; items: unknown[] };

class QueueTransport implements Transport {
  readonly calls: Array<{ method: string; params?: unknown }> = [];
  readonly snapshots: unknown[] = [];
  queue: QueueSnapshot = { generation: "g1", revision: 0, items: [] };
  enqueueError?: Error;
  dispatchError?: Error;
  takeBackError?: Error;
  private readonly listeners = new Set<(event: JsonRpcNotification) => void>();

  async request<Response>(method: string, params?: unknown): Promise<Response> {
    this.calls.push({ method, params });
    if (method === "initialize") return { protocolVersion: 1 } as Response;
    if (method === "session/new") return { sessionId: "session-1" } as Response;
    if (method === "session/input/snapshot") return structuredClone(this.queue) as Response;
    if (method === "session/input/enqueue") {
      if (this.enqueueError) throw this.enqueueError;
      const inputId = (params as { inputId: string }).inputId;
      return { results: [{ inputId, state: "queued" }], snapshot: structuredClone(this.queue) } as Response;
    }
    if (method === "session/input/dispatch") {
      if (this.dispatchError) throw this.dispatchError;
      const inputId = (params as { inputIds: string[] }).inputIds[0]!;
      return { results: [{ inputId, state: "dispatching" }], snapshot: structuredClone(this.queue) } as Response;
    }
    if (method === "session/input/takeback") {
      if (this.takeBackError) throw this.takeBackError;
      const inputId = (params as { inputId: string }).inputId;
      return { results: [{ inputId, state: "withdrawn" }], snapshot: structuredClone(this.queue) } as Response;
    }
    throw new Error(`unexpected method: ${method}`);
  }

  async sendRequest<Response>(method: string, params?: unknown): Promise<{ response: Promise<Response> }> {
    return { response: this.request<Response>(method, params) };
  }

  async notify(): Promise<void> {}

  subscribe(listener: (event: JsonRpcNotification) => void): () => void {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  }

  emit(event: JsonRpcNotification): void {
    for (const listener of this.listeners) listener(event);
  }

  async *events(): AsyncIterable<JsonRpcNotification> {}

  setRequestHandler(): void {}

  async close(): Promise<void> {}
}

function agentEvent(type: string, value: Record<string, unknown>): JsonRpcNotification {
  return {
    method: "peri/agent_event",
    params: { sessionId: "session-1", event_json: JSON.stringify({ type, value }) },
  };
}

function staleError(generation: string): RpcError {
  return new RpcError(-32602, "User input queue belongs to a different session generation", {
    rejected: true,
    snapshot: { generation, revision: 0, items: [] },
  });
}

async function startedSession(transport: QueueTransport): Promise<Session> {
  const session = new Session(transport, {
    requestHandler: async () => ({}),
    onQueueSnapshot: (snapshot) => { transport.snapshots.push(snapshot); },
  });
  await session.start({ path: "/workspace" });
  session.beginLive();
  return session;
}

function requestsFor(transport: QueueTransport, method: string) {
  return transport.calls.filter((call) => call.method === method);
}

test("queue change notification updates the generation used by the next enqueue", async () => {
  const transport = new QueueTransport();
  const session = await startedSession(transport);

  transport.emit(agentEvent("user_input_queue_changed", {
    snapshot: { generation: "g2", revision: 0, items: [] },
  }));
  expect(session.generation).toBe("g2");

  await session.enqueue("input-1", "hello");
  expect(requestsFor(transport, "session/input/enqueue")[0]?.params).toMatchObject({ generation: "g2" });
});

test("stale enqueue adopts the snapshot, throws once and never replays the input", async () => {
  const transport = new QueueTransport();
  const session = await startedSession(transport);
  transport.enqueueError = staleError("g2");

  const failure = await session.enqueue("input-1", "hello").catch((error) => error);
  expect(failure).toBeInstanceOf(SessionQueueStaleError);
  expect(failure).toMatchObject({
    name: "SessionQueueStaleError",
    generation: "g2",
    requiresRuntimeRestart: true,
  });
  expect((failure as SessionQueueStaleError).snapshot).toEqual({
    generation: "g2", revision: 0, items: [],
  });
  expect((failure as SessionQueueStaleError).cause).toBe(transport.enqueueError);
  expect(session.generation).toBe("g2");
  // 拒绝回执里的快照同样发布给宿主投影。
  expect(transport.snapshots).toContainEqual({ generation: "g2", revision: 0, items: [] });

  const enqueues = requestsFor(transport, "session/input/enqueue");
  expect(enqueues).toHaveLength(1);

  transport.enqueueError = undefined;
  await session.enqueue("input-2", "again");
  expect(requestsFor(transport, "session/input/enqueue")[1]?.params).toMatchObject({
    generation: "g2",
    inputId: "input-2",
  });
});

test("same-generation queue rejections stay RpcError instead of stale errors", async () => {
  const transport = new QueueTransport();
  const session = await startedSession(transport);
  transport.enqueueError = new RpcError(-32602, "User input queue is full", {
    rejected: true,
    snapshot: { generation: "g1", revision: 0, items: [] },
  });

  const failure = await session.enqueue("input-1", "hello").catch((error) => error);
  expect(failure).toBeInstanceOf(RpcError);
  expect(failure).not.toBeInstanceOf(SessionQueueStaleError);
  expect(session.generation).toBe("g1");
});

test("dispatch and takeBack share the stale generation handling", async () => {
  const transport = new QueueTransport();
  const session = await startedSession(transport);

  transport.dispatchError = staleError("g2");
  await expect(session.dispatch("input-1")).rejects.toBeInstanceOf(SessionQueueStaleError);
  expect(session.generation).toBe("g2");

  transport.takeBackError = staleError("g3");
  await expect(session.takeBack("input-1")).rejects.toBeInstanceOf(SessionQueueStaleError);
  expect(session.generation).toBe("g3");

  const dispatches = requestsFor(transport, "session/input/dispatch");
  const takeBacks = requestsFor(transport, "session/input/takeback");
  expect(dispatches[0]?.params).toMatchObject({ generation: "g1" });
  expect(takeBacks[0]?.params).toMatchObject({ generation: "g2" });
});

test("refreshInputQueue rebinds the generation from a fresh snapshot", async () => {
  const transport = new QueueTransport();
  const session = await startedSession(transport);
  transport.queue = { generation: "g9", revision: 1, items: [] };

  const snapshot = await session.refreshInputQueue();
  expect(snapshot).toEqual({ generation: "g9", revision: 1, items: [] });
  expect(session.generation).toBe("g9");
  expect(transport.snapshots).toContainEqual({ generation: "g9", revision: 1, items: [] });
});
