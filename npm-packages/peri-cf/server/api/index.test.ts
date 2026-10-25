import { describe, expect, test } from "bun:test";
import {
  ChatActionError,
  ChatCloseIncompleteError,
  PayloadReadError,
  SessionCreationError,
  type ChatBindingTarget,
  type ChatService,
  type ChatSessionDirectoryEntry,
  type ChatUserAction,
  type ToolPayloadReadRef,
  type ToolPayloadResponse,
} from "@peri-code/sdk";
import type { publicServerInfo } from "../config";
import { createApi, type ApiChatManager } from ".";

const info = {
  model: { provider: "openai" as const, id: "gpt-test", baseUrl: "https://api.example.com/v1" },
  storage: { url: "https://store.example.com" },
  mcp: [],
  workspace: "/workspace",
  sessionCwd: "/workspace/",
} satisfies ReturnType<typeof publicServerInfo>;

const entry: ChatSessionDirectoryEntry = {
  sessionId: "session-1",
  title: null,
  cwd: "/workspace/",
  messageCount: 3,
  createdAt: "2026-10-20T12:00:00Z",
  updatedAt: "2026-10-20T13:00:00Z",
};

interface StubChat extends ChatService {
  readonly calls: ChatUserAction[];
  readonly closeCalls: { value: number };
  failNext?: unknown;
  payloadError?: unknown;
}

function stubChat(
  id: string,
  initial?: { sessionId: string; bindingRevision: number },
): StubChat {
  const calls: ChatUserAction[] = [];
  const closeCalls = { value: 0 };
  const state: {
    sessionId: string | null;
    bindingRevision: number;
    binding: ChatBindingTarget | null;
    failNext?: unknown;
    payloadError?: unknown;
  } = {
    sessionId: initial?.sessionId ?? null,
    bindingRevision: initial?.bindingRevision ?? 0,
    binding: initial
      ? { chatId: id, sessionId: initial.sessionId, bindingRevision: initial.bindingRevision }
      : null,
  };

  const chat = {
    id,
    get sessionId() { return state.sessionId; },
    get bindingRevision() { return state.bindingRevision; },
    get binding() { return state.binding; },
    calls,
    closeCalls,
    get failNext() { return state.failNext; },
    set failNext(value: unknown) { state.failNext = value; },
    get payloadError() { return state.payloadError; },
    set payloadError(value: unknown) { state.payloadError = value; },

    execute(action: ChatUserAction) {
      calls.push(action);
      if (state.failNext !== undefined) throw state.failNext;
      const target = (): ChatBindingTarget => {
        if (!state.binding) throw new Error("stub has no binding");
        return state.binding;
      };
      switch (action.type) {
        case "send": {
          const binding = target();
          return Object.assign(Promise.resolve(), {
            requestId: action.requestId,
            inputId: "input-1",
            target: binding,
            accepted: Promise.resolve(),
            isSent: false,
          });
        }
        case "create_new_session":
        case "change_session": {
          const sessionId = action.type === "change_session"
            ? action.payload.sessionId
            : "created-session";
          state.sessionId = sessionId;
          state.bindingRevision += 1;
          state.binding = { chatId: id, sessionId, bindingRevision: state.bindingRevision };
          return Promise.resolve({
            requestId: action.requestId,
            ...state.binding,
          });
        }
        case "cancel":
          return Promise.resolve({ requestId: action.requestId, status: "idle" });
        case "force_send":
        case "take_back":
        case "answer":
          return Promise.resolve({ requestId: action.requestId, status: "accepted" });
        case "load_earlier_history":
          return Promise.resolve({ applied: 1, nextCursor: "next" });
      }
    },

    async readPayload(ref: ToolPayloadReadRef): Promise<ToolPayloadResponse> {
      if (state.payloadError !== undefined) throw state.payloadError;
      return { id: ref.id, bytes: ref.bytes, json: '{"ok":true}' };
    },

    async close(): Promise<void> {
      closeCalls.value += 1;
    },

    subscribeFacts() { return () => {}; },
  };
  return chat as unknown as StubChat;
}

interface ApiFixture {
  app: ReturnType<typeof createApi>;
  chats: Map<string, StubChat>;
  created: StubChat[];
  listCalls: Array<{ path?: string } | undefined>;
  closeErrors: Map<string, unknown>;
}

function apiWith(options: {
  chats?: StubChat[];
  listError?: unknown;
  listResult?: ChatSessionDirectoryEntry[];
} = {}): ApiFixture {
  const chats = new Map((options.chats ?? []).map((chat) => [chat.id, chat]));
  const created: StubChat[] = [];
  const listCalls: Array<{ path?: string } | undefined> = [];
  const closeErrors = new Map<string, unknown>();
  const manager: ApiChatManager = {
    async listSessions(params) {
      listCalls.push(params);
      if (options.listError !== undefined) throw options.listError;
      return options.listResult ?? [entry];
    },
    createChat() {
      const chat = stubChat("chat-new");
      created.push(chat);
      chats.set(chat.id, chat);
      return chat;
    },
    getChat: (id) => chats.get(id),
    findChatBySession: (id) =>
      [...chats.values()].find((chat) => chat.sessionId === id),
    async closeChat(id) {
      const failure = closeErrors.get(id);
      if (failure !== undefined) throw failure;
      const chat = chats.get(id);
      if (chat) await chat.close();
      chats.delete(id);
    },
  };
  const app = createApi({ manager, info, logError: () => {} });
  return { app, chats, created, listCalls, closeErrors };
}

function postAction(
  app: ReturnType<typeof createApi>,
  chatId: string,
  action: Record<string, unknown>,
  target: unknown,
) {
  return app.request(`/api/chats/${chatId}/actions`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ...action, target }),
  });
}

const boundTarget = { sessionId: "session-1", bindingRevision: 1 };

describe("server api", () => {
  test("目录只返回 Store 事实并标记 available", async () => {
    const { app } = apiWith();
    const response = await app.request("/api/chats");
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ chats: [entry], available: true });
    expect(JSON.stringify(await (await app.request("/api/chats")).json()))
      .not.toContain("running");
  });

  test("目录不可达返回 503 与 available:false", async () => {
    const { app } = apiWith({ listError: new Error("store down") });
    const response = await app.request("/api/chats");
    expect(response.status).toBe(503);
    expect(await response.json()).toEqual({
      chats: [],
      available: false,
      error: "Session directory is unavailable",
    });
  });

  test("createChat 只登记句柄，不创建 Session", async () => {
    const { app, created } = apiWith();
    const response = await app.request("/api/chats", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ path: "/workspace/" }),
    });
    expect(response.status).toBe(201);
    expect(await response.json()).toEqual({
      chat: { id: "chat-new", sessionId: null, bindingRevision: 0 },
    });
    expect(created).toHaveLength(1);
    expect(created[0]?.calls).toEqual([]);
  });

  test("非法 JSON 与非法 Action 不触达执行层", async () => {
    const chat = stubChat("chat-1", { sessionId: "session-1", bindingRevision: 1 });
    const { app } = apiWith({ chats: [chat] });

    const broken = await app.request("/api/chats/chat-1/actions", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{",
    });
    expect(broken.status).toBe(400);
    expect((await broken.json()).code).toBe("invalid_action");

    const invalid = await postAction(app, "chat-1", {
      type: "send", requestId: "request-1", payload: { text: "  " },
    }, boundTarget);
    expect(invalid.status).toBe(400);
    expect((await invalid.json()).code).toBe("invalid_action");
    expect(chat.calls).toEqual([]);
  });

  test("未知 Chat 返回 410", async () => {
    const { app } = apiWith();
    const response = await postAction(app, "missing", {
      type: "cancel", requestId: "request-1", payload: {},
    }, { sessionId: null, bindingRevision: 0 });
    expect(response.status).toBe(410);
    expect(await response.json()).toEqual({
      ok: false,
      code: "chat_not_found",
      message: "Chat not found",
      disposition: "rejected",
      chatId: "missing",
    });
  });

  test("目标绑定不符返回 409 且不执行", async () => {
    const chat = stubChat("chat-1", { sessionId: "session-1", bindingRevision: 1 });
    const { app } = apiWith({ chats: [chat] });
    const response = await postAction(app, "chat-1", {
      type: "send", requestId: "request-1", payload: { text: "继续" },
    }, { sessionId: "session-1", bindingRevision: 2 });
    expect(response.status).toBe(409);
    const body = await response.json();
    expect(body).toMatchObject({
      code: "binding_conflict", disposition: "rejected", requestId: "request-1",
    });
    expect(chat.calls).toEqual([]);
  });

  test("send 返回 202 与 inputId/绑定身份", async () => {
    const chat = stubChat("chat-1", { sessionId: "session-1", bindingRevision: 1 });
    const { app } = apiWith({ chats: [chat] });
    const response = await postAction(app, "chat-1", {
      type: "send", requestId: "request-1", payload: { text: "继续" },
    }, boundTarget);
    expect(response.status).toBe(202);
    expect(await response.json()).toEqual({
      ok: true,
      requestId: "request-1",
      chatId: "chat-1",
      sessionId: "session-1",
      bindingRevision: 1,
      inputId: "input-1",
      accepted: true,
    });
  });

  test("空句柄 create_new_session 使用 null/0 目标并返回新绑定", async () => {
    const chat = stubChat("chat-1");
    const { app } = apiWith({ chats: [chat] });
    const response = await postAction(app, "chat-1", {
      type: "create_new_session", requestId: "request-1", payload: { title: "新对话" },
    }, { sessionId: null, bindingRevision: 0 });
    expect(response.status).toBe(202);
    expect(await response.json()).toEqual({
      ok: true,
      requestId: "request-1",
      chatId: "chat-1",
      sessionId: "created-session",
      bindingRevision: 1,
    });
  });

  test("绑定失败映射为 500 并保留 disposition", async () => {
    const chat = stubChat("chat-1");
    chat.failNext = new ChatActionError(
      "request-1", "binding_failed", "load failed", "rejected",
    );
    const { app } = apiWith({ chats: [chat] });
    const response = await postAction(app, "chat-1", {
      type: "change_session", requestId: "request-1", payload: { sessionId: "session-1" },
    }, { sessionId: null, bindingRevision: 0 });
    expect(response.status).toBe(500);
    expect(await response.json()).toMatchObject({
      ok: false,
      code: "binding_failed",
      disposition: "rejected",
      requestId: "request-1",
      chatId: "chat-1",
    });
  });

  test("session/new 后续失败携带 createdSessionId 与 stage", async () => {
    const chat = stubChat("chat-1");
    chat.failNext = new SessionCreationError(
      "request-1", "created-session", "rename", "rename failed",
    );
    const { app } = apiWith({ chats: [chat] });
    const response = await postAction(app, "chat-1", {
      type: "create_new_session", requestId: "request-1", payload: {},
    }, { sessionId: null, bindingRevision: 0 });
    expect(response.status).toBe(500);
    expect(await response.json()).toMatchObject({
      ok: false,
      code: "session_creation_failed",
      disposition: "unknown",
      createdSessionId: "created-session",
      stage: "rename",
    });
  });

  test("未分类错误返回 500 internal_error 与 unknown", async () => {
    const chat = stubChat("chat-1", { sessionId: "session-1", bindingRevision: 1 });
    chat.failNext = new Error("boom");
    const { app } = apiWith({ chats: [chat] });
    const response = await postAction(app, "chat-1", {
      type: "cancel", requestId: "request-1", payload: {},
    }, boundTarget);
    expect(response.status).toBe(500);
    expect(await response.json()).toMatchObject({
      ok: false, code: "internal_error", disposition: "unknown", chatId: "chat-1",
    });
  });

  test("close 成功 204，未知 Chat 410，未确认 500 + incomplete/errors", async () => {
    const chat = stubChat("chat-1", { sessionId: "session-1", bindingRevision: 1 });
    const fixture = apiWith({ chats: [chat] });

    const missing = await fixture.app.request("/api/chats/missing", { method: "DELETE" });
    expect(missing.status).toBe(410);

    fixture.closeErrors.set("chat-1", new ChatCloseIncompleteError([
      new Error("host close failed"),
    ]));
    const incomplete = await fixture.app.request("/api/chats/chat-1", { method: "DELETE" });
    expect(incomplete.status).toBe(500);
    const body = await incomplete.json();
    expect(body).toMatchObject({
      ok: false,
      code: "close_incomplete",
      disposition: "unknown",
      incomplete: true,
      chatId: "chat-1",
    });
    expect(body.errors[0]).toContain("host close failed");

    fixture.closeErrors.delete("chat-1");
    const closed = await fixture.app.request("/api/chats/chat-1", { method: "DELETE" });
    expect(closed.status).toBe(204);
    expect(chat.closeCalls.value).toBe(1);
  });

  test("payload 按引用读取，不可用映射 404", async () => {
    const chat = stubChat("chat-1", { sessionId: "session-1", bindingRevision: 1 });
    const { app } = apiWith({ chats: [chat] });

    const ok = await app.request("/api/chats/chat-1/payload/tool%2F1?bytes=11");
    expect(ok.status).toBe(200);
    expect(await ok.json()).toEqual({
      ok: true,
      payload: { id: "tool/1", bytes: 11, json: '{"ok":true}' },
    });

    chat.payloadError = new PayloadReadError("unavailable", "no payload");
    const missing = await app.request("/api/chats/chat-1/payload/tool%2F1?bytes=11");
    expect(missing.status).toBe(404);
    expect(await missing.json()).toMatchObject({ ok: false, code: "unavailable" });
  });

  test("payload bytes 非法为 400", async () => {
    const chat = stubChat("chat-1", { sessionId: "session-1", bindingRevision: 1 });
    const { app } = apiWith({ chats: [chat] });
    const response = await app.request("/api/chats/chat-1/payload/tool-1?bytes=-1");
    expect(response.status).toBe(400);
  });
});
