import { startTransport } from "./wasm/host";
import { chatRepository } from "./chat/repository";
import { createPeriSession } from "./chat/creation";
import { createApi } from "./api/router";
import { runtimes } from "./chat/registry";
import type { Env } from "./types";
import type { ExecutionContext } from "@cloudflare/workers-types";

/**
 * 无 DO 入口：会话命令驻留在接受连接/请求的 isolate 内，展示态经 ACP `session/load` 重放复原，
 * 跨 isolate 的持久事实只有 Turso 会话存储。isolate 被回收即在途执行丢失，不复原旧执行。
 */
const api = createApi({
  repository: (env, waitUntil) => chatRepository(env, (title) => createPeriSession(env, title, startTransport, waitUntil)),
  startTransport,
  runtimes,
});

export default {
  async fetch(request: Request, env: Env, context: ExecutionContext): Promise<Response> {
    const path = new URL(request.url).pathname;
    if (path === "/api" || path.startsWith("/api/")) return api.fetch(request, env, context);
    return env.ASSETS ? env.ASSETS.fetch(request) : new Response("Not found", { status: 404 });
  },
};
