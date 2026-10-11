import { Hono } from "hono";
import { HTTPException } from "hono/http-exception";
import { errorResponse, logError } from "./http";
import type { Env } from "../types";

/** 所有 HTTP 面共用的 app：统一 no-store、错误与 404 语义；没有访问控制中间件。 */
export function apiApp(): Hono<{ Bindings: Env }> {
  const app = new Hono<{ Bindings: Env }>();
  app.use("*", async (context, next) => {
    context.header("Cache-Control", "no-store");
    await next();
    if (!context.res.headers.get("Cache-Control")?.includes("no-store"))
      context.header("Cache-Control", "no-store");
  });
  app.onError((error, context) => {
    if (!(error instanceof HTTPException) || error.status >= 500 || error.cause !== undefined)
      logError("Peri API request failed", error, { method: context.req.method, path: context.req.path });
    return errorResponse(error);
  });
  app.notFound(() => Response.json({ error: "Not found" }, {
    status: 404, headers: { "Cache-Control": "no-store" },
  }));
  return app;
}
