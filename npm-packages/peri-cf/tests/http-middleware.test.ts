import { describe, expect, test } from "bun:test";
import { createApi } from "../worker/api/router";
import { chatSchema } from "../shared/chat";
import type { ChatRepository, Env } from "../worker/types";

function fixture() {
  const calls: string[] = [];
  const repository: ChatRepository = {
    async list() { calls.push("list"); return []; },
    async get() { throw new Error("No DO query is expected"); },
    async create(title) {
      calls.push(title);
      return { id: "00000000-0000-4000-8000-000000000001", title, updatedAt: "2026-10-06T00:00:00.000Z" };
    },
  };
  const env: Env = { APP_AUTH_TOKEN: "trusted-token", CHAT_SESSIONS: {
    idFromName() { throw new Error("No DO access is expected"); },
    get() { throw new Error("No DO access is expected"); },
  } };
  const app = createApi(() => repository);
  const send = (body: BodyInit | null, headers: Record<string, string> = {}) => app.fetch(new Request("https://app.test/api/chats", {
    method: "POST", body, headers: { Authorization: "Bearer trusted-token", "Content-Type": "application/json", ...headers },
  }), env);
  return { calls, env, app, send };
}

describe("Hono authentication, bounded JSON and Zod boundary", () => {
  test.each(["Basic trusted-token", "Bearer", "Bearer trusted-token extra", "Bearer trusted-token!", `Bearer ${"a".repeat(4097)}`])(
    "rejects invalid bearer header without running a repository: %s", async (header) => {
      const app = fixture();
      const response = await app.send("{}", { Authorization: header });
      expect(response.status).toBe(401);
      expect(response.headers.get("WWW-Authenticate")).toBe("Bearer");
      expect(response.headers.get("Cache-Control")).toContain("no-store");
      expect(await response.json() as { error: string }).toEqual({ error: "Unauthorized" });
      expect(app.calls).toEqual([]);
    },
  );

  test("does not reinterpret downstream validation errors as authentication errors", async () => {
    const app = fixture();
    const malformed = await app.send("{");
    expect(malformed.status).toBe(400);
    expect(malformed.headers.get("Cache-Control")).toBe("no-store");
    const unsupportedType = await app.send("{}", { "Content-Type": "text/plain" });
    expect(unsupportedType.status).toBe(415);
    expect(unsupportedType.headers.get("Cache-Control")).toBe("no-store");
    expect(app.calls).toEqual([]);
  });

  test("preserves method and missing-route semantics with no-store", async () => {
    const app = fixture();
    for (const [path, method, expectedStatus, expectedMessage] of [
      ["/api/chats", "DELETE", 405, "Method not allowed"],
      ["/api/chats/not-a-session", "GET", 404, "Not found"],
      ["/api/unknown", "GET", 404, undefined],
    ] as const) {
      const response = await app.app.fetch(new Request(`https://app.test${path}`, {
        method, headers: { Authorization: "Bearer trusted-token" },
      }), app.env);
      expect(response.status).toBe(expectedStatus);
      expect(response.headers.get("Cache-Control")).toBe("no-store");
      if (expectedMessage) expect(await response.json()).toEqual({ error: expectedMessage });
    }
    expect(app.calls).toEqual([]);
  });

  test("keeps failed WebSocket handshake responses uncached", async () => {
    const app = fixture();
    const path = "https://app.test/api/chats/00000000-0000-4000-8000-000000000001/sync";
    const missingUpgrade = await app.app.fetch(new Request(path), app.env);
    expect(missingUpgrade.status).toBe(426);
    expect(missingUpgrade.headers.get("Cache-Control")).toBe("no-store");
    const urlCredentials = await app.app.fetch(new Request(`${path}?token=synthetic-token`, {
      headers: { Upgrade: "websocket" },
    }), app.env);
    expect(urlCredentials.status).toBe(400);
    expect(urlCredentials.headers.get("Cache-Control")).toBe("no-store");
    expect(app.calls).toEqual([]);
  });

  test.each([undefined, "1", "999999", "not-a-number"])("counts actual bytes instead of trusting Content-Length %s", async (length) => {
    const app = fixture();
    const body = JSON.stringify({ title: "Valid title", extra: "x".repeat(65_536) });
    const response = await app.send(body, length === undefined ? {} : { "Content-Length": length });
    expect(response.status).toBe(413);
    expect(await response.json() as { error: string }).toEqual({ error: "Request body is too large" });
    expect(app.calls).toEqual([]);
  });

  test("limits UTF-8 bytes, not character count", async () => {
    const app = fixture();
    const body = JSON.stringify({ title: "Valid title", extra: "界".repeat(22_000) });
    expect(body.length).toBeLessThan(65_536);
    expect((await app.send(body)).status).toBe(413);
    expect(app.calls).toEqual([]);
  });

  test.each(["null", "[]", "42", '"text"', '{"title":null}', '{"title":" "}', JSON.stringify({ title: "x".repeat(201) })])(
    "rejects invalid schema input without creating a session: %s", async (body) => {
      const app = fixture();
      expect((await app.send(body)).status).toBe(400);
      expect(app.calls).toEqual([]);
    },
  );

  test("uses schema defaults and transformations for the actual repository command", async () => {
    const app = fixture();
    expect((await app.send("{}")).status).toBe(201);
    const response = await app.send(JSON.stringify({ title: "  Persisted title  ", ignored: true }));
    expect(response.status).toBe(201);
    expect(chatSchema.parse(await response.json()).title).toBe("Persisted title");
    expect(app.calls).toEqual(["New chat", "Persisted title"]);
  });
});
