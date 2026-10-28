import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "bun:test";
import {
  ChatCloseIncompleteError,
  ChatViewRegistry,
  type ChatAgentConfig,
  type ChatBindingTarget,
  type ChatCreateOptions,
  type ChatFactObserver,
  type ChatFactSource,
  type ChatManagerContract,
  type ChatServiceContract,
  type ChatSessionDirectoryEntry,
  type ChatYjsChannel,
  type WorkspaceRef,
  WorkspaceMismatchError,
} from "@peri-code/sdk";
import { ChatRegistry } from "./registry";
import type { SandboxLease, SandboxPoolPort, SandboxPoolSnapshot } from "./sandbox";

const TARGET: ChatBindingTarget = {
  chatId: "chat-1",
  sessionId: "session-1",
  bindingRevision: 1,
};

/** 服务唯一工作区；与会话 cwd 的 `/workspace` 根一致。 */
const WORKSPACE: WorkspaceRef = {
  id: "default",
  hostPath: "/workspace",
  runtimePath: "/workspace",
};

/** 进程级 agent 模板：注册表原样交给 SDK，不在本地合并。 */
const AGENT: ChatAgentConfig = {
  settings: { config: { active_alias: "sonnet" } },
};

const SANDBOX_ID = "sbx_0123456789abcdef0123456789abcdef";
const SANDBOX_ROOT = "/sandboxes";
const DIRECTORY: ChatSessionDirectoryEntry = {
  sessionId: "session-1",
  title: null,
  cwd: "/workspace",
  messageCount: 1,
  createdAt: "2026-10-20T12:00:00Z",
  updatedAt: "2026-10-20T13:00:00Z",
};

class FakeSandboxPool implements SandboxPoolPort {
  readonly acquired: Array<string | undefined> = [];
  readonly released: string[] = [];
  readonly leases: SandboxLease[] = [];
  closedAll = 0;

  constructor(private readonly workspaceOverride?: string) {}

  async acquire(sandboxId?: string): Promise<SandboxLease> {
    this.acquired.push(sandboxId);
    const id = sandboxId ?? SANDBOX_ID;
    const workspace = this.workspaceOverride ?? `/sandboxes/${id}/workspace`;
    const lease: SandboxLease = {
      sandboxId: id,
      workspace: { id, hostPath: workspace, runtimePath: workspace },
      mcp: {
        name: "workspace",
        config: {
          type: "http",
          url: `http://127.0.0.1/${id}`,
          headers: { authorization: "Bearer sandbox-token" },
        },
      },
      release: async () => { this.released.push(id); },
    };
    this.leases.push(lease);
    return lease;
  }

  async closeAll(): Promise<void> {
    this.closedAll += 1;
  }

  snapshot(): SandboxPoolSnapshot {
    return { active: [], starting: 0 };
  }
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

/** 只实现注册表会触达的成员；其余动作在 fake 中显式失败。 */
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
  readonly createOptions: ChatCreateOptions[] = [];
  readonly listResults = new Map<string, ChatSessionDirectoryEntry[]>();
  closeError: unknown;
  shutdownError: unknown;
  private created = 0;

  constructor(readonly events: string[] = []) {}

  createChat(options: ChatCreateOptions): ChatServiceContract {
    this.createOptions.push(options);
    this.events.push(`port.createChat:${options.sessionPath ?? ""}`);
    const chat = stubChat(`chat-${++this.created}`);
    this.chats.set(chat.id, chat);
    return chat;
  }

  getChat(id: string): ChatServiceContract | undefined {
    return this.chats.get(id);
  }

  findChatBySession(): ChatServiceContract | undefined {
    return undefined;
  }

  async listSessions(params?: { path?: string }): Promise<ChatSessionDirectoryEntry[]> {
    return this.listResults.get(params?.path ?? "") ?? [];
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

/** 在 Manager 与注册表的 View 表中登记同一个句柄，并挂一条可观察的同步连接。 */
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

function hosted(options: {
  events?: string[];
  chats?: FakeChatManager;
  sessions?: { getSession(id: string): Promise<ChatSessionDirectoryEntry | null> };
  sandbox?: SandboxPoolPort;
  sandboxesRoot?: string;
} = {}) {
  const events = options.events ?? [];
  const chats = options.chats ?? new FakeChatManager(events);
  const registry = new ChatRegistry(chats, WORKSPACE, AGENT, {
    session: {
      instructions: "test instructions",
      mcpServers: { remote: { type: "http", url: "https://mcp.example.com" } },
    },
    sessions: options.sessions ?? { getSession: async () => null },
    ...(options.sandbox
      ? {
        sandbox: options.sandbox,
        sandboxesRoot: options.sandboxesRoot ?? SANDBOX_ROOT,
      }
      : {}),
  });
  return { registry, chats, views: registry.views, events };
}

describe("ChatRegistry", () => {
  test("createChat 注入 workspace、agent，并把绝对 path 转为相对 sessionPath", async () => {
    const events: string[] = [];
    const chats = new FakeChatManager(events);
    const { registry } = hosted({ events, chats });

    expect((await registry.createChat({ path: "/workspace/" })).id).toBe("chat-1");
    expect((await registry.createChat({ path: "/workspace/project" })).id).toBe("chat-2");
    expect((await registry.createChat()).id).toBe("chat-3");

    expect(events).toEqual([
      "port.createChat:",
      "port.createChat:project",
      "port.createChat:",
    ]);
    expect(chats.createOptions).toHaveLength(3);
    // 同一个模板对象跨 Chat 复用；SDK 负责逐 Chat 深拷贝与注入 store。
    for (const options of chats.createOptions) {
      expect(options.workspace).toBe(WORKSPACE);
      expect(options.agent).toBe(AGENT);
    }
    expect(chats.createOptions.map((options) => options.sessionPath))
      .toEqual(["", "project", ""]);
  });

  test("createChat 拒绝工作区外的绝对 path", async () => {
    const { registry } = hosted();

    await expect(registry.createChat({ path: "/workspace-other" }))
      .rejects.toBeInstanceOf(WorkspaceMismatchError);
    await expect(registry.createChat({ path: "/tmp/project" }))
      .rejects.toBeInstanceOf(WorkspaceMismatchError);
  });

  test("监控读面：活动句柄计数与 sandbox 快照委托", async () => {
    const sandbox = new FakeSandboxPool();
    const { registry } = hosted({ sandbox });
    expect(registry.activeChatCount()).toBe(0);

    const first = await registry.createChat();
    const second = await registry.createChat();
    expect(registry.activeChatCount()).toBe(2);
    expect(registry.sandboxSnapshot()).toEqual({ active: [], starting: 0 });

    await registry.closeChat(first.id);
    expect(registry.activeChatCount()).toBe(1);

    await registry.shutdown();
    expect(registry.activeChatCount()).toBe(0);
    expect(second.id).toBe("chat-2");
  });

  test("未启用 sandbox 时监控快照为 undefined", () => {
    const { registry } = hosted();
    expect(registry.sandboxSnapshot()).toBeUndefined();
  });

  test("新会话启动 sandbox，并把 sandbox MCP 合并进共享 Session 配置", async () => {
    const sandbox = new FakeSandboxPool();
    const { registry, chats } = hosted({ sandbox });

    const chat = await registry.createChat();

    expect(sandbox.acquired).toEqual([undefined]);
    expect(chat.id).toBe("chat-1");
    const options = chats.createOptions[0]!;
    expect(options.workspace).toBe(sandbox.leases[0]!.workspace);
    expect(options.sessionPath).toBeUndefined();
    expect(options.session).toEqual({
      instructions: "test instructions",
      mcpServers: {
        remote: { type: "http", url: "https://mcp.example.com" },
        workspace: sandbox.leases[0]!.mcp.config,
      },
    });
  });

  test("历史会话按 cwd 复用所属 sandbox，并保留原子目录 sessionPath", async () => {
    const sandbox = new FakeSandboxPool();
    const cwd = `${SANDBOX_ROOT}/${SANDBOX_ID}/workspace/project`;
    const { registry, chats } = hosted({
      sandbox,
      sessions: {
        getSession: async (id) => id === "session-1" ? { ...DIRECTORY, cwd } : null,
      },
    });

    const chat = await registry.createChat({ sessionId: "session-1" });

    expect(sandbox.acquired).toEqual([SANDBOX_ID]);
    expect(chat.id).toBe("chat-1");
    expect(chats.createOptions[0]?.workspace).toBe(sandbox.leases[0]!.workspace);
    expect(chats.createOptions[0]?.sessionPath).toBe("project");
  });

  test("旧 /workspace 会话不启动 sandbox，沿用固定工作区", async () => {
    const sandbox = new FakeSandboxPool();
    const { registry, chats } = hosted({
      sandbox,
      sessions: { getSession: async () => ({ ...DIRECTORY, cwd: "/workspace" }) },
    });

    await registry.createChat({ sessionId: "session-1" });

    expect(sandbox.acquired).toEqual([]);
    expect(chats.createOptions[0]?.workspace).toBe(WORKSPACE);
    expect(chats.createOptions[0]?.sessionPath).toBe("");
  });

  test("sandbox 工作区外的会话 cwd 以 workspace_mismatch 拒绝并释放租约", async () => {
    const sandbox = new FakeSandboxPool(`${SANDBOX_ROOT}/${SANDBOX_ID}/other`);
    const { registry } = hosted({
      sandbox,
      sessions: { getSession: async () => ({ ...DIRECTORY, cwd: `${SANDBOX_ROOT}/${SANDBOX_ID}/workspace/project` }) },
    });

    await expect(registry.createChat({ sessionId: "session-1" }))
      .rejects.toBeInstanceOf(WorkspaceMismatchError);
    expect(sandbox.released).toEqual([SANDBOX_ID]);
  });

  test("关闭 Chat 释放 sandbox 租约；shutdown 关闭全部实例", async () => {
    const sandbox = new FakeSandboxPool();
    const { registry } = hosted({ sandbox });
    const first = await registry.createChat();
    const second = await registry.createChat();

    await registry.closeChat(first.id);
    expect(sandbox.released).toEqual([sandbox.leases[0]!.sandboxId]);

    await registry.shutdown();
    expect(sandbox.closedAll).toBe(1);
    expect(second.id).toBe("chat-2");
  });

  test("listSessions 合并旧工作区与每个 sandbox 工作区", async () => {
    const root = mkdtempSync(join(tmpdir(), "peri-cf-sandboxes-"));
    try {
      mkdirSync(join(root, SANDBOX_ID, "workspace"), { recursive: true });
      const chats = new FakeChatManager();
      chats.listResults.set("", [{
        ...DIRECTORY,
        sessionId: "legacy",
        updatedAt: "2026-10-20T12:00:00Z",
      }]);
      chats.listResults.set(
        join(root, SANDBOX_ID, "workspace"),
        [{
          ...DIRECTORY,
          sessionId: "sandboxed",
          cwd: join(root, SANDBOX_ID, "workspace"),
          updatedAt: "2026-10-20T14:00:00Z",
        }],
      );
      const { registry } = hosted({
        chats,
        sandbox: new FakeSandboxPool(),
        sandboxesRoot: root,
      });

      const entries = await registry.listSessions();

      expect(entries.map((entry) => entry.sessionId)).toEqual(["sandboxed", "legacy"]);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
  test("closeChat 先关闭端口句柄，再释放 View", async () => {
    const events: string[] = [];
    const { registry, chats, views } = hosted({ events });
    const chat = attachChat(chats, views, "chat-1", { events });

    await registry.closeChat(chat.id);

    expect(events).toEqual([
      `port.closeChat:${chat.id}`,
      "view.close",
    ]);
    expect(registry.chats.getChat(chat.id)).toBeUndefined();
    expect(views.get(chat.id)).toBeUndefined();
    expect(registry.hasPendingClose(chat.id)).toBe(false);
    expect(chats.chats.has(chat.id)).toBe(false);
  });

  test("端口关闭失败时不释放 View，也不包装错误", async () => {
    const events: string[] = [];
    const chats = new FakeChatManager(events);
    const failure = new Error("port close failed");
    chats.closeError = failure;
    const { registry, views } = hosted({ events, chats });
    const chat = attachChat(chats, views, "chat-1", { events });

    const error = await registry.closeChat(chat.id).catch((reason: unknown) => reason);
    expect(error).toBe(failure);
    expect(events).toEqual([`port.closeChat:${chat.id}`]);
    expect(views.get(chat.id)).toBeDefined();
    expect(registry.chats.getChat(chat.id)).toBe(chat);
  });

  test("View 释放失败包装为 ChatCloseIncompleteError，重试后清除残留", async () => {
    const events: string[] = [];
    const { registry, chats, views } = hosted({ events });
    const chat = attachChat(chats, views, "chat-1", { events, closeFailures: 1 });

    await expect(registry.closeChat(chat.id)).rejects.toBeInstanceOf(
      ChatCloseIncompleteError,
    );
    expect(registry.chats.getChat(chat.id)).toBeUndefined();
    expect(views.get(chat.id)).toBeDefined();
    expect(registry.hasPendingClose(chat.id)).toBe(true);

    await registry.closeChat(chat.id);
    expect(views.get(chat.id)).toBeUndefined();
    expect(registry.hasPendingClose(chat.id)).toBe(false);
    expect(events).toEqual([
      `port.closeChat:${chat.id}`,
      "view.close",
      `port.closeChat:${chat.id}`,
      "view.close",
    ]);
  });

  test("hasPendingClose 只在句柄消失而 View 仍存在时为真", async () => {
    const events: string[] = [];
    const { registry, chats, views } = hosted({ events });
    const chat = attachChat(chats, views, "chat-1", { events, closeFailures: 1 });

    expect(registry.hasPendingClose(chat.id)).toBe(false);
    expect(registry.hasPendingClose("missing")).toBe(false);

    await expect(registry.closeChat(chat.id)).rejects.toBeInstanceOf(
      ChatCloseIncompleteError,
    );
    expect(registry.hasPendingClose(chat.id)).toBe(true);
    await registry.closeChat(chat.id);
    expect(registry.hasPendingClose(chat.id)).toBe(false);
  });

  test("shutdown 先释放全部 View，再关闭端口", async () => {
    const events: string[] = [];
    const { registry, chats, views } = hosted({ events });
    const first = attachChat(chats, views, "chat-1", { events });
    const second = attachChat(chats, views, "chat-2", { events });

    await registry.shutdown();

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
    const { registry, views } = hosted({ events, chats });
    const chat = attachChat(chats, views, "chat-1", { events, closeFailures: 1 });

    const failure = await registry.shutdown().catch((error: unknown) => error);
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
    const { registry, chats, views } = hosted({ events });
    attachChat(chats, views, "chat-1", { events, closeFailures: 1 });

    const failure = await registry.shutdown().catch((error: unknown) => error);
    expect(failure).toBeInstanceOf(AggregateError);
    expect((failure as AggregateError).errors).toHaveLength(1);
    expect(events).toEqual(["view.close", "port.shutdown"]);
  });

  test("shutdown 并发调用完成且 View 被释放", async () => {
    const events: string[] = [];
    const { registry, chats, views } = hosted({ events });
    const chat = attachChat(chats, views, "chat-1", { events });

    await Promise.all([registry.shutdown(), registry.shutdown()]);

    expect(views.get(chat.id)).toBeUndefined();
  });
});
