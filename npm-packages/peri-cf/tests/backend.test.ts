import { describe, expect, test } from "bun:test";
import { fetchApi } from "../worker/api/router";
import { ChatRegistry } from "../worker/chat/registry";
import { replayChunk, runtimeFixture } from "./helpers/runtime";
import type { AgentResources } from "../shared/resources";
import type { Chat, ChatRepository, Env } from "../worker/types";

/**
 * 无 DO 的 HTTP 面：会话读面走进程内运行期（冷 isolate 先经重放复原），
 * 命令面（发送/取消/交互/载荷）一律 426 —— 它们必须走接受连接的那条 WS。
 */

function emptyRepository(): ChatRepository {
  return {
    async list() { return []; },
    async get() { return null; },
    async create() { throw new Error("Unexpected create"); },
  };
}

/** 只用于路由级用例：任何会话访问都不应发生。 */
function forbidden(): { runtimes: Pick<ChatRegistry, "runtimeFor" | "peek"> } {
  return { runtimes: {
    runtimeFor() { throw new Error("Unexpected session runtime access"); },
    peek() { throw new Error("Unexpected session runtime access"); },
  } };
}

const startTransport = async () => { throw new Error("Unexpected ACP host startup"); };

function request(path: string, options: { method?: string; body?: unknown; token?: string | null;
  headers?: Record<string, string>; env?: Env; repository?: ChatRepository } = {}): Promise<Response> {
  const headers = new Headers(options.headers);
  const token = options.token === undefined ? "trusted-app-token" : options.token;
  if (token !== null) headers.set("Authorization", `Bearer ${token}`);
  if (options.body !== undefined) headers.set("Content-Type", "application/json");
  return fetchApi(new Request(`https://app.invalid${path}`, {
    method: options.method ?? "GET", headers,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  }), options.env ?? { APP_AUTH_TOKEN: "trusted-app-token" }, options.repository ?? emptyRepository(),
  { runtimes: forbidden().runtimes, startTransport });
}

describe("authenticated routing without any durable session object", () => {
  test("rejects invalid or missing bearer identity before touching any runtime", async () => {
    for (const token of [null, "wrong-token", "trusted-app-token-extra"]) {
      const response = await request("/api/v2/chats", { token });
      expect(response.status).toBe(401);
      expect(response.headers.get("WWW-Authenticate")).toBe("Bearer");
      expect(response.headers.get("Cache-Control")).toContain("no-store");
      expect(await response.json()).toEqual({ error: "Unauthorized" });
    }
  });

  test("fails closed when the app token is not configured", async () => {
    const response = await request("/api/v2/chats", { env: {}, token: "any-token" });
    expect(response.status).toBe(503);
    expect(await response.json()).toEqual({ error: "Authentication is not configured" });
  });

  test.each([
    ["/api/v2/chats/not-a-uuid", "GET", 404],
    ["/api/v2/chats", "DELETE", 405],
    ["/api/instances", "POST", 405],
  ] as const)("returns %s %s route error %s", async (path, method, status) => {
    expect((await request(path, { method })).status).toBe(status);
  });

  test("unknown chats answer 404 before any host startup", async () => {
    const app = runtimeFixture();
    const response = await app.request("/api/v2/chats/00000000-0000-4000-8000-000000000001");
    expect(response.status).toBe(404);
    expect(await response.json()).toEqual({ error: "Chat not found" });
    expect(app.transports).toEqual([]);
  });

  test("requires JSON content type and rejects malformed or oversized JSON", async () => {
    for (const [body, contentType, status] of [
      ["{}", "text/plain", 415], ["{", "application/json", 400],
      ["[]", "application/json", 400], [JSON.stringify({ title: "x".repeat(65_536) }), "application/json", 413],
    ] as const) {
      const response = await fetchApi(new Request("https://app.invalid/api/v2/chats", {
        method: "POST", body, headers: { Authorization: "Bearer trusted-app-token", "Content-Type": contentType },
      }), { APP_AUTH_TOKEN: "trusted-app-token" }, emptyRepository(), { runtimes: forbidden().runtimes, startTransport });
      expect(response.status).toBe(status);
    }
  });

  test("lists and creates actual session identities from the store", async () => {
    const chats = new Map<string, Chat>();
    const calls: string[] = [];
    const repository: ChatRepository = {
      async list() { calls.push("list"); return structuredClone([...chats.values()]); },
      async get(id) { calls.push("get"); return structuredClone(chats.get(id) ?? null); },
      async create(title) {
        calls.push(`create:${title}`);
        const chat = { id: "00000000-0000-4000-8000-000000000001", title, updatedAt: "2026-10-06T00:00:00.000Z" };
        chats.set(chat.id, chat);
        return structuredClone(chat);
      },
    };
    const created = await request("/api/v2/chats", { method: "POST", body: { title: "  中文标题  " }, repository });
    expect(created.status).toBe(201);
    expect(((await created.json()) as Chat).title).toBe("中文标题");
    const listed = await request("/api/v2/chats", { repository });
    expect(listed.headers.get("Cache-Control")).toBe("no-store");
    expect(await listed.json()).toEqual({ chats: [{ id: "00000000-0000-4000-8000-000000000001",
      title: "中文标题", updatedAt: "2026-10-06T00:00:00.000Z" }] });
    expect(calls).toEqual(["create:中文标题", "list"]);
  });

  test("store failures surface without leaking credentials or starting a host", async () => {
    const repository: ChatRepository = {
      async list() { throw new Error("private-storage-token"); },
      async get() { throw new Error("Unexpected get"); },
      async create() { throw new Error("Unexpected create"); },
    };
    const response = await request("/api/v2/chats", { repository });
    expect(response.status).toBe(500);
    expect(await response.json()).toEqual({ error: "private-storage-token" });
  });

  test("repository failures during hydration surface as 500 and start no host", async () => {
    const app = runtimeFixture();
    const chat = await app.create("Hydration failure");
    app.repository.get = async () => { throw new Error("private-storage-token"); };
    const response = await app.request(`/api/v2/chats/${chat.id}`);
    expect(response.status).toBe(500);
    expect(await response.json()).toEqual({ error: "private-storage-token" });
    expect(app.transports).toEqual([]);
  });

  test("detail refreshes authoritative metadata and replays history on a cold isolate", async () => {
    const history = [
      replayChunk("00000000-0000-4000-8000-000000000001", "user-1", "历史问题", { role: "user", replay: true }),
      replayChunk("00000000-0000-4000-8000-000000000001", "assistant-1", "历史回答", { role: "assistant", replay: true }),
    ];
    const app = runtimeFixture({ replay: history });
    const chat = await app.create("Detail");
    const first = await app.detail(chat.id);
    expect(first).toMatchObject({ chat, readiness: "ready", running: false, executionBlocked: false });
    expect(first.entries).toHaveLength(2);
    // 元数据以 Store 为准：重命名后 refetch 立即生效。
    const renamed = { ...chat, title: "Renamed in Rust Store", updatedAt: "2026-10-07T00:00:00.000Z" };
    app.chats.set(chat.id, renamed);
    expect(await app.detail(chat.id)).toMatchObject({ chat: renamed, entries: first.entries.map((entry) => ({ id: entry.id })) });
  });

  test("resources peek without waking a session and report the live instance when resident", async () => {
    const app = runtimeFixture({ resources: true, replay: [
      replayChunk("00000000-0000-4000-8000-000000000001", "user-1", "历史", { role: "user", replay: true }),
    ] });
    const chat = await app.create("Resources");
    // 未驻留本 isolate：peek 形态，不触发重放也不启动宿主。
    expect(await app.resources(chat.id)).toEqual({ sessionId: chat.id, running: false,
      executionBlocked: false, instance: null });
    expect(app.transports).toEqual([]);
    await app.detail(chat.id);
    const resident = await app.resources(chat.id);
    // 复原阶段的宿主已关闭：观测如实报告 closed + 最后一次观测到的内存。
    expect(resident.instance).toMatchObject({ phase: "closed", memory: { observation: "last-observed" } });
    expect(resident).toMatchObject({ sessionId: chat.id, running: false, executionBlocked: false });
    expect(resident.instance?.instanceId).toBeString();
    expect(app.transports).toHaveLength(1);
  });

  test("resources require authentication and validate the session identity", async () => {
    expect((await request("/api/v2/chats/00000000-0000-4000-8000-000000000001/resources", { token: null })).status).toBe(401);
    expect((await request("/api/v2/chats/invalid/resources")).status).toBe(404);
    expect((await request("/api/v2/chats/00000000-0000-4000-8000-000000000001/resources", { method: "POST" })).status).toBe(405);
  });

  test("resource observation never queries the store again", async () => {
    const app = runtimeFixture({ resources: true });
    const chat = await app.create("Observation");
    await app.detail(chat.id);
    const queries = app.repositoryCalls.length;
    app.repository.get = async () => { throw new Error("Store unavailable"); };
    const observed: AgentResources = await app.resources(chat.id);
    expect(observed.instance?.instanceId).toBeString();
    expect(app.repositoryCalls.length).toBe(queries);
  });
});

describe("removed HTTP command surface", () => {
  const chatId = "00000000-0000-4000-8000-000000000001";
  test.each([
    ["/api/v2/chats/" + chatId + "/messages", "POST"],
    ["/api/v2/chats/" + chatId + "/cancel", "POST"],
    ["/api/v2/chats/" + chatId + "/interactions/interaction%3A1", "POST"],
    ["/api/v2/chats/" + chatId + "/payloads/gen%3A1", "GET"],
  ] as const)("%s answers 426 with the upgrade marker for %s", async (path, method) => {
    const response = await request(path, { method, body: method === "POST" ? {} : undefined });
    expect(response.status).toBe(426);
    expect(response.headers.get("Cache-Control")).toBe("no-store");
    const body = await response.json() as { error: string; upgrade: boolean };
    expect(body.upgrade).toBe(true);
    expect(body.error).toContain("WebSocket");
  });

  test("rejects mutation methods on read-only routes", async () => {
    expect((await request(`/api/v2/chats/${chatId}`, { method: "DELETE" })).status).toBe(405);
  });

  test("legacy chat paths keep answering 426 for every method", async () => {
    for (const path of ["/api/chats", `/api/chats/${chatId}`, `/api/chats/${chatId}/messages`,
      `/api/chats/${chatId}/cancel`, `/api/chats/${chatId}/resources`, `/api/chats/${chatId}/sync`]) {
      const response = await request(path, { method: "POST" });
      expect(response.status).toBe(426);
      expect(await response.json()).toEqual({ error: "Client upgrade required", upgrade: true });
    }
  });

  test("the sync endpoint rejects non-upgrade requests and URL credentials", async () => {
    const path = `https://app.invalid/api/v2/chats/${chatId}/sync`;
    const env: Env = { APP_AUTH_TOKEN: "trusted-app-token" };
    const missingUpgrade = await fetchApi(new Request(path), env, emptyRepository(), { startTransport });
    expect(missingUpgrade.status).toBe(426);
    expect(missingUpgrade.headers.get("Cache-Control")).toBe("no-store");
    const urlCredentials = await fetchApi(new Request(`${path}?token=synthetic-token`, {
      headers: { Upgrade: "websocket" },
    }), env, emptyRepository(), { startTransport });
    expect(urlCredentials.status).toBe(400);
    expect(urlCredentials.headers.get("Cache-Control")).toBe("no-store");
    const wrongMethod = await fetchApi(new Request(path, { method: "DELETE" }), env, emptyRepository(), { startTransport });
    expect(wrongMethod.status).toBe(405);
  });
});
