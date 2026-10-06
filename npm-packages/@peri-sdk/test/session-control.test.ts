import { expect, test } from "bun:test";
import { Agent } from "../src/agent/agent";
import { SessionCloseIncompleteError, SessionCloseUnknownError, SessionControlNotAppliedError, SessionControlBlockedError, type ControlCommand, type ControlReceipt, type ControlSnapshot, type ControlResolution } from "../src/agent/session-control";
import { AgentClaimConflictError } from "../src/kv/agent-claim-conflict-error";
import { MemoryKV } from "../src/kv/memory-kv";
import { ManagedAgents } from "../src/managed/managed-agents";
import { Sandbox } from "../src/sandbox/sandbox";
import type { JsonRpcNotification, Transport } from "../src/transport/types";
import { closeCommand } from "./control-fixture";

const target = { turnId: "turn-exact", attemptId: "attempt-exact" };

class ControlTransport implements Transport {
  readonly calls: Array<{ method: string; params?: unknown }> = [];
  receipt?: ControlReceipt;
  snapshot: ControlSnapshot = {
    state: { lifecycle: 1, revision: 1, controlGeneration: 1, status: "closing", attempt: null },
    settlement: { status: "pending" },
  };
  failControl = false;
  resolution: ControlResolution = { status: "unknown" };
  closed = 0;

  async request<Response>(method: string, params?: unknown): Promise<Response> {
    this.calls.push({ method, params });
    if (method === "initialize" || method === "session/load") return {} as Response;
    if (method === "session/new") return { sessionId: "session-1" } as Response;
    if (method === "session/input/snapshot") return { generation: "generation-1" } as Response;
    if (method === "session/control/state") return structuredClone(this.snapshot) as Response;
    if (method === "session/control/resolve") return this.resolution as Response;
    if (method === "session/control") {
      const command = params as ControlCommand;
      if (!this.receipt || this.receipt.commandId !== command.commandId) {
        this.receipt = {
          sessionId: command.sessionId, commandId: command.commandId,
          decision: { kind: "accepted" },
          state: { lifecycle: 1, revision: 1, controlGeneration: 1,
            status: command.action.kind === "close" ? "closing" :
              command.action.kind === "resume" || command.action.kind === "reopen" ? "active" : "paused",
            attempt: target },
        };
      }
      if (this.failControl) throw new Error("control ACK lost");
      return this.receipt as Response;
    }
    throw new Error(`Unexpected method: ${method}`);
  }
  async sendRequest<Response>(method: string, params?: unknown): Promise<{ response: Promise<Response> }> {
    return { response: this.request<Response>(method, params) };
  }
  async notify(method: string): Promise<void> { throw new Error(`Unexpected notification: ${method}`); }
  subscribe(): () => void { return () => {}; }
  async *events(): AsyncIterable<JsonRpcNotification> {}
  setRequestHandler(): void {}
  async close(): Promise<void> { this.closed++; }
}

async function setup() {
  const transport = new ControlTransport();
  const kv = new MemoryKV();
  const sandbox = new Sandbox({
    id: "workspace", transportFactory: () => transport,
    storage: {
      deployment: () => ({ args: [], env: {} }), getSessions: async () => [],
      getSession: async (id) => ({ id, cwd: "/tmp/workspace", title: null, messageCount: 0, createdAt: "now", updatedAt: "now" }),
    },
  });
  const create = (id: string) => new Agent({ id, path: "/tmp/workspace", sandbox }, kv);
  const agent = create("original");
  await agent.session.start("session-1");
  return { agent, transport, create, kv, sandbox };
}

function settle(transport: ControlTransport): void {
  transport.snapshot.state.status = "closed";
  transport.snapshot.settlement = { status: "settled" };
}

test("Stop sends an exact execution binding and exposes the unchanged Store receipt", async () => {
  const { agent, transport } = await setup();
  const receipt = await agent.session.stop({ ...closeCommand, commandId: "stop-exact", target });
  expect(receipt).toBe(transport.receipt!);
  expect(transport.calls.filter((call) => call.method === "session/control")).toEqual([{ method: "session/control", params: {
    ...closeCommand, commandId: "stop-exact", sessionId: "session-1", action: { kind: "stop", target },
  } }]);
  expect(receipt.state.status).toBe("paused");
  const queries = transport.calls.filter((call) => call.method === "session/work/query").length;
  for (const source of ["inboxScan", "notification", "recovery", "cron"] as const)
    expect(await agent.session.ensureProcessing(source)).toEqual({ status: "blocked", reason: "sessionDomainControlBlocksAdmission" });
  expect(transport.calls.filter((call) => call.method === "session/work/query")).toHaveLength(queries);
  expect(() => agent.session.send("do not automatically continue")).toThrow("blocked by domain control");
  expect(transport.closed).toBe(0);
  const response = await agent.session.resume({ ...closeCommand, commandId: "resume-exact" });
  expect(response.state.status).toBe("active");
  expect(transport.calls.filter((call) => call.method === "session/input/dispatch")).toHaveLength(0);
});

test("Stop refuses missing identity and never constructs a random target", async () => {
  const { agent, transport } = await setup();
  expect(() => agent.session.stop({ ...closeCommand, target: { attemptId: "attempt-exact" } } as never)).toThrow("exact turnId");
  expect(transport.calls.filter((call) => call.method === "session/control")).toHaveLength(0);
});

test("Close blocked by another Unknown control does not resolve or seal a never-submitted Close identity", async () => {
  const { agent, transport, create } = await setup();
  transport.failControl = true;
  await expect(agent.session.pause({ ...closeCommand, commandId: "original-unknown-pause" })).rejects.toThrow("ACK lost");
  const calls = transport.calls.length;
  await expect(agent.close({ ...closeCommand, commandId: "new-close" })).rejects.toBeInstanceOf(SessionControlBlockedError);
  expect(transport.calls).toHaveLength(calls);
  expect(transport.closed).toBe(0);
  expect(agent.session.isClosed).toBe(false);
  await expect(create("other").session.start("session-1")).rejects.toBeInstanceOf(AgentClaimConflictError);
});

test("duplicate controls go back to Store; changed parameters cannot reuse an ID", async () => {
  const { agent, transport } = await setup();
  const first = await agent.session.pause(closeCommand);
  const second = await agent.session.pause(closeCommand);
  expect(first).toBe(second);
  expect(transport.calls.filter((call) => call.method === "session/control")).toHaveLength(2);
  expect(() => agent.session.resume(closeCommand)).toThrow("different parameters");
});

test("Close replays the identical command until domain settlement before transport shutdown", async () => {
  const { agent, transport } = await setup();
  const request = transport.request.bind(transport);
  let controls = 0;
  transport.request = async (method, params) => {
    if (method === "session/control" && ++controls === 3) settle(transport);
    return request(method, params);
  };
  const receipt = await agent.close(closeCommand, { drainTimeoutMs: 500 });
  const requests = transport.calls.filter((call) => call.method === "session/control");
  expect(requests).toHaveLength(3);
  for (const call of requests) expect(call.params).toEqual({
    ...closeCommand, sessionId: agent.session.id, action: { kind: "close" },
  });
  expect(receipt).toBe(transport.receipt!);
  expect(receipt.state.status).toBe("closing");
  expect(agent.session.isClosed).toBe(true);
  expect(transport.closed).toBe(1);
});

test("Close resolves an uncertain ACK instead of blindly replaying its mutation", async () => {
  const { agent, transport } = await setup();
  transport.failControl = true;
  const request = transport.request.bind(transport);
  transport.request = async (method, params) => {
    if (method === "session/control/resolve") {
      settle(transport);
      transport.resolution = { status: "applied", receipt: transport.receipt! };
    }
    return request(method, params);
  };
  const receipt = await agent.close(closeCommand, { drainTimeoutMs: 500 });
  expect(receipt).toBe(transport.receipt!);
  expect(transport.calls.filter((call) => call.method === "session/control")).toHaveLength(1);
  expect(transport.calls.filter((call) => call.method === "session/control/resolve")).toEqual([
    { method: "session/control/resolve", params: { ...closeCommand, sessionId: "session-1", action: { kind: "close" } } },
  ]);
  expect(transport.closed).toBe(1);
});

test("Close Unknown deadline retains the claim and NotApplied seals the original command", async () => {
  const { agent, transport, create } = await setup();
  transport.failControl = true;
  await expect(agent.close(closeCommand, { drainTimeoutMs: 0 })).rejects.toBeInstanceOf(SessionCloseUnknownError);
  expect(transport.closed).toBe(0);
  expect(() => agent.session.send("no uncertain continuation")).toThrow("blocked by domain control");
  await expect(create("other").session.start("session-1")).rejects.toBeInstanceOf(AgentClaimConflictError);
  transport.resolution = { status: "notApplied" };
  await expect(agent.close(closeCommand, { drainTimeoutMs: 100 })).rejects.toBeInstanceOf(SessionControlNotAppliedError);
  await expect(agent.close(closeCommand, { drainTimeoutMs: 100 })).rejects.toBeInstanceOf(SessionControlNotAppliedError);
  expect(transport.calls.filter((call) => call.method === "session/control")).toHaveLength(1);
  expect(transport.closed).toBe(0);
});

test("Unknown freezes execution and requires resolving the original command", async () => {
  const { agent, transport } = await setup();
  transport.failControl = true;
  await expect(agent.session.pause(closeCommand)).rejects.toThrow("ACK lost");
  expect(() => agent.session.send("unsafe retry")).toThrow("blocked by domain control");
  expect(() => agent.session.resume({ ...closeCommand, commandId: "replacement" })).toThrow("resolve the original");
  const command: ControlCommand = { ...closeCommand, sessionId: agent.session.id, action: { kind: "pause" } };
  expect(await agent.session.resolveControl(command)).toEqual({ status: "unknown" });
  expect(() => agent.session.send("still unsafe")).toThrow("blocked by domain control");
  transport.resolution = { status: "applied", receipt: transport.receipt! };
  const resolved = await agent.session.resolveControl(command);
  expect(resolved).toBe(transport.resolution);
  expect(transport.closed).toBe(0);
  expect(transport.calls.at(-1)?.params).toEqual(command);
});

for (const status of ["pending", "incomplete"] as const) {
  test(`Close ${status} retains claims, transport and original receipt`, async () => {
    const { agent, transport, create } = await setup();
    transport.snapshot.settlement = { status, reason: "Independent child has no verified handoff" };
    const error = await agent.close(closeCommand, { drainTimeoutMs: 0 }).catch((failure) => failure);
    expect(error).toBeInstanceOf(SessionCloseIncompleteError);
    expect(error.receipt).toBe(transport.receipt);
    expect(error.snapshot.settlement.status).toBe(status);
    expect(transport.closed).toBe(0);
    expect(agent.session.isClosed).toBe(false);
    await expect(create("other").session.start("session-1")).rejects.toBeInstanceOf(AgentClaimConflictError);
    settle(transport);
    const receipt = await agent.close(closeCommand, { drainTimeoutMs: 0 });
    expect(receipt).toBe(error.receipt);
    expect(receipt.state.status).toBe("closing");
    expect(transport.closed).toBe(1);
    expect(agent.session.isClosed).toBe(true);
    const replacement = create("other");
    await replacement.session.start("session-1");
  });
}

test("Closed without settled or from another lifecycle is not shutdown evidence", async () => {
  const { agent, transport } = await setup();
  transport.snapshot.state.status = "closed";
  await expect(agent.close(closeCommand, { drainTimeoutMs: 0 })).rejects.toBeInstanceOf(SessionCloseIncompleteError);
  settle(transport);
  transport.snapshot.state.lifecycle = 2;
  await expect(agent.close(closeCommand, { drainTimeoutMs: 0 })).rejects.toBeInstanceOf(SessionCloseIncompleteError);
  transport.snapshot.state.lifecycle = 1;
  transport.snapshot.state.revision = 0;
  await expect(agent.close(closeCommand, { drainTimeoutMs: 0 })).rejects.toBeInstanceOf(SessionCloseIncompleteError);
  expect(transport.closed).toBe(0);
});

test("active Close without a command and startup cleanup cannot kill the transport", async () => {
  const { agent, transport } = await setup();
  expect(() => agent.close(undefined as never)).toThrow("stable command");
  expect(() => agent.session.cleanupStartup()).toThrow("established Session");
  expect(transport.closed).toBe(0);
});

test("a receipt for an unexpected lifecycle is never shutdown authorization", async () => {
  const { agent, transport } = await setup();
  transport.receipt = {
    sessionId: "session-1", commandId: closeCommand.commandId,
    decision: { kind: "accepted" },
    state: { lifecycle: 2, revision: 1, controlGeneration: 1, status: "closing", attempt: null },
  };
  settle(transport);
  transport.snapshot.state.lifecycle = 2;
  await expect(agent.close(closeCommand, { drainTimeoutMs: 0 })).rejects.toBeInstanceOf(SessionCloseIncompleteError);
  expect(transport.closed).toBe(0);
});

test("NotApplied is obtained only by resolving the same stable mutation", async () => {
  const { agent, transport } = await setup();
  transport.failControl = true;
  await expect(agent.session.pause(closeCommand)).rejects.toThrow("ACK lost");
  transport.resolution = { status: "notApplied" };
  const command: ControlCommand = { ...closeCommand, sessionId: agent.session.id, action: { kind: "pause" } };
  expect(await agent.session.resolveControl(command)).toEqual({ status: "notApplied" });
  transport.failControl = false;
  await expect(agent.session.resume({ ...closeCommand, commandId: "authorized-resume" })).resolves.toMatchObject({ decision: { kind: "accepted" } });
});

test("mismatched receipts remain Unknown and untrusted actions never reach ACP", async () => {
  const { agent, transport } = await setup();
  expect(() => agent.session.control({ ...closeCommand, sessionId: agent.session.id,
    action: { kind: "finishClose" } } as never)).toThrow("Unsupported public control");
  const request = transport.request.bind(transport);
  transport.request = async (method, params) => {
    if (method === "session/control") return {
      sessionId: "session-1", commandId: "wrong-command", decision: { kind: "accepted" },
      state: { lifecycle: 1, revision: 1, controlGeneration: 1, status: "active", attempt: null },
    } as never;
    return request(method, params);
  };
  await expect(agent.session.pause(closeCommand)).rejects.toThrow("does not match");
  expect(() => agent.session.send("do not infer success")).toThrow("blocked by domain control");
  expect(transport.closed).toBe(0);
});

test("missing closeAll commands report errors without releasing any claim", async () => {
  const { agent, kv, sandbox, transport } = await setup();
  settle(transport);
  await agent.close(closeCommand, { drainTimeoutMs: 0 });
  const manager = new ManagedAgents({ kv });
  const managed = manager.createAgent({ id: "managed", path: "/tmp/workspace", sandbox });
  await managed.session.start(null);
  await expect(manager.closeAll(new Map())).rejects.toBeInstanceOf(AggregateError);
  expect(transport.closed).toBe(1);
  expect(() => manager.createAgent({ id: "managed", path: "/tmp/workspace", sandbox })).toThrow("already declared");
});

test("Close settlement cannot race a new Reopen command on this SDK transport", async () => {
  const { agent, transport } = await setup();
  settle(transport);
  let unblock!: () => void;
  let reached!: () => void;
  const gate = new Promise<void>((resolve) => { unblock = resolve; });
  const observing = new Promise<void>((resolve) => { reached = resolve; });
  const request = transport.request.bind(transport);
  transport.request = async (method, params) => {
    if (method === "session/control/state") { reached(); await gate; }
    return request(method, params);
  };
  const closing = agent.close(closeCommand, { drainTimeoutMs: 0 });
  await observing;
  expect(() => agent.session.reopen({ ...closeCommand, commandId: "racing-reopen" })).toThrow("Close is in progress");
  expect(transport.calls.filter((call) => call.method === "session/control")).toHaveLength(1);
  unblock();
  await closing;
  expect(transport.closed).toBe(1);
});

test("rejected stale controls do not cancel, resume or kill locally", async () => {
  const { agent, transport } = await setup();
  transport.receipt = { sessionId: "session-1", commandId: closeCommand.commandId,
    decision: { kind: "rejected", reason: "staleAttempt" },
    state: { lifecycle: 1, revision: 5, controlGeneration: 4, status: "paused", attempt: target } };
  expect(await agent.session.stop({ ...closeCommand, target })).toBe(transport.receipt);
  expect(() => agent.session.send("old resume must not revive")).toThrow("blocked by domain control");
  expect(transport.closed).toBe(0);
});

test("ManagedAgents closeAll reports incomplete closure and retains the Agent", async () => {
  const { agent, kv, sandbox, transport } = await setup();
  settle(transport);
  await agent.close(closeCommand, { drainTimeoutMs: 0 });
  transport.snapshot.state.status = "closing";
  transport.snapshot.settlement = { status: "incomplete", reason: "Independent is still process resident" };
  const manager = new ManagedAgents({ kv });
  const managed = manager.createAgent({ id: "managed", path: "/tmp/workspace", sandbox });
  await managed.session.start(null);
  await expect(manager.closeAll(new Map([["managed", closeCommand]]), { drainTimeoutMs: 0 })).rejects.toBeInstanceOf(AggregateError);
  expect(() => manager.createAgent({ id: "managed", path: "/tmp/workspace", sandbox })).toThrow("already declared");
  expect(transport.closed).toBe(1);
});
