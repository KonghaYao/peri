import { describe, expect, test } from "bun:test";
import { PeriWasmHostLease, type WasmAcpTransport } from "@peri-code/sdk/wasm";
import {
  type AcquireHost, type AgentEnvironment, type Chat, type ChatSessionStore, type ChatSummary,
  type RuntimeSummary, type StartHost, type SyncState,
} from "../../core/agent";
import { PeriInstance } from "./peri-instance";
import { SESSION_CWD } from "../config";
import { ChatService, mergeChatSummaries, type ChatServiceDeps } from "./service";

function summary(id: string, overrides: Partial<ChatSummary> = {}): ChatSummary {
  return {
    id, title: id, updatedAt: "2026-10-20T00:00:00Z",
    messageCount: 3, running: false, pendingInteractionCount: 0,
    lastTurnStatus: null, lastCompletedAt: null,
    ...overrides,
  };
}

function chat(id: string, title = id): Chat {
  return { id, title, updatedAt: "2026-10-20T00:00:00Z" };
}

function stateFor(id: string): SyncState {
  return {
    chat: chat(id), running: false, executionBlocked: false, readiness: "ready",
    revision: 0,
    entries: [], toolCalls: [], turns: [], activeTurn: { turnId: null, status: null },
    session: {}, tasks: [], plans: { current: [], byTurn: {} },
    interactions: [], pendingInteractions: [], inputQueue: null,
  };
}

class FakeStore implements ChatSessionStore {
  listCalls = 0;
  readonly gets: string[] = [];
  constructor(private readonly chats: readonly ChatSummary[] = []) {}

  async list(): Promise<ChatSummary[]> {
    this.listCalls += 1;
    return [...this.chats];
  }

  async get(id: string): Promise<Chat | null> {
    this.gets.push(id);
    return this.chats.find((candidate) => candidate.id === id) ?? null;
  }
}

const environment: AgentEnvironment = {
  cwd: SESSION_CWD,
  instructions: "test instructions",
  initialize: async () => {},
  openMcp: () => ({
    servers: [],
    handleMessage: () => { throw new Error("host is not started in tests"); },
    close: async () => {},
  }),
  logError: () => {},
};

const unusedHost: AcquireHost = async () => { throw new Error("host is not started in tests"); };

/** 把测试用裸启动器包成租约入口。 */
function acquireFrom(startHost: StartHost): AcquireHost {
  return (options) => PeriWasmHostLease.start(startHost, options);
}

/** 只关心驻留事实与释放：读面固定，绕过宿主复原。 */
class FakeInstance extends PeriInstance {
  busy = false;
  disposeCalls = 0;

  constructor(readonly id: string, store: Pick<ChatSessionStore, "get">) {
    super(id, store, environment, unusedHost);
  }

  override get recyclable(): boolean { return !this.busy; }

  override summary(): RuntimeSummary {
    return {
      running: this.busy, pendingInteractionCount: 0, lastTurnStatus: null, lastCompletedAt: null,
    };
  }

  override read(): Promise<SyncState> { return Promise.resolve(stateFor(this.id)); }

  override async dispose(): Promise<void> { this.disposeCalls += 1; }
}

class TestChatService extends ChatService {
  readonly created = new Map<string, FakeInstance>();

  constructor(private readonly testStore: Pick<ChatSessionStore, "get">, deps: ChatServiceDeps) {
    super(deps);
  }

  protected override createInstance(chatId: string): PeriInstance {
    const instance = new FakeInstance(chatId, this.testStore);
    this.created.set(chatId, instance);
    return instance;
  }
}

interface HarnessOptions {
  store?: FakeStore;
  startHost?: StartHost;
  createEnvironment?: (chatId?: string) => AgentEnvironment;
}

function harness(options: HarnessOptions = {}): {
  service: TestChatService; store: FakeStore; environmentIds: string[];
} {
  const store = options.store ?? new FakeStore();
  const environmentIds: string[] = [];
  const service = new TestChatService(store, {
    store,
    acquireHost: options.startHost ? acquireFrom(options.startHost) : unusedHost,
    createEnvironment: options.createEnvironment ?? ((chatId) => {
      if (chatId !== undefined) environmentIds.push(chatId);
      return { ...environment };
    }),
  });
  return { service, store, environmentIds };
}

/** 满足 `SessionCreator` 的最小传输：`session/new` 返回固定身份，rename 回显标题。 */
function sessionStartHost(sessionId: string, calls: string[] = []): StartHost {
  return async () => ({
    setRequestHandler() {},
    request: async (method: string, params: unknown) => {
      calls.push(method);
      return method === "session/new"
        ? { sessionId }
        : { sessionId, title: (params as { title: string }).title };
    },
    close: async () => {},
  }) as unknown as WasmAcpTransport;
}

describe("service list summary merge", () => {
  const stored = [summary("a"), summary("b")];

  test("resident instance facts overwrite the stored defaults", () => {
    const summaries = new Map<string, RuntimeSummary>([
      ["a", {
        running: true, pendingInteractionCount: 2,
        lastTurnStatus: "streaming", lastCompletedAt: "2026-10-20T00:00:10Z",
      }],
    ]);
    expect(mergeChatSummaries(stored, summaries)).toEqual([
      {
        ...stored[0]!, running: true, pendingInteractionCount: 2,
        lastTurnStatus: "streaming", lastCompletedAt: "2026-10-20T00:00:10Z",
      },
      stored[1]!,
    ]);
  });

  test("running flips back to false when the instance reports the turn finished", () => {
    const running = new Map<string, RuntimeSummary>([
      ["a", {
        running: true, pendingInteractionCount: 0,
        lastTurnStatus: "streaming", lastCompletedAt: null,
      }],
    ]);
    const finished = new Map<string, RuntimeSummary>([
      ["a", {
        running: false, pendingInteractionCount: 0,
        lastTurnStatus: "completed", lastCompletedAt: "2026-10-20T00:05:00Z",
      }],
    ]);
    expect(mergeChatSummaries(stored, running)[0]).toMatchObject({ running: true });
    expect(mergeChatSummaries(stored, finished)[0]).toMatchObject({
      running: false, lastTurnStatus: "completed", lastCompletedAt: "2026-10-20T00:05:00Z",
    });
  });

  test("non-resident chats do not inherit facts from another instance", () => {
    const merged = mergeChatSummaries(stored, new Map());
    expect(merged).toEqual(stored);
  });
});

describe("ChatService resident instances", () => {
  test("list 合并驻留实例事实并读取注入的 store", async () => {
    const { service, store } = harness({ store: new FakeStore([summary("a"), summary("b")]) });
    await service.read("a");
    service.created.get("a")!.busy = true;

    const listed = await service.list();

    expect(store.listCalls).toBe(1);
    expect(listed[0]).toMatchObject({ id: "a", running: true });
    expect(listed[1]).toEqual(summary("b"));
  });

  test("同一会话复用同一实例，并 touch 到 LRU 尾部", async () => {
    const { service } = harness();
    await service.read("a");
    const first = service.created.get("a")!;
    await service.read("a");

    expect(service.created.size).toBe(1);
    expect(service.created.get("a")).toBe(first);
  });

  test("超过上限时淘汰最久未用的空闲实例", async () => {
    const { service } = harness();
    for (let index = 0; index < 8; index += 1) await service.read(`chat-${index}`);
    const evicted = service.created.get("chat-0")!;

    await service.read("chat-8");

    expect(evicted.disposeCalls).toBe(1);
    expect(service.created.get("chat-1")?.disposeCalls).toBe(0);
    expect(service.created.get("chat-8")?.disposeCalls).toBe(0);
  });

  test("touch 过的实例不会先被淘汰", async () => {
    const { service } = harness();
    for (let index = 0; index < 8; index += 1) await service.read(`chat-${index}`);
    await service.read("chat-0");

    await service.read("chat-8");

    expect(service.created.get("chat-0")?.disposeCalls).toBe(0);
    expect(service.created.get("chat-1")?.disposeCalls).toBe(1);
  });

  test("全部忙碌时允许超限，不淘汰新实例", async () => {
    const { service } = harness();
    for (let index = 0; index < 8; index += 1) {
      await service.read(`chat-${index}`);
      service.created.get(`chat-${index}`)!.busy = true;
    }

    await service.read("chat-8");

    expect(service.created.size).toBe(9);
    expect([...service.created.values()].every((instance) => instance.disposeCalls === 0)).toBe(true);
  });

  test("每个实例向注入的环境工厂请求自己的 chatId", async () => {
    const environmentIds: string[] = [];
    const service = new ChatService({
      store: new FakeStore(),
      acquireHost: unusedHost,
      createEnvironment: (chatId) => {
        if (chatId !== undefined) environmentIds.push(chatId);
        return { ...environment };
      },
    });

    await Promise.allSettled([service.read("a"), service.read("b")]);

    expect(environmentIds).toEqual(["a", "b"]);
  });

  test("shutdown 释放全部实例且并发调用只释放一次", async () => {
    const { service } = harness();
    await service.read("a");
    await service.read("b");

    await Promise.all([service.shutdown(), service.shutdown()]);
    expect(service.created.get("a")?.disposeCalls).toBe(1);
    expect(service.created.get("b")?.disposeCalls).toBe(1);

    await service.shutdown();
    expect(service.created.get("a")?.disposeCalls).toBe(1);
  });
});

describe("ChatService.create", () => {
  test("通过 SessionCreator 建会话，再用 store.get 取身份", async () => {
    const createdId = "11111111-1111-4111-8111-111111111111";
    const calls: string[] = [];
    const environmentIds: (string | undefined)[] = [];
    const store = new FakeStore([summary(createdId, { title: "新会话" })]);
    const { service } = harness({
      store,
      startHost: sessionStartHost(createdId, calls),
      createEnvironment: (chatId) => {
        environmentIds.push(chatId);
        return { ...environment };
      },
    });

    const created = await service.create("新会话");

    expect(created).toMatchObject(chat(createdId, "新会话"));
    expect(environmentIds).toEqual([undefined]);
    expect(store.gets).toEqual([createdId]);
    expect(calls).toEqual(["session/new", "session/rename"]);
  });

  test("store 看不到新建会话时抛错", async () => {
    const { service } = harness({
      startHost: sessionStartHost("22222222-2222-4222-8222-222222222222"),
    });

    await expect(service.create("新会话")).rejects.toThrow("not visible in store");
  });
});
