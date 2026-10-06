import { Hono } from "hono";
import { HTTPException } from "hono/http-exception";
import { authenticatedApp } from "./app";
import { normalizeChatId, publicError, errorResponse } from "./http";
import { jsonRequest } from "./json";
import { createChatBodySchema } from "../../shared/chat";
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
  for (const [path, method] of [
    ["/api/chats/:id", "GET"],
    ["/api/chats/:id/messages", "POST"],
    ["/api/chats/:id/cancel", "POST"],
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
  frontdoor.get("/api/chats/:id/sync", (context) => {
    const id = normalizeChatId(context.req.param("id"));
    if (context.req.header("Upgrade")?.toLowerCase() !== "websocket")
      return context.json({ error: "WebSocket upgrade required" }, 426);
    if (new URL(context.req.url).searchParams.has("token"))
      return context.json({ error: "URL credentials are not supported" }, 400);
    return context.env.CHAT_SESSIONS.get(context.env.CHAT_SESSIONS.idFromName(id)).fetch(context.req.raw);
  });
  frontdoor.onError((error, context) => {
    if (!(error instanceof HTTPException))
      console.error("Peri sync upgrade failed", { message: publicError(error, context.env) });
    return errorResponse(error);
  });
  frontdoor.route("/", app);
  return frontdoor;
}

export async function fetchApi(request: Request, env: Env, repository: ChatRepository): Promise<Response> {
  return createApi(() => repository).fetch(request, env);
}
