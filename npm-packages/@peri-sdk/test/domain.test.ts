import { closeCommand, controlResponse } from "./control-fixture";
import { describe, expect, test } from "bun:test";
import { AgentClaimConflictError } from "../src/kv/agent-claim-conflict-error";
import { MemoryKV } from "../src/kv/memory-kv";
import type { AtomicManagedAgentKv } from "../src/kv/types";
import { ManagedAgents } from "../src/managed/managed-agents";
import { Sandbox } from "../src/sandbox/sandbox";
import type { JsonRpcNotification, Transport } from "../src/transport/types";
import type { SessionStorage } from "../src/storage/types";
import { readSessionView } from "../src/view/session-view";
import { EventStreamOverflowError } from "../src/transport/notification-budget";

const storedSession = (cwd: string, id = "existing") => ({
  id, title: null, cwd, messageCount: 0, createdAt: "now", updatedAt: "now",
});
const storage = (cwd: string): SessionStorage => ({
  deployment: () => ({ args: [], env: {} }),
  getSessions: async () => [storedSession(cwd)],
  getSession: async (id) => storedSession(cwd, id),
});

class MemoryClaims implements AtomicManagedAgentKv {
  readonly owners = new Map<string, string>();
  async claimIfAbsent(key: string, owner: string): Promise<boolean> {
    await Promise.resolve();
    if (this.owners.has(key)) return false;
    this.owners.set(key, owner);
    return true;
  }
  async releaseIfOwner(key: string, owner: string): Promise<boolean> {
    if (this.owners.get(key) !== owner) return false;
    this.owners.delete(key);
    return true;
  }
}

test("MemoryKV claims once and releases only for its owner", async () => {
  const kv = new MemoryKV();
  const results = await Promise.all([
    kv.claimIfAbsent("agent", "first"),
    kv.claimIfAbsent("agent", "second"),
  ]);
  expect(results).toEqual([true, false]);
  expect(await kv.releaseIfOwner("agent", "second")).toBe(false);
  expect(await kv.releaseIfOwner("agent", "first")).toBe(true);
  expect(await kv.claimIfAbsent("agent", "second")).toBe(true);
});

test("Agent lists Sessions from Sandbox storage without starting ACP", async () => {
  let transportStarted = false;
  let queriedCwd = "";
  const sandbox = new Sandbox({
    id: "workspace-1",
    storage: {
      deployment: () => ({ args: [], env: {} }),
      getSessions: async (cwd) => {
        queriedCwd = cwd;
        return [{ id: "session-1", title: null, cwd, messageCount: 0, createdAt: "now", updatedAt: "now" }];
      },
      getSession: async () => null,
    },
    transportFactory: () => { transportStarted = true; throw new Error("ACP must not start"); },
  });
  const agent = new ManagedAgents({ kv: new MemoryKV() }).createAgent({ path: "/tmp/workspace", id: "agent-1", sandbox });
  expect((await agent.getSessions()).map((entry) => entry.id)).toEqual(["session-1"]);
  expect(queriedCwd).toBe(agent.path);
  expect(transportStarted).toBe(false);
});

class FakeTransport implements Transport {
  readonly calls: Array<{ method: string; params?: any }> = [];
  readonly listeners = new Set<(event: JsonRpcNotification) => void>();
  closed = false;
  failOn?: string;
  enqueueGate?: Promise<void>;
  newSessionId = "session-1";

  async request<T>(method: string, params?: unknown): Promise<T> {
    if (method.startsWith("session/control")) return controlResponse(method, params) as T;
    this.calls.push({ method, params });
    if (method === this.failOn) throw new Error(`failed: ${method}`);
    switch (method) {
      case "initialize": return { protocolVersion: 1 } as T;
      case "session/new": return { sessionId: this.newSessionId } as T;
      case "session/load": return {} as T;
      case "session/input/snapshot": return { generation: "generation-1" } as T;
      case "session/work/query": return { control: { lifecycle: 1, revision: 0, controlGeneration: 0, status: "active", attempt: null }, work: null } as T;
      case "session/input/enqueue":
        await this.enqueueGate;
        return { results: [{ inputId: (params as any).inputId, state: "queued" }], workReceipts: [{ decision: { kind: "accepted" } }] } as T;
      case "session/input/dispatch": return { results: [{ inputId: (params as any).inputIds[0], state: "dispatching" }], workReceipts: [{ decision: { kind: "accepted" } }] } as T;
      case "session/input/takeback": return { takenBack: { inputId: (params as any).inputId } } as T;
      case "session/list": return { sessions: [{ sessionId: "session-1" }] } as T;
      default: throw new Error(`unexpected method: ${method}`);
    }
  }
  async sendRequest<T>(method: string, params?: unknown): Promise<{ response: Promise<T> }> {
    return { response: this.request<T>(method, params) };
  }
  async notify(method: string, params?: unknown): Promise<void> { this.calls.push({ method, params }); }
  subscribe(listener: (event: JsonRpcNotification) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }
  emit(event: JsonRpcNotification): void { for (const listener of this.listeners) listener(event); }
  async *events(): AsyncIterable<JsonRpcNotification> { /* no events in domain tests */ }
  setRequestHandler(): void {}
  async close(): Promise<void> { this.closed = true; }
}

function declaration(manager: ManagedAgents, id = "agent-1", transport = new FakeTransport()) {
  let created = 0;
  const agent = manager.createAgent({
    path: "/tmp/workspace",
    id,
    sandbox: new Sandbox({
      id: "workspace-1",
      storage: storage("/tmp/workspace"),
      transportFactory: () => { created++; return transport; },
    }),
    instructions: "Be helpful",
    execution: { database: `/tmp/peri-domain-test-${crypto.randomUUID()}.db` },
  });
  return { agent, transport, get created() { return created; } };
}

describe("ManagedAgents lifecycle", () => {
  test("loading routes durable recovery through SDK work query without a second publication", async () => {
    const { agent, transport } = declaration(new ManagedAgents({ kv: new MemoryClaims() }));
    await agent.session.start("existing");
    await agent.session.ensureProcessing("recovery");
    expect(transport.calls.some((call) => call.method === "session/work/query")).toBe(true);
    expect(transport.calls.some((call) => call.method === "session/input/enqueue" || call.method === "session/input/dispatch")).toBe(false);
    expect(transport.closed).toBe(false);
  });

  test("declaration is synchronous and does not claim or create transport", () => {
    const kv = new MemoryClaims();
    const manager = new ManagedAgents({ kv });
    const result = declaration(manager);
    expect(result.agent.id).toBe("agent-1");
    expect(result.created).toBe(0);
    expect(kv.owners.size).toBe(0);
  });

  test("plain unstorage Storage is rejected because it has no atomic claim", () => {
    expect(() => new ManagedAgents({ kv: { getItem: async () => null, setItem: async () => {} } as any }))
      .toThrow("atomic claimIfAbsent/releaseIfOwner");
  });

  test("same manager rejects a duplicate declaration before start", () => {
    const manager = new ManagedAgents({ kv: new MemoryClaims() });
    declaration(manager);
    expect(() => declaration(manager)).toThrow("already declared");
  });

  test("two managers racing for the same Agent: exactly one starts", async () => {
    const kv = new MemoryClaims();
    const first = declaration(new ManagedAgents({ kv }));
    const second = declaration(new ManagedAgents({ kv }));
    const results = await Promise.allSettled([first.agent.session.start(null), second.agent.session.start(null)]);
    expect(results.filter((result) => result.status === "fulfilled")).toHaveLength(1);
    const loser = results.find((result) => result.status === "rejected") as PromiseRejectedResult;
    expect(loser.reason).toBeInstanceOf(AgentClaimConflictError);
    expect(first.created + second.created).toBe(1);
  });

  test("the same Agent id in different Workspaces uses separate claims", async () => {
    const kv = new MemoryClaims();
    const first = declaration(new ManagedAgents({ kv }));
    const secondTransport = new FakeTransport();
    secondTransport.newSessionId = "session-2";
    const second = new ManagedAgents({ kv }).createAgent({
    path: "/tmp/workspace",
      id: "agent-1",
      sandbox: new Sandbox({
        id: "workspace-2",
        transportFactory: () => secondTransport,
      }),
    });
    await Promise.all([first.agent.session.start(null), second.session.start(null)]);
    expect(kv.owners.size).toBe(4);
    expect([...kv.owners.keys()].some((key) => key.includes("workspace-1:agent:agent-1"))).toBe(true);
    expect([...kv.owners.keys()].some((key) => key.includes("workspace-2:agent:agent-1"))).toBe(true);
  });

  test("different Agents cannot claim the same existing Session", async () => {
    const kv = new MemoryClaims();
    const first = declaration(new ManagedAgents({ kv }), "agent-1");
    const second = declaration(new ManagedAgents({ kv }), "agent-2");
    const results = await Promise.allSettled([
      first.agent.session.start("existing"),
      second.agent.session.start("existing"),
    ]);
    expect(results.filter((result) => result.status === "fulfilled")).toHaveLength(1);
    expect(results.find((result) => result.status === "rejected")?.reason).toBeInstanceOf(AgentClaimConflictError);
    expect(kv.owners.size).toBe(2);
  });

  test("failed start closes transport and releases only its own claims", async () => {
    const kv = new MemoryClaims();
    const manager = new ManagedAgents({ kv });
    const transport = new FakeTransport();
    transport.failOn = "session/new";
    const { agent } = declaration(manager, "agent-1", transport);
    await expect(agent.session.start(null)).rejects.toThrow("failed: session/new");
    expect(transport.closed).toBe(true);
    expect(kv.owners.size).toBe(0);
  });

  test("close releases claims after process closes; another instance can start", async () => {
    const kv = new MemoryClaims();
    const firstManager = new ManagedAgents({ kv });
    const first = declaration(firstManager);
    await first.agent.session.start(null);
    expect(kv.owners.size).toBe(2);
    await firstManager.closeAgent("agent-1", closeCommand);
    expect(first.transport.closed).toBe(true);
    expect(kv.owners.size).toBe(0);
    const second = declaration(new ManagedAgents({ kv }));
    await second.agent.session.start(null);
    expect(second.created).toBe(1);
  });

  test("send awaits delivery, allows more input, and forceSend dispatches", async () => {
    const manager = new ManagedAgents({ kv: new MemoryClaims() });
    const { agent, transport } = declaration(manager);
    const session = await agent.session.start(null);
    const first = session.send("first");
    let firstResolved = false;
    void Promise.resolve(first).then(() => { firstResolved = true; });
    await Promise.resolve();
    expect(firstResolved).toBe(false);
    transport.emit(delivered(first.inputId));
    await first;
    const second = session.send("second");
    expect(second.isSent).toBe(false);
    await second.forceSend();
    expect(second.isSent).toBe(false);
    transport.emit(delivered(second.inputId));
    await second;
    expect(second.isSent).toBe(true);
    expect(transport.calls.filter((call) => call.method === "session/input/enqueue")).toHaveLength(2);
    expect(transport.calls.some((call) => call.method === "session/input/dispatch")).toBe(true);
  });

  test("delivery event arriving before enqueue receipt still resolves send", async () => {
    const manager = new ManagedAgents({ kv: new MemoryClaims() });
    const { agent, transport } = declaration(manager);
    const session = await agent.session.start(null);
    let unblock!: () => void;
    transport.enqueueGate = new Promise((resolve) => { unblock = resolve; });
    const receipt = session.send("hello");
    transport.emit(delivered(receipt.inputId));
    await receipt;
    expect(receipt.isSent).toBe(true);
    unblock();
  });

  test("an unclaimed failure does not automatically republish or restart input", async () => {
    const { agent, transport } = declaration(new ManagedAgents({ kv: new MemoryClaims() }));
    const session = await agent.session.start(null);
    const receipt = session.send("hello");
    transport.emit(queueChanged(receipt.inputId, "dispatching"));
    transport.emit(queueChanged(receipt.inputId, "queued"));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(transport.calls.filter((call) => call.method === "session/input/dispatch")).toHaveLength(0);
    transport.emit(delivered(receipt.inputId));
    await receipt;
    expect(receipt.isSent).toBe(true);
  });

  test("repeated unknown execution preserves publication rather than withdrawing it", async () => {
    const { agent, transport } = declaration(new ManagedAgents({ kv: new MemoryClaims() }));
    const session = await agent.session.start(null);
    const receipt = session.send("hello");
    transport.emit(queueChanged(receipt.inputId, "dispatching"));
    transport.emit(queueChanged(receipt.inputId, "queued"));
    await new Promise((resolve) => setTimeout(resolve, 0));
    transport.emit(queueChanged(receipt.inputId, "dispatching"));
    transport.emit(queueChanged(receipt.inputId, "queued"));
    expect(transport.calls.some((call) => call.method === "session/input/takeback")).toBe(false);
    expect(transport.calls.some((call) => call.method === "session/input/dispatch")).toBe(false);
    expect(receipt.isSent).toBe(false);
    transport.emit(delivered(receipt.inputId));
    await receipt;
  });

  test("close rejects input waiting for delivery", async () => {
    const manager = new ManagedAgents({ kv: new MemoryClaims() });
    const { agent } = declaration(manager);
    const session = await agent.session.start(null);
    const result = Promise.resolve(session.send("hello"));
    await manager.closeAgent("agent-1", closeCommand);
    await expect(result).rejects.toThrow("closed before user input was delivered");
  });

  test("a delivery event from an older mailbox generation cannot complete send", async () => {
    const manager = new ManagedAgents({ kv: new MemoryClaims() });
    const { agent, transport } = declaration(manager);
    const session = await agent.session.start(null);
    const receipt = session.send("hello");
    let resolved = false;
    void Promise.resolve(receipt).then(() => { resolved = true; });
    const stale = delivered(receipt.inputId);
    (stale.params as any).event_json = JSON.stringify({
      type: "user_input_delivered",
      value: { input_id: receipt.inputId, generation: "old-generation" },
    });
    transport.emit(stale);
    await Promise.resolve();
    expect(resolved).toBe(false);
    transport.emit(delivered(receipt.inputId));
    await receipt;
    expect(receipt.isSent).toBe(true);
  });

  test("enqueue failure rejects the send receipt and keeps it unsent", async () => {
    const manager = new ManagedAgents({ kv: new MemoryClaims() });
    const { agent, transport } = declaration(manager);
    const session = await agent.session.start(null);
    transport.failOn = "session/input/enqueue";
    const receipt = session.send("hello");
    await expect(Promise.resolve(receipt)).rejects.toThrow("failed: session/input/enqueue");
    expect(receipt.isSent).toBe(false);
  });

  test("stream receives same-session events emitted before consumption", async () => {
    const manager = new ManagedAgents({ kv: new MemoryClaims() });
    const { agent, transport } = declaration(manager);
    const session = await agent.session.start(null);
    transport.emit({ jsonrpc: "2.0", method: "session/update", params: { sessionId: "session-1", update: { kind: "first" } } });
    transport.emit({ jsonrpc: "2.0", method: "session/update", params: { sessionId: "another", update: { kind: "other" } } });
    transport.emit(delivered("unrelated-input"));
    const iterator = session.stream()[Symbol.asyncIterator]();
    const next = await iterator.next();
    expect((next.value.params as any).update.kind).toBe("first");
    const second = await iterator.next();
    expect(second.value.method).toBe("peri/agent_event");
    await iterator.return?.();
    await manager.closeAgent("agent-1", closeCommand);
  });

  test("raw stream overflow leaves projection current and allows a new live diagnostic stream", async () => {
    const manager = new ManagedAgents({ kv: new MemoryClaims() });
    const { agent, transport } = declaration(manager);
    const session = await agent.session.start(null);
    const emit = (sessionUpdate: string, text: string) => transport.emit({ jsonrpc: "2.0", method: "session/update",
      params: { sessionId: "session-1", update: { sessionUpdate, content: { text } } } });
    emit("user_message_chunk", "go");
    for (let i = 0; i < 1050; i++) emit("agent_message_chunk", ".");
    const previous = session.stream()[Symbol.asyncIterator]();
    await expect(previous.next()).rejects.toBeInstanceOf(EventStreamOverflowError);
    expect(readSessionView(session.docs.chat, session.docs.session).entries[1]?.blocks[0]).toMatchObject({ text: ".".repeat(1050) });
    const current = session.stream()[Symbol.asyncIterator]();
    const next = current.next(); emit("agent_message_chunk", "tail");
    expect((await next).value.params.update.content.text).toBe("tail");
    await current.return?.(); await manager.closeAgent(agent.id, closeCommand);
  });

  test("ACP setup serializes HTTP headers and stdio env as named entries", async () => {
    const kv = new MemoryClaims();
    const transport = new FakeTransport();
    const sandbox = new Sandbox({
      id: "workspace-1",
      storage: storage("/tmp/workspace"),
      workspace: { url: "https://workspace.test/mcp", headers: { Authorization: "Bearer fixture" } },
      transportFactory: () => transport,
    });
    const agent = new ManagedAgents({ kv }).createAgent({
    path: "/tmp/workspace",
      id: "agent-1",
      sandbox,
      instructions: "test instructions",
      mcpServers: { other: { type: "stdio", command: "mcp-server", env: { MODE: "test" } } },
    });
    await agent.session.start(null);
    const setup = transport.calls.find((call) => call.method === "session/new")?.params as any;
    expect(setup.mcpServers).toEqual([
      { name: "workspace", type: "http", url: "https://workspace.test/mcp", headers: [{ name: "Authorization", value: "Bearer fixture" }] },
      { name: "other", type: "stdio", command: "mcp-server", env: [{ name: "MODE", value: "test" }] },
    ]);
    expect(setup._meta).toEqual({ "peri.instructions": "test instructions" });
  });

  test("loading a Session preserves its frozen instructions", async () => {
    const manager = new ManagedAgents({ kv: new MemoryClaims() });
    const { agent, transport } = declaration(manager);
    const session = await agent.session.start("existing");
    expect(session.id).toBe("existing");
    const load = transport.calls.find((call) => call.method === "session/load")?.params as any;
    expect(load).toEqual({ cwd: agent.path, mcpServers: [], sessionId: "existing" });
  });

  test("loading uses the stored path without an Agent path", async () => {
    const transport = new FakeTransport();
    const sandbox = new Sandbox({
      id: "workspace-1",
      storage: storage("/persisted/workspace"),
      transportFactory: () => transport,
    });
    const agent = new ManagedAgents({ kv: new MemoryClaims() }).createAgent({ id: "loaded", sandbox });
    await agent.session.start("existing");
    expect(transport.calls.find((call) => call.method === "session/load")?.params)
      .toEqual({ cwd: "/persisted/workspace", mcpServers: [], sessionId: "existing" });
    expect(agent.session.path).toBe("/persisted/workspace");
    expect((await agent.getSessions())[0]?.cwd).toBe("/persisted/workspace");
  });

  test("creating a Session requires an Agent path before starting ACP", async () => {
    let started = false;
    const sandbox = new Sandbox({ id: "workspace-1", transportFactory: () => { started = true; return new FakeTransport(); } });
    const agent = new ManagedAgents({ kv: new MemoryClaims() }).createAgent({ id: "new", sandbox });
    await expect(agent.session.start(null)).rejects.toThrow("Agent path is required");
    expect(started).toBe(false);
  });

  test("unknown Session ID fails before starting ACP", async () => {
    let started = false;
    const sandbox = new Sandbox({
      id: "workspace-1",
      storage: { ...storage("/tmp/workspace"), getSession: async () => null },
      transportFactory: () => { started = true; return new FakeTransport(); },
    });
    const agent = new ManagedAgents({ kv: new MemoryClaims() }).createAgent({ id: "missing", sandbox });
    await expect(agent.session.start("unknown")).rejects.toThrow("Session not found: unknown");
    expect(started).toBe(false);
  });
});

function delivered(inputId: string): JsonRpcNotification {
  return {
    jsonrpc: "2.0",
    method: "peri/agent_event",
    params: {
      sessionId: "session-1",
      event_json: JSON.stringify({ type: "user_input_delivered", value: { input_id: inputId, generation: "generation-1", content: "hello" } }),
    },
  };
}

function queueChanged(inputId: string, state: string): JsonRpcNotification {
  return {
    jsonrpc: "2.0",
    method: "peri/agent_event",
    params: {
      sessionId: "session-1",
      event_json: JSON.stringify({
        type: "user_input_queue_changed",
        value: { snapshot: { generation: "generation-1", items: [{ inputId, state }] } },
      }),
    },
  };
}
