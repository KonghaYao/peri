import { afterEach, describe, expect, spyOn, test } from "bun:test";
import { ChatApi } from "../web/api/client";

afterEach(() => {
  fetchSpy?.mockRestore();
  fetchSpy = undefined;
});

let fetchSpy: ReturnType<typeof spyOn<typeof globalThis, "fetch">> | undefined;

function respond(response: Response) {
  fetchSpy = spyOn(globalThis, "fetch").mockResolvedValue(response);
  return fetchSpy;
}

const chat = { id: "chat-1", title: "First", updatedAt: "2026-10-06T00:00:00.000Z" };

describe("ChatApi request boundary", () => {
  test("sends Bearer auth and no-store without putting the token in a URL", async () => {
    const fetchMock = respond(Response.json({ chats: [chat] }));
    const signal = new AbortController().signal;
    expect(await new ChatApi("trusted-token").list(signal)).toEqual([chat]);
    const [path, options] = fetchMock.mock.calls[0];
    expect(path).toBe("/api/chats");
    expect(options?.cache).toBe("no-store");
    expect(options?.signal).toBe(signal);
    expect(new Headers(options?.headers).get("Authorization")).toBe("Bearer trusted-token");
  });

  test("creates from the backend bare Chat response", async () => {
    const fetchMock = respond(Response.json(chat, { status: 201 }));
    expect(await new ChatApi("token").create()).toEqual(chat);
    const [path, options] = fetchMock.mock.calls[0];
    expect(path).toBe("/api/chats");
    expect(options?.method).toBe("POST");
    expect(JSON.parse(options?.body as string)).toEqual({});
    expect(new Headers(options?.headers).get("Content-Type")).toBe("application/json");
  });

  test.each([401, 403])("reports HTTP %s as an auth failure without reflecting server secrets", async (status) => {
    respond(Response.json({ error: "secret-model-token" }, { status }));
    await expect(new ChatApi("wrong-token").list()).rejects.toThrow("身份验证失败");
  });

  test("reports busy HTTP 409 without retrying the command", async () => {
    respond(Response.json({ error: "busy" }, { status: 409 }));
    await expect(new ChatApi("token").send(chat.id, "hello", new AbortController().signal)).rejects.toThrow("409");
  });

  test("rejects malformed list payloads", async () => {
    respond(Response.json({ chats: [{ ...chat, updatedAt: null }] }));
    await expect(new ChatApi("token").list()).rejects.toThrow("会话列表格式不正确");
  });

  test.each([[null], [[]], [{ ...chat, title: null }]])("rejects invalid create payloads via the shared schema", async (payload) => {
    respond(Response.json(payload));
    await expect(new ChatApi("token").create()).rejects.toThrow("新建会话响应格式不正确");
  });

  test.each([{ status: "unknown" }, { error: 42 }, { role: "system" }])("rejects invalid detail messages via the shared schema", async (invalid) => {
    respond(Response.json({ chat, messages: [{ id: "message", role: "assistant", content: "partial",
      createdAt: chat.updatedAt, ...invalid }] }));
    await expect(new ChatApi("token").detail(chat.id)).rejects.toThrow("会话历史格式不正确");
  });

  test("rejects detail for a different chat", async () => {
    respond(Response.json({ chat, messages: [] }));
    await expect(new ChatApi("token").detail("other-chat")).rejects.toThrow("不匹配");
  });

  test("URL-encodes a chat identity and posts cancellation with auth", async () => {
    const fetchMock = respond(Response.json({ cancelled: true }));
    await new ChatApi("token").cancel("chat/with spaces");
    const [path, options] = fetchMock.mock.calls[0];
    expect(path).toBe("/api/chats/chat%2Fwith%20spaces/cancel");
    expect(options?.method).toBe("POST");
    expect(new Headers(options?.headers).get("Authorization")).toBe("Bearer token");
  });
});

describe("ChatApi command acceptance", () => {
  test("posts JSON once and accepts only 202 accepted without waiting for generation", async () => {
    const fetchMock = respond(Response.json({ accepted: true }, { status: 202 }));
    const signal = new AbortController().signal;
    await new ChatApi("token").send("chat/with spaces", "hello", signal);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [path, options] = fetchMock.mock.calls[0];
    expect(path).toBe("/api/chats/chat%2Fwith%20spaces/messages");
    expect(options?.method).toBe("POST");
    expect(options?.signal).toBe(signal);
    expect(JSON.parse(options?.body as string)).toEqual({ content: "hello" });
    expect(new Headers(options?.headers).get("Accept")).not.toBe("text/event-stream");
    expect(new Headers(options?.headers).get("Authorization")).toBe("Bearer token");
  });

  test.each([[{}], [{ accepted: false }], [{ accepted: "true" }], [null], [[]]])("rejects invalid acceptance %j without replay", async payload => {
    const fetchMock = respond(Response.json(payload, { status: 202 }));
    await expect(new ChatApi("token").send(chat.id, "hello")).rejects.toThrow("接受响应格式不正确");
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  test.each([200, 201, 204])("rejects non-acceptance status %s", async status => {
    respond(status === 204 ? new Response(null, { status }) : Response.json({ accepted: true }, { status }));
    await expect(new ChatApi("token").send(chat.id, "hello")).rejects.toThrow("未确认接受");
  });

  test("rejects event-stream instead of consuming deltas", async () => {
    respond(new Response('event: done\ndata: {}\n\n', { status: 202, headers: { 'Content-Type': 'text/event-stream' } }));
    await expect(new ChatApi("token").send(chat.id, "hello")).rejects.toThrow("未确认接受");
  });

  test("does not replay an uncertain network failure", async () => {
    fetchSpy = spyOn(globalThis, "fetch").mockRejectedValue(new Error("network lost"));
    await expect(new ChatApi("token").send(chat.id, "hello")).rejects.toThrow("network lost");
    expect(fetchSpy).toHaveBeenCalledTimes(1);
  });

  test("cancel waits for the server response and does not retry", async () => {
    let resolve!: (response: Response) => void;
    fetchSpy = spyOn(globalThis, "fetch").mockReturnValue(new Promise<Response>(accept => { resolve = accept; }));
    let stopped = false;
    const stop = new ChatApi("token").cancel(chat.id).then(() => { stopped = true; });
    await Promise.resolve();
    expect(stopped).toBe(false);
    resolve(Response.json({ cancelled: true }));
    await stop;
    expect(stopped).toBe(true);
    expect(fetchSpy).toHaveBeenCalledTimes(1);
  });
});
