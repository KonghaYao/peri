import { describe, expect, test } from "bun:test";
import type { ControlCommand, ControlReceipt, ControlSnapshot, JsonRpcNotification } from "../worker/sdk";
import { ChatSessionCore, MAX_REPLY_BYTES, MAX_TRANSCRIPT_BYTES } from "../worker/chat/session";
import { fetchApi } from "../worker/api/router";
import type { AcpTransport, Chat, ChatRepository, Env, PausedSession, SessionRecord, SessionState } from "../worker/types";

interface TransportPlan {
  blocked?: boolean;
  failure?: unknown;
  closeFailure?: Error;
  chunks?: string[];
  closeGate?: Promise<void>;
  startupGate?: Promise<void>;
  resumeFailure?: Error;
  stopReceiptGate?: Promise<void>;
}

function barrier() {
  let release!: () => void;
  const promise = new Promise<void>((resolve) => { release = resolve; });
  return { promise, release };
}

const fixtureControl: ControlSnapshot = {
  state: { lifecycle: 2, revision: 5, controlGeneration: 3, status: "active",
    attempt: { turnId: "fixture-turn", attemptId: "fixture-attempt" } },
  settlement: { status: "pending" },
};

function fakeDispatcher(transport: AcpTransport) {
  return {
    handle(_method: string, _params: unknown): Promise<unknown> | undefined { return undefined; },
    seal(): void {},
    async stop(sessionId: string): Promise<PausedSession> {
      const { state } = await transport.request<ControlSnapshot>("session/control/state", { sessionId });
      if (!state.attempt) throw new Error("No exact fixture target");
      const receipt = await transport.request<ControlReceipt>("session/control", {
        sessionId, commandId: "fixture-stop-command", expectedLifecycle: state.lifecycle,
        expectedRevision: state.revision, expectedControlGeneration: state.controlGeneration,
        action: { kind: "stop", target: state.attempt },
      });
      if (receipt.decision.kind !== "accepted") throw new Error("Fixture stop rejected");
      return { lifecycle: receipt.state.lifecycle, controlGeneration: receipt.state.controlGeneration,
        resumeCommandId: `resume:${receipt.commandId}` };
    },
    async resume(sessionId: string, marker: PausedSession): Promise<void> {
      const { state } = await transport.request<ControlSnapshot>("session/control/state", { sessionId });
      if (state.status !== "paused" || state.lifecycle !== marker.lifecycle || state.controlGeneration !== marker.controlGeneration)
        throw new Error("Fixture pause authority changed");
      const receipt = await transport.request<ControlReceipt>("session/control", {
        sessionId, commandId: marker.resumeCommandId, expectedLifecycle: marker.lifecycle,
        expectedRevision: state.revision, expectedControlGeneration: marker.controlGeneration,
        action: { kind: "resume" },
      });
      if (receipt.decision.kind !== "accepted") throw new Error("Fixture resume rejected");
    },
    async stopAfterHostClose(_sessionId: string): Promise<void> {},
  };
}

class FixtureTransport implements AcpTransport {
  readonly calls: { method: string; params?: unknown }[] = [];
  readonly promptStarted: Promise<void>;
  readonly closeStarted: Promise<void>;
  readonly stopStarted: Promise<void>;
  closeRequested = false;
  closed = false;
  unsubscribed = false;
  handler?: (method: string, params: unknown) => unknown | Promise<unknown>;
  private listeners = new Set<(notification: JsonRpcNotification) => void>();
  private markPromptStarted!: () => void;
  private markCloseStarted!: () => void;
  private markStopStarted!: () => void;
  private finishPrompt!: (result: { stopReason: string }) => void;
  private sessionId = "";

  constructor(private readonly plan: TransportPlan = {}, readonly controlSnapshot = structuredClone(fixtureControl)) {
    this.promptStarted = new Promise((resolve) => { this.markPromptStarted = resolve; });
    this.closeStarted = new Promise((resolve) => { this.markCloseStarted = resolve; });
    this.stopStarted = new Promise((resolve) => { this.markStopStarted = resolve; });
  }

  async request<Result>(method: string, params?: unknown): Promise<Result> {
    this.calls.push({ method, params });
    if (method === "initialize") return { protocolVersion: 1 } as Result;
    if (method === "session/control/state") return structuredClone(this.controlSnapshot) as Result;
    if (method === "session/control") {
      const command = params as ControlCommand;
      const state = this.controlSnapshot.state;
      expect(command.expectedLifecycle).toBe(state.lifecycle);
      expect(command.expectedRevision).toBe(state.revision);
      expect(command.expectedControlGeneration).toBe(state.controlGeneration);
      if (command.action.kind === "stop") {
        expect(command.action.target).toEqual(state.attempt!);
        state.status = "paused";
        state.attempt = null;
        this.finishPrompt?.({ stopReason: "cancelled" });
      } else if (command.action.kind === "resume") {
        if (this.plan.resumeFailure) throw this.plan.resumeFailure;
        state.status = "active";
      } else throw new Error("Unexpected fixture control action");
      state.revision++;
      state.controlGeneration++;
      if (command.action.kind === "stop") {
        this.markStopStarted();
        await this.plan.stopReceiptGate;
      }
      return { sessionId: command.sessionId, commandId: command.commandId,
        decision: { kind: "accepted" }, state: structuredClone(state) } as Result;
    }
    if (method === "session/load") {
      this.sessionId = (params as { sessionId: string }).sessionId;
      this.emit("historical replay");
      return { modes: {} } as Result;
    }
    throw new Error(`Unexpected ACP request: ${method}`);
  }

  async sendRequest<Result>(method: string, params?: unknown): Promise<{ response: Promise<Result> }> {
    this.calls.push({ method, params });
    if (method !== "session/prompt") throw new Error(`Unexpected ACP send: ${method}`);
    if (this.controlSnapshot.state.status !== "active") throw new Error("Fixture activation remains paused");
    this.controlSnapshot.state.attempt = { turnId: "fixture-turn", attemptId: "fixture-attempt" };
    const response = new Promise<{ stopReason: string }>((resolve, reject) => {
      this.finishPrompt = resolve;
      this.emit("foreign session", { sessionId: "another-session" });
      this.emit("old subagent chunk", { _meta: { "peri.sourceAgentId": "child" } });
      this.emit("subagent chunk", { _meta: { peri: { sourceAgentId: "child" } } });
      for (const chunk of this.plan.chunks ?? ["你好", "🙂"]) this.emit(chunk);
      if (this.plan.failure) reject(this.plan.failure);
      else if (!this.plan.blocked) resolve({ stopReason: "end_turn" });
    });
    this.markPromptStarted();
    return { response: response as Promise<Result> };
  }

  async notify(method: string, params?: unknown): Promise<void> {
    this.calls.push({ method, params });
    throw new Error(`Unexpected ACP notification: ${method}`);
  }

  async *events(): AsyncIterable<JsonRpcNotification> {}

  subscribe(listener: (notification: JsonRpcNotification) => void): () => void {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); this.unsubscribed = true; };
  }

  setRequestHandler(handler: (method: string, params: unknown) => unknown | Promise<unknown>): void {
    this.handler = handler;
  }

  async close(): Promise<void> {
    this.closeRequested = true;
    this.markCloseStarted();
    await this.plan.closeGate;
    if (this.plan.closeFailure) throw this.plan.closeFailure;
    this.closed = true;
  }

  private emit(text: string, extra: Record<string, unknown> = {}) {
    for (const listener of this.listeners) {
      listener({ jsonrpc: "2.0", method: "session/update", params: {
        sessionId: this.sessionId,
        update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text } },
        ...extra,
      } });
    }
  }
}

async function fixture(plan: TransportPlan = {}) {
  const chats = new Map<string, Chat>();
  const repositoryCalls: string[] = [];
  const repository: ChatRepository = {
    async list() { repositoryCalls.push("list"); return structuredClone([...chats.values()]); },
    async get(id) { repositoryCalls.push("get"); return structuredClone(chats.get(id) ?? null); },
    async create(title) {
      repositoryCalls.push("create");
      const chat = { id: `00000000-0000-4000-8000-${String(chats.size + 1).padStart(12, "0")}`,
        title, updatedAt: "2026-10-06T00:00:00.000Z" };
      chats.set(chat.id, structuredClone(chat));
      return structuredClone(chat);
    },
  };
  const records = new Map<string, SessionRecord>();
  const cores = new Map<string, ChatSessionCore>();
  const work: Promise<unknown>[] = [];
  const transports = [new FixtureTransport(plan)];
  const domainStates = new Map<string, ControlSnapshot>();
  let starts = 0;
  const startup = barrier();
  let objectLookups = 0;
  const env: Env = {
    APP_AUTH_TOKEN: "trusted-app-token",
    PERI_STORAGE_URL: "libsql://fixture.invalid",
    PERI_STORAGE_TOKEN: "private-storage-token",
    MODEL_BASE_URL: "https://model.invalid/v1",
    MODEL_API_KEY: "private-model-token",
    MODEL_ID: "fixture-model",
    CHAT_SESSIONS: {
      idFromName(name) { return name; },
      get(identity) {
        objectLookups++;
        const id = String(identity);
        let core = cores.get(id);
        if (!core) {
          const state: SessionState = {
            storage: {
              async get<Value>() { return structuredClone(records.get(id)) as Value | undefined; },
              async put<Value>(_key: string, value: Value) { records.set(id, structuredClone(value) as SessionRecord); },
            },
            waitUntil(promise) { work.push(promise); },
          };
          core = new ChatSessionCore(state, env, async () => {
            startup.release();
            await plan.startupGate;
            const transport = transports[starts] ?? new FixtureTransport(plan, domainStates.get(id));
            domainStates.set(id, transport.controlSnapshot);
            transports[starts++] = transport;
            return transport;
          }, fakeDispatcher, repository);
          cores.set(id, core);
        }
        return { fetch: (request: Request) => core!.fetch(request) };
      },
    },
  };
  const request = (path: string, method = "GET", body?: unknown, token: string | null = env.APP_AUTH_TOKEN!) => {
    const headers = new Headers();
    if (token !== null) headers.set("Authorization", `Bearer ${token}`);
    if (body !== undefined) headers.set("Content-Type", "application/json");
    return fetchApi(new Request(`https://app.invalid${path}`, {
      method, headers, body: body === undefined ? undefined : JSON.stringify(body),
    }), env, repository);
  };
  const create = async (title = "Fixture chat"): Promise<Chat> => {
    const response = await request("/api/chats", "POST", { title });
    expect(response.status).toBe(201);
    return await response.json() as Chat;
  };
  return { env, chats, repository, repositoryCalls, records, cores, transports, request, create,
    startupStarted: startup.promise,
    objectLookups: () => objectLookups, drain: () => Promise.all(work) };
}

describe("authenticated API routing with a thread repository and chat DO cores", () => {
  test("message command acknowledges with JSON 202 before the job settles", async () => {
    const app = await fixture({ blocked: true });
    const chat = await app.create();
    const response = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "HTTP prompt" });
    expect(response.status).toBe(202);
    expect(response.headers.get("Content-Type")).toContain("application/json");
    expect(await response.json() as { accepted: boolean }).toEqual({ accepted: true });
    await app.transports[0].promptStarted;
    expect(app.records.get(chat.id)!.running).toBe(true);
    await app.request(`/api/chats/${chat.id}/cancel`, "POST");
    await app.drain();
    expect((await (await app.request(`/api/chats/${chat.id}`)).json() as SessionRecord).messages
      .map(({ content }) => content)).toEqual(["HTTP prompt", "你好🙂"]);
  });

  test.each([null, "wrong-token", "trusted-app-token-extra"])("rejects invalid Bearer identity %s before touching storage", async (token) => {
    const app = await fixture();
    const response = await app.request("/api/chats", "POST", {}, token);
    expect(response.status).toBe(401);
    expect(response.headers.get("WWW-Authenticate")).toBe("Bearer");
    expect(app.objectLookups()).toBe(0);
    expect(app.chats.size).toBe(0);
    expect(app.repositoryCalls).toEqual([]);
  });

  test("fails closed when the app token is not configured", async () => {
    const app = await fixture();
    delete app.env.APP_AUTH_TOKEN;
    const response = await app.request("/api/chats", "GET", undefined, "any-token");
    expect(response.status).toBe(503);
    expect(app.objectLookups()).toBe(0);
  });

  test("creates and lists actual session identities without touching any chat DO, then hydrates detail", async () => {
    const app = await fixture();
    const chat = await app.create("  中文标题  ");
    expect(chat.title).toBe("中文标题");
    expect(chat.id).toMatch(/^[0-9a-f-]{36}$/);
    expect((await app.request("/api/chats")).headers.get("Cache-Control")).toBe("no-store");
    expect(await (await app.request("/api/chats")).json() as { chats: Chat[] }).toEqual({ chats: [chat] });
    expect(app.objectLookups()).toBe(0);
    expect(app.records.size).toBe(0);
    expect(await (await app.request(`/api/chats/${chat.id}`)).json() as SessionRecord).toEqual({ chat, messages: [] });
    expect(app.transports[0].calls).toEqual([]);
  });

  test("authoritative thread metadata and chat history survive core reconstruction", async () => {
    const app = await fixture();
    const chat = await app.create();
    await (await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Persist me" })).text();
    await app.drain();
    const record = structuredClone(app.records.get(chat.id)!);
    app.cores.clear();
    expect(await (await app.request("/api/chats")).json() as { chats: Chat[] }).toEqual({ chats: [chat] });
    expect(await (await app.request(`/api/chats/${chat.id}`)).json() as SessionRecord).toEqual({ chat, messages: record.messages });
    expect(app.transports).toHaveLength(1);
  });

  test("repository creation failure does not create or hydrate a chat DO", async () => {
    const app = await fixture();
    app.repository.create = async () => { throw new Error("private-storage-token"); };
    const response = await app.request("/api/chats", "POST", {});
    expect(response.status).toBe(500);
    expect(await response.text()).not.toContain("private-storage-token");
    expect(app.chats.size).toBe(0);
    expect(app.records.size).toBe(0);
    expect(app.objectLookups()).toBe(0);
  });

  test.each(["", " ", 123, "long".repeat(51)])("rejects invalid create title %s without creating a thread", async (title) => {
    const app = await fixture();
    expect((await app.request("/api/chats", "POST", { title })).status).toBe(400);
    expect(app.chats.size).toBe(0);
  });

  test.each([
    ["/api/chats/not-a-uuid", "GET", 404],
    ["/api/chats/00000000-0000-4000-8000-000000000001", "GET", 404],
    ["/api/chats", "DELETE", 405],
    ["/api/chats/00000000-0000-4000-8000-000000000001/messages", "GET", 405],
  ] as const)("returns %s %s route error %s", async (path, method, status) => {
    const app = await fixture();
    expect((await app.request(path, method)).status).toBe(status);
  });

  test("requires JSON content type and rejects malformed or oversized JSON", async () => {
    const app = await fixture();
    for (const [body, contentType, status] of [
      ["{}", "text/plain", 415], ["{", "application/json", 400],
      ["[]", "application/json", 400], [JSON.stringify({ title: "x".repeat(65_536) }), "application/json", 413],
    ] as const) {
      const response = await fetchApi(new Request("https://app.invalid/api/chats", {
        method: "POST", body, headers: { Authorization: "Bearer trusted-app-token", "Content-Type": contentType },
      }), app.env, app.repository);
      expect(response.status).toBe(status);
    }
    expect(app.objectLookups()).toBe(0);
  });

  test("DO core independently rejects unauthenticated access", async () => {
    const app = await fixture();
    const chat = await app.create();
    const response = await app.env.CHAT_SESSIONS.get(chat.id).fetch(new Request(`https://app.invalid/api/chats/${chat.id}`));
    expect(response.status).toBe(401);
    expect(app.repositoryCalls).toEqual(["create"]);
  });

  test("list query failure is visible as an error without touching a chat DO", async () => {
    const app = await fixture();
    app.repository.list = async () => { throw new Error("private-storage-token"); };
    const response = await app.request("/api/chats");
    expect(response.status).toBe(500);
    expect(await response.text()).not.toContain("private-storage-token");
    expect(app.objectLookups()).toBe(0);
  });

  test("GET detail refreshes authoritative metadata rather than returning cached DO metadata", async () => {
    const app = await fixture();
    const chat = await app.create();
    await app.request(`/api/chats/${chat.id}`);
    const updated = { ...chat, title: "Renamed in Rust Store", updatedAt: "2026-10-07T00:00:00.000Z" };
    app.chats.set(chat.id, updated);
    expect(await (await app.request(`/api/chats/${chat.id}`)).json() as SessionRecord).toEqual({ chat: updated, messages: [] });
    expect(app.transports[0].calls).toEqual([]);
  });

  test("a hydrated chat DO cannot reassociate its transcript with another session route", async () => {
    const app = await fixture();
    const first = await app.create("First");
    const second = await app.create("Second");
    await (await app.request(`/api/chats/${first.id}/messages`, "POST", { content: "First transcript" })).text();
    await app.drain();
    const persisted = structuredClone(app.records.get(first.id)!);
    const repositoryCalls = [...app.repositoryCalls];
    const response = await app.cores.get(first.id)!.fetch(new Request(`https://app.invalid/api/chats/${second.id}`, {
      headers: { Authorization: "Bearer trusted-app-token" },
    }));
    expect(response.status).toBe(404);
    expect(app.repositoryCalls).toEqual(repositoryCalls);
    expect(app.records.get(first.id)).toEqual(persisted);
    expect(await (await app.request(`/api/chats/${first.id}`)).json() as SessionRecord).toEqual({
      chat: first, messages: persisted.messages,
    });
  });

  test("repository hydration failure never starts a WASM host", async () => {
    const app = await fixture();
    const chat = await app.create();
    app.repository.get = async () => { throw new Error("private-storage-token"); };
    const response = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Hello" });
    expect(response.status).toBe(500);
    expect(await response.text()).not.toContain("private-storage-token");
    expect(app.transports[0].calls).toEqual([]);
    expect(app.records.size).toBe(0);
  });
});

describe("chat execution, commands and persisted lifecycle", () => {
  test("prompt completion cannot close the host before an accepted Stop receipt persists its pause marker", async () => {
    const receipt = barrier();
    const app = await fixture({ blocked: true, stopReceiptGate: receipt.promise });
    const chat = await app.create();
    const response = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "First" });
    await app.transports[0].promptStarted;
    const cancellation = app.request(`/api/chats/${chat.id}/cancel`, "POST");
    await app.transports[0].stopStarted;
    expect((await app.request(`/api/chats/${chat.id}`)).status).toBe(200);
    expect(app.records.get(chat.id)!.pausedByStop).toBeUndefined();
    expect(app.records.get(chat.id)!.running).toBe(true);
    expect(app.transports[0].closeRequested).toBe(false);
    receipt.release();
    expect((await cancellation).status).toBe(200);
    expect(response.status).toBe(202);
    await app.drain();
    expect(app.transports[0].closed).toBe(true);
    expect(app.records.get(chat.id)!.pausedByStop).toEqual({
      lifecycle: 2, controlGeneration: 4, resumeCommandId: "resume:fixture-stop-command",
    });
  });

  test("persists an own-stop marker and resumes only on the next explicit send, before its prompt", async () => {
    const plan: TransportPlan = { blocked: true };
    const app = await fixture(plan);
    const chat = await app.create();
    const first = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "First" });
    await app.transports[0].promptStarted;
    expect((await app.request(`/api/chats/${chat.id}/cancel`, "POST")).status).toBe(200);
    await first.text();
    const marker = structuredClone(app.records.get(chat.id)!.pausedByStop);
    expect(marker).toEqual({ lifecycle: 2, controlGeneration: 4, resumeCommandId: "resume:fixture-stop-command" });
    app.cores.delete(chat.id);
    expect((await app.request(`/api/chats/${chat.id}`)).status).toBe(200);
    expect((await app.request("/api/chats")).status).toBe(200);
    expect(app.transports).toHaveLength(1);
    expect(app.records.get(chat.id)!.pausedByStop).toEqual(marker);
    plan.blocked = false;
    const next = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Continue" });
    expect(next.status).toBe(202);
    await app.drain();
    const calls = app.transports[1].calls;
    const resumeIndex = calls.findIndex(({ method }) => method === "session/control");
    expect(calls[resumeIndex].params).toEqual({ sessionId: chat.id, commandId: marker!.resumeCommandId,
      expectedLifecycle: 2, expectedRevision: 6, expectedControlGeneration: 4, action: { kind: "resume" } });
    expect(calls.findIndex(({ method }) => method === "session/load")).toBeLessThan(resumeIndex);
    expect(resumeIndex).toBeLessThan(calls.findIndex(({ method }) => method === "session/prompt"));
    expect(app.records.get(chat.id)!.pausedByStop).toBeUndefined();
    expect(app.records.get(chat.id)!.messages.at(-1)?.status).toBe("completed");
  });

  test("resume failure retains the own-stop marker and never sends the next prompt", async () => {
    const app = await fixture({ blocked: true, resumeFailure: new Error("resume unavailable") });
    const chat = await app.create();
    const first = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "First" });
    await app.transports[0].promptStarted;
    await app.request(`/api/chats/${chat.id}/cancel`, "POST");
    await first.text();
    const marker = structuredClone(app.records.get(chat.id)!.pausedByStop);
    const next = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Continue" });
    expect(next.status).toBe(202);
    await app.drain();
    expect(app.records.get(chat.id)!.pausedByStop).toEqual(marker);
    expect(app.transports[1].calls.some(({ method }) => method === "session/prompt")).toBe(false);
  });

  test("does not resume an external pause without an own-stop marker", async () => {
    const app = await fixture();
    const chat = await app.create();
    app.transports[0].controlSnapshot.state.status = "paused";
    const response = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Continue" });
    expect(response.status).toBe(202);
    await app.drain();
    expect(app.transports[0].calls.some(({ method }) => method === "session/control")).toBe(false);
    expect(app.records.get(chat.id)!.pausedByStop).toBeUndefined();
  });

  test("repository outage does not prevent cancelling an already hydrated running chat", async () => {
    const app = await fixture({ blocked: true });
    const chat = await app.create();
    const response = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Hello" });
    await app.transports[0].promptStarted;
    app.repository.get = async () => { throw new Error("Turso unavailable"); };
    expect((await app.request(`/api/chats/${chat.id}/cancel`, "POST")).status).toBe(200);
    expect(response.status).toBe(202);
    await app.drain();
    expect(app.records.get(chat.id)!.messages.at(-1)).toMatchObject({ content: "你好🙂", status: "cancelled" });
    expect(app.transports[0].closed).toBe(true);
  });

  test("rejects an over-budget persisted history before appending messages or starting ACP", async () => {
    const app = await fixture();
    const chat = await app.create();
    const record: SessionRecord = { chat, messages: [] };
    app.records.set(chat.id, record);
    record.messages.push({
      id: "large-message", role: "assistant", content: "x".repeat(MAX_TRANSCRIPT_BYTES),
      status: "completed", createdAt: chat.updatedAt,
    });
    app.cores.delete(chat.id);
    expect((await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Next" })).status).toBe(413);
    expect(app.records.get(chat.id)!.messages).toHaveLength(1);
    expect(app.transports[0].calls).toEqual([]);
  });

  test("accepts a reply exactly at the encoded output limit", async () => {
    const text = "x".repeat(MAX_REPLY_BYTES);
    const app = await fixture({ chunks: [text] });
    const chat = await app.create();
    const response = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Hello" });
    expect(response.status).toBe(202);
    await app.drain();
    expect(app.records.get(chat.id)!.messages.at(-1)).toMatchObject({ content: text, status: "completed" });
  });

  test("counts JSON escaping in the reply budget, cancels oversize output and preserves the accepted prefix", async () => {
    const app = await fixture({ chunks: ["partial", '"'.repeat(MAX_REPLY_BYTES / 2)] });
    const chat = await app.create();
    const response = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Hello" });
    expect(response.status).toBe(202);
    await app.drain();
    expect(app.records.get(chat.id)!.messages.at(-1)).toMatchObject({ content: "partial", status: "error" });
    expect(app.transports[0].calls.some(({ method }) => method === "session/control")).toBe(true);
  });

  test("cancel waits for confirmed transport close and durable settlement while the chat remains busy", async () => {
    const closing = barrier();
    const app = await fixture({ blocked: true, closeGate: closing.promise });
    const chat = await app.create();
    const response = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Hello" });
    await app.transports[0].promptStarted;
    let cancelSettled = false;
    const cancellation = app.request(`/api/chats/${chat.id}/cancel`, "POST").then((result) => {
      cancelSettled = true;
      return result;
    });
    await app.transports[0].closeStarted;
    expect((await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Too early" })).status).toBe(409);
    expect(cancelSettled).toBe(false);
    expect(app.records.get(chat.id)!.running).toBe(true);
    closing.release();
    expect((await cancellation).status).toBe(200);
    await response.text();
    await app.drain();
    expect(app.transports[0].closed).toBe(true);
    expect(app.records.get(chat.id)!.running).toBe(false);
    expect(app.records.get(chat.id)!.messages.at(-1)?.status).toBe("cancelled");
  });

  test("cancel reports shutdown failure and new messages fail closed", async () => {
    const app = await fixture({ blocked: true, closeFailure: new Error("shutdown uncertain") });
    const chat = await app.create();
    const response = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Hello" });
    await app.transports[0].promptStarted;
    expect((await app.request(`/api/chats/${chat.id}/cancel`, "POST")).status).toBe(500);
    expect(response.status).toBe(202);
    await app.drain();
    expect((await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Again" })).status).toBe(503);
    expect(app.records.get(chat.id)!.messages.at(-1)?.status).toBe("error");
  });

  test("cancel during startup waits and skips the prompt without sending a targetless Stop", async () => {
    const starting = barrier();
    const app = await fixture({ startupGate: starting.promise });
    const chat = await app.create();
    const response = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Hello" });
    await app.startupStarted;
    const cancellation = app.request(`/api/chats/${chat.id}/cancel`, "POST");
    expect((await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Too early" })).status).toBe(409);
    starting.release();
    expect((await cancellation).status).toBe(200);
    expect(response.status).toBe(202);
    await app.drain();
    expect(app.transports[0].calls.some(({ method }) => method === "session/prompt")).toBe(false);
    expect(app.transports[0].calls.some(({ method }) => method === "session/control/state" || method === "session/control")).toBe(false);
    expect(app.records.get(chat.id)!.messages.at(-1)?.status).toBe("cancelled");
  });

  test("projects only root-session notifications, saves final messages, closes transport and reloads the same Peri session", async () => {
    const app = await fixture();
    const chat = await app.create();
    const response = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Hello" });
    expect(response.status).toBe(202);
    expect(response.headers.get("Content-Type")).toContain("application/json");
    expect(response.status).toBe(202);
    await app.drain();
    await app.drain();
    const record = app.records.get(chat.id)!;
    expect(record.chat.id).toBe(chat.id);
    expect(app.transports[0].calls.find(({ method }) => method === "session/load")?.params).toEqual({
      cwd: "/workspace", mcpServers: [], sessionId: chat.id,
    });
    expect(app.transports[0].calls.some(({ method }) => method === "session/new")).toBe(false);
    expect(record.running).toBe(false);
    expect(record.messages.map(({ role, content, status }) => ({ role, content, status }))).toEqual([
      { role: "user", content: "Hello", status: "completed" },
      { role: "assistant", content: "你好🙂", status: "completed" },
    ]);
    expect(app.transports[0].closed).toBe(true);
    expect(app.transports[0].unsubscribed).toBe(true);
    app.cores.delete(chat.id);
    expect(await (await app.request(`/api/chats/${chat.id}`)).json() as SessionRecord).toEqual({ chat, messages: record.messages });
    const second = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Again" });
    expect(second.status).toBe(202);
    await app.drain();
    expect(app.transports[1].calls.find(({ method }) => method === "session/load")?.params).toEqual({
      cwd: "/workspace", mcpServers: [], sessionId: chat.id,
    });
    expect(app.transports[1].calls.some(({ method }) => method === "session/new")).toBe(false);
    expect((await (await app.request("/api/chats")).json() as { chats: Chat[] }).chats[0]).toEqual(chat);
  });

  test("rejects concurrent prompts with 409 and cancellation delivers an exact-target typed Stop", async () => {
    const app = await fixture({ blocked: true, chunks: ["partial"] });
    const chat = await app.create();
    const response = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "First" });
    await app.transports[0].promptStarted;
    expect((await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Second" })).status).toBe(409);
    expect(await (await app.request(`/api/chats/${chat.id}/cancel`, "POST")).json() as { cancelled: boolean }).toEqual({ cancelled: true });
    expect(response.status).toBe(202);
    await app.drain();
    await app.drain();
    expect(app.transports[0].calls.filter(({ method }) => method === "session/control")).toEqual([
      { method: "session/control", params: { sessionId: chat.id, commandId: "fixture-stop-command",
        expectedLifecycle: 2, expectedRevision: 5, expectedControlGeneration: 3,
        action: { kind: "stop", target: { turnId: "fixture-turn", attemptId: "fixture-attempt" } } } },
    ]);
    expect(app.records.get(chat.id)!.messages.at(-1)).toMatchObject({ content: "partial", status: "cancelled" });
    expect(app.records.get(chat.id)!.messages).toHaveLength(2);
    expect(await (await app.request(`/api/chats/${chat.id}/cancel`, "POST")).json() as { cancelled: boolean }).toEqual({ cancelled: false });
  });

  test("command response disconnect does not cancel ACP; explicit Stop preserves the partial reply", async () => {
    const app = await fixture({ blocked: true, chunks: ["partial"] });
    const chat = await app.create();
    const response = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Hello" });
    await app.transports[0].promptStarted;
    await response.body!.cancel();
    expect(app.transports[0].calls.some(({ method }) => method === "session/control")).toBe(false);
    expect(app.transports[0].closed).toBe(false);
    expect((await app.request(`/api/chats/${chat.id}/cancel`, "POST")).status).toBe(200);
    await app.drain();
    expect(app.transports[0].closed).toBe(true);
    expect(app.records.get(chat.id)!.messages.at(-1)).toMatchObject({ content: "partial", status: "cancelled" });
  });

  test("model failure publishes error state, persists partial reply, and redacts configured secrets", async () => {
    const app = await fixture({ chunks: ["partial"], failure: new Error("private-model-token private-storage-token trusted-app-token libsql://fixture.invalid https://model.invalid/v1") });
    const chat = await app.create();
    const response = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Hello" });
    expect(response.status).toBe(202);
    await app.drain();
    expect(app.records.get(chat.id)!.messages.at(-1)).toMatchObject({
      content: "partial", status: "error", error: "[redacted] [redacted] [redacted] [redacted] [redacted]",
    });
    expect(app.transports[0].closed).toBe(true);
  });

  test.each([
    "WASM rejection private-model-token",
    { message: "WASM rejection private-model-token" },
  ])("preserves useful diagnostics from non-Error rejection %s and still redacts secrets", async (failure) => {
    const app = await fixture({ chunks: [], failure });
    const chat = await app.create();
    const response = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Hello" });
    expect(response.status).toBe(202);
    await app.drain();
    expect(app.records.get(chat.id)!.messages.at(-1)?.error).toBe("WASM rejection [redacted]");
    expect(app.transports[0].closed).toBe(true);
  });

  test("unconfirmed transport shutdown fails the job and prevents another host in the same instance", async () => {
    const app = await fixture({ closeFailure: new Error("close failed") });
    const chat = await app.create();
    const response = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Hello" });
    expect(response.status).toBe(202);
    await app.drain();
    expect((await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Again" })).status).toBe(503);
    expect(app.transports).toHaveLength(1);
  });

  test("unconfirmed shutdown remains blocked after the DO core is reconstructed", async () => {
    const app = await fixture({ closeFailure: new Error("close failed") });
    const chat = await app.create();
    const response = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Hello" });
    expect(response.status).toBe(202);
    await app.drain();
    expect(app.records.get(chat.id)!.executionBlocked).toBe(true);
    app.cores.delete(chat.id);
    expect((await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "After restart" })).status).toBe(503);
    expect((await app.request(`/api/chats/${chat.id}/cancel`, "POST")).status).toBe(503);
    expect((await app.request(`/api/chats/${chat.id}`)).status).toBe(200);
    expect(app.transports).toHaveLength(1);
    expect(app.records.get(chat.id)!.messages).toHaveLength(2);
  });

  test("separate chats remain independently cancellable while both are generating", async () => {
    const app = await fixture({ blocked: true });
    const first = await app.create("First chat");
    const second = await app.create("Second chat");
    const firstResponse = await app.request(`/api/chats/${first.id}/messages`, "POST", { content: "First input" });
    await app.transports[0].promptStarted;
    const secondResponse = await app.request(`/api/chats/${second.id}/messages`, "POST", { content: "Second input" });
    await app.transports[1].promptStarted;
    expect((await app.request(`/api/chats/${first.id}/cancel`, "POST")).status).toBe(200);
    await firstResponse.text();
    expect(app.records.get(second.id)!.running).toBe(true);
    expect(app.transports[1].calls.some(({ method }) => method === "session/control")).toBe(false);
    expect((await app.request(`/api/chats/${second.id}/cancel`, "POST")).status).toBe(200);
    await secondResponse.text();
    expect(app.records.get(first.id)!.messages[0].content).toBe("First input");
    expect(app.records.get(second.id)!.messages[0].content).toBe("Second input");
    expect(app.transports.every((transport) => transport.closed)).toBe(true);
  });

  test("restoring an interrupted persisted run marks it failed without executing a new host", async () => {
    const app = await fixture();
    const chat = await app.create();
    const record: SessionRecord = { chat, messages: [] };
    app.records.set(chat.id, record);
    record.running = true;
    record.messages.push({ id: "assistant", role: "assistant", content: "saved partial", status: "running", createdAt: chat.updatedAt });
    app.cores.delete(chat.id);
    const response = await app.request(`/api/chats/${chat.id}`);
    const restored = await response.json() as SessionRecord;
    expect(restored.messages[0]).toMatchObject({ content: "saved partial", status: "error" });
    expect(restored.messages[0].error).toContain("not automatically resumed");
    expect(app.records.get(chat.id)!.running).toBe(false);
    expect(app.records.get(chat.id)!.chat.id).toBe(chat.id);
    expect(app.transports[0].calls).toEqual([]);
  });

  test("rejects blank content without appending history and allows a later valid request", async () => {
    const app = await fixture();
    const chat = await app.create();
    expect((await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: " " })).status).toBe(400);
    expect((await (await app.request(`/api/chats/${chat.id}`)).json() as SessionRecord).messages).toEqual([]);
    const response = await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Valid" });
    expect(response.status).toBe(202);
    await app.drain();
  });

  test("denies ACP permission requests and unsupported reverse capabilities", async () => {
    const app = await fixture();
    const chat = await app.create();
    await (await app.request(`/api/chats/${chat.id}/messages`, "POST", { content: "Hello" })).text();
    await app.drain();
    const handler = app.transports[0].handler!;
    expect(await handler("session/request_permission", {})).toEqual({ outcome: { outcome: "cancelled" } });
    expect(() => handler("fs/read_text_file", { path: "/secret" })).toThrow("not authorized");
  });
});
