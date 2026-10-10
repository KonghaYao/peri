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
  app.get("/api/chats", async (context) => {
    const repository = repositoryFactory(context.env, (promise) => context.executionCtx.waitUntil(promise));
    return context.json({ chats: await repository.list() });
  });
  app.post("/api/chats", ...jsonRequest(createChatBodySchema, "title must be a nonempty string of at most 200 characters"), async (context) => {
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
      const response = await session.fetch(new Request(new URL(`/api/chats/${encodeURIComponent(chatId)}/resources`, context.req.url), {
        headers: { Authorization: authorization },
      }));
      if (!response.ok) throw new Error(`Chat instance observation failed with HTTP ${response.status}`);
      return response.json();
    });
    return context.json({ generatedAt: new Date().toISOString(), instances });
  });
  app.all("/api/instances", () => { throw new HTTPException(405, { message: "Method not allowed" }); });
  for (const [path, method] of [
    ["/api/chats/:id", "GET"],
    ["/api/chats/:id/messages", "POST"],
    ["/api/chats/:id/cancel", "POST"],
    ["/api/chats/:id/resources", "GET"],
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
  app.all("/api/chats", () => { throw new HTTPException(405, { message: "Method not allowed" }); });
  const frontdoor = new Hono<{ Bindings: Env }>();
  frontdoor.get("/api/chats/:id/sync", async (context) => {
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
