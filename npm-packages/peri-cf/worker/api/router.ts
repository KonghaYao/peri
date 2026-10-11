import { Hono } from "hono";
import { apiApp } from "./app";
import { registerRelayRoutes } from "./relay";
import type { Env } from "../types";

/**
 * 零配置分发：静态资源之外的唯一 HTTP 面是 relay 透传
 * （`/api/relay/storage/*`、`/api/relay/model/*`）。不持有配置、凭据、会话运行时，也不注册实例。
 */
export function createApi(): Hono<{ Bindings: Env }> {
  const app = apiApp();
  registerRelayRoutes(app);
  return app;
}
