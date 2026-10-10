import { afterEach, describe, expect, spyOn, test } from "bun:test";
import { ChatApi, errorText } from "../web/api/client";

afterEach(() => {
  fetchSpy?.mockRestore();
  fetchSpy = undefined;
});

let fetchSpy: ReturnType<typeof spyOn<typeof globalThis, "fetch">> | undefined;

function state(overrides: Record<string, unknown> = {}) {
  return { chat, running: false, executionBlocked: false, readiness: "ready", entries: [],
    toolCalls: [], turns: [], activeTurn: { turnId: null, status: null }, session: {}, tasks: [],
    plans: { current: [], byTurn: {} }, interactions: [], pendingInteractions: [], inputQueue: null, ...overrides };
}

function respond(response: Response) {
  fetchSpy = spyOn(globalThis, "fetch").mockResolvedValue(response);
  return fetchSpy;
}

const chat = { id: "chat-1", title: "First", updatedAt: "2026-10-06T00:00:00.000Z" };

describe("ChatApi read-only boundary", () => {
  test("sends Bearer auth and no-store without putting the token in a URL", async () => {
    const fetchMock = respond(Response.json({ chats: [chat] }));
    const signal = new AbortController().signal;
    expect(await new ChatApi("trusted-token").list(signal)).toEqual([chat]);
    const [path, options] = fetchMock.mock.calls[0];
    expect(path).toBe("/api/v2/chats");
    expect(options?.cache).toBe("no-store");
    expect(options?.signal).toBe(signal);
    expect(new Headers(options?.headers).get("Authorization")).toBe("Bearer trusted-token");
  });

  test("creates from the backend bare Chat response", async () => {
    const fetchMock = respond(Response.json(chat, { status: 201 }));
    expect(await new ChatApi("token").create()).toEqual(chat);
    const [path, options] = fetchMock.mock.calls[0];
    expect(path).toBe("/api/v2/chats");
    expect(options?.method).toBe("POST");
    expect(JSON.parse(options?.body as string)).toEqual({});
    expect(new Headers(options?.headers).get("Content-Type")).toBe("application/json");
  });

  test("URL-encodes the chat identity when reading a detail page", async () => {
    const fetchMock = respond(Response.json(state({ chat: { ...chat, id: "chat/with spaces" } })));
    await new ChatApi("token").detail("chat/with spaces");
    const [path] = fetchMock.mock.calls[0];
    expect(path).toBe("/api/v2/chats/chat%2Fwith%20spaces");
  });

  test.each([401, 403])("reports HTTP %s with a stable authentication message", async (status) => {
    respond(Response.json({ error: "secret-model-token" }, { status }));
    await expect(new ChatApi("wrong-token").list()).rejects.toThrow("身份验证失败");
  });

  test("rejects malformed list payloads", async () => {
    respond(Response.json({ chats: [{ ...chat, updatedAt: null }] }));
    await expect(new ChatApi("token").list()).rejects.toThrow("会话列表格式不正确");
  });

  test.each([[null], [[]], [{ ...chat, title: null }]])("rejects invalid create payloads via the shared schema", async (payload) => {
    respond(Response.json(payload));
    await expect(new ChatApi("token").create()).rejects.toThrow("新建会话响应格式不正确");
  });

  test.each([{ status: 42 }, { error: "boom" }, { kind: "note" }, { blocks: "text" }])(
    "rejects invalid detail entries via the shared schema %j", async (invalid) => {
      respond(Response.json(state({ entries: [{ entryId: "turn:1:assistant", id: "turn:1:assistant", kind: "message",
        turnId: "turn:1", role: "assistant", status: "completed", createdAt: chat.updatedAt, createdAtSource: "observed",
        completedAt: null, error: null, blocks: [{ blockId: "text:0", type: "text", text: "partial" }], ...invalid }] })));
      await expect(new ChatApi("token").detail(chat.id)).rejects.toThrow("会话历史格式不正确");
    });

  test("rejects detail for a different chat", async () => {
    respond(Response.json(state()));
    await expect(new ChatApi("token").detail("other-chat")).rejects.toThrow("不匹配");
  });
});

describe("removed HTTP command surface", () => {
  test("the client exposes no HTTP command methods: commands live on the sync WebSocket", () => {
    // 命令面已删除：旧页面若仍调用这些方法，TypeScript 与运行期都会失败。
    const api = new ChatApi("token") as unknown as Record<string, unknown>;
    for (const method of ["send", "cancel", "submitInteractionAnswer", "payload", "readPayload"])
      expect(api[method]).toBeUndefined();
  });

  test("a 426 command response to a stale page becomes an actionable refresh error", async () => {
    respond(Response.json({ error: "Chat commands must use the session WebSocket", upgrade: true },
      { status: 426, headers: { "Cache-Control": "no-store" } }));
    await expect(new ChatApi("token").list()).rejects.toThrow("客户端版本过旧，请刷新页面后重试。");
  });
});

describe("error text normalisation", () => {
  test("accepts strings, Error messages and unknown values without leaking control characters", () => {
    expect(errorText("plain")).toBe("plain");
    expect(errorText(new Error("a\u001bb\u0000"))).toBe("ab");
    expect(errorText(42)).toBe("请求失败，请稍后重试。");
    expect(errorText(new Error("x".repeat(600)))).toHaveLength(512);
  });
});
