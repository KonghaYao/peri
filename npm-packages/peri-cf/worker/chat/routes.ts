import { HTTPException } from "hono/http-exception";
import { Hono } from "hono";
import { authenticatedApp } from "../api/app";
import { normalizeChatId, errorResponse, publicError } from "../api/http";
import { jsonRequest } from "../api/json";
import { sendMessageBodySchema } from "../../shared/chat";
import type { Env } from "../types";

interface ChatHandlers {
  read(id: string): Promise<Response>;
  send(id: string, content: string, signal: AbortSignal): Promise<Response>;
  cancel(id: string): Promise<Response>;
  sync(id: string, request: Request): Response;
}

export function createChatRoutes(handlers: ChatHandlers) {
  const app = authenticatedApp();
  app.get("/api/chats/:id", (context) => handlers.read(normalizeChatId(context.req.param("id"))));
  app.post("/api/chats/:id/messages", ...jsonRequest(sendMessageBodySchema, "content must be a nonempty string of at most 32768 characters"),
    (context) => handlers.send(normalizeChatId(context.req.param("id")), context.req.valid("json").content, context.req.raw.signal));
  app.post("/api/chats/:id/cancel", (context) => handlers.cancel(normalizeChatId(context.req.param("id"))));
  for (const path of ["/api/chats/:id", "/api/chats/:id/messages", "/api/chats/:id/cancel"]) {
    app.all(path, (context) => {
      normalizeChatId(context.req.param("id"));
      throw new HTTPException(405, { message: "Method not allowed" });
    });
  }
  const frontdoor = new Hono<{ Bindings: Env }>();
  frontdoor.get("/api/chats/:id/sync", (context) => handlers.sync(normalizeChatId(context.req.param("id")), context.req.raw));
  frontdoor.onError((error, context) => {
    if (!(error instanceof HTTPException))
      console.error("Peri sync upgrade failed", { message: publicError(error, context.env) });
    return errorResponse(error);
  });
  frontdoor.route("/", app);
  return frontdoor;
}
