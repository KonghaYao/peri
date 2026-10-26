import { describe, expect, test } from "bun:test";
import {
  MemoryKV,
  PayloadReadError,
  type ChatDatabase,
  type ChatRuntime,
  type ChatSessionDirectoryEntry,
  type PeriInstancePort,
  type ToolPayloadReadRef,
  type ToolPayloadResponse,
} from "@peri-code/sdk";
import { DEFAULT_WASM_RUNTIME_CLEANUP_TIMEOUT_MS } from "@peri-code/sdk/wasm";
import {
  SESSION_CWD,
  WORKSPACE_CWD,
  type ServerConfig,
} from "../config";
import { SYSTEM_INSTRUCTIONS } from "../system-instructions";
import {
  createSdkChatManager,
  type ChatInstanceFactory,
} from "./index";

const config: ServerConfig = {
  hostname: "127.0.0.1",
  port: 0,
  staticRoot: "/tmp/peri-cf-missing-static",
  machineId: "11111111-1111-4111-8111-111111111111",
  model: {
    provider: "openai",
    id: "gpt-test",
    baseUrl: "https://api.example.com/v1",
    apiKey: "model-secret",
  },
  storage: { url: "https://store.example.com", authToken: "store-secret" },
  mcpServers: [{
    name: "tools",
    url: "https://mcp.example.com",
    headers: [{ name: "Authorization", value: "Bearer mcp-token" }],
  }],
  wasmModuleUrl: "file:///tmp/peri-wasm.js",
};

const transportRuntime: ChatRuntime = {
  kind: "transport",
  factory: async () => { throw new Error("transport runtime must not start in compose tests"); },
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

  constructor(private readonly newSessionId = "created-session") {}

  attach(_sink: (fact: never) => void): () => void {
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
    return { sessionId };
  }

  async send(): Promise<void> {}
  async waitForDelivery(): Promise<void> {}
  async dispatch(): Promise<void> {}
  async takeBack(): Promise<void> {}
  async cancel(): Promise<"requested" | "idle"> { return "idle"; }
  async answer(): Promise<void> {}
  async loadEarlierHistory() { return { applied: 0, nextCursor: null }; }

  async readPayload(_ref: ToolPayloadReadRef): Promise<ToolPayloadResponse> {
    throw new PayloadReadError("unavailable", "no payload");
  }

  async close(): Promise<void> {
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

  constructor() {
    this.create = (context) => {
      this.contexts.push(context);
      const port = new FakePort();
      this.ports.push(port);
      return port;
    };
  }
}

function assembled(options: {
  useDefaultRuntime?: boolean;
  runtime?: ChatRuntime;
  lifecycle?: { activationCloseTimeoutMs?: number };
  factory?: FakeFactory;
  logError?: (label: string, error: unknown, context?: Record<string, unknown>) => void;
  logs?: Array<{ label: string; error: unknown; context?: Record<string, unknown> }>;
} = {}) {
  const database = new FakeDatabase();
  const factory = options.factory ?? new FakeFactory();
  const logs = options.logs ?? [];
  const manager = createSdkChatManager(config, {
    database,
    sessionOwnershipKv: new MemoryKV(),
    chatConfig: {
      runtime: options.useDefaultRuntime
        ? undefined
        : options.runtime ?? transportRuntime,
      lifecycle: options.lifecycle,
    },
    instanceFactory: factory.create,
    logError: options.logError ?? ((label, error, context) => logs.push({ label, error, context })),
  });
  return { manager, database, factory, logs };
}

/** 触发一次实例装配；返回激活它的句柄。 */
async function activate(
  manager: ReturnType<typeof createSdkChatManager>,
  sessionId: string,
) {
  const chat = manager.createChat();
  await chat.execute({
    type: "change_session",
    requestId: "request-1",
    payload: { sessionId },
  });
  return chat;
}

describe("createSdkChatManager", () => {
  test("createChat 只登记句柄，不读库、不启动实例", () => {
    const { manager, database, factory } = assembled();
    const chat = manager.createChat();

    expect(chat.sessionId).toBeNull();
    expect(chat.bindingRevision).toBe(0);
    expect(factory.contexts).toEqual([]);
    expect(database.getSessionCalls).toEqual([]);
    expect(manager.findChatBySession("session-1")).toBeUndefined();
  });

  test("listSessions 只读目录，不创建实例", async () => {
    const { manager, database, factory } = assembled();
    database.entries.set("session-list", entry("session-list", { title: "列表项" }));

    const listing = await manager.listSessions();
    expect(listing.map((item) => item.sessionId)).toEqual(["session-list"]);
    expect(database.listSessionsCalls).toEqual([undefined]);
    expect(factory.contexts).toEqual([]);
  });

  test("加载历史使用数据库 cwd，并注入平台装配上下文", async () => {
    const { manager, database, factory } = assembled();
    database.entries.set("session-1", entry("session-1"));
    const chat = await activate(manager, "session-1");

    expect(database.getSessionCalls).toEqual(["session-1"]);
    expect(manager.findChatBySession("session-1")?.id).toBe(chat.id);
    const context = factory.contexts[0]!;
    expect(context.chatId).toBe(chat.id);
    expect(context.path).toBe("/persisted/workspace");
    expect(context.database).toBe(database);
    expect(context.defaults).toEqual({
      path: SESSION_CWD,
      instructions: SYSTEM_INSTRUCTIONS,
      mcpServers: {
        tools: {
          type: "http",
          url: "https://mcp.example.com",
          headers: { Authorization: "Bearer mcp-token" },
        },
      },
    });
  });

  test("历史 cwd 为空时回退默认 SESSION_CWD", async () => {
    const { manager, database, factory } = assembled();
    database.entries.set("session-empty-cwd", entry("session-empty-cwd", { cwd: "" }));
    await activate(manager, "session-empty-cwd");

    expect(factory.contexts[0]?.path).toBe(SESSION_CWD);
  });

  test("create_new_session 使用默认 path 并返回真实身份", async () => {
    const { manager, database, factory } = assembled();
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
    expect(factory.contexts[0]?.path).toBe(SESSION_CWD);
    expect(database.getSessionCalls).toEqual([]);
    expect(manager.findChatBySession("created-session")?.id).toBe(chat.id);
  });

  test("harnessConfig 把配置、模型与工具面映射成宿主 agentConfig", async () => {
    const { manager, database, factory } = assembled();
    database.entries.set("session-1", entry("session-1"));
    await activate(manager, "session-1");

    expect(factory.contexts[0]?.agentConfig).toEqual({
      cwd: WORKSPACE_CWD,
      machineId: config.machineId,
      storage: {
        url: config.storage.url,
        authToken: config.storage.authToken,
      },
      settings: {
        config: {
          active_alias: "sonnet",
          providers: [{
            id: "model",
            type: config.model.provider,
            apiKey: config.model.apiKey,
            baseUrl: config.model.baseUrl,
          }],
          profiles: {
            sonnet: { provider: "model", model: config.model.id },
          },
          meta_harness: expect.objectContaining({
            "01_intro": false,
            TodoMiddleware: true,
            HumanInTheLoopMiddleware: true,
            SubAgentMiddleware: true,
            BuiltInSubagents: true,
            McpMiddleware: true,
            ToolSearch: true,
            GoalMiddleware: true,
            WorkflowMiddleware: false,
          }),
        },
      },
    });
  });

  test("默认宿主只把 LANGFUSE_ 变量交给 WASM，并上报启动诊断", async () => {
    const originalTracing = process.env.LANGFUSE_TRACING;
    const originalSecret = process.env.PERI_SERVER_SECRET;
    process.env.LANGFUSE_TRACING = "true";
    process.env.PERI_SERVER_SECRET = "must-not-leak";
    try {
      const logs: Array<{ label: string; error: unknown; context?: Record<string, unknown> }> = [];
      const { manager, database, factory } = assembled({ useDefaultRuntime: true, logs });
      database.entries.set("session-1", entry("session-1"));
      await activate(manager, "session-1");

      const runtime = factory.contexts[0]!.runtime;
      expect(runtime.kind).toBe("wasm");
      if (runtime.kind !== "wasm") throw new Error("expected wasm runtime");
      expect(runtime.moduleUrl).toBe(config.wasmModuleUrl);
      expect(runtime.env).toEqual({ LANGFUSE_TRACING: "true" });
      expect(runtime.cleanupTimeoutMs).toBe(DEFAULT_WASM_RUNTIME_CLEANUP_TIMEOUT_MS);
      expect(runtime.ports?.scheduler).toBeDefined();

      const diagnostic = new Error("runtime startup failed");
      runtime.onDiagnostic?.("startup", diagnostic);
      expect(logs).toEqual([{
        label: "Peri server runtime diagnostic",
        error: diagnostic,
        context: { phase: "runtime-start", kind: "startup" },
      }]);
    } finally {
      if (originalTracing === undefined) delete process.env.LANGFUSE_TRACING;
      else process.env.LANGFUSE_TRACING = originalTracing;
      if (originalSecret === undefined) delete process.env.PERI_SERVER_SECRET;
      else process.env.PERI_SERVER_SECRET = originalSecret;
    }
  });

  test("生命周期预算默认与 WASM 清理预算对齐，并允许覆盖", async () => {
    const defaults = assembled();
    defaults.database.entries.set("session-1", entry("session-1"));
    await activate(defaults.manager, "session-1");
    expect(defaults.factory.contexts[0]?.lifecycle).toMatchObject({
      closeDrainTimeoutMs: 3000,
      startupSettleTimeoutMs: 5000,
      activationCloseTimeoutMs: DEFAULT_WASM_RUNTIME_CLEANUP_TIMEOUT_MS,
    });

    const overridden = assembled({ lifecycle: { activationCloseTimeoutMs: 1234 } });
    overridden.database.entries.set("session-1", entry("session-1"));
    await activate(overridden.manager, "session-1");
    expect(overridden.factory.contexts[0]?.lifecycle).toMatchObject({
      activationCloseTimeoutMs: 1234,
      shutdownChatTimeoutMs: DEFAULT_WASM_RUNTIME_CLEANUP_TIMEOUT_MS + 10_000,
    });
  });
});
