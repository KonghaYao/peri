import { expect, test } from "bun:test";
import { Agent } from "../src/agent/agent";
import { closeDrainBudget, SessionCloseIncompleteError } from "../src/agent/session-close";
import { MemoryKV } from "../src/kv/memory-kv";
import { Sandbox } from "../src/sandbox/sandbox";
import { RpcError } from "../src/transport/rpc-error";
import type { JsonRpcNotification, Transport } from "../src/transport/types";

class CloseTransport implements Transport {
  readonly calls: Array<{ method: string; params?: unknown }> = [];
  incompleteAttempts = 0;
  closeRequests = 0;
  wireCloses = 0;
  failWireClose = false;
  fatal?: Error;

  async request<Response>(method: string, params?: unknown): Promise<Response> {
    this.calls.push({ method, params });
    if (method === "initialize") return { protocolVersion: 1 } as Response;
    if (method === "session/new") return { sessionId: "session-1" } as Response;
    if (method === "session/input/snapshot") return { generation: "generation-1" } as Response;
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

function fixture(transport = new CloseTransport()) {
  const kv = new MemoryKV();
  const sandbox = new Sandbox({ id: "workspace", transportFactory: () => transport });
  const agent = new Agent({ id: "agent", path: "/workspace", sandbox }, kv);
  return { agent, transport, kv };
}

test("close sends session/close with the Session id and releases transport and claims", async () => {
  const { agent, transport, kv } = fixture();
  const session = await agent.session.start(null);
  await session.close();
  expect(transport.calls.filter((call) => call.method === "session/close"))
    .toEqual([{ method: "session/close", params: { sessionId: "session-1" } }]);
  expect(transport.wireCloses).toBe(1);
  expect(kv.owners.size).toBe(0);
  expect(session.isClosed).toBe(true);
  await session.close();
  expect(transport.closeRequests).toBe(1);
});

test("incomplete close is retried until the domain settles", async () => {
  const { agent, transport } = fixture();
  transport.incompleteAttempts = 3;
  const session = await agent.session.start(null);
  await session.close({ drainTimeoutMs: 2_000 });
  expect(transport.closeRequests).toBe(4);
  expect(transport.wireCloses).toBe(1);
  expect(session.isClosed).toBe(true);
});

test("close deadline reports incomplete and keeps claims for a later retry", async () => {
  const { agent, transport, kv } = fixture();
  transport.incompleteAttempts = Number.MAX_SAFE_INTEGER;
  const session = await agent.session.start(null);
  const failure = await session.close({ drainTimeoutMs: 60 }).catch((error) => error);
  expect(failure).toBeInstanceOf(SessionCloseIncompleteError);
  expect(failure.detail).toContain("Session close incomplete");
  expect(transport.wireCloses).toBe(0);
  expect(kv.owners.size).toBe(2);
  expect(session.isClosed).toBe(false);
  transport.incompleteAttempts = transport.closeRequests;
  await session.close({ drainTimeoutMs: 2_000 });
  expect(session.isClosed).toBe(true);
  expect(kv.owners.size).toBe(0);
});

test("zero drain budget performs exactly one attempt", async () => {
  const { agent, transport } = fixture();
  transport.incompleteAttempts = 5;
  const session = await agent.session.start(null);
  await expect(session.close({ drainTimeoutMs: 0 })).rejects.toBeInstanceOf(SessionCloseIncompleteError);
  expect(transport.closeRequests).toBe(1);
});

test("a non-retryable close failure is reported unchanged", async () => {
  const { agent, transport, kv } = fixture();
  const fatal = new RpcError(-32602, "Method not found: session/close");
  transport.fatal = fatal;
  const session = await agent.session.start(null);
  await expect(session.close()).rejects.toBe(fatal);
  expect(transport.wireCloses).toBe(0);
  expect(kv.owners.size).toBe(2);
});

test("a new failure after an incomplete attempt keeps both causes", async () => {
  const { agent, transport } = fixture();
  transport.incompleteAttempts = Number.MAX_SAFE_INTEGER;
  const session = await agent.session.start(null);
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
  const { agent, transport } = fixture();
  const session = await agent.session.start(null);
  expect(() => session.close({ drainTimeoutMs: -1 })).toThrow("nonnegative safe integer");
  expect(() => session.close({ drainTimeoutMs: 1.5 })).toThrow("nonnegative safe integer");
  expect(closeDrainBudget({})).toBe(3_000);
  expect(transport.closeRequests).toBe(0);
});

test("a transport shutdown failure retains claims and can be retried", async () => {
  const { agent, transport, kv } = fixture();
  transport.failWireClose = true;
  const session = await agent.session.start(null);
  await expect(session.close()).rejects.toThrow("wire close unconfirmed");
  expect(kv.owners.size).toBe(2);
  expect(session.isClosed).toBe(false);
  transport.failWireClose = false;
  await session.close();
  expect(kv.owners.size).toBe(0);
  expect(session.isClosed).toBe(true);
});
