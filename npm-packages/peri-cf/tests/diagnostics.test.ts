import { afterEach, describe, expect, spyOn, test } from "bun:test";
import { createApi } from "../worker/api/router";
import { diagnosticMessage, errorResponse, logError, publicError } from "../worker/api/http";
import { HTTPException } from "hono/http-exception";
import type { ChatRepository, Env } from "../worker/types";

let errorLog: ReturnType<typeof spyOn<typeof console, "error">> | undefined;

afterEach(() => {
  errorLog?.mockRestore();
  errorLog = undefined;
});

const env: Env = {
  APP_AUTH_TOKEN: "synthetic-app-token",
  MODEL_API_KEY: "synthetic-model-token",
  PERI_STORAGE_TOKEN: "synthetic-store-token",
  PERI_STORAGE_URL: "libsql://synthetic.invalid",
  MODEL_BASE_URL: "https://model.invalid/v1",
  CHAT_SESSIONS: {
    idFromName(name) { return name; },
    get() { throw new Error("Unexpected DO access"); },
  },
};

describe("HTTP runtime diagnostics", () => {
  test("preserves synthetic credentials, URLs and complete causes in responses and original logs", async () => {
    const cause = new Error("synthetic-store-token libsql://synthetic.invalid?password=synthetic-password /private/project");
    const failure = new Error(`synthetic-model-token https://model.invalid/v1?token=synthetic-query ${"detail".repeat(150)}`, { cause });
    errorLog = spyOn(console, "error").mockImplementation(() => {});
    const repository: ChatRepository = {
      async list() { throw failure; },
      async create() { throw new Error("Unexpected create"); },
      async get() { throw new Error("Unexpected get"); },
    };
    const response = await createApi(() => repository).fetch(new Request("https://app.test/api/chats", {
      headers: { Authorization: "Bearer synthetic-app-token" },
    }), env);
    expect(response.status).toBe(500);
    expect(response.headers.get("Cache-Control")).toBe("no-store");
    expect(await response.json()).toEqual({ error: `${failure.message}\nCaused by: ${cause.message}` });
    expect(errorLog).toHaveBeenCalledWith("Peri API request failed", { method: "GET", path: "/api/chats" }, failure);
  });

  test("retains nested and aggregate causes without looping on circular causes", async () => {
    const circular = new Error("circular synthetic-key");
    circular.cause = circular;
    const failure = new AggregateError([
      new Error("synthetic-password", { cause: { message: "https://synthetic.invalid?token=key" } }),
      circular,
    ], "multiple failures", { cause: new Error("outer context") });
    const response = errorResponse(failure);
    expect(response.status).toBe(500);
    expect(await response.json()).toEqual({ error: "multiple failures\nCaused by: outer context\nAggregated error: synthetic-password\nCaused by: https://synthetic.invalid?token=key\nAggregated error: circular synthetic-key\nCaused by: [Circular error cause]" });
  });

  test.each([
    ["synthetic-string-token", "synthetic-string-token"],
    [{ message: "synthetic-object-token", cause: "nested token" }, "synthetic-object-token\nCaused by: nested token"],
    [{ code: "STORE_UNAVAILABLE", path: "/private/project" }, '{"code":"STORE_UNAVAILABLE","path":"/private/project"}'],
    [42, "42"],
    [null, "null"],
  ])("preserves non-Error rejection diagnostics %j", async (failure, expected) => {
    expect(diagnosticMessage(failure)).toBe(expected);
    const response = errorResponse(failure);
    expect(response.headers.get("Cache-Control")).toBe("no-store");
    expect(await response.json()).toEqual({ error: expected });
  });

  test("bounds presentation and removes control characters without changing original diagnostics", async () => {
    const failure = new Error(`synthetic-model-token\u001b\u0000 ${"x".repeat(700)}`, { cause: new Error("late synthetic-store-token") });
    const presentation = publicError(failure, env);
    expect(presentation).toHaveLength(512);
    expect(presentation.startsWith("synthetic-model-token ")).toBe(true);
    expect(presentation).not.toContain("\u001b");
    expect(presentation).not.toContain("\u0000");
    errorLog = spyOn(console, "error").mockImplementation(() => {});
    logError("Agent instance failed", failure, { instanceId: "synthetic-instance" });
    expect(errorLog).toHaveBeenCalledWith("Agent instance failed", { instanceId: "synthetic-instance" }, failure);
    expect(await errorResponse(failure).json()).toEqual({ error: `${failure.message}\nCaused by: late synthetic-store-token` });
  });

  test("retains HTTP status and diagnostic cause independently", async () => {
    const failure = new HTTPException(503, {
      message: "Store unavailable",
      cause: new Error("libsql://synthetic.invalid?authToken=synthetic-key"),
    });
    const response = errorResponse(failure);
    expect(response.status).toBe(503);
    expect(response.headers.get("Cache-Control")).toBe("no-store");
    expect(await response.json()).toEqual({ error: "Store unavailable\nCaused by: libsql://synthetic.invalid?authToken=synthetic-key" });
  });

  test("preserves explicit authentication responses with no-store", async () => {
    const failure = new HTTPException(401, { res: Response.json({ error: "Unauthorized" }, {
      status: 401, headers: { "WWW-Authenticate": "Bearer", "Cache-Control": "public" },
    }) });
    const response = errorResponse(failure);
    expect(response.status).toBe(401);
    expect(response.headers.get("Cache-Control")).toBe("no-store");
    expect(response.headers.get("WWW-Authenticate")).toBe("Bearer");
    expect(await response.json()).toEqual({ error: "Unauthorized" });
  });

  test("logs original sync upgrade failures without dumping request credentials or environment", async () => {
    const failure = new Error("WASM connection failed", { cause: new Error("https://synthetic.invalid?key=synthetic-value") });
    const upgradeEnv: Env = { ...env, CHAT_SESSIONS: {
      idFromName(name) { return name; },
      get() { return { async fetch() { throw failure; } }; },
    } };
    errorLog = spyOn(console, "error").mockImplementation(() => {});
    const app = createApi(() => { throw new Error("Unexpected repository access"); });
    const path = "/api/chats/00000000-0000-4000-8000-000000000001/sync";
    const response = await app.fetch(new Request(`https://app.test${path}`, {
      headers: { Upgrade: "websocket", Authorization: "Bearer synthetic-app-token" },
    }), upgradeEnv);
    expect(response.status).toBe(500);
    expect(response.headers.get("Cache-Control")).toBe("no-store");
    expect(await response.json()).toEqual({ error: "WASM connection failed\nCaused by: https://synthetic.invalid?key=synthetic-value" });
    expect(errorLog).toHaveBeenCalledTimes(1);
    expect(errorLog).toHaveBeenCalledWith("Peri sync upgrade failed", { method: "GET", path }, failure);
  });

  test("does not log deliberate credential-bearing authentication inputs", async () => {
    errorLog = spyOn(console, "error").mockImplementation(() => {});
    const app = createApi(() => { throw new Error("Unexpected repository access"); });
    for (const authorization of [undefined, "Bearer synthetic-wrong-token", "Bearer synthetic-app-token extra"]) {
      const response = await app.fetch(new Request("https://app.test/api/chats", {
        headers: authorization ? { Authorization: authorization } : {},
      }), env);
      expect(response.status).toBe(401);
      expect(response.headers.get("Cache-Control")).toBe("no-store");
      expect(response.headers.get("WWW-Authenticate")).toBe("Bearer");
      expect(await response.json()).toEqual({ error: "Unauthorized" });
    }
    expect(errorLog).not.toHaveBeenCalled();
  });
});
