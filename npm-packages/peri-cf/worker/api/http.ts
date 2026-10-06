import { HTTPException } from "hono/http-exception";
import { chatIdSchema } from "../../shared/chat";
import type { Env } from "../types";

export function publicError(error: unknown, env: Env): string {
  let message = typeof error === "string" ? error
    : error && typeof error === "object" && "message" in error && typeof error.message === "string"
      ? error.message : "Chat execution failed";
  for (const secret of [env.APP_AUTH_TOKEN, env.PERI_STORAGE_TOKEN, env.MODEL_API_KEY, env.PERI_STORAGE_URL, env.MODEL_BASE_URL]) {
    if (secret) message = message.split(secret).join("[redacted]");
  }
  return message.slice(0, 512);
}

export function errorResponse(error: unknown): Response {
  if (error instanceof HTTPException && error.res) {
    const headers = new Headers(error.res.headers);
    headers.set("Cache-Control", "no-store");
    return new Response(error.res.body, { status: error.status, headers });
  }
  return Response.json({ error: error instanceof HTTPException ? error.message : "Backend request failed" }, {
    status: error instanceof HTTPException ? error.status : 500,
    headers: { "Cache-Control": "no-store" },
  });
}

export function normalizeChatId(id: string | undefined): string {
  const parsed = chatIdSchema.safeParse(id);
  if (!parsed.success) throw new HTTPException(404, { message: "Not found" });
  return parsed.data;
}
