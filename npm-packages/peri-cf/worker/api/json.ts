import { createMiddleware } from "hono/factory";
import { bodyLimit } from "hono/body-limit";
import { HTTPException } from "hono/http-exception";
import { validator } from "hono/validator";
import type { z } from "zod";
import type { Env } from "../types";

const requireJson = createMiddleware<{ Bindings: Env }>(async (context, next) => {
  if (context.req.header("Content-Type")?.split(";")[0].toLowerCase().trim() !== "application/json")
    throw new HTTPException(415, { message: "Expected application/json" });
  if (!context.req.raw.body) throw new HTTPException(400, { message: "Expected a JSON object" });
  await next();
});

export const boundedRequestBody = [createMiddleware<{ Bindings: Env }>(async (context, next) => {
  // 按实际字节限制并读完请求体，不让 DO 转发流活过响应。
  const headers = new Headers(context.req.raw.headers);
  headers.delete("Content-Length");
  context.req.raw = new Request(context.req.raw, { headers });
  await next();
}), bodyLimit({ maxSize: 65_536, onError: () => { throw new HTTPException(413, { message: "Request body is too large" }); } })] as const;

export function jsonRequest<Output>(schema: z.ZodType<Output>, message: string) {
  return [
    requireJson,
    ...boundedRequestBody,
    validator("json", (body): Output => {
      const parsed = schema.safeParse(body);
      if (!parsed.success) throw new HTTPException(400, { message });
      return parsed.data;
    }),
  ] as const;
}
