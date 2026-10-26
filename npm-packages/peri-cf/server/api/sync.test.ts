import { describe, expect, test } from "bun:test";
import type { Server } from "bun";
import {
  ChatReplica,
  ChatView,
  ChatViewRegistry,
  type ChatBindingTarget,
  type ChatFact,
  type ChatFactEnvelope,
  type ChatFactObserver,
  type ChatFactSource,
  type ChatServiceContract,
  type ChatYjsChannel,
} from "@peri-code/sdk";
import {
  decodeSyncFrame,
  encodeAckFrame,
  encodeHelloFrame,
} from "@peri-code/sdk/view";
import { ChatRegistry } from "../registry";
import {
  MAX_CLIENT_SYNC_FRAME_BYTES,
  handleSyncUpgrade,
  syncWebSocketHandlers,
  type SyncSocketData,
} from "./sync";

const TARGET: ChatBindingTarget = {
  chatId: "chat-1",
  sessionId: "session-1",
  bindingRevision: 1,
};

const update = (kind: string, fields: Record<string, unknown>) => ({
  jsonrpc: "2.0" as const,
  method: "session/update",
  params: {
    sessionId: "session-1",
    update: { sessionUpdate: kind, ...fields },
  },
});

class FakeFactSource implements ChatFactSource {
  binding: ChatBindingTarget | null = null;
  private sequence = 0;
  private log: ChatFactEnvelope[] = [];
  private readonly observers = new Set<ChatFactObserver>();

  subscribeFacts(observer: ChatFactObserver): () => void {
    this.observers.add(observer);
    if (this.binding) observer.onBinding(this.binding, this.log);
    return () => this.observers.delete(observer);
  }

  bind(target: ChatBindingTarget): void {
    this.binding = { ...target };
    this.sequence = 0;
    this.log = [];
    for (const observer of [...this.observers]) observer.onBinding(this.binding, this.log);
  }

  emit(fact: ChatFact): void {
    if (!this.binding) throw new Error("source has no binding");
    const envelope: ChatFactEnvelope = { ...this.binding, sequence: ++this.sequence, fact };
    this.log.push(envelope);
    for (const observer of [...this.observers]) observer.onFact(envelope);
  }

  get observerCount(): number { return this.observers.size; }
}

/** 同步入口只读取身份与事实源；动作成员显式失败。 */
function fakeChat(id: string, source?: ChatFactSource): ChatServiceContract {
  return {
    id,
    sessionId: null,
    bindingRevision: 0,
    binding: null,
    execute(): never { throw new Error("sync fixture cannot execute actions"); },
    readPayload(): Promise<never> { throw new Error("sync fixture has no payload store"); },
    async close(): Promise<void> {},
    subscribeFacts: (observer: ChatFactObserver) =>
      source ? source.subscribeFacts(observer) : () => {},
  };
}

class TestChannel implements ChatYjsChannel {
  peer: TestChannel | null = null;
  closed = false;
  private readonly messages = new Set<(frame: Uint8Array) => void>();
  private readonly closes = new Set<(error?: Error) => void>();

  send(frame: Uint8Array): void {
    if (this.closed) throw new Error("channel is closed");
    const peer = this.peer;
    if (!peer || peer.closed) throw new Error("peer is closed");
    for (const listener of [...peer.messages]) listener(new Uint8Array(frame));
  }

  onMessage(listener: (frame: Uint8Array) => void): () => void {
    this.messages.add(listener);
    return () => this.messages.delete(listener);
  }

  onClose(listener: (error?: Error) => void): () => void {
    this.closes.add(listener);
    return () => this.closes.delete(listener);
  }

  close(reason?: Error): void {
    if (this.closed) return;
    this.closed = true;
    for (const listener of [...this.closes]) listener(reason);
    const peer = this.peer;
    if (peer && !peer.closed) peer.close(reason);
  }
}

function channelPair(): { server: TestChannel; client: TestChannel } {
  const server = new TestChannel();
  const client = new TestChannel();
  server.peer = client;
  client.peer = server;
  return { server, client };
}

/** 只握手、收帧、不 ACK 的客户端。 */
class MuteChannel implements ChatYjsChannel {
  closed = false;
  readonly frames: Uint8Array[] = [];
  error: Error | undefined;
  private readonly messages = new Set<(frame: Uint8Array) => void>();
  private readonly closes = new Set<(error?: Error) => void>();

  send(frame: Uint8Array): void {
    if (this.closed) throw new Error("channel is closed");
    this.frames.push(new Uint8Array(frame));
  }
  onMessage(listener: (frame: Uint8Array) => void): () => void {
    this.messages.add(listener);
    return () => this.messages.delete(listener);
  }
  onClose(listener: (error?: Error) => void): () => void {
    this.closes.add(listener);
    return () => this.closes.delete(listener);
  }
  close(reason?: Error): void {
    if (this.closed) return;
    this.closed = true;
    this.error = reason;
    for (const listener of [...this.closes]) listener(reason);
  }
  hello(): void {
    const frame = encodeHelloFrame({ type: "hello" });
    for (const listener of [...this.messages]) listener(frame);
  }
}

async function waitFor(check: () => boolean, timeoutMs = 2000): Promise<void> {
  const started = Date.now();
  while (!check()) {
    if (Date.now() - started > timeoutMs) throw new Error("condition timed out");
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

function nextMessage(ws: WebSocket, timeoutMs = 2000): Promise<Uint8Array> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      ws.removeEventListener("message", onMessage);
      reject(new Error("websocket frame timed out"));
    }, timeoutMs);
    const onMessage = (event: MessageEvent): void => {
      clearTimeout(timer);
      ws.removeEventListener("message", onMessage);
      resolve(new Uint8Array(event.data as ArrayBuffer));
    };
    ws.addEventListener("message", onMessage);
  });
}

function nextClose(ws: WebSocket, timeoutMs = 2000): Promise<{ code: number; reason: string }> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("websocket close timed out")), timeoutMs);
    ws.addEventListener("close", (event) => {
      clearTimeout(timer);
      resolve({ code: event.code, reason: event.reason });
    }, { once: true });
  });
}

function startSyncServer(
  chat: ChatServiceContract = fakeChat("chat-1"),
): { server: Server<SyncSocketData>; registry: ChatRegistry } {
  const registry = new ChatRegistry({
    createChat(): ChatServiceContract {
      throw new Error("sync fixture cannot create chats");
    },
    getChat: (id) => id === chat.id ? chat : undefined,
    findChatBySession: () => undefined,
    async listSessions() { return []; },
    async closeChat() {},
    async shutdown() {},
  });
  const server = Bun.serve<SyncSocketData>({
    hostname: "127.0.0.1",
    port: 0,
    fetch(request, server) {
      return handleSyncUpgrade(request, server, registry);
    },
    websocket: syncWebSocketHandlers,
  });
  return { server, registry };
}

async function openSocket(url: string): Promise<WebSocket> {
  const ws = new WebSocket(url);
  ws.binaryType = "arraybuffer";
  await new Promise<void>((resolve, reject) => {
    ws.addEventListener("open", () => resolve(), { once: true });
    ws.addEventListener("error", () => reject(new Error("websocket open failed")), { once: true });
  });
  return ws;
}

describe("sync WebSocket", () => {
  test("HELLO → SNAPSHOT → UPDATE → ACK，断开不释放 View", async () => {
    const source = new FakeFactSource();
    source.bind(TARGET);
    source.emit({
      kind: "history-batch",
      notifications: [
        update("user_message_chunk", { messageId: "u1", content: { type: "text", text: "Saved" } }),
      ],
    });
    const { server, registry } = startSyncServer(fakeChat("chat-1", source));
    const ws = await openSocket(
      `ws://127.0.0.1:${server.port}/api/chats/chat-1/sync`,
    );

    try {
      ws.send(encodeHelloFrame({ type: "hello" }));
      const first = decodeSyncFrame(await nextMessage(ws));
      expect(first.type).toBe("snapshot");
      if (first.type !== "snapshot") throw new Error("expected snapshot");
      ws.send(encodeAckFrame(first.delivery));

      source.emit({ kind: "input-delivered", inputId: "input-1", text: "Hello" });
      const next = decodeSyncFrame(await nextMessage(ws));
      expect(next.type).toBe("update");
      if (next.type !== "update") throw new Error("expected update");
      ws.send(encodeAckFrame(next.delivery));

      const closed = nextClose(ws);
      ws.close();
      await closed;
      await waitFor(() => source.observerCount === 1);
      expect(registry.views.get("chat-1")).toBeDefined();
    } finally {
      ws.close();
      await registry.views.disposeAll();
      server.stop(true);
    }
  });

  test("客户端 Yjs 更新帧被显式关闭", async () => {
    const source = new FakeFactSource();
    source.bind(TARGET);
    const { server, registry } = startSyncServer(fakeChat("chat-1", source));
    const ws = await openSocket(
      `ws://127.0.0.1:${server.port}/api/chats/chat-1/sync`,
    );

    try {
      ws.send(encodeHelloFrame({ type: "hello" }));
      const first = decodeSyncFrame(await nextMessage(ws));
      if (first.type !== "snapshot") throw new Error("expected snapshot");
      ws.send(encodeAckFrame(first.delivery));

      const close = nextClose(ws);
      // FRAME_UPDATE + wire version；SDK 在解析前拒绝非 HELLO/ACK。
      ws.send(new Uint8Array([0x03, 0x02, 0x00]));
      const closed = await close;
      expect(closed.code).toBe(1011);
      expect(closed.reason).toContain("Unexpected chat sync frame tag 3");
    } finally {
      ws.close();
      await registry.views.disposeAll();
      server.stop(true);
    }
  });

  test("未知 Chat 在升级前返回 410", async () => {
    const { server, registry } = startSyncServer();
    try {
      const response = await fetch(
        `http://127.0.0.1:${server.port}/api/chats/missing/sync`,
        { headers: { upgrade: "websocket", connection: "Upgrade" } },
      );
      expect(response.status).toBe(410);
      expect(await response.json()).toMatchObject({ code: "chat_not_found" });
    } finally {
      await registry.views.disposeAll();
      server.stop(true);
    }
  });
});

describe("sync delivery", () => {
  test("慢客户端超出信用被背压关闭", async () => {
    const source = new FakeFactSource();
    source.bind(TARGET);
    const view = new ChatView(source, {
      sync: { maxOutstandingFrames: 1, maxOutstandingBytes: 1024 * 1024, ackTimeoutMs: 5_000 },
    });
    const channel = new MuteChannel();
    view.sync(channel);
    channel.hello();
    await waitFor(() => channel.frames.length === 1);
    source.emit({ kind: "input-delivered", inputId: "input-1", text: "Hello" });
    await waitFor(() => channel.closed);
    expect(channel.error?.name).toBe("SyncBackpressureError");
    await view.dispose();
  });

  test("未 ACK 触发显式超时关闭", async () => {
    const source = new FakeFactSource();
    source.bind(TARGET);
    const view = new ChatView(source, {
      sync: { ackTimeoutMs: 20, maxOutstandingFrames: 8 },
    });
    const channel = new MuteChannel();
    view.sync(channel);
    channel.hello();
    await waitFor(() => channel.frames.length === 1);
    await waitFor(() => channel.closed, 1000);
    expect(channel.error?.name).toBe("ChatSyncTimeoutError");
    await view.dispose();
  });

  test("断线重连使用 HELLO resume 补齐且不重复条目", async () => {
    const source = new FakeFactSource();
    source.bind(TARGET);
    source.emit({ kind: "input-delivered", inputId: "input-1", text: "First" });
    const registry = new ChatViewRegistry();
    const view = registry.attach(fakeChat("chat-1", source));
    const replica = new ChatReplica();

    const firstPair = channelPair();
    const firstConnection = view.sync(firstPair.server);
    replica.connect(firstPair.client);
    await waitFor(() => replica.status === "synced" && replica.read().entries.length === 2);
    const firstIds = replica.read().entries.map((entry) => entry.id);
    await firstConnection.close();

    // 断线期间产生的新事实必须由 HELLO resume 补发，且旧条目不能重复。
    source.emit({ kind: "input-delivered", inputId: "input-2", text: "Second" });

    const secondPair = channelPair();
    const secondConnection = view.sync(secondPair.server);
    replica.connect(secondPair.client);
    await waitFor(() => replica.status === "synced" && replica.read().entries.length === 4);
    const resumedIds = replica.read().entries.map((entry) => entry.id);
    expect(resumedIds.slice(0, firstIds.length)).toEqual(firstIds);
    expect(new Set(resumedIds).size).toBe(resumedIds.length);
    expect(resumedIds).toContain("turn:input-2:user");

    await secondConnection.close();
    await replica.dispose();
    await registry.disposeAll();
    expect(source.observerCount).toBe(0);
  });
});

describe("WebSocketSyncChannel", () => {
  test("超限客户端帧以 1009 显式关闭", async () => {
    const source = new FakeFactSource();
    source.bind(TARGET);
    const { server, registry } = startSyncServer(fakeChat("chat-1", source));
    const ws = await openSocket(
      `ws://127.0.0.1:${server.port}/api/chats/chat-1/sync`,
    );
    try {
      const closed = nextClose(ws);
      ws.send(new Uint8Array(MAX_CLIENT_SYNC_FRAME_BYTES + 1));
      expect((await closed).code).toBe(1009);
    } finally {
      ws.close();
      await registry.views.disposeAll();
      server.stop(true);
    }
  });
});
