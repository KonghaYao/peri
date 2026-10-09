import { describe, expect, test } from "bun:test";
import { ChatSessionCore } from "../worker/chat/session";
import { fetchApi } from "../worker/api/router";
import { SessionDocReplica, type JsonRpcNotification } from "../worker/sdk";
import { ChatSockets, type SyncSocket } from "../worker/chat/sync";
import type { AcpTransport, Chat, Env, SessionRecord, SessionState } from "../worker/types";
import { decodeSyncFrame, encodeAckFrame, encodeAuthFrame, encodeSyncFrame, FRAME_AUTH,
  SYNC_WIRE_VERSION, MAX_AUTH_FRAME_BYTES } from "../shared/sync";
import { readSyncState } from "../shared/sync-state";

const chat: Chat = { id: "00000000-0000-4000-8000-000000000001", title: "Sync fixture", updatedAt: "2026-10-06T00:00:00.000Z" };

class FixtureSocket implements SyncSocket {
  readonly frames: Uint8Array[] = [];
  readonly replica = new SessionDocReplica();
  readyState = 1;
  autoAck = true;
  deferClose = false;
  closed?: { code: number; reason: string };
  deserializeAttachment?: () => unknown;
  private messages: ((event: { data: unknown }) => void)[] = [];
  private closures: (() => void)[] = [];

  accept(): void {}
  send(message: Uint8Array): void {
    const frame = decodeSyncFrame(message);
    if (frame.type === "snapshot") this.replica.applySnapshot(frame.snapshot);
    else this.replica.applyUpdate(frame.update);
    this.frames.push(message);
    if (this.autoAck) queueMicrotask(() => this.receive(encodeAckFrame(frame.delivery)));
  }
  close(code: number, reason: string): void {
    this.closed = { code, reason };
    if (this.deferClose) { this.readyState = 2; return; }
    this.confirmClose();
  }
  confirmClose(): void {
    this.readyState = 3;
    for (const listener of this.closures) listener();
  }
  addEventListener(type: string, listener: ((event: { data: unknown }) => void) | (() => void)): void {
    if (type === "message") this.messages.push(listener);
    else this.closures.push(listener as () => void);
  }
  receive(data: unknown): void { for (const listener of this.messages) listener({ data }); }
  state() { return readSyncState(this.replica.chat, this.replica.session); }
}

class FixtureTransport implements AcpTransport {
  closed = false;
  stops = 0;
  private listeners = new Set<(notification: JsonRpcNotification) => void>();
  private resolve!: (result: { stopReason: string }) => void;
  readonly pending = new Promise<{ stopReason: string }>((resolve) => { this.resolve = resolve; });
  async request<Result>(method: string): Promise<Result> {
    return (method === "initialize" ? { protocolVersion: 1 } : {}) as Result;
  }
  async sendRequest<Result>(): Promise<{ response: Promise<Result> }> { return { response: this.pending as Promise<Result> }; }
  async notify(): Promise<void> {}
  async *events(): AsyncIterable<JsonRpcNotification> {}
  subscribe(listener: (notification: JsonRpcNotification) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }
  setRequestHandler(): void {}
  async close(): Promise<void> { this.closed = true; }
  emit(text: string, extra: Record<string, unknown> = {}): void {
    for (const listener of this.listeners) listener({ jsonrpc: "2.0", method: "session/update", params: {
      sessionId: chat.id, update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text } }, ...extra,
    } });
  }
  finish(stopReason = "end_turn"): void { this.resolve({ stopReason }); }
  done(): void {
    for (const listener of this.listeners) listener({ jsonrpc: "2.0", method: "peri/agent_event_done",
      params: { sessionId: chat.id, stopReason: "end_turn" } });
  }
  userEcho(): void {
    for (const listener of this.listeners) listener({ jsonrpc: "2.0", method: "session/update", params: {
      sessionId: chat.id, update: { sessionUpdate: "user_message_chunk", messageId: "actual-acp-input",
        content: { type: "text", text: "Hello" } },
    } });
  }
}

function fixture(options: { record?: SessionRecord; closeFailure?: boolean; settlementFailure?: boolean; missing?: boolean } = {}) {
  let record = structuredClone(options.record);
  let reads = 0;
  let repositoryReads = 0;
  let starts = 0;
  const jobs: Promise<unknown>[] = [];
  const sockets: FixtureSocket[] = [];
  const transport = new FixtureTransport();
  const state: SessionState = { storage: {
    async get<Value>() { reads++; return structuredClone(record) as Value | undefined; },
    async put<Value>(_key: string, value: Value) { record = structuredClone(value) as SessionRecord; },
  }, waitUntil(job) { jobs.push(job); } };
  const repository = {
    async get() { repositoryReads++; return options.missing ? null : chat; },
    async list() { return [chat]; }, async create() { return chat; },
  };
  const env: Env = { APP_AUTH_TOKEN: "sync-secret", CHAT_SESSIONS: {
    idFromName(name) { return name; }, get() { return { fetch: (request: Request) => core.fetch(request) }; },
  } };
  const core = new ChatSessionCore(state, env, async () => { starts++; return transport; }, () => ({
    handle() { return undefined; }, seal() {},
    async stop() { transport.stops++; transport.finish("cancelled"); }, async resume() {},
    async stopAfterHostClose() { if (options.settlementFailure) throw new Error("settlement failed"); },
  }), repository, () => {
    const socket = new FixtureSocket(); sockets.push(socket);
    return { socket, response: new Response(null, { status: 200 }) };
  });
  if (options.closeFailure) transport.close = async () => { throw new Error("close failed"); };
  async function connect(resume?: Parameters<typeof encodeAuthFrame>[0]["resume"]) {
    const response = await fetchApi(new Request(`https://app.invalid/api/chats/${chat.id}/sync`, {
      headers: { Upgrade: "websocket" },
    }), env, repository);
    const socket = sockets.at(-1)!;
    if (resume) socket.receive(encodeAuthFrame({ type: "auth", token: "sync-secret", resume }));
    return { socket, response };
  }
  const command = (path = "messages") => core.fetch(new Request(`https://app.invalid/api/chats/${chat.id}/${path}`, {
    method: "POST", headers: { Authorization: "Bearer sync-secret", "Content-Type": "application/json" },
    ...(path === "messages" ? { body: JSON.stringify({ content: "Hello" }) } : {}),
  }));
  return { core, env, transport, connect, command, sockets, jobs,
    reads: () => reads, repositoryReads: () => repositoryReads, starts: () => starts, record: () => record,
    drain: () => Promise.all(jobs) };
}

async function until(predicate: () => boolean): Promise<void> {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (predicate()) return;
    await Bun.sleep(2);
  }
  throw new Error("Fixture state did not converge");
}

async function authenticate(app: ReturnType<typeof fixture>) {
  const { socket } = await app.connect();
  socket.receive(encodeAuthFrame({ type: "auth", token: "sync-secret" }));
  await until(() => socket.frames.length > 0 || !!socket.closed);
  return socket;
}

describe("server read-only SDK document synchronization", () => {
  test("closing peers retain their connection admission until actual transport close is confirmed", async () => {
    const app = fixture();
    const peers: FixtureSocket[] = [];
    for (let connection = 0; connection < 16; connection++) {
      const socket = await authenticate(app);
      peers.push(socket);
      socket.deferClose = true;
      socket.receive('{"type":"write"}');
      await until(() => socket.readyState === 2);
    }
    expect((await app.connect()).response.status).toBe(429);
    peers[0]!.confirmClose();
    expect((await app.connect()).response.status).toBe(200);
    for (const socket of peers) socket.confirmClose();
    app.sockets.at(-1)!.close(1000, "Done");
  });
  test("frontdoor forwards browser upgrades without Bearer headers and reads nothing before first-frame auth", async () => {
    const app = fixture();
    const { socket, response } = await app.connect();
    expect(response.status).toBe(200);
    expect(app.reads()).toBe(0);
    expect(app.repositoryReads()).toBe(0);
    expect(socket.frames).toEqual([]);
    socket.close(1000, "Done");
  });

  test.each(["wrong", "sync-secret-extra", "sync-secret\r\nx", "🙂"]) ("rejects invalid token %s without DB or snapshot access", async (token) => {
    const app = fixture();
    const { socket } = await app.connect();
    socket.receive(encodeAuthFrame({ type: "auth", token }));
    await app.drain();
    expect(socket.closed?.code).toBe(4401);
    expect(app.reads()).toBe(0);
    expect(app.repositoryReads()).toBe(0);
    expect(socket.frames).toEqual([]);
  });
  test("rejects an empty token frame without DB or snapshot access", async () => {
    const app = fixture();
    const { socket } = await app.connect();
    socket.receive(new Uint8Array([FRAME_AUTH, SYNC_WIRE_VERSION, 0, 0]));
    await app.drain();
    expect(socket.closed?.code).toBe(4401);
    expect(app.reads()).toBe(0);
    expect(app.repositoryReads()).toBe(0);
    expect(socket.frames).toEqual([]);
  });

  test.each(["{", JSON.stringify({ type: "update", chat: [] }), new Uint8Array([FRAME_AUTH]),
    encodeAckFrame(1)]) ("rejects malformed, text or non-auth first frame", async (raw) => {
      const app = fixture(); const { socket } = await app.connect();
      socket.receive(raw); await app.drain();
      expect(socket.closed?.code).toBe(4401);
      expect(app.reads()).toBe(0);
    });

  test("closes oversized auth with 1009", async () => {
    const app = fixture(); const { socket } = await app.connect();
    socket.receive(new Uint8Array(MAX_AUTH_FRAME_BYTES + 1)); await app.drain();
    expect(socket.closed?.code).toBe(1009); expect(app.reads()).toBe(0);
  });

  test("times out unauthenticated sockets after three seconds without DB access", async () => {
    const app = fixture(); const { socket } = await app.connect();
    await untilTimeout(socket);
    expect(socket.closed?.code).toBe(4401); expect(app.reads()).toBe(0);
  }, 4_500);

  test("admits at most sixteen sockets and releases disconnected slots", async () => {
    const app = fixture();
    try {
      for (let count = 0; count < 16; count++) await app.connect();
      expect((await app.connect()).response.status).toBe(429);
      expect(app.sockets).toHaveLength(16);
      app.sockets[0]!.close(1000, "Release slot");
      expect((await app.connect()).response.status).toBe(200);
      expect(app.reads()).toBe(0);
    } finally { for (const socket of app.sockets) socket.close(1000, "Done"); }
  });

  test("returns 4404 for missing chats only after authenticating", async () => {
    const app = fixture({ missing: true }); const socket = await authenticate(app);
    expect(socket.closed?.code).toBe(4404); expect(socket.frames).toEqual([]);
    expect(app.repositoryReads()).toBe(1);
  });

  test("rejects URL tokens and non-upgrade requests without opening sockets", async () => {
    const app = fixture();
    for (const [suffix, headers, status] of [["?token=sync-secret", { Upgrade: "websocket" }, 400], ["", {}, 426]] as const) {
      const response = await app.core.fetch(new Request(`https://app.invalid/api/chats/${chat.id}/sync${suffix}`, { headers }));
      expect(response.status).toBe(status);
    }
    expect(app.sockets).toEqual([]); expect(app.reads()).toBe(0);
  });

  test("authenticated client updates and repeated auth violate the read-only boundary", async () => {
    const app = fixture();
    const writable = encodeSyncFrame({ type: "update", delivery: 1, update: {
      protocol: 2, generation: "fixture", sequence: 0, chat: new Uint8Array([1]),
    } });
    for (const raw of [writable, encodeAuthFrame({ type: "auth", token: "sync-secret" })]) {
      const socket = await authenticate(app);
      socket.receive(raw);
      expect(socket.closed?.code).toBe(4403);
      expect(socket.state().messages).toEqual([]);
    }
  });

  test("syncs live SDK chunks and terminal state without embedding messages into metadata", async () => {
    const app = fixture(); const socket = await authenticate(app);
    expect(socket.state()).toMatchObject({ chat, messages: [], running: false, executionBlocked: false });
    expect((await app.command()).status).toBe(202);
    await until(() => app.starts() === 1);
    await Bun.sleep(5);
    app.transport.userEcho();
    app.transport.emit("root");
    app.transport.emit("foreign", { sessionId: "another-session" });
    app.transport.emit("old child", { _meta: { "peri.sourceAgentId": "child" } });
    app.transport.emit("child", { _meta: { peri: { sourceAgentId: "child" } } });
    await until(() => socket.state().messages.at(-1)?.content === "root");
    const metadata = socket.replica.session.getMap("peri-cf").get("state");
    app.transport.emit(" tail");
    await until(() => socket.state().messages.at(-1)?.content === "root tail");
    expect(socket.replica.session.getMap("peri-cf").get("state")).toEqual(metadata);
    expect(metadata).not.toHaveProperty("messages");
    app.transport.finish(); await app.drain();
    expect(socket.state().messages.map(({ content, status }) => ({ content, status }))).toEqual([
      { content: "Hello", status: "completed" }, { content: "root tail", status: "completed" },
    ]);
    expect(socket.state().running).toBe(false);
    expect(app.record()?.messages).toEqual(socket.state().messages.map((message) => ({ ...message, status: message.status! })));
    socket.close(1000, "Done");
  });

  test("disconnection leaves execution running and SDK state-vector resume repairs missed chunks", async () => {
    const app = fixture(); const socket = await authenticate(app);
    await app.command(); await Bun.sleep(5); app.transport.emit("first");
    await until(() => socket.state().messages.at(-1)?.content === "first");
    const resume = socket.replica.stateVector();
    socket.close(1000, "Offline");
    app.transport.emit(" missed");
    expect(app.transport.stops).toBe(0); expect(app.transport.closed).toBe(false);
    const { socket: next } = await app.connect();
    for (const raw of socket.frames) {
      const frame = decodeSyncFrame(raw);
      if (frame.type === "snapshot") next.replica.applySnapshot(frame.snapshot);
      else next.replica.applyUpdate(frame.update);
    }
    next.receive(encodeAuthFrame({ type: "auth", token: "sync-secret", resume }));
    await until(() => next.frames.length > 0);
    const frame = decodeSyncFrame(next.frames[0]!);
    expect(frame.type === "snapshot" && frame.snapshot.mode).toBe("delta");
    expect(next.state().messages.at(-1)?.content).toBe("first missed");
    expect((await app.command("cancel")).status).toBe(200); await app.drain();
    expect(next.state().messages.at(-1)?.status).toBe("cancelled");
    next.close(1000, "Done");
  });

  test.each(["close", "settlement"]) ("application error overrides early SDK completion after %s failure", async (failure) => {
    const app = fixture({ closeFailure: failure === "close", settlementFailure: failure === "settlement" });
    const socket = await authenticate(app); await app.command(); await Bun.sleep(5);
    app.transport.emit("partial"); app.transport.done(); app.transport.finish(); await app.drain();
    expect(socket.state()).toMatchObject({ running: false, executionBlocked: true, error: `${failure} failed` });
    expect(socket.state().messages.at(-1)).toMatchObject({ status: "error", error: `${failure} failed`, content: "partial" });
    expect((await app.command()).status).toBe(503);
    socket.close(1000, "Done");
  });

  test("reconstructs persisted cancelled history without starting a host and preserves UI identity", async () => {
    const messages: SessionRecord["messages"] = [
      { id: "user-1", role: "user", content: "old input", createdAt: chat.updatedAt, status: "completed" },
      { id: "assistant-1", role: "assistant", content: "saved partial", createdAt: chat.updatedAt, status: "cancelled" },
    ];
    const app = fixture({ record: { chat, messages } }); const socket = await authenticate(app);
    expect(socket.state().messages).toEqual(messages); expect(app.starts()).toBe(0);
    socket.close(1000, "Done");
  });

  test("a slow socket is evicted without cancelling the background execution", async () => {
    const app = fixture(); const socket = await authenticate(app); await app.command(); await Bun.sleep(5);
    socket.autoAck = false;
    for (let chunk = 0; chunk < 20 && !socket.closed; chunk++) {
      app.transport.emit("reply"); await Bun.sleep(20);
    }
    await until(() => !!socket.closed);
    expect(socket.closed?.code).toBe(1011); expect(app.transport.stops).toBe(0);
    app.transport.finish(); await app.drain();
    expect(app.record()?.messages.at(-1)?.content).toContain("reply");
  });

  test("closes sockets restored from the removed Base64 protocol with 1008 instead of downgrading", () => {
    const previous = new FixtureSocket();
    previous.deserializeAttachment = () => ({ version: 1, chatId: chat.id, phase: "ready" });
    const corrupt = new FixtureSocket();
    corrupt.deserializeAttachment = () => ({ version: 2, chatId: "not-a-uuid" });
    const current = new FixtureSocket();
    current.deserializeAttachment = () => ({ version: 2, chatId: chat.id, phase: "ready", credential: "a".repeat(64),
      nextDelivery: 1, acknowledged: 0, authDeadline: 0, pending: [] });
    const env: Env = { APP_AUTH_TOKEN: "sync-secret", CHAT_SESSIONS: {
      idFromName: (name) => name, get: () => ({ fetch: async () => new Response() }),
    } };
    new ChatSockets(env, {
      storage: { async get() { return undefined; }, async put() {} }, waitUntil() {},
      acceptWebSocket() {}, getWebSockets: () => [previous, corrupt, current],
    }, async () => { throw new Error("Restoring sockets must not load documents"); },
    () => { throw new Error("Restoring sockets must not open transports"); });
    expect(previous.closed).toEqual({ code: 1008, reason: "Sync protocol upgraded; reload required" });
    expect(corrupt.closed).toEqual({ code: 1011, reason: "Invalid restored connection" });
    expect(current.closed).toBeUndefined();
  });
});

async function untilTimeout(socket: FixtureSocket): Promise<void> {
  for (let attempt = 0; attempt < 65 && !socket.closed; attempt++) await Bun.sleep(50);
}
