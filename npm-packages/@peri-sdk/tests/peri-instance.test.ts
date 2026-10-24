import { expect, test } from "bun:test";
import type { ChatFact } from "../src/chat/facts";
import { InteractionAnswerError } from "../src/agent/interaction-broker";
import {
  PeriHostCleanupError,
  PeriInstance,
  PeriInstanceBindError,
  PeriInstanceCloseIncompleteError,
  PeriInstanceNotBoundError,
} from "../src/agent/peri-instance";
import { HistoryPageUnavailableError, StaleHistoryPageError } from "../src/state/history-browser";
import { SessionCloseIncompleteError } from "../src/agent/session-close";
import { RpcError } from "../src/transport/rpc-error";
import type { JsonRpcNotification, ReverseRequestHandler, Transport } from "../src/transport/types";

class FakeHost {
  closed = 0;
  confirmClose = true;
  failFirstClose = false;

  constructor(readonly transport: FakeTransport) {}

  async close(): Promise<{ confirmed: boolean; error?: unknown }> {
    this.closed++;
    await this.transport.close();
    if (this.failFirstClose && this.closed === 1)
      return { confirmed: false, error: new Error("host close unconfirmed") };
    if (!this.confirmClose) return { confirmed: false, error: new Error("host close unconfirmed") };
    return { confirmed: true };
  }
}

type QueueReceipt = {
  results: Array<{ inputId: string; state: string }>;
  snapshot?: unknown;
};

class FakeTransport implements Transport {
  readonly calls: Array<{ method: string; params?: unknown }> = [];
  readonly listeners = new Set<(event: JsonRpcNotification) => void>();
  readonly queue = { generation: "generation-1", revision: 0, items: [] as Array<{ inputId: string; state: string }> };
  loadResponse: unknown = { sessionId: "loaded", snapshotToken: "snapshot-1", nextCursor: "cursor-1" };
  replayTexts: string[] = [];
  historyPages = new Map<string, unknown>();
  bgTasksResponse: unknown = { revision: 1, tasks: [] };
  bgTasksRequests = 0;
  closeRequests = 0;
  closeIncompleteTimes = 0;
  wireCloses = 0;
  failSnapshot?: Error;
  failLoad?: Error;
  failNew?: Error;
  failRename?: Error;
  failCloseWire = false;
  reverseHandler?: ReverseRequestHandler;
  pageGate?: Promise<void>;
  initializeGate?: Promise<void>;
  newGate?: Promise<void>;
  loadGate?: Promise<void>;
  renameGate?: Promise<void>;

  async request<Response>(method: string, params?: unknown): Promise<Response> {
    this.calls.push({ method, params });
    if (method === "initialize") {
      await this.initializeGate;
      return { protocolVersion: 1 } as Response;
    }
    if (method === "session/new") {
      await this.newGate;
      if (this.failNew) throw this.failNew;
      return { sessionId: "session-new", configOptions: [] } as Response;
    }
    if (method === "session/load") {
      const value = params as { loadGeneration?: string; cursor?: string } | undefined;
      if (value?.cursor !== undefined) {
        await this.pageGate;
        const page = this.historyPages.get(value.cursor);
        if (page === undefined) throw new Error(`unexpected cursor: ${value.cursor}`);
        return page as Response;
      }
      await this.loadGate;
      if (this.failLoad) throw this.failLoad;
      for (const text of this.replayTexts) {
        this.emit({
          method: "session/update",
          params: {
            sessionId: "loaded",
            update: {
              sessionUpdate: "agent_message_chunk",
              content: { type: "text", text },
              _meta: { periReplay: true, periLoadGeneration: value?.loadGeneration },
            },
          },
        });
      }
      return this.loadResponse as Response;
    }
    if (method === "session/input/snapshot") {
      if (this.failSnapshot) throw this.failSnapshot;
      return this.queue as Response;
    }
    if (method === "session/input/enqueue") {
      const inputId = (params as { inputId: string }).inputId;
      this.queue.items.push({ inputId, state: "queued" });
      this.queue.revision++;
      return { results: [{ inputId, state: "queued" }], snapshot: structuredClone(this.queue) } as Response;
    }
    if (method === "session/input/dispatch") {
      const inputId = (params as { inputIds: string[] }).inputIds[0]!;
      return { results: [{ inputId, state: "dispatching" }], snapshot: structuredClone(this.queue) } as Response;
    }
    if (method === "session/input/takeback") {
      const inputId = (params as { inputId: string }).inputId;
      this.queue.items = this.queue.items.filter((item) => item.inputId !== inputId);
      this.queue.revision++;
      return { results: [{ inputId, state: "withdrawn" }], snapshot: structuredClone(this.queue) } as Response;
    }
    if (method === "session/bg-tasks") {
      this.bgTasksRequests++;
      return this.bgTasksResponse as Response;
    }
    if (method === "session/rename") {
      await this.renameGate;
      if (this.failRename) throw this.failRename;
      return {} as Response;
    }
    if (method === "session/close") {
      this.closeRequests++;
      if (this.closeRequests <= this.closeIncompleteTimes)
        throw new RpcError(-32010, `Session close incomplete: attempt ${this.closeRequests}`);
      return {} as Response;
    }
    throw new Error(`unexpected method: ${method}`);
  }

  async sendRequest<Response>(method: string, params?: unknown): Promise<{ response: Promise<Response> }> {
    return { response: this.request<Response>(method, params) };
  }

  async notify(method: string, params?: unknown): Promise<void> {
    this.calls.push({ method, params });
  }

  async *events(): AsyncIterable<JsonRpcNotification> {}

  subscribe(listener: (event: JsonRpcNotification) => void): () => void {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  }

  setRequestHandler(handler: ReverseRequestHandler): void {
    this.reverseHandler = handler;
  }

  async close(): Promise<void> {
    this.wireCloses++;
    if (this.failCloseWire) throw new Error("wire close unconfirmed");
  }

  emit(event: JsonRpcNotification): void {
    for (const listener of this.listeners) listener(event);
  }
}

function setup(options: {
  payloadBudgets?: { inlineBytes?: number; externalBytes?: number; maxPayloadBytes?: number };
  payloadNonce?: string;
  close?: { drainTimeoutMs?: number; bindingSettleTimeoutMs?: number; activationCloseTimeoutMs?: number };
  resolvePath?: (sessionId: string) => Promise<string | null>;
  acquireHost?: (path: string, options: { signal: AbortSignal }) => Promise<FakeHost>;
} = {}) {
  const transport = new FakeTransport();
  const host = new FakeHost(transport);
  const instance = new PeriInstance({
    acquireHost: options.acquireHost ?? (async () => host),
    resolveSessionPath: options.resolvePath ?? (async (sessionId) =>
      sessionId === "missing" ? null : `/persisted/${sessionId}`),
    payloadBudgets: options.payloadBudgets,
    payloadNonce: options.payloadNonce ?? "test-nonce",
    close: options.close ?? { drainTimeoutMs: 50 },
  });
  const facts: ChatFact[] = [];
  const detach = instance.attach((fact) => facts.push(fact));
  return { transport, host, instance, facts, detach };
}

function deferred(): { promise: Promise<void>; resolve: () => void } {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => { resolve = done; });
  return { promise, resolve };
}

async function waitForCall(transport: FakeTransport, method: string): Promise<void> {
  const deadline = Date.now() + 1_000;
  while (!transport.calls.some((call) => call.method === method)) {
    if (Date.now() > deadline) throw new Error(`Timed out waiting for ${method}`);
    await Bun.sleep(1);
  }
}

function agentEvent(type: string, value: Record<string, unknown>): JsonRpcNotification {
  return {
    method: "peri/agent_event",
    params: { sessionId: "session-new", event_json: JSON.stringify({ type, value }) },
  };
}

function agentDone(stopReason: string, requestId?: string): JsonRpcNotification {
  return {
    method: "peri/agent_event_done",
    params: {
      sessionId: "session-new",
      stopReason,
      ...(requestId ? { requestId } : {}),
    },
  };
}

function sessionUpdate(update: Record<string, unknown>, sessionId = "session-new"): JsonRpcNotification {
  return { method: "session/update", params: { sessionId, update } };
}

const factKinds = (facts: ChatFact[]): string[] => facts.map((fact) => fact.kind);

test("new session: seed config, queue, send, delivery, turn completion and close evidence", async () => {
  const { transport, host, instance, facts } = setup();
  const bound = await instance.bindNew({ path: "/workspace", title: "New chat", instructions: "be brief" });
  expect(bound.sessionId).toBe("session-new");
  expect(transport.calls.map((call) => call.method)).toEqual([
    "initialize", "session/new", "session/input/snapshot", "session/rename",
  ]);
  expect(transport.calls.find((call) => call.method === "session/new")?.params).toMatchObject({
    cwd: "/workspace",
    mcpServers: [],
    _meta: { "peri.instructions": "be brief" },
  });
  expect(factKinds(facts)).toEqual(["seed-config", "input-queue", "run-state"]);
  expect(facts[2]).toEqual({ kind: "run-state", running: false, executionBlocked: false });

  const waiting = instance.waitForDelivery("input-1");
  await instance.send("input-1", "hello");
  expect(instance.pendingInputCount).toBe(1);
  transport.emit(agentEvent("user_input_run_started", { request_id: "request-1" }));
  transport.emit(agentEvent("user_input_delivered", { input_id: "input-1", generation: "generation-1" }));
  await waiting;
  expect(instance.pendingInputCount).toBe(0);
  expect(instance.running).toBe(true);
  expect(facts).toContainEqual({ kind: "input-delivered", inputId: "input-1", text: "hello" });

  await instance.dispatch("input-1");
  await instance.takeBack("input-1");
  expect(transport.calls.some((call) => call.method === "session/input/dispatch")).toBe(true);
  expect(transport.calls.some((call) => call.method === "session/input/takeback")).toBe(true);

  expect(await instance.cancel()).toBe("requested");
  expect(transport.calls.some((call) => call.method === "session/cancel")).toBe(true);
  expect(facts).toContainEqual({ kind: "turn-cancel", restore: false });

  transport.emit(agentDone("end_turn", "request-1"));
  expect(instance.running).toBe(false);
  expect(facts).toContainEqual({ kind: "turn-complete", status: "completed", stopReason: "end_turn" });
  expect(await instance.cancel()).toBe("idle");

  await instance.close();
  expect(transport.closeRequests).toBe(1);
  expect(host.closed).toBe(1);
  expect(instance.needsReload).toBe(false);
  expect(instance.running).toBe(false);
  expect(facts).toContainEqual({ kind: "closed" });

  await instance.close();
  expect(transport.closeRequests).toBe(1);
  expect(host.closed).toBe(1);
  expect(facts.filter((fact) => fact.kind === "closed")).toHaveLength(1);
});

test("load: history batch precedes live notices and pages use the runtime cursor", async () => {
  const { transport, instance, facts } = setup();
  transport.replayTexts = ["old answer"];
  const bound = await instance.bindExisting("loaded");
  expect(bound.sessionId).toBe("loaded");
  expect(transport.calls.find((call) => call.method === "session/load")?.params).toMatchObject({
    cwd: "/persisted/loaded",
    sessionId: "loaded",
    mcpServers: [],
    loadGeneration: expect.any(String),
  });
  expect(factKinds(facts).slice(0, 3)).toEqual(["seed-config", "history-batch", "input-queue"]);
  const batch = facts.find((fact) => fact.kind === "history-batch");
  expect(JSON.stringify(batch)).toContain("old answer");

  transport.historyPages.set("cursor-1", {
    sessionId: "loaded",
    snapshotToken: "snapshot-1",
    nextCursor: "cursor-0",
    items: [{
      entryId: "loaded:payload-1",
      position: 4,
      subItems: [{ position: 0, update: {
        sessionUpdate: "agent_message_chunk", content: { type: "text", text: "page" },
      } }],
    }],
  });
  const page = await instance.loadEarlierHistory(5);
  expect(page).toEqual({ applied: 1, nextCursor: "cursor-0" });
  expect(facts).toContainEqual({
    kind: "history-page",
    nextCursor: "cursor-0",
    items: [expect.objectContaining({ entryId: "loaded:payload-1" })],
  });

  // 同一 cursor + limit 的并发请求合并（当前游标已推进到 cursor-0）。
  transport.historyPages.set("cursor-0", {
    sessionId: "loaded", snapshotToken: "snapshot-1", nextCursor: null, items: [],
  });
  const [first, second] = await Promise.all([
    instance.loadEarlierHistory(5),
    instance.loadEarlierHistory(5),
  ]);
  expect(first).toEqual(second);
  expect(transport.calls.filter((call) =>
    call.method === "session/load" && (call.params as { cursor?: string }).cursor === "cursor-0"))
    .toHaveLength(1);
  await instance.close();
});

test("history pages reject another session and an invalidated window", async () => {
  const { transport, instance } = setup();
  await instance.bindExisting("loaded");
  transport.historyPages.set("cursor-1", {
    sessionId: "someone-else", snapshotToken: "snapshot-1", nextCursor: null, items: [],
  });
  await expect(instance.loadEarlierHistory()).rejects.toBeInstanceOf(StaleHistoryPageError);
  transport.historyPages.set("cursor-1", {
    sessionId: "loaded", snapshotToken: "snapshot-1", nextCursor: null, items: [],
  });
  await instance.loadEarlierHistory();
  await expect(instance.loadEarlierHistory()).rejects.toMatchObject({
    name: "HistoryPageUnavailableError", reason: "exhausted",
  });
  transport.emit({
    method: "peri/agent_event",
    params: {
      sessionId: "loaded",
      event_json: JSON.stringify({ type: "rewind_completed", value: { messages_json: "[]" } }),
    },
  });
  await expect(instance.loadEarlierHistory()).rejects.toMatchObject({
    name: "HistoryPageUnavailableError", reason: "invalidated",
  });
  await instance.close();
});

test("new session startup failure after session/new reports the created id and cleans up", async () => {
  const { transport, host, instance, facts } = setup();
  transport.failSnapshot = new Error("snapshot failed");
  const failure = await instance.bindNew({ path: "/workspace" }).catch((error) => error);
  expect(failure).toBeInstanceOf(PeriInstanceBindError);
  expect(failure.stage).toBe("input-snapshot");
  expect(failure.createdSessionId).toBe("session-new");
  expect(transport.closeRequests).toBe(1);
  expect(host.closed).toBe(1);
  expect(instance.needsReload).toBe(false);
  expect(facts).toHaveLength(0);
  await instance.close();
});

test("rename failure is reported with its stage and the created session is closed", async () => {
  const { transport, host, instance } = setup();
  transport.failRename = new Error("rename rejected");
  const failure = await instance.bindNew({ path: "/workspace", title: "Title" }).catch((error) => error);
  expect(failure).toMatchObject({
    name: "PeriInstanceBindError", stage: "rename", createdSessionId: "session-new",
  });
  expect(transport.closeRequests).toBe(1);
  expect(host.closed).toBe(1);
  expect(instance.needsReload).toBe(false);
  await instance.close();
});

test("unconfirmed cleanup keeps resources, blocks execution and can be retried", async () => {
  const { transport, host, instance, facts } = setup();
  transport.failSnapshot = new Error("snapshot failed");
  host.failFirstClose = true;
  const failure = await instance.bindNew({ path: "/workspace" }).catch((error) => error);
  expect(failure).toBeInstanceOf(PeriInstanceBindError);
  expect(failure.cause).toBeInstanceOf(AggregateError);
  expect(instance.needsReload).toBe(true);
  await expect(instance.send("input-1", "hello")).rejects.toBeInstanceOf(PeriInstanceNotBoundError);
  expect(facts).toContainEqual({ kind: "run-state", running: false, executionBlocked: true });

  await instance.close();
  expect(host.closed).toBe(2);
  expect(instance.needsReload).toBe(false);
  expect(facts).toContainEqual({ kind: "closed" });
});

test("host close failure retains the transport and retries without a second closed fact", async () => {
  const { transport, host, instance, facts } = setup();
  await instance.bindNew({ path: "/workspace" });
  host.confirmClose = false;
  const failure = await instance.close().catch((error) => error);
  expect(failure).toBeInstanceOf(PeriHostCleanupError);
  expect(instance.needsReload).toBe(true);
  expect(facts.some((fact) => fact.kind === "closed")).toBe(false);
  expect(transport.wireCloses).toBe(1);

  host.confirmClose = true;
  await instance.close();
  expect(host.closed).toBe(2);
  expect(instance.needsReload).toBe(false);
  expect(facts.filter((fact) => fact.kind === "closed")).toHaveLength(1);
});

test("session close failure blocks close, keeps the host for retry and then confirms", async () => {
  const { transport, host, instance, facts } = setup({ close: { drainTimeoutMs: 0 } });
  await instance.bindNew({ path: "/workspace" });
  transport.closeIncompleteTimes = 1;
  const failure = await instance.close().catch((error) => error);
  expect(failure).toBeInstanceOf(SessionCloseIncompleteError);
  expect(host.closed).toBe(0);
  expect(instance.needsReload).toBe(true);
  expect(facts).toContainEqual({ kind: "run-state", running: false, executionBlocked: true });

  await instance.close();
  expect(host.closed).toBe(1);
  expect(instance.needsReload).toBe(false);
  expect(facts).toContainEqual({ kind: "closed" });
});

test("binding an unknown session fails at the load stage", async () => {
  const { instance } = setup();
  const failure = await instance.bindExisting("missing").catch((error) => error);
  expect(failure).toMatchObject({ name: "PeriInstanceBindError", stage: "session/load", createdSessionId: null });
  await instance.close();
});

test("permission answers settle the ACP request; turn end aborts with a safe default", async () => {
  const { transport, instance, facts } = setup();
  await instance.bindNew({ path: "/workspace" });
  const handler = transport.reverseHandler!;
  const permission = handler("session/request_permission", {
    sessionId: "session-new",
    toolCall: { toolCallId: "tool-1", title: "Run command" },
    options: [
      { optionId: "allow_once", name: "Allow once" },
      { optionId: "reject_once", name: "Reject" },
    ],
  });
  const began = facts.find((fact) => fact.kind === "interaction-began");
  expect(began).toBeDefined();
  const id = (began as { record: { id: string } }).record.id;
  expect(instance.pendingInteractionCount).toBe(1);
  instance.answer(id, { kind: "permission", decision: "allow_once" });
  expect(await permission).toEqual({ outcome: { outcome: "selected", optionId: "allow_once" } });
  expect(facts).toContainEqual({
    kind: "interaction-settled", id, status: "resolved", outcome: "allow_once",
  });
  await expect(instance.answer(id, { kind: "permission", decision: "allow_once" }))
    .rejects.toBeInstanceOf(InteractionAnswerError);

  const pending = handler("session/request_permission", {
    sessionId: "session-new",
    toolCall: { toolCallId: "tool-2", title: "Write" },
    options: [{ optionId: "allow_once", name: "Allow once" }],
  });
  const second = facts.filter((fact) => fact.kind === "interaction-began").at(-1) as {
    record: { id: string };
  };
  await expect(instance.answer(second.record.id, { kind: "elicitation", decision: { action: "decline" } }))
    .rejects.toBeInstanceOf(InteractionAnswerError);
  transport.emit(agentEvent("user_input_run_started", { request_id: "request-1" }));
  transport.emit(agentDone("end_turn", "request-1"));
  expect(await pending).toEqual({ outcome: { outcome: "selected", optionId: "reject_once" } });
  expect(facts).toContainEqual({ kind: "interactions-expired" });
  await instance.close();
});

test("generic elicitation, unsupported forms and AskUserQuestion normalization", async () => {
  const { transport, instance, facts } = setup();
  await instance.bindNew({ path: "/workspace" });
  const handler = transport.reverseHandler!;

  const form = handler("elicitation/create", {
    sessionId: "session-new",
    message: "Name?",
    requestedSchema: {
      type: "object",
      properties: { name: { type: "string", title: "Name" } },
      required: ["name"],
    },
  });
  const formBegin = facts.filter((fact) => fact.kind === "interaction-began").at(-1) as {
    record: { id: string; fields: Array<{ key: string }> };
  };
  expect(formBegin.record.fields.map((field) => field.key)).toEqual(["name"]);
  instance.answer(formBegin.record.id, {
    kind: "elicitation", decision: { action: "accept", content: { name: "Peri" } },
  });
  expect(await form).toEqual({ action: "accept", content: { name: "Peri" } });

  expect(await handler("elicitation/create", {
    sessionId: "session-new",
    message: "Nested?",
    requestedSchema: { type: "object", properties: { nested: { type: "object" } } },
  })).toEqual({ action: "decline" });
  expect(facts.at(-1)).toEqual({
    kind: "interaction-settled", id: expect.any(String), status: "resolved", outcome: "decline",
  });

  const asked = handler("elicitation/create", {
    sessionId: "session-new",
    mode: "form",
    message: "Choose",
    requestedSchema: {
      type: "object",
      properties: {
        "ask_user_question_0": {
          type: "string",
          title: "Env",
          description: "Deploy where?",
          oneOf: [
            { const: "prod", title: "Production", description: "live" },
            { const: "dev", title: "Development" },
          ],
        },
        "ask_user_question_1": {
          type: "array",
          title: "Features",
          description: "Pick features",
          items: {
            type: "string",
            anyOf: [{ const: "a", title: "A" }, { const: "b", title: "B" }],
          },
        },
      },
    },
  });
  const askBegin = facts.filter((fact) => fact.kind === "interaction-began").at(-1) as {
    record: {
      id: string;
      variant: string;
      fields?: unknown;
      questions?: Array<{
        id: string; header: string; question: string; multiSelect: boolean;
        options: Array<{ value: string; label: string; description?: string }>;
      }>;
    };
  };
  expect(askBegin.record.variant).toBe("ask_user");
  // ask_user 只发结构化 questions；fields 仅供普通 elicitation。
  expect(askBegin.record.fields).toBeUndefined();
  expect(askBegin.record.questions).toEqual([
    {
      id: "ask_user_question_0", header: "Env", question: "Deploy where?", multiSelect: false,
      options: [
        { value: "prod", label: "Production", description: "live" },
        { value: "dev", label: "Development" },
      ],
    },
    {
      id: "ask_user_question_1", header: "Features", question: "Pick features", multiSelect: true,
      options: [{ value: "a", label: "A" }, { value: "b", label: "B" }],
    },
  ]);
  instance.answer(askBegin.record.id, {
    kind: "elicitation",
    decision: {
      action: "accept",
      content: { ask_user_question_0: "prod", ask_user_question_1: ["a", "b"] },
    },
  });
  expect(await asked).toEqual({
    action: "accept",
    content: { ask_user_question_0: "prod", ask_user_question_1: ["a", "b"] },
  });

  expect(await handler("elicitation/create", {
    sessionId: "session-new",
    mode: "form",
    requestedSchema: {
      type: "object",
      properties: { "ask_user_question_0": { type: "string", title: "Missing description" } },
    },
  })).toEqual({ action: "decline" });
  await instance.close();
});

test("task revision gaps request one snapshot and emit it once", async () => {
  const { transport, instance, facts } = setup();
  await instance.bindExisting("loaded");
  const unstable = (event: string, revision: number): JsonRpcNotification => ({
    method: "peri/unstable_event",
    params: { sessionId: "loaded", event, data: { task_id: "task-1", revision, status: "running" } },
  });
  transport.emit(unstable("bg-task-started", 1));
  transport.bgTasksResponse = { revision: 3, tasks: [{ task_id: "task-1", status: "running" }] };
  transport.emit(unstable("bg-task-updated", 3));
  transport.emit(unstable("bg-task-updated", 5));
  await Bun.sleep(0);
  expect(transport.bgTasksRequests).toBe(1);
  expect(facts.filter((fact) => fact.kind === "task-snapshot")).toHaveLength(1);
  await instance.close();
});

test("tool payloads are externalized by content-addressed refs and stay readable", async () => {
  const { transport, instance, facts } = setup();
  await instance.bindNew({ path: "/workspace" });
  const argumentsValue = { command: "x".repeat(5000) };
  transport.emit(sessionUpdate({
    sessionUpdate: "tool_call",
    toolCallId: "tool-1",
    title: "Shell",
    rawInput: argumentsValue,
    status: "completed",
    content: [{ type: "content", content: { type: "text", text: "r".repeat(5000) } }],
    rawOutput: { code: 0 },
  }));
  const notice = facts.filter((fact) => fact.kind === "notice").at(-1) as {
    notification: { params: { update: Record<string, unknown> } };
  };
  const update = notice.notification.params.update;
  const ref = update.argumentsRef as { id: string; bytes: number; preview: string };
  const readableRef = { id: ref.id, bytes: ref.bytes };
  expect(update.rawInput).toBeUndefined();
  expect(update.content).toBeUndefined();
  expect(update.arguments).toBeUndefined();
  expect(update.argumentsRef).toMatchObject({ bytes: expect.any(Number), preview: expect.any(String) });
  expect(update.resultRef).toMatchObject({ bytes: expect.any(Number) });
  expect((await instance.readPayload(readableRef)).json).toBe(JSON.stringify(argumentsValue));
  await expect(instance.readPayload({ id: readableRef.id, bytes: readableRef.bytes + 1 }))
    .rejects.toMatchObject({ name: "PayloadReadError", code: "unavailable" });

  transport.emit(sessionUpdate({
    sessionUpdate: "tool_call",
    toolCallId: "tool-2",
    title: "Small",
    rawInput: { ok: true },
    status: "pending",
  }));
  const small = facts.filter((fact) => fact.kind === "notice").at(-1) as {
    notification: { params: { update: Record<string, unknown> } };
  };
  expect(small.notification.params.update.arguments).toEqual({ ok: true });
  await instance.close();
});

test("payload budget rejection omits the field and marks the notice", async () => {
  const { transport, instance, facts } = setup({
    payloadBudgets: { inlineBytes: 16, externalBytes: 32, maxPayloadBytes: 32 },
  });
  await instance.bindNew({ path: "/workspace" });
  transport.emit(sessionUpdate({
    sessionUpdate: "tool_call",
    toolCallId: "tool-1",
    title: "Huge",
    rawInput: { text: "x".repeat(100) },
    status: "pending",
  }));
  const notice = facts.filter((fact) => fact.kind === "notice").at(-1) as {
    notification: { params: { update: Record<string, unknown> } };
  };
  expect(notice.notification.params.update.arguments).toBeUndefined();
  expect(notice.notification.params.update.argumentsRef).toBeUndefined();
  expect(notice.notification.params.update.error).toMatchObject({ code: "payload_too_large" });
  await instance.close();
});

test("run-state follows external user messages and maps stop reasons", async () => {
  const { transport, instance, facts } = setup();
  await instance.bindNew({ path: "/workspace" });
  transport.emit(sessionUpdate({
    sessionUpdate: "user_message_chunk",
    messageId: "message-1",
    content: { type: "text", text: "external" },
  }));
  expect(instance.running).toBe(true);
  // 同一 messageId 的后续 chunk 不重复开轮。
  transport.emit(sessionUpdate({
    sessionUpdate: "user_message_chunk",
    messageId: "message-1",
    content: { type: "text", text: " continued" },
  }));
  expect(facts.filter((fact) => fact.kind === "turn-cancel")).toHaveLength(0);
  transport.emit(agentDone("cancelled"));
  expect(facts).toContainEqual({ kind: "turn-complete", status: "cancelled", stopReason: "cancelled" });

  transport.emit(agentEvent("user_input_run_started", { request_id: "request-2" }));
  transport.emit(agentDone("max_tokens", "request-2"));
  expect(facts).toContainEqual({ kind: "turn-complete", status: "error", stopReason: "max_tokens" });
  await instance.close();
});

test("a late done for another request does not terminate the current run", async () => {
  const { transport, instance, facts } = setup();
  await instance.bindNew({ path: "/workspace" });
  transport.emit(agentEvent("user_input_run_started", { request_id: "request-1" }));
  transport.emit(agentEvent("user_input_run_started", { request_id: "request-2" }));
  transport.emit(agentDone("end_turn", "request-1"));
  expect(instance.running).toBe(true);
  expect(facts.some((fact) => fact.kind === "turn-complete")).toBe(false);
  transport.emit(agentDone("end_turn", "request-2"));
  expect(instance.running).toBe(false);
  await instance.close();
});

test("a detached sink stops receiving facts but the instance keeps running", async () => {
  const { transport, instance, facts, detach } = setup();
  await instance.bindNew({ path: "/workspace" });
  detach();
  const before = facts.length;
  transport.emit(agentEvent("user_input_run_started", { request_id: "request-1" }));
  expect(facts.length).toBe(before);
  expect(instance.running).toBe(true);
  transport.emit(agentDone("end_turn", "request-1"));
  await instance.close();
});

test("close is bounded while session/new hangs, preserves resources and can retry", async () => {
  const { transport, host, instance, facts } = setup({
    close: { drainTimeoutMs: 50, bindingSettleTimeoutMs: 50 },
  });
  const creation = deferred();
  transport.newGate = creation.promise;
  const binding = instance.bindNew({ path: "/workspace" }).catch((error) => error);
  await waitForCall(transport, "session/new");

  const startedAt = performance.now();
  const first = await instance.close().catch((error) => error);
  expect(first).toBeInstanceOf(PeriInstanceCloseIncompleteError);
  expect((first as Error).message).toContain("binding did not settle");
  expect(performance.now() - startedAt).toBeLessThan(1_000);
  expect(host.closed).toBe(0);
  expect(instance.needsReload).toBe(true);
  expect(facts).toContainEqual({
    kind: "run-state", running: false, executionBlocked: true,
  });
  expect(facts.some((fact) => fact.kind === "closed")).toBe(false);

  // 第二次 close 重试同一个未结算绑定，仍不伪装已停止。
  const second = await instance.close().catch((error) => error);
  expect(second).toBeInstanceOf(PeriInstanceCloseIncompleteError);
  expect(host.closed).toBe(0);

  // 迟到的 session/new 结算保留创建身份，正常清理已创建会话与宿主。
  creation.resolve();
  const failure = await binding;
  expect(failure).toBeInstanceOf(PeriInstanceBindError);
  expect(failure).toMatchObject({ stage: "claim", createdSessionId: "session-new" });
  expect(transport.calls.some((call) => call.method === "session/close")).toBe(true);
  expect(host.closed).toBe(1);

  await instance.close();
  expect(facts.filter((fact) => fact.kind === "closed")).toHaveLength(1);
});

test("aborting a hung host activation closes the late activation with evidence", async () => {
  const transport = new FakeTransport();
  const host = new FakeHost(transport);
  const activation = deferred();
  const instance = new PeriInstance({
    acquireHost: async () => { await activation.promise; return host; },
    resolveSessionPath: async () => "/persisted/session",
    close: { drainTimeoutMs: 50, bindingSettleTimeoutMs: 50 },
  });
  const errors: unknown[][] = [];
  const originalError = console.error;
  console.error = (...args: unknown[]) => { errors.push(args); };
  try {
    const binding = instance.bindNew({ path: "/workspace" }).catch((error) => error);
    await Bun.sleep(0);
    await instance.close();
    activation.resolve();
    await binding;
    await Bun.sleep(5);
    expect(host.closed).toBe(1);
    expect(errors.some((args) => String(args[0]).includes("Late host activation"))).toBe(true);
  } finally {
    console.error = originalError;
  }
});

test("rewind tool arguments become payload refs before the notice is emitted", async () => {
  const { transport, instance, facts } = setup();
  await instance.bindNew({ path: "/workspace" });
  const big = "x".repeat(12_000);
  const messages = [
    { id: "u", role: "user", content: "go" },
    {
      id: "a",
      role: "assistant",
      content: [{ type: "tool_use", id: "call-1", name: "Bash", input: { command: big } }],
      tool_calls: [{ id: "call-2", name: "Read", arguments: { path: big } }],
    },
    { id: "t", role: "tool", tool_call_id: "call-1", content: "ok" },
  ];
  transport.emit(agentEvent("rewind_completed", { messages_json: JSON.stringify(messages) }));

  const notice = facts.filter((fact) => fact.kind === "notice").at(-1) as {
    notification: { params: { event_json: string } };
  };
  const event = JSON.parse(notice.notification.params.event_json) as {
    value: { messages_json: string };
  };
  const decoded = JSON.parse(event.value.messages_json) as Array<Record<string, unknown>>;
  const block = (decoded[1]!.content as Array<Record<string, unknown>>)[0]!;
  const call = (decoded[1]!.tool_calls as Array<Record<string, unknown>>)[0]!;
  const blockRef = block.argumentsRef as { id: string; bytes: number; preview: string };
  const callRef = call.argumentsRef as { id: string; bytes: number; preview: string };
  expect(block.input).toBeUndefined();
  expect(call.arguments).toBeUndefined();
  expect(typeof blockRef.bytes).toBe("number");
  expect(typeof blockRef.preview).toBe("string");
  expect(blockRef.bytes).toBeGreaterThan(12_000);
  expect(typeof callRef.bytes).toBe("number");
  expect(event.value.messages_json).not.toContain("x".repeat(1_000));
  expect(event.value.messages_json.length).toBeLessThan(3_000);

  expect(JSON.parse((await instance.readPayload(blockRef)).json)).toEqual({ command: big });
  await instance.close();
});

test("rewind tool arguments over budget keep the fact and expose payload_too_large", async () => {
  const { transport, instance, facts } = setup({
    payloadBudgets: { inlineBytes: 0, externalBytes: 64, maxPayloadBytes: 64 },
  });
  await instance.bindNew({ path: "/workspace" });
  const messages = [
    { id: "u", role: "user", content: "go" },
    {
      id: "a",
      role: "assistant",
      content: [{ type: "tool_use", id: "call-1", name: "Bash", input: { text: "z".repeat(200) } }],
    },
  ];
  transport.emit(agentEvent("rewind_completed", { messages_json: JSON.stringify(messages) }));
  const notice = facts.filter((fact) => fact.kind === "notice").at(-1) as {
    notification: { params: { event_json: string } };
  };
  const event = JSON.parse(notice.notification.params.event_json) as {
    value: { messages_json: string };
  };
  const decoded = JSON.parse(event.value.messages_json) as Array<Record<string, unknown>>;
  const block = (decoded[1]!.content as Array<Record<string, unknown>>)[0]!;
  expect(block.input).toBeUndefined();
  expect(block.argumentsRef).toBeUndefined();
  expect(block.argumentsError).toBe("payload_too_large");
  expect(event.value.messages_json).not.toContain("z".repeat(50));
  await instance.close();
});

test("readPayload rejects after close as unavailable", async () => {
  const { instance } = setup();
  await instance.bindNew({ path: "/workspace" });
  await instance.close();
  await expect(instance.readPayload({ id: "payload:missing", bytes: 1 }))
    .rejects.toMatchObject({ name: "PayloadReadError", code: "unavailable" });
});
