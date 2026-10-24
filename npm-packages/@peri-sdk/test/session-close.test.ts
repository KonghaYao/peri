import { expect, test } from "bun:test";
import { Session } from "../src/agent/session";
import { closeDrainBudget, SessionCloseIncompleteError } from "../src/agent/session-close";
import { RpcError } from "../src/transport/rpc-error";
import type { JsonRpcNotification, Transport } from "../src/transport/types";

class CloseTransport implements Transport {
  readonly calls: Array<{ method: string; params?: unknown }> = [];
  incompleteAttempts = 0;
  closeRequests = 0;
  wireCloses = 0;
  failWireClose = false;
  fatal?: Error;
  startupGate?: Promise<void>;
  failSnapshot = false;

  async request<Response>(method: string, params?: unknown): Promise<Response> {
    this.calls.push({ method, params });
    if (method === "initialize") {
      await this.startupGate;
      return { protocolVersion: 1 } as Response;
    }
    if (method === "session/new") return { sessionId: "session-1" } as Response;
    if (method === "session/input/snapshot") {
      if (this.failSnapshot) throw new Error("snapshot failed");
      return { generation: "generation-1" } as Response;
    }
    if (method === "session/close") {
      this.closeRequests++;
      if (this.fatal) throw this.fatal;
      if (this.closeRequests <= this.incompleteAttempts)
        throw new RpcError(-32010, `Session close incomplete: attempt ${this.closeRequests}`);
      return {} as Response;
    }
    throw new Error(`unexpected method: ${method}`);
  }
  async sendRequest<Response>(method: string, params?: unknown) {
    return { response: this.request<Response>(method, params) };
  }
  async notify(method: string, params?: unknown): Promise<void> { this.calls.push({ method, params }); }
  subscribe(): () => void { return () => {}; }
  async *events(): AsyncIterable<JsonRpcNotification> {}
  setRequestHandler(): void {}
  async close(): Promise<void> {
    this.wireCloses++;
    if (this.failWireClose) throw new Error("wire close unconfirmed");
  }
}

function sessionFor(transport = new CloseTransport()): Session {
  return new Session(transport, { requestHandler: async () => ({}) });
}

test("close sends session/close and leaves transport ownership to the caller", async () => {
  const transport = new CloseTransport();
  const session = sessionFor(transport);
  await session.start({ path: "/workspace" });
  await session.close();
  expect(transport.calls.filter((call) => call.method === "session/close"))
    .toEqual([{ method: "session/close", params: { sessionId: "session-1" } }]);
  expect(transport.wireCloses).toBe(0);
  expect(session.isClosed).toBe(true);
  await session.close();
  expect(transport.closeRequests).toBe(1);
});

test("incomplete close is retried until the domain settles", async () => {
  const transport = new CloseTransport();
  transport.incompleteAttempts = 3;
  const session = sessionFor(transport);
  await session.start({ path: "/workspace" });
  await session.close({ drainTimeoutMs: 2_000 });
  expect(transport.closeRequests).toBe(4);
  expect(session.isClosed).toBe(true);
});

test("close deadline reports incomplete and keeps the session retryable", async () => {
  const transport = new CloseTransport();
  transport.incompleteAttempts = Number.MAX_SAFE_INTEGER;
  const session = sessionFor(transport);
  await session.start({ path: "/workspace" });
  const failure = await session.close({ drainTimeoutMs: 60 }).catch((error) => error);
  expect(failure).toBeInstanceOf(SessionCloseIncompleteError);
  expect(failure.detail).toContain("Session close incomplete");
  expect(session.isClosed).toBe(false);
  transport.incompleteAttempts = transport.closeRequests;
  await session.close({ drainTimeoutMs: 2_000 });
  expect(session.isClosed).toBe(true);
});

test("zero drain budget performs exactly one attempt", async () => {
  const transport = new CloseTransport();
  transport.incompleteAttempts = 5;
  const session = sessionFor(transport);
  await session.start({ path: "/workspace" });
  await expect(session.close({ drainTimeoutMs: 0 })).rejects.toBeInstanceOf(SessionCloseIncompleteError);
  expect(transport.closeRequests).toBe(1);
});

test("a non-retryable close failure is reported unchanged", async () => {
  const transport = new CloseTransport();
  const fatal = new RpcError(-32602, "Method not found: session/close");
  transport.fatal = fatal;
  const session = sessionFor(transport);
  await session.start({ path: "/workspace" });
  await expect(session.close()).rejects.toBe(fatal);
  expect(session.isClosed).toBe(false);
});

test("a new failure after an incomplete attempt keeps both causes", async () => {
  const transport = new CloseTransport();
  transport.incompleteAttempts = Number.MAX_SAFE_INTEGER;
  const session = sessionFor(transport);
  await session.start({ path: "/workspace" });
  const closing = session.close({ drainTimeoutMs: 2_000 }).catch((error) => error);
  await Bun.sleep(60);
  const fatal = new RpcError(-32603, "transport lost during close");
  transport.fatal = fatal;
  const failure = await closing;
  expect(failure).toBeInstanceOf(SessionCloseIncompleteError);
  expect(failure.cause).toBeInstanceOf(AggregateError);
  expect((failure.cause as AggregateError).errors[1]).toBe(fatal);
});

test("close budget validation rejects invalid options before any request", async () => {
  const transport = new CloseTransport();
  const session = sessionFor(transport);
  await session.start({ path: "/workspace" });
  expect(() => session.close({ drainTimeoutMs: -1 })).toThrow("nonnegative safe integer");
  expect(() => session.close({ drainTimeoutMs: 1.5 })).toThrow("nonnegative safe integer");
  expect(closeDrainBudget({})).toBe(3_000);
  expect(transport.closeRequests).toBe(0);
});

test("concurrent close shares the same attempt", async () => {
  const transport = new CloseTransport();
  const session = sessionFor(transport);
  await session.start({ path: "/workspace" });
  const first = session.close();
  const second = session.close();
  expect(second).toBe(first);
  await first;
  expect(transport.closeRequests).toBe(1);
});

test("close during startup is rejected instead of racing the session setup", async () => {
  const transport = new CloseTransport();
  let release!: () => void;
  transport.startupGate = new Promise<void>((done) => { release = done; });
  const session = sessionFor(transport);
  const starting = session.start({ path: "/workspace" });
  await expect(session.close()).rejects.toThrow("Wait for Session startup");
  release();
  await starting;
  await session.close();
});

test("a failed startup without an established Session can be cleaned up", async () => {
  const transport = new CloseTransport();
  transport.failSnapshot = true;
  const session = sessionFor(transport);
  await expect(session.start({ path: "/workspace" })).rejects.toMatchObject({
    name: "SessionStartError", stage: "input-snapshot", createdSessionId: "session-1",
  });
  await session.close();
  expect(session.isClosed).toBe(true);
  expect(transport.closeRequests).toBe(1);
});
