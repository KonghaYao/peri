import { createApi } from "./api/router";
import type { Env } from "./types";
import type { ExecutionContext } from "@cloudflare/workers-types";

/**
 * 零配置入口：Worker 只提供静态资源与 relay 透传，不持有配置、凭据、会话运行时、
 * 同步状态与执行调度。计算实例（Peri ACP Host）驻留在浏览器，上游地址与凭据同在其侧。
 */
const api = createApi();

export default {
  async fetch(request: Request, env: Env, context: ExecutionContext): Promise<Response> {
    const path = new URL(request.url).pathname;
    if (path === "/api" || path.startsWith("/api/")) return api.fetch(request, env, context);
    return env.ASSETS ? env.ASSETS.fetch(request) : new Response("Not found", { status: 404 });
  },
};
