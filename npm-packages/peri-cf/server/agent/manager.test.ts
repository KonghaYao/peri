import { describe, expect, test } from "bun:test";
import {
  ChatCloseIncompleteError,
  ChatViewRegistry,
  type ChatBindingTarget,
  type ChatFactObserver,
  type ChatFactSource,
  type ChatManagerContract,
  type ChatServiceContract,
  type ChatSessionDirectoryEntry,
  type ChatYjsChannel,
} from "@peri-code/sdk";
import {
  AppChatManager,
  getChatManager,
  shutdownChatManager,
  type ChatPlatformConfig,
} from "./index";

const config: ChatPlatformConfig = {
  machineId: "11111111-1111-4111-8111-111111111111",
  model: {
    provider: "openai",
    id: "gpt-test",
    baseUrl: "https://api.example.com/v1",
    apiKey: "model-secret",
  },
  storage: { url: "https://store.example.com", authToken: "store-secret" },
  mcpServers: [],
  wasmModuleUrl: "file:///tmp/peri-wasm.js",
};

const TARGET: ChatBindingTarget = {
  chatId: "chat-1",
  sessionId: "session-1",
  bindingRevision: 1,
};

function entry(id: string): ChatSessionDirectoryEntry {
  return {
    sessionId: id,
    title: null,
    cwd: "/workspace/",
    messageCount: 1,
    createdAt: "2026-10-20T12:00:00Z",
    updatedAt: "2026-10-20T12:00:00Z",
  };
}

class FakeFactSource implements ChatFactSource {
  binding: ChatBindingTarget | null = null;
  private readonly observers = new Set<ChatFactObserver>();

  subscribeFacts(observer: ChatFactObserver): () => void {
    this.observers.add(observer);
    if (this.binding) observer.onBinding(this.binding, []);
    return () => this.observers.delete(observer);
  }

  bind(target: ChatBindingTarget): void {
    this.binding = { ...target };
    for (const observer of [...this.observers]) observer.onBinding(this.binding, []);
  }
}

/** 只实现编排层会触达的成员；其余动作在 fake 中显式失败。 */
function stubChat(
  id: string,
  source = new FakeFactSource(),
): ChatServiceContract & { readonly source: FakeFactSource } {
  return {
    id,
    sessionId: null,
    bindingRevision: 0,
    binding: null,
    execute(): never { throw new Error("stub chat cannot execute actions"); },
    readPayload(): Promise<never> { throw new Error("stub chat has no payload store"); },
    async close(): Promise<void> {},
    subscribeFacts: (observer) => source.subscribeFacts(observer),
    source,
  };
}

class FakeChatManager implements ChatManagerContract {
  readonly chats = new Map<string, ChatServiceContract>();
  readonly listCalls: Array<{ path?: string } | undefined> = [];
  listResult: ChatSessionDirectoryEntry[] = [entry("session-1")];
  findResult: ChatServiceContract | undefined;
  createError: unknown;
  closeError: unknown;
  shutdownError: unknown;
  private created = 0;

  constructor(readonly events: string[] = []) {}

  createChat(options?: { path?: string }): ChatServiceContract {
    this.events.push(`port.createChat:${options?.path ?? ""}`);
    if (this.createError !== undefined) throw this.createError;
    const chat = stubChat(`chat-${++this.created}`);
    this.chats.set(chat.id, chat);
    return chat;
  }

  getChat(id: string): ChatServiceContract | undefined {
    return this.chats.get(id);
  }

  findChatBySession(_sessionId: string): ChatServiceContract | undefined {
    return this.findResult;
  }

  async listSessions(options?: { path?: string }): Promise<ChatSessionDirectoryEntry[]> {
    this.listCalls.push(options);
    return this.listResult;
  }

  async closeChat(id: string): Promise<void> {
    this.events.push(`port.closeChat:${id}`);
    if (this.closeError !== undefined) throw this.closeError;
    this.chats.delete(id);
  }

  async shutdown(): Promise<void> {
    this.events.push("port.shutdown");
    if (this.shutdownError !== undefined) throw this.shutdownError;
  }
}

/** 只记录 View 释放顺序；`failures` 控制前几次 close 抛错。 */
class RecordingChannel implements ChatYjsChannel {
  closed = false;
  failures: number;

  constructor(
    private readonly events: string[],
    failures = 0,
  ) {
    this.failures = failures;
  }

  send(_frame: Uint8Array): void {}
  onMessage(_listener: (frame: Uint8Array) => void): () => void { return () => {}; }
  onClose(_listener: (error?: Error) => void): () => void { return () => {}; }

  close(_reason?: Error): void {
    this.events.push("view.close");
    if (this.failures > 0) {
      this.failures -= 1;
      throw new Error("view close failed");
    }
    this.closed = true;
  }
}

/** 在端口与注册表中登记同一个句柄，并挂一条可观察的同步连接。 */
function attachChat(
  chats: FakeChatManager,
  views: ChatViewRegistry,
  id: string,
  options: { events: string[]; closeFailures?: number },
): ChatServiceContract & { readonly source: FakeFactSource } {
  const chat = stubChat(id);
  chat.source.bind({ ...TARGET, chatId: id });
  chats.chats.set(id, chat);
  views.attach(chat).sync(new RecordingChannel(options.events, options.closeFailures));
  return chat;
}

function orchestrated(options: {
  events?: string[];
  views?: ChatViewRegistry;
  chats?: FakeChatManager;
} = {}) {
  const events = options.events ?? [];
  const chats = options.chats ?? new FakeChatManager(events);
  const views = options.views ?? new ChatViewRegistry();
  return { manager: new AppChatManager({ chats, views }), chats, views, events };
}

describe("AppChatManager", () => {
  test("四个透传方法直接委托注入的端口", async () => {
    const { manager, chats } = orchestrated();

    const created = manager.createChat({ path: "/workspace/" });
    expect(chats.events).toEqual(["port.createChat:/workspace/"]);
    expect(manager.getChat(created.id)).toBe(created);

    chats.findResult = created;
    expect(manager.findChatBySession("session-1")).toBe(created);

    const listing = await manager.listSessions({ path: "/workspace/" });
    expect(listing).toBe(chats.listResult);
    expect(chats.listCalls).toEqual([{ path: "/workspace/" }]);
  });

  test("createChat 的端口错误原样向上抛出", () => {
    const { manager, chats } = orchestrated();
    chats.createError = new Error("create failed");
    expect(() => manager.createChat()).toThrow("create failed");
  });

  test("closeChat 先关闭端口句柄，再释放 View", async () => {
    const events: string[] = [];
    const { manager, chats, views } = orchestrated({ events });
    const chat = attachChat(chats, views, "chat-1", { events });

    await manager.closeChat(chat.id);

    expect(events).toEqual([
      `port.closeChat:${chat.id}`,
      "view.close",
    ]);
    expect(manager.getChat(chat.id)).toBeUndefined();
    expect(views.get(chat.id)).toBeUndefined();
    expect(manager.hasPendingClose(chat.id)).toBe(false);
    expect(chats.chats.has(chat.id)).toBe(false);
  });

  test("端口关闭失败时不释放 View，也不包装错误", async () => {
    const events: string[] = [];
    const chats = new FakeChatManager(events);
    const failure = new Error("port close failed");
    chats.closeError = failure;
    const { manager, views } = orchestrated({ events, chats });
    const chat = attachChat(chats, views, "chat-1", { events });

    const error = await manager.closeChat(chat.id).catch((reason: unknown) => reason);
    expect(error).toBe(failure);
    expect(events).toEqual([`port.closeChat:${chat.id}`]);
    expect(views.get(chat.id)).toBeDefined();
    expect(manager.getChat(chat.id)).toBe(chat);
  });

  test("View 释放失败包装为 ChatCloseIncompleteError，重试后清除残留", async () => {
    const events: string[] = [];
    const { manager, chats, views } = orchestrated({ events });
    const chat = attachChat(chats, views, "chat-1", { events, closeFailures: 1 });

    await expect(manager.closeChat(chat.id)).rejects.toBeInstanceOf(
      ChatCloseIncompleteError,
    );
    expect(manager.getChat(chat.id)).toBeUndefined();
    expect(views.get(chat.id)).toBeDefined();
    expect(manager.hasPendingClose(chat.id)).toBe(true);

    await manager.closeChat(chat.id);
    expect(views.get(chat.id)).toBeUndefined();
    expect(manager.hasPendingClose(chat.id)).toBe(false);
    expect(events).toEqual([
      `port.closeChat:${chat.id}`,
      "view.close",
      `port.closeChat:${chat.id}`,
      "view.close",
    ]);
  });

  test("hasPendingClose 只在句柄消失而 View 仍存在时为真", async () => {
    const events: string[] = [];
    const { manager, chats, views } = orchestrated({ events });
    const chat = attachChat(chats, views, "chat-1", { events, closeFailures: 1 });

    expect(manager.hasPendingClose(chat.id)).toBe(false);
    expect(manager.hasPendingClose("missing")).toBe(false);

    await expect(manager.closeChat(chat.id)).rejects.toBeInstanceOf(
      ChatCloseIncompleteError,
    );
    expect(manager.hasPendingClose(chat.id)).toBe(true);
    await manager.closeChat(chat.id);
    expect(manager.hasPendingClose(chat.id)).toBe(false);
  });

  test("shutdown 先释放全部 View，再关闭端口", async () => {
    const events: string[] = [];
    const { manager, chats, views } = orchestrated({ events });
    const first = attachChat(chats, views, "chat-1", { events });
    const second = attachChat(chats, views, "chat-2", { events });

    await manager.shutdown();

    expect(events).toEqual([
      "view.close",
      "view.close",
      "port.shutdown",
    ]);
    expect(views.get(first.id)).toBeUndefined();
    expect(views.get(second.id)).toBeUndefined();
  });

  test("shutdown 聚合 View 与端口的失败，且两段都会尝试", async () => {
    const events: string[] = [];
    const chats = new FakeChatManager(events);
    chats.shutdownError = new Error("port shutdown failed");
    const { manager, views } = orchestrated({ events, chats });
    const chat = attachChat(chats, views, "chat-1", { events, closeFailures: 1 });

    const failure = await manager.shutdown().catch((error: unknown) => error);
    expect(failure).toBeInstanceOf(AggregateError);
    expect((failure as AggregateError).errors).toHaveLength(2);
    expect((failure as AggregateError).message).toBe(
      "Peri CF chat manager shutdown is incomplete",
    );
    expect(events).toEqual(["view.close", "port.shutdown"]);
    expect(views.get(chat.id)).toBeDefined();
  });

  test("shutdown 只有 View 失败时仍聚合上报", async () => {
    const events: string[] = [];
    const { manager, chats, views } = orchestrated({ events });
    attachChat(chats, views, "chat-1", { events, closeFailures: 1 });

    const failure = await manager.shutdown().catch((error: unknown) => error);
    expect(failure).toBeInstanceOf(AggregateError);
    expect((failure as AggregateError).errors).toHaveLength(1);
    expect(events).toEqual(["view.close", "port.shutdown"]);
  });
});

describe("进程级单例", () => {
  test("注入端口后不重复创建，shutdown 成功后清空", async () => {
    const chats = new FakeChatManager();
    const first = getChatManager(config, { chats });
    expect(getChatManager(config, { chats: new FakeChatManager() })).toBe(first);

    await shutdownChatManager();
    const second = getChatManager(config, { chats });
    expect(second).not.toBe(first);
    await shutdownChatManager();
  });

  test("shutdown 失败保留单例，允许再次关闭", async () => {
    const chats = new FakeChatManager();
    chats.shutdownError = new Error("port shutdown failed");
    const manager = getChatManager(config, { chats });

    await expect(shutdownChatManager()).rejects.toBeInstanceOf(AggregateError);
    expect(getChatManager(config, { chats })).toBe(manager);

    chats.shutdownError = undefined;
    await shutdownChatManager();
    expect(getChatManager(config, { chats })).not.toBe(manager);
    await shutdownChatManager();
  });
});
