import { Hono } from "hono";
import { bearerAuth } from "hono/bearer-auth";
import { HTTPException } from "hono/http-exception";
import { errorResponse, logError } from "./http";
import type { Env } from "../types";

const unauthorized = { message: { error: "Unauthorized" }, wwwAuthenticateHeader: "Bearer" };

export function authenticatedApp(): Hono<{ Bindings: Env }> {
  const app = new Hono<{ Bindings: Env }>();
  app.use("*", async (context, next) => {
    context.header("Cache-Control", "no-store");
    if (!context.env.APP_AUTH_TOKEN)
      return context.json({ error: "Authentication is not configured" }, 503);
    if ((context.req.header("Authorization")?.length ?? 0) > 4103)
      return context.json({ error: "Unauthorized" }, 401, { "WWW-Authenticate": "Bearer" });
    try {
      await bearerAuth<{ Bindings: Env }>({ token: context.env.APP_AUTH_TOKEN,
        noAuthenticationHeader: unauthorized, invalidAuthenticationHeader: unauthorized, invalidToken: unauthorized,
      })(context, async () => {});
    } catch (error) {
      if (error instanceof HTTPException && error.status === 400)
        throw new HTTPException(401, { res: error.getResponse() });
      throw error;
    }
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
