import { Hono } from "hono";
import { HTTPException } from "hono/http-exception";
import { authenticatedApp } from "./app";
import { normalizeChatId, logError, errorResponse } from "./http";
import { jsonRequest } from "./json";
import { createChatBodySchema } from "../../shared/chat";
import { createChatFrontdoor, keepAliveOf, type ChatRouteDeps } from "../chat/routes";
import { runtimes } from "../chat/registry";
import { collectChatInstances } from "../instances/collect";
import type { ChatRepository, Env } from "../types";

export interface ApiDeps {
  repository(env: Env, waitUntil: (promise: Promise<unknown>) => void): ChatRepository;
  startTransport: ChatRouteDeps["startTransport"];
  /** 运行期注册表；测试注入独立实例以免相互影响。 */
  runtimes?: ChatRouteDeps["registry"];
  /** 平台 socket seam；测试注入内存 socket。 */
  socketHost?: ChatRouteDeps["socketHost"];
}

/**
 * 无 DO 的进程内分发：会话运行期驻留在接受请求的 isolate，跨 isolate 的持久事实只有 Turso 会话存储。
 * 会话命令只能经 WS 控制帧下发（HTTP 没有会话亲和性）。
 */
export function createApi(deps: ApiDeps): Hono<{ Bindings: Env }> {
  const registry = deps.runtimes ?? runtimes;
  const app = authenticatedApp();
  app.get("/api/v2/chats", async (context) => {
    const repository = deps.repository(context.env, keepAliveOf(context));
    return context.json({ chats: await repository.list() });
  });
  app.post("/api/v2/chats", ...jsonRequest(createChatBodySchema, "title must be a nonempty string of at most 200 characters"), async (context) => {
    const { title } = context.req.valid("json");
    const repository = deps.repository(context.env, keepAliveOf(context));
    return context.json(await repository.create(title), 201);
  });
  app.get("/api/instances", async (context) => {
    const repository = deps.repository(context.env, keepAliveOf(context));
    const chats = await repository.list();
    // 只读探测：观测不得唤醒会话，未驻留本 isolate 的会话如实报告为不可达。
    const instances = await collectChatInstances(chats, async (chatId) => {
      const runtime = registry.peek(chatId);
      if (!runtime) throw new Error("No resident session runtime in this isolate");
      return runtime.resources();
    });
    return context.json({ generatedAt: new Date().toISOString(), instances });
  });
  app.all("/api/instances", () => { throw new HTTPException(405, { message: "Method not allowed" }); });
  app.all("/api/v2/chats", () => { throw new HTTPException(405, { message: "Method not allowed" }); });
  // 版本化入口（P3 已批准）：旧 chat 路径在切换后返回明确的升级错误，不再承载新版消费面。
  for (const path of ["/api/chats", "/api/chats/:id", "/api/chats/:id/messages", "/api/chats/:id/cancel",
    "/api/chats/:id/resources", "/api/chats/:id/payloads/:refId", "/api/chats/:id/sync"] as const) {
    app.all(path, () => Response.json({ error: "Client upgrade required", upgrade: true }, { status: 426, headers: { "Cache-Control": "no-store" } }));
  }
  const frontdoor = new Hono<{ Bindings: Env }>();
  // 会话面先注册：升级入口自带鉴权（首帧 Bearer），不能落到 `*` Bearer 中间件之后。
  frontdoor.route("/", createChatFrontdoor({ registry, startTransport: deps.startTransport,
    repository: deps.repository, socketHost: deps.socketHost }));
  frontdoor.route("/", app);
  return frontdoor;
}

export async function fetchApi(request: Request, env: Env, repository: ChatRepository,
  options: { runtimes?: ApiDeps["runtimes"]; socketHost?: ApiDeps["socketHost"];
    startTransport?: ApiDeps["startTransport"] } = {}): Promise<Response> {
  return createApi({ repository: () => repository, startTransport: options.startTransport ?? missingStartTransport,
    runtimes: options.runtimes, socketHost: options.socketHost }).fetch(request, env);
}

const missingStartTransport: ApiDeps["startTransport"] = async () => {
  throw new Error("No ACP transport start is wired for this API fixture");
};
