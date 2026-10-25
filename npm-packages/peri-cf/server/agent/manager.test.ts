import { describe, expect, test } from "bun:test";
import {
  AtomicSessionClaims,
  ChatCloseIncompleteError,
  MemoryKV,
  PayloadReadError,
  type ChatDatabase,
  type ChatFact,
  type ChatSessionDirectoryEntry,
  type ChatYjsChannel,
  type PeriInstancePort,
  type ToolPayloadReadRef,
  type ToolPayloadResponse,
} from "@peri-code/sdk";
import { publicServerInfo, type ServerConfig } from "../config";
import { createApi } from "../api";
import {
  ChatViewRegistry,
  createAppChatManager,
  getChatManager,
  resetChatManagerForTest,
  shutdownChatManager,
  type ChatInstanceFactory,
} from "./index";

const config: ServerConfig = {
  hostname: "127.0.0.1",
  port: 8787,
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
  staticRoot: "/tmp/peri-cf-dist",
};

function entry(
  id: string,
  overrides: Partial<ChatSessionDirectoryEntry> = {},
): ChatSessionDirectoryEntry {
  return {
    sessionId: id,
    title: null,
    cwd: "/persisted/workspace",
    messageCount: 1,
    createdAt: "2026-10-20T12:00:00Z",
    updatedAt: "2026-10-20T12:00:00Z",
    ...overrides,
  };
}

class FakeDatabase implements ChatDatabase {
  readonly entries = new Map<string, ChatSessionDirectoryEntry>();
  readonly getSessionCalls: string[] = [];
  readonly listSessionsCalls: Array<string | undefined> = [];

  async listSessions(path?: string): Promise<ChatSessionDirectoryEntry[]> {
    this.listSessionsCalls.push(path);
    return [...this.entries.values()].filter((item) => path === undefined || item.cwd === path);
  }

  async getSession(id: string): Promise<ChatSessionDirectoryEntry | null> {
    this.getSessionCalls.push(id);
    return this.entries.get(id) ?? null;
  }
}

class FakePort implements PeriInstancePort {
  closed = false;
  closeFailures: number;
  readonly newSessionId: string;
  readonly pending?: Promise<void>;

  constructor(options: {
    newSessionId?: string;
    closeFailures?: number;
    pending?: Promise<void>;
  } = {}) {
    this.newSessionId = options.newSessionId ?? "new-session-id";
    this.closeFailures = options.closeFailures ?? 0;
    this.pending = options.pending;
  }

  attach(_sink: (fact: ChatFact) => void): () => void {
    return () => {};
  }

  async bindNew(options: {
    path: string;
    onSessionCreated?: (sessionId: string) => void | Promise<void>;
  }): Promise<{ sessionId: string }> {
    await options.onSessionCreated?.(this.newSessionId);
    return { sessionId: this.newSessionId };
  }

  async bindExisting(sessionId: string): Promise<{ sessionId: string }> {
    if (this.pending) await this.pending;
    return { sessionId };
  }

  async send(): Promise<void> {}
  async waitForDelivery(): Promise<void> {}
  async dispatch(): Promise<void> {}
  async takeBack(): Promise<void> {}
  async cancel(): Promise<"requested" | "idle"> { return "idle"; }
  async answer(): Promise<void> {}
  async loadEarlierHistory() { return { applied: 0, nextCursor: null }; }

  async readPayload(
    _ref: ToolPayloadReadRef,
  ): Promise<ToolPayloadResponse> {
    throw new PayloadReadError("unavailable", "no payload");
  }

  async close(): Promise<void> {
    if (this.closeFailures > 0) {
      this.closeFailures -= 1;
      throw new Error("port close failed");
    }
    this.closed = true;
  }

  get running(): boolean { return false; }
  get pendingInputCount(): number { return 0; }
  get pendingInteractionCount(): number { return 0; }
  get needsReload(): boolean { return false; }
}

class FakeFactory {
  readonly contexts: Array<Parameters<ChatInstanceFactory>[0]> = [];
  readonly ports: FakePort[] = [];
  readonly create: ChatInstanceFactory;

  constructor(private readonly options: {
    newSessionId?: string;
    closeFailures?: number;
    pending?: Promise<void>;
  } = {}) {
    this.create = (context) => {
      this.contexts.push(context);
      const port = new FakePort(this.options);
      this.ports.push(port);
      return port;
    };
  }
}

class FlakyChannel implements ChatYjsChannel {
  closed = false;
  failures = 1;

  send(_frame: Uint8Array): void {}

  onMessage(_listener: (frame: Uint8Array) => void): () => void {
    return () => {};
  }

  onClose(_listener: (error?: Error) => void): () => void {
    return () => {};
  }

  close(_reason?: Error): void {
    if (this.failures > 0) {
      this.failures -= 1;
      throw new Error("channel close failed");
    }
    this.closed = true;
  }
}

function managerWith(overrides: {
  database?: FakeDatabase;
  factory?: FakeFactory;
  views?: ChatViewRegistry;
} = {}) {
  const database = overrides.database ?? new FakeDatabase();
  const factory = overrides.factory ?? new FakeFactory();
  const manager = createAppChatManager(config, {
    database,
    sessionClaims: new AtomicSessionClaims(new MemoryKV()),
    host: {
      kind: "transport",
      factory: async () => { throw new Error("transport host must not start in manager tests"); },
    },
    instanceFactory: factory.create,
    views: overrides.views,
  });
  return { manager, database, factory };
}

describe("AppChatManager", () => {
  test("createChat 只登记句柄，不读库、不启动实例", () => {
    const { manager, database, factory } = managerWith();
    const chat = manager.createChat();

    expect(chat.sessionId).toBeNull();
    expect(chat.bindingRevision).toBe(0);
    expect(factory.contexts).toEqual([]);
    expect(database.getSessionCalls).toEqual([]);
    expect(manager.findChatBySession("session-1")).toBeUndefined();
  });

  test("加载历史使用数据库 cwd，并复用同一句柄/视图", async () => {
    const { manager, database, factory } = managerWith();
    database.entries.set("session-1", entry("session-1", { cwd: "/persisted/workspace" }));
    const chat = manager.createChat();

    const result = await chat.execute({
      type: "change_session",
      requestId: "request-1",
      payload: { sessionId: "session-1" },
    });

    expect(result).toMatchObject({
      requestId: "request-1",
      chatId: chat.id,
      sessionId: "session-1",
      bindingRevision: 1,
    });
    expect(database.getSessionCalls).toEqual(["session-1"]);
    expect(factory.contexts[0]?.path).toBe("/persisted/workspace");
    expect(manager.findChatBySession("session-1")?.id).toBe(chat.id);

    const again = await chat.execute({
      type: "change_session",
      requestId: "request-2",
      payload: { sessionId: "session-1" },
    });
    expect(again.bindingRevision).toBe(1);
    expect(factory.contexts).toHaveLength(1);
    expect(chat.bindingRevision).toBe(1);
  });

  test("历史 cwd 为空时回退默认 SESSION_CWD", async () => {
    const { manager, database, factory } = managerWith();
    database.entries.set("session-empty-cwd", entry("session-empty-cwd", { cwd: "" }));
    const chat = manager.createChat();

    await chat.execute({
      type: "change_session",
      requestId: "request-3",
      payload: { sessionId: "session-empty-cwd" },
    });

    expect(factory.contexts[0]?.path).toBe("/workspace/");
  });

  test("加载中的会话可被 findChatBySession 定位", async () => {
    let release!: () => void;
    const pending = new Promise<void>((resolve) => { release = resolve; });
    const { manager, database, factory } = managerWith({
      factory: new FakeFactory({ pending }),
    });
    database.entries.set("session-loading", entry("session-loading"));
    const chat = manager.createChat();

    const loading = chat.execute({
      type: "change_session",
      requestId: "request-4",
      payload: { sessionId: "session-loading" },
    });
    for (let i = 0; i < 50 && manager.findChatBySession("session-loading") === undefined; i++) {
      await new Promise((resolve) => setTimeout(resolve, 1));
    }
    expect(manager.findChatBySession("session-loading")?.id).toBe(chat.id);
    expect(chat.bindingRevision).toBe(0);
    expect(factory.contexts).toHaveLength(1);

    release();
    await loading;
    expect(chat.bindingRevision).toBe(1);
  });

  test("create_new_session 使用默认 path 并返回真实身份", async () => {
    const { manager, database, factory } = managerWith({
      factory: new FakeFactory({ newSessionId: "created-session" }),
    });
    const chat = manager.createChat();

    const result = await chat.execute({
      type: "create_new_session",
      requestId: "request-5",
      payload: { title: "新对话" },
    });

    expect(result).toMatchObject({
      requestId: "request-5",
      chatId: chat.id,
      sessionId: "created-session",
      bindingRevision: 1,
    });
    expect(factory.contexts[0]?.path).toBe("/workspace/");
    expect(database.getSessionCalls).toEqual([]);
    expect(manager.findChatBySession("created-session")?.id).toBe(chat.id);
  });

  test("close 失败保留句柄、占用与视图，重试成功后移除", async () => {
    const views = new ChatViewRegistry();
    const { manager, database, factory } = managerWith({
      views,
      factory: new FakeFactory({ closeFailures: 1 }),
    });
    database.entries.set("session-close", entry("session-close"));
    const chat = manager.createChat();
    await chat.execute({
      type: "change_session",
      requestId: "request-6",
      payload: { sessionId: "session-close" },
    });
    expect(views.attach(chat)).toBe(views.attach(chat));

    await expect(manager.closeChat(chat.id)).rejects.toBeInstanceOf(ChatCloseIncompleteError);
    expect(manager.getChat(chat.id)).toBe(chat);
    expect(manager.findChatBySession("session-close")?.id).toBe(chat.id);
    expect(views.size).toBe(1);
    expect(manager.hasPendingClose(chat.id)).toBe(false);

    await manager.closeChat(chat.id);
    expect(manager.getChat(chat.id)).toBeUndefined();
    expect(manager.findChatBySession("session-close")).toBeUndefined();
    expect(views.size).toBe(0);
    expect(factory.ports[0]?.closed).toBe(true);
  });

  test("view 释放失败保留条目，HTTP DELETE 重试可释放", async () => {
    const views = new ChatViewRegistry();
    const { manager, database } = managerWith({ views });
    database.entries.set("session-view", entry("session-view"));
    const chat = manager.createChat();
    await chat.execute({
      type: "change_session",
      requestId: "request-view",
      payload: { sessionId: "session-view" },
    });
    const view = views.attach(chat);
    const channel = new FlakyChannel();
    view.sync(channel);

    await expect(manager.closeChat(chat.id)).rejects.toBeInstanceOf(
      ChatCloseIncompleteError,
    );
    expect(manager.getChat(chat.id)).toBeUndefined();
    expect(views.size).toBe(1);
    expect(channel.closed).toBe(false);
    expect(manager.hasPendingClose(chat.id)).toBe(true);

    // 句柄已移除后，HTTP DELETE 仍应进入残留条目重试而不是 410。
    const app = createApi({
      manager,
      info: publicServerInfo(config),
      logError: () => {},
    });
    const retried = await app.request(`/api/chats/${chat.id}`, { method: "DELETE" });
    expect(retried.status).toBe(204);
    expect(views.size).toBe(0);
    expect(channel.closed).toBe(true);
    expect(manager.hasPendingClose(chat.id)).toBe(false);
  });

  test("listSessions 只读目录，不创建实例", async () => {
    const { manager, database, factory } = managerWith();
    database.entries.set("session-list", entry("session-list", { title: "列表项" }));

    const listing = await manager.listSessions();
    expect(listing.map((item) => item.sessionId)).toEqual(["session-list"]);
    expect(database.listSessionsCalls).toEqual([undefined]);
    expect(factory.contexts).toEqual([]);
  });

  test("shutdown 释放视图并拒绝新句柄", async () => {
    const views = new ChatViewRegistry();
    const { manager, database } = managerWith({ views });
    database.entries.set("session-shutdown", entry("session-shutdown"));
    const chat = manager.createChat();
    await chat.execute({
      type: "change_session",
      requestId: "request-7",
      payload: { sessionId: "session-shutdown" },
    });
    views.attach(chat);

    await manager.shutdown();
    expect(views.size).toBe(0);
    expect(manager.getChat(chat.id)).toBeUndefined();
    expect(() => manager.createChat()).toThrow("shut down");
  });

  test("进程级单例不重复创建，shutdown 后清空", async () => {
    resetChatManagerForTest();
    const database = new FakeDatabase();
    const options = {
      database,
      sessionClaims: new AtomicSessionClaims(new MemoryKV()),
      host: {
        kind: "transport" as const,
        factory: async () => { throw new Error("not used"); },
      },
    };
    const first = getChatManager(config, options);
    const second = getChatManager(config, options);
    expect(second).toBe(first);

    await shutdownChatManager();
    const third = getChatManager(config, options);
    expect(third).not.toBe(first);
    await shutdownChatManager();
    resetChatManagerForTest();
  });
});
