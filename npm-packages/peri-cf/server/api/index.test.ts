import { describe, expect, test } from "bun:test";
import { ChatCommandError, ChatNotFoundError, ChatProjection, type Chat } from "../../core/agent";
import type { SyncState } from "../../core/shared";
import { createApi } from ".";
import type { ChatManagerPort, ChatSubscription } from "../agent";

const chat: Chat = {
  id: "11111111-1111-4111-8111-111111111111",
  title: "测试会话",
  updatedAt: "2026-10-20T00:00:00Z",
};

const chatSummary = {
  ...chat, messageCount: 3, running: false, pendingInteractionCount: 0,
  lastTurnStatus: null, lastCompletedAt: null,
};

const state: SyncState = {
  chat, running: false, executionBlocked: false, readiness: "ready",
  revision: 0,
  entries: [], toolCalls: [], turns: [], activeTurn: { turnId: null, status: null },
  session: {}, tasks: [], plans: { current: [], byTurn: {} },
  interactions: [], pendingInteractions: [], inputQueue: null,
};

const info = {
  model: { provider: "openai" as const, id: "gpt-test", baseUrl: "https://api.example.com/v1" },
  storage: { url: "https://store.example.com" },
  mcp: [],
  workspace: "/workspace",
  sessionCwd: "/workspace/",
};

function apiWith(overrides: Partial<ChatManagerPort> = {}) {
  const manager: ChatManagerPort = {
    list: async () => [chatSummary],
    create: async (title) => ({ ...chat, title }),
    read: async () => state,
    run: async () => undefined,
    watch: async () => ({ initial: state, close() {} }),
    shutdown: async () => {},
    ...overrides,
  };
  return createApi({ manager, info });
}

describe("server api", () => {
  test("列出与创建会话", async () => {
    const app = apiWith();
    const list = await app.request("/api/chats");
    expect(await list.json()).toEqual({ chats: [chatSummary] });

    const created = await app.request("/api/chats", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ title: "新会话" }),
    });
    expect(created.status).toBe(201);
    expect(await created.json()).toEqual({ chat: { ...chat, title: "新会话" } });
  });

  test("会话不存在返回 404", async () => {
    const app = apiWith({ read: async () => { throw new ChatNotFoundError(chat.id); } });
    const response = await app.request(`/api/chats/${chat.id}`);
    expect(response.status).toBe(404);
  });

  test("非法命令不触达执行层", async () => {
    let called = false;
    const app = apiWith({ run: async () => { called = true; return undefined; } });
    const response = await app.request(`/api/chats/${chat.id}/commands`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ op: "prompt", content: "   " }),
    });
    expect(response.status).toBe(400);
    expect(called).toBe(false);
    expect(await response.json()).toEqual({
      ok: false, code: "invalid_command", message: "命令格式无效", disposition: "rejected",
    });
  });

  test("命令拒绝保留稳定失败码与 rejected 处置", async () => {
    const app = apiWith({ run: async () => { throw new ChatCommandError("busy", "Chat is busy", "rejected"); } });
    const response = await app.request(`/api/chats/${chat.id}/commands`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ op: "prompt", content: "继续" }),
    });
    expect(response.status).toBe(409);
    expect(await response.json()).toEqual({
      ok: false, code: "busy", message: "Chat is busy", disposition: "rejected",
    });
  });

  test("会话缺失的命令映射为 404 not_found", async () => {
    const app = apiWith({ run: async () => { throw new ChatCommandError("not_found", "Chat not found", "rejected"); } });
    const response = await app.request(`/api/chats/${chat.id}/commands`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ op: "cancel" }),
    });
    expect(response.status).toBe(404);
    expect(await response.json()).toEqual({
      ok: false, code: "not_found", message: "Chat not found", disposition: "rejected",
    });
  });

  test("未分类异常返回 500 internal_error 与 unknown 处置", async () => {
    const app = apiWith({ run: async () => { throw new Error("boom"); } });
    const response = await app.request(`/api/chats/${chat.id}/commands`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ op: "cancel" }),
    });
    expect(response.status).toBe(500);
    expect(await response.json()).toEqual({
      ok: false, code: "internal_error", message: "命令执行失败；诊断已记录", disposition: "unknown",
    });
  });

  test("SSE 首帧带完整投影", async () => {
    const app = apiWith();
    const response = await app.request(`/api/chats/${chat.id}/events`);
    expect(response.status).toBe(200);
    expect(response.headers.get("content-type")).toContain("text/event-stream");
    const reader = response.body!.getReader();
    const chunk = await Promise.race([
      reader.read(),
      new Promise<never>((_, reject) => setTimeout(() => reject(new Error("SSE timeout")), 2_000)),
    ]);
    const text = new TextDecoder().decode(chunk.value);
    expect(text).toContain("event: state");
    expect(text).toContain(chat.id);
    expect(text).toContain('"revision":0');
    await reader.cancel();
  });

  test("SSE 首帧 revision 来自真实投影并在命令后增长", async () => {
    const projection = new ChatProjection();
    projection.setRunState({ chat, running: false, executionBlocked: false });
    const initial = projection.state();
    expect(initial.revision).toBeGreaterThan(0);

    const manager: ChatManagerPort = {
      list: async () => [],
      create: async () => chat,
      read: async () => projection.state(),
      watch: async (_chatId, onState) => {
        const unsubscribe = projection.subscribe(() => onState(projection.state()));
        return { initial: projection.state(), close: unsubscribe };
      },
      run: async (_chatId, command) => {
        if (command.op === "cancel")
          projection.setRunState({ chat, running: true, executionBlocked: false });
        return undefined;
      },
      shutdown: async () => {},
    };
    const app = createApi({ manager, info });
    const response = await app.request(`/api/chats/${chat.id}/events`);
    const reader = response.body!.getReader();
    const decoder = new TextDecoder();

    const readEvent = async (): Promise<string> => {
      const chunk = await Promise.race([
        reader.read(),
        new Promise<never>((_, reject) => setTimeout(() => reject(new Error("SSE timeout")), 2_000)),
      ]);
      return decoder.decode(chunk.value);
    };

    try {
      const first = await readEvent();
      expect(first).toContain("event: state");
      const firstRevision = Number(/"revision":(\d+)/.exec(first)?.[1]);
      expect(firstRevision).toBeGreaterThan(0);

      const command = await app.request(`/api/chats/${chat.id}/commands`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ op: "cancel" }),
      });
      expect(command.status).toBe(200);

      const updated = await readEvent();
      expect(updated).toContain("event: state");
      const updatedRevision = Number(/"revision":(\d+)/.exec(updated)?.[1]);
      expect(updatedRevision).toBeGreaterThan(firstRevision);
      expect(updated).toContain('"running":true');
    } finally {
      await reader.cancel();
    }
  });

  test("watch 建立期间断开：关闭订阅且不写首帧", async () => {
    let resolveWatch!: (subscription: ChatSubscription) => void;
    let closeCalls = 0;
    let notifyWatching!: () => void;
    const watching = new Promise<void>((resolve) => { notifyWatching = resolve; });
    const app = apiWith({
      watch: () => new Promise<ChatSubscription>((resolve) => {
        resolveWatch = resolve;
        notifyWatching();
      }),
    });

    const controller = new AbortController();
    const response = await app.request(`/api/chats/${chat.id}/events`, { signal: controller.signal });
    await watching;
    controller.abort();
    resolveWatch({ initial: state, close: () => { closeCalls += 1; } });

    const text = await Promise.race([
      response.text(),
      new Promise<never>((_, reject) => setTimeout(() => reject(new Error("SSE timeout")), 2_000)),
    ]);
    expect(closeCalls).toBe(1);
    expect(text).not.toContain("event: state");
  });
});
