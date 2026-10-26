import { expect, test } from "bun:test";
import { Session } from "../src/agent/session";
import type { JsonRpcNotification, Transport } from "../src/transport/types";

function gate() {
  let resolve!: () => void;
  const promise = new Promise<void>((complete) => { resolve = complete; });
  return { promise, resolve };
}

function replayEvent(text: string, generation: string | undefined): JsonRpcNotification {
  return {
    method: "session/update",
    params: {
      sessionId: "existing",
      update: {
        sessionUpdate: "agent_message_chunk",
        content: { type: "text", text },
        _meta: { periReplay: true, ...(generation ? { periLoadGeneration: generation } : {}) },
      },
    },
  };
}

function liveEvent(text: string): JsonRpcNotification {
  return {
    method: "session/update",
    params: {
      sessionId: "existing",
      update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text } },
    },
  };
}

class FakeTransport implements Transport {
  readonly calls: Array<{ method: string; params?: unknown }> = [];
  readonly listeners = new Set<(event: JsonRpcNotification) => void>();
  readonly startupError = new Error("input queue startup failed");
  failStartup = false;
  failInitialize = false;
  startupGate?: Promise<void>;
  loadResponse: unknown = {};
  loadEvents: JsonRpcNotification[] = [];
  liveDuringLoad: JsonRpcNotification[] = [];
  replayTexts: string[] = [];
  liveTexts: string[] = [];
  failSnapshot = false;
  newGate?: Promise<void>;
  loadGate?: Promise<void>;
  renameGate?: Promise<void>;
  reverseHandler?: (method: string, params: unknown) => unknown;
  readonly notifications: JsonRpcNotification[] = [];

  async request<Response>(method: string, params?: unknown): Promise<Response> {
    this.calls.push({ method, params });
    if (method === "initialize") {
      await this.startupGate;
      if (this.failInitialize) throw this.startupError;
      return { protocolVersion: 1 } as Response;
    }
    if (method === "session/input/snapshot") {
      if (this.failStartup || this.failSnapshot) throw this.startupError;
      return { generation: "generation-1" } as Response;
    }
    if (method === "session/new") {
      await this.newGate;
      return { sessionId: "existing" } as Response;
    }
    if (method === "session/load") {
      await this.loadGate;
      const generation = (params as { loadGeneration?: string } | undefined)?.loadGeneration;
      for (const text of this.replayTexts) this.emit(replayEvent(text, generation));
      for (const text of this.liveTexts) this.emit(liveEvent(text));
      for (const event of this.loadEvents) this.emit(event);
      for (const event of this.liveDuringLoad) this.emit(event);
      return this.loadResponse as Response;
    }
    if (method === "session/rename") {
      await this.renameGate;
      return {} as Response;
    }
    if (method === "session/close") return {} as Response;
    return {} as Response;
  }

  async sendRequest<Response>(method: string, params?: unknown): Promise<{ response: Promise<Response> }> {
    return { response: this.request<Response>(method, params) };
  }

  async notify(): Promise<void> {}
  async *events(): AsyncIterable<JsonRpcNotification> {}
  setRequestHandler(handler: (method: string, params: unknown) => unknown): void {
    this.reverseHandler = handler;
  }

  emit(event: JsonRpcNotification): void {
    for (const listener of this.listeners) listener(event);
  }

  subscribe(listener: (event: JsonRpcNotification) => void): () => void {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  }

  async close(): Promise<void> {}
}

function sessionFor(transport: FakeTransport, onNotification?: (event: JsonRpcNotification) => void) {
  return new Session(transport, {
    requestHandler: async () => ({ ok: true }),
    onNotification: onNotification ?? ((event) => { transport.notifications.push(event); }),
  });
}

test("startup failure preserves the original cause, removes the listener and can be cleaned up", async () => {
  const transport = new FakeTransport();
  transport.failInitialize = true;
  const session = sessionFor(transport);
  const failure = await session.start({ path: "/workspace", sessionId: null }).catch((error) => error);
  expect(failure).toMatchObject({ name: "SessionStartError", stage: "initialize" });
  expect(failure.cause).toBe(transport.startupError);
  expect(transport.listeners.size).toBe(0);
  await session.cleanupStartup();
  expect(session.isClosed).toBe(true);

  transport.failInitialize = false;
  const replacement = sessionFor(transport);
  await replacement.start({ path: "/workspace", sessionId: null });
  await replacement.close();
});

test("startup cleanup cannot race an active startup", async () => {
  const transport = new FakeTransport();
  const starting = gate();
  transport.startupGate = starting.promise;
  const session = sessionFor(transport);
  const started = session.start({ path: "/workspace", sessionId: null });
  expect(() => session.cleanupStartup()).toThrow("Wait for Session startup");
  starting.resolve();
  await started;
  expect(() => session.cleanupStartup()).toThrow("established Session");
  await session.close();
  expect(transport.listeners.size).toBe(0);
});

test("load uses the persisted path, forwards declarations and carries the load generation", async () => {
  const transport = new FakeTransport();
  transport.loadResponse = { _meta: { "peri.sessionWorkspaceV1": { read_only: true } } };
  const session = sessionFor(transport);
  await session.start({
    path: "/persisted/workspace",
    sessionId: "existing",
    mcpServers: [{ name: "workspace", type: "http", url: "https://example.test/mcp", headers: [] }],
  });
  expect(session.path).toBe("/persisted/workspace");
  expect(transport.calls.find((call) => call.method === "session/load")?.params).toEqual({
    cwd: "/persisted/workspace",
    mcpServers: [{ name: "workspace", type: "http", url: "https://example.test/mcp", headers: [] }],
    sessionId: "existing",
    loadGeneration: expect.any(String),
  });
  await session.close();
});

test("a load response from a superseded generation is rejected", async () => {
  const transport = new FakeTransport();
  transport.loadResponse = { _meta: { periLoadGeneration: "someone-else" } };
  const session = sessionFor(transport);
  await expect(session.start({ path: "/workspace", sessionId: "existing" }))
    .rejects.toMatchObject({ name: "SessionStartError", stage: "session/load" });
  expect(transport.listeners.size).toBe(0);
});

test("history replay is staged until the load response and duplicate replay is dropped", async () => {
  const transport = new FakeTransport();
  const delivered: JsonRpcNotification[] = [];
  const session = sessionFor(transport, (event) => delivered.push(event));
  transport.loadResponse = { sessionId: "existing" };
  transport.replayTexts = ["old"];
  transport.liveTexts = ["live"];
  const started = await session.start({ path: "/workspace", sessionId: "existing" });
  expect(started.historyBatch.map((event) =>
    (event.params as { update: { content: { text: string } } }).update.content.text))
    .toEqual(["old"]);
  expect(delivered).toHaveLength(0);
  session.beginLive();
  expect(delivered.map((event) =>
    (event.params as { update: { content: { text: string } } }).update.content.text))
    .toEqual(["live"]);
  const request = transport.calls.find((call) => call.method === "session/load");
  const generation = (request?.params as { loadGeneration?: string }).loadGeneration;
  transport.emit(replayEvent("duplicate", generation));
  expect(delivered).toHaveLength(1);
  await session.close();
});

test("legacy replay without a load generation reports replayTurnPending", async () => {
  const transport = new FakeTransport();
  const session = sessionFor(transport);
  transport.loadEvents = [{
    method: "session/update",
    params: {
      sessionId: "existing",
      update: {
        sessionUpdate: "agent_message_chunk",
        content: { type: "text", text: "legacy" },
        _meta: { periReplay: true },
      },
    },
  }];
  const started = await session.start({ path: "/workspace", sessionId: "existing" });
  expect(started.replayTurnPending).toBe(true);
  expect(started.historyBatch).toHaveLength(0);
  await session.close();
});

test("reverse requests are routed through the injected handler", async () => {
  const transport = new FakeTransport();
  const seen: Array<{ method: string; params: unknown }> = [];
  const session = new Session(transport, {
    requestHandler: (method, params) => {
      seen.push({ method, params });
      return { outcome: { outcome: "selected", optionId: "reject_once" } };
    },
  });
  await session.start({ path: "/workspace", sessionId: null });
  expect(await transport.reverseHandler?.("session/request_permission", { sessionId: "existing" }))
    .toEqual({ outcome: { outcome: "selected", optionId: "reject_once" } });
  expect(seen).toEqual([{ method: "session/request_permission", params: { sessionId: "existing" } }]);
  await session.close();
});

test("delivery resolves waiters, reports text and rejects on close", async () => {
  const transport = new FakeTransport();
  const delivered: Array<{ inputId: string; text: string }> = [];
  const session = new Session(transport, {
    requestHandler: async () => ({}),
    onDelivered: (inputId, text) => delivered.push({ inputId, text }),
  });
  await session.start({ path: "/workspace", sessionId: null });
  session.beginLive();
  session.trackInput("input-1", "hello");
  const waiting = session.waitForDelivery("input-1");
  transport.emit({
    method: "peri/agent_event",
    params: {
      sessionId: "existing",
      event_json: JSON.stringify({
        type: "user_input_delivered",
        value: { input_id: "input-1", generation: "generation-1" },
      }),
    },
  });
  await waiting;
  expect(delivered).toEqual([{ inputId: "input-1", text: "hello" }]);

  session.trackInput("input-2", "pending");
  const rejected = session.waitForDelivery("input-2");
  await session.close();
  await expect(rejected).rejects.toThrow("closed");
});

test("aborting initialize stops the wait and leaves the transport to its owner", async () => {
  const transport = new FakeTransport();
  const startup = gate();
  transport.startupGate = startup.promise;
  const session = sessionFor(transport);
  const controller = new AbortController();
  const starting = session.start({ path: "/workspace", sessionId: null }, { signal: controller.signal });
  await Bun.sleep(0);
  controller.abort(new Error("user cancelled"));
  const failure = await starting.catch((error) => error);
  expect(failure).toMatchObject({
    name: "SessionStartError", stage: "initialize", createdSessionId: null,
  });
  expect(failure.cause).toMatchObject({ name: "AbortRaceError", cause: expect.any(Error) });
  expect(transport.listeners.size).toBe(0);
  await session.cleanupStartup();
  expect(session.isClosed).toBe(true);
});

test("aborting session/load reports the load stage and keeps cleanup retryable", async () => {
  const transport = new FakeTransport();
  const loading = gate();
  transport.loadGate = loading.promise;
  const session = sessionFor(transport);
  const controller = new AbortController();
  const starting = session.start({ path: "/workspace", sessionId: "existing" }, {
    signal: controller.signal,
  });
  await Bun.sleep(0);
  controller.abort();
  await expect(starting).rejects.toMatchObject({
    name: "SessionStartError", stage: "session/load", createdSessionId: null,
  });
  expect(transport.listeners.size).toBe(0);
  await session.cleanupStartup();
});

test("aborting mcp-connect reports that stage", async () => {
  const transport = new FakeTransport();
  const connecting = gate();
  const session = new Session(transport, {
    requestHandler: async () => ({}),
    onBeforeSession: async () => { await connecting.promise; },
  });
  const controller = new AbortController();
  const starting = session.start({ path: "/workspace", sessionId: null }, {
    signal: controller.signal,
  });
  await Bun.sleep(0);
  controller.abort();
  await expect(starting).rejects.toMatchObject({
    name: "SessionStartError", stage: "mcp-connect", createdSessionId: null,
  });
  await session.cleanupStartup();
});

test("session/new is not raced: abort preserves the created id and cleanup closes it", async () => {
  const transport = new FakeTransport();
  const creation = gate();
  transport.newGate = creation.promise;
  const session = sessionFor(transport);
  const controller = new AbortController();
  let settled = false;
  const starting = session.start({ path: "/workspace", sessionId: null }, {
    signal: controller.signal,
  });
  void starting.finally(() => { settled = true; }).catch(() => {});
  await Bun.sleep(0);
  controller.abort();
  await Bun.sleep(5);
  expect(settled).toBe(false);
  creation.resolve();
  await expect(starting).rejects.toMatchObject({
    name: "SessionStartError", stage: "claim", createdSessionId: "existing",
  });
  expect(transport.calls.some((call) => call.method === "session/input/snapshot")).toBe(false);
  await session.close({ drainTimeoutMs: 20 });
  expect(transport.calls.some((call) => call.method === "session/close")).toBe(true);
  expect(session.isClosed).toBe(true);
});

test("onSessionCreated runs after session/new and before input snapshot; claim failure keeps the id", async () => {
  const transport = new FakeTransport();
  const session = sessionFor(transport);
  let snapshotCalls = -1;
  const started = await session.start({ path: "/workspace", sessionId: null }, {
    onSessionCreated: (sessionId) => {
      expect(sessionId).toBe("existing");
      snapshotCalls = transport.calls.filter((call) => call.method === "session/input/snapshot").length;
    },
  });
  expect(snapshotCalls).toBe(0);
  expect(started.sessionId).toBe("existing");
  await session.close();

  const failingTransport = new FakeTransport();
  const failing = sessionFor(failingTransport);
  await expect(failing.start({ path: "/workspace", sessionId: null }, {
    onSessionCreated: () => { throw new Error("claim lost"); },
  })).rejects.toMatchObject({
    name: "SessionStartError", stage: "claim", createdSessionId: "existing",
  });
  await failing.close({ drainTimeoutMs: 20 });
});

test("rename accepts a cancellation signal without closing the session", async () => {
  const transport = new FakeTransport();
  const renaming = gate();
  transport.renameGate = renaming.promise;
  const session = sessionFor(transport);
  await session.start({ path: "/workspace", sessionId: null });
  const controller = new AbortController();
  const pending = session.rename("Title", { signal: controller.signal }).catch((error) => error);
  await Bun.sleep(0);
  controller.abort();
  const failure = await pending;
  expect(failure).toMatchObject({ name: "AbortRaceError" });
  expect(session.isActive).toBe(true);
  renaming.resolve();
  await session.close();
  expect(session.isClosed).toBe(true);
});
