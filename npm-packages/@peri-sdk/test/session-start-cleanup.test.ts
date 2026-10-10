import { expect, test } from "bun:test";
import { Agent } from "../src/agent/agent";
import { AgentClaimConflictError } from "../src/kv/agent-claim-conflict-error";
import { MemoryKV } from "../src/kv/memory-kv";
import { Sandbox } from "../src/sandbox/sandbox";
import type { JsonRpcNotification, Transport } from "../src/transport/types";
import { RpcError } from "../src/transport/rpc-error";

function gate() {
  let resolve!: () => void;
  const promise = new Promise<void>((complete) => { resolve = complete; });
  return { promise, resolve };
}

class FakeTransport implements Transport {
  readonly calls: Array<{ method: string; params?: unknown }> = [];
  readonly listeners = new Set<(event: JsonRpcNotification) => void>();
  readonly startupError = new Error("input queue startup failed");
  readonly cleanupError = new Error("transport termination unconfirmed");
  failStartup = false;
  failClose = false;
  closeCalls = 0;
  startupGate?: Promise<void>;
  closeGate?: Promise<void>;
  loadResponse: unknown = {};
  loadEvents: JsonRpcNotification[] = [];
  failCloseCommand = false;
  readonly closeCommandError = new RpcError(-32010, "Session close incomplete: prompt is still active");

  async request<Response>(method: string, params?: unknown): Promise<Response> {
    this.calls.push({ method, params });
    if (method === "initialize") {
      await this.startupGate;
      return { protocolVersion: 1 } as Response;
    }
    if (method === "session/input/snapshot") {
      if (this.failStartup) throw this.startupError;
      return { generation: "generation-1" } as Response;
    }
    if (method === "session/new") return { sessionId: "existing" } as Response;
    if (method === "session/load") {
      for (const event of this.loadEvents) this.emit(event);
      return this.loadResponse as Response;
    }
    if (method === "session/input/dispatch") return {
      results: [{ inputId: "new-input", state: "queued" }],
    } as Response;
    if (method === "session/close" && this.failCloseCommand) throw this.closeCommandError;
    return {} as Response;
  }

  async sendRequest<Response>(method: string, params?: unknown): Promise<{ response: Promise<Response> }> {
    return { response: this.request<Response>(method, params) };
  }

  async notify(): Promise<void> {}
  async *events(): AsyncIterable<JsonRpcNotification> {}
  setRequestHandler(): void {}

  emit(event: JsonRpcNotification): void {
    for (const listener of this.listeners) listener(event);
  }

  subscribe(listener: (event: JsonRpcNotification) => void): () => void {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  }

  async close(): Promise<void> {
    this.closeCalls++;
    await this.closeGate;
    if (this.failClose) throw this.cleanupError;
  }
}

function setup(transport = new FakeTransport(), kv = new MemoryKV()) {
  const sandbox = new Sandbox({
    id: "workspace",
    storage: {
      getSessions: async () => [],
      getSession: async (id) => ({
        id, cwd: "/persisted/workspace", title: null, messageCount: 0,
        createdAt: "now", updatedAt: "now",
      }),
    },
    transportFactory: () => transport,
  });
  const create = (id = "agent") => new Agent({ id, path: "/requested/workspace", sandbox }, kv);
  return { agent: create(), create, sandbox, transport, kv };
}

test("startup cleanup failure retains both claims and reports both causes until close succeeds", async () => {
  const { agent, create, transport } = setup();
  transport.failStartup = true;
  transport.failClose = true;
  const failure = await agent.session.start("existing").catch((error) => error);
  expect(failure).toBeInstanceOf(AggregateError);
  expect(failure.errors).toEqual([transport.startupError, transport.cleanupError]);
  expect(failure.cause).toBe(transport.startupError);
  expect(failure.message).toContain(transport.startupError.message);
  expect(failure.message).toContain(transport.cleanupError.message);
  expect(transport.listeners.size).toBe(0);
  expect(() => agent.session.start("existing")).toThrow("cleanup-pending");
  await expect(create().session.start(null)).rejects.toBeInstanceOf(AgentClaimConflictError);
  await expect(create("other-agent").session.start("existing")).rejects.toBeInstanceOf(AgentClaimConflictError);
  await expect(agent.session.cleanupStartup()).rejects.toBe(transport.cleanupError);
  await expect(create("other-agent").session.start("existing")).rejects.toBeInstanceOf(AgentClaimConflictError);
  transport.failClose = false;
  await agent.session.cleanupStartup();
  expect(transport.closeCalls).toBe(3);
  transport.failStartup = false;
  const replacement = create();
  await replacement.session.start("existing");
  await replacement.close();
});

test("successful startup cleanup preserves the original error and allows retry", async () => {
  const { agent, transport } = setup();
  const oldDocs = agent.docs;
  transport.failStartup = true;
  await expect(agent.session.start(null)).rejects.toBe(transport.startupError);
  expect(agent.docs).not.toBe(oldDocs);
  expect(transport.closeCalls).toBe(1);
  transport.failStartup = false;
  await agent.session.start(null);
  await agent.close();
});

test("transport creation failure releases claims without a transport and allows retry", async () => {
  const { agent, sandbox, transport, create } = setup();
  const creationError = new Error("transport creation failed");
  sandbox.createTransport = async () => { throw creationError; };
  await expect(agent.session.start("existing")).rejects.toBe(creationError);
  expect(transport.closeCalls).toBe(0);
  sandbox.createTransport = async () => transport;
  const replacement = create();
  await replacement.session.start("existing");
  await replacement.close();
  await agent.session.start("existing");
  await agent.close();
});

test("concurrent close shares cleanup, retains claims on rejection and retries once", async () => {
  const { agent, transport, create } = setup();
  await agent.session.start("existing");
  const closing = gate();
  transport.closeGate = closing.promise;
  transport.failClose = true;
  const first = agent.close();
  const second = agent.close();
  expect(second).toBe(first);
  await Bun.sleep(0);
  expect(transport.closeCalls).toBe(1);
  expect(() => agent.session.send("blocked")).toThrow("not active");
  await expect(create("other-agent").session.start("existing")).rejects.toBeInstanceOf(AgentClaimConflictError);
  closing.resolve();
  await expect(first).rejects.toBe(transport.cleanupError);
  transport.failClose = false;
  const retry = agent.close();
  expect(agent.close()).toBe(retry);
  await retry;
  expect(transport.closeCalls).toBe(2);
  await agent.close();
  expect(transport.closeCalls).toBe(2);
  const replacement = create();
  await replacement.session.start("existing");
  await replacement.close();
});

for (const failStartup of [false, true]) {
  test(`startup cleanup cannot race an active startup (failure: ${failStartup})`, async () => {
    const { agent, transport } = setup();
    const starting = gate();
    transport.startupGate = starting.promise;
    transport.failStartup = failStartup;
    const started = agent.session.start("existing").catch((error) => error);
    expect(() => agent.session.cleanupStartup()).toThrow("Wait for Session startup");
    starting.resolve();
    await started;
    if (failStartup) await agent.session.cleanupStartup();
    else {
      expect(() => agent.session.cleanupStartup()).toThrow("established Session");
      await agent.close();
    }
    expect(transport.closeCalls).toBe(1);
    expect(transport.listeners.size).toBe(0);
  });
}

for (const loadResponse of [{}, { _meta: { "peri.sessionWorkspaceV1": { read_only: true } } }]) {
  test(`load accepts legacy workspace metadata without claiming ownership: ${JSON.stringify(loadResponse)}`, async () => {
    const { agent, transport } = setup();
    transport.loadResponse = loadResponse;
    await agent.session.start("existing");
    expect(agent.session.path).toBe("/persisted/workspace");
    expect(transport.calls.find((call) => call.method === "session/load")?.params).toEqual({
      cwd: "/persisted/workspace", mcpServers: [], sessionId: "existing",
    });
    await agent.close();
  });
}

test("failed startup cleanup shares retries and retains claims until confirmed", async () => {
  const { agent, transport, create } = setup();
  transport.failStartup = true;
  transport.failClose = true;
  expect(await agent.session.start("existing").catch((error) => error)).toBeInstanceOf(AggregateError);
  const closingGate = gate();
  transport.closeGate = closingGate.promise;
  const closing = agent.session.cleanupStartup();
  expect(agent.session.cleanupStartup()).toBe(closing);
  closingGate.resolve();
  await expect(closing).rejects.toBe(transport.cleanupError);
  expect(transport.closeCalls).toBe(2);
  await expect(create().session.start(null)).rejects.toBeInstanceOf(AgentClaimConflictError);
  transport.failClose = false;
  await agent.session.cleanupStartup();
  transport.failStartup = false;
  const replacement = create();
  await replacement.session.start("existing");
  await replacement.close();
});

test("claim release failure remains cleanup-pending without closing a confirmed transport twice", async () => {
  const releaseError = new Error("claim storage unavailable");
  class FailingReleaseKV extends MemoryKV {
    failRelease = true;
    override async releaseIfOwner(key: string, owner: string): Promise<boolean> {
      if (this.failRelease) throw releaseError;
      return super.releaseIfOwner(key, owner);
    }
  }
  const kv = new FailingReleaseKV();
  const { agent, transport, create } = setup(new FakeTransport(), kv);
  transport.failStartup = true;
  const failure = await agent.session.start("existing").catch((error) => error);
  expect(failure.errors).toEqual([transport.startupError, releaseError]);
  expect(() => agent.session.start(null)).toThrow("cleanup-pending");
  await expect(agent.session.cleanupStartup()).rejects.toBe(releaseError);
  expect(transport.closeCalls).toBe(1);
  kv.failRelease = false;
  await expect(create().session.start(null)).rejects.toBeInstanceOf(AgentClaimConflictError);
  await agent.session.cleanupStartup();
  expect(transport.closeCalls).toBe(1);
  transport.failStartup = false;
  const replacement = create();
  await replacement.session.start("existing");
  await replacement.close();
});

test("invalid session id leaves the session retryable", async () => {
  const { agent } = setup();
  await expect(agent.session.start("")).rejects.toThrow("nonempty");
  await agent.session.start(null);
  await agent.close();
});

test("loading history preserves the transcript without activating old execution", async () => {
  const { agent, transport } = setup();
  transport.loadEvents = [
    { method: "session/update", params: { sessionId: "existing", update: {
      sessionUpdate: "user_message_chunk", content: { type: "text", text: "old question" },
      _meta: { periReplay: true, periMessageId: "old-user" },
    } } },
    { method: "session/update", params: { sessionId: "existing", update: {
      sessionUpdate: "agent_message_chunk", content: { type: "text", text: "old answer" },
      _meta: { periReplay: true, periMessageId: "old-assistant" },
    } } },
  ];
  try {
    await agent.session.start("existing");
    await Bun.sleep(0);
    expect(transport.calls.some((call) => call.method === "session/input/enqueue" ||
      call.method === "session/input/dispatch" || call.method === "session/cancel")).toBe(false);
    expect(JSON.stringify(agent.docs.chat.toJSON())).toContain("old question");
    expect(JSON.stringify(agent.docs.chat.toJSON())).toContain("old answer");
    const info = agent.docs.session.getMap("root").get("session") as { get(key: string): unknown };
    expect(info.get("activeTurnStatus")).toBe("completed");
    expect(transport.calls.map((call) => call.method)).toEqual([
      "initialize", "session/load", "session/input/snapshot",
    ]);
  } finally { await agent.close(); }
});

test("an unobserved work notification is not part of the current ACP surface", async () => {
  const { agent, transport } = setup();
  await agent.session.start("existing");
  transport.emit({ method: "session/work/available", params: { sessionId: "existing" } });
  await Bun.sleep(0);
  expect(transport.calls.map((call) => call.method)).toEqual(["initialize", "session/load", "session/input/snapshot"]);
  await agent.close();
});

test("explicit dispatch sends the queue command for the loaded generation", async () => {
  const { agent, transport } = setup();
  await agent.session.start("existing");
  await agent.session.dispatch("new-input");
  expect(transport.calls.find((call) => call.method === "session/input/dispatch")?.params).toEqual({
    sessionId: "existing", generation: "generation-1", commandId: "new-input:dispatch", inputIds: ["new-input"],
  });
  await agent.close();
});
