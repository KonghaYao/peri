import { HTTPException } from "hono/http-exception";

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
