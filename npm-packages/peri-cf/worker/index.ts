import { startTransport } from "./wasm/host";
import { ChatSessionCore } from "./chat/session";
import { chatRepository } from "./chat/repository";
import { createPeriSession } from "./chat/creation";
import { createApi } from "./api/router";
import type { Env } from "./types";
import type { ExecutionContext } from "@cloudflare/workers-types";
import type { HibernatingSessionState } from "./chat/sync";

export class ChatSession extends ChatSessionCore {
  constructor(state: HibernatingSessionState, env: Env) {
    super(state, env, startTransport,
      chatRepository(env, (title) => createPeriSession(env, title, startTransport, (promise) => state.waitUntil(promise))));
  }
}

const api = createApi((env, waitUntil) => chatRepository(env,
  (title) => createPeriSession(env, title, startTransport, waitUntil)));

export default {
  async fetch(request: Request, env: Env, context: ExecutionContext): Promise<Response> {
    const path = new URL(request.url).pathname;
    if (path === "/api" || path.startsWith("/api/")) return api.fetch(request, env, context);
    return env.ASSETS ? env.ASSETS.fetch(request) : new Response("Not found", { status: 404 });
  },
};
