import { Hono } from "hono";
import { HTTPException } from "hono/http-exception";
import { authenticatedApp } from "./app";
import { normalizeChatId, logError, errorResponse } from "./http";
import { jsonRequest } from "./json";
import { createChatBodySchema } from "../../shared/chat";
import { collectChatInstances } from "../instances/collect";
import type { ChatRepository, Env } from "../types";

export function createApi(
  repositoryFactory: (env: Env, waitUntil: (promise: Promise<unknown>) => void) => ChatRepository,
): Hono<{ Bindings: Env }> {
  const app = authenticatedApp();
  app.get("/api/v2/chats", async (context) => {
    const repository = repositoryFactory(context.env, (promise) => context.executionCtx.waitUntil(promise));
    return context.json({ chats: await repository.list() });
  });
  app.post("/api/v2/chats", ...jsonRequest(createChatBodySchema, "title must be a nonempty string of at most 200 characters"), async (context) => {
    const { title } = context.req.valid("json");
    const repository = repositoryFactory(context.env, (promise) => context.executionCtx.waitUntil(promise));
    return context.json(await repository.create(title), 201);
  });
  app.get("/api/instances", async (context) => {
    const repository = repositoryFactory(context.env, (promise) => context.executionCtx.waitUntil(promise));
    const chats = await repository.list();
    const authorization = context.req.header("Authorization") ?? "";
    const instances = await collectChatInstances(chats, async (chatId) => {
      const session = context.env.CHAT_SESSIONS.get(context.env.CHAT_SESSIONS.idFromName(chatId));
      const response = await session.fetch(new Request(new URL(`/api/v2/chats/${encodeURIComponent(chatId)}/resources`, context.req.url), {
        headers: { Authorization: authorization },
      }));
      if (!response.ok) throw new Error(`Chat instance observation failed with HTTP ${response.status}`);
      return response.json();
    });
    return context.json({ generatedAt: new Date().toISOString(), instances });
  });
  app.all("/api/instances", () => { throw new HTTPException(405, { message: "Method not allowed" }); });
  for (const [path, method] of [
    ["/api/v2/chats/:id", "GET"],
    ["/api/v2/chats/:id/messages", "POST"],
    ["/api/v2/chats/:id/cancel", "POST"],
    ["/api/v2/chats/:id/resources", "GET"],
    ["/api/v2/chats/:id/payloads/:refId", "GET"],
    ["/api/v2/chats/:id/interactions/:interactionId", "POST"],
  ] as const) {
    app.on(method, path, async (context) => {
      const id = normalizeChatId(context.req.param("id"));
      return context.env.CHAT_SESSIONS.get(context.env.CHAT_SESSIONS.idFromName(id)).fetch(context.req.raw);
    });
    app.all(path, (context) => {
      normalizeChatId(context.req.param("id"));
      throw new HTTPException(405, { message: "Method not allowed" });
    });
  }
  app.all("/api/v2/chats", () => { throw new HTTPException(405, { message: "Method not allowed" }); });
  // 版本化入口（P3 已批准）：旧 chat 路径在切换后返回明确的升级错误，不再承载新版消费面。
  for (const path of ["/api/chats", "/api/chats/:id", "/api/chats/:id/messages", "/api/chats/:id/cancel",
    "/api/chats/:id/resources", "/api/chats/:id/payloads/:refId", "/api/chats/:id/sync"] as const) {
    app.all(path, () => Response.json({ error: "Client upgrade required", upgrade: true }, { status: 426, headers: { "Cache-Control": "no-store" } }));
  }
  const frontdoor = new Hono<{ Bindings: Env }>();
  frontdoor.get("/api/v2/chats/:id/sync", async (context) => {
    const id = normalizeChatId(context.req.param("id"));
    if (context.req.header("Upgrade")?.toLowerCase() !== "websocket")
      return context.json({ error: "WebSocket upgrade required" }, 426, { "Cache-Control": "no-store" });
    if (new URL(context.req.url).searchParams.has("token"))
      return context.json({ error: "URL credentials are not supported" }, 400, { "Cache-Control": "no-store" });
    return await context.env.CHAT_SESSIONS.get(context.env.CHAT_SESSIONS.idFromName(id)).fetch(context.req.raw);
  });
  frontdoor.onError((error, context) => {
    if (!(error instanceof HTTPException) || error.status >= 500 || error.cause !== undefined)
      logError("Peri sync upgrade failed", error, { method: context.req.method, path: context.req.path });
    return errorResponse(error);
  });
  frontdoor.route("/", app);
  return frontdoor;
}

export async function fetchApi(request: Request, env: Env, repository: ChatRepository): Promise<Response> {
  return createApi(() => repository).fetch(request, env);
}
