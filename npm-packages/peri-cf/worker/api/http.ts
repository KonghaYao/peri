import { HTTPException } from "hono/http-exception";
import { chatIdSchema } from "../../shared/chat";
import type { Env } from "../types";

export function diagnosticMessage(error: unknown): string {
  const ancestors = new Set<object>();
  function describe(value: unknown): string {
    if (typeof value === "string") return value;
    if (!value || typeof value !== "object") return String(value);
    if (ancestors.has(value)) return "[Circular error cause]";
    ancestors.add(value);
    let message: string;
    if ("message" in value && typeof value.message === "string") {
      message = value.message;
    } else {
      try {
        message = JSON.stringify(value) ?? String(value);
      } catch {
        message = String(value);
      }
    }
    if ("cause" in value && value.cause !== undefined)
      message += `\nCaused by: ${describe(value.cause)}`;
    if (value instanceof AggregateError)
      for (const failure of value.errors) message += `\nAggregated error: ${describe(failure)}`;
    ancestors.delete(value);
    return message;
  }
  return describe(error);
}

export function logError(label: string, error: unknown, context: Record<string, unknown> = {}): void {
  console.error(label, context, error);
}

export function publicError(error: unknown, _env: Env): string {
  return diagnosticMessage(error).replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, "").slice(0, 512);
}

export function errorResponse(error: unknown): Response {
  if (error instanceof HTTPException && error.res) {
    const headers = new Headers(error.res.headers);
    headers.set("Cache-Control", "no-store");
    return new Response(error.res.body, { status: error.status, headers });
  }
  return Response.json({ error: diagnosticMessage(error) }, {
    status: error instanceof HTTPException ? error.status : 500,
    headers: { "Cache-Control": "no-store" },
  });
}

export function normalizeChatId(id: string | undefined): string {
  const parsed = chatIdSchema.safeParse(id);
  if (!parsed.success) throw new HTTPException(404, { message: "Not found" });
  return parsed.data;
}
