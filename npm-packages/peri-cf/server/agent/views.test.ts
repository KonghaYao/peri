import { describe, expect, test } from "bun:test";
import {
  ChatReplica,
  type ChatBindingTarget,
  type ChatFact,
  type ChatFactEnvelope,
  type ChatFactObserver,
  type ChatFactSource,
  type ChatService,
  type ChatYjsChannel,
} from "@peri-code/sdk";
import { ChatViewRegistry } from "./views";

const TARGET: ChatBindingTarget = {
  chatId: "chat-1",
  sessionId: "session-1",
  bindingRevision: 1,
};

function update(kind: string, fields: Record<string, unknown>) {
  return { jsonrpc: "2.0" as const, method: "session/update", params: {
    sessionId: "session-1", update: { sessionUpdate: kind, ...fields },
  } };
}

class TestChannel implements ChatYjsChannel {
  peer: TestChannel | null = null;
  closed = false;
  closeReason: Error | undefined;
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
    this.closeReason = reason;
    for (const listener of [...this.closes]) listener(reason);
    const peer = this.peer;
    if (peer && !peer.closed) peer.close(reason);
  }
}

class FlakyCloseChannel extends TestChannel {
  failures = 1;

  override close(reason?: Error): void {
    if (this.failures > 0) {
      this.failures -= 1;
      throw new Error("channel close failed");
    }
    super.close(reason);
  }
}

function channelPair(): { server: TestChannel; client: TestChannel } {
  const server = new TestChannel();
  const client = new TestChannel();
  server.peer = client;
  client.peer = server;
  return { server, client };
}

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

  close(reason?: Error): void {
    for (const observer of [...this.observers]) observer.onClosed(reason);
  }

  get observerCount(): number { return this.observers.size; }
}

function fakeChat(id: string, source?: ChatFactSource): ChatService {
  return {
    id,
    subscribeFacts: (observer: ChatFactObserver) =>
      source ? source.subscribeFacts(observer) : () => {},
  } as unknown as ChatService;
}

async function waitFor(check: () => boolean, timeoutMs = 1500): Promise<void> {
  const started = Date.now();
  while (!check()) {
    if (Date.now() - started > timeoutMs) throw new Error("condition timed out");
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

describe("ChatViewRegistry", () => {
  test("同 Chat 共享一个 View，多连接各自同步", async () => {
    const source = new FakeFactSource();
    source.bind(TARGET);
    source.emit({
      kind: "history-batch",
      notifications: [
        update("user_message_chunk", { messageId: "u1", content: { type: "text", text: "Saved" } }),
      ],
    });
    const chat = fakeChat("chat-1", source);
    const registry = new ChatViewRegistry();
    const view = registry.attach(chat);
    expect(registry.attach(chat)).toBe(view);
    expect(source.observerCount).toBe(1);

    const first = new ChatReplica();
    const second = new ChatReplica();
    const firstPair = channelPair();
    const secondPair = channelPair();
    const firstConnection = view.sync(firstPair.server);
    const secondConnection = view.sync(secondPair.server);
    first.connect(firstPair.client);
    second.connect(secondPair.client);

    await waitFor(() => first.status === "synced" && second.status === "synced");
    expect(first.read().sessionId).toBe("session-1");
    expect(second.read().entries[0]?.blocks[0]).toMatchObject({ text: "Saved" });

    await firstConnection.close();
    expect(source.observerCount).toBe(1);
    expect(registry.size).toBe(1);

    source.emit({
      kind: "notice",
      notification: update("agent_message_chunk", {
        messageId: "a1", content: { type: "text", text: "Still live" },
      }),
    });
    await waitFor(() => {
      const block = second.read().entries[1]?.blocks[0];
      return block?.type === "text" && block.text === "Still live";
    });

    await secondConnection.close();
    expect(registry.size).toBe(1);
    expect(registry.revisionOf("chat-1")).toBe(1);
    await first.dispose();
    await second.dispose();
    await registry.disposeAll();
  });

  test("绑定换代复用同一 View，副本成对替换", async () => {
    const source = new FakeFactSource();
    source.bind(TARGET);
    source.emit({ kind: "input-delivered", inputId: "u1", text: "First session" });
    const chat = fakeChat("chat-1", source);
    const registry = new ChatViewRegistry();
    const view = registry.attach(chat);

    const replica = new ChatReplica();
    const pair = channelPair();
    const connection = view.sync(pair.server);
    replica.connect(pair.client);
    await waitFor(() => replica.status === "synced" && replica.read().entries.length === 2);

    source.bind({ ...TARGET, sessionId: "session-2", bindingRevision: 2 });
    source.emit({ kind: "input-delivered", inputId: "u2", text: "Second session" });

    await waitFor(() => replica.read().bindingRevision === 2
      && replica.read().sessionId === "session-2"
      && replica.read().entries.length === 2);
    expect(replica.read().entries[0]?.blocks[0]).toMatchObject({ text: "Second session" });
    expect(registry.attach(chat)).toBe(view);
    expect(registry.revisionOf("chat-1")).toBe(2);

    await connection.close();
    await replica.dispose();
    await registry.disposeAll();
  });

  test("连接断开不释放 View，显式 dispose 才释放", async () => {
    const source = new FakeFactSource();
    source.bind(TARGET);
    const chat = fakeChat("chat-1", source);
    const registry = new ChatViewRegistry();
    const view = registry.attach(chat);

    const pair = channelPair();
    const connection = view.sync(pair.server);
    pair.client.close();
    await waitFor(() => pair.server.closed);
    expect(registry.get("chat-1")).toBe(view);
    expect(registry.size).toBe(1);
    expect(source.observerCount).toBe(1);

    await connection.close();
    await registry.dispose("chat-1");
    expect(registry.get("chat-1")).toBeUndefined();
    expect(source.observerCount).toBe(0);
  });

  test("dispose 失败保留条目，重试成功后才删除并关闭 channel", async () => {
    const source = new FakeFactSource();
    source.bind(TARGET);
    const chat = fakeChat("chat-1", source);
    const registry = new ChatViewRegistry();
    const view = registry.attach(chat);
    const channel = new FlakyCloseChannel();
    view.sync(channel);

    await expect(registry.dispose("chat-1")).rejects.toBeInstanceOf(
      AggregateError,
    );
    expect(registry.get("chat-1")).toBe(view);
    expect(registry.size).toBe(1);
    expect(source.observerCount).toBe(0);
    expect(channel.closed).toBe(false);

    await registry.dispose("chat-1");
    expect(registry.get("chat-1")).toBeUndefined();
    expect(registry.size).toBe(0);
    expect(channel.closed).toBe(true);
  });

  test("同一 Chat id 的不同 ChatService 实例被拒绝", () => {
    const registry = new ChatViewRegistry();
    registry.attach(fakeChat("chat-1"));
    expect(() => registry.attach(fakeChat("chat-1"))).toThrow("different ChatService");
  });
});
