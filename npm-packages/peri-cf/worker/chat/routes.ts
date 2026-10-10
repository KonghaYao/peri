import { HTTPException } from "hono/http-exception";
import { Hono, type Context } from "hono";
import { authenticatedApp } from "../api/app";
import { logError, normalizeChatId, errorResponse } from "../api/http";
import { cfSocketHost, type SocketHost } from "./sync";
import type { ChatRegistry } from "./registry";
import type { ChatRepository, Defer, Env, StartTransport } from "../types";

/** `GET /sync` 之外的所有会话命令都必须走接受连接的那条 WS（HTTP 没有会话亲和性）。 */
const upgradeRequired = (message: string): Response => Response.json({ error: message, upgrade: true },
  { status: 426, headers: { "Cache-Control": "no-store" } });

export interface ExecutionContextLike { waitUntil(promise: Promise<unknown>): void }

export interface ChatRouteDeps {
  registry: Pick<ChatRegistry, "runtimeFor" | "peek">;
  startTransport: StartTransport;
  repository(env: Env, waitUntil: (promise: Promise<unknown>) => void): ChatRepository;
  /** 平台 socket seam；测试可注入内存 socket。 */
  socketHost?(request: Request, execution: ExecutionContextLike): SocketHost;
}

/** 平台保活入口；无执行上下文（测试/嵌入式调用）时退化为 detached 保活，不阻塞业务。 */
export function keepAliveOf(context: Context<{ Bindings: Env }>): Defer {
  return (promise) => {
    try { context.executionCtx.waitUntil(promise); }
    catch { void promise.catch(() => {}); }
  };
}

/** 会话级 HTTP 面：读面走进程内运行期，命令面一律 426（改走 WS 控制帧）。 */
export function createChatRoutes(deps: ChatRouteDeps) {
  const app = authenticatedApp();
  app.get("/api/v2/chats/:id", async (context) => {
    const id = normalizeChatId(context.req.param("id"));
    const repository = deps.repository(context.env, keepAliveOf(context));
    const runtime = deps.registry.runtimeFor(id, context.env, { startTransport: deps.startTransport, repository });
    // 冷 isolate 没有内存展示态：经 ACP 重放复原后再回答（无 DO 后 HTTP 只是读面的第二入口）。
    return context.json(await runtime.read(keepAliveOf(context)), 200, { "Cache-Control": "no-store" });
  });
  app.get("/api/v2/chats/:id/resources", (context) => {
    const id = normalizeChatId(context.req.param("id"));
    // 只读探测：本 isolate 未驻留该会话时不得为观测触发重放。
    const runtime = deps.registry.peek(id);
    return context.json(runtime?.resources() ?? { sessionId: id, running: false, executionBlocked: false, instance: null },
      200, { "Cache-Control": "no-store" });
  });
  for (const [path, message] of [
    ["/api/v2/chats/:id/messages", "Chat commands must use the session WebSocket"],
    ["/api/v2/chats/:id/cancel", "Chat commands must use the session WebSocket"],
    ["/api/v2/chats/:id/interactions/:interactionId", "Chat commands must use the session WebSocket"],
    ["/api/v2/chats/:id/payloads/:refId", "Payload reads must use the session WebSocket"],
  ] as const) {
    app.all(path, (context) => {
      normalizeChatId(context.req.param("id"));
      return upgradeRequired(message);
    });
  }
  for (const path of ["/api/v2/chats/:id", "/api/v2/chats/:id/resources"] as const) {
    app.all(path, (context) => {
      normalizeChatId(context.req.param("id"));
      throw new HTTPException(405, { message: "Method not allowed" });
    });
  }
  return app;
}

/** 会话升级入口不入鉴权中间件：首个二进制帧（Bearer）才是鉴权点，浏览器无法为 WS 带 Authorization 头。 */
export function createChatFrontdoor(deps: ChatRouteDeps) {
  const frontdoor = new Hono<{ Bindings: Env }>();
  // 升级路由必须先于 createChatRoutes 注册：后者的 `*` 鉴权中间件会拦下同路径请求。
  frontdoor.get("/api/v2/chats/:id/sync", (context) => {
    const id = normalizeChatId(context.req.param("id"));
    if (context.req.header("Upgrade")?.toLowerCase() !== "websocket")
      return context.json({ error: "WebSocket upgrade required" }, 426, { "Cache-Control": "no-store" });
    if (new URL(context.req.url).searchParams.has("token"))
      return context.json({ error: "URL credentials are not supported" }, 400, { "Cache-Control": "no-store" });
    const repository = deps.repository(context.env, keepAliveOf(context));
    const runtime = deps.registry.runtimeFor(id, context.env, { startTransport: deps.startTransport, repository });
    const execution: ExecutionContextLike = { waitUntil: keepAliveOf(context) };
    const host = deps.socketHost?.(context.req.raw, execution) ?? cfSocketHost(execution);
    return runtime.openSocket(context.req.raw, host);
  });
  frontdoor.all("/api/v2/chats/:id/sync", () => { throw new HTTPException(405, { message: "Method not allowed" }); });
  frontdoor.route("/", createChatRoutes(deps));
  frontdoor.onError((error, context) => {
    if (!(error instanceof HTTPException) || error.status >= 500 || error.cause !== undefined)
      logError("Peri sync upgrade failed", error, { method: context.req.method, path: context.req.path });
    return errorResponse(error);
  });
  return frontdoor;
}
